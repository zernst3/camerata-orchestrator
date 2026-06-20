# Rule-routing wiring into the audit pass loop

Date: 2026-06-20
Status: SHIPPED (branch dev4/rule-routing-wiring)

## What

Wires the language rule-routing core (`crates/server/src/scan_routing.rs`) into the
`audit_repo` pass loop in `crates/server/src/ai_audit.rs`. Language-scoped rules (e.g.
`RUST-*`, `REACT-*`) now run only against files of their language; cross-cutting rules
(`ARCH-`, `SEC-`, `SQL-`, `DB-`, `API-`, `PROC-`, and anything unrecognized) still
audit every file.

## Why

The dominant cost of the AI audit is re-sending the codebase once per rule-batch (see
`docs/decisions/2026-06-19_scan_cost_controls.md`, Lever 2). On a polyglot repo like
Rivet (Rust backend + TypeScript frontend), a `RUST-*` rule pays to read ~7.7M chars of
TypeScript that it provably cannot match. The routing core was already pure + tested; this
change wires it into the execution path so the saving is realized, not just estimated.

## The interaction that had to be solved correctly

`run_passes` gates the "flag novel issues beyond the adopted rules" advisory task to
`bi == 0` (the first rule-batch of each chunk) to prevent the same novel issue from being
reported under N invented names across N rule-batches of the same chunk. Routing adds a
second dimension: if we naively ran each `RouteGroup` through `run_passes` with advisory
enabled, a `.rs` file would appear in BOTH the `rust` group AND the `All` group and
receive advisory in each — re-introducing the duplicate-novel-finding problem.

## The safe wiring (what was built)

One invariant controls this:

> **Advisory runs exactly once per file chunk across the whole scan: in the cross-cutting
> `All` group only. Language-specific groups run with `advisory_disabled = true`.**

Implementation:

1. `run_passes` received a new `advisory_disabled: bool` parameter. When `true`, the
   advisory prompt is suppressed for every batch in that call (not just later batches).
   All existing call sites (resolution round) pass `false` — no behavior change.

2. A new `run_routed_passes` helper, called from `audit_repo` on the real-time path,
   iterates over `route_plan.groups` (from `plan_routes`). For each group:
   - `advisory_disabled = !matches!(group.scope, Scope::All)`
   - Files are filtered to that group's scope via `file_in_scope`.
   - `run_passes` is called with the group's files, the group's rules batched by
     `batch_size`, and the computed `advisory_disabled` flag.

3. The `All` group (if present) always runs with advisory enabled. It sees every file,
   so novel-issue discovery is comprehensive.

4. Language groups see only their language's files and check only their adopted rules
   (no advisory), so they can't produce novel-issue duplicates.

## What is NOT changed

- **Batch mode** (`ScanMode::Batch`, `run_passes_batch`): does not yet apply per-rule
  routing. It submits every rule against every file in one Anthropic Message Batch. The
  routing plan is computed and logged for the saving estimate, but not applied. Batch mode
  routing is a follow-up.
- **Resolution round**: runs the full selected ruleset against the small set of
  `needs_files`-requested bodies, with advisory enabled. No routing applied — the
  resolution set is already small and cross-file context benefits from the full ruleset.
- **`scan_routing.rs`**: untouched. The pure core is already tested independently.

## Edge cases

- **No rules (free-form audit)**: `run_routed_passes` detects `selected.is_empty()` and
  falls back to a single advisory-enabled pass over all files. `plan_routes` returns no
  groups for an empty ruleset.
- **All cross-cutting rules**: `plan_routes` produces a single `Scope::All` group.
  `run_routed_passes` iterates once, advisory enabled. Identical to the pre-routing path.
- **Only language-scoped rules, no All group**: `run_routed_passes` runs language groups
  only, all with `advisory_disabled = true`. This means no novel-issue discovery pass.
  Acceptable: users with only language rules are checking specific compliance, not doing
  free-form discovery. In practice, most projects include at least some `ARCH-`/`SEC-`
  rules, which always produce an All group. Documented as a known limitation.

## Savings example (Rivet-class polyglot repo)

| Scope | Files audited | Reduction |
|---|---|---|
| `RUST-*` rules | `.rs` files only | skip ~7.7M chars of TS/JSON |
| `REACT-*` / `TS-*` rules | `.ts`/`.tsx` files only | skip ~8.4M chars of Rust |
| `ARCH-*` / `SEC-*` rules | all files | no reduction (correct) |

Pre-routing total: every rule × every file. Post-routing total: each rule × its language
files only. The `RoutePlan.saved_fraction()` quantifies this; it is logged to stderr at
the start of each routed scan.

## Tests added

Five new unit tests in `crates/server/src/ai_audit.rs` (all in the `tests` module):

- `routing_groups_produce_correct_per_group_file_sets`: verifies that `plan_routes` on a
  polyglot fixture produces rust/web/All groups with the right file-scope filters.
- `advisory_disabled_only_in_language_groups_not_in_all_group`: verifies the
  `advisory_disabled` invariant directly — All = false, language = true.
- `routing_with_no_rules_produces_single_all_group_conceptually`: empty rules → 0 groups,
  free-form path taken.
- `routing_all_cross_cutting_rules_single_group_no_savings`: all cross-cutting → single
  All group, `saved_fraction() == 0.0`.
- `routing_single_language_repo_with_language_rules`: single language group, no All
  group, advisory_disabled invariant still holds.

All 369 tests green.

## Files changed

- `crates/server/src/ai_audit.rs`: `run_passes` gains `advisory_disabled` param;
  `run_routed_passes` added; `audit_repo` updated to use routing on the real-time path;
  5 new tests.
- `docs/decisions/2026-06-20_rule_routing_wiring.md`: this document.
