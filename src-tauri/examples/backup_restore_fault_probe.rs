//! Isolated child process for single-note restore interruption checks.
//! This feature-gated debug example is never bundled with CueNote.

#[cfg(debug_assertions)]
fn main() {
    use cuenote_lib::workspace::{BackupControl, Workspace, WorkspaceError};
    use std::io::Write;
    use std::path::PathBuf;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(
        args.len(),
        4,
        "backup_restore_fault_probe <isolated-root> <backup-id> <entry-id> <revision>"
    );
    let root = PathBuf::from(&args[0]);
    assert!(
        root.is_absolute() && root.is_dir(),
        "isolated root must exist"
    );
    let fault_root =
        PathBuf::from(std::env::var_os("CUENOTE_FAULT_ROOT").expect("isolated fault root"));
    assert_eq!(
        std::fs::canonicalize(&root).unwrap(),
        std::fs::canonicalize(fault_root).unwrap()
    );

    let mut restore_failure_diagnostics = None;
    let result = (|| -> Result<_, WorkspaceError> {
        let library = root.join("library");
        let mut workspace = Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            library.clone(),
        );
        workspace.snapshot()?;
        workspace.select_library(library)?;
        let context = workspace.backup_context()?;
        let control = BackupControl::default();
        let job = control.begin_job(None)?;
        let selected = context
            .list_notes(&args[1], &job)?
            .notes
            .into_iter()
            .find(|note| note.entry_id == args[2] && note.revision == args[3])
            .ok_or_else(|| {
                WorkspaceError::new("backupNoteMissing", "测试探针找不到所选备份笔记。")
            })?;
        let note_id = selected.note_id;
        let source = context.load_note_entry(&args[1], &args[2], &args[3], &job)?;
        let session_before = workspace.open_note(&note_id)?;
        let prepared = workspace.prepare_backup_note_restore(source, None)?;
        let protected = prepared.protect_current("2026-10-15", &job)?;
        let _mutation = job.into_mutation()?;
        let result = workspace.restore_backup_note(protected);
        if let Err(error) = &result {
            if std::env::var("CUENOTE_FAULT_ACTION").ok().as_deref() == Some("pause") {
                let session_after = workspace.snapshot()?.session;
                let same_session_id = session_after
                    .as_ref()
                    .is_some_and(|session| session.session_id == session_before.session_id);
                let same_content = session_after
                    .as_ref()
                    .is_some_and(|session| session.content == session_before.content);

                let mut draft_content = session_before.content.clone();
                draft_content.title = "恢复失败后仍可保存的草稿".into();
                let draft_save_kind = workspace
                    .save_note_content(&session_before.session_id, draft_content.clone())
                    .err()
                    .map(|error| error.kind);
                let draft_preserved = workspace.list_recovery_drafts().ok().is_some_and(|list| {
                    list.drafts.iter().any(|draft| {
                        draft.note_id == session_before.note_id
                            && draft.title == draft_content.title
                    })
                });

                let copy_path = root.join("saved-after-failed-restore.cuenote");
                let save_as = workspace.save_current_content_as(
                    &session_before.session_id,
                    draft_content.clone(),
                    copy_path.clone(),
                );
                let (save_as_succeeded, save_as_error) = match save_as {
                    Ok(copy) => (
                        cuenote_lib::storage::open_note(&copy_path).is_ok_and(|opened| {
                            opened.revision == copy.revision
                                && opened.package.content() == draft_content
                        }),
                        None,
                    ),
                    Err(error) => (false, Some(error.message)),
                };
                restore_failure_diagnostics = Some(serde_json::json!({
                    "sessionBefore": session_before,
                    "sessionAfter": session_after,
                    "sameSessionId": same_session_id,
                    "sameContent": same_content,
                    "draftSaveKind": draft_save_kind,
                    "draftPreserved": draft_preserved,
                    "saveAsSucceeded": save_as_succeeded,
                    "saveAsError": save_as_error,
                    "restoreError": error,
                }));
            }
        }
        result
    })();
    match result {
        Ok(receipt) => println!("{}", serde_json::to_string(&receipt).unwrap()),
        Err(error) => {
            if let Some(mut diagnostics) = restore_failure_diagnostics {
                diagnostics["error"] = serde_json::to_value(error).unwrap();
                println!("{}", serde_json::to_string(&diagnostics).unwrap());
            } else {
                println!("{}", serde_json::to_string(&error).unwrap());
            }
            std::io::stdout().flush().expect("flush restore error");
            std::process::exit(1);
        }
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The restore fault probe is available in debug builds only.");
    std::process::exit(2);
}
