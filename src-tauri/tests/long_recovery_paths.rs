#![cfg(windows)]

use cuenote_lib::note_format::{write_package, PlainText};
use cuenote_lib::storage;
use cuenote_lib::workspace::Workspace;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

#[test]
fn long_unprefixed_recovery_path_preserves_conflicting_draft_and_restores_it() {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("long-recovery-")
        .tempdir_in(parent)
        .unwrap();
    let root = temp.path();
    let mut seed = Workspace::new(
        root.join("settings"),
        root.join("short-recovery"),
        root.join("library"),
    );
    seed.use_default_library().unwrap();
    let initial = seed.new_note().unwrap();
    let saved = seed
        .save_note_text(
            &initial.session_id,
            PlainText {
                title: "基线 A".into(),
                cue: "线索".into(),
                content: "已保存的正文".into(),
                summary: "总结".into(),
            },
        )
        .unwrap()
        .session;
    let source = root
        .join("library/notes")
        .join(format!("{}.cuenote", saved.note_id));
    drop(seed);

    // Deliberately keep the caller's ordinary absolute path: canonicalizing it
    // would add a verbatim prefix and conceal the desktop's failing call path.
    let mut recovery = root.join("long-recovery");
    while recovery.as_os_str().encode_wide().count() < 190 {
        recovery.push("long-segment");
    }
    assert!(!recovery.to_string_lossy().starts_with(r"\\?\"));
    let expected_draft = recovery
        .join("drafts")
        .join("a".repeat(64))
        .join(format!("draft-{}.cuenote", "b".repeat(32)));
    let draft_length = expected_draft.as_os_str().encode_wide().count();
    assert!(draft_length > 260);
    eprintln!("unprefixed recovery={recovery:?}; draft path length={draft_length}");

    let mut workspace = Workspace::new(
        root.join("settings"),
        recovery.clone(),
        root.join("library"),
    );
    workspace.snapshot().unwrap();
    let session = workspace.open_note(&saved.note_id).unwrap();
    let mut external = storage::open_note(&source).unwrap().package;
    let mut external_text = external.text_projection();
    external_text.title = "外部 B".into();
    external.replace_plain_text(external_text).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&source)
        .unwrap();
    write_package(&mut file, &external).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    drop(file);
    let external_revision = storage::open_note(&source).unwrap().revision;
    let mut local = session.content;
    local.title = "本地草稿 C".into();

    let result = workspace
        .reconcile_external_changes(&session.session_id, local.clone())
        .expect("long recovery paths must preserve the conflicting draft");
    assert_eq!(result.status, "conflict");
    assert_eq!(
        storage::open_note(&source).unwrap().revision,
        external_revision
    );
    drop(workspace);

    let mut restarted = Workspace::new(root.join("settings"), recovery, root.join("library"));
    restarted.snapshot().unwrap();
    let versions = restarted.list_preserved_versions().unwrap().versions;
    for title in ["基线 A", "外部 B", "本地草稿 C"] {
        let version = versions.iter().find(|item| item.title == title).unwrap();
        let preview = restarted
            .open_preserved_version(&version.version_id)
            .unwrap();
        assert_eq!(preview.text.title, title);
        assert!(preview.read_only_reason.is_some());
    }
    let draft = versions
        .iter()
        .find(|item| item.title == "本地草稿 C")
        .unwrap();
    let restored = restarted
        .restore_preserved_as_copy(&draft.version_id)
        .unwrap()
        .session;
    assert_ne!(restored.note_id, saved.note_id);
    assert_eq!(restored.content, local);
    assert_eq!(
        storage::open_note(&source).unwrap().revision,
        external_revision
    );

    // Recovery-draft listing reconstructs paths from the caller's plain root;
    // it must also remain usable after restart, independently of version views.
    let drafts = restarted.list_recovery_drafts().unwrap().drafts;
    let recoverable = drafts
        .iter()
        .find(|draft| draft.title == "本地草稿 C")
        .unwrap();
    let recovered = restarted
        .restore_recovery_as_copy(&recoverable.recovery_id)
        .unwrap()
        .session;
    assert_ne!(recovered.note_id, saved.note_id);
    assert_ne!(recovered.note_id, restored.note_id);
    assert_eq!(recovered.content, local);
    assert_eq!(
        storage::open_note(&source).unwrap().revision,
        external_revision
    );
}
