# CueNote 开源组件

当前正式工程只使用免费开源依赖。版本以 `package-lock.json` 和 `src-tauri/Cargo.lock` 为准；下表记录直接选择的组件。运行时与构建工具均未接入付费扩展或云服务。

## 界面与构建

| 组件 | 锁定版本 | 许可 | 用途 |
| --- | --- | --- | --- |
| React / React DOM | 19.1.1 | MIT | 界面与交互 |
| Tiptap core / react / pm / starter-kit | 3.31.4 | MIT | 三分区富文本编辑、选区与独立撤销 |
| Tiptap extension-table / extension-list | 3.31.4 | MIT | 基础表格与任务列表 |
| Tiptap extension-code-block-lowlight | 3.31.4 | MIT | 保留源码的代码块与高亮装饰 |
| Tiptap extension-mathematics | 3.31.4 | MIT | 行内和块级公式节点；应用替换输入规则与异步显示 |
| KaTeX | 0.18.10 | MIT | 本地线程中的公式排版及离线字体 |
| lowlight | 3.3.0 | MIT | 语法高亮树生成 |
| highlight.js | 11.11.1 | BSD-3-Clause | 七种代码语言的离线语法定义 |
| Mind Elixir | 5.15.1 | MIT | 展开编辑思维导图的节点和层级；[许可原文](licenses/mind-elixir-LICENSE.txt) |
| @tauri-apps/api | 2.12.3 | Apache-2.0 OR MIT | 受限宿主调用 |
| @tauri-apps/cli | 2.12.1 | Apache-2.0 OR MIT | 桌面构建 |
| TypeScript | 5.9.3 | Apache-2.0 | 类型检查 |
| Vite | 7.1.7 | MIT | 前端构建 |
| @vitejs/plugin-react | 5.0.4 | MIT | React 构建适配 |
| @types/react / @types/react-dom | 19.1.16 / 19.1.9 | MIT | 开发类型声明 |

## 原生宿主与文件

| 组件 | 锁定版本 | 许可 | 用途 |
| --- | --- | --- | --- |
| Tauri / tauri-build | 2.12.3 / 2.7.2 | Apache-2.0 OR MIT | 桌面窗口、宿主与构建 |
| tauri-plugin-dialog | 2.8.2 | Apache-2.0 OR MIT | Rust 发起的本机目录选择 |
| tauri-plugin-opener | 2.7.1 | Apache-2.0 OR MIT | 打开已选笔记库目录 |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 | 结构化文件协议 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | 版本与资产指纹 |
| uuid | 1.28.0 | Apache-2.0 OR MIT | 与标题无关的稳定身份 |
| chrono | 0.4.45 | MIT OR Apache-2.0 | UTC 时间 |
| zip | 9.0.3 | MIT | 原生文件容器 |
| image | 0.25.10 | MIT OR Apache-2.0 | PNG/JPEG 校验与尺寸预算 |
| schemars | 1.2.3 | MIT | 从 Rust 契约生成 JSON Schema |
| ts-rs | 12.0.1 | MIT | 从 Rust 契约生成 TypeScript 声明 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | 隔离文件系统测试 |

许可信息从当前安装包的 `package.json`、npm 锁文件与 `cargo metadata --offline` 获取。完整间接依赖以锁文件为准；安装包任务会生成随包许可清单及所需声明。后续新增组件在实际接入时更新此表，未接入的选型不计为已交付功能。
