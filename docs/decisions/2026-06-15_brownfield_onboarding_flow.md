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

## Brownfield is the instant-value weapon, not "setup"

The most important reframe: brownfield onboarding is not plumbing you suffer through
before the product is useful. It is potentially the single best instant-value demo,
because **an existing codebase is pre-loaded with the violations the gate catches.**
The moment you point the armed gate at a real repo, it scans the existing code against
the rules and reports what is already wrong:

> "I pointed it at your repo and found 12 architecture violations and 3 hardcoded
> secrets that are in there right now."

That is value in five minutes, not a week, and it is undeniable, because it is their
code. Then the pitch closes itself: **"and now the gate stops any new ones."**

The one design rule that makes this land fast: **the user must not be made to
hand-author forty rules before they see anything.** Value must come before they have
typed a single rule. So the sequence is audit-first:

## Decision: scan -> propose -> approve -> audit + arm

1. **Point Camerata at the repo(s).** One or several (multi-repo is normal; a feature
   can span repos). Each repo is onboarded independently against the shared corpus.
2. **Scan + propose a STARTER ruleset.** The engine maps the architecture, detects the
   stack and any checks already present, extracts de-facto conventions, and proposes a
   starter RuleSet (corpus rules that apply + synthesized from observed patterns +
   conflicts flagged, each tagged mechanically-enforceable vs review-only). The user
   does NOT author rules from scratch; they start from the proposal.
3. **Approve / edit.** The Architect adjusts and approves; the Architect owns the
   final set. This is the only gate before value, and it is a review, not authoring.
4. **Audit (the instant value).** Scan the existing code against the approved rules and
   report the violations already in it. This is the five-minute payoff, the undeniable
   "here is what is wrong in your code right now."
5. **Arm (the governance PR).** Generate one reviewable PR that installs the apparatus
   that makes the rules real going forward:
   - **Prose:** `CONVENTIONS.md` / `AGENTS.md`, each rule carrying its id.
   - **Enforced CI:** a CI workflow (GitHub Actions now; ADO Pipelines on the ADO
     code-host axis) that runs the mechanical checks for the stack (fmt, lint/clippy,
     tests, AST/structural checks) so a violation fails the build, not just a doc.
   - **Gate config:** the rule-subset file the MCP gateway reads for live agent runs.
   - Optionally pre-commit hooks for fast local feedback.
   The repo goes from ungoverned to governed by merging one PR the human reviews, and
   new violations are now stopped at the gate.
6. **Incremental adoption.** Brownfield teams will not accept all rules at once.
   Support adopting a subset and expanding over time; never an all-or-nothing gate.

Audit first (here is what is already wrong), arm second (and now it is enforced). The
audit is the hook; the PR is the close.

A caution to keep honest: the **deployed CI gate** this flow installs is the
**pipeline-stage safety net** (post-hoc, repo-level, the commodity-adjacent territory
of Semgrep / pre-commit / CodeQL). It is valuable as the backstop for changes made
outside Camerata, but it is NOT the differentiated moment. The differentiator is the
**in-loop, pre-execution deny** during a governed run (Layer 1 at the tool boundary).
Do not let "the brownfield CI gate is wired" get counted as "the in-loop deny works",
they are different mechanisms. See [`ENFORCEMENT.md`](../ENFORCEMENT.md), "In-loop
enforcement vs the deployed CI gate."

Honest grounding on the audit: the content rules (hardcoded-secret, raw-SQL-concat,
secrets-in-URL, path-escape) are pure functions over file content, so they can audit an
existing repo TODAY by scanning its files, the "3 hardcoded secrets" half is real now.
The "12 architecture violations" half needs the AST-level rules (e.g. layering), which
are not built yet (see the commanded-violation demo note). So the secret/SQL audit is
real-now; the full architecture audit is pending those checks.

## Why this is the same idea as the greenfield genesis harness

VISION's genesis harness installs what a NEW repo SHOULD have before any code exists.
Brownfield onboarding is the same commitment pointed at an existing repo: install what
it SHOULD have, not merely document what it has. Same principle (codified rules are
enforced gates, not advisory documents), opposite starting point.

## Provider / axis notes

This rides the **code-host axis** (where the CI and the repo live): GitHub Actions
first (PoC, since GitHub is also the board axis there), ADO Pipelines for the ADO
code host. It is independent of which board a team uses for stories.

## Multi-repo and rule placement (added 2026-06-15)

Brownfield onboarding operates on a **SET of inter-related repos**, not one repo.
A real onboarding (a .NET API + a Python worker + a React app) is one scan: the
whole of each repo is downloaded (one tarball per repo, no per-file API calls) and
audited, and the findings + proposed ruleset **aggregate across all repos**, each
finding tagged with its repo.

The phase must be **thorough at every level**: each proposed rule is classified by
**scope** and assigned a **placement** up front, so the human approves not just
"which rules" but "where each rule and its gate live":

- **repo-local** (content rules) — the gate + config installed in EACH repo; bound
  to the repos that actually carry the violation.
- **cross-repo** (API contracts and the like) — spans the repo set, enforced at the
  integration tier (review-tier until the integration gate is built deterministically;
  see [`cross_agent_integration_gate`](2026-06-15_cross_agent_integration_gate.md)).
- **process** (VCS-workflow, e.g. conventional commits / `AB#{id}`) — account-level,
  the VCS-action gate across all repos' commits/PRs (see
  [`process_rules_and_vcs_action_gate`](2026-06-15_process_rules_and_vcs_action_gate.md)).

So the proposed-rules table carries `scope`, `applies-to repos`, and `gate
placement` columns, and `arm` installs each rule's gate at its decided location.
The findings table carries a `repo` column so violations group/filter by repo.

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
