//! Durable, resumable replacement of a library's native fact set.

use super::backups::{
    BackupContext, BackupJob, BackupKind, BackupPhase, BackupSummary, NativeLibraryFact,
};
use super::{StorageError, Workspace, WorkspaceError, WorkspaceView};
use crate::storage::{
    self,
    restore_facts::{FactDigest, RawFactStep},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const RESTORE_ROOT_NAME: &str = "library-restores";
const PLAN_FILE: &str = "plan.json";
const SOURCE_STAGE: &str = "source";
const BEFORE_STAGE: &str = "before";
const EVENTS_DIRECTORY: &str = "events";
const POINTER_PREFIX: &str = "active-";
const MAX_IDENTITY_BYTES: u64 = 4096;
const MAX_PLAN_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_FACTS: usize = 1024 + 100_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryRestorePreview {
    pub backup: BackupSummary,
    pub manifest_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LibraryRestoreAction {
    Continue,
    RollBack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryRestoreReceipt {
    pub workspace: WorkspaceView,
    pub pre_restore_backup: BackupSummary,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryRestoreStatus {
    pub restore_id: String,
    pub library_path: String,
    pub source_backup_id: String,
    pub pre_restore_backup: Option<BackupSummary>,
    pub phase: String,
    pub completed_files: usize,
    pub total_files: usize,
    pub can_continue: bool,
    pub can_roll_back: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestorePlan {
    schema_version: u32,
    restore_id: String,
    library_key: String,
    library_path: String,
    source_backup_id: String,
    source_manifest_revision: String,
    source_backup: BackupSummary,
    pre_restore_backup: BackupSummary,
    before_manifest_revision: String,
    before: Vec<NativeLibraryFact>,
    after: Vec<NativeLibraryFact>,
    #[serde(skip)]
    _task_root: PathBuf,
    #[serde(skip)]
    _recovery_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreIdentity {
    schema_version: u32,
    library_key: String,
    restore_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalEntry {
    schema_version: u32,
    restore_id: String,
    step: Option<usize>,
    state: Option<String>,
}

#[derive(Debug, Default)]
struct JournalState {
    forward_intents: HashSet<usize>,
    forward_done: HashSet<usize>,
    rollback_started: bool,
    rollback_intents: HashSet<usize>,
    rollback_done: HashSet<usize>,
    terminal: Option<String>,
    closed: bool,
    closed_state: Option<String>,
}

struct PendingRestore {
    plan: RestorePlan,
    task_root: PathBuf,
    state: JournalState,
    marker_present: bool,
}

impl Workspace {
    /// Validates an immutable backup and returns the exact manifest revision a
    /// later restore request must confirm.
    pub fn preview_library_restore(
        &self,
        backup_id: &str,
        job: &BackupJob,
    ) -> Result<LibraryRestorePreview, WorkspaceError> {
        let library_path = self.library_path()?;
        let _access = storage::library_access::lock_shared(&self.recovery_root, &library_path)?;
        let context = self.backup_context()?;
        let snapshot = context.verify_library_restore_backup(backup_id, None, job)?;
        Ok(LibraryRestorePreview {
            backup: snapshot.summary,
            manifest_revision: snapshot.manifest_revision,
        })
    }

    /// Creates a verified manual pre-restore backup, stages both snapshots,
    /// then starts the journaled raw-fact replacement.
    pub fn restore_library_from_backup(
        &mut self,
        backup_id: &str,
        expected_manifest_revision: &str,
        local_date: &str,
        job: &BackupJob,
    ) -> Result<LibraryRestoreReceipt, WorkspaceError> {
        let library_path = self.library_path()?;
        let _access = storage::library_access::lock_exclusive(&self.recovery_root, &library_path)?;
        if let Some(status) = self.pending_library_restore(&library_path)? {
            return Err(StorageError::new(
                "libraryRestorePending",
                format!("{}；请先继续或回退该整库恢复。", status.message),
            ));
        }
        storage::library_access::ensure_no_restore_marker(&library_path)?;
        storage::ensure_no_pending_publications(&self.recovery_root, &library_path)?;

        let context = self.backup_context()?;
        let source = context.verify_library_restore_backup(
            backup_id,
            Some(expected_manifest_revision),
            job,
        )?;
        let before_outcome = context.run(BackupKind::Manual, local_date, job)?;
        let pre_restore_backup = before_outcome.created.ok_or_else(|| {
            StorageError::new(
                "backupIncomplete",
                "没有生成完整的恢复前手动备份；未修改笔记库。",
            )
        })?;
        let before =
            context.verify_library_restore_backup(&pre_restore_backup.backup_id, None, job)?;
        let current = context.inspect_library_restore_facts(job)?;
        if !same_facts(&current, &before.facts) {
            return Err(StorageError::new(
                "backupChanged",
                "创建恢复前备份期间笔记库发生变化；没有开始整库恢复。",
            ));
        }

        let restore_id = Uuid::new_v4().to_string();
        let library_key = storage::library_access::library_key(&library_path);
        let task_root = restore_root(&self.recovery_root).join(&restore_id);
        prepare_restore_root(&self.recovery_root)?;
        fs::create_dir(&task_root).map_err(|error| {
            StorageError::new("io", format!("无法创建整库恢复任务目录：{error}"))
        })?;
        ensure_plain_directory(&task_root)?;
        let events = task_root.join(EVENTS_DIRECTORY);
        fs::create_dir(&events).map_err(|error| {
            StorageError::new("io", format!("无法创建整库恢复日志目录：{error}"))
        })?;
        ensure_plain_directory(&events)?;

        context.stage_library_restore_backup(&source, &task_root.join(SOURCE_STAGE), job)?;
        context.stage_library_restore_backup(&before, &task_root.join(BEFORE_STAGE), job)?;
        BackupContext::verify_library_restore_stage(
            &task_root.join(SOURCE_STAGE),
            &source.facts,
            job,
        )?;
        BackupContext::verify_library_restore_stage(
            &task_root.join(BEFORE_STAGE),
            &before.facts,
            job,
        )?;
        let after_stage = BackupContext::verify_library_restore_stage(
            &task_root.join(SOURCE_STAGE),
            &source.facts,
            job,
        );
        after_stage?;
        let before_stage = BackupContext::verify_library_restore_stage(
            &task_root.join(BEFORE_STAGE),
            &before.facts,
            job,
        );
        before_stage?;

        let current = context.inspect_library_restore_facts(job)?;
        if !same_facts(&current, &before.facts) {
            return Err(StorageError::new(
                "backupChanged",
                "笔记库在恢复暂存期间发生变化；没有开始整库恢复。",
            ));
        }
        let source_revalidated = context.verify_library_restore_backup(
            backup_id,
            Some(expected_manifest_revision),
            job,
        )?;
        if !source.same_snapshot(&source_revalidated) {
            return Err(StorageError::new(
                "backupChanged",
                "恢复源备份在暂存期间发生变化；没有开始整库恢复。",
            ));
        }
        storage::ensure_no_pending_publications(&self.recovery_root, &library_path)?;
        job.checkpoint()?;

        let canonical_library = fs::canonicalize(&library_path)
            .map_err(|error| StorageError::new("io", format!("无法确认笔记库路径：{error}")))?;
        let plan = RestorePlan {
            schema_version: 1,
            restore_id: restore_id.clone(),
            library_key: library_key.clone(),
            library_path: canonical_library.to_string_lossy().into_owned(),
            source_backup_id: source.summary.backup_id.clone(),
            source_manifest_revision: source.manifest_revision.clone(),
            source_backup: source.summary.clone(),
            pre_restore_backup: before.summary.clone(),
            before_manifest_revision: before.manifest_revision.clone(),
            before: before.facts.clone(),
            after: source.facts.clone(),
            _task_root: task_root.clone(),
            _recovery_root: self.recovery_root.clone(),
        };
        validate_plan(&plan, &canonical_library)?;
        let plan_bytes = serde_json::to_vec(&plan).map_err(|error| {
            StorageError::new("restoreJournal", format!("无法编码整库恢复计划：{error}"))
        })?;
        if plan_bytes.len() as u64 > MAX_PLAN_BYTES {
            return Err(StorageError::new(
                "restoreTooLarge",
                "整库恢复计划超过安全上限；没有修改笔记库。",
            ));
        }
        write_new_synced(
            &task_root.join(PLAN_FILE),
            &plan_bytes,
            "整库恢复计划",
            "library_restore_record_partial",
        )?;
        sync_directory(&task_root)?;
        sync_directory(&restore_root(&self.recovery_root))?;
        restore_checkpoint(
            "library_restore_plan_ready",
            &canonical_library,
            &task_root.join(PLAN_FILE),
        )?;
        let current = context.inspect_library_restore_facts(job)?;
        if !same_facts(&current, &before.facts) {
            return Err(StorageError::new(
                "backupChanged",
                "笔记库在持久化恢复计划期间发生变化；没有开始整库恢复。",
            ));
        }
        storage::ensure_no_pending_publications(&self.recovery_root, &library_path)?;
        job.checkpoint()?;

        let identity = RestoreIdentity {
            schema_version: 1,
            library_key,
            restore_id,
        };
        write_identity_new(
            &pointer_path(&self.recovery_root, &identity.library_key),
            &identity,
            "整库恢复活动指针",
        )?;
        write_identity_new(
            &library_path.join(storage::library_access::RESTORE_MARKER),
            &identity,
            "整库恢复标记",
        )?;
        sync_directory(&library_path)?;
        self.library_restore = Some(status_for(
            &plan,
            "paused",
            0,
            true,
            true,
            "整库恢复已准备，可继续或回退。",
        ));
        restore_checkpoint(
            "library_restore_marker_published",
            &library_path,
            &library_path.join(storage::library_access::RESTORE_MARKER),
        )?;
        let pending = PendingRestore {
            plan,
            task_root,
            state: JournalState::default(),
            marker_present: true,
        };
        match drive_restore(self, &library_path, pending, RestoreDirection::Forward, job) {
            Ok(receipt) => Ok(receipt),
            Err(error) => Err(error),
        }
    }

    /// Reconciles a durable whole-library restore and either finishes its
    /// source direction or reverses all completed fact operations.
    pub fn resume_library_restore(
        &mut self,
        action: LibraryRestoreAction,
        job: &BackupJob,
    ) -> Result<LibraryRestoreReceipt, WorkspaceError> {
        let library_path = self.library_path()?;
        let _access = storage::library_access::lock_exclusive(&self.recovery_root, &library_path)?;
        let mut pending =
            load_pending_restore(&self.recovery_root, &library_path)?.ok_or_else(|| {
                StorageError::new("noLibraryRestore", "此笔记库没有待处理的整库恢复。")
            })?;
        let pointer = pointer_path(&self.recovery_root, &pending.plan.library_key);
        if !regular_file_exists(&pointer)? {
            let identity = RestoreIdentity {
                schema_version: 1,
                library_key: pending.plan.library_key.clone(),
                restore_id: pending.plan.restore_id.clone(),
            };
            write_identity_new(&pointer, &identity, "整库恢复活动指针")?;
        }
        storage::ensure_no_pending_publications(&self.recovery_root, &library_path)?;

        if pending.state.terminal.is_some() {
            let completed = pending.state.terminal.as_deref() == Some("completed");
            if pending.marker_present {
                validate_stages(&pending, Some(job))?;
            }
            close_terminal(&self.recovery_root, &library_path, &mut pending, job)?;
            return self.finish_workspace_after_restore(
                pending.plan.pre_restore_backup.clone(),
                completed,
            );
        }
        validate_stages(&pending, Some(job))?;
        if !pending.marker_present {
            // The active pointer and validated plan survive a synchronized
            // marker deletion. Explicit resume is the only operation that
            // reinstates the exact stop marker.
            install_identity_marker(&library_path, &pending.plan)?;
            pending.marker_present = true;
        }

        let direction =
            if pending.state.rollback_started || action == LibraryRestoreAction::RollBack {
                RestoreDirection::Rollback
            } else {
                RestoreDirection::Forward
            };
        match drive_restore(self, &library_path, pending, direction, job) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                if self.library_restore.is_none() {
                    self.library_restore = Some(blocked_status(
                        &library_path,
                        "整库恢复未完成；已暂停普通访问，请检查恢复日志和保护备份。",
                    ));
                }
                Err(error)
            }
        }
    }

    /// Reads the local task pointer and exact library marker without trusting
    /// any journal-supplied path.
    pub fn pending_library_restore(
        &self,
        path: &Path,
    ) -> Result<Option<LibraryRestoreStatus>, WorkspaceError> {
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let identity_path = super::resolve_future_path(path)?;
                let key = storage::library_access::library_key(&identity_path);
                let pointer = pointer_path(&self.recovery_root, &key);
                let pointer_exists = match storage::library_access::restore_pointer_present(
                    &self.recovery_root,
                    &identity_path,
                ) {
                    Ok(exists) => exists,
                    Err(error) => {
                        return Ok(Some(blocked_status(
                            &identity_path,
                            &format!(
                                "本机整库恢复指针无法安全检查：{}；已暂停普通访问。",
                                error.message
                            ),
                        )))
                    }
                };
                if !pointer_exists {
                    return Ok(None);
                }
                let message = match read_identity(&pointer, "整库恢复活动指针") {
                    Ok(identity) if identity.library_key == key => {
                        "笔记库目录当前不可用，但本机仍有该库的整库恢复指针；已停止启动扫描与普通访问。请先恢复目录可用性，再继续或回退。"
                    }
                    Ok(_) => {
                        "笔记库目录当前不可用，且本机整库恢复指针身份不匹配；已暂停普通访问。"
                    }
                    Err(_) => {
                        "笔记库目录当前不可用，且本机整库恢复指针损坏；已暂停普通访问。"
                    }
                };
                return Ok(Some(blocked_status(&identity_path, message)));
            }
            Err(error) => {
                return Err(StorageError::new(
                    "io",
                    format!("无法检查整库恢复笔记库：{error}"),
                ))
            }
        };
        let key = storage::library_access::library_key(&canonical);
        let marker_path = canonical.join(storage::library_access::RESTORE_MARKER);
        let pointer = pointer_path(&self.recovery_root, &key);
        let marker_exists = match regular_file_exists(&marker_path) {
            Ok(exists) => exists,
            Err(error) => {
                return Ok(Some(blocked_status(
                    &canonical,
                    &format!(
                        "整库恢复标记无法安全检查：{}；已暂停普通访问。",
                        error.message
                    ),
                )))
            }
        };
        let pointer_exists = match regular_file_exists(&pointer) {
            Ok(exists) => exists,
            Err(error) => {
                return Ok(Some(blocked_status(
                    &canonical,
                    &format!(
                        "本机整库恢复指针无法安全检查：{}；已暂停普通访问。",
                        error.message
                    ),
                )))
            }
        };
        if !marker_exists && !pointer_exists {
            return Ok(None);
        }

        let identity = if marker_exists {
            match read_identity(&marker_path, "整库恢复标记") {
                Ok(identity) => identity,
                Err(error) => {
                    return Ok(Some(blocked_status(
                        &canonical,
                        &format!("{}；已暂停普通访问。", error.message),
                    )))
                }
            }
        } else {
            match read_identity(&pointer, "整库恢复活动指针") {
                Ok(identity) => identity,
                Err(error) => {
                    return Ok(Some(blocked_status(
                        &canonical,
                        &format!("{}；已暂停普通访问。", error.message),
                    )))
                }
            }
        };
        if identity.library_key != key {
            return Ok(Some(blocked_status(
                &canonical,
                "整库恢复标记与当前笔记库身份不一致；已暂停普通访问。",
            )));
        }
        if marker_exists && pointer_exists {
            let pointer_identity = match read_identity(&pointer, "整库恢复活动指针") {
                Ok(identity) => identity,
                Err(error) => {
                    return Ok(Some(blocked_status(
                        &canonical,
                        &format!("{}；已暂停普通访问。", error.message),
                    )))
                }
            };
            if pointer_identity != identity {
                return Ok(Some(blocked_status(
                    &canonical,
                    "整库恢复标记与本机活动指针不一致；已暂停普通访问。",
                )));
            }
        }
        let pending = match load_pending_by_identity(
            &self.recovery_root,
            &canonical,
            identity.clone(),
            marker_exists,
        ) {
            Ok(pending) => pending,
            Err(error) => {
                return Ok(Some(blocked_status(
                    &canonical,
                    &format!(
                        "整库恢复日志或暂存数据无法验证：{}；已暂停普通访问。",
                        error.message
                    ),
                )));
            }
        };
        if pending.state.terminal.is_some() && !pending.marker_present {
            return Ok(Some(status_from_pending(&pending, false)));
        }
        let stage_result = validate_stages(&pending, None);
        if let Err(error) = stage_result {
            return Ok(Some(blocked_status(
                &canonical,
                &format!(
                    "整库恢复源或保护快照无法验证：{}；已暂停普通访问。",
                    error.message
                ),
            )));
        }
        Ok(Some(status_from_pending(&pending, false)))
    }

    fn finish_workspace_after_restore(
        &mut self,
        pre_restore_backup: BackupSummary,
        completed: bool,
    ) -> Result<LibraryRestoreReceipt, WorkspaceError> {
        let path = self.library_path()?;
        let loaded = self.load_existing_library(&path);
        let library = match loaded {
            Ok(library) => library,
            Err(error) => {
                self.session = None;
                self.library = None;
                self.last_note_id = None;
                self.library_restore = None;
                return Err(post_restore_error(
                    error,
                    &pre_restore_backup,
                    "原生文件已恢复，但无法重载笔记库索引",
                ));
            }
        };
        let mut preferences = self.preferences.clone();
        if preferences
            .selected_notebook_id
            .as_ref()
            .is_some_and(|selected| !library.notebooks.contains_key(selected))
        {
            preferences.selected_notebook_id = None;
        }

        let old_session = self.session.take();
        self.last_note_id = None;
        self.library = Some(library);
        self.preferences = preferences.clone();
        self.library_restore = None;
        self.notice = None;
        self.initialized = true;
        if let Some(mut session) = old_session {
            if let Err(error) = session.release_asset_stash() {
                return Err(post_restore_error(
                    error,
                    &pre_restore_backup,
                    "原生文件已恢复，但旧编辑会话资源无法释放",
                ));
            }
        }
        if let Err(error) = self.asset_run.release() {
            return Err(post_restore_error(
                error,
                &pre_restore_backup,
                "原生文件已恢复，但旧资源暂存无法释放",
            ));
        }
        if let Err(error) = self.persist_preference(&path, None, &preferences) {
            return Err(post_restore_error(
                error,
                &pre_restore_backup,
                "原生文件已恢复，但本机笔记库设置未能更新",
            ));
        }
        self.refresh_selected_library().map_err(|error| {
            post_restore_error(
                error,
                &pre_restore_backup,
                "原生文件已恢复，但派生索引刷新未完成",
            )
        })?;
        Ok(LibraryRestoreReceipt {
            workspace: self.view(),
            pre_restore_backup,
            message: if completed {
                "已从所选备份恢复整库原生文件；恢复前的完整手动备份已保留。".into()
            } else {
                "已回退到恢复前的完整原生文件集合；保护手动备份仍已保留。".into()
            },
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RestoreDirection {
    Forward,
    Rollback,
}

fn drive_restore(
    workspace: &mut Workspace,
    library_path: &Path,
    mut pending: PendingRestore,
    direction: RestoreDirection,
    job: &BackupJob,
) -> Result<LibraryRestoreReceipt, WorkspaceError> {
    validate_stages(&pending, Some(job))?;
    if direction == RestoreDirection::Rollback && !pending.state.rollback_started {
        let entry = JournalEntry {
            schema_version: 1,
            restore_id: pending.plan.restore_id.clone(),
            step: None,
            state: Some("rollingBack".into()),
        };
        write_journal_event(&pending, "rollback-started.json", &entry)?;
        pending.state.rollback_started = true;
        restore_checkpoint(
            "library_restore_rollback_started",
            &pending.plan.library_path_as_path()?,
            &pending
                .task_root
                .join(EVENTS_DIRECTORY)
                .join("rollback-started.json"),
        )?;
    }
    let result = match direction {
        RestoreDirection::Forward if !pending.state.rollback_started => {
            drive_forward(library_path, &mut pending, job)
        }
        RestoreDirection::Forward | RestoreDirection::Rollback => {
            drive_rollback(library_path, &mut pending, job)
        }
    };
    if let Err(error) = result {
        let blocked = error.kind == "conflict"
            || error.kind == "restoreJournal"
            || error.kind == "restoreStageChanged";
        workspace.library_restore = Some(status_from_pending(&pending, blocked));
        return Err(if blocked {
            with_recovery_guidance(error, &pending.plan.pre_restore_backup)
        } else {
            with_recovery_guidance(error, &pending.plan.pre_restore_backup)
        });
    }
    let completed = pending
        .state
        .terminal
        .as_deref()
        .ok_or_else(|| journal_error("整库恢复没有持久化终态。"))?
        == "completed";
    close_terminal(
        &workspace.recovery_root,
        &workspace.library_path()?,
        &mut pending,
        job,
    )?;
    workspace.library_restore = None;
    workspace.finish_workspace_after_restore(pending.plan.pre_restore_backup.clone(), completed)
}

fn drive_forward(
    library_path: &Path,
    pending: &mut PendingRestore,
    job: &BackupJob,
) -> Result<(), WorkspaceError> {
    let steps = make_steps(&pending.plan, RestoreDirection::Forward)?;
    let total = steps.len();
    let mut completed = 0usize;
    job.report_progress(BackupPhase::Restoring, 0, total);
    validate_intermediate_collection(library_path, &pending.plan, job)?;
    // Check every step that has not been durably claimed before changing any
    // fact. A source-only target can otherwise be an external write of the
    // exact source bytes; detecting it only when its sorted step is reached
    // would leave earlier steps partially restored.
    for (index, step) in steps.iter().enumerate() {
        if !pending.state.forward_intents.contains(&index) {
            ensure_unclaimed_step_is_untouched(step)?;
        }
    }
    for (index, step) in steps.iter().enumerate() {
        job.checkpoint()?;
        if !pending.state.forward_intents.contains(&index) {
            ensure_unclaimed_step_is_untouched(step)?;
        }
        let is_complete = storage::restore_facts::inspect_step(step)?;
        if pending.state.forward_done.contains(&index) && !is_complete {
            return Err(StorageError::new(
                "conflict",
                format!(
                    "已记录完成的事实 {} 随后发生变化；没有覆盖该版本。",
                    step.target.display()
                ),
            ));
        }
        if !is_complete && !pending.state.forward_intents.contains(&index) {
            let event = JournalEntry {
                schema_version: 1,
                restore_id: pending.plan.restore_id.clone(),
                step: Some(index),
                state: None,
            };
            write_journal_event(pending, &event_name(index, "forward-intent"), &event)?;
            pending.state.forward_intents.insert(index);
            restore_checkpoint(
                "library_restore_forward_intent",
                &pending.plan.library_path_as_path()?,
                &pending
                    .task_root
                    .join(EVENTS_DIRECTORY)
                    .join(event_name(index, "forward-intent")),
            )?;
        }
        if !is_complete {
            storage::restore_facts::publish_fact(step)?;
            if !storage::restore_facts::inspect_step(step)? {
                return Err(StorageError::new(
                    "conflict",
                    format!(
                        "恢复事实 {} 尚未达到计划版本；任务证据已保留。",
                        step.target.display()
                    ),
                ));
            }
        }
        if !pending.state.forward_done.contains(&index) {
            let event = JournalEntry {
                schema_version: 1,
                restore_id: pending.plan.restore_id.clone(),
                step: Some(index),
                state: None,
            };
            write_journal_event(pending, &event_name(index, "forward-done"), &event)?;
            pending.state.forward_done.insert(index);
            restore_checkpoint(
                "library_restore_forward_done",
                &pending.plan.library_path_as_path()?,
                &pending
                    .task_root
                    .join(EVENTS_DIRECTORY)
                    .join(event_name(index, "forward-done")),
            )?;
        }
        completed += 1;
        job.report_progress(BackupPhase::Restoring, completed, total);
    }
    let actual = BackupContext::inspect_library_restore_facts_at(library_path, job)?;
    if !same_facts(&actual, &pending.plan.after) {
        return Err(StorageError::new(
            "conflict",
            "恢复后的原生文件集合与已验证源备份不一致；任务证据和恢复前备份均已保留。",
        ));
    }
    write_terminal(pending, "completed")?;
    Ok(())
}

fn drive_rollback(
    library_path: &Path,
    pending: &mut PendingRestore,
    job: &BackupJob,
) -> Result<(), WorkspaceError> {
    let forward_steps = make_steps(&pending.plan, RestoreDirection::Forward)?;
    let total = forward_steps.len();
    job.report_progress(BackupPhase::RollingBack, 0, total);
    // As in the forward direction, reject any unclaimed external change
    // before reversing even one previously claimed step.
    for (index, forward) in forward_steps.iter().enumerate() {
        if !pending.state.forward_intents.contains(&index) {
            if pending.state.rollback_intents.contains(&index)
                || pending.state.rollback_done.contains(&index)
            {
                return Err(StorageError::new(
                    "restoreJournal",
                    "回退日志记录了没有恢复意图的事实；没有改动该文件。",
                ));
            }
            ensure_unclaimed_step_is_untouched(forward)?;
        }
    }
    let mut completed = 0usize;
    for (index, forward) in forward_steps.iter().enumerate().rev() {
        job.checkpoint()?;
        let inverse = inverse_step(&pending.plan, index, &forward)?;
        if !pending.state.forward_intents.contains(&index) {
            if pending.state.rollback_intents.contains(&index)
                || pending.state.rollback_done.contains(&index)
            {
                return Err(StorageError::new(
                    "restoreJournal",
                    "回退日志记录了没有恢复意图的事实；没有改动该文件。",
                ));
            }
            ensure_unclaimed_step_is_untouched(&forward)?;
            continue;
        }
        if pending.state.rollback_done.contains(&index) {
            if !storage::restore_facts::inspect_step(&inverse)? {
                return Err(StorageError::new(
                    "conflict",
                    format!(
                        "已记录回退完成的事实 {} 随后发生变化；没有覆盖该版本。",
                        inverse.target.display()
                    ),
                ));
            }
            completed += 1;
            continue;
        }
        if pending.state.rollback_intents.contains(&index) {
            storage::restore_facts::publish_fact(&inverse)?;
            if !storage::restore_facts::inspect_step(&inverse)? {
                return Err(StorageError::new(
                    "conflict",
                    "已开始的事实回退尚未达到原始版本。",
                ));
            }
        } else {
            let mut forward_complete = storage::restore_facts::inspect_step(forward)?;
            if !forward_complete && pending.state.forward_intents.contains(&index) {
                storage::restore_facts::publish_fact(forward)?;
                forward_complete = storage::restore_facts::inspect_step(forward)?;
                if !forward_complete {
                    return Err(StorageError::new(
                        "conflict",
                        "已开始的恢复事实无法安全完成后再回退。",
                    ));
                }
                if !pending.state.forward_done.contains(&index) {
                    let done = JournalEntry {
                        schema_version: 1,
                        restore_id: pending.plan.restore_id.clone(),
                        step: Some(index),
                        state: None,
                    };
                    write_journal_event(pending, &event_name(index, "forward-done"), &done)?;
                    pending.state.forward_done.insert(index);
                    restore_checkpoint(
                        "library_restore_forward_done",
                        &pending.plan.library_path_as_path()?,
                        &pending
                            .task_root
                            .join(EVENTS_DIRECTORY)
                            .join(event_name(index, "forward-done")),
                    )?;
                }
            }
            if !forward_complete {
                continue;
            }
            let event = JournalEntry {
                schema_version: 1,
                restore_id: pending.plan.restore_id.clone(),
                step: Some(index),
                state: None,
            };
            write_journal_event(pending, &event_name(index, "rollback-intent"), &event)?;
            pending.state.rollback_intents.insert(index);
            restore_checkpoint(
                "library_restore_rollback_intent",
                &pending.plan.library_path_as_path()?,
                &pending
                    .task_root
                    .join(EVENTS_DIRECTORY)
                    .join(event_name(index, "rollback-intent")),
            )?;
            storage::restore_facts::publish_fact(&inverse)?;
            if !storage::restore_facts::inspect_step(&inverse)? {
                return Err(StorageError::new(
                    "conflict",
                    format!(
                        "回退事实 {} 尚未恢复到原始版本；任务证据已保留。",
                        inverse.target.display()
                    ),
                ));
            }
        }
        if !pending.state.rollback_done.contains(&index) {
            let event = JournalEntry {
                schema_version: 1,
                restore_id: pending.plan.restore_id.clone(),
                step: Some(index),
                state: None,
            };
            write_journal_event(pending, &event_name(index, "rollback-done"), &event)?;
            pending.state.rollback_done.insert(index);
            restore_checkpoint(
                "library_restore_rollback_done",
                &pending.plan.library_path_as_path()?,
                &pending
                    .task_root
                    .join(EVENTS_DIRECTORY)
                    .join(event_name(index, "rollback-done")),
            )?;
        }
        completed += 1;
        job.report_progress(BackupPhase::RollingBack, completed, total);
    }
    let actual = BackupContext::inspect_library_restore_facts_at(library_path, job)?;
    if !same_facts(&actual, &pending.plan.before) {
        return Err(StorageError::new(
            "conflict",
            "回退后的原生文件集合与恢复前快照不一致；任务证据和保护备份均已保留。",
        ));
    }
    write_terminal(pending, "rolledBack")?;
    Ok(())
}

fn validate_intermediate_collection(
    library_path: &Path,
    plan: &RestorePlan,
    job: &BackupJob,
) -> Result<(), WorkspaceError> {
    let actual = BackupContext::inspect_library_restore_facts_at(library_path, job)?;
    let actual_by_path = fact_map(&actual);
    let before = fact_map(&plan.before);
    let after = fact_map(&plan.after);
    let mut allowed = HashSet::new();
    for fact in plan.before.iter().chain(plan.after.iter()) {
        allowed.insert(fact.path.as_str());
    }
    for fact in &actual {
        if !allowed.contains(fact.path.as_str()) {
            return Err(StorageError::new(
                "conflict",
                format!(
                    "恢复期间发现计划之外的新原生文件 {}；未覆盖该文件。",
                    fact.path
                ),
            ));
        }
        let old = before.get(fact.path.as_str()).copied();
        let new = after.get(fact.path.as_str()).copied();
        if ![old, new]
            .into_iter()
            .flatten()
            .any(|expected| expected.length == fact.length && expected.sha256 == fact.sha256)
        {
            return Err(StorageError::new(
                "conflict",
                format!(
                    "恢复期间原生文件 {} 出现第三个版本；没有覆盖该版本。",
                    fact.path
                ),
            ));
        }
    }
    for (path, old) in &before {
        if after
            .get(path)
            .is_some_and(|new| old.length == new.length && old.sha256 == new.sha256)
            && !actual_by_path
                .get(path)
                .is_some_and(|fact| fact.length == old.length && fact.sha256 == old.sha256)
        {
            return Err(StorageError::new(
                "conflict",
                format!("恢复期间未变化的原生文件 {} 缺失；没有忽略该缺失。", path),
            ));
        }
    }
    Ok(())
}

fn ensure_unclaimed_step_is_untouched(step: &RawFactStep) -> Result<(), WorkspaceError> {
    let target = storage::restore_facts::observe_fact(&step.target)?;
    let artifacts_exist = [&step.candidate, &step.displaced]
        .into_iter()
        .map(|path| match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(StorageError::new(
                "io",
                format!("无法检查尚未认领的恢复事实工件：{error}"),
            )),
            Ok(_) => Ok(true),
        })
        .collect::<Result<Vec<_>, WorkspaceError>>()?
        .into_iter()
        .any(|exists| exists);
    if target != step.before || artifacts_exist {
        return Err(StorageError::new(
            "conflict",
            format!(
                "事实 {} 在恢复日志认领之前已发生变化；没有覆盖或删除该版本。",
                step.target.display()
            ),
        ));
    }
    Ok(())
}

fn make_steps(
    plan: &RestorePlan,
    direction: RestoreDirection,
) -> Result<Vec<RawFactStep>, WorkspaceError> {
    let before = fact_map(&plan.before);
    let after = fact_map(&plan.after);
    let keys = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut steps = Vec::new();
    let mut index = 0usize;
    for path in keys {
        let old = before.get(path).copied();
        let new = after.get(path).copied();
        if old == new {
            continue;
        }
        let (from, to) = match direction {
            RestoreDirection::Forward => (old, new),
            RestoreDirection::Rollback => (new, old),
        };
        let target = plan
            .library_path_as_path()?
            .join(validated_relative_path(path)?);
        let parent = target
            .parent()
            .ok_or_else(|| journal_error("事实目标缺少父目录。"))?;
        let direction_name = if direction == RestoreDirection::Forward {
            "forward"
        } else {
            "rollback"
        };
        let stem = format!(
            ".cuenote-restore-{}-{index:06}-{direction_name}",
            plan.restore_id
        );
        let candidate = parent.join(format!("{stem}-candidate.tmp"));
        let displaced = parent.join(format!("{stem}-displaced.tmp"));
        let source = match (direction, to) {
            (RestoreDirection::Forward, Some(_)) => Some(
                task_root_from_plan(plan)
                    .join(SOURCE_STAGE)
                    .join(validated_relative_path(path)?),
            ),
            (RestoreDirection::Rollback, Some(_)) => Some(
                task_root_from_plan(plan)
                    .join(BEFORE_STAGE)
                    .join(validated_relative_path(path)?),
            ),
            (_, None) => None,
        };
        steps.push(RawFactStep {
            target,
            recovery_root: recovery_root_from_plan(plan),
            before: from.map(to_digest),
            after: to.map(to_digest),
            source,
            candidate,
            displaced,
        });
        index += 1;
    }
    Ok(steps)
}

fn inverse_step(
    plan: &RestorePlan,
    index: usize,
    forward: &RawFactStep,
) -> Result<RawFactStep, WorkspaceError> {
    let path = relative_to_library(&plan.library_path, &forward.target)?;
    let name = format!(".cuenote-restore-{}-{index:06}-rollback", plan.restore_id);
    let parent = forward
        .target
        .parent()
        .ok_or_else(|| journal_error("回退目标缺少父目录。"))?;
    let source = forward.before.as_ref().map(|_| {
        task_root_from_plan(plan)
            .join(BEFORE_STAGE)
            .join(validated_relative_path(&path).expect("plan was validated"))
    });
    Ok(RawFactStep {
        target: forward.target.clone(),
        recovery_root: recovery_root_from_plan(plan),
        before: forward.after.clone(),
        after: forward.before.clone(),
        source,
        candidate: parent.join(format!("{name}-candidate.tmp")),
        displaced: parent.join(format!("{name}-displaced.tmp")),
    })
}

fn fact_map(facts: &[NativeLibraryFact]) -> BTreeMap<&str, &NativeLibraryFact> {
    facts
        .iter()
        .map(|fact| (fact.path.as_str(), fact))
        .collect()
}

fn same_facts(left: &[NativeLibraryFact], right: &[NativeLibraryFact]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let right = fact_map(right);
    left.iter().all(|fact| {
        right
            .get(fact.path.as_str())
            .is_some_and(|other| fact.length == other.length && fact.sha256 == other.sha256)
    })
}

fn to_digest(fact: &NativeLibraryFact) -> FactDigest {
    FactDigest {
        sha256: fact.sha256.clone(),
        length: fact.length,
    }
}

fn validate_plan(plan: &RestorePlan, library: &Path) -> Result<(), WorkspaceError> {
    if plan.schema_version != 1
        || !is_uuid(&plan.restore_id)
        || !is_uuid(&plan.source_backup_id)
        || plan.source_backup.backup_id != plan.source_backup_id
        || plan.pre_restore_backup.kind != BackupKind::Manual
        || !is_sha256(&plan.library_key)
        || !is_sha256(&plan.source_manifest_revision)
        || !is_sha256(&plan.before_manifest_revision)
        || plan.library_key != storage::library_access::library_key(library)
        || plan.library_path != library.to_string_lossy()
    {
        return Err(journal_error("整库恢复计划身份、版本或来源信息无效。"));
    }
    validate_fact_list(&plan.before)?;
    validate_fact_list(&plan.after)?;
    if plan.before.len().saturating_add(plan.after.len()) > MAX_TOTAL_FACTS.saturating_mul(2) {
        return Err(journal_error("整库恢复计划文件数量超过安全上限。"));
    }
    Ok(())
}

fn validate_fact_list(facts: &[NativeLibraryFact]) -> Result<(), WorkspaceError> {
    if facts.len() > MAX_TOTAL_FACTS {
        return Err(journal_error("整库恢复事实文件数量超过安全上限。"));
    }
    let mut previous: Option<&str> = None;
    let mut folded = HashSet::new();
    for fact in facts {
        validated_relative_path(&fact.path)?;
        if fact.length > crate::note_format::MAX_PACKAGE_BYTES
            || !is_sha256(&fact.sha256)
            || !folded.insert(fact.path.to_lowercase())
            || previous.is_some_and(|previous| previous >= fact.path.as_str())
        {
            return Err(journal_error("整库恢复事实清单排序、身份或大小无效。"));
        }
        previous = Some(&fact.path);
    }
    Ok(())
}

fn validated_relative_path(value: &str) -> Result<PathBuf, WorkspaceError> {
    if value.is_empty() || value.len() > 512 || value.contains('\\') || value.contains(':') {
        return Err(journal_error("整库恢复文件路径无效。"));
    }
    let components = Path::new(value).components().collect::<Vec<_>>();
    if components.len() != 2
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(journal_error(
            "整库恢复路径必须是 notes/ 或 notebooks/ 的直接文件。",
        ));
    }
    let directory = components[0].as_os_str().to_str().unwrap_or_default();
    let filename = components[1].as_os_str().to_str().unwrap_or_default();
    let expected_extension = match directory {
        "notes" => "cuenote",
        "notebooks" => "json",
        _ => return Err(journal_error("整库恢复路径包含未知原生目录。")),
    };
    if filename.is_empty()
        || filename.len() > 255
        || filename == "."
        || filename == ".."
        || Path::new(filename)
            .extension()
            .and_then(|extension| extension.to_str())
            != Some(expected_extension)
        || filename.contains('/')
    {
        return Err(journal_error("整库恢复原生文件名无效。"));
    }
    Ok(PathBuf::from(directory).join(filename))
}

fn task_root_from_plan(plan: &RestorePlan) -> PathBuf {
    // This field is not serialized in the plan; staging paths are resolved by
    // the fixed task directory in callers. This helper is replaced at load by
    // a process-local binding through the reserved field below.
    PathBuf::from(&plan._task_root)
}

fn recovery_root_from_plan(plan: &RestorePlan) -> PathBuf {
    PathBuf::from(&plan._recovery_root)
}

fn status_for(
    plan: &RestorePlan,
    phase: &str,
    completed: usize,
    can_continue: bool,
    can_roll_back: bool,
    message: &str,
) -> LibraryRestoreStatus {
    LibraryRestoreStatus {
        restore_id: plan.restore_id.clone(),
        library_path: super::display_path(Path::new(&plan.library_path)),
        source_backup_id: plan.source_backup_id.clone(),
        pre_restore_backup: Some(plan.pre_restore_backup.clone()),
        phase: phase.into(),
        completed_files: completed,
        total_files: step_count(plan),
        can_continue,
        can_roll_back,
        message: message.into(),
    }
}

fn status_from_pending(pending: &PendingRestore, blocked: bool) -> LibraryRestoreStatus {
    let terminal = pending.state.terminal.is_some();
    let phase = if blocked {
        "blocked"
    } else if terminal {
        "paused"
    } else if pending.state.rollback_started {
        "rollingBack"
    } else {
        "paused"
    };
    let completed = if pending.state.rollback_started {
        pending.state.rollback_done.len()
    } else {
        pending.state.forward_done.len()
    };
    status_for(
        &pending.plan,
        phase,
        completed,
        !blocked,
        !blocked && !pending.state.rollback_started && !terminal,
        if blocked {
            "整库恢复状态需要检查；已暂停普通访问，原始文件和保护备份均保留。"
        } else if terminal {
            "整库事实已达到持久化终态，等待关闭恢复标记；继续操作不会重新解释后续外部更改。"
        } else if pending.state.rollback_started {
            "整库恢复正在回退；只能继续完成回退。"
        } else if !pending.marker_present {
            "恢复标记缺失；显式继续或回退将先按已验证计划重建标记，不会自动改动事实。"
        } else {
            "整库恢复尚未完成；可以继续恢复或回退到保护快照。"
        },
    )
}

fn blocked_status(library: &Path, message: &str) -> LibraryRestoreStatus {
    LibraryRestoreStatus {
        restore_id: String::new(),
        library_path: super::display_path(library),
        source_backup_id: String::new(),
        pre_restore_backup: None,
        phase: "blocked".into(),
        completed_files: 0,
        total_files: 0,
        can_continue: false,
        can_roll_back: false,
        message: message.into(),
    }
}

fn step_count(plan: &RestorePlan) -> usize {
    let before = fact_map(&plan.before);
    let after = fact_map(&plan.after);
    before
        .keys()
        .chain(after.keys())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .count()
}

fn restore_root(recovery_root: &Path) -> PathBuf {
    recovery_root.join(RESTORE_ROOT_NAME)
}

fn pointer_path(recovery_root: &Path, library_key: &str) -> PathBuf {
    restore_root(recovery_root).join(format!("{POINTER_PREFIX}{library_key}.json"))
}

fn prepare_restore_root(recovery_root: &Path) -> Result<(), WorkspaceError> {
    fs::create_dir_all(recovery_root)
        .map_err(|error| StorageError::new("io", format!("无法准备本机恢复目录：{error}")))?;
    storage::ensure_local_directory(recovery_root)?;
    let root = restore_root(recovery_root);
    match fs::create_dir(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(StorageError::new(
                "io",
                format!("无法创建整库恢复索引目录：{error}"),
            ))
        }
    }
    ensure_plain_directory(&root)?;
    Ok(())
}

fn ensure_plain_directory(path: &Path) -> Result<(), WorkspaceError> {
    storage::ensure_local_directory(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查恢复目录：{error}")))?;
    if !metadata.is_dir() || super::is_link_or_reparse(&metadata) {
        return Err(StorageError::new(
            "unsafePath",
            "整库恢复目录不能是链接或重解析点。",
        ));
    }
    Ok(())
}

fn write_identity_new(
    path: &Path,
    identity: &RestoreIdentity,
    label: &str,
) -> Result<(), WorkspaceError> {
    let bytes = serde_json::to_vec(identity)
        .map_err(|error| journal_error(&format!("无法编码{label}：{error}")))?;
    if bytes.len() as u64 > MAX_IDENTITY_BYTES {
        return Err(journal_error("整库恢复身份文件超过大小上限。"));
    }
    let partial_stage = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if name == storage::library_access::RESTORE_MARKER => {
            "library_restore_marker_partial"
        }
        Some(name) if name.starts_with(POINTER_PREFIX) => "library_restore_pointer_partial",
        _ => "library_restore_record_partial",
    };
    write_new_synced(path, &bytes, label, partial_stage)
}

fn install_identity_marker(library: &Path, plan: &RestorePlan) -> Result<(), WorkspaceError> {
    let identity = RestoreIdentity {
        schema_version: 1,
        library_key: plan.library_key.clone(),
        restore_id: plan.restore_id.clone(),
    };
    write_identity_new(
        &library.join(storage::library_access::RESTORE_MARKER),
        &identity,
        "整库恢复标记",
    )?;
    sync_directory(library)
}

fn write_new_synced(
    path: &Path,
    bytes: &[u8],
    label: &str,
    partial_stage: &str,
) -> Result<(), WorkspaceError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| journal_error("整库恢复记录没有文件名。"))?;
    let requested_parent = path
        .parent()
        .ok_or_else(|| journal_error("整库恢复记录没有父目录。"))?;
    let parent = fs::canonicalize(requested_parent)
        .map_err(|error| StorageError::new("io", format!("无法解析{label}所在目录：{error}")))?;
    let directory = storage::StableDirectory::open(&parent).map_err(|error| {
        StorageError::new(
            "unsafePath",
            format!("无法固定{label}所在目录：{}", error.message),
        )
    })?;
    directory.ensure_current().map_err(|error| {
        StorageError::new(
            "unsafePath",
            format!("{label}所在目录已变化：{}", error.message),
        )
    })?;

    let destination = parent.join(file_name);
    let temporary = parent.join(format!(
        ".{}.{}.partial.tmp",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| StorageError::new("io", format!("无法创建{label}临时记录：{error}")))?;

    // A task-owned, bounded partial file may remain after interruption. It is
    // never parsed as a record; only its exact temporary naming pattern is
    // ignored when reading the append-only event directory.
    let split = (bytes.len() / 2).min(bytes.len().saturating_sub(1));
    file.write_all(&bytes[..split])
        .map_err(|error| StorageError::new("io", format!("无法写入{label}临时记录：{error}")))?;
    file.sync_all()
        .map_err(|error| StorageError::new("io", format!("无法同步{label}临时记录：{error}")))?;
    restore_checkpoint("library_restore_record_partial", &destination, &temporary)?;
    restore_checkpoint(partial_stage, &destination, &temporary)?;

    file.write_all(&bytes[split..])
        .map_err(|error| StorageError::new("io", format!("无法写入{label}临时记录：{error}")))?;
    file.sync_all()
        .map_err(|error| StorageError::new("io", format!("无法同步{label}临时记录：{error}")))?;
    drop(file);

    directory.ensure_current().map_err(|error| {
        StorageError::new(
            "unsafePath",
            format!("{label}所在目录已变化：{}", error.message),
        )
    })?;
    directory
        .move_no_replace(&temporary, &destination)
        .map_err(|error| StorageError::new("io", format!("无法无覆盖发布{label}记录：{error}")))?;
    directory
        .sync_publication_directory()
        .map_err(|error| StorageError::new("io", format!("无法同步{label}所在目录：{error}")))?;
    Ok(())
}

fn write_journal_event(
    pending: &PendingRestore,
    name: &str,
    event: &JournalEntry,
) -> Result<(), WorkspaceError> {
    let path = pending.task_root.join(EVENTS_DIRECTORY).join(name);
    let bytes = serde_json::to_vec(event)
        .map_err(|error| journal_error(&format!("无法编码整库恢复日志：{error}")))?;
    let partial_stage = match name {
        "rollback-started.json" => "library_restore_rollback_started_partial",
        "terminal.json" => "library_restore_terminal_partial",
        "closed.json" => "library_restore_closed_partial",
        _ if name.ends_with("-forward-intent.json") => "library_restore_intent_partial",
        _ if name.ends_with("-forward-done.json") => "library_restore_done_partial",
        _ if name.ends_with("-rollback-intent.json") => "library_restore_rollback_intent_partial",
        _ if name.ends_with("-rollback-done.json") => "library_restore_rollback_done_partial",
        _ => "library_restore_record_partial",
    };
    write_new_synced(&path, &bytes, "整库恢复日志", partial_stage)
}

fn event_name(index: usize, suffix: &str) -> String {
    format!("{index:06}-{suffix}.json")
}

fn write_terminal(pending: &mut PendingRestore, state: &str) -> Result<(), WorkspaceError> {
    if pending.state.terminal.as_deref() == Some(state) {
        return Ok(());
    }
    let event = JournalEntry {
        schema_version: 1,
        restore_id: pending.plan.restore_id.clone(),
        step: None,
        state: Some(state.into()),
    };
    write_journal_event(pending, "terminal.json", &event)?;
    pending.state.terminal = Some(state.into());
    restore_checkpoint(
        "library_restore_terminal",
        &pending.plan.library_path_as_path()?,
        &pending
            .task_root
            .join(EVENTS_DIRECTORY)
            .join("terminal.json"),
    )?;
    Ok(())
}

fn close_terminal(
    recovery_root: &Path,
    library: &Path,
    pending: &mut PendingRestore,
    job: &BackupJob,
) -> Result<(), WorkspaceError> {
    let terminal = pending
        .state
        .terminal
        .as_deref()
        .ok_or_else(|| journal_error("恢复任务尚无终态，不能关闭。"))?;
    let marker = library.join(storage::library_access::RESTORE_MARKER);
    if regular_file_exists(&marker)? {
        let identity = read_identity(&marker, "整库恢复标记")?;
        if identity.library_key != pending.plan.library_key
            || identity.restore_id != pending.plan.restore_id
        {
            return Err(journal_error(
                "整库恢复标记已被其他任务替换；未删除该标记。",
            ));
        }
        let expected = if terminal == "completed" {
            &pending.plan.after
        } else {
            &pending.plan.before
        };
        let actual = BackupContext::inspect_library_restore_facts_at(library, job)?;
        if !same_facts(&actual, expected) {
            return Err(StorageError::new(
                "conflict",
                "整库恢复已记录终态，但标记仍在且原生文件集合已变化；未移除标记或覆盖外部版本。",
            ));
        }
        fs::remove_file(&marker).map_err(|error| {
            StorageError::new("io", format!("无法移除已完成的整库恢复标记：{error}"))
        })?;
        sync_directory(library)?;
        restore_checkpoint("library_restore_marker_removed", library, &marker)?;
    }
    if !pending.state.closed {
        let event = JournalEntry {
            schema_version: 1,
            restore_id: pending.plan.restore_id.clone(),
            step: None,
            state: Some(terminal.into()),
        };
        write_journal_event(pending, "closed.json", &event)?;
        pending.state.closed = true;
        restore_checkpoint(
            "library_restore_closed",
            library,
            &pending.task_root.join(EVENTS_DIRECTORY).join("closed.json"),
        )?;
    }
    remove_exact_pointer(recovery_root, &pending.plan)?;
    Ok(())
}

fn remove_exact_pointer(recovery_root: &Path, plan: &RestorePlan) -> Result<(), WorkspaceError> {
    let path = pointer_path(recovery_root, &plan.library_key);
    if regular_file_exists(&path)? {
        let identity = read_identity(&path, "整库恢复活动指针")?;
        if identity.library_key != plan.library_key || identity.restore_id != plan.restore_id {
            return Err(journal_error(
                "本机整库恢复活动指针已变化；未删除其他任务指针。",
            ));
        }
        fs::remove_file(&path).map_err(|error| {
            StorageError::new("io", format!("无法移除已关闭的整库恢复指针：{error}"))
        })?;
        sync_directory(&restore_root(recovery_root))?;
        restore_checkpoint(
            "library_restore_pointer_removed",
            &PathBuf::from(&plan.library_path),
            &path,
        )?;
    }
    Ok(())
}

fn restore_checkpoint(
    stage: &str,
    library_path: &Path,
    artifact: &Path,
) -> Result<(), WorkspaceError> {
    crate::storage_faults::checkpoint(stage, library_path, Some(artifact))
}

fn regular_file_exists(path: &Path) -> Result<bool, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::new(
            "io",
            format!("无法检查整库恢复状态：{error}"),
        )),
        Ok(metadata) if metadata.is_file() && !super::is_link_or_reparse(&metadata) => Ok(true),
        Ok(_) => Err(journal_error("整库恢复身份路径不是本机普通文件。")),
    }
}

fn read_identity(path: &Path, label: &str) -> Result<RestoreIdentity, WorkspaceError> {
    let bytes = read_local_bounded(path, MAX_IDENTITY_BYTES, label)?;
    let identity: RestoreIdentity = serde_json::from_slice(&bytes)
        .map_err(|error| journal_error(&format!("{label}格式无效：{error}")))?;
    if identity.schema_version != 1
        || !is_sha256(&identity.library_key)
        || !is_uuid(&identity.restore_id)
    {
        return Err(journal_error(&format!("{label}身份字段无效。")));
    }
    Ok(identity)
}

fn read_local_bounded(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, WorkspaceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查{label}：{error}")))?;
    if !metadata.is_file() || super::is_link_or_reparse(&metadata) || metadata.len() > limit {
        return Err(journal_error(&format!("{label}不是大小受限的普通文件。")));
    }
    let file = storage::open_local_read(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::new("io", format!("无法读取{label}：{error}")))?;
    if bytes.len() as u64 > limit || bytes.len() as u64 != metadata.len() {
        return Err(journal_error(&format!(
            "{label}读取期间发生变化或超过大小上限。"
        )));
    }
    Ok(bytes)
}

fn load_pending_restore(
    recovery_root: &Path,
    library: &Path,
) -> Result<Option<PendingRestore>, WorkspaceError> {
    let canonical = fs::canonicalize(library)
        .map_err(|error| StorageError::new("io", format!("无法解析整库恢复笔记库：{error}")))?;
    let key = storage::library_access::library_key(&canonical);
    let marker = canonical.join(storage::library_access::RESTORE_MARKER);
    let pointer = pointer_path(recovery_root, &key);
    let marker_exists = regular_file_exists(&marker)?;
    let pointer_exists = regular_file_exists(&pointer)?;
    if !marker_exists && !pointer_exists {
        return Ok(None);
    }
    let identity = if marker_exists {
        read_identity(&marker, "整库恢复标记")?
    } else {
        read_identity(&pointer, "整库恢复活动指针")?
    };
    if identity.library_key != key {
        return Err(journal_error("整库恢复身份与当前笔记库不一致。"));
    }
    if marker_exists && pointer_exists && read_identity(&pointer, "整库恢复活动指针")? != identity
    {
        return Err(journal_error("整库恢复标记与活动指针不一致。"));
    }
    load_pending_by_identity(recovery_root, &canonical, identity, marker_exists).map(Some)
}

fn load_pending_by_identity(
    recovery_root: &Path,
    library: &Path,
    identity: RestoreIdentity,
    marker_present: bool,
) -> Result<PendingRestore, WorkspaceError> {
    prepare_existing_restore_root(recovery_root)?;
    let task_root = restore_root(recovery_root).join(&identity.restore_id);
    ensure_plain_directory(&task_root)?;
    validate_task_layout(&task_root)?;
    let bytes = read_local_bounded(&task_root.join(PLAN_FILE), MAX_PLAN_BYTES, "整库恢复计划")?;
    let mut plan: RestorePlan = serde_json::from_slice(&bytes)
        .map_err(|error| journal_error(&format!("整库恢复计划格式无效：{error}")))?;
    plan.bind_runtime_paths(task_root.clone(), recovery_root.to_path_buf());
    validate_plan(&plan, library)?;
    if plan.restore_id != identity.restore_id || plan.library_key != identity.library_key {
        return Err(journal_error("活动标记与持久化恢复计划身份不一致。"));
    }
    let state = read_journal_state(&task_root, &plan)?;
    Ok(PendingRestore {
        plan,
        task_root,
        state,
        marker_present,
    })
}

fn prepare_existing_restore_root(recovery_root: &Path) -> Result<(), WorkspaceError> {
    ensure_plain_directory(recovery_root)?;
    ensure_plain_directory(&restore_root(recovery_root))
}

fn validate_task_layout(task_root: &Path) -> Result<(), WorkspaceError> {
    let expected = [PLAN_FILE, SOURCE_STAGE, BEFORE_STAGE, EVENTS_DIRECTORY]
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut actual = HashSet::new();
    for entry in fs::read_dir(task_root).map_err(|error| {
        StorageError::new(
            "restoreJournal",
            format!("无法读取整库恢复任务目录：{error}"),
        )
    })? {
        let entry = entry.map_err(|error| {
            StorageError::new("restoreJournal", format!("无法读取恢复任务项目：{error}"))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !actual.insert(name.clone()) || !expected.contains(&name) {
            return Err(journal_error("整库恢复任务目录包含未知或重复项目。"));
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new("restoreJournal", format!("无法检查恢复任务项目：{error}"))
        })?;
        if super::is_link_or_reparse(&metadata)
            || (name == PLAN_FILE && !metadata.is_file())
            || (name != PLAN_FILE && !metadata.is_dir())
        {
            return Err(journal_error("整库恢复任务项目类型无效或包含链接。"));
        }
    }
    if actual != expected {
        return Err(journal_error("整库恢复任务目录缺少必需计划或暂存目录。"));
    }
    Ok(())
}

fn read_journal_state(
    task_root: &Path,
    plan: &RestorePlan,
) -> Result<JournalState, WorkspaceError> {
    let directory = task_root.join(EVENTS_DIRECTORY);
    ensure_plain_directory(&directory)?;
    let total_steps = step_count(plan);
    let max_events = total_steps.saturating_mul(4).saturating_add(4);
    let mut state = JournalState::default();
    let mut count = 0usize;
    let mut partial_count = 0usize;
    for entry in fs::read_dir(&directory).map_err(|error| {
        StorageError::new("restoreJournal", format!("无法读取整库恢复日志：{error}"))
    })? {
        let entry = entry.map_err(|error| {
            StorageError::new("restoreJournal", format!("无法读取恢复日志项目：{error}"))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new("restoreJournal", format!("无法检查恢复日志项目：{error}"))
        })?;
        if !metadata.is_file()
            || super::is_link_or_reparse(&metadata)
            || metadata.len() > MAX_IDENTITY_BYTES
        {
            return Err(journal_error("整库恢复日志必须是小型本机普通文件。"));
        }
        if is_partial_event_record_name(&name) {
            partial_count += 1;
            if partial_count > max_events {
                return Err(journal_error("整库恢复日志临时记录数量超过安全上限。"));
            }
            continue;
        }
        count += 1;
        if count > max_events {
            return Err(journal_error("整库恢复日志数量超过计划上限。"));
        }
        let bytes = read_local_bounded(&path, MAX_IDENTITY_BYTES, "整库恢复日志")?;
        let event: JournalEntry = serde_json::from_slice(&bytes)
            .map_err(|error| journal_error(&format!("整库恢复日志格式无效：{error}")))?;
        if event.schema_version != 1 || event.restore_id != plan.restore_id {
            return Err(journal_error("整库恢复日志身份与计划不一致。"));
        }
        if name == "rollback-started.json" {
            if event.step.is_some()
                || event.state.as_deref() != Some("rollingBack")
                || state.rollback_started
            {
                return Err(journal_error("回退方向日志无效或重复。"));
            }
            state.rollback_started = true;
        } else if name == "terminal.json" {
            if event.step.is_some()
                || !matches!(event.state.as_deref(), Some("completed" | "rolledBack"))
                || state.terminal.is_some()
            {
                return Err(journal_error("整库恢复终态日志无效或重复。"));
            }
            state.terminal = event.state;
        } else if name == "closed.json" {
            if event.step.is_some()
                || !matches!(event.state.as_deref(), Some("completed" | "rolledBack"))
                || state.closed
            {
                return Err(journal_error("整库恢复关闭日志无效或重复。"));
            }
            state.closed = true;
            state.closed_state = event.state;
        } else {
            let (index, suffix) = parse_step_event_name(&name)
                .ok_or_else(|| journal_error("整库恢复日志含未知文件名。"))?;
            if index >= total_steps || event.step != Some(index) || event.state.is_some() {
                return Err(journal_error("整库恢复步骤日志与计划不匹配。"));
            }
            let target = match suffix {
                "forward-intent" => &mut state.forward_intents,
                "forward-done" => &mut state.forward_done,
                "rollback-intent" => &mut state.rollback_intents,
                "rollback-done" => &mut state.rollback_done,
                _ => return Err(journal_error("整库恢复步骤日志类型未知。")),
            };
            if !target.insert(index) {
                return Err(journal_error("整库恢复步骤日志重复。"));
            }
        }
    }
    if state
        .forward_done
        .iter()
        .any(|index| !state.forward_intents.contains(index))
        || state
            .rollback_done
            .iter()
            .any(|index| !state.rollback_intents.contains(index))
        || state
            .rollback_intents
            .iter()
            .any(|index| !state.forward_intents.contains(index))
        || (!state.rollback_intents.is_empty() && !state.rollback_started)
        || (state.closed && state.terminal.is_none())
        || (state.closed && state.closed_state.as_deref() != state.terminal.as_deref())
        || (state.terminal.as_deref() == Some("completed")
            && (state.forward_done.len() != total_steps
                || state.rollback_started
                || !state.rollback_intents.is_empty()
                || !state.rollback_done.is_empty()))
        || (state.terminal.as_deref() == Some("rolledBack")
            && (!state.rollback_started || state.rollback_done != state.forward_intents))
    {
        return Err(journal_error("整库恢复日志事件顺序或终态条件无效。"));
    }
    Ok(state)
}

fn parse_step_event_name(name: &str) -> Option<(usize, &str)> {
    let stem = name.strip_suffix(".json")?;
    let (index, suffix) = stem.split_once('-')?;
    if index.len() != 6 || !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((index.parse().ok()?, suffix))
}

fn is_partial_event_record_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".partial.tmp"))
    else {
        return false;
    };
    let Some((target, id)) = stem.rsplit_once('.') else {
        return false;
    };
    if !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        return false;
    }
    let known_fixed = matches!(
        target,
        "rollback-started.json" | "terminal.json" | "closed.json"
    );
    let known_step = parse_step_event_name(target).is_some_and(|(_, suffix)| {
        matches!(
            suffix,
            "forward-intent" | "forward-done" | "rollback-intent" | "rollback-done"
        )
    });
    known_fixed || known_step
}

fn validate_stages(
    pending: &PendingRestore,
    job: Option<&BackupJob>,
) -> Result<(), WorkspaceError> {
    match job {
        Some(job) => {
            BackupContext::verify_library_restore_stage(
                &pending.task_root.join(SOURCE_STAGE),
                &pending.plan.after,
                job,
            )?;
            BackupContext::verify_library_restore_stage(
                &pending.task_root.join(BEFORE_STAGE),
                &pending.plan.before,
                job,
            )?;
        }
        None => {
            BackupContext::verify_library_restore_stage_at_startup(
                &pending.task_root.join(SOURCE_STAGE),
                &pending.plan.after,
            )?;
            BackupContext::verify_library_restore_stage_at_startup(
                &pending.task_root.join(BEFORE_STAGE),
                &pending.plan.before,
            )?;
        }
    }
    Ok(())
}

fn relative_to_library(library: &str, path: &Path) -> Result<String, WorkspaceError> {
    let relative = path
        .strip_prefix(library)
        .map_err(|_| journal_error("事实目标不在笔记库内。"))?;
    let text = relative.to_string_lossy().replace('\\', "/");
    validated_relative_path(&text)?;
    Ok(text)
}

fn is_uuid(value: &str) -> bool {
    Uuid::parse_str(value)
        .is_ok_and(|uuid| uuid.get_version_num() == 4 && uuid.to_string() == value)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    super::sync_directory(path)
        .map_err(|error| StorageError::new("io", format!("无法同步恢复目录：{error}")))
}

fn with_recovery_guidance(error: WorkspaceError, backup: &BackupSummary) -> WorkspaceError {
    StorageError::new(
        error.kind,
        format!(
            "{}；整库恢复任务及保护备份仍保留，恢复前备份位于 {}。",
            error.message, backup.path
        ),
    )
}

fn post_restore_error(
    error: WorkspaceError,
    backup: &BackupSummary,
    context: &str,
) -> WorkspaceError {
    StorageError::new(
        error.kind,
        format!(
            "{context}；恢复前的完整手动备份仍保留于 {}。{}",
            backup.path, error.message
        ),
    )
}

fn journal_error(message: &str) -> WorkspaceError {
    StorageError::new("restoreJournal", message)
}

impl RestorePlan {
    fn bind_runtime_paths(&mut self, task_root: PathBuf, recovery_root: PathBuf) {
        self._task_root = task_root;
        self._recovery_root = recovery_root;
    }

    fn library_path_as_path(&self) -> Result<PathBuf, WorkspaceError> {
        Ok(PathBuf::from(&self.library_path))
    }
}
