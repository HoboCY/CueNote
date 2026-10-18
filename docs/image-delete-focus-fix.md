# 删除图片后的撤销焦点修复

Windows 普通安装版 `079fa6e` 的图片验收复现：先用宽度滑块调整图片，再点击图片自身的“删除”，随后直接按 Ctrl+Z，图片未恢复。辅助功能记录确认焦点在页面 Document；点击正文后再次撤销，原 assetId、97 px 显示宽度和图片内容完整恢复。删除、保存与历史均正常，问题是图片控件被移除后没有把键盘焦点交还内容区。

修复限定于图片节点视图的删除按钮：只有删除命令成功，才同步聚焦正文 EditorView。无效或只读操作沿用原有保护；公开内容命令、文件格式、资产暂存和保存流程未变。

回归从宽度滑块和删除按钮两种焦点起点触发真实节点视图的删除监听器，验证正文获得焦点，并通过真实编辑器历史确认一次撤销完整恢复、重做只删除图片且保留邻接文字。测试在修复前因焦点仍在滑块失败，修复后完整前端 **160/160**、TypeScript 和 Vite 构建通过。节点视图测试中的 DOM 焦点由测试替身记录，不能代替普通安装版原生键盘复验。

原生证据位于仓库外 `implementation-execution/issue-33/windows-editor-079fa6e-image-{deleted,restored,restored-focused}`。本地测试日志为 `package-output/image-delete-focus-red.log`、`image-delete-focus-frontend.log`、`image-delete-focus-build.log`。新版普通安装包 `4f517a4` 的直接 Ctrl+Z / Command+Z 已在两端复验通过，图片身份、宽度和完整内容均恢复，正常退出重开后文件逐字节一致；详见[双端图片与 Mac 侧栏验收](image-block-focus-4f517a4.md)。结合已记录的人工双来源剪贴板步骤，双端 A07 通过。
