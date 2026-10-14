use cuenote_prototype_lib::storage::{
    open_at, save_at, sha256_hex, simulate_external_update_at, Asset, NoteDocument, Snapshot,
    StorageErrorKind,
};
use serde_json::json;
use std::error::Error;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
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
    if scratch_base.components().any(|component| component == Component::ParentDir) {
        return Err("scratch path must not contain parent-directory traversal".into());
    }
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

    let first_bytes = fs::read(root.join("note.cuenote"))?;
    let mut updated = snapshot.clone();
    updated.document.title = "Successful second save".to_owned();
    let second_save = save_at(&root, updated.clone(), Some(open.revision))?;
    if open_at(&root)?.snapshot != updated {
        return Err("second save did not replace the note".into());
    }
    let mut previous_preserved = false;
    for entry in fs::read_dir(root.join("recovery"))? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_string_lossy().contains("-previous-")
            && fs::read(entry.path())? == first_bytes
        {
            previous_preserved = true;
        }
    }
    if !previous_preserved { return Err("previous revision bytes were not preserved".into()); }
    println!("second_save_preserves_exact_previous_bytes: PASS");

    let stale_revision = second_save.revision;
    let mut stale_draft = snapshot.clone();
    stale_draft.document.title = "Stale local draft".to_owned();
    let external = simulate_external_update_at(&root)?;
    let external_bytes = fs::read(root.join("note.cuenote"))?;
    let conflict = save_at(&root, stale_draft.clone(), Some(stale_revision));
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
    if !has_conflict_copies(root.join("recovery").as_path(), &external_bytes, &stale_draft)? {
        return Err("conflict recovery copies did not preserve the exact current and draft content".into());
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

fn has_conflict_copies(recovery: &Path, current: &[u8], draft: &Snapshot) -> Result<bool, Box<dyn Error>> {
    for item in fs::read_dir(recovery)? {
        let item = item?;
        if item.file_type()?.is_dir() && item.file_name().to_string_lossy().starts_with("conflict-") {
            let folder = item.path();
            if fs::read(folder.join("CURRENT.cuenote"))? != current { continue; }
            let draft_bytes = fs::read(folder.join("DRAFT.cuenote"))?;
            let mut archive = zip::ZipArchive::new(Cursor::new(draft_bytes))?;
            let stored_document: NoteDocument = serde_json::from_reader(archive.by_name("document.json")?)?;
            if stored_document != draft.document { continue; }
            for asset in &draft.assets {
                let mut stored = Vec::new();
                archive.by_name(&format!("assets/{}", asset.id))?.read_to_end(&mut stored)?;
                if base64::Engine::encode(&base64::engine::general_purpose::STANDARD, stored) != asset.base64 {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
    }
    Ok(false)
}
