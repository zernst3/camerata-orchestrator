#![allow(clippy::unwrap_used)]
//! `camerata inspect --rerun-deterministic` (#588), end to end over the synthetic fixture repo with
//! a SCRIPTED agent and a SCRIPTED model (no network, no spend): a saved run's deterministic stages
//! are run again on a "newer build" with zero model calls.
//!
//! * A saved run that failed its pipeline integrity check (a bookkeeping defect injected on the
//!   build that made it) passes on a build without the defect, with zero calls counted on the
//!   scripted model and the scripted agent, and both builds are recorded.
//! * With one stored reply removed the rerun refuses as a whole, names the reply and sends nothing.
//! * A normal resume on another build is still refused.
//! * A rule library that changed since the run refuses the rerun, naming the cause.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use camerata::inspect_cmd::finder::FinderArgs;
use camerata::inspect_cmd::{rerun_deterministic_inspect, resume_inspect};
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, InspectError, Seams};
use camerata_server::ai_audit::agent_finder::run::with_agent_override;
use camerata_server::ai_audit::agent_finder::{
    AgentRun, AgentStop, FinderChoice, FollowUp, PieceAgent, PieceLimits, Room, ScriptStep,
    ScriptedAgent, TurnUsage,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};
use camerata_server::run_store::{read_record, resolve_run, runs_dir, RunOrigin};
use serde_json::{json, Value};

/// The scripted model for the stages that stay under the agent finder (group review, fix).
struct Scripted {
    calls: AtomicUsize,
    /// Answer a fix request with a fix for each finding it names, instead of an empty list.
    fixes: bool,
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: if self.fixes
                && camerata_server::run_store::journal::classify(&req).0
                    == camerata_server::run_store::PartKind::Fix
            {
                let ids: Vec<Value> = req
                    .prompt
                    .lines()
                    .filter_map(|l| l.strip_prefix('['))
                    .filter_map(|l| l.split_once("] rule "))
                    .map(|(id, _)| json!({"id": id, "fix": "Bind the value instead of building the text."}))
                    .collect();
                json!({ "fixes": ids }).to_string()
            } else {
                camerata_server::ai_audit::group_review::stand_in_reply(req.system.as_deref(), &req.prompt)
                .unwrap_or_else(|| r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string())
            },
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

const DECISION: &str = "pub fn FUNC(order: &Order, user: &User) -> f64 {\n    let base = order.subtotal * order.quantity as f64;\n    if user.is_premium {\n        let discount = base * RATE;\n        return base - discount;\n    }\n    if base > 100.0 && order.coupon.is_some() {\n        return base - 10.0;\n    }\n    base + order.shipping\n}\n";

fn stage_fixture_with(dest: &Path, clones: bool) {
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
    if clones {
        std::fs::write(
            dest.join("src/pricing.rs"),
            DECISION
                .replace("FUNC", "apply_discount")
                .replace("RATE", "0.2"),
        )
        .unwrap();
        std::fs::write(
            dest.join("src/quote.rs"),
            DECISION
                .replace("FUNC", "quote_total")
                .replace("RATE", "0.15"),
        )
        .unwrap();
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

/// A saved run that failed its pipeline integrity check: the state folder, the fixture repo and
/// the run's id. The run was made on this build with a lost-row defect injected, then its pin was
/// rewritten to name another build, so "this build" is the newer, fixed one.
struct Saved {
    run_id: String,
    repo: tempfile::TempDir,
    out: tempfile::TempDir,
    _state: tempfile::TempDir,
    _guards: Vec<camerata_test_env::EnvGuard>,
    /// How many agent turns and model calls the first run made, for the "stored" check.
    first_run_agent_calls: usize,
    ledger_path: PathBuf,
}

const BUILD_A: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

fn patch_saved_run(run_id: &str, edit: impl FnOnce(&mut Value)) {
    let path = runs_dir().join(run_id).join("run.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut v);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
}

async fn saved_run_with_a_bookkeeping_defect() -> Saved {
    saved_run_with(false).await
}

/// [`saved_run_with_a_bookkeeping_defect`], with the first run's fix replies accepted by the fix
/// stage's checks when `accepted_fixes` is set.
async fn saved_run_with(accepted_fixes: bool) -> Saved {
    saved_run_full(accepted_fixes, false).await
}

async fn saved_run_full(accepted_fixes: bool, clones: bool) -> Saved {
    let state = tempfile::tempdir().unwrap();
    let guards = vec![
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path()),
        camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
    ];
    let repo = tempfile::tempdir().unwrap();
    stage_fixture_with(repo.path(), clones);
    let out = tempfile::tempdir().unwrap();
    let agent = Arc::new(Reviewer::new(vec![real_problem()]));
    let first_calls = agent.calls.clone();
    let llm = Scripted {
        calls: AtomicUsize::new(0),
        fixes: accepted_fixes,
    };
    let first = {
        // The defect is on only while the first run does its bookkeeping: "the older build".
        let _defect = camerata_server::test_defects::LostRowGuard::set();
        Box::pin(with_agent_override(
            agent,
            run_inspect_pipeline_with(
                args(repo.path(), &out.path().join("first.zip"), false),
                true,
                &camerata_server::credentials::MemoryCredentialStore::new(),
                Seams {
                    resume: None,
                    llm: Some(&llm),
                    preflight_sink: &|_| {},
                },
            ),
        ))
        .await
    };
    let outcome = first.expect("an internal run writes its package, marked incomplete");
    assert!(
        outcome
            .incomplete_reasons
            .iter()
            .any(|r| r.contains("pipeline integrity")),
        "the first run failed its integrity check for the injected defect: {:?}",
        outcome.incomplete_reasons
    );
    assert!(
        llm.calls.load(Ordering::SeqCst) > 0,
        "the first run asked the model"
    );
    let run_id = resolve_run("latest", None, RunOrigin::Cli).unwrap();
    // The run was made by another (older) build.
    patch_saved_run(&run_id, |v| {
        v["pin"]["build"]["commit"] = json!(BUILD_A);
        v["pin"]["build"]["dirty"] = json!(false);
    });
    Saved {
        run_id,
        repo,
        out,
        _state: state,
        _guards: guards,
        first_run_agent_calls: first_calls.load(Ordering::SeqCst),
        ledger_path: outcome.ledger_path.clone(),
    }
}

/// What a rerun did, and what the scripted model and agent were asked during it.
struct Rerun {
    result: Result<camerata::inspect_cmd::InspectOutcome, InspectError>,
    model_calls: usize,
    agent_calls: usize,
    export: PathBuf,
}

async fn rerun(saved: &Saved) -> Rerun {
    let export = saved.out.path().join("rerun.zip");
    let agent = Arc::new(Reviewer::new(vec![real_problem()]));
    let agent_calls = agent.calls.clone();
    let llm = Scripted {
        calls: AtomicUsize::new(0),
        fixes: false,
    };
    let result = Box::pin(with_agent_override(
        agent,
        rerun_deterministic_inspect(
            &saved.run_id,
            &[saved.repo.path().to_path_buf()],
            Some(export.clone()),
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(&llm),
                preflight_sink: &|_| {},
            },
        ),
    ))
    .await;
    Rerun {
        result,
        model_calls: llm.calls.load(Ordering::SeqCst),
        agent_calls: agent_calls.load(Ordering::SeqCst),
        export,
    }
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

/// The stored call files of the run whose stage is `kind` (`group_review`, `fix`, ...).
fn call_files_of(run_id: &str, kind: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(runs_dir().join(run_id).join("calls"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            v["kind"] == kind
        })
        .collect();
    found.sort();
    found
}

/// Acceptance 1: a saved run with an injected bookkeeping defect, rerun on a newer build, passes
/// the integrity gate with zero model calls and records both builds.
#[test]
#[serial_test::serial(process_env)]
fn a_saved_run_with_a_bookkeeping_defect_passes_on_a_newer_build_with_zero_model_calls() {
    on_a_big_stack(async {
        if !typst_available("a_saved_run_with_a_bookkeeping_defect_passes_on_a_newer_build") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        assert!(
            saved.first_run_agent_calls > 0,
            "the first run reviewed the piece"
        );
        let done = rerun(&saved).await;
        let outcome = done
            .result
            .expect("the rerun on the fixed build passes the integrity gate");
        assert!(
            outcome.release_gate.starts_with("release gate: passed"),
            "{}",
            outcome.release_gate
        );
        assert!(
            outcome.incomplete_reasons.is_empty(),
            "{:?}",
            outcome.incomplete_reasons
        );
        assert!(done.export.exists(), "the corrected package is written");
        assert_eq!(done.model_calls, 0, "the scripted model was never asked");
        assert_eq!(done.agent_calls, 0, "the scripted agent was never asked");

        // Both builds are recorded in the run and in the ledger.
        let record = read_record(&saved.run_id).unwrap();
        assert_eq!(record.reruns.len(), 1, "{:?}", record.reruns);
        assert_eq!(
            record.reruns[0].model_build.commit.as_deref(),
            Some(BUILD_A)
        );
        let now = camerata_server::run_store::engine_pin::BuildId::current();
        assert_eq!(record.reruns[0].deterministic_build.commit, now.commit);
        assert_eq!(
            record.pin.as_ref().unwrap().build.commit.as_deref(),
            Some(BUILD_A),
            "the model work stays pinned to the build that made it"
        );
        let ledger = ledger_of(&outcome);
        let in_ledger = ledger.rerun().expect("the ledger names both builds");
        assert_eq!(in_ledger.model_build.commit.as_deref(), Some(BUILD_A));
        assert_eq!(in_ledger.deterministic_build.commit, now.commit);

        // The fix replies the run stored were all turned down by the stage's checks. The rerun
        // replays them, the checks turn them down again, and every row ends as the rule's own
        // standard fix with the reason kept: never a model-written fix.
        let saved_report: Value = serde_json::from_slice(
            &std::fs::read(
                runs_dir()
                    .join(&saved.run_id)
                    .join(camerata_server::run_store::REPORT_FILE),
            )
            .unwrap(),
        )
        .unwrap();
        let (mut rows, mut standard) = (0, 0);
        let mut walk = vec![&saved_report];
        while let Some(v) = walk.pop() {
            match v {
                Value::Object(m) => {
                    if let Some(c) = m.get("captures").and_then(Value::as_object) {
                        rows += 1;
                        if c.get("fix_source").and_then(Value::as_str) == Some("standard")
                            && c.get("fix_rejection")
                                .and_then(Value::as_str)
                                .is_some_and(|r| !r.is_empty())
                        {
                            standard += 1;
                        }
                    }
                    walk.extend(m.values());
                }
                Value::Array(a) => walk.extend(a.iter()),
                _ => {}
            }
        }
        assert!(rows > 0, "the saved report holds rows");
        assert_eq!(
            standard, rows,
            "every row ships the standard fix with its reason"
        );
        // The report's "How this was done" names both builds: see the report_export unit test
        // (the PDF's methodology text is not readable from the package).
    })
}

/// Acceptance 2: with one stored reply removed the rerun refuses, names it and makes no call.
#[test]
#[serial_test::serial(process_env)]
fn a_rerun_with_a_stored_reply_removed_refuses_names_it_and_makes_no_call() {
    on_a_big_stack(async {
        if !typst_available("a_rerun_with_a_stored_reply_removed_refuses") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        let group_review = call_files_of(&saved.run_id, "group_review");
        assert!(
            !group_review.is_empty(),
            "the fixture run stored a group review reply"
        );
        std::fs::write(&group_review[0], b"{}").unwrap();
        let done = rerun(&saved).await;
        let err = done.result.expect_err("a missing reply refuses the rerun");
        let text = err.to_string();
        assert!(text.contains("This rerun was refused"), "{text}");
        assert!(
            text.contains("group review reply"),
            "names the missing reply: {text}"
        );
        assert!(text.contains("Nothing was sent"), "{text}");
        assert_eq!(done.model_calls, 0, "no model call");
        assert_eq!(done.agent_calls, 0, "no agent call");
        assert!(!done.export.exists(), "no export is written");
    })
}

/// The agent finder's twin: a stored piece removed is a miss too, and the scripted agent is never
/// asked.
#[test]
#[serial_test::serial(process_env)]
fn a_rerun_with_a_finder_piece_removed_refuses_and_never_asks_the_agent() {
    on_a_big_stack(async {
        if !typst_available("a_rerun_with_a_finder_piece_removed_refuses") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        let checkpoints =
            camerata_server::checkpoint::checkpoint_dir_of(&runs_dir().join(&saved.run_id));
        let mut emptied = 0;
        for entry in std::fs::read_dir(&checkpoints).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("finder_")
            {
                std::fs::write(&path, b"").unwrap();
                emptied += 1;
            }
        }
        assert!(
            emptied > 0,
            "the run kept its finder pieces in {checkpoints:?}"
        );
        let done = rerun(&saved).await;
        let err = done.result.expect_err("a missing piece refuses the rerun");
        let text = err.to_string();
        assert!(
            text.contains("agent finder review of"),
            "names the piece: {text}"
        );
        assert_eq!(done.agent_calls, 0, "the scripted agent was never asked");
        assert_eq!(done.model_calls, 0, "no model call");
        assert!(!done.export.exists(), "no export is written");
    })
}

/// Acceptance 3: a normal resume on a different build is still refused.
#[test]
#[serial_test::serial(process_env)]
fn a_normal_resume_on_a_different_build_is_still_refused() {
    on_a_big_stack(async {
        let saved = saved_run_with_a_bookkeeping_defect().await;
        let llm = Scripted {
            calls: AtomicUsize::new(0),
            fixes: false,
        };
        let err = resume_inspect(
            &saved.run_id,
            &[saved.repo.path().to_path_buf()],
            Some(saved.out.path().join("resumed.zip")),
            None,
            true,
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(&llm),
                preflight_sink: &|_| {},
            },
        )
        .await
        .expect_err("a resume on another build is refused");
        let text = err.to_string();
        assert!(text.contains("must finish it"), "{text}");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    })
}

/// Acceptance 4: a rule library change that alters what the finder is asked refuses the rerun and
/// names the cause.
#[test]
#[serial_test::serial(process_env)]
fn a_rule_library_change_refuses_the_rerun_and_names_the_cause() {
    on_a_big_stack(async {
        let saved = saved_run_with_a_bookkeeping_defect().await;
        patch_saved_run(&saved.run_id, |v| {
            v["pin"]["corpus_version"] = json!("a-different-rule-library-digest");
        });
        let done = rerun(&saved).await;
        let err = done
            .result
            .expect_err("a changed rule library refuses the rerun");
        let text = err.to_string();
        assert!(text.contains("rule library changed"), "{text}");
        assert!(text.contains("finder"), "{text}");
        assert_eq!(done.model_calls, 0);
        assert_eq!(done.agent_calls, 0);
        assert!(!done.export.exists());
        assert!(
            read_record(&saved.run_id).unwrap().reruns.is_empty(),
            "a refused rerun records nothing"
        );
    })
}

/// A reply build A accepted that build B's own checks turn down: the stage then asks again, with
/// the reason, a request the run never stored. That is a named miss and a refusal with no call,
/// never a row accepted on the strength of the stored status.
#[test]
#[serial_test::serial(process_env)]
fn a_reply_the_old_build_accepted_and_the_new_build_rejects_is_a_named_miss() {
    on_a_big_stack(async {
        if !typst_available("a_reply_the_old_build_accepted_and_the_new_build_rejects") {
            return;
        }
        let saved = saved_run_with(true).await;
        let stricter = camerata_server::test_defects::RejectFixGuard::set();
        let done = rerun(&saved).await;
        drop(stricter);
        let err = done
            .result
            .expect_err("a re-ask that was never stored refuses the rerun");
        let text = err.to_string();
        assert!(text.contains("This rerun was refused"), "{text}");
        assert!(
            text.contains("fix text reply"),
            "names the missing reply: {text}"
        );
        assert_eq!(done.model_calls, 0, "no model call");
        assert_eq!(done.agent_calls, 0, "no agent call");
        assert!(!done.export.exists(), "no export is written");
    })
}

/// A rerun that was refused is not a rerun the run went through: a resume on the original build
/// afterwards exports a report and a ledger that claim none.
#[test]
#[serial_test::serial(process_env)]
fn a_refused_rerun_then_a_resume_on_the_original_build_claims_no_rerun() {
    on_a_big_stack(async {
        if !typst_available("a_refused_rerun_then_a_resume_on_the_original_build") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        let group_review = call_files_of(&saved.run_id, "group_review");
        std::fs::write(&group_review[0], b"{}").unwrap();
        rerun(&saved)
            .await
            .result
            .expect_err("the rerun is refused");
        let now = camerata_server::run_store::engine_pin::BuildId::current();
        patch_saved_run(&saved.run_id, |v| {
            v["pin"]["build"]["commit"] = json!(now.commit);
            v["pin"]["build"]["dirty"] = json!(now.dirty);
        });
        let llm = Scripted {
            calls: AtomicUsize::new(0),
            fixes: false,
        };
        let outcome = Box::pin(with_agent_override(
            Arc::new(Reviewer::new(vec![real_problem()])),
            resume_inspect(
                &saved.run_id,
                &[saved.repo.path().to_path_buf()],
                Some(saved.out.path().join("resumed.zip")),
                None,
                true,
                &camerata_server::credentials::MemoryCredentialStore::new(),
                Seams {
                    resume: None,
                    llm: Some(&llm),
                    preflight_sink: &|_| {},
                },
            ),
        ))
        .await
        .expect("the resume on the original build goes on");
        assert!(read_record(&saved.run_id).unwrap().reruns.is_empty());
        assert!(
            ledger_of(&outcome).rerun().is_none(),
            "the ledger claims no rerun"
        );
    })
}

/// A rerun makes no call, so a spend limit at or under the run's stored spend stops nothing.
#[test]
#[serial_test::serial(process_env)]
fn a_rerun_of_a_run_with_a_limit_under_its_stored_spend_runs_every_stage() {
    on_a_big_stack(async {
        if !typst_available("a_rerun_of_a_run_with_a_limit_under_its_stored_spend") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        patch_saved_run(&saved.run_id, |v| {
            v["args"]["max_cost_usd"] = json!(0.01);
        });
        let done = rerun(&saved).await;
        let outcome = done.result.expect("the rerun runs every stage");
        assert!(
            outcome.incomplete_reasons.is_empty(),
            "{:?}",
            outcome.incomplete_reasons
        );
        assert_eq!(done.model_calls, 0);
    })
}

/// A refused rerun still records the end of the tree, as any run does, so the next one is not
/// refused on files its own tools left.
#[test]
#[serial_test::serial(process_env)]
fn a_refused_rerun_records_where_the_tree_ended() {
    on_a_big_stack(async {
        if !typst_available("a_refused_rerun_records_where_the_tree_ended") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        patch_saved_run(&saved.run_id, |v| {
            v["repos"][0]["tree_at_end"] = Value::Null;
        });
        let group_review = call_files_of(&saved.run_id, "group_review");
        std::fs::write(&group_review[0], b"{}").unwrap();
        rerun(&saved)
            .await
            .result
            .expect_err("the rerun is refused");
        let record = read_record(&saved.run_id).unwrap();
        assert!(
            record.repos[0].tree_at_end.is_some(),
            "the end of the tree is recorded"
        );
    })
}

// ── `--recheck-saved`: finish the saved run from its sealed report ──────────────────────────────

async fn recheck(
    saved: &Saved,
    export: &Path,
) -> (
    Result<camerata::inspect_cmd::InspectOutcome, InspectError>,
    usize,
) {
    let llm = Scripted {
        calls: AtomicUsize::new(0),
        fixes: false,
    };
    let result = Box::pin(camerata::inspect_cmd::recheck_saved_inspect(
        &saved.run_id,
        &[saved.repo.path().to_path_buf()],
        Some(export.to_path_buf()),
        None,
        Some(5),
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            resume: None,
            llm: Some(&llm),
            preflight_sink: &|_| {},
        },
    ))
    .await;
    (result, llm.calls.load(Ordering::SeqCst))
}

/// A saved run with a lost-row defect, rechecked on the fixed build: passes the gate with zero
/// model calls even with the re-ask flag (nothing needs asking), writes the export, and the
/// workbook and the ledger name both builds.
#[test]
#[serial_test::serial(process_env)]
fn a_recheck_of_a_saved_run_passes_the_gate_with_zero_calls_and_names_both_builds() {
    on_a_big_stack(async {
        if !typst_available("a_recheck_of_a_saved_run_passes_the_gate") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        let export = saved.out.path().join("recheck.zip");
        let (result, calls) = recheck(&saved, &export).await;
        let outcome = result.expect("the recheck passes the gate");
        assert_eq!(calls, 0, "no model call");
        assert!(outcome.release_gate.starts_with("release gate: passed"));
        assert!(
            outcome.incomplete_reasons.is_empty(),
            "{:?}",
            outcome.incomplete_reasons
        );
        assert!(export.exists());
        let ledger = ledger_of(&outcome);
        let rerun = ledger.rerun().expect("the ledger names both builds");
        assert_eq!(rerun.model_build.commit.as_deref(), Some(BUILD_A));
        let zip = std::fs::read(&export).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut found = false;
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            if f.name().unwrap_or_default().ends_with(".xlsx") {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut f, &mut bytes).unwrap();
                let grid =
                    camerata_server::release_gate::xlsx_sheet_grid(&bytes, "Start here").unwrap();
                found = grid.iter().any(|r| {
                    r.first().is_some_and(|c| c == "How this was done")
                        && r.get(1).is_some_and(|c| c.contains(&BUILD_A[..12]))
                });
            }
        }
        assert!(found, "the workbook states both builds");
    })
}

/// If the release gate refuses, the command stops with its refusal and writes no export.
#[test]
#[serial_test::serial(process_env)]
fn a_recheck_the_release_gate_refuses_writes_no_export() {
    on_a_big_stack(async {
        if !typst_available("a_recheck_the_release_gate_refuses") {
            return;
        }
        let saved = saved_run_with_a_bookkeeping_defect().await;
        // A path that carries a key reaches the package: the gate must stop it.
        let key =
            camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey)
                .value;
        let path = runs_dir()
            .join(&saved.run_id)
            .join(camerata_server::run_store::REPORT_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["findings"][0]["path"] = json!(format!("src/{key}.rs"));
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let export = saved.out.path().join("blocked.zip");
        let before = saved_files(&saved);
        let (result, calls) = recheck(&saved, &export).await;
        assert_eq!(
            saved_files(&saved),
            before,
            "a refusal leaves the saved files byte-identical"
        );
        let err = result.expect_err("the gate refuses");
        assert!(matches!(err, InspectError::ReleaseBlocked(_)), "{err}");
        assert_eq!(calls, 0);
        assert!(!export.exists(), "a blocked export leaves nothing on disk");
    })
}

/// The bytes of the run's record, its saved report and its ledger.
fn saved_files(saved: &Saved) -> Vec<Vec<u8>> {
    let dir = runs_dir().join(&saved.run_id);
    [
        dir.join("run.json"),
        dir.join(camerata_server::run_store::REPORT_FILE),
        saved.ledger_path.clone(),
    ]
    .iter()
    .map(|p| std::fs::read(p).unwrap())
    .collect()
}

/// An export in a folder that does not exist is refused up front: nothing the run saved is changed.
#[test]
#[serial_test::serial(process_env)]
fn a_recheck_with_an_export_in_a_missing_folder_refuses_before_changing_anything() {
    on_a_big_stack(async {
        let saved = saved_run_with_a_bookkeeping_defect().await;
        let before = saved_files(&saved);
        let export = saved.out.path().join("no-such-folder").join("recheck.zip");
        let (result, calls) = recheck(&saved, &export).await;
        let err = result.expect_err("refused");
        assert!(err.to_string().contains("folder"), "{err}");
        assert_eq!(calls, 0);
        assert_eq!(saved_files(&saved), before);
    })
}

/// A saved run whose clone check failed on an unreadable reply and whose integrity check failed on
/// the first build's defect: the recheck confirms the clone rows from the stored reply, makes no
/// call even with the flag, and the package passes the release gate (the merged-rule fix check and
/// the fix-parity checks included).
#[test]
#[serial_test::serial(process_env)]
fn a_recheck_that_confirms_clone_rows_passes_the_release_gate_with_zero_calls() {
    on_a_big_stack(async {
        if !typst_available("a_recheck_that_confirms_clone_rows") {
            return;
        }
        let saved = saved_run_full(false, true).await;
        let record = read_record(&saved.run_id).unwrap();
        // The first build could not read the judge's replies: it stored them and failed the part.
        let report_path = runs_dir()
            .join(&saved.run_id)
            .join(camerata_server::run_store::REPORT_FILE);
        let saved_report: Value =
            serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
        assert!(
            saved_report["failed_passes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["pass"] == "clone-candidate judgment"),
            "the first run's clone check failed: {:?}",
            saved_report["failed_passes"]
        );
        // The stored replies are ones the current reader merges.
        let repo_name = record.repos[0].name.clone();
        let files = camerata_server::onboard::read_local_repo_files(
            saved.repo.path(),
            &camerata_server::onboard::scope::ScanScope::default().for_repo(&repo_name),
        )
        .unwrap()
        .files;
        let armed: std::collections::HashSet<&str> = camerata_checks::clone_rules::CloneRule::ALL
            .iter()
            .map(|r| r.id())
            .collect();
        let set = camerata_checks::clone_rules::candidates(&files, &armed);
        let answers: Vec<(String, &str)> = set
            .by_rule
            .iter()
            .flat_map(|(rule, list)| {
                list.iter()
                    .map(move |c| (c.id.clone(), rule.affirmative_word()))
            })
            .collect();
        assert!(!answers.is_empty(), "the fixture holds a clone candidate");
        let text = answers
            .iter()
            .map(|(id, word)| {
                json!({"verdicts": [{"id": id, "verdict": word, "concept": "the discount"}]})
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for entry in std::fs::read_dir(runs_dir().join(&saved.run_id).join("calls")).unwrap() {
            let path = entry.unwrap().path();
            let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if v["owner"] == "clone_judge" {
                for e in v["entries"].as_object_mut().unwrap().values_mut() {
                    if e["response"].is_object() {
                        e["response"]["text"] = json!(text);
                    }
                }
                std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
            }
        }
        let export = saved.out.path().join("clones.zip");
        let (result, calls) = recheck(&saved, &export).await;
        let outcome = result.expect("the recheck passes the gate");
        assert_eq!(
            calls, 0,
            "the stored reply settles it: no call even with the flag"
        );
        assert!(
            outcome.release_gate.starts_with("release gate: passed"),
            "{}",
            outcome.release_gate
        );
        assert!(
            outcome.incomplete_reasons.is_empty(),
            "{:?}",
            outcome.incomplete_reasons
        );
        assert!(export.exists());
        let after: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
        let clone_rows = after["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["rule_id"] == "ARCH-SINGLE-SOURCE-LOGIC-1")
            .count();
        assert!(
            clone_rows >= 2,
            "the confirmed candidate's rows are in the report: {clone_rows}"
        );
    })
}
