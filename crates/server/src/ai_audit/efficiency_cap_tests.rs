//! Rank 9: the per-measure efficiency switches reach their code paths, and the spend cap stops a
//! run between passes, loudly, within one pass of the limit. No network: every model is a stub.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::cost_cap::RunControls;
use crate::efficiency::EfficiencyConfig;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";
const REPO_MAP: &str = "REPO MAP:\nfoo.rs: fn foo()\n";

/// Answers every call with `{}`, charging a fixed `cost`, and logs start/end order and the
/// number of calls in flight.
struct StubLlm {
    cost: f64,
    calls: AtomicUsize,
    events: Mutex<Vec<(&'static str, usize)>>,
    truncated: bool,
}

impl StubLlm {
    fn new(cost: f64) -> Self {
        Self {
            cost,
            calls: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
            truncated: false,
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The 0-based rule-batch a scan prompt belongs to (`(batch n/m)` in its task line).
fn batch_of(prompt: &str) -> usize {
    (1..=64)
        .find(|n| prompt.contains(&format!("(batch {n}/")))
        .map_or(0, |n| n - 1)
}

#[async_trait::async_trait]
impl LlmPort for StubLlm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let b = batch_of(&req.prompt);
        self.events.lock().unwrap().push(("start", b));
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        self.events.lock().unwrap().push(("end", b));
        Ok(LlmResponse {
            text: "{}".to_string(),
            model: "stub".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(self.cost),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: self.truncated,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        on_delta("{}");
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn meter(efficiency: EfficiencyConfig, cap: Option<f64>) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        efficiency,
        max_cost_usd: cap,
        estimate_hint_usd: None,
        fix_model: None,
        consensus: false,
    })
}

fn off(name: &str) -> EfficiencyConfig {
    let mut config = EfficiencyConfig::default();
    config.set(name, false).unwrap();
    config
}

/// One chunk, `n_batches` rule-batches, through the real `run_passes`.
async fn scan(llm: &StubLlm, n_batches: usize, concurrency: usize, meter: &UsageMeter) -> usize {
    let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rule_batches: Vec<Vec<(String, String)>> = (0..n_batches)
        .map(|n| vec![(format!("RULE-{n}"), format!("rule {n}"))])
        .collect();
    let batches: Vec<&[(String, String)]> = rule_batches.iter().map(Vec::as_slice).collect();
    let (_f, _p, _r, ok, err) = run_passes(
        llm,
        REPO,
        REPO_MAP,
        &crate::prompt_frame::FrameNonce::generate(),
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        concurrency,
        "pass[all]",
        "eff",
        Some(meter),
        false,
    )
    .await;
    assert!(err.is_empty(), "{err:?}");
    ok
}

// ── switches reach their code paths ─────────────────────────────────────────────────────────

#[tokio::test]
async fn warm_schedule_on_holds_siblings_until_the_first_pass_returns_and_off_does_not() {
    let started_before_first_end = |events: &[(&'static str, usize)]| {
        let first_end = events.iter().position(|e| e.0 == "end").unwrap();
        events[..first_end]
            .iter()
            .filter(|e| e.0 == "start")
            .count()
    };

    let llm = StubLlm::new(0.0);
    let on = meter(EfficiencyConfig::default(), None);
    assert_eq!(scan(&llm, 4, 6, &on).await, 4);
    assert_eq!(
        started_before_first_end(&llm.events.lock().unwrap()),
        1,
        "warm-then-fan-out: the first pass goes alone"
    );

    let llm = StubLlm::new(0.0);
    let off = meter(off("warm_schedule"), None);
    assert_eq!(scan(&llm, 4, 6, &off).await, 4);
    assert_eq!(
        started_before_first_end(&llm.events.lock().unwrap()),
        4,
        "switch off: every pass is dispatched at once, as before rank 3"
    );
}

#[test]
fn label_after_prefix_off_puts_the_label_back_inside_the_cached_prefix() {
    let (digest, task, rules) = ("DIGEST", "TASK", "RULES");
    let (on, on_bps) = build_pass_prompt_with(
        true,
        REPO,
        REPO_MAP,
        "pass[web]",
        1,
        3,
        digest,
        4,
        task,
        rules,
    );
    let (off, off_bps) = build_pass_prompt_with(
        false,
        REPO,
        REPO_MAP,
        "pass[web]",
        1,
        3,
        digest,
        4,
        task,
        rules,
    );

    // The pre-rank-3 layout, spelled out.
    assert_eq!(
        off,
        format!("Repository: {REPO}\n\n{REPO_MAP}(pass[web] 2/3)\n\n{digest}\n\n{task}\n\n{rules}")
    );
    // Same bytes either way.
    assert_ne!(on, off);
    let sorted = |s: &str| {
        let mut b = s.as_bytes().to_vec();
        b.sort_unstable();
        b
    };
    assert_eq!(sorted(&on), sorted(&off));
    // Off: the label is inside the cached digest segment; on: it is after every breakpoint.
    let label = "(pass[web] 2/3)";
    assert!(off[off_bps[0]..off_bps[1]].contains(label));
    assert!(on[on_bps[1]..].contains(label));
}

#[test]
fn batch_long_cache_off_asks_for_the_five_minute_cache() {
    // A many-item submission, where the 1-hour cache is the cheaper one (rank 2's TTL rule).
    use super::batch_mode::batch_cache_ttl;
    assert_eq!(
        batch_cache_ttl(EfficiencyConfig::default(), 40),
        crate::llm::CacheTtl::OneHour
    );
    assert_eq!(
        batch_cache_ttl(off("batch_long_cache"), 40),
        crate::llm::CacheTtl::FiveMinutes
    );
}

async fn truncated_pass_calls(efficiency: EfficiencyConfig) -> usize {
    let mut llm = StubLlm::new(0.1);
    llm.truncated = true;
    let meter = meter(efficiency, None);
    let _ = audit_pass(
        &llm,
        None,
        "prompt".to_string(),
        Vec::new(),
        &crate::prompt_frame::FrameNonce::generate(),
        "o/r",
        &std::collections::HashSet::new(),
        None,
        "s",
        Some(&meter),
        1,
    )
    .await;
    llm.calls()
}

#[tokio::test]
async fn truncation_retry_off_makes_a_single_call() {
    assert_eq!(truncated_pass_calls(EfficiencyConfig::default()).await, 2);
    assert_eq!(truncated_pass_calls(off("truncation_retry")).await, 1);
    // Rank 10's switch is the fix stage's alone: the scan's truncation retry is unchanged by it.
    assert_eq!(truncated_pass_calls(off("fix_on_haiku")).await, 2);
}

#[tokio::test]
async fn fix_prefilter_off_sends_a_no_code_finding_to_the_model() {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    let finding = || {
        let mut f = Finding {
            repo: "o/r".to_string(),
            path: "supabase/migrations/1.sql".to_string(),
            line: 0,
            rule_id: "SUPABASE-RLS-ENABLED-1".to_string(),
            severity: "high".to_string(),
            snippet: "CREATE TABLE profiles (...)".to_string(),
            detail: "no RLS evidence for `profiles`".to_string(),
            ..Finding::default()
        };
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        f
    };
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];
    for (efficiency, expect_model_call) in [
        (EfficiencyConfig::default(), false),
        (off("fix_prefilter"), true),
    ] {
        let llm = StubLlm::new(0.1);
        let meter = meter(efficiency, None);
        let _ = generate_fix_specifics(
            &llm,
            "o/r",
            vec![finding()],
            &files,
            None,
            Some(&meter),
            Some(&corpus),
        )
        .await;
        assert_eq!(llm.calls() > 0, expect_model_call, "{efficiency:?}");
    }
}

// ── the spend cap ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_capped_run_stops_between_passes_within_one_pass_of_the_cap() {
    const PASS: f64 = 1.0;
    const TOTAL: usize = 24;
    for cap in [0.4, 1.0, 2.5, 4.2, 7.3, 11.9] {
        let llm = StubLlm::new(PASS);
        let meter = meter(EfficiencyConfig::default(), Some(cap));
        let ok = scan(&llm, TOTAL, 6, &meter).await;

        let spent = meter.spent_usd();
        let record = meter.cost_cap_record().unwrap();
        assert_eq!(ok, llm.calls(), "every admitted pass finished (cap {cap})");
        assert_eq!(
            ok + record.not_run_passes.len(),
            TOTAL,
            "every pair either ran or is recorded NOT RUN (cap {cap})"
        );
        assert!(ok >= 1, "the first pass is always admitted (cap {cap})");
        assert!(
            spent < cap + PASS + 1e-9,
            "spend {spent} exceeds the cap {cap} by more than one pass"
        );
        assert!(
            record.stopped_the_run() == (ok < TOTAL),
            "stopped_the_run must say whether anything was skipped (cap {cap})"
        );
        // Everything that did not run is named, with its rules, and nothing ran twice.
        let mut seen: Vec<usize> = record.not_run_passes.iter().map(|p| p.rule_batch).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), record.not_run_passes.len());
        assert!(record
            .not_run_passes
            .iter()
            .all(|p| p.rule_ids.len() == 1 && p.group == "pass[all]" && p.n_rule_batches == TOTAL));
    }
}

#[tokio::test]
async fn with_no_cap_every_pass_runs_and_nothing_is_recorded() {
    let llm = StubLlm::new(5.0);
    let meter = meter(EfficiencyConfig::default(), None);
    assert_eq!(scan(&llm, 10, 6, &meter).await, 10);
    assert!(meter.cost_cap_record().is_none());
}

#[tokio::test]
async fn a_cap_that_is_never_reached_leaves_the_run_complete() {
    let llm = StubLlm::new(0.01);
    let meter = meter(EfficiencyConfig::default(), Some(100.0));
    assert_eq!(scan(&llm, 10, 6, &meter).await, 10);
    let record = meter.cost_cap_record().unwrap();
    assert!(!record.stopped_the_run() && record.failed_passes().is_empty());
}

#[test]
fn a_stage_that_would_start_past_the_cap_is_blocked_and_recorded_once() {
    let meter = meter(EfficiencyConfig::default(), Some(1.0));
    assert!(!meter.stage_blocked("o/r", "calibration"), "under the cap");
    meter.record_stage(
        Stage::ScanPass,
        &LlmResponse {
            text: String::new(),
            model: "m".into(),
            backend: "api".into(),
            cost_usd: Some(1.5),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        },
    );
    assert!(meter.stage_blocked("o/r", "calibration"));
    assert!(meter.stage_blocked("o/r", "calibration"));
    let record = meter.cost_cap_record().unwrap();
    assert_eq!(record.not_run_stages.len(), 1, "recorded once");
    assert!(record.stopped_the_run());
}

// ── batch mode: the cap is checked before submitting ────────────────────────────────────────

fn with_cap(cap: f64, hint: Option<f64>) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        efficiency: EfficiencyConfig::default(),
        max_cost_usd: Some(cap),
        estimate_hint_usd: hint,
        fix_model: None,
        consensus: false,
    })
}

fn spend(meter: &UsageMeter, dollars: f64) {
    meter.record_stage(
        Stage::ScanPass,
        &LlmResponse {
            text: String::new(),
            model: "m".into(),
            backend: "api".into(),
            cost_usd: Some(dollars),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        },
    );
}

#[test]
fn the_batch_gate_refuses_when_spend_plus_projection_would_pass_the_cap() {
    // Nothing spent: the projection alone decides.
    assert!(with_cap(5.0, None)
        .batch_refusal_reason(Some(4.0))
        .is_none());
    assert!(with_cap(5.0, None)
        .batch_refusal_reason(Some(6.0))
        .is_some());
    // The pre-flight estimate raises a low projection.
    assert!(with_cap(5.0, Some(9.0))
        .batch_refusal_reason(Some(0.0))
        .is_some());
    assert!(with_cap(5.0, Some(3.0))
        .batch_refusal_reason(Some(0.0))
        .is_none());
    // No cap: never refuses, even with no projection.
    assert!(UsageMeter::default()
        .batch_refusal_reason(Some(1e9))
        .is_none());
    assert!(UsageMeter::default().batch_refusal_reason(None).is_none());
    let why = with_cap(5.0, None).batch_refusal_reason(Some(6.0)).unwrap();
    assert!(
        why.contains("NOT submitted") && why.contains("cannot be cancelled"),
        "{why}"
    );
}

#[test]
fn a_batch_with_no_projection_at_all_is_refused_and_says_the_limit_could_not_be_enforced() {
    // Cap set, the exact count failed, the estimator could not price it, no hint: refuse.
    let why = with_cap(5.0, None)
        .batch_refusal_reason(None)
        .expect("a limit that cannot be enforced must not silently allow the spend");
    assert!(
        why.contains("could not be enforced") && why.contains("NOT submitted"),
        "{why}"
    );
    // Any one projection source is enough to decide.
    assert!(with_cap(5.0, Some(1.0))
        .batch_refusal_reason(None)
        .is_none());
    assert!(with_cap(5.0, Some(50.0))
        .batch_refusal_reason(None)
        .is_some());
}

#[test]
fn the_whole_run_estimate_is_charged_only_for_what_remains_of_it() {
    // A $8 whole-run estimate under a $10 cap. After $7 is spent, only $1 of the estimate
    // remains, so a second sub-batch is not refused as if the whole $8 were still to come.
    let meter = with_cap(10.0, Some(8.0));
    assert!(
        meter.batch_refusal_reason(Some(0.0)).is_none(),
        "first submission"
    );
    spend(&meter, 7.0);
    assert!(
        meter.batch_refusal_reason(Some(0.0)).is_none(),
        "7 spent + 1 remaining estimate fits"
    );
    // Once the estimate is used up it contributes nothing; a real projection still counts.
    spend(&meter, 2.0);
    assert!(meter.batch_refusal_reason(Some(0.5)).is_none());
    assert!(meter.batch_refusal_reason(Some(1.5)).is_some());
}

#[test]
fn the_batch_fallback_estimate_prices_a_known_model_and_gives_up_on_an_unknown_one() {
    let files = vec![("a.rs".to_string(), "fn a() {}\n".repeat(2_000))];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules = vec![("R-1".to_string(), "d".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    assert!(
        batch_estimate_usd("claude-sonnet-5", &chunks, &batches, false).is_some_and(|d| d > 0.0)
    );
    assert!(batch_estimate_usd("no-such-model", &chunks, &batches, false).is_none());
}

#[tokio::test]
async fn a_batch_the_cap_refuses_is_never_submitted_and_every_pair_is_not_run() {
    // A client with a key set but no network: the gate must fire BEFORE any request is made,
    // so reaching the network at all would fail this test with a transport error.
    let llm = crate::llm::Llm::for_backend(crate::llm::Backend::Api, Some("sk-test".to_string()));
    let meter = UsageMeter::with_controls(RunControls {
        efficiency: EfficiencyConfig::default(),
        max_cost_usd: Some(1.0),
        estimate_hint_usd: Some(50.0),
        fix_model: None,
        consensus: false,
    });
    let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules: Vec<(String, String)> = (0..3)
        .map(|n| (format!("RULE-{n}"), format!("rule {n}")))
        .collect();
    let rule_batches: Vec<&[(String, String)]> = rules.chunks(1).collect();

    let (findings, _p, _r, ok, err) = run_passes_batch(
        &llm,
        REPO,
        REPO_MAP,
        &crate::prompt_frame::FrameNonce::generate(),
        &std::collections::HashSet::new(),
        None,
        None,
        &chunks,
        None,
        &rule_batches,
        "pass",
        Some(&meter),
        false,
    )
    .await
    .expect("a refused batch is a result, not an error");

    assert!(findings.is_empty() && ok == 0, "nothing ran");
    assert!(
        err.is_empty(),
        "a refused item is NOT RUN, not an error: {err:?}"
    );
    let record = meter.cost_cap_record().unwrap();
    assert_eq!(record.not_run_passes.len(), 3);
    assert!(record.stopped_the_run());
    assert!(
        record.notes.iter().any(|n| n.contains("NOT submitted")),
        "the refusal is said plainly: {:?}",
        record.notes
    );
    assert_eq!(meter.spent_usd(), 0.0);
}

// ── later stages cannot overshoot without bound ─────────────────────────────────────────────

#[test]
fn once_a_pass_was_refused_every_later_stage_is_blocked_even_below_the_cap() {
    let meter = with_cap(100.0, None);
    spend(&meter, 1.0);
    assert!(
        !meter.stage_blocked("o/r", STAGE_CALIBRATION),
        "nothing stopped yet"
    );
    meter.record_not_run(crate::cost_cap::NotRunPass {
        files: Vec::new(),
        repo: "o/r".into(),
        group: "pass[all]".into(),
        chunk: 1,
        n_chunks: 1,
        rule_batch: 1,
        n_rule_batches: 1,
        rule_ids: vec!["R-1".into()],
    });
    for stage in [STAGE_ALTERNATIVES, STAGE_CALIBRATION, STAGE_FIX, STAGE_DEEP] {
        assert!(meter.stage_blocked("o/r", stage), "{stage} must not start");
    }
    let record = meter.cost_cap_record().unwrap();
    assert_eq!(record.not_run_stages.len(), 4);
    assert!(record.not_run_stages.iter().all(|s| !s.partial));
}

fn many_findings(n: usize) -> Vec<Finding> {
    (0..n)
        .map(|i| Finding {
            repo: "o/r".to_string(),
            path: format!("f{i}.rs"),
            line: 3,
            rule_id: "ARCH-1".to_string(),
            severity: "high".to_string(),
            snippet: format!("snippet {i}"),
            detail: "detail".to_string(),
            ..Finding::default()
        })
        .collect()
}

#[tokio::test]
async fn calibration_stops_between_batches_and_passes_the_rest_through_unchanged() {
    // 100 findings = 3 calibration batches of 40/40/20. $1 per call against a $0.5 cap: the
    // first batch is admitted (nothing spent), the other two are not.
    let llm = StubLlm::new(1.0);
    let meter = meter(EfficiencyConfig::default(), Some(0.5));
    let findings = many_findings(100);
    let (out, _failed) = verify_findings(
        &llm,
        "o/r",
        findings,
        None,
        None,
        Some(&meter),
        false,
        "A repo.",
    )
    .await;
    assert_eq!(llm.calls(), 1, "only the first calibration batch ran");
    assert_eq!(out.len(), 100, "no finding is dropped");
    let record = meter.cost_cap_record().unwrap();
    assert_eq!(record.not_run_stages.len(), 1);
    assert_eq!(record.not_run_stages[0].stage, STAGE_CALIBRATION);
    assert!(
        record.not_run_stages[0].partial,
        "it started and was stopped part way"
    );
    assert!(record.failed_passes()[0]
        .reason
        .contains("NOT RUN (in part)"));
}

#[tokio::test]
async fn fix_generation_stops_between_batches() {
    // 60 findings with code around them = 3 fix batches per round. The first call is admitted,
    // then the cap is reached: no further batch is sent in this round or any retry round.
    let llm = StubLlm::new(1.0);
    let meter = meter(EfficiencyConfig::default(), Some(0.5));
    let files: Vec<(String, String)> = (0..60)
        .map(|i| {
            (
                format!("f{i}.rs"),
                "fn a() {}\nfn b() {}\nfn c() {}\n".to_string(),
            )
        })
        .collect();
    let (out, _failed) = generate_fix_specifics(
        &llm,
        "o/r",
        many_findings(60),
        &files,
        None,
        Some(&meter),
        None,
    )
    .await;
    assert_eq!(llm.calls(), 1, "no batch after the cap was reached");
    assert_eq!(out.len(), 60, "no finding is dropped");
    let record = meter.cost_cap_record().unwrap();
    assert!(record
        .not_run_stages
        .iter()
        .any(|s| s.stage == STAGE_FIX && s.partial));
}

// ── no cost known yet: exactly one pass in flight (warm_schedule off) ───────────────────────

#[tokio::test]
async fn with_a_cap_and_warm_schedule_off_only_one_pass_is_in_flight_until_a_cost_is_known() {
    let started_before_first_end = |events: &[(&'static str, usize)]| {
        let first_end = events.iter().position(|e| e.0 == "end").unwrap();
        events[..first_end]
            .iter()
            .filter(|e| e.0 == "start")
            .count()
    };
    // A generous cap: nothing is refused, but the first wave is a single pass.
    let llm = StubLlm::new(0.01);
    let capped = meter(off("warm_schedule"), Some(100.0));
    assert_eq!(
        scan(&llm, 6, 6, &capped).await,
        6,
        "a roomy cap loses nothing"
    );
    assert_eq!(started_before_first_end(&llm.events.lock().unwrap()), 1);
    assert!(!capped.cost_cap_record().unwrap().stopped_the_run());

    // And with no cap the unscheduled path is unchanged (all dispatched together).
    let llm = StubLlm::new(0.01);
    let uncapped = meter(off("warm_schedule"), None);
    assert_eq!(scan(&llm, 6, 6, &uncapped).await, 6);
    assert_eq!(started_before_first_end(&llm.events.lock().unwrap()), 6);
}

#[tokio::test]
async fn the_overshoot_bound_holds_with_warm_schedule_off_too() {
    const PASS: f64 = 1.0;
    const TOTAL: usize = 24;
    for cap in [0.4, 2.5, 7.3] {
        let llm = StubLlm::new(PASS);
        let meter = meter(off("warm_schedule"), Some(cap));
        let ok = scan(&llm, TOTAL, 6, &meter).await;
        let record = meter.cost_cap_record().unwrap();
        assert_eq!(ok + record.not_run_passes.len(), TOTAL, "cap {cap}");
        assert!(
            meter.spent_usd() < cap + PASS + 1e-9,
            "cap {cap}: {}",
            meter.spent_usd()
        );
        assert!(ok >= 1);
    }
}

// ── a refused call is billed, so the cap must count it ──────────────────────────────────────

/// A model refusal as the transport raises it: an error carrying what the call was BILLED.
/// On Opus 5.5 ($4 in / $20 out per million) 100K input + 10K output is exactly $0.60.
fn billed_refusal() -> anyhow::Error {
    crate::llm::refusal_error_billed(
        &serde_json::json!({
            "model": "claude-opus-5-5",
            "content": [],
            "stop_reason": "refusal",
            "usage": { "input_tokens": 100_000, "output_tokens": 10_000 }
        }),
        "claude-opus-5-5",
        "api",
        crate::llm::PriceBasis {
            batch: false,
            cache_ttl: crate::llm::CacheTtl::FiveMinutes,
        },
    )
    .expect("a refusal stop_reason is an error")
}

#[test]
fn a_realtime_refusal_counts_toward_the_cap_and_sets_the_reserve() {
    let m = meter(EfficiencyConfig::default(), Some(1.0));
    m.record_failed(Stage::ScanPass, &billed_refusal());
    assert!((m.spent_usd() - 0.60).abs() < 1e-9, "{}", m.spent_usd());
    assert!(!m.cap_reached());
    m.record_failed(Stage::ScanPass, &billed_refusal());
    assert!(m.cap_reached(), "two refusals at $0.60 pass a $1.00 cap");
    // A failure that carries no usage (a transport error) changes nothing.
    let before = m.spent_usd();
    m.record_failed(Stage::ScanPass, &anyhow::anyhow!("connection reset"));
    assert_eq!(m.spent_usd(), before);
}

#[test]
fn a_billed_batch_refusal_counts_toward_the_cap() {
    let refused = r#"{"custom_id":"c0-b0","result":{"type":"succeeded","message":{"model":"claude-opus-5-5","content":[],"stop_reason":"refusal","usage":{"input_tokens":100000,"output_tokens":10000}}}}"#;
    let rows = crate::llm::parse_batch_results_jsonl(refused, crate::llm::BATCH_CACHE_TTL).unwrap();
    let results = crate::llm::reassemble_batch_results(rows);
    assert!(results.responses.is_empty());
    let m = meter(EfficiencyConfig::default(), Some(5.0));
    let (_f, _p, _r, ok, failures) = reassemble_batch_passes(
        REPO,
        "pass",
        1,
        1,
        &[(0, 0)],
        &[1],
        &results,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
        Some(&m),
        false,
        None,
    );
    assert_eq!(ok, 0);
    assert!(failures[0].error.contains("refus"));
    // $0.60 real-time, halved for a batch row.
    assert!((m.spent_usd() - 0.30).abs() < 1e-9, "{}", m.spent_usd());
}

/// Every call refuses. The first refusal teaches the cap what a call costs, and the run still
/// stops at the cap instead of running every pass at an apparent $0.
struct AlwaysRefuses {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for AlwaysRefuses {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(billed_refusal())
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

#[tokio::test]
async fn a_run_whose_first_calls_all_refuse_still_stops_at_the_cap() {
    const TOTAL: usize = 10;
    let llm = AlwaysRefuses {
        calls: AtomicUsize::new(0),
    };
    let m = meter(EfficiencyConfig::default(), Some(1.0));
    let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rule_batches: Vec<Vec<(String, String)>> = (0..TOTAL)
        .map(|n| vec![(format!("RULE-{n}"), format!("rule {n}"))])
        .collect();
    let batches: Vec<&[(String, String)]> = rule_batches.iter().map(Vec::as_slice).collect();
    let (_f, _p, _r, ok, failures) = run_passes(
        &llm,
        REPO,
        REPO_MAP,
        &crate::prompt_frame::FrameNonce::generate(),
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        6,
        "pass[all]",
        "refuse",
        Some(&m),
        false,
    )
    .await;
    let record = m.cost_cap_record().unwrap();
    let calls = llm.calls.load(Ordering::SeqCst);
    assert_eq!(ok, 0);
    assert!(
        calls < TOTAL,
        "the cap stopped the run: {calls} of {TOTAL} calls ran"
    );
    assert_eq!(
        calls,
        failures.len(),
        "every call that ran is a named failed pass"
    );
    assert_eq!(
        calls + record.not_run_passes.len(),
        TOTAL,
        "every other pass is recorded NOT RUN"
    );
    assert!(
        m.spent_usd() >= 1.0,
        "refusals were counted: {}",
        m.spent_usd()
    );
}

// ── the cap needs every call's cost ─────────────────────────────────────────────────────────

fn response(
    model: &str,
    cost: Option<f64>,
    input: Option<u64>,
    output: Option<u64>,
) -> LlmResponse {
    LlmResponse {
        text: String::new(),
        model: model.to_string(),
        backend: "api".to_string(),
        cost_usd: cost,
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

#[tokio::test]
async fn a_call_with_tokens_but_no_cost_is_priced_from_its_tokens_under_a_cap() {
    let m = meter(EfficiencyConfig::default(), Some(10.0));
    // Opus 5.5: 100K in at $4/M + 10K out at $20/M = $0.60.
    m.record_stage(
        Stage::ScanPass,
        &response("claude-opus-5-5", None, Some(100_000), Some(10_000)),
    );
    assert!((m.spent_usd() - 0.60).abs() < 1e-9, "{}", m.spent_usd());
    assert!(
        m.admit().await.is_ok(),
        "an enforceable cap keeps admitting"
    );
}

#[tokio::test]
async fn a_call_whose_cost_cannot_be_known_makes_the_cap_unenforceable_and_stops_admission() {
    let m = meter(EfficiencyConfig::default(), Some(10.0));
    // No dollar figure, and a model the registry cannot price.
    m.record_stage(
        Stage::ScanPass,
        &response("mystery-model", None, Some(1_000), Some(100)),
    );
    assert!(m.admit().await.is_err(), "no further pass is admitted");
    let record = m.cost_cap_record().unwrap();
    assert!(
        record
            .notes
            .iter()
            .any(|n| n.contains("could not be enforced") && n.contains("mystery-model")),
        "{:?}",
        record.notes
    );
    assert!(
        m.stage_blocked(REPO, STAGE_CALIBRATION),
        "later stages are blocked too"
    );
    // Missing token counts are unpriceable too.
    let m2 = meter(EfficiencyConfig::default(), Some(10.0));
    m2.record_stage(
        Stage::ScanPass,
        &response("claude-opus-5-5", None, None, None),
    );
    assert!(m2.admit().await.is_err());
}

#[tokio::test]
async fn without_a_cap_a_call_with_no_cost_changes_nothing() {
    let m = meter(EfficiencyConfig::default(), None);
    m.record_stage(
        Stage::ScanPass,
        &response("mystery-model", None, Some(1), Some(1)),
    );
    assert!(m.admit().await.is_ok());
    assert!(m.cost_cap_record().is_none());
}

#[test]
fn a_fraction_of_a_micro_dollar_is_rounded_up_into_the_cap() {
    let m = meter(EfficiencyConfig::default(), Some(1.0));
    m.record_stage(
        Stage::ScanPass,
        &response("m", Some(0.0000001), Some(1), Some(1)),
    );
    assert!(
        (m.spent_usd() - 0.000001).abs() < 1e-12,
        "{}",
        m.spent_usd()
    );
}

/// Rank 4: the fallback estimate behind the batch spend-cap gate includes the detail
/// regeneration step when `terse_output` is on and leaves it out when off, so a run the gate
/// would refuse with the step counted is not admitted on an estimate that forgot it.
#[test]
fn the_batch_fallback_estimate_includes_detail_regeneration_only_with_terse_output_on() {
    let files = vec![("a.rs".to_string(), "fn a() {}\n".repeat(2_000))];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules = vec![("R-1".to_string(), "d".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let off = batch_estimate_usd("claude-sonnet-5", &chunks, &batches, false).unwrap();
    let on = batch_estimate_usd("claude-sonnet-5", &chunks, &batches, true).unwrap();
    assert!(on > off, "on {on} must exceed off {off}");

    // A limit between the two: refused with the step counted, admitted on the estimate without it.
    let cap = (on + off) / 2.0;
    let meter = with_cap(cap, None);
    assert!(meter.batch_refusal_reason(Some(off)).is_none());
    assert!(
        meter.batch_refusal_reason(Some(on)).is_some(),
        "the estimate that includes the regeneration step is the one that trips the limit"
    );
}
