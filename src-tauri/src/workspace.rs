//! The host-owned library selection and note editing session.
//!
//! The frontend receives only DTOs and opaque session identifiers. Paths,
//! package contents, organization fields, and revision baselines stay here.

use crate::note_format::{
    AssetManifestEntry, Block, Inline, Mark, NoteContent, NotePackage, PlainText, TableRow,
    MAX_EXPANDED_BYTES,
};
use crate::storage::{self, OpenedNote, StorageError};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

mod backup_restore;
mod backups;
mod cache_check;
mod external;
mod external_notes;
mod library_restore;
mod markdown_export;
mod scan_cache;
mod search;
pub use crate::storage::versions::{PreservedVersionSummary, VersionList};
pub use backup_restore::{
    BackupRestoreReceipt, PreparedBackupNoteRestore, ProtectedBackupNoteRestore,
};
pub use backups::{
    BackupCatalog, BackupContext, BackupControl, BackupJob, BackupKind, BackupNoteList,
    BackupNoteSummary, BackupOutcome, BackupPhase, BackupProgress, BackupSummary,
    VerifiedBackupNote,
};
pub use external::{ExternalProbe, ExternalReconciliation};
pub use external_notes::ExternalSaveReceipt;
pub use library_restore::{
    LibraryRestoreAction, LibraryRestorePreview, LibraryRestoreReceipt, LibraryRestoreStatus,
};
pub use markdown_export::MarkdownExportReceipt;
pub use search::{SearchHit, SearchResults, SearchSection, SearchSnippet, SearchStatus};

pub type WorkspaceError = StorageError;

const LIBRARY_SCHEMA_VERSION: u32 = 1;
const PREFERENCE_SCHEMA_VERSION: u32 = 1;
const MAX_NOTEBOOKS: usize = 1024;
const MAX_NOTE_FILES: usize = 100_000;
const MAX_WARNINGS: usize = 100;
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const NOTEBOOK_CONFLICT_PREFIX: &str = ".cuenote-notebook-conflict-";
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
    pub default_notebook_id: Option<String>,
    pub notebooks: Vec<NotebookSummary>,
    pub notes: Vec<NoteSummary>,
    pub trash_notes: Vec<NoteSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteSession {
    pub session_id: String,
    pub origin: SessionOrigin,
    pub note_id: String,
    pub notebook_id: Option<String>,
    pub pinned: bool,
    pub trashed_at: Option<String>,
    pub text: PlainText,
    pub content: NoteContent,
    pub assets: Vec<AssetManifestEntry>,
    pub revision: Option<String>,
    pub read_only_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum SessionOrigin {
    Library,
    External { path: String },
    PreservedPreview,
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
    pub library_restore: Option<LibraryRestoreStatus>,
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
    default_notebook_id: Option<String>,
    notebook_metadata_baselines: BTreeMap<String, Option<Vec<u8>>>,
    metadata_warnings: Vec<String>,
    view: LibraryView,
    search: SearchSnapshot,
}

struct SearchSnapshot {
    index: search::SearchIndex,
    available: bool,
    incomplete: bool,
    warnings: Vec<String>,
    generation: u64,
}

impl SearchSnapshot {
    fn unavailable(generation: u64, warnings: Vec<String>) -> Self {
        Self {
            index: search::SearchIndex::default(),
            available: false,
            incomplete: true,
            warnings,
            generation,
        }
    }
}

struct LibraryScan {
    view: LibraryView,
    search_index: search::SearchIndex,
    search_available: bool,
    incomplete: bool,
    generation: u64,
}

#[derive(Clone)]
struct SearchScanContext {
    generation: Arc<AtomicU64>,
    progress: Option<Arc<dyn Fn(SearchProgress) + Send + Sync>>,
    config_root: PathBuf,
    bypass_cache: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchProgress {
    pub library_path: String,
    pub phase: SearchStatus,
    pub checked: usize,
    pub total: usize,
    pub incomplete: bool,
}

struct NotebookMetadataScan {
    notebooks: BTreeMap<String, NotebookMetadata>,
    bytes: BTreeMap<String, Vec<u8>>,
    warnings: Vec<String>,
    directory_available: bool,
}

struct ActiveNoteSession {
    view: NoteSession,
    path: PathBuf,
    package: NotePackage,
    asset_stash: Option<AssetStash>,
    baseline_revision: Option<String>,
    notebook_id: Option<String>,
    baseline_witness: Option<storage::versions::VersionLease>,
    preserved_preview: bool,
    return_external_path: Option<PathBuf>,
    external_conflict: bool,
    was_writable: bool,
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
    asset_run: AssetRun,
    search_generation: Arc<AtomicU64>,
    search_progress: Option<Arc<dyn Fn(SearchProgress) + Send + Sync>>,
    library_restore: Option<LibraryRestoreStatus>,
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
            asset_run: AssetRun::new(),
            search_generation: Arc::new(AtomicU64::new(0)),
            search_progress: None,
            library_restore: None,
        }
    }

    /// Captures the selected library and its local backup destination for work
    /// that will continue after the workspace mutex is released.
    pub fn backup_context(&self) -> Result<BackupContext, WorkspaceError> {
        self.ensure_library_available()?;
        let library_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        BackupContext::from_workspace(
            library_path,
            self.config_root.clone(),
            self.recovery_root.clone(),
        )
    }

    /// Stores a picker-selected local directory as this device's backup
    /// preference and returns a context bound to the current library.
    pub fn set_backup_directory(
        &mut self,
        directory: PathBuf,
    ) -> Result<BackupContext, WorkspaceError> {
        self.ensure_library_available()?;
        let library_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        BackupContext::with_directory(
            library_path,
            self.config_root.clone(),
            self.recovery_root.clone(),
            directory,
        )
    }

    /// Persists a lightweight reminder that the selected library needs an
    /// automatic-backup check after this process closes. This does not scan
    /// the library or inspect the configured backup destination.
    pub fn mark_backup_check_due(&self, observed_local_date: &str) -> Result<(), WorkspaceError> {
        let Some(library) = self.library.as_ref() else {
            return Ok(());
        };
        backups::mark_backup_check_due(&self.config_root, &library.path, observed_local_date)
    }

    pub(crate) fn attach_search_generation(&mut self, generation: Arc<AtomicU64>) {
        self.search_generation = generation;
    }

    pub(crate) fn attach_search_progress(
        &mut self,
        progress: Arc<dyn Fn(SearchProgress) + Send + Sync>,
    ) {
        self.search_progress = Some(progress);
    }

    fn search_scan_context(&self) -> SearchScanContext {
        SearchScanContext {
            generation: Arc::clone(&self.search_generation),
            progress: self.search_progress.clone(),
            config_root: self.config_root.clone(),
            bypass_cache: false,
        }
    }

    pub fn snapshot(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        #[cfg(feature = "perf-acceptance")]
        let _trace = crate::performance::Trace::new("workspace-snapshot");
        let mut loaded_generation = None;
        let pending_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .or_else(|| {
                self.read_latest_preference()
                    .ok()
                    .flatten()
                    .map(|preference| PathBuf::from(preference.library_path))
            });
        let mut _startup_access = None;
        if let Some(path) = pending_path {
            let was_pending = self.library_restore.is_some();
            if self.install_pending_library_restore(&path)? {
                self.initialized = true;
                return Ok(self.view());
            }
            if storage::ensure_local_directory(&path).is_ok() {
                _startup_access = Some(storage::library_access::lock_shared(
                    &self.recovery_root,
                    &path,
                )?);
            }
            if was_pending {
                self.library = Some(self.load_existing_library(&path)?);
                self.session = None;
                self.last_note_id = None;
            }
        }
        let mut recovery_warnings = Vec::new();
        if !self.initialized {
            recovery_warnings = storage::recover_pending_publications(&self.recovery_root)?;
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
                            loaded_generation = Some(library.search.generation);
                            if library.default_notebook_id.is_some()
                                && library.metadata_warnings.is_empty()
                                && self
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
        // Initial loading already scanned every source and durable witness.
        // Later snapshots (and a concurrent invalidation) still perform a fresh
        // scan; do not repeat the same startup scan before the first response.
        if loaded_generation != Some(self.search_generation.load(Ordering::Acquire)) {
            let _ = self.refresh_selected_library();
        }
        if !recovery_warnings.is_empty() {
            if let Some(notice) = self.notice.take() {
                recovery_warnings.push(notice);
            }
            self.notice = Some(recovery_warnings.join("\n"));
        }
        Ok(self.view())
    }

    pub fn select_library(&mut self, path: PathBuf) -> Result<WorkspaceView, WorkspaceError> {
        self.ensure_library_transition_allowed()?;
        let candidate = resolve_candidate_directory(&path, true)?;
        if self.install_pending_library_restore(&candidate)? {
            self.persist_preference(&candidate, None, &self.preferences)?;
            self.initialized = true;
            return Ok(self.view());
        }
        // Construct the complete candidate before changing the visible workspace.
        let library = self.prepare_library(&path, true)?;
        let mut preferences = self.preferences.clone();
        if library.default_notebook_id.is_some()
            && library.metadata_warnings.is_empty()
            && preferences
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

    pub fn search_notes(
        &self,
        query: &str,
        notebook_id: Option<&str>,
        offset: usize,
    ) -> Result<SearchResults, WorkspaceError> {
        if query.chars().count() > search::MAX_QUERY_CHARS {
            return Err(StorageError::new(
                "invalidSearchQuery",
                format!("搜索词不能超过 {} 个字符。", search::MAX_QUERY_CHARS),
            ));
        }
        let Some(library) = self.library.as_ref() else {
            return Ok(SearchResults {
                library_path: display_path(&self.default_library_path),
                query: query.to_owned(),
                notebook_id: notebook_id.map(str::to_owned),
                status: SearchStatus::Unavailable,
                hits: Vec::new(),
                total: 0,
                offset,
                indexed_notes: 0,
                incomplete: true,
                warnings: vec!["请先选择或使用默认笔记库。".into()],
            });
        };
        let generation = self.search_generation.load(Ordering::Acquire);
        let mut dirty = generation != library.search.generation;
        let mut status = if dirty {
            SearchStatus::Building
        } else if library.search.available {
            SearchStatus::Ready
        } else {
            SearchStatus::Unavailable
        };
        let (mut hits, mut total) = if status == SearchStatus::Ready && !query.trim().is_empty() {
            library.search.index.search(query, notebook_id, offset)
        } else {
            (Vec::new(), 0)
        };
        if self.search_generation.load(Ordering::Acquire) != generation {
            dirty = true;
            status = SearchStatus::Building;
            hits.clear();
            total = 0;
        }
        let mut warnings = library.search.warnings.clone();
        if dirty {
            warnings.push("笔记库内容已变化，搜索索引正在重建。".into());
        }
        Ok(SearchResults {
            library_path: library.view.path.clone(),
            query: query.to_owned(),
            notebook_id: notebook_id.map(str::to_owned),
            status,
            hits,
            total,
            offset,
            indexed_notes: library.search.index.indexed_notes(),
            incomplete: library.search.incomplete || dirty,
            warnings,
        })
    }

    pub fn rebuild_search_index(&mut self) -> Result<LibraryView, WorkspaceError> {
        let generation = self.search_generation.load(Ordering::Acquire);
        if let Some(library) = self.library.as_mut() {
            library.search =
                SearchSnapshot::unavailable(generation, vec!["搜索索引正在重建。".into()]);
        }
        self.refresh_library_metadata_inner(true)?;
        let mut scan_context = self.search_scan_context();
        scan_context.bypass_cache = true;
        let generation = scan_context.generation.load(Ordering::Acquire);
        let Some(library) = self.library.as_ref() else {
            return Err(StorageError::new("noLibrary", "请先选择或使用默认笔记库。"));
        };
        let scan = scan_library_with_previous(
            &library.path,
            &library.notebooks,
            library.default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            Some(&library.view),
            &scan_context,
        );
        match scan {
            Ok(scan) => {
                let view = scan.view.clone();
                let library = self.library.as_mut().expect("library path was checked");
                install_library_scan(library, scan);
                Ok(view)
            }
            Err(error) => {
                if let Some(library) = self.library.as_mut() {
                    library.search = SearchSnapshot::unavailable(
                        generation,
                        vec![format!("无法重建搜索索引：{}", error.message)],
                    );
                }
                Err(error)
            }
        }
    }

    pub fn use_default_library(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        self.select_library(self.default_library_path.clone())
    }

    pub fn initialize_selected_library(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        self.ensure_library_available()?;
        let path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择笔记库。"))?;
        for entry in secure_read_dir(&path, &path)? {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                StorageError::new(
                    "libraryMetadataPending",
                    "所选目录包含无法识别的项目，不能安全初始化笔记库。",
                )
            })?;
            if name != "notes" && name != "notebooks" {
                return Err(StorageError::new(
                    "libraryMetadataPending",
                    format!(
                        "所选目录包含未识别的项目，不能安全初始化笔记库：{}",
                        display_path(&entry.path())
                    ),
                ));
            }
        }
        let notes_path = path.join("notes");
        let notebooks_path = path.join("notebooks");
        for directory in [&notes_path, &notebooks_path] {
            if let Some(entries) = secure_read_dir_if_present(&path, directory)? {
                if !entries.is_empty() {
                    return Err(StorageError::new(
                        "libraryMetadataPending",
                        format!(
                            "所选目录仍包含文件，无法安全初始化笔记库：{}",
                            display_path(directory)
                        ),
                    ));
                }
            }
        }
        ensure_child_directory(&path, &notes_path, true)?;
        ensure_child_directory(&path, &notebooks_path, true)?;
        let notebook = new_default_notebook();
        publish_notebook_metadata(&notebooks_path, &notebook)?;
        let library = self.prepare_library(&path, false)?;
        self.library = Some(library);
        self.notice = None;
        Ok(self.view())
    }

    pub fn new_note(&mut self) -> Result<NoteSession, WorkspaceError> {
        self.new_note_in(None)
    }

    pub fn new_note_in(
        &mut self,
        notebook_id: Option<&str>,
    ) -> Result<NoteSession, WorkspaceError> {
        self.refresh_library_metadata()?;
        let (library_path, assigned_notebook_id) = {
            let library = self
                .library
                .as_ref()
                .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
            let assigned = match notebook_id {
                Some(id) if library.notebooks.contains_key(id) => {
                    ensure_notebook_baseline_current(library, id)?;
                    id.to_owned()
                }
                Some(_) => {
                    return Err(StorageError::new(
                        "unknownNotebook",
                        "所选笔记本不存在于当前笔记库。",
                    ));
                }
                None => {
                    let id = library
                        .default_notebook_id
                        .clone()
                        .ok_or_else(library_metadata_pending)?;
                    ensure_notebook_baseline_current(library, &id)?;
                    id
                }
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
            origin: SessionOrigin::Library,
            note_id,
            notebook_id: Some(assigned_notebook_id.clone()),
            pinned: false,
            trashed_at: None,
            text: package.text_projection(),
            content: package.content(),
            assets: package.manifest.assets.clone(),
            revision: None,
            read_only_reason: None,
        };
        self.persist_preference(&library_path, Some(&view.note_id), &self.preferences)?;
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package,
            asset_stash: None,
            baseline_revision: None,
            notebook_id: Some(assigned_notebook_id),
            baseline_witness: None,
            preserved_preview: false,
            return_external_path: None,
            external_conflict: false,
            was_writable: true,
        });
        self.last_note_id = Some(view.note_id.clone());
        Ok(view)
    }

    pub fn open_note(&mut self, note_id: &str) -> Result<NoteSession, WorkspaceError> {
        self.refresh_library_metadata()?;
        let staged = self.stage_open_note(note_id)?;
        let view = staged.view.clone();
        if view.trashed_at.is_none() {
            let library_path = self
                .library
                .as_ref()
                .map(|library| library.path.clone())
                .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
            self.persist_preference(&library_path, Some(note_id), &self.preferences)?;
            self.last_note_id = Some(note_id.to_owned());
        }
        self.session = Some(staged);
        Ok(view)
    }

    fn stage_open_note(&self, note_id: &str) -> Result<ActiveNoteSession, WorkspaceError> {
        if !is_canonical_uuid_v4(note_id) {
            return Err(StorageError::new("invalidNoteId", "笔记身份格式无效。"));
        }
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let path = library
            .path
            .join("notes")
            .join(format!("{note_id}.cuenote"));
        ensure_regular_child(&library.path, &path)?;
        let opened = storage::open_note(&path)?;
        if opened.package.document.note_id != note_id {
            return Err(StorageError::new(
                "format",
                "笔记文件中的身份与文件名不一致。",
            ));
        }
        let (notebook_id, read_only_reason) = {
            let notebook_id = opened.package.document.organization.notebook_id.clone();
            let read_only_reason =
                read_only_reason(&opened.package, notebook_id.as_deref(), &library.notebooks);
            (notebook_id, read_only_reason)
        };
        let was_writable = read_only_reason.is_none();
        let view = note_session_from_opened(note_id, &opened, read_only_reason.clone());
        let baseline_witness = storage::versions::lease_observed_version(
            &self.recovery_root,
            &opened,
            &display_path(&path),
            "打开的笔记版本",
        )?;
        Ok(ActiveNoteSession {
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
        })
    }

    pub fn stage_image(
        &mut self,
        session_id: &str,
        bytes: Vec<u8>,
    ) -> Result<AssetManifestEntry, WorkspaceError> {
        self.stage_image_bytes(session_id, &bytes)
    }

    pub(crate) fn stage_image_bytes(
        &mut self,
        session_id: &str,
        bytes: &[u8],
    ) -> Result<AssetManifestEntry, WorkspaceError> {
        if !self.active_session_is_external() {
            self.refresh_library_metadata()?;
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可插入图片的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开笔记后再插入图片。",
            ));
        }
        if let Some(reason) = self.editor_read_only_reason(session) {
            return Err(StorageError::new("readOnly", reason));
        }
        if bytes.len() as u64 > MAX_EXPANDED_BYTES {
            return Err(StorageError::new(
                "resourceLimit",
                "图片原始数据超过 512 MiB 展开预算。",
            ));
        }

        let (media_type, width, height) = crate::note_format::inspect_image(bytes)
            .map_err(|error| StorageError::new(error.kind, error.message))?;
        let entry = AssetManifestEntry {
            id: Uuid::new_v4().to_string(),
            media_type,
            byte_length: bytes.len() as u64,
            sha256: format!("{:x}", sha2::Sha256::digest(bytes)),
            width,
            height,
        };
        let stash_path = self
            .asset_run
            .session_path(&self.recovery_root, session_id)?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("noSession", "图片暂存期间笔记会话已关闭。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "图片暂存期间笔记会话已切换。",
            ));
        }
        let stash = session
            .asset_stash
            .get_or_insert_with(|| AssetStash::new(stash_path));
        stash.put(&entry, bytes)?;
        Ok(entry)
    }

    pub fn read_image_asset(
        &self,
        session_id: &str,
        asset_id: &str,
    ) -> Result<Vec<u8>, WorkspaceError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可读取图片的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开笔记后读取图片。",
            ));
        }
        if !is_canonical_uuid_v4(asset_id) {
            return Err(StorageError::new(
                "invalidAssetId",
                "图片资产身份格式无效。",
            ));
        }
        if let Some(bytes) = session.package.assets.get(asset_id) {
            return Ok(bytes.clone());
        }
        if let Some(stash) = session.asset_stash.as_ref() {
            if let Some((entry, bytes)) = stash.read(asset_id)? {
                if entry.id == asset_id {
                    return Ok(bytes);
                }
            }
        }
        Err(StorageError::new(
            "missingAsset",
            "当前笔记会话中找不到该图片资产。",
        ))
    }

    pub fn trash_note(&mut self, session_id: &str) -> Result<SaveReceipt, WorkspaceError> {
        self.require_library_session(session_id)?;
        self.refresh_library_metadata()?;
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
        ensure_notebook_baseline_current(
            self.library.as_ref().expect("library checked above"),
            notebook_id,
        )?;

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
                Ok(published)
                    if error.kind == "postPublishCleanup" && published.package == candidate =>
                {
                    (
                        published.revision,
                        Some(format!(
                            "笔记已移入回收站并通过重读校验，但保存后步骤报告错误：{}",
                            error.message
                        )),
                    )
                }
                _ => return Err(self.retain_save_conflict(error)),
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
        active.retain_baseline(&self.recovery_root)?;
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

        let scan_context = self.search_scan_context();
        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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

    /// Release this process's image staging directories before the app exits.
    /// The persisted last-note preference intentionally remains unchanged.
    pub fn close(&mut self) -> Result<(), WorkspaceError> {
        if let Some(session) = self.session.as_mut() {
            session.release_asset_stash()?;
        }
        drop(self.session.take());
        self.asset_run.release()
    }

    pub fn restore_trashed_note(
        &mut self,
        session_id: &str,
        target_notebook_id: Option<&str>,
    ) -> Result<SaveReceipt, WorkspaceError> {
        self.require_library_session(session_id)?;
        self.refresh_library_metadata()?;
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
            Some(source) if notebooks.contains_key(source) => {
                ensure_notebook_baseline_current(
                    self.library.as_ref().expect("library checked above"),
                    source,
                )?;
                source.to_owned()
            }
            _ => {
                let target = target_notebook_id.ok_or_else(|| {
                    if default_notebook_id.is_none() {
                        library_metadata_pending()
                    } else {
                        StorageError::new(
                            "restoreTargetRequired",
                            "原笔记本已不存在，请选择一个现有笔记本后恢复。",
                        )
                    }
                })?;
                if !notebooks.contains_key(target) {
                    return Err(StorageError::new(
                        "unknownNotebook",
                        "恢复目标笔记本不存在于当前笔记库。",
                    ));
                }
                ensure_notebook_baseline_current(
                    self.library.as_ref().expect("library checked above"),
                    target,
                )?;
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
                Ok(published)
                    if error.kind == "postPublishCleanup" && published.package == candidate =>
                {
                    (
                        published.revision,
                        Some(format!(
                            "笔记已恢复并通过重读校验，但保存后步骤报告错误：{}",
                            error.message
                        )),
                    )
                }
                _ => return Err(self.retain_save_conflict(error)),
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
        active.retain_baseline(&self.recovery_root)?;
        let saved_session = active.view.clone();

        self.last_note_id = Some(note_id.clone());
        let preference_warning = self
            .persist_preference(&library_path, Some(&note_id), &self.preferences)
            .err()
            .map(|error| format!("笔记已恢复，但无法更新本机上次笔记设置：{}", error.message));

        let scan_context = self.search_scan_context();
        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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
        self.require_library_session(session_id)?;
        let library_path = self
            .library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let (candidate, baseline_revision) = {
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
            (candidate, session.baseline_revision.clone())
        };

        let _access = match self.acquire_library_access() {
            Ok(access) => access,
            Err(error) => {
                let session = self.session.as_ref().expect("session validated above");
                return Err(preserve_draft_after_save_access_failure(
                    error,
                    session,
                    &self.recovery_root,
                    &candidate,
                ));
            }
        };
        if let Err(error) = self.refresh_library_metadata() {
            let session = self.session.as_ref().expect("session validated above");
            return Err(preserve_draft_after_save_access_failure(
                error,
                session,
                &self.recovery_root,
                &candidate,
            ));
        }
        let notebooks = self
            .library
            .as_ref()
            .expect("library validated above")
            .notebooks
            .clone();
        let session = self.session.as_ref().expect("session validated above");
        if let Some(reason) =
            read_only_reason(&session.package, session.notebook_id.as_deref(), &notebooks)
        {
            let error = StorageError::new("readOnly", reason);
            let library = self.library.as_ref().expect("library validated above");
            return Err(preserve_draft_after_metadata_loss(
                error,
                session,
                library,
                &self.recovery_root,
                &candidate,
            ));
        }
        let revision = storage::save_note(
            &session.path,
            &self.recovery_root,
            &candidate,
            baseline_revision.as_deref(),
        )
        .map_err(|error| self.retain_save_conflict(error))?;

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
        active.view.read_only_reason = None;
        active.was_writable = true;
        active.retain_baseline(&self.recovery_root)?;
        let saved_session = active.view.clone();

        let scan_context = self.search_scan_context();
        let library = self.library.as_mut().expect("library checked above");
        let receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            library.default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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

    pub fn preflight_image_insert(
        &mut self,
        session_id: &str,
        content: NoteContent,
    ) -> Result<(), WorkspaceError> {
        if !self.active_session_is_external() {
            self.refresh_library_metadata()?;
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可插入图片的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开笔记后再插入图片。",
            ));
        }
        if let Some(reason) = self.editor_read_only_reason(session) {
            return Err(StorageError::new("readOnly", reason.clone()));
        }
        candidate_for_content(session, &content).map(|_| ())
    }

    pub fn preflight_note_content(
        &self,
        session_id: &str,
        content: NoteContent,
    ) -> Result<(), WorkspaceError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| StorageError::new("noSession", "当前没有可编辑的笔记会话。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开笔记后再检查内容。",
            ));
        }
        if let Some(reason) = self.editor_read_only_reason(session) {
            return Err(StorageError::new("readOnly", reason.clone()));
        }
        candidate_for_content(session, &content).map(|_| ())
    }

    fn preserve_removed_assets(
        &mut self,
        session_id: &str,
        candidate: &NotePackage,
    ) -> Result<(), WorkspaceError> {
        let removed = {
            let session = self
                .session
                .as_ref()
                .ok_or_else(|| StorageError::new("noSession", "当前没有可保存的笔记会话。"))?;
            if session.view.session_id != session_id {
                return Err(StorageError::new(
                    "staleSession",
                    "笔记会话已切换；请重新打开笔记后再保存。",
                ));
            }
            let candidate_ids = candidate
                .manifest
                .assets
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<HashSet<_>>();
            session
                .package
                .manifest
                .assets
                .iter()
                .filter(|entry| !candidate_ids.contains(entry.id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        };
        if removed.is_empty() {
            return Ok(());
        }

        let stash_path = self
            .asset_run
            .session_path(&self.recovery_root, session_id)?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| StorageError::new("noSession", "保存期间笔记会话已关闭。"))?;
        if session.view.session_id != session_id {
            return Err(StorageError::new(
                "staleSession",
                "笔记会话已切换；请重新打开笔记后再保存。",
            ));
        }
        let ActiveNoteSession {
            package,
            asset_stash,
            ..
        } = session;
        let stash = asset_stash.get_or_insert_with(|| AssetStash::new(stash_path));
        for entry in removed {
            let bytes = package.assets.get(&entry.id).ok_or_else(|| {
                StorageError::new("assetIntegrity", "要保留的图片文件在笔记包中缺失。")
            })?;
            stash.put(&entry, bytes)?;
        }
        Ok(())
    }

    pub fn save_note_content(
        &mut self,
        session_id: &str,
        content: NoteContent,
    ) -> Result<SaveReceipt, WorkspaceError> {
        #[cfg(feature = "perf-acceptance")]
        let mut trace = crate::performance::Trace::new("save-note-content");
        self.require_library_session(session_id)?;
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
            let candidate = candidate_for_content(session, &content)?;
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

        let _access = match self.acquire_library_access() {
            Ok(access) => access,
            Err(error) => {
                let session = self.session.as_ref().expect("session validated above");
                return Err(preserve_draft_after_save_access_failure(
                    error,
                    session,
                    &self.recovery_root,
                    &candidate,
                ));
            }
        };
        if let Err(error) = self.refresh_library_metadata() {
            let session = self.session.as_ref().expect("session validated above");
            return Err(preserve_draft_after_save_access_failure(
                error,
                session,
                &self.recovery_root,
                &candidate,
            ));
        }
        let notebooks = self
            .library
            .as_ref()
            .expect("library validated above")
            .notebooks
            .clone();
        let session = self.session.as_ref().expect("session validated above");
        if let Some(reason) =
            read_only_reason(&session.package, session.notebook_id.as_deref(), &notebooks)
        {
            let error = StorageError::new("readOnly", reason);
            let library = self.library.as_ref().expect("library validated above");
            return Err(preserve_draft_after_metadata_loss(
                error,
                session,
                library,
                &self.recovery_root,
                &candidate,
            ));
        }

        self.preserve_removed_assets(session_id, &candidate)?;

        #[cfg(feature = "perf-acceptance")]
        trace.mark("candidate-and-metadata");

        let (revision, publish_warning) = match storage::save_note(
            &note_path,
            &self.recovery_root,
            &candidate,
            baseline_revision.as_deref(),
        ) {
            Ok(revision) => (revision, None),
            Err(error) => match storage::open_note(&note_path) {
                Ok(published)
                    if error.kind == "postPublishCleanup" && published.package == candidate =>
                {
                    (
                        published.revision,
                        Some(format!(
                            "笔记已保存并通过重读校验，但保存后步骤报告错误：{}",
                            error.message
                        )),
                    )
                }
                _ => return Err(self.retain_save_conflict(error)),
            },
        };

        #[cfg(feature = "perf-acceptance")]
        trace.mark("native-save");
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
        active.view.assets = candidate.manifest.assets.clone();
        active.view.text = candidate.text_projection();
        active.view.revision = Some(revision);
        active.view.read_only_reason = None;
        active.was_writable = true;
        active.retain_baseline(&self.recovery_root)?;
        let saved_session = active.view.clone();

        #[cfg(feature = "perf-acceptance")]
        trace.mark("history-retention");

        let scan_context = self.search_scan_context();
        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            library.default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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
        #[cfg(feature = "perf-acceptance")]
        trace.mark("library-scan");
        Ok(SaveReceipt {
            session: saved_session,
            library: receipt_library,
        })
    }

    pub fn create_notebook(&mut self, name: &str) -> Result<LibraryView, WorkspaceError> {
        self.refresh_library_metadata()?;
        let name = validated_notebook_name(name)?;
        let (library_path, default_notebook_id, mut notebooks, metadata_warnings) = self
            .library
            .as_ref()
            .map(|library| {
                (
                    library.path.clone(),
                    library.default_notebook_id.clone(),
                    library.notebooks.clone(),
                    library.metadata_warnings.clone(),
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
        let candidate_bytes = serde_json::to_vec(&published_notebook)
            .map_err(|error| StorageError::new("libraryMetadata", error.to_string()))?;
        notebooks.insert(notebook_id.clone(), notebook);
        ensure_child_directory(&library_path, &metadata_path, true)?;
        let scan_context = self.search_scan_context();
        let scan = scan_library(
            &library_path,
            &notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &metadata_warnings,
            &scan_context,
        )?;
        let view = scan.view.clone();
        publish_notebook_metadata(&metadata_path, &published_notebook)?;
        let library = self.library.as_mut().expect("library checked above");
        library.notebooks = notebooks;
        library
            .notebook_metadata_baselines
            .insert(notebook_id, Some(candidate_bytes));
        install_library_scan(library, scan);
        Ok(view)
    }

    pub fn rename_notebook(
        &mut self,
        notebook_id: &str,
        name: &str,
    ) -> Result<LibraryView, WorkspaceError> {
        self.refresh_library_metadata()?;
        let name = validated_notebook_name(name)?;
        let (library_path, default_notebook_id, current, mut notebooks, mut metadata_warnings) =
            self.library
                .as_ref()
                .map(|library| {
                    (
                        library.path.clone(),
                        library.default_notebook_id.clone(),
                        library.notebooks.get(notebook_id).cloned(),
                        library.notebooks.clone(),
                        library.metadata_warnings.clone(),
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
        let (metadata_path, expected_bytes) = notebook_metadata_cas(
            self.library.as_ref().expect("library checked above"),
            notebook_id,
        )?;

        notebook.name = name;
        notebook.timestamps.modified_at = next_timestamp_after(&notebook.timestamps.modified_at);
        let candidate_bytes = serde_json::to_vec(&notebook).map_err(|error| {
            StorageError::new("libraryMetadata", format!("无法编码笔记本元数据：{error}"))
        })?;
        notebooks.insert(notebook_id.to_owned(), notebook);
        metadata_warnings.retain(|warning| !warning.starts_with("发现重名笔记本："));
        for warning in duplicate_notebook_name_warnings(&notebooks) {
            push_warning(&mut metadata_warnings, warning);
        }
        let scan_context = self.search_scan_context();
        let scan = scan_library(
            &library_path,
            &notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &metadata_warnings,
            &scan_context,
        )?;
        let view = scan.view.clone();
        if let Err(error) =
            storage::replace_metadata_file(&metadata_path, &expected_bytes, &candidate_bytes)
        {
            let _ = self.refresh_library_metadata();
            return Err(error);
        }

        let library = self.library.as_mut().expect("library checked above");
        library.notebooks = notebooks;
        library
            .notebook_metadata_baselines
            .insert(notebook_id.to_owned(), Some(candidate_bytes));
        library.metadata_warnings = metadata_warnings;
        install_library_scan(library, scan);
        Ok(view)
    }

    pub fn delete_notebook(&mut self, notebook_id: &str) -> Result<LibraryView, WorkspaceError> {
        self.refresh_library_metadata()?;
        let (
            library_path,
            default_notebook_id,
            current,
            mut notebooks,
            unsaved_session_uses_it,
            metadata_warnings,
        ) = self
            .library
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
                    library.metadata_warnings.clone(),
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

        let (metadata_path, expected_bytes) = notebook_metadata_cas(
            self.library.as_ref().expect("library checked above"),
            notebook_id,
        )?;
        notebooks.remove(notebook_id);
        let scan_context = self.search_scan_context();
        let mut scan = scan_library(
            &library_path,
            &notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &metadata_warnings,
            &scan_context,
        )?;
        let mut view = scan.view.clone();
        let (_, current_bytes) = read_notebook_metadata_file(&library_path, notebook_id)?;
        if current_bytes != expected_bytes {
            return Err(StorageError::new(
                "conflict",
                "笔记本元数据在删除前发生变化；原文件已保留。",
            ));
        }
        if let Err(error) =
            storage::delete_metadata_file_if_matches(&metadata_path, &expected_bytes)
        {
            let _ = self.refresh_library_metadata();
            return Err(error);
        }
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
        self.library
            .as_mut()
            .expect("library checked above")
            .notebook_metadata_baselines
            .insert(notebook_id.to_owned(), None);
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
        scan.view = view.clone();
        install_library_scan(self.library.as_mut().expect("library checked above"), scan);
        Ok(view)
    }

    pub fn update_note_organization(
        &mut self,
        session_id: &str,
        notebook_id: &str,
        pinned: bool,
    ) -> Result<SaveReceipt, WorkspaceError> {
        self.require_library_session(session_id)?;
        self.refresh_library_metadata()?;
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
        {
            let library = self.library.as_ref().expect("library checked above");
            ensure_notebook_baseline_current(library, notebook_id)?;
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
        ensure_notebook_baseline_current(
            self.library.as_ref().expect("library checked above"),
            source_notebook_id,
        )?;

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
                Ok(published)
                    if error.kind == "postPublishCleanup" && published.package == candidate =>
                {
                    (
                        published.revision,
                        Some(format!(
                            "笔记组织信息已完整发布并校验，但保存后步骤报告错误：{}",
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
        active.retain_baseline(&self.recovery_root)?;
        let saved_session = active.view.clone();

        let scan_context = self.search_scan_context();
        let library = self.library.as_mut().expect("library checked above");
        let mut receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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
        self.require_library_session(session_id)?;
        self.ensure_library_available()?;
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

        let scan_context = self.search_scan_context();
        let library = self.library.as_mut().expect("library checked above");
        let mut updated = match scan_library(
            &library_path,
            &library.notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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
        self.refresh_library_metadata()?;
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
        self.refresh_library_metadata()?;
        let (library_path, default_notebook_id) = self
            .library
            .as_ref()
            .map(|library| (library.path.clone(), library.default_notebook_id.clone()))
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let default_notebook_id = default_notebook_id.ok_or_else(library_metadata_pending)?;
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

        let scan_context = self.search_scan_context();
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
        let baseline_witness = storage::versions::lease_observed_version(
            &self.recovery_root,
            &restored,
            &display_path(&path),
            "恢复后保存的笔记版本",
        )?;
        self.session = Some(ActiveNoteSession {
            view: session.clone(),
            path,
            package: restored.package,
            asset_stash: None,
            baseline_revision: Some(restored.revision),
            notebook_id: Some(default_notebook_id.clone()),
            baseline_witness: Some(baseline_witness),
            preserved_preview: false,
            return_external_path: None,
            external_conflict: false,
            was_writable: session.read_only_reason.is_none(),
        });
        self.last_note_id = Some(note_id.clone());
        self.notice = preference_warning.clone();
        let receipt_library = match scan_library(
            &library_path,
            &library.notebooks,
            library.default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            &scan_context,
        ) {
            Ok(scan) => {
                let view = scan.view.clone();
                install_library_scan(library, scan);
                view
            }
            Err(error) => {
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
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
            let default_notebook_id = self
                .library
                .as_ref()
                .and_then(|library| library.default_notebook_id.clone());
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
            (session.package.clone(), default_notebook_id)
        };
        replace_plain_text_for_copy(&mut package, text)?;
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

    pub fn save_current_content_as(
        &mut self,
        session_id: &str,
        content: NoteContent,
        path: PathBuf,
    ) -> Result<SavedCopy, WorkspaceError> {
        let (mut package, default_notebook_id) = {
            let default_notebook_id = self
                .library
                .as_ref()
                .and_then(|library| library.default_notebook_id.clone());
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
            let original_content = session.package.content();
            let package = if content == original_content {
                session.package.clone()
            } else {
                if !supports_editable_content(&original_content) {
                    return Err(StorageError::new(
                        "readOnly",
                        "当前笔记包含编辑器尚不支持的结构；未创建副本。",
                    ));
                }
                candidate_for_content(session, &content)?
            };
            (package, default_notebook_id)
        };
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

    pub fn save_recovery_as(
        &mut self,
        recovery_id: &str,
        path: PathBuf,
    ) -> Result<SavedCopy, WorkspaceError> {
        let default_notebook_id = self
            .library
            .as_ref()
            .and_then(|library| library.default_notebook_id.clone());
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

    fn resolve_copy_destination(
        &self,
        path: &Path,
    ) -> Result<storage::ResolvedTarget, WorkspaceError> {
        let destination = storage::resolve_output_path(path)?;
        let recovery = resolve_future_path(&self.recovery_root)?;
        if destination.path().starts_with(&recovery)
            || self
                .library
                .as_ref()
                .is_some_and(|library| destination.path().starts_with(&library.path))
        {
            return Err(StorageError::new(
                "unsafePath",
                "另存目标必须位于当前笔记库和本机恢复目录之外。",
            ));
        }
        Ok(destination)
    }

    fn view(&self) -> WorkspaceView {
        let session = self.session.as_ref().map(|session| {
            let mut view = session.view.clone();
            if matches!(session.view.origin, SessionOrigin::Library) && !session.external_conflict {
                if let Some(library) = self.library.as_ref() {
                    view.read_only_reason = read_only_reason(
                        &session.package,
                        session.notebook_id.as_deref(),
                        &library.notebooks,
                    );
                }
            }
            view
        });
        WorkspaceView {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            default_library_path: display_path(&self.default_library_path),
            library: self.library.as_ref().map(|library| library.view.clone()),
            session,
            preferences: self.preferences.clone(),
            notice: self.notice.clone(),
            library_restore: self.library_restore.clone(),
        }
    }

    pub(crate) fn ensure_library_available(&self) -> Result<(), WorkspaceError> {
        if self.library_restore.is_some() {
            return Err(StorageError::new(
                "libraryRestorePending",
                "请先继续或回退未完成的整库恢复。",
            ));
        }
        if let Some(library) = self.library.as_ref() {
            storage::library_access::ensure_no_restore_pending(&self.recovery_root, &library.path)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_library_transition_allowed(&self) -> Result<(), WorkspaceError> {
        if self.library_restore.is_some() {
            return Err(StorageError::new(
                "libraryRestorePending",
                "请先继续或回退未完成的整库恢复。",
            ));
        }
        let library_path = match self.library.as_ref() {
            Some(library) => Some(library.path.clone()),
            None => self
                .read_latest_preference()?
                .map(|preference| PathBuf::from(preference.library_path)),
        };
        let Some(library_path) = library_path else {
            return Ok(());
        };
        storage::library_access::ensure_no_restore_pointer(&self.recovery_root, &library_path)?;
        match fs::symlink_metadata(&library_path) {
            Ok(_) => storage::library_access::ensure_no_restore_marker(&library_path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(StorageError::new(
                "io",
                format!("无法确认当前笔记库恢复状态：{error}"),
            )),
        }
    }

    pub(crate) fn acquire_library_access(
        &self,
    ) -> Result<Option<storage::library_access::LibraryAccessGuard>, WorkspaceError> {
        self.ensure_library_available()?;
        self.library
            .as_ref()
            .map(|library| storage::library_access::lock_shared(&self.recovery_root, &library.path))
            .transpose()
    }

    fn install_pending_library_restore(
        &mut self,
        requested: &Path,
    ) -> Result<bool, WorkspaceError> {
        match storage::library_access::restore_pointer_present(&self.recovery_root, requested) {
            Ok(true) => {
                let path =
                    resolve_future_path(requested).unwrap_or_else(|_| requested.to_path_buf());
                let status = self
                    .pending_library_restore(requested)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| {
                        blocked_restore_status(
                            &path,
                            "本机存在此笔记库的整库恢复指针；无法确认其完整性，已暂停普通访问。",
                        )
                    });
                self.install_pending_library_restore_shell(path, status);
                return Ok(true);
            }
            Ok(false) => {}
            Err(error) => {
                let status = blocked_restore_status(
                    requested,
                    &format!(
                        "无法检查本机整库恢复指针；已暂停普通访问：{}",
                        error.message
                    ),
                );
                self.install_pending_library_restore_shell(requested.to_path_buf(), status);
                return Ok(true);
            }
        }

        let resolved_identity = resolve_future_path(requested)?;
        let path = match fs::symlink_metadata(requested) {
            Ok(_) => resolve_candidate_directory(requested, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => resolved_identity,
            Err(error) => {
                return Err(StorageError::new(
                    "io",
                    format!("无法检查整库恢复笔记库：{error}"),
                ));
            }
        };
        let pointer_exists =
            match storage::library_access::restore_pointer_present(&self.recovery_root, &path) {
                Ok(exists) => exists,
                Err(error) => {
                    let status = blocked_restore_status(
                        &path,
                        &format!(
                            "无法检查本机整库恢复指针；已暂停普通访问：{}",
                            error.message
                        ),
                    );
                    self.install_pending_library_restore_shell(path, status);
                    return Ok(true);
                }
            };
        let marker = path.join(storage::library_access::RESTORE_MARKER);
        let marker_exists = match fs::symlink_metadata(&marker) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                let status = blocked_restore_status(
                    &path,
                    &format!("无法检查笔记库整库恢复标记；已暂停普通访问：{error}"),
                );
                self.install_pending_library_restore_shell(path, status);
                return Ok(true);
            }
        };
        let possibly_pending = pointer_exists || marker_exists;
        if !possibly_pending {
            self.library_restore = None;
            return Ok(false);
        }
        let status = match self.pending_library_restore(&path) {
            Ok(Some(status)) => status,
            Ok(None) => {
                self.library_restore = None;
                return Ok(false);
            }
            Err(error) => LibraryRestoreStatus {
                restore_id: String::new(),
                library_path: display_path(&path),
                source_backup_id: String::new(),
                pre_restore_backup: None,
                phase: "blocked".into(),
                completed_files: 0,
                total_files: 0,
                can_continue: false,
                can_roll_back: false,
                message: format!(
                    "无法读取整库恢复记录；当前库保持停止访问：{}",
                    error.message
                ),
            },
        };
        self.install_pending_library_restore_shell(path, status);
        Ok(true)
    }

    fn install_pending_library_restore_shell(
        &mut self,
        path: PathBuf,
        status: LibraryRestoreStatus,
    ) {
        let warnings = vec![status.message.clone()];
        self.library = Some(LibraryState {
            path: path.clone(),
            notebooks: BTreeMap::new(),
            default_notebook_id: None,
            notebook_metadata_baselines: BTreeMap::new(),
            metadata_warnings: warnings.clone(),
            view: LibraryView {
                path: display_path(&path),
                default_notebook_name: String::new(),
                default_notebook_id: None,
                notebooks: Vec::new(),
                notes: Vec::new(),
                trash_notes: Vec::new(),
                warnings: warnings.clone(),
            },
            search: SearchSnapshot::unavailable(
                self.search_generation.load(Ordering::Acquire),
                warnings,
            ),
        });
        self.session = None;
        self.last_note_id = None;
        self.library_restore = Some(status);
    }

    pub(super) fn refresh_library_metadata(&mut self) -> Result<(), WorkspaceError> {
        self.refresh_library_metadata_inner(false)
    }

    fn refresh_library_metadata_inner(
        &mut self,
        accept_visible_baseline: bool,
    ) -> Result<(), WorkspaceError> {
        self.ensure_library_available()?;
        let Some(path) = self.library.as_ref().map(|library| library.path.clone()) else {
            return Ok(());
        };
        let mut metadata = read_library_metadata(&path);
        let library = self.library.as_mut().expect("library path was checked");
        if metadata.directory_available {
            for (id, bytes) in &metadata.bytes {
                if accept_visible_baseline {
                    library
                        .notebook_metadata_baselines
                        .insert(id.clone(), Some(bytes.clone()));
                } else {
                    library
                        .notebook_metadata_baselines
                        .entry(id.clone())
                        .or_insert_with(|| Some(bytes.clone()));
                }
            }
            library.default_notebook_id =
                resolved_default_notebook_id(&metadata.notebooks, &mut metadata.warnings);
            library.notebooks = metadata.notebooks;
        }
        library.metadata_warnings = metadata.warnings;
        apply_notebook_metadata_to_view(library);
        if let Some(session) = self.session.as_mut() {
            if matches!(session.view.origin, SessionOrigin::Library)
                && !session.external_conflict
                && read_only_reason(
                    &session.package,
                    session.notebook_id.as_deref(),
                    &library.notebooks,
                )
                .is_none()
            {
                session.was_writable = true;
            }
        }
        Ok(())
    }

    pub(super) fn refresh_selected_library(
        &mut self,
    ) -> Result<Option<LibraryView>, WorkspaceError> {
        let selected_library_is_missing = self.library.as_ref().is_some_and(|library| {
            matches!(
                fs::symlink_metadata(&library.path),
                Err(error) if error.kind() == io::ErrorKind::NotFound
            )
        });
        if selected_library_is_missing {
            // Keep the last visible list while a synchronized library directory
            // is temporarily absent, but still honor its local restore pointer.
            self.ensure_library_transition_allowed()?;
        } else {
            self.refresh_library_metadata_inner(true)?;
        }
        let scan_context = self.search_scan_context();
        let Some(library) = self.library.as_mut() else {
            return Ok(None);
        };
        match scan_library_with_previous(
            &library.path,
            &library.notebooks,
            library.default_notebook_id.as_deref(),
            &self.recovery_root,
            &library.metadata_warnings,
            Some(&library.view),
            &scan_context,
        ) {
            Ok(scan) => install_library_scan(library, scan),
            Err(error) => {
                library.view.warnings = vec![format!("无法刷新笔记列表：{}", error.message)];
                library.search = SearchSnapshot::unavailable(
                    self.search_generation.load(Ordering::Acquire),
                    vec![format!("无法刷新搜索索引：{}", error.message)],
                );
            }
        }
        Ok(Some(library.view.clone()))
    }

    fn prepare_library(
        &self,
        requested: &Path,
        initialize_if_new: bool,
    ) -> Result<LibraryState, WorkspaceError> {
        let path = resolve_candidate_directory(requested, initialize_if_new)?;
        let new_path =
            fs::symlink_metadata(&path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
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

        // Selection can create a directory before acquiring its local lease;
        // no native facts are initialized until the shared lease is held.
        if initialize_if_new && new_path {
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
        let _selection_access = if initialize_if_new {
            Some(storage::library_access::lock_shared(
                &self.recovery_root,
                &canonical,
            )?)
        } else {
            None
        };
        if initialize_if_new && new_path {
            storage::probe_directory(&canonical)?;
            let notes_path = canonical.join("notes");
            let notebooks_path = canonical.join("notebooks");
            ensure_child_directory(&canonical, &notes_path, true)?;
            ensure_child_directory(&canonical, &notebooks_path, true)?;
            let notebook = new_default_notebook();
            publish_notebook_metadata(&notebooks_path, &notebook)?;
        }

        let mut metadata = read_library_metadata(&canonical);
        let default_notebook_id =
            resolved_default_notebook_id(&metadata.notebooks, &mut metadata.warnings);
        let scan_context = self.search_scan_context();
        let view = scan_library(
            &canonical,
            &metadata.notebooks,
            default_notebook_id.as_deref(),
            &self.recovery_root,
            &metadata.warnings,
            &scan_context,
        )?;
        let (view, search) = library_scan_parts(view);
        let notebook_metadata_baselines = metadata
            .bytes
            .iter()
            .map(|(id, bytes)| (id.clone(), Some(bytes.clone())))
            .collect();
        Ok(LibraryState {
            path: canonical,
            notebooks: metadata.notebooks,
            default_notebook_id,
            notebook_metadata_baselines,
            metadata_warnings: metadata.warnings,
            view,
            search,
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

struct AssetRun {
    directory: Option<PathBuf>,
    lock: Option<File>,
}

impl AssetRun {
    fn new() -> Self {
        Self {
            directory: None,
            lock: None,
        }
    }

    fn session_path(
        &mut self,
        recovery_root: &Path,
        session_id: &str,
    ) -> Result<PathBuf, WorkspaceError> {
        if !is_canonical_uuid_v4(session_id) {
            return Err(StorageError::new(
                "invalidSession",
                "笔记会话身份格式无效。",
            ));
        }
        Ok(self.ensure(recovery_root)?.join(session_id))
    }

    fn ensure(&mut self, recovery_root: &Path) -> Result<PathBuf, WorkspaceError> {
        if let Some(directory) = &self.directory {
            return Ok(directory.clone());
        }

        let recovery_root = resolve_future_path(recovery_root)?;
        fs::create_dir_all(&recovery_root).map_err(|error| {
            StorageError::new("assetStaging", format!("无法准备图片暂存目录：{error}"))
        })?;
        ensure_asset_directory(&recovery_root)?;

        let staging_root = recovery_root.join("asset-staging");
        ensure_asset_directory(&staging_root)?;
        let lock_root = staging_root.join(".locks");
        ensure_asset_directory(&lock_root)?;
        cleanup_stale_asset_runs(&staging_root, &lock_root)?;

        for _ in 0..8 {
            let run_id = Uuid::new_v4().to_string();
            let lock_path = lock_root.join(format!("{run_id}.lock"));
            let lock = open_asset_run_lock(&lock_path)?;
            match lock.try_lock() {
                Ok(()) => {
                    let directory = staging_root.join(&run_id);
                    match fs::create_dir(&directory) {
                        Ok(()) => {
                            self.directory = Some(directory.clone());
                            self.lock = Some(lock);
                            return Ok(directory);
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            let _ = lock.unlock();
                        }
                        Err(error) => {
                            let _ = lock.unlock();
                            return Err(StorageError::new(
                                "assetStaging",
                                format!("无法创建本次图片暂存目录：{error}"),
                            ));
                        }
                    }
                }
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(StorageError::new(
                        "assetStaging",
                        format!("无法锁定本次图片暂存目录：{error}"),
                    ));
                }
            }
        }
        Err(StorageError::new(
            "assetStaging",
            "多次尝试后仍无法创建独立的图片暂存目录。",
        ))
    }

    fn release(&mut self) -> Result<(), WorkspaceError> {
        if let Some(directory) = self.directory.as_ref() {
            match fs::symlink_metadata(directory) {
                Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => {
                    return Err(StorageError::new(
                        "unsafePath",
                        "本次图片暂存运行目录必须是实际目录。",
                    ));
                }
                Ok(_) => fs::remove_dir_all(directory).map_err(|error| {
                    StorageError::new("assetStaging", format!("无法释放本次图片暂存目录：{error}"))
                })?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(StorageError::new(
                        "assetStaging",
                        format!("无法检查本次图片暂存目录：{error}"),
                    ));
                }
            }
            self.directory.take();
        }
        if let Some(lock) = self.lock.take() {
            lock.unlock().map_err(|error| {
                StorageError::new(
                    "assetStaging",
                    format!("无法释放本次图片暂存运行锁：{error}"),
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for AssetRun {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

struct AssetStash {
    directory: PathBuf,
}

impl AssetStash {
    fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn put(&self, entry: &AssetManifestEntry, bytes: &[u8]) -> Result<(), WorkspaceError> {
        validate_staged_asset(entry, bytes)?;
        ensure_asset_directory(&self.directory)?;
        match self.existing_matches(entry, bytes)? {
            Some(true) => return Ok(()),
            Some(false) => {
                return Err(StorageError::new(
                    "assetIntegrity",
                    "图片暂存身份已对应其他数据，拒绝覆盖。",
                ));
            }
            None => {}
        }

        let asset_path = self.directory.join(&entry.id);
        let metadata_path = self.directory.join(format!("{}.json", entry.id));
        let result = (|| {
            write_immutable_file(&asset_path, bytes, "图片资产")?;
            let metadata = serde_json::to_vec(entry).map_err(|error| {
                StorageError::new("assetStaging", format!("无法编码图片资产信息：{error}"))
            })?;
            write_immutable_file(&metadata_path, &metadata, "图片资产信息")?;
            sync_directory(&self.directory).map_err(|error| {
                StorageError::new("assetStaging", format!("无法同步图片暂存目录：{error}"))
            })?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&asset_path);
            let _ = fs::remove_file(&metadata_path);
        }
        result
    }

    fn existing_matches(
        &self,
        entry: &AssetManifestEntry,
        bytes: &[u8],
    ) -> Result<Option<bool>, WorkspaceError> {
        let asset_path = self.directory.join(&entry.id);
        let metadata_path = self.directory.join(format!("{}.json", entry.id));
        let metadata = match fs::symlink_metadata(&metadata_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::symlink_metadata(&asset_path) {
                    Ok(_) => {
                        return Err(StorageError::new(
                            "assetIntegrity",
                            "图片暂存文件缺少对应的资产信息。",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => {
                        return Err(StorageError::new(
                            "assetStaging",
                            format!("无法检查图片暂存文件：{error}"),
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(StorageError::new(
                    "assetStaging",
                    format!("无法检查图片资产信息：{error}"),
                ));
            }
        };
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "图片资产信息必须是普通文件。",
            ));
        }
        if metadata.len() > MAX_METADATA_BYTES {
            return Err(StorageError::new(
                "assetIntegrity",
                "图片资产信息超过大小上限。",
            ));
        }
        let metadata_bytes = read_regular_file(&metadata_path, MAX_METADATA_BYTES)?;
        let existing_entry: AssetManifestEntry =
            serde_json::from_slice(&metadata_bytes).map_err(|error| {
                StorageError::new("assetIntegrity", format!("图片资产信息损坏：{error}"))
            })?;
        if existing_entry != *entry {
            return Ok(Some(false));
        }

        let asset_metadata = fs::symlink_metadata(&asset_path).map_err(|error| {
            StorageError::new("assetStaging", format!("无法检查图片暂存文件：{error}"))
        })?;
        if is_link_or_reparse(&asset_metadata) || !asset_metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "图片暂存文件必须是普通文件。",
            ));
        }
        if asset_metadata.len() != bytes.len() as u64 {
            return Ok(Some(false));
        }
        let mut file = File::open(&asset_path).map_err(|error| {
            StorageError::new("assetStaging", format!("无法打开图片暂存文件：{error}"))
        })?;
        let mut buffer = [0u8; 64 * 1024];
        for expected in bytes.chunks(buffer.len()) {
            file.read_exact(&mut buffer[..expected.len()])
                .map_err(|error| {
                    StorageError::new("assetStaging", format!("无法读取图片暂存文件：{error}"))
                })?;
            if buffer[..expected.len()] != *expected {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    fn read(
        &self,
        asset_id: &str,
    ) -> Result<Option<(AssetManifestEntry, Vec<u8>)>, WorkspaceError> {
        if !is_canonical_uuid_v4(asset_id) {
            return Err(StorageError::new(
                "invalidAssetId",
                "图片资产身份格式无效。",
            ));
        }
        let asset_path = self.directory.join(asset_id);
        let metadata_path = self.directory.join(format!("{asset_id}.json"));
        let metadata = match fs::symlink_metadata(&metadata_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::symlink_metadata(&asset_path) {
                    Ok(_) => {
                        return Err(StorageError::new(
                            "assetIntegrity",
                            "图片暂存文件缺少对应的资产信息。",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => {
                        return Err(StorageError::new(
                            "assetStaging",
                            format!("无法检查图片暂存文件：{error}"),
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(StorageError::new(
                    "assetStaging",
                    format!("无法检查图片资产信息：{error}"),
                ));
            }
        };
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(StorageError::new(
                "unsafePath",
                "图片资产信息必须是普通文件。",
            ));
        }
        if metadata.len() > MAX_METADATA_BYTES {
            return Err(StorageError::new(
                "assetIntegrity",
                "图片资产信息超过大小上限。",
            ));
        }
        let metadata_bytes = read_regular_file(&metadata_path, MAX_METADATA_BYTES)?;
        let entry: AssetManifestEntry =
            serde_json::from_slice(&metadata_bytes).map_err(|error| {
                StorageError::new("assetIntegrity", format!("图片资产信息损坏：{error}"))
            })?;
        if entry.id != asset_id {
            return Err(StorageError::new(
                "assetIntegrity",
                "图片资产信息身份与文件名不一致。",
            ));
        }
        let bytes = read_regular_file(&asset_path, MAX_EXPANDED_BYTES)?;
        validate_staged_asset(&entry, &bytes)?;
        Ok(Some((entry, bytes)))
    }
}

impl Drop for ActiveNoteSession {
    fn drop(&mut self) {
        let _ = self.release_asset_stash();
    }
}

impl ActiveNoteSession {
    fn retain_baseline(&mut self, recovery_root: &Path) -> Result<(), WorkspaceError> {
        if let Some(revision) = &self.baseline_revision {
            self.baseline_witness = Some(storage::versions::lease_observed_version(
                recovery_root,
                &OpenedNote {
                    package: self.package.clone(),
                    revision: revision.clone(),
                },
                &display_path(&self.path),
                "当前已保存版本",
            )?);
        }
        Ok(())
    }

    fn release_asset_stash(&mut self) -> Result<(), WorkspaceError> {
        let Some(stash) = self.asset_stash.as_ref() else {
            return Ok(());
        };
        match fs::symlink_metadata(&stash.directory) {
            Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => {
                return Err(StorageError::new(
                    "unsafePath",
                    "本笔记会话的图片暂存目录必须是实际目录。",
                ));
            }
            Ok(_) => fs::remove_dir_all(&stash.directory).map_err(|error| {
                StorageError::new(
                    "assetStaging",
                    format!("无法释放本笔记会话的图片暂存目录：{error}"),
                )
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(
                    "assetStaging",
                    format!("无法检查本笔记会话的图片暂存目录：{error}"),
                ));
            }
        }
        self.asset_stash.take();
        Ok(())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        drop(self.session.take());
    }
}

fn cleanup_stale_asset_runs(staging_root: &Path, lock_root: &Path) -> Result<(), WorkspaceError> {
    let entries = fs::read_dir(staging_root).map_err(|error| {
        StorageError::new("assetStaging", format!("无法读取图片暂存目录：{error}"))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            StorageError::new("assetStaging", format!("无法读取图片暂存项目：{error}"))
        })?;
        let name = entry.file_name();
        let Some(run_id) = name.to_str() else {
            continue;
        };
        if !is_canonical_uuid_v4(run_id) {
            continue;
        }
        let directory = entry.path();
        let metadata = fs::symlink_metadata(&directory).map_err(|error| {
            StorageError::new("assetStaging", format!("无法检查旧暂存目录：{error}"))
        })?;
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(StorageError::new(
                "unsafePath",
                "旧图片暂存路径必须是实际目录。",
            ));
        }

        let lock_path = lock_root.join(format!("{run_id}.lock"));
        let lock = open_asset_run_lock(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {
                fs::remove_dir_all(&directory).map_err(|error| {
                    StorageError::new("assetStaging", format!("无法清理旧图片暂存：{error}"))
                })?;
                lock.unlock().map_err(|error| {
                    StorageError::new("assetStaging", format!("无法释放暂存清理锁：{error}"))
                })?;
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(StorageError::new(
                    "assetStaging",
                    format!("无法检查旧图片暂存锁：{error}"),
                ));
            }
        }
    }
    Ok(())
}

fn open_asset_run_lock(path: &Path) -> Result<File, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            return Err(StorageError::new(
                "unsafePath",
                "图片暂存运行锁必须是普通文件。",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(StorageError::new(
                "assetStaging",
                format!("无法检查图片暂存运行锁：{error}"),
            ));
        }
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| StorageError::new("assetStaging", format!("无法打开暂存运行锁：{error}")))
}

fn ensure_asset_directory(path: &Path) -> Result<(), WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => Err(
            StorageError::new("unsafePath", "图片暂存路径必须是实际目录。"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                StorageError::new("assetStaging", format!("无法创建图片暂存目录：{error}"))
            })
        }
        Err(error) => Err(StorageError::new(
            "assetStaging",
            format!("无法检查图片暂存目录：{error}"),
        )),
    }
}

fn write_immutable_file(path: &Path, bytes: &[u8], label: &str) -> Result<(), WorkspaceError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| StorageError::new("assetStaging", format!("无法创建{label}：{error}")))?;
    let result = (|| {
        file.write_all(bytes).map_err(|error| {
            StorageError::new("assetStaging", format!("无法写入{label}：{error}"))
        })?;
        file.flush().map_err(|error| {
            StorageError::new("assetStaging", format!("无法刷新{label}：{error}"))
        })?;
        file.sync_all()
            .map_err(|error| StorageError::new("assetStaging", format!("无法同步{label}：{error}")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn read_regular_file(path: &Path, limit: u64) -> Result<Vec<u8>, WorkspaceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("assetStaging", format!("无法检查暂存文件：{error}")))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(StorageError::new(
            "unsafePath",
            "图片暂存文件必须是普通文件。",
        ));
    }
    if metadata.len() > limit {
        return Err(StorageError::new(
            "resourceLimit",
            "图片暂存文件超过大小上限。",
        ));
    }
    let file = File::open(path)
        .map_err(|error| StorageError::new("assetStaging", format!("无法打开暂存文件：{error}")))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::new("assetStaging", format!("无法读取暂存文件：{error}")))?;
    if bytes.len() as u64 > limit {
        return Err(StorageError::new(
            "resourceLimit",
            "图片暂存文件超过大小上限。",
        ));
    }
    Ok(bytes)
}

fn validate_staged_asset(entry: &AssetManifestEntry, bytes: &[u8]) -> Result<(), WorkspaceError> {
    if !is_canonical_uuid_v4(&entry.id)
        || entry.byte_length != bytes.len() as u64
        || entry.sha256 != format!("{:x}", sha2::Sha256::digest(bytes))
    {
        return Err(StorageError::new(
            "assetIntegrity",
            "图片资产与暂存信息不匹配。",
        ));
    }
    Ok(())
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

fn blocked_restore_status(path: &Path, message: &str) -> LibraryRestoreStatus {
    LibraryRestoreStatus {
        restore_id: String::new(),
        library_path: display_path(path),
        source_backup_id: String::new(),
        pre_restore_backup: None,
        phase: "blocked".into(),
        completed_files: 0,
        total_files: 0,
        can_continue: false,
        can_roll_back: false,
        message: message.into(),
    }
}

fn preserve_draft_after_metadata_loss(
    error: StorageError,
    session: &ActiveNoteSession,
    library: &LibraryState,
    recovery_root: &Path,
    candidate: &NotePackage,
) -> StorageError {
    if !session.was_writable
        || session.preserved_preview
        || session.external_conflict
        || session.package.document.organization.trashed_at.is_some()
        || !supports_editable_content(&session.package.content())
    {
        return error;
    }
    let Some(notebook_id) = session.notebook_id.as_deref() else {
        return error;
    };
    if library.notebooks.contains_key(notebook_id)
        || library
            .notebook_metadata_baselines
            .get(notebook_id)
            .and_then(Option::as_ref)
            .is_none()
    {
        return error;
    }
    storage::preserve_snapshot_for_unresolved_target(error, &session.path, recovery_root, candidate)
}

fn preserve_draft_after_save_access_failure(
    error: StorageError,
    session: &ActiveNoteSession,
    recovery_root: &Path,
    candidate: &NotePackage,
) -> StorageError {
    if !session.was_writable
        || session.preserved_preview
        || session.external_conflict
        || session.package.document.organization.trashed_at.is_some()
        || !supports_editable_content(&session.package.content())
        || (matches!(session.view.origin, SessionOrigin::Library) && session.notebook_id.is_none())
    {
        return error;
    }
    storage::preserve_snapshot_for_unresolved_target(error, &session.path, recovery_root, candidate)
}

fn supports_editable_content(content: &NoteContent) -> bool {
    supports_editable_blocks(&content.sections.cue, true, false)
        && supports_editable_blocks(&content.sections.content, false, true)
        && supports_editable_blocks(&content.sections.summary, true, false)
}

fn candidate_for_content(
    session: &ActiveNoteSession,
    content: &NoteContent,
) -> Result<NotePackage, WorkspaceError> {
    if !supports_editable_content(content) {
        return Err(StorageError::new(
            "readOnly",
            "笔记包含当前编辑器尚不支持的内容，未接受任何更改。",
        ));
    }

    let mut candidate = NotePackage {
        document: session.package.document.clone(),
        manifest: session.package.manifest.clone(),
        assets: BTreeMap::new(),
    };
    candidate.assets.clear();
    candidate.manifest.assets.clear();
    candidate.replace_content(content.clone());

    let mut referenced = HashSet::new();
    let mut manifest = Vec::new();
    let mut assets = BTreeMap::new();
    for block in &content.sections.content {
        let Block::Image { asset_id, .. } = block else {
            continue;
        };
        if !referenced.insert(asset_id.clone()) {
            continue;
        }

        let current_entry = session
            .package
            .manifest
            .assets
            .iter()
            .find(|entry| entry.id == *asset_id);
        let (entry, bytes) = if let Some(entry) = current_entry {
            let bytes =
                session.package.assets.get(asset_id).ok_or_else(|| {
                    StorageError::new("assetIntegrity", "笔记包中的图片文件缺失。")
                })?;
            (entry.clone(), bytes.clone())
        } else if let Some(stash) = session.asset_stash.as_ref() {
            stash.read(asset_id)?.ok_or_else(|| {
                StorageError::new("missingAsset", "插入的图片未暂存在当前笔记会话中。")
            })?
        } else {
            return Err(StorageError::new(
                "missingAsset",
                "插入的图片未暂存在当前笔记会话中。",
            ));
        };
        if entry.id != *asset_id {
            return Err(StorageError::new(
                "assetIntegrity",
                "图片引用与资产身份不一致。",
            ));
        }
        manifest.push(entry);
        assets.insert(asset_id.clone(), bytes);
    }

    candidate.manifest.assets = manifest;
    candidate.assets = assets;
    candidate.check_encoded_size().map_err(|error| {
        let kind = match error.kind.as_str() {
            "budget_exceeded" | "resource_limit" => error.kind,
            _ => "format".to_owned(),
        };
        StorageError::new(kind, error.message)
    })?;
    Ok(candidate)
}

fn supports_editable_blocks(blocks: &[Block], side_section: bool, top_level_content: bool) -> bool {
    blocks.iter().all(|block| match block {
        Block::Paragraph { children }
        | Block::Heading1 { children }
        | Block::Heading2 { children }
        | Block::Heading3 { children } => supports_editable_inlines(children, side_section),
        Block::OrderedList { items, .. } | Block::BulletList { items } => items
            .iter()
            .all(|item| supports_editable_blocks(&item.blocks, side_section, false)),
        Block::TaskList { items } => items
            .iter()
            .all(|item| supports_editable_blocks(&item.blocks, side_section, false)),
        Block::Blockquote { blocks } if !side_section => {
            supports_editable_blocks(blocks, false, false)
        }
        Block::CodeBlock { .. } if !side_section => true,
        Block::HorizontalRule if !side_section => true,
        Block::MathBlock { .. } if !side_section => true,
        Block::Image { .. } if !side_section && top_level_content => true,
        Block::MindMap { .. } if !side_section && top_level_content => true,
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
            Mark::Code => !side_section,
        }),
        Inline::HardBreak => true,
        Inline::MathInline { .. } => !side_section,
    })
}

fn note_session_from_opened(
    note_id: &str,
    opened: &OpenedNote,
    read_only_reason: Option<String>,
) -> NoteSession {
    NoteSession {
        session_id: Uuid::new_v4().to_string(),
        origin: SessionOrigin::Library,
        note_id: note_id.to_owned(),
        notebook_id: opened.package.document.organization.notebook_id.clone(),
        pinned: opened.package.document.organization.pinned,
        trashed_at: opened.package.document.organization.trashed_at.clone(),
        text: opened.package.text_projection(),
        content: opened.package.content(),
        assets: opened.package.manifest.assets.clone(),
        revision: Some(opened.revision.clone()),
        read_only_reason,
    }
}

fn emit_search_progress(
    context: &SearchScanContext,
    library_path: &Path,
    phase: SearchStatus,
    checked: usize,
    total: usize,
    incomplete: bool,
) {
    if let Some(progress) = &context.progress {
        progress(SearchProgress {
            library_path: display_path(library_path),
            phase,
            checked,
            total,
            incomplete,
        });
    }
}

#[cfg_attr(feature = "perf-acceptance", track_caller)]
fn scan_library(
    library_path: &Path,
    notebooks: &BTreeMap<String, NotebookMetadata>,
    default_notebook_id: Option<&str>,
    recovery_root: &Path,
    metadata_warnings: &[String],
    context: &SearchScanContext,
) -> Result<LibraryScan, WorkspaceError> {
    scan_library_with_previous(
        library_path,
        notebooks,
        default_notebook_id,
        recovery_root,
        metadata_warnings,
        None,
        context,
    )
}

#[cfg_attr(feature = "perf-acceptance", track_caller)]
fn scan_library_with_previous(
    library_path: &Path,
    notebooks: &BTreeMap<String, NotebookMetadata>,
    default_notebook_id: Option<&str>,
    recovery_root: &Path,
    metadata_warnings: &[String],
    previous: Option<&LibraryView>,
    context: &SearchScanContext,
) -> Result<LibraryScan, WorkspaceError> {
    let result = scan_library_with_previous_inner(
        library_path,
        notebooks,
        default_notebook_id,
        recovery_root,
        metadata_warnings,
        previous,
        context,
    );
    if result.is_err() {
        emit_search_progress(context, library_path, SearchStatus::Unavailable, 0, 0, true);
    }
    result
}

#[cfg_attr(feature = "perf-acceptance", track_caller)]
fn scan_library_with_previous_inner(
    library_path: &Path,
    notebooks: &BTreeMap<String, NotebookMetadata>,
    default_notebook_id: Option<&str>,
    recovery_root: &Path,
    metadata_warnings: &[String],
    previous: Option<&LibraryView>,
    context: &SearchScanContext,
) -> Result<LibraryScan, WorkspaceError> {
    let generation = context.generation.load(Ordering::Acquire);
    let notes_path = library_path.join("notes");
    let mut warnings = metadata_warnings.to_vec();
    let mut notes_directory_available = true;
    let mut incomplete = !metadata_warnings.is_empty();
    let mut entries = match secure_read_dir_if_present(library_path, &notes_path) {
        Ok(Some(entries)) => entries,
        Ok(None) => {
            notes_directory_available = false;
            incomplete = true;
            if let Some(previous) = previous {
                emit_search_progress(context, library_path, SearchStatus::Unavailable, 0, 0, true);
                return Ok(LibraryScan {
                    view: preserve_unavailable_notes_view(
                        previous,
                        notebooks,
                        default_notebook_id,
                        &notes_path,
                        "目录尚未到达",
                    ),
                    search_index: search::SearchIndex::default(),
                    search_available: false,
                    incomplete: true,
                    generation,
                });
            }
            push_warning(
                &mut warnings,
                format!("笔记目录尚未到达：{}", display_path(&notes_path)),
            );
            Vec::new()
        }
        Err(error) => {
            notes_directory_available = false;
            incomplete = true;
            if let Some(previous) = previous {
                emit_search_progress(context, library_path, SearchStatus::Unavailable, 0, 0, true);
                return Ok(LibraryScan {
                    view: preserve_unavailable_notes_view(
                        previous,
                        notebooks,
                        default_notebook_id,
                        &notes_path,
                        &error.message,
                    ),
                    search_index: search::SearchIndex::default(),
                    search_available: false,
                    incomplete: true,
                    generation,
                });
            }
            push_warning(
                &mut warnings,
                format!(
                    "无法读取笔记目录 {}：{}",
                    display_path(&notes_path),
                    error.message
                ),
            );
            Vec::new()
        }
    };
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_NOTE_FILES {
        emit_search_progress(context, library_path, SearchStatus::Unavailable, 0, 0, true);
        return Err(StorageError::new(
            "libraryTooLarge",
            "笔记文件数量超出当前索引上限。",
        ));
    }
    let mut notes = Vec::new();
    let mut trash_notes = Vec::new();
    let mut search_index = search::SearchIndex::default();
    let cache = scan_cache::Cache::load(&context.config_root, library_path, context.bypass_cache);
    let mut next_cache = BTreeMap::new();
    let mut seen_file_ids = HashSet::new();
    let mut seen_package_ids = HashMap::<String, (String, String, bool)>::new();
    let mut omitted_warnings = 0usize;
    let mut index_limit_warned = false;
    let mut search_index_limit_reached = false;
    let total = entries
        .iter()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cuenote"))
        })
        .count();
    let mut checked = 0usize;
    emit_search_progress(
        context,
        library_path,
        SearchStatus::Building,
        checked,
        total,
        incomplete,
    );
    let progress_interval = total
        .saturating_add(99)
        .checked_div(100)
        .unwrap_or(1)
        .max(1);
    // A warm scan may process a percent every few milliseconds. Keep progress
    // responsive without forcing the desktop to reconcile 1,000 rows 100 times.
    let mut last_progress = std::time::Instant::now();
    macro_rules! report_checked_file {
        () => {{
            checked += 1;
            if checked == total
                || (checked % progress_interval == 0
                    && last_progress.elapsed() >= std::time::Duration::from_millis(100))
            {
                emit_search_progress(
                    context,
                    library_path,
                    SearchStatus::Building,
                    checked,
                    total,
                    incomplete,
                );
                last_progress = std::time::Instant::now();
            }
        }};
    }
    let mut cache_checks = cache_check::Checks::new(&entries, cache.as_ref(), recovery_root);
    for (entry_index, entry) in entries.iter().enumerate() {
        let verified = cache_checks
            .as_mut()
            .is_some_and(|checks| checks.verified(entry_index));
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("cuenote") {
            continue;
        }
        if let Err(error) = storage::ensure_local_file(&path) {
            incomplete = true;
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!(
                    "无法读取笔记文件 {}：{}",
                    display_path(&path),
                    error.message
                ),
            );
            report_checked_file!();
            continue;
        }
        let file_metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                incomplete = true;
                push_warning_counted(
                    &mut warnings,
                    &mut omitted_warnings,
                    format!("无法检查笔记文件：{error}"),
                );
                report_checked_file!();
                continue;
            }
        };
        if !file_metadata.is_file() {
            incomplete = true;
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("已跳过非普通笔记文件：{}", display_path(&path)),
            );
            report_checked_file!();
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            incomplete = true;
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                "已跳过文件名无法读取的笔记。".into(),
            );
            report_checked_file!();
            continue;
        };
        let origin = display_path(&path);
        let cache_key = entry.file_name().to_string_lossy().into_owned();
        let candidate = cache
            .as_ref()
            .and_then(|cache| cache.entries.get(&cache_key))
            .filter(|entry| {
                is_canonical_uuid_v4(stem)
                    && entry.summary.note_id == stem
                    && !seen_package_ids.contains_key(stem)
            });
        let hit = candidate.filter(|_| verified);
        let (summary, opened, mut cache_entry) = if let Some(entry) = hit {
            seen_package_ids.insert(
                stem.to_owned(),
                (entry.revision.clone(), origin.clone(), true),
            );
            seen_file_ids.insert(stem.to_owned());
            (entry.summary.clone(), None, Some(entry.clone()))
        } else {
            let opened = match storage::open_note(&path) {
                Ok(opened) => opened,
                Err(error) => {
                    incomplete = true;
                    push_warning_counted(
                        &mut warnings,
                        &mut omitted_warnings,
                        format!("无法读取笔记 {stem}：{}", error.message),
                    );
                    report_checked_file!();
                    continue;
                }
            };
            let package_id = opened.package.document.note_id.clone();
            // Index only versions which have a durable independent witness. Failure
            // leaves the original untouched and warns instead of claiming protection.
            if let Err(error) = storage::versions::preserve_observed_version(
                recovery_root,
                &opened,
                &origin,
                "笔记库中取得的有效版本",
            ) {
                incomplete = true;
                push_warning_counted(
                    &mut warnings,
                    &mut omitted_warnings,
                    format!("无法保留笔记 {}，已跳过索引：{}", origin, error.message),
                );
                report_checked_file!();
                continue;
            }
            let previous = seen_package_ids.get(&package_id).cloned();
            if let Some((revision, previous_origin, _)) = &previous {
                incomplete = true;
                if revision != &opened.revision {
                    storage::versions::pin_observed_version(
                        recovery_root,
                        revision,
                        previous_origin,
                        &package_id,
                        "重复身份的另一版本",
                    )?;
                    storage::versions::preserve_conflict_version(
                        recovery_root,
                        &opened,
                        &origin,
                        "重复身份的不同版本",
                    )?;
                }
                push_warning_counted(
                    &mut warnings,
                    &mut omitted_warnings,
                    format!(
                        "发现重复笔记身份，已检查相关文件：{}",
                        opened.package.document.note_id
                    ),
                );
            }
            if !is_canonical_uuid_v4(stem) || package_id != stem {
                incomplete = true;
                storage::versions::preserve_conflict_version(
                    recovery_root,
                    &opened,
                    &origin,
                    "非标准文件名或同步冲突副本",
                )?;
                seen_package_ids.entry(package_id).or_insert((
                    opened.revision.clone(),
                    origin,
                    false,
                ));
                push_warning_counted(
                    &mut warnings,
                    &mut omitted_warnings,
                    format!("笔记文件名与内容身份不一致，已保留到历史与冲突：{stem}"),
                );
                report_checked_file!();
                continue;
            }
            if previous
                .as_ref()
                .is_some_and(|(_, _, canonical)| *canonical)
                || !seen_file_ids.insert(stem.to_owned())
            {
                incomplete = true;
                report_checked_file!();
                continue;
            }
            seen_package_ids.insert(package_id, (opened.revision.clone(), origin.clone(), true));
            let summary = note_summary_from_package(stem, &opened.package);
            let cache_entry =
                storage::versions::observed_version_witness(recovery_root, &opened, &origin)
                    .ok()
                    .map(|witness| scan_cache::Entry {
                        summary: summary.clone(),
                        revision: opened.revision.clone(),
                        projection: Vec::new(),
                        witness,
                    });
            (summary, Some(opened), cache_entry)
        };
        let notebook_id = summary.notebook_id.as_deref();
        let trashed_at = summary.trashed_at.as_ref();
        if trashed_at.is_none() && notebook_id.is_none() {
            incomplete = true;
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记 {stem} 缺少笔记本归属，保存前需先处理。"),
            );
        } else if trashed_at.is_none() && !notebooks.contains_key(notebook_id.unwrap()) {
            incomplete = true;
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记 {stem} 属于未知笔记本，当前不会重新归属。"),
            );
        }
        if trashed_at.is_some() {
            trash_notes.push(summary);
        } else {
            let indexed = !search_index_limit_reached
                && match &opened {
                    Some(opened) => search_index.add_note(summary.clone(), &opened.package),
                    None => search_index.add_projection(
                        summary.clone(),
                        &cache_entry.as_ref().expect("cache hit").projection,
                    ),
                };
            if !indexed {
                incomplete = true;
                search_index_limit_reached = true;
                cache_entry = None;
                if !index_limit_warned {
                    push_warning_counted(
                        &mut warnings,
                        &mut omitted_warnings,
                        format!(
                            "搜索索引达到 {} MiB 资源上限，部分笔记未进入索引。",
                            search::MAX_INDEX_BYTES / 1024 / 1024
                        ),
                    );
                    index_limit_warned = true;
                }
            } else if opened.is_some() {
                if let Some(entry) = cache_entry.as_mut() {
                    entry.projection = search_index.last_projection();
                }
            }
            notes.push(summary);
        }
        if let Some(entry) = cache_entry {
            next_cache.insert(cache_key, entry);
        }
        report_checked_file!();
    }
    drop(cache_checks);
    if notes_directory_available && context.generation.load(Ordering::Acquire) == generation {
        if let Some(cache) = cache {
            cache.publish(next_cache);
        }
    }
    if omitted_warnings > 0 {
        warnings.push(format!("另有 {omitted_warnings} 条笔记警告未显示。"));
    }
    sort_note_summaries(&mut notes);
    sort_trash_notes(&mut trash_notes);
    let default_notebook_name = default_notebook_id
        .and_then(|id| notebooks.get(id))
        .map(|metadata| metadata.name.clone())
        .unwrap_or_else(|| "默认笔记本".into());
    let view = LibraryView {
        path: display_path(library_path),
        default_notebook_name,
        default_notebook_id: default_notebook_id.map(str::to_owned),
        notebooks: notebook_summaries(notebooks),
        notes,
        trash_notes,
        warnings: warnings.clone(),
    };
    let phase =
        if notes_directory_available && context.generation.load(Ordering::Acquire) == generation {
            SearchStatus::Ready
        } else if notes_directory_available {
            SearchStatus::Building
        } else {
            SearchStatus::Unavailable
        };
    emit_search_progress(context, library_path, phase, checked, total, incomplete);
    Ok(LibraryScan {
        view,
        incomplete: incomplete || search_index.is_incomplete(),
        search_index,
        search_available: notes_directory_available,
        generation,
    })
}

fn library_scan_parts(scan: LibraryScan) -> (LibraryView, SearchSnapshot) {
    let warnings = scan.view.warnings.clone();
    let search = if scan.search_available {
        SearchSnapshot {
            index: scan.search_index,
            available: true,
            incomplete: scan.incomplete,
            warnings,
            generation: scan.generation,
        }
    } else {
        SearchSnapshot::unavailable(scan.generation, warnings)
    };
    (scan.view, search)
}

fn install_library_scan(library: &mut LibraryState, scan: LibraryScan) {
    let (view, search) = library_scan_parts(scan);
    library.view = view;
    library.search = search;
}

fn preserve_unavailable_notes_view(
    previous: &LibraryView,
    notebooks: &BTreeMap<String, NotebookMetadata>,
    default_notebook_id: Option<&str>,
    notes_path: &Path,
    reason: &str,
) -> LibraryView {
    let mut view = previous.clone();
    view.default_notebook_id = default_notebook_id.map(str::to_owned);
    view.default_notebook_name = default_notebook_id
        .and_then(|id| notebooks.get(id))
        .map(|metadata| metadata.name.clone())
        .unwrap_or_else(|| "默认笔记本".into());
    view.notebooks = notebook_summaries(notebooks);
    view.warnings.retain(|warning| {
        !warning.starts_with("笔记目录尚未到达：") && !warning.starts_with("无法读取笔记目录 ")
    });
    push_warning(
        &mut view.warnings,
        format!(
            "笔记目录暂不可用，已保留上次可见列表 {}：{reason}",
            display_path(notes_path)
        ),
    );
    view
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

fn read_library_metadata(library_path: &Path) -> NotebookMetadataScan {
    let notebooks_path = library_path.join("notebooks");
    let entries = match secure_read_dir_if_present(library_path, &notebooks_path) {
        Ok(Some(entries)) => entries,
        Ok(None) => {
            return NotebookMetadataScan {
                notebooks: BTreeMap::new(),
                bytes: BTreeMap::new(),
                warnings: vec![format!(
                    "笔记本元数据目录尚未到达：{}",
                    display_path(&notebooks_path)
                )],
                directory_available: false,
            };
        }
        Err(error) => {
            return NotebookMetadataScan {
                notebooks: BTreeMap::new(),
                bytes: BTreeMap::new(),
                warnings: vec![format!(
                    "无法读取笔记本元数据目录 {}：{}",
                    display_path(&notebooks_path),
                    error.message
                )],
                directory_available: false,
            };
        }
    };
    let mut scan = read_notebooks(entries);
    scan.directory_available = true;
    scan
}

fn read_notebooks(entries: Vec<fs::DirEntry>) -> NotebookMetadataScan {
    let mut scan = NotebookMetadataScan {
        notebooks: BTreeMap::new(),
        bytes: BTreeMap::new(),
        warnings: Vec::new(),
        directory_available: true,
    };
    let mut sorted = entries;
    sorted.sort_by_key(|entry| entry.file_name());
    let mut metadata_file_count = 0usize;
    let mut omitted_warnings = 0usize;
    for entry in sorted {
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("json") {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(NOTEBOOK_CONFLICT_PREFIX))
        {
            push_warning_counted(
                &mut scan.warnings,
                &mut omitted_warnings,
                format!(
                    "发现未解决的笔记本元数据冲突副本（不会作为笔记本加载）：{}",
                    display_path(&path)
                ),
            );
            continue;
        }
        metadata_file_count += 1;
        if metadata_file_count > MAX_NOTEBOOKS {
            push_warning_counted(
                &mut scan.warnings,
                &mut omitted_warnings,
                format!("笔记本元数据文件数量超出当前上限：{}", display_path(&path)),
            );
            continue;
        }
        let bytes = match read_sync_bounded(&path, MAX_METADATA_BYTES, "笔记本元数据") {
            Ok(bytes) => bytes,
            Err(error) => {
                push_warning_counted(
                    &mut scan.warnings,
                    &mut omitted_warnings,
                    format!(
                        "无法读取笔记本元数据 {}：{}",
                        display_path(&path),
                        error.message
                    ),
                );
                continue;
            }
        };
        let filename_id = match path.file_stem().and_then(|stem| stem.to_str()) {
            Some(id) => id,
            None => {
                push_warning_counted(
                    &mut scan.warnings,
                    &mut omitted_warnings,
                    format!("笔记本元数据文件名无法读取：{}", display_path(&path)),
                );
                continue;
            }
        };
        let notebook: NotebookMetadata = match serde_json::from_slice(&bytes) {
            Ok(notebook) => notebook,
            Err(error) => {
                push_warning_counted(
                    &mut scan.warnings,
                    &mut omitted_warnings,
                    format!("笔记本元数据无效 {}：{error}", display_path(&path)),
                );
                continue;
            }
        };
        let validation_error = if notebook.schema_version != LIBRARY_SCHEMA_VERSION {
            Some("元数据版本不受支持".to_owned())
        } else if !is_canonical_uuid_v4(&notebook.id) || notebook.id != filename_id {
            Some(format!("元数据身份与文件名不一致（ID {}）", notebook.id))
        } else if notebook.name.trim().is_empty()
            || notebook.name.chars().count() > 256
            || notebook.name.chars().any(char::is_control)
        {
            Some(format!("笔记本名称无效（ID {}）", notebook.id))
        } else if !is_valid_timestamp(&notebook.timestamps.created_at)
            || !is_valid_timestamp(&notebook.timestamps.modified_at)
        {
            Some(format!("笔记本时间格式无效（ID {}）", notebook.id))
        } else if scan.notebooks.contains_key(&notebook.id) {
            Some(format!("发现重复笔记本身份（ID {}）", notebook.id))
        } else if scan.notebooks.len() >= MAX_NOTEBOOKS {
            Some("笔记本元数据数量超出当前上限".to_owned())
        } else {
            None
        };
        if let Some(error) = validation_error {
            push_warning_counted(
                &mut scan.warnings,
                &mut omitted_warnings,
                format!("{error}：{}", display_path(&path)),
            );
            continue;
        }
        scan.bytes.insert(notebook.id.clone(), bytes);
        scan.notebooks.insert(notebook.id.clone(), notebook);
    }
    for warning in duplicate_notebook_name_warnings(&scan.notebooks) {
        push_warning_counted(&mut scan.warnings, &mut omitted_warnings, warning);
    }
    if omitted_warnings > 0 {
        scan.warnings.push(format!(
            "另有 {omitted_warnings} 条笔记本元数据警告未显示。"
        ));
    }
    scan
}

fn duplicate_notebook_name_warnings(notebooks: &BTreeMap<String, NotebookMetadata>) -> Vec<String> {
    let mut names = BTreeMap::<String, Vec<&NotebookMetadata>>::new();
    for notebook in notebooks.values() {
        names
            .entry(notebook.name.trim().to_lowercase())
            .or_default()
            .push(notebook);
    }
    names
        .values()
        .filter(|matches| matches.len() > 1)
        .map(|same_name| {
            let entries = same_name
                .iter()
                .map(|notebook| format!("{}（ID {}）", notebook.name, notebook.id))
                .collect::<Vec<_>>()
                .join("、");
            format!("发现重名笔记本：{entries}。元数据和笔记归属均已保留，请重命名冲突项。")
        })
        .collect()
}

fn resolved_default_notebook_id(
    notebooks: &BTreeMap<String, NotebookMetadata>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let defaults = notebooks
        .values()
        .filter(|notebook| notebook.default)
        .count();
    match defaults {
        1 => notebooks
            .values()
            .find(|notebook| notebook.default)
            .map(|notebook| notebook.id.clone()),
        0 => {
            push_warning(
                warnings,
                "默认笔记本元数据尚未完整到达；新建和恢复副本已暂停。".into(),
            );
            None
        }
        _ => {
            push_warning(
                warnings,
                "笔记库包含多个默认笔记本；新建和恢复副本已暂停。".into(),
            );
            None
        }
    }
}

fn apply_notebook_metadata_to_view(library: &mut LibraryState) {
    library.view.default_notebook_id = library.default_notebook_id.clone();
    library.view.default_notebook_name = library
        .default_notebook_id
        .as_ref()
        .and_then(|id| library.notebooks.get(id))
        .map(|notebook| notebook.name.clone())
        .unwrap_or_else(|| "默认笔记本".into());
    library.view.notebooks = notebook_summaries(&library.notebooks);
    library
        .view
        .warnings
        .retain(|warning| !warning.contains("笔记本元数据") && !warning.contains("默认笔记本"));
    library
        .view
        .warnings
        .extend(library.metadata_warnings.clone());
}

fn notebook_summaries(notebooks: &BTreeMap<String, NotebookMetadata>) -> Vec<NotebookSummary> {
    let mut summaries = notebooks
        .values()
        .map(|notebook| NotebookSummary {
            notebook_id: notebook.id.clone(),
            name: notebook.name.clone(),
            is_default: notebook.default,
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.notebook_id.cmp(&right.notebook_id))
    });
    summaries
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
    storage::ensure_local_file(&path)?;
    if !metadata.is_file() {
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
    let bytes = read_sync_bounded(&canonical, MAX_METADATA_BYTES, "笔记本元数据")?;
    Ok((canonical, bytes))
}

fn notebook_metadata_cas(
    library: &LibraryState,
    notebook_id: &str,
) -> Result<(PathBuf, Vec<u8>), WorkspaceError> {
    let expected = library
        .notebook_metadata_baselines
        .get(notebook_id)
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            StorageError::new(
                "conflict",
                "笔记本元数据已变化或尚未取得本机基线；请重新选择笔记库后重试。",
            )
        })?;
    let (path, current) = read_notebook_metadata_file(&library.path, notebook_id)?;
    if &current != expected {
        return Err(StorageError::new(
            "conflict",
            "笔记本元数据已在其他位置变化；原文件已保留，请刷新列表并确认当前元数据后重试。",
        ));
    }
    let on_disk: NotebookMetadata = serde_json::from_slice(&current).map_err(|error| {
        StorageError::new("libraryMetadata", format!("笔记本元数据无效：{error}"))
    })?;
    if library.notebooks.get(notebook_id) != Some(&on_disk) {
        return Err(StorageError::new(
            "conflict",
            "笔记本元数据与当前列表不一致；请刷新笔记库后重试。",
        ));
    }
    Ok((path, expected.clone()))
}

fn ensure_notebook_baseline_current(
    library: &LibraryState,
    notebook_id: &str,
) -> Result<(), WorkspaceError> {
    notebook_metadata_cas(library, notebook_id).map(|_| ())
}

fn library_metadata_pending() -> WorkspaceError {
    StorageError::new(
        "libraryMetadataPending",
        "笔记库默认笔记本元数据尚未完整到达；请等待同步完成后重试。",
    )
}

fn count_notebook_notes(
    library_path: &Path,
    notebook_id: &str,
    notebooks: &BTreeMap<String, NotebookMetadata>,
) -> Result<usize, WorkspaceError> {
    let notes_path = library_path.join("notes");
    let entries = secure_read_dir_if_present(library_path, &notes_path)
        .map_err(|error| {
            StorageError::new(
                "notebookUsageUnknown",
                format!("无法读取笔记目录；不能确认笔记本为空：{}", error.message),
            )
        })?
        .ok_or_else(|| {
            StorageError::new(
                "notebookUsageUnknown",
                "笔记目录尚未到达；不能确认笔记本为空。",
            )
        })?;
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
        storage::ensure_local_file(&path).map_err(|error| {
            StorageError::new(
                "notebookUsageUnknown",
                format!("无法读取笔记文件；不能确认笔记本为空：{}", error.message),
            )
        })?;
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
            storage::ensure_local_directory(child)?;
            if !metadata.is_dir() {
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
    storage::ensure_local_directory(root)?;
    storage::ensure_local_file(path)?;
    let root = fs::canonicalize(root)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记库目录：{error}")))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查笔记文件：{error}")))?;
    if !metadata.is_file() {
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
    secure_read_dir_if_present(root, directory)?.ok_or_else(|| {
        StorageError::new(
            "io",
            format!("笔记库缺少必要目录：{}", display_path(directory)),
        )
    })
}

fn secure_read_dir_if_present(
    root: &Path,
    directory: &Path,
) -> Result<Option<Vec<fs::DirEntry>>, WorkspaceError> {
    storage::ensure_local_directory(root)?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| StorageError::new("io", format!("无法解析笔记库目录：{error}")))?;
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(StorageError::new("io", format!("无法检查库目录：{error}")));
        }
    };
    storage::ensure_local_directory(directory)?;
    if !metadata.is_dir() {
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
    let entries = fs::read_dir(canonical_dir)
        .map_err(|error| StorageError::new("io", format!("无法读取库目录：{error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::new("io", format!("无法读取库目录项目：{error}")))?;
    Ok(Some(entries))
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
            storage::ensure_local_directory(&requested)?;
            if !metadata.is_dir() {
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
    storage::ensure_local_directory(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::new("io", format!("无法检查所选目录：{error}")))?;
    if !metadata.is_dir() {
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

fn read_sync_bounded(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, WorkspaceError> {
    let _no_cloud_download = storage::NoCloudDownload::enter()?;
    let file = storage::open_local_read(path)?;
    let length = file
        .metadata()
        .map_err(|error| StorageError::new("io", format!("无法检查{label}：{error}")))?
        .len();
    if length > limit {
        return Err(StorageError::new(
            "libraryMetadata",
            format!("{label}超过大小上限。"),
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
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

pub(crate) fn display_path(path: &Path) -> String {
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
