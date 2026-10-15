#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use cuenote_lib::workspace::Workspace;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn setup() -> (tempfile::TempDir, String, PathBuf, Vec<u8>) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("metadata-races-")
        .tempdir_in(parent)
        .unwrap();
    let root = temp.path();
    let library = root.join("library");
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        library.clone(),
    );
    workspace.select_library(library.clone()).unwrap();
    let view = workspace.create_notebook("Initial").unwrap();
    let id = view
        .notebooks
        .iter()
        .find(|notebook| notebook.name == "Initial")
        .unwrap()
        .notebook_id
        .clone();
    let metadata = library.join("notebooks").join(format!("{id}.json"));
    let original = std::fs::read(&metadata).unwrap();
    drop(workspace);
    (temp, id, metadata, original)
}

fn probe_binary() -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let debug = executable.parent().unwrap().parent().unwrap();
    let binary = debug.join("examples").join(format!(
        "storage_fault_probe{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        binary.is_file(),
        "build the storage_fault_probe example first"
    );
    binary
}

fn start_probe(root: &Path, operation: &[&str], stage: &str, action: &str) -> Child {
    Command::new(probe_binary())
        .arg(root)
        .args(operation)
        .env("CUENOTE_FAULT_ROOT", root)
        .env("CUENOTE_FAULT_STAGE", stage)
        .env("CUENOTE_FAULT_ACTION", action)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_for_checkpoint(child: &mut Child, root: &Path, stage: &str) {
    let marker = root.join(format!(".cuenote-fault-hit-{stage}"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !marker.is_file() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("probe exited before {stage}: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.is_file(), "probe did not reach {stage}");
}

fn replace_external_metadata(path: &Path, original: &[u8], name: &str) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(original).unwrap();
    value["name"] = serde_json::Value::String(name.to_owned());
    value["timestamps"]["modifiedAt"] =
        serde_json::Value::String("2026-10-15T08:00:00.000Z".to_owned());
    let replacement = serde_json::to_vec_pretty(&value).unwrap();
    let candidate = path.with_file_name(format!("external-{}.json", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)
        .unwrap();
    file.write_all(&replacement).unwrap();
    file.sync_all().unwrap();
    drop(file);
    atomic_replace_external(&candidate, path).unwrap();
    replacement
}

fn atomic_replace_external(candidate: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        fn wide(path: &OsStr) -> Vec<u16> {
            path.encode_wide().chain(Some(0)).collect()
        }
        let candidate_wide = wide(candidate.as_os_str());
        let target_wide = wide(target.as_os_str());
        let result =
            unsafe { MoveFileExW(candidate_wide.as_ptr(), target_wide.as_ptr(), 0x1 | 0x8) };
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(unix)]
    {
        std::fs::rename(candidate, target)
    }
}

fn resume_and_wait(child: Child, root: &Path) -> Output {
    std::fs::write(root.join(".cuenote-fault-resume"), b"resume\n").unwrap();
    child.wait_with_output().unwrap()
}

fn metadata_conflicts(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with(".cuenote-notebook-conflict-"))
        })
        .collect()
}

#[test]
fn rename_preserves_atomic_external_replacement_and_restart_reports_conflict_copy() {
    let (temp, id, metadata, original) = setup();
    let root = temp.path();
    let stage = if cfg!(windows) {
        "metadata_replace_before"
    } else {
        "metadata_replace_before_swap"
    };
    let mut child = start_probe(
        root,
        &["rename-notebook", &id, "Application rename"],
        stage,
        "pause",
    );
    wait_for_checkpoint(&mut child, root, stage);
    let external = replace_external_metadata(&metadata, &original, "External rename");
    let output = resume_and_wait(child, root);

    assert_eq!(output.status.code(), Some(1));
    let conflicts = metadata_conflicts(metadata.parent().unwrap());
    let retained = if std::fs::read(&metadata).unwrap() == external {
        external.clone()
    } else {
        let matching = conflicts
            .iter()
            .find(|path| std::fs::read(path).is_ok_and(|bytes| bytes == external))
            .expect("the displaced external version must remain in a conflict JSON");
        std::fs::read(matching).unwrap()
    };
    assert_eq!(retained, external);

    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap().library.unwrap();
    if !conflicts.is_empty() {
        assert!(view.warnings.iter().any(|warning| {
            warning.contains("未解决的笔记本元数据冲突副本")
                && warning.contains(conflicts[0].file_name().unwrap().to_str().unwrap())
        }));
    } else {
        assert!(view
            .notebooks
            .iter()
            .any(|notebook| notebook.notebook_id == id && notebook.name == "External rename"));
    }
}

#[test]
fn delete_restores_newer_metadata_instead_of_removing_it() {
    let (temp, id, metadata, original) = setup();
    let root = temp.path();
    let mut child = start_probe(
        root,
        &["delete-notebook", &id],
        "metadata_delete_before",
        "pause",
    );
    wait_for_checkpoint(&mut child, root, "metadata_delete_before");
    let external = replace_external_metadata(&metadata, &original, "External before delete");
    let output = resume_and_wait(child, root);

    assert_eq!(output.status.code(), Some(1));
    let retained = if metadata.exists() {
        std::fs::read(&metadata).unwrap()
    } else {
        let conflicts = metadata_conflicts(metadata.parent().unwrap());
        assert_eq!(
            conflicts.len(),
            1,
            "newer metadata must remain discoverable"
        );
        std::fs::read(&conflicts[0]).unwrap()
    };
    assert_eq!(
        retained, external,
        "delete removed the external metadata version"
    );

    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap().library.unwrap();
    assert!(view
        .notebooks
        .iter()
        .any(|notebook| notebook.notebook_id == id && notebook.name == "External before delete"));
}

#[test]
fn rename_post_publish_external_replacement_preserves_old_external_and_requested_versions() {
    let (temp, id, metadata, original) = setup();
    let root = temp.path();
    let mut child = start_probe(
        root,
        &["rename-notebook", &id, "Requested after publish"],
        "metadata_replace_after",
        "pause",
    );
    wait_for_checkpoint(&mut child, root, "metadata_replace_after");
    let requested = std::fs::read(&metadata).unwrap();
    let external = replace_external_metadata(&metadata, &requested, "External after publish");
    let output = resume_and_wait(child, root);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read(&metadata).unwrap(), external);
    let conflicts = metadata_conflicts(metadata.parent().unwrap());
    assert!(
        conflicts
            .iter()
            .any(|path| { std::fs::read(path).is_ok_and(|bytes| bytes == original) }),
        "the displaced original metadata must remain discoverable"
    );
    assert!(
        conflicts
            .iter()
            .any(|path| { std::fs::read(path).is_ok_and(|bytes| bytes == requested) }),
        "the requested replacement must remain discoverable"
    );

    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap().library.unwrap();
    assert!(view.notebooks.iter().any(|notebook| {
        notebook.notebook_id == id && notebook.name == "External after publish"
    }));
    assert!(
        view.warnings
            .iter()
            .filter(|warning| warning.contains("未解决的笔记本元数据冲突副本"))
            .count()
            >= 2
    );
}

#[test]
fn delete_post_move_external_recreation_keeps_both_versions_and_reports_conflict() {
    let (temp, id, metadata, original) = setup();
    let root = temp.path();
    let mut child = start_probe(
        root,
        &["delete-notebook", &id],
        "metadata_delete_after_move",
        "pause",
    );
    wait_for_checkpoint(&mut child, root, "metadata_delete_after_move");
    let external = replace_external_metadata(&metadata, &original, "External after move");
    let output = resume_and_wait(child, root);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read(&metadata).unwrap(), external);
    let conflicts = metadata_conflicts(metadata.parent().unwrap());
    assert_eq!(conflicts.len(), 1);
    assert_eq!(std::fs::read(&conflicts[0]).unwrap(), original);

    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap().library.unwrap();
    assert!(view
        .notebooks
        .iter()
        .any(|notebook| { notebook.notebook_id == id && notebook.name == "External after move" }));
    assert!(view.warnings.iter().any(|warning| {
        warning.contains("未解决的笔记本元数据冲突副本")
            && warning.contains(conflicts[0].file_name().unwrap().to_str().unwrap())
    }));
}

#[test]
fn process_exit_after_metadata_swap_leaves_a_discoverable_conflict_json() {
    let (temp, id, metadata, original) = setup();
    let root = temp.path();
    let output = start_probe(
        root,
        &["rename-notebook", &id, "Crash after publish"],
        "metadata_replace_after",
        "exit",
    )
    .wait_with_output()
    .unwrap();

    assert_eq!(output.status.code(), Some(86));
    let conflicts = metadata_conflicts(metadata.parent().unwrap());
    assert_eq!(
        conflicts.len(),
        2,
        "both sides of publication must be retained"
    );
    assert!(conflicts
        .iter()
        .any(|path| std::fs::read(path).is_ok_and(|bytes| bytes == original)));
    assert!(conflicts.iter().any(|path| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value["name"].as_str().map(str::to_owned))
            .is_some_and(|name| name == "Crash after publish")
    }));

    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap().library.unwrap();
    assert!(
        view.warnings
            .iter()
            .filter(|warning| warning.contains("未解决的笔记本元数据冲突副本"))
            .count()
            >= 2
    );
}

#[test]
fn process_exit_after_delete_quarantine_leaves_the_metadata_discoverable() {
    let (temp, _id, metadata, original) = setup();
    let root = temp.path();
    let output = start_probe(
        root,
        &["delete-notebook", &_id],
        "metadata_delete_after_move",
        "exit",
    )
    .wait_with_output()
    .unwrap();

    assert_eq!(output.status.code(), Some(86));
    assert!(!metadata.exists());
    let conflicts = metadata_conflicts(metadata.parent().unwrap());
    assert_eq!(conflicts.len(), 1, "the isolated version must be retained");
    assert_eq!(std::fs::read(&conflicts[0]).unwrap(), original);

    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap().library.unwrap();
    assert!(view.warnings.iter().any(|warning| {
        warning.contains("未解决的笔记本元数据冲突副本")
            && warning.contains(conflicts[0].file_name().unwrap().to_str().unwrap())
    }));
}
