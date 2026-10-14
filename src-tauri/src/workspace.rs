//! The host-owned library selection and note editing session.
//!
//! The frontend receives only DTOs and opaque session identifiers. Paths,
//! package contents, organization fields, and revision baselines stay here.

use crate::note_format::{NotePackage, PlainText};
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
const MAX_PREFERENCE_BYTES: u64 = 16 * 1024;
const MAX_PREFERENCE_SNAPSHOTS: usize = 64;
const RETAINED_PREFERENCE_SNAPSHOTS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteSummary {
    pub note_id: String,
    pub title: String,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryView {
    pub path: String,
    pub default_notebook_name: String,
    pub notes: Vec<NoteSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteSession {
    pub session_id: String,
    pub note_id: String,
    pub text: PlainText,
    pub revision: Option<String>,
    pub read_only_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceView {
    pub app_version: String,
    pub default_library_path: String,
    pub library: Option<LibraryView>,
    pub session: Option<NoteSession>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveReceipt {
    pub session: NoteSession,
    pub library: LibraryView,
}

/// A persisted notebook is intentionally independent of every `.cuenote` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotebookMetadata {
    schema_version: u32,
    id: String,
    name: String,
    default: bool,
    timestamps: NotebookTimestamps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            initialized: false,
        }
    }

    pub fn snapshot(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        if !self.initialized {
            self.initialized = true;
            match self.read_latest_preference() {
                Ok(Some(preference)) => {
                    let path = PathBuf::from(preference.library_path);
                    match self.load_existing_library(&path) {
                        Ok(library) => self.library = Some(library),
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
        self.persist_preference(&library.path)?;
        self.library = Some(library);
        self.session = None;
        self.notice = None;
        self.initialized = true;
        Ok(self.view())
    }

    pub fn use_default_library(&mut self) -> Result<WorkspaceView, WorkspaceError> {
        self.select_library(self.default_library_path.clone())
    }

    pub fn new_note(&mut self) -> Result<NoteSession, WorkspaceError> {
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| StorageError::new("noLibrary", "请先选择或使用默认笔记库。"))?;
        let package = NotePackage::new_plain(
            library.default_notebook_id.clone(),
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
        let path = library
            .path
            .join("notes")
            .join(format!("{note_id}.cuenote"));
        let view = NoteSession {
            session_id: Uuid::new_v4().to_string(),
            note_id,
            text: package.text_projection(),
            revision: None,
            read_only_reason: None,
        };
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package,
            baseline_revision: None,
            notebook_id: Some(library.default_notebook_id.clone()),
        });
        Ok(view)
    }

    pub fn open_note(&mut self, note_id: &str) -> Result<NoteSession, WorkspaceError> {
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
        let notebook_id = opened.package.document.organization.notebook_id.clone();
        let read_only_reason =
            read_only_reason(&opened.package, notebook_id.as_deref(), &library.notebooks);
        let view = note_session_from_opened(note_id, &opened, read_only_reason.clone());
        self.session = Some(ActiveNoteSession {
            view: view.clone(),
            path,
            package: opened.package,
            baseline_revision: Some(opened.revision),
            notebook_id,
        });
        Ok(view)
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

    pub fn library_path(&self) -> Result<PathBuf, WorkspaceError> {
        self.library
            .as_ref()
            .map(|library| library.path.clone())
            .ok_or_else(|| StorageError::new("noLibrary", "当前没有已打开的笔记库。"))
    }

    fn view(&self) -> WorkspaceView {
        WorkspaceView {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            default_library_path: display_path(&self.default_library_path),
            library: self.library.as_ref().map(|library| library.view.clone()),
            session: self.session.as_ref().map(|session| session.view.clone()),
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

    fn persist_preference(&self, path: &Path) -> Result<(), WorkspaceError> {
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
    if notebook_id.is_none() {
        return Some("此笔记缺少笔记本归属，当前以只读方式打开。".into());
    }
    if !notebooks.contains_key(notebook_id.unwrap()) {
        return Some("此笔记属于未知笔记本，当前以只读方式打开。".into());
    }
    if package.plain_text().is_none() {
        return Some("此笔记包含简易编辑器尚不支持的内容，当前以只读方式打开。".into());
    }
    None
}

fn note_session_from_opened(
    note_id: &str,
    opened: &OpenedNote,
    read_only_reason: Option<String>,
) -> NoteSession {
    NoteSession {
        session_id: Uuid::new_v4().to_string(),
        note_id: note_id.to_owned(),
        text: opened.package.text_projection(),
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
        if notebook_id.is_none() {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记 {stem} 缺少笔记本归属，保存前需先处理。"),
            );
        } else if !notebooks.contains_key(notebook_id.unwrap()) {
            push_warning_counted(
                &mut warnings,
                &mut omitted_warnings,
                format!("笔记 {stem} 属于未知笔记本，当前不会重新归属。"),
            );
        }
        notes.push(NoteSummary {
            note_id: stem.to_owned(),
            title: opened.package.document.title.clone(),
            modified_at: opened
                .package
                .document
                .timestamps
                .content_modified_at
                .clone(),
        });
    }
    if omitted_warnings > 0 {
        warnings.push(format!("另有 {omitted_warnings} 条笔记警告未显示。"));
    }
    notes.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| left.note_id.cmp(&right.note_id))
    });
    let default_notebook_name = notebooks
        .get(default_notebook_id)
        .map(|metadata| metadata.name.clone())
        .unwrap_or_else(|| "默认笔记本".into());
    Ok(LibraryView {
        path: display_path(library_path),
        default_notebook_name,
        notes,
        warnings,
    })
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

fn new_default_notebook() -> NotebookMetadata {
    let now = now_timestamp();
    NotebookMetadata {
        schema_version: LIBRARY_SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        name: "默认笔记本".into(),
        default: true,
        timestamps: NotebookTimestamps {
            created_at: now.clone(),
            modified_at: now,
        },
    }
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
