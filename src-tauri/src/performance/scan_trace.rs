//! Bounded, opt-in diagnostics; never part of desktop acceptance timing.
use serde::Serialize;
use std::path::Path;
use std::sync::{atomic::AtomicU64, atomic::Ordering, Arc, Mutex};
use std::time::Instant;

const LIMIT: usize = 2048;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct ScanTrace {
    id: u64,
    host_operation_id: u64,
    cause: String,
    entries: usize,
    workers: usize,
    start: Instant,
    data: Mutex<Data>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    active: usize,
    completed: usize,
    consumption_attempts: usize,
    omitted_jobs: usize,
    omitted_waits: usize,
    jobs: Vec<Job>,
    waits: Vec<Wait>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    index: usize,
    start_ms: f64,
    end_ms: f64,
    source_file_bytes: Option<u64>,
    has_candidate: bool,
    source_ms: Option<f64>,
    source_matches: Option<bool>,
    witness_ms: Option<f64>,
    hit: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Wait {
    index: usize,
    start_ms: f64,
    elapsed_ms: f64,
    next_scheduled: usize,
    pending_before: usize,
    active_before: usize,
    completed_before: usize,
    active_after: usize,
    completed_after: usize,
}

pub(crate) struct JobTrace {
    scan: Arc<ScanTrace>,
    last: Instant,
    job: Job,
}

pub(crate) struct WaitTrace {
    scan: Arc<ScanTrace>,
    start: Instant,
    wait: Wait,
}

impl ScanTrace {
    #[track_caller]
    pub(crate) fn new(entries: usize, workers: usize) -> Option<Arc<Self>> {
        if !super::trace_enabled() {
            return None;
        }
        let caller = std::panic::Location::caller();
        Some(Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            host_operation_id: super::host_trace_id(),
            cause: format!("{}:{}", caller.file(), caller.line()),
            entries,
            workers,
            start: Instant::now(),
            data: Mutex::new(Data::default()),
        }))
    }

    fn elapsed_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    pub(crate) fn job(
        self: &Arc<Self>,
        index: usize,
        path: &Path,
        has_candidate: bool,
    ) -> JobTrace {
        if let Ok(mut data) = self.data.lock() {
            data.active += 1;
        }
        let start_ms = self.elapsed_ms();
        // Metadata size is diagnostic only, not a byte-count or cache authority.
        let source_file_bytes = std::fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            .map(|metadata| metadata.len());
        JobTrace {
            scan: Arc::clone(self),
            last: Instant::now(),
            job: Job {
                index,
                start_ms,
                end_ms: 0.0,
                source_file_bytes,
                has_candidate,
                source_ms: None,
                source_matches: None,
                witness_ms: None,
                hit: false,
            },
        }
    }

    pub(crate) fn wait(self: &Arc<Self>, index: usize, next: usize, pending: usize) -> WaitTrace {
        let (active, completed) = self
            .data
            .lock()
            .map(|data| (data.active, data.completed))
            .unwrap_or_default();
        WaitTrace {
            scan: Arc::clone(self),
            start: Instant::now(),
            wait: Wait {
                index,
                start_ms: self.elapsed_ms(),
                elapsed_ms: 0.0,
                next_scheduled: next,
                pending_before: pending,
                active_before: active,
                completed_before: completed,
                active_after: 0,
                completed_after: 0,
            },
        }
    }

    pub(crate) fn emit(&self) {
        if let Ok(data) = self.data.lock() {
            eprintln!(
                "{}",
                serde_json::json!({
                    "diagnostic":"cache-scan", "id":self.id, "cause":self.cause,
                    "hostOperationId":self.host_operation_id,
                    "entries":self.entries, "workersRequested":self.workers,
                    "elapsedMs":self.elapsed_ms(), "data":*data,
                })
            );
        }
    }
}

impl JobTrace {
    pub(crate) fn source(&mut self, matched: bool) {
        self.job.source_ms = Some(self.last.elapsed().as_secs_f64() * 1000.0);
        self.job.source_matches = Some(matched);
        self.last = Instant::now();
    }

    pub(crate) fn witness(&mut self) {
        self.job.witness_ms = Some(self.last.elapsed().as_secs_f64() * 1000.0);
    }

    pub(crate) fn finish(mut self, hit: bool) {
        self.job.hit = hit;
        self.job.end_ms = self.scan.elapsed_ms();
        if let Ok(mut data) = self.scan.data.lock() {
            data.active = data.active.saturating_sub(1);
            data.completed += 1;
            if data.jobs.len() < LIMIT {
                data.jobs.push(self.job);
            } else {
                data.omitted_jobs += 1;
            }
        }
    }
}

impl Drop for WaitTrace {
    fn drop(&mut self) {
        self.wait.elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut data) = self.scan.data.lock() {
            data.consumption_attempts += 1;
            self.wait.active_after = data.active;
            self.wait.completed_after = data.completed;
            if data.waits.len() < LIMIT {
                data.waits.push(self.wait.clone());
            } else {
                data.omitted_waits += 1;
            }
        }
    }
}
