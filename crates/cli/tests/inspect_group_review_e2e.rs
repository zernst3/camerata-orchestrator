#![allow(clippy::unwrap_used)]
//! END-TO-END, scripted model, zero spend: `camerata inspect` runs the group reviewer (stage 2)
//! after the scan, over the planted findings of the shared fixture repo.
//!
//! * the group reviewer is asked on the model the `--group-review-model` flag names (else Fable
//!   5.1), never one from the environment;
//! * the escalation settings (`--escalation-rate`, `--escalation-floor`) reach the budget;
//! * the operator ledger and the printed summary state "Escalated N of a budget of B (...)";
//! * what it said about a site is internal: no client export carries it.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

/// Answers the scan with nothing, stage 1 with nothing usable, and stage 2 with one sub-group of
/// every site it is shown, escalating the first.
struct Scripted {
    group_review: Arc<Mutex<Vec<LlmRequest>>>,
}

impl Scripted {
    fn reply(&self, req: &LlmRequest) -> String {
        let is_stage2 = req.system.as_deref().is_some_and(|s| {
            s.starts_with(&camerata_server::ai_audit::group_review::group_review_system_prompt())
        });
        if !is_stage2 {
            return r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string();
        }
        self.group_review.lock().unwrap().push(req.clone());
        let re = regex::Regex::new(r"(?m)^(\d+): ").unwrap();
        let indices: Vec<usize> = re
            .captures_iter(&req.prompt)
            .map(|c| c[1].parse().unwrap())
            .collect();
        let escalations: Vec<serde_json::Value> = indices
            .iter()
            .take(1)
            .map(|i| {
                serde_json::json!({"index": i, "reason": "rule_misfit", "rank": 1, "why": "Worth a look."})
            })
            .collect();
        serde_json::json!({
            "summary": "These sites share one cause.",
            "subgroups": [{"label": "All", "cause": "one shared cause", "summary": "One cause.", "members": indices}],
            "outliers": [],
            "escalations": escalations,
            "disagreements": [],
        })
        .to_string()
    }
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: self.reply(&req),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(10),
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

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn stage_fixture(dest: &Path) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    copy_dir_recursive(&root, dest);
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "group-review@camerata.local"]);
    g(&["config", "user.name", "Group Review"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(repo: &Path, export: &Path) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: true,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
    }
}

async fn run(
    tweak: impl FnOnce(&mut InspectArgs),
) -> Option<(
    camerata::inspect_cmd::InspectOutcome,
    Vec<LlmRequest>,
    Vec<u8>,
    String,
)> {
    if !camerata_test_env::typst_on_path_for("inspect_group_review_e2e") {
        return None;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    // A variable nothing may read.
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_GROUP_REVIEW_MODEL", "env-picked-model");
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let llm = Scripted {
        group_review: seen.clone(),
    };
    let mut a = args(repo.path(), &export);
    tweak(&mut a);
    let outcome = run_inspect_pipeline_with(
        a,
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            resume: None,
            llm: Some(&llm),
            preflight_sink: &|_| {},
        },
    )
    .await
    .expect("the inspection completes");
    let mut findings_json = Vec::new();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&export).unwrap()).unwrap();
    archive
        .by_name("findings.json")
        .unwrap()
        .read_to_end(&mut findings_json)
        .unwrap();
    let requests = seen.lock().unwrap().clone();
    // Read while the state folder still exists.
    let ledger = std::fs::read_to_string(&outcome.ledger_path).unwrap();
    Some((outcome, requests, findings_json, ledger))
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_group_reviewer_runs_on_its_flag_and_the_ledger_states_the_budget() {
    let Some((outcome, requests, _, ledger)) = run(|a| {
        a.group_review_model = Some("claude-opus-5-5".to_string());
        a.escalation_floor = Some(2);
        a.escalation_rate = Some(1.0);
    })
    .await
    else {
        return;
    };
    assert!(!requests.is_empty(), "the group reviewer was asked");
    for req in &requests {
        assert_eq!(req.model, "claude-opus-5-5", "the flag is the model asked");
        assert!(req.prompt.contains("Rule group node: rule:"));
    }
    assert!(
        outcome.ledger_summary.contains("== ESCALATION =="),
        "{}",
        outcome.ledger_summary
    );
    assert!(
        outcome.ledger_summary.contains("of a budget of 2"),
        "the floor reached the budget: {}",
        outcome.ledger_summary
    );
    assert!(
        outcome.ledger_summary.contains("ai-group-review"),
        "the stage is in the pipeline stages: {}",
        outcome.ledger_summary
    );
    // The operator ledger on disk carries the block and the model.
    assert!(ledger.contains("\"escalation\""), "{ledger}");
    assert!(ledger.contains("claude-opus-5-5"));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_no_flag_the_group_reviewer_runs_on_fable_and_the_environment_is_ignored() {
    let Some((_, requests, findings_json, _)) = run(|_| {}).await else {
        return;
    };
    assert!(!requests.is_empty());
    for req in &requests {
        assert_eq!(req.model, "claude-fable-5-1");
        assert_ne!(req.model, "env-picked-model");
    }
    // What the group reviewer said is internal: the client's findings.json carries none of it.
    let text = String::from_utf8(findings_json).unwrap();
    for internal in [
        "escalation_reason",
        "escalation_why",
        "subgroup_cause",
        "stage2_disagreement",
        "global_rank",
        "conflicting_verdicts",
        "summary_omits_member",
    ] {
        assert!(
            !text.contains(internal),
            "the client export carries {internal}"
        );
    }
}
