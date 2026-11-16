//! The read log: every read-tool call of a run is recorded in the run's ledger with its path
//! (relative to the jail), the bytes handed to the model and whether the jail allowed it, and a
//! delegated child's reads land in the PARENT run's ledger.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_core::{AgentDriver, Role};
use serde_json::json;

use super::scripted::{final_turn, tool_turn, ScriptedCompleter};
use super::{AgentRunLedger, ApiAgentDriver, ReadVerdict};

fn role() -> Role {
    Role {
        name: "implementer".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    }
}

fn worktree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(wt.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(wt.join("README.md"), "# synthetic\n").unwrap();
    (tmp, wt)
}

fn driver(script: Vec<serde_json::Value>, wt: &Path) -> (ApiAgentDriver, Arc<ScriptedCompleter>) {
    let completer = Arc::new(ScriptedCompleter::new(script, Some(0.0)));
    let d = ApiAgentDriver::new(completer.clone(), "scripted").with_worktree(wt.to_path_buf());
    (d, completer)
}

#[tokio::test]
async fn three_reads_give_three_log_entries_with_their_paths() {
    let (_tmp, wt) = worktree();
    let (d, _) = driver(
        vec![
            tool_turn(
                "Read",
                &[
                    json!({ "file_path": "src/lib.rs" }),
                    json!({ "file_path": "src/main.rs" }),
                ],
            ),
            tool_turn("Read", &[json!({ "file_path": "README.md" })]),
            final_turn("done"),
        ],
        &wt,
    );
    d.run(&role(), "read three files").await.expect("run");

    let log = d.run_ledger().read_log();
    let paths: Vec<&str> = log.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["src/lib.rs", "src/main.rs", "README.md"], "{log:?}");
    assert!(log
        .iter()
        .all(|e| e.tool == "Read" && e.agent == "implementer"));
    assert!(log.iter().all(|e| e.verdict == ReadVerdict::Allowed));
    assert_eq!(
        log[0].bytes,
        "pub fn a() {}\n".len(),
        "bytes returned to the model"
    );
}

#[tokio::test]
async fn a_refused_read_is_logged_as_refused() {
    let (tmp, wt) = worktree();
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, "never read").unwrap();
    let (d, _) = driver(
        vec![
            tool_turn(
                "Read",
                &[json!({ "file_path": outside.display().to_string() })],
            ),
            tool_turn("Glob", &[json!({ "pattern": "../*" })]),
            final_turn("done"),
        ],
        &wt,
    );
    d.run(&role(), "try to escape").await.expect("run");

    let log = d.run_ledger().read_log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0].verdict, ReadVerdict::Refused);
    assert_eq!(log[0].bytes, 0);
    assert_eq!(log[0].path, outside.display().to_string());
    assert_eq!(log[1].verdict, ReadVerdict::Refused);
    assert_eq!(log[1].tool, "Glob");
}

#[tokio::test]
async fn every_read_tool_is_logged() {
    let (_tmp, wt) = worktree();
    let (d, _) = driver(
        vec![
            tool_turn("Glob", &[json!({ "pattern": "src/*.rs" })]),
            tool_turn("Grep", &[json!({ "pattern": "fn", "path": "src" })]),
            tool_turn("LS", &[json!({})]),
            final_turn("done"),
        ],
        &wt,
    );
    d.run(&role(), "look around").await.expect("run");
    let tools: Vec<String> = d
        .run_ledger()
        .read_log()
        .into_iter()
        .map(|e| format!("{} {}", e.tool, e.path))
        .collect();
    assert_eq!(tools, ["Glob src/*.rs", "Grep src", "LS ."]);
}

/// A test child factory standing in for the provider machinery: it builds the child exactly as
/// `ServerChildDriverFactory::build_child` hands it its API-loop context (read folders and the
/// parent's ledger), but on a scripted completer instead of a real model.
struct ScriptedChildFactory {
    server: super::ServerChildDriverFactory,
}

impl camerata_gateway::delegate::ChildDriverFactory for ScriptedChildFactory {
    fn build_child(
        &self,
        model: &str,
        worktree: &Path,
        read_dirs: &[PathBuf],
    ) -> std::io::Result<Box<dyn AgentDriver>> {
        let completer = Arc::new(ScriptedCompleter::new(
            vec![
                tool_turn("Read", &[json!({ "file_path": "src/main.rs" })]),
                final_turn("child done"),
            ],
            Some(0.0),
        ));
        Ok(Box::new(
            ApiAgentDriver::new(completer, model)
                .with_worktree(worktree.to_path_buf())
                .with_api_loop(self.server.api_loop_for_child(read_dirs)),
        ))
    }
}

fn server_child_factory() -> super::ServerChildDriverFactory {
    super::ServerChildDriverFactory::new(
        crate::model_registry::ModelRegistry::new(),
        crate::provider_policy::ProviderPolicy::default(),
        Arc::new(crate::credentials::MemoryCredentialStore::default()),
        Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
        PathBuf::from("/nonexistent/camerata-gateway"),
        vec![],
        None,
        None,
        crate::llm::ProjectBackend::Api,
    )
}

#[tokio::test]
async fn a_child_agents_reads_appear_in_the_parent_runs_log() {
    let (_tmp, wt) = worktree();
    let ledger = Arc::new(AgentRunLedger::new());
    let config = camerata_gateway::delegate::OrchestratorConfig {
        models: camerata_gateway::delegate::DelegateModels {
            fast: "scripted-fast".to_string(),
            balanced: "scripted-balanced".to_string(),
            strongest: "scripted-strong".to_string(),
            vision: String::new(),
        },
        worktree_root: wt.clone(),
        gateway_bin: PathBuf::from("/nonexistent/camerata-gateway"),
        depth: 0,
        max_depth: 1,
        child_driver_factory: Some(Arc::new(ScriptedChildFactory {
            server: server_child_factory().with_run_ledger(ledger.clone()),
        })),
    };
    let lead_completer = Arc::new(ScriptedCompleter::new(
        vec![
            tool_turn("Read", &[json!({ "file_path": "src/lib.rs" })]),
            tool_turn(
                "delegate",
                &[json!({ "subtask": "read main", "tier": "fast" })],
            ),
            final_turn("lead done"),
        ],
        Some(0.0),
    ));
    let lead = ApiAgentDriver::new(lead_completer, "scripted-strong")
        .with_worktree(wt.clone())
        .as_orchestrator(true)
        .with_orchestrator_config(config)
        .with_run_ledger(ledger.clone());

    let out = lead.run(&role(), "lead the work").await.expect("lead run");
    assert_eq!(out.result, "lead done");

    let paths: Vec<String> = ledger
        .read_log()
        .into_iter()
        .map(|e| format!("{}:{}", e.agent, e.path))
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "the lead's read and the child's read: {paths:?}"
    );
    assert_eq!(paths[0], "implementer:src/lib.rs");
    assert!(
        paths[1].ends_with(":src/main.rs") && !paths[1].starts_with("implementer:"),
        "the child's read is in the parent run's log, under the child's role: {paths:?}"
    );
}

#[tokio::test]
async fn the_read_log_is_kept_with_the_run_in_its_governance_trail() {
    let (tmp, wt) = worktree();
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, "never read").unwrap();
    let log = Arc::new(
        camerata_persistence::GovernanceLog::open_in_memory()
            .await
            .expect("in-memory governance log"),
    );
    let ledger = Arc::new(AgentRunLedger::new().with_governance(log.clone(), "run-read-log-1"));
    let (d, _) = driver(
        vec![
            tool_turn(
                "Read",
                &[
                    json!({ "file_path": "src/lib.rs" }),
                    json!({ "file_path": outside.display().to_string() }),
                ],
            ),
            final_turn("done"),
        ],
        &wt,
    );
    d.with_run_ledger(ledger)
        .run(&role(), "read")
        .await
        .expect("run");

    let reads: Vec<_> = log
        .by_run("run-read-log-1")
        .await
        .expect("by_run")
        .into_iter()
        .filter(|e| e.kind == "agent_read")
        .collect();
    assert_eq!(reads.len(), 2, "one durable event per read: {reads:?}");
    let detail: serde_json::Value =
        serde_json::from_str(reads[0].detail.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(detail["path"], "src/lib.rs");
    assert_eq!(detail["verdict"], "allowed");
    assert_eq!(detail["bytes"], "pub fn a() {}\n".len());
    let refused: serde_json::Value =
        serde_json::from_str(reads[1].detail.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(refused["verdict"], "refused");
}

fn orchestrator_factory() -> super::ServerOrchestratorDriverFactory {
    super::ServerOrchestratorDriverFactory::new(
        crate::model_registry::ModelRegistry::new(),
        crate::provider_policy::ProviderPolicy::default(),
        Arc::new(crate::credentials::MemoryCredentialStore::default()),
        Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
        PathBuf::from("/nonexistent/camerata-gateway"),
        None,
        None,
        crate::llm::ProjectBackend::Api,
    )
}

#[test]
fn a_native_lead_and_its_children_share_the_run_ledger() {
    let ledger = Arc::new(AgentRunLedger::new());
    let factory = orchestrator_factory().with_run_ledger(ledger.clone());
    let (lead_ledger, children) = factory.native_lead_parts(&[]);
    assert!(
        Arc::ptr_eq(&lead_ledger, &ledger),
        "the lead records into the run's ledger"
    );
    let child_ctx = children.api_loop_for_child(&[]);
    assert!(
        child_ctx
            .ledger
            .as_ref()
            .is_some_and(|l| Arc::ptr_eq(l, &ledger)),
        "the lead's children record into the run's ledger too"
    );

    // With no run ledger, a lead still shares its own with its children.
    let (own, children) = orchestrator_factory().native_lead_parts(&[]);
    let child_ctx = children.api_loop_for_child(&[]);
    assert!(child_ctx
        .ledger
        .as_ref()
        .is_some_and(|l| Arc::ptr_eq(l, &own)));
}
