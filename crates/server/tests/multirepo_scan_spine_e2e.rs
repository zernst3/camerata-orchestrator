#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): a scan over SEVERAL repos where one repo succeeds
//! and another does not. The aggregates that sit above the per-repo loop must keep the two apart:
//!
//! * the rule ledger is kept per (rule, repo), so a rule that ran on one repo and failed on
//!   another is never "verified clean" and never in "What's healthy";
//! * a repo that cannot be read is a failed pass, not-run ledger rows and a line on the report,
//!   never silently absent, and the release gate refuses a package that covers fewer repos than
//!   the project;
//! * a repo the spend limit never reached, and the chunks it refused, are not recorded as audited
//!   in the incremental manifest;
//! * the whole-repo AI failure of one repo is a structured failed pass for that repo.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata_server::ai_audit::ScanMode;
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, ScanReport, SelectedRule};
use camerata_server::report_export::coverage_gaps::{
    repos_without_ai_review, AI_REVIEW_ALL_PASSES,
};
use camerata_server::scan_cache::ScanManifest;

const RULE: &str = "ARCH-MULTI-TEST-1";
const FAIL_MARKER: &str = "FAIL_THIS_REPO_MARKER";

/// Answers every call with an empty, well-formed result and a fixed cost; fails every call whose
/// prompt carries [`FAIL_MARKER`], which only one repo's file does.
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
        if req.prompt.contains(FAIL_MARKER) {
            anyhow::bail!("the model refused this repository");
        }
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

fn stage_repo(dest: &Path, body: &str) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(dest.join("src/app.ts"), body).unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "multi-e2e@camerata.local"]);
    g(&["config", "user.name", "Multi E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn selected() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE.to_string(),
        directive: "multi-repo e2e rule".to_string(),
        repos: Vec::new(),
    }]
}

fn controls(cap: Option<f64>) -> RunControls {
    RunControls {
        efficiency: EfficiencyConfig::default(),
        max_cost_usd: cap,
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    }
}

async fn scan(
    llm: &dyn LlmPort,
    sources: &[(String, std::path::PathBuf)],
    cap: Option<f64>,
) -> (ScanReport, ScanManifest) {
    onboard::audit_repos_with_llm_controlled(
        llm,
        sources,
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
        controls(cap),
    )
    .await
}

fn two_repos(a: &Path, b: &Path) -> Vec<(String, std::path::PathBuf)> {
    vec![
        ("acme/one".to_string(), a.to_path_buf()),
        ("acme/two".to_string(), b.to_path_buf()),
    ]
}

// ── H1 + M3: the AI review fails on one repo ────────────────────────────────────────────

#[tokio::test]
async fn a_rule_that_ran_on_one_repo_and_failed_on_another_is_never_verified_clean() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stage_repo(a.path(), "export const a = 1;\n");
    stage_repo(
        b.path(),
        &format!("// {FAIL_MARKER}\nexport const b = 2;\n"),
    );

    let (report, manifest) = scan(&Stub::new(0.01), &two_repos(a.path(), b.path()), None).await;

    // The ledger holds two facts for the rule, one per repo.
    let one = report.ledger.rule_for_repo(RULE, "acme/one").unwrap();
    let two = report.ledger.rule_for_repo(RULE, "acme/two").unwrap();
    assert!(one.ran && one.findings_emitted == 0);
    assert!(!two.ran);
    assert!(
        two.skip_reason
            .as_deref()
            .unwrap()
            .contains("audit pass failed"),
        "{two:?}"
    );

    // The rule is not clean for the project, and says which repo it missed.
    assert!(
        !report.ledger.healthy_rule_ids().contains(&RULE.to_string()),
        "a rule that failed on a repo is not in What's healthy"
    );
    let merged = report.ledger.rule(RULE).unwrap();
    assert!(merged.ran, "it did run on acme/one");
    assert!(!merged.verified_clean());
    assert_eq!(merged.repos_not_run, vec!["acme/two".to_string()]);
    let (_, why) = report
        .ledger
        .excluded_rules()
        .into_iter()
        .find(|(id, _)| *id == RULE)
        .expect("listed as excluded");
    assert!(why.contains("acme/two"), "{why}");

    // M3: the failure is a structured failed pass for that repo, not only a message.
    assert!(report
        .failed_passes
        .iter()
        .any(|f| f.repo == "acme/two" && f.pass == AI_REVIEW_ALL_PASSES));
    assert!(!report
        .failed_passes
        .iter()
        .any(|f| f.repo == "acme/one" && f.pass == AI_REVIEW_ALL_PASSES));
    assert!(report.ai_error.as_deref().unwrap().contains("acme/two"));
    assert_eq!(repos_without_ai_review(&report), vec!["acme/two"]);

    // The integrity gate fails the rule as incomplete coverage.
    let verdict = camerata_server::integrity_gate::evaluate(
        &report.ledger,
        &camerata_server::integrity_gate::IntegrityGateOptions::smoke(),
    );
    assert!(
        verdict
            .failures
            .iter()
            .any(|f| f.code == "partial_coverage" && f.detail.contains("acme/two")),
        "{:?}",
        verdict.failures
    );

    // H3: the repo whose review failed is not recorded as audited.
    assert!(manifest
        .files
        .get("acme/one")
        .is_some_and(|f| !f.is_empty()));
    assert!(
        manifest.files.get("acme/two").is_none_or(|f| f.is_empty()),
        "{:?}",
        manifest.files
    );
    assert!(!manifest.ai_covered);
}

#[tokio::test]
async fn every_repo_failing_the_same_way_is_counted_in_the_error() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stage_repo(a.path(), &format!("// {FAIL_MARKER}\n"));
    stage_repo(b.path(), &format!("// {FAIL_MARKER}\n"));
    let (report, _m) = scan(&Stub::new(0.01), &two_repos(a.path(), b.path()), None).await;
    let error = report.ai_error.as_deref().unwrap();
    assert!(error.contains("1 other repository"), "{error}");
    let failed: Vec<_> = report
        .failed_passes
        .iter()
        .filter(|f| f.pass == AI_REVIEW_ALL_PASSES)
        .map(|f| f.repo.as_str())
        .collect();
    assert_eq!(failed, vec!["acme/one", "acme/two"]);
}

// ── H2: a repo that cannot be read ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_repo_that_cannot_be_read_is_on_the_report_and_blocks_the_package() {
    let a = tempfile::tempdir().unwrap();
    stage_repo(a.path(), "export const a = 1;\n");
    // A folder that is gone: the walk cannot read it.
    let gone = a.path().join("no-longer-here");
    let sources = vec![
        ("acme/one".to_string(), a.path().to_path_buf()),
        ("acme/gone".to_string(), gone),
    ];
    let (report, manifest) = scan(&Stub::new(0.01), &sources, None).await;

    // Not listed as inspected; listed as not inspected, with a reason.
    assert_eq!(report.repos, vec!["acme/one".to_string()]);
    assert_eq!(
        report.repos_not_inspected.len(),
        1,
        "{:?}",
        report.repos_not_inspected
    );
    assert_eq!(report.repos_not_inspected[0].0, "acme/gone");
    assert!(!report.repos_not_inspected[0].1.is_empty());
    assert!(report.failed_passes.iter().any(|f| f.repo == "acme/gone"
        && f.pass == onboard::REPOSITORY_READ_PASS
        && !f.reason.is_empty()));
    assert!(!report.ai_review_complete());

    // Every rule that applies to it is on record as not run for that repo.
    let gone = report.ledger.rule_for_repo(RULE, "acme/gone").unwrap();
    assert!(!gone.ran);
    assert!(gone
        .skip_reason
        .as_deref()
        .unwrap()
        .contains("repository could not be read"));
    let floor = report
        .ledger
        .rule_for_repo(onboard::AUDIT_RULES[0], "acme/gone")
        .unwrap();
    assert!(!floor.ran);
    assert!(!report.ledger.healthy_rule_ids().contains(&RULE.to_string()));
    assert!(!report
        .ledger
        .healthy_rule_ids()
        .contains(&onboard::AUDIT_RULES[0].to_string()));
    assert!(manifest.files.get("acme/gone").is_none_or(|f| f.is_empty()));

    // The release gate refuses a package for fewer repos than the project holds.
    let options = camerata_server::report_export::ReportOptions {
        client_name: "Acme Corp".to_string(),
        ..Default::default()
    };
    let refused = camerata_server::report_export::require_all_repos(&report, &options)
        .expect_err("a report that skipped a repo is not packaged");
    assert_eq!(refused.repos, vec!["acme/gone".to_string()]);
    let text = refused.to_string();
    assert!(text.contains("1 of 2 repositories"), "{text}");
    assert!(text.contains("acme/gone"), "{text}");
}

#[tokio::test]
async fn the_release_gate_also_refuses_a_project_repo_the_report_does_not_list() {
    let a = tempfile::tempdir().unwrap();
    stage_repo(a.path(), "export const a = 1;\n");
    let sources = vec![("acme/one".to_string(), a.path().to_path_buf())];
    let (report, _m) = scan(&Stub::new(0.01), &sources, None).await;
    assert!(report.repos_not_inspected.is_empty());

    let mut options = camerata_server::report_export::ReportOptions {
        client_name: "Acme Corp".to_string(),
        ..Default::default()
    };
    // No project named: nothing to compare against.
    assert!(camerata_server::report_export::require_all_repos(&report, &options).is_ok());
    options.expected_repos = vec!["acme/one".to_string()];
    assert!(camerata_server::report_export::require_all_repos(&report, &options).is_ok());
    options.expected_repos = vec!["acme/one".to_string(), "acme/two".to_string()];
    let refused = camerata_server::report_export::require_all_repos(&report, &options)
        .expect_err("the project holds a repo the report leaves out");
    assert_eq!(refused.repos, vec!["acme/two".to_string()]);
}

// ── H3 + M2: the spend limit ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_repo_the_spend_limit_never_reached_is_not_recorded_as_audited() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stage_repo(a.path(), "export const a = 1;\n");
    stage_repo(b.path(), "export const b = 2;\n");
    let llm = Stub::new(10.0);
    let (report, manifest) = scan(&llm, &two_repos(a.path(), b.path()), Some(5.0)).await;

    // Repo one's single pass is admitted and costs $10; repo two's model work never starts.
    assert_eq!(llm.calls(), 1);
    let cap = report.ledger.cost_cap().unwrap();
    assert!(cap.stopped_the_run());
    assert_eq!(cap.repos_not_finished, vec!["acme/two".to_string()]);
    assert_eq!(cap.unfinished_repos(), vec!["acme/two".to_string()]);

    // M2: the disclosure counts the repo's files as not reviewed.
    let fp = report
        .failed_passes
        .iter()
        .find(|f| f.repo == "acme/two" && f.pass == AI_REVIEW_ALL_PASSES)
        .expect("repo two has a failed pass");
    assert!(fp.reason.contains("NOT RUN"), "{}", fp.reason);
    assert_eq!(fp.files_not_reviewed, 1);

    // The rule is not clean: it was never run on repo two.
    assert!(!report.ledger.healthy_rule_ids().contains(&RULE.to_string()));
    assert!(!report.ledger.rule_for_repo(RULE, "acme/two").unwrap().ran);

    // H3: the manifest knows repo one's file and not repo two's.
    assert_eq!(
        manifest.files.get("acme/one").map(|f| f.len()),
        Some(1),
        "{:?}",
        manifest.files
    );
    assert!(
        manifest.files.get("acme/two").is_none_or(|f| f.is_empty()),
        "{:?}",
        manifest.files
    );
    assert!(!manifest.ai_covered);
}

/// Three ~150K-character files = two chunks (350K per chunk).
fn stage_two_chunk_repo(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    for f in 0..3 {
        let body: String = (0..6_000)
            .map(|i| format!("export const v{f}_{i} = {i};\n"))
            .collect();
        std::fs::write(dest.join(format!("src/big{f}.ts")), body).unwrap();
    }
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "multi-e2e@camerata.local"]);
    g(&["config", "user.name", "Multi E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

#[tokio::test]
async fn files_of_a_chunk_the_spend_limit_refused_are_not_recorded_as_audited() {
    let dir = tempfile::tempdir().unwrap();
    stage_two_chunk_repo(dir.path());
    let sources = vec![("acme/shop".to_string(), dir.path().to_path_buf())];
    let llm = Stub::new(1.0);
    let (report, manifest) = scan(&llm, &sources, Some(0.5)).await;

    // Chunk one ran; chunk two was refused, and the refusal names its files.
    assert_eq!(llm.calls(), 1);
    let cap = report.ledger.cost_cap().unwrap();
    assert_eq!(cap.not_run_passes.len(), 1);
    let refused_files = &cap.not_run_passes[0].files;
    assert!(!refused_files.is_empty(), "{:?}", cap.not_run_passes);
    let recorded = manifest.files.get("acme/shop").unwrap();
    for path in refused_files {
        assert!(
            !recorded.contains_key(path),
            "{path} was not reviewed and must not be recorded as audited: {recorded:?}"
        );
    }
    assert_eq!(
        recorded.len() + refused_files.len(),
        3,
        "the files that were reviewed are recorded"
    );
    assert!(!manifest.ai_covered);
    // The disclosure counts what was not reviewed.
    assert!(report
        .failed_passes
        .iter()
        .any(|f| f.reason.contains("NOT RUN") && f.files_not_reviewed == refused_files.len()));
}

// ── H5: one recommendation per (rule, repo) ─────────────────────────────────────────────

#[tokio::test]
async fn a_repo_with_no_cap_is_never_blocked() {
    // The guard in the other direction: no cap, both repos run.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stage_repo(a.path(), "export const a = 1;\n");
    stage_repo(b.path(), "export const b = 2;\n");
    let llm = Stub::new(10.0);
    let (report, manifest) = scan(&llm, &two_repos(a.path(), b.path()), None).await;
    assert_eq!(llm.calls(), 2);
    assert!(report.repos_not_inspected.is_empty());
    assert!(report.ledger.healthy_rule_ids().contains(&RULE.to_string()));
    assert!(manifest.ai_covered);
    assert_eq!(manifest.files.len(), 2);
    // The ledger has one entry per repo for the rule, and stages name their repo.
    assert!(report.ledger.rule_for_repo(RULE, "acme/one").unwrap().ran);
    assert!(report.ledger.rule_for_repo(RULE, "acme/two").unwrap().ran);
    assert_eq!(report.ledger.repos(), vec!["acme/one", "acme/two"]);
    assert!(report
        .ledger
        .stages()
        .iter()
        .filter(|s| s.stage == "cross-family-merge")
        .all(|s| s.repo == "acme/one" || s.repo == "acme/two"));
}
