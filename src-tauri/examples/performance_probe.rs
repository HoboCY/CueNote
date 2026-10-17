//! Diagnostic host timings, NOT desktop acceptance scores. Operates only on an
//! explicitly marked disposable copy; the immutable corpus is never modified.
use std::{error::Error, fs, path::PathBuf, time::Instant};

use cuenote_lib::workspace::Workspace;
use serde_json::json;

fn elapsed(operation: &str, start: Instant, detail: serde_json::Value) {
    println!(
        "{}",
        json!({"operation":operation,"elapsedMs":start.elapsed().as_secs_f64()*1000.0,
            "scope":"host-only","optimized":!cfg!(debug_assertions),"detail":detail})
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(
            "Usage: performance_probe <isolated root> <ordinary note id> <stress note id>".into(),
        );
    }
    let root = PathBuf::from(&args[0]);
    if !root.is_absolute()
        || fs::read(root.join(".cuenote-performance"))? != b"CueNote isolated performance root v1\n"
    {
        return Err("An absolute, explicitly marked disposable root is required".into());
    }
    let root = root.canonicalize()?;
    let library = root.join("library").canonicalize()?;
    if library.parent() != Some(root.as_path()) {
        return Err("Library must be a direct child of the disposable root".into());
    }
    let make_workspace = || {
        Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            library.clone(),
        )
    };
    let mut workspace = make_workspace();
    let start = Instant::now();
    let view = workspace.select_library(library.clone())?;
    let count = view.library.as_ref().map_or(0, |v| v.notes.len());
    if count != 1000 {
        return Err(format!("Expected 1000 readable notes; found {count}").into());
    }
    elapsed("initial-index", start, json!({"notes":count}));
    for (profile, id) in [("ordinary", &args[1]), ("stress", &args[2])] {
        let start = Instant::now();
        let session = workspace.open_note(id)?;
        elapsed("open", start, json!({"profile":profile,"noteId":id}));
        let mut content = session.content.clone();
        content.title.push_str(" · host measurement");
        let start = Instant::now();
        let receipt = workspace.save_note_content(&session.session_id, content.clone())?;
        elapsed(
            "save",
            start,
            json!({"profile":profile,"revision":receipt.session.revision}),
        );
        if workspace.open_note(id)?.content != content {
            return Err("Saved content differs on reopen".into());
        }
        let start = Instant::now();
        let result = workspace.search_notes("host measurement", None, 0)?;
        elapsed(
            "search",
            start,
            json!({"hits":result.total,"indexed":result.indexed_notes}),
        );
        if !result.hits.iter().any(|hit| &hit.note.note_id == id) {
            return Err("Saved content missing from index".into());
        }
    }
    workspace.open_note(&args[1])?;
    drop(workspace);
    let start = Instant::now();
    let view = make_workspace().snapshot()?;
    elapsed(
        "restart-snapshot",
        start,
        json!({"session":view.session.as_ref().map(|s|&s.note_id)}),
    );
    if view.session.as_ref().map(|s| &s.note_id) != Some(&args[1]) {
        return Err("Restart did not reopen the ordinary note".into());
    }
    Ok(())
}
