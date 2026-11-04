//! Tests: convention commit tests.

use super::*;
use camerata_ui_core::adopt::CommitGate;

/// A finding carrying an arbitrary flattened `kind` payload.
fn row(repo: &str, rule_id: &str, kind: serde_json::Value) -> FindingView {
    let mut v = serde_json::json!({
        "repo": repo, "path": "src/lib.rs", "line": 7, "rule_id": rule_id,
        "severity": "info", "snippet": "s", "detail": "d", "status": "active",
    });
    for (k, val) in kind.as_object().expect("kind payload is an object") {
        v[k] = val.clone();
    }
    serde_json::from_value(v).expect("valid FindingView fixture")
}

fn convention(repo: &str, rule_id: &str, option: &str, label: &str) -> FindingView {
    row(
        repo,
        rule_id,
        serde_json::json!({
            "kind": "convention_observed",
            "convention_option_id": option,
            "convention_label": label,
        }),
    )
}

fn render_conventions(
    findings: Vec<FindingView>,
    model: AdoptionModel,
    only_unopened: bool,
) -> String {
    #[component]
    fn Harness(findings: Vec<FindingView>, model: AdoptionModel, only_unopened: bool) -> Element {
        rsx! {
            ConventionsPanel { findings, model, only_unopened }
        }
    }
    let mut vdom = VirtualDom::new_with_props(
        Harness,
        HarnessProps {
            findings,
            model,
            only_unopened,
        },
    );
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

fn render_dialog(plan: CommitPlan, typed: &str, busy: bool) -> String {
    #[component]
    fn Harness(plan: CommitPlan, typed: String, busy: bool) -> Element {
        rsx! {
            ConventionCommitDialog {
                plan,
                typed,
                busy,
                on_typed: Callback::new(|_: String| {}),
                on_cancel: Callback::new(|()| {}),
                on_review_unopened: Callback::new(|()| {}),
                on_commit: Callback::new(|()| {}),
            }
        }
    }
    let mut vdom = VirtualDom::new_with_props(
        Harness,
        HarnessProps {
            plan,
            typed: typed.to_string(),
            busy,
        },
    );
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

/// The commit button's rendered markup, isolated from the rest of the dialog so "is it
/// disabled" is a question about that element and not about the page.
fn commit_button(html: &str) -> String {
    let start = html
        .find("adopt-dialog-commit")
        .expect("the commit button renders");
    // Walk back to the opening `<button` and forward to its closing tag.
    let open = html[..start].rfind("<button").expect("an opening tag");
    let end = html[open..].find("</button>").expect("a closing tag") + open;
    html[open..end].to_string()
}

// ── THE GATE, UNDER RENDER ───────────────────────────────────────────────

/// req_pass2_14 (**THE BYPASS PROOF, RENDERED**): the dialog's Commit button is SHUT for
/// every wrong destination and open only for the right one.
///
/// The pure gate is already proven in `camerata_ui_core::adopt`. This is the other half of
/// the same requirement and the half that actually ships: that the button consults it. The
/// failure this catches is the common one — a dialog that renders a confirmation field,
/// looks thorough, and enables its button regardless.
#[test]
fn req_pass2_14_the_dialog_commit_button_is_shut_for_every_wrong_destination() {
    let findings = vec![
        convention("acme/payments-api", "R-1", "a", "A convention"),
        convention("acme/payments-web", "R-2", "b", "B convention"),
    ];
    let all = adoptable_conventions(&findings);
    let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "acme/payments-api");
    assert_eq!(
        plan.adopted_count(),
        1,
        "precondition: there is something to write"
    );

    for wrong in [
        "",
        "   ",
        "acme/payments-web", // the OTHER repo in the same scan
        "payments-api",
        "acme",
        "acme/payments",
        "acme/payments-api2",
        "yes",
    ] {
        let html = render_dialog(plan.clone(), wrong, false);
        let button = commit_button(&html);
        assert!(
            button.contains("disabled"),
            "typing {wrong:?} must leave the Commit button disabled; button=\n{button}"
        );
        assert!(
            html.contains("adopt-dialog-commit shut"),
            "and visibly shut; typed={wrong:?}; html=\n{html}"
        );
    }

    let html = render_dialog(plan.clone(), "acme/payments-api", false);
    let button = commit_button(&html);
    assert!(
        !button.contains("disabled"),
        "the real destination opens it; button=\n{button}"
    );
    assert!(html.contains("adopt-dialog-commit armed"), "html=\n{html}");
}

/// req_pass2_14: the gate is a TYPED FIELD, not a checkbox. The ruling is explicit about
/// the distinction and about why, so the distinction is pinned rather than left to taste.
#[test]
fn req_pass2_14_the_confirmation_is_typed_not_a_checkbox() {
    let findings = vec![convention("me/api", "R-1", "a", "A")];
    let plan = CommitPlan::conventions(
        &adoptable_conventions(&findings),
        &AdoptionModel::new(),
        "me/api",
    );
    let html = render_dialog(plan, "", false);
    assert!(
        html.contains("adopt-dialog-confirm-input"),
        "a text field; html=\n{html}"
    );
    assert!(
        html.contains("type=\"text\"") || html.contains("type=text"),
        "typed input; html=\n{html}"
    );
    assert!(
        !html.contains("type=\"checkbox\""),
        "a checkbox is muscle memory at 1am \u{2014} the ruling forbids it; html=\n{html}"
    );
    assert!(
        html.contains("Type me/api to confirm"),
        "and it asks for the destination BY NAME; html=\n{html}"
    );
}

/// req_pass2_14: an EMPTY plan cannot be committed however the field is filled in. A
/// dialog that writes nothing must not present a live Commit.
#[test]
fn req_pass2_14_an_empty_plan_renders_a_shut_commit() {
    let plan = CommitPlan {
        destination_repo: "me/api".to_string(),
        noun: "conventions".to_string(),
        adopted: vec![],
        skipped: 4,
    };
    assert_eq!(commit_gate(&plan, "me/api"), CommitGate::NothingToCommit);
    let html = render_dialog(plan, "me/api", false);
    assert!(commit_button(&html).contains("disabled"), "html=\n{html}");
}

/// req_pass2_14: while a commit is in flight every exit is DISABLED rather than removed,
/// so a double-click cannot fire a second write and the dialog does not jump under the
/// cursor.
#[test]
fn req_pass2_14_an_in_flight_commit_disables_every_exit() {
    let findings = vec![convention("me/api", "R-1", "a", "A")];
    let plan = CommitPlan::conventions(
        &adoptable_conventions(&findings),
        &AdoptionModel::new(),
        "me/api",
    );
    let html = render_dialog(plan, "me/api", true);
    assert!(commit_button(&html).contains("disabled"), "html=\n{html}");
    assert!(html.contains("Writing"), "and it says so; html=\n{html}");
    for class in ["adopt-dialog-cancel", "adopt-dialog-review"] {
        assert!(html.contains(class), "{class} still present; html=\n{html}");
    }
}

// ── WHAT THE DIALOG SAYS ─────────────────────────────────────────────────

/// req_pass2_14: the dialog states the four things the ruling requires — the destination,
/// the adopted count, the skipped count, and **how many were adopted by default and never
/// opened** — in the approved copy.
#[test]
fn req_pass2_14_the_dialog_states_the_destination_the_counts_and_the_unopened() {
    let findings: Vec<FindingView> = (1..=36)
        .map(|i| convention("acme/api", &format!("R-{i}"), "opt", "Label"))
        .collect();
    let all = adoptable_conventions(&findings);
    let mut model = AdoptionModel::new();
    for i in 1..=11 {
        model.mark_opened(&adoption_key("acme/api", &format!("R-{i}")));
    }
    let plan = CommitPlan::conventions(&all, &model, "acme/api");
    let html = render_dialog(plan, "", false);

    assert!(
        html.contains(
            "You are writing 36 conventions into a client repository. This changes how \
                 future AI agents working on that codebase will behave."
        ),
        "the approved headline, verbatim; html=\n{html}"
    );
    assert!(
        html.contains(
            "25 conventions were adopted by default and not opened. Adoption is opt-out, \
                 so these were selected for you."
        ),
        "the approved warning, with the REAL number; html=\n{html}"
    );
    assert!(
        html.contains("Destination repository:") && html.contains("acme/api"),
        "the destination is named explicitly; html=\n{html}"
    );
    assert!(
        html.contains("36 to write") && html.contains("0 skipped"),
        "both counts; html=\n{html}"
    );
}

/// req_pass2_14: with every convention opened the warning is GONE, not rendered as a zero.
/// A standing "0 were not opened" row teaches the eye to skip exactly the line the gate
/// exists to make unskippable.
#[test]
fn req_pass2_14_the_unopened_warning_disappears_when_nothing_is_unopened() {
    let findings = vec![
        convention("me/api", "R-1", "a", "A"),
        convention("me/api", "R-2", "b", "B"),
    ];
    let all = adoptable_conventions(&findings);
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "R-1"));
    model.mark_opened(&adoption_key("me/api", "R-2"));
    let plan = CommitPlan::conventions(&all, &model, "me/api");
    assert_eq!(plan.unopened_count(), 0);
    let html = render_dialog(plan, "", false);
    assert!(
        !html.contains("adopted by default and not opened"),
        "no zero sentence; html=\n{html}"
    );
    assert!(
        !html.contains("adopt-dialog-warning"),
        "and no empty warning box; html=\n{html}"
    );
}

/// req_pass2_14 (**THE UNOPENED COUNT IS REAL, RENDERED**): open one more convention and
/// the dialog's number falls by exactly one.
///
/// The pure test walks the whole ladder; this one proves the RENDERED dialog is reading
/// that ladder and not a constant. Three renders of the same plan shape with three
/// different openness records.
#[test]
fn req_pass2_14_the_rendered_unopened_count_follows_the_open_gesture() {
    let findings: Vec<FindingView> = (1..=4)
        .map(|i| convention("me/api", &format!("R-{i}"), "opt", "Label"))
        .collect();
    let all = adoptable_conventions(&findings);
    let mut model = AdoptionModel::new();
    for (i, expected) in (1..=4).zip([
        "4 conventions were",
        "3 conventions were",
        "2 conventions were",
        "1 convention was",
    ]) {
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        let html = render_dialog(plan, "", false);
        assert!(
            html.contains(expected),
            "before opening R-{i} the dialog must say {expected:?}; html=\n{html}"
        );
        model.mark_opened(&adoption_key("me/api", &format!("R-{i}")));
    }
    // And with the fourth opened, the warning is gone entirely.
    let plan = CommitPlan::conventions(&all, &model, "me/api");
    let html = render_dialog(plan, "", false);
    assert!(
        !html.contains("adopt-dialog-warning"),
        "everything opened, so no warning at all; html=\n{html}"
    );
}

/// req_pass2_14: the dialog lists the rules it is about to write, and flags the unopened
/// ones in that list. The count tells the curator how many; the list tells him which,
/// without leaving the dialog.
#[test]
fn req_pass2_14_the_dialog_lists_the_rules_and_flags_the_unopened_ones() {
    let findings = vec![
        convention("me/api", "READ-ME-1", "a", "A convention"),
        convention("me/api", "UNREAD-1", "b", "B convention"),
    ];
    let all = adoptable_conventions(&findings);
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "READ-ME-1"));
    let plan = CommitPlan::conventions(&all, &model, "me/api");
    let html = render_dialog(plan, "", false);
    assert!(
        html.contains("READ-ME-1") && html.contains("UNREAD-1"),
        "html=\n{html}"
    );
    assert_eq!(
        html.matches("adopt-unopened-flag").count(),
        1,
        "exactly the one that was not opened carries the flag; html=\n{html}"
    );
}

// ── THREE EXITS ──────────────────────────────────────────────────────────

/// req_pass2_14: the dialog offers THREE exits, not two. The ruling is explicit that the
/// safer path must be offered rather than implied:
///
/// > It offers three exits, not two: Cancel, Review the unopened set, Commit.
#[test]
fn req_pass2_14_the_dialog_offers_three_exits() {
    let findings = vec![
        convention("me/api", "R-1", "a", "A"),
        convention("me/api", "R-2", "b", "B"),
    ];
    let all = adoptable_conventions(&findings);
    let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "me/api");
    let html = render_dialog(plan, "", false);
    assert!(
        html.contains("adopt-dialog-cancel") && html.contains("Cancel"),
        "html=\n{html}"
    );
    assert!(
        html.contains("adopt-dialog-review") && html.contains("Review the 2 not opened"),
        "the third exit, naming its size; html=\n{html}"
    );
    assert!(
        html.contains("adopt-dialog-commit") && html.contains("Commit"),
        "html=\n{html}"
    );
}

/// req_pass2_14: the third exit is HIDDEN when everything was opened. A button that
/// filters to an empty list is the 403-button bug in miniature, and the standing
/// hide-dead-end-affordances rule forbids it.
#[test]
fn req_pass2_14_the_review_exit_is_absent_when_there_is_nothing_to_review() {
    let findings = vec![convention("me/api", "R-1", "a", "A")];
    let all = adoptable_conventions(&findings);
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "R-1"));
    let plan = CommitPlan::conventions(&all, &model, "me/api");
    let html = render_dialog(plan, "", false);
    assert!(
        !html.contains("adopt-dialog-review"),
        "nothing unopened, so no review exit; html=\n{html}"
    );
    assert!(html.contains("adopt-dialog-cancel") && html.contains("adopt-dialog-commit"));
}

// ── THE CONVENTIONS PANEL ────────────────────────────────────────────────

/// req_pass2_14 (Q4): adoption is OPT-OUT on the rendered panel. Every untouched
/// convention reads as selected and the commit bar counts all of them; one Skip removes
/// exactly one.
#[test]
fn req_pass2_14_the_conventions_panel_is_opt_out_under_render() {
    let findings = vec![
        convention("me/api", "R-1", "a", "A convention"),
        convention("me/api", "R-2", "b", "B convention"),
        convention("me/api", "R-3", "c", "C convention"),
    ];
    let html = render_conventions(findings.clone(), AdoptionModel::new(), false);
    assert_eq!(
        html.matches("adopt-state on").count(),
        3,
        "all three pre-selected; html=\n{html}"
    );
    assert!(
        html.contains("3 to write"),
        "the bar counts them; html=\n{html}"
    );
    assert!(
        html.contains("Write 3 into me/api"),
        "and the button names the destination; html=\n{html}"
    );
    assert!(
        html.contains("Adoption is OPT-OUT"),
        "the panel says so out loud; html=\n{html}"
    );

    let mut model = AdoptionModel::new();
    model.set_adopted(&adoption_key("me/api", "R-2"), false);
    let html = render_conventions(findings, model, false);
    assert_eq!(html.matches("adopt-state on").count(), 2, "html=\n{html}");
    assert!(
        html.contains("Skipped"),
        "the skipped row says so; html=\n{html}"
    );
    assert!(
        html.contains("2 to write") && html.contains("1 skipped"),
        "html=\n{html}"
    );
}

/// req_pass2_14 (**DEVIATIONS DO NOT BLOCK ADOPTION, RENDERED**) — the owner's §13
/// reversal of the design's own stance:
///
/// > "if I say 'this is the rule and convention' then I mean it, and that can be adopted."
///
/// A rule with only Deviation rows (no `ConventionObserved` row at all) appears in the
/// Conventions panel, pre-selected, labelled with its exception count, with the count
/// rendered as context and the panel saying in words that remediation is separate work.
#[test]
fn req_pass2_14_a_convention_with_live_deviations_is_adoptable_under_render() {
    let dev = |path_line: usize| {
        let mut f = row(
            "me/api",
            "API-VERSIONING-1",
            serde_json::json!({
                "kind": "deviation",
                "deviating_sites": 2,
                "assessed_sites": 47,
                "convention_option_id": "versioned",
                "convention_label": "URI-versioned endpoints",
            }),
        );
        f.line = path_line;
        f
    };
    let html = render_conventions(vec![dev(12), dev(40)], AdoptionModel::new(), false);

    assert!(html.contains("API-VERSIONING-1"), "listed; html=\n{html}");
    assert!(
        html.contains("URI-versioned endpoints"),
        "by its authored label; html=\n{html}"
    );
    assert!(
        html.contains("observed with 2 exceptions"),
        "\u{a7}13's exact phrase; html=\n{html}"
    );
    assert_eq!(
        html.matches("adopt-state on").count(),
        1,
        "PRE-SELECTED \u{2014} the exceptions are context, not a gate; html=\n{html}"
    );
    assert!(
        html.contains("1 to write") && html.contains("Write 1 into me/api"),
        "and it reaches the commit; html=\n{html}"
    );
    assert!(
        html.contains("adopting states the standard going forward"),
        "the panel states WHY the exceptions do not block; html=\n{html}"
    );
    assert_eq!(
        html.matches("adopt-row").count(),
        1,
        "two deviating rows are ONE convention, not two; html=\n{html}"
    );
}

/// req_pass2_14: opening a deviating convention's detail names the exceptions and says
/// plainly that dispositioning them does not gate the adoption — the one place a curator
/// would otherwise reasonably assume the old, blocked behaviour.
#[test]
fn req_pass2_14_an_opened_deviating_convention_says_remediation_is_separate() {
    let f = row(
        "me/api",
        "API-VERSIONING-1",
        serde_json::json!({
            "kind": "deviation",
            "deviating_sites": 2,
            "assessed_sites": 47,
            "convention_option_id": "versioned",
            "convention_label": "URI-versioned endpoints",
        }),
    );
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "API-VERSIONING-1"));
    let html = render_conventions(vec![f], model, false);
    assert!(
        html.contains("adopt-detail"),
        "the detail is open; html=\n{html}"
    );
    assert!(
        html.contains("does not gate this adoption"),
        "and it says so; html=\n{html}"
    );
    assert!(
        html.contains("AGENTS.md"),
        "and names where this is written; html=\n{html}"
    );
    assert!(
        html.contains("adopt-jump") && html.contains("Review the 2 exceptions in the Queue"),
        "\u{a7}13's \u{201c}with a jump into the Queue\u{201d}; html=\n{html}"
    );
}

/// req_pass2_14: a CLEAN convention gets no jump. There are no exceptions to review, and
/// a button that filtered the Queue to nothing would be the 403-button bug.
#[test]
fn req_pass2_14_a_clean_convention_has_no_exceptions_jump() {
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "R-1"));
    let html = render_conventions(vec![convention("me/api", "R-1", "a", "A")], model, false);
    assert!(
        html.contains("adopt-detail"),
        "the detail is open; html=\n{html}"
    );
    assert!(html.contains("observed, no exceptions"), "html=\n{html}");
    assert!(
        !html.contains("adopt-jump"),
        "nothing to jump to; html=\n{html}"
    );
}

/// req_pass2_14: a convention with no AUTHORED option id is listed and explained, never
/// silently committed. Writing the rule's default option instead would install a position
/// the audit never observed, which is the one dishonesty available on this surface.
#[test]
fn req_pass2_14_a_convention_with_no_authored_option_cannot_be_written_under_render() {
    let f = row(
        "me/api",
        "LOOSE-1",
        serde_json::json!({
            "kind": "deviation",
            "deviating_sites": 2,
            "assessed_sites": 9,
            "convention_option_id": null,
            "convention_label": "something the producer saw",
        }),
    );
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "LOOSE-1"));
    let html = render_conventions(vec![f], model, false);
    assert!(html.contains("LOOSE-1"), "still listed; html=\n{html}");
    assert!(
        html.contains("Cannot write"),
        "marked unwritable; html=\n{html}"
    );
    assert!(
        html.contains("No authored option matches"),
        "and told why; html=\n{html}"
    );
    assert!(
        html.contains("Nothing selected to write"),
        "and the commit bar has nothing; html=\n{html}"
    );
    assert!(
        !html.contains("adopt-toggle"),
        "with no Skip/Adopt toggle, because there is no decision to make; html=\n{html}"
    );
}

/// req_pass2_14: the panel's own unopened indicator follows the record, and the "not
/// opened" flag leaves a row the moment it is opened.
#[test]
fn req_pass2_14_the_panel_flags_exactly_the_unopened_rows() {
    let findings = vec![
        convention("me/api", "R-1", "a", "A"),
        convention("me/api", "R-2", "b", "B"),
        convention("me/api", "R-3", "c", "C"),
    ];
    let html = render_conventions(findings.clone(), AdoptionModel::new(), false);
    assert_eq!(
        html.matches("adopt-unopened-flag").count(),
        3,
        "html=\n{html}"
    );
    assert!(
        html.contains("3 not opened"),
        "the bar agrees; html=\n{html}"
    );

    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "R-2"));
    let html = render_conventions(findings, model, false);
    assert_eq!(
        html.matches("adopt-unopened-flag").count(),
        2,
        "html=\n{html}"
    );
    assert!(html.contains("2 not opened"), "html=\n{html}");
    assert!(
        html.contains("Opened"),
        "and the opened row says so; html=\n{html}"
    );
}

/// req_pass2_14: the dialog's third exit lands somewhere useful — the panel's
/// unopened-only view shows exactly the adopted-but-unopened rows and offers the way back.
#[test]
fn req_pass2_14_the_unopened_only_view_shows_exactly_that_set() {
    let findings = vec![
        convention("me/api", "OPENED-1", "a", "A"),
        convention("me/api", "UNREAD-1", "b", "B"),
        convention("me/api", "SKIPPED-1", "c", "C"),
    ];
    let mut model = AdoptionModel::new();
    model.mark_opened(&adoption_key("me/api", "OPENED-1"));
    model.set_adopted(&adoption_key("me/api", "SKIPPED-1"), false);
    let html = render_conventions(findings, model, true);
    assert!(
        html.contains("UNREAD-1"),
        "the unopened one is shown; html=\n{html}"
    );
    assert!(
        !html.contains("OPENED-1"),
        "the opened one is not; html=\n{html}"
    );
    assert!(
        !html.contains("SKIPPED-1"),
        "nor is a skipped one \u{2014} it is not being written; html=\n{html}"
    );
    assert!(
        html.contains("adopt-filter-clear"),
        "and there is a way back; html=\n{html}"
    );
}

// ── THE NO-CONVENTION PICK (Q3) ──────────────────────────────────────────

fn render_no_convention(findings: Vec<FindingView>, model: AdoptionModel) -> String {
    #[component]
    fn Harness(findings: Vec<FindingView>, model: AdoptionModel) -> Element {
        rsx! {
            NoConventionPanel { findings, model }
        }
    }
    let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { findings, model });
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

fn split_row(repo: &str, rule_id: &str) -> FindingView {
    row(
        repo,
        rule_id,
        serde_json::json!({
            "kind": "no_convention",
            "observed_options": [
                { "option_id": "result", "label": "Result types", "sites": 9 },
                { "option_id": "panic", "label": "Panics", "sites": null },
            ],
        }),
    )
}

/// req_pass2_14 (Q3): the `NoConvention` pick DEFAULTS to "leave as reported", states its
/// client-facing governance nature in the UI, and offers the authored options as the only
/// alternatives.
#[test]
fn req_pass2_14_the_no_convention_pick_defaults_to_leave_and_says_it_is_governance() {
    let html = render_no_convention(
        vec![split_row("me/api", "ERROR-HANDLING-1")],
        AdoptionModel::new(),
    );
    assert!(
        html.contains("CLIENT-FACING governance act"),
        "Q3: it must SAY SO; html=\n{html}"
    );
    assert!(
        html.contains("AGENTS.md"),
        "and name where it lands; html=\n{html}"
    );
    assert!(
        html.contains("Leave as reported"),
        "the default option is present; html=\n{html}"
    );
    assert_eq!(
        html.matches("adopt-state off").count(),
        1,
        "and the row reads as left-as-reported; html=\n{html}"
    );
    assert!(
        html.contains("Nothing selected to write"),
        "so the commit bar has nothing; html=\n{html}"
    );
    assert!(
        html.contains("Recommend: Result types (9 sites)") && html.contains("Recommend: Panics"),
        "the authored options, with counts only where the producer counted; html=\n{html}"
    );
    assert!(
        !html.contains("Panics (0 sites)"),
        "never a fabricated count; html=\n{html}"
    );
}

/// req_pass2_14 (Q3): a pick inherits the SAME gate. Not a lighter one, not none.
#[test]
fn req_pass2_14_a_no_convention_pick_inherits_the_same_typed_gate() {
    let findings = vec![split_row("me/api", "ERROR-HANDLING-1")];
    let mut model = AdoptionModel::new();
    model.set_recommendation(
        &adoption_key("me/api", "ERROR-HANDLING-1"),
        Some("result".to_string()),
    );

    let panel = render_no_convention(findings.clone(), model.clone());
    assert!(panel.contains("Write 1 into me/api"), "html=\n{panel}");
    assert_eq!(panel.matches("adopt-state on").count(), 1, "html=\n{panel}");

    let plan = CommitPlan::recommendations(&split_rules(&findings), &model, "me/api");
    assert!(
        plan.headline()
            .contains("1 recommendation into a client repository"),
        "{}",
        plan.headline()
    );
    let shut = render_dialog(plan.clone(), "me/web", false);
    assert!(commit_button(&shut).contains("disabled"), "html=\n{shut}");
    let armed = render_dialog(plan, "me/api", false);
    assert!(
        !commit_button(&armed).contains("disabled"),
        "html=\n{armed}"
    );
}

// ── FOCUS: the dialog must not leave the pane's keys armed ───────────────

/// req_pass2_14 (**THE FOCUS CARRY-OVER**): typing a destination into the dialog cannot
/// dispose a finding.
///
/// The destination this test uses contains `c`, `i`, `t` AND `f`, which are exactly the
/// four disposition hotkeys — so if the scoping ever regressed, typing this repository's
/// name would Confirm a row, open two reason prompts and mark something a false positive.
/// Every character is walked through the real `pane_command`.
///
/// `pane_keys_armed` is DEFINED as `pane_command(focus, "c", none).is_some()`, so the
/// indicator cannot report armed while the handler refuses; the render assertion below
/// pins that the pane says "off" in the state a dialog leaves it in.
#[test]
fn req_pass2_14_typing_the_destination_cannot_reach_the_panes_disposition_keys() {
    let destination = "acme-fintech/api";
    assert!(
        ['c', 'i', 't', 'f']
            .iter()
            .all(|c| destination.contains(*c)),
        "precondition: the fixture destination contains every hotkey letter"
    );
    // The dialog's input is not the pane's cursor, whatever else is true.
    for focus in [PaneFocus::Elsewhere, PaneFocus::ReasonPrompt] {
        assert!(!pane_keys_armed(focus), "{focus:?} must report keys OFF");
        for ch in destination.chars() {
            let key = ch.to_string();
            assert!(
                pane_command(focus, &key, KeyMods::none()).is_none(),
                "{ch:?} typed under {focus:?} must produce no command"
            );
        }
    }
    // And the pane RENDERS that state, so the curator is not told otherwise.
    #[component]
    fn PaneHarness() -> Element {
        rsx! {
            FindingDetailPane {
                row: None,
                focus: PaneFocus::Elsewhere,
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
    let mut vdom = VirtualDom::new(PaneHarness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("pane-keys armed"),
        "the pane must not claim armed keys while a dialog holds focus; html=\n{html}"
    );
}

// ── THE WIRE: read, merge, write, emit ───────────────────────────────────

/// req_pass2_14 (**THE COMMIT IS REAL, NOT A SEAM**): `commit_adoptions` performs the
/// read-modify-write against the EXISTING ruleset route and then calls the EXISTING
/// emit-local route, against a mock BFF that asserts both bodies.
///
/// The body assertion is the point. A commit that posted only the adopted rule would be
/// accepted by the server and would DELETE every other selection the project has, because
/// `upsert_base_rules` replaces the list rather than merging it.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn req_pass2_14_commit_adoptions_reads_merges_writes_and_emits() {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // 1. The project's current ruleset — one UNRELATED selection with a drift baseline.
    Mock::given(method("GET"))
        .and(path("/api/projects/proj-1/ruleset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "ruleset": {
                "selections": [{
                    "rule_id": "UNRELATED-1",
                    "chosen_option": "keep-me",
                    "repos": ["me/web"],
                    "applied_hash": "sha-abc",
                    "applied_directive": "as baselined",
                    "applied_version": 2,
                }],
                "cross_repo": [],
                "process": [],
                "custom": [{ "name": "HOUSE", "body": "b", "domain": "*", "repos": [] }],
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // 2. The merged write: the untouched selection SURVIVES with its baseline intact, the
    //    adopted convention is appended, and no `custom` key is sent.
    Mock::given(method("POST"))
        .and(path("/api/projects/proj-1/ruleset"))
        .and(body_json(serde_json::json!({
            "selections": [
                {
                    "rule_id": "UNRELATED-1",
                    "chosen_option": "keep-me",
                    "repos": ["me/web"],
                    "applied_hash": "sha-abc",
                    "applied_directive": "as baselined",
                    "applied_version": 2,
                },
                {
                    "rule_id": "API-VERSIONING-1",
                    "chosen_option": "versioned",
                    "repos": ["me/api"],
                    "applied_hash": null,
                    "applied_directive": null,
                    "applied_version": null,
                }
            ],
            "cross_repo": [],
            "process": [],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;
    // 3. The EXISTING emit path, cascade all-false: write the governance files in place.
    Mock::given(method("POST"))
        .and(path("/api/projects/proj-1/emit-local"))
        .and(body_json(
            serde_json::json!({ "branch": false, "push": false, "open_pr": false }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "results": [{ "repo": "me/api", "ok": true }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (ok, msg) = super::commit_adoptions(
        "proj-1",
        "me/api",
        &[AdoptedRule {
            rule_id: "API-VERSIONING-1".to_string(),
            option_id: "versioned".to_string(),
            label: "URI-versioned endpoints".to_string(),
            opened: true,
        }],
    )
    .await;

    assert!(ok, "the commit must succeed: {msg}");
    assert!(msg.contains("me/api"), "and name the destination: {msg}");
}

/// req_pass2_14: a FAILED READ writes nothing. Pressing on would post a merge built over
/// an empty ruleset, which `upsert_base_rules` would then make the project's whole
/// selection set — the one failure mode on this path that destroys data rather than
/// merely not doing anything.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn req_pass2_14_a_failed_ruleset_read_writes_nothing() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/proj-1/ruleset"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    // Deliberately NOT mounted: any POST would 404, but the assertion that matters is the
    // message — the function must report that nothing was written.
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (ok, msg) = super::commit_adoptions(
        "proj-1",
        "me/api",
        &[AdoptedRule {
            rule_id: "R-1".to_string(),
            option_id: "o".to_string(),
            label: "O".to_string(),
            opened: true,
        }],
    )
    .await;

    assert!(!ok, "a failed read is a failed commit");
    assert!(
        msg.contains("nothing was written"),
        "and it says so plainly: {msg}"
    );
}

/// req_pass2_14: an empty adopted set never reaches the network at all.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn req_pass2_14_an_empty_commit_makes_no_requests() {
    use wiremock::MockServer;
    let server = MockServer::start().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (ok, msg) = super::commit_adoptions("proj-1", "me/api", &[]).await;
    assert!(!ok);
    assert!(msg.contains("Nothing selected"), "{msg}");
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "no request may be made for an empty commit"
    );
}
