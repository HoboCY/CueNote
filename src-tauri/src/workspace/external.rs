//! File notifications are hints. Session replacement happens only after the
//! frontend has settled editors, stopped writes, and captured the final draft.
use super::*;
use crate::storage::versions;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalProbe {
    pub session_id: Option<String>,
    pub status: String,
    pub revision: Option<String>,
    pub message: Option<String>,
    pub library: Option<LibraryView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalReconciliation {
    pub status: String,
    pub session: Option<NoteSession>,
    pub library: LibraryView,
    pub message: Option<String>,
}

impl Workspace {
    pub(super) fn retain_save_conflict(&self, mut error: WorkspaceError) -> WorkspaceError {
        if !matches!(error.kind.as_str(), "conflict" | "publishUncertain") {
            return error;
        }
        if let Some(active) = &self.session {
            if let Some(revision) = &active.baseline_revision {
                let held = OpenedNote {
                    package: active.package.clone(),
                    revision: revision.clone(),
                };
                if let Err(preservation) = versions::preserve_conflict_version(
                    &self.recovery_root,
                    &held,
                    &display_path(&active.path),
                    "保存冲突前的编辑基线",
                ) {
                    let detail = format!(
                        "编辑基线仍在已有版本记录中，但无法标记为冲突：{}",
                        preservation.message
                    );
                    error.recovery_error = Some(match error.recovery_error.take() {
                        Some(existing) => format!("{existing}\n{detail}"),
                        None => detail,
                    });
                }
            }
        }
        error
    }

    pub fn probe_external_changes(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<ExternalProbe, WorkspaceError> {
        if self
            .session
            .as_ref()
            .map(|session| session.view.session_id.as_str())
            != session_id
        {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换，已忽略旧的外部变化核对。",
            ));
        }
        let library = self.refresh_external_library()?;
        let mut probe = ExternalProbe {
            session_id: session_id.map(str::to_owned),
            status: "unchanged".into(),
            revision: None,
            message: None,
            library,
        };
        let Some(session) = self.session.as_ref() else {
            return Ok(probe);
        };
        if session.preserved_preview {
            return Ok(probe);
        }
        match storage::open_note(&session.path) {
            Ok(opened) => {
                versions::preserve_observed_version(
                    &self.recovery_root,
                    &opened,
                    &display_path(&session.path),
                    "核对到的文件版本",
                )?;
                probe.revision = Some(opened.revision.clone());
                if session.baseline_revision.as_deref() != Some(opened.revision.as_str()) {
                    probe.status = "changed".into();
                }
            }
            Err(error) => {
                let missing = fs::symlink_metadata(&session.path)
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
                if missing && session.baseline_revision.is_none() {
                    return Ok(probe);
                }
                probe.status = if missing { "missing" } else { "unreadable" }.into();
                probe.message = Some(error.message);
            }
        }
        Ok(probe)
    }

    pub fn reconcile_external_changes(
        &mut self,
        session_id: &str,
        content: NoteContent,
    ) -> Result<ExternalReconciliation, WorkspaceError> {
        let active = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "没有需要核对的笔记。"))?;
        if active.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换，未替换当前内容。",
            ));
        }
        if active.preserved_preview {
            return self.external_result("unchanged", None, None);
        }
        let path = active.path.clone();
        let origin = display_path(&path);
        let current = storage::open_note(&path);
        let same_revision = match &current {
            Ok(opened) => active.baseline_revision.as_deref() == Some(opened.revision.as_str()),
            Err(_) => {
                active.baseline_revision.is_none()
                    && fs::symlink_metadata(&path)
                        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            }
        };
        let dirty = content != active.package.content();
        if same_revision && !dirty && !active.external_conflict {
            return self.external_result("unchanged", None, None);
        }

        let candidate = if dirty {
            Some(candidate_for_content(active, &content)?)
        } else {
            None
        };
        let held = active
            .baseline_revision
            .as_ref()
            .map(|revision| OpenedNote {
                package: active.package.clone(),
                revision: revision.clone(),
            });
        let identity_matches = current
            .as_ref()
            .is_ok_and(|opened| opened.package.document.note_id == active.view.note_id);
        let conflict = dirty || !identity_matches || active.external_conflict;

        // Preserve data from the exact reads/held package, not a later reread of
        // a path which a non-cooperating writer may already have changed again.
        if let Some(held) = &held {
            if conflict {
                versions::preserve_conflict_version(
                    &self.recovery_root,
                    held,
                    &origin,
                    "外部变化前的编辑基线",
                )?;
            } else {
                versions::preserve_observed_version(
                    &self.recovery_root,
                    held,
                    &origin,
                    "外部刷新前的版本",
                )?;
            }
        }
        if let Some(candidate) = candidate {
            let draft = storage::create_recovery_snapshot(&self.recovery_root, &candidate)?;
            let opened = storage::open_note(&draft.path)?;
            versions::preserve_conflict_version(
                &self.recovery_root,
                &opened,
                &origin,
                "外部变化时的本地草稿",
            )?;
        }
        if let Ok(opened) = &current {
            if conflict {
                versions::preserve_conflict_version(
                    &self.recovery_root,
                    opened,
                    &origin,
                    "检测到的外部版本",
                )?;
            } else {
                versions::preserve_observed_version(
                    &self.recovery_root,
                    opened,
                    &origin,
                    "外部刷新的新版本",
                )?;
            }
        }

        if conflict {
            let message = match &current {
                Ok(_) if !identity_matches => {
                    "文件的笔记身份已变化。已保留取得的版本，请在历史与冲突中选择或整理。"
                        .to_owned()
                }
                Ok(_) => "外部文件与本地草稿发生冲突。已分别保留版本，请在历史与冲突中选择或整理。"
                    .to_owned(),
                Err(error) => format!(
                    "外部文件已删除或暂不可读，当前内容仍保留；没有建立空白文件。{}",
                    error.message
                ),
            };
            let active = self.session.as_mut().expect("session checked above");
            active.view.read_only_reason = Some(message.clone());
            active.external_conflict = true;
            return self.external_result("conflict", None, Some(message));
        }

        let opened = current.expect("a clean refresh requires a readable matching identity");
        let baseline_witness = versions::lease_observed_version(
            &self.recovery_root,
            &opened,
            &origin,
            "外部刷新的新版本",
        )?;
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "当前没有笔记库。"))?;
        let notebook_id = opened.package.document.organization.notebook_id.clone();
        let reason = read_only_reason(&opened.package, notebook_id.as_deref(), &library.notebooks);
        let view = note_session_from_opened(&opened.package.document.note_id, &opened, reason);
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package: opened.package,
            asset_stash: None,
            baseline_revision: Some(opened.revision),
            notebook_id,
            baseline_witness: Some(baseline_witness),
            preserved_preview: false,
            external_conflict: false,
        });
        self.external_result(
            "refreshed",
            Some(view),
            Some("已保留旧版并载入外部更新。".into()),
        )
    }

    pub fn list_preserved_versions(&mut self) -> Result<VersionList, WorkspaceError> {
        versions::list_versions(&self.recovery_root)
    }

    pub fn open_preserved_version(
        &mut self,
        version_id: &str,
    ) -> Result<NoteSession, WorkspaceError> {
        let preserved = versions::open_version(&self.recovery_root, version_id)?;
        let opened = preserved.opened;
        let baseline_witness = if preserved.summary.conflict {
            None // Conflict records are never automatically rotated.
        } else {
            Some(versions::lease_observed_version(
                &self.recovery_root,
                &OpenedNote {
                    package: opened.package.clone(),
                    revision: preserved.summary.source_revision.clone(),
                },
                &preserved.summary.origin,
                "正在预览的保留版本",
            )?)
        };
        let view = note_session_from_opened(
            &opened.package.document.note_id,
            &opened,
            Some("这是保留版本的只读预览。可恢复为新笔记或另存完整副本。".into()),
        );
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path: preserved.path,
            notebook_id: opened.package.document.organization.notebook_id.clone(),
            package: opened.package,
            asset_stash: None,
            baseline_revision: Some(opened.revision),
            baseline_witness,
            preserved_preview: true,
            external_conflict: false,
        });
        Ok(view)
    }

    pub fn restore_preserved_as_copy(
        &mut self,
        version_id: &str,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let source = versions::open_version(&self.recovery_root, version_id)?;
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择笔记库。"))?;
        let mut package = source.opened.package;
        assign_copy_identity(&mut package, &library.default_notebook_id);
        let note_id = package.document.note_id.clone();
        let path = library
            .path
            .join("notes")
            .join(format!("{note_id}.cuenote"));
        storage::save_note(&path, &self.recovery_root, &package, None)?;
        let opened = storage::open_note(&path)?;
        if opened.package != package {
            return Err(StorageError::new(
                "conflict",
                "恢复副本在发布后发生变化；保留版本仍在。",
            ));
        }
        let session = self.open_note(&note_id)?;
        let library = self
            .refresh_external_library()?
            .expect("library checked above");
        Ok(SaveReceipt { session, library })
    }

    pub fn save_preserved_as(
        &mut self,
        version_id: &str,
        path: PathBuf,
    ) -> Result<SavedCopy, WorkspaceError> {
        let source = versions::open_version(&self.recovery_root, version_id)?;
        let mut package = source.opened.package;
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择笔记库。"))?;
        assign_copy_identity(&mut package, &library.default_notebook_id);
        let destination = self.resolve_copy_destination(&path)?;
        let (saved_path, revision) = storage::save_copy(&destination, &package)?;
        Ok(SavedCopy {
            path: display_path(&saved_path),
            revision,
        })
    }

    pub fn resolve_preserved_version(&mut self, version_id: &str) -> Result<(), WorkspaceError> {
        versions::resolve_version(&self.recovery_root, version_id)
    }

    fn refresh_external_library(&mut self) -> Result<Option<LibraryView>, WorkspaceError> {
        let Some(library) = self.library.as_mut() else {
            return Ok(None);
        };
        match scan_library(
            &library.path,
            &library.notebooks,
            &library.default_notebook_id,
            &self.recovery_root,
        ) {
            Ok(view) => library.view = view,
            Err(error) => {
                library.view.warnings =
                    vec![format!("无法核对笔记库；已保留当前列表。{}", error.message)]
            }
        }
        Ok(Some(library.view.clone()))
    }

    fn external_result(
        &mut self,
        status: &str,
        session: Option<NoteSession>,
        message: Option<String>,
    ) -> Result<ExternalReconciliation, WorkspaceError> {
        let library = self
            .refresh_external_library()?
            .ok_or_else(|| StorageError::new("noLibrary", "当前没有笔记库。"))?;
        Ok(ExternalReconciliation {
            status: status.into(),
            session,
            library,
            message,
        })
    }
}
