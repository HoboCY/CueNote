#![cfg(all(debug_assertions, feature = "storage-fault-injection"))]

use cuenote_lib::note_format::{write_package, PlainText};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::ffi::OsStr;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn setup() -> (tempfile::TempDir, String, PathBuf, Vec<u8>) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    std::fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("trash-delete-races-")
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
    let note = workspace.new_note().unwrap();
    let saved = workspace
        .save_note_text(
            &note.session_id,
            PlainText {
                title: "Trash race baseline".into(),
                cue: "Cue".into(),
                content: "Original trashed content".into(),
                summary: "Summary".into(),
            },
        )
        .unwrap();
    let trashed = workspace.trash_note(&saved.session.session_id).unwrap();
    assert!(trashed.session.trashed_at.is_some());
    let path = library
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let original = std::fs::read(&path).unwrap();
    drop(workspace);
    (temp, note.note_id, path, original)
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

fn start_probe(root: &Path, note_id: &str, stage: &str, action: &str) -> Child {
    Command::new(probe_binary())
        .arg(root)
        .arg("delete-trashed-note")
        .arg(note_id)
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

fn external_package(path: &Path) -> Vec<u8> {
    let mut package = storage::open_note(path).unwrap().package;
    let mut text = package.text_projection();
    text.content = "Newer OneDrive version".into();
    package.replace_plain_text(text).unwrap();
    let mut bytes = Cursor::new(Vec::new());
    write_package(&mut bytes, &package).unwrap();
    bytes.into_inner()
}

fn install_external_package(path: &Path, bytes: &[u8]) {
    let candidate = path.with_file_name(format!(".external-{}.cuenote", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
    drop(file);
    if path.exists() {
        atomic_replace_external(&candidate, path).unwrap();
    } else {
        std::fs::rename(&candidate, path).unwrap();
    }
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

fn isolated_deletions(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| {
                    name.starts_with(".cuenote-delete-") && name.ends_with(".cuenote")
                })
        })
        .collect()
}

#[test]
fn a_newer_package_swapped_in_after_check_is_not_permanently_deleted() {
    let (temp, note_id, path, _original) = setup();
    let root = temp.path();
    let mut child = start_probe(root, &note_id, "trashed_delete_before_isolate", "pause");
    wait_for_checkpoint(&mut child, root, "trashed_delete_before_isolate");
    let external = external_package(&path);
    install_external_package(&path, &external);
    let output = resume_and_wait(child, root);

    assert_eq!(output.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["kind"], "conflict");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        external,
        "the newer synchronized package must remain at the canonical path"
    );
}

#[test]
fn external_recreation_after_isolation_keeps_both_packages_and_reports_conflict() {
    let (temp, note_id, path, original) = setup();
    let root = temp.path();
    let directory = path.parent().unwrap();
    let mut child = start_probe(root, &note_id, "trashed_delete_after_isolate", "pause");
    wait_for_checkpoint(&mut child, root, "trashed_delete_after_isolate");
    assert!(
        !path.exists(),
        "the canonical package is isolated while paused"
    );
    let isolated = isolated_deletions(directory);
    assert_eq!(isolated.len(), 1);
    assert_eq!(std::fs::read(&isolated[0]).unwrap(), original);
    let external = external_package(&isolated[0]);
    install_external_package(&path, &external);

    let output = resume_and_wait(child, root);

    assert_eq!(output.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["kind"], "conflict");
    assert_eq!(std::fs::read(&path).unwrap(), external);
    assert_eq!(std::fs::read(&isolated[0]).unwrap(), original);
}

#[test]
fn process_exit_after_isolation_is_discovered_and_recoverable_on_restart() {
    let (temp, note_id, path, original) = setup();
    let root = temp.path();
    let directory = path.parent().unwrap();
    let output = start_probe(root, &note_id, "trashed_delete_after_isolate", "exit")
        .wait_with_output()
        .unwrap();

    assert_eq!(output.status.code(), Some(86));
    assert!(!path.exists());
    let isolated = isolated_deletions(directory);
    assert_eq!(isolated.len(), 1);
    assert_eq!(std::fs::read(&isolated[0]).unwrap(), original);

    let isolated_name = isolated[0].file_name().unwrap().to_string_lossy();
    let isolated_stem = isolated[0].file_stem().unwrap().to_string_lossy();
    let mut reopened = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("unused-default"),
    );
    let view = reopened.snapshot().unwrap();
    let library = view.library.unwrap();
    assert!(library.warnings.iter().any(|warning| {
        warning.contains("已保留到历史与冲突") && warning.contains(isolated_stem.as_ref())
    }));

    let versions = reopened.list_preserved_versions().unwrap().versions;
    let recovered = versions
        .iter()
        .find(|version| {
            version.conflict
                && version.note_id == note_id
                && version.origin.contains(isolated_name.as_ref())
        })
        .expect("startup should preserve the isolated package as a recoverable conflict version");
    let preview = reopened
        .open_preserved_version(&recovered.version_id)
        .unwrap();
    assert_eq!(preview.note_id, note_id);
    assert_eq!(preview.text.content, "Original trashed content");
    assert!(preview.read_only_reason.is_some());
}
