# 正式工程基础验证

日期：2026-10-14。对应任务：[启动正式桌面工程并贯通宿主调用 #13](https://github.com/HoboCY/CueNote/issues/13)。

本次验证针对 `codex/project-foundation` 本地工作区中的正式工程，保留独立技术样机及原有文档。当前代码尚未提交或推送，远程干净检出的复现检查尚未完成，任务保持打开。

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
