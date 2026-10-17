//! Independent corpus verification.
//!
//! `verify` never trusts the manifest as evidence: it walks the actual tree,
//! hashes every file, re-reads every package with the native `read_package`
//! validator, re-derives every identity/parameter from the recorded scale, and
//! recomputes the known query hit sets from the packages on disk. Regenerated
//! packages are compared byte-for-byte as an in-process determinism spot check.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::io::Cursor;
use std::path::Path;

use cuenote_lib::note_format::{read_package, Block, Inline, MindMapNode, NotePackage, TableRow};
use serde_json::{json, Value};

use crate::content;
use crate::manifest::{
    self, CorpusManifest, FileRecord, ImageRecord, NoteRecord, NotebookMetadata,
};
use crate::model::{self, Collection, NoteRef, Profile, Scale};
use crate::png;

const REQUIRED_LANGUAGES: [&str; 7] = [
    "javascript",
    "typescript",
    "python",
    "rust",
    "csharp",
    "sql",
    "json",
];
const LATEX_TERMS: [&str; 8] = [
    "\\frac", "\\sqrt", "\\sum", "\\int", "\\alpha", "bmatrix", "aligned", "^",
];
const FIRST_IMAGE_MAX_INDEX: usize = 4;
const MAP_MAX_INDEX: usize = 5;

#[derive(Default)]
struct ContentStats {
    images: Vec<(String, u32)>,
    map: Option<(String, String, Vec<MindMapNode>)>,
    languages: BTreeSet<String>,
    unknown_language: bool,
    inline_math: Vec<String>,
    math_blocks: Vec<String>,
    code_blocks: usize,
    first_image_index: Option<usize>,
    map_index: Option<usize>,
    marks: BTreeSet<&'static str>,
    headings: BTreeSet<u8>,
    has_table: bool,
    has_blockquote: bool,
    has_task_list: bool,
    has_ordered_list: bool,
    has_bullet_list: bool,
}

struct ObservedImage {
    path: String,
    note_id: String,
    asset_id: String,
    width: u32,
    height: u32,
    bytes: u64,
    ancillary_bytes: u64,
    sha256: String,
}

struct ObservedNote {
    path: String,
    note_id: String,
    profile: Profile,
    collection: Collection,
    text_chars: usize,
    image_count: usize,
    map_nodes: usize,
    map_id: Option<String>,
    node_ids: Vec<String>,
    sha256: String,
    bytes: u64,
    images: Vec<ObservedImage>,
    segments: Vec<String>,
}

pub fn run(root: &Path) -> Result<Value, Box<dyn Error>> {
    if !root.is_absolute() {
        return Err("verify requires an absolute root path".into());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve root {}: {error}", root.display()))?;

    let marker = fs::read(root.join(model::MARKER_PATH))
        .map_err(|error| format!("{} is missing: {error}", model::MARKER_PATH))?;
    if marker != model::MARKER_BYTES {
        return Err(format!("{} has unexpected content", model::MARKER_PATH).into());
    }

    let manifest_bytes = fs::read(root.join(model::MANIFEST_PATH))
        .map_err(|error| format!("{} is missing: {error}", model::MANIFEST_PATH))?;
    let manifest: CorpusManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("manifest.json is not a valid corpus manifest: {error}"))?;
    if manifest.format != manifest::FORMAT || manifest.format_version != manifest::FORMAT_VERSION {
        return Err(format!(
            "unsupported corpus format {:?} version {}",
            manifest.format, manifest.format_version
        )
        .into());
    }
    let scale = Scale::from_name(&manifest.scale)
        .ok_or_else(|| format!("unknown corpus scale {:?}", manifest.scale))?;
    check_generation_parameters(&manifest, &scale)?;

    // ----- file inventory -------------------------------------------------
    let actual_files = manifest::walk_files(&root)?;
    let actual_by_path: BTreeMap<&str, &FileRecord> = actual_files
        .iter()
        .map(|record| (record.path.as_str(), record))
        .collect();
    let manifest_by_path: BTreeMap<&str, &FileRecord> = manifest
        .files
        .iter()
        .map(|record| (record.path.as_str(), record))
        .collect();
    if actual_by_path.len() != actual_files.len() {
        return Err("corpus contains duplicate file paths".into());
    }
    if manifest_by_path.len() != manifest.files.len() {
        return Err("manifest lists the same path more than once".into());
    }
    let actual_paths: BTreeSet<&str> = actual_by_path.keys().copied().collect();
    let manifest_paths: BTreeSet<&str> = manifest_by_path.keys().copied().collect();
    if actual_paths != manifest_paths {
        return Err("manifest file list disagrees with the files on disk".into());
    }
    let mut manifest_self_entries = 0usize;
    for record in &manifest.files {
        if record.path == model::MANIFEST_PATH {
            manifest_self_entries += 1;
            if record.sha256.is_some() || record.bytes.is_some() {
                return Err("manifest.json must not claim its own hash".into());
            }
            continue;
        }
        let actual = actual_by_path
            .get(record.path.as_str())
            .expect("path sets are equal");
        if record.sha256.as_deref() != actual.sha256.as_deref() || record.bytes != actual.bytes {
            return Err(format!("manifest hash/bytes differ from disk for {}", record.path).into());
        }
    }
    if manifest_self_entries != 1 {
        return Err("manifest must contain exactly one self entry for manifest.json".into());
    }

    // ----- notebooks ------------------------------------------------------
    let notebooks = model::notebooks(&scale);
    let notebooks_by_id: BTreeMap<String, model::NotebookRef> = notebooks
        .iter()
        .map(|notebook| (notebook.notebook_id.clone(), notebook.clone()))
        .collect();
    if notebooks_by_id.len() != scale.notebooks {
        return Err("model notebook ids are not unique".into());
    }
    let disk_notebooks = read_notebooks(&root, &notebooks)?;

    let manifest_notebook_ids: BTreeSet<&str> = manifest
        .notebooks
        .iter()
        .map(|record| record.notebook_id.as_str())
        .collect();
    if manifest_notebook_ids.len() != manifest.notebooks.len()
        || manifest_notebook_ids.len() != scale.notebooks
    {
        return Err("manifest notebook records are not one per notebook".into());
    }
    for record in &manifest.notebooks {
        let metadata = disk_notebooks.get(&record.notebook_id).ok_or_else(|| {
            format!(
                "manifest notebook {} is missing on disk",
                record.notebook_id
            )
        })?;
        if record.path
            != format!(
                "{}/notebooks/{}.json",
                model::LIBRARY_DIR,
                record.notebook_id
            )
            || record.name != metadata.name
            || record.is_default != metadata.default
        {
            return Err(format!(
                "manifest notebook record {} disagrees with disk metadata",
                record.notebook_id
            )
            .into());
        }
    }

    // ----- notes ----------------------------------------------------------
    let default_notebook_id = notebooks
        .iter()
        .find(|notebook| notebook.is_default)
        .map(|notebook| notebook.notebook_id.clone())
        .ok_or("scale model has no default notebook")?;

    let mut model_map: BTreeMap<String, (NoteRef, usize, String)> = BTreeMap::new();
    for note in model::library_notes(&scale) {
        let serial = model::note_serial(&scale, &note);
        let path = format!("{}/notes/{}.cuenote", model::LIBRARY_DIR, note.note_id);
        if model_map
            .insert(note.note_id.clone(), (note, serial, path))
            .is_some()
        {
            return Err("model note ids are not unique".into());
        }
    }
    let samples = model::sample_notes();
    for (index, note) in samples.iter().enumerate() {
        let serial = model::note_serial(&scale, note);
        let path = model::SAMPLE_PATHS[index].to_owned();
        if model_map
            .insert(note.note_id.clone(), (note.clone(), serial, path))
            .is_some()
        {
            return Err("model note ids are not unique across library and samples".into());
        }
    }
    if manifest.standalone_samples.ordinary.note_id != samples[0].note_id
        || manifest.standalone_samples.ordinary.path != model::SAMPLE_PATHS[0]
        || manifest.standalone_samples.stress.note_id != samples[1].note_id
        || manifest.standalone_samples.stress.path != model::SAMPLE_PATHS[1]
    {
        return Err("manifest standalone sample pointers disagree with the model".into());
    }

    let manifest_note_ids: BTreeSet<&str> = manifest
        .notes
        .iter()
        .map(|record| record.note_id.as_str())
        .collect();
    let model_note_ids: BTreeSet<&str> = model_map.keys().map(String::as_str).collect();
    if manifest_note_ids != model_note_ids || manifest.notes.len() != model_map.len() {
        return Err("manifest note list disagrees with the deterministic model".into());
    }
    let manifest_notes_by_id: BTreeMap<&str, &NoteRecord> = manifest
        .notes
        .iter()
        .map(|record| (record.note_id.as_str(), record))
        .collect();

    let mut observed_notes: Vec<ObservedNote> = Vec::with_capacity(model_map.len());
    let mut seen_note_ids = HashSet::new();
    let mut seen_map_ids = HashSet::new();
    let mut seen_node_ids = HashSet::new();
    let mut seen_asset_ids = HashSet::new();

    for (note_id, (reference, serial, expected_path)) in &model_map {
        let disk_path = root.join(expected_path);
        if !disk_path.is_file() {
            return Err(format!("expected note file {expected_path} is missing").into());
        }
        let observed = inspect_note(
            expected_path,
            &disk_path,
            reference,
            *serial,
            &default_notebook_id,
            &scale,
            &notebooks_by_id,
        )?;
        if observed.note_id != *note_id {
            return Err(format!("{expected_path} holds a different note id").into());
        }
        if !seen_note_ids.insert(observed.note_id.clone()) {
            return Err(format!("duplicate note id {}", observed.note_id).into());
        }
        for image in &observed.images {
            if !seen_asset_ids.insert(image.asset_id.clone()) {
                return Err(format!("duplicate asset id {}", image.asset_id).into());
            }
        }
        if let Some(map_id) = &observed.map_id {
            if !seen_map_ids.insert(map_id.clone()) {
                return Err(format!("duplicate mind map id {map_id}").into());
            }
        }
        for node_id in &observed.node_ids {
            if !seen_node_ids.insert(node_id.clone()) {
                return Err("duplicate mind map node id".into());
            }
        }
        observed_notes.push(observed);
    }

    // Cross-check manifest note records against observed content.
    for observed in &observed_notes {
        let record = manifest_notes_by_id
            .get(observed.note_id.as_str())
            .expect("model sets are equal");
        if record.path != observed.path
            || Profile::from_str(&record.profile) != Some(observed.profile)
            || record.collection != observed.collection.as_str()
            || record.text_chars != observed.text_chars
            || record.image_count != observed.image_count
            || record.map_nodes != observed.map_nodes
            || record.sha256 != observed.sha256
            || record.bytes != observed.bytes
        {
            return Err(format!(
                "manifest note record {} disagrees with the verified package",
                observed.path
            )
            .into());
        }
    }

    // ----- images ---------------------------------------------------------
    let manifest_images: HashMap<(String, String), &ImageRecord> = manifest
        .images
        .iter()
        .map(|record| ((record.path.clone(), record.asset_id.clone()), record))
        .collect();
    if manifest_images.len() != manifest.images.len() {
        return Err("manifest lists an image more than once".into());
    }
    let mut observed_images = 0usize;
    for observed in &observed_notes {
        for image in &observed.images {
            observed_images += 1;
            let record = manifest_images
                .get(&(image.path.clone(), image.asset_id.clone()))
                .ok_or_else(|| {
                    format!(
                        "manifest is missing image {} in {}",
                        image.asset_id, image.path
                    )
                })?;
            if record.note_id != image.note_id
                || record.width != image.width
                || record.height != image.height
                || record.bytes != image.bytes
                || record.ancillary_bytes != image.ancillary_bytes
                || record.sha256 != image.sha256
            {
                return Err(format!(
                    "manifest image record {} disagrees with the verified asset",
                    image.asset_id
                )
                .into());
            }
        }
    }
    if observed_images != manifest.images.len() {
        return Err("manifest image count disagrees with the verified corpus".into());
    }

    // ----- known queries --------------------------------------------------
    check_queries(&manifest, &scale, &observed_notes)?;

    // ----- totals ---------------------------------------------------------
    check_totals(&manifest, &actual_files, &observed_notes)?;

    // ----- in-process determinism spot checks -----------------------------
    let spot_checks = determinism_spot_checks(&root, &scale, &default_notebook_id)?;

    Ok(json!({
        "status": "ok",
        "root": root.display().to_string(),
        "scale": scale.name,
        "files": actual_files.len(),
        "bytes": actual_files.iter().filter_map(|record| record.bytes).sum::<u64>(),
        "notes": observed_notes.len(),
        "notebooks": scale.notebooks,
        "images": observed_images,
        "knownQueries": manifest.known_queries.len(),
        "determinismSpotChecks": spot_checks,
    }))
}

fn check_generation_parameters(
    manifest: &CorpusManifest,
    scale: &Scale,
) -> Result<(), Box<dyn Error>> {
    let expected = manifest::generation_parameters(scale);
    let parameters = &manifest.generation_parameters;
    if parameters != &expected {
        return Err("generation parameters do not match a known Q51 profile".into());
    }
    Ok(())
}

fn read_notebooks(
    root: &Path,
    expected: &[model::NotebookRef],
) -> Result<BTreeMap<String, NotebookMetadata>, Box<dyn Error>> {
    let directory = root.join(model::LIBRARY_DIR).join("notebooks");
    if !directory.is_dir() {
        return Err("library/notebooks is missing".into());
    }
    let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut notebooks: BTreeMap<String, NotebookMetadata> = BTreeMap::new();
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !path.is_file() || !name.ends_with(".json") {
            return Err(format!("unexpected notebooks entry {name}").into());
        }
        let bytes = fs::read(&path)?;
        let metadata: NotebookMetadata = serde_json::from_slice(&bytes)
            .map_err(|error| format!("notebook metadata {name} is invalid: {error}"))?;
        let stem = name.trim_end_matches(".json");
        if metadata.schema_version != 1
            || metadata.id != stem
            || !is_stable_uuid(&metadata.id)
            || metadata.name.trim().is_empty()
            || metadata.name.chars().count() > 256
            || metadata.name.chars().any(char::is_control)
            || !is_valid_timestamp(&metadata.timestamps.created_at)
            || !is_valid_timestamp(&metadata.timestamps.modified_at)
        {
            return Err(format!("notebook metadata {name} fails validation").into());
        }
        if notebooks.insert(metadata.id.clone(), metadata).is_some() {
            return Err(format!("duplicate notebook metadata {name}").into());
        }
    }
    if notebooks.len() != expected.len() {
        return Err("notebook metadata count disagrees with the model".into());
    }
    let mut defaults = 0usize;
    for notebook in expected {
        let metadata = notebooks
            .get(&notebook.notebook_id)
            .ok_or_else(|| format!("model notebook {} is missing on disk", notebook.notebook_id))?;
        if metadata.name != notebook.name || metadata.default != notebook.is_default {
            return Err(format!(
                "notebook {} metadata disagrees with the model",
                notebook.notebook_id
            )
            .into());
        }
        let (created_at, modified_at) = model::notebook_timestamps(notebook.index);
        if metadata.timestamps.created_at != created_at
            || metadata.timestamps.modified_at != modified_at
        {
            return Err(format!(
                "notebook {} timestamps are not the fixed deterministic values",
                notebook.notebook_id
            )
            .into());
        }
        if notebook.is_default {
            defaults += 1;
        }
    }
    if defaults != 1 {
        return Err("exactly one default notebook is required".into());
    }
    Ok(notebooks)
}

fn inspect_note(
    path: &str,
    disk_path: &Path,
    reference: &NoteRef,
    serial: usize,
    default_notebook_id: &str,
    scale: &Scale,
    notebooks: &BTreeMap<String, model::NotebookRef>,
) -> Result<ObservedNote, Box<dyn Error>> {
    let file_bytes = fs::read(disk_path)?;
    let sha256 = manifest::sha256_bytes(&file_bytes);
    let bytes = file_bytes.len() as u64;
    let package = read_package(Cursor::new(&file_bytes))
        .map_err(|error| format!("{path} is not a valid package: {error}"))?;

    if package.document.note_id != reference.note_id {
        return Err(format!("{path} note id does not match the deterministic model").into());
    }
    if package.document.title.is_empty() {
        return Err(format!("{path} has an empty title").into());
    }
    if package.document.sections.cue.is_empty()
        || package.document.sections.content.is_empty()
        || package.document.sections.summary.is_empty()
    {
        return Err(format!("{path} does not populate all three sections").into());
    }

    let mut stats = ContentStats::default();
    collect_stats(&package.document.sections.content, &mut stats);
    for (index, block) in package.document.sections.content.iter().enumerate() {
        match block {
            Block::Image { .. } => {
                stats.first_image_index.get_or_insert(index);
            }
            Block::MindMap { .. } => {
                stats.map_index.get_or_insert(index);
            }
            _ => {}
        }
    }

    let image_count = stats.images.len();
    let profile = if image_count == model::ORDINARY_IMAGES
        && stats
            .images
            .iter()
            .all(|(_, side)| *side == model::ORDINARY_IMAGE_SIDE)
    {
        Profile::Ordinary
    } else if image_count == model::STRESS_IMAGES
        && stats
            .images
            .iter()
            .all(|(_, side)| *side == model::STRESS_IMAGE_SIDE)
    {
        Profile::Stress
    } else if image_count == 0
        && stats.map.is_none()
        && stats.code_blocks == 0
        && stats.inline_math.is_empty()
        && stats.math_blocks.is_empty()
    {
        Profile::Text
    } else {
        return Err(format!("{path} does not match any known Q51 profile").into());
    };
    if profile != reference.profile {
        return Err(format!(
            "{path} content profile {} disagrees with its deterministic identity {}",
            profile.as_str(),
            reference.profile.as_str()
        )
        .into());
    }

    let text_chars = content::text_chars_of(&package);
    if text_chars != reference.profile.expected_chars() {
        return Err(format!(
            "{path} counts {text_chars} Unicode code points, expected {}",
            reference.profile.expected_chars()
        )
        .into());
    }

    let images = image_records(path, reference, &package, &stats)?;
    let asset_bytes: u64 = package
        .assets
        .values()
        .map(|bytes| bytes.len() as u64)
        .sum();
    if asset_bytes != reference.profile.asset_target_bytes() {
        return Err(format!(
            "{path} carries {asset_bytes} image bytes, expected exact {}",
            reference.profile.asset_target_bytes()
        )
        .into());
    }

    let (map_nodes, map_id, node_ids) = match &stats.map {
        Some((map_id, root_id, nodes)) => {
            check_map_shape(path, map_id, root_id, nodes)?;
            (
                nodes.len(),
                Some(map_id.clone()),
                nodes.iter().map(|node| node.id.clone()).collect(),
            )
        }
        None => (0, None, Vec::new()),
    };
    if map_nodes != reference.profile.map_nodes() {
        return Err(format!(
            "{path} has {map_nodes} mind-map nodes, expected {}",
            reference.profile.map_nodes()
        )
        .into());
    }

    if profile != Profile::Text {
        check_coverage(path, &stats)?;
        check_rich_format(path, &stats)?;
        if stats
            .first_image_index
            .is_none_or(|index| index > FIRST_IMAGE_MAX_INDEX)
        {
            return Err(format!("{path} does not place an image early in the body").into());
        }
        if stats.map_index.is_none_or(|index| index > MAP_MAX_INDEX) {
            return Err(format!("{path} does not place the mind map early in the body").into());
        }
    } else {
        check_simple_format(path, &stats)?;
    }

    let timestamps = &package.document.timestamps;
    let (created_at, content_modified_at, organization_modified_at) =
        model::note_timestamps(serial);
    if timestamps.created_at != created_at
        || timestamps.content_modified_at != content_modified_at
        || timestamps.organization_modified_at != organization_modified_at
    {
        return Err(format!("{path} timestamps are not the fixed deterministic values").into());
    }

    let notebook_id = package
        .document
        .organization
        .notebook_id
        .as_deref()
        .ok_or_else(|| format!("{path} has no notebook assignment"))?;
    if !notebooks.contains_key(notebook_id) {
        return Err(format!("{path} references an unknown notebook").into());
    }
    if package.document.organization.trashed_at.is_some() {
        return Err(format!("{path} is unexpectedly trashed").into());
    }
    let expected_notebook = match reference.collection {
        Collection::Library => {
            let index = model::notebook_index(scale, serial);
            model::notebooks(scale)[index].notebook_id.clone()
        }
        Collection::Sample => default_notebook_id.to_owned(),
    };
    if notebook_id != expected_notebook {
        return Err(format!("{path} is assigned to an unexpected notebook").into());
    }

    let segments = content::search_segments(&package);
    let unique_hits = segments
        .iter()
        .filter(|segment| segment.contains(&reference.unique_token))
        .count();
    if unique_hits != 1 {
        return Err(format!(
            "{path} must contain its unique token {} exactly once (found {unique_hits})",
            reference.unique_token
        )
        .into());
    }
    match reference.collection {
        Collection::Library => {
            let shared = model::shared_token(reference.profile);
            if !segments.iter().any(|segment| segment.contains(shared)) {
                return Err(format!("{path} is missing its shared token {shared}").into());
            }
        }
        Collection::Sample => {
            for profile in [Profile::Text, Profile::Ordinary, Profile::Stress] {
                if segments
                    .iter()
                    .any(|segment| segment.contains(model::shared_token(profile)))
                {
                    return Err(format!("{path} must not carry shared library tokens").into());
                }
            }
        }
    }
    let astral = segments
        .iter()
        .any(|segment| segment.contains(model::ASTRAL_QUERY));
    if astral != model::uses_astral_token(reference) {
        return Err(format!("{path} astral-token placement disagrees with the model").into());
    }

    Ok(ObservedNote {
        path: path.to_owned(),
        note_id: reference.note_id.clone(),
        profile,
        collection: reference.collection,
        text_chars,
        image_count,
        map_nodes,
        map_id,
        node_ids,
        sha256,
        bytes,
        images,
        segments,
    })
}

fn image_records(
    path: &str,
    reference: &NoteRef,
    package: &NotePackage,
    stats: &ContentStats,
) -> Result<Vec<ObservedImage>, Box<dyn Error>> {
    let mut records = Vec::with_capacity(stats.images.len());
    for (asset_id, side) in &stats.images {
        let bytes = package
            .assets
            .get(asset_id)
            .ok_or_else(|| format!("{path} image {asset_id} is missing from the package"))?;
        let (ancillary_bytes, capa_chunks) = png::ancillary_profile(bytes)
            .map_err(|error| format!("{path} image {asset_id}: {error}"))?;
        if capa_chunks != 1 {
            return Err(
                format!("{path} image {asset_id} must carry exactly one caPa chunk").into(),
            );
        }
        let entry = package
            .manifest
            .assets
            .iter()
            .find(|entry| &entry.id == asset_id)
            .ok_or_else(|| format!("{path} image {asset_id} has no manifest entry"))?;
        if entry.width != *side || entry.height != *side {
            return Err(format!("{path} image {asset_id} dimensions disagree").into());
        }
        check_pattern(path, asset_id, *side, bytes)?;
        records.push(ObservedImage {
            path: path.to_owned(),
            note_id: reference.note_id.clone(),
            asset_id: asset_id.clone(),
            width: *side,
            height: *side,
            bytes: bytes.len() as u64,
            ancillary_bytes,
            sha256: png::sha256_bytes(bytes),
        });
    }
    Ok(records)
}

fn check_pattern(
    path: &str,
    asset_id: &str,
    side: u32,
    bytes: &[u8],
) -> Result<(), Box<dyn Error>> {
    let seed = png::pattern_seed(asset_id);
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|error| format!("{path} image {asset_id} does not decode: {error}"))?
        .to_luma8();
    if image.width() != side || image.height() != side {
        return Err(format!("{path} image {asset_id} decodes to unexpected dimensions").into());
    }
    for y in (0..side).step_by(101) {
        for x in (0..side).step_by(89) {
            if image.get_pixel(x, y)[0] != png::expected_pixel(side, seed, x, y) {
                return Err(format!(
                    "{path} image {asset_id} pixel ({x},{y}) is not the deterministic pattern"
                )
                .into());
            }
        }
    }
    Ok(())
}

fn check_map_shape(
    path: &str,
    map_id: &str,
    root_id: &str,
    nodes: &[MindMapNode],
) -> Result<(), Box<dyn Error>> {
    let shape = model::map_shape(nodes.len());
    if shape.root_children > model::MAX_ROOT_CHILDREN || shape.node_count() != nodes.len() {
        return Err(format!("{path} map {map_id} has an unsupported shape").into());
    }
    let by_id: HashMap<&str, &MindMapNode> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let root = by_id
        .get(root_id)
        .ok_or_else(|| format!("{path} map {map_id} root is missing"))?;
    if root.children.len() != shape.root_children {
        return Err(format!(
            "{path} map {map_id} root has {} children, expected {}",
            root.children.len(),
            shape.root_children
        )
        .into());
    }
    let mut leaves = 0usize;
    for (branch_index, branch_id) in root.children.iter().enumerate() {
        let branch = by_id
            .get(branch_id.as_str())
            .ok_or_else(|| format!("{path} map {map_id} branch is missing"))?;
        let expected = shape.base_leaves + usize::from(branch_index < shape.extra_leaves);
        if branch.children.len() != expected {
            return Err(format!(
                "{path} map {map_id} branch {branch_index} has {} children, expected {expected}",
                branch.children.len()
            )
            .into());
        }
        for leaf_id in &branch.children {
            let leaf = by_id
                .get(leaf_id.as_str())
                .ok_or_else(|| format!("{path} map {map_id} leaf is missing"))?;
            if !leaf.children.is_empty() {
                return Err(format!("{path} map {map_id} must stay a few-level map").into());
            }
            leaves += 1;
        }
    }
    if leaves + 1 + shape.root_children != nodes.len() {
        return Err(format!("{path} map {map_id} node accounting disagrees").into());
    }
    Ok(())
}

fn collect_stats(blocks: &[Block], stats: &mut ContentStats) {
    for block in blocks {
        collect_block_stats(block, stats);
    }
}

fn collect_block_stats(block: &Block, stats: &mut ContentStats) {
    match block {
        Block::Paragraph { children } => {
            collect_inline_features(children, stats);
        }
        Block::Heading1 { children } => {
            stats.headings.insert(1);
            collect_inline_features(children, stats);
        }
        Block::Heading2 { children } => {
            stats.headings.insert(2);
            collect_inline_features(children, stats);
        }
        Block::Heading3 { children } => {
            stats.headings.insert(3);
            collect_inline_features(children, stats);
        }
        Block::OrderedList { items, .. } => {
            stats.has_ordered_list = true;
            for item in items {
                collect_stats(&item.blocks, stats);
            }
        }
        Block::BulletList { items } => {
            stats.has_bullet_list = true;
            for item in items {
                collect_stats(&item.blocks, stats);
            }
        }
        Block::TaskList { items } => {
            stats.has_task_list = true;
            for item in items {
                collect_stats(&item.blocks, stats);
            }
        }
        Block::Blockquote { blocks } => {
            stats.has_blockquote = true;
            collect_stats(blocks, stats);
        }
        Block::CodeBlock { language, .. } => {
            stats.code_blocks += 1;
            if let Some(language) = language {
                if REQUIRED_LANGUAGES.contains(&language.as_str()) {
                    stats.languages.insert(language.clone());
                } else {
                    stats.unknown_language = true;
                }
            }
        }
        Block::Table { rows } => {
            stats.has_table = true;
            for row in rows {
                let cells = match row {
                    TableRow::Header { cells } | TableRow::Body { cells } => cells,
                };
                for cell in cells {
                    collect_inline_features(cell, stats);
                }
            }
        }
        Block::MathBlock { latex } => stats.math_blocks.push(latex.clone()),
        Block::Image {
            asset_id,
            display_width_px,
        } => stats.images.push((asset_id.clone(), *display_width_px)),
        Block::MindMap {
            map_id,
            root_id,
            nodes,
        } => {
            stats.map = Some((map_id.clone(), root_id.clone(), nodes.clone()));
        }
        Block::HorizontalRule => {}
    }
}

fn collect_inline_features(inlines: &[Inline], stats: &mut ContentStats) {
    for inline in inlines {
        match inline {
            Inline::Text { marks, .. } => {
                for mark in marks {
                    stats.marks.insert(match mark {
                        cuenote_lib::note_format::Mark::Bold => "bold",
                        cuenote_lib::note_format::Mark::Italic => "italic",
                        cuenote_lib::note_format::Mark::Strike => "strike",
                        cuenote_lib::note_format::Mark::Code => "code",
                        cuenote_lib::note_format::Mark::Link { .. } => "link",
                    });
                }
            }
            Inline::MathInline { latex } => stats.inline_math.push(latex.clone()),
            Inline::HardBreak => {}
        }
    }
}

fn check_rich_format(path: &str, stats: &ContentStats) -> Result<(), Box<dyn Error>> {
    for requirement in [
        ("bold", stats.marks.contains("bold")),
        ("italic", stats.marks.contains("italic")),
        ("strike", stats.marks.contains("strike")),
        ("code mark", stats.marks.contains("code")),
        ("link mark", stats.marks.contains("link")),
        ("heading level 1", stats.headings.contains(&1)),
        ("heading level 2", stats.headings.contains(&2)),
        ("table", stats.has_table),
        ("blockquote", stats.has_blockquote),
        ("task list", stats.has_task_list),
        ("ordered list", stats.has_ordered_list),
        ("bullet list", stats.has_bullet_list),
    ] {
        if !requirement.1 {
            return Err(format!("{path} is missing {}", requirement.0).into());
        }
    }
    Ok(())
}

fn check_simple_format(path: &str, stats: &ContentStats) -> Result<(), Box<dyn Error>> {
    if !stats.has_bullet_list || !stats.headings.contains(&2) || !stats.headings.contains(&3) {
        return Err(format!(
            "{path} plain-text notes need level-2/level-3 headings and a bullet list"
        )
        .into());
    }
    Ok(())
}

fn check_coverage(path: &str, stats: &ContentStats) -> Result<(), Box<dyn Error>> {
    for language in REQUIRED_LANGUAGES {
        if !stats.languages.contains(language) {
            return Err(format!("{path} is missing the {language} code sample").into());
        }
    }
    if !stats.unknown_language {
        return Err(format!("{path} is missing an unknown-language code sample").into());
    }
    if stats.inline_math.is_empty() || stats.math_blocks.len() < 2 {
        return Err(format!("{path} is missing inline or block math samples").into());
    }
    let mut latex = String::new();
    for value in stats.inline_math.iter().chain(stats.math_blocks.iter()) {
        latex.push_str(value);
    }
    for term in LATEX_TERMS {
        if !latex.contains(term) {
            return Err(format!("{path} math samples are missing {term:?}").into());
        }
    }
    Ok(())
}

fn check_queries(
    manifest: &CorpusManifest,
    scale: &Scale,
    observed: &[ObservedNote],
) -> Result<(), Box<dyn Error>> {
    let mut folded: Vec<(&str, Vec<String>)> = Vec::with_capacity(observed.len());
    for note in observed {
        folded.push((
            note.note_id.as_str(),
            note.segments
                .iter()
                .map(|segment| segment.to_ascii_lowercase())
                .collect(),
        ));
    }

    let mut manifest_queries: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for record in &manifest.known_queries {
        let mut ids = record.note_ids.clone();
        ids.sort();
        ids.dedup();
        if ids != record.note_ids {
            return Err(format!(
                "manifest query {:?} note ids are not sorted and unique",
                record.query
            )
            .into());
        }
        if manifest_queries
            .insert(record.query.as_str(), record.note_ids.clone())
            .is_some()
        {
            return Err(format!("duplicate manifest query {:?}", record.query).into());
        }
    }

    let mut model_queries: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in model::expected_queries(scale) {
        model_queries.insert(record.query, record.note_ids);
    }
    let manifest_keys: BTreeSet<&str> = manifest_queries.keys().copied().collect();
    let model_keys: BTreeSet<&str> = model_queries.keys().map(String::as_str).collect();
    if manifest_keys != model_keys {
        return Err("manifest query list disagrees with the deterministic model".into());
    }

    for (query, expected) in &manifest_queries {
        let needle = query.to_ascii_lowercase();
        let mut actual: Vec<String> = folded
            .iter()
            .filter(|(_, segments)| segments.iter().any(|segment| segment.contains(&needle)))
            .map(|(note_id, _)| (*note_id).to_owned())
            .collect();
        actual.sort();
        if &actual != expected {
            return Err(format!(
                "query {query:?} hits {} notes on disk but {} are recorded",
                actual.len(),
                expected.len()
            )
            .into());
        }
        let model_expected = model_queries.get(*query).expect("query key sets are equal");
        if &actual != model_expected {
            return Err(
                format!("query {query:?} hits disagree with the deterministic model").into(),
            );
        }
    }
    Ok(())
}

fn check_totals(
    manifest: &CorpusManifest,
    files: &[FileRecord],
    observed: &[ObservedNote],
) -> Result<(), Box<dyn Error>> {
    let bytes: u64 = files
        .iter()
        .filter(|record| record.path != model::MANIFEST_PATH)
        .filter_map(|record| record.bytes)
        .sum();
    let images = observed.iter().map(|note| note.image_count).sum::<usize>();
    let image_bytes = observed
        .iter()
        .flat_map(|note| note.images.iter())
        .map(|image| image.bytes)
        .sum::<u64>();
    let ancillary_bytes = observed
        .iter()
        .flat_map(|note| note.images.iter())
        .map(|image| image.ancillary_bytes)
        .sum::<u64>();
    let map_nodes = observed.iter().map(|note| note.map_nodes).sum::<usize>();
    let mut text = 0u64;
    let mut ordinary = 0u64;
    let mut stress = 0u64;
    for note in observed {
        match note.profile {
            Profile::Text => text += note.text_chars as u64,
            Profile::Ordinary => ordinary += note.text_chars as u64,
            Profile::Stress => stress += note.text_chars as u64,
        }
    }
    let totals = &manifest.totals;
    let mut mismatches: Vec<String> = Vec::new();
    macro_rules! compare {
        ($label:literal, $recorded:expr, $actual:expr) => {
            if $recorded != $actual {
                mismatches.push(format!(
                    "{}: manifest {} vs verified {}",
                    $label, $recorded, $actual
                ));
            }
        };
    }
    compare!("files", totals.files, files.len());
    compare!("bytes", totals.bytes, bytes);
    compare!("notes", totals.notes, observed.len());
    compare!("images", totals.images, images);
    compare!("imageBytes", totals.image_bytes, image_bytes);
    compare!("ancillaryBytes", totals.ancillary_bytes, ancillary_bytes);
    compare!("mapNodes", totals.map_nodes, map_nodes);
    compare!("textChars.text", totals.text_chars.text, text);
    compare!("textChars.ordinary", totals.text_chars.ordinary, ordinary);
    compare!("textChars.stress", totals.text_chars.stress, stress);
    if !totals.manifest_bytes_excluded {
        mismatches.push("manifestBytesExcluded must be true".to_owned());
    }
    if !mismatches.is_empty() {
        return Err(format!(
            "manifest totals disagree with the verified corpus: {}",
            mismatches.join("; ")
        )
        .into());
    }
    Ok(())
}

fn determinism_spot_checks(
    root: &Path,
    scale: &Scale,
    default_notebook_id: &str,
) -> Result<usize, Box<dyn Error>> {
    let mut checks = 0usize;
    let mut refs: Vec<NoteRef> = Vec::new();
    let library = model::library_notes(scale);
    for profile in [Profile::Text, Profile::Ordinary, Profile::Stress] {
        if let Some(note) = library.iter().find(|note| note.profile == profile) {
            refs.push(note.clone());
        }
    }
    refs.extend(model::sample_notes());

    for reference in &refs {
        let serial = model::note_serial(scale, reference);
        let notebook_id = match reference.collection {
            Collection::Library => {
                let index = model::notebook_index(scale, serial);
                model::notebooks(scale)[index].notebook_id.clone()
            }
            Collection::Sample => default_notebook_id.to_owned(),
        };
        let built = content::build_note(reference, &notebook_id, serial)?;
        let path = match reference.collection {
            Collection::Library => {
                format!("{}/notes/{}.cuenote", model::LIBRARY_DIR, reference.note_id)
            }
            Collection::Sample => {
                let index = usize::from(reference.profile == Profile::Stress);
                model::SAMPLE_PATHS[index].to_owned()
            }
        };
        let disk = fs::read(root.join(&path))?;
        if disk.len() != built.bytes.len()
            || manifest::sha256_bytes(&disk) != manifest::sha256_bytes(&built.bytes)
        {
            return Err(
                format!("regenerated package for {path} is not byte-identical to disk").into(),
            );
        }
        checks += 1;
    }

    let notebook = &model::notebooks(scale)[0];
    let rebuilt = manifest::notebook_metadata_bytes(notebook)?;
    let disk = fs::read(
        root.join(model::LIBRARY_DIR)
            .join("notebooks")
            .join(format!("{}.json", notebook.notebook_id)),
    )?;
    if disk != rebuilt {
        return Err("regenerated notebook metadata is not byte-identical to disk".into());
    }
    checks += 1;
    Ok(checks)
}

fn is_valid_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string() == value)
        .unwrap_or(false)
}

fn is_stable_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .map(|parsed| parsed.get_version_num() == 4 && parsed.to_string() == value)
        .unwrap_or(false)
}
