use cuenote_lib::note_format::{Block, Inline, NoteContent, PlainText, Sections};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::path::Path;

fn setup() -> (tempfile::TempDir, Workspace, String, String) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("save-failures-")
        .tempdir_in(parent)
        .unwrap();
    let root = temp.path();
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    );
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    workspace
        .save_note_text(&session.session_id, text("原件已保存"))
        .unwrap();
    (temp, workspace, session.session_id, session.note_id)
}

fn text(content: &str) -> PlainText {
    PlainText {
        title: "保存故障".into(),
        cue: "线索".into(),
        content: content.into(),
        summary: "总结".into(),
    }
}

fn rich_content(content: &str) -> NoteContent {
    NoteContent {
        title: "独立富内容副本".into(),
        sections: Sections {
            cue: Vec::new(),
            content: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: content.into(),
                    marks: Vec::new(),
                }],
            }],
            summary: Vec::new(),
        },
    }
}

#[cfg(windows)]
#[test]
fn read_only_original_remains_valid_and_its_unsaved_draft_is_found_after_restart() {
    let (temp, mut workspace, session_id, note_id) = setup();
    let root = temp.path();
    let path = root
        .join("library/notes")
        .join(format!("{note_id}.cuenote"));
    let before = storage::open_note(&path).unwrap();
    // Restore the exact original attributes even if an assertion fails.
    struct RestorePermissions(std::path::PathBuf, std::fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    let _restore = RestorePermissions(path.clone(), permissions.clone());
    let mut readonly = permissions;
    readonly.set_readonly(true);
    std::fs::set_permissions(&path, readonly).unwrap();

    let error = workspace
        .save_note_text(&session_id, text("只读失败后草稿"))
        .unwrap_err();
    assert!(error.recovery_path.is_some(), "{error:?}");
    assert_eq!(storage::open_note(&path).unwrap().revision, before.revision);
    let mut restarted = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    );
    restarted.snapshot().unwrap();
    let list = restarted.list_recovery_drafts().unwrap();
    assert_eq!(list.drafts.len(), 1);
    let restored = restarted
        .restore_recovery_as_copy(&list.drafts[0].recovery_id)
        .unwrap();
    assert_eq!(restored.session.text.content, "只读失败后草稿");
    assert_ne!(restored.session.note_id, note_id);
    assert_eq!(storage::open_note(&path).unwrap().revision, before.revision);
}

#[test]
fn missing_library_and_unavailable_recovery_still_allow_a_verified_independent_copy() {
    let (temp, mut workspace, session_id, note_id) = setup();
    let root = temp.path();
    let original = root
        .join("library/notes")
        .join(format!("{note_id}.cuenote"));
    let before = storage::open_note(&original).unwrap();
    // Both directories belong exclusively to this newly created fixture. The
    // active Windows baseline lease keeps a lock-file handle under recovery,
    // so simulate unavailable recovery snapshots at the drafts subdirectory.
    std::fs::rename(root.join("library"), root.join("library-away")).unwrap();
    std::fs::rename(
        root.join("recovery/drafts"),
        root.join("recovery/drafts-away"),
    )
    .unwrap();
    std::fs::write(
        root.join("recovery/drafts"),
        b"unavailable recovery drafts directory",
    )
    .unwrap();

    let draft = text("两个位置均不可达时的最新输入");
    let error = workspace
        .save_note_text(&session_id, draft.clone())
        .unwrap_err();
    assert!(error.recovery_path.is_none());
    assert!(error.recovery_error.is_some(), "{error:?}");
    assert!(workspace.list_recovery_drafts().is_err());
    let outside = root.join("safe-location");
    std::fs::create_dir(&outside).unwrap();
    let destination = outside.join("独立副本.cuenote");
    let copy = workspace
        .save_current_as(&session_id, draft, destination.clone())
        .unwrap();
    let opened = storage::open_note(&destination).unwrap();
    assert_eq!(opened.revision, copy.revision);
    assert_eq!(
        opened.package.text_projection().content,
        "两个位置均不可达时的最新输入"
    );
    assert_ne!(opened.package.document.note_id, note_id);
    let rich = rich_content("恢复区不可用时仍可另存的富内容");
    let rich_destination = outside.join("独立富内容副本.cuenote");
    workspace
        .save_current_content_as(&session_id, rich.clone(), rich_destination.clone())
        .unwrap();
    let rich_copy = storage::open_note(&rich_destination).unwrap();
    assert_eq!(rich_copy.package.content(), rich);
    assert_ne!(rich_copy.package.document.note_id, note_id);
    let untouched = root
        .join("library-away/notes")
        .join(format!("{note_id}.cuenote"));
    assert_eq!(
        storage::open_note(&untouched).unwrap().revision,
        before.revision
    );
    // A second export to the same name must reject without replacing the copy.
    assert!(workspace
        .save_current_as(&session_id, text("不能覆盖副本"), destination.clone())
        .is_err());
    assert_eq!(
        storage::open_note(&destination).unwrap().revision,
        opened.revision
    );
}

#[test]
fn can_switch_away_from_a_missing_library_without_a_restore_pointer() {
    let (temp, mut workspace, _session_id, _note_id) = setup();
    let root = temp.path();
    let library = workspace.library_path().unwrap();
    std::fs::rename(&library, root.join("library-away")).unwrap();

    let selected = root.join("replacement-library");
    workspace.select_library(selected.clone()).unwrap();
    assert_eq!(
        workspace.library_path().unwrap(),
        std::fs::canonicalize(&selected).unwrap()
    );
    assert!(selected.join("notes").is_dir());
}
