//! Static, offline SVG tree pages. Pagination bounds image dimensions without
//! truncating nodes; parent numbers retain topology across page boundaries.
use super::*;
use crate::note_format::MindMapNode;

const ROWS_PER_PAGE: usize = 42;
const CHARACTERS_PER_LINE: usize = 44;

pub(super) fn render(
    map_id: &str,
    root_id: &str,
    nodes: &[MindMapNode],
) -> Result<Vec<(String, Vec<u8>)>, WorkspaceError> {
    let by_id: HashMap<&str, &MindMapNode> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut stack = vec![(root_id, 0usize, 0usize)];
    let mut lines = Vec::new();
    let mut number = 0;
    while let Some((id, depth, parent)) = stack.pop() {
        let node = by_id
            .get(id)
            .ok_or_else(|| StorageError::new("export", "导图节点引用不完整。"))?;
        number += 1;
        let caption = if parent == 0 {
            format!("#{number} · 根节点")
        } else {
            format!("#{number} · 上级 #{parent} · 层级 {depth}")
        };
        lines.push((depth, true, caption));
        for paragraph in node.text.split('\n') {
            let chars = paragraph.chars().collect::<Vec<_>>();
            if chars.is_empty() {
                lines.push((depth, false, String::new()));
            }
            for chunk in chars.chunks(CHARACTERS_PER_LINE) {
                lines.push((depth, false, chunk.iter().collect::<String>()));
            }
        }
        for child in node.children.iter().rev() {
            stack.push((child.as_str(), depth + 1, number));
        }
    }
    let mut pages = Vec::new();
    for (page, rows) in lines.chunks(ROWS_PER_PAGE).enumerate() {
        let height = 76 + rows.len() * 24;
        let mut svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1040\" height=\"{height}\" viewBox=\"0 0 1040 {height}\"><title>思维导图静态预览，第 {} 页</title><rect width=\"100%\" height=\"100%\" fill=\"#fff\"/><g font-family=\"sans-serif\" font-size=\"16\" fill=\"#203145\"><text x=\"24\" y=\"30\" font-weight=\"bold\">思维导图 · 第 {} 页</text>", page + 1, page + 1);
        for (row, (depth, header, value)) in rows.iter().enumerate() {
            let x = 32 + depth.min(&12) * 18;
            let y = 64 + row * 24;
            if *header {
                svg.push_str(&format!("<circle cx=\"{}\" cy=\"{}\" r=\"3\" fill=\"#4786b5\"/><text x=\"{x}\" y=\"{y}\" fill=\"#526678\" font-size=\"13\">{}</text>", x - 10, y - 5, xml(value)));
            } else {
                svg.push_str(&format!("<path d=\"M {} {} v 24\" stroke=\"#c7d6e3\"/><text x=\"{x}\" y=\"{y}\" xml:space=\"preserve\">{}</text>", x - 10, y - 19, xml(value)));
            }
        }
        svg.push_str("</g></svg>\n");
        pages.push((
            format!("assets/mind-map-{map_id}-{:04}.svg", page + 1),
            svg.into_bytes(),
        ));
    }
    Ok(pages)
}

fn xml(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            ch if ch.is_control() => vec!['\u{fffd}'],
            ch => vec![ch],
        })
        .collect()
}
