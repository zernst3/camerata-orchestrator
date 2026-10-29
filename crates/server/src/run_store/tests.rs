//! The run store: the plan before the work, replay of stored replies, the lock, interrupted runs,
//! batches, the resume checks and the cleanup. A scripted model; no live calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::journal::{wrap, JournalLlm};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

fn response(text: &str, cost: f64) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(cost),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// Answers each distinct prompt with the reply `reply(prompt)` and counts the calls. A prompt
/// containing "BOOM" fails like an unreachable provider while `fail_boom` is set.
struct Scripted {
    calls: AtomicUsize,
    fail_boom: std::sync::atomic::AtomicBool,
}

impl Scripted {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail_boom: std::sync::atomic::AtomicBool::new(false),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn answer(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if req.prompt.contains("BOOM") && self.fail_boom.load(Ordering::SeqCst) {
            return Err(anyhow::Error::new(
                camerata_llm::failure::LlmCallFailure::new(
                    camerata_llm::failure::FailureClass::Transport,
                    "Anthropic API HTTP 529: overloaded",
                ),
            ));
        }
        if req.prompt.contains("MALFORMED") {
            return Ok(response("no json at all", 0.01));
        }
        Ok(response(
            &serde_json::json!({ "echo": req.prompt }).to_string(),
            0.05,
        ))
    }
}

#[async_trait::async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.answer(&req)
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.answer(&req)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().unwrap();
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@example.com"]);
    g(&["config", "user.name", "T"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "one"]);
    dir
}

fn models() -> ModelsRecord {
    ModelsRecord {
        audit: "claude-a".to_string(),
        calibration: "claude-a".to_string(),
        fix: "claude-a".to_string(),
        backend: "api".to_string(),
        mode: "parallel".to_string(),
    }
}

fn key() -> AiScanKey {
    AiScanKey {
        audit_model: "claude-a".to_string(),
        calibration_model: String::new(),
        prompt_schema_version: 2,
        directives_fingerprint: "fp1".to_string(),
        terse_output: true,
        applies_to_routing: true,
    }
}

fn new_run(repo: &tempfile::TempDir) -> Arc<RunStore> {
    RunStore::create(NewRun {
        project_key: "proj-1".to_string(),
        repos: vec![("acme/api".to_string(), repo.path().to_path_buf())],
        models: models(),
        ai_scan_key: key(),
        args: serde_json::json!({"client": "x"}),
        estimate_usd: Some(10.0),
        include_alternatives: true,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

/// Run `fut` the way a run does: with its prompt seed in scope (the journal refuses a call
/// without one).
async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([9u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

fn ask(prompt: &str) -> LlmRequest {
    LlmRequest::new(prompt)
}

/// Close the store the way a finished process would (drops the lock) and reopen the run.
fn reopen(store: Arc<RunStore>) -> Arc<RunStore> {
    let id = store.run_id();
    drop(store);
    RunStore::open(&id).unwrap()
}

#[test]
#[serial_test::serial(process_env)]
fn the_plan_is_on_disk_with_every_part_pending_before_any_work() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let on_disk = read_record(&store.run_id()).unwrap();
    assert_eq!(on_disk.status, RunStatus::Running);
    assert_eq!(on_disk.attempt, 1);
    assert!(on_disk.plan.iter().all(|p| p.status == PartStatus::Pending));
    let kinds: Vec<PartKind> = on_disk.plan.iter().map(|p| p.kind).collect();
    for k in [
        PartKind::Tools,
        PartKind::Scan,
        PartKind::Calibration,
        PartKind::Alternatives,
        PartKind::Detail,
        PartKind::Fix,
        PartKind::Merge,
        PartKind::Export,
    ] {
        assert!(kinds.contains(&k), "{k:?} is planned");
    }
    assert_eq!(on_disk.repos[0].commit.as_deref().map(str::len), Some(40));
    assert!(on_disk.repos[0].clean);
    assert_eq!(on_disk.ai_scan_key, key());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the run folder is private");
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_good_reply_is_replayed_on_a_resume_and_a_failed_one_is_asked_again() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        model.fail_boom.store(true, Ordering::SeqCst);
        {
            let journal = JournalLlm::new(&model, store.clone());
            assert!(journal.complete(ask("part one")).await.is_ok());
            assert!(journal.complete(ask("MALFORMED part")).await.is_ok());
            assert!(journal.complete(ask("BOOM part")).await.is_err());
        }
        assert_eq!(model.calls(), 3);

        // The process ends; the provider is back; the run is resumed.
        let store = reopen(store);
        assert_eq!(store.record().attempt, 2);
        model.fail_boom.store(false, Ordering::SeqCst);
        let before = model.calls();
        let journal = JournalLlm::new(&model, store.clone());
        let replayed = journal.complete(ask("part one")).await.unwrap();
        assert_eq!(
            model.calls(),
            before,
            "a stored good reply is not asked again"
        );
        assert_eq!(
            replayed.cost_usd,
            Some(0.0),
            "a replay costs this attempt nothing"
        );
        assert!(replayed.text.contains("part one"));
        assert!(journal.complete(ask("MALFORMED part")).await.is_ok());
        assert!(journal.complete(ask("BOOM part")).await.is_ok());
        assert_eq!(
            model.calls(),
            before + 2,
            "the malformed reply and the failed call were asked again"
        );
    })
    .await;
}

/// A call that failed at its first occurrence and succeeded at its second (the end-of-run second
/// try sends the same prompt) has a good reply stored. A resume asks the prompt once, so it must
/// be answered from that stored reply, not sent to the model again: the uninterrupted run used
/// that reply, and paying for it twice is what the store exists to prevent.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_good_reply_stored_at_a_later_occurrence_answers_the_first_ask_of_a_resume() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        {
            let j = JournalLlm::new(&model, store.clone());
            model.fail_boom.store(true, Ordering::SeqCst);
            assert!(
                j.complete(ask("BOOM part")).await.is_err(),
                "first try fails"
            );
            model.fail_boom.store(false, Ordering::SeqCst);
            let second = j.complete(ask("BOOM part")).await.unwrap();
            assert!(second.text.contains("BOOM part"), "the second try is good");
        }
        assert_eq!(model.calls(), 2);

        let store = reopen(store);
        let before = model.calls();
        let j = JournalLlm::new(&model, store);
        let replayed = j.complete(ask("BOOM part")).await.unwrap();
        assert_eq!(
            model.calls(),
            before,
            "the stored good reply answers the resume's first ask; nothing is sent again"
        );
        assert!(replayed.text.contains("BOOM part"));
        assert_eq!(replayed.cost_usd, Some(0.0));
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_same_request_asked_twice_replays_in_order() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        struct Counting(AtomicUsize);
        #[async_trait::async_trait]
        impl LlmPort for Counting {
            async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                Ok(response(&format!(r#"{{"n":{n}}}"#), 0.01))
            }
            async fn complete_streaming(
                &self,
                req: LlmRequest,
                _d: &mut (dyn for<'a> FnMut(&'a str) + Send),
            ) -> anyhow::Result<LlmResponse> {
                self.complete(req).await
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        let model = Counting(AtomicUsize::new(0));
        {
            let j = JournalLlm::new(&model, store.clone());
            assert_eq!(j.complete(ask("same")).await.unwrap().text, r#"{"n":0}"#);
            assert_eq!(j.complete(ask("same")).await.unwrap().text, r#"{"n":1}"#);
        }
        let store = reopen(store);
        let j = JournalLlm::new(&model, store);
        assert_eq!(j.complete(ask("same")).await.unwrap().text, r#"{"n":0}"#);
        assert_eq!(j.complete(ask("same")).await.unwrap().text, r#"{"n":1}"#);
        assert_eq!(model.0.load(Ordering::SeqCst), 2, "nothing was asked again");
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_content_failure_sets_aside_the_replies_of_its_stage_and_nothing_else() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        {
            let j = JournalLlm::new(&model, store.clone());
            // Stage `Other` (no known system prompt): one reply a later step will reject, one it keeps.
            j.complete(ask("rejected one")).await.unwrap();
        }
        store.finish(&FinishOutcome {
            failures: vec![FinishFailure {
                repo: "acme/api".to_string(),
                // `of_pass_name` maps this to Scan; the calls above are `Other`, so they stay good.
                pass: "audit 1/1 · rules 1/1".to_string(),
                class: "content".to_string(),
                files: Vec::new(),
                scope_note: false,
            }],
            ..FinishOutcome::default()
        });
        let store = reopen(store);
        let before = model.calls();
        let j = JournalLlm::new(&model, store);
        j.complete(ask("rejected one")).await.unwrap();
        assert_eq!(
            model.calls(),
            before,
            "an unrelated stage keeps its stored replies"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_rejected_reply_of_a_failed_stage_is_asked_again_on_the_resume() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        // A calibration request: the system prompt the stage sends.
        let calibration = |prompt: &str| {
            LlmRequest::new(prompt).with_system(crate::ai_audit::verify_system_prompt())
        };
        let model = Scripted::new();
        {
            let j = JournalLlm::new(&model, store.clone());
            j.complete(calibration("Repository: acme/api\nrejected"))
                .await
                .unwrap();
            j.complete(calibration("Repository: other/repo\nkept"))
                .await
                .unwrap();
        }
        store.finish(&FinishOutcome {
            failures: vec![FinishFailure {
                repo: "acme/api".to_string(),
                pass: "hour estimation".to_string(),
                class: "content".to_string(),
                files: Vec::new(),
                scope_note: false,
            }],
            ..FinishOutcome::default()
        });
        let store = reopen(store);
        let before = model.calls();
        let j = JournalLlm::new(&model, store);
        j.complete(calibration("Repository: acme/api\nrejected"))
            .await
            .unwrap();
        assert_eq!(
            model.calls(),
            before + 1,
            "the rejected reply goes to the model again"
        );
        j.complete(calibration("Repository: other/repo\nkept"))
            .await
            .unwrap();
        assert_eq!(model.calls(), before + 1, "another repo's reply is kept");
    })
    .await;
}

#[test]
#[serial_test::serial(process_env)]
fn a_live_lock_refuses_a_second_worker_and_a_dead_one_is_taken_over() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    let dir = store.dir().to_path_buf();
    assert_eq!(liveness_of(&dir), Liveness::Live);
    assert!(matches!(RunStore::open(&id), Err(RunStoreError::Busy(_))));
    assert_eq!(store.record().state_for_test(&dir), RunState::Running);

    // The process dies without cleaning up: the kernel releases its lock on the file, and the
    // file it left behind names a pid that is gone. (In one process the only way to release the
    // lock is to drop its holder; the file is then written back as the dead process left it.)
    let dead = {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    drop(store);
    let stale = serde_json::json!({
        "pid": dead,
        "started_at": now(),
        "heartbeat_at": now(),
    });
    std::fs::write(dir.join("lock.json"), stale.to_string()).unwrap();
    assert_eq!(liveness_of(&dir), Liveness::Stale);
    let record = read_record(&id).unwrap();
    assert_eq!(record.state(liveness_of(&dir)), RunState::Interrupted);
    assert!(
        !record.remaining_parts().is_empty(),
        "every part of an interrupted run is remaining"
    );
    assert!(
        list_runs()
            .iter()
            .any(|r| r.run_id == id && r.state == RunState::Interrupted),
        "the listing says Interrupted"
    );
    // A stale lock is taken over by the resume.
    let again = RunStore::open(&id).unwrap();
    assert_eq!(again.record().attempt, 2);
}

/// The lock is the operating system's, not the heartbeat: a process that still holds it is live
/// however old its heartbeat (the laptop slept, the runtime was starved), so a second taker is
/// refused and the cockpit says Running, not Interrupted. Once the holder is gone the same file
/// is stale and is taken over, and the taker's lock is removed when it is dropped.
#[test]
#[serial_test::serial(process_env)]
fn a_held_lock_with_an_old_heartbeat_is_live_and_a_released_one_is_stale() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    let dir = store.dir().to_path_buf();
    let old = chrono::Utc::now() - chrono::Duration::seconds(STALE_HEARTBEAT_SECS + 30);
    let stale = serde_json::json!({
        "pid": std::process::id(),
        "started_at": now(),
        "heartbeat_at": old.to_rfc3339(),
    });
    std::fs::write(dir.join("lock.json"), stale.to_string()).unwrap();
    assert_eq!(
        liveness_of(&dir),
        Liveness::Live,
        "the holder is alive, whatever its heartbeat says"
    );
    assert!(
        matches!(RunStore::open(&id), Err(RunStoreError::Busy(_))),
        "a second taker is refused while the lock is held"
    );
    assert_eq!(store.record().state_for_test(&dir), RunState::Running);

    // The holder is gone: the same file, no longer held, is stale and is taken over.
    drop(store);
    std::fs::write(dir.join("lock.json"), stale.to_string()).unwrap();
    assert_eq!(liveness_of(&dir), Liveness::Stale);
    let taken = RunLock::acquire(&dir).expect("a released lock is taken over");
    assert_eq!(liveness_of(&dir), Liveness::Live);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("lock.json")).unwrap()).unwrap();
    assert_eq!(
        written["pid"],
        std::process::id(),
        "the lock is now the taker's"
    );
    drop(taken);
    assert_eq!(
        liveness_of(&dir),
        Liveness::None,
        "dropping the lock removes it"
    );
}

impl RunRecord {
    fn state_for_test(&self, dir: &std::path::Path) -> RunState {
        self.state(liveness_of(dir))
    }
}

#[test]
#[serial_test::serial(process_env)]
fn a_resume_is_refused_in_plain_words_when_anything_it_depends_on_differs() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    drop(store);
    let record = read_record(&id).unwrap();
    let trees = |dir: &std::path::Path| vec![("acme/api".to_string(), tree_state(dir))];
    let ok = |record: &RunRecord, key: &AiScanKey, models: &ModelsRecord, t| {
        check_resume(&ResumeCheck {
            record,
            ai_scan_key: key,
            models,
            trees: t,
        })
    };
    assert_eq!(ok(&record, &key(), &models(), trees(repo.path())), Ok(()));

    // Another commit.
    std::fs::write(repo.path().join("a.txt"), "two\n").unwrap();
    let g = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .current_dir(repo.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    };
    // An uncommitted edit.
    let edited = ok(&record, &key(), &models(), trees(repo.path()));
    assert!(
        matches!(&edited, Err(RunStoreError::Refused(m)) if m.contains("changes that were not there")),
        "{edited:?}"
    );
    g(&["commit", "-qam", "two"]);
    let moved = ok(&record, &key(), &models(), trees(repo.path()));
    assert!(
        matches!(&moved, Err(RunStoreError::Refused(m)) if m.contains("commit")),
        "{moved:?}"
    );
    g(&["reset", "-q", "--hard", "HEAD~1"]);
    assert_eq!(ok(&record, &key(), &models(), trees(repo.path())), Ok(()));

    // A different model, rules, or prompt version.
    let mut other_models = models();
    other_models.audit = "claude-b".to_string();
    assert!(matches!(
        ok(&record, &key(), &other_models, trees(repo.path())),
        Err(RunStoreError::Refused(m)) if m.contains("different models")
    ));
    let mut other_rules = key();
    other_rules.directives_fingerprint = "fp2".to_string();
    assert!(matches!(
        ok(&record, &other_rules, &models(), trees(repo.path())),
        Err(RunStoreError::Refused(m)) if m.contains("rules changed")
    ));
    let mut other_prompt = key();
    other_prompt.prompt_schema_version = 3;
    assert!(matches!(
        ok(&record, &other_prompt, &models(), trees(repo.path())),
        Err(RunStoreError::Refused(m)) if m.contains("scan asks the model")
    ));
    let mut other_engine = record.clone();
    other_engine.engine_version = "9.9.9+prompt1".to_string();
    assert!(matches!(
        ok(&other_engine, &key(), &models(), trees(repo.path())),
        Err(RunStoreError::Refused(m)) if m.contains("another version")
    ));
}

#[test]
#[serial_test::serial(process_env)]
fn the_tree_a_run_left_behind_is_accepted_and_a_complete_run_is_a_no_op() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    // The inspection's own build step leaves a file behind.
    std::fs::write(repo.path().join("Cargo.lock"), "# built\n").unwrap();
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        export_written: true,
        complete: false,
        incomplete_reasons: vec!["one part failed".to_string()],
        failures: Vec::new(),
        not_run: Vec::new(),
    });
    drop(store);
    let record = read_record(&id).unwrap();
    assert_eq!(record.status, RunStatus::Incomplete);
    let check = |record: &RunRecord| {
        check_resume(&ResumeCheck {
            record,
            ai_scan_key: &key(),
            models: &models(),
            trees: vec![("acme/api".to_string(), tree_state(repo.path()))],
        })
    };
    assert_eq!(
        check(&record),
        Ok(()),
        "the run's own leftovers are not a change"
    );

    let store = RunStore::open(&id).unwrap();
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        export_written: true,
        complete: true,
        ..FinishOutcome::default()
    });
    drop(store);
    let done = read_record(&id).unwrap();
    assert_eq!(done.status, RunStatus::Complete);
    assert!(done.plan.iter().all(|p| p.status == PartStatus::Done));
    match check(&done) {
        Err(RunStoreError::AlreadyComplete(m)) => assert!(m.contains("nothing to resume")),
        other => panic!("{other:?}"),
    }
}

#[test]
#[serial_test::serial(process_env)]
fn finish_marks_failed_parts_failed_and_not_run_parts_pending() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        export_written: false,
        complete: false,
        failures: vec![FinishFailure {
            repo: "acme/api".to_string(),
            pass: "fix-specific generation".to_string(),
            class: "transport".to_string(),
            files: Vec::new(),
            scope_note: false,
        }],
        not_run: vec![FinishNotRun {
            repo: "acme/api".to_string(),
            pass: "rule-alternative recommendations".to_string(),
        }],
        incomplete_reasons: vec!["x".to_string()],
    });
    let record = store.record();
    let status = |kind| record.plan.iter().find(|p| p.kind == kind).unwrap().status;
    assert_eq!(status(PartKind::Fix), PartStatus::Failed);
    assert_eq!(status(PartKind::Alternatives), PartStatus::Pending);
    assert_eq!(status(PartKind::Scan), PartStatus::Done);
    assert_eq!(status(PartKind::Export), PartStatus::Failed);
    let remaining: Vec<PartKind> = record.remaining_parts().iter().map(|p| p.kind).collect();
    assert!(remaining.contains(&PartKind::Fix));
    assert!(remaining.contains(&PartKind::Alternatives));
    assert!(!remaining.contains(&PartKind::Scan));
}

#[test]
fn a_scan_pass_name_maps_to_scan_and_a_tool_pass_to_tools() {
    assert_eq!(
        PartKind::of_pass_name("audit 1/3 · rules 2/4"),
        PartKind::Scan
    );
    assert_eq!(
        PartKind::of_pass_name("hour estimation"),
        PartKind::Calibration
    );
    assert_eq!(
        PartKind::of_pass_name("calibration batch 2 of 3"),
        PartKind::Calibration
    );
    assert_eq!(
        PartKind::of_pass_name("commodity taint pass"),
        PartKind::Tools
    );
    assert_eq!(
        PartKind::of_pass_name("the eslint preview pass"),
        PartKind::Tools
    );
    assert_eq!(
        PartKind::of_pass_name("rule-alternative recommendations"),
        PartKind::Alternatives
    );
    assert_eq!(
        PartKind::of_pass_name("detail regeneration"),
        PartKind::Detail
    );
    assert_eq!(
        PartKind::of_pass_name("fix-specific generation"),
        PartKind::Fix
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_estimate_counts_stored_and_unsent_calls() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        model.fail_boom.store(true, Ordering::SeqCst);
        {
            let j = JournalLlm::new(&model, store.clone());
            j.complete(ask("one")).await.unwrap();
            j.complete(ask("two")).await.unwrap();
            let _ = j.complete(ask("BOOM three")).await;
        }
        let id = store.run_id();
        drop(store);
        let e = estimate_resume(&id).unwrap();
        assert_eq!(e.calls_stored, 2);
        assert_eq!(e.calls_to_send_again, 1);
        assert!(e.usd > 0.0 && e.usd <= 10.0, "{e:?}");
        assert!(
            e.render().contains("2 model call(s) stored"),
            "{}",
            e.render()
        );
        assert!(!e.remaining.is_empty());
    })
    .await;
}

fn make_run(root: &std::path::Path, id: &str, project: &str, commit: &str, status: RunStatus) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let record = RunRecord {
        schema: STORE_SCHEMA_VERSION,
        run_id: id.to_string(),
        created_at: id.to_string(),
        updated_at: id.to_string(),
        status,
        origin: RunOrigin::Cli,
        project_id: None,
        attempt: 1,
        engine_version: engine_version(),
        project_key: project.to_string(),
        repos: vec![RepoRecord {
            name: "a/b".to_string(),
            dir: "/Users/auditor/a".to_string(),
            commit: Some(commit.to_string()),
            clean: true,
            tree_at_start: "x".to_string(),
            tree_at_end: None,
        }],
        models: models(),
        rules_fingerprint: "fp".to_string(),
        ai_scan_key: key(),
        nonce_seed: "00".repeat(16),
        args: serde_json::Value::Null,
        estimate_usd: None,
        plan: Vec::new(),
        incomplete_reasons: Vec::new(),
        notes: Vec::new(),
    };
    std::fs::write(dir.join("run.json"), serde_json::to_vec(&record).unwrap()).unwrap();
}

#[test]
fn cleanup_keeps_the_newest_and_never_drops_an_incomplete_run_without_a_newer_complete_one() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // Newest first by id: 7 .. 1.
    make_run(r, "20261007", "p", "c3", RunStatus::Complete);
    make_run(r, "20261006", "p", "c3", RunStatus::Incomplete);
    make_run(r, "20261005", "p", "c2", RunStatus::Incomplete);
    make_run(r, "20261004", "p", "c1", RunStatus::Incomplete);
    make_run(r, "20261003", "p", "c1", RunStatus::Complete);
    make_run(r, "20261002", "p", "c0", RunStatus::Incomplete);
    make_run(r, "20261001", "p", "c0", RunStatus::Complete);
    make_run(r, "20260901", "other", "c0", RunStatus::Complete);
    let deleted = prune_runs_in(r, "p", 3);
    // Beyond the newest three (07, 06, 05): 04 incomplete has a newer complete run (03) for the
    // same commit only if that run is NEWER; 03 is older, so 04 stays. 03 and 01 are complete and
    // go. 02 is incomplete with a newer complete run for its commit? c0: 01 is older, so it stays.
    assert_eq!(
        deleted,
        vec!["20261003".to_string(), "20261001".to_string()]
    );
    assert!(
        r.join("20261004").exists(),
        "incomplete, no newer complete run on c1"
    );
    assert!(
        r.join("20261002").exists(),
        "incomplete, no newer complete run on c0"
    );
    assert!(r.join("20260901").exists(), "another project is untouched");

    // Once a newer complete run exists for the same commit, the old incomplete one may go.
    make_run(r, "20261008", "p", "c1", RunStatus::Complete);
    let deleted = prune_runs_in(r, "p", 3);
    assert!(deleted.contains(&"20261004".to_string()), "{deleted:?}");
    assert!(!deleted.contains(&"20261002".to_string()), "{deleted:?}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn wrapping_a_plain_completer_journals_it() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        let journaled = wrap(&model, store.clone());
        journaled.complete(ask("via wrap")).await.unwrap();
        assert_eq!(model.calls(), 1);
        let id = store.run_id();
        drop(journaled);
        drop(store);
        let again = RunStore::open(&id).unwrap();
        let journaled = wrap(&model, again);
        journaled.complete(ask("via wrap")).await.unwrap();
        assert_eq!(model.calls(), 1, "replayed from the store");
    })
    .await;
}

/// A model call journaled with no prompt seed in scope would carry a random delimiter token, so
/// its stored reply could never match the same call on a resume and it would be paid again. In
/// tests the journal refuses it (the model is not called and nothing is stored); with the seed in
/// scope the same call goes through.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_journal_refuses_a_call_with_no_prompt_seed_in_scope() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let model = Scripted::new();
    let journal = JournalLlm::new(&model, store.clone());
    let err = journal.complete(ask("unseeded")).await.unwrap_err();
    assert!(err.to_string().contains("no prompt seed in scope"), "{err}");
    let err = journal
        .complete_streaming(ask("unseeded"), &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no prompt seed in scope"), "{err}");
    assert_eq!(model.calls(), 0, "the refused call never reached the model");
    assert_eq!(
        std::fs::read_dir(store.dir().join("calls"))
            .unwrap()
            .count(),
        0,
        "nothing was stored for it"
    );
    seeded(async {
        journal.complete(ask("seeded")).await.unwrap();
    })
    .await;
    assert_eq!(model.calls(), 1);
}

/// In production the same call goes ahead (the operator's money is spent either way) but the run
/// says so: a note on the run, written once however many such calls are made, and a warning on
/// stderr beside it. A seeded call leaves no note.
#[test]
#[serial_test::serial(process_env)]
fn in_production_an_unseeded_call_runs_and_the_run_records_a_note() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    use super::journal::{check_seed, SeedPolicy, UNSEEDED_CALL_NOTE};
    check_seed(&store, true, SeedPolicy::WarnAndRecord).unwrap();
    assert!(
        store.record().notes.is_empty(),
        "a seeded call leaves no note"
    );
    check_seed(&store, false, SeedPolicy::WarnAndRecord).unwrap();
    check_seed(&store, false, SeedPolicy::WarnAndRecord).unwrap();
    assert_eq!(store.record().notes, vec![UNSEEDED_CALL_NOTE.to_string()]);
    let id = store.run_id();
    drop(store);
    assert_eq!(
        read_record(&id).unwrap().notes,
        vec![UNSEEDED_CALL_NOTE.to_string()],
        "the note is on disk"
    );
    assert!(check_seed(&RunStore::open(&id).unwrap(), false, SeedPolicy::Refuse).is_err());
}

/// A note on the run is put on the report the operator sees (the cockpit shows the message under
/// the scan result, the command line prints the notes): after the report's own notes, each once,
/// and a run with no notes leaves the message as it was.
#[test]
#[serial_test::serial(process_env)]
fn the_runs_notes_are_put_on_the_report_the_operator_sees() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let mut report = crate::onboard::ScanReport::gated(&["acme/one".to_string()]);
    report.message = Some("acme/one: AI review deselected".to_string());
    note_report(&store, &mut report);
    assert_eq!(
        report.message.as_deref(),
        Some("acme/one: AI review deselected"),
        "no notes, no change"
    );
    store.add_note("a model call was made with no prompt seed in scope");
    store.add_note("a model call was made with no prompt seed in scope");
    note_report(&store, &mut report);
    assert_eq!(
        report.message.as_deref(),
        Some("acme/one: AI review deselected · a model call was made with no prompt seed in scope")
    );
    note_report(&store, &mut report);
    assert_eq!(
        report.message.as_deref(),
        Some("acme/one: AI review deselected · a model call was made with no prompt seed in scope"),
        "a note already on the report is not added twice"
    );
    let mut bare = crate::onboard::ScanReport::gated(&[]);
    bare.message = None;
    note_report(&store, &mut bare);
    assert_eq!(
        bare.message.as_deref(),
        Some("a model call was made with no prompt seed in scope"),
        "a report with no message of its own carries the note alone"
    );
}

mod batches {
    use super::*;
    use crate::ai_audit::BatchApi;
    use crate::llm::{
        BatchItem, BatchItemParams, BatchMessage, BatchRequestCounts, BatchResultRow, BatchStatus,
        CacheTtl,
    };

    #[derive(Default)]
    struct FakeBatches {
        submits: AtomicUsize,
        fetches: AtomicUsize,
        completer: Option<Scripted>,
    }

    #[async_trait::async_trait]
    impl BatchApi for FakeBatches {
        fn has_api_key(&self) -> bool {
            true
        }
        fn realtime(&self) -> &dyn LlmPort {
            self.completer.as_ref().unwrap()
        }
        async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
            None
        }
        async fn submit(&self, _items: Vec<BatchItem>) -> anyhow::Result<String> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Ok("msgbatch_1".to_string())
        }
        async fn poll(&self, _id: &str) -> anyhow::Result<BatchStatus> {
            Ok(BatchStatus {
                processing_status: "ended".to_string(),
                request_counts: BatchRequestCounts::default(),
            })
        }
        async fn fetch(&self, _id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(vec![BatchResultRow {
                custom_id: "c0-b0".to_string(),
                response: Some(response(r#"{"findings":[]}"#, 0.2)),
                error: None,
                billed: None,
            }])
        }
    }

    impl Default for Scripted {
        fn default() -> Self {
            Self::new()
        }
    }

    fn item() -> BatchItem {
        BatchItem {
            custom_id: "c0-b0".to_string(),
            params: BatchItemParams {
                model: "claude-a".to_string(),
                max_tokens: 100,
                system: None,
                messages: vec![BatchMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("hi"),
                }],
                output_config: None,
                temperature: None,
            },
        }
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_submitted_batch_is_never_submitted_again_and_a_fetched_one_is_not_fetched_again() {
        seeded(async {
            let (_s, _g) = state_dir();
            let repo = git_repo();
            let store = new_run(&repo);
            let fake = Arc::new(FakeBatches {
                completer: Some(Scripted::new()),
                ..FakeBatches::default()
            });
            {
                let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
                let id = api.submit(vec![item()]).await.unwrap();
                assert_eq!(id, "msgbatch_1");
                // The process dies here: the batch was submitted, not fetched.
            }
            assert!(
                std::fs::read_dir(store.dir().join("batches"))
                    .unwrap()
                    .count()
                    == 1,
                "the batch id is on disk as soon as the submit returned"
            );
            let store = reopen(store);
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let id = api.submit(vec![item()]).await.unwrap();
            assert_eq!(id, "msgbatch_1");
            assert_eq!(
                fake.submits.load(Ordering::SeqCst),
                1,
                "nothing new was submitted"
            );
            assert_eq!(api.poll(&id).await.unwrap().processing_status, "ended");
            let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(fake.fetches.load(Ordering::SeqCst), 1);
            drop(api);
            let store = reopen(store);
            let api = journal::JournalBatchApi::new(fake.clone(), store);
            let id = api.submit(vec![item()]).await.unwrap();
            let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(
                fake.fetches.load(Ordering::SeqCst),
                1,
                "fetched results are kept"
            );
            assert_eq!(fake.submits.load(Ordering::SeqCst), 1);
            assert_eq!(rows[0].response.as_ref().unwrap().cost_usd, Some(0.0));
        })
        .await;
    }
}

#[test]
#[serial_test::serial(process_env)]
fn the_preview_says_what_remains_and_why_a_resume_cannot_start() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        failures: vec![FinishFailure {
            repo: "acme/api".to_string(),
            pass: "fix-specific generation".to_string(),
            class: "transport".to_string(),
            files: Vec::new(),
            scope_note: false,
        }],
        incomplete_reasons: vec!["a part failed".to_string()],
        ..FinishOutcome::default()
    });
    drop(store);
    let preview = preview_resume(&id).unwrap();
    assert_eq!(preview.state, RunState::Incomplete);
    assert!(preview.refusal.is_none(), "{:?}", preview.refusal);
    assert!(preview.text.contains("fix text"), "{}", preview.text);
    // The repo moves on: the preview says why it cannot be resumed.
    std::fs::write(repo.path().join("a.txt"), "changed\n").unwrap();
    let preview = preview_resume(&id).unwrap();
    assert!(
        preview
            .refusal
            .as_deref()
            .is_some_and(|r| r.contains("changes that were not there")),
        "{:?}",
        preview.refusal
    );
    assert!(matches!(
        preview_resume("no-such-run"),
        Err(RunStoreError::NotFound(_))
    ));
}

/// A cockpit run made for the listing tests: `origin` Cockpit, the repo `repo` as "acme/api".
fn cockpit_run(repo: &tempfile::TempDir) -> Arc<RunStore> {
    RunStore::create(NewRun {
        project_key: "proj-1".to_string(),
        repos: vec![("acme/api".to_string(), repo.path().to_path_buf())],
        models: models(),
        ai_scan_key: key(),
        args: serde_json::json!({"repos": ["acme/api"]}),
        estimate_usd: Some(10.0),
        include_alternatives: false,
        origin: RunOrigin::Cockpit,
        project_id: None,
    })
    .unwrap()
}

/// The listing says Live for a run a process holds, Interrupted for one whose process is gone
/// before it finished, Incomplete for one that finished with a part left, and Complete for one that
/// finished: and only an unfinished run of the cockpit is offered for resuming here. A run that is
/// live is never offered.
#[test]
#[serial_test::serial(process_env)]
fn the_listing_tells_live_interrupted_incomplete_and_complete_apart() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let all = |_: &RunRecord| true;

    // Live: the store is held.
    let live = cockpit_run(&repo);
    let live_id = live.run_id();
    let listed = list_inspections_in(&runs_dir(), all);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, InspectionStatus::Live);
    assert!(!listed[0].resumable_here, "a live run is never offered");

    // Interrupted: dropped while still marked running.
    drop(live);
    let listed = list_inspections_in(&runs_dir(), all);
    assert_eq!(listed[0].id, live_id);
    assert_eq!(listed[0].status, InspectionStatus::Interrupted);
    assert!(listed[0].resumable_here);
    assert_eq!(listed[0].origin, RunOrigin::Cockpit);
    assert_eq!(listed[0].repos, vec!["acme/api".to_string()]);
    assert!(listed[0].remaining_parts > 0, "{:?}", listed[0]);

    // Incomplete: finished with a failed part.
    let store = RunStore::open(&live_id).unwrap();
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        export_written: false,
        complete: false,
        incomplete_reasons: vec!["one part failed".to_string()],
        failures: vec![FinishFailure {
            repo: "acme/api".to_string(),
            pass: "hour estimation".to_string(),
            class: "transport".to_string(),
            files: Vec::new(),
            scope_note: false,
        }],
        ..FinishOutcome::default()
    });
    drop(store);
    let listed = list_inspections_in(&runs_dir(), all);
    assert_eq!(listed[0].status, InspectionStatus::Incomplete);
    assert!(listed[0].resumable_here);
    assert_eq!(listed[0].reasons, vec!["one part failed".to_string()]);

    // Complete: nothing remains, and nothing is offered.
    let store = RunStore::open(&live_id).unwrap();
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        export_written: true,
        complete: true,
        ..FinishOutcome::default()
    });
    drop(store);
    let listed = list_inspections_in(&runs_dir(), all);
    assert_eq!(listed[0].status, InspectionStatus::Complete);
    assert_eq!(listed[0].remaining_parts, 0);
    assert!(!listed[0].resumable_here);

    // A command-line run is listed but not resumable from the cockpit.
    let cli = new_run(&repo);
    drop(cli);
    let listed = list_inspections_in(&runs_dir(), all);
    let cli_row = listed
        .iter()
        .find(|r| r.origin == RunOrigin::Cli)
        .expect("the command line run is listed");
    assert_eq!(cli_row.status, InspectionStatus::Interrupted);
    assert!(!cli_row.resumable_here);
    // Newest first.
    assert!(listed.windows(2).all(|w| w[0].id >= w[1].id));
}

/// Only the open project's runs are listed.
#[test]
#[serial_test::serial(process_env)]
fn the_listing_holds_only_the_runs_the_caller_says_belong_to_the_project() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let other = git_repo();
    drop(cockpit_run(&repo));
    let stranger = RunStore::create(NewRun {
        project_key: "proj-2".to_string(),
        repos: vec![("acme/other".to_string(), other.path().to_path_buf())],
        models: models(),
        ai_scan_key: key(),
        args: serde_json::Value::Null,
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cockpit,
        project_id: None,
    })
    .unwrap();
    drop(stranger);
    let mine = list_inspections_in(&runs_dir(), |r| {
        r.repos.iter().all(|x| x.name == "acme/api")
    });
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].repos, vec!["acme/api".to_string()]);
}
