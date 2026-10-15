//! Child process for isolated native-backup interruption tests.
//! This example is feature-gated and is never bundled with CueNote.

#[cfg(debug_assertions)]
fn main() {
    use cuenote_lib::workspace::{BackupControl, BackupKind, Workspace};
    use std::io::Write;
    use std::path::PathBuf;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert!(
        args.len() == 3,
        "backup_fault_probe <isolated-root> <automatic|manual> <YYYY-MM-DD>"
    );
    let root = PathBuf::from(&args[0]);
    assert!(root.is_absolute(), "test root must be absolute");
    let kind = match args[1].as_str() {
        "automatic" => BackupKind::Automatic,
        "manual" => BackupKind::Manual,
        _ => panic!("backup kind must be automatic or manual"),
    };

    let library = root.join("library");
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        library.clone(),
    );
    workspace.snapshot().expect("reopen isolated workspace");
    workspace
        .select_library(library)
        .expect("select isolated library");
    let context = workspace
        .backup_context()
        .expect("capture isolated backup context");
    let control = BackupControl::default();
    let job = control.begin_job(None).expect("begin isolated backup job");
    match context.run(kind, &args[2], &job) {
        Ok(outcome) => {
            println!(
                "{}",
                serde_json::json!({
                    "kind": "ok",
                    "created": outcome.created,
                    "backupCount": outcome.catalog.backups.len(),
                    "message": outcome.message
                })
            );
        }
        Err(error) => {
            println!("{}", serde_json::to_string(&error).unwrap());
            std::io::stdout()
                .flush()
                .expect("flush backup error output");
            std::process::exit(1);
        }
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The backup fault probe is available in debug builds only.");
    std::process::exit(2);
}
