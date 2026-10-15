//! Read synchronized facts only when their contents are available locally.
//! Cloud placeholders are not links: accept only the verified Windows Cloud
//! Files tag family, while keeping unknown reparse points and symlinks denied.
use super::StorageError;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

fn unavailable() -> StorageError {
    StorageError::new("cloudUnavailable", "此文件或目录尚未完整保存在本机，或云盘尚未完成校验。请在 OneDrive 或 Finder 中下载整个笔记库并设置为始终保留在此设备，等待完成后重试；当前不会把它当作空白内容保存。")
}

fn unsafe_path() -> StorageError {
    StorageError::new(
        "unsafePath",
        "不能通过符号链接、目录联接或未知重解析点访问笔记库文件。",
    )
}

pub(super) fn read_error(context: &str, error: io::Error) -> StorageError {
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(11) {
        // EDEADLK is the documented result when materialization is disabled.
        return unavailable();
    }
    StorageError::new(
        "io",
        format!("{context}：{error}。若此位置由云盘管理，请确认文件已下载到本机后重试。"),
    )
}

fn check_metadata(metadata: &Metadata, directory: bool) -> Result<(), StorageError> {
    if metadata.file_type().is_symlink() {
        return Err(unsafe_path());
    }
    if directory && !metadata.is_dir() {
        return Err(StorageError::new("unsafePath", "所选位置必须是实际目录。"));
    }
    if !directory && !metadata.is_file() {
        return Err(StorageError::new("unsafePath", "所选笔记必须是普通文件。"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // RECALL_ON_OPEN overlaps FILE_ATTRIBUTE_EA outside enumeration data;
        // do not interpret that bit in MetadataExt::file_attributes().
        if metadata.file_attributes() & (0x0000_1000 | 0x0040_0000) != 0 {
            return Err(unavailable());
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::darwin::fs::MetadataExt;
        if metadata.st_flags() & 0x4000_0000 != 0 {
            return Err(unavailable());
        }
    }
    Ok(())
}

pub(crate) fn ensure_local_file(path: &Path) -> Result<(), StorageError> {
    ensure_local_path(path, false)
}

pub(crate) fn ensure_local_directory(path: &Path) -> Result<(), StorageError> {
    ensure_local_path(path, true)
}

fn ensure_local_path(path: &Path, directory: bool) -> Result<(), StorageError> {
    let _policy = NoCloudDownload::enter()?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| read_error("无法检查本机文件状态", error))?;
    check_metadata(&metadata, directory)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            let file = OpenOptions::new()
                .access_mode(0x0000_0080) // FILE_READ_ATTRIBUTES, no content read
                .share_mode(0x7)
                .custom_flags(0x0020_0000 | 0x0200_0000)
                .open(path)
                .map_err(|error| read_error("无法检查云盘文件状态", error))?;
            check_windows_handle(&file, true)?;
        }
    }
    Ok(())
}

pub(crate) fn open_local_read(path: &Path) -> Result<File, StorageError> {
    let _policy = NoCloudDownload::enter()?;
    ensure_local_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x1).custom_flags(0x0020_0000);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0000_0100);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000);
    }
    let file = options
        .open(path)
        .map_err(|error| read_error("无法打开本机笔记文件", error))?;
    check_metadata(
        &file
            .metadata()
            .map_err(|error| read_error("无法检查已打开文件", error))?,
        false,
    )?;
    #[cfg(windows)]
    check_windows_handle(&file, true)?;
    Ok(file)
}

#[cfg(windows)]
pub(super) fn check_windows_handle(file: &File, require_local: bool) -> Result<(), StorageError> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct AttributeTagInfo {
        attributes: u32,
        tag: u32,
    }
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: *mut std::ffi::c_void,
            class: i32,
            buffer: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    #[link(name = "CldApi")]
    unsafe extern "system" {
        fn CfGetPlaceholderStateFromAttributeTag(attributes: u32, tag: u32) -> u32;
    }
    let mut info = AttributeTagInfo {
        attributes: 0,
        tag: 0,
    };
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            9,
            (&mut info as *mut AttributeTagInfo).cast(),
            std::mem::size_of::<AttributeTagInfo>() as u32,
        )
    } == 0
    {
        return Err(read_error(
            "无法验证文件的重解析类型",
            io::Error::last_os_error(),
        ));
    }
    if require_local && info.attributes & (0x0000_1000 | 0x0040_0000) != 0 {
        return Err(unavailable());
    }
    if info.attributes & 0x0000_0400 == 0 {
        return Ok(());
    }
    if info.tag & !0x0000_f000 != 0x9000_001a {
        return Err(unsafe_path());
    }
    let state = unsafe { CfGetPlaceholderStateFromAttributeTag(info.attributes, info.tag) };
    if state == u32::MAX || state & 0x1 == 0 {
        return Err(unsafe_path());
    }
    if require_local && state & (0x10 | 0x20) != 0 {
        return Err(unavailable());
    }
    Ok(())
}

/// Scope this to one synchronous filesystem operation. The thread policy stops
/// a dataless ancestor from silently downloading while resolving a child path.
pub(crate) struct NoCloudDownload {
    #[cfg(target_os = "macos")]
    previous: i32,
    #[cfg(target_os = "macos")]
    _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl NoCloudDownload {
    pub(crate) fn enter() -> Result<Self, StorageError> {
        #[cfg(target_os = "macos")]
        {
            let previous = unsafe { getiopolicy_np(3, 1) };
            if previous < 0 || unsafe { setiopolicy_np(3, 1, 1) } != 0 {
                return Err(read_error(
                    "无法设置本机文件读取策略",
                    io::Error::last_os_error(),
                ));
            }
            Ok(Self {
                previous,
                _same_thread: std::marker::PhantomData,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Self {})
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for NoCloudDownload {
    fn drop(&mut self) {
        unsafe {
            setiopolicy_np(3, 1, self.previous);
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getiopolicy_np(kind: i32, scope: i32) -> i32;
    fn setiopolicy_np(kind: i32, scope: i32, policy: i32) -> i32;
}
