#![cfg(windows)]

use cuenote_lib::note_format::{write_package, NotePackage, PlainText};
use cuenote_lib::storage::{open_note, save_note};
use cuenote_lib::workspace::Workspace;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// This sets a documented generic offline-storage attribute on an isolated
// ordinary file. It does not create a Cloud Files provider or test OneDrive.
struct OfflineFixture {
    path: PathBuf,
    original_attributes: u32,
}

impl OfflineFixture {
    fn new(path: &Path) -> Self {
        let original_attributes = fs::metadata(path).unwrap().file_attributes();
        set_attributes(path, original_attributes | 0x0000_1000);
        Self {
            path: path.to_owned(),
            original_attributes,
        }
    }
}

impl Drop for OfflineFixture {
    fn drop(&mut self) {
        set_attributes(&self.path, self.original_attributes);
    }
}

fn set_attributes(path: &Path, attributes: u32) {
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(name: *const u16, attributes: u32) -> i32;
    }
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    assert_ne!(
        unsafe { SetFileAttributesW(wide.as_ptr(), attributes) },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

#[test]
fn offline_file_is_not_consumed_or_overwritten_and_can_reopen_when_local() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("cloud-file-")
        .tempdir_in(base)
        .unwrap();
    let notes = temp.path().join("notes");
    fs::create_dir_all(&notes).unwrap();
    let path = notes.join(format!("{}.cuenote", Uuid::new_v4()));
    let package = NotePackage::new_plain(
        Uuid::new_v4().to_string(),
        PlainText {
            title: "Offline marker fixture".into(),
            cue: "Download first".into(),
            content: "Do not replace this package with an empty note.".into(),
            summary: String::new(),
        },
    );
    write_package(fs::File::create(&path).unwrap(), &package).unwrap();
    let baseline = open_note(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    let offline = OfflineFixture::new(&path);

    let error = open_note(&path).unwrap_err();
    assert_eq!(error.kind, "cloudUnavailable");
    assert!(error.message.contains("本机"));
    let mut candidate = package.clone();
    candidate.document.title = "Local new title".into();
    let failed_save = save_note(
        &path,
        &temp.path().join("recovery"),
        &candidate,
        Some(&baseline.revision),
    )
    .unwrap_err();
    assert_eq!(failed_save.kind, "cloudUnavailable");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(offline);
    assert_eq!(open_note(&path).unwrap().package, package);
}

#[test]
fn offline_note_preserves_workspace_draft_and_can_save_after_local_availability_returns() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&base).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("cloud-workspace-")
        .tempdir_in(base)
        .unwrap();
    let library = temp.path().join("library");
    let mut workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        library.clone(),
    );
    workspace.select_library(library.clone()).unwrap();
    let note = workspace.new_note().unwrap();
    let text = PlainText {
        title: "Saved before going offline".into(),
        cue: String::new(),
        content: "Original saved content".into(),
        summary: String::new(),
    };
    let saved = workspace
        .save_note_text(&note.session_id, text.clone())
        .unwrap();
    let path = library
        .join("notes")
        .join(format!("{}.cuenote", note.note_id));
    let baseline = fs::read(&path).unwrap();
    let offline = OfflineFixture::new(&path);
    let probe = workspace
        .probe_external_changes(Some(&note.session_id))
        .unwrap();
    assert_eq!(probe.status, "unreadable");
    assert!(probe.message.unwrap().contains("本机"));
    let draft = PlainText {
        content: "Keep this unsaved offline edit".into(),
        ..text
    };
    let error = workspace
        .save_note_text(&saved.session.session_id, draft.clone())
        .unwrap_err();
    assert_eq!(error.kind, "cloudUnavailable");
    let protected = open_note(Path::new(error.recovery_path.as_deref().unwrap())).unwrap();
    assert_eq!(protected.package.plain_text().unwrap(), draft);
    assert_eq!(fs::read(&path).unwrap(), baseline);

    drop(offline);
    workspace
        .save_note_text(&saved.session.session_id, draft.clone())
        .unwrap();
    assert_eq!(
        open_note(&path).unwrap().package.plain_text().unwrap(),
        draft
    );
}
