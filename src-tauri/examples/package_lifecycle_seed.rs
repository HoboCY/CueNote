// CI-only data generator. It is an example, never bundled into the application.
use cuenote_lib::note_format::PlainText;
use cuenote_lib::workspace::{BackupControl, BackupKind, Workspace};
use std::path::PathBuf;

fn main() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    assert_eq!(paths.len(), 3);
    for path in &paths {
        assert!(path.is_absolute() && !path.exists(), "Refuse existing data");
    }
    let mut workspace = Workspace::new(paths[0].clone(), paths[1].clone(), paths[2].clone());
    workspace.use_default_library().unwrap();
    let note = workspace.new_note().unwrap();
    for content in ["已保存的历史版本", "安装升级验收：1321312321312312"] {
        workspace
            .save_note_text(
                &note.session_id,
                PlainText {
                    title: "安装器保留数据".into(),
                    cue: "线索".into(),
                    content: content.into(),
                    summary: "总结".into(),
                },
            )
            .unwrap();
    }
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let result = workspace
        .backup_context()
        .unwrap()
        .run(BackupKind::Manual, "2026-10-17", &job)
        .unwrap();
    assert!(result.created.is_some());
    println!("Seeded native note, saved history and verified backup");
}
