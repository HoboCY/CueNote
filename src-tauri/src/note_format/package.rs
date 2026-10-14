use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::{Read, Seek, Write};

use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;

use super::{
    AssetManifest, Block, Document, Inline, NoteContent, Organization, PlainText, Sections,
    Timestamps,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotePackage {
    pub document: Document,
    pub manifest: AssetManifest,
    pub assets: BTreeMap<String, Vec<u8>>,
}

impl NotePackage {
    pub fn content(&self) -> NoteContent {
        NoteContent {
            title: self.document.title.clone(),
            sections: self.document.sections.clone(),
        }
    }

    pub fn replace_content(&mut self, content: NoteContent) {
        if self.content() == content {
            return;
        }
        self.document.title = content.title;
        self.document.sections = content.sections;
        self.document.timestamps.content_modified_at =
            next_modified_at(&self.document.timestamps.content_modified_at);
    }

    pub(crate) fn validate(&self) -> Result<(), FormatError> {
        super::validation::validate_package(self)
    }

    pub fn new_plain(notebook_id: String, text: PlainText) -> Self {
        let now = now_string();
        Self {
            document: Document {
                schema_version: 1,
                note_id: Uuid::new_v4().to_string(),
                title: text.title,
                sections: Sections {
                    cue: blocks_from_plain(&text.cue),
                    content: blocks_from_plain(&text.content),
                    summary: blocks_from_plain(&text.summary),
                },
                organization: Organization {
                    notebook_id: Some(notebook_id),
                    pinned: false,
                    trashed_at: None,
                },
                timestamps: Timestamps {
                    created_at: now.clone(),
                    content_modified_at: now.clone(),
                    organization_modified_at: now,
                },
            },
            manifest: AssetManifest {
                schema_version: 1,
                assets: Vec::new(),
            },
            assets: BTreeMap::new(),
        }
    }

    pub fn plain_text(&self) -> Option<PlainText> {
        Some(PlainText {
            title: self.document.title.clone(),
            cue: plain_section(&self.document.sections.cue)?,
            content: plain_section(&self.document.sections.content)?,
            summary: plain_section(&self.document.sections.summary)?,
        })
    }

    pub fn text_projection(&self) -> PlainText {
        PlainText {
            title: self.document.title.clone(),
            cue: project_blocks(&self.document.sections.cue),
            content: project_blocks(&self.document.sections.content),
            summary: project_blocks(&self.document.sections.summary),
        }
    }

    pub fn replace_plain_text(&mut self, text: PlainText) -> Result<(), FormatError> {
        if self.plain_text().as_ref() == Some(&text) {
            return Ok(());
        }
        if self.plain_text().is_none() {
            return Err(FormatError::new(
                "read_only",
                "Rich v1 content cannot be replaced by the plain-text editor.",
            ));
        }

        self.document.title = text.title;
        self.document.sections = Sections {
            cue: blocks_from_plain(&text.cue),
            content: blocks_from_plain(&text.content),
            summary: blocks_from_plain(&text.summary),
        };
        self.document.timestamps.content_modified_at =
            next_modified_at(&self.document.timestamps.content_modified_at);
        Ok(())
    }
}

pub fn read_package<R: Read + Seek>(reader: R) -> Result<NotePackage, FormatError> {
    super::archive::read(reader)
}

pub fn write_package<W: Write + Seek>(writer: W, package: &NotePackage) -> Result<(), FormatError> {
    super::archive::write(writer, package)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError {
    pub kind: String,
    pub message: String,
}

impl FormatError {
    pub(crate) fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.message)
    }
}

impl Error for FormatError {}

fn now_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn blocks_from_plain(text: &str) -> Vec<Block> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut children = Vec::new();
    for (index, part) in text.split('\n').enumerate() {
        if index > 0 {
            children.push(Inline::HardBreak);
        }
        children.push(Inline::Text {
            text: part.to_owned(),
            marks: Vec::new(),
        });
    }
    vec![Block::Paragraph { children }]
}

fn plain_section(blocks: &[Block]) -> Option<String> {
    let mut paragraphs = Vec::with_capacity(blocks.len());
    for block in blocks {
        let Block::Paragraph { children } = block else {
            return None;
        };
        let mut text = String::new();
        for child in children {
            match child {
                Inline::Text { text: part, marks } if marks.is_empty() => text.push_str(part),
                Inline::HardBreak => text.push('\n'),
                _ => return None,
            }
        }
        paragraphs.push(text);
    }
    Some(paragraphs.join("\n\n"))
}

fn next_modified_at(previous: &str) -> String {
    let mut now = Utc::now();
    if let Ok(previous) = DateTime::parse_from_rfc3339(previous) {
        let previous_millis = previous.timestamp_millis();
        if now.timestamp_millis() <= previous_millis {
            if let Some(next_millis) = previous_millis.checked_add(1) {
                if let Some(next) = DateTime::<Utc>::from_timestamp_millis(next_millis) {
                    now = next;
                }
            }
        }
    }
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn project_blocks(blocks: &[Block]) -> String {
    let projected = blocks.iter().map(project_block).collect::<Vec<_>>();
    projected.join("\n\n")
}

fn project_block(block: &Block) -> String {
    match block {
        Block::Paragraph { children }
        | Block::Heading1 { children }
        | Block::Heading2 { children }
        | Block::Heading3 { children } => project_inlines(children),
        Block::OrderedList { start, items } => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                format!(
                    "{}. {}",
                    start.saturating_add(index as u32),
                    project_blocks(&item.blocks)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::BulletList { items } => items
            .iter()
            .map(|item| format!("- {}", project_blocks(&item.blocks)))
            .collect::<Vec<_>>()
            .join("\n"),
        Block::TaskList { items } => items
            .iter()
            .map(|item| {
                format!(
                    "[{}] {}",
                    if item.checked { "x" } else { " " },
                    project_blocks(&item.blocks)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Blockquote { blocks } => project_blocks(blocks)
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Block::CodeBlock { code, .. } => code.clone(),
        Block::HorizontalRule => "---".to_owned(),
        Block::Table { rows } => rows
            .iter()
            .map(|row| {
                let cells = match row {
                    super::TableRow::Header { cells } | super::TableRow::Body { cells } => cells,
                };
                cells
                    .iter()
                    .map(|cell| project_inlines(cell))
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::MathBlock { latex } => format!("$$\n{latex}\n$$"),
        Block::Image { asset_id, .. } => format!("[image {asset_id}]"),
        Block::MindMap { root_id, nodes, .. } => project_mind_map(root_id, nodes),
    }
}

fn project_inlines(inlines: &[Inline]) -> String {
    let mut output = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => output.push_str(text),
            Inline::HardBreak => output.push('\n'),
            Inline::MathInline { latex } => output.push_str(latex),
        }
    }
    output
}

fn project_mind_map(root_id: &str, nodes: &[super::MindMapNode]) -> String {
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<std::collections::HashMap<_, _>>();
    let mut lines = Vec::new();
    let mut stack = vec![(root_id, 0usize)];
    while let Some((node_id, depth)) = stack.pop() {
        if let Some(node) = by_id.get(node_id) {
            lines.push(format!("{}{}", "  ".repeat(depth.min(64)), node.text));
            for child in node.children.iter().rev() {
                stack.push((child.as_str(), depth + 1));
            }
        }
    }
    lines.join("\n")
}
