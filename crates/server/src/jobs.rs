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

use camerata_liveness::LivenessTracker;

use crate::onboard::{Finding, ScanReport};

/// Per-job activity metadata for stall detection and cancel signalling.
#[derive(Debug, Clone)]
pub struct JobMeta {
    /// Liveness tracker: wraps the epoch-ms timestamp + optional progress label.
    /// Replaces the previous bare `last_activity_ms: u128` field.
    pub tracker: LivenessTracker,
    /// Set to `true` when a cancel has been requested; checked by the job worker.
    pub cancel_requested: Arc<std::sync::atomic::AtomicBool>,
}

/// The lifecycle status of one deterministic scan tool, mirroring how the AI passes stream
/// `running` → `done` into the transcript. Stable wire strings so the UI can style each state.
pub mod det_status {
    /// Queued but not started.
    pub const STARTING: &str = "starting";
    /// The tool is executing.
    pub const RUNNING: &str = "running";
    /// The tool finished (findings counted).
    pub const DONE: &str = "done";
}

/// One deterministic-scan tool's live progress: which tool, its lifecycle status, and how
/// many findings it has produced so far. The "floor" (the always-on security scanner) is one
/// such entry; each scan-preview tool (clippy/ruff/eslint/semgrep) is another. Keyed by
/// `tool` so a status/findings update locates the right row.
#[derive(Clone, Debug, Serialize, Default, PartialEq)]
pub struct DetToolProgress {
    /// The tool name (`floor`, `clippy`, `ruff`, `eslint`, `semgrep`, …).
    pub tool: String,
    /// `starting` | `running` | `done` (see [`det_status`]).
    pub status: String,
    /// Findings this tool has produced so far.
    pub findings: usize,
}

/// The deterministic pass's overall progress: every tool's per-row state plus an aggregate
/// done/total so the UI can render a single bar AND the per-tool breakdown. Mirrors the AI
/// passes' `done`/`total` shape so the cockpit renders both consistently.
#[derive(Clone, Debug, Serialize, Default, PartialEq)]
pub struct DetProgress {
    /// Per-tool rows in registration order (floor first, then each preview tool).
    pub tools: Vec<DetToolProgress>,
    /// Tools finished (`status == done`).
    pub done: usize,
    /// Tools known so far (grows as the floor + each preview tool registers).
    pub total: usize,
}

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
    /// Live progress of the DETERMINISTIC pass (the always-on floor + the scan-preview
    /// tools) — per-tool status + findings + an overall done/total. Separate from the AI
    /// `done`/`total` above so the UI can show a "Deterministic scan" progress view even in
    /// deterministic-only mode (where the AI agent drawer is empty).
    #[serde(default)]
    pub deterministic: DetProgress,
    /// The final, authoritative report once `status == "done"`.
    pub report: Option<ScanReport>,
    /// A human note (e.g. the failure reason).
    pub message: Option<String>,
    /// Batch mode (#61): the Anthropic Message Batch id currently being processed
    /// (`msgbatch_01...`). Set when the batch scan mode submits a batch; cleared on
    /// completion. The UI can surface this for status/debugging ("batch in flight:
    /// msgbatch_01AbCd"). `None` for parallel/sequential mode jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

/// In-memory job store, shared into handlers + the background worker.
#[derive(Clone, Default)]
pub struct JobStore {
    inner: Arc<Mutex<HashMap<String, JobState>>>,
    counter: Arc<AtomicU64>,
    job_meta: Arc<Mutex<HashMap<String, JobMeta>>>,
}

impl JobStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicU64::new(0)),
            job_meta: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a fresh `running` job and return its id.
    pub fn create(&self, _label: &str) -> String {
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
        let meta = JobMeta {
            // LivenessTracker::new() initialises to current wall clock (not stalled).
            tracker: LivenessTracker::new(),
            cancel_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        if let Ok(mut m) = self.job_meta.lock() {
            m.insert(id.clone(), meta);
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

    /// Update the job's last-activity timestamp to now. Called by `det_tool_running`,
    /// `det_tool_done`, and the streaming scan-tool path (per stdout line and per mtime
    /// advance) so idle time stays low while a tool is actively running.
    pub(crate) fn touch_activity(&self, id: &str) {
        if let Some(meta) = self.job_meta.lock().unwrap().get(id) {
            meta.tracker.tick();
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

    /// Register a deterministic tool that is about to run (status `starting`), growing the
    /// deterministic `total`. Idempotent on `tool`: registering an ALREADY-KNOWN tool is a
    /// no-op (it neither double-counts the total NOR resets an in-flight/done status — so a
    /// later `det_tool_done` still sees its true prior state). Called as the floor and each
    /// preview tool come into scope, so the per-tool list + denominator build up live.
    pub fn det_register_tool(&self, id: &str, tool: &str) {
        self.with(id, |j| {
            if j.deterministic.tools.iter().any(|t| t.tool == tool) {
                return;
            }
            j.deterministic.tools.push(DetToolProgress {
                tool: tool.to_string(),
                status: det_status::STARTING.to_string(),
                findings: 0,
            });
            j.deterministic.total += 1;
        });
    }

    /// Mark a deterministic tool as `running`. Registers it first if unseen (so a caller can
    /// skip the explicit register step). Does not change the done count.
    pub fn det_tool_running(&self, id: &str, tool: &str) {
        self.det_register_tool(id, tool);
        self.with(id, |j| {
            if let Some(t) = j.deterministic.tools.iter_mut().find(|t| t.tool == tool) {
                t.status = det_status::RUNNING.to_string();
            }
        });
        self.touch_activity(id);
    }

    /// Mark a deterministic tool `done` with its final findings count, incrementing the
    /// deterministic `done` aggregate once (a re-finish of an already-done tool is a no-op on
    /// the aggregate). Registers the tool first if unseen.
    pub fn det_tool_done(&self, id: &str, tool: &str, findings: usize) {
        self.det_register_tool(id, tool);
        self.with(id, |j| {
            if let Some(t) = j.deterministic.tools.iter_mut().find(|t| t.tool == tool) {
                let was_done = t.status == det_status::DONE;
                t.status = det_status::DONE.to_string();
                t.findings = findings;
                if !was_done {
                    j.deterministic.done += 1;
                }
            }
        });
        self.touch_activity(id);
    }

    /// Pre-declare the COMPLETE set of deterministic tools the scan will run, all at once,
    /// before any of them starts executing.  This is the "N" in the "1/N tools" progress
    /// display: by registering every tool upfront the UI shows the true pipeline size from
    /// the very first poll rather than growing the denominator one tool at a time.
    ///
    /// Each tool is registered with status `starting`; subsequent `det_tool_running` /
    /// `det_tool_done` calls update them in place.  Idempotent: already-known tools are
    /// skipped (their existing status/findings are not reset).
    pub fn declare_tools(&self, id: &str, tools: &[&str]) {
        for tool in tools {
            self.det_register_tool(id, tool);
        }
    }

    /// Snapshot the deterministic progress (test/poll helper).
    #[must_use]
    pub fn det_progress(&self, id: &str) -> Option<DetProgress> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.get(id).map(|j| j.deterministic.clone()))
    }

    /// Record the Anthropic Message Batch id on the job. Called by the batch scan mode
    /// immediately after `Llm::submit_batch` succeeds so the UI can display the batch id
    /// in the status line. Cleared by `finish` (the batch completed and the id is no
    /// longer informative).
    pub fn set_batch_id(&self, id: &str, batch_id: impl Into<String>) {
        self.with(id, |j| j.batch_id = Some(batch_id.into()));
    }

    /// Request cancellation of a job by setting its cancel flag. The background worker is
    /// expected to check `is_cancel_requested` periodically and stop early.
    pub fn request_cancel(&self, id: &str) {
        if let Some(meta) = self.job_meta.lock().unwrap().get(id) {
            meta.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Return `true` when a cancel has been requested for this job.
    pub fn is_cancel_requested(&self, id: &str) -> bool {
        self.job_meta
            .lock()
            .unwrap()
            .get(id)
            .map(|m| m.cancel_requested.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// How many milliseconds has this job been idle (no tool activity) relative to `now_ms`?
    /// Returns `None` for an unknown job id.
    ///
    /// The signature accepts and returns `u128` for backwards-compatibility with the HTTP
    /// polling endpoint. Internally delegates to `LivenessTracker::idle_ms` (which uses `u64`
    /// — safe for all practical wall-clock values, saturating past year 584 million).
    pub fn idle_ms(&self, id: &str, now_ms: u128) -> Option<u128> {
        self.job_meta
            .lock()
            .unwrap()
            .get(id)
            .map(|m| u128::from(m.tracker.idle_ms(now_ms.try_into().unwrap_or(u64::MAX))))
    }

    /// Cancel a job: set the cancel flag and update the job status to `"cancelled"`.
    pub fn cancel(&self, id: &str) {
        self.request_cancel(id);
        self.with(id, |job| {
            job.status = "cancelled".to_string();
        });
    }

    /// Finish the job with the authoritative report.
    pub fn finish(&self, id: &str, report: ScanReport) {
        self.with(id, |j| {
            j.status = "done".to_string();
            j.report = Some(report);
            j.batch_id = None; // batch completed — id no longer informative
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

    /// Return the deep-tier report from the most recently COMPLETED job that
    /// had a non-None `report.deep` field. Used by the deep-report export
    /// endpoint to find the last successful deep audit without needing the job
    /// id. Returns `None` when no completed job has a deep report yet.
    #[must_use]
    pub fn latest_deep_report(&self) -> Option<crate::ai_audit::DeepReport> {
        let guard = self.inner.lock().ok()?;
        guard
            .values()
            .filter_map(|j| j.report.as_ref()?.deep.clone())
            .next()
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
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        }
    }

    #[test]
    fn lifecycle_create_progress_findings_finish() {
        let store = JobStore::new();
        let id = store.create("audit");
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
        let a = store.create("audit");
        let b = store.create("audit");
        assert_ne!(a, b);
        store.fail(&a, "no token");
        assert_eq!(store.get(&a).unwrap().status, "failed");
        assert_eq!(store.get(&a).unwrap().message.as_deref(), Some("no token"));
        assert!(store.get("job-nope").is_none());
    }

    /// The deterministic-progress model: registering tools grows `total`, a start→done
    /// transition increments `done` exactly once, and findings counts are recorded per tool.
    #[test]
    fn deterministic_progress_lifecycle() {
        let store = JobStore::new();
        let id = store.create("audit");

        // Nothing yet.
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (0, 0));
        assert!(p.tools.is_empty());

        // The floor registers + runs + finishes.
        store.det_tool_running(&id, "floor");
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (0, 1), "running grows total, not done");
        assert_eq!(p.tools[0].tool, "floor");
        assert_eq!(p.tools[0].status, det_status::RUNNING);

        store.det_tool_done(&id, "floor", 3);
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (1, 1), "done increments once");
        assert_eq!(p.tools[0].status, det_status::DONE);
        assert_eq!(p.tools[0].findings, 3);

        // A second tool: register (starting) then done.
        store.det_register_tool(&id, "clippy");
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (1, 2));
        assert_eq!(p.tools[1].status, det_status::STARTING);

        store.det_tool_done(&id, "clippy", 0);
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (2, 2));

        // Re-finishing an already-done tool must not double-count.
        store.det_tool_done(&id, "clippy", 5);
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (2, 2), "re-finish is idempotent on done");
        assert_eq!(p.tools[1].findings, 5);
    }

    /// The deterministic progress serializes onto the job state with its `tools`/`done`/
    /// `total` shape (the wire contract the UI's poll deserializes).
    #[test]
    fn deterministic_progress_serializes() {
        let store = JobStore::new();
        let id = store.create("audit");
        store.det_tool_done(&id, "floor", 2);
        let js = store.get(&id).unwrap();
        let json = serde_json::to_string(&js).unwrap();
        assert!(json.contains("\"deterministic\""));
        assert!(json.contains("\"floor\""));
        assert!(json.contains("\"tools\""));
        // A round-trip back into a Value confirms the nested shape.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["deterministic"]["done"].as_u64(), Some(1));
        assert_eq!(v["deterministic"]["total"].as_u64(), Some(1));
        assert_eq!(v["deterministic"]["tools"][0]["tool"].as_str(), Some("floor"));
        assert_eq!(v["deterministic"]["tools"][0]["findings"].as_u64(), Some(2));
    }

    /// `set_batch_id` persists the batch id on the job, and `finish` clears it.
    #[test]
    fn batch_id_lifecycle() {
        let store = JobStore::new();
        let id = store.create("audit");

        // Initially absent.
        assert!(store.get(&id).unwrap().batch_id.is_none());

        // Set when a batch is submitted.
        store.set_batch_id(&id, "msgbatch_01AbCd");
        assert_eq!(
            store.get(&id).unwrap().batch_id.as_deref(),
            Some("msgbatch_01AbCd")
        );

        // Cleared on completion (batch id no longer informative once done).
        let report = ScanReport::gated(&["me/api".to_string()]);
        store.finish(&id, report);
        assert!(
            store.get(&id).unwrap().batch_id.is_none(),
            "batch_id cleared on finish"
        );
    }

    #[test]
    fn job_cancel_marks_done_and_sets_flag() {
        let store = JobStore::new();
        let id = store.create("audit");
        assert!(!store.is_cancel_requested(&id));
        store.cancel(&id);
        assert!(store.is_cancel_requested(&id));
        let job = store.get(&id).unwrap();
        assert_eq!(job.status, "cancelled");
    }

    #[test]
    fn idle_ms_returns_none_for_unknown_job() {
        let store = JobStore::new();
        assert!(store.idle_ms("nonexistent", 99999).is_none());
    }

    #[test]
    fn det_tool_running_touches_activity() {
        let store = JobStore::new();
        let id = store.create("audit");
        // Record activity time just before
        let before_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        // Small sleep to ensure time advances
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.det_tool_running(&id, "bash");
        let after_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let idle = store.idle_ms(&id, after_ms).unwrap();
        // idle should be very small (just updated)
        assert!(idle < after_ms - before_ms + 10, "idle should be near 0 after touch");
    }

    /// `declare_tools` pre-registers the full pipeline upfront so `total` reflects the true
    /// count BEFORE any tool starts executing.  Subsequent `det_tool_running` / `det_tool_done`
    /// calls must update in place without double-counting (idempotent re-registration).
    #[test]
    fn declare_tools_predeclares_full_pipeline() {
        let store = JobStore::new();
        let id = store.create("audit");

        // Declare the full pipeline: floor + two preview linters + dep-audit (4 tools).
        store.declare_tools(&id, &["floor", "clippy", "semgrep", "dep-audit"]);

        let p = store.det_progress(&id).unwrap();
        assert_eq!(p.total, 4, "total must equal the declared pipeline size");
        assert_eq!(p.done, 0, "no tool has run yet");
        assert_eq!(p.tools.len(), 4, "four tool rows");
        assert!(
            p.tools.iter().all(|t| t.status == det_status::STARTING),
            "all declared tools start in `starting` status"
        );

        // Now run the floor — it transitions starting → running → done.
        // The total must NOT grow (floor was already declared).
        store.det_tool_running(&id, "floor");
        let p = store.det_progress(&id).unwrap();
        assert_eq!(p.total, 4, "total unchanged after running a declared tool");

        store.det_tool_done(&id, "floor", 2);
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (1, 4), "one done, total still 4");

        // Run the rest.
        store.det_tool_done(&id, "clippy", 0);
        store.det_tool_done(&id, "semgrep", 1);
        store.det_tool_done(&id, "dep-audit", 3);
        let p = store.det_progress(&id).unwrap();
        assert_eq!((p.done, p.total), (4, 4), "all four done");
    }

    /// `declare_tools` is idempotent: re-declaring already-known tools (e.g. calling it
    /// twice) must not grow the total or reset their status.
    #[test]
    fn declare_tools_idempotent_on_known_tools() {
        let store = JobStore::new();
        let id = store.create("audit");

        store.declare_tools(&id, &["floor", "dep-audit"]);
        store.det_tool_done(&id, "floor", 1);

        // Re-declare the same set — total stays 2, floor stays done.
        store.declare_tools(&id, &["floor", "dep-audit"]);
        let p = store.det_progress(&id).unwrap();
        assert_eq!(p.total, 2, "re-declare must not grow total");
        assert_eq!(p.done, 1, "floor remains done after re-declare");
        assert_eq!(
            p.tools.iter().find(|t| t.tool == "floor").unwrap().status,
            det_status::DONE,
            "floor status must not be reset by re-declare"
        );
    }
}
