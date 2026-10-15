//! Native notifications request a fingerprint check; they never mutate notes.
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
pub(crate) struct LibraryWatch {
    watched: Mutex<Option<(PathBuf, RecommendedWatcher)>>,
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
    let path = PathBuf::from(&library.path);
    if current
        .as_ref()
        .is_some_and(|(existing, _)| existing == &path)
    {
        return;
    }
    let emitter = app.clone();
    let selected_path = path.clone();
    let create = || -> notify::Result<RecommendedWatcher> {
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let should_check = match event {
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
                Ok(event) => {
                    event.paths.is_empty()
                        || event.paths.iter().any(|changed| {
                            changed.starts_with(&selected_path)
                                || selected_path.starts_with(changed)
                        })
                }
                Err(_) => true, // Overflow/errors also invalidate assumptions.
            };
            if should_check {
                let _ = emitter.emit("cuenote:files-changed", ());
            }
        })?;
        watcher.watch(&path, RecursiveMode::Recursive)?;
        // The parent continues to signal removal/recreation of the chosen root.
        if let Some(parent) = path.parent() {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }
        Ok(watcher)
    };
    match create() {
        Ok(watcher) => *current = Some((path, watcher)),
        Err(error) => {
            *current = None;
            library.warnings.push(format!(
                "暂时无法监听文件变化；重新聚焦和保存时仍会核对版本。{error}"
            ));
        }
    }
}
