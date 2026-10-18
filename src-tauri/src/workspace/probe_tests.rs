use super::*;
use std::sync::Mutex;

fn defer_at(
    saves: &ForegroundSaves,
    checkpoint: ProbeCheckpoint,
) -> (ProbeAttempt, Arc<Mutex<Option<probe_control::SaveIntent>>>) {
    let intent = Arc::new(Mutex::new(None));
    let held = intent.clone();
    let incoming = saves.clone();
    (
        saves.probe().with_hook(move |at| {
            if at == checkpoint {
                *held.lock().unwrap() = Some(incoming.begin_save());
            }
        }),
        intent,
    )
}

fn write_external_note(workspace: &Workspace, title: &str) -> PathBuf {
    let library = workspace.library.as_ref().unwrap();
    let package = NotePackage::new_plain(
        library.default_notebook_id.clone().unwrap(),
        PlainText {
            title: title.into(),
            cue: String::new(),
            content: "Native body".into(),
            summary: String::new(),
        },
    );
    let path = library
        .path
        .join("notes")
        .join(format!("{}.cuenote", package.document.note_id));
    crate::note_format::write_package(&mut File::create(&path).unwrap(), &package).unwrap();
    path
}

fn fixture() -> (tempfile::TempDir, Workspace) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("probe-yield-")
        .tempdir_in(parent)
        .unwrap();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("library"),
    );
    workspace.use_default_library().unwrap();
    (temp, workspace)
}

#[test]
fn pending_save_defers_background_refresh_and_retry_completes() {
    let (_temp, mut workspace) = fixture();
    let before = workspace.library.as_ref().unwrap().view.clone();
    let saves = ForegroundSaves::default();
    let saving = saves.begin_save();
    let probe = workspace
        .probe_external_changes_interruptible(None, &saves.probe())
        .unwrap();
    assert_eq!(probe.status, "deferred");
    assert!(probe.library.is_none());
    assert_eq!(workspace.library.as_ref().unwrap().view, before);
    drop(saving);
    let retry = workspace
        .probe_external_changes_interruptible(None, &saves.probe())
        .unwrap();
    assert_eq!(retry.status, "unchanged");
    assert_eq!(retry.library, Some(before));
}

fn cache_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(root.join("settings/search-cache-v1"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn committed_projection_keeps_dirty_search_building_until_full_probe() {
    let (temp, mut workspace) = fixture();
    let session = workspace.new_note().unwrap();
    let mut content = session.content;
    content.title = "Previous title".into();
    workspace
        .save_note_content(&session.session_id, content.clone())
        .unwrap();
    let cache_before = cache_files(temp.path());
    write_external_note(&workspace, "Arrived without a frontend notification");
    workspace.search_generation.fetch_add(1, Ordering::AcqRel);
    let progress = Arc::new(Mutex::new(Vec::new()));
    let sink = progress.clone();
    workspace.attach_search_progress(Arc::new(move |event| {
        sink.lock().unwrap().push(event.phase)
    }));
    content.title = "Committed title".into();
    let saved = workspace
        .save_note_content(&session.session_id, content)
        .unwrap();
    assert_eq!(saved.library.notes.len(), 1);
    assert_eq!(saved.library.notes[0].title, "Committed title");
    let search = workspace.search_notes("Committed", None, 0).unwrap();
    assert_eq!(search.status, SearchStatus::Building);
    assert!(search.hits.is_empty());
    assert!(progress.lock().unwrap().is_empty());
    assert_eq!(cache_files(temp.path()), cache_before);
    let probe = workspace
        .probe_external_changes(Some(&session.session_id))
        .unwrap();
    assert_eq!(probe.library.unwrap().notes.len(), 2);
    assert_eq!(
        workspace.search_notes("Committed", None, 0).unwrap().total,
        1
    );
    assert_eq!(workspace.search_notes("Arrived", None, 0).unwrap().total, 1);
    assert_eq!(progress.lock().unwrap().last(), Some(&SearchStatus::Ready));
}

#[test]
fn metadata_refreshed_before_saving_still_requires_a_complete_projection_refresh() {
    let (_temp, mut workspace) = fixture();
    let session = workspace.new_note().unwrap();
    workspace
        .save_note_content(&session.session_id, session.content.clone())
        .unwrap();
    let library = workspace.library.as_ref().unwrap();
    let notebook_id = library.default_notebook_id.as_ref().unwrap();
    let path = library
        .path
        .join("notebooks")
        .join(format!("{notebook_id}.json"));
    let mut metadata: NotebookMetadata = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    metadata.name = "Metadata seen while opening".into();
    fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    // Opening refreshes metadata but does not scan all note files.
    let reopened = workspace.open_note(&session.note_id).unwrap();
    write_external_note(&workspace, "External arrival before save");
    let mut content = reopened.content;
    content.title = "Saved after reopening".into();
    let receipt = workspace
        .save_note_content(&reopened.session_id, content)
        .unwrap();
    assert_eq!(
        receipt.library.notes.len(),
        2,
        "metadata must match the complete index, not only the previous metadata read"
    );
    assert_eq!(
        receipt.library.default_notebook_name,
        "Metadata seen while opening"
    );
}

#[test]
fn committed_projection_falls_back_when_metadata_changes_or_index_is_incomplete() {
    for metadata_changes in [true, false] {
        let (_temp, mut workspace) = fixture();
        let session = workspace.new_note().unwrap();
        let mut content = session.content;
        content.title = "Original title".into();
        workspace
            .save_note_content(&session.session_id, content.clone())
            .unwrap();
        let library = workspace.library.as_ref().unwrap();
        if metadata_changes {
            let notebook_id = library.default_notebook_id.as_ref().unwrap();
            let path = library
                .path
                .join("notebooks")
                .join(format!("{notebook_id}.json"));
            let mut metadata: NotebookMetadata =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            metadata.name = "Changed notebook name".into();
            fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        } else {
            let path = library
                .path
                .join("notes")
                .join(format!("{}.cuenote", session.note_id));
            fs::copy(path, library.path.join("notes/duplicate.cuenote")).unwrap();
            workspace
                .probe_external_changes(Some(&session.session_id))
                .unwrap();
            assert!(
                workspace
                    .search_notes("Original", None, 0)
                    .unwrap()
                    .incomplete
            );
        }
        write_external_note(&workspace, "Another externally arrived note");
        content.title = "Saved title".into();
        let receipt = workspace
            .save_note_content(&session.session_id, content)
            .unwrap();
        assert_eq!(
            receipt.library.notes.len(),
            2,
            "fallback scans include other new files"
        );
        assert_eq!(
            workspace
                .search_notes("externally arrived", None, 0)
                .unwrap()
                .total,
            1
        );
        if metadata_changes {
            assert_eq!(
                receipt.library.default_notebook_name,
                "Changed notebook name"
            );
        } else {
            assert!(receipt
                .library
                .warnings
                .iter()
                .any(|warning| warning.contains("重复笔记身份")));
        }
    }
}

#[test]
fn save_arriving_before_probe_commit_keeps_the_previous_view_and_cache() {
    for checkpoint in [
        ProbeCheckpoint::MetadataPrepared,
        ProbeCheckpoint::NoteRetentionComplete,
        ProbeCheckpoint::BeforeCommit,
    ] {
        deferred_refresh_keeps_the_previous_view_and_cache(checkpoint);
    }
}

fn deferred_refresh_keeps_the_previous_view_and_cache(checkpoint: ProbeCheckpoint) {
    use std::sync::Mutex;
    let (temp, mut workspace) = fixture();
    let session = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &session.session_id,
            PlainText {
                title: "Original".into(),
                cue: String::new(),
                content: "Old body".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let library = workspace.library.as_ref().unwrap();
    let before = library.view.clone();
    let baseline = library.notebook_metadata_baselines.clone();
    let old_projection = library.search.index.last_projection();
    let cache_before = cache_files(temp.path());
    let notebook_id = library.default_notebook_id.as_ref().unwrap();
    let notebook_path = library
        .path
        .join("notebooks")
        .join(format!("{notebook_id}.json"));
    let mut metadata: NotebookMetadata =
        serde_json::from_slice(&fs::read(&notebook_path).unwrap()).unwrap();
    metadata.name = "External name".into();
    fs::write(notebook_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let path = library
        .path
        .join("notes")
        .join(format!("{}.cuenote", session.note_id));
    let mut package = storage::open_note(&path).unwrap().package;
    package.document.title = "External title".into();
    crate::note_format::write_package(&mut File::create(&path).unwrap(), &package).unwrap();
    let observed = storage::open_note(&path).unwrap();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_sink = progress.clone();
    workspace.attach_search_progress(Arc::new(move |event| {
        progress_sink.lock().unwrap().push(event.phase)
    }));
    let saves = ForegroundSaves::default();
    let intent = Arc::new(Mutex::new(None));
    let held = intent.clone();
    let incoming = saves.clone();
    let attempt = saves.probe().with_hook(move |at| {
        if at == checkpoint {
            *held.lock().unwrap() = Some(incoming.begin_save());
        }
    });
    let probe = workspace
        .probe_external_changes_interruptible(Some(&session.session_id), &attempt)
        .unwrap();
    assert_eq!(probe.status, "deferred");
    assert_eq!(workspace.library.as_ref().unwrap().view, before);
    assert_eq!(
        workspace
            .library
            .as_ref()
            .unwrap()
            .notebook_metadata_baselines,
        baseline
    );
    assert_eq!(cache_files(temp.path()), cache_before);
    assert_eq!(
        workspace
            .library
            .as_ref()
            .unwrap()
            .search
            .index
            .last_projection(),
        old_projection
    );
    assert!(progress
        .lock()
        .unwrap()
        .iter()
        .all(|phase| *phase == SearchStatus::Building));
    if checkpoint != ProbeCheckpoint::MetadataPrepared {
        let versions = workspace.list_preserved_versions().unwrap();
        let retained = versions
            .versions
            .iter()
            .find(|version| version.source_revision == observed.revision)
            .unwrap();
        assert_eq!(
            storage::versions::open_version(&workspace.recovery_root, &retained.version_id)
                .unwrap()
                .opened
                .package
                .content(),
            observed.package.content()
        );
    }
    intent.lock().unwrap().take();
    let retry = workspace
        .probe_external_changes_interruptible(Some(&session.session_id), &saves.probe())
        .unwrap();
    assert_eq!(retry.status, "changed");
    let view = retry.library.unwrap();
    assert_eq!(view.default_notebook_name, "External name");
    assert_eq!(view.notes[0].title, "External title");
    assert_eq!(progress.lock().unwrap().last(), Some(&SearchStatus::Ready));
}

#[test]
fn obsolete_generation_does_not_publish_cache_or_ready() {
    let (temp, mut workspace) = fixture();
    write_external_note(&workspace, "External source");
    let cache_before = cache_files(temp.path());
    let generation = workspace.search_generation.clone();
    let old_generation = generation.load(Ordering::Acquire);
    let progress = Arc::new(Mutex::new(Vec::new()));
    let sink = progress.clone();
    workspace.attach_search_progress(Arc::new(move |event| {
        sink.lock().unwrap().push(event.phase)
    }));
    let attempt = ForegroundSaves::default().probe().with_hook(move |at| {
        if at == ProbeCheckpoint::BeforeCommit {
            generation.fetch_add(1, Ordering::AcqRel);
        }
    });
    let probe = workspace
        .probe_external_changes_interruptible(None, &attempt)
        .unwrap();
    assert_eq!(probe.library.unwrap().notes.len(), 1);
    assert_eq!(
        workspace.library.as_ref().unwrap().search.generation,
        old_generation
    );
    assert_eq!(cache_files(temp.path()), cache_before);
    assert!(progress
        .lock()
        .unwrap()
        .iter()
        .all(|phase| *phase == SearchStatus::Building));
    workspace
        .probe_external_changes_interruptible(None, &ForegroundSaves::default().probe())
        .unwrap();
    assert_eq!(progress.lock().unwrap().last(), Some(&SearchStatus::Ready));
    assert_ne!(cache_files(temp.path()), cache_before);
}

#[test]
fn unavailable_notes_keep_the_previous_list_and_only_commit_a_completed_probe() {
    for replace_with_file in [false, true] {
        let (temp, mut workspace) = fixture();
        write_external_note(&workspace, "Known note");
        workspace.probe_external_changes(None).unwrap();
        let before = workspace.library.as_ref().unwrap().view.clone();
        let notes = temp.path().join("library/notes");
        fs::rename(&notes, temp.path().join("library/notes-away")).unwrap();
        if replace_with_file {
            fs::write(&notes, b"directory is unavailable").unwrap();
        }
        let progress = Arc::new(Mutex::new(Vec::new()));
        let sink = progress.clone();
        workspace.attach_search_progress(Arc::new(move |event| {
            sink.lock().unwrap().push(event.phase)
        }));
        let saves = ForegroundSaves::default();
        let (attempt, intent) = defer_at(&saves, ProbeCheckpoint::BeforeCommit);
        assert_eq!(
            workspace
                .probe_external_changes_interruptible(None, &attempt)
                .unwrap()
                .status,
            "deferred"
        );
        assert_eq!(workspace.library.as_ref().unwrap().view, before);
        assert!(workspace.library.as_ref().unwrap().search.available);
        assert!(progress.lock().unwrap().is_empty());
        intent.lock().unwrap().take();
        let completed = workspace
            .probe_external_changes_interruptible(None, &saves.probe())
            .unwrap();
        assert_eq!(completed.library.unwrap().notes, before.notes);
        assert!(!workspace.library.as_ref().unwrap().search.available);
        assert_eq!(&*progress.lock().unwrap(), &[SearchStatus::Unavailable]);
    }
}

#[test]
fn preservation_warning_and_current_note_error_are_not_hidden_by_deferred() {
    for active_session in [false, true] {
        let (temp, mut workspace) = fixture();
        let session = if active_session {
            Some(workspace.new_note().unwrap())
        } else {
            None
        };
        let path = if let Some(active) = workspace.session.as_ref() {
            crate::note_format::write_package(
                &mut File::create(&active.path).unwrap(),
                &active.package,
            )
            .unwrap();
            active.path.clone()
        } else {
            write_external_note(&workspace, "Cannot retain this source")
        };
        let original = fs::read(&path).unwrap();
        let before = workspace.library.as_ref().unwrap().view.clone();
        let cache_before = cache_files(temp.path());
        let versions = temp.path().join("recovery/versions");
        if versions.exists() {
            fs::rename(&versions, temp.path().join("recovery/versions-away")).unwrap();
        }
        fs::write(&versions, b"blocked version directory").unwrap();
        let saves = ForegroundSaves::default();
        let (attempt, _intent) = defer_at(&saves, ProbeCheckpoint::NoteRetentionComplete);
        let result = workspace.probe_external_changes_interruptible(
            session.as_ref().map(|s| s.session_id.as_str()),
            &attempt,
        );
        if active_session {
            let error = result.unwrap_err();
            assert_eq!(error.kind, "unsafePath");
            assert_eq!(workspace.library.as_ref().unwrap().view, before);
            assert_eq!(cache_files(temp.path()), cache_before);
        } else {
            let probe = result.unwrap();
            assert_eq!(probe.status, "unchanged");
            assert!(probe
                .library
                .unwrap()
                .warnings
                .iter()
                .any(|warning| warning.contains("无法保留笔记")));
            assert!(workspace.library.as_ref().unwrap().search.incomplete);
        }
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn no_editor_new_note_and_preserved_preview_all_commit_after_retry() {
    for session_kind in ["none", "new", "preview"] {
        let (_temp, mut workspace) = fixture();
        let session_id = match session_kind {
            "new" => Some(workspace.new_note().unwrap().session_id),
            "preview" => {
                write_external_note(&workspace, "Preserved source");
                workspace.probe_external_changes(None).unwrap();
                let version_id = workspace.list_preserved_versions().unwrap().versions[0]
                    .version_id
                    .clone();
                Some(
                    workspace
                        .open_preserved_version(&version_id)
                        .unwrap()
                        .session_id,
                )
            }
            _ => None,
        };
        let note = write_external_note(&workspace, "Arrived while editing");
        let note_id = storage::open_note(&note).unwrap().package.document.note_id;
        let before = workspace.library.as_ref().unwrap().view.clone();
        let saves = ForegroundSaves::default();
        let (attempt, intent) = defer_at(&saves, ProbeCheckpoint::BeforeCommit);
        assert_eq!(
            workspace
                .probe_external_changes_interruptible(session_id.as_deref(), &attempt)
                .unwrap()
                .status,
            "deferred",
            "{session_kind}"
        );
        assert_eq!(workspace.library.as_ref().unwrap().view, before);
        intent.lock().unwrap().take();
        let retry = workspace
            .probe_external_changes_interruptible(session_id.as_deref(), &saves.probe())
            .unwrap();
        assert_eq!(retry.status, "unchanged");
        assert!(retry
            .library
            .unwrap()
            .notes
            .iter()
            .any(|note| note.note_id == note_id));
    }
}

#[test]
fn late_notebook_metadata_does_not_make_a_deferred_editor_writable() {
    let (_temp, mut workspace) = fixture();
    let path = write_external_note(&workspace, "Unknown notebook");
    let mut package = storage::open_note(&path).unwrap().package;
    let library = workspace.library.as_ref().unwrap();
    let mut metadata = library.notebooks.values().next().unwrap().clone();
    metadata.id = Uuid::new_v4().to_string();
    metadata.default = false;
    metadata.name = "Arrives later".into();
    // Native organization is an independent synchronized fact.
    package.document.organization.notebook_id = Some(metadata.id.clone());
    crate::note_format::write_package(&mut File::create(&path).unwrap(), &package).unwrap();
    let metadata_path = library
        .path
        .join("notebooks")
        .join(format!("{}.json", metadata.id));
    let session = workspace.open_note(&package.document.note_id).unwrap();
    assert!(session.read_only_reason.is_some());
    assert!(!workspace.session.as_ref().unwrap().was_writable);
    fs::write(metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let original = fs::read(&path).unwrap();
    let saves = ForegroundSaves::default();
    let (attempt, intent) = defer_at(&saves, ProbeCheckpoint::MetadataPrepared);
    assert_eq!(
        workspace
            .probe_external_changes_interruptible(Some(&session.session_id), &attempt)
            .unwrap()
            .status,
        "deferred"
    );
    assert!(!workspace.session.as_ref().unwrap().was_writable);
    assert!(workspace
        .editor_read_only_reason(workspace.session.as_ref().unwrap())
        .is_some());
    assert!(!workspace
        .library
        .as_ref()
        .unwrap()
        .notebook_metadata_baselines
        .contains_key(&metadata.id));
    intent.lock().unwrap().take();
    let retry = workspace
        .probe_external_changes_interruptible(Some(&session.session_id), &saves.probe())
        .unwrap();
    assert!(retry.session_read_only_reason.is_none());
    assert!(workspace.session.as_ref().unwrap().was_writable);
    assert!(workspace
        .library
        .as_ref()
        .unwrap()
        .notebook_metadata_baselines
        .contains_key(&metadata.id));
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn duplicate_identity_protection_error_keeps_its_warning_instead_of_deferring() {
    let (_temp, mut workspace) = fixture();
    let path = write_external_note(&workspace, "First version");
    let opened = storage::open_note(&path).unwrap();
    storage::versions::preserve_conflict_version(
        &workspace.recovery_root,
        &opened,
        &display_path(&path),
        "Existing protected version",
    )
    .unwrap();
    let id = workspace.list_preserved_versions().unwrap().versions[0]
        .version_id
        .clone();
    let version = workspace.recovery_root.join("versions").join(id);
    fs::rename(&version, version.with_extension("held")).unwrap();
    fs::write(&version, b"block conflict publication").unwrap();
    let mut second = opened.package.clone();
    second.document.title = "Second version".into();
    let duplicate = path.parent().unwrap().join("zz-conflict.cuenote");
    crate::note_format::write_package(&mut File::create(&duplicate).unwrap(), &second).unwrap();
    let source_bytes = (fs::read(&path).unwrap(), fs::read(&duplicate).unwrap());
    let saves = ForegroundSaves::default();
    let (attempt, _intent) = defer_at(&saves, ProbeCheckpoint::BeforeCommit);
    let probe = workspace
        .probe_external_changes_interruptible(None, &attempt)
        .unwrap();
    assert_eq!(probe.status, "unchanged");
    assert!(probe
        .library
        .unwrap()
        .warnings
        .iter()
        .any(|warning| warning.contains("无法刷新笔记列表")));
    assert!(!workspace.library.as_ref().unwrap().search.available);
    assert_eq!(
        (fs::read(path).unwrap(), fs::read(duplicate).unwrap()),
        source_bytes
    );
}

#[test]
fn external_editor_probe_holds_library_access_until_commit_or_defer() {
    use std::sync::atomic::AtomicUsize;
    for defer in [false, true] {
        let (temp, mut workspace) = fixture();
        let source = write_external_note(&workspace, "Library source");
        let documents = temp.path().join("documents");
        fs::create_dir_all(&documents).unwrap();
        let external = documents.join("outside.cuenote");
        fs::copy(source, &external).unwrap();
        let session = workspace.open_external_note(external).unwrap();
        let before = workspace.library.as_ref().unwrap().view.clone();
        let cache_before = cache_files(temp.path());
        let library = workspace.library_path().unwrap();
        let recovery = workspace.recovery_root.clone();
        let assert_busy = move || match storage::library_access::lock_exclusive(&recovery, &library)
        {
            Err(error) => assert_eq!(error.kind, "libraryBusy"),
            Ok(_) => panic!("whole-library recovery acquired access during an uncommitted probe"),
        };
        let checkpoints = Arc::new(AtomicUsize::new(0));
        let terminal_checks = checkpoints.clone();
        let terminal_busy = assert_busy.clone();
        workspace.attach_search_progress(Arc::new(move |event| {
            if event.phase != SearchStatus::Building {
                terminal_busy();
                terminal_checks.fetch_add(1, Ordering::AcqRel);
            }
        }));
        let saves = ForegroundSaves::default();
        let incoming = saves.clone();
        let intent = Arc::new(Mutex::new(None));
        let held = intent.clone();
        let attempt = saves.probe().with_hook(move |at| {
            if at == ProbeCheckpoint::BeforeCommit {
                assert_busy();
                if defer {
                    *held.lock().unwrap() = Some(incoming.begin_save());
                }
            }
        });
        let probe = workspace
            .probe_external_changes_interruptible(Some(&session.session_id), &attempt)
            .unwrap();
        assert_eq!(probe.status, if defer { "deferred" } else { "unchanged" });
        assert_eq!(
            checkpoints.load(Ordering::Acquire),
            if defer { 0 } else { 1 }
        );
        if defer {
            assert_eq!(workspace.library.as_ref().unwrap().view, before);
            assert_eq!(cache_files(temp.path()), cache_before);
        }
        assert!(storage::library_access::lock_exclusive(
            &workspace.recovery_root,
            &workspace.library_path().unwrap()
        )
        .is_ok());
    }
}
