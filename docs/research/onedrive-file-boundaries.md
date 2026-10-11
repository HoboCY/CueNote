# CueNote 使用 OneDrive 同步笔记文件时，保存与冲突处理有哪些边界？

研究日期：2026-10-11

范围：微软官方 OneDrive 文档；没有访问用户 OneDrive、安装软件或进行真实同步测试。

本报告支持 [CueNote 使用 OneDrive 同步笔记文件时，保存与冲突处理有哪些边界？](https://github.com/HoboCY/CueNote/issues/9)，并关联 [CueNote 的笔记与附件如何可靠保存、备份并在 Windows/macOS 打开？](https://github.com/HoboCY/CueNote/issues/6)。

## 已确认的产品上下文

- Windows 与 macOS 打开同一种笔记文件；用户用 OneDrive 同步文件。
- 应用管理一个本地笔记库，提供默认且可更改的位置。自动保存约在停止输入 1 秒后发生，也在切换笔记和关闭前执行；界面要显示保存中、已保存或错误。
- 有修改的日子自动生成一份备份，保留最近 30 份，并支持手动备份及恢复前备份；笔记标题可以重复。
- 还未决定一个笔记对应一个文件、一个笔记本对应一个文件，还是整个库采用单一文件。本文不确定扩展名、数据库或包格式。

## 官方文档能支持的结论

1. **同步会传播文件级增、改、删。** OneDrive 文档说明，在同步文件夹中新增、修改或删除文件/文件夹，会反映到 OneDrive 网站，反向变化也会同步；在线时自动同步。[微软：同步电脑的文件和文件夹](https://support.microsoft.com/en-us/onedrive/sync-your-computer-s-files-and-folders-with-onedrive?tabs=macos) 这描述文件状态传播，不说明 CueNote 保存函数已完成，也不构成应用级事务或内容合并承诺。

2. **本机保存和云端上传是两个状态。** OneDrive 图标文档把旋转箭头定义为同步进行中（包括上传），并指出暂停、错误或待处理等状态。[微软：OneDrive 图标含义](https://support.microsoft.com/en-us/onedrive/what-do-the-onedrive-icons-mean) 应用写入成功只能支持“已保存到本机”；除非应用另有可靠状态来源，不能把它显示成“已同步到云端”。

3. **微软的实时合著能力有明确的格式和应用前提。** OneDrive/SharePoint 上的 Office 合著文档列出 .docx、.pptx、.xlsx 等现代格式，并要求支持合著的 Office 应用。[微软：SharePoint/OneDrive 文档协作与合著](https://support.microsoft.com/en-us/sharepoint/get-started-with-sharepoint/document-collaboration-and-co-authoring) 这说明普通文件同步不能被当作任意笔记格式的实时合并功能；它没有说明 CueNote 文件不能放在 OneDrive。

4. **冲突副本行为只在窄范围文档中有明确说明。** 微软的一篇 OneDrive for work or school 同步故障文档称，非 Office 文件冲突时可能同时保留服务器版和本机版，本机副本会带设备名；该文还写到最多保留 5 个冲突版本。[微软：解决 OneDrive for work or school 同步问题](https://learn.microsoft.com/en-us/troubleshoot/sharepoint/sync/troubleshoot-sync-issues) 这是特定账户/同步故障文档，且页面提及旧版 groove.exe 客户端；不能据此承诺个人 OneDrive、当前所有客户端或所有冲突都会保留副本。当前个人账户下 CueNote 文件的精确冲突结果仍未知。

5. **按需文件可能只有云端占位符。** Windows 和 macOS 的 Files On-Demand 都区分仅在线、本机可用和始终保留在设备上的文件；仅在线文件离线时打不开，标记“始终保留在此设备”会下载到本机并占用空间。[Windows 文件按需](https://support.microsoft.com/en-us/onedrive/save-disk-space-with-onedrive-files-on-demand-for-windows) [Mac 文件按需](https://support.microsoft.com/en-us/onedrive/save-disk-space-with-onedrive-files-on-demand-for-mac) 在两台电脑都编辑前，应确认笔记库已完全下载；仅在 Finder/Explorer 中可见不等于文件内容可离线读取。

6. **删除会传播，云端回收站是有条件的恢复渠道。** 同步目录里的删除会同步到 OneDrive；微软称已删除内容可能从 OneDrive 网站回收站恢复，但永久删除后无法恢复。回收站的保留期限因账户类型和设置而异。[微软：还原已删除的 OneDrive 文件](https://support.microsoft.com/en-us/onedrive/restore-your-onedrive-files) 这不等于应用的本地备份：账户类型、回收站期限、同步状态和用户操作都会影响能否恢复。

7. **路径上限因存储层和操作系统而异。** 微软文档列出云端、OneDrive 同步客户端以及 Windows/macOS 本地或应用各自的路径和文件名限制；不同层级的限制并不相同。[微软：文件路径长度限制](https://support.microsoft.com/en-us/onedrive/what-are-file-path-length-limits) 因此不把任一层级的最大值当作 CueNote 的安全长度；浅目录、短名称和跨平台安全字符集更稳妥。

## 对 CueNote 的设计推断（不是微软要求）

- 将“应用本机保存”与“OneDrive 同步”分开表达：保存中、已保存到本机、保存失败属于应用状态；同步进行中、待同步或同步出错应来自 OneDrive 可验证状态。若应用拿不到后者，就不声称云端已同步。
- 把笔记粒度当作冲突影响范围来评估。每笔记一个可独立替换的文件，理论上能把改动范围限制在单篇笔记；单一库文件或共享数据库则会让不同笔记的编辑都写入同一路径。OneDrive 文档没有给 CueNote 文件提供内容级合并、跨设备锁或数据库事务保证，也没有宣称所有数据库文件都禁止同步。
- 附件若分成独立文件，笔记文件和附件可能在不同时间到达；应用需要处理“引用已到、附件未到”等状态。自包含单笔记文件可减少这种多文件依赖，但会带来文件更大等权衡。微软文档没有承诺多文件集合的原子事务，设计时不能依赖笔记与附件同时到达。
- 设计推断：应用保存前比较版本号/哈希可以发现已经发生的外部改动，但比较之后、应用写入之前，本机 OneDrive 客户端仍可能更新本地文件；单次比较不能充当跨设备锁。应用需要检测外部改动、协调本机写入，并保留能够取得的版本供处理，不能承诺恢复应用从未读取到的远端版本。
- 因为笔记标题允许重复，标题不能单独充当唯一文件名。可评估使用稳定内部 ID 作为文件身份，把标题作为内容字段或显示名；这也能避免标题中的跨平台特殊字符直接决定路径。此处是产品设计建议，不是既定规格。

## 可讨论的候选策略

- 用户确认文件粒度后，再定义外部版本变更时的策略：提示重新载入、保留冲突副本供用户比较，或由应用支持的格式合并；不要静默覆盖，且不要依赖 OneDrive 一定创建冲突副本。
- 将“已保存”限定为本机写入已成功；把 OneDrive 上传确认作为独立状态。若启动时发现笔记库正在按需下载或无法读取，给出清晰错误并阻止覆盖空数据。
- 应用备份的保存位置和 OneDrive 同步位置应明确。若备份也放在同一个同步树内，目录删除可能连同备份一起传播；独立备份位置可降低这一风险，但跨设备可用性需要另行决定。
- 笔记身份优先使用稳定 ID；对标题和附件路径设置跨平台安全规则，并避免过深目录。具体字符/长度限制需在产品文件格式确定后落成。

- 对 OneDrive 占位文件先验证内容是否已下载、当前是否可读；不能把未下载状态当成空白笔记，也不能据此覆盖原文件。
- 外部版本变化而本机编辑仍未保存时，保留内存编辑与磁盘版本，再让用户处理；检测失败不应静默覆盖。
- 约 1 秒空闲自动保存会频繁写入活跃文件；确定文件粒度后，要验证同步进行中与同时编辑时的写入交互。
- 不论选择笔记文件还是库文件，都需验证异常中断后重开是否能读取、恢复最近一次本机成功保存的内容。
## 仍需实际验证

1. 同一 OneDrive 账户下，Windows 保存后观察同步图标，再在 Mac 打开同一笔记；反向重复，并记录本机保存完成与云端出现的时间差。
2. 同一笔记在两台设备离线编辑，再分别联网；比较文件、应用状态和 OneDrive 实际冲突结果。分别测试个人账户与工作/学校账户（若产品范围包含两者）。
3. 两设备同时改不同笔记，再同时改同一笔记；检验所选文件粒度是否隔离冲突，以及应用能否检测较晚到达的外部版本。
4. 把库设为仅在线、部分本机可用和始终保留三种状态，测试启动、离线打开、保存、切换和关闭行为。
5. 从另一设备或 OneDrive 网站删除、改名、移动文件，并在应用仍开着时观察；验证应用备份、回收站恢复和打开中的编辑之间的交互。
6. 用重复标题、含特殊字符标题、长标题、长路径和带图片/思维导图的笔记，在 Windows 与 macOS 间双向同步。验证两端都能解析同一种格式，且不发生静默跳过或损坏。

没有执行以上现场测试；结果仍取决于 CueNote 文件粒度、OneDrive 账户类型、客户端版本和实际设备设置。

## 官方来源（均于 2026-10-11 查阅）

1. [同步电脑的文件和文件夹](https://support.microsoft.com/en-us/onedrive/sync-your-computer-s-files-and-folders-with-onedrive?tabs=macos)
2. [OneDrive 图标含义](https://support.microsoft.com/en-us/onedrive/what-do-the-onedrive-icons-mean)
3. [文档协作与合著](https://support.microsoft.com/en-us/sharepoint/get-started-with-sharepoint/document-collaboration-and-co-authoring)
4. [解决 OneDrive for work or school 同步问题](https://learn.microsoft.com/en-us/troubleshoot/sharepoint/sync/troubleshoot-sync-issues)
5. [Windows 文件按需](https://support.microsoft.com/en-us/onedrive/save-disk-space-with-onedrive-files-on-demand-for-windows)
6. [Mac 文件按需](https://support.microsoft.com/en-us/onedrive/save-disk-space-with-onedrive-files-on-demand-for-mac)
7. [还原已删除的 OneDrive 文件](https://support.microsoft.com/en-us/onedrive/restore-your-onedrive-files)
8. [文件路径长度限制](https://support.microsoft.com/en-us/onedrive/what-are-file-path-length-limits)
