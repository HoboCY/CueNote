use chrono::{Duration, NaiveDate};
use cuenote_lib::note_format::PlainText;
use cuenote_lib::storage;
use cuenote_lib::workspace::{BackupControl, BackupKind, Workspace};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod support;

fn workspace_temp() -> tempfile::TempDir {
    support::tempdir("backups-")
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

#[test]
fn manual_backup_is_a_native_readable_snapshot_with_empty_notebooks() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let library = workspace.create_notebook("Empty notebook").unwrap();
    let empty_notebook = library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Empty notebook")
        .unwrap()
        .notebook_id
        .clone();
    let default_notebook = library
        .notebooks
        .iter()
        .find(|notebook| notebook.is_default)
        .unwrap()
        .notebook_id
        .clone();
    let default_metadata = fs::read(
        library_path
            .join("notebooks")
            .join(format!("{default_notebook}.json")),
    )
    .unwrap();
    let conflict_file_name = format!(
        ".cuenote-notebook-conflict-{}.json",
        uuid::Uuid::new_v4().simple()
    );
    fs::write(
        library_path.join("notebooks").join(&conflict_file_name),
        default_metadata,
    )
    .unwrap();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Backup source".into(),
                cue: "A saved cue".into(),
                content: "Native note content".into(),
                summary: "A saved summary".into(),
            },
        )
        .unwrap();
    workspace.trash_note(&note.session_id).unwrap();

    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let outcome = context.run(BackupKind::Manual, "2026-10-15", &job).unwrap();

    let summary = outcome.created.unwrap();
    assert_eq!(summary.kind, BackupKind::Manual);
    assert_eq!(summary.note_count, 1);
    assert_eq!(summary.notebook_count, 3);
    let backup_path = PathBuf::from(&summary.path);
    assert!(backup_path
        .join("notebooks")
        .join(format!("{empty_notebook}.json"))
        .is_file());
    assert!(backup_path
        .join("notebooks")
        .join(conflict_file_name)
        .is_file());
    let opened = storage::open_note(
        &backup_path
            .join("notes")
            .join(format!("{}.cuenote", note.note_id)),
    )
    .unwrap();
    assert_eq!(
        opened.package.text_projection().content,
        "Native note content"
    );
    assert!(opened.package.document.organization.trashed_at.is_some());
    assert_eq!(outcome.catalog.backups.len(), 1);
}

#[test]
fn automatic_backups_use_observed_days_and_retain_only_the_latest_thirty() {
    let (_temp, mut workspace, _library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let session_id = note.session_id.clone();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let first_day = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

    for day in 0..31 {
        let local_date = first_day
            .checked_add_signed(Duration::days(day))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        workspace
            .save_note_text(
                &session_id,
                PlainText {
                    title: "Daily snapshot".into(),
                    cue: String::new(),
                    content: format!("saved version {day}"),
                    summary: String::new(),
                },
            )
            .unwrap();
        let job = control.begin_job(None).unwrap();
        let outcome = context
            .run(BackupKind::Automatic, &local_date, &job)
            .unwrap();
        assert!(
            outcome.created.is_some(),
            "missing snapshot for {local_date}"
        );
    }

    let last_day = first_day
        .checked_add_signed(Duration::days(30))
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    workspace
        .save_note_text(
            &session_id,
            PlainText {
                title: "Daily snapshot".into(),
                cue: String::new(),
                content: "another saved change on day 31".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let same_day_job = control.begin_job(None).unwrap();
    let same_day = context
        .run(BackupKind::Automatic, &last_day, &same_day_job)
        .unwrap();
    assert!(same_day.created.is_none());
    drop(same_day_job);

    let next_day_date = first_day
        .checked_add_signed(Duration::days(31))
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    let next_day_job = control.begin_job(None).unwrap();
    let next_day_outcome = context
        .run(BackupKind::Automatic, &next_day_date, &next_day_job)
        .unwrap();
    assert!(next_day_outcome.created.is_none());
    assert_eq!(next_day_outcome.catalog.backups.len(), 30);
    assert!(next_day_outcome
        .catalog
        .backups
        .iter()
        .all(|backup| backup.kind == BackupKind::Automatic));
}

#[test]
fn close_marker_is_a_local_preference_and_next_automatic_check_uses_the_actual_day() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Close marker".into(),
                cue: String::new(),
                content: "before close".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let first_job = control.begin_job(None).unwrap();
    let first = context
        .run(BackupKind::Automatic, "2026-04-01", &first_job)
        .unwrap();
    assert_eq!(first.created.unwrap().local_date, "2026-04-01");
    drop(first_job);

    // A corrupt source must not block the close marker: it only writes a small
    // local preference and never scans or hashes the library or backup root.
    let note_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let valid_note = fs::read(&note_path).unwrap();
    fs::write(&note_path, b"temporarily unavailable during close").unwrap();
    workspace.mark_backup_check_due("2026-04-01").unwrap();
    workspace.mark_backup_check_due("2026-04-02").unwrap();
    fs::write(&note_path, valid_note).unwrap();

    let same_day_job = control.begin_job(None).unwrap();
    let same_day = context
        .run(BackupKind::Automatic, "2026-04-02", &same_day_job)
        .unwrap();
    assert!(same_day.created.is_none());
    assert!(same_day.catalog.warnings.is_empty());
    drop(same_day_job);

    let unchanged_next_day = control.begin_job(None).unwrap();
    let next_day = context
        .run(BackupKind::Automatic, "2026-04-03", &unchanged_next_day)
        .unwrap();
    assert!(next_day.created.is_none());
    assert!(next_day.catalog.warnings.is_empty());
    assert_eq!(next_day.catalog.backups.len(), 1);
}

#[test]
fn close_marker_reports_missed_observation_and_backs_up_the_current_date() {
    let (_temp, mut workspace, _library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Close marker change".into(),
                cue: String::new(),
                content: "before close".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let first_job = control.begin_job(None).unwrap();
    context
        .run(BackupKind::Automatic, "2026-04-10", &first_job)
        .unwrap();
    drop(first_job);
    workspace.mark_backup_check_due("2026-04-10").unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Close marker change".into(),
                cue: String::new(),
                content: "changed while closed".into(),
                summary: String::new(),
            },
        )
        .unwrap();

    let job = control.begin_job(None).unwrap();
    let outcome = context
        .run(BackupKind::Automatic, "2026-04-12", &job)
        .unwrap();
    let created = outcome.created.unwrap();
    assert_eq!(created.local_date, "2026-04-12");
    assert!(outcome.catalog.warnings.iter().any(|warning| {
        warning.contains("上次离开笔记库时（2026-04-10）的自动备份检查未完成")
            && warning.contains("现按当前实际观察日备份")
    }));
}

#[test]
fn close_marker_is_a_noop_without_a_selected_library() {
    let temp = workspace_temp();
    let workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );

    workspace.mark_backup_check_due("not-a-date").unwrap();
    assert!(!temp.path().join("settings").exists());
}

#[test]
fn source_mutation_during_copy_cancels_without_publishing_a_mixed_snapshot() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Before".into(),
                cue: String::new(),
                content: "Before copy".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let changed = Arc::new(AtomicBool::new(false));
    let callback_changed = Arc::clone(&changed);
    let callback_control = control.clone();
    let callback_library = library_path.clone();
    let callback_note_id = note.note_id.clone();
    let job = control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == cuenote_lib::workspace::BackupPhase::Copying
                && progress.checked == 0
                && !callback_changed.swap(true, Ordering::AcqRel)
            {
                let _mutation = callback_control.begin_mutation().unwrap();
                let path = callback_library
                    .join("notes")
                    .join(format!("{callback_note_id}.cuenote"));
                let mut opened = storage::open_note(&path).unwrap();
                opened.package.document.title = "Changed during copy".into();
                cuenote_lib::note_format::write_package(
                    std::fs::File::create(&path).unwrap(),
                    &opened.package,
                )
                .unwrap();
            }
        })))
        .unwrap();

    let error = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap_err();
    assert_eq!(error.kind, "backupCancelled");
    assert!(changed.load(Ordering::Acquire));
    assert!(library_path
        .join("notes")
        .join(format!("{}.cuenote", note.note_id))
        .is_file());

    let backup_root = fs::read_dir(context.directory())
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
        .map(|entry| entry.path())
        .unwrap();
    assert!(fs::read_dir(backup_root)
        .unwrap()
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(".staging-")));
}

#[test]
fn source_change_at_publication_progress_is_caught_by_the_final_source_scan() {
    let (_temp, mut workspace, library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Before".into(),
                cue: String::new(),
                content: "Before publication".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let callback_changed = Arc::clone(&changed);
    let callback_library = library_path.clone();
    let callback_note_id = note.note_id.clone();
    let control = BackupControl::default();
    let job = control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == cuenote_lib::workspace::BackupPhase::Publishing
                && progress.checked == 0
                && !callback_changed.swap(true, Ordering::AcqRel)
            {
                let path = callback_library
                    .join("notes")
                    .join(format!("{callback_note_id}.cuenote"));
                let mut opened = storage::open_note(&path).unwrap();
                opened.package.document.title = "Changed before publish".into();
                cuenote_lib::note_format::write_package(
                    std::fs::File::create(&path).unwrap(),
                    &opened.package,
                )
                .unwrap();
            }
        })))
        .unwrap();

    let error = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap_err();
    assert_eq!(error.kind, "backupChanged");
    assert!(changed.load(Ordering::Acquire));
    drop(job);

    let catalog_job = control.begin_job(None).unwrap();
    assert!(context.catalog(&catalog_job).unwrap().backups.is_empty());
}

#[test]
fn changed_automatic_backup_is_preserved_when_retention_revalidates_it() {
    let (_temp, mut workspace, _library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let session_id = note.session_id.clone();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let first_day = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
    let mut oldest_path = None;

    for day in 0..30 {
        let local_date = first_day
            .checked_add_signed(Duration::days(day))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        workspace
            .save_note_text(
                &session_id,
                PlainText {
                    title: "Retention check".into(),
                    cue: String::new(),
                    content: format!("version {day}"),
                    summary: String::new(),
                },
            )
            .unwrap();
        let job = control.begin_job(None).unwrap();
        let outcome = context
            .run(BackupKind::Automatic, &local_date, &job)
            .unwrap();
        if day == 0 {
            oldest_path = outcome.created.map(|backup| PathBuf::from(backup.path));
        }
    }

    let oldest_path = oldest_path.unwrap();
    let oldest_note = oldest_path
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    workspace
        .save_note_text(
            &session_id,
            PlainText {
                title: "Retention check".into(),
                cue: String::new(),
                content: "version 30".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let callback_fired = Arc::new(AtomicBool::new(false));
    let callback_fired_copy = Arc::clone(&callback_fired);
    let corrupted_backup_note = oldest_note.clone();
    let job = control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == cuenote_lib::workspace::BackupPhase::CheckingSource
                && progress.checked == 0
                && !callback_fired_copy.swap(true, Ordering::AcqRel)
            {
                fs::write(&corrupted_backup_note, b"changed after catalog validation").unwrap();
            }
        })))
        .unwrap();
    let local_date = first_day
        .checked_add_signed(Duration::days(30))
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();

    let outcome = context
        .run(BackupKind::Automatic, &local_date, &job)
        .unwrap();
    assert!(outcome.created.is_some());
    assert!(callback_fired.load(Ordering::Acquire));
    assert!(outcome
        .catalog
        .warnings
        .iter()
        .any(|warning| warning.contains("轮换")));
    assert!(oldest_path.is_dir());
    assert_eq!(
        fs::read(oldest_note).unwrap(),
        b"changed after catalog validation"
    );
    assert_eq!(outcome.catalog.backups.len(), 30);
}

#[test]
fn catalog_excludes_a_corrupt_snapshot_without_deleting_it() {
    let (_temp, mut workspace, _library_path) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Integrity check".into(),
                cue: String::new(),
                content: "Catalog must revalidate".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let created = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    drop(job);

    let backup_path = PathBuf::from(created.path);
    let note_path = backup_path
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    fs::write(&note_path, b"corrupt but retain for recovery").unwrap();

    let catalog_job = control.begin_job(None).unwrap();
    let catalog = context.catalog(&catalog_job).unwrap();
    assert!(catalog.backups.is_empty());
    assert_eq!(catalog.warnings.len(), 1);
    assert!(catalog.warnings[0].contains("已排除"));
    assert!(backup_path.is_dir());
    assert_eq!(
        fs::read(note_path).unwrap(),
        b"corrupt but retain for recovery"
    );
}

#[test]
fn default_backup_directory_works_when_config_and_recovery_share_app_support() {
    let temp = workspace_temp();
    let app_support = temp.path().join("Application Support");
    let library_path = temp.path().join("Library");
    let mut workspace = Workspace::new(
        app_support.clone(),
        app_support.join("recovery"),
        library_path.clone(),
    );
    workspace.select_library(library_path).unwrap();

    let context = workspace.backup_context().unwrap();
    assert!(context.directory().ends_with("backups"));
    assert!(context
        .directory()
        .starts_with(fs::canonicalize(app_support).unwrap()));
}

#[test]
fn valid_directory_can_replace_a_preference_that_became_unsafe() {
    let temp = workspace_temp();
    let config_root = temp.path().join("settings");
    let recovery_root = temp.path().join("recovery");
    let first_library = temp.path().join("first-library");
    let future_library = temp.path().join("future-library");
    let invalid_old_directory = future_library.join("old-backups");
    let mut first_workspace = Workspace::new(
        config_root.clone(),
        recovery_root.clone(),
        first_library.clone(),
    );
    first_workspace.select_library(first_library).unwrap();
    first_workspace
        .set_backup_directory(invalid_old_directory)
        .unwrap();
    drop(first_workspace);

    let mut selected_workspace = Workspace::new(config_root, recovery_root, future_library.clone());
    selected_workspace.select_library(future_library).unwrap();
    assert_eq!(
        selected_workspace.backup_context().err().unwrap().kind,
        "unsafePath"
    );

    let replacement = temp.path().join("replacement-backups");
    let context = selected_workspace
        .set_backup_directory(replacement.clone())
        .unwrap();
    assert!(context.directory().ends_with("replacement-backups"));
    assert!(selected_workspace.backup_context().is_ok());
}
