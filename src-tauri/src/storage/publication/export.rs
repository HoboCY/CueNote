//! Publish one complete Markdown folder. No file is written at the final path
//! until the private sibling has been fully written and verified.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn publish_export(
    destination: &super::super::ResolvedTarget,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), StorageError> {
    let target = &destination.path;
    let parent = &destination.parent;
    let parent_handle = &destination.directory;
    parent_handle.ensure_current()?;
    match fs::symlink_metadata(target) {
        Ok(_) => {
            return Err(StorageError::new(
                "alreadyExists",
                "导出位置已存在，请选择新位置；没有覆盖任何文件。",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StorageError::io("无法检查导出位置", error)),
    }
    if !files.contains_key(Path::new("note.md"))
        || files.keys().any(|path| !valid_relative_name(path))
    {
        return Err(StorageError::new("export", "导出文件清单无效。"));
    }
    let staging = parent.join(format!(".cuenote-export-staging-{}", Uuid::new_v4()));
    let mut published = false;
    let result = (|| {
        let staged = parent_handle.create_child_directory(&staging)?;
        let assets_path = staging.join("assets");
        let assets = staged.create_child_directory(&assets_path)?;
        for (relative, bytes) in files {
            let directory = if relative.parent() == Some(Path::new("assets")) {
                &assets
            } else {
                &staged
            };
            let (candidate, mut file) = directory.create_candidate()?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| StorageError::io("无法完整写入导出文件", error))?;
            drop(file);
            crate::storage_faults::checkpoint("markdown_export_write", target, Some(&candidate))?;
            directory
                .move_no_replace(&candidate, &staging.join(relative))
                .map_err(|error| StorageError::io("无法完成暂存导出文件", error))?;
        }
        verify_files(&staging, files)?;
        assets
            .sync_publication_directory()
            .and_then(|_| staged.sync_publication_directory())
            .map_err(|error| StorageError::io("无法同步导出暂存目录", error))?;
        let identity = staged.identity.clone();
        let assets_identity = assets.identity.clone();
        crate::storage_faults::checkpoint("markdown_export_ready", target, None)?;
        // Windows intentionally pins ancestors without FILE_SHARE_DELETE. Drop
        // child pins before renaming; retain the original parent throughout.
        drop(assets);
        drop(staged);
        // move_no_replace can fail its post-operation check after the rename.
        // From this point errors must never trigger deletion or report success.
        published = true;
        parent_handle
            .move_no_replace(&staging, target)
            .map_err(|error| StorageError::io("无法确认导出目录发布结果", error))?;
        crate::storage_faults::checkpoint("markdown_export_published", target, None)?;
        let final_directory = StableDirectory::open(target)?;
        if final_directory.identity != identity {
            return Err(StorageError::new("unsafePath", "导出目录身份已变化。"));
        }
        let final_assets = StableDirectory::open(&target.join("assets"))?;
        if final_assets.identity != assets_identity {
            return Err(StorageError::new("unsafePath", "导出附件目录身份已变化。"));
        }
        crate::storage_faults::checkpoint("markdown_export_verified", target, None)?;
        parent_handle
            .sync_publication_directory()
            .map_err(|error| StorageError::io("无法确认导出目录同步结果", error))?;
        verify_files(target, files)?;
        final_assets.ensure_current()?;
        final_directory.ensure_current()?;
        Ok(())
    })();
    result.map_err(|error: StorageError| {
        StorageError::new(
            if published {
                "exportUncertain"
            } else {
                "exportFailed"
            },
            format!(
                "{} {}原生笔记没有因导出而修改。暂存位置：{}；目标位置：{}。",
                error.message,
                if published {
                    "导出结果尚未确认，请检查目录完整性后再使用；没有报告成功。"
                } else {
                    "导出未完成，可能留有暂存目录。"
                },
                staging.display(),
                target.display()
            ),
        )
    })
}

fn valid_relative_name(path: &Path) -> bool {
    if path == Path::new("note.md") {
        return true;
    }
    path.parent() == Some(Path::new("assets"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                !name.starts_with('.')
                    && name
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || b"-_.".contains(&ch))
            })
}

fn verify_files(root: &Path, expected: &BTreeMap<PathBuf, Vec<u8>>) -> Result<(), StorageError> {
    super::super::ensure_local_directory(root)?;
    super::super::ensure_local_directory(&root.join("assets"))?;
    let mut actual = BTreeSet::new();
    for directory in [root.to_path_buf(), root.join("assets")] {
        for entry in fs::read_dir(&directory)
            .map_err(|error| StorageError::io("无法核对导出文件集合", error))?
        {
            let path = entry
                .map_err(|error| StorageError::io("无法核对导出条目", error))?
                .path();
            if path == root.join("assets") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| StorageError::new("unsafePath", "导出条目越界。"))?
                .to_path_buf();
            let bytes = expected
                .get(&relative)
                .ok_or_else(|| StorageError::new("export", "导出目录包含非预期条目。"))?;
            let file = super::super::open_local_read(&path)?;
            let mut observed = Vec::new();
            file.take(bytes.len() as u64 + 1)
                .read_to_end(&mut observed)
                .map_err(|error| StorageError::io("无法回读导出文件", error))?;
            if observed != *bytes {
                return Err(StorageError::new("export", "导出文件回读不一致。"));
            }
            actual.insert(relative);
        }
    }
    if actual != expected.keys().cloned().collect() {
        return Err(StorageError::new("export", "导出文件集合不完整。"));
    }
    Ok(())
}

impl StableDirectory {
    fn create_child_directory(&self, path: &Path) -> Result<Self, StorageError> {
        self.ensure_current()?;
        if path.parent() != Some(self.path.as_path()) {
            return Err(StorageError::new(
                "unsafePath",
                "暂存目录必须是固定目录的直接子目录。",
            ));
        }
        #[cfg(windows)]
        fs::create_dir(path).map_err(|error| StorageError::io("无法创建导出暂存目录", error))?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::io::{AsRawFd, FromRawFd};
            #[cfg(target_os = "macos")]
            type Mode = u16;
            #[cfg(target_os = "linux")]
            type Mode = u32;
            unsafe extern "C" {
                fn mkdirat(fd: i32, name: *const std::ffi::c_char, mode: Mode) -> i32;
            }
            let name = self
                .child_component(path)
                .map_err(|error| StorageError::io("暂存目录名无效", error))?;
            if unsafe { mkdirat(self.directory_handle().as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                return Err(StorageError::io(
                    "无法创建导出暂存目录",
                    io::Error::last_os_error(),
                ));
            }
            // Open relative to the pinned parent; path-based Self::open would
            // introduce a new parent lookup between creation and pinning.
            #[cfg(target_os = "linux")]
            let flags = 0x0001_0000 | 0x0002_0000 | 0x0008_0000; // DIRECTORY, NOFOLLOW, CLOEXEC
            #[cfg(target_os = "macos")]
            let flags = 0x0010_0000 | 0x0000_0100 | 0x0100_0000; // DIRECTORY, NOFOLLOW, CLOEXEC
            let fd = unsafe { openat(self.directory_handle().as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(StorageError::io(
                    "无法固定导出暂存目录",
                    io::Error::last_os_error(),
                ));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let metadata = file
                .metadata()
                .map_err(|error| StorageError::io("无法核对暂存目录", error))?;
            let child = Self {
                path: path.to_path_buf(),
                identity: format!("unix:{}:{}", metadata.dev(), metadata.ino()),
                handles: vec![file],
            };
            child.ensure_current()?;
            return Ok(child);
        }
        #[cfg(windows)]
        {
            let child = Self::open(path)?;
            child.ensure_current()?;
            Ok(child)
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        Err(StorageError::new("export", "当前平台不支持安全导出。"))
    }
}
