# CueNote

CueNote 是面向 Windows x64 与 Apple Silicon macOS 的个人康奈尔笔记应用。首版规格包含标题、线索区、内容区和总结区，以及 Markdown 排版、公式、代码、图片粘贴、可编辑思维导图与本地文件保存；笔记库由用户通过 OneDrive 同步。

当前开发版本已接入选择笔记库、标题与康奈尔三分区编辑，以及原生文件保存和重开。正文支持基础 Markdown 排版、表格、代码、公式、图片及思维导图，线索和总结使用精简排版，各区保留独立撤销历史。停止输入约一秒后自动保存，切换和关闭会等待待保存内容，也可取消离开；保存失败会保留草稿并提供安全另存，重启后可找回已持久化的恢复草稿。侧栏可展开/收起。完整日用验收尚未完成，技术样机保留在独立原型分支。

笔记本支持新建、重命名和删除空本，默认笔记本不可删除。可在全部笔记或单本中浏览、创建笔记，通过摘要和时间区分同名笔记；移动和置顶不改变内容修改时间。重启定位到上次编辑的笔记，侧栏与列表范围偏好只保存在本机。

删除笔记先进入独立的只读回收站，不自动清空。恢复时返回原笔记本；原本已删除时选择现有目标。已无正常笔记的自建本可以删除，回收站记录仍可恢复。永久删除单独确认，只移除选中的笔记包及内含附件，独立备份、历史与恢复副本不受影响。

## 开发环境

需要 Node.js/npm、Rust，以及目标平台的 Tauri 构建依赖。Windows 需要 C++ 构建工具、Windows SDK 和 WebView2；macOS 桌面开发需要 Xcode 或其命令行工具。安装步骤以 [Tauri 官方前置条件](https://v2.tauri.app/start/prerequisites/)为准。

本仓库使用 npm 与 Cargo 锁文件；不要把前端浏览器预览当作桌面宿主验证。首次安装和 Rust 构建需要访问依赖源，后续可使用本机缓存。

## 运行

在仓库根目录执行：

```sh
npm ci
npm run desktop
```

只查看浏览器界面：

```sh
npm run dev
```

开发服务器使用 `http://127.0.0.1:4175`。浏览器模式会显示预览状态，不模拟宿主调用成功。

## 检查与构建

双平台个人安装包使用独立的[构建与交付流程](docs/personal-packages.md)，
包含锁定工具链、完整许可材料及安装/升级/卸载数据保留检查。

```sh
npm run typecheck
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo check --locked --manifest-path src-tauri/Cargo.toml
cargo test --locked --manifest-path src-tauri/Cargo.toml
cargo run --locked --manifest-path src-tauri/Cargo.toml --bin export_note_schema -- --check
npm run build:desktop -- --debug
```

最后一条命令生成调试桌面程序，不生成安装包。文件格式、真实目录的安全保存和库会话分别有集成测试；磁盘测试使用 `src-tauri/target/testdata` 中的隔离目录。受限执行器可能阻止 Windows 文件替换，须先区分执行器权限与真实目录能力，不能为通过测试改成直接覆盖。

保存中断矩阵使用单独启用的调试功能，先构建子进程探针再运行测试：

```sh
cargo build --locked --manifest-path src-tauri/Cargo.toml --features storage-fault-injection --example storage_fault_probe
cargo test --locked --manifest-path src-tauri/Cargo.toml --features storage-fault-injection --test storage_interruptions -- --test-threads=1
```

该矩阵在隔离目录注入进程退出、候选损坏与模拟空间不足，不填满用户磁盘。普通构建不启用注入，发布构建忽略故障环境变量。

修改 Rust 文档类型后，运行 `cargo run --manifest-path src-tauri/Cargo.toml --bin export_note_schema` 更新 JSON Schema 与 TypeScript，再执行上面的同步检查。调试桌面验证可显式设置绝对路径 `CUENOTE_TEST_ROOT`，把设置、恢复区和默认库放入独立测试目录；发布构建忽略此变量。真实桌面验证请使用 Tauri 构建产物，普通 Cargo 开发构建依赖前端开发服务器。

## 工程结构

| 位置 | 职责 |
| --- | --- |
| `src/` | React 界面及宿主交互 |
| `src-tauri/` | Rust 宿主、笔记格式、安全存储、库会话及桌面构建 |
| `schemas/`、`src/generated/` | 从 Rust 类型生成的 v1 文档契约 |
| `docs/` | 工作约定、架构决策与开发计划 |
| `GLOSSARY.md` | 项目领域术语 |

笔记格式、目录布局、资源上限与失败行为见[首篇笔记实现契约](docs/note-storage-contract.md)。笔记库可以交给 OneDrive 同步；本机设置与恢复副本独立存放，应用只报告本机保存结果。

自动保存、可取消的关闭等待与安全另存见[自动保存和恢复契约](docs/autosave-recovery-contract.md)。另存成功只表示独立副本已保存，不会把原位置的失败状态改成成功；尚未落盘的最后输入不承诺在断电或强制结束时零丢失。

三分区编辑能力、Markdown 输入、基础表格与文档转换边界见[富文本编辑契约](docs/rich-editor-contract.md)。文件使用应用原生结构，编辑器内部 JSON 不作为磁盘协议。

正文支持反引号行内代码、围栏代码块，以及 JavaScript、TypeScript、Python、Rust、C#、SQL、JSON 的离线高亮。代码块可选择或填写语言；未知语言保留标记并显示纯文本。缩进和空行按原文保存，连续回车留在代码块内，通过工具栏或 Ctrl/⌘+Enter 离开。详见[代码编辑契约](docs/code-editing-contract.md)与[全文搜索契约](docs/search-contract.md)。

正文支持 `$...$` 行内公式和独占行 `$$` 围起的公式块，也可从工具栏插入，点击公式修改 LaTeX 源码。公式使用随应用打包的 KaTeX 离线排版；语法错误或超出显示限制时显示源码提示，完整源码仍可编辑和保存。确认修改可通过正文撤销。详见[公式编辑契约](docs/math-editing-contract.md)。

正文支持粘贴剪贴板提供的 PNG/JPEG 图片及转换为 PNG 的系统位图，图片保存在笔记文件内。图片可等比调整显示尺寸、上下移动、删除和撤销；原资产字节保持不变。超出资源预算时提示并保留已有草稿，切换和关闭等待已开始的粘贴。详见[图片实现契约](docs/image-editing-contract.md)。

正文可插入思维导图内容块，展开后修改节点文字和层级，收起后更新预览并返回原位置。展开中的修改参加自动保存；导图内部历史独立，收起写回一次正文历史。单篇所有导图合计最多 5,000 个节点，完整结构保存在原生笔记文件内。详见[思维导图实现契约](docs/mindmap-editing-contract.md)。

笔记本归属、置顶、排序和重启定位见[笔记本与列表契约](docs/notebook-management-contract.md)。这些组织信息属于原生事实文件；本项的本机验证不代表 OneDrive 组织信息同步验收已经完成。

备份面板可创建和校验完整库快照，并从备份恢复单篇笔记或整个笔记库。恢复前先备份当前库；整库恢复中断后可继续或回退，完成前暂停普通库操作。此流程按文件记录进度，不提供整个目录的原子事务。详见[备份约定](docs/backup-contract.md)、[单篇恢复约定](docs/backup-restore-contract.md)与[整库恢复约定](docs/library-restore-contract.md)。

## 规格与计划

可通过“打开库外笔记…”直接编辑独立 `.cuenote` 文件，并另存完整副本到指定笔记本。原文件保持独立，库外内容不进入当前库的搜索和整库备份。详见[库外笔记约定](docs/external-notes-contract.md)。

“导出 Markdown…”将当前内容导出为独立目录，保留三分区、代码、公式和相对图片附件；导图提供静态 SVG 预览。特殊语法附原文，移动时保留整个目录。数学显示取决于阅读器，导出不替代原生笔记备份。详见[Markdown 导出约定](docs/markdown-export-contract.md)。

- [首版最终规格与验收标准](https://github.com/HoboCY/CueNote/issues/8#issuecomment-6165724793)
- [整体技术方案](https://github.com/HoboCY/CueNote/issues/7#issuecomment-6164489548)
- [GitHub 开发与验收总任务](https://github.com/HoboCY/CueNote/issues/12)
- [开发任务与依赖](docs/implementation-plan.md)
- [正式工程本机验证记录](docs/foundation-validation.md)
- [首篇笔记保存与重开验证](docs/note-storage-validation.md)
- [自动保存与恢复验证](docs/autosave-recovery-validation.md)
- [笔记本与列表验证](docs/notebook-management-validation.md)
- [回收站验证](docs/trash-validation.md)
- [三分区富文本验证](docs/rich-editor-validation.md)
- [代码编辑验证](docs/code-editing-validation.md)
- [公式编辑验证](docs/math-editing-validation.md)
- [图片编辑验证](docs/image-editing-validation.md)
- [思维导图验证](docs/mindmap-editing-validation.md)
- [外部变化与冲突验证](docs/external-changes-validation.md)
- [同步文件到达与按需文件验证](docs/sync-files-validation.md)
- [全文搜索验证](docs/search-validation.md)
- [备份验证](docs/backup-validation.md)
- [单篇恢复验证](docs/backup-restore-validation.md)
- [整库恢复验证](docs/library-restore-validation.md)
- [库外笔记验证](docs/external-notes-validation.md)
- [Markdown 导出验证](docs/markdown-export-validation.md)
- [本机容量与性能验证方法](docs/performance-method.md)
- [Windows 本机容量与性能结果](docs/performance-validation.md)

Windows 千篇库与压力笔记的本机测量及主代理审查已完成。Mac、真实 OneDrive 双端、安装升级及完整日用验收仍需后续测试；当前版本不代表已达到日用标准。

## 许可

项目采用 [Apache License 2.0](LICENSE)。第三方依赖遵守各自许可；直接依赖见[开源组件](docs/open-source-components.md)，正式分发前补齐随包许可清单。
