# Windows A09 自动保存与关闭保护核验报告（4f517a4）

结论先行：限定证据范围内 164 项断言、162 通过、0 失败、2 未测；**WindowsA09 不能标全通过**——仍缺“明确冲突补测”和“可观察的未保存关闭完成路径”。

## 1. 范围与输入

- 证据根（只读，未做任何原地更改）：`C:\Users\Hobo\.codex\visualizations\2026\10\10\01a12393-3b57-76e3-afff-50bd8ef5f539\implementation-execution\issue-33`
- 14 个快照目录：`windows-editor-4f517a4-a09-{initial,new,idle,switched,returned,wait,newer,switch-wait,switch-cancelled,close-wait,close-cancelled,latest-saved,occupied-failed,reopened}`（实际列出的 14 个）。
- 被测程序：普通安装 exe SHA-256 `ed2391ed6cd1c050d7929631d574dab85636a00959967904e3539568997a1a68`；源码 `4f517a4d1461bec164ea92061fb4b596c7572dce`。13 个阶段 `inspection.json` 均为该 exe、PID 39092。
- 补充证据位于 `…\mac-management-4f517a4`：`windows-a09-*.json` 输入计划、`windows-a09-real-lock\{acquired,released}.json`、`windows-a09-occupied-receipt.json`、`hold-windows-a09-lock.ps1`、`windows-a09-occupied-original.ps1` 与修正后的 `windows-a09-occupied.ps1`、`windows-a09-reopened-uia-readonly.json` 与 `windows-a09-reopened-uia-verification.json`。
- 冻结原文：停止输入约 1 秒后触发自动保存；切换和关闭先处理待保存；保存快照之后新输入仍为待保存；失败/冲突不显示已保存；取消关闭保留草稿。未添加离线/OneDrive 等额外 A09 门槛（UIA 中的 OneDrive 提示文案仅存在、不参与判定）。

## 2. 方法与产物

- 断言脚本（只读证据）：`C:/Users/Hobo/.codex/visualizations/2026/10/10/01a12393-3b57-76e3-afff-50bd8ef5f539/implementation-execution/issue-33/mac-management-4f517a4/windows-a09-verifier/verify-windows-a09-4f517a4.ps1`
- 机器结果：同目录 `verify-windows-a09-4f517a4.results.json`；日志 `verify-windows-a09-4f517a4.log`。
- 方法：用 .NET `System.IO.Compression` 独立打开全部 `.cuenote` ZIP，读取 `document.json` 与 `assets/manifest.json` 并重算资产 SHA-256/字节；重算 `native-snapshot.json` 列出的全部文件 SHA/字节；逐项断言 `accessibility.json`（阶段 UIA）与只读 UIA。快照与 ZIP 的 `contentModifiedAt` 存在尾零格式差异（如 `…32.15Z` 对 `…32.150Z`），按瞬时比较。

## 3. 阶段事实（磁盘/内存/UIA）

| 阶段 | 磁盘标记 | note SHA(前8) | UIA 内存标记 | UIA 状态 |
|---|---|---|---|---|
| initial | — | — | — | 9 notes / 2 books，无测试笔记 |
| new | 空 | 325ad05b | — | 10 notes / 2 books，测试笔记新建 |
| idle | 001 | be922eaf | 001 | 已保存到本地 |
| switched | 001,002 | 0a15938f | 列表摘要 001,002 | 已切到另一篇笔记；返回后另行核对 |
| returned | 001,002 | 0a15938f | 001,002 | 已保存到本地 |
| wait | 001,002 | 0a15938f | 001,002,003 | 正在保存…，无“已保存到本地” |
| newer | 001,002 | 0a15938f | 001,002,003,004 | 正在保存…，无“已保存到本地” |
| switch-wait | 001,002 | 0a15938f | 001,002,003,004 | 正在保存…＋可取消切换提示 |
| switch-cancelled | 001,002 | 0a15938f | 001,002,003,004 | 取消后保稿（编辑区仍见 003/004） |
| close-wait | 001,002 | 0a15938f | 001,002,003,004 | 正在保存…＋可取消关闭提示（关闭 CueNote） |
| close-cancelled | 001,002 | 0a15938f | 001,002,003,004 | 取消后保稿（编辑区仍见 003/004） |
| latest-saved | 001…004 | c3015d10 | 001…004 | 已保存到本地 |
| occupied-failed | 001…004 | c3015d10 | 001…005 | “保存失败，请保留当前内容”＋“os error 32”，无“已保存到本地” |
| reopened | 001…005 | 5be26531 | （只读 UIA 五标记齐全） | 已保存到本地（重开后） |

- 六个 waiting 阶段（wait/newer/switch-wait/switch-cancelled/close-wait/close-cancelled）原件 SHA 均等于 returned 的 `0a15938f…`（保持 002）；内存新输入 003（wait）/004 在可见编辑文本节点（bounds 791,699）中可见。
- 测试笔记 `c8e46c7c-5699-4e9e-a2da-4f664aba5c21`（标题 `Windows 自动保存与关闭保护 · 4f517a4`）为原生新建；initial 9 notes/2 books → new 后 10 notes/2 books。其余 9 篇笔记和 2 个笔记本，共 11 个文件，在全部 14 阶段 SHA 不变。
- `reopened/history` 5 份快照：`325ad05b`（空）、`be922eaf`（001）、`0a15938f`（002）、`bb1482fc`（001–003，**第一份真实在途保存快照，无004**，modified 03:55:57.265Z）、`c3015d10`（001–004）；无任何含 005 的历史。`reopened/failed-drafts/draft-96bf44b8…` 为完整 001–005（03:57:27.053Z）；reopened 原件为 001–005（03:57:52.338Z）。
- 候选 `.tmp`：`notes/.cuenote-candidate-e3e62f39a3d04a1b96bc71f78b7668ac.tmp`（SHA `e4ec7169…`，844B，完整005）在 occupied-failed 与 reopened 中均原样保留；已单独记录，**不计为原生笔记**（两阶段均 notes=10 且 `.cuenote`=10）。
- 占用保护：`occupied-failed` 时原件 SHA 仍为 004（`c3015d10`），receipt `sourceShaBefore=sourceShaAfter=c3015d10…`、`released=true`；锁 `acquired`（pid 50200，selfCheckBlocked）→ `released`（explicitRelease）→ 占用开始 → 占用结束，时序 03:55:42 < 03:56:47 < 03:57:25 < 03:57:28。
- 输入计划审计（`windows-a09-*.json`，排除 first-launch/receipt）：无任何 key code 83（Ctrl+S）——idle 为文本 001＋wait 1600ms，switch 为文本 002＋点击“打开笔记”，close-wait 为 Alt+F4（115/18），cancel 计划点击“取消切换”/“取消关闭”。
- 只读 UIA：`windows-a09-reopened-uia-verification.json` PID 48204、`foregroundClaimed=false`、`readOnlyAccessibility=true`、`loadedAllFiveInputMarkers=true`；只读 UIA 原文含全部五个标记。reopened 目录仅有 native-snapshot＋failed-drafts/history，无 accessibility.json/inspection.json/window.png（无前台截图）。

## 4. 局限（必读）

1. **明确冲突尚未补测**：限定输入内没有冲突场景证据，本报告不宣称该项通过；WindowsA09 不能标全通过。
2. **可观察的未保存关闭完成路径未建立**：Alt+F4 计划返回 foreground guard 错误，此后程序退出、005 在 03:57:52.338Z 落盘；日志无法严格区分“自动化请求的关闭”与其他关闭，故不计正常退出码、不主张关闭因果；reopened 只证明完整重开（PID 48204 只读 UIA）。`a09-failed-close` 没有成功目录，不当通过。
3. 占用脚本首次使用精确 `保存失败` 断言，真实文案带后缀（“保存失败，请保留当前内容”），断言在占用捕获后提前结束并由 finally 释放句柄；原始脚本保留，修正断言（`保存失败*`）仅对现有 UIA 自检通过（现有 UIA 匹配 2 处），**未重跑以避免额外输入**。
4. `windows-a09-lock.stdout/stderr` 均为 0 字节；只读 UIA 为普通 Explorer 启动（PID 48204）并声明 `foregroundClaimed=false`，未操控桌面/前台。
5. 本报告未扫描其他证据目录、未运行产品测试、未修改任何证据文件。

## 5. 结论

- 已验证：idle/切换保存、关闭等待与取消保护、waiting 阶段原件保持 002 且内存新输入待保存、取消保稿、失败时不显示已保存且原件不变、候选/失败草稿保留、历史在途快照、完整重开（001–005）、输入计划无 Ctrl+S 替代。
- 未测：明确冲突补测、可观察的未保存关闭完成路径。因此 **WindowsA09 = 部分通过（PASS_WITH_UNTESTED），不得标全通过**；机器可读结果见 [windows-autosave-4f517a4-results.json](windows-autosave-4f517a4-results.json)。
