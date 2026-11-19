#![allow(clippy::unwrap_used)]
//! The agent finder's model calls show in Agent activity through the real brownfield pipeline
//! (`onboard::audit_repos_with_llm_controlled`): a stand-in agent plays the finder, no model is
//! called, and the transcript store the scan was given holds the finder's row.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use camerata_server::ai_audit::agent_finder::run::with_agent_override;
use camerata_server::ai_audit::agent_finder::{FinderChoice, ScriptedAgent};
use camerata_server::ai_audit::ScanMode;
use camerata_server::cost_cap::RunControls;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::transcript::TranscriptStore;

const RULE: &str = "ARCH-ACTIVITY-TEST-1";

/// A model that must never be asked: the finder is the stand-in agent.
struct Silent;

#[async_trait]
impl LlmPort for Silent {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("no model in this test")
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

fn stage_repo(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(dest.join("src/a.ts"), "export const a = 1;\n").unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "activity-e2e@camerata.local"]);
    g(&["config", "user.name", "Activity E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

#[tokio::test]
async fn the_agent_finder_shows_its_piece_row_when_a_scan_is_given_a_transcript_store() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let sources = vec![("acme/shop".to_string(), dir.path().to_path_buf())];
    let selected = vec![SelectedRule {
        id: RULE.to_string(),
        directive: "activity e2e rule".to_string(),
        repos: Vec::new(),
    }];
    let store = TranscriptStore::new();
    let agent = std::sync::Arc::new(ScriptedAgent::new(Vec::new()));
    let controls = RunControls {
        finder: FinderChoice::Agent,
        ..RunControls::default()
    };
    let _ = with_agent_override(agent, async {
        onboard::audit_repos_with_llm_controlled(
            &Silent,
            &sources,
            &selected,
            Vec::new(),
            None,
            None,
            ScanMode::Parallel,
            false,
            Some((&store, "scan-key")),
            None,
            None,
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
    })
    .await;
    let rows = store.get("scan-key");
    assert!(
        rows.iter()
            .any(|r| r.role == "Finder, piece 1 of 1 (acme/shop)"),
        "{rows:#?}"
    );
}
