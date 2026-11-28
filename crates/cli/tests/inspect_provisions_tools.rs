#![allow(clippy::unwrap_used)]
//! #587: `camerata inspect` provisions every scan tool and runtime it needs when the run is
//! triggered, before the first model call, and refuses the run when it cannot. `--plan-only`
//! lists the same tool set with each tool's state. A SCRIPTED model (no network, no spend) and
//! fake tools served from memory (no download) throughout.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use camerata::inspect_cmd::plan_only::run_plan_only_with_tools;
use camerata::inspect_cmd::{ledger_path, run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};
use camerata_server::pinned_tools::fixtures::FakeTools;
use camerata_server::pinned_tools::{Artifact, ToolEnv};

/// A scripted model that records, at its FIRST call, whether ruff was already installed.
struct Scripted {
    calls: AtomicUsize,
    tools: ToolEnv,
    ruff_ready_at_first_call: Mutex<Option<bool>>,
}

impl Scripted {
    fn new(tools: &ToolEnv) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            tools: tools.clone(),
            ruff_ready_at_first_call: Mutex::new(None),
        }
    }
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            *self.ruff_ready_at_first_call.lock().unwrap() =
                Some(self.tools.is_installed(Artifact::Ruff));
        }
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: camerata_server::ai_audit::group_review::stand_in_reply(req.system.as_deref(), &req.prompt)
                .unwrap_or_else(|| r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string()),
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

/// A Python repository: the stack that needs ruff.
fn stage_python_repo(dest: &Path) {
    std::fs::write(
        dest.join("app.py"),
        "import os\n\ndef handler(request):\n    q = \"SELECT * FROM t WHERE id = \" + request.args.get('id')\n    return os.system(q)\n",
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
    g(&["config", "user.email", "tools-e2e@camerata.local"]);
    g(&["config", "user.name", "Tools E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(repo: &Path, export: &Path, client_engagement: bool) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: true,
        model: Some("claude-sonnet-5".to_string()),
        calibration_model: Some("claude-sonnet-5".to_string()),
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Acme Widgets".to_string(),
        client_engagement,
        run_client_tooling: false,
        consensus: false,
        finder: Default::default(),
        exclude: Vec::new(),
    }
}

struct Run {
    result: Result<camerata::inspect_cmd::InspectOutcome, String>,
    model_calls: usize,
    ruff_ready_at_first_call: Option<bool>,
    printed: String,
}

async fn run(args: InspectArgs, tools: &FakeTools) -> Run {
    let llm = Scripted::new(&tools.env);
    let printed = Mutex::new(String::new());
    let result = run_inspect_pipeline_with(
        args,
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            resume: None,
            llm: Some(&llm),
            preflight_sink: &|t| printed.lock().unwrap().push_str(t),
            tools: Some(&tools.env),
        },
    )
    .await
    .map_err(|e| e.to_string());
    let ruff_ready_at_first_call = *llm.ruff_ready_at_first_call.lock().unwrap();
    Run {
        result,
        model_calls: llm.calls.load(Ordering::SeqCst),
        ruff_ready_at_first_call,
        printed: printed.into_inner().unwrap(),
    }
}

/// The owner's words on #587: "ruff should have been there from the beginning if it was
/// needed." A Python repo on a machine with no ruff: ruff is provisioned into the tooling folder
/// before the finder's first model call, the tool list said it would be, and the ruff pass runs.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_python_repo_gets_ruff_provisioned_before_the_first_model_call_and_the_pass_runs() {
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = tempfile::tempdir().unwrap();
    stage_python_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let tools = FakeTools::new();
    assert!(
        !tools.env.is_installed(Artifact::Ruff),
        "no ruff anywhere yet"
    );

    let run = run(args(repo.path(), &export, true), &tools).await;

    assert!(
        run.model_calls > 0,
        "the finder ran: {:?}",
        run.result.as_ref().err()
    );
    assert_eq!(
        run.ruff_ready_at_first_call,
        Some(true),
        "ruff must be provisioned BEFORE the first model call"
    );
    assert!(
        run.printed.contains("== TOOLS =="),
        "the tool list is printed before the run: {}",
        run.printed
    );
    assert!(
        run.printed
            .contains("ruff 0.17.5 (for ruff): will be installed"),
        "{}",
        run.printed
    );
    let ruff_runs = FakeTools::runs_of(&tools.entry(Artifact::Ruff));
    assert!(
        !ruff_runs.is_empty(),
        "the ruff pass ran the provisioned ruff: {:?}",
        run.result.as_ref().err()
    );
}

/// With provisioning made to fail, a client run is refused before ANY model call: nothing is
/// sent, nothing is exported, no ledger is written.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_run_whose_tools_cannot_be_provisioned_is_refused_before_any_model_call() {
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = tempfile::tempdir().unwrap();
    stage_python_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let tools = FakeTools::new();
    tools
        .source
        .fail_everything("network unreachable (fixture)");

    let run = run(args(repo.path(), &export, true), &tools).await;

    let err = run.result.expect_err("the run is refused");
    assert_eq!(run.model_calls, 0, "no model may be called");
    assert!(
        err.contains("this client inspection did not start"),
        "{err}"
    );
    assert!(err.contains("could not be provisioned"), "{err}");
    assert!(err.contains("Nothing was sent to a model"), "{err}");
    assert!(!export.exists(), "no export may be written");
    assert!(
        !ledger_path(repo.path()).exists(),
        "no ledger may be written"
    );
    assert!(
        !err.contains('\u{2014}') && !err.contains('\u{2013}'),
        "no em or en dash: {err}"
    );
}

/// `--plan-only` lists the tool set and each tool's state, from the same computation, and only
/// reads the tooling folder: nothing is downloaded or installed.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn plan_only_lists_the_tools_and_their_state_without_installing_anything() {
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = tempfile::tempdir().unwrap();
    stage_python_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let tools = FakeTools::new();

    let printed = Mutex::new(String::new());
    let verdict = run_plan_only_with_tools(
        args(repo.path(), &out.path().join("inspect.zip"), true),
        true,
        &|t| printed.lock().unwrap().push_str(t),
        &tools.env,
    )
    .await;
    let text = printed.into_inner().unwrap();
    assert!(verdict.is_ok(), "{verdict:?}\n{text}");
    assert!(text.contains("== TOOLS =="), "{text}");
    assert!(
        text.contains("ruff 0.17.5 (for ruff): will be installed"),
        "{text}"
    );
    assert!(
        text.contains("osv-scanner v1.9.2 (for osv-scanner): will be installed"),
        "{text}"
    );
    assert!(text.contains("tooling folder:"), "{text}");
    assert!(
        tools.source.fetched().is_empty(),
        "a plan downloads nothing"
    );
    assert!(!tools.tooling().exists(), "a plan installs nothing");

    // Installed now: the same plan reads it as ready.
    tools.env.ensure_ruff().await.unwrap();
    let printed = Mutex::new(String::new());
    run_plan_only_with_tools(
        args(repo.path(), &out.path().join("inspect.zip"), true),
        true,
        &|t| printed.lock().unwrap().push_str(t),
        &tools.env,
    )
    .await
    .unwrap();
    let text = printed.into_inner().unwrap();
    assert!(text.contains("ruff 0.17.5 (for ruff): ready"), "{text}");
}

/// On a machine Camerata has no builds for, the plan says each tool cannot be provisioned and
/// that a client run would not start; an internal plan still passes.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn plan_only_says_a_client_run_would_not_start_when_a_tool_cannot_be_provisioned() {
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = tempfile::tempdir().unwrap();
    stage_python_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let fakes = FakeTools::new();
    let pins = fakes.env.clone();
    let no_platform =
        ToolEnv::new(fakes.tooling(), None, fakes.source.clone()).with_pins(move |a| pins.pins(a));

    let printed = Mutex::new(String::new());
    let verdict = run_plan_only_with_tools(
        args(repo.path(), &out.path().join("inspect.zip"), true),
        true,
        &|t| printed.lock().unwrap().push_str(t),
        &no_platform,
    )
    .await;
    let text = printed.into_inner().unwrap();
    let err = verdict
        .expect_err("a client run would not start")
        .to_string();
    assert!(err.contains("would not start"), "{err}");
    assert!(text.contains("cannot be provisioned"), "{text}");

    let internal = run_plan_only_with_tools(
        args(repo.path(), &out.path().join("inspect.zip"), false),
        true,
        &|_| {},
        &no_platform,
    )
    .await;
    assert!(internal.is_ok(), "{internal:?}");
}

/// The real `camerata` binary, offline (`CAMERATA_OFFLINE=1`) and pointed at a tooling folder
/// seeded with fake tools under the real pins (`CAMERATA_TOOLING_DIR`), lists every tool as ready:
/// the setup every test that starts the binary uses, so none of them can download.
#[test]
fn the_binary_offline_over_a_seeded_tooling_folder_lists_every_tool_ready() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    std::fs::create_dir_all(&repo).unwrap();
    stage_python_repo(&repo);
    let tooling = root.path().join("tooling");
    {
        let tooling = tooling.clone();
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(
                camerata_server::pinned_tools::fixtures::seed_fakes_under_the_real_pins(&tooling),
            );
        })
        .join()
        .unwrap();
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_camerata"))
        .args([
            "inspect",
            "--plan-only",
            "--internal",
            "--client",
            "Acme Widgets",
            "--repo-name",
            "acme/api",
            "--repo",
        ])
        .arg(&repo)
        .arg("--export")
        .arg(root.path().join("inspect.zip"))
        .env("CAMERATA_STATE_DIR", root.path().join("state"))
        .env("CAMERATA_NO_KEYCHAIN", "1")
        .env("CAMERATA_OFFLINE", "1")
        .env("CAMERATA_TOOLING_DIR", &tooling)
        .env("ANTHROPIC_API_KEY", "dummy-key-not-a-real-credential")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("ruff 0.17.5 (for ruff): ready"), "{stdout}");
    assert!(
        stdout.contains("osv-scanner v1.9.2 (for osv-scanner): ready"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("tooling folder: {}", tooling.display())),
        "{stdout}"
    );
}
