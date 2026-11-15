//! "Patterns across services" never says clean beside a real problem.
//!
//! A cell may read clean only when its service has no open problem for the rule (or the rule's
//! family: a tool row the review tree files under the rule, a source collapsed into another row,
//! a rule a row also matches or a site a merge absorbed). A problem that needs a decision, or one
//! the reviewer put back after the engine threw it out, is open. A problem the reviewer ruled out
//! is not. Synthetic two-service reports throughout: `acme/api` and `acme/web`.

use std::collections::HashMap;

use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
use camerata_api_types::decisions::{Decision, DecisionState, Scope};

use super::overlay::apply_overlay;
use super::*;
use crate::onboard::patterns_matrix::{build, cell_for};
use crate::onboard::{Finding, ScanReport};
use crate::scan_ledger::{CellState, RuleTier};

const RULE: &str = "ARCH-SERVER-AUTHZ-1";
const API: &str = "acme/api";
const WEB: &str = "acme/web";

fn two_services() -> ScanReport {
    let mut report = ScanReport::gated(&[API.to_string(), WEB.to_string()]);
    report.gated = false;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:00:00Z".to_string();
    report
}

fn ran(report: &mut ScanReport, repo: &str, emitted: usize) {
    report
        .ledger
        .record_rule(repo, RULE, RuleTier::Semantic, true, None, 5, emitted);
}

fn problem(repo: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: RULE.to_string(),
        severity: "high".to_string(),
        snippet: format!("router.post(\"/admin/{line}\", handler)"),
        detail: "The handler changes a record without checking the caller may do so.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

fn convention(repo: &str) -> Finding {
    Finding {
        path: "src/routes.ts".to_string(),
        line: 1,
        severity: "low".to_string(),
        kind: Some(FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: "authorization-on-the-server-ui-renders-permission-hints"
                .to_string(),
            convention_label: "Authorization on the server".to_string(),
        })),
        ..problem(repo, "src/routes.ts", 1)
    }
}

fn decision(key: &str, state: DecisionState) -> Decision {
    Decision {
        id: "dec-000001".to_string(),
        key: key.to_string(),
        state,
        reason: "a real problem".to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "auditor".to_string(),
        decided_at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

fn section(reviewed: &ReviewedReport) -> PatternsAcrossServicesJson {
    build_report_json(reviewed, None, &ReportOptions::default())
        .patterns_across_services
        .expect("a project of two services has the section")
}

fn cell<'a>(s: &'a PatternsAcrossServicesJson, repo: &str) -> &'a PatternCellJson {
    let row = s.rows.iter().find(|r| r.rule_id == RULE);
    assert!(
        row.is_some(),
        "the services differ on {RULE}, so the section has its row: {:?}",
        s.rows
    );
    let found = row.and_then(|r| r.cells.iter().find(|c| c.repo == repo));
    assert!(found.is_some(), "the row has a cell for {repo}");
    found.unwrap()
}

#[test]
fn a_followed_convention_does_not_hide_a_problem_merged_into_another_row() {
    // The api's rule found a convention it follows AND a problem; the problem was collapsed into
    // a tool's row at the same place, so no row carries the rule id but that row's source does.
    let mut r = two_services();
    ran(&mut r, API, 2);
    ran(&mut r, WEB, 0);
    r.findings.push(convention(API));
    let mut tool = problem(API, "src/admin.ts", 40);
    tool.rule_id = "semgrep.express-missing-authz".to_string();
    tool.preview_tool = Some("semgrep".to_string());
    tool.flagged_by = vec![crate::review_tree::FlaggedBy {
        rule_id: RULE.to_string(),
        tool: None,
        site_key: "k".to_string(),
        path: "src/admin.ts".to_string(),
        line: 40,
    }];
    r.findings.push(tool);
    assert_eq!(
        cell_for(&r, RULE, API),
        CellState::Violated { sites: 1 },
        "the api holds an open problem for the rule, so its cell is not clean"
    );
    assert_eq!(cell_for(&r, RULE, WEB), CellState::Clean);
    let m = build(&r);
    assert_eq!(m.rows.len(), 1, "the services differ on the rule: {m:?}");
    assert_eq!(
        m.rows[0].clean(),
        vec![WEB],
        "clean only in the other service"
    );
}

#[test]
fn a_problem_the_review_tree_files_under_the_rule_makes_the_cell_not_clean() {
    // A tool row the tree places under the native rule (its family) while the rule's own ledger
    // entry found nothing.
    let mut r = two_services();
    ran(&mut r, API, 0);
    ran(&mut r, WEB, 1);
    r.findings.push(problem(WEB, "src/admin.ts", 7));
    let mut tool = problem(API, "src/admin.ts", 40);
    tool.rule_id = "semgrep.express-missing-authz".to_string();
    tool.preview_tool = Some("semgrep".to_string());
    tool.group_path = vec![
        "cat:authorization".to_string(),
        format!("rule:{RULE}"),
        format!("sub:{RULE}/1"),
    ];
    r.findings.push(tool);
    assert_eq!(
        cell_for(&r, RULE, API),
        CellState::Violated { sites: 1 },
        "a row in the rule's family is a problem for the rule"
    );
    assert!(
        build(&r).rows.is_empty(),
        "both services have a problem, so they do not differ"
    );
}

#[test]
fn a_held_problem_the_reviewer_put_back_makes_the_exported_cell_not_clean() {
    // At the seal the api's problem had been thrown out by the engine and only the convention
    // row was left, so the matrix stored on the sealed report reads clean for the api. The
    // reviewer put the problem back: it is held until decided, and it is open.
    let mut r = two_services();
    ran(&mut r, API, 2);
    ran(&mut r, WEB, 1);
    r.findings.push(convention(API));
    r.findings.push(problem(WEB, "src/admin.ts", 7));
    let mut thrown = problem(API, "src/admin.ts", 40);
    crate::engine_discard::mark(&mut thrown, crate::engine_discard::DISCARD_VERDICT);
    r.discarded = vec![thrown];
    r.ledger.set_discarded(&r.discarded);
    let sealed = r.seal();
    let key = sealed.discarded[0].site_key.clone();
    let reviewed = apply_overlay(
        &sealed,
        &[decision(&key, DecisionState::EngineRestored)],
        &[],
        &[],
    );
    assert!(
        reviewed
            .report()
            .findings
            .iter()
            .any(|f| f.repo == API && f.path == "src/admin.ts"),
        "anti-vacuity: the problem is back among the rows"
    );
    let s = section(&reviewed);
    assert!(
        s.rows
            .iter()
            .filter(|row| row.rule_id == RULE)
            .flat_map(|row| row.cells.iter())
            .all(|c| !(c.repo == API && c.state == CellState::Clean)),
        "the api has an open (held) problem for the rule and the section says clean: {:?}",
        s.rows
    );
}

#[test]
fn a_problem_that_needs_a_decision_is_open() {
    let mut r = two_services();
    ran(&mut r, API, 1);
    ran(&mut r, WEB, 0);
    let mut held = problem(API, "src/admin.ts", 40);
    held.needs_review = true;
    held.confidence = Some("needs-review".to_string());
    r.findings.push(held);
    let sealed = r.seal();
    let s = section(&ReviewedReport::unreviewed(&sealed, &HashMap::new()));
    assert_eq!(cell(&s, API).state, CellState::Violated { sites: 1 });
    assert_eq!(cell(&s, WEB).state, CellState::Clean);
}

#[test]
fn a_problem_the_reviewer_ruled_out_lets_the_cell_read_clean() {
    // The api's only problem is ruled out; the web's is open. The api reads clean and the web
    // names its one problem.
    let mut r = two_services();
    ran(&mut r, API, 1);
    ran(&mut r, WEB, 1);
    r.findings.push(problem(API, "src/admin.ts", 40));
    r.findings.push(problem(WEB, "src/admin.ts", 7));
    let sealed = r.seal();
    let api_key = sealed
        .findings
        .iter()
        .find(|f| f.repo == API)
        .map(finding_key)
        .unwrap();
    let mut dispositions = HashMap::new();
    dispositions.insert(
        api_key,
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the route is behind the admin gateway".to_string(),
            ..DispositionWire::default()
        },
    );
    let s = section(&ReviewedReport::unreviewed(&sealed, &dispositions));
    assert_eq!(cell(&s, API).state, CellState::Clean);
    assert_eq!(cell(&s, API).label, "Clean");
    assert_eq!(cell(&s, WEB).state, CellState::Violated { sites: 1 });
    assert_eq!(
        cell(&s, WEB).label,
        "1 problem",
        "a cell with problems says so in the product's word and names the count"
    );
}
