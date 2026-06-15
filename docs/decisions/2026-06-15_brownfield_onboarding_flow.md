# Brownfield onboarding: installing governance into an existing repo

Date: 2026-06-15
Status: Accepted (design); NOT built. PoC target: GitHub.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`VISION.md`](../VISION.md) (the onboarding axis and the genesis
harness), [`RATIONALE.md`](../RATIONALE.md), [`ENFORCEMENT.md`](../ENFORCEMENT.md).

## Context

Every real team already has code, often several long-lived repos with no concept of
Camerata enforcement. Greenfield (scaffold a fresh repo with the rules baked in from
commit zero) is the easy case. The valuable case is **brownfield**: take an existing,
historical repo and install governance into it. Crucially, the deliverable is not a
document that says "you should do X." It is the **mechanical apparatus that makes X
real**: the prose conventions, the rule config the gate reads, and a CI workflow that
actually fails the build when a rule is violated. Prose without enforcement is the
exact "examples are not enforcement" failure the whole project exists to fix.

## Decision: brownfield onboarding produces a governance PR, not a recommendation

The flow, end to end:

1. **Point Camerata at the repo(s).** One or several (multi-repo is normal; a feature
   can span repos). Each repo is onboarded independently against the shared corpus.
2. **Review (the engine as staff engineer).** Map the architecture, detect the stack
   and any checks already present (linters, CI, type-checking), and extract the
   conventions the code already follows (its de-facto patterns), versus what it lacks.
3. **Propose a RuleSet.** Some rules selected from the Camerata corpus that apply,
   some synthesized from observed patterns, conflicts between corpus and existing
   patterns flagged for the human, and each rule tagged mechanically-enforceable vs
   review-only. The Architect edits and approves; the Architect owns the final set.
4. **Install it (the load-bearing step).** Camerata generates a single reviewable
   **governance PR** that deploys the apparatus that makes the rules real:
   - **Prose:** `CONVENTIONS.md` / `AGENTS.md`, each rule carrying its id.
   - **Enforced CI:** a CI workflow (GitHub Actions now; ADO Pipelines on the ADO
     code-host axis) that runs the mechanical checks for the stack (fmt, lint/clippy,
     tests, AST/structural checks) so a violation fails the build, not just a doc.
   - **Gate config:** the rule-subset file the MCP gateway reads for live agent runs.
   - Optionally pre-commit hooks for fast local feedback.
   The repo goes from ungoverned to governed by merging one PR the human reviews.
5. **Incremental adoption.** Brownfield teams will not accept all rules at once.
   Support adopting a subset and expanding over time; never an all-or-nothing gate.

## Why this is the same idea as the greenfield genesis harness

VISION's genesis harness installs what a NEW repo SHOULD have before any code exists.
Brownfield onboarding is the same commitment pointed at an existing repo: install what
it SHOULD have, not merely document what it has. Same principle (codified rules are
enforced gates, not advisory documents), opposite starting point.

## Provider / axis notes

This rides the **code-host axis** (where the CI and the repo live): GitHub Actions
first (PoC, since GitHub is also the board axis there), ADO Pipelines for the ADO
code host. It is independent of which board a team uses for stories.

## Honest current state

This is a design capture, not built. What exists today: the gate (deny-before-execute
+ the enforced rule registry) and the layer-2 check runner (fmt/clippy/test). What
does NOT exist: the repo-review-and-extract step, the rule-synthesis step, and the
governance-PR generator (the CONVENTIONS.md + CI-workflow + gate-config writer). Those
are the brownfield-onboarding build.

## Open questions

- Convention extraction quality: how confidently can the review distinguish a
  deliberate pattern from incidental drift before proposing it as a rule?
- The synthesized CI workflow must match the repo's real toolchain (test runner, lint
  config); detection has to be robust or the PR's CI will be red on arrival.
- How AST-level rules (e.g. a layering rule) are expressed portably enough to deploy
  into a target repo's CI, since those are the rules most worth enforcing and the
  hardest to make deterministic (see the commanded-violation demo note).
