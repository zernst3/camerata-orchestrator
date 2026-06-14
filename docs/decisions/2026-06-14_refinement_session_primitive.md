# The refinement session: one primitive, three contexts; stories as source of truth

Date: 2026-06-14
Status: Accepted (implemented)
Deciders: Zach (PO), Claude (architect)

## Context

The consumer flow needed a model for the back-and-forth between a non-technical
user and the AI lead engineer. The first cut was a one-shot "clarify loop" over a
form. Zach reframed it: there should be ONE repeating back-and-forth primitive,
reused everywhere it is needed, with user stories (and later bug stories) as the
durable source of truth, exactly like real software development.

## Decision

1. **One primitive: the refinement session.** A `RefinementSession` is the single
   unit of back-and-forth: the AI reviews the current artifacts, proposes story
   edits / questions / product suggestions / a confidence update; the user edits,
   answers, adds, and deletes; repeat until the user is happy with the confidence.
   It is a first-class, persistable, replayable type (`crates/intake/src/refinement.rs`),
   not a one-shot function.

2. **Three contexts, same loop.** The session runs in three places via one
   `RefinementContext` enum: `PreBuild` (onboarding to ready spec), `MidBuild`
   (a builder escalation pauses execution and opens a session scoped to the
   question, then resumes), and `PostBuild` (QA + structured bug reports become bug
   stories that feed a session, then re-execute). The transitions are identical
   across all three; only what seeds the session differs.

3. **Stories are the source of truth.** After the onboarding document seeds the
   first investigation, it is FROZEN as read-only origin, and the user stories
   (plus bug stories) become the living source of truth, mirrored by the
   `Project` aggregate (`crates/intake/src/project.rs`). This is the same
   discipline as real dev: stories and bug tickets are the truth, not a transient
   prompt.

4. **The lifecycle is refinement alternating with execution.** Onboarding ->
   refinement (N) -> execution -> [mid-build escalation -> refinement -> resume] ->
   post-build refinement (N) -> publish. The governance gate applies to every
   execution, and the same refinement primitive bookends each one.

## Why one primitive rather than three flows

The three moments (pre-build, mid-build, post-build) look different on the surface
but are the same activity: align on what to build, with the AI, before committing
work. Modeling them as one type means the UI, the persistence, the reviewer seam,
and the confidence model are written once and reused, and a capability added to one
(e.g. product suggestions, honesty verdicts) is automatically available in all
three. Three bespoke flows would have drifted.

## The intelligence seam

The AI side of a session is the `RefinementReviewer` trait
(`crates/intake/src/review.rs`): `StubRefinementReviewer` (deterministic, offline,
guarantees convergence) and `ClaudeRefinementReviewer` (live). `RefinementDriver`
runs review -> apply -> answer -> repeat. This keeps the loop testable without a
network and provider-neutral, the same stance as the rest of the engine.

## Where the rest of this feature's design lives

- The user-facing flow, screen by screen: `docs/CONSUMER_UX.md`.
- Persistence + version history of the artifacts: the persistence decision record.
- The shared-design corpus built on top of stories + bug stories: the corpus
  decision record.
