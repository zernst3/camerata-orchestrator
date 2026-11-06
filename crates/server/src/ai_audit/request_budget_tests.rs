//! M1 item 2: a request is checked against the model's window before it is sent; a chunk that
//! would not fit becomes several requests, and a single file too large on its own is split by
//! line ranges with overlap. Nothing is dropped.

use std::sync::Mutex;

use super::repo_map::{estimate_tokens, CHARS_PER_TOKEN_ESTIMATE};
use super::request_budget::{
    ensure_request_fits, fit_chunks, input_limit_tokens, request_bytes, LINE_OVERLAP,
};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

fn nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef")
}

/// A model id the registry does not list: the default 200k-token window.
const SMALL_WINDOW_MODEL: &str = "unlisted-small-window-model";

/// `lines` lines, each carrying a marker that names the file and the line, padded to `width`.
fn file_with_markers(name: &str, lines: usize, width: usize) -> (String, String) {
    let body: String = (1..=lines)
        .map(|n| format!("// MARK[{name}:{n}] {}\n", "x".repeat(width)))
        .collect();
    (name.to_string(), body)
}

fn markers(name: &str, lines: usize) -> Vec<String> {
    (1..=lines).map(|n| format!("MARK[{name}:{n}]")).collect()
}

/// Every `MARK[...]` in `texts`, found in one pass over each text.
fn found_markers<'a>(
    texts: impl IntoIterator<Item = &'a str>,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for text in texts {
        let mut rest = text;
        while let Some(at) = rest.find("MARK[") {
            rest = &rest[at..];
            let Some(end) = rest.find(']') else { break };
            out.insert(rest[..=end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

#[test]
fn a_chunk_over_the_limit_becomes_several_parts_that_cover_every_line() {
    let files: Vec<(String, String)> = (0..30)
        .map(|i| file_with_markers(&format!("f{i}.rs"), 200, 40))
        .collect();
    let whole = build_digest(&files, &nonce());
    // A budget a fifth of the chunk's digest: five or more parts.
    let budget_tokens = estimate_tokens(whole.len()) / 5;
    let chunk: &[(String, String)] = &files;
    let (parts, report) = fit_chunks(&[chunk], None, &nonce(), budget_tokens);
    assert!(parts.len() >= 5, "got {} parts", parts.len());
    assert_eq!(report.chunks_split, 1);
    assert_eq!(report.parts_added, parts.len() - 1);
    for p in &parts {
        assert!(
            p.digest.len() <= budget_tokens * CHARS_PER_TOKEN_ESTIMATE,
            "a part is over the budget"
        );
    }
    // The parts' file slices tile the chunk exactly, in order.
    let tiled: Vec<&str> = parts
        .iter()
        .flat_map(|p| p.files.iter().map(|(n, _)| n.as_str()))
        .collect();
    let want: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(tiled, want);
    // Every line of every file is in some part's digest.
    let found = found_markers(parts.iter().map(|p| p.digest.as_str()));
    for (name, _) in &files {
        for m in markers(name, 200) {
            assert!(found.contains(&m), "{m} is in no part");
        }
    }
}

#[test]
fn a_chunk_that_fits_is_one_part_with_the_digest_it_always_had() {
    let files = vec![file_with_markers("a.rs", 10, 20)];
    let chunk: &[(String, String)] = &files;
    let (parts, report) = fit_chunks(&[chunk], None, &nonce(), 100_000);
    assert_eq!(parts.len(), 1);
    assert_eq!(report, Default::default());
    assert_eq!(parts[0].digest, build_digest(&files, &nonce()));
}

#[test]
fn a_single_huge_file_is_covered_by_overlapping_line_ranges() {
    let files = vec![file_with_markers("huge.rs", 6000, 50)];
    let whole = build_digest(&files, &nonce());
    let budget_tokens = estimate_tokens(whole.len()) / 6;
    let chunk: &[(String, String)] = &files;
    let (parts, report) = fit_chunks(&[chunk], None, &nonce(), budget_tokens);
    assert!(parts.len() >= 6, "got {} parts", parts.len());
    assert_eq!(report.files_split_by_lines, 1);
    for p in &parts {
        assert_eq!(p.files.len(), 1);
        assert!(p.digest.len() <= budget_tokens * CHARS_PER_TOKEN_ESTIMATE);
    }
    // Every line is covered, with its real number: the marker and the `NNNN|` prefix agree.
    for p in &parts {
        for line in p.digest.lines().filter(|l| l.contains("MARK[")) {
            let (no, rest) = line.split_once("| ").unwrap();
            let n: usize = no.trim().parse().unwrap();
            assert!(
                rest.contains(&format!("MARK[huge.rs:{n}]")),
                "misnumbered: {line}"
            );
        }
    }
    let found = found_markers(parts.iter().map(|p| p.digest.as_str()));
    for m in markers("huge.rs", 6000) {
        assert!(found.contains(&m), "{m} missing");
    }
    // Neighbouring parts share at least LINE_OVERLAP lines.
    for pair in parts.windows(2) {
        let a = found_markers([pair[0].digest.as_str()]);
        let b = found_markers([pair[1].digest.as_str()]);
        let shared = a.intersection(&b).count();
        assert!(shared >= LINE_OVERLAP, "only {shared} lines shared");
    }
    assert!(parts[1].digest.contains("part 2 of"));
}

#[test]
fn one_line_longer_than_a_part_is_cut_and_still_covered() {
    let long = "y".repeat(50_000);
    let files = vec![(
        "min.js".to_string(),
        format!("first\n{long}END-OF-LONG\nlast\n"),
    )];
    let chunk: &[(String, String)] = &files;
    let (parts, _report) = fit_chunks(&[chunk], None, &nonce(), 4_000);
    assert!(parts.len() > 2);
    let joined: String = parts.iter().map(|p| p.digest.as_str()).collect();
    assert!(joined.contains("END-OF-LONG"));
    assert!(joined.contains("   3| last"));
    for p in &parts {
        assert!(p.digest.len() <= 4_000 * CHARS_PER_TOKEN_ESTIMATE);
    }
}

#[test]
fn an_override_digest_that_does_not_fit_falls_back_to_the_whole_files() {
    let files: Vec<(String, String)> = (0..10)
        .map(|i| file_with_markers(&format!("c{i}.rs"), 100, 40))
        .collect();
    let chunk: &[(String, String)] = &files;
    let huge_candidate = "z".repeat(1_000_000);
    let budget = estimate_tokens(build_digest(&files, &nonce()).len()) / 3;
    let (parts, _) = fit_chunks(&[chunk], Some(&[huge_candidate]), &nonce(), budget);
    assert!(parts.len() >= 3);
    assert!(parts.iter().all(|p| !p.digest.starts_with('z')));
    let found = found_markers(parts.iter().map(|p| p.digest.as_str()));
    for (name, _) in &files {
        for m in markers(name, 100) {
            assert!(found.contains(&m));
        }
    }
}

#[test]
fn the_limit_is_the_window_minus_the_output_and_an_unknown_model_gets_the_smallest_window() {
    // Listed: 1,000,000 window, 128,000 output.
    assert_eq!(input_limit_tokens("claude-sonnet-5-5", 8192), 872_000);
    // Unlisted: 200,000 window minus the request's own max_tokens.
    assert_eq!(input_limit_tokens(SMALL_WINDOW_MODEL, 10_000), 190_000);
}

#[test]
fn an_oversized_request_is_refused_before_it_is_sent() {
    let big = LlmRequest::new("x".repeat(700_000))
        .with_model(SMALL_WINDOW_MODEL)
        .with_max_tokens(8_192);
    assert!(request_bytes(&big) >= 700_000);
    let err = ensure_request_fits(&big).expect_err("700k bytes is over a 200k-token window");
    assert!(err.estimated_tokens > err.limit_tokens);
    assert!(err.to_string().contains("was not sent"));
    let small = LlmRequest::new("x".repeat(1_000))
        .with_model(SMALL_WINDOW_MODEL)
        .with_max_tokens(8_192);
    assert!(ensure_request_fits(&small).is_ok());
}

struct Capture {
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for Capture {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.requests.lock().unwrap().push(req);
        Ok(LlmResponse {
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

async fn run_over(
    llm: &Capture,
    chunk: &[(String, String)],
    rules: &[(String, String)],
) -> (usize, Vec<PassFailure>) {
    let map = RepoMap::whole_text("REPO MAP:\nx\n");
    let batches: Vec<&[(String, String)]> = vec![rules];
    let (_f, _p, _r, ok, err) = run_passes(
        llm,
        "acme/widgets",
        &map,
        &nonce(),
        &std::collections::HashSet::new(),
        Some(SMALL_WINDOW_MODEL),
        None,
        None,
        &[chunk],
        None,
        &batches,
        2,
        "pass[all]",
        "fit",
        None,
        false,
    )
    .await;
    (ok, err)
}

#[tokio::test]
async fn run_passes_splits_a_chunk_for_a_small_window_and_sends_nothing_over_it() {
    // Three 250 KB files: one 840 KB digest, far over a 200k-token window.
    let files: Vec<(String, String)> = (0..3)
        .map(|i| file_with_markers(&format!("big{i}.rs"), 4_000, 50))
        .collect();
    let llm = Capture {
        requests: Mutex::new(Vec::new()),
    };
    let rules = vec![("RULE-A".to_string(), "do a".to_string())];
    let (ok, err) = run_over(&llm, &files, &rules).await;
    assert!(err.is_empty(), "{err:?}");
    let sent = llm.requests.into_inner().unwrap();
    assert!(
        sent.len() >= 2,
        "one oversized chunk must be several requests"
    );
    assert_eq!(ok, sent.len());
    for req in &sent {
        let limit = input_limit_tokens(SMALL_WINDOW_MODEL, req.max_tokens);
        assert!(
            estimate_tokens(request_bytes(req)) <= limit,
            "a request is over the window estimate"
        );
    }
    let found = found_markers(sent.iter().map(|r| r.prompt.as_str()));
    for (name, _) in &files {
        for m in markers(name, 4_000) {
            assert!(found.contains(&m), "{m} was never sent");
        }
    }
}

#[tokio::test]
async fn run_passes_refuses_a_request_it_cannot_make_fit_and_sends_nothing() {
    let files = vec![file_with_markers("a.rs", 10, 10)];
    let llm = Capture {
        requests: Mutex::new(Vec::new()),
    };
    // One rule directive larger than the whole window: no digest split can help.
    let rules = vec![("RULE-A".to_string(), "r".repeat(900_000))];
    let (ok, err) = run_over(&llm, &files, &rules).await;
    assert_eq!(ok, 0);
    assert_eq!(err.len(), 1, "the pass is a disclosed failure: {err:?}");
    assert!(err[0].error.contains("was not sent"), "{:?}", err[0].error);
    assert!(
        llm.requests.into_inner().unwrap().is_empty(),
        "nothing oversized may be sent"
    );
}

#[tokio::test]
async fn the_alternatives_stage_splits_a_chunk_for_a_small_window_too() {
    use super::structured_output_tests::{stage_of, Recorder};
    let files: Vec<(String, String)> = (0..3)
        .map(|i| file_with_markers(&format!("alt{i}.rs"), 4_000, 50))
        .collect();
    let rule = RuleAlternatives {
        rule_id: "MULTI-0".to_string(),
        options: vec![RuleOptionView {
            id: "opt-a".to_string(),
            label: "Option A".to_string(),
            directive: "Do it the A way.".to_string(),
            why: "A is simple.".to_string(),
            violates: false,
        }],
        selected_option_id: Some("opt-a".to_string()),
        kind: Some(camerata_rules::RuleKind::Decision),
    };
    let llm = Recorder::new();
    let _ = recommend_alternatives(
        &llm,
        "o/r",
        &files,
        &files,
        &nonce(),
        &[rule],
        Some(SMALL_WINDOW_MODEL),
        None,
        None,
    )
    .await
    .unwrap();
    let sent: Vec<LlmRequest> = llm
        .requests()
        .into_iter()
        .filter(|r| stage_of(r) == "alternatives")
        .collect();
    // The stage asks a further digest only for a rule the first did not settle, so it sends as
    // many requests as it needs; each is inside the window, and the first digest is a part of the
    // oversized chunk, not all of it.
    assert!(!sent.is_empty());
    for req in &sent {
        assert!(
            estimate_tokens(request_bytes(req))
                <= input_limit_tokens(SMALL_WINDOW_MODEL, req.max_tokens)
        );
    }
    let whole = build_digest(&files, &nonce()).len();
    assert!(sent.iter().all(|r| r.prompt.len() < whole));
}
