#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use cuenote_lib::note_format::write_package;
use cuenote_lib::note_format::PlainText;
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod support;

const ORIGINAL: &str = "已确认保存的原件";
const DRAFT: &str = "中断前请求保存的新草稿 📝";

fn setup() -> (tempfile::TempDir, String) {
    let temp = support::tempdir("interruptions-");
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

fn paused_probe_with_noncooperating_write(
    root: &Path,
    note_id: &str,
    target: &Path,
) -> (Output, String) {
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
        "Build the storage_fault_probe example first"
    );
    let mut child = Command::new(binary)
        .arg(root)
        .arg(note_id)
        .env("CUENOTE_FAULT_ROOT", root)
        .env("CUENOTE_FAULT_STAGE", "publish_before")
        .env("CUENOTE_FAULT_ACTION", "pause")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let reached = root.join(".cuenote-fault-hit-publish_before");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !reached.is_file() && std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("probe exited before the publish checkpoint: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !reached.is_file() {
        let _ = child.kill();
        panic!("probe did not reach publish_before");
    }

    let mut external = storage::open_note(target).unwrap().package;
    let mut external_text = external.text_projection();
    external_text.content = "非协作写入者发布的有效版本 B".into();
    external.replace_plain_text(external_text).unwrap();
    let mut encoded = Cursor::new(Vec::new());
    write_package(&mut encoded, &external).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(target)
        .unwrap();
    file.write_all(&encoded.into_inner()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let external_revision = storage::open_note(target).unwrap().revision;

    std::fs::write(root.join(".cuenote-fault-resume"), b"resume\n").unwrap();
    (child.wait_with_output().unwrap(), external_revision)
}

fn recovery_packages(root: &Path) -> Vec<storage::OpenedNote> {
    fn collect(directory: &Path, packages: &mut Vec<storage::OpenedNote>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                collect(&path, packages);
            } else if metadata.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("cuenote")
            {
                packages.push(storage::open_note(&path).unwrap());
            }
        }
    }

    let mut packages = Vec::new();
    collect(&root.join("recovery"), &mut packages);
    packages
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
        "publish_os_returned",
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
        let expected = if matches!(stage, "publish_os_returned" | "publish_after") {
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

#[test]
fn noncooperating_valid_write_after_final_hash_is_reported_as_conflict() {
    let (temp, note_id) = setup();
    let root = temp.path();
    let target = root
        .join("library/notes")
        .join(format!("{note_id}.cuenote"));
    let original_revision = storage::open_note(&target).unwrap().revision;

    let (output, external_revision) =
        paused_probe_with_noncooperating_write(root, &note_id, &target);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["kind"], "conflict", "{error}");
    let recovery = recovery_packages(root);
    assert!(
        recovery
            .iter()
            .any(|version| version.revision == original_revision),
        "the previously held version A must remain readable"
    );
    assert!(
        recovery
            .iter()
            .any(|version| version.revision == external_revision),
        "the displaced noncooperating version B must remain readable"
    );
    assert!(
        recovery
            .iter()
            .any(|version| { version.package.text_projection().content == DRAFT }),
        "the interrupted local version C must remain readable"
    );
}

#[test]
fn cooperative_subprocess_writers_cannot_both_publish_from_the_same_revision() {
    const WRITER_A: &str = "协作写入者 A 的新版本";
    const WRITER_B: &str = "协作写入者 B 的新版本";

    let (temp, note_id) = setup();
    let root = temp.path();
    let target = root
        .join("library/notes")
        .join(format!("{note_id}.cuenote"));
    let original_revision = storage::open_note(&target).unwrap().revision;
    let barrier = root.join("cooperative-writers");
    std::fs::create_dir(&barrier).unwrap();
    let gate = barrier.join("start");
    let debug = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let binary = debug.join("examples").join(format!(
        "storage_fault_probe{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        binary.is_file(),
        "Build the storage_fault_probe example before this test"
    );

    let writers = [("writer-a", WRITER_A), ("writer-b", WRITER_B)];
    let mut children = Vec::new();
    let mut ready_files = Vec::new();
    for (name, content) in writers {
        let ready = barrier.join(format!("{name}.ready"));
        children.push(
            Command::new(&binary)
                .arg(root)
                .arg(&note_id)
                .arg(content)
                .arg(&ready)
                .arg(&gate)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        ready_files.push(ready);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !ready_files.iter().all(|path| path.is_file()) {
        let mut exited_early = None;
        for (index, child) in children.iter_mut().enumerate() {
            if let Some(status) = child.try_wait().unwrap() {
                exited_early = Some((index, status));
                break;
            }
        }
        if let Some((index, status)) = exited_early {
            for child in &mut children {
                let _ = child.kill();
            }
            for child in &mut children {
                let _ = child.wait();
            }
            panic!("cooperative writer {index} exited before the start gate: {status}");
        }
        if std::time::Instant::now() >= deadline {
            for child in &mut children {
                let _ = child.kill();
            }
            for child in &mut children {
                let _ = child.wait();
            }
            panic!("both cooperative writers did not open the same note in time");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::write(&gate, b"go\n").unwrap();

    let outputs = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect::<Vec<_>>();
    let mut winner = None;
    let mut loser = None;
    for ((name, content), output) in writers.into_iter().zip(outputs) {
        if output.status.success() {
            assert!(
                winner.replace(content).is_none(),
                "both writers published: {output:?}"
            );
        } else {
            let error: serde_json::Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|_| panic!("{name} returned malformed error: {output:?}"));
            assert_eq!(error["kind"], "conflict", "{name}: {error}");
            assert!(
                loser.replace(content).is_none(),
                "both writers conflicted: {output:?}"
            );
        }
    }
    let winner = winner.expect("one serialized writer must publish");
    let loser = loser.expect("the stale writer must receive a conflict");
    assert_eq!(
        storage::open_note(&target)
            .unwrap()
            .package
            .text_projection()
            .content,
        winner
    );

    let recovery = recovery_packages(root);
    assert!(
        recovery
            .iter()
            .any(|version| version.revision == original_revision),
        "the common base version must remain readable"
    );
    assert!(
        recovery
            .iter()
            .any(|version| version.package.text_projection().content == loser),
        "the losing cooperative draft must remain readable"
    );
}

#[test]
fn starting_another_workspace_does_not_recover_an_active_publication() {
    let (temp, note_id) = setup();
    let root = temp.path();
    let debug = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let binary = debug.join("examples").join(format!(
        "storage_fault_probe{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut child = Command::new(binary)
        .arg(root)
        .arg(&note_id)
        .env("CUENOTE_FAULT_ROOT", root)
        .env("CUENOTE_FAULT_STAGE", "publish_os_returned")
        .env("CUENOTE_FAULT_ACTION", "pause")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let reached = root.join(".cuenote-fault-hit-publish_os_returned");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !reached.is_file() && std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !reached.is_file() {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!("writer did not reach publication checkpoint: {output:?}");
    }
    // Startup recovery must distinguish another live writer from a crashed one.
    let startup = reopen(root).snapshot();
    std::fs::write(root.join(".cuenote-fault-resume"), b"resume\n").unwrap();
    let output = child.wait_with_output().unwrap();
    startup.unwrap();
    assert!(
        output.status.success(),
        "startup interfered with the live save: {output:?}"
    );
    assert_eq!(
        storage::open_note(
            &root
                .join("library/notes")
                .join(format!("{note_id}.cuenote"))
        )
        .unwrap()
        .package
        .text_projection()
        .content,
        DRAFT
    );
}

#[test]
fn ordinary_saved_versions_share_the_note_origin_and_rotate() {
    let (temp, note_id) = setup();
    let mut workspace = reopen(temp.path());
    workspace.snapshot().unwrap();
    let mut session = workspace.open_note(&note_id).unwrap();
    for index in 0..34 {
        let mut text = session.text.clone();
        text.content = format!("ordinary revision {index}");
        session = workspace
            .save_note_text(&session.session_id, text)
            .unwrap()
            .session;
    }
    let versions = workspace.list_preserved_versions().unwrap().versions;
    let ordinary: Vec<_> = versions
        .iter()
        .filter(|version| !version.conflict && version.note_id == note_id)
        .collect();
    assert!(
        ordinary.len() <= 31,
        "30 versions plus a leased baseline is the bound, found {}",
        ordinary.len()
    );
    assert!(
        ordinary
            .iter()
            .all(|version| version.origin.ends_with(&format!("{note_id}.cuenote"))),
        "temporary publication paths are not note origins"
    );
}
