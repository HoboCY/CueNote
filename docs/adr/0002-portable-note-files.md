---
status: accepted
---

# 用单篇完整文件保存笔记，将本机索引与同步数据分开

2026-10-14，CueNote 确定以 `.cuenote` ZIP 保存一篇笔记的 UTF-8 结构化内容、图片资产和可编辑导图，首版使用统一格式版本 1；笔记身份独立于标题和路径，笔记本元信息单独存放，搜索索引与设备偏好是本机派生状态。这个取舍让 Windows/macOS 可直接打开同一篇完整笔记，并由用户将事实文件交给 OneDrive 同步；代价是保存需重写整包，不能依靠 Markdown 导出恢复全部编辑语义，也不能把多个文件视为跨设备事务。

目录能力检测、候选包校验、版本检查和平台原子发布共同保护保存；不支持所需操作的目录必须提示更换，失败保留草稿及原文件，不退化成直接覆盖。遇到不支持的新版格式时提示升级并禁止覆盖。2026-10-18 的[普通用户启动对照](../windows-user-launch-results.json)未复现工具环境中伴随 AppData 重定向出现的错误 17，基础保存重开通过；没有因此放宽文件发布规则。Mac/OneDrive 的完整实机验收仍见[验收记录](../device-acceptance.md)。

产品行为以 [保存规则](https://github.com/HoboCY/CueNote/issues/6#issuecomment-6116499938) 为准；具体架构、历史/冲突副本与备份恢复分工见 [整体技术方案的解决结论](https://github.com/HoboCY/CueNote/issues/7#issuecomment-6164489548)。
