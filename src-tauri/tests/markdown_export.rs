use cuenote_lib::note_format::{Block, Inline, ListItem, Mark, TableRow, TaskListItem};
use cuenote_lib::workspace::Workspace;
use std::fs;
use std::path::Path;

fn setup() -> (tempfile::TempDir, Workspace) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata");
    fs::create_dir_all(&parent).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("markdown-export-")
        .tempdir_in(parent)
        .unwrap();
    let workspace = Workspace::new(
        temp.path().join("settings"),
        temp.path().join("recovery"),
        temp.path().join("library"),
    );
    (temp, workspace)
}

fn paragraph(text: &str) -> Block {
    Block::Paragraph {
        children: vec![Inline::Text {
            text: text.into(),
            marks: vec![],
        }],
    }
}

#[test]
fn adjacent_inline_code_keeps_two_separate_code_spans() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let mut content = session.content;
    content.sections.content = vec![Block::Paragraph {
        children: ["first", "second"]
            .into_iter()
            .map(|text| Inline::Text {
                text: text.into(),
                marks: vec![Mark::Code],
            })
            .collect(),
    }];
    let target = temp.path().join("adjacent-code");
    workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap();
    let markdown = fs::read_to_string(target.join("note.md")).unwrap();
    // An HTML comment separates delimiter runs without adding visible whitespace.
    assert!(markdown.contains("`first`<!-- -->`second`"), "{markdown}");
}

#[test]
fn malformed_cyclic_map_is_rejected_before_creating_an_export_directory() {
    use cuenote_lib::note_format::MindMapNode;
    use uuid::Uuid;
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let mut content = session.content.clone();
    let root_id = Uuid::new_v4().to_string();
    content.sections.content = vec![Block::MindMap {
        map_id: Uuid::new_v4().to_string(),
        root_id: root_id.clone(),
        nodes: vec![MindMapNode {
            id: root_id.clone(),
            text: "cycle".into(),
            children: vec![root_id],
        }],
    }];
    let target = temp.path().join("invalid-map");
    let error = workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap_err();
    assert_eq!(error.kind, "format");
    assert!(!target.exists());
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().content,
        session.content
    );
}

#[test]
fn export_current_draft_in_cornell_order_without_saving_or_switching_native_session() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    workspace
        .save_note_content(&session.session_id, session.content.clone())
        .unwrap();
    let source = temp
        .path()
        .join("library/notes")
        .join(format!("{}.cuenote", session.note_id));
    let before = fs::read(&source).unwrap();
    let mut content = session.content.clone();
    content.title = "导出草稿".into();
    content.sections.cue = vec![paragraph("问题")];
    content.sections.content = vec![paragraph("答案")];
    content.sections.summary = vec![paragraph("结论")];
    let target = temp.path().join("导出目录");
    let receipt = workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap();
    let markdown = fs::read_to_string(target.join("note.md")).unwrap();
    assert!(markdown
        .starts_with("# 导出草稿\n\n## 线索\n\n问题\n\n## 内容\n\n答案\n\n## 总结\n\n结论\n"));
    assert_eq!(receipt.attachment_count, 0);
    assert_eq!(fs::read(source).unwrap(), before);
    let after = workspace.snapshot().unwrap().session.unwrap();
    assert_eq!(after.session_id, session.session_id);
    assert_eq!(after.content, session.content);
}

#[test]
fn supported_rich_format_and_literal_punctuation_survive_markdown_export() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let mut content = session.content.clone();
    content.sections.content = vec![
        Block::Heading2 {
            children: vec![Inline::Text {
                text: "二级标题".into(),
                marks: vec![],
            }],
        },
        Block::Paragraph {
            children: vec![
                Inline::Text {
                    text: "粗体".into(),
                    marks: vec![Mark::Bold],
                },
                Inline::Text {
                    text: "斜体".into(),
                    marks: vec![Mark::Italic],
                },
                Inline::Text {
                    text: "删除".into(),
                    marks: vec![Mark::Strike],
                },
                Inline::Text {
                    text: "链接".into(),
                    marks: vec![Mark::Link {
                        href: "https://example.com/a_(b)".into(),
                    }],
                },
                Inline::HardBreak,
                Inline::Text {
                    text: "[字面] <script> $5 &x;".into(),
                    marks: vec![],
                },
            ],
        },
        Block::Blockquote {
            blocks: vec![
                paragraph("引用"),
                Block::BulletList {
                    items: vec![ListItem {
                        blocks: vec![paragraph("嵌套")],
                    }],
                },
            ],
        },
        Block::OrderedList {
            start: 3,
            items: vec![ListItem {
                blocks: vec![paragraph("第三项"), paragraph("同项第二段")],
            }],
        },
        Block::TaskList {
            items: vec![TaskListItem {
                checked: true,
                blocks: vec![paragraph("已完成")],
            }],
        },
        Block::HorizontalRule,
        Block::Table {
            rows: vec![
                TableRow::Header {
                    cells: vec![vec![Inline::Text {
                        text: "表头".into(),
                        marks: vec![],
                    }]],
                },
                TableRow::Body {
                    cells: vec![vec![Inline::Text {
                        text: "a|b".into(),
                        marks: vec![],
                    }]],
                },
            ],
        },
    ];
    let target = temp.path().join("rich");
    workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap();
    let markdown = fs::read_to_string(target.join("note.md")).unwrap();
    for expected in [
        "## 二级标题",
        "**粗体**",
        "*斜体*",
        "~~删除~~",
        "[链接](<https://example.com/a_(b)>)",
        "\\[字面\\] \\<script\\> \\$5 \\&x\\;",
        "> 引用\n> \n> - 嵌套",
        "3. 第三项\n   \n   同项第二段",
        "- [x] 已完成",
        "| 表头 |\n| --- |\n| a\\|b |",
    ] {
        assert!(
            markdown.contains(expected),
            "missing {expected:?}: {markdown}"
        );
    }
}

#[test]
fn code_and_math_sources_keep_whitespace_delimiters_and_table_pipes() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let mut content = session.content;
    let code = "\t// 中文\n\n  <div>$x$</div>\n``````\n  tail  ";
    let block_math = "\\begin{aligned}\nx &= \\frac{1}{2} \\\\\ny &= \\sqrt{3}\n\\end{aligned}";
    let table_math = "\\left|x\\right| + \\lVert y \\rVert";
    let multiline = "\t a|b\r\n  c  ";
    content.sections.content = vec![
        Block::CodeBlock {
            language: Some("unknown-lang".into()),
            code: code.into(),
        },
        Block::MathBlock {
            latex: block_math.into(),
        },
        Block::Paragraph {
            children: vec![Inline::MathInline {
                latex: "\\sum_{i=1}^n i + \\$5".into(),
            }],
        },
        Block::Table {
            rows: vec![
                TableRow::Header {
                    cells: vec![
                        vec![Inline::Text {
                            text: "代码".into(),
                            marks: vec![],
                        }],
                        vec![Inline::Text {
                            text: "公式".into(),
                            marks: vec![],
                        }],
                    ],
                },
                TableRow::Body {
                    cells: vec![
                        vec![Inline::Text {
                            text: " `a|b` ".into(),
                            marks: vec![Mark::Code],
                        }],
                        vec![Inline::MathInline {
                            latex: table_math.into(),
                        }],
                    ],
                },
                TableRow::Body {
                    cells: vec![
                        vec![Inline::Text {
                            text: multiline.into(),
                            marks: vec![Mark::Code],
                        }],
                        vec![Inline::MathInline {
                            latex: "x^2".into(),
                        }],
                    ],
                },
            ],
        },
    ];
    let target = temp.path().join("sources");
    workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap();
    let markdown = fs::read_to_string(target.join("note.md")).unwrap();
    assert!(
        markdown.contains(&format!("```````unknown-lang\n{code}\n```````")),
        "{markdown}"
    );
    assert!(markdown.contains(&format!("$$\n{block_math}\n$$")));
    assert!(markdown.contains("$\\sum_{i=1}^n i + \\$5$"));
    assert!(markdown.contains("``  `a\\|b`  ``"));
    assert!(markdown.contains(&format!("${table_math}$")));
    assert!(markdown.contains("| $x^2$ |"));
    let attachments: Vec<Vec<u8>> = fs::read_dir(target.join("assets"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect();
    for source in [code, table_math, multiline, "\\sum_{i=1}^n i + \\$5"] {
        assert!(
            attachments.contains(&source.as_bytes().to_vec()),
            "missing exact source: {source:?}"
        );
    }
    assert!(markdown.contains("数学公式的显示取决于阅读器"));
}

#[test]
fn pasted_images_and_complete_static_mind_map_travel_with_the_export_folder() {
    use cuenote_lib::note_format::MindMapNode;
    use uuid::Uuid;
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let image = include_bytes!("fixtures/pixel.png");
    let asset = workspace
        .stage_image(&session.session_id, image.to_vec())
        .unwrap();
    let mut content = session.content;
    let map_id = Uuid::new_v4().to_string();
    let root_id = Uuid::new_v4().to_string();
    let child_id = Uuid::new_v4().to_string();
    content.sections.content = vec![
        Block::Image {
            asset_id: asset.id.clone(),
            display_width_px: 320,
        },
        Block::MindMap {
            map_id: map_id.clone(),
            root_id: root_id.clone(),
            nodes: vec![
                MindMapNode {
                    id: root_id,
                    text: "根节点 <script> & 中文".into(),
                    children: vec![child_id.clone()],
                },
                MindMapNode {
                    id: child_id,
                    text: format!("{}最后一个节点", "长节点保留全部文字\n".repeat(90)),
                    children: vec![],
                },
            ],
        },
    ];
    let target = temp.path().join("before-move");
    workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap();
    let moved = temp.path().join("移动后的导出");
    fs::rename(target, &moved).unwrap();
    let markdown = fs::read_to_string(moved.join("note.md")).unwrap();
    let image_path = format!("assets/image-{}.png", asset.id);
    assert!(markdown.contains(&format!("]({image_path})")));
    assert_eq!(fs::read(moved.join(image_path)).unwrap(), image);
    let mut previews = fs::read_dir(moved.join("assets"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
        .collect::<Vec<_>>();
    previews.sort();
    assert!(
        previews.len() > 1,
        "large maps must use bounded readable pages"
    );
    let mut all = String::new();
    for preview in previews {
        let name = preview.file_name().unwrap().to_str().unwrap();
        assert!(markdown.contains(&format!("](assets/{name})")));
        let svg = fs::read_to_string(preview).unwrap();
        assert!(!svg.contains("<script>"));
        assert!(!svg.contains("href="));
        all.push_str(&svg);
    }
    assert!(all.contains("根节点 &lt;script&gt; &amp; 中文"));
    assert!(all.contains("最后一个节点"));
    assert!(all.contains("上级 #1"));
    assert!(markdown.contains("静态预览，不能在 Markdown 中继续编辑导图"));
}

#[test]
fn existing_and_managed_targets_are_rejected_without_overwrite_or_session_change() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let existing = temp.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("note.md"), b"keep me").unwrap();
    let occupied = temp.path().join("occupied");
    fs::write(&occupied, b"keep file").unwrap();
    for path in [&existing, &occupied] {
        assert_eq!(
            workspace
                .export_current_markdown_bundle(
                    &session.session_id,
                    session.content.clone(),
                    path.clone()
                )
                .unwrap_err()
                .kind,
            "alreadyExists"
        );
    }
    assert_eq!(fs::read(existing.join("note.md")).unwrap(), b"keep me");
    assert_eq!(fs::read(occupied).unwrap(), b"keep file");
    for directory in ["library", "settings", "recovery"] {
        let target = temp.path().join(directory).join("forbidden-export");
        assert_eq!(
            workspace
                .export_current_markdown_bundle(
                    &session.session_id,
                    session.content.clone(),
                    target.clone()
                )
                .unwrap_err()
                .kind,
            "unsafePath"
        );
        assert!(!target.exists());
    }
    assert!(workspace
        .export_current_markdown_bundle(
            &session.session_id,
            session.content.clone(),
            temp.path().join("missing/parent/export")
        )
        .is_err());
    assert_eq!(
        workspace
            .export_current_markdown_bundle(
                "old-session",
                session.content,
                temp.path().join("stale")
            )
            .unwrap_err()
            .kind,
        "staleSession"
    );
    assert_eq!(
        workspace.snapshot().unwrap().session.unwrap().session_id,
        session.session_id
    );
}

#[test]
fn exceptional_delimiters_keep_visible_type_and_exact_sources_instead_of_changing_meaning() {
    let (temp, mut workspace) = setup();
    workspace.use_default_library().unwrap();
    let session = workspace.new_note().unwrap();
    let mut content = session.content;
    content.sections.content = vec![
        Block::Paragraph {
            children: vec![
                Inline::Text {
                    text: "a".into(),
                    marks: vec![],
                },
                Inline::Text {
                    text: "!".into(),
                    marks: vec![Mark::Bold],
                },
                Inline::Text {
                    text: "b".into(),
                    marks: vec![],
                },
                Inline::MathInline {
                    latex: "a$b".into(),
                },
            ],
        },
        Block::Table {
            rows: vec![
                TableRow::Header {
                    cells: vec![vec![Inline::Text {
                        text: "Code".into(),
                        marks: vec![],
                    }]],
                },
                TableRow::Body {
                    cells: vec![vec![Inline::Text {
                        text: "a\\|b".into(),
                        marks: vec![Mark::Code],
                    }]],
                },
            ],
        },
        Block::MathBlock {
            latex: "x\n$$\ny".into(),
        },
    ];
    let target = temp.path().join("edge-cases");
    workspace
        .export_current_markdown_bundle(&session.session_id, content, target.clone())
        .unwrap();
    let markdown = fs::read_to_string(target.join("note.md")).unwrap();
    assert!(markdown.contains("a<strong>\\!</strong>b"), "{markdown}");
    assert!(markdown.contains("```tex\n$a$b$\n```"));
    assert!(markdown.contains("```tex\n$$\nx\n$$\ny\n$$\n```"));
    let attachments = fs::read_dir(target.join("assets"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    for source in ["a$b", "a\\|b", "x\n$$\ny"] {
        assert!(
            attachments.contains(&source.as_bytes().to_vec()),
            "missing {source}"
        );
    }
}

#[cfg(all(debug_assertions, feature = "storage-fault-injection"))]
mod faults {
    use super::*;
    use std::process::Command;

    #[test]
    fn export_child() {
        let Some(root) = std::env::var_os("CUENOTE_EXPORT_PROBE_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        assert!(root.is_absolute() && root.is_dir());
        assert_eq!(
            fs::canonicalize(&root).unwrap(),
            fs::canonicalize(std::env::var_os("CUENOTE_FAULT_ROOT").unwrap()).unwrap()
        );
        let mut workspace = Workspace::new(
            root.join("settings"),
            root.join("recovery"),
            root.join("library"),
        );
        workspace.use_default_library().unwrap();
        let session = workspace.new_note().unwrap();
        workspace
            .save_note_content(&session.session_id, session.content.clone())
            .unwrap();
        let native = root
            .join("library/notes")
            .join(format!("{}.cuenote", session.note_id));
        fs::write(root.join("source-path.txt"), native.to_str().unwrap()).unwrap();
        fs::copy(native, root.join("original.bin")).unwrap();
        let image = workspace
            .stage_image(
                &session.session_id,
                include_bytes!("fixtures/pixel.png").to_vec(),
            )
            .unwrap();
        let mut content = session.content;
        content.title = "中断导出草稿".into();
        content.sections.content = vec![Block::Image {
            asset_id: image.id,
            display_width_px: 1,
        }];
        let result = workspace.export_current_markdown_bundle(
            &session.session_id,
            content,
            root.join("export"),
        );
        fs::write(
            root.join("result.json"),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn changed_published_files_are_rejected_by_the_final_readback() {
        use std::time::{Duration, Instant};
        let (temp, _) = setup();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "faults::export_child", "--nocapture"])
            .env("CUENOTE_EXPORT_PROBE_ROOT", temp.path())
            .env("CUENOTE_FAULT_ROOT", temp.path())
            .env("CUENOTE_FAULT_STAGE", "markdown_export_verified")
            .env("CUENOTE_FAULT_ACTION", "pause")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let marker = temp
            .path()
            .join(".cuenote-fault-hit-markdown_export_verified");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() && Instant::now() < deadline {
            assert!(
                child.try_wait().unwrap().is_none(),
                "export child exited before pause"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            marker.exists(),
            "export did not reach the final verification boundary"
        );
        fs::write(
            temp.path().join("export/note.md"),
            b"changed by another writer",
        )
        .unwrap();
        fs::write(temp.path().join(".cuenote-fault-resume"), b"resume").unwrap();
        assert!(child.wait().unwrap().success());
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join("result.json")).unwrap()).unwrap();
        assert_eq!(result["Err"]["kind"], "exportUncertain");
        let source = fs::read_to_string(temp.path().join("source-path.txt")).unwrap();
        assert_eq!(
            fs::read(source).unwrap(),
            fs::read(temp.path().join("original.bin")).unwrap()
        );
        assert_eq!(
            fs::read(temp.path().join("export/note.md")).unwrap(),
            b"changed by another writer"
        );
    }

    #[test]
    fn interruption_corruption_and_io_errors_never_report_incomplete_exports_as_success() {
        for (stage, action, published) in [
            ("markdown_export_write", "io", false),
            ("markdown_export_write", "disk_full", false),
            ("markdown_export_write", "corrupt", false),
            ("markdown_export_write", "exit", false),
            ("markdown_export_ready", "io", false),
            ("markdown_export_ready", "exit", false),
            ("markdown_export_published", "io", true),
            ("markdown_export_published", "exit", true),
            ("markdown_export_verified", "io", true),
        ] {
            let (temp, _) = setup();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "faults::export_child", "--nocapture"])
                .env("CUENOTE_EXPORT_PROBE_ROOT", temp.path())
                .env("CUENOTE_FAULT_ROOT", temp.path())
                .env("CUENOTE_FAULT_STAGE", stage)
                .env("CUENOTE_FAULT_ACTION", action)
                .output()
                .unwrap();
            assert!(
                temp.path()
                    .join(format!(".cuenote-fault-hit-{stage}"))
                    .exists(),
                "missing fault marker {stage}/{action}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let source = fs::read_to_string(temp.path().join("source-path.txt")).unwrap();
            assert_eq!(
                fs::read(source).unwrap(),
                fs::read(temp.path().join("original.bin")).unwrap()
            );
            let result_path = temp.path().join("result.json");
            if action == "exit" {
                assert_eq!(output.status.code(), Some(86));
                assert!(!result_path.exists());
            } else {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stdout)
                );
                let result: serde_json::Value =
                    serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
                assert_eq!(
                    result["Err"]["kind"],
                    if published {
                        "exportUncertain"
                    } else {
                        "exportFailed"
                    }
                );
            }
            let export = temp.path().join("export");
            assert_eq!(export.exists(), published);
            if published {
                assert!(fs::read_to_string(export.join("note.md"))
                    .unwrap()
                    .contains("中断导出草稿"));
                let attachments = fs::read_dir(export.join("assets"))
                    .unwrap()
                    .map(|entry| fs::read(entry.unwrap().path()).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(
                    attachments,
                    vec![include_bytes!("fixtures/pixel.png").to_vec()]
                );
            }
        }
    }
}
