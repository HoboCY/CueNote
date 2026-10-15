//! Crash-safe publication of opaque files during whole-library restoration.
//!
//! The caller persists the restore intent before invoking this module. This
//! module only moves exact, bounded byte sequences and leaves every recovery
//! artifact in place for inspection or retry.

use super::publication::StableDirectory;
#[cfg(windows)]
use super::replace_windows;
use super::{
    acquire_save_lock, ensure_local_directory, ensure_local_file, open_local_read,
    prepare_recovery_root, StorageError,
};
use crate::note_format::MAX_PACKAGE_BYTES;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FactDigest {
    pub(crate) sha256: String,
    pub(crate) length: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct RawFactStep {
    pub(crate) target: PathBuf,
    pub(crate) recovery_root: PathBuf,
    pub(crate) before: Option<FactDigest>,
    pub(crate) after: Option<FactDigest>,
    pub(crate) source: Option<PathBuf>,
    pub(crate) candidate: PathBuf,
    pub(crate) displaced: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepState {
    Complete,
    NeedCandidate,
    ReadyToPublish,
    NeedDisplace,
    NeedFinishPartialCreate,
    RecoverPartialCandidate,
}

/// Reads an opaque fact file and returns its exact bounded byte digest.
pub(crate) fn observe_fact(path: &Path) -> Result<Option<FactDigest>, StorageError> {
    let _policy = super::NoCloudDownload::enter()?;
    if !path.is_absolute() {
        return Err(invalid_step("事实文件路径必须是绝对路径。"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid_step("事实文件路径缺少父目录。"))?;
    ensure_local_directory(parent)?;
    let directory = StableDirectory::open(parent)?;
    directory.ensure_current()?;
    let digest = observe_in_directory(path)?;
    directory.ensure_current()?;
    Ok(digest)
}

/// Reports whether this exact step has completed without changing its facts.
pub(crate) fn inspect_step(step: &RawFactStep) -> Result<bool, StorageError> {
    let _policy = super::NoCloudDownload::enter()?;
    validate_step(step)?;
    let parent = step.target.parent().expect("validated target parent");
    let recovery_root = prepare_recovery_root(&step.recovery_root, parent)?;
    ensure_local_directory(&recovery_root)?;
    let _lock = acquire_save_lock(&recovery_root, &step.target)
        .map_err(|error| StorageError::io("无法协调整库事实检查", error))?;
    let directory = StableDirectory::open(parent)?;
    Ok(assess_step(step, &directory)? == StepState::Complete)
}

/// Publishes one opaque fact step. Re-entry recognizes completed and
/// interrupted operations using only the target and the two owned artifacts.
pub(crate) fn publish_fact(step: &RawFactStep) -> Result<(), StorageError> {
    let _policy = super::NoCloudDownload::enter()?;
    validate_step(step)?;
    let parent = step.target.parent().expect("validated target parent");
    let recovery_root = prepare_recovery_root(&step.recovery_root, parent)?;
    ensure_local_directory(&recovery_root)?;
    let _lock = acquire_save_lock(&recovery_root, &step.target)
        .map_err(|error| StorageError::io("无法协调整库事实发布", error))?;
    let directory = StableDirectory::open(parent)?;

    let mut state = assess_step(step, &directory)?;
    if state == StepState::Complete {
        return Ok(());
    }

    if state == StepState::NeedCandidate {
        prepare_candidate(step, &directory)?;
        state = assess_step(step, &directory)?;
    }

    if state == StepState::RecoverPartialCandidate {
        preserve_partial_candidate(step, &directory)?;
        state = assess_step(step, &directory)?;
        if state == StepState::NeedCandidate {
            prepare_candidate(step, &directory)?;
            state = assess_step(step, &directory)?;
        }
    }

    match state {
        StepState::Complete => Ok(()),
        StepState::NeedCandidate => {
            Err(conflict("候选文件未能按预期准备；所有事实和工件均已保留。"))
        }
        StepState::ReadyToPublish => publish_ready_step(step, &directory),
        StepState::NeedDisplace => finish_displacement(step, &directory),
        StepState::NeedFinishPartialCreate => finish_partial_create(step, &directory),
        StepState::RecoverPartialCandidate => Err(conflict(
            "事实候选仍处于未完成状态；目标和已有工件均已保留。",
        )),
    }
}

fn validate_step(step: &RawFactStep) -> Result<(), StorageError> {
    if !step.target.is_absolute() || !step.recovery_root.is_absolute() {
        return Err(invalid_step("目标路径和恢复目录必须是绝对路径。"));
    }
    let parent = step
        .target
        .parent()
        .ok_or_else(|| invalid_step("目标事实路径缺少父目录。"))?;
    if !parent.is_absolute() {
        return Err(invalid_step("目标事实父目录必须是绝对路径。"));
    }
    if step.candidate.parent() != Some(parent) || step.displaced.parent() != Some(parent) {
        return Err(invalid_step("候选和移出工件必须与目标位于同一目录。"));
    }
    validate_artifact_name(&step.candidate)?;
    validate_artifact_name(&step.displaced)?;
    let target_name = step
        .target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_step("目标事实文件名无效。"))?;
    let candidate_name = step.candidate.file_name().and_then(|name| name.to_str());
    let displaced_name = step.displaced.file_name().and_then(|name| name.to_str());
    if candidate_name.is_none()
        || displaced_name.is_none()
        || same_component(target_name, candidate_name.expect("checked candidate name"))
        || same_component(target_name, displaced_name.expect("checked displaced name"))
        || same_component(
            candidate_name.expect("checked candidate name"),
            displaced_name.expect("checked displaced name"),
        )
    {
        return Err(invalid_step("目标、候选和移出工件必须使用不同文件名。"));
    }

    match (&step.before, &step.after) {
        (None, None) => return Err(invalid_step("整库事实步骤不能同时缺少前后版本。")),
        (Some(before), Some(after)) if before == after => {
            return Err(invalid_step("整库事实步骤不能发布相同的前后版本。"));
        }
        _ => {}
    }
    if step.after.is_none() && step.source.is_some() {
        return Err(invalid_step("删除事实步骤不能指定源文件。"));
    }
    for digest in step.before.iter().chain(step.after.iter()) {
        validate_digest(digest)?;
    }
    if let Some(source) = &step.source {
        if !source.is_absolute()
            || path_matches(source, &step.target)
            || path_matches(source, &step.candidate)
            || path_matches(source, &step.displaced)
        {
            return Err(invalid_step("事实源文件路径无效或与目标工件重合。"));
        }
    }
    Ok(())
}

fn validate_artifact_name(path: &Path) -> Result<(), StorageError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_step("事实工件必须使用可验证的固定文件名。"))?;
    if name.len() > 180
        || !name.ends_with(".tmp")
        || name == ".tmp"
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid_step(
            "事实候选和移出工件必须使用安全的 ASCII .tmp 文件名。",
        ));
    }
    Ok(())
}

fn validate_digest(digest: &FactDigest) -> Result<(), StorageError> {
    if digest.length > MAX_PACKAGE_BYTES
        || digest.sha256.len() != 64
        || !digest
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_step("整库事实版本摘要格式无效或超过大小上限。"));
    }
    Ok(())
}

fn same_component(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn path_matches(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn assess_step(step: &RawFactStep, directory: &StableDirectory) -> Result<StepState, StorageError> {
    directory.ensure_current()?;
    let target = observe_in_directory(&step.target)?;
    let candidate = observe_in_directory(&step.candidate)?;
    let displaced = observe_in_directory(&step.displaced)?;
    let before = step.before.as_ref();
    let after = step.after.as_ref();

    let matches =
        |actual: &Option<FactDigest>, expected: Option<&FactDigest>| actual.as_ref() == expected;

    match (before, after) {
        (None, Some(after)) => {
            if matches(&target, Some(after))
                && matches(&displaced, None)
                && (matches(&candidate, None) || matches(&candidate, Some(after)))
            {
                return Ok(StepState::Complete);
            }
            if matches(&target, None) && matches(&displaced, None) {
                return match candidate {
                    None => Ok(StepState::NeedCandidate),
                    Some(candidate) if &candidate == after => Ok(StepState::ReadyToPublish),
                    Some(candidate)
                        if candidate_is_source_prefix_at(step, &step.candidate, &candidate)? =>
                    {
                        Ok(StepState::RecoverPartialCandidate)
                    }
                    Some(_) => Err(conflict("创建目标、候选或移出工件包含未知版本。")),
                };
            }
            Err(conflict("创建目标、候选或移出工件包含未知版本。"))
        }
        (Some(before), None) => {
            if matches(&target, None)
                && matches(&displaced, Some(before))
                && matches(&candidate, None)
            {
                return Ok(StepState::Complete);
            }
            if matches(&target, Some(before))
                && matches(&displaced, None)
                && matches(&candidate, None)
            {
                return Ok(StepState::ReadyToPublish);
            }
            Err(conflict("删除目标、候选或移出工件包含未知版本。"))
        }
        (Some(before), Some(after)) => {
            if matches(&target, Some(after)) && matches(&displaced, Some(before)) {
                return match candidate {
                    None => Ok(StepState::Complete),
                    Some(candidate) if &candidate == after => Ok(StepState::Complete),
                    Some(_) => Err(conflict("替换目标、候选或移出工件包含未知版本。")),
                };
            }
            if matches(&target, Some(before)) && matches(&displaced, None) {
                return match candidate {
                    None => Ok(StepState::NeedCandidate),
                    Some(candidate) if &candidate == after => Ok(StepState::ReadyToPublish),
                    Some(candidate)
                        if candidate_is_source_prefix_at(step, &step.candidate, &candidate)? =>
                    {
                        Ok(StepState::RecoverPartialCandidate)
                    }
                    Some(_) => Err(conflict("替换目标、候选或移出工件包含未知版本。")),
                };
            }
            if matches(&target, Some(after))
                && matches(&displaced, None)
                && matches(&candidate, Some(before))
            {
                return Ok(StepState::NeedDisplace);
            }
            if matches(&target, None)
                && matches(&displaced, Some(before))
                && matches(&candidate, Some(after))
            {
                return Ok(StepState::NeedFinishPartialCreate);
            }
            Err(conflict("替换目标、候选或移出工件包含未知版本。"))
        }
        (None, None) => unreachable!("validated fact step has at least one version"),
    }
}

fn prepare_candidate(step: &RawFactStep, directory: &StableDirectory) -> Result<(), StorageError> {
    let after = step
        .after
        .as_ref()
        .expect("only steps with an after version need candidates");
    let source = step
        .source
        .as_deref()
        .ok_or_else(|| conflict("缺少生成预期恢复事实的源文件；未覆盖任何事实。"))?;
    directory.ensure_current()?;
    ensure_local_file(source)?;
    let mut input = open_local_read(source)?;
    let metadata = input
        .metadata()
        .map_err(|error| StorageError::io("无法读取整库事实源文件信息", error))?;
    if metadata.len() > MAX_PACKAGE_BYTES {
        return Err(invalid_step("整库事实源文件超过大小上限。"));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&step.candidate)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => conflict("事实候选路径已被占用；未覆盖该文件。"),
            _ => StorageError::io("无法无覆盖创建整库事实候选", error),
        })?;

    output
        .sync_all()
        .map_err(|error| StorageError::io("无法同步新建整库事实候选", error))?;
    directory
        .sync_publication_directory()
        .map_err(|error| StorageError::io("无法同步新建整库事实候选目录", error))?;
    crate::storage_faults::checkpoint(
        "library_fact_candidate_created",
        &step.target,
        Some(&step.candidate),
    )?;

    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| StorageError::io("无法读取整库事实源文件", error))?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        if length > MAX_PACKAGE_BYTES {
            return Err(invalid_step("整库事实源文件超过大小上限。"));
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|error| StorageError::io("无法写入整库事实候选", error))?;
        if length == count as u64 {
            crate::storage_faults::checkpoint(
                "library_fact_candidate_partial",
                &step.target,
                Some(&step.candidate),
            )?;
        }
    }
    output
        .sync_all()
        .map_err(|error| StorageError::io("无法同步整库事实候选", error))?;
    let copied = FactDigest {
        sha256: format!("{:x}", hash.finalize()),
        length,
    };
    if &copied != after {
        return Err(conflict(
            "整库事实源文件与已持久化的恢复版本不一致；候选已保留。",
        ));
    }
    directory
        .sync_publication_directory()
        .map_err(|error| StorageError::io("无法同步整库事实候选目录", error))?;
    crate::storage_faults::checkpoint(
        "library_fact_candidate_ready",
        &step.target,
        Some(&step.candidate),
    )?;
    Ok(())
}

/// Returns true only when the candidate is a strict, byte-for-byte prefix of
/// the fully verified source version. This makes a partial owned candidate
/// recoverable while keeping arbitrary or complete-but-different files as a
/// conflict.
fn candidate_is_source_prefix_at(
    step: &RawFactStep,
    candidate_path: &Path,
    candidate_digest: &FactDigest,
) -> Result<bool, StorageError> {
    let Some(after) = &step.after else {
        return Ok(false);
    };
    if after.length == 0 || candidate_digest.length >= after.length {
        return Ok(false);
    }
    let Some(source) = &step.source else {
        return Ok(false);
    };

    ensure_local_file(source)?;
    ensure_local_file(candidate_path)?;
    let mut source_file = open_local_read(source)?;
    let mut candidate_file = open_local_read(candidate_path)?;
    let source_metadata = source_file
        .metadata()
        .map_err(|error| StorageError::io("无法读取整库事实源文件信息", error))?;
    let candidate_metadata = candidate_file
        .metadata()
        .map_err(|error| StorageError::io("无法读取整库事实候选信息", error))?;
    if source_metadata.len() != after.length
        || candidate_metadata.len() != candidate_digest.length
        || source_metadata.len() > MAX_PACKAGE_BYTES
    {
        return Ok(false);
    }

    let mut source_hash = Sha256::new();
    let mut candidate_hash = Sha256::new();
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    let mut remaining = candidate_digest.length;
    while remaining > 0 {
        let count = usize::try_from(remaining.min(COPY_BUFFER_BYTES as u64))
            .expect("copy chunk fits usize");
        let mut source_chunk = vec![0; count];
        let mut candidate_chunk = vec![0; count];
        source_file
            .read_exact(&mut source_chunk)
            .map_err(|error| StorageError::io("无法读取整库事实源文件", error))?;
        candidate_file
            .read_exact(&mut candidate_chunk)
            .map_err(|error| StorageError::io("无法读取整库事实候选", error))?;
        if source_chunk != candidate_chunk {
            return Ok(false);
        }
        source_hash.update(&source_chunk);
        candidate_hash.update(&candidate_chunk);
        remaining -= count as u64;
    }
    if candidate_file
        .read(&mut buffer[..1])
        .map_err(|error| StorageError::io("无法读取整库事实候选", error))?
        != 0
    {
        return Ok(false);
    }

    let mut source_length = candidate_digest.length;
    loop {
        let count = source_file
            .read(&mut buffer)
            .map_err(|error| StorageError::io("无法读取整库事实源文件", error))?;
        if count == 0 {
            break;
        }
        source_length = source_length.saturating_add(count as u64);
        if source_length > MAX_PACKAGE_BYTES {
            return Ok(false);
        }
        source_hash.update(&buffer[..count]);
    }
    let source_actual = FactDigest {
        sha256: format!("{:x}", source_hash.finalize()),
        length: source_length,
    };
    let candidate_actual = FactDigest {
        sha256: format!("{:x}", candidate_hash.finalize()),
        length: candidate_digest.length,
    };
    Ok(source_actual == *after && candidate_actual == *candidate_digest)
}

/// Preserves a verified partial candidate before clearing its deterministic
/// name and rebuilding it from the immutable, digest-checked source.
fn preserve_partial_candidate(
    step: &RawFactStep,
    directory: &StableDirectory,
) -> Result<(), StorageError> {
    if assess_step(step, directory)? != StepState::RecoverPartialCandidate {
        return Err(conflict(
            "部分候选或其目标状态已变化；没有移动目标或移出版本。",
        ));
    }
    let expected = observe_in_directory(&step.candidate)?
        .ok_or_else(|| conflict("部分事实候选已消失；目标和移出版本未修改。"))?;
    let evidence = unique_partial_artifact(step, "evidence");
    copy_candidate_to_evidence(step, directory, &evidence, &expected)?;

    directory.ensure_current()?;
    let quarantined = unique_partial_artifact(step, "candidate");
    move_no_replace(
        directory,
        &step.candidate,
        &quarantined,
        "保留中断的事实候选",
    )?;
    sync_publication(directory, "无法同步中断事实候选的保留工件目录")?;

    let quarantined_digest = observe_in_directory(&quarantined)?;
    if quarantined_digest.as_ref() != Some(&expected)
        || !candidate_is_source_prefix_at(step, &quarantined, &expected)?
        || !matches_expected_before(step)?
        || observe_in_directory(&step.displaced)?.is_some()
    {
        // If another writer substituted the candidate while it was being
        // preserved, put it back at the deterministic name when that remains
        // safe. Both evidence paths stay in place regardless.
        if observe_in_directory(&step.candidate)?.is_none() {
            let _ = move_no_replace(
                directory,
                &quarantined,
                &step.candidate,
                "恢复发生变化的事实候选",
            );
            let _ = sync_publication(directory, "无法同步恢复的事实候选目录");
        }
        return Err(conflict(
            "部分候选在保全期间发生变化；候选证据已保留，未改动目标或移出版本。",
        ));
    }
    Ok(())
}

fn matches_expected_before(step: &RawFactStep) -> Result<bool, StorageError> {
    Ok(observe_in_directory(&step.target)? == step.before)
}

fn unique_partial_artifact(step: &RawFactStep, kind: &str) -> PathBuf {
    let parent = step.target.parent().expect("validated target parent");
    parent.join(format!(
        ".cuenote-restore-partial-{kind}-{}.tmp",
        Uuid::new_v4().simple()
    ))
}

fn copy_candidate_to_evidence(
    step: &RawFactStep,
    directory: &StableDirectory,
    evidence: &Path,
    expected: &FactDigest,
) -> Result<(), StorageError> {
    let mut input = open_local_read(&step.candidate)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(evidence)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => conflict("部分候选证据路径已被占用；未覆盖该文件。"),
            _ => StorageError::io("无法无覆盖创建部分候选证据", error),
        })?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| StorageError::io("无法读取部分事实候选", error))?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        if length > MAX_PACKAGE_BYTES {
            return Err(conflict("部分候选在保全期间超过事实文件大小上限。"));
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|error| StorageError::io("无法写入部分候选证据", error))?;
    }
    output
        .sync_all()
        .map_err(|error| StorageError::io("无法同步部分候选证据", error))?;
    let copied = FactDigest {
        sha256: format!("{:x}", hash.finalize()),
        length,
    };
    sync_publication(directory, "无法同步部分候选证据目录")?;
    if copied != *expected {
        return Err(conflict(
            "候选在保全过程中发生变化；原候选和证据副本均已保留。",
        ));
    }
    Ok(())
}

fn publish_ready_step(step: &RawFactStep, directory: &StableDirectory) -> Result<(), StorageError> {
    let has_after = step.after.is_some();
    if has_after {
        verify_digest_at(&step.candidate, step.after.as_ref().expect("checked after"))?;
        crate::storage_faults::checkpoint(
            "library_fact_candidate_ready",
            &step.target,
            Some(&step.candidate),
        )?;
    }

    let checkpoint_artifact = if has_after {
        &step.candidate
    } else {
        &step.displaced
    };
    crate::storage_faults::checkpoint(
        "library_fact_before_publish",
        &step.target,
        Some(checkpoint_artifact),
    )?;
    directory.ensure_current()?;

    match (&step.before, &step.after) {
        (None, Some(_)) => {
            move_no_replace(directory, &step.candidate, &step.target, "创建恢复事实")?;
            sync_publication(directory, "无法同步新建恢复事实目录")?;
            crate::storage_faults::checkpoint(
                "library_fact_after_publish",
                &step.target,
                Some(&step.candidate),
            )?;
        }
        (Some(_), None) => {
            move_no_replace(
                directory,
                &step.target,
                &step.displaced,
                "保留被删除的旧事实",
            )?;
            sync_publication(directory, "无法同步已移出事实目录")?;
            crate::storage_faults::checkpoint(
                "library_fact_after_publish",
                &step.target,
                Some(&step.displaced),
            )?;
        }
        (Some(_), Some(_)) => replace_fact(step, directory)?,
        (None, None) => unreachable!("validated fact step has at least one version"),
    }

    ensure_complete(step, directory)
}

fn replace_fact(step: &RawFactStep, directory: &StableDirectory) -> Result<(), StorageError> {
    #[cfg(windows)]
    {
        if let Err(error) = replace_windows(&step.target, &step.candidate, &step.displaced) {
            return Err(publish_io_error(
                "无法原子替换整库事实；目标、候选和移出工件均已保留",
                error,
                step,
            ));
        }
        sync_publication(directory, "无法同步整库事实替换目录")?;
        crate::storage_faults::checkpoint(
            "library_fact_after_publish",
            &step.target,
            Some(&step.displaced),
        )?;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if let Err(error) = directory.swap(&step.candidate, &step.target) {
            return Err(publish_io_error(
                "无法确认整库事实原子交换；目标、候选和移出工件均已保留",
                error,
                step,
            ));
        }
        sync_publication(directory, "无法同步整库事实原子交换目录")?;
        crate::storage_faults::checkpoint(
            "library_fact_after_publish",
            &step.target,
            Some(&step.candidate),
        )?;
        move_no_replace(
            directory,
            &step.candidate,
            &step.displaced,
            "保留被替换的旧事实",
        )?;
        sync_publication(directory, "无法同步已移出事实目录")?;
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = directory;
        return Err(StorageError::new(
            "publish",
            "当前平台没有实现整库事实安全原子替换。",
        ));
    }
    Ok(())
}

fn finish_displacement(
    step: &RawFactStep,
    directory: &StableDirectory,
) -> Result<(), StorageError> {
    crate::storage_faults::checkpoint(
        "library_fact_before_publish",
        &step.target,
        Some(&step.candidate),
    )?;
    directory.ensure_current()?;
    move_no_replace(
        directory,
        &step.candidate,
        &step.displaced,
        "保留被替换的旧事实",
    )?;
    sync_publication(directory, "无法同步已移出事实目录")?;
    ensure_complete(step, directory)
}

fn finish_partial_create(
    step: &RawFactStep,
    directory: &StableDirectory,
) -> Result<(), StorageError> {
    verify_digest_at(&step.candidate, step.after.as_ref().expect("checked after"))?;
    crate::storage_faults::checkpoint(
        "library_fact_candidate_ready",
        &step.target,
        Some(&step.candidate),
    )?;
    crate::storage_faults::checkpoint(
        "library_fact_before_publish",
        &step.target,
        Some(&step.candidate),
    )?;
    directory.ensure_current()?;
    move_no_replace(directory, &step.candidate, &step.target, "继续发布恢复事实")?;
    sync_publication(directory, "无法同步继续发布的恢复事实目录")?;
    crate::storage_faults::checkpoint(
        "library_fact_after_publish",
        &step.target,
        Some(&step.displaced),
    )?;
    ensure_complete(step, directory)
}

fn ensure_complete(step: &RawFactStep, directory: &StableDirectory) -> Result<(), StorageError> {
    match assess_step(step, directory)? {
        StepState::Complete => Ok(()),
        _ => Err(conflict(
            "整库事实发布后的实际版本尚未达到预期；目标和所有工件均已保留。",
        )),
    }
}

fn verify_digest_at(path: &Path, expected: &FactDigest) -> Result<(), StorageError> {
    match observe_fact(path)? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) => Err(conflict("事实候选内容与预期恢复版本不一致；未发布该文件。")),
        None => Err(conflict("事实候选文件缺失；未发布目标。")),
    }
}

fn move_no_replace(
    directory: &StableDirectory,
    source: &Path,
    destination: &Path,
    context: &str,
) -> Result<(), StorageError> {
    directory
        .move_no_replace(source, destination)
        .map_err(|error| {
            if matches!(error.kind(), io::ErrorKind::AlreadyExists)
                || matches!(error.raw_os_error(), Some(80 | 183))
            {
                conflict(&format!("{context}时目标工件已存在；未覆盖已有文件。"))
            } else {
                StorageError::io(context, error)
            }
        })
}

fn sync_publication(directory: &StableDirectory, context: &str) -> Result<(), StorageError> {
    directory
        .sync_publication_directory()
        .map_err(|error| StorageError::io(context, error))
}

fn observe_in_directory(path: &Path) -> Result<Option<FactDigest>, StorageError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::io("无法检查整库事实文件", error)),
        Ok(_) => {
            ensure_local_file(path)?;
            let file = open_local_read(path)?;
            digest_file(file, path).map(Some)
        }
    }
}

fn digest_file(file: File, path: &Path) -> Result<FactDigest, StorageError> {
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io("无法读取整库事实文件信息", error))?;
    if metadata.len() > MAX_PACKAGE_BYTES {
        return Err(invalid_step("整库事实文件超过大小上限。"));
    }
    let mut input = file.take(MAX_PACKAGE_BYTES + 1);
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| StorageError::io("无法读取整库事实文件", error))?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        if length > MAX_PACKAGE_BYTES {
            return Err(invalid_step("整库事实文件超过大小上限。"));
        }
        hash.update(&buffer[..count]);
    }
    let digest = FactDigest {
        sha256: format!("{:x}", hash.finalize()),
        length,
    };
    if metadata.len() != digest.length {
        return Err(conflict(&format!(
            "整库事实文件读取期间长度变化：{}。",
            path.display()
        )));
    }
    Ok(digest)
}

fn invalid_step(message: &str) -> StorageError {
    StorageError::new("invalidRestoreFact", message)
}

fn conflict(message: &str) -> StorageError {
    StorageError::new("conflict", message)
}

fn publish_io_error(context: &str, error: io::Error, step: &RawFactStep) -> StorageError {
    StorageError::new(
        "publish",
        format!(
            "{context}：{error}。候选保留于 {}，移出版本保留于 {}。",
            step.candidate.display(),
            step.displaced.display()
        ),
    )
}
