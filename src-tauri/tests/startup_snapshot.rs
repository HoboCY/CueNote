use cuenote_lib::note_format::{write_package, NotePackage, PlainText};
use cuenote_lib::storage;
use cuenote_lib::workspace::{
    BackupControl, BackupKind, BackupPhase, ScanState, SearchStatus, Workspace,
};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod support;

fn workspace(root: &Path) -> Workspace {
    Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("Library"),
    )
}

fn plain(title: &str, content: &str) -> PlainText {
    PlainText {
        title: title.into(),
        cue: String::new(),
        content: content.into(),
        summary: String::new(),
    }
}

struct WarmLibrary {
    temp: tempfile::TempDir,
    library: PathBuf,
    remembered: String,
    sibling: String,
}

/// Builds a library with two saved notes and a completed warm scan. The note
/// created last is the remembered current note in the persisted preference.
fn warm_two_note_library() -> WarmLibrary {
    let temp = support::tempdir("startup-snapshot-");
    let library = temp.path().join("Library");
    let mut app = workspace(temp.path());
    app.select_library(library.clone()).unwrap();
    let sibling = app.new_note().unwrap();
    app.save_note_text(&sibling.session_id, plain("Sibling note", "sibling-token"))
        .unwrap();
    let remembered = app.new_note().unwrap();
    app.save_note_text(
        &remembered.session_id,
        plain("Remembered note", "remembered-token"),
    )
    .unwrap();
    // A complete scan persists durable witnesses and the disposable warm cache.
    app.snapshot().unwrap();
    drop(app);
    WarmLibrary {
        temp,
        library,
        remembered: remembered.note_id,
        sibling: sibling.note_id,
    }
}

fn note_path(library: &Path, note_id: &str) -> PathBuf {
    library.join("notes").join(format!("{note_id}.cuenote"))
}

fn rewrite_note(path: &Path, text: PlainText) -> String {
    let source = storage::open_note(path).unwrap();
    let mut package = source.package;
    package.replace_plain_text(text).unwrap();
    write_package(&mut File::create(path).unwrap(), &package).unwrap();
    storage::open_note(path).unwrap().revision
}

fn cache_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(root.join("settings/search-cache-v1"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!files.is_empty(), "fixture must have a warm scan cache");
    files
}

#[test]
fn startup_snapshot_reports_pending_with_only_the_freshly_opened_current_note() {
    let fixture = warm_two_note_library();
    let cached_before = cache_files(fixture.temp.path());
    let sibling_path = note_path(&fixture.library, &fixture.sibling);
    let changed_revision = rewrite_note(
        &sibling_path,
        plain("Sibling changed", "sibling-token-changed"),
    );

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    let session_id = view.session.as_ref().unwrap().session_id.clone();
    let library = view.library.as_ref().unwrap();
    assert_eq!(library.scan_state, ScanState::Pending);
    assert_eq!(library.notes.len(), 1);
    assert_eq!(library.notes[0].note_id, fixture.remembered);
    assert_eq!(view.session.as_ref().unwrap().note_id, fixture.remembered);
    assert_eq!(cache_files(fixture.temp.path()), cached_before);

    // Pending never publishes search results, even for the freshly opened note.
    let pending_search = restarted
        .search_notes("sibling-token-changed", None, 0)
        .unwrap();
    assert_eq!(pending_search.status, SearchStatus::Building);
    assert_eq!(pending_search.total, 0);
    assert!(pending_search.incomplete);
    assert_eq!(
        restarted
            .search_notes("remembered-token", None, 0)
            .unwrap()
            .status,
        SearchStatus::Building
    );

    // The unrelated changed note is not claimed as observed before the probe.
    let preserved = restarted.list_preserved_versions().unwrap().versions;
    assert!(!preserved.iter().any(|version| {
        version.note_id == fixture.sibling
            && version.source_revision == changed_revision
            && !version.conflict
    }));

    let probed = restarted.probe_external_changes(Some(&session_id)).unwrap();
    let complete = probed.library.unwrap();
    assert_eq!(complete.scan_state, ScanState::Complete);
    assert!(complete
        .notes
        .iter()
        .any(|note| note.note_id == fixture.sibling));
    assert!(complete
        .notes
        .iter()
        .any(|note| note.note_id == fixture.remembered));
    let indexed = restarted
        .search_notes("sibling-token-changed", None, 0)
        .unwrap();
    assert_eq!(indexed.status, SearchStatus::Ready);
    assert_eq!(indexed.total, 1);
    let preserved = restarted.list_preserved_versions().unwrap().versions;
    assert!(preserved.iter().any(|version| {
        version.note_id == fixture.sibling
            && version.source_revision == changed_revision
            && !version.conflict
    }));
}

#[test]
fn a_second_startup_snapshot_completes_the_scan_without_a_file_notification() {
    let fixture = warm_two_note_library();
    let mut restarted = workspace(fixture.temp.path());
    let first = restarted.startup_snapshot().unwrap();
    assert_eq!(first.library.unwrap().scan_state, ScanState::Pending);
    let session_id = first.session.unwrap().session_id;
    let next = restarted.startup_snapshot().unwrap();
    let complete = next.library.unwrap();
    assert_eq!(complete.scan_state, ScanState::Complete);
    assert_eq!(complete.notes.len(), 2);
    assert_eq!(next.session.unwrap().session_id, session_id);
    let search = restarted.search_notes("sibling-token", None, 0).unwrap();
    assert_eq!(search.status, SearchStatus::Ready);
    assert_eq!(search.total, 1);
}

#[test]
fn startup_snapshot_falls_back_to_a_complete_scan_when_the_remembered_note_is_missing() {
    let fixture = warm_two_note_library();
    fs::remove_file(note_path(&fixture.library, &fixture.remembered)).unwrap();

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert!(view.session.is_none());
    let library = view.library.unwrap();
    assert_eq!(library.scan_state, ScanState::Complete);
    assert!(!library
        .notes
        .iter()
        .any(|note| note.note_id == fixture.remembered));
    assert!(library
        .notes
        .iter()
        .any(|note| note.note_id == fixture.sibling));
}

#[test]
fn startup_snapshot_does_not_serve_a_stale_summary_for_a_corrupt_remembered_note() {
    let fixture = warm_two_note_library();
    let remembered_path = note_path(&fixture.library, &fixture.remembered);
    fs::write(&remembered_path, b"not a note package").unwrap();

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert!(view.session.is_none());
    let library = view.library.unwrap();
    assert_eq!(library.scan_state, ScanState::Complete);
    assert!(!library
        .notes
        .iter()
        .any(|note| note.note_id == fixture.remembered));
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("无法读取笔记")));
    let search = restarted.search_notes("remembered-token", None, 0).unwrap();
    assert_eq!(search.total, 0);
    assert!(search.hits.is_empty());
    assert_eq!(
        fs::read(&remembered_path).unwrap(),
        b"not a note package".to_vec()
    );
}

#[test]
fn startup_snapshot_does_not_reuse_cached_identity_for_a_mismatched_note_file() {
    let fixture = warm_two_note_library();
    let remembered_path = note_path(&fixture.library, &fixture.remembered);
    let notebook_id = storage::open_note(&remembered_path)
        .unwrap()
        .package
        .document
        .organization
        .notebook_id
        .clone()
        .unwrap();
    let replacement =
        NotePackage::new_plain(notebook_id, plain("Replacement note", "replacement-token"));
    assert_ne!(replacement.document.note_id, fixture.remembered);
    write_package(&mut File::create(&remembered_path).unwrap(), &replacement).unwrap();
    let replacement_bytes = fs::read(&remembered_path).unwrap();

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert!(view.session.is_none());
    let library = view.library.unwrap();
    assert_eq!(library.scan_state, ScanState::Complete);
    assert!(!library
        .notes
        .iter()
        .any(|note| note.note_id == fixture.remembered));
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("身份不一致")));
    assert_eq!(fs::read(&remembered_path).unwrap(), replacement_bytes);
}

#[test]
fn startup_snapshot_does_not_bypass_a_damaged_observed_witness() {
    let fixture = warm_two_note_library();
    let remembered_path = note_path(&fixture.library, &fixture.remembered);
    let remembered_bytes = fs::read(&remembered_path).unwrap();
    let remembered_revision = storage::open_note(&remembered_path).unwrap().revision;
    let mut warm = workspace(fixture.temp.path());
    warm.snapshot().unwrap();
    let version = warm
        .list_preserved_versions()
        .unwrap()
        .versions
        .into_iter()
        .find(|version| {
            version.note_id == fixture.remembered
                && version.source_revision == remembered_revision
                && !version.conflict
        })
        .expect("warm scan retained the remembered note version");
    drop(warm);
    let witness = fixture
        .temp
        .path()
        .join("recovery/versions")
        .join(&version.version_id)
        .join("metadata.json");
    fs::write(&witness, b"damaged observed witness").unwrap();

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert!(view.session.is_none());
    let library = view.library.unwrap();
    assert_eq!(library.scan_state, ScanState::Complete);
    assert!(!library
        .notes
        .iter()
        .any(|note| note.note_id == fixture.remembered));
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("无法保留笔记")));
    let search = restarted.search_notes("remembered-token", None, 0).unwrap();
    assert_eq!(search.total, 0);
    assert!(search.incomplete);
    assert_eq!(
        fs::read(&witness).unwrap(),
        b"damaged observed witness".to_vec()
    );
    assert_eq!(fs::read(&remembered_path).unwrap(), remembered_bytes);
}

#[test]
fn startup_snapshot_falls_back_to_complete_when_notebook_metadata_is_unhealthy() {
    let fixture = warm_two_note_library();
    let metadata_path = fs::read_dir(fixture.library.join("notebooks"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&metadata_path, b"{ not valid notebook metadata").unwrap();

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    let library = view.library.as_ref().unwrap();
    assert_eq!(library.scan_state, ScanState::Complete);
    assert!(!library.warnings.is_empty());
    // The ordinary path still opens the remembered note, now read-only because
    // its notebook metadata is unavailable.
    assert!(view
        .session
        .as_ref()
        .is_some_and(|session| session.read_only_reason.is_some()));
}

#[test]
fn startup_snapshot_keeps_a_pending_library_restore_blocked() {
    let temp = support::tempdir("startup-snapshot-restore-");
    let library = temp.path().join("Library");
    let mut app = workspace(temp.path());
    app.select_library(library.clone()).unwrap();
    let note = app.new_note().unwrap();
    app.save_note_text(&note.session_id, plain("Source version", "source bytes"))
        .unwrap();
    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let source = app
        .backup_context()
        .unwrap()
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let preview = app
        .preview_library_restore(&source.backup_id, &job)
        .unwrap();
    drop(job);
    app.save_note_text(&note.session_id, plain("Current version", "current bytes"))
        .unwrap();

    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_cancelled = Arc::clone(&cancelled);
    let callback_control = control.clone();
    let job = control
        .begin_job(Some(Arc::new(move |progress| {
            if progress.phase == BackupPhase::Restoring
                && progress.checked == 0
                && !callback_cancelled.swap(true, Ordering::SeqCst)
            {
                callback_control.cancel();
            }
        })))
        .unwrap();
    let error = app
        .restore_library_from_backup(
            &source.backup_id,
            &preview.manifest_revision,
            "2026-10-15",
            &job,
        )
        .unwrap_err();
    assert_eq!(error.kind, "backupCancelled");
    assert!(cancelled.load(Ordering::SeqCst));
    drop(job);
    drop(app);

    let mut restarted = workspace(temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert!(view.library_restore.is_some());
    assert!(view.session.is_none());
    let library = view.library.unwrap();
    assert_eq!(library.scan_state, ScanState::Unavailable);
    assert!(library.notes.is_empty());
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("恢复")));
}

#[test]
fn full_snapshot_keeps_complete_semantics_and_scans_every_note() {
    let fixture = warm_two_note_library();

    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.snapshot().unwrap();
    let library = view.library.unwrap();
    assert_eq!(library.scan_state, ScanState::Complete);
    assert_eq!(library.notes.len(), 2);
    assert!(view.session.is_some());
    assert_eq!(
        restarted
            .search_notes("sibling-token", None, 0)
            .unwrap()
            .status,
        SearchStatus::Ready
    );
}

#[test]
fn startup_reads_and_retains_current_rich_content_and_images_before_the_full_scan() {
    let fixture = warm_two_note_library();
    let current_path = note_path(&fixture.library, &fixture.remembered);
    let original = storage::open_note(&current_path).unwrap().package;
    let rich_path = support::fixture_file(fixture.temp.path(), "valid-rich-v1.cuenote");
    let mut rich = storage::open_note(&rich_path).unwrap().package;
    rich.document.note_id = fixture.remembered.clone();
    rich.document.organization = original.document.organization;
    write_package(&mut File::create(&current_path).unwrap(), &rich).unwrap();
    let revision = storage::open_note(&current_path).unwrap().revision;
    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert_eq!(view.library.unwrap().scan_state, ScanState::Pending);
    let session = view.session.unwrap();
    assert_eq!(session.content, rich.content());
    assert!(!rich.assets.is_empty());
    for (asset_id, bytes) in &rich.assets {
        assert_eq!(
            restarted
                .read_image_asset(&session.session_id, asset_id)
                .unwrap(),
            *bytes
        );
    }
    assert!(restarted
        .list_preserved_versions()
        .unwrap()
        .versions
        .iter()
        .any(|v| { v.note_id == fixture.remembered && v.source_revision == revision }));
}

#[test]
fn startup_rejects_a_future_version_instead_of_serving_the_cached_current_note() {
    let fixture = warm_two_note_library();
    let current_path = note_path(&fixture.library, &fixture.remembered);
    let future = include_bytes!("fixtures/unsupported-version.cuenote");
    fs::write(&current_path, future).unwrap();
    let mut restarted = workspace(fixture.temp.path());
    let view = restarted.startup_snapshot().unwrap();
    assert!(view.session.is_none());
    assert_eq!(view.library.unwrap().scan_state, ScanState::Complete);
    assert!(view.notice.unwrap().contains("版本"));
    assert_eq!(
        restarted
            .search_notes("remembered-token", None, 0)
            .unwrap()
            .total,
        0
    );
    assert_eq!(fs::read(&current_path).unwrap(), future);
}

#[test]
fn the_first_full_probe_preserves_external_content_and_a_new_startup_draft() {
    let fixture = warm_two_note_library();
    let current_path = note_path(&fixture.library, &fixture.remembered);
    let mut restarted = workspace(fixture.temp.path());
    let initial = restarted.startup_snapshot().unwrap();
    assert_eq!(initial.library.unwrap().scan_state, ScanState::Pending);
    let session = initial.session.unwrap();
    let mut local = storage::open_note(&current_path).unwrap().package;
    local
        .replace_plain_text(plain("Local draft", "new-startup-draft"))
        .unwrap();
    rewrite_note(&current_path, plain("External", "external-startup-version"));
    let probe = restarted
        .probe_external_changes(Some(&session.session_id))
        .unwrap();
    assert_eq!(probe.status, "changed");
    assert_eq!(probe.library.unwrap().scan_state, ScanState::Complete);
    let result = restarted
        .reconcile_external_changes(&session.session_id, local.content())
        .unwrap();
    assert_eq!(result.status, "conflict");
    assert!(result.session.is_none());
    assert_eq!(
        storage::open_note(&current_path)
            .unwrap()
            .package
            .text_projection()
            .content,
        "external-startup-version"
    );
    let versions = restarted.list_preserved_versions().unwrap().versions;
    let mut texts = Vec::new();
    for version in versions.iter().filter(|v| v.note_id == fixture.remembered) {
        texts.push(
            restarted
                .open_preserved_version(&version.version_id)
                .unwrap()
                .text
                .content,
        );
    }
    for expected in [
        "remembered-token",
        "new-startup-draft",
        "external-startup-version",
    ] {
        assert!(
            texts.iter().any(|text| text == expected),
            "missing retained version: {expected}"
        );
    }
}
