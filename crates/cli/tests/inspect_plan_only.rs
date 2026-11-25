#![allow(clippy::unwrap_used)]
//! `camerata inspect --plan-only`: the figures of a run, with no model call, no network, no
//! credential read and nothing saved. The model handed in as a seam panics if it is called, the
//! state folder is listed before and after, and the printed text is searched for file text.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use camerata::inspect_cmd::finder::FinderArgs;
use camerata::inspect_cmd::plan_only::run_plan_only_with;
use camerata::inspect_cmd::{InspectArgs, InspectError, Seams};
use camerata_server::ai_audit::agent_finder::FinderChoice;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

/// A model that fails the test the moment anything calls it.
struct NeverCalled;

#[async_trait]
impl LlmPort for NeverCalled {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        panic!("--plan-only called the model");
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        panic!("--plan-only called the model");
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A marker that sits in the fixture's file text and must never reach the printed output.
const MARKER: &str = "acme_fixture_body_marker_7d41";

fn stage_repo(dest: &Path, files: usize) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(
        dest.join("package.json"),
        r#"{"name":"svc","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        dest.join("src/index.ts"),
        format!(
            "import {{ helper }} from './util';\n// {MARKER}\nexport const run = () => helper();\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dest.join("src/util.ts"),
        format!("// {MARKER}\nexport const helper = () => 1;\n"),
    )
    .unwrap();
    for i in 0..files {
        std::fs::write(
            dest.join(format!("src/part{i}.ts")),
            format!("// {MARKER}\nexport const part{i} = {i};\n"),
        )
        .unwrap();
    }
}

fn args(repos: &[(&Path, &str)], export: &Path, cap: Option<f64>) -> InspectArgs {
    InspectArgs {
        repo: repos[0].0.to_path_buf(),
        also_repos: repos[1..].iter().map(|(p, _)| p.to_path_buf()).collect(),
        repo_names: repos.iter().map(|(_, n)| (*n).to_string()).collect(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: false,
        verbose: false,
        max_cost_usd: cap,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        client_engagement: true,
        run_client_tooling: false,
        consensus: false,
        finder: FinderArgs {
            finder: FinderChoice::Agent,
            ..FinderArgs::default()
        },
        exclude: Vec::new(),
    }
}

/// Every path under `dir`, relative, sorted.
fn listing(dir: &Path) -> Vec<PathBuf> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries {
            let path = e.unwrap().path();
            out.push(path.strip_prefix(base).unwrap().to_path_buf());
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Run `--plan-only` with the never-called model and return what it printed and its result.
async fn plan(args: InspectArgs) -> (String, Result<(), InspectError>) {
    let printed = Mutex::new(String::new());
    let model = NeverCalled;
    let result = run_plan_only_with(
        args,
        true,
        Seams {
            resume: None,
            llm: Some(&model),
            preflight_sink: &|text| printed.lock().unwrap().push_str(text),
        },
    )
    .await;
    (printed.into_inner().unwrap(), result)
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn plan_only_prints_the_figures_and_changes_nothing() {
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    stage_repo(&repo, 3);
    let out = tempfile::tempdir().unwrap();
    let before = listing(state.path());

    let (text, result) = plan(args(
        &[(&repo, "acme/api")],
        &out.path().join("inspect.zip"),
        None,
    ))
    .await;

    result.expect("nothing blocks, so plan-only succeeds");
    for want in [
        "acme/api: 6 files admitted",
        "total: 6 files admitted",
        "left out:",
        "piece 1 of 1",
        "context 0 files",
        "checklist",
        "first request about",
        "turn limit",
        "projected, API prices",
        "spend limit it would run under: none",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(
        !text.contains(MARKER),
        "file text reached the output:\n{text}"
    );
    assert_eq!(listing(state.path()), before, "the state folder changed");
    assert!(listing(out.path()).is_empty(), "an export was written");
    println!("{text}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_spend_limit_under_the_projection_is_refused_with_its_reason() {
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    stage_repo(&repo, 2);
    let out = tempfile::tempdir().unwrap();
    let before = listing(state.path());
    // A limit far below any projection.
    let tiny_limit = 0.01;

    let (text, result) = plan(args(
        &[(&repo, "acme/api")],
        &out.path().join("inspect.zip"),
        Some(tiny_limit),
    ))
    .await;

    let err = result.expect_err("a limit under the projection must refuse");
    let why = err.to_string();
    assert!(why.contains("spend limit"), "{why}");
    assert!(why.contains("projected, API prices"), "{why}");
    assert!(
        text.contains("WARNING: the spend limit is under the projection"),
        "{text}"
    );
    assert!(
        text.contains("piece 1 of 1"),
        "the figures still print:\n{text}"
    );
    assert_eq!(listing(state.path()), before, "the state folder changed");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn several_repos_get_their_own_figures_and_a_total() {
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("api");
    let b = root.path().join("web");
    stage_repo(&a, 1);
    stage_repo(&b, 4);
    let out = tempfile::tempdir().unwrap();
    let before = listing(state.path());

    let (text, result) = plan(args(
        &[(&a, "acme/api"), (&b, "acme/web")],
        &out.path().join("inspect.zip"),
        None,
    ))
    .await;

    result.expect("nothing blocks");
    for want in [
        "acme/api: 4 files admitted",
        "acme/web: 7 files admitted",
        "total: 11 files admitted",
        " acme/api:\n  piece 1 of 1",
        " acme/web:\n  piece 1 of 1",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains(MARKER));
    assert_eq!(
        listing(state.path()),
        before,
        "a multi-repo plan must not record which folders the run covers"
    );
}

/// The real binary: the flag is wired, it exits 0 with the figures, exits non-zero with the reason
/// under a tiny limit, and leaves the state folder as it was. No API key is set and the keychain
/// seam is on, so any attempt to call the provider would have nothing to call it with.
#[test]
fn the_binary_exits_zero_with_figures_and_nonzero_under_a_small_limit() {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    stage_repo(&repo, 2);
    let out = tempfile::tempdir().unwrap();
    let before = listing(state.path());
    let run = |extra: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_camerata"))
            .args([
                "inspect",
                "--plan-only",
                "--internal",
                "--client",
                "Example Client Ltd",
            ])
            .args(["--repo-name", "acme/api", "--repo"])
            .arg(&repo)
            .arg("--export")
            .arg(out.path().join("inspect.zip"))
            .args(extra)
            .env("CAMERATA_STATE_DIR", state.path())
            .env("CAMERATA_NO_KEYCHAIN", "1")
            .env("CAMERATA_DISABLE_DEP_AUDIT", "1")
            .env_remove("ANTHROPIC_API_KEY")
            .output()
            .unwrap()
    };

    let ok = run(&[]);
    let stdout = String::from_utf8_lossy(&ok.stdout);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(stdout.contains("total: 5 files admitted"), "{stdout}");
    assert!(stdout.contains("piece 1 of 1"), "{stdout}");
    assert!(!stdout.contains(MARKER));

    let refused = run(&["--max-cost-usd", "0.01"]);
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("spend limit is under the projection"),
        "{stderr}"
    );
    assert!(String::from_utf8_lossy(&refused.stdout).contains("piece 1 of 1"));

    assert_eq!(listing(state.path()), before, "the state folder changed");
    assert!(listing(out.path()).is_empty(), "an export was written");
}
