use super::*;
use std::sync::{atomic::AtomicBool, mpsc, Mutex};
use std::thread::JoinHandle;

// Only sixteen directory entries may be observed ahead of serial consumption.
// Up to eight workers stream bytes; native decoding stays on the scanner thread.
const WINDOW: usize = 16;

struct Job {
    index: usize,
    path: PathBuf,
    candidate: Option<Candidate>,
}

struct Candidate {
    revision: String,
    witness: storage::versions::ObservedVersionWitness,
    note_id: String,
    title: String,
}

pub(super) struct Checks<'a> {
    entries: &'a [fs::DirEntry],
    cache: &'a scan_cache::Cache,
    next: usize,
    input: Option<mpsc::SyncSender<Job>>,
    output: mpsc::Receiver<(usize, bool)>,
    pending: BTreeMap<usize, bool>,
    workers: Vec<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    #[cfg(feature = "perf-acceptance")]
    trace: Option<Arc<crate::performance::scan_trace::ScanTrace>>,
}

impl<'a> Checks<'a> {
    #[cfg_attr(feature = "perf-acceptance", track_caller)]
    pub fn new(
        entries: &'a [fs::DirEntry],
        cache: Option<&'a scan_cache::Cache>,
        recovery: &Path,
    ) -> Option<Self> {
        let cache = cache.filter(|cache| !cache.entries.is_empty())?;
        let count = std::thread::available_parallelism()
            .map_or(1, |count| count.get())
            .min(8)
            .min(entries.len());
        if count == 0 {
            return None;
        }
        let (input, receiver) = mpsc::sync_channel::<Job>(WINDOW);
        let receiver = Arc::new(Mutex::new(receiver));
        // Only WINDOW jobs are outstanding, so both this channel and pending
        // are bounded by WINDOW even while the head entry is slow.
        let (sender, output) = mpsc::channel();
        let mut workers = Vec::new();
        let cancelled = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "perf-acceptance")]
        let trace = crate::performance::scan_trace::ScanTrace::new(entries.len(), count);
        for _ in 0..count {
            let receiver = Arc::clone(&receiver);
            let sender = sender.clone();
            let recovery = recovery.to_path_buf();
            let cancelled = Arc::clone(&cancelled);
            #[cfg(feature = "perf-acceptance")]
            let trace = trace.clone();
            if let Ok(worker) = std::thread::Builder::new()
                .name("note-cache-check".into())
                .spawn(move || loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => break,
                    };
                    let Ok(job) = job else {
                        break;
                    };
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    #[cfg(feature = "perf-acceptance")]
                    let mut job_trace = trace
                        .as_ref()
                        .map(|trace| trace.job(job.index, &job.path, job.candidate.is_some()));
                    let hit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        job.candidate.as_ref().is_some_and(|candidate| {
                            let source_matches = storage::source_matches_cached_revision(
                                &job.path,
                                &candidate.revision,
                            )
                            .unwrap_or(false);
                            #[cfg(feature = "perf-acceptance")]
                            if let Some(trace) = &mut job_trace {
                                trace.source(source_matches);
                            }
                            if !source_matches {
                                return false;
                            }
                            let witness_matches =
                                storage::versions::verify_observed_version_witness(
                                    &recovery,
                                    &candidate.witness,
                                    &candidate.revision,
                                    &display_path(&job.path),
                                    &candidate.note_id,
                                    &candidate.title,
                                )
                                .unwrap_or(false);
                            #[cfg(feature = "perf-acceptance")]
                            if let Some(trace) = &mut job_trace {
                                trace.witness();
                            }
                            witness_matches
                        })
                    }))
                    .unwrap_or(false);
                    #[cfg(feature = "perf-acceptance")]
                    if let Some(trace) = job_trace {
                        trace.finish(hit);
                    }
                    if sender.send((job.index, hit)).is_err() {
                        break;
                    }
                })
            {
                workers.push(worker);
            }
        }
        drop(sender);
        if workers.is_empty() {
            return None;
        }
        let mut checks = Self {
            entries,
            cache,
            next: 0,
            input: Some(input),
            output,
            pending: BTreeMap::new(),
            workers,
            cancelled,
            #[cfg(feature = "perf-acceptance")]
            trace,
        };
        for _ in 0..WINDOW.min(entries.len()) {
            checks.schedule();
        }
        Some(checks)
    }

    fn schedule(&mut self) {
        let Some(entry) = self.entries.get(self.next) else {
            return;
        };
        let path = entry.path();
        let name = entry.file_name();
        let candidate = name
            .to_str()
            .and_then(|name| self.cache.entries.get(name))
            .filter(|candidate| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| {
                        is_canonical_uuid_v4(stem) && candidate.summary.note_id == stem
                    })
            })
            .map(|entry| Candidate {
                revision: entry.revision.clone(),
                witness: entry.witness.clone(),
                note_id: entry.summary.note_id.clone(),
                title: entry.summary.title.clone(),
            });
        let job = Job {
            index: self.next,
            path,
            candidate,
        };
        self.next += 1;
        if let Some(input) = &self.input {
            if input.send(job).is_err() {
                self.input = None;
            }
        }
    }

    // Called exactly once for each sorted directory entry, including skipped
    // extensions. Eligibility/duplicates are checked again by the scanner.
    pub fn verified(&mut self, index: usize) -> bool {
        #[cfg(feature = "perf-acceptance")]
        let _wait = self
            .trace
            .as_ref()
            .map(|trace| trace.wait(index, self.next, self.pending.len()));
        while !self.pending.contains_key(&index) {
            match self.output.recv() {
                Ok((index, hit)) => {
                    self.pending.insert(index, hit);
                }
                Err(_) => return false,
            }
        }
        let hit = self.pending.remove(&index).unwrap_or(false);
        self.schedule();
        hit
    }
}

impl Drop for Checks<'_> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        drop(self.input.take());
        // Errors/early returns still release all handles and shared leases.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        #[cfg(feature = "perf-acceptance")]
        if let Some(trace) = &self.trace {
            trace.emit();
        }
    }
}
