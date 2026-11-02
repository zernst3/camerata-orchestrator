//! A resume of a fix or detail part that failed on content asks the failing findings again.
//!
//! Retry fixes F3 item 8 replayed a reply every entry of which the stage had turned down, on the
//! idea that the stage's next round changes the request. That holds for a round that is followed
//! by another; it does not for the last one, and a resume of the hearth run printed "0 to send
//! again" and failed identically (R1-1 d). A wholly rejected reply is now asked again (and counts
//! as "to send again"); a reply only PART of which was turned down still replays, so the accepted
//! entries cost nothing, and the retry for the rest names the reason, so it is a changed request.
//! Fake model only; no live calls.

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

/// How many stored replies were turned down whole (and so are asked again on a resume).
fn rejected_entries(store: &RunStore) -> usize {
    call_files(store)
        .iter()
        .flat_map(|f| f.entries.values())
        .filter(|e| e.status == EntryStatus::Rejected)
        .count()
}

/// The blocks of a fix or detail prompt: `(id, line, block text)` for each finding, from the
/// `[id] rule ...` header to the next one.
fn blocks_of(prompt: &str) -> Vec<(String, usize, String)> {
    let mut out: Vec<(String, usize, String)> = Vec::new();
    for l in prompt.lines() {
        if l.starts_with('[') && l.contains("] rule ") {
            let id = l[1..l.find(']').unwrap()].to_string();
            out.push((id, 0, String::new()));
        }
        if let Some((_, line, text)) = out.last_mut() {
            if let Some(place) = l.strip_prefix("Location: ") {
                *line = place.rsplit_once(':').unwrap().1.parse().unwrap();
            }
            text.push_str(l);
            text.push('\n');
        }
    }
    out
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

/// A wholly rejected fix reply is asked again on a resume (R1-1 d); the live run's good feedback
/// round was stored and is replayed, so only the rejected request is paid for again and the fix
/// the live run's changed request earned comes out the same.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_asks_a_wholly_rejected_fix_reply_again_and_replays_the_good_round() {
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
        assert_eq!(
            live.calls(),
            2,
            "round one rejected, the feedback round good"
        );
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
            1,
            "the rejected request is asked again; the stored good feedback round is replayed"
        );
    })
    .await;
}

/// A give-up (every round rejected) is asked again in full on a resume: nothing in the store is a
/// good reply, so every request is sent again, the estimate says so, and the part is still a
/// give-up with this model.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_of_a_give_up_asks_every_rejected_request_again() {
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

        let estimate = crate::run_store::estimate_resume(&store.run_id()).unwrap();
        assert_eq!(estimate.calls_stored, 0);
        assert_eq!(estimate.calls_to_send_again, sent, "{estimate:?}");

        let store = reopen(store);
        let resumed = FixModel::new(true);
        let journal = JournalLlm::new(&resumed, store.clone());
        let (_out, failed) =
            generate_fix_specifics(&journal, "o/r", one_finding(), &files(), None, None, None)
                .await;
        assert_eq!(failed.len(), 1, "the give-up stands with this model");
        assert_eq!(
            resumed.calls(),
            sent,
            "every rejected request is asked again"
        );
    })
    .await;
}

/// A fix model for two findings, the second at [`FAILING_LINE`]: it writes a good fix for the
/// first always, and for the failing one a fix naming something not in the evidence (turned down)
/// unless `fixes_with_reason` is set and the failing one's block carries the rejection note.
struct TwoFixModel {
    fixes_with_reason: bool,
    calls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
}

impl TwoFixModel {
    fn new(fixes_with_reason: bool) -> Self {
        Self {
            fixes_with_reason,
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl LlmPort for TwoFixModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(req.prompt.clone());
        let fixes: Vec<serde_json::Value> = blocks_of(&req.prompt)
            .iter()
            .map(|(id, line, block)| {
                let fix = if *line == FAILING_LINE
                    && !(self.fixes_with_reason && block.contains("was rejected"))
                {
                    "Call `neverInEvidence` first."
                } else {
                    "Validate the input before it reaches the query."
                };
                serde_json::json!({"id": id, "fix": fix})
            })
            .collect();
        Ok(response(
            &serde_json::json!({ "fixes": fixes }).to_string(),
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

/// The line of the finding the scripted models treat as the one that fails.
const FAILING_LINE: usize = 5;

fn two_fix_findings() -> Vec<Finding> {
    [1, FAILING_LINE]
        .iter()
        .map(|&line| {
            finding(
                "src/work.ts",
                line,
                "`runWork` passes `raw` straight to `doWork`",
                "doWork(raw)",
            )
        })
        .collect()
}

/// R1-1 (d), the fix mirror: a part that failed on content (one finding's fix turned down in
/// every round, the other written) is resumed by asking the failing finding again and only it.
/// The good fix comes from the store, the estimate counts the failing requests under "to send
/// again", and the retry names the stored reason.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_of_a_content_failed_fix_part_asks_the_failing_finding_again_and_only_it() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let live = TwoFixModel::new(false);
        let live_out = {
            let journal = JournalLlm::new(&live, store.clone());
            let (out, failed) = generate_fix_specifics(
                &journal,
                "o/r",
                two_fix_findings(),
                &files(),
                None,
                None,
                None,
            )
            .await;
            assert_eq!(failed.len(), 1, "the part failed on content: {failed:?}");
            assert!(out[0].fix_specific.is_some());
            assert!(out[1].fix_specific.is_none());
            out
        };
        let rejected = rejected_entries(&store);
        assert!(
            rejected >= 1,
            "the failing finding's rounds are rejected replies"
        );
        let estimate = crate::run_store::estimate_resume(&store.run_id()).unwrap();
        assert_eq!(estimate.calls_to_send_again, rejected, "{estimate:?}");
        assert!(estimate.calls_stored >= 1, "{estimate:?}");

        let store = reopen(store);
        let resumed = TwoFixModel::new(true);
        let journal = JournalLlm::new(&resumed, store.clone());
        let (out, failed) = generate_fix_specifics(
            &journal,
            "o/r",
            two_fix_findings(),
            &files(),
            None,
            None,
            None,
        )
        .await;
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            resumed.calls.load(Ordering::SeqCst),
            1,
            "exactly one fresh call"
        );
        let prompts = resumed.prompts.lock().unwrap().clone();
        let blocks = blocks_of(&prompts[0]);
        assert_eq!(blocks.len(), 1, "only the failing finding is sent");
        assert_eq!(blocks[0].1, FAILING_LINE);
        assert!(blocks[0].2.contains("was rejected"), "{}", blocks[0].2);
        assert_eq!(
            out[0].fix_specific, live_out[0].fix_specific,
            "from the store"
        );
        assert_eq!(
            out[1].fix_specific.as_deref(),
            Some("Validate the input before it reaches the query.")
        );
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

/// The detail stage: the full-batch reply was turned down whole, the later rounds earned the
/// explanations; a resume asks the rejected requests again (never more than the live run sent)
/// and replays the good ones, and the details come out the same.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_asks_a_wholly_rejected_detail_reply_again_and_replays_the_good_ones() {
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
        let live_calls = live.calls.load(Ordering::SeqCst);
        assert!(live_calls >= 2);
        let rejected = rejected_entries(&store);
        assert!(rejected >= 1);

        let estimate = crate::run_store::estimate_resume(&store.run_id()).unwrap();
        assert_eq!(estimate.calls_to_send_again, rejected, "{estimate:?}");

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
        let again = resumed.calls.load(Ordering::SeqCst);
        assert!(
            (1..=rejected).contains(&again),
            "only rejected requests are asked again ({again} of {rejected}); the good ones replay"
        );
        assert!(
            again < live_calls,
            "the good replies are not paid for again"
        );
    })
    .await;
}

/// A detail model that writes a good explanation for every finding, except the one at
/// [`FAILING_LINE`]: that one drops the name its statement made (a check that is not the
/// invented-precondition one, so no fallback) unless its block carries a reason for a previous
/// rejection and `fixes_with_reason` is set.
struct FailingLineDetailModel {
    fixes_with_reason: bool,
    calls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
}

impl FailingLineDetailModel {
    fn new(fixes_with_reason: bool) -> Self {
        Self {
            fixes_with_reason,
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl LlmPort for FailingLineDetailModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(req.prompt.clone());
        let entries: Vec<serde_json::Value> = blocks_of(&req.prompt)
            .iter()
            .map(|(id, line, block)| {
                let reasoned = block.contains("Your previous answer was turned down because");
                let why = if *line == FAILING_LINE && !(self.fixes_with_reason && reasoned) {
                    "The handler skips the check."
                } else {
                    "The handler skips the check on run_work."
                };
                serde_json::json!({"id": id, "why": why, "fix_direction": "Add the check."})
            })
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

fn ten_statements() -> Vec<Finding> {
    (1..=10)
        .map(|line| {
            finding(
                "src/work.ts",
                line,
                &format!("Skips the check{HEADLINE_SEP}Calls run_work with no check."),
                "doWork(raw)",
            )
        })
        .collect()
}

/// R1-1 (d): a part that failed on content (round one wrote 9 of 10 and turned one down, every
/// later round for that one was turned down) is resumed by asking the failing finding again and
/// only it. The nine good details come from the store, the estimate counts the failing requests
/// under "to send again", and the retry names the stored reason, so it is a changed request.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_of_a_content_failed_detail_part_asks_the_failing_finding_again_and_only_it() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let nonce = crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef");
        let live = FailingLineDetailModel::new(false);
        let live_out = {
            let journal = JournalLlm::new(&live, store.clone());
            let (out, failed) = regenerate_full_details(
                &journal,
                "o/r",
                ten_statements(),
                &files(),
                &[],
                &nonce,
                None,
                None,
            )
            .await;
            assert_eq!(failed.len(), 1, "the part failed on content: {failed:?}");
            assert!(out[FAILING_LINE - 1]
                .detail
                .contains(super::detail_regen::DETAIL_INCOMPLETE_MARKER));
            out
        };
        // Round one wrote nine and stays a good reply; the rounds for the failing one were
        // turned down whole.
        let rejected = rejected_entries(&store);
        assert!(
            rejected >= 1,
            "the failing finding's rounds are rejected replies"
        );
        let estimate = crate::run_store::estimate_resume(&store.run_id()).unwrap();
        assert_eq!(estimate.calls_to_send_again, rejected, "{estimate:?}");
        assert!(estimate.calls_stored >= 1, "{estimate:?}");

        let store = reopen(store);
        let resumed = FailingLineDetailModel::new(true);
        let journal = JournalLlm::new(&resumed, store.clone());
        let (out, failed) = regenerate_full_details(
            &journal,
            "o/r",
            ten_statements(),
            &files(),
            &[],
            &nonce,
            None,
            None,
        )
        .await;
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            resumed.calls.load(Ordering::SeqCst),
            1,
            "exactly one fresh call"
        );
        let prompts = resumed.prompts.lock().unwrap().clone();
        let blocks = blocks_of(&prompts[0]);
        assert_eq!(blocks.len(), 1, "only the failing finding is sent");
        assert_eq!(blocks[0].1, FAILING_LINE);
        assert!(
            blocks[0]
                .2
                .contains("Your previous answer was turned down because: dropped `run_work`"),
            "{}",
            blocks[0].2
        );
        assert!(out[FAILING_LINE - 1]
            .detail
            .contains("skips the check on run_work"));
        for (i, f) in out
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != FAILING_LINE - 1)
        {
            assert_eq!(
                f.detail, live_out[i].detail,
                "finding {i} comes from the store"
            );
        }
    })
    .await;
}

/// No stage replays a wholly rejected reply (R1-1 d; the earlier policy replayed it for the fix
/// and detail stages and a resume of a content failure sent nothing). The fix and detail stages
/// still replay an unusable (cut-off) reply, whose next round splits the batch and was stored.
/// The old assertions that a rejected fix or detail reply replays encoded the bug and are
/// replaced here.
#[test]
fn no_stage_replays_a_wholly_rejected_reply_and_only_fix_and_detail_replay_an_unusable_one() {
    use crate::run_store::journal::replays_on_resume;
    use crate::run_store::PartKind;
    for kind in [PartKind::Fix, PartKind::Detail] {
        assert!(replays_on_resume(EntryStatus::Done, kind));
        assert!(!replays_on_resume(EntryStatus::Rejected, kind));
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
