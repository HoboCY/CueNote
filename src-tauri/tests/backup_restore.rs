use cuenote_lib::note_format::PlainText;
use cuenote_lib::storage;
use cuenote_lib::workspace::{
    BackupContext, BackupControl, BackupJob, BackupKind, VerifiedBackupNote, Workspace,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("backup-restore-")
        .tempdir_in(base)
        .unwrap()
}

fn selected_workspace() -> (tempfile::TempDir, Workspace, PathBuf) {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    (temp, workspace, library)
}

fn backup_note_source(
    workspace: &Workspace,
    control: &BackupControl,
    note_id: &str,
) -> (BackupJob, BackupContext, String, VerifiedBackupNote) {
    let context = workspace.backup_context().unwrap();
    let job = control.begin_job(None).unwrap();
    let backup = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let entry = context
        .list_notes(&backup.backup_id, &job)
        .unwrap()
        .notes
        .into_iter()
        .find(|note| note.note_id == note_id)
        .unwrap();
    let source = context
        .load_note_entry(&backup.backup_id, &entry.entry_id, &entry.revision, &job)
        .unwrap();
    (job, context, backup.backup_id, source)
}

#[test]
fn lists_each_note_from_a_verified_backup_with_stable_entry_id_and_package_revision() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "A backed-up note".into(),
                cue: "Remember the seam".into(),
                content: "Known body text".into(),
                summary: "Known backup excerpt".into(),
            },
        )
        .unwrap();
    let source_note = storage::open_note(
        &library_path
            .join("notes")
            .join(format!("{}.cuenote", saved.session.note_id)),
    )
    .unwrap();
    let expected_modified_at = source_note
        .package
        .document
        .timestamps
        .content_modified_at
        .clone();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let backup_job = control.begin_job(None).unwrap();
    let backup = context
        .run(BackupKind::Manual, "2026-10-15", &backup_job)
        .unwrap()
        .created
        .unwrap();
    drop(backup_job);

    let backed_up_note = PathBuf::from(&backup.path)
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let expected_revision = format!("{:x}", Sha256::digest(fs::read(&backed_up_note).unwrap()));
    let list_job = control.begin_job(None).unwrap();
    let listed = context.list_notes(&backup.backup_id, &list_job).unwrap();
    assert_eq!(listed.backup_id, backup.backup_id);
    assert_eq!(listed.notes.len(), 1);
    let listed_note = &listed.notes[0];
    assert_eq!(listed_note.note_id, saved.session.note_id);
    assert_eq!(listed_note.title, "A backed-up note");
    assert_eq!(listed_note.revision, expected_revision);
    assert_eq!(listed_note.modified_at, expected_modified_at);
    assert_eq!(listed_note.notebook_id, saved.session.notebook_id);
    assert_eq!(listed_note.notebook_name.as_deref(), Some("默认笔记本"));
    assert!(!listed_note.pinned);
    assert_eq!(listed_note.trashed_at, None);
    assert_eq!(listed_note.excerpt, "Known backup excerpt");
    let first_entry_id = listed_note.entry_id.clone();
    drop(list_job);

    let repeat_job = control.begin_job(None).unwrap();
    let repeated = context.list_notes(&backup.backup_id, &repeat_job).unwrap();
    assert_eq!(repeated.notes[0].entry_id, first_entry_id);
}

#[test]
fn loads_only_the_selected_note_entry_and_rejects_a_stale_revision() {
    let (_temp, mut workspace, _library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Entry-bound source".into(),
                cue: String::new(),
                content: "Selected package".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let backup_job = control.begin_job(None).unwrap();
    let backup = context
        .run(BackupKind::Manual, "2026-10-15", &backup_job)
        .unwrap()
        .created
        .unwrap();
    drop(backup_job);

    let job = control.begin_job(None).unwrap();
    let list = context.list_notes(&backup.backup_id, &job).unwrap();
    let entry = &list.notes[0];
    assert!(context
        .load_note_entry(&backup.backup_id, &entry.entry_id, &entry.revision, &job,)
        .is_ok());
    assert_eq!(
        context
            .load_note_entry(&backup.backup_id, &entry.entry_id, &"0".repeat(64), &job,)
            .unwrap_err()
            .kind,
        "backupChanged"
    );
}

#[test]
fn verified_backup_job_transfers_exclusivity_to_a_mutation_guard() {
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();

    let mutation = job.into_mutation().unwrap();
    assert_eq!(control.begin_job(None).err().unwrap().kind, "backupBusy");
    drop(mutation);

    let next_job = control.begin_job(None).unwrap();
    assert!(next_job.checkpoint().is_ok());
}

#[test]
fn restore_protects_current_library_then_reopens_the_complete_selected_package() {
    let (temp, mut workspace, library_path) = selected_workspace();
    let default_notebook_id = workspace
        .snapshot()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .unwrap();
    let mut original = storage::open_note(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid-rich-v1.cuenote"),
    )
    .unwrap()
    .package;
    original.document.note_id = uuid::Uuid::new_v4().to_string();
    original.document.organization.notebook_id = Some(default_notebook_id);
    original.document.organization.pinned = true;
    original.document.organization.trashed_at = None;
    let note_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", original.document.note_id));
    storage::save_note(&note_path, &temp.path().join("recovery"), &original, None).unwrap();
    let opened = workspace.open_note(&original.document.note_id).unwrap();
    let source_trashed = workspace.trash_note(&opened.session_id).unwrap();
    let backed_up_package = storage::open_note(&note_path).unwrap().package;
    assert!(backed_up_package.document.organization.pinned);
    assert!(backed_up_package.document.organization.trashed_at.is_some());

    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let source_backup = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let listed = context.list_notes(&source_backup.backup_id, &job).unwrap();
    let source_entry = listed
        .notes
        .iter()
        .find(|note| note.note_id == original.document.note_id)
        .unwrap();
    let source = context
        .load_note_entry(
            &source_backup.backup_id,
            &source_entry.entry_id,
            &source_entry.revision,
            &job,
        )
        .unwrap();

    let untrashed = workspace
        .restore_trashed_note(&source_trashed.session.session_id, None)
        .unwrap();
    let mut edited = untrashed.session.content;
    edited.title = "Current saved version".into();
    workspace
        .save_note_content(&untrashed.session.session_id, edited)
        .unwrap();
    let current_package = storage::open_note(&note_path).unwrap().package;
    assert_ne!(current_package, backed_up_package);

    let prepared = workspace.prepare_backup_note_restore(source, None).unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();
    let mutation = job.into_mutation().unwrap();
    let receipt = workspace.restore_backup_note(protected).unwrap();
    drop(mutation);
    let pre_restore_backup = receipt.pre_restore_backup.path.clone();

    assert_eq!(receipt.pre_restore_backup.kind, BackupKind::Manual);
    assert_eq!(receipt.session.note_id, original.document.note_id);
    assert!(receipt.session.pinned);
    assert!(receipt.session.trashed_at.is_some());
    assert!(!receipt.reassigned_notebook);
    let restored = storage::open_note(&note_path).unwrap().package;
    assert_eq!(restored, backed_up_package);
    let protected_current = storage::open_note(
        &PathBuf::from(pre_restore_backup)
            .join("notes")
            .join(format!("{}.cuenote", original.document.note_id)),
    )
    .unwrap()
    .package;
    assert_eq!(protected_current, current_package);
}

#[test]
fn prepare_rejects_a_duplicate_same_identity_file_before_any_restore_plan_exists() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Unique target".into(),
                cue: String::new(),
                content: "Body".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let backup = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let entry = context
        .list_notes(&backup.backup_id, &job)
        .unwrap()
        .notes
        .remove(0);
    let source = context
        .load_note_entry(&backup.backup_id, &entry.entry_id, &entry.revision, &job)
        .unwrap();

    let canonical_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let conflict_path = library_path
        .join("notes")
        .join(format!("{}-sync-conflict.cuenote", saved.session.note_id));
    fs::copy(canonical_path, conflict_path).unwrap();

    assert_eq!(
        workspace
            .prepare_backup_note_restore(source, None)
            .err()
            .unwrap()
            .kind,
        "restoreAmbiguous"
    );
}

#[test]
fn conflicted_source_notebook_can_be_explicitly_reassigned_to_another_current_notebook() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Notebook conflict target".into(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();
    let alternate_library = workspace.create_notebook("恢复目标笔记本").unwrap();
    let target_notebook_id = alternate_library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "恢复目标笔记本")
        .unwrap()
        .notebook_id
        .clone();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let backup = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let entry = context
        .list_notes(&backup.backup_id, &job)
        .unwrap()
        .notes
        .into_iter()
        .find(|note| note.note_id == saved.session.note_id)
        .unwrap();
    let source_path = PathBuf::from(&backup.path)
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let source_package = storage::open_note(&source_path).unwrap().package;
    let notebook_id = saved.session.notebook_id.unwrap();
    let notebook_path = library_path
        .join("notebooks")
        .join(format!("{notebook_id}.json"));
    let conflict_path = library_path
        .join("notebooks")
        .join(".cuenote-notebook-conflict-late.json");
    fs::copy(notebook_path, conflict_path).unwrap();
    let available = workspace.backup_restore_notebook_ids().unwrap();
    assert!(!available.contains(&notebook_id));
    assert!(available.contains(&target_notebook_id));

    let target_path = library_path
        .join("notebooks")
        .join(format!("{target_notebook_id}.json"));
    let late_conflict_path = library_path
        .join("notebooks")
        .join(".cuenote-notebook-conflict-target-late.json");
    fs::copy(&target_path, &late_conflict_path).unwrap();
    let stale_source = context
        .load_note_entry(&backup.backup_id, &entry.entry_id, &entry.revision, &job)
        .unwrap();
    let stale_error = workspace
        .prepare_backup_note_restore(stale_source, Some(&target_notebook_id))
        .err()
        .unwrap();
    assert_eq!(stale_error.kind, "restoreNotebookConflict");
    fs::remove_file(late_conflict_path).unwrap();

    let source = context
        .load_note_entry(&backup.backup_id, &entry.entry_id, &entry.revision, &job)
        .unwrap();
    let prepared = workspace
        .prepare_backup_note_restore(source, Some(&target_notebook_id))
        .unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();
    let mutation = job.into_mutation().unwrap();
    let receipt = workspace.restore_backup_note(protected).unwrap();
    drop(mutation);

    assert!(receipt.reassigned_notebook);
    assert_eq!(
        receipt.session.notebook_id.as_deref(),
        Some(target_notebook_id.as_str())
    );
    let restored = storage::open_note(
        &library_path
            .join("notes")
            .join(format!("{}.cuenote", saved.session.note_id)),
    )
    .unwrap()
    .package;
    assert_eq!(
        restored.document.organization.notebook_id.as_deref(),
        Some(target_notebook_id.as_str())
    );
    let mut expected = source_package;
    expected.document.organization.notebook_id = Some(target_notebook_id);
    expected.document.timestamps.organization_modified_at = restored
        .document
        .timestamps
        .organization_modified_at
        .clone();
    assert_eq!(restored, expected);
}

#[test]
fn restore_recreates_a_missing_canonical_target_from_the_prepared_backup() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Recover missing file".into(),
                cue: "Cue".into(),
                content: "Saved before deletion".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let control = BackupControl::default();
    let (job, _, _, source) = backup_note_source(&workspace, &control, &saved.session.note_id);
    let note_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    fs::remove_file(&note_path).unwrap();

    let prepared = workspace.prepare_backup_note_restore(source, None).unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();
    let mutation = job.into_mutation().unwrap();
    let receipt = workspace.restore_backup_note(protected).unwrap();
    drop(mutation);

    assert!(!receipt.reassigned_notebook);
    let restored = storage::open_note(&note_path).unwrap();
    assert_eq!(
        restored.package.plain_text().unwrap().title,
        "Recover missing file"
    );
    assert_eq!(
        restored.package.plain_text().unwrap().content,
        "Saved before deletion"
    );
}

#[test]
fn missing_source_notebook_requires_an_explicit_valid_reassignment() {
    let (temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Missing notebook".into(),
                cue: String::new(),
                content: "Keep this package".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let default_notebook_id = workspace
        .snapshot()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .unwrap();
    let note_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let mut original = storage::open_note(&note_path).unwrap();
    let original_modified_at = original
        .package
        .document
        .timestamps
        .organization_modified_at
        .clone();
    original.package.document.organization.notebook_id = Some(uuid::Uuid::new_v4().to_string());
    storage::save_note(
        &note_path,
        &temp.path().join("recovery"),
        &original.package,
        Some(&original.revision),
    )
    .unwrap();
    workspace.open_note(&saved.session.note_id).unwrap();

    let control = BackupControl::default();
    let (job, context, backup_id, source) =
        backup_note_source(&workspace, &control, &saved.session.note_id);
    assert_eq!(
        workspace
            .prepare_backup_note_restore(source, None)
            .err()
            .unwrap()
            .kind,
        "restoreTargetRequired"
    );

    let entry = context
        .list_notes(&backup_id, &job)
        .unwrap()
        .notes
        .into_iter()
        .find(|entry| entry.note_id == saved.session.note_id)
        .unwrap();
    let source = context
        .load_note_entry(&backup_id, &entry.entry_id, &entry.revision, &job)
        .unwrap();
    let prepared = workspace
        .prepare_backup_note_restore(source, Some(&default_notebook_id))
        .unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();
    let mutation = job.into_mutation().unwrap();
    let receipt = workspace.restore_backup_note(protected).unwrap();
    drop(mutation);

    assert!(receipt.reassigned_notebook);
    assert_eq!(
        receipt.session.notebook_id.as_deref(),
        Some(default_notebook_id.as_str())
    );
    let restored = storage::open_note(&note_path).unwrap().package;
    assert_eq!(
        restored.document.organization.notebook_id.as_deref(),
        Some(default_notebook_id.as_str())
    );
    assert_ne!(
        restored.document.timestamps.organization_modified_at,
        original_modified_at
    );
}

#[test]
fn restore_rejects_same_identity_file_changes_after_the_pre_restore_backup() {
    let (temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "CAS target".into(),
                cue: String::new(),
                content: "Original body".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let control = BackupControl::default();
    let (job, _, _, source) = backup_note_source(&workspace, &control, &saved.session.note_id);
    let prepared = workspace.prepare_backup_note_restore(source, None).unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();

    let note_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let mut external = storage::open_note(&note_path).unwrap();
    let mut text = external.package.plain_text().unwrap();
    text.title = "External same-identity update".into();
    external.package.replace_plain_text(text).unwrap();
    storage::save_note(
        &note_path,
        &temp.path().join("recovery"),
        &external.package,
        Some(&external.revision),
    )
    .unwrap();

    let mutation = job.into_mutation().unwrap();
    let error = workspace.restore_backup_note(protected).err().unwrap();
    drop(mutation);
    assert_eq!(error.kind, "restoreConflict");
    assert!(error.recovery_path.is_some());
    assert_eq!(
        storage::open_note(&note_path)
            .unwrap()
            .package
            .plain_text()
            .unwrap()
            .title,
        "External same-identity update"
    );
}

#[test]
fn restore_rechecks_notebook_metadata_after_the_pre_restore_backup() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Late metadata conflict".into(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();
    let control = BackupControl::default();
    let (job, _, _, source) = backup_note_source(&workspace, &control, &saved.session.note_id);
    let prepared = workspace.prepare_backup_note_restore(source, None).unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();

    let notebook_id = saved.session.notebook_id.unwrap();
    let notebook_path = library_path
        .join("notebooks")
        .join(format!("{notebook_id}.json"));
    let conflict_path = library_path
        .join("notebooks")
        .join(format!(".cuenote-notebook-conflict-{notebook_id}.json"));
    fs::copy(notebook_path, conflict_path).unwrap();

    let mutation = job.into_mutation().unwrap();
    let error = workspace.restore_backup_note(protected).err().unwrap();
    drop(mutation);
    assert_eq!(error.kind, "restoreNotebookConflict");
    assert!(error.recovery_path.is_some());
}

#[test]
fn restore_rejects_an_editor_session_change_after_the_pre_restore_backup() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let first = workspace.new_note().unwrap();
    let first = workspace
        .save_note_text(
            &first.session_id,
            PlainText {
                title: "First note".into(),
                cue: String::new(),
                content: "Do not replace after session switch".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let second = workspace.new_note().unwrap();
    let second = workspace
        .save_note_text(
            &second.session_id,
            PlainText {
                title: "Second note".into(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();
    workspace.open_note(&first.session.note_id).unwrap();

    let control = BackupControl::default();
    let (job, _, _, source) = backup_note_source(&workspace, &control, &first.session.note_id);
    let prepared = workspace.prepare_backup_note_restore(source, None).unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();
    workspace.open_note(&second.session.note_id).unwrap();

    let mutation = job.into_mutation().unwrap();
    let error = workspace.restore_backup_note(protected).err().unwrap();
    drop(mutation);
    assert_eq!(error.kind, "restoreConflict");
    assert!(error.recovery_path.is_some());
    let first_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", first.session.note_id));
    assert_eq!(
        storage::open_note(&first_path)
            .unwrap()
            .package
            .plain_text()
            .unwrap()
            .title,
        "First note"
    );
}

#[test]
fn backup_entries_distinguish_same_identity_files_and_restore_the_selected_entry() {
    let (temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Canonical copy".into(),
                cue: String::new(),
                content: "Canonical content".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let canonical_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let mut duplicate = storage::open_note(&canonical_path).unwrap();
    let mut duplicate_text = duplicate.package.plain_text().unwrap();
    duplicate_text.title = "Selected conflict copy".into();
    duplicate_text.content = "Selected conflict content".into();
    duplicate
        .package
        .replace_plain_text(duplicate_text)
        .unwrap();
    let conflict_path = library_path
        .join("notes")
        .join(format!("{}-sync-conflict.cuenote", saved.session.note_id));
    storage::save_note(
        &conflict_path,
        &temp.path().join("recovery"),
        &duplicate.package,
        None,
    )
    .unwrap();

    let control = BackupControl::default();
    let (job, context, backup_id, _source) =
        backup_note_source(&workspace, &control, &saved.session.note_id);
    let listed = context.list_notes(&backup_id, &job).unwrap();
    let same_identity = listed
        .notes
        .iter()
        .filter(|entry| entry.note_id == saved.session.note_id)
        .collect::<Vec<_>>();
    assert_eq!(same_identity.len(), 2);
    assert_ne!(same_identity[0].entry_id, same_identity[1].entry_id);
    assert_ne!(same_identity[0].revision, same_identity[1].revision);
    let selected = same_identity
        .iter()
        .find(|entry| entry.title == "Selected conflict copy")
        .unwrap();
    let source = context
        .load_note_entry(&backup_id, &selected.entry_id, &selected.revision, &job)
        .unwrap();

    let retained_conflict = temp.path().join("retained-conflict.cuenote");
    fs::rename(&conflict_path, retained_conflict).unwrap();
    let prepared = workspace.prepare_backup_note_restore(source, None).unwrap();
    let protected = prepared.protect_current("2026-10-15", &job).unwrap();
    let mutation = job.into_mutation().unwrap();
    let receipt = workspace.restore_backup_note(protected).unwrap();
    drop(mutation);

    assert_eq!(receipt.session.note_id, saved.session.note_id);
    let restored = storage::open_note(&canonical_path).unwrap().package;
    assert_eq!(
        restored.plain_text().unwrap().title,
        "Selected conflict copy"
    );
    assert_eq!(
        restored.plain_text().unwrap().content,
        "Selected conflict content"
    );
}

#[test]
fn load_rejects_a_backup_with_a_modified_manifest_without_writing_the_target() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Stay in current file".into(),
                cue: String::new(),
                content: "Current value".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let target_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    let before = storage::open_note(&target_path).unwrap().package;
    let control = BackupControl::default();
    let (job, context, backup_id, source) =
        backup_note_source(&workspace, &control, &saved.session.note_id);
    drop(source);
    let entry = context
        .list_notes(&backup_id, &job)
        .unwrap()
        .notes
        .into_iter()
        .find(|entry| entry.note_id == saved.session.note_id)
        .unwrap();
    let catalog = context.catalog(&job).unwrap();
    let backup = catalog
        .backups
        .iter()
        .find(|backup| backup.backup_id == backup_id)
        .unwrap();
    let manifest_path = PathBuf::from(&backup.path).join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let files = manifest["files"].as_array_mut().unwrap();
    let note_file = files
        .iter_mut()
        .find(|file| {
            file["path"]
                .as_str()
                .is_some_and(|path| path.starts_with("notes/"))
        })
        .unwrap();
    note_file["sha256"] = serde_json::Value::String("0".repeat(64));
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    assert_eq!(
        context
            .load_note_entry(&backup.backup_id, &entry.entry_id, &entry.revision, &job,)
            .err()
            .unwrap()
            .kind,
        "backupIncomplete"
    );
    assert_eq!(storage::open_note(&target_path).unwrap().package, before);
}
