use cuenote_lib::note_format::{Block, Inline, Mark, NoteContent, Sections};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::path::{Path, PathBuf};

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("code-content-")
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

#[test]
fn body_code_and_unknown_languages_roundtrip_through_workspace_and_file() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery, default);
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();

    let inline = "array[`key`] $value";
    let source = "\tconst value = `$1`;\n\n// 中文注释\n<div title=\"x\">$2</div>\n";
    let languages = [
        Some("javascript"),
        Some("typescript"),
        Some("python"),
        Some("rust"),
        Some("csharp"),
        Some("sql"),
        Some("json"),
        Some("mystery-λ"),
        None,
    ];
    let mut blocks = vec![Block::Paragraph {
        children: vec![
            Inline::Text {
                text: inline.into(),
                marks: vec![Mark::Code],
            },
            Inline::Text {
                text: " remains literal".into(),
                marks: Vec::new(),
            },
        ],
    }];
    blocks.extend(languages.into_iter().map(|language| Block::CodeBlock {
        language: language.map(str::to_owned),
        code: source.into(),
    }));
    let expected = NoteContent {
        title: "代码保真".into(),
        sections: Sections {
            cue: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "简单线索".into(),
                    marks: Vec::new(),
                }],
            }],
            content: blocks,
            summary: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "简单总结".into(),
                    marks: Vec::new(),
                }],
            }],
        },
    };

    let saved = workspace
        .save_note_content(&created.session_id, expected.clone())
        .unwrap();
    assert_eq!(saved.session.content, expected);
    let expected_projection = format!(
        "array[`key`] $value remains literal\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
        source, source, source, source, source, source, source, source, source
    );
    assert_eq!(saved.session.text.content, expected_projection);
    assert_eq!(
        workspace
            .save_note_text(&created.session_id, saved.session.text.clone())
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let stored = storage::open_note(&note_path).unwrap();
    assert_eq!(stored.package.document.schema_version, 1);
    assert_eq!(stored.package.content(), expected);
    assert_eq!(
        stored.package.text_projection().content,
        expected_projection
    );
    assert_eq!(
        workspace.open_note(&created.note_id).unwrap().content,
        expected
    );

    let mut restarted = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    restarted.select_library(library).unwrap();
    let reopened = restarted.open_note(&created.note_id).unwrap();
    assert!(reopened.read_only_reason.is_none());
    assert_eq!(reopened.content, expected);

    let mut copied_content = reopened.content.clone();
    copied_content.title = "代码保真副本".into();
    let copy_path = temp.path().join("code-copy.cuenote");
    restarted
        .save_current_content_as(
            &reopened.session_id,
            copied_content.clone(),
            copy_path.clone(),
        )
        .unwrap();
    let copied = storage::open_note(&copy_path).unwrap();
    assert_ne!(copied.package.document.note_id, created.note_id);
    assert_eq!(copied.package.content(), copied_content);
    assert_eq!(copied.package.document.schema_version, 1);
}

#[test]
fn code_and_math_are_rejected_in_side_sections() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery.clone(), default);
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    workspace
        .save_note_content(&created.session_id, created.content.clone())
        .unwrap();
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let before = storage::open_note(&note_path).unwrap();

    let mut side_inline = created.content.clone();
    side_inline.sections.cue.push(Block::Paragraph {
        children: vec![Inline::Text {
            text: "inline".into(),
            marks: vec![Mark::Code],
        }],
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, side_inline)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut side_block = created.content.clone();
    side_block.sections.summary.push(Block::CodeBlock {
        language: Some("rust".into()),
        code: "fn main() {}".into(),
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, side_block)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut math_block = created.content.clone();
    math_block.sections.summary.push(Block::MathBlock {
        latex: "x^2".into(),
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, math_block)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut math_inline = created.content;
    math_inline.sections.cue.push(Block::Paragraph {
        children: vec![Inline::MathInline {
            latex: "x^2".into(),
        }],
    });
    assert_eq!(
        workspace
            .save_note_content(&created.session_id, math_inline)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let after = storage::open_note(&note_path).unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.package, before.package);
}
