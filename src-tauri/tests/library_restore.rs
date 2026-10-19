use cuenote_lib::note_format::PlainText;
use cuenote_lib::workspace::{BackupControl, BackupKind, BackupPhase, Workspace};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod support;

fn isolated_workspace() -> (tempfile::TempDir, Workspace, PathBuf) {
    let temp = support::tempdir("library-restore-");
    let library = temp.path().join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    (temp, workspace, library)
}

fn native_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for (directory, extension) in [("notes", "cuenote"), ("notebooks", "json")] {
        for entry in fs::read_dir(root.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                files.insert(
                    format!(
                        "{directory}/{}",
                        path.file_name().unwrap().to_str().unwrap()
                    ),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    files
}

#[cfg(windows)]
fn write_pending_intent(recovery: &Path, intent: &serde_json::Value) -> (PathBuf, Vec<u8>) {
    let journal = recovery.join(".pending-publications");
    fs::create_dir_all(&journal).unwrap();
    let path = journal.join(format!("{}.json", uuid::Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec(intent).unwrap();
    fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

#[cfg(windows)]
fn unrelated_pending_intent(other_library: &Path) -> serde_json::Value {
    let note_id = uuid::Uuid::new_v4().to_string();
    let notes = other_library.join("notes");
    let target = notes.join(format!("{note_id}.cuenote"));
    let candidate = notes.join(format!(
        ".cuenote-candidate-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let displaced = notes.join(format!(
        ".cuenote-publish-backup-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    serde_json::json!({
        "version": 1,
        "note_id": note_id,
        "target": target,
        "candidate": candidate,
        "displaced": displaced,
        "expected_revision": "1".repeat(64),
        "candidate_revision": "2".repeat(64),
        "directory_identity": "synthetic-unobserved-directory"
    })
}

struct InterruptedFixture {
    _temp: tempfile::TempDir,
    workspace: Workspace,
    library: PathBuf,
    control: BackupControl,
    source_id: String,
    manifest_revision: String,
    source_facts: BTreeMap<String, Vec<u8>>,
    before_facts: BTreeMap<String, Vec<u8>>,
}

fn interrupted_fixture() -> InterruptedFixture {
    let (temp, mut workspace, library) = isolated_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Source version".into(),
                cue: String::new(),
                content: "Source bytes".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    workspace.create_notebook("Source empty notebook").unwrap();
    let source_facts = native_files(&library);
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let source = workspace
        .backup_context()
        .unwrap()
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let preview = workspace
        .preview_library_restore(&source.backup_id, &job)
        .unwrap();
    drop(job);

    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Current version".into(),
                cue: String::new(),
                content: "Current bytes".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let extra = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &extra.session_id,
            PlainText {
                title: "Current only".into(),
                cue: String::new(),
                content: "Must be restored on rollback".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    workspace.create_notebook("Current-only notebook").unwrap();
    let before_facts = native_files(&library);

    InterruptedFixture {
        _temp: temp,
        workspace,
        library,
        control,
        source_id: source.backup_id,
        manifest_revision: preview.manifest_revision,
        source_facts,
        before_facts,
    }
}

fn interrupt_after_first_restored_file(fixture: &mut InterruptedFixture) {
    let control = fixture.control.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_cancelled = Arc::clone(&cancelled);
    let job = fixture
        .control
        .begin_restore_job(Some(Arc::new(move |progress| {
            if progress.phase == BackupPhase::Restoring
                && progress.checked > 0
                && !callback_cancelled.swap(true, Ordering::SeqCst)
            {
                control.cancel();
            }
        })))
        .unwrap();
    let error = fixture
        .workspace
        .restore_library_from_backup(
            &fixture.source_id,
            &fixture.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap_err();
    assert_eq!(error.kind, "backupCancelled");
    assert!(cancelled.load(Ordering::SeqCst));
    drop(job);
    let pending = fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .unwrap();
    assert_eq!(pending.phase, "paused");
    assert!(pending.can_continue);
    assert!(pending.can_roll_back);
}

fn restore_task_root(fixture: &InterruptedFixture) -> PathBuf {
    let marker: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.library.join(".cuenote-restore.json")).unwrap())
            .unwrap();
    fixture
        ._temp
        .path()
        .join("recovery/library-restores")
        .join(marker["restoreId"].as_str().unwrap())
}

fn interrupted_native_facts(fixture: &InterruptedFixture) -> BTreeMap<String, Vec<u8>> {
    native_files(&fixture.library)
}

fn cancel_restore_at_restoring_zero(fixture: &mut InterruptedFixture) -> PathBuf {
    let control = fixture.control.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_cancelled = Arc::clone(&cancelled);
    let job = fixture
        .control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == BackupPhase::Restoring
                && progress.checked == 0
                && !callback_cancelled.swap(true, Ordering::SeqCst)
            {
                control.cancel();
            }
        })))
        .unwrap();
    let error = fixture
        .workspace
        .restore_library_from_backup(
            &fixture.source_id,
            &fixture.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap_err();
    assert_eq!(error.kind, "backupCancelled");
    assert!(cancelled.load(Ordering::SeqCst));
    drop(job);
    let pending = fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .unwrap();
    assert_eq!(pending.phase, "paused");
    assert_eq!(pending.completed_files, 0);
    let task_root = restore_task_root(fixture);
    assert_eq!(fs::read_dir(task_root.join("events")).unwrap().count(), 0);
    task_root
}

fn create_unclaimed_source_only_target(fixture: &mut InterruptedFixture) -> (String, Vec<u8>) {
    let (relative, bytes) = fixture
        .source_facts
        .iter()
        .find(|(path, _)| path.ends_with(".cuenote"))
        .unwrap();
    fs::remove_file(fixture.library.join(relative.as_str())).unwrap();
    let source_path = relative.as_str().to_owned();
    let source_bytes = bytes.to_vec();
    cancel_restore_at_restoring_zero(fixture);
    fs::write(fixture.library.join(&source_path), &source_bytes).unwrap();
    (source_path, source_bytes)
}

fn finish_durable_source_state(
    fixture: &InterruptedFixture,
    remove_marker: bool,
) -> (PathBuf, String, Vec<u8>) {
    let task_root = restore_task_root(fixture);
    let plan_path = task_root.join("plan.json");
    let plan: serde_json::Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    let restore_id = plan["restoreId"].as_str().unwrap().to_owned();
    let facts_by_path = |field: &str| {
        plan[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|fact| (fact["path"].as_str().unwrap().to_owned(), fact.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let before = facts_by_path("before");
    let after = facts_by_path("after");
    let paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let changed = paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .collect::<Vec<_>>();

    for (index, _) in changed.iter().enumerate() {
        for suffix in ["forward-intent", "forward-done"] {
            let event_path = task_root
                .join("events")
                .join(format!("{index:06}-{suffix}.json"));
            if !event_path.exists() {
                fs::write(
                    event_path,
                    serde_json::to_vec(&serde_json::json!({
                        "schemaVersion": 1,
                        "restoreId": restore_id.clone(),
                        "step": index,
                        "state": null
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
        }
    }

    for (directory, extension) in [("notes", "cuenote"), ("notebooks", "json")] {
        for entry in fs::read_dir(fixture.library.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                fs::remove_file(path).unwrap();
            }
        }
    }
    for (relative, bytes) in &fixture.source_facts {
        let path = fixture.library.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fs::write(
        task_root.join("events/terminal.json"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "restoreId": restore_id.clone(),
            "step": null,
            "state": "completed"
        }))
        .unwrap(),
    )
    .unwrap();
    if remove_marker {
        fs::remove_file(fixture.library.join(".cuenote-restore.json")).unwrap();
    }

    let (relative, _) = fixture
        .source_facts
        .iter()
        .find(|(relative, source)| {
            fixture
                .before_facts
                .get(*relative)
                .is_some_and(|before| before != *source)
        })
        .unwrap();
    let external_version = fixture.before_facts[relative].clone();
    fs::write(fixture.library.join(relative.as_str()), &external_version).unwrap();
    (task_root, relative.as_str().to_owned(), external_version)
}

fn restore_id(task_root: &Path) -> String {
    let plan: serde_json::Value =
        serde_json::from_slice(&fs::read(task_root.join("plan.json")).unwrap()).unwrap();
    plan["restoreId"].as_str().unwrap().to_owned()
}

fn write_restore_event(
    task_root: &Path,
    name: &str,
    restore_id: &str,
    step: Option<usize>,
    state: Option<&str>,
) {
    fs::write(
        task_root.join("events").join(name),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "restoreId": restore_id,
            "step": step,
            "state": state
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn whole_library_restore_protects_current_facts_and_reopens_the_exact_source_set() {
    let (temp, mut workspace, library) = isolated_workspace();
    workspace.create_notebook("Source empty notebook").unwrap();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Source note".into(),
                cue: "Source cue".into(),
                content: "Source body before replacement".into(),
                summary: "Source conclusion".into(),
            },
        )
        .unwrap();
    let source_facts = native_files(&library);
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let source = workspace
        .backup_context()
        .unwrap()
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let preview = workspace
        .preview_library_restore(&source.backup_id, &job)
        .unwrap();
    drop(job);

    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Current text must be protected".into(),
                cue: "Current cue".into(),
                content: "Current body".into(),
                summary: "Current conclusion".into(),
            },
        )
        .unwrap();
    let extra = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &extra.session_id,
            PlainText {
                title: "Only in the current library".into(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();
    workspace
        .create_notebook("Only current empty notebook")
        .unwrap();
    fs::write(library.join("notes/readme.txt"), b"Unrelated file stays").unwrap();
    let before_facts = native_files(&library);

    let job = control.begin_job(None).unwrap();
    let restored = workspace
        .restore_library_from_backup(
            &source.backup_id,
            &preview.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap();
    assert_ne!(restored.pre_restore_backup.backup_id, source.backup_id);
    assert_eq!(
        native_files(Path::new(&restored.pre_restore_backup.path)),
        before_facts
    );
    assert_eq!(native_files(&library), source_facts);
    assert_eq!(native_files(Path::new(&source.path)), source_facts);
    assert_eq!(
        fs::read(library.join("notes/readme.txt")).unwrap(),
        b"Unrelated file stays"
    );
    assert!(restored.workspace.library_restore.is_none());
    assert_eq!(restored.workspace.library.as_ref().unwrap().notes.len(), 1);
    assert!(restored
        .workspace
        .library
        .as_ref()
        .unwrap()
        .notebooks
        .iter()
        .any(|notebook| notebook.name == "Source empty notebook"));
    drop(job);
    drop(workspace);

    let mut reopened = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    let view = reopened.snapshot().unwrap();
    assert!(view.library_restore.is_none());
    assert_eq!(view.library.as_ref().unwrap().notes.len(), 1);
    let session = reopened.open_note(&note.note_id).unwrap();
    assert_eq!(session.text.title, "Source note");
    assert_eq!(session.text.content, "Source body before replacement");
    assert_eq!(native_files(&library), source_facts);
}

#[cfg(windows)]
#[test]
fn unrelated_pending_publication_does_not_block_other_library_restore() {
    let mut fixture = interrupted_fixture();
    let recovery = fixture._temp.path().join("recovery");
    let other_library = fixture
        ._temp
        .path()
        .join("ordinary-win-management-9760cf6/library");
    let (record_path, record_bytes) =
        write_pending_intent(&recovery, &unrelated_pending_intent(&other_library));
    assert_ne!(
        fs::canonicalize(&recovery).unwrap(),
        recovery,
        "the recovery root must remain in non-extended form to exercise the Windows journal-path regression"
    );

    let job = fixture.control.begin_job(None).unwrap();
    let restored = fixture
        .workspace
        .restore_library_from_backup(
            &fixture.source_id,
            &fixture.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap();
    assert_eq!(native_files(&fixture.library), fixture.source_facts);
    assert_eq!(
        fs::read(&record_path).unwrap(),
        record_bytes,
        "unrelated pending publication bytes must remain untouched"
    );
    assert!(restored.workspace.library_restore.is_none());
}

#[cfg(windows)]
#[test]
fn related_pending_publication_with_changed_directory_blocks_library_restore() {
    let mut fixture = interrupted_fixture();
    let recovery = fixture._temp.path().join("recovery");
    let library = fs::canonicalize(&fixture.library).unwrap();
    let (record_path, record_bytes) =
        write_pending_intent(&recovery, &unrelated_pending_intent(&library));
    let job = fixture.control.begin_job(None).unwrap();
    let error = fixture
        .workspace
        .restore_library_from_backup(
            &fixture.source_id,
            &fixture.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap_err();
    assert_eq!(error.kind, "unsafePath");
    assert!(error.message.contains("目录身份已变化"));
    assert_eq!(native_files(&fixture.library), fixture.before_facts);
    assert_eq!(fs::read(record_path).unwrap(), record_bytes);
}

#[cfg(windows)]
#[test]
fn malformed_unrelated_pending_publication_still_blocks_library_restore() {
    let mut fixture = interrupted_fixture();
    let recovery = fixture._temp.path().join("recovery");
    let other_library = fixture._temp.path().join("OtherLibrary");
    let mut intent = unrelated_pending_intent(&other_library);
    intent["candidate"] = serde_json::json!(other_library.join("outside-notes.tmp"));
    let (record_path, record_bytes) = write_pending_intent(&recovery, &intent);
    let job = fixture.control.begin_job(None).unwrap();
    let error = fixture
        .workspace
        .restore_library_from_backup(
            &fixture.source_id,
            &fixture.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap_err();
    assert_eq!(error.kind, "unsafePath");
    assert_eq!(native_files(&fixture.library), fixture.before_facts);
    assert_eq!(fs::read(record_path).unwrap(), record_bytes);
}

#[test]
fn cancelled_restore_can_explicitly_continue_to_the_verified_source_set() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);

    let job = fixture.control.begin_job(None).unwrap();
    let receipt = fixture
        .workspace
        .resume_library_restore(cuenote_lib::workspace::LibraryRestoreAction::Continue, &job)
        .unwrap();
    assert_eq!(native_files(&fixture.library), fixture.source_facts);
    assert!(receipt.workspace.library_restore.is_none());
}

#[test]
fn cancelled_restore_can_explicitly_roll_back_to_the_protected_current_set() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);

    let job = fixture.control.begin_job(None).unwrap();
    let receipt = fixture
        .workspace
        .resume_library_restore(cuenote_lib::workspace::LibraryRestoreAction::RollBack, &job)
        .unwrap();
    assert_eq!(native_files(&fixture.library), fixture.before_facts);
    assert!(receipt.message.contains("回退"));
    assert!(receipt.workspace.library_restore.is_none());
}

#[test]
fn pending_snapshot_does_not_scan_or_replay_and_blocks_normal_workspace_operations() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let interrupted_facts = interrupted_native_facts(&fixture);

    let view = fixture.workspace.snapshot().unwrap();
    assert_eq!(view.library_restore.as_ref().unwrap().phase, "paused");
    assert_eq!(
        view.library_restore.as_ref().unwrap().library_path,
        view.library.as_ref().unwrap().path,
        "pending recovery controls must match the selected library identity"
    );
    assert!(view.library.as_ref().unwrap().notes.is_empty());
    assert_eq!(interrupted_native_facts(&fixture), interrupted_facts);

    for error in [
        fixture.workspace.new_note().unwrap_err(),
        fixture
            .workspace
            .create_notebook("Must stay blocked")
            .unwrap_err(),
        fixture.workspace.rebuild_search_index().unwrap_err(),
    ] {
        assert_eq!(error.kind, "libraryRestorePending");
    }
    let backup_error = match fixture.workspace.backup_context() {
        Ok(_) => panic!("normal backup context must remain blocked"),
        Err(error) => error,
    };
    assert_eq!(backup_error.kind, "libraryRestorePending");
    assert_eq!(interrupted_native_facts(&fixture), interrupted_facts);
}

#[test]
fn restore_survives_file_notifications_after_publishing_its_protective_backup() {
    let mut fixture = interrupted_fixture();
    let control = fixture.control.clone();
    let notified = Arc::new(AtomicBool::new(false));
    let callback_notified = Arc::clone(&notified);
    let job = fixture
        .control
        .begin_restore_job(Some(Arc::new(move |progress| {
            if progress.phase == BackupPhase::Restoring {
                // Deliver the same hint as the OS watcher after the protective
                // backup is published and the restore marker has been written.
                control.cancel_for_file_change();
                callback_notified.store(true, Ordering::SeqCst);
            }
        })))
        .unwrap();
    let receipt = fixture
        .workspace
        .restore_library_from_backup(
            &fixture.source_id,
            &fixture.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap();
    assert!(notified.load(Ordering::SeqCst));
    assert_eq!(native_files(&fixture.library), fixture.source_facts);
    assert!(receipt.workspace.library_restore.is_none());
    assert!(!fixture.library.join(".cuenote-restore.json").exists());
}

#[test]
fn ordinary_write_racing_with_restore_is_rejected_without_cancelling_restore() {
    let control = BackupControl::default();
    let job = control.begin_restore_job(None).unwrap();
    let error = control
        .begin_mutation()
        .err()
        .expect("ordinary write must be rejected");
    assert_eq!(error.kind, "libraryRestorePending");
    job.checkpoint().unwrap();
    control.cancel();
    assert_eq!(job.checkpoint().unwrap_err().kind, "backupCancelled");
    drop(job);
    control.begin_mutation().unwrap();
}

#[test]
fn file_notification_still_cancels_an_ordinary_backup() {
    let fixture = interrupted_fixture();
    let context = fixture.workspace.backup_context().unwrap();
    let control = fixture.control.clone();
    let job = fixture
        .control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == BackupPhase::Copying {
                control.cancel_for_file_change();
            }
        })))
        .unwrap();
    let error = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap_err();
    assert_eq!(error.kind, "backupCancelled");
    assert_eq!(native_files(&fixture.library), fixture.before_facts);
}

#[test]
fn queued_mutation_prevents_a_restore_job_from_starting() {
    let control = BackupControl::default();
    let mutation = control.begin_mutation().unwrap();
    let error = control
        .begin_restore_job(None)
        .err()
        .expect("queued write must block restore");
    assert_eq!(error.kind, "backupBusy");
    drop(mutation);
    control
        .begin_restore_job(None)
        .unwrap()
        .checkpoint()
        .unwrap();
}

#[test]
fn missing_or_corrupt_source_stage_fails_closed_without_replaying_facts() {
    for corrupt in [false, true] {
        let mut fixture = interrupted_fixture();
        interrupt_after_first_restored_file(&mut fixture);
        let interrupted_facts = interrupted_native_facts(&fixture);
        let task_root = restore_task_root(&fixture);
        let staged_fact = fixture.source_facts.keys().next().unwrap();
        let staged_path = task_root.join("source").join(staged_fact);
        if corrupt {
            fs::write(&staged_path, b"not a native package").unwrap();
        } else {
            fs::remove_file(&staged_path).unwrap();
        }

        let view = fixture.workspace.snapshot().unwrap();
        let status = view.library_restore.unwrap();
        assert_eq!(status.phase, "blocked");
        assert!(!status.can_continue);
        assert!(!status.can_roll_back);
        assert_eq!(interrupted_native_facts(&fixture), interrupted_facts);
        assert_eq!(
            fixture.workspace.new_note().unwrap_err().kind,
            "libraryRestorePending"
        );
    }
}

#[test]
fn mismatched_marker_identity_fails_closed_without_replaying_facts() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let interrupted_facts = interrupted_native_facts(&fixture);
    let marker_path = fixture.library.join(".cuenote-restore.json");
    let mut marker: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    marker["restoreId"] = serde_json::Value::String(uuid::Uuid::new_v4().to_string());
    fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();

    let view = fixture.workspace.snapshot().unwrap();
    let status = view.library_restore.unwrap();
    assert_eq!(status.phase, "blocked");
    assert!(!status.can_continue);
    assert!(!status.can_roll_back);
    assert_eq!(interrupted_native_facts(&fixture), interrupted_facts);
    assert_eq!(
        fixture
            .workspace
            .create_notebook("Must stay blocked")
            .unwrap_err()
            .kind,
        "libraryRestorePending"
    );
}

#[test]
fn invalid_plan_native_path_fails_closed_without_following_it() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let interrupted_facts = interrupted_native_facts(&fixture);
    let task_root = restore_task_root(&fixture);
    let plan_path = task_root.join("plan.json");
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    let escaped = fixture._temp.path().join("must-not-be-created.cuenote");
    plan["after"][0]["path"] = serde_json::Value::String("../must-not-be-created.cuenote".into());
    fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();

    let view = fixture.workspace.snapshot().unwrap();
    let status = view.library_restore.unwrap();
    assert_eq!(status.phase, "blocked");
    assert!(!status.can_continue);
    assert!(!status.can_roll_back);
    assert!(!escaped.exists());
    assert_eq!(interrupted_native_facts(&fixture), interrupted_facts);
    assert_eq!(
        fixture.workspace.new_note().unwrap_err().kind,
        "libraryRestorePending"
    );
}

#[test]
fn external_change_after_terminal_marker_removal_is_never_replayed_as_rollback() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let (task_root, relative, external_version) = finish_durable_source_state(&fixture, true);
    let staged_fact = fixture.source_facts.keys().next().unwrap();
    fs::remove_file(task_root.join("source").join(staged_fact)).unwrap();

    let view = fixture.workspace.snapshot().unwrap();
    let status = view.library_restore.unwrap();
    assert_eq!(status.phase, "paused");
    assert!(!status.can_roll_back);

    let job = fixture.control.begin_job(None).unwrap();
    fixture
        .workspace
        .resume_library_restore(cuenote_lib::workspace::LibraryRestoreAction::RollBack, &job)
        .unwrap();
    assert_eq!(
        fs::read(fixture.library.join(relative.as_str())).unwrap(),
        external_version
    );
    assert!(fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .is_none());
}

#[test]
fn terminal_marker_requires_both_verified_stages_before_closing() {
    for stage_name in ["source", "before"] {
        let mut fixture = interrupted_fixture();
        interrupt_after_first_restored_file(&mut fixture);
        let (task_root, relative, _) = finish_durable_source_state(&fixture, false);
        fs::write(
            fixture.library.join(&relative),
            &fixture.source_facts[&relative],
        )
        .unwrap();
        let staged_fact = fixture
            .source_facts
            .keys()
            .find(|path| fixture.before_facts.contains_key(*path))
            .unwrap();
        let damaged_stage_file = task_root.join(stage_name).join(staged_fact);
        fs::remove_file(&damaged_stage_file).unwrap();
        let facts_before_resume = native_files(&fixture.library);
        let marker = fixture.library.join(".cuenote-restore.json");
        assert!(marker.exists());

        let job = fixture.control.begin_job(None).unwrap();
        let error = fixture
            .workspace
            .resume_library_restore(cuenote_lib::workspace::LibraryRestoreAction::Continue, &job)
            .unwrap_err();
        assert_eq!(error.kind, "restoreStageChanged");
        assert!(marker.exists());
        assert!(damaged_stage_file.parent().unwrap().exists());
        assert!(task_root.join("plan.json").exists());
        assert!(task_root.join("events/terminal.json").exists());
        assert_eq!(native_files(&fixture.library), facts_before_resume);
    }
}

#[test]
fn closed_journal_direction_must_match_terminal_and_keeps_restore_evidence() {
    for remove_marker in [false, true] {
        let mut fixture = interrupted_fixture();
        interrupt_after_first_restored_file(&mut fixture);
        let (task_root, _, _) = finish_durable_source_state(&fixture, remove_marker);
        let facts_before = native_files(&fixture.library);
        let restore_id = restore_id(&task_root);
        write_restore_event(
            &task_root,
            "closed.json",
            &restore_id,
            None,
            Some("rolledBack"),
        );

        let status = fixture
            .workspace
            .pending_library_restore(&fixture.library)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, "blocked");
        assert_eq!(
            fixture.library.join(".cuenote-restore.json").exists(),
            !remove_marker
        );
        assert!(task_root.join("plan.json").exists());
        assert!(task_root.join("source").exists());
        assert!(task_root.join("before").exists());
        assert!(task_root.join("events/terminal.json").exists());
        assert!(task_root.join("events/closed.json").exists());
        assert_eq!(native_files(&fixture.library), facts_before);
    }
}

#[test]
fn journal_terminal_rolled_back_requires_rollback_direction() {
    let mut fixture = interrupted_fixture();
    let task_root = cancel_restore_at_restoring_zero(&mut fixture);
    let restore_id = restore_id(&task_root);
    write_restore_event(
        &task_root,
        "terminal.json",
        &restore_id,
        None,
        Some("rolledBack"),
    );
    let facts_before = native_files(&fixture.library);

    let status = fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, "blocked");
    assert!(fixture.library.join(".cuenote-restore.json").exists());
    assert!(task_root.join("plan.json").exists());
    assert!(task_root.join("events/terminal.json").exists());
    assert_eq!(native_files(&fixture.library), facts_before);
}

#[test]
fn journal_terminal_completed_cannot_include_rollback_direction() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let (task_root, _, _) = finish_durable_source_state(&fixture, false);
    let restore_id = restore_id(&task_root);
    write_restore_event(
        &task_root,
        "rollback-started.json",
        &restore_id,
        None,
        Some("rollingBack"),
    );
    let facts_before = native_files(&fixture.library);

    let status = fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, "blocked");
    assert!(fixture.library.join(".cuenote-restore.json").exists());
    assert!(task_root.join("plan.json").exists());
    assert!(task_root.join("events/terminal.json").exists());
    assert!(task_root.join("events/rollback-started.json").exists());
    assert_eq!(native_files(&fixture.library), facts_before);
}

#[test]
fn journal_terminal_rolled_back_requires_all_started_facts_rolled_back() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let task_root = restore_task_root(&fixture);
    let restore_id = restore_id(&task_root);
    write_restore_event(
        &task_root,
        "rollback-started.json",
        &restore_id,
        None,
        Some("rollingBack"),
    );
    write_restore_event(
        &task_root,
        "terminal.json",
        &restore_id,
        None,
        Some("rolledBack"),
    );
    let facts_before = native_files(&fixture.library);

    let status = fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, "blocked");
    assert!(fixture.library.join(".cuenote-restore.json").exists());
    assert!(task_root.join("plan.json").exists());
    assert!(task_root.join("events/terminal.json").exists());
    assert_eq!(native_files(&fixture.library), facts_before);
}

#[test]
fn journal_rollback_intent_requires_a_forward_intent_for_the_same_fact() {
    let mut fixture = interrupted_fixture();
    let task_root = cancel_restore_at_restoring_zero(&mut fixture);
    let restore_id = restore_id(&task_root);
    write_restore_event(
        &task_root,
        "rollback-started.json",
        &restore_id,
        None,
        Some("rollingBack"),
    );
    write_restore_event(
        &task_root,
        "000000-rollback-intent.json",
        &restore_id,
        Some(0),
        None,
    );
    let facts_before = native_files(&fixture.library);

    let status = fixture
        .workspace
        .pending_library_restore(&fixture.library)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, "blocked");
    assert!(fixture.library.join(".cuenote-restore.json").exists());
    assert!(task_root.join("events/rollback-started.json").exists());
    assert!(task_root
        .join("events/000000-rollback-intent.json")
        .exists());
    assert_eq!(native_files(&fixture.library), facts_before);
}

#[test]
fn terminal_state_with_live_marker_refuses_external_fact_changes() {
    for action in [
        cuenote_lib::workspace::LibraryRestoreAction::Continue,
        cuenote_lib::workspace::LibraryRestoreAction::RollBack,
    ] {
        let mut fixture = interrupted_fixture();
        interrupt_after_first_restored_file(&mut fixture);
        let (_, relative, external_version) = finish_durable_source_state(&fixture, false);
        let marker = fixture.library.join(".cuenote-restore.json");
        assert!(marker.exists());
        let changed_facts = native_files(&fixture.library);
        assert_eq!(
            fs::read(fixture.library.join(&relative)).unwrap(),
            external_version
        );

        let job = fixture.control.begin_job(None).unwrap();
        let error = fixture
            .workspace
            .resume_library_restore(action, &job)
            .unwrap_err();
        assert_eq!(error.kind, "conflict");
        assert!(marker.exists());
        assert_eq!(native_files(&fixture.library), changed_facts);
    }
}

#[test]
fn detached_backup_context_cannot_snapshot_a_pending_restore() {
    let mut fixture = interrupted_fixture();
    let stale_context = fixture.workspace.backup_context().unwrap();
    cancel_restore_at_restoring_zero(&mut fixture);
    let facts_before = native_files(&fixture.library);

    let job = fixture.control.begin_job(None).unwrap();
    let error = stale_context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap_err();
    assert_eq!(error.kind, "libraryRestorePending");
    assert_eq!(native_files(&fixture.library), facts_before);
}

#[test]
fn detached_backup_context_rechecks_restore_marker_before_publication() {
    let fixture = interrupted_fixture();
    let context = fixture.workspace.backup_context().unwrap();
    let before = native_files(&fixture.library);
    let list_job = fixture.control.begin_job(None).unwrap();
    let before_catalog = context.catalog(&list_job).unwrap();
    drop(list_job);

    let marker = fixture.library.join(".cuenote-restore.json");
    let marker_written = Arc::new(AtomicBool::new(false));
    let marker_written_callback = Arc::clone(&marker_written);
    let marker_callback = marker.clone();
    let job = fixture
        .control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == BackupPhase::Publishing && progress.checked == 0 {
                fs::write(&marker_callback, b"pending").unwrap();
                marker_written_callback.store(true, Ordering::SeqCst);
            }
        })))
        .unwrap();
    let error = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap_err();
    assert!(marker_written.load(Ordering::SeqCst));
    assert_eq!(error.kind, "libraryRestorePending");
    assert_eq!(native_files(&fixture.library), before);
    drop(job);
    fs::remove_file(&marker).unwrap();

    let list_job = fixture.control.begin_job(None).unwrap();
    let after_catalog = context.catalog(&list_job).unwrap();
    assert_eq!(
        before_catalog
            .backups
            .iter()
            .map(|backup| &backup.backup_id)
            .collect::<BTreeSet<_>>(),
        after_catalog
            .backups
            .iter()
            .map(|backup| &backup.backup_id)
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn continue_refuses_unclaimed_exact_source_bytes_at_a_source_only_path() {
    let mut fixture = interrupted_fixture();
    let (source_only_path, source_bytes) = create_unclaimed_source_only_target(&mut fixture);
    let externally_changed_facts = native_files(&fixture.library);
    assert_eq!(
        fs::read(fixture.library.join(&source_only_path)).unwrap(),
        source_bytes
    );

    let job = fixture.control.begin_job(None).unwrap();
    let error = fixture
        .workspace
        .resume_library_restore(cuenote_lib::workspace::LibraryRestoreAction::Continue, &job)
        .unwrap_err();
    assert_eq!(error.kind, "conflict");
    assert_eq!(native_files(&fixture.library), externally_changed_facts);
}

#[test]
fn rollback_refuses_to_delete_unclaimed_exact_source_bytes_at_a_source_only_path() {
    let mut fixture = interrupted_fixture();
    let (source_only_path, source_bytes) = create_unclaimed_source_only_target(&mut fixture);
    let externally_changed_facts = native_files(&fixture.library);
    assert_eq!(
        fs::read(fixture.library.join(&source_only_path)).unwrap(),
        source_bytes
    );

    let job = fixture.control.begin_job(None).unwrap();
    let error = fixture
        .workspace
        .resume_library_restore(cuenote_lib::workspace::LibraryRestoreAction::RollBack, &job)
        .unwrap_err();
    assert_eq!(error.kind, "conflict");
    assert_eq!(native_files(&fixture.library), externally_changed_facts);
}

#[test]
fn deleting_a_trashed_note_during_pending_library_restore_is_rejected() {
    let mut fixture = interrupted_fixture();
    let active = fixture.workspace.snapshot().unwrap().session.unwrap();
    let trashed = fixture.workspace.trash_note(&active.session_id).unwrap();
    let trash_session = fixture
        .workspace
        .open_note(&trashed.session.note_id)
        .unwrap();
    let note_path = fixture
        .library
        .join("notes")
        .join(format!("{}.cuenote", trash_session.note_id));
    let before = fs::read(&note_path).unwrap();
    cancel_restore_at_restoring_zero(&mut fixture);

    let error = fixture
        .workspace
        .delete_trashed_note(&trash_session.session_id, true)
        .unwrap_err();
    assert_eq!(error.kind, "libraryRestorePending");
    assert_eq!(fs::read(&note_path).unwrap(), before);
}

#[test]
fn pending_snapshot_after_library_is_renamed_stays_blocked_without_replaying_facts() {
    let mut fixture = interrupted_fixture();
    interrupt_after_first_restored_file(&mut fixture);
    let facts_before_rename = native_files(&fixture.library);
    let moved_library = fixture._temp.path().join("library-unavailable");
    fs::rename(&fixture.library, &moved_library).unwrap();

    let mut reopened = Workspace::new(
        fixture._temp.path().join("settings"),
        fixture._temp.path().join("recovery"),
        fixture._temp.path().join("default"),
    );
    let view = reopened.snapshot().unwrap();

    assert!(view.library_restore.is_some(), "{view:?}");
    assert!(view.session.is_none());
    assert!(view.library.unwrap().notes.is_empty());
    assert!(!fixture.library.exists());
    assert_eq!(native_files(&moved_library), facts_before_rename);
}
