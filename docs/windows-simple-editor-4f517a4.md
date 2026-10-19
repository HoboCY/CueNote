# Windows 精简分区排版补充（4f517a4）

2026-10-19，普通安装版 `4f517a4`（程序 SHA-256 `ed2391ed6cd1c050d7929631d574dab85636a00959967904e3539568997a1a68`，`D:\Apps\CueNote\cuenote.exe`，PID 31992）通过 Explorer 用户环境运行，在专用库 `package-output\ordinary-win-management-9760cf6\library` 新建笔记 `6c41d3b0-4661-460c-91c9-87d05b0ee2d7`，补齐 Windows 侧此前唯一缺口：线索/总结区的简单标题、加粗与列表。

- 线索区与总结区各输入 `##` 小标题、加粗段落、`-` 无序项、`1.` 有序项与 `- [ ]` 任务项。原生 `document.json` 为 `heading2` / `bold` / `bulletList` / `orderedList` / `taskList` 结构，任务未勾选，正文区为空，无外层项目符号。
- 保存后正常请求关闭：`normalRequestAccepted=true`、`processEnded=true`；`exitCode=null`、`exitStatusAvailable=false`，不声称应用退出码 0。
- 重开由 Explorer（`explorer.exe`）启动新进程 PID 38980，程序哈希一致。重开脚本重算 11 个原生文件（9 笔记 + 2 笔记本）逐字节一致；本核对再次重算当前库 11/11 一致。Windows 未能把重开窗口切到前台，随后只做只读后台 UIA 读取：211 个元素中同时读到笔记标题与两分区共 10 个精确字符串（H2/加粗/无序/有序/任务 ×2），验证文件明确标注“后台读取、不声称截图或输入”。
- 打开前源状态保留：`windows-simple-format-ready-4f517a4`（窗口属性、辅助功能、截图）与 `windows-previous-launch-before-simple-format.json` 启动记录；`windows-simple-format-initial-4f517a4` 为空目录（无文件内容）。原有 8 篇笔记与 2 个笔记本在四个阶段逐字节不变，未改动产品文件。主执行人另查看 `simple-summary/window.png`，确认两个分区的实际排版；这张图属于关闭前，不冒充重开截图。

Windows 正文 Markdown、三分区撤销/重做、代码、公式与表格证据复用 `079fa6e`（[普通 Windows 版编辑验收](windows-editor-079fa6e.md)）与 `7a11fb1` 的待办/引用/分隔线修复复验（[双端补充验收](editor-block-followup-7a11fb1.md)）；`4f517a4` 的差异仅 `src/image-extensions.ts`、对应测试与图片修复文档，不涉及上述路径。真实中文输入法沿用用户对 `cbd9c08` 的明确确认（[普通启动对照](windows-user-launch-results.json) `humanEditingFollowup`），本轮 UIA/键盘步骤为原生输入但不冒充人工输入法。

独立核对脚本与逐项计数见 [windows-simple-editor-4f517a4-results.json](windows-simple-editor-4f517a4-results.json)；本补充 44 份事实副本（4 阶段 × 11 文件）、36 个包内文档全部通过，属冻结 Mac 归档之外的 Windows 补充证据。主执行人已回收引擎核对产物并逐文件验证。结合上述有效既有证据，Windows A06 通过；离线与双向等要求分别留在对应的 A10/A11/A25/A26，#33 不关闭。
