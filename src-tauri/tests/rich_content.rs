use cuenote_lib::note_format::{
    AssetManifestEntry, Block, Inline, ListItem, Mark, MediaType, MindMapNode, NoteContent,
    NotePackage, PlainText, Sections, TableRow, TaskListItem,
};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
mod support;

fn workspace_temp() -> tempfile::TempDir {
    support::tempdir("rich-content-")
}

fn content() -> NoteContent {
    let bold = vec![Mark::Bold];
    NoteContent {
        title: "结构化笔记".into(),
        sections: Sections {
            cue: vec![Block::Heading2 {
                children: vec![Inline::Text {
                    text: "线索".into(),
                    marks: bold.clone(),
                }],
            }],
            content: vec![
                Block::Paragraph {
                    children: vec![
                        Inline::Text {
                            text: "粗体".into(),
                            marks: vec![Mark::Bold],
                        },
                        Inline::Text {
                            text: "斜体".into(),
                            marks: vec![Mark::Italic],
                        },
                        Inline::Text {
                            text: "删除线".into(),
                            marks: vec![Mark::Strike],
                        },
                        Inline::Text {
                            text: "链接".into(),
                            marks: vec![Mark::Link {
                                href: "https://example.com/note".into(),
                            }],
                        },
                        Inline::HardBreak,
                        Inline::Text {
                            text: "换行后".into(),
                            marks: Vec::new(),
                        },
                    ],
                },
                Block::Heading1 {
                    children: vec![Inline::Text {
                        text: "一级标题".into(),
                        marks: Vec::new(),
                    }],
                },
                Block::Heading2 {
                    children: vec![Inline::Text {
                        text: "二级标题".into(),
                        marks: Vec::new(),
                    }],
                },
                Block::Heading3 {
                    children: vec![Inline::Text {
                        text: "三级标题".into(),
                        marks: Vec::new(),
                    }],
                },
                Block::OrderedList {
                    start: 3,
                    items: vec![ListItem {
                        blocks: vec![Block::Paragraph {
                            children: vec![Inline::Text {
                                text: "有序项".into(),
                                marks: Vec::new(),
                            }],
                        }],
                    }],
                },
                Block::BulletList {
                    items: vec![ListItem {
                        blocks: vec![Block::Paragraph {
                            children: vec![Inline::Text {
                                text: "项目符号项".into(),
                                marks: Vec::new(),
                            }],
                        }],
                    }],
                },
                Block::TaskList {
                    items: vec![TaskListItem {
                        checked: true,
                        blocks: vec![Block::Paragraph {
                            children: vec![Inline::Text {
                                text: "已完成任务".into(),
                                marks: Vec::new(),
                            }],
                        }],
                    }],
                },
                Block::Blockquote {
                    blocks: vec![Block::Paragraph {
                        children: vec![Inline::Text {
                            text: "引用".into(),
                            marks: Vec::new(),
                        }],
                    }],
                },
                Block::HorizontalRule,
                Block::Table {
                    rows: vec![
                        TableRow::Header {
                            cells: vec![
                                vec![Inline::Text {
                                    text: "列一".into(),
                                    marks: Vec::new(),
                                }],
                                vec![Inline::Text {
                                    text: "列二".into(),
                                    marks: Vec::new(),
                                }],
                            ],
                        },
                        TableRow::Body {
                            cells: vec![
                                vec![Inline::Text {
                                    text: "单元格".into(),
                                    marks: vec![Mark::Bold],
                                }],
                                vec![Inline::Text {
                                    text: "第二格".into(),
                                    marks: Vec::new(),
                                }],
                            ],
                        },
                    ],
                },
            ],
            summary: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "结论".into(),
                    marks: bold,
                }],
            }],
        },
    }
}

fn workspace_paths(temp: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
        temp.path().join("Library"),
    )
}

fn plain_package(notebook_id: &str, title: &str) -> NotePackage {
    NotePackage::new_plain(
        notebook_id.to_owned(),
        PlainText {
            title: title.to_owned(),
            cue: String::new(),
            content: "原始内容".into(),
            summary: String::new(),
        },
    )
}

fn rich_package(notebook_id: &str) -> NotePackage {
    let mut package = plain_package(notebook_id, "只读富内容");
    let asset_id = "55ad7e2b-9523-4f0b-9041-426d7f94d924".to_owned();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    package.document.sections.content.push(Block::Image {
        asset_id: asset_id.clone(),
        display_width_px: 1,
    });
    package.manifest.assets.push(AssetManifestEntry {
        id: asset_id.clone(),
        media_type: MediaType::Png,
        byte_length: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        width: 1,
        height: 1,
    });
    package.assets.insert(asset_id, bytes);
    let root_id = "65ad7e2b-9523-4f0b-9041-426d7f94d924".to_owned();
    let child_id = "75ad7e2b-9523-4f0b-9041-426d7f94d924".to_owned();
    package.document.sections.content.push(Block::MindMap {
        map_id: "85ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
        root_id: root_id.clone(),
        nodes: vec![
            MindMapNode {
                id: root_id,
                text: "根节点".into(),
                children: vec![child_id.clone()],
            },
            MindMapNode {
                id: child_id,
                text: "子节点".into(),
                children: Vec::new(),
            },
        ],
    });
    package
}

#[test]
fn basic_structured_content_survives_save_open_and_restart() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings.clone(), recovery.clone(), default.clone());
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    let expected = content();

    let saved = workspace
        .save_note_content(&created.session_id, expected.clone())
        .unwrap();
    assert_eq!(saved.session.content, expected);
    assert_eq!(saved.session.text.title, expected.title);

    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let stored = storage::open_note(&note_path).unwrap();
    assert_eq!(stored.package.content(), expected);
    assert_eq!(saved.session.text, stored.package.text_projection());

    let reopened = workspace.open_note(&created.note_id).unwrap();
    assert_eq!(reopened.content, expected);
    assert_eq!(reopened.text, stored.package.text_projection());

    let mut restarted = Workspace::new(settings, recovery, default);
    restarted.select_library(library).unwrap();
    let after_restart = restarted.open_note(&created.note_id).unwrap();
    assert_eq!(after_restart.content, expected);
    assert_eq!(after_restart.text, stored.package.text_projection());
}

#[test]
fn structured_save_preserves_note_identity_organization_and_timestamps() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery.clone(), default);
    let selected = workspace.select_library(library.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let mut package = plain_package(&notebook_id, "保存前");
    package.document.organization.pinned = true;
    package.document.timestamps.created_at = "2020-01-02T03:04:05.000Z".into();
    package.document.timestamps.content_modified_at = "2099-01-02T03:04:05.000Z".into();
    package.document.timestamps.organization_modified_at = "2021-02-03T04:05:06.000Z".into();
    let note_id = package.document.note_id.clone();
    let note_path = library.join("notes").join(format!("{note_id}.cuenote"));
    storage::save_note(&note_path, &recovery, &package, None).unwrap();
    let before = storage::open_note(&note_path).unwrap().package;
    let session = workspace.open_note(&note_id).unwrap();
    let mut edited = session.content;
    edited.title = "保存后".into();

    workspace
        .save_note_content(&session.session_id, edited.clone())
        .unwrap();

    let after = storage::open_note(&note_path).unwrap().package;
    assert_eq!(after.content(), edited);
    assert_eq!(after.document.note_id, before.document.note_id);
    assert_eq!(after.document.organization, before.document.organization);
    assert_eq!(
        after.document.timestamps.created_at,
        before.document.timestamps.created_at
    );
    assert_eq!(
        after.document.timestamps.organization_modified_at,
        before.document.timestamps.organization_modified_at
    );
    assert!(
        after.document.timestamps.content_modified_at
            > before.document.timestamps.content_modified_at
    );
    assert_eq!(after.manifest, before.manifest);
    assert_eq!(after.assets, before.assets);

    let saved_session = workspace
        .save_note_content(&session.session_id, edited)
        .unwrap()
        .session;
    assert_eq!(saved_session.content, after.content());
    let unchanged = storage::open_note(&note_path).unwrap().package;
    assert_eq!(
        unchanged.document.timestamps.content_modified_at,
        after.document.timestamps.content_modified_at
    );
}

#[test]
fn legacy_plain_save_refreshes_the_structured_session_snapshot() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery, default);
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    let text = PlainText {
        title: "纯文本标题".into(),
        cue: "线索".into(),
        content: "正文".into(),
        summary: "总结".into(),
    };

    let saved = workspace.save_note_text(&created.session_id, text).unwrap();

    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    let stored = storage::open_note(&note_path).unwrap();
    assert_eq!(saved.session.content, stored.package.content());
}

#[test]
fn malformed_or_unsupported_content_does_not_replace_the_saved_note() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery.clone(), default);
    let selected = workspace.select_library(library.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let package = plain_package(&notebook_id, "原件");
    let note_id = package.document.note_id.clone();
    let note_path = library.join("notes").join(format!("{note_id}.cuenote"));
    storage::save_note(&note_path, &recovery, &package, None).unwrap();
    let before = storage::open_note(&note_path).unwrap();
    let session = workspace.open_note(&note_id).unwrap();
    let drafts_before = workspace.list_recovery_drafts().unwrap().drafts;

    let mut malformed = session.content.clone();
    malformed.sections.content.push(Block::Table {
        rows: vec![TableRow::Body {
            cells: vec![vec![Inline::Text {
                text: "缺少表头".into(),
                marks: Vec::new(),
            }]],
        }],
    });
    let malformed_error = workspace
        .save_note_content(&session.session_id, malformed)
        .unwrap_err();
    assert_eq!(malformed_error.kind, "format");

    let mut nested_map = session.content.clone();
    nested_map.sections.content.push(Block::BulletList {
        items: vec![ListItem {
            blocks: vec![Block::MindMap {
                map_id: "85ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
                root_id: "65ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
                nodes: vec![MindMapNode {
                    id: "65ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
                    text: "列表内不可编辑的导图".into(),
                    children: Vec::new(),
                }],
            }],
        }],
    });
    let unsupported_error = workspace
        .save_note_content(&session.session_id, nested_map)
        .unwrap_err();
    assert_eq!(unsupported_error.kind, "readOnly");

    let after = storage::open_note(&note_path).unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.package, before.package);
    assert_eq!(
        workspace.list_recovery_drafts().unwrap().drafts,
        drafts_before
    );
}

#[test]
fn stale_structured_save_preserves_external_version_and_records_recovery() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery.clone(), default);
    let selected = workspace.select_library(library.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let package = plain_package(&notebook_id, "初始版本");
    let note_id = package.document.note_id.clone();
    let note_path = library.join("notes").join(format!("{note_id}.cuenote"));
    storage::save_note(&note_path, &recovery, &package, None).unwrap();
    let opened = workspace.open_note(&note_id).unwrap();
    let baseline = storage::open_note(&note_path).unwrap();

    let mut external_package = baseline.package.clone();
    let mut external_content = external_package.content();
    external_content.title = "其他操作已保存".into();
    external_package.replace_content(external_content.clone());
    let external_revision = storage::save_note(
        &note_path,
        &recovery,
        &external_package,
        Some(&baseline.revision),
    )
    .unwrap();
    let mut stale_content = content();
    stale_content.title = "本地待恢复".into();

    let error = workspace
        .save_note_content(&opened.session_id, stale_content.clone())
        .unwrap_err();

    assert_eq!(error.kind, "conflict");
    let current = storage::open_note(&note_path).unwrap();
    assert_eq!(current.revision, external_revision);
    assert_eq!(current.package.content(), external_content);
    let drafts = workspace.list_recovery_drafts().unwrap().drafts;
    let draft = drafts
        .iter()
        .find(|draft| draft.note_id == note_id && draft.title == stale_content.title)
        .expect("the complete structured draft must be available for recovery");
    let restored = workspace
        .restore_recovery_as_copy(&draft.recovery_id)
        .unwrap();
    assert_ne!(restored.session.note_id, note_id);
    assert_eq!(restored.session.content, stale_content);
}

#[test]
fn read_only_rich_content_can_be_saved_as_a_full_independent_copy() {
    let temp = workspace_temp();
    let (settings, recovery, default, library) = workspace_paths(&temp);
    let mut workspace = Workspace::new(settings, recovery.clone(), default);
    let selected = workspace.select_library(library.clone()).unwrap();
    let notebook_id = selected.library.unwrap().default_notebook_id.unwrap();
    let mut package = rich_package(&notebook_id);
    package.document.organization.notebook_id = Some("35ad7e2b-9523-4f0b-9041-426d7f94d924".into());
    package.document.organization.pinned = true;
    let note_id = package.document.note_id.clone();
    let note_path = library.join("notes").join(format!("{note_id}.cuenote"));
    storage::save_note(&note_path, &recovery, &package, None).unwrap();
    let before = storage::open_note(&note_path).unwrap();
    let session = workspace.open_note(&note_id).unwrap();
    assert!(session
        .read_only_reason
        .as_deref()
        .unwrap()
        .contains("未知笔记本"));
    assert_eq!(
        workspace
            .save_note_content(&session.session_id, session.content.clone())
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let copy_path = temp.path().join("完整副本.cuenote");
    workspace
        .save_current_content_as(
            &session.session_id,
            session.content.clone(),
            copy_path.clone(),
        )
        .unwrap();

    let copied = storage::open_note(&copy_path).unwrap();
    assert_ne!(copied.package.document.note_id, note_id);
    assert_eq!(copied.package.document.title, before.package.document.title);
    assert_eq!(
        copied.package.document.sections,
        before.package.document.sections
    );
    assert_eq!(copied.package.manifest, before.package.manifest);
    assert_eq!(copied.package.assets, before.package.assets);
    assert_eq!(
        copied.package.document.organization.notebook_id,
        Some(notebook_id)
    );
    assert!(!copied.package.document.organization.pinned);
    assert_eq!(copied.package.document.organization.trashed_at, None);

    let mut changed = session.content;
    changed.title = "改动后的富内容".into();
    let changed_copy_path = temp.path().join("编辑后副本.cuenote");
    workspace
        .save_current_content_as(
            &session.session_id,
            changed.clone(),
            changed_copy_path.clone(),
        )
        .unwrap();
    let changed_copy = storage::open_note(&changed_copy_path).unwrap();
    assert_eq!(changed_copy.package.content(), changed);
    assert_ne!(changed_copy.package.document.note_id, note_id);
    let after = storage::open_note(&note_path).unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.package, before.package);
}
