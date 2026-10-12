# CueNote Rust 跨平台与开源组件适配补充

- 研究任务：[Rust 路线及免费开源组件能否满足 CueNote 首版？](https://github.com/HoboCY/CueNote/issues/10)；支持：[CueNote 首版采用什么整体技术方案？](https://github.com/HoboCY/CueNote/issues/7)。
- 核验日期：2026-10-12。范围限于官方文档与指定仓库的源码/包元数据；没有安装依赖、编写应用或实机运行。
- 需求边界：Windows/macOS 康奈尔布局笔记；Markdown 所见即所得、图片粘贴、可编辑思维导图；单篇 `.cuenote` 为 ZIP，含 JSON 与图片。
- 复用：[桌面宿主可行性](https://github.com/HoboCY/CueNote/blob/ec926bb18ff785e4a88fa2a89ba7ae058a19f35f/docs/research/desktop-feasibility.md) 与 [编辑器/导图可行性](https://github.com/HoboCY/CueNote/blob/ecac5c63649b7c5b559e8f9f51b8d0758926b949/docs/research/editor-feasibility.md)，不重复比较通用桌面框架和编辑器。

## 判定

- **事实：**Tauri 2 的 Rust 宿主可以搭配编译为 HTML/JavaScript/CSS 的任意前端；JavaScript 可通过 invoke 调用 Rust。[官方架构说明](https://v2.tauri.app/start/)；[Rust 调用文档](https://v2.tauri.app/develop/calling-rust/)
- **事实：**Windows WebView 使用 Microsoft Edge WebView2；macOS 使用系统 WebKit。两端可复用应用代码，但渲染和系统输入行为会受不同 WebView 影响。[Wry 平台说明](https://github.com/tauri-apps/wry)
- **事实：**Tauri `v2.12.2` 的 Cargo workspace 许可为 Apache-2.0 OR MIT。[官方 Cargo.toml](https://github.com/tauri-apps/tauri/blob/tauri-v2.12.2/Cargo.toml)
- **事实：**官方构建示例包含 `aarch64-apple-darwin`，并标明 Apple Silicon M1 及以上；这证明目标架构有构建路径，不是 CueNote 在目标机器上的验收。[官方 CI 构建示例](https://v2.tauri.app/distribute/pipelines/github/)
- **推断：**Tauri 2 + React/TypeScript 编辑器 + Rust 文件服务适合作为先验证的组合：前端承载 Tiptap/Mind Elixir，Rust 负责 `.cuenote` ZIP 的读写和图片资产管理。
- **事实：**Iced 0.14.0 官方项目列出 Windows、macOS、Linux 与 Web；该版本许可元数据为 MIT。[Iced 项目](https://github.com/iced-rs/iced)；[0.14.0 Cargo.toml](https://github.com/iced-rs/iced/blob/0.14.0/Cargo.toml)
- **事实：**Iced `text_editor` 文档把控件定义为多行文本输入；`markdown` widget 的接口是解析并显示 Markdown。[text_editor API](https://docs.rs/iced/0.14.0/iced/widget/text_editor/index.html)；[markdown widget API](https://docs.rs/iced/0.14.0/iced/widget/markdown/index.html)
- **推断：**Markdown 显示控件不等同于 Markdown 所见即所得编辑器。Iced 可作为全 Rust 桌面壳，但本需求还需自行构建富文本编辑、图片粘贴与可编辑导图交互层；目前资料不能证明这条路实现量更低。

## 实际功能的免费开源边界

- **事实：**核对的 Tiptap 官方 `main` 包元数据当前均为 3.30.3，`@tiptap/core`、`@tiptap/starter-kit`、`@tiptap/extension-image`、`@tiptap/extension-file-handler`、`@tiptap/markdown` 的 `package.json` 均标 MIT；仓库 `LICENSE.md` 也是 MIT。
- 对应源码：[core](https://github.com/ueberdosis/tiptap/blob/main/packages/core/package.json)、[StarterKit](https://github.com/ueberdosis/tiptap/blob/main/packages/starter-kit/package.json)、[Image](https://github.com/ueberdosis/tiptap/blob/main/packages/extension-image/package.json)、[FileHandler](https://github.com/ueberdosis/tiptap/blob/main/packages/extension-file-handler/package.json)、[Markdown](https://github.com/ueberdosis/tiptap/blob/main/packages/markdown/package.json)、[LICENSE](https://github.com/ueberdosis/tiptap/blob/main/LICENSE.md)。
- **事实：**StarterKit 含常用段落、标题、列表、粗体/斜体、链接与撤销/重做；Image 提供图片节点显示；FileHandler 提供文件粘贴/拖放回调，但明确不做上传。[StarterKit](https://tiptap.dev/docs/editor/extensions/functionality/starterkit)；[Image](https://tiptap.dev/docs/editor/extensions/nodes/image)；[FileHandler](https://tiptap.dev/docs/editor/extensions/functionality/filehandler)
- **事实：**Tiptap Markdown 包可在 Markdown 字符串与 Tiptap JSON 间解析/序列化，但官方仍标为 Beta，提示行为与支持边界可能变化。[Markdown 文档](https://tiptap.dev/docs/editor/markdown)
- **事实：**Mind Elixir 当前 `master` 包元数据为 5.15.1，许可标 MIT；官方 README 列出节点编辑、撤销/重做、结构化 `getData()`/`getDataString()` 与 `init()`/`refresh()`。
- 对应源码：[package.json](https://github.com/ssshooter/mind-elixir-core/blob/master/package.json)；[README](https://github.com/ssshooter/mind-elixir-core/blob/master/readme.md)；[LICENSE](https://github.com/ssshooter/mind-elixir-core/blob/master/LICENSE)。
- **结论/推断：**常用排版、图片节点、文件粘贴入口和可编辑导图数据可由上述免费 MIT 组件提供，不要求购买 Tiptap 云服务。
- Tiptap Platform/部分 Pro 扩展是另一条收费边界，面向协作、AI、文档历史等高级能力；这些不是单机 `.cuenote` 的必要路径。[开源与平台边界](https://tiptap.dev/open-source-to-platform)
- **实现责任：**FileHandler 不保存图片；CueNote 仍需把剪贴板文件安全地复制进 ZIP、在 JSON 中保存稳定引用，并定义导入/导出往返。
- Mind Elixir 与 Tiptap 各自有撤销历史；跨焦点撤销由应用明确路由，是设计推断而非现成统一能力。

## 未知与验证边界

- 中文输入法组合态、跨编辑器撤销/重做、从不同应用粘贴图片的剪贴板格式，必须在 Windows 与真实 Mac 上试用。
- 目标 Mac M1 Pro 来自用户输入；macOS `26.6.2` 尚未实核。本报告没有设备、系统版本、签名或发行验收。
- Tiptap 源码按 `main`、Mind Elixir 按 `master` 记录；最终选型应锁定具体包版本并复核锁文件及传递依赖许可证。
- 本报告只核对所列源码与包元数据，尚未验证各包组合或跨平台兼容性。

## 主要官方资料

1. [Tauri 架构与前端选择](https://v2.tauri.app/start/)；[Windows/macOS 构建前置条件](https://v2.tauri.app/start/prerequisites/)。
2. [Tauri/Wry 的 Windows WebView2 与 macOS WebKit 平台实现](https://github.com/tauri-apps/wry)；[Tauri v2.12.2 许可元数据](https://github.com/tauri-apps/tauri/blob/tauri-v2.12.2/Cargo.toml)。
3. [Tauri 官方 Apple Silicon 构建示例](https://v2.tauri.app/distribute/pipelines/github/)。
4. [Iced 0.14.0 平台与许可](https://github.com/iced-rs/iced/blob/0.14.0/Cargo.toml)；[text_editor API](https://docs.rs/iced/0.14.0/iced/widget/text_editor/index.html)；[Markdown 显示 API](https://docs.rs/iced/0.14.0/iced/widget/markdown/index.html)。
5. [Tiptap 开源/收费功能边界](https://tiptap.dev/open-source-to-platform)。
6. [Tiptap 常用排版、图片和文件粘贴扩展](https://tiptap.dev/docs/editor/extensions/functionality/starterkit)；[Markdown Beta 文档](https://tiptap.dev/docs/editor/markdown)。
7. Tiptap 包元数据：[core](https://github.com/ueberdosis/tiptap/blob/main/packages/core/package.json)、[StarterKit](https://github.com/ueberdosis/tiptap/blob/main/packages/starter-kit/package.json)、[Image](https://github.com/ueberdosis/tiptap/blob/main/packages/extension-image/package.json)、[FileHandler](https://github.com/ueberdosis/tiptap/blob/main/packages/extension-file-handler/package.json)、[Markdown](https://github.com/ueberdosis/tiptap/blob/main/packages/markdown/package.json) 与 [MIT LICENSE](https://github.com/ueberdosis/tiptap/blob/main/LICENSE.md)。
8. [Mind Elixir 可编辑数据接口](https://github.com/ssshooter/mind-elixir-core/blob/master/readme.md)；[包许可字段](https://github.com/ssshooter/mind-elixir-core/blob/master/package.json)；[MIT LICENSE](https://github.com/ssshooter/mind-elixir-core/blob/master/LICENSE)。
