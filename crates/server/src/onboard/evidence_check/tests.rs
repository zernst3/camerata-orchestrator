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
fn a_snippet_mixing_a_redacted_real_secret_and_a_readable_docs_key_is_verified() {
    // An older stored row: the model-side redaction masked the real key and left the vendor's
    // documentation key readable, so the snippet matches the model-redacted window only.
    let real = format!("sk_live_{}", "a1B2c3D4e5F6g7H8i9J0k1L2");
    let docs = camerata_secrets::fixtures::stripe_docs_test_key();
    let source = format!("const a = 1;\nconst keys = ['{real}', '{docs}'];\nconst b = 2;\n");
    let files = vec![("src/pay.ts".to_string(), source)];
    let snippet =
        crate::onboard::redact::redact_visible(&format!("const keys = ['{real}', '{docs}'];"))
            .into_owned();
    assert!(
        !snippet.contains(&real) && snippet.contains(&docs),
        "{snippet}"
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
        standard_source: None,
        differs_from_default: None,
    }));
    let mut not_assessed = row(6, "the model declined");
    not_assessed.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::ToolFailed,
        reason_detail: "tool failed".to_string(),
    }));
    let mut dependency = row(3, "lodash@4.17.20");
    dependency.rule_id = crate::dep_audit::DEP_AUDIT_RULE_ID.to_string();
    let tool_message = sarif_rows("Possible open redirect: no allowlist check.").remove(0);
    let originals = vec![
        no_snippet,
        no_line,
        convention,
        not_assessed,
        dependency,
        tool_message,
    ];
    let mut rows = originals.clone();
    let counts = verify_evidence(&mut rows, &files(), None);
    assert_eq!(rows, originals, "an exempt row is not touched");
    assert_eq!(counts.exempt, 6);
    assert_eq!(counts.checked, 0);
    // The six rows hit every kind in the one list.
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
    // Deterministic rows (no `AI-` rule id, no calibration verdict): a model-written row on a
    // file the scan did not admit is held instead (#546).
    let mut rows = vec![row(6, REAL), row(6, "made up")];
    for r in &mut rows {
        r.rule_id = "SEM-TOOL-RULE".to_string();
        r.preview = true;
        r.preview_tool = Some("semgrep".to_string());
    }
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

// ── grouping keeps every row's own mark ──────────────────────────────────────────────────────

fn structural(line: usize, snippet: &str) -> Finding {
    Finding {
        severity: "low".to_string(),
        category: Some("arch-conformance".to_string()),
        confidence: Some("needs-review".to_string()),
        rule_id: "AI-ARCH-LAYERING".to_string(),
        ..row(line, snippet)
    }
}

#[test]
fn grouping_keeps_a_fabricated_occurrences_mark_on_that_row_and_only_that_row() {
    // Nothing is folded into a group row any more, so the check's marks stay on the rows it
    // marked: the fabricated occurrence stays held and names its site, its neighbour is untouched.
    let mut rows = vec![
        structural(3, "const app = express();"),
        structural(6, "this line was never written"),
    ];
    verify_evidence(&mut rows, &files(), None);
    let grouped = crate::review_tree::collapse_and_group(rows, &[]);
    assert_eq!(grouped.len(), 2, "both occurrences are still rows");
    let fabricated = grouped.iter().find(|f| f.line == 6).unwrap();
    assert!(marked(fabricated), "held: {:?}", fabricated.captures);
    assert!(
        sites_listed(fabricated).is_none(),
        "the row is its own site"
    );
    let real = grouped.iter().find(|f| f.line == 3).unwrap();
    assert!(!marked(real));
    assert_eq!(
        fabricated.group_path[1], real.group_path[1],
        "one rule node"
    );
}

#[test]
fn grouping_keeps_a_failing_absorbed_site_named_on_its_row() {
    let mut first = structural(3, "never written either");
    first.also_locations = vec![site("src/app.ts", 5, "also never written")];
    let mut rows = vec![first, structural(6, REAL)];
    verify_evidence(&mut rows, &files(), None);
    let grouped = crate::review_tree::collapse_and_group(rows, &[]);
    let held = grouped.iter().find(|f| f.line == 3).unwrap();
    assert!(marked(held));
    assert_eq!(
        sites_listed(held),
        Some("src/app.ts:5"),
        "the failing absorbed site is named"
    );
}

#[test]
fn grouped_verified_occurrences_are_not_held_by_this_rule() {
    let mut rows = vec![structural(3, "const app = express();"), structural(6, REAL)];
    verify_evidence(&mut rows, &files(), None);
    let grouped = crate::review_tree::collapse_and_group(rows, &[]);
    assert!(grouped
        .iter()
        .all(|f| !marked(f) && sites_listed(f).is_none()));
}

// ── absorbed sites the check does not apply to are counted too ───────────────────────────────

#[test]
fn every_absorbed_site_is_checked_or_exempt_and_the_two_add_up() {
    let mut consequence = site("src/app.ts", 3, "downstream impact, not code");
    consequence.consequence = true;
    let mut other_repo = site("src/app.ts", 3, "const app = express();");
    other_repo.repo = "acme/other".to_string();
    let mut f = row(6, REAL);
    f.also_locations = vec![
        site("src/app.ts", 6, REAL),
        site("src/app.ts", 3, "made up"),
        consequence,
        other_repo,
        site("src/app.ts", 3, ""),
        site("src/app.ts", 0, REAL),
    ];
    // A row exempt by kind: its sites are counted exempt, none is checked.
    let mut convention = row(6, "summary");
    convention.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "x".to_string(),
        convention_label: "X".to_string(),
        standard_source: None,
        differs_from_default: None,
    }));
    convention.also_locations = vec![site("src/app.ts", 6, REAL), site("src/app.ts", 6, REAL)];
    let total_sites = f.also_locations.len() + convention.also_locations.len();
    let mut rows = vec![f, convention];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert_eq!(
        (
            counts.site_checked,
            counts.site_verified,
            counts.site_unverified
        ),
        (2, 1, 1)
    );
    assert_eq!(counts.site_exempt, 6);
    assert_eq!(counts.sites_seen(), total_sites);
    assert!(counts.reconciles());
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_evidence_check(counts);
    ledger.record_evidence_check(counts);
    assert_eq!(ledger.evidence_check().site_exempt, 12);
    assert_eq!(ledger.evidence_check().sites_seen(), total_sites * 2);
}

// ---- a tool's message is not a quotation ----------------------------------------------------

/// A preview row as the scan builds it from a tool's SARIF: the snippet is the tool's message.
fn sarif_rows(message: &str) -> Vec<Finding> {
    let sarif = serde_json::json!({"runs": [{"results": [{
        "ruleId": "camerata.security.taint-open-redirect-js",
        "level": "error",
        "message": {"text": message},
        "locations": [{"physicalLocation": {
            "artifactLocation": {"uri": "src/app.ts"},
            "region": {"startLine": 6}
        }}]
    }]}]});
    crate::scan_tools::parse_sarif(
        "acme/api",
        crate::scan_tools::ScanTool::Semgrep,
        &sarif.to_string(),
    )
    .expect("the SARIF parses")
}

/// The shape from lumen: semgrep's taint rules say what they found in prose that never appears in
/// the file, and every such row was escalated as unverified evidence. The tool gave the row its
/// line; its message is a description, not code the row quotes.
#[test]
fn a_preview_row_whose_snippet_is_the_tools_message_is_not_held_for_unverified_evidence() {
    let mut rows = sarif_rows(
        "Possible open redirect: a redirect target this function did not construct itself is \
         handed to a redirect response with no allowlist check.",
    );
    assert_eq!(
        exemption(&rows[0]),
        Some(Exempt::ToolMessage),
        "{:?}",
        rows[0].snippet
    );
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
    assert_eq!((counts.exempt, counts.unverified), (1, 0));
}

/// A tool row that does quote code is still held to the file: the exemption is for a snippet that
/// IS the message, not for every preview row.
#[test]
fn a_preview_row_that_quotes_code_that_is_not_there_is_still_held() {
    let mut quoted = row(6, "db.execute(buildQuery(untrusted))");
    quoted.preview = true;
    quoted.preview_tool = Some("eslint".to_string());
    assert_eq!(exemption(&quoted), None);
    let mut rows = vec![quoted];
    verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]));
}

// ---- #546: a finding on a file the repo does not have is never passed unexamined ----

fn missing(mut f: Finding) -> Finding {
    f.path = "src/gone.ts".to_string();
    f
}

fn convention_row() -> Finding {
    let mut f = row(6, "uses a repository layer everywhere");
    f.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "repo".to_string(),
        convention_label: "Repository layer".to_string(),
        standard_source: None,
        differs_from_default: None,
    }));
    f
}

/// Each exempt shape, citing a file that is not there, is held with a reason.
#[test]
fn an_exempt_row_citing_a_missing_file_is_held_with_its_reason() {
    let shapes = [
        ("line 0", missing(row(0, "no lockfile is committed"))),
        ("empty quote", missing(row(6, ""))),
        ("informational kind", missing(convention_row())),
    ];
    for (name, shape) in shapes {
        let mut rows = vec![shape.clone()];
        let counts = verify_evidence(&mut rows, &files(), None);
        assert!(marked(&rows[0]), "{name}: {:?}", rows[0].captures);
        let why = rows[0].captures.get(EVIDENCE_UNVERIFIED_CAPTURE).unwrap();
        assert!(why.contains("src/gone.ts"), "{name}: {why}");
        assert_eq!(rows.len(), 1, "{name}: never dropped");
        let mut without_mark = rows[0].clone();
        without_mark.captures.remove(EVIDENCE_UNVERIFIED_CAPTURE);
        assert_eq!(without_mark, shape, "{name}: text never changed");
        assert_eq!(
            (counts.checked, counts.unverified, counts.exempt),
            (1, 1, 0),
            "{name}"
        );
        assert!(counts.reconciles(), "{name}");
    }
}

/// The same shapes on a file that is there stay exempt and unmarked.
#[test]
fn an_exempt_row_citing_a_file_that_exists_is_still_exempt() {
    let mut rows = vec![
        row(0, "no central error handler"),
        row(6, ""),
        convention_row(),
    ];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(rows.iter().all(|r| !marked(r)));
    assert_eq!((counts.exempt, counts.unverified), (3, 0));
}

/// A row that cites no file at all (a project-level row) has nothing to look up.
#[test]
fn an_exempt_row_citing_no_file_stays_exempt() {
    let mut project = row(0, "no security policy");
    project.path = String::new();
    let mut rows = vec![project];
    let counts = verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]));
    assert_eq!(counts.exempt, 1);
}

/// A file that exists on disk but outside the admitted files (here a file over the read limit).
fn excluded_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("camerata-evidence-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/big.ts"), FILE).unwrap();
    dir
}

fn on_big(mut f: Finding) -> Finding {
    f.path = "src/big.ts".to_string();
    f
}

#[test]
fn a_model_written_row_on_an_existing_file_that_was_not_admitted_is_held() {
    let dir = excluded_dir("model");
    let mut rows = vec![on_big(row(6, REAL))];
    let counts = verify_evidence(&mut rows, &files(), Some(&dir));
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    let why = rows[0].captures.get(EVIDENCE_UNVERIFIED_CAPTURE).unwrap();
    assert!(why.contains("admitted"), "{why}");
    assert_eq!((counts.verified, counts.unverified), (0, 1));
    // An adopted corpus rule the model calibrated, and an agent finder row, are model rows too.
    let mut adopted = on_big(row(6, REAL));
    adopted.rule_id = "SEC-HARDCODED-SECRET".to_string();
    adopted.confidence = Some("high".to_string());
    let mut agent = on_big(row(6, REAL));
    agent.rule_id = "SEC-HARDCODED-SECRET".to_string();
    agent.captures.insert(
        crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE.to_string(),
        "1".to_string(),
    );
    let mut rows = vec![adopted, agent];
    verify_evidence(&mut rows, &files(), Some(&dir));
    assert!(rows.iter().all(marked), "{rows:#?}");
}

#[test]
fn a_deterministic_row_on_an_existing_file_that_was_not_admitted_keeps_the_disk_fallback() {
    let dir = excluded_dir("det");
    let mut secrets = on_big(row(6, REAL));
    // An outside tool's row: deterministic by what the row itself says.
    secrets.rule_id = "SEM-TOOL-RULE".to_string();
    secrets.preview = true;
    secrets.preview_tool = Some("semgrep".to_string());
    let mut rows = vec![secrets];
    let counts = verify_evidence(&mut rows, &files(), Some(&dir));
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
    assert_eq!((counts.verified, counts.unverified), (1, 0));
}

// ---- #546 review fixes ----

/// A file that exists on disk but outside the admitted files, and a place to put others.
fn dir_with(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("camerata-evidence-{name}-{}", std::process::id()));
    for (path, text) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    dir
}

/// A row an outside tool wrote (a scan preview), the shape `scan_tools` produces.
fn deterministic(mut f: Finding) -> Finding {
    f.rule_id = "SEM-TOOL-RULE".to_string();
    f.preview = true;
    f.preview_tool = Some("semgrep".to_string());
    f
}

/// Ruling 1: a model-written exempt row on an excluded on-disk file is held.
#[test]
fn a_model_written_exempt_row_on_an_existing_file_that_was_not_admitted_is_held() {
    let dir = excluded_dir("exempt-model");
    let shapes = [
        ("line 0", on_big(row(0, "no central error handler"))),
        ("empty quote", on_big(row(6, ""))),
        ("informational kind", on_big(convention_row())),
    ];
    for (name, shape) in shapes {
        let mut rows = vec![shape.clone()];
        verify_evidence(&mut rows, &files(), Some(&dir));
        assert!(marked(&rows[0]), "{name}: {:?}", rows[0].captures);
        // The same shape from a deterministic source keeps the disk fallback: exempt, unmarked.
        let mut det = vec![deterministic(shape)];
        verify_evidence(&mut det, &files(), Some(&dir));
        assert!(!marked(&det[0]), "{name}: {:?}", det[0].captures);
    }
}

/// Ruling 3: a model row whose calibration failed (no confidence, a corpus rule id) is still
/// model-written, because the parser marked it at birth.
#[test]
fn a_model_row_with_no_confidence_is_still_model_written() {
    let dir = excluded_dir("no-confidence");
    // The rule is one the ledger lists as deterministic, so only the mark can make it model-written.
    let det = DeterministicRules::of(["SEC-NO-RAW-SQL-CONCAT-1"]);
    let mut born_model = on_big(row(6, REAL));
    born_model.rule_id = "SEC-NO-RAW-SQL-CONCAT-1".to_string();
    assert!(born_model.confidence.is_none());
    born_model
        .captures
        .insert("model_written".to_string(), "1".to_string());
    let mut rows = vec![born_model];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &det);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
}

/// Ruling 2: absorbed places get the same file check as the primary.
#[test]
fn an_absorbed_place_on_a_missing_file_holds_a_verified_primary() {
    // A verified primary on src/app.ts plus a place on a missing file, in each shape the quote
    // check skips: line 0, an empty quote, a consequence, and no cited line with a quote.
    let shapes: [(&str, MergedLocation); 3] = [
        ("line 0", site("src/gone.ts", 0, "no lockfile")),
        ("empty quote", site("src/gone.ts", 3, "")),
        ("consequence", {
            let mut s = site("src/gone.ts", 3, "impact text");
            s.consequence = true;
            s
        }),
    ];
    for (name, place) in shapes {
        let mut f = row(6, REAL);
        f.also_locations = vec![place];
        let mut rows = vec![f];
        verify_evidence(&mut rows, &files(), None);
        assert!(marked(&rows[0]), "{name}: {:?}", rows[0].captures);
        assert_eq!(
            rows[0]
                .captures
                .get(EVIDENCE_UNVERIFIED_SITES_CAPTURE)
                .map(String::as_str),
            Some(format!("src/gone.ts:{}", if name == "line 0" { 0 } else { 3 }).as_str()),
            "{name}"
        );
    }
}

#[test]
fn an_informational_row_with_a_place_on_a_missing_file_is_held() {
    let mut f = convention_row();
    f.also_locations = vec![site("src/gone.ts", 3, "x")];
    let mut rows = vec![f];
    verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
}

#[test]
fn a_place_on_an_unadmitted_file_holds_a_model_row_and_not_a_deterministic_one() {
    let dir = excluded_dir("place");
    let place = || site("src/big.ts", 6, REAL);
    let mut model = row(6, REAL);
    model.also_locations = vec![place()];
    let mut det = deterministic(row(6, REAL));
    det.also_locations = vec![{
        let mut p = place();
        p.rule_id = "SEM-TOOL-RULE".to_string();
        p
    }];
    let mut rows = vec![model, det];
    verify_evidence(&mut rows, &files(), Some(&dir));
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    assert!(!marked(&rows[1]), "{:?}", rows[1].captures);
}

/// Ruling 4: placeholder paths name no file.
#[test]
fn a_placeholder_path_is_not_a_missing_file_and_any_other_unknown_path_is() {
    for placeholder in ["(repo)", "(scan preview)"] {
        let mut f = row(0, "a finding about the project");
        f.path = placeholder.to_string();
        let mut rows = vec![f];
        let counts = verify_evidence(&mut rows, &files(), None);
        assert!(!marked(&rows[0]), "{placeholder}: {:?}", rows[0].captures);
        assert_eq!(counts.exempt, 1, "{placeholder}");
    }
    let mut other = row(0, "a finding");
    other.path = "(other)".to_string();
    let mut rows = vec![other];
    verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]));
}

/// Ruling 5: `./` and doubled slashes find the admitted file; a case variant stays held.
#[test]
fn a_leading_dot_slash_and_doubled_slashes_find_the_admitted_file() {
    let mut a = row(6, REAL);
    a.path = "./src/app.ts".to_string();
    let mut b = row(6, REAL);
    b.path = "src//app.ts".to_string();
    let mut c = row(6, REAL);
    c.path = "SRC/app.ts".to_string();
    let mut model_exempt = row(0, "no handler");
    model_exempt.path = "./src/app.ts".to_string();
    let mut rows = vec![a, b, c, model_exempt];
    verify_evidence(&mut rows, &files(), None);
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
    assert!(!marked(&rows[1]), "{:?}", rows[1].captures);
    assert!(marked(&rows[2]), "a case variant stays held");
    assert!(!marked(&rows[3]), "{:?}", rows[3].captures);
}

/// Ruling 6: a directory and a symlink are not files.
#[test]
fn a_directory_path_and_a_symlink_path_are_held_as_missing() {
    let dir = dir_with("dirlink", &[("src/real.ts", FILE)]);
    let link = dir.join("src/link.ts");
    if !link.exists() {
        std::os::unix::fs::symlink(dir.join("src/real.ts"), &link).unwrap();
    }
    let mut directory = row(0, "about a folder");
    directory.path = "src".to_string();
    let mut symlink = row(0, "about a link");
    symlink.path = "src/link.ts".to_string();
    let mut real = row(0, "about a file");
    real.path = "src/real.ts".to_string();
    let mut rows = vec![
        deterministic(directory),
        deterministic(symlink),
        deterministic(real),
    ];
    verify_evidence(&mut rows, &[], Some(&dir));
    assert!(marked(&rows[0]), "a directory is not a file");
    assert!(marked(&rows[1]), "a symlink is not followed");
    assert!(!marked(&rows[2]), "{:?}", rows[2].captures);
}

/// An unmarked row (an older scan's, or one whose calibration failed) is deterministic only when
/// a deterministic producer made it: an outside tool's row, or a rule the scan ledger lists under a
/// deterministic tier. Anything else is model-written and held on a file that was not admitted.
#[test]
fn an_unmarked_row_is_deterministic_only_by_a_deterministic_producer() {
    let dir = excluded_dir("unmarked");
    let unmarked_engine = || {
        let mut f = on_big(row(6, REAL));
        f.rule_id = "ARCH-NO-DIRECT-DB-1".to_string();
        f
    };
    assert!(unmarked_engine().confidence.is_none());
    assert!(unmarked_engine().captures.is_empty());

    // The same unmarked Engine row: held with no ledger, held when the ledger lists the rule under
    // the semantic tier, kept when the ledger lists it under a deterministic tier.
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    ledger.record_rule(
        "acme/api",
        "ARCH-NO-DIRECT-DB-1",
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        1,
        1,
    );
    ledger.record_rule(
        "acme/api",
        "SEC-NO-HARDCODED-SECRETS-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        1,
        1,
    );
    let det = DeterministicRules::from_ledger(&ledger);
    let mut engine = vec![unmarked_engine()];
    verify_evidence_with(&mut engine, &files(), Some(&dir), &det);
    assert!(marked(&engine[0]), "{:?}", engine[0].captures);
    let mut engine = vec![unmarked_engine()];
    verify_evidence(&mut engine, &files(), Some(&dir));
    assert!(marked(&engine[0]), "no ledger: held");
    let mut floor = unmarked_engine();
    floor.rule_id = "SEC-NO-HARDCODED-SECRETS-1".to_string();
    let mut rows = vec![floor];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &det);
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);

    // An unmarked outside-tool row, the dependency scanner and the runtime audit keep the fallback.
    let tool = deterministic(unmarked_engine());
    let mut dependency = unmarked_engine();
    dependency.rule_id = crate::dep_audit::DEP_AUDIT_RULE_ID.to_string();
    let mut runtime = unmarked_engine();
    runtime.rule_id = crate::runtime_audit::EOL_RULE_ID.to_string();
    let mut rows = vec![tool, dependency, runtime];
    verify_evidence(&mut rows, &files(), Some(&dir));
    for r in &rows {
        assert!(!marked(r), "{}: {:?}", r.rule_id, r.captures);
    }
}

/// A model-written row's place in another repository cannot be checked and is held; a
/// deterministic row's is unchanged.
#[test]
fn a_model_rows_place_in_another_repo_is_held_and_a_tool_rows_is_not() {
    let elsewhere =
        || MergedLocation::site("acme/web", "src/other.ts", 3, "AI-RAW-SQL", "x", false);
    let mut model = row(6, REAL);
    model.also_locations = vec![elsewhere()];
    let mut tool = deterministic(row(6, REAL));
    tool.also_locations = vec![{
        let mut p = elsewhere();
        p.rule_id = "SEM-TOOL-RULE".to_string();
        p
    }];
    let mut rows = vec![model, tool];
    verify_evidence(&mut rows, &files(), None);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    let why = rows[0].captures.get(EVIDENCE_UNVERIFIED_CAPTURE).unwrap();
    assert!(why.contains("could not be checked"), "{why}");
    assert!(!marked(&rows[1]), "{:?}", rows[1].captures);
}

/// The birth mark outranks every deterministic signal: a row the parser marked is model-written
/// even when its rule id is one the ledger lists under a deterministic tier (a model row that
/// adopted a floor rule's id).
#[test]
fn the_birth_mark_wins_over_a_deterministic_rule_id() {
    let dir = excluded_dir("birth-mark");
    let det = DeterministicRules::of(["SEC-NO-HARDCODED-SECRETS-1"]);
    let mut born_model = on_big(row(6, REAL));
    born_model.rule_id = "SEC-NO-HARDCODED-SECRETS-1".to_string();
    let mut floor = born_model.clone();
    born_model
        .captures
        .insert(MODEL_WRITTEN_CAPTURE.to_string(), "1".to_string());
    // Same rule id, no mark: the floor's own row keeps the disk fallback.
    floor.captures.remove(MODEL_WRITTEN_CAPTURE);
    let mut rows = vec![born_model, floor];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &det);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    assert!(!marked(&rows[1]), "{:?}", rows[1].captures);
}

/// Round 2, item 2: confidence never decides. An unrated runtime row carries a "needs-review"
/// confidence and is deterministic by its rule; it keeps the disk fallback.
#[test]
fn an_unrated_runtime_row_is_not_model_written() {
    let dir = excluded_dir("unrated-runtime");
    let mut vuln = on_big(row(6, REAL));
    vuln.rule_id = crate::runtime_audit::VULN_RULE_ID.to_string();
    vuln.confidence = Some("needs-review".to_string());
    vuln.needs_review = true;
    let mut rows = vec![vuln];
    verify_evidence(&mut rows, &[], Some(&dir));
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
}

/// Round 2, item 3: an older stored row carries `produced_tier = semantic` instead of the birth
/// mark, and a model can report under a floor rule id the ledger lists as deterministic.
#[test]
fn a_stored_row_tagged_semantic_is_model_written_even_under_a_floor_rule_id() {
    let dir = excluded_dir("produced-tier");
    let det = DeterministicRules::of(["SEC-NO-RAW-SQL-CONCAT-1"]);
    let mut stored = on_big(row(6, REAL));
    stored.rule_id = "SEC-NO-RAW-SQL-CONCAT-1".to_string();
    let mut floor = stored.clone();
    stored.captures.insert(
        crate::onboard::PRODUCED_TIER_CAPTURE.to_string(),
        "semantic".to_string(),
    );
    floor.captures.insert(
        crate::onboard::PRODUCED_TIER_CAPTURE.to_string(),
        "deterministic".to_string(),
    );
    let mut rows = vec![stored, floor];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &det);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    assert!(!marked(&rows[1]), "{:?}", rows[1].captures);
}

/// A place a model wrote (an `AI-` rule) on a deterministic row is held when its file was not
/// admitted.
#[test]
fn a_model_written_place_on_a_deterministic_row_is_held() {
    let dir = excluded_dir("model-place");
    let mut tool = deterministic(row(6, REAL));
    tool.also_locations = vec![site("src/big.ts", 6, REAL)]; // rule AI-RAW-SQL
    let mut rows = vec![tool];
    verify_evidence(&mut rows, &files(), Some(&dir));
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
}

/// An agent finder row is model-written by what it carries, even when its rule id is one the
/// ledger lists as deterministic and it has no birth mark or semantic tier.
#[test]
fn an_agent_finder_row_under_a_deterministic_rule_id_is_held_on_an_unadmitted_file() {
    let dir = excluded_dir("agent-det-rule");
    let det = DeterministicRules::of(["SEC-NO-RAW-SQL-CONCAT-1"]);
    let mut agent = on_big(row(6, REAL));
    agent.rule_id = "SEC-NO-RAW-SQL-CONCAT-1".to_string();
    agent.captures.insert(
        crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE.to_string(),
        "1".to_string(),
    );
    let mut rows = vec![agent];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &det);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    let why = rows[0].captures.get(EVIDENCE_UNVERIFIED_CAPTURE).unwrap();
    assert!(why.contains("admitted"), "{why}");
}

/// A row the reviewer added by hand is deterministic by structure: not model-written, so it keeps
/// the disk fallback, even under a rule id the ledger does not list.
#[test]
fn a_reviewer_row_on_an_unadmitted_file_is_not_held_as_model_written() {
    let dir = excluded_dir("reviewer-row");
    let mut reviewer = on_big(row(6, REAL));
    reviewer.rule_id = "REVIEWER-ADDED".to_string();
    reviewer.captures.insert(
        crate::report_export::overlay::PROVENANCE_CAPTURE.to_string(),
        crate::report_export::overlay::REVIEWER_PROVENANCE.to_string(),
    );
    let mut rows = vec![reviewer];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &DeterministicRules::none());
    assert!(!marked(&rows[0]), "{:?}", rows[0].captures);
}

/// An `AI-` rule id is a model's, even if a rule set were to list it as deterministic.
#[test]
fn an_ai_prefixed_rule_id_is_model_written_even_if_listed_as_deterministic() {
    let dir = excluded_dir("ai-prefix");
    let det = DeterministicRules::of(["AI-RAW-SQL"]);
    let mut ai = on_big(row(6, REAL));
    ai.rule_id = "AI-RAW-SQL".to_string();
    let mut rows = vec![ai];
    verify_evidence_with(&mut rows, &files(), Some(&dir), &det);
    assert!(marked(&rows[0]), "{:?}", rows[0].captures);
    let why = rows[0].captures.get(EVIDENCE_UNVERIFIED_CAPTURE).unwrap();
    assert!(why.contains("admitted"), "{why}");
}
