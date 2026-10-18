use cuenote_lib::{note_format::PlainText, workspace::Workspace};
use std::{fs, path::Path};

fn workspace(root: &Path) -> Workspace {
    Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("Library"),
    )
}

fn test_root() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("search-cache-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn warm_cache_detects_changed_bytes_even_with_equal_size_and_timestamp() {
    use cuenote_lib::note_format::{write_package, NotePackage};
    use std::{
        fs::{File, FileTimes},
        io::{self, Cursor},
    };
    // Stored JSON entries make equal-length substitutions produce equal-length
    // archives regardless of the generated UUIDs/timestamps. Deflate can vary
    // by one byte even when only AAAA is replaced with BBBB.
    let encode_stored = |package: &NotePackage| {
        let mut encoded = Cursor::new(Vec::new());
        write_package(&mut encoded, package).unwrap();
        let mut source = zip::ZipArchive::new(Cursor::new(encoded.into_inner())).unwrap();
        let mut target = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..source.len() {
            let mut entry = source.by_index(index).unwrap();
            target
                .start_file(
                    entry.name().unwrap(),
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            io::copy(&mut entry, &mut target).unwrap();
        }
        target.finish().unwrap().into_inner()
    };
    let temp = test_root();
    let root = temp.path();
    let mut app = workspace(root);
    let view = app.select_library(root.join("Library")).unwrap();
    let mut package = NotePackage::new_plain(
        view.library.unwrap().default_notebook_id.unwrap(),
        PlainText {
            title: "Source AAAA".into(),
            cue: "Cue".into(),
            content: "Original".into(),
            summary: "End".into(),
        },
    );
    let path = root
        .join("Library/notes")
        .join(format!("{}.cuenote", package.document.note_id));
    fs::write(&path, encode_stored(&package)).unwrap();
    app.snapshot().unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(app.search_notes("AAAA", None, 0).unwrap().total, 1);
    package.document.title = "Source BBBB".into();
    let encoded = encode_stored(&package);
    assert_eq!(
        encoded.len() as u64,
        metadata.len(),
        "fixture must preserve length"
    );
    fs::write(&path, encoded).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        metadata.modified().unwrap()
    );
    app.snapshot().unwrap();
    assert_eq!(app.search_notes("AAAA", None, 0).unwrap().total, 0);
    assert_eq!(app.search_notes("BBBB", None, 0).unwrap().total, 1);
}

#[test]
fn damaged_historical_witness_cannot_make_a_warm_index_claim_protection() {
    let temp = test_root();
    let root = temp.path();
    let mut app = workspace(root);
    app.select_library(root.join("Library")).unwrap();
    let note = app.new_note().unwrap();
    let receipt = app
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Witness current".into(),
                cue: String::new(),
                content: "protected-token".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let revision = receipt.session.revision.unwrap();
    let original_path = root
        .join("Library/notes")
        .join(format!("{}.cuenote", note.note_id));
    let original = fs::read(&original_path).unwrap();
    let version = app
        .list_preserved_versions()
        .unwrap()
        .versions
        .into_iter()
        .find(|version| version.source_revision == revision && !version.conflict)
        .unwrap();
    drop(app);
    let witness = root
        .join("recovery/versions")
        .join(&version.version_id)
        .join("note.cuenote");
    fs::write(&witness, b"damaged witness").unwrap();
    let mut reopened = workspace(root);
    reopened.snapshot().unwrap();
    let result = reopened.search_notes("protected-token", None, 0).unwrap();
    assert!(result.incomplete);
    assert_eq!(result.total, 0);
    assert_eq!(fs::read(witness).unwrap(), b"damaged witness");
    assert_eq!(fs::read(original_path).unwrap(), original);
}

#[test]
fn local_search_cache_is_disposable_and_never_changes_saved_notes() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("search-cache-")
        .tempdir_in(base)
        .unwrap();
    let root = temp.path();
    let mut app = workspace(root);
    app.select_library(root.join("Library")).unwrap();
    let session = app.new_note().unwrap();
    app.save_note_text(
        &session.session_id,
        PlainText {
            title: "离线索引".into(),
            cue: "坐标".into(),
            content: "Retained source café 公式".into(),
            summary: "完整笔记".into(),
        },
    )
    .unwrap();
    let note_path = root
        .join("Library/notes")
        .join(format!("{}.cuenote", session.note_id));
    let original = fs::read(&note_path).unwrap();
    let cache_root = root.join("settings/search-cache-v1");
    let caches = fs::read_dir(&cache_root)
        .expect("a rebuildable local index is persisted")
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert!(!caches.is_empty());
    drop(app);

    let mut restarted = workspace(root);
    restarted.snapshot().unwrap();
    assert_eq!(
        restarted.search_notes("café", None, 0).unwrap().hits[0]
            .note
            .note_id,
        session.note_id
    );
    drop(restarted);
    // Damaged derived files must fall back to the complete native files.
    for path in fs::read_dir(&cache_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
    {
        fs::write(path, b"damaged derived index").unwrap();
    }
    let mut rebuilt = workspace(root);
    rebuilt.snapshot().unwrap();
    let results = rebuilt.search_notes("café", None, 0).unwrap();
    assert_eq!(results.total, 1);
    assert!(!results.incomplete);
    assert_eq!(fs::read(note_path).unwrap(), original);
}

#[test]
fn warm_large_library_checks_changed_sources_and_damaged_witnesses_across_batches() {
    use cuenote_lib::note_format::{write_package, NotePackage};
    let temp = test_root();
    let root = temp.path();
    let mut app = workspace(root);
    let notebook = app
        .select_library(root.join("Library"))
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .unwrap();
    let mut packages = Vec::new();
    for index in 0..129 {
        let mut package = NotePackage::new_plain(
            notebook.clone(),
            PlainText {
                title: format!("Large cache {index}"),
                cue: String::new(),
                content: "batch-baseline-token".into(),
                summary: String::new(),
            },
        );
        package.document.note_id = format!("00000000-0000-4000-8000-{index:012x}");
        let path = root
            .join("Library/notes")
            .join(format!("{}.cuenote", package.document.note_id));
        write_package(&mut fs::File::create(&path).unwrap(), &package).unwrap();
        packages.push((package, path));
    }
    app.snapshot().unwrap();
    assert_eq!(
        app.search_notes("batch-baseline-token", None, 0)
            .unwrap()
            .total,
        129
    );
    let witness = app
        .list_preserved_versions()
        .unwrap()
        .versions
        .into_iter()
        .find(|version| version.note_id == packages[64].0.document.note_id && !version.conflict)
        .unwrap();
    drop(app);
    // Exercise early, middle and final entries without relying on completion
    // order or timestamps to authorize a cached search result.
    for index in [0, 63, 128] {
        let (package, path) = &mut packages[index];
        package.document.title = "batch-changed-token".into();
        write_package(&mut fs::File::create(path).unwrap(), package).unwrap();
    }
    let damaged = root
        .join("recovery/versions")
        .join(witness.version_id)
        .join("note.cuenote");
    fs::write(&damaged, b"damaged batch witness").unwrap();
    let untouched_source = fs::read(&packages[64].1).unwrap();
    let mut reopened = workspace(root);
    reopened.snapshot().unwrap();
    let changed = reopened
        .search_notes("batch-changed-token", None, 0)
        .unwrap();
    assert_eq!(changed.total, 3);
    assert!(changed.incomplete);
    assert_eq!(
        reopened
            .search_notes("batch-baseline-token", None, 0)
            .unwrap()
            .total,
        128
    );
    assert_eq!(fs::read(&damaged).unwrap(), b"damaged batch witness");
    assert_eq!(fs::read(&packages[64].1).unwrap(), untouched_source);
}
