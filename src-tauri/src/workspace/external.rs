//! File notifications are hints. Session replacement happens only after the
//! frontend has settled editors, stopped writes, and captured the final draft.
use super::*;
use crate::storage::versions;

fn note_file_is_missing(path: &Path) -> bool {
    // A missing or unavailable parent says nothing about an individual note.
    // Preserve that distinction so a temporarily absent library is not shown
    // as an externally deleted note.
    path.parent()
        .is_some_and(|parent| storage::ensure_local_directory(parent).is_ok())
        && fs::symlink_metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalProbe {
    pub session_id: Option<String>,
    pub status: String,
    pub revision: Option<String>,
    pub message: Option<String>,
    pub library: Option<LibraryView>,
    pub session_read_only_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalReconciliation {
    pub status: String,
    pub session: Option<NoteSession>,
    pub library: Option<LibraryView>,
    pub message: Option<String>,
}

enum PreparedProbeRefresh {
    Visible(Option<LibraryView>),
    Scanned {
        // External editors do not hold the command-level library lease. Keep
        // their acquired lease across current-note checks and publication too.
        access: Option<storage::library_access::LibraryAccessGuard>,
        metadata: Option<PreparedLibraryMetadata>,
        result: Result<ScanCompletion, WorkspaceError>,
        context: SearchScanContext,
    },
}

impl PreparedProbeRefresh {
    fn commit(self, workspace: &mut Workspace) -> Option<LibraryView> {
        match self {
            Self::Visible(view) => view,
            Self::Scanned {
                access: _access,
                metadata,
                result,
                context,
            } => {
                if let Some(metadata) = metadata {
                    workspace.apply_library_metadata(metadata, true);
                }
                let library = workspace
                    .library
                    .as_mut()
                    .expect("selected library stayed locked");
                match result {
                    Ok(completion) => install_library_scan(library, completion.publish(&context)),
                    Err(error) => {
                        library.view.warnings =
                            vec![format!("无法刷新笔记列表：{}", error.message)];
                        library.view.scan_state = ScanState::Unavailable;
                        library.search = SearchSnapshot::unavailable(
                            context.generation.load(Ordering::Acquire),
                            vec![format!("无法刷新搜索索引：{}", error.message)],
                        );
                        emit_search_progress(
                            &context,
                            &library.path,
                            SearchStatus::Unavailable,
                            0,
                            0,
                            true,
                        );
                    }
                }
                Some(library.view.clone())
            }
        }
    }
}

impl Workspace {
    pub(crate) fn probe_external_changes_interruptible(
        &mut self,
        session_id: Option<&str>,
        attempt: &ProbeAttempt,
    ) -> Result<ExternalProbe, WorkspaceError> {
        self.probe_external_changes_inner(session_id, Some(attempt))
    }

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
        self.probe_external_changes_inner(session_id, None)
    }

    fn probe_external_changes_inner(
        &mut self,
        session_id: Option<&str>,
        attempt: Option<&ProbeAttempt>,
    ) -> Result<ExternalProbe, WorkspaceError> {
        #[cfg(feature = "perf-acceptance")]
        let mut trace = crate::performance::Trace::new("probe-external-changes");
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
        if attempt.is_some_and(|probe| probe.should_yield(ProbeCheckpoint::Start)) {
            #[cfg(feature = "perf-acceptance")]
            trace.mark("deferred-before-scan");
            return Ok(ExternalProbe::deferred(session_id));
        }
        let (library, pending) = if let Some(attempt) = attempt {
            let Some(pending) = self.prepare_probe_refresh(attempt)? else {
                #[cfg(feature = "perf-acceptance")]
                trace.mark("deferred-during-scan");
                return Ok(ExternalProbe::deferred(session_id));
            };
            (None, Some(pending))
        } else {
            (self.refresh_external_library()?, None)
        };
        #[cfg(feature = "perf-acceptance")]
        trace.mark(if pending.is_some() {
            "library-refresh-prepared"
        } else {
            "library-refreshed"
        });
        let mut probe = ExternalProbe {
            session_id: session_id.map(str::to_owned),
            status: "unchanged".into(),
            revision: None,
            message: None,
            library,
            session_read_only_reason: None,
        };
        if let Some(session) = self
            .session
            .as_ref()
            .filter(|session| !session.preserved_preview)
        {
            match storage::open_note(&session.path) {
                Ok(opened) => {
                    #[cfg(feature = "perf-acceptance")]
                    trace.mark("current-note-opened");
                    versions::preserve_observed_version(
                        &self.recovery_root,
                        &opened,
                        &display_path(&session.path),
                        "核对到的文件版本",
                    )?;
                    #[cfg(feature = "perf-acceptance")]
                    trace.mark("current-note-preserved");
                    probe.revision = Some(opened.revision.clone());
                    if session.baseline_revision.as_deref() != Some(opened.revision.as_str()) {
                        probe.status = "changed".into();
                    }
                }
                Err(error) => {
                    let missing = note_file_is_missing(&session.path);
                    if !(missing && session.baseline_revision.is_none()) {
                        if let Some(attempt) = attempt {
                            attempt.must_report();
                        }
                        probe.status = if missing { "missing" } else { "unreadable" }.into();
                        probe.message = Some(if missing {
                            error.message
                        } else {
                            format!("无法读取笔记文件或所在目录：{}", error.message)
                        });
                    }
                }
            }
        }
        if attempt.is_some_and(|probe| probe.should_yield(ProbeCheckpoint::BeforeCommit)) {
            #[cfg(feature = "perf-acceptance")]
            trace.mark("deferred-before-commit");
            return Ok(ExternalProbe::deferred(session_id));
        }
        if let Some(pending) = pending {
            probe.library = pending.commit(self);
            #[cfg(feature = "perf-acceptance")]
            trace.mark("library-refresh-committed");
        }
        if let Some(session) = &self.session {
            probe.session_read_only_reason = self.editor_read_only_reason(session);
        }
        Ok(probe)
    }

    fn prepare_probe_refresh(
        &self,
        attempt: &ProbeAttempt,
    ) -> Result<Option<PreparedProbeRefresh>, WorkspaceError> {
        // Keep the same selected-library lease and restore checks as a normal
        // external probe, including the independent external-editor fallback.
        let _access = if self.active_session_is_external() {
            self.ensure_library_transition_allowed()?;
            match self.acquire_library_access() {
                Ok(access) => access,
                Err(error) => {
                    attempt.must_report();
                    let mut view = self.library.as_ref().map(|library| library.view.clone());
                    if let Some(view) = &mut view {
                        push_warning(
                            &mut view.warnings,
                            format!("笔记库暂不能刷新，库外笔记仍保留：{}", error.message),
                        );
                    }
                    return Ok(Some(PreparedProbeRefresh::Visible(view)));
                }
            }
        } else {
            None
        };
        let missing = self.library.as_ref().is_some_and(|library| {
            fs::symlink_metadata(&library.path)
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        });
        if missing {
            self.ensure_library_transition_allowed()?;
        } else {
            self.ensure_library_available()?;
        }
        let Some(library) = &self.library else {
            return Ok(Some(PreparedProbeRefresh::Visible(None)));
        };
        let metadata = (!missing).then(|| PreparedLibraryMetadata::read(&library.path));
        if attempt.should_yield(ProbeCheckpoint::MetadataPrepared) {
            return Ok(None);
        }
        let (notebooks, default, warnings) = match &metadata {
            Some(prepared) if prepared.metadata.directory_available => (
                &prepared.metadata.notebooks,
                prepared.default_notebook_id.as_deref(),
                &prepared.metadata.warnings,
            ),
            Some(prepared) => (
                &library.notebooks,
                library.default_notebook_id.as_deref(),
                &prepared.metadata.warnings,
            ),
            None => (
                &library.notebooks,
                library.default_notebook_id.as_deref(),
                &library.metadata_warnings,
            ),
        };
        let context = self.search_scan_context();
        let result = match scan_library_with_previous_inner(
            &library.path,
            notebooks,
            default,
            &self.recovery_root,
            warnings,
            Some(&library.view),
            &context,
            Some(attempt),
        ) {
            Ok(ScanAttempt::Deferred) => return Ok(None),
            Ok(ScanAttempt::Complete(completion)) => Ok(completion),
            Err(error) => {
                attempt.must_report();
                Err(error)
            }
        };
        Ok(Some(PreparedProbeRefresh::Scanned {
            access: _access,
            metadata,
            result,
            context,
        }))
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
            Err(_) => active.baseline_revision.is_none() && note_file_is_missing(&path),
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
                    "{}，当前内容仍保留；没有建立空白文件。{}",
                    if note_file_is_missing(&path) {
                        "外部笔记文件已删除"
                    } else {
                        "笔记文件或所在目录暂不可读"
                    },
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
        let notebook_id = opened.package.document.organization.notebook_id.clone();
        let is_external = self.active_session_is_external();
        let reason = if is_external {
            external_notes::external_read_only_reason(&opened.package)
        } else {
            let library = self
                .library
                .as_ref()
                .ok_or_else(|| StorageError::new("noLibrary", "当前没有笔记库。"))?;
            read_only_reason(&opened.package, notebook_id.as_deref(), &library.notebooks)
        };
        let was_writable = reason.is_none();
        let mut view = note_session_from_opened(&opened.package.document.note_id, &opened, reason);
        if is_external {
            external_notes::set_external_origin(&mut view, &path);
        }
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package: opened.package,
            asset_stash: None,
            baseline_revision: Some(opened.revision),
            notebook_id,
            baseline_witness: Some(baseline_witness),
            preserved_preview: false,
            return_external_path: None,
            external_conflict: false,
            was_writable,
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
        let return_external_path = self.session.as_ref().and_then(|session| {
            if matches!(session.view.origin, SessionOrigin::External { .. }) {
                Some(session.path.clone())
            } else {
                session.return_external_path.clone()
            }
        });
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
        let mut view = note_session_from_opened(
            &opened.package.document.note_id,
            &opened,
            Some("这是保留版本的只读预览。可恢复为新笔记或另存完整副本。".into()),
        );
        view.origin = SessionOrigin::PreservedPreview;
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path: preserved.path,
            notebook_id: opened.package.document.organization.notebook_id.clone(),
            package: opened.package,
            asset_stash: None,
            baseline_revision: Some(opened.revision),
            baseline_witness,
            preserved_preview: true,
            return_external_path,
            external_conflict: false,
            was_writable: false,
        });
        Ok(view)
    }

    pub fn restore_preserved_as_copy(
        &mut self,
        version_id: &str,
    ) -> Result<SaveReceipt, WorkspaceError> {
        self.refresh_library_metadata()?;
        let source = versions::open_version(&self.recovery_root, version_id)?;
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择笔记库。"))?;
        let default_notebook_id = library
            .default_notebook_id
            .as_deref()
            .ok_or_else(library_metadata_pending)?;
        let mut package = source.opened.package;
        assign_copy_identity(&mut package, default_notebook_id);
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
        if let Some(default_notebook_id) = self
            .library
            .as_ref()
            .and_then(|library| library.default_notebook_id.as_deref())
        {
            assign_copy_identity(&mut package, default_notebook_id);
        } else {
            assign_copy_identity_preserving_organization(&mut package);
        }
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
        if self.active_session_is_external() {
            self.ensure_library_transition_allowed()?;
            return match self.acquire_library_access() {
                Ok(_access) => self.refresh_selected_library(),
                Err(error) => {
                    let mut view = self.library.as_ref().map(|library| library.view.clone());
                    if let Some(view) = &mut view {
                        push_warning(
                            &mut view.warnings,
                            format!("笔记库暂不能刷新，库外笔记仍保留：{}", error.message),
                        );
                    }
                    Ok(view)
                }
            };
        }
        self.refresh_selected_library()
    }

    fn external_result(
        &mut self,
        status: &str,
        session: Option<NoteSession>,
        message: Option<String>,
    ) -> Result<ExternalReconciliation, WorkspaceError> {
        let library = self.refresh_external_library()?;
        Ok(ExternalReconciliation {
            status: status.into(),
            session,
            library,
            message,
        })
    }
}

impl ExternalProbe {
    fn deferred(session_id: Option<&str>) -> Self {
        Self {
            session_id: session_id.map(str::to_owned),
            status: "deferred".into(),
            revision: None,
            message: None,
            library: None,
            session_read_only_reason: None,
        }
    }
}
