//! The host-owned library selection and note editing session.
//!
//! The frontend receives only DTOs and opaque session identifiers. Paths,
//! package contents, organization fields, and revision baselines stay here.

use crate::note_format::{Block, Inline, Mark, NoteContent, NotePackage, PlainText, TableRow};
use crate::storage::{self, OpenedNote, StorageError};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub type WorkspaceError = StorageError;

const LIBRARY_SCHEMA_VERSION: u32 = 1;
const PREFERENCE_SCHEMA_VERSION: u32 = 1;
const MAX_NOTEBOOKS: usize = 1024;
const MAX_NOTE_FILES: usize = 100_000;
const MAX_WARNINGS: usize = 100;
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_NOTE_EXCERPT_CHARS: usize = 180;
const MAX_PREFERENCE_BYTES: u64 = 16 * 1024;
const MAX_PREFERENCE_SNAPSHOTS: usize = 64;
const RETAINED_PREFERENCE_SNAPSHOTS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteSummary {
    pub note_id: String,
    pub title: String,
    pub modified_at: String,
    pub trashed_at: Option<String>,
    pub notebook_id: Option<String>,
    pub pinned: bool,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotebookSummary {
    pub notebook_id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryView {
    pub path: String,
    pub default_notebook_name: String,
    pub default_notebook_id: String,
    pub notebooks: Vec<NotebookSummary>,
    pub notes: Vec<NoteSummary>,
    pub trash_notes: Vec<NoteSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteSession {
    pub session_id: String,
    pub note_id: String,
    pub notebook_id: Option<String>,
    pub pinned: bool,
    pub trashed_at: Option<String>,
    pub text: PlainText,
    pub content: NoteContent,
    pub revision: Option<String>,
    pub read_only_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewPreferences {
    pub sidebar_collapsed: bool,
    pub selected_notebook_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceView {
    pub app_version: String,
    pub default_library_path: String,
    pub library: Option<LibraryView>,
    pub session: Option<NoteSession>,
    pub preferences: ViewPreferences,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveReceipt {
    pub session: NoteSession,
    pub library: LibraryView,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryDraftSummary {
    pub recovery_id: String,
    pub note_id: String,
    pub title: String,
    pub saved_at: String,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryList {
    pub drafts: Vec<RecoveryDraftSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedCopy {
    pub path: String,
    pub revision: String,
}

/// A persisted notebook is intentionally independent of every `.cuenote` file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotebookMetadata {
    schema_version: u32,
    id: String,
    name: String,
    default: bool,
    timestamps: NotebookTimestamps,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotebookTimestamps {
    created_at: String,
    modified_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspacePreference {
    schema_version: u32,
    sequence: u64,
    saved_at: String,
    library_path: String,
    #[serde(default)]
    last_note_id: Option<String>,
    #[serde(default)]
    sidebar_collapsed: bool,
    #[serde(default)]
    selected_notebook_id: Option<String>,
}

struct LibraryState {
    path: PathBuf,
    notebooks: BTreeMap<String, NotebookMetadata>,
    default_notebook_id: String,
    view: LibraryView,
}

struct ActiveNoteSession {
    view: NoteSession,
    path: PathBuf,
    package: NotePackage,
    baseline_revision: Option<String>,
    notebook_id: Option<String>,
}

/// Coordinates one selected library and at most one host-owned editor session.
pub struct Workspace {
    config_root: PathBuf,
    recovery_root: PathBuf,
    default_library_path: PathBuf,
    library: Option<LibraryState>,
    session: Option<ActiveNoteSession>,
    notice: Option<String>,
    last_note_id: Option<String>,
    preferences: ViewPreferences,
    initialized: bool,
}

impl Workspace {
    pub fn new(
        config_root: PathBuf,
        recovery_root: PathBuf,
        default_library_path: PathBuf,
    ) -> Self {
        Self {
            config_root,
            recovery_root,
            default_library_path,
            library: None,
            session: None,
            notice: None,
            last_note_id: None,
            preferences: ViewPreferences::default(),
            initialized: false,
        }
    }

    pub fn snapshot(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        if !self.initialized {
            self.initialized = true;
            match self.read_latest_preference() {
                Ok(Some(preference)) => {
                    self.preferences = ViewPreferences {
                        sidebar_collapsed: preference.sidebar_collapsed,
                        selected_notebook_id: preference.selected_notebook_id,
                    };
                    self.last_note_id = preference
                        .last_note_id
                        .clone()
                        .filter(|note_id| is_canonical_uuid_v4(note_id));
                    if preference.last_note_id.is_some() && self.last_note_id.is_none() {
                        self.notice = Some("上次笔记身份无效，已返回笔记列表。".into());
                    }
                    let path = PathBuf::from(preference.library_path);
                    match self.load_existing_library(&path) {
                        Ok(library) => {
                            if self
                                .preferences
                                .selected_notebook_id
                                .as_ref()
                                .is_some_and(|id| !library.notebooks.contains_key(id))
                            {
                                self.preferences.selected_notebook_id = None;
                                if let Err(error) = self.persist_preference(
                                    &library.path,
                                    self.last_note_id.as_deref(),
                                    &self.preferences,
                                ) {
                                    self.notice = Some(format!(
                                        "已清除失效的笔记本筛选，但无法保存本机界面设置：{}",
                                        error.message
                                    ));
                                }
                            }
                            self.library = Some(library);
                            if let Some(note_id) = self.last_note_id.clone() {
                                let note_path = self
                                    .library
                                    .as_ref()
                                    .expect("library was just loaded")
                                    .path
                                    .join("notes")
                                    .join(format!("{note_id}.cuenote"));
                                match fs::symlink_metadata(&note_path) {
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                        self.last_note_id = None;
                                        let library_path = self
                                            .library
                                            .as_ref()
                                            .expect("library was just loaded")
                                            .path
                                            .clone();
                                        if let Err(error) = self.persist_preference(
                                            &library_path,
                                            None,
                                            &self.preferences,
                                        ) {
                                            self.notice = Some(format!(
                                                "上次笔记已不存在，已返回笔记列表；无法更新本机设置：{}",
                                                error.message
                                            ));
                                        }
                                    }
                                    Err(error) => {
                                        self.notice = Some(format!(
                                            "无法检查上次笔记；请检查文件后重新打开。{error}"
                                        ));
                                    }
                                    Ok(_) => match self.open_note(&note_id) {
                                        Ok(session) if session.trashed_at.is_some() => {
                                            self.session = None;
                                            self.last_note_id = None;
                                            let library_path = self
                                                .library
                                                .as_ref()
                                                .expect("library was just loaded")
                                                .path
                                                .clone();
                                            self.notice = Some(match self.persist_preference(
                                                    &library_path,
                                                    None,
                                                    &self.preferences,
                                                ) {
                                                    Ok(()) => "上次笔记已移入回收站，已返回笔记列表。".into(),
                                                    Err(error) => format!(
                                                        "上次笔记已移入回收站，已返回笔记列表；无法更新本机设置：{}",
                                                        error.message
                                                    ),
                                                });
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            self.notice = Some(format!(
                                                "无法重新打开上次笔记；笔记列表仍可用。{}",
                                                error.message
                                            ));
                                        }
                                    },
                                }
                            }
                        }
                        Err(error) => {
                            self.notice = Some(format!(
                                "无法重新打开上次使用的笔记库；请检查目录后重新选择。{}",
                                error.message
                            ));
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.notice = Some(format!("无法读取上次的笔记库设置：{}", error.message));
                }
            }
        }
        if let Some(library) = self.library.as_mut() {
            match scan_library(
                &library.path,
                &library.notebooks,
                &library.default_notebook_id,
            ) {
                Ok(view) => library.view = view,
                Err(error) => {
                    library.view.warnings = vec![format!("无法刷新笔记列表：{}", error.message)]
                }
            }
        }
        Ok(self.view())
    }

    pub fn select_library(&mut self, path: PathBuf) -> Result<WorkspaceView, WorkspaceError> {
        // Construct the complete candidate before changing the visible workspace.
        let library = self.prepare_library(&path, true)?;
        let mut preferences = self.preferences.clone();
        if preferences
            .selected_notebook_id
            .as_ref()
            .is_some_and(|id| !library.notebooks.contains_key(id))
        {
            preferences.selected_notebook_id = None;
        }
        self.persist_preference(&library.path, None, &preferences)?;
        self.library = Some(library);
        self.session = None;
        self.last_note_id = None;
        self.preferences = preferences;
        self.notice = None;
        self.initialized = true;
        Ok(self.view())
    }

    pub fn use_default_library(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        self.select_library(self.default_library_path.clone())
    }

    pub fn new_note(&mut self) -> Result<NoteSession, WorkspaceError> {
        self.new_note_in(None)
    }

    pub fn new_note_in(
        &mut self,
        notebook_id: Option<&str>,
    ) -> Result<NoteSession, WorkspaceError> {
        let (library_path, assigned_notebook_id) = {
            let library = self
                .library
                .as_ref()
                .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
            let assigned = match notebook_id {
                Some(id) if library.notebooks.contains_key(id) => id.to_owned(),
                Some(_) => {
                    return Err(StorageError::new(
                        "unknownNotebook",
                        "所选笔记本不存在于当前笔记库。",
                    ));
                }
                None => library.default_notebook_id.clone(),
            };
            (library.path.clone(), assigned)
        };
        let package = NotePackage::new_plain(
            assigned_notebook_id.clone(),
            PlainText {
                title: String::new(),
                cue: String::new(),
                content: String::new(),
                summary: String::new(),
            },
        );
        let note_id = package.document.note_id.clone();
        if !is_canonical_uuid_v4(&note_id) {
            return Err(StorageError::new(
                "format",
                "原生笔记格式生成了无效的笔记身份。",
            ));
        }
        let path = library_path
            .join("notes")
            .join(format!("{note_id}.cuenote"));
        let view = NoteSession {
            session_id: Uuid::new_v4().to_string(),
            note_id,
            notebook_id: Some(assigned_notebook_id.clone()),
            pinned: false,
            trashed_at: None,
            text: package.text_projection(),
            content: package.content(),
            revision: None,
            read_only_reason: None,
        };
        self.persist_preference(&library_path, Some(&view.note_id), &self.preferences)?;
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package,
            baseline_revision: None,
            notebook_id: Some(assigned_notebook_id),
        });
        self.last_note_id = Some(view.note_id.clone());
        Ok(view)
    }

    pub fn open_note(&mut self, note_id: &str) -> Result<NoteSession, WorkspaceError> {
        if !is_canonical_uuid_v4(note_id) {
            return Err(StorageError::new("invalidNoteId", "笔记身份格式无效。"));
        }
        let (library_path, path) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library
                        .path
                        .join("notes")
                        .join(format!("{note_id}.cuenote")),
                )
            })
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        ensure_regular_child(&library_path, &path)?;
        let opened = storage::open_note(&path)?;
        if opened.package.document.note_id != note_id {
            return Err(StorageError::new(
                "format",
                "笔记文件中的身份与文件名不一致。",
            ));
        }
        let (notebook_id, read_only_reason) = {
            let library = self.library.as_ref().expect("library was checked above");
            let notebook_id = opened.package.document.organization.notebook_id.clone();
            let read_only_reason =
                read_only_reason(&opened.package, notebook_id.as_deref(), &library.notebooks);
            (notebook_id, read_only_reason)
        };
        let view = note_session_from_opened(note_id, &opened, read_only_reason.clone());
        if view.trashed_at.is_none() {
            self.persist_preference(&library_path, Some(note_id), &self.preferences)?;
            self.last_note_id = Some(note_id.to_owned());
        }
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package: opened.package,
            baseline_revision: Some(opened.revision),
            notebook_id,
        });
        Ok(view)
    }

    pub fn trash_note(&mut self, session_id: &str) -> Result<SaveReceipt, WorkspaceError> {
        let (library_path, default_notebook_id, notebooks) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library.default_notebook_id.clone(),
                    library.notebooks.clone(),
                )
            })
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可移入回收站的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请保留当前文字后重新打开笔记。",
            ));
        }
        if session.package.document.organization.trashed_at.is_some() {
            return Err(StorageError::new("trashedNote", "此笔记已在回收站中。"));
        }
        let notebook_id = session
            .package
            .document
            .organization
            .notebook_id
            .as_deref()
            .ok_or_else(|| StorageError::new("invalidNotebook", "此笔记缺少笔记本归属。"))?;
        if !notebooks.contains_key(notebook_id) {
            return Err(StorageError::new(
                "invalidNotebook",
                "此笔记属于未知笔记本；回收站状态未更改。",
            ));
        }

        let mut candidate = session.package.clone();
        candidate.document.organization.trashed_at = Some(now_timestamp());
        candidate.document.timestamps.organization_modified_at =
            next_timestamp_after(&candidate.document.timestamps.organization_modified_at);
        let note_path = session.path.clone();
        let baseline_revision = session.baseline_revision.clone();
        let note_id = session.view.note_id.clone();
        let (revision, publish_warning) = match storage::save_note(
            &note_path,
            &self.recovery_root,
            &candidate,
            baseline_revision.as_deref(),
        ) {
            Ok(revision) => (revision, None),
            Err(error) => match storage::open_note(&note_path) {
                Ok(published) if published.package == candidate => (
                    published.revision,
                    Some(format!(
                        "笔记已移入回收站并通过重读校验，但保存后步骤报告错误：{}",
                        error.message
                    )),
                ),
                _ => return Err(error),
            },
        };

        let read_only_reason = read_only_reason(
            &candidate,
            candidate.document.organization.notebook_id.as_deref(),
            &notebooks,
        );
        let active = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("staleSession", "笔记会话在整理期间已关闭。"))?;
        if active.view.session_id != session_id || active.view.note_id != note_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话在整理期间发生变化；已发布的回收站状态仍保留在原笔记文件中。",
            ));
        }
        active.package = candidate.clone();
        active.baseline_revision = Some(revision.clone());
        active.view.trashed_at = candidate.document.organization.trashed_at.clone();
        active.view.read_only_reason = read_only_reason;
        active.view.revision = Some(revision);
        let saved_session = active.view.clone();

        self.last_note_id = None;
        let preference_warning = self
            .persist_preference(&library_path, None, &self.preferences)
            .err()
            .map(|error| {
                format!(
                    "笔记已移入回收站，但无法更新本机上次笔记设置：{}",
                    error.message
                )
            });

        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library =
            match scan_library(&library_path, &library.notebooks, &default_notebook_id) {
                Ok(view) => {
                    library.view = view.clone();
                    view
                }
                Err(error) => {
                    let mut view = library.view.clone();
                    view.notes.retain(|note| note.note_id != note_id);
                    view.trash_notes.retain(|note| note.note_id != note_id);
                    view.trash_notes
                        .push(note_summary_from_package(&note_id, &candidate));
                    sort_trash_notes(&mut view.trash_notes);
                    push_warning(
                        &mut view.warnings,
                        format!("笔记已移入回收站，但无法刷新笔记列表：{}", error.message),
                    );
                    library.view = view.clone();
                    view
                }
            };
        if let Some(warning) = publish_warning {
            push_warning(&mut receipt_library.warnings, warning);
        }
        if let Some(warning) = preference_warning {
            push_warning(&mut receipt_library.warnings, warning);
        }
        library.view.warnings = receipt_library.warnings.clone();
        Ok(SaveReceipt {
            session: saved_session,
            library: receipt_library,
        })
    }

    pub fn restore_trashed_note(
        &mut self,
        session_id: &str,
        target_notebook_id: Option<&str>,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let (library_path, default_notebook_id, notebooks) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library.default_notebook_id.clone(),
                    library.notebooks.clone(),
                )
            })
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可恢复的回收站笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开回收站中的笔记后再恢复。",
            ));
        }
        let source_notebook_id = session.package.document.organization.notebook_id.as_deref();
        if session.package.document.organization.trashed_at.is_none() {
            return Err(StorageError::new("notTrashed", "所选笔记不在回收站中。"));
        }
        let notebook_id = match source_notebook_id {
            Some(source) if notebooks.contains_key(source) => source.to_owned(),
            _ => {
                let target = target_notebook_id.ok_or_else(|| {
                    StorageError::new(
                        "restoreTargetRequired",
                        "原笔记本已不存在，请选择一个现有笔记本后恢复。",
                    )
                })?;
                if !notebooks.contains_key(target) {
                    return Err(StorageError::new(
                        "unknownNotebook",
                        "恢复目标笔记本不存在于当前笔记库。",
                    ));
                }
                target.to_owned()
            }
        };

        let mut candidate = session.package.clone();
        candidate.document.organization.notebook_id = Some(notebook_id.clone());
        candidate.document.organization.trashed_at = None;
        candidate.document.timestamps.organization_modified_at =
            next_timestamp_after(&candidate.document.timestamps.organization_modified_at);
        let note_path = session.path.clone();
        let baseline_revision = session.baseline_revision.clone();
        let note_id = session.view.note_id.clone();
        let (revision, publish_warning) = match storage::save_note(
            &note_path,
            &self.recovery_root,
            &candidate,
            baseline_revision.as_deref(),
        ) {
            Ok(revision) => (revision, None),
            Err(error) => match storage::open_note(&note_path) {
                Ok(published) if published.package == candidate => (
                    published.revision,
                    Some(format!(
                        "笔记已恢复并通过重读校验，但保存后步骤报告错误：{}",
                        error.message
                    )),
                ),
                _ => return Err(error),
            },
        };

        let read_only_reason = read_only_reason(&candidate, Some(&notebook_id), &notebooks);
        let active = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("staleSession", "笔记会话在恢复期间已关闭。"))?;
        if active.view.session_id != session_id || active.view.note_id != note_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话在恢复期间发生变化；已发布的恢复状态仍保留在原笔记文件中。",
            ));
        }
        active.package = candidate.clone();
        active.baseline_revision = Some(revision.clone());
        active.notebook_id = Some(notebook_id.clone());
        active.view.notebook_id = Some(notebook_id);
        active.view.trashed_at = None;
        active.view.read_only_reason = read_only_reason;
        active.view.revision = Some(revision);
        let saved_session = active.view.clone();

        self.last_note_id = Some(note_id.clone());
        let preference_warning = self
            .persist_preference(&library_path, Some(&note_id), &self.preferences)
            .err()
            .map(|error| format!("笔记已恢复，但无法更新本机上次笔记设置：{}", error.message));

        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library =
            match scan_library(&library_path, &library.notebooks, &default_notebook_id) {
                Ok(view) => {
                    library.view = view.clone();
                    view
                }
                Err(error) => {
                    let mut view = library.view.clone();
                    view.trash_notes.retain(|note| note.note_id != note_id);
                    view.notes.retain(|note| note.note_id != note_id);
                    view.notes
                        .push(note_summary_from_package(&note_id, &candidate));
                    sort_note_summaries(&mut view.notes);
                    push_warning(
                        &mut view.warnings,
                        format!("笔记已恢复，但无法刷新笔记列表：{}", error.message),
                    );
                    library.view = view.clone();
                    view
                }
            };
        if let Some(warning) = publish_warning {
            push_warning(&mut receipt_library.warnings, warning);
        }
        if let Some(warning) = preference_warning {
            push_warning(&mut receipt_library.warnings, warning);
        }
        library.view.warnings = receipt_library.warnings.clone();
        Ok(SaveReceipt {
            session: saved_session,
            library: receipt_library,
        })
    }

    pub fn save_note_text(
        &mut self,
        session_id: &str,
        text: PlainText,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let library_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可保存的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请保留当前文字后重新打开笔记。",
            ));
        }
        if let Some(reason) = session.view.read_only_reason.as_ref() {
            return Err(StorageError::new("readOnly", reason.clone()));
        }
        if session.package.plain_text().is_none() {
            return Err(StorageError::new(
                "readOnly",
                "此笔记包含结构化内容，请使用富文本编辑器保存。",
            ));
        }

        let mut candidate = session.package.clone();
        candidate
            .replace_plain_text(text.clone())
            .map_err(|error| StorageError::new("format", error.to_string()))?;
        if candidate.document.note_id != session.view.note_id
            || candidate.document.organization.notebook_id != session.notebook_id
        {
            return Err(StorageError::new(
                "format",
                "简易编辑器不能更改笔记身份或笔记本归属。",
            ));
        }
        let revision = storage::save_note(
            &session.path,
            &self.recovery_root,
            &candidate,
            session.baseline_revision.as_deref(),
        )?;

        let active = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("staleSession", "笔记会话在保存期间已关闭。"))?;
        // Workspace calls currently run under one host mutex. Keep this guard so a
        // future asynchronous implementation cannot publish into a changed session.
        if active.view.session_id != session_id || active.view.note_id != candidate.document.note_id
        {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话在保存期间发生变化；已保存快照仍保留在原笔记文件中。",
            ));
        }
        active.package = candidate;
        active.baseline_revision = Some(revision.clone());
        active.view.text = text;
        active.view.content = active.package.content();
        active.view.revision = Some(revision);
        let saved_session = active.view.clone();

        let library = self.library.as_mut().expect("library checked above");
        let receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            &library.default_notebook_id,
        ) {
            Ok(view) => {
                library.view = view.clone();
                view
            }
            Err(error) => {
                let mut view = library.view.clone();
                push_warning(
                    &mut view.warnings,
                    format!("笔记已保存，但无法刷新列表：{}", error.message),
                );
                library.view = view.clone();
                view
            }
        };
        Ok(SaveReceipt {
            session: saved_session,
            library: receipt_library,
        })
    }

    pub fn save_note_content(
        &mut self,
        session_id: &str,
        content: NoteContent,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let library_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let (candidate, note_path, baseline_revision, note_id) = {
            let session = self
                .session
                .as_ref()
                .ok_or_else(|| StorageError::new("noSession", "当前没有可保存的笔记会话。"))?;
            if session.view.session_id != session_id {
                return Err(StorageError::new(
                    "staleSession",
                    "笔记会话已切换；请保留当前内容后重新打开笔记。",
                ));
            }
            if let Some(reason) = session.view.read_only_reason.as_ref() {
                return Err(StorageError::new("readOnly", reason.clone()));
            }
            if !supports_editable_content(&content) {
                return Err(StorageError::new(
                    "readOnly",
                    "笔记包含当前编辑器尚不支持的内容，未保存任何更改。",
                ));
            }

            let mut candidate = session.package.clone();
            candidate.replace_content(content.clone());
            candidate
                .validate()
                .map_err(|error| StorageError::new("format", error.to_string()))?;
            if candidate.document.note_id != session.view.note_id
                || candidate.document.organization != session.package.document.organization
                || candidate.document.organization.notebook_id != session.notebook_id
            {
                return Err(StorageError::new(
                    "format",
                    "结构化编辑器不能更改笔记身份或组织信息。",
                ));
            }
            (
                candidate,
                session.path.clone(),
                session.baseline_revision.clone(),
                session.view.note_id.clone(),
            )
        };

        let (revision, publish_warning) = match storage::save_note(
            &note_path,
            &self.recovery_root,
            &candidate,
            baseline_revision.as_deref(),
        ) {
            Ok(revision) => (revision, None),
            Err(error) => match storage::open_note(&note_path) {
                Ok(published) if published.package == candidate => (
                    published.revision,
                    Some(format!(
                        "笔记已保存并通过重读校验，但保存后步骤报告错误：{}",
                        error.message
                    )),
                ),
                _ => return Err(error),
            },
        };

        let active = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("staleSession", "笔记会话在保存期间已关闭。"))?;
        if active.view.session_id != session_id || active.view.note_id != note_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话在保存期间发生变化；已保存快照仍保留在原笔记文件中。",
            ));
        }
        active.package = candidate.clone();
        active.baseline_revision = Some(revision.clone());
        active.view.content = content;
        active.view.text = candidate.text_projection();
        active.view.revision = Some(revision);
        let saved_session = active.view.clone();

        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            &library.default_notebook_id,
        ) {
            Ok(view) => {
                library.view = view.clone();
                view
            }
            Err(error) => {
                let mut view = library.view.clone();
                push_warning(
                    &mut view.warnings,
                    format!("笔记已保存，但无法刷新列表：{}", error.message),
                );
                library.view = view.clone();
                view
            }
        };
        if let Some(warning) = publish_warning {
            push_warning(&mut receipt_library.warnings, warning);
        }
        library.view.warnings = receipt_library.warnings.clone();
        Ok(SaveReceipt {
            session: saved_session,
            library: receipt_library,
        })
    }

    pub fn create_notebook(&mut self, name: &str) -> Result<LibraryView, WorkspaceError> {
        let name = validated_notebook_name(name)?;
        let (library_path, default_notebook_id, mut notebooks) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library.default_notebook_id.clone(),
                    library.notebooks.clone(),
                )
            })
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        ensure_unique_notebook_name(&notebooks, &name, None)?;
        if notebooks.len() >= MAX_NOTEBOOKS {
            return Err(StorageError::new(
                "libraryTooLarge",
                "笔记本数量已达到当前上限。",
            ));
        }

        let notebook = new_notebook(name, false);
        let notebook_id = notebook.id.clone();
        let published_notebook = notebook.clone();
        let metadata_path = library_path.join("notebooks");
        notebooks.insert(notebook_id, notebook);
        let view = scan_library(&library_path, &notebooks, &default_notebook_id)?;
        publish_notebook_metadata(&metadata_path, &published_notebook)?;
        let library = self.library.as_mut().expect("library checked above");
        library.notebooks = notebooks;
        library.view = view.clone();
        Ok(view)
    }

    pub fn rename_notebook(
        &mut self,
        notebook_id: &str,
        name: &str,
    ) -> Result<LibraryView, WorkspaceError> {
        let name = validated_notebook_name(name)?;
        let (library_path, default_notebook_id, current, mut notebooks) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library.default_notebook_id.clone(),
                    library.notebooks.get(notebook_id).cloned(),
                    library.notebooks.clone(),
                )
            })
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let mut notebook =
            current.ok_or_else(|| StorageError::new("unknownNotebook", "所选笔记本不存在。"))?;
        if notebook.default {
            return Err(StorageError::new(
                "defaultNotebook",
                "内置默认笔记本名称不能更改。",
            ));
        }
        ensure_unique_notebook_name(&notebooks, &name, Some(notebook_id))?;
        let (metadata_path, expected_bytes) =
            read_notebook_metadata_file(&library_path, notebook_id)?;
        let on_disk: NotebookMetadata =
            serde_json::from_slice(&expected_bytes).map_err(|error| {
                StorageError::new("libraryMetadata", format!("笔记本元数据无效：{error}"))
            })?;
        if on_disk != notebook {
            return Err(StorageError::new(
                "conflict",
                "笔记本元数据已在其他位置修改；请重新打开笔记库后重试。",
            ));
        }

        notebook.name = name;
        notebook.timestamps.modified_at = next_timestamp_after(&notebook.timestamps.modified_at);
        let candidate_bytes = serde_json::to_vec(&notebook).map_err(|error| {
            StorageError::new("libraryMetadata", format!("无法编码笔记本元数据：{error}"))
        })?;
        notebooks.insert(notebook_id.to_owned(), notebook);
        let view = scan_library(&library_path, &notebooks, &default_notebook_id)?;
        storage::replace_metadata_file(&metadata_path, &expected_bytes, &candidate_bytes)?;

        let library = self.library.as_mut().expect("library checked above");
        library.notebooks = notebooks;
        library.view = view.clone();
        Ok(view)
    }

    pub fn delete_notebook(&mut self, notebook_id: &str) -> Result<LibraryView, WorkspaceError> {
        let (library_path, default_notebook_id, current, mut notebooks, unsaved_session_uses_it) =
            self.library
                .as_ref()
                .map(|library| {
                    (
                        library.path.clone(),
                        library.default_notebook_id.clone(),
                        library.notebooks.get(notebook_id).cloned(),
                        library.notebooks.clone(),
                        self.session.as_ref().is_some_and(|session| {
                            session.baseline_revision.is_none()
                                && session.notebook_id.as_deref() == Some(notebook_id)
                        }),
                    )
                })
                .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let notebook =
            current.ok_or_else(|| StorageError::new("unknownNotebook", "所选笔记本不存在。"))?;
        if notebook.default {
            return Err(StorageError::new(
                "defaultNotebook",
                "内置默认笔记本不能删除。",
            ));
        }
        if unsaved_session_uses_it {
            return Err(StorageError::new(
                "notebookInUse",
                "当前未保存笔记属于此笔记本；请先保存或切换该笔记。",
            ));
        }
        let assigned_notes = count_notebook_notes(&library_path, notebook_id, &notebooks)?;
        if assigned_notes > 0 {
            return Err(StorageError::new(
                "notebookInUse",
                "此笔记本仍有笔记，请先将笔记移动到其他笔记本，再删除此笔记本。",
            ));
        }

        let (metadata_path, expected_bytes) =
            read_notebook_metadata_file(&library_path, notebook_id)?;
        let on_disk: NotebookMetadata =
            serde_json::from_slice(&expected_bytes).map_err(|error| {
                StorageError::new("libraryMetadata", format!("笔记本元数据无效：{error}"))
            })?;
        if on_disk != notebook {
            return Err(StorageError::new(
                "conflict",
                "笔记本元数据已在其他位置修改；请重新打开笔记库后重试。",
            ));
        }
        notebooks.remove(notebook_id);
        let mut view = scan_library(&library_path, &notebooks, &default_notebook_id)?;
        let (_, current_bytes) = read_notebook_metadata_file(&library_path, notebook_id)?;
        if current_bytes != expected_bytes {
            return Err(StorageError::new(
                "conflict",
                "笔记本元数据在删除前发生变化；原文件已保留。",
            ));
        }
        fs::remove_file(&metadata_path)
            .map_err(|error| StorageError::new("io", format!("无法删除笔记本元数据：{error}")))?;
        if let Err(error) = sync_directory(
            metadata_path
                .parent()
                .expect("notebook metadata path has a parent"),
        ) {
            push_warning(
                &mut view.warnings,
                format!("笔记本已删除，但无法同步元数据目录：{error}"),
            );
        }

        self.library
            .as_mut()
            .expect("library checked above")
            .notebooks = notebooks;
        let removed_filter = self.preferences.selected_notebook_id.as_deref() == Some(notebook_id);
        let library_path_for_preferences = library_path.clone();
        if removed_filter {
            self.preferences.selected_notebook_id = None;
            if let Err(error) = self.persist_preference(
                &library_path_for_preferences,
                self.last_note_id.as_deref(),
                &self.preferences,
            ) {
                push_warning(
                    &mut view.warnings,
                    format!(
                        "已清除失效的笔记本筛选，但无法保存本机界面设置：{}",
                        error.message
                    ),
                );
            }
        }
        self.library.as_mut().expect("library checked above").view = view.clone();
        Ok(view)
    }

    pub fn update_note_organization(
        &mut self,
        session_id: &str,
        notebook_id: &str,
        pinned: bool,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let (library_path, default_notebook_id, notebook_exists) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library.default_notebook_id.clone(),
                    library.notebooks.contains_key(notebook_id),
                )
            })
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        if !notebook_exists {
            return Err(StorageError::new(
                "unknownNotebook",
                "目标笔记本不存在于当前笔记库。",
            ));
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可整理的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请保留当前文字后重新打开笔记。",
            ));
        }
        if session.package.document.organization.trashed_at.is_some() {
            return Err(StorageError::new(
                "trashedNote",
                "回收站笔记不能更改笔记本归属或置顶状态。",
            ));
        }
        let source_notebook_id = session
            .package
            .document
            .organization
            .notebook_id
            .as_deref()
            .ok_or_else(|| {
                StorageError::new(
                    "invalidNotebook",
                    "此笔记缺少有效笔记本归属，组织信息未更改。",
                )
            })?;
        if !self
            .library
            .as_ref()
            .expect("library checked above")
            .notebooks
            .contains_key(source_notebook_id)
        {
            return Err(StorageError::new(
                "invalidNotebook",
                "此笔记属于未知笔记本，组织信息未更改。",
            ));
        }

        if source_notebook_id == notebook_id
            && session.package.document.organization.pinned == pinned
        {
            return Ok(SaveReceipt {
                session: session.view.clone(),
                library: self
                    .library
                    .as_ref()
                    .expect("library checked above")
                    .view
                    .clone(),
            });
        }

        let mut candidate = session.package.clone();
        candidate.document.organization.notebook_id = Some(notebook_id.to_owned());
        candidate.document.organization.pinned = pinned;
        candidate.document.timestamps.organization_modified_at =
            next_timestamp_after(&candidate.document.timestamps.organization_modified_at);
        let note_path = session.path.clone();
        let baseline_revision = session.baseline_revision.clone();
        let note_id = session.view.note_id.clone();
        let (revision, publish_warning) = match storage::save_note(
            &note_path,
            &self.recovery_root,
            &candidate,
            baseline_revision.as_deref(),
        ) {
            Ok(revision) => (revision, None),
            Err(error) => match storage::open_note(&note_path) {
                Ok(published) if published.package == candidate => (
                    published.revision,
                    Some(format!(
                        "笔记组织信息已完整发布并校验，但保存后步骤报告错误：{}",
                        error.message
                    )),
                ),
                _ => return Err(error),
            },
        };

        let active = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("staleSession", "笔记会话在整理期间已关闭。"))?;
        if active.view.session_id != session_id || active.view.note_id != note_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话在整理期间发生变化；已发布的组织信息仍保留在原笔记文件中。",
            ));
        }
        active.package = candidate;
        active.baseline_revision = Some(revision.clone());
        active.notebook_id = Some(notebook_id.to_owned());
        active.view.notebook_id = Some(notebook_id.to_owned());
        active.view.pinned = pinned;
        active.view.revision = Some(revision);
        let saved_session = active.view.clone();

        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library =
            match scan_library(&library_path, &library.notebooks, &default_notebook_id) {
                Ok(view) => {
                    library.view = view.clone();
                    view
                }
                Err(error) => {
                    let mut view = library.view.clone();
                    push_warning(
                        &mut view.warnings,
                        format!("笔记组织信息已保存，但无法刷新列表：{}", error.message),
                    );
                    library.view = view.clone();
                    view
                }
            };
        if let Some(warning) = publish_warning {
            push_warning(&mut receipt_library.warnings, warning);
            library.view.warnings = receipt_library.warnings.clone();
        }
        Ok(SaveReceipt {
            session: saved_session,
            library: receipt_library,
        })
    }

    pub fn delete_trashed_note(
        &mut self,
        session_id: &str,
        confirmed: bool,
    ) -> Result<WorkspaceView, WorkspaceError> {
        if !confirmed {
            return Err(StorageError::new(
                "confirmationRequired",
                "永久删除回收站笔记前需要明确确认。",
            ));
        }
        let (library_path, default_notebook_id) = self
            .library
            .as_ref()
            .map(|library| (library.path.clone(), library.default_notebook_id.clone()))
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let session = self.session.as_ref().ok_or_else(|| {
            StorageError::new("noSession", "当前没有可永久删除的回收站笔记会话。")
        })?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开选中的回收站笔记后再删除。",
            ));
        }
        if session.package.document.organization.trashed_at.is_none() {
            return Err(StorageError::new(
                "notTrashed",
                "只能永久删除回收站中的笔记。",
            ));
        }
        let note_id = session.view.note_id.clone();
        let note_path = session.path.clone();
        let expected_revision = session
            .baseline_revision
            .clone()
            .ok_or_else(|| StorageError::new("conflict", "回收站笔记缺少版本基线。"))?;
        let notes_directory = library_path.join("notes");
        ensure_child_directory(&library_path, &notes_directory, false)?;
        let expected_path = notes_directory.join(format!("{note_id}.cuenote"));
        if note_path != expected_path {
            return Err(StorageError::new(
                "unsafePath",
                "所选会话不再指向当前笔记库中的对应笔记文件。",
            ));
        }
        ensure_regular_child(&library_path, &expected_path)?;
        let deletion_warning = storage::delete_trashed_note(
            &expected_path,
            &self.recovery_root,
            &note_id,
            &expected_revision,
        )?;

        let preference_warning = if self.last_note_id.as_deref() == Some(note_id.as_str()) {
            self.last_note_id = None;
            self.persist_preference(&library_path, None, &self.preferences)
                .err()
                .map(|error| {
                    format!(
                        "笔记文件已永久删除，但无法更新本机上次笔记设置：{}",
                        error.message
                    )
                })
        } else {
            None
        };
        self.session = None;

        let library = self.library.as_mut().expect("library checked above");
        let mut updated =
            match scan_library(&library_path, &library.notebooks, &default_notebook_id) {
                Ok(view) => view,
                Err(error) => {
                    let mut view = library.view.clone();
                    view.notes.retain(|note| note.note_id != note_id);
                    view.trash_notes.retain(|note| note.note_id != note_id);
                    push_warning(
                        &mut view.warnings,
                        format!("笔记文件已永久删除，但无法刷新笔记列表：{}", error.message),
                    );
                    view
                }
            };
        if let Some(warning) = deletion_warning {
            push_warning(&mut updated.warnings, warning);
        }
        if let Some(warning) = preference_warning {
            push_warning(&mut updated.warnings, warning);
        }
        library.view = updated;
        Ok(self.view())
    }

    pub fn set_view_preferences(
        &mut self,
        sidebar_collapsed: bool,
        selected_notebook_id: Option<String>,
    ) -> Result<(), WorkspaceError> {
        if let Some(id) = selected_notebook_id.as_deref() {
            if !is_canonical_uuid_v4(id) {
                return Err(StorageError::new(
                    "invalidNotebook",
                    "笔记本筛选身份格式无效。",
                ));
            }
            if !self
                .library
                .as_ref()
                .is_some_and(|library| library.notebooks.contains_key(id))
            {
                return Err(StorageError::new(
                    "unknownNotebook",
                    "笔记本筛选不属于当前笔记库。",
                ));
            }
        }
        let preferences = ViewPreferences {
            sidebar_collapsed,
            selected_notebook_id,
        };
        let path = match self.library.as_ref() {
            Some(library) => library.path.clone(),
            None => self
                .read_latest_preference()?
                .map(|preference| PathBuf::from(preference.library_path))
                .unwrap_or_else(|| self.default_library_path.clone()),
        };
        self.persist_preference(&path, self.last_note_id.as_deref(), &preferences)?;
        self.preferences = preferences;
        Ok(())
    }

    pub fn list_recovery_drafts(&mut self) -> Result<RecoveryList, WorkspaceError> {
        let (drafts, warnings) = storage::list_recovery_drafts(&self.recovery_root)?;
        Ok(RecoveryList {
            drafts: drafts
                .into_iter()
                .map(|draft| RecoveryDraftSummary {
                    recovery_id: draft.recovery_id,
                    note_id: draft.note_id,
                    title: draft.title,
                    saved_at: draft.saved_at,
                    revision: draft.revision,
                })
                .collect(),
            warnings,
        })
    }

    pub fn restore_recovery_as_copy(
        &mut self,
        recovery_id: &str,
    ) -> Result<SaveReceipt, WorkspaceError> {
        let (library_path, default_notebook_id) = self
            .library
            .as_ref()
            .map(|library| (library.path.clone(), library.default_notebook_id.clone()))
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let source = storage::open_recovery_draft(&self.recovery_root, recovery_id)?;
        let mut package = source.package;
        assign_copy_identity(&mut package, &default_notebook_id);
        let note_id = package.document.note_id.clone();
        let path = library_path
            .join("notes")
            .join(format!("{note_id}.cuenote"));
        let revision = storage::save_note(&path, &self.recovery_root, &package, None)?;
        let restored = match storage::open_note(&path) {
            Ok(opened) if opened.revision == revision && opened.package == package => opened,
            Ok(_) => {
                let mut error = StorageError::new(
                    "publish",
                    "恢复副本已写入，但重新读取后的内容与请求不一致；源恢复草稿仍保留。",
                );
                error.recovery_path = Some(source.snapshot.path.to_string_lossy().into_owned());
                return Err(error);
            }
            Err(mut error) => {
                error.message = format!(
                    "恢复副本已写入，但无法重新读取；源恢复草稿仍保留：{}",
                    error.message
                );
                error.recovery_path = Some(source.snapshot.path.to_string_lossy().into_owned());
                return Err(error);
            }
        };
        let preference_warning = self
            .persist_preference(&library_path, Some(&note_id), &self.preferences)
            .err()
            .map(|error| format!("恢复副本已保存，但无法记录上次笔记位置：{}", error.message));
        if let Err(mut error) =
            storage::remove_recovery_snapshot(&self.recovery_root, &source.snapshot)
        {
            error.message = format!(
                "恢复副本已验证并保存，但无法清理已处理的源恢复草稿：{}",
                error.message
            );
            error.recovery_path = Some(source.snapshot.path.to_string_lossy().into_owned());
            return Err(error);
        }

        let library = self.library.as_mut().expect("library checked above");
        let read_only_reason = read_only_reason(
            &restored.package,
            restored
                .package
                .document
                .organization
                .notebook_id
                .as_deref(),
            &library.notebooks,
        );
        let session = note_session_from_opened(&note_id, &restored, read_only_reason);
        self.session = Some(ActiveNoteSession {
            view: session.clone(),
            path,
            package: restored.package,
            baseline_revision: Some(restored.revision),
            notebook_id: Some(default_notebook_id.clone()),
        });
        self.last_note_id = Some(note_id.clone());
        self.notice = preference_warning.clone();
        let receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            &library.default_notebook_id,
        ) {
            Ok(view) => {
                library.view = view.clone();
                view
            }
            Err(error) => {
                let mut view = library.view.clone();
                push_warning(
                    &mut view.warnings,
                    format!("恢复副本已保存，但无法刷新列表：{}", error.message),
                );
                library.view = view.clone();
                view
            }
        };
        let mut receipt_library = receipt_library;
        if let Some(warning) = preference_warning {
            push_warning(&mut receipt_library.warnings, warning);
            if let Some(library) = self.library.as_mut() {
                library.view.warnings = receipt_library.warnings.clone();
            }
        }
        Ok(SaveReceipt {
            session,
            library: receipt_library,
        })
    }

    pub fn save_current_as(
        &mut self,
        session_id: &str,
        text: PlainText,
        path: PathBuf,
    ) -> Result<SavedCopy, WorkspaceError> {
        let (mut package, default_notebook_id) = {
            let library = self
                .library
                .as_ref()
                .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
            let session = self
                .session
                .as_ref()
                .ok_or_else(|| StorageError::new("noSession", "当前没有可另存的笔记会话。"))?;
            if session.view.session_id != session_id {
                return Err(StorageError::new(
                    "staleSession",
                    "笔记会话已切换；请重新打开笔记后再另存。",
                ));
            }
            (session.package.clone(), library.default_notebook_id.clone())
        };
        replace_plain_text_for_copy(&mut package, text)?;
        assign_copy_identity(&mut package, &default_notebook_id);
        let destination = self.resolve_copy_destination(&path)?;
        let (saved_path, revision) = storage::save_copy(&destination, &package)?;
        Ok(SavedCopy {
            path: display_path(&saved_path),
            revision,
        })
    }

    pub fn save_current_content_as(
        &mut self,
        session_id: &str,
        content: NoteContent,
        path: PathBuf,
    ) -> Result<SavedCopy, WorkspaceError> {
        let (mut package, default_notebook_id) = {
            let library = self
                .library
                .as_ref()
                .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
            let session = self
                .session
                .as_ref()
                .ok_or_else(|| StorageError::new("noSession", "当前没有可另存的笔记会话。"))?;
            if session.view.session_id != session_id {
                return Err(StorageError::new(
                    "staleSession",
                    "笔记会话已切换；请重新打开笔记后再另存。",
                ));
            }
            (session.package.clone(), library.default_notebook_id.clone())
        };

        let original_content = package.content();
        if content != original_content {
            if !supports_editable_content(&original_content) || !supports_editable_content(&content)
            {
                return Err(StorageError::new(
                    "readOnly",
                    "当前笔记或另存内容包含编辑器尚不支持的结构；未创建副本。",
                ));
            }
            package.replace_content(content);
            package
                .validate()
                .map_err(|error| StorageError::new("format", error.to_string()))?;
        }
        assign_copy_identity(&mut package, &default_notebook_id);
        let destination = self.resolve_copy_destination(&path)?;
        let (saved_path, revision) = storage::save_copy(&destination, &package)?;
        Ok(SavedCopy {
            path: display_path(&saved_path),
            revision,
        })
    }

    pub fn save_recovery_as(
        &mut self,
        recovery_id: &str,
        path: PathBuf,
    ) -> Result<SavedCopy, WorkspaceError> {
        let default_notebook_id = self
            .library
            .as_ref()
            .map(|library| library.default_notebook_id.clone());
        let source = storage::open_recovery_draft(&self.recovery_root, recovery_id)?;
        let mut package = source.package;
        if let Some(default_notebook_id) = default_notebook_id.as_deref() {
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

    pub fn library_path(&self) -> Result<PathBuf, WorkspaceError> {
        self.library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "当前没有已打开的笔记库。"))
    }

    fn resolve_copy_destination(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let destination = storage::resolve_output_path(path)?;
        let recovery = resolve_future_path(&self.recovery_root)?;
        if destination.starts_with(&recovery)
            || self
                .library
                .as_ref()
                .is_some_and(|library| destination.starts_with(&library.path))
        {
            return Err(StorageError::new(
                "unsafePath",
                "另存目标必须位于当前笔记库和本机恢复目录之外。",
            ));
        }
        Ok(destination)
    }

    fn view(&self) -> WorkspaceView {
        WorkspaceView {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            default_library_path: display_path(&self.default_library_path),
            library: self.library.as_ref().map(|library| library.view.clone()),
            session: self.session.as_ref().map(|session| session.view.clone()),
            preferences: self.preferences.clone(),
            notice: self.notice.clone(),
        }
    }

    fn prepare_library(
        &self,
        requested: &Path,
        create: bool,
    ) -> Result<LibraryState, WorkspaceError> {
        let path = resolve_candidate_directory(requested, create)?;
        let recovery = resolve_future_path(&self.recovery_root)?;
        if path == recovery || recovery.starts_with(&path) || path.starts_with(&recovery) {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库与本机恢复目录必须分开；请选择其他位置。",
            ));
        }
        let config = resolve_future_path(&self.config_root)?;
        if path == config || config.starts_with(&path) {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库不能包含 CueNote 的本机设置目录；请选择其他位置。",
            ));
        }

        if create {
            fs::create_dir_all(&path)
                .map_err(|error| StorageError::new("io", format!("无法创建笔记库目录：{error}")))?;
        }
        let canonical = ensure_real_directory(&path, None)?;
        if canonical == recovery
            || recovery.starts_with(&canonical)
            || canonical.starts_with(&recovery)
        {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库与本机恢复目录必须分开；请选择其他位置。",
            ));
        }
        if canonical == config || config.starts_with(&canonical) {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库不能包含 CueNote 的本机设置目录；请选择其他位置。",
            ));
        }
        if create {
            storage::probe_directory(&canonical)?;
        }
        let notes_path = canonical.join("notes");
        let notebooks_path = canonical.join("notebooks");
        if create {
            ensure_child_directory(&canonical, &notes_path, true)?;
            ensure_child_directory(&canonical, &notebooks_path, true)?;
        } else {
            ensure_child_directory(&canonical, &notes_path, false)?;
            ensure_child_directory(&canonical, &notebooks_path, false)?;
        }

        let notes = secure_read_dir(&canonical, &notes_path)?;
        let notebooks = secure_read_dir(&canonical, &notebooks_path)?;
        let mut metadata = read_notebooks(&notebooks_path, notebooks)?;
        if metadata.is_empty() {
            let has_existing_notes = notes.iter().any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("cuenote"))
            });
            if has_existing_notes || !create {
                return Err(StorageError::new(
                    "libraryMetadata",
                    "笔记库缺少笔记本元数据；已有笔记不会被自动归入新建笔记本。",
                ));
            }
            let notebook = new_default_notebook();
            publish_notebook_metadata(&notebooks_path, &notebook)?;
            metadata.insert(notebook.id.clone(), notebook);
        }
        let defaults = metadata
            .values()
            .filter(|notebook| notebook.default)
            .count();
        if defaults != 1 {
            return Err(StorageError::new(
                "libraryMetadata",
                if defaults == 0 {
                    "笔记库没有默认笔记本元数据。"
                } else {
                    "笔记库包含多个默认笔记本元数据。"
                },
            ));
        }
        let default_notebook_id = metadata
            .values()
            .find(|notebook| notebook.default)
            .expect("exactly one default notebook was checked")
            .id
            .clone();
        let view = scan_library(&canonical, &metadata, &default_notebook_id)?;
        Ok(LibraryState {
            path: canonical,
            notebooks: metadata,
            default_notebook_id,
            view,
        })
    }

    fn load_existing_library(&self, path: &Path) -> Result<LibraryState, WorkspaceError> {
        self.prepare_library(path, false)
    }

    fn persist_preference(
        &self,
        path: &Path,
        last_note_id: Option<&str>,
        preferences: &ViewPreferences,
    ) -> Result<(), WorkspaceError> {
        ensure_config_directory(&self.config_root)?;
        prune_preferences(&self.config_root)?;
        let sequence = latest_preference_sequence(&self.config_root)?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| StorageError::new("settings", "笔记库设置序号已耗尽。"))?;
        let preference = WorkspacePreference {
            schema_version: PREFERENCE_SCHEMA_VERSION,
            sequence,
            saved_at: now_timestamp(),
            library_path: path.to_string_lossy().into_owned(),
            last_note_id: last_note_id.map(str::to_owned),
            sidebar_collapsed: preferences.sidebar_collapsed,
            selected_notebook_id: preferences.selected_notebook_id.clone(),
        };
        let bytes = serde_json::to_vec(&preference).map_err(|error| {
            StorageError::new("settings", format!("无法编码笔记库设置：{error}"))
        })?;
        publish_unique_snapshot(
            &self.config_root,
            &format!("workspace-{sequence:020}-{}.json", Uuid::new_v4()),
            &bytes,
            ".workspace-",
        )?;
        let _ = prune_preferences(&self.config_root);
        Ok(())
    }

    fn read_latest_preference(&self) -> Result<Option<WorkspacePreference>, WorkspaceError> {
        let metadata = match fs::symlink_metadata(&self.config_root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StorageError::new(
                    "settings",
                    format!("无法检查笔记库设置目录：{error}"),
                ));
            }
        };
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库设置位置必须是实际目录。",
            ));
        }
        let mut entries = fs::read_dir(&self.config_root)
            .map_err(|error| StorageError::new("settings", format!("无法读取笔记库设置：{error}")))?
            .filter_map(Result::ok)
            .filter(|entry| is_preference_filename(&entry.file_name()))
            .collect::<Vec<_>>();
        if entries.len() > MAX_PREFERENCE_SNAPSHOTS {
            return Err(StorageError::new(
                "settings",
                "笔记库设置快照数量超出安全上限；请保留当前目录并检查设置文件。",
            ));
        }
        entries.sort_by_key(|entry| entry.file_name());
        let Some(entry) = entries.pop() else {
            return Ok(None);
        };
        let file_metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            StorageError::new("settings", format!("无法检查笔记库设置快照：{error}"))
        })?;
        if is_link_or_reparse(&file_metadata) || !file_metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库设置快照必须是普通文件。",
            ));
        }
        let bytes = read_bounded(&entry.path(), MAX_PREFERENCE_BYTES, "笔记库设置")?;
        let preference: WorkspacePreference = serde_json::from_slice(&bytes).map_err(|error| {
            StorageError::new("settings", format!("笔记库设置快照无效：{error}"))
        })?;
        if preference.schema_version != PREFERENCE_SCHEMA_VERSION
            || preference.sequence == 0
            || preference.saved_at != preference.saved_at.trim()
            || !is_valid_timestamp(&preference.saved_at)
        {
            return Err(StorageError::new(
                "settings",
                "笔记库设置快照包含不支持的版本或时间。",
            ));
        }
        let path = PathBuf::from(&preference.library_path);
        if !path.is_absolute() {
            return Err(StorageError::new(
                "settings",
                "上次使用的笔记库路径不是绝对路径。",
            ));
        }
        if preference_sequence_from_filename(&entry.file_name()) != Some(preference.sequence) {
            return Err(StorageError::new(
                "settings",
                "笔记库设置快照序号与文件名不一致。",
            ));
        }
        Ok(Some(preference))
    }
}

fn read_only_reason(
    package: &NotePackage,
    notebook_id: Option<&str>,
    notebooks: &BTreeMap<String, NotebookMetadata>,
) -> Option<String> {
    if package.document.organization.trashed_at.is_some() {
        return Some("此笔记在回收站中，请先恢复后再编辑。".into());
    }
    if notebook_id.is_none() {
        return Some("此笔记缺少笔记本归属，当前以只读方式打开。".into());
    }
    if !notebooks.contains_key(notebook_id.unwrap()) {
        return Some("此笔记属于未知笔记本，当前以只读方式打开。".into());
    }
    if !supports_editable_content(&package.content()) {
        return Some("此笔记包含当前编辑器尚不支持的内容，当前以只读方式打开。".into());
    }
    None
}

fn supports_editable_content(content: &NoteContent) -> bool {
    supports_editable_blocks(&content.sections.cue, true)
        && supports_editable_blocks(&content.sections.content, false)
        && supports_editable_blocks(&content.sections.summary, true)
}

fn supports_editable_blocks(blocks: &[Block], side_section: bool) -> bool {
    blocks.iter().all(|block| match block {
        Block::Paragraph { children }
        | Block::Heading1 { children }
        | Block::Heading2 { children }
        | Block::Heading3 { children } => supports_editable_inlines(children, side_section),
        Block::OrderedList { items, .. } | Block::BulletList { items } => items
            .iter()
            .all(|item| supports_editable_blocks(&item.blocks, side_section)),
        Block::TaskList { items } => items
            .iter()
            .all(|item| supports_editable_blocks(&item.blocks, side_section)),
        Block::Blockquote { blocks } if !side_section => supports_editable_blocks(blocks, false),
        Block::HorizontalRule if !side_section => true,
        Block::Table { rows } if !side_section => rows.iter().all(|row| {
            let cells = match row {
                TableRow::Header { cells } | TableRow::Body { cells } => cells,
            };
            cells
                .iter()
                .all(|cell| supports_editable_inlines(cell, false))
        }),
        _ => false,
    })
}

fn supports_editable_inlines(inlines: &[Inline], side_section: bool) -> bool {
    inlines.iter().all(|inline| match inline {
        Inline::Text { marks, .. } => marks.iter().all(|mark| match mark {
            Mark::Bold => true,
            Mark::Italic | Mark::Strike | Mark::Link { .. } => !side_section,
            Mark::Code => false,
        }),
        Inline::HardBreak => true,
        Inline::MathInline { .. } => false,
    })
}

fn note_session_from_opened(
    note_id: &str,
    opened: &OpenedNote,
    read_only_reason: Option<String>,
) -> NoteSession {
    NoteSession {
        session_id: Uuid::new_v4().to_string(),
        note_id: note_id.to_owned(),
        notebook_id: opened.package.document.organization.notebook_id.clone(),
        pinned: opened.package.document.organization.pinned,
        trashed_at: opened.package.document.organization.trashed_at.clone(),
        text: opened.package.text_projection(),
        content: opened.package.content(),
        revision: Some(opened.revision.clone()),
        read_only_reason,
    }
}

fn scan_library(
    library_path: &Path,
    notebooks: &BTreeMap<String, NotebookMetadata>,
    default_notebook_id: &str,
) -> Result<LibraryView, WorkspaceError> {
    let notes_path = library_path.join("notes");
    let mut entries = secure_read_dir(library_path, &notes_path)?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_NOTE_FILES {
        return Err(StorageError::new(
            "libraryTooLarge",
            "笔记文件数量超出当前索引上限。",
        ));
    }
    let mut notes = Vec::new();
    let mut trash_notes = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_file_ids = HashSet::new();
    let mut seen_package_ids = HashMap::<String, bool>::new();
    let mut omitted_warnings = 0usize;
    for entry in entries {
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("cuenote") {
            continue;
        }
        let file_metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                push_warning_counted(
                    &mut warnings,
                    &mut omitted_warnings,
                    format!("无法检查笔记文件：{error}"),
                );
                continue;
            }
        };
        if is_link_or_reparse(&file_metadata) || !file_metadata.is_file() {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("已跳过非普通笔记文件：{}", display_path(&path)),
            );
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                "已跳过文件名无法读取的笔记。".into(),
            );
            continue;
        };
        if !is_canonical_uuid_v4(stem) {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记文件名不是有效身份，已跳过：{}", display_path(&path)),
            );
            continue;
        }
        if !seen_file_ids.insert(stem.to_owned()) {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("发现重复笔记身份，已跳过后续文件：{stem}"),
            );
            continue;
        }
        let opened = match storage::open_note(&path) {
            Ok(opened) => opened,
            Err(error) => {
                push_warning_counted(
                    &mut warnings,
                    &mut omitted_warnings,
                    format!("无法读取笔记 {}：{}", stem, error.message),
                );
                continue;
            }
        };
        let package_id = opened.package.document.note_id.clone();
        let previously_canonical = seen_package_ids.get(&package_id).copied();
        let duplicate_package_id = previously_canonical.is_some();
        if duplicate_package_id {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!(
                    "发现重复笔记身份，已检查相关文件：{}",
                    opened.package.document.note_id
                ),
            );
        }
        if opened.package.document.note_id != stem {
            seen_package_ids.entry(package_id).or_insert(false);
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记文件名与内容身份不一致，已跳过：{stem}"),
            );
            continue;
        }
        if previously_canonical == Some(true) {
            continue;
        }
        seen_package_ids.insert(package_id, true);
        let notebook_id = opened.package.document.organization.notebook_id.as_deref();
        let trashed_at = opened.package.document.organization.trashed_at.clone();
        if trashed_at.is_none() && notebook_id.is_none() {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记 {stem} 缺少笔记本归属，保存前需先处理。"),
            );
        } else if trashed_at.is_none() && !notebooks.contains_key(notebook_id.unwrap()) {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记 {stem} 属于未知笔记本，当前不会重新归属。"),
            );
        }
        let summary = note_summary_from_package(stem, &opened.package);
        if trashed_at.is_some() {
            trash_notes.push(summary);
        } else {
            notes.push(summary);
        }
    }
    if omitted_warnings > 0 {
        warnings.push(format!("另有 {omitted_warnings} 条笔记警告未显示。"));
    }
    sort_note_summaries(&mut notes);
    sort_trash_notes(&mut trash_notes);
    let default_notebook_name = notebooks
        .get(default_notebook_id)
        .map(|metadata| metadata.name.clone())
        .unwrap_or_else(|| "默认笔记本".into());
    let mut notebook_summaries = notebooks
        .values()
        .map(|notebook| NotebookSummary {
            notebook_id: notebook.id.clone(),
            name: notebook.name.clone(),
            is_default: notebook.default,
        })
        .collect::<Vec<_>>();
    notebook_summaries.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.notebook_id.cmp(&right.notebook_id))
    });
    Ok(LibraryView {
        path: display_path(library_path),
        default_notebook_name,
        default_notebook_id: default_notebook_id.to_owned(),
        notebooks: notebook_summaries,
        notes,
        trash_notes,
        warnings,
    })
}

fn sort_note_summaries(notes: &mut [NoteSummary]) {
    notes.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.note_id.cmp(&right.note_id))
    });
}

fn read_notebooks(
    notebooks_path: &Path,
    entries: Vec<fs::DirEntry>,
) -> Result<BTreeMap<String, NotebookMetadata>, WorkspaceError> {
    let mut metadata = BTreeMap::new();
    let mut sorted = entries;
    sorted.sort_by_key(|entry| entry.file_name());
    for entry in sorted {
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("json") {
            continue;
        }
        let file_metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new("libraryMetadata", format!("无法检查笔记本元数据：{error}"))
        })?;
        if is_link_or_reparse(&file_metadata) || !file_metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                format!("笔记本元数据不是普通文件：{}", display_path(&path)),
            ));
        }
        let bytes = read_bounded(&path, MAX_METADATA_BYTES, "笔记本元数据")?;
        let notebook: NotebookMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            StorageError::new(
                "libraryMetadata",
                format!("笔记本元数据无效 {}：{error}", display_path(&path)),
            )
        })?;
        let filename_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| StorageError::new("libraryMetadata", "笔记本元数据文件名无法读取。"))?;
        if notebook.schema_version != LIBRARY_SCHEMA_VERSION {
            return Err(StorageError::new(
                "unsupportedLibraryVersion",
                format!("笔记本元数据版本不受支持：{}", display_path(&path)),
            ));
        }
        if metadata.contains_key(&notebook.id) {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("发现重复笔记本身份：{}", notebook.id),
            ));
        }
        if !is_canonical_uuid_v4(&notebook.id) || notebook.id != filename_id {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("笔记本元数据身份与文件名不一致：{}", display_path(&path)),
            ));
        }
        if notebook.name.trim().is_empty()
            || notebook.name.chars().count() > 256
            || notebook.name.chars().any(char::is_control)
        {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("笔记本名称无效：{}", display_path(&path)),
            ));
        }
        if !is_valid_timestamp(&notebook.timestamps.created_at)
            || !is_valid_timestamp(&notebook.timestamps.modified_at)
        {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("笔记本时间格式无效：{}", display_path(&path)),
            ));
        }
        if metadata.insert(notebook.id.clone(), notebook).is_some() {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("发现重复笔记本身份：{}", display_path(notebooks_path)),
            ));
        }
        if metadata.len() > MAX_NOTEBOOKS {
            return Err(StorageError::new(
                "libraryTooLarge",
                "笔记本元数据数量超出当前上限。",
            ));
        }
    }
    Ok(metadata)
}

fn validated_notebook_name(name: &str) -> Result<String, WorkspaceError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 256 || name.chars().any(char::is_control) {
        return Err(StorageError::new(
            "invalidNotebookName",
            "笔记本名称必须非空、最多 256 个字符且不包含控制字符。",
        ));
    }
    Ok(name.to_owned())
}

fn ensure_unique_notebook_name(
    notebooks: &BTreeMap<String, NotebookMetadata>,
    name: &str,
    except_id: Option<&str>,
) -> Result<(), WorkspaceError> {
    let normalized = name.to_lowercase();
    if notebooks.values().any(|notebook| {
        Some(notebook.id.as_str()) != except_id && notebook.name.trim().to_lowercase() == normalized
    }) {
        return Err(StorageError::new(
            "duplicateNotebookName",
            "笔记本名称已存在；请使用不同名称。",
        ));
    }
    Ok(())
}

fn read_notebook_metadata_file(
    library_path: &Path,
    notebook_id: &str,
) -> Result<(PathBuf, Vec<u8>), WorkspaceError> {
    if !is_canonical_uuid_v4(notebook_id) {
        return Err(StorageError::new("invalidNotebook", "笔记本身份格式无效。"));
    }
    let requested_directory = library_path.join("notebooks");
    let directory = ensure_child_directory(library_path, &requested_directory, false)?;
    let path = directory.join(format!("{notebook_id}.json"));
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        StorageError::new("libraryMetadata", format!("无法检查笔记本元数据：{error}"))
    })?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            "笔记本元数据必须是笔记库内的普通文件。",
        ));
    }
    let canonical = fs::canonicalize(&path).map_err(|error| {
        StorageError::new("libraryMetadata", format!("无法解析元数据：{error}"))
    })?;
    if !canonical.starts_with(&directory) {
        return Err(StorageError::new(
            "unsafePath",
            "笔记本元数据路径解析到了笔记库之外。",
        ));
    }
    let bytes = read_bounded(&canonical, MAX_METADATA_BYTES, "笔记本元数据")?;
    Ok((canonical, bytes))
}

fn count_notebook_notes(
    library_path: &Path,
    notebook_id: &str,
    notebooks: &BTreeMap<String, NotebookMetadata>,
) -> Result<usize, WorkspaceError> {
    let notes_path = library_path.join("notes");
    let entries = secure_read_dir(library_path, &notes_path)?;
    let mut assigned = 0usize;
    for entry in entries {
        let path = entry.path();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cuenote"))
        {
            continue;
        }
        let file_metadata = fs::symlink_metadata(&path).map_err(|error| {
            StorageError::new(
                "notebookUsageUnknown",
                format!("无法检查笔记库中的文件；不能确认笔记本为空：{error}"),
            )
        })?;
        if is_link_or_reparse(&file_metadata) || !file_metadata.is_file() {
            return Err(StorageError::new(
                "notebookUsageUnknown",
                "笔记库中存在非普通笔记文件；不能确认笔记本为空。",
            ));
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                StorageError::new(
                    "notebookUsageUnknown",
                    "笔记库中存在无法识别的笔记身份；不能确认笔记本为空。",
                )
            })?;
        if !is_canonical_uuid_v4(stem) {
            return Err(StorageError::new(
                "notebookUsageUnknown",
                "笔记库中存在身份无效的笔记文件；不能确认笔记本为空。",
            ));
        }
        let opened = storage::open_note(&path).map_err(|error| {
            StorageError::new(
                "notebookUsageUnknown",
                format!("无法读取笔记；不能确认笔记本为空：{}", error.message),
            )
        })?;
        if opened.package.document.note_id != stem {
            return Err(StorageError::new(
                "notebookUsageUnknown",
                "笔记文件名与内容身份不一致；不能确认笔记本为空。",
            ));
        }
        if opened.package.document.organization.trashed_at.is_some() {
            continue;
        }
        let assigned_id = opened
            .package
            .document
            .organization
            .notebook_id
            .as_deref()
            .ok_or_else(|| {
                StorageError::new(
                    "notebookUsageUnknown",
                    "笔记缺少笔记本归属；不能确认笔记本为空。",
                )
            })?;
        if !notebooks.contains_key(assigned_id) {
            return Err(StorageError::new(
                "notebookUsageUnknown",
                "笔记属于未知笔记本；不能确认笔记本为空。",
            ));
        }
        if assigned_id == notebook_id {
            assigned += 1;
        }
    }
    Ok(assigned)
}

fn note_summary_from_package(note_id: &str, package: &NotePackage) -> NoteSummary {
    let projection = package.text_projection();
    let excerpt_source = if !projection.summary.trim().is_empty() {
        &projection.summary
    } else if !projection.content.trim().is_empty() {
        &projection.content
    } else {
        &projection.cue
    };
    NoteSummary {
        note_id: note_id.to_owned(),
        title: package.document.title.clone(),
        modified_at: package.document.timestamps.content_modified_at.clone(),
        trashed_at: package.document.organization.trashed_at.clone(),
        notebook_id: package.document.organization.notebook_id.clone(),
        pinned: package.document.organization.pinned,
        excerpt: truncate_excerpt(excerpt_source),
    }
}

fn sort_trash_notes(notes: &mut [NoteSummary]) {
    notes.sort_by(|left, right| {
        right
            .trashed_at
            .cmp(&left.trashed_at)
            .then_with(|| left.note_id.cmp(&right.note_id))
    });
}

fn truncate_excerpt(summary: &str) -> String {
    let normalized = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = normalized.chars();
    let excerpt = characters
        .by_ref()
        .take(MAX_NOTE_EXCERPT_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

fn new_default_notebook() -> NotebookMetadata {
    new_notebook("默认笔记本".into(), true)
}

fn new_notebook(name: String, default: bool) -> NotebookMetadata {
    let now = now_timestamp();
    NotebookMetadata {
        schema_version: LIBRARY_SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        name,
        default,
        timestamps: NotebookTimestamps {
            created_at: now.clone(),
            modified_at: now,
        },
    }
}

fn assign_copy_identity(package: &mut NotePackage, default_notebook_id: &str) {
    let created_at = next_timestamp_after(&package.document.timestamps.created_at);
    let organization_modified_at =
        next_timestamp_after(&package.document.timestamps.organization_modified_at);
    package.document.note_id = Uuid::new_v4().to_string();
    package.document.organization.notebook_id = Some(default_notebook_id.to_owned());
    package.document.organization.pinned = false;
    package.document.organization.trashed_at = None;
    package.document.timestamps.created_at = created_at;
    package.document.timestamps.organization_modified_at = organization_modified_at;
}

fn assign_copy_identity_preserving_organization(package: &mut NotePackage) {
    let created_at = next_timestamp_after(&package.document.timestamps.created_at);
    let organization_modified_at =
        next_timestamp_after(&package.document.timestamps.organization_modified_at);
    package.document.note_id = Uuid::new_v4().to_string();
    package.document.timestamps.created_at = created_at;
    package.document.timestamps.organization_modified_at = organization_modified_at;
}

fn next_timestamp_after(previous: &str) -> String {
    let now = Utc::now();
    let previous = chrono::DateTime::parse_from_rfc3339(previous)
        .ok()
        .map(|value| value.with_timezone(&Utc));
    let timestamp = match previous {
        Some(previous) if now.timestamp_millis() <= previous.timestamp_millis() => previous
            .timestamp_millis()
            .checked_add(1)
            .and_then(chrono::DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or(now),
        _ => now,
    };
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn replace_plain_text_for_copy(
    package: &mut NotePackage,
    text: PlainText,
) -> Result<(), WorkspaceError> {
    if package.plain_text().is_none() {
        if package.text_projection() == text {
            return Ok(());
        }
        return Err(StorageError::new(
            "readOnly",
            "当前笔记包含富内容，无法用纯文本编辑内容后另存；可以按原内容另存为完整副本。",
        ));
    }
    package
        .replace_plain_text(text)
        .map_err(|error| StorageError::new("format", error.to_string()))
}

fn publish_notebook_metadata(
    notebooks_path: &Path,
    notebook: &NotebookMetadata,
) -> Result<(), WorkspaceError> {
    let bytes = serde_json::to_vec(notebook)
        .map_err(|error| StorageError::new("libraryMetadata", error.to_string()))?;
    publish_unique_snapshot(
        notebooks_path,
        &format!("{}.json", notebook.id),
        &bytes,
        ".notebook-",
    )
}

fn ensure_child_directory(
    library_path: &Path,
    child: &Path,
    create: bool,
) -> Result<PathBuf, WorkspaceError> {
    match fs::symlink_metadata(child) {
        Ok(metadata) => {
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new(
                    "unsafePath",
                    format!("笔记库子目录必须是实际目录：{}", display_path(child)),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            fs::create_dir(child).map_err(|error| {
                StorageError::new("io", format!("无法创建笔记库子目录：{error}"))
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(StorageError::new(
                "libraryMetadata",
                format!("笔记库缺少必要子目录：{}", display_path(child)),
            ));
        }
        Err(error) => {
            return Err(StorageError::new(
                "io",
                format!("无法检查笔记库子目录：{error}"),
            ));
        }
    }
    let canonical_library = fs::canonicalize(library_path)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记库目录：{error}")))?;
    let canonical_child = fs::canonicalize(child)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记库子目录：{error}")))?;
    if !canonical_child.starts_with(&canonical_library) {
        return Err(StorageError::new(
            "unsafePath",
            "笔记库子目录解析到了笔记库之外的位置。",
        ));
    }
    Ok(canonical_child)
}

fn ensure_regular_child(root: &Path, path: &Path) -> Result<(), WorkspaceError> {
    let root = fs::canonicalize(root)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记库目录：{error}")))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查笔记文件：{error}")))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            "笔记路径必须指向笔记库内的普通文件。",
        ));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记文件：{error}")))?;
    if !canonical.starts_with(root.join("notes")) {
        return Err(StorageError::new(
            "unsafePath",
            "笔记文件解析到了笔记库之外的位置。",
        ));
    }
    Ok(())
}

fn secure_read_dir(root: &Path, directory: &Path) -> Result<Vec<fs::DirEntry>, WorkspaceError> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记库目录：{error}")))?;
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| StorageError::new("io", format!("无法检查库目录：{error}")))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "笔记库子目录必须是实际目录。",
        ));
    }
    let canonical_dir = fs::canonicalize(directory)
        .map_err(|error| StorageError::new("io", format!("无法解析库目录：{error}")))?;
    if !canonical_dir.starts_with(&canonical_root) {
        return Err(StorageError::new(
            "unsafePath",
            "库子目录解析到了笔记库之外的位置。",
        ));
    }
    fs::read_dir(canonical_dir)
        .map_err(|error| StorageError::new("io", format!("无法读取库目录：{error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::new("io", format!("无法读取库目录项目：{error}")))
}

fn resolve_candidate_directory(
    path: &Path,
    allow_missing: bool,
) -> Result<PathBuf, WorkspaceError> {
    let requested = normalize_path(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| StorageError::new("io", format!("无法解析所选目录：{error}")))?
            .join(path)
    });
    match fs::symlink_metadata(&requested) {
        Ok(metadata) => {
            if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(StorageError::new(
                    "unsafePath",
                    "所选笔记库必须是实际目录，不能是文件或重解析点。",
                ));
            }
            fs::canonicalize(&requested)
                .map_err(|error| StorageError::new("io", format!("无法解析所选笔记库：{error}")))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && allow_missing => {
            resolve_future_path(&requested)
        }
        Err(error) => Err(StorageError::new(
            "io",
            format!("无法访问所选笔记库：{error}"),
        )),
    }
}

fn resolve_future_path(path: &Path) -> Result<PathBuf, WorkspaceError> {
    let absolute = normalize_path(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| StorageError::new("io", format!("无法解析目录路径：{error}")))?
            .join(path)
    });
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| StorageError::new("io", "目录路径没有可解析的现存父目录。"))?;
                suffix.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| StorageError::new("io", "目录路径没有可解析的现存父目录。"))?;
            }
            Err(error) => {
                return Err(StorageError::new(
                    "io",
                    format!("无法检查目录路径：{error}"),
                ));
            }
        }
    }
    let mut resolved = fs::canonicalize(ancestor)
        .map_err(|error| StorageError::new("io", format!("无法解析目录父路径：{error}")))?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                normalized.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                }
            }
            std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn ensure_config_directory(path: &Path) -> Result<(), WorkspaceError> {
    fs::create_dir_all(path).map_err(|error| {
        StorageError::new("settings", format!("无法准备笔记库设置目录：{error}"))
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        StorageError::new("settings", format!("无法检查笔记库设置目录：{error}"))
    })?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "笔记库设置位置必须是实际目录。",
        ));
    }
    Ok(())
}

fn ensure_real_directory(
    path: &Path,
    required_parent: Option<&Path>,
) -> Result<PathBuf, WorkspaceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查所选目录：{error}")))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(StorageError::new(
            "unsafePath",
            "所选位置必须是实际目录，不能通过符号链接或重解析点访问。",
        ));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| StorageError::new("io", format!("无法解析所选目录：{error}")))?;
    if required_parent.is_some_and(|parent| !canonical.starts_with(parent)) {
        return Err(StorageError::new(
            "unsafePath",
            "所选目录解析到了授权位置之外。",
        ));
    }
    Ok(canonical)
}

fn read_bounded(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, WorkspaceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查{label}：{error}")))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            format!("{label}必须是普通文件。"),
        ));
    }
    if metadata.len() > limit {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("{label}超过大小上限。"),
        ));
    }
    let file = File::open(path)
        .map_err(|error| StorageError::new("io", format!("无法打开{label}：{error}")))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::new("io", format!("无法读取{label}：{error}")))?;
    if bytes.len() as u64 > limit {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("{label}超过大小上限。"),
        ));
    }
    Ok(bytes)
}

fn publish_unique_snapshot(
    directory: &Path,
    final_name: &str,
    bytes: &[u8],
    temporary_prefix: &str,
) -> Result<(), WorkspaceError> {
    let destination = directory.join(final_name);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(StorageError::new(
            "conflict",
            format!("拒绝覆盖现有元数据：{}", display_path(&destination)),
        ));
    }
    let temporary = directory.join(format!("{temporary_prefix}{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| StorageError::new("io", format!("无法创建临时元数据：{error}")))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| StorageError::new("io", format!("无法写入元数据：{error}")))?;
        file.flush()
            .map_err(|error| StorageError::new("io", format!("无法刷新元数据：{error}")))?;
        file.sync_all()
            .map_err(|error| StorageError::new("io", format!("无法同步元数据：{error}")))?;
        drop(file);
        fs::rename(&temporary, &destination)
            .map_err(|error| StorageError::new("io", format!("无法发布元数据快照：{error}")))?;
        sync_directory(directory)
            .map_err(|error| StorageError::new("io", format!("无法同步元数据目录：{error}")))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn prune_preferences(directory: &Path) -> Result<(), WorkspaceError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| StorageError::new("settings", format!("无法读取笔记库设置：{error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            StorageError::new("settings", format!("无法读取笔记库设置项目：{error}"))
        })?;
    entries.retain(|entry| is_preference_filename(&entry.file_name()));
    entries.sort_by_key(|entry| entry.file_name());
    let remove_count = entries.len().saturating_sub(RETAINED_PREFERENCE_SNAPSHOTS);
    for entry in entries.into_iter().take(remove_count) {
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            StorageError::new("settings", format!("无法检查旧设置快照：{error}"))
        })?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "旧的笔记库设置快照不是普通文件。",
            ));
        }
        fs::remove_file(entry.path()).map_err(|error| {
            StorageError::new("settings", format!("无法清理旧设置快照：{error}"))
        })?;
    }
    Ok(())
}

fn latest_preference_sequence(directory: &Path) -> Result<Option<u64>, WorkspaceError> {
    let mut latest = None;
    for entry in fs::read_dir(directory)
        .map_err(|error| StorageError::new("settings", format!("无法读取笔记库设置：{error}")))?
    {
        let entry = entry
            .map_err(|error| StorageError::new("settings", format!("无法读取设置项目：{error}")))?;
        if let Some(sequence) = preference_sequence_from_filename(&entry.file_name()) {
            latest = Some(latest.map_or(sequence, |current: u64| current.max(sequence)));
        }
    }
    Ok(latest)
}

fn is_preference_filename(name: &std::ffi::OsStr) -> bool {
    preference_sequence_from_filename(name).is_some()
}

fn preference_sequence_from_filename(name: &std::ffi::OsStr) -> Option<u64> {
    let name = name.to_str()?;
    let stem = name.strip_prefix("workspace-")?.strip_suffix(".json")?;
    if stem.len() < 38 {
        return None;
    }
    let (sequence, id_part) = stem.split_at(stem.len() - 37);
    let id = id_part.strip_prefix('-')?;
    if sequence.len() != 20 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if !is_canonical_uuid_v4(id) {
        return None;
    }
    sequence.parse().ok()
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(warning);
    }
}

fn push_warning_counted(warnings: &mut Vec<String>, omitted: &mut usize, warning: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(warning);
    } else {
        *omitted += 1;
    }
}

fn is_canonical_uuid_v4(value: &str) -> bool {
    Uuid::parse_str(value)
        .map(|id| id.to_string() == value && id.get_version_num() == 4)
        .unwrap_or(false)
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn is_valid_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| {
            timestamp
                .to_utc()
                .to_rfc3339_opts(SecondsFormat::Millis, true)
                == value
        })
        .unwrap_or(false)
}

fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        value.into_owned()
    }
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes() & 0x0000_0400 != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}
