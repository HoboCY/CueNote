use cuenote_lib::note_format::{write_package, Block, Inline, NotePackage, PlainText};
use cuenote_lib::storage;
use cuenote_lib::workspace::{SessionOrigin, Workspace};
use std::fs::{self, File};
use std::path::Path;
use uuid::Uuid;

fn setup() -> (tempfile::TempDir, Workspace) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("external-note-")
        .tempdir_in(parent)
        .unwrap();
    let workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("library"),
    );
    (temp, workspace)
}

fn external_file(root: &Path) -> std::path::PathBuf {
    let directory = root.join("documents");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("中文笔记.cuenote");
    let package = NotePackage::new_plain(
        Uuid::new_v4().to_string(),
        PlainText {
            title: "独立笔记".into(),
            cue: "线索".into(),
            content: "原内容".into(),
            summary: "总结".into(),
        },
    );
    write_package(&mut File::create(&path).unwrap(), &package).unwrap();
    path
}

#[test]
fn edit_original_without_selecting_a_library_and_keep_identity_and_organization() {
    let (temp, mut workspace) = setup();
    let path = external_file(temp.path());
    let original = storage::open_note(&path).unwrap();
    let session = workspace.open_external_note(path.clone()).unwrap();
    assert!(matches!(session.origin, SessionOrigin::External { .. }));
    assert_eq!(session.read_only_reason, None);
    assert_eq!(session.notebook_id, None);
    let mut content = session.content;
    content.sections.content = vec![Block::Paragraph {
        children: vec![Inline::Text {
            text: "库外编辑后".into(),
            marks: vec![],
        }],
    }];
    let receipt = workspace
        .save_external_note_content(&session.session_id, content.clone())
        .unwrap();
    let saved = storage::open_note(&path).unwrap();
    assert_eq!(saved.package.content(), content);
    assert_eq!(
        saved.package.document.note_id,
        original.package.document.note_id
    );
    assert_eq!(
        saved.package.document.organization,
        original.package.document.organization
    );
    assert_eq!(
        receipt.session.revision.as_deref(),
        Some(saved.revision.as_str())
    );
    assert!(!temp.path().join("library").exists());
    let reopened = workspace.open_external_note(path).unwrap();
    assert_eq!(reopened.content, content);
}

#[test]
fn library_actions_cannot_modify_an_external_file_even_with_a_matching_note_identity() {
    let (temp, mut workspace) = setup();
    let library = workspace.use_default_library().unwrap().library.unwrap();
    let notebook = library.default_notebook_id.unwrap();
    let local = workspace.new_note().unwrap();
    workspace
        .save_note_content(&local.session_id, local.content)
        .unwrap();
    let path = external_file(temp.path());
    fs::copy(
        temp.path()
            .join("library/notes")
            .join(format!("{}.cuenote", local.note_id)),
        &path,
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    let session = workspace.open_external_note(path.clone()).unwrap();
    assert_eq!(
        workspace
            .update_note_organization(&session.session_id, &notebook, true)
            .unwrap_err()
            .kind,
        "sessionScope"
    );
    assert_eq!(
        workspace.trash_note(&session.session_id).unwrap_err().kind,
        "sessionScope"
    );
    assert_eq!(
        workspace
            .restore_trashed_note(&session.session_id, None)
            .unwrap_err()
            .kind,
        "sessionScope"
    );
    assert_eq!(
        workspace
            .delete_trashed_note(&session.session_id, true)
            .unwrap_err()
            .kind,
        "sessionScope"
    );
    assert_eq!(
        workspace
            .save_note_content(&session.session_id, session.content.clone())
            .unwrap_err()
            .kind,
        "sessionScope"
    );
    let snapshot = workspace.snapshot().unwrap();
    assert!(snapshot.session.unwrap().read_only_reason.is_none());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn external_images_use_the_same_budget_checks_and_survive_original_path_save() {
    let (temp, mut workspace) = setup();
    let path = external_file(temp.path());
    let session = workspace.open_external_note(path.clone()).unwrap();
    let bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let asset = workspace
        .stage_image(&session.session_id, bytes.clone())
        .unwrap();
    let mut content = session.content;
    content.sections.content.push(Block::Image {
        asset_id: asset.id.clone(),
        display_width_px: 1,
    });
    workspace
        .preflight_image_insert(&session.session_id, content.clone())
        .unwrap();
    workspace
        .preflight_note_content(&session.session_id, content.clone())
        .unwrap();
    workspace
        .save_external_note_content(&session.session_id, content.clone())
        .unwrap();
    let reopened = workspace.open_external_note(path).unwrap();
    assert_eq!(reopened.content, content);
    assert_eq!(
        workspace
            .read_image_asset(&reopened.session_id, &asset.id)
            .unwrap(),
        bytes
    );
}

#[test]
fn save_external_draft_into_selected_notebook_creates_complete_independent_copy() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let library = workspace.create_notebook("目标笔记本").unwrap();
    let notebook_id = library
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "目标笔记本")
        .unwrap()
        .notebook_id
        .clone();
    let path = external_file(temp.path());
    let before = fs::read(&path).unwrap();
    let external = workspace.open_external_note(path.clone()).unwrap();
    let asset = workspace
        .stage_image(
            &external.session_id,
            include_bytes!("fixtures/pixel.png").to_vec(),
        )
        .unwrap();
    let mut draft = external.content.clone();
    draft.title = "未写回原文件的新标题".into();
    draft.sections.content.push(Block::Image {
        asset_id: asset.id.clone(),
        display_width_px: 1,
    });
    let node_id = Uuid::new_v4().to_string();
    draft.sections.content.push(Block::MindMap {
        map_id: Uuid::new_v4().to_string(),
        root_id: node_id.clone(),
        nodes: vec![cuenote_lib::note_format::MindMapNode {
            id: node_id,
            text: "完整导图".into(),
            children: vec![],
        }],
    });
    assert_eq!(
        workspace
            .save_external_into_library(
                &external.session_id,
                &Uuid::new_v4().to_string(),
                draft.clone()
            )
            .unwrap_err()
            .kind,
        "unknownNotebook"
    );
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().session_id,
        external.session_id
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let receipt = workspace
        .save_external_into_library(&external.session_id, &notebook_id, draft.clone())
        .unwrap();
    assert!(matches!(receipt.session.origin, SessionOrigin::Library));
    assert_ne!(receipt.session.note_id, external.note_id);
    assert_eq!(
        receipt.session.notebook_id.as_deref(),
        Some(notebook_id.as_str())
    );
    assert_eq!(receipt.library.notes.len(), 1);
    let copied = storage::open_note(
        &temp
            .path()
            .join("library/notes")
            .join(format!("{}.cuenote", receipt.session.note_id)),
    )
    .unwrap();
    assert_eq!(copied.package.content(), draft);
    assert_eq!(
        workspace
            .read_image_asset(&receipt.session.session_id, &asset.id)
            .unwrap(),
        include_bytes!("fixtures/pixel.png")
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn clean_external_refresh_retains_external_scope_without_a_library() {
    let (temp, mut workspace) = setup();
    let path = external_file(temp.path());
    let session = workspace.open_external_note(path.clone()).unwrap();
    let mut package = storage::open_note(&path).unwrap().package;
    let mut text = package.text_projection();
    text.content = "其他程序更新".into();
    package.replace_plain_text(text).unwrap();
    write_package(&mut File::create(&path).unwrap(), &package).unwrap();
    let result = workspace
        .reconcile_external_changes(&session.session_id, session.content)
        .unwrap();
    assert_eq!(result.status, "refreshed");
    let refreshed = result.session.unwrap();
    assert!(matches!(refreshed.origin, SessionOrigin::External { .. }));
    assert!(refreshed.read_only_reason.is_none());
    assert_eq!(refreshed.text.content, "其他程序更新");
    let mut next = refreshed.content;
    next.title = "继续编辑".into();
    workspace
        .save_external_note_content(&refreshed.session_id, next)
        .unwrap();
    assert_eq!(
        storage::open_note(&path).unwrap().package.content().title,
        "继续编辑"
    );
}

#[test]
fn external_conflict_keeps_disk_and_draft_and_can_export_without_a_library() {
    let (temp, mut workspace) = setup();
    let path = external_file(temp.path());
    let session = workspace.open_external_note(path.clone()).unwrap();
    let mut local = session.content;
    local.title = "我的未保存草稿".into();
    let mut changed = storage::open_note(&path).unwrap().package;
    let mut text = changed.text_projection();
    text.title = "磁盘另一个版本".into();
    changed.replace_plain_text(text).unwrap();
    write_package(&mut File::create(&path).unwrap(), &changed).unwrap();
    let disk = fs::read(&path).unwrap();
    let error = workspace
        .save_external_note_content(&session.session_id, local.clone())
        .unwrap_err();
    assert_eq!(error.kind, "conflict");
    let recovered = storage::open_note(Path::new(error.recovery_path.as_ref().unwrap())).unwrap();
    assert_eq!(recovered.package.content(), local);
    assert_eq!(fs::read(&path).unwrap(), disk);
    let output = temp.path().join("documents/保留草稿.cuenote");
    workspace
        .save_current_content_as(&session.session_id, local.clone(), output.clone())
        .unwrap();
    assert_eq!(
        storage::open_note(&output).unwrap().package.content(),
        local
    );
    assert_eq!(fs::read(path).unwrap(), disk);
}

#[test]
fn returning_from_preserved_preview_reopens_only_the_host_authorized_external_file() {
    let (temp, mut workspace) = setup();
    let path = external_file(temp.path());
    let original = workspace.open_external_note(path).unwrap();
    let versions = workspace.list_preserved_versions().unwrap();
    let preview = workspace
        .open_preserved_version(&versions.versions[0].version_id)
        .unwrap();
    let copy_path = temp.path().join("documents/历史副本.cuenote");
    workspace
        .save_preserved_as(&versions.versions[0].version_id, copy_path.clone())
        .unwrap();
    assert_eq!(
        storage::open_note(&copy_path).unwrap().package.content(),
        original.content
    );
    assert!(matches!(preview.origin, SessionOrigin::PreservedPreview));
    assert_eq!(
        workspace
            .reopen_external_note(&original.session_id)
            .unwrap_err()
            .kind,
        "staleSession"
    );
    let returned = workspace.reopen_external_note(&preview.session_id).unwrap();
    assert_eq!(returned.note_id, original.note_id);
    assert_eq!(returned.origin, original.origin);
    assert!(returned.read_only_reason.is_none());
}

#[test]
fn active_external_file_stays_out_of_search_and_backup_while_library_changes_refresh() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let path = external_file(temp.path());
    let external = workspace.open_external_note(path).unwrap();
    let mut second = Workspace::new(
        temp.path().join("second-settings"),
        temp.path().join("second-recovery"),
        temp.path().join("library"),
    );
    second.use_default_library().unwrap();
    let local = second.new_note().unwrap();
    let mut content = local.content;
    content.title = "后来出现的库内笔记".into();
    second
        .save_note_content(&local.session_id, content)
        .unwrap();
    let probe = workspace
        .probe_external_changes(Some(&external.session_id))
        .unwrap();
    assert_eq!(probe.library.unwrap().notes.len(), 1);
    assert_eq!(
        workspace.search_notes("独立笔记", None, 0).unwrap().total,
        0
    );
    assert_eq!(
        workspace.search_notes("后来出现", None, 0).unwrap().total,
        1
    );
    let context = workspace.backup_context().unwrap();
    let control = cuenote_lib::workspace::BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let backup = context
        .run(
            cuenote_lib::workspace::BackupKind::Manual,
            "2026-10-15",
            &job,
        )
        .unwrap()
        .created
        .unwrap();
    assert_eq!(backup.note_count, 1);
    assert!(!Path::new(&backup.path)
        .join("notes")
        .join(format!("{}.cuenote", external.note_id))
        .exists());
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().session_id,
        external.session_id
    );
}

#[test]
fn failed_external_open_preserves_active_session_and_protects_managed_files() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let local = workspace.new_note().unwrap();
    workspace
        .save_note_content(&local.session_id, local.content)
        .unwrap();
    let local_path = temp
        .path()
        .join("library/notes")
        .join(format!("{}.cuenote", local.note_id));
    let path = external_file(temp.path());
    let session = workspace.open_external_note(path.clone()).unwrap();
    let bytes = fs::read(&path).unwrap();
    let custom_backup = temp.path().join("custom-backups");
    workspace
        .set_backup_directory(custom_backup.clone())
        .unwrap();
    fs::create_dir_all(&custom_backup).unwrap();
    let backup_note = custom_backup.join("retained.cuenote");
    fs::write(&backup_note, &bytes).unwrap();
    let config_note = temp.path().join("settings/config.cuenote");
    fs::write(&config_note, &bytes).unwrap();
    let recovery_note = temp.path().join("recovery/retained.cuenote");
    fs::write(&recovery_note, &bytes).unwrap();
    for protected in [local_path, backup_note, config_note, recovery_note] {
        let before = fs::read(&protected).unwrap();
        assert_eq!(
            workspace
                .open_external_note(protected.clone())
                .unwrap_err()
                .kind,
            "unsafePath"
        );
        assert_eq!(fs::read(protected).unwrap(), before);
        assert_eq!(
            workspace.snapshot().unwrap().session.unwrap().session_id,
            session.session_id
        );
    }
    let corrupt = temp.path().join("documents/corrupt.cuenote");
    fs::write(&corrupt, b"not a native package").unwrap();
    assert!(workspace.open_external_note(corrupt).is_err());
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().session_id,
        session.session_id
    );
}

#[test]
fn missing_notebook_attribution_does_not_disable_external_draft_protection() {
    let (temp, mut workspace) = setup();
    let path = external_file(temp.path());
    let mut package = storage::open_note(&path).unwrap().package;
    package.document.organization.notebook_id = None;
    write_package(&mut File::create(&path).unwrap(), &package).unwrap();
    let session = workspace.open_external_note(path.clone()).unwrap();
    let mut draft = session.content;
    draft.title = "路径不再适合写入时仍须保留".into();
    fs::create_dir(path.parent().unwrap().join("notes")).unwrap();
    fs::create_dir(path.parent().unwrap().join("notebooks")).unwrap();
    let error = workspace
        .save_external_note_content(&session.session_id, draft.clone())
        .unwrap_err();
    assert_eq!(error.kind, "unsafePath");
    let recovered = storage::open_note(Path::new(
        error
            .recovery_path
            .as_ref()
            .expect("writable external draft must be retained"),
    ))
    .unwrap();
    assert_eq!(recovered.package.content(), draft);
    assert_eq!(storage::open_note(&path).unwrap().package, package);
}
