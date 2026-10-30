//! Retry diagnosis P6: every rejected reply records why, on the reply that was rejected and on no
//! other. The run store's call entry carries an optional `reason`; the failed part's attempts
//! carry the same reasons into `failures.json` and the cockpit's failure panel. Fake model only;
//! no live calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::run_store::journal::{CallEntry, CallFile, EntryStatus, JournalLlm};
use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};

fn response(text: &str, truncated: bool) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated,
    }
}

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().unwrap();
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
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

async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([7u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

/// Every stored call file of the run.
fn call_files(store: &RunStore) -> Vec<CallFile> {
    std::fs::read_dir(store.dir().join("calls"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).ok())
        .collect()
}

/// The first entry of the one call file whose first entry has `status`; panics unless exactly
/// one has.
fn the_entry_with(files: &[CallFile], status: EntryStatus) -> CallEntry {
    let hits: Vec<&CallEntry> = files
        .iter()
        .map(|f| &f.entries[&0])
        .filter(|e| e.status == status)
        .collect();
    assert_eq!(hits.len(), 1, "exactly one entry is {status:?}: {files:?}");
    hits[0].clone()
}

// ── the fix stage ────────────────────────────────────────────────────────────────────────────

fn fix_finding(path: &str, line: usize, detail: &str, snippet: &str) -> Finding {
    Finding {
        repo: "o/r".to_string(),
        path: path.to_string(),
        line,
        rule_id: "ARCH-REASON-1".to_string(),
        severity: "medium".to_string(),
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

/// Round one answers both findings, the second with a name that is in no evidence. A reply to a
/// prompt that carries a rejection note answers its one finding with a grounded fix.
struct FixStub {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for FixStub {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = if req.prompt.contains("was rejected") {
            r#"{"fixes":[{"index":0,"fix":"Validate `raw` before it reaches `doWork`."}]}"#
        } else {
            r#"{"fixes":[{"index":0,"fix":"Validate `raw` before it reaches `query`."},{"index":1,"fix":"Call `neverInEvidence` first."}]}"#
        };
        Ok(response(text, false))
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

fn two_fix_findings() -> (Vec<Finding>, Vec<(String, String)>) {
    let a = fix_finding(
        "src/db.ts",
        2,
        "`runQuery` passes `raw` straight to `query`",
        "query(raw)",
    );
    let b = fix_finding(
        "src/work.ts",
        2,
        "`runWork` passes `raw` straight to `doWork`",
        "doWork(raw)",
    );
    let files = vec![
        (
            "src/db.ts".to_string(),
            "export function runQuery(raw: string) {\n  return query(raw);\n}\n".to_string(),
        ),
        (
            "src/work.ts".to_string(),
            "export function runWork(raw: string) {\n  return doWork(raw);\n}\n".to_string(),
        ),
    ];
    (vec![a, b], files)
}

/// One reply with one ungrounded fix out of two carries a reason naming the token but STAYS
/// good: the fix it got right is kept, so a resume replays it instead of paying for both fixes
/// again (the fix it got wrong was re-asked with a changed request, whose reply is stored under
/// its own key). The reply that answered the retry in full is left good, with no reason.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_fix_reply_with_one_ungrounded_fix_carries_the_token_and_the_clean_reply_stays_done() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let stub = FixStub {
            calls: AtomicUsize::new(0),
        };
        let journal = JournalLlm::new(&stub, store.clone());
        let (findings, files) = two_fix_findings();
        let (out, failed) =
            generate_fix_specifics(&journal, "o/r", findings, &files, None, None, None).await;
        assert!(failed.is_empty(), "{failed:?}");
        assert!(out.iter().all(|f| f.fix_specific.is_some()));
        assert_eq!(
            stub.calls.load(Ordering::SeqCst),
            2,
            "round one and one retry"
        );

        let files = call_files(&store);
        assert_eq!(files.len(), 2, "two distinct requests");
        let entries: Vec<CallEntry> = files.iter().map(|f| f.entries[&0].clone()).collect();
        assert!(
            entries.iter().all(|e| e.status == EntryStatus::Done),
            "a reply with an accepted fix in it stays good: {entries:?}"
        );
        let first = entries
            .iter()
            .find(|e| e.reason.is_some())
            .expect("the partly rejected reply says why");
        let reason = first.reason.as_deref().unwrap();
        assert!(reason.contains("neverInEvidence"), "{reason}");
        assert!(
            reason.contains("src/work.ts:2"),
            "names the finding: {reason}"
        );
        assert!(
            reason.starts_with("1 of 2 fix(es) turned down"),
            "only the reply's own rejections: {reason}"
        );
        assert_eq!(
            entries.iter().filter(|e| e.reason.is_none()).count(),
            1,
            "the clean reply stays good, no reason"
        );
    })
    .await;
}

/// A reply whose every fix was turned down is set aside: nothing in it is worth replaying.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_fix_reply_with_every_fix_turned_down_is_set_aside() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let stub = FixStub {
            calls: AtomicUsize::new(0),
        };
        let journal = JournalLlm::new(&stub, store.clone());
        // Only the second finding: the stub's round-one fix for index 0 names `query`, which is
        // not in this finding's evidence, so the one fix of the reply is turned down.
        let (findings, files) = two_fix_findings();
        let findings = vec![findings[1].clone()];
        let (out, failed) =
            generate_fix_specifics(&journal, "o/r", findings, &files, None, None, None).await;
        assert!(failed.is_empty(), "{failed:?}");
        assert!(out[0].fix_specific.is_some());
        let files = call_files(&store);
        assert_eq!(files.len(), 2, "two distinct requests");
        let first = the_entry_with(&files, EntryStatus::Rejected);
        let reason = first
            .reason
            .as_deref()
            .expect("the rejected reply says why");
        assert!(reason.starts_with("1 of 1 fix(es) turned down"), "{reason}");
        assert!(reason.contains("`query`"), "{reason}");
        let second = the_entry_with(&files, EntryStatus::Done);
        assert_eq!(second.reason, None);
    })
    .await;
}

/// A stage that gave up leaves its accepted replies good. Before, every reply of the stage was
/// marked rejected.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_failed_fix_stage_does_not_set_aside_the_replies_that_were_accepted() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let stub = FixStub {
            calls: AtomicUsize::new(0),
        };
        let journal = JournalLlm::new(&stub, store.clone());
        let (findings, files) = two_fix_findings();
        let _ = generate_fix_specifics(&journal, "o/r", findings, &files, None, None, None).await;
        crate::run_store::journal::quarantine_rejected(
            &store,
            &[crate::run_store::FinishFailure {
                repo: "o/r".to_string(),
                pass: "fix-specific generation".to_string(),
                class: "content".to_string(),
                files: Vec::new(),
                scope_note: false,
            }],
        );
        let files = call_files(&store);
        assert_eq!(files.len(), 2);
        // Both replies had an accepted fix in them (the first one of two, the retry its one), so
        // both stay good; the quarantine does not touch the fix stage.
        assert!(
            files
                .iter()
                .all(|f| f.entries[&0].status == EntryStatus::Done),
            "{files:?}"
        );
        assert_eq!(
            files
                .iter()
                .filter(|f| f.entries[&0].reason.is_some())
                .count(),
            1
        );
    })
    .await;
}

/// The give-up carries the same reasons per finding into the failed part's attempts, and from
/// there into failures.json and failures.md.
#[tokio::test]
async fn a_fix_give_up_records_the_reason_per_finding_in_its_attempts_and_in_failures_json() {
    struct AlwaysInvents;
    #[async_trait::async_trait]
    impl LlmPort for AlwaysInvents {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            Ok(response(
                r#"{"fixes":[{"index":0,"fix":"Call `neverInEvidence` first."}]}"#,
                false,
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
    let (findings, files) = two_fix_findings();
    let findings = vec![findings[0].clone()];
    let (_out, failed) =
        generate_fix_specifics(&AlwaysInvents, "o/r", findings, &files, None, None, None).await;
    assert_eq!(failed.len(), 1);
    let with_reasons: Vec<&camerata_llm::failure::AttemptRecord> = failed[0]
        .attempts
        .iter()
        .filter(|a| !a.rejections.is_empty())
        .collect();
    assert_eq!(with_reasons.len(), 1, "{:?}", failed[0].attempts);
    let reason = &with_reasons[0].rejections[0];
    assert!(reason.contains("src/db.ts:2"), "{reason}");
    assert!(reason.contains("neverInEvidence"), "{reason}");

    let mut report = crate::release_gate::test_scan_report();
    report.failed_passes = failed;
    let built = crate::failure_report::build(&report);
    let json = serde_json::to_string(&built).unwrap();
    assert!(json.contains("\"rejections\""), "{json}");
    assert!(json.contains("neverInEvidence"), "{json}");
    assert!(built.render_md().contains("turned down:"), "md lists them");
}

// ── a reply cut off at the output limit ──────────────────────────────────────────────────────

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_reply_cut_off_at_the_output_limit_is_stored_unusable_with_the_reason() {
    struct CutOff;
    #[async_trait::async_trait]
    impl LlmPort for CutOff {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            Ok(response(r#"{"details":[{"id":"zz","pa"#, true))
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
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let journal = JournalLlm::new(&CutOff, store.clone());
        journal.complete(LlmRequest::new("anything")).await.unwrap();
        let files = call_files(&store);
        let entry = &files[0].entries[&0];
        assert_eq!(entry.status, EntryStatus::Unusable);
        let reason = entry.reason.as_deref().unwrap();
        assert!(reason.contains("cut off at the output limit"), "{reason}");
    })
    .await;
}

/// A reply that holds no JSON says so, and a good reply carries no reason.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_reply_with_no_json_says_so_and_a_good_one_has_no_reason() {
    struct Mixed(Mutex<Vec<&'static str>>);
    #[async_trait::async_trait]
    impl LlmPort for Mixed {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            let text = self.0.lock().unwrap().remove(0);
            Ok(response(text, false))
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
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let stub = Mixed(Mutex::new(vec!["sorry, no json", r#"{"ok":true}"#]));
        let journal = JournalLlm::new(&stub, store.clone());
        journal.complete(LlmRequest::new("first")).await.unwrap();
        journal.complete(LlmRequest::new("second")).await.unwrap();
        let files = call_files(&store);
        let bad = the_entry_with(&files, EntryStatus::Unusable);
        assert!(bad.reason.as_deref().unwrap().contains("no JSON"));
        let good = the_entry_with(&files, EntryStatus::Done);
        assert_eq!(good.reason, None);
    })
    .await;
}

/// The reason is scrubbed of secrets before it is stored.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stored_reason_is_scrubbed_of_secrets() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let stub = FixStub {
            calls: AtomicUsize::new(0),
        };
        let journal = JournalLlm::new(&stub, store.clone());
        let req = LlmRequest::new(r#"{"anything":1}"#);
        let key = crate::run_store::journal::request_key(&req);
        journal.complete(req).await.unwrap();
        journal.note_rejected_reply(
            &key,
            "named sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA1234 in the fix",
            true,
        );
        let files = call_files(&store);
        let reason = files[0].entries[&0].reason.clone().unwrap();
        assert!(!reason.contains("AAAAAAAA"), "{reason}");
        assert_eq!(files[0].entries[&0].status, EntryStatus::Rejected);
    })
    .await;
}
