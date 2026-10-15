//! Local cross-process coordination for whole-library recovery.
//!
//! The lock lives outside the synchronized library. The marker is only a
//! persistent stop signal; it is never treated as a distributed lock.

use super::StorageError;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

pub(crate) const RESTORE_MARKER: &str = ".cuenote-restore.json";

pub(crate) struct LibraryAccessGuard {
    _file: File,
}

pub(crate) fn library_key(library: &Path) -> String {
    let identity = library.to_string_lossy();
    #[cfg(windows)]
    let identity = {
        let identity = identity.as_ref();
        let display_identity = identity
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| identity.strip_prefix(r"\\?\").map(str::to_owned))
            .unwrap_or_else(|| identity.to_owned());
        display_identity.to_lowercase()
    };
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

pub(crate) fn ensure_no_restore_marker(library: &Path) -> Result<(), StorageError> {
    super::ensure_local_directory(library)?;
    match fs::symlink_metadata(library.join(RESTORE_MARKER)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::io(
            "无法确认整库恢复状态，已暂停普通访问",
            error,
        )),
        Ok(_) => Err(StorageError::new(
            "libraryRestorePending",
            "此笔记库有未完成的整库恢复。请先继续恢复或回退，完成前不能编辑或备份中间状态。",
        )),
    }
}

pub(crate) fn lock_shared(
    recovery_root: &Path,
    library: &Path,
) -> Result<LibraryAccessGuard, StorageError> {
    ensure_no_restore_pending(recovery_root, library)?;
    let guard = acquire(recovery_root, library, false)?;
    ensure_no_restore_pending(recovery_root, library)?;
    Ok(guard)
}

pub(crate) fn ensure_no_restore_pending(
    recovery_root: &Path,
    library: &Path,
) -> Result<(), StorageError> {
    ensure_no_restore_pointer(recovery_root, library)?;
    ensure_no_restore_marker(library)?;
    let canonical = fs::canonicalize(library)
        .map_err(|error| StorageError::io("无法解析笔记库恢复状态路径", error))?;
    if canonical != library {
        ensure_no_restore_pointer(recovery_root, &canonical)?;
    }
    Ok(())
}

pub(crate) fn ensure_no_restore_pointer(
    recovery_root: &Path,
    library_identity: &Path,
) -> Result<(), StorageError> {
    if restore_pointer_present(recovery_root, library_identity)? {
        return Err(StorageError::new(
            "libraryRestorePending",
            "本机仍有此笔记库的恢复记录。请刷新恢复状态并完成继续或回退，不能绕过记录编辑。",
        ));
    }
    Ok(())
}

pub(crate) fn restore_pointer_present(
    recovery_root: &Path,
    library_identity: &Path,
) -> Result<bool, StorageError> {
    if !is_safe_directory_or_missing(recovery_root, "本机恢复目录")? {
        return Ok(false);
    }
    let restore_root = recovery_root.join("library-restores");
    if !is_safe_directory_or_missing(&restore_root, "整库恢复指针目录")? {
        return Ok(false);
    }
    let pointer = restore_root.join(format!("active-{}.json", library_key(library_identity)));
    match fs::symlink_metadata(pointer) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io("无法确认本机整库恢复状态", error)),
        Ok(_) => Ok(true),
    }
}

fn is_safe_directory_or_missing(path: &Path, label: &str) -> Result<bool, StorageError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io("无法确认本机整库恢复目录", error)),
        Ok(metadata) if metadata.is_dir() && !super::is_link_or_reparse(&metadata) => Ok(true),
        Ok(_) => Err(StorageError::new(
            "unsafePath",
            format!("{label}不是安全的本机普通目录。"),
        )),
    }
}

pub(crate) fn lock_exclusive(
    recovery_root: &Path,
    library: &Path,
) -> Result<LibraryAccessGuard, StorageError> {
    acquire(recovery_root, library, true)
}

fn acquire(
    recovery_root: &Path,
    library: &Path,
    exclusive: bool,
) -> Result<LibraryAccessGuard, StorageError> {
    super::ensure_local_directory(library)?;
    let canonical = fs::canonicalize(library)
        .map_err(|error| StorageError::io("无法解析笔记库协调路径", error))?;
    let recovery = super::prepare_recovery_root(recovery_root, &canonical)?;
    let locks = recovery.join(".locks");
    match fs::create_dir(&locks) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(StorageError::io("无法创建笔记库协调目录", error)),
    }
    super::ensure_local_directory(&locks)?;
    let metadata = fs::symlink_metadata(&locks)
        .map_err(|error| StorageError::io("无法检查笔记库协调目录", error))?;
    if super::is_link_or_reparse(&metadata) {
        return Err(StorageError::new(
            "unsafePath",
            "笔记库协调目录不能是链接或重解析点。",
        ));
    }
    let lock_path = locks.join(format!("{}.library.lock", library_key(&canonical)));
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if !metadata.is_file() || super::is_link_or_reparse(&metadata) => {
            return Err(StorageError::new(
                "unsafePath",
                "笔记库协调锁必须是本机普通文件。",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StorageError::io("无法检查笔记库协调锁", error)),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x3).custom_flags(0x0020_0000);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0000_0100);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000);
    }
    let file = options
        .open(&lock_path)
        .map_err(|error| StorageError::io("无法打开笔记库协调锁", error))?;
    if !file
        .metadata()
        .map_err(|error| StorageError::io("无法检查已打开的协调锁", error))?
        .is_file()
    {
        return Err(StorageError::new(
            "unsafePath",
            "已打开的协调锁不是普通文件。",
        ));
    }
    #[cfg(windows)]
    super::local_files::check_windows_handle(&file, true)?;
    let result = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match result {
        Ok(()) => Ok(LibraryAccessGuard { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(StorageError::new(
            "libraryBusy",
            "另一个 CueNote 操作正在使用此笔记库；请等待完成后重试。",
        )),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(StorageError::io("无法协调笔记库访问", error))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn distinct_case_sensitive_libraries_do_not_share_recovery_state() {
        let root = tempfile::tempdir().unwrap();
        let upper = root.path().join("Library");
        let lower = root.path().join("library");
        fs::create_dir(&upper).unwrap();
        match fs::create_dir(&lower) {
            Ok(()) => {}
            // A case-insensitive volume cannot hold the two distinct libraries.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return,
            Err(error) => panic!("cannot prepare second library: {error}"),
        }
        let recovery = root.path().join("recovery");
        let _upper_guard = lock_exclusive(&recovery, &upper).unwrap();
        let _lower_guard = lock_shared(&recovery, &lower).unwrap();
        let active = recovery.join("library-restores");
        fs::create_dir_all(&active).unwrap();
        let canonical_upper = fs::canonicalize(&upper).unwrap();
        fs::write(
            active.join(format!("active-{}.json", library_key(&canonical_upper))),
            b"pending",
        )
        .unwrap();
        assert!(ensure_no_restore_pending(&recovery, &upper).is_err());
        assert!(ensure_no_restore_pending(&recovery, &lower).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn missing_restore_root_symlink_fails_closed_instead_of_hiding_a_pointer() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("Library");
        let recovery = root.path().join("recovery");
        let restore_root = recovery.join("library-restores");
        fs::create_dir_all(&library).unwrap();
        fs::create_dir_all(&restore_root).unwrap();
        fs::rename(&restore_root, recovery.join("library-restores-away")).unwrap();
        symlink(recovery.join("missing-restore-root"), &restore_root).unwrap();

        let error = ensure_no_restore_pointer(&recovery, &library).unwrap_err();
        assert_eq!(error.kind, "unsafePath");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn canonical_and_display_identities_share_a_pending_pointer() {
        let root = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(root.path()).unwrap();
        let canonical_text = canonical.to_string_lossy();
        let display = Path::new(
            canonical_text
                .strip_prefix(r"\\?\")
                .expect("Windows canonical path should use an extended prefix"),
        );
        let recovery = root.path().join("recovery");
        let pointer_directory = recovery.join("library-restores");
        fs::create_dir_all(&pointer_directory).unwrap();
        fs::write(
            pointer_directory.join(format!("active-{}.json", library_key(&canonical))),
            b"pending",
        )
        .unwrap();

        let error = ensure_no_restore_pointer(&recovery, display).unwrap_err();
        assert_eq!(error.kind, "libraryRestorePending");
        assert_eq!(library_key(&canonical), library_key(display));
        assert_eq!(
            library_key(Path::new(r"\\?\UNC\server\share\Library")),
            library_key(Path::new(r"\\server\share\Library"))
        );
    }

    #[test]
    fn restore_pointer_directory_junction_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("Library");
        let recovery = root.path().join("recovery");
        let restore_root = recovery.join("library-restores");
        let junction_target = root.path().join("restore-target");
        fs::create_dir_all(&library).unwrap();
        fs::create_dir_all(&recovery).unwrap();
        fs::create_dir_all(&junction_target).unwrap();

        let normal_path = |path: &Path| {
            let value = path.to_string_lossy();
            value
                .strip_prefix(r"\\?\")
                .unwrap_or(value.as_ref())
                .replace('/', "\\")
        };
        let linked = Command::new("cmd.exe")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(normal_path(&restore_root))
            .arg(normal_path(&junction_target))
            .output()
            .unwrap();
        assert!(
            linked.status.success(),
            "could not create isolated junction fixture: stdout={}, stderr={}",
            String::from_utf8_lossy(&linked.stdout),
            String::from_utf8_lossy(&linked.stderr),
        );

        let error = restore_pointer_present(&recovery, &library).unwrap_err();
        assert_eq!(error.kind, "unsafePath");
        fs::remove_dir(&restore_root).unwrap();
    }

    #[test]
    fn restore_pointer_parent_that_is_not_a_directory_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("Library");
        let recovery = root.path().join("recovery");
        fs::create_dir_all(&library).unwrap();
        fs::create_dir_all(&recovery).unwrap();
        fs::write(recovery.join("library-restores"), b"not a directory").unwrap();

        let error = restore_pointer_present(&recovery, &library).unwrap_err();
        assert_eq!(error.kind, "unsafePath");
    }
}
