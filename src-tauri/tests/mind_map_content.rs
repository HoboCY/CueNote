use cuenote_lib::note_format::{
    AssetManifestEntry, Block, MindMapNode, NoteContent, NotePackage, PlainText, MAX_MAP_NODES,
};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::path::{Path, PathBuf};
use uuid::Uuid;
mod support;

fn workspace_temp() -> tempfile::TempDir {
    support::tempdir("mind-map-content-")
}

fn selected_workspace(temp: &tempfile::TempDir) -> (Workspace, PathBuf) {
    let library = temp.path().join("Library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    (workspace, library)
}

fn mind_map(node_count: usize) -> Block {
    assert!(node_count > 0);
    let map_id = Uuid::new_v4().to_string();
    let ids = (0..node_count)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    let nodes = ids
        .iter()
        .enumerate()
        .map(|(index, id)| MindMapNode {
            id: id.clone(),
            text: format!("节点 {index}"),
            children: ids.get(index + 1).cloned().into_iter().collect(),
        })
        .collect();
    Block::MindMap {
        map_id,
        root_id: ids[0].clone(),
        nodes,
    }
}

fn branching_map() -> Block {
    let map_id = Uuid::new_v4().to_string();
    let ids = (0..4)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    Block::MindMap {
        map_id,
        root_id: ids[0].clone(),
        nodes: vec![
            MindMapNode {
                id: ids[0].clone(),
                text: "根节点".into(),
                children: vec![ids[1].clone(), ids[2].clone()],
            },
            MindMapNode {
                id: ids[1].clone(),
                text: "第一分支".into(),
                children: vec![ids[3].clone()],
            },
            MindMapNode {
                id: ids[2].clone(),
                text: "第二分支".into(),
                children: Vec::new(),
            },
            MindMapNode {
                id: ids[3].clone(),
                text: "叶节点".into(),
                children: Vec::new(),
            },
        ],
    }
}

fn image_block(asset: &AssetManifestEntry) -> Block {
    Block::Image {
        asset_id: asset.id.clone(),
        display_width_px: asset.width,
    }
}

fn content_with_map_budget(
    base: &NoteContent,
    assets: &[AssetManifestEntry],
    first_map_nodes: usize,
    second_map_nodes: usize,
) -> NoteContent {
    let mut content = base.clone();
    content.sections.content = assets.iter().map(image_block).collect();
    content.sections.content.push(mind_map(first_map_nodes));
    content.sections.content.push(mind_map(second_map_nodes));
    content
}

fn note_path(library: &Path, note_id: &str) -> PathBuf {
    library.join("notes").join(format!("{note_id}.cuenote"))
}

#[test]
fn top_level_maps_round_trip_in_order_with_images_and_restore_after_saved_deletion() {
    let temp = workspace_temp();
    let (mut workspace, library) = selected_workspace(&temp);
    let original = workspace.new_note().unwrap();
    let image_bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let first_image = workspace
        .stage_image(&original.session_id, image_bytes.clone())
        .unwrap();
    let mut initial_content = original.content.clone();
    initial_content
        .sections
        .content
        .push(image_block(&first_image));
    let initial = workspace
        .save_note_content(&original.session_id, initial_content)
        .unwrap();

    let second_image = workspace
        .stage_image(&original.session_id, image_bytes.clone())
        .unwrap();
    let mut intended = initial.session.content.clone();
    intended.sections.content = vec![
        image_block(&first_image),
        mind_map(500),
        image_block(&second_image),
        mind_map(1),
    ];
    let saved_before = storage::open_note(&note_path(&library, &original.note_id)).unwrap();
    let session_before = workspace.snapshot().unwrap().session.unwrap();

    workspace
        .preflight_note_content(&original.session_id, intended.clone())
        .unwrap();

    let session_after_preflight = workspace.snapshot().unwrap().session.unwrap();
    let saved_after_preflight =
        storage::open_note(&note_path(&library, &original.note_id)).unwrap();
    assert_eq!(session_after_preflight.content, session_before.content);
    assert_eq!(session_after_preflight.assets, session_before.assets);
    assert_eq!(session_after_preflight.revision, session_before.revision);
    assert_eq!(saved_after_preflight.revision, saved_before.revision);
    assert_eq!(saved_after_preflight.package, saved_before.package);

    let saved = workspace
        .save_note_content(&original.session_id, intended.clone())
        .unwrap();
    assert_eq!(saved.session.content, intended);
    assert_eq!(
        saved
            .session
            .assets
            .iter()
            .map(|asset| asset.id.as_str())
            .collect::<Vec<_>>(),
        vec![first_image.id.as_str(), second_image.id.as_str()]
    );
    let reopened = workspace.open_note(&original.note_id).unwrap();
    assert_eq!(reopened.content, intended);
    assert_eq!(
        workspace
            .read_image_asset(&reopened.session_id, &first_image.id)
            .unwrap(),
        image_bytes
    );
    assert_eq!(
        workspace
            .read_image_asset(&reopened.session_id, &second_image.id)
            .unwrap(),
        image_bytes
    );

    let mut without_maps = reopened.content.clone();
    without_maps
        .sections
        .content
        .retain(|block| !matches!(block, Block::MindMap { .. }));
    let deleted = workspace
        .save_note_content(&reopened.session_id, without_maps)
        .unwrap();
    assert!(deleted
        .session
        .content
        .sections
        .content
        .iter()
        .all(|block| !matches!(block, Block::MindMap { .. })));

    let after_delete = workspace.open_note(&original.note_id).unwrap();
    workspace
        .preflight_note_content(&after_delete.session_id, intended.clone())
        .unwrap();
    workspace
        .save_note_content(&after_delete.session_id, intended.clone())
        .unwrap();
    let restored = workspace.open_note(&original.note_id).unwrap();
    assert_eq!(restored.content, intended);
}

#[test]
fn map_content_save_as_preserves_the_map_and_leaves_the_active_session_unchanged() {
    let temp = workspace_temp();
    let (mut workspace, library) = selected_workspace(&temp);
    let original = workspace.new_note().unwrap();
    let mut intended = original.content.clone();
    intended.sections.content.push(mind_map(500));
    let saved = workspace
        .save_note_content(&original.session_id, intended.clone())
        .unwrap();
    let active_before = workspace.snapshot().unwrap().session.unwrap();
    let copy_path = temp.path().join("mind-map-copy.cuenote");

    workspace
        .save_current_content_as(
            &saved.session.session_id,
            intended.clone(),
            copy_path.clone(),
        )
        .unwrap();

    let copied = storage::open_note(&copy_path).unwrap();
    assert_eq!(copied.package.content(), intended);
    assert_ne!(copied.package.document.note_id, original.note_id);
    let active_after = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(active_after.session_id, active_before.session_id);
    assert_eq!(active_after.content, active_before.content);
    assert_eq!(active_after.revision, active_before.revision);
    assert_eq!(
        storage::open_note(&note_path(&library, &original.note_id))
            .unwrap()
            .package
            .content(),
        intended
    );
}

#[test]
fn preflight_enforces_the_aggregate_map_node_budget_across_maps_and_images() {
    let temp = workspace_temp();
    let (mut workspace, library) = selected_workspace(&temp);
    let session = workspace.new_note().unwrap();
    let image_bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let first_image = workspace
        .stage_image(&session.session_id, image_bytes.clone())
        .unwrap();
    let second_image = workspace
        .stage_image(&session.session_id, image_bytes)
        .unwrap();
    let mut baseline_content = session.content.clone();
    baseline_content.sections.content = vec![image_block(&first_image), image_block(&second_image)];
    let baseline = workspace
        .save_note_content(&session.session_id, baseline_content)
        .unwrap();
    let note_file = note_path(&library, &session.note_id);
    let saved_before = storage::open_note(&note_file).unwrap();

    let below_budget = content_with_map_budget(
        &baseline.session.content,
        &baseline.session.assets,
        2_500,
        MAX_MAP_NODES - 2_501,
    );
    workspace
        .preflight_note_content(&session.session_id, below_budget)
        .unwrap();

    let exact_budget = content_with_map_budget(
        &baseline.session.content,
        &baseline.session.assets,
        2_500,
        MAX_MAP_NODES - 2_500,
    );
    workspace
        .preflight_note_content(&session.session_id, exact_budget.clone())
        .unwrap();

    workspace
        .save_note_content(&session.session_id, exact_budget.clone())
        .unwrap();
    let exact_reopened = workspace.open_note(&session.note_id).unwrap();
    assert_eq!(exact_reopened.content, exact_budget);
    assert_eq!(
        storage::open_note(&note_file).unwrap().package.content(),
        exact_budget
    );
    let saved_exact_before_rejection = storage::open_note(&note_file).unwrap();

    let over_budget = content_with_map_budget(
        &exact_reopened.content,
        &exact_reopened.assets,
        2_501,
        MAX_MAP_NODES - 2_500,
    );
    assert_eq!(
        workspace
            .preflight_note_content(&exact_reopened.session_id, over_budget.clone())
            .unwrap_err()
            .kind,
        "resource_limit"
    );
    assert_eq!(
        workspace
            .save_note_content(&exact_reopened.session_id, over_budget)
            .unwrap_err()
            .kind,
        "resource_limit"
    );

    let active_after = workspace.snapshot().unwrap().session.unwrap();
    let saved_after = storage::open_note(&note_file).unwrap();
    assert_eq!(active_after.content, exact_reopened.content);
    assert_eq!(active_after.assets, exact_reopened.assets);
    assert_eq!(active_after.revision, exact_reopened.revision);
    assert_eq!(saved_after.revision, saved_exact_before_rejection.revision);
    assert_eq!(saved_after.package, saved_exact_before_rejection.package);
    assert_ne!(saved_after.package, saved_before.package);
}

#[test]
fn conflicted_map_edit_is_preserved_in_a_recovery_copy_with_text_and_edge_order() {
    let temp = workspace_temp();
    let (mut workspace, library) = selected_workspace(&temp);
    let initial = workspace.new_note().unwrap();
    let mut original_content = initial.content.clone();
    original_content.sections.content.push(branching_map());
    workspace
        .save_note_content(&initial.session_id, original_content)
        .unwrap();
    let original_path = note_path(&library, &initial.note_id);
    let stale_session = workspace.open_note(&initial.note_id).unwrap();
    let baseline = storage::open_note(&original_path).unwrap();

    let mut external_package = baseline.package.clone();
    let mut external_content = external_package.content();
    external_content.title = "外部已保存版本".into();
    external_package.replace_content(external_content.clone());
    let external_revision = storage::save_note(
        &original_path,
        &temp.path().join("recovery"),
        &external_package,
        Some(&baseline.revision),
    )
    .unwrap();

    let mut recovery_content = stale_session.content.clone();
    recovery_content.title = "导图冲突恢复草稿".into();
    let Block::MindMap { nodes, .. } = recovery_content
        .sections
        .content
        .iter_mut()
        .find(|block| matches!(block, Block::MindMap { .. }))
        .unwrap()
    else {
        unreachable!();
    };
    nodes[0].text = "恢复后的根节点文字".into();
    nodes[0].children.swap(0, 1);
    nodes[1].text = "改名后的第一分支".into();

    let conflict = workspace
        .save_note_content(&stale_session.session_id, recovery_content.clone())
        .unwrap_err();
    assert_eq!(conflict.kind, "conflict");
    let current = storage::open_note(&original_path).unwrap();
    assert_eq!(current.revision, external_revision);
    assert_eq!(current.package.content(), external_content);

    let drafts = workspace.list_recovery_drafts().unwrap().drafts;
    let draft = drafts
        .iter()
        .find(|draft| draft.note_id == initial.note_id && draft.title == recovery_content.title)
        .expect("the complete mind map draft must be listed for recovery");
    let restored = workspace
        .restore_recovery_as_copy(&draft.recovery_id)
        .unwrap();
    assert_ne!(restored.session.note_id, initial.note_id);
    assert_eq!(restored.session.content, recovery_content);

    let restored_path = note_path(&library, &restored.session.note_id);
    let restored_package = storage::open_note(&restored_path).unwrap().package;
    assert_eq!(restored_package.content(), recovery_content);
    let Block::MindMap { nodes, .. } = restored_package
        .document
        .sections
        .content
        .iter()
        .find(|block| matches!(block, Block::MindMap { .. }))
        .unwrap()
    else {
        unreachable!();
    };
    assert_eq!(nodes[0].text, "恢复后的根节点文字");
    assert_eq!(nodes[0].children[0], nodes[2].id);
    assert_eq!(nodes[0].children[1], nodes[1].id);
    assert_eq!(nodes[1].text, "改名后的第一分支");
}

#[test]
fn preflight_rejects_missing_images_nested_maps_sidebar_maps_and_invalid_map_graphs() {
    let temp = workspace_temp();
    let (mut workspace, _) = selected_workspace(&temp);
    let session = workspace.new_note().unwrap();
    let valid_map = mind_map(2);

    let mut missing_image = session.content.clone();
    missing_image.sections.content.push(Block::Image {
        asset_id: Uuid::new_v4().to_string(),
        display_width_px: 1,
    });
    assert_eq!(
        workspace
            .preflight_note_content(&session.session_id, missing_image)
            .unwrap_err()
            .kind,
        "missingAsset"
    );
    assert_eq!(
        workspace
            .stage_image(&session.session_id, b"not an image".to_vec())
            .unwrap_err()
            .kind,
        "invalid_asset"
    );

    let mut nested_quote = session.content.clone();
    nested_quote.sections.content.push(Block::Blockquote {
        blocks: vec![valid_map.clone()],
    });
    assert_eq!(
        workspace
            .preflight_note_content(&session.session_id, nested_quote)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut nested_list = session.content.clone();
    nested_list.sections.content.push(Block::BulletList {
        items: vec![cuenote_lib::note_format::ListItem {
            blocks: vec![valid_map.clone()],
        }],
    });
    assert_eq!(
        workspace
            .preflight_note_content(&session.session_id, nested_list)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut cue_map = session.content.clone();
    cue_map.sections.cue.push(valid_map.clone());
    assert_eq!(
        workspace
            .preflight_note_content(&session.session_id, cue_map)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut summary_map = session.content.clone();
    summary_map.sections.summary.push(valid_map.clone());
    assert_eq!(
        workspace
            .preflight_note_content(&session.session_id, summary_map)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let mut cyclic_map = session.content.clone();
    let mut malformed_map = valid_map;
    let Block::MindMap { nodes, .. } = &mut malformed_map else {
        unreachable!();
    };
    let root_id = nodes[0].id.clone();
    nodes[0].children.push(root_id);
    cyclic_map.sections.content.push(malformed_map);
    assert_eq!(
        workspace
            .preflight_note_content(&session.session_id, cyclic_map)
            .unwrap_err()
            .kind,
        "format"
    );
}

#[test]
fn preflight_rejects_read_only_and_stale_sessions() {
    let temp = workspace_temp();
    let (mut workspace, library) = selected_workspace(&temp);
    let created = workspace.new_note().unwrap();
    let unknown_notebook_id = Uuid::new_v4().to_string();
    let mut package = NotePackage::new_plain(
        unknown_notebook_id,
        PlainText {
            title: "未知笔记本的只读笔记".into(),
            cue: String::new(),
            content: String::new(),
            summary: String::new(),
        },
    );
    package.document.note_id = created.note_id.clone();
    storage::save_note(
        &note_path(&library, &created.note_id),
        &temp.path().join("recovery"),
        &package,
        None,
    )
    .unwrap();
    let read_only = workspace.open_note(&created.note_id).unwrap();
    assert!(read_only.read_only_reason.is_some());
    let mut attempted = read_only.content.clone();
    attempted.sections.content.push(mind_map(1));
    assert_eq!(
        workspace
            .preflight_note_content(&read_only.session_id, attempted)
            .unwrap_err()
            .kind,
        "readOnly"
    );

    let current = workspace.new_note().unwrap();
    let mut stale_attempt = current.content.clone();
    stale_attempt.sections.content.push(mind_map(1));
    assert_eq!(
        workspace
            .preflight_note_content(&read_only.session_id, stale_attempt)
            .unwrap_err()
            .kind,
        "staleSession"
    );
}
