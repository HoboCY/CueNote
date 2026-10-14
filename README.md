# CueNote

CueNote 是面向 Windows x64 与 Apple Silicon macOS 的个人康奈尔笔记应用。首版规格包含标题、线索区、内容区和总结区，以及 Markdown 排版、公式、代码、图片粘贴、可编辑思维导图与本地文件保存；笔记库由用户通过 OneDrive 同步。

当前为正式工程的基础开发预览：提供桌面窗口、康奈尔布局、侧栏展开/收起与受限宿主调用。笔记编辑和保存尚未接入，界面不会接收或保存真实笔记。已验证的技术样机保留在独立原型分支。

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

```sh
npm run typecheck
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo check --locked --manifest-path src-tauri/Cargo.toml
npm run build:desktop -- --debug
```

最后一条命令生成调试桌面程序，不生成安装包。当前工程没有文件保存等领域行为测试；后续任务围绕实际保存、编辑和恢复接口添加测试，不用空测试数量代替验收。

## 工程结构

| 位置 | 职责 |
| --- | --- |
| `src/` | React 界面及宿主交互 |
| `src-tauri/` | Rust 宿主、能力配置及桌面构建 |
| `docs/` | 工作约定、架构决策与开发计划 |
| `GLOSSARY.md` | 项目领域术语 |

只在行为落地时增加模块，避免提前创建空的存储、同步或编辑层。笔记格式与用户数据目录将由首个可写笔记流程实现。

## 规格与计划

- [首版最终规格与验收标准](https://github.com/HoboCY/CueNote/issues/8#issuecomment-6165724793)
- [整体技术方案](https://github.com/HoboCY/CueNote/issues/7#issuecomment-6164489548)
- [GitHub 开发与验收总任务](https://github.com/HoboCY/CueNote/issues/12)
- [开发任务与依赖](docs/implementation-plan.md)
- [正式工程本机验证记录](docs/foundation-validation.md)

Mac、真实 OneDrive 双端、完整编辑/保存、容量与性能验收仍需后续实现与测试；基础工程可运行不代表已达到日用标准。

## 许可

项目采用 [Apache License 2.0](LICENSE)。第三方依赖继续遵守各自许可；正式分发前按已定任务补齐依赖许可清单。
