//! The Engine Check host's reads and writes, and the honest-label scan.

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review_view::{CorrectionRef, EngineCheckError};
use camerata_ui::review_io::{RecordingWrites, Written};
use camerata_ui::review_source::FixtureReviewSource;
use camerata_ui_core::engine_check::{Correction, CorrectionKind, SEVERITIES};
use camerata_ui_core::test_support::review_fixtures::dataset_base;
use camerata_ui_core::triage::PaneFocus;

use super::*;

fn card() -> EngineCheckCard {
    dataset_base().engine.cards[0].clone()
}

#[tokio::test]
async fn engine_check_no_without_correction_marks_held() {
    // Esc after choosing what was wrong, or Next: the No is stored with no correction, which is
    // what holds the row from export and raises the shell's Held count.
    let writes = RecordingWrites::default();
    let c = card();
    store_pending_no(&writes, &c.site.site_key, EngineCheckError::WrongSeverity)
        .await
        .unwrap();
    let log = writes.log.borrow();
    assert_eq!(log.len(), 1);
    let Written::EngineVerdict(key, req) = &log[0] else {
        panic!("a verdict: {:?}", log[0]);
    };
    assert_eq!(key, &c.site.site_key);
    assert!(!req.right);
    assert_eq!(req.wrong, Some(EngineCheckError::WrongSeverity));
    assert_eq!(req.correction, None, "no correction yet");
}

#[tokio::test]
async fn a_committed_correction_posts_the_decision_then_the_verdict_that_names_it() {
    let writes = RecordingWrites::default();
    let c = card();
    let mut f = Correction::start(EngineCheckError::NotARealIssue, &c);
    f.reason = "the policy is on a view".to_string();
    commit_correction(&writes, &f, &c, "insp-1").await.unwrap();
    let log = writes.log.borrow();
    assert_eq!(log.len(), 2);
    let Written::Decision(d) = &log[0] else {
        panic!("the decision first: {:?}", log[0]);
    };
    assert_eq!(d.state, DecisionState::FalsePositive);
    assert_eq!(d.key, c.site.site_key);
    assert_eq!(d.run_id, "insp-1");
    let Written::EngineVerdict(_, v) = &log[1] else {
        panic!("then the verdict: {:?}", log[1]);
    };
    assert_eq!(v.wrong, Some(EngineCheckError::NotARealIssue));
    assert_eq!(
        v.correction,
        Some(CorrectionRef::Decision("dec-000001".to_string()))
    );
}

#[tokio::test]
async fn an_edit_correction_names_the_edit_in_the_verdict() {
    let writes = RecordingWrites::default();
    let c = card();
    let mut f = Correction::start(EngineCheckError::WrongSeverity, &c);
    f.value = SEVERITIES
        .iter()
        .find(|s| **s != c.site.severity)
        .unwrap()
        .to_string();
    f.reason = "reachable without sign-in".to_string();
    commit_correction(&writes, &f, &c, "insp-1").await.unwrap();
    let log = writes.log.borrow();
    let Written::Edit(e) = &log[0] else {
        panic!("an edit: {:?}", log[0]);
    };
    assert_eq!(e.field, camerata_api_types::edits::EditField::Severity);
    let Written::EngineVerdict(_, v) = &log[1] else {
        panic!("{:?}", log[1]);
    };
    assert_eq!(
        v.correction,
        Some(CorrectionRef::Edit("edit-000001".to_string()))
    );
}

#[tokio::test]
async fn a_missing_reason_or_a_refused_correction_stores_no_verdict() {
    let c = card();
    // Nothing is posted when the form is incomplete.
    let writes = RecordingWrites::default();
    let f = Correction::start(EngineCheckError::NotARealIssue, &c);
    let err = commit_correction(&writes, &f, &c, "insp-1")
        .await
        .unwrap_err();
    assert!(err.starts_with("A reason is required"), "{err}");
    assert!(writes.log.borrow().is_empty());
    // A correction the server refuses is not followed by a verdict claiming it was made.
    let writes = RecordingWrites::default();
    *writes.refuse.borrow_mut() = Some("The server said no.".to_string());
    let mut f = Correction::start(EngineCheckError::NotARealIssue, &c);
    f.reason = "r".to_string();
    let err = commit_correction(&writes, &f, &c, "insp-1")
        .await
        .unwrap_err();
    assert_eq!(err, "The server said no.");
    assert!(writes.log.borrow().is_empty());
}

#[tokio::test]
async fn a_thrown_out_no_puts_the_row_back_and_names_the_restore() {
    let writes = RecordingWrites::default();
    let mut c = card();
    c.stratum = SampleStratum::ThrownOut;
    let mut f = Correction::start(EngineCheckError::ThrownOutRealProblem, &c);
    assert_eq!(f.kind, CorrectionKind::Restore);
    f.reason = "a real problem: the query is built from input".to_string();
    commit_correction(&writes, &f, &c, "insp-1").await.unwrap();
    let log = writes.log.borrow();
    let Written::Decision(d) = &log[0] else {
        panic!("{:?}", log[0]);
    };
    assert_eq!(d.state, DecisionState::EngineRestored);
}

/// Confirming the throw-out posts a false positive on the thrown-out row (never a put-back) and
/// then the verdict that names it, so the gate's hold clears and the row stays out.
#[tokio::test]
async fn confirming_a_throw_out_posts_a_false_positive_then_the_verdict_naming_it() {
    let writes = RecordingWrites::default();
    let mut c = card();
    c.stratum = SampleStratum::ThrownOut;
    let mut f = Correction::confirm_throw_out(&c);
    assert_eq!(f.kind, CorrectionKind::ConfirmThrownOut);
    f.reason = "a copy of a row the report already carries".to_string();
    commit_correction(&writes, &f, &c, "insp-1").await.unwrap();
    let log = writes.log.borrow();
    assert_eq!(log.len(), 2);
    let Written::Decision(d) = &log[0] else {
        panic!("{:?}", log[0]);
    };
    assert_eq!(d.state, DecisionState::FalsePositive);
    assert_eq!(d.key, c.site.site_key);
    let Written::EngineVerdict(key, req) = &log[1] else {
        panic!("{:?}", log[1]);
    };
    assert_eq!(key, &c.site.site_key);
    assert!(!req.right);
    assert_eq!(req.wrong, Some(EngineCheckError::ThrownOutRealProblem));
    assert!(matches!(req.correction, Some(CorrectionRef::Decision(_))));
}

#[tokio::test]
async fn confirming_a_throw_out_without_a_reason_writes_nothing() {
    let writes = RecordingWrites::default();
    let mut c = card();
    c.stratum = SampleStratum::ThrownOut;
    let f = Correction::confirm_throw_out(&c);
    let err = commit_correction(&writes, &f, &c, "insp-1")
        .await
        .unwrap_err();
    assert!(err.contains("A reason is required"), "{err}");
    assert!(writes.log.borrow().is_empty(), "no decision, no verdict");
}

#[derive(Clone, PartialEq, Props)]
struct FormCase {
    lane: EngineCheckLane,
    state: LaneState,
}

fn form_page(case: FormCase) -> Element {
    rsx! {
        EngineScreen {
            lane: Some(case.lane),
            status: EngineStatus::Ready,
            state: case.state,
            focus: PaneFocus::Cursor,
        }
    }
}

/// The control opens the confirmation in place with a required reason, and leaving it stores
/// nothing: the No it stands behind is already stored.
#[test]
fn the_confirm_form_asks_for_a_reason_and_names_what_it_does() {
    let mut lane = dataset_base().engine;
    let at = lane
        .cards
        .iter()
        .position(|c| c.stratum == SampleStratum::ThrownOut)
        .expect("a thrown-out card");
    {
        let card = &mut lane.cards[at];
        card.site.held =
            Some(camerata_api_types::review_view::HoldReason::EngineCheckNoWithoutCorrection);
        card.verdict = Some(camerata_api_types::review_view::EngineCheckVerdict {
            key: card.site.site_key.clone(),
            run_id: lane.run_id.clone(),
            right: false,
            wrong: Some(EngineCheckError::ThrownOutRealProblem),
            correction: None,
            by: "architect".to_string(),
            at: "2026-11-04T09:00:00Z".to_string(),
            carried_from: None,
        });
    }
    let mut state = LaneState::default();
    state.go(at, lane.cards.len());
    let closed = crate::test_harness::render_fragment(
        form_page,
        FormCase {
            lane: lane.clone(),
            state: state.clone(),
        },
    );
    assert!(closed.contains("Confirm thrown out"), "{closed}");
    assert!(!closed.contains("engine-correction"), "no form until asked");
    state.begin_confirm(&lane.cards[at]);
    let open = crate::test_harness::render_fragment(form_page, FormCase { lane, state });
    assert!(open.contains("Confirm it was rightly thrown out"), "{open}");
    assert!(open.contains("Why (required"), "the reason is required");
    assert!(open.contains("Commit the correction"));
    assert!(
        !open.contains("Confirm thrown out"),
        "the button gives way to the form"
    );
}

#[tokio::test]
async fn the_merge_picker_reads_the_rows_of_the_cards_sub_group() {
    let reads = FixtureReviewSource::base();
    let c = card();
    let options = read_merge_options(&reads, &c).await.unwrap();
    assert!(
        options.iter().any(|(k, _)| *k == c.site.site_key),
        "the card's own row is among them"
    );
    let targets = camerata_ui_core::engine_check::merge_targets(&c, &options);
    assert!(targets.iter().all(|(k, _)| *k != c.site.site_key));
    assert!(!targets.is_empty());
    assert!(reads.calls().iter().all(|c| c == "sites_page"));
}

/// Design A10 item 6: no UI string calls an Engine Check rate by the word that claims the check
/// sees what the engine missed. The sources of the lane are scanned for it.
#[test]
fn no_engine_check_ui_string_says_accuracy() {
    let word = ["accu", "racy"].concat();
    for (name, source) in [
        (
            "cockpit/engine_check.rs",
            include_str!("../engine_check.rs"),
        ),
        (
            "gallery/engine_states.rs",
            include_str!("../../gallery/engine_states.rs"),
        ),
        (
            "ui-core engine_check.rs",
            include_str!("../../../../ui-core/src/engine_check.rs"),
        ),
    ] {
        for (n, line) in source.lines().enumerate() {
            assert!(
                !line.to_lowercase().contains(&word),
                "{name}:{}: {line}",
                n + 1
            );
        }
    }
}
