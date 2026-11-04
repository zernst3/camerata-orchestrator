//! Render tests: kind sort group header.

use super::*;

// ── Pass 2 step 1: the Kind column + the three informational panels ──────
// the pass-2 curator UI design notes §11 step 1.

pub(super) fn kinded_finding(
    rule_id: &str,
    severity: &str,
    kind: serde_json::Value,
) -> FindingView {
    let mut row = serde_json::json!({
        "repo": "me/api", "path": "src/x.rs", "line": 1, "rule_id": rule_id,
        "severity": severity, "snippet": "s", "detail": "d", "status": "active"
    });
    for (k, v) in kind.as_object().expect("kind payload is an object") {
        row[k] = v.clone();
    }
    serde_json::from_value(row).expect("valid FindingView fixture")
}

/// req_pass2_5: the Kind column exists in the findings table, carries the WIRE tag as its
/// cell value (so the filter vocabulary is the wire's, never re-derived prose), and its
/// filter offers only the three tags that can actually appear in the Queue — the
/// informational kinds route to their own panels, so listing them would be dead.
#[test]
fn req_pass2_5_findings_table_has_a_kind_column_carrying_the_wire_tag() {
    let cols = finding_columns(vec!["me/api".to_string()], false);
    let kind_col = cols
        .iter()
        .find(|c| c.id == ColumnId("kind"))
        .expect("the findings table must carry a Kind column");
    assert_eq!(kind_col.header, "Kind");

    let defect = kinded_finding(
        "SQL-INJECTION-1",
        "critical",
        serde_json::json!({"kind": "defect"}),
    );
    let deviation = kinded_finding(
        "API-VERSIONING-1",
        "medium",
        serde_json::json!({
            "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
            "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
        }),
    );
    let unmapped = kinded_finding("LEGACY-1", "high", serde_json::json!({}));
    assert_eq!(
        (kind_col.accessor)(&defect),
        CellValue::Text("defect".to_string())
    );
    assert_eq!(
        (kind_col.accessor)(&deviation),
        CellValue::Text("deviation".to_string())
    );
    assert_eq!(
        (kind_col.accessor)(&unmapped),
        CellValue::Text("unspecified".to_string()),
        "a kind-less row must read as its own explicit alarm value, never as a defect"
    );
}

/// req_pass2_4: the one-rule-one-surface invariant is SILENT when healthy and LOUD when a
/// rule reaches two surfaces — the UI surfaces the pipeline bug rather than picking a
/// winner and hiding it.
#[test]
fn req_pass2_4_conflict_banner_is_silent_when_healthy_and_loud_when_violated() {
    fn healthy() -> Element {
        rsx! {
            KindSurfaceConflictBanner {
                findings: vec![
                    kinded_finding("D-1", "high", serde_json::json!({ "kind": "defect" })),
                    kinded_finding(
                        "C-1",
                        "info",
                        serde_json::json!({
                            "kind": "convention_observed",
                            "convention_option_id": "opt",
                            "convention_label": "A convention",
                        }),
                    ),
                ],
            }
        }
    }
    let mut vdom = VirtualDom::new(healthy);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("kind-conflict-banner"),
        "a healthy scan must not ring the alarm; html=\n{html}"
    );

    fn violated() -> Element {
        rsx! {
            KindSurfaceConflictBanner {
                findings: vec![
                    kinded_finding(
                        "API-VERSIONING-1",
                        "info",
                        serde_json::json!({
                            "kind": "convention_observed",
                            "convention_option_id": "opt",
                            "convention_label": "URI-versioned endpoints",
                        }),
                    ),
                    kinded_finding(
                        "API-VERSIONING-1",
                        "medium",
                        serde_json::json!({
                            "kind": "deviation",
                            "deviating_sites": 2,
                            "assessed_sites": 47,
                            "convention_option_id": "opt",
                            "convention_label": "URI-versioned endpoints",
                        }),
                    ),
                ],
            }
        }
    }
    let mut vdom = VirtualDom::new(violated);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("kind-conflict-banner"),
        "alarm rendered; html=\n{html}"
    );
    assert!(
        html.contains("Pipeline bug"),
        "named as upstream; html=\n{html}"
    );
    assert!(
        html.contains("API-VERSIONING-1"),
        "the offending rule named; html=\n{html}"
    );
    assert!(
        html.contains("Queue"),
        "and the surfaces it reached; html=\n{html}"
    );
    assert!(
        html.contains("Conventions"),
        "and the surfaces it reached; html=\n{html}"
    );
}

/// req_pass2_6: the Conventions panel lists each observed convention by its AUTHORED label
/// and renders NO severity — these kinds ship at `info` by construction, so a severity
/// badge would imply a ranking decision that does not exist (design §9.4).
#[test]
fn req_pass2_6_conventions_panel_lists_labels_and_renders_no_severity() {
    fn harness() -> Element {
        rsx! {
            ConventionsPanel {
                findings: vec![kinded_finding(
                    "API-VERSIONING-1",
                    "info",
                    serde_json::json!({
                        "kind": "convention_observed",
                        "convention_option_id": "versioned",
                        "convention_label": "URI-versioned endpoints",
                    }),
                )],
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("API-VERSIONING-1"), "rule id; html=\n{html}");
    assert!(
        html.contains("URI-versioned endpoints"),
        "authored label; html=\n{html}"
    );
    for forbidden in ["chorale-badge", "severity", "info", "critical", "Critical"] {
        assert!(
            !html.contains(forbidden),
            "no severity may render on an informational panel (found {forbidden:?}); html=\n{html}"
        );
    }
}

/// req_pass2_6: the No-convention panel enumerates the options actually observed with
/// their site counts, and renders an uncounted option HONESTLY (label alone) rather than
/// fabricating a number.
#[test]
fn req_pass2_6_no_convention_panel_lists_observed_options_and_never_fabricates_counts() {
    fn harness() -> Element {
        rsx! {
            NoConventionPanel {
                findings: vec![kinded_finding(
                    "ERROR-HANDLING-1",
                    "info",
                    serde_json::json!({
                        "kind": "no_convention",
                        "observed_options": [
                            { "option_id": "result", "label": "Result types", "sites": 9 },
                            { "option_id": "panic", "label": "Panics", "sites": null },
                        ],
                    }),
                )],
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("ERROR-HANDLING-1"), "rule id; html=\n{html}");
    assert!(
        html.contains("Result types (9 sites)"),
        "counted option; html=\n{html}"
    );
    assert!(
        html.contains("Panics"),
        "uncounted option still listed; html=\n{html}"
    );
    assert!(
        !html.contains("Panics (0 sites)") && !html.contains("Panics ("),
        "an uncounted option must never acquire a fabricated count; html=\n{html}"
    );
}

/// req_pass2_6: the Not-assessed panel groups by the STRUCTURED reason vocabulary, shows
/// the producer's detail, and offers NO buttons — there is no per-rule re-run mechanism in
/// the product, and a disabled one would be the 403-button bug.
#[test]
fn req_pass2_6_not_assessed_panel_groups_by_reason_and_offers_no_actions() {
    fn harness() -> Element {
        rsx! {
            NotAssessedPanel {
                findings: vec![
                    kinded_finding(
                        "ESLINT-1",
                        "info",
                        serde_json::json!({
                            "kind": "not_assessed",
                            "reason": "tool_failed",
                            "reason_detail": "eslint exited 2",
                        }),
                    ),
                    kinded_finding(
                        "TSX-1",
                        "info",
                        serde_json::json!({
                            "kind": "not_assessed",
                            "reason": "no_files_matched",
                            "reason_detail": "no .tsx files in scan scope",
                        }),
                    ),
                ],
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("tool_failed"),
        "structured reason group; html=\n{html}"
    );
    assert!(
        html.contains("no_files_matched"),
        "structured reason group; html=\n{html}"
    );
    assert!(
        html.contains("eslint exited 2"),
        "producer detail; html=\n{html}"
    );
    // EDITED for design §8 (exception (a)): this assertion was `!html.contains("<button")`.
    //
    // Design §3 says "NotAssessed gets no buttons … A disabled 're-run' would be exactly
    // the 403-button bug", and design §8 says the rule hub is reachable "from every
    // surface that shows a rule id (… Not assessed list …)". Both are the design's own
    // words and they collide only on the literal element name. What §3 forbids is a
    // DECISION the curator cannot make; the hub link is navigation, and it is the one
    // affordance §8 requires here.
    //
    // So the assertion now says what it always meant, and says it more strictly than the
    // string it replaces: every control on this panel is a hub door, exactly one per row,
    // and none of them is an action. Rendering the door as a `<span onclick>` to keep the
    // old string green would have been dodging the test rather than answering it — and
    // would have cost the keyboard the control, which a `<button>` gives for free.
    let buttons = html.matches("<button").count();
    assert_eq!(
        buttons,
        html.matches("rule-hub-link").count(),
        "every control on the Not assessed panel must be a rule-hub door; html=\n{html}"
    );
    assert_eq!(
        buttons, 2,
        "one door per listed rule, and nothing else; html=\n{html}"
    );
    for forbidden in [
        "btn-run",
        "btn-restart",
        "btn-edit-sm",
        "Re-run",
        "Adopt",
        "Skip",
    ] {
        assert!(
            !html.contains(forbidden),
            "NotAssessed needs no decision, so it must offer no {forbidden} affordance; \
                 html=\n{html}"
        );
    }
}

/// req_pass2_7 (END-TO-END through the real table): the live `FindingsTable` carries the
/// Kind column, DROPS an informational row entirely, and lays the remaining rules out in
/// the curated default order. This exercises the path the curator actually sees —
/// `TriageModel::visible`'s kind filter, `queue_sort_key`, and the real column set —
/// rather than the pieces in isolation.
#[test]
fn req_pass2_7_live_findings_table_drops_informational_rows_and_keeps_the_curated_order() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: vec![
                    kinded_finding(
                        "SQL-INJECTION-1",
                        "critical",
                        serde_json::json!({ "kind": "defect" }),
                    ),
                    kinded_finding(
                        "API-VERSIONING-1",
                        "medium",
                        serde_json::json!({
                            "kind": "deviation",
                            "deviating_sites": 2,
                            "assessed_sites": 47,
                            "convention_option_id": "versioned",
                            "convention_label": "URI-versioned endpoints",
                        }),
                    ),
                    kinded_finding("LEGACY-UNMAPPED-1", "high", serde_json::json!({})),
                    kinded_finding(
                        "ALREADY-GOOD-1",
                        "info",
                        serde_json::json!({
                            "kind": "convention_observed",
                            "convention_option_id": "versioned",
                            "convention_label": "URI-versioned endpoints",
                        }),
                    ),
                ],
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);

    // The table groups by rule and mounts COLLAPSED, so the SSR snapshot shows the Kind
    // column header and one group header per rule, not the individual cells. That is
    // enough to prove the three things this test exists for; the chip renderer itself is
    // covered by the column-accessor test above.
    assert!(
        html.contains(">Kind<"),
        "the Kind column header; html=\n{html}"
    );
    assert!(
        !html.contains("ALREADY-GOOD-1"),
        "an informational row must never reach the accusing queue; html=\n{html}"
    );

    // And the curated default sort survives into the real table: critical defect, then
    // the kind-less ALARM row (high), then the medium deviation. If the UI re-derived
    // severity or ranked the alarm as a plain defect, this order would change.
    //
    // Scoped to the GROUP HEADERS rather than the whole document (Pass 2 step 2): the
    // rule-level decision bar now also names each deviation rule, above the table, so a
    // first-occurrence search over the full html would be measuring the bar's position
    // instead of the group order. Narrowing it makes the assertion say what it always
    // meant.
    let table = group_header_region(&html);
    let at = |needle: &str| {
        table
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing from the queue; html=\n{html}"))
    };
    assert!(
        at("SQL-INJECTION-1") < at("LEGACY-UNMAPPED-1"),
        "critical defect before the high alarm row; html=\n{html}"
    );
    assert!(
        at("LEGACY-UNMAPPED-1") < at("API-VERSIONING-1"),
        "high alarm row before the medium deviation; html=\n{html}"
    );
}

/// The "Patterns across services" tab is read-only: each service's cell as the server stated
/// it, "not applicable" and "not reviewed" printed as themselves, the fixed sentence for the
/// row, no severity and no control.
#[test]
fn the_patterns_panel_prints_each_cell_as_stated_and_is_read_only() {
    fn harness() -> Element {
        let view: camerata_ui_core::patterns::PatternsView =
            serde_json::from_value(serde_json::json!({
                "repos": ["acme/web", "acme/api", "acme/worker", "acme/jobs"],
                "rows": [{
                    "rule_id": "ARCH-PAGINATION-1",
                    "cells": [
                        {"repo": "acme/web", "state": "violated", "sites": 2},
                        {"repo": "acme/api", "state": "clean"},
                        {"repo": "acme/worker", "state": "not_applicable", "reason": "no files"},
                        {"repo": "acme/jobs", "state": "not_reviewed", "reason": "spend limit"}
                    ]
                }],
                "themes": []
            }))
            .unwrap();
        rsx! { PatternsPanel { view, titles: None } }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("These services handle ARCH-PAGINATION-1 differently."),
        "{html}"
    );
    for expected in [
        "Violated, 2 sites",
        "Clean",
        "Not applicable: no files",
        "Not reviewed: spend limit",
    ] {
        assert!(html.contains(expected), "{expected} missing from {html}");
    }
    assert!(!html.contains("<button"), "read-only: {html}");
    // The panel says in a sentence that it carries no severity; it renders none.
    assert!(
        !html.contains("sev-") && !html.contains("severity-chip"),
        "{html}"
    );
}

/// With the rule library loaded the tab prints the rule's title (as the report does) and drops
/// a row whose rule is not in the library, so the two agree.
#[test]
fn the_patterns_panel_prints_the_rule_title_and_drops_an_unknown_rule() {
    fn harness() -> Element {
        let view: camerata_ui_core::patterns::PatternsView =
            serde_json::from_value(serde_json::json!({
                "repos": ["acme/web", "acme/api"],
                "rows": [
                    {"rule_id": "ARCH-PAGINATION-1", "cells": [
                        {"repo": "acme/web", "state": "violated", "sites": 2},
                        {"repo": "acme/api", "state": "clean"}]},
                    {"rule_id": "GONE-1", "cells": [
                        {"repo": "acme/web", "state": "violated", "sites": 1},
                        {"repo": "acme/api", "state": "clean"}]}
                ],
                "themes": []
            }))
            .unwrap();
        let titles: std::collections::HashMap<String, String> = [(
            "ARCH-PAGINATION-1".to_string(),
            "Cursor pagination".to_string(),
        )]
        .into();
        rsx! { PatternsPanel { view, titles: Some(titles) } }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("These services handle Cursor pagination differently."),
        "{html}"
    );
    assert!(
        !html.contains("ARCH-PAGINATION-1"),
        "no raw rule id: {html}"
    );
    assert!(
        !html.contains("GONE-1"),
        "an unknown rule is dropped: {html}"
    );
}

/// An empty section states its case in a sentence, never a blank area.
#[test]
fn the_patterns_panel_states_an_empty_case() {
    fn harness() -> Element {
        rsx! { PatternsPanel { view: camerata_ui_core::patterns::PatternsView::default(), titles: None } }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("kind-panel-empty"), "{html}");
}

/// The report the server sends carries the section under `patterns_across_services`; a
/// report without it (one repo, or an older server) has none and shows no tab.
#[test]
fn scan_report_view_reads_the_patterns_section_from_the_wire() {
    let parse = |json: serde_json::Value| -> ScanReportView {
        serde_json::from_value(json).expect("valid ScanReportView fixture")
    };
    let with = parse(serde_json::json!({
        "repos": ["a/one", "a/two"], "files_scanned": 1,
        "findings": [], "proposed_rules": [], "gated": false,
        "patterns_across_services": {
            "repos": ["a/one", "a/two"],
            "rows": [{"rule_id": "R-1", "cells": [
                {"repo": "a/one", "state": "violated", "sites": 1},
                {"repo": "a/two", "state": "clean"}
            ]}]
        }
    }));
    assert!(with.patterns_across_services.is_present());
    assert_eq!(with.patterns_across_services.count(), 1);
    let without = parse(serde_json::json!({
        "repos": ["a/one"], "files_scanned": 1,
        "findings": [], "proposed_rules": [], "gated": false
    }));
    assert!(!without.patterns_across_services.is_present());
}

/// req_pass2_6: each panel's empty state is a sentence, not a blank area — the curator has
/// to be able to tell "nothing of this kind" from "this did not render".
#[test]
fn req_pass2_6_informational_panels_state_their_empty_case() {
    fn harness() -> Element {
        rsx! {
            ConventionsPanel { findings: vec![] }
            NoConventionPanel { findings: vec![] }
            NotAssessedPanel { findings: vec![] }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert_eq!(
        html.matches("kind-panel-empty").count(),
        3,
        "every panel states its own empty case; html=\n{html}"
    );
    // Key and click activity here is not review activity for the timer: only the Queue
    // table (with its strip and pane), the viewer and the stack are review surfaces.
    assert!(!html.contains("data-review-surface"), "{html}");
}

// ── Pass 2 step 2: MANUAL column sorting over the curated default ────────
// the pass-2 curator UI design notes §5: "the default sort is a curated
// order ... the column headers still allow any manual re-sort chorale supports — the
// default is opinionated, the table is not locked."
//
// Zach's ruling (2026-10-12): "Keep sort with [unspecified] first, sure, as long as the
// table can be manually sorted by me as the user." So the kind-aware key stays, demoted
// from the only order to the DEFAULT order, and a return path to it is mandatory.
//
// These tests drive chorale's REAL sort pipeline (`toggle_sort` / `clear_sort` /
// `filtered_sorted_rows` / `visible_grouped_view`) over the REAL column set, because the
// sort the curator experiences is chorale's, not ours.

/// The findings fixture these sort tests re-cut: one row per severity band, deliberately
/// NOT in any natural order, so a passing assertion can only come from the sort under
/// test. Returned already in the curated default order (as `FindingsTable` hands it to
/// chorale) so a cleared sort is provably a return to THAT order.
fn sortable_queue_rows() -> Vec<FindingView> {
    let raw = vec![
        kinded_finding(
            "B-LOW-DEFECT",
            "low",
            serde_json::json!({ "kind": "defect" }),
        ),
        kinded_finding(
            "A-MEDIUM-DEFECT",
            "medium",
            serde_json::json!({ "kind": "defect" }),
        ),
        kinded_finding(
            "C-CRITICAL-DEFECT",
            "critical",
            serde_json::json!({ "kind": "defect" }),
        ),
        kinded_finding(
            "D-HIGH-DEFECT",
            "high",
            serde_json::json!({ "kind": "defect" }),
        ),
    ];
    TriageModel::new().visible(&raw)
}

/// The rows' serialized bytes, in order. `FindingView` is `Serialize` but not `Debug`, and
/// this is the comparison the round-trip test actually wants anyway: byte-identical, with a
/// readable diff when it is not.
fn row_bytes(findings: &[FindingView]) -> String {
    serde_json::to_string(findings).expect("FindingView serializes")
}

/// Mint `(RowId, FindingView)` pairs in the given order, as `FindingsTable` does.
fn sort_rows(findings: &[FindingView]) -> Vec<(RowId, FindingView)> {
    findings.iter().map(|f| (RowId::new(), f.clone())).collect()
}

/// M10: eleven services each have a `src/index.ts`. Grouped by file, the Queue must show one
/// group per repo's file, not one merged `src/index.ts` group, and say which repo each is.
#[test]
fn m10_the_same_path_in_two_repos_is_two_file_groups() {
    use chorale_core::{set_grouping, visible_grouped_view, GroupedRow};

    fn in_repo(repo: &str, line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": repo, "path": "src/index.ts", "line": line, "rule_id": "R-1",
            "severity": "high", "snippet": format!("s{line}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }
    let rows = vec![
        in_repo("acme/a", 1),
        in_repo("acme/b", 1),
        in_repo("acme/a", 2),
        in_repo("acme/b", 2),
    ];
    let headers = |repos: Vec<String>| -> Vec<(String, usize)> {
        let state = finding_table_state(sort_rows(&rows), repos, false);
        let state = set_grouping(&state, vec![ColumnId("file")]);
        visible_grouped_view(&state)
            .into_iter()
            .filter_map(|r| match r {
                GroupedRow::Header {
                    label, row_count, ..
                } => Some((label, row_count)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        headers(vec!["acme/a".into(), "acme/b".into()]),
        vec![
            ("acme/a: src/index.ts".to_string(), 2),
            ("acme/b: src/index.ts".to_string(), 2)
        ]
    );
    // One repo in play: the bare path, as before.
    let single: Vec<FindingView> = rows
        .iter()
        .filter(|f| f.repo == "acme/a")
        .cloned()
        .collect();
    let state = finding_table_state(sort_rows(&single), vec!["acme/a".into()], false);
    let state = set_grouping(&state, vec![ColumnId("file")]);
    let labels: Vec<String> = visible_grouped_view(&state)
        .into_iter()
        .filter_map(|r| match r {
            GroupedRow::Header { label, .. } => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["src/index.ts".to_string()]);
}

/// A `TableState` over the real findings columns, rows in the curated default order and
/// chorale's own sort EMPTY — exactly the state `FindingsTable` mounts.
fn queue_table_state(findings: &[FindingView]) -> chorale_core::TableState<FindingView> {
    chorale_core::TableState::new(
        sort_rows(findings),
        finding_columns(vec!["me/api".to_string()], false),
    )
}

/// req_pass2_8: a manual sort on the Severity header must order the AUTHORED severity by
/// its CALIBRATED rank (critical > high > medium > low), not by its alphabet.
///
/// This is the one column whose words do not sort into their own meaning: alphabetically
/// `low` lands above `medium`, so a lexicographic header cut reads
/// "critical, high, low, medium" — a visibly wrong severity ladder. The rank is the same
/// ordering of the same authored string that `queue_sort_key` uses; nothing is re-derived.
#[test]
fn req_pass2_8_severity_header_sort_is_calibrated_not_alphabetical() {
    use chorale_core::{filtered_sorted_rows, toggle_sort, SortAction};

    let findings = sortable_queue_rows();
    let state = queue_table_state(&findings);
    let sorted = toggle_sort(&state, ColumnId("severity"), SortAction::Replace);
    let order: Vec<String> = filtered_sorted_rows(&sorted)
        .iter()
        .map(|f| f.severity.clone())
        .collect();
    assert_eq!(
        order,
        vec!["critical", "high", "medium", "low"],
        "ascending Severity must walk the calibrated ladder; an alphabetical cut would \
             put `low` above `medium`"
    );

    // And descending is the exact reverse — not a different alphabet.
    let sorted = toggle_sort(&sorted, ColumnId("severity"), SortAction::Replace);
    let order: Vec<String> = filtered_sorted_rows(&sorted)
        .iter()
        .map(|f| f.severity.clone())
        .collect();
    assert_eq!(order, vec!["low", "medium", "high", "critical"]);
}

/// req_pass2_8: the Queue names the order it is currently showing and, when that order is
/// the curator's own cut, offers an explicit way back to the curated default.
///
/// The indicator is not decoration: the default order is a judgement (cheapest deviations
/// first, the `unspecified` alarm floated to the top of its band) and a curator 300 rows
/// into a sitting has to be able to tell at a glance whether he is looking at it or at his
/// own re-cut.
#[test]
fn req_pass2_8_queue_names_the_active_order_and_defaults_to_the_curated_one() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: sortable_queue_rows(),
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("queue-order"),
        "the Queue must carry an order indicator; html=\n{html}"
    );
    assert!(
        html.contains("Camerata default order"),
        "on mount the indicator must name the CURATED DEFAULT, not a blank; html=\n{html}"
    );
    assert!(
        !html.contains("queue-order-reset"),
        "no manual sort is active on mount, so the return-to-default control must not be \
             rendered — its absence is part of the indicator; html=\n{html}"
    );
}

/// req_pass2_8 (UNDER RENDER, the manual state): with a cut active the bar names the column
/// and direction in an accented style AND renders the way back to the default.
///
/// `FindingsTable` owns its own `TableState`, and SSR cannot click a column header, so the
/// bar is its own component precisely so this state can be rendered at all. Without it,
/// "the Default order button appears when you sort" would be untested in the product.
#[test]
fn req_pass2_8_order_bar_renders_the_manual_state_with_the_way_back() {
    use chorale_core::{SortDirection, SortState};

    // `Callback::new` has to run inside the Dioxus runtime, so the sort is passed into a
    // harness ROOT rather than built into props from outside the VirtualDom.
    #[component]
    fn OrderBarHarness(sort: Vec<SortState>) -> Element {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        rsx! {
            QueueOrderBar {
                order: queue_order(OrderPreset::Curated, &sort, &cols),
                on_reset: Callback::new(|()| {}),
            }
        }
    }

    fn bar(sort: Vec<SortState>) -> String {
        let mut vdom = VirtualDom::new_with_props(OrderBarHarness, OrderBarHarnessProps { sort });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    // Default state: names the curated order, no reset control, no accent.
    let html = bar(vec![]);
    assert!(
        html.contains(camerata_ui_core::order_preset::CURATED_SENTENCE),
        "html=\n{html}"
    );
    assert!(!html.contains("queue-order-reset"), "html=\n{html}");
    assert!(!html.contains("queue-order-value manual"), "html=\n{html}");

    // Manual state: column, direction, accent, and the Default order button.
    let html = bar(vec![SortState::new(
        ColumnId("severity"),
        SortDirection::Desc,
    )]);
    assert!(html.contains("Severity"), "names the column; html=\n{html}");
    assert!(
        html.contains('\u{2193}'),
        "names the direction; html=\n{html}"
    );
    assert!(
        html.contains("queue-order-value manual"),
        "a manual cut reads differently from the default; html=\n{html}"
    );
    assert!(
        html.contains("queue-order-reset") && html.contains("Default order"),
        "the way back must be visible and labelled; html=\n{html}"
    );
    assert!(
        !html.contains(camerata_ui_core::order_preset::CURATED_SENTENCE),
        "the bar must not claim to be showing the default while a cut is active; html=\n{html}"
    );
}

/// req_pass2_8 (THE failure mode that would actually hurt): the round trip. Default order,
/// a manual cut, back to the default — and the restored default must be BYTE-IDENTICAL to
/// where it started, not approximately restored.
///
/// Driven through chorale end to end, because that is where the guarantee comes from:
/// `filtered_sorted_pairs` only reorders when `state.sort` is non-empty, so an empty sort
/// hands back `state.rows` exactly as `TriageModel::visible` ordered them. Losing the
/// curated default with no way back would be worse than having no manual sort at all.
#[test]
fn req_pass2_8_manual_sort_round_trips_to_a_byte_identical_default_order() {
    use chorale_core::{clear_sort, filtered_sorted_rows, toggle_sort, SortAction};

    let findings = sortable_queue_rows();
    let state = queue_table_state(&findings);
    let baseline = filtered_sorted_rows(&state);
    assert_eq!(
        row_bytes(&baseline),
        row_bytes(&findings),
        "an empty sort must hand back the curated order untouched"
    );

    // A cut that provably MOVES the rows, so the restore below cannot pass vacuously. By
    // rule id this fixture is scrambled relative to the default (the default leads on
    // severity, and the rule ids were chosen to disagree with it).
    let by_rule = toggle_sort(&state, ColumnId("type"), SortAction::Replace);
    assert_ne!(
        row_bytes(&filtered_sorted_rows(&by_rule)),
        row_bytes(&baseline),
        "the Finding-type cut must actually re-cut the list"
    );
    assert_eq!(
        row_bytes(&filtered_sorted_rows(&clear_sort(&by_rule))),
        row_bytes(&baseline),
        "`Default order` must restore the curated order byte-for-byte"
    );

    // And from a reversed severity cut, which is the one that inverts the whole queue.
    let desc = toggle_sort(
        &toggle_sort(&state, ColumnId("severity"), SortAction::Replace),
        ColumnId("severity"),
        SortAction::Replace,
    );
    assert_ne!(
        row_bytes(&filtered_sorted_rows(&desc)),
        row_bytes(&baseline),
        "descending severity must re-cut"
    );
    assert_eq!(
        row_bytes(&filtered_sorted_rows(&clear_sort(&desc))),
        row_bytes(&baseline),
        "`Default order` must restore the curated order from a reversed cut too"
    );

    // And from a MULTI-column cut (shift+click), which is exactly the state chorale's own
    // third-click cycle cannot undo on its own: a plain click clears one column, leaving
    // the others set. This is why the explicit control is the contract.
    let multi = toggle_sort(&state, ColumnId("file"), SortAction::Append);
    let multi = toggle_sort(&multi, ColumnId("severity"), SortAction::Append);
    assert_eq!(multi.sort.len(), 2, "two columns are sorted");
    let one_more_click = toggle_sort(&multi, ColumnId("severity"), SortAction::Replace);
    assert!(
        !one_more_click.sort.is_empty(),
        "a plain click out of a multi-sort does NOT reach the default — it replaces the \
             list with one column. Without the explicit control there would be no way back."
    );
    assert_eq!(
        row_bytes(&filtered_sorted_rows(&clear_sort(&multi))),
        row_bytes(&baseline),
        "`Default order` must restore the curated order from a multi-column cut too"
    );
}

/// req_pass2_8: the default order survives a SCAN CHANGE. A fresh mount starts with
/// chorale's sort empty, which is the curated default — the curator never inherits the
/// previous scan's re-cut, and never has to reset it by hand.
#[test]
fn req_pass2_8_a_fresh_scan_mounts_on_the_curated_default() {
    let state = queue_table_state(&sortable_queue_rows());
    assert!(
        state.sort.is_empty(),
        "a newly mounted findings table carries no manual sort"
    );
    assert_eq!(
        order_indicator_text(
            OrderPreset::Curated,
            &state.sort,
            &finding_columns(vec![], false)
        ),
        camerata_ui_core::order_preset::CURATED_SENTENCE,
        "and the indicator says so"
    );
}

/// req_pass2_8 (manual sort vs RULE GROUPING — the deliberate choice): a manual cut
/// reorders the GROUPS and the rows within them; it never flattens the grouping.
///
/// That falls out of chorale's own pipeline rather than being imposed: `visible_grouped_view`
/// sorts the whole row set first and then partitions it into groups in first-occurrence
/// order, so each group lands at the position of its best row under the active cut and its
/// rows keep that cut inside the group. It is also exactly what design §5 specifies
/// ("Groups order by their best row; rows order within groups by the same key").
///
/// Grouping is load-bearing — it is what the deviation group-header sentence and the later
/// rule-level bulk actions hang off — so "every group still has exactly one header and a
/// contiguous block of rows" is the invariant under test, not just the row order.
#[test]
fn req_pass2_8_manual_sort_reorders_rule_groups_without_flattening_them() {
    use chorale_core::{toggle_sort, visible_grouped_view, GroupedRow, SortAction};

    // Two rules, two sites each, so a cut can both reorder the groups and reorder inside
    // one. Severities are chosen so the severity cut interleaves the rules if grouping were
    // ignored — that is what makes this test able to fail.
    let findings = TriageModel::new().visible(&[
        kinded_finding(
            "RULE-B",
            "critical",
            serde_json::json!({ "kind": "defect" }),
        ),
        kinded_finding("RULE-A", "high", serde_json::json!({ "kind": "defect" })),
        kinded_finding("RULE-B", "low", serde_json::json!({ "kind": "defect" })),
        kinded_finding("RULE-A", "medium", serde_json::json!({ "kind": "defect" })),
    ]);
    let mut state = queue_table_state(&findings);
    // Group by rule only (the real table adds a second File level; one level is enough to
    // pin the invariant and keeps the walk readable).
    state.grouping = vec![ColumnId("type")];
    state.grouped_pagination = chorale_core::GroupedPaginationMode::Virtualized;

    /// Walk a grouped view into (group label, row count) pairs, asserting as it goes that
    /// no group's header appears twice and that every data row sits under a header.
    fn group_blocks(view: &[GroupedRow<FindingView>]) -> Vec<(String, usize)> {
        let mut blocks: Vec<(String, usize)> = Vec::new();
        for row in view {
            match row {
                GroupedRow::Header { label, .. } => {
                    assert!(
                        !blocks.iter().any(|(l, _)| l == label),
                        "group {label} appeared twice — the grouping was flattened"
                    );
                    blocks.push((label.clone(), 0));
                }
                GroupedRow::Data(..) => {
                    blocks
                        .last_mut()
                        .expect("a data row before any group header")
                        .1 += 1;
                }
                // `GroupedRow` is `#[non_exhaustive]`, so the wildcard is mandatory rather
                // than a choice. It panics instead of skipping: a chorale upgrade that adds
                // a row kind this walk does not understand would otherwise make the
                // grouping invariant quietly stop being checked.
                _ => panic!(
                    "chorale's GroupedRow grew a variant this grouping-invariant walk does \
                         not understand; teach it the new row kind rather than skipping it"
                ),
            }
        }
        blocks
    }

    // Default: RULE-B leads on its critical row; each rule keeps both of its sites.
    assert_eq!(
        group_blocks(&visible_grouped_view(&state)),
        vec![("RULE-B".to_string(), 2), ("RULE-A".to_string(), 2)],
        "default order: the group with the critical row first, both groups intact"
    );

    // Ascending Severity: RULE-B still leads (critical), and inside each group the two
    // sites are now in ladder order rather than the default's.
    let asc = toggle_sort(&state, ColumnId("severity"), SortAction::Replace);
    assert_eq!(
        group_blocks(&visible_grouped_view(&asc)),
        vec![("RULE-B".to_string(), 2), ("RULE-A".to_string(), 2)],
        "a manual cut keeps one header per group and its rows contiguous"
    );

    // Descending Severity: RULE-B's best row is now its `low` one, so the GROUPS swap —
    // the group order follows the group's best row under the active cut.
    let desc = toggle_sort(&asc, ColumnId("severity"), SortAction::Replace);
    assert_eq!(
        group_blocks(&visible_grouped_view(&desc)),
        vec![("RULE-B".to_string(), 2), ("RULE-A".to_string(), 2)],
        "reversing the cut still yields exactly two intact groups"
    );
    let rows: Vec<String> = visible_grouped_view(&desc)
        .iter()
        .filter_map(|r| match r {
            GroupedRow::Data(_, f) => Some(f.severity.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        vec!["low", "critical", "medium", "high"],
        "within each group the rows follow the descending cut; the groups are led by \
             their own best row under it"
    );
}

/// req_pass2_8: a manual cut PRESERVES the deviation-ratio honesty baked into the default.
///
/// The default key orders deviations by ascending `deviating_sites / assessed_sites` in
/// parts-per-million, with an unknown or zero denominator sorting after every known ratio
/// rather than fabricating a position. chorale's sort is STABLE, so ties in the manual key
/// keep the curated order underneath — a cut by Kind groups the deviations together and the
/// cheapest-first, unknown-last ratio order survives inside that block, untouched and
/// un-recomputed.
#[test]
fn req_pass2_8_manual_sort_preserves_the_deviation_ratio_order_within_ties() {
    use chorale_core::{filtered_sorted_rows, toggle_sort, SortAction};

    let dev = |rule: &str, deviating: u32, assessed: serde_json::Value| {
        kinded_finding(
            rule,
            "medium",
            serde_json::json!({
                "kind": "deviation",
                "deviating_sites": deviating,
                "assessed_sites": assessed,
                "convention_option_id": "versioned",
                "convention_label": "URI-versioned endpoints",
            }),
        )
    };
    let findings = TriageModel::new().visible(&[
        dev("A-UNKNOWN-RATIO", 3, serde_json::Value::Null),
        dev("B-COSTLY", 19, serde_json::json!(47)),
        dev("C-CHEAP", 2, serde_json::json!(47)),
        kinded_finding(
            "D-DEFECT",
            "medium",
            serde_json::json!({ "kind": "defect" }),
        ),
    ]);
    let state = queue_table_state(&findings);

    // Sanity: the default already orders them cheapest-first, unknown last.
    assert_eq!(
        findings
            .iter()
            .map(|f| f.rule_id.as_str())
            .collect::<Vec<_>>(),
        vec!["D-DEFECT", "C-CHEAP", "B-COSTLY", "A-UNKNOWN-RATIO"],
    );

    // A manual cut by Kind. Every deviation ties on the Kind key, so the stable sort hands
    // the ratio ordering straight through.
    let by_kind = toggle_sort(&state, ColumnId("kind"), SortAction::Replace);
    assert_eq!(
        filtered_sorted_rows(&by_kind)
            .iter()
            .map(|f| f.rule_id.clone())
            .collect::<Vec<_>>(),
        vec!["D-DEFECT", "C-CHEAP", "B-COSTLY", "A-UNKNOWN-RATIO"],
        "cheapest deviation first and the unknown denominator last, inside the manual cut \
             — the ratio is never re-derived and never given a fabricated position"
    );
}

/// req_pass2_8: the order indicator names the active column AND direction, in priority
/// order for a multi-column cut, and says the curated default is still underneath (which it
/// is — chorale's sort is stable).
#[test]
fn req_pass2_8_order_indicator_names_the_column_and_direction() {
    use chorale_core::{SortDirection, SortState};

    let cols = finding_columns(vec!["me/api".to_string()], false);
    assert_eq!(
        order_indicator_text(OrderPreset::Curated, &[], &cols),
        camerata_ui_core::order_preset::CURATED_SENTENCE
    );

    let one = [SortState::new(ColumnId("severity"), SortDirection::Desc)];
    let text = order_indicator_text(OrderPreset::Curated, &one, &cols);
    assert!(
        text.contains("Severity"),
        "names the column header; got {text}"
    );
    assert!(text.contains('\u{2193}'), "names the direction; got {text}");
    assert!(
        text.contains("Camerata default order underneath"),
        "says the curated order is still the tie-break; got {text}"
    );

    let two = [
        SortState::new(ColumnId("file"), SortDirection::Asc),
        SortState::new(ColumnId("severity"), SortDirection::Desc),
    ];
    let text = order_indicator_text(OrderPreset::Curated, &two, &cols);
    assert!(
        text.find("File").expect("File named") < text.find("Severity").expect("Severity named"),
        "a multi-column cut is listed in priority order; got {text}"
    );

    // An id with no column (a column removed from the set) degrades to the raw id rather
    // than dropping the cut from the sentence and claiming an order that isn't showing.
    let gone = [SortState::new(
        ColumnId("no-such-column"),
        SortDirection::Asc,
    )];
    assert!(order_indicator_text(OrderPreset::Curated, &gone, &cols).contains("no-such-column"));
}

/// req_pass2_8: the whole Queue is re-cuttable. Every column the curator would re-cut by is
/// `.sortable()`, so chorale renders its header clickable with its own ↑/↓ arrow; only
/// Snippet (free text, no meaningful order) is not.
#[test]
fn req_pass2_8_every_meaningful_queue_column_header_is_sortable() {
    let cols = finding_columns(vec!["me/api".to_string()], false);
    for id in [
        "repo",
        "kind",
        "severity",
        "authority",
        "type",
        "needs_review",
        "status",
        "file",
        "loc",
    ] {
        let col = cols
            .iter()
            .find(|c| c.id == ColumnId(id))
            .unwrap_or_else(|| panic!("column {id} must exist"));
        assert!(
            col.sortable,
            "{id}: the curator must be able to re-cut the list by this column"
        );
    }
}

/// req_pass2_8: the Severity column's filter vocabulary is built from the SAME function as
/// its cell value, so the two cannot drift. A stale option list would match nothing and
/// read as "no findings" — a worse failure than a wrong sort.
#[test]
fn req_pass2_8_severity_filter_vocabulary_matches_the_cell_it_filters() {
    let cols = finding_columns(vec!["me/api".to_string()], false);
    let sev = cols
        .iter()
        .find(|c| c.id == ColumnId("severity"))
        .expect("Severity column");
    let options = match &sev.filter {
        FilterKind::MultiSelect { options } => options.clone(),
        other => panic!("Severity must keep its multi-select filter, got {other:?}"),
    };
    for s in ["critical", "high", "medium", "low"] {
        let cell = (sev.accessor)(&kinded_finding("R", s, serde_json::json!({})));
        let CellValue::Text(cell) = cell else {
            panic!("Severity cells are text");
        };
        assert!(
            options.contains(&cell),
            "{s}: the filter option list must contain the exact cell value it filters \
                 (cell={cell}, options={options:?})"
        );
    }
}

/// req_pass2_8: making the cell value the sort key must not change what the curator READS.
///
/// chorale renders a `RenderKind::Badge` cell by resolving the cell value in the badge map
/// and drawing the matched variant's LABEL (`badge_span` in chorale-dioxus), so asserting
/// the map resolution is asserting the rendered text. Done here rather than over SSR HTML
/// because the real table mounts with every group collapsed, so no severity cell is in the
/// snapshot to look at.
///
/// The one place the ladder prefix IS visible is the Severity filter's checkbox list, whose
/// option labels are the raw values chorale filters on — it now reads "1 critical / 2 high /
/// 3 medium / 4 low". That is the accepted cost of chorale 0.2.3 having no sort-key hook
/// separate from the cell value; the list was already in this order, and it is honest about
/// the ladder rather than wrong about it.
#[test]
fn req_pass2_8_severity_cell_still_renders_the_authored_label() {
    let cols = finding_columns(vec!["me/api".to_string()], false);
    let sev = cols
        .iter()
        .find(|c| c.id == ColumnId("severity"))
        .expect("Severity column");
    let RenderKind::Badge(map) = &sev.render_kind else {
        panic!("Severity must still render as a badge");
    };
    for (authored, label, color) in [
        ("critical", "Critical", "red"),
        ("high", "High", "orange"),
        ("medium", "Medium", "yellow"),
        ("low", "Low", "gray"),
    ] {
        let CellValue::Text(cell) =
            (sev.accessor)(&kinded_finding("R", authored, serde_json::json!({})))
        else {
            panic!("Severity cells are text");
        };
        let variant = map.resolve(&cell).unwrap_or_else(|| {
            panic!("{authored}: cell value {cell:?} must resolve in the badge map")
        });
        assert_eq!(
            variant.label, label,
            "{authored}: the rendered badge must read the authored label, never the \
                 ladder prefix"
        );
        assert_eq!(variant.color, color, "{authored}: colour unchanged");
    }
}

/// req_pass2_8 (a DELIBERATE non-change, pinned so it is not "fixed" by accident): the Kind
/// column's cell value stays the bare WIRE TAG.
///
/// Severity needed its ladder position in the cell because its words sort against their own
/// meaning. Kind's do not: the wire tags sort `defect` → `deviation` → `unspecified`, which
/// is a coherent actionability order. Prefixing a rank here would buy nothing and would cost
/// the thing req_pass2_5 pins — that the Kind filter's vocabulary IS the wire's, never
/// re-derived prose.
///
/// The `unspecified`-first priority therefore lives where Zach's ruling put it: in the
/// DEFAULT order (`kind_rank`), which is what the table shows until the curator chooses
/// otherwise. The alarm is also surfaced independently of row order by the Queue tab's
/// count chip, so a manual cut can never hide it.
#[test]
fn req_pass2_8_kind_column_keeps_the_bare_wire_tag_as_its_cell_value() {
    use chorale_core::{filtered_sorted_rows, toggle_sort, SortAction};

    let cols = finding_columns(vec!["me/api".to_string()], false);
    let kind = cols
        .iter()
        .find(|c| c.id == ColumnId("kind"))
        .expect("Kind column");
    let row = kinded_finding("R", "high", serde_json::json!({ "kind": "defect" }));
    assert_eq!(
        (kind.accessor)(&row),
        CellValue::Text("defect".to_string()),
        "no ladder prefix on Kind — the wire tag is the cell value and the filter vocabulary"
    );

    // And the cut it produces is the actionability order, not an accident nobody checked.
    let findings = TriageModel::new().visible(&[
        kinded_finding("A-UNMAPPED", "high", serde_json::json!({})),
        kinded_finding("B-DEFECT", "high", serde_json::json!({ "kind": "defect" })),
        kinded_finding(
            "C-DEVIATION",
            "high",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                "convention_option_id": "v", "convention_label": "L",
            }),
        ),
    ]);
    assert_eq!(
        findings
            .iter()
            .map(|f| f.rule_id.as_str())
            .collect::<Vec<_>>(),
        vec!["A-UNMAPPED", "B-DEFECT", "C-DEVIATION"],
        "the DEFAULT floats the kind-less alarm to the top of its severity band"
    );
    let by_kind = toggle_sort(
        &queue_table_state(&findings),
        ColumnId("kind"),
        SortAction::Replace,
    );
    assert_eq!(
        filtered_sorted_rows(&by_kind)
            .iter()
            .map(|f| f.rule_id.clone())
            .collect::<Vec<_>>(),
        vec!["B-DEFECT", "C-DEVIATION", "A-UNMAPPED"],
        "the manual Kind cut is the wire tags' own order: defect, deviation, unspecified"
    );
}

// ── Pass 2 step 2: the deviation group header ────────────────────────────
// the pass-2 curator UI design notes §2/§4/§11-step-2: "Group headers carry
// the Deviation sentence ... rendered from data: 'N of M sites deviate from
// <convention_label>'. No prose parsing" and "Group headers carry rule-level bulk
// actions for deviations ... because the natural unit of a deviation decision is the
// rule, not the site."

/// A deviation rule with two sites, plus a defect rule, as `FindingsTable` receives them.
/// Paths differ so the two deviating sites have distinct `finding_key`s (a collision would
/// make the bulk-equivalence assertions vacuous).
pub(super) fn deviation_queue_rows() -> Vec<FindingView> {
    let mut dev_a = kinded_finding(
        "API-VERSIONING-1",
        "medium",
        serde_json::json!({
            "kind": "deviation",
            "deviating_sites": 2,
            "assessed_sites": 47,
            "convention_option_id": "versioned",
            "convention_label": "URI-versioned endpoints",
        }),
    );
    dev_a.path = "src/routes.rs".to_string();
    dev_a.line = 12;
    let mut dev_b = dev_a.clone();
    dev_b.line = 40;
    // Two sites of one snippet: the server keeps them apart with an ordinal.
    dev_b.site_key = camerata_api_types::site_key::site_key(
        &dev_b.repo,
        &dev_b.path,
        &dev_b.rule_id,
        &dev_b.snippet,
        1,
    );
    let mut defect = kinded_finding(
        "SQL-INJECTION-1",
        "critical",
        serde_json::json!({ "kind": "defect" }),
    );
    defect.path = "src/handlers.rs".to_string();
    defect.line = 88;
    vec![dev_a, dev_b, defect]
}

fn deviation_table_html() -> String {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: deviation_queue_rows(),
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

/// The rendered table from its FIRST group-header `<tr>` onwards.
///
/// Load-bearing, not tidiness: the bulk bar above the table also names the rule and
/// repeats the sentence, so an assertion made against the whole document could be
/// satisfied by the bar while chorale's group header still showed a bare rule id. Every
/// "the group header carries X" claim below is checked against this slice only.
pub(super) fn group_header_region(html: &str) -> &str {
    let at = html
        .find("class=\"chorale-group-header\"")
        .unwrap_or_else(|| panic!("no chorale group header rendered; html=\n{html}"));
    &html[at..]
}

/// The rendered document BEFORE the table — where the bulk bar lives.
fn above_table_region(html: &str) -> &str {
    let at = html
        .find("class=\"chorale-group-header\"")
        .unwrap_or_else(|| panic!("no chorale group header rendered; html=\n{html}"));
    &html[..at]
}

/// req_pass2_9: the deviation rule group STATES ITS CLAIM on chorale's own group header,
/// assembled from the row's structured fields.
///
/// The table mounts collapsed, so the group header is the only thing the curator sees
/// before drilling in — and a bare rule id there says nothing about whether the convention
/// behind it is near-unanimous (2 of 47, decide in one look) or barely a majority (19 of
/// 47, think about it). The numbers exist on every deviation row as `deviating_sites` /
/// `assessed_sites` / `convention_label`; until this shipped, nothing in the UI read them
/// and the header rendered exactly `▶API-VERSIONING-1(2)`.
#[test]
fn req_pass2_9_deviation_group_header_carries_the_structured_sentence() {
    let html = deviation_table_html();
    let headers = group_header_region(&html);
    assert!(
        headers.contains("2 of 47 sites deviate from"),
        "the deviation group header must carry the structured numerator/denominator \
             sentence; group headers=\n{headers}"
    );
    // dioxus_ssr escapes the quotes around the label, so the label is asserted on its own
    // rather than as part of the quoted clause.
    assert!(
        headers.contains("URI-versioned endpoints"),
        "and name the convention verbatim from `convention_label`; group headers=\n{headers}"
    );
    assert!(
        headers.contains("API-VERSIONING-1 \u{b7} 2 of 47"),
        "the rule id and its claim are one header line; group headers=\n{headers}"
    );
}

/// req_pass2_9: the deviation group offers the RULE-LEVEL bulk disposition, so the common
/// case is one gesture rather than N.
///
/// The count on the button is the number of ROWS the click will move (2), which is the
/// honest label for what it does — deliberately not the site count in the sentence, which
/// counts sites across the whole scan including merged ones.
#[test]
fn req_pass2_9_deviation_group_offers_a_rule_level_bulk_action() {
    let html = deviation_table_html();
    let bar = above_table_region(&html);
    assert!(
        bar.contains("deviation-bulk"),
        "a view with deviations must render the rule-level decision bar; above table=\n{bar}"
    );
    assert!(
        bar.contains("False positive all 2"),
        "the bulk action is labelled with the row count it will move; above table=\n{bar}"
    );
    assert!(
        bar.contains("2 of 47 sites deviate from"),
        "and states the claim it is acting on; above table=\n{bar}"
    );
}

/// req_pass2_9: a DEFECT group gets the plain header and no rule-level bulk action.
///
/// Design §4: "Defect group headers are plainer: `rule-id · DEFECT · worst-severity ·
/// (n)`", and §2's sketch puts the bulk pair on the deviation group only — the `(3)` on
/// its defect group is chorale's row count, not a `Confirm all 3`. The reason is in the
/// data, not in taste: every row of a deviation rule repeats the SAME group-level claim,
/// so the rule is the decision unit; a defect group is N independent accusations that
/// merely share a rule id, and one click asserting all N would be asserting facts the
/// curator has not looked at. A curator who genuinely wants to bulk-dispose a defect group
/// still can — chorale's per-group checkbox plus the toolbar — but it costs two deliberate
/// gestures, which is the right price for that claim.
#[test]
fn req_pass2_9_defect_group_header_is_plain_and_has_no_bulk_action() {
    let html = deviation_table_html();
    let headers = group_header_region(&html);
    assert!(
        headers.contains("SQL-INJECTION-1"),
        "the defect group still has its header; group headers=\n{headers}"
    );
    assert!(
        !headers.contains("SQL-INJECTION-1 \u{b7}"),
        "but carries no sentence after it; group headers=\n{headers}"
    );

    let bar = above_table_region(&html);
    assert_eq!(
        bar.matches("deviation-bulk-group").count(),
        1,
        "exactly one rule-level decision row — the deviation's; above table=\n{bar}"
    );
    assert!(
        !bar.contains("SQL-INJECTION-1"),
        "the defect rule is not offered a rule-level bulk action; above table=\n{bar}"
    );
}

/// req_pass2_9: the header sentence survives a MANUAL column cut and the collapsed mount.
///
/// The sentence rides the group-by VALUE, which is chorale's own machinery, so a re-sort
/// reorders the groups and carries each header's text with it rather than stranding a
/// separately-rendered banner against a group that moved. This drives chorale's real
/// pipeline (`toggle_sort` + `visible_grouped_view`) over the real column set, the same
/// way the req_pass2_8 sort tests do.
#[test]
fn req_pass2_9_group_header_sentence_survives_a_manual_re_sort() {
    use chorale_core::{toggle_sort, visible_grouped_view, GroupedRow, SortAction};

    let findings = TriageModel::new().visible(&deviation_queue_rows());
    let mut state = finding_table_state(sort_rows(&findings), vec!["me/api".to_string()], false);
    state.grouping = vec![RULE_GROUP_COLUMN];
    state.grouped_pagination = chorale_core::GroupedPaginationMode::Virtualized;

    let labels = |s: &chorale_core::TableState<FindingView>| -> Vec<String> {
        visible_grouped_view(s)
            .iter()
            .filter_map(|r| match r {
                GroupedRow::Header { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    };

    let before = labels(&state);
    assert_eq!(
        before,
        vec![
            "SQL-INJECTION-1".to_string(),
            "API-VERSIONING-1 \u{b7} 2 of 47 sites deviate from \"URI-versioned endpoints\""
                .to_string(),
        ],
        "default order: the critical defect's plain header, then the deviation's claim"
    );

    // Descending Severity puts the medium deviation first. The header text must travel
    // with its group, not stay behind.
    let asc = toggle_sort(&state, ColumnId("severity"), SortAction::Replace);
    let desc = toggle_sort(&asc, ColumnId("severity"), SortAction::Replace);
    let after = labels(&desc);
    assert_eq!(after.len(), 2, "still exactly two groups; no flattening");
    assert_eq!(
        after[0], "API-VERSIONING-1 \u{b7} 2 of 47 sites deviate from \"URI-versioned endpoints\"",
        "the deviation group led the reversed cut, sentence intact"
    );
    assert_eq!(after[1], "SQL-INJECTION-1");
}

/// req_pass2_9 (THE HONESTY CASE, UNDER RENDER): a deviation whose producer could not
/// count the total renders the numerator ALONE on the real group header.
///
/// The pure sentence builder is unit-tested in ui-core; this proves the honest string is
/// what the product actually puts on screen. `assessed_sites: None` is the case most
/// likely to be quietly wrong, because the two ways to break it — fabricate a total, or
/// let an `unwrap_or(0)` print "3 of 0" — both still render something plausible-looking.
#[test]
fn req_pass2_9_absent_denominator_renders_the_numerator_alone_on_the_header() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: vec![kinded_finding(
                    "NAMING-1",
                    "medium",
                    serde_json::json!({
                        "kind": "deviation",
                        "deviating_sites": 3,
                        "assessed_sites": serde_json::Value::Null,
                        "convention_option_id": serde_json::Value::Null,
                        "convention_label": "snake_case table names",
                    }),
                )],
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    let headers = group_header_region(&html);
    assert!(
        headers.contains("NAMING-1 \u{b7} 3 sites deviate from"),
        "numerator alone on the header; group headers=\n{headers}"
    );
    assert!(
        headers.contains("snake_case table names"),
        "the convention is still named; group headers=\n{headers}"
    );
    assert!(
        !headers.contains("3 of "),
        "no fabricated denominator anywhere on the header; group headers=\n{headers}"
    );
    // The bar repeats the same honest sentence — one builder, two surfaces.
    let bar = above_table_region(&html);
    assert!(
        bar.contains("3 sites deviate from") && !bar.contains("3 of "),
        "and the bulk bar states it the same way; above table=\n{bar}"
    );
}

/// req_pass2_9: the grouping key stays INVISIBLE. It is a delivery mechanism for the
/// header text, not a column; shipping it as a visible "Rule group" column would put the
/// sentence on every single row as well.
#[test]
fn req_pass2_9_the_rule_group_key_is_never_a_visible_column() {
    let state = finding_table_state(
        sort_rows(&TriageModel::new().visible(&deviation_queue_rows())),
        vec!["me/api".to_string()],
        false,
    );
    assert_eq!(
        state.column_visibility.get(&RULE_GROUP_COLUMN),
        Some(&false),
        "the grouping key is hidden in the state the product mounts"
    );
    let col = state
        .columns
        .iter()
        .find(|c| c.id == RULE_GROUP_COLUMN)
        .expect("the grouping key column exists");
    assert!(!col.sortable, "it is not a column the curator sorts by");
    assert!(
        matches!(col.filter, chorale_core::FilterKind::None),
        "nor filters by"
    );

    let html = deviation_table_html();
    assert!(
        !html.contains(">Rule group<"),
        "and its header label never reaches the DOM; html=\n{html}"
    );
}

/// req_pass2_9: the bar renders one decision row per group, props-only.
///
/// `DeviationBulkBar` is its own component for the same reason `QueueOrderBar` is: SSR
/// cannot click, and a bar built inline in `FindingsTable` could only ever be observed in
/// whatever state that table happens to mount in. As props-only data it can be rendered
/// in any state, including the multi-group one a real audit produces.
#[test]
fn req_pass2_9_bulk_bar_renders_one_decision_row_per_group() {
    #[component]
    fn BarHarness(groups: Vec<DeviationBulkRow>) -> Element {
        rsx! {
            DeviationBulkBar {
                groups,
                // EDITED for Pass 2 step 3 (exception (b)): the bar gained a second,
                // required callback prop. Nothing else about this harness changed.
                on_confirm_all: Callback::new(move |_: usize| {}),
                on_false_positive_all: Callback::new(move |_: usize| {}),
            }
        }
    }
    let groups = vec![
        DeviationBulkRow {
            rule_id: "API-VERSIONING-1".into(),
            sentence: "2 of 47 sites deviate from \"URI-versioned endpoints\"".into(),
            row_count: 2,
        },
        DeviationBulkRow {
            rule_id: "NAMING-1".into(),
            sentence: "3 sites deviate from \"snake_case table names\"".into(),
            row_count: 3,
        },
    ];
    let mut vdom = VirtualDom::new_with_props(BarHarness, BarHarnessProps { groups });
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert_eq!(
        html.matches("deviation-bulk-group").count(),
        2,
        "one decision row per group; html=\n{html}"
    );
    assert!(html.contains("False positive all 2"), "html=\n{html}");
    assert!(html.contains("False positive all 3"), "html=\n{html}");
    assert!(
        html.contains("2 of 47 sites deviate from") && html.contains("3 sites deviate from"),
        "each row states its own claim, including the uncounted one; html=\n{html}"
    );
}

/// req_pass2_9: no deviations means no bar at all — not an empty one.
///
/// Unlike the informational panels (whose empty state is a sentence, because "nothing of
/// this kind" is itself information the curator needs), this is a list of ACTIONS. An
/// empty action bar with a heading is a standing reminder of nothing.
#[test]
fn req_pass2_9_bulk_bar_renders_nothing_when_there_are_no_deviations() {
    #[component]
    fn EmptyBarHarness() -> Element {
        rsx! {
            DeviationBulkBar {
                groups: Vec::<DeviationBulkRow>::new(),
                // EDITED for Pass 2 step 3 (exception (b)): new required prop.
                on_confirm_all: Callback::new(move |_: usize| {}),
                on_false_positive_all: Callback::new(move |_: usize| {}),
            }
        }
    }
    let mut vdom = VirtualDom::new(EmptyBarHarness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("deviation-bulk"),
        "no deviations, no bar; html=\n{html}"
    );
    assert!(
        !html.contains("Rule-level decisions"),
        "and no orphan heading; html=\n{html}"
    );
}

/// req_pass2_9: a defect-only Queue gets no bulk bar at all.
#[test]
fn req_pass2_9_defect_only_queue_renders_no_bulk_bar() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: vec![kinded_finding(
                    "SQL-INJECTION-1",
                    "critical",
                    serde_json::json!({ "kind": "defect" }),
                )],
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("deviation-bulk"),
        "defects alone offer no rule-level decision; html=\n{html}"
    );
}

/// req_pass2_9: the bulk bar is not rendered in the triage tables where it would be dead.
///
/// Its one action is False positive, which needs the reason textarea that exists only in
/// the Unresolved toolbar — and a deviation already moved to Ignored or Tech debt has had
/// its rule-level decision made. A bar of buttons that cannot complete is the 403-button
/// shape the standing hide-dead-end-affordances rule forbids.
#[test]
fn req_pass2_9_bulk_bar_is_absent_from_the_non_unresolved_tables() {
    #[component]
    fn Harness(view: TriageState) -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let mut dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        // Park every row in the table under test, so the rows are actually present.
        use_hook(move || {
            let mut model = TriageModel::new();
            model.move_to(&deviation_queue_rows(), view);
            dispositions.set(model.dispositions);
        });
        rsx! {
            FindingsTable {
                findings: deviation_queue_rows(),
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: view,
                dispositions,
            }
        }
    }
    // EDITED for Pass 2 step 3 (exception (a)): `Confirmed` is a fifth table the bar
    // must also stay out of — a deviation group already confirmed has had its
    // rule-level decision made.
    for view in [
        TriageState::Confirmed,
        TriageState::Ignored,
        TriageState::TechDebt,
        TriageState::FalsePositive,
    ] {
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { view });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("deviation-bulk"),
            "{} must not render the rule-level bulk bar; html=\n{html}",
            view.label()
        );
        // The group header's sentence is NOT view-scoped — the claim is true wherever the
        // row sits, so it still rides the header here.
        assert!(
            group_header_region(&html).contains("2 of 47 sites deviate from"),
            "{}'s group header still states the claim; html=\n{html}",
            view.label()
        );
    }
}
