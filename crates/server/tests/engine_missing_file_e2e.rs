#![allow(clippy::unwrap_used)]
//! END-TO-END (#546): the Engine finder, through the real brownfield pipeline
//! (`onboard::audit_repos_with_llm_controlled`) with a stand-in model. The model plants two rows
//! the evidence check used to pass: an exempt-shape row (no cited line) on a file the repo does
//! not have, and a row with a real quote on a file that exists on disk but was excluded by
//! agreement, so the scan never admitted it. Both must come out of the scan held (marked
//! `evidence_unverified`), never silently passing and never dropped. No model is called.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use camerata_server::ai_audit::agent_finder::FinderChoice;
use camerata_server::ai_audit::ScanMode;
use camerata_server::cost_cap::RunControls;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use camerata_server::onboard::scope::ScanScope;
use camerata_server::onboard::{self, SelectedRule};

/// A semantic-tier rule, and a floor rule the model can also report under (H12).
const SEMANTIC_RULE: &str = "ARCH-NO-DIRECT-DB-1";
const FLOOR_RULE: &str = "SEC-NO-RAW-SQL-CONCAT-1";

/// Answers every call with two findings: one on a file the repo does not have, one on a file the
/// scan excluded by agreement. Both are reported under `rule`.
struct PlantsMissingFile {
    rule: &'static str,
}

#[async_trait]
impl LlmPort for PlantsMissingFile {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: format!(
                r#"{{"findings":[{{"rule":"{rule}","severity":"high","path":"SECURITY.md","line":0,"code":"no security policy is published","title":"no security policy","detail":"the repository publishes no security policy"}},{{"rule":"{rule}","severity":"high","path":"src/excluded.rs","line":1,"code":"db.execute(query);","title":"direct DB call","detail":"controller calls the database directly"}}],"verdicts":[]}}"#,
                rule = self.rule
            ),
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
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
    std::fs::write(
        dest.join("src/a.rs"),
        "fn handler() { db.execute(query); }\n",
    )
    .unwrap();
    std::fs::write(
        dest.join("src/excluded.rs"),
        "fn other() { db.execute(query); }\n",
    )
    .unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "missing-file-e2e@camerata.local"]);
    g(&["config", "user.name", "Missing File E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

async fn scan_with(rule: &'static str) -> onboard::ScanReport {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let sources = vec![("acme/shop".to_string(), dir.path().to_path_buf())];
    let selected = vec![SelectedRule {
        id: rule.to_string(),
        directive: "controllers must not call the database directly".to_string(),
        repos: Vec::new(),
    }];
    let controls = RunControls {
        scope: ScanScope::from_patterns(&["shop/src/excluded.rs".to_string()]).unwrap(),
        finder: FinderChoice::Engine,
        ..RunControls::default()
    };
    let (report, _manifest) = onboard::audit_repos_with_llm_controlled(
        &PlantsMissingFile { rule },
        &sources,
        &selected,
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
        Some(&corpus),
        &HashMap::new(),
        controls,
    )
    .await;
    report
}

fn assert_both_planted_rows_are_held(report: &onboard::ScanReport) {
    for planted_path in ["SECURITY.md", "src/excluded.rs"] {
        let planted: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.path == planted_path)
            .collect();
        assert!(
            !planted.is_empty(),
            "the planted row on {planted_path} must be in the report, not dropped: {:#?}",
            report.findings
        );
        for f in planted {
            assert!(
                f.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE),
                "a row on {planted_path} must be held: {f:#?}"
            );
        }
    }
    assert!(report.ledger.evidence_check().reconciles());
}

#[tokio::test]
async fn engine_rows_on_a_missing_file_and_on_an_unadmitted_file_are_held_by_the_scan() {
    assert_both_planted_rows_are_held(&scan_with(SEMANTIC_RULE).await);
}

/// The model can report under a floor rule id, which the ledger records as deterministic. The row
/// is still the model's, and still held.
#[tokio::test]
async fn engine_rows_reported_under_a_floor_rule_id_are_held_by_the_scan() {
    assert_both_planted_rows_are_held(&scan_with(FLOOR_RULE).await);
}
