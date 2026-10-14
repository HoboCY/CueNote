# 正式工程基础验证

日期：2026-10-14。对应任务：[启动正式桌面工程并贯通宿主调用 #13](https://github.com/HoboCY/CueNote/issues/13)。

最初的工作区验证针对 `codex/project-foundation` 本地正式工程，保留独立技术样机及原有文档。当时尚未提交或推送；后续按用户授权完成提交、远程检出和交付复验，结果见下方“交付复验”。

## 构建结果

环境为当前 Windows x64、Node.js 24.21.0、Rust/Cargo 1.99.0，以及已安装的 Visual Studio C++ 工具、Windows SDK 和 WebView2。依赖版本由 npm 与 Cargo 锁文件固定。

| 检查 | 结果 |
| --- | --- |
| `npm run typecheck` | 通过 |
| `npm run build` | 通过，生成前端静态资源 |
| `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` | 通过 |
| `cargo check --offline --locked --manifest-path src-tauri/Cargo.toml` | 通过 |
| `npm run build:desktop -- --debug` | 通过，生成包含前端资源的桌面调试程序；本次 Cargo 使用本机缓存 |

最终观察的程序为 `src-tauri/target/debug/cuenote.exe`，SHA-256：

```text
4D85D2B120AA5487606CBE09618BC9550BA7741E85E95127960D48FB76B886DA
```

直接执行 `cargo build` 会生成依赖开发服务器的开发模式程序；验证独立运行时必须使用上面的 Tauri 构建命令。本次实际观察的是后者，在没有前端开发服务器的条件下加载内置资源。

## 真实桌面观察

观察时间为 2026-10-14 19:17（UTC+8）。通过仅对本次测试进程启用的 WebView2 本机调试端口读取真实桌面内容，未注入宿主模拟或替换命令返回值；正式配置不包含调试端口参数。

6 项检查全部通过：

1. 窗口加载 `http://tauri.localhost/`，界面显示“桌面已连接”及 `v0.1.0`。
2. 真实 `get_app_version` 宿主命令返回 `0.1.0`。
3. 标题、线索区、内容区、总结区均存在，默认窗口没有横向溢出，也没有会接收未保存文字的输入控件。
4. 点击侧栏按钮后，宽度从 252 缩为 72 像素；`aria-expanded` 为 `false`，标签为“展开侧栏”。
5. 再次点击后展开状态恢复。
6. 本次交互观察期间未捕获运行异常。

截图和机器可读检查结果保存在本次会话的 `implementation-planning` 产物目录，文件名分别为 `foundation-windows.png` 和 `foundation-windows.json`。这些本机产物尚未随代码发布。

## 审查与边界

独立静态审查未发现实质问题：真实宿主命令、窗口权限和 CSP 范围符合当前基础工程；浏览器预览不模拟桌面连接成功。

当前只有基础窗口、康奈尔布局预览、侧栏交互和受限宿主调用，没有笔记编辑、保存或恢复行为。本次结果不代表完整 A05/A24 验收完成。macOS 构建、安装包、真实 OneDrive 双端、性能及完整日用验收，分别由后续任务负责。

下一项功能为 [首篇笔记保存与重开 #14](https://github.com/HoboCY/CueNote/issues/14)。

## 交付复验

基础工程提交为 `03758c3298318870e2324cdeda8478d7f77e601f`，已推送到 `origin/codex/project-foundation`。从 GitHub 新克隆该分支后核对 SHA 完全一致，初始工作区干净。

- `npm ci --no-audit --no-fund` 成功安装锁定依赖。
- Rust 格式检查通过。
- `npm run build:desktop -- --debug` 的 TypeScript、Vite 和 Windows Rust/Tauri 构建全部通过，使用全新 Cargo target 目录及本机依赖源缓存。
- 构建后已跟踪和未跟踪文件状态均为空。
- 新构建程序再次通过上述 6 项真实桌面检查；持续运行数分钟，未发现相应应用错误事件。

新检出程序的 SHA-256 为 `7D6163EF21D30B4569C3C02E9AB430E66A5370C1F95444B08F04FEFCF646E780`。不同路径的调试构建不要求二进制逐字节相同；本次证明锁定代码与依赖可以从新检出构建并运行。

本次产物位于会话 `implementation-execution/foundation-check/artifacts` 目录，包含构建证据、真实窗口检查 JSON 和截图。对桌面进程的核实使用提升权限查询；受限查询不可见不能作为程序退出的证据。

## Standards

独立审查固定树 `114b39b9d7421b050bffba23a149d71ce895e72d`，相对基础提交 `fc4488a3a66d44b6d5701c3805dcd38097df48bb`：未发现实质的项目约定违规。术语、能力范围、CSP 和静态预览符合项目文档。

## Spec

独立对照 #13 的 5 项验收条件：未发现遗漏实现、越界或错误实现。原先待补的干净检出验证已由上述复验完成；Mac、安装包、完整 A05/A24 仍属于后续任务。

审查结论：Standards 0 项、Spec 0 项实质发现。
