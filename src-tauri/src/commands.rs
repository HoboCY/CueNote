//! Desktop commands accept session/note identities, never frontend filesystem paths.
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::note_format::PlainText;
use crate::workspace::{NoteSession, SaveReceipt, Workspace, WorkspaceError, WorkspaceView};

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
