use crate::note_format::{
    read_package, write_package, FormatError, NotePackage, MAX_PACKAGE_BYTES,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Cursor;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

mod recovery;

pub(crate) use recovery::{
    create_recovery_snapshot, list_recovery_drafts, open_recovery_draft, remove_recovery_snapshot,
    save_copy, RecoverySnapshot,
};

const HISTORY_LIMIT: usize = 30;
const MAX_METADATA_REPLACEMENT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageError {
    pub kind: String,
    pub message: String,
    pub recovery_path: Option<String>,
    pub recovery_error: Option<String>,
}

impl StorageError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            recovery_path: None,
            recovery_error: None,
        }
    }

    fn io(context: &str, error: impl std::fmt::Display) -> Self {
        Self::new("io", format!("{context}: {error}"))
    }

    fn format(error: FormatError) -> Self {
        let guidance = match error.kind.as_str() {
            "unsupported_version" => {
                "此笔记使用当前版本不支持的格式。请升级 CueNote 后再打开；当前禁止覆盖。"
            }
            "budget_exceeded" | "resource_limit" => "笔记内容或资源超过安全上限，无法打开或覆盖。",
            "invalid_manifest" | "asset_integrity" | "invalid_asset" => {
                "笔记的图片附件缺失、损坏或与清单不一致，无法打开或覆盖。"
            }
            "invalid_archive" | "invalid_document" => {
                "笔记文件损坏或结构不受支持，无法打开或覆盖。"
            }
            _ => "笔记格式无效，无法打开或覆盖。",
        };
        Self::new(
            error.kind,
            format!("{guidance} 技术诊断：{}", error.message),
        )
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StorageError {}

#[derive(Debug)]
pub struct OpenedNote {
    pub package: NotePackage,
    pub revision: String,
}

pub fn open_note(path: &Path) -> Result<OpenedNote, StorageError> {
    let target = resolve_target(path)?;
    inspect_note(&target)?.ok_or_else(|| {
        StorageError::new("io", format!("笔记文件不存在：{}", target.path.display()))
    })
}

/// Atomically replaces a small library metadata file after confirming its current bytes.
/// The old bytes remain at the destination on a failed publish or in the reported
/// same-directory backup when the platform has already completed the replacement.
pub(crate) fn replace_metadata_file(
    target: &Path,
    expected_bytes: &[u8],
    replacement_bytes: &[u8],
) -> Result<(), StorageError> {
    if expected_bytes.len() as u64 > MAX_METADATA_REPLACEMENT_BYTES
        || replacement_bytes.len() as u64 > MAX_METADATA_REPLACEMENT_BYTES
    {
        return Err(StorageError::new(
            "libraryMetadata",
            "笔记本元数据超过大小上限。",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "元数据文件缺少父目录。"))?;
    let current = read_metadata_file(target)?;
    if current != expected_bytes {
        return Err(StorageError::new(
            "conflict",
            "笔记本元数据已发生变化；原文件已保留。",
        ));
    }

    let candidate = parent.join(format!(
        ".workspace-metadata-candidate-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)
        .map_err(|error| StorageError::io("无法创建笔记本元数据候选", error))?;
    let write_result = file
        .write_all(replacement_bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&candidate);
        return Err(StorageError::io("无法同步笔记本元数据候选", error));
    }

    let latest = match read_metadata_file(target) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
    };
    if latest != expected_bytes {
        let _ = fs::remove_file(&candidate);
        return Err(StorageError::new(
            "conflict",
            "笔记本元数据在发布前发生变化；原文件已保留。",
        ));
    }

    let result = replace_metadata_candidate(
        target,
        &candidate,
        parent,
        expected_bytes,
        replacement_bytes,
    );
    if result.is_err() {
        let _ = fs::remove_file(&candidate);
    }
    result
}

pub fn save_note(
    path: &Path,
    recovery_root: &Path,
    package: &NotePackage,
    expected_revision: Option<&str>,
) -> Result<String, StorageError> {
    let target = match resolve_target(path) {
        Ok(target) => target,
        Err(error) => {
            return Err(preserve_snapshot_for_unresolved_target(
                error,
                path,
                recovery_root,
                package,
            ));
        }
    };
    let note_id = package.document.note_id.clone();
    let recovery = match prepare_recovery_root(recovery_root, &target.parent) {
        Ok(root) => root,
        Err(mut error) => {
            error.recovery_error = Some(format!(
                "无法在修改笔记前准备独立恢复目录，因此没有写入候选或发布笔记：{}",
                error.message
            ));
            return Err(error);
        }
    };

    let snapshot = match create_recovery_snapshot(&recovery, package) {
        Ok(snapshot) => snapshot,
        Err(mut error) => {
            if error.recovery_path.is_none() {
                error.recovery_error = Some(format!(
                    "保存前恢复快照未能完成，原笔记没有进入发布阶段：{}",
                    error.message
                ));
            }
            return Err(error);
        }
    };
    if let Err(error) =
        crate::storage_faults::checkpoint("recovery_snapshot_persisted", &target.path, None)
    {
        return Err(fail_with_snapshot(error, &snapshot, None, false));
    }

    let _lock = match acquire_save_lock(&recovery, &target.path) {
        Ok(lock) => lock,
        Err(error) => {
            return Err(fail_with_snapshot(
                StorageError::io("无法协调此笔记的保存", error),
                &snapshot,
                None,
                false,
            ));
        }
    };

    let candidate = match create_candidate_path(&target.parent) {
        Ok((candidate, file)) => (candidate, file),
        Err(error) => {
            return Err(fail_with_snapshot(error, &snapshot, None, false));
        }
    };
    let (candidate_path, candidate_file) = candidate;
    if let Err(error) =
        crate::storage_faults::checkpoint("candidate_created", &target.path, Some(&candidate_path))
    {
        return Err(fail_with_snapshot(
            error,
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }
    let candidate_revision =
        match write_and_verify_candidate(candidate_file, &candidate_path, &target.path, package) {
            Ok(revision) => revision,
            Err(error) => {
                return Err(fail_with_snapshot(
                    error,
                    &snapshot,
                    Some(&candidate_path),
                    false,
                ));
            }
        };

    let baseline = match inspect_note(&target) {
        Ok(baseline) => baseline,
        Err(error) => {
            return Err(fail_with_snapshot(
                error,
                &snapshot,
                Some(&candidate_path),
                false,
            ));
        }
    };
    let baseline_revision = baseline.as_ref().map(|note| note.revision.as_str());
    let expected_matches = match expected_revision {
        Some(expected) => baseline_revision == Some(expected),
        None => baseline.is_none(),
    };
    if !expected_matches {
        return Err(fail_with_snapshot(
            StorageError::new(
                "conflict",
                "笔记已被其他操作创建、修改或删除，请保留当前版本并处理恢复草稿。",
            ),
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }
    if let Some(baseline) = &baseline {
        if baseline.package.document.note_id != note_id {
            return Err(fail_with_snapshot(
                StorageError::new("conflict", "笔记身份与保存路径中的现有笔记身份不一致。"),
                &snapshot,
                Some(&candidate_path),
                false,
            ));
        }
    }

    let history_backup = if let Some(old_revision) = baseline_revision {
        match preserve_history_version(&target.path, &recovery, &note_id, old_revision) {
            Ok(path) => Some(path),
            Err(error) => {
                return Err(fail_with_snapshot(
                    error,
                    &snapshot,
                    Some(&candidate_path),
                    false,
                ));
            }
        }
    } else {
        None
    };
    if let Err(error) =
        crate::storage_faults::checkpoint("history_persisted", &target.path, Some(&candidate_path))
    {
        return Err(fail_with_snapshot(
            error,
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }

    let current_again = match inspect_note(&target) {
        Ok(current) => current,
        Err(error) => {
            return Err(fail_with_snapshot(
                error,
                &snapshot,
                Some(&candidate_path),
                false,
            ));
        }
    };
    if current_again.as_ref().map(|note| note.revision.as_str()) != baseline_revision {
        return Err(fail_with_snapshot(
            StorageError::new(
                "conflict",
                "笔记在保存期间发生变化；原版本和恢复草稿均已保留。",
            ),
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }

    if let Err(error) =
        crate::storage_faults::checkpoint("publish_before", &target.path, Some(&candidate_path))
    {
        return Err(fail_with_snapshot(
            error,
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }

    let publish_result = if baseline.is_some() {
        publish_replace(
            &candidate_path,
            &target.path,
            baseline_revision.unwrap(),
            history_backup.as_deref(),
        )
    } else {
        publish_create(&candidate_path, &target.path)
    };
    if let Err(error) = publish_result {
        #[cfg(windows)]
        if baseline.is_some() {
            return Err(fail_with_snapshot(
                error,
                &snapshot,
                Some(&candidate_path),
                true,
            ));
        }
        return Err(fail_with_snapshot(
            error,
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }

    if let Err(error) =
        crate::storage_faults::checkpoint("publish_after", &target.path, Some(&candidate_path))
    {
        let mut error = error;
        error.message = format!("发布已经完成，后续步骤发生故障：{}", error.message);
        return Err(fail_with_snapshot(
            error,
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }

    if let Err(error) = sync_parent_directory(&target.parent) {
        return Err(fail_with_snapshot(
            StorageError::io("笔记已发布，但无法同步所在目录", error),
            &snapshot,
            Some(&candidate_path),
            false,
        ));
    }

    let published = match inspect_note(&target) {
        Ok(Some(published)) if published.revision == candidate_revision => published,
        Ok(_) => {
            return Err(fail_with_snapshot(
                StorageError::new(
                    "publish",
                    "发布后的笔记校验与候选版本不一致；已保留恢复草稿。",
                ),
                &snapshot,
                Some(&candidate_path),
                false,
            ));
        }
        Err(error) => {
            return Err(fail_with_snapshot(
                StorageError::new(
                    "publish",
                    format!("发布后无法重新读取笔记：{}", error.message),
                ),
                &snapshot,
                Some(&candidate_path),
                false,
            ));
        }
    };
    let _ = published;

    rotate_history(&history_directory(&recovery, &note_id));
    if let Err(error) = remove_owned_save_candidate(&candidate_path) {
        let mut storage_error = StorageError::io("笔记已保存，但无法清理同目录候选文件", error);
        storage_error.recovery_error = Some(format!(
            "已验证恢复快照仍保留于 {}；候选清理路径为 {}",
            snapshot.path.display(),
            candidate_path.display()
        ));
        storage_error.recovery_path = Some(snapshot.path.to_string_lossy().into_owned());
        return Err(storage_error);
    }
    if let Err(mut error) = remove_recovery_snapshot(&recovery, &snapshot) {
        error.message = format!("笔记已保存，但无法确认清理本次恢复快照：{}", error.message);
        if fs::symlink_metadata(&snapshot.path).is_ok() {
            error.recovery_path = Some(snapshot.path.to_string_lossy().into_owned());
        } else {
            error.recovery_path = None;
        }
        return Err(error);
    }
    Ok(candidate_revision)
}

fn preserve_snapshot_for_unresolved_target(
    mut error: StorageError,
    requested_path: &Path,
    recovery_root: &Path,
    package: &NotePackage,
) -> StorageError {
    let requested_parent = requested_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let note_parent = match resolve_recovery_path(requested_parent) {
        Ok((parent, _)) => parent,
        Err(recovery_error) => {
            error.recovery_error = Some(format!(
                "无法验证目标目录与恢复目录的分离关系；未写入恢复快照：{}",
                recovery_error.message
            ));
            return error;
        }
    };
    let recovery = match prepare_recovery_root(recovery_root, &note_parent) {
        Ok(recovery) => recovery,
        Err(recovery_error) => {
            error.recovery_error = Some(format!(
                "目标笔记不可访问，且无法准备独立恢复目录：{}",
                recovery_error.message
            ));
            return error;
        }
    };
    let snapshot = match create_recovery_snapshot(&recovery, package) {
        Ok(snapshot) => snapshot,
        Err(snapshot_error) => {
            error.recovery_path = snapshot_error.recovery_path.clone();
            error.recovery_error = Some(format!(
                "目标笔记不可访问，保存前恢复快照未能完成：{}",
                snapshot_error.message
            ));
            return error;
        }
    };
    if let Err(checkpoint_error) =
        crate::storage_faults::checkpoint("recovery_snapshot_persisted", requested_path, None)
    {
        error.recovery_error = Some(format!(
            "目标笔记不可访问；恢复快照已保留于 {}，随后发生故障：{}",
            snapshot.path.display(),
            checkpoint_error.message
        ));
    }
    error.recovery_path = Some(snapshot.path.to_string_lossy().into_owned());
    error
}

pub fn probe_directory(path: &Path) -> Result<(), StorageError> {
    let directory = canonical_directory(path)?;
    let token = Uuid::new_v4().simple().to_string();
    let create_source_path = directory.join(format!(".cuenote-probe-{token}-create-source"));
    let original_path = directory.join(format!(".cuenote-probe-{token}-old"));
    let candidate_path = directory.join(format!(".cuenote-probe-{token}-new"));
    let backup_path = directory.join(format!(".cuenote-probe-{token}-backup"));
    let mut cleanup = ProbeCleanup::default();
    let result = (|| {
        const CREATED_BYTES: &[u8] = b"cuenote probe creation\n";
        const REPLACED_BYTES: &[u8] = b"cuenote probe replacement\n";

        cleanup.create_synced(&create_source_path, CREATED_BYTES, "无法创建目录创建探针")?;
        if let Err(error) = publish_create(&create_source_path, &original_path) {
            cleanup.preserve_if_unowned(&original_path);
            return Err(StorageError::new(
                "publish",
                format!(
                    "所选目录不支持安全无覆盖原子创建：{}；失败目标归属未确认，若仍存在则保留：{}。",
                    error.message,
                    original_path.display()
                ),
            ));
        }
        cleanup.record_published(&original_path, CREATED_BYTES);
        let created = read_probe_file(&original_path, CREATED_BYTES.len())
            .map_err(|error| StorageError::io("无法读取目录创建探针", error))?;
        if created != CREATED_BYTES {
            return Err(StorageError::new("publish", "目录原子创建探针内容不匹配。"));
        }
        cleanup.create_synced(&candidate_path, REPLACED_BYTES, "无法创建目录替换探针")?;

        #[cfg(windows)]
        match fs::symlink_metadata(&backup_path) {
            Ok(_) => {
                cleanup.preserve_if_unowned(&backup_path);
                return Err(StorageError::new(
                    "conflict",
                    format!(
                        "目录探针备份路径已被占用，未执行替换：{}",
                        backup_path.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                cleanup.preserve_if_unowned(&backup_path);
                return Err(StorageError::io(
                    &format!("无法确认目录探针备份路径为空 {}", backup_path.display()),
                    error,
                ));
            }
        }

        if let Err(error) = replace_path(&candidate_path, &original_path, &backup_path) {
            #[cfg(windows)]
            cleanup.preserve_if_unowned(&backup_path);
            return Err(StorageError::io(
                &format!(
                    "目录不支持安全原子替换；若替换留下未确认文件，将保留其路径 {} / {}",
                    original_path.display(),
                    backup_path.display()
                ),
                error,
            ));
        }
        cleanup.forget(&candidate_path);
        cleanup.update_expected(&original_path, REPLACED_BYTES);

        #[cfg(windows)]
        {
            if let Err(error) = verify_probe_file(&backup_path, CREATED_BYTES) {
                cleanup.preserve_if_unowned(&backup_path);
                return Err(StorageError::new(
                    "publish",
                    format!(
                        "目录替换已返回成功，但无法确认备份文件归属或内容；已保留路径 {}：{}",
                        backup_path.display(),
                        error.message
                    ),
                ));
            }
            cleanup.record_published(&backup_path, CREATED_BYTES);
        }

        let published = read_probe_file(&original_path, REPLACED_BYTES.len())
            .map_err(|error| StorageError::io("无法读取目录探针文件", error))?;
        if published != REPLACED_BYTES {
            return Err(StorageError::new("publish", "目录探针替换后的内容不匹配。"));
        }
        sync_parent_directory(&directory)
            .map_err(|error| StorageError::io("无法同步目录探针", error))?;
        Ok(())
    })();

    let cleanup_result = cleanup.cleanup(&directory);
    match (result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup_error)) => Err(StorageError::new(
            "io",
            format!("目录探针完成，但清理未确认归属的探针文件时失败：{cleanup_error}"),
        )),
        (Err(error), Err(cleanup_error)) => Err(StorageError::new(
            error.kind,
            format!("{} 清理探针文件时失败：{cleanup_error}", error.message),
        )),
    }
}

#[derive(Default)]
struct ProbeCleanup {
    owned: Vec<OwnedProbeFile>,
    uncertain_paths: Vec<PathBuf>,
}

struct OwnedProbeFile {
    path: PathBuf,
    expected: Option<Vec<u8>>,
}

impl ProbeCleanup {
    fn create_synced(
        &mut self,
        path: &Path,
        content: &[u8],
        context: &str,
    ) -> Result<(), StorageError> {
        let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(error) => {
                self.preserve_if_unowned(path);
                return Err(StorageError::io(
                    &format!("{context} {}", path.display()),
                    error,
                ));
            }
        };
        self.owned.push(OwnedProbeFile {
            path: path.to_path_buf(),
            expected: None,
        });
        if let Err(error) = file
            .write_all(content)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
        {
            return Err(StorageError::io(
                &format!("{context}失败，未完成文件保留于 {}", path.display()),
                error,
            ));
        }
        if let Some(owned) = self.owned.last_mut() {
            owned.expected = Some(content.to_vec());
        }
        Ok(())
    }

    fn record_published(&mut self, path: &Path, expected: &[u8]) {
        self.owned.push(OwnedProbeFile {
            path: path.to_path_buf(),
            expected: Some(expected.to_vec()),
        });
    }

    fn update_expected(&mut self, path: &Path, expected: &[u8]) {
        if let Some(owned) = self.owned.iter_mut().find(|owned| owned.path == path) {
            owned.expected = Some(expected.to_vec());
        }
    }

    fn forget(&mut self, path: &Path) {
        self.owned.retain(|owned| owned.path != path);
    }

    fn preserve_if_unowned(&mut self, path: &Path) {
        if !self.owned.iter().any(|owned| owned.path == path)
            && !self.uncertain_paths.iter().any(|known| known == path)
        {
            self.uncertain_paths.push(path.to_path_buf());
        }
    }

    fn cleanup(&mut self, directory: &Path) -> Result<(), String> {
        let mut details: Vec<(PathBuf, String)> = Vec::new();
        let owned_files = std::mem::take(&mut self.owned);
        for owned in owned_files.into_iter().rev() {
            let metadata = match fs::symlink_metadata(&owned.path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    details.push((owned.path.clone(), format!("无法检查，保留：{error}")));
                    continue;
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                details.push((owned.path.clone(), "路径类型已变化，未清理".to_owned()));
                continue;
            }
            let Some(expected) = owned.expected else {
                details.push((
                    owned.path.clone(),
                    "文件写入或同步未完成，保留以供检查".to_owned(),
                ));
                continue;
            };
            match read_probe_file(&owned.path, expected.len()) {
                Ok(actual) if actual == expected => {}
                Ok(_) => {
                    details.push((owned.path.clone(), "文件内容已变化，未清理".to_owned()));
                    continue;
                }
                Err(error) => {
                    details.push((owned.path.clone(), format!("无法验证内容，保留：{error}")));
                    continue;
                }
            }
            if let Err(error) = fs::remove_file(&owned.path) {
                if error.kind() != io::ErrorKind::NotFound {
                    details.push((owned.path.clone(), format!("无法清理，保留：{error}")));
                }
            }
        }

        for path in std::mem::take(&mut self.uncertain_paths) {
            if details.iter().any(|(known, _)| known == &path) {
                continue;
            }
            match fs::symlink_metadata(&path) {
                Ok(_) => details.push((path, "归属未确认，已保留".to_owned())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => details.push((path, format!("无法确认是否存在，保留：{error}"))),
            }
        }

        if let Err(error) = sync_parent_directory(directory) {
            details.push((directory.to_path_buf(), format!("无法同步目录项：{error}")));
        }
        if details.is_empty() {
            Ok(())
        } else {
            Err(details
                .into_iter()
                .map(|(path, detail)| format!("{}（{detail}）", path.display()))
                .collect::<Vec<_>>()
                .join("；"))
        }
    }
}

fn read_probe_file(path: &Path, expected_len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(expected_len as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn verify_probe_file(path: &Path, expected: &[u8]) -> Result<(), StorageError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| StorageError::io("无法检查探针备份", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StorageError::new("publish", "探针备份路径不是普通文件。"));
    }
    let actual = read_probe_file(path, expected.len())
        .map_err(|error| StorageError::io("无法读取探针备份", error))?;
    if actual != expected {
        return Err(StorageError::new("publish", "探针备份内容不匹配。"));
    }
    Ok(())
}

#[cfg(test)]
mod probe_cleanup_tests {
    use super::*;

    #[test]
    fn failed_no_replace_publish_preserves_the_occupied_target() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("probe-source");
        let target = directory.path().join("probe-old");
        let foreign_bytes = b"created by another process";
        fs::write(&target, foreign_bytes).unwrap();

        let mut cleanup = ProbeCleanup::default();
        cleanup
            .create_synced(&source, b"probe payload", "probe source")
            .unwrap();
        let publish_error = publish_create(&source, &target).unwrap_err();
        cleanup.preserve_if_unowned(&target);

        let cleanup_error = cleanup.cleanup(directory.path()).unwrap_err();
        assert!(publish_error.message.contains("创建"));
        assert!(cleanup_error.contains("归属未确认"));
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), foreign_bytes);
    }
}

struct ResolvedTarget {
    path: PathBuf,
    parent: PathBuf,
}

fn resolve_target(path: &Path) -> Result<ResolvedTarget, StorageError> {
    let name = path
        .file_name()
        .ok_or_else(|| StorageError::new("unsafePath", "笔记路径必须包含文件名。"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent_meta = fs::symlink_metadata(parent)
        .map_err(|error| StorageError::io("无法读取笔记所在目录", error))?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "笔记所在位置必须是实际目录，不能通过目录符号链接访问。",
        ));
    }
    let parent = fs::canonicalize(parent)
        .map_err(|error| StorageError::io("无法解析笔记所在目录", error))?;
    Ok(ResolvedTarget {
        path: parent.join(name),
        parent,
    })
}

pub(crate) fn resolve_output_path(path: &Path) -> Result<PathBuf, StorageError> {
    resolve_target(path).map(|target| target.path)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, StorageError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| StorageError::io("无法读取所选目录", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "所选路径必须是实际目录，不能是符号链接或文件。",
        ));
    }
    fs::canonicalize(path).map_err(|error| StorageError::io("无法解析所选目录", error))
}

fn inspect_note(target: &ResolvedTarget) -> Result<Option<OpenedNote>, StorageError> {
    match fs::symlink_metadata(&target.path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StorageError::new(
                    "unsafePath",
                    "笔记路径必须指向普通文件，不能覆盖符号链接或目录。",
                ));
            }
            let mut file = open_read_nofollow(&target.path)
                .map_err(|error| StorageError::io("无法打开笔记文件", error))?;
            if !file
                .metadata()
                .map_err(|error| StorageError::io("无法读取笔记文件信息", error))?
                .is_file()
            {
                return Err(StorageError::new("unsafePath", "笔记路径不是普通文件。"));
            }
            read_validated_file(&mut file).map(Some)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::io("无法检查笔记文件", error)),
    }
}

fn read_validated_file(file: &mut File) -> Result<OpenedNote, StorageError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| StorageError::io("无法定位笔记文件", error))?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(MAX_PACKAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::io("无法读取笔记文件", error))?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(StorageError::new(
            "budget_exceeded",
            "笔记文件超过 256 MiB 安全上限，无法打开或覆盖。",
        ));
    }
    let package = read_package(Cursor::new(bytes.as_slice())).map_err(StorageError::format)?;
    let revision = digest_bytes(&bytes);
    Ok(OpenedNote { package, revision })
}

fn write_and_verify_package(mut file: File, package: &NotePackage) -> Result<String, StorageError> {
    write_package(&mut file, package).map_err(StorageError::format)?;
    file.flush()
        .map_err(|error| StorageError::io("无法写入完整候选笔记", error))?;
    file.sync_all()
        .map_err(|error| StorageError::io("无法同步候选笔记", error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| StorageError::io("无法重新读取候选笔记", error))?;
    let written_id = package.document.note_id.as_str();
    let opened = read_validated_file(&mut file)?;
    if opened.package.document.note_id != written_id {
        return Err(StorageError::new(
            "invalid_document",
            "候选笔记校验后身份与请求保存的笔记不一致。",
        ));
    }
    Ok(opened.revision)
}

fn write_and_verify_candidate(
    mut file: File,
    candidate: &Path,
    target: &Path,
    package: &NotePackage,
) -> Result<String, StorageError> {
    write_package(&mut file, package).map_err(StorageError::format)?;
    file.flush()
        .map_err(|error| StorageError::io("无法写入完整候选笔记", error))?;
    file.sync_all()
        .map_err(|error| StorageError::io("无法同步候选笔记", error))?;
    crate::storage_faults::checkpoint(
        "candidate_synced_before_validation",
        target,
        Some(candidate),
    )?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| StorageError::io("无法重新读取候选笔记", error))?;
    let opened = read_validated_file(&mut file)?;
    if opened.package.document.note_id != package.document.note_id {
        return Err(StorageError::new(
            "invalid_document",
            "候选笔记校验后身份与请求保存的笔记不一致。",
        ));
    }
    crate::storage_faults::checkpoint("candidate_validated", target, Some(candidate))?;
    Ok(opened.revision)
}

fn fail_with_snapshot(
    mut error: StorageError,
    snapshot: &RecoverySnapshot,
    candidate: Option<&Path>,
    keep_candidate: bool,
) -> StorageError {
    error.recovery_path = Some(snapshot.path.to_string_lossy().into_owned());
    if let Some(candidate) = candidate {
        if keep_candidate {
            if fs::symlink_metadata(candidate).is_ok() {
                error.recovery_error = Some(format!(
                    "保存前恢复快照保留于 {}；同目录候选文件也保留于 {}，其完整性未能确认",
                    snapshot.path.display(),
                    candidate.display()
                ));
            }
        } else if let Err(cleanup_error) = remove_owned_save_candidate(candidate) {
            error.recovery_error = Some(format!(
                "保存前恢复快照保留于 {}；无法清理同目录候选文件 {}：{cleanup_error}",
                snapshot.path.display(),
                candidate.display()
            ));
        }
    }
    error
}

fn remove_owned_save_candidate(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => Err(
            io::Error::new(io::ErrorKind::InvalidData, "候选路径类型已变化，未删除"),
        ),
        Ok(_) => fs::remove_file(path),
    }
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes() & 0x0000_0400 != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn create_candidate_path(parent: &Path) -> Result<(PathBuf, File), StorageError> {
    for _ in 0..4 {
        let path = parent.join(format!(
            ".cuenote-candidate-{}.tmp",
            Uuid::new_v4().simple()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StorageError::io("无法创建同目录候选笔记", error)),
        }
    }
    Err(StorageError::new(
        "io",
        "多次尝试后仍无法创建唯一候选笔记。",
    ))
}

fn prepare_recovery_root(root: &Path, note_parent: &Path) -> Result<PathBuf, StorageError> {
    let (root, existed) = resolve_recovery_path(root)?;
    if root.starts_with(note_parent) || note_parent.starts_with(&root) {
        return Err(StorageError::new(
            "unsafePath",
            "本机恢复目录必须与笔记目录树分开。",
        ));
    }
    if !existed {
        fs::create_dir_all(&root)
            .map_err(|error| StorageError::io("无法准备本机恢复目录", error))?;
    }
    let metadata = fs::symlink_metadata(&root)
        .map_err(|error| StorageError::io("无法检查本机恢复目录", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "恢复位置必须是实际目录，不能通过符号链接访问。",
        ));
    }
    let root =
        fs::canonicalize(&root).map_err(|error| StorageError::io("无法解析本机恢复目录", error))?;
    if root.starts_with(note_parent) || note_parent.starts_with(&root) {
        return Err(StorageError::new(
            "unsafePath",
            "本机恢复目录必须与笔记目录树分开。",
        ));
    }
    sync_directory_chain(&root).map_err(|error| {
        StorageError::io(&format!("无法同步本机恢复目录链 {}", root.display()), error)
    })?;
    Ok(root)
}

fn resolve_recovery_path(path: &Path) -> Result<(PathBuf, bool), StorageError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| StorageError::io("无法解析本机恢复目录", error))?
            .join(path)
    };
    let mut ancestor = absolute;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&ancestor) {
            Ok(metadata) => {
                if missing.is_empty() && metadata.file_type().is_symlink() {
                    return Err(StorageError::new(
                        "unsafePath",
                        "恢复位置必须是实际目录，不能通过符号链接访问。",
                    ));
                }
                let effective_metadata = if metadata.file_type().is_symlink() {
                    fs::metadata(&ancestor)
                        .map_err(|error| StorageError::io("无法检查恢复目录祖先", error))?
                } else {
                    metadata
                };
                if !effective_metadata.is_dir() {
                    return Err(StorageError::new(
                        "unsafePath",
                        "恢复位置必须是实际目录，不能位于普通文件之下。",
                    ));
                }
                let mut resolved = fs::canonicalize(&ancestor)
                    .map_err(|error| StorageError::io("无法解析本机恢复目录", error))?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok((resolved, missing.is_empty()));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    StorageError::new("unsafePath", "恢复目录路径无法解析为目录。")
                })?;
                missing.push(name.to_os_string());
                if !ancestor.pop() {
                    return Err(StorageError::new(
                        "unsafePath",
                        "恢复目录路径缺少可验证的现存祖先。",
                    ));
                }
            }
            Err(error) => return Err(StorageError::io("无法检查本机恢复目录", error)),
        }
    }
}

fn acquire_save_lock(recovery_root: &Path, target: &Path) -> io::Result<File> {
    let locks = recovery_root.join(".locks");
    fs::create_dir_all(&locks)?;
    sync_directory_chain(&locks)?;
    let key = digest_bytes(target.to_string_lossy().to_lowercase().as_bytes());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(locks.join(format!("{key}.lock")))?;
    file.lock()?;
    Ok(file)
}

fn preserve_history_version(
    target: &Path,
    recovery_root: &Path,
    note_id: &str,
    expected_revision: &str,
) -> Result<PathBuf, StorageError> {
    let directory = history_directory(recovery_root, note_id);
    fs::create_dir_all(&directory)
        .map_err(|error| StorageError::io("无法准备笔记历史目录", error))?;
    sync_directory_chain(&directory).map_err(|error| {
        StorageError::io(
            &format!("无法同步笔记历史目录链 {}", directory.display()),
            error,
        )
    })?;
    let mut destination = directory.join(format!("{expected_revision}.cuenote"));
    if destination.exists() {
        match inspect_recovery_note(&destination) {
            Ok(Some(note))
                if note.revision == expected_revision
                    && note.package.document.note_id == note_id =>
            {
                sync_directory_chain(&directory).map_err(|error| {
                    StorageError::io(
                        &format!(
                            "无法同步已存在的历史目录链；旧版副本保留于 {}",
                            destination.display()
                        ),
                        error,
                    )
                })?;
                return Ok(destination);
            }
            Ok(_) | Err(_) => {
                destination = directory.join(format!(
                    "{expected_revision}-{}.cuenote",
                    Uuid::new_v4().simple()
                ));
            }
        }
    }
    let mut source = open_read_nofollow(target)
        .map_err(|error| StorageError::io("无法读取待保留的旧版笔记", error))?;
    let old = read_validated_file(&mut source)?;
    if old.revision != expected_revision || old.package.document.note_id != note_id {
        return Err(StorageError::new(
            "conflict",
            "保留旧版前，当前笔记的版本或身份发生变化。",
        ));
    }
    let mut backup = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|error| StorageError::io("无法创建独立旧版副本", error))?;
    let copy_result = (|| {
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| StorageError::io("无法定位待保留的旧版笔记", error))?;
        copy_at_most_package_size(&mut source, &mut backup, "无法复制旧版笔记")?;
        backup
            .flush()
            .map_err(|error| StorageError::io("无法写入完整旧版副本", error))?;
        backup
            .sync_all()
            .map_err(|error| StorageError::io("无法同步完整旧版副本", error))
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_file(&destination);
        return Err(error);
    }
    let verified_result = (|| {
        backup
            .seek(SeekFrom::Start(0))
            .map_err(|error| StorageError::io("无法重新读取旧版副本", error))?;
        read_validated_file(&mut backup)
    })();
    let verified = match verified_result {
        Ok(verified) => verified,
        Err(error) => {
            let _ = fs::remove_file(&destination);
            return Err(StorageError::io("无法校验独立旧版副本", error));
        }
    };
    if verified.revision != expected_revision || verified.package.document.note_id != note_id {
        let _ = fs::remove_file(&destination);
        return Err(StorageError::new(
            "conflict",
            "独立旧版副本的身份或内容指纹校验失败。",
        ));
    }
    if let Err(error) = backup.seek(SeekFrom::Start(0)) {
        let _ = fs::remove_file(&destination);
        return Err(StorageError::io("无法定位已验证旧版副本", error));
    }
    sync_directory_chain(&directory).map_err(|error| {
        StorageError::io(
            &format!(
                "无法同步历史目录项；旧版副本保留于 {}",
                destination.display()
            ),
            error,
        )
    })?;
    Ok(destination)
}

fn inspect_recovery_note(path: &Path) -> Result<Option<OpenedNote>, StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "恢复文件路径缺少目录。"))?;
    let target = ResolvedTarget {
        path: path.to_path_buf(),
        parent: parent.to_path_buf(),
    };
    inspect_note(&target)
}

fn history_directory(recovery_root: &Path, note_id: &str) -> PathBuf {
    recovery_root
        .join("history")
        .join(digest_bytes(note_id.as_bytes()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn copy_at_most_package_size<R: Read, W: Write>(
    source: &mut R,
    destination: &mut W,
    context: &str,
) -> Result<u64, StorageError> {
    let copied = io::copy(
        &mut source.by_ref().take(MAX_PACKAGE_BYTES + 1),
        destination,
    )
    .map_err(|error| StorageError::io(context, error))?;
    if copied > MAX_PACKAGE_BYTES {
        return Err(StorageError::new(
            "budget_exceeded",
            "笔记文件超过格式层规定的安全大小上限，已拒绝复制。",
        ));
    }
    Ok(copied)
}

fn copy_verified_note(
    source_path: &Path,
    destination_path: &Path,
    expected_note_id: &str,
) -> Result<String, StorageError> {
    let mut source = open_read_nofollow(source_path)
        .map_err(|error| StorageError::io("无法读取恢复草稿来源", error))?;
    let source_note = read_validated_file(&mut source)?;
    if source_note.package.document.note_id != expected_note_id {
        return Err(StorageError::new(
            "invalid_document",
            "候选草稿的笔记身份与当前保存请求不一致。",
        ));
    }
    let mut destination = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(destination_path)
        .map_err(|error| StorageError::io("无法创建恢复草稿副本", error))?;
    let copy_result = (|| {
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| StorageError::io("无法定位恢复草稿来源", error))?;
        copy_at_most_package_size(&mut source, &mut destination, "无法复制恢复草稿")?;
        destination
            .flush()
            .map_err(|error| StorageError::io("无法写入恢复草稿副本", error))?;
        destination
            .sync_all()
            .map_err(|error| StorageError::io("无法同步恢复草稿副本", error))
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_file(destination_path);
        return Err(error);
    }
    let verified_result = (|| {
        destination
            .seek(SeekFrom::Start(0))
            .map_err(|error| StorageError::io("无法重新读取恢复草稿", error))?;
        read_validated_file(&mut destination)
    })();
    let verified = match verified_result {
        Ok(verified) => verified,
        Err(error) => {
            let _ = fs::remove_file(destination_path);
            return Err(StorageError::io("无法校验恢复草稿副本", error));
        }
    };
    if verified.package.document.note_id != expected_note_id
        || verified.revision != source_note.revision
    {
        let _ = fs::remove_file(destination_path);
        return Err(StorageError::new(
            "invalid_archive",
            "恢复草稿副本与候选文件校验结果不一致。",
        ));
    }
    let parent = destination_path
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "恢复草稿副本路径缺少父目录。"))?;
    sync_directory_chain(parent).map_err(|error| {
        StorageError::io(
            &format!(
                "恢复草稿副本已校验但无法同步目录链；文件保留于 {}",
                destination_path.display()
            ),
            error,
        )
    })?;
    Ok(verified.revision)
}

fn rotate_history(directory: &Path) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut versions = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("cuenote") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.1.cmp(&left.1));
    for (path, _) in versions.into_iter().skip(HISTORY_LIMIT) {
        let _ = fs::remove_file(path);
    }
}

fn publish_create(candidate: &Path, target: &Path) -> Result<(), StorageError> {
    #[cfg(windows)]
    {
        move_file_no_replace(candidate, target).map_err(|error| {
            if matches!(error.raw_os_error(), Some(80 | 183)) {
                StorageError::new("conflict", "保存前已有其他笔记文件创建。")
            } else {
                StorageError::io("无法原子创建笔记文件", error)
            }
        })
    }
    #[cfg(unix)]
    {
        fs::hard_link(candidate, target).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                StorageError::new("conflict", "保存前已有其他笔记文件创建。")
            } else {
                StorageError::io("文件系统无法安全原子创建笔记", error)
            }
        })?;
        let _ = fs::remove_file(candidate);
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = candidate;
        let _ = target;
        Err(StorageError::new(
            "publish",
            "当前平台没有实现无覆盖原子创建笔记。",
        ))
    }
}

fn publish_replace(
    candidate: &Path,
    target: &Path,
    expected_revision: &str,
    history_backup: Option<&Path>,
) -> Result<(), StorageError> {
    #[cfg(windows)]
    {
        let backup = target.with_file_name(format!(
            ".cuenote-publish-backup-{}.tmp",
            Uuid::new_v4().simple()
        ));
        if let Err(error) = replace_windows(target, candidate, &backup) {
            let restoration = restore_windows_backup_if_target_missing(
                &backup,
                target,
                expected_revision,
                history_backup,
            );
            return Err(StorageError::new(
                "publish",
                format!(
                    "无法安全发布笔记：{error}；{restoration}；候选文件路径为 {}，替换备份路径为 {}。",
                    candidate.display(),
                    backup.display()
                ),
            ));
        }
        match inspect_path_revision(&backup) {
            Ok(Some(revision)) if revision == expected_revision => {
                let _ = fs::remove_file(backup);
                Ok(())
            }
            Ok(Some(revision)) => Err(StorageError::new(
                "conflict",
                format!(
                    "替换已完成，但 Windows 备份版本与保存基线不一致（预期 {expected_revision}，实际 {revision}）；备份保留于 {}。",
                    backup.display()
                ),
            )),
            Ok(None) => Err(StorageError::new(
                "publish",
                format!(
                    "替换已完成，但 Windows 未留下可读取的旧版备份；备份路径保留为 {}。",
                    backup.display()
                ),
            )),
            Err(error) => Err(StorageError::new(
                "publish",
                format!(
                    "替换已完成，但无法校验 Windows 旧版备份：{}；备份保留于 {}。",
                    error.message,
                    backup.display()
                ),
            )),
        }
    }
    #[cfg(unix)]
    {
        let _ = history_backup;
        fs::rename(candidate, target)
            .map_err(|error| StorageError::io("无法原子替换笔记文件", error))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = candidate;
        let _ = target;
        let _ = expected_revision;
        let _ = history_backup;
        Err(StorageError::new(
            "publish",
            "当前平台没有实现安全原子替换笔记。",
        ))
    }
}

fn inspect_path_revision(path: &Path) -> Result<Option<String>, StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "笔记路径缺少目录。"))?;
    let target = ResolvedTarget {
        path: path.to_path_buf(),
        parent: parent.to_path_buf(),
    };
    Ok(inspect_note(&target)?.map(|note| note.revision))
}

fn replace_path(candidate: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        replace_windows(target, candidate, backup)
    }
    #[cfg(unix)]
    {
        let _ = backup;
        fs::rename(candidate, target)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = candidate;
        let _ = target;
        let _ = backup;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic replacement is unsupported on this platform",
        ))
    }
}

fn read_metadata_file(path: &Path) -> Result<Vec<u8>, StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::io("无法检查笔记本元数据", error))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            "笔记本元数据必须是普通文件。",
        ));
    }
    if metadata.len() > MAX_METADATA_REPLACEMENT_BYTES {
        return Err(StorageError::new(
            "libraryMetadata",
            "笔记本元数据超过大小上限。",
        ));
    }
    let file = open_read_nofollow(path)
        .map_err(|error| StorageError::io("无法读取笔记本元数据", error))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_METADATA_REPLACEMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::io("无法读取笔记本元数据", error))?;
    if bytes.len() as u64 > MAX_METADATA_REPLACEMENT_BYTES {
        return Err(StorageError::new(
            "libraryMetadata",
            "笔记本元数据超过大小上限。",
        ));
    }
    Ok(bytes)
}

fn replace_metadata_candidate(
    target: &Path,
    candidate: &Path,
    parent: &Path,
    expected_bytes: &[u8],
    replacement_bytes: &[u8],
) -> Result<(), StorageError> {
    let backup = parent.join(format!(
        ".workspace-metadata-backup-{}.tmp",
        Uuid::new_v4().simple()
    ));

    #[cfg(windows)]
    {
        if let Err(error) = replace_windows(target, candidate, &backup) {
            let restoration =
                restore_metadata_backup_if_target_missing(&backup, target, expected_bytes);
            return Err(StorageError::new(
                "publish",
                format!(
                    "无法原子替换笔记本元数据：{error}；{restoration}；备份路径为 {}。",
                    backup.display()
                ),
            ));
        }
        let backup_bytes = read_metadata_file(&backup).map_err(|error| {
            StorageError::new(
                "publish",
                format!(
                    "元数据已替换，但无法校验旧版备份：{}；备份保留于 {}。",
                    error.message,
                    backup.display()
                ),
            )
        })?;
        if backup_bytes != expected_bytes {
            return Err(StorageError::new(
                "conflict",
                format!(
                    "元数据已替换，但旧版备份与发布基线不一致；备份保留于 {}。",
                    backup.display()
                ),
            ));
        }
        let published_bytes = read_metadata_file(target).map_err(|error| {
            StorageError::new(
                "publish",
                format!(
                    "元数据已替换，但无法验证新文件：{}；旧版备份保留于 {}。",
                    error.message,
                    backup.display()
                ),
            )
        })?;
        if published_bytes != replacement_bytes {
            return Err(StorageError::new(
                "publish",
                format!(
                    "元数据已替换，但新文件与候选不一致；旧版备份保留于 {}。",
                    backup.display()
                ),
            ));
        }
        let _ = fs::remove_file(backup);
        let _ = sync_parent_directory(parent);
        Ok(())
    }

    #[cfg(unix)]
    {
        fs::hard_link(target, &backup)
            .map_err(|error| StorageError::io("无法保留笔记本元数据旧版", error))?;
        let backup_bytes = read_metadata_file(&backup)?;
        if backup_bytes != expected_bytes {
            let _ = fs::remove_file(&backup);
            return Err(StorageError::new(
                "conflict",
                "笔记本元数据在备份期间发生变化；原文件已保留。",
            ));
        }
        if let Err(error) = fs::rename(candidate, target) {
            let recovery = match fs::symlink_metadata(target) {
                Err(status) if status.kind() == io::ErrorKind::NotFound => {
                    match fs::hard_link(&backup, target) {
                        Ok(()) => "已使用旧版备份恢复原路径".to_owned(),
                        Err(restore_error) => format!(
                            "无法恢复原路径；旧版字节仍保留于 {}：{restore_error}",
                            backup.display()
                        ),
                    }
                }
                Ok(_) => "目标路径仍存在，未覆盖原文件".to_owned(),
                Err(status) => format!("无法检查目标路径，旧版备份保留：{status}"),
            };
            if read_metadata_file(target).is_ok_and(|bytes| bytes == expected_bytes) {
                let _ = fs::remove_file(&backup);
            }
            return Err(StorageError::new(
                "publish",
                format!("无法原子替换笔记本元数据：{error}；{recovery}。"),
            ));
        }
        if let Err(error) = sync_parent_directory(parent) {
            let restoration = match fs::rename(&backup, target) {
                Ok(()) => {
                    let _ = sync_parent_directory(parent);
                    "已恢复原元数据字节".to_owned()
                }
                Err(restore_error) => format!(
                    "无法恢复目标路径；旧版字节仍保留于 {}：{restore_error}",
                    backup.display()
                ),
            };
            return Err(StorageError::new(
                "publish",
                format!("无法同步笔记本元数据目录：{error}；{restoration}。"),
            ));
        }
        let backup_bytes = read_metadata_file(&backup).map_err(|error| {
            StorageError::new(
                "publish",
                format!(
                    "元数据已替换，但无法校验旧版备份：{}；备份保留于 {}。",
                    error.message,
                    backup.display()
                ),
            )
        })?;
        if backup_bytes != expected_bytes {
            return Err(StorageError::new(
                "conflict",
                format!(
                    "元数据已替换，但旧版备份与发布基线不一致；备份保留于 {}。",
                    backup.display()
                ),
            ));
        }
        let published_bytes = read_metadata_file(target).map_err(|error| {
            StorageError::new(
                "publish",
                format!(
                    "元数据已替换，但无法验证新文件：{}；旧版备份保留于 {}。",
                    error.message,
                    backup.display()
                ),
            )
        })?;
        if published_bytes != replacement_bytes {
            return Err(StorageError::new(
                "publish",
                format!(
                    "元数据已替换，但新文件与候选不一致；旧版备份保留于 {}。",
                    backup.display()
                ),
            ));
        }
        let _ = fs::remove_file(backup);
        let _ = sync_parent_directory(parent);
        Ok(())
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = (target, candidate, parent, expected_bytes, replacement_bytes);
        Err(StorageError::new(
            "publish",
            "当前平台没有实现安全原子替换笔记本元数据。",
        ))
    }
}

#[cfg(windows)]
fn restore_metadata_backup_if_target_missing(
    backup: &Path,
    target: &Path,
    expected_bytes: &[u8],
) -> String {
    match fs::symlink_metadata(target) {
        Ok(_) => return "目标路径仍存在，未移动备份".to_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return format!("无法检查目标路径，备份保留：{error}"),
    }
    match read_metadata_file(backup) {
        Ok(bytes) if bytes == expected_bytes => match move_file_no_replace(backup, target) {
            Ok(()) => "目标缺失时已恢复校验过的原元数据".to_owned(),
            Err(error) => format!(
                "无法恢复目标路径；原元数据备份保留于 {}：{error}",
                backup.display()
            ),
        },
        Ok(_) => format!("备份与发布基线不一致，未移动：{}", backup.display()),
        Err(error) => format!(
            "无法读取元数据备份：{}；路径为 {}",
            error.message,
            backup.display()
        ),
    }
}

#[cfg(windows)]
fn replace_windows(target: &Path, candidate: &Path, backup: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let candidate_wide = candidate
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let backup_wide = backup
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            candidate_wide.as_ptr(),
            backup_wide.as_ptr(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

#[cfg(windows)]
fn restore_windows_backup_if_target_missing(
    backup: &Path,
    target: &Path,
    expected_revision: &str,
    history_backup: Option<&Path>,
) -> String {
    match fs::symlink_metadata(target) {
        Ok(_) => return "目标路径仍存在，未执行恢复移动".to_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return format!("无法确认目标路径是否缺失，因此未执行恢复移动：{error}");
        }
    }
    let backup_status = match inspect_path_revision(backup) {
        Ok(Some(revision)) if revision == expected_revision => {
            return match move_file_no_replace(backup, target) {
                Ok(()) => "目标路径缺失，已用无覆盖移动恢复校验过的基线备份".to_owned(),
                Err(error) => format!(
                    "目标路径缺失；已校验备份仍保留于 {}，无覆盖恢复失败：{error}",
                    backup.display()
                ),
            };
        }
        Ok(Some(revision)) => format!(
            "替换备份版本与基线不同（预期 {expected_revision}，实际 {revision}），备份保留于 {}",
            backup.display()
        ),
        Ok(None) => format!("替换备份路径 {} 不存在或不可读", backup.display()),
        Err(error) => format!(
            "无法校验替换备份（{}）：{}",
            backup.display(),
            error.message
        ),
    };
    if let Some(history) = history_backup {
        match restore_from_verified_history(history, target, expected_revision) {
            Ok(()) => format!(
                "目标路径缺失；替换备份状态：{backup_status}；已从独立历史副本无覆盖恢复基线版本"
            ),
            Err(error) => format!(
                "目标路径缺失；替换备份状态：{backup_status}；历史副本也无法无覆盖恢复：{error}"
            ),
        }
    } else {
        format!("目标路径缺失；替换备份状态：{backup_status}；没有独立历史副本可用于恢复")
    }
}

#[cfg(windows)]
fn restore_from_verified_history(
    history: &Path,
    target: &Path,
    expected_revision: &str,
) -> Result<(), String> {
    let history_note = open_note(history).map_err(|error| error.message)?;
    if history_note.revision != expected_revision {
        return Err(format!(
            "历史文件 {} 的版本与保存基线不一致",
            history.display()
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| "目标笔记路径缺少目录".to_owned())?;
    let candidate = parent.join(format!(".cuenote-restore-{}.tmp", Uuid::new_v4().simple()));
    let copied_revision =
        copy_verified_note(history, &candidate, &history_note.package.document.note_id)
            .map_err(|error| error.message)?;
    if copied_revision != expected_revision {
        let cleanup = match fs::remove_file(&candidate) {
            Ok(()) => format!("临时候选已删除；历史副本仍保留于 {}", history.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                format!("临时候选已不存在；历史副本仍保留于 {}", history.display())
            }
            Err(error) => format!(
                "无法清理临时候选（{error}），候选仍在 {}；历史副本保留于 {}",
                candidate.display(),
                history.display()
            ),
        };
        return Err(format!("恢复候选版本与基线不一致；{cleanup}"));
    }
    move_file_no_replace(&candidate, target).map_err(|error| {
        format!(
            "无覆盖移动失败，已验证恢复候选保留于 {}：{error}",
            candidate.display()
        )
    })
}

#[cfg(windows)]
fn move_file_no_replace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn ReplaceFileW(
        replaced_file_name: *const u16,
        replacement_file_name: *const u16,
        backup_file_name: *const u16,
        flags: u32,
        exclude: *mut std::ffi::c_void,
        reserved: *mut std::ffi::c_void,
    ) -> i32;
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

fn open_read_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x0000_0001).custom_flags(0x0020_0000);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0000_0100);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000);
    }
    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0000_0100);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x0000_0400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "笔记路径是 Windows 重解析点",
            ));
        }
    }
    Ok(file)
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        Ok(())
    }
}

fn sync_directory_chain(path: &Path) -> io::Result<()> {
    let mut current = Some(path);
    while let Some(directory) = current {
        sync_parent_directory(directory)?;
        let parent = directory.parent();
        if parent == Some(directory) {
            break;
        }
        current = parent;
    }
    Ok(())
}
