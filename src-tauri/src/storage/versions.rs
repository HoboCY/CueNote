use super::{
    digest_bytes, inspect_note, is_link_or_reparse, open_read_nofollow, sync_directory_chain,
    write_and_verify_package, OpenedNote, ResolvedTarget, StorageError,
};
use crate::note_format::MAX_PACKAGE_BYTES;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const VERSIONS_DIRECTORY: &str = "versions";
const GROUPS_DIRECTORY: &str = "groups";
const LOCKS_DIRECTORY: &str = ".locks";
const STORE_LOCK_NAME: &str = "versions.lock";
const PACKAGE_NAME: &str = "note.cuenote";
const METADATA_NAME: &str = "metadata.json";
const RESOLVED_NAME: &str = "resolved";
const RESOLVED_MARKER: &[u8] = b"cuenote-preserved-version-resolved-v1\n";
const VERSION_METADATA_LIMIT: u64 = 64 * 1024;
const VERSION_TEXT_LIMIT: usize = 16 * 1024;
const TITLE_DISPLAY_CHARACTERS: usize = 180;
const VERSION_SCAN_LIMIT: usize = 100_000;
const WARNING_LIMIT: usize = 100;
const VERSION_SCHEMA: u32 = 1;
const WITNESS_HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreservedVersionSummary {
    pub version_id: String,
    pub note_id: String,
    pub title: String,
    pub saved_at: String,
    pub source_revision: String,
    pub revision: String,
    pub origin: String,
    pub reason: String,
    pub conflict: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionList {
    pub versions: Vec<PreservedVersionSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct PreservedNote {
    pub summary: PreservedVersionSummary,
    pub opened: OpenedNote,
    pub path: PathBuf,
}

/// Holds a shared lock on a durable observed version while an editor uses it.
/// Dropping the lease only releases the OS lock; it never removes version data.
#[derive(Debug)]
pub struct VersionLease {
    pub version_id: String,
    lock: File,
}

impl Drop for VersionLease {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionMetadata {
    schema_version: u32,
    version_id: String,
    note_id: String,
    title: String,
    saved_at: String,
    source_revision: String,
    revision: String,
    origin: String,
    origin_key: String,
    reason: String,
    conflict: bool,
}

impl VersionMetadata {
    fn summary(&self) -> PreservedVersionSummary {
        PreservedVersionSummary {
            version_id: self.version_id.clone(),
            note_id: self.note_id.clone(),
            title: self.title.clone(),
            saved_at: self.saved_at.clone(),
            source_revision: self.source_revision.clone(),
            revision: self.revision.clone(),
            origin: self.origin.clone(),
            reason: self.reason.clone(),
            conflict: self.conflict,
        }
    }
}

#[derive(Debug)]
struct StoredVersion {
    metadata: VersionMetadata,
    opened: OpenedNote,
    path: PathBuf,
}

#[derive(Debug)]
struct StoreOutcome {
    version_id: String,
    created: bool,
}

pub fn preserve_observed_version(
    root: &Path,
    opened: &OpenedNote,
    origin: &str,
    reason: &str,
) -> Result<(), StorageError> {
    let root = prepare_root(root, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备版本存储目录。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备已保存版本目录。"))?;
    let _store_lock = acquire_store_lock(&root, false)?;
    let outcome = store_version_locked(&versions, opened, origin, reason, false)?;
    if outcome.created {
        rotate_observed_versions_locked(
            &root,
            &versions,
            &outcome.version_id,
            &opened.package.document.note_id,
            &normalize_origin(origin)?.1,
        );
    }
    Ok(())
}

pub fn preserve_conflict_version(
    root: &Path,
    opened: &OpenedNote,
    origin: &str,
    reason: &str,
) -> Result<(), StorageError> {
    let root = prepare_root(root, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备版本存储目录。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备已保存版本目录。"))?;
    let _store_lock = acquire_store_lock(&root, false)?;
    store_version_locked(&versions, opened, origin, reason, true)?;
    Ok(())
}

pub fn lease_observed_version(
    root: &Path,
    opened: &OpenedNote,
    origin: &str,
    reason: &str,
) -> Result<VersionLease, StorageError> {
    let root = prepare_root(root, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备版本存储目录。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备已保存版本目录。"))?;
    let _store_lock = acquire_store_lock(&root, false)?;
    let outcome = store_version_locked(&versions, opened, origin, reason, false)?;
    let lock = acquire_version_lock(&root, &outcome.version_id, true)?;
    if outcome.created {
        rotate_observed_versions_locked(
            &root,
            &versions,
            &outcome.version_id,
            &opened.package.document.note_id,
            &normalize_origin(origin)?.1,
        );
    }
    Ok(VersionLease {
        version_id: outcome.version_id,
        lock,
    })
}

/// A durable receipt for an observed version whose package bytes were already
/// fully validated by an earlier open and preserve.
///
/// The receipt is only an optimization hint for a caller that has just
/// re-observed the exact same source bytes. It must never be used on its own to
/// skip preservation, and it never replaces the source-side full validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ObservedVersionWitness {
    pub(crate) version_id: String,
    pub(crate) source_revision: String,
    pub(crate) stored_revision: String,
}

/// Binds the durable metadata of the observed version that
/// `preserve_observed_version` (or `lease_observed_version`) just confirmed for
/// `opened`.
///
/// Precondition: the caller must have completed that preservation successfully
/// for exactly this `opened` package and `origin`, immediately before calling
/// this function. The returned witness must not be used to skip the existing
/// preservation of a new source; it only lets `verify_observed_version_witness`
/// re-check the already stored version without parsing the package again.
///
/// This function reads `metadata.json` again under the existing shared store
/// lock and requires it to bind exactly to the opened note
/// identity/title, the opened source revision, the normalized origin, and an
/// ordinary (non-conflict) version. Metadata that is unavailable or does not
/// match is an error; no package bytes are parsed or re-encoded here.
pub(crate) fn observed_version_witness(
    root: &Path,
    opened: &OpenedNote,
    origin: &str,
) -> Result<ObservedVersionWitness, StorageError> {
    let note_id = opened.package.document.note_id.as_str();
    validate_note_id(note_id)?;
    validate_revision(&opened.revision, "sourceRevision")?;
    let (display_origin, origin_key) = normalize_origin(origin)?;
    let version_id = make_version_id(false, note_id, &opened.revision, &origin_key)?;

    let root = prepare_root(root, false)?
        .ok_or_else(|| StorageError::new("versionNotFound", "已保存版本不存在；没有跳过保留。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, false)?
        .ok_or_else(|| StorageError::new("versionNotFound", "已保存版本不存在；没有跳过保留。"))?;
    let _store_lock = acquire_store_lock(&root, true)?;
    // Publication, resolution and rotation require the exclusive store lock.
    // The cached receipt does not pin the version after this read; subsequent
    // use must revalidate it. A per-version lock here adds a durable index write.
    let metadata = read_metadata(&versions, &version_id)?;
    if metadata.conflict
        || metadata.version_id != version_id
        || metadata.note_id != note_id
        || metadata.source_revision != opened.revision
        || metadata.origin != display_origin
        || metadata.origin_key != origin_key
        || metadata.title != display_title(&opened.package.document.title)
    {
        return Err(StorageError::new(
            "invalidVersion",
            "已保存的观察版本与刚完成保留的笔记包不一致；相关数据未被修改。",
        ));
    }
    Ok(ObservedVersionWitness {
        version_id: metadata.version_id,
        source_revision: metadata.source_revision,
        stored_revision: metadata.revision,
    })
}

/// Re-verifies a cached observed-version receipt against the durable stored
/// version without parsing the package or decoding any assets.
///
/// Caller contract: invoke this only after a fresh, safe, bounded streaming
/// SHA-256 of the source file equals a prior fully validated per-file cache
/// entry for the same `source_revision`, `origin`, `note_id`, and `title`.
/// This function never substitutes for the existing full open+preserve path:
///
/// - `Ok(true)`: the stored metadata, deterministic ordinary version id,
///   rotation grouping, and the complete bounded stored package bytes all still
///   match the receipt. The caller may reuse the previous full validation.
/// - `Ok(false)`: the receipt is missing or does not match durable state
///   (changed, corrupt, oversized, or a guard failed). The caller must fall
///   back to the existing full open+preserve path.
/// - `Err`: verification could not be coordinated (invalid arguments or lock
///   acquisition failure). The caller must also fall back to the existing full
///   open+preserve path.
///
/// The verifier only takes the existing shared store lock and never
/// writes, deletes, rotates, or re-permissions version data. The stored
/// revision may differ from the source revision because preservation may
/// re-encode the package; the stored package is hashed against its own stored
/// revision, never against the source revision.
pub(crate) fn verify_observed_version_witness(
    root: &Path,
    witness: &ObservedVersionWitness,
    source_revision: &str,
    origin: &str,
    note_id: &str,
    title: &str,
) -> Result<bool, StorageError> {
    if witness.source_revision != source_revision
        || !is_lower_hex(&witness.version_id, 64)
        || !is_lower_hex(&witness.source_revision, 64)
        || !is_lower_hex(&witness.stored_revision, 64)
    {
        return Ok(false);
    }
    validate_note_id(note_id)?;
    let (display_origin, origin_key) = normalize_origin(origin)?;
    if make_version_id(false, note_id, source_revision, &origin_key)? != witness.version_id {
        return Ok(false);
    }

    let Some(root) = prepare_root(root, false)? else {
        return Ok(false);
    };
    let Some(versions) = ensure_child_directory(&root, VERSIONS_DIRECTORY, false)? else {
        return Ok(false);
    };
    let _store_lock = acquire_store_lock(&root, true)?;
    // Like open_version, this excludes cooperating version-store writers for
    // the whole read. VersionLease keeps its separate lock beyond that scope.

    let metadata = match read_metadata(&versions, &witness.version_id) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(false),
    };
    if metadata.conflict
        || metadata.version_id != witness.version_id
        || metadata.note_id != note_id
        || metadata.source_revision != source_revision
        || metadata.revision != witness.stored_revision
        || metadata.origin != display_origin
        || metadata.origin_key != origin_key
        || metadata.title != display_title(title)
    {
        return Ok(false);
    }

    let Ok(group_id) = make_group_id(note_id, &origin_key) else {
        return Ok(false);
    };
    let Some(groups) = ensure_child_directory(&versions, GROUPS_DIRECTORY, false)? else {
        return Ok(false);
    };
    let Some(group) = ensure_child_directory(&groups, &group_id, false)? else {
        return Ok(false);
    };
    if validate_group_member(&group, &witness.version_id).is_err() {
        return Ok(false);
    }

    let directory = version_directory(&versions, &witness.version_id);
    let Ok(stable_directory) = super::publication::StableDirectory::open(&directory) else {
        return Ok(false);
    };
    if stable_directory.ensure_current().is_err() {
        return Ok(false);
    }
    let package_path = directory.join(PACKAGE_NAME);
    let mut package = match open_read_nofollow(&package_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(StorageError::io("无法安全打开已保存版本笔记包", error)),
    };
    if validate_regular_file(&package, "已保存版本笔记包不是普通文件").is_err() {
        return Ok(false);
    }

    // The stored package must hash over its complete bounded bytes; metadata,
    // size, or timestamps alone are never sufficient for a positive receipt.
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; WITNESS_HASH_BUFFER_BYTES];
    let mut total_bytes: u64 = 0;
    loop {
        let read = match package.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(StorageError::io("无法读取已保存版本笔记包", error)),
        };
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        if total_bytes > MAX_PACKAGE_BYTES {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
    }
    if format!("{:x}", hasher.finalize()) != witness.stored_revision {
        return Ok(false);
    }
    Ok(true)
}

/// Pins a previously observed package as a conflict without consulting its mutable origin.
pub fn pin_observed_version(
    root: &Path,
    source_revision: &str,
    origin: &str,
    note_id: &str,
    reason: &str,
) -> Result<(), StorageError> {
    validate_note_id(note_id)?;
    validate_revision(source_revision, "sourceRevision")?;
    let (_, origin_key) = normalize_origin(origin)?;
    validate_display_text(reason, "reason")?;

    let root = prepare_root(root, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备版本存储目录。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备已保存版本目录。"))?;
    let _store_lock = acquire_store_lock(&root, false)?;
    let observed_id = make_version_id(false, note_id, source_revision, &origin_key)?;
    let observed = read_stored_version(&versions, &observed_id).map_err(|error| {
        if error.kind == "versionNotFound" {
            StorageError::new(
                "versionConflict",
                "找不到与指定基线一致的已保存观察版本；没有读取可变来源文件。",
            )
        } else {
            error
        }
    })?;
    if observed.metadata.conflict
        || observed.metadata.note_id != note_id
        || observed.metadata.source_revision != source_revision
        || observed.metadata.origin_key != origin_key
    {
        return Err(StorageError::new(
            "versionConflict",
            "找不到与指定基线一致的已保存观察版本；没有读取可变来源文件。",
        ));
    }
    let opened = OpenedNote {
        package: observed.opened.package,
        revision: source_revision.to_owned(),
    };
    store_version_locked(&versions, &opened, origin, reason, true)?;
    Ok(())
}

pub fn list_versions(root: &Path) -> Result<VersionList, StorageError> {
    let Some(root) = prepare_root(root, false)? else {
        return Ok(VersionList {
            versions: Vec::new(),
            warnings: Vec::new(),
        });
    };
    let Some(versions_directory) = ensure_child_directory(&root, VERSIONS_DIRECTORY, false)? else {
        return Ok(VersionList {
            versions: Vec::new(),
            warnings: Vec::new(),
        });
    };
    let _store_lock = acquire_store_lock(&root, true)?;
    let mut versions = Vec::new();
    let mut warnings = Vec::new();
    let entries = fs::read_dir(&versions_directory)
        .map_err(|error| StorageError::io("无法扫描已保存版本目录", error))?;
    let mut scanned = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                push_warning(
                    &mut warnings,
                    "无法读取一个版本目录项；相关数据已保留。".into(),
                );
                continue;
            }
        };
        scanned += 1;
        ensure_scan_limit(scanned)?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            push_warning(
                &mut warnings,
                "发现无法识别的版本目录项；相关数据已保留。".into(),
            );
            continue;
        };
        if id == GROUPS_DIRECTORY {
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| StorageError::io("无法检查版本分组目录", error))?;
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                push_warning(
                    &mut warnings,
                    "版本分组索引不是实际目录；相关数据已保留。".into(),
                );
            }
            continue;
        }
        if !is_lower_hex(&id, 64) {
            push_warning(
                &mut warnings,
                "发现未识别的版本目录项；相关数据已保留且未打开。".into(),
            );
            continue;
        }
        match read_metadata(&versions_directory, &id) {
            Ok(metadata) => {
                match read_resolved_marker(&version_directory(&versions_directory, &id)) {
                    Ok(true) => {}
                    Ok(false) => versions.push(metadata.summary()),
                    Err(_) => {
                        push_warning(
                            &mut warnings,
                            "一个版本的处理标记无法验证；该版本仍显示为待处理。".into(),
                        );
                        versions.push(metadata.summary());
                    }
                }
            }
            Err(_) => push_warning(
                &mut warnings,
                "一个版本的元数据无法验证；相关数据已保留。".into(),
            ),
        }
    }
    versions.sort_by(|left, right| {
        right
            .saved_at
            .cmp(&left.saved_at)
            .then_with(|| left.version_id.cmp(&right.version_id))
    });
    Ok(VersionList { versions, warnings })
}

pub fn open_version(root: &Path, version_id: &str) -> Result<PreservedNote, StorageError> {
    validate_version_id(version_id)?;
    let root = prepare_root(root, false)?
        .ok_or_else(|| StorageError::new("versionNotFound", "已保存版本不存在。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, false)?
        .ok_or_else(|| StorageError::new("versionNotFound", "已保存版本不存在。"))?;
    let _store_lock = acquire_store_lock(&root, true)?;
    let stored = read_stored_version(&versions, version_id)?;
    Ok(PreservedNote {
        summary: stored.metadata.summary(),
        opened: stored.opened,
        path: stored.path,
    })
}

pub fn resolve_version(root: &Path, version_id: &str) -> Result<(), StorageError> {
    validate_version_id(version_id)?;
    let root = prepare_root(root, false)?
        .ok_or_else(|| StorageError::new("versionNotFound", "已保存版本不存在。"))?;
    let versions = ensure_child_directory(&root, VERSIONS_DIRECTORY, false)?
        .ok_or_else(|| StorageError::new("versionNotFound", "已保存版本不存在。"))?;
    let _store_lock = acquire_store_lock(&root, false)?;
    let _stored = read_stored_version(&versions, version_id)?;
    let directory = version_directory(&versions, version_id);
    match read_resolved_marker(&directory) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => return Err(error),
    }
    let marker = directory.join(RESOLVED_NAME);
    let mut file = create_new_file(&marker, true)?;
    file.write_all(RESOLVED_MARKER)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|error| StorageError::io("无法同步版本处理标记", error))?;
    sync_directory_chain(&directory).map_err(|error| {
        StorageError::io(
            "版本已标记处理，但无法确认目录同步；版本内容仍已保留",
            error,
        )
    })?;
    Ok(())
}

fn store_version_locked(
    versions: &Path,
    opened: &OpenedNote,
    origin: &str,
    reason: &str,
    conflict: bool,
) -> Result<StoreOutcome, StorageError> {
    let note_id = opened.package.document.note_id.as_str();
    validate_note_id(note_id)?;
    validate_revision(&opened.revision, "sourceRevision")?;
    let (display_origin, origin_key) = normalize_origin(origin)?;
    validate_display_text(reason, "reason")?;
    let version_id = make_version_id(conflict, note_id, &opened.revision, &origin_key)?;
    let durability = VersionDurability::new(versions)
        .map_err(|error| StorageError::io("无法确认版本存储目录同步", error))?;
    let destination = version_directory(versions, &version_id);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => {
            return Err(StorageError::new(
                "unsafePath",
                "版本标识已被非目录项占用；相关数据未被修改。",
            ));
        }
        Ok(_) => {
            let existing = read_stored_version(versions, &version_id)?;
            if existing.metadata.note_id == note_id
                && existing.metadata.source_revision == opened.revision
                && existing.metadata.origin_key == origin_key
                && existing.metadata.conflict == conflict
            {
                if !conflict {
                    ensure_group_member(&durability, note_id, &origin_key, &version_id).map_err(
                        |error| {
                            version_publish_error("已保存版本有效，但无法更新其轮换索引", error)
                        },
                    )?;
                }
                return Ok(StoreOutcome {
                    version_id,
                    created: false,
                });
            }
            return Err(StorageError::new(
                "versionConflict",
                "版本标识对应的元数据与请求不一致；现有数据已保留。",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StorageError::io("无法检查已保存版本目录", error)),
    }

    let staging = create_staging_directory(versions, &durability)?;
    let package_path = staging.join(PACKAGE_NAME);
    let package_file = create_new_file(&package_path, true).map_err(with_candidate_warning)?;
    let revision =
        write_and_verify_package(package_file, &opened.package).map_err(with_candidate_warning)?;
    if opened.package.document.note_id != note_id {
        return Err(with_candidate_warning(StorageError::new(
            "invalid_document",
            "准备保存的版本笔记身份发生变化；有效候选数据已保留。",
        )));
    }

    let metadata = VersionMetadata {
        schema_version: VERSION_SCHEMA,
        version_id: version_id.clone(),
        note_id: note_id.to_owned(),
        title: display_title(&opened.package.document.title),
        saved_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        source_revision: opened.revision.clone(),
        revision,
        origin: display_origin,
        origin_key,
        reason: reason.to_owned(),
        conflict,
    };
    let metadata_bytes = serde_json::to_vec(&metadata).map_err(|error| {
        with_candidate_warning(StorageError::new(
            "versionMetadata",
            format!("无法编码版本元数据：{error}"),
        ))
    })?;
    if metadata_bytes.len() as u64 > VERSION_METADATA_LIMIT {
        return Err(with_candidate_warning(StorageError::new(
            "budget_exceeded",
            "版本元数据超过大小上限；有效笔记包已保留。",
        )));
    }
    let metadata_path = staging.join(METADATA_NAME);
    let mut metadata_file =
        create_new_file(&metadata_path, true).map_err(with_candidate_warning)?;
    metadata_file
        .write_all(&metadata_bytes)
        .and_then(|_| metadata_file.flush())
        .and_then(|_| metadata_file.sync_all())
        .map_err(|error| version_publish_error("无法同步版本元数据", error))?;
    drop(metadata_file);
    durability
        .sync(&staging, "candidate-ready")
        .map_err(|error| version_publish_error("候选版本已校验，但无法同步候选目录", error))?;

    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(with_candidate_warning(StorageError::new(
                "versionConflict",
                "版本标识在发布期间被其他目录项占用；有效候选数据已保留。",
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(with_candidate_warning(StorageError::io(
                "无法检查版本发布位置",
                error,
            )))
        }
    }
    fs::rename(&staging, &destination).map_err(|error| {
        version_publish_error("已验证的候选版本无法发布；候选数据已保留", error)
    })?;
    durability.sync(versions, "published").map_err(|error| {
        version_publish_error("版本已发布并验证，但无法确认版本目录同步", error)
    })?;
    if !conflict {
        ensure_group_member(&durability, note_id, &metadata.origin_key, &version_id).map_err(
            |error| version_publish_error("版本已发布并验证，但无法更新其轮换索引", error),
        )?;
    }
    Ok(StoreOutcome {
        version_id,
        created: true,
    })
}

fn rotate_observed_versions_locked(
    root: &Path,
    versions: &Path,
    protected_id: &str,
    note_id: &str,
    origin_key: &str,
) {
    #[cfg(feature = "perf-acceptance")]
    let mut trace = crate::performance::rotation_trace::RotationTrace::new();
    let group_id = match make_group_id(note_id, origin_key) {
        Ok(group_id) => group_id,
        Err(_) => return,
    };
    let Some(groups) = ensure_child_directory(versions, GROUPS_DIRECTORY, false)
        .ok()
        .flatten()
    else {
        return;
    };
    let Some(group) = ensure_child_directory(&groups, &group_id, false)
        .ok()
        .flatten()
    else {
        return;
    };
    let entries = match fs::read_dir(&group) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    // At most 30 directory members cannot contain more than 30 eligible
    // versions. No deletion is possible, so avoid decoding every retained
    // package just to establish that fact. At larger counts retain the full
    // verification and lease checks below, including for malformed members.
    let entries = match entries
        .take(VERSION_SCAN_LIMIT + 1)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(entries) if entries.len() <= 30 => {
            #[cfg(feature = "perf-acceptance")]
            if let Some(trace) = &mut trace {
                trace.members = entries.len();
            }
            return;
        }
        Ok(entries) if entries.len() <= VERSION_SCAN_LIMIT => {
            #[cfg(feature = "perf-acceptance")]
            if let Some(trace) = &mut trace {
                trace.members = entries.len();
            }
            entries
        }
        _ => return,
    };
    let mut matching = Vec::new();
    let mut scanned = 0usize;
    for entry in entries {
        scanned += 1;
        if scanned > VERSION_SCAN_LIMIT {
            return;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            return;
        };
        if !is_lower_hex(&id, 64) {
            return;
        }
        if validate_group_member(&group, &id).is_err() {
            return;
        }
        let Ok(metadata) = read_metadata(versions, &id) else {
            continue;
        };
        let unresolved = matches!(
            read_resolved_marker(&version_directory(versions, &id)),
            Ok(false)
        );
        if metadata.conflict
            || metadata.note_id != note_id
            || metadata.origin_key != origin_key
            || !unresolved
        {
            continue;
        }
        #[cfg(feature = "perf-acceptance")]
        let started = trace.as_ref().map(|_| std::time::Instant::now());
        let stored = read_stored_version(versions, &id);
        #[cfg(feature = "perf-acceptance")]
        if let (Some(trace), Some(started)) = (&mut trace, started) {
            trace.parsed(started, false);
        }
        let Ok(stored) = stored else {
            continue;
        };
        let _ = stored;
        let saved_at = DateTime::parse_from_rfc3339(&metadata.saved_at)
            .map(|value| value.timestamp_millis())
            .unwrap_or(i64::MIN);
        matching.push((id, saved_at));
    }
    matching.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    #[cfg(feature = "perf-acceptance")]
    if let Some(trace) = &mut trace {
        trace.eligible = matching.len();
    }
    for (id, _) in matching.into_iter().skip(30) {
        if id == protected_id {
            continue;
        }
        let Ok(lock) = acquire_version_lock(root, &id, false) else {
            continue;
        };
        if lock.try_lock().is_err() {
            continue;
        }
        let directory = version_directory(versions, &id);
        if !directory_contains_only_known_files(&directory) {
            continue;
        }
        // Rotation removes only complete, verified, unleased observed versions.
        #[cfg(feature = "perf-acceptance")]
        let started = trace.as_ref().map(|_| std::time::Instant::now());
        let stored = read_stored_version(versions, &id);
        #[cfg(feature = "perf-acceptance")]
        if let (Some(trace), Some(started)) = (&mut trace, started) {
            trace.parsed(started, true);
        }
        if let Ok(stored) = stored {
            if stored.metadata.conflict
                || stored.metadata.note_id != note_id
                || stored.metadata.origin_key != origin_key
            {
                continue;
            }
            drop(stored);
            if fs::remove_file(directory.join(METADATA_NAME)).is_err() {
                continue;
            }
            if fs::remove_file(directory.join(PACKAGE_NAME)).is_err() {
                continue;
            }
            if fs::remove_dir(&directory).is_ok() {
                #[cfg(feature = "perf-acceptance")]
                if let Some(trace) = &mut trace {
                    trace.deleted += 1;
                }
                let _ = fs::remove_file(group.join(&id));
                let _ = sync_directory_chain(&group);
            }
        }
    }
}

fn ensure_group_member(
    durability: &VersionDurability<'_>,
    note_id: &str,
    origin_key: &str,
    version_id: &str,
) -> Result<(), StorageError> {
    validate_version_id(version_id)?;
    let groups = durability.ensure_child(durability.versions, GROUPS_DIRECTORY)?;
    let group_id = make_group_id(note_id, origin_key)?;
    let group = durability.ensure_child(&groups, &group_id)?;
    let path = group.join(version_id);
    let file = match fs::symlink_metadata(&path) {
        Ok(_) => open_group_member(&group, version_id, true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_new_file(&path, true)?,
        Err(error) => return Err(StorageError::io("无法检查版本分组索引项", error)),
    };
    // An earlier attempt may have created the member but failed its durability
    // barrier. Existence and valid bytes alone do not establish a durable retry.
    sync_version_file(&file, &path)
        .map_err(|error| StorageError::io("无法同步版本分组索引项", error))?;
    durability
        .sync(&group, "member-directory")
        .map_err(|error| StorageError::io("无法同步版本分组索引", error))
}

fn sync_version_file(file: &File, path: &Path) -> io::Result<()> {
    version_sync_checkpoint("member-file", path)?;
    file.sync_all()
}

fn version_sync_checkpoint(stage: &str, path: &Path) -> io::Result<()> {
    #[cfg(test)]
    tests::sync_checkpoint(stage, path)?;
    #[cfg(not(test))]
    let _ = (stage, path);
    Ok(())
}

/// One operation under the exclusive store lock. Ancestors of `versions` are
/// made durable once; every later mutation still syncs its chain through
/// `versions`, inclusive. No state is reused across operations or retries.
struct VersionDurability<'a> {
    versions: &'a Path,
    #[cfg(unix)]
    anchor: File,
}

impl<'a> VersionDurability<'a> {
    fn new(versions: &'a Path) -> io::Result<Self> {
        validate_sync_directory(versions)?;
        let context = Self {
            versions,
            #[cfg(unix)]
            anchor: nofollow_open_options().read(true).open(versions)?,
        };
        let mut current = Some(versions);
        while let Some(path) = current {
            version_sync_checkpoint("entry", path)?;
            super::sync_parent_directory(path)?;
            current = path.parent();
        }
        context.check_anchor()?;
        Ok(context)
    }

    fn check_anchor(&self) -> io::Result<()> {
        validate_sync_directory(self.versions)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let original = self.anchor.metadata()?;
            let current = fs::symlink_metadata(self.versions)?;
            if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
                return Err(io::Error::other("Version durability anchor changed"));
            }
        }
        Ok(())
    }

    fn sync(&self, path: &Path, stage: &str) -> io::Result<()> {
        let suffix = path
            .strip_prefix(self.versions)
            .map_err(|_| io::Error::other("Directory is outside version durability scope"))?;
        if suffix
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::other("Invalid version durability path"));
        }
        self.check_anchor()?;
        // Validate the whole descendant chain before opening any directory.
        let mut checked = self.versions.to_path_buf();
        for component in suffix.components() {
            checked.push(component.as_os_str());
            validate_sync_directory(&checked)?;
        }
        let mut current = path;
        loop {
            version_sync_checkpoint(stage, current)?;
            super::sync_parent_directory(current)?;
            if current == self.versions {
                break;
            }
            current = current
                .parent()
                .ok_or_else(|| io::Error::other("Missing version ancestor"))?;
        }
        self.check_anchor()
    }

    fn ensure_child(&self, parent: &Path, name: &str) -> Result<PathBuf, StorageError> {
        let path = parent.join(name);
        check_or_create_directory_with(&path, true, |parent| {
            self.sync(parent, "directory-created")
        })?;
        Ok(path)
    }
}

fn validate_sync_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(io::Error::other(
            "Version sync path is not a real directory",
        ));
    }
    Ok(())
}

fn validate_group_member(group: &Path, version_id: &str) -> Result<(), StorageError> {
    open_group_member(group, version_id, false).map(drop)
}

fn open_group_member(group: &Path, version_id: &str, writable: bool) -> Result<File, StorageError> {
    validate_version_id(version_id)?;
    let path = group.join(version_id);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| StorageError::io("无法检查版本分组索引项", error))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() != 0 {
        return Err(StorageError::new(
            "unsafePath",
            "版本分组索引项不是预期的普通空文件；相关数据已保留。",
        ));
    }
    let file = if writable {
        let mut options = nofollow_open_options();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0x0000_0001);
        }
        options.open(&path)
    } else {
        open_read_nofollow(&path)
    }
    .map_err(|error| StorageError::io("无法安全打开版本分组索引项", error))?;
    validate_regular_file(&file, "版本分组索引项不是普通文件")?;
    if file
        .metadata()
        .map_err(|error| StorageError::io("无法检查版本分组索引项", error))?
        .len()
        != 0
    {
        return Err(StorageError::new(
            "invalidVersionIndex",
            "版本分组索引项内容无效；相关数据已保留。",
        ));
    }
    Ok(file)
}

fn directory_contains_only_known_files(directory: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(directory) else {
        return false;
    };
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return false;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut package = false;
    let mut metadata_file = false;
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        let Ok(entry_metadata) = fs::symlink_metadata(entry.path()) else {
            return false;
        };
        if is_link_or_reparse(&entry_metadata) || !entry_metadata.is_file() {
            return false;
        }
        match name {
            PACKAGE_NAME if !package => package = true,
            METADATA_NAME if !metadata_file => metadata_file = true,
            _ => return false,
        }
    }
    package && metadata_file
}

fn read_stored_version(versions: &Path, version_id: &str) -> Result<StoredVersion, StorageError> {
    validate_version_id(version_id)?;
    let metadata = read_metadata(versions, version_id)?;
    let directory = version_directory(versions, version_id);
    let package_path = directory.join(PACKAGE_NAME);
    let target = ResolvedTarget {
        path: package_path.clone(),
        directory: super::publication::StableDirectory::open(&directory)?,
        parent: directory,
    };
    let opened = inspect_note(&target)?.ok_or_else(|| {
        StorageError::new("invalidVersion", "保存版本缺少笔记包；相关数据已保留。")
    })?;
    if opened.package.document.note_id != metadata.note_id
        || display_title(&opened.package.document.title) != metadata.title
        || opened.revision != metadata.revision
    {
        return Err(StorageError::new(
            "invalidVersion",
            "保存版本的笔记身份或内容指纹与元数据不一致；相关数据已保留。",
        ));
    }
    Ok(StoredVersion {
        metadata,
        opened,
        path: package_path,
    })
}

fn read_metadata(versions: &Path, version_id: &str) -> Result<VersionMetadata, StorageError> {
    let directory = version_directory(versions, version_id);
    let directory_metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(StorageError::new(
                "versionNotFound",
                "已保存版本不存在；没有打开其他路径。",
            ));
        }
        Err(error) => return Err(StorageError::io("无法检查保存版本目录", error)),
    };
    if is_link_or_reparse(&directory_metadata) || !directory_metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "保存版本必须位于实际目录中；相关数据未被修改。",
        ));
    }
    let path = directory.join(METADATA_NAME);
    let mut file = open_read_nofollow(&path)
        .map_err(|error| StorageError::io("无法安全打开版本元数据", error))?;
    validate_regular_file(&file, "版本元数据不是普通文件")?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(VERSION_METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::io("无法读取版本元数据", error))?;
    if bytes.len() as u64 > VERSION_METADATA_LIMIT {
        return Err(StorageError::new(
            "budget_exceeded",
            "版本元数据超过大小上限；相关数据已保留。",
        ));
    }
    let metadata: VersionMetadata = serde_json::from_slice(&bytes).map_err(|error| {
        StorageError::new("invalidVersionMetadata", format!("版本元数据无效：{error}"))
    })?;
    validate_metadata(&metadata, version_id)?;
    Ok(metadata)
}

fn validate_metadata(metadata: &VersionMetadata, expected_id: &str) -> Result<(), StorageError> {
    validate_version_id(expected_id)?;
    if metadata.schema_version != VERSION_SCHEMA
        || metadata.version_id != expected_id
        || metadata.title.len() > VERSION_TEXT_LIMIT
        || metadata.origin.len() > VERSION_TEXT_LIMIT
        || metadata.origin_key.len() > VERSION_TEXT_LIMIT
        || metadata.reason.len() > VERSION_TEXT_LIMIT
    {
        return Err(StorageError::new(
            "invalidVersionMetadata",
            "保存版本元数据字段不受支持或超过上限。",
        ));
    }
    validate_note_id(&metadata.note_id)?;
    validate_revision(&metadata.source_revision, "sourceRevision")?;
    validate_revision(&metadata.revision, "revision")?;
    validate_display_text(&metadata.origin, "origin")?;
    validate_display_text(&metadata.origin_key, "originKey")?;
    validate_display_text(&metadata.reason, "reason")?;
    DateTime::parse_from_rfc3339(&metadata.saved_at)
        .map_err(|_| StorageError::new("invalidVersionMetadata", "保存版本时间格式无效。"))?;
    let expected = make_version_id(
        metadata.conflict,
        &metadata.note_id,
        &metadata.source_revision,
        &metadata.origin_key,
    )?;
    if expected != metadata.version_id {
        return Err(StorageError::new(
            "invalidVersionMetadata",
            "保存版本标识与元数据内容不一致。",
        ));
    }
    Ok(())
}

fn read_resolved_marker(directory: &Path) -> Result<bool, StorageError> {
    let path = directory.join(RESOLVED_NAME);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io("无法检查版本处理标记", error)),
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => Err(
            StorageError::new("unsafePath", "版本处理标记不是普通文件；相关数据已保留。"),
        ),
        Ok(_) => {
            let mut file = open_read_nofollow(&path)
                .map_err(|error| StorageError::io("无法安全打开版本处理标记", error))?;
            validate_regular_file(&file, "版本处理标记不是普通文件")?;
            let mut bytes = Vec::new();
            (&mut file)
                .take(RESOLVED_MARKER.len() as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| StorageError::io("无法读取版本处理标记", error))?;
            if bytes == RESOLVED_MARKER {
                Ok(true)
            } else {
                Err(StorageError::new(
                    "invalidVersionMarker",
                    "版本处理标记内容无效；相关数据已保留。",
                ))
            }
        }
    }
}

fn prepare_root(root: &Path, create: bool) -> Result<Option<PathBuf>, StorageError> {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| StorageError::io("无法解析版本存储目录", error))?
            .join(root)
    };
    let mut current = PathBuf::new();
    let mut found_anchor = false;
    for component in absolute.components() {
        match component {
            Component::Prefix(_) => {
                current.push(component.as_os_str());
            }
            Component::RootDir => {
                current.push(component.as_os_str());
                if !current.as_os_str().is_empty() {
                    if !check_or_create_directory(&current, create)? {
                        return Ok(None);
                    }
                    found_anchor = true;
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(StorageError::new(
                    "unsafePath",
                    "版本存储目录不能包含父目录跳转。",
                ));
            }
            Component::Normal(name) => {
                current.push(name);
                if !found_anchor {
                    continue;
                }
                if !check_or_create_directory(&current, create)? {
                    return Ok(None);
                }
            }
        }
    }
    if current.as_os_str().is_empty() {
        return Err(StorageError::new("unsafePath", "版本存储目录无法解析。"));
    }
    match fs::symlink_metadata(&current) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => Err(
            StorageError::new("unsafePath", "版本存储目录及其父目录必须是实际目录。"),
        ),
        Ok(_) => fs::canonicalize(&current)
            .map(Some)
            .map_err(|error| StorageError::io("无法解析版本存储目录", error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound && !create => Ok(None),
        Err(error) => Err(StorageError::io("无法检查版本存储目录", error)),
    }
}

fn check_or_create_directory(path: &Path, create: bool) -> Result<bool, StorageError> {
    check_or_create_directory_with(path, create, sync_directory_chain)
}

fn check_or_create_directory_with(
    path: &Path,
    create: bool,
    sync_parent: impl Fn(&Path) -> io::Result<()>,
) -> Result<bool, StorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => Err(
            StorageError::new("unsafePath", "版本存储目录及其父目录必须是实际目录。"),
        ),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound && !create => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| StorageError::new("unsafePath", "版本存储目录缺少可验证父目录。"))?;
            match fs::create_dir(path) {
                Ok(()) => {
                    sync_parent(parent).map_err(|error| {
                        StorageError::io("版本存储目录已创建但无法确认父目录同步", error)
                    })?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(StorageError::io("无法创建版本存储目录", error)),
            }
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| StorageError::io("无法检查新建版本存储目录", error))?;
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new(
                    "unsafePath",
                    "新建版本存储目录不是实际目录；相关数据未被修改。",
                ));
            }
            Ok(true)
        }
        Err(error) => Err(StorageError::io("无法检查版本存储目录", error)),
    }
}

fn ensure_child_directory(
    parent: &Path,
    name: &str,
    create: bool,
) -> Result<Option<PathBuf>, StorageError> {
    let path = parent.join(name);
    if !check_or_create_directory(&path, create)? {
        return Ok(None);
    }
    Ok(Some(path))
}

fn acquire_store_lock(root: &Path, shared: bool) -> Result<File, StorageError> {
    let locks = ensure_child_directory(root, LOCKS_DIRECTORY, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备版本锁目录。"))?;
    let path = locks.join(STORE_LOCK_NAME);
    let file = open_lock_file(&path)?;
    if shared {
        file.lock_shared()
            .map_err(|error| StorageError::io("无法读取版本存储锁", error))?;
    } else {
        file.lock()
            .map_err(|error| StorageError::io("无法取得版本存储锁", error))?;
    }
    Ok(file)
}

fn acquire_version_lock(root: &Path, version_id: &str, shared: bool) -> Result<File, StorageError> {
    validate_version_id(version_id)?;
    let locks = ensure_child_directory(root, LOCKS_DIRECTORY, true)?
        .ok_or_else(|| StorageError::new("io", "无法准备版本锁目录。"))?;
    let path = locks.join(format!("{version_id}.lock"));
    let file = open_lock_file(&path)?;
    if shared {
        file.lock_shared()
            .map_err(|error| StorageError::io("无法固定已保存版本", error))?;
    }
    Ok(file)
}

fn open_lock_file(path: &Path) -> Result<File, StorageError> {
    for _ in 0..3 {
        match fs::symlink_metadata(path) {
            Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
                return Err(StorageError::new(
                    "unsafePath",
                    "版本锁路径必须是普通文件；相关数据未被修改。",
                ));
            }
            Ok(_) => {
                let file = nofollow_open_options()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|error| StorageError::io("无法打开版本锁文件", error))?;
                validate_regular_file(&file, "版本锁路径不是普通文件")?;
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let file = nofollow_open_options()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(path);
                match file {
                    Ok(file) => {
                        validate_regular_file(&file, "新建版本锁路径不是普通文件")?;
                        file.sync_all()
                            .map_err(|error| StorageError::io("无法同步版本锁文件", error))?;
                        if let Some(parent) = path.parent() {
                            sync_directory_chain(parent)
                                .map_err(|error| StorageError::io("无法同步版本锁目录", error))?;
                        }
                        return Ok(file);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(StorageError::io("无法创建版本锁文件", error)),
                }
            }
            Err(error) => return Err(StorageError::io("无法检查版本锁文件", error)),
        }
    }
    Err(StorageError::new(
        "io",
        "多次尝试后仍无法安全打开版本锁文件。",
    ))
}

fn nofollow_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000);
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0000_0100);
    }
    options
}

fn create_new_file(path: &Path, read_write: bool) -> Result<File, StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "版本文件缺少父目录。"))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| StorageError::io("无法检查版本文件父目录", error))?;
    if is_link_or_reparse(&parent_metadata) || !parent_metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "版本文件父目录必须是实际目录。",
        ));
    }
    let mut options = nofollow_open_options();
    options.write(true).create_new(true);
    if read_write {
        options.read(true);
    }
    let file = options
        .open(path)
        .map_err(|error| StorageError::io("无法创建新的版本文件", error))?;
    validate_regular_file(&file, "新建版本文件不是普通文件")?;
    Ok(file)
}

fn validate_regular_file(file: &File, message: &str) -> Result<(), StorageError> {
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io("无法检查版本文件", error))?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(StorageError::new("unsafePath", message));
    }
    Ok(())
}

fn create_staging_directory(
    versions: &Path,
    durability: &VersionDurability<'_>,
) -> Result<PathBuf, StorageError> {
    for _ in 0..4 {
        let path = versions.join(format!(".pending-{}", Uuid::new_v4().simple()));
        match fs::create_dir(&path) {
            Ok(()) => {
                durability
                    .sync(versions, "candidate-created")
                    .map_err(|error| {
                        version_publish_error("候选版本目录已创建但无法同步", error)
                    })?;
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| StorageError::io("无法检查候选版本目录", error))?;
                if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                    return Err(StorageError::new(
                        "unsafePath",
                        "候选版本目录不是实际目录；相关数据已保留。",
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StorageError::io("无法创建候选版本目录", error)),
        }
    }
    Err(StorageError::new(
        "io",
        "多次尝试后仍无法创建唯一版本候选目录。",
    ))
}

fn version_publish_error(context: &str, error: impl std::fmt::Display) -> StorageError {
    let mut result = StorageError::new("versionPublish", format!("{context}：{error}"));
    result.recovery_error =
        Some("版本候选目录中的已写入数据已保留；可通过版本列表检查。".to_owned());
    result
}

fn with_candidate_warning(mut error: StorageError) -> StorageError {
    if error.recovery_error.is_none() {
        error.recovery_error =
            Some("候选目录中已经写入的数据已保留；可通过版本列表检查。".to_owned());
    }
    error
}

fn normalize_origin(origin: &str) -> Result<(String, String), StorageError> {
    validate_display_text(origin, "origin")?;
    #[cfg(windows)]
    {
        let mut display = origin.replace('/', "\\");
        let lower = display.to_ascii_lowercase();
        if lower.starts_with("\\\\?\\unc\\") {
            display = format!("\\\\{}", &display[8..]);
        } else if lower.starts_with("\\\\?\\") {
            display = display[4..].to_owned();
        }
        if Path::new(&display).is_absolute() {
            if display.as_bytes().get(1) == Some(&b':')
                && display.as_bytes()[0].is_ascii_alphabetic()
            {
                let drive = display[..1].to_ascii_uppercase();
                display.replace_range(0..1, &drive);
            }
            return Ok((display.clone(), display));
        }
    }
    Ok((origin.to_owned(), origin.to_owned()))
}

fn make_version_id(
    conflict: bool,
    note_id: &str,
    source_revision: &str,
    origin_key: &str,
) -> Result<String, StorageError> {
    let key = serde_json::to_vec(&(
        VERSION_SCHEMA,
        conflict,
        note_id,
        source_revision,
        origin_key,
    ))
    .map_err(|error| StorageError::new("versionId", format!("无法生成版本标识：{error}")))?;
    Ok(digest_bytes(&key))
}

fn make_group_id(note_id: &str, origin_key: &str) -> Result<String, StorageError> {
    validate_note_id(note_id)?;
    validate_display_text(origin_key, "originKey")?;
    let key = serde_json::to_vec(&(VERSION_SCHEMA, note_id, origin_key)).map_err(|error| {
        StorageError::new("versionId", format!("无法生成版本分组标识：{error}"))
    })?;
    Ok(digest_bytes(&key))
}

fn validate_version_id(value: &str) -> Result<(), StorageError> {
    if is_lower_hex(value, 64) {
        Ok(())
    } else {
        Err(StorageError::new(
            "invalidVersionId",
            "已保存版本标识格式无效。",
        ))
    }
}

fn validate_note_id(value: &str) -> Result<(), StorageError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|_| StorageError::new("invalidNoteId", "笔记身份格式无效。"))?;
    if parsed.get_version_num() != 4 || parsed.to_string() != value {
        return Err(StorageError::new("invalidNoteId", "笔记身份格式无效。"));
    }
    Ok(())
}

fn validate_revision(value: &str, field: &str) -> Result<(), StorageError> {
    if is_lower_hex(value, 64) {
        Ok(())
    } else {
        Err(StorageError::new(
            "invalidRevision",
            format!("{field} 格式无效。"),
        ))
    }
}

fn validate_display_text(value: &str, field: &str) -> Result<(), StorageError> {
    if value.len() <= VERSION_TEXT_LIMIT {
        Ok(())
    } else {
        Err(StorageError::new(
            "budget_exceeded",
            format!("版本 {field} 超过显示长度上限。"),
        ))
    }
}

fn display_title(value: &str) -> String {
    let mut characters = value.chars();
    let excerpt: String = characters.by_ref().take(TITLE_DISPLAY_CHARACTERS).collect();
    if characters.next().is_none() {
        excerpt
    } else {
        let mut truncated: String = value
            .chars()
            .take(TITLE_DISPLAY_CHARACTERS.saturating_sub(1))
            .collect();
        truncated.push('…');
        truncated
    }
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn version_directory(versions: &Path, version_id: &str) -> PathBuf {
    versions.join(version_id)
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() < WARNING_LIMIT - 1 {
        warnings.push(warning);
    } else if warnings.len() == WARNING_LIMIT - 1 {
        warnings.push("其余版本目录问题未展开；相关数据仍已保留。".to_owned());
    }
}

fn ensure_scan_limit(scanned: usize) -> Result<(), StorageError> {
    if scanned > VERSION_SCAN_LIMIT {
        Err(StorageError::new(
            "versionLimit",
            format!("版本目录超过单次扫描上限 {VERSION_SCAN_LIMIT}；没有静默省略其余条目。"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lease_observed_version, list_versions, open_version, pin_observed_version,
        preserve_conflict_version, preserve_observed_version, resolve_version,
    };
    use crate::note_format::{write_package, NotePackage, PlainText};
    use crate::storage::StorageError;
    use std::fs::{self, OpenOptions};
    use std::io;
    use std::path::Path;
    use tempfile::TempDir;

    thread_local! {
        static SYNC_FAILURE: std::cell::RefCell<Option<SyncFault>> = const { std::cell::RefCell::new(None) };
    }

    struct SyncFault {
        stage: &'static str,
        path: Option<std::path::PathBuf>,
        hits: usize,
    }

    pub(super) fn sync_checkpoint(stage: &str, path: &Path) -> io::Result<()> {
        SYNC_FAILURE.with(|state| {
            let mut state = state.borrow_mut();
            if let Some(fault) = state.as_mut() {
                if fault.stage == stage
                    && fault
                        .path
                        .as_deref()
                        .is_none_or(|expected| expected == path)
                {
                    fault.hits += 1;
                    return Err(io::Error::other("injected version sync failure"));
                }
            }
            Ok(())
        })
    }

    struct SyncFailure;
    impl Drop for SyncFailure {
        fn drop(&mut self) {
            SYNC_FAILURE.with(|state| *state.borrow_mut() = None);
        }
    }

    fn fail_sync(stage: &'static str) -> SyncFailure {
        fail_sync_at(stage, None)
    }

    fn fail_sync_at(stage: &'static str, path: Option<std::path::PathBuf>) -> SyncFailure {
        SYNC_FAILURE.with(|state| {
            assert!(state.borrow().is_none());
            *state.borrow_mut() = Some(SyncFault {
                stage,
                path,
                hits: 0,
            });
        });
        SyncFailure
    }

    fn assert_sync_hits(expected: usize) {
        SYNC_FAILURE.with(|state| assert_eq!(state.borrow().as_ref().unwrap().hits, expected));
    }

    #[test]
    fn failed_member_sync_cannot_be_reported_successful_on_retry() {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        write_package_file(&source, &plain("Preserve me", "original content")).unwrap();
        let opened = crate::storage::open_note(&source).unwrap();
        let fault = fail_sync("member-file");
        for _ in 0..2 {
            assert!(
                preserve_observed_version(&root, &opened, "external note", "observed").is_err()
            );
        }
        assert_sync_hits(2);
        drop(fault);
        preserve_observed_version(&root, &opened, "external note", "observed").unwrap();
        let versions = list_versions(&root).unwrap().versions;
        assert_eq!(versions.len(), 1);
        let preserved = open_version(&root, &versions[0].version_id).unwrap();
        assert_eq!(preserved.opened.package.document.title, "Preserve me");
        assert_eq!(
            preserved.opened.package.text_projection().content,
            "original content"
        );
    }

    #[test]
    fn failed_version_barriers_report_failure_and_retain_written_data() {
        for stage in ["entry", "candidate-created", "candidate-ready", "published"] {
            let directory = version_temp();
            let source = directory.path().join("origin.cuenote");
            let root = directory.path().join("preserved");
            write_package_file(&source, &plain("Preserve me", "unchanged source")).unwrap();
            let source_bytes = fs::read(&source).unwrap();
            let opened = crate::storage::open_note(&source).unwrap();
            let fault = fail_sync(stage);
            assert!(
                preserve_observed_version(&root, &opened, "external note", "observed").is_err(),
                "{stage}"
            );
            assert_sync_hits(1);
            drop(fault);
            let entries: Vec<_> = fs::read_dir(root.join("versions"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            match stage {
                "entry" => assert!(entries.is_empty()),
                "candidate-created" => {
                    assert_eq!(entries.len(), 1);
                    assert!(entries[0]
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with(".pending-"));
                }
                "candidate-ready" | "published" => {
                    assert_eq!(entries.len(), 1);
                    let retained =
                        crate::storage::open_note(&entries[0].join("note.cuenote")).unwrap();
                    assert_eq!(retained.package.document.title, "Preserve me");
                }
                _ => unreachable!(),
            }
            let retained_bytes: Vec<_> = entries
                .iter()
                .filter_map(|path| {
                    let file = path.join("note.cuenote");
                    fs::read(&file).ok().map(|bytes| (file, bytes))
                })
                .collect();
            preserve_observed_version(&root, &opened, "external note", "observed").unwrap();
            assert_eq!(list_versions(&root).unwrap().versions.len(), 1);
            assert_eq!(fs::read(&source).unwrap(), source_bytes);
            for (file, bytes) in retained_bytes {
                assert_eq!(fs::read(file).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn retries_sync_every_member_directory_without_rewriting_the_preserved_package() {
        for level in ["group", "groups", "versions"] {
            let directory = version_temp();
            let source = directory.path().join("origin.cuenote");
            let root = directory.path().join("preserved");
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            write_package_file(&source, &plain("Durable retry", "unchanged source")).unwrap();
            let opened = crate::storage::open_note(&source).unwrap();
            let versions = root.join("versions");
            let origin_key = super::normalize_origin("external note").unwrap().1;
            let group_id =
                super::make_group_id(&opened.package.document.note_id, &origin_key).unwrap();
            let target = match level {
                "group" => versions.join("groups").join(group_id),
                "groups" => versions.join("groups"),
                _ => versions.clone(),
            };
            let fault = fail_sync_at("member-directory", Some(target));
            assert!(
                preserve_observed_version(&root, &opened, "external note", "observed").is_err()
            );
            let id = list_versions(&root).unwrap().versions[0].version_id.clone();
            let package = versions.join(&id).join("note.cuenote");
            let before = fs::read(&package).unwrap();
            assert!(
                preserve_observed_version(&root, &opened, "external note", "observed").is_err()
            );
            assert_sync_hits(2);
            drop(fault);
            preserve_observed_version(&root, &opened, "external note", "observed").unwrap();
            assert_eq!(fs::read(package).unwrap(), before);
            assert_eq!(list_versions(&root).unwrap().versions.len(), 1);
            assert_eq!(
                open_version(&root, &id)
                    .unwrap()
                    .opened
                    .package
                    .document
                    .title,
                "Durable retry"
            );
        }
    }

    #[test]
    fn retry_after_group_creation_failure_still_syncs_existing_directories() {
        for parent in ["versions", "groups"] {
            let directory = version_temp();
            let source = directory.path().join("origin.cuenote");
            let root = directory.path().join("preserved");
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            write_package_file(&source, &plain("Group retry", "retained content")).unwrap();
            let opened = crate::storage::open_note(&source).unwrap();
            let target = if parent == "versions" {
                root.join("versions")
            } else {
                root.join("versions/groups")
            };
            let fault = fail_sync_at("directory-created", Some(target.clone()));
            assert!(
                preserve_observed_version(&root, &opened, "external note", "observed").is_err()
            );
            assert_sync_hits(1);
            drop(fault);
            let fault = fail_sync_at("member-directory", Some(target));
            assert!(
                preserve_observed_version(&root, &opened, "external note", "observed").is_err()
            );
            assert_sync_hits(1);
            drop(fault);
            preserve_observed_version(&root, &opened, "external note", "observed").unwrap();
            let versions = list_versions(&root).unwrap().versions;
            assert_eq!(versions.len(), 1);
            assert_eq!(
                open_version(&root, &versions[0].version_id)
                    .unwrap()
                    .opened
                    .package
                    .text_projection()
                    .content,
                "retained content"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn retained_group_member_blocks_external_write_and_rename() {
        let directory = version_temp();
        let id = "a".repeat(64);
        let member = directory.path().join(&id);
        let moved = directory.path().join("moved");
        fs::write(&member, []).unwrap();
        for writable in [false, true] {
            let held = super::open_group_member(directory.path(), &id, writable).unwrap();
            assert!(OpenOptions::new().write(true).open(&member).is_err());
            assert!(fs::rename(&member, &moved).is_err());
            drop(held);
            drop(OpenOptions::new().write(true).open(&member).unwrap());
            fs::rename(&member, &moved).unwrap();
            fs::rename(&moved, &member).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn durability_context_rejects_replaced_anchor_and_unsafe_descendants() {
        use std::os::unix::fs::symlink;
        let directory = version_temp();
        let versions = directory.path().join("versions");
        let outside = directory.path().join("outside");
        fs::create_dir(&versions).unwrap();
        fs::create_dir(&outside).unwrap();
        let context = super::VersionDurability::new(&versions).unwrap();
        assert!(context.sync(&outside, "test").is_err());
        assert!(context.sync(&versions.join("../outside"), "test").is_err());
        symlink(&outside, versions.join("linked")).unwrap();
        assert!(context.sync(&versions.join("linked"), "test").is_err());
        fs::rename(&versions, directory.path().join("old-versions")).unwrap();
        fs::create_dir(&versions).unwrap();
        assert!(context.sync(&versions, "test").is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    fn version_temp() -> TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
        fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("preserved-versions-")
            .tempdir_in(base)
            .unwrap()
    }

    fn plain(title: &str, content: &str) -> NotePackage {
        NotePackage::new_plain(
            "00000000-0000-4000-8000-000000000001".to_owned(),
            PlainText {
                title: title.to_owned(),
                cue: String::new(),
                content: content.to_owned(),
                summary: String::new(),
            },
        )
    }

    fn write_package_file(path: &Path, package: &NotePackage) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        write_package(&mut file, package)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        file.sync_all()
    }

    #[test]
    fn preserves_the_observed_package_after_its_origin_is_overwritten() -> Result<(), StorageError>
    {
        let directory = version_temp();
        let origin_directory = directory.path().join("origin");
        fs::create_dir(&origin_directory).unwrap();
        let source = origin_directory.join("note.cuenote");
        let root = directory.path().join("preserved");
        let observed = plain("Observed title", "content from the opened file");
        write_package_file(&source, &observed).unwrap();
        let mut source_bytes = fs::read(&source).unwrap();
        let eocd = source_bytes.len() - 22;
        assert_eq!(&source_bytes[eocd..eocd + 4], b"PK\x05\x06");
        source_bytes[eocd + 20] = 1;
        source_bytes.push(b'x');
        fs::write(&source, source_bytes).unwrap();
        let opened = crate::storage::open_note(&source)?;
        let source_revision = opened.revision.clone();
        let note_id = opened.package.document.note_id.clone();

        let mut replacement = opened.package.clone();
        replacement.document.title = "Later title".to_owned();
        replacement.document.sections.content =
            plain("", "later content").document.sections.content;
        write_package_file(&source, &replacement).unwrap();

        preserve_observed_version(
            &root,
            &opened,
            source.to_string_lossy().as_ref(),
            "external-change",
        )?;

        let list = list_versions(&root)?;
        assert_eq!(list.versions.len(), 1);
        let summary = &list.versions[0];
        assert_eq!(summary.note_id, note_id);
        assert_eq!(summary.source_revision, source_revision);
        assert_ne!(summary.revision, summary.source_revision);
        assert_eq!(summary.title, "Observed title");
        assert!(!summary.conflict);
        let preserved = open_version(&root, &summary.version_id)?;
        assert_eq!(preserved.opened.revision, summary.revision);
        assert_eq!(preserved.opened.package.document.title, "Observed title");
        assert_eq!(
            preserved.opened.package.text_projection().content,
            "content from the opened file"
        );
        Ok(())
    }

    #[test]
    fn preserves_native_v1_titles_longer_than_the_summary_excerpt() -> Result<(), StorageError> {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let full_title = "标题🙂".repeat(8_000);
        let package = plain(&full_title, "large-title package remains valid");
        write_package_file(&source, &package).unwrap();
        let opened = crate::storage::open_note(&source)?;

        preserve_observed_version(&root, &opened, "external note", "observed")?;

        let list = list_versions(&root)?;
        assert_eq!(list.versions.len(), 1);
        assert!(list.versions[0].title.chars().count() <= 180);
        assert!(list.versions[0].title.ends_with('…'));
        let preserved = open_version(&root, &list.versions[0].version_id)?;
        assert_eq!(preserved.opened.package.document.title, full_title);
        Ok(())
    }

    #[test]
    fn witness_readers_wait_for_version_store_maintenance() -> Result<(), StorageError> {
        use std::sync::mpsc::{self, RecvTimeoutError};
        use std::time::Duration;

        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let origin = "external note";
        write_package_file(&source, &plain("Protected receipt", "original content")).unwrap();
        let opened = crate::storage::open_note(&source)?;
        preserve_observed_version(&root, &opened, origin, "observed")?;
        let witness = super::observed_version_witness(&root, &opened, origin)?;
        let metadata = root
            .join(super::VERSIONS_DIRECTORY)
            .join(&witness.version_id)
            .join(super::METADATA_NAME);
        let original = fs::read(&metadata).unwrap();

        for verify_cached in [false, true] {
            let store_lock = super::acquire_store_lock(&root, false)?;
            // An exclusive writer may temporarily leave unreadable state.
            // Neither receipt reader may observe it before the writer finishes.
            fs::write(&metadata, b"in-flight maintenance").unwrap();
            let (started_tx, started_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let reader_root = root.clone();
            let reader_source = source.clone();
            let reader_witness = witness.clone();
            let reader = std::thread::spawn(move || {
                let opened = crate::storage::open_note(&reader_source).unwrap();
                started_tx.send(()).unwrap();
                let result = if verify_cached {
                    super::verify_observed_version_witness(
                        &reader_root,
                        &reader_witness,
                        &opened.revision,
                        origin,
                        &opened.package.document.note_id,
                        &opened.package.document.title,
                    )
                } else {
                    super::observed_version_witness(&reader_root, &opened, origin).map(|_| true)
                };
                result_tx.send(result).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let premature = result_rx.recv_timeout(Duration::from_millis(200));
            fs::write(&metadata, &original).unwrap();
            drop(store_lock);
            assert!(matches!(premature, Err(RecvTimeoutError::Timeout)));
            assert!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap()?);
            reader.join().unwrap();
        }
        assert_eq!(fs::read(metadata).unwrap(), original);
        Ok(())
    }

    #[test]
    fn ordinary_rotation_keeps_thirty_and_never_rotates_pending_conflicts(
    ) -> Result<(), StorageError> {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let origin = "external note";
        let mut package = plain("Version 0", "content 0");
        let note_id = package.document.note_id.clone();
        write_package_file(&source, &package).unwrap();
        let first = crate::storage::open_note(&source)?;
        preserve_observed_version(&root, &first, origin, "observed")?;
        preserve_conflict_version(&root, &first, origin, "duplicate-id")?;

        for index in 1..=32 {
            package = plain(&format!("Version {index}"), &format!("content {index}"));
            package.document.note_id = note_id.clone();
            write_package_file(&source, &package).unwrap();
            let opened = crate::storage::open_note(&source)?;
            preserve_observed_version(&root, &opened, origin, "observed")?;
        }

        let list = list_versions(&root)?;
        assert_eq!(
            list.versions
                .iter()
                .filter(|version| !version.conflict)
                .count(),
            30
        );
        let conflict = list
            .versions
            .iter()
            .find(|version| version.conflict)
            .unwrap();
        let opened_conflict = open_version(&root, &conflict.version_id)?;
        assert_eq!(opened_conflict.opened.package.document.title, "Version 0");
        Ok(())
    }

    #[test]
    fn an_active_lease_keeps_an_older_observation_during_rotation() -> Result<(), StorageError> {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let origin = "external note";
        let mut package = plain("Leased baseline", "baseline content");
        let note_id = package.document.note_id.clone();
        write_package_file(&source, &package).unwrap();
        let first = crate::storage::open_note(&source)?;
        let lease = lease_observed_version(&root, &first, origin, "editor-baseline")?;

        for index in 1..=31 {
            package = plain(&format!("Version {index}"), &format!("content {index}"));
            package.document.note_id = note_id.clone();
            write_package_file(&source, &package).unwrap();
            let opened = crate::storage::open_note(&source)?;
            preserve_observed_version(&root, &opened, origin, "observed")?;
        }

        let list = list_versions(&root)?;
        assert_eq!(
            list.versions
                .iter()
                .filter(|version| !version.conflict)
                .count(),
            31
        );
        assert!(list
            .versions
            .iter()
            .any(|version| version.version_id == lease.version_id));
        let baseline = open_version(&root, &lease.version_id)?;
        assert_eq!(baseline.opened.package.document.title, "Leased baseline");
        drop(lease);
        Ok(())
    }

    #[test]
    fn resolution_hides_a_version_without_deleting_its_package() -> Result<(), StorageError> {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let package = plain("Keep the bytes", "resolved content");
        write_package_file(&source, &package).unwrap();
        let opened = crate::storage::open_note(&source)?;
        preserve_conflict_version(&root, &opened, "external note", "duplicate-id")?;
        let version_id = list_versions(&root)?.versions[0].version_id.clone();

        resolve_version(&root, &version_id)?;
        assert!(list_versions(&root)?.versions.is_empty());
        let preserved = open_version(&root, &version_id)?;
        assert_eq!(preserved.opened.package.document.title, "Keep the bytes");
        assert!(preserved.path.is_file());
        Ok(())
    }

    #[test]
    fn pinning_a_conflict_uses_the_stored_observation_after_origin_changes(
    ) -> Result<(), StorageError> {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let origin = "external note";
        let original = plain("Previously observed", "original content");
        write_package_file(&source, &original).unwrap();
        let observed = crate::storage::open_note(&source)?;
        let note_id = observed.package.document.note_id.clone();
        let source_revision = observed.revision.clone();
        preserve_observed_version(&root, &observed, origin, "observed")?;

        let mut replacement = original.clone();
        replacement.document.title = "Changed at origin".to_owned();
        write_package_file(&source, &replacement).unwrap();
        pin_observed_version(&root, &source_revision, origin, &note_id, "duplicate-id")?;

        let list = list_versions(&root)?;
        assert_eq!(list.versions.len(), 2);
        let conflict = list
            .versions
            .iter()
            .find(|version| version.conflict)
            .unwrap();
        let pinned = open_version(&root, &conflict.version_id)?;
        assert_eq!(pinned.opened.package.document.title, "Previously observed");
        assert_eq!(
            pinned.opened.package.text_projection().content,
            "original content"
        );
        Ok(())
    }

    #[test]
    fn ids_cannot_be_used_as_paths_and_symlink_roots_are_rejected() -> Result<(), StorageError> {
        let directory = version_temp();
        let source = directory.path().join("origin.cuenote");
        let root = directory.path().join("preserved");
        let package = plain("Opaque id", "content");
        write_package_file(&source, &package).unwrap();
        let opened = crate::storage::open_note(&source)?;
        preserve_observed_version(&root, &opened, "external note", "observed")?;
        let version_id = list_versions(&root)?.versions[0].version_id.clone();

        let invalid = open_version(&root, "../outside").unwrap_err();
        assert_eq!(invalid.kind, "invalidVersionId");
        let mut tampered_id = version_id.clone();
        tampered_id.replace_range(0..1, if &tampered_id[..1] == "0" { "1" } else { "0" });
        assert_eq!(
            open_version(&root, &tampered_id).unwrap_err().kind,
            "versionNotFound"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let empty_target = directory.path().join("empty-target");
            fs::create_dir(&empty_target).unwrap();
            let linked_root = directory.path().join("linked-preserved");
            symlink(&empty_target, &linked_root).unwrap();
            assert_eq!(list_versions(&linked_root).unwrap_err().kind, "unsafePath");
            assert!(!empty_target.join(".locks").exists());
        }
        Ok(())
    }
}
