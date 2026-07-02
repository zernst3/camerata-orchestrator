# ADR: Completeness ("did you catch them all?") as an L3 reviewer dimension

**Date:** 2026-07-02
**Status:** Accepted (first cut shipped)

## Context

Some defect stories are *cross-cutting*: the fix isn't one edit, it's an invariant that must hold
*everywhere* it applies ("every AI action must animate the Bombe", "no client/server contract may
drift", "every FK needs an index"). The failure mode is a **spot-fix**: the developer fixes the one
instance the story surfaced and leaves the siblings. Manual sweeps catch these (they did, twice, in
one session — 10 dead-button contract bugs, 13→14 missing Bombe guards), but a sweep you have to
remember to run is not a gate.

The tempting wrong answer is a **per-defect linter** ("spawns calling an LLM endpoint must hold a
LoadingGuard"). That's a one-off for a one-off; immortalizing a checker per weird bug produces a
graveyard of hyper-specific rules. Rejected.

The right question is the *general* one: is there a "did you catch them all?" gate parameterized by
defect **class**, not hardcoded to one instance?

## Why a linter can't do it (why it must be L3)

- **L1** = deny-before-execute (real-time MCP gate, per rule-subset).
- **L2** = structural post-task check (lint / AST / rule audit) — enforces **pre-codified** patterns.
  Deterministic and cheap *because* the class is known in advance.
- A novel "are there siblings of *this* mistake?" cannot be an L2 lint: the class is defined **ad hoc
  by the story**, not by a rule written yesterday, so there's nothing for a matcher to match. It
  requires **abstracting the class from the instance** and reasoning about scope — an agent task.

That is **L3**: reasoning, not matching. Camerata already has an L3 reviewer (`run_l3_review`, R7)
that verifies a diff against story intent + rules. Completeness is a natural **third dimension of the
same review**, not a new layer.

## Decision

Add a **completeness dimension** to the L3 reviewer (`crates/server/src/review_agent.rs`,
`L3_SYSTEM_PROMPT`). Its job becomes verifying the diff (1) conforms to rules, (2) fulfils story
intent, and **(3) addresses the story's FULL SCOPE**: for a cross-cutting story (signals like
"every / all / across the app / anywhere that", or a fix for a mistake likely repeated elsewhere), a
narrow spot-fix is **bounced** with a request to sweep the codebase for the remaining instances. A
genuinely local story must NOT manufacture a completeness concern.

### The discovery → enforcement pipeline

- **L3 discovers** (reasoning, per-story, token-costly): flags likely-incomplete cross-cutting fixes.
- **L2 enforces** (matching, continuous, cheap): if a class keeps recurring, *promote it to a rule*.

Novel/semantic stays in L3; anything that earns permanence drops to L2. That is Camerata's own thesis
(turn a defect into a gate) applied one level up — at the **class**, not the instance.

## Honest limits

- **Probabilistic, not a proof.** The L3 reviewer is a single blind LLM call over story + rules +
  diff; it cannot see the rest of the codebase, so it judges completeness by whether the diff's
  breadth plausibly matches the story's implied scope. It catches the common case (a spot-fix of a
  pervasive story) and will occasionally miss (a diff that *looks* broad but skips a spot it can't
  see). It is a strong net, complementing — not replacing — L1/L2.
- A future, heavier step would make it **agentic** (give the reviewer codebase search to actually
  enumerate siblings), at the cost of the current blind-single-call isolation. Out of scope here.

## Consequences

- No per-defect linter is added (explicitly rejected). The general capability lives in one place.
- Cross-cutting stories now get an automatic "did you sweep for the rest?" check whenever L3 review is
  enabled for the project (`Project::l3_review`).
