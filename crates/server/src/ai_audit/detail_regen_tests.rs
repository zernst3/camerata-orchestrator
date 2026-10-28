//! Rank 4 (`terse_output`): terse scan output and the post-merge detail regeneration.
//!
//! What these pin, in the order the brief asks:
//!
//! * switch OFF gives byte-identical prompts to today;
//! * an old-format response (full `detail`, `proposed_rules`) still parses;
//! * for each consumer of `detail`, whether it runs on the short or the full text;
//! * every finding that leaves `audit_repo` carries the regenerated full detail, and a finding
//!   whose regeneration failed carries the incomplete marker and a failed pass, never dropped.
//!
//! No network: every model is a stub.

use std::sync::Mutex;

use super::detail_regen::{
    mark_detail_incomplete, regenerate_full_details, DETAIL_FAILED_PASS, DETAIL_INCOMPLETE_MARKER,
};
use super::*;
use crate::cost_cap::RunControls;
use crate::efficiency::EfficiencyConfig;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::stage_telemetry::Stage;

const REPO: &str = "acme/webhooks";
const HANDLER: &str = "src/handlers/webhook.rs";
const SNIPPET: &str = "process_refund(payload.account_id, payload.amount_cents);";
const TITLE: &str = "Webhook trusts a config flag before refunding";
/// What the terse scan writes, and what the regeneration writes for the same finding.
const SHORT: &str = "Trusts REQUIRE_WEBHOOK_SIGNATURE from src/config.rs.";
const FULL: &str = "The handler reads REQUIRE_WEBHOOK_SIGNATURE from src/config.rs and, when it is false, calls process_refund with no signature check, so an unauthenticated caller can trigger a refund. Verify the signature in the handler itself.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scan,
    Regen,
    Calibrate,
    Other,
}

fn role_of(system: &str) -> Role {
    if system.starts_with("You are a senior software architect")
        || system.starts_with("You are a senior application-security engineer")
    {
        Role::Scan
    } else if system.starts_with("You write the full explanation") {
        Role::Regen
    } else if system.starts_with("You are calibrating") {
        Role::Calibrate
    } else {
        Role::Other
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Regen {
    /// Writes [`FULL`] for every finding it is asked about.
    Writes,
    /// Answers with something that is not the expected JSON.
    Garbage,
    /// The transport fails.
    Down,
    /// Writes [`FULL`] for the first finding of each batch only.
    FirstOnly,
}

struct RoleLlm {
    scan: String,
    regen: Regen,
    log: Mutex<Vec<(Role, LlmRequest)>>,
}

impl RoleLlm {
    fn new(scan: &str, regen: Regen) -> Self {
        Self {
            scan: scan.to_string(),
            regen,
            log: Mutex::new(Vec::new()),
        }
    }
    fn roles(&self) -> Vec<Role> {
        self.log.lock().unwrap().iter().map(|(r, _)| *r).collect()
    }
    fn prompts(&self, role: Role) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| *r == role)
            .map(|(_, q)| q.prompt.clone())
            .collect()
    }
    fn systems(&self, role: Role) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| *r == role)
            .map(|(_, q)| q.system.clone().unwrap_or_default())
            .collect()
    }
}

#[async_trait::async_trait]
impl LlmPort for RoleLlm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let role = role_of(req.system.as_deref().unwrap_or(""));
        self.log.lock().unwrap().push((role, req.clone()));
        let text = match role {
            Role::Scan => self.scan.clone(),
            Role::Regen => match self.regen {
                Regen::Down => anyhow::bail!("transport down"),
                Regen::Garbage => "not json".to_string(),
                Regen::Writes | Regen::FirstOnly => {
                    let asked = (0..64)
                        .take_while(|i| req.prompt.contains(&format!("[{i}] rule ")))
                        .count();
                    let n = if self.regen == Regen::FirstOnly {
                        asked.min(1)
                    } else {
                        asked
                    };
                    let entries: Vec<String> = (0..n)
                        .map(|i| format!(r#"{{"index":{i},"detail":"{FULL}"}}"#))
                        .collect();
                    format!(r#"{{"details":[{}]}}"#, entries.join(","))
                }
            },
            Role::Calibrate | Role::Other => "{}".to_string(),
        };
        Ok(LlmResponse {
            text,
            model: "stub".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
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
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A terse-format scan reply: one-sentence detail, no `proposed_rules`.
fn terse_scan(n_findings: usize) -> String {
    let findings: Vec<String> = (0..n_findings)
        .map(|i| {
            format!(
                r#"{{"path":"{HANDLER}","line":{},"severity":"high","rule":"ARCH-X-1","title":"{TITLE} {i}","code":"{SNIPPET}","detail":"{SHORT}"}}"#,
                9 + i * 40
            )
        })
        .collect();
    format!(
        r#"{{"findings":[{}],"needs_files":[]}}"#,
        findings.join(",")
    )
}

fn files() -> Vec<(String, String)> {
    let mut handler = String::new();
    // Enough lines apart that distinct findings never share a code location.
    for i in 1..=120 {
        if i == 9 || i == 49 || i == 89 {
            handler.push_str(&format!("    {SNIPPET}\n"));
        } else {
            handler.push_str(&format!("// line {i}\n"));
        }
    }
    vec![
        (
            "src/config.rs".to_string(),
            "pub const REQUIRE_WEBHOOK_SIGNATURE: bool = false;\n".to_string(),
        ),
        (HANDLER.to_string(), handler),
    ]
}

fn selected() -> Vec<(String, String)> {
    vec![(
        "ARCH-X-1".to_string(),
        "Verify the webhook signature.".to_string(),
    )]
}

fn meter_with(config: EfficiencyConfig) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        efficiency: config,
        max_cost_usd: None,
        estimate_hint_usd: None,
    })
}

fn terse_off() -> EfficiencyConfig {
    let mut config = EfficiencyConfig::default();
    config.set("terse_output", false).unwrap();
    config
}

async fn audit(llm: &RoleLlm, meter: &UsageMeter) -> (Vec<Finding>, Vec<FailedPass>) {
    let files = files();
    let (findings, _proposed, _recs, failed, _samples) = audit_repo(
        llm,
        REPO,
        &files,
        &selected(),
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        Some(meter),
        Some(&files),
    )
    .await
    .expect("audit_repo against the stub");
    (findings, failed)
}

fn finding(detail: &str) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: HANDLER.to_string(),
        line: 9,
        rule_id: "ARCH-X-1".to_string(),
        severity: "high".to_string(),
        snippet: SNIPPET.to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

// ── switch OFF: byte-identical to today ─────────────────────────────────────────────────────

/// The exact instruction lines the passes carried before rank 4.
const ADVISORY_LINE_BEFORE_RANK_4: &str = "── Check the code above against the ADOPTED rules below (batch 1/3); ALSO flag any other genuine issues NOT covered by an adopted rule. Use the REPO MAP for cross-file context. ──";
const LATER_LINE_BEFORE_RANK_4: &str = "── Check the code above against ONLY the ADOPTED rules below (batch 2/3). Do NOT report issues outside these rules — a separate pass already covers novel findings. Use the REPO MAP for cross-file context. ──";

#[test]
fn switch_off_prompts_are_byte_identical_to_before_rank_4() {
    // The system prompt: the unparameterized function IS the OFF form, and the cache key's pin
    // test (`scan_cache::ai_key_tests`) hashes its bytes, so a drift fails there too.
    assert_eq!(audit_system_prompt_with(false), audit_system_prompt());
    assert_eq!(
        deep_security_system_prompt_with(false),
        deep_security_system_prompt()
    );
    assert!(audit_system_prompt().contains(r#""proposed_rules": ["#));
    assert!(audit_system_prompt()
        .contains(r#""detail": "why it's a problem and what the fix direction is","#));
    // The instruction lines, against the literal text they had.
    assert_eq!(
        pass_task_line(true, 0, 3, false),
        ADVISORY_LINE_BEFORE_RANK_4
    );
    assert_eq!(pass_task_line(false, 1, 3, false), LATER_LINE_BEFORE_RANK_4);
}

#[tokio::test]
async fn switch_off_sends_the_old_system_prompt_and_makes_no_regeneration_call() {
    let llm = RoleLlm::new(
        &format!(
            r#"{{"findings":[{{"path":"{HANDLER}","line":9,"severity":"high","rule":"ARCH-X-1","title":"{TITLE}","code":"{SNIPPET}","detail":"{FULL}"}}],"proposed_rules":[],"needs_files":[]}}"#
        ),
        Regen::Writes,
    );
    let meter = meter_with(terse_off());
    let (findings, failed) = audit(&llm, &meter).await;

    assert_eq!(
        llm.roles(),
        vec![Role::Scan, Role::Calibrate],
        "no regeneration step"
    );
    for system in llm.systems(Role::Scan) {
        assert_eq!(system, audit_system_prompt());
    }
    for prompt in llm.prompts(Role::Scan) {
        assert!(!prompt.contains("In THIS pass only"), "{prompt}");
    }
    assert_eq!(findings.len(), 1);
    assert!(findings[0].detail.contains(FULL));
    assert!(
        failed.iter().all(|f| f.pass != DETAIL_FAILED_PASS),
        "{failed:?}"
    );
    assert!(meter
        .stage_telemetry(0)
        .stages
        .iter()
        .all(|s| s.stage != Stage::DetailRegeneration));
}

// ── switch ON: the prompts ──────────────────────────────────────────────────────────────────

#[test]
fn switch_on_asks_for_one_sentence_and_no_proposed_rules() {
    for prompt in [
        audit_system_prompt_with(true),
        deep_security_system_prompt_with(true),
    ] {
        assert!(!prompt.contains("proposed_rules"), "{prompt}");
        assert!(prompt.contains("SHORT DETAIL"), "{prompt}");
        assert!(prompt.contains("at most 25 words"), "{prompt}");
        assert!(prompt.contains("Borderline:"), "{prompt}");
        assert!(prompt.contains("needs_files"));
    }
    // The proposal schema is gone from the terse prompt, not merely emptied.
    assert!(!audit_system_prompt_with(true).contains(r#""enforcement""#));
    assert!(audit_system_prompt().contains(r#""enforcement""#));
}

#[test]
fn only_the_advisory_line_restores_proposed_rules() {
    assert!(pass_task_line(true, 0, 2, true).contains("proposed_rules"));
    assert!(pass_task_line(true, 0, 2, true).starts_with(&pass_task_line(true, 0, 2, false)));
    assert!(!pass_task_line(false, 1, 2, true).contains("proposed_rules"));
    assert_eq!(
        pass_task_line(false, 1, 2, true),
        pass_task_line(false, 1, 2, false),
        "a non-advisory line is the same either way"
    );
}

#[tokio::test]
async fn every_pass_of_a_terse_scan_shares_one_system_prompt() {
    // The system prompt leads the cache prefix. If the advisory pass and its siblings differed
    // there, the siblings could not read the entry the advisory pass wrote.
    let llm = RoleLlm::new(&terse_scan(0), Regen::Writes);
    let meter = meter_with(EfficiencyConfig::default());
    let files = files();
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rule_batches: Vec<Vec<(String, String)>> = (0..3)
        .map(|n| vec![(format!("RULE-{n}"), format!("rule {n}"))])
        .collect();
    let batches: Vec<&[(String, String)]> = rule_batches.iter().map(Vec::as_slice).collect();
    let (_f, _p, _r, ok, err) = run_passes(
        &llm,
        REPO,
        "REPO MAP",
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        &chunks,
        &batches,
        2,
        "pass[all]",
        "t",
        Some(&meter),
        false,
    )
    .await;
    assert!(err.is_none(), "{err:?}");
    assert_eq!(ok, 3);
    let systems = llm.systems(Role::Scan);
    assert_eq!(systems.len(), 3);
    assert!(systems.iter().all(|s| *s == audit_system_prompt_with(true)));
    let with_rules_instruction = llm
        .prompts(Role::Scan)
        .iter()
        .filter(|p| p.contains("In THIS pass only, ALSO add a top-level `proposed_rules`"))
        .count();
    assert_eq!(
        with_rules_instruction, 1,
        "only the advisory pass proposes rules"
    );
}

// ── parsing ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn an_old_format_response_still_parses() {
    // A response (or a cached one) from before rank 4: full detail, proposed_rules present.
    let old = format!(
        r#"{{"findings":[{{"path":"a.rs","line":3,"severity":"medium","rule":"auth-on-write","title":"{TITLE}","code":"x();","detail":"{FULL}"}}],
            "proposed_rules":[{{"name":"auth-on-write","title":"Require auth on writes","rationale":"three handlers skip it","severity":"high","enforcement":"review"}}],
            "needs_files":["b.rs"]}}"#
    );
    let (findings, proposed) = parse_ai_findings(REPO, &old, &Default::default());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].detail, format!("{TITLE}{HEADLINE_SEP}{FULL}"));
    assert_eq!(proposed.len(), 1);
    assert_eq!(proposed[0].id, "AI-AUTH-ON-WRITE");
    assert_eq!(parse_needs_files(&old), vec!["b.rs".to_string()]);

    // The terse format: no proposed_rules key at all.
    let (findings, proposed) = parse_ai_findings(REPO, &terse_scan(1), &Default::default());
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].detail,
        format!("{TITLE} 0{HEADLINE_SEP}{SHORT}")
    );
    assert!(proposed.is_empty());
}

// ── the consumers of `detail`: short or full ────────────────────────────────────────────────

#[test]
fn merge_by_location_reads_no_detail_so_it_runs_on_the_short_one() {
    let files = files();
    let rows = |detail_a: &str, detail_b: &str| {
        let mut a = finding(detail_a);
        a.rule_id = "ARCH-X-1".to_string();
        let mut b = finding(detail_b);
        b.rule_id = "AI-OTHER-NAME".to_string();
        b.severity = "medium".to_string();
        merge_by_location(vec![a, b], &files)
            .into_iter()
            .map(|f| (f.path, f.line, f.rule_id, f.severity, f.also_matches))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(SHORT, SHORT),
        rows(FULL, FULL),
        "the exact-location merge must decide identically on short and full detail"
    );
    assert_eq!(rows(SHORT, SHORT).len(), 1, "and it does merge them");
}

#[test]
fn calibration_and_the_severity_floor_read_detail_so_they_run_on_the_full_one() {
    // D5 floor: the phrase that raises a finding to critical is in the explanation, not in the
    // one-sentence statement. On the short text there is no floor; on the full text there is.
    let short = finding(&format!("{TITLE}{HEADLINE_SEP}{SHORT}"));
    let full = finding(&format!("{TITLE}{HEADLINE_SEP}{FULL}"));
    assert!(severity_calibration_floor(&short).is_none());
    assert!(
        severity_calibration_floor(&full).is_some(),
        "an unauthenticated refund path is a critical floor on the full text"
    );
    // The scan text the floor and the ceiling read contains the detail verbatim.
    assert!(calibration_floor_scan_text(&full).contains("unauthenticated caller"));
}

#[test]
fn the_semantic_merge_decision_differs_between_short_and_full_so_it_runs_on_the_full_one() {
    // Two AI findings in one file, far apart, under different rule ids, whose full explanations
    // describe one defect in near-identical prose. The prose overlap signal sees that only on
    // the full text.
    let pair = |a_detail: &str, b_detail: &str| {
        let mut a = finding(a_detail);
        a.rule_id = "AI-FLAG-TRUSTED".to_string();
        a.line = 9;
        a.snippet = "if !REQUIRE_WEBHOOK_SIGNATURE {".to_string();
        let mut b = finding(b_detail);
        b.rule_id = "AI-REFUND-UNGUARDED".to_string();
        b.line = 89;
        b.snippet = "process_refund(payload)".to_string();
        (a, b)
    };
    let (a, b) = pair(
        "The handler reads REQUIRE_WEBHOOK_SIGNATURE from src/config.rs and calls process_refund with no signature check so an unauthenticated caller can trigger a refund",
        "The handler reads REQUIRE_WEBHOOK_SIGNATURE from src/config.rs and calls process_refund with no signature check so an unauthenticated caller can trigger a refund",
    );
    assert!(
        semantic_pair_merges(&a, &b, None),
        "identical full explanations are one defect"
    );
    let (a, b) = pair("Trusts a flag.", "Unguarded refund.");
    assert!(
        !semantic_pair_merges(&a, &b, None),
        "the one-sentence statements share too little prose to merge them"
    );
}

#[test]
fn the_object_veto_reads_detail_so_it_runs_on_the_full_one() {
    // `objects_conflict` extracts the named objects from snippet + detail. A veto that exists
    // only in the explanation (two different tables) cannot be applied to a short sentence that
    // names neither.
    let mut a = finding("RLS is missing on `public.orders`");
    a.snippet = "ALTER TABLE a".to_string();
    let mut b = finding("RLS is missing on `public.profiles`");
    b.snippet = "ALTER TABLE b".to_string();
    assert!(objects_conflict(&a, &b));
    let mut a_short = finding("RLS is missing.");
    a_short.snippet = "ALTER TABLE a".to_string();
    let mut b_short = finding("RLS is missing.");
    b_short.snippet = "ALTER TABLE b".to_string();
    assert!(!objects_conflict(&a_short, &b_short));
}

#[test]
fn the_fix_evidence_blob_carries_the_detail_so_the_fix_stage_sees_the_full_one() {
    let files = files();
    let short = finding(&format!("{TITLE}{HEADLINE_SEP}{SHORT}"));
    let full = finding(&format!("{TITLE}{HEADLINE_SEP}{FULL}"));
    // `process_refund` is in the surrounding code in both; `signature` appears only in the
    // explanation, and a fix that names it is grounded only if the full detail is the evidence.
    assert!(!fix_evidence_blob(&short, &files).contains("Verify the signature"));
    assert!(fix_evidence_blob(&full, &files).contains("Verify the signature"));
}

#[tokio::test]
async fn audit_repo_hands_calibration_and_everything_after_it_the_full_detail() {
    let llm = RoleLlm::new(&terse_scan(1), Regen::Writes);
    let meter = meter_with(EfficiencyConfig::default());
    let (findings, failed) = audit(&llm, &meter).await;

    // Order: scan, then regeneration, then calibration.
    assert_eq!(
        llm.roles(),
        vec![Role::Scan, Role::Regen, Role::Calibrate],
        "regeneration runs after the first merge and before calibration"
    );
    let calibration = llm.prompts(Role::Calibrate).join("\n");
    assert!(
        calibration.contains(FULL) && !calibration.contains(SHORT),
        "calibration must judge the full explanation, not the one-sentence statement: {calibration}"
    );
    // What leaves audit_repo (and so enters the cross-family merge, the fix stage and the
    // export) is the headline plus the regenerated explanation.
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0]
            .detail
            .starts_with(&format!("{TITLE} 0{HEADLINE_SEP}{FULL}")),
        "{}",
        findings[0].detail
    );
    assert!(!findings[0].detail.contains(SHORT));
    assert!(!findings[0].detail.contains(DETAIL_INCOMPLETE_MARKER));
    assert!(
        failed.iter().all(|f| f.pass != DETAIL_FAILED_PASS),
        "{failed:?}"
    );
    // The regeneration is its own line in the per-stage telemetry.
    let telemetry = meter.stage_telemetry(0);
    let detail = telemetry
        .stages
        .iter()
        .find(|s| s.stage == Stage::DetailRegeneration)
        .expect("the regeneration stage is recorded");
    assert_eq!(detail.usage.calls, 1);
}

#[tokio::test]
async fn only_the_findings_that_survive_the_merge_are_regenerated() {
    // Three scan rows, two of them at the SAME location under different rule ids: the merge
    // leaves two survivors, and the regeneration is asked about two, not three.
    let scan = format!(
        r#"{{"findings":[
            {{"path":"{HANDLER}","line":9,"severity":"high","rule":"ARCH-X-1","title":"a","code":"{SNIPPET}","detail":"{SHORT}"}},
            {{"path":"{HANDLER}","line":9,"severity":"medium","rule":"invented-name","title":"b","code":"{SNIPPET}","detail":"{SHORT}"}},
            {{"path":"{HANDLER}","line":49,"severity":"high","rule":"ARCH-X-1","title":"c","code":"{SNIPPET}","detail":"{SHORT}"}}
        ],"needs_files":[]}}"#
    );
    let llm = RoleLlm::new(&scan, Regen::Writes);
    let meter = meter_with(EfficiencyConfig::default());
    let (findings, _) = audit(&llm, &meter).await;
    assert_eq!(findings.len(), 2);
    let regen = llm.prompts(Role::Regen);
    assert_eq!(regen.len(), 1, "one batched call, not one per finding");
    assert!(regen[0].contains("[1] rule ") && !regen[0].contains("[2] rule "));
}

// ── failure: marked, reported, never dropped ────────────────────────────────────────────────

#[tokio::test]
async fn a_failed_regeneration_keeps_the_finding_marked_and_reports_a_failed_pass() {
    for regen in [Regen::Down, Regen::Garbage] {
        let llm = RoleLlm::new(&terse_scan(2), regen);
        let meter = meter_with(EfficiencyConfig::default());
        let (findings, failed) = audit(&llm, &meter).await;

        assert_eq!(findings.len(), 2, "no finding is dropped");
        for f in &findings {
            assert!(
                f.detail.contains(DETAIL_INCOMPLETE_MARKER),
                "a short detail never ships without the marker: {}",
                f.detail
            );
            assert!(
                f.detail.contains(SHORT),
                "the scan's sentence is kept: {}",
                f.detail
            );
        }
        let pass = failed
            .iter()
            .find(|f| f.pass == DETAIL_FAILED_PASS)
            .unwrap_or_else(|| panic!("a failed pass is reported: {failed:?}"));
        assert!(pass.reason.contains("2 of 2"), "{}", pass.reason);
        assert!(pass.reason.contains(DETAIL_INCOMPLETE_MARKER));
        // Two rounds were attempted before giving up.
        assert_eq!(llm.prompts(Role::Regen).len(), 2);
        // Calibration still ran: the failure does not stop the pipeline.
        assert!(llm.roles().contains(&Role::Calibrate));
    }
}

#[tokio::test]
async fn a_partial_regeneration_repeats_for_what_was_left_out() {
    // The first call writes only the first finding; the repeat covers the rest.
    let llm = RoleLlm::new(&terse_scan(2), Regen::FirstOnly);
    let meter = meter_with(EfficiencyConfig::default());
    let (findings, failed) = audit(&llm, &meter).await;
    assert_eq!(findings.len(), 2);
    assert!(
        findings.iter().all(|f| f.detail.contains(FULL)),
        "{findings:?}"
    );
    assert!(findings
        .iter()
        .all(|f| !f.detail.contains(DETAIL_INCOMPLETE_MARKER)));
    assert!(failed.iter().all(|f| f.pass != DETAIL_FAILED_PASS));
    assert_eq!(llm.prompts(Role::Regen).len(), 2);
}

#[tokio::test]
async fn the_spend_limit_stopping_the_step_leaves_every_finding_marked() {
    let llm = RoleLlm::new(&terse_scan(1), Regen::Writes);
    // Cap already exhausted by the scan pass (each stub call costs 0.01).
    let meter = UsageMeter::with_controls(RunControls {
        efficiency: EfficiencyConfig::default(),
        max_cost_usd: Some(0.005),
        estimate_hint_usd: None,
    });
    let (findings, failed) = audit(&llm, &meter).await;
    assert_eq!(findings.len(), 1);
    assert!(findings[0].detail.contains(DETAIL_INCOMPLETE_MARKER));
    assert!(
        failed.iter().any(|f| f.pass == DETAIL_FAILED_PASS),
        "{failed:?}"
    );
    assert!(
        llm.prompts(Role::Regen).is_empty(),
        "no call after the limit bit"
    );
    let record = meter.cost_cap_record().expect("a cap was set");
    assert!(
        record
            .not_run_stages
            .iter()
            .any(|s| s.stage == STAGE_DETAIL),
        "{record:?}"
    );
}

#[tokio::test]
async fn regeneration_alone_keeps_the_headline_and_replaces_only_the_explanation() {
    let llm = RoleLlm::new("{}", Regen::Writes);
    let input = vec![
        finding(&format!("{TITLE}{HEADLINE_SEP}{SHORT}")),
        finding("only a title, no explanation"),
        finding(""),
    ];
    let (out, failed) =
        regenerate_full_details(&llm, REPO, input, &files(), &selected(), None, None).await;
    assert!(failed.is_empty());
    assert_eq!(out[0].detail, format!("{TITLE}{HEADLINE_SEP}{FULL}"));
    assert_eq!(
        out[1].detail,
        format!("only a title, no explanation{HEADLINE_SEP}{FULL}")
    );
    assert_eq!(out[2].detail, FULL);
    // The rule's directive and the surrounding code reached the prompt.
    let prompt = llm.prompts(Role::Regen).join("\n");
    assert!(prompt.contains("Rule: Verify the webhook signature."));
    assert!(prompt.contains("Surrounding code:"));
    assert!(prompt.contains("Short statement: "));
}

#[test]
fn the_incomplete_marker_is_idempotent() {
    let mut f = finding("short");
    mark_detail_incomplete(&mut f);
    mark_detail_incomplete(&mut f);
    assert_eq!(f.detail, format!("short {DETAIL_INCOMPLETE_MARKER}"));
    let mut empty = finding("");
    mark_detail_incomplete(&mut empty);
    assert_eq!(empty.detail, DETAIL_INCOMPLETE_MARKER);
}

// ── cost arithmetic ─────────────────────────────────────────────────────────────────────────

#[test]
fn batching_beats_one_call_per_finding() {
    // Rank 9's arithmetic (`input_cost_usd`, the one function every dollar figure uses). Output
    // tokens are the same either way (each finding's explanation is written once), so the
    // difference is the input every call repeats: the system prompt, the instructions and the
    // JSON frame. One call per finding repeats it n times, a batch once per DETAIL_BATCH_SIZE.
    let system_tokens = (super::detail_regen::detail_regen_system_prompt().len() / 4) as u64;
    let n: u64 = 140;
    let price = |tokens: u64| {
        crate::llm::input_cost_usd(
            "claude-sonnet-5",
            tokens,
            crate::llm::PriceBasis {
                batch: false,
                cache_ttl: crate::llm::CacheTtl::FiveMinutes,
            },
        )
        .expect("sonnet is priced")
    };
    let per_finding = price(n * system_tokens);
    let batched = price(n.div_ceil(25) * system_tokens);
    assert!(
        batched * 20.0 < per_finding,
        "batching repeats the system prompt {} times instead of {n}: {batched} vs {per_finding}",
        n.div_ceil(25)
    );
}

// ── the deep-tier security lens follows the same rule ───────────────────────────────────────

#[tokio::test]
async fn the_deep_security_lens_asks_for_a_short_detail_and_regenerates_its_survivors() {
    let llm = RoleLlm::new(&terse_scan(1), Regen::Writes);
    let meter = meter_with(EfficiencyConfig::default());
    let files = files();
    let result = run_security_lens(
        &llm,
        REPO,
        &files,
        "REPO MAP",
        None,
        ScanMode::Sequential,
        None,
        Some(&meter),
    )
    .await;

    assert_eq!(
        llm.systems(Role::Scan),
        vec![deep_security_system_prompt_with(true)]
    );
    assert_eq!(llm.prompts(Role::Regen).len(), 1);
    assert_eq!(result.security_findings.len(), 1);
    let detail = &result.security_findings[0].detail;
    assert!(detail.contains(FULL) && !detail.contains(SHORT), "{detail}");
    assert!(result.error.is_none(), "{:?}", result.error);
}

#[tokio::test]
async fn a_deep_security_regeneration_failure_marks_the_finding_and_reports_on_the_lens() {
    let llm = RoleLlm::new(&terse_scan(1), Regen::Down);
    let meter = meter_with(EfficiencyConfig::default());
    let files = files();
    let result = run_security_lens(
        &llm,
        REPO,
        &files,
        "REPO MAP",
        None,
        ScanMode::Sequential,
        None,
        Some(&meter),
    )
    .await;

    assert_eq!(result.security_findings.len(), 1, "not dropped");
    assert!(result.security_findings[0]
        .detail
        .contains(DETAIL_INCOMPLETE_MARKER));
    assert!(result
        .error
        .as_deref()
        .is_some_and(|e| e.contains(DETAIL_FAILED_PASS)));
}

#[tokio::test]
async fn the_deep_security_lens_with_the_switch_off_is_unchanged() {
    let llm = RoleLlm::new(&terse_scan(1), Regen::Writes);
    let meter = meter_with(terse_off());
    let files = files();
    let result = run_security_lens(
        &llm,
        REPO,
        &files,
        "REPO MAP",
        None,
        ScanMode::Sequential,
        None,
        Some(&meter),
    )
    .await;

    assert_eq!(llm.systems(Role::Scan), vec![deep_security_system_prompt()]);
    assert!(llm.prompts(Role::Regen).is_empty());
    assert!(result.security_findings[0].detail.contains(SHORT));
}
