//! Native notifications request a fingerprint check; they never mutate notes.
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

struct WatchedLibrary {
    path: PathBuf,
    _watcher: RecommendedWatcher,
    needs_rearm: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct LibraryWatch {
    watched: Mutex<Option<WatchedLibrary>>,
}

pub(crate) fn configure(app: &AppHandle, library: Option<&mut crate::workspace::LibraryView>) {
    let state = app.state::<LibraryWatch>();
    let Ok(mut current) = state.watched.lock() else {
        return;
    };
    let Some(library) = library else {
        *current = None;
        return;
    };
    let _policy = match crate::storage::NoCloudDownload::enter() {
        Ok(policy) => policy,
        Err(error) => {
            library.warnings.push(error.message);
            return;
        }
    };
    let path = PathBuf::from(&library.path);
    if current.as_ref().is_some_and(|existing| {
        existing.path == path && !existing.needs_rearm.load(Ordering::Acquire)
    }) {
        return;
    }
    let emitter = app.clone();
    let selected_path = path.clone();
    let needs_rearm = Arc::new(AtomicBool::new(false));
    let callback_rearm = Arc::clone(&needs_rearm);
    let create = || -> notify::Result<RecommendedWatcher> {
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let should_check = match event {
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
                Ok(event) => {
                    // Recreating a directory does not necessarily reattach an
                    // OS watcher to the new object at the same path. The next
                    // probe rearms it; ordinary descendant edits do not.
                    if event.paths.is_empty()
                        || event
                            .paths
                            .iter()
                            .any(|changed| selected_path.starts_with(changed))
                    {
                        callback_rearm.store(true, Ordering::Release);
                    }
                    event.paths.is_empty()
                        || event.paths.iter().any(|changed| {
                            changed.starts_with(&selected_path)
                                || selected_path.starts_with(changed)
                        })
                }
                Err(_) => {
                    callback_rearm.store(true, Ordering::Release);
                    true // Overflow/errors also invalidate assumptions.
                }
            };
            if should_check {
                let _ = emitter.emit("cuenote:files-changed", ());
            }
        })?;
        // Keep an ancestor watch even while synchronization temporarily removes
        // the library or one of its parents. Do not recreate missing data here.
        if let Some(parent) = path.ancestors().skip(1).find(|parent| parent.is_dir()) {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }
        if path.is_dir() {
            watcher.watch(&path, RecursiveMode::Recursive)?;
        }
        Ok(watcher)
    };
    match create() {
        Ok(watcher) => {
            *current = Some(WatchedLibrary {
                path,
                _watcher: watcher,
                needs_rearm,
            });
        }
        Err(error) => {
            if current
                .as_ref()
                .is_some_and(|existing| existing.path != path)
            {
                *current = None;
            }
            library.warnings.push(format!(
                "暂时无法监听文件变化；重新聚焦和保存时仍会核对版本。{error}"
            ));
        }
    }
}
