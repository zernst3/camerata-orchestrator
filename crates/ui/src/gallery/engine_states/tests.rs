use super::*;
use crate::style::GLOBAL_CSS;
use camerata_api_types::review_view::EngineCheckError;
use camerata_ui_core::engine_check::{card_for_door, sample_line};
use camerata_ui_core::test_support::review_fixtures::{
    build, dataset_base, fixture, EngineMode, Params, SEED,
};
use camerata_ui_core::tree::export::Door;

fn page(id: &str) -> String {
    render(id).replace(GLOBAL_CSS, "")
}

#[test]
fn every_engine_state_renders_with_no_em_dash_and_no_raw_nul() {
    assert_eq!(states().len(), 11, "every engine state is built");
    for (id, render) in states() {
        let html = render().replace(GLOBAL_CSS, "");
        assert!(!html.contains('\u{2014}'), "{id} has an em dash");
        assert!(!html.contains('\0'), "{id} has a raw NUL");
        assert!(html.contains("Engine Check"), "{id}");
        assert!(html.contains("engine-lane"), "{id}");
    }
}

#[test]
fn the_pending_card_asks_the_question_and_shows_both_panels() {
    let html = page("engine/card-pending");
    let lane = dataset_base().engine;
    assert!(html.contains("Did the engine get this right?"));
    assert!(html.contains("Yes (Y)") && html.contains("No (N)"));
    assert!(html.contains("As the client would see it"));
    assert!(html.contains("What the engine did"));
    for label in [
        "Stage 1",
        "Placed",
        "Escalation",
        "Collapsed from",
        "Duplicate link",
        "Outlier",
        "Review flags",
    ] {
        assert!(html.contains(label), "{label}");
    }
    assert!(html.contains("Card 1 of 50"));
    assert!(html.contains("No verdict yet"));
    assert!(html.contains(&sample_line(&lane).replace('\'', "&#39;")));
    assert!(html.contains("quality control of the engine, internal only, never in a client export"));
    assert!(!html.contains("engine-wrong-list"), "the list is closed");
}

#[test]
fn the_no_list_offers_the_eight_choices_with_digits() {
    let html = page("engine/no-list-open");
    assert!(html.contains("What was wrong?"));
    for (i, e) in EngineCheckError::ALL.into_iter().enumerate() {
        assert!(
            html.contains(camerata_ui_core::engine_check::error_label(e)),
            "{e:?}"
        );
        assert!(html.contains(&format!("engine-wrong-digit\">{}<", i + 1)));
    }
    assert!(html.contains("engine-wrong-item highlighted"));
}

#[test]
fn a_no_without_a_correction_shows_held_and_the_shell_counts_it() {
    let html = page("engine/no-pending-correction");
    assert!(html.contains("held from export until corrected"));
    assert!(html.contains("class=\"engine-state held\""));
    assert!(html.contains("Correct it"));
    let ds = build(&{
        let mut p = Params::base(SEED);
        p.engine_mode = EngineMode::NoWithoutCorrection;
        p
    });
    assert!(
        html.contains(&format!("Held {}", ds.shell.counts.held)),
        "the strip's Held count"
    );
}

#[test]
fn a_corrected_no_says_the_corrected_version_ships() {
    let html = page("engine/no-corrected");
    assert!(html.contains("No, corrected: the corrected version is what ships"));
    assert!(html.contains("class=\"engine-state corrected\""));
    assert!(!html.contains("Correct it"));
}

#[test]
fn all_verdicts_in_shows_the_final_rates_apart_with_their_honest_labels() {
    let html = page("engine/all-verdicts");
    assert!(html.contains("All verdicts are in."));
    assert!(html.contains("Engine Check report (operator only)"));
    assert!(html.contains("Error rate among reported findings:"));
    assert!(html.contains("Real problems among thrown-out findings:"));
    assert!(html.contains("\u{b1}"), "the margin");
    let word = ["accu", "racy"].concat();
    assert!(!html.to_lowercase().contains(&word));
}

#[test]
fn a_small_category_says_indicative_only() {
    let html = page("engine/small-category");
    assert!(html.contains("indicative only"), "{html}");
}

#[test]
fn lane_off_says_so_on_the_tab_and_in_the_body_with_a_way_to_settings() {
    let html = page("engine/lane-off");
    assert!(html.contains("Engine Check off"), "the tab");
    assert!(html.contains("Engine Check is off: the sample size is 0"));
    assert!(html.contains("Open Settings"));
    assert!(!html.contains("engine-card"), "no card");
}

#[test]
fn loading_shows_skeleton_rows() {
    let html = page("engine/loading");
    assert!(html.contains("Loading the Engine Check"));
    assert!(html.contains("tree-skel-row"));
}

#[test]
fn an_export_panel_door_to_an_engine_check_row_focuses_that_card() {
    // The host moves the lane to the card the door names; the screen then draws that card.
    let ds = build(&{
        let mut p = Params::base(SEED);
        p.engine_mode = EngineMode::NoWithoutCorrection;
        p
    });
    let lane = ds.engine.clone();
    let held = lane
        .cards
        .iter()
        .find(|c| c.site.held.is_some())
        .expect("a held card")
        .clone();
    let door = Door::EngineCard {
        key: held.site.site_key.clone(),
    };
    let at = card_for_door(&lane, &door).expect("the lane has the card");
    let mut state = LaneState::default();
    assert!(state.focus_door(&lane, &door));
    assert_eq!(state.card, at);

    #[derive(Clone, PartialEq, Props)]
    struct Case {
        lane: camerata_api_types::review_view::EngineCheckLane,
        state: LaneState,
    }
    fn app(case: Case) -> Element {
        rsx! {
            EngineScreen {
                lane: Some(case.lane),
                status: EngineStatus::Ready,
                state: case.state,
                focus: PaneFocus::Cursor,
            }
        }
    }
    let html = test_harness::render_fragment(app, Case { lane, state });
    assert!(html.contains(&format!("Card {} of ", at + 1)), "{html}");
    assert!(
        html.contains(&held.site.headline),
        "the door's card is the one on screen"
    );
    assert!(html.contains(&format!(
        "data-card=\"{}\"",
        camerata_ui_core::tree::dom_token(&held.site.site_key)
    )));
    assert!(html.contains("held from export until corrected"));
}

/// D4: one verdict is not a rate to trust. A stratum rate over fewer verdicts than the
/// small-category line says "indicative only", with a margin that is not zero.
#[test]
fn a_stratum_rate_over_one_verdict_says_indicative_only_and_is_not_certain() {
    for id in ["engine/no-pending-correction", "engine/no-corrected"] {
        let html = page(id);
        assert!(
            html.contains("Error rate among reported findings: 100% \u{b1} 40, indicative only (1 of 1 checked)"),
            "{id}: {html}"
        );
        assert!(
            !html.contains("100% \u{b1} 0"),
            "{id}: a zero margin reads as certainty"
        );
    }
}

/// D5 (spec 7): a corrected card shows the correction summary, so the reviewer sees what was
/// done: the ruling and its reason, and the edit chip the row carries.
#[test]
fn a_corrected_card_shows_what_was_done() {
    let html = page("engine/no-corrected");
    assert!(html.contains("class=\"engine-fix-summary\""), "{html}");
    assert!(
        html.contains("False positive: Engine Check said No; the row is not a real issue."),
        "the ruling and its reason: {html}"
    );
    // The verdict's own reason matches the correction made (a false positive is "not a real issue").
    assert!(
        html.contains("(not a real issue)"),
        "the state line names the same error the correction fixed: {html}"
    );
    // A card with nothing corrected shows no summary.
    for id in ["engine/card-pending", "engine/no-pending-correction"] {
        assert!(!page(id).contains("engine-fix-summary"), "{id}");
    }
}

fn thrown_out_ds(
    mode: EngineMode,
) -> camerata_ui_core::test_support::review_fixtures::ReviewDataSet {
    let mut p = Params::base(SEED);
    p.engine_mode = mode;
    build(&p)
}

/// Wave 17 soft (b): a thrown-out card held by an uncorrected No names both ways out and offers
/// the second as its own control.
#[test]
fn a_held_thrown_out_card_offers_put_it_back_and_confirm_thrown_out() {
    let html = page("engine/thrown-out-held");
    assert!(
        html.contains("thrown out by the engine"),
        "the stratum chip"
    );
    assert!(html.contains("Confirm thrown out"), "{html}");
    assert!(html.contains("Put it back into review"));
    assert!(html.contains("held from export until put back or confirmed"));
    assert!(
        html.contains("confirm it was rightly thrown out"),
        "the state line names the second way out"
    );
    assert!(
        !html.contains("held from export until corrected"),
        "the kept-row wording is not used on a thrown-out card"
    );
    // The shell's Held count and the export gate both carry the row.
    let ds = thrown_out_ds(EngineMode::ThrownOutNo);
    let base = dataset_base();
    assert_eq!(ds.shell.counts.held, base.shell.counts.held + 1);
    assert!(
        html.contains(&format!("Held {}", ds.shell.counts.held)),
        "the strip"
    );
    let card = ds
        .engine
        .cards
        .iter()
        .find(|c| c.verdict.is_some())
        .expect("the thrown-out card");
    assert!(ds.gate.held.iter().any(|h| h.key == card.site.site_key
        && h.reason
            == camerata_api_types::review_view::HoldReason::EngineCheckNoWithoutCorrection));
    // A kept row's hold keeps its own words and no confirmation.
    let kept = page("engine/no-pending-correction");
    assert!(kept.contains("held from export until corrected"));
    assert!(!kept.contains("Confirm thrown out"));
}

#[test]
fn a_confirmed_throw_out_shows_what_was_decided_and_no_confirm_control() {
    let html = page("engine/thrown-out-confirmed");
    assert!(html.contains("throw-out confirmed"), "{html}");
    assert!(html.contains("stays out of the client package"));
    assert!(!html.contains("Confirm thrown out"), "it is settled");
    assert!(!html.contains("Put it back into review"));
    assert!(!html.contains("what ships"));
    // Wave 19 D2: the engine was right, so the thrown-out rate is not "100%".
    assert!(
        html.contains("Real problems among thrown-out findings: 0% "),
        "{html}"
    );
    assert!(
        html.contains("False positive"),
        "the card shows the decision that settled it"
    );
    let ds = thrown_out_ds(EngineMode::ThrownOutConfirmed);
    assert_eq!(
        ds.shell.counts.held,
        dataset_base().shell.counts.held,
        "nothing holds the export on its account"
    );
}

/// Ruling 2 of 2026-11-04: when the per-category floor beats the sample size, the header says so.
#[test]
fn a_floor_binding_lane_says_the_floor_won_over_the_sample_size() {
    let html = page("engine/floor-binds");
    let lane = match fixture("engine/floor-binds").unwrap().body {
        camerata_ui_core::test_support::review_fixtures::Body::Engine(b) => match b.lane {
            camerata_ui_core::test_support::review_fixtures::Loaded::Ready(l) => l,
            other => panic!("{other:?}"),
        },
        _ => panic!("not an engine state"),
    };
    assert!(
        lane.drawn > lane.setting,
        "{} drawn of {}",
        lane.drawn,
        lane.setting
    );
    let line = sample_line(&lane);
    assert!(
        line.contains("at least 3 from each of 4 categories, above your sample size of 6"),
        "{line}"
    );
    assert!(html.contains(&line), "the header carries it");
    assert!(
        !html.contains("Allocated by unread share"),
        "the floor line replaces the share note"
    );
    for a in lane.allocation.iter().filter(|a| a.unread >= 3) {
        assert!(a.drawn >= 3, "{} drew {}", a.category, a.drawn);
    }
}
