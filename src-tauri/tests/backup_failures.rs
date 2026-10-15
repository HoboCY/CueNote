#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use chrono::{Duration, NaiveDate};
use cuenote_lib::note_format::{Block, Inline, MindMapNode, NoteContent, PlainText, Sections};
use cuenote_lib::storage;
use cuenote_lib::workspace::{BackupControl, BackupKind, BackupSummary, Workspace};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_temp(prefix: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .unwrap()
}

fn workspace_at(root: &Path) -> Workspace {
    let library = root.join("library");
    Workspace::new(root.join("settings"), root.join("recovery"), library)
}

fn reopen_workspace(root: &Path) -> Workspace {
    let mut workspace = workspace_at(root);
    workspace.snapshot().expect("reopen isolated workspace");
    workspace
        .select_library(root.join("library"))
        .expect("select isolated library");
    workspace
}

fn read_tree(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(
                !metadata.file_type().is_symlink(),
                "unexpected symlink: {path:?}"
            );
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    if directory.is_dir() {
        visit(directory, directory, &mut files);
    }
    files
}

fn backup_files(backups: &[BackupSummary]) -> BTreeMap<String, BTreeMap<PathBuf, Vec<u8>>> {
    backups
        .iter()
        .map(|backup| {
            let path = PathBuf::from(&backup.path);
            (backup.backup_id.clone(), read_tree(&path))
        })
        .collect()
}

fn backup_ids(backups: &[BackupSummary]) -> BTreeSet<String> {
    backups
        .iter()
        .map(|backup| backup.backup_id.clone())
        .collect()
}

fn recovery_history(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    read_tree(&root.join("recovery").join("history"))
}

fn staging_directories(root: &Path) -> Vec<PathBuf> {
    fn visit(current: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".staging-"))
            {
                found.push(path);
            } else {
                visit(&path, found);
            }
        }
    }

    let mut found = Vec::new();
    visit(root, &mut found);
    found.sort();
    found
}

fn probe_binary() -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let debug = executable.parent().unwrap().parent().unwrap();
    let binary = debug.join("examples").join(format!(
        "backup_fault_probe{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        binary.is_file(),
        "build the backup_fault_probe example first"
    );
    binary
}

fn run_backup_probe(root: &Path, kind: &str, local_date: &str, action: &str) -> Output {
    Command::new(probe_binary())
        .arg(root)
        .arg(kind)
        .arg(local_date)
        // The root encloses both source files and the isolated backup destination.
        .env("CUENOTE_FAULT_ROOT", root)
        .env("CUENOTE_FAULT_STAGE", "backup_file_copied")
        .env("CUENOTE_FAULT_ACTION", action)
        .output()
        .unwrap()
}

fn create_backup(
    context: &cuenote_lib::workspace::BackupContext,
    control: &BackupControl,
    kind: BackupKind,
    local_date: &str,
) -> BackupSummary {
    let job = control.begin_job(None).unwrap();
    context
        .run(kind, local_date, &job)
        .unwrap()
        .created
        .expect("fixture backup should create a snapshot")
}

#[test]
fn native_backup_round_trip_preserves_rich_packages_assets_empty_notebooks_and_trash() {
    let temp = workspace_temp("backup-rich-");
    let root = temp.path();
    let mut workspace = workspace_at(root);
    let initial = workspace
        .select_library(root.join("library"))
        .expect("select isolated library")
        .library
        .unwrap();
    let empty_notebook_id = workspace
        .create_notebook("空白笔记本")
        .unwrap()
        .notebooks
        .into_iter()
        .find(|notebook| notebook.name == "空白笔记本")
        .unwrap()
        .notebook_id;
    let note = workspace.new_note().unwrap();
    let image_bytes = include_bytes!("fixtures/pixel.png").to_vec();
    let image = workspace
        .stage_image(&note.session_id, image_bytes.clone())
        .unwrap();
    let map_root = uuid::Uuid::new_v4().to_string();
    let map_child = uuid::Uuid::new_v4().to_string();
    let expected = NoteContent {
        title: "备份原生富内容".into(),
        sections: Sections {
            cue: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "线索分区仍然存在".into(),
                    marks: Vec::new(),
                }],
            }],
            content: vec![
                Block::Image {
                    asset_id: image.id.clone(),
                    display_width_px: image.width,
                },
                Block::MindMap {
                    map_id: uuid::Uuid::new_v4().to_string(),
                    root_id: map_root.clone(),
                    nodes: vec![
                        MindMapNode {
                            id: map_root,
                            text: "可编辑根节点".into(),
                            children: vec![map_child.clone()],
                        },
                        MindMapNode {
                            id: map_child,
                            text: "可编辑子节点".into(),
                            children: Vec::new(),
                        },
                    ],
                },
                Block::MathBlock {
                    latex: "\\frac{a+b}{c}".into(),
                },
                Block::CodeBlock {
                    language: Some("rust".into()),
                    code: "fn backup_is_native() -> bool { true }".into(),
                },
            ],
            summary: vec![Block::Paragraph {
                children: vec![Inline::Text {
                    text: "总结分区仍然存在".into(),
                    marks: Vec::new(),
                }],
            }],
        },
    };
    workspace
        .save_note_content(&note.session_id, expected.clone())
        .unwrap();
    workspace.trash_note(&note.session_id).unwrap();

    let source_path = root
        .join("library")
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let source_bytes = fs::read(&source_path).unwrap();
    let source_package = storage::open_note(&source_path).unwrap().package;
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let summary = create_backup(&context, &control, BackupKind::Manual, "2026-10-15");
    let backup_path = PathBuf::from(&summary.path);
    let backup_note_path = backup_path
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));

    assert_eq!(summary.kind, BackupKind::Manual);
    assert_eq!(fs::read(&backup_note_path).unwrap(), source_bytes);
    let backup_package = storage::open_note(&backup_note_path).unwrap().package;
    assert_eq!(backup_package, source_package);
    assert_eq!(backup_package.content(), expected);
    assert_eq!(backup_package.assets.get(&image.id).unwrap(), &image_bytes);
    assert!(backup_package.document.organization.trashed_at.is_some());
    assert!(backup_path
        .join("notebooks")
        .join(format!("{empty_notebook_id}.json"))
        .is_file());
    assert!(initial.notebooks.iter().any(|notebook| notebook.is_default));

    drop(workspace);
    let mut restarted = reopen_workspace(root);
    let view = restarted.snapshot().unwrap().library.unwrap();
    assert!(view.notebooks.iter().any(|notebook| {
        notebook.notebook_id == empty_notebook_id && notebook.name == "空白笔记本"
    }));
    let reopened = restarted.open_note(&note.note_id).unwrap();
    assert!(reopened.trashed_at.is_some());
    assert_eq!(reopened.content, expected);
    assert_eq!(
        restarted
            .read_image_asset(&reopened.session_id, &image.id)
            .unwrap(),
        image_bytes
    );
}

#[test]
fn copy_process_exit_and_disk_full_keep_existing_backups_history_and_unlisted_staging() {
    let temp = workspace_temp("backup-failures-");
    let root = temp.path();
    let mut workspace = workspace_at(root);
    let view = workspace
        .select_library(root.join("library"))
        .expect("select isolated library")
        .library
        .unwrap();
    workspace.create_notebook("保留空笔记本").unwrap();
    let note = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "故障前已有版本".into(),
                cue: "恢复历史基线".into(),
                content: "稳定保存的内容".into(),
                summary: String::new(),
            },
        )
        .unwrap();
    let reopened_note = workspace.open_note(&note.note_id).unwrap();
    let mut next_version = reopened_note.text.clone();
    next_version.content = "已保存并有恢复历史的内容".into();
    workspace
        .save_note_text(&reopened_note.session_id, next_version)
        .unwrap();
    let default_notebook = view
        .notebooks
        .iter()
        .find(|notebook| notebook.is_default)
        .unwrap();
    let default_metadata = fs::read(
        root.join("library")
            .join("notebooks")
            .join(format!("{}.json", default_notebook.notebook_id)),
    )
    .unwrap();
    let conflict_path = root.join("library").join("notebooks").join(format!(
        ".cuenote-notebook-conflict-{}.json",
        uuid::Uuid::new_v4()
    ));
    fs::write(&conflict_path, &default_metadata).unwrap();
    let context = workspace.backup_context().unwrap();
    let control = BackupControl::default();
    let manual = create_backup(&context, &control, BackupKind::Manual, "2026-09-15");
    let first_automatic_day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
    let mut automatic_backups = Vec::with_capacity(30);
    for day in 0..30 {
        let local_date = first_automatic_day
            .checked_add_signed(Duration::days(day))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        let current = workspace.open_note(&note.note_id).unwrap();
        let mut saved_version = current.text.clone();
        saved_version.content = format!("阈值前已保存版本 {local_date}");
        workspace
            .save_note_text(&current.session_id, saved_version)
            .unwrap();
        let automatic = create_backup(&context, &control, BackupKind::Automatic, &local_date);
        assert_eq!(automatic.local_date, local_date);
        automatic_backups.push(automatic);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(automatic_backups.len(), 30);

    let trigger_date = first_automatic_day
        .checked_add_signed(Duration::days(30))
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    let current = workspace.open_note(&note.note_id).unwrap();
    let mut change_after_baseline = current.text.clone();
    change_after_baseline.content = "第 31 个观察日期的已保存修改".into();
    workspace
        .save_note_text(&current.session_id, change_after_baseline)
        .unwrap();
    let before_job = control.begin_job(None).unwrap();
    let before_catalog = context.catalog(&before_job).unwrap();
    assert_eq!(before_catalog.backups.len(), 31);
    assert_eq!(
        before_catalog
            .backups
            .iter()
            .filter(|backup| backup.kind == BackupKind::Automatic)
            .count(),
        30
    );
    assert_eq!(
        before_catalog
            .backups
            .iter()
            .filter(|backup| backup.kind == BackupKind::Manual)
            .count(),
        1
    );
    assert_eq!(automatic_backups[0].local_date, "2026-09-16");
    assert_eq!(automatic_backups[29].local_date, "2026-10-15");
    let oldest_automatic = before_catalog
        .backups
        .iter()
        .filter(|backup| backup.kind == BackupKind::Automatic)
        .min_by(|left, right| left.created_at.cmp(&right.created_at))
        .unwrap();
    assert_eq!(oldest_automatic.backup_id, automatic_backups[0].backup_id);
    let oldest_automatic_id = oldest_automatic.backup_id.clone();
    let expected_ids = backup_ids(&before_catalog.backups);
    assert_eq!(expected_ids.len(), 31);
    let old_backup_files = backup_files(&before_catalog.backups);
    let manual_backup_files = old_backup_files.get(&manual.backup_id).unwrap().clone();
    let conflict_bytes = fs::read(&conflict_path).unwrap();
    let conflict_name = conflict_path.file_name().unwrap();
    assert!(old_backup_files.values().all(|files| {
        files.iter().any(|(path, bytes)| {
            path.file_name() == Some(conflict_name) && bytes == &conflict_bytes
        })
    }));
    let history_before = recovery_history(root);
    assert!(
        !history_before.is_empty(),
        "fixture must contain recovery history"
    );
    drop(workspace);

    let marker = root.join(".cuenote-fault-hit-backup_file_copied");
    let exit_output = run_backup_probe(root, "automatic", &trigger_date, "exit");
    assert_eq!(exit_output.status.code(), Some(86), "{exit_output:?}");
    assert!(
        marker.is_file(),
        "child must reach the post-copy checkpoint"
    );
    let orphan_staging = staging_directories(&root.join("backups"));
    assert_eq!(
        orphan_staging.len(),
        1,
        "process exit should leave one isolated staging directory"
    );

    let after_exit = reopen_workspace(root);
    let exit_context = after_exit.backup_context().unwrap();
    let exit_job = BackupControl::default().begin_job(None).unwrap();
    let exit_catalog = exit_context.catalog(&exit_job).unwrap();
    assert_eq!(backup_ids(&exit_catalog.backups), expected_ids);
    assert_eq!(backup_files(&exit_catalog.backups), old_backup_files);
    assert_eq!(fs::read(&conflict_path).unwrap(), conflict_bytes);
    let history_after_exit = recovery_history(root);
    assert!(history_before.iter().all(|(path, bytes)| {
        history_after_exit
            .get(path)
            .is_some_and(|retained| retained == bytes)
    }));
    assert!(!exit_catalog.backups.iter().any(|backup| {
        Path::new(&backup.path)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".staging-"))
    }));
    drop(after_exit);

    fs::remove_file(&marker).unwrap();
    let disk_full_output = run_backup_probe(root, "automatic", &trigger_date, "disk_full");
    assert_eq!(
        disk_full_output.status.code(),
        Some(1),
        "{disk_full_output:?}"
    );
    assert!(
        marker.is_file(),
        "disk-full injection must reach the copy checkpoint"
    );
    let disk_full_error: serde_json::Value =
        serde_json::from_slice(&disk_full_output.stdout).unwrap();
    assert_eq!(disk_full_error["kind"], "io");
    assert!(disk_full_error["message"]
        .as_str()
        .unwrap_or_default()
        .contains("隔离故障注入"));

    let after_disk_full = reopen_workspace(root);
    let full_context = after_disk_full.backup_context().unwrap();
    let full_job = BackupControl::default().begin_job(None).unwrap();
    let full_catalog = full_context.catalog(&full_job).unwrap();
    assert_eq!(backup_ids(&full_catalog.backups), expected_ids);
    assert_eq!(backup_files(&full_catalog.backups), old_backup_files);
    assert_eq!(staging_directories(&root.join("backups")), orphan_staging);
    assert_eq!(fs::read(&conflict_path).unwrap(), conflict_bytes);
    let history_after_disk_full = recovery_history(root);
    assert!(history_before.iter().all(|(path, bytes)| {
        history_after_disk_full
            .get(path)
            .is_some_and(|retained| retained == bytes)
    }));
    drop(full_job);

    let restart_job = BackupControl::default().begin_job(None).unwrap();
    let created_after_restart = full_context
        .run(BackupKind::Automatic, &trigger_date, &restart_job)
        .expect("normal restart should permit a fresh verified backup");
    let new_summary = created_after_restart
        .created
        .expect("normal restart should publish the pending automatic backup");
    assert_eq!(new_summary.kind, BackupKind::Automatic);
    assert_eq!(new_summary.local_date, trigger_date);
    drop(restart_job);
    let catalog_after_restart_job = BackupControl::default().begin_job(None).unwrap();
    let catalog_after_restart = full_context.catalog(&catalog_after_restart_job).unwrap();
    assert_eq!(catalog_after_restart.backups.len(), 31);
    assert!(catalog_after_restart
        .backups
        .iter()
        .any(|backup| backup.backup_id == new_summary.backup_id));
    assert_eq!(
        catalog_after_restart
            .backups
            .iter()
            .filter(|backup| backup.kind == BackupKind::Automatic)
            .count(),
        30
    );
    assert_eq!(
        catalog_after_restart
            .backups
            .iter()
            .filter(|backup| backup.kind == BackupKind::Manual)
            .count(),
        1
    );
    let retained_ids = backup_ids(&catalog_after_restart.backups);
    assert!(!retained_ids.contains(&oldest_automatic_id));
    assert!(automatic_backups
        .iter()
        .skip(1)
        .all(|backup| { retained_ids.contains(&backup.backup_id) }));
    assert!(retained_ids.contains(&manual.backup_id));
    let files_after_restart = backup_files(&catalog_after_restart.backups);
    assert_eq!(
        files_after_restart.get(&manual.backup_id),
        Some(&manual_backup_files)
    );
    let new_note_path = PathBuf::from(&new_summary.path)
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let new_note = storage::open_note(&new_note_path).unwrap().package;
    assert_eq!(
        new_note.text_projection().content,
        "第 31 个观察日期的已保存修改"
    );
    assert_eq!(staging_directories(&root.join("backups")), orphan_staging);
    assert!(!catalog_after_restart.backups.iter().any(|backup| {
        Path::new(&backup.path)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".staging-"))
    }));
    assert!(expected_ids.contains(&manual.backup_id));
    assert!(expected_ids.contains(&oldest_automatic_id));

    println!(
        "{}",
        serde_json::json!({
            "case": "backup-copy-failure-recovery",
            "faultStage": "backup_file_copied",
            "triggerDate": trigger_date,
            "seededAutomaticBackups": automatic_backups.len(),
            "seededManualBackups": 1,
            "completeBackupsBeforeFailure": expected_ids.len(),
            "completeBackupsAfterEachFailure": expected_ids.len(),
            "actions": [
                { "action": "exit", "exitCode": exit_output.status.code(), "completeBackups": expected_ids.len() },
                { "action": "disk_full", "exitCode": disk_full_output.status.code(), "errorKind": disk_full_error["kind"], "completeBackups": expected_ids.len() }
            ],
            "automaticBackupsAfterRetry": 30,
            "manualBackupsAfterRetry": 1,
            "oldestAutomaticPrunedAfterSuccess": oldest_automatic_id,
            "manualBackupPreservedAfterSuccess": files_after_restart.get(&manual.backup_id) == Some(&manual_backup_files),
            "recoveryHistoryEntries": history_before.len(),
            "orphanStagingDirectories": orphan_staging.len(),
            "restartBackupId": new_summary.backup_id,
            "verdict": "PASS"
        })
    );
}
