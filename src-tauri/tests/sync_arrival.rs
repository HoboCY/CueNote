use cuenote_lib::note_format::PlainText;
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::fs;
use std::path::Path;

fn sync_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("sync-arrival-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn late_notebook_metadata_fills_the_original_reference_without_rewriting_the_note() {
    let temp = sync_temp();
    let root = temp.path();
    let source_path = root.join("source-library");
    let mut source = Workspace::new(
        root.join("source-settings"),
        root.join("source-recovery"),
        source_path.clone(),
    );
    let initial = source.select_library(source_path.clone()).unwrap();
    let default_id = initial.library.unwrap().default_notebook_id.unwrap();
    let organized = source.create_notebook("Later notebook").unwrap();
    let notebook_id = organized
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Later notebook")
        .unwrap()
        .notebook_id
        .clone();
    let note = source.new_note_in(Some(&notebook_id)).unwrap();
    let saved = source
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Arrives before its notebook".into(),
                cue: "Keep the original association".into(),
                content: "Content must not be rewritten during metadata discovery.".into(),
                summary: "Two independent files may arrive in either order.".into(),
            },
        )
        .unwrap();
    source
        .update_note_organization(&saved.session.session_id, &notebook_id, true)
        .unwrap();
    let filename = format!("{}.cuenote", note.note_id);
    let original_bytes = fs::read(source_path.join("notes").join(&filename)).unwrap();
    let receiver_path = root.join("receiver-library");
    fs::create_dir_all(receiver_path.join("notes")).unwrap();
    fs::create_dir_all(receiver_path.join("notebooks")).unwrap();
    fs::write(receiver_path.join("notes").join(&filename), &original_bytes).unwrap();
    fs::copy(
        source_path
            .join("notebooks")
            .join(format!("{default_id}.json")),
        receiver_path
            .join("notebooks")
            .join(format!("{default_id}.json")),
    )
    .unwrap();
    let mut receiver = Workspace::new(
        root.join("receiver-settings"),
        root.join("receiver-recovery"),
        receiver_path.clone(),
    );
    let arrived = receiver.select_library(receiver_path.clone()).unwrap();
    let listed = &arrived.library.as_ref().unwrap().notes[0];
    assert_eq!(listed.notebook_id.as_deref(), Some(notebook_id.as_str()));
    assert!(listed.pinned);
    let unknown = receiver.open_note(&note.note_id).unwrap();
    assert!(unknown.read_only_reason.is_some());

    fs::copy(
        source_path
            .join("notebooks")
            .join(format!("{notebook_id}.json")),
        receiver_path
            .join("notebooks")
            .join(format!("{notebook_id}.json")),
    )
    .unwrap();
    let discovered = receiver
        .probe_external_changes(Some(&unknown.session_id))
        .unwrap();
    assert!(discovered
        .library
        .unwrap()
        .notebooks
        .iter()
        .any(|notebook| {
            notebook.notebook_id == notebook_id && notebook.name == "Later notebook"
        }));
    assert_eq!(discovered.session_read_only_reason, None);
    let ready = receiver.snapshot().unwrap();
    let session = ready.session.unwrap();
    assert_eq!(session.session_id, unknown.session_id);
    assert_eq!(session.content, unknown.content);
    assert_eq!(session.notebook_id.as_deref(), Some(notebook_id.as_str()));
    assert!(session.pinned);
    assert_eq!(session.read_only_reason, None);
    let receiver_note = receiver_path.join("notes").join(filename);
    assert_eq!(fs::read(&receiver_note).unwrap(), original_bytes);
    assert_eq!(
        storage::open_note(&receiver_note)
            .unwrap()
            .package
            .plain_text()
            .unwrap()
            .content,
        "Content must not be rewritten during metadata discovery."
    );

    // The note was initially unknown and became editable only after metadata
    // arrived. If that metadata later disappears, preserve the complete draft
    // before refusing to publish it into a library whose ownership is unclear.
    fs::remove_file(
        receiver_path
            .join("notebooks")
            .join(format!("{notebook_id}.json")),
    )
    .unwrap();
    let draft = PlainText {
        title: "Draft while metadata is missing".into(),
        cue: "Keep this edit recoverable".into(),
        content: "Do not discard the candidate after late metadata disappears.".into(),
        summary: "Full candidate preserved".into(),
    };
    let error = receiver
        .save_note_text(&unknown.session_id, draft.clone())
        .unwrap_err();
    assert_eq!(error.kind, "readOnly");
    assert!(error.recovery_path.is_some(), "{error:?}");
    let recovery = receiver.list_recovery_drafts().unwrap();
    assert_eq!(recovery.drafts.len(), 1);
    let recovered = receiver
        .restore_recovery_as_copy(&recovery.drafts[0].recovery_id)
        .unwrap();
    assert_eq!(recovered.session.text, draft);
    assert_ne!(recovered.session.note_id, note.note_id);
    assert_eq!(fs::read(&receiver_note).unwrap(), original_bytes);
}

#[test]
fn note_before_all_metadata_keeps_partial_library_selected_until_metadata_arrives() {
    let temp = sync_temp();
    let root = temp.path();
    let source_path = root.join("source-library");
    let mut source = Workspace::new(
        root.join("source-settings"),
        root.join("source-recovery"),
        source_path.clone(),
    );
    source.select_library(source_path.clone()).unwrap();
    let source_note = source.new_note().unwrap();
    source
        .save_note_text(
            &source_note.session_id,
            PlainText {
                title: "Arrives first".into(),
                cue: String::new(),
                content: "Keep this package untouched while metadata is missing.".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let filename = format!("{}.cuenote", source_note.note_id);
    let original_bytes = fs::read(source_path.join("notes").join(&filename)).unwrap();

    let receiver_path = root.join("receiver-library");
    fs::create_dir_all(receiver_path.join("notes")).unwrap();
    fs::write(receiver_path.join("notes").join(&filename), &original_bytes).unwrap();
    let mut receiver = Workspace::new(
        root.join("receiver-settings"),
        root.join("receiver-recovery"),
        root.join("receiver-default"),
    );
    let selected = receiver.select_library(receiver_path.clone()).unwrap();
    let library = selected.library.unwrap();
    assert_eq!(library.default_notebook_id, None);
    assert_eq!(library.notes.len(), 1);
    assert_eq!(library.notes[0].note_id, source_note.note_id);
    let unknown = receiver.open_note(&source_note.note_id).unwrap();
    assert!(unknown.read_only_reason.is_some());
    assert_eq!(
        receiver.new_note().err().unwrap().kind,
        "libraryMetadataPending"
    );
    assert_eq!(
        receiver.initialize_selected_library().err().unwrap().kind,
        "libraryMetadataPending"
    );
    assert!(!receiver_path.join("notebooks").exists());
    assert_eq!(
        fs::read(receiver_path.join("notes").join(&filename)).unwrap(),
        original_bytes
    );

    fs::create_dir_all(receiver_path.join("notebooks")).unwrap();
    let source_default_id = source
        .snapshot()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .unwrap();
    fs::copy(
        source_path
            .join("notebooks")
            .join(format!("{source_default_id}.json")),
        receiver_path
            .join("notebooks")
            .join(format!("{source_default_id}.json")),
    )
    .unwrap();
    let probe = receiver
        .probe_external_changes(Some(&unknown.session_id))
        .unwrap();
    assert_eq!(probe.session_read_only_reason, None);
    let refreshed = receiver.snapshot().unwrap();
    let session = refreshed.session.unwrap();
    assert_eq!(session.session_id, unknown.session_id);
    assert_eq!(session.read_only_reason, None);
    assert_eq!(session.content, unknown.content);
    assert_eq!(
        fs::read(receiver_path.join("notes").join(filename)).unwrap(),
        original_bytes
    );
}

#[test]
fn existing_empty_library_requires_explicit_initialization() {
    let temp = sync_temp();
    let library_path = temp.path().join("already-synced-library");
    fs::create_dir_all(&library_path).unwrap();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default-library"),
    );

    let selected = workspace.select_library(library_path.clone()).unwrap();
    assert_eq!(selected.library.unwrap().default_notebook_id, None);
    let pending = workspace.snapshot().unwrap();
    assert!(pending.library.is_some());
    assert_eq!(pending.library.unwrap().default_notebook_id, None);
    assert!(!library_path.join("notes").exists());
    assert!(!library_path.join("notebooks").exists());

    let unrelated = library_path.join("unrecognized.sync-item");
    fs::write(&unrelated, b"preserve me").unwrap();
    assert_eq!(
        workspace.initialize_selected_library().err().unwrap().kind,
        "libraryMetadataPending"
    );
    assert!(!library_path.join("notes").exists());
    assert!(!library_path.join("notebooks").exists());
    assert_eq!(fs::read(&unrelated).unwrap(), b"preserve me");
    fs::remove_file(&unrelated).unwrap();

    let initialized = workspace.initialize_selected_library().unwrap();
    let library = initialized.library.unwrap();
    assert!(library.default_notebook_id.is_some());
    assert!(library_path.join("notes").is_dir());
    assert!(library_path.join("notebooks").is_dir());
    assert_eq!(library.notebooks.len(), 1);
}

#[test]
fn unavailable_library_directories_preserve_visible_snapshot_and_cached_session_metadata() {
    let temp = sync_temp();
    let root = temp.path();
    let library_path = root.join("library");
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        library_path.clone(),
    );
    let selected = workspace.select_library(library_path.clone()).unwrap();
    let default_id = selected.library.unwrap().default_notebook_id.unwrap();
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Visible during unavailability".into(),
                cue: String::new(),
                content: "Keep the last known library snapshot available.".into(),
                summary: String::new(),
            },
        )
        .unwrap();

    fs::rename(&library_path, root.join("library-away")).unwrap();
    let unavailable = workspace.snapshot().unwrap();
    let library = unavailable.library.unwrap();
    assert_eq!(
        library.default_notebook_id.as_deref(),
        Some(default_id.as_str())
    );
    assert!(library
        .notes
        .iter()
        .any(|item| item.note_id == note.note_id));
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("保留上次可见列表")));
    let session = unavailable.session.unwrap();
    assert_eq!(session.session_id, saved.session.session_id);
    assert_eq!(session.content, saved.session.content);
    assert_eq!(session.read_only_reason, None);

    let unavailable_probe = workspace
        .probe_external_changes(Some(&saved.session.session_id))
        .unwrap();
    assert_eq!(unavailable_probe.status, "unreadable");
    assert!(unavailable_probe
        .message
        .as_deref()
        .unwrap()
        .contains("目录"));

    // Existing defaults may be used only while their on-disk metadata is available.
    assert!(workspace.new_note().is_err());

    fs::rename(root.join("library-away"), &library_path).unwrap();
    let restored_probe = workspace
        .probe_external_changes(Some(&saved.session.session_id))
        .unwrap();
    assert_eq!(restored_probe.status, "unchanged");
    fs::remove_file(
        library_path
            .join("notes")
            .join(format!("{}.cuenote", note.note_id)),
    )
    .unwrap();
    let deleted_probe = workspace
        .probe_external_changes(Some(&saved.session.session_id))
        .unwrap();
    assert_eq!(deleted_probe.status, "missing");
}

#[test]
fn existing_empty_library_checks_directory_capability_before_initializing() {
    let temp = sync_temp();
    let library_path = temp.path().join("selected-empty-library");
    fs::create_dir_all(library_path.join("notes")).unwrap();
    fs::create_dir_all(library_path.join("notebooks")).unwrap();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default-library"),
    );
    workspace.select_library(library_path.clone()).unwrap();

    // Child directories remain writable: only the selected directory fails its probe.
    #[cfg(windows)]
    let account = {
        let output = std::process::Command::new("whoami.exe").output().unwrap();
        assert!(output.status.success());
        let account = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        let output = std::process::Command::new("icacls.exe")
            .arg(&library_path)
            .args(["/deny", &format!("{account}:(WD)")])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        account
    };
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        let original = fs::metadata(&library_path).unwrap().permissions();
        fs::set_permissions(&library_path, fs::Permissions::from_mode(0o555)).unwrap();
        original
    };

    let result = workspace.initialize_selected_library();

    #[cfg(windows)]
    {
        let output = std::process::Command::new("icacls.exe")
            .arg(&library_path)
            .args(["/remove:d", &account])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    #[cfg(unix)]
    fs::set_permissions(&library_path, permissions).unwrap();

    let error = result
        .err()
        .expect("initialization must reject a failed directory probe");
    assert!(error.message.contains("目录创建探针"), "{}", error.message);
    assert_eq!(
        fs::read_dir(library_path.join("notebooks"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(fs::read_dir(&library_path).unwrap().count(), 2);
    assert!(workspace
        .initialize_selected_library()
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .is_some());
}

#[test]
fn same_named_notebooks_from_independent_libraries_remain_distinct_and_warn_until_renamed() {
    let temp = sync_temp();
    let root = temp.path();
    let first_path = root.join("first-library");
    let mut first = Workspace::new(
        root.join("first-settings"),
        root.join("first-recovery"),
        first_path.clone(),
    );
    first.select_library(first_path.clone()).unwrap();
    let first_notebooks = first.create_notebook("Shared name").unwrap();
    let first_id = first_notebooks
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Shared name")
        .unwrap()
        .notebook_id
        .clone();
    let first_note = first.new_note_in(Some(&first_id)).unwrap();
    first
        .save_note_text(
            &first_note.session_id,
            PlainText {
                title: "First notebook note".into(),
                cue: String::new(),
                content: "Association one".into(),
                summary: String::new(),
            },
        )
        .unwrap();

    let second_path = root.join("second-library");
    let mut second = Workspace::new(
        root.join("second-settings"),
        root.join("second-recovery"),
        second_path.clone(),
    );
    second.select_library(second_path.clone()).unwrap();
    let second_notebooks = second.create_notebook("Shared name").unwrap();
    let second_id = second_notebooks
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Shared name")
        .unwrap()
        .notebook_id
        .clone();
    assert_ne!(first_id, second_id);
    let second_note = second.new_note_in(Some(&second_id)).unwrap();
    second
        .save_note_text(
            &second_note.session_id,
            PlainText {
                title: "Second notebook note".into(),
                cue: String::new(),
                content: "Association two".into(),
                summary: String::new(),
            },
        )
        .unwrap();

    let first_note_path = first_path
        .join("notes")
        .join(format!("{}.cuenote", first_note.note_id));
    let second_note_path = second_path
        .join("notes")
        .join(format!("{}.cuenote", second_note.note_id));
    let first_note_bytes = fs::read(&first_note_path).unwrap();
    let second_note_bytes = fs::read(&second_note_path).unwrap();
    fs::copy(
        &second_note_path,
        &first_path
            .join("notes")
            .join(format!("{}.cuenote", second_note.note_id)),
    )
    .unwrap();
    fs::copy(
        second_path
            .join("notebooks")
            .join(format!("{second_id}.json")),
        first_path
            .join("notebooks")
            .join(format!("{second_id}.json")),
    )
    .unwrap();

    let active_session_id = first.snapshot().unwrap().session.unwrap().session_id;
    let probe = first
        .probe_external_changes(Some(&active_session_id))
        .unwrap();
    let library = probe.library.unwrap();
    let same_name_ids = library
        .notebooks
        .iter()
        .filter(|notebook| notebook.name == "Shared name")
        .map(|notebook| notebook.notebook_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        same_name_ids,
        [first_id.as_str(), second_id.as_str()]
            .into_iter()
            .collect()
    );
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("Shared name")
            && warning.contains(&first_id)
            && warning.contains(&second_id)));
    assert!(library
        .notes
        .iter()
        .any(|note| note.note_id == first_note.note_id
            && note.notebook_id.as_deref() == Some(first_id.as_str())));
    assert!(library
        .notes
        .iter()
        .any(|note| note.note_id == second_note.note_id
            && note.notebook_id.as_deref() == Some(second_id.as_str())));
    assert_eq!(fs::read(&first_note_path).unwrap(), first_note_bytes);
    assert_eq!(
        fs::read(
            first_path
                .join("notes")
                .join(format!("{}.cuenote", second_note.note_id))
        )
        .unwrap(),
        second_note_bytes
    );

    first
        .rename_notebook(&second_id, "Renamed arrival")
        .unwrap();
    let renamed = first.snapshot().unwrap().library.unwrap();
    assert!(!renamed
        .warnings
        .iter()
        .any(|warning| warning.contains("Shared name")
            && warning.contains(&first_id)
            && warning.contains(&second_id)));
    assert!(renamed
        .notes
        .iter()
        .any(|note| note.note_id == first_note.note_id
            && note.notebook_id.as_deref() == Some(first_id.as_str())));
    assert!(renamed
        .notes
        .iter()
        .any(|note| note.note_id == second_note.note_id
            && note.notebook_id.as_deref() == Some(second_id.as_str())));
}

#[test]
fn metadata_is_portable_and_bad_sibling_files_are_preserved_without_breaking_valid_items() {
    let temp = sync_temp();
    let root = temp.path();
    let library_path = root.join("shared-library");
    let mut first = Workspace::new(
        root.join("first-settings"),
        root.join("first-recovery"),
        library_path.clone(),
    );
    first.select_library(library_path.clone()).unwrap();
    let portable = first.create_notebook("Portable").unwrap();
    let portable_id = portable
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Portable")
        .unwrap()
        .notebook_id
        .clone();
    let pinned = first.new_note_in(Some(&portable_id)).unwrap();
    let pinned = first
        .save_note_text(
            &pinned.session_id,
            PlainText {
                title: "Pinned note".into(),
                cue: String::new(),
                content: "Portable pin state".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    first
        .update_note_organization(&pinned.session.session_id, &portable_id, true)
        .unwrap();

    let trashed = first.new_note().unwrap();
    first
        .save_note_text(
            &trashed.session_id,
            PlainText {
                title: "Trashed note".into(),
                cue: String::new(),
                content: "Portable trash state".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let trashed = first.open_note(&trashed.note_id).unwrap();
    first.trash_note(&trashed.session_id).unwrap();
    first
        .set_view_preferences(true, Some(portable_id.clone()))
        .unwrap();

    let bad_bytes = b"{not valid notebook metadata".to_vec();
    let bad_path = library_path.join("notebooks").join("invalid.json");
    fs::write(&bad_path, &bad_bytes).unwrap();
    let conflicting_bytes = fs::read(
        library_path
            .join("notebooks")
            .join(format!("{portable_id}.json")),
    )
    .unwrap();
    let conflict_path = library_path.join("notebooks").join("conflict.json");
    fs::write(&conflict_path, &conflicting_bytes).unwrap();

    let mut second = Workspace::new(
        root.join("second-settings"),
        root.join("second-recovery"),
        root.join("second-default"),
    );
    let portable_view = second.select_library(library_path.clone()).unwrap();
    let portable_view = portable_view.library.unwrap();
    assert!(portable_view
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == portable_id && notebook.name == "Portable"));
    assert!(portable_view
        .notes
        .iter()
        .any(|note| note.note_id == pinned.session.note_id && note.pinned));
    assert!(portable_view
        .trash_notes
        .iter()
        .any(|note| note.note_id == trashed.note_id));
    assert!(portable_view
        .warnings
        .iter()
        .any(|warning| warning.contains("invalid.json")));
    assert!(portable_view
        .warnings
        .iter()
        .any(|warning| warning.contains("conflict.json")));
    assert!(!second.snapshot().unwrap().preferences.sidebar_collapsed);
    assert_eq!(
        second.snapshot().unwrap().preferences.selected_notebook_id,
        None
    );

    let renamed = second
        .rename_notebook(&portable_id, "Renamed elsewhere")
        .unwrap();
    assert!(renamed.notebooks.iter().any(
        |notebook| notebook.notebook_id == portable_id && notebook.name == "Renamed elsewhere"
    ));
    let stale_write = first
        .rename_notebook(&portable_id, "Stale overwrite")
        .err()
        .unwrap();
    assert_eq!(stale_write.kind, "conflict");

    let first_refreshed = first.snapshot().unwrap();
    let refreshed_library = first_refreshed.library.as_ref().unwrap();
    assert!(refreshed_library.notebooks.iter().any(
        |notebook| notebook.notebook_id == portable_id && notebook.name == "Renamed elsewhere"
    ));
    let after_refresh = first
        .rename_notebook(&portable_id, "Renamed after refresh")
        .unwrap();
    assert!(after_refresh.notebooks.iter().any(|notebook| {
        notebook.notebook_id == portable_id && notebook.name == "Renamed after refresh"
    }));

    let empty = second.create_notebook("Empty").unwrap();
    let empty_id = empty
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Empty")
        .unwrap()
        .notebook_id
        .clone();
    let first_refreshed = first.snapshot().unwrap();
    let refreshed_library = first_refreshed.library.unwrap();
    assert!(refreshed_library
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == portable_id
            && notebook.name == "Renamed after refresh"));
    assert!(refreshed_library
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == empty_id && notebook.name == "Empty"));
    assert!(first_refreshed.preferences.sidebar_collapsed);
    assert_eq!(
        first_refreshed.preferences.selected_notebook_id,
        Some(portable_id)
    );
    assert_eq!(fs::read(&bad_path).unwrap(), bad_bytes);
    assert_eq!(fs::read(&conflict_path).unwrap(), conflicting_bytes);
}
