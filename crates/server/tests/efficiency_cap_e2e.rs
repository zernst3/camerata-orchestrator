#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): the efficiency switches and the spend cap through the
//! real brownfield pipeline (`onboard::audit_repos_with_llm_controlled`).
//!
//! * the set of measures that were on is recorded on the operator ledger and in the telemetry
//!   table, and a switch changes behavior (`result_cache` off forces a full AI re-scan);
//! * a capped run stops between passes, names every (chunk, rule-batch) it did not run in the
//!   failed-pass list, records the unchecked rules as NOT run in the ledger, and says nothing
//!   about dollars to the client; a run under its cap is complete.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata_server::ai_audit::ScanMode;
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, ScanReport, SelectedRule};
use camerata_server::scan_cache::ScanManifest;

/// Answers every call with an empty, well-formed result and a fixed cost.
struct Stub {
    cost: f64,
    calls: AtomicUsize,
}

impl Stub {
    fn new(cost: f64) -> Self {
        Self {
            cost,
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmPort for Stub {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(self.cost),
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

fn stage_repo(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(dest.join("src/app.ts"), "export const a = 1;\n").unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "cap-e2e@camerata.local"]);
    g(&["config", "user.name", "Cap E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

/// 90 rules = six rule-batches of 15 in parallel mode, over one chunk.
fn selected() -> Vec<SelectedRule> {
    (0..90)
        .map(|n| SelectedRule {
            id: format!("ARCH-CAP-TEST-{n:02}-1"),
            directive: format!("cap e2e rule {n}"),
            repos: Vec::new(),
        })
        .collect()
}

async fn scan(
    llm: &dyn LlmPort,
    dir: &Path,
    prior: Option<&ScanManifest>,
    controls: RunControls,
) -> (ScanReport, ScanManifest) {
    let sources = vec![("acme/shop".to_string(), dir.to_path_buf())];
    onboard::audit_repos_with_llm_controlled(
        llm,
        &sources,
        &selected(),
        Vec::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        prior,
        false,
        false,
        true,
        true,
        BackendResolution::Api,
        None,
        &HashMap::new(),
        controls,
    )
    .await
}

fn controls(cap: Option<f64>, efficiency: EfficiencyConfig) -> RunControls {
    RunControls {
        efficiency,
        max_cost_usd: cap,
        estimate_hint_usd: None,
    }
}

#[tokio::test]
async fn the_ledger_and_the_telemetry_state_which_measures_were_on() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let mut efficiency = EfficiencyConfig::default();
    efficiency.set("warm_schedule", false).unwrap();
    efficiency.set("fix_prefilter", false).unwrap();

    let (report, _m) = scan(
        &Stub::new(0.01),
        dir.path(),
        None,
        controls(None, efficiency),
    )
    .await;

    assert_eq!(report.ledger.efficiency(), Some(&efficiency));
    let telemetry = report.ledger.stage_telemetry().expect("a model ran");
    assert_eq!(telemetry.efficiency, Some(efficiency));
    let table = telemetry.render_table();
    assert!(table.contains("warm_schedule=off"), "{table}");
    assert!(table.contains("fix_prefilter=off"), "{table}");
    assert!(table.contains("result_cache=on"), "{table}");

    // The ledger is persisted as JSON; the set survives the round trip.
    let back: camerata_server::scan_ledger::ScanLedger =
        serde_json::from_str(&serde_json::to_string(&report.ledger).unwrap()).unwrap();
    assert_eq!(back.efficiency(), Some(&efficiency));
}

#[tokio::test]
async fn result_cache_off_forces_a_full_ai_rescan_that_the_default_would_have_skipped() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let first = Stub::new(0.01);
    let (_r, manifest) = scan(
        &first,
        dir.path(),
        None,
        controls(None, EfficiencyConfig::default()),
    )
    .await;
    assert!(first.calls() > 0);

    let reuse = Stub::new(0.01);
    let (report, _m) = scan(
        &reuse,
        dir.path(),
        Some(&manifest),
        controls(None, EfficiencyConfig::default()),
    )
    .await;
    assert_eq!(
        reuse.calls(),
        0,
        "unchanged tree, default: nothing is re-asked"
    );
    assert!(
        report
            .message
            .as_deref()
            .unwrap_or("")
            .contains("fully cached"),
        "{:?}",
        report.message
    );

    let mut off = EfficiencyConfig::default();
    off.set("result_cache", false).unwrap();
    let rescan = Stub::new(0.01);
    let (report, _m) = scan(&rescan, dir.path(), Some(&manifest), controls(None, off)).await;
    assert!(rescan.calls() > 0, "switch off: the AI tier runs again");
    assert!(
        report
            .message
            .as_deref()
            .unwrap_or("")
            .contains("result cache switched off"),
        "{:?}",
        report.message
    );
}

#[tokio::test]
async fn a_capped_scan_stops_between_passes_and_says_exactly_what_it_did_not_run() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Stub::new(1.0);
    let (report, _m) = scan(
        &llm,
        dir.path(),
        None,
        controls(Some(2.5), EfficiencyConfig::default()),
    )
    .await;

    // pass 1 alone ($1), then passes are admitted while spent + in_flight * $1 < $2.5: two more.
    assert_eq!(llm.calls(), 3);
    let usage = report.actual_usage.as_ref().unwrap();
    assert!(
        usage.cost_usd <= 2.5 + 1.0,
        "within one pass of the cap: {usage:?}"
    );

    assert!(report.ledger.is_cost_capped());
    let cap = report.ledger.cost_cap().unwrap();
    assert_eq!(cap.not_run_passes.len(), 3);

    // The client-visible disclosure: one failed-pass entry, NOT RUN, no dollars.
    let not_run: Vec<_> = report
        .failed_passes
        .iter()
        .filter(|f| f.reason.contains("NOT RUN"))
        .collect();
    assert_eq!(not_run.len(), 1, "{:?}", report.failed_passes);
    assert!(not_run[0].pass.contains("chunk 1/1"), "{}", not_run[0].pass);
    assert!(
        not_run[0].reason.contains("rule-batch 4, 5, 6 of 6"),
        "{}",
        not_run[0].reason
    );
    assert!(
        not_run[0].reason.contains("45 rule check"),
        "{}",
        not_run[0].reason
    );
    assert!(!not_run[0].reason.contains('$'));

    // The ledger does not call a rule that never ran "ran, found nothing".
    let ran_batches = ["ARCH-CAP-TEST-00-1", "ARCH-CAP-TEST-44-1"];
    for id in ran_batches {
        assert!(
            report.ledger.rule(id).unwrap().ran,
            "{id} was in a pass that ran"
        );
    }
    for id in ["ARCH-CAP-TEST-45-1", "ARCH-CAP-TEST-89-1"] {
        let entry = report.ledger.rule(id).unwrap();
        assert!(!entry.ran, "{id} was never checked");
        assert!(
            entry
                .skip_reason
                .as_deref()
                .unwrap_or("")
                .contains("spend limit"),
            "{entry:?}"
        );
    }
}

#[tokio::test]
async fn a_scan_under_its_cap_is_complete() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Stub::new(0.01);
    let (report, _m) = scan(
        &llm,
        dir.path(),
        None,
        controls(Some(50.0), EfficiencyConfig::default()),
    )
    .await;
    assert!(!report.ledger.is_cost_capped());
    assert!(report
        .failed_passes
        .iter()
        .all(|f| !f.reason.contains("NOT RUN")));
    assert!(report.ledger.rule("ARCH-CAP-TEST-89-1").unwrap().ran);
}

#[tokio::test]
async fn a_later_repo_after_the_cap_is_reached_runs_nothing_and_is_recorded() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stage_repo(a.path());
    stage_repo(b.path());
    let sources = vec![
        ("acme/one".to_string(), a.path().to_path_buf()),
        ("acme/two".to_string(), b.path().to_path_buf()),
    ];
    let llm = Stub::new(10.0);
    let (report, _m) = onboard::audit_repos_with_llm_controlled(
        &llm,
        &sources,
        &selected(),
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        false,
        false,
        true,
        true,
        BackendResolution::Api,
        None,
        &HashMap::new(),
        controls(Some(5.0), EfficiencyConfig::default()),
    )
    .await;
    // Repo one's single pass is admitted (nothing spent) and costs $10; repo two's is refused.
    assert_eq!(llm.calls(), 1);
    assert!(report.ledger.is_cost_capped());
    let cap = report.ledger.cost_cap().unwrap();
    assert!(cap.not_run_passes.iter().any(|p| p.repo == "acme/two"));
    assert!(report
        .failed_passes
        .iter()
        .any(|f| f.repo == "acme/two" && f.reason.contains("NOT RUN")));
}
