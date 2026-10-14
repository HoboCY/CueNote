//! Desktop commands accept session/note identities, never frontend filesystem paths.
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::note_format::PlainText;
use crate::workspace::{
    NoteSession, RecoveryList, SaveReceipt, SavedCopy, Workspace, WorkspaceError, WorkspaceView,
};

pub type WorkspaceState = Arc<Mutex<Workspace>>;

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
        let mut workspace = state.lock().map_err(|_| {
            command_error("笔记服务已中断。请保留当前文字，重新启动 CueNote 后再试。")
        })?;
        action(&mut workspace)
    })
    .await
    .map_err(|error| command_error(format!("笔记操作未完成：{error}")))?
}

#[tauri::command]
pub async fn get_workspace(
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceView, WorkspaceError> {
    work(state.inner().clone(), Workspace::snapshot).await
}

#[tauri::command]
pub async fn use_default_library(
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceView, WorkspaceError> {
    work(state.inner().clone(), Workspace::use_default_library).await
}

#[tauri::command]
pub async fn choose_library(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
) -> Result<Option<WorkspaceView>, WorkspaceError> {
    // The blocking picker must run off the event-loop thread, especially on macOS.
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
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
    work(state.inner().clone(), move |workspace| {
        workspace.select_library(path).map(Some)
    })
    .await
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
pub async fn new_note(state: State<'_, WorkspaceState>) -> Result<NoteSession, WorkspaceError> {
    work(state.inner().clone(), Workspace::new_note).await
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
    work(state.inner().clone(), move |workspace| {
        workspace.save_note_text(&session_id, text)
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
    work(state.inner().clone(), |_| Ok(())).await?;
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
    work(state.inner().clone(), move |workspace| {
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
pub async fn save_current_as(
    app: AppHandle,
    state: State<'_, WorkspaceState>,
    session_id: String,
    text: PlainText,
) -> Result<Option<SavedCopy>, WorkspaceError> {
    let Some(path) = pick_copy_path(app).await? else {
        return Ok(None);
    };
    work(state.inner().clone(), move |workspace| {
        workspace.save_current_as(&session_id, text, path).map(Some)
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
    work(state.inner().clone(), move |workspace| {
        workspace.save_recovery_as(&recovery_id, path).map(Some)
    })
    .await
}
