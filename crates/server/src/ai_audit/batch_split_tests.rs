//! M1 item 3: a batch submission is split by body bytes as well as by item count, so no body
//! goes over the API's 256 MB limit; the results of the several batches join back in order; and a
//! resume after a partial submission submits only the batches that are missing.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::batch_mode::{
    max_batch_body_bytes, plan_sub_batches, set_test_body_limit, submit_round, BatchApi, BatchWork,
    RoundContext, MAX_BATCH_BODY_BYTES,
};
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};

fn ok_reply() -> LlmResponse {
    LlmResponse {
        reported_model: None,
        text: "{\"findings\":[]}".to_string(),
        model: "stub".to_string(),
        backend: "stub".to_string(),
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

struct Realtime;

#[async_trait::async_trait]
impl LlmPort for Realtime {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(ok_reply())
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

/// A Message Batches API that records each submission (its custom ids and body size) and answers
/// every item. The `fail_on`th (0-based) submit call fails outright, as an outage would.
struct Api {
    realtime: Realtime,
    submissions: Mutex<Vec<Vec<String>>>,
    bodies: Mutex<Vec<usize>>,
    calls: AtomicUsize,
    fail_on: Option<usize>,
}

impl Api {
    fn new(fail_on: Option<usize>) -> Self {
        Self {
            realtime: Realtime,
            submissions: Mutex::new(Vec::new()),
            bodies: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail_on,
        }
    }
}

#[async_trait::async_trait]
impl BatchApi for Api {
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
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_on == Some(call) {
            anyhow::bail!("Message Batches API HTTP 503: submission refused");
        }
        let body = serde_json::to_vec(&serde_json::json!({ "requests": items }))?;
        self.bodies.lock().unwrap().push(body.len());
        let mut subs = self.submissions.lock().unwrap();
        subs.push(items.iter().map(|i| i.custom_id.clone()).collect());
        Ok(format!("msgbatch_{}_{}", call, subs.len() - 1))
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        let index: usize = batch_id.rsplit('_').next().unwrap().parse().unwrap();
        let ids = self.submissions.lock().unwrap()[index].clone();
        Ok(ids
            .into_iter()
            .map(|custom_id| BatchResultRow {
                custom_id,
                response: Some(ok_reply()),
                error: None,
                billed: None,
            })
            .collect())
    }
}

const MODEL: &str = "claude-sonnet-5-5";

/// `n` batch items whose requests carry `prompt_bytes` bytes each.
fn work(n: usize, prompt_bytes: usize) -> Vec<BatchWork> {
    (0..n)
        .map(|i| BatchWork {
            custom_id: format!("c{i}-b0"),
            ci: i,
            bi: 0,
            req: LlmRequest::new(format!("{i:06}{}", "p".repeat(prompt_bytes)))
                .with_model(MODEL)
                .with_max_tokens(1024),
        })
        .collect()
}

fn context<'a>(
    api: &'a dyn BatchApi,
    chunks: &'a [&'a [(String, String)]],
    batches: &'a [&'a [(String, String)]],
    max_body_bytes: usize,
) -> RoundContext<'a> {
    RoundContext {
        api,
        repo: "acme/widgets",
        label: "pass",
        model: MODEL,
        n_c: chunks.len(),
        n_b: batches.len(),
        chunks,
        batches,
        job: None,
        meter: None,
        per_item_usd: None,
        max_body_bytes,
        fatal_errors: false,
    }
}

/// The inputs `context` borrows: one file per chunk and one rule batch.
type Files = Vec<(String, String)>;

fn shape(n: usize) -> (Vec<Files>, Files) {
    let chunks = (0..n)
        .map(|i| vec![(format!("f{i}.rs"), "fn f() {}".to_string())])
        .collect();
    (chunks, vec![("RULE-A".to_string(), "do a".to_string())])
}

#[test]
fn the_body_limit_is_a_named_constant_below_the_api_limit() {
    const {
        assert!(MAX_BATCH_BODY_BYTES < 256 * 1024 * 1024);
        assert!(MAX_BATCH_BODY_BYTES < 256_000_000);
    }
    assert_eq!(max_batch_body_bytes(), MAX_BATCH_BODY_BYTES);
    set_test_body_limit(Some(1234));
    assert_eq!(max_batch_body_bytes(), 1234);
    set_test_body_limit(None);
}

#[tokio::test(start_paused = true)]
async fn work_several_times_the_limit_is_several_batches_each_under_it_and_joined_in_order() {
    // 30 items of ~100 KB: 3 MB of bodies against a 1 MB limit.
    let items = work(30, 100_000);
    let all: Vec<&BatchWork> = items.iter().collect();
    let (chunk_store, rules) = shape(30);
    let chunks: Vec<&[(String, String)]> = chunk_store.iter().map(Vec::as_slice).collect();
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let api = Api::new(None);
    let limit = 1_000_000;
    let ctx = context(&api, &chunks, &batches, limit);

    let out = submit_round(&ctx, &all).await.unwrap();

    let bodies = api.bodies.lock().unwrap().clone();
    assert!(
        bodies.len() >= 3,
        "3 MB of work against a 1 MB limit is at least 3 batches: {bodies:?}"
    );
    for b in &bodies {
        assert!(*b <= limit + 64, "a body of {b} bytes is over {limit}");
    }
    // Every item went out exactly once, in order, and every result is back.
    let sent: Vec<String> = api
        .submissions
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .cloned()
        .collect();
    let want: Vec<String> = items.iter().map(|w| w.custom_id.clone()).collect();
    assert_eq!(sent, want);
    assert_eq!(out.results.len(), 30);
    assert!(out.results.values().all(Result::is_ok));
    assert!(out.refused.is_empty());
}

#[test]
fn the_plan_is_contiguous_deterministic_and_respects_the_item_cap_and_the_bytes() {
    let items = work(40, 50_000);
    let all: Vec<&BatchWork> = items.iter().collect();
    let (chunk_store, rules) = shape(40);
    let chunks: Vec<&[(String, String)]> = chunk_store.iter().map(Vec::as_slice).collect();
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let api = Api::new(None);
    let ctx = context(&api, &chunks, &batches, 500_000);
    let a = plan_sub_batches(&ctx, &all);
    let b = plan_sub_batches(&ctx, &all);
    assert_eq!(a, b, "the same work splits at the same places");
    assert!(a.len() > 1);
    // The ranges tile the work with no gap or overlap.
    assert_eq!(a[0].start, 0);
    assert_eq!(a.last().unwrap().end, 40);
    for pair in a.windows(2) {
        assert_eq!(pair[0].end, pair[1].start);
    }
    // One item that alone exceeds the limit still gets a batch of its own, never dropped.
    let huge = work(3, 2_000_000);
    let all: Vec<&BatchWork> = huge.iter().collect();
    let plan = plan_sub_batches(&ctx, &all);
    assert_eq!(plan.iter().map(|r| r.len()).sum::<usize>(), 3);
}

// ── A resume after a partial submission ──────────────────────────────────────────────────────

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

fn new_run(repo: &tempfile::TempDir) -> Arc<crate::run_store::RunStore> {
    use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};
    RunStore::create(NewRun {
        project_key: "proj-1".to_string(),
        repos: vec![("acme/api".to_string(), repo.path().to_path_buf())],
        models: ModelsRecord {
            audit: MODEL.to_string(),
            calibration: MODEL.to_string(),
            fix: MODEL.to_string(),
            backend: "api".to_string(),
            mode: "batch".to_string(),
            resolved: true,
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: MODEL.to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp1".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

#[tokio::test(start_paused = true)]
#[serial_test::serial(process_env)]
async fn a_resume_after_a_partial_submission_submits_only_the_missing_batches() {
    use crate::run_store::journal::JournalBatchApi;
    let state = tempfile::tempdir().unwrap();
    let _guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = git_repo();
    let store = new_run(&repo);
    let run_id = store.run_id();

    let items = work(12, 100_000);
    let all: Vec<&BatchWork> = items.iter().collect();
    let (chunk_store, rules) = shape(12);
    let chunks: Vec<&[(String, String)]> = chunk_store.iter().map(Vec::as_slice).collect();
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let limit = 400_000; // three or so items per batch: four batches
    let seed = Some([9u8; crate::prompt_frame::NONCE_BYTES]);

    // First attempt: the second submission is refused outright (nothing accepted), so the run
    // stops with the first, third and fourth batches submitted and fetched.
    let first_api = Arc::new(Api::new(Some(1)));
    let planned = {
        let journal = JournalBatchApi::new(first_api.clone(), store.clone());
        let ctx = context(&journal, &chunks, &batches, limit);
        let n_batches = plan_sub_batches(&ctx, &all).len();
        assert!(n_batches >= 4, "got {n_batches}");
        let out = crate::prompt_frame::with_seed(seed, submit_round(&ctx, &all))
            .await
            .unwrap();
        let failed: Vec<&String> = out
            .results
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(id, _)| id)
            .collect();
        assert!(
            !failed.is_empty(),
            "the refused batch's items have no reply"
        );
        n_batches
    };
    let first_sent: HashSet<String> = first_api
        .submissions
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .cloned()
        .collect();
    assert_eq!(first_api.submissions.lock().unwrap().len(), planned - 1);

    // The process is gone. A resume reopens the run and runs the same work again.
    drop(store);
    let store = crate::run_store::RunStore::open(&run_id).unwrap();
    let second_api = Arc::new(Api::new(None));
    let journal = JournalBatchApi::new(second_api.clone(), store);
    let ctx = context(&journal, &chunks, &batches, limit);
    let out = crate::prompt_frame::with_seed(seed, submit_round(&ctx, &all))
        .await
        .unwrap();

    // Only the missing batch was submitted again; nothing already submitted was.
    let resent = second_api.submissions.lock().unwrap().clone();
    assert_eq!(resent.len(), 1, "exactly the one missing batch: {resent:?}");
    for id in &resent[0] {
        assert!(!first_sent.contains(id), "{id} was submitted twice");
    }
    // And now every item has its reply, the earlier ones replayed from the run store.
    assert_eq!(out.results.len(), 12);
    assert!(out.results.values().all(Result::is_ok));
}
