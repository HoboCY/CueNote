//! Immutable, independently readable snapshots of the library's native files.
//!
//! This module deliberately enumerates the fact directories itself. The
//! workspace/search scan is a presentation aid and may preserve warnings by
//! skipping unreadable files, which is unsafe for an all-or-nothing backup.

use super::{
    is_canonical_uuid_v4, is_link_or_reparse, is_valid_timestamp, read_bounded,
    resolve_future_path, sync_directory, NotebookMetadata, WorkspaceError, LIBRARY_SCHEMA_VERSION,
    MAX_METADATA_BYTES, MAX_NOTEBOOKS, MAX_NOTE_FILES,
};
use crate::note_format::{NotePackage, MAX_PACKAGE_BYTES};
use crate::storage::{self, StorageError};
use chrono::{NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;

const BACKUP_SCHEMA_VERSION: u32 = 1;
const BACKUP_PREFERENCES_SCHEMA_VERSION: u32 = 1;
const BACKUP_PREFERENCES_PREFIX: &str = "backup-preferences-";
const BACKUP_PREFERENCES_LIMIT: usize = 32;
const MAX_BACKUP_PREFERENCES_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BACKUP_LIBRARY_STATES: usize = 4096;
const MAX_BACKUPS_PER_LIBRARY: usize = 10_000;
const MAX_BACKUP_WARNINGS: usize = 100;
const MAX_BACKUP_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const REQUIRED_FACT_DIRECTORIES: [&str; 2] = ["notes", "notebooks"];
const MANIFEST_FILE_NAME: &str = "manifest.json";
const STAGING_PREFIX: &str = ".staging-";

type ProgressCallback = Arc<dyn Fn(BackupProgress) + Send + Sync>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackupKind {
    Automatic,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BackupPhase {
    Preparing,
    CheckingSource,
    Copying,
    Verifying,
    Publishing,
    Restoring,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupProgress {
    pub library_path: String,
    pub phase: BackupPhase,
    pub checked: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupSummary {
    pub backup_id: String,
    pub kind: BackupKind,
    pub created_at: String,
    pub local_date: String,
    pub path: String,
    pub note_count: usize,
    pub notebook_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupCatalog {
    pub library_path: String,
    pub directory: String,
    pub backups: Vec<BackupSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupOutcome {
    pub catalog: BackupCatalog,
    pub created: Option<BackupSummary>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupNoteList {
    pub library_path: String,
    pub backup_id: String,
    /// Current notebook IDs that the Workspace has freshly validated as
    /// available restore targets. The desktop host fills this after listing
    /// the immutable backup entries.
    pub available_notebook_ids: Vec<String>,
    pub notes: Vec<BackupNoteSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupNoteSummary {
    pub entry_id: String,
    pub revision: String,
    pub note_id: String,
    pub title: String,
    pub modified_at: String,
    pub notebook_id: Option<String>,
    pub notebook_name: Option<String>,
    pub pinned: bool,
    pub trashed_at: Option<String>,
    pub excerpt: String,
}

/// A package loaded from a complete immutable backup. Construction is limited
/// to `BackupContext::load_note_entry`; the workspace restore stages consume
/// this proof without accepting caller-provided paths or package bytes.
#[derive(Debug)]
pub struct VerifiedBackupNote {
    pub(in crate::workspace) backup_id: String,
    pub(in crate::workspace) entry_id: String,
    pub(in crate::workspace) revision: String,
    pub(in crate::workspace) note_id: String,
    pub(in crate::workspace) relative_path: String,
    pub(in crate::workspace) source_library_key: String,
    pub(in crate::workspace) package: NotePackage,
}

impl VerifiedBackupNote {
    pub(super) fn has_valid_provenance(&self) -> bool {
        is_canonical_uuid_v4(&self.backup_id)
            && is_sha256(&self.entry_id)
            && is_sha256(&self.revision)
            && self.relative_path.starts_with("notes/")
            && valid_manifest_path(&self.relative_path)
            && backup_note_entry_id(&self.backup_id, &self.relative_path, &self.revision)
                == self.entry_id
            && is_canonical_uuid_v4(&self.note_id)
            && self.package.document.note_id == self.note_id
    }
}

#[derive(Clone)]
pub struct BackupControl {
    inner: Arc<Mutex<BackupControlState>>,
}

#[derive(Default)]
struct BackupControlState {
    epoch: u64,
    pending_mutations: usize,
    next_job_id: u64,
    active_job: Option<ActiveJob>,
}

#[derive(Clone, Copy)]
struct ActiveJob {
    id: u64,
    epoch: u64,
}

impl Default for BackupControl {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackupControlState {
                next_job_id: 1,
                ..BackupControlState::default()
            })),
        }
    }
}

impl BackupControl {
    /// Registers a write-capable workspace operation before it waits for the
    /// workspace mutex. Existing backup work is cancelled immediately.
    pub fn begin_mutation(&self) -> BackupMutationGuard {
        let mut state = lock_control(&self.inner);
        state.epoch = state.epoch.wrapping_add(1);
        state.pending_mutations = state.pending_mutations.saturating_add(1);
        BackupMutationGuard {
            control: self.clone(),
        }
    }

    /// Cancels the active job without waiting for its filesystem work to stop.
    pub fn cancel(&self) {
        let mut state = lock_control(&self.inner);
        state.epoch = state.epoch.wrapping_add(1);
    }

    /// Starts one backup or catalog job if no library mutation is queued.
    pub fn begin_job(
        &self,
        progress: Option<Arc<dyn Fn(BackupProgress) + Send + Sync>>,
    ) -> Result<BackupJob, WorkspaceError> {
        let mut state = lock_control(&self.inner);
        if state.pending_mutations > 0 || state.active_job.is_some() {
            return Err(StorageError::new(
                "backupBusy",
                "笔记库正在修改或已有备份任务；请稍后重试。",
            ));
        }
        let id = state.next_job_id;
        state.next_job_id = state
            .next_job_id
            .checked_add(1)
            .ok_or_else(|| StorageError::new("backupBusy", "备份任务序号已耗尽。"))?;
        let epoch = state.epoch;
        state.active_job = Some(ActiveJob { id, epoch });
        Ok(BackupJob {
            control: self.clone(),
            id,
            epoch,
            progress,
            library_path: Mutex::new(String::new()),
        })
    }
}

/// A write guard that invalidates any copy already in progress. It must be
/// created before acquiring the workspace mutex.
pub struct BackupMutationGuard {
    control: BackupControl,
}

impl Drop for BackupMutationGuard {
    fn drop(&mut self) {
        let mut state = lock_control(&self.control.inner);
        state.pending_mutations = state.pending_mutations.saturating_sub(1);
    }
}

/// Owns a control epoch for one catalog or backup operation.
pub struct BackupJob {
    control: BackupControl,
    id: u64,
    epoch: u64,
    progress: Option<ProgressCallback>,
    library_path: Mutex<String>,
}

impl BackupJob {
    pub fn is_cancelled(&self) -> bool {
        let state = lock_control(&self.control.inner);
        !job_is_current(&state, self.id, self.epoch)
    }

    pub fn checkpoint(&self) -> Result<(), WorkspaceError> {
        let state = lock_control(&self.control.inner);
        if job_is_current(&state, self.id, self.epoch) {
            Ok(())
        } else {
            Err(backup_cancelled())
        }
    }

    /// Atomically ends the cancellable backup phase and transfers its
    /// coordinator slot into an exclusive workspace mutation guard.
    pub fn into_mutation(self) -> Result<BackupMutationGuard, WorkspaceError> {
        let control = self.control.clone();
        let inner = Arc::clone(&control.inner);
        let mut state = lock_control(&inner);
        if !job_is_current(&state, self.id, self.epoch) {
            return Err(backup_cancelled());
        }
        state.active_job = None;
        state.epoch = state.epoch.wrapping_add(1);
        state.pending_mutations = state.pending_mutations.saturating_add(1);
        drop(state);
        drop(self);
        Ok(BackupMutationGuard { control })
    }

    fn commit_if_current<T>(
        &self,
        action: impl FnOnce() -> Result<T, WorkspaceError>,
    ) -> Result<T, WorkspaceError> {
        // Preference snapshots are bounded local writes. The short gate keeps
        // a stale job from racing an announced mutation or destination change.
        let state = lock_control(&self.control.inner);
        if !job_is_current(&state, self.id, self.epoch) {
            return Err(backup_cancelled());
        }
        action()
    }

    fn bind_library(&self, library_path: &Path) {
        let mut current = self
            .library_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = super::display_path(library_path);
    }

    fn report(&self, phase: BackupPhase, checked: usize, total: usize) {
        // Invoke application code only after releasing the coordinator mutex.
        if let Some(progress) = &self.progress {
            let library_path = self
                .library_path
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            progress(BackupProgress {
                library_path,
                phase,
                checked,
                total,
            });
        }
    }

    /// Emits a host-visible progress phase without holding the backup
    /// coordinator lock while invoking application code.
    pub fn report_progress(&self, phase: BackupPhase, checked: usize, total: usize) {
        self.report(phase, checked, total);
    }

    fn publish(
        &self,
        staged_directory: &Path,
        destination: &Path,
        parent: &Path,
    ) -> Result<(), WorkspaceError> {
        let mut state = lock_control(&self.control.inner);
        if !job_is_current(&state, self.id, self.epoch) {
            return Err(backup_cancelled());
        }
        if destination.exists() || fs::symlink_metadata(destination).is_ok() {
            return Err(StorageError::new(
                "backupConflict",
                "备份编号已存在；未覆盖任何已有副本。",
            ));
        }
        storage::publish_directory_no_replace(parent, staged_directory, destination)
            .map_err(|error| StorageError::new("io", format!("无法发布已校验备份：{error}")))?;
        // `state` remains locked across the atomic rename and directory sync,
        // so a queued write cannot pass the publication gate in between.
        state.active_job = Some(ActiveJob {
            id: self.id,
            epoch: self.epoch,
        });
        Ok(())
    }
}

impl Drop for BackupJob {
    fn drop(&mut self) {
        let mut state = lock_control(&self.control.inner);
        if state.active_job.is_some_and(|active| active.id == self.id) {
            state.active_job = None;
        }
    }
}

fn lock_control(control: &Mutex<BackupControlState>) -> MutexGuard<'_, BackupControlState> {
    control
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn job_is_current(state: &BackupControlState, id: u64, epoch: u64) -> bool {
    state.pending_mutations == 0
        && state.epoch == epoch
        && state
            .active_job
            .is_some_and(|active| active.id == id && active.epoch == epoch)
}

fn backup_cancelled() -> WorkspaceError {
    StorageError::new("backupCancelled", "笔记库发生变化，备份已取消；请重试。")
}

/// Detached paths and immutable source identity for a single selected library.
/// Hashing, copying and package validation do not retain the Workspace mutex.
#[derive(Clone)]
pub struct BackupContext {
    library_path: PathBuf,
    directory: PathBuf,
    source_key: String,
    config_root: PathBuf,
    recovery_root: PathBuf,
}

impl BackupContext {
    pub(super) fn from_workspace(
        library_path: PathBuf,
        config_root: PathBuf,
        recovery_root: PathBuf,
    ) -> Result<Self, WorkspaceError> {
        let library_path = fs::canonicalize(&library_path)
            .map_err(|error| StorageError::new("io", format!("无法解析当前笔记库目录：{error}")))?;
        storage::ensure_local_directory(&library_path)?;
        let config_root = resolve_future_path(&config_root)?;
        let recovery_root = resolve_future_path(&recovery_root)?;
        let preferences = load_backup_preferences(&config_root)?;
        let requested = match preferences.directory.as_deref() {
            Some(directory) => PathBuf::from(directory),
            None => default_backup_directory(&recovery_root)?,
        };
        let directory =
            validate_backup_location(&requested, &library_path, &config_root, &recovery_root)?;
        Ok(Self {
            source_key: source_library_key(&library_path),
            library_path,
            directory,
            config_root,
            recovery_root,
        })
    }

    pub(super) fn with_directory(
        library_path: PathBuf,
        config_root: PathBuf,
        recovery_root: PathBuf,
        directory: PathBuf,
    ) -> Result<Self, WorkspaceError> {
        let library_path = fs::canonicalize(&library_path)
            .map_err(|error| StorageError::new("io", format!("无法解析当前笔记库目录：{error}")))?;
        storage::ensure_local_directory(&library_path)?;
        let config_root = resolve_future_path(&config_root)?;
        let recovery_root = resolve_future_path(&recovery_root)?;
        let directory =
            validate_backup_location(&directory, &library_path, &config_root, &recovery_root)?;
        let mut preferences = load_backup_preferences(&config_root)?;
        preferences.directory = Some(super::display_path(&directory));
        store_backup_preferences(&config_root, preferences)?;
        Ok(Self {
            source_key: source_library_key(&library_path),
            library_path,
            directory,
            config_root,
            recovery_root,
        })
    }

    pub fn library_path(&self) -> &Path {
        &self.library_path
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub(super) fn source_library_key(&self) -> &str {
        &self.source_key
    }

    pub fn catalog(&self, job: &BackupJob) -> Result<BackupCatalog, WorkspaceError> {
        let job = job;
        job.bind_library(&self.library_path);
        job.checkpoint()?;
        self.validate_destination_now(false)?;
        let backup_root = self
            .library_backup_root(true)?
            .ok_or_else(|| StorageError::new("io", "无法准备本机备份目录。"))?;
        let _lock = acquire_backup_root_lock(&backup_root)?;
        self.catalog_locked(&backup_root, job)
    }

    /// Lists native notes inside one verified backup. The caller supplies only
    /// the opaque backup identity; all paths are derived from this context and
    /// the validated manifest.
    pub fn list_notes(
        &self,
        backup_id: &str,
        job: &BackupJob,
    ) -> Result<BackupNoteList, WorkspaceError> {
        job.bind_library(&self.library_path);
        job.checkpoint()?;
        if !is_canonical_uuid_v4(backup_id) {
            return Err(StorageError::new("backupNotFound", "备份身份无效。"));
        }
        self.validate_destination_now(false)?;
        let backup_root = self
            .library_backup_root(false)?
            .ok_or_else(|| StorageError::new("backupNotFound", "指定备份不存在。"))?;
        let _lock = acquire_backup_root_lock(&backup_root)?;
        let backup_path = backup_root.join(backup_id);
        let validated = validate_backup_directory(&backup_path, &self.source_key, job)?;
        if validated.summary.backup_id != backup_id {
            return Err(StorageError::new(
                "backupIncomplete",
                "指定备份身份与已验证清单不一致。",
            ));
        }

        let mut notebook_names = BTreeMap::<String, Option<String>>::new();
        let notebook_files = validated
            .inventory
            .files
            .values()
            .filter(|file| file.path.starts_with("notebooks/"))
            .collect::<Vec<_>>();
        let note_files = validated
            .inventory
            .files
            .values()
            .filter(|file| file.path.starts_with("notes/"))
            .collect::<Vec<_>>();
        let total = notebook_files.len().saturating_add(note_files.len());
        let mut checked = 0usize;
        for file in notebook_files {
            job.checkpoint()?;
            let path = backup_path.join(relative_path(&file.path)?);
            let bytes =
                super::read_sync_bounded(&path, MAX_METADATA_BYTES, "备份中的笔记本元数据")?;
            if bytes.len() as u64 != file.length || sha256_hex(&bytes) != file.sha256 {
                return Err(StorageError::new(
                    "backupChanged",
                    format!("读取笔记本元数据期间备份发生变化：{}", file.path),
                ));
            }
            let notebook: super::NotebookMetadata =
                serde_json::from_slice(&bytes).map_err(|error| {
                    StorageError::new(
                        "backupIncomplete",
                        format!("备份中的笔记本元数据无效 {}：{error}", file.path),
                    )
                })?;
            validate_notebook_metadata(&notebook, &path, &file.path)?;
            match notebook_names.entry(notebook.id) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Some(notebook.name));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
            checked += 1;
            job.report(BackupPhase::Verifying, checked, total);
        }

        let mut notes = Vec::with_capacity(note_files.len());
        for file in note_files {
            job.checkpoint()?;
            let path = backup_path.join(relative_path(&file.path)?);
            storage::ensure_local_file(&path)?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                StorageError::new("backupIncomplete", format!("无法检查备份笔记：{error}"))
            })?;
            if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() != file.length
            {
                return Err(StorageError::new(
                    "backupChanged",
                    format!("备份笔记在读取期间发生变化：{}", file.path),
                ));
            }
            let opened = storage::open_note(&path)?;
            if opened.revision != file.sha256 {
                return Err(StorageError::new(
                    "backupChanged",
                    format!("备份笔记在读取期间发生变化：{}", file.path),
                ));
            }
            let note_id = opened.package.document.note_id.clone();
            let summary = super::note_summary_from_package(&note_id, &opened.package);
            notes.push(BackupNoteSummary {
                entry_id: backup_note_entry_id(backup_id, &file.path, &opened.revision),
                revision: opened.revision,
                note_id: summary.note_id,
                title: summary.title,
                modified_at: summary.modified_at,
                notebook_name: summary
                    .notebook_id
                    .as_ref()
                    .and_then(|id| notebook_names.get(id))
                    .and_then(Clone::clone),
                notebook_id: summary.notebook_id,
                pinned: summary.pinned,
                trashed_at: summary.trashed_at,
                excerpt: summary.excerpt,
            });
            checked += 1;
            job.report(BackupPhase::Verifying, checked, total);
        }

        let revalidated = validate_backup_directory(&backup_path, &self.source_key, job)?;
        if revalidated.manifest_sha256 != validated.manifest_sha256
            || !revalidated.inventory.same_facts(&validated.inventory)
        {
            return Err(StorageError::new(
                "backupChanged",
                "列出备份笔记期间备份内容发生变化。",
            ));
        }
        job.checkpoint()?;
        Ok(BackupNoteList {
            library_path: super::display_path(&self.library_path),
            backup_id: backup_id.to_owned(),
            available_notebook_ids: Vec::new(),
            notes,
        })
    }

    pub fn load_note_entry(
        &self,
        backup_id: &str,
        entry_id: &str,
        expected_revision: &str,
        job: &BackupJob,
    ) -> Result<VerifiedBackupNote, WorkspaceError> {
        job.bind_library(&self.library_path);
        job.checkpoint()?;
        if !is_canonical_uuid_v4(backup_id) || !is_sha256(entry_id) || !is_sha256(expected_revision)
        {
            return Err(StorageError::new(
                "backupNoteNotFound",
                "备份笔记身份无效。",
            ));
        }
        let backup_root = self
            .library_backup_root(false)?
            .ok_or_else(|| StorageError::new("backupNotFound", "指定备份不存在。"))?;
        let _lock = acquire_backup_root_lock(&backup_root)?;
        let backup_path = backup_root.join(backup_id);
        let validated = validate_backup_directory(&backup_path, &self.source_key, job)?;
        let file = validated
            .inventory
            .files
            .values()
            .find(|file| {
                file.path.starts_with("notes/")
                    && backup_note_entry_id(backup_id, &file.path, &file.sha256) == entry_id
            })
            .ok_or_else(|| {
                StorageError::new("backupNoteNotFound", "所选笔记不在指定的已验证备份中。")
            })?;
        if file.sha256 != expected_revision {
            return Err(StorageError::new(
                "backupChanged",
                "所选备份笔记版本与当前选择不一致；请重新读取备份列表。",
            ));
        }
        let path = backup_path.join(relative_path(&file.path)?);
        storage::ensure_local_file(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new("backupIncomplete", format!("无法检查所选备份笔记：{error}"))
        })?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() != file.length {
            return Err(StorageError::new(
                "backupChanged",
                "所选备份笔记在读取期间发生变化。",
            ));
        }
        let opened = storage::open_note(&path)?;
        if opened.revision != expected_revision {
            return Err(StorageError::new(
                "backupChanged",
                "所选备份笔记在读取期间发生变化。",
            ));
        }
        let note_id = opened.package.document.note_id.clone();
        let revalidated = validate_backup_directory(&backup_path, &self.source_key, job)?;
        if revalidated.manifest_sha256 != validated.manifest_sha256
            || !revalidated.inventory.same_facts(&validated.inventory)
        {
            return Err(StorageError::new(
                "backupChanged",
                "读取备份笔记期间备份内容发生变化。",
            ));
        }
        job.checkpoint()?;
        Ok(VerifiedBackupNote {
            backup_id: backup_id.to_owned(),
            entry_id: entry_id.to_owned(),
            revision: opened.revision,
            note_id,
            relative_path: file.path.clone(),
            source_library_key: self.source_key.clone(),
            package: opened.package,
        })
    }

    fn catalog_locked(
        &self,
        backup_root: &Path,
        job: &BackupJob,
    ) -> Result<BackupCatalog, WorkspaceError> {
        job.checkpoint()?;
        let mut catalog = BackupCatalog {
            library_path: super::display_path(&self.library_path),
            directory: super::display_path(&self.directory),
            backups: Vec::new(),
            warnings: Vec::new(),
        };
        let entries = fs::read_dir(backup_root)
            .map_err(|error| StorageError::new("io", format!("无法读取备份目录：{error}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StorageError::new("io", format!("无法读取备份项目：{error}")))?;
        let candidates = entries
            .into_iter()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| is_canonical_uuid_v4(name))
            })
            .collect::<Vec<_>>();
        if candidates.len() > MAX_BACKUPS_PER_LIBRARY {
            return Err(StorageError::new(
                "backupTooLarge",
                "备份目录项目数量超过安全上限，无法完整核验。",
            ));
        }
        let total = candidates.len();
        for (index, entry) in candidates.into_iter().enumerate() {
            job.checkpoint()?;
            let path = entry.path();
            match validate_backup_directory(&path, &self.source_key, job) {
                Ok(validated) => catalog.backups.push(validated.summary),
                Err(error) if error.kind == "backupCancelled" => return Err(error),
                Err(error) => push_backup_warning(
                    &mut catalog.warnings,
                    format!(
                        "已排除未完成或损坏的备份 {}：{}",
                        super::display_path(&path),
                        error.message
                    ),
                ),
            }
            job.report(BackupPhase::Verifying, index + 1, total);
        }
        catalog
            .backups
            .sort_by(|left, right| right.created_at.cmp(&left.created_at));
        job.checkpoint()?;
        Ok(catalog)
    }

    /// Creates a manual snapshot or checks whether an observed local change
    /// needs its one automatic snapshot for the injected local date.
    pub fn run(
        &self,
        kind: BackupKind,
        observed_local_date: &str,
        job: &BackupJob,
    ) -> Result<BackupOutcome, WorkspaceError> {
        let job = job;
        job.bind_library(&self.library_path);
        validate_local_date(observed_local_date)?;
        job.checkpoint()?;
        self.validate_destination_now(true)?;
        let backup_root = self
            .library_backup_root(true)?
            .ok_or_else(|| StorageError::new("io", "无法准备本机备份目录。"))?;
        let _lock = acquire_backup_root_lock(&backup_root)?;
        let mut catalog = self.catalog_locked(&backup_root, job)?;
        let current = scan_fact_tree(&self.library_path, job, BackupPhase::CheckingSource, true)?;

        if kind == BackupKind::Manual {
            let summary =
                self.create_snapshot(BackupKind::Manual, observed_local_date, &current, job)?;
            catalog.backups.push(summary.clone());
            catalog
                .backups
                .sort_by(|left, right| right.created_at.cmp(&left.created_at));
            job.report(BackupPhase::Complete, 1, 1);
            return Ok(BackupOutcome {
                catalog,
                created: Some(summary),
                message: "手动备份已完成并通过完整性校验。".into(),
            });
        }

        let mut preferences = load_backup_preferences(&self.config_root)?;
        let source_state = preferences
            .libraries
            .entry(self.source_key.clone())
            .or_default();
        let fingerprint = current.fingerprint();
        let changed_since_last_observation =
            source_state.observed_fingerprint.as_deref() != Some(fingerprint.as_str());
        let mut warnings = Vec::new();
        if let Some(close_date) = source_state.close_check_due_date.take() {
            if close_date.as_str() < observed_local_date && changed_since_last_observation {
                push_backup_warning(
                    &mut warnings,
                    format!(
                        "上次离开笔记库时（{close_date}）的自动备份检查未完成；现按当前实际观察日备份。"
                    ),
                );
            }
        }
        if changed_since_last_observation {
            if let (Some(pending_date), Some(pending_fingerprint)) = (
                source_state.pending_local_date.as_deref(),
                source_state.pending_fingerprint.as_deref(),
            ) {
                if pending_fingerprint != fingerprint && pending_date != observed_local_date {
                    push_backup_warning(
                        &mut warnings,
                        format!(
                            "未能在 {pending_date} 备份时观察到的版本；当前文件已变化，将按 {observed_local_date} 记录最新状态。"
                        ),
                    );
                    source_state.pending_local_date = Some(observed_local_date.to_owned());
                }
            } else if !current.files.is_empty() {
                source_state.pending_local_date = Some(observed_local_date.to_owned());
            }
            if source_state.pending_local_date.as_deref() == Some(observed_local_date)
                || source_state.pending_local_date.is_none()
            {
                source_state.pending_local_date = Some(observed_local_date.to_owned());
            }
            source_state.pending_fingerprint = Some(fingerprint.clone());
        }
        source_state.observed_fingerprint = Some(fingerprint.clone());
        source_state.observed_local_date = Some(observed_local_date.to_owned());

        let pending_date = source_state.pending_local_date.clone();
        let pending_fingerprint = source_state.pending_fingerprint.clone();
        let already_backed_up = pending_date.as_ref().is_some_and(|date| {
            catalog
                .backups
                .iter()
                .any(|summary| summary.kind == BackupKind::Automatic && &summary.local_date == date)
        });
        if already_backed_up {
            source_state.pending_local_date = None;
            source_state.pending_fingerprint = None;
        }
        job.commit_if_current(|| store_backup_preferences(&self.config_root, preferences))?;

        let Some(pending_date) = pending_date.filter(|_| !already_backed_up) else {
            catalog.warnings.extend(warnings);
            let message = if catalog.warnings.is_empty() {
                "今天没有观察到新的已保存内容，无需自动备份。".to_owned()
            } else {
                "今天没有需要新增的自动备份；请留意未完成日期提示。".to_owned()
            };
            return Ok(BackupOutcome {
                catalog,
                created: None,
                message,
            });
        };

        if pending_fingerprint.as_deref() != Some(&fingerprint) {
            return Err(StorageError::new(
                "backupChanged",
                "自动备份观察版本已变化；请重新检查当前笔记库。",
            ));
        }
        let summary = self.create_snapshot(BackupKind::Automatic, &pending_date, &current, job)?;
        catalog.backups.push(summary.clone());
        catalog
            .backups
            .sort_by(|left, right| right.created_at.cmp(&left.created_at));
        catalog.warnings.extend(warnings);

        let rotation = self.rotate_automatic_backups(&backup_root, &catalog.backups, job);
        let excluded = rotation
            .removed_ids
            .iter()
            .chain(rotation.invalid_ids.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        catalog
            .backups
            .retain(|backup| !excluded.contains(&backup.backup_id));
        for warning in rotation.warnings {
            push_backup_warning(&mut catalog.warnings, warning);
        }
        let mut stored = load_backup_preferences(&self.config_root)?;
        if let Some(source_state) = stored.libraries.get_mut(&self.source_key) {
            if source_state.pending_fingerprint.as_deref() == Some(&fingerprint)
                && source_state.pending_local_date.as_deref() == Some(pending_date.as_str())
            {
                source_state.pending_local_date = None;
                source_state.pending_fingerprint = None;
            }
        }
        if let Err(error) =
            job.commit_if_current(|| store_backup_preferences(&self.config_root, stored))
        {
            push_backup_warning(
                &mut catalog.warnings,
                format!("备份已发布，但本机自动备份记录未更新：{}", error.message),
            );
        }
        job.report(BackupPhase::Complete, 1, 1);
        Ok(BackupOutcome {
            catalog,
            created: Some(summary),
            message: "自动备份已完成并通过完整性校验。".into(),
        })
    }

    fn validate_destination_now(&self, create: bool) -> Result<(), WorkspaceError> {
        let validated = validate_backup_location(
            &self.directory,
            &self.library_path,
            &self.config_root,
            &self.recovery_root,
        )?;
        if create {
            ensure_directory_tree(&validated)?;
            storage::probe_directory(&validated)?;
        } else if fs::symlink_metadata(&validated).is_ok() {
            super::ensure_real_directory(&validated, None)?;
        }
        Ok(())
    }

    fn library_backup_root(&self, create: bool) -> Result<Option<PathBuf>, WorkspaceError> {
        let directory = validate_backup_location(
            &self.directory,
            &self.library_path,
            &self.config_root,
            &self.recovery_root,
        )?;
        if !create && fs::symlink_metadata(&directory).is_err() {
            return Ok(None);
        }
        ensure_directory_tree(&directory)?;
        let path = directory.join(&self.source_key);
        if create {
            ensure_directory(&path, true)?;
        } else if fs::symlink_metadata(&path).is_err() {
            return Ok(None);
        } else {
            ensure_directory(&path, false)?;
        }
        Ok(Some(path))
    }

    fn create_snapshot(
        &self,
        kind: BackupKind,
        local_date: &str,
        source: &FactInventory,
        job: &BackupJob,
    ) -> Result<BackupSummary, WorkspaceError> {
        job.checkpoint()?;
        let backup_root = self
            .library_backup_root(true)?
            .ok_or_else(|| StorageError::new("io", "无法准备备份目录。"))?;
        let backup_id = Uuid::new_v4().to_string();
        let staged = backup_root.join(format!("{STAGING_PREFIX}{backup_id}"));
        fs::create_dir(&staged)
            .map_err(|error| StorageError::new("io", format!("无法创建备份暂存目录：{error}")))?;
        let mut created_files = Vec::<PathBuf>::new();
        let mut published = false;
        let result = (|| {
            for name in REQUIRED_FACT_DIRECTORIES {
                ensure_directory(&staged.join(name), true)?;
            }
            let total = source.files.len();
            for (index, file) in source.files.values().enumerate() {
                job.checkpoint()?;
                let destination = staged.join(relative_path(&file.path)?);
                created_files.push(destination.clone());
                job.report(BackupPhase::Copying, index, total);
                let copied = copy_and_hash(&file.source_path, &destination, file.length, job)?;
                if copied.length != file.length || copied.sha256 != file.sha256 {
                    return Err(StorageError::new(
                        "backupChanged",
                        format!("复制期间文件版本发生变化：{}", file.path),
                    ));
                }
                crate::storage_faults::checkpoint(
                    "backup_file_copied",
                    &file.source_path,
                    Some(&destination),
                )?;
                job.report(BackupPhase::Copying, index + 1, total);
            }

            job.report(BackupPhase::Verifying, 0, 1);
            let staged_inventory = scan_fact_tree(&staged, job, BackupPhase::Verifying, false)?;
            if !source.same_facts(&staged_inventory) {
                return Err(StorageError::new(
                    "backupIncomplete",
                    "暂存副本的文件集合或 SHA-256 与复制前不一致。",
                ));
            }
            let source_after_copy =
                scan_fact_tree(&self.library_path, job, BackupPhase::CheckingSource, true)?;
            if !source.same_facts(&source_after_copy) {
                return Err(StorageError::new(
                    "backupChanged",
                    "复制期间笔记库文件集合或内容发生变化；未发布混合版本。",
                ));
            }

            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let manifest = BackupManifest {
                schema_version: BACKUP_SCHEMA_VERSION,
                backup_id: backup_id.clone(),
                source_library_key: self.source_key.clone(),
                kind,
                created_at: created_at.clone(),
                local_date: local_date.to_owned(),
                directories: REQUIRED_FACT_DIRECTORIES
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                files: source.manifest_files(),
            };
            let manifest_path = staged.join(MANIFEST_FILE_NAME);
            write_manifest(&manifest_path, &manifest)?;
            created_files.push(manifest_path);
            for name in REQUIRED_FACT_DIRECTORIES {
                sync_directory(&staged.join(name)).map_err(|error| {
                    StorageError::new("io", format!("无法同步备份子目录：{error}"))
                })?;
            }
            sync_directory(&staged).map_err(|error| {
                StorageError::new("io", format!("无法同步备份暂存目录：{error}"))
            })?;

            let validated = validate_backup_directory_with_id(
                &staged,
                &self.source_key,
                Some(&backup_id),
                job,
            )?;
            if validated.summary.backup_id != backup_id
                || validated.summary.kind != kind
                || validated.summary.local_date != local_date
            {
                return Err(StorageError::new(
                    "backupIncomplete",
                    "暂存备份清单与当前请求不一致。",
                ));
            }
            job.report(BackupPhase::Publishing, 0, 1);
            job.checkpoint()?;
            // Progress listeners may synchronously trigger a save or tests
            // may edit a source file at this boundary. Recheck after notifying
            // them and immediately before taking the publication gate.
            let final_source_check =
                scan_fact_tree(&self.library_path, job, BackupPhase::CheckingSource, true)?;
            if !source.same_facts(&final_source_check) {
                return Err(StorageError::new(
                    "backupChanged",
                    "发布前发现笔记库已变化；未发布混合版本。",
                ));
            }

            let final_path = backup_root.join(&backup_id);
            job.publish(&staged, &final_path, &backup_root)?;
            published = true;
            let mut summary = validated.summary;
            summary.path = super::display_path(&final_path);
            job.report(BackupPhase::Publishing, 1, 1);
            Ok(summary)
        })();

        if result.is_err() && !published {
            cleanup_staging(&staged, &created_files);
        }
        result
    }

    fn rotate_automatic_backups(
        &self,
        backup_root: &Path,
        backups: &[BackupSummary],
        job: &BackupJob,
    ) -> RotationResult {
        let mut automatic = backups
            .iter()
            .filter(|backup| backup.kind == BackupKind::Automatic)
            .collect::<Vec<_>>();
        automatic.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        let mut result = RotationResult::default();
        for backup in automatic.into_iter().skip(30) {
            if let Err(error) = job.checkpoint() {
                result.warnings.push(format!(
                    "已发布新备份；备份任务已取消，停止清理旧自动备份：{}",
                    error.message
                ));
                break;
            }
            let path = backup_root.join(&backup.backup_id);
            let validated = match validate_backup_directory(&path, &self.source_key, job) {
                Ok(validated) => validated,
                Err(error) => {
                    if error.kind == "backupCancelled" {
                        result.warnings.push(format!(
                            "已发布新备份；备份任务已取消，停止清理旧自动备份 {}。",
                            backup.backup_id
                        ));
                        break;
                    }
                    result.invalid_ids.push(backup.backup_id.clone());
                    result.warnings.push(format!(
                        "已发布新备份，但旧自动备份 {} 无法再核验，因此未继续轮换：{}",
                        backup.backup_id, error.message
                    ));
                    break;
                }
            };
            if validated.summary.kind != BackupKind::Automatic
                || validated.summary.backup_id != backup.backup_id
            {
                result.invalid_ids.push(backup.backup_id.clone());
                result.warnings.push(format!(
                    "已发布新备份，但旧自动备份 {} 身份不一致，因此未继续轮换。",
                    backup.backup_id
                ));
                break;
            }
            match remove_verified_backup(&path, &validated, job) {
                Ok(()) => result.removed_ids.push(backup.backup_id.clone()),
                Err(error) => {
                    if validate_backup_directory(&path, &self.source_key, &NoopJob).is_err() {
                        result.invalid_ids.push(backup.backup_id.clone());
                    }
                    result.warnings.push(format!(
                        "已发布新备份，但无法完成旧自动备份 {} 的轮换；未继续清理：{}",
                        backup.backup_id, error.message
                    ));
                    break;
                }
            }
        }
        result
    }
}

#[derive(Default)]
struct RotationResult {
    removed_ids: Vec<String>,
    invalid_ids: Vec<String>,
    warnings: Vec<String>,
}

struct BackupRootLock {
    _file: File,
}

fn acquire_backup_root_lock(directory: &Path) -> Result<BackupRootLock, WorkspaceError> {
    let path = directory.join(".cuenote-backup.lock");
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(StorageError::new("unsafePath", "备份锁路径不是普通文件。"));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)
        .map_err(|error| StorageError::new("io", format!("无法打开备份协调锁：{error}")))?;
    if !file
        .metadata()
        .map_err(|error| StorageError::new("io", format!("无法检查备份协调锁：{error}")))?
        .is_file()
    {
        return Err(StorageError::new("unsafePath", "备份协调锁不是普通文件。"));
    }
    match file.try_lock() {
        Ok(()) => Ok(BackupRootLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(StorageError::new(
            "backupBusy",
            "另一个 CueNote 实例正在检查或创建此笔记库的备份。",
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(StorageError::new(
            "io",
            format!("无法取得备份协调锁：{error}"),
        )),
    }
}

// Retention runs only after a completed publication. It performs no external
// work or callbacks, so a tiny local job adapter is enough for revalidation.
struct NoopJob;

impl BackupJobLike for NoopJob {
    fn checkpoint(&self) -> Result<(), WorkspaceError> {
        Ok(())
    }
    fn report(&self, _phase: BackupPhase, _checked: usize, _total: usize) {}
}

trait BackupJobLike {
    fn checkpoint(&self) -> Result<(), WorkspaceError>;
    fn report(&self, phase: BackupPhase, checked: usize, total: usize);
}

impl BackupJobLike for BackupJob {
    fn checkpoint(&self) -> Result<(), WorkspaceError> {
        BackupJob::checkpoint(self)
    }
    fn report(&self, phase: BackupPhase, checked: usize, total: usize) {
        BackupJob::report(self, phase, checked, total)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupManifest {
    schema_version: u32,
    backup_id: String,
    source_library_key: String,
    kind: BackupKind,
    created_at: String,
    local_date: String,
    directories: Vec<String>,
    files: Vec<BackupManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupManifestFile {
    path: String,
    length: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FactFile {
    path: String,
    length: u64,
    sha256: String,
    source_path: PathBuf,
}

#[derive(Debug, Clone)]
struct FactInventory {
    directories: Vec<String>,
    files: BTreeMap<String, FactFile>,
    note_count: usize,
    notebook_count: usize,
}

impl FactInventory {
    fn same_facts(&self, other: &Self) -> bool {
        self.directories == other.directories
            && self.note_count == other.note_count
            && self.notebook_count == other.notebook_count
            && self.files.len() == other.files.len()
            && self.files.iter().all(|(path, file)| {
                other
                    .files
                    .get(path)
                    .is_some_and(|other| file.length == other.length && file.sha256 == other.sha256)
            })
    }

    fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for directory in &self.directories {
            hasher.update(b"directory\0");
            hasher.update(directory.as_bytes());
            hasher.update(b"\0");
        }
        for file in self.files.values() {
            hasher.update(file.path.as_bytes());
            hasher.update(b"\0");
            hasher.update(file.length.to_le_bytes());
            hasher.update(b"\0");
            hasher.update(file.sha256.as_bytes());
            hasher.update(b"\n");
        }
        hex_digest(hasher.finalize().as_slice())
    }

    fn manifest_files(&self) -> Vec<BackupManifestFile> {
        self.files
            .values()
            .map(|file| BackupManifestFile {
                path: file.path.clone(),
                length: file.length,
                sha256: file.sha256.clone(),
            })
            .collect()
    }
}

#[derive(Debug)]
struct ValidatedBackup {
    summary: BackupSummary,
    manifest: BackupManifest,
    manifest_sha256: String,
    inventory: FactInventory,
}

fn scan_fact_tree(
    root: &Path,
    job: &impl BackupJobLike,
    phase: BackupPhase,
    allow_cloud_files: bool,
) -> Result<FactInventory, WorkspaceError> {
    let _cloud_policy = storage::NoCloudDownload::enter()?;
    job.checkpoint()?;
    storage::ensure_local_directory(root)?;
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| StorageError::new("io", format!("无法检查事实目录：{error}")))?;
    if (!allow_cloud_files && is_link_or_reparse(&root_metadata)) || !root_metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "事实目录必须是本机普通目录。",
        ));
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| StorageError::new("io", format!("无法解析事实目录：{error}")))?;
    let mut directories = Vec::with_capacity(REQUIRED_FACT_DIRECTORIES.len());
    let mut entries_by_directory = Vec::<(String, Vec<fs::DirEntry>)>::new();
    let mut total = 0usize;
    for name in REQUIRED_FACT_DIRECTORIES {
        job.checkpoint()?;
        let path = canonical_root.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new(
                "backupIncomplete",
                format!("缺少或无法检查笔记库目录 {name}：{error}"),
            )
        })?;
        storage::ensure_local_directory(&path)?;
        let canonical = fs::canonicalize(&path)
            .map_err(|error| StorageError::new("io", format!("无法解析 {name} 目录：{error}")))?;
        if (!allow_cloud_files && is_link_or_reparse(&metadata))
            || !metadata.is_dir()
            || canonical != canonical_root.join(name)
        {
            return Err(StorageError::new(
                "unsafePath",
                format!("笔记库 {name} 目录不是库内的普通目录。"),
            ));
        }
        let mut entries = fs::read_dir(&canonical)
            .map_err(|error| StorageError::new("io", format!("无法读取 {name} 目录：{error}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StorageError::new("io", format!("无法读取 {name} 项目：{error}")))?;
        entries.sort_by_key(|entry| entry.file_name());
        total = total.saturating_add(entries.len());
        directories.push(name.to_owned());
        entries_by_directory.push((name.to_owned(), entries));
    }
    job.report(phase, 0, total);
    let mut files = BTreeMap::new();
    let mut folded_paths = BTreeSet::new();
    let mut note_count = 0usize;
    let mut notebook_count = 0usize;
    let mut checked = 0usize;
    for (directory, entries) in entries_by_directory {
        for entry in entries {
            job.checkpoint()?;
            checked += 1;
            let path = entry.path();
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    StorageError::new("backupIncomplete", "事实文件名不是有效 UTF-8。")
                })?;
            let extension = path.extension().and_then(|extension| extension.to_str());
            let expected_extension = if directory == "notes" {
                "cuenote"
            } else {
                "json"
            };
            if !extension
                .is_some_and(|extension| extension.eq_ignore_ascii_case(expected_extension))
            {
                // Finder metadata, unrelated app files and stale non-native
                // artifacts are outside the source facts. Published backup
                // directories, by contrast, must contain only manifested
                // native assets.
                if allow_cloud_files {
                    job.report(phase, checked, total);
                    continue;
                }
                return Err(StorageError::new(
                    "backupIncomplete",
                    format!("备份包含清单之外的非原生文件：{}", path.display()),
                ));
            }
            storage::ensure_local_file(&path)?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                StorageError::new(
                    "io",
                    format!("无法检查事实文件 {}：{error}", path.display()),
                )
            })?;
            if (!allow_cloud_files && is_link_or_reparse(&metadata)) || !metadata.is_file() {
                return Err(StorageError::new(
                    "unsafePath",
                    format!("事实目录中的原生文件不是可读取普通文件：{}", path.display()),
                ));
            }
            if filename.contains('/')
                || filename.contains('\\')
                || filename.contains(':')
                || filename == "."
                || filename == ".."
            {
                return Err(StorageError::new(
                    "unsafePath",
                    "事实文件名包含不能安全移植的路径字符。",
                ));
            }
            let relative = format!("{directory}/{filename}");
            if !folded_paths.insert(relative.to_lowercase()) {
                return Err(StorageError::new(
                    "backupIncomplete",
                    format!("事实文件名在跨平台文件系统中冲突：{relative}"),
                ));
            }
            if directory == "notes" {
                note_count += 1;
                if note_count > MAX_NOTE_FILES {
                    return Err(StorageError::new(
                        "libraryTooLarge",
                        "笔记文件数量超出备份上限。",
                    ));
                }
                let file_length = metadata.len();
                if file_length > MAX_PACKAGE_BYTES {
                    return Err(StorageError::new(
                        "budget_exceeded",
                        format!("笔记文件超过安全上限：{relative}"),
                    ));
                }
                let opened = storage::open_note(&path)?;
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default();
                if is_canonical_uuid_v4(stem) && opened.package.document.note_id != stem {
                    return Err(StorageError::new(
                        "backupIncomplete",
                        format!("标准笔记文件名与原生笔记身份不一致：{relative}"),
                    ));
                }
                let digest = hash_file(&path, file_length, job)?;
                if digest.length != file_length || digest.sha256 != opened.revision {
                    return Err(StorageError::new(
                        "backupChanged",
                        format!("校验期间笔记文件发生变化：{relative}"),
                    ));
                }
                files.insert(
                    relative.clone(),
                    FactFile {
                        path: relative,
                        length: digest.length,
                        sha256: digest.sha256,
                        source_path: path,
                    },
                );
            } else {
                notebook_count += 1;
                if notebook_count > MAX_NOTEBOOKS {
                    return Err(StorageError::new(
                        "libraryTooLarge",
                        "笔记本元数据文件数量超出备份上限。",
                    ));
                }
                let bytes = super::read_sync_bounded(&path, MAX_METADATA_BYTES, "笔记本元数据")?;
                let notebook: NotebookMetadata =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        StorageError::new(
                            "libraryMetadata",
                            format!("笔记本元数据无效 {relative}：{error}"),
                        )
                    })?;
                validate_notebook_metadata(&notebook, &path, &relative)?;
                let digest = hash_file(&path, bytes.len() as u64, job)?;
                if digest.length != bytes.len() as u64 || digest.sha256 != sha256_hex(&bytes) {
                    return Err(StorageError::new(
                        "backupChanged",
                        format!("校验期间笔记本元数据发生变化：{relative}"),
                    ));
                }
                files.insert(
                    relative.clone(),
                    FactFile {
                        path: relative,
                        length: digest.length,
                        sha256: digest.sha256,
                        source_path: path,
                    },
                );
            }
            job.report(phase, checked, total);
        }
    }
    Ok(FactInventory {
        directories,
        files,
        note_count,
        notebook_count,
    })
}

fn validate_notebook_metadata(
    notebook: &NotebookMetadata,
    path: &Path,
    relative: &str,
) -> Result<(), WorkspaceError> {
    validate_notebook_metadata_fields(notebook, relative)?;
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        if is_canonical_uuid_v4(stem) && notebook.id != stem {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("标准笔记本文件名与元数据身份不一致：{relative}"),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_notebook_metadata_fields(
    notebook: &NotebookMetadata,
    relative: &str,
) -> Result<(), WorkspaceError> {
    if notebook.schema_version != LIBRARY_SCHEMA_VERSION {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("笔记本元数据版本不受支持：{relative}"),
        ));
    }
    if !is_canonical_uuid_v4(&notebook.id) {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("笔记本元数据身份无效：{relative}"),
        ));
    }
    if notebook.name.trim().is_empty()
        || notebook.name.chars().count() > 256
        || notebook.name.chars().any(char::is_control)
    {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("笔记本名称无效：{relative}"),
        ));
    }
    if !is_valid_timestamp(&notebook.timestamps.created_at)
        || !is_valid_timestamp(&notebook.timestamps.modified_at)
    {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("笔记本时间格式无效：{relative}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileDigest {
    length: u64,
    sha256: String,
}

fn hash_file(
    path: &Path,
    max_bytes: u64,
    job: &impl BackupJobLike,
) -> Result<FileDigest, WorkspaceError> {
    let mut file = storage::open_local_read(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    let mut length = 0u64;
    loop {
        job.checkpoint()?;
        let read = file.read(&mut buffer).map_err(|error| {
            StorageError::new(
                "io",
                format!("无法读取备份源文件 {}：{error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        length = bounded_next_length(length, read, max_bytes)?;
        hasher.update(&buffer[..read]);
    }
    Ok(FileDigest {
        length,
        sha256: hex_digest(hasher.finalize().as_slice()),
    })
}

fn copy_and_hash(
    source: &Path,
    destination: &Path,
    max_bytes: u64,
    job: &BackupJob,
) -> Result<FileDigest, WorkspaceError> {
    let mut input = storage::open_local_read(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| StorageError::new("io", format!("无法创建备份文件：{error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    let mut length = 0u64;
    loop {
        job.checkpoint()?;
        let read = input
            .read(&mut buffer)
            .map_err(|error| StorageError::new("io", format!("无法读取笔记库文件：{error}")))?;
        if read == 0 {
            break;
        }
        let next_length = bounded_next_length(length, read, max_bytes)?;
        output
            .write_all(&buffer[..read])
            .map_err(|error| StorageError::new("io", format!("无法写入备份副本：{error}")))?;
        length = next_length;
        hasher.update(&buffer[..read]);
    }
    output
        .flush()
        .and_then(|_| output.sync_all())
        .map_err(|error| StorageError::new("io", format!("无法同步备份文件：{error}")))?;
    Ok(FileDigest {
        length,
        sha256: hex_digest(hasher.finalize().as_slice()),
    })
}

fn bounded_next_length(current: u64, read: usize, max_bytes: u64) -> Result<u64, WorkspaceError> {
    current
        .checked_add(read as u64)
        .filter(|length| *length <= max_bytes)
        .ok_or_else(|| {
            StorageError::new(
                "backupChanged",
                "文件在校验或复制期间超过已记录长度或安全大小上限。",
            )
        })
}

fn validate_backup_directory(
    directory: &Path,
    expected_source_key: &str,
    job: &impl BackupJobLike,
) -> Result<ValidatedBackup, WorkspaceError> {
    validate_backup_directory_with_id(directory, expected_source_key, None, job)
}

fn validate_backup_directory_with_id(
    directory: &Path,
    expected_source_key: &str,
    expected_backup_id: Option<&str>,
    job: &impl BackupJobLike,
) -> Result<ValidatedBackup, WorkspaceError> {
    job.checkpoint()?;
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        StorageError::new("backupIncomplete", format!("备份目录不可用：{error}"))
    })?;
    storage::ensure_local_directory(directory)?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "已发布备份必须是本机普通目录。",
        ));
    }
    let directory_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StorageError::new("backupIncomplete", "备份目录名称无效。"))?;
    let backup_id = expected_backup_id.unwrap_or(directory_name);
    if !is_canonical_uuid_v4(backup_id)
        || (expected_backup_id.is_none() && directory_name != backup_id)
    {
        return Err(StorageError::new("backupIncomplete", "备份目录身份无效。"));
    }
    let mut root_entries = fs::read_dir(directory)
        .map_err(|error| {
            StorageError::new("backupIncomplete", format!("无法读取备份目录：{error}"))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            StorageError::new("backupIncomplete", format!("无法读取备份项目：{error}"))
        })?;
    root_entries.sort_by_key(|entry| entry.file_name());
    if root_entries.len() != 3 {
        return Err(StorageError::new(
            "backupIncomplete",
            "备份根目录包含缺失或未识别的项目。",
        ));
    }
    for name in REQUIRED_FACT_DIRECTORIES {
        if !root_entries.iter().any(|entry| entry.file_name() == name) {
            return Err(StorageError::new(
                "backupIncomplete",
                format!("备份缺少原生目录：{name}"),
            ));
        }
    }
    if !root_entries
        .iter()
        .any(|entry| entry.file_name() == MANIFEST_FILE_NAME)
    {
        return Err(StorageError::new("backupIncomplete", "备份清单不存在。"));
    }
    for entry in &root_entries {
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child).map_err(|error| {
            StorageError::new("backupIncomplete", format!("无法检查备份项目：{error}"))
        })?;
        if is_link_or_reparse(&metadata) {
            return Err(StorageError::new("unsafePath", "备份包含链接或重解析点。"));
        }
        let name = entry.file_name();
        if name == MANIFEST_FILE_NAME {
            if !metadata.is_file() {
                return Err(StorageError::new(
                    "backupIncomplete",
                    "备份清单不是普通文件。",
                ));
            }
        } else if !metadata.is_dir() {
            return Err(StorageError::new("backupIncomplete", "备份原生目录无效。"));
        }
    }

    let manifest_path = directory.join(MANIFEST_FILE_NAME);
    let manifest_bytes = read_bounded(&manifest_path, MAX_BACKUP_MANIFEST_BYTES, "整库备份清单")?;
    let manifest: BackupManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| StorageError::new("backupIncomplete", format!("备份清单无效：{error}")))?;
    if manifest.schema_version != BACKUP_SCHEMA_VERSION
        || manifest.backup_id != backup_id
        || manifest.source_library_key != expected_source_key
        || !is_canonical_uuid_v4(&manifest.backup_id)
        || !is_sha256(&manifest.source_library_key)
        || !is_valid_timestamp(&manifest.created_at)
        || !valid_local_date(&manifest.local_date)
    {
        return Err(StorageError::new(
            "backupIncomplete",
            "备份清单的版本、库身份或时间字段无效。",
        ));
    }
    if manifest.directories
        != REQUIRED_FACT_DIRECTORIES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    {
        return Err(StorageError::new(
            "backupIncomplete",
            "备份清单中的原生目录集合不完整。",
        ));
    }
    let inventory = scan_fact_tree(directory, job, BackupPhase::Verifying, false)?;
    let manifest_files = manifest.files.iter().cloned().collect::<Vec<_>>();
    if manifest_files.len() != inventory.files.len() {
        return Err(StorageError::new(
            "backupIncomplete",
            "备份文件集合与 SHA-256 清单不一致。",
        ));
    }
    let mut previous_path = None::<&str>;
    for file in &manifest.files {
        job.checkpoint()?;
        if !valid_manifest_path(&file.path)
            || !is_sha256(&file.sha256)
            || previous_path.is_some_and(|previous| previous >= file.path.as_str())
        {
            return Err(StorageError::new(
                "backupIncomplete",
                "备份清单包含无效、重复或未排序的文件项。",
            ));
        }
        previous_path = Some(file.path.as_str());
        let observed = inventory.files.get(&file.path).ok_or_else(|| {
            StorageError::new(
                "backupIncomplete",
                format!("清单所列文件缺失：{}", file.path),
            )
        })?;
        if observed.length != file.length || observed.sha256 != file.sha256 {
            return Err(StorageError::new(
                "backupIncomplete",
                format!("备份文件与清单 SHA-256 不一致：{}", file.path),
            ));
        }
    }
    let summary = BackupSummary {
        backup_id: manifest.backup_id.clone(),
        kind: manifest.kind,
        created_at: manifest.created_at.clone(),
        local_date: manifest.local_date.clone(),
        path: super::display_path(directory),
        note_count: inventory.note_count,
        notebook_count: inventory.notebook_count,
    };
    Ok(ValidatedBackup {
        summary,
        manifest,
        manifest_sha256: sha256_hex(&manifest_bytes),
        inventory,
    })
}

fn write_manifest(path: &Path, manifest: &BackupManifest) -> Result<(), WorkspaceError> {
    let bytes = serde_json::to_vec(manifest).map_err(|error| {
        StorageError::new("backupIncomplete", format!("无法生成备份清单：{error}"))
    })?;
    if bytes.len() as u64 > MAX_BACKUP_MANIFEST_BYTES {
        return Err(StorageError::new(
            "libraryTooLarge",
            "备份清单超过安全大小上限。",
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| StorageError::new("io", format!("无法创建备份清单：{error}")))?;
    file.write_all(&bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|error| StorageError::new("io", format!("无法同步备份清单：{error}")))
}

fn cleanup_staging(staged: &Path, created_files: &[PathBuf]) {
    for path in created_files.iter().rev() {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.is_file() && !is_link_or_reparse(&metadata) {
                let _ = fs::remove_file(path);
            }
        }
    }
    for name in REQUIRED_FACT_DIRECTORIES {
        let directory = staged.join(name);
        if let Ok(metadata) = fs::symlink_metadata(&directory) {
            if metadata.is_dir() && !is_link_or_reparse(&metadata) {
                let _ = fs::remove_dir(directory);
            }
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(staged) {
        if metadata.is_dir() && !is_link_or_reparse(&metadata) {
            let _ = fs::remove_dir(staged);
        }
    }
}

fn remove_verified_backup(
    directory: &Path,
    validated: &ValidatedBackup,
    job: &BackupJob,
) -> Result<(), WorkspaceError> {
    if validated.summary.kind != BackupKind::Automatic {
        return Err(StorageError::new(
            "backupIncomplete",
            "仅允许清理自动备份。",
        ));
    }
    let parent = directory
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "待轮换备份缺少父目录。"))?;
    let retiring = parent.join(format!(
        ".retiring-{}-{}",
        validated.summary.backup_id,
        Uuid::new_v4().simple()
    ));
    job.checkpoint()?;
    storage::move_directory_no_replace(parent, directory, &retiring).map_err(|error| {
        StorageError::new("io", format!("无法将旧自动备份隔离到唯一清理路径：{error}"))
    })?;

    // The catalog validation may be stale. Re-read the actual isolated
    // directory and require the same automatic manifest before deleting any
    // path. If this fails, keep the uniquely named directory for recovery.
    let cleanup = (|| {
        let current = validate_backup_directory_with_id(
            &retiring,
            &validated.manifest.source_library_key,
            Some(&validated.summary.backup_id),
            job,
        )?;
        if current.summary.kind != BackupKind::Automatic
            || current.manifest_sha256 != validated.manifest_sha256
        {
            return Err(StorageError::new(
                "backupIncomplete",
                "隔离后的旧备份内容或清单已变化。",
            ));
        }

        for file in current.inventory.files.values() {
            job.checkpoint()?;
            let path = retiring.join(relative_path(&file.path)?);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| StorageError::new("io", format!("无法检查旧备份文件：{error}")))?;
            if is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(StorageError::new("unsafePath", "待轮换备份发生变化。"));
            }
            let digest = hash_file(&path, file.length, job)?;
            if digest.length != file.length || digest.sha256 != file.sha256 {
                return Err(StorageError::new(
                    "backupIncomplete",
                    "待轮换备份文件再次变化。",
                ));
            }
            fs::remove_file(path).map_err(|error| {
                StorageError::new("io", format!("无法清理旧自动备份文件：{error}"))
            })?;
        }
        job.checkpoint()?;
        let manifest_path = retiring.join(MANIFEST_FILE_NAME);
        let manifest_metadata = fs::symlink_metadata(&manifest_path)
            .map_err(|error| StorageError::new("io", format!("无法检查旧备份清单：{error}")))?;
        if is_link_or_reparse(&manifest_metadata) || !manifest_metadata.is_file() {
            return Err(StorageError::new("unsafePath", "待轮换备份清单发生变化。"));
        }
        let manifest_bytes =
            read_bounded(&manifest_path, MAX_BACKUP_MANIFEST_BYTES, "旧自动备份清单")?;
        if sha256_hex(&manifest_bytes) != current.manifest_sha256 {
            return Err(StorageError::new(
                "backupIncomplete",
                "待轮换备份清单再次变化。",
            ));
        }
        fs::remove_file(manifest_path)
            .map_err(|error| StorageError::new("io", format!("无法清理旧备份清单：{error}")))?;
        for name in REQUIRED_FACT_DIRECTORIES {
            job.checkpoint()?;
            let child = retiring.join(name);
            let metadata = fs::symlink_metadata(&child)
                .map_err(|error| StorageError::new("io", format!("无法检查旧备份目录：{error}")))?;
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new("unsafePath", "待轮换备份目录发生变化。"));
            }
            fs::remove_dir(child)
                .map_err(|error| StorageError::new("io", format!("无法清理旧备份目录：{error}")))?;
        }
        job.checkpoint()?;
        let retiring_metadata = fs::symlink_metadata(&retiring)
            .map_err(|error| StorageError::new("io", format!("无法检查旧自动备份：{error}")))?;
        if is_link_or_reparse(&retiring_metadata) || !retiring_metadata.is_dir() {
            return Err(StorageError::new(
                "unsafePath",
                "清理隔离路径类型发生变化。",
            ));
        }
        fs::remove_dir(&retiring)
            .map_err(|error| StorageError::new("io", format!("无法移除旧自动备份：{error}")))?;
        sync_directory(parent)
            .map_err(|error| StorageError::new("io", format!("无法同步轮换目录：{error}")))?;
        Ok(())
    })();

    cleanup.map_err(|error| {
        StorageError::new(
            error.kind,
            format!(
                "清理隔离路径 {} 保留剩余内容：{}",
                super::display_path(&retiring),
                error.message
            ),
        )
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupPreferences {
    schema_version: u32,
    sequence: u64,
    directory: Option<String>,
    #[serde(default)]
    libraries: BTreeMap<String, BackupSourceState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupSourceState {
    observed_fingerprint: Option<String>,
    observed_local_date: Option<String>,
    pending_fingerprint: Option<String>,
    pending_local_date: Option<String>,
    #[serde(default)]
    close_check_due_date: Option<String>,
}

pub(super) fn mark_backup_check_due(
    config_root: &Path,
    library_path: &Path,
    observed_local_date: &str,
) -> Result<(), WorkspaceError> {
    validate_local_date(observed_local_date)?;
    let mut preferences = load_backup_preferences(config_root)?;
    let source_state = preferences
        .libraries
        .entry(source_library_key(library_path))
        .or_default();
    source_state.close_check_due_date = Some(observed_local_date.to_owned());
    store_backup_preferences(config_root, preferences)
}

fn load_backup_preferences(config_root: &Path) -> Result<BackupPreferences, WorkspaceError> {
    let metadata = match fs::symlink_metadata(config_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(default_preferences()),
        Err(error) => {
            return Err(StorageError::new(
                "settings",
                format!("无法检查本机备份设置目录：{error}"),
            ));
        }
    };
    storage::ensure_local_directory(config_root)?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "本机备份设置目录必须是普通目录。",
        ));
    }
    let entries = fs::read_dir(config_root)
        .map_err(|error| StorageError::new("settings", format!("无法读取备份设置：{error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::new("settings", format!("无法读取备份设置项目：{error}")))?;
    let mut snapshots = entries
        .into_iter()
        .filter_map(|entry| {
            parse_backup_preferences_filename(&entry.file_name())
                .map(|sequence| (sequence, entry.path()))
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| left.0.cmp(&right.0));
    let Some((sequence, path)) = snapshots.last() else {
        return Ok(default_preferences());
    };
    let bytes = read_bounded(&path, MAX_BACKUP_PREFERENCES_BYTES, "本机备份设置")?;
    let preferences: BackupPreferences = serde_json::from_slice(&bytes)
        .map_err(|error| StorageError::new("settings", format!("本机备份设置无效：{error}")))?;
    validate_preferences(&preferences, *sequence)?;
    Ok(preferences)
}

fn default_preferences() -> BackupPreferences {
    BackupPreferences {
        schema_version: BACKUP_PREFERENCES_SCHEMA_VERSION,
        sequence: 0,
        directory: None,
        libraries: BTreeMap::new(),
    }
}

fn validate_preferences(
    preferences: &BackupPreferences,
    filename_sequence: u64,
) -> Result<(), WorkspaceError> {
    if preferences.schema_version != BACKUP_PREFERENCES_SCHEMA_VERSION
        || preferences.sequence != filename_sequence
        || preferences.libraries.len() > MAX_BACKUP_LIBRARY_STATES
    {
        return Err(StorageError::new(
            "settings",
            "本机备份设置版本、序号或库数量无效。",
        ));
    }
    if preferences
        .directory
        .as_deref()
        .is_some_and(|directory| directory.trim().is_empty() || !Path::new(directory).is_absolute())
    {
        return Err(StorageError::new("settings", "本机备份目录设置无效。"));
    }
    for (key, state) in &preferences.libraries {
        if !is_sha256(key)
            || state
                .observed_fingerprint
                .as_deref()
                .is_some_and(|fingerprint| !is_sha256(fingerprint))
            || state
                .pending_fingerprint
                .as_deref()
                .is_some_and(|fingerprint| !is_sha256(fingerprint))
            || state
                .observed_local_date
                .as_deref()
                .is_some_and(|date| !valid_local_date(date))
            || state
                .pending_local_date
                .as_deref()
                .is_some_and(|date| !valid_local_date(date))
            || state
                .close_check_due_date
                .as_deref()
                .is_some_and(|date| !valid_local_date(date))
        {
            return Err(StorageError::new("settings", "本机自动备份记录无效。"));
        }
    }
    Ok(())
}

fn store_backup_preferences(
    config_root: &Path,
    mut preferences: BackupPreferences,
) -> Result<(), WorkspaceError> {
    super::ensure_config_directory(config_root)?;
    let previous = load_backup_preferences(config_root)?;
    preferences.schema_version = BACKUP_PREFERENCES_SCHEMA_VERSION;
    preferences.sequence = previous
        .sequence
        .checked_add(1)
        .ok_or_else(|| StorageError::new("settings", "本机备份设置序号已耗尽。"))?;
    if preferences.libraries.len() > MAX_BACKUP_LIBRARY_STATES {
        return Err(StorageError::new(
            "settings",
            "本机自动备份记录超过安全数量上限。",
        ));
    }
    validate_preferences(&preferences, preferences.sequence)?;
    let bytes = serde_json::to_vec(&preferences)
        .map_err(|error| StorageError::new("settings", format!("无法编码备份设置：{error}")))?;
    if bytes.len() as u64 > MAX_BACKUP_PREFERENCES_BYTES {
        return Err(StorageError::new(
            "settings",
            "本机备份设置超过安全大小上限。",
        ));
    }
    let name = format!(
        "{BACKUP_PREFERENCES_PREFIX}{:020}-{}.json",
        preferences.sequence,
        Uuid::new_v4()
    );
    super::publish_unique_snapshot(config_root, &name, &bytes, ".backup-preferences-")?;
    prune_backup_preferences(config_root)?;
    Ok(())
}

fn prune_backup_preferences(config_root: &Path) -> Result<(), WorkspaceError> {
    let entries = fs::read_dir(config_root)
        .map_err(|error| StorageError::new("settings", format!("无法读取备份设置：{error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::new("settings", format!("无法读取备份设置项目：{error}")))?;
    let mut snapshots = entries
        .into_iter()
        .filter_map(|entry| {
            parse_backup_preferences_filename(&entry.file_name())
                .map(|sequence| (sequence, entry.path()))
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| left.0.cmp(&right.0));
    let remove_count = snapshots.len().saturating_sub(BACKUP_PREFERENCES_LIMIT);
    for (_, path) in snapshots.into_iter().take(remove_count) {
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new("settings", format!("无法检查旧备份设置：{error}"))
        })?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "旧的本机备份设置不是普通文件。",
            ));
        }
        fs::remove_file(path).map_err(|error| {
            StorageError::new("settings", format!("无法清理旧备份设置：{error}"))
        })?;
    }
    Ok(())
}

fn parse_backup_preferences_filename(name: &std::ffi::OsStr) -> Option<u64> {
    let name = name.to_str()?;
    let body = name
        .strip_prefix(BACKUP_PREFERENCES_PREFIX)?
        .strip_suffix(".json")?;
    let (sequence, id) = body.split_once('-')?;
    if sequence.len() != 20
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
        || !is_canonical_uuid_v4(id)
    {
        return None;
    }
    sequence.parse().ok()
}

fn default_backup_directory(recovery_root: &Path) -> Result<PathBuf, WorkspaceError> {
    let preferred = recovery_root
        .parent()
        .unwrap_or(recovery_root)
        .join("backups");
    let mut candidates = vec![preferred];
    #[cfg(windows)]
    if let Some(local_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local_data).join("CueNote").join("backups"));
    }
    #[cfg(not(windows))]
    if let Some(local_data) = std::env::var_os("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(local_data).join("CueNote").join("backups"));
    }
    for candidate in candidates {
        let candidate = resolve_future_path(&candidate)?;
        if !is_within_known_sync_root(&candidate) {
            return Ok(candidate);
        }
    }
    Err(StorageError::new(
        "unsafePath",
        "无法找到 OneDrive 同步目录之外的默认备份位置；请在本机选择独立目录。",
    ))
}

fn validate_backup_location(
    requested: &Path,
    library: &Path,
    config: &Path,
    recovery: &Path,
) -> Result<PathBuf, WorkspaceError> {
    let requested = resolve_future_path(requested)?;
    for protected in [library, recovery] {
        if paths_overlap(&requested, protected) {
            return Err(StorageError::new(
                "unsafePath",
                "备份目录必须与笔记库、本机设置和恢复目录分开；请选择独立本机目录。",
            ));
        }
    }
    // On macOS Tauri's app-config and app-local-data roots can be the same
    // Application Support directory. A dedicated child (for example
    // `Application Support/CueNote/backups`) is safe; the config root itself
    // and any ancestor that would contain all app state are not.
    if requested == config || path_is_within(config, &requested) {
        return Err(StorageError::new(
            "unsafePath",
            "备份目录不能是本机设置目录或其父目录；请选择专用子目录或其他本机目录。",
        ));
    }
    if is_within_known_sync_root(&requested) {
        return Err(StorageError::new(
            "unsafePath",
            "备份目录不能位于已识别的 OneDrive 同步目录内；请选择独立本机目录。",
        ));
    }
    if fs::symlink_metadata(&requested).is_ok() {
        super::ensure_real_directory(&requested, None)
    } else {
        Ok(requested)
    }
}

fn known_sync_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for variable in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(path) = std::env::var_os(variable).filter(|path| !path.is_empty()) {
            roots.push(PathBuf::from(path));
        }
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(home) = home {
        roots.push(home.join("OneDrive"));
        let cloud_storage = home.join("Library").join("CloudStorage");
        if let Ok(entries) = fs::read_dir(cloud_storage) {
            roots.extend(entries.filter_map(Result::ok).filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let is_onedrive = name
                    .get(..8)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("OneDrive"));
                let is_directory = entry.file_type().is_ok_and(|file_type| file_type.is_dir());
                (is_onedrive && is_directory).then(|| entry.path())
            }));
        }
    }
    roots
        .into_iter()
        .filter(|path| path.is_absolute())
        .map(|path| resolve_future_path(&path).unwrap_or(path))
        .collect()
}

fn is_within_known_sync_root(path: &Path) -> bool {
    known_sync_roots()
        .iter()
        .any(|root| path_is_within(path, root))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    path_is_within(left, right) || path_is_within(right, left)
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().replace('/', "\\").to_lowercase();
        let root = root.to_string_lossy().replace('/', "\\").to_lowercase();
        path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|suffix| suffix.starts_with('\\'))
    }
    #[cfg(not(windows))]
    {
        path == root || path.starts_with(root)
    }
}

fn source_library_key(path: &Path) -> String {
    let mut hasher = Sha256::new();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(not(any(windows, unix)))]
    hasher.update(path.to_string_lossy().as_bytes());
    hex_digest(hasher.finalize().as_slice())
}

fn ensure_directory_tree(path: &Path) -> Result<PathBuf, WorkspaceError> {
    if fs::symlink_metadata(path).is_ok() {
        return super::ensure_real_directory(path, None);
    }
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::new("unsafePath", "备份目录没有有效父目录。"))?;
    ensure_directory_tree(parent)?;
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(StorageError::new(
                "io",
                format!("无法创建本机备份目录：{error}"),
            ));
        }
    }
    super::ensure_real_directory(path, None)
}

fn ensure_directory(path: &Path, create: bool) -> Result<PathBuf, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            storage::ensure_local_directory(path)?;
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new(
                    "unsafePath",
                    "备份子目录必须是本机普通目录。",
                ));
            }
            fs::canonicalize(path)
                .map_err(|error| StorageError::new("io", format!("无法解析备份子目录：{error}")))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            let parent = path
                .parent()
                .ok_or_else(|| StorageError::new("unsafePath", "备份子目录没有有效父目录。"))?;
            let canonical_parent = super::ensure_real_directory(parent, None)?;
            fs::create_dir(path)
                .or_else(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|error| StorageError::new("io", format!("无法创建备份子目录：{error}")))?;
            super::ensure_real_directory(path, Some(&canonical_parent))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(StorageError::new(
            "backupIncomplete",
            format!("备份子目录不存在：{}", path.display()),
        )),
        Err(error) => Err(StorageError::new(
            "io",
            format!("无法检查备份子目录：{error}"),
        )),
    }
}

fn relative_path(relative: &str) -> Result<PathBuf, WorkspaceError> {
    if !valid_manifest_path(relative) {
        return Err(StorageError::new("unsafePath", "备份相对路径无效。"));
    }
    let mut components = relative.split('/');
    let first = components.next().unwrap_or_default();
    let second = components.next().unwrap_or_default();
    if components.next().is_some() {
        return Err(StorageError::new("unsafePath", "备份相对路径层级无效。"));
    }
    Ok(Path::new(first).join(second))
}

fn valid_manifest_path(path: &str) -> bool {
    let mut parts = path.split('/');
    let Some(directory) = parts.next() else {
        return false;
    };
    let Some(filename) = parts.next() else {
        return false;
    };
    if parts.next().is_some()
        || !REQUIRED_FACT_DIRECTORIES.contains(&directory)
        || filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains(':')
        || filename.chars().any(char::is_control)
    {
        return false;
    }
    let extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str());
    match (directory, extension) {
        ("notes", Some(extension)) => extension.eq_ignore_ascii_case("cuenote"),
        ("notebooks", Some(extension)) => extension.eq_ignore_ascii_case("json"),
        _ => false,
    }
}

fn validate_local_date(date: &str) -> Result<(), WorkspaceError> {
    if valid_local_date(date) {
        Ok(())
    } else {
        Err(StorageError::new(
            "invalidBackupDate",
            "备份日期必须使用 YYYY-MM-DD 格式。",
        ))
    }
}

fn valid_local_date(date: &str) -> bool {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|parsed| parsed.format("%Y-%m-%d").to_string() == date)
        .unwrap_or(false)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

fn backup_note_entry_id(backup_id: &str, relative_path: &str, revision: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(backup_id.as_bytes());
    hasher.update([0]);
    hasher.update(relative_path.as_bytes());
    hasher.update([0]);
    hasher.update(revision.as_bytes());
    hex_digest(hasher.finalize().as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn push_backup_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() < MAX_BACKUP_WARNINGS {
        warnings.push(warning);
    }
}
