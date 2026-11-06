//! Pins the order of the KEV enrichment relative to the corpus severity bounds at every scan
//! call site.
//!
//! `vuln_intel::enrich_dependency_findings` floors a known-exploited dependency finding to
//! `critical` and writes "Severity raised from X to critical" into its detail.
//! `severity_bounds::apply_scan_severity_bounds` runs afterwards and has the last word on
//! severity. The sentence is true only because that order holds and no `max_severity` ceiling
//! governs dependency findings (the behavioural half is pinned in `vuln_intel/tests.rs`). This
//! test pins the order itself in each of the three scan paths, so a refactor that moves the
//! enrichment to after the bounds, or drops the bounds from a path, fails here rather than
//! shipping a report whose text disagrees with its severity.

const ENRICH_CALL: &str = "vuln_intel::enrich_dependency_findings(";
const BOUNDS_CALL: &str = "severity_bounds::apply_scan_severity_bounds(";

/// The call sites in `source` (comment lines excluded) as `(byte offset, is_enrichment)`, in
/// source order.
fn call_sites(source: &str) -> Vec<(usize, bool)> {
    let mut sites = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            if trimmed.contains(ENRICH_CALL) {
                sites.push((offset, true));
            }
            if trimmed.contains(BOUNDS_CALL) {
                sites.push((offset, false));
            }
        }
        offset += line.len();
    }
    sites
}

/// Every enrichment call is followed, before any other enrichment call, by a bounds call.
fn assert_each_enrichment_precedes_its_bounds(name: &str, source: &str, enrichments: usize) {
    let sites = call_sites(source);
    let found = sites.iter().filter(|(_, enrich)| *enrich).count();
    assert_eq!(
        found, enrichments,
        "{name}: expected {enrichments} enrichment call site(s), found {found}"
    );
    for (i, (_, is_enrichment)) in sites.iter().enumerate() {
        if !is_enrichment {
            continue;
        }
        let next = sites.get(i + 1);
        assert!(
            matches!(next, Some((_, false))),
            "{name}: an enrichment call must be followed by `apply_scan_severity_bounds` before \
             anything else enriches, so the corpus bounds get the last word on severity"
        );
    }
}

#[test]
fn the_desktop_scan_paths_enrich_before_the_corpus_bounds() {
    // `onboard_audit` and `onboard_audit_start`.
    assert_each_enrichment_precedes_its_bounds(
        "server/src/lib.rs",
        include_str!("../src/lib.rs"),
        2,
    );
}

#[test]
fn the_cli_inspect_path_enriches_before_the_corpus_bounds() {
    assert_each_enrichment_precedes_its_bounds(
        "cli/src/inspect_cmd.rs",
        include_str!("../../cli/src/inspect_cmd.rs"),
        1,
    );
}
