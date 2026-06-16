//! Async audit JOBS — the delivery layer for scan Mode 3.
//!
//! A scan can take many minutes on a big multi-repo run; holding one HTTP request open that
//! long is fragile (proxy timeouts, a frozen-looking app, all-or-nothing loss). Instead the
//! audit runs in a background `tokio::spawn`, writing PROGRESS + incremental FINDINGS into
//! this store as it goes, and the UI polls the job by id. So the user can submit, walk away,
//! and watch findings stream in — and the run survives a dropped poll because the work is
//! decoupled from the request.
//!
//! In-memory and ephemeral (like the transcript store): a job lives for the app session. The
//! findings accumulated while running are a RAW preview (pre-final-dedup/calibration); the
//! `report` set on completion is the authoritative result the UI switches to.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::onboard::{Finding, ScanReport};

/// A live audit job's state, as the UI polls it.
#[derive(Clone, Debug, Serialize, Default)]
pub struct JobState {
    /// `running` | `done` | `failed`.
    pub status: String,
    /// Passes completed so far.
    pub done: usize,
    /// Total passes known so far (grows as repos are fetched + chunked).
    pub total: usize,
    /// Findings discovered so far — a live preview (pre-final calibration).
    pub findings: Vec<Finding>,
    /// The final, authoritative report once `status == "done"`.
    pub report: Option<ScanReport>,
    /// A human note (e.g. the failure reason).
    pub message: Option<String>,
}

/// In-memory job store, shared into handlers + the background worker.
#[derive(Clone, Default)]
pub struct JobStore {
    inner: Arc<Mutex<HashMap<String, JobState>>>,
    counter: Arc<AtomicU64>,
}

impl JobStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a fresh `running` job and return its id.
    pub fn create(&self) -> String {
        let id = format!("job-{}", self.counter.fetch_add(1, Ordering::Relaxed) + 1);
        if let Ok(mut g) = self.inner.lock() {
            g.insert(
                id.clone(),
                JobState {
                    status: "running".to_string(),
                    ..Default::default()
                },
            );
        }
        id
    }

    fn with<F: FnOnce(&mut JobState)>(&self, id: &str, f: F) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(j) = g.get_mut(id) {
                f(j);
            }
        }
    }

    /// Grow the known total pass count (repos are discovered + chunked incrementally, so the
    /// denominator climbs as the job learns how much work there is).
    pub fn add_total(&self, id: &str, n: usize) {
        self.with(id, |j| j.total += n);
    }

    /// Mark `n` passes complete.
    pub fn inc_done(&self, id: &str, n: usize) {
        self.with(id, |j| j.done += n);
    }

    /// Append findings discovered by a pass (live preview).
    pub fn add_findings(&self, id: &str, findings: Vec<Finding>) {
        self.with(id, |j| j.findings.extend(findings));
    }

    /// Finish the job with the authoritative report.
    pub fn finish(&self, id: &str, report: ScanReport) {
        self.with(id, |j| {
            j.status = "done".to_string();
            j.report = Some(report);
        });
    }

    /// Fail the job with a reason.
    pub fn fail(&self, id: &str, message: impl Into<String>) {
        self.with(id, |j| {
            j.status = "failed".to_string();
            j.message = Some(message.into());
        });
    }

    /// Snapshot a job's state (None for an unknown id).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<JobState> {
        self.inner.lock().ok().and_then(|g| g.get(id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(rule: &str) -> Finding {
        Finding {
            repo: "me/api".into(),
            path: "src/x.rs".into(),
            line: 1,
            rule_id: rule.into(),
            severity: "high".into(),
            snippet: "x".into(),
            detail: "d".into(),
            status: "active".into(),
        }
    }

    #[test]
    fn lifecycle_create_progress_findings_finish() {
        let store = JobStore::new();
        let id = store.create();
        assert_eq!(store.get(&id).unwrap().status, "running");

        store.add_total(&id, 4);
        store.inc_done(&id, 1);
        store.add_findings(&id, vec![finding("R1")]);
        store.inc_done(&id, 1);
        store.add_findings(&id, vec![finding("R2"), finding("R3")]);

        let j = store.get(&id).unwrap();
        assert_eq!((j.done, j.total), (2, 4));
        assert_eq!(j.findings.len(), 3);
        assert!(j.report.is_none());

        let report = ScanReport::gated(&["me/api".to_string()]);
        store.finish(&id, report);
        let j = store.get(&id).unwrap();
        assert_eq!(j.status, "done");
        assert!(j.report.is_some());
    }

    #[test]
    fn unique_ids_and_fail_path() {
        let store = JobStore::new();
        let a = store.create();
        let b = store.create();
        assert_ne!(a, b);
        store.fail(&a, "no token");
        assert_eq!(store.get(&a).unwrap().status, "failed");
        assert_eq!(store.get(&a).unwrap().message.as_deref(), Some("no token"));
        assert!(store.get("job-nope").is_none());
    }
}
