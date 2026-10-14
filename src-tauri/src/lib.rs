mod commands;
pub mod note_format;
pub mod storage;
pub mod workspace;

use std::sync::{Arc, Mutex};
use tauri::Manager;

#[tauri::command]
fn get_app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn create_workspace(app: &tauri::App) -> Result<workspace::Workspace, Box<dyn std::error::Error>> {
    // Integration runs exercise the real host against an explicitly isolated directory.
    // Release builds never read this override.
    #[cfg(debug_assertions)]
    if let Some(root) = std::env::var_os("CUENOTE_TEST_ROOT") {
        let root = std::path::PathBuf::from(root);
        if !root.is_absolute() {
            return Err("CUENOTE_TEST_ROOT must be an absolute path".into());
        }
        return Ok(workspace::Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            root.join("笔记库"),
        ));
    }

    let documents = app
        .path()
        .document_dir()
        .or_else(|_| app.path().home_dir().map(|home| home.join("Documents")))?;
    Ok(workspace::Workspace::new(
        app.path().app_config_dir()?,
        app.path().app_local_data_dir()?.join("recovery"),
        documents.join("CueNote"),
    ))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let workspace: commands::WorkspaceState = Arc::new(Mutex::new(create_workspace(app)?));
            app.manage(workspace);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_version,
            commands::get_workspace,
            commands::use_default_library,
            commands::choose_library,
            commands::open_library_folder,
            commands::new_note,
            commands::open_note,
            commands::save_note_text,
        ])
        .run(tauri::generate_context!())
        .expect("error while running CueNote");
}
