//! Disposable, device-local projections. Neither cache metadata nor file times
//! authorize a hit: the scanner must hash the source and its durable witness.
use super::*;
use crate::storage::versions::ObservedVersionWitness;

const SCHEMA: u32 = 1;
// This optional optimization has a smaller budget than native content. A larger
// library still indexes normally; it simply does not persist this projection.
const MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 4_096;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Entry {
    pub summary: NoteSummary,
    pub revision: String,
    pub projection: Vec<(SearchSection, String)>,
    pub witness: ObservedVersionWitness,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    library: String,
    entries: BTreeMap<String, Entry>,
}

pub(super) struct Cache {
    directory: PathBuf,
    prefix: String,
    library: String,
    previous_digest: Option<String>,
    pub entries: BTreeMap<String, Entry>,
}

impl Cache {
    pub fn load(config_root: &Path, library: &Path, bypass: bool) -> Option<Self> {
        Self::try_load(config_root, library, bypass).ok()
    }

    fn try_load(config_root: &Path, library: &Path, bypass: bool) -> Result<Self, WorkspaceError> {
        ensure_config_directory(config_root)?;
        let config = ensure_real_directory(config_root, None)?;
        let directory = config.join("search-cache-v1");
        ensure_config_directory(&directory)?;
        let directory = ensure_real_directory(&directory, Some(&config))?;
        let library = display_path(
            &fs::canonicalize(library)
                .map_err(|error| StorageError::new("cache", error.to_string()))?,
        );
        let prefix = format!("{:x}-", sha2::Sha256::digest(library.as_bytes()));
        let mut cache = Self {
            directory,
            prefix,
            library,
            previous_digest: None,
            entries: BTreeMap::new(),
        };
        if !bypass {
            for path in cache.files()?.into_iter().rev().take(2) {
                let Some(expected) = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.rsplit('-').next())
                else {
                    continue;
                };
                let Ok(bytes) = read_sync_bounded(&path, MAX_CACHE_BYTES, "本机搜索缓存")
                else {
                    continue;
                };
                let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
                if digest != expected {
                    continue;
                }
                let Ok(snapshot) = serde_json::from_slice::<Snapshot>(&bytes) else {
                    continue;
                };
                if snapshot.schema != SCHEMA
                    || snapshot.library != cache.library
                    || snapshot.entries.len() > MAX_NOTE_FILES
                {
                    continue;
                }
                cache.entries = snapshot.entries;
                cache.previous_digest = Some(digest);
                break;
            }
        }
        Ok(cache)
    }

    fn files(&self) -> Result<Vec<PathBuf>, WorkspaceError> {
        let entries = fs::read_dir(&self.directory)
            .map_err(|error| StorageError::new("cache", error.to_string()))?;
        let mut paths = Vec::new();
        for (index, item) in entries.enumerate() {
            if index >= MAX_CACHE_FILES {
                return Err(StorageError::new("cache", "缓存文件数量超限。"));
            }
            let item = item.map_err(|error| StorageError::new("cache", error.to_string()))?;
            if item
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&self.prefix) && name.ends_with(".json"))
            {
                paths.push(item.path());
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// Cache publication is optional. Failure never undoes a save or marks a
    /// fully validated in-memory index unavailable; the next scan can rebuild.
    pub fn publish(self, entries: BTreeMap<String, Entry>) {
        let _ = self.try_publish(entries);
    }

    fn try_publish(&self, entries: BTreeMap<String, Entry>) -> Result<(), WorkspaceError> {
        let snapshot = Snapshot {
            schema: SCHEMA,
            library: self.library.clone(),
            entries,
        };
        let mut bytes = LimitedBytes(Vec::new());
        serde_json::to_writer(&mut bytes, &snapshot)
            .map_err(|error| StorageError::new("cache", error.to_string()))?;
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes.0));
        if self.previous_digest.as_deref() == Some(&digest) {
            return Ok(());
        }
        let directory = storage::StableDirectory::open(&self.directory)?;
        directory.ensure_current()?;
        let name = format!(
            "{}{:020}-{}-{digest}.json",
            self.prefix,
            Utc::now().timestamp_micros(),
            Uuid::new_v4()
        );
        publish_unique_snapshot(&self.directory, &name, &bytes.0, ".search-cache-")?;
        let files = self.files()?;
        let remove_count = files.len().saturating_sub(2);
        for path in files.into_iter().take(remove_count) {
            directory.ensure_current()?;
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| StorageError::new("cache", error.to_string()))?;
            if metadata.is_file() && !is_link_or_reparse(&metadata) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }
}

struct LimitedBytes(Vec<u8>);
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if (self.0.len() as u64).saturating_add(bytes.len() as u64) > MAX_CACHE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "搜索缓存超过资源上限",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
