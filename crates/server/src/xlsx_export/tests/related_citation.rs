//! H5 item 1: a citation attached to an AI-tier finding by `classify_ai_finding`'s KEYWORD
//! match is a "related standard", never presented as a rule Camerata checked. These tests pin
//! both directions across every artifact (the report JSON the PDF is compiled from, the xlsx
//! workbook and `findings.json`): a keyword-matched finding carries the "related" label, and a
//! finding whose OWN rule was checked keeps its plain citation.

use super::*;
use crate::report_export::{citation_for_finding, RELATED_STANDARD_LABEL_PREFIX};

async fn real_corpus() -> camerata_rules::RuleSet {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    corpus
}

/// An AI-tier finding with an invented rule id that only classifies into the RLS class by
/// keyword (`category` + detail text). It has no corpus entry of its own.
fn keyword_matched_ai_finding() -> Finding {
    let mut f = finding(
        "AI-RLS-GAP-CUSTOM-7",
        "supabase/functions/orders/index.ts",
        42,
        "critical",
    );
    f.category = Some("rls-policy".to_string());
    f.confidence = Some("high".to_string());
    f.detail = "Cross-tenant row level security gap: the orders table has no RLS policy, \
                so any authenticated user can read every tenant's rows."
        .to_string();
    f
}

/// `findings.json`'s one row, as the wire JSON a consumer reads (serialized, not the
/// in-memory struct, so a `#[serde(skip)]` field can never make this test pass by accident).
fn only_findings_json_row(
    report: &ScanReport,
    corpus: &camerata_rules::RuleSet,
) -> serde_json::Value {
    let json = crate::report_export::build_report_json(
        &report.clone().seal(),
        &HashMap::new(),
        Some(corpus),
        &empty_opts(),
    );
    let export = build_findings_export(
        &report.clone().seal(),
        &HashMap::new(),
        Some(corpus),
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).expect("findings.json must serialize");
    let rows = value["findings"]
        .as_array()
        .expect("findings array")
        .clone();
    assert_eq!(rows.len(), 1, "fixture has exactly one finding");
    rows[0].clone()
}

#[tokio::test]
async fn keyword_matched_ai_finding_cites_a_related_standard_in_every_artifact() {
    let corpus = real_corpus().await;
    let f = keyword_matched_ai_finding();
    let grounding =
        crate::report_export::resolve_citation("SUPABASE-RLS-ENABLED-1", None, Some(&corpus));
    assert_eq!(
        grounding.kind, "grounded",
        "fixture assumes the grounding rule is cited"
    );

    // The citation itself: same real sources, but labelled as a related standard.
    let citation = citation_for_finding(&f, Some(&corpus));
    assert_eq!(
        citation.kind, "related",
        "a keyword-matched citation must not claim the rule was checked"
    );
    assert!(
        citation.label.starts_with(RELATED_STANDARD_LABEL_PREFIX),
        "label must say related standard, got {:?}",
        citation.label
    );
    assert!(citation.label.ends_with(&grounding.label));
    assert_eq!(citation.sources.len(), grounding.sources.len());

    // Report JSON (the PDF's data): still curated, carrying the related citation.
    let report = report_with(vec![f], vec![]);
    let json = crate::report_export::build_report_json(
        &report.clone().seal(),
        &HashMap::new(),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings.len(),
        1,
        "labelling the citation honestly must not drop the finding from the curated set"
    );
    let group = &json.curated_findings[0];
    assert_eq!(group.citation.kind, "related");
    assert_eq!(group.sites[0].citation.kind, "related");

    // findings.json: the label says related standard and provenance stays AI-advisory (it
    // used to read "Deterministic" because the borrowed citation was "grounded").
    let row = only_findings_json_row(&report, &corpus);
    let label = row["citation_label"].as_str().expect("citation_label");
    assert!(
        label.starts_with(RELATED_STANDARD_LABEL_PREFIX),
        "findings.json citation_label must say related standard, got {label:?}"
    );
    assert_eq!(row["provenance"], "AI-advisory");

    // xlsx: the Citation column (P) and the Provenance column carry the same honest words.
    let xlsx = build_workbook(
        &report.clone().seal(),
        &HashMap::new(),
        Some(&corpus),
        &empty_opts(),
    )
    .expect("workbook builds");
    let citation_cell = cell_text(&xlsx, "xl/worksheets/sheet2.xml", "P2");
    assert!(
        citation_cell.starts_with(RELATED_STANDARD_LABEL_PREFIX),
        "xlsx citation cell must say related standard, got {citation_cell:?}"
    );
    assert_eq!(
        cell_text(&xlsx, "xl/worksheets/sheet2.xml", "O2"),
        "AI-advisory",
        "a keyword-matched AI row's provenance must never read Deterministic"
    );
}

#[tokio::test]
async fn a_finding_whose_own_rule_was_checked_keeps_its_plain_citation() {
    let corpus = real_corpus().await;

    // A deterministic floor finding of a cited corpus rule.
    let deterministic = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.py", 1, "critical");
    // An AI-tier finding reported against a corpus rule the AI pass was asked to check (its
    // own rule id is a corpus id, so no keyword match is involved).
    let mut ai_checked = finding("SUPABASE-RLS-ENABLED-1", "b.sql", 2, "high");
    ai_checked.confidence = Some("high".to_string());

    for f in [deterministic.clone(), ai_checked] {
        let citation = citation_for_finding(&f, Some(&corpus));
        assert_eq!(
            citation.kind, "grounded",
            "{}: a rule that was checked keeps its grounded citation",
            f.rule_id
        );
        assert!(
            !citation.label.starts_with(RELATED_STANDARD_LABEL_PREFIX),
            "{}: must not be labelled a related standard, got {:?}",
            f.rule_id,
            citation.label
        );
    }

    let report = report_with(vec![deterministic], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let row = only_findings_json_row(&report, &corpus);
    let label = row["citation_label"].as_str().expect("citation_label");
    assert!(!label.starts_with(RELATED_STANDARD_LABEL_PREFIX));
    assert_eq!(row["provenance"], "Deterministic");
}

#[test]
fn the_report_explains_related_standards_in_one_plain_sentence() {
    let report = report_with(
        vec![finding("SEC-CRITICAL-1", "a.rs", 1, "critical")],
        vec![],
    );
    let json = crate::report_export::build_report_json(
        &report.seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let note = &json.methodology.citation_note;
    assert!(
        note.to_lowercase().contains("related standard"),
        "got {note:?}"
    );
    assert_eq!(note.matches(". ").count(), 0, "one sentence, got {note:?}");
    assert!(note.ends_with('.'));
    assert!(
        !note.contains('\u{2014}'),
        "no em dashes in buyer-facing text"
    );

    // The PDF template renders both the note and the related heading over the sources.
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains("d.methodology.citation_note"));
    assert!(template.contains("group.citation.kind == \"related\""));
}

#[test]
fn the_index_sheet_defines_related_standard() {
    let report = report_with(
        vec![finding("SEC-CRITICAL-1", "a.rs", 1, "critical")],
        vec![],
    );
    let xlsx = build_workbook(&report.seal(), &HashMap::new(), None, &empty_opts())
        .expect("workbook builds");
    let shared = read_zip_entry(&xlsx, "xl/sharedStrings.xml");
    assert!(
        shared.contains("Related standard"),
        "the Index sheet's vocabulary must define a related standard"
    );
}

#[test]
fn the_index_sheet_provenance_lines_are_accurate_and_free_of_em_dashes() {
    let report = report_with(
        vec![finding("SEC-CRITICAL-1", "a.rs", 1, "critical")],
        vec![],
    );
    let xlsx = build_workbook(&report.seal(), &HashMap::new(), None, &empty_opts())
        .expect("workbook builds");
    let shared = read_zip_entry(&xlsx, "xl/sharedStrings.xml");
    // A related row now carries sources, so "no corpus citation" would be false.
    assert!(
        !shared.contains("no corpus citation"),
        "AI-advisory must not be defined as having no citation"
    );
    assert!(
        shared.contains("model-inferred; may cite a related standard"),
        "AI-advisory is model-inferred and may cite a related standard"
    );
    for line in [
        "Deterministic: corpus-grounded",
        "Preview: {tool}: scan-time deterministic tool pass",
    ] {
        assert!(shared.contains(line), "missing Index line {line:?}");
    }
    // The vocabulary lines are client-facing: no em dash in them.
    for entry in shared.split("<si>").filter(|s| {
        s.contains("corpus-grounded")
            || s.contains("scan-time deterministic tool pass")
            || s.contains("model-inferred")
    }) {
        assert!(
            !entry.contains('\u{2014}'),
            "em dash in Index line: {entry}"
        );
    }
}
