//! Tests for audit repo.

use super::*;

// ── LlmPort seam: AI-failure guard (#audit-llm-seam-guard) ──────────────────────
//
// These tests exercise the load-bearing behavior that was previously untestable
// without a live model: when the LLM is UNAVAILABLE in an AI-review ("both") scan,
// every audit pass errors, and `audit_repo` must SURFACE that the AI review was
// skipped (return `Err`) — NEVER a silent clean Ok([]) — so the caller
// (`onboard::audit_repos`) records "AI audit skipped" while the deterministic floor
// findings still return independently. The `LlmPort` trait is the seam that lets a
// test substitute a model client without any token, env mutation, or network.

/// A `LlmPort` whose every call fails — simulates the LLM being unavailable
/// (CLI not installed, API key invalid, network down). Both `complete` and
/// `complete_streaming` return `Err`, so every audit pass that touches it fails.
pub(super) struct FailingCompleter;

#[async_trait::async_trait]
impl LlmPort for FailingCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("simulated LLM unavailable (complete)")
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("simulated LLM unavailable (complete_streaming)")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A `LlmPort` that returns canned text — proves the seam works in the OTHER
/// direction (a stub can feed the audit findings without a live model). The text is
/// the audit's expected JSON shape so `parse_ai_findings` yields a finding.
pub(super) struct StubCompleter {
    pub(super) text: String,
}

#[async_trait::async_trait]
impl LlmPort for StubCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: self.text.clone(),
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
        _req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        on_delta(&self.text);
        self.complete(LlmRequest::new("")).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// W2 UNIT regression for the zero-row semantic-tier collapse: a fake LLM reports ONE
/// security finding under a bare kebab name (no adopted corpus rule matches it — `selected`
/// is empty) and `audit_repo` must still return it, tagged `AI-`, rather than the model's
/// response being silently dropped anywhere in the chunk/merge/calibration chain. Narrower
/// and faster than the full `semantic_tier_cross_file_backstop_e2e.rs` integration test
/// (which also drives the finding through `report_export::build_report_json`) — this one
/// pins the contract at the `audit_repo` boundary alone.
#[tokio::test]
async fn audit_repo_with_no_adopted_rule_still_returns_the_models_own_security_finding() {
    let findings_json = r#"{
            "findings": [
                {
                    "path": "src/handlers/account.rs",
                    "line": 4,
                    "severity": "critical",
                    "rule": "missing-ownership-check-on-account-delete",
                    "title": "Any authenticated user can delete any account by id",
                    "code": "delete_account(req.account_id);",
                    "detail": "The handler never confirms req.account_id belongs to the caller before deleting it — no ownership predicate anywhere on this path.",
                    "captures": {}
                }
            ],
            "proposed_rules": [],
            "needs_files": []
        }"#;
    let stub = StubCompleter {
        text: findings_json.to_string(),
    };
    let files = vec![(
        "src/handlers/account.rs".to_string(),
        "fn delete(req: Req) {\n    delete_account(req.account_id);\n}\n".to_string(),
    )];
    // Empty `selected` — the whole point: no adopted rule exists for this defect, so it can
    // ONLY ever reach the architect via the semantic tier's own backstop reasoning.
    let selected: Vec<(String, String)> = Vec::new();

    let (findings, _proposed, _recs, failed_passes, _stage_samples) = audit_repo(
        &stub,
        "acme/app",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        Some(&files),
    )
    .await
    .expect("audit_repo must succeed against a well-formed stub response");

    assert_eq!(
        findings.len(),
        1,
        "the model's orphan security finding must survive to audit_repo's return value: \
             failed_passes={failed_passes:?}"
    );
    assert!(
        findings[0].rule_id.starts_with("AI-"),
        "an id with no adopted-rule match must be tagged AI-tier: {}",
        findings[0].rule_id
    );
    assert_eq!(findings[0].severity, "critical");
    assert_eq!(findings[0].path, "src/handlers/account.rs");
}

/// Records every `LlmRequest` it receives — used to inspect the ACTUAL prompt +
/// cache-breakpoint structure `run_passes` builds for a real multi-chunk,
/// multi-rule-batch scan (GAP-3 regression coverage), rather than only testing the pure
/// `build_pass_prompt` helper in isolation. Returns an empty-findings `"{}"` from every
/// call so `audit_pass`'s parse step is a harmless no-op.
#[derive(Default)]
pub(super) struct CapturingCompleter {
    pub(super) seen: std::sync::Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for CapturingCompleter {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.seen.lock().unwrap().push(req);
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

/// END-TO-END regression for GAP-3, exercising the REAL `run_passes` code path
/// (parallel mode) with 2 chunks × 2 rule-batches — not just the pure
/// `build_pass_prompt` helper. Confirms: (a) every request carries TWO cache
/// breakpoints (n_b == 2 > 1); (b) the bytes up to the FIRST breakpoint (the repo map)
/// are byte-identical across ALL FOUR requests, regardless of chunk or rule-batch —
/// this is what makes the repo map cache-hit across chunks, the core Gap-3 fix; (c) the
/// segment between the two breakpoints (chunk label + digest) is identical across a
/// chunk's two rule-batches but differs between chunks (the pre-existing parallel-mode
/// win, preserved); (d) rule content never leaks into a cached segment.
#[tokio::test]
async fn run_passes_produces_byte_identical_repo_map_prefix_across_chunks() {
    let completer = CapturingCompleter::default();
    let chunk0: Vec<(String, String)> = vec![(
        "chunk0.rs".to_string(),
        "// CHUNK0 marker\nfn chunk0() {}".to_string(),
    )];
    let chunk1: Vec<(String, String)> = vec![(
        "chunk1.rs".to_string(),
        "// CHUNK1 marker\nfn chunk1() {}".to_string(),
    )];
    let chunks: Vec<&[(String, String)]> = vec![&chunk0, &chunk1];

    let batch0: Vec<(String, String)> = vec![("RULE-A".to_string(), "rule A desc".to_string())];
    let batch1: Vec<(String, String)> = vec![("RULE-B".to_string(), "rule B desc".to_string())];
    let batches: Vec<&[(String, String)]> = vec![&batch0, &batch1];

    let repo_map =
        RepoMap::whole_text("REPO MAP:\nchunk0.rs: fn chunk0()\nchunk1.rs: fn chunk1()\n");
    let adopted = std::collections::HashSet::new();

    let (_findings, _proposed, _requested, ok, err) = run_passes(
        &completer,
        "acme/widgets",
        &repo_map,
        &crate::prompt_frame::FrameNonce::generate(),
        &adopted,
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        4,
        "parallel",
        "test-session",
        None,
        false,
    )
    .await;
    assert!(err.is_empty(), "no pass should fail: {err:?}");
    assert_eq!(ok, 4, "2 chunks x 2 rule-batches = 4 passes");

    let seen = completer.seen.lock().unwrap();
    assert_eq!(seen.len(), 4);

    // (a) Every request carries exactly two cache breakpoints (n_b == 2 > 1).
    for req in seen.iter() {
        assert_eq!(
            req.cache_breakpoints.len(),
            2,
            "n_b==2 keeps both breakpoints: {:?}",
            req.cache_breakpoints
        );
    }

    // (b) The repo-map segment (bytes up to the FIRST breakpoint) is byte-identical
    // across every one of the 4 requests, regardless of chunk or rule-batch.
    fn repo_map_segment(req: &LlmRequest) -> &str {
        &req.prompt[..req.cache_breakpoints[0]]
    }
    let first_segment = repo_map_segment(&seen[0]).to_string();
    for req in seen.iter() {
        assert_eq!(
            repo_map_segment(req),
            first_segment,
            "the repo-map segment must be byte-identical across every chunk/rule-batch"
        );
    }

    // (c) Bucket by chunk (via the CHUNK0/CHUNK1 marker in the digest segment) — within
    // a chunk, the digest segment (between the two breakpoints) is identical across its
    // two rule-batches; across chunks, it differs.
    fn digest_segment(req: &LlmRequest) -> &str {
        &req.prompt[req.cache_breakpoints[0]..req.cache_breakpoints[1]]
    }
    let chunk0_segs: Vec<&str> = seen
        .iter()
        .map(digest_segment)
        .filter(|s| s.contains("CHUNK0"))
        .collect();
    let chunk1_segs: Vec<&str> = seen
        .iter()
        .map(digest_segment)
        .filter(|s| s.contains("CHUNK1"))
        .collect();
    assert_eq!(chunk0_segs.len(), 2, "both rule-batches for chunk 0");
    assert_eq!(chunk1_segs.len(), 2, "both rule-batches for chunk 1");
    assert_eq!(
        chunk0_segs[0], chunk0_segs[1],
        "same chunk's digest segment is identical across rule-batches"
    );
    assert_eq!(chunk1_segs[0], chunk1_segs[1]);
    assert_ne!(
        chunk0_segs[0], chunk1_segs[0],
        "different chunks' digest segments differ"
    );

    // (d) Rule content never leaks into a cached segment — only the uncached suffix.
    for req in seen.iter() {
        let suffix = &req.prompt[req.cache_breakpoints[1]..];
        let cached = &req.prompt[..req.cache_breakpoints[1]];
        assert!(suffix.contains("RULE-A") || suffix.contains("RULE-B"));
        assert!(
            !cached.contains("RULE-A") && !cached.contains("RULE-B"),
            "rules must never sit inside a cached segment"
        );
    }
}

/// n_b == 1 (single rule-batch per chunk — batch mode's default
/// `BATCH_RULE_BATCH_SIZE`): only ONE cache breakpoint (the repo map) survives; the
/// chunk digest is folded into the uncached suffix rather than paying a cache-write
/// premium for a segment that's read back zero times.
#[tokio::test]
async fn run_passes_drops_the_digest_breakpoint_when_only_one_rule_batch() {
    let completer = CapturingCompleter::default();
    let chunk0: Vec<(String, String)> =
        vec![("chunk0.rs".to_string(), "fn chunk0() {}".to_string())];
    let chunk1: Vec<(String, String)> =
        vec![("chunk1.rs".to_string(), "fn chunk1() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![&chunk0, &chunk1];

    let batch0: Vec<(String, String)> = vec![("RULE-A".to_string(), "rule A desc".to_string())];
    let batches: Vec<&[(String, String)]> = vec![&batch0]; // n_b == 1

    let repo_map =
        RepoMap::whole_text("REPO MAP:\nchunk0.rs: fn chunk0()\nchunk1.rs: fn chunk1()\n");
    let adopted = std::collections::HashSet::new();

    let (_findings, _proposed, _requested, ok, err) = run_passes(
        &completer,
        "acme/widgets",
        &repo_map,
        &crate::prompt_frame::FrameNonce::generate(),
        &adopted,
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        4,
        "parallel",
        "test-session",
        None,
        false,
    )
    .await;
    assert!(err.is_empty(), "no pass should fail: {err:?}");
    assert_eq!(ok, 2, "2 chunks x 1 rule-batch = 2 passes");

    let seen = completer.seen.lock().unwrap();
    for req in seen.iter() {
        assert_eq!(
            req.cache_breakpoints.len(),
            1,
            "n_b==1 drops the digest breakpoint: {:?}",
            req.cache_breakpoints
        );
    }
}

/// GUARD: in an AI-review scan, when the LLM is unavailable and EVERY pass fails,
/// `audit_repo` SURFACES the failure (returns `Err`) instead of silently reporting a
/// clean Ok([]). This is the "never a silent clean" contract from `audit_repo`'s
/// `ok_passes == 0` branch. `feedback: None` drives the non-streaming `complete` path;
/// the streaming path is covered by `audit_repo_surfaces_ai_failure_streaming`.
#[tokio::test]
async fn audit_repo_surfaces_ai_failure_not_silent_clean() {
    let llm = FailingCompleter;
    // One source file + one semantic rule so there is real AI work to attempt
    // (an empty file set short-circuits to Ok before any pass runs).
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { db.execute(query); }\n".to_string(),
    )];
    let selected = vec![(
        "ARCH-NO-DIRECT-DB-1".to_string(),
        "Controllers must not call the database directly.".to_string(),
    )];
    let res = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[], // alternatives (none — no multi-option rules in this test)
        &std::collections::HashMap::new(), // forced
        None, // model
        None, // calibration model
        ScanMode::Parallel, // real-time path (the "both" AI-review path)
        false, // thorough
        None, // feedback -> non-streaming complete()
        None, // job
        None, // meter
        None, // map_files
    )
    .await;
    // (1) The AI failure is SURFACED, not swallowed into a clean result.
    let err = res.expect_err("all passes failed -> audit_repo must return Err, never Ok([])");
    // (2) It does not fabricate success; the error names the simulated unavailability.
    assert!(
        err.to_string().contains("simulated LLM unavailable"),
        "surfaced error should carry the underlying LLM failure, got: {err}"
    );
}

/// Same guard for the STREAMING path: an AI-review scan with a transcript store uses
/// `complete_streaming`, which also fails — `audit_repo` must still surface `Err`.
#[tokio::test]
async fn audit_repo_surfaces_ai_failure_streaming() {
    let llm = FailingCompleter;
    let store = crate::transcript::TranscriptStore::default();
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { db.execute(query); }\n".to_string(),
    )];
    let selected = vec![(
        "ARCH-NO-DIRECT-DB-1".to_string(),
        "Controllers must not call the database directly.".to_string(),
    )];
    let res = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        Some((&store, "job-1")), // feedback present -> streaming path
        None,
        None,
        None,
    )
    .await;
    let err = res.expect_err("streaming all-fail must surface Err, never a silent clean");
    assert!(
        err.to_string().contains("simulated LLM unavailable"),
        "streaming-path error should carry the underlying failure, got: {err}"
    );
}

/// A completer whose FIRST call comes back as a model refusal (`stop_reason: "refusal"`,
/// which the transport turns into an error via `refusal_error`) and whose later calls
/// answer `{"findings":[]}`. `refuse_all` makes every call refuse.
struct RefusingCompleter {
    calls: std::sync::atomic::AtomicUsize,
    refuse_all: bool,
    /// Also refuses the third call, which is the end-of-round second try of the pass the
    /// first call refused.
    refuse_second_try: bool,
}

impl RefusingCompleter {
    fn respond(&self) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.refuse_all || n == 0 || (self.refuse_second_try && n == 2) {
            let refusal = crate::llm::refusal_error(&serde_json::json!({
                "stop_reason": "refusal", "content": []
            }))
            .expect("a refusal stop_reason is an error");
            return Err(refusal);
        }
        Ok(LlmResponse {
            text: r#"{"findings":[]}"#.to_string(),
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
}

#[async_trait::async_trait]
impl LlmPort for RefusingCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.respond()
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.respond()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A model REFUSAL is a failed pass, never an empty finding list: when every pass is
/// refused the audit returns `Err` (it does not report a clean Ok([])).
#[tokio::test]
async fn audit_repo_treats_a_total_refusal_as_failure_not_a_clean_scan() {
    let llm = RefusingCompleter {
        calls: Default::default(),
        refuse_all: true,
        refuse_second_try: false,
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { db.execute(query); }\n".to_string(),
    )];
    let selected = vec![(
        "ARCH-NO-DIRECT-DB-1".to_string(),
        "Controllers must not call the database directly.".to_string(),
    )];
    let res = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await;
    let err = res.expect_err("every pass refused -> Err, never Ok([])");
    assert!(err.to_string().contains("refus"), "{err}");
}

/// One refused pass among several that answered: the survivors' findings are returned but
/// the gap is DISCLOSED in `failed_passes` (so the export is marked incomplete), not
/// swallowed.
#[tokio::test]
async fn audit_repo_discloses_a_single_refused_pass_as_a_failed_pass() {
    let llm = RefusingCompleter {
        calls: Default::default(),
        refuse_all: false,
        refuse_second_try: true,
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { db.execute(query); }\n".to_string(),
    )];
    // More rules than one rule-batch holds, so Parallel mode issues more than one pass.
    let selected: Vec<(String, String)> = (0..(RULE_BATCH_SIZE + 1))
        .map(|i| {
            (
                format!("ARCH-NO-DIRECT-DB-{i}"),
                "Controllers must not call the database directly.".to_string(),
            )
        })
        .collect();
    let (_findings, _proposed, _recs, failed_passes, _samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("some passes answered -> Ok with the gap disclosed");
    let rows: Vec<_> = failed_passes
        .iter()
        .filter(|f| f.reason.contains("refus"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "exactly the one refused pass is disclosed, by name: {failed_passes:?}"
    );
    assert!(
        rows[0].pass.starts_with("audit pass[all] "),
        "{}",
        rows[0].pass
    );
    assert!(
        rows[0]
            .reason
            .contains("1 of 2 passes in this round answered"),
        "the row says how many passes answered: {}",
        rows[0].reason
    );
}

/// Every failed pass gets its own named row, and the row carries how many passes answered,
/// so 2-of-40 and 39-of-40 are told apart.
#[test]
fn failed_pass_rows_name_every_failed_pass_and_the_round_size() {
    let failures: Vec<PassFailure> = (0..2)
        .map(|i| PassFailure::at("pass[rust]", i, 5, 0, 8, &"model refused (refusal)"))
        .collect();
    let rows = failed_pass_rows("me/api", 38, &failures);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].pass, "audit pass[rust] 1/5 · rules 1/8");
    assert_eq!(rows[1].pass, "audit pass[rust] 2/5 · rules 1/8");
    for r in &rows {
        assert_eq!(r.repo, "me/api");
        assert!(r.reason.contains("38 of 40 passes in this round answered"));
        assert!(r.reason.contains("refus"));
    }
    let all_but_one = failed_pass_rows("me/api", 1, &failures);
    assert!(all_but_one[0].reason.contains("1 of 3 passes"));
}

/// BATCH: a refused item's per-item reason reaches the failed pass. It must say it was a
/// refusal, not "missing from results".
#[test]
fn a_refused_batch_item_is_reported_as_a_refusal_not_as_missing() {
    let mut results = crate::llm::BatchResults::default();
    results.responses.insert(
        "c0-b0".to_string(),
        LlmResponse {
            text: r#"{"findings":[]}"#.to_string(),
            model: "m".to_string(),
            backend: "api/batch".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        },
    );
    results.errors.insert(
        "c0-b1".to_string(),
        crate::llm::refusal_error(&serde_json::json!({"stop_reason": "refusal"}))
            .unwrap()
            .to_string(),
    );
    // c0-b2 is in neither map: genuinely missing.
    let (_f, _p, _r, ok, failures) = reassemble_batch_passes(
        "me/api",
        "pass",
        1,
        3,
        &[(0, 0), (0, 1), (0, 2)],
        &[1],
        &results,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
        None,
        false,
        None,
    );
    assert_eq!(ok, 1);
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0].pass, "pass 1/1 · rules 2/3");
    assert!(failures[0].error.contains("refus"), "{}", failures[0].error);
    assert!(failures[1].error.contains("missing from results"));
    let rows = failed_pass_rows("me/api", ok, &failures);
    assert!(rows[0].reason.contains("refus"));
}

/// First call answers and asks for another file; every later call (the resolution round)
/// is refused.
struct NeedsFileThenRefuses {
    calls: std::sync::atomic::AtomicUsize,
}

impl NeedsFileThenRefuses {
    fn respond(&self) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n > 0 {
            return Err(crate::llm::refusal_error(&serde_json::json!({
                "stop_reason": "refusal"
            }))
            .unwrap());
        }
        Ok(LlmResponse {
            text: r#"{"findings":[],"needs_files":["src/b.rs"]}"#.to_string(),
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
}

#[async_trait::async_trait]
impl LlmPort for NeedsFileThenRefuses {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.respond()
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.respond()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// H2: a refused RESOLUTION pass yields zero findings for the files it was to re-judge;
/// that must be a disclosed failed pass, never "resolved clean".
#[tokio::test]
async fn a_refused_resolution_pass_is_disclosed_as_a_failed_pass() {
    let llm = NeedsFileThenRefuses {
        calls: Default::default(),
    };
    let files = vec![
        ("src/a.rs".to_string(), "fn a() {}\n".to_string()),
        (
            "src/b.rs".to_string(),
            "fn b() { db.execute(q); }\n".to_string(),
        ),
    ];
    let selected = vec![(
        "ARCH-NO-DIRECT-DB-1".to_string(),
        "Controllers must not call the database directly.".to_string(),
    )];
    let (_findings, _proposed, _recs, failed_passes, _samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("the main round answered, so the audit returns");
    assert!(
        failed_passes
            .iter()
            .any(|f| f.pass.starts_with("audit resolution") && f.reason.contains("refus")),
        "the refused resolution pass must be disclosed: {failed_passes:?}"
    );
}

/// HAPPY PATH (seam works both ways): a `StubCompleter` returning a canned finding lets
/// `audit_repo` complete WITHOUT a live model and yields the parsed finding — proving
/// the trait substitution is faithful, not just an error sink.
#[tokio::test]
async fn audit_repo_with_stub_completer_returns_findings() {
    // Minimal audit-JSON the parser recognizes: one finding under the adopted rule.
    let canned = r#"{"findings":[{"rule":"ARCH-NO-DIRECT-DB-1","severity":"high","path":"src/lib.rs","code":"db.execute(query)","title":"direct DB call","detail":"controller calls the database directly"}]}"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { db.execute(query); }\n".to_string(),
    )];
    let selected = vec![(
        "ARCH-NO-DIRECT-DB-1".to_string(),
        "Controllers must not call the database directly.".to_string(),
    )];
    let (findings, _proposed, _recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors -> audit_repo returns Ok");
    // The stub's canned finding survived parse + calibration (also stub-served).
    assert!(
        findings.iter().any(|f| f.rule_id == "ARCH-NO-DIRECT-DB-1"),
        "stub-served finding must survive the audit pipeline: {findings:?}"
    );
}

// ---- Audit-integrated alternative recommendation (2026-09-22 design) ----------------

pub(super) fn two_option_alternatives(rule_id: &str, selected: Option<&str>) -> RuleAlternatives {
    RuleAlternatives {
        rule_id: rule_id.to_string(),
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
        selected_option_id: selected.map(str::to_string),
        // Fixtures default to a pure decision rule — the kind under which every
        // alternatives-pass outcome (including no-convention) is reachable.
        kind: Some(camerata_rules::RuleKind::Decision),
    }
}

/// req_a5_4 fixture: the SAME two-option shape as [`two_option_alternatives`], except
/// `opt-b` is marked `violates: true` — a genuine `default_with_exceptions`/`defect`-shaped
/// rule whose non-default option IS the degenerate/absent form of the requirement (e.g.
/// `ARCH-IDEMPOTENCY-KEYS-1`'s "no deduplication at all" option). Used by the tests proving
/// A5.4: selecting this option must emit a finding and must NOT rewrite the directive.
fn two_option_alternatives_opt_b_violates(
    rule_id: &str,
    selected: Option<&str>,
) -> RuleAlternatives {
    let mut alt = two_option_alternatives(rule_id, selected);
    alt.options[1].violates = true;
    alt
}

#[test]
fn build_alternatives_block_includes_every_option_and_marks_the_selected_one() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let block = build_alternatives_block(&[alt]);
    assert!(block.contains("MULTI-RULE-1"));
    assert!(
        block.contains("opt-a"),
        "every option id must be present: {block}"
    );
    assert!(
        block.contains("opt-b"),
        "every option id must be present: {block}"
    );
    assert!(
        block.contains("Do it the A way."),
        "every directive must be present: {block}"
    );
    assert!(
        block.contains("Do it the B way."),
        "every directive must be present: {block}"
    );
    assert!(
        block.contains("why: A is simple."),
        "every rationale must be present: {block}"
    );
    // The marker sits on the SELECTED option's line, not the other one.
    let a_line = block.lines().find(|l| l.contains("opt-a")).unwrap();
    let b_line = block.lines().find(|l| l.contains("opt-b")).unwrap();
    assert!(
        a_line.contains("CURRENTLY SELECTED"),
        "selected option must be marked: {a_line}"
    );
    assert!(
        !b_line.contains("CURRENTLY SELECTED"),
        "unselected option must not be marked: {b_line}"
    );
}

#[test]
fn build_alternatives_block_marks_none_selected_when_nothing_is_chosen() {
    let alt = two_option_alternatives("MULTI-RULE-1", None);
    let block = build_alternatives_block(&[alt]);
    assert!(
        block.contains("no option currently selected"),
        "must state explicitly that nothing is selected: {block}"
    );
    assert!(!block.contains("CURRENTLY SELECTED"));
}

/// W19 — provenance must state the model that RAN, not the one the caller happened to
/// name. Three copies of this resolution existed and what reached provenance was none of
/// them: it was the caller's raw pick, so a run driven by `CAMERATA_AUDIT_MODEL` or by
/// the default recorded `None`.
///
/// That ambiguity is what made the 2026-10-16 five-repo sweep unfalsifiable. Its baseline
/// recorded `claude-sonnet-5` and both later runs recorded nothing, so "did the engine
/// regress" could be neither confirmed nor refuted, and a week went into a regression
/// that did not exist.
/// W31 — a row the engine thinks MIGHT be deliberate is capped at low, by construction.
///
/// Some faults cannot be told apart from a deliberate choice by reading code. A public
/// storage bucket is the canonical case: nothing in a repository separates a
/// catalog-photo bucket that is public on purpose from a user-document bucket that is
/// public by accident, and inferring it from the bucket's NAME is the prose-matching this
/// effort has been removing everywhere else.
///
/// The engine already knew, and said so in `detail` prose ("very likely intentional").
/// Prose is unreadable to every severity pass, ranker and renderer, so three such rows
/// shipped at HIGH, held, each carrying text that undercut its own severity. The flag
/// makes the same judgement structural, and the cap removes that contradiction by
/// construction rather than asking a renderer to notice it.
/// W24 severity anchor — a test-coverage rule never ships at CRITICAL.
///
/// Critical in this report means an unauthenticated exposure or a full compromise.
/// Missing coverage is real work and belongs in the findings, but it was never the thing
/// an attacker exploited, and ranking it beside "anyone can read every row" spends the
/// client's alarm on the wrong row.
/// D7 — the `Rejected:` authoring token never reaches the client.
///
/// A `violates` option's `why` is interpolated VERBATIM into the defect detail, directly
/// after the label. The corpus's strongest authoring convention asks that `why` to open
/// with the literal token `Rejected:` — and 71 of 377 flagged options do — so the client
/// currently reads "…was judged to follow "X" for RULE-1 — Rejected: this conflates…".
///
/// A build-time authoring convention leaking into a paid deliverable is the same
/// internal-vocabulary defect as the detector-voice labels, in a field nobody looked at.
/// Stripping it at the render boundary is the owner's ruling (2026-10-19) because it
/// costs one function and leaves the 71 existing uses correct, where a corpus migration
/// would have touched 306 options to remove a token the guide elsewhere calls a wart.
#[test]
fn req_d7_the_rejected_token_is_stripped_before_the_client_sees_it() {
    // The token, with the sentence after it capitalised as an author would write it.
    assert_eq!(
        strip_authoring_token(
            "Rejected: this conflates a performance optimisation with a security defect."
        ),
        "This conflates a performance optimisation with a security defect.",
        "the token goes, and the clause after it still opens like a sentence"
    );
    // Case and spacing as actually authored across the corpus.
    assert_eq!(
        strip_authoring_token("Rejected:  already lower case here."),
        "Already lower case here."
    );
    assert_eq!(
        strip_authoring_token("rejected: lowercase token"),
        "Lowercase token"
    );

    // `Avoided:` is the same leak by another author's hand - found in
    // PYTHON-TESTING-UNIT-LAYOUT-1 / scattered during the 2026-10-20 rationale pass. A
    // token list rather than one token, because the next one will not be either of these.
    assert_eq!(
        strip_authoring_token("Avoided: makes coverage reporting harder."),
        "Makes coverage reporting harder."
    );

    // A `why` that never used the convention is untouched - 306 of 377 options.
    let plain = "Each record triggers its own query, so latency grows with the result set.";
    assert_eq!(strip_authoring_token(plain), plain);

    // And the word must only be stripped as an OPENING TOKEN. A `why` that legitimately
    // discusses something being rejected keeps its prose.
    let mentions = "The request is rejected: the server returns 403 before touching the row.";
    assert_eq!(strip_authoring_token(mentions), mentions);
}

#[test]
fn req_w24_a_test_coverage_rule_is_capped_below_critical() {
    let capped = |rule: &str, sev: &str| {
        let f = Finding {
            rule_id: rule.to_string(),
            severity: sev.to_string(),
            ..Finding::default()
        };
        apply_severity_ceiling_rules(vec![f]).remove(0)
    };

    let out = capped("JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1", "critical");
    assert_eq!(
        out.severity, "high",
        "a test rule must not ship at critical"
    );
    assert!(
        out.calibration_rationale
            .as_deref()
            .is_some_and(|r| r.contains("test-coverage")),
        "the demotion must explain itself: {:?}",
        out.calibration_rationale
    );

    // Cap-only: a genuinely serious coverage gap still outranks ordinary debt, so High
    // and below are untouched. The anchor removes the top rung, not the ranking.
    for sev in ["high", "medium", "low", "info"] {
        assert_eq!(
            capped("CSHARP-TESTING-AAA-STRUCTURE-1", sev).severity,
            sev,
            "the anchor caps critical only; {sev} must pass through"
        );
    }

    // A real exposure still reaches critical — the anchor must not be a blanket demotion.
    assert_eq!(
        capped("SUPABASE-RLS-ENABLED-1", "critical").severity,
        "critical"
    );

    // Segment-anchored, so a rule that merely CONTAINS the letters is unaffected. The D5
    // floor bug raised anything containing "urls" because it also contains "rls"; this
    // is the same trap one rule over.
    assert!(!crate::is_test_coverage_class("SEC-NO-ATTESTING-TOKEN-1"));
    assert!(crate::is_test_coverage_class(
        "GO-TESTING-BLACK-BOX-PKG-TEST-1"
    ));
}

#[test]
fn req_w31_a_could_be_intentional_row_is_capped_at_low() {
    let cap = |sev: &str| {
        let f = Finding {
            rule_id: "SUPABASE-STORAGE-PUBLIC-BUCKET-1".to_string(),
            severity: sev.to_string(),
            could_be_intentional: true,
            ..Finding::default()
        };
        apply_severity_ceiling_rules(vec![f]).remove(0)
    };

    for sev in ["critical", "high", "medium"] {
        let out = cap(sev);
        assert_eq!(
            out.severity, "low",
            "a {sev} row the engine flagged as possibly deliberate must not ship at {sev}"
        );
        assert!(
            out.calibration_rationale
                .as_deref()
                .is_some_and(|r| r.to_lowercase().contains("intentional")),
            "the cap must say WHY, or a reader sees an unexplained demotion: {:?}",
            out.calibration_rationale
        );
    }

    // Cap, not clamp. An INFO row stays info — raising it would be inventing severity for
    // a row we are explicitly unsure about.
    assert_eq!(cap("info").severity, "info");
    assert_eq!(cap("low").severity, "low");

    // The converse, so the cap cannot be satisfied by demoting everything: an unflagged
    // row of the same rule keeps its authored severity.
    let unflagged = Finding {
        rule_id: "SUPABASE-STORAGE-PUBLIC-BUCKET-1".to_string(),
        severity: "high".to_string(),
        ..Finding::default()
    };
    assert_eq!(
        apply_severity_ceiling_rules(vec![unflagged])
            .remove(0)
            .severity,
        "high",
        "an unflagged row must keep its authored severity"
    );
}

#[test]
fn req_w19_resolve_audit_model_prefers_the_caller_then_env_then_the_default() {
    // The caller's explicit pick always wins.
    assert_eq!(
        resolve_audit_model(Some("claude-sonnet-5")),
        "claude-sonnet-5"
    );

    // Blank is not a pick. Falling through on an empty string is what makes
    // `--model ""` behave like omitting the flag rather than selecting nothing.
    assert_eq!(resolve_audit_model(Some("   ")), crate::llm::DEFAULT_MODEL);

    // No pick and no env: the compiled-in default, NAMED rather than left as None.
    // This is the case that used to record nothing.
    assert_eq!(resolve_audit_model(None), crate::llm::DEFAULT_MODEL);
    assert!(
        !resolve_audit_model(None).trim().is_empty(),
        "the resolved model is always a real name - an empty string here would \
             reintroduce the ambiguity this function exists to remove"
    );
}

#[test]
fn parse_alternative_recommendations_accepts_a_real_option_id() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-b","recommendation_reasoning":"the codebase already does it the B way"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].rule_id, "MULTI-RULE-1");
    assert_eq!(recs[0].recommended_option_id, "opt-b");
    assert_eq!(
        recs[0].recommendation_reasoning,
        "the codebase already does it the B way"
    );
    assert!(!recs[0].hallucinated);
    assert!(!recs[0].operator_chosen);
}

#[test]
fn parse_alternative_recommendations_rejects_a_hallucinated_option_id() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-does-not-exist","recommendation_reasoning":"bogus"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert_eq!(recs.len(), 1);
    // Falls back to the currently-selected option, never the hallucinated id.
    assert_eq!(recs[0].recommended_option_id, "opt-a");
    assert!(
        recs[0].hallucinated,
        "a non-real option id must be flagged hallucinated"
    );
    assert!(
        recs[0]
            .recommendation_reasoning
            .contains("opt-does-not-exist"),
        "the correction note should name the bogus id: {}",
        recs[0].recommendation_reasoning
    );
}

#[test]
fn parse_alternative_recommendations_falls_back_to_the_first_option_when_nothing_is_selected() {
    let alt = two_option_alternatives("MULTI-RULE-1", None);
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"nonsense","recommendation_reasoning":""}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert_eq!(
        recs[0].recommended_option_id, "opt-a",
        "first option is the last-resort fallback"
    );
    assert!(recs[0].hallucinated);
}

#[test]
fn parse_alternative_recommendations_falls_back_when_a_rule_is_missing_from_the_response() {
    let alts = vec![
        two_option_alternatives("MULTI-RULE-1", Some("opt-a")),
        two_option_alternatives("MULTI-RULE-2", Some("opt-b")),
    ];
    // The model only answered rule 1 — rule 2 must still get a fallback entry, never be
    // silently dropped (every listed rule gets exactly one output entry).
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","recommendation_reasoning":"fine as-is"}]}"#;
    let recs = parse_alternative_recommendations(raw, &alts);
    assert_eq!(
        recs.len(),
        2,
        "every listed rule must get exactly one entry"
    );
    let r2 = recs.iter().find(|r| r.rule_id == "MULTI-RULE-2").unwrap();
    assert_eq!(
        r2.recommended_option_id, "opt-b",
        "falls back to its own selected option"
    );
    assert!(r2.hallucinated);
}

#[test]
fn parse_alternative_recommendations_degrades_on_malformed_json_without_panicking() {
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    for raw in ["not json at all", "", "{ not valid json ]"] {
        let recs = parse_alternative_recommendations(raw, &alts);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].recommended_option_id, "opt-a");
        assert!(recs[0].hallucinated);
    }
}

// ── P7: evidence-gated applicability ────────────────────────────────────────────────

/// A genuine, evidenced pick: `applicable: true` WITH a real evidence line is trusted
/// as-is — `applicable` stays true, `evidence` is carried through, and this is NOT
/// `hallucinated` (a real, evidenced answer, not a correction).
#[test]
fn parse_alternative_recommendations_accepts_an_evidenced_applicable_pick() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":true,"recommended_option_id":"opt-b","evidence":"src/api/list_users.rs:42 — cursor param already threaded through","recommendation_reasoning":"the codebase already does it the B way"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert_eq!(recs.len(), 1);
    assert!(recs[0].applicable);
    // The evidence is carried through, with the model's em dash written as a comma on the way in
    // (a client reads `path:line, note`; `parse_evidence_citation` reads the place out of it).
    assert_eq!(
        recs[0].evidence.as_deref(),
        Some("src/api/list_users.rs:42, cursor param already threaded through")
    );
    assert_eq!(recs[0].recommended_option_id, "opt-b");
    assert!(
        !recs[0].hallucinated,
        "an evidenced applicable pick is not a hallucination"
    );
}

/// THE CORE P7 GUARANTEE: a model claiming `applicable: true` but citing NO evidence line
/// must never be trusted at face value — it is corrected to `applicable: false` (no
/// findings will be checked against it) and flagged `hallucinated: true` so the UI can show
/// the correction happened. This is what stops the report from asserting "the adopted
/// offset-pagination model" when nothing in the repo actually establishes any convention.
#[test]
fn parse_alternative_recommendations_corrects_an_unevidenced_applicable_claim() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":true,"recommended_option_id":"opt-b","recommendation_reasoning":"the codebase adopts the B model"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert_eq!(recs.len(), 1);
    assert!(
        !recs[0].applicable,
        "an unevidenced applicability claim must be corrected to not-applicable"
    );
    assert_eq!(recs[0].evidence, None);
    assert!(recs[0].hallucinated, "the correction must be flagged");
    assert!(
        recs[0].recommendation_reasoning.contains("did not cite"),
        "the correction note must explain WHY it was downgraded: {}",
        recs[0].recommendation_reasoning
    );
}

/// An empty-string `evidence` field is treated identically to an absent one — never
/// accepted as "evidence" just because the key was present.
#[test]
fn parse_alternative_recommendations_rejects_blank_evidence_as_no_evidence() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":true,"recommended_option_id":"opt-b","evidence":"   ","recommendation_reasoning":"vague"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert!(!recs[0].applicable);
    assert_eq!(recs[0].evidence, None);
    assert!(recs[0].hallucinated);
}

/// The model explicitly saying `applicable: false` (no evidence anywhere for the concern)
/// is trusted as a genuine "not applicable" answer — NOT flagged hallucinated, since the
/// model correctly declined to assert an unevidenced convention.
#[test]
fn parse_alternative_recommendations_accepts_an_explicit_not_applicable() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":false,"recommendation_reasoning":"no pagination anywhere in this codebase"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert!(!recs[0].applicable);
    assert_eq!(recs[0].evidence, None);
    assert!(
        !recs[0].hallucinated,
        "an honest, explicit not-applicable answer is not a hallucination"
    );
    assert_eq!(
        recs[0].recommendation_reasoning,
        "no pagination anywhere in this codebase"
    );
}

/// Back-compat: a response that omits the `applicable` key entirely (pre-P7 shape) is NOT
/// penalized — it fails OPEN to `applicable: true` with no evidence requirement, exactly
/// the pre-existing behavior. Only an EXPLICIT `true` claim is evidence-gated.
#[test]
fn parse_alternative_recommendations_treats_absent_applicable_key_as_back_compat_applicable() {
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-b","recommendation_reasoning":"fine"}]}"#;
    let recs = parse_alternative_recommendations(raw, &[alt]);
    assert!(recs[0].applicable);
    assert!(!recs[0].hallucinated);
}

/// A fallback recommendation (total parse failure, or a rule the model never answered) is
/// ALWAYS `applicable: true` — a pipeline failure is not evidence the rule's concern is
/// inapplicable, so the rule keeps being checked exactly as before P7.
#[test]
fn fallback_recommendations_are_always_applicable() {
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let recs = parse_alternative_recommendations("not json at all", &alts);
    assert!(recs[0].applicable);
    assert_eq!(recs[0].evidence, None);

    let alts2 = vec![
        two_option_alternatives("MULTI-RULE-1", Some("opt-a")),
        two_option_alternatives("MULTI-RULE-2", Some("opt-b")),
    ];
    let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","recommendation_reasoning":"fine"}]}"#;
    let recs2 = parse_alternative_recommendations(raw, &alts2);
    let missing = recs2.iter().find(|r| r.rule_id == "MULTI-RULE-2").unwrap();
    assert!(
        missing.applicable,
        "a rule the model never answered stays applicable"
    );
}

/// HAPPY PATH end-to-end: `audit_repo` fed one multi-option rule (via `alternatives`)
/// recommends an option AND tags the finding under that rule with `evaluated_option_id`
/// — one scan, two outputs per rule, per the design doc. The `StubCompleter` serves the
/// SAME canned JSON to every call (the recommendation pass, the violation pass, and the
/// no-op calibration pass); the response carries both a `recommendations` array (read by
/// the recommendation pass) and a `findings` array (read by the violation pass) — each
/// pass reads only the key it cares about, so one canned response drives the whole
/// pipeline deterministically.
#[tokio::test]
async fn audit_repo_tags_findings_with_the_recommended_option_for_a_multi_option_rule() {
    let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "recommended_option_id": "opt-b", "recommendation_reasoning": "the repo already does it the B way"}
            ],
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "medium", "path": "src/lib.rs", "code": "old pattern here", "title": "does it the A way", "detail": "should be B"}
            ],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { old pattern here; }\n".to_string(),
    )];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(), // forced — none, this is the "ask the model" path
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].rule_id, "MULTI-RULE-1");
    assert_eq!(recs[0].recommended_option_id, "opt-b");
    assert!(!recs[0].operator_chosen);

    let f = findings
        .iter()
        .find(|f| f.rule_id == "MULTI-RULE-1")
        .expect("the stub's canned finding must survive the pipeline");
    assert_eq!(
        f.evaluated_option_id.as_deref(),
        Some("opt-b"),
        "the finding must be tagged with the option it was actually judged under: {f:?}"
    );
}

/// P7 CORE GUARANTEE, end-to-end: when the recommendation pass marks a rule NOT
/// APPLICABLE (no evidence its concern exists in this repo at all), `audit_repo` emits
/// ZERO findings for it — even though the stub's canned response includes a "finding"
/// under that exact rule id (proving the hard post-filter works, not merely that the rule
/// was left out of the prompt, which a stub ignores anyway).
#[tokio::test]
async fn audit_repo_emits_no_findings_for_a_not_applicable_rule() {
    let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "applicable": false, "recommendation_reasoning": "no evidence of pagination anywhere in this codebase"}
            ],
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "medium", "path": "src/lib.rs", "code": "old pattern here", "title": "does it the A way", "detail": "should be B"}
            ],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { old pattern here; }\n".to_string(),
    )];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    assert_eq!(recs.len(), 1);
    assert!(!recs[0].applicable);
    // A10: the stray the model cited anyway is KEPT, marked thrown out, and apart from every
    // row that ships: it is never a finding of the report.
    let (thrown, shipped) = crate::engine_discard::split_discarded(findings);
    assert!(
        shipped.iter().all(|f| f.rule_id != "MULTI-RULE-1"),
        "a not-applicable rule must emit ZERO findings, even one the model cited anyway: \
             {shipped:?}"
    );
    assert_eq!(thrown.len(), 1, "{thrown:?}");
    assert_eq!(
        crate::engine_discard::reason_of(&thrown[0]),
        Some(crate::engine_discard::RULE_NOT_APPLICABLE)
    );
}

/// FORCED (operator-chosen) mode, the `rescan-alternatives` path: when the rule id is
/// present in `forced`, `audit_repo` does NOT ask the model to recommend — it rewrites the
/// rule's directive straight to the forced option and tags findings with that id, marking
/// the recommendation `operator_chosen: true`. The stub's response has NO `recommendations`
/// key at all, proving the recommendation pass genuinely never ran (there is nothing for it
/// to parse) — if it had run, the fallback path would still produce a result, so the
/// stronger assertion here is `operator_chosen: true` with the EXACT forced id, unmodified.
#[tokio::test]
async fn audit_repo_forced_mode_skips_recommendation_and_tags_the_operator_choice() {
    let canned = r#"{
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "medium", "path": "src/lib.rs", "code": "old pattern here", "title": "does it the A way", "detail": "should be B"}
            ],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { old pattern here; }\n".to_string(),
    )];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let mut forced = std::collections::HashMap::new();
    forced.insert("MULTI-RULE-1".to_string(), "opt-b".to_string());

    let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &forced,
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].recommended_option_id, "opt-b");
    assert!(
        recs[0].operator_chosen,
        "a forced pick must be marked operator_chosen"
    );
    assert!(
        !recs[0].hallucinated,
        "a real forced option id is not a hallucination"
    );

    let f = findings
        .iter()
        .find(|f| f.rule_id == "MULTI-RULE-1")
        .unwrap();
    assert_eq!(f.evaluated_option_id.as_deref(), Some("opt-b"));
}

/// A forced option id that is NOT real on the rule is rejected (never silently trusted) —
/// falls back to the rule's currently-selected option and is flagged `hallucinated`.
#[tokio::test]
async fn audit_repo_forced_mode_rejects_an_invalid_forced_option_id() {
    let canned = r#"{"findings": [], "proposed_rules": []}"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![("src/lib.rs".to_string(), "fn handler() {}\n".to_string())];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let mut forced = std::collections::HashMap::new();
    forced.insert("MULTI-RULE-1".to_string(), "opt-does-not-exist".to_string());

    let (_findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &forced,
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    assert_eq!(recs.len(), 1);
    assert_eq!(
        recs[0].recommended_option_id, "opt-a",
        "falls back to the selected option"
    );
    assert!(recs[0].hallucinated);
    assert!(recs[0].operator_chosen);
}

/// e2e-style (design doc §Testing): a scan over a fixture with TWO multi-option rules —
/// one WITH a currently-selected/default option, one WITHOUT (the "no default, architect
/// never chose" case the removed gate used to block on) — yields a recommendation AND
/// violations-under-the-recommended-option for BOTH, in one pass. Proves the no-default
/// rule is handled identically to the with-default one: it simply gets a recommendation
/// instead of blocking.
#[tokio::test]
async fn audit_repo_e2e_two_multi_option_rules_one_with_default_one_without() {
    let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-WITH-DEFAULT-1", "recommended_option_id": "opt-a", "recommendation_reasoning": "matches existing pattern"},
                {"rule_id": "MULTI-NO-DEFAULT-1", "recommended_option_id": "opt-b", "recommendation_reasoning": "best fit despite no prior default"}
            ],
            "findings": [
                {"rule": "MULTI-WITH-DEFAULT-1", "severity": "medium", "path": "src/lib.rs", "line": 2, "code": "thing one;", "title": "t1", "detail": "d1"},
                {"rule": "MULTI-NO-DEFAULT-1", "severity": "low", "path": "src/lib.rs", "line": 3, "code": "thing two;", "title": "t2", "detail": "d2"}
            ],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    // The two findings sit on DIFFERENT lines so the cross-rule location-merge pass
    // (`collapse_identical`, which fuses same-(path,line) findings into one row) doesn't
    // collapse them into a single finding — this test wants two distinct rows to assert
    // each one's own `evaluated_option_id` independently.
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() {\n    thing one;\n    thing two;\n}\n".to_string(),
    )];
    let selected = vec![
        (
            "MULTI-WITH-DEFAULT-1".to_string(),
            "Do it the A way.".to_string(),
        ),
        (
            "MULTI-NO-DEFAULT-1".to_string(),
            "Do it the A way.".to_string(),
        ),
    ];
    let alternatives = vec![
        // Has a currently-selected option (the corpus default, in the real pipeline).
        two_option_alternatives("MULTI-WITH-DEFAULT-1", Some("opt-a")),
        // No default and never chosen — `selected_option_id: None` is exactly the state
        // the removed "must choose an alternative" gate used to block on.
        two_option_alternatives("MULTI-NO-DEFAULT-1", None),
    ];
    let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("no-default rule must not block or error the scan");

    assert_eq!(recs.len(), 2, "both rules must get a recommendation");
    let with_default = recs
        .iter()
        .find(|r| r.rule_id == "MULTI-WITH-DEFAULT-1")
        .unwrap();
    let no_default = recs
        .iter()
        .find(|r| r.rule_id == "MULTI-NO-DEFAULT-1")
        .unwrap();
    assert_eq!(with_default.recommended_option_id, "opt-a");
    assert_eq!(
        no_default.recommended_option_id, "opt-b",
        "a rule with no prior default must still get a real recommendation, not block"
    );
    assert!(!with_default.hallucinated);
    assert!(!no_default.hallucinated);

    for (rule_id, expected_option) in [
        ("MULTI-WITH-DEFAULT-1", "opt-a"),
        ("MULTI-NO-DEFAULT-1", "opt-b"),
    ] {
        let f = findings
            .iter()
            .find(|f| f.rule_id == rule_id)
            .unwrap_or_else(|| panic!("missing finding for {rule_id}: {findings:?}"));
        assert_eq!(
            f.evaluated_option_id.as_deref(),
            Some(expected_option),
            "{rule_id}'s finding must be tagged under its recommended option"
        );
    }
}

// ---- Pass-1 W2 (A5.4): the alternatives pass may never silence a rule -----------------
// (the audit findings tracker)

/// req_a5_4: selecting an option marked `violates` must NOT rewrite the rule's effective
/// directive, and must emit a `Finding` carrying the rule's citation (`rule_id`) and
/// remediation (`evaluated_option_id`, which `report_export::resolve_fix` resolves).
///
/// FAILS BEFORE THE FIX: before this change, `apply_alternative_decision` didn't exist —
/// both of `audit_repo`'s alternatives branches unconditionally rewrote the directive to
/// ANY decided option's text (`entry.1 = opt.directive.clone()`, with no `violates` check
/// at all) and never built a finding. Run against that shape, this test's first assertion
/// (`entry.1` unchanged) would fail — the directive WOULD have been rewritten to opt-b's —
/// and the second assertion (`Some(finding)`) would fail too, since nothing was ever
/// returned.
#[test]
fn req_a5_4_violates_option_does_not_rewrite_directive_and_emits_a_finding() {
    let mut effective_selected = vec![crate::scan_routing::AuditRule::new(
        "MULTI-RULE-1",
        "Do it the A way.",
    )];
    let alt = two_option_alternatives_opt_b_violates("MULTI-RULE-1", Some("opt-a"));
    let opt_b = alt.options.iter().find(|o| o.id == "opt-b").unwrap();
    assert!(
        opt_b.violates,
        "fixture sanity: opt-b must be the violates option"
    );

    let finding = apply_alternative_decision(
        &mut effective_selected,
        "me/api",
        "MULTI-RULE-1",
        opt_b,
        "the repo consistently does it the B way",
        Some("src/lib.rs:42 - does it the B way with no exception handling"),
    );

    assert_eq!(
        effective_selected[0].directive, "Do it the A way.",
        "a violates option must NEVER rewrite the rule's effective directive"
    );
    let f = finding.expect("a violates option must emit a finding");
    assert_eq!(
        f.rule_id, "MULTI-RULE-1",
        "the finding carries the rule's own citation"
    );
    assert_eq!(
        f.evaluated_option_id.as_deref(),
        Some("opt-b"),
        "remediation resolution (report_export::resolve_fix) keys off this"
    );
    assert_eq!(f.path, "src/lib.rs");
    assert_eq!(f.line, 42);
    assert!(f.located);
}

/// req_w12: a REAL security rule from the shipped corpus, given a codebase that matches
/// the degenerate option its own author marked `Rejected:`, must REPORT that as a defect —
/// never record it as "the convention this codebase chose".
///
/// This is the end-to-end proof of the W12 defect, driven through the real corpus and the
/// real alternatives-pass code path rather than a synthetic fixture.
/// `SUPABASE-STORAGE-KEY-SANITIZATION-1` is the exemplar because it is `structured`-tier
/// and not gate-armed, so it genuinely ENTERED the alternatives pass before this change
/// (the `mechanical`/`architectural` members of the W12 set were already excluded from
/// that pass by `EnforcementKind::is_ci_enforced`, so their silence had a different
/// cause), and because its rejected option describes a real, unsafe CODEBASE state —
/// keeping the visitor's own filename as the storage object key behind a `../` blocklist.
///
/// Three things are asserted, which together are the whole claim:
///
/// 1. The rule is no longer ELIGIBLE for the alternatives pass at all
///    (`is_semantic_multi_option_rule`), because a `defect`-kind rule has nothing to
///    decide among — so its directive can never be replaced by its own degenerate option.
/// 2. Fed that degenerate option through the alternatives-decision chokepoint anyway, it
///    EMITS a `FindingKind::Defect` and leaves the effective directive untouched.
/// 3. It produces NO `ConventionObserved` row — the literal "reported as a consistently
///    wrong thing in the code" surface the owner named.
///
/// FAILS BEFORE THE FIX: the rule was `kind = "decision"` with ZERO `violates` options, so
/// (1) it WAS eligible for the alternatives pass, (2) `apply_alternative_decision` took
/// its `!option.violates` branch — returning `None` (no finding) and REWRITING the
/// effective directive to "strip only `../` and keep the client filename", i.e. adopting
/// the vulnerability as the project's directive — and (3) with no violation trace left to
/// find, `conventions_observed_findings` emitted a `ConventionObserved` row asserting the
/// codebase consistently follows this "convention". All three assertions failed.
#[tokio::test]
async fn req_w12_corpus_security_rule_reports_its_rejected_option_instead_of_recording_a_convention(
) {
    const RULE_ID: &str = "SUPABASE-STORAGE-KEY-SANITIZATION-1";

    let corpus_dir = camerata_rules::corpus_path();
    if !corpus_dir.exists() {
        return; // skip without the bundled corpus checked out
    }
    let corpus = camerata_rules::load_corpus(&corpus_dir)
        .await
        .expect("corpus loads");
    let rule = corpus
        .get_by_id(RULE_ID)
        .expect("the W12 exemplar rule must exist in the shipped corpus");

    // (1) A defect-kind rule is never offered to the model as a choice.
    assert!(
        !crate::onboard::is_semantic_multi_option_rule(rule),
        "{RULE_ID} must be excluded from the alternatives pass — a defect-kind rule has no \
             acceptable alternative to decide among, so its directive must never be \
             replaceable by its own degenerate option"
    );

    // The option this rule's own author marked `Rejected:` — located from the corpus text,
    // not hardcoded, so the test tracks the rule rather than a snapshot of it.
    let rejected = rule
        .options
        .iter()
        .find(|o| o.why.trim_start().starts_with("Rejected:"))
        .expect("the W12 exemplar must still carry exactly one `Rejected:` option");
    assert!(
        rejected.violates,
        "the `Rejected:` option ({}) must be marked `violates` — it describes keeping the \
             client's filename as the storage key, which is the defect this rule exists to \
             report, not an alternative a project may adopt",
        rejected.id
    );

    // (2) Fed the degenerate option, the chokepoint emits instead of rewriting.
    let correct_directive = rule
        .options
        .iter()
        .find(|o| !o.violates)
        .expect("the rule must still have a non-degenerate option")
        .directive
        .clone();
    let mut effective_selected = vec![crate::scan_routing::AuditRule::new(
        RULE_ID,
        correct_directive.clone(),
    )];
    let rejected_view = RuleOptionView {
        id: rejected.id.clone(),
        label: rejected.label.clone(),
        directive: rejected.directive.clone(),
        why: rejected.why.clone(),
        violates: rejected.violates,
    };

    let finding = apply_alternative_decision(
        &mut effective_selected,
        "client/app",
        RULE_ID,
        &rejected_view,
        "every upload call site passes file.name straight through as the object key",
        Some("src/upload.ts:31 - uses file.name as the storage path after stripping '../'"),
    );

    assert_eq!(
        effective_selected[0].directive, correct_directive,
        "the degenerate option must NEVER become this project's effective directive — \
             that is how an unsanitized upload path got adopted as a convention"
    );
    let f = finding
        .expect("a codebase matching the `Rejected:` option must produce a finding, not silence");
    assert_eq!(
        f.rule_id, RULE_ID,
        "the finding carries the rule's citation"
    );
    assert_eq!(
        f.kind,
        Some(camerata_api_types::audit::FindingKind::Defect),
        "an objective fault, never a consistency deviation"
    );
    assert_eq!(
        f.evaluated_option_id.as_deref(),
        Some(rejected.id.as_str()),
        "report_export::resolve_fix keys the remediation text off this"
    );

    // (3) And it is never recorded as an observed convention.
    let alternatives = vec![RuleAlternatives {
        rule_id: RULE_ID.to_string(),
        options: rule
            .options
            .iter()
            .map(|o| RuleOptionView {
                id: o.id.clone(),
                label: o.label.clone(),
                directive: o.directive.clone(),
                why: o.why.clone(),
                violates: o.violates,
            })
            .collect(),
        selected_option_id: rule.default_option.clone(),
        kind: rule.kind(),
    }];
    let recommendations = vec![RuleRecommendation {
        repo: String::new(),
        rule_id: RULE_ID.to_string(),
        recommended_option_id: rejected.id.clone(),
        recommendation_reasoning: "the repo keeps the client filename as the key".to_string(),
        evidence: Some("src/upload.ts:31".to_string()),
        ..RuleRecommendation::default()
    }];
    let observed =
        conventions_observed_findings("client/app", &[], &recommendations, &alternatives);
    assert!(
        observed.is_empty(),
        "a codebase matching the `Rejected:` option must NOT be reported as having adopted \
             a convention — that is the exact defect W12 fixes: {observed:?}"
    );
}

/// req_a5_4 positive twin: a genuine `decision`-shaped pick — the SAME two-option shape,
/// but the selected option is NOT marked `violates` — rewrites the directive exactly as
/// before this fix, and emits NO defect finding. Proves the fix is additive (it only
/// changes behavior for a `violates` option) rather than a blanket behavior change that
/// would also break the ordinary, non-defect multi-option case.
#[test]
fn req_a5_4_non_violates_option_rewrites_directive_and_emits_no_defect() {
    let mut effective_selected = vec![crate::scan_routing::AuditRule::new(
        "MULTI-RULE-1",
        "Do it the A way.",
    )];
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let opt_b = alt.options.iter().find(|o| o.id == "opt-b").unwrap();
    assert!(
        !opt_b.violates,
        "fixture sanity: opt-b is a genuine alternative here"
    );

    let finding = apply_alternative_decision(
        &mut effective_selected,
        "me/api",
        "MULTI-RULE-1",
        opt_b,
        "the repo consistently does it the B way",
        Some("src/lib.rs:42 - does it the B way"),
    );

    assert_eq!(
        effective_selected[0].directive, "Do it the B way.",
        "a genuine alternative DOES rewrite the directive, same as before this fix"
    );
    assert!(
        finding.is_none(),
        "a non-violates option must never emit a defect finding"
    );
}

/// req_a5_4, end-to-end through `audit_repo`: a rule whose alternatives pass DECIDES the
/// repo follows the `violates` option emits a finding EVEN THOUGH the stub's canned
/// per-chunk violation response is empty (`"findings": []`) — proving the emission comes
/// from the alternatives-decision mechanism itself, not from the ordinary per-chunk
/// violation pass independently noticing the same thing. The finding is then fed through
/// the SAME ledger `record_rule` call `onboard::audit_repos` makes for every semantic rule,
/// proving the rule is NEVER reported healthy (`ScanLedger::healthy_rule_ids`).
///
/// FAILS BEFORE THE FIX: pre-fix, the chosen option's directive was rewritten to the
/// violating option's own directive, the per-chunk pass then checked conformance against
/// THAT (the repo trivially "conforms" to its own already-adopted defect), the canned
/// per-chunk response contributes zero findings, and `findings` would be empty for this
/// rule — which is the exact silent-healthy shape A5 describes.
#[tokio::test]
async fn req_a5_4_audit_repo_emits_a_finding_for_a_violates_option_and_ledger_never_calls_it_healthy(
) {
    let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "recommended_option_id": "opt-b", "recommendation_reasoning": "the repo does it the B way everywhere", "evidence": "src/lib.rs:1 - no exception handling at all"}
            ],
            "findings": [],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { old pattern here; }\n".to_string(),
    )];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives_opt_b_violates(
        "MULTI-RULE-1",
        Some("opt-a"),
    )];
    let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].recommended_option_id, "opt-b");

    let f = findings
        .iter()
        .find(|f| f.rule_id == "MULTI-RULE-1")
        .expect(
            "a violates-option decision must emit a finding even with an empty canned \
                 per-chunk response",
        );
    assert_eq!(f.evaluated_option_id.as_deref(), Some("opt-b"));

    // Replicate the EXACT ledger recording `onboard::audit_repos`'s semantic-phase loop
    // performs for every pre-declared semantic rule id, using this real `audit_repo`
    // output — the production code path's own invariant, not a hand-waved assertion.
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    let emitted = findings
        .iter()
        .filter(|f| f.rule_id == "MULTI-RULE-1")
        .count();
    ledger.record_rule(
        "",
        "MULTI-RULE-1",
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        files.len(),
        emitted,
    );
    assert!(
        !ledger
            .healthy_rule_ids()
            .contains(&"MULTI-RULE-1".to_string()),
        "a rule whose selected option is marked violates must NEVER be reported healthy"
    );
}

// ---- Pass-1 W3: the five-kind taxonomy at emission (A5.2/A5.3/A5.5/A5.6) -------------

/// Minimal deviation-shaped finding fixture: an AI-tier row under `rule`, as it looks
/// right before `assign_deviation_kinds` runs (kind = Defect, exactly what
/// `parse_ai_findings` stamps at parse time).
fn deviation_fixture(rule: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: "me/api".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "high".to_string(),
        snippet: "the odd one out".to_string(),
        detail: "does not follow the majority pattern".to_string(),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

fn deviation_rec(rule: &str, option: &str, assessed_sites: Option<usize>) -> RuleRecommendation {
    RuleRecommendation {
        repo: String::new(),
        rule_id: rule.to_string(),
        recommended_option_id: option.to_string(),
        recommendation_reasoning: "the repo does it the A way nearly everywhere".to_string(),
        evidence: Some("src/a.rs:1 — the A pattern".to_string()),
        applicable: true,
        assessed_sites,
        // The fixture's own reasoning says "the repo does it the A way nearly everywhere",
        // i.e. the convention IS followed — so the model would report this. Set
        // explicitly rather than inherited from `Default` (which is `None`, meaning "the
        // model did not say") because W21/W22 makes that distinction load-bearing: see
        // `convention_observed_requires_the_model_to_affirm_the_convention_is_followed`
        // for the negative cases.
        currently_followed: Some(true),
        ..Default::default()
    }
}

/// req A5.6: a Deviation row carries the owner's canonical sentence as STRUCTURE —
/// numerator (deviating sites, computed from rows + absorbed merge sites), denominator
/// (the recommendation's structured sites_total), and the observed majority convention
/// as AUTHORED option identity. Verified failing before `assign_deviation_kinds` existed:
/// every AI row stayed `Defect` (the parse-time stamp), so the kind assertion below had
/// nothing to match.
#[test]
fn req_a5_6_deviation_carries_numerator_and_denominator() {
    let mut findings = vec![
        deviation_fixture("MULTI-RULE-1", "src/a.rs", 10),
        {
            // A merged row that ABSORBED one more site under the same rule — the
            // numerator must count it (2 rows + 1 absorbed = 3 deviating sites).
            let mut f = deviation_fixture("MULTI-RULE-1", "src/b.rs", 20);
            f.also_locations.push(MergedLocation::site(
                "me/api".to_string(),
                "src/c.rs".to_string(),
                30,
                "MULTI-RULE-1".to_string(),
                "third site".to_string(),
                false,
            ));
            f
        },
        // An unrelated rule's row must be untouched.
        deviation_fixture("OTHER-RULE-9", "src/z.rs", 5),
    ];
    let recs = vec![deviation_rec("MULTI-RULE-1", "opt-a", Some(47))];
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    assign_deviation_kinds(&mut findings, &recs, &alts);

    use camerata_api_types::audit::FindingKind;
    for f in findings.iter().filter(|f| f.rule_id == "MULTI-RULE-1") {
        match &f.kind {
            Some(FindingKind::Deviation(d)) => {
                assert_eq!(d.deviating_sites, 3, "2 rows + 1 absorbed site");
                assert_eq!(d.assessed_sites, Some(47));
                assert_eq!(d.convention_option_id.as_deref(), Some("opt-a"));
                assert_eq!(
                    d.convention_label, "Option A",
                    "AUTHORED label, never prose"
                );
            }
            other => panic!("expected Deviation, got {other:?}"),
        }
    }
    assert_eq!(
        findings
            .iter()
            .find(|f| f.rule_id == "OTHER-RULE-9")
            .unwrap()
            .kind,
        Some(FindingKind::Defect),
        "a rule with no recommendation keeps its Defect kind"
    );
}

/// req A5.2/A5.6 honesty, amended by R1-4: a model denominator SMALLER than the
/// structurally-counted numerator is raised to the numerator, never shipped as a nonsense
/// "3 of 2 sites" and never dropped to None (tutorly's row showed no denominator at all).
/// The earlier test here pinned the drop, which was the defect; it is replaced.
#[test]
fn req_a5_2_deviation_denominator_below_numerator_is_raised_to_the_numerator() {
    let mut findings = vec![
        deviation_fixture("MULTI-RULE-1", "src/a.rs", 10),
        deviation_fixture("MULTI-RULE-1", "src/b.rs", 20),
        deviation_fixture("MULTI-RULE-1", "src/c.rs", 30),
    ];
    // Deviating 3, the model's sites_total 2: Some(3), not None.
    let recs = vec![deviation_rec("MULTI-RULE-1", "opt-a", Some(2))];
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    assign_deviation_kinds(&mut findings, &recs, &alts);
    match &findings[0].kind {
        Some(camerata_api_types::audit::FindingKind::Deviation(d)) => {
            assert_eq!(d.deviating_sites, 3);
            assert_eq!(
                d.assessed_sites,
                Some(3),
                "a denominator below the counted numerator is raised to it, not dropped"
            );
        }
        other => panic!("expected Deviation, got {other:?}"),
    }
    // No model count at all stays "not counted".
    let mut findings = vec![deviation_fixture("MULTI-RULE-1", "src/a.rs", 10)];
    let recs = vec![deviation_rec("MULTI-RULE-1", "opt-a", None)];
    assign_deviation_kinds(&mut findings, &recs, &alts);
    match &findings[0].kind {
        Some(camerata_api_types::audit::FindingKind::Deviation(d)) => {
            assert_eq!(d.assessed_sites, None);
        }
        other => panic!("expected Deviation, got {other:?}"),
    }
}

/// req A5.4 × A5.6: rows under a rule whose DECIDED option is flagged `violates` are
/// defects — `assign_deviation_kinds` must never relabel them as consistency deviations.
#[test]
fn req_a5_6_violates_option_rows_stay_defect_never_deviation() {
    let mut findings = vec![deviation_fixture("MULTI-RULE-1", "src/a.rs", 10)];
    let recs = vec![deviation_rec("MULTI-RULE-1", "opt-b", Some(10))];
    let alts = vec![two_option_alternatives_opt_b_violates(
        "MULTI-RULE-1",
        Some("opt-a"),
    )];
    assign_deviation_kinds(&mut findings, &recs, &alts);
    assert_eq!(
        findings[0].kind,
        Some(camerata_api_types::audit::FindingKind::Defect),
        "a violates-option decision marks the repo as following the rule's own named \
             defect — its rows are Defects, never Deviations"
    );
}

/// req A5.3: "no convention adopted" is ONE informational row per rule, enforced
/// structurally — even a malformed recommendations vec carrying TWO no-convention
/// entries for one rule yields exactly one row. Verified failing by removing the
/// per-rule dedup: two rows came out.
#[test]
fn req_a5_3_no_convention_findings_emit_exactly_one_row_per_rule() {
    use camerata_api_types::audit::ObservedOptionUsage;
    let mk = |sites: Option<usize>| RuleRecommendation {
        repo: String::new(),
        rule_id: "MULTI-RULE-1".to_string(),
        recommended_option_id: "opt-a".to_string(),
        recommendation_reasoning: "used both ways with no majority".to_string(),
        applicable: true,
        no_convention: true,
        observed_options: vec![
            ObservedOptionUsage {
                option_id: "opt-a".to_string(),
                label: "Option A".to_string(),
                sites,
            },
            ObservedOptionUsage {
                option_id: "opt-b".to_string(),
                label: "Option B".to_string(),
                sites: Some(2),
            },
        ],
        ..Default::default()
    };
    // Malformed on purpose: the same rule twice.
    let rows = no_convention_findings("me/api", &[mk(Some(3)), mk(Some(9))]);
    assert_eq!(rows.len(), 1, "ONE row per rule, never N");
    let f = &rows[0];
    assert_eq!(f.rule_id, "MULTI-RULE-1");
    assert_eq!(f.severity, "info");
    assert!(!f.located);
    match &f.kind {
        Some(camerata_api_types::audit::FindingKind::NoConvention(nc)) => {
            assert_eq!(nc.observed_options.len(), 2);
            assert_eq!(nc.observed_options[0].option_id, "opt-a");
            assert_eq!(nc.observed_options[0].sites, Some(3));
        }
        other => panic!("expected NoConvention, got {other:?}"),
    }
}

/// req A5.3 structural gate at PARSE time: the no-convention outcome is honored only for
/// a pure Decision-kind rule AND only when backed by a validated observed-usage
/// breakdown or real evidence; anything else is corrected (flagged `hallucinated`,
/// judged against an option exactly as before). Verified failing before the gate
/// existed: the raw claim would have been invisible (field unparsed) or, once parsed
/// naively, honored for every kind.
#[test]
fn req_a5_3_parse_refuses_no_convention_for_non_decision_or_unbacked_claims() {
    // NOTE: the claim carries a real evidence citation — P7's evidence-gates-
    // applicability check runs FIRST and downgrades ANY unevidenced `applicable: true`
    // to not-applicable, no-convention claims included (case (c) below proves that).
    let raw = r#"{"recommendations": [
            {"rule_id": "MULTI-RULE-1", "applicable": true, "no_convention": true,
             "evidence": "src/lib.rs:1 — both patterns in one file",
             "observed_options": [{"option_id": "opt-a", "sites": 3},
                                   {"option_id": "opt-b", "sites": 2},
                                   {"option_id": "not-a-real-option", "sites": 9}],
             "recommendation_reasoning": "even split"}
        ]}"#;

    // (a) Decision-kind + validated breakdown -> honored; the invented option id is
    // dropped from the breakdown, never carried.
    let decision = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let recs = parse_alternative_recommendations(raw, &decision);
    assert_eq!(recs.len(), 1);
    assert!(
        recs[0].no_convention,
        "decision-kind + breakdown must be honored"
    );
    assert!(!recs[0].hallucinated);
    assert_eq!(
        recs[0]
            .observed_options
            .iter()
            .map(|o| o.option_id.as_str())
            .collect::<Vec<_>>(),
        vec!["opt-a", "opt-b"],
        "an observed option id must be one of the rule's REAL options"
    );
    assert_eq!(
        recs[0].observed_options[0].label, "Option A",
        "authored label"
    );

    // (b) The SAME answer for a default_with_exceptions rule -> refused, corrected.
    let mut dwe = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    dwe.kind = Some(camerata_rules::RuleKind::DefaultWithExceptions);
    let recs = parse_alternative_recommendations(raw, &[dwe]);
    assert!(
        !recs[0].no_convention,
        "a default_with_exceptions rule has an externally-correct default — the \
             no-convention escape must never be honored for it"
    );
    assert!(
        recs[0].hallucinated,
        "the correction must be visible, never silent"
    );

    // (c) Decision-kind but NOTHING backing the claim (no breakdown, no evidence) ->
    // refused: a bare \"no convention\" can never silently drop a rule's coverage.
    let raw_unbacked = r#"{"recommendations": [
            {"rule_id": "MULTI-RULE-1", "applicable": true, "no_convention": true,
             "recommendation_reasoning": "trust me"}
        ]}"#;
    let decision = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let recs = parse_alternative_recommendations(raw_unbacked, &decision);
    assert!(!recs[0].no_convention);
    assert!(recs[0].hallucinated);
}

/// req A5.2/A5.6: `sites_total` is read as a STRUCTURED field off the recommendation
/// JSON (the deviation sentence's denominator) — zero/absent/negative-shaped values are
/// None, never fabricated.
#[test]
fn req_a5_2_parse_reads_sites_total_as_structured_denominator() {
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let raw = r#"{"recommendations": [
            {"rule_id": "MULTI-RULE-1", "applicable": true, "recommended_option_id": "opt-a",
             "evidence": "src/a.rs:1 — the A pattern", "sites_total": 47,
             "recommendation_reasoning": "A everywhere"}
        ]}"#;
    let recs = parse_alternative_recommendations(raw, &alts);
    assert_eq!(recs[0].assessed_sites, Some(47));

    let raw_zero = r#"{"recommendations": [
            {"rule_id": "MULTI-RULE-1", "applicable": true, "recommended_option_id": "opt-a",
             "evidence": "src/a.rs:1 — the A pattern", "sites_total": 0,
             "recommendation_reasoning": "A everywhere"}
        ]}"#;
    let recs = parse_alternative_recommendations(raw_zero, &alts);
    assert_eq!(recs[0].assessed_sites, None, "a zero count is no count");
}

/// W21/W22 — "the codebase consistently follows X" requires the model to have said the
/// codebase follows X. `applicable: true` does not mean that and never did.
///
/// The shipped defect, from the 2026-10-18 hearth export: `ARCH-EXPLICIT-TX-1` rendered
/// *"The codebase consistently follows 'Services own the transaction lifecycle…' — zero
/// violations were observed this scan"* over a repo whose same paragraph read *"The
/// codebase has no transaction management yet […] No existing pattern uses ambient or
/// per-call transactions, so this is the right aspirational target."* Six of 26 convention
/// rows carried that contradiction by keyword search; the graders found 13 of 26 by
/// reading them.
///
/// Nothing was wrong with the model's answer. The prompt defines `applicable: true` as
/// "already follows this rule's concern OR concretely needs it", and instructs the model
/// to say plainly when a rule applies but nothing follows it yet — which it did, in prose,
/// where no renderer or severity pass could read it.
#[test]
fn convention_observed_requires_the_model_to_affirm_the_convention_is_followed() {
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];

    // The affirmative case still emits — the feature is a deliberate product surface and
    // must survive this fix.
    let followed = vec![deviation_rec("MULTI-RULE-1", "opt-a", Some(12))];
    assert_eq!(
        conventions_observed_findings("me/api", &[], &followed, &alts).len(),
        1,
        "an affirmed convention must still be reported"
    );

    // The hearth shape: the rule applies precisely BECAUSE the codebase lacks the thing it
    // governs. Applicable, evidenced, non-violates option, zero trace — every condition
    // the old code checked — and still not a convention.
    let mut aspirational = deviation_rec("MULTI-RULE-1", "opt-a", Some(12));
    aspirational.currently_followed = Some(false);
    aspirational.recommendation_reasoning =
        "no existing pattern; recommending opt-a as the best-practice fit for this stack"
            .to_string();
    assert!(
        conventions_observed_findings("me/api", &[], &[aspirational], &alts).is_empty(),
        "an aspirational target is not a convention the codebase follows"
    );

    // Absent fails CLOSED, unlike every other optional model field here. The asymmetry is
    // the point: a missed convention row costs the client a mildly interesting paragraph,
    // a wrong one puts a false statement about their own code in front of them over our
    // name.
    let mut unsaid = deviation_rec("MULTI-RULE-1", "opt-a", Some(12));
    unsaid.currently_followed = None;
    assert!(
        conventions_observed_findings("me/api", &[], &[unsaid], &alts).is_empty(),
        "a model that did not answer the question has not affirmed the convention"
    );
}

/// req A5.5: a decided, genuine (non-violates) convention with ZERO trace of a
/// violation ships as ONE informational convention_observed row — and any trace at all
/// (an own row, an also_matches demotion, or an absorbed merge site) suppresses it.
/// Verified failing before `conventions_observed_findings` existed: zero-violation
/// rules produced no row of any kind (the A5.5 ship-the-convention-set requirement had
/// no carrier).
#[test]
fn req_a5_5_convention_observed_emitted_only_when_zero_trace() {
    use camerata_api_types::audit::FindingKind;
    let recs = vec![deviation_rec("MULTI-RULE-1", "opt-a", Some(12))];
    let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];

    // Zero trace -> exactly one informational row carrying the authored option.
    let rows = conventions_observed_findings("me/api", &[], &recs, &alts);
    assert_eq!(rows.len(), 1);
    let f = &rows[0];
    assert_eq!(f.severity, "info");
    assert_eq!(f.rule_id, "MULTI-RULE-1");
    assert_eq!((f.path.as_str(), f.line, f.located), ("src/a.rs", 1, true));
    match &f.kind {
        Some(FindingKind::ConventionObserved(c)) => {
            assert_eq!(c.convention_option_id, "opt-a");
            assert_eq!(c.convention_label, "Option A");
        }
        other => panic!("expected ConventionObserved, got {other:?}"),
    }

    // An own row under the rule -> suppressed.
    let own = vec![deviation_fixture("MULTI-RULE-1", "src/x.rs", 2)];
    assert!(conventions_observed_findings("me/api", &own, &recs, &alts).is_empty());

    // A same-location demotion (also_matches) -> suppressed too.
    let mut other_rule = deviation_fixture("OTHER-RULE-9", "src/x.rs", 2);
    other_rule.also_matches.push("MULTI-RULE-1".to_string());
    assert!(
        conventions_observed_findings("me/api", &[other_rule], &recs, &alts).is_empty(),
        "a violation absorbed into another row is still a violation — never claim \
             zero-violation consistency over it"
    );
}

/// req A5.3 END-TO-END through `audit_repo`: a no-convention outcome yields exactly ONE
/// informational row for the rule — even when the model ALSO free-associates per-site
/// findings under that rule in the violation passes. Verified failing before the fix by
/// disabling the honored-no-convention branch in `parse_alternative_recommendations`
/// (the pre-fix behavior): the canned per-chunk findings shipped as 2 violation rows.
#[tokio::test]
async fn req_a5_3_audit_repo_no_convention_yields_one_informational_row_never_n() {
    let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "applicable": true, "no_convention": true,
                 "observed_options": [{"option_id": "opt-a", "sites": 3},
                                       {"option_id": "opt-b", "sites": 2}],
                 "evidence": "src/lib.rs:1 — both patterns in one file",
                 "recommendation_reasoning": "three sites do A, two do B — no majority"}
            ],
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "high", "title": "does it the B way",
                 "path": "src/lib.rs", "line": 1, "code": "fn handler() { b_way(); }",
                 "detail": "stray per-site claim the hard filter must drop"},
                {"rule": "MULTI-RULE-1", "severity": "high", "title": "does it the A way",
                 "path": "src/other.rs", "line": 2, "code": "fn other() { a_way(); }",
                 "detail": "second stray per-site claim"}
            ],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![
        (
            "src/lib.rs".to_string(),
            "fn handler() { b_way(); }\n".to_string(),
        ),
        (
            "src/other.rs".to_string(),
            "fn other() { a_way(); }\n".to_string(),
        ),
    ];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let (findings, _proposed, recs, _failed, stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    assert!(recs.iter().any(|r| r.no_convention));
    // A10: the two strays are kept, marked thrown out, apart from the rows that ship.
    let (thrown, findings) = crate::engine_discard::split_discarded(findings);
    assert_eq!(thrown.len(), 2, "{thrown:?}");
    let under_rule: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.rule_id == "MULTI-RULE-1")
        .collect();
    assert_eq!(
        under_rule.len(),
        1,
        "ONE informational row, never N violations: {under_rule:?}"
    );
    let f = under_rule[0];
    assert_eq!(f.severity, "info");
    match &f.kind {
        Some(camerata_api_types::audit::FindingKind::NoConvention(nc)) => {
            assert_eq!(nc.observed_options.len(), 2);
        }
        other => panic!("expected NoConvention, got {other:?}"),
    }
    // The strays were dropped VISIBLY (accounted), and the one row was added VISIBLY.
    assert!(stage_samples.iter().any(|s| s.stage == "ai-calibration"
        && s.accounting
            .other
            .iter()
            .any(|(label, n)| label == "no_convention_rule_filtered" && *n == 2)));
    assert!(stage_samples
        .iter()
        .any(|s| s.stage == "no-convention-emission"));
}

/// A10 (`no_engine_path_deletes_a_finding`): every judgment path that used to delete a row
/// keeps it, marked with its reason. One scripted run feeds `audit_repo` six model entries:
/// a stray under a not-applicable rule, two strays under a no-convention rule, a row whose
/// own text says to discard it, a row filled with placeholder text and one real defect. Every
/// entry comes back as a row; only the real one is unmarked.
#[tokio::test]
async fn no_engine_path_deletes_a_finding() {
    use crate::engine_discard::{
        reason_of, DISCARD_VERDICT, NO_CONVENTION, PLACEHOLDER_ROW, RULE_NOT_APPLICABLE,
    };
    let canned = r#"{
            "recommendations": [
                {"rule_id": "NA-RULE-1", "applicable": false,
                 "recommendation_reasoning": "no evidence of pagination anywhere in this codebase"},
                {"rule_id": "MULTI-RULE-1", "applicable": true, "no_convention": true,
                 "observed_options": [{"option_id": "opt-a", "sites": 3},
                                       {"option_id": "opt-b", "sites": 2}],
                 "evidence": "src/lib.rs:1 — both patterns in one file",
                 "recommendation_reasoning": "three sites do A, two do B — no majority"}
            ],
            "findings": [
                {"rule": "NA-RULE-1", "severity": "medium", "title": "stray under na rule",
                 "path": "src/na.rs", "line": 1, "code": "fn na() { old(); }", "detail": "stray"},
                {"rule": "MULTI-RULE-1", "severity": "high", "title": "does it the B way",
                 "path": "src/lib.rs", "line": 1, "code": "fn handler() { b_way(); }",
                 "detail": "stray per-site claim"},
                {"rule": "MULTI-RULE-1", "severity": "high", "title": "does it the A way",
                 "path": "src/other.rs", "line": 2, "code": "fn other() { a_way(); }",
                 "detail": "second stray per-site claim"},
                {"rule": "AI-NOTE-ONLY", "severity": "low", "title": "a note",
                 "path": "src/note.rs", "line": 3, "code": "fn note() { keep(); }",
                 "detail": "Nothing to act on. Discard this finding."},
                {"rule": "AI-FILLER", "severity": "low", "title": "placeholder",
                 "path": "src/filler.rs", "line": 4, "code": "x", "detail": "x"},
                {"rule": "AI-REAL-DEFECT", "severity": "high", "title": "unchecked owner",
                 "path": "src/real.rs", "line": 5, "code": "fn real() { delete(id); }",
                 "detail": "The handler deletes by id without checking the owner."}
            ],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![
        ("src/na.rs".to_string(), "fn na() { old(); }\n".to_string()),
        (
            "src/lib.rs".to_string(),
            "fn handler() { b_way(); }\n".to_string(),
        ),
        (
            "src/other.rs".to_string(),
            "fn other() { a_way(); }\n".to_string(),
        ),
        (
            "src/note.rs".to_string(),
            "fn note() { keep(); }\n".to_string(),
        ),
        ("src/filler.rs".to_string(), "x\n".to_string()),
        (
            "src/real.rs".to_string(),
            "fn real() { delete(id); }\n".to_string(),
        ),
    ];
    let selected = vec![
        ("NA-RULE-1".to_string(), "Paginate lists.".to_string()),
        ("MULTI-RULE-1".to_string(), "Do it the A way.".to_string()),
    ];
    let alternatives = vec![
        two_option_alternatives("NA-RULE-1", Some("opt-a")),
        two_option_alternatives("MULTI-RULE-1", Some("opt-a")),
    ];
    let (findings, _proposed, _recs, _failed, stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    let at = |path: &str| {
        findings
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("the row at {path} was deleted: {findings:?}"))
    };
    assert_eq!(reason_of(at("src/na.rs")), Some(RULE_NOT_APPLICABLE));
    assert_eq!(reason_of(at("src/lib.rs")), Some(NO_CONVENTION));
    assert_eq!(reason_of(at("src/other.rs")), Some(NO_CONVENTION));
    assert_eq!(reason_of(at("src/note.rs")), Some(DISCARD_VERDICT));
    assert!(
        reason_of(at("src/filler.rs")).is_some_and(|r| r.starts_with(PLACEHOLDER_ROW)),
        "{:?}",
        at("src/filler.rs")
    );
    assert_eq!(reason_of(at("src/real.rs")), None);
    // The ledger still reconciles: the rows that left the calibration stage are accounted
    // for by their labels, and the rows kept apart are counted by reason.
    assert!(stage_samples.iter().all(|s| {
        s.rows_in + s.accounting.added_total()
            >= s.rows_out + s.accounting.accounted().min(s.rows_in)
    }));
}

/// req A5.5 END-TO-END through `audit_repo`: a decided convention with zero violations
/// ships ONE convention_observed row (the derived decision set ships in the product).
/// Verified failing before the fix: the pre-W3 pipeline returned ZERO rows for this
/// exact input (nothing carried a zero-violation convention).
#[tokio::test]
async fn req_a5_5_audit_repo_ships_convention_observed_for_a_clean_decided_rule() {
    let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "applicable": true,
                 "currently_followed": true,
                 "recommended_option_id": "opt-a", "sites_total": 12,
                 "evidence": "src/lib.rs:1 — the A pattern",
                 "recommendation_reasoning": "A everywhere, consistently"}
            ],
            "findings": [],
            "proposed_rules": []
        }"#;
    let llm = StubCompleter {
        text: canned.to_string(),
    };
    let files = vec![(
        "src/lib.rs".to_string(),
        "fn handler() { a_way(); }\n".to_string(),
    )];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
    let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
    let (findings, _proposed, _recs, _failed, stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &alternatives,
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    let rows: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.rule_id == "MULTI-RULE-1")
        .collect();
    assert_eq!(rows.len(), 1, "exactly one convention_observed row");
    match &rows[0].kind {
        Some(camerata_api_types::audit::FindingKind::ConventionObserved(c)) => {
            assert_eq!(c.convention_option_id, "opt-a");
        }
        other => panic!("expected ConventionObserved, got {other:?}"),
    }
    assert!(stage_samples
        .iter()
        .any(|s| s.stage == "convention-observed-emission"));
}

/// req A5.6 END-TO-END through `audit_repo`: a rule ARMED for a language with no
/// matching files ships ONE not_assessed honesty row — never silently reads as clean.
/// Verified failing before the fix: the routing loop's `continue` skipped the group
/// with no trace, so this exact input returned zero findings.
#[tokio::test]
async fn req_a5_6_audit_repo_emits_not_assessed_for_armed_rule_with_no_matching_files() {
    let llm = StubCompleter {
        text: r#"{"findings": [], "proposed_rules": []}"#.to_string(),
    };
    // A RUST-scoped rule armed against a repo with only Python files.
    let files = vec![("app.py".to_string(), "print('hello')\n".to_string())];
    let selected = vec![(
        "RUST-NO-UNWRAP-1".to_string(),
        "Never unwrap in handlers.".to_string(),
    )];
    let (findings, _proposed, _recs, _failed, stage_samples) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("stub completer never errors");

    let rows: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.rule_id == "RUST-NO-UNWRAP-1")
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "one honesty row per armed-but-unevaluated rule"
    );
    let f = rows[0];
    assert_eq!(f.severity, "info");
    match &f.kind {
        Some(camerata_api_types::audit::FindingKind::NotAssessed(na)) => {
            assert_eq!(
                na.reason,
                camerata_api_types::audit::NotAssessedReason::NoFilesMatched
            );
            assert!(!na.reason_detail.is_empty());
        }
        other => panic!("expected NotAssessed, got {other:?}"),
    }
    assert!(stage_samples
        .iter()
        .any(|s| s.stage == "not-assessed-emission"));
}

// ---- W2: no-vanishing merge guarantee ------------------------------------------------
//
// Both merge passes pick a PRIMARY and literally `.remove()` it from the group's Vec to
// build the output row's `also_matches` from the rest. That `.remove()` is exactly the
// operation a future refactor could get wrong (e.g. special-casing "group now empty" into
// an early `None`/empty-Vec return instead of "the primary IS the row"). These tests pin
// the invariant directly: a group whose primary is removed during merge must ALWAYS still
// emit that one row, for every group size including the singleton case (where removing the
// primary leaves literally nothing else).

#[test]
fn merge_location_group_singleton_whose_primary_is_removed_still_emits_a_row() {
    let f = site_finding("AI-LONE-FINDING", "a.rs", 7, "high", "only member");
    let merged = head_row(vec![f]);
    assert_eq!(
        merged.rule_id, "AI-LONE-FINDING",
        "the only member IS the primary — removing it from the group must still yield it \
             as the output row, not an empty/default finding"
    );
    assert!(
        merged.also_matches.is_empty(),
        "a singleton group has no siblings to demote"
    );
}

#[test]
fn head_row_is_the_deterministic_member_among_three_at_one_site() {
    // Three findings at the same site; `SEC-` beats the `AI-` ones on class+origin.
    let sec = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "db.rs", 20, "high", "sec");
    let ai_a = site_finding("AI-RAW-QUERY-BUILDING", "db.rs", 20, "medium", "sec");
    let ai_b = site_finding("AI-UNSAFE-QUERY", "db.rs", 20, "medium", "sec");
    let head = head_row(vec![sec, ai_a, ai_b]);
    assert_eq!(
        head.rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
        "the deterministic member heads the group"
    );
}

// ---- Semantic dedup (design §1d) ----------------------------------------------------

#[test]
fn semantic_det_and_ai_same_category_share_a_cluster_with_the_det_row_at_its_head() {
    // A deterministic RLS finding and an AI RLS finding three lines apart are the same
    // defect: they share a cluster, the deterministic side heads it and keeps its exact
    // line, and neither row is merged into the other.
    let mut det = site_finding("RLS-MISSING", "a.rs", 10, "high", "");
    det.category = Some("rls-policy".to_string());
    let mut ai = site_finding("AI-rls-thing", "a.rs", 13, "medium", "");
    ai.category = Some("rls-policy".to_string());
    let groups = clusters(vec![det, ai], &[]);
    assert_eq!(groups.len(), 1, "same defect: one cluster");
    assert_eq!(groups[0].len(), 2, "both rows are still rows");
    let head = head_of(&groups[0]);
    assert_eq!(
        head.rule_id, "RLS-MISSING",
        "deterministic side heads the cluster"
    );
    assert_eq!(
        head.line, 10,
        "the head keeps its own deterministic anchor line"
    );
    let severities: Vec<&str> = groups[0].iter().map(|f| f.severity.as_str()).collect();
    assert_eq!(
        severities,
        vec!["high", "medium"],
        "each row keeps its own severity"
    );
}

#[test]
fn semantic_two_ai_same_construct_merge() {
    // Two AI findings in the same handler body but > window lines apart merge via the
    // same-construct rule (both located inside one brace block corroborates the fusion).
    let content = "fn handler() {\n a\n b\n c\n d\n e\n f\n g\n h\n}\n";
    let mut a = site_finding("AI-authz-1", "h.rs", 2, "medium", "");
    a.category = Some("authorization".to_string());
    let mut b = site_finding("AI-authz-2", "h.rs", 8, "medium", "");
    b.category = Some("authorization".to_string());
    let files = vec![("h.rs".to_string(), content.to_string())];
    let out = cluster_heads(vec![a, b], &files);
    assert_eq!(out.len(), 1, "one construct, one defect");
}

#[test]
fn semantic_same_category_different_object_does_not_merge() {
    // Same category, adjacent lines, but disjoint structural objects (two different
    // tables) — the discrimination guard keeps them separate.
    let mut a = site_finding("AI-rls-a", "s.sql", 10, "high", "public.orders");
    a.category = Some("rls-policy".to_string());
    let mut b = site_finding("AI-rls-b", "s.sql", 12, "high", "public.payments");
    b.category = Some("rls-policy".to_string());
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(out.len(), 2, "disjoint tables stay distinct");
}

#[test]
fn semantic_category_none_never_merges() {
    // Uncategorized findings (heuristic returns None too) never fuse — fail-open to
    // over-telling.
    let a = site_finding("XYZ-1", "a.rs", 10, "high", "");
    let b = site_finding("XYZ-2", "a.rs", 11, "high", "");
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(out.len(), 2, "no category ⇒ no merge");
}

#[test]
fn semantic_two_deterministic_never_merge() {
    // Two deterministic rows are two distinct defects by construction — never merge even
    // when same-category and adjacent.
    let mut a = site_finding("RLS-A", "a.rs", 10, "high", "");
    a.category = Some("rls-policy".to_string());
    let mut b = site_finding("RLS-B", "a.rs", 11, "high", "");
    b.category = Some("rls-policy".to_string());
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(out.len(), 2, "two deterministic rows stay separate");
}
