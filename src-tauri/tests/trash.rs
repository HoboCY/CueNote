use cuenote_lib::note_format::{AssetManifestEntry, Block, MediaType, NotePackage, PlainText};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
#[cfg(any(windows, all(debug_assertions, feature = "storage-fault-injection")))]
use std::process::Command;

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("trash-")
        .tempdir_in(base)
        .unwrap()
}

fn selected_workspace() -> (tempfile::TempDir, Workspace, PathBuf, PathBuf, PathBuf) {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    workspace.select_library(library.clone()).unwrap();
    (temp, workspace, library, config, recovery)
}

#[test]
fn soft_deleting_a_note_moves_it_to_read_only_trash_without_changing_content_time() {
    let (_temp, mut workspace, library, _config, _recovery) = selected_workspace();
    let created = workspace.new_note().unwrap();
    let receipt = workspace
        .save_note_text(
            &created.session_id,
            PlainText {
                title: "Keep this title".into(),
                cue: "Keep this cue".into(),
                content: "Keep this body".into(),
                summary: "Keep this summary".into(),
            },
        )
        .unwrap();
    let before = receipt
        .library
        .notes
        .iter()
        .find(|note| note.note_id == created.note_id)
        .unwrap()
        .modified_at
        .clone();

    let trashed = workspace.trash_note(&receipt.session.session_id).unwrap();

    assert_eq!(trashed.session.note_id, created.note_id);
    assert!(trashed.session.trashed_at.is_some());
    assert!(trashed.session.read_only_reason.is_some());
    assert!(!trashed
        .library
        .notes
        .iter()
        .any(|note| note.note_id == created.note_id));
    let in_trash = trashed
        .library
        .trash_notes
        .iter()
        .find(|note| note.note_id == created.note_id)
        .unwrap();
    assert_eq!(in_trash.title, "Keep this title");
    assert_eq!(in_trash.modified_at, before);
    assert!(in_trash.trashed_at.is_some());

    let reopened = workspace.open_note(&created.note_id).unwrap();
    assert_eq!(reopened.text.content, "Keep this body");
    assert!(reopened.trashed_at.is_some());
    assert!(reopened.read_only_reason.is_some());
    let package_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let bytes_before_rejected_edits = std::fs::read(&package_path).unwrap();
    let rejected_text = workspace
        .save_note_text(
            &reopened.session_id,
            PlainText {
                title: "Must not change".into(),
                cue: String::new(),
                content: "Must not change".into(),
                summary: String::new(),
            },
        )
        .unwrap_err();
    assert_eq!(rejected_text.kind, "readOnly");
    let rejected_organization = workspace
        .update_note_organization(
            &reopened.session_id,
            reopened.notebook_id.as_deref().unwrap(),
            true,
        )
        .unwrap_err();
    assert_eq!(rejected_organization.kind, "trashedNote");
    assert_eq!(
        std::fs::read(&package_path).unwrap(),
        bytes_before_rejected_edits
    );
    let unchanged = storage::open_note(&package_path).unwrap().package;
    assert_eq!(
        unchanged.document.organization.notebook_id,
        reopened.notebook_id
    );
    assert!(!unchanged.document.organization.pinned);
}

#[test]
fn restoring_a_note_returns_it_to_its_original_notebook_without_changing_content_time() {
    let (_temp, mut workspace, _library, _config, _recovery) = selected_workspace();
    let initial_library = workspace.snapshot().unwrap().library.unwrap();
    let original_notebook_id = initial_library.default_notebook_id;
    let created = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &created.session_id,
            PlainText {
                title: "Restore me".into(),
                cue: "Cue".into(),
                content: "Body remains unchanged".into(),
                summary: "Summary".into(),
            },
        )
        .unwrap();
    let saved = workspace
        .update_note_organization(&saved.session.session_id, &original_notebook_id, true)
        .unwrap();
    let content_modified_at = saved.library.notes[0].modified_at.clone();
    let trashed = workspace.trash_note(&saved.session.session_id).unwrap();

    let restored = workspace
        .restore_trashed_note(&trashed.session.session_id, None)
        .unwrap();

    assert!(restored.session.trashed_at.is_none());
    assert_eq!(
        restored.session.notebook_id.as_deref(),
        Some(original_notebook_id.as_str())
    );
    assert!(restored.session.pinned);
    assert_eq!(restored.session.text.content, "Body remains unchanged");
    assert!(!restored
        .library
        .trash_notes
        .iter()
        .any(|note| note.note_id == created.note_id));
    let listed = restored
        .library
        .notes
        .iter()
        .find(|note| note.note_id == created.note_id)
        .unwrap();
    assert_eq!(listed.modified_at, content_modified_at);
    assert!(listed.trashed_at.is_none());
}

#[test]
fn trash_and_restore_preserve_rich_sections_embedded_assets_and_pin_state() {
    let (_temp, mut workspace, library, _config, recovery) = selected_workspace();
    let default_notebook_id = workspace
        .snapshot()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id;
    let mut package = NotePackage::new_plain(
        default_notebook_id.clone(),
        PlainText {
            title: "Rich package".into(),
            cue: "Cue stays rich".into(),
            content: "Body stays rich".into(),
            summary: "Summary stays rich".into(),
        },
    );
    package.document.organization.pinned = true;
    let asset_id = "55ad7e2b-9523-4f0b-9041-426d7f94d924".to_owned();
    let asset = include_bytes!("fixtures/pixel.png").to_vec();
    package.document.sections.content.push(Block::Image {
        asset_id: asset_id.clone(),
        display_width_px: 1,
    });
    package.manifest.assets.push(AssetManifestEntry {
        id: asset_id.clone(),
        media_type: MediaType::Png,
        byte_length: asset.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&asset)),
        width: 1,
        height: 1,
    });
    package.assets.insert(asset_id, asset);
    let note_id = package.document.note_id.clone();
    let package_path = library.join("notes").join(format!("{note_id}.cuenote"));
    storage::save_note(&package_path, &recovery, &package, None).unwrap();
    let opened = workspace.open_note(&note_id).unwrap();
    let content_modified_at = package.document.timestamps.content_modified_at.clone();

    let trashed = workspace.trash_note(&opened.session_id).unwrap();
    let after_trash = storage::open_note(&package_path).unwrap().package;
    assert_eq!(after_trash.document.sections, package.document.sections);
    assert_eq!(after_trash.manifest, package.manifest);
    assert_eq!(after_trash.assets, package.assets);
    assert_eq!(
        after_trash.document.timestamps.content_modified_at,
        content_modified_at
    );
    assert!(after_trash.document.organization.pinned);
    assert!(after_trash.document.organization.trashed_at.is_some());

    let restored = workspace
        .restore_trashed_note(&trashed.session.session_id, None)
        .unwrap();
    let after_restore = storage::open_note(&package_path).unwrap().package;
    assert_eq!(after_restore.document.sections, package.document.sections);
    assert_eq!(after_restore.manifest, package.manifest);
    assert_eq!(after_restore.assets, package.assets);
    assert_eq!(
        after_restore.document.timestamps.content_modified_at,
        content_modified_at
    );
    assert!(after_restore.document.organization.pinned);
    assert!(after_restore.document.organization.trashed_at.is_none());
    assert_eq!(
        restored.session.notebook_id.as_deref(),
        Some(default_notebook_id.as_str())
    );
}

#[test]
fn trash_listing_sorts_by_trash_time_then_note_id() {
    let (_temp, mut workspace, library, _config, recovery) = selected_workspace();
    let default_notebook_id = workspace
        .snapshot()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id;
    let fixtures = [
        (
            "30000000-0000-4000-8000-000000000003",
            "2026-01-02T00:00:00.000Z",
        ),
        (
            "10000000-0000-4000-8000-000000000001",
            "2026-01-03T00:00:00.000Z",
        ),
        (
            "20000000-0000-4000-8000-000000000002",
            "2026-01-02T00:00:00.000Z",
        ),
        (
            "40000000-0000-4000-8000-000000000004",
            "2026-01-01T00:00:00.000Z",
        ),
    ];
    for (note_id, trashed_at) in fixtures {
        let mut package = NotePackage::new_plain(
            default_notebook_id.clone(),
            PlainText {
                title: note_id.to_owned(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        );
        package.document.note_id = note_id.to_owned();
        package.document.organization.trashed_at = Some(trashed_at.to_owned());
        package.document.timestamps.organization_modified_at = trashed_at.to_owned();
        let path = library.join("notes").join(format!("{note_id}.cuenote"));
        storage::save_note(&path, &recovery, &package, None).unwrap();
    }

    let listed = workspace.snapshot().unwrap().library.unwrap().trash_notes;

    assert_eq!(
        listed
            .iter()
            .map(|note| note.note_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "10000000-0000-4000-8000-000000000001",
            "20000000-0000-4000-8000-000000000002",
            "30000000-0000-4000-8000-000000000003",
            "40000000-0000-4000-8000-000000000004",
        ]
    );
}

#[test]
fn permanent_deletion_requires_confirmation_and_preserves_same_title_notes_and_history() {
    let (_temp, mut workspace, library, _config, recovery) = selected_workspace();
    let first = workspace.new_note().unwrap();
    let first = workspace
        .save_note_text(
            &first.session_id,
            PlainText {
                title: "Same title".into(),
                cue: String::new(),
                content: "First version".into(),
                summary: String::new(),
            },
        )
        .unwrap()
        .session;
    workspace
        .save_note_text(
            &first.session_id,
            PlainText {
                title: "Same title".into(),
                cue: String::new(),
                content: "Current version".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let second = workspace.new_note().unwrap();
    let second = workspace
        .save_note_text(
            &second.session_id,
            PlainText {
                title: "Same title".into(),
                cue: String::new(),
                content: "Keep this other note".into(),
                summary: String::new(),
            },
        )
        .unwrap()
        .session;
    let first_path = library
        .join("notes")
        .join(format!("{}.cuenote", first.note_id));
    let second_path = library
        .join("notes")
        .join(format!("{}.cuenote", second.note_id));
    let history_directory = recovery
        .join("history")
        .join(format!("{:x}", Sha256::digest(first.note_id.as_bytes())));
    assert_eq!(std::fs::read_dir(&history_directory).unwrap().count(), 1);

    let first = workspace.open_note(&first.note_id).unwrap();
    let trashed = workspace.trash_note(&first.session_id).unwrap();
    let history_after_trash = std::fs::read_dir(&history_directory).unwrap().count();
    assert!(history_after_trash >= 1);
    let trash_session = workspace.open_note(&trashed.session.note_id).unwrap();
    let cancelled = workspace
        .delete_trashed_note(&trash_session.session_id, false)
        .unwrap_err();

    assert_eq!(cancelled.kind, "confirmationRequired");
    assert!(first_path.is_file());
    assert!(second_path.is_file());
    assert_eq!(
        std::fs::read_dir(&history_directory).unwrap().count(),
        history_after_trash
    );
    let unchanged = workspace.snapshot().unwrap();
    assert_eq!(
        unchanged.session.unwrap().session_id,
        trash_session.session_id
    );
    assert!(unchanged
        .library
        .unwrap()
        .trash_notes
        .iter()
        .any(|note| note.note_id == first.note_id));

    let deleted = workspace
        .delete_trashed_note(&trash_session.session_id, true)
        .unwrap();
    assert!(deleted.session.is_none());
    assert!(!first_path.exists());
    assert!(second_path.is_file());
    assert_eq!(
        std::fs::read_dir(&history_directory).unwrap().count(),
        history_after_trash
    );
    let library_after = deleted.library.unwrap();
    assert!(!library_after
        .trash_notes
        .iter()
        .any(|note| note.note_id == first.note_id));
    assert!(library_after
        .notes
        .iter()
        .any(|note| note.note_id == second.note_id && note.title == "Same title"));
}

#[test]
fn a_permanent_delete_lock_failure_keeps_the_package_and_active_session() {
    let (_temp, mut workspace, library, _config, recovery) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Lock failure".into(),
                cue: String::new(),
                content: "Keep this package".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let trashed = workspace.trash_note(&saved.session.session_id).unwrap();
    let package_path = library
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let original_bytes = std::fs::read(&package_path).unwrap();
    let locks_directory = recovery.join(".locks");
    let canonical_target = std::fs::canonicalize(package_path.parent().unwrap())
        .unwrap()
        .join(package_path.file_name().unwrap());
    let target_key = format!(
        "{:x}",
        Sha256::digest(canonical_target.to_string_lossy().to_lowercase().as_bytes())
    );
    let save_lock = locks_directory.join(format!("{target_key}.lock"));
    let preserved_lock = locks_directory.join(format!("{target_key}.lock-preserved"));
    // Fail only this target's save-lock open. Do not move .locks: it also
    // contains the active baseline VersionLease handle on Windows.
    std::fs::rename(&save_lock, &preserved_lock).unwrap();
    std::fs::create_dir(&save_lock).unwrap();

    let error = workspace
        .delete_trashed_note(&trashed.session.session_id, true)
        .unwrap_err();

    assert_eq!(error.kind, "io");
    assert_eq!(std::fs::read(&package_path).unwrap(), original_bytes);
    let after_failure = workspace.snapshot().unwrap();
    assert_eq!(
        after_failure.session.unwrap().session_id,
        trashed.session.session_id
    );
    assert!(after_failure
        .library
        .unwrap()
        .trash_notes
        .iter()
        .any(|summary| summary.note_id == note.note_id));
}

#[test]
fn a_permanent_delete_rejects_an_external_revision_and_keeps_the_session() {
    let (_temp, mut workspace, library, _config, recovery) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "External revision".into(),
                cue: String::new(),
                content: "Session baseline".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let trashed = workspace.trash_note(&saved.session.session_id).unwrap();
    let package_path = library
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let external = storage::open_note(&package_path).unwrap();
    let mut external_package = external.package;
    external_package.document.organization.pinned = true;
    external_package
        .document
        .timestamps
        .organization_modified_at = "2026-12-31T23:59:59.999Z".into();
    storage::save_note(
        &package_path,
        &recovery,
        &external_package,
        Some(&external.revision),
    )
    .unwrap();

    let error = workspace
        .delete_trashed_note(&trashed.session.session_id, true)
        .unwrap_err();

    assert_eq!(error.kind, "conflict");
    let latest = storage::open_note(&package_path).unwrap();
    assert!(latest.package.document.organization.pinned);
    assert_eq!(latest.package.text_projection().content, "Session baseline");
    let after_failure = workspace.snapshot().unwrap();
    assert_eq!(
        after_failure.session.unwrap().session_id,
        trashed.session.session_id
    );
    assert!(after_failure
        .library
        .unwrap()
        .trash_notes
        .iter()
        .any(|summary| summary.note_id == note.note_id));
}

#[test]
fn deleting_the_original_notebook_requires_an_explicit_restore_target() {
    let (_temp, mut workspace, _library, _config, _recovery) = selected_workspace();
    let notebooks = workspace.create_notebook("Research").unwrap();
    let original_notebook_id = notebooks
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Research")
        .unwrap()
        .notebook_id
        .clone();
    let default_notebook_id = notebooks.default_notebook_id;
    let created = workspace.new_note_in(Some(&original_notebook_id)).unwrap();
    let saved = workspace
        .save_note_text(
            &created.session_id,
            PlainText {
                title: "Moved after restore".into(),
                cue: String::new(),
                content: "Still here".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let trashed = workspace.trash_note(&saved.session.session_id).unwrap();

    let deleted_notebook = workspace.delete_notebook(&original_notebook_id).unwrap();

    assert!(!deleted_notebook
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == original_notebook_id));
    let missing_target = workspace
        .restore_trashed_note(&trashed.session.session_id, None)
        .unwrap_err();
    assert_eq!(missing_target.kind, "restoreTargetRequired");
    assert!(workspace
        .snapshot()
        .unwrap()
        .session
        .unwrap()
        .trashed_at
        .is_some());

    let restored = workspace
        .restore_trashed_note(&trashed.session.session_id, Some(&default_notebook_id))
        .unwrap();
    assert_eq!(
        restored.session.notebook_id.as_deref(),
        Some(default_notebook_id.as_str())
    );
    assert!(restored.session.trashed_at.is_none());
}

#[test]
fn startup_returns_to_the_list_when_the_remembered_note_is_now_trashed() {
    let (temp, mut workspace, library, config, recovery) = selected_workspace();
    let default = temp.path().join("default");
    let created = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &created.session_id,
            PlainText {
                title: "Remembered note".into(),
                cue: String::new(),
                content: "Keep the note in trash".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", saved.session.note_id));
    drop(workspace);

    let opened = storage::open_note(&note_path).unwrap();
    let mut trashed_package = opened.package;
    trashed_package.document.organization.trashed_at = Some("2026-10-16T12:00:01.000Z".into());
    trashed_package.document.timestamps.organization_modified_at =
        "2026-10-16T12:00:00.000Z".into();
    storage::save_note(
        &note_path,
        &recovery,
        &trashed_package,
        Some(&opened.revision),
    )
    .unwrap();

    let restarted = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();

    assert!(restarted.session.is_none());
    assert!(restarted.notice.unwrap().contains("回收站"));
    let library_view = restarted.library.unwrap();
    assert!(library_view.notes.is_empty());
    assert_eq!(library_view.trash_notes.len(), 1);
    assert_eq!(library_view.trash_notes[0].note_id, saved.session.note_id);
}

#[cfg(all(debug_assertions, feature = "storage-fault-injection"))]
#[test]
fn a_publish_after_error_reconciles_the_trashed_session_and_allows_restore() {
    const WORKER_ROOT: &str = "CUENOTE_TRASH_PUBLISH_WORKER_ROOT";
    const TEST_NAME: &str =
        "a_publish_after_error_reconciles_the_trashed_session_and_allows_restore";

    if let Some(worker_root) = std::env::var_os(WORKER_ROOT) {
        let root = PathBuf::from(worker_root);
        let mut workspace = Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            root.join("default"),
        );
        let snapshot = workspace.snapshot().unwrap();
        let active = snapshot.session.unwrap();
        let trashed = workspace
            .trash_note(&active.session_id)
            .expect("a complete package published before the injected error must reconcile");
        assert!(trashed.session.trashed_at.is_some());
        assert!(trashed
            .library
            .warnings
            .iter()
            .any(|warning| warning.contains("发布后") || warning.contains("保存后")));

        let restored = workspace
            .restore_trashed_note(&trashed.session.session_id, None)
            .expect("the reconciled trash state must be the baseline for restore");
        assert!(restored.session.trashed_at.is_none());
        assert_eq!(restored.session.text.content, active.text.content);
        return;
    }

    let (temp, mut workspace, library, _config, _recovery) = selected_workspace();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Published before injected failure".into(),
                cue: "Cue preserved".into(),
                content: "Body preserved".into(),
                summary: "Summary preserved".into(),
            },
        )
        .unwrap();
    let package_path = library
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    drop(workspace);

    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(TEST_NAME)
        .arg("--nocapture")
        .env(WORKER_ROOT, temp.path())
        .env("CUENOTE_FAULT_ROOT", temp.path())
        .env("CUENOTE_FAULT_STAGE", "publish_after")
        .env("CUENOTE_FAULT_ACTION", "io")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fault worker failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        temp.path()
            .join(".cuenote-fault-hit-publish_after")
            .is_file(),
        "publish-after fault injection did not reach the requested storage boundary"
    );
    let published = storage::open_note(&package_path).unwrap();
    assert!(published.package.document.organization.trashed_at.is_none());
    assert_eq!(
        published.package.text_projection().content,
        "Body preserved"
    );
}

#[cfg(windows)]
#[test]
fn permanent_deletion_refuses_a_reparse_point_notes_directory() {
    let (temp, mut workspace, library, _config, _recovery) = selected_workspace();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Must stay outside".into(),
                cue: String::new(),
                content: "Protected by the library boundary".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let trashed = workspace.trash_note(&saved.session.session_id).unwrap();
    let note_name = format!("{}.cuenote", note.note_id);
    let notes_path = library.join("notes");
    let original_notes = library.join("notes-original");
    let outside = temp.path().join("outside-notes");
    std::fs::create_dir(&outside).unwrap();
    std::fs::copy(notes_path.join(&note_name), outside.join(&note_name)).unwrap();
    let original_hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(notes_path.join(&note_name)).unwrap())
    );
    let external_hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(outside.join(&note_name)).unwrap())
    );
    assert_eq!(original_hash, external_hash);
    std::fs::rename(&notes_path, &original_notes).unwrap();
    let normal_path = |path: &Path| {
        let value = path.to_string_lossy();
        value
            .strip_prefix(r"\\?\")
            .unwrap_or(value.as_ref())
            .replace('/', "\\")
    };
    let junction_target = normal_path(&notes_path);
    let junction_source = normal_path(&outside);
    let linked = Command::new("cmd.exe")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&junction_target)
        .arg(&junction_source)
        .output()
        .unwrap();
    let link_error = String::from_utf8_lossy(&linked.stderr);
    assert!(
        linked.status.success(),
        "could not create isolated junction fixture: target={junction_target:?}, source={junction_source:?}, stdout={}, stderr={}",
        String::from_utf8_lossy(&linked.stdout),
        link_error,
    );
    let result = workspace.delete_trashed_note(&trashed.session.session_id, true);
    let external_after = std::fs::read(outside.join(&note_name)).unwrap();
    let original_after = std::fs::read(original_notes.join(&note_name)).unwrap();
    let active_session = workspace.snapshot().unwrap().session;
    std::fs::remove_dir(&notes_path).unwrap();

    let error = result.unwrap_err();
    assert_eq!(error.kind, "unsafePath");
    assert_eq!(
        format!("{:x}", Sha256::digest(external_after)),
        external_hash
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(original_after)),
        original_hash
    );
    assert_eq!(
        active_session.unwrap().session_id,
        trashed.session.session_id
    );
    assert!(temp.path().exists());
}
