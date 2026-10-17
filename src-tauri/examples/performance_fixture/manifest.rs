//! Corpus manifest schema, deterministic notebook metadata and file hashing.

use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{self, NotebookRef};

pub const FORMAT: &str = "cuenote-q51-performance-corpus";
pub const FORMAT_VERSION: u32 = 1;
pub const GENERATOR: &str = "src-tauri/examples/performance_fixture.rs";

pub const CHARS_RULE: &str = "Unicode code points over title + inline text + inline/block LaTeX source + code text + table cell text + mind-map node text; asset ids, link URLs, code language markers, image bytes and other metadata excluded; hard breaks count zero. totals.textChars aggregates every package of a profile, including standalone samples.";

pub const STABLE_ID_RULE: &str =
    "sha256(domain bytes || 0x00 || index u64 big-endian), first 16 bytes formatted as UUID with v4/variant bits forced; stable across machines and runs";

pub const ANCILLARY_RULE: &str = "each PNG is padded to its exact target byte size by one zero-payload ancillary chunk (caPa) inserted before IEND; ancillaryBytes is the total ancillary chunk capacity added inside the PNG container; no ZIP or file-level padding is used";

pub const TIMESTAMPS_RULE: &str =
    "all package timestamps are derived from the fixed base recorded in the manifest; no wall-clock value is embedded";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationParameters {
    pub text_notes: usize,
    pub ordinary_notes: usize,
    pub stress_notes: usize,
    pub notebooks: usize,
    pub text_note_chars: usize,
    pub ordinary_chars: usize,
    pub stress_chars: usize,
    pub ordinary_images: usize,
    pub ordinary_image_side: u32,
    pub ordinary_asset_bytes: u64,
    pub stress_images: usize,
    pub stress_image_side: u32,
    pub stress_asset_bytes: u64,
    pub ordinary_map_nodes: usize,
    pub stress_map_nodes: usize,
    pub max_root_children: usize,
    pub timestamp_base_seconds: i64,
    pub timestamp_step_seconds: i64,
    pub chars_rule: String,
    pub stable_id_rule: String,
    pub ancillary_padding_rule: String,
    pub timestamps_rule: String,
    pub library_path: String,
    pub samples_path: String,
}

pub fn generation_parameters(scale: &model::Scale) -> GenerationParameters {
    GenerationParameters {
        text_notes: scale.text_notes,
        ordinary_notes: scale.ordinary_notes,
        stress_notes: scale.stress_notes,
        notebooks: scale.notebooks,
        text_note_chars: model::TEXT_CHARS,
        ordinary_chars: model::ORDINARY_CHARS,
        stress_chars: model::STRESS_CHARS,
        ordinary_images: model::ORDINARY_IMAGES,
        ordinary_image_side: model::ORDINARY_IMAGE_SIDE,
        ordinary_asset_bytes: model::ORDINARY_ASSET_BYTES,
        stress_images: model::STRESS_IMAGES,
        stress_image_side: model::STRESS_IMAGE_SIDE,
        stress_asset_bytes: model::STRESS_ASSET_BYTES,
        ordinary_map_nodes: model::ORDINARY_MAP_NODES,
        stress_map_nodes: model::STRESS_MAP_NODES,
        max_root_children: model::MAX_ROOT_CHILDREN,
        timestamp_base_seconds: model::TIMESTAMP_BASE_SECONDS,
        timestamp_step_seconds: model::TIMESTAMP_STEP_SECONDS,
        chars_rule: CHARS_RULE.to_owned(),
        stable_id_rule: STABLE_ID_RULE.to_owned(),
        ancillary_padding_rule: ANCILLARY_RULE.to_owned(),
        timestamps_rule: TIMESTAMPS_RULE.to_owned(),
        library_path: model::LIBRARY_DIR.to_owned(),
        samples_path: model::SAMPLES_DIR.to_owned(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextCharsTotals {
    pub text: u64,
    pub ordinary: u64,
    pub stress: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Totals {
    pub files: usize,
    pub bytes: u64,
    pub manifest_bytes_excluded: bool,
    pub notes: usize,
    pub notebooks: usize,
    pub images: usize,
    pub image_bytes: u64,
    pub ancillary_bytes: u64,
    pub map_nodes: usize,
    pub text_chars: TextCharsTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileRecord {
    pub path: String,
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotebookRecord {
    pub path: String,
    pub notebook_id: String,
    pub name: String,
    pub is_default: bool,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteRecord {
    pub path: String,
    pub note_id: String,
    pub profile: String,
    pub collection: String,
    pub text_chars: usize,
    pub image_count: usize,
    pub map_nodes: usize,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageRecord {
    pub path: String,
    pub note_id: String,
    pub asset_id: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub ancillary_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRecord {
    pub query: String,
    pub note_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SamplePointer {
    pub path: String,
    pub note_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StandaloneSamples {
    pub ordinary: SamplePointer,
    pub stress: SamplePointer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CorpusManifest {
    pub format: String,
    pub format_version: u32,
    pub scale: String,
    pub generator: String,
    pub generation_parameters: GenerationParameters,
    pub totals: Totals,
    pub standalone_samples: StandaloneSamples,
    pub notebooks: Vec<NotebookRecord>,
    pub notes: Vec<NoteRecord>,
    pub images: Vec<ImageRecord>,
    pub known_queries: Vec<QueryRecord>,
    pub files: Vec<FileRecord>,
}

// ---------------------------------------------------------------------------
// Notebook metadata (shape identical to the host's library metadata files)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotebookTimestamps {
    pub created_at: String,
    pub modified_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotebookMetadata {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub default: bool,
    pub timestamps: NotebookTimestamps,
}

pub fn notebook_metadata_bytes(notebook: &NotebookRef) -> Result<Vec<u8>, Box<dyn Error>> {
    let (created_at, modified_at) = model::notebook_timestamps(notebook.index);
    let metadata = NotebookMetadata {
        schema_version: 1,
        id: notebook.notebook_id.clone(),
        name: notebook.name.clone(),
        default: notebook.is_default,
        timestamps: NotebookTimestamps {
            created_at,
            modified_at,
        },
    };
    Ok(serde_json::to_vec(&metadata)?)
}

// ---------------------------------------------------------------------------
// File walking and hashing
// ---------------------------------------------------------------------------

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<(String, u64), Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut length = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        length += read as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), length))
}

/// Sorted relative-path file list with streamed SHA-256 and byte length.
pub fn walk_files(root: &Path) -> Result<Vec<FileRecord>, Box<dyn Error>> {
    let mut records = Vec::new();
    walk_dir(root, root, &mut records)?;
    records.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(records)
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<FileRecord>) -> Result<(), Box<dyn Error>> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(
                format!("symlinks are not allowed in the corpus: {:?}", entry.path()).into(),
            );
        }
        if file_type.is_dir() {
            walk_dir(root, &entry.path(), out)?;
        } else if file_type.is_file() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)?
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let (sha256, bytes) = sha256_file(&path)?;
            out.push(FileRecord {
                path: relative,
                sha256: Some(sha256),
                bytes: Some(bytes),
            });
        } else {
            return Err(format!("unsupported file type: {:?}", entry.path()).into());
        }
    }
    Ok(())
}

pub fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}
