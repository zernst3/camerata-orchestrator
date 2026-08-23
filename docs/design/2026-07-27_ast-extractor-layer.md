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

---

## Pass 4b-1 "foundation" landed

The three foundation pieces from Pass 4b-1's scope (config + D3 config-awareness + the
extractor/resolver utility layer) are built. **No `ImportBoundaryChecker` or other real
config-gated checker exists yet** — that's Pass 4b-2/4c, building on this.

### 1. `.camerata/architecture.toml` config + loader (D1)

`crates/checks/src/architecture_config.rs`. `ArchitectureConfig` is a typed struct matching
§3's schema exactly (`version`, `layers: BTreeMap<String, Vec<String>>`,
`imports: BTreeMap<String, Vec<String>>`, and optional `db`/`dtos`/`authz`/`helpers` sections),
`Serialize + Deserialize` so it round-trips through `toml`. Two loaders, mirroring how
`checks.toml` is read on both paths:

- `load_architecture_config(repo_root: &Path)` — worktree-on-disk (Layer-2), byte-for-byte the
  same `Ok(Some)`/`Ok(None)`/`Err` contract as `manifest::load_manifest`.
- `architecture_config_from_files(files: &[(String, String)])` — the scan's `RepoView`-shaped
  file slice, looking for `ARCHITECTURE_CONFIG_PATH` (`.camerata/architecture.toml`) by exact
  path match.

Missing file → `Ok(None)` on both (the common case). Malformed TOML (or a config missing the
required `version` field) → `Err`, never a panic — every loader-facing test asserts this
explicitly (`malformed_config_on_disk_returns_err_not_panic`,
`malformed_in_file_slice_returns_err_not_panic`, `config_missing_required_version_field_returns_err`).

Two behaviors beyond a bare loader, both needed by future checkers:

- `ArchitectureConfig::layer_for_path(path) -> Result<Option<&str>, LayerConflict>` — glob-
  classifies a file into a declared layer (reusing `arch_checker::matches_any_glob`, the same
  `**`-capable matcher every checker already uses). Two or more layers matching the SAME file
  returns `Err(LayerConflict)` rather than picking one — per design §3, a broken map must
  report as a config diagnostic, never silently pick a side or fire a rule finding.
- `ArchitectureConfig::diagnostics() -> Vec<String>` — the STATICALLY detectable subset of map
  errors (no repo files needed): an `[imports]` entry naming an undeclared layer, or the exact
  same glob string declared under two different `[layers]` entries.

17 tests: full-example parse (the design doc's own worked TOML, verbatim), round-trip through
`Serialize`, minimal/version-only configs, all four loader branches (missing/valid/malformed/
missing-required-field) on both the disk and file-slice paths, `layer_for_path`'s three
outcomes, and both `diagnostics()` cases.

### 2. D3 config-aware degradation

`ArchChecker` gained a second defaulted trait method alongside `advisory_coexisting`:

```rust
fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool { false }
```

(`crates/checks/src/arch_checker.rs`, next to the trait's existing `advisory_coexisting`).
Default `false` — every checker registered as of this pass (Group A + both Supabase checkers)
answers its rules from the repo's files alone, no config needed, so nothing changes for them.
A future config-gated checker overrides it to check config presence for `repo` (e.g. via
`architecture_config_from_files(repo.files)`).

The static `all_checker_rule_ids()` is UNCHANGED (kept for existing callers/tests that don't
have a `RepoView` in hand). The new per-repo sibling:

```rust
pub fn checker_rule_ids_for_repo(repo: &RepoView<'_>) -> HashSet<&'static str>
```

(`arch_checker.rs`, right after `all_checker_rule_ids`) filters out a checker when EITHER it's
`advisory_coexisting` (the existing D3 exception) OR `config_unsatisfied_for(repo)` is `true`.
`crates/server/src/onboard.rs` (around what was line 680, the `semantic` rule-set computation)
was refactored to call this per-repo: the `arch_checker_rule_ids` binding moved from BEFORE the
per-repo loop (computed once, statically) to INSIDE the `Ok(ExtractedRepo { files, .. })` match
arm, right after a repo's files are read — since config presence is a file-content fact, it can
only be known once the files are in hand. `semantic` (the rule set handed to the LLM audit) is
now computed at that same point, from the per-repo set. No other call site changed; the e2e
test (`group_a_architectural_checkers_e2e.rs::arch_handler_no_db_rule_id_stays_eligible_for_the_llm_prompt_per_d3`)
still passes unchanged, proving the refactor is behavior-preserving for every checker that
predates config-gating.

Since no real config-gated checker exists yet, the mechanism is proven with a unit-test-only
dummy checker in `arch_checker.rs`'s test module (`DummyConfigGatedChecker`), whose
`config_unsatisfied_for` checks `architecture_config_from_files(repo.files)`: one test with
`.camerata/architecture.toml` present in `RepoView::files` (→ `false`, excluded from the LLM
prompt as normal), one absent (→ `true`, stays advisory), plus a test asserting
`checker_rule_ids_for_repo` equals the static `all_checker_rule_ids()` for every currently-
registered (non-gated) checker regardless of the repo's files. This is the load-bearing
contract Pass 4b-2's `ImportBoundaryChecker` builds against.

### 3. The extractor utility layer (D2)

New `crates/checks/src/extract/` module tree, dispatched from `extract::{imports, functions,
method_calls}` over a `SourceLang` resolved by `extract::lang_for_path`:

```rust
pub enum SourceLang { Rust, TypeScript, Tsx, JavaScript, Python }
pub fn lang_for_path(path: &str) -> Option<SourceLang>;

pub enum ImportKind { Named, Namespace, Default, Dynamic, ReExport, SideEffect }
pub struct Import { pub specifier: String, pub names: Vec<String>, pub kind: ImportKind, pub line: usize }
pub fn imports(lang: SourceLang, source: &str) -> Vec<Import>;

pub struct FunctionSpan { pub name: String, pub start_line: usize, pub end_line: usize, pub attrs: Vec<String> }
pub fn functions(lang: SourceLang, source: &str) -> Vec<FunctionSpan>;

pub struct MethodCall { pub receiver: String, pub method: String, pub line: usize }
pub fn method_calls(lang: SourceLang, source: &str) -> Vec<MethodCall>;
```

One addition beyond the design sketch: `Import` gained a `kind: ImportKind` field (the task's
routing brief asked for "module path + imported symbols + **kind**"). It distinguishes a plain
named import from a re-export (`export { a } from 'x'` — still a dependency edge, but
semantically different from a normal import), a namespace/glob import (no enumerable names), a
default import, a side-effect-only import, and a dynamic `import()`/`require()`/
`importlib.import_module()` call — future checkers can filter on it without re-deriving it from
`specifier` shape.

**Per-language implementation** (one dispatch surface, per-language files behind it — `ecma.rs`
covers TS/TSX/JS in ONE implementation rather than three near-identical files, since the
node-kind vocabulary these functions need is identical across those three tree-sitter
grammars; this deviates from the design sketch's per-file-per-dialect suggestion for less
duplication, noted here as an engineering call):

- **`extract/rust_syn.rs`** — `syn::parse_file` + a `syn::visit::Visit` visitor. Line numbers
  via `proc-macro2`'s `span-locations` feature (`Span::start().line`); attribute TEXT is
  sliced verbatim from the source by `Span::byte_range()` rather than re-serialized through
  `quote`, so `#[get("/x")]` round-trips byte-for-byte. Walks `use`/`extern crate` (any
  nesting: top-level, inside `mod`, inside a fn body — `syn::visit`'s default dispatch covers
  all three for free), every `fn` (top-level, `impl` methods, default-bodied trait methods,
  fns nested inside fn bodies), and every `receiver.method(...)` call site.
- **`extract/ecma.rs`** — native `tree-sitter-typescript`/`tree-sitter-javascript`, one
  `EcmaDialect` enum (`TypeScript`/`Tsx`/`JavaScript`) selecting the grammar. A manual
  recursive tree walk matching on `node.kind()` (not the tree-sitter Query DSL the design
  sketch mentioned — a walk was faster to get provably correct in this pass and carries the
  same panic-safety contract; noted as an engineering-choice deviation, not a functional gap).
  Handles `import`/`export ... from`/dynamic `import()`/CommonJS `require()`, function
  declarations/methods/arrow-functions-via-binding-context (`const f = () => {}`, object
  methods, `export default`, and — captured anonymously — a bare callback argument like
  `router.get('/x', (req,res) => {})`), decorators (TS/JS attach as PRECEDING SIBLINGS, unlike
  Python), and `a.b.c()`-shaped call sites.
- **`extract/python.rs`** — native `tree-sitter-python`. Handles `import a, b as c`,
  `from x import (a, b as c)`, `from x import *`, leading-dot relative imports (`from . import
  x`, `from ..pkg import y` — see the resolver note below for how the dot-count is preserved
  in `specifier`), `importlib.import_module('x')` as the closest Python analogue to a dynamic
  import, decorators (attach as CHILDREN of a `decorated_definition` wrapper, unlike TS/JS),
  and `self.db.query()`-shaped call sites.

**Never-panic contract, verified per language**: `syn::parse_file` returns `Err` (not a panic)
on anything unparseable — every public function maps that to an empty `Vec`. Both tree-sitter
grammar families ALWAYS return `Some(Tree)` (error-recovery `ERROR` nodes embedded in an
otherwise-valid tree, never a parse failure) — the walkers only ever index into a node after
checking its kind/field, so a garbled tree still degrades to fewer/zero results. Adversarial
tests per language (truncated mid-token source, "binary-like" content built from
`\u{FFFD}`/control-char sequences, empty source, and a moderate-depth nesting stress case per
language to rule out a naive-recursion stack-overflow risk) all assert "does not panic" rather
than a specific result — the accepted failure mode is silence, never a crash.

Cargo deps added to `crates/checks/Cargo.toml`: `syn = "2"` (features `full`, `visit`,
`extra-traits`), `proc-macro2 = "1"` (feature `span-locations`), `tree-sitter = "0.25"`,
`tree-sitter-typescript = "0.23"`, `tree-sitter-javascript = "0.25"`, `tree-sitter-python =
"0.25"`. All confirmed to build NATIVELY (cc-compiled, statically linked, no wasm engine) —
verified in isolation before wiring them into this crate: `tree-sitter 0.25.10` is
ABI-compatible with all three grammar crates at these pinned versions. Build-time impact was
modest in this workspace (an incremental `cargo check -p camerata-checks` pulling in all four
new deps plus their own transitive deps — `cc`, `regex`, `streaming-iterator`, etc. — finished
in ~18s from a cold dependency fetch; day-to-day incremental builds are unaffected since these
are leaf dependencies with no proc-macro re-expansion cost on every build).

Per-language test coverage (fixture-driven, covering the design's named "tricky forms" —
re-exports, aliased imports, dynamic `import()`, `from x import y as z`, nested modules — plus
the adversarial no-panic proof):

| File | Tests |
|---|---|
| `extract/mod.rs` (dispatch) | 3 |
| `extract/rust_syn.rs` | 19 |
| `extract/ecma.rs` | 22 |
| `extract/python.rs` | 17 |

### 4. The cross-file import resolver

`crates/checks/src/extract/resolver.rs`. `build_import_graph(files: &[(String, String)]) ->
Vec<ResolvedImport>` walks every file, extracts its imports via the layer above, and resolves
each specifier to a target file when — and only when — resolution is unambiguous. Per the
design's explicit FP-avoidance stance, EVERY resolution path fails false-negative:

- **Rust**: only `crate::`/`self::`/`super::`-prefixed specifiers are resolved (v1 scope — a
  bare/extern-crate specifier like `serde::Deserialize` is out of scope and dropped, since
  resolving it correctly needs full crate-name resolution this pass doesn't attempt). The
  crate root is found structurally (the nearest ancestor `src/` directory in the importing
  file's own path — no `Cargo.toml` read needed), and `mod.rs`/`lib.rs`/`main.rs` collapse to
  their containing directory's module path, matching Rust's own file-to-module convention.
- **TS/TSX/JS**: relative specifiers (`./`, `../`, `/`) resolve via directory-join + a fixed
  extension/index-file priority list (`.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`, then
  `/index.*`). Bare specifiers only resolve through a `tsconfig.json` `compilerOptions.paths`
  alias (read from the same file slice, `serde_json`-parsed — already a crate dependency, no
  new one needed); a malformed or JSONC-with-comments `tsconfig.json` degrades to "no aliases"
  rather than erroring the whole resolver.
- **Python**: leading-dot relative imports resolve by counting the dots and walking up the
  importing file's own directory tree (real Python package semantics: level 1 = the importing
  file's own containing directory, each extra dot one more level up). An absolute dotted
  import (`import a.b.c`) resolves by SUFFIX match against every `.py`/`.pyi` file's own
  derived dotted module path, since this extractor doesn't know the repo's actual source root
  (`src/`, `app/`, or the repo root itself) — zero matches OR more than one both drop the edge
  (the ambiguity case), never a guess.

`reachable_from(graph, start) -> HashSet<String>` is a small BFS-with-visited-set utility over
the resulting edge list, added so "handles cycles without infinite loops" has real traversal
logic to prove against rather than being trivially true of a flat edge list — a genuine A↔B
import cycle terminates in `O(V+E)`.

20 resolver tests: one per language's relative/absolute resolution paths, the tsconfig-alias
path (including a malformed-tsconfig no-panic case), the ambiguous-Python-suffix-match drop,
cross-language aggregation in one `build_import_graph` call, the cycle-traversal test, and two
adversarial cases (empty file set; a file that "imports itself", which must resolve to NO
self-edge rather than a meaningless loop).

### Test totals + verification

`cargo test -p camerata-checks --lib`: **469 passed, 0 failed** (101 new tests across the six
files above: 3 dispatch + 19 Rust + 22 ecma + 17 Python + 20 resolver + 17 config, plus 3 new
D3 unit tests in `arch_checker.rs`). `cargo test -p camerata-server --lib`: **1181 passed, 0
failed** (refactor-only on this crate — the `semantic`/`arch_checker_rule_ids` move in
`onboard.rs`; no new server-side tests this pass). The existing
`group_a_architectural_checkers_e2e` integration test (2 tests) still passes unchanged,
confirming the D3 refactor is behavior-preserving. `cargo check --workspace` green.

### What Pass 4b-2 (`ImportBoundaryChecker`) builds on this

- Load `.camerata/architecture.toml` for the repo via `architecture_config_from_files`; skip
  entirely (stay LLM-advisory via `config_unsatisfied_for`) when absent.
- Call `extract::resolver::build_import_graph(repo.files)` once to get every intra-repo import
  edge.
- Classify each edge's endpoints via `ArchitectureConfig::layer_for_path` (surfacing a
  `LayerConflict` as a config diagnostic, never a rule finding).
- For each edge where BOTH endpoints land in a declared layer, check `to`'s layer against
  `[imports][from]`; a target layer not in that list is the `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1`
  violation. The same edge set, filtered by `[dtos]`/`[db]` presence, answers
  `ARCH-API-DTOS-1` and the import facet of `ARCH-STRICT-LAYERING-1` per design §4 Group C.
- Override `config_unsatisfied_for` to return `true` whenever
  `architecture_config_from_files(repo.files)` is `Ok(None)` (or the specific section the
  rule needs is absent) — the D3 mechanism this pass built is otherwise a no-op until this
  override exists.

---

## Pass 4b-2 "`ImportBoundaryChecker`" landed

The first REAL config-gated checker on this layer: `crates/checks/src/import_boundary_checker.rs`
(`ImportBoundaryChecker`), registered in `arch_checker::all_checkers()` alongside the two
Supabase checkers, Group A's three, and the handler-no-db promotion. It answers all three rule
ids design §4 Group C names, as ONE struct building ONE model (the resolved import graph) per
the design's own framing:

- **`ARCH-NO-CROSS-BOUNDARY-IMPORTS-1`** — any resolved import edge whose source and target
  both classify into a declared `[layers]` entry, where the target layer is not in the source
  layer's `[imports]` allow-list. A source layer with NO `[imports]` key at all is treated
  exactly like an explicit empty list (the schema's own "any edge not listed is forbidden"
  semantics) — this includes same-layer edges unless a project explicitly lists itself as its
  own allowed target, a literal reading of the worked example's `domain = []`.
- **`ARCH-API-DTOS-1`** — the SAME resolved edge set, filtered to `[dtos].controllers` ->
  `[dtos].domain_types` edges. Silently a no-op whenever `[dtos]` isn't configured (the rule's
  own `default = false`), independent of whether `[layers]`/`[imports]` are configured.
- **`ARCH-STRICT-LAYERING-1` (import facet only)** — deliberately NOT built on the resolved
  graph: a real DB-client package (`prisma`, `supabase-js`, a bespoke `sea_orm` wrapper) is
  typically EXTERNAL, so the resolver would drop it (correctly — see its own false-negative
  discipline). Instead, a raw per-file `extract::imports` scan checks each declared-layer
  file's specifiers for an EXACT TOKEN match (specifier split on non-alphanumeric/`_`
  boundaries, e.g. `@prisma/client` -> `["prisma","client"]`) against `[db].handles` — a token
  match, not a substring match, so a marker like `"db"` can never spuriously fire on
  `"database-types"`. The call-site facet (`db.query(...)` inside a handler body) is Pass 4c,
  not this checker.

**A deliberate widening beyond the design table's literal wording:** the import facet also
exempts `[db].tx_flow_control_in` layers (unioned with `[db].allowed_in`), not just
`allowed_in`. The design table names the `tx_flow_control_in` exemption for the Group-D CALL
facet only, but a service that legitimately wraps a repository call in `db.transaction(...)`
(the rule TOML's own carve-out) needs to IMPORT the DB client to do that — an import-facet
check with no matching exemption would flag that same legitimate service, defeating the
option's purpose. Documented in the checker's module doc as a call for Pass 4c to revisit once
the call-site facet exists and both facets can be cross-checked against real fixtures.

**D3, this pass's version:** `ArchChecker::config_unsatisfied_for` answers ONE bool per
checker — it cannot express "satisfied for rule A, unsatisfied for rule B" within a single
repo. `ImportBoundaryChecker` resolves this by gating on config PRESENCE alone:
`architecture_config_from_files(repo.files).ok().flatten().is_none()`. Absent OR malformed
(the config module's own contract: treat `Err` like `Ok(None)`) → all three rule ids stay
LLM-advisory for that repo; present and parses → all three are excluded, i.e. the same simple
presence/absence gate the task named. This is a KNOWN coarsening for `ARCH-API-DTOS-1`
specifically: a repo that configures `[layers]`/`[imports]`/`[db]` but never opts into
`[dtos]` gets `ARCH-API-DTOS-1` excluded from the LLM prompt even though this checker emits
zero verdicts for it. Flagged here as a refinement candidate — a future pass could widen
`ArchChecker` to a per-rule-id `config_unsatisfied_for` (e.g. returning a
`HashSet<&'static str>` of UNSATISFIED ids instead of one bool) — not fixed in this pass, since
the task's own framing named the coarser presence/absence gate.

**Interest globs** include the v1 source extensions (`**/*.rs`, `**/*.ts`, `**/*.tsx`,
`**/*.js`, `**/*.jsx`, `**/*.py`) PLUS `.camerata/architecture.toml` and `tsconfig.json` /
`**/tsconfig.json` — the config and any tsconfig alias file must be declared here too, or the
Layer-2 runner's `collect_interest_files` (which only reads the UNION of every ARMED checker's
globs from the worktree) would never see them, and the checker would wrongly read "no config"
even when one exists on disk.

**False-negative safeguards** (all proven by dedicated tests, see below): a `LayerConflict`
(overlapping `[layers]` globs matching the same file) on either edge endpoint drops the edge;
an unresolved import (external package, ambiguous Python suffix match, ...) never reaches the
checker at all (the resolver already dropped it); a file matching no declared layer is never
judged; a malformed `.camerata/architecture.toml` degrades to the same "absent" path, never an
`Err` propagated or a panic.

**Tests.** `crates/checks/src/import_boundary_checker.rs`: 23 unit tests — a Rust AND a TS
fixture each proving a cross-layer violation with correct file:line + layer names in the
message, a compliant layered repo (both languages) staying clean, the no-config abstain (zero
findings + `config_unsatisfied_for` true + rule ids absent from
`checker_rule_ids_for_repo`), the config-present exclusion (rule ids present in
`checker_rule_ids_for_repo`), malformed-config-treated-as-absent, external-package /
unclassified-file / layer-conflict false-negative cases, the full `ARCH-API-DTOS-1` fixture
plus its "no `[dtos]` section" silence case, the `ARCH-STRICT-LAYERING-1` import-facet fixture
(flagged outside `allowed_in`, clean inside `allowed_in`, exempted via
`tx_flow_control_in`, silent with no `[db]` section, and the token-vs-substring marker-match
adversarial case), and malformed-source / empty-file-set no-panic cases. Two supporting fixes
to existing `arch_checker.rs` tests were required now that a REAL config-gated checker exists
(previously only a test-only `DummyConfigGatedChecker` proved the mechanism): the static
`all_checker_rule_ids()` set (which has no per-repo config awareness) now correctly asserts it
STILL CONTAINS the three gated ids (this checker doesn't opt into `advisory_coexisting`), and
the old `checker_rule_ids_for_repo_matches_static_set_when_no_checker_is_config_gated` test —
whose premise ("no checker is config gated") this pass falsifies — was replaced by two tests
proving the real divergence: the per-repo set drops the three ids for an unconfigured repo,
and matches the static set again once `.camerata/architecture.toml` is present.

**Integration — the scan (Plug point A).** `crates/server/tests/import_boundary_checker_e2e.rs`
+ two fixtures: `tests/fixtures/import_boundary_repo/` (a real `.camerata/architecture.toml`,
4 layers, plus a Rust AND a TypeScript handler file each importing the repositories layer
directly — both flagged, with the compliant services/repositories/domain edges in the SAME
repo staying silent) and `tests/fixtures/import_boundary_unconfigured_repo/` (the identical
TS violation shape with NO config file at all). Three tests, driven through the real
`onboard::audit_repos`: the configured repo fires exactly 2 findings (one per language) with
correct file/line/layer content and the `camerata-arch` preview tag; the unconfigured repo
gets zero deterministic findings for all three rule ids; and a third test builds a `RepoView`
from each fixture's REAL scan-read files (`read_local_repo_files`) and asserts
`checker_rule_ids_for_repo` diverges exactly as designed (configured excludes all three ids,
unconfigured leaves all three eligible) — the PER-REPO, config-aware D3 contract exercised
end-to-end for the first time by a real checker rather than a test-only dummy.

**Integration — the Layer-2 gov-dev gate (Plug point B).**
`crates/checks/tests/import_boundary_checker_gov_dev_loop_e2e.rs`, mirroring
`arch_check_runner_gov_dev_loop_e2e.rs`'s existing convention: drives the public
`camerata_checks::runner_for_worktree` entry point over a real on-disk worktree. Four tests —
a cross-boundary violation bounces with the exact file:line and rule id in the diagnostics; a
fully compliant worktree passes clean; an un-armed rule never bounces even with matching
files present; and an unconfigured worktree with the IDENTICAL violation shape does not bounce
(D3 holds at Layer-2 too, not just the scan).

**Test totals + verification.** `cargo test -p camerata-checks --lib`: 493 passed (23 new for
`import_boundary_checker`, plus 2 updated + 2 new tests in `arch_checker.rs`'s own suite).
`cargo test -p camerata-checks -p camerata-server` (all lib + integration + doc tests): every
suite green, including the two new e2e files above (3 + 4 tests). `cargo check --workspace`
green throughout.

### What Pass 4c still needs

- **`functions()` / `method_calls()`-based checkers** — this pass never called those two
  extractor functions; Pass 4c is where they get their first real consumer.
- **Production `HandlerNoDbChecker`**: route-attribute/decorator classification
  (`#[get(...)]`, `@app.route`, Express `router.get` registration) replacing
  `handler_no_direct_db`'s name heuristic, sharpened by `[layers]` when present (a file in the
  `handlers` layer replaces name-guessing entirely).
- **`ARCH-STRICT-LAYERING-1`'s CALL facet**: a `[db].handles` receiver method call
  (`db.query(...)`) inside a body whose layer isn't in `[db].allowed_in`, with calls inside
  `tx_flow_control_in` layers exempted ONLY when they match a transaction-primitive shape
  (`db.transaction(...)`) — this pass's import-facet exemption is coarser (any import from a
  tx-flow-control layer is exempt, not just transaction-wrapping calls), so Pass 4c's call
  facet is where the finer-grained, call-site-verified version of that exemption belongs.
- **`ARCH-RESOURCE-LIFECYCLE-1` (spawn facet)**: `syn`-based `tokio::process::Command`
  builder-chain scan for a missing `.kill_on_drop(true)`.
- Optional refinement candidate (not required, noted above): per-rule-id
  `config_unsatisfied_for` granularity, so a repo that configures `[layers]`/`[db]` but not
  `[dtos]` doesn't have `ARCH-API-DTOS-1` coarsely excluded from the LLM prompt alongside the
  other two.

---

## Pass 4c "call-site AST checkers" landed

The three Group D checkers named above are all built. This is the last of the architectural
checker set — `functions()`/`method_calls()` get their first real consumers, and
`ARCH-HANDLER-NO-DB-1` moves from a Pass 4a interim lexical promotion to a real AST checker.

### 1. Production `HandlerNoDbChecker` (`ARCH-HANDLER-NO-DB-1`)

`crates/checks/src/handler_no_db_checker.rs` — a full rewrite of the same struct name, built
on `extract::functions` + `extract::method_calls` instead of the deleted
`crate::architectural::handler_no_direct_db` lexical scanner (that module, and its 9 tests, are
gone — `crates/checks/src/architectural.rs` no longer exists). **Exactly one checker answers
`ARCH-HANDLER-NO-DB-1`** — proven by a dedicated registry test
(`exactly_one_checker_owns_arch_handler_no_db_1`).

Three-tier classification per function, innermost-containing-span correlation between every
`method_calls` call site and the `functions` span it falls inside (ties broken toward the
tightest/innermost span, so a call inside a nested closure/fn attributes to that nested scope,
not the outer one):

- **Deterministic (`high` severity)** — the file classifies into a `.camerata/architecture.toml`
  `[layers]` entry literally named `"handlers"` (the schema's own worked-example convention).
  Every function in that file is handler-scoped with NO attribute/name guessing; a file
  confirmed to be some OTHER declared layer is skipped entirely (config already answered "not a
  handler," so no fallback guess is attempted for it either).
- **`needs-review` (attr fallback)** — no layer confirmation, but the function carries a route
  attribute/decorator/registration marker: Rust `#[get("/x")]` / `#[actix_web::get(...)]`,
  Python `@app.route(...)` / `@app.get(...)`, or the new synthetic Express marker (below).
- **`needs-review` (name fallback)** — weakest tier, ported verbatim from the deleted lexical
  checker's own marker list (`handler`/`handle_`/`controller`/`_route`/`endpoint`).

A direct DB-handle call inside a handler-classified function is a violation when the call's
receiver's LAST `.`/`::` segment EXACTLY matches a `[db].handles` marker (or the default
`db`/`pool`/`conn`/`tx`/`executor` list absent config) — an exact-segment match, not the old
lexical scanner's raw-text substring search, so an identifier like `database` can never
spuriously match marker `db`
(`ast_version_does_not_confuse_a_longer_identifier_with_an_exact_db_marker` proves this directly
against a case that would have been genuinely ambiguous for a text-based scanner).

**Express registration support (new extractor capability):** `extract::ecma`'s `functions()`
previously only captured decorators for an anonymous arrow/function-expression callback; it now
also recognizes the Express/Koa registration shape (`router.get('/x', (req,res) => {...})`) via
a new `express_route_marker` helper, tagging the callback's `FunctionSpan.attrs` with a
synthetic `<express-route:router.get>` marker. Deliberately excludes `.use(...)` (Express
middleware registration, which would over-classify ordinary middleware as a handler) — 4 new
`extract::ecma` tests cover this (all 8 HTTP verbs, the `use` exclusion, and a plain unrelated
callback like `.map()` staying unmarked).

**D3 (config-aware, per-repo — NOT the deleted checker's unconditional `advisory_coexisting`):**
`config_unsatisfied_for` returns `true` unless the repo declares BOTH a `"handlers"` layer AND a
non-empty `[db].handles` list — the same per-repo `ImportBoundaryChecker` pattern from Pass
4b-2, not the interim checker's always-advisory mechanism. A per-FILE fallback to `needs-review`
can still occur inside an otherwise-"configured" repo (a file the layer map doesn't classify) —
a known, deliberate coarsening at the same granularity `ImportBoundaryChecker`'s own
`ARCH-API-DTOS-1` note already documents.

**Supersession mechanics:** the interim `handler_no_db_checker.rs` (7 tests, wrapping the
lexical proof function, `advisory_coexisting() == true` unconditionally) is fully replaced.
`crates/server/tests/group_a_architectural_checkers_e2e.rs` — the one existing e2e test
asserting on the interim's behavior — is updated: the fixture (no `.camerata/architecture.toml`,
a name-only-marked `list_orgs_handler`) still produces the identical `needs-review` finding
(behavior-preserving for the unconfigured case), and the D3 test is rewritten from a check
against the STATIC `all_checker_rule_ids()` (which now legitimately contains
`ARCH-HANDLER-NO-DB-1`, since the production checker isn't unconditionally advisory anymore) to
the PER-REPO `checker_rule_ids_for_repo`, matching how `ImportBoundaryChecker`'s own tests
already work.

22 tests in `handler_no_db_checker.rs`: deterministic Rust + TS fires (high severity, no
needs-review marker), a config-confirmed OTHER-layer file never flagged even though it touches
`db` and is named like a handler (the direct improvement over the deleted lexical checker, which
had no layer awareness at all), a delegating handler staying clean, all three attr-marker
languages (Rust attribute, Python decorator, Express registration) landing `needs-review`, the
name-only fallback matching the deleted checker's exact behavior, the AST-vs-lexical
false-positive-avoidance case above, Python multi-language coverage, every D3 branch, registry
wiring (including the "exactly one owner" assertion), and malformed/truncated/binary-ish source
across all three v1 languages plus a `LayerConflict` case (falls back to heuristic, never a
panic and never a fabricated deterministic verdict).

### 2. `ARCH-STRICT-LAYERING-1` call facet — `StrictLayeringCallChecker`

`crates/checks/src/strict_layering_call_checker.rs` (new file) — the call-site half of this
rule, alongside `ImportBoundaryChecker`'s existing import facet (Pass 4b-2). Built on
`extract::method_calls` + the SAME `.camerata/architecture.toml` `[layers]`/`[db]` map, but
answering a genuinely different predicate: any `[db].handles`-marked call inside a file whose
layer isn't in `[db].allowed_in`, UNLESS the layer is in `[db].tx_flow_control_in` AND the call
itself is a `.transaction(...)` invocation.

**The `tx_flow_control_in` exemption is now call-shape-verified, not import-coarsened.** Pass
4b-2's import facet exempted EVERY import from a `tx_flow_control_in` layer (documented there as
a deliberate, coarser widening, since an import alone can't distinguish "wraps a repo call in a
transaction" from "issues ad-hoc queries"). This checker is the finer-grained version that note
anticipated: `service_calling_db_directly_without_transaction_is_still_flagged` proves a service
in a `tx_flow_control_in` layer that calls `db.query(...)` directly (not
`.transaction(...)`) is STILL flagged, while `service_wrapping_a_call_in_transaction_is_exempt`
proves the actual `.transaction(...)` call is not. The two facets' exemptions now compose
correctly at their respective granularities.

**Two checkers, one rule id — by design, not a conflict.** `ARCH-STRICT-LAYERING-1` is answered
by BOTH `ImportBoundaryChecker` (import facet) and `StrictLayeringCallChecker` (call facet);
`all_checkers()` runs every registered checker and unions their findings, so this is legitimate
(unlike `ARCH-HANDLER-NO-DB-1`, which this pass supersedes down to exactly one owner — a
different rule with a different history). A registry test
(`checker_is_registered_and_shares_the_rule_id_with_import_boundary_checker`) asserts exactly
TWO checkers own this rule id, guarding against an accidental future duplicate or an accidental
removal of one of the two facets.

**D3:** gated on `[db]` section presence (not full layers+handlers — this facet only needs
`[db]` to answer anything). Silent + advisory when `[db]` is absent or the config itself is
absent/malformed; false-negative safeguards mirror `ImportBoundaryChecker`'s own (unclassified
file, `LayerConflict`, exact-token marker matching that never substring-matches a longer
identifier).

14 tests: Rust + TS fires, a compliant repository-layer file staying clean, both transaction-
exemption directions, every silence condition, the false-negative safeguards, malformed-source
and empty-file-set no-panic cases, and the two-owners registry assertion.

### 3. `ARCH-RESOURCE-LIFECYCLE-1` spawn facet — `ResourceLifecycleChecker`

`crates/checks/src/resource_lifecycle_checker.rs` (new file) — Rust-only (v1 scope per the
design table), a bespoke `syn::visit::Visit`-based scan (NOT built on `extract::method_calls`,
since chain/variable correlation is specific to this one rule) for a `tokio::process::Command`
builder chain that reaches `.spawn()`/`.output()`/`.status()` with no `.kill_on_drop(true)`
anywhere in its chain.

**Two call shapes handled:** an INLINE chain (`Command::new("x").arg("y").spawn()`, checked by
recursively unwrapping the receiver spine of the terminal call) and a VARIABLE-TRACKED chain
(`let mut cmd = Command::new("x"); cmd.arg("y"); cmd.kill_on_drop(true); cmd.spawn();` — this
codebase's OWN `subprocess.rs::run_with_heartbeat` shape, proven directly by
`variable_tracked_with_kill_on_drop_as_a_separate_statement_is_clean`). Variable tracking is
FILE-WIDE, not per-function-scope — a documented, deliberate simplification: it can only ever
SUPPRESS a real violation (two functions coincidentally reusing a variable name like `cmd`,
one protected), never fabricate one, so it errs in the safe direction per the false-negative
mandate.

**`std::process::Command` is explicitly skipped** (an explicit `std::` path segment in the
`Command::new` call) — that type has no `kill_on_drop` method at all, so flagging it would be a
fabricated finding, not a real one (`std_process_command_is_skipped_never_flagged`).

**D3: `needs-review` ALWAYS, via `advisory_coexisting`, not `config_unsatisfied_for`.** This is
NOT a config gap — the rule's own TOML draws the line between "spawn disposition is
mechanically greppable" and "tracked shutdown + temp-file RAII are verified by review" by
design. `ResourceLifecycleChecker` is now the checker `arch_checker.rs`'s own
`advisory_coexisting` doc-comment cites as the canonical example (replacing the deleted interim
`HandlerNoDbChecker` reference, since that checker no longer uses this mechanism).

15 tests: inline and variable-tracked fires, all three kill-on-drop-present shapes (inline
chain, separate statement, chained at bind time) staying clean, `.output()`/`.status()` also
checked, the `std::process::Command` and unrelated-`.spawn()`-method false-negative safeguards,
`tokio::spawn(...)` (a free function, never even visited as a method call) correctly ignored,
non-Rust glob scoping, malformed/binary-ish source no-panic, and the `advisory_coexisting`/
registry assertions.

### Registry + shared updates

- `arch_checker::all_checkers()` now registers 8 checkers: the 2 Supabase checkers, the 2
  remaining Group A checkers (`PythonTestFileNamingChecker`, `UtcDatesChecker`), the production
  `HandlerNoDbChecker`, `ImportBoundaryChecker`, and this pass's two new checkers
  (`StrictLayeringCallChecker`, `ResourceLifecycleChecker`).
- `all_checkers_registry_covers_expected_rule_ids` and
  `checker_rule_ids_for_repo_drops_the_real_config_gated_checkers_ids_when_unconfigured` both
  extended to include `ARCH-HANDLER-NO-DB-1` in their "gated ids" lists (it now behaves like
  `ImportBoundaryChecker`'s ids: present in the static set, config-aware per-repo).
- `checker_rule_ids_for_repo_matches_static_set_when_every_config_gated_checker_is_satisfied`
  (renamed from `..._when_the_config_gated_checker_is_satisfied`) now uses a fixture config that
  satisfies EVERY config-gated checker registered as of this pass (a `"handlers"` layer + a
  `[db]` section with non-empty `handles`), not just a bare `version = 1` — the old minimal
  fixture stopped being sufficient once `HandlerNoDbChecker` and `StrictLayeringCallChecker`
  both gained their own, stricter `config_unsatisfied_for` gates.
- `all_checker_rule_ids_excludes_advisory_coexisting_checkers` now asserts against
  `ARCH-RESOURCE-LIFECYCLE-1` (the new unconditionally-advisory checker) instead of the deleted
  interim `ARCH-HANDLER-NO-DB-1` case.

### Integration proof (scan + gov-dev)

- **Scan (Plug point A).** `crates/server/tests/handler_no_db_and_strict_layering_call_e2e.rs`
  + two fixtures: `tests/fixtures/handler_no_db_repo/` (a real `.camerata/architecture.toml`
  with `[db]`, a Rust AND a TypeScript handler each calling `db` directly, a service wrapping a
  repo call in `db.transaction(...)`, and a repository querying `db` directly) and
  `tests/fixtures/handler_no_db_unconfigured_repo/` (the IDENTICAL TS handler shape, no config
  at all). Three tests: the configured repo fires BOTH `ARCH-HANDLER-NO-DB-1` and
  `ARCH-STRICT-LAYERING-1` exactly twice each (one per language) at `high` severity with the
  compliant service/repository files staying silent under either rule; the unconfigured repo
  gets ZERO findings for either rule (the handler's name carries no marker either, so this
  proves config is doing real classification work, not just downgrading severity); and a third
  test proves `checker_rule_ids_for_repo` diverges exactly as designed between the two fixtures,
  mirroring `import_boundary_checker_e2e.rs`'s own convention.
- **Gov-dev (Plug point B).**
  `crates/checks/tests/handler_no_db_and_strict_layering_call_gov_dev_loop_e2e.rs`, mirroring
  `import_boundary_checker_gov_dev_loop_e2e.rs`. Four tests over the real
  `camerata_checks::runner_for_worktree` entry point: a direct-DB-call handler bounces under
  BOTH rule ids with the exact file:line; a compliant worktree (repository DB access + a
  tx-wrapped service) passes clean; an unarmed role never bounces; and an unconfigured worktree
  with the identical violation shape does not bounce either (D3 holds at Layer-2 too).

### Test totals + verification

`cargo test -p camerata-checks --lib`: **532 passed, 0 failed** (up from 493 at the end of Pass
4b-2 — net +39: the deleted `architectural.rs` module's 9 tests, the interim
`handler_no_db_checker.rs`'s 7 tests replaced by the production version's 22 (net +15), the new
`strict_layering_call_checker.rs` (+14), the new `resource_lifecycle_checker.rs` (+15), and 4
new `extract::ecma` tests for the Express route marker). `cargo test -p camerata-server --lib`:
**1181 passed, 0 failed** (unchanged — this pass only added integration tests, no server `lib.rs`
changes). Every integration test file green, including the two new e2e files above (3 + 4
tests) and the updated `group_a_architectural_checkers_e2e.rs` (2 tests, one rewritten for the
production checker's per-repo D3 gate). `cargo check --workspace` green throughout (the only
warnings present are pre-existing and unrelated to this pass — `camerata-ui`/`camerata-cli` dead
code, untouched by any file this pass changed).

### What's left after Pass 4c

With this pass, every architectural-tier checker named in §4 Groups A-D is built and registered
except the two Group E rules, which stay EXPLICITLY DEFERRED (not an oversight — see §4's own
table): `ARCH-STRUCTURED-ERRORS-1` (the honest mechanism is a contract test against the
project's envelope schema, not static analysis — correct home is AI review + a `.camerata/
checks.toml` manifest entry) and `ARCH-EXACT-DECIMALS-1` (needs a `[decimals]` config section
naming the exactness-sensitive surface before it's checkable at all; a name-heuristic would be
FP-prone across three legitimate alternatives). The only remaining item on the Pass 4a-4c build
order (§5) is **Pass 5, the CI distributable** — packaging this checker set for a client's own
CI pipeline outside the Camerata-hosted scan/gov-dev loop, out of scope for this pass.
