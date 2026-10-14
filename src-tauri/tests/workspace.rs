use cuenote_lib::note_format::PlainText;
use cuenote_lib::note_format::{AssetManifestEntry, Block, Inline, MediaType, NotePackage};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn workspace_temp() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("workspace-")
        .tempdir_in(base)
        .unwrap()
}

fn notebook_json(id: &str, default: bool) -> String {
    format!(
        r#"{{"schemaVersion":1,"id":"{id}","name":"笔记本","default":{default},"timestamps":{{"createdAt":"2026-10-14T12:00:00.000Z","modifiedAt":"2026-10-14T12:00:00.000Z"}}}}"#
    )
}

const KNOWN_NOTEBOOK_ID: &str = "25ad7e2b-9523-4f0b-9041-426d7f94d924";

fn add_test_image(package: &mut NotePackage) {
    let id = "55ad7e2b-9523-4f0b-9041-426d7f94d924".to_owned();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    package.document.sections.content.push(Block::Image {
        asset_id: id.clone(),
        display_width_px: 1,
    });
    package.manifest.assets.push(AssetManifestEntry {
        id: id.clone(),
        media_type: MediaType::Png,
        byte_length: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        width: 1,
        height: 1,
    });
    package.assets.insert(id, bytes);
}

#[test]
fn choosing_a_chinese_library_persists_the_default_notebook_and_location() {
    let temp = workspace_temp();
    let library = temp.path().join("学习笔记 📚");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    let selected = workspace.select_library(library.clone()).unwrap();
    assert_eq!(
        selected.library.as_ref().unwrap().default_notebook_name,
        "默认笔记本"
    );
    assert!(!selected.library.as_ref().unwrap().path.starts_with(r"\\?\"));
    let initial_metadata = std::fs::read_dir(library.join("notebooks"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(initial_metadata.len(), 1);
    let reopened = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();
    assert_eq!(
        reopened.library.unwrap().path,
        selected.library.unwrap().path
    );
    let current_metadata = std::fs::read_dir(library.join("notebooks"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(initial_metadata, current_metadata);
}

#[test]
fn the_most_recent_library_selection_wins_across_a_restart() {
    let temp = workspace_temp();
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let first_library = temp.path().join("First");
    let second_library = temp.path().join("Second");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    workspace.select_library(first_library).unwrap();
    let selected = workspace.select_library(second_library).unwrap();

    let restored = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();
    assert_eq!(
        restored.library.unwrap().path,
        selected.library.unwrap().path
    );
}

#[test]
fn an_unavailable_remembered_library_does_not_silently_fall_back() {
    let temp = workspace_temp();
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let remembered = temp.path().join("Remembered");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    workspace.select_library(remembered.clone()).unwrap();
    std::fs::rename(&remembered, temp.path().join("Moved")).unwrap();

    let restored = Workspace::new(config, recovery, default)
        .snapshot()
        .unwrap();
    assert!(restored.library.is_none());
    assert!(restored.notice.is_some());
}

#[test]
fn library_and_recovery_roots_cannot_overlap() {
    let temp = workspace_temp();
    let recovery = temp.path().join("recovery");
    let library = recovery.join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery,
        temp.path().join("default"),
    );

    let error = workspace.select_library(library.clone()).unwrap_err();
    assert_eq!(error.kind, "unsafePath");
    assert!(!library.exists());
    assert!(workspace.snapshot().unwrap().library.is_none());
}

#[test]
fn saving_and_reopening_preserves_the_note_identity_and_exact_text() {
    let temp = workspace_temp();
    let library = temp.path().join("Notes");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    workspace.select_library(library).unwrap();

    let first = workspace.new_note().unwrap();

    let text = PlainText {
        title: "同名笔记".into(),
        cue: "关键问题".into(),
        content: "第一行\n第二行".into(),
        summary: "结论".into(),
    };
    let receipt = workspace
        .save_note_text(&first.session_id, text.clone())
        .unwrap();
    assert_eq!(receipt.session.note_id, first.note_id);
    assert_eq!(receipt.session.text, text);
    assert_eq!(receipt.library.notes.len(), 1);
    assert_eq!(receipt.library.notes[0].title, "同名笔记");

    let same_title = workspace.new_note().unwrap();
    assert_ne!(first.note_id, same_title.note_id);
    let second_text = PlainText {
        content: "另一篇内容".into(),
        ..text.clone()
    };
    let second_receipt = workspace
        .save_note_text(&same_title.session_id, second_text)
        .unwrap();
    assert_eq!(second_receipt.library.notes.len(), 2);
    assert!(second_receipt
        .library
        .notes
        .iter()
        .all(|note| note.title == "同名笔记"));

    let mut reopened = Workspace::new(config, recovery, default);
    assert_eq!(reopened.snapshot().unwrap().library.unwrap().notes.len(), 2);
    let opened = reopened.open_note(&first.note_id).unwrap();
    assert_eq!(opened.text, text);
    assert_ne!(opened.note_id, same_title.note_id);
}

#[test]
fn invalid_library_metadata_is_rejected_without_replacing_the_current_session() {
    let temp = workspace_temp();
    let library = temp.path().join("Current");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(config, recovery, default);
    let selected = workspace.select_library(library).unwrap();
    let draft = workspace.new_note().unwrap();

    let invalid = temp.path().join("Invalid");
    std::fs::create_dir_all(invalid.join("notes")).unwrap();
    std::fs::create_dir_all(invalid.join("notebooks")).unwrap();
    let notebook_id = "25ad7e2b-9523-4f0b-9041-426d7f94d924";
    std::fs::write(
        invalid.join("notebooks").join(format!("{notebook_id}.json")),
        r#"{"schemaVersion":1,"id":"25ad7e2b-9523-4f0b-9041-426d7f94d924","name":"默认笔记本","default":true,"timestamps":{"createdAt":"2026-10-14T12:00:00.000Z","modifiedAt":"2026-10-14T12:00:00.000Z"},"unexpected":true}"#,
    )
    .unwrap();

    let error = workspace.select_library(invalid).unwrap_err();
    assert_eq!(error.kind, "libraryMetadata");
    let current = workspace.snapshot().unwrap();
    assert_eq!(
        current.library.unwrap().path,
        selected.library.unwrap().path
    );
    assert_eq!(current.session.unwrap().session_id, draft.session_id);
}

#[test]
fn replacing_the_active_note_session_rejects_a_stale_save() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace.select_library(temp.path().join("Notes")).unwrap();
    let stale = workspace.new_note().unwrap();
    let current = workspace.new_note().unwrap();

    let error = workspace
        .save_note_text(
            &stale.session_id,
            PlainText {
                title: "尚未保存".into(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, "staleSession");
    let snapshot = workspace.snapshot().unwrap();
    assert_eq!(snapshot.session.unwrap().session_id, current.session_id);
}

#[test]
fn an_existing_note_without_notebook_metadata_is_not_assigned_to_a_new_default() {
    let temp = workspace_temp();
    let library = temp.path().join("Legacy");
    std::fs::create_dir_all(library.join("notes")).unwrap();
    std::fs::create_dir_all(library.join("notebooks")).unwrap();
    let note_id = "25ad7e2b-9523-4f0b-9041-426d7f94d924";
    std::fs::write(
        library.join("notes").join(format!("{note_id}.cuenote")),
        b"keep",
    )
    .unwrap();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );

    let error = workspace.select_library(library.clone()).unwrap_err();
    assert_eq!(error.kind, "libraryMetadata");
    assert!(std::fs::read(library.join("notes").join(format!("{note_id}.cuenote"))).is_ok());
    assert_eq!(
        std::fs::read_dir(library.join("notebooks"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn duplicate_notebook_ids_are_rejected_instead_of_overwriting_metadata() {
    let temp = workspace_temp();
    let library = temp.path().join("Duplicate");
    std::fs::create_dir_all(library.join("notes")).unwrap();
    std::fs::create_dir_all(library.join("notebooks")).unwrap();
    let first_id = "25ad7e2b-9523-4f0b-9041-426d7f94d924";
    let second_id = "35ad7e2b-9523-4f0b-9041-426d7f94d924";
    std::fs::write(
        library.join("notebooks").join(format!("{first_id}.json")),
        notebook_json(first_id, true),
    )
    .unwrap();
    std::fs::write(
        library.join("notebooks").join(format!("{second_id}.json")),
        notebook_json(first_id, false),
    )
    .unwrap();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );

    let error = workspace.select_library(library).unwrap_err();
    assert_eq!(error.kind, "libraryMetadata");
    assert!(error.message.contains("重复笔记本身份"));
}

#[test]
fn rich_and_unknown_notebook_sessions_cannot_be_forced_through_plain_text_save() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    std::fs::create_dir_all(library.join("notes")).unwrap();
    std::fs::create_dir_all(library.join("notebooks")).unwrap();
    std::fs::write(
        library
            .join("notebooks")
            .join(format!("{KNOWN_NOTEBOOK_ID}.json")),
        notebook_json(KNOWN_NOTEBOOK_ID, true),
    )
    .unwrap();
    let recovery = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();

    let mut rich = NotePackage::new_plain(
        KNOWN_NOTEBOOK_ID.into(),
        PlainText {
            title: "富内容".into(),
            cue: String::new(),
            content: String::new(),
            summary: String::new(),
        },
    );
    rich.document.sections.content = vec![Block::MathBlock {
        latex: "保留的结构".into(),
    }];
    let rich_id = rich.document.note_id.clone();
    let rich_path = library.join("notes").join(format!("{rich_id}.cuenote"));
    let rich_revision = storage::save_note(&rich_path, &recovery, &rich, None).unwrap();
    let rich_session = workspace.open_note(&rich_id).unwrap();
    assert!(rich_session.read_only_reason.is_none());
    assert_eq!(
        workspace
            .save_note_text(&rich_session.session_id, rich_session.text.clone())
            .unwrap_err()
            .kind,
        "readOnly"
    );
    assert_eq!(
        storage::open_note(&rich_path).unwrap().revision,
        rich_revision
    );

    let unknown_notebook_id = "35ad7e2b-9523-4f0b-9041-426d7f94d924";
    let unknown = NotePackage::new_plain(
        unknown_notebook_id.into(),
        PlainText {
            title: "未知笔记本归属".into(),
            cue: String::new(),
            content: "原始内容".into(),
            summary: String::new(),
        },
    );
    let unknown_id = unknown.document.note_id.clone();
    let unknown_path = library.join("notes").join(format!("{unknown_id}.cuenote"));
    let unknown_revision = storage::save_note(&unknown_path, &recovery, &unknown, None).unwrap();
    let unknown_session = workspace.open_note(&unknown_id).unwrap();
    assert!(unknown_session
        .read_only_reason
        .as_deref()
        .unwrap()
        .contains("未知笔记本"));
    assert_eq!(
        workspace
            .save_note_text(&unknown_session.session_id, unknown_session.text.clone())
            .unwrap_err()
            .kind,
        "readOnly"
    );
    assert_eq!(
        storage::open_note(&unknown_path).unwrap().revision,
        unknown_revision
    );
}

#[test]
fn duplicate_note_identities_are_warned_and_listed_only_once() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    std::fs::create_dir_all(library.join("notes")).unwrap();
    std::fs::create_dir_all(library.join("notebooks")).unwrap();
    std::fs::write(
        library
            .join("notebooks")
            .join(format!("{KNOWN_NOTEBOOK_ID}.json")),
        notebook_json(KNOWN_NOTEBOOK_ID, true),
    )
    .unwrap();
    let recovery = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let note = NotePackage::new_plain(
        KNOWN_NOTEBOOK_ID.into(),
        PlainText {
            title: "重复身份".into(),
            cue: String::new(),
            content: "内容".into(),
            summary: String::new(),
        },
    );
    let note_id = note.document.note_id.clone();
    let original = library.join("notes").join(format!("{note_id}.cuenote"));
    storage::save_note(&original, &recovery, &note, None).unwrap();
    let second_name = "45ad7e2b-9523-4f0b-9041-426d7f94d924";
    std::fs::copy(
        &original,
        library.join("notes").join(format!("{second_name}.cuenote")),
    )
    .unwrap();

    let view = workspace.snapshot().unwrap().library.unwrap();
    assert_eq!(view.notes.len(), 1);
    assert_eq!(view.notes[0].note_id, note_id);
    assert!(view
        .warnings
        .iter()
        .any(|warning| warning.contains("重复笔记身份")));
}

#[test]
fn saving_unchanged_text_keeps_the_content_and_organization_timestamps() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let recovery = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let note = workspace.new_note().unwrap();
    let text = PlainText {
        title: "不变标题".into(),
        cue: "线索".into(),
        content: "正文".into(),
        summary: "总结".into(),
    };
    workspace
        .save_note_text(&note.session_id, text.clone())
        .unwrap();
    let path = library
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let before = storage::open_note(&path)
        .unwrap()
        .package
        .document
        .timestamps;
    let opened = workspace.open_note(&note.note_id).unwrap();
    workspace.save_note_text(&opened.session_id, text).unwrap();
    let after = storage::open_note(&path)
        .unwrap()
        .package
        .document
        .timestamps;

    assert_eq!(after.content_modified_at, before.content_modified_at);
    assert_eq!(
        after.organization_modified_at,
        before.organization_modified_at
    );
    assert_eq!(after.created_at, before.created_at);
}

#[test]
fn recovery_lists_after_restart_and_copy_restore_preserves_full_package() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut original_workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    original_workspace.select_library(library.clone()).unwrap();
    let blank = original_workspace.new_note().unwrap();
    let original_text = PlainText {
        title: "原始笔记".into(),
        cue: "原线索".into(),
        content: "原正文".into(),
        summary: "原总结".into(),
    };
    original_workspace
        .save_note_text(&blank.session_id, original_text)
        .unwrap();
    let original_path = library
        .join("notes")
        .join(format!("{}.cuenote", blank.note_id));
    let original_opened = storage::open_note(&original_path).unwrap();

    let mut draft = original_opened.package.clone();
    draft
        .replace_plain_text(PlainText {
            title: "待恢复草稿".into(),
            cue: "恢复线索".into(),
            content: "保留完整结构与附件".into(),
            summary: "恢复总结".into(),
        })
        .unwrap();
    draft.document.sections.content.push(Block::Heading1 {
        children: vec![Inline::Text {
            text: "富内容也要保留".into(),
            marks: Vec::new(),
        }],
    });
    add_test_image(&mut draft);
    let conflict =
        storage::save_note(&original_path, &recovery, &draft, Some("stale-revision")).unwrap_err();
    assert_eq!(conflict.kind, "conflict");
    assert_eq!(
        storage::open_note(&original_path).unwrap().revision,
        original_opened.revision
    );

    let copy_directory = temp.path().join("safe-copies");
    std::fs::create_dir_all(&copy_directory).unwrap();
    let mut without_library = Workspace::new(
        temp.path().join("unselected-settings"),
        recovery.clone(),
        default.clone(),
    );
    let listed = without_library.list_recovery_drafts().unwrap();
    assert_eq!(listed.warnings, Vec::<String>::new());
    assert_eq!(listed.drafts.len(), 1);
    let recovery_id = listed.drafts[0].recovery_id.clone();
    assert_eq!(listed.drafts[0].note_id, blank.note_id);
    assert_eq!(listed.drafts[0].title, "待恢复草稿");
    assert_eq!(
        listed.drafts[0].revision,
        storage::open_note(Path::new(conflict.recovery_path.as_deref().unwrap(),))
            .unwrap()
            .revision
    );

    let invalid_revision_id = format!(
        "{}:{}",
        recovery_id.rsplit_once(':').unwrap().0,
        "0".repeat(64)
    );
    let changed = without_library
        .save_recovery_as(
            &invalid_revision_id,
            copy_directory.join("must-not-be-created.cuenote"),
        )
        .unwrap_err();
    assert_eq!(changed.kind, "recoveryChanged");

    let into_recovery = without_library
        .save_recovery_as(&recovery_id, recovery.join("blocked.cuenote"))
        .unwrap_err();
    assert_eq!(into_recovery.kind, "unsafePath");
    assert!(!recovery.join("blocked.cuenote").exists());

    let recovery_copy_path = copy_directory.join("recovery-copy.cuenote");
    let recovery_copy = without_library
        .save_recovery_as(&recovery_id, recovery_copy_path.clone())
        .unwrap();
    let recovery_copy_note = storage::open_note(&recovery_copy_path).unwrap();
    assert_eq!(recovery_copy.revision, recovery_copy_note.revision);
    assert_ne!(recovery_copy_note.package.document.note_id, blank.note_id);
    assert_eq!(
        recovery_copy_note.package.document.organization.notebook_id,
        draft.document.organization.notebook_id
    );
    let mut expected_recovery_copy = draft.clone();
    expected_recovery_copy.document.note_id = recovery_copy_note.package.document.note_id.clone();
    expected_recovery_copy.document.timestamps.created_at = recovery_copy_note
        .package
        .document
        .timestamps
        .created_at
        .clone();
    expected_recovery_copy
        .document
        .timestamps
        .organization_modified_at = recovery_copy_note
        .package
        .document
        .timestamps
        .organization_modified_at
        .clone();
    assert_eq!(recovery_copy_note.package, expected_recovery_copy);
    assert_eq!(
        without_library.list_recovery_drafts().unwrap().drafts.len(),
        1
    );

    let mut restarted = Workspace::new(config, recovery.clone(), default);
    restarted.snapshot().unwrap();
    let into_library = restarted
        .save_recovery_as(&recovery_id, library.join("notes/blocked.cuenote"))
        .unwrap_err();
    assert_eq!(into_library.kind, "unsafePath");
    assert!(!library.join("notes/blocked.cuenote").exists());
    let active_original = restarted.open_note(&blank.note_id).unwrap();
    let current_copy_path = copy_directory.join("current-copy.cuenote");
    let current_copy_text = PlainText {
        title: "另存编辑".into(),
        cue: "新的线索".into(),
        content: "原会话不应改变".into(),
        summary: "副本总结".into(),
    };
    let current_copy = restarted
        .save_current_as(
            &active_original.session_id,
            current_copy_text.clone(),
            current_copy_path.clone(),
        )
        .unwrap();
    let current_copy_note = storage::open_note(&current_copy_path).unwrap();
    assert_eq!(current_copy.revision, current_copy_note.revision);
    assert_eq!(
        current_copy_note.package.text_projection(),
        current_copy_text
    );
    assert_ne!(current_copy_note.package.document.note_id, blank.note_id);
    let unchanged_session = restarted.snapshot().unwrap().session.unwrap();
    assert_eq!(unchanged_session.session_id, active_original.session_id);
    assert_eq!(unchanged_session.revision, active_original.revision);
    assert_eq!(
        storage::open_note(&original_path).unwrap().revision,
        original_opened.revision
    );

    let restored = restarted.restore_recovery_as_copy(&recovery_id).unwrap();
    assert_ne!(restored.session.note_id, blank.note_id);
    let restored_path = library
        .join("notes")
        .join(format!("{}.cuenote", restored.session.note_id));
    let restored_note = storage::open_note(&restored_path).unwrap();
    assert_eq!(
        restored_note.package.document.sections,
        draft.document.sections
    );
    assert_eq!(restored_note.package.manifest, draft.manifest);
    assert_eq!(restored_note.package.assets, draft.assets);
    assert_eq!(restored_note.package.document.title, draft.document.title);
    assert_ne!(
        restored_note.package.document.timestamps.created_at,
        draft.document.timestamps.created_at
    );
    assert_ne!(
        restored_note
            .package
            .document
            .timestamps
            .organization_modified_at,
        draft.document.timestamps.organization_modified_at
    );
    assert_eq!(restarted.list_recovery_drafts().unwrap().drafts.len(), 0);
    assert_eq!(
        storage::open_note(&original_path).unwrap().revision,
        original_opened.revision
    );
}

#[test]
fn unreadable_recovery_draft_is_warned_about_and_left_in_place() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let config = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(config.clone(), recovery.clone(), default.clone());
    workspace.select_library(library.clone()).unwrap();
    let blank = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &blank.session_id,
            PlainText {
                title: "原件".into(),
                cue: String::new(),
                content: "原始内容".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let path = library
        .join("notes")
        .join(format!("{}.cuenote", blank.note_id));
    let original = storage::open_note(&path).unwrap();
    let mut draft = original.package.clone();
    draft
        .replace_plain_text(PlainText {
            title: "损坏草稿".into(),
            cue: String::new(),
            content: "损坏后仍应保留".into(),
            summary: String::new(),
        })
        .unwrap();
    let error = storage::save_note(&path, &recovery, &draft, Some("stale")).unwrap_err();
    let recovery_path = PathBuf::from(error.recovery_path.unwrap());
    std::fs::write(&recovery_path, b"invalid package bytes").unwrap();

    let mut restarted = Workspace::new(config, recovery, default);
    let listing = restarted.list_recovery_drafts().unwrap();
    assert!(listing.drafts.is_empty());
    assert!(listing
        .warnings
        .iter()
        .any(|warning| warning.contains("校验失败")));
    assert_eq!(
        std::fs::read(&recovery_path).unwrap(),
        b"invalid package bytes"
    );
    assert_eq!(
        storage::open_note(&path).unwrap().revision,
        original.revision
    );
}

#[test]
fn missing_library_target_is_checkpointed_when_recovery_is_available() {
    let temp = workspace_temp();
    let library = temp.path().join("Library");
    let recovery = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let blank = workspace.new_note().unwrap();
    std::fs::remove_dir(library.join("notes")).unwrap();

    let text = PlainText {
        title: "目录暂不可用".into(),
        cue: String::new(),
        content: "恢复快照仍需保存".into(),
        summary: String::new(),
    };
    let error = workspace
        .save_note_text(&blank.session_id, text.clone())
        .unwrap_err();

    assert_eq!(error.kind, "io");
    let recovery_path = error
        .recovery_path
        .as_deref()
        .expect("available external recovery must preserve the editor contents");
    assert!(error.recovery_error.is_none());
    let saved = storage::open_note(Path::new(recovery_path)).unwrap();
    assert_eq!(saved.package.text_projection(), text);
    assert_eq!(saved.package.document.note_id, blank.note_id);
    assert!(!library.join("notes").exists());
    assert_eq!(workspace.list_recovery_drafts().unwrap().drafts.len(), 1);
}
