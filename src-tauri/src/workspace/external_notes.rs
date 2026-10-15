//! Host-authorized independent files. Display paths are never input capabilities.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSaveReceipt {
    pub session: NoteSession,
    pub warning: Option<String>,
}

impl Workspace {
    pub(crate) fn require_current_session(
        &self,
        expected: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        if self
            .session
            .as_ref()
            .map(|session| session.view.session_id.as_str())
            != expected
        {
            return Err(StorageError::new(
                "staleSession",
                "选择文件期间笔记会话已切换，请重新打开文件。",
            ));
        }
        Ok(())
    }

    pub(crate) fn active_session_is_external(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| matches!(session.view.origin, SessionOrigin::External { .. }))
    }

    pub(super) fn editor_read_only_reason(&self, session: &ActiveNoteSession) -> Option<String> {
        if session.preserved_preview || session.external_conflict {
            return session.view.read_only_reason.clone();
        }
        if matches!(session.view.origin, SessionOrigin::External { .. }) {
            return external_read_only_reason(&session.package);
        }
        match &self.library {
            Some(library) => read_only_reason(
                &session.package,
                session.notebook_id.as_deref(),
                &library.notebooks,
            ),
            None => Some("请先选择笔记库。".into()),
        }
    }

    pub(super) fn require_library_session(&self, session_id: &str) -> Result<(), WorkspaceError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new("staleSession", "笔记会话已切换。"));
        }
        if !matches!(session.view.origin, SessionOrigin::Library) {
            return Err(StorageError::new(
                "sessionScope",
                "此操作仅适用于当前笔记库中的笔记。",
            ));
        }
        Ok(())
    }

    /// Only the native file picker (or an explicitly authorized host caller) supplies paths.
    pub fn open_external_note(&mut self, path: PathBuf) -> Result<NoteSession, WorkspaceError> {
        self.ensure_library_transition_allowed()?;
        let resolved = storage::resolve_output_path(&path)?;
        let path = resolved.path().to_path_buf();
        self.validate_external_note_path(&path)?;
        let opened = storage::open_note(&path)?;
        storage::probe_directory(path.parent().expect("resolved file has parent"))?;
        let reason = external_read_only_reason(&opened.package);
        let mut view = note_session_from_opened(&opened.package.document.note_id, &opened, reason);
        set_external_origin(&mut view, &path);
        let witness = storage::versions::lease_observed_version(
            &self.recovery_root,
            &opened,
            &display_path(&path),
            "打开的库外笔记版本",
        )?;
        self.session = Some(ActiveNoteSession {
            notebook_id: opened.package.document.organization.notebook_id.clone(),
            package: opened.package,
            baseline_revision: Some(opened.revision),
            path,
            baseline_witness: Some(witness),
            asset_stash: None,
            preserved_preview: false,
            return_external_path: None,
            external_conflict: false,
            was_writable: view.read_only_reason.is_none(),
            view: view.clone(),
        });
        Ok(view)
    }

    pub fn reopen_external_note(
        &mut self,
        session_id: &str,
    ) -> Result<NoteSession, WorkspaceError> {
        self.require_current_session(Some(session_id))?;
        let current = self.session.as_ref().expect("session checked above");
        let path = match current.view.origin {
            SessionOrigin::External { .. } => Some(current.path.clone()),
            SessionOrigin::PreservedPreview => current.return_external_path.clone(),
            SessionOrigin::Library => None,
        }
        .ok_or_else(|| StorageError::new("sessionScope", "此会话没有已授权的库外原文件。"))?;
        self.open_external_note(path)
    }

    pub fn save_external_note_content(
        &mut self,
        session_id: &str,
        content: NoteContent,
    ) -> Result<ExternalSaveReceipt, WorkspaceError> {
        let session = self.external_session(session_id)?;
        if let Some(reason) = &session.view.read_only_reason {
            return Err(StorageError::new("readOnly", reason.clone()));
        }
        let candidate = candidate_for_content(session, &content)?;
        let path = session.path.clone();
        let baseline = session.baseline_revision.clone();
        if let Err(error) = self.validate_external_note_path(&path) {
            return Err(preserve_draft_after_save_access_failure(
                error,
                session,
                &self.recovery_root,
                &candidate,
            ));
        }
        self.preserve_removed_assets(session_id, &candidate)?;
        let (revision, warning) =
            match storage::save_note(&path, &self.recovery_root, &candidate, baseline.as_deref()) {
                Ok(revision) => (revision, None),
                Err(error) => match storage::open_note(&path) {
                    Ok(published)
                        if error.kind == "postPublishCleanup" && published.package == candidate =>
                    {
                        (
                            published.revision,
                            Some(format!(
                                "笔记已保存并重读验证，保存后清理未完成：{}",
                                error.message
                            )),
                        )
                    }
                    _ => return Err(self.retain_save_conflict(error)),
                },
            };
        let active = self
            .session
            .as_mut()
            .expect("validated session under host mutex");
        active.package = candidate;
        active.baseline_revision = Some(revision.clone());
        active.view.content = content;
        active.view.text = active.package.text_projection();
        active.view.assets = active.package.manifest.assets.clone();
        active.view.revision = Some(revision);
        active.retain_baseline(&self.recovery_root)?;
        Ok(ExternalSaveReceipt {
            session: active.view.clone(),
            warning,
        })
    }

    /// Copy the held editor snapshot, including staged assets, without saving the source.
    pub fn save_external_into_library(
        &mut self,
        session_id: &str,
        notebook_id: &str,
        content: NoteContent,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let source = self.external_session(session_id)?;
        let mut package = if content == source.package.content() {
            source.package.clone()
        } else {
            candidate_for_content(source, &content)?
        };
        let _access = self.acquire_library_access()?;
        self.refresh_library_metadata()?;
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择目标笔记库。"))?;
        if !library.notebooks.contains_key(notebook_id) {
            return Err(StorageError::new("unknownNotebook", "目标笔记本不存在。"));
        }
        ensure_notebook_baseline_current(library, notebook_id)?;
        assign_copy_identity(&mut package, notebook_id);
        let note_id = package.document.note_id.clone();
        let library_path = library.path.clone();
        let path = library_path
            .join("notes")
            .join(format!("{note_id}.cuenote"));
        let revision = storage::save_note(&path, &self.recovery_root, &package, None)?;
        let staged = self.stage_open_note(&note_id)?;
        if staged.baseline_revision.as_deref() != Some(revision.as_str())
            || staged.package != package
        {
            return Err(StorageError::new(
                "publish",
                "副本回读与请求不一致；原文件和当前草稿仍保留。",
            ));
        }
        self.persist_preference(&library_path, Some(&note_id), &self.preferences)?;
        let library = self
            .refresh_selected_library()?
            .ok_or_else(|| StorageError::new("noLibrary", "目标笔记库不可用，当前草稿仍保留。"))?;
        let session = staged.view.clone();
        self.session = Some(staged);
        self.last_note_id = Some(note_id);
        Ok(SaveReceipt { session, library })
    }

    fn external_session(&self, session_id: &str) -> Result<&ActiveNoteSession, WorkspaceError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "没有打开的库外笔记。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new("staleSession", "笔记会话已切换。"));
        }
        if !matches!(session.view.origin, SessionOrigin::External { .. }) {
            return Err(StorageError::new(
                "sessionScope",
                "此操作仅适用于直接打开的库外笔记。",
            ));
        }
        Ok(session)
    }

    fn validate_external_note_path(&self, path: &Path) -> Result<(), WorkspaceError> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cuenote"))
        {
            return Err(StorageError::new(
                "format",
                "请选择 .cuenote 原生笔记文件。",
            ));
        }
        let recovery = resolve_future_path(&self.recovery_root)?;
        let config = resolve_future_path(&self.config_root)?;
        let parent = path
            .parent()
            .ok_or_else(|| StorageError::new("unsafePath", "笔记路径没有父目录。"))?;
        if path.starts_with(&recovery)
            || recovery.starts_with(parent)
            || path.starts_with(&config)
            || self
                .library
                .as_ref()
                .is_some_and(|library| path.starts_with(&library.path))
            || backups::configured_backup_directory(&self.config_root)?
                .is_some_and(|directory| path.starts_with(directory))
        {
            return Err(StorageError::new(
                "unsafePath",
                "库外笔记必须位于当前笔记库、应用设置、恢复和备份目录之外。",
            ));
        }
        // Managed library/backup trees must retain their restore and immutability guards.
        if path
            .ancestors()
            .skip(1)
            .any(|ancestor| ancestor.join("notes").is_dir() && ancestor.join("notebooks").is_dir())
        {
            return Err(StorageError::new(
                "unsafePath",
                "此文件位于笔记库或备份快照中。请打开对应笔记库，或先另存为独立文件。",
            ));
        }
        Ok(())
    }
}

pub(super) fn external_read_only_reason(package: &NotePackage) -> Option<String> {
    if package.document.organization.trashed_at.is_some() {
        Some("此笔记带有回收站标记，可另存独立副本到笔记库后编辑。".into())
    } else if !supports_editable_content(&package.content()) {
        Some("此笔记包含当前编辑器尚不支持的内容，当前以只读方式打开。".into())
    } else {
        None
    }
}

pub(super) fn set_external_origin(view: &mut NoteSession, path: &Path) {
    view.origin = SessionOrigin::External {
        path: display_path(path),
    };
    view.notebook_id = None;
    view.pinned = false;
    view.trashed_at = None;
}
