pub mod storage;

use std::path::{Component, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

use storage::{
    info_at, open_at, save_at, scratch_root, simulate_external_update_at, OpenResult,
    PrototypeInfo, RevisionResult, SaveResult, Snapshot,
};

#[derive(Clone, Default)]
pub struct StorageLock(pub Arc<Mutex<()>>);

async fn run_blocking<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| error.to_string())?
}

fn prototype_root(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("CUENOTE_PROTOTYPE_SCRATCH_DIR") {
        let root = PathBuf::from(configured);
        if !root.is_absolute()
            || root.file_name().and_then(|name| name.to_str()) != Some("PROTOTYPE-wipe-me")
            || root.components().any(|component| component == Component::ParentDir)
        {
            return Err("prototype scratch override must be an absolute path ending in PROTOTYPE-wipe-me without '..'".to_owned());
        }
        return Ok(root);
    }
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("resolve local app data directory: {error}"))?;
    Ok(scratch_root(&local_data))
}

fn lock_error() -> String {
    "scratch storage lock was poisoned".to_owned()
}

#[tauri::command]
async fn prototype_info(app: AppHandle) -> Result<PrototypeInfo, String> {
    run_blocking(move || {
        let root = prototype_root(&app)?;
        info_at(&root).map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
async fn prototype_open(
    app: AppHandle,
    storage_lock: State<'_, StorageLock>,
) -> Result<OpenResult, String> {
    let storage_lock = storage_lock.0.clone();
    run_blocking(move || {
        let root = prototype_root(&app)?;
        let _guard = storage_lock.lock().map_err(|_| lock_error())?;
        open_at(&root).map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
async fn prototype_save(
    app: AppHandle,
    storage_lock: State<'_, StorageLock>,
    snapshot: Snapshot,
    expected_revision: Option<String>,
) -> Result<SaveResult, String> {
    let storage_lock = storage_lock.0.clone();
    run_blocking(move || {
        let root = prototype_root(&app)?;
        let _guard = storage_lock.lock().map_err(|_| lock_error())?;
        save_at(&root, snapshot, expected_revision).map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
async fn prototype_simulate_external_update(
    app: AppHandle,
    storage_lock: State<'_, StorageLock>,
) -> Result<RevisionResult, String> {
    let storage_lock = storage_lock.0.clone();
    run_blocking(move || {
        let root = prototype_root(&app)?;
        let _guard = storage_lock.lock().map_err(|_| lock_error())?;
        simulate_external_update_at(&root).map_err(|error| error.to_string())
    })
    .await
}

pub fn run() {
    tauri::Builder::default()
        .manage(StorageLock::default())
        .invoke_handler(tauri::generate_handler![
            prototype_info,
            prototype_open,
            prototype_save,
            prototype_simulate_external_update
        ])
        .run(tauri::generate_context!())
        .expect("CueNote storage prototype failed to start");
}
