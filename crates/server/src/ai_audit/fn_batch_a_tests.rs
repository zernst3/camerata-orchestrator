//! False-negative audit, batch A (the AI tier): every place a real finding could be dropped,
//! downgraded or hidden now reports it with a disclosed note. One section per item (H1, H2, H5,
//! H7, M1, M2, M3, M4, M5, M6, M7). No network: every model is a scripted stub.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";
const REPO_MAP: &str = "REPO MAP:\na.rs: fn a()\n";

/// One complete finding followed by one that the output limit cut off mid-object.
const CUT_OFF: &str = r#"{"findings":[{"path":"a.rs","line":3,"severity":"high","rule":"RULE-0","title":"Complete one","code":"let a = 1;","detail":"d { with \"braces\" }"},{"path":"b.rs","li"#;
/// A cut-off reply with no complete finding at all.
const CUT_OFF_EMPTY: &str = r#"{"findings":[{"path":"a.rs","line":3,"sever"#;
const TWO_COMPLETE_ONE_CUT: &str = r#"```json
{"findings":[
 {"path":"a.rs","line":1,"severity":"low","rule":"RULE-0","title":"One","code":"x = 1","detail":"first"},
 {"path":"a.rs","line":2,"severity":"critical","rule":"RULE-0","title":"Two","code":"y = {2}","detail":"second \" quote"},
 {"path":"a.rs","line":3,"severity":"high","rule":"RULE-0","title":"Thr"#;

/// Answers each call with the next scripted reply; the last reply repeats once the script runs
/// out.
struct ScriptedLlm {
    replies: Mutex<Vec<(String, bool)>>,
    calls: AtomicUsize,
}

impl ScriptedLlm {
    fn new(replies: &[(&str, bool)]) -> Self {
        Self {
            replies: Mutex::new(replies.iter().map(|(t, c)| (t.to_string(), *c)).collect()),
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn response(text: &str, truncated: bool) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated,
    }
}

#[async_trait::async_trait]
impl LlmPort for ScriptedLlm {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut replies = self.replies.lock().unwrap();
        let (text, truncated) = if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies[0].clone()
        };
        Ok(response(&text, truncated))
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

async fn scan(llm: &ScriptedLlm, n_files: usize) -> PassesOut {
    let files: Vec<(String, String)> = (0..n_files)
        .map(|i| (format!("f{i}.rs"), format!("fn f{i}() {{}}")))
        .collect();
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules = vec![("RULE-0".to_string(), "rule zero".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let adopted: std::collections::HashSet<String> = ["RULE-0".to_string()].into();
    run_passes(
        llm,
        REPO,
        REPO_MAP,
        &crate::prompt_frame::FrameNonce::generate(),
        &adopted,
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        1,
        "pass",
        "fna",
        None,
        false,
    )
    .await
}

fn adopted() -> std::collections::HashSet<String> {
    ["RULE-0".to_string()].into()
}

fn finding(rule: &str, sev: &str, detail: &str) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: "src/h.rs".to_string(),
        line: 3,
        rule_id: rule.to_string(),
        severity: sev.to_string(),
        snippet: "db.get(invoice_id)".to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

fn adjustments(f: &Finding) -> Vec<String> {
    f.captures
        .get(SEVERITY_ADJUSTMENTS_KEY)
        .map(|v| v.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

// ── H1: a reply still cut off after the retry ───────────────────────────────────────────────

#[test]
fn salvage_returns_every_complete_finding_and_stops_at_the_cut() {
    let (f, _p) = salvage_complete_findings(REPO, TWO_COMPLETE_ONE_CUT, &adopted());
    assert_eq!(f.len(), 2, "{f:?}");
    assert_eq!(f[0].severity, "low");
    assert_eq!(f[1].severity, "critical");
    // Braces and quotes inside a string value do not end an object early.
    assert!(f[1].snippet.contains("{2}"), "{}", f[1].snippet);
    assert!(f[1].detail.contains("second \" quote"), "{}", f[1].detail);
}

#[test]
fn salvage_of_a_reply_with_no_complete_finding_is_empty_not_a_panic() {
    assert!(salvage_complete_findings(REPO, CUT_OFF_EMPTY, &adopted())
        .0
        .is_empty());
    assert!(
        salvage_complete_findings(REPO, "no json at all", &adopted())
            .0
            .is_empty()
    );
    assert!(
        salvage_complete_findings(REPO, "{\"findings\": [", &adopted())
            .0
            .is_empty()
    );
}

#[tokio::test]
async fn a_reply_cut_off_after_the_retry_is_a_failed_pass_that_keeps_its_complete_findings() {
    let llm = ScriptedLlm::new(&[(CUT_OFF, true)]);
    let (findings, _p, _r, ok, failures) = scan(&llm, 3).await;
    assert_eq!(
        llm.calls(),
        2,
        "the first send plus the one truncation retry"
    );
    assert_eq!(ok, 0, "a cut-off pass earns no `ran` credit");
    assert_eq!(
        findings.len(),
        1,
        "the complete finding is kept: {findings:?}"
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(
        failures[0].files, 3,
        "it names how many files went unreviewed"
    );
    assert_eq!(failures[0].recovered, Some(1));
    assert!(
        failures[0]
            .error
            .contains("cut off at the output limit after 2 attempts"),
        "{}",
        failures[0].error
    );
    let rows = failed_pass_rows(REPO, ok, &failures);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].files_not_reviewed, 3);
    assert!(
        rows[0].reason.contains("1 complete finding recovered"),
        "{}",
        rows[0].reason
    );
    assert!(
        rows[0].reason.contains("past the cut-off is missing"),
        "{}",
        rows[0].reason
    );
}

#[tokio::test]
async fn a_cut_off_reply_with_nothing_complete_is_a_failed_pass_with_no_findings() {
    let llm = ScriptedLlm::new(&[(CUT_OFF_EMPTY, true)]);
    let (findings, _p, _r, ok, failures) = scan(&llm, 2).await;
    assert_eq!((ok, findings.len(), failures.len()), (0, 0, 1));
    assert_eq!(failures[0].files, 2);
    assert_eq!(failures[0].recovered, Some(0));
}

#[tokio::test]
async fn audit_repo_keeps_the_recovered_findings_instead_of_failing_the_whole_repo() {
    // Every pass is cut off, but one carried a complete finding: the repo is not "all passes
    // failed" (which would drop it); the finding ships and the gap is a disclosed failed pass.
    let llm = ScriptedLlm::new(&[(CUT_OFF, true)]);
    let files = vec![(
        "a.rs".to_string(),
        "let a = 1;\nfn b() {}\nlet a = 1;\n".to_string(),
    )];
    let selected = vec![("RULE-0".to_string(), "Rule zero.".to_string())];
    let (findings, _p, _recs, failed_passes, _samples) = audit_repo(
        &llm,
        REPO,
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
    .expect("a recovered finding keeps the repo's result");
    assert!(findings.iter().any(|f| f.path == "a.rs"), "{findings:?}");
    assert!(
        failed_passes
            .iter()
            .any(|f| f.reason.contains("cut off at the output limit")),
        "{failed_passes:?}"
    );
}

// ── H2: a batch item cut off (also the legacy batch, which has no retry) ────────────────────

#[test]
fn a_batch_item_still_cut_off_is_a_failed_pass_with_its_complete_findings_kept() {
    let mut results = crate::llm::BatchResults::default();
    results
        .responses
        .insert("c0-b0".to_string(), response(CUT_OFF, true));
    results
        .responses
        .insert("c1-b0".to_string(), response(r#"{"findings":[]}"#, false));
    let (findings, _p, _r, ok, failures) = reassemble_batch_passes(
        REPO,
        "batch",
        2,
        1,
        &[(0, 0), (1, 0)],
        &[4, 2],
        &results,
        &std::collections::HashSet::new(),
        &adopted(),
        None,
        None,
    );
    assert_eq!(ok, 1, "only the complete item answered");
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(
        failures[0].files, 4,
        "the cut-off item names its chunk's files"
    );
    assert_eq!(failures[0].recovered, Some(1));
    assert!(
        failures[0].error.contains("cut off at the output limit"),
        "{}",
        failures[0].error
    );
}

#[test]
fn a_batch_item_cut_off_with_nothing_complete_is_never_zero_findings() {
    let mut results = crate::llm::BatchResults::default();
    results
        .responses
        .insert("c0-b0".to_string(), response(CUT_OFF_EMPTY, true));
    let (findings, _p, _r, ok, failures) = reassemble_batch_passes(
        REPO,
        "batch",
        1,
        1,
        &[(0, 0)],
        &[5],
        &results,
        &std::collections::HashSet::new(),
        &adopted(),
        None,
        None,
    );
    assert_eq!((ok, findings.len(), failures.len()), (0, 0, 1));
    assert_eq!(failures[0].files, 5);
}

// ── H5: absorbed members keep their rating and headline ─────────────────────────────────────

#[test]
fn the_new_security_tokens_classify_idor_traversal_mass_assignment_and_privilege_ids() {
    for id in [
        "AI-IDOR-INVOICES",
        "AI-PATH-TRAVERSAL-DOWNLOAD",
        "AI-MASS-ASSIGNMENT-USER",
        "AI-UNAUTHENTICATED-DELETE",
        "AI-PRIVILEGE-ESCALATION-ADMIN",
    ] {
        let f = finding(id, "high", "d");
        assert_eq!(finding_class(&f), FindingClass::Security, "{id}");
    }
    assert_eq!(
        finding_class(&finding("AI-TREE-SHAKING-UNUSED", "low", "d")),
        FindingClass::Hygiene
    );
}

#[test]
fn an_absorbed_critical_keeps_its_rule_severity_and_headline_in_also_locations() {
    let files = vec![(
        "src/h.rs".to_string(),
        "fn a() {}\nfn b() {}\ndb.get(invoice_id)\n".to_string(),
    )];
    let det = finding("SEC-NO-RAW-SQL-CONCAT-1", "medium", "Raw SQL concatenation");
    let ai = finding(
        "AI-ODD-NAME",
        "critical",
        &format!("Any user can read any invoice{HEADLINE_SEP}the handler never checks the owner"),
    );
    let merged = merge_by_location(vec![det, ai], &files);
    assert_eq!(merged.len(), 1);
    let row = &merged[0];
    assert_eq!(row.rule_id, "SEC-NO-RAW-SQL-CONCAT-1");
    assert_eq!(
        row.severity, "medium",
        "which severity wins a merge is unchanged"
    );
    assert_eq!(row.also_locations.len(), 1, "{:?}", row.also_locations);
    let site = &row.also_locations[0];
    assert_eq!(site.rule_id, "AI-ODD-NAME");
    assert_eq!(site.severity, "critical");
    assert_eq!(site.headline, "Any user can read any invoice");
    // The lowering is on the record.
    let adj = adjustments(row);
    assert_eq!(adj.len(), 1, "{adj:?}");
    assert!(
        adj[0].starts_with("severity lowered from critical to medium:"),
        "{}",
        adj[0]
    );
}

#[test]
fn an_absorbed_member_of_the_same_rating_still_leaves_its_site_and_no_adjustment() {
    let files = vec![(
        "src/h.rs".to_string(),
        "fn a() {}\nfn b() {}\ndb.get(invoice_id)\n".to_string(),
    )];
    let a = finding("AI-IDOR-INVOICES", "high", "IDOR on invoices");
    let b = finding("AI-OTHER-NAME", "high", "Missing owner check");
    let merged = merge_by_location(vec![a, b], &files);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].also_locations.len(), 1);
    assert!(adjustments(&merged[0]).is_empty());
}

// ── H7: the self-hedge cap reads the producer's words only and never caps a floor critical ──

#[test]
fn a_calibration_reason_that_says_confirm_whether_does_not_cap_a_floor_critical() {
    let f = finding(
        "AI-UNAUTH-DELETE",
        "critical",
        "The delete endpoint is unauthenticated and removes any account. [calibrated: confirm whether this is intended]",
    );
    let out = apply_severity_ceiling_rule(f);
    assert_eq!(out.severity, "critical");
    assert!(adjustments(&out).is_empty());
}

#[test]
fn a_calibration_reason_alone_does_not_trigger_the_self_hedge_cap() {
    let f = finding(
        "AI-MISSING-CHECK",
        "high",
        "The handler skips the owner check. [calibrated: confirm whether this is a defect]",
    );
    let out = apply_severity_ceiling_rule(f);
    assert_eq!(out.severity, "high", "the producer never hedged");
    assert_ne!(out.confidence.as_deref(), Some("needs-review"));
}

#[test]
fn the_producers_own_hedge_still_caps_to_low() {
    let f = finding(
        "AI-HEDGE",
        "high",
        "This appears intentional; confirm whether it is expected. [calibrated: ok]",
    );
    let out = apply_severity_ceiling_rule(f);
    assert_eq!(out.severity, "low");
    assert_eq!(out.confidence.as_deref(), Some("needs-review"));
    let adj = adjustments(&out);
    assert_eq!(adj.len(), 1, "{adj:?}");
    assert!(
        adj[0].starts_with("severity lowered from high to low:"),
        "{}",
        adj[0]
    );
}

// ── M1: the CORS clamp is for CORS rules ────────────────────────────────────────────────────

#[test]
fn cors_words_in_another_rules_prose_do_not_move_its_severity() {
    let f = finding(
        "AI-SQL-INJECTION-EXPORT",
        "critical",
        "String SQL in the export query lets a caller read every row; unrelated to the CORS setup nearby.",
    );
    let out = apply_severity_ceiling_rule(f);
    assert_eq!(out.severity, "critical");
}

#[test]
fn a_cors_rule_is_still_clamped_to_medium_and_the_move_is_recorded() {
    let f = finding(
        "AI-CORS-REFLECTED-ORIGIN",
        "critical",
        "The CORS policy reflects the request's Origin and allows credentials.",
    );
    let out = apply_severity_ceiling_rule(f);
    assert_eq!(out.severity, "medium");
    let adj = adjustments(&out);
    assert_eq!(adj.len(), 1, "{adj:?}");
    assert!(
        adj[0].starts_with("severity lowered from critical to medium:"),
        "{}",
        adj[0]
    );
}

#[test]
fn a_floor_earned_critical_on_a_cors_named_rule_is_not_clamped() {
    let f = finding(
        "AI-CORS-OPEN-ADMIN",
        "critical",
        "Wildcard CORS on an unauthenticated admin endpoint exposes every account.",
    );
    let out = apply_severity_ceiling_rule(f);
    assert_eq!(out.severity, "critical");
}

// ── M2: every severity move is on the record ────────────────────────────────────────────────

#[test]
fn a_verdict_downgrade_records_the_old_severity_and_the_reason() {
    let f = finding("AI-X-1", "high", "d");
    let raw = r#"{"verdicts":[{"index":0,"severity":"low","confidence":"high","reason":"only reachable from tests"}]}"#;
    let out = apply_verdicts(raw, vec![f]).remove(0);
    assert_eq!(out.severity, "low");
    let adj = adjustments(&out);
    assert_eq!(adj.len(), 1, "{adj:?}");
    assert!(
        adj[0].starts_with("severity lowered from high to low:"),
        "{}",
        adj[0]
    );
    assert!(adj[0].contains("only reachable from tests"), "{}", adj[0]);
}

#[test]
fn an_unchanged_verdict_records_nothing() {
    let f = finding("AI-X-1", "high", "d");
    let raw = r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"fine"}]}"#;
    assert!(adjustments(&apply_verdicts(raw, vec![f]).remove(0)).is_empty());
}

#[test]
fn the_floor_raise_is_recorded_too() {
    let f = finding(
        "AI-UNAUTH-EXPORT",
        "medium",
        "The export endpoint requires no authentication and returns every record.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(out.severity, "critical");
    let adj = adjustments(&out);
    assert_eq!(adj.len(), 1, "{adj:?}");
    assert!(
        adj[0].starts_with("severity raised from medium to critical:"),
        "{}",
        adj[0]
    );
}

#[test]
fn a_repeated_adjustment_sentence_is_not_duplicated() {
    let mut f = finding("AI-X-1", "high", "d");
    record_severity_adjustment(&mut f, "high", "low", "same reason");
    record_severity_adjustment(&mut f, "high", "low", "same reason");
    record_severity_adjustment(&mut f, "low", "low", "no move");
    assert_eq!(adjustments(&f).len(), 1);
}

// ── M3: severity labels ─────────────────────────────────────────────────────────────────────

#[test]
fn model_severity_labels_are_trimmed_case_folded_and_mapped() {
    for (raw, want) in [
        ("Critical", "critical"),
        (" HIGH ", "high"),
        ("Severe", "high"),
        ("Moderate", "medium"),
        ("MEDIUM", "medium"),
        ("Informational", "low"),
        ("minor", "low"),
    ] {
        assert_eq!(parse_model_severity(raw, "medium"), want, "{raw}");
    }
    // An unknown label keeps what the caller already had.
    assert_eq!(parse_model_severity("bogus", "high"), "high");
}

#[test]
fn a_capitalised_scan_severity_is_not_flattened_to_medium() {
    let raw = r#"{"findings":[{"path":"a.rs","line":1,"severity":"Critical","rule":"RULE-0","title":"t","code":"c","detail":"d"}]}"#;
    let (f, _) = parse_ai_findings(REPO, raw, &adopted());
    assert_eq!(f[0].severity, "critical");
}

#[test]
fn an_unknown_verdict_severity_keeps_the_current_severity() {
    let f = finding("AI-X-1", "high", "d");
    let raw = r#"{"verdicts":[{"index":0,"severity":"urgent","confidence":"high","reason":"r"}]}"#;
    assert_eq!(apply_verdicts(raw, vec![f]).remove(0).severity, "high");
}

#[test]
fn a_capitalised_verdict_severity_is_honoured() {
    let f = finding("AI-X-1", "medium", "d");
    let raw = r#"{"verdicts":[{"index":0,"severity":"High","confidence":"high","reason":"r"}]}"#;
    assert_eq!(apply_verdicts(raw, vec![f]).remove(0).severity, "high");
}

// ── M4: rule labels ─────────────────────────────────────────────────────────────────────────

fn rule_of(raw_rule_field: &str, adopted_ids: &[&str]) -> String {
    let adopted: std::collections::HashSet<String> =
        adopted_ids.iter().map(|s| s.to_string()).collect();
    let raw = format!(
        r#"{{"findings":[{{"path":"a.rs","line":1,"severity":"high",{raw_rule_field},"title":"t","code":"c","detail":"d"}}]}}"#
    );
    parse_ai_findings(REPO, &raw, &adopted).0.remove(0).rule_id
}

#[test]
fn rule_labels_are_cleaned_before_they_are_matched_to_adopted_ids() {
    let adopted = ["SEC-NO-RAW-SQL-CONCAT-1"];
    for label in [
        r#""rule":"SEC-NO-RAW-SQL-CONCAT-1""#,
        r#""rule":"[SEC-NO-RAW-SQL-CONCAT-1]""#,
        r#""rule":"SEC-NO-RAW-SQL-CONCAT-1: string concatenation in the query""#,
        r#""rule":"sec_no_raw_sql_concat_1""#,
        r#""rule":"`SEC-NO-RAW-SQL-CONCAT-1`""#,
        r#""rule_id":"SEC-NO-RAW-SQL-CONCAT-1""#,
        r#""id":"SEC-NO-RAW-SQL-CONCAT-1""#,
        // A missing version suffix and an extended label both mean the adopted id.
        r#""rule":"SEC-NO-RAW-SQL-CONCAT""#,
        r#""rule":"SEC-NO-RAW-SQL-CONCAT-1-IN-REPORTS""#,
    ] {
        assert_eq!(
            rule_of(label, &adopted),
            "SEC-NO-RAW-SQL-CONCAT-1",
            "{label}"
        );
    }
}

#[test]
fn an_unrelated_label_is_still_an_invented_ai_rule() {
    assert_eq!(
        rule_of(r#""rule":"some new problem""#, &["SEC-NO-RAW-SQL-CONCAT-1"]),
        "AI-SOME-NEW-PROBLEM"
    );
    // An ambiguous prefix is not guessed at.
    assert_eq!(
        rule_of(r#""rule":"SEC-NO""#, &["SEC-NO-A-1", "SEC-NO-B-1"]),
        "AI-SEC-NO"
    );
}

// ── M5: lines and the duplicate key ─────────────────────────────────────────────────────────

fn line_of(line_json: &str) -> usize {
    let raw = format!(
        r#"{{"findings":[{{"path":"a.rs","line":{line_json},"severity":"high","rule":"RULE-0","title":"t","code":"c","detail":"d"}}]}}"#
    );
    parse_ai_findings(REPO, &raw, &adopted()).0.remove(0).line
}

#[test]
fn line_numbers_are_accepted_as_strings_and_ranges() {
    assert_eq!(line_of("42"), 42);
    assert_eq!(line_of(r#""42""#), 42);
    assert_eq!(line_of(r#""L42""#), 42);
    assert_eq!(line_of(r#""42-47""#), 42);
    assert_eq!(line_of("42.0"), 42);
    assert_eq!(line_of(r#""none""#), 0);
    assert_eq!(line_of("null"), 0);
}

#[test]
fn two_distinct_findings_of_one_rule_at_one_path_and_line_are_both_kept() {
    let mut a = finding("AI-X-1", "low", "first");
    a.line = 0;
    a.snippet = "first problem".to_string();
    let mut b = finding("AI-X-1", "high", "second");
    b.line = 0;
    b.snippet = "second problem".to_string();
    let mut all = vec![a, b];
    dedup_findings_keep_max_severity(&mut all);
    assert_eq!(all.len(), 2, "different snippets are different findings");
}

#[test]
fn a_repeat_collapses_and_keeps_the_highest_severity_seen() {
    let a = finding("AI-X-1", "low", "first");
    let mut b = finding("AI-X-1", "critical", "second");
    b.snippet = "  DB.get(invoice_id) ".to_string();
    let c = finding("AI-X-2", "medium", "other rule");
    let mut all = vec![a, b, c];
    dedup_findings_keep_max_severity(&mut all);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].severity, "critical");
    assert_eq!(
        all[0].detail, "first",
        "the first repeat's text is the one kept"
    );
    assert_eq!(all[1].rule_id, "AI-X-2");
}

// ── M6: requested files ─────────────────────────────────────────────────────────────────────

#[test]
fn requested_paths_are_normalised_to_the_scan_form() {
    for raw in [
        "./src/b.rs",
        "/src/b.rs",
        "src\\b.rs",
        " `src/b.rs` ",
        ".//src/b.rs",
    ] {
        assert_eq!(normalize_requested_path(raw), "src/b.rs", "{raw}");
    }
}

#[tokio::test]
async fn a_deferred_judgment_resolves_a_dotted_path_and_names_a_file_that_is_not_in_the_scan() {
    let llm = ScriptedLlm::new(&[
        (
            r#"{"findings":[],"needs_files":["./src/b.rs","src/zzz.rs"]}"#,
            false,
        ),
        (r#"{"findings":[]}"#, false),
    ]);
    let files = vec![
        ("src/a.rs".to_string(), "fn a() {}\n".to_string()),
        ("src/b.rs".to_string(), "fn b() {}\n".to_string()),
    ];
    let selected = vec![("RULE-0".to_string(), "Rule zero.".to_string())];
    let (_f, _p, _recs, failed_passes, _s) = audit_repo(
        &llm,
        REPO,
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
    .expect("the scan runs");
    assert_eq!(
        llm.calls(),
        2,
        "the dotted path resolved, so the resolution round ran"
    );
    let rows: Vec<_> = failed_passes
        .iter()
        .filter(|f| f.pass == "deferred judgment")
        .collect();
    assert_eq!(rows.len(), 1, "{failed_passes:?}");
    assert!(
        rows[0]
            .reason
            .contains("requested files not in scan: src/zzz.rs"),
        "{}",
        rows[0].reason
    );
    assert!(!rows[0].reason.contains("src/b.rs"), "{}", rows[0].reason);
}

// ── M7: the recommendation pass reads every file chunk ──────────────────────────────────────

fn two_option_alt(rule: &str) -> RuleAlternatives {
    let opt = |id: &str| crate::onboard::RuleOptionView {
        id: id.to_string(),
        label: id.to_string(),
        directive: format!("Do it the {id} way."),
        why: "why".to_string(),
        violates: false,
    };
    RuleAlternatives {
        rule_id: rule.to_string(),
        options: vec![opt("opt-a"), opt("opt-b")],
        selected_option_id: Some("opt-a".to_string()),
        kind: Some(camerata_rules::RuleKind::Decision),
    }
}

/// Two files that cannot share one digest chunk.
fn two_chunk_files() -> Vec<(String, String)> {
    let big = |name: &str| {
        (
            name.to_string(),
            format!("// {name}\n{}\n", "x".repeat(250_000)),
        )
    };
    vec![big("src/a.rs"), big("src/b.rs")]
}

const NOT_APPLICABLE: &str = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":false,"recommendation_reasoning":"nothing here"}]}"#;
const APPLICABLE: &str = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-b","recommendation_reasoning":"found it","applicable":true,"evidence":"src/b.rs:2"}]}"#;

async fn recommend(llm: &ScriptedLlm) -> Vec<RuleRecommendation> {
    let files = two_chunk_files();
    recommend_alternatives(
        llm,
        REPO,
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        &[two_option_alt("MULTI-RULE-1")],
        None,
        None,
        None,
    )
    .await
    .expect("the recommendation pass runs")
}

#[tokio::test]
async fn a_rule_that_applies_only_in_a_later_chunk_is_not_judged_not_applicable() {
    let llm = ScriptedLlm::new(&[(NOT_APPLICABLE, false), (APPLICABLE, false)]);
    let recs = recommend(&llm).await;
    assert_eq!(llm.calls(), 2);
    assert_eq!(recs.len(), 1);
    assert!(recs[0].applicable, "{:?}", recs[0]);
    assert_eq!(recs[0].recommended_option_id, "opt-b");
}

#[tokio::test]
async fn a_rule_that_applies_in_the_first_chunk_costs_one_call() {
    let llm = ScriptedLlm::new(&[(APPLICABLE, false)]);
    let recs = recommend(&llm).await;
    assert_eq!(llm.calls(), 1);
    assert!(recs[0].applicable);
}

#[tokio::test]
async fn a_rule_every_chunk_calls_not_applicable_is_not_applicable() {
    let llm = ScriptedLlm::new(&[(NOT_APPLICABLE, false)]);
    let recs = recommend(&llm).await;
    assert_eq!(llm.calls(), 2, "asked once per file chunk");
    assert_eq!(recs.len(), 1);
    assert!(!recs[0].applicable);
}

// ── H1 (deep tier): the security lens's cut-off reply ───────────────────────────────────────

#[tokio::test]
async fn a_cut_off_deep_security_chunk_keeps_its_complete_findings_and_is_reported() {
    let llm = ScriptedLlm::new(&[(CUT_OFF, true)]);
    let files: Vec<(String, String)> = vec![("a.rs".to_string(), "let a = 1;\n".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules = vec![("RULE-0".to_string(), "rule zero".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let (findings, _p, _r, ok, failures) = run_security_passes(
        &llm,
        REPO,
        REPO_MAP,
        &crate::prompt_frame::FrameNonce::generate(),
        &adopted(),
        None,
        None,
        &chunks,
        &batches,
        1,
        None,
    )
    .await;
    assert_eq!((ok, findings.len(), failures.len()), (0, 1, 1));
    assert_eq!(failures[0].files, 1);
    assert_eq!(failures[0].recovered, Some(1));
    assert!(
        failures[0].error.contains("cut off at the output limit"),
        "{}",
        failures[0].error
    );
}
