#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use cuenote_lib::note_format::PlainText;
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORIGINAL: &str = "已确认保存的原件";
const DRAFT: &str = "中断前请求保存的新草稿 📝";

fn setup() -> (tempfile::TempDir, String) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("interruptions-")
        .tempdir_in(parent)
        .unwrap();
    let mut workspace = reopen(temp.path());
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    workspace
        .save_note_text(
            &session.session_id,
            PlainText {
                title: "阶段中断验证".into(),
                cue: "旧版与恢复快照".into(),
                content: ORIGINAL.into(),
                summary: "完整原生文件".into(),
            },
        )
        .unwrap();
    (temp, session.note_id)
}

fn reopen(root: &Path) -> Workspace {
    Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    )
}

fn probe(root: &Path, note_id: &str, stage: &str, action: &str) -> Output {
    let debug = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let binary: PathBuf = debug.join("examples").join(format!(
        "storage_fault_probe{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        binary.is_file(),
        "Build the storage_fault_probe example before this matrix"
    );
    Command::new(binary)
        .arg(root)
        .arg(note_id)
        .env("CUENOTE_FAULT_ROOT", root)
        .env("CUENOTE_FAULT_STAGE", stage)
        .env("CUENOTE_FAULT_ACTION", action)
        .output()
        .unwrap()
}

#[test]
fn process_exit_at_each_save_stage_preserves_readable_original_and_recoverable_draft() {
    for stage in [
        "recovery_snapshot_persisted",
        "candidate_created",
        "candidate_synced_before_validation",
        "candidate_validated",
        "history_persisted",
        "publish_before",
        "publish_after",
    ] {
        let (temp, note_id) = setup();
        let root = temp.path();
        let output = probe(root, &note_id, stage, "exit");
        assert_eq!(output.status.code(), Some(86), "{stage}: {output:?}");
        assert!(
            root.join(format!(".cuenote-fault-hit-{stage}")).is_file(),
            "{stage}"
        );
        let original_path = root
            .join("library/notes")
            .join(format!("{note_id}.cuenote"));
        let original = storage::open_note(&original_path).unwrap();
        let expected = if stage == "publish_after" {
            DRAFT
        } else {
            ORIGINAL
        };
        assert_eq!(
            original.package.text_projection().content,
            expected,
            "{stage}"
        );

        // A new workspace is the same public restart/recovery boundary the UI uses.
        let mut restarted = reopen(root);
        restarted.snapshot().unwrap();
        let list = restarted.list_recovery_drafts().unwrap();
        assert!(list.warnings.is_empty(), "{stage}: {:?}", list.warnings);
        assert_eq!(list.drafts.len(), 1, "{stage}");
        let restored = restarted
            .restore_recovery_as_copy(&list.drafts[0].recovery_id)
            .unwrap();
        assert_ne!(restored.session.note_id, note_id, "{stage}");
        assert_eq!(restored.session.text.content, DRAFT, "{stage}");
        assert_eq!(
            storage::open_note(&original_path).unwrap().revision,
            original.revision,
            "{stage}"
        );
        let restored_path = root
            .join("library/notes")
            .join(format!("{}.cuenote", restored.session.note_id));
        assert_eq!(
            storage::open_note(&restored_path)
                .unwrap()
                .package
                .text_projection()
                .content,
            DRAFT,
            "{stage}"
        );
        println!(
            "{}",
            serde_json::json!({
                "stage": stage,
                "exitCode": output.status.code(),
                "originalRevision": original.revision,
                "originalContent": expected,
                "recoveryRevision": list.drafts[0].revision,
                "restoredNoteId": restored.session.note_id,
                "restoredRevision": restored.session.revision,
                "verdict": "PASS"
            })
        );
    }
}

#[test]
fn simulated_disk_full_and_invalid_candidate_never_replace_the_original() {
    for (stage, action) in [
        ("candidate_created", "disk_full"),
        ("candidate_synced_before_validation", "corrupt"),
        ("publish_before", "io"),
    ] {
        let (temp, note_id) = setup();
        let root = temp.path();
        let original_path = root
            .join("library/notes")
            .join(format!("{note_id}.cuenote"));
        let original = storage::open_note(&original_path).unwrap();
        let output = probe(root, &note_id, stage, action);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{stage}/{action}: {output:?}"
        );
        assert!(root.join(format!(".cuenote-fault-hit-{stage}")).is_file());
        let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(error["recoveryPath"].is_string(), "{error}");
        assert_eq!(
            storage::open_note(&original_path).unwrap().revision,
            original.revision
        );
        let mut restarted = reopen(root);
        restarted.snapshot().unwrap();
        let list = restarted.list_recovery_drafts().unwrap();
        assert!(!list.drafts.is_empty(), "{stage}/{action}");
        let restored = restarted
            .restore_recovery_as_copy(&list.drafts[0].recovery_id)
            .unwrap();
        assert_eq!(restored.session.text.content, DRAFT);
        println!(
            "{}",
            serde_json::json!({
                "stage": stage, "action": action, "exitCode": output.status.code(),
                "originalRevision": original.revision, "recoveryPath": error["recoveryPath"],
                "restoredRevision": restored.session.revision, "verdict": "PASS"
            })
        );
    }
}

#[test]
fn failed_publication_does_not_rotate_away_previously_retained_history() {
    let (temp, note_id) = setup();
    let root = temp.path();
    let mut workspace = reopen(root);
    workspace.snapshot().unwrap();
    let session = workspace.open_note(&note_id).unwrap();
    for index in 1..=32 {
        let mut text = session.text.clone();
        text.content = format!("已经保存的版本 {index}");
        workspace.save_note_text(&session.session_id, text).unwrap();
    }
    fn history_versions(root: &Path) -> std::collections::BTreeSet<String> {
        let mut versions = std::collections::BTreeSet::new();
        for directory in std::fs::read_dir(root.join("recovery/history")).unwrap() {
            for file in std::fs::read_dir(directory.unwrap().path()).unwrap() {
                let path = file.unwrap().path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "cuenote")
                {
                    versions.insert(storage::open_note(&path).unwrap().revision);
                }
            }
        }
        versions
    }
    let before = history_versions(root);
    assert_eq!(before.len(), 30);
    let original_path = root
        .join("library/notes")
        .join(format!("{note_id}.cuenote"));
    let original = storage::open_note(&original_path).unwrap();
    let output = probe(root, &note_id, "publish_before", "io");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(root.join(".cuenote-fault-hit-publish_before").is_file());
    let after = history_versions(root);
    assert!(before.is_subset(&after));
    assert!(after.contains(&original.revision));
    assert_eq!(
        storage::open_note(&original_path).unwrap().revision,
        original.revision
    );
    println!(
        "{}",
        serde_json::json!({
            "case": "failed-publication-history-retention",
            "historyBefore": before.len(), "historyAfter": after.len(),
            "originalRevision": original.revision, "verdict": "PASS"
        })
    );
}
