#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): the three-pass consensus option through the real
//! brownfield pipeline (`onboard::audit_repos_with_llm_controlled`). Off by default: one AI
//! audit, every finding kept. On: three audits, and only what two of them agree on survives,
//! with the vote recorded as a reconciling ledger stage.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata_server::ai_audit::{audit_system_prompt, ScanMode};
use camerata_server::cost_cap::RunControls;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, ScanReport, SelectedRule};

const RULE: &str = "ARCH-CONSENSUS-TEST-1";

/// Scan pass N (1-based) answers with the finding every pass agrees on plus, on passes 1 and
/// 3, a different one-off finding. Every other call gets an empty, well-formed answer.
struct Varying {
    scan_calls: AtomicUsize,
}

/// One finding per file: the pipeline merges same-rule findings within a file into one row.
fn finding(file: &str, code: &str) -> String {
    format!(
        r#"{{"rule":"{RULE}","severity":"high","path":"src/{file}","line":1,"code":"{code}","title":"t","detail":"d"}}"#
    )
}

#[async_trait]
impl LlmPort for Varying {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        // Every pass appends the untrusted-content clause after its own system prompt.
        let text = if req
            .system
            .as_deref()
            .is_some_and(|s| s.starts_with(audit_system_prompt().as_str()))
        {
            let n = self.scan_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let mut found = vec![finding("a.ts", "export const a = 1;")];
            match n {
                1 => found.push(finding("b.ts", "export const b = 2;")),
                3 => found.push(finding("c.ts", "export const c = 3;")),
                _ => {}
            }
            format!(
                r#"{{"findings":[{}],"proposed_rules":[],"needs_files":[]}}"#,
                found.join(",")
            )
        } else {
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
    for (file, body) in [
        ("a.ts", "export const a = 1;\n"),
        ("b.ts", "export const b = 2;\n"),
        ("c.ts", "export const c = 3;\n"),
    ] {
        std::fs::write(dest.join("src").join(file), body).unwrap();
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
    g(&["config", "user.email", "consensus-e2e@camerata.local"]);
    g(&["config", "user.name", "Consensus E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

async fn scan(consensus: bool) -> (ScanReport, usize) {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Varying {
        scan_calls: AtomicUsize::new(0),
    };
    let sources = vec![("acme/shop".to_string(), dir.path().to_path_buf())];
    let selected = vec![SelectedRule {
        id: RULE.to_string(),
        directive: "consensus e2e rule".to_string(),
        repos: Vec::new(),
    }];
    let (report, _m) = onboard::audit_repos_with_llm_controlled(
        &llm,
        &sources,
        &selected,
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
        RunControls {
            consensus,
            // The scripted model recognises the long-form scan prompt, so the terse scan (and
            // the extra full-explanation step it brings) is switched off.
            efficiency: {
                let mut e = camerata_server::efficiency::EfficiencyConfig::default();
                e.apply_entry("terse_output=off").unwrap();
                e
            },
            ..RunControls::default()
        },
    )
    .await;
    (report, llm.scan_calls.load(Ordering::SeqCst))
}

fn snippets(report: &ScanReport) -> Vec<String> {
    let mut v: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE)
        .map(|f| f.snippet.trim().to_string())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn consensus_is_off_by_default_and_one_audit_keeps_everything() {
    assert!(!RunControls::default().consensus);
    let (report, scan_calls) = scan(false).await;
    assert_eq!(scan_calls, 1);
    assert_eq!(
        snippets(&report),
        vec!["export const a = 1;", "export const b = 2;"]
    );
    assert!(!report
        .ledger
        .stages()
        .iter()
        .any(|s| s.stage == "consensus"));
}

#[tokio::test]
async fn with_consensus_on_only_what_two_of_three_audits_report_survives() {
    let (report, scan_calls) = scan(true).await;
    assert_eq!(scan_calls, 3, "three full audits");
    assert_eq!(snippets(&report), vec!["export const a = 1;"]);
    let stage = report
        .ledger
        .stages()
        .iter()
        .find(|s| s.stage == "consensus")
        .expect("the vote is a ledger stage");
    assert_eq!((stage.rows_in, stage.rows_out), (5, 1));
    assert_eq!(stage.unaccounted, 0, "every dropped row is accounted for");
    assert!(
        !report
            .failed_passes
            .iter()
            .any(|p| p.pass.starts_with("consensus")),
        "all three audits answered, so no consensus failure is disclosed: {:?}",
        report.failed_passes
    );
}
