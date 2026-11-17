# ADR: The Possible false positive flag (an operator review aid)

**Date:** 2026-11-15
**Status:** Proposed (branch `feat/possible-false-positive`, into `feat/audit-report-export`).

## Context

The operator reviews every row before a client sees anything. False alarms at medium or higher
cost the most review time on every test fixture. A separate round attacks them at the source. This
flag is the review aid asked for in addition: each row the engine itself has reasons to doubt
carries a visible flag, so the operator can look at those rows first and rule them out quickly.

The owner's rule bounds it: anything that might leave a real violation unremediated is a defect.
The reviewer, not the engine, removes a false alarm.

## Decision

1. **A closed list of reasons, worked out once, with no model call.**
   `camerata_api_types::review::FpReason` has seven variants, (a) to (g).
   `crate::possible_false_positive::FpIndex` works them out from data the run already holds. The
   review view and the internal exports both call it; neither works a reason out itself.
2. **A flag only.** It never hides, drops, merges or lowers a row. It changes no severity, kind,
   escalation, row count or decision, and it never holds an export. A flagged row stays a problem
   until the operator rules it out ("Ruled out", the existing term).
3. **The operator only.** It appears in the review UI (a chip, the reasons in the detail and the
   queue's "Possible false positives first" look) and in internal packages (a technical-band
   column, a `findings.json` key and a Start here count). A client package carries none of it.
4. **Vocabulary.** "Possible false positive" is its own term for its own concept, a reviewers-only
   one (`vocabulary::INTERNAL_TERMS`). The retired-word check blanks an owned term before it
   matches, so "Possible false positive" passes while a bare "false positive" is still refused in
   favour of "Ruled out".
5. **Choices made inside the brief.**
   - Reason (a) also fires when the group reviewer says the row is not real ("no", or "unclear"
     against stage 1's "yes"), besides a lower severity. Detection defaults to recall
     (ORCH-PRECISION-RECALL-1).
   - Reason (b) ignores a `needs_review` that only test scope set. Calibration sets it on every
     test-scoped row, and counting it would flag a secret committed in a test by the back door.
   - Reason (f) needs the recorded rule-minimum note (`camerata_rules::severity_bounds::
     MIN_SEVERITY_REASON`). A severity a rule of thumb raised is not "only a rule minimum".
   - A row the reviewer added and a row the engine threw out are never flagged.
   - The queue look orders flagged rows first inside each band (above the line, over budget,
     below). The line and the bands keep their meaning.

## Open question for the owner

Should a client export be refused while a flagged row is still undecided? Recommendation: no.
Show the count instead (internal Start here: "N rows flagged as possible false positives, M
decided"). Refusing would turn a review aid into a gate on the engine's doubt, the same reasoning
as the 2026-10-16 ruling on `could_be_intentional`.

## On the agent finder's rows (2026-11-17)

An agent finder row (it carries the finder's captures, or is an open question) is held for review by construction, has no confidence and keeps the model's own rating. So on those rows reason (b) fires only on a recorded low confidence (never held-for-review alone), (g) never fires, and (f) fires only when a recorded raise (the unauthenticated-access floor) lifted the severity above the model's rating. Reasons (a), (c), (d) and (e) work as before wherever their data exists. An agent row is not counted as a deterministic row at its place. The old engine finder's results are unchanged.
