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
    /// The findings the reply was about, for a stage whose item is one call about several
    /// findings. A resume settles the reply finding by finding (see [`BatchMember`]), so it does
    /// not depend on the next run building the same call. Empty for a stage whose item is one
    /// thing (stage 1's site), which a resume matches by `key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<BatchMember>,
    /// The provider batch this line was read from when a resume read a live batch's row (set
    /// only there). Every process names its rounds `r0..`, so the key alone does not say which
    /// batch a reply came from; a resume skips a live row only when this batch has already been
    /// read under this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

/// One finding a call asked about: which finding, and how the reply names it. A resume reads a
/// returned reply by these, not by the call's own key, so a batch that was live when the process
/// stopped reaches its findings whatever batches the resumed run would build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchMember {
    /// The finding's site key: what identifies it from one run of the audit to the next.
    pub key: String,
    /// The line the finding was at when it was asked. A site key tells apart two findings of the
    /// same rule and snippet in one file only by their order, so a key that now names a finding
    /// at another line is a different finding: a reply is never written into it.
    pub line: usize,
    /// The id the call gave the finding, which the reply's entry for it carries.
    pub tag: String,
    /// The round (or second try) the finding was asked in.
    pub round: String,
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
    /// For a stage whose items are calls about several findings: the findings each item (by
    /// `custom_id`) asked about. When present, a resume settles the batch's rows by these, never
    /// by the item key. Empty for a stage whose items are single things.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub members: BTreeMap<String, Vec<BatchMember>>,
    /// True once every row of the batch has been absorbed or recorded as failed. A batch that has
    /// not settled is polled again on resume, never submitted again.
    #[serde(default)]
    pub settled: bool,
    /// The submit nonce of the [`SubmitIntent`] written before this batch was submitted, so a
    /// resume can tell an intent whose batch was saved from one whose batch was not. `None` on
    /// a record written before intents existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

/// What the runner is about to submit, written BEFORE `submit` is called. A kill between the
/// provider accepting the batch and the batch's id being saved leaves an intent no
/// [`BatchRecord`] carries the nonce of: the resume reads that as a batch that may have been
/// submitted and orphaned (it cannot be polled or cancelled, because its id was never saved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitIntent {
    /// A random token for this one submission, copied onto the [`BatchRecord`] saved after it.
    pub nonce: String,
    /// The `custom_id` of each item in the submission, mapped to the item's key.
    pub items: BTreeMap<String, String>,
}

/// The checkpoint files of one stage.
#[derive(Debug, Clone)]
pub struct StageCheckpoint {
    absorbed: PathBuf,
    batches: PathBuf,
    intents: PathBuf,
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
            intents: run_dir.join(format!("{stem}.intents.json")),
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

    /// Every absorbed item in the order it was recorded (a later line for the same key replaces
    /// an earlier one and takes its place, except a reply read finding by finding, which is kept:
    /// every process names its rounds from `r0`, so two processes can record different replies
    /// under one key). A reply read finding by finding depends on the order the earlier process
    /// read them in: a finding a first reply left out is written by a later one.
    pub fn absorbed_in_order(&self) -> Vec<AbsorbedLine> {
        let Ok(text) = std::fs::read_to_string(&self.absorbed) else {
            return Vec::new();
        };
        let mut lines: Vec<AbsorbedLine> = Vec::new();
        for line in text.lines() {
            if let Ok(parsed) = serde_json::from_str::<AbsorbedLine>(line) {
                if parsed.members.is_empty() {
                    lines.retain(|l| l.key != parsed.key);
                }
                lines.push(parsed);
            }
        }
        lines
    }

    /// Append one absorbed item.
    pub fn record_absorbed(&self, line: &AbsorbedLine) -> std::io::Result<()> {
        if let Some(dir) = self.absorbed.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut json = serde_json::to_string(line).map_err(std::io::Error::other)?;
        json.push('\n');
        // A crash can leave the last line cut short, with no newline. Start on a line of our own,
        // or the new record is glued to the cut one and the pair reads as one broken line.
        if file_ends_mid_line(&self.absorbed) {
            json.insert(0, '\n');
        }
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
        crate::run_store::write_atomic(&self.batches, json.as_bytes())
    }

    /// Every submission intent still on file: the ones written before a `submit` and not yet
    /// cleared. Empty when there are none.
    pub fn intents(&self) -> Vec<SubmitIntent> {
        std::fs::read_to_string(&self.intents)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Replace the intent list (an empty list clears it). Written the way the batch list is.
    pub fn save_intents(&self, intents: &[SubmitIntent]) -> std::io::Result<()> {
        if let Some(dir) = self.intents.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(intents).map_err(std::io::Error::other)?;
        crate::run_store::write_atomic(&self.intents, json.as_bytes())
    }
}

/// Whether the file at `path` has bytes and its last byte is not a newline (an append was cut short).
fn file_ends_mid_line(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return false;
    };
    if len == 0 || file.seek(SeekFrom::End(-1)).is_err() {
        return false;
    }
    let mut last = [0u8; 1];
    file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// A fresh random token for one submission (see [`SubmitIntent::nonce`]).
pub fn new_submit_nonce() -> String {
    let mut bytes = [0u8; 12];
    // The OS random source cannot be unavailable on a supported platform; if it were, a token
    // from the clock still tells two submissions of one process apart.
    if getrandom::getrandom(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..12]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── The run's checkpoint folder, carried with the audit future ──────────────────────────────────
//
// The audit's stages are many calls deep and none of them knows which run it belongs to. The run's
// folder is therefore carried with the future that runs the audit, the way the run's delimiter seed
// is ([`crate::prompt_frame::with_seed`]): never global, so two runs in one process (two tests, two
// projects) cannot read each other's checkpoints.

tokio::task_local! {
    static RUN_CHECKPOINT_DIR: PathBuf;
}

/// The folder of a run's stage checkpoints: `checkpoints/` inside the run's own folder, so a run
/// that is deleted (a client's data being removed) takes its checkpoints, which hold model replies
/// derived from the client's code, with it.
pub fn checkpoint_dir_of(run_dir: &Path) -> PathBuf {
    run_dir.join("checkpoints")
}

/// Run `fut` with the run's checkpoint folder in scope (`None` runs it with no checkpoints).
pub async fn with_run_checkpoints<F: std::future::Future>(
    dir: Option<PathBuf>,
    fut: F,
) -> F::Output {
    match dir {
        Some(dir) => RUN_CHECKPOINT_DIR.scope(dir, fut).await,
        None => fut.await,
    }
}

/// The checkpoint key of one batch of a stage's round: the round, then a digest of the site keys
/// of the findings it covers. The same findings asked in a later round (a smaller batch, a
/// re-ask) have a key of their own, so a restart replays a round's finished batches and never
/// mistakes a later round for an earlier one.
pub fn batch_key<'a>(round: &str, site_keys: impl Iterator<Item = &'a str>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for key in site_keys {
        h.update((key.len() as u64).to_le_bytes());
        h.update(key.as_bytes());
    }
    let digest: String = h
        .finalize()
        .iter()
        .take(12)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{round}:{digest}")
}

/// The checkpoint of the stage named `stage` (a file stem: a stage on two repos uses two names)
/// in the run in scope, or `None` outside a run.
pub fn stage_checkpoint(stage: &str) -> Option<StageCheckpoint> {
    RUN_CHECKPOINT_DIR
        .try_with(|dir| StageCheckpoint::new(dir, stage))
        .ok()
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
            members: Vec::new(),
            batch_id: None,
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
    fn absorbed_in_order_keeps_recording_order_and_the_members_of_each_reply() {
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "detail");
        let mut first = line("r1:b", "first");
        first.members = vec![BatchMember {
            key: "site-1".to_string(),
            line: 7,
            tag: "aaaaaa".to_string(),
            round: "r1".to_string(),
        }];
        cp.record_absorbed(&first).unwrap();
        cp.record_absorbed(&line("r0:a", "zero")).unwrap();
        cp.record_absorbed(&line("r1:b", "again")).unwrap();
        let keys: Vec<String> = cp.absorbed_in_order().into_iter().map(|l| l.key).collect();
        assert_eq!(
            keys,
            ["r0:a", "r1:b"],
            "a later line takes the place of an earlier one"
        );
        // The members survive the file; a line written without them reads back with none.
        cp.record_absorbed(&first).unwrap();
        let back = cp.absorbed_in_order();
        assert_eq!(back.len(), 3);
        assert_eq!(back.last().unwrap().members, first.members);
        assert!(back[0].members.is_empty());
    }

    #[test]
    fn replies_of_two_processes_under_one_round_key_are_both_kept_in_order() {
        // Every process names its rounds from r0, so two processes can record a reply for the
        // same key about the same findings. Both are real replies a resume must read in order.
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "detail");
        let member = BatchMember {
            key: "site-1".to_string(),
            line: 7,
            tag: "aaaaaa".to_string(),
            round: "r0".to_string(),
        };
        for text in ["process one", "process two"] {
            let mut l = line("r0:a", text);
            l.members = vec![member.clone()];
            cp.record_absorbed(&l).unwrap();
        }
        let texts: Vec<String> = cp.absorbed_in_order().into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["process one", "process two"]);
    }

    #[test]
    fn a_checkpoint_line_written_before_members_and_batch_ids_existed_still_loads() {
        let old = concat!(
            r#"{"key":"site-1","model":"m","backend":"api","cost_usd":0.5,"input_tokens":10,"#,
            r#""output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"#,
            r#""text":"reply"}"#,
            "\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "calibration");
        std::fs::write(dir.path().join("calibration.jsonl"), old).unwrap();
        let got = cp.absorbed_in_order();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, "site-1");
        assert_eq!(got[0].text, "reply");
        assert!(got[0].members.is_empty());
        assert_eq!(got[0].batch_id, None);
        assert_eq!(cp.absorbed().len(), 1);
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
            members: BTreeMap::new(),
            settled: false,
            nonce: Some("n1".to_string()),
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

    #[test]
    fn submit_intents_round_trip_and_clear_and_nonces_differ() {
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "calibration");
        assert!(cp.intents().is_empty());
        let intent = SubmitIntent {
            nonce: new_submit_nonce(),
            items: BTreeMap::from([("s1".to_string(), "site-1".to_string())]),
        };
        cp.save_intents(std::slice::from_ref(&intent)).unwrap();
        assert_eq!(cp.intents(), vec![intent]);
        cp.save_intents(&[]).unwrap();
        assert!(cp.intents().is_empty());
        assert_ne!(new_submit_nonce(), new_submit_nonce());
    }

    /// A crash that cuts an appended line short leaves a line with no newline. The next record
    /// must not be glued to it (the two would read as one broken line and BOTH be lost): it
    /// starts on its own line, the cut line is skipped, and the new record reads back.
    #[test]
    fn a_record_appended_after_a_torn_line_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "calibration");
        cp.record_absorbed(&line("site-1", "first")).unwrap();
        let path = dir.path().join("calibration.jsonl");
        let mut torn = std::fs::read(&path).unwrap();
        torn.extend_from_slice(br#"{"key":"site-2","model":"m","backe"#);
        std::fs::write(&path, torn).unwrap();

        cp.record_absorbed(&line("site-3", "third")).unwrap();

        let keys: Vec<String> = cp.absorbed().into_iter().map(|l| l.key).collect();
        assert_eq!(
            keys,
            ["site-1", "site-3"],
            "the cut line is skipped, the new one read"
        );
    }

    /// The batch and intent lists are written whole or not at all: a half-written list is never
    /// what a reader sees, and no temp file is left beside them.
    #[test]
    fn the_batch_list_is_written_through_the_atomic_writer() {
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "calibration");
        let record = BatchRecord {
            batch_id: "msgbatch_1".to_string(),
            ttl: "1h".to_string(),
            items: BTreeMap::new(),
            members: BTreeMap::new(),
            settled: false,
            nonce: None,
        };
        cp.save_batches(std::slice::from_ref(&record)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("calibration.batches.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "a run's files are private to the user");
        }
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_batch_record_written_before_nonces_existed_still_reads() {
        let old =
            r#"[{"batch_id":"msgbatch_1","ttl":"1h","items":{"s1":"site-1"},"settled":false}]"#;
        let dir = tempfile::tempdir().unwrap();
        let cp = StageCheckpoint::new(dir.path(), "calibration");
        std::fs::write(dir.path().join("calibration.batches.json"), old).unwrap();
        let records = cp.batches();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].nonce, None);
    }
}
