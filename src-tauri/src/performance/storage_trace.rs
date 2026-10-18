//! Bounded, opt-in storage timing. No paths, note text, or storage authority.
use std::{
    cell::Cell,
    io::Write,
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    sync::OnceLock,
    time::Instant,
};

const LIMIT: u64 = 8192;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();
thread_local! { static PARENT: Cell<u64> = const { Cell::new(0) }; }

pub(crate) struct StorageTrace(Option<Active>);

struct Active {
    id: u64,
    parent: u64,
    operation: &'static str,
    start: Instant,
    last: Instant,
    phases: Vec<(&'static str, f64)>,
    bytes: Option<usize>,
    succeeded: bool,
    phases_omitted: usize,
    // The parent chain belongs to the synchronous thread that entered it.
    _thread: PhantomData<Rc<()>>,
}

impl StorageTrace {
    pub(crate) fn new(operation: &'static str) -> Self {
        if !super::trace_enabled() {
            return Self(None);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed) + 1;
        if id > LIMIT {
            if id == LIMIT + 1 {
                emit(
                    &serde_json::json!({"diagnostic":"storage-trace-limit","limit":LIMIT,"complete":false}),
                );
            }
            return Self(None);
        }
        EPOCH.get_or_init(Instant::now);
        let now = Instant::now();
        let parent = PARENT.replace(id);
        Self(Some(Active {
            id,
            parent,
            operation,
            start: now,
            last: now,
            phases: Vec::with_capacity(12),
            bytes: None,
            succeeded: false,
            phases_omitted: 0,
            _thread: PhantomData,
        }))
    }

    pub(crate) fn mark(&mut self, phase: &'static str) {
        if let Some(active) = &mut self.0 {
            let now = Instant::now();
            if active.phases.len() < 16 {
                active.phases.push((
                    phase,
                    now.duration_since(active.last).as_secs_f64() * 1000.0,
                ));
            } else {
                active.phases_omitted += 1;
            }
            active.last = now;
        }
    }

    pub(crate) fn bytes(&mut self, bytes: usize) {
        if let Some(active) = &mut self.0 {
            active.bytes = Some(bytes);
        }
    }

    pub(crate) fn success(&mut self) {
        if let Some(active) = &mut self.0 {
            active.succeeded = true;
        }
    }
}

impl Drop for StorageTrace {
    fn drop(&mut self) {
        self.mark("return");
        if let Some(active) = &self.0 {
            PARENT.set(active.parent);
            let elapsed = active.start.elapsed().as_secs_f64() * 1000.0;
            let observed = NEXT_ID.load(Ordering::Relaxed);
            emit(&serde_json::json!({
                "diagnostic":"storage-phase", "id":active.id, "parent":active.parent,
                "operation":active.operation, "bytes":active.bytes,
                "succeeded":active.succeeded, "phases":active.phases,
                "startMs":active.start.saturating_duration_since(*EPOCH.get().expect("trace epoch")).as_secs_f64()*1000.0,
                "elapsedMs":elapsed, "phasesOmitted":active.phases_omitted,
                "observedAtEnd":observed, "admittedAtEnd":observed.min(LIMIT),
                "droppedAtEnd":observed.saturating_sub(LIMIT),
                "scope":"inclusive wall times; parents include nested diagnostic logging overhead"
            }));
        }
    }
}

fn emit(value: &serde_json::Value) {
    // Diagnostics must never turn a successful storage operation into a failure.
    if let Ok(mut bytes) = serde_json::to_vec(value) {
        bytes.push(b'\n');
        let _ = std::io::stderr().lock().write_all(&bytes);
    }
}
