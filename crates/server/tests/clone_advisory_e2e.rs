#![allow(clippy::unwrap_used)]
//! END-TO-END: the Wave C clone rules through the REAL brownfield pipeline
//! (`onboard::audit_repos_with_llm`: floor, architectural checkers, the clone advisory seam, the
//! semantic pass, calibration, merge, severity bounds, the ledger, provenance and the fresh scan
//! manifest) with a SCRIPTED model. ZERO API SPEND: the stub never touches the network, and the
//! offline scenarios hand it a model that fails the test if it is called at all.
//!
//! The repo has one application decision, a premium discount, written twice with different
//! rates. The stub answers the clone judge's closed question and gives the semantic and
//! calibration passes an empty answer.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata_api_types::audit::{FindingKind, NotAssessedReason};
use camerata_server::ai_audit::ScanMode;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::clone_advisory::resolve_clone_judge_model;
use camerata_server::onboard::{self, Finding, ScanReport, SelectedRule};
use camerata_server::scan_cache::ScanManifest;
use camerata_server::scan_ledger::{ADVISORY_PARTIAL_NOTE, ADVISORY_UNAVAILABLE_NOTE};
use camerata_server::transcript::TranscriptStore;

const SINGLE: &str = "ARCH-SINGLE-SOURCE-LOGIC-1";
const NAMING: &str = "ARCH-NAMING-ONE-CONCEPT-1";
const UI: &str = "UI-SHARED-PRIMITIVE-1";

/// The clone judge's calls are the only ones carrying this phrase in the system prompt.
const JUDGE_MARKER: &str = "exactly one verdict word";

/// A model that answers the clone judge per rule and everything else with an empty answer.
struct Scripted {
    judge_calls: AtomicUsize,
    other_calls: AtomicUsize,
    models: std::sync::Mutex<Vec<String>>,
    /// Answer each rule's question with these words (single-source, naming).
    single_word: &'static str,
    naming_word: &'static str,
}

impl Scripted {
    fn new(single_word: &'static str, naming_word: &'static str) -> Self {
        Self {
            judge_calls: AtomicUsize::new(0),
            other_calls: AtomicUsize::new(0),
            models: std::sync::Mutex::new(Vec::new()),
            single_word,
            naming_word,
        }
    }
}

fn asked_ids(req: &LlmRequest) -> Vec<String> {
    let json = req
        .prompt
        .split_once("Candidates (JSON):\n")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let items: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i["id"].as_str().map(str::to_string))
        .collect()
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let system = req.system.clone().unwrap_or_default();
        let text = if system.contains(JUDGE_MARKER) {
            self.judge_calls.fetch_add(1, Ordering::SeqCst);
            self.models.lock().unwrap().push(req.model.clone());
            let word = if system.contains("same_decision") {
                self.single_word
            } else if system.contains("same_concept") {
                self.naming_word
            } else {
                "unsure"
            };
            let items: Vec<serde_json::Value> = asked_ids(&req)
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id,
                        "verdict": word,
                        "concept": if word.starts_with("same") { "the premium discount calculation" } else { "" },
                    })
                })
                .collect();
            serde_json::json!({ "verdicts": items }).to_string()
        } else {
            self.other_calls.fetch_add(1, Ordering::SeqCst);
            r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string()
        };
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.001),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        })
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let response = self.complete(req).await?;
        on_delta(&response.text);
        Ok(response)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn discount(func: &str, rate: &str) -> String {
    format!(
        "import {{ money }} from './money';
export function {func}(order, user) {{
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

/// Write the repo into `dest` and make it a one-commit git repo (the audited-ref capture shells
/// out to git).
fn stage(dest: &Path, files: &[(&str, String)]) {
    for (path, content) in files {
        let full = dest.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "clone-e2e@camerata.local"]);
    g(&["config", "user.name", "Clone E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn duplicated_discount_repo(dest: &Path) {
    stage(
        dest,
        &[
            ("src/cart/total.ts", discount("applyDiscount", "0.2")),
            ("src/checkout/summary.ts", discount("quoteTotal", "0.15")),
        ],
    );
}

fn selected() -> Vec<SelectedRule> {
    [SINGLE, NAMING, UI]
        .into_iter()
        .map(|id| SelectedRule {
            id: id.to_string(),
            directive: format!("clone advisory e2e: {id}"),
            repos: Vec::new(),
        })
        .collect()
}

async fn corpus() -> camerata_rules::RuleSet {
    camerata_rules::load_corpus(&camerata_rules::corpus_path())
        .await
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
async fn scan(
    llm: &dyn LlmPort,
    dir: &Path,
    corpus: &camerata_rules::RuleSet,
    prior: Option<&ScanManifest>,
    run_ai_review: bool,
    backend: BackendResolution,
    store: Option<&TranscriptStore>,
) -> (ScanReport, ScanManifest) {
    let sources = vec![("acme/shop".to_string(), dir.to_path_buf())];
    onboard::audit_repos_with_llm(
        llm,
        &sources,
        &selected(),
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        store.map(|s| (s, "scan-key")),
        None,
        prior,
        false,
        false,
        run_ai_review,
        true,
        backend,
        Some(corpus),
        &HashMap::new(),
    )
    .await
}

fn under<'a>(report: &'a ScanReport, id: &str) -> Vec<&'a Finding> {
    report.findings.iter().filter(|f| f.rule_id == id).collect()
}

#[tokio::test]
async fn a_judged_scan_ships_the_confirmed_finding_and_stamps_the_judge() {
    let dir = tempfile::tempdir().unwrap();
    duplicated_discount_repo(dir.path());
    let corpus = corpus().await;
    let llm = Scripted::new("same_decision", "different_concept");
    let store = TranscriptStore::new();
    let (report, manifest) = scan(
        &llm,
        dir.path(),
        &corpus,
        None,
        true,
        BackendResolution::Api,
        Some(&store),
    )
    .await;

    // The confirmed decision ships, high because the rates differ, with both sites and values.
    let single = under(&report, SINGLE);
    assert_eq!(single.len(), 1, "{:#?}", report.findings);
    let f = single[0];
    assert!(matches!(f.kind, Some(FindingKind::Defect)));
    assert_eq!(f.severity, "high");
    assert!(f.preview, "a deterministic-tier row");
    assert!(f.detail.contains("do not agree"), "{}", f.detail);
    assert!(
        f.detail.contains("`0.2`") && f.detail.contains("`0.15`"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("src/cart/total.ts") && f.detail.contains("src/checkout/summary.ts"));
    assert_eq!(f.also_locations.len() + 1, 2);
    // The naming candidate was judged a different concept: no defect. The rule ran completely
    // and found nothing to report, so it states the convention it observed instead.
    let naming = under(&report, NAMING);
    assert_eq!(naming.len(), 1, "{naming:#?}");
    assert!(matches!(
        naming[0].kind,
        Some(FindingKind::ConventionObserved(_))
    ));
    assert!(
        naming[0]
            .detail
            .contains("1 candidate sites reviewed and judged not to be the same concept"),
        "{}",
        naming[0].detail
    );

    // The ledger credits both rules as having run; the semantic pass read them too.
    for id in [SINGLE, NAMING] {
        let entry = report
            .ledger
            .rule(id)
            .unwrap_or_else(|| panic!("{id} in the ledger"));
        assert!(entry.ran, "{id}: {entry:?}");
    }
    assert_eq!(report.ledger.rule(SINGLE).unwrap().findings_emitted, 1);

    // Provenance names the judge model: the fast tier, not the audit model.
    assert_eq!(
        report.provenance.clone_judge_model.as_deref(),
        Some(resolve_clone_judge_model(None).as_str())
    );
    assert_ne!(
        report.provenance.clone_judge_model,
        report.provenance.audit_model
    );
    assert_eq!(
        llm.models
            .lock()
            .unwrap()
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1
    );
    // The judge's spend is in the same meter as every other pass.
    assert!(report.actual_usage.as_ref().unwrap().calls >= 2);

    // The transcript shows the candidate list and the verdicts.
    let agents = store.get("scan-key");
    let judge = agents
        .iter()
        .find(|a| a.session_id == format!("clone-judge:{SINGLE}"))
        .expect("a clone-judge transcript for the rule");
    assert!(judge.prompt.contains("Ranked candidates:"));
    assert!(judge.output.contains("same_decision"));

    // The exporter takes the row as a real, actionable finding: not held for want of a fix, and
    // carrying the rule's authored Fix line.
    let json = camerata_server::report_export::build_report_json(
        &report.clone().seal(),
        &HashMap::new(),
        Some(&corpus),
        &camerata_server::report_export::ReportOptions::default(),
    );
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == SINGLE)
        .unwrap_or_else(|| {
            panic!(
                "the finding must be curated, not held: curated {:?}, held {:?}",
                json.curated_findings
                    .iter()
                    .map(|g| &g.rule_id)
                    .collect::<Vec<_>>(),
                json.held_for_review_findings
                    .iter()
                    .map(|g| &g.rule_id)
                    .collect::<Vec<_>>()
            )
        });
    // The Fix is the rule's authored remediation: either as `fix`, or, when fix-specific
    // generation fell back to it, as `fix_for_this_finding` (the exporter shows it once).
    let site = &group.sites[0];
    assert!(
        site.fix
            .iter()
            .chain(site.fix_for_this_finding.iter())
            .any(|fix| fix.contains("one exported function")),
        "{:?} / {:?}",
        site.fix,
        site.fix_for_this_finding
    );

    // The verdicts ride in the fresh manifest for the next scan.
    assert!(!manifest.clone_verdicts.is_empty());
    // No React package: the UI rule had nothing in scope and claims nothing.
    assert!(under(&report, UI).is_empty(), "{:#?}", under(&report, UI));
}

#[tokio::test]
async fn an_unchanged_rescan_reuses_the_cached_verdicts_and_makes_no_judge_call() {
    let dir = tempfile::tempdir().unwrap();
    duplicated_discount_repo(dir.path());
    let corpus = corpus().await;
    let first = Scripted::new("same_decision", "different_concept");
    let (report1, manifest) = scan(
        &first,
        dir.path(),
        &corpus,
        None,
        true,
        BackendResolution::Api,
        None,
    )
    .await;
    assert!(first.judge_calls.load(Ordering::SeqCst) >= 1);

    let second = Scripted::new("different", "different_concept"); // would change the answer if asked
    let (report2, _) = scan(
        &second,
        dir.path(),
        &corpus,
        Some(&manifest),
        true,
        BackendResolution::Api,
        None,
    )
    .await;
    assert_eq!(
        second.judge_calls.load(Ordering::SeqCst),
        0,
        "every verdict came from the cache"
    );
    assert_eq!(
        under(&report2, SINGLE).len(),
        1,
        "the same finding, from the cached verdict"
    );
    assert_eq!(
        under(&report2, SINGLE)[0].detail,
        under(&report1, SINGLE)[0].detail
    );
    assert_eq!(
        report2.provenance.clone_judge_model, None,
        "no judge call, no judge model stamped"
    );

    // A forced full scan (no prior manifest) asks again.
    let third = Scripted::new("different", "different_concept");
    let (report3, _) = scan(
        &third,
        dir.path(),
        &corpus,
        None,
        true,
        BackendResolution::Api,
        None,
    )
    .await;
    assert!(third.judge_calls.load(Ordering::SeqCst) >= 1);
    let rows = under(&report3, SINGLE);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert!(
        matches!(rows[0].kind, Some(FindingKind::ConventionObserved(_))),
        "a candidate judged different is not a defect"
    );
}

/// A model that fails the test if it is called at all.
struct Forbidden;

#[async_trait]
impl LlmPort for Forbidden {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        panic!("no model call is allowed in this scan");
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        panic!("no model call is allowed in this scan");
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn a_deterministic_only_scan_reports_the_candidates_as_not_assessed_and_never_clean() {
    let dir = tempfile::tempdir().unwrap();
    duplicated_discount_repo(dir.path());
    let corpus = corpus().await;
    let (report, manifest) = scan(
        &Forbidden,
        dir.path(),
        &corpus,
        None,
        false,
        BackendResolution::Api,
        None,
    )
    .await;

    for id in [SINGLE, NAMING] {
        let rows = under(&report, id);
        assert_eq!(rows.len(), 1, "{id}: one row, {rows:#?}");
        let Some(FindingKind::NotAssessed(kind)) = &rows[0].kind else {
            panic!("{id}: a not_assessed row, got {:?}", rows[0].kind);
        };
        assert_eq!(kind.reason, NotAssessedReason::PassFailed);
        assert!(
            kind.reason_detail.contains("deterministic-only scan"),
            "{kind:?}"
        );
        assert_eq!(rows[0].severity, "info");
        let entry = report.ledger.rule(id).unwrap();
        assert!(!entry.ran, "{id}");
        assert_eq!(
            entry.skip_reason.as_deref(),
            Some(ADVISORY_UNAVAILABLE_NOTE),
            "{id}"
        );
        assert!(!entry.verified_clean());
    }
    // Candidates are never promoted, and no judge model is stamped.
    assert!(report
        .findings
        .iter()
        .all(|f| !matches!(f.kind, Some(FindingKind::Defect))
            || (f.rule_id != SINGLE && f.rule_id != NAMING)));
    assert_eq!(report.provenance.clone_judge_model, None);
    assert!(manifest.clone_verdicts.is_empty());
    // The integrity gate reads the unavailable judgment as a scope choice, not a defect.
    let options = camerata_server::integrity_gate::IntegrityGateOptions {
        required_tiers: Vec::new(),
        require_semantic_tier: false,
        require_recorded_model: false,
    };
    let gate = camerata_server::integrity_gate::evaluate(&report.ledger, &options);
    assert!(
        gate.failures
            .iter()
            .filter(|f| f.code == "unjustified_skip")
            .all(|f| !f.detail.contains(SINGLE) && !f.detail.contains(NAMING)),
        "{:#?}",
        gate.failures
    );
}

#[tokio::test]
async fn a_blocked_project_names_the_block_in_the_not_assessed_row() {
    let dir = tempfile::tempdir().unwrap();
    duplicated_discount_repo(dir.path());
    let corpus = corpus().await;
    let blocked = BackendResolution::Blocked {
        message: "this project may not call a model".to_string(),
    };
    let (report, _) = scan(&Forbidden, dir.path(), &corpus, None, true, blocked, None).await;
    let rows = under(&report, SINGLE);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert!(
        rows[0].detail.contains("this project may not call a model"),
        "{}",
        rows[0].detail
    );
    assert_eq!(
        report.ai_blocked_reason.as_deref(),
        Some("this project may not call a model")
    );
}

#[tokio::test]
async fn a_repo_with_no_repeated_code_gets_convention_rows_not_findings() {
    let dir = tempfile::tempdir().unwrap();
    stage(
        dir.path(),
        &[
            ("src/cart/total.ts", discount("applyDiscount", "0.2")),
            (
                "src/other/mood.ts",
                "export const mood = 'calm';\n".to_string(),
            ),
        ],
    );
    let corpus = corpus().await;
    // Deterministic-only: zero candidates means the rules ran clean without any judgment.
    let (report, _) = scan(
        &Forbidden,
        dir.path(),
        &corpus,
        None,
        false,
        BackendResolution::Api,
        None,
    )
    .await;
    for (id, text) in [
        (
            SINGLE,
            "No application decision was found written in more than one file",
        ),
        (NAMING, "No helper was found under more than one name"),
    ] {
        let rows = under(&report, id);
        assert_eq!(rows.len(), 1, "{id}: {rows:#?}");
        assert!(
            matches!(rows[0].kind, Some(FindingKind::ConventionObserved(_))),
            "{id}"
        );
        assert!(rows[0].detail.starts_with(text), "{}", rows[0].detail);
        assert_eq!(rows[0].severity, "info");
        assert!(
            report.ledger.rule(id).unwrap().verified_clean() || report.ledger.rule(id).unwrap().ran
        );
    }
    assert!(
        under(&report, UI).is_empty(),
        "no React package: nothing to be consistent about"
    );
}

#[tokio::test]
async fn a_partly_judged_rule_says_so_in_the_ledger_and_in_one_row() {
    let dir = tempfile::tempdir().unwrap();
    duplicated_discount_repo(dir.path());
    let corpus = corpus().await;
    // The judge is unsure: the candidate is neither shipped nor called clean.
    let llm = Scripted::new("unsure", "unsure");
    let (report, manifest) = scan(
        &llm,
        dir.path(),
        &corpus,
        None,
        true,
        BackendResolution::Api,
        None,
    )
    .await;
    for id in [SINGLE, NAMING] {
        let rows = under(&report, id);
        assert_eq!(rows.len(), 1, "{id}: {rows:#?}");
        assert!(
            matches!(rows[0].kind, Some(FindingKind::NotAssessed(_))),
            "{id}"
        );
        let entry = report.ledger.rule(id).unwrap();
        assert_eq!(
            entry.coverage_note.as_deref(),
            Some(ADVISORY_PARTIAL_NOTE),
            "{id}"
        );
    }
    assert!(manifest.clone_verdicts.is_empty(), "unsure is never cached");
}
