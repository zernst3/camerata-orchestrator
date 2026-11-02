//! M5: the evidence check marks a row whose quoted snippet is not near its line, and only that.

use camerata_api_types::audit::{
    ConventionObservedKind, FindingKind, NotAssessedKind, NotAssessedReason,
};

use super::*;

const FILE: &str = "\
import express from 'express';

const app = express();

app.get('/users/:id', async (req, res) => {
  const row = await db.query(`select * from users where id = ${req.params.id}`);
  res.json(row);
});

app.listen(3000);
";

fn files() -> Vec<(String, String)> {
    vec![("src/app.ts".to_string(), FILE.to_string())]
}

fn row(line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/api".to_string(),
        path: "src/app.ts".to_string(),
        line,
        rule_id: "AI-RAW-SQL".to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        detail: "Raw SQL built from a request parameter.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

fn marked(f: &Finding) -> bool {
    f.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE)
}

const REAL: &str = "const row = await db.query(`select * from users where id = ${req.params.id}`);";

#[test]
fn a_fabricated_snippet_is_flagged_and_the_row_is_otherwise_untouched() {
    let original = row(6, "db.execute(buildQuery(untrusted))");
    let mut rows = vec![original.clone()];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    assert_eq!(counts.unverified, 1);
    assert_eq!(counts.verified, 0);
    // Never dropped, text never changed: only the mark is added.
    assert_eq!(rows.len(), 1);
    let mut without_mark = rows[0].clone();
    without_mark.captures.remove(EVIDENCE_UNVERIFIED_CAPTURE);
    assert_eq!(without_mark, original);
}

#[test]
fn a_snippet_two_lines_off_is_verified() {
    // The code is on line 6; the model said 4 and then 8.
    for cited in [4, 8] {
        let mut rows = vec![row(cited, REAL)];
        let counts = verify_evidence(&mut rows, &files(), None);
        assert!(!marked(&rows[0]), "cited line {cited}");
        assert_eq!((counts.verified, counts.unverified), (1, 0));
    }
}

/// The same code, wrapped over two lines the way a model often quotes it. The exact-line
/// correction cannot find it (it matches one line), so only the window can verify it.
const REAL_WRAPPED: &str =
    "const row =\n  await db.query(`select * from users where id = ${req.params.id}`);";

#[test]
fn a_snippet_beyond_the_window_is_flagged() {
    // The code is on line 6. Line 10 is four lines past it, one more than the window.
    assert_eq!(EVIDENCE_WINDOW_LINES, 3);
    let mut rows = vec![row(10, REAL_WRAPPED)];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]));
    assert_eq!(counts.unverified, 1);
    // Three lines past is inside the window.
    let mut near = vec![row(9, REAL_WRAPPED)];
    verify_evidence(&mut near, &files(), None);
    assert!(!marked(&near[0]), "{:?}", near[0].captures);
}

#[test]
fn the_existing_line_correction_runs_first() {
    // The model said line 10; the snippet is on line 6 verbatim, so the row is moved there and
    // then verified, as `resolve_finding_lines` always did.
    let mut rows = vec![row(10, REAL)];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert_eq!(rows[0].line, 6);
    assert!(!marked(&rows[0]));
    assert_eq!(counts.verified, 1);
}

#[test]
fn a_row_whose_snippet_is_near_its_line_keeps_the_line_its_producer_cited() {
    // A detector may cite the line after the one it quotes (the rule's `allow` line, with the
    // `match` line quoted). Inside the window the row is verified and left exactly as it was.
    let mut rows = vec![row(5, "const app = express();")];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert_eq!(rows[0].line, 5, "a row near its snippet is not moved");
    assert_eq!(counts.verified, 1);
}

#[test]
fn a_snippet_that_differs_only_in_whitespace_is_verified() {
    let reflowed =
        "const   row =\n    await db.query(`select * from users where id = ${req.params.id}`);";
    let mut rows = vec![row(6, reflowed)];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
    assert_eq!(counts.verified, 1);
}

#[test]
fn a_snippet_cut_short_by_the_snippet_cap_is_verified() {
    let mut rows = vec![row(6, "const row = await db.query(`select * from users")];
    verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]));
}

#[test]
fn a_redacted_snippet_is_checked_against_the_redacted_file() {
    let key = format!("sk_live_{}", "a1B2c3D4e5F6g7H8i9J0k1L2");
    let source = format!("const a = 1;\nconst stripe = new Stripe('{key}');\nconst b = 2;\n");
    let files = vec![("src/pay.ts".to_string(), source)];
    let mut snippet = format!("const stripe = new Stripe('{key}');");
    crate::onboard::redact::redact_string(&mut snippet);
    assert!(
        !snippet.contains(&key),
        "the fixture must redact: {snippet}"
    );
    let mut f = row(2, &snippet);
    f.path = "src/pay.ts".to_string();
    let mut rows = vec![f];
    verify_evidence(&mut rows, &files, None);
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
}

#[test]
fn every_exempt_kind_is_left_alone_and_none_is_counted_as_checked() {
    let mut no_snippet = row(6, "");
    no_snippet.rule_id = "COVERAGE-1".to_string();
    let no_line = row(0, "no central error handler");
    let mut convention = row(6, "uses a repository layer everywhere");
    convention.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "repo".to_string(),
        convention_label: "Repository layer".to_string(),
    }));
    let mut not_assessed = row(6, "the model declined");
    not_assessed.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::ToolFailed,
        reason_detail: "tool failed".to_string(),
    }));
    let mut dependency = row(3, "lodash@4.17.20");
    dependency.rule_id = crate::dep_audit::DEP_AUDIT_RULE_ID.to_string();
    let originals = vec![no_snippet, no_line, convention, not_assessed, dependency];
    let mut rows = originals.clone();
    let counts = verify_evidence(&mut rows, &files(), None);
    assert_eq!(rows, originals, "an exempt row is not touched");
    assert_eq!(counts.exempt, 5);
    assert_eq!(counts.checked, 0);
    // The five rows hit the four kinds in the one list.
    let kinds: Vec<Exempt> = originals.iter().filter_map(exemption).collect();
    for kind in Exempt::ALL {
        assert!(kinds.contains(&kind), "{}", kind.label());
    }
}

#[test]
fn the_counts_reconcile() {
    let mut rows = vec![
        row(6, REAL),
        row(5, REAL),
        row(6, "made up"),
        row(6, "also made up"),
        row(0, "file level"),
        row(9, ""),
    ];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert_eq!(counts.checked, 4);
    assert_eq!(counts.verified, 2);
    assert_eq!(counts.unverified, 2);
    assert_eq!(counts.exempt, 2);
    assert!(counts.reconciles());
    assert_eq!(counts.rows_seen(), rows.len());
    let marks = rows.iter().filter(|f| marked(f)).count();
    assert_eq!(marks, counts.unverified);
}

#[test]
fn the_ledger_adds_each_repos_counts() {
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    let mut a = vec![row(6, REAL), row(6, "made up")];
    let mut b = vec![row(6, REAL)];
    verify_and_record(&mut ledger, &mut a, &files(), None);
    verify_and_record(&mut ledger, &mut b, &files(), None);
    let total = ledger.evidence_check();
    assert_eq!(
        (
            total.checked,
            total.verified,
            total.unverified,
            total.exempt
        ),
        (3, 2, 1, 0)
    );
    assert!(total.reconciles());
    // It survives the ledger being written out and read back.
    let json = serde_json::to_string(&ledger).unwrap();
    let back: crate::scan_ledger::ScanLedger = serde_json::from_str(&json).unwrap();
    assert_eq!(back.evidence_check(), total);
}

#[test]
fn a_mark_from_an_earlier_run_is_cleared_when_the_row_now_verifies() {
    let mut f = row(6, REAL);
    f.captures
        .insert(EVIDENCE_UNVERIFIED_CAPTURE.to_string(), "old".to_string());
    let mut rows = vec![f];
    verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]));
}

#[test]
fn a_file_the_scan_never_held_is_read_from_the_repo_folder() {
    let dir = std::env::temp_dir().join(format!("camerata-evidence-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/app.ts"), FILE).unwrap();
    let mut rows = vec![row(6, REAL), row(6, "made up")];
    let counts = verify_evidence(&mut rows, &[], Some(&dir));
    assert_eq!((counts.verified, counts.unverified), (1, 1));
    assert!(!marked(&rows[0]) && marked(&rows[1]));
    let _ = std::fs::remove_file(dir.join("src/app.ts"));
}

#[test]
fn a_file_that_does_not_exist_is_unverified_and_a_path_out_of_the_repo_is_not_followed() {
    let dir = std::env::temp_dir().join(format!("camerata-evidence-esc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut missing = row(6, REAL);
    missing.path = "src/gone.ts".to_string();
    let mut escape = row(6, REAL);
    escape.path = "../outside.ts".to_string();
    let mut rows = vec![missing, escape];
    let counts = verify_evidence(&mut rows, &[], Some(&dir));
    assert_eq!(counts.unverified, 2);
    assert!(rows.iter().all(marked));
}

#[test]
fn a_line_past_the_end_of_the_file_is_unverified() {
    let mut rows = vec![row(400, REAL_WRAPPED)];
    verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]));
}

// ── absorbed sites ───────────────────────────────────────────────────────────────────────────

use crate::onboard::MergedLocation;

fn site(path: &str, line: usize, snippet: &str) -> MergedLocation {
    MergedLocation::site("acme/api", path, line, "AI-RAW-SQL", snippet, false)
}

fn sites_listed(f: &Finding) -> Option<&str> {
    f.captures
        .get(EVIDENCE_UNVERIFIED_SITES_CAPTURE)
        .map(String::as_str)
}

#[test]
fn a_row_whose_primary_verifies_but_whose_absorbed_site_does_not_is_held_and_names_the_site() {
    let mut f = row(6, REAL);
    f.also_locations = vec![
        site("src/app.ts", 6, REAL),
        site("src/app.ts", 3, "app.destroyEverything(untrusted)"),
    ];
    let mut rows = vec![f];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]), "the row is held: {:?}", rows[0].captures);
    assert_eq!(sites_listed(&rows[0]), Some("src/app.ts:3"));
    // The primary itself verified; only the site failed.
    assert_eq!(
        (counts.checked, counts.verified, counts.unverified),
        (1, 0, 1)
    );
    assert_eq!(
        (
            counts.site_checked,
            counts.site_verified,
            counts.site_unverified
        ),
        (2, 1, 1)
    );
    assert!(counts.reconciles());
    // Nothing about the row's text or its sites changed.
    assert_eq!(rows[0].also_locations.len(), 2);
    assert_eq!(
        rows[0].also_locations[1].snippet,
        "app.destroyEverything(untrusted)"
    );
}

#[test]
fn verified_sites_leave_the_row_alone_and_are_counted() {
    let mut f = row(6, REAL);
    f.also_locations = vec![site("src/app.ts", 5, REAL)];
    let mut rows = vec![f];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]));
    assert!(sites_listed(&rows[0]).is_none());
    assert_eq!((counts.site_checked, counts.site_verified), (1, 1));
}

#[test]
fn a_consequence_site_and_a_site_with_no_snippet_are_not_checked() {
    let mut f = row(6, REAL);
    let mut consequence = site("src/app.ts", 3, "downstream impact, not code");
    consequence.consequence = true;
    f.also_locations = vec![consequence, site("src/app.ts", 3, "  ")];
    let mut rows = vec![f];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]));
    assert_eq!(counts.site_checked, 0);
}

#[test]
fn a_site_mark_from_an_earlier_run_is_cleared_when_every_site_now_verifies() {
    let mut f = row(6, REAL);
    f.also_locations = vec![site("src/app.ts", 6, REAL)];
    f.captures.insert(
        EVIDENCE_UNVERIFIED_SITES_CAPTURE.to_string(),
        "old:1".to_string(),
    );
    f.captures
        .insert(EVIDENCE_UNVERIFIED_CAPTURE.to_string(), "old".to_string());
    let mut rows = vec![f];
    verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]));
    assert!(sites_listed(&rows[0]).is_none());
}
