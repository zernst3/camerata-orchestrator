//! The run store: the plan before the work, replay of stored replies, the lock, interrupted runs,
//! batches, the resume checks and the cleanup. A scripted model; no live calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::journal::{wrap, JournalLlm};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

fn response(text: &str, cost: f64) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: None,
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
            // An echo that is also a stage 1 verdict (`real`), so a calibration request gets a reply
            // its own parser accepts.
            &serde_json::json!({ "echo": req.prompt, "real": "yes" }).to_string(),
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
        resolved: true,
        group_review: String::new(),
        finder: String::new(),
        finder_model: String::new(),
        scan_exclusions: Vec::new(),
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
        pin: crate::run_store::engine_pin::EnginePin::for_run(None, &Default::default()),
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
            build: &engine_pin::BuildId::current(),
            corpus_version: None,
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
    // Round 1 item 10: the Excluded by agreement list is part of the run's identity. Pieces
    // checkpointed under another list may hold findings in files that are now excluded (or miss
    // ones that are no longer), so a resume with a different list is refused.
    let mut other_list = models();
    other_list.scan_exclusions = vec!["app/seeds/**".to_string()];
    let refused = ok(&record, &key(), &other_list, trees(repo.path()));
    assert!(
        matches!(&refused, Err(RunStoreError::Refused(m))
            if m.contains("Excluded by agreement") && m.contains("start a new run")),
        "{refused:?}"
    );
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
}

/// The tree-changed refusal names the changed paths of that repo, at most twenty, then how many
/// more. An operator-only message.
#[test]
#[serial_test::serial(process_env)]
fn the_tree_changed_refusal_names_the_changed_paths_up_to_twenty() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    drop(store);
    let record = read_record(&id).unwrap();
    let refuse_now = || {
        check_resume(&ResumeCheck {
            record: &record,
            ai_scan_key: &key(),
            models: &models(),
            trees: vec![("acme/api".to_string(), tree_state(repo.path()))],
            build: &engine_pin::BuildId::current(),
            corpus_version: None,
        })
    };
    // One stray file: named.
    std::fs::write(repo.path().join("acme-stray.txt"), "x\n").unwrap();
    let one = refuse_now();
    assert!(
        matches!(&one, Err(RunStoreError::Refused(m))
            if m.contains("changed paths now") && m.contains("acme-stray.txt")
                && !m.contains("more")),
        "{one:?}"
    );
    // An edit and twenty-five strays: twenty named, then "and 6 more".
    std::fs::write(repo.path().join("a.txt"), "two\n").unwrap();
    for n in 1..=25 {
        std::fs::write(repo.path().join(format!("acme-stray-{n:02}.txt")), "x\n").unwrap();
    }
    let many = refuse_now();
    let Err(RunStoreError::Refused(message)) = &many else {
        panic!("{many:?}")
    };
    // 27 changed paths in all: the edit, 26 strays (the first one too).
    assert!(message.contains("and 7 more"), "{message}");
    assert_eq!(message.matches(".txt").count(), 20, "{message}");
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
            build: &engine_pin::BuildId::current(),
            corpus_version: None,
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
        pin: None,
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
        segments: Vec::new(),
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

    const CREDIT: &str = "Your credit balance is too low to access the Anthropic API.";

    /// A batch API that answers every item with a reply costing $0.50, except the items named in
    /// `credit_fail`, which come back as a credit error. Each submit gets its own id and the fake
    /// keeps what each one was sent.
    #[derive(Default)]
    struct Ledger {
        sent: std::sync::Mutex<Vec<Vec<String>>>,
        credit_fail: std::sync::Mutex<Vec<String>>,
        fetches: AtomicUsize,
        realtime: Option<Scripted>,
    }

    impl Ledger {
        fn failing(ids: &[&str]) -> Self {
            Self {
                credit_fail: std::sync::Mutex::new(ids.iter().map(|s| s.to_string()).collect()),
                realtime: Some(Scripted::new()),
                ..Self::default()
            }
        }
        fn restore_credit(&self) {
            self.credit_fail.lock().unwrap().clear();
        }
        fn sent(&self) -> Vec<Vec<String>> {
            self.sent.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl BatchApi for Ledger {
        fn has_api_key(&self) -> bool {
            true
        }
        fn realtime(&self) -> &dyn LlmPort {
            self.realtime.as_ref().unwrap()
        }
        async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
            None
        }
        async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
            let mut sent = self.sent.lock().unwrap();
            sent.push(items.into_iter().map(|i| i.custom_id).collect());
            Ok(format!("msgbatch_{}", sent.len()))
        }
        async fn poll(&self, _id: &str) -> anyhow::Result<BatchStatus> {
            Ok(BatchStatus {
                processing_status: "ended".to_string(),
                request_counts: BatchRequestCounts::default(),
            })
        }
        async fn fetch(&self, id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let which: usize = id.trim_start_matches("msgbatch_").parse().unwrap();
            let ids = self.sent.lock().unwrap()[which - 1].clone();
            let failing = self.credit_fail.lock().unwrap().clone();
            Ok(ids
                .into_iter()
                .map(|custom_id| {
                    let failed = failing.contains(&custom_id);
                    BatchResultRow {
                        response: (!failed).then(|| response(r#"{"findings":[]}"#, 0.5)),
                        error: failed.then(|| CREDIT.to_string()),
                        billed: None,
                        custom_id,
                    }
                })
                .collect())
        }
    }

    fn named(cid: &str) -> BatchItem {
        let mut i = item();
        i.custom_id = cid.to_string();
        i.params.messages[0].content = serde_json::json!(format!("ask {cid}"));
        i
    }

    fn three() -> Vec<BatchItem> {
        vec![named("i1"), named("i2"), named("i3")]
    }

    fn ids_of(rows: &[BatchResultRow]) -> Vec<String> {
        rows.iter().map(|r| r.custom_id.clone()).collect()
    }

    /// Attempt one: the three items go out and the second comes back as a credit error. The
    /// store is reopened (a new attempt) and returned with the fake.
    async fn after_a_credit_stop() -> (Arc<RunStore>, Arc<Ledger>, tempfile::TempDir) {
        let repo = git_repo();
        let store = new_run(&repo);
        let fake = Arc::new(Ledger::failing(&["i2"]));
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let id = api.submit(three()).await.unwrap();
        let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(rows.iter().filter(|r| r.response.is_some()).count(), 2);
        drop(api);
        (reopen(store), fake, repo)
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_resume_sends_only_the_rows_without_a_good_reply_and_pays_for_the_rest_once() {
        seeded(async {
            let (_s, _g) = state_dir();
            let (store, fake, _repo) = after_a_credit_stop().await;
            let spent_before = journal::stored_spend(store.dir());
            fake.restore_credit();
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let id = api.submit(three()).await.unwrap();
            assert_eq!(
                fake.sent(),
                vec![
                    vec!["i1".to_string(), "i2".to_string(), "i3".to_string()],
                    vec!["i2".to_string()]
                ],
                "exactly one new batch, with only the item that had no good reply"
            );
            assert_eq!(api.poll(&id).await.unwrap().processing_status, "ended");
            let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
            assert!(rows
                .iter()
                .all(|r| r.response.is_some() && r.error.is_none()));
            assert_eq!(
                fake.fetches.load(Ordering::SeqCst),
                2,
                "the stored rows were not fetched again"
            );
            // The stored spend is the two good rows of the first batch plus the one new row.
            let spent = journal::stored_spend(store.dir());
            assert!((spent_before - 1.0).abs() < 1e-9, "{spent_before}");
            assert!((spent - 1.5).abs() < 1e-9, "{spent}");
            // Fetched again, it is answered from the store, and a stage's next resume sends nothing.
            let again = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(ids_of(&again), ["i1", "i2", "i3"]);
            drop(api);
            let store = reopen(store);
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let id = api.submit(three()).await.unwrap();
            let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(fake.sent().len(), 2, "all rows are good: no inner submit");
            assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
            assert!(rows.iter().all(|r| r.response.is_some()));
            assert!((journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9);
        })
        .await;
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_resent_row_that_fails_again_stops_cleanly_and_a_third_attempt_sends_it_again() {
        seeded(async {
            let (_s, _g) = state_dir();
            let (store, fake, _repo) = after_a_credit_stop().await;
            // The credit is still out on the second attempt.
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let id = api.submit(three()).await.unwrap();
            let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
            assert!(rows[0].response.is_some() && rows[2].response.is_some());
            assert!(
                rows[1].response.is_none(),
                "the failed row is still a failure"
            );
            assert_eq!(rows[1].error.as_deref(), Some(CREDIT));
            drop(api);

            let store = reopen(store);
            fake.restore_credit();
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let id = api.submit(three()).await.unwrap();
            assert_eq!(
                fake.sent().into_iter().skip(1).collect::<Vec<_>>(),
                vec![vec!["i2".to_string()], vec!["i2".to_string()]],
                "the third attempt sends the row again, alone"
            );
            let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
            assert!(rows.iter().all(|r| r.response.is_some()));
            assert!(
                (journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9,
                "the rows with a good reply are paid once"
            );
        })
        .await;
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_batch_sent_for_failed_rows_and_not_fetched_is_not_sent_again() {
        seeded(async {
            let (_s, _g) = state_dir();
            let (store, fake, _repo) = after_a_credit_stop().await;
            fake.restore_credit();
            let first = {
                let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
                api.submit(three()).await.unwrap()
                // The process dies here: the new batch was submitted, not fetched.
            };
            let store = reopen(store);
            let api = journal::JournalBatchApi::new(fake.clone(), store);
            let second = api.submit(three()).await.unwrap();
            assert_eq!(
                second, first,
                "the batch that may be running is the one returned"
            );
            assert_eq!(fake.sent().len(), 2, "nothing was submitted a third time");
        })
        .await;
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn after_a_restart_the_resent_batch_fetched_by_id_still_returns_the_stored_good_rows() {
        seeded(async {
            let (_s, _g) = state_dir();
            let (store, fake, _repo) = after_a_credit_stop().await;
            fake.restore_credit();
            let resend = {
                let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
                api.submit(three()).await.unwrap()
            };
            // A new process: the stage polls and fetches by id and never calls submit.
            let store = reopen(store);
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            assert_eq!(api.poll(&resend).await.unwrap().processing_status, "ended");
            let rows = api.fetch(&resend, CacheTtl::FiveMinutes).await.unwrap();
            assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
            assert!(rows.iter().all(|r| r.response.is_some()));
            assert_eq!(fake.sent().len(), 2, "nothing was submitted again");
            assert!((journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9);
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
        pin: crate::run_store::engine_pin::EnginePin::for_run(None, &Default::default()),
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
        pin: crate::run_store::engine_pin::EnginePin::for_run(None, &Default::default()),
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

// ---- the saved models are the resolved ids, so a changed default is noticed on resume ----

/// Nothing picked anywhere: the record names what each stage will call (the built-in default for
/// the scan and calibration, Haiku 5.5 for the fix stage), not an empty pick.
#[test]
fn the_models_record_stores_the_resolved_ids_not_the_raw_picks() {
    let controls = crate::cost_cap::RunControls::default();
    let none = ModelsRecord::from_picks(None, None, &controls, "api", "parallel");
    assert!(none.resolved);
    assert_eq!(none.audit, crate::llm::DEFAULT_MODEL);
    // A13: stage 1 has a default of its own (Opus 5.5); the scan keeps the built-in default.
    assert_eq!(
        none.calibration,
        camerata_api_types::project::STAGE1_DEFAULT_MODEL
    );
    assert_eq!(none.fix, camerata_api_types::project::FIX_STAGE_HAIKU_MODEL);

    // Picks are kept; a blank pick is no pick; an explicit fix model wins.
    let controls = crate::cost_cap::RunControls {
        scope: Default::default(),
        fix_model: Some("claude-opus-5".to_string()),
        ..crate::cost_cap::RunControls::default()
    };
    let picked = ModelsRecord::from_picks(
        Some("claude-opus-5-5"),
        Some("  "),
        &controls,
        "cli",
        "batch",
    );
    assert_eq!(picked.audit, "claude-opus-5-5");
    assert_eq!(
        picked.calibration, "claude-opus-5-5",
        "blank is no pick: stage 1 runs on its default, which is also Opus 5.5 (A13)"
    );
    // A blank pick no longer follows the scan model.
    let other_scan = ModelsRecord::from_picks(
        Some("claude-haiku-5-5"),
        Some("  "),
        &crate::cost_cap::RunControls::default(),
        "cli",
        "batch",
    );
    assert_eq!(other_scan.audit, "claude-haiku-5-5");
    assert_eq!(other_scan.calibration, "claude-opus-5-5");
    assert_eq!(picked.fix, "claude-opus-5");
    assert_eq!(
        (picked.backend.as_str(), picked.mode.as_str()),
        ("cli", "batch")
    );
}

/// A run saved under one default is refused when the default has moved, which a raw empty pick
/// could not show.
#[test]
#[serial_test::serial(process_env)]
fn a_changed_default_is_noticed_on_resume() {
    // Its own state dir under the shared key, like every other test here that saves a run: the
    // state dir is process environment, and a sibling test moving it mid-run made this read the
    // record from the wrong folder (CI, 2026-10-30).
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    drop(store);
    let record = read_record(&id).unwrap();
    let trees = || vec![("acme/api".to_string(), tree_state(repo.path()))];
    let check = |now: &ModelsRecord| {
        check_resume(&ResumeCheck {
            record: &record,
            ai_scan_key: &key(),
            models: now,
            trees: trees(),
            build: &engine_pin::BuildId::current(),
            corpus_version: None,
        })
    };
    assert_eq!(check(&models()), Ok(()));
    let mut moved = models();
    moved.calibration = "claude-newer-default".to_string();
    assert!(matches!(
        check(&moved),
        Err(RunStoreError::Refused(m)) if m.contains("different models")
    ));
    let mut moved_fix = models();
    moved_fix.fix = "claude-newer-default".to_string();
    assert!(matches!(
        check(&moved_fix),
        Err(RunStoreError::Refused(m)) if m.contains("different models")
    ));
}

/// A run saved before ids were resolved has no `resolved` field and raw (possibly empty) picks:
/// it still reads back, and resumes on what it recorded.
#[test]
fn a_run_saved_with_raw_picks_reads_back_and_resumes() {
    let old: ModelsRecord = serde_json::from_str(
        r#"{"audit":"claude-a","calibration":"","fix":"","backend":"api","mode":"parallel"}"#,
    )
    .unwrap();
    assert!(!old.resolved, "an old record is not marked resolved");
    // Now resolved: the empty raw picks said nothing, so they do not block the resume.
    assert!(old.matches_for_resume(&models()));
    // What the old record did record still has to match.
    let mut other_audit = models();
    other_audit.audit = "claude-b".to_string();
    assert!(!old.matches_for_resume(&other_audit));
    let mut other_mode = models();
    other_mode.mode = "batch".to_string();
    assert!(!old.matches_for_resume(&other_mode));
    // An explicit raw pick is compared.
    let old_picked = ModelsRecord {
        calibration: "claude-cal".to_string(),
        ..old.clone()
    };
    let mut now = models();
    assert!(!old_picked.matches_for_resume(&now));
    now.calibration = "claude-cal".to_string();
    assert!(old_picked.matches_for_resume(&now));
}

// ---- the group reviewer (stage 2) is a part of a run ----

/// A stage 2 request: the system prompt the group reviewer sends.
fn stage2_request(prompt: &str) -> LlmRequest {
    LlmRequest::new(prompt).with_system(crate::ai_audit::group_review::group_review_system_prompt())
}

/// Answers every call with one fixed text.
struct Fixed(String, AtomicUsize);

#[async_trait::async_trait]
impl LlmPort for Fixed {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(response(&self.0, 0.05))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

const GOOD_STAGE2_REPLY: &str = r#"{"summary":"s","subgroups":[{"label":"a","cause":"b","summary":"c","members":[0]}],"outliers":[],"escalations":[],"disagreements":[]}"#;

#[test]
#[serial_test::serial(process_env)]
fn a_group_review_pass_name_maps_to_the_group_review_part() {
    assert_eq!(
        PartKind::of_pass_name("group review"),
        PartKind::GroupReview
    );
    // The whole-run part is in the plan, so a failed group review is remaining work.
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    assert!(
        store
            .record()
            .plan
            .iter()
            .any(|p| p.kind == PartKind::GroupReview && p.status == PartStatus::Pending),
        "{:?}",
        store.record().plan
    );
    store.finish(&FinishOutcome {
        scan_reached_end: true,
        complete: false,
        failures: vec![FinishFailure {
            repo: "acme/api".to_string(),
            pass: "group review".to_string(),
            class: "content".to_string(),
            files: Vec::new(),
            scope_note: false,
        }],
        incomplete_reasons: vec!["x".to_string()],
        ..FinishOutcome::default()
    });
    let record = store.record();
    let status = |kind| record.plan.iter().find(|p| p.kind == kind).unwrap().status;
    assert_eq!(status(PartKind::GroupReview), PartStatus::Failed);
    assert_eq!(
        status(PartKind::Scan),
        PartStatus::Done,
        "the scan is not re-run for it"
    );
    let remaining: Vec<PartKind> = record.remaining_parts().iter().map(|p| p.kind).collect();
    assert_eq!(
        remaining
            .iter()
            .filter(|k| **k == PartKind::GroupReview)
            .count(),
        1
    );
    assert!(!remaining.contains(&PartKind::Scan));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_good_stage2_reply_is_replayed_and_one_that_breaks_the_schema_is_asked_again() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let good = Fixed(GOOD_STAGE2_REPLY.to_string(), AtomicUsize::new(0));
        // Parses as JSON, but is not a stage 2 reply (an extra field).
        let bad = Fixed(
            r#"{"summary":"s","subgroups":[],"outliers":[],"escalations":[],"disagreements":[],"confidence":"high"}"#.to_string(),
            AtomicUsize::new(0),
        );
        {
            assert!(JournalLlm::new(&good, store.clone()).complete(stage2_request("Repository: acme/api\ngood")).await.is_ok());
            assert!(JournalLlm::new(&bad, store.clone()).complete(stage2_request("Repository: acme/api\nbad")).await.is_ok());
        }
        let store = reopen(store);
        let (g0, b0) = (good.1.load(Ordering::SeqCst), bad.1.load(Ordering::SeqCst));
        let j = JournalLlm::new(&good, store.clone());
        j.complete(stage2_request("Repository: acme/api\ngood")).await.unwrap();
        assert_eq!(good.1.load(Ordering::SeqCst), g0, "a good stage 2 reply is replayed");
        let j = JournalLlm::new(&bad, store);
        j.complete(stage2_request("Repository: acme/api\nbad")).await.unwrap();
        assert_eq!(
            bad.1.load(Ordering::SeqCst),
            b0 + 1,
            "a reply the closed schema turns down is not replayed: it would be turned down again"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn one_failed_rule_group_does_not_set_aside_every_stage2_reply_of_the_repo() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let good = Fixed(GOOD_STAGE2_REPLY.to_string(), AtomicUsize::new(0));
        JournalLlm::new(&good, store.clone())
            .complete(stage2_request("Repository: acme/api\nkept"))
            .await
            .unwrap();
        store.finish(&FinishOutcome {
            failures: vec![FinishFailure {
                repo: "acme/api".to_string(),
                pass: "group review".to_string(),
                class: "content".to_string(),
                files: Vec::new(),
                scope_note: false,
            }],
            ..FinishOutcome::default()
        });
        let store = reopen(store);
        let before = good.1.load(Ordering::SeqCst);
        JournalLlm::new(&good, store)
            .complete(stage2_request("Repository: acme/api\nkept"))
            .await
            .unwrap();
        assert_eq!(
            good.1.load(Ordering::SeqCst),
            before,
            "the rule groups that were read are not paid for again"
        );
    })
    .await;
}

// ── The model-input chokepoint and the run store ──────────────────────────────────────────────

/// An inspection request as the stages build it: its files already went through
/// `files_for_model`, so the secret appears in its redacted form. Everything the key is made of
/// is set (system prompt, breakpoints, effort, schema).
fn saved_run_shape() -> LlmRequest {
    LlmRequest::new(
        "Repository: acme/api\nREPO MAP\nsrc/billing.rs\n<<<UNTRUSTED t \"src/billing.rs\">>>\n\
         1: const KEY = \"sk_live_\u{2026}FtVR (77 chars)\";\n<<<END t>>>\nASK: review the file above.",
    )
    .with_system("You are a conformance auditor. Credential values appear as prefix\u{2026}tail.")
    .with_model("claude-sonnet-5-5")
    .with_max_tokens(8192)
    .with_cache_breakpoints([20, 60])
    .with_effort("high")
    .with_response_schema(serde_json::json!({ "type": "object" }))
}

/// SALVAGE CRITICAL. The key a saved run filed a reply under was the digest of the request as the
/// stage built it. The chokepoint leaves a request that is already redacted exactly as it was, so
/// the digest of that shape is the digest it always had: the value below was produced by
/// `request_key` BEFORE the chokepoint existed. If this changes, every saved run stops replaying.
#[test]
fn the_key_of_an_already_redacted_request_is_the_key_it_always_had() {
    assert_eq!(
        super::journal::request_key(&saved_run_shape()),
        "373ffb48ade0259251473ae32dd953c0360f259093f63b1cb32b8b3d1098ca47"
    );
}

/// Records the request it was handed.
struct Recording(std::sync::Mutex<Vec<LlmRequest>>);

#[async_trait::async_trait]
impl LlmPort for Recording {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.0.lock().unwrap().push(req);
        Ok(response(r#"{"ok":true}"#, 0.05))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The key names what the model receives. A request that still carries a secret (a stage that
/// forgot to redact) is named by the digest of its redacted form, the same digest the
/// already-redacted request has, so the two are one request to the store.
#[test]
fn the_key_is_computed_over_the_request_after_the_chokepoint() {
    let planted =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey);
    let raw = LlmRequest::new(format!("Repository: acme/api\nkey: {}\nASK", planted.text))
        .with_system(format!("sys {}", planted.text))
        .with_model("claude-sonnet-5-5");
    // Redacted upstream, as a stage that does it right builds the request.
    let by_hand = LlmRequest::new(camerata_secrets::redact_str(&raw.prompt).into_owned())
        .with_system(camerata_secrets::redact_str(raw.system.as_deref().unwrap_or("")).into_owned())
        .with_model("claude-sonnet-5-5");
    assert_ne!(raw.prompt, by_hand.prompt, "the fixture really is a secret");
    assert_eq!(
        super::journal::request_key(&raw),
        super::journal::request_key(&by_hand),
        "the key is the digest of what is sent"
    );
}

/// The journal hands the model seam the redacted request and files the reply under its key, so a
/// resume that asks the same question (with or without the secret in it) replays and pays nothing.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_journal_sends_the_redacted_request_and_a_resume_replays_it() {
    seeded(async {
        let (_s, _g) = state_dir();
        let _on =
            camerata_test_env::EnvGuard::remove(crate::feature_flags::MODEL_PROMPT_REDACTION_ENV);
        let repo = git_repo();
        let store = new_run(&repo);
        let planted =
            camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::SupabaseSecretKey);
        let raw = || {
            LlmRequest::new(format!("Repository: acme/api\nenv: {}\nASK", planted.text))
                .with_model("claude-sonnet-5-5")
        };
        let model = Recording(Default::default());
        JournalLlm::new(&model, store.clone())
            .complete(raw())
            .await
            .unwrap();
        {
            let sent = model.0.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert!(
                !sent[0].prompt.contains(&planted.value),
                "the model seam received a raw secret"
            );
        }
        let key = super::journal::request_key(&raw());
        assert!(
            store
                .dir()
                .join("calls")
                .join(format!("{key}.json"))
                .exists(),
            "the reply is filed under the key of what was sent"
        );

        let store = reopen(store);
        JournalLlm::new(&model, store)
            .complete(raw())
            .await
            .unwrap();
        assert_eq!(
            model.0.lock().unwrap().len(),
            1,
            "the resume replays: no second call"
        );
    })
    .await;
}

// ── A refused reply is read again on a resume (Z-2: a paid reply is never paid for twice because a
// parser was wrong) ──────────────────────────────────────────────────────────────────────────────

/// A stage 2 reply whose one escalation has no `rank`: the parser before ecc043b4 refused it (and
/// the run stored it as unusable); the parser now reads it.
const NO_RANK_STAGE2_REPLY: &str = r#"{"summary":"s","subgroups":[{"label":"a","cause":"b","summary":"c","members":[0,1]}],"outliers":[],"escalations":[{"index":1,"reason":"rule_misfit","why":"the rule does not fit this site"}],"disagreements":[]}"#;

/// What a CLI model says when it tries to use a tool instead of answering: no JSON in it.
const TOOL_TRANSCRIPT_REPLY: &str =
    "I will read the files first.\n<tool_use name=\"Read\">{\"path\":\"src/a.rs\"}</tool_use>";

/// Store `text` as the reply to `req` the way the journal stores one that came back unusable, in
/// the run `store`'s own folder: what a run made before a parser fix left behind.
fn store_unusable(store: &RunStore, req: &LlmRequest, text: &str, truncated: bool) -> String {
    use super::journal::{request_key, CallEntry, CallFile, EntryStatus, StoredResponse};
    let key = request_key(req);
    let (kind, repo, files) = super::journal::classify(req);
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(
        0,
        CallEntry {
            attempt: 1,
            status: EntryStatus::Unusable,
            at: "2026-11-05T16:00:00Z".to_string(),
            response: Some(StoredResponse {
                text: text.to_string(),
                model: "stub".to_string(),
                backend: "cli".to_string(),
                cost_usd: Some(0.2),
                input_tokens: Some(100),
                output_tokens: Some(10),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
                truncated,
                reported_model: None,
                cli_cost_usd: None,
                elapsed_ms: None,
                ended_unix_ms: None,
            }),
            error: None,
            class: None,
            cost_usd: 0.2,
            reason: Some(
                "the reply is not a group reviewer reply that fits the closed schema".to_string(),
            ),
            output_path: None,
        },
    );
    let file = CallFile {
        key: key.clone(),
        kind,
        repo,
        files,
        entries,
    };
    let dir = store.dir().join("calls");
    std::fs::create_dir_all(&dir).unwrap();
    write_atomic(
        &dir.join(format!("{key}.json")),
        &serde_json::to_vec(&file).unwrap(),
    )
    .unwrap();
    key
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_stage2_reply_the_current_parser_accepts_resumes_with_zero_calls() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let req = stage2_request("Repository: acme/api\nno rank");
        store_unusable(&store, &req, NO_RANK_STAGE2_REPLY, false);
        let store = reopen(store);

        // The run's own estimate no longer counts it as a call to send again.
        let estimate = super::journal::estimate_remaining(store.dir(), &store.record());
        assert_eq!(estimate.calls_to_send_again, 0, "{estimate:?}");
        assert_eq!(estimate.calls_stored, 1, "{estimate:?}");

        let model = Fixed(GOOD_STAGE2_REPLY.to_string(), AtomicUsize::new(0));
        let replayed = JournalLlm::new(&model, store.clone())
            .complete(req)
            .await
            .unwrap();
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            0,
            "the paid reply is read again, not paid for again"
        );
        assert_eq!(replayed.text, NO_RANK_STAGE2_REPLY);
        assert_eq!(
            replayed.cost_usd,
            Some(0.0),
            "a replay costs this attempt nothing"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_tool_transcript_a_cut_off_reply_and_a_refused_one_are_still_asked_again() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let transcript = stage2_request("Repository: acme/api\ntranscript");
        let cut_off = stage2_request("Repository: acme/api\ncut off");
        let refused = stage2_request("Repository: acme/api\nrefused");
        store_unusable(&store, &transcript, TOOL_TRANSCRIPT_REPLY, false);
        // JSON that would parse, but the model was cut off: never a complete reply.
        store_unusable(&store, &cut_off, NO_RANK_STAGE2_REPLY, true);
        // A field the closed schema does not have: the current parser refuses it too.
        store_unusable(
            &store,
            &refused,
            r#"{"summary":"s","subgroups":[],"outliers":[],"escalations":[],"disagreements":[],"confidence":"high"}"#,
            false,
        );
        let store = reopen(store);

        let estimate = super::journal::estimate_remaining(store.dir(), &store.record());
        assert_eq!(estimate.calls_to_send_again, 3, "{estimate:?}");

        let model = Fixed(GOOD_STAGE2_REPLY.to_string(), AtomicUsize::new(0));
        let journal = JournalLlm::new(&model, store.clone());
        for req in [transcript, cut_off, refused] {
            journal.complete(req).await.unwrap();
        }
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            3,
            "a reply the current parser still refuses is called again"
        );
    })
    .await;
}

/// A reply that is replayed is a good one from then on: the run's accounting says so (a later
/// failed pass sets it aside like any good reply, and the counts agree with the estimate).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_replayed_refused_reply_is_stored_as_a_good_one() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let req = stage2_request("Repository: acme/api\nno rank");
        let key = store_unusable(&store, &req, NO_RANK_STAGE2_REPLY, false);
        let store = reopen(store);
        let model = Fixed(GOOD_STAGE2_REPLY.to_string(), AtomicUsize::new(0));
        JournalLlm::new(&model, store.clone())
            .complete(req)
            .await
            .unwrap();
        let on_disk: super::journal::CallFile = serde_json::from_slice(
            &std::fs::read(store.dir().join("calls").join(format!("{key}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            on_disk.entries[&0].status,
            super::journal::EntryStatus::Done
        );
        assert!(
            on_disk.entries[&0].reason.is_none(),
            "{:?}",
            on_disk.entries[&0].reason
        );
    })
    .await;
}

/// Every other stage that reads a reply decides usability the same way (a JSON object that
/// parses, not cut off), so a stored unusable reply that parses now is replayed there too.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_unusable_scan_reply_that_parses_now_is_replayed_and_one_with_no_json_is_not() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let scan = |what: &str| {
            LlmRequest::new(format!("Repository: acme/api\n{what}"))
                .with_system(crate::ai_audit::audit_system_prompt_with(false))
        };
        let parses = scan("parses");
        let none = scan("none");
        store_unusable(
            &store,
            &parses,
            r#"{"findings":[],"proposed_rules":[],"needs_files":[]}"#,
            false,
        );
        store_unusable(&store, &none, "no json at all", false);
        let store = reopen(store);
        let model = Fixed("{}".to_string(), AtomicUsize::new(0));
        let journal = JournalLlm::new(&model, store);
        journal.complete(parses).await.unwrap();
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            0,
            "the reply that parses is replayed"
        );
        journal.complete(none).await.unwrap();
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            1,
            "the reply with no JSON is asked again"
        );
    })
    .await;
}

// ── The files of a run are written whole or not at all ──────────────────────────────────────────

/// Two writes to one path never share a temporary file: a shared name lets two writers (two
/// threads of this process) interleave their bytes in it, and the rename then publishes a record
/// neither of them wrote.
#[test]
fn every_atomic_write_has_its_own_temporary_file_in_the_same_folder() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("run.json");
    let a = temp_path_for(&target);
    let b = temp_path_for(&target);
    assert_ne!(a, b, "two writes, two temporary files");
    assert_eq!(
        a.parent(),
        target.parent(),
        "same folder, so the rename is atomic"
    );
    assert_eq!(b.parent(), target.parent());
}

/// Eight threads write different whole records to one path, over and over, while a reader reads
/// it. Every read, and the end state, is one writer's whole record.
#[test]
fn concurrent_writers_never_publish_a_mixed_or_cut_record() {
    let dir = tempfile::tempdir().unwrap();
    let target = std::sync::Arc::new(dir.path().join("call.json"));
    write_atomic(&target, &vec![b'0'; 300_000]).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (target, stop) = (target.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut bad = 0usize;
            while !stop.load(Ordering::SeqCst) {
                if let Ok(bytes) = std::fs::read(&*target) {
                    let first = bytes.first().copied();
                    if bytes.len() != 300_000 || bytes.iter().any(|b| Some(*b) != first) {
                        bad += 1;
                    }
                }
            }
            bad
        })
    };
    let writers: Vec<_> = (0..8u8)
        .map(|n| {
            let target = target.clone();
            std::thread::spawn(move || {
                for _ in 0..40 {
                    write_atomic(&target, &vec![b'a' + n; 300_000]).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let bad = reader.join().unwrap();
    assert_eq!(bad, 0, "a reader saw a mixed or cut record");
    let end = std::fs::read(&*target).unwrap();
    assert_eq!(end.len(), 300_000);
    assert!(
        end.iter().all(|b| *b == end[0]),
        "the end state is one writer's record"
    );
}

/// A crash between the temporary file and the rename leaves a cut temporary file beside the
/// record. The record is still the old whole one, and nothing that lists the folder takes the
/// temporary file for a record.
#[test]
#[serial_test::serial(process_env)]
fn a_cut_temporary_file_is_never_read_as_a_record() {
    use super::journal::{request_key, CallFile};
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let req = ask("a question");
    let key = request_key(&req);
    let dir = store.dir().join("calls");
    std::fs::create_dir_all(&dir).unwrap();
    let good = CallFile {
        key: key.clone(),
        kind: PartKind::Other,
        repo: String::new(),
        files: Vec::new(),
        entries: Default::default(),
    };
    write_atomic(
        &dir.join(format!("{key}.json")),
        &serde_json::to_vec(&good).unwrap(),
    )
    .unwrap();
    // The crash: a temporary file cut in the middle of its JSON, in the folder.
    let cut = temp_path_for(&dir.join(format!("{key}.json")));
    std::fs::write(
        &cut,
        br#"{"key":"zz","kind":"other","entries":{"0":{"attempt":1,"sta"#,
    )
    .unwrap();

    let again = reopen(store);
    let listed: Vec<CallFile> = std::fs::read_dir(again.dir().join("calls"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).ok())
        .collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, key);
    let estimate = super::journal::estimate_remaining(again.dir(), &again.record());
    assert_eq!(
        estimate.calls_stored + estimate.calls_to_send_again,
        0,
        "{estimate:?}"
    );
}

// ── A refused reply is replayed only if the kind's OWN parser accepts it ───────────────────────

/// A tool-call transcript that happens to hold a JSON object (the tool's input). It is a JSON
/// object that parses, and it is not a reply of any stage.
const TRANSCRIPT_WITH_OBJECT: &str =
    "I will read the file first.\n<tool_use name=\"Read\">{\"path\":\"src/a.rs\"}</tool_use>\nThen I will answer.";

/// A refusal that quotes an object.
const REFUSAL_WITH_OBJECT: &str =
    "I cannot help with that. The request {\"findings\": \"none\"} is not something I can answer.";

fn kind_request(kind: &str, what: &str) -> LlmRequest {
    let base = LlmRequest::new(format!("Repository: acme/api\n{what}"));
    match kind {
        "scan" => base.with_system(crate::ai_audit::audit_system_prompt_with(false)),
        "calibration" => base.with_system(crate::ai_audit::verify_system_prompt()),
        "alternatives" => base.with_system(crate::ai_audit::alternatives_system_prompt()),
        _ => base,
    }
}

/// What each kind's own parser accepts (the shape its stage asks for).
fn accepted_reply(kind: &str) -> &'static str {
    match kind {
        "scan" => r#"{"findings":[],"proposed_rules":[],"needs_files":[]}"#,
        "calibration" => r#"{"severity":"medium","real":"yes","reason":"checked","effort":"low"}"#,
        "alternatives" => r#"{"recommendations":[]}"#,
        _ => r#"{"anything":true}"#,
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_transcript_or_refusal_holding_an_object_is_called_again_for_every_kind() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let mut asked = Vec::new();
        for kind in ["scan", "calibration", "alternatives", "other"] {
            for (i, text) in [TRANSCRIPT_WITH_OBJECT, REFUSAL_WITH_OBJECT]
                .into_iter()
                .enumerate()
            {
                let req = kind_request(kind, &format!("{kind} {i}"));
                store_unusable(&store, &req, text, false);
                asked.push(req);
            }
        }
        let store = reopen(store);
        let model = Fixed("{}".to_string(), AtomicUsize::new(0));
        let journal = JournalLlm::new(&model, store);
        for req in asked.iter().cloned() {
            journal.complete(req).await.unwrap();
        }
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            asked.len(),
            "no transcript or refusal is replayed as a reply"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_reply_the_kinds_own_parser_accepts_is_replayed_with_zero_calls() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let mut asked = Vec::new();
        // "other" has no parser a resume can run (the clone judge and the deep lenses each read
        // their own shape, and the request that would say which is not stored): it is never
        // replayed from Unusable, so it is not in this list.
        for kind in ["scan", "calibration", "alternatives"] {
            let req = kind_request(kind, kind);
            store_unusable(&store, &req, accepted_reply(kind), false);
            asked.push(req);
        }
        let store = reopen(store);
        let model = Fixed("{}".to_string(), AtomicUsize::new(0));
        let journal = JournalLlm::new(&model, store);
        for req in asked {
            journal.complete(req).await.unwrap();
        }
        assert_eq!(model.1.load(Ordering::SeqCst), 0, "zero calls");
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_unusable_reply_of_the_other_kind_is_never_replayed() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let req = kind_request("other", "judge");
        store_unusable(&store, &req, accepted_reply("other"), false);
        let store = reopen(store);
        let model = Fixed("{}".to_string(), AtomicUsize::new(0));
        JournalLlm::new(&model, store).complete(req).await.unwrap();
        assert_eq!(model.1.load(Ordering::SeqCst), 1);
    })
    .await;
}

/// The fresh classification uses the same parsers: a transcript with an object in it is stored
/// as unusable when it comes back, not as a good reply.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_fresh_transcript_holding_an_object_is_stored_unusable() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Fixed(TRANSCRIPT_WITH_OBJECT.to_string(), AtomicUsize::new(0));
        let req = kind_request("scan", "fresh");
        JournalLlm::new(&model, store.clone())
            .complete(req.clone())
            .await
            .unwrap();
        let store = reopen(store);
        JournalLlm::new(&model, store).complete(req).await.unwrap();
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            2,
            "asked again, not replayed"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_estimate_and_the_store_name_the_whole_stored_spend() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        {
            let j = JournalLlm::new(&model, store.clone());
            j.complete(ask("calibrate this").with_system(crate::ai_audit::verify_system_prompt()))
                .await
                .unwrap();
            j.complete(ask("judge this")).await.unwrap();
        }
        let id = store.run_id();
        drop(store);
        let e = estimate_resume(&id).unwrap();
        assert!((e.stored_usd - 0.10).abs() < 1e-9, "{e:?}");
        assert!(
            e.render().contains("stored calls cost $0.10"),
            "the text a resume shows says what is already spent: {}",
            e.render()
        );
        let reopened = RunStore::open(&id).unwrap();
        assert!(
            (reopened.stored_usd() - 0.10).abs() < 1e-9,
            "the whole stored figure, which a spend limit counts up front"
        );
    })
    .await;
}

/// Answers every call with a model refusal that carries what it was billed ($0.60 on Opus 5.5).
struct BilledRefuser;

#[async_trait::async_trait]
impl LlmPort for BilledRefuser {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Err(crate::llm::refusal_error_billed(
            &serde_json::json!({
                "model": "claude-opus-5-5",
                "content": [],
                "stop_reason": "refusal",
                "usage": { "input_tokens": 100_000, "output_tokens": 10_000 }
            }),
            "claude-opus-5-5",
            "api",
            crate::llm::PriceBasis {
                batch: false,
                cache_ttl: crate::llm::CacheTtl::FiveMinutes,
            },
        )
        .expect("a refusal stop_reason is an error"))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_billed_refusal_is_in_the_stored_spend_a_resume_starts_from() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        {
            let j = JournalLlm::new(&BilledRefuser, store.clone());
            assert!(j.complete(ask("calibrate this")).await.is_err());
            assert!(j.complete(ask("judge this")).await.is_err());
        }
        let id = store.run_id();
        drop(store);
        let reopened = RunStore::open(&id).unwrap();
        assert!(
            (reopened.stored_usd() - 1.20).abs() < 1e-6,
            "two refusals billed $0.60 each, so the resumed meter starts at $1.20: {}",
            reopened.stored_usd()
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_call_of_a_finished_part_is_not_counted_as_to_send_again() {
    // The saved tutorly run: a first try of a group review call came back unusable, its item was
    // read on a later try, and the part finished. The resume said "1 to send again" for it.
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let model = Scripted::new();
        model.fail_boom.store(true, Ordering::SeqCst);
        {
            let j = JournalLlm::new(&model, store.clone());
            let _ = j
                .complete(
                    ask("Repository: acme/api\nBOOM")
                        .with_system(crate::ai_audit::verify_system_prompt()),
                )
                .await;
        }
        let id = store.run_id();
        let open_part = estimate_resume(&id).unwrap();
        assert_eq!(
            open_part.calls_to_send_again, 1,
            "while its part is open the call is owed: {open_part:?}"
        );
        store.set_part("acme/api", PartKind::Calibration, PartStatus::Done, None);
        let done_part = estimate_resume(&id).unwrap();
        assert_eq!(
            done_part.calls_to_send_again, 0,
            "a part that finished owes nothing: {done_part:?}"
        );
        assert!(
            done_part.render().contains("0 to send again"),
            "{}",
            done_part.render()
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_model_the_backend_reported_is_stored_beside_the_requested_one() {
    struct Reports;
    #[async_trait::async_trait]
    impl LlmPort for Reports {
        async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
            let mut r = response("{}", 0.05);
            r.model = req.model.clone();
            r.reported_model = Some("claude-opus-5-5".to_string());
            Ok(r)
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            self.complete(req).await
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        {
            let j = JournalLlm::new(&Reports, store.clone());
            let req = ask("one").with_model("claude-haiku-5-5".to_string());
            j.complete(req).await.unwrap();
        }
        let id = store.run_id();
        drop(store);
        let dir = runs_dir().join(&id).join("calls");
        let file = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .next()
            .expect("a stored call");
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("\"model\":\"claude-haiku-5-5\""), "{text}");
        assert!(
            text.contains("\"reported_model\":\"claude-opus-5-5\""),
            "{text}"
        );
        // And a replay hands it back.
        let again = RunStore::open(&id).unwrap();
        let j = JournalLlm::new(&Reports, again);
        let replayed = j
            .complete(ask("one").with_model("claude-haiku-5-5".to_string()))
            .await
            .unwrap();
        assert_eq!(replayed.reported_model.as_deref(), Some("claude-opus-5-5"));
    })
    .await;
}

// ---- a resumed run keeps the group review model it started on ----

fn controls_with_group_review(model: &str) -> crate::cost_cap::RunControls {
    crate::cost_cap::RunControls {
        scope: Default::default(),
        review: camerata_api_types::project::ReviewSettings {
            group_review_model: model.to_string(),
            ..camerata_api_types::project::ReviewSettings::default()
        },
        ..crate::cost_cap::RunControls::default()
    }
}

/// The record names the group reviewer's resolved id, defaults included.
#[test]
fn the_models_record_stores_the_resolved_group_review_model() {
    let started = ModelsRecord::from_picks(
        None,
        None,
        &controls_with_group_review("claude-fable-5-1"),
        "api",
        "parallel",
    );
    assert_eq!(started.group_review, "claude-fable-5-1");
    let default = ModelsRecord::from_picks(
        None,
        None,
        &crate::cost_cap::RunControls::default(),
        "api",
        "parallel",
    );
    assert_eq!(
        default.group_review,
        camerata_api_types::project::STAGE2_DEFAULT_MODEL
    );
}

/// A run started on Fable 5.1 and resumed with no model flag (the default is now Opus 5.5)
/// makes its remaining group review calls on Fable 5.1: no mixed models in one run.
#[test]
fn a_resume_with_no_model_flag_keeps_the_group_review_model_the_run_started_on() {
    let started = ModelsRecord::from_picks(
        None,
        None,
        &controls_with_group_review("claude-fable-5-1"),
        "api",
        "parallel",
    );
    let mut resumed = crate::cost_cap::RunControls::default();
    assert_eq!(
        resumed.group_review_model_for(),
        "claude-opus-5-5",
        "the default has moved on"
    );
    let notice = started
        .hold_group_review_model(&mut resumed, None)
        .expect("no explicit pick, nothing to refuse");
    assert_eq!(resumed.group_review_model_for(), "claude-fable-5-1");
    assert_eq!(
        notice,
        "Group review stays on claude-fable-5-1, the model this run started with."
    );
    // The same model named explicitly is not a different pick.
    let mut same = crate::cost_cap::RunControls::default();
    started
        .hold_group_review_model(&mut same, Some("claude-fable-5-1"))
        .unwrap();
    // A different one is refused, and the controls are left as they were.
    let mut other = crate::cost_cap::RunControls::default();
    let why = started
        .hold_group_review_model(&mut other, Some("claude-opus-5-5"))
        .expect_err("a different explicit model is refused");
    assert!(
        why.contains("claude-fable-5-1") && why.contains("claude-opus-5-5"),
        "{why}"
    );
    assert_eq!(other.group_review_model_for(), "claude-opus-5-5");
}

/// A run saved before the group reviewer was recorded has nothing to keep: the current pick stands.
#[test]
fn a_record_without_a_group_review_model_leaves_the_pick_alone() {
    let mut old = models();
    old.group_review = String::new();
    let mut resumed = controls_with_group_review("claude-opus-5-5");
    let notice = old.hold_group_review_model(&mut resumed, None).unwrap();
    assert_eq!(resumed.group_review_model_for(), "claude-opus-5-5");
    assert!(notice.contains("claude-opus-5-5"), "{notice}");
}

/// A moved built-in default is kept, not refused: a resolved record whose only difference is the
/// group review model still allows the resume.
#[test]
fn a_resume_is_not_refused_because_the_group_review_default_moved() {
    let mut saved = models();
    saved.group_review = "claude-fable-5-1".to_string();
    let mut now = models();
    now.group_review = "claude-fable-5-2".to_string();
    assert!(saved.matches_for_resume(&now));
    now.audit = "some-other-model".to_string();
    assert!(
        !saved.matches_for_resume(&now),
        "the other models still count"
    );
}

// ---- a run saved before the group review model was recorded ----

/// Answers every stage 2 call with a good reply, from the model the request named (as the
/// backends do), so a stored reply says which model gave it.
struct AnswersAsAsked;

#[async_trait::async_trait]
impl LlmPort for AnswersAsAsked {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let mut r = response(GOOD_STAGE2_REPLY, 0.05);
        r.model = req.model.clone();
        Ok(r)
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A record that names no group review model takes it from the group review replies the run
/// already stored (their key includes the model), so the resume reuses them and pays nothing
/// twice; the resume says which model it stays on.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_record_without_a_group_review_model_takes_it_from_its_stored_replies() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        assert!(store.record().models.group_review.is_empty());
        {
            let journal = JournalLlm::new(&AnswersAsAsked, store.clone());
            journal
                .complete(stage2_request("review one").with_model("claude-fable-5-1"))
                .await
                .unwrap();
        }
        let store = reopen(store);
        let mut resumed = crate::cost_cap::RunControls::default();
        let notice = store
            .hold_group_review_model(&mut resumed, None)
            .expect("no explicit pick");
        assert_eq!(resumed.group_review_model_for(), "claude-fable-5-1");
        assert_eq!(
            notice,
            "Group review stays on claude-fable-5-1, the model this run started with."
        );
        // The stored reply is found under the controls' model, so the resume replays it.
        let model = Fixed(GOOD_STAGE2_REPLY.to_string(), AtomicUsize::new(0));
        let journal = JournalLlm::new(&model, store.clone());
        journal
            .complete(stage2_request("review one").with_model(resumed.group_review_model_for()))
            .await
            .unwrap();
        assert_eq!(
            model.1.load(Ordering::SeqCst),
            0,
            "the stored reply was reused"
        );
        // An explicit different model is refused for such a run too.
        let mut other = crate::cost_cap::RunControls::default();
        assert!(store
            .hold_group_review_model(&mut other, Some("claude-opus-5-5"))
            .is_err());
    })
    .await;
}

/// With no recorded model and no stored group review reply, the resume says which model it will
/// use.
#[test]
#[serial_test::serial(process_env)]
fn a_record_with_no_model_and_no_stored_replies_says_the_model_it_will_use() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let mut resumed = controls_with_group_review("claude-opus-5-5");
    let notice = store.hold_group_review_model(&mut resumed, None).unwrap();
    assert_eq!(resumed.group_review_model_for(), "claude-opus-5-5");
    assert!(
        notice.contains("claude-opus-5-5") && notice.contains("did not record"),
        "{notice}"
    );
}

// ---- a run keeps its finder for life ----

fn controls_with_finder(
    finder: crate::ai_audit::agent_finder::FinderChoice,
) -> crate::cost_cap::RunControls {
    crate::cost_cap::RunControls {
        scope: Default::default(),
        finder,
        ..crate::cost_cap::RunControls::default()
    }
}

#[test]
fn a_run_records_the_finder_its_controls_chose() {
    use crate::ai_audit::agent_finder::FinderChoice;
    for (choice, word) in [
        (FinderChoice::Agent, "agent"),
        (FinderChoice::Engine, "engine"),
    ] {
        let rec =
            ModelsRecord::from_picks(None, None, &controls_with_finder(choice), "cli", "parallel");
        assert_eq!(rec.finder, word);
    }
}

#[test]
fn the_finder_hold_keeps_the_recorded_finder_and_defaults_to_the_engine() {
    use crate::ai_audit::agent_finder::FinderChoice;
    let record = |word: &str| ModelsRecord {
        finder: word.to_string(),
        ..models()
    };
    // The project now says agent; the run started on the engine, or recorded nothing (an older run).
    for recorded in ["engine", ""] {
        let mut c = controls_with_finder(FinderChoice::Agent);
        let note = record(recorded).hold_finder(&mut c);
        assert_eq!(c.finder, FinderChoice::Engine, "recorded {recorded:?}");
        assert_eq!(
            note.as_deref(),
            Some("The finder stays on engine, the one this run started with.")
        );
    }
    // The project now says engine; the run started on the agent.
    let mut c = controls_with_finder(FinderChoice::Engine);
    let note = record("agent").hold_finder(&mut c);
    assert_eq!(c.finder, FinderChoice::Agent);
    assert_eq!(
        note.as_deref(),
        Some("The finder stays on agent, the one this run started with.")
    );
    // Nothing differs: no note.
    let mut c = controls_with_finder(FinderChoice::Agent);
    assert_eq!(record("agent").hold_finder(&mut c), None);
    assert_eq!(c.finder, FinderChoice::Agent);
}

// ---- a resumed agent run keeps its finder model ----

/// Outside review round 1: a run started on one finder model finishes on it after the built-in
/// default moves, and says so; a different explicit pick is refused; an engine run has none.
#[test]
fn the_finder_model_hold_keeps_the_recorded_model_and_says_so() {
    use crate::ai_audit::agent_finder::FinderChoice;
    let record = |model: &str| ModelsRecord {
        finder: "agent".to_string(),
        finder_model: model.to_string(),
        ..models()
    };
    // Recorded from the picks: the resolved model of an agent run, none for an engine run.
    let agent = controls_with_finder(FinderChoice::Agent);
    let rec = ModelsRecord::from_picks(None, None, &agent, "api", "parallel");
    assert_eq!(rec.finder_model, agent.finder_model_for());
    let engine = controls_with_finder(FinderChoice::Engine);
    assert_eq!(
        ModelsRecord::from_picks(None, None, &engine, "api", "parallel").finder_model,
        ""
    );
    // The default is Opus now; the run started on Fable 5.2: it stays on Fable 5.2.
    let mut c = controls_with_finder(FinderChoice::Agent);
    let line = record("claude-fable-5-2")
        .hold_finder_model(&mut c)
        .unwrap();
    assert_eq!(c.finder_model_for(), "claude-fable-5-2");
    assert_eq!(
        line.as_deref(),
        Some("The finder stays on claude-fable-5-2, the model this run started with.")
    );
    // The same explicit pick is fine; a different one is refused.
    let mut same = controls_with_finder(FinderChoice::Agent);
    same.finder_model = Some("claude-fable-5-2".to_string());
    assert!(record("claude-fable-5-2")
        .hold_finder_model(&mut same)
        .is_ok());
    let mut other = controls_with_finder(FinderChoice::Agent);
    other.finder_model = Some("claude-opus-5-5".to_string());
    let why = record("claude-fable-5-2")
        .hold_finder_model(&mut other)
        .unwrap_err();
    assert!(
        why.contains("claude-fable-5-2") && why.contains("claude-opus-5-5"),
        "{why}"
    );
    // An older run recorded none and the resume names none: it keeps Fable 5.2, the default
    // when it started, and the line says so.
    let mut old = controls_with_finder(FinderChoice::Agent);
    let line = record("").hold_finder_model(&mut old).unwrap().unwrap();
    assert!(
        line.contains("claude-fable-5-2") && line.contains("recorded none"),
        "{line}"
    );
    assert_eq!(old.finder_model_for(), "claude-fable-5-2");
    // With an explicit pick on such a run, the pick stands.
    let mut picked = controls_with_finder(FinderChoice::Agent);
    picked.finder_model = Some("claude-opus-5-5".to_string());
    let line = record("").hold_finder_model(&mut picked).unwrap().unwrap();
    assert!(line.contains("did not record a finder model"), "{line}");
    assert_eq!(picked.finder_model_for(), "claude-opus-5-5");
    // An engine run has no finder model to hold.
    let mut e = controls_with_finder(FinderChoice::Engine);
    assert_eq!(
        record("claude-fable-5-2").hold_finder_model(&mut e),
        Ok(None)
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_finished_session_leaves_one_segment_and_a_resume_appends_a_second() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    assert_eq!(
        read_record(&id).unwrap().segments.len(),
        1,
        "the session's segment is on disk from the start"
    );
    store.finish(&FinishOutcome::default());
    let first = read_record(&id).unwrap().segments;
    assert_eq!(first.len(), 1, "finish closes the session's segment");
    assert!(first[0].finished_unix_ms >= first[0].started_unix_ms);
    assert!(first[0].started_unix_ms > 0);

    // The process ends; a resume opens the run again and finishes it.
    let resumed = reopen(store);
    let opened = read_record(&id).unwrap().segments;
    assert_eq!(
        opened.len(),
        2,
        "the resume's own segment is on disk at once"
    );
    assert_eq!(
        opened[0], first[0],
        "opening a resume changes no earlier segment"
    );
    resumed.finish(&FinishOutcome::default());
    let both = read_record(&id).unwrap().segments;
    assert_eq!(both.len(), 2, "the resume appended its own segment");
    assert_eq!(both[0], first[0], "the first segment is never overwritten");
    assert!(both[1].started_unix_ms >= first[0].finished_unix_ms);
}

#[test]
#[serial_test::serial(process_env)]
fn a_session_that_never_reaches_finish_still_records_its_segment_end() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    // A failed or cancelled job drops the store without `finish`.
    drop(store);
    let segments = read_record(&id).unwrap().segments;
    assert_eq!(segments.len(), 1, "the drop closed the open segment");
    assert!(segments[0].finished_unix_ms >= segments[0].started_unix_ms);
}

#[test]
#[serial_test::serial(process_env)]
fn a_segment_is_closed_once_whichever_comes_first() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    store.end_segment();
    store.end_segment();
    store.finish(&FinishOutcome::default());
    drop(store);
    assert_eq!(read_record(&id).unwrap().segments.len(), 1);
}

#[test]
#[serial_test::serial(process_env)]
fn a_run_saved_before_segments_existed_still_loads() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    store.finish(&FinishOutcome::default());
    let path = runs_dir().join(&id).join("run.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(value.as_object_mut().unwrap().remove("segments").is_some());
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(read_record(&id).unwrap().segments.is_empty());
}

#[test]
#[serial_test::serial(process_env)]
fn a_killed_session_leaves_a_segment_with_the_end_of_its_last_save() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    let dir = store.dir().to_path_buf();
    let at_create = read_record(&id).unwrap().segments[0].elapsed_ms;
    std::thread::sleep(std::time::Duration::from_millis(30));
    store.persist().unwrap();
    // What a kill leaves behind is run.json as the last save wrote it: no destructor runs.
    let left_by_the_kill = std::fs::read(dir.join("run.json")).unwrap();
    let killed = read_record(&id).unwrap().segments;
    assert_eq!(killed.len(), 1, "{killed:?}");
    assert!(
        killed[0].elapsed_ms >= at_create + 30,
        "the save moved the end: {at_create} then {killed:?}"
    );
    assert_eq!(
        killed[0].finished_unix_ms,
        killed[0].started_unix_ms + killed[0].elapsed_ms,
        "the end is the last save's"
    );

    // In one process the only way to release the lock is to drop the holder (which would close
    // the segment), so put the killed process's file and its dead lock back, then resume.
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
    std::fs::write(dir.join("run.json"), left_by_the_kill).unwrap();
    let stale = serde_json::json!({"pid": dead, "started_at": now(), "heartbeat_at": now()});
    std::fs::write(dir.join("lock.json"), stale.to_string()).unwrap();
    let resumed = RunStore::open(&id).unwrap();
    let two = read_record(&id).unwrap().segments;
    assert_eq!(two.len(), 2, "{two:?}");
    assert_eq!(
        two[0], killed[0],
        "the killed session's segment is kept as saved"
    );
    drop(resumed);
}

#[test]
#[serial_test::serial(process_env)]
fn a_session_that_did_no_work_can_take_its_segment_back_and_earlier_ones_stay() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    store.finish(&FinishOutcome::default());
    let first = read_record(&id).unwrap().segments;
    let refused = reopen(store);
    assert_eq!(read_record(&id).unwrap().segments.len(), 2);
    refused.discard_segment();
    drop(refused);
    assert_eq!(read_record(&id).unwrap().segments, first);
}

#[test]
fn the_saved_journal_keeps_a_calls_timing_and_an_older_line_still_loads() {
    let mut timed = response("answer", 0.1);
    timed.elapsed_ms = Some(1_234);
    let stored = journal::StoredResponse::of(&timed);
    assert_eq!(stored.elapsed_ms, Some(1_234));
    assert!(stored.ended_unix_ms.is_some_and(|t| t > 0));
    let back: journal::StoredResponse =
        serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
    assert_eq!(back.elapsed_ms, Some(1_234), "the timing survives the save");
    // A replay adds nothing to the live telemetry of the resuming session.
    assert_eq!(back.replayed().elapsed_ms, None);

    // A journal line written before the fields existed.
    let mut value = serde_json::to_value(&stored).unwrap();
    let object = value.as_object_mut().unwrap();
    assert!(object.remove("elapsed_ms").is_some());
    assert!(object.remove("ended_unix_ms").is_some());
    let old: journal::StoredResponse = serde_json::from_value(value).expect("an old line loads");
    assert_eq!((old.elapsed_ms, old.ended_unix_ms), (None, None));
}

// ── The engine pin (V1 item 39) ──────────────────────────────────────────────────────────────

fn build_with(commit: Option<&str>, dirty: bool) -> engine_pin::BuildId {
    engine_pin::BuildId {
        commit: commit.map(str::to_string),
        dirty,
        ..engine_pin::BuildId::clean_test_build()
    }
}

/// A saved run on the test build, and the result of resuming it on `now` with `corpus` loaded.
fn resume_on(
    record: &RunRecord,
    now: &engine_pin::BuildId,
    corpus: Option<&str>,
) -> Result<(), RunStoreError> {
    check_resume(&ResumeCheck {
        record,
        ai_scan_key: &record.ai_scan_key,
        models: &record.models,
        trees: record
            .repos
            .iter()
            .map(|r| (r.name.clone(), tree_state(std::path::Path::new(&r.dir))))
            .collect(),
        build: now,
        corpus_version: corpus,
    })
}

fn pinned_record() -> (
    tempfile::TempDir,
    camerata_test_env::EnvGuard,
    tempfile::TempDir,
    RunRecord,
) {
    let (s, g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    drop(store);
    (s, g, repo, read_record(&id).unwrap())
}

#[test]
#[serial_test::serial(process_env)]
fn the_run_saves_the_build_it_started_on() {
    let (_s, _g, _repo, record) = pinned_record();
    let pin = record.pin.expect("a new run records its pin");
    assert_eq!(pin.build, engine_pin::BuildId::current());
    assert!(pin.build.commit.is_some());
}

#[test]
#[serial_test::serial(process_env)]
fn a_resume_on_the_same_build_still_resumes() {
    let (_s, _g, _repo, record) = pinned_record();
    let pin = record.pin.clone().unwrap();
    assert_eq!(
        resume_on(&record, &pin.build, Some(&pin.corpus_version)),
        Ok(())
    );
    assert_eq!(resume_on(&record, &pin.build, None), Ok(()));
}

#[test]
#[serial_test::serial(process_env)]
fn a_resume_on_another_commit_is_refused_naming_both_builds() {
    let (_s, _g, _repo, record) = pinned_record();
    let other = build_with(Some(&"b".repeat(40)), false);
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &other, None) else {
        panic!("a different commit must refuse");
    };
    assert!(m.contains("must finish it"), "{m}");
    assert!(
        m.contains(&"a".repeat(12)) && m.contains(&"b".repeat(12)),
        "{m}"
    );
    assert!(
        !m.contains(&"a".repeat(13)),
        "twelve characters, not the whole hash: {m}"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_run_saved_without_a_pin_cannot_be_resumed() {
    let (_s, _g, _repo, mut record) = pinned_record();
    record.pin = None;
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &engine_pin::BuildId::current(), None)
    else {
        panic!("no pin must refuse");
    };
    assert!(
        m.contains("before Camerata recorded which build ran it"),
        "{m}"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_build_with_no_commit_cannot_continue_a_saved_inspection() {
    let (_s, _g, _repo, record) = pinned_record();
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &build_with(None, false), None) else {
        panic!("no commit must refuse");
    };
    assert!(m.contains("does not know which build it is"), "{m}");
    assert!(m.contains(&"a".repeat(12)), "{m}");
}

#[test]
#[serial_test::serial(process_env)]
fn a_changed_rule_library_or_prompt_schema_refuses_a_resume() {
    let (_s, _g, _repo, record) = pinned_record();
    let now = engine_pin::BuildId::current();
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &now, Some("sha256:other")) else {
        panic!("a different corpus must refuse");
    };
    assert!(m.contains("rule library changed"), "{m}");

    let mut newer = now.clone();
    newer.prompt_schema += 1;
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &newer, None) else {
        panic!("a different prompt schema must refuse");
    };
    assert!(
        m.contains("questions Camerata asks the model changed"),
        "{m}"
    );
    let mut newer = now;
    newer.clone_prompt_schema += 1;
    assert!(matches!(
        resume_on(&record, &newer, None),
        Err(RunStoreError::Refused(m)) if m.contains("questions Camerata asks")
    ));
}

#[test]
#[serial_test::serial(process_env)]
fn a_clean_inspection_is_not_continued_on_a_dirty_build_of_the_same_commit() {
    let (_s, _g, _repo, record) = pinned_record();
    let dirty = build_with(Some(&"a".repeat(40)), true);
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &dirty, None) else {
        panic!("a clean run on a dirty build must refuse");
    };
    assert!(m.contains("started on a clean build"), "{m}");
    assert!(m.contains(&"a".repeat(12)), "{m}");
}

#[test]
#[serial_test::serial(process_env)]
fn a_dirty_inspection_resumes_on_a_dirty_build_of_the_same_commit_and_says_so() {
    let (_s, _g, repo, mut record) = pinned_record();
    record.pin.as_mut().unwrap().build.dirty = true;
    let dirty = build_with(Some(&"a".repeat(40)), true);
    assert_eq!(resume_on(&record, &dirty, None), Ok(()));
    let note = engine_pin::dirty_note(record.pin.as_ref(), &dirty).expect("a note");
    assert!(note.contains("uncommitted changes"), "{note}");
    // A clean saved build gets no note (it is refused on a dirty one instead).
    let mut clean = record.clone();
    clean.pin.as_mut().unwrap().build.dirty = false;
    assert_eq!(engine_pin::dirty_note(clean.pin.as_ref(), &dirty), None);
    // Through the one implementation of resume: the note lands on the run.
    let mut saved = read_record(&record.run_id).unwrap();
    saved.pin.as_mut().unwrap().build.dirty = true;
    std::fs::write(
        runs_dir().join(&record.run_id).join("run.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    let pin = engine_pin::EnginePin {
        build: dirty,
        ..record.pin.clone().unwrap()
    };
    let sources = vec![("acme/api".to_string(), repo.path().to_path_buf())];
    let (store, _) = open_for_resume(&record.run_id, &sources, &key(), &models(), &pin).unwrap();
    assert!(store
        .record()
        .notes
        .iter()
        .any(|n| n.contains("uncommitted changes")));
}

#[test]
#[serial_test::serial(process_env)]
fn a_saved_run_with_no_commit_says_to_start_again_not_to_open_an_unknown_build() {
    let (_s, _g, _repo, mut record) = pinned_record();
    record.pin.as_mut().unwrap().build.commit = None;
    let Err(RunStoreError::Refused(m)) = resume_on(&record, &engine_pin::BuildId::current(), None)
    else {
        panic!("a saved run with no commit must refuse");
    };
    assert!(m.contains("did not record its commit"), "{m}");
    assert!(m.contains("Start it again"), "{m}");
    assert!(!m.contains("(unknown)"), "{m}");
}

#[test]
#[serial_test::serial(process_env)]
fn open_for_resume_refuses_another_build_before_opening_the_run() {
    let (_s, _g, repo, record) = pinned_record();
    let pin = engine_pin::EnginePin {
        build: build_with(Some(&"c".repeat(40)), false),
        ..record.pin.clone().unwrap()
    };
    let sources = vec![("acme/api".to_string(), repo.path().to_path_buf())];
    let result = open_for_resume(&record.run_id, &sources, &key(), &models(), &pin);
    assert!(
        matches!(&result, Err(RunStoreError::Refused(m)) if m.contains("must finish it")),
        "{:?}",
        result.err()
    );
    // The preview runs the build checks too, on the running build.
    let mut stored = read_record(&record.run_id).unwrap();
    stored.pin.as_mut().unwrap().build.commit = Some("d".repeat(40));
    std::fs::write(
        runs_dir().join(&record.run_id).join("run.json"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let preview = preview_resume(&record.run_id).unwrap();
    assert!(preview
        .refusal
        .is_some_and(|m| m.contains("must finish it")));
}

#[test]
fn a_changed_clone_judge_pick_is_put_back_to_the_saved_one_with_a_note() {
    let pin = engine_pin::EnginePin {
        build: engine_pin::BuildId::clean_test_build(),
        corpus_version: String::new(),
        clone_judge_model: "judge-a".to_string(),
    };
    let mut none = crate::cost_cap::RunControls::default();
    assert!(pin.hold_clone_judge_model(&mut none).is_some());
    assert_eq!(none.clone_judge_model.as_deref(), Some("judge-a"));
    let mut same = crate::cost_cap::RunControls {
        clone_judge_model: Some("judge-a".to_string()),
        ..Default::default()
    };
    assert_eq!(pin.hold_clone_judge_model(&mut same), None);
    let mut other = crate::cost_cap::RunControls {
        clone_judge_model: Some("judge-b".to_string()),
        ..Default::default()
    };
    let note = pin.hold_clone_judge_model(&mut other).expect("a note");
    assert!(
        note.contains("judge-a") && note.contains("judge-b"),
        "{note}"
    );
    assert_eq!(other.clone_judge_model.as_deref(), Some("judge-a"));
}

#[test]
fn a_client_inspection_does_not_start_on_a_dirty_or_commitless_build() {
    let clean = build_with(Some(&"a".repeat(40)), false);
    assert_eq!(engine_pin::client_start_refusal(true, &clean), None);
    let dirty = engine_pin::client_start_refusal(true, &build_with(Some(&"a".repeat(40)), true))
        .expect("dirty refuses");
    assert!(dirty.contains("changes that are not committed"), "{dirty}");
    let none = engine_pin::client_start_refusal(true, &build_with(None, false))
        .expect("no commit refuses");
    assert!(none.contains("does not know which build it is"), "{none}");
    // Internal runs still start on any build.
    assert_eq!(
        engine_pin::client_start_refusal(false, &build_with(Some(&"a".repeat(40)), true)),
        None
    );
    assert_eq!(
        engine_pin::client_start_refusal(false, &build_with(None, false)),
        None
    );
}
