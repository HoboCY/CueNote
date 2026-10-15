mod commands;
mod file_watch;
pub mod note_format;
pub mod storage;
mod storage_faults;
pub mod workspace;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Editing is enabled only after the frontend has subscribed to close requests
/// and armed this gate. Native close and application quit share the same path.
#[derive(Default)]
pub(crate) struct CloseGate {
    armed: AtomicBool,
    approved: AtomicBool,
}

impl CloseGate {
    fn needs_confirmation(&self) -> bool {
        self.armed.load(Ordering::Acquire) && !self.approved.load(Ordering::Acquire)
    }
}

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
        .manage(CloseGate::default())
        .manage(file_watch::LibraryWatch::default())
        .setup(|app| {
            let mut workspace = create_workspace(app)?;
            let watcher = app.state::<file_watch::LibraryWatch>();
            workspace.attach_search_generation(watcher.search_generation());
            let emitter = app.handle().clone();
            workspace.attach_search_progress(Arc::new(
                move |progress: workspace::SearchProgress| {
                    let _ = emitter.emit("cuenote:search-progress", progress);
                },
            ));
            let workspace = commands::WorkspaceState::new(workspace);
            app.manage(workspace);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(true) = event {
                let watcher = window.state::<file_watch::LibraryWatch>();
                file_watch::invalidate_search_index(&watcher);
                let _ = window.emit("cuenote:files-changed", ());
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.state::<CloseGate>().needs_confirmation() {
                    api.prevent_close();
                    let _ = window.emit("cuenote:close-requested", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_app_version,
            commands::get_workspace,
            commands::search_notes,
            commands::rebuild_search_index,
            commands::run_backup,
            commands::list_backups,
            commands::list_backup_notes,
            commands::restore_backup_note,
            commands::choose_backup_directory,
            commands::cancel_backup,
            commands::use_default_library,
            commands::initialize_selected_library,
            commands::choose_library,
            commands::open_library_folder,
            commands::new_note,
            commands::create_notebook,
            commands::rename_notebook,
            commands::delete_notebook,
            commands::update_note_organization,
            commands::trash_note,
            commands::restore_trashed_note,
            commands::delete_trashed_note,
            commands::set_view_preferences,
            commands::open_note,
            commands::save_note_text,
            commands::save_note_content,
            commands::stage_image_raw,
            commands::preflight_image_insert,
            commands::preflight_note_content,
            commands::read_image_asset,
            commands::activate_close_guard,
            commands::complete_close,
            commands::list_recovery_drafts,
            commands::restore_recovery_as_copy,
            commands::save_current_as,
            commands::save_current_content_as,
            commands::save_recovery_as,
            commands::probe_external_changes,
            commands::reconcile_external_changes,
            commands::list_preserved_versions,
            commands::open_preserved_version,
            commands::restore_preserved_as_copy,
            commands::resolve_preserved_version,
            commands::save_preserved_as,
        ])
        .build(tauri::generate_context!())
        .expect("error while building CueNote")
        .run(|app, event| {
            // Includes application-level quit (for example macOS Cmd+Q), which
            // does not have to arrive as a window close request.
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if app.state::<CloseGate>().needs_confirmation() {
                    api.prevent_exit();
                    let _ = app.emit("cuenote:close-requested", ());
                }
            }
        });
}
