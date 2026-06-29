# Human-in-the-loop escalation + resume: design

> **Status: DRAFT v0.1 (2026-06-29).** TOP PRIORITY. The escalation REVIEW surface is partly built
> (routines), but the load-bearing half — a paused agent that RESUMES from your decision — is not.
> Today a Governed Development run that hits a review-needed denial (e.g. the test-tamper guard)
> simply FAILS and leaves the worktree; there is no review surface and no resume. This doc designs
> the durable loop for BOTH pages, and answers: where it lives in the UI, the affordances, and the
> user's flow. Tracking: issue #43.

## The gap, stated plainly
A governed agent's whole value is that it can stop, ask you, and continue. Today:
- **Raise / See / Unblock** exist for **routines** (the dashboard's "blocked, needs review" panel).
- **Resume** exists nowhere: the decision is recorded (`resume_payload`) but no run consumes it, and
  routine runs are scripted (no live agent to resume).
- **Governed Development** has **no escalation surface at all**; the test-tamper guard fails the run.

So the loop is open exactly where it matters most.

## 1. Where it lives in the UI

**Governed Development** already has the right home: the left-nav **"NEEDS YOU"** section (today it
reads `NEEDS YOU (0)`). The design wires it:
- A Unit of Work whose run paused for review appears under **NEEDS YOU** with a badge; the count is
  the number of decisions waiting on you.
- Selecting that UoW opens its **Review panel** inline on the dev surface (the same right-hand area
  that shows a UoW's dev controls), not a separate page. One place to see it and act.
- A small global indicator (the NEEDS YOU count) so you notice it from any tab.

**Routines** keeps its existing inline "blocked, needs review" panel on the row (already built);
the same review component is shared, so both pages flow identically.

## 2. The affordances (yes: free text + approve / amend / reject)

When a run pauses, the Review panel shows:
- **What happened** — the rule it hit (e.g. `AGENTIC-NO-TEST-TAMPER-1`), the exact change that
  triggered it (the diff: which test was modified), and the agent's stated reasoning.
- **What you must decide** (the "stopped for" line).
- **Ask** — a chat with the lead-engineer agent to clarify. Chatting NEVER unblocks (it only helps
  you understand). This already exists for routines.
- **Decide** — a **free-text decision box** plus three explicit actions:
  - **Approve** — allow the change as-is; the agent resumes and continues (commits + proceeds).
  - **Amend / Redirect** — your free-text directive ("no, do it this way instead"); the agent
    resumes WITH the correction (it adjusts, then continues).
  - **Reject** — the change is reverted; the agent abandons that approach (and either tries another
    path or stops the run cleanly, your choice).

So your instinct is right: it is a free-text directive + Approve / Amend / Reject, with a clarifying
chat alongside. (The free-text answer is translated into a structured `ResumePayload` — the existing
AI-translation step, with a deterministic fallback — so an ambiguous answer re-escalates rather than
being applied blindly.)

## 3. Your flow, end to end (Governed Development)

1. You start a governed dev run on a UoW (its story is approved, contract written, gates passed).
2. The agent works behind the gate. It hits a review-needed denial (it modified an existing test, or
   any rule whose disposition is "escalate").
3. **Instead of failing, the run PAUSES** at that point and raises an escalation. The UoW moves to a
   `BlockedNeedsReview` state and appears under **NEEDS YOU**.
4. You open it, read what it did and why it stopped, optionally **Ask** to understand.
5. You **Approve**, **Amend** (with a directive), or **Reject**.
6. The agent **RESUMES** from your decision: it picks up its prior context + your directive and keeps
   working to the next checkpoint or to completion. The decision is recorded on the UoW's provenance
   (who decided what, when) so the trail is auditable.

The same six steps describe a routine, except step 1 is a scheduled fire and the surface is the
routine row.

## 4. Architecture: resume by CHECKPOINT + RE-SPAWN (not a live suspended process)

Keeping a live agent process blocked for minutes/hours (Option B) is fragile (timeouts, lost
connections, a process pinned waiting). The robust design is **checkpoint + re-spawn** (Option A):

- When the gate denies a write whose disposition is **escalate** (rather than hard-deny), the run
  does NOT fail. It:
  1. **Checkpoints**: the worktree is already on disk; persist the run's progress (plan/step index),
     the agent's conversation/context so far, and the pending action (the denied write + the rule +
     the reason).
  2. **Raises an escalation tied to the UoW** (escalations become UoW-scoped, not routine-only).
  3. Sets the UoW `BlockedNeedsReview` → NEEDS YOU.
- On your decision, **RESUME = re-spawn** the agent from the checkpoint with your directive injected
  into its context ("approved, proceed" / "do X instead" / "that change is rejected, revert it").
  The agent continues from where it paused. No long-lived blocked process; the pause is durable
  (survives an app restart) because it is persisted state, not a held thread.

This reuses the existing run engine (spawn an agent behind the gate); it adds a checkpoint record, a
UoW-scoped escalation, and a resume entry point that threads the directive in.

### What changes vs today
- Escalations: generalize from routine-scoped to **{routine | uow}**-scoped (a `subject` field).
- The gate's review-needed dispositions (test-tamper "escalate", and any rule set to escalate) call
  a **pause+checkpoint** path instead of `fail()`.
- A **resume** run path: re-spawn from the checkpoint + the `ResumePayload`.
- UI: the NEEDS YOU wiring + the shared Review panel on the UoW dev surface (reuse the routines
  panel component) + Approve/Amend/Reject actions.

## 5. Build phases (proposed)

1. **UoW-scoped escalations + the NEEDS YOU surface + the Review panel on the dev page** (Approve /
   Amend / Reject + chat), raising on the test-tamper guard instead of failing. This makes the
   review loop VISIBLE in Governed Development even before full re-spawn resume.
2. **Checkpoint persistence**: capture the run's resumable state at the pause.
3. **Resume = re-spawn from checkpoint + directive.** Closes the loop. The hardest part; do it on the
   real live run path (`CAMERATA_LIVE_BUILD`).
4. **Routines resume**: thread the recorded directive into the routine's next run (cheap, since
   routine runs are scripted/lightweight).
5. **Tests throughout**: extend `escalation_flow_e2e` to assert resume applies the directive (the
   marked TODO there), a UoW-escalation integration test, and a live-run pause/resume E2E (hermetic
   with the scripted gate).

## 6. Open decisions
- **OPEN-1:** Is the global discovery surface ONLY the NEEDS YOU nav count, or also a toast /
  notification when a run pauses while you are elsewhere? (My lean: NEEDS YOU count + a toast.)
- **OPEN-2:** On **Reject**, does the agent try an alternative approach automatically, or stop the
  run for you to restart? (My lean: stop cleanly; auto-retry is a later refinement.)
- **OPEN-3:** Which rules are "escalate" vs "hard-deny"? Security floor stays hard-deny (never
  pausable). Test-tamper is escalate. Make the disposition a per-rule property the corpus carries.
- **OPEN-4:** Phase 1 (visible review surface, raise-not-fail) before the resume engine is real —
  acceptable as an interim (you can SEE + decide, the run still has to be re-run), or hold until
  resume lands? (My lean: ship Phase 1 interim; it is strictly better than a silent fail.)

Tell me your calls on §6 and confirm the UX in §1-3, and I will start with Phase 1.
