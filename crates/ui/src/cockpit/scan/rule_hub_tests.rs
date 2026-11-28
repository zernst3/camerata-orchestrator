//! Tests: rule hub tests.

use super::*;
use camerata_ui_core::triage::rule_hub;

fn row(repo: &str, rule_id: &str, path: &str, kind: serde_json::Value) -> FindingView {
    let mut v = serde_json::json!({
        "repo": repo, "path": path, "line": 4, "rule_id": rule_id,
        "severity": "medium", "snippet": "s", "detail": "d", "status": "active"
    });
    for (k, val) in kind.as_object().expect("kind payload is an object") {
        v[k] = val.clone();
    }
    serde_json::from_value(v).expect("valid FindingView fixture")
}

fn deviation_rows() -> Vec<FindingView> {
    vec![
        row(
            "me/api",
            "API-VERSIONING-1",
            "src/routes.rs",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                "convention_option_id": "uri", "convention_label": "URI-versioned endpoints",
                "evaluated_option_id": "uri"
            }),
        ),
        row(
            "me/api",
            "API-VERSIONING-1",
            "src/admin.rs",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                "convention_option_id": "uri", "convention_label": "URI-versioned endpoints",
                "evaluated_option_id": "uri"
            }),
        ),
    ]
}

fn strip_html(hub: RuleHubView) -> String {
    #[component]
    fn Harness(hub: RuleHubView) -> Element {
        rsx! {
            RuleHubStrip { hub, on_jump: Callback::new(|_: KindSurface| {}) }
        }
    }
    let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { hub });
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

/// req_pass2_12: the hub strip is the JOIN §8 asks for — it states the rule's kind
/// outcome, its counts, the convention behind it, the option it was evaluated against,
/// and offers one way in. Before this, a curator holding a rule id had nothing that
/// answered "and what happened to this rule in this scan" short of re-reading four
/// surfaces.
#[test]
fn req_pass2_12_the_hub_states_the_whole_of_one_rule_in_one_strip() {
    let rows = deviation_rows();
    let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("the rule is in the scan");
    let html = strip_html(hub);
    assert!(
        html.contains("This rule in this scan"),
        "the strip names itself; html=\n{html}"
    );
    assert!(
        html.contains("API-VERSIONING-1"),
        "the rule id; html=\n{html}"
    );
    assert!(html.contains("Deviation"), "the kind chip; html=\n{html}");
    assert!(
        html.contains("2 of 47 sites deviate"),
        "the structured outcome sentence, verbatim; html=\n{html}"
    );
    assert!(html.contains("2 row(s)"), "the row count; html=\n{html}");
    assert!(html.contains("2 site(s)"), "the site count; html=\n{html}");
    assert!(
        html.contains("URI-versioned endpoints"),
        "the convention label; html=\n{html}"
    );
    assert!(
        html.contains("evaluated against"),
        "the Evaluated-against fact, relocated here from RuleAlternativesPanel; html=\n{html}"
    );
    assert!(
        html.contains("Show in the Queue, filtered to this rule"),
        "the one jump, naming where it goes; html=\n{html}"
    );
}

/// req_pass2_12: THE HONESTY CONSTRAINT (§8). The hub may state how many sites conform,
/// because that number is in the data. It must not offer to show them, because they are
/// not. The strip therefore says so in the same sentence, and carries exactly ONE
/// clickable control — the jump.
#[test]
fn req_pass2_12_the_hub_counts_conforming_sites_and_offers_nothing_to_click_for_them() {
    let rows = deviation_rows();
    let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("present");
    let html = strip_html(hub);
    assert!(
        html.contains("45 of 47 assessed sites conform"),
        "the count, which IS in the data; html=\n{html}"
    );
    assert!(
        html.contains("not in the scan data"),
        "the strip says why there is nothing to open; html=\n{html}"
    );
    assert_eq!(
        html.matches("<button").count(),
        1,
        "exactly one control: the jump. A second one here would be a fabricated \
             navigation into data that does not exist. html=\n{html}"
    );
    assert!(
        !html.contains("<a "),
        "and no anchors either; html=\n{html}"
    );
}

/// req_pass2_12: an unknown denominator yields no conforming sentence at all. The
/// producer could not count honestly, so the hub says nothing rather than inventing a
/// total to subtract from.
#[test]
fn req_pass2_12_the_hub_is_silent_about_conformance_without_a_denominator() {
    let rows = vec![row(
        "me/api",
        "API-VERSIONING-1",
        "src/routes.rs",
        serde_json::json!({
            "kind": "deviation", "deviating_sites": 3, "assessed_sites": null,
            "convention_label": "URI-versioned endpoints"
        }),
    )];
    let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("present");
    let html = strip_html(hub);
    assert!(
        !html.contains("conform"),
        "no conformance claim; html=\n{html}"
    );
    assert!(
        html.contains("3 sites deviate"),
        "the numerator alone, as the group header renders it; html=\n{html}"
    );
}

/// req_pass2_12: the hub works for the informational surfaces too, and names the PANEL it
/// would take you to rather than the Queue. A convention row has no Queue to jump into.
#[test]
fn req_pass2_12_the_hub_names_the_panel_for_an_informational_rule() {
    let rows = vec![row(
        "me/api",
        "SQL-PARAM-1",
        "-",
        serde_json::json!({
            "kind": "convention_observed", "convention_option_id": "p",
            "convention_label": "Parameterized queries"
        }),
    )];
    let hub = rule_hub(&rows, "me/api", "SQL-PARAM-1").expect("present");
    let html = strip_html(hub);
    assert!(
        html.contains("Convention observed"),
        "the informational chip, not the Unspecified alarm; html=\n{html}"
    );
    assert!(
        html.contains("Show in \u{201c}Conventions\u{201d}"),
        "the jump names the panel; html=\n{html}"
    );
    assert!(
        !html.contains("Show in the Queue"),
        "there is no Queue for a compliment; html=\n{html}"
    );
}

/// req_pass2_12: the hub SURFACES the one-rule-one-surface violation (§8: "the UI should
/// assert it rather than defend against it"), naming the surfaces, and stays silent on a
/// healthy rule.
#[test]
fn req_pass2_12_the_hub_surfaces_a_cross_surface_conflict_and_is_quiet_otherwise() {
    let conflicted = vec![
        row(
            "me/api",
            "DUP-1",
            "src/a.rs",
            serde_json::json!({"kind": "defect"}),
        ),
        row(
            "me/api",
            "DUP-1",
            "-",
            serde_json::json!({
                "kind": "convention_observed", "convention_option_id": "o",
                "convention_label": "L"
            }),
        ),
    ];
    let html = strip_html(rule_hub(&conflicted, "me/api", "DUP-1").expect("present"));
    assert!(
        html.contains("rule-hub-conflict"),
        "the conflict is rendered; html=\n{html}"
    );
    assert!(html.contains("Queue"), "names the surfaces; html=\n{html}");
    assert!(
        html.contains("Conventions"),
        "names the surfaces; html=\n{html}"
    );

    let clean = vec![row(
        "me/api",
        "OK-1",
        "src/a.rs",
        serde_json::json!({"kind": "defect"}),
    )];
    let html = strip_html(rule_hub(&clean, "me/api", "OK-1").expect("present"));
    assert!(
        !html.contains("rule-hub-conflict"),
        "a healthy rule must not ring the alarm; html=\n{html}"
    );
}

/// req_pass2_12: the kind-less ALARM keeps its identity inside the hub. The hub is the
/// surface a curator reaches for when a row confuses him, so it is the LAST place an
/// unmapped emission path should be quietly described as a defect.
#[test]
fn req_pass2_12_the_hub_keeps_an_unmapped_row_an_alarm() {
    let rows = vec![row("me/api", "LEGACY-1", "src/a.rs", serde_json::json!({}))];
    let html = strip_html(rule_hub(&rows, "me/api", "LEGACY-1").expect("present"));
    assert!(
        html.contains("Unspecified"),
        "the alarm chip; html=\n{html}"
    );
    assert!(
        html.contains("unmapped emission path"),
        "and the sentence says what it means; html=\n{html}"
    );
    assert!(
        !html.contains("Defect"),
        "never dressed as a defect; html=\n{html}"
    );
}

// ── REACHABILITY: §8's "from every surface that shows a rule id" ─────────
//
// The hub is only a hub if a curator can get to it from wherever he is holding the rule
// id. These render each such surface and assert the rule id is the door — a
// `rule-hub-link` control carrying that exact id — rather than inert text.

fn panel_html(which: KindSurface, findings: Vec<FindingView>) -> String {
    #[component]
    fn Harness(which: KindSurface, findings: Vec<FindingView>) -> Element {
        let cb = Callback::new(|_: RuleHubRequest| {});
        match which {
            KindSurface::Conventions => rsx! { ConventionsPanel { findings, on_open_hub: cb } },
            KindSurface::NoConvention => {
                rsx! { NoConventionPanel { findings, on_open_hub: cb } }
            }
            _ => rsx! { NotAssessedPanel { findings, on_open_hub: cb } },
        }
    }
    let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { which, findings });
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

#[test]
fn req_pass2_12_every_informational_panel_makes_its_rule_ids_the_door_to_the_hub() {
    let conventions = panel_html(
        KindSurface::Conventions,
        vec![row(
            "me/api",
            "SQL-PARAM-1",
            "-",
            serde_json::json!({
                "kind": "convention_observed", "convention_option_id": "p",
                "convention_label": "Parameterized queries"
            }),
        )],
    );
    let no_convention = panel_html(
        KindSurface::NoConvention,
        vec![row(
            "me/api",
            "SPLIT-1",
            "-",
            serde_json::json!({
                "kind": "no_convention",
                "observed_options": [{"option_id": "a", "label": "A", "sites": 2}]
            }),
        )],
    );
    let not_assessed = panel_html(
        KindSurface::NotAssessed,
        vec![row(
            "me/api",
            "NA-1",
            "-",
            serde_json::json!({
                "kind": "not_assessed", "reason": "tool_failed",
                "reason_detail": "eslint exited 2"
            }),
        )],
    );
    for (name, html, rule) in [
        ("Conventions", &conventions, "SQL-PARAM-1"),
        ("No convention", &no_convention, "SPLIT-1"),
        ("Not assessed", &not_assessed, "NA-1"),
    ] {
        assert!(
            html.contains("rule-hub-link"),
            "{name} panel must make its rule id the hub's door; html=\n{html}"
        );
        assert!(
            html.contains(rule),
            "{name} panel must name the rule on that door; html=\n{html}"
        );
    }
}

/// req_pass2_12: the DETAIL PANE's rule id is the hub's door — which is how a QUEUE ROW
/// reaches the hub. A chorale cell renderer is `Send + Sync` and a Dioxus `Callback` is
/// not, so the row's own "Finding type" cell cannot carry the click; the pane opens on
/// the same click that focuses the row, so the hub is one further click from any row.
#[test]
fn req_pass2_12_the_detail_pane_is_how_a_queue_row_reaches_the_hub() {
    let f = row(
        "me/api",
        "SQL-INJECTION-1",
        "src/a.rs",
        serde_json::json!({"kind": "defect"}),
    );
    let pane_row = DetailPaneRow::from_finding(&f, None, None, 0, 1);
    #[component]
    fn Harness(row: DetailPaneRow) -> Element {
        rsx! {
            FindingDetailPane {
                row: Some(row),
                focus: PaneFocus::Cursor,
                prompt: None,
                prompt_reason: String::new(),
                undo: None,
                on_key: Callback::new(|_: (String, KeyMods)| {}),
                on_focus: Callback::new(|_: PaneFocus| {}),
                on_prompt_reason: Callback::new(|_: String| {}),
                on_prompt_commit: Callback::new(|()| {}),
                on_prompt_cancel: Callback::new(|()| {}),
                on_undo: Callback::new(|()| {}),
                on_dispose: Callback::new(|_: TriageState| {}),
                on_open_detail: Callback::new(|()| {}),
                on_open_hub: Callback::new(|()| {}),
            }
        }
    }
    let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { row: pane_row });
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("rule-hub-link"),
        "the pane's rule id must be the hub's door; html=\n{html}"
    );
    assert!(
        html.contains("SQL-INJECTION-1"),
        "naming the rule; html=\n{html}"
    );
}

/// req_pass2_12: the DEVIATION BULK BAR's rule id is the hub's door — which is how a
/// deviation GROUP HEADER reaches it. chorale's real group-header `<tr>` takes no host
/// content (step 2 established this), and the bar is the header's reachable stand-in.
#[test]
fn req_pass2_12_the_bulk_bar_is_how_a_deviation_group_header_reaches_the_hub() {
    #[component]
    fn Harness(groups: Vec<DeviationBulkRow>) -> Element {
        rsx! {
            DeviationBulkBar {
                groups,
                on_confirm_all: Callback::new(|_: usize| {}),
                on_false_positive_all: Callback::new(|_: usize| {}),
                on_open_hub: Callback::new(|_: usize| {}),
            }
        }
    }
    let groups = deviation_groups(&deviation_rows());
    let rows: Vec<DeviationBulkRow> = groups.iter().map(DeviationBulkRow::from_group).collect();
    let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { groups: rows });
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("rule-hub-link"),
        "the bar's rule id must be the hub's door; html=\n{html}"
    );
    assert!(
        html.contains("API-VERSIONING-1"),
        "naming the rule; html=\n{html}"
    );
}

/// req_pass2_12: §8 names three navigation paths and says the rest are "a page of
/// plumbing for a navigation the curator has never needed". This pins the refusal: the
/// hub's ONE control is its jump, and in particular there is no sibling-deviation link,
/// no tool-log link and no similar-findings link.
#[test]
fn req_pass2_12_the_hub_builds_exactly_one_navigation_and_no_link_graph() {
    for rows in [
        deviation_rows(),
        vec![row(
            "me/api",
            "D-1",
            "src/a.rs",
            serde_json::json!({"kind": "defect"}),
        )],
        vec![row(
            "me/api",
            "C-1",
            "-",
            serde_json::json!({
                "kind": "convention_observed", "convention_option_id": "o",
                "convention_label": "L"
            }),
        )],
        vec![row(
            "me/api",
            "N-1",
            "-",
            serde_json::json!({"kind": "not_assessed", "reason": "no_files_matched", "reason_detail": ""}),
        )],
    ] {
        let rule_id = rows[0].rule_id.clone();
        let html = strip_html(rule_hub(&rows, "me/api", &rule_id).expect("present"));
        assert_eq!(
            html.matches("<button").count(),
            1,
            "{rule_id}: one jump and nothing else; html=\n{html}"
        );
        for forbidden in ["sibling", "tool log", "similar", "conforming endpoints"] {
            assert!(
                !html.to_lowercase().contains(forbidden),
                "{rule_id}: §8 explicitly does not build a {forbidden} path; html=\n{html}"
            );
        }
    }
}

// ── the convention model on the strip (design 2026-11-20, section 11, tests 22 to 24, 28) ──

fn convention_record(places_recorded: bool) -> camerata_api_types::audit::RuleConvention {
    use camerata_api_types::audit::{
        CensusPlace, FinderRowRef, OptionBasis, OptionCensus, PlaceStatus, RuleConvention,
        StandardSource, StandardState,
    };
    let place = |path: &str, line: usize, status: PlaceStatus| CensusPlace {
        path: path.into(),
        line,
        text: format!("code at {path}"),
        checked: true,
        status,
    };
    RuleConvention {
        repo: "me/api".into(),
        rule_id: "ARCH-PAR-1".into(),
        default_option_id: Some("par".into()),
        judged_option_id: Some("par".into()),
        observed_option_id: Some("seq".into()),
        assessed_places: Some(5),
        places_recorded,
        options: vec![
            OptionCensus {
                option_id: "par".into(),
                label: "Run in parallel".into(),
                count: Some(1),
                places: vec![place("src/p.ts", 3, PlaceStatus::FollowsRuleDefault)],
                ..OptionCensus::default()
            },
            OptionCensus {
                option_id: "seq".into(),
                label: "Sequential".into(),
                count: Some(4),
                places: vec![place("src/s.ts", 7, PlaceStatus::Conforms)],
                ..OptionCensus::default()
            },
        ],
        standard: StandardState {
            option_id: Some("seq".into()),
            source: StandardSource::Observed,
            differs_from_default: Some("Run in parallel".into()),
            ..StandardState::default()
        },
        finder_rows: vec![FinderRowRef {
            path: "src/f.ts".into(),
            line: 9,
            note: "N+1 queries".into(),
            option_basis: OptionBasis::OnlyOther,
            option_id: Some("seq".into()),
            status: PlaceStatus::Conforms,
            ..FinderRowRef::default()
        }],
    }
}

fn model_html(record: camerata_api_types::audit::RuleConvention, rulings: bool) -> String {
    #[component]
    fn Harness(hub: RuleHubView, rulings: bool) -> Element {
        let on_ruling: Option<Callback<RulingAction>> =
            rulings.then(|| Callback::new(|_: RulingAction| {}));
        rsx! {
            RuleHubStrip { hub, on_jump: Callback::new(|_: KindSurface| {}), on_ruling }
        }
    }
    let hub = camerata_ui_core::triage::rule_hub_with_conventions(
        &[],
        std::slice::from_ref(&record),
        "me/api",
        "ARCH-PAR-1",
    )
    .expect("a rule with a record has a hub");
    let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { hub, rulings });
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

/// Test 22: the overlay lists the places by status and each one is a control that opens it; the
/// finder's note is shown; the standard's origin is never "AI recommended" (test 25).
#[test]
fn the_hub_lists_conforming_places_and_each_is_an_open_control() {
    let html = model_html(convention_record(true), false);
    assert!(html.contains("Observed: 4 of 5 places follow"), "{html}");
    assert!(html.contains("Standard: observed in the code"), "{html}");
    assert!(
        html.contains("Differs from the rule&#39;s default: Run in parallel")
            || html.contains("Differs from the rule's default: Run in parallel"),
        "{html}"
    );
    for place in ["src/s.ts:7", "src/p.ts:3", "src/f.ts:9"] {
        assert!(html.contains(place), "{place}: {html}");
    }
    assert!(html.contains("N+1 queries"), "{html}");
    assert!(
        html.contains("Follows the standard") && html.contains("Follows the rule"),
        "{html}"
    );
    // The jump, plus one open link for each of the three places: no ruling controls without a
    // handler.
    assert_eq!(html.matches("<button").count(), 1 + 3, "{html}");
    assert!(!html.contains("AI recommended"), "{html}");
    assert!(!html.contains("The places were not recorded"), "{html}");
}

/// Tests 23 and 24: Accept, the other options, "No recommendation, leave it to the client" with
/// its effect text, and Withdraw once a ruling is live.
#[test]
fn the_hub_offers_accept_the_other_options_and_leave_it_to_the_client() {
    let html = model_html(convention_record(true), true);
    assert!(html.contains("Accept: Sequential"), "{html}");
    assert!(
        html.contains("Choose another option") && html.contains("Run in parallel"),
        "{html}"
    );
    assert!(
        html.contains("Sets this option as the convention for this inspection."),
        "{html}"
    );
    assert!(
        html.contains("No recommendation, leave it to the client"),
        "{html}"
    );
    assert!(
        html.contains(
            "The report lists the options and the places that follow each, with none suggested."
        ),
        "{html}"
    );
    assert!(!html.contains("Leave as reported"), "{html}");
    assert!(!html.contains("Withdraw"), "{html}");
    let mut live = convention_record(true);
    live.standard.source = camerata_api_types::audit::StandardSource::Reviewer;
    live.standard.ruling_id = Some("std-000001".into());
    let html = model_html(live, true);
    assert!(html.contains("Withdraw"), "{html}");
    assert!(html.contains("Standard: set by you"), "{html}");
    assert!(!html.contains("Accept: Sequential"), "{html}");
}

/// Test 28: a report with no recorded places says so.
#[test]
fn the_hub_of_an_old_report_says_the_places_were_not_recorded() {
    let html = model_html(convention_record(false), false);
    assert!(
        html.contains("The places were not recorded for this scan."),
        "{html}"
    );
}

/// Test 25 (the badge): a rule with a convention record never reads "AI recommended"; each origin
/// has its own words.
#[test]
fn the_badge_of_a_rule_with_a_record_never_says_ai_recommended() {
    use camerata_api_types::audit::StandardSource;
    let mut c = convention_record(true);
    let (_, observed) = super::rescan::convention_badge(&c);
    assert_eq!(observed, "Observed in the code");
    c.standard.source = StandardSource::Reviewer;
    assert_eq!(super::rescan::convention_badge(&c).1, "Set by you");
    c.standard = camerata_api_types::audit::StandardState {
        left_to_client: true,
        ..Default::default()
    };
    assert_eq!(super::rescan::convention_badge(&c).1, "Left to the client");
    c.standard = Default::default();
    assert_eq!(
        super::rescan::convention_badge(&c).1,
        "Proposal, no convention yet"
    );
}
