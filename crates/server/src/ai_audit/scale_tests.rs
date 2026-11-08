//! M1 item 4: the scan's prompt assembly at the size of a large repo. A synthetic repo of
//! generated small files is written to a temp dir, read back the way a scan reads a repo, and run
//! through the real-time and the batch scan paths with a scripted model. No model call is made.
//!
//! It asserts what M1 promises: no request is over the model's window estimate, every file is in
//! at least one request's chunk, and no batch body is over the limit. The 5,000-file run is
//! ignored in the normal test run when it is slow (see the timing note on the test); the
//! 1,000-file run always runs.

use std::collections::HashSet;
use std::sync::Mutex;

use super::batch_mode::BatchApi;
use super::candidate_passes::CandidateEnv;
use super::repo_map::{estimate_tokens, tree_budget_for_window};
use super::request_budget::{context_window_tokens, input_limit_tokens, request_bytes};
use super::*;
use crate::llm::{BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl};
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::scan_routing::{plan_routes, AuditRule};

const REPO: &str = "acme/scale";
/// A model id the registry does not list: the default 200k-token window, the hardest case.
const MODEL: &str = "unlisted-small-window-model";

fn ok_reply() -> LlmResponse {
    LlmResponse {
        reported_model: None,
        text: "{\"findings\":[]}".to_string(),
        model: "stub".to_string(),
        backend: "stub".to_string(),
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// Answers every request with no findings and keeps it.
struct Scripted {
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.requests.lock().unwrap().push(req);
        Ok(ok_reply())
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

/// A batch API that keeps the size of every body it is handed and answers every item.
struct ScriptedBatch {
    realtime: Scripted,
    body_bytes: Mutex<Vec<usize>>,
    items: Mutex<Vec<BatchItem>>,
    pending: Mutex<Vec<Vec<String>>>,
}

#[async_trait::async_trait]
impl BatchApi for ScriptedBatch {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.realtime
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        let body = serde_json::to_vec(&serde_json::json!({ "requests": items }))?;
        self.body_bytes.lock().unwrap().push(body.len());
        let mut pending = self.pending.lock().unwrap();
        pending.push(items.iter().map(|i| i.custom_id.clone()).collect());
        let id = format!("msgbatch_{}", pending.len() - 1);
        self.items.lock().unwrap().extend(items);
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        let n: usize = batch_id.trim_start_matches("msgbatch_").parse().unwrap();
        let ids = self.pending.lock().unwrap()[n].clone();
        Ok(ids
            .into_iter()
            .map(|custom_id| BatchResultRow {
                custom_id,
                response: Some(ok_reply()),
                error: None,
                billed: None,
            })
            .collect())
    }
}

/// `n` small generated source files in a temp dir, across directories, each ~600 bytes.
fn write_repo(root: &std::path::Path, n: usize) {
    for i in 0..n {
        let dir = root.join(format!("src/area{}/module{}", i % 25, (i / 25) % 40));
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = format!("// generated file {i}\nexport class Component{i} {{}}\n");
        for l in 0..14 {
            body.push_str(&format!(
                "export function handler{i}x{l}(input: string): string {{ return input + \"{l}\"; }}\n"
            ));
        }
        std::fs::write(dir.join(format!("component_{i}.ts")), body).unwrap();
    }
}

fn rules() -> Vec<AuditRule> {
    (0..20)
        .map(|i| AuditRule::new(format!("RULE-{i}"), format!("directive number {i}")))
        .collect()
}

/// The file paths named by the per-file frames of a prompt.
fn framed_paths(prompt: &str, nonce: &crate::prompt_frame::FrameNonce, out: &mut HashSet<String>) {
    let open = format!("<<<UNTRUSTED {nonce} \"");
    let mut rest = prompt;
    while let Some(at) = rest.find(&open) {
        rest = &rest[at + open.len()..];
        if let Some(end) = rest.find("\">>>") {
            // The repo map and the symbols section are framed too, but are not files.
            if !matches!(&rest[..end], "repo map" | "symbols") {
                out.insert(rest[..end].to_string());
            }
        }
    }
}

async fn scale_run(n: usize) {
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path(), n);
    let extracted = crate::onboard::files::read_local_repo_files(dir.path()).unwrap();
    assert_eq!(extracted.files.len(), n, "every generated file is read");
    let mut files = crate::onboard::redact::files_for_model(&extracted.files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let rules = rules();
    let plan = plan_routes(&rules, &files, true);
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding_files(&[&files]);
    let window = context_window_tokens(MODEL);
    let map = RepoMap::new(&files, &nonce, tree_budget_for_window(window));
    let adopted: HashSet<String> = rules.iter().map(|r| r.id.to_ascii_uppercase()).collect();
    let want: HashSet<String> = files.iter().map(|(p, _)| p.clone()).collect();

    // Real time.
    let llm = Scripted {
        requests: Mutex::new(Vec::new()),
    };
    let mut skipped = Vec::new();
    let (_f, _p, _r, ok, err) = run_routed_passes(
        &llm,
        REPO,
        &files,
        &rules,
        &plan,
        &map,
        &nonce,
        &adopted,
        Some(MODEL),
        None,
        None,
        6,
        RULE_BATCH_SIZE,
        None,
        &mut skipped,
        CandidateEnv::production(),
    )
    .await;
    assert!(err.is_empty(), "{:?}", err.first());
    let sent = llm.requests.into_inner().unwrap();
    assert_eq!(ok, sent.len());
    let mut covered: HashSet<String> = HashSet::new();
    for req in &sent {
        let limit = input_limit_tokens(MODEL, req.max_tokens);
        assert!(
            estimate_tokens(request_bytes(req)) <= limit,
            "a request is over the window estimate"
        );
        framed_paths(&req.prompt, &nonce, &mut covered);
    }
    let missing: Vec<&String> = want.difference(&covered).take(5).collect();
    let extra: Vec<&String> = covered.difference(&want).take(5).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "every file is in at least one request's chunk: missing {missing:?}, extra {extra:?}"
    );

    // Batch: the same assembly, as batch items; a small body limit makes the byte split show.
    let api = ScriptedBatch {
        realtime: Scripted {
            requests: Mutex::new(Vec::new()),
        },
        body_bytes: Mutex::new(Vec::new()),
        items: Mutex::new(Vec::new()),
        pending: Mutex::new(Vec::new()),
    };
    let limit = 900_000usize;
    batch_mode::set_test_body_limit(Some(limit));
    let mut skipped = Vec::new();
    let out = run_routed_passes_batch(
        &api,
        REPO,
        &files,
        &rules,
        &plan,
        &map,
        &nonce,
        &adopted,
        Some(MODEL),
        None,
        RULE_BATCH_SIZE,
        None,
        &mut skipped,
        CandidateEnv::production(),
    )
    .await
    .unwrap();
    batch_mode::set_test_body_limit(None);
    assert!(out.4.is_empty(), "{:?}", out.4.first());
    let bodies = api.body_bytes.lock().unwrap().clone();
    assert!(
        bodies.len() > 1,
        "the work is several batches at this limit"
    );
    for b in &bodies {
        assert!(*b <= limit + 64, "a batch body is {b} bytes, over {limit}");
    }
    let mut batch_covered: HashSet<String> = HashSet::new();
    for item in api.items.lock().unwrap().iter() {
        let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
        let text = match content.as_str() {
            Some(t) => t.to_string(),
            None => content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .collect::<String>(),
        };
        framed_paths(&text, &nonce, &mut batch_covered);
    }
    let missing: Vec<&String> = want.difference(&batch_covered).take(5).collect();
    assert!(
        missing.is_empty(),
        "every file is in some batch item: missing {missing:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_thousand_file_repo_assembles_within_the_window_and_the_batch_limit() {
    scale_run(1_000).await;
}

/// 5,000 files. Ignored in the normal run only if it takes more than about 20 seconds in debug
/// (run it with `--ignored`); the 1,000-file test above always runs.
#[tokio::test(start_paused = true)]
async fn a_five_thousand_file_repo_assembles_within_the_window_and_the_batch_limit() {
    scale_run(5_000).await;
}
