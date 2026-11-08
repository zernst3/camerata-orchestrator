//! WO-B merge, C4 and the waiting column. The client README names the workbook's sheets from the
//! vocabulary, so a renamed sheet renames the README too; and the workbook's "Why it is waiting"
//! column reads the row's own `why_waiting` reason, not a generic sentence.

use super::support::*;
use crate::release_gate::{xlsx_sheet_grid, xlsx_sheet_names};
use crate::report_export::apply_overlay;
use crate::xlsx_export::tickets::TICKET_HEADERS;
use crate::xlsx_export::vocabulary as v;
use crate::{product_export_readme, ManifestProvenance, ProvenanceRepo};

/// The README text with every run of whitespace collapsed, so a name wrapped over two lines still
/// reads as one name.
fn flat(readme: &str) -> String {
    readme.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn provenance_of(repos: &[&str]) -> ManifestProvenance {
    ManifestProvenance {
        repos: repos
            .iter()
            .map(|name| ProvenanceRepo {
                name: (*name).to_string(),
                ..ProvenanceRepo::default()
            })
            .collect(),
        ..ManifestProvenance::default()
    }
}

/// The README of a client package for a project of one repo and of two.
fn client_readmes() -> [String; 2] {
    [
        product_export_readme("acme-inspection", &provenance_of(&["acme/portal"]), false),
        product_export_readme(
            "acme-inspection",
            &provenance_of(&["acme/portal", "acme/billing"]),
            false,
        ),
    ]
}

/// The ticket file names its columns in the vocabulary's words: a place is a place there too.
#[test]
fn the_ticket_columns_use_the_vocabulary_terms() {
    assert_eq!(
        TICKET_HEADERS,
        [
            "Title",
            "Description",
            "Priority",
            "Labels",
            "Estimate",
            // Per group until the plan is per Fix; then this becomes the Fix id.
            "Group id",
            v::H_PLACE_COUNT,
            v::H_PLACES,
        ]
    );
    for header in TICKET_HEADERS {
        assert_eq!(v::banned_synonym_in(header), None, "{header:?}");
    }
}

#[test]
fn the_client_readme_names_only_sheets_that_exist() {
    let reviewed = apply_overlay(&mixed_sealed(), &[], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("the workbook");
    let tabs = xlsx_sheet_names(&xlsx).unwrap();
    for readme in client_readmes() {
        let text = flat(&readme);
        // Every "<name> sheet" in the README is a sheet of the vocabulary.
        let mut from = 0;
        while let Some(at) = text[from..].find(" sheet") {
            let end = from + at;
            let head = &text[..end];
            // " sheets" is not the end of a name.
            if text[end + 6..].starts_with('s') {
                from = end + 6;
                continue;
            }
            assert!(
                v::SHEETS.iter().any(|s| head.ends_with(s.name)),
                "the README says a sheet that is not in the vocabulary: ...{}",
                &text[end.saturating_sub(40)..end + 6]
            );
            from = end + 6;
        }
        // The old names are gone.
        for old in [
            "Remediation plan",
            "Sites sheet",
            "Tree sheet",
            "Coverage",
            "False Positives",
            "All Findings",
            "Decisions sheet",
            "sheet for each area",
        ] {
            assert!(
                !text.contains(old),
                "the README still says {old:?}:\n{readme}"
            );
        }
        // The sheets it names for a single-repo project are tabs of the workbook.
        for s in v::SHEETS
            .iter()
            .filter(|s| s.name != v::PATTERNS_ACROSS_SERVICES)
        {
            if text.contains(s.name) {
                assert!(
                    tabs.iter().any(|t| t == s.name),
                    "the README names {:?}, which the workbook does not have: {tabs:?}",
                    s.name
                );
            }
        }
    }
    // The README says what the workbook's first sheets are.
    let single = flat(&client_readmes()[0]);
    for name in [
        v::START_HERE,
        v::FIX_PLAN,
        v::ALL_PROBLEMS,
        v::RULES_CHECKED,
        v::RULED_OUT,
        v::REVIEWER_DECISIONS,
    ] {
        assert!(single.contains(name), "the README never names {name:?}");
    }
}

#[test]
fn the_client_readme_uses_no_retired_synonym_and_no_banned_character() {
    for readme in client_readmes() {
        let text = readme.clone();
        assert_eq!(
            v::banned_synonym_in(&text),
            None,
            "the README uses a retired word:\n{readme}"
        );
        for c in ['\u{2014}', '\u{2013}', '\u{00D7}'] {
            assert!(!readme.contains(c), "the README has {c:?}:\n{readme}");
        }
    }
}

#[test]
fn the_tickets_paragraph_points_at_the_all_problems_sheet_and_the_fix_id() {
    let readme = flat(&client_readmes()[0]);
    let paragraph = readme
        .split("csv/tickets.csv")
        .nth(1)
        .expect("the README describes the tickets file");
    assert!(
        paragraph.contains(&format!("{} sheet", v::ALL_PROBLEMS)),
        "{paragraph}"
    );
    assert!(paragraph.contains(v::H_FIX_ID), "{paragraph}");
    // Every column the file has is named, as the file names it.
    for header in TICKET_HEADERS {
        assert!(
            paragraph.contains(header),
            "{header:?} missing: {paragraph}"
        );
    }
}

#[test]
fn the_release_gate_inserts_its_no_tree_sentence_after_the_readmes_tickets_paragraph() {
    let readme = product_export_readme("acme-inspection", &provenance_of(&["acme/portal"]), false);
    let end = crate::tickets_paragraph_end();
    assert!(
        readme.contains(&end),
        "the gate looks for {end:?} and the README does not end its tickets paragraph there:\n{readme}"
    );
}

// ── The waiting column ──────────────────────────────────────────────────────────────────────

#[test]
fn the_why_it_is_waiting_column_reads_the_rows_own_reason() {
    let mut report = crate::release_gate::test_scan_report();
    let mut uncited = sql_row("src/model.rs", 5);
    uncited.rule_id = "AI-CUSTOM-ARCH-RULE-1".to_string();
    uncited.severity = "medium".to_string();
    report.findings = vec![sql_row("src/a.rs", 10), uncited];
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let options = client_options();
    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &options).unwrap();
    let grid = xlsx_sheet_grid(&xlsx, v::NEEDS_A_DECISION).unwrap();
    let col = grid[0]
        .iter()
        .position(|h| h == v::H_WHY_IT_IS_WAITING)
        .expect("the column");
    let reasons: Vec<&str> = grid
        .iter()
        .skip(1)
        .map(|r| r.get(col).map_or("", String::as_str))
        .filter(|r| !r.is_empty())
        .collect();
    assert_eq!(reasons.len(), 1, "{grid:?}");
    assert_eq!(
        reasons[0],
        "No outside standard supports this problem yet, so a reviewer must rule on it before you \
         act.",
        "the sheet says a generic sentence, not the row's own reason"
    );
}
