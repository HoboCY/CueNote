# CueNote

CueNote 是面向 Windows x64 与 Apple Silicon macOS 的个人康奈尔笔记应用。首版规格包含标题、线索区、内容区和总结区，以及 Markdown 排版、公式、代码、图片粘贴、可编辑思维导图与本地文件保存；笔记库由用户通过 OneDrive 同步。

当前开发版本已接入选择笔记库、标题与康奈尔三分区编辑，以及原生文件保存和重开。正文支持基础 Markdown 排版与表格，线索和总结使用精简排版，各区保留独立撤销历史。停止输入约一秒后自动保存，切换和关闭会等待待保存内容，也可取消离开；保存失败会保留草稿并提供安全另存，重启后可找回已持久化的恢复草稿。侧栏可展开/收起。包含尚未接入编辑功能的代码、公式、图片或导图的有效 v1 笔记暂以只读方式打开，并保留完整副本。完整日用验收尚未完成，技术样机保留在独立原型分支。

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

笔记本归属、置顶、排序和重启定位见[笔记本与列表契约](docs/notebook-management-contract.md)。这些组织信息属于原生事实文件；本项的本机验证不代表 OneDrive 组织信息同步验收已经完成。

## 规格与计划

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

Mac、真实 OneDrive 双端、完整编辑能力、容量与性能验收仍需后续实现与测试；当前版本不代表已达到日用标准。

## 许可

项目采用 [Apache License 2.0](LICENSE)。第三方依赖遵守各自许可；直接依赖见[开源组件](docs/open-source-components.md)，正式分发前补齐随包许可清单。
