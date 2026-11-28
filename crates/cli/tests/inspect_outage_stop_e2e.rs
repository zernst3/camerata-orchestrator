#![allow(clippy::unwrap_used)]
//! #582, end to end over the fixture repos with a SCRIPTED model (no network, no spend): an outage
//! mid-run either costs minutes or stops the run, and a stopped run starts no later stage.
//!
//! The fake provider drops every connection for N VIRTUAL seconds, from the first request of a
//! chosen stage. Every wait of the run's network wait sleeps through the process sleeper, which
//! here moves a virtual clock and returns at once, so a fifteen-minute wait takes no time.
//!
//! * A short drop: the run waits, sends the same requests again and finishes with the same output
//!   as an uninterrupted run.
//! * A long drop: the run exits with the outage code and has not called the group reviewer or the
//!   possible-duplicate check. A resume finishes every repo and runs the group review once, over
//!   all of them.
//! * An auth failure never waits.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use camerata::inspect_cmd::{
    resume_inspect, run_inspect_pipeline_controlled, InspectArgs, InspectError, InspectOutcome,
    Seams, EXIT_OUTAGE_STOPPED,
};
use camerata_server::cost_cap::FailureClass;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

// ── The virtual clock ────────────────────────────────────────────────────────────────────────

/// A sleeper that moves the virtual clock by each wait and returns at once.
struct VirtualClock {
    secs: AtomicU64,
}

impl camerata_server::network_wait::Sleeper for VirtualClock {
    fn sleep<'a>(
        &'a self,
        d: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.secs.fetch_add(d.as_secs(), Ordering::SeqCst);
        Box::pin(tokio::task::yield_now())
    }
}

/// Installs the virtual clock as the process sleeper while alive.
struct ClockGuard(Arc<VirtualClock>);

impl ClockGuard {
    fn install() -> Self {
        let clock = Arc::new(VirtualClock {
            secs: AtomicU64::new(0),
        });
        camerata_server::network_wait::set_process_sleeper(Some(clock.clone()));
        Self(clock)
    }
    fn now(&self) -> u64 {
        self.0.secs.load(Ordering::SeqCst)
    }
}

impl Drop for ClockGuard {
    fn drop(&mut self) {
        camerata_server::network_wait::set_process_sleeper(None);
    }
}

// ── The fake provider ────────────────────────────────────────────────────────────────────────

/// The stage a request belongs to, read from its system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Finder,
    Fix,
    CloneJudge,
    GroupReview,
    DuplicateCheck,
    Other,
}

fn stage_of(req: &LlmRequest) -> Stage {
    let system = req.system.as_deref().unwrap_or_default();
    if camerata_server::ai_audit::group_review::stand_in_reply(Some(system), "").is_some() {
        Stage::GroupReview
    } else if system.starts_with(&camerata_server::possible_duplicate::check::system_prompt()) {
        Stage::DuplicateCheck
    } else if system.starts_with("You write ONE concrete, codebase-specific fix") {
        Stage::Fix
    } else if system.starts_with("You are a senior software architect performing a CONFORMANCE") {
        Stage::Finder
    } else if system
        .starts_with(camerata_server::onboard::clone_advisory::prompt::JUDGE_SYSTEM_PREFIX)
    {
        Stage::CloneJudge
    } else {
        Stage::Other
    }
}

/// What goes wrong, from the first request of `from` on.
#[derive(Clone, Copy)]
enum Fault {
    /// Every connection drops for `secs` virtual seconds.
    Drop { from: Stage, secs: u64 },
    /// The provider refuses the key.
    Auth { from: Stage },
}

struct Provider {
    clock: Arc<VirtualClock>,
    fault: Option<Fault>,
    started_at: Mutex<Option<u64>>,
    /// Every request: its stage and whether it was answered.
    calls: Mutex<Vec<(Stage, bool)>>,
    /// The run's control, when the test hands it one: a request that arrives once it refuses
    /// sends (an operator or outage stop) is counted in `after_stop`.
    control: Mutex<Option<Arc<camerata_server::operator_stop::RunControl>>>,
    after_stop: std::sync::atomic::AtomicUsize,
    /// The batches the provider accepted, by id: their items.
    batches: Mutex<Vec<(String, Vec<camerata_server::llm::BatchItem>)>>,
}

impl Provider {
    fn new(clock: &ClockGuard, fault: Option<Fault>) -> Self {
        Self {
            clock: clock.0.clone(),
            fault,
            started_at: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            control: Mutex::new(None),
            after_stop: std::sync::atomic::AtomicUsize::new(0),
            batches: Mutex::new(Vec::new()),
        }
    }

    fn watch(&self, control: &Arc<camerata_server::operator_stop::RunControl>) {
        *self.control.lock().unwrap() = Some(control.clone());
    }

    /// Requests of any kind that reached the provider after the run's stop.
    fn after_stop(&self) -> usize {
        self.after_stop.load(Ordering::SeqCst)
    }

    fn calls_of(&self, stage: Stage) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| *s == stage)
            .count()
    }

    fn refused(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, ok)| !ok)
            .count()
    }

    fn respond(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        if self
            .control
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.refuses_sends())
        {
            self.after_stop.fetch_add(1, Ordering::SeqCst);
        }
        let stage = stage_of(req);
        let now = self.clock.secs.load(Ordering::SeqCst);
        let failure = match self.fault {
            None => None,
            Some(Fault::Drop { from, secs }) => {
                let mut started = self.started_at.lock().unwrap();
                if started.is_none() && stage == from {
                    *started = Some(now);
                }
                started.filter(|at| now < at + secs).map(|_| {
                    (
                        FailureClass::Transport,
                        "Anthropic API request failed: connection reset by peer",
                    )
                })
            }
            Some(Fault::Auth { from }) => {
                let mut started = self.started_at.lock().unwrap();
                if started.is_none() && stage == from {
                    *started = Some(now);
                }
                started.map(|_| {
                    (
                        FailureClass::AuthOrBilling,
                        "Anthropic API HTTP 401: invalid x-api-key",
                    )
                })
            }
        };
        self.calls.lock().unwrap().push((stage, failure.is_none()));
        if let Some((class, message)) = failure {
            let mut f = camerata_server::failure::LlmCallFailure::new(class, message);
            f.retryable = class == FailureClass::Transport;
            return Err(anyhow::Error::new(f));
        }
        Ok(self.answer(req))
    }

    /// The reply the provider gives `req` when it answers.
    fn answer(&self, req: &LlmRequest) -> LlmResponse {
        let stage = stage_of(req);
        let text = if stage == Stage::CloneJudge {
            clone_verdicts(&req.prompt)
        } else {
            camerata_server::ai_audit::group_review::stand_in_reply(
                req.system.as_deref(),
                &req.prompt,
            )
            .unwrap_or_else(|| {
            r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string()
            })
        };
        LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(10),
            output_tokens: Some(10),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        }
    }
}

#[async_trait]
impl LlmPort for Provider {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.respond(&req)
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.respond(&req)?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The clone judge's answer: "unsure" for every candidate the request frames (an undecided
/// candidate is not reported, so the check completes and adds no row).
fn clone_verdicts(prompt: &str) -> String {
    let verdicts: Vec<serde_json::Value> = prompt
        .lines()
        .filter(|l| l.starts_with("<<<UNTRUSTED "))
        .filter_map(|l| l.split("\"candidate ").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(|id| serde_json::json!({"id": id, "verdict": "unsure", "concept": ""}))
        .collect();
    serde_json::json!({ "verdicts": verdicts }).to_string()
}

/// Two copies of one pricing decision: the clone check's candidates.
const DECISION: &str = "pub fn FUNC(order: &Order, user: &User) -> f64 {\n    let base = order.subtotal * order.quantity as f64;\n    if user.is_premium {\n        let discount = base * RATE;\n        return base - discount;\n    }\n    if base > 100.0 && order.coupon.is_some() {\n        return base - 10.0;\n    }\n    base + order.shipping\n}\n";

/// A batch item as the request it carries (the system prompt, the user text, the model).
fn request_of(item: &camerata_server::llm::BatchItem) -> LlmRequest {
    let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
    let prompt = match content.as_str() {
        Some(t) => t.to_string(),
        None => content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
    };
    let mut req = LlmRequest::new(prompt);
    req.system = item.params.system.clone();
    req.model = item.params.model.clone();
    req
}

/// The same provider through the Message Batches API. A submit while the network is down is
/// refused at connect (it never reached the provider, so the run may wait and send it again);
/// a submit once the run stopped is counted in `after_stop`. Polls and fetches are free and
/// always answer.
#[async_trait]
impl camerata_server::ai_audit::BatchApi for Provider {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        self
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, items: Vec<camerata_server::llm::BatchItem>) -> anyhow::Result<String> {
        let first = items.first().map(request_of);
        if let Some(Err(e)) = first.as_ref().map(|r| self.respond(r)) {
            let mut f = camerata_server::failure::LlmCallFailure::new(
                camerata_server::failure::classify_error(&e),
                format!("{e:#}"),
            );
            f.attempts = vec![camerata_server::failure::AttemptRecord {
                message: "error sending request: connection refused".to_string(),
                not_sent: true,
                ..Default::default()
            }];
            return Err(anyhow::Error::new(f));
        }
        let mut batches = self.batches.lock().unwrap();
        let id = format!("msgbatch_acme_{}", batches.len());
        batches.push((id.clone(), items));
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<camerata_server::llm::BatchStatus> {
        Ok(camerata_server::llm::BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: Default::default(),
        })
    }
    async fn fetch(
        &self,
        batch_id: &str,
        _ttl: camerata_server::llm::CacheTtl,
    ) -> anyhow::Result<Vec<camerata_server::llm::BatchResultRow>> {
        let batches = self.batches.lock().unwrap();
        let items = batches
            .iter()
            .find(|(id, _)| id == batch_id)
            .map(|(_, items)| items.clone())
            .unwrap_or_default();
        Ok(items
            .iter()
            .map(|item| camerata_server::llm::BatchResultRow {
                custom_id: item.custom_id.clone(),
                response: Some(self.answer(&request_of(item))),
                error: None,
                billed: None,
            })
            .collect())
    }
}

// ── The runs ─────────────────────────────────────────────────────────────────────────────────

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

fn stage_fixture(dest: &Path, clones: bool) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    copy(&root, dest);
    if clones {
        for (file, func, rate) in [
            ("src/pricing.rs", "apply_discount", "0.2"),
            ("src/quote.rs", "quote_total", "0.15"),
        ] {
            std::fs::write(
                dest.join(file),
                DECISION.replace("FUNC", func).replace("RATE", rate),
            )
            .unwrap();
        }
    }
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "outage-e2e@camerata.local"],
        &["config", "user.name", "Outage E2E"],
        &["add", "."],
        &["commit", "-q", "-m", "fixture"],
    ] {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
}

/// Two fixture repos, the state folder and the export path, kept alive for the test.
struct Workspace {
    _state: tempfile::TempDir,
    _state_env: camerata_test_env::EnvGuard,
    _dep_env: camerata_test_env::EnvGuard,
    repos: Vec<tempfile::TempDir>,
    out: tempfile::TempDir,
}

impl Workspace {
    fn new(repos: usize) -> Self {
        Self::with(repos, false)
    }

    /// `repos` fixture repos, each holding two copies of one decision when `clones` is set.
    fn with(repos: usize, clones: bool) -> Self {
        let state = tempfile::tempdir().unwrap();
        let state_env = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let dep_env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let repos = (0..repos)
            .map(|_| {
                let r = tempfile::tempdir().unwrap();
                stage_fixture(r.path(), clones);
                r
            })
            .collect();
        Self {
            _state: state,
            _state_env: state_env,
            _dep_env: dep_env,
            repos,
            out: tempfile::tempdir().unwrap(),
        }
    }

    fn export(&self, name: &str) -> PathBuf {
        self.out.path().join(name)
    }

    fn args(&self, export: &Path) -> InspectArgs {
        let names = ["acme/acme-api", "acme/acme-web"];
        InspectArgs {
            repo: self.repos[0].path().to_path_buf(),
            also_repos: self.repos[1..]
                .iter()
                .map(|r| r.path().to_path_buf())
                .collect(),
            repo_names: names[..self.repos.len()]
                .iter()
                .map(|n| (*n).to_string())
                .collect(),
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
            client: "Acme Corp".to_string(),
            client_engagement: false,
            run_client_tooling: false,
            consensus: false,
            finder: Default::default(),
            exclude: Vec::new(),
        }
    }

    async fn run(&self, llm: &Provider, export: &Path) -> Result<InspectOutcome, InspectError> {
        self.run_args(llm, self.args(export)).await
    }

    /// A run with `args`, on a run control the provider watches: a request that reaches the
    /// provider once the run stopped is counted ([`Provider::after_stop`]). An outage stop must
    /// have reached the control by the end: the provider's count is only as good as that.
    async fn run_args(
        &self,
        llm: &Provider,
        args: InspectArgs,
    ) -> Result<InspectOutcome, InspectError> {
        let control = Arc::new(camerata_server::operator_stop::RunControl::new());
        llm.watch(&control);
        let out = self.run_on(llm, args, control.clone()).await;
        let stopped = match &out {
            Ok(o) => o.outage_stopped(),
            Err(e) => e.exit_code() == EXIT_OUTAGE_STOPPED,
        };
        if stopped {
            assert!(
                control.refuses_sends(),
                "the outage stop never reached the run's control: waiting calls could still send"
            );
        }
        out
    }

    async fn run_on(
        &self,
        llm: &dyn LlmPort,
        args: InspectArgs,
        control: Arc<camerata_server::operator_stop::RunControl>,
    ) -> Result<InspectOutcome, InspectError> {
        run_inspect_pipeline_controlled(
            args,
            true,
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(llm),
                preflight_sink: &|_| {},
            },
            control,
        )
        .await
    }

    async fn resume(
        &self,
        llm: &Provider,
        run_id: &str,
        export: &Path,
    ) -> Result<InspectOutcome, InspectError> {
        resume_inspect(
            run_id,
            &[],
            Some(export.to_path_buf()),
            None,
            true,
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(llm),
                preflight_sink: &|_| {},
            },
        )
        .await
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

/// What a run found, in a form two runs can be compared by: every finding's rule, place, severity
/// and title, sorted.
fn findings_of(export: &Path) -> Vec<String> {
    let doc: serde_json::Value =
        serde_json::from_str(&zip_entry(export, "json/inspection.json")).unwrap();
    let mut out: Vec<String> = doc["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            format!(
                "{} | {} | {} | {} | {} | {}",
                f["repo"], f["rule_id"], f["file"], f["line"], f["severity"], f["title"]
            )
        })
        .collect();
    out.sort();
    out
}

fn typst(test: &str) -> bool {
    camerata_test_env::typst_on_path_for(test)
}

/// An uninterrupted run over `repos` fixture repos: its findings and how many group review calls
/// it made.
/// (Over fixture repos that hold clone candidates when `clones` is set.)
async fn reference_with(repos: usize, clones: bool) -> (Vec<String>, usize) {
    let clock = ClockGuard::install();
    let ws = Workspace::with(repos, clones);
    let llm = Provider::new(&clock, None);
    let export = ws.export("reference.zip");
    let outcome = ws
        .run(&llm, &export)
        .await
        .expect("the reference run completes");
    assert!(
        outcome.incomplete_reasons.is_empty(),
        "{:?}",
        outcome.incomplete_reasons
    );
    assert_eq!(clock.now(), 0, "an uninterrupted run never waits");
    (findings_of(&export), llm.calls_of(Stage::GroupReview))
}

// ── A short drop costs minutes ───────────────────────────────────────────────────────────────

async fn a_short_drop_waits_and_finishes_like_an_uninterrupted_run(from: Stage, test: &str) {
    a_short_drop_case(from, test, false).await;
}

async fn a_short_drop_case(from: Stage, test: &str, clones: bool) {
    if !typst(test) {
        return;
    }
    let (want, _) = reference_with(1, clones).await;
    let clock = ClockGuard::install();
    let ws = Workspace::with(1, clones);
    // Three minutes: far past the transport's own retries, well inside the network wait.
    let llm = Provider::new(&clock, Some(Fault::Drop { from, secs: 180 }));
    let export = ws.export("short.zip");
    let outcome = ws
        .run(&llm, &export)
        .await
        .expect("the run waits and completes");
    assert!(llm.refused() > 0, "the drop happened");
    assert!(
        clock.now() >= 180,
        "the run waited out the drop: {} s",
        clock.now()
    );
    assert!(
        outcome.incomplete_reasons.is_empty(),
        "a short drop left the run incomplete: {:?}",
        outcome.incomplete_reasons
    );
    assert!(!outcome.outage_stopped());
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(
        findings_of(&export),
        want,
        "the same output as an uninterrupted run"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_short_drop_during_the_finder_costs_minutes_not_the_run() {
    a_short_drop_waits_and_finishes_like_an_uninterrupted_run(
        Stage::Finder,
        "a_short_drop_during_the_finder",
    )
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_short_drop_during_the_fix_stage_costs_minutes_not_the_run() {
    a_short_drop_waits_and_finishes_like_an_uninterrupted_run(
        Stage::Fix,
        "a_short_drop_during_the_fix_stage",
    )
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_short_drop_during_the_clone_check_costs_minutes_not_the_run() {
    a_short_drop_case(
        Stage::CloneJudge,
        "a_short_drop_during_the_clone_check",
        true,
    )
    .await;
}

// ── A long drop stops the run, and no later stage starts ─────────────────────────────────────

async fn a_long_drop_stops_the_run_and_a_resume_reviews_every_repo_once(from: Stage, test: &str) {
    a_long_drop_case(from, test, false).await;
}

async fn a_long_drop_case(from: Stage, test: &str, clones: bool) {
    if !typst(test) {
        return;
    }
    let (want, want_group_calls) = reference_with(2, clones).await;
    assert!(want_group_calls > 0, "the reference run had a group review");
    let clock = ClockGuard::install();
    let ws = Workspace::with(2, clones);
    // A day: past any wait.
    let down = Provider::new(
        &clock,
        Some(Fault::Drop {
            from,
            secs: 24 * 3600,
        }),
    );
    let export = ws.export("stopped.zip");
    let outcome = ws
        .run(&down, &export)
        .await
        .expect("an internal export is written");
    assert!(outcome.outage_stopped(), "{:?}", outcome.incomplete_reasons);
    assert_eq!(outcome.exit_code(), EXIT_OUTAGE_STOPPED);
    assert!(down.calls_of(from) > 0, "the drop reached the stage");
    assert_eq!(
        down.after_stop(),
        0,
        "a model request of some kind reached the provider after the outage stop"
    );
    assert!(
        clock.now() >= camerata_server::network_wait::NETWORK_WAIT_TOTAL.as_secs(),
        "the run waited the whole network wait before stopping: {} s",
        clock.now()
    );
    assert_eq!(
        down.calls_of(Stage::GroupReview),
        0,
        "the group reviewer was called after the outage stop"
    );
    assert_eq!(
        down.calls_of(Stage::DuplicateCheck),
        0,
        "the possible-duplicate check was called after the outage stop"
    );
    // The internal export is written from what finished, and says so.
    assert!(export.exists());
    let manifest: serde_json::Value =
        serde_json::from_str(&zip_entry(&export, "manifest.json")).unwrap();
    assert_eq!(manifest["inspection"]["run_incomplete"], true);

    // The provider is back: the resume finishes every repo and reviews them once, together.
    let up = Provider::new(&clock, None);
    let run_id = outcome.run_id.clone().expect("the run was saved");
    let resumed_export = ws.export("resumed.zip");
    let resumed = ws
        .resume(&up, &run_id, &resumed_export)
        .await
        .expect("the resume completes");
    assert!(
        resumed.incomplete_reasons.is_empty(),
        "{:?}",
        resumed.incomplete_reasons
    );
    assert_eq!(resumed.exit_code(), 0);
    assert_eq!(
        up.calls_of(Stage::GroupReview),
        want_group_calls,
        "the resume runs exactly one group review, over every repo"
    );
    assert_eq!(findings_of(&resumed_export), want);
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_long_drop_during_the_clone_check_stops_the_run_before_the_group_review() {
    a_long_drop_case(
        Stage::CloneJudge,
        "a_long_drop_during_the_clone_check",
        true,
    )
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_long_drop_during_the_finder_stops_the_run_before_the_group_review() {
    a_long_drop_stops_the_run_and_a_resume_reviews_every_repo_once(
        Stage::Finder,
        "a_long_drop_during_the_finder",
    )
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_long_drop_during_the_fix_stage_stops_the_run_before_the_group_review() {
    a_long_drop_stops_the_run_and_a_resume_reviews_every_repo_once(
        Stage::Fix,
        "a_long_drop_during_the_fix_stage",
    )
    .await;
}

/// Owner and PO ruling: after a stop, an `--internal` run writes its export marked incomplete,
/// built ONLY from what already finished; the export step starts no model-calling stage.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn after_an_outage_stop_the_internal_export_makes_no_model_call() {
    if !typst("after_an_outage_stop_the_internal_export") {
        return;
    }
    let clock = ClockGuard::install();
    let ws = Workspace::new(1);
    // The provider goes down at the fix stage, after the finder answered.
    let llm = Provider::new(
        &clock,
        Some(Fault::Drop {
            from: Stage::Fix,
            secs: 24 * 3600,
        }),
    );
    let export = ws.export("internal.zip");
    let outcome = ws
        .run(&llm, &export)
        .await
        .expect("an internal export is written");
    assert!(outcome.outage_stopped());
    let answered_before_stop = llm
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, ok)| *ok)
        .count();
    // Nothing after the stop was answered: every request from the drop on was refused, and none
    // came from a stage after the scan.
    assert_eq!(llm.calls_of(Stage::GroupReview), 0);
    assert_eq!(llm.calls_of(Stage::DuplicateCheck), 0);
    assert_eq!(
        llm.after_stop(),
        0,
        "a model request of some kind reached the provider after the outage stop"
    );
    assert!(answered_before_stop > 0);
    assert!(export.exists());
    assert!(zip_entry(&export, "README.txt").contains("INTERNAL INCOMPLETE"));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn after_an_outage_stop_a_client_export_is_refused() {
    if !typst("after_an_outage_stop_a_client_export") {
        return;
    }
    let clock = ClockGuard::install();
    let ws = Workspace::new(1);
    let llm = Provider::new(
        &clock,
        Some(Fault::Drop {
            from: Stage::Fix,
            secs: 24 * 3600,
        }),
    );
    let export = ws.export("client.zip");
    let mut args = ws.args(&export);
    args.client_engagement = true;
    let err = ws
        .run_args(&llm, args)
        .await
        .expect_err("a client export of a stopped run is refused");
    assert_eq!(err.exit_code(), EXIT_OUTAGE_STOPPED, "{err}");
    assert!(!export.exists(), "no package was written for the client");
    assert_eq!(llm.calls_of(Stage::GroupReview), 0);
    assert_eq!(llm.after_stop(), 0);
}

// ── An auth stop never waits ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_auth_failure_stops_the_run_without_waiting_for_the_network() {
    if !typst("an_auth_failure_stops_the_run_without_waiting") {
        return;
    }
    let clock = ClockGuard::install();
    let ws = Workspace::new(1);
    let llm = Provider::new(
        &clock,
        Some(Fault::Auth {
            from: Stage::Finder,
        }),
    );
    let export = ws.export("auth.zip");
    let outcome = ws
        .run(&llm, &export)
        .await
        .expect("an internal export is written");
    assert!(outcome.outage_stopped());
    assert_eq!(outcome.exit_code(), EXIT_OUTAGE_STOPPED);
    assert_eq!(clock.now(), 0, "an auth failure waited for the network");
    assert_eq!(llm.calls_of(Stage::GroupReview), 0);
}

/// The batch path (#582 review): a long drop while the fix stage's batches are submitted stops the
/// run, and once it stopped no batch is submitted. Polls and fetches are not billed and may go on.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_long_drop_in_batch_mode_stops_the_run_and_submits_no_batch_after_the_stop() {
    if !typst("a_long_drop_in_batch_mode") {
        return;
    }
    let clock = ClockGuard::install();
    let _poll = camerata_test_env::EnvGuard::set("CAMERATA_BATCH_POLL_SECS", "1");
    let ws = Workspace::new(1);
    let provider = Arc::new(Provider::new(
        &clock,
        Some(Fault::Drop {
            from: Stage::Fix,
            secs: 24 * 3600,
        }),
    ));
    let seam = camerata_server::ai_audit::BatchSeam::new(provider.clone());
    let control = Arc::new(camerata_server::operator_stop::RunControl::new());
    provider.watch(&control);
    let export = ws.export("batch.zip");
    let mut args = ws.args(&export);
    args.batch = true;
    args.realtime = false;
    let outcome = ws
        .run_on(&seam, args, control.clone())
        .await
        .expect("an internal export is written");
    assert!(outcome.outage_stopped(), "{:?}", outcome.incomplete_reasons);
    assert!(
        !provider.batches.lock().unwrap().is_empty(),
        "the run used the batch path before the drop"
    );
    assert!(
        provider.calls_of(Stage::Fix) > 0,
        "the drop reached the fix stage"
    );
    assert!(control.refuses_sends());
    assert_eq!(
        provider.after_stop(),
        0,
        "a batch was submitted (or a request sent) after the outage stop"
    );
    assert_eq!(provider.calls_of(Stage::GroupReview), 0);
}
