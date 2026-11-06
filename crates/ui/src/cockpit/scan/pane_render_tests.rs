//! Tests: pane render tests.

use super::*;

fn sample() -> FindingView {
    serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": "src/handlers.rs", "line": 88,
        "rule_id": "SQL-INJECTION-1", "severity": "critical",
        "snippet": "db.query(format!(\"select … {}\", id))",
        "detail": "String-interpolated SQL reaches the driver unparameterised.",
        "status": "active", "confidence": "high", "effort": "medium",
        "kind": "defect",
        "also_locations": [
            { "repo": "me/api", "path": "src/export.rs", "line": 31,
              "rule_id": "SQL-INJECTION-1", "snippet": "db.query(format!(..))",
              "consequence": false },
            { "repo": "me/api", "path": "src/report.rs", "line": 4,
              "rule_id": "DATA-EXFIL-1", "snippet": "every customer row is readable",
              "consequence": true }
        ]
    }))
    .expect("valid FindingView fixture")
}

fn deviation() -> FindingView {
    serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": "src/routes.rs", "line": 12,
        "rule_id": "API-VERSIONING-1", "severity": "medium", "snippet": "get(\"/users\")",
        "detail": "", "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
        "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
    }))
    .expect("valid FindingView fixture")
}

fn render(
    row: Option<DetailPaneRow>,
    focus: PaneFocus,
    prompt: Option<TriageState>,
    undo: Option<UndoBanner>,
) -> String {
    #[component]
    fn Harness(
        row: Option<DetailPaneRow>,
        focus: PaneFocus,
        prompt: Option<TriageState>,
        undo: Option<UndoBanner>,
    ) -> Element {
        rsx! {
            FindingDetailPane {
                row,
                focus,
                prompt,
                prompt_reason: String::new(),
                undo,
                on_key: Callback::new(|_: (String, KeyMods)| {}),
                on_focus: Callback::new(|_: PaneFocus| {}),
                on_prompt_reason: Callback::new(|_: String| {}),
                on_prompt_commit: Callback::new(|()| {}),
                on_prompt_cancel: Callback::new(|()| {}),
                on_undo: Callback::new(|()| {}),
                on_dispose: Callback::new(|_: TriageState| {}),
                on_open_detail: Callback::new(|()| {}),
            }
        }
    }
    let mut vdom = VirtualDom::new_with_props(
        Harness,
        HarnessProps {
            row,
            focus,
            prompt,
            undo,
        },
    );
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

/// req_pass2_11: the pane shows the focused row's whole case — rule id, kind badge,
/// severity, location, snippet, cited standard, and the fix with its effort estimate.
/// This is the content that made the modal's open/decide/close cycle necessary; the
/// point of the pane is that none of it costs a click.
#[test]
fn req_pass2_11_the_pane_shows_the_focused_rows_whole_case() {
    let row = DetailPaneRow::from_finding(
        &sample(),
        Some("Never interpolate into SQL; bind parameters."),
        Some("Replace the format! with a bound parameter.".to_string()),
        0,
        12,
    );
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(
        html.contains("finding-pane"),
        "pane container; html=\n{html}"
    );
    assert!(html.contains("SQL-INJECTION-1"), "rule id; html=\n{html}");
    assert!(html.contains("Defect"), "kind badge; html=\n{html}");
    assert!(html.contains("critical"), "severity; html=\n{html}");
    assert!(
        html.contains("src/handlers.rs:88"),
        "location; html=\n{html}"
    );
    assert!(
        html.contains("Never interpolate into SQL"),
        "cited standard; html=\n{html}"
    );
    assert!(html.contains("Replace the format!"), "fix; html=\n{html}");
    assert!(html.contains("medium"), "effort estimate; html=\n{html}");
    assert!(
        html.contains("1 of 12"),
        "position in the queue; html=\n{html}"
    );
}

/// req_pass2_11 (design §6): a merged row's ABSORBED sites are listed, with consequence
/// sites subordinated under "also affects" — the full-disclosure surface C5 lacked.
#[test]
fn req_pass2_11_the_pane_lists_every_absorbed_site() {
    let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(
        html.contains("Sites (3)"),
        "1 primary + 2 absorbed; html=\n{html}"
    );
    assert!(
        html.contains("src/export.rs:31"),
        "peer site; html=\n{html}"
    );
    assert!(
        html.contains("src/report.rs:4"),
        "consequence site; html=\n{html}"
    );
    assert!(
        html.contains("also affects"),
        "the consequence sub-label; html=\n{html}"
    );
    assert!(
        html.contains("DATA-EXFIL-1"),
        "the absorbed site's own rule id; html=\n{html}"
    );
}

/// req_pass2_11: a single-site row says nothing about sites at all, rather than
/// rendering an empty section or a lonely "Sites (1)".
#[test]
fn req_pass2_11_an_unmerged_row_renders_no_sites_section() {
    let row = DetailPaneRow::from_finding(&deviation(), None, None, 0, 1);
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(
        !html.contains("Sites ("),
        "no sites section for one site; html=\n{html}"
    );
}

/// req_pass2_11: a deviation's structured claim renders in the pane verbatim from
/// `DeviationHeadline::sentence` — the same words the group header carries.
#[test]
fn req_pass2_11_the_pane_states_a_deviations_claim() {
    let row = DetailPaneRow::from_finding(&deviation(), None, None, 0, 1);
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(html.contains("Deviation"), "kind badge; html=\n{html}");
    assert!(
        html.contains("2 of 47 sites deviate from &#34;URI-versioned endpoints&#34;"),
        "the structured sentence; html=\n{html}"
    );
}

/// req_pass2_11 — **THE SCOPING, RENDERED.** The pane tells the curator whether the keys
/// are live, and it is the HANDLER'S OWN GATE that decides what it says. While the reason
/// prompt holds focus the pane says the keys are off; while the cursor holds focus it
/// names all four.
#[test]
fn req_pass2_11_the_pane_says_whether_the_keys_are_armed() {
    let row = || Some(DetailPaneRow::from_finding(&sample(), None, None, 0, 1));

    let armed = render(row(), PaneFocus::Cursor, None, None);
    assert!(
        armed.contains("pane-keys armed"),
        "armed class; html=\n{armed}"
    );
    for label in ["C confirm", "I ignore", "T tech debt", "F false positive"] {
        assert!(
            armed.contains(label),
            "{label} must be named; html=\n{armed}"
        );
    }

    // Typing a reason: the keys are OFF, and the pane says so in those words.
    let typing = render(
        row(),
        PaneFocus::ReasonPrompt,
        Some(TriageState::Ignored),
        None,
    );
    assert!(
        typing.contains("pane-keys off"),
        "off class; html=\n{typing}"
    );
    assert!(
        typing.contains("Keys are off while you type"),
        "and says why; html=\n{typing}"
    );
    assert!(!typing.contains("pane-keys armed"), "html=\n{typing}");

    // Focus elsewhere on the page: also off, with the way back.
    let away = render(row(), PaneFocus::Elsewhere, None, None);
    assert!(away.contains("pane-keys off"), "html=\n{away}");
    assert!(!away.contains("pane-keys armed"), "html=\n{away}");
}

/// M4: a row the engine flagged shows each flag as a chip in the pane; a row with none shows
/// no flag element at all.
#[test]
fn m4_the_pane_shows_a_chip_for_each_review_flag() {
    let mut flagged = sample();
    flagged.captures.insert(
        "review_flags".to_string(),
        r#"["fix_ungrounded","unverified_name:src/config.rs"]"#.to_string(),
    );
    let row = DetailPaneRow::from_finding(&flagged, None, None, 0, 1);
    assert_eq!(
        row.review_flags,
        vec![
            "fix_ungrounded".to_string(),
            "unverified_name:src/config.rs".to_string()
        ]
    );
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(html.contains("review-flag-chip"), "html=\n{html}");
    assert!(html.contains("fix_ungrounded"), "html=\n{html}");
    assert!(
        html.contains("unverified_name:src/config.rs"),
        "html=\n{html}"
    );

    let clean = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
    let html = render(Some(clean), PaneFocus::Cursor, None, None);
    assert!(!html.contains("data-review-flags"), "html=\n{html}");
}

/// req_pass2_11: the `I`/`F` prompt renders with the disposition it is collecting a
/// reason FOR named in it, so a pre-focused textarea is never ambiguous about which of
/// the two it will commit.
#[test]
fn req_pass2_11_the_reason_prompt_names_the_disposition_it_will_commit() {
    let row = || Some(DetailPaneRow::from_finding(&sample(), None, None, 0, 1));
    let ignoring = render(
        row(),
        PaneFocus::ReasonPrompt,
        Some(TriageState::Ignored),
        None,
    );
    assert!(ignoring.contains("pane-reason-prompt"), "html=\n{ignoring}");
    assert!(
        !ignoring.contains('\u{2014}'),
        "no em-dash in the prompt's text or placeholder; html=\n{ignoring}"
    );
    assert!(
        ignoring.contains("Ignored"),
        "names the disposition; html=\n{ignoring}"
    );
    assert!(
        ignoring.contains("required"),
        "states the invariant; html=\n{ignoring}"
    );

    let fp = render(
        row(),
        PaneFocus::ReasonPrompt,
        Some(TriageState::FalsePositive),
        None,
    );
    assert!(fp.contains("False positive"), "html=\n{fp}");

    // No prompt open: no textarea in the pane at all (design §9.2 — an unused textarea
    // is noise 95% of the time).
    let idle = render(row(), PaneFocus::Cursor, None, None);
    assert!(!idle.contains("pane-reason-prompt"), "html=\n{idle}");
}

/// req_pass2_11: the undo toast renders what it will reverse and how long is left, and
/// disappears when there is nothing to undo.
#[test]
fn req_pass2_11_the_undo_toast_names_its_action_and_its_window() {
    let row = || Some(DetailPaneRow::from_finding(&sample(), None, None, 0, 1));
    let with_undo = render(
        row(),
        PaneFocus::Cursor,
        None,
        Some(UndoBanner {
            message: "Confirmed 1 finding.".to_string(),
            seconds_left: 4,
        }),
    );
    assert!(with_undo.contains("pane-undo"), "html=\n{with_undo}");
    assert!(
        with_undo.contains("Confirmed 1 finding."),
        "html=\n{with_undo}"
    );
    assert!(with_undo.contains("Undo"), "html=\n{with_undo}");
    assert!(
        with_undo.contains("4s"),
        "the countdown; html=\n{with_undo}"
    );

    let without = render(row(), PaneFocus::Cursor, None, None);
    assert!(!without.contains("pane-undo"), "html=\n{without}");
}

/// req_pass2_11: with no row focused — an empty triage table, or every group collapsed —
/// the pane says so instead of rendering a shell of empty labels, and offers no
/// disposition buttons, because there is nothing to dispose.
#[test]
fn req_pass2_11_an_unfocused_pane_offers_nothing_to_act_on() {
    let html = render(None, PaneFocus::Elsewhere, None, None);
    assert!(
        html.contains("finding-pane"),
        "the pane still occupies its dock; html=\n{html}"
    );
    assert!(html.contains("pane-empty"), "html=\n{html}");
    assert!(
        !html.contains("pane-dispose"),
        "no dead buttons; html=\n{html}"
    );
    assert!(
        !html.contains("pane-keys armed"),
        "and no armed keys; html=\n{html}"
    );
}

/// req_pass2_11: the pane IS a focusable region, with the id `focus_detail_pane` targets.
///
/// This is the physical half of the Q5 scoping. "The keys fire while the detail pane
/// holds focus" is only a real condition if there is something that can hold focus, that
/// a Tab reaches, and that a row click can be routed to — and if the id the row-click
/// handler focuses is the id the pane actually renders. A drift between those two would
/// leave the pane permanently unfocusable by mouse, and therefore the keys permanently
/// dead, with the indicator none the wiser.
#[test]
fn req_pass2_11_the_pane_is_the_focusable_region_the_scoping_names() {
    let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(
        html.contains("tabindex=\"0\""),
        "Tab must reach it; html=\n{html}"
    );
    assert!(
        html.contains(&format!("id=\"{DETAIL_PANE_ID}\"")),
        "the id `focus_detail_pane` focuses; html=\n{html}"
    );
}

/// req_pass2_11 (design §9.6): the pane replaces the row-click modal, but keeps it one
/// deliberate click away — the modal carries the on-demand enclosing-code-block fetch,
/// which is far too expensive to re-run on every cursor-down.
#[test]
fn req_pass2_11_the_pane_keeps_the_modal_one_click_away() {
    let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    assert!(html.contains("pane-full-detail"), "html=\n{html}");
    assert!(html.contains("Full detail"), "html=\n{html}");
    // And not on an empty pane, where there is no row to open.
    let empty = render(None, PaneFocus::Elsewhere, None, None);
    assert!(!empty.contains("pane-full-detail"), "html=\n{empty}");
}

/// req_pass2_11: the pane carries the same four dispositions as buttons, so the gesture
/// is available to the mouse too and the keys are an accelerator rather than the only
/// way in. Their labels name their key.
#[test]
fn req_pass2_11_the_pane_offers_the_four_dispositions_as_buttons_too() {
    let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
    let html = render(Some(row), PaneFocus::Cursor, None, None);
    let n = html.matches("pane-dispose").count();
    assert_eq!(n, 4, "one button per disposition; html=\n{html}");
    for s in [
        TriageState::Confirmed,
        TriageState::Ignored,
        TriageState::TechDebt,
        TriageState::FalsePositive,
    ] {
        assert!(
            html.contains(s.label()),
            "{} button; html=\n{html}",
            s.label()
        );
    }
}
