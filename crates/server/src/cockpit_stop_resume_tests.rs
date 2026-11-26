//! The app's path after a stop (#567): a spend-limit stop (the cost checkpoint's pause) and a
//! credit stop of an inspection started from the app, then "Run what remains" through the app's
//! own routes. The agent finder runs on a scripted stand-in agent that charges a fixed amount for
//! each piece and records every piece it is asked to review; the fake `claude` of
//! [`crate::cockpit_resume_tests`] plays the stages after the finder. No live or paid model call.
//!
//! The synthetic repository has three folders of about 216 KB each, so the finder cuts it into
//! three pieces (`alpha`, `beta`, `gamma`) and reviews them one at a time (the command line
//! backend has one piece in flight), then the joints between them.

use std::sync::{Arc, Mutex};

use super::*;
use crate::ai_audit::agent_finder::{
    AgentRun, FollowUp, PieceAgent, PieceLimits, Room, ScriptStep, ScriptedAgent, TurnUsage,
};
use crate::cockpit_resume_tests::{body_json, finished, FAKE_CLAUDE};
use axum::extract::Query;

/// The provider's own words when the account is out of credit.
const CREDIT: &str =
    "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing.";

/// What one piece costs, and the joints call.
const PIECE_USD: f64 = 2.0;
const JOINTS_USD: f64 = 0.5;

/// A stand-in piece agent that does a complete review of whatever room it is given: it reads
/// every piece file in full, finds nothing, answers every rule of the checklist clean and writes
/// its card. It records the first folder of each piece it is asked to run (`joints` for the joints
/// call). While `credit_out_for` names a folder, a run of that folder's piece fails the way the
/// provider refuses an account with no credit, before anything is paid.
#[derive(Clone, Default)]
struct StandIn {
    seen: Arc<Mutex<Vec<String>>>,
    credit_out_for: Arc<Mutex<Option<String>>>,
}

impl StandIn {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn key(room: &Room) -> String {
        if room.extra_files().is_empty() {
            room.piece_files()
                .first()
                .and_then(|p| p.split('/').next())
                .unwrap_or_default()
                .to_string()
        } else {
            "joints".to_string()
        }
    }

    /// The rule ids the room's checklist asks about (`## RULE-ID: title`).
    fn checklist_rules(room: &Room) -> Vec<String> {
        let text = room
            .read("_camerata/checklist.md", 1, 2_000)
            .map(|r| r.text)
            .unwrap_or_default();
        text.lines()
            .filter_map(|l| l.split_once('\t').map(|(_, rest)| rest))
            .filter_map(|l| l.strip_prefix("## "))
            .filter_map(|l| l.split_once(": ").map(|(id, _)| id.trim().to_string()))
            .collect()
    }

    fn script(room: &Room) -> (Vec<ScriptStep>, f64) {
        let out = |file: &str, text: String| ScriptStep::Write {
            path: format!("_camerata/out/{file}"),
            text,
        };
        if !room.extra_files().is_empty() {
            return (
                vec![
                    out("problems-1.json", "[]".to_string()),
                    out("leads-answers.json", "[]".to_string()),
                ],
                JOINTS_USD,
            );
        }
        let mut steps = Vec::new();
        for (path, lines) in room.piece_line_counts() {
            let mut first = 1;
            while first <= lines {
                steps.push(ScriptStep::Read {
                    path: path.clone(),
                    first_line: first,
                    count: 1_000,
                });
                first += 1_000;
            }
        }
        let place = room.piece_files().first().cloned().unwrap_or_default();
        let answers: Vec<serde_json::Value> = Self::checklist_rules(room)
            .iter()
            .map(|id| {
                serde_json::json!({
                    "rule": id, "answer": "clean", "places": [{"path": place, "line": 1}]
                })
            })
            .collect();
        steps.push(out("problems-1.json", "[]".to_string()));
        steps.push(out(
            "checklist.json",
            serde_json::json!({"citations": [], "answers": answers}).to_string(),
        ));
        steps.push(out(
            "card.md",
            "Entry points: none. It trusts its callers.".to_string(),
        ));
        steps.push(out("unread.json", "[]".to_string()));
        (steps, PIECE_USD)
    }
}

#[async_trait::async_trait]
impl PieceAgent for StandIn {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        let key = Self::key(room);
        self.seen.lock().unwrap().push(key.clone());
        if self.credit_out_for.lock().unwrap().as_deref() == Some(key.as_str()) {
            anyhow::bail!("{CREDIT}");
        }
        let (steps, cost) = Self::script(room);
        ScriptedAgent::new(steps)
            .with_usage(TurnUsage {
                input_tokens: 1_000,
                output_tokens: 100,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: Some(cost),
            })
            .run(room, brief, limits)
            .await
    }

    async fn follow_up(
        &self,
        room: &Room,
        _prior: &AgentRun,
        turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        anyhow::bail!(
            "the stand-in did a complete review of {}, so no follow-up was expected: {}",
            Self::key(room),
            turn.message
        )
    }
}

/// An app inspection of the synthetic repository on the agent finder, ready to be started.
struct Rig {
    _root: tempfile::TempDir,
    _guards: Vec<camerata_test_env::EnvGuard>,
    state: AppState,
    project_id: String,
    agent: StandIn,
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// Three folders of about 216 KB each: more than one piece's target together, so three pieces.
fn stage_repo(dest: &std::path::Path) {
    for folder in ["alpha", "beta", "gamma"] {
        let dir = dest.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let text: String = (1..=2_700)
            .map(|i| format!("pub fn {folder}_item_{i:05}() -> u64 {{ {i} * 7 + 11 }} // acme-fixture line\n"))
            .collect();
        std::fs::write(dir.join("lib.rs"), text).unwrap();
    }
    git(dest, &["init", "-q", "-b", "main"]);
    git(
        dest,
        &["config", "user.email", "stop-resume-test@camerata.local"],
    );
    git(dest, &["config", "user.name", "Stop Resume Test"]);
    git(dest, &["add", "."]);
    git(dest, &["commit", "-q", "-m", "fixture"]);
}

fn rig() -> Rig {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state_dir = root.path().join("state");
    let repo = root.path().join("repo");
    for d in [&bin, &state_dir, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let claude = bin.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    stage_repo(&repo);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let guards = vec![
        camerata_test_env::EnvGuard::set("PATH", &path),
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &state_dir),
        camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
        camerata_test_env::EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1"),
        camerata_test_env::EnvGuard::set("FAKE_LOG", root.path().join("calls.log")),
        camerata_test_env::EnvGuard::set(
            crate::efficiency::EFFICIENCY_ENV,
            "terse_output=off,warm_schedule=off",
        ),
        // Anything that would leave the machine goes to a closed local port instead.
        camerata_test_env::EnvGuard::set("HTTPS_PROXY", "http://127.0.0.1:9"),
        camerata_test_env::EnvGuard::set("ALL_PROXY", "http://127.0.0.1:9"),
    ];
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Stop and resume", vec!["acme/three".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&project.id, crate::project::ProjectBackend::Cli)
        .unwrap();
    state
        .projects
        .set_client_engagement(&project.id, false, false)
        .unwrap();
    state
        .settings
        .set_repo_path("acme/three", Some(repo.to_string_lossy().into_owned()));
    let project = state.projects.get(&project.id).unwrap();
    state
        .projects
        .set_review_settings(
            &project.id,
            camerata_api_types::project::ReviewSettings {
                finder: camerata_api_types::project::ProjectFinder::Agent,
                ..project.review
            },
        )
        .unwrap()
        .unwrap();
    Rig {
        _root: root,
        _guards: guards,
        state,
        project_id: project.id,
        agent: StandIn::default(),
    }
}

fn request(max_cost_usd: Option<f64>) -> AuditReq {
    serde_json::from_value(serde_json::json!({
        "repos": ["acme/three"],
        "rules": [{
            "id": "unchecked-input",
            "directive": "Every input must be checked before it is used.",
        }],
        "mode": "parallel",
        "incremental": false,
        "run_ai_review": true,
        "run_deterministic": false,
        "max_cost_usd": max_cost_usd,
    }))
    .unwrap()
}

impl Rig {
    fn agent(&self) -> Arc<dyn PieceAgent> {
        Arc::new(self.agent.clone())
    }

    /// Start an inspection from the app, as the scan button does; the job's final state.
    async fn start(&self, max_cost_usd: Option<f64>) -> crate::jobs::JobState {
        let job_id = crate::ai_audit::agent_finder::run::with_agent_override(
            self.agent(),
            start_audit_job(
                &self.state,
                request(max_cost_usd),
                AuditJobOptions::default(),
            ),
        )
        .await;
        finished(&self.state, &job_id).await
    }

    /// `POST /api/inspections/:id/resume`, as the app's Start button sends it. `Ok` is the
    /// resume job's final state; `Err` is the refusal's status and words.
    async fn resume(
        &self,
        run_id: &str,
        max_cost_usd: Option<f64>,
    ) -> Result<crate::jobs::JobState, (StatusCode, String)> {
        let resp = crate::ai_audit::agent_finder::run::with_agent_override(
            self.agent(),
            run_resume_start(
                State(self.state.clone()),
                Path(run_id.to_string()),
                Query(ResumeQuery { max_cost_usd }),
            ),
        )
        .await
        .into_response();
        let status = resp.status();
        let body = body_json(resp).await;
        if status != StatusCode::OK {
            return Err((
                status,
                body["error"].as_str().unwrap_or_default().to_string(),
            ));
        }
        let job_id = body["job_id"].as_str().unwrap().to_string();
        Ok(finished(&self.state, &job_id).await)
    }

    async fn plan(&self, run_id: &str) -> serde_json::Value {
        let resp = run_resume_plan(State(self.state.clone()), Path(run_id.to_string()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    }

    fn last_scan(&self) -> crate::onboard::ScanReport {
        self.state
            .get_last_scan(&self.project_id)
            .expect("the inspection is the project's last scan")
    }
}

/// Run `fut` on a runtime whose worker threads have a large stack: the job runs the whole
/// pipeline on a worker as one deep chain of futures, and a 2 MiB worker stack overflows in a
/// debug build (the command line's agent finder tests run on a big stack for the same reason).
fn on_big_stacks<F: std::future::Future<Output = ()> + Send + 'static>(fut: F) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(64 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(fut);
        })
        .unwrap()
        .join()
        .unwrap();
}

fn client_export_locked(report: &crate::onboard::ScanReport) -> bool {
    let client = crate::report_export::ReportOptions {
        internal: false,
        ..Default::default()
    };
    crate::report_export::require_complete_run(report, &client).is_err()
}

fn pieces_of(seen: &[String], key: &str) -> usize {
    seen.iter().filter(|s| s.as_str() == key).count()
}

/// A spend-limit stop of an app inspection: the cost checkpoint pauses it before the second
/// piece (spent $2, two pieces left at $2 each, over the $5 approved). The pause keeps the export
/// locked and the plan says what remains. A resume at a raised limit that is still under the
/// projection pauses again before any new piece, as the command line does. A resume at a limit
/// that covers it runs only the two unfinished pieces and the joints, and the lock lifts.
#[test]
#[serial_test::serial(process_env)]
fn an_app_inspection_paused_by_its_spend_limit_resumes_at_a_raised_limit_and_runs_only_what_remains(
) {
    on_big_stacks(async {
        if !camerata_test_env::python3_on_path_for("an_app_inspection_paused_by_its_spend_limit") {
            return;
        }
        let rig = rig();

        // ── The inspection, at a $5 limit: paused before the second piece ───────────────────
        let job = rig.start(Some(5.0)).await;
        assert_eq!(job.status, "done", "{:?}", job.message);
        assert_eq!(
            rig.agent.seen(),
            vec!["alpha".to_string()],
            "paused before beta"
        );
        let first = rig.last_scan();
        let run_id = first
            .run_id
            .clone()
            .expect("the inspection was saved as a run");
        assert!(client_export_locked(&first), "the export stays locked");
        let reasons = first.incomplete_reasons();
        assert!(
            reasons
                .iter()
                .any(|r| r.contains(crate::cost_checkpoint::CAUSE_PROJECTED)),
            "{reasons:?}"
        );
        let pause = job
            .cost_checkpoint
            .clone()
            .expect("the job shows the checkpoint");
        assert!(pause.stopped, "{pause:?}");
        assert_eq!(pause.pieces_left, 2, "{pause:?}");

        // ── The plan: what remains, its projected cost, and no refusal ──────────────────────
        let plan = rig.plan(&run_id).await;
        assert!(plan["refusal"].is_null(), "{plan}");
        assert_eq!(plan["saved_limit_usd"], 5.0, "{plan}");
        let remaining = plan["estimate"]["remaining"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(!remaining.is_empty(), "{plan}");
        assert!(
            plan["text"]
                .as_str()
                .unwrap_or_default()
                .contains("estimated cost of the remaining work"),
            "{plan}"
        );

        // ── A raised limit under the projection ($6): paused again, no new piece ─────────────
        let job = rig
            .resume(&run_id, Some(5.5))
            .await
            .unwrap_or_else(|(s, why)| {
                panic!("a raised limit on a paused run is accepted: {s} {why}")
            });
        assert_eq!(job.status, "done", "{:?}", job.message);
        assert_eq!(
            rig.agent.seen(),
            vec!["alpha".to_string()],
            "a limit under the projection sends no new piece"
        );
        let second = rig.last_scan();
        assert_eq!(second.run_id.as_deref(), Some(run_id.as_str()));
        assert!(client_export_locked(&second), "the export stays locked");
        let pause = job
            .cost_checkpoint
            .clone()
            .expect("the job shows the checkpoint");
        assert!(pause.stopped, "{pause:?}");
        // The stored $2 counts: $2 spent and two pieces at $2 make $6, and the advised limit adds
        // the hard limit's reserve for the piece in flight.
        let message = pause.pause_message();
        assert!(message.contains("spent $2.00, 2 pieces left"), "{message}");
        assert!(message.contains("New estimate $6.00"), "{message}");
        assert!(
            message.contains("resume with a spend limit of at least $8.00"),
            "{message}"
        );

        // ── A limit that covers it: only beta, gamma and the joints run; the lock lifts ─────
        let job = rig
            .resume(&run_id, Some(10.0))
            .await
            .unwrap_or_else(|(s, why)| panic!("{s} {why}"));
        assert_eq!(job.status, "done", "{:?}", job.message);
        let seen = rig.agent.seen();
        assert_eq!(
            pieces_of(&seen, "alpha"),
            1,
            "alpha was not sent again: {seen:?}"
        );
        assert_eq!(pieces_of(&seen, "beta"), 1, "{seen:?}");
        assert_eq!(pieces_of(&seen, "gamma"), 1, "{seen:?}");
        let last = rig.last_scan();
        assert!(
            last.incomplete_reasons().is_empty(),
            "{:?}",
            last.incomplete_reasons()
        );
        assert!(!client_export_locked(&last), "the export lock lifts");
        assert_eq!(
            crate::run_store::read_record(&run_id).unwrap().status,
            crate::run_store::RunStatus::Complete
        );
    });
}

/// A credit stop of an app inspection: the provider refuses the second piece for billing, the
/// run stops (the third piece and the joints are not sent), the export stays locked and the plan
/// names what remains. A raised limit is refused (the limit did not stop it). Once the credit is
/// back, the resume runs only the two unfinished pieces and the joints, and the lock lifts.
#[test]
#[serial_test::serial(process_env)]
fn an_app_inspection_stopped_for_credit_resumes_only_the_unfinished_parts() {
    on_big_stacks(async {
        if !camerata_test_env::python3_on_path_for("an_app_inspection_stopped_for_credit") {
            return;
        }
        let rig = rig();
        *rig.agent.credit_out_for.lock().unwrap() = Some("beta".to_string());

        let job = rig.start(None).await;
        assert_eq!(job.status, "done", "{:?}", job.message);
        assert_eq!(
            rig.agent.seen(),
            vec!["alpha".to_string(), "beta".to_string()],
            "the run stopped at the credit error: gamma and the joints were not sent"
        );
        let first = rig.last_scan();
        let run_id = first
            .run_id
            .clone()
            .expect("the inspection was saved as a run");
        assert!(
            first.ledger.is_outage_stopped(),
            "a credit error stops the run"
        );
        assert!(client_export_locked(&first), "the export stays locked");

        let plan = rig.plan(&run_id).await;
        assert!(plan["refusal"].is_null(), "{plan}");
        let remaining = plan["estimate"]["remaining"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(!remaining.is_empty(), "{plan}");

        let (status, why) = rig
            .resume(&run_id, Some(50.0))
            .await
            .expect_err("a raised limit is only for a run the limit stopped");
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(why.contains("not stopped by its spend limit"), "{why}");

        // The credit is back.
        *rig.agent.credit_out_for.lock().unwrap() = None;
        let job = rig
            .resume(&run_id, None)
            .await
            .unwrap_or_else(|(s, why)| panic!("{s} {why}"));
        assert_eq!(job.status, "done", "{:?}", job.message);
        let seen = rig.agent.seen();
        assert_eq!(
            pieces_of(&seen, "alpha"),
            1,
            "alpha was not sent again: {seen:?}"
        );
        assert_eq!(pieces_of(&seen, "beta"), 2, "beta is sent again: {seen:?}");
        assert_eq!(pieces_of(&seen, "gamma"), 1, "{seen:?}");
        let last = rig.last_scan();
        assert!(
            last.incomplete_reasons().is_empty(),
            "{:?}",
            last.incomplete_reasons()
        );
        assert!(!client_export_locked(&last), "the export lock lifts");
    });
}
