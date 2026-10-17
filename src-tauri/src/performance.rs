//! Launch isolation only. All editor, storage, history and indexing code is shared
//! with the normal optimized build. This feature is never used for installers.
use std::{error::Error, fs, path::PathBuf};

use sha2::{Digest, Sha256};

const IDENTIFIER: &str = "com.hobocy.cuenote.performance";

/// Optional coarse diagnostics. Never used as desktop acceptance measurements.
pub(crate) struct Trace {
    operation: &'static str,
    start: std::time::Instant,
    last: std::time::Instant,
    enabled: bool,
}

impl Trace {
    pub(crate) fn new(operation: &'static str) -> Self {
        let now = std::time::Instant::now();
        Self {
            operation,
            start: now,
            last: now,
            enabled: std::env::var_os("CUENOTE_PERF_TRACE").as_deref()
                == Some(std::ffi::OsStr::new("1")),
        }
    }

    pub(crate) fn mark(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        if self.enabled {
            eprintln!(
                "{}",
                serde_json::json!({"diagnostic":"host-phase","operation":self.operation,
                "phase":phase,"elapsedMs":now.duration_since(self.last).as_secs_f64()*1000.0,
                "totalMs":now.duration_since(self.start).as_secs_f64()*1000.0})
            );
        }
        self.last = now;
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        self.mark("return");
    }
}

fn root() -> Result<PathBuf, Box<dyn Error>> {
    let path = PathBuf::from(
        std::env::var_os("CUENOTE_PERF_ROOT").ok_or("CUENOTE_PERF_ROOT is required")?,
    );
    if !path.is_absolute() {
        return Err("CUENOTE_PERF_ROOT must be absolute".into());
    }
    for ancestor in path.ancestors() {
        crate::storage::ensure_local_directory(ancestor)?;
        let metadata = fs::symlink_metadata(ancestor)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("Performance roots cannot use reparse points".into());
            }
        }
        if metadata.file_type().is_symlink() {
            return Err("Performance roots cannot use symbolic links".into());
        }
    }
    let path = path.canonicalize()?;
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("onedrive") || name == "cloudstorage" {
            return Err("Performance roots must be outside synchronized folders".into());
        }
    }
    for variable in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(sync) = std::env::var_os(variable) {
            if let Ok(sync) = PathBuf::from(sync).canonicalize() {
                if path.starts_with(sync) {
                    return Err("Performance roots must be outside OneDrive".into());
                }
            }
        }
    }
    let marker = path.join(".cuenote-performance");
    crate::storage::ensure_local_file(&marker)?;
    if fs::read(&marker)? != b"CueNote isolated performance root v1\n" {
        return Err("Performance root ownership marker is missing or invalid".into());
    }
    Ok(path)
}

pub(crate) fn workspace() -> Result<crate::workspace::Workspace, Box<dyn Error>> {
    let root = root()?;
    Ok(crate::workspace::Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    ))
}

pub(crate) fn isolate(
    mut context: tauri::Context<tauri::Wry>,
) -> Result<tauri::Context<tauri::Wry>, Box<dyn Error>> {
    if context.config().identifier != IDENTIFIER {
        return Err("Use tauri.performance.conf.json for performance builds".into());
    }
    let root = root()?;
    let profile = root.join("webview");
    fs::create_dir_all(&profile)?;
    crate::storage::ensure_local_directory(&profile)?;
    if profile.canonicalize()?.parent() != Some(root.as_path()) {
        return Err("Performance WebView profile escaped its isolated root".into());
    }
    // Absolute paths are resolved by PathBuf::join as absolute on Windows/Linux.
    // WKWebView instead uses a unique store identity (macOS 14+).
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    let mut store = [0_u8; 16];
    store.copy_from_slice(&digest[..16]);
    for window in &mut context.config_mut().app.windows {
        window.data_directory = Some(profile.clone());
        window.data_store_identifier = Some(store);
    }
    Ok(context)
}
