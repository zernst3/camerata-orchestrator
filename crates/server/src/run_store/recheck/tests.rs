//! The recheck of a saved, incomplete inspection (`camerata inspect --recheck-saved`). Synthetic
//! data only: a scratch git repo with one decision written twice, a run saved the way the scan
//! saves it (the real clone judge over a journaled scripted model that answers garbage, the real
//! sealed report and ledger writers), and a counting model that must stay unasked.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata_api_types::audit::{FinderRowRef, FindingKind, PlaceStatus, RuleConvention};
use camerata_checks::clone_rules::CloneRule;
use std::sync::Arc;

use super::*;
use crate::ai_audit::rejection_reason_tests::{call_files, new_run, state_dir};
use crate::llm::{LlmRequest, LlmResponse};
use crate::onboard::clone_advisory::{judge_clone_candidates, JudgeRequest, VerdictCache};
use crate::onboard::Finding;
use crate::review_tree::FlaggedBy;
use crate::run_store::engine_pin::BuildId;
use crate::run_store::finish_run;
use crate::run_store::journal::JournalLlm;
use crate::scan_ledger::{ScanLedger, INTEGRITY_PASS_PREFIX};

const RULE: CloneRule = CloneRule::SingleSourceLogic;
const REPO: &str = "o/r";
const BUILD_A: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

fn discount(func: &str, rate: &str) -> String {
    format!(
        "export function {func}(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = base * {rate};
    return base - discount;
  }}
  if (base > 100 && order.coupon !== null) {{
    return base - 10;
  }}
  return base + order.shipping;
}}
"
    )
}

/// A model that answers with `reply` and counts how often it is asked.
struct Counting {
    calls: AtomicUsize,
    models: std::sync::Mutex<Vec<String>>,
    reply: Box<dyn Fn(&LlmRequest) -> String + Send + Sync>,
}

impl Counting {
    fn garbage() -> Self {
        Self::with(|_| "I cannot give verdicts for these.".to_string())
    }

    fn with(reply: impl Fn(&LlmRequest) -> String + Send + Sync + 'static) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            models: std::sync::Mutex::new(Vec::new()),
            reply: Box::new(reply),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmPort for Counting {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.models.lock().unwrap().push(req.model.clone());
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: (self.reply)(&req),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.001),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
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

/// The ids a judge request asks about.
fn asked(req: &LlmRequest) -> Vec<String> {
    req.prompt
        .split_once(crate::onboard::clone_advisory::prompt::BATCH_MARKER)
        .map(|(_, rest)| rest)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|i| i["id"].as_str().map(str::to_string))
        .collect()
}

/// One verdict object per candidate: what the first build's reader turned down and the current
/// one merges.
fn disjoint_objects(ids: &[String]) -> String {
    ids.iter()
        .map(|id| {
            serde_json::json!({"verdicts": [{"id": id, "verdict": "same_decision",
                "concept": "the discount"}]})
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the saved run's stored reply to the judge is, after the first build turned it down.
#[derive(Clone, Copy, PartialEq)]
enum Stored {
    /// Verdict objects the current reader merges.
    ReadableNow,
    /// Prose with no verdict in it.
    Unreadable,
}

#[derive(Clone, Copy)]
struct Spec {
    stored: Stored,
    /// The first build's integrity check lost a finder row that the collapse had merged into
    /// another row's `flagged_by` (#586).
    merged_row_defect: bool,
    /// A second row of the rule, judged, so the part is not a whole-rule failure.
    partly_judged: bool,
    /// A failed part the recheck does not redo (the fix stage), which must stay as saved.
    other_failure: bool,
}

const NORMAL: Spec = Spec {
    stored: Stored::ReadableNow,
    merged_row_defect: true,
    partly_judged: false,
    other_failure: false,
};

struct Saved {
    run_id: String,
    repo: tempfile::TempDir,
    ledger_file: std::path::PathBuf,
    _state: (tempfile::TempDir, camerata_test_env::EnvGuard),
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
    std::fs::create_dir_all(dir.path().join("src/cart")).unwrap();
    std::fs::create_dir_all(dir.path().join("src/checkout")).unwrap();
    std::fs::write(
        dir.path().join("src/cart/total.ts"),
        discount("applyDiscount", "0.2"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/checkout/summary.ts"),
        discount("quoteTotal", "0.15"),
    )
    .unwrap();
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "one decision, written twice"]);
    dir
}

fn controls() -> crate::cost_cap::RunControls {
    crate::cost_cap::RunControls::default()
}

fn rule_files(repo: &std::path::Path) -> Vec<(String, String)> {
    crate::onboard::read_local_repo_files(repo, &controls().scope.for_repo(REPO))
        .unwrap()
        .files
}

fn finding(rule: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "medium".to_string(),
        snippet: "const query = build(userInput);".to_string(),
        detail: "The query is built from the caller's text.".to_string(),
        ..Finding::default()
    }
}

/// A report the way the first build left it, built from the fewest fields a report needs: every
/// other field is read as its default, as it is for a report saved before the field existed.
fn report_of(findings: &[Finding]) -> ScanReport {
    serde_json::from_value(serde_json::json!({
        "repos": [REPO],
        "files_scanned": 2,
        "findings": findings,
        "proposed_rules": [],
        "gated": false,
    }))
    .unwrap()
}

/// Save a run the way the first build saves it: the real clone judge asks a scripted model
/// through the run's journal and fails on garbage, the sealed report and the ledger are written,
/// the run ends incomplete, and its pin names an older build.
async fn saved_run(spec: Spec) -> Saved {
    let state = state_dir();
    let repo = git_repo();
    let store = new_run(repo.path());
    let files = rule_files(repo.path());
    let armed: std::collections::HashSet<&str> = [RULE.id()].into_iter().collect();
    let set = camerata_checks::clone_rules::candidates(&files, &armed);
    let ids: Vec<String> = set.by_rule[&RULE].iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids.len(), 1, "the fixture holds one candidate: {ids:?}");
    let judge_model = store.record().pin.unwrap().clone_judge_model;
    let stub = Counting::garbage();
    let journal = JournalLlm::new(&stub, store.clone());
    let mut cache = VerdictCache::default();
    let outcome = crate::prompt_frame::with_seed(
        Some(store.nonce_seed()),
        judge_clone_candidates(
            Some(&journal),
            JudgeRequest {
                repo: REPO,
                files: &files,
                set,
                judge_model: &judge_model,
                corpus: None,
                unavailable_reason: "",
            },
            &mut cache,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(outcome.failed.len(), 1, "the first build's judge failed");
    assert_eq!(stub.calls(), 2, "the first ask and its repair");
    // The first build turned the stored replies down; the current reader may read them.
    if spec.stored == Stored::ReadableNow {
        for file in call_files(&store) {
            let mut file = file;
            for entry in file.entries.values_mut() {
                if let Some(r) = entry.response.as_mut() {
                    r.text = disjoint_objects(&ids);
                }
            }
            std::fs::write(
                store.dir().join("calls").join(format!("{}.json", file.key)),
                serde_json::to_vec(&file).unwrap(),
            )
            .unwrap();
        }
    }
    let mut rows = vec![finding("SEC-NO-RAW-SQL-1", "src/db.ts", 20)];
    let mut ledger = ScanLedger::new();
    crate::onboard::clone_advisory::record_in_ledger(&mut ledger, REPO, &outcome);
    let mut failed = outcome.failed.clone();
    let mut records: Vec<RuleConvention> = Vec::new();
    if spec.merged_row_defect {
        // A finder row the collapse folded into the survivor's `flagged_by`: kept, not lost.
        rows[0].flagged_by.push(FlaggedBy {
            rule_id: "SEC-OPT-1".to_string(),
            tool: None,
            site_key: "k-folded".to_string(),
            path: "src/db.ts".to_string(),
            line: 9,
        });
        records.push(RuleConvention {
            repo: REPO.to_string(),
            rule_id: "SEC-OPT-1".to_string(),
            finder_rows: vec![FinderRowRef {
                site_key: "k-folded".to_string(),
                path: "src/db.ts".to_string(),
                line: 9,
                keyed_rule_id: "SEC-OPT-1".to_string(),
                status: PlaceStatus::Departs,
                ..FinderRowRef::default()
            }],
            ..RuleConvention::default()
        });
        let mut block = crate::scan_ledger::ConventionsBlock::from_records(&records, &rows);
        block.lost_finder_rows = 1;
        ledger.record_conventions(block);
        failed.push(
            crate::scan_ledger::convention_accounting_failure(
                REPO,
                &[("SEC-OPT-1".to_string(), "src/db.ts".to_string(), 9)],
            )
            .unwrap(),
        );
    }
    rows.extend(outcome.not_assessed.clone());
    if spec.other_failure {
        failed.push(FailedPass {
            repo: REPO.to_string(),
            pass: "fix text".to_string(),
            reason: "a fix reply was turned down".to_string(),
            files_not_reviewed: 0,
            class: FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
            scope_note: false,
        });
    }
    if spec.partly_judged {
        // A confirmed row of the rule: the first build judged part of it.
        rows.push(finding(RULE.id(), "src/cart/total.ts", 1));
    }
    let mut report = report_of(&rows);
    report.conventions = records;
    report.failed_passes = failed;
    report.ledger = ledger;
    let sealed = report.seal();
    crate::run_store::write_report(&store.dir().join(REPORT_FILE), &sealed).unwrap();
    let ledger_file = state.0.path().join("o-r.ledger.json");
    std::fs::write(&ledger_file, serde_json::to_vec(&sealed.ledger).unwrap()).unwrap();
    finish_run(&store, &sealed, false);
    let run_id = store.run_id();
    drop(store);
    // The run was made by an older build.
    let path = crate::run_store::runs_dir().join(&run_id).join("run.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["pin"]["build"]["commit"] = serde_json::json!(BUILD_A);
    v["pin"]["build"]["dirty"] = serde_json::json!(false);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    Saved {
        run_id,
        repo,
        ledger_file,
        _state: state,
    }
}

fn selected() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE.id().to_string(),
        directive: String::new(),
        repos: Vec::new(),
    }]
}

struct Ctx {
    sources: Vec<(String, std::path::PathBuf)>,
    selected: Vec<SelectedRule>,
    controls: crate::cost_cap::RunControls,
}

fn ctx(saved: &Saved) -> Ctx {
    Ctx {
        sources: vec![(REPO.to_string(), saved.repo.path().to_path_buf())],
        selected: selected(),
        controls: controls(),
    }
}

fn inputs<'a>(saved: &'a Saved, c: &'a Ctx, reask: ReaskFlag, client: bool) -> RecheckInputs<'a> {
    RecheckInputs {
        run_id: saved.run_id.clone(),
        sources: &c.sources,
        selected: &c.selected,
        corpus: None,
        controls: &c.controls,
        pin: EnginePin::for_run(None, &c.controls),
        client_engagement: client,
        ledger_file: &saved.ledger_file,
        reask,
    }
}

async fn recheck(
    saved: &Saved,
    reask: ReaskFlag,
    llm: &Counting,
) -> Result<Rechecked, RecheckError> {
    let c = ctx(saved);
    recheck_saved_run(&inputs(saved, &c, reask, false), Some(llm), &mut |_| {}).await
}

fn clone_rows(report: &ScanReport) -> Vec<&Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE.id())
        .collect()
}

// ── Acceptance: zero model calls ─────────────────────────────────────────────────────────────

/// The saved run's integrity failure is the #586 case (a finder row merged into another row's
/// `flagged_by`) and its clone part failed with a stored reply the current reader merges: the
/// recheck passes with ZERO model calls and the rows are the saved rows plus the clone verdicts.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_saved_run_with_the_merged_row_defect_and_a_readable_stored_reply_rechecks_with_zero_calls(
) {
    let saved = saved_run(NORMAL).await;
    let before = load_saved_report(
        &crate::run_store::runs_dir().join(&saved.run_id),
        &saved.ledger_file,
    )
    .unwrap();
    assert!(
        before
            .failed_passes
            .iter()
            .any(|f| f.pass.starts_with(INTEGRITY_PASS_PREFIX)),
        "the saved run carries the first build's integrity failure"
    );
    let llm = Counting::garbage();
    let done = recheck(&saved, ReaskFlag::Off, &llm)
        .await
        .expect("the recheck passes");
    assert_eq!(llm.calls(), 0, "no model call at all");
    let report = &done.report;
    assert!(
        report.failed_passes.is_empty(),
        "both failures are put right: {:?}",
        report.failed_passes
    );
    assert!(report.incomplete_reasons().is_empty());
    // Saved rows are used as saved, and the clone verdicts are added: the rule's one
    // not-assessed row is replaced by one row per site of the confirmed candidate.
    let saved_other: Vec<&Finding> = before
        .findings
        .iter()
        .filter(|f| f.rule_id != RULE.id())
        .collect();
    let now_other: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id != RULE.id())
        .collect();
    assert_eq!(now_other.len(), saved_other.len());
    for (a, b) in saved_other.iter().zip(&now_other) {
        assert_eq!((&a.path, a.line, &a.rule_id), (&b.path, b.line, &b.rule_id));
    }
    let rows = clone_rows(report);
    assert_eq!(rows.len(), 2, "one row per site of the confirmed candidate");
    assert!(
        rows.iter()
            .all(|r| !matches!(r.kind, Some(FindingKind::NotAssessed(_)))),
        "the not-assessed row is gone"
    );
    let mut paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["src/cart/total.ts", "src/checkout/summary.ts"]);
    // The ledger counts the rule as having run, with the rows it emitted.
    let entry = report.ledger.rule_for_repo(RULE.id(), REPO).unwrap();
    assert!(entry.ran && entry.skip_reason.is_none());
    assert_eq!(entry.findings_emitted, 2);
    assert!(entry.coverage_note.is_none(), "{:?}", entry.coverage_note);
    // The row accounting stays whole.
    assert_eq!(report.ledger.total_unaccounted(), 0);
    assert!(report
        .ledger
        .stages()
        .iter()
        .any(|s| s.stage == "clone-advisory-recheck"));
}

/// Across the whole recheck the model is a panic: any stage that asked it would fail the test, and
/// the ledger holds exactly the saved stages plus the one the delta adds.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn no_stage_but_the_named_clone_part_is_ever_entered() {
    struct Forbidden;
    #[async_trait]
    impl LlmPort for Forbidden {
        async fn complete(&self, _: LlmRequest) -> anyhow::Result<LlmResponse> {
            panic!("a stage asked the model during a recheck that needs no call");
        }
        async fn complete_streaming(
            &self,
            _: LlmRequest,
            _: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            panic!("a stage asked the model during a recheck that needs no call");
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let saved = saved_run(NORMAL).await;
    let before = load_saved_report(
        &crate::run_store::runs_dir().join(&saved.run_id),
        &saved.ledger_file,
    )
    .unwrap();
    let c = ctx(&saved);
    let done = recheck_saved_run(
        &inputs(&saved, &c, ReaskFlag::Allow { max_parts: 5 }, false),
        Some(&Forbidden),
        &mut |_| {},
    )
    .await
    .expect("a stored reply settles it: the flag asks nothing");
    let stages_before: Vec<&str> = before
        .ledger
        .stages()
        .iter()
        .map(|s| s.stage.as_str())
        .collect();
    let stages_now: Vec<&str> = done
        .report
        .ledger
        .stages()
        .iter()
        .map(|s| s.stage.as_str())
        .collect();
    assert_eq!(
        stages_now[..stages_before.len()],
        stages_before[..],
        "the saved stages are kept as saved"
    );
    assert_eq!(
        &stages_now[stages_before.len()..],
        ["collapse-identical", "group", "clone-advisory-recheck"],
        "the delta, then the scan's own steps for its rows, are the only stages this run adds"
    );
    assert_eq!(
        done.report.actual_usage, before.actual_usage,
        "no model spend was added to the run"
    );
}

// ── The delta: what a stored reply cannot settle ─────────────────────────────────────────────

const UNREADABLE: Spec = Spec {
    stored: Stored::Unreadable,
    merged_row_defect: true,
    partly_judged: false,
    other_failure: false,
};

/// A stored reply the current reader cannot read, and no flag: the recheck refuses naming the
/// part, with zero calls.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_unreadable_stored_reply_without_the_flag_refuses_naming_the_part_and_asks_nothing() {
    let saved = saved_run(UNREADABLE).await;
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains(RULE.id()) && why.contains(REPO), "{why}");
    assert!(why.contains("--reask-failed-judge"), "{why}");
    assert_eq!(llm.calls(), 0);
}

/// With the flag exactly one call goes out, on the run's own judge model, and the run records it
/// on this build.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_the_flag_the_unreadable_part_is_asked_once_on_the_runs_own_model_and_recorded() {
    let saved = saved_run(UNREADABLE).await;
    let run_model = read_record(&saved.run_id)
        .unwrap()
        .pin
        .unwrap()
        .clone_judge_model;
    let llm = Counting::with(|req| disjoint_objects(&asked(req)));
    let mut told = String::new();
    let c = ctx(&saved);
    let done = recheck_saved_run(
        &inputs(&saved, &c, ReaskFlag::Allow { max_parts: 5 }, false),
        Some(&llm),
        &mut |t| told.push_str(t),
    )
    .await
    .expect("the one call settles it");
    assert_eq!(llm.calls(), 1, "exactly one call");
    assert_eq!(
        llm.models.lock().unwrap().as_slice(),
        std::slice::from_ref(&run_model)
    );
    assert!(
        told.contains(RULE.id()) && told.contains("1 clone check part"),
        "the part is listed before it runs: {told}"
    );
    assert_eq!(clone_rows(&done.report).len(), 2);
    let detail = done.record.recheck.as_ref().expect("recheck detail");
    assert_eq!(detail.clone_parts.len(), 1);
    assert_eq!(
        detail.clone_parts[0].how,
        RedoHow::AskedAgain { model: run_model }
    );
    assert_eq!(done.record.deterministic_build, BuildId::current());
    // The call is in the run's own store, filed under the request the scan sent.
    let stored: usize = call_files(&done.store)
        .iter()
        .map(|f| f.entries.len())
        .sum();
    assert!(stored >= 3, "the two first-build replies and the new one");
}

/// The re-asked reply is a verdict object that names no candidate asked about: the call is
/// recorded as rejected in the run's store, never kept as a good reply, and the usage and the
/// ledger carry the call.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_re_asked_reply_with_no_usable_verdict_is_stored_as_rejected_and_the_call_is_counted() {
    let saved = saved_run(UNREADABLE).await;
    let llm = Counting::with(|_| {
        r#"{"verdicts":[{"id":"not-asked","verdict":"same_decision","concept":"x"}]}"#.to_string()
    });
    let Err(RecheckError::Refused(_)) =
        recheck(&saved, ReaskFlag::Allow { max_parts: 5 }, &llm).await
    else {
        panic!("expected a refusal");
    };
    let dir = crate::run_store::runs_dir()
        .join(&saved.run_id)
        .join("calls");
    let statuses: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            serde_json::from_slice::<crate::run_store::journal::CallFile>(
                &std::fs::read(e.unwrap().path()).unwrap(),
            )
            .ok()
        })
        .flat_map(|f| f.entries.into_values())
        .filter(|e| {
            e.response
                .as_ref()
                .is_some_and(|r| r.text.contains("not-asked"))
        })
        .map(|e| e.status)
        .collect();
    assert_eq!(
        statuses,
        [crate::run_store::journal::EntryStatus::Rejected],
        "the unusable reply is stored as rejected, not good"
    );
}

/// A call that was made is on the run's usage and on the ledger's clone judge stage.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_re_asked_call_is_added_to_the_runs_usage_and_the_ledger() {
    let saved = saved_run(UNREADABLE).await;
    let llm = Counting::with(|req| disjoint_objects(&asked(req)));
    let done = recheck(&saved, ReaskFlag::Allow { max_parts: 5 }, &llm)
        .await
        .unwrap();
    let usage = done.report.actual_usage.clone().expect("usage");
    assert_eq!(usage.calls, 1);
    assert!((usage.cost_usd - 0.001).abs() < 1e-9, "{}", usage.cost_usd);
    let telemetry = done
        .report
        .ledger
        .stage_telemetry()
        .expect("ledger telemetry");
    let record = telemetry
        .stages
        .iter()
        .find(|r| r.stage == crate::stage_telemetry::Stage::CloneJudge)
        .expect("clone judge stage");
    assert_eq!(record.usage.calls, 1);
}

/// The cap: more parts than the flag allows refuse before any call.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn more_parts_than_the_cap_refuse_before_any_call() {
    let saved = saved_run(UNREADABLE).await;
    let llm = Counting::with(|req| disjoint_objects(&asked(req)));
    let Err(RecheckError::Refused(why)) =
        recheck(&saved, ReaskFlag::Allow { max_parts: 0 }, &llm).await
    else {
        panic!("expected a refusal");
    };
    assert!(why.contains("more than the 0 allowed"), "{why}");
    assert_eq!(llm.calls(), 0, "nothing is sent over the cap");
}

/// A re-asked reply that still holds no verdict leaves the part failed: refused, nothing exported.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_re_asked_reply_that_still_cannot_be_read_refuses() {
    let saved = saved_run(UNREADABLE).await;
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) =
        recheck(&saved, ReaskFlag::Allow { max_parts: 5 }, &llm).await
    else {
        panic!("expected a refusal");
    };
    assert!(why.contains("still could not be settled"), "{why}");
    assert_eq!(llm.calls(), 1, "never more than the one call");
}

/// The spend limit: a call that could pass it is refused before it is sent.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_call_that_could_pass_the_spend_limit_is_refused_before_it_is_sent() {
    let saved = saved_run(UNREADABLE).await;
    let llm = Counting::with(|req| disjoint_objects(&asked(req)));
    let mut c = ctx(&saved);
    c.controls.max_cost_usd = Some(0.000_001);
    let Err(RecheckError::Refused(why)) = recheck_saved_run(
        &inputs(&saved, &c, ReaskFlag::Allow { max_parts: 5 }, false),
        Some(&llm),
        &mut |_| {},
    )
    .await
    else {
        panic!("expected a refusal");
    };
    assert!(why.contains("spend limit"), "{why}");
    assert_eq!(llm.calls(), 0);
}

/// A part the first build judged in part is not a whole failure: refused, naming it.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_part_judged_in_part_is_refused_not_redone() {
    let saved = saved_run(Spec {
        partly_judged: true,
        ..NORMAL
    })
    .await;
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains(RULE.id()) && why.contains("judged"), "{why}");
    assert_eq!(llm.calls(), 0);
}

// ── Both builds are recorded ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_run_the_ledger_and_the_report_state_both_builds_and_what_reran() {
    let saved = saved_run(NORMAL).await;
    let llm = Counting::garbage();
    let done = recheck(&saved, ReaskFlag::Off, &llm).await.unwrap();
    let a = &BUILD_A[..12];
    let b = BuildId::current().short_commit().unwrap();
    let in_ledger = done.report.ledger.rerun().expect("the ledger records it");
    assert_eq!(in_ledger.model_build.commit.as_deref(), Some(BUILD_A));
    assert_eq!(in_ledger.deterministic_build, BuildId::current());
    // "How this was done", as the PDF data carries it.
    let opts = crate::report_export::ReportOptions::default();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(
        &done.report,
        &std::collections::HashMap::new(),
    );
    let note = crate::report_export::build_report_json(&reviewed, None, &opts)
        .methodology
        .deterministic_note;
    let expected = format!(
        "The findings, fix text and reviews in this inspection come from Camerata build {a}. The \
         final consistency checks and the export were run again on Camerata build {b}, using \
         those saved results unchanged. The duplicated code check for 1 candidate pair in o/r was \
         completed on build {b}, by reading the model's saved answer again. Any duplicated code it \
         confirmed carries the rule's standard fix and is not included in Patterns across services."
    );
    assert!(note.ends_with(&expected), "{note}");
    assert!(
        !note.contains('\u{2014}') && !note.contains('\u{2013}'),
        "no dashes: {note}"
    );
    // The workbook's Start here sheet says the same.
    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &opts).unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, "Start here").unwrap();
    let cell = grid
        .iter()
        .find(|r| r.first().is_some_and(|c| c == "How this was done"))
        .and_then(|r| r.get(1))
        .expect("Start here has a How this was done row");
    assert!(cell.contains(a) && cell.contains(b.as_str()), "{cell}");
    // The run's own record keeps it once the export is written.
    done.store.commit_recheck(done.record.clone());
    let record = read_record(&saved.run_id).unwrap();
    assert_eq!(record.reruns.len(), 1);
    assert_eq!(
        record.pin.as_ref().unwrap().build.commit.as_deref(),
        Some(BUILD_A),
        "the model work stays pinned to the build that made it"
    );
}

// ── The final checks are this build's own ────────────────────────────────────────────────────

/// A finder row that is in no final row, no row set aside and no `flagged_by` is still lost on
/// this build: the check replaces the stored failure with its own result, and still fails.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_row_that_is_really_lost_still_fails_the_check_on_this_build() {
    let saved = saved_run(NORMAL).await;
    // Remove the survivor's `flagged_by`: the finder row is now in none of the rows.
    let dir = crate::run_store::runs_dir().join(&saved.run_id);
    let mut report = load_saved_report(&dir, &saved.ledger_file).unwrap();
    for f in &mut report.findings {
        f.flagged_by.clear();
    }
    let sealed = report.seal();
    crate::run_store::write_report(&dir.join(REPORT_FILE), &sealed).unwrap();
    let llm = Counting::garbage();
    let done = recheck(&saved, ReaskFlag::Off, &llm).await.unwrap();
    let integrity: Vec<&FailedPass> = done
        .report
        .failed_passes
        .iter()
        .filter(|f| f.pass.starts_with(INTEGRITY_PASS_PREFIX))
        .collect();
    assert_eq!(integrity.len(), 1, "{:?}", done.report.failed_passes);
    assert!(integrity[0].reason.contains("SEC-OPT-1"));
    assert!(!done.report.incomplete_reasons().is_empty());
}

// ── Refusals ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_missing_or_unreadable_report_or_ledger_refuses_naming_the_cause() {
    let saved = saved_run(NORMAL).await;
    let dir = crate::run_store::runs_dir().join(&saved.run_id);
    let llm = Counting::garbage();
    // Unreadable ledger.
    std::fs::write(&saved.ledger_file, b"{ not a ledger").unwrap();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(
        why.contains("ledger") && why.contains("could not be read"),
        "{why}"
    );
    // Missing ledger.
    std::fs::remove_file(&saved.ledger_file).unwrap();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains("ledger") && why.contains("missing"), "{why}");
    // Unreadable report.
    std::fs::write(dir.join(REPORT_FILE), b"[1, 2").unwrap();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(
        why.contains("report") && why.contains("could not be read"),
        "{why}"
    );
    // Missing report.
    std::fs::remove_file(dir.join(REPORT_FILE)).unwrap();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains("report") && why.contains("missing"), "{why}");
    assert_eq!(llm.calls(), 0);
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_complete_run_is_refused_as_already_complete() {
    let saved = saved_run(NORMAL).await;
    let dir = crate::run_store::runs_dir().join(&saved.run_id);
    let path = dir.join("run.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["status"] = serde_json::json!("complete");
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let llm = Counting::garbage();
    let Err(RecheckError::AlreadyComplete(why)) = recheck(&saved, ReaskFlag::Off, &llm).await
    else {
        panic!("expected AlreadyComplete");
    };
    assert!(why.contains("already complete"), "{why}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_run_refuses_a_build_with_uncommitted_changes() {
    let saved = saved_run(NORMAL).await;
    let c = ctx(&saved);
    let mut client = inputs(&saved, &c, ReaskFlag::Off, true);
    client.pin.build.dirty = true;
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck_saved_run(&client, Some(&llm), &mut |_| {}).await
    else {
        panic!("expected a refusal");
    };
    assert!(why.contains("not committed"), "{why}");
    // An internal run is not held to it.
    let mut internal = inputs(&saved, &c, ReaskFlag::Off, false);
    internal.pin.build.dirty = true;
    assert!(recheck_saved_run(&internal, Some(&llm), &mut |_| {})
        .await
        .is_ok());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_repository_that_is_not_the_tree_the_run_read_refuses_the_delta() {
    let saved = saved_run(NORMAL).await;
    std::fs::write(
        saved.repo.path().join("src/new.ts"),
        "export const x = 1;\n",
    )
    .unwrap();
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains("changes that were not there"), "{why}");
    assert_eq!(llm.calls(), 0);
}

/// A report saved before a field existed reads with that field's default; a field the ledger
/// says the run had is not one that may be missing.
#[test]
fn a_report_in_an_older_format_loads_but_never_without_what_the_ledger_says_it_had() {
    let sealed = report_of(&[finding("SEC-X-1", "a.ts", 1)]).seal();
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("l.json");
    std::fs::write(&ledger, serde_json::to_vec(&sealed.ledger).unwrap()).unwrap();
    let write = |v: &serde_json::Value| {
        std::fs::write(dir.path().join(REPORT_FILE), serde_json::to_vec(v).unwrap()).unwrap();
    };
    let mut v = serde_json::to_value(&sealed).unwrap();
    for gone in [
        "excluded_noise_lines",
        "volume_by_repo",
        "patterns_across_services",
        "repos_not_inspected",
        "recommendations_by_repo",
    ] {
        assert!(v.as_object_mut().unwrap().remove(gone).is_some(), "{gone}");
    }
    write(&v);
    let loaded = load_saved_report(dir.path(), &ledger).expect("an older report still loads");
    assert_eq!(loaded.findings.len(), 1);
    // The embedded ledger missing: refused, never read as an empty one.
    v.as_object_mut().unwrap().remove("ledger");
    write(&v);
    let Err(RecheckError::Refused(why)) = load_saved_report(dir.path(), &ledger) else {
        panic!("expected a refusal");
    };
    assert!(why.contains("embedded ledger"), "{why}");
}

/// A saved ledger file the report's own ledger does not match is refused, naming the mismatch.
#[test]
fn a_ledger_file_that_is_not_the_reports_ledger_is_refused() {
    let sealed = report_of(&[finding("SEC-X-1", "a.ts", 1)]).seal();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(REPORT_FILE),
        serde_json::to_vec(&sealed).unwrap(),
    )
    .unwrap();
    let mut other = sealed.ledger.clone();
    other.record_rule(
        "o/r",
        "SEC-X-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        1,
        0,
    );
    let ledger = dir.path().join("l.json");
    std::fs::write(&ledger, serde_json::to_vec(&other).unwrap()).unwrap();
    let Err(RecheckError::Refused(why)) = load_saved_report(dir.path(), &ledger) else {
        panic!("expected a refusal");
    };
    assert!(why.contains("does not match"), "{why}");
}

/// The first build saves its ledger file BEFORE it seals, and the seal rebuilds the review tree
/// (which records the fix merge and the place merge in the report's ledger). A run written in
/// that order has a ledger file without those two blocks as the seal left them, and it must load.
#[test]
fn a_ledger_file_written_before_the_seal_rebuilt_the_tree_still_loads() {
    let sealed = report_of(&[finding("SEC-X-1", "a.ts", 1)]).seal();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(REPORT_FILE),
        serde_json::to_vec(&sealed).unwrap(),
    )
    .unwrap();
    let mut file: serde_json::Value = serde_json::to_value(&sealed.ledger).unwrap();
    let o = file.as_object_mut().unwrap();
    assert!(
        o.contains_key("fix_merge") && o.contains_key("place_merge"),
        "the seal recorded both blocks"
    );
    o.remove("fix_merge");
    o.remove("place_merge");
    let ledger = dir.path().join("l.json");
    std::fs::write(&ledger, serde_json::to_vec(&file).unwrap()).unwrap();
    load_saved_report(dir.path(), &ledger).expect("loads");
}

/// The ledger stage checks are the first build's: a stage failure it stored stays as saved, and a
/// stage it never checked (one added after the convention check) is not checked by the recheck.
#[test]
fn the_recheck_adds_no_check_the_first_build_never_ran() {
    let mut report = report_of(&[finding("SEC-X-1", "a.ts", 1)]);
    // A stage the first build checked and flagged, kept as saved.
    let flagged = report
        .ledger
        .record_stage("early", 1, 5, Default::default())
        .clone();
    report
        .failed_passes
        .push(crate::scan_ledger::stage_disclosure(REPO, &flagged).expect("a gap to disclose"));
    // A stage recorded after the first build's checks, with a gap it never looked at.
    report.ledger.record_stage("late", 1, 9, Default::default());
    run_integrity_checks(&mut report);
    let passes: Vec<&str> = report
        .failed_passes
        .iter()
        .map(|f| f.pass.as_str())
        .collect();
    assert_eq!(passes, ["pipeline integrity (early)"], "{passes:?}");
}

/// The client sentence, pinned exactly for each variant.
#[test]
fn the_how_this_was_done_sentence_is_exactly_the_owners_text_for_each_variant() {
    let (a, b) = ("aaaaaaaaaaaa", "bbbbbbbbbbbb");
    let base = "The findings, fix text and reviews in this inspection come from Camerata build aaaaaaaaaaaa. The final consistency checks and the export were run again on Camerata build bbbbbbbbbbbb, using those saved results unchanged.";
    let tail = " Any duplicated code it confirmed carries the rule's standard fix and is not included in Patterns across services.";
    let part = |how| RedonePart {
        repo: "acme/api".to_string(),
        rule: RULE.id().to_string(),
        candidates: 16,
        batches: 1,
        asked_again: 0,
        how,
    };
    assert_eq!(RecheckDetail::default().sentence(a, b), base);
    let read = RecheckDetail {
        clone_parts: vec![part(RedoHow::ReadStored)],
    };
    assert_eq!(
        read.sentence(a, b),
        format!("{base} The duplicated code check for 16 candidate pairs in acme/api was completed on build bbbbbbbbbbbb, by reading the model's saved answer again.{tail}")
    );
    let asked = RecheckDetail {
        clone_parts: vec![part(RedoHow::AskedAgain {
            model: "claude-haiku-4-5".to_string(),
        })],
    };
    assert_eq!(
        asked.sentence(a, b),
        format!("{base} The duplicated code check for 16 candidate pairs in acme/api was completed on build bbbbbbbbbbbb, by asking claude-haiku-4-5 once more.{tail}")
    );
}

/// The new rows go through the scan's own deterministic steps: keyed, placed, and the stages are
/// on the ledger.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_new_rows_are_collapsed_grouped_and_keyed_by_the_scans_own_steps() {
    let saved = saved_run(NORMAL).await;
    let llm = Counting::garbage();
    let done = recheck(&saved, ReaskFlag::Off, &llm).await.unwrap();
    let rows = clone_rows(&done.report);
    assert!(rows.iter().all(|r| !r.site_key.is_empty()));
    let names: Vec<&str> = done
        .report
        .ledger
        .stages()
        .iter()
        .map(|s| s.stage.as_str())
        .collect();
    assert!(
        names.contains(&"collapse-identical") && names.contains(&"group"),
        "{names:?}"
    );
    assert_eq!(done.report.ledger.total_unaccounted(), 0);
}

fn full_rows(report: &ScanReport, not_rule: bool) -> Vec<Finding> {
    report
        .findings
        .iter()
        .filter(|f| (f.rule_id != RULE.id()) == not_rule)
        .cloned()
        .collect()
}

/// Everything the recheck does not redo is what was saved, field for field: every other row, the
/// set-aside rows, the coverage notes and every failed part it does not redo. A second seal
/// changes none of it.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn everything_not_redone_is_the_saved_value_field_for_field_before_and_after_the_reseal() {
    let saved = saved_run(Spec {
        other_failure: true,
        ..NORMAL
    })
    .await;
    let dir = crate::run_store::runs_dir().join(&saved.run_id);
    let before = load_saved_report(&dir, &saved.ledger_file).unwrap();
    // Sealing the saved report again changes nothing.
    let resealed = before.clone().seal().into_inner();
    assert_eq!(full_rows(&resealed, true), full_rows(&before, true));
    assert_eq!(resealed.discarded, before.discarded);
    let llm = Counting::garbage();
    let done = recheck(&saved, ReaskFlag::Off, &llm).await.unwrap();
    let after = &done.report;
    assert_eq!(
        full_rows(after, true),
        full_rows(&before, true),
        "every other row"
    );
    assert_eq!(after.discarded, before.discarded, "the set-aside rows");
    assert_eq!(
        serde_json::to_value(&after.coverage_notes).unwrap(),
        serde_json::to_value(&before.coverage_notes).unwrap(),
        "the coverage notes"
    );
    let other = |r: &ScanReport| -> Vec<FailedPass> {
        r.failed_passes
            .iter()
            .filter(|f| f.pass == "fix text")
            .cloned()
            .collect()
    };
    assert_eq!(other(after).len(), 1, "the failed part not redone stays");
    assert_eq!(
        serde_json::to_value(other(after)).unwrap(),
        serde_json::to_value(other(&before)).unwrap()
    );
}

/// A BatchSeam over the model, as the scan's seam is built, carries the flag path.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_flag_path_works_through_a_batch_seam() {
    struct Shim(Counting);
    #[async_trait]
    impl crate::ai_audit::BatchApi for Shim {
        fn has_api_key(&self) -> bool {
            false
        }
        fn realtime(&self) -> &dyn LlmPort {
            &self.0
        }
        async fn count_input_tokens(&self, _: &LlmRequest, _: &str) -> Option<u64> {
            None
        }
        async fn submit(&self, _: Vec<crate::llm::BatchItem>) -> anyhow::Result<String> {
            anyhow::bail!("no batch")
        }
        async fn poll(&self, _: &str) -> anyhow::Result<crate::llm::BatchStatus> {
            anyhow::bail!("no batch")
        }
        async fn fetch(
            &self,
            _: &str,
            _: crate::llm::CacheTtl,
        ) -> anyhow::Result<Vec<crate::llm::BatchResultRow>> {
            anyhow::bail!("no batch")
        }
        async fn cancel_batch(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }
    let saved = saved_run(UNREADABLE).await;
    let shim = Arc::new(Shim(Counting::with(|req| disjoint_objects(&asked(req)))));
    let seam = crate::ai_audit::BatchSeam::new(shim.clone());
    let c = ctx(&saved);
    let done = recheck_saved_run(
        &inputs(&saved, &c, ReaskFlag::Allow { max_parts: 5 }, false),
        Some(&seam),
        &mut |_| {},
    )
    .await
    .expect("the one call goes through the seam");
    assert_eq!(shim.0.calls(), 1);
    assert_eq!(clone_rows(&done.report).len(), 2);
}

// ── A clone check part judged in part, batch by batch (#594) ─────────────────────────────────

/// The family id a candidate's rows carry (`captures["family"]`).
fn family_of(id: &str) -> String {
    id.strip_prefix(&format!("{}:", RULE.id()))
        .unwrap_or(id)
        .to_string()
}

/// One function whose statements follow a sequence unique to family `i`, so no two families share
/// a run of tokens long enough to be one clone candidate.
fn shaped_body(i: usize, name: &str, rate: &str) -> String {
    let shapes = [
        "  const v = order.a * user.b;\n",
        "  if (order.a > user.b) {\n    return order.c;\n  }\n",
        "  total = total + order.a - user.b;\n",
        "  for (const x of order.items) {\n    total += x.p;\n  }\n",
        "  if (user.a && !order.b) {\n    total = 0;\n  }\n",
        "  total = order.a ? user.b : order.c;\n",
    ];
    let mut s = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = format!("export function {name}(order, user) {{\n  let total = {rate};\n");
    for _ in 0..14 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.push_str(shapes[(s % 6) as usize]);
    }
    out.push_str("  return total;\n}\n");
    out
}

/// A decision written twice in each of `n` structurally different shapes, so the clone search finds
/// `n` candidates for the rule.
fn many_family_repo(n: usize) -> tempfile::TempDir {
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
    for i in 0..n {
        std::fs::create_dir_all(dir.path().join(format!("src/a{i}"))).unwrap();
        std::fs::create_dir_all(dir.path().join(format!("src/b{i}"))).unwrap();
        std::fs::write(
            dir.path().join(format!("src/a{i}/total.ts")),
            shaped_body(i, &format!("applyA{i}"), "0.2"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(format!("src/b{i}/summary.ts")),
            shaped_body(i, &format!("quoteB{i}"), "0.15"),
        )
        .unwrap();
    }
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "decisions, written twice"]);
    dir
}

/// What the first build left of the third batch of [`three_batch_run`].
#[derive(Clone, Copy, PartialEq)]
enum Third {
    /// Prose nothing reads.
    Unreadable,
    /// A reply the current reader reads, for every candidate.
    Readable,
    /// A reply the current reader reads for all but one of its candidates.
    Partial,
    /// The scan never batched the candidates this way: its verdict cache held the first candidate,
    /// so its batches are shifted by one and none of the rebuilt keys 1 to 3 has a stored call.
    Shifted,
}

/// Each request a scripted model saw: its digest and the candidate ids it asked about.
type AskLog = Vec<(String, Vec<String>)>;

struct ThreeBatches {
    saved: Saved,
    /// The candidate ids of each batch, in the order asked.
    batches: Vec<Vec<String>>,
    /// The rows the first build saved for the rule (the good batch's), as saved.
    saved_rows: Vec<Finding>,
}

/// A saved run with three judge batches for one rule, the way the scan leaves it: batch 1 was
/// answered with good verdicts and merged (one candidate judged `different`, one `unsure`, the
/// rest `same_decision`); batch 2 was answered with prose the first build's reader turned down
/// and the current one reads; batch 3 was answered with prose nothing reads, unless
/// `Third::Readable`.
async fn three_batch_run(third: Third) -> ThreeBatches {
    let state = state_dir();
    let repo = many_family_repo(22);
    let store = new_run(repo.path());
    let files = rule_files(repo.path());
    let armed: std::collections::HashSet<&str> = [RULE.id()].into_iter().collect();
    let mut set = camerata_checks::clone_rules::candidates(&files, &armed);
    let ids: Vec<String> = set.by_rule[&RULE].iter().map(|c| c.id.clone()).collect();
    if third == Third::Shifted {
        // The verdict cache served the first candidate: the scan batched the rest.
        set.by_rule.get_mut(&RULE).unwrap().remove(0);
    }
    let size = crate::onboard::clone_advisory::BATCH_SIZE;
    assert!(
        ids.len() > 2 * size + 1 && ids.len() <= 3 * size,
        "the fixture holds three batches of candidates: {}",
        ids.len()
    );
    let batches: Vec<Vec<String>> = ids.chunks(size).map(<[String]>::to_vec).collect();
    let judge_model = store.record().pin.unwrap().clone_judge_model;
    let first = if third == Third::Shifted {
        ids[1..=size].to_vec()
    } else {
        batches[0].clone()
    };
    let log: Arc<std::sync::Mutex<AskLog>> = Arc::default();
    let seen = log.clone();
    let stub = Counting::with(move |req| {
        let want = asked(req);
        seen.lock()
            .unwrap()
            .push((crate::run_store::journal::request_key(req), want.clone()));
        if want == first {
            let verdicts: Vec<serde_json::Value> = want
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let word = match i {
                        0 => "different",
                        1 => "unsure",
                        _ => "same_decision",
                    };
                    serde_json::json!({"id": id, "verdict": word, "concept": "the discount"})
                })
                .collect();
            serde_json::json!({ "verdicts": verdicts }).to_string()
        } else {
            "I cannot give verdicts for these.".to_string()
        }
    });
    let journal = JournalLlm::new(&stub, store.clone());
    let mut cache = VerdictCache::default();
    let outcome = crate::prompt_frame::with_seed(
        Some(store.nonce_seed()),
        judge_clone_candidates(
            Some(&journal),
            JudgeRequest {
                repo: REPO,
                files: &files,
                set,
                judge_model: &judge_model,
                corpus: None,
                unavailable_reason: "",
            },
            &mut cache,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(outcome.failed.len(), 1, "the judge failed in part");
    assert!(
        outcome.not_assessed.is_empty(),
        "a part judged in part saves confirmed rows"
    );
    // The first build turned the replies of batches 2 and 3 down; the current reader may read
    // them. Each stored call is matched to its batch by the ids it asked about.
    let log = log.lock().unwrap().clone();
    for mut file in call_files(&store) {
        let Some((_, want)) = log.iter().find(|(k, _)| *k == file.key) else {
            continue;
        };
        let text = if *want == batches[1] {
            Some(disjoint_objects(&batches[1]))
        } else if *want == batches[2] && third == Third::Readable {
            Some(disjoint_objects(&batches[2]))
        } else if *want == batches[2] && third == Third::Partial {
            Some(disjoint_objects(&batches[2][..batches[2].len() - 1]))
        } else {
            None
        };
        if let Some(text) = text {
            for entry in file.entries.values_mut() {
                if let Some(r) = entry.response.as_mut() {
                    r.text = text.clone();
                }
                if third == Third::Partial && *want == batches[2] {
                    // Turned down whole, so the journal asks again rather than replays it.
                    entry.status = crate::run_store::journal::EntryStatus::Rejected;
                }
            }
            std::fs::write(
                store.dir().join("calls").join(format!("{}.json", file.key)),
                serde_json::to_vec(&file).unwrap(),
            )
            .unwrap();
        }
    }
    let mut rows = vec![finding("SEC-NO-RAW-SQL-1", "src/db.ts", 20)];
    rows.extend(outcome.findings.clone());
    let mut ledger = ScanLedger::new();
    crate::onboard::clone_advisory::record_in_ledger(&mut ledger, REPO, &outcome);
    let mut report = report_of(&rows);
    report.failed_passes = outcome.failed.clone();
    report.ledger = ledger;
    let sealed = report.seal();
    // The rows as the first build saved them: sealed, with their keys.
    let saved_rows: Vec<Finding> = sealed
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE.id())
        .cloned()
        .collect();
    crate::run_store::write_report(&store.dir().join(REPORT_FILE), &sealed).unwrap();
    let ledger_file = state.0.path().join("o-r.ledger.json");
    std::fs::write(&ledger_file, serde_json::to_vec(&sealed.ledger).unwrap()).unwrap();
    finish_run(&store, &sealed, false);
    let run_id = store.run_id();
    drop(store);
    let path = crate::run_store::runs_dir().join(&run_id).join("run.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["pin"]["build"]["commit"] = serde_json::json!(BUILD_A);
    v["pin"]["build"]["dirty"] = serde_json::json!(false);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    ThreeBatches {
        saved: Saved {
            run_id,
            repo,
            ledger_file,
            _state: state,
        },
        batches,
        saved_rows,
    }
}

/// The saved files a refused recheck must leave as they were.
fn saved_bytes(saved: &Saved) -> Vec<Vec<u8>> {
    let dir = crate::run_store::runs_dir().join(&saved.run_id);
    [
        dir.join(REPORT_FILE),
        dir.join("run.json"),
        saved.ledger_file.clone(),
    ]
    .iter()
    .map(|p| std::fs::read(p).unwrap())
    .collect()
}

/// Change the saved report's rows in place, leaving everything else (the seal's inputs, the
/// ledger) as saved.
fn edit_saved_findings(dir: &std::path::Path, edit: impl FnOnce(&mut Vec<serde_json::Value>)) {
    let path = dir.join(REPORT_FILE);
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let rows = v["findings"].as_array_mut().unwrap();
    edit(rows);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
}

/// The good batch's rows after the recheck are the saved rows, field for field and in order.
fn assert_rows_as_saved(kept: &[&Finding], saved: &[Finding]) {
    assert_eq!(kept.len(), saved.len(), "the same number of rows");
    for (now, was) in kept.iter().zip(saved) {
        let (now, was) = (
            serde_json::to_value(now).unwrap(),
            serde_json::to_value(was).unwrap(),
        );
        for (key, value) in was.as_object().unwrap() {
            assert_eq!(now.get(key), Some(value), "saved field {key} changed");
        }
        assert_eq!(
            now.as_object().unwrap().len(),
            was.as_object().unwrap().len(),
            "no field added"
        );
    }
}

fn rows_of_families<'a>(report: &'a ScanReport, ids: &[String]) -> Vec<&'a Finding> {
    let fams: std::collections::HashSet<String> = ids.iter().map(|i| family_of(i)).collect();
    clone_rows(report)
        .into_iter()
        .filter(|f| {
            f.captures
                .get("family")
                .is_some_and(|fam| fams.contains(fam))
        })
        .collect()
}

/// Without the flag the unreadable batch refuses the recheck, naming it; nothing is asked and no
/// saved file changes.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_part_with_an_unreadable_batch_refuses_naming_it_and_changes_nothing() {
    let run = three_batch_run(Third::Unreadable).await;
    let before = saved_bytes(&run.saved);
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&run.saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(
        why.contains("batch 3 of 3"),
        "names the unreadable batch: {why}"
    );
    assert!(
        !why.contains("batch 1 of 3") && !why.contains("batch 2 of 3"),
        "names only the batch left: {why}"
    );
    assert!(!why.contains("Resume the run"), "{why}");
    assert_eq!(llm.calls(), 0);
    assert_eq!(saved_bytes(&run.saved), before, "saved files untouched");
}

/// With the flag: exactly one call (the unreadable batch), the good batch's saved rows are
/// byte-identical, the rejected-but-readable batch's verdicts are added with no call, and no
/// candidate has a second row.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_the_flag_one_call_is_made_good_rows_stay_and_the_readable_batch_is_added() {
    let run = three_batch_run(Third::Unreadable).await;
    let log: Arc<std::sync::Mutex<Vec<Vec<String>>>> = Arc::default();
    let seen = log.clone();
    let llm = Counting::with(move |req| {
        seen.lock().unwrap().push(asked(req));
        disjoint_objects(&asked(req))
    });
    let done = recheck(&run.saved, ReaskFlag::Allow { max_parts: 5 }, &llm)
        .await
        .expect("the recheck passes");
    assert_eq!(llm.calls(), 1, "only the unreadable batch is asked");
    assert_eq!(
        log.lock().unwrap().as_slice(),
        &[run.batches[2].clone()],
        "and it asks only about its own candidates"
    );
    let report = &done.report;
    assert!(
        report.failed_passes.is_empty(),
        "{:?}",
        report.failed_passes
    );
    // The good batch's saved rows, byte for byte.
    let kept = rows_of_families(report, &run.batches[0]);
    assert_rows_as_saved(&kept, &run.saved_rows);
    // The rejected-but-readable batch and the asked batch got their rows, once each.
    for batch in [&run.batches[1], &run.batches[2]] {
        // (The collapse may fold rows of different candidates into one survivor.)
        assert!(
            !rows_of_families(report, batch).is_empty(),
            "the batch's candidates are judged"
        );
    }
    // No candidate is merged twice: a (family, path, line) row occurs once.
    let mut seen_rows = std::collections::HashSet::new();
    for f in clone_rows(report) {
        let key = (
            f.captures.get("family").cloned(),
            f.path.clone(),
            f.line,
            f.needs_review,
        );
        assert!(seen_rows.insert(key.clone()), "a second row for {key:?}");
    }
    assert!(
        clone_rows(report)
            .iter()
            .all(|f| !matches!(f.kind, Some(FindingKind::NotAssessed(_)))),
        "the part is settled: no not-assessed row"
    );
}

/// With only a good batch and a rejected-but-readable one, the part is settled from the run's own
/// stored replies: zero calls, nothing left unfinished.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_good_batch_and_readable_ones_finish_with_zero_calls() {
    let run = three_batch_run(Third::Readable).await;
    let llm = Counting::garbage();
    let done = recheck(&run.saved, ReaskFlag::Off, &llm)
        .await
        .expect("the recheck passes");
    assert_eq!(llm.calls(), 0);
    assert!(done.report.incomplete_reasons().is_empty());
    let kept = rows_of_families(&done.report, &run.batches[0]);
    assert_rows_as_saved(&kept, &run.saved_rows);
}

/// A saved row of a candidate whose batch was never merged cannot be tied to the batches: a second
/// row would follow, so the recheck refuses rather than guess.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_saved_row_for_a_candidate_of_an_unmerged_batch_refuses_naming_the_part() {
    let run = three_batch_run(Third::Readable).await;
    let dir = crate::run_store::runs_dir().join(&run.saved.run_id);
    edit_saved_findings(&dir, |rows| {
        let mut stray = rows[1].clone();
        stray["captures"]["family"] = serde_json::json!(family_of(&run.batches[1][0]));
        rows.push(stray);
    });
    let before = saved_bytes(&run.saved);
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&run.saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains(RULE.id()) && why.contains("judged"), "{why}");
    assert!(!why.contains("Resume the run"), "{why}");
    assert_eq!(llm.calls(), 0);
    assert_eq!(saved_bytes(&run.saved), before);
}

/// A good batch whose confirmed candidate has no saved row cannot be tied either: refused.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_good_batch_whose_confirmed_candidate_has_no_saved_row_refuses() {
    let run = three_batch_run(Third::Readable).await;
    let dir = crate::run_store::runs_dir().join(&run.saved.run_id);
    let gone = family_of(&run.batches[0][5]);
    edit_saved_findings(&dir, |rows| {
        rows.retain(|r| r["captures"]["family"] != serde_json::json!(gone));
    });
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&run.saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains(RULE.id()), "{why}");
    assert_eq!(llm.calls(), 0);
}

/// A stored reply that covers all but one of a batch's candidates does not settle it: without
/// the flag the recheck refuses, naming the batch and what is left, and changes nothing.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_reply_that_misses_a_candidate_does_not_settle_its_batch() {
    let run = three_batch_run(Third::Partial).await;
    let before = saved_bytes(&run.saved);
    let llm = Counting::garbage();
    let Err(RecheckError::Refused(why)) = recheck(&run.saved, ReaskFlag::Off, &llm).await else {
        panic!("expected a refusal");
    };
    assert!(why.contains("batch 3 of 3"), "{why}");
    let n = run.batches[2].len();
    assert!(
        why.contains(&format!("{} of {n}", n - 1)),
        "names what the stored reply covers: {why}"
    );
    assert_eq!(llm.calls(), 0);
    assert_eq!(saved_bytes(&run.saved), before);
}

/// With the flag the partly read batch is asked once; a reply that is still partial refuses,
/// naming the batch and the count left, and the saved files stay as they were.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_re_asked_reply_that_is_still_partial_refuses_naming_the_count_left() {
    let run = three_batch_run(Third::Partial).await;
    let before = saved_bytes(&run.saved);
    let llm = Counting::with(|req| {
        let ids = asked(req);
        disjoint_objects(&ids[..ids.len() - 1])
    });
    let Err(RecheckError::Refused(why)) =
        recheck(&run.saved, ReaskFlag::Allow { max_parts: 5 }, &llm).await
    else {
        panic!("expected a refusal");
    };
    assert_eq!(llm.calls(), 1);
    let n = run.batches[2].len();
    assert!(
        why.contains("batch 3 of 3") && why.contains(&format!("1 of {n} candidates left")),
        "{why}"
    );
    assert_eq!(saved_bytes(&run.saved), before);
}

/// The scan batched only the candidates its verdict cache did not hold; the run does not record
/// which those were, so a batch whose rebuilt request has no stored call cannot be shown to be
/// one the scan sent. It is never asked, flag or not.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_batch_with_no_stored_call_is_never_asked() {
    let run = three_batch_run(Third::Shifted).await;
    let before = saved_bytes(&run.saved);
    let llm = Counting::with(|req| disjoint_objects(&asked(req)));
    let Err(RecheckError::Refused(why)) =
        recheck(&run.saved, ReaskFlag::Allow { max_parts: 9 }, &llm).await
    else {
        panic!("expected a refusal");
    };
    assert_eq!(llm.calls(), 0, "nothing the scan never sent is asked");
    assert!(why.contains("cannot be reproduced"), "{why}");
    assert_eq!(saved_bytes(&run.saved), before);
}
