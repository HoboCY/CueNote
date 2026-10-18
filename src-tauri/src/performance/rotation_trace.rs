//! Counts and wall times for existing rotation checks. No retention authority.
use std::time::Instant;

pub(crate) struct RotationTrace {
    start: Instant,
    pub(crate) members: usize,
    pub(crate) eligible: usize,
    parsed: usize,
    parse_ms: f64,
    deletion_checks: usize,
    deletion_check_ms: f64,
    pub(crate) deleted: usize,
}

impl RotationTrace {
    pub(crate) fn new() -> Option<Self> {
        super::trace_enabled().then(|| Self {
            start: Instant::now(),
            members: 0,
            eligible: 0,
            parsed: 0,
            parse_ms: 0.0,
            deletion_checks: 0,
            deletion_check_ms: 0.0,
            deleted: 0,
        })
    }

    pub(crate) fn parsed(&mut self, start: Instant, deletion: bool) {
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if deletion {
            self.deletion_checks += 1;
            self.deletion_check_ms += elapsed;
        } else {
            self.parsed += 1;
            self.parse_ms += elapsed;
        }
    }
}

impl Drop for RotationTrace {
    fn drop(&mut self) {
        eprintln!(
            "{}",
            serde_json::json!({
                "diagnostic":"version-rotation", "elapsedMs":self.start.elapsed().as_secs_f64()*1000.0,
                "members":self.members, "fullParseAttempts":self.parsed, "fullParseMs":self.parse_ms,
                "eligible":self.eligible, "deletionChecks":self.deletion_checks,
                "deletionCheckMs":self.deletion_check_ms, "deleted":self.deleted,
            })
        );
    }
}
