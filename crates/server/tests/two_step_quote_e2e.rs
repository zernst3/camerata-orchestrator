#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): the two-step quote (owner ruling R-9). A run that
//! carries the pre-flight's plan quotes again once the scan stage has run, from the sites the scan
//! reported and what the scan stage cost; the re-quote reaches the cockpit job, names the spend
//! limit when its expected cost is above it, and lands on the scan ledger beside the pre-flight
//! figure and what the run actually cost.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use camerata_server::ai_audit::{self, ScanMode};
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::jobs::JobStore;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse, ProjectBackend};
use camerata_server::onboard::{self, ScanReport, SelectedRule};
use camerata_server::quote::{QuoteModelIds, QuotePlan};
use camerata_ui_core::scan::RunRepoInput;

const RULE: &str = "ARCH-QUOTE-TEST-1";
const REPO: &str = "acme/api";
/// The sites the scripted scan reports.
const SITES: usize = 12;
/// What every scripted call costs.
const CALL_USD: f64 = 0.01;

/// The scan reports [`SITES`] findings, one a line; stage 1 confirms each; anything else gets an
/// empty object. Every call costs [`CALL_USD`].
struct Stub;

fn scan_reply() -> String {
    let findings: Vec<String> = (0..SITES)
        .map(|i| {
            format!(
                r#"{{"path":"src/app.ts","line":{},"severity":"medium","rule":"{RULE}","title":"Unchecked call {i}","code":"callUnchecked{i}();","detail":"The call {i} is not checked."}}"#,
                i + 1
            )
        })
        .collect();
    format!(
        r#"{{"findings":[{}],"needs_files":[]}}"#,
        findings.join(",")
    )
}

#[async_trait]
impl LlmPort for Stub {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let system = req.system.as_deref().unwrap_or_default();
        let text = if system.starts_with(&ai_audit::verify_system_prompt()) {
            r#"{"severity":"medium","real":"yes","effort":"low","category":"correctness","reason":"confirmed"}"#
                .to_string()
        } else if system.starts_with("You are a senior software architect") {
            scan_reply()
        } else {
            "{}".to_string()
        };
        Ok(LlmResponse {
            reported_model: None,
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(CALL_USD),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
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

fn source() -> String {
    (0..SITES)
        .map(|i| format!("callUnchecked{i}();\n"))
        .collect()
}

fn stage_repo(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(dest.join("src/app.ts"), source()).unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "quote-e2e@camerata.local"]);
    g(&["config", "user.name", "Quote E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

/// The pre-flight's plan for the fixture: one file, one rule, priced models.
fn plan() -> QuotePlan {
    QuotePlan::new(
        &QuoteModelIds {
            scan: "claude-sonnet-5-5".to_string(),
            calibration: "claude-sonnet-5-5".to_string(),
            group_review: "claude-sonnet-5-5".to_string(),
            fix: Some("claude-haiku-5-5".to_string()),
            clone_judge: None,
        },
        vec![RunRepoInput {
            repo: REPO.to_string(),
            code_chars: source().len(),
            files: 1,
            selected: 1,
            multi_option_selected: 0,
            routing_groups: 1,
        }],
        ProjectBackend::Api,
        false,
        false,
        false,
    )
    .expect("the models are priced")
}

fn controls(cap: Option<f64>) -> RunControls {
    let mut efficiency = EfficiencyConfig::default();
    efficiency.set("terse_output", false).unwrap();
    RunControls {
        stored_spend_usd: 0.0,
        review: Default::default(),
        efficiency,
        max_cost_usd: cap,
        estimate_hint_usd: None,
        quote_plan: Some(plan()),
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    }
}

async fn scan(dir: &Path, cap: Option<f64>, jobs: &JobStore, jid: &str) -> ScanReport {
    let (report, _manifest) = onboard::audit_repos_with_llm_controlled(
        &Stub,
        &[(REPO.to_string(), dir.to_path_buf())],
        &[SelectedRule {
            id: RULE.to_string(),
            directive: "every call is checked".to_string(),
            repos: Vec::new(),
        }],
        Vec::new(),
        Some("claude-sonnet-5-5"),
        Some("claude-sonnet-5-5"),
        ScanMode::Sequential,
        false,
        None,
        Some((jobs, jid)),
        None,
        false,
        false,
        true,
        true,
        BackendResolution::Api,
        None,
        &HashMap::new(),
        controls(cap),
    )
    .await;
    report
}

/// R-9: the pre-flight figure, the re-quote after the scan stage and the actual cost are all on
/// the scan ledger, and the re-quote is the one the cockpit's run view was shown.
#[tokio::test]
async fn both_quotes_and_the_actual_are_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let jobs = JobStore::new();
    let jid = jobs.create("quote e2e", None);
    let report = scan(dir.path(), None, &jobs, &jid).await;

    let quotes = report.ledger.quotes().expect("the run kept its quotes");
    let want = plan();
    assert_eq!(quotes.preflight_expected_usd, want.preflight_expected_usd);
    assert_eq!(quotes.preflight_ceiling_usd, want.preflight_ceiling_usd);
    assert_eq!(quotes.requotes.len(), 1, "{quotes:?}");
    let q = &quotes.requotes[0];
    assert_eq!(q.repo, REPO);
    assert_eq!(
        q.sites, SITES,
        "the re-quote counts the sites the scan reported"
    );
    assert!(
        (q.scan_spent_usd - CALL_USD).abs() < 1e-9,
        "the scan stage was one call: {q:?}"
    );
    assert!(q.expected_usd > q.scan_spent_usd && q.ceiling_usd >= q.expected_usd);
    let actual = report.actual_usage.as_ref().unwrap().cost_usd;
    assert_eq!(quotes.actual_usd, Some(actual), "the actual is the run's");
    assert!(
        actual > q.scan_spent_usd,
        "the later stages ran after the re-quote"
    );

    let shown = jobs
        .get(&jid)
        .unwrap()
        .requote
        .expect("the run view shows it");
    assert_eq!(shown, q.line());
    assert!(
        shown.starts_with(&format!(
            "re-quote after the scan stage: about ${:.2}, at most ${:.2}",
            q.expected_usd, q.ceiling_usd
        )),
        "{shown}"
    );
    assert!(!shown.contains("spend limit"), "{shown}");
}

/// A run without a pre-flight plan makes no re-quote and keeps no quotes.
#[tokio::test]
async fn a_run_without_a_plan_makes_no_requote() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let jobs = JobStore::new();
    let jid = jobs.create("quote e2e", None);
    let mut c = controls(None);
    c.quote_plan = None;
    let (report, _) = onboard::audit_repos_with_llm_controlled(
        &Stub,
        &[(REPO.to_string(), dir.path().to_path_buf())],
        &[SelectedRule {
            id: RULE.to_string(),
            directive: "every call is checked".to_string(),
            repos: Vec::new(),
        }],
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        Some((&jobs, &jid)),
        None,
        false,
        false,
        true,
        true,
        BackendResolution::Api,
        None,
        &HashMap::new(),
        c,
    )
    .await;
    assert!(report.ledger.quotes().is_none());
    assert!(jobs.get(&jid).unwrap().requote.is_none());
}
