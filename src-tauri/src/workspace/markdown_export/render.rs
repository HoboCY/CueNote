use super::*;

pub(super) fn document(
    package: &NotePackage,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, WorkspaceError> {
    let mut renderer = Renderer {
        package,
        files: BTreeMap::new(),
        source_number: 0,
        table_sources: Vec::new(),
    };
    let markdown = format!("# {}\n\n## 线索\n\n{}\n\n## 内容\n\n{}\n\n## 总结\n\n{}\n\n---\n\n数学公式的显示取决于阅读器对 LaTeX 的支持。源码附件保留原始字节；请连同 assets 文件夹一起移动。\n", text(&package.document.title), renderer.blocks(&package.document.sections.cue)?, renderer.blocks(&package.document.sections.content)?, renderer.blocks(&package.document.sections.summary)?);
    renderer
        .files
        .insert(PathBuf::from("note.md"), markdown.into_bytes());
    Ok(renderer.files)
}

struct Renderer<'a> {
    package: &'a NotePackage,
    files: BTreeMap<PathBuf, Vec<u8>>,
    source_number: usize,
    table_sources: Vec<String>,
}

impl Renderer<'_> {
    fn blocks(&mut self, items: &[Block]) -> Result<String, WorkspaceError> {
        items
            .iter()
            .map(|block| self.block(block))
            .collect::<Result<Vec<_>, _>>()
            .map(|items| items.join("\n\n"))
    }

    fn block(&mut self, block: &Block) -> Result<String, WorkspaceError> {
        match block {
            Block::Paragraph { children } => self.inline_block("", children),
            Block::Heading1 { children } => self.inline_block("# ", children),
            Block::Heading2 { children } => self.inline_block("## ", children),
            Block::Heading3 { children } => self.inline_block("### ", children),
            Block::HorizontalRule => Ok("---".into()),
            Block::Blockquote { blocks } => Ok(self
                .blocks(blocks)?
                .split('\n')
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n")),
            Block::BulletList { items } => items
                .iter()
                .map(|item| self.list_item("- ", &item.blocks))
                .collect::<Result<Vec<_>, _>>()
                .map(|items| items.join("\n")),
            Block::OrderedList { start, items } => items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    self.list_item(
                        &format!("{}. ", u64::from(*start) + index as u64),
                        &item.blocks,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|items| items.join("\n")),
            Block::TaskList { items } => items
                .iter()
                .map(|item| {
                    self.list_item(if item.checked { "- [x] " } else { "- [ ] " }, &item.blocks)
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|items| items.join("\n")),
            Block::Table { rows } => {
                let mut table = Vec::new();
                for (index, row) in rows.iter().enumerate() {
                    let (TableRow::Header { cells } | TableRow::Body { cells }) = row;
                    table.push(format!(
                        "| {} |",
                        cells
                            .iter()
                            .map(|cell| self.inlines(cell, true))
                            .collect::<Result<Vec<_>, _>>()?
                            .join(" | ")
                    ));
                    if index == 0 {
                        table.push(format!("| {} |", vec!["---"; cells.len()].join(" | ")));
                    }
                }
                let mut result = table.join("\n");
                for source in self.table_sources.drain(..) {
                    result.push_str(&format!("\n\n{source}"));
                }
                Ok(result)
            }
            Block::CodeBlock { language, code } => {
                let source = self.source("code", "txt", code);
                let language = language.as_deref().unwrap_or_default();
                // An arbitrary label cannot inject another block into the info string.
                let safe_language = language
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "_+-#.".contains(ch));
                let info = if safe_language { language } else { "" };
                let label = if safe_language {
                    String::new()
                } else {
                    format!("\n\n语言标记：{}", text(language))
                };
                Ok(format!(
                    "{}\n\n[代码原文]({source}){label}",
                    fenced(code, info)
                ))
            }
            Block::MathBlock { latex } => {
                let expression = format!("$$\n{latex}\n$$");
                if ambiguous_math(latex, true) {
                    let source = self.source("formula", "tex", latex);
                    Ok(format!(
                        "公式块原文（分隔符不能安全显示）：\n\n{}\n\n[公式原文]({source})",
                        fenced(&expression, "tex")
                    ))
                } else {
                    Ok(expression)
                }
            }
            Block::Image { asset_id, .. } => {
                let asset = self
                    .package
                    .manifest
                    .assets
                    .iter()
                    .find(|asset| asset.id == *asset_id)
                    .ok_or_else(|| StorageError::new("missingAsset", "导出图片的清单缺失。"))?;
                let bytes = self
                    .package
                    .assets
                    .get(asset_id)
                    .ok_or_else(|| StorageError::new("missingAsset", "导出图片文件缺失。"))?;
                let extension = match asset.media_type {
                    crate::note_format::MediaType::Png => "png",
                    crate::note_format::MediaType::Jpeg => "jpg",
                };
                let path = format!("assets/image-{asset_id}.{extension}");
                self.files.insert(PathBuf::from(&path), bytes.clone());
                Ok(format!("![图片]({path})"))
            }
            Block::MindMap {
                map_id,
                root_id,
                nodes,
            } => {
                let previews = mind_map_preview::render(map_id, root_id, nodes)?;
                let mut rendered = "思维导图：静态预览，不能在 Markdown 中继续编辑导图。节点编号与上级编号用于跨页查看层级。".to_owned();
                for (path, bytes) in previews {
                    rendered.push_str(&format!("\n\n![思维导图静态预览]({path})"));
                    self.files.insert(PathBuf::from(path), bytes);
                }
                let path = format!("assets/mind-map-{map_id}.json");
                let source = serde_json::to_vec_pretty(block).map_err(|error| {
                    StorageError::new("export", format!("无法导出导图节点原文：{error}"))
                })?;
                self.files.insert(PathBuf::from(&path), source);
                rendered.push_str(&format!("\n\n[导图节点原文]({path})"));
                Ok(rendered)
            }
        }
    }

    fn source(&mut self, kind: &str, extension: &str, value: &str) -> String {
        self.source_number += 1;
        let path = format!("assets/{kind}-{}.{extension}", self.source_number);
        self.files
            .insert(PathBuf::from(&path), value.as_bytes().to_vec());
        path
    }

    fn inline_block(
        &mut self,
        prefix: &str,
        children: &[Inline],
    ) -> Result<String, WorkspaceError> {
        let mut result = format!("{prefix}{}", self.inlines(children, false)?);
        for source in self.table_sources.drain(..) {
            result.push_str(&format!("\n\n{source}"));
        }
        Ok(result)
    }

    fn list_item(&mut self, marker: &str, children: &[Block]) -> Result<String, WorkspaceError> {
        let content = self.blocks(children)?;
        let mut lines = content.split('\n');
        let mut item = format!("{marker}{}", lines.next().unwrap_or_default());
        let indent = " ".repeat(if marker.starts_with("- [") {
            2
        } else {
            marker.len()
        });
        for line in lines {
            item.push_str(&format!("\n{indent}{line}"));
        }
        Ok(item)
    }

    fn inlines(&mut self, items: &[Inline], table: bool) -> Result<String, WorkspaceError> {
        let mut result = String::new();
        for inline in items {
            let rendered = match inline {
                Inline::HardBreak => "<br />".into(),
                Inline::MathInline { latex } => {
                    if table && latex.contains('|') || ambiguous_math(latex, false) {
                        let source = self.source("formula", "tex", latex);
                        let label = format!("公式 {}", self.source_number);
                        let expression = format!("${latex}$");
                        let display = if ambiguous_math(latex, false) {
                            fenced(&expression, "tex")
                        } else {
                            expression
                        };
                        self.table_sources.push(format!(
                            "{label}（行内公式原文）：\n\n{display}\n\n[公式原文]({source})"
                        ));
                        format!("[{label}（见后文）]({source})")
                    } else {
                        format!("${latex}$")
                    }
                }
                Inline::Text { text: value, marks } => {
                    let mut rendered = if marks.contains(&Mark::Code) {
                        if value.contains(['\r', '\n', '\0']) || table && value.contains("\\|") {
                            let source = self.source("inline-code", "txt", value);
                            let label = format!("代码 {}", self.source_number);
                            self.table_sources.push(format!(
                                "{label}（行内代码原文）：\n\n{}\n\n[代码原文]({source})",
                                fenced(value, "")
                            ));
                            format!("[{label}（见后文）]({source})")
                        } else {
                            code_span(value, table)
                        }
                    } else {
                        text(value)
                    };
                    for mark in marks {
                        let (delimiter, tag) = match mark {
                            Mark::Code => continue,
                            Mark::Bold => ("**", "strong"),
                            Mark::Italic => ("*", "em"),
                            Mark::Strike => ("~~", "del"),
                            Mark::Link { href } => {
                                let href = href
                                    .replace('&', "&amp;")
                                    .replace('<', "%3C")
                                    .replace('>', "%3E")
                                    .replace('\\', "%5C")
                                    .replace('|', "%7C");
                                rendered = format!("[{rendered}](<{href}>)");
                                continue;
                            }
                        };
                        if !value.starts_with(char::is_alphanumeric)
                            || !value.ends_with(char::is_alphanumeric)
                        {
                            rendered = format!("<{tag}>{rendered}</{tag}>");
                        } else {
                            rendered = format!("{delimiter}{rendered}{delimiter}");
                        }
                    }
                    rendered
                }
            };
            if (result.ends_with('*') && rendered.starts_with('*'))
                || (result.ends_with('~') && rendered.starts_with('~'))
                || (result.ends_with('$') && rendered.starts_with('$'))
                || (result.ends_with('`') && rendered.starts_with('`'))
            {
                result.push_str("<!-- -->");
            }
            result.push_str(&rendered);
        }
        Ok(result)
    }
}

fn code_span(value: &str, table: bool) -> String {
    if value.is_empty() {
        return "<code></code>".into();
    }
    let ticks = "`".repeat(longest_run(value, '`') + 1);
    let padding = value.starts_with('`')
        || value.ends_with('`')
        || (value.starts_with(' ') && value.ends_with(' ') && value.chars().any(|ch| ch != ' '));
    let value = if table {
        value.replace('|', "\\|")
    } else {
        value.to_owned()
    };
    if padding {
        format!("{ticks} {value} {ticks}")
    } else {
        format!("{ticks}{value}{ticks}")
    }
}

fn fenced(value: &str, language: &str) -> String {
    let ticks = "`".repeat(3.max(longest_run(value, '`') + 1));
    format!(
        "{ticks}{language}\n{value}{}{ticks}",
        if value.ends_with('\n') { "" } else { "\n" }
    )
}

fn longest_run(value: &str, needle: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in value.chars() {
        current = if ch == needle { current + 1 } else { 0 };
        longest = longest.max(current);
    }
    longest
}

fn ambiguous_math(value: &str, block: bool) -> bool {
    // Single-dollar readers differ on whether backslashes escape a dollar
    // inside math. Keep such expressions as visible exact source, not a
    // prematurely closed expression with changed mathematical meaning.
    if !block && value.contains('$') {
        return true;
    }
    if value.trim().is_empty() || value.contains('\0') || !block && value.contains(['\r', '\n']) {
        return true;
    }
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '$' && backslashes % 2 == 0 {
            return true;
        }
        backslashes = if ch == '\\' { backslashes + 1 } else { 0 };
    }
    !block && backslashes % 2 != 0
}

fn text(value: &str) -> String {
    let mut escaped = String::new();
    let mut leading = true;
    for ch in value.chars() {
        match ch {
            '\r' => escaped.push_str("&#13;"),
            '\n' => {
                escaped.push_str("<br />");
                leading = true;
            }
            '\t' => escaped.push_str("&#9;"),
            ' ' if leading => escaped.push_str("&#32;"),
            ch if ch.is_ascii_punctuation() => {
                escaped.push('\\');
                escaped.push(ch);
                leading = false;
            }
            ch => {
                escaped.push(ch);
                leading = false;
            }
        }
    }
    escaped
}
