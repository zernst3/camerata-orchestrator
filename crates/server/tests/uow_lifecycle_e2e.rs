//! Hermetic end-to-end regression net for the GOVERNED-DEV LIFECYCLE state machine
//! (Pillar 2).
//!
//! THE CLAIM under test: a story's Unit of Work can only walk the lifecycle
//! `Intake → Investigating → DecisionsApproved → Development → AwaitingQa → SignedOff`
//! through the real store transition methods, the no-code-first DECISION GATE is enforced
//! at both `approve_decisions` and `start_development`, the R3.g CONTRACT precondition
//! blocks a boundary-crossing story until a contract is written, `from-workitem` DEDUPES,
//! and a critical SOC-2 finding raises the sign-off BLOCK signal.
//!
//! HERMETIC: NO network, NO `claude`/process spawn, NO real scan. Everything is driven
//! through `AppState::seeded()` + the public `UowStore` API + the pure `crate::lifecycle`
//! state machine + the real `ensure_development_gate` seam. The in-memory artifact store
//! is NOT attached (the default `#[tokio::test]` current-thread harness degrades the store
//! path gracefully), so decisions live in the inline cache the gate reads.

use camerata_server::lifecycle::{TransitionError, UowStage};
use camerata_server::uow::UowStore;
use camerata_server::{ensure_development_gate, AppState};

use camerata_worktracker::investigation::DecisionRecord;
use chrono::Utc;

// ════════════════════════════════════════════════════════════════════════════════════
// Shared fixtures
// ════════════════════════════════════════════════════════════════════════════════════

fn approved(story: &str, slug: &str) -> DecisionRecord {
    DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/{slug}"),
        "Decision",
        "Question?",
        "Rationale",
        vec![],
        Utc::now(),
    )
    .approve(Utc::now())
}

fn pending(story: &str, slug: &str) -> DecisionRecord {
    DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/{slug}"),
        "Decision",
        "Question?",
        "Rationale",
        vec![],
        Utc::now(),
    )
}

fn rejected(story: &str, slug: &str) -> DecisionRecord {
    DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/{slug}"),
        "Decision",
        "Question?",
        "Rationale",
        vec![],
        Utc::now(),
    )
    .reject("not acceptable", Utc::now())
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 1 — The full stage progression through the REAL store transition methods
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope1_full_progression_walks_every_stage_in_order() {
    let store = UowStore::new();
    let s = "S-walk";

    assert_eq!(
        store.get_or_create(s).stage,
        UowStage::Intake,
        "a new UoW starts at Intake"
    );

    let after_inv = store.begin_investigation(s).expect("Intake -> Investigating");
    assert_eq!(after_inv.stage, UowStage::Investigating);

    store.set_decisions(s, vec![approved(s, "a"), approved(s, "b")]);
    let after_approve = store.approve_decisions(s).expect("Investigating -> DecisionsApproved");
    assert_eq!(after_approve.stage, UowStage::DecisionsApproved);

    let after_dev = store.start_development(s).expect("DecisionsApproved -> Development");
    assert_eq!(after_dev.stage, UowStage::Development);

    let after_qa = store.finish_development(s).expect("Development -> AwaitingQa");
    assert_eq!(after_qa.stage, UowStage::AwaitingQa);

    let after_signoff = store.sign_off(s, "zach", "run-1", None);
    assert_eq!(
        after_signoff.stage,
        UowStage::SignedOff,
        "sign-off from AwaitingQa advances to SignedOff"
    );
}

#[test]
fn scope1_invalid_transitions_are_refused_and_leave_stage_unchanged() {
    let store = UowStore::new();

    // start_development directly from Intake is illegal (needs DecisionsApproved first).
    let s1 = "S-skip-dev";
    store.set_decisions(s1, vec![approved(s1, "a")]); // even with approved decisions...
    let err = store.start_development(s1).unwrap_err();
    assert!(
        matches!(
            err,
            TransitionError::WrongStage {
                attempted: "start_development",
                from: UowStage::Intake,
                expected: UowStage::DecisionsApproved,
            }
        ),
        "start_development from Intake must be a WrongStage error, got {err:?}"
    );
    assert_eq!(
        store.get_or_create(s1).stage,
        UowStage::Intake,
        "a refused transition leaves the stage unchanged"
    );

    // finish_development from Intake is illegal.
    let s2 = "S-skip-finish";
    let err = store.finish_development(s2).unwrap_err();
    assert!(matches!(
        err,
        TransitionError::WrongStage {
            attempted: "finish_development",
            ..
        }
    ));
    assert_eq!(store.get_or_create(s2).stage, UowStage::Intake);

    // sign_off transition from Investigating is illegal at the pure-machine level:
    // `UowStore::sign_off` records the sign-off but never fabricates the stage jump.
    let s3 = "S-early-signoff";
    store.begin_investigation(s3).unwrap();
    let uow = store.sign_off(s3, "zach", "run-x", None);
    assert!(uow.sign_off.is_some(), "the sign-off record is still written");
    assert_eq!(
        uow.stage,
        UowStage::Investigating,
        "but the stage is NOT illegally advanced to SignedOff from Investigating"
    );

    // begin_investigation twice: the second is refused.
    let s4 = "S-double-begin";
    store.begin_investigation(s4).unwrap();
    assert!(store.begin_investigation(s4).is_err());
    assert_eq!(store.get_or_create(s4).stage, UowStage::Investigating);
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 2 — The no-code-first DECISION GATE: blocked-then-allowed
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope2_decision_gate_blocks_into_development_until_every_decision_approved() {
    let store = UowStore::new();
    let s = "S-gate";
    store.begin_investigation(s).unwrap();

    // (a) No decisions at all: blocked.
    let err = store.approve_decisions(s).unwrap_err();
    assert!(
        matches!(err, TransitionError::DecisionsNotApproved { total: 0, .. }),
        "an empty decision set blocks the gate, got {err:?}"
    );

    // (b) A PENDING decision present: blocked.
    store.set_decisions(s, vec![approved(s, "a"), pending(s, "b")]);
    let err = store.approve_decisions(s).unwrap_err();
    assert!(
        matches!(
            err,
            TransitionError::DecisionsNotApproved {
                total: 2,
                unapproved: 1
            }
        ),
        "a pending decision blocks the gate, got {err:?}"
    );
    assert_eq!(store.get_or_create(s).stage, UowStage::Investigating);

    // (c) A REJECTED decision present: still blocked (rejected is not approved).
    store.set_decisions(s, vec![approved(s, "a"), rejected(s, "b")]);
    let err = store.approve_decisions(s).unwrap_err();
    assert!(
        matches!(err, TransitionError::DecisionsNotApproved { total: 2, .. }),
        "a rejected decision blocks the gate, got {err:?}"
    );
    assert_eq!(store.get_or_create(s).stage, UowStage::Investigating);

    // (d) Every decision approved: ALLOWED.
    store.set_decisions(s, vec![approved(s, "a"), approved(s, "b")]);
    let ok = store.approve_decisions(s).expect("gate opens when all approved");
    assert_eq!(ok.stage, UowStage::DecisionsApproved);
}

#[test]
fn scope2_start_development_rechecks_the_gate_after_a_reopen() {
    // The re-check at the point of no return: a decision re-opened AFTER approval must
    // re-block start_development even though the stage already reached DecisionsApproved.
    let store = UowStore::new();
    let s = "S-recheck";
    store.begin_investigation(s).unwrap();
    store.set_decisions(s, vec![approved(s, "a")]);
    store.approve_decisions(s).unwrap();
    assert_eq!(store.get_or_create(s).stage, UowStage::DecisionsApproved);

    // Re-open the decision (now Pending). start_development must re-block.
    store.set_decisions(s, vec![pending(s, "a")]);
    let err = store.start_development(s).unwrap_err();
    assert!(
        matches!(err, TransitionError::DecisionsNotApproved { .. }),
        "start_development must re-check the decision gate, got {err:?}"
    );
    assert_eq!(
        store.get_or_create(s).stage,
        UowStage::DecisionsApproved,
        "the stage is not advanced when the re-check blocks"
    );

    // Re-approve: the gate opens.
    store.set_decisions(s, vec![approved(s, "a")]);
    let ok = store.start_development(s).expect("re-approved -> Development");
    assert_eq!(ok.stage, UowStage::Development);
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3 — The R3.g CONTRACT precondition, through the REAL `ensure_development_gate`
//   seam (the gate the governed-run start calls). Blocked when the work crosses a
//   contract boundary and no contract is written; passes once a contract is set.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope3_contract_precondition_blocks_boundary_crossing_until_contract_written() {
    let state = AppState::seeded();
    let s = "owner/repo#101";

    // Decisions are approved so the contract precondition (which is checked AFTER the
    // decision gate in ensure_development_gate) is the thing under test.
    state.uow().set_decisions(s, vec![approved(s, "a")]);

    // Mark the work as crossing a contract boundary with NO contract written.
    state.uow().set_contract(s, "", true);
    let blocked = ensure_development_gate(&state, s);
    assert!(
        blocked.is_err(),
        "a boundary-crossing story with no contract must be blocked"
    );
    let msg = blocked.unwrap_err();
    assert!(
        msg.contains("contract") && msg.contains("R3.g"),
        "the block message must name the contract precondition (R3.g): {msg}"
    );

    // A whitespace-only contract is still empty -> still blocked.
    state.uow().set_contract(s, "   \n\t ", true);
    assert!(
        ensure_development_gate(&state, s).is_err(),
        "a whitespace-only contract is treated as empty and still blocks"
    );

    // Write a real contract -> the precondition passes (gate returns Ok).
    state
        .uow()
        .set_contract(s, "The /widgets endpoint returns {id, name}.", true);
    assert!(
        ensure_development_gate(&state, s).is_ok(),
        "a written contract satisfies the R3.g precondition"
    );
}

#[test]
fn scope3_no_contract_required_when_work_does_not_cross_a_boundary() {
    let state = AppState::seeded();
    let s = "owner/repo#102";
    state.uow().set_decisions(s, vec![approved(s, "a")]);

    // crosses_boundary = false: an empty contract is fine.
    state.uow().set_contract(s, "", false);
    assert!(
        ensure_development_gate(&state, s).is_ok(),
        "a story that does not cross a boundary needs no contract"
    );
}

#[test]
fn scope3_decision_gate_blocks_in_ensure_development_gate_before_contract() {
    // ensure_development_gate enforces the decision gate FIRST: with no approved
    // decisions it must block regardless of the contract state.
    let state = AppState::seeded();
    let s = "owner/repo#103";
    state.uow().set_contract(s, "a real contract", false);

    // No decisions: blocked on the decision gate.
    let err = ensure_development_gate(&state, s).unwrap_err();
    assert!(
        err.contains("decision"),
        "the decision gate blocks first: {err}"
    );

    // A pending decision: still blocked.
    state.uow().set_decisions(s, vec![pending(s, "a")]);
    assert!(ensure_development_gate(&state, s).is_err());

    // Approve it: now the gate passes (no boundary crossing, contract irrelevant).
    state.uow().set_decisions(s, vec![approved(s, "a")]);
    assert!(ensure_development_gate(&state, s).is_ok());
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 4 — from-workitem DEDUP: the same work item never produces a duplicate UoW.
//   The handler's dedup is project-scoped (`list_for_project`) + `get_or_create`. We
//   exercise the exact store behavior the handler relies on.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope4_from_workitem_dedups_within_the_active_project() {
    let state = AppState::seeded();
    // Activate a project whose repos cover the work item's repo so the dedup scope applies.
    let proj = state
        .projects()
        .create("Dedup", vec!["o/r".to_string()])
        .expect("create active project");
    let story_id = "o/r#7"; // the spine story id for work item o/r#7

    // First creation: materialize the UoW (mirrors the handler's get_or_create).
    let first = state.uow().get_or_create(story_id);
    assert_eq!(first.story_id, story_id);

    // The handler's dedup check: is there already a UoW for this story in the project?
    let already = state
        .uow()
        .list_for_project(&proj.id, &proj.repos)
        .iter()
        .any(|u| u.story_id == story_id);
    assert!(
        already,
        "the first UoW must be visible in the project's view (so the second call dedups)"
    );

    // Second creation of the SAME work item: get_or_create returns the EXISTING UoW,
    // never a duplicate. The total count for this story id stays exactly one.
    let second = state.uow().get_or_create(story_id);
    assert_eq!(second.story_id, first.story_id);
    let count = state
        .uow()
        .list()
        .iter()
        .filter(|u| u.story_id == story_id)
        .count();
    assert_eq!(count, 1, "the same work item must never create a duplicate UoW");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 5 — sign-off BLOCK: a critical SOC-2 scoped-scan finding raises the block signal.
//   The waive-with-reason enforcement itself lives in the private sign_off_run handler;
//   here we assert the reachable building block — attaching critical-finding evidence sets
//   the `is_sign_off_blocked` signal the handler reads, and non-critical evidence does not.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope5_critical_scoped_finding_raises_sign_off_block_signal() {
    use camerata_server::evidence::{ScopedScanSummary, UowEvidenceRecord};

    let store = UowStore::new();
    let s = "S-evidence";

    // Walk to AwaitingQa so a sign-off would otherwise be legal.
    store.begin_investigation(s).unwrap();
    store.set_decisions(s, vec![approved(s, "a")]);
    store.approve_decisions(s).unwrap();
    store.start_development(s).unwrap();
    store.finish_development(s).unwrap();
    assert_eq!(store.get_or_create(s).stage, UowStage::AwaitingQa);

    // No evidence yet -> not blocked (the gate only blocks on an EXISTING critical finding).
    assert!(
        !store.is_sign_off_blocked(s),
        "no evidence record means no block"
    );

    // Attach evidence WITHOUT a critical finding -> still not blocked.
    let mut clean = UowEvidenceRecord::new(s, "run-1", Utc::now().to_rfc3339());
    clean.set_scoped_scan(ScopedScanSummary {
        files_scanned: 3,
        total_findings: 1,
        has_critical: false,
        findings: vec![],
    });
    store.attach_evidence(s, clean);
    assert!(
        !store.is_sign_off_blocked(s),
        "a non-critical scoped finding does not block sign-off"
    );

    // Attach evidence WITH a critical finding -> blocked.
    let mut critical = UowEvidenceRecord::new(s, "run-2", Utc::now().to_rfc3339());
    critical.set_scoped_scan(ScopedScanSummary {
        files_scanned: 3,
        total_findings: 2,
        has_critical: true,
        findings: vec![],
    });
    store.attach_evidence(s, critical);
    assert!(
        store.is_sign_off_blocked(s),
        "a CRITICAL scoped finding raises the sign-off block signal the handler enforces"
    );
}
