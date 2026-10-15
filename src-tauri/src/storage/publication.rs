//! Crash-safe replacement of one complete note file.
//!
//! The publication journal is deliberately small and stays in the independent
//! recovery root. Every path from a journal is revalidated before it is used.

use super::{open_note, preserve_observed_version, OpenedNote, StorageError};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const JOURNAL_DIRECTORY: &str = ".pending-publications";
const MAX_INTENTS: usize = 256;
const MAX_JOURNAL_ENTRIES: usize = MAX_INTENTS * 2;
const MAX_INTENT_BYTES: u64 = 64 * 1024;
const INTENT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    note_id: String,
    target: PathBuf,
    candidate: PathBuf,
    displaced: Option<PathBuf>,
    expected_revision: Option<String>,
    candidate_revision: String,
    directory_identity: String,
}

#[derive(Debug)]
pub(super) struct PublicationReceipt {
    intent_path: PathBuf,
    target: PathBuf,
    candidate: PathBuf,
    displaced: Option<PathBuf>,
    note_id: String,
    expected_revision: Option<String>,
    candidate_revision: String,
    candidate_note: OpenedNote,
    displaced_revision: Option<String>,
}

/// An open handle pins the directory object while a save is in flight.
/// Windows intentionally omits FILE_SHARE_DELETE. macOS uses its descriptor
/// for the relative swap operation and checks that the selected path still
/// names the same directory after publication.
pub(crate) struct StableDirectory {
    path: PathBuf,
    identity: String,
    handles: Vec<File>,
}

impl StableDirectory {
    pub(crate) fn open(path: &Path) -> Result<Self, StorageError> {
        #[cfg(windows)]
        let (handles, identity) = {
            // Hold every directory component without FILE_SHARE_DELETE. This
            // prevents an ancestor rename/replacement from redirecting the
            // path-based ReplaceFileW call during the transaction.
            let mut ancestors = path.ancestors().collect::<Vec<_>>();
            ancestors.reverse();
            let mut handles = Vec::with_capacity(ancestors.len());
            let mut identity = String::new();
            for ancestor in ancestors {
                let (handle, next_identity) = open_directory_handle(ancestor)?;
                handles.push(handle);
                identity = next_identity;
            }
            (handles, identity)
        };
        #[cfg(not(windows))]
        let (handles, identity) = {
            let (handle, identity) = open_directory_handle(path)?;
            (vec![handle], identity)
        };
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            handles,
        })
    }

    pub(crate) fn ensure_current(&self) -> Result<(), StorageError> {
        super::ensure_local_directory(&self.path)?;
        let canonical = fs::canonicalize(&self.path)
            .map_err(|error| StorageError::io("无法验证笔记所在目录是否仍是原目录", error))?;
        if canonical != self.path {
            return Err(StorageError::new(
                "unsafePath",
                "笔记所在目录路径已被替换或重定向；没有继续发布。",
            ));
        }
        let (_, current_identity) = open_directory_handle(&self.path)?;
        if current_identity != self.identity {
            return Err(StorageError::new(
                "unsafePath",
                "笔记所在目录在保存期间已被替换；没有继续发布。",
            ));
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.directory_handle().sync_all()
        }
        #[cfg(windows)]
        {
            // FlushFileBuffers does not provide a portable directory flush on
            // Windows. The journal file itself is flushed before publication.
            let _ = self.directory_handle();
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = self.directory_handle();
            Ok(())
        }
    }

    pub(crate) fn sync_publication_directory(&self) -> io::Result<()> {
        self.ensure_current()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message))?;
        self.sync()
    }

    /// Moves a direct child of this pinned directory without replacing an
    /// existing destination. A post-operation identity error is ambiguous:
    /// callers must retain the destination and source paths for recovery.
    pub(crate) fn move_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.ensure_current()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message))?;
        #[cfg(windows)]
        {
            self.ensure_child_paths(source, destination)?;
            super::move_file_no_replace(source, destination)?;
            return self
                .ensure_current()
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message));
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::io::AsRawFd;
            let source_name = self.child_component(source)?;
            let destination_name = self.child_component(destination)?;
            let result = unsafe {
                renameatx_np(
                    self.directory_handle().as_raw_fd(),
                    source_name.as_ptr(),
                    self.directory_handle().as_raw_fd(),
                    destination_name.as_ptr(),
                    RENAME_EXCL,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            return self
                .ensure_current()
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let source_name = self.child_component(source)?;
            let destination_name = self.child_component(destination)?;
            let result = unsafe {
                renameat2(
                    self.directory_handle().as_raw_fd(),
                    source_name.as_ptr(),
                    self.directory_handle().as_raw_fd(),
                    destination_name.as_ptr(),
                    RENAME_NOREPLACE,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            return self
                .ensure_current()
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message));
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = (source, destination);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "atomic no-replace move is unsupported on this platform",
            ));
        }
    }

    /// Creates a unique candidate as a direct child of the directory retained
    /// by this handle. Unix uses openat so an ancestor rename cannot redirect
    /// candidate creation; Windows keeps its pinned ancestors while using the
    /// existing create-new path helper.
    pub(crate) fn create_candidate(&self) -> Result<(PathBuf, File), StorageError> {
        self.ensure_current()?;
        #[cfg(windows)]
        {
            super::create_candidate_path(&self.path)
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.create_candidate_at()
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            Err(StorageError::new(
                "publish",
                "当前平台不支持在固定目录中安全创建候选文件。",
            ))
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn create_candidate_at(&self) -> Result<(PathBuf, File), StorageError> {
        use std::ffi::CString;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        for _ in 0..4 {
            let file_name = format!(".cuenote-candidate-{}.tmp", Uuid::new_v4().simple());
            let name = CString::new(file_name.as_bytes())
                .expect("candidate filename contains no interior NUL");
            let descriptor = unsafe {
                openat(
                    self.directory_handle().as_raw_fd(),
                    name.as_ptr(),
                    CANDIDATE_OPEN_FLAGS,
                    0o600i32,
                )
            };
            if descriptor >= 0 {
                let file = unsafe { File::from_raw_fd(descriptor) };
                return Ok((self.path.join(file_name), file));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(StorageError::io("无法在固定目录中创建候选笔记", error));
        }
        Err(StorageError::new(
            "io",
            "多次尝试后仍无法创建唯一候选笔记。",
        ))
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn child_component(&self, path: &Path) -> io::Result<std::ffi::CString> {
        if path.parent() != Some(self.path.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "atomic metadata operation requires direct children of the pinned directory",
            ));
        }
        component(path)
    }

    #[cfg(windows)]
    fn ensure_child_paths(&self, source: &Path, destination: &Path) -> io::Result<()> {
        if source.parent() == Some(self.path.as_path())
            && destination.parent() == Some(self.path.as_path())
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "atomic metadata operation requires direct children of the pinned directory",
            ))
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn swap(&self, left: &Path, right: &Path) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        self.ensure_current()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message))?;
        let left = component(left)?;
        let right = component(right)?;
        #[cfg(target_os = "macos")]
        let result = unsafe {
            renameatx_np(
                self.directory_handle().as_raw_fd(),
                left.as_ptr(),
                self.directory_handle().as_raw_fd(),
                right.as_ptr(),
                RENAME_SWAP,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            renameat2(
                self.directory_handle().as_raw_fd(),
                left.as_ptr(),
                self.directory_handle().as_raw_fd(),
                right.as_ptr(),
                RENAME_EXCHANGE,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        self.ensure_current()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.message))
    }

    fn directory_handle(&self) -> &File {
        self.handles
            .last()
            .expect("stable directory owns its opened directory handle")
    }
}

pub(super) fn publish_create(
    recovery_root: &Path,
    directory: &StableDirectory,
    candidate: &Path,
    target: &Path,
    note_id: &str,
    candidate_revision: &str,
) -> Result<PublicationReceipt, StorageError> {
    directory.ensure_current()?;
    let candidate_note = open_note(candidate)?;
    if candidate_note.revision != candidate_revision
        || candidate_note.package.document.note_id != note_id
    {
        return Err(StorageError::new(
            "conflict",
            "待发布候选笔记在发布前发生变化；恢复草稿已保留。",
        ));
    }
    if let Some(observed) = open_optional(target)? {
        let candidate_note = OpenedNote {
            package: candidate_note.package.clone(),
            revision: candidate_note.revision.clone(),
        };
        preserve_conflict_pair(
            recovery_root,
            target,
            Some(&observed),
            &candidate_note,
            "create-target-occupied",
        )?;
        return Err(StorageError::new(
            "conflict",
            "保存前已有其他笔记文件创建；当前文件未被覆盖。",
        ));
    }

    let intent = Intent {
        version: INTENT_VERSION,
        note_id: note_id.to_owned(),
        target: target.to_path_buf(),
        candidate: candidate.to_path_buf(),
        displaced: None,
        expected_revision: None,
        candidate_revision: candidate_revision.to_owned(),
        directory_identity: directory.identity.clone(),
    };
    let intent_path = persist_intent(recovery_root, &intent)?;

    directory.ensure_current()?;
    #[cfg(windows)]
    let publish_result = super::move_file_no_replace(candidate, target);
    #[cfg(unix)]
    let publish_result = fs::hard_link(candidate, target);
    #[cfg(not(any(windows, unix)))]
    let publish_result = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "无覆盖原子创建不受支持",
    ));

    if let Err(error) = publish_result {
        if error.kind() == io::ErrorKind::AlreadyExists
            || matches!(error.raw_os_error(), Some(80 | 183))
        {
            if let Some(observed) = open_optional(target)? {
                let candidate_note = OpenedNote {
                    package: candidate_note.package.clone(),
                    revision: candidate_note.revision.clone(),
                };
                preserve_conflict_pair(
                    recovery_root,
                    target,
                    Some(&observed),
                    &candidate_note,
                    "create-race",
                )?;
            }
            return Err(StorageError::new(
                "conflict",
                "保存前已有其他笔记文件创建；外部版本和恢复草稿均已保留。",
            ));
        } else {
            return Err(StorageError::io("无法无覆盖原子创建笔记文件", error));
        }
    }

    // This is the first operation after the OS publish primitive. In
    // particular, no candidate, backup, or journal cleanup precedes it.
    crate::storage_faults::checkpoint("publish_os_returned", target, Some(candidate))?;
    directory.ensure_current()?;
    let published = open_note(target)?;
    if published.revision != candidate_revision || published.package.document.note_id != note_id {
        preserve_conflict_pair(
            recovery_root,
            target,
            Some(&published),
            &candidate_note,
            "create-after-publish-race",
        )?;
        return Err(StorageError::new(
            "conflict",
            "原子创建返回后目标内容已变化；候选与恢复草稿均已保留。",
        ));
    }

    Ok(PublicationReceipt {
        intent_path,
        target: target.to_path_buf(),
        candidate: candidate.to_path_buf(),
        displaced: None,
        note_id: note_id.to_owned(),
        expected_revision: None,
        candidate_revision: candidate_revision.to_owned(),
        candidate_note,
        displaced_revision: None,
    })
}

pub(super) fn publish_replace(
    recovery_root: &Path,
    directory: &StableDirectory,
    candidate: &Path,
    target: &Path,
    note_id: &str,
    expected_revision: &str,
    candidate_revision: &str,
) -> Result<PublicationReceipt, StorageError> {
    directory.ensure_current()?;
    let candidate_note = open_note(candidate)?;
    if candidate_note.revision != candidate_revision
        || candidate_note.package.document.note_id != note_id
    {
        return Err(StorageError::new(
            "conflict",
            "待发布候选笔记在发布前发生变化；恢复草稿已保留。",
        ));
    }

    let current = match open_optional(target)? {
        Some(current) => current,
        None => {
            preserve_conflict_version_for(
                recovery_root,
                target,
                &candidate_note,
                "publish-target-missing",
            )?;
            return Err(StorageError::new(
                "conflict",
                "笔记在最终发布检查前已被删除；恢复草稿已保留。",
            ));
        }
    };
    if current.package.document.note_id != note_id || current.revision != expected_revision {
        let candidate_note = OpenedNote {
            package: candidate_note.package.clone(),
            revision: candidate_note.revision.clone(),
        };
        preserve_conflict_pair(
            recovery_root,
            target,
            Some(&current),
            &candidate_note,
            "publish-before-check",
        )?;
        return Err(StorageError::new(
            "conflict",
            "笔记在发布前已由其他写入者修改；外部版本和恢复草稿均已保留。",
        ));
    }

    #[cfg(windows)]
    let displaced = target.with_file_name(format!(
        ".cuenote-publish-backup-{}.tmp",
        Uuid::new_v4().simple()
    ));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let displaced = candidate.to_path_buf();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    return Err(StorageError::new(
        "publish",
        "当前平台没有实现可保留被替换版本的原子发布。",
    ));

    if displaced.as_path() != candidate {
        match fs::symlink_metadata(&displaced) {
            Ok(_) => {
                return Err(StorageError::new(
                    "conflict",
                    format!(
                        "原子替换备份路径已被占用；未发布候选：{}",
                        displaced.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::io("无法确认原子替换备份路径可用", error));
            }
        }
    }

    let intent = Intent {
        version: INTENT_VERSION,
        note_id: note_id.to_owned(),
        target: target.to_path_buf(),
        candidate: candidate.to_path_buf(),
        displaced: Some(displaced.clone()),
        expected_revision: Some(expected_revision.to_owned()),
        candidate_revision: candidate_revision.to_owned(),
        directory_identity: directory.identity.clone(),
    };
    let intent_path = persist_intent(recovery_root, &intent)?;

    directory.ensure_current()?;
    #[cfg(windows)]
    let publish_result = super::replace_windows(target, candidate, &displaced);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let publish_result = directory.swap(candidate, target);

    if let Err(error) = publish_result {
        // ReplaceFileW can report failure after changing one or more names.
        // Preserve readable evidence but leave every path and the intent in
        // place for restart recovery; never try a blind rollback.
        let candidate_note = OpenedNote {
            package: candidate_note.package.clone(),
            revision: candidate_note.revision.clone(),
        };
        let evidence_result = (|| {
            preserve_conflict_version_for(
                recovery_root,
                target,
                &candidate_note,
                "publish-os-error",
            )?;
            if let Some(observed) = open_optional(&displaced)? {
                preserve_displaced(recovery_root, target, &observed, note_id, expected_revision)?;
            }
            if let Some(observed) = open_optional(target)? {
                preserve_conflict_version_for(
                    recovery_root,
                    target,
                    &observed,
                    "publish-os-error-target",
                )?;
            }
            Ok::<(), StorageError>(())
        })();
        let mut failure = StorageError::new(
            "publish",
            format!("无法确认笔记原子发布结果：{error}；候选、备份和恢复意图已保留。"),
        );
        if let Err(evidence_error) = evidence_result {
            failure.recovery_error = Some(format!(
                "冲突版本副本未能完整写入：{}",
                evidence_error.message
            ));
        }
        return Err(failure);
    }

    // This checkpoint must remain directly after ReplaceFileW/RENAME_SWAP.
    crate::storage_faults::checkpoint("publish_os_returned", target, Some(candidate))?;
    directory.ensure_current()?;

    let published = open_optional(target)?.ok_or_else(|| {
        StorageError::new(
            "publish",
            "原子替换已返回，但目标路径缺失；备份与恢复意图已保留。",
        )
    })?;
    if published.package.document.note_id != note_id || published.revision != candidate_revision {
        let candidate_note = OpenedNote {
            package: candidate_note.package.clone(),
            revision: candidate_note.revision.clone(),
        };
        preserve_conflict_pair(
            recovery_root,
            target,
            Some(&published),
            &candidate_note,
            "publish-after-check",
        )?;
        return Err(StorageError::new(
            "conflict",
            "原子替换已返回，但目标随后发生变化；观察到的版本和恢复草稿均已保留。",
        ));
    }

    let displaced_note = open_optional(&displaced)?.ok_or_else(|| {
        StorageError::new(
            "publish",
            format!(
                "原子替换已返回，但没有可验证的被替换文件；恢复意图保留于 {}。",
                intent_path.display()
            ),
        )
    })?;
    if displaced_note.package.document.note_id != note_id {
        preserve_as_conflict(
            recovery_root,
            target,
            &displaced_note,
            "publish-displaced",
            "原子替换移出的有效笔记身份与目标路径身份不同。",
        )?;
        return Err(StorageError::new(
            "conflict",
            "原子替换移出了不同身份的笔记；该版本已保留，保存未确认。",
        ));
    }

    preserve_displaced(
        recovery_root,
        target,
        &displaced_note,
        note_id,
        expected_revision,
    )?;
    if displaced_note.revision != expected_revision {
        preserve_as_conflict(
            recovery_root,
            target,
            &published,
            "publish-candidate-raced",
            "并发写入者的版本被原子替换捕获；已保留当前候选内容。",
        )?;
        return Err(StorageError::new(
            "conflict",
            "原子替换捕获到发布前基线以外的有效版本；冲突副本与本地恢复草稿均已保留。",
        ));
    }

    Ok(PublicationReceipt {
        intent_path,
        target: target.to_path_buf(),
        candidate: candidate.to_path_buf(),
        displaced: Some(displaced),
        note_id: note_id.to_owned(),
        expected_revision: Some(expected_revision.to_owned()),
        candidate_revision: candidate_revision.to_owned(),
        candidate_note,
        displaced_revision: Some(displaced_note.revision),
    })
}

pub(super) fn complete_publication(
    recovery_root: &Path,
    directory: &StableDirectory,
    receipt: &PublicationReceipt,
) -> Result<(), StorageError> {
    directory.ensure_current()?;
    let published = match open_optional(&receipt.target)? {
        Some(published) => published,
        None => {
            preserve_conflict_version_for(
                recovery_root,
                &receipt.target,
                &receipt.candidate_note,
                "publication-target-missing",
            )?;
            return Err(StorageError::new(
                "conflict",
                "发布后目标文件已消失；候选版本和恢复意图保留。",
            ));
        }
    };
    if published.package.document.note_id != receipt.note_id
        || published.revision != receipt.candidate_revision
    {
        preserve_conflict_pair(
            recovery_root,
            &receipt.target,
            Some(&published),
            &receipt.candidate_note,
            "publication-final-check",
        )?;
        return Err(StorageError::new(
            "conflict",
            "发布后的目标内容已变化；没有清理任何其他版本或恢复意图。",
        ));
    }

    if let Some(displaced) = &receipt.displaced {
        let old = open_optional(displaced)?.ok_or_else(|| {
            StorageError::new(
                "publish",
                format!(
                    "被替换版本已不可读取；恢复意图保留于 {}。",
                    receipt.intent_path.display()
                ),
            )
        })?;
        if old.package.document.note_id != receipt.note_id
            || Some(old.revision.as_str()) != receipt.displaced_revision.as_deref()
            || Some(old.revision.as_str()) != receipt.expected_revision.as_deref()
        {
            preserve_as_conflict(
                recovery_root,
                &receipt.target,
                &old,
                "publication-cleanup-check",
                "待清理的原子替换备份已变化；已额外保留其当前版本。",
            )?;
            preserve_conflict_version_for(
                recovery_root,
                &receipt.target,
                &receipt.candidate_note,
                "publication-cleanup-check",
            )?;
            return Err(StorageError::new(
                "conflict",
                "待清理的替换备份已变化；没有删除新的文件内容。",
            ));
        }
        remove_checked_note(displaced, &old)?;
    } else if let Some(candidate) = open_optional(&receipt.candidate)? {
        if candidate.package.document.note_id != receipt.note_id
            || candidate.revision != receipt.candidate_revision
        {
            preserve_as_conflict(
                recovery_root,
                &receipt.target,
                &candidate,
                "publication-candidate-cleanup-check",
                "候选路径在确认保存后已被替换。",
            )?;
            return Err(StorageError::new(
                "conflict",
                "候选路径在确认保存后已变化；未删除该路径。",
            ));
        }
        remove_checked_note(&receipt.candidate, &candidate)?;
    }

    directory
        .sync()
        .map_err(|error| StorageError::io("无法同步原子发布后的笔记目录", error))?;
    remove_intent(&receipt.intent_path)?;
    Ok(())
}

/// Reconciles journals left by process interruption. Any record or path that
/// cannot be tied to the recorded directory and revisions remains untouched.
pub(crate) fn recover_pending_publications(
    recovery_root: &Path,
) -> Result<Vec<String>, StorageError> {
    match fs::symlink_metadata(recovery_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StorageError::io("无法检查本机恢复目录", error)),
        Ok(metadata) if super::is_link_or_reparse(&metadata) || !metadata.is_dir() => {
            return Err(StorageError::new(
                "unsafePath",
                "本机恢复目录必须是实际目录。",
            ));
        }
        Ok(_) => {}
    }
    let recovery_root = canonical_recovery_root(recovery_root)?;
    let journal = recovery_root.join(JOURNAL_DIRECTORY);
    match fs::symlink_metadata(&journal) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StorageError::io("无法检查待恢复发布目录", error)),
        Ok(metadata) if super::is_link_or_reparse(&metadata) || !metadata.is_dir() => {
            return Err(StorageError::new(
                "unsafePath",
                "待恢复发布路径不是实际目录；没有读取或修改其中的路径。",
            ));
        }
        Ok(_) => {}
    }

    let mut records = Vec::new();
    let mut scanned = 0usize;
    for entry in
        fs::read_dir(&journal).map_err(|error| StorageError::io("无法列出待恢复发布记录", error))?
    {
        let entry = entry.map_err(|error| StorageError::io("无法读取待恢复发布目录项", error))?;
        scanned += 1;
        if scanned > MAX_JOURNAL_ENTRIES {
            return Err(StorageError::new(
                "recoveryLimit",
                "待恢复发布目录项超过安全上限；没有处理或删除记录。",
            ));
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| StorageError::io("无法检查待恢复发布记录", error))?;
        if super::is_link_or_reparse(&metadata) || !metadata.is_file() {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            records.push(path);
            if records.len() > MAX_INTENTS {
                return Err(StorageError::new(
                    "recoveryLimit",
                    "待恢复发布记录超过安全上限；没有处理或删除记录。",
                ));
            }
        }
    }
    records.sort();

    let mut warnings = Vec::new();
    for path in records {
        match recover_intent_if_owned(&recovery_root, &path, false) {
            Ok(Some(message)) => warnings.push(message),
            Ok(None) => {}
            Err(error) => warnings.push(format!(
                "待恢复发布记录 {} 保留待人工处理：{}",
                path.display(),
                error.message
            )),
        }
    }
    Ok(warnings)
}

/// Settle this library's earlier single-note publications before a whole-library
/// plan is captured. The caller holds the exclusive library lease. Records with
/// unknown ownership remain blocking; no path in an invalid record is mutated.
pub(crate) fn ensure_no_pending_publications(
    recovery_root: &Path,
    library: &Path,
) -> Result<(), StorageError> {
    let journal = recovery_root.join(JOURNAL_DIRECTORY);
    match fs::symlink_metadata(&journal) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StorageError::io("无法检查已有单篇发布记录", error)),
        Ok(metadata) if super::is_link_or_reparse(&metadata) || !metadata.is_dir() => {
            return Err(StorageError::new(
                "unsafePath",
                "单篇发布记录目录不安全，不能开始整库恢复。",
            ));
        }
        Ok(_) => {}
    }
    let recovery_root = canonical_recovery_root(recovery_root)?;
    let library = fs::canonicalize(library)
        .map_err(|error| StorageError::io("无法解析待恢复笔记库", error))?;
    let mut records = Vec::new();
    for (index, entry) in fs::read_dir(&journal)
        .map_err(|error| StorageError::io("无法列出已有单篇发布记录", error))?
        .enumerate()
    {
        if index >= MAX_JOURNAL_ENTRIES {
            return Err(StorageError::new(
                "recoveryLimit",
                "已有单篇发布记录过多，不能开始整库恢复。",
            ));
        }
        let path = entry
            .map_err(|error| StorageError::io("无法读取已有单篇发布记录", error))?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let intent = read_intent(&path)?.ok_or_else(|| {
            StorageError::new(
                "publicationPending",
                "存在无法确认归属的单篇发布记录，请先处理后再恢复整库。",
            )
        })?;
        validate_intent(&recovery_root, &path, &intent)?;
        if intent.target.starts_with(&library) {
            records.push(path);
        }
    }
    for path in records {
        recover_intent_if_owned(&recovery_root, &path, true)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::io("无法复核单篇发布记录", error)),
            Ok(_) => {
                return Err(StorageError::new(
                    "publicationPending",
                    format!(
                        "此笔记库仍有未解决的单篇发布记录，未开始整库恢复：{}",
                        path.display()
                    ),
                ))
            }
        }
    }
    Ok(())
}

fn recover_intent_if_owned(
    recovery_root: &Path,
    intent_path: &Path,
    library_exclusive: bool,
) -> Result<Option<String>, StorageError> {
    let Some(observed_intent) = read_intent(intent_path)? else {
        return Ok(None);
    };
    validate_intent(recovery_root, intent_path, &observed_intent)?;

    // A whole-library journal owns any intermediate files in notes/. Never
    // replay an older note intent into that transaction during app startup.
    let mut _library_access = None;
    if let Some(parent) = observed_intent.target.parent() {
        if parent.file_name().and_then(|name| name.to_str()) == Some("notes") {
            if let Some(library) = parent.parent() {
                super::library_access::ensure_no_restore_pending(recovery_root, library)?;
                if !library_exclusive {
                    _library_access =
                        Some(super::library_access::lock_shared(recovery_root, library)?);
                }
            }
        }
    }

    let _lock = match super::try_acquire_save_lock(recovery_root, &observed_intent.target)
        .map_err(|error| StorageError::io("无法协调待恢复的笔记发布", error))?
    {
        Some(lock) => lock,
        None => return Ok(None),
    };

    // A live writer may have completed while recovery was waiting for the
    // same note lock. Its journal is then gone and needs no recovery.
    let Some(intent) = read_intent(intent_path)? else {
        return Ok(None);
    };
    validate_intent(recovery_root, intent_path, &intent)?;
    if intent.target != observed_intent.target || intent.note_id != observed_intent.note_id {
        return Err(StorageError::new(
            "conflict",
            "发布恢复记录在取得笔记锁期间发生变化；原记录已保留。",
        ));
    }
    recover_intent(recovery_root, intent_path, &intent)
}

fn recover_intent(
    recovery_root: &Path,
    intent_path: &Path,
    intent: &Intent,
) -> Result<Option<String>, StorageError> {
    let parent = intent.target.parent().expect("validated target parent");
    let directory = StableDirectory::open(parent)?;
    if directory.identity != intent.directory_identity {
        return Err(StorageError::new(
            "unsafePath",
            "发布记录所指目录身份已变化；目标、候选和备份均未修改。",
        ));
    }

    let target = open_optional(&intent.target)?;
    let candidate = open_optional(&intent.candidate)?;
    let displaced = intent
        .displaced
        .as_ref()
        .map(|path| open_optional(path))
        .transpose()?
        .flatten();

    if intent.expected_revision.is_none()
        && target.is_none()
        && candidate.as_ref().is_some_and(|note| {
            note.package.document.note_id == intent.note_id
                && note.revision == intent.candidate_revision
        })
    {
        directory.ensure_current()?;
        if open_optional(&intent.target)?.is_some() {
            return Err(StorageError::new(
                "conflict",
                "恢复未完成创建时目标路径已被占用；候选和发布记录均保留。",
            ));
        }
        remove_checked_note(
            &intent.candidate,
            candidate.as_ref().expect("candidate was checked"),
        )?;
        directory
            .sync()
            .map_err(|error| StorageError::io("无法同步未完成创建的候选清理", error))?;
        remove_intent(intent_path)?;
        return Ok(None);
    }

    if let Some(published) = target.as_ref().filter(|note| {
        note.package.document.note_id == intent.note_id
            && note.revision == intent.candidate_revision
    }) {
        preserve_observed_version(
            recovery_root,
            published,
            &intent.target.to_string_lossy(),
            "recovered-publication",
        )?;

        if let Some(displaced_note) = displaced.as_ref() {
            if intent.expected_revision.as_deref() != Some(displaced_note.revision.as_str()) {
                preserve_as_conflict(
                    recovery_root,
                    &intent.target,
                    published,
                    "recovered-raced-publication",
                    "发布恢复发现基线之后又有版本被替换。",
                )?;
            }
            preserve_displaced(
                recovery_root,
                &intent.target,
                displaced_note,
                &intent.note_id,
                intent.expected_revision.as_deref().unwrap_or_default(),
            )?;
            directory.ensure_current()?;
            let latest_target = open_optional(&intent.target)?.ok_or_else(|| {
                StorageError::new("conflict", "恢复发布期间目标文件已消失；所有路径均保留。")
            })?;
            let latest_displaced = open_optional(
                intent
                    .displaced
                    .as_ref()
                    .expect("displaced note has a path"),
            )?
            .ok_or_else(|| {
                StorageError::new("conflict", "恢复发布期间备份文件已变化；发布记录保留。")
            })?;
            if latest_target.revision != intent.candidate_revision
                || latest_displaced.revision != displaced_note.revision
                || latest_displaced.package.document.note_id != intent.note_id
            {
                return Err(StorageError::new(
                    "conflict",
                    "恢复期间目标或备份再次变化；已保留可读取版本，没有清理路径。",
                ));
            }
            remove_checked_note(
                intent
                    .displaced
                    .as_ref()
                    .expect("displaced note has a path"),
                &latest_displaced,
            )?;
        } else if intent.expected_revision.is_some() {
            return Err(StorageError::new(
                "publish",
                "替换目标已是候选版本，但发布记录中的旧版备份缺失；记录保留。",
            ));
        }

        if let Some(candidate_note) = candidate.as_ref() {
            // On Unix create uses a hard link and leaves the candidate name
            // until completion. For replacement on swap platforms this is the
            // displaced file and was already handled above.
            if intent.displaced.is_none()
                && candidate_note.revision == intent.candidate_revision
                && candidate_note.package.document.note_id == intent.note_id
            {
                remove_checked_note(&intent.candidate, candidate_note)?;
            }
        }
        directory
            .sync()
            .map_err(|error| StorageError::io("无法同步恢复后的笔记目录", error))?;
        remove_intent(intent_path)?;
        return Ok(Some(format!(
            "已恢复笔记 {} 的原子发布记录；发布前后版本均已保存。",
            intent.note_id
        )));
    }

    if target
        .as_ref()
        .is_some_and(|note| Some(note.revision.as_str()) == intent.expected_revision.as_deref())
        && candidate.as_ref().is_some_and(|note| {
            note.package.document.note_id == intent.note_id
                && note.revision == intent.candidate_revision
        })
        && (intent.displaced.as_ref() == Some(&intent.candidate) || displaced.is_none())
    {
        // The journal reached disk before the OS operation. The target is
        // still the exact baseline, so the candidate was not published.
        let candidate_note = candidate.as_ref().expect("candidate was checked");
        remove_checked_note(&intent.candidate, candidate_note)?;
        directory
            .sync()
            .map_err(|error| StorageError::io("无法同步未完成发布的候选清理", error))?;
        remove_intent(intent_path)?;
        return Ok(None);
    }

    if let Some(observed) = target.as_ref() {
        if observed.package.document.note_id == intent.note_id {
            preserve_as_conflict(
                recovery_root,
                &intent.target,
                observed,
                "pending-publication-target",
                "重启时发现目标与发布记录的候选及基线均不一致。",
            )?;
        }
    }
    if let Some(observed) = displaced.as_ref() {
        preserve_displaced(
            recovery_root,
            &intent.target,
            observed,
            &intent.note_id,
            intent.expected_revision.as_deref().unwrap_or_default(),
        )?;
    }
    if let Some(observed) = candidate.as_ref() {
        if observed.package.document.note_id == intent.note_id
            && observed.revision != intent.candidate_revision
        {
            preserve_as_conflict(
                recovery_root,
                &intent.target,
                observed,
                "pending-publication-candidate",
                "重启时发现候选路径内容与发布记录不一致。",
            )?;
        }
    }
    Err(StorageError::new(
        "publish",
        "发布记录中的目标、候选或备份状态不确定；没有移动或删除任何文件。",
    ))
}

fn preserve_displaced(
    recovery_root: &Path,
    target_origin: &Path,
    opened: &OpenedNote,
    note_id: &str,
    expected_revision: &str,
) -> Result<(), StorageError> {
    if opened.package.document.note_id == note_id && opened.revision == expected_revision {
        preserve_observed_version(
            recovery_root,
            opened,
            &target_origin.to_string_lossy(),
            "publication-baseline",
        )
    } else {
        preserve_as_conflict(
            recovery_root,
            target_origin,
            opened,
            "publication-displaced",
            "原子发布移出了一个与已确认基线不同的有效版本。",
        )
    }
}

fn preserve_as_conflict(
    recovery_root: &Path,
    origin: &Path,
    opened: &OpenedNote,
    reason: &str,
    detail: &str,
) -> Result<(), StorageError> {
    preserve_conflict_version_for(
        recovery_root,
        origin,
        opened,
        &format!("{reason}: {detail}"),
    )
}

fn preserve_conflict_version_for(
    recovery_root: &Path,
    origin: &Path,
    opened: &OpenedNote,
    reason: &str,
) -> Result<(), StorageError> {
    super::preserve_conflict_version(recovery_root, opened, &origin.to_string_lossy(), reason)
}

pub(super) fn preserve_conflict_pair(
    recovery_root: &Path,
    origin: &Path,
    observed: Option<&OpenedNote>,
    candidate: &OpenedNote,
    reason: &str,
) -> Result<(), StorageError> {
    if let Some(observed) = observed {
        preserve_conflict_version_for(recovery_root, origin, observed, reason)?;
    }
    preserve_conflict_version_for(recovery_root, origin, candidate, reason)
}

fn open_optional(path: &Path) -> Result<Option<OpenedNote>, StorageError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::io("无法检查发布路径", error)),
        Ok(_) => open_note(path).map(Some),
    }
}

fn persist_intent(recovery_root: &Path, intent: &Intent) -> Result<PathBuf, StorageError> {
    let recovery_root = canonical_recovery_root(recovery_root)?;
    let journal = recovery_root.join(JOURNAL_DIRECTORY);
    match fs::create_dir(&journal) {
        Ok(()) => {
            super::sync_parent_directory(&recovery_root)
                .map_err(|error| StorageError::io("无法同步发布恢复目录", error))?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(StorageError::io("无法创建发布恢复目录", error)),
    }
    let metadata = fs::symlink_metadata(&journal)
        .map_err(|error| StorageError::io("无法检查发布恢复目录", error))?;
    if super::is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "发布恢复路径不是实际目录。",
        ));
    }
    let mut entries = 0usize;
    let mut intents = 0usize;
    for entry in fs::read_dir(&journal)
        .map_err(|error| StorageError::io("无法统计现有发布恢复记录", error))?
    {
        let entry = entry.map_err(|error| StorageError::io("无法统计发布恢复目录项", error))?;
        entries += 1;
        if entries > MAX_JOURNAL_ENTRIES {
            return Err(StorageError::new(
                "recoveryLimit",
                "待恢复发布目录项超过安全上限；没有创建新发布记录。",
            ));
        }
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("json")
        {
            intents += 1;
        }
    }
    if intents >= MAX_INTENTS {
        return Err(StorageError::new(
            "recoveryLimit",
            "待恢复发布记录已达到安全上限；没有创建新发布记录。",
        ));
    }

    let path = journal.join(format!("{}.json", Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec(intent)
        .map_err(|error| StorageError::new("publish", error.to_string()))?;
    if bytes.len() as u64 > MAX_INTENT_BYTES {
        return Err(StorageError::new(
            "publish",
            "发布恢复记录超过安全大小上限。",
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| StorageError::io("无法创建发布恢复记录", error))?;
    file.write_all(&bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|error| StorageError::io("无法持久化发布恢复记录", error))?;
    super::sync_parent_directory(&journal)
        .map_err(|error| StorageError::io("无法同步发布恢复记录目录", error))?;
    Ok(path)
}

fn read_intent(path: &Path) -> Result<Option<Intent>, StorageError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StorageError::io("无法检查发布恢复记录", error)),
    };
    if super::is_link_or_reparse(&metadata)
        || !metadata.is_file()
        || metadata.len() > MAX_INTENT_BYTES
    {
        return Err(StorageError::new(
            "unsafePath",
            "发布恢复记录类型或大小不受支持；原记录已保留。",
        ));
    }
    let file = match super::open_read_nofollow(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(path)
                    .is_err_and(|current| current.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(StorageError::io("无法打开发布恢复记录", error)),
    };
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_INTENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::io("无法读取发布恢复记录", error))?;
    if bytes.len() as u64 > MAX_INTENT_BYTES {
        return Err(StorageError::new(
            "recoveryLimit",
            "发布恢复记录超过安全大小上限；原记录已保留。",
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| StorageError::new("publish", format!("发布恢复记录无效：{error}")))
}

fn validate_intent(
    recovery_root: &Path,
    intent_path: &Path,
    intent: &Intent,
) -> Result<(), StorageError> {
    if intent.version != INTENT_VERSION || !valid_note_id(&intent.note_id) {
        return Err(StorageError::new(
            "publish",
            "发布恢复记录版本或笔记身份无效；原记录已保留。",
        ));
    }
    if !valid_revision(&intent.candidate_revision)
        || intent
            .expected_revision
            .as_deref()
            .is_some_and(|revision| !valid_revision(revision))
    {
        return Err(StorageError::new(
            "publish",
            "发布恢复记录中的版本指纹无效；原记录已保留。",
        ));
    }
    if intent.directory_identity.is_empty() || intent.directory_identity.len() > 256 {
        return Err(StorageError::new(
            "publish",
            "发布恢复记录中的目录身份无效；原记录已保留。",
        ));
    }
    let parent = intent
        .target
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "发布恢复目标缺少父目录；原记录已保留。"))?;
    let target_name = format!("{}.cuenote", intent.note_id);
    let journal_directory = recovery_root.join(JOURNAL_DIRECTORY);
    let record_name = intent_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".json"));
    if !intent.target.is_absolute()
        || intent.target.file_name().and_then(|name| name.to_str()) != Some(target_name.as_str())
        || intent.candidate.parent() != Some(parent)
        || !is_candidate_name(&intent.candidate)
        || intent_path.parent() != Some(journal_directory.as_path())
        || !record_name.is_some_and(|name| is_lower_hex(name, 32))
    {
        return Err(StorageError::new(
            "unsafePath",
            "发布恢复记录中的目标或候选路径不符合安全边界；原记录已保留。",
        ));
    }
    match (&intent.expected_revision, &intent.displaced) {
        (None, None) => {}
        (Some(_), Some(displaced))
            if displaced.parent() == Some(parent)
                && (displaced == &intent.candidate || is_backup_name(displaced)) => {}
        _ => {
            return Err(StorageError::new(
                "unsafePath",
                "发布恢复记录中的备份路径不符合安全边界；原记录已保留。",
            ));
        }
    }
    Ok(())
}

fn canonical_recovery_root(path: &Path) -> Result<PathBuf, StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::io("无法检查本机恢复目录", error))?;
    if super::is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "本机恢复目录必须是实际目录。",
        ));
    }
    fs::canonicalize(path).map_err(|error| StorageError::io("无法解析本机恢复目录", error))
}

fn remove_intent(path: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::io("无法检查待清理发布记录", error))?;
    if super::is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            "待清理发布记录路径类型已变化；没有删除。",
        ));
    }
    fs::remove_file(path).map_err(|error| StorageError::io("无法清理已完成发布记录", error))?;
    if let Some(parent) = path.parent() {
        super::sync_parent_directory(parent)
            .map_err(|error| StorageError::io("无法同步已清理的发布记录", error))?;
    }
    Ok(())
}

fn remove_checked_note(path: &Path, expected: &OpenedNote) -> Result<(), StorageError> {
    let latest = open_optional(path)?
        .ok_or_else(|| StorageError::new("conflict", "待清理文件已消失；没有删除其他路径。"))?;
    if latest.package.document.note_id != expected.package.document.note_id
        || latest.revision != expected.revision
    {
        return Err(StorageError::new(
            "conflict",
            "待清理文件内容已变化；新版本已保留在原路径。",
        ));
    }
    fs::remove_file(path).map_err(|error| StorageError::io("无法清理已验证发布文件", error))?;
    if let Some(parent) = path.parent() {
        super::sync_parent_directory(parent)
            .map_err(|error| StorageError::io("无法同步发布文件清理", error))?;
    }
    Ok(())
}

fn valid_note_id(note_id: &str) -> bool {
    Uuid::parse_str(note_id)
        .is_ok_and(|parsed| parsed.get_version_num() == 4 && parsed.to_string() == note_id)
}

fn valid_revision(revision: &str) -> bool {
    revision.len() == 64
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_candidate_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(".cuenote-candidate-"))
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|name| is_lower_hex(name, 32))
}

fn is_backup_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(".cuenote-publish-backup-"))
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|name| is_lower_hex(name, 32))
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn component(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "路径缺少文件名"))?;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL 字节"))
}

#[cfg(target_os = "macos")]
const RENAME_SWAP: u32 = 0x0000_0002;
#[cfg(target_os = "linux")]
const RENAME_EXCHANGE: u32 = 0x0000_0002;
#[cfg(target_os = "macos")]
const RENAME_EXCL: u32 = 0x0000_0004;
#[cfg(target_os = "linux")]
const RENAME_NOREPLACE: u32 = 0x0000_0001;
// These open flags match the Linux and Darwin values in the installed libc
// platform definitions; keeping them local avoids adding a production crate.
#[cfg(target_os = "linux")]
const CANDIDATE_OPEN_FLAGS: i32 =
    0x0000_0002 | 0x0000_0040 | 0x0000_0080 | 0x0002_0000 | 0x0008_0000;
#[cfg(target_os = "macos")]
const CANDIDATE_OPEN_FLAGS: i32 =
    0x0000_0002 | 0x0000_0200 | 0x0000_0800 | 0x0000_0100 | 0x0100_0000;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn openat(directory: i32, path: *const std::ffi::c_char, flags: i32, ...) -> i32;
    fn renameatx_np(
        from_directory: i32,
        from_path: *const std::ffi::c_char,
        to_directory: i32,
        to_path: *const std::ffi::c_char,
        flags: u32,
    ) -> i32;
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn openat(directory: i32, path: *const std::ffi::c_char, flags: i32, ...) -> i32;
    fn renameat2(
        from_directory: i32,
        from_path: *const std::ffi::c_char,
        to_directory: i32,
        to_path: *const std::ffi::c_char,
        flags: u32,
    ) -> i32;
}

#[cfg(windows)]
fn open_directory_handle(path: &Path) -> Result<(File, String), StorageError> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation: FileTime,
        last_access: FileTime,
        last_write: FileTime,
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut std::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE, // deliberately no FILE_SHARE_DELETE
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw == (-1isize) as *mut std::ffi::c_void {
        return Err(StorageError::io(
            "无法固定笔记所在目录",
            io::Error::last_os_error(),
        ));
    }
    let handle = unsafe { File::from_raw_handle(raw as _) };
    let mut info = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle() as _, info.as_mut_ptr()) } == 0 {
        return Err(StorageError::io(
            "无法读取笔记目录身份",
            io::Error::last_os_error(),
        ));
    }
    let info = unsafe { info.assume_init() };
    if info.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(StorageError::new(
            "unsafePath",
            "笔记所在路径不是实际目录。",
        ));
    }
    // Ancestors may be verified Cloud Files directories without every sibling
    // being downloaded. The target directory itself is checked before access.
    super::local_files::check_windows_handle(&handle, false)?;
    let identity = format!(
        "windows:{:08x}:{:08x}:{:08x}",
        info.volume_serial, info.index_high, info.index_low
    );
    Ok((handle, identity))
}

#[cfg(unix)]
fn open_directory_handle(path: &Path) -> Result<(File, String), StorageError> {
    use std::os::unix::fs::MetadataExt;
    let handle =
        File::open(path).map_err(|error| StorageError::io("无法打开笔记所在目录", error))?;
    let metadata = handle
        .metadata()
        .map_err(|error| StorageError::io("无法读取笔记目录身份", error))?;
    if !metadata.is_dir() {
        return Err(StorageError::new("unsafePath", "笔记所在路径不是目录。"));
    }
    Ok((
        handle,
        format!("unix:{}:{}", metadata.dev(), metadata.ino()),
    ))
}

#[cfg(not(any(unix, windows)))]
fn open_directory_handle(path: &Path) -> Result<(File, String), StorageError> {
    let handle =
        File::open(path).map_err(|error| StorageError::io("无法打开笔记所在目录", error))?;
    if !handle
        .metadata()
        .map_err(|error| StorageError::io("无法读取笔记目录身份", error))?
        .is_dir()
    {
        return Err(StorageError::new("unsafePath", "笔记所在路径不是目录。"));
    }
    Ok((handle, format!("other:{}", path.display())))
}
