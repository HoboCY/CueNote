# OneDrive 顺序双端同步复核 · cf350d8（issue-33）

2026-10-18 在 Windows 与 Mac 真机，以 cf350d8 普通候选应用按 Windows→Mac→Windows→Mac 顺序编辑同一篇笔记。各端保存只读快照，Mac 证据再经 SCP 回收；传输完成以接收端实际文件和应用读取为证。快照仅用于校验，未注入同步库。快照时间是各机记录时间，不是云传输完成的精确时间；两机时钟未校准。

## 候选与端点

- Windows：`c40e70235e3b84ad843fe8a223eb4621630c5bf8f5a2063d97e1feb002ace038`（`D:\Apps\CueNote\cuenote.exe`，pid 99764）；与 `docs/device-acceptance-results.json` 的 `previousOrdinaryCandidates 中保留的 cf350d8 记录` 及当前视图 `inspection.json` 一致。
- Mac：`a672311c6baef35c63f2317b19c5ea1c2c5a5d4ab41bf5ace04318a2b7f8c3e8`（`/Applications/CueNote.app/Contents/MacOS/cuenote`，pid 40427）；与 `previousOrdinaryCandidates 中保留的 cf350d8 记录` 及两个 Mac 视图 `summary.json` 一致。
- 端点路径：Windows `E:\OneDrive\CueNote-Acceptance-20261018\library-cf350d8`；Mac `/Users/hobo/Library/CloudStorage/OneDrive-个人/CueNote-Acceptance-20261018/library-cf350d8`。
- 笔记 id `d2732090-8d20-4a37-9177-e132d7018126`；两个笔记本文件全程不变。

## 阶段与哈希序列

| 阶段 | 端 | 快照 UTC | 笔记字节 | 笔记 SHA-256（前 12） | document.json | 关键内容 |
| --- | --- | --- | --- | --- | --- | --- |
| S1 windows-rich-baseline | Win | 11:58:02Z | 5893 | `bb42de5e3eff` | `792b6e5b4561` | 基线：标题 `双端同步验收 · CueNote cf350d8`，WIN-SUMMARY-001，导图根 `双端知识地图`，宽 360 |
| S2 mac-received-windows | Mac | 12:04:08Z | 5893 | `bb42de5e3eff` | `792b6e5b4561` | 与 S1 完全一致（传输） |
| S3 mac-title-written-v2 | Mac | 12:10:34Z | 5890 | `d9a2a6bfb94a` | `fec21d6a4889` | 标题改为 `双端同步验收 · Mac 已接续` |
| S4 windows-received-mac-title | Win | 12:15:22Z | 5890 | `d9a2a6bfb94a` | `fec21d6a4889` | 与 S3 完全一致（传输） |
| S5 mac-content-written | Mac | 12:22:59Z | 5919 | `b783e0b3a463` | `ee5427be372e` | 总结区 `MAC-SUMMARY-002：Mac 已接续，图片、公式、代码和导图均可见。`，导图根 `双端知识地图 · Mac 续写`，宽 360 |
| S6 windows-received-mac-content | Win | 12:22:58Z | 5919 | `b783e0b3a463` | `ee5427be372e` | 与 S5 完全一致（传输） |
| S7 windows-image-width-written | Win | 12:24:28Z | 5919 | `2745e7db5197` | `1f183463ef3c` | 图片宽度 360 → 420 |
| S8 mac-received-width | Mac | 12:24:56Z | 5919 | `2745e7db5197` | `1f183463ef3c` | 与 S7 完全一致（传输），最终态 |

完整哈希：

- 笔记文件：`bb42de5e3eff311baa923d50e1a15671b2c2eb2dbbcadba0e60d376f987dc004`（S1、S2）；`d9a2a6bfb94a4e02bc75f42348a74db2f1a926d3f400a993c9e7556b8099cad4`（S3、S4）；`b783e0b3a463c361f25b473d28a7258ea0a73a7bc662aeff2b103037f1fa605c`（S5、S6）；`2745e7db51978b1dc780bb82db21441032a06f5464468579feaa855d8d98a728`（S7、S8）。
- `document.json` 条目：`792b6e5b4561c391c4f147580f8bf58e5df2b913ee111f3da432e835f182adda`（S1、S2，1590 B）；`fec21d6a488991e4900008f4b5c220d1b94df16f84ba9d5ccbc6efa0a7d9ce06`（S3、S4，1588 B）；`ee5427be372ea1a246ce554a08acbed5dd5c527725b19eb44da69b30716907cf`（S5、S6，1630 B）；`1f183463ef3cf9dcb8659efca7777ea034f338952053a38d30efd0c38924f023`（S7、S8，1630 B）。
- 资产条目恒定：`assets/manifest.json` `253e6f8ea6818d22a401636223b1aba978cb3cf0ab382d3dcfc8d406ce260532`（219 B）；`assets/264faec3-1fb7-4d3e-9551-f9a265c3f0a1` `48f5e3c6b7717d06b0996cf5432b7622e0ba989456e70c5733f21aaf13c443c5`（4329 B）。

## 实际比较结果

- 源记录 SHA 与拷贝字节：24/24（8 个快照 × 3 个文件）全部匹配，字节数一致。
- 原生 ZIP：8 个阶段的条目集合相同且均为 3 条：`document.json`、`assets/manifest.json`、`assets/264faec3-1fb7-4d3e-9551-f9a265c3f0a1`。
- 逐字段比较 7 组相邻阶段，共 7 处变化：S1→S2、S3→S4、S5→S6、S7→S8 为 0 差异；S2→S3 仅 `title` 与 `timestamps.contentModifiedAt`；S4→S5 仅导图根节点文本、总结区首段文本与 `contentModifiedAt`；S6→S7 仅 `sections.content[2].displayWidthPx` 360→420 与 `contentModifiedAt`。
- 其余字段全程不变：noteId、schemaVersion、organization（notebookId、pinned、trashedAt）、createdAt、organizationModifiedAt、线索区 WIN-CUE-001、正文 WIN-CONTENT-001（含中文标点与 emoji）、image assetId、mapId/rootId 与子节点 id、子节点文本、代码 language 与源码（含制表符、行首空格）、公式 latex。跨哈希阶段为逐字段等价，同哈希阶段为逐字节等价。
- `contentModifiedAt` 依次为 11:57:25.341Z、12:09:53.296Z、12:22:26.851Z、12:24:26.206Z，属正常保存时间戳变化。
- `document.json` 解析结果与各 `snapshot.json` 内 `documents[0]` 逐一相等（8/8）。

## GUI 证据（本次复核只读 AX/摘要与哈希；截图由主执行人目视）

- `mac-cf350d8-title-roundtrip-v2-view`：pid 40427、应用 SHA `a672311c…`、截图 SHA `dda3e788…` 与记录一致；AX 中标题编辑框与列表均为 `双端同步验收 · Mac 已接续`，图片宽 360，导图根仍为 `双端知识地图`；列表行时间 20:09、摘要仍为 WIN-SUMMARY-001。
- `mac-cf350d8-summary-scalar-view`：同一 pid 与应用 SHA，截图 SHA `613c65ae…` 与记录一致；AX 显示总结区为 `MAC-SUMMARY-002…`、导图根为 `双端知识地图 · Mac 续写`、子节点未变、图片宽 360。
- `windows-cf350d8-current-view`：普通候选 pid 99764、exe SHA `c40e7023…`；辅助功能树显示已渲染图片（ControlType.Image 361×163）、新导图根与子节点、代码（含制表符与行首空格）、公式按钮、总结区 `MAC-SUMMARY-002…`，保存按钮「已保存到本地」为禁用；`window.png` 73123 B，本机复核 SHA `6eb51342…`（捕获侧未记期望值）。
- 主执行记录：三处截图目视确认图片、公式、代码与导图渲染正常；Windows 在无未保存草稿时自动刷新，所见一致。独立复核读取了 AX 和哈希，没有重复目视检查。
- Mac 两个视图的 `summary.json` 明确记录 `q51Claimed: false`、clipboard/ime 为 false；Windows 的 `inspection.json` 记录普通安装版窗口及无观测端点/业务 IPC 的范围。三个视图均不作为 Q51、人工 IME 或外部程序剪贴板证据。

## 保留的失败助手尝试

- 补充快照 `mac-sync-cf350d8-partial-unicode`（12:19:05Z，不在指定 8 阶段内；其笔记 SHA `70b5d9d1…`、5851 B 已复核与记录匹配）：总结区仅写入 `M`，`contentModifiedAt` 12:18:19.812Z。
- 捕获记录：多字符 CGEvent 只把 `M` 写入富文本；改用逐字符 CGEvent 后保存出精确总结区文字；标题与文本域多字符输入正常。判为测试助手限制，不是产品 IME 缺陷证据。
- 后续 S3、S5 快照确认标题与总结区最终正确。

## 子步骤与边界

- Mac 初始占位符保护 → Finder「始终保留」→ 下载完成，仅记录为同步前置子步骤，不构成验收。
- 不主张完整 A10/A11/A06/A08 或 Q51；不含人工 IME、外部程序剪贴板、离线/冲突、9760cf6 验证。
- 未覆盖完整矩阵、性能、交互安装/升级/卸载等；相关未决项以 `docs/device-acceptance-results.json` 为准，device 验收文档由主任务维护。
