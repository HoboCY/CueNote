//! Safe, single-note restore from an already verified immutable backup.
//!
//! The plan types keep backup provenance and pre-restore protection private so
//! callers cannot construct a restore request from arbitrary paths or bytes.

use super::backups::{BackupContext, BackupJob, BackupKind, BackupSummary, VerifiedBackupNote};
use super::{
    display_path, ensure_child_directory, ensure_notebook_baseline_current, is_canonical_uuid_v4,
    next_timestamp_after, secure_read_dir, LibraryView, NoteSession, NotebookMetadata,
    StorageError, Workspace, WorkspaceError, MAX_METADATA_BYTES, MAX_NOTEBOOKS, MAX_NOTE_FILES,
    NOTEBOOK_CONFLICT_PREFIX,
};
use crate::storage;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupRestoreReceipt {
    pub session: NoteSession,
    pub library: LibraryView,
    pub pre_restore_backup: BackupSummary,
    pub reassigned_notebook: bool,
    pub message: String,
    pub warnings: Vec<String>,
}

pub struct PreparedBackupNoteRestore {
    context: BackupContext,
    source: VerifiedBackupNote,
    library_path: PathBuf,
    target_notebook_id: String,
    reassigned_notebook: bool,
    target_revision: Option<String>,
    notebook_metadata: Vec<u8>,
    session: Option<RestoreSessionBaseline>,
}

#[derive(Clone, PartialEq, Eq)]
struct RestoreSessionBaseline {
    session_id: String,
    note_id: String,
    revision: Option<String>,
    baseline_revision: Option<String>,
}

pub struct ProtectedBackupNoteRestore {
    prepared: PreparedBackupNoteRestore,
    pre_restore_backup: BackupSummary,
}

impl PreparedBackupNoteRestore {
    /// Creates and fully verifies the exact pre-restore library snapshot. The
    /// snapshot must contain the selected note's previously observed revision,
    /// or confirm that the canonical target was absent.
    pub fn protect_current(
        self,
        local_date: &str,
        job: &BackupJob,
    ) -> Result<ProtectedBackupNoteRestore, WorkspaceError> {
        job.checkpoint()?;
        let outcome = self.context.run(BackupKind::Manual, local_date, job)?;
        let pre_restore_backup = outcome.created.ok_or_else(|| {
            StorageError::new(
                "backupIncomplete",
                "恢复前未能生成新的手动保护备份；笔记没有写入。",
            )
        })?;
        if pre_restore_backup.kind != BackupKind::Manual {
            return Err(StorageError::new(
                "backupIncomplete",
                "恢复前保护副本不是手动完整备份；笔记没有写入。",
            ));
        }

        let listed = self
            .context
            .list_notes(&pre_restore_backup.backup_id, job)?;
        let selected = listed
            .notes
            .iter()
            .filter(|note| note.note_id == self.source.note_id)
            .collect::<Vec<_>>();
        match self.target_revision.as_deref() {
            Some(expected) if selected.len() == 1 && selected[0].revision == expected => {}
            None if selected.is_empty() => {}
            _ => {
                return Err(StorageError::new(
                    "backupChanged",
                    "恢复前保护副本中的目标笔记与准备阶段的文件版本不一致；没有覆盖笔记。",
                ));
            }
        }
        job.checkpoint()?;
        Ok(ProtectedBackupNoteRestore {
            prepared: self,
            pre_restore_backup,
        })
    }
}

impl Workspace {
    /// Returns notebook identities suitable for an explicit backup-restore
    /// target selection. Refresh the host's baseline while the workspace lock
    /// is held, then exclude any identity whose metadata is conflicted on disk.
    pub fn backup_restore_notebook_ids(&mut self) -> Result<Vec<String>, WorkspaceError> {
        self.refresh_library_metadata_inner(true)?;
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let library_path = library.path.clone();
        let (notebooks, _) = scan_current_restore_notebooks(&library_path, library)?;
        Ok(notebooks.into_keys().collect())
    }

    /// Captures the current library, editor session, target file revision, and
    /// target notebook metadata before creating a pre-restore snapshot.
    pub fn prepare_backup_note_restore(
        &self,
        source: VerifiedBackupNote,
        target_notebook_id: Option<&str>,
    ) -> Result<PreparedBackupNoteRestore, WorkspaceError> {
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let context = self.backup_context()?;
        if source.source_library_key != context.source_library_key() {
            return Err(StorageError::new(
                "backupLibraryMismatch",
                "备份笔记属于其他笔记库；请切换到对应笔记库后重试。",
            ));
        }
        if !source.has_valid_provenance() || !is_canonical_uuid_v4(&source.note_id) {
            return Err(StorageError::new(
                "backupNoteInvalid",
                "备份笔记身份无效；无法安全恢复。",
            ));
        }

        let (valid_notebooks, conflicted_notebooks) =
            scan_current_restore_notebooks(&library.path, library)?;
        let source_notebook_id = source.package.document.organization.notebook_id.as_deref();
        let valid_source_notebook =
            source_notebook_id.filter(|notebook_id| valid_notebooks.contains_key(*notebook_id));
        let (notebook_id, reassigned_notebook) = if let Some(source_id) = valid_source_notebook {
            if target_notebook_id.is_some_and(|target| target != source_id) {
                return Err(StorageError::new(
                    "restoreNotebookLocked",
                    "原笔记本仍有效；恢复时不能更改笔记本归属。",
                ));
            }
            (source_id.to_owned(), false)
        } else {
            let target = target_notebook_id.ok_or_else(|| {
                StorageError::new(
                    "restoreTargetRequired",
                    "原笔记本已不存在或存在未解决的元数据冲突，请明确选择一个当前有效的笔记本。",
                )
            })?;
            if !is_canonical_uuid_v4(target) {
                return Err(StorageError::new(
                    "unknownNotebook",
                    "恢复目标笔记本不属于当前笔记库。",
                ));
            }
            if !valid_notebooks.contains_key(target) {
                return Err(if conflicted_notebooks.contains(target) {
                    StorageError::new(
                        "restoreNotebookConflict",
                        "恢复目标笔记本存在未解决的元数据冲突；请重新载入备份笔记列表后重试。",
                    )
                } else {
                    StorageError::new("unknownNotebook", "恢复目标笔记本不属于当前笔记库。")
                });
            }
            (target.to_owned(), true)
        };
        let notebook_metadata = valid_notebooks
            .get(&notebook_id)
            .expect("selected notebook is in the freshly validated set")
            .clone();

        let target_revision = scan_restore_target(&library.path, &source.note_id)?;
        let session = self.session.as_ref().map(|session| RestoreSessionBaseline {
            session_id: session.view.session_id.clone(),
            note_id: session.view.note_id.clone(),
            revision: session.view.revision.clone(),
            baseline_revision: session.baseline_revision.clone(),
        });
        Ok(PreparedBackupNoteRestore {
            context,
            source,
            library_path: library.path.clone(),
            target_notebook_id: notebook_id,
            reassigned_notebook,
            target_revision,
            notebook_metadata,
            session,
        })
    }

    /// Publishes a protected restore only if the selected library, editor
    /// session, note file, and notebook metadata still match their baselines.
    pub fn restore_backup_note(
        &mut self,
        protected: ProtectedBackupNoteRestore,
    ) -> Result<BackupRestoreReceipt, WorkspaceError> {
        let prepared = protected.prepared;
        let library_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| {
                post_backup_error(
                    &protected.pre_restore_backup,
                    "当前没有选中的笔记库，未发布恢复。",
                )
            })?;
        if library_path != prepared.library_path {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                "当前笔记库已切换；没有发布恢复。",
            ));
        }
        if !session_matches(self.session.as_ref(), prepared.session.as_ref()) {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                "编辑会话已变化；没有发布恢复。",
            ));
        }
        if !prepared
            .source
            .source_library_key
            .eq(prepared.context.source_library_key())
        {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                "备份笔记与当前笔记库身份不一致；没有发布恢复。",
            ));
        }

        let library = self.library.as_ref().expect("library was checked above");
        ensure_notebook_baseline_current(library, &prepared.target_notebook_id)
            .map_err(|error| with_pre_restore_backup(error, &protected.pre_restore_backup))?;
        verify_fresh_notebook_metadata(
            &library_path,
            &prepared.target_notebook_id,
            &prepared.notebook_metadata,
            library,
        )
        .map_err(|error| with_pre_restore_backup(error, &protected.pre_restore_backup))?;
        let target_revision = scan_restore_target(&library_path, &prepared.source.note_id)
            .map_err(|error| with_pre_restore_backup(error, &protected.pre_restore_backup))?;
        if target_revision != prepared.target_revision {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                "目标笔记文件已变化；没有发布恢复。",
            ));
        }

        let source_note_id = prepared.source.note_id.clone();
        let note_path = library_path
            .join("notes")
            .join(format!("{source_note_id}.cuenote"));
        let mut candidate = prepared.source.package;
        candidate.document.organization.notebook_id = Some(prepared.target_notebook_id.clone());
        if prepared.reassigned_notebook {
            candidate.document.timestamps.organization_modified_at =
                next_timestamp_after(&candidate.document.timestamps.organization_modified_at);
        }
        let (revision, save_warning) = match storage::save_note(
            &note_path,
            &self.recovery_root,
            &candidate,
            prepared.target_revision.as_deref(),
        ) {
            Ok(revision) => (revision, None),
            Err(error) => match storage::open_note(&note_path) {
                Ok(published) if published.package == candidate => (
                    published.revision,
                    Some(format!(
                        "恢复文件已发布并重读确认，但保存步骤报告：{}。原有文件版本已保存在恢复前保护备份 {}。",
                        error.message, protected.pre_restore_backup.path
                    )),
                ),
                _ => {
                return Err(post_backup_cause(
                    error,
                        &protected.pre_restore_backup,
                    &format!(
                        "恢复发布结果未能确认；请检查 {}。",
                        display_path(&note_path)
                    ),
                    ));
                }
            },
        };

        let opened = match storage::open_note(&note_path) {
            Ok(opened) if opened.revision == revision && opened.package == candidate => opened,
            Ok(_) => {
                return Err(post_backup_error(
                    &protected.pre_restore_backup,
                    &format!(
                        "恢复文件已发布，但重读内容与请求不一致；请检查 {}。",
                        display_path(&note_path)
                    ),
                ));
            }
            Err(error) => {
                return Err(post_backup_cause(
                    error,
                    &protected.pre_restore_backup,
                    &format!(
                        "恢复文件可能已发布，但无法重读确认；请检查 {}。",
                        display_path(&note_path)
                    ),
                ));
            }
        };
        debug_assert_eq!(opened.package, candidate);

        self.refresh_library_metadata().map_err(|error| {
            post_backup_cause(
                error,
                &protected.pre_restore_backup,
                "恢复文件已发布，但无法刷新笔记本元数据。",
            )
        })?;
        let staged_session = match self.stage_open_note(&source_note_id) {
            Ok(session) => session,
            Err(error) => {
                return Err(post_backup_cause(
                    error,
                    &protected.pre_restore_backup,
                    &format!(
                        "恢复文件已通过完整包校验并发布，但无法建立编辑会话；请检查 {}。",
                        display_path(&note_path)
                    ),
                ));
            }
        };
        let session = staged_session.view.clone();
        if session.note_id != source_note_id
            || session.revision.as_deref() != Some(revision.as_str())
            || session.notebook_id.as_deref() != Some(prepared.target_notebook_id.as_str())
            || session.pinned != candidate.document.organization.pinned
            || session.trashed_at != candidate.document.organization.trashed_at
            || staged_session.baseline_revision.as_deref() != Some(revision.as_str())
            || staged_session.package != candidate
        {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                &format!(
                    "恢复文件已发布，但待切换的编辑会话与请求版本不一致；请检查 {}。",
                    display_path(&note_path)
                ),
            ));
        }
        let current_library = self.library.as_ref().ok_or_else(|| {
            post_backup_error(
                &protected.pre_restore_backup,
                "恢复文件已发布，但再次打开后笔记库已关闭。",
            )
        })?;
        let expected_read_only_reason = super::read_only_reason(
            &candidate,
            candidate.document.organization.notebook_id.as_deref(),
            &current_library.notebooks,
        );
        if session.read_only_reason != expected_read_only_reason {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                &format!(
                    "恢复文件已发布，但待切换笔记会话的可编辑状态与当前笔记本不一致；请检查 {}。",
                    display_path(&note_path)
                ),
            ));
        }
        ensure_notebook_baseline_current(current_library, &prepared.target_notebook_id).map_err(
            |error| {
                post_backup_cause(
                    error,
                    &protected.pre_restore_backup,
                    "恢复文件已发布，但笔记本元数据复核失败。",
                )
            },
        )?;
        verify_fresh_notebook_metadata(
            &library_path,
            &prepared.target_notebook_id,
            &prepared.notebook_metadata,
            current_library,
        )
        .map_err(|error| {
            post_backup_cause(
                error,
                &protected.pre_restore_backup,
                "恢复文件已发布，但笔记本元数据复核失败。",
            )
        })?;

        let mut warnings = Vec::new();
        if let Some(warning) = save_warning {
            warnings.push(warning);
        }
        let library = match self.refresh_selected_library() {
            Ok(Some(library)) => library,
            Ok(None) => {
                return Err(post_backup_cause(
                    StorageError::new("noLibrary", "恢复后笔记库已关闭。"),
                    &protected.pre_restore_backup,
                    "恢复文件已发布，但刷新笔记库视图失败。",
                ));
            }
            Err(error) => {
                warnings.push(format!("恢复已完成，但无法刷新笔记列表：{}", error.message));
                self.library
                    .as_ref()
                    .expect("selected library remains")
                    .view
                    .clone()
            }
        };

        match storage::open_note(&note_path) {
            Ok(final_opened)
                if final_opened.revision == revision && final_opened.package == candidate => {}
            Ok(_) => {
                return Err(post_backup_error(
                    &protected.pre_restore_backup,
                    &format!(
                        "恢复文件已发布，但最终重读内容与待切换会话不一致；请检查 {}。",
                        display_path(&note_path)
                    ),
                ));
            }
            Err(error) => {
                return Err(post_backup_cause(
                    error,
                    &protected.pre_restore_backup,
                    &format!(
                        "恢复文件已发布，但最终重读校验失败；请检查 {}。",
                        display_path(&note_path)
                    ),
                ));
            }
        }

        let current_library = self.library.as_ref().ok_or_else(|| {
            post_backup_error(
                &protected.pre_restore_backup,
                "恢复文件已发布，但刷新视图后笔记库已关闭。",
            )
        })?;
        ensure_notebook_baseline_current(current_library, &prepared.target_notebook_id).map_err(
            |error| {
                post_backup_cause(
                    error,
                    &protected.pre_restore_backup,
                    "恢复文件已发布，但刷新视图后的笔记本元数据复核失败。",
                )
            },
        )?;
        verify_fresh_notebook_metadata(
            &library_path,
            &prepared.target_notebook_id,
            &prepared.notebook_metadata,
            current_library,
        )
        .map_err(|error| {
            post_backup_cause(
                error,
                &protected.pre_restore_backup,
                "恢复文件已发布，但刷新视图后的笔记本元数据复核失败。",
            )
        })?;
        let expected_read_only_reason = super::read_only_reason(
            &candidate,
            candidate.document.organization.notebook_id.as_deref(),
            &current_library.notebooks,
        );
        if session.read_only_reason != expected_read_only_reason {
            return Err(post_backup_error(
                &protected.pre_restore_backup,
                &format!(
                    "恢复文件已发布，但刷新视图后的笔记状态与待切换会话不一致；请检查 {}。",
                    display_path(&note_path)
                ),
            ));
        }

        if session.trashed_at.is_none() {
            self.persist_preference(&library_path, Some(&source_note_id), &self.preferences)
                .map_err(|error| {
                    post_backup_cause(
                        error,
                        &protected.pre_restore_backup,
                        "恢复文件已发布，但无法更新当前笔记设置。",
                    )
                })?;
            self.last_note_id = Some(source_note_id);
        }
        self.session = Some(staged_session);

        Ok(BackupRestoreReceipt {
            session,
            library,
            pre_restore_backup: protected.pre_restore_backup,
            reassigned_notebook: prepared.reassigned_notebook,
            message: if prepared.reassigned_notebook {
                "笔记已从备份恢复并归入所选的当前笔记本。".into()
            } else {
                "笔记已从备份恢复。".into()
            },
            warnings,
        })
    }
}

fn scan_restore_target(
    library_path: &Path,
    note_id: &str,
) -> Result<Option<String>, WorkspaceError> {
    let notes_path = library_path.join("notes");
    let notes_directory = ensure_child_directory(library_path, &notes_path, false)?;
    let entries = secure_read_dir(library_path, &notes_directory)?;
    if entries.len() > MAX_NOTE_FILES {
        return Err(StorageError::new(
            "resourceLimit",
            "笔记文件数量超过安全上限，无法确认恢复目标是否唯一。",
        ));
    }
    let canonical_path = notes_directory.join(format!("{note_id}.cuenote"));
    let mut target_revision = None;
    for entry in entries {
        let path = entry.path();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cuenote"))
        {
            continue;
        }
        storage::ensure_local_file(&path)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| StorageError::new("io", format!("无法检查笔记文件：{error}")))?;
        if !metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "存在无法安全确认身份的非普通笔记文件；没有发布恢复。",
            ));
        }
        let opened = storage::open_note(&path)?;
        let is_canonical_target = path == canonical_path;
        if is_canonical_target {
            if opened.package.document.note_id != note_id {
                return Err(StorageError::new(
                    "conflict",
                    "恢复目标路径中的笔记身份与文件名不一致。",
                ));
            }
            target_revision = Some(opened.revision);
        } else if opened.package.document.note_id == note_id {
            return Err(StorageError::new(
                "restoreAmbiguous",
                "笔记库中存在同一笔记身份的非标准或冲突文件；请先处理冲突后重试。",
            ));
        }
    }
    Ok(target_revision)
}

fn scan_current_restore_notebooks(
    library_path: &Path,
    library: &super::LibraryState,
) -> Result<(BTreeMap<String, Vec<u8>>, BTreeSet<String>), WorkspaceError> {
    let requested_directory = library_path.join("notebooks");
    let directory = ensure_child_directory(library_path, &requested_directory, false)?;
    let entries = secure_read_dir(library_path, &directory)?;
    if entries.len() > MAX_NOTEBOOKS.saturating_mul(4) {
        return Err(StorageError::new(
            "resourceLimit",
            "笔记本元数据项目超过安全上限，无法确认恢复目标唯一。",
        ));
    }
    let mut valid_notebooks = BTreeMap::new();
    let mut conflicted_notebooks = BTreeSet::new();
    for entry in entries {
        let path = entry.path();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        storage::ensure_local_file(&path)?;
        let file_metadata = fs::symlink_metadata(&path)
            .map_err(|error| StorageError::new("io", format!("无法检查笔记本元数据：{error}")))?;
        if !file_metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "存在无法安全核验的笔记本元数据文件。",
            ));
        }
        let bytes = super::read_sync_bounded(&path, MAX_METADATA_BYTES, "笔记本元数据")?;
        let notebook: NotebookMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            StorageError::new("libraryMetadata", format!("笔记本元数据无效：{error}"))
        })?;
        super::backups::validate_notebook_metadata_fields(&notebook, &display_path(&path))?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| StorageError::new("libraryMetadata", "笔记本元数据文件名无法读取。"))?;
        let is_conflict_copy = filename.starts_with(NOTEBOOK_CONFLICT_PREFIX);
        let canonical_path = directory.join(format!("{}.json", notebook.id));
        if !is_conflict_copy
            && path.file_stem().and_then(|stem| stem.to_str()) != Some(notebook.id.as_str())
        {
            return Err(StorageError::new(
                "libraryMetadata",
                "笔记本元数据身份与规范文件名不一致。",
            ));
        }
        if is_conflict_copy || path != canonical_path {
            conflicted_notebooks.insert(notebook.id);
            continue;
        }
        if valid_notebooks.insert(notebook.id.clone(), bytes).is_some() {
            conflicted_notebooks.insert(notebook.id.clone());
        }
        if valid_notebooks.len() > MAX_NOTEBOOKS {
            return Err(StorageError::new(
                "resourceLimit",
                "有效笔记本数量超过安全上限，无法确认恢复目标。",
            ));
        }
    }
    for notebook_id in &conflicted_notebooks {
        valid_notebooks.remove(notebook_id);
    }
    for (notebook_id, bytes) in &valid_notebooks {
        let metadata: NotebookMetadata = serde_json::from_slice(bytes).map_err(|error| {
            StorageError::new("libraryMetadata", format!("笔记本元数据无效：{error}"))
        })?;
        if library.notebooks.get(notebook_id) != Some(&metadata)
            || library
                .notebook_metadata_baselines
                .get(notebook_id)
                .and_then(Option::as_ref)
                != Some(bytes)
        {
            return Err(StorageError::new(
                "conflict",
                "笔记本元数据已变化；请刷新备份笔记列表后重试。",
            ));
        }
    }
    Ok((valid_notebooks, conflicted_notebooks))
}

fn verify_fresh_notebook_metadata(
    library_path: &Path,
    notebook_id: &str,
    expected_bytes: &[u8],
    library: &super::LibraryState,
) -> Result<(), WorkspaceError> {
    let (valid_notebooks, conflicted_notebooks) =
        scan_current_restore_notebooks(library_path, library)?;
    if conflicted_notebooks.contains(notebook_id) {
        return Err(StorageError::new(
            "restoreNotebookConflict",
            "恢复目标笔记本存在未解决的冲突元数据副本；请先处理冲突后重试。",
        ));
    }
    let actual_bytes = valid_notebooks
        .get(notebook_id)
        .ok_or_else(|| StorageError::new("libraryMetadata", "恢复目标笔记本元数据已不存在。"))?;
    if actual_bytes != expected_bytes {
        return Err(StorageError::new(
            "conflict",
            "恢复目标笔记本元数据已变化；请刷新后重试。",
        ));
    }
    Ok(())
}

fn session_matches(
    current: Option<&super::ActiveNoteSession>,
    expected: Option<&RestoreSessionBaseline>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(current), Some(expected)) => {
            current.view.session_id == expected.session_id
                && current.view.note_id == expected.note_id
                && current.view.revision == expected.revision
                && current.baseline_revision == expected.baseline_revision
        }
        _ => false,
    }
}

fn with_pre_restore_backup(mut error: WorkspaceError, backup: &BackupSummary) -> WorkspaceError {
    error.message = format!(
        "{} 恢复前保护备份已生成，位于 {}。",
        error.message, backup.path
    );
    error.recovery_path = Some(backup.path.clone());
    error
}

fn post_backup_error(backup: &BackupSummary, message: &str) -> WorkspaceError {
    post_backup_cause(
        StorageError::new("restoreConflict", message),
        backup,
        message,
    )
}

fn post_backup_cause(
    mut error: WorkspaceError,
    backup: &BackupSummary,
    message: &str,
) -> WorkspaceError {
    let cause = error.message.clone();
    error.message = format!("{message} 恢复前保护备份位于 {}。", backup.path);
    if cause != message {
        error.message = format!(
            "{message} 原因：{cause} 恢复前保护备份位于 {}。",
            backup.path
        );
    }
    error.recovery_path = Some(backup.path.clone());
    error
}
