#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use cuenote_lib::note_format::{Block, Inline};
use cuenote_lib::storage;
use cuenote_lib::workspace::{BackupControl, BackupKind, Workspace};
use sha2::Digest;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod support;

const SOURCE_NOTE_ID: &str = "11111111-1111-4111-8111-111111111111";
const CURRENT_ONLY_NOTE_ID: &str = "22222222-2222-4222-8222-222222222222";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    library: PathBuf,
    source_backup: cuenote_lib::workspace::BackupSummary,
    manifest_revision: String,
    source_facts: BTreeMap<String, Vec<u8>>,
    before_facts: BTreeMap<String, Vec<u8>>,
    source_note_path: PathBuf,
    current_only_note_path: PathBuf,
}

fn native_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for (directory, extension) in [("notes", "cuenote"), ("notebooks", "json")] {
        for entry in fs::read_dir(root.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                files.insert(
                    format!(
                        "{directory}/{}",
                        path.file_name().unwrap().to_str().unwrap()
                    ),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    files
}

fn isolated_workspace(root: &Path) -> Workspace {
    let library = root.join("library");
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        library.clone(),
    );
    workspace.select_library(library).unwrap();
    workspace
}

fn fixture() -> Fixture {
    fixture_with_payload(0)
}

fn fixture_with_payload(payload_chars: usize) -> Fixture {
    let temp = support::tempdir("library-restore-fault-");
    let root = temp.path().to_path_buf();
    let library = root.join("library");
    let mut workspace = isolated_workspace(&root);

    let selected = workspace.snapshot().unwrap().library.unwrap();
    let default_notebook_id = selected.default_notebook_id.unwrap();
    workspace.create_notebook("源备份中的空笔记本").unwrap();

    let fixture_path = support::fixture_file(&root, "valid-rich-v1.cuenote");
    let mut source_package = storage::open_note(&fixture_path).unwrap().package;
    source_package.document.note_id = SOURCE_NOTE_ID.into();
    source_package.document.organization.notebook_id = Some(default_notebook_id);
    source_package.document.organization.pinned = true;
    source_package.document.organization.trashed_at = Some("2026-10-01T00:00:00.000Z".into());
    if payload_chars > 0 {
        source_package
            .document
            .sections
            .content
            .push(Block::Paragraph {
                children: vec![Inline::Text {
                    text: noisy_hex_text(payload_chars),
                    marks: Vec::new(),
                }],
            });
    }
    assert!(
        !source_package.assets.is_empty(),
        "fixture must retain image bytes"
    );
    let source_note_path = library
        .join("notes")
        .join(format!("{SOURCE_NOTE_ID}.cuenote"));
    storage::save_note(
        &source_note_path,
        &root.join("recovery"),
        &source_package,
        None,
    )
    .unwrap();
    let source_facts = native_files(&library);
    assert!(source_facts.contains_key(&format!("notes/{SOURCE_NOTE_ID}.cuenote")));
    assert!(
        source_facts
            .keys()
            .filter(|path| path.starts_with("notebooks/"))
            .count()
            >= 2
    );

    let control = BackupControl::default();
    let job = control.begin_job(None).unwrap();
    let context = workspace.backup_context().unwrap();
    let source_backup = context
        .run(BackupKind::Manual, "2026-10-15", &job)
        .unwrap()
        .created
        .unwrap();
    let preview = workspace
        .preview_library_restore(&source_backup.backup_id, &job)
        .unwrap();
    drop(job);

    let opened = storage::open_note(&source_note_path).unwrap();
    let mut current_package = opened.package;
    current_package.document.title = "恢复前保护版本".into();
    current_package.document.organization.pinned = false;
    current_package.document.organization.trashed_at = None;
    storage::save_note(
        &source_note_path,
        &root.join("recovery"),
        &current_package,
        Some(&opened.revision),
    )
    .unwrap();

    let mut current_only = current_package;
    current_only.document.note_id = CURRENT_ONLY_NOTE_ID.into();
    current_only.document.title = "仅存在于恢复前的图片笔记".into();
    let current_only_note_path = library
        .join("notes")
        .join(format!("{CURRENT_ONLY_NOTE_ID}.cuenote"));
    storage::save_note(
        &current_only_note_path,
        &root.join("recovery"),
        &current_only,
        None,
    )
    .unwrap();
    workspace
        .create_notebook("仅存在于恢复前的空笔记本")
        .unwrap();
    let before_facts = native_files(&library);
    assert!(before_facts.len() > source_facts.len());

    Fixture {
        _temp: temp,
        root,
        library,
        source_backup,
        manifest_revision: preview.manifest_revision,
        source_facts,
        before_facts,
        source_note_path,
        current_only_note_path,
    }
}

fn noisy_hex_text(length: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut state = 0x7a5b_31d9_2e17_4c63u64;
    let mut text = String::with_capacity(length);
    for _ in 0..length {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        text.push(HEX[(state >> 60) as usize] as char);
    }
    text
}

fn probe_binary() -> PathBuf {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!(
            "library_restore_fault_probe{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        binary.is_file(),
        "build the library_restore_fault_probe example first"
    );
    binary
}

fn probe_command(
    fixture: &Fixture,
    args: &[String],
    stage: Option<&str>,
    action: Option<&str>,
) -> Command {
    let mut command = Command::new(probe_binary());
    command.args(args).env("CUENOTE_FAULT_ROOT", &fixture.root);
    if let Some(stage) = stage {
        command.env("CUENOTE_FAULT_STAGE", stage);
    } else {
        command.env_remove("CUENOTE_FAULT_STAGE");
    }
    if let Some(action) = action {
        command.env("CUENOTE_FAULT_ACTION", action);
    } else {
        command.env_remove("CUENOTE_FAULT_ACTION");
    }
    command
}

fn restore_args(fixture: &Fixture) -> Vec<String> {
    vec![
        "restore".into(),
        fixture.root.to_string_lossy().into_owned(),
        fixture.source_backup.backup_id.clone(),
        fixture.manifest_revision.clone(),
    ]
}

fn resume_args(fixture: &Fixture, action: &str) -> Vec<String> {
    vec![
        "resume".into(),
        fixture.root.to_string_lossy().into_owned(),
        action.into(),
    ]
}

fn inspect(fixture: &Fixture) -> serde_json::Value {
    let args = vec![
        "inspect".into(),
        fixture.root.to_string_lossy().into_owned(),
    ];
    let output = probe_command(fixture, &args, None, None).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_exit_at(fixture: &Fixture, stage: &str, output: &Output) {
    assert!(
        fixture
            .root
            .join(format!(".cuenote-fault-hit-{stage}"))
            .is_file(),
        "stage was not reached: {output:?}"
    );
    assert_eq!(output.status.code(), Some(86), "{output:?}");
}

fn assert_injected_fault(fixture: &Fixture, stage: &str, action: &str, output: &Output) {
    assert!(
        fixture
            .root
            .join(format!(".cuenote-fault-hit-{stage}"))
            .is_file(),
        "stage was not reached: {output:?}"
    );
    if action == "exit" {
        assert_eq!(output.status.code(), Some(86), "{output:?}");
        return;
    }
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "io", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("隔离故障注入"),
        "{response}"
    );
}

fn assert_pending_gate(fixture: &Fixture) -> serde_json::Value {
    let response = inspect(fixture);
    assert!(!response["pending"].is_null(), "{response}");
    assert_eq!(
        response["gateError"]["kind"], "libraryRestorePending",
        "{response}"
    );
    assert!(
        !response["workspace"]["libraryRestore"].is_null(),
        "{response}"
    );
    response
}

fn assert_library_facts(fixture: &Fixture, expected: &BTreeMap<String, Vec<u8>>) {
    assert_library_facts_with_context(fixture, expected, "");
}

fn assert_library_facts_with_context(
    fixture: &Fixture,
    expected: &BTreeMap<String, Vec<u8>>,
    context: &str,
) {
    let actual = native_files(&fixture.library);
    let mismatches = expected
        .keys()
        .chain(actual.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|path| {
            let expected_bytes = expected.get(path);
            let actual_bytes = actual.get(path);
            if expected_bytes == actual_bytes {
                return None;
            }
            let describe = |bytes: Option<&Vec<u8>>| {
                bytes.map_or_else(
                    || "missing".to_owned(),
                    |bytes| format!("{}:{:x}", bytes.len(), sha2::Sha256::digest(bytes)),
                )
            };
            Some(format!(
                "{} expected={} actual={}",
                path,
                describe(expected_bytes),
                describe(actual_bytes)
            ))
        })
        .collect::<Vec<_>>();
    assert!(
        mismatches.is_empty(),
        "native facts differ {context}: {mismatches:?}"
    );
}

fn assert_native_facts_are_planned_versions(fixture: &Fixture) {
    let actual = native_files(&fixture.library);
    let paths = fixture
        .before_facts
        .keys()
        .chain(fixture.source_facts.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(actual.keys().all(|path| paths.contains(path)));
    for path in paths {
        match actual.get(&path) {
            Some(observed) => assert!(
                fixture.before_facts.get(&path) == Some(observed)
                    || fixture.source_facts.get(&path) == Some(observed),
                "{path} has a third version during recovery"
            ),
            None => assert!(
                !fixture.before_facts.contains_key(&path)
                    || !fixture.source_facts.contains_key(&path),
                "unchanged {path} unexpectedly disappeared"
            ),
        }
    }
}

fn assert_source_backup_retained(fixture: &Fixture) {
    assert_eq!(
        native_files(Path::new(&fixture.source_backup.path)),
        fixture.source_facts,
        "the immutable selected backup must retain every original native byte"
    );
}

fn assert_pre_restore_backup_retained(fixture: &Fixture) {
    let source_dir = Path::new(&fixture.source_backup.path);
    let sibling_root = source_dir.parent().unwrap();
    let mut backups = fs::read_dir(sibling_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path != source_dir && path.join("manifest.json").is_file())
        .collect::<Vec<_>>();
    backups.sort();
    assert_eq!(
        backups.len(),
        1,
        "expected one retained pre-restore backup: {backups:?}"
    );
    assert_eq!(native_files(&backups[0]), fixture.before_facts);
}

fn wait_for_pause(fixture: &Fixture, stage: &str, child: &mut Child) {
    let marker = fixture.root.join(format!(".cuenote-fault-hit-{stage}"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !marker.is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "probe exited before pausing at {stage}"
        );
        assert!(Instant::now() < deadline, "probe never reached {stage}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn release_pause(fixture: &Fixture) {
    fs::write(fixture.root.join(".cuenote-fault-resume"), b"continue").unwrap();
}

fn start_paused_restore(fixture: &Fixture, stage: &str) -> Child {
    probe_command(fixture, &restore_args(fixture), Some(stage), Some("pause"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn partial_candidates(fixture: &Fixture) -> Vec<(PathBuf, Vec<u8>)> {
    fs::read_dir(fixture.library.join("notes"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(".cuenote-restore-")
                        && name.ends_with("-forward-candidate.tmp")
                })
        })
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

fn assert_partial_candidate_evidence(
    fixture: &Fixture,
    interrupted_candidate: &Path,
    partial_bytes: &[u8],
) {
    let evidence = fs::read_dir(fixture.library.join("notes"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("tmp"))
        .filter(|path| path != interrupted_candidate)
        .any(|path| fs::read(path).is_ok_and(|bytes| bytes == partial_bytes));
    assert!(
        evidence,
        "the interrupted partial candidate must survive in an evidence artifact"
    );
}

#[test]
fn interrupted_forward_stages_restart_to_exact_source_bytes() {
    for stage in [
        "library_restore_plan_ready",
        "library_restore_marker_published",
        "library_restore_forward_intent",
        "library_fact_candidate_ready",
        "library_fact_before_publish",
        "library_fact_after_publish",
        "library_restore_forward_done",
        "library_restore_terminal",
        "library_restore_marker_removed",
        "library_restore_closed",
        "library_restore_pointer_removed",
    ] {
        let fixture = fixture();
        let output = probe_command(&fixture, &restore_args(&fixture), Some(stage), Some("exit"))
            .output()
            .unwrap();
        assert_exit_at(&fixture, stage, &output);

        let terminal_window = matches!(
            stage,
            "library_restore_terminal"
                | "library_restore_marker_removed"
                | "library_restore_closed"
        );
        let no_active_marker = matches!(
            stage,
            "library_restore_plan_ready" | "library_restore_pointer_removed"
        );
        if stage == "library_restore_plan_ready" {
            assert_library_facts(&fixture, &fixture.before_facts);
            assert!(inspect(&fixture)["pending"].is_null());
            assert_source_backup_retained(&fixture);
            assert_pre_restore_backup_retained(&fixture);
            continue;
        }
        if no_active_marker {
            assert_library_facts(&fixture, &fixture.source_facts);
            assert!(inspect(&fixture)["pending"].is_null());
            assert_source_backup_retained(&fixture);
            assert_pre_restore_backup_retained(&fixture);
            continue;
        }

        let pending = assert_pending_gate(&fixture);
        if terminal_window {
            assert_eq!(pending["pending"]["phase"], "paused", "{pending}");
        }
        let resumed = probe_command(&fixture, &resume_args(&fixture, "continue"), None, None)
            .output()
            .unwrap();
        assert_eq!(resumed.status.code(), Some(0), "stage={stage}: {resumed:?}");
        assert_library_facts(&fixture, &fixture.source_facts);
        assert_source_backup_retained(&fixture);
        assert_pre_restore_backup_retained(&fixture);
    }
}

#[test]
fn rollback_interruption_restarts_in_rollback_direction_and_restores_preimage() {
    for stage in [
        "library_restore_rollback_started",
        "library_restore_rollback_intent",
        "library_restore_rollback_done",
    ] {
        let fixture = fixture();
        let forward = probe_command(
            &fixture,
            &restore_args(&fixture),
            Some("library_fact_after_publish"),
            Some("exit"),
        )
        .output()
        .unwrap();
        assert_exit_at(&fixture, "library_fact_after_publish", &forward);
        assert_pending_gate(&fixture);

        let rollback = probe_command(
            &fixture,
            &resume_args(&fixture, "rollback"),
            Some(stage),
            Some("exit"),
        )
        .output()
        .unwrap();
        assert_exit_at(&fixture, stage, &rollback);
        assert_pending_gate(&fixture);

        // A fresh Continue must honor the durable rollback-started event.
        let resumed = probe_command(&fixture, &resume_args(&fixture, "continue"), None, None)
            .output()
            .unwrap();
        assert_eq!(resumed.status.code(), Some(0), "stage={stage}: {resumed:?}");
        assert_library_facts(&fixture, &fixture.before_facts);
        assert_source_backup_retained(&fixture);
        assert_pre_restore_backup_retained(&fixture);
    }
}

#[test]
fn cancellation_while_staging_keeps_the_preimage_and_publishes_no_marker() {
    let fixture = fixture();
    let output = probe_command(&fixture, &restore_args(&fixture), None, None)
        .env("CUENOTE_PROBE_CANCEL_PHASE", "copying")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "backupCancelled", "{response}");
    assert_library_facts(&fixture, &fixture.before_facts);
    assert!(inspect(&fixture)["pending"].is_null());
    assert_source_backup_retained(&fixture);
}

#[test]
fn third_version_during_a_paused_step_is_preserved_as_a_conflict() {
    let fixture = fixture();
    let mut child = start_paused_restore(&fixture, "library_restore_forward_intent");
    wait_for_pause(&fixture, "library_restore_forward_intent", &mut child);
    let third_version = b"external third version; preserve these exact bytes";
    fs::write(&fixture.source_note_path, third_version).unwrap();
    release_pause(&fixture);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "conflict", "{response}");
    assert_eq!(fs::read(&fixture.source_note_path).unwrap(), third_version);
    assert_pending_gate(&fixture);
    assert_source_backup_retained(&fixture);
}

#[test]
fn unknown_native_addition_during_a_paused_restore_is_retained() {
    let fixture = fixture();
    let mut child = start_paused_restore(&fixture, "library_restore_forward_intent");
    wait_for_pause(&fixture, "library_restore_forward_intent", &mut child);
    let unknown_path = fixture.library.join("notes/external-third.cuenote");
    let unknown_bytes = fs::read(&fixture.source_note_path).unwrap();
    fs::write(&unknown_path, &unknown_bytes).unwrap();
    release_pause(&fixture);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "conflict", "{response}");
    assert_eq!(fs::read(&unknown_path).unwrap(), unknown_bytes);
    assert_pending_gate(&fixture);
    assert_source_backup_retained(&fixture);
}

#[test]
fn external_deletion_cannot_be_mistaken_for_a_completed_fact_removal() {
    let fixture = fixture();
    let mut child = start_paused_restore(&fixture, "library_restore_forward_intent");
    wait_for_pause(&fixture, "library_restore_forward_intent", &mut child);
    fs::remove_file(&fixture.current_only_note_path).unwrap();
    release_pause(&fixture);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "conflict", "{response}");
    assert!(!fixture.current_only_note_path.exists());
    assert_pending_gate(&fixture);
    assert_source_backup_retained(&fixture);
}

#[test]
fn partial_candidate_faults_can_continue_or_roll_back_without_losing_evidence() {
    for fault_stage in [
        "library_fact_candidate_created",
        "library_fact_candidate_partial",
    ] {
        for fault_action in ["exit", "io", "disk_full"] {
            for recovery_action in ["continue", "rollback"] {
                let fixture = fixture_with_payload(600_000);
                let expected_source =
                    &fixture.source_facts[&format!("notes/{SOURCE_NOTE_ID}.cuenote")];
                assert!(
                    expected_source.len() > 128 * 1024,
                    "fixture archive must exceed two copy chunks: {}",
                    expected_source.len()
                );

                let output = probe_command(
                    &fixture,
                    &restore_args(&fixture),
                    Some(fault_stage),
                    Some(fault_action),
                )
                .output()
                .unwrap();
                if fault_action == "exit" {
                    assert_exit_at(&fixture, fault_stage, &output);
                } else {
                    assert!(
                        fixture
                            .root
                            .join(format!(".cuenote-fault-hit-{fault_stage}"))
                            .is_file(),
                        "partial-candidate checkpoint was not reached: {output:?}"
                    );
                    assert_eq!(output.status.code(), Some(1), "{output:?}");
                    let response: serde_json::Value =
                        serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(response["error"]["kind"], "io", "{response}");
                    let injected_message = if fault_action == "io" {
                        "测试注入"
                    } else {
                        "隔离故障注入"
                    };
                    assert!(
                        response["error"]["message"]
                            .as_str()
                            .unwrap()
                            .contains(injected_message),
                        "{response}"
                    );
                }

                let candidates = partial_candidates(&fixture);
                assert_eq!(candidates.len(), 1, "{candidates:?}");
                let (candidate_path, partial_bytes) = &candidates[0];
                assert!(
                    partial_bytes.len() < expected_source.len(),
                    "candidate must be a strict prefix: partial={}, full={}",
                    partial_bytes.len(),
                    expected_source.len()
                );
                assert!(
                    expected_source.starts_with(partial_bytes),
                    "checkpoint must leave bytes copied from the verified source stage"
                );
                assert_eq!(
                    fs::read(&fixture.source_note_path).unwrap(),
                    fixture.before_facts[&format!("notes/{SOURCE_NOTE_ID}.cuenote")],
                    "the large note target must still be the exact preimage at candidate interruption"
                );
                // Notebooks sort before notes, so earlier notebook deletions
                // may already be committed when the first note candidate is staged.
                assert_native_facts_are_planned_versions(&fixture);
                assert_pending_gate(&fixture);
                assert_source_backup_retained(&fixture);
                assert_pre_restore_backup_retained(&fixture);

                let resumed = probe_command(
                    &fixture,
                    &resume_args(&fixture, recovery_action),
                    None,
                    None,
                )
                .output()
                .unwrap();
                assert_eq!(resumed.status.code(), Some(0), "{resumed:?}");
                if recovery_action == "continue" {
                    assert_library_facts_with_context(
                        &fixture,
                        &fixture.source_facts,
                        &format!("after {fault_stage}/{fault_action}/{recovery_action}"),
                    );
                } else {
                    assert_library_facts_with_context(
                        &fixture,
                        &fixture.before_facts,
                        &format!("after {fault_stage}/{fault_action}/{recovery_action}"),
                    );
                }
                assert_partial_candidate_evidence(&fixture, candidate_path, partial_bytes);
                assert_source_backup_retained(&fixture);
                assert_pre_restore_backup_retained(&fixture);
                assert!(candidate_path.is_absolute());
            }
        }
    }
}

#[test]
fn partial_restore_records_are_restartable_without_accepting_partial_json() {
    for stage in [
        "library_restore_pointer_partial",
        "library_restore_marker_partial",
        "library_restore_intent_partial",
        "library_restore_done_partial",
        "library_restore_terminal_partial",
        "library_restore_closed_partial",
    ] {
        for action in ["exit", "disk_full"] {
            let decisions: &[&str] = match stage {
                "library_restore_marker_partial"
                | "library_restore_intent_partial"
                | "library_restore_done_partial" => &["continue", "rollback"],
                "library_restore_terminal_partial" => &["continue", "rollback"],
                "library_restore_closed_partial" => &["rollback"],
                _ => &[],
            };
            let run_count = decisions.len().max(1);
            for decision_index in 0..run_count {
                let fixture = fixture();
                let output =
                    probe_command(&fixture, &restore_args(&fixture), Some(stage), Some(action))
                        .output()
                        .unwrap();
                assert_injected_fault(&fixture, stage, action, &output);

                if stage == "library_restore_pointer_partial" {
                    assert_library_facts(&fixture, &fixture.before_facts);
                    assert!(inspect(&fixture)["pending"].is_null());
                    assert_source_backup_retained(&fixture);
                    assert_pre_restore_backup_retained(&fixture);
                    continue;
                }

                let pending = assert_pending_gate(&fixture);
                if stage == "library_restore_marker_partial"
                    || stage == "library_restore_intent_partial"
                {
                    assert_library_facts(&fixture, &fixture.before_facts);
                } else if stage == "library_restore_done_partial" {
                    assert_native_facts_are_planned_versions(&fixture);
                } else {
                    assert_library_facts(&fixture, &fixture.source_facts);
                }
                if stage == "library_restore_closed_partial" {
                    assert_eq!(pending["pending"]["phase"], "paused", "{pending}");
                }
                assert_source_backup_retained(&fixture);
                assert_pre_restore_backup_retained(&fixture);

                let decision = decisions[decision_index];
                let resumed = probe_command(&fixture, &resume_args(&fixture, decision), None, None)
                    .output()
                    .unwrap();
                assert_eq!(
                    resumed.status.code(),
                    Some(0),
                    "{stage}/{action}/{decision}: {resumed:?}"
                );
                let terminal_already_committed = stage == "library_restore_closed_partial";
                if decision == "rollback" && !terminal_already_committed {
                    assert_library_facts(&fixture, &fixture.before_facts);
                } else {
                    assert_library_facts(&fixture, &fixture.source_facts);
                }
                assert_source_backup_retained(&fixture);
                assert_pre_restore_backup_retained(&fixture);
                assert!(inspect(&fixture)["pending"].is_null());
            }
        }
    }
}

#[test]
fn partial_rollback_records_resume_in_the_durable_direction() {
    for stage in [
        "library_restore_rollback_started_partial",
        "library_restore_rollback_intent_partial",
        "library_restore_rollback_done_partial",
    ] {
        for action in ["exit", "disk_full"] {
            let restart_decisions: &[&str] = if stage == "library_restore_rollback_started_partial"
            {
                &["continue", "rollback"]
            } else {
                // The durable rollback-started event makes Continue resume rollback.
                &["continue"]
            };
            for restart_decision in restart_decisions {
                let fixture = fixture();
                let forward = probe_command(
                    &fixture,
                    &restore_args(&fixture),
                    Some("library_fact_after_publish"),
                    Some("exit"),
                )
                .output()
                .unwrap();
                assert_exit_at(&fixture, "library_fact_after_publish", &forward);
                assert_pending_gate(&fixture);
                assert_native_facts_are_planned_versions(&fixture);

                let interrupted = probe_command(
                    &fixture,
                    &resume_args(&fixture, "rollback"),
                    Some(stage),
                    Some(action),
                )
                .output()
                .unwrap();
                assert_injected_fault(&fixture, stage, action, &interrupted);
                assert_pending_gate(&fixture);
                assert_native_facts_are_planned_versions(&fixture);
                assert_source_backup_retained(&fixture);
                assert_pre_restore_backup_retained(&fixture);

                let resumed = probe_command(
                    &fixture,
                    &resume_args(&fixture, restart_decision),
                    None,
                    None,
                )
                .output()
                .unwrap();
                assert_eq!(
                    resumed.status.code(),
                    Some(0),
                    "{stage}/{action}/{restart_decision}: {resumed:?}"
                );
                let rollback_is_durable = stage != "library_restore_rollback_started_partial"
                    || *restart_decision == "rollback";
                if rollback_is_durable {
                    assert_library_facts(&fixture, &fixture.before_facts);
                } else {
                    assert_library_facts(&fixture, &fixture.source_facts);
                }
                assert_source_backup_retained(&fixture);
                assert_pre_restore_backup_retained(&fixture);
                assert!(inspect(&fixture)["pending"].is_null());
            }
        }
    }
}

#[test]
fn a_durable_terminal_completed_state_cannot_be_reversed_by_rollback() {
    for action in ["exit", "disk_full"] {
        let fixture = fixture();
        let interrupted = probe_command(
            &fixture,
            &restore_args(&fixture),
            Some("library_restore_terminal"),
            Some(action),
        )
        .output()
        .unwrap();
        assert_injected_fault(&fixture, "library_restore_terminal", action, &interrupted);
        assert_library_facts(&fixture, &fixture.source_facts);
        assert_pending_gate(&fixture);

        let resumed = probe_command(&fixture, &resume_args(&fixture, "rollback"), None, None)
            .output()
            .unwrap();
        assert_eq!(resumed.status.code(), Some(0), "{resumed:?}");
        assert_library_facts(&fixture, &fixture.source_facts);
        assert_source_backup_retained(&fixture);
        assert_pre_restore_backup_retained(&fixture);
        assert!(inspect(&fixture)["pending"].is_null());
    }
}
