#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): where a model comes from.
//!
//! A model is chosen by the open project's settings (the cockpit) or by explicit flags (the
//! command line), else the built-in default (Sonnet 5.5). It is never read from the environment:
//! a paid run once stopped because a stage sent no model and the transport filled one in from a
//! local `.env`. These tests set every variable that used to pick a model, run the real
//! brownfield pipeline against a scripted model, and prove that
//!
//! * every request the inspection sends names a model (none is empty);
//! * the only sources of those names are the explicit picks and the built-in defaults, never a
//!   value from the environment;
//! * the provenance records the model that actually ran.

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use camerata_server::ai_audit::ScanMode;
use camerata_server::cost_cap::RunControls;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, ScanReport, SelectedRule};
use camerata_test_env::EnvGuard;
use std::collections::HashMap;

/// Every variable that used to pick a model. The values name a model no run may ever use.
const REMOVED_MODEL_VARIABLES: [&str; 6] = [
    "CAMERATA_LLM_MODEL",
    "CAMERATA_AUDIT_MODEL",
    "CAMERATA_CALIBRATION_MODEL",
    "CAMERATA_CLONE_JUDGE_MODEL",
    "CAMERATA_LEAD_ENGINEER_MODEL",
    "CAMERATA_REVIEWER_MODEL",
];
const ENV_MODEL: &str = "env-picked-model";

/// Sets every removed variable for the length of a test.
fn set_every_removed_variable() -> Vec<EnvGuard> {
    REMOVED_MODEL_VARIABLES
        .iter()
        .map(|name| EnvGuard::set(name, ENV_MODEL))
        .collect()
}

/// Answers every call with an empty, well-formed result, priced, and records the model each
/// request named.
struct Recorder {
    requested: Mutex<Vec<String>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            requested: Mutex::new(Vec::new()),
        }
    }
    fn requested(&self) -> Vec<String> {
        self.requested.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmPort for Recorder {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.requested.lock().unwrap().push(req.model.clone());
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
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
    g(&["config", "user.email", "model-source-e2e@camerata.local"]);
    g(&["config", "user.name", "Model Source E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn selected() -> Vec<SelectedRule> {
    (0..30)
        .map(|n| SelectedRule {
            id: format!("ARCH-MODEL-SRC-{n:02}-1"),
            directive: format!("model source e2e rule {n}"),
            repos: Vec::new(),
        })
        .collect()
}

async fn scan(
    llm: &dyn LlmPort,
    dir: &Path,
    model: Option<&str>,
    calibration_model: Option<&str>,
) -> ScanReport {
    let sources = vec![("acme/shop".to_string(), dir.to_path_buf())];
    onboard::audit_repos_with_llm_controlled(
        llm,
        &sources,
        &selected(),
        Vec::new(),
        model,
        calibration_model,
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
        RunControls::default(),
    )
    .await
    .0
}

/// Nothing chosen anywhere: every request names the built-in default (or the cheap stages'
/// built-in default), whatever the environment says, and the provenance records what ran.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_no_pick_every_request_names_a_built_in_default_and_never_the_environment() {
    let _env = set_every_removed_variable();
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Recorder::new();

    let report = scan(&llm, dir.path(), None, None).await;

    let requested = llm.requested();
    assert!(!requested.is_empty(), "the scan ran");
    let allowed = [
        camerata_server::llm::DEFAULT_MODEL.to_string(),
        camerata_api_types::project::FIX_STAGE_HAIKU_MODEL.to_string(),
        camerata_fleet::tier::default_fast_model(),
    ];
    for model in &requested {
        assert!(!model.trim().is_empty(), "a request named no model");
        assert_ne!(model, ENV_MODEL, "a model came from the environment");
        assert!(
            allowed.contains(model),
            "`{model}` is neither the built-in default nor a built-in cheap-stage default"
        );
    }
    assert_eq!(
        report.provenance.audit_model.as_deref(),
        Some(camerata_server::llm::DEFAULT_MODEL),
        "the provenance records the scan model that ran"
    );
    // Calibration runs only when there are findings to calibrate; when it ran it followed the
    // scan model, not the environment.
    if let Some(calibration) = report.provenance.calibration_model.as_deref() {
        assert_eq!(calibration, camerata_server::llm::DEFAULT_MODEL);
    }
}

/// The project's picks (the cockpit passes them from the open project's model settings) are the
/// only source: every request names one of them or a built-in cheap-stage default, and the
/// provenance records exactly the picks.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn explicit_picks_are_the_only_source_and_the_provenance_records_them() {
    let _env = set_every_removed_variable();
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Recorder::new();

    let report = scan(
        &llm,
        dir.path(),
        Some("claude-opus-5-5"),
        Some("claude-haiku-5-5"),
    )
    .await;

    let requested = llm.requested();
    assert!(!requested.is_empty(), "the scan ran");
    let allowed = [
        "claude-opus-5-5".to_string(),
        "claude-haiku-5-5".to_string(),
        camerata_api_types::project::FIX_STAGE_HAIKU_MODEL.to_string(),
        camerata_fleet::tier::default_fast_model(),
    ];
    for model in &requested {
        assert!(!model.trim().is_empty(), "a request named no model");
        assert_ne!(model, ENV_MODEL, "a model came from the environment");
        assert!(
            allowed.contains(model),
            "`{model}` was not chosen by anyone"
        );
    }
    assert!(
        requested.iter().any(|m| m == "claude-opus-5-5"),
        "the scan ran on the model the project picked: {requested:?}"
    );
    assert_eq!(
        report.provenance.audit_model.as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        report.provenance.calibration_model.as_deref(),
        Some("claude-haiku-5-5")
    );
}

/// The three resolvers the cockpit and the command line share ignore every removed variable.
#[test]
#[serial_test::serial(process_env)]
fn the_shared_resolvers_ignore_every_removed_variable() {
    let _env = set_every_removed_variable();
    assert_eq!(
        camerata_server::ai_audit::resolve_audit_model(None),
        camerata_server::llm::DEFAULT_MODEL
    );
    assert_eq!(
        camerata_server::ai_audit::resolve_audit_model(Some("picked")),
        "picked"
    );
    assert_eq!(
        camerata_server::ai_audit::resolve_calibration_model(None, "scan-model"),
        "scan-model"
    );
    assert_eq!(
        camerata_server::ai_audit::resolve_calibration_model(Some("picked"), "scan-model"),
        "picked"
    );
    assert_eq!(
        camerata_server::ai_audit::stage_model(None),
        camerata_server::llm::DEFAULT_MODEL
    );
    assert_eq!(
        camerata_server::onboard::clone_advisory::resolve_clone_judge_model(None),
        camerata_fleet::tier::default_fast_model()
    );
}

/// The `.env` template no longer offers a model line, and says models are chosen elsewhere.
#[test]
fn the_env_template_has_no_model_lines() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env.example");
    let text = std::fs::read_to_string(path).unwrap();
    for line in text.lines() {
        let assignment = line.trim_start_matches('#').trim();
        let name = assignment.split('=').next().unwrap_or_default().trim();
        assert!(
            !(name.starts_with("CAMERATA_") && name.contains("MODEL") && assignment.contains('=')),
            "the template still offers a model variable: {line}"
        );
    }
    assert!(
        text.contains("Models are NOT chosen here"),
        "the header says where models are chosen"
    );
}
