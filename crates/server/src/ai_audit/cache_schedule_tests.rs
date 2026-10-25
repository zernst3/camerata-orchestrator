//! Rank 3, cache scheduling on the API path. Two properties, both "billing only, never recall":
//!
//! 1. the prompt carries exactly the same parts as before (only the per-group label line moved
//!    out of the cached prefix), so no model sees different content;
//! 2. the passes are scheduled warm-then-fan-out, so a cache entry is written once and read by
//!    the rest, instead of every concurrent pass missing and writing it.

use std::collections::HashSet;
use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";
const REPO_MAP: &str = "REPO MAP:\nfoo.rs: fn foo()\n";

/// The layout BEFORE this change, kept here as the reference the new layout is held against:
/// the label line sat between the repo map and the digest, inside the cached prefix.
fn layout_before(
    label: &str,
    ci: usize,
    n_c: usize,
    digest: &str,
    task: &str,
    rules: &str,
) -> String {
    format!(
        "Repository: {REPO}\n\n{REPO_MAP}({label} {}/{n_c})\n\n{digest}\n\n{task}\n\n{rules}",
        ci + 1
    )
}

fn label_line(label: &str, ci: usize, n_c: usize) -> String {
    format!("({label} {}/{n_c})\n\n", ci + 1)
}

#[test]
fn the_prompt_carries_exactly_the_same_parts_as_before_only_the_label_line_moved() {
    let digest = "fn a() {}\nfn b() {}";
    let task = "── Check the code above against ONLY the ADOPTED rules below. ──";
    let rules = "RULES:\n- RULE-A: do A\n- RULE-B: do B";
    for (label, ci, n_c, n_b) in [("pass[all]", 0, 1, 1), ("pass[web]", 2, 5, 4)] {
        let before = layout_before(label, ci, n_c, digest, task, rules);
        let (after, _bps) =
            build_pass_prompt(REPO, REPO_MAP, label, ci, n_c, digest, n_b, task, rules);

        // Same bytes, rearranged: identical length and identical multiset of bytes.
        assert_eq!(before.len(), after.len());
        let sorted = |s: &str| {
            let mut b = s.as_bytes().to_vec();
            b.sort_unstable();
            b
        };
        assert_eq!(sorted(&before), sorted(&after));

        // Every part appears exactly once in both.
        let label_line = label_line(label, ci, n_c);
        for part in [
            format!("Repository: {REPO}\n\n{REPO_MAP}"),
            format!("{digest}\n\n"),
            label_line.clone(),
            task.to_string(),
            rules.to_string(),
        ] {
            assert_eq!(before.matches(&part).count(), 1, "before: {part:?}");
            assert_eq!(after.matches(&part).count(), 1, "after: {part:?}");
        }

        // Take the label line out of both and what remains is identical, in the same order:
        // the label line is the ONLY thing that moved.
        assert_eq!(
            before.replace(&label_line, ""),
            after.replace(&label_line, "")
        );
    }
}

#[test]
fn the_group_label_is_outside_the_cached_prefix() {
    let (prompt, bps) = build_pass_prompt(
        REPO,
        REPO_MAP,
        "pass[web]",
        1,
        3,
        "DIGEST",
        4,
        "TASK",
        "RULES",
    );
    let cached = &prompt[..*bps.last().unwrap()];
    assert!(
        !cached.contains("pass[web]") && !cached.contains("1/3") && !cached.contains("2/3"),
        "the label and chunk index must not sit in a cached block: {cached:?}"
    );
    assert!(
        prompt[*bps.last().unwrap()..].contains("(pass[web] 2/3)"),
        "the label travels in the uncached suffix"
    );
}

#[test]
fn two_routing_groups_over_the_same_files_share_every_cached_byte() {
    let (all, bps_all) = build_pass_prompt(
        REPO,
        REPO_MAP,
        "pass[all]",
        0,
        2,
        "SAME DIGEST",
        3,
        "T1",
        "R1",
    );
    let (web, bps_web) = build_pass_prompt(
        REPO,
        REPO_MAP,
        "pass[web]",
        0,
        2,
        "SAME DIGEST",
        3,
        "T2",
        "R2",
    );
    assert_eq!(bps_all, bps_web, "same breakpoint offsets");
    let end = *bps_all.last().unwrap();
    assert_eq!(
        &all[..end],
        &web[..end],
        "a second group must read the first group's cache entry, not rewrite it"
    );
}

#[test]
fn breakpoints_sit_at_the_stable_prefix_boundaries() {
    let digest = "DIGEST-BODY";
    let (prompt, bps) = build_pass_prompt(
        REPO,
        REPO_MAP,
        "pass[all]",
        0,
        1,
        digest,
        3,
        "TASK",
        "RULES",
    );
    assert_eq!(bps.len(), 2);
    assert_eq!(
        &prompt[..bps[0]],
        format!("Repository: {REPO}\n\n{REPO_MAP}")
    );
    assert_eq!(&prompt[bps[0]..bps[1]], format!("{digest}\n\n"));
    let suffix = &prompt[bps[1]..];
    assert!(suffix.contains("TASK") && suffix.contains("RULES"));

    // One rule-batch: the digest is read by nobody, so only the repo map is cached.
    let (prompt_one, bps_one) = build_pass_prompt(
        REPO,
        REPO_MAP,
        "pass[all]",
        0,
        1,
        digest,
        1,
        "TASK",
        "RULES",
    );
    assert_eq!(bps_one.len(), 1);
    assert!(prompt_one[bps_one[0]..].contains(digest));
}

// ── Scheduling, observed through a stub that models the provider's prompt cache ─────────────

/// Models the part of Anthropic's prompt cache this change is about: a cached prefix becomes
/// readable only once the request that wrote it has RETURNED, and a request that arrives while
/// the prefix is still unwritten pays a write of its own. Records dispatch order too.
#[derive(Default)]
struct CacheModelLlm {
    inner: Mutex<CacheModel>,
}

#[derive(Default)]
struct CacheModel {
    written: HashSet<String>,
    events: Vec<(&'static str, usize, usize)>,
    in_flight: usize,
    max_in_flight: usize,
    cache_write_prefixes: usize,
    cache_read_prefixes: usize,
    breakpoints_per_request: Vec<usize>,
}

/// (chunk, batch), read back out of the prompt the scan built.
fn identify(prompt: &str) -> (usize, usize) {
    let chunk = (0..8)
        .find(|n| prompt.contains(&format!("CHUNK{n} marker")))
        .expect("chunk marker");
    let batch = (1..=8)
        .find(|n| prompt.contains(&format!("(batch {n}/")))
        .expect("batch marker")
        - 1;
    (chunk, batch)
}

#[async_trait::async_trait]
impl LlmPort for CacheModelLlm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let (chunk, batch) = identify(&req.prompt);
        let prefixes: Vec<String> = req
            .cache_breakpoints
            .iter()
            .map(|&bp| req.prompt[..bp].to_string())
            .collect();
        {
            let mut m = self.inner.lock().unwrap();
            m.events.push(("start", chunk, batch));
            m.in_flight += 1;
            m.max_in_flight = m.max_in_flight.max(m.in_flight);
            m.breakpoints_per_request.push(req.cache_breakpoints.len());
            // The longest prefix already written is a read; anything beyond it is a write.
            for p in &prefixes {
                if m.written.contains(p) {
                    m.cache_read_prefixes += 1;
                } else {
                    m.cache_write_prefixes += 1;
                }
            }
        }
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        {
            let mut m = self.inner.lock().unwrap();
            m.in_flight -= 1;
            m.events.push(("end", chunk, batch));
            m.written.extend(prefixes);
        }
        Ok(LlmResponse {
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
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        on_delta("{}");
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl CacheModel {
    fn pos(&self, ev: (&'static str, usize, usize)) -> usize {
        self.events
            .iter()
            .position(|e| *e == ev)
            .unwrap_or_else(|| panic!("{ev:?} never happened: {:?}", self.events))
    }
}

async fn scan(llm: &CacheModelLlm, n_chunks: usize, n_batches: usize, concurrency: usize) {
    let chunk_files: Vec<Vec<(String, String)>> = (0..n_chunks)
        .map(|n| {
            vec![(
                format!("c{n}.rs"),
                format!("// CHUNK{n} marker\nfn c{n}() {{}}"),
            )]
        })
        .collect();
    let chunks: Vec<&[(String, String)]> = chunk_files.iter().map(Vec::as_slice).collect();
    let rule_batches: Vec<Vec<(String, String)>> = (0..n_batches)
        .map(|n| vec![(format!("RULE-{n}"), format!("rule {n} desc"))])
        .collect();
    let batches: Vec<&[(String, String)]> = rule_batches.iter().map(Vec::as_slice).collect();
    let (_f, _p, _r, ok, err) = run_passes(
        llm,
        REPO,
        REPO_MAP,
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        &chunks,
        &batches,
        concurrency,
        "pass[all]",
        "cache-sched",
        None,
        false,
    )
    .await;
    assert!(err.is_none(), "{err:?}");
    assert_eq!(ok, n_chunks * n_batches);
}

#[tokio::test]
async fn the_first_pass_of_a_prefix_returns_before_its_siblings_start() {
    let llm = CacheModelLlm::default();
    scan(&llm, 3, 8, 6).await;
    let m = llm.inner.lock().unwrap();

    // The very first pass goes alone.
    let first_end = m.pos(("end", 0, 0));
    for (c, b) in [(0, 1), (0, 7), (1, 0), (2, 5)] {
        assert!(
            first_end < m.pos(("start", c, b)),
            "pass ({c},{b}) started before the first pass returned"
        );
    }
    // Each chunk's first rule-batch returns before that chunk's other rule-batches start.
    for c in 0..3 {
        let warm_end = m.pos(("end", c, 0));
        for b in 1..8 {
            assert!(
                warm_end < m.pos(("start", c, b)),
                "chunk {c}: rule-batch {b} started before the digest was written"
            );
        }
    }
    assert!(m.max_in_flight <= 6, "{}", m.max_in_flight);
}

#[tokio::test]
async fn each_prefix_is_written_once_and_every_other_pass_reads_it() {
    let llm = CacheModelLlm::default();
    let (n_chunks, n_batches) = (3, 8);
    scan(&llm, n_chunks, n_batches, 6).await;
    let m = llm.inner.lock().unwrap();

    // Breakpoints: every pass marks the repo map and its chunk digest.
    assert!(m.breakpoints_per_request.iter().all(|&n| n == 2));
    let passes = n_chunks * n_batches;
    assert_eq!(m.breakpoints_per_request.len(), passes);

    // One write for the repo map plus one per chunk digest; everything else is a read.
    assert_eq!(
        m.cache_write_prefixes,
        1 + n_chunks,
        "repo map once, each chunk digest once"
    );
    assert_eq!(m.cache_read_prefixes, passes * 2 - (1 + n_chunks));
}

#[tokio::test]
async fn a_second_routing_group_reads_the_first_groups_digest() {
    // Two `run_passes` calls (two routing groups) over the SAME files: with the label out of the
    // cached prefix the second group writes nothing new.
    let llm = CacheModelLlm::default();
    scan(&llm, 1, 4, 6).await;
    let writes_after_first = llm.inner.lock().unwrap().cache_write_prefixes;
    assert_eq!(writes_after_first, 2, "repo map + one digest");
    // Same chunk files, different label.
    let chunk = vec![(
        "c0.rs".to_string(),
        "// CHUNK0 marker\nfn c0() {}".to_string(),
    )];
    let chunks: Vec<&[(String, String)]> = vec![chunk.as_slice()];
    let rules = [
        vec![("RULE-X".to_string(), "x".to_string())],
        vec![("RULE-Y".to_string(), "y".to_string())],
    ];
    let batches: Vec<&[(String, String)]> = rules.iter().map(Vec::as_slice).collect();
    let (_f, _p, _r, ok, _e) = run_passes(
        &llm,
        REPO,
        REPO_MAP,
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        &chunks,
        &batches,
        6,
        "pass[web]",
        "group-two",
        None,
        false,
    )
    .await;
    assert_eq!(ok, 2);
    assert_eq!(
        llm.inner.lock().unwrap().cache_write_prefixes,
        writes_after_first,
        "the second group must not write a prefix the first already wrote"
    );
}
