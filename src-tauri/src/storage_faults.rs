//! Opt-in process/error injection at real storage boundaries. These controls are
//! absent from normal execution and never read by release builds.
use crate::storage::StorageError;
use std::path::Path;

pub(crate) fn checkpoint(
    stage: &str,
    target: &Path,
    candidate: Option<&Path>,
) -> Result<(), StorageError> {
    #[cfg(all(debug_assertions, feature = "storage-fault-injection"))]
    {
        enabled::checkpoint(stage, target, candidate)
    }
    #[cfg(not(all(debug_assertions, feature = "storage-fault-injection")))]
    {
        let _ = (stage, target, candidate);
        Ok(())
    }
}

#[cfg(all(debug_assertions, feature = "storage-fault-injection"))]
mod enabled {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    static INJECTED: AtomicBool = AtomicBool::new(false);

    pub(super) fn checkpoint(
        stage: &str,
        target: &Path,
        candidate: Option<&Path>,
    ) -> Result<(), StorageError> {
        if std::env::var("CUENOTE_FAULT_STAGE").ok().as_deref() != Some(stage) {
            return Ok(());
        }
        let Some(root) = std::env::var_os("CUENOTE_FAULT_ROOT").map(PathBuf::from) else {
            return Ok(());
        };
        if !root.is_absolute() {
            return Ok(());
        }
        let Ok(root) = fs::canonicalize(root) else {
            return Ok(());
        };
        let inside = |path: &Path| {
            path.parent()
                .and_then(|parent| fs::canonicalize(parent).ok())
                .is_some_and(|parent| parent.starts_with(&root))
        };
        if !inside(target) || candidate.is_some_and(|path| !inside(path)) {
            return Ok(());
        }
        if !matches!(
            stage,
            "recovery_snapshot_persisted"
                | "candidate_created"
                | "candidate_synced_before_validation"
                | "candidate_validated"
                | "history_persisted"
                | "publish_before"
                | "publish_after"
        ) {
            return Ok(());
        }
        let action = std::env::var("CUENOTE_FAULT_ACTION").unwrap_or_default();
        if !matches!(
            action.as_str(),
            "exit" | "io" | "disk_full" | "corrupt" | "pause"
        ) {
            return Ok(());
        }
        if INJECTED.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // A marker makes parent-process observation authoritative; an absent
        // marker is never counted as the requested stage having been reached.
        let marker = root.join(format!(".cuenote-fault-hit-{stage}"));
        let mut marker_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .map_err(|error| StorageError::new("faultHarness", error.to_string()))?;
        write!(marker_file, "{stage}\n{action}\n{}\n", std::process::id())
            .and_then(|_| marker_file.sync_all())
            .map_err(|error| StorageError::new("faultHarness", error.to_string()))?;

        if (stage == "candidate_created" && matches!(action.as_str(), "exit" | "io" | "disk_full"))
            || action == "corrupt"
        {
            if let Some(candidate) = candidate {
                if !candidate
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".cuenote-candidate-"))
                {
                    return Err(StorageError::new("faultHarness", "Not a candidate file"));
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .open(candidate)
                    .map_err(|error| StorageError::new("faultHarness", error.to_string()))?;
                file.set_len(0)
                    .and_then(|_| file.write_all(b"PK\x03\x04"))
                    .and_then(|_| file.sync_all())
                    .map_err(|error| StorageError::new("faultHarness", error.to_string()))?;
            }
        }
        match action.as_str() {
            "exit" => std::process::exit(86),
            "io" => Err(StorageError::new("io", "测试注入：文件操作暂时不可用。")),
            "disk_full" => {
                #[cfg(windows)]
                let code = 112; // ERROR_DISK_FULL
                #[cfg(not(windows))]
                let code = 28; // ENOSPC
                Err(StorageError::new(
                    "io",
                    format!(
                        "候选写入失败：{}（隔离故障注入）",
                        std::io::Error::from_raw_os_error(code)
                    ),
                ))
            }
            "pause" => {
                let resume = root.join(".cuenote-fault-resume");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while !resume.exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                if !resume.exists() {
                    return Err(StorageError::new(
                        "faultHarness",
                        "Timed out waiting for test resume",
                    ));
                }
                Ok(())
            }
            _ => Ok(()), // Corruption is rejected by the actual package reader.
        }
    }
}
