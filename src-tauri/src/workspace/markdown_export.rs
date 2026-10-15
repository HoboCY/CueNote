use super::*;
mod mind_map_preview;
mod render;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownExportReceipt {
    pub directory: String,
    pub markdown_path: String,
    pub attachment_count: usize,
}

impl Workspace {
    /// The host authorizes a fresh destination directory. Export a cached
    /// snapshot without flushing autosave or mutating the native session.
    pub fn export_current_markdown_bundle(
        &self,
        session_id: &str,
        content: NoteContent,
        path: PathBuf,
    ) -> Result<MarkdownExportReceipt, WorkspaceError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可导出的笔记。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换，请重新导出。",
            ));
        }
        let original = session.package.content();
        let package = if original == content {
            session.package.clone()
        } else {
            if !supports_editable_content(&original) {
                return Err(StorageError::new(
                    "readOnly",
                    "不支持修改当前只读结构；未导出。",
                ));
            }
            candidate_for_content(session, &content)?
        };
        let destination = storage::resolve_output_path(&path)?;
        let target = destination.path();
        for protected in [
            Some(resolve_future_path(&self.config_root)?),
            Some(resolve_future_path(&self.recovery_root)?),
            self.library.as_ref().map(|library| library.path.clone()),
            backups::configured_backup_directory(&self.config_root)?,
        ]
        .into_iter()
        .flatten()
        {
            if target.starts_with(&protected) || protected.starts_with(target) {
                return Err(StorageError::new(
                    "unsafePath",
                    "导出目标必须位于笔记库、应用设置、恢复和备份目录之外。",
                ));
            }
        }
        if target
            .ancestors()
            .skip(1)
            .any(|ancestor| ancestor.join("notes").is_dir() && ancestor.join("notebooks").is_dir())
        {
            return Err(StorageError::new(
                "unsafePath",
                "请将 Markdown 导出到笔记库和备份快照之外。",
            ));
        }
        let files = render::document(&package)?;
        storage::publish_export(&destination, &files)?;
        Ok(MarkdownExportReceipt {
            directory: display_path(target),
            markdown_path: display_path(&target.join("note.md")),
            attachment_count: files.len() - 1,
        })
    }
}
