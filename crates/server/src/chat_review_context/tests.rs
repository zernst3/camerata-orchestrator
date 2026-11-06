//! The chat assistant's view of an inspection is the review screen's own report: the SEALED rows
//! with their holds and junk marks, the groups and the stage 2 verdicts, the evidence and the code
//! at the cited lines, and the ledger, within a bounded size. Secret values are built at runtime
//! (`camerata_secrets::fixtures`).

use camerata_api_types::review::{
    EscalationBudget, EscalationCandidate, EscalationReason, OutlierReason,
};
use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::*;
use crate::onboard::{Finding, ScanReport, SealedReport};
use crate::report_export::apply_overlay;
use crate::review_tree::NodeLevel;
use crate::review_view::test_support::{row, sealed};

fn reviewed(rows: Vec<Finding>, mutate: impl FnOnce(&mut ScanReport)) -> ReviewedReport {
    apply_overlay(&sealed(rows, mutate), &[], &[], &[])
}

fn render(reviewed: &ReviewedReport) -> String {
    render_review_for_chat(reviewed, &CodeFiles::new())
}

fn key_of(sealed: &SealedReport, path: &str) -> String {
    sealed
        .findings
        .iter()
        .find(|f| f.path == path)
        .expect("the row")
        .site_key
        .clone()
}

/// A row the junk check holds: its text is a model refusal.
fn junk_row(path: &str) -> Finding {
    let mut f = row("AI-SERVICE-ROLE", path, 8, "high");
    f.detail = "Service role key \u{2014} I cannot determine this.".to_string();
    f
}

/// A row whose quoted evidence the evidence check could not find: held for a ruling.
fn unverified_row(path: &str) -> Finding {
    let mut f = row("AI-OPEN-REDIRECT", path, 20, "high");
    f.captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the snippet was not found within 3 lines of line 20".to_string(),
    );
    f
}

/// The context reads the sealed report. A row that reached the seal with a secret in its text is
/// redacted by the seal's safety net, and the rendering reads the sealed row.
#[test]
fn the_context_is_made_from_the_sealed_rows_so_no_raw_secret_is_in_it() {
    let key = planted(SecretShape::StripeLiveSecretKey);
    let mut leaky = row("SEC-NO-VENDOR-TOKEN-1", "src/billing.rs", 12, "critical");
    leaky.snippet = key.text.clone();
    leaky.detail = format!(
        "Hardcoded key \u{2014} The deploy script prints {}.",
        key.text
    );
    assert!(
        leaky.snippet.contains(&key.value),
        "the raw row holds the secret"
    );

    let out = render(&reviewed(vec![leaky], |_| {}));

    assert!(
        !out.contains(&key.value),
        "the sealed row is what the context reads: {out}"
    );
    assert!(out.contains("src/billing.rs:12"), "{out}");
    assert!(
        out.contains("sk_live_\u{2026}"),
        "the redacted form is shown: {out}"
    );
}

/// Each row's hold and junk mark is visible, so the operator can ask why it is held.
#[test]
fn a_held_row_names_why_it_is_held_and_a_junk_row_shows_its_mark() {
    let out = render(&reviewed(
        vec![
            junk_row("src/roles.rs"),
            unverified_row("src/redirect.rs"),
            row("SEC-AUTHZ-4", "src/fine.rs", 5, "high"),
        ],
        |_| {},
    ));
    let held = out
        .split("Held rows")
        .nth(1)
        .expect("a held section")
        .to_string();
    let junk_line = held
        .lines()
        .find(|l| l.contains("src/roles.rs:8"))
        .expect("the junk row is listed");
    assert!(junk_line.contains("HELD"), "{junk_line}");
    assert!(junk_line.contains("junk text"), "{junk_line}");
    assert!(
        held.contains("JUNK:"),
        "the junk mark's own reason is shown: {held}"
    );
    let unverified_line = held
        .lines()
        .find(|l| l.contains("src/redirect.rs:20"))
        .expect("the unverified row is listed");
    assert!(
        unverified_line.contains("evidence unverified"),
        "{unverified_line}"
    );
    assert!(out.contains("2 awaiting a ruling"), "{out}");
    // A row nothing holds carries no hold mark.
    let fine = out
        .lines()
        .find(|l| l.contains("src/fine.rs:5"))
        .expect("the fine row is listed");
    assert!(!fine.contains("HELD") && !fine.contains("JUNK"), "{fine}");
}

#[test]
fn a_possible_duplicate_names_the_row_it_may_duplicate() {
    let a = row("SEC-AUTHZ-4", "src/a.rs", 5, "critical");
    let b = row("SEC-AUTHZ-4", "src/b.rs", 9, "high");
    let sealed_first = sealed(vec![a.clone(), b.clone()], |_| {});
    let a_key = key_of(&sealed_first, "src/a.rs");
    let out = render(&reviewed(vec![a, b], |report| {
        let b_row = report
            .findings
            .iter_mut()
            .find(|f| f.path == "src/b.rs")
            .unwrap();
        b_row.possible_duplicate_of = Some(crate::review_tree::DuplicateLink {
            key: a_key.clone(),
            hint: "same_cause:shared helper".to_string(),
        });
    }));
    let line = out
        .lines()
        .find(|l| l.contains("src/b.rs:9"))
        .expect("the row is listed");
    assert!(
        line.contains(&format!("possible duplicate of {}", shown_key(&a_key))),
        "{line}"
    );
    assert!(!out.contains('\0'), "no NUL byte reaches a prompt");
    assert!(line.contains("same_cause:shared helper"), "{line}");
    assert!(
        out.lines()
            .any(|l| l.contains("src/a.rs:5") && l.contains(&format!("key {}", shown_key(&a_key)))),
        "the row it points at shows its key: {out}"
    );
}

/// The groups the review screen shows: the rule groups with their sub-groups, each with the
/// group reviewer's summary and cause and the counts the tree states.
#[test]
fn the_groups_and_their_stage_2_summaries_are_in_the_context() {
    let rows: Vec<Finding> = (0..4)
        .map(|i| {
            row(
                "SEC-NO-RAW-SQL-CONCAT-1",
                &format!("src/q{i}.rs"),
                10 + i,
                "high",
            )
        })
        .collect();
    let out = render(&reviewed(rows, |report| {
        let tree = report.tree.as_mut().expect("a tree");
        tree.group_reviewed = true;
        for node in &mut tree.nodes {
            match node.level {
                NodeLevel::Rule => {
                    node.summary =
                        Some("Four queries build SQL by string concatenation.".to_string())
                }
                NodeLevel::Subgroup => {
                    node.summary =
                        Some("These all take the table name from the request.".to_string());
                    node.cause = Some("the table name comes from user input".to_string());
                }
                NodeLevel::Category => {}
            }
        }
    }));
    let groups = out
        .split("Groups")
        .nth(1)
        .expect("a groups section")
        .to_string();
    assert!(groups.contains("rule:SEC-NO-RAW-SQL-CONCAT-1"), "{groups}");
    assert!(
        groups.contains("Four queries build SQL by string concatenation."),
        "{groups}"
    );
    assert!(
        groups.contains("the table name comes from user input"),
        "{groups}"
    );
    assert!(groups.contains("4 sites"), "{groups}");
}

/// The stage 2 verdicts: who is on the escalation queue and why, the outliers, the sub-group a
/// site was placed in and where stage 2 disagreed with stage 1.
#[test]
fn the_stage_2_verdicts_are_in_the_context() {
    let rows = vec![
        row("SEC-AUTHZ-4", "src/a0.rs", 5, "critical"),
        row("SEC-AUTHZ-4", "src/a1.rs", 6, "high"),
        row("SEC-AUTHZ-4", "src/a2.rs", 7, "high"),
    ];
    let first = sealed(rows.clone(), |_| {});
    let (k0, k1) = (key_of(&first, "src/a0.rs"), key_of(&first, "src/a1.rs"));
    let out = render(&reviewed(rows, |report| {
        let tree = report.tree.as_mut().expect("a tree");
        tree.candidates = vec![EscalationCandidate {
            key: k0.clone(),
            node_id: "rule:SEC-AUTHZ-4".to_string(),
            reason: EscalationReason::UnauthenticatedReach,
            severity: "critical".to_string(),
            stage2_rank: 1,
            global_rank: 1,
            escalated: false,
            over_budget_reason: None,
            why: "reachable without signing in".to_string(),
            slot: 1,
            stage2_severity: None,
        }];
        tree.budget = EscalationBudget::compute(1.0, 20, 10_000);
        let allowed = EscalationReason::default_over_budget_set();
        camerata_api_types::review::re_mark(&mut tree.candidates, &mut tree.budget, &allowed);
        tree.group_reviewed = true;
        let tree = report.tree.take().unwrap();
        tree.sync_rows(&mut report.findings);
        report.tree = Some(tree);
        let r1 = report
            .findings
            .iter_mut()
            .find(|f| f.site_key == k1)
            .unwrap();
        r1.review.outlier_reason = Some(OutlierReason::DifferentObject);
        r1.review.outlier_why = "builds a table name".to_string();
        r1.review.subgroup_label = "Table names".to_string();
        r1.review.subgroup_cause = "the table comes from the request".to_string();
        r1.review.stage2_disagreement = vec![camerata_api_types::review::Stage2Disagreement {
            field: camerata_api_types::review::DisagreementField::Severity,
            stage1: "high".to_string(),
            mine: "low".to_string(),
            why: "the value is a constant".to_string(),
        }];
    }));
    let queue = out
        .split("Escalation queue")
        .nth(1)
        .expect("a queue section")
        .to_string();
    assert!(queue.contains("unauthenticated reach"), "{queue}");
    assert!(queue.contains("reachable without signing in"), "{queue}");
    assert!(queue.contains("src/a0.rs:5"), "{queue}");

    let outlier = out.lines().find(|l| l.contains("src/a1.rs:6")).expect("a1");
    assert!(outlier.contains("outlier"), "{outlier}");
    assert!(out.contains("builds a table name"), "{out}");
    assert!(out.contains("Table names"), "{out}");
    assert!(
        out.contains("stage 2 disagrees on severity: stage 1 said high, stage 2 says low"),
        "{out}"
    );
    assert!(out.contains("the value is a constant"), "{out}");
}

/// The evidence a row cites: its quoted code and its explanation, and the other places merged
/// into it.
#[test]
fn a_held_rows_evidence_and_merged_places_are_in_the_context() {
    let mut held = junk_row("src/roles.rs");
    held.snippet = "const role = req.headers['x-role'];".to_string();
    held.also_locations = vec![crate::onboard::MergedLocation::site(
        "acme/app",
        "src/other.rs",
        44,
        "AI-SERVICE-ROLE",
        "role = header",
        false,
    )];
    let out = render(&reviewed(vec![held], |_| {}));
    assert!(out.contains("const role = req.headers['x-role'];"), "{out}");
    assert!(out.contains("I cannot determine this."), "{out}");
    assert!(out.contains("src/other.rs:44"), "{out}");
}

/// The code at the cited lines of the rows that need it first (held rows), numbered, as the model
/// saw it; a file that could not be read leaves the row without code and nothing else changes.
#[test]
fn the_code_at_a_held_rows_cited_line_is_in_the_context() {
    let reviewed = reviewed(
        vec![
            junk_row("src/roles.rs"),
            row("SEC-AUTHZ-4", "src/fine.rs", 5, "high"),
        ],
        |_| {},
    );
    let wanted = rows_wanting_code(&reviewed);
    assert_eq!(
        wanted.first(),
        Some(&("acme/app".to_string(), "src/roles.rs".to_string())),
        "a held row's file is wanted first: {wanted:?}"
    );

    let text: String = (1..=40).map(|n| format!("line {n} of roles\n")).collect();
    let mut code = CodeFiles::new();
    code.insert(("acme/app".to_string(), "src/roles.rs".to_string()), text);
    let out = render_review_for_chat(&reviewed, &code);
    assert!(
        out.contains("> 8: line 8 of roles"),
        "the cited line is marked: {out}"
    );
    assert!(out.contains("5: line 5 of roles"), "{out}");
    assert!(out.contains("11: line 11 of roles"), "{out}");
    assert!(
        !out.contains("line 30 of roles"),
        "only the lines around it: {out}"
    );
    // Without the file the context is the same minus the code.
    let bare = render(&reviewed);
    assert!(bare.contains("src/roles.rs:8") && !bare.contains("line 8 of roles"));
}

/// The ledger: what ran and what the engine did to the rows, and what the run could not do.
#[test]
fn the_ledger_is_in_the_context() {
    let out = render(&reviewed(
        vec![row("SEC-AUTHZ-4", "src/a.rs", 5, "high")],
        |report| {
            report.ledger.record_stage(
                "ai calibration",
                10,
                9,
                crate::scan_ledger::StageAccounting {
                    deduped: 1,
                    ..Default::default()
                },
            );
            report.ledger.record_runner_notes(vec![
                "2 stage 2 escalations came back without a rank".to_string(),
            ]);
        },
    ));
    let ledger = out
        .split("Ledger")
        .nth(1)
        .expect("a ledger section")
        .to_string();
    assert!(ledger.contains("ai calibration"), "{ledger}");
    assert!(ledger.contains("10 in, 9 out"), "{ledger}");
    assert!(ledger.contains("came back without a rank"), "{ledger}");
}

#[test]
fn rows_the_engine_threw_out_are_counted_not_listed_as_findings() {
    let mut thrown = row("AI-NOTHING", "src/gone.rs", 3, "low");
    thrown.detail = "Nothing \u{2014} Discard this finding.".to_string();
    thrown.captures.insert(
        crate::engine_discard::ENGINE_DISCARDED_CAPTURE.to_string(),
        "the model said discard".to_string(),
    );
    let out = render(&reviewed(
        vec![thrown, row("SEC-AUTHZ-4", "src/a.rs", 5, "high")],
        |_| {},
    ));
    assert!(out.contains("1 thrown out by the engine"), "{out}");
    assert!(
        !out.lines()
            .any(|l| l.contains("src/gone.rs:3") && l.contains("[low]")),
        "{out}"
    );
}

/// A big run stays inside the bound, and the rows that need an answer come first, so the cut falls
/// on the least useful part.
#[test]
fn the_context_is_bounded_and_the_held_rows_come_before_the_top_findings() {
    let mut rows: Vec<Finding> = (0..3_000)
        .map(|i| {
            row(
                "SEC-NO-RAW-SQL-CONCAT-1",
                &format!("src/q{i:04}.rs"),
                10,
                "high",
            )
        })
        .collect();
    rows.push(junk_row("src/held_one.rs"));
    let out = render(&reviewed(rows, |_| {}));
    assert!(
        out.chars().count() <= MAX_CONTEXT_CHARS + 200,
        "{} characters",
        out.chars().count()
    );
    let held_at = out.find("src/held_one.rs:8").expect("the held row is in");
    let top_at = out.find("Top findings").expect("the top findings are in");
    assert!(held_at < top_at, "held rows come first");
    assert!(
        out.contains("3001"),
        "the total is stated even though the list is capped: {out}"
    );
}

/// "Why is this critical?": the stage 1 verdict (real or not, and its reason) and the calibration
/// rationale and ratings are on the row.
#[test]
fn the_stage_1_verdict_and_calibration_rationale_are_on_each_row() {
    let mut f = row("SEC-AUTHZ-4", "src/a.rs", 5, "critical");
    f.captures.insert(
        crate::ai_audit::STAGE1_CAPTURE.to_string(),
        serde_json::json!({ "real": "yes", "reason": "the handler skips the owner check" })
            .to_string(),
    );
    f.calibration_rationale =
        Some("Raised to critical: unauthenticated access to a protected operation.".to_string());
    f.confidence = Some("high".to_string());
    f.effort = Some("low".to_string());
    f.category = Some("authorization".to_string());
    let out = render(&reviewed(vec![f], |_| {}));
    assert!(
        out.contains("stage 1: real yes \u{2014} the handler skips the owner check"),
        "{out}"
    );
    assert!(
        out.contains(
            "calibration: Raised to critical: unauthenticated access to a protected operation."
        ),
        "{out}"
    );
    assert!(
        out.contains("confidence high, effort low, category authorization"),
        "{out}"
    );
}

#[test]
fn a_row_with_no_stage_1_record_says_nothing_about_it() {
    let out = render(&reviewed(
        vec![row("SEC-AUTHZ-4", "src/a.rs", 5, "high")],
        |_| {},
    ));
    assert!(
        !out.contains("stage 1:") && !out.contains("calibration:"),
        "{out}"
    );
}

// ── The endpoint: the sealed report decides ────────────────────────────────────────────────────

fn scan_report_of(rows: Vec<Finding>) -> ScanReport {
    let mut report = ScanReport::gated(&["owner/repo".to_string()]);
    report.gated = false;
    report.code_lines = 10_000;
    report.findings = rows;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:05:00Z".to_string();
    report.normalize_loaded();
    report
}

async fn context_json(state: crate::AppState) -> serde_json::Value {
    let resp = crate::active_project_context(axum::extract::State(state)).await;
    serde_json::to_value(&resp.0).expect("the context serialises")
}

/// With an inspection recorded, the chat context is the review screen's report: the junk row is
/// marked held, the row count is the sealed count, and a secret the raw row carried is not in any
/// field.
#[tokio::test]
async fn the_chat_context_endpoint_reads_the_sealed_report_of_the_latest_inspection() {
    let state = crate::AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["owner/repo".to_string()])
        .expect("project");
    let key = planted(SecretShape::StripeLiveSecretKey);
    let mut leaky = row("SEC-NO-VENDOR-TOKEN-1", "src/billing.rs", 12, "critical");
    leaky.snippet = key.text.clone();
    leaky.detail = format!("Hardcoded key \u{2014} prints {}.", key.text);
    state
        .inspections
        .append(crate::inspections::InspectionEntry::from_report(
            &project.id,
            scan_report_of(vec![leaky, junk_row("src/roles.rs")]),
            crate::inspections::InspectionKind::Full,
        ))
        .expect("inspection stored");

    let body = context_json(state).await;
    let text = body.to_string();
    assert!(
        !text.contains(&key.value),
        "a raw secret reached the context: {text}"
    );
    let section = body["scan_results_section"]
        .as_str()
        .expect("the review section");
    assert!(section.contains("Held rows"), "{section}");
    assert!(
        section
            .lines()
            .any(|l| l.contains("src/roles.rs:8") && l.contains("HELD") && l.contains("JUNK")),
        "{section}"
    );
    assert_eq!(body["finding_count"], 2, "{body}");
    assert!(
        body["findings_summary"]
            .as_str()
            .unwrap()
            .contains("(HELD)"),
        "the one-line summary marks the held row: {body}"
    );
}

/// Before any inspection is recorded the scan in memory is what the chat has: it is sealed first,
/// so its secrets are redacted.
#[tokio::test]
async fn without_an_inspection_the_last_scan_is_sealed_before_the_chat_reads_it() {
    let state = crate::AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["owner/repo".to_string()])
        .expect("project");
    let key = planted(SecretShape::SupabaseSecretKey);
    let mut leaky = row("SEC-NO-VENDOR-TOKEN-1", "src/env.rs", 3, "critical");
    leaky.detail = format!("Key in env file \u{2014} the value is {}.", key.text);
    state.set_last_scan(project.id.clone(), scan_report_of(vec![leaky]));

    let body = context_json(state).await;
    assert!(
        !body.to_string().contains(&key.value),
        "a raw secret reached the context: {body}"
    );
    assert!(body["scan_results_section"]
        .as_str()
        .unwrap()
        .contains("src/env.rs"));
}
