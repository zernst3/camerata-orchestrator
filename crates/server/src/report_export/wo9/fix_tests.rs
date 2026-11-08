//! Item #1 (design R-4): one fix is one row on every surface. A committed secret that six rules
//! flag at one line, and a seventh flags as a whole file, is one Fix: one plan row, one ticket, one
//! entry of the top three and one escalation entry for each place, each naming every rule.
//! Synthetic fixtures only.

use std::collections::HashMap;

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review::EscalationState;

use super::support::*;
use crate::onboard::{Finding, SealedReport};
use crate::release_gate::{xlsx_sheet_grid, xlsx_sheet_names};
use crate::report_export::{apply_overlay, build_report_json, ReportOptions, ReviewedReport};
use crate::xlsx_export::vocabulary as v;

const RULES_AT_LINE: [&str; 6] = [
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-VENDOR-TOKEN-1",
    "SUPA-NO-SERVICE-ROLE-CLIENT-1",
    "SUPA-NO-PUBLIC-ENV-PREFIX-1",
    "AI-SECRET-EXPOSURE-1",
    "AI-SECRET-EXPOSURE-2",
];
const FILE_RULE: &str = "SEC-NO-SECRET-FILE-1";
const AREAS: [&str; 6] = [
    "Universal",
    "Javascript",
    "Supabase Secrets",
    "Secret Exposure",
    "Secret Exposure",
    "Secret Exposure",
];

fn secret_row(rule: &str, area: &str, severity: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: ".env.production".to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("KEY_{rule}=redacted"),
        detail: format!("{rule}. A real credential is committed to the repository."),
        effort: Some("low".to_string()),
        category: Some(area.to_string()),
        fix_specific: Some(format!("Rotate the credential and remove it ({rule}).")),
        located: true,
        ..Finding::default()
    }
}

/// Six rules at `.env.production:6` in four areas, and the file itself (line 0), all escalated.
fn secret_report() -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    let mut rows: Vec<Finding> = RULES_AT_LINE
        .iter()
        .zip(AREAS)
        .map(|(rule, area)| secret_row(rule, area, "critical", 6))
        .collect();
    rows.push(secret_row(FILE_RULE, "Universal", "critical", 0));
    for r in &mut rows {
        r.review.escalation_state = EscalationState::Escalated;
    }
    report.findings = rows;
    report.seal()
}

fn reviewed_of(
    sealed: &SealedReport,
    records: &[camerata_api_types::decisions::Decision],
) -> ReviewedReport {
    apply_overlay(sealed, records, &[], &[])
}

fn options() -> ReportOptions {
    client_options()
}

fn sheet(reviewed: &ReviewedReport, name: &str) -> Vec<Vec<String>> {
    let xlsx = crate::xlsx_export::build_workbook(reviewed, None, &options()).expect("workbook");
    assert!(xlsx_sheet_names(&xlsx).unwrap().iter().any(|n| n == name));
    xlsx_sheet_grid(&xlsx, name).unwrap_or_else(|e| panic!("{e}"))
}

fn col(grid: &[Vec<String>], header: &str) -> usize {
    grid[0]
        .iter()
        .position(|h| h == header)
        .unwrap_or_else(|| panic!("no {header:?} column in {:?}", grid[0]))
}

fn tickets(reviewed: &ReviewedReport) -> Vec<Vec<String>> {
    let bytes =
        crate::xlsx_export::tickets::tickets_csv(reviewed, None, &HashMap::new()).expect("tickets");
    let mut reader = csv::Reader::from_reader(bytes.as_slice());
    let mut out = vec![reader.headers().unwrap().iter().map(String::from).collect()];
    for record in reader.records() {
        out.push(record.unwrap().iter().map(String::from).collect());
    }
    out
}

#[test]
fn six_rules_at_one_line_and_the_file_are_one_fix_everywhere() {
    let sealed = secret_report();
    let reviewed = reviewed_of(&sealed, &[]);

    // The Fix plan: one row, every rule named.
    let plan = sheet(&reviewed, v::FIX_PLAN);
    assert_eq!(plan.len() - 1, 1, "one plan row: {plan:?}");
    let rules_cell = &plan[1][col(&plan, v::H_RULES)];
    assert_eq!(rules_cell.lines().count(), 7, "{rules_cell}");
    assert_eq!(
        plan[1][col(&plan, v::H_PLACES)],
        "2",
        "the line and the file"
    );

    // The ticket: one, listing all seven rules and their citations.
    let t = tickets(&reviewed);
    assert_eq!(t.len() - 1, 1, "one ticket: {t:?}");
    assert_eq!(t[0][5], v::H_FIX_ID, "the ticket column is the Fix id");
    let description = &t[1][col(&t, "Description")];
    for rule in RULES_AT_LINE.iter().chain(&[FILE_RULE]) {
        assert!(
            description.contains(&format!("({rule})")),
            "{rule}: {description}"
        );
    }
    assert!(description.contains(v::TICKET_CLOSES), "{description}");
    assert_eq!(t[1][col(&t, "Estimate")], "2 to 4 hours per place");
    assert_eq!(
        t[1][5],
        plan[1][col(&plan, v::H_FIX_ID)],
        "the ticket is the plan row"
    );

    // All problems: the line and the file are two places, one Fix between them.
    let problems = sheet(&reviewed, v::ALL_PROBLEMS);
    assert_eq!(problems.len() - 1, 2, "{problems:?}");
    let fix = col(&problems, v::H_FIX_ID);
    assert_eq!(problems[1][fix], problems[2][fix]);
    assert_eq!(problems[1][fix], plan[1][col(&plan, v::H_FIX_ID)]);
    let rules = col(&problems, v::H_RULES);
    let counts: Vec<usize> = (1..=2)
        .map(|i| problems[i][rules].lines().count())
        .collect();
    assert!(counts.contains(&6) && counts.contains(&1), "{counts:?}");

    // The PDF: one entry at the Fix's effort once, and one escalation entry for each place.
    let json = build_report_json(&reviewed, None, &options());
    assert_eq!(
        json.three_things.items.len(),
        1,
        "{:?}",
        json.three_things.items
    );
    let item = &json.three_things.items[0];
    assert_eq!(item.hours_label, "2 to 4 hours");
    assert_eq!(item.rules.len(), 7, "{:?}", item.rules);
    assert_eq!(item.places, 2);
    assert!(
        json.three_things
            .total_hours_label
            .contains("2 to 4 hours total"),
        "{}",
        json.three_things.total_hours_label
    );
    let places: Vec<(String, usize)> = json
        .escalations
        .items
        .iter()
        .map(|e| (e.path.clone(), e.line))
        .collect();
    assert_eq!(places.len(), 2, "one entry for each place: {places:?}");
    let at_line = json.escalations.items.iter().find(|e| e.line == 6).unwrap();
    assert_eq!(at_line.rules.len(), 6, "{:?}", at_line.rules);
    let at_file = json.escalations.items.iter().find(|e| e.line == 0).unwrap();
    assert_eq!(at_file.rules.len(), 1);
}

#[test]
fn a_whole_file_place_prints_the_file_alone() {
    let sealed = secret_report();
    let reviewed = reviewed_of(&sealed, &[]);
    let problems = sheet(&reviewed, v::ALL_PROBLEMS);
    let line = col(&problems, v::H_LINE);
    let lines: Vec<&str> = problems.iter().skip(1).map(|r| r[line].as_str()).collect();
    assert!(lines.contains(&v::WHOLE_FILE), "{lines:?}");
    let t = tickets(&reviewed);
    let places = &t[1][col(&t, v::H_PLACE_LIST)];
    assert!(places.lines().any(|l| l == ".env.production"), "{places}");
    assert!(!places.contains(":0"), "{places}");
}

#[test]
fn effort_is_the_largest_band_counted_once_and_the_order_is_never_later_than_a_member() {
    let mut report = crate::release_gate::test_scan_report();
    let mut a = secret_row("SEC-A-1", "Universal", "high", 6);
    a.effort = Some("low".to_string());
    let mut b = secret_row("SEC-B-1", "Javascript", "high", 6);
    b.effort = Some("high".to_string());
    report.findings = vec![a, b];
    let reviewed = reviewed_of(&report.seal(), &[]);
    let plan = sheet(&reviewed, v::FIX_PLAN);
    assert_eq!(plan.len() - 1, 1);
    assert_eq!(
        plan[1][col(&plan, v::H_EFFORT)],
        "high",
        "the largest band, once"
    );
    let json = build_report_json(&reviewed, None, &options());
    for item in &json.three_things.items {
        assert!(
            item.hours_label.starts_with("3 to 5 days"),
            "{}",
            item.hours_label
        );
    }
    // A high severity Fix is First whatever its effort.
    assert_eq!(plan[1][col(&plan, v::H_FIX_ORDER)], v::FIX_ORDER_FIRST);
}

#[test]
fn a_decided_member_and_an_open_member_show_open_with_counts_and_keep_the_place_open() {
    let sealed = secret_report();
    // Four of the six rules at the line are confirmed; two stay open.
    let decided: Vec<camerata_api_types::decisions::Decision> = sealed
        .findings
        .iter()
        .filter(|f| f.line == 6)
        .take(4)
        .enumerate()
        .map(|(i, f)| {
            decision(
                &format!("dec-{i:06}"),
                &f.site_key,
                DecisionState::Confirmed,
                "a real secret",
            )
        })
        .collect();
    let reviewed = reviewed_of(&sealed, &decided);
    let plan = sheet(&reviewed, v::FIX_PLAN);
    assert_eq!(plan.len() - 1, 1);
    let state = &plan[1][col(&plan, v::H_DECISION_SO_FAR)];
    assert!(state.starts_with(v::OPEN), "{state}");
    assert!(
        state.contains("4 of 7 rules confirmed by the reviewer, 3 open"),
        "{state}"
    );
    // The place stays open while any rule at it is open.
    let problems = sheet(&reviewed, v::ALL_PROBLEMS);
    let decision_col = col(&problems, v::H_DECISION_SO_FAR);
    let line_row = problems
        .iter()
        .skip(1)
        .find(|r| r[col(&problems, v::H_LINE)] == "6")
        .unwrap();
    assert!(line_row[decision_col].starts_with(v::OPEN), "{line_row:?}");

    // All seven decided alike: that state. Decided differently: Mixed, see rules.
    let all: Vec<camerata_api_types::decisions::Decision> = sealed
        .findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            decision(
                &format!("dec-{i:06}"),
                &f.site_key,
                if i == 0 {
                    DecisionState::Ignored
                } else {
                    DecisionState::Confirmed
                },
                "reviewed",
            )
        })
        .collect();
    let mixed = sheet(&reviewed_of(&sealed, &all), v::FIX_PLAN);
    // One rule is ignored as accepted risk, so it has no place left to fix; the rest are alike.
    let state = &mixed[1][col(&mixed, v::H_DECISION_SO_FAR)];
    assert_eq!(state, v::CONFIRMED_BY_REVIEWER, "{state}");
}

#[test]
fn a_fix_decision_writes_one_record_per_member_and_the_merge_gate_accepts_it() {
    use crate::decisions::{DecisionCtx, DecisionStore};
    use camerata_api_types::decisions::{NewDecisionReq, Scope};

    let sealed = secret_report();
    let tree = sealed.tree.as_ref().expect("a tree");
    assert_eq!(tree.fixes.len(), 1, "{:?}", tree.fixes);
    let fix = &tree.fixes[0];
    let node_id = fix.node_id();
    assert!(tree.is_decidable(&node_id));
    let members = tree.batch_members(&node_id);
    assert_eq!(members.len(), 7, "every site of every rule group");

    let store = DecisionStore::new();
    let req = NewDecisionReq {
        key: node_id.clone(),
        scope: Scope::Node(node_id.clone()),
        state: DecisionState::Confirmed,
        reason: "one secret, one decision".to_string(),
        decided_by: String::new(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: String::new(),
        decided_at: String::new(),
    };
    let ctx = DecisionCtx {
        run_id: "insp-1",
        engine_version: "0.0.0-test",
        now: NOW,
    };
    let (node, written) = store
        .record_node("p1", &node_id, &members, &req, ctx)
        .expect("a Fix decision is recorded");
    assert_eq!(written, 7);
    let records = store.active("p1");
    let per_site: Vec<_> = records
        .iter()
        .filter(|d| d.inherited_from.as_deref() == Some(node.id.as_str()))
        .collect();
    assert_eq!(per_site.len(), 7, "one record for each member");
    assert_eq!(node.applies_to.len(), 7);

    // The reviewed report treats every member as decided, and the merge gate's per-record check
    // (every site a group decision covers has its own record) is satisfied.
    let reviewed = reviewed_of(&sealed, &records);
    let plan = sheet(&reviewed, v::FIX_PLAN);
    assert_eq!(
        plan[1][col(&plan, v::H_DECISION_SO_FAR)],
        v::CONFIRMED_BY_REVIEWER
    );
    let rows = crate::report_export::decisions_file::decision_rows(&reviewed);
    let gaps = crate::release_gate::held::group_decision_gaps(&rows);
    assert!(gaps.is_empty(), "{gaps:?}");
    assert!(rows
        .iter()
        .any(|d| d.scope == "node" && d.node_id == node_id));
}
