use cuenote_lib::note_format::PlainText;
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::path::{Path, PathBuf};
#[cfg(all(debug_assertions, feature = "storage-fault-injection"))]
use std::process::Command;

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("notebook-management-")
        .tempdir_in(base)
        .unwrap()
}

fn selected_workspace() -> (
    tempfile::TempDir,
    Workspace,
    PathBuf,
    PathBuf,
    PathBuf,
    PathBuf,
) {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    workspace.select_library(library.clone()).unwrap();
    (temp, workspace, library, config, recovery, default)
}

#[test]
fn create_notebook_trims_name_and_persists_empty_notebook() {
    let (_temp, mut workspace, library_path, config, recovery, default) = selected_workspace();

    let library = workspace.create_notebook("  Reading  ").unwrap();
    let notebook = library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Reading")
        .unwrap();
    let notebook_id = notebook.notebook_id.clone();
    assert!(!notebook.is_default);
    assert!(library.notes.is_empty());
    assert!(library_path
        .join("notebooks")
        .join(format!("{notebook_id}.json"))
        .is_file());

    let reopened = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();
    assert!(reopened
        .library
        .unwrap()
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == notebook_id && notebook.name == "Reading"));
}

#[test]
fn note_organization_keeps_content_time_and_sorts_pinned_notes_first() {
    let (_temp, mut workspace, library_path, _config, _recovery, _default) = selected_workspace();
    let custom = workspace.create_notebook("Study").unwrap();
    let notebook_id = custom
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Study")
        .unwrap()
        .notebook_id
        .clone();
    let default_notebook_id = custom.default_notebook_id.clone().unwrap();

    let first_text = PlainText {
        title: String::new(),
        cue: String::new(),
        content: "Body remains intact".into(),
        summary: String::new(),
    };
    let first = workspace.new_note_in(Some(&notebook_id)).unwrap();
    workspace
        .save_note_text(&first.session_id, first_text)
        .unwrap();
    let second = workspace.new_note_in(Some(&notebook_id)).unwrap();
    workspace
        .save_note_text(
            &second.session_id,
            PlainText {
                title: String::new(),
                cue: String::new(),
                content: "Newer pinned body".into(),
                summary: "Newer pinned takeaway".into(),
            },
        )
        .unwrap();
    let third = workspace.new_note_in(Some(&notebook_id)).unwrap();
    workspace
        .save_note_text(
            &third.session_id,
            PlainText {
                title: String::new(),
                cue: String::new(),
                content: "Newest unpinned body".into(),
                summary: "Newest unpinned takeaway".into(),
            },
        )
        .unwrap();

    set_content_modified_at(&library_path, &first.note_id, "2026-10-01T12:00:00.000Z");
    set_content_modified_at(&library_path, &second.note_id, "2026-10-02T12:00:00.000Z");
    set_content_modified_at(&library_path, &third.note_id, "2026-10-03T12:00:00.000Z");

    let second_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", second.note_id));
    let before = storage::open_note(&second_path).unwrap();
    let first_session = workspace.open_note(&first.note_id).unwrap();
    workspace
        .update_note_organization(&first_session.session_id, &notebook_id, true)
        .unwrap();
    let second_session = workspace.open_note(&second.note_id).unwrap();
    let updated = workspace
        .update_note_organization(&second_session.session_id, &default_notebook_id, true)
        .unwrap();
    let after = storage::open_note(&second_path).unwrap();

    assert_eq!(
        after.package.document.timestamps.content_modified_at,
        before.package.document.timestamps.content_modified_at
    );
    assert!(
        after.package.document.timestamps.organization_modified_at
            > before.package.document.timestamps.organization_modified_at
    );
    assert_eq!(
        after.package.text_projection().summary,
        "Newer pinned takeaway"
    );
    assert_eq!(
        updated.session.notebook_id.as_deref(),
        Some(default_notebook_id.as_str())
    );
    assert!(updated.session.pinned);
    assert_eq!(
        updated
            .library
            .notes
            .iter()
            .map(|note| note.note_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            second.note_id.as_str(),
            first.note_id.as_str(),
            third.note_id.as_str()
        ]
    );
    assert!(updated.library.notes[0].pinned);
    assert!(updated.library.notes[1].pinned);
    assert!(!updated.library.notes[2].pinned);
    assert_eq!(updated.library.notes[0].excerpt, "Newer pinned takeaway");
    assert_eq!(updated.library.notes[1].excerpt, "Body remains intact");
    assert_eq!(updated.library.notes[0].title, "");
    assert!(updated.library.notes[0].modified_at > updated.library.notes[1].modified_at);
    assert!(updated.library.notes[2].modified_at > updated.library.notes[0].modified_at);

    let opened = workspace.open_note(&first.note_id).unwrap();
    assert_eq!(opened.notebook_id.as_deref(), Some(notebook_id.as_str()));
    assert!(opened.pinned);
    assert_eq!(
        storage::open_note(
            &library_path
                .join("notes")
                .join(format!("{}.cuenote", first.note_id))
        )
        .unwrap()
        .package
        .document
        .timestamps
        .content_modified_at,
        "2026-10-01T12:00:00.000Z"
    );
}

fn set_content_modified_at(library: &Path, note_id: &str, timestamp: &str) {
    let recovery = library.parent().unwrap().join("recovery");
    let path = library.join("notes").join(format!("{note_id}.cuenote"));
    let mut opened = storage::open_note(&path).unwrap();
    opened.package.document.timestamps.content_modified_at = timestamp.into();
    storage::save_note(&path, &recovery, &opened.package, Some(&opened.revision)).unwrap();
}

#[test]
fn notebook_names_are_trimmed_unique_and_default_notebook_is_protected() {
    let (_temp, mut workspace, _library_path, _config, _recovery, _default) = selected_workspace();
    let default_id = workspace
        .snapshot()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .unwrap();
    assert_eq!(
        workspace.delete_notebook(&default_id).unwrap_err().kind,
        "defaultNotebook"
    );
    assert_eq!(
        workspace.create_notebook("  \t  ").unwrap_err().kind,
        "invalidNotebookName"
    );

    let first = workspace.create_notebook("  Reading  ").unwrap();
    assert_eq!(
        first
            .notebooks
            .iter()
            .find(|notebook| !notebook.is_default)
            .unwrap()
            .name,
        "Reading"
    );
    assert_eq!(
        workspace.create_notebook("reading").unwrap_err().kind,
        "duplicateNotebookName"
    );

    let second = workspace.create_notebook("Archive").unwrap();
    let archive_id = second
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Archive")
        .unwrap()
        .notebook_id
        .clone();
    assert_eq!(
        workspace
            .rename_notebook(&archive_id, " READING ")
            .unwrap_err()
            .kind,
        "duplicateNotebookName"
    );
    let renamed = workspace
        .rename_notebook(&archive_id, "  Keepsakes  ")
        .unwrap();
    assert!(renamed
        .notebooks
        .iter()
        .any(|notebook| notebook.name == "Keepsakes"));

    let empty = workspace.create_notebook("Empty").unwrap();
    let empty_id = empty
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Empty")
        .unwrap()
        .notebook_id
        .clone();
    let deleted = workspace.delete_notebook(&empty_id).unwrap();
    assert!(!deleted
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == empty_id));
}

#[test]
fn deletion_keeps_unsaved_notebooks_and_refuses_unreadable_note_usage() {
    let (_temp, mut workspace, library_path, _config, _recovery, _default) = selected_workspace();
    let open_library = workspace.create_notebook("Open draft").unwrap();
    let open_id = open_library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Open draft")
        .unwrap()
        .notebook_id
        .clone();
    let draft = workspace.new_note_in(Some(&open_id)).unwrap();
    let open_metadata = library_path
        .join("notebooks")
        .join(format!("{open_id}.json"));
    let open_bytes = std::fs::read(&open_metadata).unwrap();
    assert_eq!(
        workspace.delete_notebook(&open_id).unwrap_err().kind,
        "notebookInUse"
    );
    assert_eq!(std::fs::read(open_metadata).unwrap(), open_bytes);
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().session_id,
        draft.session_id
    );

    let occupied_library = workspace.create_notebook("Occupied").unwrap();
    let occupied_id = occupied_library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Occupied")
        .unwrap()
        .notebook_id
        .clone();
    let occupied_note = workspace.new_note_in(Some(&occupied_id)).unwrap();
    workspace
        .save_note_text(
            &occupied_note.session_id,
            PlainText {
                title: "In use".into(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();
    let occupied_error = workspace.delete_notebook(&occupied_id).unwrap_err();
    assert_eq!(occupied_error.kind, "notebookInUse");
    assert!(occupied_error.message.contains("移动到其他笔记本"));

    let corrupt_library = workspace.create_notebook("Unreadable note").unwrap();
    let corrupt_id = corrupt_library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Unreadable note")
        .unwrap()
        .notebook_id
        .clone();
    let corrupt_metadata = library_path
        .join("notebooks")
        .join(format!("{corrupt_id}.json"));
    let corrupt_metadata_bytes = std::fs::read(&corrupt_metadata).unwrap();
    let corrupt_note_id = "11111111-1111-4111-8111-111111111111";
    let corrupt_note_path = library_path
        .join("notes")
        .join(format!("{corrupt_note_id}.cuenote"));
    let original_note_bytes = b"unreadable package bytes";
    std::fs::write(&corrupt_note_path, original_note_bytes).unwrap();

    assert_eq!(
        workspace.delete_notebook(&corrupt_id).unwrap_err().kind,
        "notebookUsageUnknown"
    );
    assert_eq!(
        std::fs::read(&corrupt_metadata).unwrap(),
        corrupt_metadata_bytes
    );
    assert_eq!(
        std::fs::read(corrupt_note_path).unwrap(),
        original_note_bytes
    );
}

#[test]
fn organization_update_preserves_rich_package_assets_and_noop_bytes() {
    let (_temp, mut workspace, library_path, _config, recovery, _default) = selected_workspace();
    let target_library = workspace.create_notebook("Archive").unwrap();
    let target_id = target_library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Archive")
        .unwrap()
        .notebook_id
        .clone();
    let default_id = target_library.default_notebook_id.clone().unwrap();
    let mut package = cuenote_lib::note_format::read_package(std::io::Cursor::new(include_bytes!(
        "fixtures/valid-rich-v1.cuenote"
    )))
    .unwrap();
    package.document.organization.notebook_id = Some(default_id);
    let note_id = package.document.note_id.clone();
    let path = library_path
        .join("notes")
        .join(format!("{note_id}.cuenote"));
    storage::save_note(&path, &recovery, &package, None).unwrap();
    let original = storage::open_note(&path).unwrap();
    let original_content_time = original
        .package
        .document
        .timestamps
        .content_modified_at
        .clone();
    let session = workspace.open_note(&note_id).unwrap();

    let changed = workspace
        .update_note_organization(&session.session_id, &target_id, true)
        .unwrap();
    let organized = storage::open_note(&path).unwrap();
    assert_eq!(
        organized.package.document.sections,
        original.package.document.sections
    );
    assert_eq!(organized.package.manifest, original.package.manifest);
    assert_eq!(organized.package.assets, original.package.assets);
    assert_eq!(
        organized.package.document.timestamps.content_modified_at,
        original_content_time
    );
    assert_eq!(
        changed.session.notebook_id.as_deref(),
        Some(target_id.as_str())
    );
    assert!(changed.session.pinned);

    let bytes_before_noop = std::fs::read(&path).unwrap();
    let revision_before_noop = organized.revision;
    workspace
        .update_note_organization(&changed.session.session_id, &target_id, true)
        .unwrap();
    let no_op_reopen = storage::open_note(&path).unwrap();
    assert_eq!(no_op_reopen.revision, revision_before_noop);
    assert_eq!(std::fs::read(&path).unwrap(), bytes_before_noop);
}

#[test]
fn failed_organization_update_preserves_active_session_after_external_revision() {
    let (_temp, mut workspace, library_path, _config, recovery, _default) = selected_workspace();
    let custom = workspace.create_notebook("Target").unwrap();
    let target_id = custom
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Target")
        .unwrap()
        .notebook_id
        .clone();
    let created = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &created.session_id,
            PlainText {
                title: "Local copy".into(),
                cue: "cue stays".into(),
                content: "active text stays".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let active = workspace.open_note(&created.note_id).unwrap();
    let path = library_path
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let mut external = storage::open_note(&path).unwrap();
    external.package.document.title = "External edit".into();
    let external_revision = storage::save_note(
        &path,
        &recovery,
        &external.package,
        Some(&external.revision),
    )
    .unwrap();

    assert_eq!(
        workspace
            .update_note_organization(&active.session_id, &target_id, true)
            .unwrap_err()
            .kind,
        "conflict"
    );
    let after_failure = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(after_failure.session_id, active.session_id);
    assert_eq!(after_failure.note_id, created.note_id);
    assert_eq!(after_failure.text.content, "active text stays");
    assert_eq!(after_failure.text.cue, "cue stays");
    assert!(!after_failure.pinned);
    let retained_external = storage::open_note(&path).unwrap();
    assert_eq!(retained_external.revision, external_revision);
    assert_eq!(retained_external.package.document.title, "External edit");
    assert_ne!(
        retained_external
            .package
            .document
            .organization
            .notebook_id
            .as_deref(),
        Some(target_id.as_str())
    );
    assert!(!retained_external.package.document.organization.pinned);
}

#[cfg(all(debug_assertions, feature = "storage-fault-injection"))]
#[test]
fn organization_update_reconciles_a_published_error_before_the_next_text_save() {
    const WORKER_ROOT: &str = "CUENOTE_NOTEBOOK_ORGANIZATION_WORKER_ROOT";
    const TEST_NAME: &str =
        "organization_update_reconciles_a_published_error_before_the_next_text_save";

    if let Some(worker_root) = std::env::var_os(WORKER_ROOT) {
        let root = PathBuf::from(worker_root);
        let mut workspace = Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            root.join("default"),
        );
        let snapshot = workspace.snapshot().unwrap();
        let library = snapshot.library.unwrap();
        let active = snapshot.session.unwrap();
        let target_id = library
            .notebooks
            .iter()
            .find(|notebook| notebook.name == "Published")
            .unwrap()
            .notebook_id
            .clone();

        let organized = workspace
            .update_note_organization(&active.session_id, &target_id, true)
            .expect(
                "a complete candidate verified at the target should update the session and return a warning",
            );
        assert!(organized.session.pinned);
        assert_eq!(
            organized.session.notebook_id.as_deref(),
            Some(target_id.as_str())
        );
        assert!(organized
            .library
            .warnings
            .iter()
            .any(|warning| warning.contains("发布后") || warning.contains("保存后")));

        let path = root
            .join("library/notes")
            .join(format!("{}.cuenote", active.note_id));
        let after_organization = storage::open_note(&path).unwrap();
        let original_text = active.text.clone();
        assert_eq!(after_organization.package.text_projection(), original_text);
        assert_eq!(
            after_organization
                .package
                .document
                .organization
                .notebook_id
                .as_deref(),
            Some(target_id.as_str())
        );
        assert!(after_organization.package.document.organization.pinned);

        let next_text = PlainText {
            content: "Follow-up text save succeeds".into(),
            ..original_text
        };
        let saved = workspace
            .save_note_text(&organized.session.session_id, next_text.clone())
            .expect("the reconciled published revision must be the next save baseline");
        assert_eq!(saved.session.text, next_text);
        let after_text_save = storage::open_note(&path).unwrap();
        assert_eq!(after_text_save.package.text_projection(), next_text);
        assert_eq!(
            after_text_save
                .package
                .document
                .organization
                .notebook_id
                .as_deref(),
            Some(target_id.as_str())
        );
        assert!(after_text_save.package.document.organization.pinned);
        return;
    }

    let (temp, mut workspace, library_path, _config, _recovery, _default) = selected_workspace();
    let custom = workspace.create_notebook("Published").unwrap();
    let target_id = custom
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Published")
        .unwrap()
        .notebook_id
        .clone();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Saved before organization".into(),
                cue: "Keep this cue".into(),
                content: "Keep this content".into(),
                summary: "Keep this summary".into(),
            },
        )
        .unwrap();
    let path = library_path
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));

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
    let after_worker = storage::open_note(&path).unwrap();
    assert_eq!(
        after_worker.package.text_projection().content,
        "Follow-up text save succeeds"
    );
    assert_eq!(
        after_worker
            .package
            .document
            .organization
            .notebook_id
            .as_deref(),
        Some(target_id.as_str())
    );
    assert!(after_worker.package.document.organization.pinned);
}

#[cfg(windows)]
#[test]
fn failed_notebook_rename_keeps_metadata_bytes_and_active_draft() {
    use std::os::windows::fs::OpenOptionsExt;

    let (_temp, mut workspace, library_path, _config, _recovery, _default) = selected_workspace();
    let library = workspace.create_notebook("Before").unwrap();
    let notebook_id = library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Before")
        .unwrap()
        .notebook_id
        .clone();
    let metadata_path = library_path
        .join("notebooks")
        .join(format!("{notebook_id}.json"));
    let original_bytes = std::fs::read(&metadata_path).unwrap();
    let draft = workspace.new_note_in(Some(&notebook_id)).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001)
        .open(&metadata_path)
        .unwrap();

    let error = workspace
        .rename_notebook(&notebook_id, "After")
        .unwrap_err();

    assert_eq!(error.kind, "publish");
    assert_eq!(std::fs::read(&metadata_path).unwrap(), original_bytes);
    let current = workspace.snapshot().unwrap();
    assert_eq!(current.session.unwrap().session_id, draft.session_id);
    assert!(current
        .library
        .unwrap()
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == notebook_id && notebook.name == "Before"));
    drop(lock);
}

#[test]
fn view_preferences_preserve_last_note_across_restart_and_missing_note_returns_to_list() {
    let (_temp, mut workspace, library_path, config, recovery, default) = selected_workspace();
    let library = workspace.create_notebook("Projects").unwrap();
    let notebook_id = library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Projects")
        .unwrap()
        .notebook_id
        .clone();
    let note = workspace.new_note_in(Some(&notebook_id)).unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Reopen me".into(),
                cue: String::new(),
                content: "saved content".into(),
                summary: String::new(),
            },
        )
        .unwrap();

    workspace
        .set_view_preferences(true, Some(notebook_id.clone()))
        .unwrap();
    let same_session = workspace.snapshot().unwrap();
    assert_eq!(same_session.session.unwrap().session_id, note.session_id);

    let mut restarted = Workspace::new(config.clone(), recovery.clone(), default.clone());
    let restored = restarted.snapshot().unwrap();
    assert_eq!(restored.session.unwrap().note_id, note.note_id);
    assert!(restored.preferences.sidebar_collapsed);
    assert_eq!(
        restored.preferences.selected_notebook_id.as_deref(),
        Some(notebook_id.as_str())
    );

    std::fs::remove_file(
        library_path
            .join("notes")
            .join(format!("{}.cuenote", note.note_id)),
    )
    .unwrap();
    let missing = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();
    assert!(missing.session.is_none());
    assert!(missing.library.unwrap().notes.is_empty());
}

#[test]
fn legacy_workspace_preferences_without_view_fields_still_load() {
    let (_temp, _workspace, _library_path, config, recovery, default) = selected_workspace();
    let preference_path = std::fs::read_dir(&config)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("workspace-"))
        })
        .unwrap();
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&preference_path).unwrap()).unwrap();
    let object = json.as_object_mut().unwrap();
    object.remove("lastNoteId");
    object.remove("sidebarCollapsed");
    object.remove("selectedNotebookId");
    std::fs::write(&preference_path, serde_json::to_vec(&json).unwrap()).unwrap();

    let restored = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();
    assert!(restored.session.is_none());
    assert!(!restored.preferences.sidebar_collapsed);
    assert!(restored.preferences.selected_notebook_id.is_none());
}
