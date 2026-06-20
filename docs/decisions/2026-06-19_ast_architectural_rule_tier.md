# An AST-checkable architectural enforcement tier

Date: 2026-06-19
Status: Accepted (design); proof checker built, production trait/dependency ROUTED.
Deciders: Zach (architect), Claude (architect)
Issue: #47

Companion docs: [`ENFORCEMENT.md`](../ENFORCEMENT.md) (the tiers/locations that exist today),
[`2026-06-15_process_rules_and_vcs_action_gate.md`](2026-06-15_process_rules_and_vcs_action_gate.md)
(the fourth enforcement *location*).

## Context: the gap between "advisory" and "deterministic"

Today the Layer-1 gate (`crates/gateway/src/lib.rs`, `evaluate_call` over `RULE_REGISTRY`)
enforces only a handful of deterministic rules. Everything else in the corpus is carried as
**agent context** — emitted to `AGENTS.md` / `CONVENTIONS.md` — and is a no-op at the gate. The
existing enforcement tiers (`EnforcementKind` in `crates/rules/src/lib.rs`) are:

| Tier | What it is | Where it lands | Who judges it |
|---|---|---|---|
| `Prose` | human-readable rationale / behavioral guidance | `AGENTS.md` | the agent, by judgment |
| `Structured` | a citable convention, no machine check | `CONVENTIONS.md` | human review at PR |
| `Mechanical` | a runnable lint / regex / query-plan / migration audit | `CONVENTIONS.md` + CI | a tool (pattern match) |

There is a class of rule that fits NONE of these cleanly:

- *"A handler does not touch the database directly."*
- *"A service does not bypass the repository layer."*
- *"No module imports across a forbidden architectural boundary."*

These are **deterministic** (a hard yes/no, no LLM judgment needed) yet **not expressible as a
regex / lint pattern**. The same token — `db.query(...)` — is *correct* inside a repository
function and a *violation* inside a handler. A flat regex (`db\.\w+\(`) cannot tell the two
apart, because the distinction is the token's *position in the parse tree*, not its text. A
`Mechanical` lint can sometimes approximate this with project-specific configuration, but the
general, portable form of the rule needs to reason over **structure**, i.e. an AST.

Calling these `Mechanical` is dishonest about the mechanism, and calling them `Structured`
(human-reviewed) throws away the fact that they are perfectly automatable. They deserve their
own tier.

## Decision: add an `Architectural` enforcement tier

Add `EnforcementKind::Architectural` (serde tag `"architectural"`). It denotes a rule that is
**deterministically checkable by AST / static analysis**: parse the code into its syntax tree
and reason over its structure. No regex can express it; no LLM is needed to judge it.

Strictness/automation ordering: `Prose` < `Structured` < `Mechanical` < `Architectural`.
`Architectural` is the most *precise* tier — it never emits the weak "an index probably exists
somewhere" finding a digest scan produces, because it answers a structural question exactly.

### How it differs from the existing tiers

| Aspect | `Mechanical` | `Architectural` |
|---|---|---|
| Mechanism | lint pattern / regex / query-plan / migration audit | parse to AST, reason over structure |
| Can a regex express it? | yes (that *is* the mechanism) | no — position in the tree is load-bearing |
| Example | "no `var`", "FK columns are indexed" | "no DB call inside a handler body" |
| Needs an LLM? | no | no |
| Emits to | `CONVENTIONS.md` (citable) | `CONVENTIONS.md` (citable) |
| Enforcement stage | CI tier | CI tier |

`Architectural` shares the *partitioning* of `Mechanical` (citable in `CONVENTIONS.md`, carries a
`_Conformance:_` line, wired into the CI governance workflow), which is why the codebase treats
"is this CI-tier?" as `EnforcementKind::is_ci_enforced()` returning true for **both**
`Mechanical` and `Architectural`. The difference between the two is the *implementation* of the
check, not where it lands or when it runs.

## Where it runs: CI tier, NOT the write-time gate (argued)

The Layer-1 gate (`gated_write` / `evaluate_call`) fires on the **content of a single write**:
one file, one tool call, mid-edit. An architectural check fundamentally cannot run there, for
three reasons:

1. **It needs the whole module, not one edit.** Whether a handler touches the DB is a property of
   the *finished* function body and often of *other* modules (does this `use` cross a boundary?
   the boundary map lives elsewhere). A single in-flight write does not carry that.
2. **The tree is incomplete mid-task.** An agent legitimately writes a handler in several edits;
   flagging "no service call yet" on the first write would be a false positive that fights the
   agent instead of governing the result.
3. **Parsing must succeed.** An AST check needs syntactically valid input. Mid-edit code is
   frequently un-parseable; the right time to demand a clean parse is when the work is assembled.

This is the same argument that already places `Mechanical` rules in CI rather than the gate
(`split_scannable_rules` excludes them from the write-time AI scan precisely because they are
"enforced in CI from build/runtime/DB context"). `Architectural` joins them: it is excluded from
the write-time scan and emitted into `.camerata/ci-checks.json` + the governance workflow, where
it runs against the assembled tree. The write-time gate stays deterministic and cheap; the LLM
stays advisory.

## Proposed checker shape: an `ArchitecturalCheck` trait over a parsed module

The production design is a trait evaluated over a parsed module:

```rust
/// A deterministic architectural rule evaluated over one parsed module.
pub trait ArchitecturalCheck {
    /// The rule id this check reports under (e.g. "ARCH-HANDLER-NO-DB-1").
    fn rule_id(&self) -> &str;
    /// Evaluate the parsed module; return zero or more violations.
    fn check(&self, module: &ParsedModule) -> Vec<ArchViolation>;
}
```

- **Rust:** `ParsedModule` wraps a `syn::File`; checks walk the AST (`syn::visit::Visit`) — e.g.
  find every `ItemFn` whose name marks it a handler, then look for a `MethodCall` whose receiver
  resolves to a DB-handle identifier inside that fn's body.
- **Language-agnostic later:** the same trait sits behind one parser per language (tree-sitter for
  the long tail), so a rule like *handler-no-direct-db* has a Rust impl and a TS impl reporting
  under the same rule id. The corpus rule (TOML) is language-independent; the *checker* is
  per-language.

### ROUTE-1: the trait + the `syn` dependency are routed, not auto-applied

Introducing the `ArchitecturalCheck` trait as cross-crate public surface, and adding `syn` as a
direct dependency of a crate, are both **structural** changes (new public API surface / new build
dependency). Per the project's ROUTE-1 stance, structural changes route to the architect rather
than being auto-applied. So this issue ships:

- the **tier** (`EnforcementKind::Architectural`) — additive, within `crates/rules`;
- a **proof checker** as a pure function (no `syn`, no new dep, no new public trait) — within
  `crates/checks`;
- this **design doc** carrying the trait + `syn` proposal for the architect to accept.

`syn` 2.0 already exists in `Cargo.lock` transitively (via proc-macro derives), so adopting it as
a direct dependency is low-cost when accepted; it is routed here only because adding a *direct*
dependency and a cross-crate trait are structural decisions, not because the crate is heavy.

## Worked example (the first rule): `ARCH-HANDLER-NO-DB-1`

**Rule:** a handler/controller function must not call a database handle directly; it delegates to
a service, which delegates to a repository. Only repositories hold a DB handle. (This is the
AST-checkable companion to the existing `Mechanical` rule `ARCH-STRICT-LAYERING-1`, which states
the same boundary but enforces it with a lint-style import restriction.)

**Why AST, not regex:** `db.query(...)` is correct in `fetch_all_orgs` (a repository fn) and a
violation in `list_orgs_handler` (a handler fn). Only the parse tree — function boundary + brace
scope — distinguishes them.

**Proof checker (shipped):** `crates/checks/src/architectural.rs::handler_no_direct_db(source:
&str) -> Vec<ArchViolation>`. It is a pure function with no new dependency. To stay self-contained
ahead of the `syn` decision it does genuine *structural* reasoning lexically: it tracks function
declarations and brace depth so a DB-handle method call is flagged only when it lexically sits
inside a handler function body. Its unit tests prove the structural discrimination a regex cannot
do:

- a handler calling `db.query(...)` → flagged;
- a handler delegating to `svc.list_orgs()` → clean;
- a *repository* fn calling `db.query(...)` → clean (not a handler);
- a `db.query(...)` outside any handler scope → clean;
- both a repo fn and a handler present → only the handler flagged, with the correct line;
- a `db.query` in a comment, and `database_url` / `dbg!` substrings → clean (comment-stripped,
  word-boundary-aware).

The lexical approach's known limitations (no string-literal stripping beyond `//` comments,
handler-by-name rather than by route-macro/attribute, no type resolution) are documented on the
function and are *precisely the argument* for the `syn`-backed production version: the AST removes
all three by reasoning over the real tree instead of token positions.

## Second example rule (corpus only)

`ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` — modules import only across declared, allowed dependency
boundaries (controller↛repository, domain↛infrastructure, feature↛sibling-internals). This is a
property of the **import graph**: an import string alone does not say which boundary it crosses;
the checker needs the boundary map plus the parsed `use`/import declarations. Shipped as a corpus
TOML rule under the new tier; its checker is future work behind the same `ArchitecturalCheck`
trait.

## What this issue changed

- `crates/rules/src/lib.rs`: added `EnforcementKind::Architectural` with serde rename, `as_str` /
  `from_str` round-trip, `is_ci_enforced()` / `emits_to_conventions()` partitioning helpers,
  `Display`, and tests (TOML deserialize, round-trip, partitioning, real-corpus load).
- `crates/server/src/{onboard,lib,arm,ai_audit}.rs`: updated the exhaustive matches and the
  CI-tier / CONVENTIONS-vs-AGENTS partitioning so `Architectural` behaves like `Mechanical` for
  placement (CONVENTIONS.md + CI workflow), honest placement strings, and is excluded from the
  write-time AI scan.
- `crates/checks/src/architectural.rs`: the proof checker + 9 unit tests.
- `crates/rules/principles/api-layer/arch-handler-no-db-1.toml`,
  `arch-no-cross-boundary-imports-1.toml`: the two example corpus rules.

## What is deliberately NOT built (follow-ups)

- The `ArchitecturalCheck` trait and the `syn`-backed Rust parser (ROUTED above).
- Wiring architectural checks into the CI governance workflow as a real runner (today the workflow
  scaffold *lists* CI-tier rules; it does not yet execute an AST runner).
- The `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` checker implementation.
- The language-agnostic (tree-sitter) layer.
