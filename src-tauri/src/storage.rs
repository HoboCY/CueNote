use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

const NOTE_FILE: &str = "note.cuenote";
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ASSET_BYTES: usize = 24 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES: usize = 54 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 72 * 1024 * 1024;
const MAX_ASSETS: usize = 128;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    Invalid,
    Io,
    Corrupt,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct StorageError {
    pub kind: StorageErrorKind,
    message: String,
}

impl StorageError {
    fn new(kind: StorageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.kind {
            StorageErrorKind::Invalid => "INVALID",
            StorageErrorKind::Io => "IO",
            StorageErrorKind::Corrupt => "CORRUPT",
            StorageErrorKind::Conflict => "CONFLICT",
        };
        write!(f, "{prefix}: {}", self.message)
    }
}

impl std::error::Error for StorageError {}

fn io_error(action: &str, error: impl fmt::Display) -> StorageError {
    StorageError::new(StorageErrorKind::Io, format!("{action}: {error}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteDocument {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub cue: serde_json::Value,
    pub content: serde_json::Value,
    pub summary: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    pub mime: String,
    pub base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub document: NoteDocument,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    pub revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub revision: String,
    pub elapsed_ms: u64,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionResult {
    pub revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrototypeInfo {
    pub path: String,
    pub recovery_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssetManifest {
    schema_version: u32,
    assets: Vec<AssetMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssetMetadata {
    id: String,
    mime: String,
}

#[derive(Debug)]
struct ValidatedAsset {
    metadata: AssetMetadata,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct ValidatedSnapshot {
    snapshot: Snapshot,
    document_bytes: Vec<u8>,
    assets: Vec<ValidatedAsset>,
    manifest_bytes: Vec<u8>,
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn unique_token() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{time}-{}-{counter}", std::process::id())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_mime(value: &str) -> bool {
    matches!(
        value,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

fn validate_snapshot(snapshot: Snapshot) -> Result<ValidatedSnapshot, StorageError> {
    let document = &snapshot.document;
    if document.schema_version != 1 {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "only document schemaVersion 1 is supported",
        ));
    }
    if !valid_id(&document.id) {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "document id must use 1-128 ASCII letters, digits, '_' or '-'",
        ));
    }
    if document.title.as_bytes().len() > 4096 || document.title.contains('\0') {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "title is too long or contains a NUL character",
        ));
    }
    if snapshot.assets.len() > MAX_ASSETS {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            format!("at most {MAX_ASSETS} assets are accepted"),
        ));
    }

    let document_bytes = serde_json::to_vec(document)
        .map_err(|error| StorageError::new(StorageErrorKind::Invalid, error.to_string()))?;
    if document_bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "document JSON exceeds the 8 MiB prototype limit",
        ));
    }

    let mut seen = HashSet::new();
    let mut total_asset_bytes = 0usize;
    let mut assets = Vec::with_capacity(snapshot.assets.len());
    let mut manifest_assets = Vec::with_capacity(snapshot.assets.len());
    for asset in &snapshot.assets {
        if !valid_id(&asset.id) {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                format!("asset id {:?} is invalid", asset.id),
            ));
        }
        if !seen.insert(asset.id.clone()) {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                format!("duplicate asset id {:?}", asset.id),
            ));
        }
        if !valid_mime(&asset.mime) {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                format!("asset MIME {:?} is not an accepted raster image type", asset.mime),
            ));
        }
        let encoded_limit = MAX_ASSET_BYTES.div_ceil(3) * 4;
        if asset.base64.len() > encoded_limit {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                format!("asset {:?} exceeds the 24 MiB prototype limit", asset.id),
            ));
        }
        let bytes = STANDARD.decode(&asset.base64).map_err(|error| {
            StorageError::new(
                StorageErrorKind::Invalid,
                format!("asset {:?} is not valid standard base64: {error}", asset.id),
            )
        })?;
        if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                format!("asset {:?} is empty or exceeds the 24 MiB prototype limit", asset.id),
            ));
        }
        if STANDARD.encode(&bytes) != asset.base64 {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                format!("asset {:?} base64 must be canonical", asset.id),
            ));
        }
        total_asset_bytes = total_asset_bytes.saturating_add(bytes.len());
        if total_asset_bytes > MAX_TOTAL_ASSET_BYTES {
            return Err(StorageError::new(
                StorageErrorKind::Invalid,
                "assets exceed the 54 MiB total prototype limit",
            ));
        }
        let metadata = AssetMetadata {
            id: asset.id.clone(),
            mime: asset.mime.clone(),
        };
        manifest_assets.push(metadata.clone());
        assets.push(ValidatedAsset { metadata, bytes });
    }

    let manifest = AssetManifest {
        schema_version: 1,
        assets: manifest_assets,
    };
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|error| StorageError::new(StorageErrorKind::Invalid, error.to_string()))?;
    if manifest_bytes.len() > MAX_MANIFEST_BYTES
        || (document_bytes.len() as u64)
            .saturating_add(total_asset_bytes as u64)
            .saturating_add(manifest_bytes.len() as u64)
            > MAX_EXPANDED_BYTES
    {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "snapshot exceeds the 64 MiB expanded prototype limit",
        ));
    }

    Ok(ValidatedSnapshot {
        snapshot,
        document_bytes,
        assets,
        manifest_bytes,
    })
}

fn archive_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>, StorageError> {
    let validated = validate_snapshot(snapshot.clone())?;
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    writer
        .start_file("document.json", SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))
        .map_err(|error| StorageError::new(StorageErrorKind::Io, error.to_string()))?;
    writer
        .write_all(&validated.document_bytes)
        .map_err(|error| io_error("write document.json into ZIP", error))?;
    writer
        .start_file("assets/manifest.json", SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))
        .map_err(|error| StorageError::new(StorageErrorKind::Io, error.to_string()))?;
    writer
        .write_all(&validated.manifest_bytes)
        .map_err(|error| io_error("write assets manifest into ZIP", error))?;
    for asset in &validated.assets {
        writer
            .start_file(format!("assets/{}", asset.metadata.id), SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))
            .map_err(|error| StorageError::new(StorageErrorKind::Io, error.to_string()))?;
        writer
            .write_all(&asset.bytes)
            .map_err(|error| io_error("write asset into ZIP", error))?;
    }
    let cursor = writer
        .finish()
        .map_err(|error| StorageError::new(StorageErrorKind::Io, error.to_string()))?;
    let bytes = cursor.into_inner();
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "compressed note exceeds the 72 MiB prototype limit",
        ));
    }
    Ok(bytes)
}

fn allowed_archive_name(name: &str) -> bool {
    if name == "document.json" || name == "assets/manifest.json" {
        return true;
    }
    let Some(id) = name.strip_prefix("assets/") else {
        return false;
    };
    !id.contains('/') && !id.contains('\\') && valid_id(id)
}

fn read_zip_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, StorageError> {
    let mut entry = archive
        .by_name(name)
        .map_err(|error| StorageError::new(StorageErrorKind::Corrupt, error.to_string()))?;
    if entry.size() > limit as u64 {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("ZIP entry {name:?} exceeds its size limit"),
        ));
    }
    let declared = entry.size();
    let mut bytes = Vec::with_capacity(declared as usize);
    entry
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::new(StorageErrorKind::Corrupt, error.to_string()))?;
    if bytes.len() > limit || bytes.len() as u64 != declared {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("ZIP entry {name:?} has an invalid expanded size"),
        ));
    }
    Ok(bytes)
}

fn decode_archive(bytes: &[u8]) -> Result<Snapshot, StorageError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "compressed note exceeds the 72 MiB prototype limit",
        ));
    }

    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| StorageError::new(StorageErrorKind::Corrupt, error.to_string()))?;
    if archive.len() < 2 || archive.len() > MAX_ASSETS + 2 {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "ZIP entry count is outside the supported range",
        ));
    }

    let mut names = HashSet::new();
    let mut declared_total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| StorageError::new(StorageErrorKind::Corrupt, error.to_string()))?;
        let name = entry.name();
        if entry.is_dir() || !allowed_archive_name(name) || !names.insert(name.to_owned()) {
            return Err(StorageError::new(
                StorageErrorKind::Corrupt,
                format!("ZIP entry {name:?} is invalid or duplicated"),
            ));
        }
        declared_total = declared_total.saturating_add(entry.size());
        if declared_total > MAX_EXPANDED_BYTES {
            return Err(StorageError::new(
                StorageErrorKind::Corrupt,
                "ZIP expanded content exceeds the 64 MiB prototype limit",
            ));
        }
    }
    if !names.contains("document.json") || !names.contains("assets/manifest.json") {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "note ZIP is missing document.json or assets/manifest.json",
        ));
    }

    let document_bytes = read_zip_entry(&mut archive, "document.json", MAX_DOCUMENT_BYTES)?;
    let document: NoteDocument = serde_json::from_slice(&document_bytes)
        .map_err(|error| StorageError::new(StorageErrorKind::Corrupt, error.to_string()))?;
    let manifest_bytes = read_zip_entry(&mut archive, "assets/manifest.json", MAX_MANIFEST_BYTES)?;
    let manifest: AssetManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| StorageError::new(StorageErrorKind::Corrupt, error.to_string()))?;
    if manifest.schema_version != 1 || manifest.assets.len() > MAX_ASSETS {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "asset manifest schema or count is unsupported",
        ));
    }

    let mut expected_names = HashSet::from([
        "document.json".to_owned(),
        "assets/manifest.json".to_owned(),
    ]);
    let mut seen_asset_ids = HashSet::new();
    let mut total_asset_bytes = 0usize;
    let mut assets = Vec::with_capacity(manifest.assets.len());
    for metadata in manifest.assets {
        if !valid_id(&metadata.id)
            || !valid_mime(&metadata.mime)
            || !seen_asset_ids.insert(metadata.id.clone())
        {
            return Err(StorageError::new(
                StorageErrorKind::Corrupt,
                "asset manifest contains an invalid or duplicate asset",
            ));
        }
        let name = format!("assets/{}", metadata.id);
        expected_names.insert(name.clone());
        let data = read_zip_entry(&mut archive, &name, MAX_ASSET_BYTES)?;
        if data.is_empty() {
            return Err(StorageError::new(
                StorageErrorKind::Corrupt,
                format!("asset {:?} is empty", metadata.id),
            ));
        }
        total_asset_bytes = total_asset_bytes.saturating_add(data.len());
        if total_asset_bytes > MAX_TOTAL_ASSET_BYTES {
            return Err(StorageError::new(
                StorageErrorKind::Corrupt,
                "assets exceed the 54 MiB total prototype limit",
            ));
        }
        assets.push(Asset {
            id: metadata.id,
            mime: metadata.mime,
            base64: STANDARD.encode(data),
        });
    }
    if expected_names != names {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "ZIP contains an unlisted asset or an asset is missing",
        ));
    }

    let snapshot = Snapshot { document, assets };
    validate_snapshot(snapshot.clone()).map_err(|error| {
        StorageError::new(
            StorageErrorKind::Corrupt,
            format!("note schema is invalid: {error}"),
        )
    })?;
    Ok(snapshot)
}

fn revision(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn validate_revision(value: &str) -> Result<(), StorageError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "expectedRevision must be a lowercase SHA-256 hex string or null",
        ));
    }
    Ok(())
}

fn ensure_real_directory(path: &Path) -> Result<(), StorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(StorageError::new(
                StorageErrorKind::Io,
                format!("prototype path {} is not a real directory", path.display()),
            ));
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("inspect prototype directory", error)),
    }
    fs::create_dir_all(path).map_err(|error| io_error("create prototype directory", error))?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error("inspect prototype directory", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::new(
            StorageErrorKind::Io,
            format!("prototype path {} is not a real directory", path.display()),
        ));
    }
    Ok(())
}

fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, StorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StorageError::new(
                    StorageErrorKind::Corrupt,
                    format!("{} is not a regular note file", path.display()),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("inspect note file", error)),
    }

    let file = File::open(path).map_err(|error| io_error("open note file", error))?;
    let mut bytes = Vec::new();
    file.take(MAX_ARCHIVE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read note file", error))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "current note exceeds the 72 MiB prototype limit",
        ));
    }
    Ok(Some(bytes))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error("create recovery file", error))?;
    file.write_all(bytes)
        .map_err(|error| io_error("write recovery file", error))?;
    file.sync_all()
        .map_err(|error| io_error("sync recovery file", error))?;
    Ok(())
}

fn create_temp_file(directory: &Path, bytes: &[u8]) -> Result<TempFile, StorageError> {
    for _ in 0..8 {
        let path = directory.join(format!(".note-{}.tmp", unique_token()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)
                    .map_err(|error| io_error("write prepared note ZIP", error))?;
                file.sync_all()
                    .map_err(|error| io_error("sync prepared note ZIP", error))?;
                return Ok(TempFile(path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create prepared note ZIP", error)),
        }
    }
    Err(StorageError::new(
        StorageErrorKind::Io,
        "could not allocate a unique temporary ZIP file",
    ))
}

fn read_prepared_file(path: &Path) -> Result<Vec<u8>, StorageError> {
    let file = File::open(path).map_err(|error| io_error("reopen prepared ZIP", error))?;
    let mut bytes = Vec::new();
    file.take(MAX_ARCHIVE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read prepared ZIP", error))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(StorageError::new(
            StorageErrorKind::Invalid,
            "prepared ZIP exceeds the 72 MiB prototype limit",
        ));
    }
    Ok(bytes)
}

fn new_recovery_folder(recovery_dir: &Path, prefix: &str) -> Result<PathBuf, StorageError> {
    for _ in 0..8 {
        let folder = recovery_dir.join(format!("{prefix}-{}", unique_token()));
        match fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create recovery copy directory", error)),
        }
    }
    Err(StorageError::new(
        StorageErrorKind::Io,
        "could not allocate a unique recovery directory",
    ))
}

fn preserve_conflict(
    recovery_dir: &Path,
    current: Option<&[u8]>,
    draft: &[u8],
    reason: &str,
) -> Result<PathBuf, StorageError> {
    let folder = new_recovery_folder(recovery_dir, "conflict")?;
    match current {
        Some(bytes) => write_new_file(&folder.join("CURRENT.cuenote"), bytes)?,
        None => write_new_file(
            &folder.join("CURRENT-MISSING.txt"),
            b"The current note file was missing at conflict detection time.\n",
        )?,
    }
    write_new_file(&folder.join("DRAFT.cuenote"), draft)?;
    write_new_file(&folder.join("reason.txt"), reason.as_bytes())?;
    Ok(folder)
}

fn preserve_corrupt_draft(
    recovery_dir: &Path,
    current: &[u8],
    draft: &[u8],
    reason: &str,
) -> Result<PathBuf, StorageError> {
    preserve_conflict(recovery_dir, Some(current), draft, reason)
}

fn matches_expected(current: Option<&[u8]>, expected: Option<&str>) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(bytes), Some(expected)) => revision(bytes) == expected,
        _ => false,
    }
}

fn atomic_replace(temp: &Path, destination: &Path, destination_exists: bool) -> Result<(), StorageError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::{null, null_mut};

        #[link(name = "Kernel32")]
        extern "system" {
            fn ReplaceFileW(
                replaced: *const u16,
                replacement: *const u16,
                backup: *const u16,
                flags: u32,
                exclude: *mut std::ffi::c_void,
                reserved: *mut std::ffi::c_void,
            ) -> i32;
            fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
        }

        let destination_wide: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
        let temp_wide: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let succeeded = unsafe {
            if destination_exists {
                ReplaceFileW(
                    destination_wide.as_ptr(),
                    temp_wide.as_ptr(),
                    null(),
                    0,
                    null_mut(),
                    null_mut(),
                )
            } else {
                // Without MOVEFILE_REPLACE_EXISTING, a file created after our last
                // revision check causes this first-save to fail instead of overwrite.
                MoveFileExW(temp_wide.as_ptr(), destination_wide.as_ptr(), 0)
            }
        };
        if succeeded == 0 {
            return Err(io_error(
                "atomically replace scratch note",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        if destination_exists {
            fs::rename(temp, destination).map_err(|error| io_error("atomically replace scratch note", error))
        } else {
            // A same-directory hard link is an atomic create-if-absent operation.
            fs::hard_link(temp, destination)
                .map_err(|error| io_error("atomically create scratch note", error))?;
            fs::remove_file(temp).map_err(|error| io_error("remove prepared ZIP link", error))
        }
    }
}

fn expected_matches_or_preserve(
    expected: Option<&str>,
    current: Option<&[u8]>,
    recovery_dir: &Path,
    draft: &[u8],
    label: &str,
) -> Result<(), StorageError> {
    if matches_expected(current, expected) {
        return Ok(());
    }
    let folder = preserve_conflict(
        recovery_dir,
        current,
        draft,
        &format!("{label}: the saved revision changed before replacement"),
    )?;
    Err(StorageError::new(
        StorageErrorKind::Conflict,
        format!(
            "saved revision changed; current state and draft were preserved in {}",
            folder.display()
        ),
    ))
}

pub fn open_at(root: &Path) -> Result<OpenResult, StorageError> {
    let recovery_dir = root.join("recovery");
    ensure_real_directory(root)?;
    ensure_real_directory(&recovery_dir)?;
    let note_path = root.join(NOTE_FILE);
    let bytes = read_optional_file(&note_path)?.ok_or_else(|| {
        StorageError::new(
            StorageErrorKind::Io,
            format!("scratch note does not exist at {}", note_path.display()),
        )
    })?;
    let snapshot = decode_archive(&bytes)?;
    Ok(OpenResult {
        snapshot,
        revision: revision(&bytes),
    })
}

pub fn save_at(
    root: &Path,
    snapshot: Snapshot,
    expected_revision: Option<String>,
) -> Result<SaveResult, StorageError> {
    let started = Instant::now();
    if let Some(expected) = expected_revision.as_deref() {
        validate_revision(expected)?;
    }
    let validated = validate_snapshot(snapshot)?;
    let archive_bytes = archive_snapshot(&validated.snapshot)?;
    let recovery_dir = root.join("recovery");
    ensure_real_directory(root)?;
    ensure_real_directory(&recovery_dir)?;
    let note_path = root.join(NOTE_FILE);

    let temp = create_temp_file(root, &archive_bytes)?;
    let prepared = read_prepared_file(&temp.0)?;
    let prepared_snapshot = decode_archive(&prepared)?;
    if prepared_snapshot != validated.snapshot {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "prepared ZIP did not round-trip to the submitted snapshot",
        ));
    }
    let prepared_revision = revision(&prepared);

    let initial = read_optional_file(&note_path)?;
    expected_matches_or_preserve(
        expected_revision.as_deref(),
        initial.as_deref(),
        &recovery_dir,
        &prepared,
        "initial revision check",
    )?;
    if let Some(bytes) = initial.as_deref() {
        if let Err(error) = decode_archive(bytes) {
            let folder = preserve_corrupt_draft(
                &recovery_dir,
                bytes,
                &prepared,
                &format!("current note is invalid: {error}"),
            )?;
            return Err(StorageError::new(
                StorageErrorKind::Corrupt,
                format!(
                    "current note is invalid and was left untouched; current and draft copies are in {}",
                    folder.display()
                ),
            ));
        }
    }

    if let Some(bytes) = initial.as_deref() {
        let previous_revision = revision(bytes);
        let previous_path = recovery_dir.join(format!(
            "{}-previous-{}.cuenote",
            unique_token(),
            &previous_revision[..12]
        ));
        write_new_file(&previous_path, bytes)?;
    }

    // Re-read immediately before the atomic operation to catch external updates
    // that occurred while the ZIP was being prepared or the previous copy written.
    let current_before_replace = read_optional_file(&note_path)?;
    expected_matches_or_preserve(
        expected_revision.as_deref(),
        current_before_replace.as_deref(),
        &recovery_dir,
        &prepared,
        "pre-replacement revision check",
    )?;

    if let Err(replace_error) = atomic_replace(
        &temp.0,
        &note_path,
        current_before_replace.is_some(),
    ) {
        if let Ok(current_after_failure) = read_optional_file(&note_path) {
            if !matches_expected(current_after_failure.as_deref(), expected_revision.as_deref()) {
                return match expected_matches_or_preserve(
                    expected_revision.as_deref(),
                    current_after_failure.as_deref(),
                    &recovery_dir,
                    &prepared,
                    "atomic replacement race",
                ) {
                    Err(conflict_error) => Err(conflict_error),
                    Ok(()) => Err(replace_error),
                };
            }
        }
        return Err(replace_error);
    }
    Ok(SaveResult {
        revision: prepared_revision,
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        bytes: prepared.len() as u64,
    })
}

pub fn simulate_external_update_at(root: &Path) -> Result<RevisionResult, StorageError> {
    let current = open_at(root)?;
    let current_revision = current.revision;
    let mut snapshot = current.snapshot;
    snapshot.document.title = format!("External update {}", unique_token());
    let saved = save_at(root, snapshot, Some(current_revision))?;
    Ok(RevisionResult {
        revision: saved.revision,
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    revision(bytes)
}

pub fn scratch_root(app_local_data_dir: &Path) -> PathBuf {
    app_local_data_dir.join("PROTOTYPE-wipe-me")
}

pub fn info_at(root: &Path) -> Result<PrototypeInfo, StorageError> {
    let recovery_dir = root.join("recovery");
    ensure_real_directory(root)?;
    ensure_real_directory(&recovery_dir)?;
    Ok(PrototypeInfo {
        path: root.join(NOTE_FILE).to_string_lossy().into_owned(),
        recovery_path: recovery_dir.to_string_lossy().into_owned(),
    })
}
