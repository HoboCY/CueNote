# Mac 普通候选编辑器验收补充（4f517a4）

2026-10-19，Mac 单端 A06 通过：普通安装版 `4f517a4` 完成 Q50 正文 Markdown、基础表格增删行列与撤销、行内/块公式、八种带语言代码块、三个分区各自撤销/重做及保存重开核对，中文输入法沿用已确认的人工证据。本记录补齐 [Windows 编辑子步骤](windows-editor-079fa6e.md)的 Mac 对应项；A25/A26 完整项及 #33 仍开放。

候选源码 `4f517a4d1461bec164ea92061fb4b596c7572dce`，Mac 程序 SHA-256 `fc323333171cefbacd3822b1fdd6a99468b8b0a8cb23ad9715c80a22b8243df0`，使用专用库 `/Users/hobo/CueNote-ordinary-acceptance-20261019/library`。45 个动作作业全部由原生键鼠驱动，每步核对前台 PID 与窗口归属；助手作用域明确 `nativeSyntheticInputOnly=true`，`ipc`/`javascript`/`clipboard` 均为 false。正常关闭后经 Launch Services 重开，应用 PID 8712→10951→11538，证明会话重建。

## 正文编辑、表格与独立撤销/重做

新笔记 `c8c8e83c-7997-45f2-bf96-63afc8c8b8f3` 在 `editor-*` 阶段完成：

- 三分区分别输入线索、正文、总结，随后按 总结→线索→正文 依次撤销，再按同序重做。逐步对比确认每次只有目标分区变化，其余分区内容不变，最终三分区与撤销前一致。
- 1–3 级标题（`heading1/2/3`）、加粗/斜体/删除线/行内代码（marks 逐项核对）、链接 `https://example.com`、无序/有序/任务列表、引用、分隔线均从原生 `document.json` 核对。
- 工具栏创建 2×2 表格并填写表头与正文；增行到 3×2、增列到 3×3；删除行、删除列后各直接按 Cmd+Z 恢复为 3×3。每步表头/正文行标记、行列数与单元格文字与预期一致，表格外内容不变。
- 点击复选框把任务变为已勾选；线索区与总结区各自输入 H2 + 加粗 + 无序/有序/任务列表并保存通过。
- 正常关闭重开后，笔记文件与关闭前字节相同（SHA-256 `05d4f50dd79680c764b691dcc94fd41befa9be61c794c92d9480fba8eb0861b0`），三分区、表格与任务状态完整，可继续编辑。

`editor-three-sections`（316）动作计划点击了错误工具栏坐标，未产生任何输入，原生文件与 `editor-title` 逐字节相同。该阶段按“无输入”保留并排除，不计通过也不计缺陷。

## 代码与公式

第二篇笔记 `1a7ec137-17d5-43f1-be3f-11f4fbe036c3`：

- 8 个带语言代码块与 `expected-code-input.json` 逐字一致：javascript/typescript/python/rust/csharp/sql/json/brainfuck；Tab、空格、空行、中文、HTML、`$`、反引号均保留，brainfuck 作为未知语言纯文本处理。首个多行文本动作（376）只插入 JavaScript 第一行，作为不完整输入记录并排除，随后用真实 Enter/Tab 恢复完整源码，无丢字声明。
- 行内公式 9 个节点：8 个有效（分数、根号、上下标、希腊字母、求和、积分、矩阵、对齐）+ 1 个无效命令；无效命令源码保留并显示错误。转义美元 `\$5` 保持文字，行内代码中的 `$not_math$` 未被解析成公式。块公式多行源码与预期逐字符一致。
- 源码框把 Formula 3 由 `x_i^2` 改为 `x_i^3`，撤销恢复 `x_i^2`，重做恢复 `x_i^3`；三步只有目标公式变化，其余内容与改前相同。
- 关闭重开后代码与公式保持，笔记文件与重做阶段字节相同（SHA-256 `9f1d645f1ca40fb9795697ee7dd0da7443f5d288409edc76edd20b3d4af5d3e5`）。`job-401/403/405` 窗口截图为主执行人目视记录（7 种语言高亮、未知语言纯文本、8 条公式与块公式渲染、错误可见且源码保留）；本核对未重新查看截图。

## 完整性与复用边界

冻结根 `management-through-405-4f517a4-evidence` 归档 SHA-256 `ae137beb717d9c552687c093c20871ad9b67e2616d751b30b3599c222b08243b`、73,586,392 字节；清单 4,563 个文件（105,781,055 字节）逐一重算一致。149 份库快照与 3 份库外快照共 1,100 份事实副本、772 个包内文档、248 份资产载荷全部通过（包内文档与快照一致，资产长度与 SHA-256 与清单一致）。

以新增笔记前的最新快照 `export-all-formats-published` 为基线：原有 5 篇笔记（含 `0e4e816b-79d5-43f4-8798-062bfc8d8e38`、`bd4f5a25-2e82-4f66-aa20-f3d08195dcf2`）与 2 个笔记本在全部编辑、代码、公式阶段逐字节不变，文件集合只增加两篇新笔记。

中文组合输入及各分区独立撤销/重做沿用用户对 `9760cf6` 的明确人工确认（[Mac 人工输入](macos-human-input-9760cf6.md)），真实输入法证据有效，不要求重复；`4f517a4` 的源码差异仅为 `src/image-extensions.ts` 及其测试/修复文档，不涉及输入与编辑器路径。用户无需因本次候选重复该人工步骤。

关闭证据只有正常终止请求与进程消失（`closed=true`）；助手退出码 0 不代表应用，`exitStatusAvailable=false`，不声称应用退出码 0。

独立核对脚本与逐项计数为 `verify-editor-405.ps1`、`packages-through405-verified.json`、`verify-editor-405.log`，共 10,448 条断言全部通过。任务 `4d72310325c143128691fd4e7cb57bf8` 已完成并读至末页（cursor 372，hasMore=false）；主执行人已将引擎产物复制到本机 `implementation-execution/issue-33/mac-management-4f517a4/editor-verifier-405`，逐文件校验一致。结构化结果见 [macos-editor-4f517a4-results.json](macos-editor-4f517a4-results.json)。

公式与代码的离线、双向、导出与搜索等完整项归属 A25/A26，不额外增加为冻结 A06 的门槛；A25/A26 与 #33 仍开放。Windows 侧精简补充见 [windows-simple-editor-4f517a4.md](windows-simple-editor-4f517a4.md)。
