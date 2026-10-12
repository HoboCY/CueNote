use cuenote_prototype_lib::storage::{
    open_at, save_at, sha256_hex, simulate_external_update_at, Asset, NoteDocument, Snapshot,
    StorageErrorKind,
};
use serde_json::json;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("storage_probe: FAIL: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let scratch_arg = args.next().ok_or(
        "usage: storage_probe <path-containing-PROTOTYPE-wipe-me>",
    )?;
    if args.next().is_some() {
        return Err("expected exactly one scratch path argument".into());
    }
    let scratch_base = PathBuf::from(scratch_arg);
    if !scratch_base
        .components()
        .any(|component| component.as_os_str() == "PROTOTYPE-wipe-me")
    {
        return Err("scratch path must be under a PROTOTYPE-wipe-me directory".into());
    }
    fs::create_dir_all(&scratch_base)?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis();
    let root = scratch_base.join(format!("storage_probe-{timestamp}-{}", std::process::id()));
    fs::create_dir(&root)?;
    let overall = Instant::now();

    let snapshot = sample_snapshot();
    let first_started = Instant::now();
    let first_save = save_at(&root, snapshot.clone(), None)?;
    let open = open_at(&root)?;
    if open.snapshot != snapshot {
        return Err("document or asset bytes changed during ZIP round-trip".into());
    }
    println!(
        "first_save_and_open: PASS bytes={} save_elapsed_ms={} total_elapsed_ms={}",
        first_save.bytes,
        first_save.elapsed_ms,
        first_started.elapsed().as_millis()
    );

    let stale_revision = open.revision.clone();
    let mut stale_draft = snapshot.clone();
    stale_draft.document.title = "Stale local draft".to_owned();
    let external = simulate_external_update_at(&root)?;
    let conflict = save_at(&root, stale_draft, Some(stale_revision));
    match conflict {
        Err(error) if error.kind == StorageErrorKind::Conflict => {}
        Err(error) => return Err(format!("expected revision conflict, got {error}").into()),
        Ok(_) => return Err("stale draft unexpectedly replaced the external revision".into()),
    }
    let after_conflict = open_at(&root)?;
    if after_conflict.revision != external.revision
        || !after_conflict.snapshot.document.title.starts_with("External update ")
    {
        return Err("external revision was changed by the stale draft".into());
    }
    if !has_conflict_copies(root.join("recovery").as_path())? {
        return Err("conflict recovery copies were not created".into());
    }
    println!("external_conflict_preserves_current_and_draft: PASS");

    let note_path = root.join("note.cuenote");
    let corrupt_bytes = b"intentionally corrupt scratch note".to_vec();
    fs::write(&note_path, &corrupt_bytes)?;
    match open_at(&root) {
        Err(error) if error.kind == StorageErrorKind::Corrupt => {}
        Err(error) => return Err(format!("expected corrupt-read rejection, got {error}").into()),
        Ok(_) => return Err("corrupt archive unexpectedly opened".into()),
    }
    let corrupt_revision = sha256_hex(&corrupt_bytes);
    match save_at(&root, snapshot, Some(corrupt_revision)) {
        Err(error) if error.kind == StorageErrorKind::Corrupt => {}
        Err(error) => return Err(format!("expected corrupt-save rejection, got {error}").into()),
        Ok(_) => return Err("save unexpectedly replaced the corrupt archive".into()),
    }
    if fs::read(&note_path)? != corrupt_bytes {
        return Err("corrupt archive was changed by save".into());
    }
    println!("corrupt_read_does_not_overwrite: PASS");
    println!(
        "storage_probe: PASS total_elapsed_ms={} scratch={}",
        overall.elapsed().as_millis(),
        root.display()
    );
    Ok(())
}

fn sample_snapshot() -> Snapshot {
    Snapshot {
        document: NoteDocument {
            schema_version: 1,
            id: "storage-probe-note".to_owned(),
            title: "Storage probe round-trip".to_owned(),
            cue: json!({"questions": ["Does the file round-trip?"]}),
            content: json!({
                "blocks": [
                    {"type": "text", "text": "Unicode survives: 笔记 📝"},
                    {"type": "image", "src": "asset:probe-image"}
                ]
            }),
            summary: json!({"text": "Round-trip probe."}),
        },
        assets: vec![Asset {
            id: "probe-image".to_owned(),
            mime: "image/png".to_owned(),
            base64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"asset bytes used only by the storage probe",
            ),
        }],
    }
}

fn has_conflict_copies(recovery: &Path) -> Result<bool, Box<dyn Error>> {
    for item in fs::read_dir(recovery)? {
        let item = item?;
        if item.file_type()?.is_dir() && item.file_name().to_string_lossy().starts_with("conflict-") {
            let folder = item.path();
            let has_current = folder.join("CURRENT.cuenote").is_file()
                || folder.join("CURRENT-MISSING.txt").is_file();
            if has_current && folder.join("DRAFT.cuenote").is_file() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
