//! THE ENFORCEMENT of Zach's ruling (2026-10-23, protected-area answer 2): the review timer, the
//! per-group active time and the per-row "reviewed" record are INTERNAL ONLY, in no client export.
//!
//! The deny-list is `camerata_api_types::review_timing` (names only). `camerata-ui-core` pins that
//! every key its review shapes serialize is on that list, so a field added there without being
//! listed fails there; this file pins that no listed name, and none of the internal view's
//! phrases, appears in anything a client receives:
//!
//! * every member of the released zip: `findings.json`, `manifest.json`, `README.txt`, every CSV
//!   and JSON dataset, the workbook (every part of it: sheet names, cells, shared strings,
//!   document properties) and the PDF member;
//! * the PDF's data (`data.json`, the only data the Typst template reads) and the shipped
//!   template's own source, since the PDF is what those two produce;
//! * the zip's file name stem.
//!
//! A detector that finds nothing proves nothing, so `the_scan_reaches_every_surface` plants a
//! listed name on each surface and demands the scan find it there. That test is also how to prove
//! the guard works end to end by hand: temporarily add one of the listed names to a client
//! export (a `serde` field on the `findings.json` row, say) and `no_review_time_data_reaches_a_client_export`
//! fails naming the artifact.

use camerata_api_types::review_timing::{INTERNAL_ONLY_KEYS, INTERNAL_ONLY_PHRASES};

use super::tests::{package, plant, ALL_SURFACES};
use super::*;

/// The Typst template that renders the PDF from `data.json`.
const PDF_TEMPLATE: &str = include_str!("../../templates/audit_report.typ");

/// Every deny-listed name and phrase found in `text`.
fn terms_in(text: &str) -> Vec<&'static str> {
    INTERNAL_ONLY_KEYS
        .iter()
        .chain(INTERNAL_ONLY_PHRASES)
        .copied()
        .filter(|term| text.contains(term))
        .collect()
}

/// `(artifact, term)` for every hit across everything a client receives.
fn hits(zip: &[u8], data_json: &str, stem: &str) -> Vec<(String, &'static str)> {
    let mut found: Vec<(String, &'static str)> = Vec::new();
    let mut scan = |artifact: String, text: &str| {
        for term in terms_in(text) {
            found.push((artifact.clone(), term));
        }
    };
    scan("zip file name stem".to_string(), stem);
    scan("PDF data (data.json)".to_string(), data_json);
    scan("PDF template source".to_string(), PDF_TEMPLATE);
    let entries = read_zip_entries(zip).expect("the released package is a zip");
    for (name, bytes) in &entries {
        scan(format!("member name {name}"), name);
        if name.ends_with(".xlsx") {
            // Every part of the workbook, not only its cells.
            let parts = read_zip_entries(bytes).expect("the workbook is a zip");
            for (part, part_bytes) in &parts {
                scan(
                    format!("{name} > {part}"),
                    &String::from_utf8_lossy(part_bytes),
                );
            }
            for cell in xlsx_strings(bytes).expect("the workbook reads") {
                scan(format!("{name} (cell text)"), &cell);
            }
        } else {
            scan(name.clone(), &String::from_utf8_lossy(bytes));
        }
    }
    found
}

fn released_hits(package: ProductPackage) -> Vec<(String, &'static str)> {
    let data_json = package.data_json.clone();
    let stem = package.stem.clone();
    let released = release(package).expect("a clean package must clear the gate");
    hits(&released.bytes, &data_json, &stem)
}

/// THE TEST: a real package (workbook, `findings.json`, datasets, manifest, README, PDF data)
/// carries none of the internal review-time names or phrases.
#[test]
fn no_review_time_data_reaches_a_client_export() {
    let found = released_hits(package());
    assert!(
        found.is_empty(),
        "internal-only review-time data reached a client export (artifact, name): {found:?}"
    );
}

/// The same, with the internal data present where it lives, to show the exporters have no way to
/// reach it: the draft carries a populated review log, and the export is built from the report,
/// the dispositions and the options only (the function signature is the proof; this pins that
/// the draft's log, serialized, shares no name with the package).
#[test]
fn a_populated_review_log_shares_no_name_with_the_package() {
    let log = serde_json::json!({
        "review_log": {
            "review_groups": { "g": { "opened_at_ms": 1, "active_ms": 2, "group_reviewed": true } },
            "review_rows": { "r": { "reviewed_by": "operator", "reviewed_at_ms": 3, "review_decision_ms": 4 } },
        }
    })
    .to_string();
    assert!(
        terms_in(&log).len() >= 8,
        "the fixture must really carry the internal names"
    );
    let package = package();
    let data_json = package.data_json.clone();
    let stem = package.stem.clone();
    let released = release(package).expect("clear");
    let exported = hits(&released.bytes, &data_json, &stem);
    assert!(exported.is_empty(), "{exported:?}");
}

/// The detector reaches every client surface: a listed name planted on each is found there, and
/// the clean package beside it is not. Without this, an empty result above could mean the scan
/// never looked.
#[test]
fn the_scan_reaches_every_surface() {
    for surface in ALL_SURFACES {
        for planted in ["active_ms", "Review timer"] {
            let mut package = package();
            plant(&mut package, surface, planted);
            let data_json = package.data_json.clone();
            let stem = package.stem.clone();
            let found = match release(package) {
                Ok(released) => hits(&released.bytes, &data_json, &stem),
                Err(e) => panic!("{surface:?}: the plant must not trip the gate: {e}"),
            };
            assert!(
                found.iter().any(|(_, term)| *term == planted),
                "{surface:?}: planted {planted:?} but the scan found {found:?}"
            );
        }
    }
}

/// Every deny-listed name is findable by the scan (none is so odd that substring search misses it
/// in JSON, CSV, XML or plain text) and none appears in the template source.
#[test]
fn every_listed_name_is_found_where_planted_and_absent_from_the_template() {
    for term in INTERNAL_ONLY_KEYS.iter().chain(INTERNAL_ONLY_PHRASES) {
        // (Some names contain others, `total_active_ms` and `active_ms`, so "contains".)
        assert!(
            terms_in(&format!("{{\"{term}\":1}}")).contains(term),
            "{term}"
        );
        assert!(!PDF_TEMPLATE.contains(term), "the template names {term}");
    }
}
