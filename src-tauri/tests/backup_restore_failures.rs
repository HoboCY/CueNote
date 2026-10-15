#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use cuenote_lib::note_format::NotePackage;
use cuenote_lib::storage;
use cuenote_lib::workspace::{
    BackupControl, BackupKind, BackupNoteSummary, BackupSummary, Workspace,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    note_path: PathBuf,
    source: NotePackage,
    current: NotePackage,
    source_backup: BackupSummary,
    entry: BackupNoteSummary,
    backup_note_bytes: Vec<u8>,
}

fn reopen(root: &Path) -> Workspace {
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    );
    workspace.snapshot().unwrap();
    workspace.select_library(root.join("library")).unwrap();
    workspace
}

fn fixture() -> Fixture {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("backup-restore-fault-")
        .tempdir_in(base)
        .unwrap();
    let root = temp.path().to_path_buf();
    let mut workspace = reopen(&root);
    let library = workspace.snapshot().unwrap().library.unwrap();
    workspace.create_notebook("恢复前须保护的空笔记本").unwrap();
    let mut source = storage::open_note(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid-rich-v1.cuenote"),
    )
    .unwrap()
    .package;
    source.document.note_id = uuid::Uuid::new_v4().to_string();
    source.document.organization.notebook_id = library.default_notebook_id;
    source.document.organization.pinned = true;
    source.document.organization.trashed_at = None;
    let note_path = root
        .join("library/notes")
        .join(format!("{}.cuenote", source.document.note_id));
    storage::save_note(&note_path, &root.join("recovery"), &source, None).unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let source_backup = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    drop(job);
    let list_job = control.begin_job(None).unwrap();
    let entry = context
        .list_notes(&source_backup.backup_id, &list_job)
        .unwrap()
        .notes
        .remove(0);
    drop(list_job);
    let source_path = Path::new(&source_backup.path)
        .join("notes")
        .join(note_path.file_name().unwrap());
    let backup_note_bytes = fs::read(source_path).unwrap();
    let active = workspace.open_note(&source.document.note_id).unwrap();
    let mut content = active.content;
    content.title = "最新已保存内容：必须在恢复前保护".into();
    workspace
        .save_note_content(&active.session_id, content)
        .unwrap();
    let current = storage::open_note(&note_path).unwrap().package;
    assert_ne!(current, source);
    drop(workspace);
    Fixture {
        _temp: temp,
        root,
        note_path,
        source,
        current,
        source_backup,
        entry,
        backup_note_bytes,
    }
}

fn fault_probe(fixture: &Fixture, stage: &str, action: &str) -> Output {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!(
            "backup_restore_fault_probe{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        binary.is_file(),
        "build the backup_restore_fault_probe example first"
    );
    Command::new(binary)
        .args([
            fixture.root.to_str().unwrap(),
            &fixture.source_backup.backup_id,
            &fixture.entry.entry_id,
            &fixture.entry.revision,
        ])
        .env("CUENOTE_FAULT_ROOT", &fixture.root)
        .env("CUENOTE_FAULT_STAGE", stage)
        .env("CUENOTE_FAULT_ACTION", action)
        .output()
        .unwrap()
}

fn fault_probe_paused_after_publish_with_notebook_conflict(fixture: &Fixture) -> Output {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!(
            "backup_restore_fault_probe{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        binary.is_file(),
        "build the backup_restore_fault_probe example first"
    );
    let mut child = Command::new(binary)
        .args([
            fixture.root.to_str().unwrap(),
            &fixture.source_backup.backup_id,
            &fixture.entry.entry_id,
            &fixture.entry.revision,
        ])
        .env("CUENOTE_FAULT_ROOT", &fixture.root)
        .env("CUENOTE_FAULT_STAGE", "publish_after")
        .env("CUENOTE_FAULT_ACTION", "pause")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let marker = fixture.root.join(".cuenote-fault-hit-publish_after");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "restore probe exited before pausing after publish"
        );
        assert!(
            Instant::now() < deadline,
            "restore probe did not reach publish_after"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let notebook_id = fixture
        .source
        .document
        .organization
        .notebook_id
        .as_deref()
        .unwrap();
    let notebook_path = fixture
        .root
        .join("library/notebooks")
        .join(format!("{notebook_id}.json"));
    let conflict_path = fixture
        .root
        .join("library/notebooks/.cuenote-notebook-conflict-after-publish.json");
    fs::copy(notebook_path, conflict_path).unwrap();
    fs::write(fixture.root.join(".cuenote-fault-resume"), b"continue").unwrap();
    child.wait_with_output().unwrap()
}

fn assert_fault_reached(fixture: &Fixture, stage: &str, action: &str, output: &Output) {
    assert!(
        fixture
            .root
            .join(format!(".cuenote-fault-hit-{stage}"))
            .is_file(),
        "stage not reached: {output:?}"
    );
    let confirmed_publication_warning = stage == "publish_after" && action == "disk_full";
    let expected_exit = if action == "exit" {
        86
    } else if confirmed_publication_warning {
        0
    } else {
        1
    };
    assert_eq!(output.status.code(), Some(expected_exit), "{output:?}");
    if action != "exit" {
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        if confirmed_publication_warning {
            assert!(
                response["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| {
                        warning
                            .as_str()
                            .is_some_and(|message| message.contains("隔离故障注入"))
                    }),
                "post-publication failure must remain visible: {response}"
            );
        } else {
            assert!(response["message"]
                .as_str()
                .unwrap()
                .contains("隔离故障注入"));
        }
    }
}

fn assert_source_backup_retained(fixture: &Fixture) {
    let path = Path::new(&fixture.source_backup.path)
        .join("notes")
        .join(fixture.note_path.file_name().unwrap());
    assert_eq!(fs::read(path).unwrap(), fixture.backup_note_bytes);
}

#[test]
fn failed_pre_restore_backup_never_overwrites_the_current_note() {
    for action in ["exit", "disk_full"] {
        let fixture = fixture();
        let output = fault_probe(&fixture, "backup_file_copied", action);
        assert_fault_reached(&fixture, "backup_file_copied", action, &output);
        let mut reopened = reopen(&fixture.root);
        let session = reopened
            .open_note(&fixture.current.document.note_id)
            .unwrap();
        assert_eq!(session.content, fixture.current.content());
        assert_eq!(
            storage::open_note(&fixture.note_path).unwrap().package,
            fixture.current
        );
        let job = BackupControl::default().begin_job(None).unwrap();
        let catalog = reopened.backup_context().unwrap().catalog(&job).unwrap();
        assert_eq!(catalog.backups.len(), 1);
        assert_eq!(
            catalog.backups[0].backup_id,
            fixture.source_backup.backup_id
        );
        assert_source_backup_retained(&fixture);
        println!(
            "{}",
            serde_json::json!({"stage":"backup_file_copied","action":action,"currentNoteUnchanged":true,"completeBackups":1})
        );
    }
}

#[test]
fn publication_interruptions_keep_the_original_in_a_verified_pre_restore_backup() {
    for stage in ["publish_before", "publish_after"] {
        for action in ["exit", "disk_full"] {
            let fixture = fixture();
            let output = fault_probe(&fixture, stage, action);
            assert_fault_reached(&fixture, stage, action, &output);
            let mut reopened = reopen(&fixture.root);
            let session = reopened
                .open_note(&fixture.source.document.note_id)
                .unwrap();
            let actual = storage::open_note(&fixture.note_path).unwrap().package;
            let expected = if stage == "publish_before" {
                &fixture.current
            } else {
                &fixture.source
            };
            assert_eq!(&actual, expected, "stage={stage} action={action}");
            assert_eq!(session.content, expected.content());
            let job = BackupControl::default().begin_job(None).unwrap();
            let catalog = reopened.backup_context().unwrap().catalog(&job).unwrap();
            assert_eq!(catalog.backups.len(), 2);
            let pre_restore = catalog
                .backups
                .iter()
                .find(|backup| backup.backup_id != fixture.source_backup.backup_id)
                .unwrap();
            assert_eq!(pre_restore.kind, BackupKind::Manual);
            assert_eq!(pre_restore.notebook_count, 2);
            let protected_note = storage::open_note(
                &Path::new(&pre_restore.path)
                    .join("notes")
                    .join(fixture.note_path.file_name().unwrap()),
            )
            .unwrap();
            assert_eq!(protected_note.package, fixture.current);
            assert_source_backup_retained(&fixture);
            println!(
                "{}",
                serde_json::json!({"stage":stage,"action":action,"completeBackups":2,"originalProtected":true,"preRestoreBackupId":pre_restore.backup_id,"targetIsRestored":stage=="publish_after"})
            );
        }
    }
}

#[test]
fn notebook_metadata_failure_after_publish_is_reported_as_published() {
    let fixture = fixture();
    let output = fault_probe_paused_after_publish_with_notebook_conflict(&fixture);
    assert!(fixture
        .root
        .join(".cuenote-fault-hit-publish_after")
        .is_file());
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("恢复文件已发布"), "{message}");
    assert!(!message.contains("没有发布恢复"), "{message}");
    assert!(message.contains("恢复前保护备份位于"), "{message}");
    assert_eq!(response["sameSessionId"], true, "{response}");
    assert_eq!(response["sameContent"], true, "{response}");
    assert_eq!(response["draftSaveKind"], "conflict", "{response}");
    assert_eq!(response["draftPreserved"], true, "{response}");
    assert_eq!(response["saveAsSucceeded"], true, "{response}");
    assert_eq!(
        storage::open_note(&fixture.note_path).unwrap().package,
        fixture.source
    );
    assert_source_backup_retained(&fixture);

    let reopened = reopen(&fixture.root);
    let job = BackupControl::default().begin_job(None).unwrap();
    let catalog = reopened.backup_context().unwrap().catalog(&job).unwrap();
    assert_eq!(catalog.backups.len(), 2);
    let pre_restore = catalog
        .backups
        .iter()
        .find(|backup| backup.backup_id != fixture.source_backup.backup_id)
        .unwrap();
    let protected_note = storage::open_note(
        &Path::new(&pre_restore.path)
            .join("notes")
            .join(fixture.note_path.file_name().unwrap()),
    )
    .unwrap();
    assert_eq!(protected_note.package, fixture.current);
}
