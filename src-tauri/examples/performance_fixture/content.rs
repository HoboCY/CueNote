//! Deterministic note packages for the Q51 corpus.
//!
//! Character counting rule (documented identically in the manifest): Unicode
//! code points over the note title, inline text, inline/block LaTeX source, code
//! text, table cell text and mind-map node text. IDs, link URLs, code language
//! markers, image assets and all other metadata are excluded, hard breaks count
//! zero. Generation fills each profile to the exact target with a deterministic
//! filler paragraph that contains no query token.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::Cursor;

use cuenote_lib::note_format::{
    AssetManifest, AssetManifestEntry, Block, Document, Inline, ListItem, Mark, MediaType,
    MindMapNode, NotePackage, Organization, Sections, TableRow, TaskListItem, Timestamps,
};

use crate::model::{self, Collection, NoteRef, Profile};
use crate::png;

pub struct ImageInfo {
    pub asset_id: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub ancillary_bytes: u64,
    pub sha256: String,
}

pub struct BuiltNote {
    pub bytes: Vec<u8>,
    pub text_chars: usize,
    pub images: Vec<ImageInfo>,
    pub map_nodes: usize,
}

pub fn build_note(
    note: &NoteRef,
    notebook_id: &str,
    serial: usize,
) -> Result<BuiltNote, Box<dyn Error>> {
    let (created_at, content_modified_at, organization_modified_at) =
        model::note_timestamps(serial);

    let mut package = NotePackage {
        document: Document {
            schema_version: 1,
            note_id: note.note_id.clone(),
            title: title(note),
            sections: Sections {
                cue: Vec::new(),
                content: Vec::new(),
                summary: Vec::new(),
            },
            organization: Organization {
                notebook_id: Some(notebook_id.to_owned()),
                pinned: false,
                trashed_at: None,
            },
            timestamps: Timestamps {
                created_at,
                content_modified_at,
                organization_modified_at,
            },
        },
        manifest: AssetManifest {
            schema_version: 1,
            assets: Vec::new(),
        },
        assets: BTreeMap::new(),
    };

    let image_count = note.profile.image_count();
    let per_image_target = if image_count == 0 {
        0
    } else {
        note.profile.asset_target_bytes() / image_count as u64
    };
    let mut images = Vec::with_capacity(image_count);
    let mut asset_ids = Vec::with_capacity(image_count);
    for index in 0..image_count {
        let asset_id = model::derived_id("q51-asset", &note.note_id, index as u64);
        let side = note.profile.image_side();
        let seed = png::pattern_seed(&asset_id);
        let base = png::patterned_png(side, seed)?;
        let (padded, ancillary_bytes) = png::pad_png_exact(base, per_image_target as usize)?;
        let sha256 = png::sha256_bytes(&padded);
        package.manifest.assets.push(AssetManifestEntry {
            id: asset_id.clone(),
            media_type: MediaType::Png,
            byte_length: padded.len() as u64,
            sha256: sha256.clone(),
            width: side,
            height: side,
        });
        package.assets.insert(asset_id.clone(), padded);
        images.push(ImageInfo {
            asset_id,
            width: side,
            height: side,
            bytes: per_image_target,
            ancillary_bytes,
            sha256,
        });
        asset_ids.push(images.last().expect("image just pushed").asset_id.clone());
    }

    let map_nodes = match note.profile {
        Profile::Text => Vec::new(),
        _ => build_map_nodes(note),
    };
    let map_node_count = map_nodes.len();
    let map_id = model::derived_id("q51-mindmap", &note.note_id, 0);
    let root_id = map_nodes
        .first()
        .map(|node| node.id.clone())
        .unwrap_or_default();

    package.document.sections.cue = cue_section(note);
    package.document.sections.summary = summary_section(note);
    package.document.sections.content =
        content_section(note, &asset_ids, &map_id, &root_id, map_nodes);
    let target = note.profile.expected_chars();
    let base_chars = text_chars_of(&package);
    let padding = target.checked_sub(base_chars).ok_or_else(|| {
        format!(
            "{} note {} base content already has {} chars (target {})",
            note.profile.as_str(),
            note.note_id,
            base_chars,
            target
        )
    })?;
    if padding > 0 {
        package.document.sections.content.push(Block::Paragraph {
            children: vec![text(&filler_text(padding))],
        });
    }
    let text_chars = text_chars_of(&package);
    if text_chars != target {
        return Err(format!(
            "{} note {} counted {} chars, expected {}",
            note.profile.as_str(),
            note.note_id,
            text_chars,
            target
        )
        .into());
    }

    let mut bytes = Cursor::new(Vec::new());
    cuenote_lib::note_format::write_package(&mut bytes, &package)?;
    let bytes = bytes.into_inner();
    Ok(BuiltNote {
        bytes,
        text_chars,
        images,
        map_nodes: map_node_count,
    })
}

fn label(note: &NoteRef) -> String {
    match note.profile {
        Profile::Text => format!("纯文字笔记 {:04}", note.index),
        Profile::Ordinary => format!("普通样本 {:02}", note.index),
        Profile::Stress => format!("压力样本 {:02}", note.index),
    }
}

fn title(note: &NoteRef) -> String {
    match note.collection {
        Collection::Library => format!("{} · {}", label(note), model::shared_token(note.profile)),
        Collection::Sample => format!(
            "独立{}性能样本",
            match note.profile {
                Profile::Ordinary => "普通",
                _ => "压力",
            }
        ),
    }
}

fn cue_section(note: &NoteRef) -> Vec<Block> {
    let keyword = match note.profile {
        Profile::Text => "关键词：文字笔记、索引、包含匹配",
        Profile::Ordinary => "关键词：普通样本、首屏资源、导图 50 节点",
        Profile::Stress => "关键词：压力样本、大包、导图 500 节点",
    };
    vec![
        Block::Heading3 {
            children: vec![text("线索区")],
        },
        Block::Paragraph {
            children: vec![marked(keyword, vec![Mark::Bold])],
        },
        bullet_block(&["固定参数能否复现同一份字节？", "三个分区是否都有内容？"]),
    ]
}

fn summary_section(note: &NoteRef) -> Vec<Block> {
    let _ = note;
    vec![
        Block::Heading3 {
            children: vec![text("总结区")],
        },
        Block::Paragraph {
            children: vec![marked(
                "本样本用于性能验收，不代表产品质量结论。",
                vec![Mark::Bold],
            )],
        },
        bullet_block(&[
            "复现步骤与校验值记录在 manifest.json。",
            "先核对哈希与码点数再测时延。",
        ]),
    ]
}

fn content_section(
    note: &NoteRef,
    asset_ids: &[String],
    map_id: &str,
    root_id: &str,
    map_nodes: Vec<MindMapNode>,
) -> Vec<Block> {
    if note.profile == Profile::Text {
        return text_only_content(note);
    }
    let heading = format!("内容区 · {}", label(note));
    let mut blocks = vec![
        Block::Heading1 {
            children: vec![text(&heading)],
        },
        Block::Paragraph {
            children: vec![
                marked("混合 Mixed ", vec![Mark::Bold]),
                marked("中文与 English", vec![Mark::Italic]),
                marked("删除线示例", vec![Mark::Strike]),
                text(" 行内代码 "),
                marked("inline_code()", vec![Mark::Code]),
                text(" 公式 "),
                Inline::MathInline {
                    latex: "\\frac{\\sqrt{x^2+y^2}}{n}".to_owned(),
                },
                text(" 参见 "),
                marked(
                    "KaTeX 支持列表",
                    vec![Mark::Link {
                        href: "https://katex.org/docs/supported".to_owned(),
                    }],
                ),
            ],
        },
        Block::Image {
            asset_id: asset_ids[0].clone(),
            display_width_px: note.profile.image_side(),
        },
        Block::MindMap {
            map_id: map_id.to_owned(),
            root_id: root_id.to_owned(),
            nodes: map_nodes,
        },
        Block::Paragraph {
            children: vec![text(&format!("本记录唯一检索标记 {}。", note.unique_token))],
        },
    ];
    if model::uses_astral_token(note) {
        blocks.push(Block::Paragraph {
            children: vec![text(&format!(
                "星体符号 {} 与罕用字符 𐐷 𐐸 用于验证码点计数。",
                model::ASTRAL_QUERY
            ))],
        });
    }
    blocks.extend([
        Block::Heading2 {
            children: vec![text("结构与标记")],
        },
        ordered_block(&["有序一", "有序二 mixed", "有序三"]),
        bullet_block(&["无序项目 mixed", "中文项目", "English item"]),
        task_block(&[(true, "已完成任务"), (false, "待处理任务")]),
        Block::Blockquote {
            blocks: vec![paragraph(
                "引用：康奈尔笔记法强调线索区与总结区的回顾作用。",
            )],
        },
        table_block(),
        code_block("javascript", CODE_JAVASCRIPT),
        code_block("typescript", CODE_TYPESCRIPT),
        code_block("python", CODE_PYTHON),
        code_block("rust", CODE_RUST),
        code_block("csharp", CODE_CSHARP),
        code_block("sql", CODE_SQL),
        code_block("json", CODE_JSON),
        code_block("brainfuck", CODE_UNKNOWN),
        Block::MathBlock {
            latex: MATH_ALIGNED.to_owned(),
        },
        Block::MathBlock {
            latex: MATH_MATRIX.to_owned(),
        },
    ]);
    for asset_id in &asset_ids[1..] {
        blocks.push(Block::Image {
            asset_id: asset_id.clone(),
            display_width_px: note.profile.image_side(),
        });
    }
    blocks
}

fn text_only_content(note: &NoteRef) -> Vec<Block> {
    let variants = [
        "样本记录用于验证搜索包含匹配与索引完整性。",
        "Mixed English sentences with CJK 混排 keep the corpus diverse.",
        "康奈尔笔记法用线索区提示回顾，用内容区承载主要记录。",
        "确定性生成保证跨机器跨运行得到相同字节与哈希。",
    ];
    let mut blocks = vec![
        Block::Heading2 {
            children: vec![text(&label(note))],
        },
        Block::Paragraph {
            children: vec![text(&format!(
                "唯一检索标记 {} 与混排 Mixed English 内容。",
                note.unique_token
            ))],
        },
        paragraph(variants[note.index % variants.len()]),
    ];
    if model::uses_astral_token(note) {
        blocks.push(Block::Paragraph {
            children: vec![text(&format!(
                "星体符号 {} 与罕用字符 𐐷 𐐸 用于验证码点计数。",
                model::ASTRAL_QUERY
            ))],
        });
    }
    blocks.extend([
        bullet_block(&["纯文字样本没有图片与导图。", "段落与列表仍然参与检索。"]),
        Block::Heading3 {
            children: vec![text("小节")],
        },
        paragraph(variants[(note.index + 2) % variants.len()]),
    ]);
    blocks
}

pub fn build_map_nodes(note: &NoteRef) -> Vec<MindMapNode> {
    let count = note.profile.map_nodes();
    let shape = model::map_shape(count);
    let mut nodes = Vec::with_capacity(count);
    let root_id = model::derived_id("q51-map-node", &note.note_id, 0);
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        ids.push(model::derived_id(
            "q51-map-node",
            &note.note_id,
            index as u64,
        ));
    }
    let mut next = 1usize;
    let mut branch_ids = Vec::with_capacity(shape.root_children);
    let mut branches = Vec::with_capacity(shape.root_children);
    for branch in 0..shape.root_children {
        let branch_id = ids[next].clone();
        next += 1;
        let leaf_count = shape.base_leaves + usize::from(branch < shape.extra_leaves);
        let mut children = Vec::with_capacity(leaf_count);
        for leaf in 0..leaf_count {
            let leaf_id = ids[next].clone();
            next += 1;
            children.push(leaf_id.clone());
            branches.push(MindMapNode {
                id: leaf_id,
                text: format!("叶节点 {branch:02}-{leaf:02} 混排 leaf"),
                children: Vec::new(),
            });
        }
        branch_ids.push(branch_id.clone());
        nodes.push(MindMapNode {
            id: branch_id,
            text: format!("分支 {branch:02}"),
            children,
        });
    }
    debug_assert_eq!(next, count);
    let mut ordered = Vec::with_capacity(count);
    ordered.push(MindMapNode {
        id: root_id,
        text: format!("导图根节点 {}", label(note)),
        children: branch_ids,
    });
    ordered.append(&mut nodes);
    ordered.extend(branches);
    debug_assert_eq!(ordered.len(), count);
    ordered
}

// ---------------------------------------------------------------------------
// Character counting
// ---------------------------------------------------------------------------

pub fn text_chars_of(package: &NotePackage) -> usize {
    let mut total = package.document.title.chars().count();
    total += count_blocks(&package.document.sections.cue);
    total += count_blocks(&package.document.sections.content);
    total += count_blocks(&package.document.sections.summary);
    total
}

fn count_blocks(blocks: &[Block]) -> usize {
    blocks.iter().map(count_block).sum()
}

fn count_block(block: &Block) -> usize {
    match block {
        Block::Paragraph { children }
        | Block::Heading1 { children }
        | Block::Heading2 { children }
        | Block::Heading3 { children } => count_inlines(children),
        Block::OrderedList { items, .. } => items.iter().map(item_chars).sum(),
        Block::BulletList { items } => items.iter().map(item_chars).sum(),
        Block::TaskList { items } => items
            .iter()
            .map(|item: &TaskListItem| count_blocks(&item.blocks))
            .sum(),
        Block::Blockquote { blocks } => count_blocks(blocks),
        Block::CodeBlock { code, .. } => code.chars().count(),
        Block::HorizontalRule => 0,
        Block::Table { rows } => rows
            .iter()
            .map(|row| match row {
                TableRow::Header { cells } | TableRow::Body { cells } => {
                    cells.iter().map(|cell| count_inlines(cell)).sum::<usize>()
                }
            })
            .sum(),
        Block::MathBlock { latex } => latex.chars().count(),
        Block::Image { .. } => 0,
        Block::MindMap { nodes, .. } => nodes.iter().map(|node| node.text.chars().count()).sum(),
    }
}

fn item_chars(item: &ListItem) -> usize {
    count_blocks(&item.blocks)
}

fn count_inlines(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text { text, .. } => text.chars().count(),
            Inline::HardBreak => 0,
            Inline::MathInline { latex } => latex.chars().count(),
        })
        .sum()
}

/// Deterministic filler that contains no known query token of any profile.
pub fn filler_text(count: usize) -> String {
    const CHUNK: &str = "关于确定性性能样本的填充段落用于将正文补齐到规定码点数。The quick brown fox jumps over the lazy dog while this filler keeps the corpus reproducible across machines and repeated runs. 山海经注记与星图测绘记录显示规范样本能够跨机器复现相同的字节与哈希。";
    let mut out = String::with_capacity(count * 3);
    while out.chars().count() < count {
        out.push_str(CHUNK);
    }
    out.chars().take(count).collect()
}

// ---------------------------------------------------------------------------
// Search projection (mirrors workspace/search.rs matching semantics)
// ---------------------------------------------------------------------------

/// Projected searchable segments: title, then every cue/content/summary segment
/// in the same granularity the host index uses. Matching is a per-segment
/// `contains` check on ASCII-lowercased text.
pub fn search_segments(package: &NotePackage) -> Vec<String> {
    let mut segments = Vec::new();
    push_segment(&mut segments, &package.document.title);
    for blocks in [
        package.document.sections.cue.as_slice(),
        package.document.sections.content.as_slice(),
        package.document.sections.summary.as_slice(),
    ] {
        collect_blocks(blocks, &mut segments);
    }
    segments
}

fn push_segment(segments: &mut Vec<String>, value: &str) {
    if !value.is_empty() {
        segments.push(value.to_owned());
    }
}

fn collect_blocks(blocks: &[Block], segments: &mut Vec<String>) {
    for block in blocks {
        collect_block(block, segments);
    }
}

fn collect_block(block: &Block, segments: &mut Vec<String>) {
    match block {
        Block::Paragraph { children }
        | Block::Heading1 { children }
        | Block::Heading2 { children }
        | Block::Heading3 { children } => push_segment(segments, &inline_text(children)),
        Block::OrderedList { items, .. } | Block::BulletList { items } => {
            for item in items {
                collect_blocks(&item.blocks, segments);
            }
        }
        Block::TaskList { items } => {
            for item in items {
                collect_blocks(&item.blocks, segments);
            }
        }
        Block::Blockquote { blocks } => collect_blocks(blocks, segments),
        Block::CodeBlock { code, .. } => push_segment(segments, code),
        Block::HorizontalRule | Block::Image { .. } => {}
        Block::Table { rows } => {
            for row in rows {
                let cells = match row {
                    TableRow::Header { cells } | TableRow::Body { cells } => cells,
                };
                for cell in cells {
                    push_segment(segments, &inline_text(cell));
                }
            }
        }
        Block::MathBlock { latex } => push_segment(segments, latex),
        Block::MindMap { root_id, nodes, .. } => {
            let by_id: std::collections::HashMap<&str, &MindMapNode> =
                nodes.iter().map(|node| (node.id.as_str(), node)).collect();
            let mut stack = vec![root_id.as_str()];
            while let Some(node_id) = stack.pop() {
                let Some(node) = by_id.get(node_id) else {
                    continue;
                };
                push_segment(segments, &node.text);
                for child in node.children.iter().rev() {
                    stack.push(child.as_str());
                }
            }
        }
    }
}

fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::HardBreak => out.push('\n'),
            Inline::MathInline { latex } => out.push_str(latex),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Block helpers
// ---------------------------------------------------------------------------

fn text(value: &str) -> Inline {
    Inline::Text {
        text: value.to_owned(),
        marks: Vec::new(),
    }
}

fn marked(value: &str, marks: Vec<Mark>) -> Inline {
    Inline::Text {
        text: value.to_owned(),
        marks,
    }
}

fn paragraph(value: &str) -> Block {
    Block::Paragraph {
        children: vec![text(value)],
    }
}

fn bullet_block(values: &[&str]) -> Block {
    Block::BulletList {
        items: values
            .iter()
            .map(|value| ListItem {
                blocks: vec![paragraph(value)],
            })
            .collect(),
    }
}

fn ordered_block(values: &[&str]) -> Block {
    Block::OrderedList {
        start: 1,
        items: values
            .iter()
            .map(|value| ListItem {
                blocks: vec![paragraph(value)],
            })
            .collect(),
    }
}

fn task_block(values: &[(bool, &str)]) -> Block {
    Block::TaskList {
        items: values
            .iter()
            .map(|(checked, value)| TaskListItem {
                checked: *checked,
                blocks: vec![paragraph(value)],
            })
            .collect(),
    }
}

fn table_block() -> Block {
    let cell = |value: &str, marks: Vec<Mark>| vec![marked(value, marks)];
    Block::Table {
        rows: vec![
            TableRow::Header {
                cells: vec![
                    cell("列 A", vec![Mark::Bold]),
                    cell("列 B", vec![Mark::Bold]),
                    cell("列 C", vec![Mark::Bold]),
                ],
            },
            TableRow::Body {
                cells: vec![
                    cell("甲 mixed", vec![]),
                    cell("乙 English", vec![Mark::Italic]),
                    cell("丙 中文", vec![]),
                ],
            },
            TableRow::Body {
                cells: vec![
                    cell("42", vec![]),
                    cell("$100", vec![]),
                    cell("<b>HTML</b>", vec![]),
                ],
            },
        ],
    }
}

fn code_block(language: &str, code: &str) -> Block {
    Block::CodeBlock {
        language: Some(language.to_owned()),
        code: code.to_owned(),
    }
}

const CODE_JAVASCRIPT: &str = "const 样本 = {\n  名称: \"混合 Mixed\",\n  模板: `值 ${1 + 1}`,\n}; // 中文注释，空行与缩进保留\n\nmodule.exports = 样本;\n";
const CODE_TYPESCRIPT: &str = "interface 行 { 名称: string; 数量: number; }\nconst 行: 行 = { 名称: \"甲\", 数量: 2 };\ntype 键 = keyof 行;\n";
const CODE_PYTHON: &str = "def 求和(值: list[int]) -> int:\n\t\"\"\"返回总和 $x$ 与 <b>HTML</b> 原样文本\"\"\"\n\treturn sum(值)  # 中文注释\n";
const CODE_RUST: &str =
    "fn 合计(值: &[i32]) -> i32 {\n    let 总和 = 值.iter().sum();\n\n    总和\n}\n";
const CODE_CSHARP: &str = "public record 行(string 名称, int 数量);\n// <div class=\"note\">HTML 保持代码文本</div>\nvar 行 = new 行(\"甲\", 1);\n";
const CODE_SQL: &str =
    "SELECT 名称, COUNT(*) AS 数量\nFROM 笔记\nWHERE 状态 = '活跃' -- 中文注释\nGROUP BY 名称;\n";
const CODE_JSON: &str =
    "{\n  \"名称\": \"样本\",\n  \"金额\": \"$100\",\n  \"标签\": [\"a\", \"b\"]\n}\n";
const CODE_UNKNOWN: &str = "+[----->+++<]>+.---[->++<] 中文注释与 <p>HTML</p> 保持原样\n";
const MATH_ALIGNED: &str = "\\begin{aligned}\n\\sum_{i=1}^{n} i &= \\frac{n(n+1)}{2} \\\\\n\\int_0^\\infty e^{-x^2}\\,dx &= \\frac{\\sqrt{\\pi}}{2}\n\\end{aligned}";
const MATH_MATRIX: &str =
    "\\begin{bmatrix}\n\\alpha & \\beta \\\\\n\\gamma & \\delta\n\\end{bmatrix}";
