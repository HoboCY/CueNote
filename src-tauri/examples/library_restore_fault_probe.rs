//! Isolated child process for whole-library restore interruption checks.
//! This feature-gated debug example is never bundled with CueNote.

#[cfg(debug_assertions)]
fn main() {
    use cuenote_lib::workspace::{
        BackupControl, BackupPhase, BackupProgress, LibraryRestoreAction, Workspace, WorkspaceError,
    };
    use serde_json::{json, Value};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn workspace_at(root: &Path) -> Result<Workspace, WorkspaceError> {
        if !root.is_absolute() || !root.is_dir() {
            return Err(WorkspaceError::new(
                "probeRoot",
                "隔离探针根目录必须是已存在的绝对路径。",
            ));
        }
        if let Some(fault_root) = std::env::var_os("CUENOTE_FAULT_ROOT") {
            let fault_root = PathBuf::from(fault_root);
            if !fault_root.is_absolute()
                || std::fs::canonicalize(root).ok() != std::fs::canonicalize(fault_root).ok()
            {
                return Err(WorkspaceError::new(
                    "probeRoot",
                    "故障注入根目录与隔离 Workspace 不一致。",
                ));
            }
        }
        let library = root.join("library");
        let mut workspace = Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            library.clone(),
        );
        let view = workspace.snapshot()?;
        if view.library_restore.is_none() && view.library.is_none() {
            workspace.select_library(library)?;
        }
        Ok(workspace)
    }

    fn begin_job(
        control: &BackupControl,
    ) -> Result<cuenote_lib::workspace::BackupJob, WorkspaceError> {
        let cancel_on_copy = std::env::var("CUENOTE_PROBE_CANCEL_PHASE")
            .ok()
            .is_some_and(|phase| phase == "copying");
        if !cancel_on_copy {
            return control.begin_restore_job(None);
        }
        let callback_control = control.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        control.begin_restore_job(Some(Arc::new(move |progress: BackupProgress| {
            if progress.phase == BackupPhase::Copying && !cancelled.swap(true, Ordering::AcqRel) {
                callback_control.cancel();
            }
        })))
    }

    fn run() -> Result<Value, WorkspaceError> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let mode = args.first().map(String::as_str).unwrap_or_default();
        let root = args
            .get(1)
            .map(PathBuf::from)
            .ok_or_else(|| WorkspaceError::new("probeArgs", "缺少隔离根目录。"))?;
        let mut workspace = workspace_at(&root)?;
        let control = BackupControl::default();
        match mode {
            "restore" if args.len() == 4 => {
                let job = begin_job(&control)?;
                let result = workspace.restore_library_from_backup(
                    &args[2],
                    &args[3],
                    "2026-10-15",
                    &job,
                )?;
                Ok(json!({ "receipt": result }))
            }
            "resume" if args.len() == 3 => {
                let action = match args[2].as_str() {
                    "continue" => LibraryRestoreAction::Continue,
                    "rollback" => LibraryRestoreAction::RollBack,
                    _ => {
                        return Err(WorkspaceError::new(
                            "probeArgs",
                            "resume 操作必须是 continue 或 rollback。",
                        ));
                    }
                };
                let job = begin_job(&control)?;
                let receipt = workspace.resume_library_restore(action, &job)?;
                Ok(json!({ "receipt": receipt }))
            }
            "inspect" if args.len() == 2 => {
                let library = workspace.library_path()?;
                let pending = workspace.pending_library_restore(&library)?;
                let gate_error = if pending.is_some() {
                    workspace
                        .create_notebook("fault-probe pending gate")
                        .err()
                        .map(|error| json!({ "kind": error.kind, "message": error.message }))
                } else {
                    None
                };
                Ok(json!({
                    "workspace": workspace.snapshot()?,
                    "pending": pending,
                    "gateError": gate_error,
                }))
            }
            _ => Err(WorkspaceError::new(
                "probeArgs",
                "用法：library_restore_fault_probe <restore root backup-id revision | resume root continue|rollback | inspect root>",
            )),
        }
    }

    let (response, code) = match run() {
        Ok(response) => (response, 0),
        Err(error) => (json!({ "error": error }), 1),
    };
    println!(
        "{}",
        serde_json::to_string(&response).expect("serialize probe response")
    );
    std::io::stdout().flush().expect("flush probe response");
    if code != 0 {
        std::process::exit(code);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The library restore fault probe is available in debug builds only.");
    std::process::exit(2);
}
