# Process rules + the VCS-action enforcement point (a fourth gate location)

Date: 2026-06-15
Status: Accepted (design); NOT built.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`ENFORCEMENT.md`](../ENFORCEMENT.md) (the tiers that exist today),
[`cross_agent_integration_gate`](2026-06-15_cross_agent_integration_gate.md) (Tier 3),
[`credential_delegated_scope_and_build_targets`](2026-06-15_credential_delegated_scope_and_build_targets.md).

## Context: a real rule that no existing gate location can enforce

Motivating example, from Zach's workplace. ADO is the board; a backend automation links
ADO tickets to GitHub commits/PRs by a strict text convention: the **PR title** and the
**first line of every commit message** must contain `AB#{ticketId}`. If that token is
missing, the link silently never forms. A real team needs Camerata to enforce this:
create such a rule, gate it firmly, and **error out** when the format is not adhered to.
This is completely custom per user/account.

Now place that rule against the three enforcement locations that exist or are designed:

| Tier | Enforces on | Scope | Sees commit msg / PR title? |
|---|---|---|---|
| Layer 1 — MCP tool-gateway (`gated_write`) | file **content** of one write | one agent | **No** |
| Layer 2 — CheckRunner (fmt/clippy/test) | one agent's **diff** | one agent | **No** |
| Tier 3 — integration gate | the assembled **tree** / contracts | cross-agent | **No** |

Every existing location enforces on **code artifacts** (file content, diffs, the tree).
The `AB#{id}` rule is not about code at all — it is about **VCS metadata**: the commit
message and the PR title. Nothing in Layers 1–3 ever sees those, so none of them can
enforce it. This is a genuinely new enforcement surface, not a variant of an existing one.

## Decision: a fourth enforcement point — the VCS-action gate

Add a **process-rule** category enforced at a new location: the **VCS-action gate**,
which fires at the moment Camerata performs a version-control action (a commit, a branch
push, a PR open/update) and validates that action's **metadata** against the active
process rules. A violation aborts the action and reports the specific rule, the same
binary/deterministic posture as the other gates.

### Why this location is clean (the cage makes it free)

The agent cannot perform VCS actions at all: `Bash` is on the denylist, so the agent has
no `git`. **Camerata's own orchestration code is the sole committer and PR-opener.** That
means there is exactly one chokepoint for every commit and PR in the system, and it is
code Camerata controls. The VCS-action gate is not a new thing to intercept — it is a
validation step at a chokepoint that already exists and is already singular. The cage's
"agent has no git" property, built for security, is what makes this gate trivially
complete: there is no second path that bypasses it.

### What a process rule is

A process rule is a deterministic predicate over the **metadata of a VCS action**, not
over code:

- **Commit:** the message matches a required shape (`AB#{id}` prefix, conventional-commit
  type, a trailer, a ticket reference, a sign-off line).
- **PR:** the title / body matches a required shape (the `AB#{id}` token, a linked-issue
  reference, a required checklist, a target-branch constraint).
- **Branch:** a naming convention (`feature/*`, `release/x.y.z`).

These are **per-user/account custom** — the `AB#{id}` format is one team's convention, not
a universal. They are authored by the user (the brownfield onboarding flow is the natural
place to capture them, alongside the repo-local content rules), stored per account, and
applied to that account's VCS actions.

### Definition of "enforced" (same hard line as Tier 3)

- The rule is a concrete, deterministic predicate (a regex / structured matcher over the
  metadata string), never an LLM judging whether the message "looks right."
- The verdict is binary and reproducible: the commit/PR either carries `AB#{id}` or it is
  refused.
- A refused action does not proceed. There is no "warn and continue" — Zach's framing was
  "gated firmly, error out," and a process gate that warns is a linter, not a gate.

## Where this fits the rule taxonomy

With this ADR the rule model has two orthogonal axes, and the four rule categories raised
in conversation now each have a home:

**Scope axis** (what the rule ranges over):

- `corpus-global` — from the camerata-ai corpus, applies everywhere (today's `domain`
  mechanism).
- `repo-local` — authored/derived for one repo, lives in the repo; the brownfield flow
  proposes a starter set ([brownfield ADR](2026-06-15_brownfield_onboarding_flow.md)).
- `cross-repo` — spans repos/agents; API contracts and the like
  ([integration-gate ADR](2026-06-15_cross_agent_integration_gate.md), `INTEGRATION-*`).
- `process` — VCS-workflow conventions, per account (**this ADR**, `PROCESS-*`).

**Enforcement-point axis** (where the gate fires):

- `content` — on file content, Layer 1 (`gated_write`) + Layer 2 (CheckRunner).
- `integration` — on the assembled tree, Tier 3.
- `vcs-action` — on commit/PR/branch metadata, at Camerata's VCS chokepoint (**this ADR**).

The two axes are independent: a process rule is `scope = process`, `enforcement-point =
vcs-action`. Encoding both on the `Rule` type (rather than overloading the existing
`domain` field, which is a tech-area tag) is the model change this ADR implies.

## Mechanism

A new `PROCESS-*` rule family and a `vcs-action` enforcement point. Camerata's
commit/PR-opening path (the orchestration code that lands a governed branch) gains a
validation step: before it commits or opens a PR, it runs the account's process rules
against the proposed message/title and refuses on a miss, surfacing the failed rule id and
the expected format. Because that path is the only committer (agents have no git), the
gate is complete by construction.

## Honest current state

Not built. There is no `PROCESS-*` family, no `vcs-action` enforcement point, and the
`Rule` type has only a `domain` tag (no scope / enforcement-point axes). Camerata's
live-run commit/PR path is itself still thin (the live fleet is opt-in behind
`CAMERATA_LIVE_BUILD=1`). This ADR defines the surface; building it pairs naturally with
hardening the live commit/PR path and with the brownfield rule-capture step (where a user
would author their `AB#{id}` rule).

## Open questions

- Authoring UX: process rules are custom and finicky (regex over commit messages). The
  capture step should offer templates (`AB#{id}`, conventional-commits, linked-issue) so
  the user picks and parameterizes rather than writing raw regex.
- Scope of application: per-connection (all repos under a token) vs per-repo vs per-board?
  The `AB#{id}` convention is usually org-wide, suggesting per-connection default with
  per-repo override.
- Bounce vs abort: on a violation, does Camerata rewrite the message to comply (it knows
  the ticket id from the story's `external_ref`) or hard-refuse and report? Leaning
  auto-comply where the needed datum is known, hard-refuse where it is not.
