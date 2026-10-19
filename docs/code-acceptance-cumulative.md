# A26 代码双端验收汇总

2026-10-19，按冻结规格复核已有普通安装版证据，A26 双端通过。该结论复用有效实测记录，不声称重新执行全部步骤；双端完整通过数更新为 15/26，#33 继续开放。

| 冻结要求 | 已核对的证据 |
| --- | --- |
| JavaScript、TypeScript、Python、Rust、C#、SQL、JSON 及未知语言 | [Windows 原生输入与保存](windows-editor-079fa6e.md)、[Mac 原生输入与保存](macos-editor-4f517a4.md)均逐字符核验八种代码。 |
| 高亮、未知语言纯文本 | Mac 原生截图已有目视记录。本轮重新查看 Windows 普通版 `windows-editor-079fa6e-code/window.png`，七种语言的语法着色与未知代码块的纯文本显示可见；没有只凭语言标记推断高亮。 |
| 空行、Tab/空格、中文、HTML、美元、反引号 | 两端编辑记录及包内文档逐项核验，未变为公式或富文本。 |
| 双端重开及语言标记保真 | 两端本机正常重开，以及八语言笔记 `1a7ec137-17d5-43f1-be3f-11f4fbe036c3` 的[真实 OneDrive Mac→Windows→Mac 往返](onedrive-reverse-4f517a4.md)，完整代码、语言和空白保持。Windows 在该笔记修改标题并保存。 |
| Markdown 导出围栏有效 | [Windows 导出](windows-markdown-export-9760cf6.md)和[Mac 导出](macos-markdown-export-4f517a4.md)使用共享全格式合成样本，八语言原文附件、围栏、特殊字符均准确，移动后由独立读取器检查。 |
| 代码与公式源码可搜索 | [Windows 搜索补测](windows-functional-followup-9760cf6.md)与[Mac 管理搜索](macos-management-4f517a4.md)均命中 `console.log` 和 `\frac{1}{3}`；Mac 还验证了代码查询大小写。 |

这些证据覆盖不同阶段和样本：八语言跨端样本从 Mac 发起，导出是预制全格式样本，搜索是既定通用代码与公式查询。冻结规格没有要求每种语言分别搜索，也没有要求所有步骤必须在同一篇笔记完成，因此不追加这些门槛。

复用范围已核对：`079fa6e` 至当前 Windows `3518441` 的生产改动只有待办输入规则、图片焦点和发布记录路径；代码高亮、公式、导出与搜索路径未变。Mac 仍为 `4f517a4`。独立只读任务 `7393b6d2d4164feb8321d2ced91aa9f9` 已完成并读至末页（cursor 210，hasMore=false）；主执行人另核对 Windows 普通版截图、查询记录及源码差异。

A25 仍缺两端实机断网渲染；不因 A26 通过而勾选公式完整项。证据来源和哈希见[结构化结果](code-acceptance-cumulative-results.json)。
