//! Retry fixes F3, item 8: on a resume, a reply whose every entry the fix or detail stage turned
//! down is not asked again identically. The stored reply is replayed, the stage turns it down
//! again for the same reason, and its next round sends the changed request the live run sent
//! (the feedback line, the smaller batch), which is stored too. Fake model only; no live calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::detail_regen::regenerate_full_details;
use super::reply_schemas::check::{address_by_id, prompt_ids};
use super::structured_output_tests::{files, finding, response};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::run_store::journal::{CallFile, EntryStatus, JournalLlm};
use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().unwrap();
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([5u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

fn new_run(repo_dir: &std::path::Path) -> Arc<RunStore> {
    RunStore::create(NewRun {
        project_key: "proj-1".to_string(),
        repos: vec![("o/r".to_string(), repo_dir.to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: AUDIT_PROMPT_SCHEMA_VERSION,
            directives_fingerprint: "fp".to_string(),
            terse_output: false,
            applies_to_routing: false,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

fn reopen(store: Arc<RunStore>) -> Arc<RunStore> {
    let id = store.run_id();
    drop(store);
    RunStore::open(&id).unwrap()
}

fn call_files(store: &RunStore) -> Vec<CallFile> {
    std::fs::read_dir(store.dir().join("calls"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).ok())
        .collect()
}

/// Answers a fix prompt that carries a rejection note with a good fix, any other with a fix
/// naming something that is nowhere in the repository (turned down). `always_invents` turns the
/// good answer off.
struct FixModel {
    always_invents: bool,
    calls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
}

impl FixModel {
    fn new(always_invents: bool) -> Self {
        Self {
            always_invents,
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for FixModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(req.prompt.clone());
        let fix = if req.prompt.contains("was rejected") && !self.always_invents {
            "Validate the input before it reaches the query."
        } else {
            "Call `neverInEvidence` first."
        };
        let text = serde_json::json!({"fixes": [{"index": 0, "fix": fix}]}).to_string();
        Ok(response(
            &address_by_id(&req.prompt, &text),
            "api",
            "claude-sonnet-5-5",
        ))
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

fn one_finding() -> Vec<Finding> {
    vec![finding(
        "src/work.ts",
        2,
        "`runWork` passes `raw` straight to `doWork`",
        "doWork(raw)",
    )]
}

/// A wholly rejected fix reply is replayed, not asked again: the resume sends nothing the live
/// run did not send, and gets the fix the live run's changed request earned.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_does_not_ask_a_wholly_rejected_fix_reply_again() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let live = FixModel::new(false);
        {
            let journal = JournalLlm::new(&live, store.clone());
            let (out, failed) =
                generate_fix_specifics(&journal, "o/r", one_finding(), &files(), None, None, None)
                    .await;
            assert!(failed.is_empty(), "{failed:?}");
            assert!(out[0].fix_specific.is_some());
        }
        assert_eq!(live.calls(), 2, "round one rejected, the feedback round good");
        let stored = call_files(&store);
        assert!(stored
            .iter()
            .any(|f| f.entries[&0].status == EntryStatus::Rejected));

        let store = reopen(store);
        let resumed = FixModel::new(false);
        let journal = JournalLlm::new(&resumed, store.clone());
        let (out, failed) =
            generate_fix_specifics(&journal, "o/r", one_finding(), &files(), None, None, None)
                .await;
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            out[0].fix_specific.as_deref(),
            Some("Validate the input before it reaches the query.")
        );
        assert_eq!(
            resumed.calls(),
            0,
            "the rejected reply is replayed and the feedback round was stored: nothing is paid again"
        );
    })
    .await;
}

/// A give-up (every round rejected) is replayed the same way: a resume sends no request again,
/// identical or changed, and the part is still a give-up.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_of_a_give_up_sends_no_identical_request() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let live = FixModel::new(true);
        {
            let journal = JournalLlm::new(&live, store.clone());
            let (_out, failed) =
                generate_fix_specifics(&journal, "o/r", one_finding(), &files(), None, None, None)
                    .await;
            assert_eq!(failed.len(), 1);
        }
        let sent = live.calls();
        assert_eq!(sent, MAX_FIX_REGENERATIONS + 2, "the rounds and the solo");

        let store = reopen(store);
        let resumed = FixModel::new(true);
        let journal = JournalLlm::new(&resumed, store.clone());
        let (_out, failed) =
            generate_fix_specifics(&journal, "o/r", one_finding(), &files(), None, None, None)
                .await;
        assert_eq!(failed.len(), 1, "the give-up stands");
        assert_eq!(resumed.calls(), 0, "nothing is sent again");
    })
    .await;
}

/// Answers a detail prompt with entries that keep the statement's named token only when the
/// batch is a single finding, so the full-batch reply is turned down whole and the smaller
/// rounds earn the explanation.
struct DetailModel {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for DetailModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let ids = prompt_ids(&req.prompt);
        let why = if ids.len() > 1 {
            "The handler skips the check."
        } else {
            "The handler skips the check on run_work."
        };
        let entries: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({"id": id, "why": why, "fix_direction": "Add the check."}))
            .collect();
        Ok(response(
            &serde_json::json!({ "details": entries }).to_string(),
            "api",
            "claude-sonnet-5-5",
        ))
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

fn two_statements() -> Vec<Finding> {
    ["src/work.ts"; 2]
        .iter()
        .enumerate()
        .map(|(i, path)| {
            finding(
                path,
                i + 1,
                &format!("Skips the check{HEADLINE_SEP}Calls run_work with no check."),
                "doWork(raw)",
            )
        })
        .collect()
}

/// The detail stage: the full-batch reply was turned down whole, the half-size round earned the
/// explanations; a resume replays both and asks nothing.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_does_not_ask_a_wholly_rejected_detail_reply_again() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let nonce = crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef");
        let live = DetailModel {
            calls: AtomicUsize::new(0),
        };
        {
            let journal = JournalLlm::new(&live, store.clone());
            let (out, failed) = regenerate_full_details(
                &journal,
                "o/r",
                two_statements(),
                &files(),
                &[],
                &nonce,
                None,
                None,
            )
            .await;
            assert!(failed.is_empty(), "{failed:?}");
            assert!(out
                .iter()
                .all(|f| f.detail.contains("skips the check on run_work")));
        }
        assert!(live.calls.load(Ordering::SeqCst) >= 2);
        assert!(call_files(&store)
            .iter()
            .any(|f| f.entries[&0].status == EntryStatus::Rejected));

        let store = reopen(store);
        let resumed = DetailModel {
            calls: AtomicUsize::new(0),
        };
        let journal = JournalLlm::new(&resumed, store.clone());
        let (out, failed) = regenerate_full_details(
            &journal,
            "o/r",
            two_statements(),
            &files(),
            &[],
            &nonce,
            None,
            None,
        )
        .await;
        assert!(failed.is_empty(), "{failed:?}");
        assert!(out
            .iter()
            .all(|f| f.detail.contains("skips the check on run_work")));
        assert_eq!(
            resumed.calls.load(Ordering::SeqCst),
            0,
            "nothing is paid again"
        );
    })
    .await;
}

/// The stages that re-ask a rejected reply as a whole (calibration, scan, alternatives) keep
/// their policy: `quarantine_rejected` sets those aside and a resume asks them again, as
/// `run_store::tests` pins. This file only widens it for the two stages whose own rounds change
/// the request.
#[test]
fn only_the_fix_and_detail_stages_replay_what_they_turned_down() {
    use crate::run_store::journal::replays_on_resume;
    use crate::run_store::PartKind;
    for kind in [PartKind::Fix, PartKind::Detail] {
        assert!(replays_on_resume(EntryStatus::Done, kind));
        assert!(replays_on_resume(EntryStatus::Rejected, kind));
        assert!(replays_on_resume(EntryStatus::Unusable, kind));
        assert!(!replays_on_resume(EntryStatus::Failed, kind));
    }
    for kind in [
        PartKind::Scan,
        PartKind::Calibration,
        PartKind::Alternatives,
        PartKind::Other,
    ] {
        assert!(replays_on_resume(EntryStatus::Done, kind));
        assert!(!replays_on_resume(EntryStatus::Rejected, kind));
        assert!(!replays_on_resume(EntryStatus::Unusable, kind));
        assert!(!replays_on_resume(EntryStatus::Failed, kind));
    }
}
