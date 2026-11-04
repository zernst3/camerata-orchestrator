//! Render tests: confirmed and removals.

use super::*;

// ── Pass 2 step 3: `Confirmed` on the curator surface ───────────────────

/// req_pass2_10: the rule-level bar offers BOTH directions now, over the same group.
///
/// Step 2 shipped it with only `False positive all N` because no disposition honestly
/// meant "confirmed". This is the half that was withheld, rendered through the SHIPPED
/// component so the button's label and count come from the group's own `row_count`.
#[test]
fn req_pass2_10_bulk_bar_offers_confirm_all_alongside_false_positive_all() {
    #[component]
    fn BarHarness(groups: Vec<DeviationBulkRow>) -> Element {
        rsx! {
            DeviationBulkBar {
                groups,
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
    assert!(html.contains("Confirm all 2"), "html=\n{html}");
    assert!(html.contains("Confirm all 3"), "html=\n{html}");
    // Both directions on every group, and the counts agree with the false-positive
    // button beside them: one group row, one row set, two readings of it.
    assert!(html.contains("False positive all 2"), "html=\n{html}");
    assert!(html.contains("False positive all 3"), "html=\n{html}");
    assert_eq!(
        html.matches("deviation-bulk-confirm").count(),
        2,
        "one Confirm control per group; html=\n{html}"
    );
}

/// req_pass2_10: a Confirm button exists in the Unresolved toolbar, and it is the only
/// action there with no reason input of its own.
///
/// EDITED by the §9.2 removal, and this is one of that sweep's two named exceptions. The
/// assertion used to be `html.matches("ignore-reason").count() == 2` — "Confirm adds no
/// third reason input" — which counted the two always-visible textareas §9.2 deletes. The
/// test's INTENT (Confirm is the action that needs no justification) survives the removal
/// intact and is now asserted in a form the removal makes STRICTER rather than looser:
/// the resting toolbar has ZERO standing reason inputs, not two, and Confirm is still the
/// control that commits without raising one. Rewriting the count from 2 to 0 and stopping
/// there would have left the test passing while asserting almost nothing, so the
/// Ignore/False-positive side is pinned here too: both are present, and neither is a
/// textarea.
#[test]
fn req_pass2_10_unresolved_toolbar_offers_confirm_with_no_reason_input() {
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
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("triage-confirm"),
        "the Unresolved toolbar must offer Confirm; html=\n{html}"
    );
    // §9.2: the resting toolbar holds NO reason input at all. Confirm never needed one;
    // Ignore and False positive now raise theirs on demand.
    assert_eq!(
        html.matches("ignore-reason").count(),
        0,
        "§9.2 removes the always-visible reason textareas; html=\n{html}"
    );
    assert!(
        !html.contains("<textarea"),
        "and nothing else quietly took their place; html=\n{html}"
    );
    // The two actions that DO need a reason are still offered — the removal took the
    // boxes, not the decisions.
    assert!(html.contains("triage-ignore"), "Ignore; html=\n{html}");
    assert!(
        html.contains("triage-false-positive"),
        "Mark false positive; html=\n{html}"
    );
}

/// req_pass2_10: the Confirmed table has exactly one way out and no reason inputs.
///
/// Mirrors the FalsePositive table. The reason textareas live only in the Unresolved
/// toolbar, so offering Ignore or Tech debt here would be a control that cannot complete.
#[test]
fn req_pass2_10_confirmed_table_offers_only_the_way_back() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let mut dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        use_hook(move || {
            let mut model = TriageModel::new();
            model.confirm(&deviation_queue_rows());
            dispositions.set(model.dispositions);
        });
        rsx! {
            FindingsTable {
                findings: deviation_queue_rows(),
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Confirmed,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Move to Unresolved"),
        "the confirmed rows must be reversible; html=\n{html}"
    );
    assert!(
        !html.contains("ignore-reason"),
        "no reason inputs in a table that cannot use them; html=\n{html}"
    );
    assert!(
        !html.contains("triage-confirm"),
        "and no Confirm button in the Confirmed table; html=\n{html}"
    );
    // The confirmed rows are actually HERE — the table is not merely empty.
    assert!(
        group_header_region(&html).contains("2 of 47 sites deviate from"),
        "the confirmed deviation rows render in this table; html=\n{html}"
    );
}

/// B.7: the progress strip is the work order's line of text, a labelled group and not a live
/// region, and draws nothing for the ETA until the review timer supplies one.
#[test]
fn b7_progress_strip_is_plain_text_with_an_empty_named_eta_seam() {
    #[component]
    fn StripHarness(counts: TriageCounts, eta: Option<String>) -> Element {
        rsx! { ProgressStrip { counts, eta } }
    }
    let render = |eta: Option<String>| {
        let counts = TriageCounts {
            unresolved: 187,
            confirmed: 310,
            ignored: 71,
            false_positive: 29,
            tech_debt: 15,
            ..TriageCounts::default()
        };
        let mut vdom = VirtualDom::new_with_props(StripHarness, StripHarnessProps { counts, eta });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    };
    let html = render(None);
    assert!(
            html.contains(
                "Undecided 187 of 612 \u{b7} Confirmed 310 \u{b7} Ignored 71 \u{b7} FP 29 \u{b7} Tech debt 15"
            ),
            "{html}"
        );
    assert!(html.contains("role=\"group\"") && html.contains("aria-label=\"Review progress\""));
    assert!(!html.contains("aria-live"), "not a live region: {html}");
    assert!(!html.contains("progress-eta"), "no ETA is invented: {html}");
    assert!(!html.contains('\u{2014}'));
    // The seam works the day the timer exists.
    let with = render(Some("about 3h 10m left at your pace".to_string()));
    assert!(
        with.contains("progress-eta") && with.contains("about 3h 10m left"),
        "{with}"
    );
}

/// req_pass2_10 (THE PROCESS GATE, rendered): a Queue of confirmed findings does not
/// block Process, and the gate says why the button is still disabled.
///
/// Step 1's build note recorded that the gate itself was never render-tested, because it
/// lives inside `ScanResults` (behind `use_resource`, not SSR-testable). `ProcessGate` is
/// the extraction that closes that: every state below is the real component, rendered.
#[test]
fn req_pass2_10_process_gate_treats_confirmed_as_resolved() {
    #[component]
    fn GateHarness(counts: TriageCounts) -> Element {
        rsx! {
            ProcessGate {
                counts,
                processing: false,
                on_process: Callback::new(move |()| {}),
            }
        }
    }
    let render = |counts: TriageCounts| {
        let mut vdom = VirtualDom::new_with_props(GateHarness, GateHarnessProps { counts });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    };

    // (1) Two rows nobody has decided: blocked, and the hint NAMES Confirm as an exit.
    // Before this step it offered only "Ignore or save as Tech debt".
    let html = render(TriageCounts {
        unresolved: 2,
        ..Default::default()
    });
    assert!(html.contains("Resolve the 2 remaining"), "html=\n{html}");
    assert!(
        html.contains("Confirm"),
        "the hint must name the exit the curator reaches for most; html=\n{html}"
    );
    assert!(html.contains("disabled"), "html=\n{html}");

    // (2) Everything CONFIRMED: nothing outstanding, so the blocking hint is gone. The
    // button stays disabled because Confirmed commits nothing — and the gate says so
    // instead of leaving a dead control unexplained.
    let html = render(TriageCounts {
        confirmed: 2,
        ..Default::default()
    });
    assert!(
        !html.contains("remaining Unresolved"),
        "a confirmed finding is decided, not outstanding; html=\n{html}"
    );
    assert!(html.contains("Nothing to process"), "html=\n{html}");
    assert!(html.contains("disabled"), "html=\n{html}");

    // (3) Confirmed + ignored: nothing outstanding AND something to commit. Enabled.
    let html = render(TriageCounts {
        confirmed: 1,
        ignored: 1,
        ..Default::default()
    });
    assert!(!html.contains("remaining Unresolved"), "html=\n{html}");
    assert!(!html.contains("Nothing to process"), "html=\n{html}");
    assert!(
        !html.contains("disabled"),
        "nothing outstanding and a baseline waiver to commit; html=\n{html}"
    );

    // (4) One row still unresolved alongside a confirmed one: blocked by that row alone.
    let html = render(TriageCounts {
        unresolved: 1,
        confirmed: 1,
        ignored: 1,
        ..Default::default()
    });
    assert!(html.contains("Resolve the 1 remaining"), "html=\n{html}");
    assert!(html.contains("disabled"), "html=\n{html}");
}
// ── req_pass2_13: THE REMOVALS SWEEP (design §9) ─────────────────────────
//
// "Removal is the design work." Each test below pins BOTH halves of one removal: the
// affordance is gone, AND the replacement reaches the information it carried. A removal
// whose replacement falls short is a capability loss dressed as cleanup.

/// req_pass2_13 (§9.5): the two-swatch findings-key legend is GONE from the Queue, and the
/// red full-row critical highlight it claimed to explain is still there.
///
/// The legend is not replaced by silence. It made two claims and the per-row columns make
/// both, more precisely:
///
/// * "red = Security findings (deterministic, stop-the-line)" → the SEVERITY badge, which
///   reads `Critical` on exactly the rows `finding_row_class` highlights. The legend's own
///   wording was wrong about this: the highlight has never consulted the floor rule set.
/// * "Architectural findings (everything else)" → the AUTHORITY badge, which distinguishes
///   `Rule · enforced` (deterministic floor) from `AI · advisory` and `Preview`, per row,
///   rather than lumping three tiers into one "everything else".
#[test]
fn req_pass2_13_the_findings_key_legend_is_gone_and_the_critical_highlight_stays() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: vec![
                    kinded_finding("SQL-INJECTION-1", "critical", serde_json::json!({ "kind": "defect" })),
                    kinded_finding("AI-LAYERING", "medium", serde_json::json!({ "kind": "defect" })),
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
    for gone in [
        "findings-key",
        "Security findings (deterministic, stop-the-line)",
        "Architectural findings (everything else)",
    ] {
        assert!(!html.contains(gone), "§9.5 removes {gone:?}; html=\n{html}");
    }
    // The highlight STAYS, and on exactly the severity the legend's swatch was red for.
    let crit = kinded_finding("SQL-INJECTION-1", "critical", serde_json::json!({}));
    let not_crit = kinded_finding("AI-LAYERING", "high", serde_json::json!({}));
    assert_eq!(
        finding_row_class(&crit),
        Some("finding-row-critical".to_string()),
        "§9.5: the red full-row critical highlight stays"
    );
    assert_eq!(finding_row_class(&not_crit), None);

    // And the replacement columns are present and carry the two facts the legend glossed.
    let cols = finding_columns(vec!["me/api".to_string()], false);
    let sev = cols
        .iter()
        .find(|c| c.id == ColumnId("severity"))
        .expect("Severity column");
    assert_eq!(
        (sev.accessor)(&crit),
        CellValue::Text(severity_sort_value("critical")),
        "the Severity column marks exactly the highlighted rows"
    );
    let auth = cols
        .iter()
        .find(|c| c.id == ColumnId("authority"))
        .expect("Authority column");
    let RenderKind::Badge(map) = &auth.render_kind else {
        panic!("Authority renders as a badge");
    };
    for (value, label) in [
        ("enforced", "Rule \u{b7} enforced"),
        ("advisory", "AI \u{b7} advisory"),
    ] {
        assert_eq!(
            map.resolve(value).expect("authority variant").label,
            label,
            "the Authority column, not a legend, is what separates deterministic from advisory"
        );
    }
    assert!(
        html.contains(">Severity<"),
        "Severity header; html=\n{html}"
    );
    assert!(
        html.contains(">Authority<"),
        "Authority header; html=\n{html}"
    );
}

/// req_pass2_13 (§9.1, the half that MOVED): the hub carries the Evaluated-against line
/// and the Why? answer, and carries them as the authored LABEL rather than the raw option
/// id it used to show.
///
/// It also adds NO control while doing so — asserted here as well as in the §8 tests,
/// because this is the change that could have broken "the hub builds exactly one
/// navigation and no link graph" without any of those tests noticing: they render the
/// strip with `alternatives` defaulted to `None`, so they cannot see this path.
#[test]
fn req_pass2_13_the_hub_carries_the_evaluated_against_line_and_the_why_answer() {
    #[component]
    fn Harness(hub: RuleHubView, alternatives: Option<AlternativeControlsRow>) -> Element {
        rsx! {
            RuleHubStrip {
                hub,
                on_jump: Callback::new(|_: KindSurface| {}),
                alternatives,
            }
        }
    }
    let render = |hub: RuleHubView, alternatives: Option<AlternativeControlsRow>| {
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { hub, alternatives });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    };

    let mut row = kinded_finding(
        "API-VERSIONING-1",
        "medium",
        serde_json::json!({
            "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
            "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
        }),
    );
    row.evaluated_option_id = Some("versioned".to_string());
    let hub = rule_hub(std::slice::from_ref(&row), "me/api", "API-VERSIONING-1")
        .expect("the rule is in the scan");

    // WITHOUT the alternatives context (a rule the audit recorded no recommendation for):
    // unchanged behaviour, the raw option id tag.
    let bare = render(hub.clone(), None);
    assert!(
        bare.contains("evaluated against"),
        "the pre-existing tag survives for a rule with no recommendation; html=\n{bare}"
    );
    assert!(bare.contains("versioned"), "html=\n{bare}");

    // WITH it: the LABEL, the provenance, the evidence, and the reasoning the panel's
    // Why? button hides behind a click.
    let rule = proposed_rule_with_options(
        "API-VERSIONING-1",
        "Version the API",
        &[
            ("versioned", "URI-versioned endpoints"),
            ("header", "Header-versioned"),
        ],
    );
    let rec = recommendation_with_evidence(
        "versioned",
        "Thirty-one of forty-seven endpoints already carry a /v1 prefix.",
        Some("src/routes.rs:12"),
        true,
    );
    let alt = alternative_controls_row(&rule, &rec, 2, Some("versioned"), None, false);
    let html = render(hub.clone(), Some(alt));
    assert!(html.contains("Evaluated against:"), "html=\n{html}");
    assert!(
        html.contains("URI-versioned endpoints"),
        "the authored LABEL, which is what the curator reads elsewhere; html=\n{html}"
    );
    assert!(
        html.contains("AI recommended"),
        "the provenance badge; html=\n{html}"
    );
    assert!(html.contains("Evidence: src/routes.rs:12"), "html=\n{html}");
    assert!(
        html.contains("Thirty-one of forty-seven endpoints"),
        "the Why? ANSWER, rendered rather than hidden behind a modal inside a modal; \
             html=\n{html}"
    );
    assert!(
        !html.contains("rule-hub-evaluated"),
        "and NOT the raw option-id tag as well — one fact, one vocabulary; html=\n{html}"
    );
    // Where the control actually is, named. The hub cannot carry the Change select (see
    // `RuleAlternativesPanel`), so it must not leave the curator hunting for it.
    assert!(html.contains("Rule alternatives"), "html=\n{html}");

    // THE §8 INVARIANT SURVIVES THE ADDITION: still exactly one control, the jump.
    assert_eq!(
        html.matches("<button").count(),
        1,
        "the alternatives context is READ-ONLY; §8's one-navigation rule holds; html=\n{html}"
    );
}

/// req_pass2_13 (§9.1, the half that could NOT move): `RuleAlternativesPanel` still lists
/// a multi-option rule the scan found NOTHING for, and the hub still cannot.
///
/// This is the coverage proof that blocks the removal, asserted rather than asserted-in-
/// prose. A clean multi-option rule has no Queue row and no panel row anywhere else; if
/// the panel went, its option choice would be unreachable and unchangeable. `rule_hub`
/// returning `None` for it is deliberate (its own doc: "there is no '0 findings' hub"),
/// so this is a property of the replacement, not a gap to be patched.
#[test]
fn req_pass2_13_the_hub_cannot_reach_a_clean_multi_option_rule_so_the_panel_stays() {
    // A scan whose findings touch ONE rule, while the audit recommended on TWO.
    let found = kinded_finding(
        "API-VERSIONING-1",
        "medium",
        serde_json::json!({"kind": "defect"}),
    );
    let findings = vec![found];

    assert!(
        rule_hub(&findings, "me/api", "ERROR-HANDLING-1").is_none(),
        "the hub describes what a scan FOUND; a clean rule has nothing for it to describe"
    );

    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
        let audit = use_signal(|| Option::<ScanReportView>::None);
        let mut recs = std::collections::HashMap::new();
        recs.insert(
            "API-VERSIONING-1".to_string(),
            recommendation("versioned", "most endpoints already do", false, false),
        );
        recs.insert(
            "ERROR-HANDLING-1".to_string(),
            recommendation("result", "no call sites panic today", false, false),
        );
        rsx! {
            RuleAlternativesPanel {
                project_id: "p".to_string(),
                proposed_rules: vec![
                    proposed_rule_with_options(
                        "API-VERSIONING-1",
                        "Version the API",
                        &[("versioned", "URI-versioned endpoints"), ("header", "Header-versioned")],
                    ),
                    proposed_rule_with_options(
                        "ERROR-HANDLING-1",
                        "Handle errors consistently",
                        &[("result", "Result types"), ("panic", "Panics")],
                    ),
                ],
                findings: vec![kinded_finding(
                    "API-VERSIONING-1",
                    "medium",
                    serde_json::json!({"kind": "defect"}),
                )],
                repos: vec!["me/api".to_string()],
                recommendations: recs,
                audit,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("ERROR-HANDLING-1"),
        "the CLEAN rule must still be listed and overridable — it is the one whose option \
             choice is least visible; html=\n{html}"
    );
    assert!(
        html.contains("0 findings"),
        "and labelled honestly; html=\n{html}"
    );
    assert!(
        html.contains("Result types"),
        "with its Change select populated; html=\n{html}"
    );
    assert!(
        html.contains("API-VERSIONING-1"),
        "the rule WITH findings is listed too, because this is still where the Change \
             select lives for every rule; html=\n{html}"
    );
}

/// req_pass2_13 (§9.1, precondition): `alternative_controls_row` resolves "Evaluated
/// against" in the one priority order the product means, and nothing else re-derives it.
///
/// This order used to live inline in `RuleAlternativesPanel`'s render body, where it was
/// unreachable by any test. §9.1 moves this line onto other surfaces, and a line whose
/// central judgement can only be produced inside one component cannot move — so the
/// extraction is a precondition of the removal, and the order is pinned here before
/// anything starts depending on it from a second place.
#[test]
fn req_pass2_13_the_alternatives_line_resolves_one_evaluated_option_in_one_order() {
    let rule = proposed_rule_with_options(
        "API-VERSIONING-1",
        "Version the API",
        &[
            ("versioned", "URI-versioned endpoints"),
            ("header", "Header-versioned"),
        ],
    );
    let rec = recommendation("versioned", "most endpoints already do", false, false);

    // 1. A STAGED OVERRIDE wins everything: it is what the next rescan will use, and the
    //    curator has to see what he just picked.
    let row = alternative_controls_row(&rule, &rec, 3, Some("versioned"), Some("header"), false);
    assert_eq!(row.evaluated_option_id, "header");
    assert_eq!(row.evaluated_label, "Header-versioned");
    assert!(row.staged);
    assert_eq!(row.badge_text, "Override, pending rescan");

    // 2. No override: the option the FINDINGS were produced under beats the
    //    recommendation. A prior rescan can have applied an override the recommendation
    //    field knows nothing about, and showing the recommendation then would describe a
    //    scan the curator is not looking at.
    let row = alternative_controls_row(&rule, &rec, 3, Some("header"), None, false);
    assert_eq!(row.evaluated_option_id, "header");
    assert!(!row.staged);

    // 3. No findings at all (the case the standalone panel exists for): the AI's pick.
    let row = alternative_controls_row(&rule, &rec, 0, None, None, false);
    assert_eq!(row.evaluated_option_id, "versioned");
    assert_eq!(row.badge_text, "AI recommended");
    assert_eq!(row.finding_count, 0);

    // 4. No findings and no recommendation: the rule's declared default.
    let mut defaulted = rule.clone();
    defaulted.default_option = Some("header".to_string());
    let empty = recommendation("", "", false, false);
    let row = alternative_controls_row(&defaulted, &empty, 0, None, None, false);
    assert_eq!(row.evaluated_option_id, "header");

    // 5. Nothing anywhere: the label says so rather than rendering a blank that reads as
    //    "no opinion".
    let row = alternative_controls_row(&rule, &empty, 0, None, None, false);
    assert_eq!(row.evaluated_label, "(none)");

    // 6. The operator's own prior choice is labelled as his, not as the AI's.
    let chosen = recommendation("versioned", "operator forced", false, true);
    let row = alternative_controls_row(&rule, &chosen, 1, Some("versioned"), None, false);
    assert_eq!(row.badge_text, "Your choice");

    // 7. Accepted is ADDITIVE, never a replacement for the provenance badge: "I accepted
    //    it" and "the AI picked it" are both true and the curator needs both.
    let row = alternative_controls_row(&rule, &rec, 1, Some("versioned"), None, true);
    assert!(row.accepted);
    assert_eq!(row.badge_text, "AI recommended");
}

/// req_pass2_13 (§9.1, precondition): the control line renders every state, including the
/// two a click would be needed to reach through the panel.
#[test]
fn req_pass2_13_the_alternatives_line_renders_every_state_props_only() {
    #[component]
    fn Harness(row: AlternativeControlsRow) -> Element {
        rsx! {
            RuleAlternativeControls {
                row,
                on_why: Callback::new(|()| {}),
                on_pick: Callback::new(|_: String| {}),
            }
        }
    }
    let render = |row: AlternativeControlsRow| {
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { row });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    };
    let rule = proposed_rule_with_options(
        "API-VERSIONING-1",
        "Version the API",
        &[
            ("versioned", "URI-versioned endpoints"),
            ("header", "Header-versioned"),
        ],
    );

    // The resting state: the fact, the provenance, the Why? door, the Change select.
    let rec = recommendation("versioned", "most endpoints already do", false, false);
    let html = render(alternative_controls_row(
        &rule,
        &rec,
        3,
        Some("versioned"),
        None,
        false,
    ));
    assert!(html.contains("Evaluated against:"), "html=\n{html}");
    assert!(
        html.contains("URI-versioned endpoints"),
        "the LABEL, not the id; html=\n{html}"
    );
    assert!(html.contains("AI recommended"), "html=\n{html}");
    assert!(html.contains("rec-why"), "the Why? door; html=\n{html}");
    assert!(
        html.contains("rec-select"),
        "the Change select; html=\n{html}"
    );
    assert!(html.contains("3 finding(s)"), "html=\n{html}");
    assert!(!html.contains("rec-badge-accepted"), "html=\n{html}");
    assert!(!html.contains("Not applicable"), "html=\n{html}");

    // STAGED — reachable through the panel only by driving a real <select> change, which
    // a Tier-1 SSR test cannot do. Props-only is what makes it assertable.
    let html = render(alternative_controls_row(
        &rule,
        &rec,
        3,
        Some("versioned"),
        Some("header"),
        false,
    ));
    assert!(html.contains("Override, pending rescan"), "html=\n{html}");
    assert!(html.contains("rec-badge-pending"), "html=\n{html}");

    // ACCEPTED — likewise reachable only after a successful server round trip.
    let html = render(alternative_controls_row(
        &rule,
        &rec,
        3,
        Some("versioned"),
        None,
        true,
    ));
    assert!(html.contains("rec-badge-accepted"), "html=\n{html}");
    assert!(
        html.contains("AI recommended"),
        "both, not one; html=\n{html}"
    );

    // Evidence and the not-applicable marker, together.
    let flagged = recommendation_with_evidence("versioned", "r", Some("src/routes.rs:12"), false);
    let html = render(alternative_controls_row(
        &rule, &flagged, 0, None, None, false,
    ));
    assert!(html.contains("Evidence: src/routes.rs:12"), "html=\n{html}");
    assert!(
        html.contains("Not applicable to this repo"),
        "html=\n{html}"
    );
    assert!(
        html.contains("0 findings"),
        "a clean multi-option rule still renders; html=\n{html}"
    );
}

/// A twelve-row deviation group, for the bulk prompt's heading. Twelve so the count in
/// the heading cannot be coincidentally right.
fn twelve_row_group() -> DeviationGroup {
    let base = deviation_queue_rows();
    let dev = base
        .iter()
        .find(|f| f.rule_id == "API-VERSIONING-1")
        .expect("a deviation row")
        .clone();
    let rows: Vec<FindingView> = (0..12)
        .map(|i| {
            let mut f = dev.clone();
            f.line = 100 + i;
            f
        })
        .collect();
    deviation_groups(&rows)
        .into_iter()
        .next()
        .expect("one group")
}

/// req_pass2_13 (§9.2): the Queue's on-demand reason prompt, in the two shapes the two
/// removed textareas used to serve, NAMING THE COUNT in each.
///
/// Props-only so SSR can render it open — the state a click would otherwise be needed to
/// reach. The heading is `bulk_reason_heading`'s, which is unit-tested in core; what is
/// asserted here is that the component renders THAT heading for the prompt it was handed,
/// rather than a second phrasing of the same thing.
#[test]
fn req_pass2_13_the_on_demand_reason_prompt_names_its_disposition_and_its_count() {
    #[component]
    fn PromptHarness(prompt: ToolbarReasonPrompt) -> Element {
        rsx! {
            QueueReasonPrompt {
                prompt,
                reason: String::new(),
                on_reason: Callback::new(|_: String| {}),
                on_commit: Callback::new(|()| {}),
                on_cancel: Callback::new(|()| {}),
            }
        }
    }
    let render = |prompt: ToolbarReasonPrompt| {
        let mut vdom = VirtualDom::new_with_props(PromptHarness, PromptHarnessProps { prompt });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    };

    // The per-selection shape: three rows picked in the table, raised by "Ignore".
    let html = render(ToolbarReasonPrompt {
        state: TriageState::Ignored,
        rows: deviation_queue_rows().into_iter().take(3).collect(),
    });
    assert!(
        html.contains(&bulk_reason_heading(TriageState::Ignored, 3)),
        "the heading core writes, not a second phrasing; html=\n{html}"
    );
    assert!(
        html.contains("3 findings"),
        "it names the count; html=\n{html}"
    );
    assert!(
        html.contains("Ignored"),
        "and the disposition; html=\n{html}"
    );
    assert!(
        html.contains("required"),
        "and that the reason is required; html=\n{html}"
    );
    assert!(
        html.contains("queue-prompt-commit"),
        "a way to commit; html=\n{html}"
    );
    assert!(
        html.contains("queue-prompt-cancel"),
        "and a way out; html=\n{html}"
    );

    // The bulk shape: one deviation group of twelve, raised by "False positive all 12".
    // The textarea this replaces could not have said "12" — it had no idea which button
    // was about to read it.
    let html = render(ToolbarReasonPrompt {
        state: TriageState::FalsePositive,
        rows: twelve_row_group().rows,
    });
    assert!(
        html.contains("12 findings"),
        "the group's own count; html=\n{html}"
    );
    assert!(
        html.contains("False positive"),
        "the disposition; html=\n{html}"
    );

    // Singular is singular. A prompt that says "1 findings" over a destructive act reads
    // as a template nobody checked.
    let html = render(ToolbarReasonPrompt {
        state: TriageState::FalsePositive,
        rows: deviation_queue_rows().into_iter().take(1).collect(),
    });
    assert!(html.contains("1 finding ("), "html=\n{html}");
    assert!(!html.contains("1 findings"), "html=\n{html}");

    // AND THE COUNT IS THE ROW LIST, not a number travelling beside it. This is the
    // property that makes the heading trustworthy: the prompt carries the findings,
    // so there is no second list for the commit to disagree with after a re-sort or a
    // filter lands between raising the prompt and committing it.
    let rows = deviation_queue_rows();
    let sel = ToolbarReasonPrompt {
        state: TriageState::Ignored,
        rows: rows.clone(),
    };
    assert_eq!(sel.count(), rows.len());
    let group = twelve_row_group();
    let bulk = ToolbarReasonPrompt {
        state: TriageState::FalsePositive,
        rows: group.rows.clone(),
    };
    assert_eq!(bulk.count(), group.rows.len());
    assert_eq!(
        bulk.count(),
        group.row_count(),
        "the prompt's count is the same one the bar's own button label uses"
    );
}

/// req_pass2_13 (§9.2): the Queue at rest has NO reason input, in any triage table.
///
/// The removal's whole claim is that "an empty textarea the curator is not using is noise
/// 95% of the time", so the resting state is the thing to pin — and to pin across all
/// five tables, because the old textareas lived in the Unresolved arm and their absence
/// elsewhere was previously the reason the other tables could not offer Ignore at all.
#[test]
fn req_pass2_13_no_triage_table_renders_a_standing_reason_textarea() {
    for view in TriageState::all() {
        #[component]
        fn Harness(view: TriageState) -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
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
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { view });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("<textarea"),
            "{}: §9.2 leaves no standing reason box; html=\n{html}",
            view.label()
        );
        assert!(
            !html.contains("queue-reason-prompt"),
            "{}: and the on-demand prompt is CLOSED until a gesture raises it; html=\n{html}",
            view.label()
        );
    }
}

/// req_pass2_13 (§9.2, the handled risk): the bulk bar's `False positive all N` no longer
/// reads a toolbar textarea, and its tooltip no longer promises one.
///
/// This is the one place the removal CHANGED a behaviour rather than relocating it, so it
/// is pinned explicitly. Before: the button read whatever stood in the shared
/// false-positive box — which could be blank, or could be a reason typed minutes earlier
/// for a different rule, and the bar never said how many rows it was about to move.
/// After: it raises the prompt carrying its own group's count, and the require-reason
/// invariant stays exactly where it was, in `TriageModel::dispose_with_undo`.
#[test]
fn req_pass2_13_the_bulk_false_positive_bar_raises_a_prompt_instead_of_reading_a_textarea() {
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
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("deviation-bulk-fp"),
        "the bulk false-positive control is still offered; html=\n{html}"
    );
    assert!(
        !html.contains("reason above"),
        "it must not promise a textarea that no longer exists; html=\n{html}"
    );
    assert!(
        !html.contains("<textarea"),
        "and there is no textarea for it to have read; html=\n{html}"
    );

    // The invariant it defers to is unchanged and still refuses: core's own entry point,
    // the one every bulk door now shares. Asserted here rather than taken on trust,
    // because "the reason source moved" is exactly the change that could drop it.
    let groups = deviation_groups(&deviation_queue_rows());
    let group = groups.first().expect("a deviation group");
    let mut model = TriageModel::new();
    assert!(
        model
            .dispose_with_undo(&group.rows, TriageState::FalsePositive, "   ", 0)
            .is_none(),
        "a blank reason must still be refused, wherever the box now lives"
    );
    assert!(
        model.dispositions.is_empty(),
        "and must write nothing when it refuses"
    );
    let entry = model
        .dispose_with_undo(
            &group.rows,
            TriageState::FalsePositive,
            "the parser mis-read all of these",
            0,
        )
        .expect("a real reason is accepted");
    assert_eq!(
        entry.row_count(),
        group.rows.len(),
        "and a real reason must still move the whole group"
    );
}

/// req_pass2_13 (§9.3): the Queue's hedging column is the STRUCTURED Confidence chip, and
/// the 300px prose column it replaces is gone from the table.
///
/// Coverage, item by item, against what the old column could say:
///
/// | old cell | old source | new cell | new source |
/// |---|---|---|---|
/// | `test` | `f.in_test` | `test` | `f.in_test` |
/// | `yes` | `f.needs_review` OR a regex over `f.detail` | `needs-review` | `f.needs_review` OR `f.confidence` |
/// | `no` | neither | `high` / `unrated`, told apart | `f.confidence` |
///
/// The one thing the old column rendered that this one does not is the hedging SENTENCE,
/// inline on the row. That is the removal, and the next test proves where it went.
#[test]
fn req_pass2_13_the_queue_hedging_column_is_the_structured_confidence_chip() {
    let cols = finding_columns(vec!["me/api".to_string()], false);
    let col = cols
        .iter()
        .find(|c| c.id == ColumnId("needs_review"))
        .expect("the hedging column must still exist, re-headed");
    assert_eq!(
        col.header, "Confidence",
        "§4: Confidence REPLACES \"Needs review\" as the column the curator reads"
    );
    let width = col.initial_width.expect("the column sets an initial width");
    assert!(
        width <= 160.0,
        "§9.3 recovers ~300px off the 300px column; got {width}px"
    );

    // Every state the old column could report, still reported — plus the one it could not.
    let cases = [
        (serde_json::json!({"in_test": true}), "test"),
        (serde_json::json!({"needs_review": true}), "needs-review"),
        (
            serde_json::json!({"confidence": "needs-review"}),
            "needs-review",
        ),
        (serde_json::json!({"confidence": "high"}), "high"),
        (serde_json::json!({}), "unrated"),
    ];
    for (patch, want) in &cases {
        let mut row = serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": 1, "rule_id": "R-1",
            "severity": "medium", "snippet": "s", "detail": "d"
        });
        for (k, v) in patch.as_object().expect("object") {
            row[k] = v.clone();
        }
        let f: FindingView = serde_json::from_value(row).expect("fixture");
        assert_eq!(
            (col.accessor)(&f),
            CellValue::Text((*want).to_string()),
            "cell value for {patch}"
        );
    }

    // THE PROSE TERM IS GONE from the cell value. This row's `detail` carries the legacy
    // marker and its structured verdict CONTRADICTS it; the old accessor would have read
    // `yes` off the sentence, over the top of the calibration pass's own answer.
    let contradicted: FindingView = serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": "src/a.rs", "line": 1, "rule_id": "R-1",
        "severity": "medium", "snippet": "s",
        "detail": "Debatable. [needs review: premature for a mini app]",
        "confidence": "high", "needs_review": false
    }))
    .expect("fixture");
    assert_eq!(
        (col.accessor)(&contradicted),
        CellValue::Text("high".to_string()),
        "the cell must read the structured verdict, never the sentence beside it"
    );

    // Filter vocabulary from the same closed set as the cell (the Severity discipline).
    let FilterKind::MultiSelect { options } = &col.filter else {
        panic!(
            "Confidence keeps a multi-select filter, got {:?}",
            col.filter
        );
    };
    assert_eq!(options, &confidence_tags());
    for (_, want) in &cases {
        assert!(
            options.contains(&(*want).to_string()),
            "{want} is emitted but not filterable"
        );
    }

    // And every emitted value resolves to a chip, so no state renders as a blank cell the
    // curator would misread as "nothing to say here".
    let RenderKind::Badge(map) = &col.render_kind else {
        panic!("Confidence renders as a badge");
    };
    for tag in confidence_tags() {
        let variant = map
            .resolve(&tag)
            .unwrap_or_else(|| panic!("{tag} must resolve to a badge variant"));
        assert!(
            !variant.label.is_empty(),
            "{tag}: a chip with no label is a blank cell"
        );
    }
    assert_ne!(
        map.resolve("high").expect("high").label,
        map.resolve("unrated").expect("unrated").label,
        "\"calibration was confident\" and \"calibration never looked\" must not render alike"
    );
}

/// req_pass2_13 (§9.3, the other half): the hedging SENTENCE the removed column drew
/// inline is still reaching the curator — in the detail pane, which is where §9.3 sends it
/// ("the reason text lives in the detail pane").
///
/// This is the test that makes the removal a relocation rather than a capability loss, so
/// it asserts in both directions: the reason is absent from the rendered Queue and present
/// in the pane describing that same row.
#[test]
fn req_pass2_13_the_hedging_reason_left_the_row_and_arrived_in_the_pane() {
    const REASON: &str = "premature for a mini app";
    let hedged: FindingView = serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": "src/a.rs", "line": 7, "rule_id": "AI-LAYERING",
        "severity": "medium", "snippet": "fn x() {}", "status": "active",
        "detail": format!("No mappers crate. [needs review: {REASON}]"),
        "kind": "defect", "needs_review": true, "confidence": "needs-review"
    }))
    .expect("fixture");

    // Gone from the Queue: not in any cell value, and not drawn by a renderer either.
    let cols = finding_columns(vec!["me/api".to_string()], false);
    for c in &cols {
        if let CellValue::Text(v) = (c.accessor)(&hedged) {
            assert!(
                !v.contains(REASON),
                "{:?}: the hedging sentence must not be a cell value any more; got {v:?}",
                c.id
            );
        }
    }
    fn queue() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        rsx! {
            FindingsTable {
                findings: vec![
                    serde_json::from_value(serde_json::json!({
                        "repo": "me/api", "path": "src/a.rs", "line": 7,
                        "rule_id": "AI-LAYERING", "severity": "medium", "snippet": "fn x() {}",
                        "status": "active",
                        "detail": "No mappers crate. [needs review: premature for a mini app]",
                        "kind": "defect", "needs_review": true, "confidence": "needs-review"
                    })).expect("fixture"),
                ],
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
            }
        }
    }
    let mut vdom = VirtualDom::new(queue);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains(">Confidence<"),
        "the new header; html=\n{html}"
    );
    assert!(
        !html.contains(">Needs review<"),
        "the old column header is gone; html=\n{html}"
    );
    for gone in ["nr-reason", "nr-flag", "test-flag"] {
        assert!(
            !html.contains(gone),
            "the prose cell renderer and its chips are gone (found {gone:?}); html=\n{html}"
        );
    }

    // Arrived in the pane. `DetailPaneRow` is the ONE place the pane's props are built, so
    // asserting on it covers the pane for every row it can ever focus.
    let pane = DetailPaneRow::from_finding(&hedged, None, None, 0, 1);
    assert_eq!(
        pane.needs_review.as_deref(),
        Some(REASON),
        "the hedge the Queue stopped drawing must reach the pane"
    );
    assert!(
        !pane.explanation.contains("[needs review"),
        "and the pane splits it OFF the body rather than leaving the marker inline twice"
    );
    assert_eq!(
        pane.confidence.as_deref(),
        Some("needs-review"),
        "the structured value the chip reads is in the pane too, so the two agree"
    );
}

/// req_pass2_13 (§9.4): NO informational surface renders a severity, and the proof is a
/// fixture that WOULD leak one.
///
/// §9.4: "Conventions, No convention and Not assessed panels render NO severity anywhere.
/// The kinds ship at `info` by construction; a severity badge there is a dead affordance
/// implying a ranking decision that does not exist."
///
/// The existing `req_pass2_6_conventions_panel_lists_labels_and_renders_no_severity` pins
/// one panel with an `info` fixture — which cannot distinguish "the panel ignores
/// severity" from "the fixture happened to be boring". These rows are authored
/// `critical`/`high`/`medium` precisely so the assertion can only pass if the panels never
/// read the field at all. All three panels, plus the rule hub strip that every one of
/// their rule ids opens.
#[test]
fn req_pass2_13_no_informational_surface_renders_a_severity() {
    fn harness() -> Element {
        rsx! {
            ConventionsPanel {
                findings: vec![kinded_finding(
                    "API-VERSIONING-1",
                    "critical",
                    serde_json::json!({
                        "kind": "convention_observed",
                        "convention_option_id": "versioned",
                        "convention_label": "URI-versioned endpoints",
                    }),
                )],
            }
            NoConventionPanel {
                findings: vec![kinded_finding(
                    "ERROR-HANDLING-1",
                    "high",
                    serde_json::json!({
                        "kind": "no_convention",
                        "observed_options": [
                            { "option_id": "result", "label": "Result types", "sites": 9 },
                        ],
                    }),
                )],
            }
            NotAssessedPanel {
                findings: vec![kinded_finding(
                    "ESLINT-1",
                    "medium",
                    serde_json::json!({
                        "kind": "not_assessed",
                        "reason": "tool_unavailable",
                        "reason_detail": "eslint is not installed",
                    }),
                )],
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    // Each panel really did render its row, so the absence below is an absence of
    // severity and not an absence of output.
    for present in ["API-VERSIONING-1", "ERROR-HANDLING-1", "ESLINT-1"] {
        assert!(
            html.contains(present),
            "{present} must render; html=\n{html}"
        );
    }
    for forbidden in [
        "severity",
        "Severity",
        "critical",
        "Critical",
        "high",
        "High",
        "medium",
        "Medium",
        "chorale-badge",
        "finding-row-critical",
    ] {
        assert!(
            !html.contains(forbidden),
            "§9.4: no severity on an informational panel (found {forbidden:?}); html=\n{html}"
        );
    }

    // And the hub strip those rule ids open — the fourth informational surface, reached
    // from all three panels — says nothing about severity either.
    #[component]
    fn HubHarness(hub: RuleHubView) -> Element {
        rsx! { RuleHubStrip { hub, on_jump: Callback::new(|_: KindSurface| {}) } }
    }
    let rows = vec![kinded_finding(
        "API-VERSIONING-1",
        "critical",
        serde_json::json!({
            "kind": "convention_observed",
            "convention_option_id": "versioned",
            "convention_label": "URI-versioned endpoints",
        }),
    )];
    let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("the rule is in the scan");
    let mut vdom = VirtualDom::new_with_props(HubHarness, HubHarnessProps { hub });
    vdom.rebuild_in_place();
    let hub_html = dioxus_ssr::render(&vdom);
    assert!(
        hub_html.contains("Convention observed"),
        "the hub rendered the informational rule; html=\n{hub_html}"
    );
    for forbidden in ["severity", "Severity", "critical", "Critical"] {
        assert!(
            !hub_html.contains(forbidden),
            "§9.4: no severity on the hub for an informational rule (found {forbidden:?}); \
                 html=\n{hub_html}"
        );
    }
}
