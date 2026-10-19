//! Workspace-level resource rejection must leave already valid sources and drafts intact.
use cuenote_lib::note_format::{
    write_package, Block, Inline, NotePackage, PlainText, MAX_PACKAGE_BYTES,
};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use uuid::Uuid;
mod support;

fn external_package(title: &str) -> NotePackage {
    NotePackage::new_plain(
        Uuid::new_v4().to_string(),
        PlainText {
            title: title.into(),
            cue: "线索".into(),
            content: "库外内容".into(),
            summary: "总结".into(),
        },
    )
}

fn external_file(root: &Path, name: &str, title: &str) -> PathBuf {
    let directory = root.join("documents");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    write_package(&mut File::create(&path).unwrap(), &external_package(title)).unwrap();
    path
}

/// Extend a valid package past the limit without allocating its contents in memory.
fn oversized_external_file(root: &Path) -> PathBuf {
    let path = external_file(root, "超限笔记.cuenote", "超限外部笔记");
    let file = File::options().write(true).open(&path).unwrap();
    file.set_len(MAX_PACKAGE_BYTES + 1).unwrap();
    file.sync_all().unwrap();
    path
}

fn stream_sha256(path: &Path) -> (u64, String) {
    let mut file = File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut length = 0u64;
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        length += count as u64;
        hasher.update(&buffer[..count]);
    }
    (length, format!("{:x}", hasher.finalize()))
}

/// Stay below the block-nesting guard while pushing serialized JSON past 64 containers.
fn nested_blockquotes(depth: usize) -> Block {
    let mut block = Block::Paragraph {
        children: vec![Inline::Text {
            text: "深层内容".into(),
            marks: vec![],
        }],
    };
    for _ in 0..depth {
        block = Block::Blockquote {
            blocks: vec![block],
        };
    }
    block
}

#[test]
fn open_external_note_rejects_an_oversized_file_without_touching_the_source_or_current_draft() {
    let temp = support::tempdir("resource-preservation-");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    let valid_path = external_file(temp.path(), "有效笔记.cuenote", "有效外部笔记");
    let active = workspace.open_external_note(valid_path.clone()).unwrap();
    let draft_asset = workspace
        .stage_image(
            &active.session_id,
            include_bytes!("fixtures/pixel.png").to_vec(),
        )
        .unwrap();
    let session_before = workspace.snapshot().unwrap().session.unwrap();
    let valid_before = stream_sha256(&valid_path);
    let recovery_before = workspace.list_recovery_drafts().unwrap().drafts;

    let oversized_path = oversized_external_file(temp.path());
    let oversized_before = stream_sha256(&oversized_path);
    assert_eq!(oversized_before.0, MAX_PACKAGE_BYTES + 1);

    let error = workspace
        .open_external_note(oversized_path.clone())
        .unwrap_err();
    assert_eq!(error.kind, "budget_exceeded");
    assert!(
        error.message.contains("256 MiB") && error.message.contains("安全上限"),
        "unexpected reason: {}",
        error.message
    );

    assert_eq!(stream_sha256(&oversized_path), oversized_before);
    assert_eq!(stream_sha256(&valid_path), valid_before);
    let session_after = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(session_after, session_before);
    assert_eq!(
        workspace.list_recovery_drafts().unwrap().drafts,
        recovery_before
    );
    assert_eq!(
        workspace
            .read_image_asset(&active.session_id, &draft_asset.id)
            .unwrap(),
        include_bytes!("fixtures/pixel.png")
    );
}

#[test]
fn overly_deep_document_rejection_keeps_the_saved_note_and_active_session() {
    let temp = support::tempdir("resource-preservation-");
    let library = temp.path().join("library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    let mut baseline = created.content.clone();
    baseline.title = "深度预算基线".into();
    workspace
        .save_note_content(&created.session_id, baseline)
        .unwrap();
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let saved_before = storage::open_note(&note_path).unwrap();
    let session_before = workspace.snapshot().unwrap().session.unwrap();
    let recovery_before = workspace.list_recovery_drafts().unwrap().drafts;

    let mut attempted = session_before.content.clone();
    attempted.sections.content.push(nested_blockquotes(31));
    let error = workspace
        .save_note_content(&created.session_id, attempted)
        .unwrap_err();
    assert_eq!(error.kind, "format");
    assert!(
        error.message.contains("64 levels"),
        "unexpected reason: {}",
        error.message
    );

    let session_after = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(session_after, session_before);
    let saved_after = storage::open_note(&note_path).unwrap();
    assert_eq!(saved_after.revision, saved_before.revision);
    assert_eq!(saved_after.package, saved_before.package);
    assert_eq!(
        workspace.list_recovery_drafts().unwrap().drafts,
        recovery_before
    );
}
