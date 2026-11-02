//! Stage 1 calibration, the per-site first reviewer (review tree design 4.1, WO4).
//!
//! One structured call per site, a code window of 30 lines each side, `real: yes|no|unclear`
//! instead of a confidence, a verdict that never removes a site, the stage 1 model in the ledger,
//! and every request checked against the model's window before it is sent.

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use repo_map::CHARS_PER_TOKEN_ESTIMATE;

// ── Test doubles ───────────────────────────────────────────────────────────────────────────

type Reply = Box<dyn Fn(&LlmRequest) -> anyhow::Result<LlmResponse> + Send + Sync>;

/// Answers every call with `reply(&request)` and records every request.
struct Scripted {
    reply: Reply,
    seen: std::sync::Mutex<Vec<LlmRequest>>,
}

impl Scripted {
    fn new(
        reply: impl Fn(&LlmRequest) -> anyhow::Result<LlmResponse> + Send + Sync + 'static,
    ) -> Self {
        Self {
            reply: Box::new(reply),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Answers every call with the same text.
    fn text(text: &str) -> Self {
        let text = text.to_string();
        Self::new(move |_| Ok(response(&text)))
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().expect("lock").clone()
    }

    /// The requests that are stage 1 calibration calls (their system prompt is stage 1's).
    fn calibration_requests(&self) -> Vec<LlmRequest> {
        self.requests().into_iter().filter(is_stage1).collect()
    }
}

fn is_stage1(req: &LlmRequest) -> bool {
    req.system
        .as_deref()
        .is_some_and(|s| s.starts_with(&verify_system_prompt()))
}

fn response(text: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
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

#[async_trait::async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let out = (self.reply)(&req);
        self.seen.lock().expect("lock").push(req);
        out
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

const GOOD: &str = r#"{"severity":"high","real":"yes","effort":"low","category":"authorization","reason":"reachable without a check"}"#;

fn site(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: "ARCH-X-1".to_string(),
        severity: "medium".to_string(),
        snippet: format!("call_{line}();"),
        detail: "The handler skips the check.".to_string(),
        ..Finding::default()
    }
}

/// A file of `n` lines, `line 1` .. `line n`.
fn numbered_file(path: &str, n: usize) -> (String, String) {
    (
        path.to_string(),
        (1..=n).map(|i| format!("line {i}\n")).collect(),
    )
}

async fn run(
    llm: &Scripted,
    findings: Vec<Finding>,
    files: &[(String, String)],
) -> (Vec<Finding>, Vec<FailedPass>) {
    verify_findings(
        llm,
        "acme/app",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("This is a Next.js repo with 3 code files.").with_files(files),
    )
    .await
}

fn apply(raw: &str, findings: Vec<Finding>) -> Vec<Finding> {
    apply_verdicts(raw, findings)
}

// ── 4.8 item 1: a verdict never removes a site ─────────────────────────────────────────────

#[test]
fn stage1_verdict_never_removes_a_site() {
    let findings = vec![
        site("a.rs", 1),
        site("b.rs", 2),
        site("c.rs", 3),
        site("d.rs", 4),
    ];
    let raw = r#"{"verdicts":[
        {"index":0,"severity":"high","real":"yes","effort":"low","reason":"x"},
        {"index":1,"severity":"high","real":"no","effort":"low","reason":"the guard is two lines up"},
        {"index":2,"severity":"medium","real":"unclear","effort":"medium","reason":"the caller is not shown"}
    ]}"#;
    let out = apply(raw, findings);
    assert_eq!(out.len(), 4, "every site is kept, whatever the verdict");
    // `real` is the only source of the derived confidence.
    assert_eq!(out[0].confidence.as_deref(), Some("high"));
    assert_eq!(out[1].confidence.as_deref(), Some("disputed"));
    assert_eq!(out[2].confidence.as_deref(), Some("needs-review"));
    assert_eq!(out[3].confidence, None, "no verdict, no opinion recorded");
    // A site rated "not real" keeps the severity the rules of thumb gave it: the group reviewer,
    // not stage 1, decides what a likely false alarm is worth.
    assert_eq!(out[1].severity, "high");
}

// ── 4.8 item 2: the stage 1 prompt ─────────────────────────────────────────────────────────

#[tokio::test]
async fn stage1_prompt_contains_no_confidence_instruction() {
    let llm = Scripted::text(GOOD);
    let files = vec![numbered_file("a.rs", 10)];
    let _ = run(&llm, vec![site("a.rs", 5)], &files).await;
    let reqs = llm.calibration_requests();
    assert_eq!(reqs.len(), 1);
    let system = reqs[0].system.clone().expect("system prompt");
    for text in [
        system.to_ascii_lowercase(),
        reqs[0].prompt.to_ascii_lowercase(),
    ] {
        assert!(
            !text.contains("confiden"),
            "no confidence instruction anywhere in the request:\n{text}"
        );
        assert!(!text.contains("treat it as a preference"));
        assert!(!text.contains("needs review"));
    }
    // The first-reviewer framing, and the rules of thumb word for word.
    assert!(system.contains("You are the first reviewer of an automated audit."));
    assert!(system.contains("You never remove one."));
    assert!(system.contains(
        "Unauthenticated access to ANY protected resource or operation, OR a path to full-account"
    ));
    assert!(system.contains(
        "An open or unvalidated redirect (the destination is attacker-controlled or otherwise"
    ));
    assert!(system.contains(
        "A privileged/role/admin check enforced ONLY in client-visible (UI/browser) code"
    ));
    // `unclear` is defined as "the code shown does not settle it".
    assert!(system.contains("the code shown does not settle it"));
    // The prompt works for any capable model: it names no model.
    for name in ["Claude", "Anthropic", "Opus", "Sonnet", "Haiku", "GPT"] {
        assert!(!system.contains(name), "model-agnostic prompt names {name}");
    }
}

#[test]
fn the_stage1_reply_schema_is_the_per_site_verdict() {
    let schema = reply_schemas::stage1_reply_schema();
    let props = schema["properties"].as_object().expect("an object schema");
    for field in ["severity", "real", "effort", "category", "reason"] {
        assert!(props.contains_key(field), "{field} is in the stage 1 reply");
    }
    assert!(!props.contains_key("confidence"), "no confidence field");
    assert_eq!(
        props["real"]["enum"],
        serde_json::json!(["yes", "no", "unclear"])
    );
}

// ── 4.8 item 3: unclear sets needs_review and never escalates ──────────────────────────────

#[test]
fn unclear_sets_needs_review_but_never_escalates_by_itself() {
    let mut high = site("a.rs", 1);
    high.severity = "high".to_string();
    let mut in_test = site("t.rs", 2);
    in_test.in_test = true;
    in_test.severity = "medium".to_string();
    let mut stale = site("s.rs", 3);
    stale.needs_review = true; // a stale flag from an earlier pass
    stale.severity = "medium".to_string();

    let raw = r#"{"verdicts":[
        {"index":0,"severity":"medium","real":"unclear","effort":"low","reason":"the caller is not shown"},
        {"index":1,"severity":"medium","real":"yes","effort":"low","reason":"fine"},
        {"index":2,"severity":"medium","real":"yes","effort":"low","reason":"fine"}
    ]}"#;
    let out = apply(raw, vec![high, in_test, stale]);

    // unclear: flagged for review, severity is the verdict's and was not raised.
    assert!(out[0].needs_review, "unclear sets needs_review");
    assert_eq!(out[0].confidence.as_deref(), Some("needs-review"));
    assert_eq!(out[0].severity, "medium", "unclear does not raise severity");
    assert!(
        out[0].calibration_rationale.is_none(),
        "nothing else changed"
    );
    // test code: flagged for review even when the site is real; confidence still comes from `real`.
    assert!(out[1].needs_review, "test code sets needs_review");
    assert_eq!(out[1].confidence.as_deref(), Some("high"));
    // a real site in production code clears a stale flag.
    assert!(!out[2].needs_review, "needs_review is derived, not sticky");
    // No site changed severity because of the flag.
    assert_eq!(out[1].severity, "medium");
    assert_eq!(out[2].severity, "medium");
}

// ── 4.8 item 13: thorough consensus is removed ─────────────────────────────────────────────

/// A scan reply with two findings at two lines of `src/a.rs`, a calibration reply per site.
fn scan_and_calibration_stub() -> Scripted {
    Scripted::new(|req| {
        if is_stage1(req) {
            Ok(response(GOOD))
        } else {
            Ok(response(
                r#"{"findings":[
                    {"rule":"ARCH-X-1","severity":"high","title":"first","path":"src/a.rs","line":2,"code":"call_two();","detail":"one"},
                    {"rule":"ARCH-X-1","severity":"high","title":"second","path":"src/a.rs","line":4,"code":"call_four();","detail":"two"}
                ],"proposed_rules":[],"needs_files":[]}"#,
            ))
        }
    })
}

fn scan_files() -> Vec<(String, String)> {
    vec![(
        "src/a.rs".to_string(),
        "fn a() {\n    call_two();\n    let x = 1;\n    call_four();\n}\n".to_string(),
    )]
}

async fn audit(
    llm: &Scripted,
    thorough: bool,
    calibration_model: Option<&str>,
) -> (Vec<Finding>, Vec<crate::scan_ledger::StageSample>) {
    let selected = vec![("ARCH-X-1".to_string(), "Check before you call.".to_string())];
    let (findings, _proposed, _recs, _failed, samples) = audit_repo(
        llm,
        "me/api",
        &scan_files(),
        &crate::scan_routing::rules_from_pairs(&selected),
        &[],
        &std::collections::HashMap::new(),
        None,
        calibration_model,
        ScanMode::Parallel,
        thorough,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("the stub never errors");
    (findings, samples)
}

#[tokio::test]
async fn thorough_flag_no_longer_runs_three_passes() {
    let llm = scan_and_calibration_stub();
    let (findings, _) = audit(&llm, true, None).await;
    let sites = findings.iter().filter(|f| f.rule_id == "ARCH-X-1").count();
    assert_eq!(sites, 2, "the scan produced two sites");
    assert_eq!(
        llm.calibration_requests().len(),
        sites,
        "one call per site, however thorough is set"
    );
    // The same count with the flag off: the flag changes nothing.
    let plain = scan_and_calibration_stub();
    let _ = audit(&plain, false, None).await;
    assert_eq!(
        plain.calibration_requests().len(),
        llm.calibration_requests().len()
    );
}

#[test]
fn the_thorough_flag_prints_a_removal_notice_for_one_release() {
    let notice = thorough_notice(true).expect("the flag set prints a notice");
    assert!(notice.contains("--thorough"));
    assert!(notice.contains("one pass"));
    assert!(
        thorough_notice(false).is_none(),
        "no notice without the flag"
    );
}

// ── The code window: 30 lines each side, clamped at the file edges ─────────────────────────

fn window_of(n: usize, line: usize) -> CodeWindow {
    let content: String = (1..=n).map(|i| format!("line {i}\n")).collect();
    code_window(&content, line, CODE_WINDOW_RADIUS)
}

#[test]
fn the_code_window_covers_30_lines_each_side_clamped_at_the_file_edges() {
    assert_eq!(CODE_WINDOW_RADIUS, 30);
    // Mid-file: 30 lines each side of the site.
    let w = window_of(200, 100);
    assert_eq!((w.first_line, w.last_line), (70, 130));
    assert_eq!(w.text.lines().count(), 61);
    // Near the top: clamped at line 1, the far side still reaches 30 past the site.
    let w = window_of(200, 5);
    assert_eq!((w.first_line, w.last_line), (1, 35));
    // The first and last lines of the file.
    let w = window_of(200, 1);
    assert_eq!((w.first_line, w.last_line), (1, 31));
    let w = window_of(200, 200);
    assert_eq!((w.first_line, w.last_line), (170, 200));
    // Near the bottom.
    let w = window_of(200, 195);
    assert_eq!((w.first_line, w.last_line), (165, 200));
    // A file shorter than the window is shown whole.
    let w = window_of(10, 4);
    assert_eq!((w.first_line, w.last_line), (1, 10));
    // A line past the end of the file clamps to the last line.
    let w = window_of(40, 90);
    assert_eq!(w.last_line, 40);
    assert!(w.first_line <= 40);
    // The window's lines carry their real numbers.
    let w = window_of(200, 100);
    assert!(w
        .text
        .lines()
        .next()
        .expect("a line")
        .trim_start()
        .starts_with("70|"));
    assert!(w
        .text
        .lines()
        .last()
        .expect("a line")
        .trim_start()
        .starts_with("130|"));
    // No line number, or an empty file, shows nothing.
    assert_eq!(window_of(200, 0), CodeWindow::default());
    assert_eq!(
        code_window("", 3, CODE_WINDOW_RADIUS),
        CodeWindow::default()
    );
}

#[tokio::test]
async fn the_request_for_a_site_carries_its_window_and_nothing_beyond_it() {
    let llm = Scripted::text(GOOD);
    let files = vec![numbered_file("a.rs", 200)];
    let _ = run(&llm, vec![site("a.rs", 100)], &files).await;
    let reqs = llm.calibration_requests();
    assert_eq!(reqs.len(), 1);
    let prompt = &reqs[0].prompt;
    assert!(prompt.contains("line 70\n"), "30 lines above the site");
    assert!(prompt.contains("line 130\n"), "30 lines below the site");
    assert!(!prompt.contains("line 69\n"), "no more than 30 above");
    assert!(!prompt.contains("line 131\n"), "no more than 30 below");
    // Stage 1 is sent the window, not the digest of the whole repo.
    assert!(!prompt.contains("line 1\n"));
    assert!(!prompt.contains("line 200\n"));
    // The corpus rule text rides with the site when the run knows it.
    let llm = Scripted::text(GOOD);
    let rules = vec![("ARCH-X-1".to_string(), "Check before you call.".to_string())];
    let _ = verify_findings(
        &llm,
        "acme/app",
        vec![site("a.rs", 100)],
        None,
        None,
        None,
        &Stage1Inputs::new("shape")
            .with_files(&files)
            .with_rules(&rules),
    )
    .await;
    assert!(llm.calibration_requests()[0]
        .prompt
        .contains("Check before you call."));
}

// ── One call per site, grouped, in order ───────────────────────────────────────────────────

#[tokio::test]
async fn each_site_is_its_own_call_and_gets_its_own_verdict() {
    // Reply by the site in the request: line 1 is real, line 2 is not, line 3 is unclear.
    let llm = Scripted::new(|req| {
        let (real, sev) = if req.prompt.contains("call_1();") {
            ("yes", "critical")
        } else if req.prompt.contains("call_2();") {
            ("no", "low")
        } else {
            ("unclear", "medium")
        };
        Ok(response(&format!(
            r#"{{"severity":"{sev}","real":"{real}","effort":"high","category":"injection","reason":"r"}}"#
        )))
    });
    let files = vec![numbered_file("a.rs", 10)];
    let (out, failed) = run(
        &llm,
        vec![site("a.rs", 1), site("a.rs", 2), site("a.rs", 3)],
        &files,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        llm.calibration_requests().len(),
        3,
        "three sites, three calls"
    );
    for req in llm.calibration_requests() {
        let n = ["call_1();", "call_2();", "call_3();"]
            .iter()
            .filter(|s| req.prompt.contains(**s))
            .count();
        assert_eq!(n, 1, "a request holds exactly one site:\n{}", req.prompt);
    }
    assert_eq!(
        out.iter().map(|f| f.line).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the order of the sites is kept"
    );
    assert_eq!(out[0].severity, "critical");
    assert_eq!(out[1].severity, "low");
    assert_eq!(
        out.iter().map(|f| f.confidence.clone()).collect::<Vec<_>>(),
        vec![
            Some("high".to_string()),
            Some("disputed".to_string()),
            Some("needs-review".to_string())
        ]
    );
    assert!(out.iter().all(|f| f.effort.as_deref() == Some("high")));
}

#[tokio::test]
async fn sites_beyond_one_group_each_still_get_a_call_and_a_verdict() {
    let n = CALIBRATION_BATCH_SIZE * 2 + 5;
    let llm = Scripted::text(GOOD);
    let files = vec![numbered_file("a.rs", 10)];
    let findings: Vec<Finding> = (0..n).map(|i| site("a.rs", 1 + i % 10)).collect();
    let (out, failed) = run(&llm, findings, &files).await;
    assert!(failed.is_empty());
    assert_eq!(out.len(), n);
    assert_eq!(llm.calibration_requests().len(), n);
    assert!(out.iter().all(|f| f.confidence.as_deref() == Some("high")));
}

#[tokio::test]
async fn concurrent_sites_keep_their_own_verdicts_in_order() {
    let llm = Scripted::new(|req| {
        let sev = if req.prompt.contains("call_1();") {
            "low"
        } else {
            "high"
        };
        Ok(response(&format!(
            r#"{{"severity":"{sev}","real":"yes","effort":"low","reason":"r"}}"#
        )))
    });
    let files = vec![numbered_file("a.rs", 10)];
    let findings = vec![site("a.rs", 1), site("a.rs", 2), site("a.rs", 3)];
    let (out, _) = verify_findings(
        &llm,
        "acme/app",
        findings,
        None,
        None,
        None,
        &Stage1Inputs::new("shape")
            .with_files(&files)
            .with_concurrency(4),
    )
    .await;
    assert_eq!(
        out.iter().map(|f| f.severity.as_str()).collect::<Vec<_>>(),
        vec!["low", "high", "high"]
    );
}

// ── A3: the model ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_stage1_model_is_recorded_in_the_ledger() {
    let llm = scan_and_calibration_stub();
    let (_, samples) = audit(&llm, false, Some("claude-opus-5")).await;
    let sample = samples
        .iter()
        .find(|s| s.stage == "ai-calibration")
        .expect("the calibration stage is sampled");
    assert_eq!(sample.model.as_deref(), Some("claude-opus-5"));
    for req in llm.calibration_requests() {
        assert_eq!(req.model, "claude-opus-5", "the pick is the model asked");
    }
    // It reaches the ledger entry.
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    let sample = samples
        .into_iter()
        .find(|s| s.stage == "ai-calibration")
        .expect("sample");
    let entry = ledger.record_stage_sample(sample);
    assert_eq!(entry.model.as_deref(), Some("claude-opus-5"));
    let json = serde_json::to_string(&ledger).expect("ledger serializes");
    assert!(json.contains("claude-opus-5"));

    // Without a pick the stage runs on the scan model, and the ledger says which.
    let llm = scan_and_calibration_stub();
    let (_, samples) = audit(&llm, false, None).await;
    let sample = samples
        .iter()
        .find(|s| s.stage == "ai-calibration")
        .expect("sample");
    assert_eq!(
        sample.model.as_deref(),
        Some(resolve_audit_model(None).as_str())
    );
}

#[test]
fn the_stage1_model_comes_from_the_flag_or_the_project_setting_never_the_environment() {
    assert_eq!(resolve_calibration_model(Some("pick"), "scan"), "pick");
    assert_eq!(resolve_calibration_model(None, "scan"), "scan");
    assert_eq!(resolve_calibration_model(Some("  "), "scan"), "scan");
    // The stage reads no environment variable at all.
    let source = include_str!("calibration.rs");
    let env_read = ["env", "::var"].concat();
    assert!(
        !source.contains(&env_read),
        "stage 1 must not read the environment"
    );
}

// ── A7: every request is checked against the window ────────────────────────────────────────

#[tokio::test]
async fn a_site_whose_request_does_not_fit_is_split_or_disclosed_never_sent_oversized() {
    // How many bytes a request may carry for the model this run asks, whatever its window is.
    let limit_bytes =
        request_budget::input_limit_tokens(&resolve_audit_model(None), calibration_max_tokens(1))
            * CHARS_PER_TOKEN_ESTIMATE;
    // Split: 61 lines that together are half as large again as the window do not fit; 31 of
    // them do. The code shown shrinks until the request fits, and the site is still asked.
    let wide = "x".repeat(limit_bytes / 40);
    let content: String = (1..=200).map(|i| format!("{wide} {i}\n")).collect();
    let files = vec![("wide.rs".to_string(), content)];
    let llm = Scripted::text(GOOD);
    let (out, failed) = run(&llm, vec![site("wide.rs", 100)], &files).await;
    let reqs = llm.calibration_requests();
    assert_eq!(reqs.len(), 1, "the site is still asked once");
    for req in &reqs {
        assert!(
            request_budget::ensure_request_fits(req).is_ok(),
            "an oversized request was sent"
        );
    }
    assert!(
        !reqs[0].prompt.contains(&format!("{wide} 70\n")),
        "the window shrank from 30 lines a side"
    );
    assert_eq!(
        out[0].confidence.as_deref(),
        Some("high"),
        "the verdict landed"
    );
    assert!(failed.is_empty(), "{failed:?}");

    // Disclosed: a site whose own evidence is over the window cannot be split. Nothing is sent
    // for it, the finding passes through unchanged and the part is disclosed by name.
    let mut huge = site("a.rs", 3);
    huge.snippet = "y".repeat(limit_bytes * 2);
    let llm = Scripted::text(GOOD);
    let files = vec![numbered_file("a.rs", 10)];
    let (out, failed) = run(&llm, vec![huge.clone(), site("a.rs", 4)], &files).await;
    assert_eq!(out.len(), 2, "no site is dropped");
    assert_eq!(
        out[0].severity, huge.severity,
        "the refused site is unchanged"
    );
    assert_eq!(out[0].confidence, None);
    assert_eq!(
        out[1].confidence.as_deref(),
        Some("high"),
        "the other site is asked"
    );
    for req in llm.calibration_requests() {
        assert!(
            request_budget::ensure_request_fits(&req).is_ok(),
            "an oversized request was sent"
        );
        assert!(!req.prompt.contains(&"y".repeat(1000)));
    }
    assert_eq!(
        llm.calibration_requests().len(),
        1,
        "only the site that fits is sent"
    );
    assert_eq!(
        failed.len(),
        1,
        "the refusal is a disclosed failed part: {failed:?}"
    );
    assert!(failed[0].reason.contains("a.rs:3"), "{}", failed[0].reason);
    assert!(
        failed[0].reason.contains("does not fit"),
        "{}",
        failed[0].reason
    );
}

// ── No [needs review: ...] in client text ──────────────────────────────────────────────────

#[tokio::test]
async fn client_text_carries_no_needs_review_suffix() {
    let llm = Scripted::text(
        r#"{"severity":"medium","real":"unclear","effort":"low","reason":"the caller is not shown"}"#,
    );
    let files = vec![numbered_file("a.rs", 10)];
    let mut test_site = site("a.rs", 2);
    test_site.in_test = true;
    let (out, _) = run(&llm, vec![site("a.rs", 1), test_site], &files).await;
    for f in &out {
        assert!(f.needs_review, "unclear and test code are flagged");
        assert!(
            !f.detail.contains("[needs review"),
            "the suffix is gone: {}",
            f.detail
        );
        assert!(!f.detail.to_ascii_lowercase().contains("needs review"));
    }
    // Through the envelope reader too, with and without a reason.
    let raw = r#"{"verdicts":[
        {"index":0,"severity":"medium","real":"unclear","effort":"low","reason":"why"},
        {"index":1,"severity":"medium","real":"unclear","effort":"low","reason":""}
    ]}"#;
    for f in apply(raw, vec![site("a.rs", 1), site("a.rs", 2)]) {
        assert!(!f.detail.contains("[needs review"), "{}", f.detail);
    }
}

// ── The precondition hold is an outlier reason on the row ──────────────────────────────────

#[test]
fn a_held_upward_raise_is_recorded_on_the_row_as_severity_raise_held() {
    let mut f = site("a.rs", 1);
    f.severity = "high".to_string();
    f.detail = "Exploiting this requires forging a session cookie.".to_string();
    let raw = r#"{"verdicts":[{"index":0,"severity":"critical","real":"yes","effort":"low","reason":"r"}]}"#;
    let out = apply(raw, vec![f]);
    assert_eq!(out[0].severity, "high", "the raise is held");
    assert!(out[0].calibration_rationale.is_some());
    assert_eq!(
        outlier_reasons_of(&out[0]),
        vec![crate::review_tree::OutlierReason::SeverityRaiseHeld]
    );
    // A verdict that does not raise records nothing.
    let clean = apply(raw, vec![site("a.rs", 2)]);
    assert!(outlier_reasons_of(&clean[0]).is_empty());
}

#[test]
fn the_stage1_verdict_stays_on_the_row_for_the_group_reviewer() {
    let raw = r#"{"verdicts":[{"index":0,"severity":"high","real":"no","effort":"low","reason":"guarded two lines up"}]}"#;
    let out = apply(raw, vec![site("a.rs", 1)]);
    let v = stage1_of(&out[0]).expect("the stage 1 verdict is recorded on the row");
    assert_eq!(v.real, Real::No);
    assert_eq!(v.reason, "guarded two lines up");
}

// ── Failures: a site that cannot be asked is a disclosed failed part ───────────────────────

#[tokio::test]
async fn a_failed_site_is_disclosed_by_name_and_the_others_are_calibrated() {
    let llm = Scripted::new(|req| {
        if req.prompt.contains("call_2();") {
            Err(anyhow::anyhow!("connection reset"))
        } else {
            Ok(response(GOOD))
        }
    });
    let files = vec![numbered_file("a.rs", 10)];
    let (out, failed) = run(
        &llm,
        vec![site("a.rs", 1), site("a.rs", 2), site("a.rs", 3)],
        &files,
    )
    .await;
    assert_eq!(out.len(), 3, "no site is dropped");
    assert_eq!(out[0].confidence.as_deref(), Some("high"));
    assert_eq!(
        out[1].confidence, None,
        "the failed site passes through unchanged"
    );
    assert_eq!(out[2].confidence.as_deref(), Some("high"));
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].reason.contains("a.rs:2"), "{}", failed[0].reason);
    assert!(!failed[0].reason.contains("a.rs:1"));
}

#[tokio::test]
async fn an_unusable_reply_is_a_disclosed_failed_part_not_a_silent_gap() {
    let llm = Scripted::new(|req| {
        if req.prompt.contains("call_2();") {
            Ok(response("I could not decide."))
        } else {
            Ok(response(GOOD))
        }
    });
    let files = vec![numbered_file("a.rs", 10)];
    let (out, failed) = run(&llm, vec![site("a.rs", 1), site("a.rs", 2)], &files).await;
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].confidence, None);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].reason.contains("a.rs:2"), "{}", failed[0].reason);
}

// ── What WO7 wraps: the batch-shaped interface over the same sites ─────────────────────────

#[test]
fn the_sites_expose_one_request_each_for_the_batch_runner() {
    let files = vec![numbered_file("a.rs", 10), numbered_file("b.rs", 10)];
    let findings = vec![site("a.rs", 2), site("b.rs", 3), site("a.rs", 9)];
    let inputs = Stage1Inputs::new("shape").with_files(&files);
    let mut items = Stage1Items::build("acme/app", &findings, Some("claude-opus-5"), &inputs);

    let requests = items.items();
    assert_eq!(requests.len(), 3, "one request per site");
    assert_eq!(
        requests.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    for (id, req) in &requests {
        assert_eq!(req.model, "claude-opus-5");
        assert!(req.response_schema.is_some(), "structured output");
        assert!(request_budget::ensure_request_fits(req).is_ok());
        assert!(req.prompt.contains(&findings[*id].path));
    }
    assert!(items.refused().is_empty());

    // A good reply is absorbed; a reply that is not a verdict is refused with a reason.
    assert!(items.absorb(0, &response(GOOD)).is_ok());
    let rejected = items
        .absorb(1, &response("no verdict here"))
        .expect_err("not a verdict");
    assert!(!rejected.truncated);
    assert!(!rejected.reason.is_empty());
    // A reply cut off at the output limit is retried once with a larger cap; any other is not.
    let mut cut = response(r#"{"severity":"high","real":"y"#);
    cut.truncated = true;
    let rejected_cut = items.absorb(2, &cut).expect_err("cut off");
    assert!(rejected_cut.truncated);
    let retry = items
        .retry(2, &rejected_cut)
        .expect("a retry for a cut-off reply");
    assert!(retry.max_tokens > requests[2].1.max_tokens);
    assert!(
        items.retry(1, &rejected).is_none(),
        "no retry for an unusable reply"
    );

    let out = items.into_findings();
    assert_eq!(out.len(), 3, "no site is dropped");
    assert_eq!(out[0].confidence.as_deref(), Some("high"));
    assert_eq!(out[1].confidence, None);
    assert_eq!(out[2].confidence, None);
}

// ── Prompt size on a scripted fixture run ──────────────────────────────────────────────────

/// The fixture the report quotes: 12 sites over 3 files of 120 lines. Prints the bytes of
/// every stage 1 request so the before and after can be compared on the same input.
#[tokio::test]
async fn prompt_size_per_site_on_the_scripted_fixture() {
    let files = vec![
        numbered_file("src/file0.ts", 120),
        numbered_file("src/file1.ts", 120),
        numbered_file("src/file2.ts", 120),
    ];
    let findings: Vec<Finding> = (0..12)
        .map(|i| {
            let mut f = site(&format!("src/file{}.ts", i % 3), 20 + i * 5);
            f.snippet = format!("const value{i} = db.query(userInput{i});");
            f.detail =
                "User input reaches the query without validation. It is concatenated.".to_string();
            f.severity = "high".to_string();
            f
        })
        .collect();
    let llm = Scripted::text(GOOD);
    let _ = run(&llm, findings, &files).await;
    let reqs = llm.calibration_requests();
    let system: usize = reqs
        .iter()
        .map(|r| r.system.as_ref().map_or(0, String::len))
        .sum();
    let user: usize = reqs.iter().map(|r| r.prompt.len()).sum();
    eprintln!(
        "STAGE1 PROMPT SIZE calls={} total_bytes={} system_bytes={} user_bytes={} \
         per_site_total={} per_site_user={}",
        reqs.len(),
        system + user,
        system,
        user,
        (system + user) / 12,
        user / 12
    );
    assert_eq!(reqs.len(), 12);
}

// ── `real: no` never throws a site out ─────────────────────────────────────────────────────

/// Only the architect throws a row out. A verdict of `real: no` leaves an ordinary row: no discard
/// mark, no discard-shaped text for the junk detection to read, still in `findings` after the
/// step that removes rows the model's own text says to discard. This holds even when the model's
/// reason itself says "discard this finding".
#[test]
fn a_real_no_verdict_keeps_the_row_with_no_discard_mark_or_discard_shaped_text() {
    let mut invented = site("a.rs", 1);
    invented.rule_id = "AI-SOME-INVENTED-RULE".to_string();
    invented.detail = "The handler skips the check.".to_string();
    let adopted = site("b.rs", 2);
    let raw = r#"{"verdicts":[
        {"index":0,"severity":"low","real":"no","effort":"low","reason":"This is a placeholder statement. Discard this finding."},
        {"index":1,"severity":"low","real":"no","effort":"low","reason":"Discard this finding, it is not real."}
    ]}"#;
    let mut rows = apply(raw, vec![invented, adopted]);
    for f in &rows {
        assert_eq!(f.confidence.as_deref(), Some("disputed"));
        assert!(
            !f.needs_review,
            "real: no flags nothing for review by itself"
        );
        assert!(
            !f.detail.to_ascii_lowercase().contains("discard"),
            "no discard-shaped text reaches the row's explanation: {}",
            f.detail
        );
        assert!(
            f.captures.keys().all(|k| !k.contains("discard")),
            "no discard mark: {:?}",
            f.captures.keys().collect::<Vec<_>>()
        );
        assert_eq!(f.status, "active");
    }
    // The reason is still kept for the group reviewer, in its own field.
    assert!(stage1_of(&rows[0])
        .expect("kept")
        .reason
        .contains("Discard"));
    // The step that marks model-invented rows the model's own text says to discard reads the
    // row's explanation and headline only, never the stage 1 record, so it marks neither.
    let marked = mark_discarded_invented_rows("acme/app", &mut rows);
    assert_eq!(marked, 0, "a real: no verdict throws nothing out");
    assert_eq!(rows.len(), 2, "both sites stay in findings");
    assert!(
        rows.iter().all(|f| !crate::engine_discard::is_discarded(f)),
        "no engine_discarded mark on either site"
    );
    // The stage 1 record rides beside M4's captures as bookkeeping, not as an object name.
    for key in [STAGE1_CAPTURE, OUTLIER_REASONS_CAPTURE] {
        assert!(BOOKKEEPING_CAPTURE_KEYS.contains(&key), "{key}");
    }
    assert!(BOOKKEEPING_CAPTURE_KEYS.contains(&crate::review_flags::REVIEW_FLAGS_CAPTURE));
    assert!(BOOKKEEPING_CAPTURE_KEYS.contains(&crate::junk_finding::JUNK_CONTENT_CAPTURE));
}

// ── One call per site must not re-pay for the instructions ─────────────────────────────────

/// Every site's request opens with the same text (the instructions in the system prompt, then the
/// repository line and the proportionality paragraph), and a cache breakpoint marks the end of
/// it, so the API can serve the prefix from its prompt cache instead of charging it per site.
#[tokio::test]
async fn every_site_request_marks_the_shared_prefix_for_the_prompt_cache() {
    let llm = Scripted::text(GOOD);
    let files = vec![numbered_file("a.rs", 40), numbered_file("b.rs", 40)];
    let _ = run(
        &llm,
        vec![site("a.rs", 5), site("b.rs", 20), site("a.rs", 33)],
        &files,
    )
    .await;
    let reqs = llm.calibration_requests();
    assert_eq!(reqs.len(), 3);
    let cut = |r: &LlmRequest| {
        assert_eq!(r.cache_breakpoints.len(), 1, "one breakpoint");
        let at = r.cache_breakpoints[0];
        assert!(at > 0 && at < r.prompt.len());
        assert!(r.prompt.is_char_boundary(at));
        r.prompt[..at].to_string()
    };
    let prefix = cut(&reqs[0]);
    assert!(prefix.contains("Repository: acme/app"));
    assert!(
        !prefix.contains("call_5();"),
        "no site text in the shared prefix"
    );
    for r in &reqs[1..] {
        assert_eq!(cut(r), prefix, "the same prefix for every site");
        assert_eq!(r.system, reqs[0].system, "and the same system prompt");
    }
}

// ── The client's explanation carries no engine bookkeeping ─────────────────────────────────

/// A calibrated site's client `detail` is exactly the text it had before calibration: no
/// `[calibrated: ...]` and no `[needs review: ...]`, whatever the verdict. The reason lives in the
/// row's stage 1 record and nowhere in client text.
#[test]
fn a_calibrated_sites_client_detail_is_exactly_its_pre_calibration_text() {
    for (real, reason) in [
        ("yes", "reachable without a check"),
        ("unclear", "the caller is not shown"),
        ("no", "the guard is two lines up"),
    ] {
        let before = site("a.rs", 1);
        let raw = format!(
            r#"{{"verdicts":[{{"index":0,"severity":"high","real":"{real}","effort":"low","reason":"{reason}"}}]}}"#
        );
        let out = apply(&raw, vec![before.clone()]).remove(0);
        assert_eq!(out.detail, before.detail, "real: {real}");
        assert!(!out.detail.contains("[calibrated"));
        assert!(!out.detail.contains("[needs review"));
        assert_eq!(stage1_of(&out).expect("kept on the row").reason, reason);
    }
}

// ── The estimator's figures are the engine's ───────────────────────────────────────────────

/// The pre-flight prices stage 1 from constants in ui-core. They must be the engine's own: the
/// reply budget per site is the one the request cap is built from, and the shared prefix (system
/// prompt plus shared opening) is the size the real prompt is, within a fifth.
#[test]
fn the_estimator_prices_stage_one_from_the_engines_own_figures() {
    use camerata_ui_core::scan::{CALIBRATION_OUTPUT_TOKENS_PER_SITE, CALIBRATION_PREFIX_TOKENS};
    assert_eq!(
        CALIBRATION_OUTPUT_TOKENS_PER_SITE,
        f64::from(VERIFY_TOKENS_PER_FINDING)
    );
    let items = Stage1Items::build(
        "acme/app",
        &[site("a.rs", 1)],
        None,
        &Stage1Inputs::new("This is a Next.js repo with 3 code files."),
    );
    let prefix_bytes = items.system_len() + items.shared_prefix().len();
    let tokens = prefix_bytes.div_ceil(CHARS_PER_TOKEN_ESTIMATE) as f64;
    // The estimate is made with 4 characters a token, as the rest of the estimator is, so compare
    // on the same footing.
    let tokens_at_four = prefix_bytes as f64 / 4.0;
    assert!(
        (tokens_at_four - CALIBRATION_PREFIX_TOKENS).abs() < 0.2 * CALIBRATION_PREFIX_TOKENS
            || (tokens - CALIBRATION_PREFIX_TOKENS).abs() < 0.2 * CALIBRATION_PREFIX_TOKENS,
        "the prefix is {prefix_bytes} bytes ({tokens_at_four} tokens at 4 a token) against the \
         estimator's {CALIBRATION_PREFIX_TOKENS}"
    );
}
