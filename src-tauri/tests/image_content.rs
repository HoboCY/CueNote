use cuenote_lib::note_format::MediaType;
use cuenote_lib::note_format::MAX_PACKAGE_BYTES;
use cuenote_lib::note_format::{Block, MindMapNode, NotePackage};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use image::ImageEncoder;

fn workspace_temp() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("image-content-")
        .tempdir_in(base)
        .unwrap()
}

fn grayscale_png(width: u32, height: u32) -> Vec<u8> {
    let pixels = vec![127; width as usize * height as usize];
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&pixels, width, height, image::ExtendedColorType::L8)
        .unwrap();
    bytes
}

fn oversized_comment_jpeg() -> Vec<u8> {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode(&[20, 90, 140], 1, 1, image::ColorType::Rgb8.into())
        .unwrap();
    let mut bytes = Vec::with_capacity(MAX_PACKAGE_BYTES as usize + 70_000);
    bytes.extend_from_slice(&jpeg[..2]);
    while bytes.len() <= MAX_PACKAGE_BYTES as usize {
        bytes.extend_from_slice(&[0xff, 0xfe, 0xff, 0xff]);
        let new_len = bytes.len() + 65_533;
        bytes.resize(new_len, 0);
    }
    bytes.extend_from_slice(&jpeg[2..]);
    bytes
}

#[test]
fn new_note_session_exposes_an_empty_asset_manifest() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();

    let session = workspace.new_note().unwrap();

    assert!(session.assets.is_empty());
}

#[test]
fn staging_a_png_returns_its_manifest_and_preserves_original_bytes() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();

    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();

    assert_eq!(entry.media_type, MediaType::Png);
    assert_eq!(entry.byte_length, bytes.len() as u64);
    assert_eq!(
        entry.sha256,
        "d0f93a5b68134a34cd3ada916e1fccf7bedeaa03a8f2371e11ed3c249d5aacdf"
    );
    assert_eq!((entry.width, entry.height), (1, 1));
    assert_eq!(
        workspace
            .read_image_asset(&session.session_id, &entry.id)
            .unwrap(),
        bytes
    );
    assert!(session.assets.is_empty());
}

#[test]
fn staging_a_jpeg_preserves_the_original_jpeg_bytes() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode(
            &[20, 90, 140, 200, 130, 30],
            2,
            1,
            image::ColorType::Rgb8.into(),
        )
        .unwrap();

    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();

    assert_eq!(entry.media_type, MediaType::Jpeg);
    assert_eq!((entry.width, entry.height), (2, 1));
    assert_eq!(
        workspace
            .read_image_asset(&session.session_id, &entry.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn unsupported_or_corrupt_image_bytes_are_rejected_before_stash_creation() {
    let temp = workspace_temp();
    let recovery = temp.path().join("recovery");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();

    assert_eq!(
        workspace
            .stage_image(&session.session_id, b"not an image".to_vec())
            .unwrap_err()
            .kind,
        "invalid_asset"
    );
    assert_eq!(
        workspace
            .stage_image(
                &session.session_id,
                include_bytes!("fixtures/pixel.png")[..33].to_vec()
            )
            .unwrap_err()
            .kind,
        "invalid_asset"
    );
    assert!(!recovery.join("asset-staging").exists());
}

#[test]
fn single_image_pixel_budget_accepts_exact_and_rejects_over_limit() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();

    let exact = grayscale_png(25_000_000, 1);
    let entry = workspace.stage_image(&session.session_id, exact).unwrap();
    assert_eq!((entry.width, entry.height), (25_000_000, 1));

    let over = grayscale_png(25_000_001, 1);
    assert_eq!(
        workspace
            .stage_image(&session.session_id, over)
            .unwrap_err()
            .kind,
        "resource_limit"
    );
}

#[test]
fn document_pixel_budget_accepts_exact_and_rejects_over_without_accepting_a_draft() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let png = grayscale_png(25_000_000, 1);
    let mut content = session.content.clone();

    for _ in 0..4 {
        let entry = workspace
            .stage_image(&session.session_id, png.clone())
            .unwrap();
        content.sections.content.push(Block::Image {
            asset_id: entry.id,
            display_width_px: 1,
        });
    }
    workspace
        .preflight_image_insert(&session.session_id, content.clone())
        .unwrap();

    let fifth = workspace.stage_image(&session.session_id, png).unwrap();
    let mut over_budget = content;
    over_budget.sections.content.push(Block::Image {
        asset_id: fifth.id,
        display_width_px: 1,
    });
    assert_eq!(
        workspace
            .preflight_image_insert(&session.session_id, over_budget)
            .unwrap_err()
            .kind,
        "resource_limit"
    );
    let unchanged = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(unchanged.content, session.content);
    assert!(unchanged.assets.is_empty());
}

#[test]
fn package_budget_rejects_image_insertion_without_changing_the_current_draft() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = oversized_comment_jpeg();
    assert!(bytes.len() as u64 > MAX_PACKAGE_BYTES);
    let entry = workspace.stage_image(&session.session_id, bytes).unwrap();
    let mut attempted = session.content.clone();
    attempted.sections.content.push(Block::Image {
        asset_id: entry.id,
        display_width_px: 1,
    });

    assert_eq!(
        workspace
            .preflight_image_insert(&session.session_id, attempted)
            .unwrap_err()
            .kind,
        "budget_exceeded"
    );
    let unchanged = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(unchanged.content, session.content);
    assert!(unchanged.assets.is_empty());
}

#[test]
fn saved_image_reopens_with_its_manifest_and_original_bytes() {
    let temp = workspace_temp();
    let recovery = temp.path().join("recovery");
    let library = temp.path().join("library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut content = session.content.clone();
    content.sections.content.push(Block::Image {
        asset_id: entry.id.clone(),
        display_width_px: entry.width,
    });

    workspace
        .preflight_image_insert(&session.session_id, content.clone())
        .unwrap();
    let saved = workspace
        .save_note_content(&session.session_id, content.clone())
        .unwrap();

    assert_eq!(saved.session.content, content);
    assert_eq!(saved.session.assets, vec![entry.clone()]);
    assert_eq!(
        workspace
            .read_image_asset(&session.session_id, &entry.id)
            .unwrap(),
        bytes
    );
    let reopened = workspace.open_note(&session.note_id).unwrap();
    assert_eq!(reopened.content, content);
    assert_eq!(reopened.assets, vec![entry.clone()]);
    assert_eq!(
        workspace
            .read_image_asset(&reopened.session_id, &entry.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn saved_manifest_keeps_first_reference_order_and_deduplicates_repeated_images() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let first = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let second = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut content = session.content.clone();
    content.sections.content.extend([
        Block::Image {
            asset_id: second.id.clone(),
            display_width_px: 1,
        },
        Block::Image {
            asset_id: first.id.clone(),
            display_width_px: 1,
        },
        Block::Image {
            asset_id: second.id.clone(),
            display_width_px: 1,
        },
    ]);

    let saved = workspace
        .save_note_content(&session.session_id, content.clone())
        .unwrap();
    let reopened = workspace.open_note(&session.note_id).unwrap();

    assert_eq!(saved.session.content, content);
    assert_eq!(saved.session.assets, vec![second.clone(), first.clone()]);
    assert_eq!(reopened.assets, vec![second.clone(), first.clone()]);
    assert_eq!(
        workspace
            .read_image_asset(&reopened.session_id, &second.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn image_blocks_are_only_accepted_at_top_level_in_body_content() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let entry = workspace
        .stage_image(
            &session.session_id,
            include_bytes!("fixtures/pixel.png").to_vec(),
        )
        .unwrap();
    let mut side_section = session.content.clone();
    side_section.sections.cue.push(Block::Image {
        asset_id: entry.id,
        display_width_px: 1,
    });

    assert_eq!(
        workspace
            .preflight_image_insert(&session.session_id, side_section)
            .unwrap_err()
            .kind,
        "readOnly"
    );
}

#[test]
fn saving_an_image_content_copy_preserves_assets_without_changing_the_active_session() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut content = session.content.clone();
    content.sections.content.push(Block::Image {
        asset_id: entry.id.clone(),
        display_width_px: entry.width,
    });
    let copy_path = temp.path().join("image-copy.cuenote");

    workspace
        .save_current_content_as(&session.session_id, content.clone(), copy_path.clone())
        .unwrap();

    let copied = storage::open_note(&copy_path).unwrap();
    assert_eq!(copied.package.content(), content);
    assert_eq!(copied.package.manifest.assets, vec![entry.clone()]);
    assert_eq!(copied.package.assets[&entry.id], bytes);
    let active = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(active.content, session.content);
    assert!(active.assets.is_empty());
}

#[test]
fn deleting_and_saving_an_image_keeps_it_available_for_undo() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut with_image = session.content.clone();
    with_image.sections.content.push(Block::Image {
        asset_id: entry.id.clone(),
        display_width_px: entry.width,
    });
    workspace
        .save_note_content(&session.session_id, with_image.clone())
        .unwrap();

    // Reopening drops the original session stash, so undo must recover the
    // removed asset from the package being replaced.
    let reopened = workspace.open_note(&session.note_id).unwrap();
    let mut without_image = reopened.content.clone();
    without_image
        .sections
        .content
        .retain(|block| !matches!(block, Block::Image { .. }));
    workspace
        .save_note_content(&reopened.session_id, without_image)
        .unwrap();

    workspace
        .preflight_image_insert(&reopened.session_id, with_image.clone())
        .unwrap();
    let restored = workspace
        .save_note_content(&reopened.session_id, with_image.clone())
        .unwrap();

    assert_eq!(restored.session.content, with_image);
    assert_eq!(restored.session.assets, vec![entry.clone()]);
    assert_eq!(
        workspace
            .read_image_asset(&reopened.session_id, &entry.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn deleting_an_image_preserves_the_existing_stash_copy_without_rewriting_it() {
    let temp = workspace_temp();
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("default"),
    );
    workspace
        .select_library(temp.path().join("library"))
        .unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut with_image = session.content.clone();
    with_image.sections.content.push(Block::Image {
        asset_id: entry.id.clone(),
        display_width_px: entry.width,
    });
    workspace
        .save_note_content(&session.session_id, with_image)
        .unwrap();
    workspace
        .save_note_content(&session.session_id, session.content.clone())
        .unwrap();

    assert_eq!(
        workspace
            .read_image_asset(&session.session_id, &entry.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn explicit_close_removes_this_runs_staging_and_keeps_the_last_note_preference() {
    let temp = workspace_temp();
    let settings = temp.path().join("settings");
    let recovery = temp.path().join("recovery");
    let library = temp.path().join("library");
    let default = temp.path().join("default");
    let mut workspace = Workspace::new(settings.clone(), recovery.clone(), default.clone());
    workspace.select_library(library).unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut content = session.content.clone();
    content.sections.content.push(Block::Image {
        asset_id: entry.id.clone(),
        display_width_px: entry.width,
    });
    workspace
        .save_note_content(&session.session_id, content)
        .unwrap();
    let staging_root = recovery.join("asset-staging");

    workspace.close().unwrap();

    let remaining_runs = std::fs::read_dir(&staging_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != ".locks")
        .collect::<Vec<_>>();
    assert!(remaining_runs.is_empty());
    assert!(workspace.snapshot().unwrap().session.is_none());

    let mut restarted = Workspace::new(settings, recovery, default);
    let reopened = restarted.snapshot().unwrap().session.unwrap();
    assert_eq!(reopened.note_id, session.note_id);
    assert_eq!(reopened.assets, vec![entry.clone()]);
    assert_eq!(
        restarted
            .read_image_asset(&reopened.session_id, &entry.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn stale_save_recovery_copy_keeps_image_bytes_and_manifest() {
    let temp = workspace_temp();
    let recovery = temp.path().join("recovery");
    let library = temp.path().join("library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let session = workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let entry = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut local_content = session.content.clone();
    local_content.title = "需要恢复的图片草稿".into();
    local_content.sections.content.push(Block::Image {
        asset_id: entry.id.clone(),
        display_width_px: entry.width,
    });
    workspace
        .save_note_content(&session.session_id, local_content.clone())
        .unwrap();
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", session.note_id));
    let baseline = storage::open_note(&note_path).unwrap();
    let mut external_package = baseline.package.clone();
    let mut external_content = external_package.content();
    external_content.title = "外部更新".into();
    external_package.replace_content(external_content.clone());
    storage::save_note(
        &note_path,
        &recovery,
        &external_package,
        Some(&baseline.revision),
    )
    .unwrap();

    let error = workspace
        .save_note_content(&session.session_id, local_content.clone())
        .unwrap_err();
    assert_eq!(error.kind, "conflict");
    assert!(error.recovery_path.is_some());
    let draft = workspace
        .list_recovery_drafts()
        .unwrap()
        .drafts
        .into_iter()
        .find(|draft| draft.note_id == session.note_id && draft.title == local_content.title)
        .unwrap();

    let restored = workspace
        .restore_recovery_as_copy(&draft.recovery_id)
        .unwrap();
    assert_eq!(restored.session.content, local_content);
    assert_eq!(restored.session.assets, vec![entry.clone()]);
    assert_eq!(
        workspace
            .read_image_asset(&restored.session.session_id, &entry.id)
            .unwrap(),
        bytes
    );
    assert_eq!(
        storage::open_note(&note_path).unwrap().package.content(),
        external_content
    );
}

#[test]
fn image_stashes_are_session_scoped_and_survive_another_workspace_drop() {
    let temp = workspace_temp();
    let recovery = temp.path().join("recovery");
    let mut first_workspace = Workspace::new(
        temp.path().join("first-settings"),
        recovery.clone(),
        temp.path().join("first-default"),
    );
    let mut second_workspace = Workspace::new(
        temp.path().join("second-settings"),
        recovery.clone(),
        temp.path().join("second-default"),
    );
    first_workspace
        .select_library(temp.path().join("first-library"))
        .unwrap();
    second_workspace
        .select_library(temp.path().join("second-library"))
        .unwrap();
    let first_session = first_workspace.new_note().unwrap();
    let second_session = second_workspace.new_note().unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let first_entry = first_workspace
        .stage_image(&first_session.session_id, bytes.clone())
        .unwrap();
    let second_entry = second_workspace
        .stage_image(&second_session.session_id, bytes.clone())
        .unwrap();

    let staging_root = recovery.join("asset-staging");
    assert!(std::fs::read_dir(&staging_root).unwrap().any(|run| run
        .unwrap()
        .path()
        .join(&first_session.session_id)
        .exists()));
    assert_eq!(
        first_workspace
            .read_image_asset(&first_session.session_id, &first_entry.id)
            .unwrap(),
        bytes
    );
    assert_eq!(
        second_workspace
            .read_image_asset(&second_session.session_id, &first_entry.id)
            .unwrap_err()
            .kind,
        "missingAsset"
    );
    let replacement = first_workspace.new_note().unwrap();
    assert!(!std::fs::read_dir(&staging_root).unwrap().any(|run| run
        .unwrap()
        .path()
        .join(&first_session.session_id)
        .exists()));
    assert_eq!(
        first_workspace
            .read_image_asset(&first_session.session_id, &first_entry.id)
            .unwrap_err()
            .kind,
        "staleSession"
    );
    assert_eq!(
        first_workspace
            .stage_image(&first_session.session_id, bytes.clone())
            .unwrap_err()
            .kind,
        "staleSession"
    );
    drop(first_workspace);
    assert_eq!(
        second_workspace
            .read_image_asset(&second_session.session_id, &second_entry.id)
            .unwrap(),
        bytes
    );
    assert_ne!(replacement.session_id, first_session.session_id);
}

#[test]
fn read_only_notes_reject_image_staging_without_creating_a_stash() {
    let temp = workspace_temp();
    let recovery = temp.path().join("recovery");
    let library = temp.path().join("library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        recovery.clone(),
        temp.path().join("default"),
    );
    workspace.select_library(library.clone()).unwrap();
    let created = workspace.new_note().unwrap();
    let mut package =
        NotePackage::new_plain(created.notebook_id.clone().unwrap(), created.text.clone());
    package.document.note_id = created.note_id.clone();
    let root_id = "55ad7e2b-9523-4f0b-9041-426d7f94d924".to_owned();
    package.document.sections.content.push(Block::MindMap {
        map_id: "65ad7e2b-9523-4f0b-9041-426d7f94d924".into(),
        root_id: root_id.clone(),
        nodes: vec![MindMapNode {
            id: root_id,
            text: "只读节点".into(),
            children: Vec::new(),
        }],
    });
    let note_path = library
        .join("notes")
        .join(format!("{}.cuenote", created.note_id));
    storage::save_note(&note_path, &recovery, &package, None).unwrap();
    let read_only = workspace.open_note(&created.note_id).unwrap();

    assert!(read_only.read_only_reason.is_some());
    assert_eq!(
        workspace
            .stage_image(
                &read_only.session_id,
                include_bytes!("fixtures/pixel.png").to_vec()
            )
            .unwrap_err()
            .kind,
        "readOnly"
    );
    assert_eq!(
        workspace
            .preflight_image_insert(&read_only.session_id, read_only.content.clone())
            .unwrap_err()
            .kind,
        "readOnly"
    );
    assert!(!recovery.join("asset-staging").exists());
}
