# The integration gate: a generic reconciliation engine with pluggable per-stack extractors

Date: 2026-07-05
Status: Accepted; BUILT (GAP-6, branch `fix/gap6-integration-gate`).
Deciders: Zach (architect), Claude (architect)

Companion docs: [`2026-06-15_cross_agent_integration_gate.md`](./2026-06-15_cross_agent_integration_gate.md)
(the tier's design + the determinism hard line), [`ENFORCEMENT.md`](../ENFORCEMENT.md)
(the third tier), [`TECHNICAL.md`](../TECHNICAL.md) (the code map).

## Context

The 2026-06-15 ADR designed the cross-agent integration gate (the third enforcement
tier) and drew the DETERMINISM HARD LINE: a cross-agent contract must be a deterministic
comparison of concrete artifacts, never a model judging whether two sides "line up." The
first partial implementation (`crates/gateway/src/integration_gate.rs` +
`check_integration_gate_live`) violated that line: it read a prose contract and asked an
LLM whether the assembled diffs honored it. That is probabilistic convention wearing the
gate's uniform, exactly the failure mode the ADR warned against.

Zach's overriding correction: the gate must be STACK-GENERALIZED. Nothing is baked into a
particular stack EXCEPT pluggable per-stack EXTRACTORS. The reconciliation, comparison,
and verdict logic is 100% generic. A shared compiled type across a Rust boundary is not a
different mechanism: it is merely the case where the extractor finds zero drift.

## Decision

Build a generic, deterministic reconciliation engine over a small NEUTRAL vocabulary, fed
by pluggable per-stack extractors, and drive it off the project's SELECTED `INTEGRATION-*`
rules. Where a stack or seam has no extractor, report REVIEW-TIER (human QA), never a
faked green.

### 1. The normalized vocabulary (`crates/checks/src/integration/vocab.rs`)

Every extractor normalizes its repo's source into two lists over one neutral vocabulary:

- **PRODUCED** artifacts: what a repo EXPOSES.
- **CONSUMED** usages: what a repo DEPENDS ON from another.

The artifact kinds are deliberately small (the seams that generalize cleanly):

- `Endpoint { method, path }` — the workhorse. `path` is normalized (leading slash, no
  trailing slash, every param spelling `:id` / `{id}` / `<id>` / `[id]` / `$id`, template
  interpolations, and concrete ids / UUIDs collapsed to `{}`), so `/users/:id` (express),
  `/users/{id}` (axum), and a client's `/users/42` all reconcile. Literal-segment casing
  is PRESERVED (a `/Members` vs `/members` disagreement is a real drift).
- `Type { name }`, `Event { name }`, `Entity { name }`, `ConfigKey { name }`.

Endpoints optionally carry a `Shape` (request/response field-name sets + status codes) and
a `guarded` flag (does the producer enforce an auth check). Field names compare
casing-insensitively (`member_id` == `memberId`), so only genuine drift fails. An absent
shape is never a finding: absence of evidence is not evidence of drift.

### 2. The generic reconciliation engine (`crates/checks/src/integration/engine.rs`)

The engine assembles all repos' produced/consumed lists into one `AssembledTree` and
reconciles it against a selected `SeamRule`. There is NOT ONE `match language` in this
layer. Each rule is a relational comparison of neutral records:

- `ApiContract` — every consumed endpoint matches a produced route by method + normalized
  path; where both carry a shape, the shapes agree.
- `EventWiring` — every emitted event has a consumer AND every subscription is emitted
  (both directions; no dangling ends).
- `AuthSeam` — every UI-gated affordance maps to a guarded producer endpoint.

The verdict is BINARY and REPRODUCIBLE: same input → same ordered output, no model, no
network. This is the cross-agent sibling of the per-agent `CheckRunner`: where a
`CheckRunner` evaluates one worktree, the engine evaluates the assembled tree.

### 3. Pluggable per-stack extractors (`crates/checks/src/integration/extractor.rs`)

The `Extractor` trait is the ONLY stack-aware code. Extractors are SELECTED off the SAME
`WorktreeLanguage` detection the Layer-2 linters use (`select_extractors(lang)`), so the
extractor sits exactly where the linter does. Built first, to prove the engine:

- `GenericRouteExtractor` (ENDPOINT seam): recognizes the shared route-declaration and
  route-call idioms across web stacks (`app.get("/x")`, `.route("/x", get(h))`,
  `@GetMapping("/x")`, `axios.post("/x")`, `fetch("/x", {method})`, go `NewRequest`).
  Routes normalize cleanly across stacks, so ONE generic extractor covers several stacks
  at once. Explicit `// camerata:integration-guard` and `// camerata:ui-gated` markers
  keep guard/gating status deterministic (a staged AST extractor would infer them).
- `GenericEventExtractor` (EVENT seam): `emit`/`publish`/`dispatch` vs
  `subscribe`/`on`/`listen`.

A shared compiled type is the case where the extractor emits matching records and the
engine finds zero drift, NOT a different code path.

### 4. The review-tier fallback (the hard line, honestly enforced)

`select_extractors` returns the extractors that CAN run. Any seam a selected rule needs
but no extractor covers (an unknown stack, or an undeterminable guard status) is reported
as a `ReviewItem` — routed to human QA and honestly labeled, NEVER scored as a pass and
NEVER as a fail. An LLM opinion about consistency never shows up green in this tier.

### 5. Per-seam relational rules + waivers

The rules are RELATIONAL and PER-SEAM. `AuthSeam` fires ONLY for consumptions the UI
actually gates (`ui_gated == Some(true)`); a public endpoint the UI does not gate is out
of scope — no false positive. Intra-project mix (some endpoints gated, some public) is
handled by per-seam firing plus explicit per-endpoint waivers via the existing suppression
model (`camerata:allow INTEGRATION-AUTH-SEAM-1 -- <reason>` inline, or a baseline entry).
A reason-less waiver does not suppress (mirrors the per-agent invariant).

### 6. Verdict routing

`GateVerdict::bounce_targets()` groups deterministic failures by responsible repo, so a
mismatch bounces to the responsible agent(s) with the specific delta (the same
bounce-and-revise loop as Layer 2). A genuine two-sides-incompatible design fork escalates
to the architect rather than looping one agent.

### 7. Wiring (`crates/server/src/dev_implement_run.rs`)

`run_multi_repo_integration_gate` now runs the DETERMINISTIC engine FIRST, driven by the
project's selected `INTEGRATION-*` rules (broadening the old contract-only trigger: the
gate runs whenever an integration rule is on, whether or not a prose contract exists). The
deterministic verdict is authoritative. The old model-backed contract check survives ONLY
as an optional advisory that runs when a prose contract AND an LLM are present; it never
turns a deterministic pass into a fail on its own.

## What was built vs staged

Built: the neutral vocabulary, the generic engine, the endpoint + event extractors, the
three `INTEGRATION-*` corpus rules, the review-tier fallback, per-seam firing + waivers,
verdict routing, and the server wiring. Staged (listed here, not built): full typed
request/response SCHEMA recovery (beyond field-name sets), migration-vs-entity
reconciliation, config-key declaration-vs-read, shared-type reconciliation, and
stack-native (tree-sitter) AST extractors that replace the line-idiom heuristics with
precise parses. Each is an additional extractor or artifact kind; none changes the engine.

## Consequences

- The gate is now deterministic-first and honest: a pass is a real mechanical pass, a
  review-tier seam is labeled as human-reviewed, and a model opinion never shows up green.
- Adding a stack means adding an extractor, not touching the engine.
- The corpus gains an `integration` domain (opt-in rules), so a single-repo project with
  no seam never sees the gate.
