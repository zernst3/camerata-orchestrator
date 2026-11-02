//! Run checkpoints: the persisted, resumable state of a governed development run that PAUSED for
//! human review (e.g. the test-tamper guard), so the run can later RESUME by re-spawning the agent
//! from where it stopped instead of failing.
//!
//! A checkpoint captures the IDENTITY and run-specific state needed to continue: which story/UoW,
//! the worktree (repo + branch + dir) with the agent's partial work still on disk, the `base_commit`
//! the run diffs against (so a resumed run continues from the same baseline), the bounce-loop
//! `iteration` and `max_iterations` budget, and the `model`. Everything else (the decisions, the
//! grounding, the read dirs, the L3/integration bundles) is RE-DERIVED from the project + UoW + story
//! at resume time, exactly as a fresh run derives it, so the checkpoint stays small and durable.
//!
//! Persistence mirrors the other stores: in-memory `Arc<Mutex<Vec<_>>>`, optionally flushed to
//! `<data_dir>/camerata/checkpoints.json`. The pause survives an app restart because it is persisted
//! state, not a held thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// The Checkpoint / NewCheckpoint domain types now live in the framework-agnostic core
// (RUST-HEADLESS-CORE-1); re-exported so `crate::checkpoint::{Checkpoint, NewCheckpoint}` call sites
// are unchanged. The CheckpointStore below (Arc<Mutex> + JSON persistence) stays in this adapter.
pub use camerata_app_core::checkpoint::{Checkpoint, NewCheckpoint};

/// Checkpoint store. In-memory by default; [`at`](Self::at) persists to
/// `<data_dir>/camerata/checkpoints.json`. `Clone` is a shallow Arc handle for `AppState`.
#[derive(Clone, Default)]
pub struct CheckpointStore {
    items: Arc<Mutex<Vec<Checkpoint>>>,
    counter: Arc<AtomicUsize>,
    path: Option<Arc<PathBuf>>,
}

impl CheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn at(path: PathBuf) -> Self {
        let items: Vec<Checkpoint> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let max = items
            .iter()
            .filter_map(|c| c.id.strip_prefix("ckpt-"))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        Self {
            items: Arc::new(Mutex::new(items)),
            counter: Arc::new(AtomicUsize::new(max)),
            path: Some(Arc::new(path)),
        }
    }

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(items) = self.items.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*items) {
            let _ = std::fs::write(p.as_ref(), s);
        }
    }

    /// Create a checkpoint, assigning a fresh `ckpt-N` id + created timestamp. Returns it.
    pub fn create(&self, new: NewCheckpoint) -> Checkpoint {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let c = Checkpoint {
            id: format!("ckpt-{n}"),
            story_id: new.story_id,
            run_id: new.run_id,
            escalation_id: new.escalation_id,
            pause_reason: new.pause_reason,
            repo: new.repo,
            branch: new.branch,
            worktree_dir: new.worktree_dir,
            base_commit: new.base_commit,
            iteration: new.iteration,
            max_iterations: new.max_iterations,
            model: new.model,
            project_id: new.project_id,
            created: Self::now_rfc3339(),
            resumed: None,
        };
        if let Ok(mut g) = self.items.lock() {
            g.push(c.clone());
        }
        self.flush();
        c
    }

    pub fn get(&self, id: &str) -> Option<Checkpoint> {
        self.items.lock().ok()?.iter().find(|c| c.id == id).cloned()
    }

    pub fn list(&self) -> Vec<Checkpoint> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// The latest UN-resumed checkpoint for a story, if any (the one a resume would continue from).
    pub fn latest_open_for_story(&self, story_id: &str) -> Option<Checkpoint> {
        self.items
            .lock()
            .ok()?
            .iter()
            .rfind(|c| c.story_id == story_id && c.resumed.is_none())
            .cloned()
    }

    /// Stamp a checkpoint as resumed (so it is not consumed twice). Returns the updated record, or
    /// `None` for an unknown id or one already resumed.
    pub fn mark_resumed(&self, id: &str) -> Option<Checkpoint> {
        let mut guard = self.items.lock().ok()?;
        let c = guard
            .iter_mut()
            .find(|c| c.id == id && c.resumed.is_none())?;
        c.resumed = Some(Self::now_rfc3339());
        let updated = c.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }
}

// ── Stage checkpoints (design 6.2) ──────────────────────────────────────────────────────────────
//
// A post-scan stage run by the shared batch runner ([`crate::ai_audit::stage_batch`]) writes two
// files under `checkpoints/<run_id>/`:
//
// * `<stage>.jsonl`: one line per ABSORBED item, keyed by the item's key (its site key), with the
//   usage it was billed and the reply text it was absorbed from;
// * `<stage>.batches.json`: every batch the stage submitted, with the item keys it carries and
//   whether its rows have all been dealt with.
//
// A resume reads them: an absorbed item is not sent again (its usage is written to the meter, so
// the ledger is never a zero-token stage, and its reply is replayed into the stage unless the
// stage already holds it); a batch that has not settled is polled, not submitted again; only an
// item with no result and no live batch is sent. Both files are written as the work happens, so a
// crash loses at most the item being written.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One absorbed item: its key, what it was billed and the reply it was absorbed from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbsorbedLine {
    /// The item's key (a site key, a rule-group id): what the stage calls it.
    pub key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// The reply the item was absorbed from, so a resume can hand it to a stage that was rebuilt
    /// from nothing without paying for the item again.
    #[serde(default)]
    pub text: String,
}

/// One batch the stage submitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    pub batch_id: String,
    /// The prompt-cache TTL the batch was submitted under (`5m` or `1h`), which its results are
    /// priced at.
    pub ttl: String,
    /// The batch's `custom_id` for each item, mapped to the item's key.
    pub items: BTreeMap<String, String>,
    /// True once every row of the batch has been absorbed or recorded as failed. A batch that has
    /// not settled is polled again on resume, never submitted again.
    #[serde(default)]
    pub settled: bool,
}

/// The two checkpoint files of one stage.
#[derive(Debug, Clone)]
pub struct StageCheckpoint {
    absorbed: PathBuf,
    batches: PathBuf,
}

impl StageCheckpoint {
    /// The checkpoint of the stage named `stage` under `run_dir` (`checkpoints/<run_id>`). The
    /// name is a file stem: two stages, or one stage on two repos, use two names.
    pub fn new(run_dir: &Path, stage: &str) -> Self {
        let stem: String = stage
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Self {
            absorbed: run_dir.join(format!("{stem}.jsonl")),
            batches: run_dir.join(format!("{stem}.batches.json")),
        }
    }

    /// Every absorbed item recorded so far. A line that does not parse (a write cut short by a
    /// crash) is skipped; the item it named is simply sent again. A later line for the same key
    /// replaces an earlier one.
    pub fn absorbed(&self) -> Vec<AbsorbedLine> {
        let Ok(text) = std::fs::read_to_string(&self.absorbed) else {
            return Vec::new();
        };
        let mut by_key: BTreeMap<String, AbsorbedLine> = BTreeMap::new();
        for line in text.lines() {
            if let Ok(parsed) = serde_json::from_str::<AbsorbedLine>(line) {
                by_key.insert(parsed.key.clone(), parsed);
            }
        }
        by_key.into_values().collect()
    }

    /// Append one absorbed item.
    pub fn record_absorbed(&self, line: &AbsorbedLine) -> std::io::Result<()> {
        if let Some(dir) = self.absorbed.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut json = serde_json::to_string(line).map_err(std::io::Error::other)?;
        json.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.absorbed)?;
        file.write_all(json.as_bytes())?;
        file.flush()
    }

    /// Every batch recorded so far, oldest first.
    pub fn batches(&self) -> Vec<BatchRecord> {
        std::fs::read_to_string(&self.batches)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Replace the batch list. Written beside the file and renamed over it, so a crash leaves the
    /// old list or the new one and never half of one.
    pub fn save_batches(&self, batches: &[BatchRecord]) -> std::io::Result<()> {
        if let Some(dir) = self.batches.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(batches).map_err(std::io::Error::other)?;
        let tmp = self.batches.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.batches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(store: &CheckpointStore, story: &str) -> Checkpoint {
        store.create(NewCheckpoint {
            story_id: story.to_string(),
            run_id: "run-1".to_string(),
            escalation_id: "esc-1".to_string(),
            pause_reason: "test-tamper".to_string(),
            repo: "me/api".to_string(),
            branch: "camerata/me-api-1".to_string(),
            worktree_dir: "/tmp/wt".to_string(),
            base_commit: "abc123".to_string(),
            iteration: 0,
            max_iterations: 3,
            model: "claude-opus-5".to_string(),
            project_id: Some("proj-1".to_string()),
        })
    }

    #[test]
    fn create_assigns_id_and_get_finds_it() {
        let store = CheckpointStore::new();
        let c = mk(&store, "me/api#1");
        assert_eq!(c.id, "ckpt-1");
        assert!(c.resumed.is_none());
        assert_eq!(store.get("ckpt-1").unwrap().base_commit, "abc123");
        assert!(store.get("nope").is_none());
    }

    #[test]
    fn latest_open_for_story_and_mark_resumed() {
        let store = CheckpointStore::new();
        let _c1 = mk(&store, "me/api#1");
        let c2 = mk(&store, "me/api#1"); // a second pause on the same story
                                         // latest_open returns the most recent un-resumed one.
        assert_eq!(store.latest_open_for_story("me/api#1").unwrap().id, c2.id);
        // mark it resumed; it is no longer "open", and can't be resumed twice.
        assert!(store.mark_resumed(&c2.id).is_some());
        assert!(
            store.mark_resumed(&c2.id).is_none(),
            "already resumed -> None"
        );
        // now the earlier one is the latest open.
        assert_eq!(
            store.latest_open_for_story("me/api#1").unwrap().id,
            "ckpt-1"
        );
        assert!(store.latest_open_for_story("other").is_none());
    }

    fn line(key: &str, text: &str) -> AbsorbedLine {
        AbsorbedLine {
            key: key.to_string(),
            model: "m".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.5),
            input_tokens: Some(10),
            output_tokens: Some(2),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            text: text.to_string(),
        }
    }

    #[test]
    fn a_stage_checkpoint_skips_a_torn_line_and_the_last_line_for_a_key_wins() {
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "calibration");
        assert!(cp.absorbed().is_empty(), "no file yet");
        cp.record_absorbed(&line("site-1", "first")).unwrap();
        cp.record_absorbed(&line("site-2", "two")).unwrap();
        cp.record_absorbed(&line("site-1", "second")).unwrap();
        // A write cut short by a crash leaves half a line at the end.
        let path = dir.path().join("calibration.jsonl");
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("{\"key\":\"site-3\",\"text\":\"cut");
        std::fs::write(&path, raw).unwrap();

        let got = cp.absorbed();
        assert_eq!(got.len(), 2, "the torn line is skipped");
        let one = got.iter().find(|l| l.key == "site-1").unwrap();
        assert_eq!(one.text, "second");
        assert_eq!(one.cost_usd, Some(0.5));
    }

    #[test]
    fn stage_checkpoints_are_named_per_stage_and_the_batch_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let a = StageCheckpoint::new(dir.path(), "calibration");
        let b = StageCheckpoint::new(dir.path(), "fix/acme widgets");
        a.record_absorbed(&line("x", "a")).unwrap();
        assert!(b.absorbed().is_empty(), "another stage has its own file");
        assert!(dir.path().join("fix_acme_widgets.jsonl").parent().is_some());
        assert!(a.batches().is_empty());
        let record = BatchRecord {
            batch_id: "msgbatch_1".to_string(),
            ttl: "1h".to_string(),
            items: BTreeMap::from([("s1".to_string(), "site-1".to_string())]),
            settled: false,
        };
        a.save_batches(std::slice::from_ref(&record)).unwrap();
        assert_eq!(a.batches(), vec![record.clone()]);
        let settled = BatchRecord {
            settled: true,
            ..record
        };
        a.save_batches(std::slice::from_ref(&settled)).unwrap();
        assert_eq!(a.batches(), vec![settled]);
        assert!(
            !dir.path().join("calibration.batches.json.tmp").exists(),
            "the temp file was renamed away"
        );
    }
}
