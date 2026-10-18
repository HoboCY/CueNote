//! Foreground saves and image reads interrupt probes only at safe boundaries.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone, Default)]
pub(crate) struct ForegroundRequests(Arc<AtomicUsize>);

pub(crate) struct ForegroundIntent {
    requests: ForegroundRequests,
}

impl ForegroundRequests {
    pub(crate) fn begin_save(&self) -> ForegroundIntent {
        self.begin_request()
    }

    pub(crate) fn begin_image_read(&self) -> ForegroundIntent {
        self.begin_request()
    }

    fn begin_request(&self) -> ForegroundIntent {
        self.0.fetch_add(1, Ordering::AcqRel);
        ForegroundIntent {
            requests: self.clone(),
        }
    }

    pub(crate) fn probe(&self) -> ProbeAttempt {
        ProbeAttempt {
            requests: self.clone(),
            must_report: AtomicBool::new(false),
            #[cfg(test)]
            hook: None,
        }
    }
}

impl Drop for ForegroundIntent {
    fn drop(&mut self) {
        self.requests.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProbeCheckpoint {
    Start,
    MetadataPrepared,
    BeforeEntry,
    EntryVerified,
    NoteRetentionComplete,
    BeforeCommit,
}

pub(crate) struct ProbeAttempt {
    requests: ForegroundRequests,
    must_report: AtomicBool,
    #[cfg(test)]
    hook: Option<Arc<dyn Fn(ProbeCheckpoint) + Send + Sync>>,
}

impl ProbeAttempt {
    pub(super) fn should_yield(&self, _at: ProbeCheckpoint) -> bool {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(_at);
        }
        !self.must_report.load(Ordering::Acquire) && self.requests.0.load(Ordering::Acquire) != 0
    }

    /// Preserve the existing classification/visibility of protection warnings.
    pub(super) fn must_report(&self) {
        self.must_report.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn with_hook(
        mut self,
        hook: impl Fn(ProbeCheckpoint) + Send + Sync + 'static,
    ) -> Self {
        self.hook = Some(Arc::new(hook));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_save_intents_survive_independent_error_returns() {
        let saves = ForegroundRequests::default();
        let probe = saves.probe();
        assert!(!probe.should_yield(ProbeCheckpoint::Start));
        let first = saves.begin_save();
        let failing = || -> Result<(), ()> {
            let _second = saves.begin_save();
            assert!(probe.should_yield(ProbeCheckpoint::Start));
            Err(())
        };
        assert!(failing().is_err());
        assert!(probe.should_yield(ProbeCheckpoint::Start));
        drop(first);
        assert!(!probe.should_yield(ProbeCheckpoint::Start));
        let _next = saves.begin_save();
        probe.must_report();
        assert!(!probe.should_yield(ProbeCheckpoint::BeforeCommit));
        assert!(saves.probe().should_yield(ProbeCheckpoint::Start));
    }
}
