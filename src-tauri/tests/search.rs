use cuenote_lib::note_format::{
    AssetManifestEntry, Block, Inline, Mark, MediaType, MindMapNode, NotePackage, PlainText,
    TableRow,
};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("search-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn search_finds_a_title_case_insensitively_and_returns_the_source_spelling() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace.select_library(library_path.clone()).unwrap();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Vector Crates".into(),
                cue: "Aerial bearings".into(),
                content: "Service plan lower".into(),
                summary: "Staged release".into(),
            },
        )
        .unwrap();

    let results = workspace.search_notes("cRATEs", None, 0).unwrap();

    assert_eq!(results.query, "cRATEs");
    assert_eq!(results.status, cuenote_lib::workspace::SearchStatus::Ready);
    assert_eq!(results.indexed_notes, 1);
    assert_eq!(results.total, 1);
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].note.note_id, note.note_id);
    assert_eq!(
        results.hits[0].section,
        cuenote_lib::workspace::SearchSection::Title
    );
    assert_eq!(results.hits[0].snippet.matched, "Crates");
    assert_eq!(results.hits[0].snippet.before, "Vector ");
    assert_eq!(results.hits[0].snippet.after, "");
    assert_eq!(PathBuf::from(results.library_path), library_path);
    assert!(workspace.search_notes(&"x".repeat(1_025), None, 0).is_err());
}

#[test]
fn search_indexes_rich_text_code_latex_tables_lists_and_mind_map_text_only() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let recovery_path = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery_path.clone(),
        temp.path().join("default"),
    );
    let selected = workspace.select_library(library_path.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let mut package = NotePackage::new_plain(
        notebook_id,
        PlainText {
            title: "Asset Ledger".into(),
            cue: "Original Aerial Note".into(),
            content: String::new(),
            summary: "Release summary".into(),
        },
    );
    let image_id = "33333333-3333-4333-8333-333333333333";
    let map_id = "44444444-4444-4444-8444-444444444444";
    let root_id = "11111111-1111-4111-8111-111111111111";
    let child_id = "22222222-2222-4222-8222-222222222222";
    package.document.sections.content = vec![
        Block::Paragraph {
            children: vec![
                Inline::Text {
                    text: "康奈尔".into(),
                    marks: Vec::new(),
                },
                Inline::Text {
                    text: "笔记连续".into(),
                    marks: vec![Mark::Bold],
                },
            ],
        },
        Block::Paragraph {
            children: vec![Inline::Text {
                text: "Café".into(),
                marks: Vec::new(),
            }],
        },
        Block::CodeBlock {
            language: Some("rust".into()),
            code: "fn search_token() { emit(7); }".into(),
        },
        Block::MathBlock {
            latex: r"\sum_{n=1}^{9} n".into(),
        },
        Block::Paragraph {
            children: vec![Inline::MathInline {
                latex: r"\frac{alpha_i}{2}".into(),
            }],
        },
        Block::Table {
            rows: vec![
                TableRow::Header {
                    cells: vec![vec![Inline::Text {
                        text: "Header Atlas".into(),
                        marks: Vec::new(),
                    }]],
                },
                TableRow::Body {
                    cells: vec![vec![Inline::Text {
                        text: "Cell Meridian".into(),
                        marks: Vec::new(),
                    }]],
                },
            ],
        },
        Block::BulletList {
            items: vec![cuenote_lib::note_format::ListItem {
                blocks: vec![Block::Paragraph {
                    children: vec![Inline::Text {
                        text: "List Cardinal".into(),
                        marks: Vec::new(),
                    }],
                }],
            }],
        },
        Block::MindMap {
            map_id: map_id.into(),
            root_id: root_id.into(),
            nodes: vec![
                MindMapNode {
                    id: root_id.into(),
                    text: "Mind Root".into(),
                    children: vec![child_id.into()],
                },
                MindMapNode {
                    id: child_id.into(),
                    text: "Node Meridian".into(),
                    children: Vec::new(),
                },
            ],
        },
        Block::Image {
            asset_id: image_id.into(),
            display_width_px: 1,
        },
    ];
    package.document.sections.summary = vec![
        Block::Paragraph {
            children: vec![
                Inline::Text {
                    text: "前缀中文".into(),
                    marks: Vec::new(),
                },
                Inline::Text {
                    text: "😊长尾摘要".into(),
                    marks: vec![Mark::Bold],
                },
            ],
        },
        Block::Paragraph {
            children: vec![Inline::Text {
                text: "Release summary".into(),
                marks: Vec::new(),
            }],
        },
    ];
    let image =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pixel.png"))
            .unwrap();
    package.manifest.assets.push(AssetManifestEntry {
        id: image_id.into(),
        media_type: MediaType::Png,
        byte_length: image.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&image)),
        width: 1,
        height: 1,
    });
    package.assets.insert(image_id.into(), image);
    let note_id = package.document.note_id.clone();
    storage::save_note(
        &library_path
            .join("notes")
            .join(format!("{note_id}.cuenote")),
        &recovery_path,
        &package,
        None,
    )
    .unwrap();

    workspace.rebuild_search_index().unwrap();

    // Exercise replacement of an existing rich note, not only initial indexing.
    let session = workspace.open_note(&note_id).unwrap();
    let mut revised = session.content;
    revised.title = "Updated asset ledger".into();
    workspace
        .save_note_content(&session.session_id, revised)
        .unwrap();

    for (query, section, matched) in [
        ("original aerial", "cue", "Original Aerial"),
        ("奈尔笔记", "content", "奈尔笔记"),
        ("search_token", "content", "search_token"),
        (r"\sum_{n=1}^{9}", "content", r"\sum_{n=1}^{9}"),
        (r"\frac{alpha_i}", "content", r"\frac{alpha_i}"),
        ("cell meridian", "content", "Cell Meridian"),
        ("list cardinal", "content", "List Cardinal"),
        ("node meridian", "content", "Node Meridian"),
        ("😊长尾", "summary", "😊长尾"),
        ("release summary", "summary", "Release summary"),
    ] {
        let results = workspace.search_notes(query, None, 0).unwrap();
        assert_eq!(results.total, 1, "query {query}");
        assert_eq!(results.hits[0].note.note_id, note_id, "query {query}");
        assert_eq!(
            format!("{:?}", results.hits[0].section).to_lowercase(),
            section
        );
        assert_eq!(results.hits[0].snippet.matched, matched, "query {query}");
    }

    for excluded in [image_id, "[image", "**"] {
        assert_eq!(
            workspace.search_notes(excluded, None, 0).unwrap().total,
            0,
            "excluded token {excluded}"
        );
    }

    let whitespace = workspace.search_notes(" \t", None, 0).unwrap();
    assert_eq!(whitespace.query, " \t");
    assert!(whitespace.hits.is_empty());
    assert_eq!(
        workspace
            .search_notes(" original aerial ", None, 0)
            .unwrap()
            .total,
        0
    );
    assert_eq!(workspace.search_notes("CAFÉ", None, 0).unwrap().total, 0);
    assert_eq!(workspace.search_notes("CAFé", None, 0).unwrap().total, 1);
    assert_eq!(
        workspace
            .search_notes("node meridian", Some("other-notebook"), 0)
            .unwrap()
            .total,
        0
    );
}

#[test]
fn search_matches_actual_segments_without_joining_structural_boundaries() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let recovery_path = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery_path.clone(),
        temp.path().join("default"),
    );
    let selected = workspace.select_library(library_path.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let mut package = NotePackage::new_plain(
        notebook_id,
        PlainText {
            title: "Segment boundaries".into(),
            cue: String::new(),
            content: String::new(),
            summary: String::new(),
        },
    );
    let map_root = "11111111-1111-4111-8111-111111111111";
    let map_child = "22222222-2222-4222-8222-222222222222";
    package.document.sections.content = vec![
        Block::Paragraph {
            children: vec![Inline::Text {
                text: "paragraph-left".into(),
                marks: Vec::new(),
            }],
        },
        Block::Paragraph {
            children: vec![Inline::Text {
                text: "paragraph-right".into(),
                marks: Vec::new(),
            }],
        },
        Block::Paragraph {
            children: vec![
                Inline::Text {
                    text: "hard-left".into(),
                    marks: Vec::new(),
                },
                Inline::HardBreak,
                Inline::Text {
                    text: "hard-right".into(),
                    marks: Vec::new(),
                },
            ],
        },
        Block::CodeBlock {
            language: None,
            code: "code-left\ncode-right".into(),
        },
        Block::Table {
            rows: vec![TableRow::Header {
                cells: vec![
                    vec![Inline::Text {
                        text: "left".into(),
                        marks: Vec::new(),
                    }],
                    vec![Inline::Text {
                        text: "right".into(),
                        marks: Vec::new(),
                    }],
                ],
            }],
        },
        Block::MindMap {
            map_id: "33333333-3333-4333-8333-333333333333".into(),
            root_id: map_root.into(),
            nodes: vec![
                MindMapNode {
                    id: map_root.into(),
                    text: "map-left".into(),
                    children: vec![map_child.into()],
                },
                MindMapNode {
                    id: map_child.into(),
                    text: "map-right".into(),
                    children: Vec::new(),
                },
            ],
        },
        Block::Paragraph {
            children: vec![
                Inline::Text {
                    text: "style-left".into(),
                    marks: Vec::new(),
                },
                Inline::Text {
                    text: "style-right".into(),
                    marks: vec![Mark::Bold],
                },
            ],
        },
    ];
    let note_id = package.document.note_id.clone();
    storage::save_note(
        &library_path
            .join("notes")
            .join(format!("{note_id}.cuenote")),
        &recovery_path,
        &package,
        None,
    )
    .unwrap();
    workspace.rebuild_search_index().unwrap();

    for invented_join in [
        "left\tright",
        "paragraph-left\n\nparagraph-right",
        "map-left\nmap-right",
    ] {
        assert_eq!(
            workspace
                .search_notes(invented_join, None, 0)
                .unwrap()
                .total,
            0,
            "structure separators must not create a searchable match: {invented_join:?}"
        );
    }
    for actual_segment in [
        "hard-left\nhard-right",
        "code-left\ncode-right",
        "style-leftstyle-right",
    ] {
        let results = workspace.search_notes(actual_segment, None, 0).unwrap();
        assert_eq!(results.total, 1, "query {actual_segment:?}");
        assert_eq!(
            results.hits[0].section,
            cuenote_lib::workspace::SearchSection::Content
        );
        assert_eq!(results.hits[0].snippet.matched, actual_segment);
    }
}

#[test]
fn saved_note_is_searchable_before_the_followup_probe_checks_other_files() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let recovery = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library_path.clone()).unwrap();
    let other = workspace.new_note().unwrap();
    let other_title = other.text.title.clone();
    workspace
        .save_note_text(&other.session_id, other.text.clone())
        .unwrap();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Previous cardinal".into(),
                cue: "old needle".into(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();
    let other_path = library_path
        .join("notes")
        .join(format!("{}.cuenote", other.note_id));
    let mut external = storage::open_note(&other_path).unwrap();
    external.package.document.title = "Externally revised atlas".into();
    storage::save_note(
        &other_path,
        &recovery,
        &external.package,
        Some(&external.revision),
    )
    .unwrap();
    let cache_files = || -> std::collections::BTreeMap<_, _> {
        std::fs::read_dir(temp.path().join("settings/search-cache-v1"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect()
    };
    let cache_before = cache_files();

    let content = NotePackage::new_plain(
        note.notebook_id.clone().unwrap(),
        PlainText {
            title: "Committed cardinal".into(),
            cue: "new needle".into(),
            content: "body".into(),
            summary: "Saved summary".into(),
        },
    )
    .content();
    let receipt = workspace
        .save_note_content(&note.session_id, content)
        .unwrap();
    let saved = receipt
        .library
        .notes
        .iter()
        .find(|item| item.note_id == note.note_id)
        .unwrap();
    assert_eq!(saved.title, "Committed cardinal");
    assert_eq!(saved.excerpt, "Saved summary");
    assert_eq!(
        workspace.search_notes("new needle", None, 0).unwrap().total,
        1
    );
    assert_eq!(
        workspace.search_notes("old needle", None, 0).unwrap().total,
        0
    );
    assert_eq!(
        receipt
            .library
            .notes
            .iter()
            .find(|item| item.note_id == other.note_id)
            .unwrap()
            .title,
        other_title,
        "a safe save projects the committed note without waiting for unrelated files"
    );
    assert_eq!(
        cache_files(),
        cache_before,
        "unverified library entries must not be published as a fresh disk cache"
    );

    let probe = workspace
        .probe_external_changes(Some(&note.session_id))
        .unwrap();
    assert_eq!(probe.status, "unchanged");
    assert_eq!(
        probe
            .library
            .unwrap()
            .notes
            .iter()
            .find(|item| item.note_id == other.note_id)
            .unwrap()
            .title,
        "Externally revised atlas"
    );
    assert_eq!(
        workspace
            .search_notes("Externally revised", None, 0)
            .unwrap()
            .total,
        1
    );
    assert_ne!(cache_files(), cache_before);
    assert_eq!(
        storage::open_note(&other_path).unwrap().package,
        external.package
    );
}

#[test]
fn saved_note_growth_respects_the_full_search_budget_and_shrink_recovers() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    let notebook = workspace
        .select_library(library.clone())
        .unwrap()
        .library
        .unwrap()
        .default_notebook_id
        .unwrap();
    for (index, title) in [(1, "Budget first"), (2, "Budget second")] {
        let mut package = NotePackage::new_plain(
            notebook.clone(),
            PlainText {
                title: title.into(),
                cue: String::new(),
                content: "a".repeat(31 * 1024 * 1024),
                summary: String::new(),
            },
        );
        package.document.note_id = format!("00000000-0000-4000-8000-{index:012}");
        let path = library
            .join("notes")
            .join(format!("{}.cuenote", package.document.note_id));
        cuenote_lib::note_format::write_package(
            &mut std::fs::File::create(path).unwrap(),
            &package,
        )
        .unwrap();
    }
    workspace.rebuild_search_index().unwrap();
    let initial = workspace.search_notes("Budget", None, 0).unwrap();
    assert_eq!(initial.total, 2);
    assert!(!initial.incomplete);
    let session = workspace
        .open_note("00000000-0000-4000-8000-000000000001")
        .unwrap();
    let mut content = session.content;
    let Block::Paragraph { children } = &mut content.sections.content[0] else {
        panic!("plain body");
    };
    let Inline::Text { text, .. } = &mut children[0] else {
        panic!("plain text");
    };
    text.push_str(&"b".repeat(3 * 1024 * 1024));
    workspace
        .save_note_content(&session.session_id, content)
        .unwrap();
    let over_budget = workspace.search_notes("Budget", None, 0).unwrap();
    assert!(over_budget.incomplete);
    assert_eq!(over_budget.indexed_notes, 1);
    assert_eq!(
        workspace
            .search_notes("Budget first", None, 0)
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        workspace
            .search_notes("Budget second", None, 0)
            .unwrap()
            .total,
        0
    );
    let smaller = NotePackage::new_plain(
        notebook,
        PlainText {
            title: "Budget first smaller".into(),
            cue: String::new(),
            content: "small body".into(),
            summary: String::new(),
        },
    )
    .content();
    workspace
        .save_note_content(&session.session_id, smaller)
        .unwrap();
    let recovered = workspace.search_notes("Budget", None, 0).unwrap();
    assert!(!recovered.incomplete);
    assert_eq!(recovered.indexed_notes, 2);
    assert_eq!(recovered.total, 2);
}

#[test]
fn search_tracks_save_move_trash_and_restore() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    let selected = workspace.select_library(library_path).unwrap();
    let default_notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let created = workspace.create_notebook("Research").unwrap();
    let research_notebook_id = created
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Research")
        .unwrap()
        .notebook_id
        .clone();
    let session = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &session.session_id,
            PlainText {
                title: "Lifecycle sample".into(),
                cue: "First needle".into(),
                content: "Body remains searchable".into(),
                summary: "Summary remains searchable".into(),
            },
        )
        .unwrap();

    assert_eq!(
        workspace
            .search_notes("needle", Some(&default_notebook_id), 0)
            .unwrap()
            .total,
        1
    );
    workspace
        .update_note_organization(&session.session_id, &research_notebook_id, true)
        .unwrap();
    assert_eq!(
        workspace
            .search_notes("needle", Some(&default_notebook_id), 0)
            .unwrap()
            .total,
        0
    );
    let moved = workspace
        .search_notes("needle", Some(&research_notebook_id), 0)
        .unwrap();
    assert_eq!(moved.total, 1);
    assert!(moved.hits[0].note.pinned);

    workspace
        .save_note_text(
            &session.session_id,
            PlainText {
                title: "Lifecycle sample revised".into(),
                cue: "Second needle".into(),
                content: "Body remains searchable".into(),
                summary: "Summary remains searchable".into(),
            },
        )
        .unwrap();
    assert_eq!(
        workspace
            .search_notes("first needle", None, 0)
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        workspace
            .search_notes("second needle", Some(&research_notebook_id), 0)
            .unwrap()
            .total,
        1
    );

    workspace.trash_note(&session.session_id).unwrap();
    assert_eq!(
        workspace
            .search_notes("second needle", None, 0)
            .unwrap()
            .total,
        0
    );
    workspace
        .restore_trashed_note(&session.session_id, Some(&research_notebook_id))
        .unwrap();
    assert_eq!(
        workspace
            .search_notes("second needle", Some(&research_notebook_id), 0)
            .unwrap()
            .total,
        1
    );
}

#[test]
fn search_rebuilds_external_notes_and_reports_incomplete_scans_after_restart() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let recovery_path = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery_path.clone(),
        temp.path().join("default"),
    );
    let selected = workspace.select_library(library_path.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let package = NotePackage::new_plain(
        notebook_id,
        PlainText {
            title: "External Starling".into(),
            cue: "External marker".into(),
            content: String::new(),
            summary: String::new(),
        },
    );
    let note_id = package.document.note_id.clone();
    let canonical_path = library_path
        .join("notes")
        .join(format!("{note_id}.cuenote"));
    storage::save_note(&canonical_path, &recovery_path, &package, None).unwrap();
    assert_eq!(
        workspace.search_notes("starling", None, 0).unwrap().total,
        0
    );

    let duplicate_path = library_path.join("notes").join("duplicate-copy.cuenote");
    storage::save_note(&duplicate_path, &recovery_path, &package, None).unwrap();
    std::fs::write(
        library_path.join("notes").join("broken.cuenote"),
        b"not a note package",
    )
    .unwrap();
    workspace.rebuild_search_index().unwrap();
    let rebuilt = workspace.search_notes("starling", None, 0).unwrap();
    assert_eq!(rebuilt.status, cuenote_lib::workspace::SearchStatus::Ready);
    assert_eq!(
        rebuilt.total, 1,
        "one note produces one hit despite a duplicate file"
    );
    assert!(
        rebuilt.incomplete,
        "duplicate identity and unreadable file are tracked structurally"
    );

    drop(workspace);
    let mut restarted = Workspace::new(
        temp.path().join("settings"),
        recovery_path,
        temp.path().join("default"),
    );
    restarted.select_library(library_path).unwrap();
    let startup = restarted.search_notes("external marker", None, 0).unwrap();
    assert_eq!(startup.total, 1);
    assert!(startup.incomplete);
}

#[test]
fn search_returns_no_old_hits_when_notes_directory_is_unavailable_and_recovers() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let recovery_path = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery_path.clone(),
        temp.path().join("default"),
    );
    let selected = workspace.select_library(library_path.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let note = workspace.new_note().unwrap();
    let note_id = note.note_id.clone();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Inside library".into(),
                cue: "directory probe token".into(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap();

    let outside = NotePackage::new_plain(
        notebook_id,
        PlainText {
            title: "Outside package".into(),
            cue: "external-only token".into(),
            content: String::new(),
            summary: String::new(),
        },
    );
    let outside_directory = temp.path().join("external");
    std::fs::create_dir_all(&outside_directory).unwrap();
    storage::save_note(
        &outside_directory.join("outside.cuenote"),
        &recovery_path,
        &outside,
        None,
    )
    .unwrap();
    assert_eq!(
        workspace
            .search_notes("external-only", None, 0)
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        workspace
            .search_notes("directory probe", None, 0)
            .unwrap()
            .total,
        1
    );

    let notes_path = library_path.join("notes");
    let unavailable_path = library_path.join("notes-delayed");
    std::fs::rename(&notes_path, &unavailable_path).unwrap();
    let unavailable_view = workspace.rebuild_search_index().unwrap();
    assert!(
        unavailable_view
            .notes
            .iter()
            .any(|item| item.note_id == note_id),
        "a temporarily missing notes directory must preserve the last visible list"
    );
    let unavailable = workspace.search_notes("directory probe", None, 0).unwrap();
    assert_eq!(
        unavailable.status,
        cuenote_lib::workspace::SearchStatus::Unavailable
    );
    assert!(unavailable.hits.is_empty());
    assert_eq!(unavailable.total, 0);

    std::fs::rename(&unavailable_path, &notes_path).unwrap();
    workspace.rebuild_search_index().unwrap();
    let recovered = workspace.search_notes("directory probe", None, 0).unwrap();
    assert_eq!(
        recovered.status,
        cuenote_lib::workspace::SearchStatus::Ready
    );
    assert_eq!(recovered.total, 1);
    assert_eq!(
        workspace
            .search_notes("external-only", None, 0)
            .unwrap()
            .total,
        0
    );
}

#[test]
fn search_scopes_ranks_and_paginates_one_hit_per_note_with_local_timing() {
    let temp = workspace_temp();
    let library_path = temp.path().join("Library");
    let recovery_path = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery_path.clone(),
        temp.path().join("default"),
    );
    let selected = workspace.select_library(library_path.clone()).unwrap();
    let default_notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let created = workspace.create_notebook("Scoped").unwrap();
    let scoped_notebook_id = created
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Scoped")
        .unwrap()
        .notebook_id
        .clone();
    let timestamp = "2026-10-01T00:00:00.000Z";
    let mut ids = Vec::new();
    for index in 0..105 {
        let notebook_id = if index == 104 {
            scoped_notebook_id.clone()
        } else {
            default_notebook_id.clone()
        };
        let mut package = NotePackage::new_plain(
            notebook_id,
            PlainText {
                title: format!("Page {index:03} common-token"),
                cue: "common-token in cue".into(),
                content: "common-token in content".into(),
                summary: String::new(),
            },
        );
        package.document.note_id = format!("00000000-0000-4000-8000-{index:012}");
        package.document.timestamps.content_modified_at = timestamp.into();
        package.document.organization.pinned = index == 104;
        let note_id = package.document.note_id.clone();
        storage::save_note(
            &library_path
                .join("notes")
                .join(format!("{note_id}.cuenote")),
            &recovery_path,
            &package,
            None,
        )
        .unwrap();
        ids.push(note_id);
    }
    workspace.rebuild_search_index().unwrap();

    let page = workspace.search_notes("COMMON-TOKEN", None, 0).unwrap();
    assert_eq!(page.total, 105);
    assert_eq!(page.hits.len(), 100);
    assert_eq!(
        page.hits[0].note.note_id, ids[104],
        "pinned notes rank first"
    );
    assert_eq!(page.hits[1].note.note_id, ids[0]);
    assert_eq!(
        page.hits[1].section,
        cuenote_lib::workspace::SearchSection::Title
    );
    assert_eq!(page.hits[1].snippet.matched, "common-token");
    assert_eq!(
        workspace
            .search_notes("common-token", Some(&scoped_notebook_id), 0)
            .unwrap()
            .total,
        1
    );
    let last_page = workspace.search_notes("common-token", None, 100).unwrap();
    assert_eq!(last_page.offset, 100);
    assert_eq!(last_page.total, 105);
    assert_eq!(last_page.hits.len(), 5);
    assert_eq!(last_page.hits[0].note.note_id, ids[99]);
    assert_eq!(last_page.hits[4].note.note_id, ids[103]);

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        let results = workspace.search_notes("common-token", None, 0).unwrap();
        samples.push(start.elapsed());
        assert_eq!(results.total, 105);
    }
    let min = samples.iter().copied().min().unwrap().as_secs_f64() * 1_000.0;
    let max = samples.iter().copied().max().unwrap().as_secs_f64() * 1_000.0;
    let mean = samples
        .iter()
        .map(|sample| sample.as_secs_f64())
        .sum::<f64>()
        * 1_000.0
        / samples.len() as f64;
    eprintln!(
        "search query timing: sample_notes=105 iterations={} profile={profile} mean_ms={mean:.3} range_ms={min:.3}..{max:.3}",
        samples.len()
    );
}
