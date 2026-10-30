#![allow(clippy::unwrap_used)]
//! Where `camerata inspect` gets a model: its explicit flags (`--model`, `--calibration-model`,
//! `--fix-model`), else the built-in default (Sonnet 5.5). Never the environment. Every variable
//! that used to pick a model is set here to an id nothing may run; the run, driven by a SCRIPTED
//! model (no network, no spend), must ignore all of them:
//!
//! * every request names a model, and that model is a flag or a built-in default;
//! * the export's provenance records the model that actually ran.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

const REMOVED_MODEL_VARIABLES: [&str; 6] = [
    "CAMERATA_LLM_MODEL",
    "CAMERATA_AUDIT_MODEL",
    "CAMERATA_CALIBRATION_MODEL",
    "CAMERATA_CLONE_JUDGE_MODEL",
    "CAMERATA_LEAD_ENGINEER_MODEL",
    "CAMERATA_REVIEWER_MODEL",
];
const ENV_MODEL: &str = "env-picked-model";

struct Recorder {
    requested: Arc<Mutex<Vec<String>>>,
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

fn stage_repo(dest: &Path) {
    std::fs::write(
        dest.join("app.py"),
        "def handler(request):\n    return request.args.get('q')\n",
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
    g(&["config", "user.email", "model-source@camerata.local"]);
    g(&["config", "user.name", "Model Source"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(
    repo: &Path,
    export: &Path,
    model: Option<&str>,
    calibration_model: Option<&str>,
    fix_model: Option<&str>,
) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: true,
        model: model.map(str::to_string),
        calibration_model: calibration_model.map(str::to_string),
        fix_model: fix_model.map(str::to_string),
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

/// Runs the pipeline with every removed variable set; returns the models the requests named and
/// the manifest's recorded models.
async fn run(
    model: Option<&str>,
    calibration_model: Option<&str>,
    fix_model: Option<&str>,
) -> Option<(Vec<String>, serde_json::Value)> {
    if !camerata_test_env::typst_on_path_for("inspect_model_source") {
        return None;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let _env: Vec<camerata_test_env::EnvGuard> = REMOVED_MODEL_VARIABLES
        .iter()
        .map(|name| camerata_test_env::EnvGuard::set(name, ENV_MODEL))
        .collect();
    let repo = tempfile::tempdir().unwrap();
    stage_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let requested = Arc::new(Mutex::new(Vec::new()));
    let llm = Recorder {
        requested: requested.clone(),
    };
    run_inspect_pipeline_with(
        args(repo.path(), &export, model, calibration_model, fix_model),
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

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&export).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let models = requested.lock().unwrap().clone();
    Some((models, manifest))
}

/// No flags: the built-in default runs everywhere, whatever the environment says.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_no_flags_the_built_in_default_runs_and_the_environment_is_ignored() {
    let Some((requested, manifest)) = run(None, None, None).await else {
        return;
    };
    assert!(!requested.is_empty(), "the scan called the scripted model");
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
            "`{model}` is not a built-in default"
        );
    }
    assert_eq!(
        manifest["inspection"]["models"][0]["model"],
        camerata_server::llm::DEFAULT_MODEL,
        "the export records the model that ran: {}",
        manifest["inspection"]
    );
}

/// Explicit flags are the only way to change a model: every request names a flag's model or a
/// built-in cheap-stage default, and the export records the flag.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn explicit_flags_are_the_only_source_and_the_export_records_them() {
    let Some((requested, manifest)) = run(
        Some("claude-opus-5-5"),
        Some("claude-haiku-5-5"),
        Some("claude-sonnet-5-5"),
    )
    .await
    else {
        return;
    };
    let allowed = [
        "claude-opus-5-5".to_string(),
        "claude-haiku-5-5".to_string(),
        "claude-sonnet-5-5".to_string(),
        camerata_api_types::project::FIX_STAGE_HAIKU_MODEL.to_string(),
        camerata_fleet::tier::default_fast_model(),
    ];
    assert!(!requested.is_empty(), "the scan called the scripted model");
    for model in &requested {
        assert!(!model.trim().is_empty(), "a request named no model");
        assert_ne!(model, ENV_MODEL, "a model came from the environment");
        assert!(allowed.contains(model), "`{model}` was named by no flag");
    }
    assert_eq!(
        manifest["inspection"]["models"][0]["model"], "claude-opus-5-5",
        "the export records the model the flag chose: {}",
        manifest["inspection"]
    );
}
