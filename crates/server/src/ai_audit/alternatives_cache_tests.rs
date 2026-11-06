//! Retry fixes F3, item 5: the alternatives stage re-sent the whole file digest, 25k to 40k input
//! tokens, uncached with every rule batch. It now puts the digest behind cache breakpoints, the
//! way the scan does, so a second rule batch reads it from the cache. No network: the model is a
//! recorder.

use super::structured_output_tests::{stage_of, Recorder};
use super::*;
use crate::llm::LlmRequest;

fn alternatives(rule: &str) -> RuleAlternatives {
    RuleAlternatives {
        rule_id: rule.to_string(),
        options: vec![
            RuleOptionView {
                id: "opt-a".to_string(),
                label: "Option A".to_string(),
                directive: "Do it the A way.".to_string(),
                why: "A is simple.".to_string(),
                violates: false,
            },
            RuleOptionView {
                id: "opt-b".to_string(),
                label: "Option B".to_string(),
                directive: "Do it the B way.".to_string(),
                why: "B is scalable.".to_string(),
                violates: false,
            },
        ],
        selected_option_id: Some("opt-a".to_string()),
        kind: Some(camerata_rules::RuleKind::Decision),
    }
}

fn files() -> Vec<(String, String)> {
    vec![(
        "src/digest_marker.ts".to_string(),
        "export const DIGEST_MARKER_SENTINEL = 1;\n".to_string(),
    )]
}

fn nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::generate_avoiding(&[])
}

async fn alternatives_requests(n_rules: usize) -> Vec<LlmRequest> {
    let llm = Recorder::new();
    let alts: Vec<RuleAlternatives> = (0..n_rules)
        .map(|i| alternatives(&format!("MULTI-{i}")))
        .collect();
    let _ = recommend_alternatives(
        &llm,
        "o/r",
        &files(),
        &files(),
        &nonce(),
        &alts,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    llm.requests()
        .into_iter()
        .filter(|r| stage_of(r) == "alternatives")
        .collect()
}

/// With two rule batches, both carry a breakpoint after the digest, the bytes before it are
/// identical in the two requests (so the second reads the first's cache entry), the digest is
/// inside that prefix, and what differs between the batches (the rules) is after it.
#[tokio::test]
async fn the_digest_is_a_cached_prefix_on_the_second_rule_batch() {
    let reqs = alternatives_requests(ALTERNATIVES_BATCH_SIZE + 3).await;
    assert_eq!(reqs.len(), 2, "two rule batches");
    let (first, second) = (&reqs[0], &reqs[1]);
    assert_eq!(
        first.cache_breakpoints.len(),
        1,
        "{:?}",
        first.cache_breakpoints
    );
    assert_eq!(first.cache_breakpoints, second.cache_breakpoints);
    let at = first.cache_breakpoints[0];
    assert!(at < first.prompt.len() && at < second.prompt.len());
    assert_eq!(
        &first.prompt[..at],
        &second.prompt[..at],
        "the second batch repeats the cached prefix byte for byte"
    );
    assert!(first.prompt[..at].contains("DIGEST_MARKER_SENTINEL"));
    assert!(first.prompt[..at].starts_with("Repository: o/r"));
    // The rules differ per batch and sit after the breakpoint.
    assert!(first.prompt[at..].contains("MULTI-0"));
    assert!(second.prompt[at..].contains(&format!("MULTI-{ALTERNATIVES_BATCH_SIZE}")));
    assert_ne!(&first.prompt[at..], &second.prompt[at..]);
    // The system prompt and the schema, which come first in the cache key, are the same.
    assert_eq!(first.system, second.system);
    assert_eq!(first.response_schema, second.response_schema);
}

/// One request over one digest repeats nothing, so it is not cached (a cache write costs more
/// than the input it writes and nothing would read it).
#[tokio::test]
async fn a_single_rule_batch_over_one_digest_carries_no_breakpoint() {
    let reqs = alternatives_requests(3).await;
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].cache_breakpoints.is_empty());
}

#[test]
fn the_breakpoints_follow_what_a_later_request_repeats() {
    // (map end, digest end, rule batches, digests)
    assert_eq!(
        alternatives_cache_breakpoints(10, 50, 1, 1),
        Vec::<usize>::new()
    );
    assert_eq!(alternatives_cache_breakpoints(10, 50, 3, 1), vec![50]);
    assert_eq!(alternatives_cache_breakpoints(10, 50, 1, 2), vec![10]);
    assert_eq!(alternatives_cache_breakpoints(10, 50, 3, 2), vec![10, 50]);
}
