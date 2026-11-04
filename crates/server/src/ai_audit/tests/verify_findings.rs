//! Tests for verify findings.

use super::*;

pub(super) fn finding(rule: &str, sev: &str) -> Finding {
    Finding {
        could_be_intentional: false,
        site_key: String::new(),
        group_path: Vec::new(),
        group_role: crate::review_tree::GroupRole::Primary,
        review: Default::default(),
        flagged_by: Vec::new(),
        possible_duplicate_of: None,
        repo: "me/api".into(),
        kind: None,
        path: "a.rs".into(),
        line: 1,
        rule_id: rule.into(),
        severity: sev.into(),
        snippet: "x".into(),
        detail: "d".into(),
        status: "active".into(),
        also_matches: Vec::new(),
        preview: false,
        preview_tool: None,
        in_test: false,
        needs_review: false,
        confidence: None,
        effort: None,
        category: None,
        located: true,
        captures: Default::default(),
        evaluated_option_id: None,
        also_locations: Vec::new(),
        fix_specific: None,
        calibration_rationale: None,
    }
}

#[test]
fn apply_verdicts_recalibrates_and_keeps_all() {
    let findings = vec![
        finding("AI-TIMING", "medium"), // index 0 -> low confidence, kept
        finding("AI-AUTHZ", "high"),    // index 1 -> downgraded to low, kept
        finding("AI-REAL", "high"),     // index 2 -> kept as-is
    ];
    let raw = r#"{"verdicts":[
            {"index":0,"real":"unclear","reason":"negligible timing residual"},
            {"index":1,"severity":"low","real":"yes","reason":"low impact"},
            {"index":2,"severity":"high","real":"yes","reason":"concrete"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    // The calibration pass NEVER drops — every finding reaches the architect.
    assert_eq!(out.len(), 3, "no finding is dropped");
    let timing = out.iter().find(|f| f.rule_id == "AI-TIMING").unwrap();
    assert!(timing.needs_review, "an unclear verdict flags the finding");
    assert_eq!(timing.confidence.as_deref(), Some("needs-review"));
    assert!(
        !timing.detail.contains("[needs review"),
        "the flag is structured, not a suffix on the client text"
    );
    let authz = out.iter().find(|f| f.rule_id == "AI-AUTHZ").unwrap();
    assert_eq!(authz.severity, "low", "recalibrated down");
}

/// GUARD (this fix): `verify_findings` must record its OWN transcript entry — a
/// non-empty REAL prompt and the model's verbatim response as output, terminating in a
/// "done" status — rather than leaving the "calibrating…" agent with an empty prompt and
/// "no output captured" the way `audit_repo`'s old placeholder registration did.
#[tokio::test]
async fn verify_findings_records_prompt_and_output_to_transcript() {
    let resp_text =
        r#"{"severity":"critical","real":"yes","effort":"low","reason":"stub verdict"}"#;
    let llm = StubCompleter {
        text: resp_text.to_string(),
    };
    let findings = vec![finding("AI-X", "medium")];
    let store = crate::transcript::TranscriptStore::default();

    let (out, _estimation_failed) = verify_findings(
        &llm,
        "me/api",
        findings,
        None,
        Some((&store, "run-1")),
        None,
        &Stage1Inputs::new("This repository has 1 code files."),
    )
    .await;

    // Calibration itself still applies as before.
    assert_eq!(out[0].severity, "critical", "verdict still applied");

    let transcripts = store.get("run-1");
    let entry = transcripts
        .iter()
        .find(|a| a.session_id == "audit-me/api-calibrate")
        .expect("calibration pass registers its own transcript entry");
    assert!(
        !entry.prompt.is_empty(),
        "the generated prompt must be recorded, not left empty"
    );
    assert!(
        entry.prompt.contains("The finding (scanned severity"),
        "the recorded prompt should be the ACTUAL prompt sent, got: {}",
        entry.prompt
    );
    assert_eq!(
        entry.output, resp_text,
        "output must equal the stub's response — no 'no output captured'"
    );
    assert_eq!(entry.status, "done");
}

/// `feedback: None` (the pre-existing, non-cockpit call path) must still calibrate
/// findings exactly as before — no panic, no behavior change — since transcript
/// recording is purely additive.
#[tokio::test]
async fn verify_findings_with_no_feedback_still_calibrates() {
    let resp_text =
        r#"{"severity":"critical","real":"yes","effort":"low","reason":"stub verdict"}"#;
    let llm = StubCompleter {
        text: resp_text.to_string(),
    };
    let findings = vec![finding("AI-X", "medium")];

    let (out, _estimation_failed) = verify_findings(
        &llm,
        "me/api",
        findings,
        None,
        None, // feedback
        None,
        &Stage1Inputs::new("This repository has 1 code files."),
    )
    .await;

    assert_eq!(
        out[0].severity, "critical",
        "calibration still applies without a transcript store"
    );
}

/// Records which `LlmPort` method was actually invoked — `complete` (flat, no progress
/// signal, hard 300s CLI deadline) vs `complete_streaming` (heartbeat + salvage on the
/// CLI leg). Used to prove the three aggregate passes route through the tolerant
/// streaming seam EVEN WHEN there is no live transcript to feed (`feedback = None`),
/// which used to force them onto the flat `complete` path.
struct SeamRecordingCompleter {
    complete_calls: std::sync::atomic::AtomicUsize,
    streaming_calls: std::sync::atomic::AtomicUsize,
    text: String,
}

#[async_trait::async_trait]
impl LlmPort for SeamRecordingCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.complete_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.streaming_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text = super::reply_schemas::check::address_by_id(&req.prompt, &self.text);
        on_delta(&text);
        Ok(LlmResponse {
            text,
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// GUARD (this fix): `feedback: None` (the headless `camerata inspect` shape — see
/// `crates/cli/src/inspect_cmd.rs`) used to force `verify_findings` onto the flat,
/// non-streaming `complete` path, which has a hard 300s CLI subprocess deadline with no
/// progress reset and no partial-output salvage. It must now route through
/// `complete_streaming` regardless of whether a transcript is attached.
#[tokio::test]
async fn verify_findings_with_no_feedback_uses_the_streaming_seam_not_flat_complete() {
    let resp_text = r#"{"severity":"high","real":"yes","effort":"low","reason":"x"}"#;
    let completer = SeamRecordingCompleter {
        complete_calls: 0.into(),
        streaming_calls: 0.into(),
        text: resp_text.to_string(),
    };
    let findings = vec![finding("AI-X", "medium")];

    let _ = verify_findings(
        &completer,
        "me/api",
        findings,
        None,
        None, // feedback: None — the headless-CLI shape
        None,
        &Stage1Inputs::new("This repository has 1 code files."),
    )
    .await;

    assert_eq!(
        completer
            .complete_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "must never use the flat non-streaming path"
    );
    assert_eq!(
        completer
            .streaming_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "must route through complete_streaming even with feedback = None"
    );
}

/// Same guard for `recommend_alternatives`.
#[tokio::test]
async fn recommend_alternatives_with_no_feedback_uses_the_streaming_seam_not_flat_complete() {
    let resp_text = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","applicable":true,"evidence":"a.rs:1"}]}"#;
    let completer = SeamRecordingCompleter {
        complete_calls: 0.into(),
        streaming_calls: 0.into(),
        text: resp_text.to_string(),
    };
    let files = vec![("a.rs".to_string(), "fn f() {}".to_string())];
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));

    let res = recommend_alternatives(
        &completer,
        "me/api",
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        &[alt],
        None,
        None,
        None,
    )
    .await;

    assert!(
        res.is_ok(),
        "the streaming call succeeded, so this must be Ok: {res:?}"
    );
    assert_eq!(
        completer
            .complete_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "must never use the flat non-streaming path"
    );
    assert_eq!(
        completer
            .streaming_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "must route through complete_streaming even with feedback = None"
    );
}

/// Same guard for `generate_fix_specifics` — it never took a `feedback`/transcript
/// parameter at all, so it always hit the flat `complete` path before this fix.
#[tokio::test]
async fn generate_fix_specifics_uses_the_streaming_seam_not_flat_complete() {
    let resp_text = r#"{"fixes":[{"index":0,"fix":"Validate the input before using it and return an error otherwise."}]}"#;
    let completer = SeamRecordingCompleter {
        complete_calls: 0.into(),
        streaming_calls: 0.into(),
        text: resp_text.to_string(),
    };
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");

    let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;

    assert_eq!(
        completer
            .complete_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "must never use the flat non-streaming path"
    );
    assert_eq!(
        completer
            .streaming_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "must route through complete_streaming"
    );
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Validate the input before using it and return an error otherwise.")
    );
}

/// Every site is its own call, and each call asks for ONE verdict: its output cap is the
/// per-verdict budget plus the stage's thinking allowance, never the old batch-sized cap.
#[tokio::test]
async fn verify_findings_caps_each_sites_reply_at_one_verdict() {
    let completer = CapturingCompleter::default();
    let findings: Vec<Finding> = (0..CALIBRATION_BATCH_SIZE)
        .map(|_| finding("AI-X", "medium"))
        .collect();

    let _ = verify_findings(
        &completer,
        "me/api",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("This repository has many code files."),
    )
    .await;

    let seen = completer.seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        CALIBRATION_BATCH_SIZE,
        "one call per site, not one per chunk"
    );
    let expected = calibration_max_tokens(1);
    assert!(expected >= 4096, "never under the old flat floor");
    for req in seen.iter() {
        assert_eq!(req.max_tokens, expected);
        assert_eq!(
            req.effort.as_deref(),
            Some("medium"),
            "calibration runs at medium effort"
        );
    }
    assert!(
        expected >= VERIFY_TOKENS_PER_FINDING + 1_000,
        "the cap covers the verdict's text plus the thinking allowance"
    );
}

/// A SMALL finding set must see ZERO behavior change — it stays pinned at the original
/// flat 4096 floor.
#[tokio::test]
async fn verify_findings_max_tokens_floor_unchanged_for_small_finding_sets() {
    let completer = CapturingCompleter::default();
    let findings = vec![finding("AI-X", "medium")];

    let _ = verify_findings(
        &completer,
        "me/api",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("This repository has 1 code files."),
    )
    .await;

    let seen = completer.seen.lock().unwrap();
    assert_eq!(seen[0].max_tokens, calibration_max_tokens(1));
    assert!(seen[0].max_tokens >= 4096, "never under the old flat floor");
}

/// A fake that streams a FEW tokens of real output and then returns `Ok` with only that
/// PARTIAL text — exactly the shape `complete_cli_streaming`'s SALVAGE path returns when
/// the subprocess is killed mid-response after producing something usable (see that
/// function's own SALVAGE doc comment in `crates/llm/src/llm.rs`). The first call
/// "dies" after covering only finding 0; later calls (the automatic next-round retry)
/// are fully healthy. Proves the pass extracts whatever a partial/salvaged response
/// actually contains — recovering finding 0 in the SAME round — instead of discarding
/// the whole round because the stream never finished cleanly.
struct SalvagingFixCompleter {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for SalvagingFixCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("this fake only supports the streaming path")
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text = if call == 0 {
            // Streamed a few real tokens, then the process died — SALVAGE returns `Ok`
            // with only what was actually produced, never an `Err` that discards it.
            on_delta(r#"{"fixes":[{"index":0,"fix":"Validate the"#);
            r#"{"fixes":[{"index":0,"fix":"Validate the input before using it and return an error otherwise."}]}"#.to_string()
        } else {
            // Recovered: every subsequent call (the automatic retry round) answers in
            // full for every plausible local index.
            let entries: Vec<String> = (0..50)
                    .map(|i| {
                        format!(
                            r#"{{"index":{i},"fix":"Validate the input before using it and return an error otherwise."}}"#
                        )
                    })
                    .collect();
            format!(r#"{{"fixes":[{}]}}"#, entries.join(","))
        };
        let text = super::reply_schemas::check::address_by_id(&req.prompt, &text);
        on_delta(&text);
        Ok(LlmResponse {
            text,
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn generate_fix_specifics_recovers_partial_output_from_a_salvaged_stream() {
    let findings = vec![
        fx("ARCH-1", "a.rs", 1, "some real defect", "let x = 1;"),
        fx("ARCH-1", "b.rs", 2, "some real defect", "let y = 2;"),
        fx("ARCH-1", "c.rs", 3, "some real defect", "let z = 3;"),
    ];
    let completer = SalvagingFixCompleter { calls: 0.into() };

    let (out, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Validate the input before using it and return an error otherwise."),
        "the salvaged partial round alone must recover finding 0's fix"
    );
    assert!(
        out[1].fix_specific.is_some() && out[2].fix_specific.is_some(),
        "findings missing from the salvaged round must be recovered by the automatic \
             next-round retry, not lost: {out:?}"
    );
    assert!(out.iter().all(|f| !f.needs_review));
}

/// A large finding set is split into bounded chunks and every finding survives the
/// round trip — no finding is dropped at a chunk boundary.
#[tokio::test]
async fn verify_findings_chunks_large_finding_sets_and_merges_without_dropping() {
    let n = CALIBRATION_BATCH_SIZE * 2 + 4;
    let findings: Vec<Finding> = (0..n).map(|_| finding("AI-X", "medium")).collect();
    // `CapturingCompleter` always returns `"{}"` (no verdict) — each site degrades to
    // "finding unchanged" and is disclosed as a failed part, which is fine: this test's
    // assertions are about COUNT and CALL-COUNT (no finding dropped, no site skipped), not
    // verdict content.
    let completer = CapturingCompleter::default();

    let (out, _estimation_failed) = verify_findings(
        &completer,
        "me/api",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("This repository has many code files."),
    )
    .await;

    assert_eq!(out.len(), n, "no finding dropped across chunk boundaries");
    assert_eq!(
        completer.seen.lock().unwrap().len(),
        n,
        "every site is asked, in whatever group it falls"
    );
}

// ── C3-1b: run-wide estimation-pass failure is disclosed, never silently dropped ────

/// When the calibration pass fails OUTRIGHT (every call, every finding), no finding gets
/// an effort estimate — this must surface as a `FailedPass{pass: "hour estimation", ..}`
/// so the export can disclose the gap, AND every finding must still ship (calibration
/// never drops anything, estimated or not).
#[tokio::test]
async fn verify_findings_records_a_failed_pass_when_estimation_fails_run_wide() {
    let findings = vec![finding("AI-X", "medium"), finding("AI-Y", "high")];
    let (out, failed_passes) = verify_findings(
        &FailingCompleter,
        "me/api",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("This repository has 2 code files."),
    )
    .await;

    assert_eq!(
        out.len(),
        2,
        "a failed estimation pass must never drop a finding"
    );
    assert!(
        out.iter().all(|f| f.effort.is_none()),
        "no finding got an estimate: {out:?}"
    );
    assert_eq!(
        failed_passes.len(),
        1,
        "a run-wide estimation miss must be disclosed exactly once: {failed_passes:?}"
    );
    assert_eq!(failed_passes[0].repo, "me/api");
    assert_eq!(failed_passes[0].pass, "hour estimation");
}

/// The sanity counterpart: when calibration succeeds and returns real effort estimates,
/// NO failed pass is fabricated.
#[tokio::test]
async fn verify_findings_records_no_failed_pass_when_estimates_land() {
    let resp_text = r#"{"severity":"high","real":"yes","effort":"low","reason":"x"}"#;
    let llm = StubCompleter {
        text: resp_text.to_string(),
    };
    let findings = vec![finding("AI-X", "medium")];
    let (out, failed_passes) = verify_findings(
        &llm,
        "me/api",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("This repository has 1 code files."),
    )
    .await;

    assert_eq!(out[0].effort.as_deref(), Some("low"));
    assert!(
        failed_passes.is_empty(),
        "a successful estimation pass must not fabricate a failure disclosure: \
             {failed_passes:?}"
    );
}

/// Calibration must ACCEPT an explicit `"critical"` verdict and apply it to the finding —
/// this is the D5-shaped case: an AI-tier finding starts at "high" from the raw audit pass,
/// and the calibration pass upgrades it to "critical" when it judges the finding clears the
/// bar (e.g. an unauthenticated privileged operation).
#[test]
fn apply_verdicts_accepts_explicit_critical_verdict() {
    let findings = vec![finding("AI-UNAUTH-CHARGE", "high")];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"unauthenticated charge with service_role"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].severity, "critical",
        "an explicit critical verdict must be applied"
    );
}

/// ANTI-OVER-ROTATION GUARD: ordinary findings must NEVER be auto-escalated to critical.
/// An explicit "high" verdict stays "high" (it is a recognized value, not funneled through
/// the "unknown -> medium" fallback into anything resembling critical), and a verdict that
/// omits `severity` entirely leaves the finding's prior severity untouched — neither path
/// can produce "critical" without the model saying so explicitly.
#[test]
fn apply_verdicts_does_not_auto_escalate_ordinary_findings_to_critical() {
    let findings = vec![
        finding("AI-ORDINARY-HIGH", "medium"), // index 0: explicit "high" verdict
        finding("AI-UNTOUCHED", "high"),       // index 1: verdict omits severity
    ];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"high","real":"yes","reason":"a real but contained bug"},
            {"index":1,"real":"yes","reason":"no severity opinion given"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    let ordinary = out
        .iter()
        .find(|f| f.rule_id == "AI-ORDINARY-HIGH")
        .unwrap();
    assert_eq!(
        ordinary.severity, "high",
        "an explicit high verdict must stay high, never inflate to critical"
    );
    let untouched = out.iter().find(|f| f.rule_id == "AI-UNTOUCHED").unwrap();
    assert_eq!(
        untouched.severity, "high",
        "no severity field in the verdict leaves the finding's prior severity as-is"
    );
}

// ── Upward-calibration precondition guardrail ─────────────────────────────
//
// Calibration review finding: a High "unverified session read" finding whose own note
// says exploitation "requires forging a cookie" was promoted to Critical by calibration
// and took a top-3 do-now slot from a genuinely unconditional critical. These guard the
// fix: a recorded exploit precondition can BLOCK an upward move, never cause a downward
// one.

pub(super) fn finding_with_detail(rule: &str, sev: &str, detail: &str) -> Finding {
    Finding {
        detail: detail.to_string(),
        ..finding(rule, sev)
    }
}

/// A High finding whose own text records an exploit precondition ("requires forging a
/// session cookie") must NOT be raised to Critical even when the calibration model's
/// verdict says so — the raise is refused and the base (authored) severity is kept.
#[test]
fn apply_verdicts_blocks_upward_raise_when_precondition_is_recorded() {
    let findings = vec![finding_with_detail(
        "AI-UNVERIFIED-SESSION-READ",
        "high",
        "Server code authorizes from an unverified session read; exploitation requires \
             forging a session cookie.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"escalate to critical"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].severity, "high",
        "a recorded exploit precondition must block the upward raise to critical"
    );
    assert!(
        out[0]
            .calibration_rationale
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains("precondition"),
        "the block must be recorded on the finding so it's auditable: {:?}",
        out[0].calibration_rationale
    );
}

/// The SAME raising verdict on a High finding with NO precondition language must still
/// go through to Critical — the guardrail must not over-block ordinary calibration.
#[test]
fn apply_verdicts_still_raises_when_no_precondition_is_recorded() {
    let findings = vec![finding_with_detail(
        "AI-UNAUTH-EXPORT",
        "high",
        "The export endpoint returns every user's records with no authorization check.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"unconditionally exploitable with one request"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].severity, "critical",
        "with no precondition recorded, the raise must still apply"
    );
}

/// A finding already AT critical, whose text also records a precondition phrase, must
/// stay critical — the guardrail only ever blocks a RAISE, it must never be read as
/// license to demote an already-critical finding.
#[test]
fn apply_verdicts_never_demotes_an_already_critical_finding_with_a_precondition() {
    let findings = vec![finding_with_detail(
        "AI-ALREADY-CRITICAL",
        "critical",
        "Full account takeover is possible; exploitation requires only an unauthenticated \
             request.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"confirmed critical"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].severity, "critical",
        "an already-critical finding must never be demoted by precondition language"
    );
}

// ── cycle-5 regression: precondition cap must be authoritative on the verdict path too ────
//
// Calibration review: two High-severity planted defects (a mutation gated by an unverified
// session that requires forging a cookie; a public bucket of sensitive documents that
// requires knowing a path) still came out Critical. These pin the verdict-path guard against
// the exact wording of those two classes — path-knowledge preconditions, not just
// cookie/token forging — bidirectional with the "do-not-break" raise test above.

/// A path-knowledge precondition ("requires knowing the exact ... path") must block the
/// upward raise exactly like a cookie-forging precondition does — the guard is phrase-based
/// on "requires ", not scoped to session/cookie language specifically.
#[test]
fn apply_verdicts_blocks_upward_raise_for_public_bucket_path_knowledge_precondition() {
    let findings = vec![finding_with_detail(
        "AI-PUBLIC-BUCKET-SENSITIVE-DOCS",
        "high",
        "A storage bucket of sensitive documents is publicly readable; exploitation requires \
             knowing the exact object path, which is not discoverable by brute force.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"sensitive data exposed"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].severity, "high",
        "a path-knowledge precondition must block the upward raise to critical, same as a \
             cookie-forging precondition"
    );
}

/// UNDER-RATING GUARD (do-not-break): a committed secret with NO precondition language must
/// still raise to Critical through the ordinary verdict path — the precondition guard must
/// never be mistaken for a general brake on critical verdicts.
#[test]
fn apply_verdicts_committed_secret_raises_to_critical_without_precondition() {
    let findings = vec![finding_with_detail(
        "AI-STRIPE-SECRET-COMMITTED",
        "high",
        "A live Stripe secret key is hardcoded in this file and committed to the repository.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"secret committed to version control"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].severity, "critical",
        "a committed secret with no precondition language must raise to critical"
    );
}
