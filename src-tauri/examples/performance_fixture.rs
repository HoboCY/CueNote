//! Deterministic Q51 performance corpus CLI (isolated, not part of the app).
//!
//! ```text
//! performance_fixture generate <absolute NEW root> [--small]
//! performance_fixture verify <root>
//! performance_fixture determinism <root-a> <root-b>
//! ```
//!
//! `generate` refuses to touch an existing path, never deletes or overwrites and
//! only creates files inside the new root. The root it creates matches the
//! host's isolated performance-root contract:
//!
//! - `.cuenote-performance`   ownership marker (`CueNote isolated performance root v1\n`)
//! - `library/notes/`         1000 deterministic note packages (`<noteId>.cuenote`)
//! - `library/notebooks/`     10 deterministic notebooks, exactly one default
//! - `samples/`               standalone ordinary and stress packages
//! - `manifest.json`          every file hash/size, generation parameters, code
//!                            point counts, image dimensions/bytes, node counts
//!                            and known search query -> exact note id sets
//!
//! Text is counted in Unicode code points as documented in `content.rs` and in
//! the manifest `charsRule`. Image bytes are exact (5 MiB ordinary, 50 MiB
//! stress per package) using the disclosed `caPa` ancillary capacity, never ZIP
//! or file-level padding. All identities, timestamps and bytes are pure
//! functions of the recorded scale.
//!
//! `verify` re-walks the tree, hashes and re-reads every package with the native
//! `read_package` validator, re-derives identities from the scale, recomputes
//! query hit sets from disk and spot-regenerates packages to prove byte
//! determinism. `determinism` compares two generated roots file by file.

#[path = "performance_fixture/content.rs"]
mod content;
#[path = "performance_fixture/manifest.rs"]
mod manifest;
#[path = "performance_fixture/model.rs"]
mod model;
#[path = "performance_fixture/png.rs"]
mod png;
#[path = "performance_fixture/verify.rs"]
mod verify;
#[path = "performance_fixture/verify_edited.rs"]
mod verify_edited;

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{json, Value};

use manifest::{
    CorpusManifest, FileRecord, ImageRecord, NoteRecord, NotebookRecord, QueryRecord,
    SamplePointer, StandaloneSamples, TextCharsTotals, Totals,
};
use model::Scale;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    match run(&arguments) {
        Ok(report) => {
            match serde_json::to_string_pretty(&report) {
                Ok(text) => println!("{text}"),
                Err(error) => {
                    eprintln!("performance_fixture: cannot serialize report: {error}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("performance_fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> Result<Value, Box<dyn Error>> {
    let command = arguments.get(1).map(String::as_str);
    match command {
        Some("generate") => {
            let root = arguments
                .get(2)
                .ok_or("usage: performance_fixture generate <absolute NEW root> [--small]")?;
            if arguments.iter().any(|argument| argument == "--small") {
                generate(Path::new(root), &model::SMALL)
            } else {
                generate(Path::new(root), &model::FULL)
            }
        }
        Some("verify") => {
            let root = arguments
                .get(2)
                .ok_or("usage: performance_fixture verify <root>")?;
            verify::run(Path::new(root))
        }
        Some("determinism") => {
            let first = arguments
                .get(2)
                .ok_or("usage: performance_fixture determinism <root-a> <root-b>")?;
            let second = arguments
                .get(3)
                .ok_or("usage: performance_fixture determinism <root-a> <root-b>")?;
            determinism(Path::new(first), Path::new(second))
        }
        Some("verify-edited") if arguments.len() == 4 => {
            verify_edited::run(Path::new(&arguments[2]), Path::new(&arguments[3]))
        }
        _ => Err(
            "usage: performance_fixture <generate <absolute NEW root> [--small] | verify <root> | determinism <root-a> <root-b>>"
                .into(),
        ),
    }
}

fn generate(root: &Path, scale: &Scale) -> Result<Value, Box<dyn Error>> {
    let started = Instant::now();
    if !root.is_absolute() {
        return Err("generate requires an absolute NEW root path".into());
    }
    if root.parent().is_none() {
        return Err("generate refuses a filesystem root as the corpus root".into());
    }
    if root.symlink_metadata().is_ok() {
        return Err(format!(
            "generate refuses to touch the existing path {}",
            root.display()
        )
        .into());
    }
    let root = root.to_path_buf();

    std::fs::create_dir_all(root.join(model::LIBRARY_DIR).join("notes"))?;
    std::fs::create_dir_all(root.join(model::LIBRARY_DIR).join("notebooks"))?;
    std::fs::create_dir_all(root.join(model::SAMPLES_DIR))?;
    manifest::write_new_file(&root.join(model::MARKER_PATH), model::MARKER_BYTES)?;

    let notebooks = model::notebooks(scale);
    let mut notebook_records = Vec::with_capacity(notebooks.len());
    for notebook in &notebooks {
        let bytes = manifest::notebook_metadata_bytes(notebook)?;
        let relative = format!(
            "{}/notebooks/{}.json",
            model::LIBRARY_DIR,
            notebook.notebook_id
        );
        manifest::write_new_file(&root.join(&relative), &bytes)?;
        notebook_records.push(NotebookRecord {
            path: relative,
            notebook_id: notebook.notebook_id.clone(),
            name: notebook.name.clone(),
            is_default: notebook.is_default,
            sha256: manifest::sha256_bytes(&bytes),
            bytes: bytes.len() as u64,
        });
    }

    let mut note_records: Vec<NoteRecord> = Vec::new();
    let mut image_records: Vec<ImageRecord> = Vec::new();
    let mut text_chars = TextCharsTotals {
        text: 0,
        ordinary: 0,
        stress: 0,
    };

    let library = model::library_notes(scale);
    let total_library = library.len();
    for (done, note) in library.iter().enumerate() {
        let serial = model::note_serial(scale, note);
        let notebook = &notebooks[model::notebook_index(scale, serial)];
        let relative = format!("{}/notes/{}.cuenote", model::LIBRARY_DIR, note.note_id);
        let (record, images) = write_note(&root, &relative, note, &notebook.notebook_id, serial)?;
        accumulate_text_chars(&mut text_chars, note.profile, record.text_chars);
        note_records.push(record);
        image_records.extend(images);
        if scale.name == "full" && (done + 1) % 100 == 0 {
            eprintln!(
                "progress: {}/{total_library} library notes written",
                done + 1
            );
        }
    }

    let samples = model::sample_notes();
    for (index, note) in samples.iter().enumerate() {
        let serial = model::note_serial(scale, note);
        let relative = model::SAMPLE_PATHS[index].to_owned();
        let (record, images) =
            write_note(&root, &relative, note, &notebooks[0].notebook_id, serial)?;
        accumulate_text_chars(&mut text_chars, note.profile, record.text_chars);
        note_records.push(record);
        image_records.extend(images);
    }

    let known_queries: Vec<QueryRecord> = model::expected_queries(scale)
        .into_iter()
        .map(|expected| QueryRecord {
            query: expected.query,
            note_ids: expected.note_ids,
        })
        .collect();

    let mut files = manifest::walk_files(&root)?;
    files.push(FileRecord {
        path: model::MANIFEST_PATH.to_owned(),
        sha256: None,
        bytes: None,
    });
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let bytes_total: u64 = files.iter().filter_map(|record| record.bytes).sum();
    let image_bytes: u64 = image_records.iter().map(|record| record.bytes).sum();
    let ancillary_bytes: u64 = image_records
        .iter()
        .map(|record| record.ancillary_bytes)
        .sum();
    let map_nodes: usize = note_records.iter().map(|record| record.map_nodes).sum();

    let corpus = CorpusManifest {
        format: manifest::FORMAT.to_owned(),
        format_version: manifest::FORMAT_VERSION,
        scale: scale.name.to_owned(),
        generator: manifest::GENERATOR.to_owned(),
        generation_parameters: manifest::generation_parameters(scale),
        totals: Totals {
            files: files.len(),
            bytes: bytes_total,
            manifest_bytes_excluded: true,
            notes: note_records.len(),
            notebooks: notebook_records.len(),
            images: image_records.len(),
            image_bytes,
            ancillary_bytes,
            map_nodes,
            text_chars: text_chars.clone(),
        },
        standalone_samples: StandaloneSamples {
            ordinary: SamplePointer {
                path: model::SAMPLE_PATHS[0].to_owned(),
                note_id: samples[0].note_id.clone(),
            },
            stress: SamplePointer {
                path: model::SAMPLE_PATHS[1].to_owned(),
                note_id: samples[1].note_id.clone(),
            },
        },
        notebooks: notebook_records,
        notes: note_records,
        images: image_records,
        known_queries,
        files,
    };

    let mut manifest_bytes = serde_json::to_vec_pretty(&corpus)?;
    manifest_bytes.push(b'\n');
    manifest::write_new_file(&root.join(model::MANIFEST_PATH), &manifest_bytes)?;

    Ok(json!({
        "status": "ok",
        "command": "generate",
        "root": root.display().to_string(),
        "scale": scale.name,
        "files": corpus.totals.files,
        "bytes": corpus.totals.bytes,
        "notes": corpus.totals.notes,
        "notebooks": corpus.totals.notebooks,
        "images": corpus.totals.images,
        "imageBytes": corpus.totals.image_bytes,
        "ancillaryBytes": corpus.totals.ancillary_bytes,
        "mapNodes": corpus.totals.map_nodes,
        "textChars": corpus.totals.text_chars,
        "knownQueries": corpus.known_queries.len(),
        "manifestSha256": manifest::sha256_bytes(&manifest_bytes),
        "durationMs": started.elapsed().as_secs_f64() * 1000.0,
    }))
}

fn write_note(
    root: &Path,
    relative: &str,
    note: &model::NoteRef,
    notebook_id: &str,
    serial: usize,
) -> Result<(NoteRecord, Vec<ImageRecord>), Box<dyn Error>> {
    let built = content::build_note(note, notebook_id, serial)?;
    let path = root.join(relative);
    manifest::write_new_file(&path, &built.bytes)?;
    let images = built
        .images
        .iter()
        .map(|image| ImageRecord {
            path: relative.to_owned(),
            note_id: note.note_id.clone(),
            asset_id: image.asset_id.clone(),
            width: image.width,
            height: image.height,
            bytes: image.bytes,
            ancillary_bytes: image.ancillary_bytes,
            sha256: image.sha256.clone(),
        })
        .collect();
    Ok((
        NoteRecord {
            path: relative.to_owned(),
            note_id: note.note_id.clone(),
            profile: note.profile.as_str().to_owned(),
            collection: note.collection.as_str().to_owned(),
            text_chars: built.text_chars,
            image_count: built.images.len(),
            map_nodes: built.map_nodes,
            sha256: manifest::sha256_bytes(&built.bytes),
            bytes: built.bytes.len() as u64,
        },
        images,
    ))
}

fn accumulate_text_chars(totals: &mut TextCharsTotals, profile: model::Profile, chars: usize) {
    let chars = chars as u64;
    match profile {
        model::Profile::Text => totals.text += chars,
        model::Profile::Ordinary => totals.ordinary += chars,
        model::Profile::Stress => totals.stress += chars,
    }
}

fn determinism(first: &Path, second: &Path) -> Result<Value, Box<dyn Error>> {
    let first = first
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", first.display()))?;
    let second = second
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", second.display()))?;
    let first_files = manifest::walk_files(&first)?;
    let second_files = manifest::walk_files(&second)?;
    if first_files.len() != second_files.len() {
        return Err(format!(
            "roots differ: {} has {} files, {} has {}",
            first.display(),
            first_files.len(),
            second.display(),
            second_files.len()
        )
        .into());
    }
    for (left, right) in first_files.iter().zip(second_files.iter()) {
        if left.path != right.path {
            return Err(format!("roots differ: {:?} vs {:?}", left.path, right.path).into());
        }
        if left.sha256 != right.sha256 || left.bytes != right.bytes {
            return Err(format!("file {} is not byte-identical across roots", left.path).into());
        }
    }
    let first_manifest = std::fs::read(first.join(model::MANIFEST_PATH))?;
    let second_manifest = std::fs::read(second.join(model::MANIFEST_PATH))?;
    if first_manifest != second_manifest {
        return Err("manifest.json bytes differ across roots".into());
    }
    Ok(json!({
        "status": "ok",
        "command": "determinism",
        "files": first_files.len(),
        "byteIdentical": true,
        "manifestSha256": manifest::sha256_bytes(&first_manifest),
        "roots": [first.display().to_string(), second.display().to_string()],
    }))
}
