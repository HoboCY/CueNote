use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::note_format::{Block, Inline, NotePackage, TableRow};

use super::NoteSummary;

pub(crate) const MAX_QUERY_CHARS: usize = 1_024;
pub(crate) const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
const RESULT_PAGE_SIZE: usize = 100;
const SNIPPET_CONTEXT_CHARS: usize = 48;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchStatus {
    Ready,
    Building,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchSection {
    Title,
    Cue,
    Content,
    Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchSnippet {
    pub before: String,
    pub matched: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchHit {
    pub note: NoteSummary,
    pub section: SearchSection,
    pub snippet: SearchSnippet,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchResults {
    pub library_path: String,
    pub query: String,
    pub notebook_id: Option<String>,
    pub status: SearchStatus,
    pub hits: Vec<SearchHit>,
    pub total: usize,
    pub offset: usize,
    pub indexed_notes: usize,
    pub incomplete: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Default)]
pub(crate) struct SearchIndex {
    entries: Vec<IndexedNote>,
    indexed_bytes: usize,
    incomplete: bool,
}

#[derive(Clone)]
struct IndexedNote {
    note: NoteSummary,
    fields: Vec<IndexedField>,
}

#[derive(Clone)]
struct IndexedField {
    section: SearchSection,
    text: String,
    ascii_folded: String,
}

impl SearchIndex {
    pub(crate) fn add_note(&mut self, note: NoteSummary, package: &NotePackage) -> bool {
        let mut remaining = (MAX_INDEX_BYTES - self.indexed_bytes) / 2;
        let mut fields = Vec::new();
        let mut title = BoundedSegments::new(SearchSection::Title, remaining);
        title.push_segment(|text| text.push_str(&package.document.title));
        if title.failed {
            self.incomplete = true;
            return false;
        }
        remaining -= title.used_bytes;
        fields.extend(title.into_fields());
        for (section, blocks) in [
            (SearchSection::Cue, package.document.sections.cue.as_slice()),
            (
                SearchSection::Content,
                package.document.sections.content.as_slice(),
            ),
            (
                SearchSection::Summary,
                package.document.sections.summary.as_slice(),
            ),
        ] {
            let mut segments = BoundedSegments::new(section, remaining);
            project_blocks(blocks, &mut segments);
            if segments.failed {
                self.incomplete = true;
                return false;
            }
            remaining -= segments.used_bytes;
            fields.extend(segments.into_fields());
        }
        let note_bytes = fields.iter().map(|(_, text)| text.len()).sum::<usize>() * 2;

        let fields = fields
            .into_iter()
            .map(|(section, text)| IndexedField {
                ascii_folded: text.to_ascii_lowercase(),
                section,
                text,
            })
            .collect();
        self.indexed_bytes += note_bytes;
        self.entries.push(IndexedNote { note, fields });
        true
    }

    pub(crate) fn indexed_notes(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_incomplete(&self) -> bool {
        self.incomplete
    }

    pub(crate) fn search(
        &self,
        query: &str,
        notebook_id: Option<&str>,
        offset: usize,
    ) -> (Vec<SearchHit>, usize) {
        let folded_query = query.to_ascii_lowercase();
        let mut matches = Vec::<(&IndexedNote, &IndexedField, usize)>::new();
        for entry in &self.entries {
            if notebook_id.is_some_and(|filter| entry.note.notebook_id.as_deref() != Some(filter)) {
                continue;
            }
            if let Some((field, position)) = entry.fields.iter().find_map(|field| {
                field
                    .ascii_folded
                    .find(&folded_query)
                    .map(|position| (field, position))
            }) {
                matches.push((entry, field, position));
            }
        }
        matches.sort_by(|(left, _, _), (right, _, _)| {
            right
                .note
                .pinned
                .cmp(&left.note.pinned)
                .then_with(|| right.note.modified_at.cmp(&left.note.modified_at))
                .then_with(|| left.note.note_id.cmp(&right.note.note_id))
        });
        let total = matches.len();
        let hits = matches
            .into_iter()
            .skip(offset)
            .take(RESULT_PAGE_SIZE)
            .map(|(entry, field, position)| SearchHit {
                note: entry.note.clone(),
                section: field.section,
                snippet: make_snippet(&field.text, position, folded_query.len()),
            })
            .collect();
        (hits, total)
    }
}

struct BoundedText {
    text: String,
    max_bytes: usize,
    failed: bool,
}

impl BoundedText {
    fn new(max_bytes: usize) -> Self {
        Self {
            text: String::new(),
            max_bytes,
            failed: false,
        }
    }

    fn push_str(&mut self, value: &str) {
        if self.text.len().saturating_add(value.len()) > self.max_bytes {
            self.failed = true;
            return;
        }
        self.text.push_str(value);
    }

    fn push(&mut self, value: char) {
        if self.text.len().saturating_add(value.len_utf8()) > self.max_bytes {
            self.failed = true;
            return;
        }
        self.text.push(value);
    }
}

struct BoundedSegments {
    section: SearchSection,
    segments: Vec<String>,
    max_bytes: usize,
    used_bytes: usize,
    failed: bool,
}

impl BoundedSegments {
    fn new(section: SearchSection, max_bytes: usize) -> Self {
        Self {
            section,
            segments: Vec::new(),
            max_bytes,
            used_bytes: 0,
            failed: false,
        }
    }

    fn push_segment(&mut self, project: impl FnOnce(&mut BoundedText)) {
        if self.failed {
            return;
        }
        let remaining = self.max_bytes - self.used_bytes;
        let mut text = BoundedText::new(remaining);
        project(&mut text);
        if text.failed {
            self.failed = true;
            return;
        }
        if !text.text.is_empty() {
            self.used_bytes += text.text.len();
            self.segments.push(text.text);
        }
    }

    fn into_fields(self) -> impl Iterator<Item = (SearchSection, String)> {
        let section = self.section;
        self.segments.into_iter().map(move |text| (section, text))
    }
}

fn project_blocks(blocks: &[Block], output: &mut BoundedSegments) {
    for block in blocks {
        project_block(block, output);
    }
}

fn project_block(block: &Block, output: &mut BoundedSegments) {
    match block {
        Block::Paragraph { children }
        | Block::Heading1 { children }
        | Block::Heading2 { children }
        | Block::Heading3 { children } => {
            output.push_segment(|text| project_inlines(children, text));
        }
        Block::OrderedList { items, .. } => {
            project_list_items(items.iter().map(|item| &item.blocks), output)
        }
        Block::BulletList { items } => {
            project_list_items(items.iter().map(|item| &item.blocks), output)
        }
        Block::TaskList { items } => {
            project_list_items(items.iter().map(|item| &item.blocks), output)
        }
        Block::Blockquote { blocks } => project_blocks(blocks, output),
        Block::CodeBlock { code, .. } => {
            output.push_segment(|text| text.push_str(code));
        }
        Block::HorizontalRule | Block::Image { .. } => {}
        Block::Table { rows } => {
            for row in rows {
                let cells = match row {
                    TableRow::Header { cells } | TableRow::Body { cells } => cells,
                };
                for cell in cells {
                    output.push_segment(|text| project_inlines(cell, text));
                }
            }
        }
        Block::MathBlock { latex } => {
            output.push_segment(|text| text.push_str(latex));
        }
        Block::MindMap { root_id, nodes, .. } => project_mind_map(root_id, nodes, output),
    }
}

fn project_list_items<'a>(
    items: impl Iterator<Item = &'a Vec<Block>>,
    output: &mut BoundedSegments,
) {
    for blocks in items {
        project_blocks(blocks, output);
    }
}

fn project_inlines(inlines: &[Inline], output: &mut BoundedText) {
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => output.push_str(text),
            Inline::HardBreak => output.push('\n'),
            Inline::MathInline { latex } => output.push_str(latex),
        }
    }
}

fn project_mind_map(
    root_id: &str,
    nodes: &[crate::note_format::MindMapNode],
    output: &mut BoundedSegments,
) {
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut stack = vec![root_id];
    while let Some(node_id) = stack.pop() {
        let Some(node) = by_id.get(node_id) else {
            continue;
        };
        output.push_segment(|text| text.push_str(&node.text));
        for child in node.children.iter().rev() {
            stack.push(child);
        }
    }
}

fn make_snippet(source: &str, match_start: usize, match_len: usize) -> SearchSnippet {
    let match_end = match_start + match_len;
    let before_start = source[..match_start]
        .char_indices()
        .rev()
        .nth(SNIPPET_CONTEXT_CHARS.saturating_sub(1))
        .map(|(index, _)| index)
        .unwrap_or(0);
    let after_end = source[match_end..]
        .char_indices()
        .nth(SNIPPET_CONTEXT_CHARS)
        .map(|(index, _)| match_end + index)
        .unwrap_or(source.len());
    SearchSnippet {
        before: source[before_start..match_start].to_owned(),
        matched: source[match_start..match_end].to_owned(),
        after: source[match_end..after_end].to_owned(),
    }
}
