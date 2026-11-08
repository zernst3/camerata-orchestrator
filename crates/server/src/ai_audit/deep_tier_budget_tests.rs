//! M1 item 5: the deep tier's requests go through the same size check as the scan's. A repo
//! whose whole-repo digest is over the model's window used to produce one oversized prose-lens
//! request (and a digest silently clipped at the character cap); now every request fits and every
//! file is covered by each lens.

use std::sync::Mutex;

use super::repo_map::estimate_tokens;
use super::request_budget::{input_limit_tokens, request_bytes};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

/// A model id the registry does not list: the default 200k-token window.
const SMALL_WINDOW_MODEL: &str = "unlisted-small-window-model";

struct Capture {
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for Capture {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.requests.lock().unwrap().push(req);
        Ok(LlmResponse {
            reported_model: None,
            text: "{}".to_string(),
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
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

fn big_file(name: &str, lines: usize) -> (String, String) {
    let body: String = (1..=lines)
        .map(|n| format!("// MARK[{name}:{n}] {}\n", "x".repeat(50)))
        .collect();
    (name.to_string(), body)
}

fn markers_in(texts: &[&str]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for text in texts {
        let mut rest = *text;
        while let Some(at) = rest.find("MARK[") {
            rest = &rest[at..];
            let Some(end) = rest.find(']') else { break };
            out.insert(rest[..=end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

#[tokio::test]
async fn every_deep_tier_request_fits_the_window_and_each_lens_covers_every_file() {
    // Three 250 KB files: a 860 KB whole-repo digest, over a 200k-token window.
    let files: Vec<(String, String)> = (0..3)
        .map(|i| big_file(&format!("deep{i}.rs"), 4_000))
        .collect();
    let llm = Capture {
        requests: Mutex::new(Vec::new()),
    };
    let _ = run_deep_tier(
        &llm,
        "acme/app",
        &files,
        Some(SMALL_WINDOW_MODEL),
        ScanMode::Parallel,
        None,
        None,
        true,
    )
    .await;
    let sent = llm.requests.into_inner().unwrap();
    for req in &sent {
        assert!(
            estimate_tokens(request_bytes(req))
                <= input_limit_tokens(SMALL_WINDOW_MODEL, req.max_tokens),
            "a deep-tier request is over the window estimate ({} bytes)",
            request_bytes(req)
        );
    }
    let all_marks: Vec<String> = files
        .iter()
        .flat_map(|(n, _)| (1..=4_000).map(move |l| format!("MARK[{n}:{l}]")))
        .collect();
    for (what, needle) in [
        ("SOC-2 lens", DeepLens::Soc2Gap.title()),
        ("threat-model lens", DeepLens::ThreatModel.title()),
        ("security lens", "DEEP SECURITY AUDIT"),
    ] {
        let prompts: Vec<&str> = sent
            .iter()
            .filter(|r| r.prompt.contains(needle))
            .map(|r| r.prompt.as_str())
            .collect();
        assert!(!prompts.is_empty(), "{what} sent nothing");
        let found = markers_in(&prompts);
        let missing = all_marks.iter().filter(|m| !found.contains(*m)).count();
        assert_eq!(missing, 0, "{what} never saw {missing} lines");
    }
}

#[tokio::test]
async fn a_lens_that_cannot_fit_even_split_is_refused_before_sending_and_disclosed() {
    let files = vec![big_file("a.rs", 10)];
    let llm = Capture {
        requests: Mutex::new(Vec::new()),
    };
    let nonce = crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef");
    let map = RepoMap::whole_text("REPO MAP:\n");
    // A system prompt larger than the whole window: no split of the code can help.
    let result = run_prose_lens(
        &llm,
        DeepLens::ThreatModel,
        "acme/app",
        &map,
        &files,
        &nonce,
        "s".repeat(900_000),
        Some(SMALL_WINDOW_MODEL),
        None,
        None,
    )
    .await;
    let error = result.error.expect("the refusal is disclosed on the lens");
    assert!(error.contains("was not sent"), "{error}");
    assert!(llm.requests.into_inner().unwrap().is_empty());
}
