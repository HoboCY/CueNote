---
status: accepted
---

# 用 Tauri 与 Rust 管理文件，用网页组件提供编辑能力

2026-10-14，CueNote 在用户选择 Tauri 路线、Windows 样机集成及人工试用通过后，确定采用 Tauri 2 + Rust + React/TypeScript，并复用免费开源的 Tiptap/ProseMirror 与 Mind Elixir；Rust 通过受限操作接口集中管理笔记文件、恢复与系统交互。该分工减少自行实现富文本和导图编辑器的工作，但需要分别验证 Windows 与 macOS 的 WebView、输入法和剪贴板体验；Windows 通过不能代替 Mac 或 OneDrive 验收。

首版面向 Windows x64 和 Apple Silicon Mac；最低 OS 版本、容量与性能门槛由验收决策确定。完整架构及理由以 [整体技术方案的解决结论](https://github.com/HoboCY/CueNote/issues/7#issuecomment-6164489548) 为准；[样机观察及用户反馈](https://github.com/HoboCY/CueNote/issues/11#issuecomment-6163449072) 是选择依据，不是正式功能交付。
