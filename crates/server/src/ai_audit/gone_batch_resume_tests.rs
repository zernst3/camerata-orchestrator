//! #356 through the stage runner and the real run-store journal: a batch whose results expired
//! before they were fetched no longer blocks every resume. The stage settles it as gone with its
//! note and its unanswered items go out once in a new batch; what was answered is replayed.
//!
//! A counting stub stands in for the provider. No live or paid model calls.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use camerata_llm::failure::{FailureClass, LlmCallFailure};

use super::stage_batch::{run_stage, ItemReject, StageCtx, StageItems, StageMode, StageSettings};
use super::*;
use crate::checkpoint::StageCheckpoint;
use crate::cost_cap::RunControls;
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};
use crate::run_store::{NewRun, RunOrigin, RunStore};

const REPO: &str = "acme/widgets";
const CREDIT_OUT: &str = "Your credit balance is too low to access the Anthropic API.";

/// No item here is ever sent in real time; a call is counted and refused.
struct NoRealtime {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for NoRealtime {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("no real-time call is expected in this test")
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The provider. Each submit gets the next id `msgbatch_g{n}`. A batch named in `unreachable`
/// cannot be polled (a transient 503, so the stage leaves it live); one named in `gone` answers
/// 404 on every poll and fetch (its results expired). While `credit_out` is set the row of
/// `item-1` is a credit error. `answered` lists every item a fetched row answered (and billed).
struct Provider {
    sent: Mutex<Vec<Vec<(String, String)>>>,
    unreachable: Mutex<BTreeSet<String>>,
    gone: Mutex<BTreeSet<String>>,
    credit_out: AtomicBool,
    answered: Mutex<Vec<String>>,
    polls_of_gone: AtomicUsize,
    /// While set, every batch submitted is added to `unreachable`.
    losing: AtomicBool,
    realtime: NoRealtime,
}

impl Provider {
    fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            unreachable: Mutex::new(BTreeSet::new()),
            gone: Mutex::new(BTreeSet::new()),
            credit_out: AtomicBool::new(false),
            answered: Mutex::new(Vec::new()),
            polls_of_gone: AtomicUsize::new(0),
            losing: AtomicBool::new(false),
            realtime: NoRealtime {
                calls: AtomicUsize::new(0),
            },
        }
    }
    /// The item names of each submitted batch, in order.
    fn sent_items(&self) -> Vec<Vec<String>> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|b| {
                let mut names: Vec<String> = b.iter().map(|(_, n)| n.clone()).collect();
                names.sort();
                names
            })
            .collect()
    }
    fn answered(&self) -> Vec<String> {
        let mut a = self.answered.lock().unwrap().clone();
        a.sort();
        a
    }
    /// Every batch submitted from now on cannot be reached: the process stops waiting on it.
    fn lose_contact(&self) {
        self.losing.store(true, Ordering::SeqCst);
    }
    /// The batches that could not be reached have since expired at the provider, and contact is
    /// back.
    fn expire_unreachable(&self) {
        self.losing.store(false, Ordering::SeqCst);
        let lost: Vec<String> = std::mem::take(&mut *self.unreachable.lock().unwrap())
            .into_iter()
            .collect();
        self.gone.lock().unwrap().extend(lost);
    }
    fn answer_for(&self, id: &str) -> Option<anyhow::Error> {
        if self.gone.lock().unwrap().contains(id) {
            self.polls_of_gone.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"type":"error","error":{"type":"not_found_error","message":"batch not found"}}"#;
            let mut f = LlmCallFailure::new(
                FailureClass::Content,
                format!("batch results HTTP 404 Not Found: {body}"),
            );
            f.http = Some((404, body.to_string()));
            return Some(anyhow::Error::new(f));
        }
        if self.unreachable.lock().unwrap().contains(id) {
            return Some(anyhow::anyhow!("Message Batches API HTTP 503: unavailable"));
        }
        None
    }
}

#[async_trait::async_trait]
impl BatchApi for Provider {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.realtime
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        let mut sent = self.sent.lock().unwrap();
        sent.push(
            items
                .into_iter()
                .map(|i| {
                    let text = i.params.messages[0].content.to_string();
                    let name = (0..4)
                        .map(|n| format!("item-{n}"))
                        .find(|n| text.contains(&format!("ITEM={n};")))
                        .unwrap_or_default();
                    (i.custom_id, name)
                })
                .collect(),
        );
        let id = format!("msgbatch_g{}", sent.len());
        if self.losing.load(Ordering::SeqCst) {
            self.unreachable.lock().unwrap().insert(id.clone());
        }
        Ok(id)
    }
    async fn poll(&self, id: &str) -> anyhow::Result<BatchStatus> {
        if let Some(e) = self.answer_for(id) {
            return Err(e);
        }
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        if let Some(e) = self.answer_for(id) {
            return Err(e);
        }
        let which: usize = id.trim_start_matches("msgbatch_g").parse().unwrap();
        let sent = self.sent.lock().unwrap()[which - 1].clone();
        let out = self.credit_out.load(Ordering::SeqCst);
        Ok(sent
            .into_iter()
            .map(|(custom_id, name)| {
                if out && name == "item-1" {
                    return BatchResultRow {
                        custom_id,
                        response: None,
                        error: Some(CREDIT_OUT.to_string()),
                        billed: None,
                    };
                }
                self.answered.lock().unwrap().push(name);
                BatchResultRow {
                    custom_id,
                    response: Some(LlmResponse {
                        text: "answer".to_string(),
                        model: "claude-sonnet-5-5".to_string(),
                        backend: "api".to_string(),
                        cost_usd: Some(1.0),
                        input_tokens: Some(100),
                        output_tokens: Some(10),
                        cache_read_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        or_cache_discount: None,
                        truncated: false,
                        reported_model: None,
                        cli_cost_usd: None,
                        elapsed_ms: None,
                    }),
                    error: None,
                    billed: None,
                }
            })
            .collect())
    }
}

struct Items {
    ids: Vec<String>,
    got: std::collections::BTreeMap<String, String>,
}

impl Items {
    fn of(ids: &[&str]) -> Self {
        Self {
            ids: ids.iter().map(|i| i.to_string()).collect(),
            got: Default::default(),
        }
    }
}

impl StageItems for Items {
    type Id = String;
    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.ids
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    LlmRequest::new(format!("ITEM={id};"))
                        .with_model("claude-sonnet-5-5")
                        .with_max_tokens(1024),
                )
            })
            .collect()
    }
    fn absorb(&mut self, id: &String, resp: LlmResponse) -> Result<(), ItemReject> {
        self.got.insert(id.clone(), resp.text);
        Ok(())
    }
    fn retry(&self, _id: &String, _reject: &ItemReject) -> Option<LlmRequest> {
        None
    }
}

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().unwrap();
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@example.com"]);
    g(&["config", "user.name", "T"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "one"]);
    dir
}

fn new_run(repo: &tempfile::TempDir) -> Arc<RunStore> {
    RunStore::create(NewRun {
        pin: crate::run_store::engine_pin::EnginePin::for_run(None, &Default::default()),
        project_key: "proj-356".to_string(),
        repos: vec![(REPO.to_string(), repo.path().to_path_buf())],
        models: crate::run_store::ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "batch".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp1".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({"client": "x"}),
        estimate_usd: Some(10.0),
        include_alternatives: true,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

/// Run `fut` the way a run does: with its prompt seed in scope.
async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([9u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

fn meter(stored: f64) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        scope: Default::default(),
        max_cost_usd: None,
        stored_spend_usd: stored,
        ..RunControls::default()
    })
}

/// One attempt of the calibration stage over `ids`: a fresh process on the saved run `id`, with
/// the journal over `fake` and the stage's checkpoint. Returns the outcome and the meter's notes.
async fn attempt(
    run: &str,
    fake: &Arc<Provider>,
    ids: &[&str],
) -> (stage_batch::StageOutcome<String>, Vec<String>, Items) {
    let store = RunStore::open(run).unwrap();
    let dir = store.checkpoint_dir();
    seeded(async {
        let api = crate::run_store::journal::JournalBatchApi::new(fake.clone(), store.clone());
        let cp = StageCheckpoint::new(&dir, "calibration_acme_widgets");
        let meter = meter(store.stored_usd());
        let mut items = Items::of(ids);
        let ctx = StageCtx {
            repo: REPO,
            model: "claude-sonnet-5-5",
            realtime: api.realtime(),
            batch: Some(&api),
            meter: Some(&meter),
            job: None,
            checkpoint: Some(&cp),
            settings: StageSettings::default(),
            feedback: None,
        };
        let out = run_stage(Stage::Calibration, &mut items, StageMode::Batch, &ctx).await;
        let notes = meter.notes();
        (out, notes, items)
    })
    .await
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test(start_paused = true)]
#[serial_test::serial(process_env)]
async fn a_resume_after_an_expired_unfetched_batch_sends_its_unanswered_items_once_and_finishes() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let run = new_run(&repo).run_id();
    let fake = Arc::new(Provider::new());

    // Attempt 1: item-0 is answered in a batch of its own and stored.
    let (out, _, _) = attempt(&run, &fake, &["item-0"]).await;
    assert!(out.is_complete(), "{:?}", out.failed);
    // Attempt 2: the other two go out; the process stops waiting before their batch is fetched.
    fake.lose_contact();
    let (out, _, _) = attempt(&run, &fake, &["item-0", "item-1", "item-2"]).await;
    assert_eq!(
        sorted(out.failed.iter().map(|f| f.id.clone()).collect()),
        names(&["item-1", "item-2"]),
        "the two items wait on a batch that may be running"
    );
    assert_eq!(fake.sent_items().len(), 2);
    // Its results expire before the next attempt.
    fake.expire_unreachable();

    // Attempt 3: the gone batch is settled with its note and the two items go out once in a new
    // batch; item-0 is replayed and the stage finishes.
    let (out, notes, items) = attempt(&run, &fake, &["item-0", "item-1", "item-2"]).await;
    assert!(
        out.failed.is_empty(),
        "the resume finishes: {:?}",
        out.failed
    );
    assert_eq!(items.got.len(), 3, "every item has its answer");
    assert_eq!(
        fake.sent_items(),
        vec![
            names(&["item-0"]),
            names(&["item-1", "item-2"]),
            names(&["item-1", "item-2"])
        ],
        "only the gone batch's unanswered items are sent again, once"
    );
    let notes = notes.join("\n");
    assert!(
        notes.contains("msgbatch_g2") && notes.contains("no longer exists at the provider"),
        "the gone batch is disclosed with the existing note: {notes}"
    );
    assert_eq!(
        fake.answered(),
        names(&["item-0", "item-1", "item-2"]),
        "every item was answered, and paid for, once"
    );

    // Attempt 4: nothing is sent, and the gone batch is not asked about again.
    let polls = fake.polls_of_gone.load(Ordering::SeqCst);
    let (out, _, items) = attempt(&run, &fake, &["item-0", "item-1", "item-2"]).await;
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert_eq!(items.got.len(), 3);
    assert_eq!(fake.sent_items().len(), 3, "a second resume sends nothing");
    assert_eq!(fake.polls_of_gone.load(Ordering::SeqCst), polls);
    assert_eq!(fake.realtime.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
#[serial_test::serial(process_env)]
async fn a_credit_stopped_item_whose_resend_batch_expired_is_sent_once_more_and_never_twice() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let run = new_run(&repo).run_id();
    let fake = Arc::new(Provider::new());
    let all = ["item-0", "item-1", "item-2"];

    // Attempt 1: item-1 is refused for credit; the run stops.
    fake.credit_out.store(true, Ordering::SeqCst);
    let (out, _, _) = attempt(&run, &fake, &all).await;
    assert_eq!(out.failed.len(), 1, "{:?}", out.failed);
    fake.credit_out.store(false, Ordering::SeqCst);
    // Attempt 2: item-1 goes out again, and the process stops waiting before it is fetched.
    fake.lose_contact();
    let (out, _, _) = attempt(&run, &fake, &all).await;
    assert_eq!(out.failed.len(), 1, "{:?}", out.failed);
    assert_eq!(fake.sent_items().last().unwrap(), &names(&["item-1"]));
    let sent_before = fake.sent_items().len();
    fake.expire_unreachable();

    // Attempt 3: item-1 is sent once more and the stage finishes.
    let (out, _, items) = attempt(&run, &fake, &all).await;
    assert!(
        out.failed.is_empty(),
        "the resume finishes: {:?}",
        out.failed
    );
    assert_eq!(items.got.len(), 3);
    assert_eq!(
        fake.sent_items()[sent_before..],
        [names(&["item-1"])],
        "item-1 is sent once more, alone"
    );
    // Attempt 4: nothing more.
    let (out, _, _) = attempt(&run, &fake, &all).await;
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert_eq!(
        fake.sent_items().len(),
        sent_before + 1,
        "never a second time"
    );
    assert_eq!(
        fake.answered(),
        names(&["item-0", "item-1", "item-2"]),
        "every item was answered, and paid for, once"
    );
}
