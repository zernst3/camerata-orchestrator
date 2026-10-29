#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): the false-negative audit, Batch B. Intake and ledger
//! items through the real brownfield pipeline (`onboard::audit_repos_with_llm_controlled`):
//!
//! * H3: a scan with a failed AI pass is never a baseline for the next one;
//! * H4: a whole-repo AI failure or block is a failed pass named `AI review (all passes)`, and a
//!   rule that ran on one repo but not another is partial, never verified clean;
//! * H11: an incremental run records what it re-audited and carried, and marks each carried
//!   finding with the commit it was last found at;
//! * H12: an AI finding under a deterministic-tier rule id is stamped as AI-produced.

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

const EMPTY: &str = r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#;
const AI_RULE: &str = "ARCH-FN-B-TEST-1";
/// A rule id the deterministic floor owns. The model reporting under it is still AI output.
const FLOOR_RULE: &str = "SEC-NO-HARDCODED-SECRETS-1";

/// How the scripted model behaves.
struct Script {
    /// The call numbers (1-based) that fail with a transport error.
    fail_calls: Vec<usize>,
    /// Every request whose prompt contains this text fails.
    fail_when_prompt_has: Option<&'static str>,
    /// The reply to a request that does not fail.
    reply: String,
    calls: AtomicUsize,
}

impl Script {
    fn ok() -> Self {
        Self::replying(EMPTY.to_string())
    }
    fn replying(reply: String) -> Self {
        Self {
            fail_calls: Vec::new(),
            fail_when_prompt_has: None,
            reply,
            calls: AtomicUsize::new(0),
        }
    }
    fn failing_calls(calls: &[usize]) -> Self {
        Self {
            fail_calls: calls.to_vec(),
            ..Self::ok()
        }
    }
    fn failing_when_prompt_has(marker: &'static str) -> Self {
        Self {
            fail_when_prompt_has: Some(marker),
            ..Self::ok()
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmPort for Script {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_calls.contains(&n) {
            anyhow::bail!("scripted transport failure on call {n}");
        }
        if let Some(marker) = self.fail_when_prompt_has {
            if req.prompt.contains(marker) {
                anyhow::bail!("scripted transport failure for {marker}");
            }
        }
        Ok(LlmResponse {
            text: self.reply.clone(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.0),
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A one-commit git repo with the given files.
fn stage_repo(dest: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let full = dest.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    git(dest, &["init", "-q", "-b", "main"]);
    git(dest, &["config", "user.email", "fn-b@camerata.local"]);
    git(dest, &["config", "user.name", "FN B"]);
    git(dest, &["add", "."]);
    git(dest, &["commit", "-q", "-m", "fixture"]);
}

fn commit_all(dest: &Path) -> String {
    git(dest, &["add", "."]);
    git(dest, &["commit", "-q", "-m", "change"]);
    git(dest, &["rev-parse", "HEAD"])
}

/// 90 rules = six rule-batches of 15 in parallel mode, over one chunk.
fn selected() -> Vec<SelectedRule> {
    let mut rules: Vec<SelectedRule> = (0..90)
        .map(|n| SelectedRule {
            id: format!("ARCH-FN-B-FILLER-{n:02}-1"),
            directive: format!("filler rule {n}"),
            repos: Vec::new(),
        })
        .collect();
    rules.push(SelectedRule {
        id: AI_RULE.to_string(),
        directive: "the rule the scripted model reports under".to_string(),
        repos: Vec::new(),
    });
    rules
}

fn controls() -> RunControls {
    RunControls {
        efficiency: EfficiencyConfig::default(),
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        consensus: false,
    }
}

async fn scan_repos(
    llm: &dyn LlmPort,
    sources: &[(String, std::path::PathBuf)],
    prior: Option<&ScanManifest>,
    backend: BackendResolution,
) -> (ScanReport, ScanManifest) {
    onboard::audit_repos_with_llm_controlled(
        llm,
        sources,
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
        backend,
        None,
        &HashMap::new(),
        controls(),
    )
    .await
}

async fn scan_one(
    llm: &dyn LlmPort,
    dir: &Path,
    prior: Option<&ScanManifest>,
) -> (ScanReport, ScanManifest) {
    let sources = vec![("acme/shop".to_string(), dir.to_path_buf())];
    scan_repos(llm, &sources, prior, BackendResolution::Api).await
}

// ── H3 ────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_scan_with_a_failed_pass_is_not_a_baseline_the_next_scan_reuses() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path(), &[("src/app.ts", "export const a = 1;\n")]);

    // Run 1: one of the seven rule-batch passes fails, and fails again when it is tried once more
    // at the end of the round (call 8); the others answer.
    let first = Script::failing_calls(&[1, 8]);
    let (report, manifest) = scan_one(&first, dir.path(), None).await;
    assert!(
        report
            .failed_passes
            .iter()
            .any(|f| f.files_not_reviewed > 0),
        "the failed pass is disclosed: {:?}",
        report.failed_passes
    );
    assert!(
        report.ai_error.is_none(),
        "not a whole-repo failure: {:?}",
        report.ai_error
    );
    assert!(
        !manifest.ai_covered,
        "a scan that left files unreviewed must not be stamped as a complete AI baseline"
    );

    // Run 2: nothing changed, the model is healthy. The incomplete baseline is refused, so the
    // AI review runs again over every file, and says why.
    let second = Script::ok();
    let (report, manifest) = scan_one(&second, dir.path(), Some(&manifest)).await;
    assert!(second.calls() > 0, "the AI review was re-run in full");
    let message = report.message.clone().unwrap_or_default();
    assert!(
        message.contains("prior scan incomplete, full AI review re-run"),
        "{message}"
    );
    assert!(
        report.failed_passes.is_empty(),
        "{:?}",
        report.failed_passes
    );
    assert!(manifest.ai_covered, "a complete run is a baseline again");

    // Run 3: the complete baseline is reused (no model call for unchanged files).
    let third = Script::ok();
    let (_report, _m) = scan_one(&third, dir.path(), Some(&manifest)).await;
    assert_eq!(third.calls(), 0, "a complete baseline is reused");
}

// ── H4 ────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_whole_repo_ai_failure_is_a_failed_pass_and_the_rule_is_partial_not_clean() {
    let ok_repo = tempfile::tempdir().unwrap();
    let bad_repo = tempfile::tempdir().unwrap();
    stage_repo(ok_repo.path(), &[("src/fine.ts", "export const a = 1;\n")]);
    stage_repo(
        bad_repo.path(),
        &[("src/broken.ts", "export const b = 2; // BAD-REPO-MARKER\n")],
    );
    let sources = vec![
        ("acme/fine".to_string(), ok_repo.path().to_path_buf()),
        ("acme/broken".to_string(), bad_repo.path().to_path_buf()),
    ];
    let llm = Script::failing_when_prompt_has("BAD-REPO-MARKER");
    let (report, _m) = scan_repos(&llm, &sources, None, BackendResolution::Api).await;

    // The failure is a row, with the real reason and the files it left unreviewed.
    let row = report
        .failed_passes
        .iter()
        .find(|f| f.repo == "acme/broken" && f.pass == "AI review (all passes)")
        .unwrap_or_else(|| panic!("no whole-repo failed pass: {:?}", report.failed_passes));
    assert_eq!(row.files_not_reviewed, 1);
    assert!(row.reason.contains("BAD-REPO-MARKER"), "{}", row.reason);
    assert!(
        !report
            .failed_passes
            .iter()
            .any(|f| f.repo == "acme/fine" && f.pass == "AI review (all passes)"),
        "the healthy repo is not listed"
    );

    // The rule ran on one repo and not the other: partial, never verified clean.
    let entry = report.ledger.rule(AI_RULE).expect("the rule is recorded");
    assert!(entry.ran, "it did run on the healthy repo");
    assert!(
        entry
            .partial_reason
            .as_deref()
            .is_some_and(|why| why.contains("acme/broken")),
        "{entry:?}"
    );
    assert!(!entry.verified_clean(), "{entry:?}");
    assert!(
        report
            .ledger
            .excluded_rules()
            .iter()
            .any(|(id, why)| *id == AI_RULE && why.contains("acme/broken")),
        "{:?}",
        report.ledger.excluded_rules()
    );
}

#[tokio::test]
async fn a_blocked_ai_review_is_a_failed_pass_for_every_repo_with_the_block_reason() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stage_repo(a.path(), &[("src/a.ts", "export const a = 1;\n")]);
    stage_repo(b.path(), &[("src/b.ts", "export const b = 2;\n")]);
    let sources = vec![
        ("acme/one".to_string(), a.path().to_path_buf()),
        ("acme/two".to_string(), b.path().to_path_buf()),
    ];
    let llm = Script::ok();
    let (report, _m) = scan_repos(
        &llm,
        &sources,
        None,
        BackendResolution::Blocked {
            message: "no model key for this project".to_string(),
        },
    )
    .await;
    assert_eq!(llm.calls(), 0, "a blocked run calls no model");
    for repo in ["acme/one", "acme/two"] {
        let row = report
            .failed_passes
            .iter()
            .find(|f| f.repo == repo && f.pass == "AI review (all passes)")
            .unwrap_or_else(|| panic!("no failed pass for {repo}: {:?}", report.failed_passes));
        assert_eq!(row.files_not_reviewed, 1, "{row:?}");
        assert!(row.reason.contains("no model key"), "{}", row.reason);
    }
}

// ── H11 ───────────────────────────────────────────────────────────────────────────────────

fn finding_reply(path: &str) -> String {
    let finding = serde_json::json!({
        "path": path,
        "line": 1,
        "severity": "high",
        "rule": AI_RULE,
        "title": "scripted finding",
        "code": "export const a = 1;",
        "detail": "the scripted model flags this line",
    });
    serde_json::json!({"findings": [finding], "proposed_rules": [], "needs_files": [], "verdicts": [], "fixes": []})
        .to_string()
}

#[tokio::test]
async fn an_incremental_run_records_what_it_carried_and_marks_each_carried_finding() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(
        dir.path(),
        &[
            ("src/a.ts", "export const a = 1;\n"),
            ("src/b.ts", "export const b = 2;\n"),
        ],
    );
    let first_sha = git(dir.path(), &["rev-parse", "HEAD"]);
    let (first_report, manifest) = scan_one(
        &Script::replying(finding_reply("src/a.ts")),
        dir.path(),
        None,
    )
    .await;
    assert!(first_report.ledger.incremental().is_none(), "a full scan");
    assert!(
        first_report.findings.iter().any(|f| f.path == "src/a.ts"),
        "the first run found the scripted finding: {:?}",
        first_report.findings
    );

    // Only b.ts changes.
    std::fs::write(dir.path().join("src/b.ts"), "export const b = 3;\n").unwrap();
    commit_all(dir.path());
    let (report, _m) = scan_one(&Script::ok(), dir.path(), Some(&manifest)).await;

    let carry = report.ledger.incremental().expect("an incremental run");
    assert_eq!(carry.changed, 1, "{carry:?}");
    assert_eq!(carry.carried_files, 1, "{carry:?}");
    assert_eq!(carry.carried_findings, 1, "{carry:?}");
    assert_eq!(carry.prior_sha.as_deref(), Some(first_sha.as_str()));

    let carried = report
        .findings
        .iter()
        .find(|f| f.path == "src/a.ts" && f.rule_id == AI_RULE)
        .expect("the finding for the unchanged file is carried");
    assert_eq!(
        camerata_server::report_export::carried_from(carried).as_deref(),
        Some(first_sha.as_str())
    );
}

#[tokio::test]
async fn a_finding_carried_twice_keeps_the_commit_it_was_last_found_at() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(
        dir.path(),
        &[
            ("src/a.ts", "export const a = 1;\n"),
            ("src/b.ts", "export const b = 2;\n"),
        ],
    );
    let first_sha = git(dir.path(), &["rev-parse", "HEAD"]);
    let (_r, m1) = scan_one(
        &Script::replying(finding_reply("src/a.ts")),
        dir.path(),
        None,
    )
    .await;
    std::fs::write(dir.path().join("src/b.ts"), "export const b = 3;\n").unwrap();
    commit_all(dir.path());
    let (_r, m2) = scan_one(&Script::ok(), dir.path(), Some(&m1)).await;
    std::fs::write(dir.path().join("src/b.ts"), "export const b = 4;\n").unwrap();
    commit_all(dir.path());
    let (report, _m3) = scan_one(&Script::ok(), dir.path(), Some(&m2)).await;
    let carried = report
        .findings
        .iter()
        .find(|f| f.path == "src/a.ts" && f.rule_id == AI_RULE)
        .expect("carried again");
    assert_eq!(
        camerata_server::report_export::carried_from(carried).as_deref(),
        Some(first_sha.as_str()),
        "the commit where the file was last audited, not the middle run"
    );
}

#[tokio::test]
async fn findings_carried_from_the_same_commit_stay_separate_rows() {
    // The carried-from mark and the producer stamp are bookkeeping. A merge stage that read
    // them as "the same captured object" would fuse unrelated findings from different files.
    let dir = tempfile::tempdir().unwrap();
    stage_repo(
        dir.path(),
        &[
            ("src/a.ts", "export const a = 1;\n"),
            ("src/b.ts", "export const b = 2;\n"),
            ("src/c.ts", "export const c = 3;\n"),
        ],
    );
    let finding = |path: &str, code: &str, title: &str| {
        serde_json::json!({
            "path": path, "line": 1, "severity": "high", "rule": AI_RULE,
            "title": title, "code": code, "detail": format!("{title} in {path}"),
        })
    };
    let reply = serde_json::json!({
        "findings": [
            finding("src/a.ts", "export const a = 1;", "first problem"),
            finding("src/b.ts", "export const b = 2;", "second problem"),
        ],
        "proposed_rules": [], "needs_files": [], "verdicts": [], "fixes": [],
    })
    .to_string();
    let (first, manifest) = scan_one(&Script::replying(reply), dir.path(), None).await;
    let fresh_rows = first
        .findings
        .iter()
        .filter(|f| f.rule_id == AI_RULE)
        .count();
    assert_eq!(fresh_rows, 2, "{:?}", first.findings);

    std::fs::write(dir.path().join("src/c.ts"), "export const c = 4;\n").unwrap();
    commit_all(dir.path());
    let (report, _m) = scan_one(&Script::ok(), dir.path(), Some(&manifest)).await;
    let carried: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == AI_RULE)
        .collect();
    assert_eq!(carried.len(), 2, "{carried:?}");
    assert!(carried
        .iter()
        .all(|f| camerata_server::report_export::carried_from(f).is_some()));
    assert!(carried.iter().all(|f| f.also_locations.is_empty()));
}

// ── H12 ───────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_ai_finding_under_a_floor_rule_id_is_stamped_as_ai_produced() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path(), &[("src/a.ts", "export const a = 1;\n")]);
    let mut rules = selected();
    rules.push(SelectedRule {
        id: FLOOR_RULE.to_string(),
        directive: "no hardcoded secrets".to_string(),
        repos: Vec::new(),
    });
    let finding = serde_json::json!({
        "path": "src/a.ts", "line": 1, "severity": "high", "rule": FLOOR_RULE,
        "title": "a secret", "code": "export const a = 1;", "detail": "model-found",
    });
    let reply = serde_json::json!({"findings": [finding], "proposed_rules": [], "needs_files": [], "verdicts": [], "fixes": []}).to_string();
    let llm = Script::replying(reply);
    let sources = vec![("acme/shop".to_string(), dir.path().to_path_buf())];
    let (report, _m) = onboard::audit_repos_with_llm_controlled(
        &llm,
        &sources,
        &rules,
        Vec::new(),
        None,
        None,
        ScanMode::Parallel,
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
        controls(),
    )
    .await;
    let f = report
        .findings
        .iter()
        .find(|f| f.rule_id == FLOOR_RULE)
        .unwrap_or_else(|| panic!("the model's finding is reported: {:?}", report.findings));
    assert_eq!(
        f.captures
            .get(onboard::PRODUCED_TIER_CAPTURE)
            .map(String::as_str),
        Some("semantic"),
        "{:?}",
        f.captures
    );
}
