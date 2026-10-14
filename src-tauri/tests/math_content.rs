use cuenote_lib::note_format::{Block, Inline, MindMapNode, NoteContent, Sections, TableRow};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::path::{Path, PathBuf};

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("math-content-")
        .tempdir_in(base)
        .unwrap()
}

fn workspace_paths(temp: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
        temp.path().join("Library"),
    )
}

fn math_content() -> NoteContent {
    NoteContent {
        title: "数学内容保真".into(),
        sections: Sections {
            cue: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "关键公式".into(),
                    marks: Vec::new(),
                }],
            }],
            content: vec![
                Block::Paragraph {
                    children: vec![
                        Inline::Text {
                            text: "E = ".into(),
                            marks: Vec::new(),
                        },
                        Inline::MathInline {
                            latex: "mc^2".into(),
                        },
                    ],
                },
                Block::MathBlock {
                    latex: "\\int_0^1 x^2 \\, dx".into(),
                },
                Block::BulletList {
                    items: vec![cuenote_lib::note_format::ListItem {
                        blocks: vec![Block::Paragraph {
                            children: vec![Inline::MathInline {
                                latex: "\\sqrt{x}".into(),
                            }],
                        }],
                    }],
                },
                Block::Table {
                    rows: vec![
                        TableRow::Header {
                            cells: vec![vec![Inline::Text {
                                text: "量".into(),
                                marks: Vec::new(),
                            }]],
                        },
                        TableRow::Body {
                            cells: vec![vec![Inline::MathInline {
                                latex: "a^2+b^2".into(),
                            }]],
                        },
                    ],
                },
            ],
            summary: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "保留源码".into(),
                    marks: Vec::new(),
                }],
            }],
        },
    }
}

#[test]
fn content_math_survives_workspace_save_open_restart_and_save_as() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings.clone(), recovery.clone(), default.clone());
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    let expected = math_content();

    let saved = workspace
        .save_note_content(&created.session_id, expected.clone())
        .unwrap();

    assert_eq!(saved.session.content, expected);
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let stored = storage::open_note(&note_path).unwrap();
    assert_eq!(stored.package.document.schema_version, 1);
    assert_eq!(stored.package.content(), expected);
    assert_eq!(saved.session.text, stored.package.text_projection());
    assert_eq!(
        saved.session.text.content,
        "E = mc^2\n\n$$\n\\int_0^1 x^2 \\, dx\n$$\n\n- \\sqrt{x}\n\n量\na^2+b^2"
    );

    let reopened = workspace.open_note(&created.note_id).unwrap();
    assert_eq!(reopened.content, expected);
    assert_eq!(reopened.text, stored.package.text_projection());

    let mut restarted = Workspace::new(settings, recovery, default);
    restarted.select_library(library).unwrap();
    let restarted_session = restarted.open_note(&created.note_id).unwrap();
    assert!(restarted_session.read_only_reason.is_none());
    assert_eq!(restarted_session.content, expected);
    let copy_path = temp.path().join("math-copy.cuenote");
    restarted
        .save_current_content_as(
            &restarted_session.session_id,
            expected.clone(),
            copy_path.clone(),
        )
        .unwrap();
    let copied = storage::open_note(&copy_path).unwrap();
    assert_ne!(copied.package.document.note_id, created.note_id);
    assert_eq!(copied.package.document.schema_version, 1);
    assert_eq!(copied.package.content(), expected);
}

#[test]
fn arbitrary_latex_is_preserved_without_host_validation() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery, default);
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();

    let invalid = r"\unknownCommand{unfinished";
    let macro_source = r"\newcommand{\R}{\mathbb{R}} \R";
    let deep_source = format!("{}x{}", "{".repeat(256), "}".repeat(256));
    let large_source = "x".repeat(300_000);
    let expected = NoteContent {
        title: "原样保存任意 LaTeX".into(),
        sections: Sections {
            cue: Vec::new(),
            content: vec![
                Block::Paragraph {
                    children: vec![
                        Inline::MathInline {
                            latex: String::new(),
                        },
                        Inline::MathInline {
                            latex: invalid.into(),
                        },
                        Inline::MathInline {
                            latex: macro_source.into(),
                        },
                        Inline::MathInline {
                            latex: deep_source.clone(),
                        },
                    ],
                },
                Block::MathBlock {
                    latex: large_source.clone(),
                },
            ],
            summary: Vec::new(),
        },
    };

    let saved = workspace
        .save_note_content(&created.session_id, expected.clone())
        .unwrap();
    assert_eq!(saved.session.content, expected);
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let stored = storage::open_note(&note_path).unwrap();
    assert_eq!(stored.package.content(), expected);
    let projected = stored.package.text_projection().content;
    assert!(projected.contains(invalid));
    assert!(projected.contains(macro_source));
    assert!(projected.contains(&deep_source));
    assert!(projected.ends_with(&format!("$$\n{large_source}\n$$")));
    assert_eq!(
        workspace.open_note(&created.note_id).unwrap().content,
        expected
    );
}

#[test]
fn math_is_rejected_in_side_sections_and_unsupported_content_stays_read_only() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery.clone(), default);
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    let expected = math_content();
    let saved = workspace
        .save_note_content(&created.session_id, expected.clone())
        .unwrap();
    assert!(saved.session.read_only_reason.is_none());
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let before = storage::open_note(&note_path).unwrap();

    let mut cue_math = expected.clone();
    cue_math.sections.cue.push(Block::Paragraph {
        children: vec![Inline::MathInline {
            latex: "x^2".into(),
        }],
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, cue_math)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut summary_math = expected.clone();
    summary_math.sections.summary.push(Block::MathBlock {
        latex: "x^2".into(),
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, summary_math)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut sidebar_map = expected.clone();
    sidebar_map.sections.cue.push(Block::MindMap {
        map_id: "85ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
        root_id: "65ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
        nodes: vec![MindMapNode {
            id: "65ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
            text: "线索区不可编辑的导图".into(),
            children: Vec::new(),
        }],
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, sidebar_map)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let after = storage::open_note(&note_path).unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.package, before.package);
    assert_eq!(after.package.content(), expected);
}
