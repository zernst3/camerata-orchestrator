# The code-AST extractor layer (Passes 3–4)

Date: 2026-07-27
Status: Design — for Zach's routing decisions (§0), then build
Companion: [`2026-06-19_ast_architectural_rule_tier.md`](../decisions/2026-06-19_ast_architectural_rule_tier.md) (the tier + the routed `syn` proposal),
[`2026-07-26_architectural-executor-feasibility.md`](2026-07-26_architectural-executor-feasibility.md) (§2.1 seam, §3 candidate checkers, §4 effort; Passes 1–2 landed)

**Scope:** the extractors + checkers for the architectural-tier corpus rules that need real
AST / import-graph analysis, behind the EXISTING `ArchChecker` seam
(`crates/checks/src/arch_checker.rs`). Nothing in Passes 1–2 changes shape: checkers stay
`ArchChecker` impls over `RepoView`, findings ride the same adapter into the scan
(`onboard::architectural`) and the Layer-2 loop (`NativeArchCheckRunner`).

---

## 0. Decisions routed to Zach (ROUTE-1) — decide these first

| # | Decision | Recommendation | Why it's structural |
|---|---|---|---|
| **D1** | **The boundary-map config file: name + schema.** A new operator-authored file `.camerata/architecture.toml` (§3) declaring layers, allowed import directions, DB-handle ownership, and helper/component exemptions. | Ship as proposed in §3. It lands inside `.camerata/`, so the existing `SEC-NO-CAMERATA-CONFIG-1` hard-guard (`gateway/src/lib.rs:1489` — denies ALL writes under `.camerata/`) already makes it agent-tamper-proof with zero new guard code. | New public operator surface; the single most load-bearing new contract in this layer. Once clients author these files, the schema is hard to change. |
| **D2** | **Parser dependencies + v1 language set.** `syn` 2 as a direct dep of `camerata-checks` (already in `Cargo.lock` transitively at 2.0.117, per the 06-19 ADR); `tree-sitter` + native grammar crates `tree-sitter-typescript` (covers TS **and** TSX), `tree-sitter-javascript`, `tree-sitter-python`. v1 languages: **Rust, TypeScript/TSX, JavaScript, Python**. Defer Go/Java/C#/Ruby (stack detection knows them; no architectural corpus rule demands them yet). | Accept. Native (cc-compiled, statically linked) grammars, NOT wasm — wasm grammars require bundling a wasm engine (wasmtime), a far heavier dependency than three C parsers. | New build dependencies (C toolchain via `cc` at build time) on a core crate; the language set defines what "Camerata enforces architecture" means in sales conversations. |
| **D3** | **Unconfigured-repo degradation policy.** When a repo has no `.camerata/architecture.toml` (or lacks the section a checker needs): the config-gated checkers **skip entirely and their rules STAY in the LLM-advisory prompt** (the LLM-exclusion subtraction becomes per-repo + config-aware, not the static `all_checker_rule_ids()` set). Alternative considered: fall back to name-heuristics at a `needs-review` severity (the current proof checker's behavior). | Skip-to-advisory for all import-graph/layering rules; heuristic fallback ONLY for `ARCH-HANDLER-NO-DB-1` (where the promoted proof checker's name-markers are battle-tested and the finding is inherently local). Rationale: a guessed layer map on someone else's repo is exactly the FP that burns trust in a paid audit; an unconfigured repo must degrade to "the AI reviews it," never to a wrong deterministic verdict. | Determines the tier's honesty story — "deterministic" must never mean "guessed." |

Everything below assumes D1–D3 as recommended.

---

## 1. The extractor abstraction: shared utilities, checker-owned models

**Recommendation: no shared enriched model handed to checkers.** Keep the memo §2.1 stance
("each checker owns its model") and add a new module of **shared per-language extractor
utilities** — pure functions checkers call to build their private models. The `ArchChecker`
trait, `RepoView`, the registry, and both wiring points are untouched; this whole layer is
internal to `camerata-checks`.

```rust
// crates/checks/src/extract/mod.rs  (new module; per-language impls in
// extract/{rust_syn,ts,js,python}.rs behind one dispatch surface)

/// Source language, resolved from the file extension (mirrors the scan's
/// `lang_for_ext`; kept local so camerata-checks stays server-independent).
pub enum SourceLang { Rust, TypeScript, Tsx, JavaScript, Python }
pub fn lang_for_path(path: &str) -> Option<SourceLang>;

/// One import/use declaration. `specifier` is the raw module string
/// ("@/lib/db", "crate::repositories::user", "app.repos.user");
/// `names` the imported bindings when the syntax carries them.
pub struct Import { pub specifier: String, pub names: Vec<String>, pub line: usize }
pub fn imports(lang: SourceLang, source: &str) -> Vec<Import>;

/// One function/method definition with its body span. `markers` carries
/// name + attributes/decorators (#[get("/…")], @app.route, export default)
/// so checkers can classify handler-ness structurally, not just by name.
pub struct FunctionSpan {
    pub name: String, pub start_line: usize, pub end_line: usize,
    pub attrs: Vec<String>,
}
pub fn functions(lang: SourceLang, source: &str) -> Vec<FunctionSpan>;

/// One `receiver.method(...)` call site (the shape both DB-boundary rules need).
pub struct MethodCall { pub receiver: String, pub method: String, pub line: usize }
pub fn method_calls(lang: SourceLang, source: &str) -> Vec<MethodCall>;
```

- **Rust impl:** `syn::parse_file` + `syn::visit::Visit`. Line numbers require the
  `proc-macro2/span-locations` feature — enable it, or every violation reports line 0.
- **TS/TSX/JS/Python impls:** one tree-sitter parse per file; each extractor fn is a small
  tree-sitter **query** (`(import_statement)`, `(function_definition)`, `(call_expression
  function: (member_expression))`) over the shared tree. An internal per-file parse cache
  (parse once, run all three queries) keeps a multi-checker scan from re-parsing.
- **Never panic:** a file that fails to parse yields empty extractor results (fewer findings,
  never a crash and never an FP) — same contract the SQL splitter already honors.
- **Why not a pre-built enriched model:** the RLS checker proved models are per-family
  (a migration timeline is not an import graph); a mandatory shared `ParsedModule` forces
  every checker through the most expensive representation. Utilities compose; a god-model
  ossifies. The import graph IS shared across four checkers — but as a model built by ONE
  checker struct answering four rule ids (§4), not as trait-level currency.

**One small seam amendment (routine):** `interest_globs` today has no `**` (the minimal
matcher in `arch_checker.rs` is segment-anchored). Code-AST checkers need `src/**/*.ts`-class
patterns. Extend `glob_match` with `**` support (+ tests); no trait change.

## 2. Parser strategy + language coverage (detail behind D2)

| Language | Parser | Crate(s) | v1? | Driven by |
|---|---|---|---|---|
| Rust | `syn` 2 (full AST, visitor) | `syn` (features: `full`, `visit`) + `proc-macro2` (`span-locations`) | **Yes** | Dogfood repos; handler-no-db proof is Rust; agora-rs port target |
| TypeScript / TSX | tree-sitter | `tree-sitter`, `tree-sitter-typescript` | **Yes** | The Supabase/TS/React/Node paid-audit stack — the commercial target; UI-* rules; server-authz |
| JavaScript / JSX | tree-sitter | `tree-sitter-javascript` | **Yes** | Same stack, un-typed repos |
| Python | tree-sitter | `tree-sitter-python` | **Yes** | Stack detection ships it; python-testing domain exists |
| Go / Java / C# / Ruby | tree-sitter | — | **Defer** | Detected by `detect_stack` but zero architectural-tier corpus rules demand them; add grammar + extractor impl per language when an engagement does (~½ day each once the dispatch surface exists) |
| SQL | — | — | n/a | Already served by the Pass-1 splitter; do NOT migrate it to tree-sitter |

**Grammar management:** native crates, pinned versions, compiled by `cc` at build time and
statically linked. Cost: a C toolchain requirement (already present — `fs2`/ring-class deps
assume one) and ~tens of seconds of one-time build. No runtime assets, no wasm engine, no
network. Version-pin all four together in `Cargo.toml`; the `tree-sitter` runtime crate and
grammar crates must agree on ABI version (this is the one known upgrade foot-gun — pin, and
bump as a set).

## 3. The boundary-map config: `.camerata/architecture.toml` (detail behind D1)

New file beside `checks.toml` / `features.toml`, same trust model: **operator-authored,
agent-write-denied** by the existing `.camerata/` hard-guard. Loaded two ways with zero new
plumbing: the scan sees it as a `RepoView` file (add it to the boundary checkers'
`interest_globs`); the Layer-2 runner reads it from the worktree the same way
`ManifestCheckRunner` reads `checks.toml`.

```toml
# .camerata/architecture.toml — declares the repo's architectural boundaries.
# Operator-authored; agents cannot write this file (SEC-NO-CAMERATA-CONFIG-1).
version = 1

[layers]                      # layer name → path globs owning it (repo-relative)
handlers     = ["src/routes/**", "src/controllers/**"]
services     = ["src/services/**"]
repositories = ["src/repositories/**"]
domain       = ["src/domain/**"]

[imports]                     # allowed dependency direction between DECLARED layers.
# key may import values; any other declared-layer→declared-layer edge is forbidden.
handlers     = ["services", "domain"]
services     = ["repositories", "domain"]
repositories = ["domain"]
domain       = []

[db]                          # arms ARCH-STRICT-LAYERING-1 + hardens ARCH-HANDLER-NO-DB-1
handles    = ["db", "pool", "conn", "prisma", "supabase"]
allowed_in = ["repositories"]
tx_flow_control_in = ["services"]   # the corpus rule's explicit exemption

[dtos]                        # optional — arms ARCH-API-DTOS-1 (a default=false rule)
domain_types = ["src/domain/**"]
controllers  = ["src/controllers/**"]

[authz]                       # optional — arms ARCH-SERVER-AUTHZ-1
ui_paths             = ["apps/ui/**"]
forbidden_ui_imports = ["@api/lib/permissions", "hasOrgPermission"]

[helpers]                     # optional — exemption files for the UI single-funnel rules
date_helper     = ["src/lib/dates.ts"]          # UI-UTC-DATES-1
image_component = ["src/components/AppImage.tsx"] # UI-IMAGE-COMPONENT-1
```

Semantics that keep FPs structurally impossible:

- **Absent file / absent section ⇒ the checkers needing it skip** (per D3: rule stays
  LLM-advisory for that repo). Skip is silent-by-design at the finding level, but surfaced
  as a one-line scan note ("boundary rules present but unconfigured — see architecture.toml
  template") so the degradation is visible, not mysterious.
- **Only declared-layer→declared-layer edges are judged.** An import that resolves to a file
  in NO declared layer, or that doesn't resolve to a repo file at all (external package,
  std lib), is ignored. Resolver failure mode = false negative, never false positive.
- **Overlapping layer globs are a config ERROR** (one file, two layers), reported as a
  config diagnostic, not as rule findings — a broken map must not fire architecture findings.
- **Findings cite the map**: every violation message names both layers and the matching
  globs ("`src/routes/orders.ts` (layer `handlers` via `src/routes/**`) imports
  `src/repositories/orders.ts` (layer `repositories`) — `handlers` may import: services,
  domain"), so an operator can see instantly whether the finding is real or the map is wrong.
- **Scan-side template emission (cheap follow-up):** the onboarding flow already proposes
  rules per stack; have it emit a commented-out `architecture.toml` skeleton from the
  detected stack so "unconfigured" has a one-edit path to "configured."

## 4. Per-checker mapping — every remaining architectural-tier rule

Out of scope by prior art: the 4 `process-*` rules (VCS-action gate) and the 4 `supabase/*`
rules (Passes 1–2). `AGENTIC-NO-TEST-TAMPER-1` is also **excluded**: its own TOML documents
that Camerata already ships the per-diff escalation check in the gov-dev pipeline.

**Group A — no AST at all (land first, no new dependencies):**

| Rule | Model | Detection | Verdict class | Stack scope |
|---|---|---|---|---|
| `PYTHON-TESTING-FILE-NAMING-1` | file tree only | files under test dirs (`tests/`, `test/`, `**/tests/**`) not matching `test_*.py` / `*_test.py`; also collects stray `test*.py` that pytest would silently skip | **Deterministic** | Python |
| `UI-UTC-DATES-1` | per-file forbidden-call scan (lexical, comment/string-aware — the Pass-1 splitter's discipline, not a bare regex) | `toLocaleString(` / `toLocaleDateString(` / `toLocaleTimeString(` / `Intl.DateTimeFormat(` in feature code outside `[helpers].date_helper` | **Deterministic with config** (helper exempted); without config: findings demoted to `needs-review` (medium) since the helper itself would self-flag | TS/JS UI |
| `ARCH-HANDLER-NO-DB-1` (interim promotion) | the EXISTING lexical proof `handler_no_direct_db` wrapped in an `ArchChecker` | as shipped (name-markers + brace depth); `[db].handles` overrides the default marker list when configured | **Needs-review** unconfigured (name heuristic), tightened by config; superseded in Group D | Rust now; TS via Group D |

**Group B — shallow structure: the import extractor (first tree-sitter consumer):**

| Rule | Model | Detection | Verdict class | Stack scope |
|---|---|---|---|---|
| `UI-IMAGE-COMPONENT-1` | per-file import list (`extract::imports`) + JSX element scan for raw `<img>` | import of `next/image` (or raw `<img>` element) in any file other than `[helpers].image_component` | **Deterministic with config**; unconfigured: skip (a repo may legitimately have no wrapper policy) | Next.js/React (TSX/JSX) |
| `ARCH-SERVER-AUTHZ-1` | per-file import list within `[authz].ui_paths` | import whose specifier or imported name matches `forbidden_ui_imports` | **Deterministic with config**; unconfigured: skip (helper names are inherently project-specific — the TOML's own `qualifies` says so) | UI stacks (TS/JS first) |

**Group C — the import graph (boundary map + resolver; the core of the layer):**

One checker struct, `ImportBoundaryChecker`, builds the model once — per-file imports for
every v1 language, each specifier best-effort resolved to a repo file (relative paths;
tsconfig `paths` aliases; `crate::`/`mod` mapping for Rust; dotted-module → path for
Python), then assigned a layer by `[layers]` glob match — and answers three rule ids:

| Rule | Detection over the graph | Verdict class | Stack scope |
|---|---|---|---|
| `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` | any edge `layer(from) → layer(to)` where `to ∉ [imports][from]` | **Deterministic** (config-gated; skip unconfigured) | Rust + TS/JS + Python |
| `ARCH-API-DTOS-1` | file in `[dtos].controllers` importing a file in `[dtos].domain_types` | **Deterministic** (config-gated; note: `default = false` — only runs where the project selected the DTO option) | same |
| `ARCH-STRICT-LAYERING-1` (import facet) | DB-client package import (`[db].handles`-associated modules) outside `[db].allowed_in` layers | **Deterministic** (config-gated) — complements the Group-D call facet | same |

**Group D — call-site AST (function boundaries + receivers; production handler-no-db):**

| Rule | Model | Detection | Verdict class | Stack scope |
|---|---|---|---|---|
| `ARCH-HANDLER-NO-DB-1` (production) | `extract::functions` + `extract::method_calls`; handler-ness from route attrs/decorators (`#[get]`, `@app.route`, Express `router.get` registration) with name-markers as fallback | `[db].handles` receiver method call inside a handler body; when `[layers]` exists, "file in `handlers` layer" replaces name guessing entirely | **Deterministic with config**; needs-review on attr/name fallback | Rust (`syn`) + TS + Python |
| `ARCH-STRICT-LAYERING-1` (call facet) | same call-site model + layer map | DB-handle call in a file whose layer ∉ `[db].allowed_in`; calls in `tx_flow_control_in` layers matching transaction primitives (`db.transaction(`) exempt | **Deterministic with config**; skip unconfigured | same |
| `ARCH-RESOURCE-LIFECYCLE-1` (spawn facet only) | Rust call-chain scan via `syn`: `tokio::process::Command` builder chains | spawn chain with no `.kill_on_drop(true)` in it | **Needs-review** always (the rule's own TOML splits it: spawn disposition is the greppable facet; tracked-shutdown + temp-RAII "verified by review" stays AI-advisory) | Rust v1; Node later |

**Group E — explicitly deferred (stay AI-advisory; document why):**

| Rule | Why deferred |
|---|---|
| `ARCH-STRUCTURED-ERRORS-1` | Its own `qualifies` says the honest mechanism is a **contract test** hitting failure paths against the project's envelope schema — that's runtime, not static analysis. A static "all handlers route through one error middleware" check requires modeling each framework's middleware chain; poor precision-to-effort. Correct home today: AI review + a `.camerata/checks.toml` manifest entry running the project's own contract test. |
| `ARCH-EXACT-DECIMALS-1` | Deciding WHICH surface is exactness-sensitive is project domain knowledge (the TOML says so). Checkable once a `[decimals]` config section annotates the surface — add in v1.1 with a worked config example; a name-heuristic (`price`/`amount` fields typed `f64`/`number`) is FP-prone across three verdict-legitimate alternatives (decimal, integer-cents, strings). |

## 5. Build order + effort (calibrated against the ~5x overestimation pattern)

| Pass | Contents | Estimate | Hard vs routine |
|---|---|---|---|
| **4a** | `architecture.toml` loader (+ config-diagnostic path, overlap detection, template emission note) · `**` glob support in `arch_checker.rs` · Group A checkers (file-naming, utc-dates, proof-checker promotion) · registry + tests | **~1 day** | All routine — the config loader mirrors `manifest.rs` line-for-line; checkers are path/lexical-grade with fixture tests |
| **4b** | `syn` + tree-sitter deps · `extract` module dispatch + `imports()` for all 4 v1 languages · specifier→file **resolver** · `ImportBoundaryChecker` (3 rule ids) + Group B checkers · e2e fixture repos (a TS layered app, a Rust workspace) | **~1.5–2 days** | **Genuinely hard: the resolver** (tsconfig aliases, Rust `mod` trees, Python packages — bounded by the ignore-unresolved rule, but the resolution table needs adversarial fixtures). Tree-sitter dep wiring is routine (pin ABI-compatible versions); grammar bundling is a solved cc-build problem, not a project |
| **4c** | `functions()` / `method_calls()` extractors (syn visitor + 3 tree-sitter queries) · production `HandlerNoDbChecker` (attrs/decorators + layer map) · `StrictLayeringChecker` call facet w/ tx exemption · resource-lifecycle spawn facet | **~1–1.5 days** | Moderately hard: handler classification per framework (route attrs/decorators/Express registration — enumerable, but each is a small research-and-fixture unit). The tx-flow-control exemption needs careful fixtures to avoid FPs on legitimate service code |
| Deferred | Group E, extra languages, wasm anything | — | Crisp triggers: `[decimals]` config demand; an engagement in a deferred language |

**Total Passes 4a–4c: ~3.5–4.5 orchestrated days.** The three genuinely hard parts, named:
(1) the boundary-map schema itself (a design problem — hence D1, not an effort line);
(2) cross-file import resolution (the only real algorithmic work; failure mode engineered to
false-negative); (3) per-framework handler classification in Group D. Multi-grammar
tree-sitter bundling — often assumed hard — is NOT: native grammar crates are pinned Cargo
deps that compile and link like any `cc` dependency.

Build 4a first and ship it: it lands two fully-deterministic rules and the config surface
with zero new dependencies, so the D2 dependency decision gates only 4b/4c, not the whole
layer.

---

## Pass 4a "Group A" landed

The three Group A checkers from §4's table are built on the existing `ArchChecker` seam —
zero new dependencies, zero config, no AST:

- **`PYTHON-TESTING-FILE-NAMING-1`** — `crates/checks/src/python_testing.rs`
  (`PythonTestFileNamingChecker`). Pure path/naming logic over `**/tests/**/*.py` +
  `**/test/**/*.py`: flags a file whose name signals test intent (`test*.py` prefix) but
  matches neither pytest discovery form (`test_*.py` / `*_test.py`), the exact "silently
  skipped" failure mode the corpus rule names. `__init__.py`/`conftest.py` are exempt. Fully
  deterministic — excluded from the LLM prompt, same as the Supabase checkers.
- **`UI-UTC-DATES-1`** — `crates/checks/src/ui_dates.rs` (`UtcDatesChecker`). Lexical,
  comment-aware (line-comment + block-comment stripping over `char`s, never raw bytes, so a
  multi-byte UTF-8 line can't panic a `str` slice) scan over `**/*.ts`/`.tsx`/`.js`/`.jsx` for
  `toLocaleString(` / `toLocaleDateString(` / `toLocaleTimeString(` / `Intl.DateTimeFormat(`.
  Since the `.camerata/architecture.toml` `[helpers].date_helper` exemption (D1) isn't built
  yet, EVERY finding carries a trailing `[needs review: ...]` marker — the same
  message-suffix convention `ui_core::rules::split_needs_review` already renders for
  calibration-flagged findings, so this required zero changes to `Finding`, the adapter, or
  the report/template/serializer.
- **`ARCH-HANDLER-NO-DB-1`** (interim promotion) — `crates/checks/src/handler_no_db_checker.rs`
  (`HandlerNoDbChecker`), wrapping the existing `architectural::handler_no_direct_db` lexical
  proof checker per-`.rs` file. Findings carry the same `[needs review: ...]` marker (name
  heuristic, no layer map to sharpen it).

**Seam amendment — `**` glob support:** `arch_checker::glob_match` gained a recursive
segment matcher (`glob_match_segments`) so a `**` glob segment matches zero-or-more whole
path segments (backtracking, bounded by path segment count — never a stack-overflow risk on
real repo paths). Exact-match/`*`-within-segment behavior for existing globs (the Supabase
checkers) is unchanged; new unit tests cover zero-segment, multi-segment, trailing-`**`, and
the "must not substring-match a similarly-named segment" (`testsuite/` ≠ `tests/`) cases.

**D3 (advisory-coexisting checkers):** `ArchChecker` gained a defaulted trait method,
`advisory_coexisting() -> bool` (default `false`). `all_checker_rule_ids()` — the set
`onboard.rs` subtracts from the LLM/semantic-audit prompt — now filters OUT any checker
that returns `true`. Only `HandlerNoDbChecker` overrides it: `ARCH-HANDLER-NO-DB-1` is
registered (and runs) like any other checker, but its rule id stays in the LLM prompt
alongside the native `needs-review` finding, per D3's exception for the name-heuristic
fallback. `PythonTestFileNamingChecker` and `UtcDatesChecker` both stay at the trait default
(fully excluded from the LLM prompt) — the task calling for advisory-coexistence named only
the handler-no-db promotion, and both of those two checkers answer their rule with a real
deterministic verdict, not a heuristic proxy.

Registered in `arch_checker::all_checkers()` alongside the two Supabase checkers. Tests:
unit coverage per checker (fires / clean / glob-scoping / malformed-input-no-panic) plus one
integration test (`crates/server/tests/group_a_architectural_checkers_e2e.rs`) driving all
three through the real `onboard::audit_repos` deterministic scan path over a committed
fixture (`tests/fixtures/group_a_arch_repo/`), and asserting the D3 exclusion-set behavior
end to end. `cargo test -p camerata-checks -p camerata-server` and
`cargo check --workspace` both green.
