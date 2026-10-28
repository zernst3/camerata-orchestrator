//! Rank 2: batch mode as the API default. Every test drives [`run_passes_batch`] through a fake
//! [`batch_mode::BatchApi`] (no network): the switch ON reaches resubmission, the shared
//! truncation retry and the failed-pass disclosure; OFF reproduces the pre-rank-2 path; the
//! spend limit gates the resubmission; the TTL rule and the mode resolution are pinned.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::batch_mode::{
    batch_cache_ttl, failed_scan_pass, one_hour_cache_is_cheaper, resolve_scan_mode, BatchApi,
    CLI_BACKEND_REALTIME_NOTICE,
};
use super::*;
use crate::cost_cap::RunControls;
use crate::efficiency::EfficiencyConfig;
use crate::llm::{
    BackendResolution, BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl,
    LlmPort, LlmRequest, LlmResponse,
};

const REPO: &str = "acme/widgets";
const REPO_MAP: &str = "REPO MAP:\nfoo.rs: fn foo()\n";

/// What the fake answers for one item in one submission round (0-based).
#[derive(Clone)]
enum Reply {
    Ok,
    Truncated,
    Errored,
    Missing,
}

fn response(text: &str, truncated: bool, cost: f64) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "api/batch".to_string(),
        cost_usd: Some(cost),
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated,
    }
}

/// The real-time side of the fake: counts calls and records each request's output cap.
struct Realtime {
    calls: AtomicUsize,
    max_tokens: Mutex<Vec<u32>>,
}

#[async_trait::async_trait]
impl LlmPort for Realtime {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.max_tokens.lock().unwrap().push(req.max_tokens);
        Ok(response(&finding_json("RETRIED-1"), false, 0.01))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Answers one item: (submission round, custom id) -> reply.
type Script = dyn Fn(usize, &str) -> Reply + Send + Sync;

/// A Message Batches API that answers each item by a script keyed on (round, custom id), and
/// logs every submission.
struct FakeBatch {
    script: Box<Script>,
    submissions: Mutex<Vec<Vec<BatchItem>>>,
    pending: Mutex<HashMap<String, Vec<String>>>,
    realtime: Realtime,
    cost: f64,
}

impl FakeBatch {
    fn new(script: impl Fn(usize, &str) -> Reply + Send + Sync + 'static) -> Self {
        Self {
            script: Box::new(script),
            submissions: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            realtime: Realtime {
                calls: AtomicUsize::new(0),
                max_tokens: Mutex::new(Vec::new()),
            },
            cost: 0.01,
        }
    }
    fn submitted_ids(&self) -> Vec<Vec<String>> {
        self.submissions
            .lock()
            .unwrap()
            .iter()
            .map(|items| items.iter().map(|i| i.custom_id.clone()).collect())
            .collect()
    }
}

/// A findings reply naming one rule, so a test can tell which reply a pass used.
fn finding_json(rule: &str) -> String {
    format!(
        r#"{{"findings":[{{"path":"foo.rs","line":1,"rule":"{rule}","severity":"low","title":"t","detail":"d","snippet":"fn foo()"}}]}}"#
    )
}

#[async_trait::async_trait]
impl BatchApi for FakeBatch {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.realtime
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        Some(1_000)
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        let mut subs = self.submissions.lock().unwrap();
        let id = format!("msgbatch_{}", subs.len());
        self.pending.lock().unwrap().insert(
            id.clone(),
            items.iter().map(|i| i.custom_id.clone()).collect(),
        );
        subs.push(items);
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        let round: usize = batch_id.trim_start_matches("msgbatch_").parse().unwrap();
        let ids = self.pending.lock().unwrap()[batch_id].clone();
        Ok(ids
            .into_iter()
            .filter_map(|id| {
                let row = |response: Option<LlmResponse>, error: Option<&str>| BatchResultRow {
                    custom_id: id.clone(),
                    response,
                    error: error.map(str::to_string),
                };
                match (self.script)(round, &id) {
                    Reply::Ok => Some(row(
                        Some(response(
                            &finding_json(&format!("R{round}")),
                            false,
                            self.cost,
                        )),
                        None,
                    )),
                    Reply::Truncated => Some(row(
                        Some(response(r#"{"findings":[{"path":"#, true, self.cost)),
                        None,
                    )),
                    Reply::Errored => Some(row(None, Some("overloaded"))),
                    Reply::Missing => None,
                }
            })
            .collect())
    }
}

fn eff(batch_default: bool) -> EfficiencyConfig {
    EfficiencyConfig {
        batch_default,
        ..EfficiencyConfig::default()
    }
}

fn meter(efficiency: EfficiencyConfig, cap: Option<f64>) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        efficiency,
        max_cost_usd: cap,
        estimate_hint_usd: None,
    })
}

/// Two chunks, two rule-batches of one rule each: four items `c0-b0 c0-b1 c1-b0 c1-b1`.
async fn run(api: &FakeBatch, meter: &UsageMeter) -> (Vec<Finding>, usize, Option<anyhow::Error>) {
    let c0 = vec![("foo.rs".to_string(), "fn foo() {}\n".to_string())];
    let c1 = vec![("bar.rs".to_string(), "fn bar() {}\n".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![&c0, &c1];
    let b0 = vec![("RULE-A-1".to_string(), "do a".to_string())];
    let b1 = vec![("RULE-B-1".to_string(), "do b".to_string())];
    let batches: Vec<&[(String, String)]> = vec![&b0, &b1];
    let adopted: HashSet<String> = HashSet::new();
    let (findings, _proposed, _requested, ok, last_err) = run_passes_batch(
        api,
        REPO,
        REPO_MAP,
        &adopted,
        None,
        None,
        &chunks,
        &batches,
        "pass",
        Some(meter),
        false,
    )
    .await
    .expect("the batch path runs");
    (findings, ok, last_err)
}

#[tokio::test(start_paused = true)]
async fn switch_on_resubmits_an_errored_item_once_in_a_follow_up_batch() {
    let api = FakeBatch::new(|round, id| match (round, id) {
        (0, "c1-b0") => Reply::Errored,
        (0, "c0-b1") => Reply::Missing,
        _ => Reply::Ok,
    });
    let m = meter(eff(true), None);
    let (findings, ok, _) = run(&api, &m).await;
    assert_eq!(
        api.submitted_ids(),
        vec![
            vec!["c0-b0", "c0-b1", "c1-b0", "c1-b1"],
            vec!["c0-b1", "c1-b0"],
        ],
        "only the failed items go out again, together, once"
    );
    assert_eq!(ok, 4, "every item ended with a reply");
    assert_eq!(
        findings.iter().filter(|f| f.rule_id == "AI-R1").count(),
        2,
        "the resubmitted items' replies are used"
    );
    assert!(m.take_failed_scan_passes(REPO).is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_item_that_fails_twice_is_a_failed_pass_and_the_export_says_incomplete() {
    let api = FakeBatch::new(|_round, id| {
        if id == "c1-b1" {
            Reply::Errored
        } else {
            Reply::Ok
        }
    });
    let m = meter(eff(true), None);
    let (_findings, ok, last_err) = run(&api, &m).await;
    assert_eq!(api.submitted_ids().len(), 2, "resubmitted exactly once");
    assert_eq!(ok, 3);
    assert!(last_err.is_some());
    let failed = m.take_failed_scan_passes(REPO);
    assert_eq!(failed.len(), 1, "{failed:?}");
    let fp = &failed[0];
    assert_eq!(fp.pass, "scan pass pass chunk 2/2 rule-batch 2/2");
    assert!(
        fp.reason.contains("resubmitted once and failed again"),
        "{}",
        fp.reason
    );
    assert!(
        fp.reason.contains("This inspection is incomplete."),
        "{}",
        fp.reason
    );
    assert!(!fp.reason.contains('\u{2014}'), "no em-dash: {}", fp.reason);

    // The same FailedPass reaches the client report's failed-pass disclosure, in the summary
    // and the methodology, exactly like any other failed pass.
    let mut report = crate::onboard::ScanReport::gated(&[REPO.to_string()]);
    report.gated = false;
    report.failed_passes = failed;
    let json = crate::report_export::build_report_json(
        &report.seal(),
        &HashMap::new(),
        None,
        &crate::report_export::ReportOptions::default(),
    );
    for lines in [
        &json.executive_summary.failed_passes,
        &json.methodology.failed_passes,
    ] {
        assert!(
            lines
                .iter()
                .any(|l| l.contains("chunk 2/2 rule-batch 2/2") && l.contains("incomplete")),
            "{lines:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn switch_off_reproduces_the_single_submission_with_no_resubmission() {
    let api = FakeBatch::new(|_round, id| {
        if id == "c1-b1" {
            Reply::Errored
        } else {
            Reply::Ok
        }
    });
    let m = meter(eff(false), None);
    let (_findings, ok, last_err) = run(&api, &m).await;
    assert_eq!(api.submitted_ids().len(), 1, "no follow-up batch");
    assert_eq!(ok, 3);
    assert!(
        last_err.is_some(),
        "the failure still reaches last_err as before"
    );
    assert!(
        m.take_failed_scan_passes(REPO).is_empty(),
        "pre-rank-2: no per-item disclosure"
    );
    let items = &api.submissions.lock().unwrap()[0];
    let body = serde_json::to_string(items).unwrap();
    assert!(
        body.contains(r#""ttl":"1h""#),
        "off keeps the 1-hour batch cache"
    );
}

#[tokio::test(start_paused = true)]
async fn a_truncated_batch_item_gets_the_shared_real_time_truncation_retry() {
    let api = FakeBatch::new(|_round, id| {
        if id == "c0-b0" {
            Reply::Truncated
        } else {
            Reply::Ok
        }
    });
    let m = meter(eff(true), None);
    let (findings, ok, _) = run(&api, &m).await;
    assert_eq!(ok, 4);
    assert_eq!(api.realtime.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *api.realtime.max_tokens.lock().unwrap(),
        vec![16_384],
        "the same doubled cap the real-time retry asks for"
    );
    assert!(
        findings.iter().any(|f| f.rule_id == "AI-RETRIED-1"),
        "the retry's reply is the one used"
    );
    assert_eq!(
        api.submitted_ids().len(),
        1,
        "a truncated reply is not resubmitted"
    );

    // OFF: the cut-off reply is used as it came, with no real-time call.
    let api = FakeBatch::new(|_round, id| {
        if id == "c0-b0" {
            Reply::Truncated
        } else {
            Reply::Ok
        }
    });
    let m = meter(eff(false), None);
    run(&api, &m).await;
    assert_eq!(api.realtime.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn the_truncation_retry_switch_still_governs_batch_items() {
    let api = FakeBatch::new(|_round, _id| Reply::Truncated);
    let mut efficiency = eff(true);
    efficiency.truncation_retry = false;
    let m = meter(efficiency, None);
    run(&api, &m).await;
    assert_eq!(api.realtime.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn the_spend_limit_is_checked_before_the_resubmission_batch() {
    // Each reply costs $5, so after the first round the $10 limit is passed and the follow-up
    // batch must not be submitted.
    let mut api = FakeBatch::new(|round, id| match (round, id) {
        (0, "c1-b1") => Reply::Errored,
        _ => Reply::Ok,
    });
    api.cost = 5.0;
    let m = meter(eff(true), Some(10.0));
    let (_findings, ok, _) = run(&api, &m).await;
    assert_eq!(ok, 3);
    assert_eq!(api.submitted_ids().len(), 1, "the resubmission was refused");
    let record = m.cost_cap_record().expect("a cap was set");
    assert_eq!(record.not_run_passes.len(), 1, "{record:?}");
    assert_eq!(
        (
            record.not_run_passes[0].chunk,
            record.not_run_passes[0].rule_batch
        ),
        (2, 2)
    );
    assert!(
        m.take_failed_scan_passes(REPO).is_empty(),
        "a refused item is NOT RUN (the cap's own disclosure), not a failed pass"
    );
}

#[test]
fn one_hour_ttl_only_when_the_arithmetic_says_it_is_cheaper() {
    assert!(!one_hour_cache_is_cheaper(0));
    assert!(
        !one_hour_cache_is_cheaper(1),
        "one item: 2x write against 1.25x"
    );
    assert!(one_hour_cache_is_cheaper(2), "2.1 against 2.5");
    assert!(one_hour_cache_is_cheaper(500));

    assert_eq!(batch_cache_ttl(eff(true), 1), CacheTtl::FiveMinutes);
    assert_eq!(batch_cache_ttl(eff(true), 40), CacheTtl::OneHour);
    assert_eq!(
        batch_cache_ttl(eff(false), 1),
        CacheTtl::OneHour,
        "off: the pre-rank-2 1-hour batch cache"
    );
    let mut short = eff(true);
    short.batch_long_cache = false;
    assert_eq!(batch_cache_ttl(short, 40), CacheTtl::FiveMinutes);
}

#[tokio::test(start_paused = true)]
async fn a_one_item_resubmission_asks_for_the_five_minute_cache() {
    let api = FakeBatch::new(|round, id| match (round, id) {
        (0, "c0-b0") => Reply::Errored,
        _ => Reply::Ok,
    });
    let m = meter(eff(true), None);
    run(&api, &m).await;
    let subs = api.submissions.lock().unwrap();
    let first = serde_json::to_string(&subs[0]).unwrap();
    let retry = serde_json::to_string(&subs[1]).unwrap();
    assert!(first.contains(r#""ttl":"1h""#), "four items: 1 hour");
    assert!(!retry.contains(r#""ttl":"1h""#), "one item: 5 minutes");
}

#[test]
fn an_unknown_scan_mode_is_an_error_naming_the_choices() {
    let err = ScanMode::parse(Some("batchh")).unwrap_err();
    assert!(err.contains("batchh") && err.contains("parallel") && err.contains("batch"));
    assert_eq!(ScanMode::parse(None), Ok(None));
    assert_eq!(ScanMode::parse(Some("  ")), Ok(None));
    assert_eq!(ScanMode::parse(Some("AUTO")), Ok(None));
    assert_eq!(ScanMode::parse(Some("Batch")), Ok(Some(ScanMode::Batch)));
    assert_eq!(
        ScanMode::parse(Some("parallel")),
        Ok(Some(ScanMode::Parallel))
    );
    assert_eq!(
        ScanMode::parse(Some("sequential")),
        Ok(Some(ScanMode::Sequential))
    );
}

#[test]
fn the_default_mode_is_batch_on_the_api_backend_only_with_the_switch_on() {
    let api = BackendResolution::Api;
    let on = resolve_scan_mode(None, &api, eff(true));
    assert_eq!((on.mode, on.notice), (ScanMode::Batch, None));
    let off = resolve_scan_mode(None, &api, eff(false));
    assert_eq!(
        (off.mode, off.notice),
        (ScanMode::Parallel, None),
        "off: batch is opt-in again"
    );
    let forced = resolve_scan_mode(Some(ScanMode::Parallel), &api, eff(true));
    assert_eq!(forced.mode, ScanMode::Parallel, "real time can be forced");
    let opted = resolve_scan_mode(Some(ScanMode::Batch), &api, eff(false));
    assert_eq!(opted.mode, ScanMode::Batch, "off: opting in still works");
    let blocked = BackendResolution::Blocked {
        message: "no key".to_string(),
    };
    assert_eq!(
        resolve_scan_mode(None, &blocked, eff(true)).mode,
        ScanMode::Parallel
    );
}

#[test]
fn the_cli_backend_stays_real_time_and_says_so() {
    let choice = resolve_scan_mode(None, &BackendResolution::Cli, eff(true));
    assert_eq!(choice.mode, ScanMode::Parallel);
    assert_eq!(choice.notice.as_deref(), Some(CLI_BACKEND_REALTIME_NOTICE));
    assert!(!CLI_BACKEND_REALTIME_NOTICE.contains('\u{2014}'));
    let off = resolve_scan_mode(None, &BackendResolution::Cli, eff(false));
    assert_eq!(off.notice, None, "off: today's default, nothing to explain");
}

#[test]
fn a_failed_scan_pass_names_its_place_and_caps_its_reason() {
    let fp = failed_scan_pass(REPO, "pass", 0, 3, 1, 4, 15, &"x".repeat(1_000));
    assert_eq!(fp.pass, "scan pass pass chunk 1/3 rule-batch 2/4");
    assert!(fp.reason.contains("15 rule check(s)"));
    assert!(fp.reason.len() < 500, "{}", fp.reason.len());
}

/// A real-time pass that errors while its siblings succeed used to vanish into `last_err`; it is
/// now disclosed in the same shape a twice-failed batch item is.
#[tokio::test]
async fn a_failed_real_time_pass_is_disclosed_too() {
    struct FailSecond(AtomicUsize);
    #[async_trait::async_trait]
    impl LlmPort for FailSecond {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
                anyhow::bail!("HTTP 500 after retries");
            }
            Ok(response(r#"{"findings":[]}"#, false, 0.01))
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            let r = self.complete(req).await?;
            on_delta(&r.text);
            Ok(r)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let llm = FailSecond(AtomicUsize::new(0));
    let m = meter(eff(true), None);
    let c0 = vec![("foo.rs".to_string(), "fn foo() {}\n".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![&c0];
    let b0 = vec![("RULE-A-1".to_string(), "do a".to_string())];
    let b1 = vec![("RULE-B-1".to_string(), "do b".to_string())];
    let batches: Vec<&[(String, String)]> = vec![&b0, &b1];
    let (_f, _p, _r, ok, last_err) = run_passes(
        &llm,
        REPO,
        REPO_MAP,
        &HashSet::new(),
        None,
        None,
        None,
        &chunks,
        &batches,
        1,
        "pass",
        "audit-test",
        Some(&m),
        false,
    )
    .await;
    assert_eq!(ok, 1);
    assert!(last_err.is_some());
    let failed = m.take_failed_scan_passes(REPO);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].reason.contains("HTTP 500"),
        "{}",
        failed[0].reason
    );
    assert!(failed[0].reason.contains("This inspection is incomplete."));
}
