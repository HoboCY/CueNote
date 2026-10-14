use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Cursor;

use chrono::DateTime;
use image::{ImageFormat, ImageReader, Limits};
use serde::de::{DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserializer;
use serde_json::{Map, Number, Value};
use sha2::Digest;
use uuid::{Uuid, Version};

use super::package::{FormatError, NotePackage};
use super::{AssetManifest, Block, Document, Inline, Mark, MediaType, TableRow};

pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_JSON_DEPTH: usize = 64;
pub const MAX_ASSETS: usize = 8_192;
pub const MAX_MAP_NODES: usize = 5_000;
pub const MAX_PIXELS_PER_IMAGE: u64 = 25_000_000;
pub const MAX_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_IMAGE_DIMENSION: u32 = 25_000_000;
const MAX_BLOCK_NESTING: usize = 32;

#[derive(Default)]
pub(crate) struct DocumentReferences {
    pub asset_ids: BTreeSet<String>,
}

pub(crate) fn parse_document_json(bytes: &[u8]) -> Result<Document, FormatError> {
    parse_versioned_json(bytes, "invalid_document", "document")
}

pub(crate) fn parse_manifest_json(bytes: &[u8]) -> Result<AssetManifest, FormatError> {
    parse_versioned_json(bytes, "invalid_manifest", "manifest")
}

fn parse_versioned_json<T: DeserializeOwned>(
    bytes: &[u8],
    kind: &'static str,
    label: &str,
) -> Result<T, FormatError> {
    // Duplicate keys and excessive nesting are rejected before consulting the version.
    // Version is then checked before v1 AST decoding so newer fields/nodes produce a
    // precise upgrade message rather than an ordinary corruption error.
    let value = parse_json_value(bytes, kind)?;
    if let Some(version) = value
        .as_object()
        .and_then(|object| object.get("schemaVersion"))
        .and_then(Value::as_u64)
    {
        if version != 1 {
            return Err(FormatError::new(
                "unsupported_version",
                format!("Unsupported {label} schema version {version}."),
            ));
        }
    }
    require_nullable_fields(&value, kind)?;
    serde_json::from_value(value).map_err(|error| FormatError::new(kind, error.to_string()))
}

fn parse_json_value(bytes: &[u8], kind: &'static str) -> Result<Value, FormatError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = JsonSeed { depth: 0 }
        .deserialize(&mut deserializer)
        .map_err(|error| FormatError::new(kind, error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| FormatError::new(kind, error.to_string()))?;
    Ok(value)
}

pub(crate) fn validate_json_depth(bytes: &[u8], kind: &'static str) -> Result<(), FormatError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    DepthSeed { depth: 0 }
        .deserialize(&mut deserializer)
        .map_err(|error| FormatError::new(kind, error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| FormatError::new(kind, error.to_string()))
}

pub(crate) fn validate_document(document: &Document) -> Result<DocumentReferences, FormatError> {
    if document.schema_version != 1 {
        return Err(FormatError::new(
            "unsupported_version",
            format!(
                "Unsupported document schema version {}.",
                document.schema_version
            ),
        ));
    }
    validate_uuid(&document.note_id, "noteId")?;
    validate_timestamp(&document.timestamps.created_at, "createdAt")?;
    validate_timestamp(
        &document.timestamps.content_modified_at,
        "contentModifiedAt",
    )?;
    validate_timestamp(
        &document.timestamps.organization_modified_at,
        "organizationModifiedAt",
    )?;
    if let Some(notebook_id) = &document.organization.notebook_id {
        validate_uuid(notebook_id, "notebookId")?;
    }
    if let Some(trashed_at) = &document.organization.trashed_at {
        validate_timestamp(trashed_at, "trashedAt")?;
    }

    let mut references = DocumentReferences::default();
    let mut map_ids = HashSet::new();
    let mut node_ids = HashSet::new();
    let mut map_node_count = 0usize;
    validate_blocks(
        &document.sections.cue,
        true,
        false,
        0,
        &mut references,
        &mut map_ids,
        &mut node_ids,
        &mut map_node_count,
    )?;
    validate_blocks(
        &document.sections.content,
        false,
        true,
        0,
        &mut references,
        &mut map_ids,
        &mut node_ids,
        &mut map_node_count,
    )?;
    validate_blocks(
        &document.sections.summary,
        true,
        false,
        0,
        &mut references,
        &mut map_ids,
        &mut node_ids,
        &mut map_node_count,
    )?;
    Ok(references)
}

pub(crate) fn validate_package(package: &NotePackage) -> Result<(), FormatError> {
    let references = validate_document(&package.document)?;
    validate_manifest(&package.manifest)?;

    if package.assets.len() > MAX_ASSETS || package.manifest.assets.len() > MAX_ASSETS {
        return Err(resource_error("The package contains too many assets."));
    }
    let mut manifest_ids = BTreeSet::new();
    let mut total_pixels = 0u64;
    let mut total_asset_bytes = 0u64;
    for entry in &package.manifest.assets {
        validate_uuid(&entry.id, "asset id")?;
        if !manifest_ids.insert(entry.id.clone()) {
            return Err(FormatError::new(
                "invalid_manifest",
                format!("Duplicate asset id {}.", entry.id),
            ));
        }
        let bytes = package.assets.get(&entry.id).ok_or_else(|| {
            FormatError::new(
                "invalid_manifest",
                format!("Referenced asset {} is missing.", entry.id),
            )
        })?;
        if entry.byte_length != bytes.len() as u64 {
            return Err(FormatError::new(
                "asset_integrity",
                format!("Asset {} has a mismatched byte length.", entry.id),
            ));
        }
        let actual_hash = format!("{:x}", sha2::Sha256::digest(bytes));
        if entry.sha256 != actual_hash {
            return Err(FormatError::new(
                "asset_integrity",
                format!("Asset {} has a mismatched SHA-256 digest.", entry.id),
            ));
        }
        let (width, height) = decode_asset(bytes, entry.media_type)?;
        if (entry.width, entry.height) != (width, height) {
            return Err(FormatError::new(
                "asset_integrity",
                format!("Asset {} has mismatched image dimensions.", entry.id),
            ));
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| resource_error("Image dimensions overflow the pixel budget."))?;
        if pixels == 0 || pixels > MAX_PIXELS_PER_IMAGE {
            return Err(resource_error(&format!(
                "Asset {} exceeds the per-image pixel limit.",
                entry.id
            )));
        }
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or_else(|| resource_error("Images exceed the total pixel budget."))?;
        if total_pixels > MAX_TOTAL_IMAGE_PIXELS {
            return Err(resource_error("Images exceed the total pixel budget."));
        }
        total_asset_bytes = total_asset_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| resource_error("Expanded package exceeds its size budget."))?;
        if total_asset_bytes > MAX_EXPANDED_BYTES {
            return Err(resource_error("Expanded package exceeds its size budget."));
        }
    }

    if manifest_ids != references.asset_ids {
        return Err(FormatError::new(
            "invalid_manifest",
            "The manifest must contain all and only referenced image assets.",
        ));
    }
    let package_asset_ids = package.assets.keys().cloned().collect::<BTreeSet<_>>();
    if package_asset_ids != manifest_ids {
        return Err(FormatError::new(
            "invalid_manifest",
            "The asset files must match the manifest entries exactly.",
        ));
    }
    Ok(())
}

fn validate_manifest(manifest: &AssetManifest) -> Result<(), FormatError> {
    if manifest.schema_version != 1 {
        return Err(FormatError::new(
            "unsupported_version",
            format!(
                "Unsupported manifest schema version {}.",
                manifest.schema_version
            ),
        ));
    }
    if manifest.assets.len() > MAX_ASSETS {
        return Err(resource_error("The package contains too many assets."));
    }
    Ok(())
}

fn validate_blocks(
    blocks: &[Block],
    simple_section: bool,
    top_content: bool,
    nesting: usize,
    references: &mut DocumentReferences,
    map_ids: &mut HashSet<String>,
    node_ids: &mut HashSet<String>,
    map_node_count: &mut usize,
) -> Result<(), FormatError> {
    if nesting > MAX_BLOCK_NESTING {
        return Err(resource_error(
            "The document structure is nested too deeply.",
        ));
    }
    for block in blocks {
        match block {
            Block::Paragraph { children }
            | Block::Heading1 { children }
            | Block::Heading2 { children }
            | Block::Heading3 { children } => {
                validate_inlines(children, simple_section)?;
            }
            Block::OrderedList { start, items } => {
                if *start == 0 {
                    return Err(invalid_document("Ordered list start must be positive."));
                }
                for item in items {
                    validate_blocks(
                        &item.blocks,
                        simple_section,
                        false,
                        nesting + 1,
                        references,
                        map_ids,
                        node_ids,
                        map_node_count,
                    )?;
                }
            }
            Block::BulletList { items } => {
                for item in items {
                    validate_blocks(
                        &item.blocks,
                        simple_section,
                        false,
                        nesting + 1,
                        references,
                        map_ids,
                        node_ids,
                        map_node_count,
                    )?;
                }
            }
            Block::TaskList { items } => {
                for item in items {
                    validate_blocks(
                        &item.blocks,
                        simple_section,
                        false,
                        nesting + 1,
                        references,
                        map_ids,
                        node_ids,
                        map_node_count,
                    )?;
                }
            }
            Block::Blockquote { blocks } if !simple_section => validate_blocks(
                blocks,
                false,
                false,
                nesting + 1,
                references,
                map_ids,
                node_ids,
                map_node_count,
            )?,
            Block::CodeBlock { .. }
            | Block::HorizontalRule
            | Block::Table { .. }
            | Block::MathBlock { .. }
                if !simple_section =>
            {
                if let Block::Table { rows } = block {
                    validate_table(rows)?;
                }
            }
            Block::Image {
                asset_id,
                display_width_px,
            } if !simple_section => {
                if !top_content {
                    return Err(invalid_document(
                        "Images are allowed only as top-level content blocks.",
                    ));
                }
                validate_uuid(asset_id, "assetId")?;
                if *display_width_px == 0 {
                    return Err(invalid_document("Image display width must be positive."));
                }
                references.asset_ids.insert(asset_id.clone());
            }
            Block::MindMap {
                map_id,
                root_id,
                nodes,
            } if !simple_section => {
                if !top_content {
                    return Err(invalid_document(
                        "Mind maps are allowed only as top-level content blocks.",
                    ));
                }
                validate_mind_map(map_id, root_id, nodes, map_ids, node_ids, map_node_count)?;
            }
            _ => {
                return Err(invalid_document(
                    "This block is not allowed in the cue or summary section.",
                ));
            }
        }
    }
    Ok(())
}

fn validate_inlines(inlines: &[Inline], simple_section: bool) -> Result<(), FormatError> {
    for inline in inlines {
        match inline {
            Inline::Text { marks, .. } => {
                for mark in marks {
                    match mark {
                        Mark::Bold => {}
                        Mark::Italic | Mark::Strike | Mark::Code | Mark::Link { .. }
                            if simple_section =>
                        {
                            return Err(invalid_document(
                                "Cue and summary sections allow bold text marks only.",
                            ));
                        }
                        Mark::Link { href } => validate_link(href)?,
                        Mark::Italic | Mark::Strike | Mark::Code => {}
                    }
                }
            }
            Inline::HardBreak => {}
            Inline::MathInline { .. } if !simple_section => {}
            Inline::MathInline { .. } => {
                return Err(invalid_document(
                    "Cue and summary sections do not support inline math.",
                ));
            }
        }
    }
    Ok(())
}

fn validate_link(href: &str) -> Result<(), FormatError> {
    let Some((scheme, rest)) = href.split_once(':') else {
        return Err(invalid_document("Links must use http, https, or mailto."));
    };
    let contains_forbidden =
        |text: &str| text.chars().any(|ch| ch.is_control() || ch.is_whitespace());
    let valid = match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => {
            rest.starts_with("//")
                && !rest[2..]
                    .split(['/', '?', '#'])
                    .next()
                    .unwrap_or_default()
                    .is_empty()
                && !contains_forbidden(rest)
        }
        "mailto" => !rest.is_empty() && !contains_forbidden(rest),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_document("Links must use http, https, or mailto."))
    }
}

fn validate_table(rows: &[TableRow]) -> Result<(), FormatError> {
    if rows.is_empty() {
        return Err(invalid_document("A table must contain a header row."));
    }
    let mut expected_columns = None;
    for (index, row) in rows.iter().enumerate() {
        let cells = match (index, row) {
            (0, TableRow::Header { cells }) => cells,
            (0, _) => return Err(invalid_document("The first table row must be the header.")),
            (_, TableRow::Body { cells }) => cells,
            (_, TableRow::Header { .. }) => {
                return Err(invalid_document(
                    "Only the first table row may be a header.",
                ));
            }
        };
        if cells.is_empty() {
            return Err(invalid_document(
                "Table rows must contain at least one cell.",
            ));
        }
        if expected_columns.get_or_insert(cells.len()) != &cells.len() {
            return Err(invalid_document(
                "Table rows must have the same number of cells.",
            ));
        }
        for cell in cells {
            validate_inlines(cell, false)?;
        }
    }
    Ok(())
}

fn validate_mind_map(
    map_id: &str,
    root_id: &str,
    nodes: &[super::MindMapNode],
    map_ids: &mut HashSet<String>,
    global_node_ids: &mut HashSet<String>,
    aggregate_count: &mut usize,
) -> Result<(), FormatError> {
    validate_uuid(map_id, "mapId")?;
    validate_uuid(root_id, "rootId")?;
    if !map_ids.insert(map_id.to_owned()) {
        return Err(invalid_document("Mind map IDs must be unique."));
    }
    if nodes.is_empty() {
        return Err(invalid_document("A mind map must contain a root node."));
    }
    *aggregate_count = aggregate_count
        .checked_add(nodes.len())
        .ok_or_else(|| resource_error("Mind maps exceed the node budget."))?;
    if *aggregate_count > MAX_MAP_NODES {
        return Err(resource_error("Mind maps exceed the node budget."));
    }

    let mut local_ids = HashMap::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        validate_uuid(&node.id, "mind map node id")?;
        if !global_node_ids.insert(node.id.clone())
            || local_ids.insert(node.id.clone(), index).is_some()
        {
            return Err(invalid_document("Mind map node IDs must be unique."));
        }
    }
    let Some(&root) = local_ids.get(root_id) else {
        return Err(invalid_document(
            "Mind map rootId does not identify a node.",
        ));
    };
    let mut parents = vec![0u8; nodes.len()];
    for node in nodes {
        let mut children_seen = HashSet::new();
        for child_id in &node.children {
            validate_uuid(child_id, "mind map child id")?;
            if !children_seen.insert(child_id) {
                return Err(invalid_document("Mind map child IDs must not repeat."));
            }
            let Some(&child_index) = local_ids.get(child_id) else {
                return Err(invalid_document("Mind map child does not identify a node."));
            };
            parents[child_index] = parents[child_index].saturating_add(1);
            if parents[child_index] > 1 {
                return Err(invalid_document(
                    "Every non-root mind map node must have exactly one parent.",
                ));
            }
        }
    }
    if parents[root] != 0
        || parents
            .iter()
            .enumerate()
            .any(|(index, count)| index != root && *count != 1)
    {
        return Err(invalid_document(
            "A mind map must have one root and exactly one parent per other node.",
        ));
    }

    let mut colors = vec![0u8; nodes.len()];
    colors[root] = 1;
    let mut stack = vec![(root, 0usize)];
    while let Some((node_index, child_offset)) = stack.last_mut() {
        if *child_offset == nodes[*node_index].children.len() {
            colors[*node_index] = 2;
            stack.pop();
            continue;
        }
        let child_id = &nodes[*node_index].children[*child_offset];
        *child_offset += 1;
        let child_index = local_ids[child_id];
        match colors[child_index] {
            0 => {
                colors[child_index] = 1;
                stack.push((child_index, 0));
            }
            1 => return Err(invalid_document("Mind maps cannot contain cycles.")),
            _ => {}
        }
    }
    if colors.iter().any(|color| *color != 2) {
        return Err(invalid_document(
            "Every mind map node must be reachable from rootId.",
        ));
    }
    Ok(())
}

fn decode_asset(bytes: &[u8], media_type: MediaType) -> Result<(u32, u32), FormatError> {
    let probe = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| FormatError::new("invalid_asset", error.to_string()))?;
    let format = probe
        .format()
        .ok_or_else(|| invalid_asset("Image format could not be identified."))?;
    let expected_format = match media_type {
        MediaType::Png => ImageFormat::Png,
        MediaType::Jpeg => ImageFormat::Jpeg,
    };
    if format != expected_format {
        return Err(invalid_asset(
            "Image media type does not match its contents.",
        ));
    }
    let dimensions = probe
        .into_dimensions()
        .map_err(|error| FormatError::new("invalid_asset", error.to_string()))?;
    let pixels = u64::from(dimensions.0)
        .checked_mul(u64::from(dimensions.1))
        .ok_or_else(|| resource_error("Image dimensions overflow the pixel budget."))?;
    if pixels == 0 || pixels > MAX_PIXELS_PER_IMAGE {
        return Err(resource_error(
            "An image exceeds the per-image pixel limit.",
        ));
    }

    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| FormatError::new("invalid_asset", error.to_string()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(512 * 1024 * 1024);
    decoder.limits(limits);
    let decoded = decoder
        .decode()
        .map_err(|error| FormatError::new("invalid_asset", error.to_string()))?;
    if (decoded.width(), decoded.height()) != dimensions {
        return Err(invalid_asset("Decoded image dimensions changed."));
    }
    Ok(dimensions)
}

fn validate_uuid(value: &str, field: &str) -> Result<(), FormatError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|_| invalid_document(&format!("{field} must be a lowercase UUID v4.")))?;
    if parsed.to_string() != value || parsed.get_version() != Some(Version::Random) {
        return Err(invalid_document(&format!(
            "{field} must be a lowercase UUID v4."
        )));
    }
    Ok(())
}

fn validate_timestamp(value: &str, field: &str) -> Result<(), FormatError> {
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| {
        invalid_document(&format!(
            "{field} must be a UTC timestamp with milliseconds."
        ))
    })?;
    if parsed.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string() != value {
        return Err(invalid_document(&format!(
            "{field} must be a UTC timestamp with milliseconds."
        )));
    }
    Ok(())
}

fn require_nullable_fields(value: &Value, kind: &'static str) -> Result<(), FormatError> {
    match value {
        Value::Array(values) => {
            for value in values {
                require_nullable_fields(value, kind)?;
            }
        }
        Value::Object(object) => {
            if object.get("type") == Some(&Value::String("codeBlock".into()))
                && !object.contains_key("language")
            {
                return Err(FormatError::new(
                    kind,
                    "codeBlock.language is required and nullable.",
                ));
            }
            if object.contains_key("pinned")
                && (!object.contains_key("notebookId") || !object.contains_key("trashedAt"))
            {
                return Err(FormatError::new(
                    kind,
                    "organization.notebookId and organization.trashedAt are required and nullable.",
                ));
            }
            for value in object.values() {
                require_nullable_fields(value, kind)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn invalid_document(message: &str) -> FormatError {
    FormatError::new("invalid_document", message)
}

fn invalid_asset(message: &str) -> FormatError {
    FormatError::new("invalid_asset", message)
}

fn resource_error(message: &str) -> FormatError {
    FormatError::new("resource_limit", message)
}

struct JsonSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for JsonSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(JsonVisitor { depth: self.depth })
    }
}

struct JsonVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON numbers are not allowed"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom("JSON nesting exceeds 64 levels"));
        }
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(JsonSeed {
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom("JSON nesting exceeds 64 levels"));
        }
        let mut values = Map::new();
        let mut keys = HashSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let value = object.next_value_seed(JsonSeed {
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

struct DepthSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for DepthSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DepthVisitor { depth: self.depth })
    }
}

struct DepthVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for DepthVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value within the supported nesting depth")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_string<E>(self, _: String) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom("JSON nesting exceeds 64 levels"));
        }
        while sequence
            .next_element_seed(DepthSeed {
                depth: self.depth + 1,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut object: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom("JSON nesting exceeds 64 levels"));
        }
        while object.next_key::<IgnoredAny>()?.is_some() {
            object.next_value_seed(DepthSeed {
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}
