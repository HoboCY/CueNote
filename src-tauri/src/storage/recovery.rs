use super::{
    digest_bytes, inspect_recovery_note, publish_create, resolve_recovery_path, ResolvedTarget,
    StorageError,
};
use crate::note_format::NotePackage;
use chrono::{SecondsFormat, Utc};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_RECOVERY_SCAN_ENTRIES: usize = 100_000;
const MAX_RECOVERY_DRAFTS: usize = 20_000;
const MAX_RECOVERY_WARNINGS: usize = 100;

#[derive(Debug, Clone)]
pub(crate) struct RecoverySnapshot {
    pub(crate) path: PathBuf,
    pub(crate) recovery_id: String,
    pub(crate) note_id: String,
    pub(crate) revision: String,
}

#[derive(Debug)]
pub(crate) struct RecoveryDraftRecord {
    pub(crate) recovery_id: String,
    pub(crate) note_id: String,
    pub(crate) title: String,
    pub(crate) saved_at: String,
    pub(crate) revision: String,
}

#[derive(Debug)]
pub(crate) struct OpenedRecoveryDraft {
    pub(crate) package: NotePackage,
    pub(crate) snapshot: RecoverySnapshot,
}

pub(crate) fn create_recovery_snapshot(
    recovery_root: &Path,
    package: &NotePackage,
) -> Result<RecoverySnapshot, StorageError> {
    let note_id = package.document.note_id.clone();
    let note_hash = digest_bytes(note_id.as_bytes());
    let drafts_root = ensure_child_directory(recovery_root, "drafts")?;
    let directory = ensure_child_directory(&drafts_root, &note_hash)?;
    let (candidate_path, candidate_file) = super::create_candidate_path(&directory)?;

    let revision = match super::write_and_verify_package(candidate_file, package) {
        Ok(revision) => revision,
        Err(error) => {
            let cleanup = remove_owned_candidate(&candidate_path);
            return Err(append_candidate_cleanup(error, &candidate_path, cleanup));
        }
    };

    let draft_token = Uuid::new_v4().simple().to_string();
    let destination = directory.join(format!("draft-{draft_token}.cuenote"));
    if let Err(error) = publish_create(&candidate_path, &destination) {
        let cleanup = remove_owned_candidate(&candidate_path);
        return Err(append_candidate_cleanup(
            StorageError::new(
                "recovery",
                format!(
                    "无法无覆盖发布保存前恢复快照：{}；目标归属未确认，若仍存在则保留 {}。",
                    error.message,
                    destination.display()
                ),
            ),
            &candidate_path,
            cleanup,
        ));
    }

    #[cfg(windows)]
    let destination = match super::windows_verbatim_path(&destination) {
        Ok(path) => path,
        Err(error) => {
            let mut storage_error = StorageError::io(
                &format!(
                    "恢复快照已发布，但无法准备其长路径；文件仍保留于 {}",
                    destination.display()
                ),
                error,
            );
            storage_error.recovery_path = Some(destination.to_string_lossy().into_owned());
            return Err(storage_error);
        }
    };

    if let Err(error) = super::sync_directory_chain(&directory) {
        let mut storage_error = StorageError::io(
            &format!(
                "恢复快照已写入并校验，但无法同步目录链；文件保留于 {}",
                destination.display()
            ),
            error,
        );
        storage_error.recovery_path = Some(destination.to_string_lossy().into_owned());
        return Err(storage_error);
    }

    let opened = match inspect_recovery_note(&destination) {
        Ok(Some(opened))
            if opened.revision == revision && opened.package.document.note_id == note_id =>
        {
            opened
        }
        Ok(_) => {
            let mut error = StorageError::new(
                "recovery",
                format!(
                    "恢复快照发布后身份或内容指纹不匹配；文件保留于 {}。",
                    destination.display()
                ),
            );
            error.recovery_path = Some(destination.to_string_lossy().into_owned());
            return Err(error);
        }
        Err(mut error) => {
            error.message = format!(
                "恢复快照发布后无法重读；文件可能保留于 {}：{}",
                destination.display(),
                error.message
            );
            error.recovery_path = Some(destination.to_string_lossy().into_owned());
            return Err(error);
        }
    };

    // Unix publication uses a hard link and normally removes this source itself.
    if fs::symlink_metadata(&candidate_path).is_ok() {
        if let Err(error) = remove_owned_candidate(&candidate_path) {
            let mut storage_error = StorageError::io(
                &format!(
                    "恢复快照已持久化于 {}，但无法清理同内容候选文件 {}",
                    destination.display(),
                    candidate_path.display()
                ),
                error,
            );
            storage_error.recovery_path = Some(destination.to_string_lossy().into_owned());
            return Err(storage_error);
        }
    }

    let _ = opened;
    Ok(RecoverySnapshot {
        path: destination,
        recovery_id: recovery_id(&note_hash, &draft_token, &revision),
        note_id,
        revision,
    })
}

pub(crate) fn list_recovery_drafts(
    recovery_root: &Path,
) -> Result<(Vec<RecoveryDraftRecord>, Vec<String>), StorageError> {
    let Some(root) = existing_recovery_root(recovery_root)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let drafts_root = root.join("drafts");
    let drafts_metadata = match fs::symlink_metadata(&drafts_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(error) => return Err(StorageError::io("无法检查恢复草稿目录", error)),
    };
    if is_link_or_reparse(&drafts_metadata) || !drafts_metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "恢复草稿目录必须是实际目录，不能是文件或重解析点。",
        ));
    }
    let entries = fs::read_dir(&drafts_root)
        .map_err(|error| StorageError::io("无法读取恢复草稿目录", error))?;
    let mut scanned_entries = 0usize;
    let mut drafts = Vec::new();
    let mut warnings = Vec::new();

    for entry in entries {
        scanned_entries += 1;
        ensure_scan_limit(scanned_entries)?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                push_warning(&mut warnings, format!("无法读取恢复草稿目录项：{error}"));
                continue;
            }
        };
        let directory = entry.path();
        let name = match entry.file_name().to_str().map(str::to_owned) {
            Some(name) if is_lower_hex(&name, 64) => name,
            Some(_) => {
                push_warning(
                    &mut warnings,
                    format!(
                        "恢复草稿分组名称无效，已保留并跳过：{}",
                        directory.display()
                    ),
                );
                continue;
            }
            None => {
                push_warning(
                    &mut warnings,
                    format!(
                        "恢复草稿分组名称不可读，已保留并跳过：{}",
                        directory.display()
                    ),
                );
                continue;
            }
        };
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) => {
                push_warning(
                    &mut warnings,
                    format!(
                        "无法检查恢复草稿分组，已保留：{}：{error}",
                        directory.display()
                    ),
                );
                continue;
            }
        };
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            push_warning(
                &mut warnings,
                format!(
                    "恢复草稿分组不是普通目录，已保留并跳过：{}",
                    directory.display()
                ),
            );
            continue;
        }
        let group_entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                push_warning(
                    &mut warnings,
                    format!(
                        "无法读取恢复草稿分组，已保留其内容：{}：{error}",
                        directory.display()
                    ),
                );
                continue;
            }
        };
        for group_entry in group_entries {
            scanned_entries += 1;
            ensure_scan_limit(scanned_entries)?;
            let group_entry = match group_entry {
                Ok(entry) => entry,
                Err(error) => {
                    push_warning(
                        &mut warnings,
                        format!("无法读取恢复草稿项（分组 {name}）：{error}"),
                    );
                    continue;
                }
            };
            let path = group_entry.path();
            let file_name = match group_entry.file_name().to_str().map(str::to_owned) {
                Some(name) => name,
                None => {
                    push_warning(
                        &mut warnings,
                        format!("恢复草稿文件名不可读，已保留并跳过：{}", path.display()),
                    );
                    continue;
                }
            };
            let Some(token) = parse_draft_filename(&file_name) else {
                push_warning(
                    &mut warnings,
                    format!("恢复目录中有未识别文件，已保留并跳过：{}", path.display()),
                );
                continue;
            };
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    push_warning(
                        &mut warnings,
                        format!("无法检查恢复草稿，已保留：{}：{error}", path.display()),
                    );
                    continue;
                }
            };
            if is_link_or_reparse(&metadata) || !metadata.is_file() {
                push_warning(
                    &mut warnings,
                    format!("恢复草稿不是普通文件，已保留并跳过：{}", path.display()),
                );
                continue;
            }
            let opened = match inspect_recovery_note(&path) {
                Ok(Some(opened)) => opened,
                Ok(None) => {
                    push_warning(
                        &mut warnings,
                        format!("恢复草稿不可读取，已保留：{}", path.display()),
                    );
                    continue;
                }
                Err(error) => {
                    push_warning(
                        &mut warnings,
                        format!(
                            "恢复草稿校验失败，已保留：{}：{}",
                            path.display(),
                            error.message
                        ),
                    );
                    continue;
                }
            };
            let note_id = opened.package.document.note_id.clone();
            if digest_bytes(note_id.as_bytes()) != name {
                push_warning(
                    &mut warnings,
                    format!("恢复草稿身份与分组不一致，已保留：{}", path.display()),
                );
                continue;
            }
            let modified = match metadata.modified() {
                Ok(modified) => modified,
                Err(error) => {
                    push_warning(
                        &mut warnings,
                        format!(
                            "无法读取恢复草稿时间，已保留但暂不列出：{}：{error}",
                            path.display()
                        ),
                    );
                    continue;
                }
            };
            let saved_at = chrono::DateTime::<Utc>::from(modified)
                .to_rfc3339_opts(SecondsFormat::Millis, true);
            let revision = opened.revision;
            if drafts.len() >= MAX_RECOVERY_DRAFTS {
                return Err(StorageError::new(
                    "recoveryLimit",
                    format!(
                        "恢复草稿超过单次列举上限 {MAX_RECOVERY_DRAFTS}；没有静默省略其余草稿。"
                    ),
                ));
            }
            drafts.push(RecoveryDraftRecord {
                recovery_id: recovery_id(&name, &token, &revision),
                note_id,
                title: opened.package.document.title,
                saved_at,
                revision,
            });
        }
    }

    drafts.sort_by(|left, right| {
        right
            .saved_at
            .cmp(&left.saved_at)
            .then_with(|| left.recovery_id.cmp(&right.recovery_id))
    });
    Ok((drafts, warnings))
}

pub(crate) fn open_recovery_draft(
    recovery_root: &Path,
    requested_id: &str,
) -> Result<OpenedRecoveryDraft, StorageError> {
    let (note_hash, token, expected_revision) = parse_recovery_id(requested_id)?;
    let root = existing_recovery_root(recovery_root)?
        .ok_or_else(|| StorageError::new("recoveryNotFound", "恢复草稿不存在或恢复目录不可用。"))?;
    let drafts_root = checked_child_directory(&root, "drafts")?;
    let directory = checked_child_directory(&drafts_root, &note_hash)?;
    let path = directory.join(format!("draft-{token}.cuenote"));
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| StorageError::io("无法检查指定恢复草稿", error))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            "指定恢复草稿不是普通文件。",
        ));
    }
    let opened = inspect_recovery_note(&path)?
        .ok_or_else(|| StorageError::new("recoveryNotFound", "指定恢复草稿不存在。"))?;
    let note_id = opened.package.document.note_id.clone();
    if digest_bytes(note_id.as_bytes()) != note_hash || opened.revision != expected_revision {
        return Err(StorageError::new(
            "recoveryChanged",
            "恢复草稿在列出后发生变化；请刷新恢复列表后重试。",
        ));
    }
    Ok(OpenedRecoveryDraft {
        package: opened.package,
        snapshot: RecoverySnapshot {
            path,
            recovery_id: requested_id.to_owned(),
            note_id,
            revision: opened.revision,
        },
    })
}

pub(crate) fn remove_recovery_snapshot(
    recovery_root: &Path,
    snapshot: &RecoverySnapshot,
) -> Result<(), StorageError> {
    let opened = open_recovery_draft(recovery_root, &snapshot.recovery_id)?;
    if opened.snapshot.path != snapshot.path
        || opened.snapshot.note_id != snapshot.note_id
        || opened.snapshot.revision != snapshot.revision
    {
        return Err(StorageError::new(
            "recoveryChanged",
            "本次保存对应的恢复快照已变化；未删除该文件。",
        ));
    }
    let parent = snapshot
        .path
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "恢复快照路径缺少父目录。"))?;
    fs::remove_file(&snapshot.path)
        .map_err(|error| StorageError::io("无法清理本次保存的恢复快照", error))?;
    super::sync_directory_chain(parent).map_err(|error| {
        StorageError::io(
            &format!(
                "本次恢复快照已删除，但无法同步恢复目录链 {}",
                parent.display()
            ),
            error,
        )
    })
}

pub(crate) fn save_copy(
    target: &ResolvedTarget,
    package: &NotePackage,
) -> Result<(PathBuf, String), StorageError> {
    target.directory.ensure_current()?;
    let (candidate_path, candidate_file) = target.directory.create_candidate()?;
    if let Err(error) = target.directory.ensure_current() {
        return Err(preserve_copy_candidate(error, &candidate_path));
    }
    let revision = match super::write_and_verify_package(candidate_file, package) {
        Ok(revision) => revision,
        Err(error) => {
            return Err(preserve_copy_candidate(error, &candidate_path));
        }
    };
    if let Err(error) = target.directory.ensure_current() {
        return Err(preserve_copy_candidate(error, &candidate_path));
    }
    if let Err(error) = target
        .directory
        .move_no_replace(&candidate_path, &target.path)
    {
        let error = if error.kind() == io::ErrorKind::AlreadyExists
            || matches!(error.raw_os_error(), Some(80 | 183))
        {
            StorageError::new("conflict", "另存目标已存在；没有覆盖该文件。")
        } else {
            StorageError::io("无法无覆盖原子发布另存文件", error)
        };
        return Err(preserve_copy_candidate(error, &candidate_path));
    }
    target
        .directory
        .sync_publication_directory()
        .map_err(|error| {
            StorageError::io(
                &format!(
                    "另存文件已发布，但无法同步所在目录 {}",
                    target.parent.display()
                ),
                error,
            )
        })?;
    target.directory.ensure_current()?;
    let opened = inspect_recovery_note(&target.path)?
        .ok_or_else(|| StorageError::new("publish", "另存文件发布后无法重新读取。"))?;
    if opened.revision != revision || opened.package.document.note_id != package.document.note_id {
        return Err(StorageError::new(
            "publish",
            "另存文件发布后的身份或内容指纹与候选不匹配。",
        ));
    }
    target.directory.ensure_current()?;
    Ok((target.path.clone(), revision))
}

fn preserve_copy_candidate(mut error: StorageError, candidate: &Path) -> StorageError {
    // A directory handle prevents redirection, but does not establish ownership
    // of a child name after another process may have replaced that child.
    // Leave all failure artifacts untouched instead of deleting by name.
    error.recovery_error = Some(format!(
        "另存候选未执行清理；若尚未发布，文件保留在原固定目录中。创建时路径：{}。目录变化时需先核对实际位置。",
        candidate.display()
    ));
    error
}

fn ensure_child_directory(parent: &Path, name: &str) -> Result<PathBuf, StorageError> {
    let directory = parent.join(name);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => {
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new(
                    "unsafePath",
                    format!("恢复目录项必须是实际目录：{}", directory.display()),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(&directory) {
                Ok(()) => {
                    super::sync_directory_chain(parent).map_err(|error| {
                        StorageError::io(
                            &format!("无法同步恢复目录父链 {}", parent.display()),
                            error,
                        )
                    })?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(StorageError::io(
                        &format!("无法创建恢复目录 {}", directory.display()),
                        error,
                    ));
                }
            }
            let metadata = fs::symlink_metadata(&directory)
                .map_err(|error| StorageError::io("无法检查新建恢复目录", error))?;
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new(
                    "unsafePath",
                    format!("新建恢复目录不是实际目录：{}", directory.display()),
                ));
            }
        }
        Err(error) => {
            return Err(StorageError::io(
                &format!("无法检查恢复目录 {}", directory.display()),
                error,
            ));
        }
    }
    Ok(directory)
}

fn checked_child_directory(parent: &Path, name: &str) -> Result<PathBuf, StorageError> {
    let directory = parent.join(name);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| StorageError::io("无法检查恢复目录项", error))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            format!("恢复目录项不是实际目录：{}", directory.display()),
        ));
    }
    Ok(directory)
}

fn existing_recovery_root(root: &Path) -> Result<Option<PathBuf>, StorageError> {
    let (resolved, existed) = resolve_recovery_path(root)?;
    if !existed {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(&resolved)
        .map_err(|error| StorageError::io("无法检查本机恢复目录", error))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "本机恢复目录必须是实际目录，不能通过链接或重解析点访问。",
        ));
    }
    fs::canonicalize(&resolved)
        .map(Some)
        .map_err(|error| StorageError::io("无法解析本机恢复目录", error))
}

fn parse_recovery_id(value: &str) -> Result<(String, String, String), StorageError> {
    let mut parts = value.split(':');
    let note_hash = parts.next().unwrap_or_default();
    let token = parts.next().unwrap_or_default();
    let revision = parts.next().unwrap_or_default();
    let valid_uuid = Uuid::parse_str(token)
        .map(|uuid| uuid.get_version_num() == 4 && uuid.simple().to_string() == token)
        .unwrap_or(false);
    if parts.next().is_some()
        || !is_lower_hex(note_hash, 64)
        || !is_lower_hex(token, 32)
        || !is_lower_hex(revision, 64)
        || !valid_uuid
    {
        return Err(StorageError::new(
            "invalidRecoveryId",
            "恢复草稿标识格式无效。",
        ));
    }
    Ok((note_hash.to_owned(), token.to_owned(), revision.to_owned()))
}

fn parse_draft_filename(file_name: &str) -> Option<String> {
    let token = file_name.strip_prefix("draft-")?.strip_suffix(".cuenote")?;
    if !is_lower_hex(token, 32) {
        return None;
    }
    let uuid = Uuid::parse_str(token).ok()?;
    (uuid.get_version_num() == 4 && uuid.simple().to_string() == token).then(|| token.to_owned())
}

fn recovery_id(note_hash: &str, token: &str, revision: &str) -> String {
    format!("{note_hash}:{token}:{revision}")
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ensure_scan_limit(scanned: usize) -> Result<(), StorageError> {
    if scanned > MAX_RECOVERY_SCAN_ENTRIES {
        Err(StorageError::new(
            "recoveryLimit",
            format!("恢复目录超过单次扫描上限 {MAX_RECOVERY_SCAN_ENTRIES}；没有静默省略其余条目。"),
        ))
    } else {
        Ok(())
    }
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() < MAX_RECOVERY_WARNINGS - 1 {
        warnings.push(warning);
    } else if warnings.len() == MAX_RECOVERY_WARNINGS - 1 {
        warnings.push("其余恢复目录问题未展开；相关文件仍保留。".to_owned());
    }
}

fn remove_owned_candidate(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => Err(
            io::Error::new(io::ErrorKind::InvalidData, "候选路径类型已变化，未删除"),
        ),
        Ok(_) => fs::remove_file(path),
    }
}

fn append_candidate_cleanup(
    mut error: StorageError,
    candidate: &Path,
    cleanup: io::Result<()>,
) -> StorageError {
    if let Err(cleanup_error) = cleanup {
        error.recovery_error = Some(format!(
            "无法清理恢复候选 {}：{cleanup_error}",
            candidate.display()
        ));
    }
    error
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes() & 0x400 != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod copy_target_tests {
    use super::*;
    use crate::note_format::PlainText;

    fn package() -> NotePackage {
        NotePackage::new_plain(
            uuid::Uuid::new_v4().to_string(),
            PlainText {
                title: "retained copy target".to_owned(),
                cue: String::new(),
                content: "copy payload".to_owned(),
                summary: String::new(),
            },
        )
    }

    #[cfg(windows)]
    #[test]
    fn retained_copy_target_blocks_ancestor_rename_and_publishes_safely() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected");
        let moved = root.path().join("selected-moved");
        let nested = selected.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let requested = nested.join("copy.cuenote");
        let target = super::super::resolve_output_path(&requested).unwrap();

        assert!(fs::rename(&selected, &moved).is_err());

        let (saved, revision) = save_copy(&target, &package()).unwrap();
        assert_eq!(saved.file_name(), requested.file_name());
        assert!(nested.join("copy.cuenote").is_file());
        let opened = inspect_recovery_note(&saved).unwrap().unwrap();
        assert_eq!(opened.revision, revision);
        assert!(opened.package.document.title == "retained copy target");
        assert!(!moved.exists());
    }

    #[cfg(unix)]
    #[test]
    fn retained_copy_target_rejects_ancestor_redirect_without_library_publish() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected");
        let moved = root.path().join("selected-moved");
        let library = root.path().join("library");
        let nested = selected.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(library.join("nested")).unwrap();
        let requested = nested.join("copy.cuenote");
        let target = super::super::resolve_output_path(&requested).unwrap();

        fs::rename(&selected, &moved).unwrap();
        std::os::unix::fs::symlink(&library, &selected).unwrap();

        let _error = save_copy(&target, &package()).unwrap_err();
        assert!(!library.join("nested").join("copy.cuenote").exists());
        assert!(!moved.join("nested").join("copy.cuenote").exists());
        assert_eq!(fs::read_dir(library.join("nested")).unwrap().count(), 0);
    }

    #[test]
    fn retained_copy_target_keeps_no_overwrite_behavior() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let requested = nested.join("copy.cuenote");
        let target = super::super::resolve_output_path(&requested).unwrap();
        let foreign_bytes = b"created by another process";
        fs::write(target.path(), foreign_bytes).unwrap();

        let error = save_copy(&target, &package()).unwrap_err();
        assert_eq!(fs::read(target.path()).unwrap(), foreign_bytes);
        assert!(error.recovery_error.is_some());
        let retained = fs::read_dir(&nested)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "tmp"))
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        assert_eq!(
            inspect_recovery_note(&retained[0])
                .unwrap()
                .unwrap()
                .package
                .document
                .title,
            "retained copy target"
        );
    }
}
