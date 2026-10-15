//! Desktop commands accept session/note identities, never frontend filesystem paths.
use std::sync::{Arc, Mutex};

use tauri::{
    ipc::{InvokeBody, Request, Response},
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_dialog::DialogExt;

use crate::note_format::{AssetManifestEntry, NoteContent, PlainText};
use crate::workspace::{
    BackupCatalog, BackupContext, BackupControl, BackupJob, BackupKind, BackupOutcome,
    BackupProgress, ExternalProbe, ExternalReconciliation, LibraryView, NoteSession, RecoveryList,
    SaveReceipt, SavedCopy, SearchResults, VersionList, Workspace, WorkspaceError, WorkspaceView,
};

#[derive(Clone)]
pub struct WorkspaceState {
    workspace: Arc<Mutex<Workspace>>,
    backups: BackupControl,
}

impl WorkspaceState {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace: Arc::new(Mutex::new(workspace)),
            backups: BackupControl::default(),
        }
    }

    pub(crate) fn cancel_backup(&self) {
        self.backups.cancel();
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
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
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
        let _mutation = state.backups.begin_mutation();
        let mut workspace = state.workspace.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        let _policy = crate::storage::NoCloudDownload::enter()?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记操作未完成：{error}")))?
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

async fn backup_work<T: Send + 'static>(
    app: AppHandle,
    state: WorkspaceState,
    request_id: String,
    expected_library_path: String,
    selected_directory: Option<std::path::PathBuf>,
    action: impl FnOnce(BackupContext, &BackupJob) -> Result<T, WorkspaceError> + Send + 'static,
) -> Result<T, WorkspaceError> {
    if request_id.is_empty() || request_id.len() > 128 || request_id.chars().any(char::is_control) {
        return Err(command_error("备份请求身份无效，请重试。"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _policy = crate::storage::NoCloudDownload::enter()?;
        // Directory changes invalidate a job just like a library transition.
        let mutation = selected_directory
            .as_ref()
            .map(|_| state.backups.begin_mutation());
        let mut workspace = state
            .workspace
            .lock()
            .map_err(|_| command_error("笔记服务已中断，请重新启动 CueNote 后重试备份。"))?;
        require_backup_library(&workspace, &expected_library_path)?;
        let context = if let Some(directory) = selected_directory {
            workspace.set_backup_directory(directory)?
        } else {
            workspace.backup_context()?
        };
        drop(mutation);
        let job = state.backups.begin_job(Some(Arc::new(move |progress| {
            let _ = app.emit(
                "cuenote:backup-progress",
                DesktopBackupProgress {
                    request_id: request_id.clone(),
                    progress,
                },
            );
        })))?;
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
    let mut view = mutate(state.inner().clone(), Workspace::snapshot).await?;
    crate::file_watch::configure(&app, view.library.as_mut());
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
    let mut view = mutate(state.inner().clone(), |workspace| {
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
    let mut view = mutate(state.inner().clone(), move |workspace| {
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
pub async fn save_note_text(
    state: State<'_, WorkspaceState>,
    session_id: String,
    text: PlainText,
) -> Result<SaveReceipt, WorkspaceError> {
    mutate(state.inner().clone(), move |workspace| {
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
    mutate(state.inner().clone(), move |workspace| {
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
    work(state.inner().clone(), move |workspace| {
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
    work(state.inner().clone(), move |workspace| {
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
    work(state.inner().clone(), move |workspace| {
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
    work(state.inner().clone(), move |workspace| {
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
    mutate(state.inner().clone(), |workspace| {
        // Preserve a bounded handoff when a quick close beats the automatic
        // backup debounce. Do this before releasing the editing session so a
        // local settings failure leaves the existing close-retry path usable.
        workspace.mark_backup_check_due(&chrono::Local::now().format("%Y-%m-%d").to_string())?;
        workspace.close()
    })
    .await?;
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
    work(state.inner().clone(), Workspace::list_recovery_drafts).await
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
    let mut probe = work(state.inner().clone(), move |workspace| {
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
    mutate(state.inner().clone(), move |workspace| {
        workspace.reconcile_external_changes(&session_id, content)
    })
    .await
}

#[tauri::command]
pub async fn list_preserved_versions(
    state: State<'_, WorkspaceState>,
) -> Result<VersionList, WorkspaceError> {
    work(state.inner().clone(), Workspace::list_preserved_versions).await
}

#[tauri::command]
pub async fn open_preserved_version(
    state: State<'_, WorkspaceState>,
    version_id: String,
) -> Result<NoteSession, WorkspaceError> {
    work(state.inner().clone(), move |workspace| {
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
    mutate(state.inner().clone(), move |workspace| {
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
    mutate(state.inner().clone(), move |workspace| {
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
    mutate(state.inner().clone(), move |workspace| {
        workspace
            .save_current_content_as(&session_id, content, path)
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
    mutate(state.inner().clone(), move |workspace| {
        workspace.save_recovery_as(&recovery_id, path).map(Some)
    })
    .await
}
