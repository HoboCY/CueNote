use cuenote_lib::note_format::{write_package, Block, Inline, MindMapNode, PlainText};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn reopen(root: &Path) -> Workspace {
    Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    )
}

fn setup() -> (
    tempfile::TempDir,
    Workspace,
    cuenote_lib::workspace::NoteSession,
) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("external-")
        .tempdir_in(parent)
        .unwrap();
    let mut workspace = reopen(temp.path());
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let receipt = workspace
        .save_note_text(
            &session.session_id,
            PlainText {
                title: "外部变化".into(),
                cue: "旧线索".into(),
                content: "已持有的旧版 A".into(),
                summary: "旧总结".into(),
            },
        )
        .unwrap();
    (temp, workspace, receipt.session)
}

fn write_external(path: &Path, content: &str) {
    let mut package = storage::open_note(path).unwrap().package;
    let mut text = package.text_projection();
    text.content = content.into();
    package.replace_plain_text(text).unwrap();
    write_package_file(path, &package);
}

fn write_package_file(path: &Path, package: &cuenote_lib::note_format::NotePackage) {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap();
    write_package(&mut file, package).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
}

fn note_path(temp: &tempfile::TempDir, note_id: &str) -> PathBuf {
    temp.path()
        .join("library/notes")
        .join(format!("{note_id}.cuenote"))
}

fn draft_content(
    session: &cuenote_lib::workspace::NoteSession,
    text: &str,
) -> cuenote_lib::note_format::NoteContent {
    let mut content = session.content.clone();
    content.sections.content.push(Block::Paragraph {
        children: vec![Inline::Text {
            text: text.to_owned(),
            marks: Vec::new(),
        }],
    });
    content
}

#[test]
fn clean_external_refresh_preserves_the_held_version_before_adopting_the_new_one() {
    let (temp, mut workspace, session) = setup();
    let path = temp
        .path()
        .join("library/notes")
        .join(format!("{}.cuenote", session.note_id));
    write_external(&path, "外部新版 B");
    let probe = workspace
        .probe_external_changes(Some(&session.session_id))
        .unwrap();
    assert_eq!(probe.status, "changed");
    // The probe is observational: no editor input is replaced until reconciliation.
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().session_id,
        session.session_id
    );
    let reconciled = workspace
        .reconcile_external_changes(&session.session_id, session.content)
        .unwrap();
    assert_eq!(reconciled.status, "refreshed");
    let refreshed = reconciled.session.unwrap();
    assert_eq!(refreshed.text.content, "外部新版 B");
    assert_ne!(refreshed.session_id, session.session_id);
    // Removing the source proves the old version is independently usable after restart.
    std::fs::remove_file(path).unwrap();
    let mut restarted = reopen(temp.path());
    restarted.snapshot().unwrap();
    let versions = restarted.list_preserved_versions().unwrap();
    let mut contents = Vec::new();
    for version in versions.versions {
        let preview = restarted
            .open_preserved_version(&version.version_id)
            .unwrap();
        assert!(preview.read_only_reason.is_some());
        contents.push(preview.text.content);
    }
    assert!(contents.iter().any(|text| text == "已持有的旧版 A"));
    assert!(contents.iter().any(|text| text == "外部新版 B"));
}

#[test]
fn dirty_conflict_keeps_every_acquired_version_and_preserves_further_edits_before_leaving() {
    let (temp, mut workspace, session) = setup();
    let path = temp
        .path()
        .join("library/notes")
        .join(format!("{}.cuenote", session.note_id));
    let original_bytes = std::fs::read(&path).unwrap();
    let mut local = storage::open_note(&path).unwrap().package;
    let mut text = local.text_projection();
    text.content = "尚未保存的本地草稿 C".into();
    local.replace_plain_text(text.clone()).unwrap();
    write_external(&path, "来自其他程序的 B");

    let result = workspace
        .reconcile_external_changes(&session.session_id, local.content())
        .unwrap();
    assert_eq!(result.status, "conflict");
    assert!(result.session.is_none());
    assert_eq!(
        storage::open_note(&path)
            .unwrap()
            .package
            .text_projection()
            .content,
        "来自其他程序的 B"
    );
    assert!(workspace
        .save_note_content(&session.session_id, local.content())
        .is_err());

    // The remote writer can revert to the exact original fingerprint while the
    // user keeps editing. An explicit leave must still preserve the newest draft.
    std::fs::write(&path, &original_bytes).unwrap();
    text.content = "冲突后继续编辑的草稿 C2".into();
    local.replace_plain_text(text).unwrap();
    let protected = workspace
        .reconcile_external_changes(&session.session_id, local.content())
        .unwrap();
    assert_eq!(protected.status, "conflict");

    let mut found = std::collections::BTreeMap::new();
    for version in workspace.list_preserved_versions().unwrap().versions {
        let preview = workspace
            .open_preserved_version(&version.version_id)
            .unwrap();
        assert!(preview.read_only_reason.is_some());
        found.insert(preview.text.content, version.version_id);
    }
    for expected in [
        "已持有的旧版 A",
        "来自其他程序的 B",
        "尚未保存的本地草稿 C",
        "冲突后继续编辑的草稿 C2",
    ] {
        assert!(
            found.contains_key(expected),
            "missing retained version: {expected}"
        );
    }
    let copy = workspace
        .restore_preserved_as_copy(&found["冲突后继续编辑的草稿 C2"])
        .unwrap();
    assert_ne!(copy.session.note_id, session.note_id);
    assert_eq!(copy.session.text.content, "冲突后继续编辑的草稿 C2");
    assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    let handled = &found["尚未保存的本地草稿 C"];
    workspace.resolve_preserved_version(handled).unwrap();
    assert!(!workspace
        .list_preserved_versions()
        .unwrap()
        .versions
        .iter()
        .any(|version| &version.version_id == handled));
    assert_eq!(
        workspace
            .open_preserved_version(handled)
            .unwrap()
            .text
            .content,
        "尚未保存的本地草稿 C"
    );
}

#[test]
fn missing_corrupt_and_unsupported_sources_keep_the_draft_without_creating_a_blank_note() {
    for failure in ["deleted", "corrupt", "unsupported"] {
        let (temp, mut workspace, session) = setup();
        let path = note_path(&temp, &session.note_id);
        let draft = draft_content(&session, &format!("{failure} 时仍在编辑的草稿"));
        let invalid_bytes = match failure {
            "corrupt" => Some(b"not a valid cuenote package".to_vec()),
            "unsupported" => Some(include_bytes!("fixtures/unsupported-version.cuenote").to_vec()),
            _ => None,
        };
        match &invalid_bytes {
            Some(bytes) => fs::write(&path, bytes).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }

        let probe = workspace
            .probe_external_changes(Some(&session.session_id))
            .unwrap();
        assert_eq!(
            probe.status,
            if failure == "deleted" {
                "missing"
            } else {
                "unreadable"
            },
            "unexpected probe state for {failure}"
        );
        let reconciliation = workspace
            .reconcile_external_changes(&session.session_id, draft.clone())
            .unwrap();
        assert_eq!(reconciliation.status, "conflict", "{failure}");
        assert!(reconciliation.session.is_none(), "{failure}");

        let active = workspace.snapshot().unwrap().session.unwrap();
        assert_eq!(active.session_id, session.session_id, "{failure}");
        assert!(active.read_only_reason.is_some(), "{failure}");
        assert_eq!(active.text.content, "已持有的旧版 A", "{failure}");
        assert!(!reconciliation
            .library
            .notes
            .iter()
            .any(|note| note.note_id == session.note_id));

        match invalid_bytes {
            Some(bytes) => assert_eq!(fs::read(&path).unwrap(), bytes, "{failure}"),
            None => assert!(
                !path.exists(),
                "deletion must not create a replacement note"
            ),
        }

        let mut recovered_draft = false;
        for version in workspace.list_preserved_versions().unwrap().versions {
            let preview = workspace
                .open_preserved_version(&version.version_id)
                .unwrap();
            if preview.content == draft {
                recovered_draft = true;
                break;
            }
        }
        assert!(
            recovered_draft,
            "the {failure} draft was not independently recoverable"
        );
    }
}

#[test]
fn duplicate_identity_under_a_nonstandard_name_is_preserved_without_touching_either_file() {
    let (temp, mut workspace, session) = setup();
    let canonical = note_path(&temp, &session.note_id);
    let original_bytes = fs::read(&canonical).unwrap();
    let alias = canonical.with_file_name("!duplicate-copy.cuenote");
    let mut package = storage::open_note(&canonical).unwrap().package;
    let mut alias_text = package.text_projection();
    alias_text.title = "同步副本标题".into();
    alias_text.content = "同步副本的不同有效内容".into();
    package.replace_plain_text(alias_text).unwrap();
    write_package_file(&alias, &package);
    let alias_bytes = fs::read(&alias).unwrap();

    let probe = workspace
        .probe_external_changes(Some(&session.session_id))
        .unwrap();
    assert_eq!(probe.status, "unchanged");
    let library = probe.library.unwrap();
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("重复笔记身份")));
    assert!(library
        .warnings
        .iter()
        .any(|warning| warning.contains("文件名与内容身份不一致")));
    assert_eq!(fs::read(&canonical).unwrap(), original_bytes);
    assert_eq!(fs::read(&alias).unwrap(), alias_bytes);

    let mut preserved_alias = false;
    for version in workspace.list_preserved_versions().unwrap().versions {
        let preview = workspace
            .open_preserved_version(&version.version_id)
            .unwrap();
        if preview.text.title == "同步副本标题" && preview.text.content == "同步副本的不同有效内容"
        {
            assert!(preview.read_only_reason.is_some());
            preserved_alias = true;
            break;
        }
    }
    assert!(
        preserved_alias,
        "the nonstandard duplicate content was not retained"
    );
    assert_eq!(fs::read(&canonical).unwrap(), original_bytes);
    assert_eq!(fs::read(&alias).unwrap(), alias_bytes);
}

#[test]
fn stale_session_external_change_requests_are_rejected_without_replacing_current_input() {
    let (_temp, mut workspace, old_session) = setup();
    let new_note = workspace.new_note().unwrap();
    let current = workspace
        .save_note_text(
            &new_note.session_id,
            PlainText {
                title: "当前笔记会话".into(),
                cue: String::new(),
                content: "当前仍在编辑的内容".into(),
                summary: String::new(),
            },
        )
        .unwrap()
        .session;

    assert_eq!(
        workspace
            .probe_external_changes(Some(&old_session.session_id))
            .unwrap_err()
            .kind,
        "staleSession"
    );
    let stale_draft = draft_content(&old_session, "过期会话草稿");
    assert_eq!(
        workspace
            .reconcile_external_changes(&old_session.session_id, stale_draft)
            .unwrap_err()
            .kind,
        "staleSession"
    );
    let active = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(active.session_id, current.session_id);
    assert_eq!(active.content, current.content);
}

#[test]
fn failed_version_preservation_does_not_refresh_the_session_or_consume_the_draft() {
    let (temp, mut workspace, session) = setup();
    let path = note_path(&temp, &session.note_id);
    let draft = draft_content(&session, "版本存储故障期间的本地输入");
    write_external(&path, "外部已发布的 B");

    let recovery = temp.path().join("recovery");
    let versions = recovery.join("versions");
    fs::rename(&versions, recovery.join("versions-quarantined")).unwrap();
    fs::write(&versions, b"occupied by a file").unwrap();

    let error = workspace
        .reconcile_external_changes(&session.session_id, draft.clone())
        .unwrap_err();
    assert_eq!(error.kind, "unsafePath");
    let active = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(active.session_id, session.session_id);
    assert_eq!(active.text.content, "已持有的旧版 A");
    assert_eq!(
        storage::open_note(&path)
            .unwrap()
            .package
            .text_projection()
            .content,
        "外部已发布的 B"
    );
    assert!(draft
        .sections
        .content
        .iter()
        .any(|block| matches!(block, Block::Paragraph { children } if format!("{children:?}").contains("版本存储故障期间的本地输入"))));
}

#[test]
fn probe_then_exact_revert_still_preserves_the_dirty_draft_as_a_conflict() {
    let (temp, mut workspace, session) = setup();
    let path = note_path(&temp, &session.note_id);
    let original_bytes = fs::read(&path).unwrap();
    write_external(&path, "探测时观察到的 B");

    let probe = workspace
        .probe_external_changes(Some(&session.session_id))
        .unwrap();
    assert_eq!(probe.status, "changed");
    fs::write(&path, &original_bytes).unwrap();
    let draft = draft_content(&session, "B 回退后仍需保留的草稿 C");

    let reconciled = workspace
        .reconcile_external_changes(&session.session_id, draft.clone())
        .unwrap();
    assert_eq!(reconciled.status, "conflict");
    assert!(reconciled.session.is_none());
    assert_eq!(fs::read(&path).unwrap(), original_bytes);

    let mut recovered_draft = false;
    for version in workspace.list_preserved_versions().unwrap().versions {
        let preview = workspace
            .open_preserved_version(&version.version_id)
            .unwrap();
        recovered_draft |= preview.content == draft;
    }
    assert!(
        recovered_draft,
        "the dirty draft disappeared after B reverted to A"
    );
}

#[test]
fn preserved_image_and_editable_mind_map_reopen_and_save_as_a_complete_copy() {
    let (temp, mut workspace, initial) = setup();
    let image_bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let image = workspace
        .stage_image(&initial.session_id, image_bytes.clone())
        .unwrap();
    let root_id = Uuid::new_v4().to_string();
    let child_id = Uuid::new_v4().to_string();
    let map_id = Uuid::new_v4().to_string();
    let map = Block::MindMap {
        map_id,
        root_id: root_id.clone(),
        nodes: vec![
            MindMapNode {
                id: root_id.clone(),
                text: "根节点".into(),
                children: vec![child_id.clone()],
            },
            MindMapNode {
                id: child_id,
                text: "子节点".into(),
                children: Vec::new(),
            },
        ],
    };
    let mut content = initial.content.clone();
    content.sections.content.push(Block::Image {
        asset_id: image.id.clone(),
        display_width_px: image.width,
    });
    content.sections.content.push(map);
    let saved = workspace
        .save_note_content(&initial.session_id, content.clone())
        .unwrap()
        .session;

    let source = note_path(&temp, &saved.note_id);
    let mut external_package = storage::open_note(&source).unwrap().package;
    let mut external_content = external_package.content();
    external_content.title = "外部更新带图和导图".into();
    external_package.replace_content(external_content);
    write_package_file(&source, &external_package);
    assert_eq!(
        workspace
            .probe_external_changes(Some(&saved.session_id))
            .unwrap()
            .status,
        "changed"
    );
    assert_eq!(
        workspace
            .reconcile_external_changes(&saved.session_id, saved.content.clone())
            .unwrap()
            .status,
        "refreshed"
    );

    let version_id = workspace
        .list_preserved_versions()
        .unwrap()
        .versions
        .into_iter()
        .find(|version| version.title == "外部更新带图和导图")
        .expect("the rich external version should be retained")
        .version_id;
    drop(workspace);
    let mut restarted = reopen(temp.path());
    restarted.snapshot().unwrap();
    let preview = restarted.open_preserved_version(&version_id).unwrap();
    assert_eq!(preview.text.title, "外部更新带图和导图");
    assert!(preview
        .content
        .sections
        .content
        .iter()
        .any(|block| matches!(block, Block::Image { asset_id, .. } if asset_id == &image.id)));
    assert!(preview.content.sections.content.iter().any(
        |block| matches!(block, Block::MindMap { nodes, .. } if nodes.iter().any(|node| node.id == root_id && node.text == "根节点"))
    ));
    assert_eq!(
        restarted
            .read_image_asset(&preview.session_id, &image.id)
            .unwrap(),
        image_bytes
    );

    let copy = restarted
        .restore_preserved_as_copy(&version_id)
        .unwrap()
        .session;
    let mut edited = copy.content.clone();
    let root = edited
        .sections
        .content
        .iter_mut()
        .find_map(|block| match block {
            Block::MindMap { nodes, .. } => nodes.iter_mut().find(|node| node.id == root_id),
            _ => None,
        })
        .expect("restored copy should contain an editable map root");
    root.text = "已编辑的根节点".into();
    let saved_copy = restarted
        .save_note_content(&copy.session_id, edited.clone())
        .unwrap()
        .session;
    assert_eq!(saved_copy.content, edited);
    assert_eq!(
        restarted
            .read_image_asset(&saved_copy.session_id, &image.id)
            .unwrap(),
        image_bytes
    );
    let reopened_copy = restarted.open_note(&saved_copy.note_id).unwrap();
    assert_eq!(reopened_copy.content, edited);
    assert!(reopened_copy.content.sections.content.iter().any(
        |block| matches!(block, Block::MindMap { nodes, .. } if nodes.iter().any(|node| node.id == root_id && node.text == "已编辑的根节点"))
    ));
    assert_eq!(
        restarted
            .read_image_asset(&reopened_copy.session_id, &image.id)
            .unwrap(),
        image_bytes
    );
}
