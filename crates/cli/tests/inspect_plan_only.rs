#![allow(clippy::unwrap_used)]
//! `camerata inspect --plan-only`: the figures of a run, with no model call, no network, no
//! credential read and nothing saved. The spawned binary runs with a made-up key and every proxy
//! variable pointed at a local listener that must see no connection; the state folder and the
//! repository folders are compared by path and content hash before and after; the printed text
//! is searched for file text; and the figures are compared with what a real run does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::finder::FinderArgs;
use camerata::inspect_cmd::plan_only::run_plan_only_with;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, InspectError, Seams};
use camerata_server::ai_audit::agent_finder::run::with_agent_override;
use camerata_server::ai_audit::agent_finder::{
    AgentRun, AgentStop, FinderChoice, FollowUp, PieceAgent, PieceLimits, Room,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

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

/// Run `--plan-only` and return what it printed and its result.
async fn plan(args: InspectArgs) -> (String, Result<(), InspectError>) {
    let printed = Mutex::new(String::new());
    let result =
        run_plan_only_with(args, true, &|text| printed.lock().unwrap().push_str(text)).await;
    (printed.into_inner().unwrap(), result)
}

/// Every path under `dir` with a hash of its content (0 for a folder), so a changed, added or
/// removed file shows.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, u64> {
    use std::hash::{Hash, Hasher};
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, u64>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries {
            let path = e.unwrap().path();
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            if path.is_dir() {
                out.insert(rel, 0);
                walk(base, &path, out);
            } else {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::fs::read(&path).unwrap().hash(&mut h);
                out.insert(rel, h.finish());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
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
    let before = snapshot(state.path());

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
    assert_eq!(snapshot(state.path()), before, "the state folder changed");
    assert!(snapshot(out.path()).is_empty(), "an export was written");
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
    let before = snapshot(state.path());
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
    assert_eq!(snapshot(state.path()), before, "the state folder changed");
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
    let before = snapshot(state.path());

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
        snapshot(state.path()),
        before,
        "a multi-repo plan must not record which folders the run covers"
    );
}

/// The real binary: the flag is wired, it exits 0 with the figures, exits non-zero with the reason
/// under a tiny limit, and leaves the state folder as it was. A made-up key is set and every proxy
/// variable points at a local listener, so an attempt to reach the provider would arrive there:
/// the listener must see no connection.
#[test]
fn the_binary_exits_zero_with_figures_makes_no_connection_and_refuses_under_a_small_limit() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    stage_repo(&repo, 2);
    let out = tempfile::tempdir().unwrap();
    let before = snapshot(state.path());
    let repo_before = snapshot(&repo);
    let run = |extra: &[&str]| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_camerata"));
        cmd.args([
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
        .env("ANTHROPIC_API_KEY", "dummy-key-not-a-real-credential")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
        for var in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            cmd.env(var, &proxy);
        }
        cmd.output().unwrap()
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

    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "something connected to the proxy: the plan reached for the network"
    );
    assert_eq!(snapshot(state.path()), before, "the state folder changed");
    assert_eq!(snapshot(&repo), repo_before, "the repository changed");
    assert!(snapshot(out.path()).is_empty(), "an export was written");
}

/// A baseline the real loader deletes (outdated), one it keeps (current), and the legacy cache
/// file a real run sweeps out of the repository: plan-only touches none of them.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn plan_only_leaves_baselines_and_legacy_files_alone() {
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
    let root = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    for (name, baseline) in [("old", r#"{"version":0}"#), ("current", r#"{"version":3}"#)] {
        let repo = root.path().join(name);
        stage_repo(&repo, 1);
        std::fs::write(repo.join(".camerata-scan-cache.json"), "{}").unwrap();
        let cache = camerata_server::scan_cache::state_dir::manifest_cache_path(&repo);
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, baseline).unwrap();
        let (state_before, repo_before) = (snapshot(state.path()), snapshot(&repo));

        // A current baseline with the agent finder is a refusal a real run makes; either way
        // nothing may change.
        let _ = plan(args(
            &[(&repo, "acme/api")],
            &out.path().join("inspect.zip"),
            None,
        ))
        .await;

        assert_eq!(
            snapshot(state.path()),
            state_before,
            "{name}: the state folder changed"
        );
        assert_eq!(
            snapshot(&repo),
            repo_before,
            "{name}: the repository changed"
        );
        assert!(cache.exists(), "{name}: the baseline was removed");
    }
}

/// What a real run prints to stderr at the start, and its mode notice, are in the plan's own
/// warnings, so "warnings: none" is only said when it is true.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_start_warning_is_in_the_plans_warnings() {
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    stage_repo(&repo, 1);
    // A file over the read limit is a warning at the start of a real run, never a refusal.
    std::fs::write(repo.join("src/big.ts"), "x".repeat(1_100_000)).unwrap();
    let out = tempfile::tempdir().unwrap();

    let (text, result) = plan(args(
        &[(&repo, "acme/api")],
        &out.path().join("inspect.zip"),
        None,
    ))
    .await;

    result.expect("a warning is not a refusal");
    assert!(!text.contains("warnings: none"), "{text}");
    assert!(
        text.contains("warning: ") || text.contains("warnings ("),
        "{text}"
    );
    assert!(
        text.contains("big.ts"),
        "the warning names the file:\n{text}"
    );
}

/// The engine finder on a batch with a limit under the estimate: the run's own gate refuses the
/// submission, so plan-only refuses with the same reason.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_engine_finder_on_a_batch_under_a_small_limit_is_refused_like_a_run() {
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("api");
    stage_repo(&repo, 1);
    let out = tempfile::tempdir().unwrap();
    let mut a = args(
        &[(&repo, "acme/api")],
        &out.path().join("inspect.zip"),
        Some(0.01),
    );
    a.finder.finder = FinderChoice::Engine;
    a.client_engagement = false;
    a.batch = true;

    let (text, result) = plan(a).await;

    let why = result.expect_err("the batch gate refuses").to_string();
    assert!(why.contains("batch NOT submitted"), "{why}");
    assert!(text.contains("WARNING: batch NOT submitted"), "{text}");
}

// ── the drift test: the plan against a real run over the same fixture ────────────────────────

/// A repository of four large files in one folder, so the agent finder cuts it into two pieces.
fn stage_big_repo(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(
        dest.join("package.json"),
        r#"{"name":"svc","version":"1.0.0"}"#,
    )
    .unwrap();
    for f in 0..4 {
        let mut body = String::from("import { shared } from './shared';\n");
        for i in 0..2_500 {
            body.push_str(&format!(
                "export const value_{f}_{i} = (x: number) => shared(x) + {f} * {i};\n"
            ));
        }
        std::fs::write(dest.join(format!("src/part{f}.ts")), body).unwrap();
    }
    std::fs::write(
        dest.join("src/shared.ts"),
        "export const shared = (x: number) => x;\n",
    )
    .unwrap();
}

/// A model for the stages the fixture reaches; the finder's agent is scripted below.
struct Quiet;

#[async_trait]
impl LlmPort for Quiet {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.01),
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

/// An agent that records the room and limits each piece is given, then stops the piece.
struct Recorder {
    seen: Arc<Mutex<Vec<(usize, usize, usize, usize)>>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl PieceAgent for Recorder {
    async fn run(
        &self,
        room: &Room,
        _brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let checklist = room
            .read_plain("_camerata/checklist.md", 1, usize::MAX)
            .map(|r| r.text.len())
            .unwrap_or(0);
        self.seen.lock().unwrap().push((
            room.piece_files().len(),
            room.context_files().len(),
            checklist,
            limits.max_turns,
        ));
        Ok(AgentRun::empty(AgentStop::Failed("recorded".to_string())))
    }
    async fn follow_up(
        &self,
        _room: &Room,
        _prior: &AgentRun,
        _turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        anyhow::bail!("not used")
    }
}

/// Run a real pipeline over `repo` with the recorder; returns the pre-flight texts it printed,
/// what each piece was given, and a debug dump of the result (where a pause note would show).
async fn real_run(
    repo: &Path,
    cap: Option<f64>,
) -> (Vec<String>, Vec<(usize, usize, usize, usize)>, String) {
    let out = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Recorder {
        seen: seen.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let printed = Mutex::new(Vec::<String>::new());
    let mut a = args(&[(repo, "acme/api")], &out.path().join("inspect.zip"), cap);
    // No baseline is read, so a run saved by an earlier call cannot change what this one does.
    a.full = true;
    let result = Box::pin(with_agent_override(
        Arc::new(agent),
        run_inspect_pipeline_with(
            a,
            true,
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(&Quiet),
                preflight_sink: &|t| printed.lock().unwrap().push(t.to_string()),
            },
        ),
    ))
    .await;
    let seen = seen.lock().unwrap().clone();
    (printed.into_inner().unwrap(), seen, format!("{result:?}"))
}

fn on_a_big_stack<F: std::future::Future<Output = ()> + Send + 'static>(fut: F) {
    std::thread::Builder::new()
        .stack_size(128 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(fut);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The plan reports what the real run cuts: the same pre-flight text, the same pieces with the
/// same files, context, checklist and turn limit, and the same pause under a tiny limit.
#[test]
#[serial_test::serial(process_env)]
fn the_plan_matches_what_a_real_run_cuts_and_says() {
    on_a_big_stack(async {
        let state = tempfile::tempdir().unwrap();
        let _state =
            camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
        let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
        let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("api");
        stage_big_repo(&repo);
        let out = tempfile::tempdir().unwrap();

        let planned_args = |cap: Option<f64>| {
            let mut a = args(&[(&repo, "acme/api")], &out.path().join("inspect.zip"), cap);
            a.full = true;
            a
        };
        let (text, result) = plan(planned_args(None)).await;
        result.expect("nothing blocks");
        let (printed, seen, _) = real_run(&repo, None).await;

        let real_preflight = printed
            .iter()
            .find(|t| t.contains("== PRE-FLIGHT ESTIMATE"))
            .expect("the real run prints its pre-flight");
        assert!(
            text.contains(real_preflight.as_str()),
            "the pre-flight text differs:\n{text}"
        );
        let planned = text.lines().filter(|l| l.starts_with("  piece ")).count();
        assert!(
            planned >= 2,
            "the fixture should be cut in two or more:\n{text}"
        );
        assert_eq!(
            planned,
            seen.len(),
            "the plan and the real run cut different piece counts"
        );
        // Each real piece's files, context files, checklist bytes and turn limit appear in the
        // plan's matching piece line.
        for (files, ctx, checklist, turns) in &seen {
            let hit = text.lines().any(|l| {
                l.starts_with("  piece ")
                    && l.contains(&format!("): {} files,", camerata_n(*files)))
                    && l.contains(&format!("context {ctx} files,"))
                    && l.contains(&format!("checklist {} bytes", camerata_n(*checklist)))
                    && l.ends_with(&format!("turn limit {turns}"))
            });
            assert!(
                hit,
                "no plan line for the real piece {files}/{ctx}/{checklist}/{turns}:\n{text}"
            );
        }
        // The pre-flight's own pieces table agrees with the cut.
        assert!(
            text.contains(&format!("{} piece(s) and", seen.len())),
            "the pre-flight table counts a different number of pieces than the run cuts:\n{text}"
        );

        // The same pause under a tiny limit.
        let tiny = 0.01;
        let (_, small, dump) = real_run(&repo, Some(tiny)).await;
        assert!(small.is_empty(), "no piece may start under the tiny limit");
        let (_, refused) = plan(planned_args(Some(tiny))).await;
        let why = refused.expect_err("refused").to_string();
        let pause = &why[why
            .find("Paused before going over")
            .expect("the pause message")..];
        assert!(
            dump.contains(pause),
            "the real run's pause differs from the plan's:\n{pause}\n{dump}"
        );
    });
}

/// Thousands separators the way the plan prints them.
fn camerata_n(v: usize) -> String {
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
