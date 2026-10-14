use cuenote_lib::note_format::{write_package, NotePackage, PlainText};
use cuenote_lib::storage::{open_note, probe_directory, save_note};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use uuid::Uuid;

fn package(note_id: &str, title: &str, content: &str) -> NotePackage {
    let mut package = NotePackage::new_plain(
        Uuid::new_v4().to_string(),
        PlainText {
            title: title.to_owned(),
            cue: "线索".to_owned(),
            content: content.to_owned(),
            summary: "总结".to_owned(),
        },
    );
    package.document.note_id = note_id.to_owned();
    package
}

fn note_id() -> String {
    Uuid::new_v4().to_string()
}

fn tempdir() -> TempDir {
    let testdata = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("testdata");
    fs::create_dir_all(&testdata).unwrap();
    tempfile::Builder::new()
        .prefix("storage-")
        .tempdir_in(testdata)
        .unwrap()
}

fn paths(temp: &TempDir) -> (PathBuf, PathBuf) {
    let library = temp.path().join("中文 笔记库 📚");
    fs::create_dir_all(&library).unwrap();
    (library, temp.path().join("本机恢复"))
}

fn find_note_files(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(directory) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_note_files(&path));
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("cuenote") {
            found.push(path);
        }
    }
    found
}

fn write_external_package(path: &Path, package: &NotePackage) {
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    write_package(&mut file, package).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
}

#[test]
fn saves_and_reopens_chinese_paths_and_same_title_notes_by_stable_identity() {
    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let first_id = note_id();
    let second_id = note_id();
    let first_path = library.join("first.cuenote");
    let second_path = library.join("second.cuenote");
    let first = package(&first_id, "同名标题", "第一篇内容");
    let second = package(&second_id, "同名标题", "第二篇内容");

    let first_revision = save_note(&first_path, &recovery, &first, None).unwrap();
    let second_revision = save_note(&second_path, &recovery, &second, None).unwrap();

    let opened_first = open_note(&first_path).unwrap();
    let opened_second = open_note(&second_path).unwrap();
    assert_eq!(opened_first.revision, first_revision);
    assert_eq!(opened_second.revision, second_revision);
    assert_eq!(
        first_revision,
        format!("{:x}", Sha256::digest(fs::read(&first_path).unwrap()))
    );
    assert_eq!(
        second_revision,
        format!("{:x}", Sha256::digest(fs::read(&second_path).unwrap()))
    );
    assert_eq!(opened_first.package.document.note_id, first_id);
    assert_eq!(opened_second.package.document.note_id, second_id);
    assert_eq!(opened_first.package.document.title, "同名标题");
    assert_eq!(opened_second.package.document.title, "同名标题");
    assert_eq!(opened_first.package.text_projection().content, "第一篇内容");
    assert_eq!(
        opened_second.package.text_projection().content,
        "第二篇内容"
    );
}

#[test]
fn stale_revision_keeps_current_note_and_writes_a_reopenable_draft() {
    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let path = library.join("conflict.cuenote");
    let id = note_id();
    let initial = package(&id, "冲突", "初始内容");
    let initial_revision = save_note(&path, &recovery, &initial, None).unwrap();
    let external = package(&id, "冲突", "外部更新");
    write_external_package(&path, &external);
    let external_revision = open_note(&path).unwrap().revision;
    let draft = package(&id, "冲突", "本地草稿");

    let error = save_note(&path, &recovery, &draft, Some(&initial_revision)).unwrap_err();

    assert_eq!(error.kind, "conflict");
    let current = open_note(&path).unwrap();
    assert_eq!(current.revision, external_revision);
    assert_eq!(current.package.text_projection().content, "外部更新");
    let recovery_path = error.recovery_path.expect("draft should be persisted");
    let recovered = open_note(Path::new(&recovery_path)).unwrap();
    assert_eq!(recovered.package.text_projection().content, "本地草稿");
}

#[test]
fn successful_replacement_keeps_the_previous_version_as_an_independent_note_file() {
    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let path = library.join("history.cuenote");
    let id = note_id();
    let first = package(&id, "历史", "旧版内容");
    let old_revision = save_note(&path, &recovery, &first, None).unwrap();
    let second = package(&id, "历史", "新版内容");

    let new_revision = save_note(&path, &recovery, &second, Some(&old_revision)).unwrap();

    assert_ne!(old_revision, new_revision);
    assert_eq!(open_note(&path).unwrap().revision, new_revision);
    let history = find_note_files(&recovery.join("history"));
    let old = history
        .iter()
        .map(|history_path| open_note(history_path).unwrap())
        .find(|note| note.revision == old_revision)
        .expect("old version should be retained in recovery storage");
    assert_eq!(old.package.document.note_id, id);
    assert_eq!(old.package.text_projection().content, "旧版内容");
}

#[test]
fn successful_saves_rotate_history_to_the_most_recent_thirty_versions() {
    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let path = library.join("rotation.cuenote");
    let id = note_id();
    let mut current_revision =
        save_note(&path, &recovery, &package(&id, "轮换", "第 0 版"), None).unwrap();
    let initial_revision = current_revision.clone();
    let mut newest_history_revision = String::new();

    for version in 1..=31 {
        let replacement = package(&id, "轮换", &format!("第 {version} 版"));
        if version == 31 {
            newest_history_revision = current_revision.clone();
        }
        current_revision =
            save_note(&path, &recovery, &replacement, Some(&current_revision)).unwrap();
    }

    let history = find_note_files(&recovery.join("history"))
        .into_iter()
        .map(|history_path| open_note(&history_path).unwrap().revision)
        .collect::<Vec<_>>();
    assert_eq!(history.len(), 30);
    assert!(!history.contains(&initial_revision));
    assert!(history.contains(&newest_history_revision));
    assert_eq!(open_note(&path).unwrap().revision, current_revision);
}

#[test]
fn malformed_existing_file_is_never_treated_as_a_blank_note_or_overwritten() {
    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let path = library.join("damaged.cuenote");
    let damaged = b"not a cuenote package";
    fs::write(&path, damaged).unwrap();

    let open_error = open_note(&path).unwrap_err();
    assert_eq!(open_error.kind, "invalid_archive");
    assert!(open_error.message.contains("笔记文件损坏或结构不受支持"));
    let replacement = package(&note_id(), "不能覆盖损坏文件", "保留的草稿");
    let save_error = save_note(&path, &recovery, &replacement, None).unwrap_err();

    assert_eq!(save_error.kind, "invalid_archive");
    assert_eq!(fs::read(&path).unwrap(), damaged);
    let recovery_path = save_error.recovery_path.expect("draft should be persisted");
    assert_eq!(
        open_note(Path::new(&recovery_path))
            .unwrap()
            .package
            .text_projection()
            .content,
        "保留的草稿"
    );
}

#[test]
fn directory_probe_checks_same_directory_replacement_and_cleans_its_files() {
    let temp = tempdir();

    probe_directory(temp.path()).unwrap();

    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn directory_probe_explains_invalid_selection_without_changing_it() {
    let temp = tempdir();
    let file = temp.path().join("not-a-directory");
    let bytes = b"keep selected object";
    fs::write(&file, bytes).unwrap();

    let error = probe_directory(&file).unwrap_err();

    assert_eq!(error.kind, "unsafePath");
    assert!(error.message.contains("实际目录"));
    assert_eq!(fs::read(&file).unwrap(), bytes);
}

#[test]
fn overlapping_recovery_roots_are_rejected_without_writing_drafts() {
    let temp = tempdir();
    let library = temp.path().join("库");
    fs::create_dir_all(&library).unwrap();

    let nested_recovery = library.join("恢复");
    let nested_note = library.join("nested.cuenote");
    let nested_error = save_note(
        &nested_note,
        &nested_recovery,
        &package(&note_id(), "恢复目录重叠", "不得写入库内恢复区"),
        None,
    )
    .unwrap_err();

    assert_eq!(nested_error.kind, "unsafePath");
    assert!(nested_error.recovery_path.is_none());
    assert!(!nested_recovery.exists());
    assert!(!nested_note.exists());

    let ancestor_recovery = temp.path();
    let ancestor_note = library.join("ancestor.cuenote");
    let ancestor_error = save_note(
        &ancestor_note,
        ancestor_recovery,
        &package(&note_id(), "恢复目录重叠", "不得写入祖先恢复区"),
        None,
    )
    .unwrap_err();

    assert_eq!(ancestor_error.kind, "unsafePath");
    assert!(ancestor_error.recovery_path.is_none());
    assert!(!ancestor_recovery.join("drafts").exists());
    assert!(!ancestor_note.exists());
}

#[cfg(unix)]
#[test]
fn note_symlink_is_rejected_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let outside = temp.path().join("outside.cuenote");
    let bytes = b"outside data";
    fs::write(&outside, bytes).unwrap();
    let link = library.join("linked.cuenote");
    symlink(&outside, &link).unwrap();

    let error = open_note(&link).unwrap_err();
    let save_error =
        save_note(&link, &recovery, &package(&note_id(), "草稿", "内容"), None).unwrap_err();

    assert_eq!(error.kind, "unsafePath");
    assert_eq!(save_error.kind, "unsafePath");
    assert_eq!(fs::read(&outside).unwrap(), bytes);
}

#[cfg(windows)]
#[test]
fn failed_windows_replace_keeps_the_old_file_and_a_reopenable_draft() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let path = library.join("locked.cuenote");
    let id = note_id();
    let old = package(&id, "发布失败", "原文件仍可读");
    let old_revision = save_note(&path, &recovery, &old, None).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001)
        .open(&path)
        .unwrap();
    let draft = package(&id, "发布失败", "待恢复草稿");

    let error = save_note(&path, &recovery, &draft, Some(&old_revision)).unwrap_err();
    drop(lock);

    assert_eq!(error.kind, "publish");
    assert_eq!(open_note(&path).unwrap().revision, old_revision);
    let recovery_path = error.recovery_path.expect("draft should be persisted");
    assert_eq!(
        open_note(Path::new(&recovery_path))
            .unwrap()
            .package
            .text_projection()
            .content,
        "待恢复草稿"
    );
}

#[cfg(windows)]
#[test]
fn failed_windows_replace_reports_unavailable_recovery_and_retains_candidate() {
    use std::os::windows::fs::OpenOptionsExt;

    let temp = tempdir();
    let (library, recovery) = paths(&temp);
    let path = library.join("locked-with-blocked-recovery.cuenote");
    let id = note_id();
    let old = package(&id, "恢复不可用", "原文件仍可读");
    let old_revision = save_note(&path, &recovery, &old, None).unwrap();

    let blocked_recovery = recovery
        .join("drafts")
        .join(format!("{:x}", Sha256::digest(id.as_bytes())));
    fs::create_dir_all(blocked_recovery.parent().unwrap()).unwrap();
    fs::write(&blocked_recovery, b"block recovery directory").unwrap();

    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001)
        .open(&path)
        .unwrap();
    let draft = package(&id, "恢复不可用", "仍保留的候选内容");
    let error = save_note(&path, &recovery, &draft, Some(&old_revision)).unwrap_err();
    drop(lock);

    assert_eq!(error.kind, "publish");
    assert!(error.recovery_path.is_none());
    let recovery_error = error
        .recovery_error
        .expect("unavailable recovery is reported");
    assert!(recovery_error.contains("无法创建恢复草稿目录"));
    let retained = recovery_error
        .split_once("；同目录候选文件仍保留于 ")
        .expect("candidate location is reported")
        .1
        .strip_suffix("，其完整性未能确认")
        .unwrap();
    let retained_path = Path::new(retained);
    assert_eq!(
        open_note(retained_path)
            .unwrap()
            .package
            .text_projection()
            .content,
        "仍保留的候选内容"
    );
    assert_eq!(open_note(&path).unwrap().revision, old_revision);
    assert_eq!(
        fs::read(&blocked_recovery).unwrap(),
        b"block recovery directory"
    );
}
