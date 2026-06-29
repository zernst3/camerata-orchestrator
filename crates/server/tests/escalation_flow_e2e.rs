//! Hermetic end-to-end net for the human-in-the-loop ESCALATION review loop (Routines): the parts
//! that are BUILT today, at the seam level the HTTP handlers orchestrate.
//!
//! HONEST SCOPE. This asserts the loop that is real right now: a blocked run RAISES a deduped
//! escalation, it is DISCOVERABLE (`list_open` / `open_for_routine`), and a human answer RESOLVES
//! it, durably recording the decision (`human_answer` + `translated_directive` + the structured
//! `resume_payload`) and returning the routine to `Idle`. It does NOT assert agent RESUMPTION from
//! the decision, because that is not built (issue #43): `resume_payload` is recorded for a future
//! resumed run to consult, but no run path consumes it yet, and routine runs are scripted (no live
//! agent to suspend). These asserts pin the CURRENT contract and extend cleanly when resume lands.
//!
//! HERMETIC: in-memory stores, the scripted gate (`run_now` -> `run_event_script`), and the
//! deterministic `scaffold_resume_payload` (the AI translator is the live path, exercised
//! elsewhere). No network, no AI, no process spawn.

use camerata_server::escalation::{
    raise_if_blocked, scaffold_resume_payload, EscalationStatus, EscalationStore,
};
use camerata_server::routine::{CreateRoutineReq, RoutineStatus, RoutineStore};

fn mk_routine(store: &RoutineStore) -> camerata_server::routine::Routine {
    store.create(&CreateRoutineReq {
        name: "Nightly audit".into(),
        schedule: "daily 04:00".into(),
        intent: "audit dependencies".into(),
        prompt: "P".into(),
        scope: "read-only".into(),
        model: None,
        project_id: None,
    })
}

#[test]
fn blocked_run_raises_escalation_then_human_answer_resolves_and_records_the_decision() {
    let routines = RoutineStore::new();
    let escalations = EscalationStore::new();
    let r = mk_routine(&routines);

    // 1. RUN: the scripted gate blocks it (2 denies, 1 allow), so the run lands BlockedNeedsReview.
    let ran = routines.run_now(&r.id).unwrap();
    assert_eq!(ran.status, RoutineStatus::BlockedNeedsReview);
    assert_eq!(ran.last_run.as_ref().unwrap().denies, 2);

    // 2. RAISE: the blocked run raises a deduped escalation; link it to the run-history entry.
    let esc_id = raise_if_blocked(&escalations, &ran).expect("a blocked run raises an escalation");
    routines.link_last_run_escalation(&r.id, &esc_id);

    // 3. SEE: it is discoverable as an open, standalone-readable escalation for this routine.
    assert_eq!(escalations.list_open().len(), 1);
    let open = escalations
        .open_for_routine(&r.id)
        .expect("open escalation for the routine");
    assert_eq!(open.id, esc_id);
    assert_eq!(open.status, EscalationStatus::Open);
    assert!(!open.reason.is_empty(), "the review states WHY it stopped");
    assert!(
        !open.stopped_for.is_empty(),
        "the human is told WHAT decision is needed"
    );
    assert!(open.human_answer.is_none());

    // Dedup: raising again while one is open returns the SAME escalation, not a second.
    let again = raise_if_blocked(&escalations, &ran);
    assert_eq!(again.as_deref(), Some(esc_id.as_str()));
    assert_eq!(
        escalations.list_open().len(),
        1,
        "at most one open review per routine"
    );

    // 4. UNBLOCK: a human answer resolves it. The answer is translated into a structured resume
    //    payload (deterministic scaffold here; the AI translator is the live path) and durably
    //    recorded on the escalation.
    let answer = "Approved: the modified test reflects the new intended behavior, proceed.";
    let payload = scaffold_resume_payload(&open, answer);
    let resolved = escalations
        .resolve_with_payload(&esc_id, answer, &payload)
        .expect("the answer resolves the open escalation");
    assert_eq!(resolved.status, EscalationStatus::Resolved);
    assert_eq!(resolved.human_answer.as_deref(), Some(answer));
    assert!(
        resolved.translated_directive.is_some(),
        "the decision is rendered into a resume directive (for the UI)"
    );
    // The CONTRACT the resume gap (#43) will consume: the STRUCTURED payload is durably recorded.
    let rp = resolved
        .resume_payload
        .as_ref()
        .expect("structured resume payload recorded for a future resumed run");
    assert!(!rp.directive.is_empty());

    // 5. The routine returns to Idle so its next slot can run (what the answer handler does).
    let idle = routines.set_status(&r.id, RoutineStatus::Idle).unwrap();
    assert_eq!(idle.status, RoutineStatus::Idle);

    // 6. It is no longer open; resolving an already-resolved escalation is a no-op.
    assert!(escalations.list_open().is_empty());
    assert!(escalations.open_for_routine(&r.id).is_none());
    assert!(escalations
        .resolve_with_payload(&esc_id, answer, &payload)
        .is_none());

    // 7. The run-history entry links to the escalation, so a dashboard row can jump to the review.
    let runs = routines.runs(&r.id).unwrap();
    assert_eq!(runs[0].escalation_id.as_deref(), Some(esc_id.as_str()));

    // NOTE (#43, the resume gap): `resume_payload` above is RECORDED but not yet CONSUMED. No run
    // path resumes a live agent from it, and routine runs are scripted (nothing to suspend). When
    // resume is built, extend this test to assert the next run applies `rp.directive`.
}

#[test]
fn a_clean_run_raises_no_escalation() {
    // raise_if_blocked is a no-op when the run had no denies (only blocked runs escalate). We can't
    // make the scripted gate pass, so assert the guard directly: a routine with a clean last_run
    // (denies == 0) does not raise.
    let routines = RoutineStore::new();
    let escalations = EscalationStore::new();
    let r = mk_routine(&routines);
    // No run yet -> last_run is None -> denies treated as 0 -> no escalation.
    assert!(raise_if_blocked(&escalations, &r).is_none());
    assert!(escalations.list_open().is_empty());
}
