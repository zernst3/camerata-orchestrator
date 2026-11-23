#![allow(clippy::unwrap_used)]
//! `camerata inspect --finder agent`, end to end over the synthetic fixture repo with a SCRIPTED
//! agent and a scripted model (no network, no spend): the agent finder's problems go through the
//! group reviewer, the review tree, the exports and the release gate.
//!
//! * The package passes the gate; nothing downstream needs a second rater to have rated the
//!   agent's problems.
//! * An agent problem whose `code` is not in the file is caught by the evidence check.
//! * With a failed part a client export is refused (exit 6, no zip) and an internal export is
//!   written marked incomplete, which the release gate refuses.
//! * The settings that cannot go together are refused before the agent or the model is called.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::finder::FinderArgs;
use camerata::inspect_cmd::{
    run_inspect_pipeline_with, InspectArgs, InspectError, Seams, EXIT_INCOMPLETE_RUN,
};
use camerata_server::ai_audit::agent_finder::run::with_agent_override;
use camerata_server::ai_audit::agent_finder::{
    AgentRun, AgentStop, FinderChoice, FollowUp, PieceAgent, PieceLimits, Room, ScriptStep,
    ScriptedAgent, TurnUsage,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};
use serde_json::{json, Value};

/// The scripted model for the stages that stay under the agent finder (group review, fix).
struct Scripted {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            text: camerata_server::ai_audit::group_review::stand_in_reply(req.system.as_deref(), &req.prompt)
                .unwrap_or_else(|| r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string()),
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

/// A scripted agent that reads every file of its piece, writes `problems`, answers every rule on
/// the checklist it finds in its room `clean` at a line it read, and ends as `stop` says.
struct Reviewer {
    problems: Vec<Value>,
    stop: AgentStop,
    calls: Arc<AtomicUsize>,
    /// Cite problem 1 to the first two rules of the checklist (and answer them `problems`).
    cite_to_two_rules: bool,
    /// The rules problem 1 was cited to.
    cited_seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Reviewer {
    fn new(problems: Vec<Value>) -> Self {
        Self {
            problems,
            stop: AgentStop::Finished,
            calls: Arc::new(AtomicUsize::new(0)),
            cite_to_two_rules: false,
            cited_seen: Arc::default(),
        }
    }

    fn script(&self, room: &Room) -> Vec<ScriptStep> {
        let counts = room.piece_line_counts();
        let mut steps: Vec<ScriptStep> = counts
            .iter()
            .map(|(p, n)| ScriptStep::Read {
                path: p.clone(),
                first_line: 1,
                count: *n,
            })
            .collect();
        let checklist = room
            .read("_camerata/checklist.md", 1, 100_000)
            .map(|r| r.text)
            .unwrap_or_default();
        let first = counts.first().map(|(p, _)| p.clone()).unwrap_or_default();
        let ids: Vec<String> = checklist
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, rest)| rest))
            .filter_map(|l| l.strip_prefix("## "))
            .filter_map(|l| l.split_once(": ").map(|(id, _)| id.to_string()))
            .collect();
        let cited: Vec<String> = if self.cite_to_two_rules {
            ids.iter().take(2).cloned().collect()
        } else {
            Vec::new()
        };
        *self.cited_seen.lock().unwrap() = cited.clone();
        let citations: Vec<Value> = cited.iter().map(|r| json!({"n": 1, "rule": r})).collect();
        let answers: Vec<Value> = checklist
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, rest)| rest))
            .filter_map(|l| l.strip_prefix("## "))
            .filter_map(|l| l.split_once(": ").map(|(id, _)| id.to_string()))
            .map(|id| {
                if cited.contains(&id) {
                    json!({"rule": id, "answer": "problems", "problems": [1]})
                } else {
                    json!({"rule": id, "answer": "clean", "places": [{"path": first, "line": 1}]})
                }
            })
            .collect();
        let out = |file: &str, v: &Value| ScriptStep::Write {
            path: format!("_camerata/out/{file}"),
            text: v.to_string(),
        };
        steps.push(out("problems-1.json", &Value::Array(self.problems.clone())));
        steps.push(out(
            "checklist.json",
            &json!({"citations": citations, "answers": answers}),
        ));
        steps.push(ScriptStep::Write {
            path: "_camerata/out/card.md".to_string(),
            text: "It trusts its callers.".to_string(),
        });
        steps.push(out("unread.json", &json!([])));
        steps
    }
}

#[async_trait]
impl PieceAgent for Reviewer {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ScriptedAgent::new(self.script(room))
            .with_stop(self.stop.clone())
            .with_usage(TurnUsage {
                input_tokens: 5_000,
                output_tokens: 500,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: Some(0.05),
            })
            .run(room, brief, limits)
            .await
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        ScriptedAgent::new(vec![])
            .follow_up(room, prior, turn, limits)
            .await
    }
}

fn problem(n: u32, path: &str, line: usize, code: &str, label: &str) -> Value {
    json!({
        "n": n, "path": path, "line": line, "end_line": line, "severity": "high",
        "title": format!("Problem {n} in the example code"), "code": code,
        "what": "The query is built from the caller's text.", "why": "A caller can change the query.",
        "fix": "Bind the value instead.", "sure": "yes", "effort": "low", "label": label
    })
}

/// Run `fut` on a runtime with a large stack: the whole pipeline is one deep chain of futures and
/// the default test thread stack overflows in a debug build.
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

fn typst_available(test: &str) -> bool {
    camerata_test_env::typst_on_path_for(test)
}

fn stage_fixture(dest: &Path) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    fn copy(src: &Path, dest: &Path) {
        std::fs::create_dir_all(dest).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dest.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    copy(&root, dest);
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "agent-finder-e2e@camerata.local"]);
    g(&["config", "user.name", "Agent Finder E2E"]);
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
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: vec!["warm_schedule=off".to_string()],
        client: "Example Client Ltd".to_string(),
        client_engagement,
        run_client_tooling: false,
        consensus: false,
        finder: FinderArgs {
            finder: FinderChoice::Agent,
            ..FinderArgs::default()
        },
        exclude: Vec::new(),
    }
}

struct Run {
    result: Result<camerata::inspect_cmd::InspectOutcome, InspectError>,
    export: PathBuf,
    model_calls: usize,
    _out: tempfile::TempDir,
    _repo: tempfile::TempDir,
}

async fn run_agent(
    agent: Arc<dyn PieceAgent>,
    client_engagement: bool,
    tweak: impl FnOnce(&mut InspectArgs),
) -> Run {
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let llm = Scripted {
        calls: AtomicUsize::new(0),
    };
    let mut a = args(repo.path(), &export, client_engagement);
    tweak(&mut a);
    let result = Box::pin(with_agent_override(
        agent,
        run_inspect_pipeline_with(
            a,
            true,
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(&llm),
                preflight_sink: &|_| {},
            },
        ),
    ))
    .await;
    Run {
        result,
        export,
        model_calls: llm.calls.load(Ordering::SeqCst),
        _out: out,
        _repo: repo,
    }
}

fn zip_entry(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn ledger_of(
    outcome: &camerata::inspect_cmd::InspectOutcome,
) -> camerata_server::scan_ledger::ScanLedger {
    serde_json::from_slice(&std::fs::read(&outcome.ledger_path).unwrap()).unwrap()
}

fn real_problem() -> Value {
    problem(
        1,
        "src/db.rs",
        9,
        "format!(\"SELECT * FROM users WHERE id = {user_id}\")",
        "SEC-NO-RAW-SQL-CONCAT-1",
    )
}

#[test]
#[serial_test::serial(process_env)]
fn the_agent_finders_package_passes_the_release_gate() {
    on_a_big_stack(async {
        if !typst_available("the_agent_finders_package_passes_the_release_gate") {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let agent = Arc::new(Reviewer::new(vec![real_problem()]));
        let calls = agent.calls.clone();
        // The operator's own (internal) package: the fixture's three critical problems have no
        // reviewer decision, which refuses a client package (the test below), and the headless
        // command has no way to record one.
        let run = run_agent(agent, false, |_| {}).await;
        let outcome = run.result.expect("the agent finder's package is released");
        assert!(
            outcome.release_gate.starts_with("release gate: passed"),
            "{}",
            outcome.release_gate
        );
        assert!(run.export.exists());
        if let Some(keep) = std::env::var_os("CAMERATA_E2E_KEEP_PACKAGE") {
            let keep = PathBuf::from(keep);
            std::fs::create_dir_all(&keep).unwrap();
            std::fs::copy(&run.export, keep.join("inspect.zip")).unwrap();
        }
        assert!(
            outcome.incomplete_reasons.is_empty(),
            "{:?}",
            outcome.incomplete_reasons
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one piece, one agent call");
        assert!(
            run.model_calls > 0,
            "the group reviewer and the fix stage still ran"
        );
        let ledger = ledger_of(&outcome);
        assert_eq!(ledger.evidence_check().unverified, 0);
        let blocks = ledger.finder_coverage();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].pieces.iter().all(|p| p.complete()), "{blocks:?}");
        let findings = zip_entry(&run.export, "json/inspection.json");
        assert!(
            findings.contains("src/db.rs"),
            "the agent's problem is in the package"
        );
        assert!(!findings.contains("finder-run") && !findings.contains("finder-piece"));
    })
}

/// The release gate refuses a client package while a critical problem has no reviewer decision,
/// naming the places. The headless command records no decision, so a run with a critical problem
/// is exported as an internal package, or reviewed and exported from the cockpit.
#[test]
#[serial_test::serial(process_env)]
fn the_agent_finders_client_package_is_refused_while_a_critical_problem_has_no_decision() {
    on_a_big_stack(async {
        if !typst_available("the_agent_finders_client_package_is_refused_while_a_critical") {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let agent = Arc::new(Reviewer::new(vec![real_problem()]));
        let run = run_agent(agent, true, |_| {}).await;
        let err = run
            .result
            .expect_err("a client package with an undecided critical problem is refused");
        let text = format!("{err:?}");
        assert!(
            text.contains("CriticalOrUnsureWithoutDecision") && text.contains("src/db.rs line 9"),
            "{text}"
        );
        assert!(!run.export.exists(), "nothing is written");
    })
}

/// An agent problem whose `code` is not in the file is caught by the evidence check: the release
/// gate refuses the package, naming the place.
#[test]
#[serial_test::serial(process_env)]
fn an_agent_problem_whose_quote_is_not_in_the_file_is_caught_before_release() {
    on_a_big_stack(async {
        if !typst_available("an_agent_problem_whose_quote_is_not_in_the_file") {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let fake = problem(
            2,
            "src/config.rs",
            6,
            "pub const NEVER_WRITTEN: &str = \"not in this file\";",
            "SEC-NO-HARDCODED-SECRETS-1",
        );
        let agent = Arc::new(Reviewer::new(vec![real_problem(), fake]));
        let run = run_agent(agent, true, |_| {}).await;
        let err = run
            .result
            .expect_err("a package with a quote that is not in the file is refused");
        let text = format!("{err:?}");
        assert!(
            text.contains("EvidenceUnverified") && text.contains("src/config.rs line 6"),
            "{text}"
        );
        assert!(!run.export.exists(), "nothing is written");
    })
}

/// A problem cited to two rules is one row under the first; the second rule has a problem too, so
/// the ledger must not read it as verified clean (the line in `onboard.rs` that adds it).
#[test]
#[serial_test::serial(process_env)]
fn a_rule_a_problem_was_also_cited_to_is_never_verified_clean() {
    on_a_big_stack(async {
        if !typst_available("a_rule_a_problem_was_also_cited_to_is_never_verified_clean") {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let mut reviewer = Reviewer::new(vec![real_problem()]);
        reviewer.cite_to_two_rules = true;
        let cited = reviewer.cited_seen.clone();
        // An internal run: the scripted model has no fix text for the cited rules, which would
        // refuse a client package; the ledger is written either way.
        let run = run_agent(Arc::new(reviewer), false, |_| {}).await;
        let outcome = run.result.expect("an internal package is written");
        let ledger = ledger_of(&outcome);
        let cited = cited.lock().unwrap().clone();
        assert_eq!(cited.len(), 2, "{cited:?}");
        for rule in &cited {
            let entry = ledger.rule(rule).expect("the cited rule is in the ledger");
            assert!(
                !entry.verified_clean(),
                "{rule} has a problem cited to it and must not read as verified clean"
            );
        }
    })
}

/// No agent stands in: the command line hands the run its finder setup and the run builds the
/// real API agent from it on the run's own meter. A model with no price stops that agent before
/// any request (so nothing is sent and nothing is spent), and the reason it gives shows both that
/// the key reached it through the setup (an agent with no key is never built) and that it holds
/// the run's meter (an agent with no meter says so instead).
#[test]
#[serial_test::serial(process_env)]
fn the_run_builds_its_own_agent_from_the_setup_with_the_key_and_the_meter() {
    on_a_big_stack(async {
        if !typst_available("the_run_builds_its_own_agent_from_the_setup") {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let repo = tempfile::tempdir().unwrap();
        stage_fixture(repo.path());
        let out = tempfile::tempdir().unwrap();
        let export = out.path().join("inspect.zip");
        let llm = Scripted {
            calls: AtomicUsize::new(0),
        };
        let creds = camerata_server::credentials::MemoryCredentialStore::new();
        camerata_server::credentials::CredentialStore::set(
            &creds,
            camerata_server::credentials::ANTHROPIC_API_KEY,
            "sk-ant-test-only",
        )
        .unwrap();
        let mut a = args(repo.path(), &export, false);
        a.finder.model = Some("unpriced-test-model".to_string());
        let result = Box::pin(run_inspect_pipeline_with(
            a,
            true,
            &creds,
            Seams {
                resume: None,
                llm: Some(&llm),
                preflight_sink: &|_| {},
            },
        ))
        .await;
        let outcome = result.expect("an internal export is written");
        let lines = outcome.failure_lines.join("\n");
        assert!(
            lines.contains("no price in the model registry"),
            "the run's own agent refused the unpriced model: {lines}"
        );
        assert!(
            !lines.contains("no usage meter") && !lines.contains("needs an Anthropic key"),
            "{lines}"
        );
    })
}

#[test]
#[serial_test::serial(process_env)]
fn a_failed_agent_part_refuses_a_client_export_and_marks_an_internal_one() {
    on_a_big_stack(async {
        if !typst_available("a_failed_agent_part_refuses_a_client_export") {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let failing = || {
            let mut a = Reviewer::new(vec![]);
            a.stop = AgentStop::TurnLimit("60 turns".to_string());
            Arc::new(a)
        };
        // A client run: refused, exit 6, no package.
        let client = Box::pin(run_agent(failing(), true, |_| {})).await;
        let err = client
            .result
            .expect_err("a client export with a failed part is refused");
        assert_eq!(err.exit_code(), EXIT_INCOMPLETE_RUN);
        assert!(!client.export.exists(), "no zip is written for a client");
        // An internal run: written, marked incomplete, and the gate refuses that zip.
        let internal = Box::pin(run_agent(failing(), false, |_| {})).await;
        let outcome = internal.result.expect("an internal export is written");
        assert!(!outcome.incomplete_reasons.is_empty());
        assert_eq!(outcome.exit_code(), EXIT_INCOMPLETE_RUN);
        assert!(internal.export.exists());
        let manifest: Value =
            serde_json::from_str(&zip_entry(&internal.export, "manifest.json")).unwrap();
        assert_eq!(manifest["inspection"]["run_incomplete"], true);
        assert!(zip_entry(&internal.export, "README.txt").contains("INTERNAL INCOMPLETE"));
        let bytes = std::fs::read(&internal.export).unwrap();
        assert!(!camerata_server::release_gate::audit_zip(&bytes, None).is_clean());
        // The failed part is on the operator's failure report with its files.
        assert!(
            !outcome.failure_lines.is_empty(),
            "the failed part is reported"
        );
    })
}

#[test]
#[serial_test::serial(process_env)]
fn settings_that_cannot_go_together_are_refused_before_the_agent_or_the_model_is_called() {
    on_a_big_stack(async {
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");

        // The consensus option.
        let agent = Arc::new(Reviewer::new(vec![]));
        let calls = agent.calls.clone();
        let run = run_agent(agent, false, |a| a.consensus = true).await;
        let why = run.result.expect_err("refused").to_string();
        assert!(
            why.contains("--finder agent") && why.contains("--consensus"),
            "{why}"
        );
        assert_eq!((calls.load(Ordering::SeqCst), run.model_calls), (0, 0));

        // Forced pieces on a client engagement.
        let agent = Arc::new(Reviewer::new(vec![]));
        let calls = agent.calls.clone();
        let run = run_agent(agent, true, |a| a.finder.force_pieces = Some(4)).await;
        let why = run.result.expect_err("refused").to_string();
        assert!(
            why.contains("--finder-force-pieces") && why.contains("--internal"),
            "{why}"
        );
        assert_eq!((calls.load(Ordering::SeqCst), run.model_calls), (0, 0));

        // An earlier inspection of the same code exists, so the run would be incremental.
        let repo = tempfile::tempdir().unwrap();
        stage_fixture(repo.path());
        let manifest = camerata_server::scan_cache::ManifestBuilder::new().finish();
        let path = camerata_server::scan_cache::state_dir::manifest_cache_path(repo.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let export = out.path().join("inspect.zip");
        let agent = Arc::new(Reviewer::new(vec![]));
        let calls = agent.calls.clone();
        let llm = Scripted {
            calls: AtomicUsize::new(0),
        };
        let mut a = args(repo.path(), &export, false);
        a.full = false;
        let result = Box::pin(with_agent_override(
            agent,
            run_inspect_pipeline_with(
                a,
                true,
                &camerata_server::credentials::MemoryCredentialStore::new(),
                Seams {
                    resume: None,
                    llm: Some(&llm),
                    preflight_sink: &|_| {},
                },
            ),
        ))
        .await;
        let why = result
            .expect_err("an incremental agent run is refused")
            .to_string();
        assert!(
            why.contains("full inspection") && why.contains("--full"),
            "{why}"
        );
        assert_eq!(
            (
                calls.load(Ordering::SeqCst),
                llm.calls.load(Ordering::SeqCst)
            ),
            (0, 0),
            "refused before any call"
        );
        assert!(!export.exists());
    })
}

/// Outside review Part 1 item 3: under the agent finder the pre-flight prints the agent finder's
/// estimate, never the engine's scan figure.
#[test]
#[serial_test::serial(process_env)]
fn the_agent_finder_prints_its_own_estimate_never_the_engines() {
    on_a_big_stack(async {
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let repo = tempfile::tempdir().unwrap();
        stage_fixture(repo.path());
        let out = tempfile::tempdir().unwrap();
        let export = out.path().join("inspect.zip");
        let seen: Mutex<String> = Mutex::new(String::new());
        let llm = Scripted {
            calls: AtomicUsize::new(0),
        };
        let agent = Arc::new(Reviewer::new(vec![real_problem()]));
        let _ = Box::pin(with_agent_override(
            agent,
            run_inspect_pipeline_with(
                args(repo.path(), &export, false),
                true,
                &camerata_server::credentials::MemoryCredentialStore::new(),
                Seams {
                    resume: None,
                    llm: Some(&llm),
                    preflight_sink: &|text| seen.lock().unwrap().push_str(text),
                },
            ),
        ))
        .await;
        let text = seen.lock().unwrap().clone();
        assert!(text.contains("PRE-FLIGHT ESTIMATE"), "{text}");
        assert!(
            text.contains("finder: agent. Estimated cost of the finder stage only"),
            "{text}"
        );
        assert!(
            !text.contains("estimated cost: about"),
            "the engine's figure must not print under the agent finder: {text}"
        );
    })
}

/// Outside review Part 1 item 4: a rule file that does not parse stops an agent run before
/// the estimate, the agent and any model call, and names the rule, the file and the reason.
#[test]
#[serial_test::serial(process_env)]
fn a_corrupt_rule_file_stops_every_agent_run_before_any_spend() {
    on_a_big_stack(async {
        let state = tempfile::tempdir().unwrap();
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let corpus = tempfile::tempdir().unwrap();
        let dir = corpus.path().join("sec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sec-corrupt-1.toml"),
            "id = \"SEC-CORRUPT-1\"\n[[[ not toml",
        )
        .unwrap();
        let _corpus = camerata_test_env::EnvGuard::set("CAMERATA_CORPUS_PATH", corpus.path());
        // Client and internal runs alike: graded evidence must not come from a broken rule set.
        for client in [true, false] {
            let agent = Arc::new(Reviewer::new(vec![real_problem()]));
            let calls = agent.calls.clone();
            let run = run_agent(agent, client, |_| {}).await;
            let why = run
                .result
                .expect_err("an agent run must not go on with a rule file missing")
                .to_string();
            assert!(
                why.contains("SEC-CORRUPT-1") && why.contains("sec-corrupt-1.toml"),
                "{why}"
            );
            assert_eq!((calls.load(Ordering::SeqCst), run.model_calls), (0, 0));
            assert!(!run.export.exists());
        }
    })
}
