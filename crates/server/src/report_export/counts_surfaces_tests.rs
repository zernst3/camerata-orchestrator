//! Every surface prints from the counts object (single counts, work order C). Synthetic rows
//! only. Each test here was seen failing at its assertion before the surfaces were changed.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;

fn json_of(findings: Vec<crate::onboard::Finding>) -> AuditReportJson {
    let sealed = report_with(findings, vec!["SYN-A-1", "SYN-B-1"]).seal();
    build_report_json(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
}

/// A named string field of a serialised part of the PDF data; empty when the field is not there.
fn text_field(part: &impl serde::Serialize, key: &str) -> String {
    serde_json::to_value(part)
        .ok()
        .and_then(|v| v.get(key).and_then(|s| s.as_str().map(str::to_string)))
        .unwrap_or_default()
}

/// Two rows at one place and one dependency advisory.
fn two_rows_one_place_and_a_dependency() -> Vec<crate::onboard::Finding> {
    vec![
        finding("SYN-A-1", "src/a.rs", 5, "high"),
        finding("SYN-B-1", "src/a.rs", 5, "medium"),
        finding(crate::dep_audit::DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high"),
    ]
}

#[test]
fn narrative_and_methodology_state_the_same_problem_total() {
    let json = json_of(two_rows_one_place_and_a_dependency());
    assert_eq!(json.counts.problems.total, 1, "anti-vacuity: one place");
    assert_eq!(json.counts.rows.total, 3, "anti-vacuity: three source rows");
    let narrative = &json.executive_summary.narrative;
    let reconciliation = text_field(&json.methodology, "reconciliation");
    assert!(
        narrative.contains("found 1 problem."),
        "the narrative states the problems as places: {narrative:?}"
    );
    assert!(
        reconciliation.contains("2 problem rows at 1 place"),
        "the methodology line reconciles the rows to the same places: {reconciliation:?}"
    );
    assert!(
        reconciliation.starts_with("3 source rows:"),
        "{reconciliation:?}"
    );
    for text in [narrative.as_str(), reconciliation.as_str()] {
        assert!(
            !text.contains("3 problems"),
            "no sentence calls three source rows problems: {text:?}"
        );
    }
}

#[test]
fn cover_strip_severities_add_up_to_the_problem_total() {
    let json = json_of(vec![
        finding("SYN-A-1", "src/a.rs", 5, "critical"),
        finding("SYN-A-1", "src/b.rs", 5, "high"),
        finding("SYN-A-1", "src/c.rs", 5, "medium"),
        finding("SYN-A-1", "src/d.rs", 5, "medium"),
        finding("SYN-A-1", "src/e.rs", 5, "low"),
    ]);
    assert_eq!(json.counts.problems.total, 5, "anti-vacuity");
    let strip = text_field(&json.cover, "strip");
    let number_before = |word: &str| -> Option<usize> {
        let at = strip.find(&format!(" {word}"))?;
        strip[..at]
            .rsplit(' ')
            .next()?
            .replace(',', "")
            .parse()
            .ok()
    };
    let parts: Vec<Option<usize>> = ["critical", "high", "medium", "low"]
        .iter()
        .map(|w| number_before(w))
        .collect();
    assert!(
        parts.iter().all(Option::is_some),
        "the strip states all four severities, Low included: {strip:?}"
    );
    let sum: usize = parts.iter().flatten().sum();
    assert_eq!(sum, json.counts.problems.total, "{strip:?}");
}

/// Template and builders: no surface counts by itself.
#[test]
fn no_surface_counts_by_itself() {
    let root = env!("CARGO_MANIFEST_DIR");
    let read = |p: &str| std::fs::read_to_string(format!("{root}/{p}")).unwrap_or_default();

    // The template prints strings and named fields: no `.len()` inside a printed number, and no
    // local count that is printed later.
    let template = read("templates/audit_report.typ");
    assert!(!template.is_empty(), "the template is read");
    let offenders: Vec<String> = template
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let code = l.trim_start();
            !code.starts_with("//")
                && l.contains(".len()")
                && (l.contains("str(") || l.contains("let n = "))
        })
        .map(|(i, l)| format!("line {}: {}", i + 1, l.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "the template counts by itself:\n{}",
        offenders.join("\n")
    );

    // The sentence builders take the counts object, never loose integers.
    let prose = read("src/report_export/counts_prose.rs");
    assert!(!prose.is_empty(), "counts_prose.rs exists");
    for builder in [
        "narrative",
        "decision_sentence",
        "methodology_line",
        "next_steps",
        "cover_strip",
        "grid_accepted_line",
        "grid_left_out",
    ] {
        let head = format!("fn {builder}(");
        let at = prose
            .find(&head)
            .unwrap_or_else(|| panic!("counts_prose.rs has no `{builder}`"));
        let signature = &prose[at..prose[at..].find('{').map_or(prose.len(), |e| at + e)];
        assert!(
            signature.contains("&ReportCounts"),
            "`{builder}` takes the counts: {signature}"
        );
        for loose in ["usize", "u32", "u64", "i32", "i64"] {
            assert!(
                !signature.contains(loose),
                "`{builder}` takes a loose integer: {signature}"
            );
        }
    }
    // The old builders that took loose integers are gone.
    for (file, name) in [
        (
            "src/report_export/classification.rs",
            "fn default_narrative",
        ),
        ("src/report_export/marking.rs", "fn next_steps_note_for"),
        ("src/report_export/marking.rs", "fn next_steps_note"),
    ] {
        assert!(
            !read(file).contains(name),
            "{file} still has `{name}`, which counts on its own"
        );
    }
}

/// A tree sealed before the per-rule whole-file key holds one place for a file whatever the rules
/// that flag it. Loading it re-merges the places, so the cockpit and the export agree.
#[test]
fn an_old_stored_tree_has_its_whole_file_places_merged_again_on_load() {
    use crate::review_tree::{place_id_of, PlaceNode};
    let whole = |rule: &str| finding(rule, "src/whole.rs", 0, "medium");
    let report = report_with(vec![whole("SYN-A-1"), whole("SYN-B-1")], vec![]);
    let sealed = report.seal();
    let keys: Vec<String> = sealed.findings.iter().map(|f| f.site_key.clone()).collect();
    assert!(
        keys.iter().all(|k| !k.is_empty()),
        "anti-vacuity: site keys"
    );
    assert!(
        sealed.tree.as_ref().is_some_and(|t| t.places.is_empty()),
        "anti-vacuity: today's seal stores no place for two single-row places"
    );
    // The tree as an older engine stored it: one place for the file whatever the rules.
    let stored = sealed.overlaid(|r| {
        r.tree.as_mut().expect("the tree").places = vec![PlaceNode {
            id: place_id_of("owner/repo", "src/whole.rs", 0),
            repo: "owner/repo".to_string(),
            path: "src/whole.rs".to_string(),
            line: 0,
            carrier: keys[0].clone(),
            members: keys.clone(),
            rules: vec!["SYN-A-1".to_string(), "SYN-B-1".to_string()],
            fix_ids: Vec::new(),
        }];
    });
    assert_eq!(stored.tree.as_ref().map(|t| t.places.len()), Some(1));
    let reviewed = ReviewedReport::unreviewed(&stored, &HashMap::new());
    let tree = reviewed.report().tree.as_ref().expect("the tree");
    assert!(
        tree.places.is_empty(),
        "two whole-file problems of two rules are two places of one row each, none stored: {:?}",
        tree.places
    );
    let counts = report_counts(&reviewed, None, &empty_opts());
    assert_eq!(counts.problems.total, 2, "the export counts two places too");
    assert_eq!(counts.rows.folded, 0);
}
