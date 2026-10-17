//! Desktop commands accept session/note identities, never frontend filesystem paths.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{
    ipc::{InvokeBody, Request, Response},
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_dialog::DialogExt;

use crate::note_format::{AssetManifestEntry, NoteContent, PlainText};
use crate::workspace::{
    BackupCatalog, BackupContext, BackupControl, BackupJob, BackupKind, BackupNoteList,
    BackupOutcome, BackupPhase, BackupProgress, BackupRestoreReceipt, ExternalProbe,
    ExternalReconciliation, ExternalSaveReceipt, LibraryRestoreAction, LibraryRestorePreview,
    LibraryRestoreReceipt, LibraryView, MarkdownExportReceipt, NoteSession, RecoveryList,
    SaveReceipt, SavedCopy, SearchResults, VersionList, Workspace, WorkspaceError, WorkspaceView,
};

#[derive(Clone)]
pub struct WorkspaceState {
    workspace: Arc<Mutex<Workspace>>,
    backups: BackupControl,
    restore_running: Arc<AtomicBool>,
}

impl WorkspaceState {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace: Arc::new(Mutex::new(workspace)),
            backups: BackupControl::default(),
            restore_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn cancel_backup(&self) {
        self.backups.cancel();
    }

    pub(crate) fn cancel_backup_for_file_change(&self) {
        self.backups.cancel_for_file_change();
    }

    fn ensure_not_restoring(&self) -> Result<(), WorkspaceError> {
        if self.restore_running.load(Ordering::Acquire) {
            return Err(command_error("整库恢复正在进行；普通笔记操作已暂停。"));
        }
        Ok(())
    }
}

struct RestoreRunningGuard(Arc<AtomicBool>);

impl RestoreRunningGuard {
    fn begin(state: &WorkspaceState) -> Result<Self, WorkspaceError> {
        state
            .restore_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| command_error("已有整库恢复正在进行。"))?;
        Ok(Self(Arc::clone(&state.restore_running)))
    }
}

impl Drop for RestoreRunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn command_error(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError {
        kind: "desktop".into(),
        message: message.into(),
        recovery_path: None,
        recovery_error: None,
    }
}

async fn work<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        let _access = workspace.acquire_library_access()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记操作未完成：{error}")))?
}

/// Announce before waiting for the workspace lock. A backup copies outside that
/// lock; this guard also prevents it publishing between an edit request and the
/// corresponding filesystem operation (including operations that later fail).
async fn mutate<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let _mutation = state.backups.begin_mutation()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        let _access = workspace.acquire_library_access()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记操作未完成：{error}")))?
}

/// Note saves build a candidate from the cached editor session before touching
/// the library. Let Workspace acquire the local library lease itself so it can
/// preserve that candidate if the selected library is unavailable.
async fn save_mutation<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(feature = "perf-acceptance")]
        let mut trace = crate::performance::Trace::new("save-command");
        state.ensure_not_restoring()?;
        let _mutation = state.backups.begin_mutation()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        #[cfg(feature = "perf-acceptance")]
        trace.mark("workspace-lock-acquired");
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记操作未完成：{error}")))?
}

/// Editor-only calls follow the host-owned session, independent of an unrelated library.
async fn editor_work<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断。"))?;
        state.ensure_not_restoring()?;
        workspace.ensure_library_transition_allowed()?;
        let _access = if workspace.active_session_is_external() {
            None
        } else {
            workspace.acquire_library_access()?
        };
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记操作未完成：{error}")))?
}

/// Recovery catalog reads use only the local recovery directory.
async fn recovery_read<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        workspace.ensure_library_transition_allowed()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("恢复目录读取未完成：{error}")))?
}

/// Opening a recovery preview changes the active cached session, so require a
/// clear restore identity while still avoiding a lease on an unavailable old
/// library.
async fn recovery_session_work<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        workspace.ensure_library_transition_allowed()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("恢复预览未完成：{error}")))?
}

/// Explicit exports use only a cached session or a local recovery package and
/// publish outside the selected library. They remain available while a known
/// restore pointer blocks ordinary library operations.
async fn cached_export<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("独立副本导出未完成：{error}")))?
}

/// Library transitions do not need to acquire the old library lease. The
/// Workspace validates its persistent restore identity and the target is
/// opened under its own shared lease before it becomes selected.
async fn switch_library<T: Send + 'static>(
    state: WorkspaceState,
    action: impl FnOnce(&mut Workspace) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let _mutation = state.backups.begin_mutation()?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记库切换未完成：{error}")))?
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopBackupProgress {
    request_id: String,
    #[serde(flatten)]
    progress: BackupProgress,
}

fn require_backup_library(workspace: &Workspace, expected: &str) -> Result<(), WorkspaceError> {
    if crate::workspace::display_path(&workspace.library_path()?) != expected {
        return Err(command_error(
            "笔记库已切换；此次备份操作已取消，请在当前笔记库重试。",
        ));
    }
    Ok(())
}

fn validate_backup_request_id(request_id: &str) -> Result<(), WorkspaceError> {
    if request_id.is_empty() || request_id.len() > 128 || request_id.chars().any(char::is_control) {
        return Err(command_error("备份请求身份无效，请重试。"));
    }
    Ok(())
}

fn begin_desktop_backup_job(
    app: AppHandle,
    state: &WorkspaceState,
    request_id: String,
) -> Result<BackupJob, WorkspaceError> {
    state
        .backups
        .begin_job(Some(desktop_backup_progress(app, request_id)))
}

fn desktop_backup_progress(
    app: AppHandle,
    request_id: String,
) -> Arc<dyn Fn(BackupProgress) + Send + Sync> {
    Arc::new(move |progress| {
        let _ = app.emit(
            "cuenote:backup-progress",
            DesktopBackupProgress {
                request_id: request_id.clone(),
                progress,
            },
        );
    })
}

async fn backup_work<T: Send + 'static>(
    app: AppHandle,
    state: WorkspaceState,
    request_id: String,
    expected_library_path: String,
    selected_directory: Option<std::path::PathBuf>,
    action: impl FnOnce(BackupContext, &BackupJob) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    validate_backup_request_id(&request_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        // Directory changes invalidate a job just like a library transition.
        let mutation = selected_directory
            .as_ref()
            .map(|_| state.backups.begin_mutation())
            .transpose()?;
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断，请重新启动 CueNote 后重试备份。"))?;
        require_backup_library(&workspace, &expected_library_path)?;
        state.ensure_not_restoring()?;
        let _access = workspace.acquire_library_access()?;
        let context = if let Some(directory) = selected_directory {
            workspace.set_backup_directory(directory)?
        } else {
            workspace.backup_context()?
        };
        drop(mutation);
        let job = begin_desktop_backup_job(app, &state, request_id)?;
        // Hashing, copying and package validation must not hold the editor's
        // mutex. The coordinator guards the final publication separately.
        drop(workspace);
        action(context, &job)
    })
    .await
    .map_err(|error| command_error(format!("备份操作未完成：{error}")))?
}

#[tauri::command]
pub async fn run_backup(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    kind: BackupKind,
    request_id: String,
    expected_library_path: String,
) -> Result<BackupOutcome, WorkspaceError> {
    backup_work(
        app,
        state.inner().clone(),
        request_id,
        expected_library_path,
        None,
        move |context, job| {
            let local_date = chrono::Local::now().format("%Y-%m-%d").to_string();
            context.run(kind, &local_date, job)
        },
    )
    .await
}

#[tauri::command]
pub async fn list_backups(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    request_id: String,
    expected_library_path: String,
) -> Result<BackupCatalog, WorkspaceError> {
    backup_work(
        app,
        state.inner().clone(),
        request_id,
        expected_library_path,
        None,
        |context, job| context.catalog(job),
    )
    .await
}

#[tauri::command]
pub async fn list_backup_notes(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    backup_id: String,
    request_id: String,
    expected_library_path: String,
) -> Result<BackupNoteList, WorkspaceError> {
    validate_backup_request_id(&request_id)?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断，请重新启动 CueNote 后重试备份。"))?;
        require_backup_library(&workspace, &expected_library_path)?;
        state.ensure_not_restoring()?;
        let _access = workspace.acquire_library_access()?;
        let context = workspace.backup_context()?;
        let job = begin_desktop_backup_job(app, &state, request_id)?;
        let available_notebook_ids = workspace.backup_restore_notebook_ids()?;
        job.checkpoint()?;
        drop(workspace);

        let mut result = context.list_notes(&backup_id, &job)?;
        job.checkpoint()?;
        result.available_notebook_ids = available_notebook_ids;
        Ok(result)
    })
    .await
    .map_err(|error| command_error(format!("备份操作未完成：{error}")))?
}

#[tauri::command]
pub async fn restore_backup_note(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    backup_id: String,
    entry_id: String,
    expected_revision: String,
    target_notebook_id: Option<String>,
    request_id: String,
    expected_library_path: String,
) -> Result<BackupRestoreReceipt, WorkspaceError> {
    validate_backup_request_id(&request_id)?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        let (context, job, _access) = {
            let workspace = state
                .workspace
                .lock()
                .map_err(|_| command_error("笔记服务已中断，请重新启动 CueNote 后重试恢复。"))?;
            require_backup_library(&workspace, &expected_library_path)?;
            state.ensure_not_restoring()?;
            let access = workspace.acquire_library_access()?;
            let context = workspace.backup_context()?;
            let job = begin_desktop_backup_job(app.clone(), &state, request_id.clone())?;
            (context, job, access)
        };
        let source = context.load_note_entry(&backup_id, &entry_id, &expected_revision, &job)?;
        let prepared = {
            let workspace = state
                .workspace
                .lock()
                .map_err(|_| command_error("笔记服务已中断，恢复尚未进入写入阶段。"))?;
            require_backup_library(&workspace, &expected_library_path)?;
            job.checkpoint()?;
            workspace.prepare_backup_note_restore(source, target_notebook_id.as_deref())?
        };
        // Protecting the whole current library can take time. Keep the editing
        // mutex available for cancellation/close and the existing read paths.
        let local_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let protected = prepared.protect_current(&local_date, &job)?;
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("恢复前备份已生成，但笔记服务已中断；恢复未进入写入阶段。")
        })?;
        require_backup_library(&workspace, &expected_library_path)?;
        // Atomically hand the job slot to a mutation guard. A cancellation
        // accepted before this point aborts; after it, report the actual safe
        // publication result and do not pretend a late cancel rolled it back.
        let _mutation = job.into_mutation()?;
        let _ = app.emit(
            "cuenote:backup-progress",
            DesktopBackupProgress {
                request_id,
                progress: BackupProgress {
                    library_path: expected_library_path,
                    phase: BackupPhase::Restoring,
                    checked: 0,
                    total: 1,
                },
            },
        );
        workspace.restore_backup_note(protected)
    })
    .await
    .map_err(|error| command_error(format!("备份恢复操作未完成：{error}")))?
}

#[tauri::command]
pub async fn preview_library_restore(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    backup_id: String,
    request_id: String,
    expected_library_path: String,
) -> Result<LibraryRestorePreview, WorkspaceError> {
    validate_backup_request_id(&request_id)?;
    let owned = state.inner().clone();
    work(owned.clone(), move |workspace| {
        require_backup_library(workspace, &expected_library_path)?;
        let job = begin_desktop_backup_job(app, &owned, request_id)?;
        workspace.preview_library_restore(&backup_id, &job)
    })
    .await
}

async fn library_restore_work(
    app: AppHandle,
    state: WorkspaceState,
    request_id: String,
    expected_library_path: String,
    action: impl FnOnce(&mut Workspace, &BackupJob) -> Result<LibraryRestoreReceipt, WorkspaceError>
        + Send
        + 'static,
) -> Result<LibraryRestoreReceipt, WorkspaceError> {
    validate_backup_request_id(&request_id)?;
    let progress_app = app.clone();
    let mut receipt = tauri::async_runtime::spawn_blocking(move || {
        let _running = RestoreRunningGuard::begin(&state)?;
        let job = state
            .backups
            .begin_restore_job(Some(desktop_backup_progress(progress_app, request_id)))?;
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断。"))?;
        require_backup_library(&workspace, &expected_library_path)?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        job.checkpoint()?;
        action(&mut workspace, &job)
    })
    .await
    .map_err(|error| {
        command_error(format!(
            "整库恢复任务已中断，请读取恢复记录后继续或回退：{error}"
        ))
    })??;
    crate::file_watch::configure(&app, receipt.workspace.library.as_mut());
    Ok(receipt)
}

#[tauri::command]
pub async fn restore_library_from_backup(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    backup_id: String,
    expected_manifest_revision: String,
    request_id: String,
    expected_library_path: String,
) -> Result<LibraryRestoreReceipt, WorkspaceError> {
    library_restore_work(
        app,
        state.inner().clone(),
        request_id,
        expected_library_path,
        move |workspace, job| {
            workspace.restore_library_from_backup(
                &backup_id,
                &expected_manifest_revision,
                &chrono::Local::now().format("%Y-%m-%d").to_string(),
                job,
            )
        },
    )
    .await
}

#[tauri::command]
pub async fn resume_library_restore(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    action: LibraryRestoreAction,
    request_id: String,
    expected_library_path: String,
) -> Result<LibraryRestoreReceipt, WorkspaceError> {
    library_restore_work(
        app,
        state.inner().clone(),
        request_id,
        expected_library_path,
        move |workspace, job| workspace.resume_library_restore(action, job),
    )
    .await
}

#[tauri::command]
pub async fn choose_backup_directory(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    request_id: String,
    expected_library_path: String,
) -> Result<Option<BackupCatalog>, WorkspaceError> {
    let expected = expected_library_path.clone();
    work(state.inner().clone(), move |workspace| {
        require_backup_library(workspace, &expected)
    })
    .await?;
    let picker = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title("选择独立本机备份目录（OneDrive 同步目录之外）")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| command_error(format!("无法选择备份目录：{error}")))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let directory = selected
        .into_path()
        .map_err(|error| command_error(format!("请选择本机文件夹：{error}")))?;
    backup_work(
        app,
        state.inner().clone(),
        request_id,
        expected_library_path,
        Some(directory),
        |context, job| context.catalog(job).map(Some),
    )
    .await
}

#[tauri::command]
pub fn cancel_backup(state: State<'_, WorkspaceState>) {
    state.cancel_backup();
}

#[tauri::command]
pub async fn get_workspace(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceView, WorkspaceError> {
    let state = state.inner().clone();
    let mut view = tauri::async_runtime::spawn_blocking(move || {
        state.ensure_not_restoring()?;
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断。"))?;
        state.ensure_not_restoring()?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        workspace.snapshot()
    })
    .await
    .map_err(|error| command_error(format!("无法读取工作区：{error}")))??;
    let watched = if view.library_restore.is_none() {
        view.library.as_mut()
    } else {
        None
    };
    crate::file_watch::configure(&app, watched);
    Ok(view)
}

#[tauri::command]
pub async fn search_notes(
    state: State<'_, WorkspaceState>,
    query: String,
    notebook_id: Option<String>,
    offset: Option<usize>,
) -> Result<SearchResults, WorkspaceError> {
    work(state.inner().clone(), move |workspace| {
        workspace.search_notes(&query, notebook_id.as_deref(), offset.unwrap_or(0))
    })
    .await
}

#[tauri::command]
pub async fn rebuild_search_index(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<LibraryView, WorkspaceError> {
    let mut view = work(state.inner().clone(), Workspace::rebuild_search_index).await?;
    crate::file_watch::configure(&app, Some(&mut view));
    Ok(view)
}

#[tauri::command]
pub async fn use_default_library(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceView, WorkspaceError> {
    let mut view = switch_library(state.inner().clone(), |workspace| {
        workspace.ensure_library_transition_allowed()?;
        workspace.mark_backup_check_due(&chrono::Local::now().format("%Y-%m-%d").to_string())?;
        workspace.use_default_library()
    })
    .await?;
    crate::file_watch::configure(&app, view.library.as_mut());
    Ok(view)
}

#[tauri::command]
pub async fn initialize_selected_library(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceView, WorkspaceError> {
    let mut view = mutate(state.inner().clone(), |workspace| {
        workspace.mark_backup_check_due(&chrono::Local::now().format("%Y-%m-%d").to_string())?;
        workspace.initialize_selected_library()
    })
    .await?;
    crate::file_watch::configure(&app, view.library.as_mut());
    Ok(view)
}

#[tauri::command]
pub async fn choose_library(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<Option<WorkspaceView>, WorkspaceError> {
    // The blocking picker must run off the event-loop thread, especially on macOS.
    let picker = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title("选择 CueNote 笔记库文件夹")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| command_error(format!("无法选择笔记库：{error}")))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| command_error(format!("请选择本机文件夹：{error}")))?;
    let mut view = switch_library(state.inner().clone(), move |workspace| {
        workspace.ensure_library_transition_allowed()?;
        workspace.mark_backup_check_due(&chrono::Local::now().format("%Y-%m-%d").to_string())?;
        workspace.select_library(path).map(Some)
    })
    .await?;
    crate::file_watch::configure(&app, view.as_mut().and_then(|view| view.library.as_mut()));
    Ok(view)
}

#[tauri::command]
pub async fn open_library_folder(state: State<'_, WorkspaceState>) -> Result<(), WorkspaceError> {
    work(state.inner().clone(), |workspace| {
        let path = workspace.library_path()?;
        tauri_plugin_opener::open_path(path, None::<&str>)
            .map_err(|error| command_error(format!("无法打开笔记库目录：{error}")))
    })
    .await
}

#[tauri::command]
pub async fn new_note(
    state: State<'_, WorkspaceState>,
    notebook_id: Option<String>,
) -> Result<NoteSession, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.new_note_in(notebook_id.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn create_notebook(
    state: State<'_, WorkspaceState>,
    name: String,
) -> Result<LibraryView, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.create_notebook(&name)
    })
    .await
}

#[tauri::command]
pub async fn rename_notebook(
    state: State<'_, WorkspaceState>,
    notebook_id: String,
    name: String,
) -> Result<LibraryView, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.rename_notebook(&notebook_id, &name)
    })
    .await
}

#[tauri::command]
pub async fn delete_notebook(
    state: State<'_, WorkspaceState>,
    notebook_id: String,
) -> Result<LibraryView, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.delete_notebook(&notebook_id)
    })
    .await
}

#[tauri::command]
pub async fn update_note_organization(
    state: State<'_, WorkspaceState>,
    session_id: String,
    notebook_id: String,
    pinned: bool,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.update_note_organization(&session_id, &notebook_id, pinned)
    })
    .await
}

#[tauri::command]
pub async fn trash_note(
    state: State<'_, WorkspaceState>,
    session_id: String,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.trash_note(&session_id)
    })
    .await
}

#[tauri::command]
pub async fn restore_trashed_note(
    state: State<'_, WorkspaceState>,
    session_id: String,
    target_notebook_id: Option<String>,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.restore_trashed_note(&session_id, target_notebook_id.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn delete_trashed_note(
    state: State<'_, WorkspaceState>,
    session_id: String,
    confirmed: bool,
) -> Result<WorkspaceView, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.delete_trashed_note(&session_id, confirmed)
    })
    .await
}

#[tauri::command]
pub async fn set_view_preferences(
    state: State<'_, WorkspaceState>,
    sidebar_collapsed: bool,
    selected_notebook_id: Option<String>,
) -> Result<(), WorkspaceError> {
    work(state.inner().clone(), move |workspace| {
        workspace.set_view_preferences(sidebar_collapsed, selected_notebook_id)
    })
    .await
}

#[tauri::command]
pub async fn open_note(
    state: State<'_, WorkspaceState>,
    note_id: String,
) -> Result<NoteSession, WorkspaceError> {
    work(state.inner().clone(), move |workspace| {
        workspace.open_note(&note_id)
    })
    .await
}

#[tauri::command]
pub async fn open_external_note(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    expected_session_id: Option<String>,
) -> Result<Option<NoteSession>, WorkspaceError> {
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("CueNote 笔记", &["cuenote"])
            .blocking_pick_file()
    })
    .await
    .map_err(|error| command_error(format!("无法打开文件选择器：{error}")))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| command_error(format!("请选择本机文件：{error}")))?;
    recovery_session_work(state.inner().clone(), move |workspace| {
        workspace.require_current_session(expected_session_id.as_deref())?;
        workspace.open_external_note(path).map(Some)
    })
    .await
}

#[tauri::command]
pub async fn save_external_note_content(
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<ExternalSaveReceipt, WorkspaceError> {
    recovery_session_work(state.inner().clone(), move |workspace| {
        workspace.save_external_note_content(&session_id, content)
    })
    .await
}

#[tauri::command]
pub async fn reopen_external_note(
    state: State<'_, WorkspaceState>,
    session_id: String,
) -> Result<NoteSession, WorkspaceError> {
    recovery_session_work(state.inner().clone(), move |workspace| {
        workspace.reopen_external_note(&session_id)
    })
    .await
}

#[tauri::command]
pub async fn save_external_into_library(
    state: State<'_, WorkspaceState>,
    session_id: String,
    notebook_id: String,
    content: NoteContent,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.save_external_into_library(&session_id, &notebook_id, content)
    })
    .await
}

#[tauri::command]
pub async fn save_note_text(
    state: State<'_, WorkspaceState>,
    session_id: String,
    text: PlainText,
) -> Result<SaveReceipt, WorkspaceError> {
    save_mutation(state.inner().clone(), move |workspace| {
        workspace.save_note_text(&session_id, text)
    })
    .await
}

#[tauri::command]
pub async fn save_note_content(
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<SaveReceipt, WorkspaceError> {
    save_mutation(state.inner().clone(), move |workspace| {
        workspace.save_note_content(&session_id, content)
    })
    .await
}

#[tauri::command]
pub async fn stage_image_raw(
    state: State<'_, WorkspaceState>,
    request: Request<'_>,
) -> Result<AssetManifestEntry, WorkspaceError> {
    let session_id = request
        .headers()
        .get("x-cuenote-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| command_error("图片请求缺少有效的笔记会话身份。"))?;
    let bytes = match request.body() {
        InvokeBody::Raw(bytes) => bytes.clone(),
        InvokeBody::Json(_) => {
            return Err(command_error("图片请求必须使用原始字节数据。"));
        }
    };
    editor_work(state.inner().clone(), move |workspace| {
        workspace.stage_image_bytes(&session_id, &bytes)
    })
    .await
}

#[tauri::command]
pub async fn preflight_image_insert(
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<(), WorkspaceError> {
    editor_work(state.inner().clone(), move |workspace| {
        workspace.preflight_image_insert(&session_id, content)
    })
    .await
}

#[tauri::command]
pub async fn preflight_note_content(
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<(), WorkspaceError> {
    editor_work(state.inner().clone(), move |workspace| {
        workspace.preflight_note_content(&session_id, content)
    })
    .await
}

#[tauri::command]
pub async fn read_image_asset(
    state: State<'_, WorkspaceState>,
    session_id: String,
    asset_id: String,
) -> Result<Response, WorkspaceError> {
    editor_work(state.inner().clone(), move |workspace| {
        workspace
            .read_image_asset(&session_id, &asset_id)
            .map(Response::new)
    })
    .await
}

#[tauri::command]
pub fn activate_close_guard(gate: State<'_, crate::CloseGate>) {
    gate.armed.store(true, std::sync::atomic::Ordering::Release);
}

#[tauri::command]
pub async fn complete_close(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<(), WorkspaceError> {
    // The frontend freezes new transitions and flushes its latest text before
    // requesting this. Wait for any existing host operation to release its lock.
    state.cancel_backup();
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断。"))?;
        // Preserve a bounded handoff when a quick close beats the automatic
        // backup debounce. Do this before releasing the editing session so a
        // local settings failure leaves the existing close-retry path usable.
        if workspace.ensure_library_available().is_ok() {
            workspace
                .mark_backup_check_due(&chrono::Local::now().format("%Y-%m-%d").to_string())?;
        }
        workspace.close()
    })
    .await
    .map_err(|error| command_error(format!("无法完成关闭：{error}")))??;
    app.state::<crate::CloseGate>()
        .approved
        .store(true, std::sync::atomic::Ordering::Release);
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn list_recovery_drafts(
    state: State<'_, WorkspaceState>,
) -> Result<RecoveryList, WorkspaceError> {
    recovery_read(state.inner().clone(), Workspace::list_recovery_drafts).await
}

#[tauri::command]
pub async fn restore_recovery_as_copy(
    state: State<'_, WorkspaceState>,
    recovery_id: String,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.restore_recovery_as_copy(&recovery_id)
    })
    .await
}

async fn pick_copy_path(app: AppHandle) -> Result<Option<std::path::PathBuf>, WorkspaceError> {
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("另存完整笔记副本（请选择新文件名）")
            .set_file_name("CueNote-恢复副本.cuenote")
            .add_filter("CueNote 笔记", &["cuenote"])
            .blocking_save_file()
    })
    .await
    .map_err(|error| command_error(format!("无法选择另存位置：{error}")))?;
    selected
        .map(|path| {
            path.into_path()
                .map_err(|error| command_error(format!("请选择本机文件路径：{error}")))
        })
        .transpose()
}

#[tauri::command]
pub async fn probe_external_changes(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    session_id: Option<String>,
) -> Result<ExternalProbe, WorkspaceError> {
    let mut probe = editor_work(state.inner().clone(), move |workspace| {
        workspace.probe_external_changes(session_id.as_deref())
    })
    .await?;
    crate::file_watch::configure(&app, probe.library.as_mut());
    Ok(probe)
}

#[tauri::command]
pub async fn reconcile_external_changes(
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<ExternalReconciliation, WorkspaceError> {
    editor_work(state.inner().clone(), move |workspace| {
        workspace.reconcile_external_changes(&session_id, content)
    })
    .await
}

#[tauri::command]
pub async fn list_preserved_versions(
    state: State<'_, WorkspaceState>,
) -> Result<VersionList, WorkspaceError> {
    recovery_read(state.inner().clone(), Workspace::list_preserved_versions).await
}

#[tauri::command]
pub async fn open_preserved_version(
    state: State<'_, WorkspaceState>,
    version_id: String,
) -> Result<NoteSession, WorkspaceError> {
    recovery_session_work(state.inner().clone(), move |workspace| {
        workspace.open_preserved_version(&version_id)
    })
    .await
}

#[tauri::command]
pub async fn restore_preserved_as_copy(
    state: State<'_, WorkspaceState>,
    version_id: String,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
        workspace.restore_preserved_as_copy(&version_id)
    })
    .await
}

#[tauri::command]
pub async fn resolve_preserved_version(
    state: State<'_, WorkspaceState>,
    version_id: String,
) -> Result<(), WorkspaceError> {
    work(state.inner().clone(), move |workspace| {
        workspace.resolve_preserved_version(&version_id)
    })
    .await
}

#[tauri::command]
pub async fn save_preserved_as(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    version_id: String,
) -> Result<Option<SavedCopy>, WorkspaceError> {
    let Some(path) = pick_copy_path(app).await? else {
        return Ok(None);
    };
    cached_export(state.inner().clone(), move |workspace| {
        workspace.save_preserved_as(&version_id, path).map(Some)
    })
    .await
}

#[tauri::command]
pub async fn save_current_as(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    session_id: String,
    text: PlainText,
) -> Result<Option<SavedCopy>, WorkspaceError> {
    let Some(path) = pick_copy_path(app).await? else {
        return Ok(None);
    };
    cached_export(state.inner().clone(), move |workspace| {
        workspace.save_current_as(&session_id, text, path).map(Some)
    })
    .await
}

#[tauri::command]
pub async fn save_current_content_as(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<Option<SavedCopy>, WorkspaceError> {
    let Some(path) = pick_copy_path(app).await? else {
        return Ok(None);
    };
    cached_export(state.inner().clone(), move |workspace| {
        workspace
            .save_current_content_as(&session_id, content, path)
            .map(Some)
    })
    .await
}

#[tauri::command]
pub async fn export_current_markdown(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    session_id: String,
    content: NoteContent,
) -> Result<Option<MarkdownExportReceipt>, WorkspaceError> {
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择 Markdown 导出位置（会新建一个完整导出文件夹）")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| command_error(format!("无法选择导出目录：{error}")))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let parent = selected
        .into_path()
        .map_err(|error| command_error(format!("请选择本机文件夹：{error}")))?;
    let path = parent.join(format!(
        "CueNote-{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        uuid::Uuid::new_v4().simple()
    ));
    cached_export(state.inner().clone(), move |workspace| {
        workspace
            .export_current_markdown_bundle(&session_id, content, path)
            .map(Some)
    })
    .await
}

#[tauri::command]
pub async fn save_recovery_as(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    recovery_id: String,
) -> Result<Option<SavedCopy>, WorkspaceError> {
    let Some(path) = pick_copy_path(app).await? else {
        return Ok(None);
    };
    cached_export(state.inner().clone(), move |workspace| {
        workspace.save_recovery_as(&recovery_id, path).map(Some)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_format::{Block, Inline, Sections};
    use std::path::Path;

    fn fixture() -> (tempfile::TempDir, Workspace, String, String) {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
        std::fs::create_dir_all(&parent).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("command-save-failures-")
            .tempdir_in(parent)
            .unwrap();
        let root = temp.path();
        let mut workspace = Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            root.join("library"),
        );
        workspace.use_default_library().unwrap();
        let session = workspace.new_note().unwrap();
        workspace
            .save_note_text(&session.session_id, test_text("saved source"))
            .unwrap();
        (temp, workspace, session.session_id, session.note_id)
    }

    fn test_text(content: &str) -> PlainText {
        PlainText {
            title: "host save test".into(),
            cue: String::new(),
            content: content.into(),
            summary: String::new(),
        }
    }

    fn test_content(content: &str) -> NoteContent {
        NoteContent {
            title: "host rich copy".into(),
            sections: Sections {
                cue: Vec::new(),
                content: vec![Block::Paragraph {
                    children: vec![Inline::Text {
                        text: content.into(),
                        marks: Vec::new(),
                    }],
                }],
                summary: Vec::new(),
            },
        }
    }

    #[test]
    fn host_save_preserves_draft_and_cached_exports_work_with_unavailable_library() {
        let (temp, workspace, session_id, note_id) = fixture();
        let root = temp.path();
        let library = workspace.library_path().unwrap();
        let source = library.join("notes").join(format!("{note_id}.cuenote"));
        let before = crate::storage::open_note(&source).unwrap();
        std::fs::rename(&library, root.join("library-away")).unwrap();
        std::fs::rename(
            root.join("recovery/drafts"),
            root.join("recovery/drafts-away"),
        )
        .unwrap();
        std::fs::write(
            root.join("recovery/drafts"),
            b"unavailable drafts directory",
        )
        .unwrap();
        let state = WorkspaceState::new(workspace);

        let draft = test_text("latest host plain draft");
        let session_id_for_save = session_id.clone();
        let error =
            tauri::async_runtime::block_on(save_mutation(state.clone(), move |workspace| {
                workspace.save_note_text(&session_id_for_save, draft)
            }))
            .unwrap_err();
        assert!(error.recovery_path.is_none());
        assert!(error.recovery_error.is_some(), "{error:?}");

        let destination = root.join("outside/plain-copy.cuenote");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let text = test_text("latest host plain draft");
        let session_id_for_plain_copy = session_id.clone();
        let plain_copy =
            tauri::async_runtime::block_on(cached_export(state.clone(), move |workspace| {
                workspace.save_current_as(&session_id_for_plain_copy, text, destination)
            }))
            .unwrap();
        let plain = crate::storage::open_note(&root.join("outside/plain-copy.cuenote")).unwrap();
        assert_eq!(plain.revision, plain_copy.revision);
        assert_eq!(
            plain.package.text_projection().content,
            "latest host plain draft"
        );

        let content = test_content("latest host rich draft");
        let content_for_save = content.clone();
        let session_id_for_rich_copy = session_id.clone();
        let rich_destination = root.join("outside/rich-copy.cuenote");
        tauri::async_runtime::block_on(cached_export(state.clone(), move |workspace| {
            workspace.save_current_content_as(
                &session_id_for_rich_copy,
                content_for_save,
                rich_destination,
            )
        }))
        .unwrap();
        let rich = crate::storage::open_note(&root.join("outside/rich-copy.cuenote")).unwrap();
        assert_eq!(rich.package.content(), content);
        assert_eq!(
            crate::storage::open_note(
                &root
                    .join("library-away/notes")
                    .join(format!("{note_id}.cuenote"))
            )
            .unwrap()
            .revision,
            before.revision
        );
    }

    #[test]
    fn missing_library_pointer_blocks_host_reads_and_mutations_but_allows_cached_export() {
        let (temp, workspace, session_id, note_id) = fixture();
        let root = temp.path();
        let library = workspace.library_path().unwrap();
        let pointer_directory = root.join("recovery/library-restores");
        std::fs::create_dir_all(&pointer_directory).unwrap();
        std::fs::write(
            pointer_directory.join(format!(
                "active-{}.json",
                crate::storage::library_access::library_key(&library)
            )),
            b"pending restore",
        )
        .unwrap();
        std::fs::rename(&library, root.join("library-away")).unwrap();
        let state = WorkspaceState::new(workspace);

        let replacement = root.join("blocked-replacement-library");
        let switch_error =
            tauri::async_runtime::block_on(switch_library(state.clone(), move |workspace| {
                workspace.select_library(replacement)
            }))
            .unwrap_err();
        assert_eq!(switch_error.kind, "libraryRestorePending");

        let read_error = tauri::async_runtime::block_on(work(state.clone(), move |workspace| {
            workspace.open_note(&note_id)
        }))
        .unwrap_err();
        assert_eq!(read_error.kind, "libraryRestorePending");

        let mutation_error =
            tauri::async_runtime::block_on(mutate(state.clone(), |workspace| workspace.new_note()))
                .unwrap_err();
        assert_eq!(mutation_error.kind, "libraryRestorePending");

        let recovery_list_error = tauri::async_runtime::block_on(recovery_read(
            state.clone(),
            Workspace::list_recovery_drafts,
        ))
        .unwrap_err();
        assert_eq!(recovery_list_error.kind, "libraryRestorePending");

        let preserved_preview_error =
            tauri::async_runtime::block_on(recovery_session_work(state.clone(), |workspace| {
                workspace.open_preserved_version("not-a-real-version")
            }))
            .unwrap_err();
        assert_eq!(preserved_preview_error.kind, "libraryRestorePending");

        assert!(!library.exists());

        let destination = root.join("outside/pending-cached-copy.cuenote");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        tauri::async_runtime::block_on(cached_export(state, move |workspace| {
            workspace.save_current_as(
                &session_id,
                test_text("cached export while pending"),
                destination,
            )
        }))
        .unwrap();
        let copied =
            crate::storage::open_note(&root.join("outside/pending-cached-copy.cuenote")).unwrap();
        assert_eq!(
            copied.package.text_projection().content,
            "cached export while pending"
        );
        assert!(!library.exists());
    }

    #[test]
    fn cached_export_does_not_require_the_library_access_lock() {
        let (temp, workspace, session_id, note_id) = fixture();
        let root = temp.path();
        let library = workspace.library_path().unwrap();
        let source = library.join("notes").join(format!("{note_id}.cuenote"));
        let before = crate::storage::open_note(&source).unwrap();
        let _exclusive =
            crate::storage::library_access::lock_exclusive(&root.join("recovery"), &library)
                .unwrap();

        let state = WorkspaceState::new(workspace);
        let destination = root.join("outside/library-lock-copy.cuenote");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let session_for_copy = session_id.clone();
        tauri::async_runtime::block_on(cached_export(state, move |workspace| {
            workspace.save_current_as(
                &session_for_copy,
                test_text("copy while old library lock is unavailable"),
                destination,
            )
        }))
        .unwrap();

        let copied =
            crate::storage::open_note(&root.join("outside/library-lock-copy.cuenote")).unwrap();
        assert_eq!(
            copied.package.text_projection().content,
            "copy while old library lock is unavailable"
        );
        assert_eq!(
            crate::storage::open_note(&source).unwrap().revision,
            before.revision
        );
    }

    #[test]
    fn host_library_switch_can_leave_missing_library_without_pointer() {
        let (temp, workspace, _session_id, _note_id) = fixture();
        let root = temp.path();
        let old_library = workspace.library_path().unwrap();
        std::fs::rename(&old_library, root.join("library-away")).unwrap();
        let selected_path = root.join("replacement-library");
        let selected = tauri::async_runtime::block_on(switch_library(
            WorkspaceState::new(workspace),
            move |workspace| workspace.select_library(selected_path),
        ))
        .unwrap();
        assert!(selected.library.unwrap().notes.is_empty());
        assert!(root.join("replacement-library/notes").is_dir());
    }
}
