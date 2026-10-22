# W29 detector layer — the actual implementation design (2026-10-21)

## Why this exists

W29's nine consistency rules are only valuable when they FIND the inconsistency. Shipping them
"emit-only" (a convention in CONVENTIONS.md, never scanned) is a half-measure: it governs the
client's future work but flags nothing in the code in front of us. This doc designs the missing
**detector layer** so each rule is built rule + detector + safe-twin together, to the project's
correctness bar. Companion to the build-order plan `2026-10-21_w29-execution-plan.md`.

## What already EXISTS (reuse, do not rebuild)

Grounded against `crates/checks/src`:

- **Per-language AST extraction** (`extract/`): `extract::imports(lang, src) -> Vec<Import>`,
  `extract::functions(lang, src) -> Vec<FunctionSpan>`, `extract::method_calls(...)`,
  `extract::lang_for_path(path) -> Option<SourceLang>`; dialect parsers in `ecma.rs` (JS/TS),
  `python.rs`, `rust_syn.rs`. Native tree-sitter grammars (ts/js/python) + `syn` (Rust) are
  already dependencies.
- **Cross-file import graph + reachability** (`extract/resolver.rs`):
  `build_import_graph(files) -> Vec<ResolvedImport>` and `reachable_from(graph, start) -> HashSet`.
  This is most of ARCH-DEAD-CODE-1 for free.
- **The checker contract** (`arch_checker.rs`): `trait ArchChecker { fn check(&self, repo:
  &RepoView) -> Vec<ArchViolation>; }`, run by `arch_check_runner`; `ArchViolation`s become
  `onboard::Finding`s via `server/src/onboard/architectural.rs`. `import_boundary_checker.rs` and
  `strict_layering_call_checker.rs` are worked models.
- **The eslint tier** (`scan_tools.rs` + `dep_audit`-style provisioning): produces
  `react-hooks/*` findings, verified green post-W30. UI-REACT-HOOKS-LINT-1 reuses it wholesale.
- **Per-domain proposal** (`propose.rs::propose_corpus_rules`, selecting by `r.domain` against
  `domains_for_stack`): a new rule in an ALREADY-mapped domain is proposed automatically, with no
  per-rule wiring. Only a NEW domain needs `domains_for_stack`/`domain_to_glob` wiring. There is
  NO `arch` or `style` domain (corrected 2026-10-21 by the Fable audit: existing ARCH-* rules live
  in `api-layer`/`universal`/`permissions`, and `domain_to_glob` has no `arch`/`style` arm, so a
  rule homed there would fall to `**` with a warning and never be proposed). Each W29 rule is
  homed in an existing, already-mapped domain; see "Domain home per rule" below.

## What is MISSING (build these)

### Shared pass 1 — a literal index (small; for ARCH-SHARED-CONSTANT-1)
`extract::literals(lang, src) -> Vec<Literal { value, kind (string|number), span }>`, a tree-sitter
query per dialect (sibling to `imports()`), plus a cross-file `BTreeMap<normalized_value,
Vec<(file, span)>>` built in the checker. Shape heuristics from the source: ignore trivial values
(0/1/"", protocol/i18n strings) via an allowlist; flag a domain literal repeated across ≥2 modules.

### Shared pass 2 — a token clone detector (the big one; for 3 rules)
A jscpd-class, token-based, cross-file duplicate detector. This is the one genuinely new
substantial pass; rules 4/5/6 reuse it.

- **API:** `clone::detect(files, opts: {min_tokens, langs}) -> Vec<CloneFamily { members:
  Vec<CodeSpan>, token_len }>`.
- **Algorithm:** tokenize each file (reuse the tree-sitter parse already available per dialect;
  tokens = node kinds + normalized identifiers), compute a rolling hash over windows of
  `min_tokens`, bucket equal hashes, verify candidates by token-equality (hash collisions
  rejected), then merge overlapping windows into maximal families. Deterministic; no model; one
  pass per repo (source §91: costs no tokens, reduces advisory input).
- **Normalization knob:** identifier-insensitive matching (so `a ?? b` vs `a || b` compare by
  structure) with a flag to KEEP operators/literals distinct — that distinction is what lets
  ARCH-SINGLE-SOURCE-LOGIC-1 say "the copies do NOT agree."

### The nine checkers (ArchChecker impls)

| # | rule | det. | uses | algorithm sketch | safe twin |
|---|------|------|------|------------------|-----------|
| 1 | ARCH-ONE-IMPORT-PATH-1 | D | `imports()` + `resolver` + new `exports()` | canonicalize every imported symbol through re-export edges to its `(origin_file, name)`; flag when importers outside the barrel's own subtree reach it via ≥2 distinct resolved targets (see the trap section) | one `index.ts` barrel everyone outside uses; files inside the barrel's dir importing siblings directly |
| 3 | ARCH-SHARED-CONSTANT-1 | D | shared pass 1 | literal repeated across ≥2 modules, minus allowlist | protocol literals / i18n strings |
| — | ARCH-DEAD-CODE-1 | D | `resolver` graph+reachability | exported symbols unreachable from any entry point | a public package's exported API (externally referenced) |
| 2 | ARCH-SINGLE-SOURCE-LOGIC-1 | H | clone detector | clone families containing a decision (operator/literal/conditional); advisory judges "business decision?" + "copies agree?"; headline on disagreement | already-composed variants |
| 5 | ARCH-NAMING-ONE-CONCEPT-1 | H | clone detector + export overlap | near-identical bodies under different names; advisory judges "same concept?" | domain homonyms (same name, different concept) |
| 6 | UI-SHARED-PRIMITIVE-1 | H | clone detector (JSX/hook bodies) | repeated UI wiring; advisory judges "same mechanism?" | already-composed variants |
| 4 | UI-STATE-SHAPE-1 | J+D edge | tree-sitter query | `useState<bool>` pairs set in one handler; `useEffect` calling only a setter from props/state | independent booleans |
| 7 | STYLE-NEEDLESS-COMPLEXITY-1 | D | tree-sitter query (+ import graph for the call-site check) | negated ternaries; needless optional props | a ternary genuinely clearer un-negated |
| 8 | UI-REACT-HOOKS-LINT-1 | D (tool) | the eslint tier (exists) | corpus rule + the two tests; detector already green | — |

Determinism legend (source): **D** a pass decides, no model; **H** a deterministic pass proposes
candidates and the advisory tier re-ranks (the candidate list is never dropped); **J** judgment
with a deterministic edge.

### The advisory hook for the H rules
The clone/candidate list is deterministic; the "is this a business decision / same concept / same
mechanism" judgment is the advisory tier. Feed candidates to it as structured input (not whole
files), matching the existing audit pass. The candidate list is authoritative and never dropped;
the advisory only re-ranks and annotates (source §6).

## Domain home per rule (decided 2026-10-21; all existing, already-mapped domains)

Verified against `onboard/propose.rs`: `javascript` is reached by JS (direct) and TS (via the
`javascript:typescript` parent expansion); `ui` by React/Next/Vue/Angular/Svelte/Dioxus/Leptos;
`universal` is pipeline-selected and bound to every repo by id.

| rule | domain | why |
|------|--------|-----|
| ARCH-ONE-IMPORT-PATH-1 | `javascript` | ES-module re-exports/barrels are a JS/TS construct |
| ARCH-SHARED-CONSTANT-1 | `universal` | literal extraction spans ts/js/python/rust |
| ARCH-DEAD-CODE-1 | `universal` | the resolver graph spans ts/js/python/rust |
| ARCH-SINGLE-SOURCE-LOGIC-1 | `universal` | clone detector is per-dialect, rule is language-neutral |
| ARCH-NAMING-ONE-CONCEPT-1 | `universal` | same |
| UI-SHARED-PRIMITIVE-1 | `ui` | JSX/hook bodies |
| UI-STATE-SHAPE-1 | `ui` | React state hooks |
| UI-REACT-HOOKS-LINT-1 | `javascript:react` | sits with the other React rules; the eslint tier is React-scoped |
| STYLE-NEEDLESS-COMPLEXITY-1 | `javascript` | negated ternaries + needless optional TS props |

**Defect found while deciding this (fix with the first `ui` rule):** `domain_to_glob("ui")` is
`**/*.{tsx,css}`, which misses `.jsx` (and `.ts` hook modules). A plain-JS React repo's UI files
are out of scope for every `ui` rule today. Widen to `**/*.{jsx,tsx,ts,js,css}` or narrower with a
test; record the choice.

## Wiring per checker (the orphan-rule checklist)
Reconciled with `docs/plans/2026-10-21_w29-execution-plan.md` (the plan's version was closer to
right; this replaces the earlier "arch/ui/javascript all are mapped" shortcut). For each rule:
(1) the corpus TOML rule at `crates/rules/principles/<domain>/<id>.toml` using the domain from the
table above, passing every corpus gate (kind, inspectable, sources, W25 voice/no-vendor, W26
fix+citation if high+); (2) the `ArchChecker` impl in `crates/checks/src`; (3) register it in
`arch_check_runner`; (4) a proposed-rules test asserting the rule IS proposed for a matching stack
(the domain is already mapped, so no `domains_for_stack`/`domain_to_glob` edit is expected; the
test is what proves it rather than assuming it); (5) the positive + safe-twin tests (9 rules ⇒
≥18 tests).

**Protected area.** The work order (§91) says the rule corpus "always needs Zach's go". W32 had a
recorded go (D6); W29's nine new corpus rules did not have one as of 2026-10-21. Do not commit
corpus TOMLs until that go is recorded in `docs/OVERNIGHT_DECISIONS.md`. Detector code and its
tests may be built ahead of it.

## Build order (unchanged from the plan)
Wave A (deterministic, reuse-heavy): ONE-IMPORT-PATH, SHARED-CONSTANT, DEAD-CODE.
Wave B: UI-REACT-HOOKS-LINT (cheapest; detector exists).
Wave C: build the clone detector, then SINGLE-SOURCE-LOGIC, then NAMING + SHARED-PRIMITIVE.
Wave D: STATE-SHAPE, NEEDLESS-COMPLEXITY.

## False-positive traps — discovered while starting the build (2026-10-21)

Reading the actual code before writing it surfaced real correctness traps. Each deterministic
rule has a way to be WRONG that a naive implementation walks straight into; the guard is part of
the rule, not an afterthought. The standing law (from `extract/resolver.rs`'s own module doc):
**false-negative, never false-positive** — a consistency finding a client cannot immediately
verify as real is worse than a missed one.

- **ARCH-DEAD-CODE-1 — the reachability INVERSION trap (the big one).** (Superseded by the full
  build spec "ARCH-DEAD-CODE-1: the algorithm and its traps" below; kept as the origin of the
  guard.) `resolver::reachable_from`
  follows only RESOLVED import edges, and `build_import_graph` deliberately DROPS every edge it
  cannot resolve unambiguously (bare/external specifiers, dynamic `import()`, reflection, un-aliased
  TS paths, Rust 2015 unprefixed paths). Every OTHER checker benefits from that drop (it never
  flags on a dropped edge). Dead-code INVERTS the logic — it flags on UN-reachability — so a file
  reached ONLY via a dropped edge looks unreachable and would be falsely called dead. **Guard:**
  never flag a file dead on graph-unreachability alone. Require corroboration: (a) unreachable from
  every entry point (bin/lib/index roots + test files), AND (b) its module name/basename appears in
  NO other file's text at all (catches dynamic/unresolvable references), AND (c) it is not itself an
  entry point, a public-API/re-export surface, or a test. Err to false-negative. The safe twin — a
  public package's exported API — must stay silent because the lib/index root reaches it.

- **ARCH-ONE-IMPORT-PATH-1: the canonical-symbol algorithm and its traps.** (Rewritten
  2026-10-21. The first version keyed the group on the resolved `to_file`, but the rule's positive
  case, work order §7, is a symbol imported from BOTH its origin and a re-export. Those two
  imports resolve to DIFFERENT `to_file`s (origin vs re-exporting module), so grouping by
  `to_file` split exactly the case the rule exists for and never flagged it.)

  *Algorithm.*
  1. `build_import_graph` gives resolved edges. Extend `ResolvedImport` to carry `names` (today it
     drops them, `resolver.rs:43-49`); the `Import` already has them.
  2. Re-export map. For each `ImportKind::ReExport` edge `M -> O`: a named re-export maps
     `(M, exported_name) -> (O, origin_name)`. **Extractor change required:** `collect_export_from`
     (`ecma.rs`) records only the pre-`as` name, so `export { a as b } from './x'` is
     indistinguishable from `export { a }`. Record both names (origin and exported), or an aliased
     re-export will map the wrong symbol and produce a false positive. A star re-export
     (`export * from O`) maps `(M, n) -> (O, n)` only for names `n` that O itself exports (needs a
     new `extract::exports(lang, src)` listing a file's own named exports and `default`) AND that M
     does not declare itself AND that no other star source of M also exports (ambiguity → skip).
     `export * as ns` maps only `ns`, as a namespace; skip it.
  3. Canonicalize: follow the map to a fixpoint (visited-set cycle guard; a cycle → skip the
     symbol). Every named/default import `(F imports n from T)` becomes `(origin_file, origin_name)`
     plus the resolved `T` it was reached through.
  4. Group by `(origin_file, origin_name)`; collect the set of distinct `T`s. Flag when that set
     has ≥2 members after the exclusions below. The finding cites the re-export line(s) and every
     importer per path (work order: "flag the re-export and the split importers").

  *Traps and guards.*
  - **Relative-position spelling** (`./utils` from dir A vs `../a/utils` from dir B; or a tsconfig
    alias vs a relative path to the SAME file): both resolve to the same `T`, so the set has one
    member and nothing is flagged. Keying on resolved targets, not specifiers, is what makes this
    safe. Alias-vs-relative spelling is an import-style question, not this rule.
  - **Same name, different symbol**: grouping on `(origin_file, name)` keeps them apart.
  - **The barrel's own internals** (NEW): files inside a barrel's directory conventionally import
    siblings directly (importing through your own `index.ts` invites import cycles), while outside
    consumers use the barrel. That is the safe twin, not a split. **Guard:** an importer located
    inside the directory subtree of the re-exporting module does not count toward the set.
  - **Test files** (NEW): tests reach internals directly by design. Exclude test files from the
    importer set (false-negative bias; revisit if a client wants tests held to the same rule).
  - **Namespace imports** (`import * as ns from M`): which members are used is unknown without
    member-access analysis. Skip them; false negative, never a guess.
  - **Lone re-export with no split importers**: a non-index re-export that nobody yet imports both
    ways is not flagged in v1 (no verifiable split exists). If Zach wants the "a second path merely
    EXISTS" case flagged, it is a separate, lower-confidence finding; record that decision first.

- **ARCH-SHARED-CONSTANT-1 — the trivial/infrastructure-literal trap.** (Superseded by the full
  build spec "ARCH-SHARED-CONSTANT-1: the algorithm and its traps" below.) `0`, `1`, `""`, HTTP status
  codes, protocol strings, and i18n keys repeat everywhere legitimately. **Guard:** an allowlist of
  trivial/protocol/i18n shapes; only flag a DOMAIN value repeated across ≥2 modules.

- **The H rules (single-source / naming / shared-primitive) — the candidate-vs-finding line.** The
  clone detector's output is a CANDIDATE list, not findings. Presenting candidates directly would
  flag coincidental structural similarity as a defect. **Guard:** the advisory tier judges "same
  business decision / same concept / same mechanism"; only confirmed candidates become findings.
  The candidate list is never dropped (it is re-ranked), but it is never shipped unjudged.

**Build consequence:** each rule's positive test must be paired with a safe-twin test that exercises
its specific trap above (a dynamically-referenced file for dead-code; for one-import-path, a
relative-position variant, an aliased re-export, AND a barrel-internal sibling import; an
i18n/protocol literal for shared-constant; a coincidental clone for the H rules). That is what "correct to the QA boundary" means for this phase.

## ARCH-DEAD-CODE-1: the algorithm and its traps

Written 2026-10-21 against branch tip `6e1731c`, after reading `extract/{mod,ecma,python,rust_syn,
resolver}.rs`, `one_import_path_checker.rs` (and the review defects fixed in `daa2d90`),
`server/src/onboard/files.rs`, `camerata-check/src/lib.rs` and `arch_check_runner.rs`. Build-ready;
every "decide" below is decided. The governing law is the resolver's: **false-negative, never
false-positive**. A client reads each finding as a fact they can check with one `grep`; if they
cannot, the finding is wrong even when the file really is dead.

### 1. What is flagged

**Unit: a whole source file.** Not a symbol, not a branch, not a CSS class (see "Why file-level"
below). Languages: TypeScript/TSX/JavaScript (`.ts .tsx .js .jsx .mjs .cjs`), Python (`.py`), Rust
(`.rs`). Vue/Svelte/Astro single-file components, CSS and HTML are never flagged; they are read
only as *context* (they can keep other files alive).

The exact fact a finding states, per family:

- **ECMA / Python.** "No file in the repository imports `F`; `F` is not an entry point that a
  framework, a build tool, a manifest or a test loads by convention; and `F`'s name `<key>` appears
  nowhere else in the repository's text." A second form, for a file that *is* imported but only
  by files the same scan proved unreachable: "`F` is imported only by `A` (and `B`), which are
  themselves unreachable."
- **Rust.** "No `mod` declaration in the crate declares `F`, and no `#[path]` attribute,
  `include!` or Cargo target names it, so `cargo` never compiles it; its name appears nowhere else
  in the repository." (For Rust the first fact alone is already decisive: an undeclared `.rs` file
  under `src/` is not part of the build. The mention guard is kept anyway.)

**Why file-level, against the law.** The work order §1 asks for symbol-level ("N symbols in this
file are never used"). Symbol-level cannot meet the law with today's extractors: an exported
symbol with no *named* importer can still be live through `import * as ns` member access, through
`export *` chains consumed outside the repo, through Python attribute access on a module import
(`import a.b; a.b.sym`), through Rust path use without `use` (`crate::m::f()`), through framework
convention exports (`getServerSideProps`, `loader`, `load`, `GET`, `metadata`, `config`,
`handler`), through decorator registration (Flask/FastAPI/Celery/Click/pytest fixtures), through
JSX (`<Foo />`), through reflection and through `__all__` strings. Each of those is a finding the
client cannot verify in one step and several are outright wrong. The file-level fact has one
verification step (`grep -rn <key>`), one framework-convention surface (paths, enumerated in G4
to G7 below) and a graph that already exists. Symbol-level is the v2 scope, listed under "Known
false-negative limits" with the guard set it would need, and is an owner question (§7).

### 2. The algorithm, step by step

**Inputs.** `repo.files` as the architectural tier receives them. Two feed facts matter and both
are wiring items, not checker code (see "Wiring"): (a) the scan must hand this checker
*reference text* it does not admit today (`.html .htm .vue .svelte .astro .md .mdx .css .scss
.less .ejs .hbs .pug .njk .j2 .jinja .jinja2 Makefile Procfile Justfile`), or the mention guard is
blind to `<script src>`, SFC imports and README run instructions; (b) when the scan truncated at
`HARD_CAP_FILES`, the checker must not run (a missing importer is a false dead file).

**Step 0. Shared helpers (hoist, do not copy).** Move `is_test_path` and `emitted_js_paths` out
of `one_import_path_checker.rs` into a new `extract::paths` module (`pub fn is_test_path(&str)
-> bool`, `pub fn emitted_js_paths(&[(String,String)]) -> HashSet<&str>`), and re-export them from
the one-import-path module so its tests are untouched. Add `resolver::reachable_from_all(graph,
starts: impl IntoIterator<Item=&str>) -> HashSet<String>` (one multi-source BFS; `reachable_from`
becomes the single-start wrapper).

**Step 1. Candidate set `C`.** A file is a candidate iff `lang_for_path(path)` is `Some` **and
none** of the exclusions G1 to G3 fires (test paths, declaration files, root-level and dot-dir
files, config-shaped basenames, emitted twins, generated markers, keep markers). Everything else in
`files`, source or not, is *context*: it can carry edges and mentions but is never flagged.

**Step 2. Import graph.** `G = resolver::build_import_graph(repo.files)` over *every* file
(tests and configs included; their edges keep things alive). Note for the implementer: literal
`import('./x')`, `require('./x')` and `importlib.import_module('x.y')` ARE edges in `G`
(`ecma.rs:292`, `python.rs:191`; `ImportKind::Dynamic`); only computed specifiers are dropped.
Type-only and side-effect imports are edges too and must stay edges here (a file used only for
its types is live).

**Step 3. Roots `R` (ECMA + Python).** `R` = every context file (step 1) ∪ every candidate for
which any root predicate fires:

- R1 **mentioned**: the mention guard G8 finds `key(F)` in another file.
- R2 **manifest entry**: `F` is the resolved target of a `package.json` `main`, `module`,
  `browser`, `types`, `typings`, `bin` (string or map values), or `exports` (every string leaf,
  recursively, conditions included) field, resolved with `EcmaResolver::resolve(package_json_path,
  "./" + value)` after stripping a leading `./`; or of a `pyproject.toml` `[project.scripts]` /
  `[project.entry-points.*]` / `[tool.poetry.scripts]` value (`pkg.mod:func` → `pkg/mod.py`, by
  suffix match exactly as `resolve_python` does, dropped if ambiguous). `serde_json` and `toml`
  are already dependencies; a malformed manifest parses to nothing, never an error.
- R3 **executable by itself**: first line starts with `#!`, or (Python) the text contains
  `__name__` followed within 40 bytes by `"__main__"`/`'__main__'`.
- R4 **public surface**: `F` is inside a package whose nearest ancestor `package.json` lacks
  `"private": true` AND has no `exports` field (every file is deep-importable from outside the
  repo); or inside a package whose `exports` contains a subpath pattern (`"./*"`, `"./lib/*"`) and
  `F` is under the directory the pattern's target names.
- R5 **framework / tool convention path** (G4 to G7).
- R6 **computed-load subtree** and **directory reference** (G9, G10).
- R7 **invisible entry**: a manifest entry (R2) that resolves to a path NOT in `files` (today:
  anything under a `bin/` directory, which the scan prunes) marks the whole package (the manifest's
  directory subtree) as root. The checker cannot see what that entry imports.

`Live = reachable_from_all(G, R)`; `Dead = C \ Live` restricted to ECMA and Python files.

**Step 4. Rust (separate pass; `mod` closure, not the import graph).** The import graph carries
only `use crate::/self::/super::` edges (`resolver.rs:201`); it has no `mod` edges because
`rust_syn::imports` does not extract `mod x;` at all. What makes a Rust file part of the build is
a `mod` declaration, so:

- New extractor `extract::rust_syn::mod_decls(source) -> Vec<ModDecl>` with
  ```
  pub struct ModDecl {
      pub name: String,              // `mod foo;` → "foo"
      pub path_attr: Option<String>, // `#[path = "x.rs"]` verbatim value, if any
      pub inline_parents: Vec<String>, // names of enclosing inline `mod a { .. }` blocks, outermost first
      pub line: usize,
  }
  ```
  Only declarations WITHOUT a body (`syn::ItemMod { content: None }`) are emitted; an inline
  block is not a file. `cfg` gating is ignored on purpose: a `#[cfg(test)] mod tests;` or
  `#[cfg(feature = "x")] mod x;` still declares the file, and "declared" is the whole criterion.
  Never panics (`syn::parse_file` `Err` → empty vec).
- Crate roots per `src_root` (from `resolver::rust_src_root`, exposed `pub(crate)`): `src/lib.rs`,
  `src/main.rs`, `build.rs` beside `src/`, every `.rs` directly in sibling `tests/`, `examples/`,
  `benches/` and every `main.rs` one level below them, plus any `path =` under `[lib]`,
  `[[bin]]`, `[[test]]`, `[[example]]`, `[[bench]]` in the nearest ancestor `Cargo.toml` (parsed
  with `toml`, `.ok()`).
- Resolution of `ModDecl` in file `F` with directory `D = dir(F)`: the owning directory is `D`
  when `F` is a crate root or named `mod.rs`, else `D/stem(F)`; append `inline_parents` as
  further segments. Without `path_attr`: try `<owning>/<name>.rs` then `<owning>/<name>/mod.rs`
  (first present wins). With `path_attr`: `normalize_join(D, path_attr)` when `inline_parents` is
  empty, else `normalize_join(<owning>, path_attr)` (the Rust reference's rule for inline blocks).
  A declaration that resolves to nothing in `files` is ignored.
- `Declared = closure from the crate roots over mod_decls`. `Dead_rs = { .rs candidates under this
  src_root (or under tests/ examples/ benches/) } \ Declared \ roots`, minus any file G8 finds
  mentioned, minus G1 to G3, G11, G12.
- **Skip the whole crate** (emit nothing for it) when: `src/` has neither `lib.rs` nor `main.rs`
  in `files` (roots invisible; today `src/bin/`-only crates are pruned); or the crate's
  `Cargo.toml` text contains `[[bin]]` or `src/bin` (a bin target may own `#[path]` declarations
  the checker cannot see); or `Cargo.toml` names `automod`, `mod_use` or `include_dir` as a
  dependency (macro-generated declarations).

**Step 5. Emit.** Sort `Dead` by path. For each dead file compute `importers_in_dead = { from_file
of edges into F } ∩ Dead` (always a subset of `Dead`; a live importer would have made `F` live).
Then:

- **Directory roll-up.** For each directory `D` (deepest first) where every candidate under `D`
  (recursively) is dead and the count is ≥ 2: emit ONE finding at `D` (`file` = first dead file
  under `D` by path, `line = 0`, `object = Some("D/")`) listing every file, and remove them from
  the per-file pass. Keeps an abandoned `src/legacy/` tree at one row, not forty.
- **Per-file finding** otherwise: `file = F`, `line = 1`, `object = Some(F)`.
- Severity `SEVERITY_MEDIUM` (the checker contract forbids `low`; the corpus `max_severity =
  "low"` clamps at export, exactly as ONE-IMPORT-PATH does).
- Stable sort by `(file, line, message)`.

### 3. Every false-positive trap found, each with its guard

Candidate exclusions (the file is never flagged):

- **G1 Tests, fixtures, mocks, stories, e2e.** `extract::paths::is_test_path` (the widened set
  from `daa2d90`: `tests test __tests__ testdata fixtures benches __mocks__ e2e cypress stories`
  segments; `.test .spec _test .cy .stories` stems), plus `conftest.py`, `pytest.ini`-adjacent
  `tests.py`, and any `.snap`. Test code is live by definition; a helper imported only by tests
  is test code, not dead (the test file is a root).
- **G2 Declaration files.** `*.d.ts`, `*.d.mts`, `*.d.cts`, `*.pyi`: consumed by the type checker
  through `include`/`typeRoots`/triple-slash, never imported.
- **G3 Root-level, dot-dir and config-shaped files.** Any file whose directory is `""`; any path
  with a segment starting with `.` (`.storybook/ .github/ .husky/ .vscode/`); any basename
  starting with `.` (`.eslintrc.js`); basenames matching `*.config.*`, `*.conf.*`, `*rc.{js,cjs,
  mjs,ts}`, `*.setup.*`, `setupTests.*`, `jest.setup.*`, `vitest.setup.*`, `test-setup.*`,
  `*.workspace.*`, `instrumentation.*`, `middleware.*`, `proxy.*`, `service-worker.*`, `sw.*`,
  `manifest.*`, `reportWebVitals.*`, `polyfills.*`, `preload.*`, `env.*`, `global.*`, `globals.*`,
  `register.*`, `vite-env.*`, `next-env.*`. These are loaded by tools by name.
- **G11 Generated markers.** Any of `@generated`, `Code generated`, `DO NOT EDIT`, `do not edit`,
  `auto-generated`, `automatically generated`, `AUTOGENERATED` in the first 10 lines. The scan
  already prunes `generated/`, `__generated__/` and the `.gen.*`/`.generated.*`/`.pb.*`/`_pb2.*`
  suffixes (`files.rs:103-153`); this catches the rest. Codegen output is regenerated, not deleted.
- **G12 Keep marker.** A comment in the first 10 lines matching `keep-file:` followed by any
  non-space text (`// keep-file: loaded by the plugin host via glob`). This is the work order's
  "comment the rule recognises". Neutral token, no product name (W25). The finding's fix sentence
  names it.
- **G17 Emitted twins.** `extract::paths::emitted_js_paths`: a committed `X.js` beside `X.ts`/
  `X.tsx` is build output, never a candidate and never a root/importer (`daa2d90`).

Root predicates (the file is a candidate but counts as alive):

- **G8 The mention guard (the master guard).** `key(F)` = basename minus the final language
  extension (`foo.service.ts` → `foo.service`; `useThing.ts` → `useThing`); for index-like
  basenames (`index.*`, `mod.rs`, `__init__.py`, `lib.rs`, `main.rs`, `mod.ts`) the parent
  directory name instead. `F` is mentioned iff `key(F)` occurs, case-insensitively, as a whole
  word (the characters before and after are not in `[A-Za-z0-9_$]`; the key itself may contain
  `.` and `-`) in the content of any OTHER file in `files`, of any type. Any literal import
  specifier mentions its target this way (`'@/lib/orders'`, `'./orders.js'`, `app.orders`,
  `crate::orders`), so every edge the resolver DROPS (bare specifier, `baseUrl`-only path, webpack
  or vite alias, `jsconfig` path, `package.json` `imports` map, NodeNext `.js` suffix, ambiguous
  Python absolute import, un-extended tsconfig, Rust 2015 path) still keeps its target alive.
  Short or generic keys are treated as mentioned without searching: `key.len() < 3` or `key` in
  the stoplist `index utils util types type helpers helper constants const config main app api
  lib core common shared base data model models service services test tests src client server
  store hooks hook routes route pages page components component schema errors error logger auth
  user users db handler handlers actions state styles theme setup`. The search is a boundary-
  checked substring scan over every file's content for each dead candidate only (the unreachable
  set is small), so cost is `O(|unreachable| × total bytes)`.
- **G4 Framework file-system routing** (per package: the nearest ancestor `package.json`
  directory `P`; detection is by dependency name in that manifest OR by config basename in `P`):
  - Next.js (`next` dep or `next.config.*`): every file under `P/pages/**` and `P/src/pages/**`;
    under `P/app/**` and `P/src/app/**` only the convention basenames `page layout loading error
    not-found route template default global-error sitemap robots opengraph-image twitter-image
    icon apple-icon manifest` (any extension) plus `middleware.* instrumentation.* proxy.*` at
    `P` or `P/src`.
  - Remix / React Router framework mode (`@remix-run/*` or `react-router` dep with `app/root.*`):
    `P/app/routes/**`, `P/app/root.*`, `P/app/entry.*`, `P/app/routes.*`.
  - SvelteKit (`@sveltejs/kit`): `P/src/routes/**`, `P/src/hooks.*`, `P/src/hooks/**`,
    `P/src/params/**`, `P/src/service-worker.*`, `P/src/lib/server/**` is NOT exempt (ordinary).
  - Nuxt (`nuxt` dep or `nuxt.config.*`): for the app dir `A` (`P` or `P/app` when
    `P/app/app.vue` exists): `A/pages A/layouts A/middleware A/plugins A/composables A/utils
    A/components A/stores A/server/api A/server/routes A/server/middleware A/server/plugins
    A/server/utils` subtrees, and `A/app.* A/error.*`.
  - Astro (`astro` dep): `P/src/pages/**`, `P/src/content/config.*`, `P/src/middleware.*`.
  - Expo Router (`expo-router` dep): `P/app/**` entirely.
  - Gatsby (`gatsby` dep): `P/src/pages/**`, `P/src/templates/**`, `P/gatsby-*.{js,ts}` (root
    anyway, G3).
  - Docusaurus (`@docusaurus/core`): `P/src/pages/**`, `P/src/theme/**` (swizzled components).
  - Vercel / Netlify / Cloudflare functions by folder: `P/api/**` when `vercel.json` exists in
    `P`; `P/netlify/functions/**`, `P/netlify/edge-functions/**`; `P/functions/**` when
    `wrangler.toml` or `firebase.json` exists in `P`.
  - Storybook: `.storybook/` is a dot-dir (G3); stories are test paths (G1).
  A framework the checker does not recognise is covered by nothing but G8; that is a known limit,
  not a trap, because a convention-loaded file in an unknown framework is still unmentioned and
  WOULD be flagged. Mitigation: a Vite/CRA/plain repo has no file-system routing; the list above
  is the set of routers in common use as of 2026-10. Record new ones as found.
- **G5 Config files consumed by tools.** G3 covers the basename shapes; additionally anything
  under `P/config/**`, `P/scripts/**`, `P/tools/**`, `P/bin/**` (when visible), `P/docs/**`,
  `P/examples/**`, `P/example/**`, `P/samples/**`, `P/benchmarks/**`, `P/migrations/**`,
  `P/seeds/**`, `P/db/**`, `P/prisma/**`, `P/supabase/**`, `P/.storybook/**`, `P/cypress/**`,
  `P/playwright/**`, `P/e2e/**`, `P/public/**`, `P/static/**`, `P/assets/**`. Scripts are run by
  hand; examples and docs are read by people; migrations are run by a migrator by glob.
- **G6 Python conventions.** Basenames anywhere: `__init__.py __main__.py conftest.py setup.py
  manage.py wsgi.py asgi.py settings.py urls.py celery.py tasks.py admin.py apps.py models.py
  forms.py serializers.py views.py tests.py checks.py locustfile.py noxfile.py fabfile.py
  gunicorn.conf.py conf.py`; directory segments `migrations management templatetags dags
  spiders commands alembic versions settings fixtures`. Django autodiscovers `admin apps models
  migrations management/commands templatetags tests`; Celery autodiscovers `tasks`; Airflow and
  Scrapy load `dags/` and `spiders/` by folder. A Django `signals.py` or `receivers.py` is NOT
  exempt: nothing loads it unless `apps.py` imports it, and an unimported one is a real,
  well-known bug (and `apps.py` importing it mentions it, G8).
- **G7 Rust targets and gates.** Covered structurally in step 4: `#[cfg]`-gated and feature-gated
  `mod` declarations declare; `src/bin/`, `tests/`, `examples/`, `benches/`, `build.rs` and
  `Cargo.toml` `path =` entries are roots; `#[path]` is followed; `include!`/`include_str!` and
  `#[cfg_attr(.., path = "..")]` are literal strings and so are mentions (G8). Crates whose roots
  the checker cannot see are skipped entirely.
- **G9 Computed dynamic loads.** New extractor `extract::computed_loads(lang, src) ->
  Vec<ComputedLoad { prefix: Option<String>, line }>` for: ECMA `import(<non-string>)`,
  `require(<non-string>)`, `require.resolve(<non-string>)`, `require.context(<string>, ..)`,
  `import.meta.glob(<string>)`, `import.meta.globEager(<string>)`; Python
  `importlib.import_module(<non-string>)`, `__import__(<non-string>)`, `import_module(<non-
  string>)`, `pkgutil.iter_modules(..)`, `pkgutil.walk_packages(..)`, `importlib.util.spec_from_
  file_location(..)`, `entry_points(..)`; Rust `include_dir!(<string>)`, `automod::dir!(<string>)`.
  `prefix` is the literal text before the first `${`, `+`, `*`, `{`, `[` or `?` in the first
  argument (a template literal's first quasi, a binary `+`'s leftmost string operand, a glob's
  literal head), `None` when the argument carries no literal at all. Guard: with a prefix that
  `normalize_join(dir(caller), prefix)` turns into a directory present in `files` (any file has
  that directory as a path prefix), every candidate under it is a root. With no prefix, every
  candidate under the caller's nearest `package.json`/`pyproject.toml`/`Cargo.toml` directory is a
  root (the loader could name anything in its package). Err to false-negative.
- **G10 Directory references by literal.** Every string literal (any file in `files`, via a
  cheap regex over quoted strings, no parse needed) that contains `/`, is the first argument of a
  loader-ish call (`require.context import.meta.glob glob globSync sync fg fastGlob readdir
  readdirSync opendir walk iter_modules walk_packages listdir glob rglob iterdir read_dir
  include_dir dir`) and whose literal head (G9's rule) names a directory in `files` marks every
  candidate under that directory as a root. Tailwind `content` globs, tsconfig `include` and
  similar *scan* lists are deliberately NOT honoured (they would mark every file in `src/` alive
  and silence the rule); only loader calls are.
- **G13 Barrels and `export *`.** `ImportKind::ReExport` edges are ordinary edges in `G`; a file
  reached only through `export * from './x'` in a live barrel is live.
- **G14 Type-only imports** keep the target alive (`import type { T } from './types'` is an edge;
  do not filter on `type_only` here, unlike ONE-IMPORT-PATH).
- **G15 Side-effect imports** (`import './polyfill'`) are edges.
- **G16 NodeNext `.js` specifiers for `.ts` sources** resolve only when the `.js` exists
  (`resolver.rs` limit); G8 covers (`'./foo.js'` mentions `foo`).
- **G18 Aliases the resolver does not know** (`baseUrl`-relative bare paths, webpack/vite
  `resolve.alias`, `jsconfig.json`, `package.json` `imports`, tsconfig `extends` chains):
  dropped edges, G8 covers.
- **G19 Vue / Svelte / Astro / MDX importers.** Their `import` lines are invisible to the graph
  (`lang_for_path` → `None`), so a composable imported only from `Foo.vue` would look dead. G8
  covers once the files are in `files`, which is the wiring precondition (a) above. Until that
  wiring lands, the checker MUST NOT run on a package whose manifest depends on `vue`, `svelte`,
  `astro`, `@mdx-js/*`, `nuxt`, `@sveltejs/kit` (detect by manifest text; skip the package).
- **G20 Truncated scans.** `ExtractedRepo.truncated` is not visible to a checker; wiring item.
- **G21 Python `__init__.py` re-export surfaces.** `from .foo import Foo` and `from . import foo`
  inside `__init__.py` are edges (`resolve_python` handles both); `__all__ = ['foo']` is a mention.
  Namespace packages (no `__init__.py`) need nothing special.
- **G22 Workers, URLs and HTML.** `new Worker(new URL('./worker.ts', import.meta.url))`,
  `<script src="/src/main.tsx">`, `importScripts('sw-helpers.js')`: all literal mentions (G8),
  given wiring (a).
- **G23 Public packages consumed outside the repo** (work order safe twin): R4.
- **G24 Monorepo bare-name imports** (`import { Button } from '@acme/ui'`): the target package's
  entry is a manifest root (R2) and its internals are reached from it through `G`.
- **G25 Plugin registries by naming convention** (every `*.plugin.ts` under `plugins/` loaded by a
  glob): G9/G10 when the loader is in the repo; R4 when the loader is an external tool reading
  this package; otherwise G8 catches a registry file that lists them. A convention with none of
  those is the residual risk this rule accepts, bounded by `keep-file:` (G12).
- **G26 Short and homonymous names.** `a/orders.ts` (dead) and `b/orders.ts` (live): any
  `'./orders'` mentions both keys, so the dead one is silently kept. Correct direction.
- **G27 Two-way dead cycles.** `A ↔ B`, neither reachable: both dead, each listed as imported only
  by the other. The BFS visited-set handles the cycle.
- **G28 Size cap.** A `>400 KB` importer is dropped by the scan (`MAX_FILE_BYTES`), taking its
  edges with it. Residual, accepted: such files are generated or vendored bundles that do not
  import hand-written modules; a bundle that does is also prunable by the noise suffixes.
- **G29 Same-stem self-mention.** A file mentioning its own name in a comment does not count;
  "other files" only.
- **G30 Layer-2 versus scan feeds differ.** `camerata-check` loads by the checker's globs with
  `multilang::PRUNED_DIRS` (13 names, includes `bin`); the scan uses `NOISE_DIRS` (46 names, also
  `bin`, `out`, `coverage`, `generated`, `vendor`...). The checker must not assume either; every
  guard above reads only what is in `files`.

### 4. Tests (positive and safe twins), by name

Helper: `files(vec![(path, content)])` and `run(&f)` as in `one_import_path_checker.rs`; add
`run_truncated` only if the truncated flag is modelled in `RepoView` (it is not; see wiring).

Positive:
- `ts_file_with_no_importer_and_no_mention_is_flagged`: `src/app.ts` (`import './used';`),
  `src/used.ts`, `src/orphan.ts` (`export const x = 1;`), `package.json` (`{"private":true,
  "main":"src/app.ts"}`). Expect 1 finding: `file = "src/orphan.ts"`, `line = 1`,
  `object = Some("src/orphan.ts")`, message contains `orphan`, `nowhere else`, `keep-file:`.
- `python_module_with_no_importer_and_no_mention_is_flagged`: `app/__init__.py`, `app/main.py`
  (`from . import api`), `app/api.py`, `app/stale.py`. Expect exactly `app/stale.py`.
- `rust_file_not_declared_by_any_mod_is_flagged`: `crates/x/Cargo.toml` (`[package]\nname="x"`),
  `crates/x/src/lib.rs` (`mod a;`), `crates/x/src/a.rs`, `crates/x/src/old.rs`. Expect exactly
  `crates/x/src/old.rs`; message contains `mod` and `cargo`.
- `rust_nested_inline_mod_declaration_is_resolved`: `src/lib.rs` (`mod a { mod b; }`),
  `src/a/b.rs`, `src/c.rs`. Expect only `src/c.rs`.
- `rust_path_attribute_declares_the_named_file`: `src/lib.rs` (`#[path = "impl/real.rs"] mod
  real;`), `src/impl/real.rs`, `src/other.rs`. Expect only `src/other.rs`.
- `rust_cfg_test_and_feature_gated_mods_are_declared`: `src/lib.rs` (`#[cfg(test)] mod tests;
  #[cfg(feature = "x")] mod x;`), `src/tests.rs`, `src/x.rs`, `src/dead.rs`. Expect only
  `src/dead.rs`.
- `file_reachable_only_through_dead_files_is_listed_with_its_importer`: `src/app.ts` (no
  imports), `src/legacy.ts` (`import './legacy-helper';`), `src/legacy-helper.ts`. Expect two
  findings or one roll-up; the helper's message contains `imported only by` and `src/legacy.ts`.
- `whole_dead_directory_rolls_up_to_one_finding`: `src/app.ts`, `src/old/index.ts` (`export *
  from './a'; export * from './b';`), `src/old/a.ts`, `src/old/b.ts`. Expect exactly 1 finding,
  `object = Some("src/old/")`, `line = 0`, message lists all three files.
- `dead_file_in_a_live_directory_is_reported_alone`: `src/lib/live.ts` imported by `src/app.ts`,
  `src/lib/dead.ts`. Expect 1 per-file finding, no roll-up.
- `findings_are_sorted_and_stable`: three dead files in reverse lexical order; assert order.

Safe twins (each expects zero findings unless stated):
- `twin_file_imported_only_by_a_test_is_live`: `src/helper.ts`, `src/helper.test.ts` (`import
  './helper'`), `src/app.ts`.
- `twin_test_fixture_and_mock_files_are_never_candidates`: `__mocks__/fs.ts`, `fixtures/a.ts`,
  `e2e/x.ts`, `stories/b.stories.tsx`, `cypress/support.ts`, `src/app.ts`.
- `twin_declaration_file_is_never_a_candidate`: `src/types/global.d.ts`, `src/app.ts`.
- `twin_root_level_and_dot_dir_files_are_never_candidates`: `tailwind.config.ts`, `.storybook/
  main.ts`, `.eslintrc.js`, `scripts/build.mjs`, `src/app.ts`.
- `twin_config_shaped_basenames_anywhere_are_never_candidates`: `src/jest.setup.ts`,
  `apps/web/vitest.config.ts`, `src/middleware.ts`, `src/instrumentation.ts`, `src/app.ts`.
- `twin_next_pages_and_app_router_conventions_are_live`: `package.json` (`{"dependencies":
  {"next":"15"}}`), `pages/about.tsx`, `pages/api/orders.ts`, `app/dashboard/page.tsx`,
  `app/dashboard/layout.tsx`, `app/api/orders/route.ts`, `middleware.ts`.
  Positive sibling `next_app_dir_non_convention_file_is_still_a_candidate`: add
  `app/dashboard/_old.tsx` → exactly that one finding.
- `twin_remix_routes_root_and_entry_are_live`: `package.json` (`@remix-run/react`),
  `app/root.tsx`, `app/routes/_index.tsx`, `app/entry.client.tsx`.
- `twin_sveltekit_routes_and_hooks_are_live`: `package.json` (`@sveltejs/kit`),
  `src/routes/+page.ts`, `src/routes/api/+server.ts`, `src/hooks.server.ts`, `src/params/id.ts`.
- `twin_nuxt_autoload_dirs_are_live`: `nuxt.config.ts`, `composables/useThing.ts`,
  `components/Thing.ts`, `server/api/orders.ts`, `middleware/auth.ts`, `plugins/x.ts`.
- `twin_expo_router_app_dir_is_live`: `package.json` (`expo-router`), `app/(tabs)/index.tsx`.
- `twin_netlify_and_vercel_function_folders_are_live`: `vercel.json` + `api/hello.ts`;
  `netlify/functions/hello.ts`.
- `twin_package_json_main_bin_and_exports_targets_are_live`: `package.json` (`{"private":true,
  "main":"src/server.ts","bin":{"x":"src/cli.ts"},"exports":{".":{"import":"./src/index.ts"},
  "./util":"./src/util/index.ts"}}`), those four files, each importing nothing. Zero findings.
- `twin_exports_subpath_pattern_keeps_the_target_dir_live`: `package.json` (`"exports":{"./*":
  "./src/*.ts"}`), `src/anything.ts`.
- `twin_public_package_without_exports_is_deep_importable`: `package.json` (`{"name":"@acme/
  lib","main":"src/index.ts"}`, no `private`), `src/index.ts`, `src/deep.ts`. Zero.
  Positive sibling `private_package_without_exports_still_flags_unreached_file`: same with
  `"private": true` → `src/deep.ts` flagged.
- `twin_invisible_manifest_entry_disables_the_package`: `package.json` (`"bin":"bin/cli.js"`,
  `bin/cli.js` absent from `files`), `src/cli-impl.ts`. Zero.
- `twin_pyproject_script_entry_is_live`: `pyproject.toml` (`[project.scripts]\nx = "pkg.cli:
  main"`), `pkg/__init__.py`, `pkg/cli.py`.
- `twin_python_main_guard_and_shebang_are_entry_points`: `tools/run.py` (`if __name__ ==
  "__main__":`), `jobs/nightly.py` (first line `#!/usr/bin/env python3`).
- `twin_django_autodiscovered_modules_are_live`: `shop/models.py`, `shop/admin.py`,
  `shop/apps.py`, `shop/migrations/0001_initial.py`, `shop/management/commands/seed.py`,
  `shop/templatetags/money.py`, `shop/tasks.py`, `config/settings/prod.py`, `config/urls.py`.
  Positive sibling `django_signals_module_nobody_imports_is_flagged`: add `shop/signals.py` with
  no importer and no mention → one finding.
- `twin_python_dunder_all_string_is_a_mention`: `pkg/__init__.py` (`__all__ = ["legacy"]`),
  `pkg/legacy.py`.
- `twin_mention_in_manifest_or_ci_keeps_file_live`: `.github/workflows/ci.yml` (`run: node
  src/migrate-users.ts`), `src/migrate-users.ts`; `Dockerfile` (`CMD ["node","src/worker.js"]`),
  `src/worker.js`; `serverless.yml` (`handler: src/handlers/orders.handler`), `src/handlers/
  orders.ts`.
- `twin_html_script_tag_is_a_mention`: `index.html` (`<script type="module" src="/src/
  bootstrap.tsx">`), `src/bootstrap.tsx`. (Requires wiring (a); the unit test feeds the HTML
  directly.)
- `twin_vue_sfc_import_is_a_mention`: `src/App.vue` (`import { useThing } from './composables/
  useThing'`), `src/composables/useThing.ts`.
- `twin_vue_package_is_skipped_when_sfcs_are_not_fed`: `package.json` (`"vue":"3"`), no `.vue`
  in `files`, `src/composables/useThing.ts`. Zero (G19 skip).
- `twin_unresolvable_alias_import_is_a_mention`: `webpack.config.js` (`alias: {"@lib": path.
  resolve("src/lib")}`), `src/app.ts` (`import { x } from '@lib/orders'`), `src/lib/orders.ts`.
- `twin_nodenext_js_suffix_import_is_a_mention`: `src/app.ts` (`import './orders.js'`),
  `src/orders.ts`.
- `twin_python_ambiguous_absolute_import_is_a_mention`: `app/main.py` (`import services.user`),
  `app/services/user.py`, `vendor_copy/services/user.py` (the resolver drops this edge,
  `resolver.rs:843`); zero findings.
- `twin_literal_dynamic_import_is_an_edge`: `src/app.ts` (`const m = await import('./lazy')`),
  `src/lazy.ts`.
- `twin_computed_import_with_literal_prefix_keeps_that_directory_live`: `src/i18n.ts`
  (``import(`./locales/${lang}`)``), `src/locales/en.ts`, `src/locales/fr.ts`, and `src/other.ts`
  which IS flagged (prefix scope only).
- `twin_computed_require_without_prefix_keeps_the_package_live`: `packages/host/src/load.ts`
  (`require(name)`), `packages/host/src/plugin-a.ts`; `packages/other/src/dead.ts` IS flagged.
- `twin_require_context_and_import_meta_glob_keep_directory_live`: `src/registry.ts`
  (`import.meta.glob('./plugins/*.ts')`), `src/plugins/a.ts`; `src/icons.ts`
  (`require.context('./icons', true, /\.svg$/)`), `src/icons/x.ts`.
- `twin_tailwind_content_glob_is_not_a_loader`: `tailwind.config.js` (`content: ["./src/**/*.
  tsx"]`), `src/dead.tsx` → still flagged (G10 ignores scan globs).
- `twin_python_pkgutil_walk_keeps_package_live`: `app/plugins/__init__.py`
  (`pkgutil.iter_modules(__path__)`), `app/plugins/foo.py`.
- `twin_generated_marker_file_is_never_a_candidate`: `src/schema.ts` (first line `// @generated
  by codegen`), nothing imports it.
- `twin_keep_file_marker_is_honoured`: `src/plugin.ts` (`// keep-file: loaded by name from the
  host app`).
- `twin_emitted_js_beside_ts_is_ignored`: `src/a.ts` (dead), `src/a.js` (emitted copy). Expect
  exactly one finding, for `src/a.ts`, and `src/a.js` never named.
- `twin_type_only_import_keeps_types_file_live`: `src/app.ts` (`import type { T } from
  './types/order'`), `src/types/order.ts`.
- `twin_side_effect_import_keeps_file_live`: `src/app.ts` (`import './polyfills/intl'`),
  `src/polyfills/intl.ts`.
- `twin_star_re_export_chain_keeps_origin_live`: `src/app.ts` (`import { a } from './lib'`),
  `src/lib/index.ts` (`export * from './a'`), `src/lib/a.ts`.
- `twin_monorepo_bare_import_reaches_sibling_package_internals`: `packages/ui/package.json`
  (`{"name":"@acme/ui","main":"src/index.ts","private":true}`), `packages/ui/src/index.ts`
  (`export * from './button'`), `packages/ui/src/button.ts`, `packages/app/src/a.ts`
  (`import { Button } from '@acme/ui'`).
- `twin_short_or_stoplisted_key_is_never_flagged`: `src/db.ts`, `src/utils.ts`, `src/x.ts`, none
  imported. Zero.
- `twin_homonymous_stem_elsewhere_keeps_file_live`: `a/orders.ts` (unreferenced), `b/orders.ts`,
  `b/app.ts` (`import './orders'`). Zero.
- `twin_rust_crate_without_visible_root_is_skipped`: `crates/x/src/a.rs`, `crates/x/src/b.rs`,
  no `lib.rs`/`main.rs`. Zero.
- `twin_rust_crate_with_bin_targets_is_skipped`: `Cargo.toml` with `[[bin]]`, `src/lib.rs`
  (`mod a;`), `src/a.rs`, `src/shared.rs`. Zero.
- `twin_rust_tests_examples_benches_and_build_rs_are_roots`: `src/lib.rs`, `build.rs`,
  `tests/it.rs` (`mod common;`), `tests/common/mod.rs`, `examples/demo.rs`, `benches/b.rs`. Zero.
- `twin_rust_include_macro_is_a_mention`: `src/lib.rs` (`include!("generated_tables.rs");`),
  `src/generated_tables.rs`. Zero.
- `twin_two_way_dead_cycle_is_reported_once_each_without_looping`: `src/app.ts`, `src/a.ts` (`import
  './b'`), `src/b.ts` (`import './a'`). Expect a roll-up or two findings; test terminates.
- `twin_empty_and_malformed_inputs_never_panic`: empty `files`; a `package.json` of `{ not json`;
  a `Cargo.toml` of `[[[`; a 0-byte `.rs`; a `.ts` of binary noise. Zero findings, no panic.
- `twin_non_source_repo_has_no_candidates`: only `.java` and `.yml` files. Zero findings AND
  `config_unsatisfied_for(repo)` is `true` (see wiring).

### 5. Severity, confidence, domain, message, corpus TOML

- **Severity.** Emit `SEVERITY_MEDIUM`; corpus `max_severity = "low"` clamps the export (the
  ONE-IMPORT-PATH mechanism, `report_export::clamp_to_rule_max_severity`). The work order's
  "medium when the dead path still runs side effects" is not decidable deterministically at file
  level and is not attempted; every finding is low.
- **Confidence.** One tier. Every finding is a verified fact; nothing is `needs-review`
  (`advisory_coexisting` stays `false`).
- **Domain.** `universal` (table above). Pipeline-selected and bound to every repo, so the
  proposed-rules test is the negative one: it is proposed for a TS repo, a Python repo and a Rust
  repo, with `recommended` and `is_auto_recommended` true (a `policy` rule is auto-recommended).
- **`interest_globs`.** Source: `**/*.ts **/*.tsx **/*.js **/*.jsx **/*.mjs **/*.cjs **/*.py
  **/*.pyi **/*.rs`. Context: `**/*.json **/*.yaml **/*.yml **/*.toml **/*.html **/*.htm
  **/*.vue **/*.svelte **/*.astro **/*.md **/*.mdx **/*.css **/*.scss **/*.less **/*.sh
  **/*.txt **/*.cfg **/*.ini **/*.env **/.env* **/*.tf **/*.xml **/*.ejs **/*.hbs **/*.pug
  **/*.njk **/*.j2 **/*.jinja **/*.jinja2 **/*.sql **/Dockerfile* **/Makefile **/Procfile
  **/Justfile` (the glob matcher has no brace expansion; list each). Because context globs make
  `checker_applies` true for a repo with no candidate language, implement
  `config_unsatisfied_for(repo)` to return `true` when `files` has no `.ts/.tsx/.js/.jsx/.mjs/
  .cjs/.py/.rs` file: the ids then stay advisory and the ledger does not credit a run, so a Java
  repo never reads "dead code: clean".
- **Message shape** (client voice, no product names, no em-dashes; `<key>` is the mention key):
  - Per file: "`src/lib/old-helper.ts` is not imported by any file in the repository, is not an
    entry point that a framework, build tool, manifest or test loads by convention, and its name
    `old-helper` appears nowhere else in the repository. Delete the file. If it is loaded in a way
    the repository does not show, say how in a `keep-file:` comment in its first lines so the
    reason travels with the code."
  - Reached only through dead files: "`src/lib/b.ts` is imported only by `src/lib/a.ts`, which is
    itself unreachable (see that finding), and its name `b` appears nowhere else in the
    repository. Delete it with its importer, or keep both and document why with a `keep-file:`
    comment."
  - Directory roll-up: "Every source file under `src/legacy/` (14 files: `src/legacy/index.ts`,
    `src/legacy/a.ts`, ... and 11 more) is unreachable: nothing imports into the folder, no
    framework, build tool, manifest or test loads it, and its name appears nowhere else in the
    repository. Delete the folder, or document why it is kept."
  - Rust: "`src/old.rs` is not declared by any `mod` statement in its crate, and no `#[path]`
    attribute, `include!` or Cargo target names it, so `cargo` never compiles it. Its name `old`
    appears nowhere else in the repository. Delete the file, or add the `mod` declaration that was
    meant to bring it into the build."
- **Corpus TOML** `crates/rules/principles/universal/arch-dead-code-1.toml`: `id =
  "ARCH-DEAD-CODE-1"`, `version = 1`, `domain = "universal"`, `layer = "universal"`,
  `enforcement = "architectural"`, `kind = "defect"` (one correct state, one violating option;
  same shape as ONE-IMPORT-PATH), `inspectable = true`, `max_severity = "low"`, `default = true`,
  `verification = "policy"` with internal sources only (this design section; the work order §1;
  the owner go in `docs/OVERNIGHT_DECISIONS.md` 2026-10-21). No external authority states the
  three-language file-level rule as specified here; `knip` (unused files, JS/TS), `vulture`
  (Python) and `rustc`'s `dead_code` lint are related controls to name in a TOML comment, not
  `[[sources]]`, exactly as the ONE-IMPORT-PATH TOML names eslint-plugin-import. `qualifies` must
  state the file-level scope, the mention guard, the convention exemptions and the Rust `mod`
  criterion in client voice. Options: `every-source-file-is-reachable` (default; directive: every
  source file is imported by another file, is an entry point a framework, build tool, manifest or
  test loads, or carries a `keep-file:` comment saying how it is loaded) and
  `unreachable-files-are-kept` (`violates = true`; fault-first `why`). `remediation` names the
  delete, the `keep-file:` alternative and the verification (`grep` for the name, build and test
  pass). The TOML lands with the detector under the recorded go; `camerata corpus lock` after.

**Wiring (beyond the standard checklist).** (a) `files.rs`: add a `REFERENCE_TEXT_EXTS` tier
(`html htm vue svelte astro md mdx css scss less ejs hbs pug njk j2 jinja jinja2`) and basenames
(`Makefile Procfile Justfile`) admitted into a SEPARATE `ExtractedRepo.reference_files` list under
the same noise/size/UTF-8 rules, and pass `files ++ reference_files` to `audit_architectural`
ONLY (`onboard.rs:1600`); the AI digest (`ai_audit::build_digest`, which takes whatever slice it is
given and caps by characters) and the secrets floor keep today's input, so no model cost or prompt
displacement changes. A server test asserts `is_reference_text("index.html")`,
`is_reference_text("App.vue")`, and that `build_digest` input is unchanged. (b) `onboard.rs:1600`:
skip `ARCH-DEAD-CODE-1` (drop it from `repo_selected_ids` for that call) when
`ExtractedRepo.truncated` is true, and record the reason in the ledger as config-gated. (c) Fix
the `bin` prune (see "found wrong" in the report): `NOISE_DIRS`/`PRUNED_DIRS` should prune
`bin/Debug`, `bin/Release` and a `bin/` that is a sibling of `obj/`, not every `bin/`; until that
lands R7 covers the entry-point case. (d) Register in `all_checkers`; proposed-rules test;
positive + safe-twin tests above.

### 6. Known false-negative limits (stated plainly)

- Symbol-level dead exports, dead private functions, constant-guarded branches and unreferenced
  CSS classes (all in the work order's "looks at") are not detected. v2 scope.
- A file imported only by tests, stories or fixtures is treated as live.
- Any file under a recognised framework convention path, a `config/ scripts/ tools/ docs/
  examples/ migrations/ seeds/ db/ public/` folder, at the repo root or in a dot-dir is never
  flagged, even when genuinely dead.
- A file whose name is short (`< 3` chars) or on the stoplist is never flagged; a file whose stem
  coincides with any word anywhere in the repository (a README sentence, a YAML key, an unrelated
  module of the same name) is never flagged.
- A computed `require`/`import()` with no literal prefix silences the whole package; a Python
  `pkgutil`/`__import__` use silences its package.
- Packages that depend on Vue, Svelte, Astro, MDX, Nuxt or SvelteKit are skipped until
  reference-text wiring (a) lands; after it, their SFCs are context only, never candidates.
- Rust: crates with `[[bin]]` targets, `src/bin/`-only crates, crates using `automod`/
  `mod_use`/`include_dir`, and any file declared only by a macro the checker cannot expand are
  skipped or kept. `mod` declarations inside macro bodies are not seen.
- Files reachable only from an importer the scan dropped for size (`> 400 KB`) are flagged; the
  risk is accepted and bounded as described in G28.
- Unknown frameworks' convention-loaded files are the one residual true false-positive surface;
  bounded by `keep-file:` and the explicit router list, to be extended as repos are met.
- Cross-repository consumers of a private package (another repo importing by git URL or a
  published tarball) are invisible; a private package is treated as consumed only inside the repo.

### 7. Owner questions

1. **Scope: file-level now, symbol-level later? ANSWERED 2026-10-21.** Owner ruling (recorded in
   `docs/OVERNIGHT_DECISIONS.md`, DECISIONS MADE): ARCH-DEAD-CODE-1 ships FILE-level in v1. Unused
   individual exports are a separate future rule with its own id (the working name was
   ARCH-DEAD-EXPORT-1), built when the member-access analysis, framework-export allowlist,
   decorator awareness and identifier-absence guard listed above exist. The checker's module doc
   and the corpus TOML say so.

### 8. Built version: where it completes or departs from this spec (2026-10-21)

Implemented in `crates/checks/src/dead_code_checker/` (one module per concern) plus the scan
wiring in `crates/server/src/onboard/{files,architectural}.rs`. Every guard `G1` to `G30` has a
safe-twin test; the places the build chose something the text above left open or got wrong:

- **The mention guard counts only LIVE files, run to a fixed point.** As written, G8 makes the
  spec's own `file_reachable_only_through_dead_files_is_listed_with_its_importer` impossible: the
  dead importer's `import './legacy-helper'` mentions the helper, so the helper would be live. A
  mention inside a file that is itself unreachable proves nothing. A dead file whose name appears
  only in dead files is reported with a third message form ("its name appears only in `X`, which
  is itself unreachable"), so the message never claims "nowhere else" when a grep would find it.
- **Short-key rule versus the spec's test fixtures.** Keys under three characters are always
  mentioned, so fixtures named `a.ts`, `b.ts`, `c.rs` or `en.ts` can never be reported. The tests
  use longer names (`alpha.ts`, `cold.rs`, `german.ts`). The same applies to the stoplist
  (`user`, `api`, `app`): `twin_python_ambiguous_absolute_import_is_a_mention` uses `orders`.
- **Package manifests in the framework tests.** A package with no `"private": true` and no
  `exports` is public surface (R4) and roots every file, so the spec's Next, Remix, SvelteKit,
  Expo and similar fixtures (public manifests) pass for the wrong reason. The built tests mark
  them private. Framework dependencies that bring single-file components (G19) also root the
  package when no `.vue`/`.svelte`/`.astro`/`.mdx` file is in the scan, so those fixtures include
  one. `twin_computed_require_without_prefix_keeps_the_package_live` needs a `package.json` per
  package to scope the load; without one the nearest manifest is the repository root.
- **`twin_empty_and_malformed_inputs_never_panic`**: a `.ts` of binary noise that nothing imports is
  a genuine candidate, so it is the entry file (`src/app.ts`) in the built test.
- **R4 and G19 are scoped by NEAREST `package.json`**, so a nested private package is not made
  public by an outer public one. R7 and exports patterns root the manifest's whole subtree.
  Python `pyproject.toml` entries resolve through `resolver::resolve_python_module` (the import
  graph's suffix match, ambiguity dropped).
- **Extra test-file conventions (G1)**: `test_*.py`, `*.e2e.*`, `*.e2e-spec.*`, `__snapshots__/`
  and `tests.py` count as tests. Without them an unimported `test_x.py` is reported.
- **Roll-up** takes the OUTERMOST fully dead folder (shallowest first), never the repository
  root, and requires every SOURCE file under the folder (tests and exempt files included) to be a
  dead candidate, so "every source file under it is unreachable" is literally true.
- **Computed loads (G9)**: the directory a load can reach is its literal prefix cut at the last
  `/`. A prefix that is not relative (`@/plugins/`, `plugin-`), a missing prefix, and every Python
  load root the whole nearest-manifest package; a prefix naming a folder the scan lacks roots
  nothing. `require.context(dir)` and `include_dir!(dir)` get a trailing `/`. G10 reads a literal
  as written from the caller's folder, the nearest manifest folder and the repository root, and
  never roots a folder from a `.`/`./` cut.
- **Rust**: `src/bin/*.rs` and `src/bin/*/main.rs` are crate roots (the `bin` prune fix makes them
  visible), as is `[package] build`. The spec both skips a crate with `[[bin]]` and lists
  `[[bin]]` `path =` as roots; the skip wins (its twin test), so `[lib]`, `[[test]]`, `[[example]]`
  and `[[bench]]` paths are the Cargo roots read. An inline `mod a { }` carrying `#[path]` is not
  modelled (rare); the mention guard backstops. A file declared only by an uncompiled file is dead
  too, with a message naming the declarer.
- **Scan feeds**: `bin` is pruned only as .NET build output (`bin/Debug`, `bin/Release`) in both
  `NOISE_DIRS` and `PRUNED_DIRS`; this also fixed the secrets floor not reading `bin/` scripts.
  `reachable_from` is left unchanged (a new `reachable_from_all` sits beside it). A scan whose
  REFERENCE list hit its own cap is treated like a truncated one.
- **Layer 2 (open question)**: the rule runs at the write-time gate too (`NativeArchCheckRunner`
  runs any armed checker). A file an agent has just written and not yet wired in is reported,
  which may bounce multi-step work. Whether to mark the rule scan/CI-tier only is the owner's
  call; nothing here changes it.

## ARCH-SHARED-CONSTANT-1: the algorithm and its traps

Written 2026-10-21 against `6e1731c`. Same law, same precision bar as the two sections above.
The rule's value is "`X` is written as a bare literal in N files"; the whole difficulty is
deciding, deterministically, which `X` are *domain* values and which are the protocol, the
framework, the styling or the test data. The definition below is deliberately narrow. It is
allowed to miss most of what a human would call a domain literal; it is not allowed to call a
Tailwind class a domain constant.

### 1. What is flagged

**Unit: a literal value**, together with the set of its *bare* occurrences across files. One
finding per value per language family. Languages: TypeScript/TSX/JavaScript, Python, Rust; an
occurrence set never mixes families (a TS client and a Python server both writing `/api/v1/orders`
share a contract, not a constant, and "export one constant" is not a fix across languages).

The exact fact a finding states:
- Without a home: "`<value>` is written as a bare literal in N files: `a.ts:12`, `b.ts:40`,
  `c.ts:9`." (N ≥ 2, distinct files.)
- With a home: "`<value>` is defined once as the constant `NAME` in `home.ts:4`, but is written
  as a bare literal in N other files: ..." (N ≥ 1.)

"Bare" means the literal appears in code as itself, not as the initializer of a module-level
constant (the "home" shape, §2 step 4). The client verifies either sentence with one `grep` for
the quoted value.

### 2. The algorithm, step by step

**Step 1. New extractor `extract::literals(lang, src) -> Vec<Literal>`** (the "shared pass 1"
above), a sibling of `imports()` in `mod.rs`, dispatching to `ecma::literals`, `python::literals`
and `rust_syn::literals`. Exact output shape:

```
pub enum LiteralKind { Str, Number }

pub struct Literal {
    pub value: String,       // Str: unescaped content; Number: canonical text (see normalization)
    pub kind: LiteralKind,
    pub line: usize,
    pub ctx: LiteralCtx,
}

/// Where the literal sits. Every field is decided from the parent chain of the literal node
/// (tree-sitter) or from a context stack carried by the `syn` visitor (Rust). Never a guess:
/// a shape the walker does not recognise leaves the field at its default.
#[derive(Default)]
pub struct LiteralCtx {
    /// Source of `import`/`export ... from`/`require()`/`import()`/`from x import`/
    /// `import_module()`; also the first argument of `jest.mock`, `vi.mock`, `require.resolve`.
    pub import_specifier: bool,
    /// Last identifier of the nearest enclosing call's callee chain (`i18n.t(..)` → "t",
    /// `logging.getLogger(..)` → "getLogger", `cn(..)` → "cn"), walking up through arrays,
    /// objects, parentheses, spreads and template literals; `None` when no call encloses it.
    pub callee: Option<String>,
    /// The full callee text of that same call (`res.headers.set`, `os.environ.get`), so a
    /// guard can ask "is the receiver a headers object / the environment".
    pub callee_chain: Option<String>,
    /// For `subscript`: the receiver expression's text (`process.env`, `os.environ`, `row`).
    pub subscript_receiver: Option<String>,
    /// The literal IS a key of an object/dict literal (`{ 'a-b': 1 }`).
    pub object_key: bool,
    /// The literal is a VALUE in an object/dict literal: the key's text (identifier or string).
    pub value_of_key: Option<String>,
    /// `x['a-b']` / `x["a-b"]` subscript.
    pub subscript: bool,
    /// JSX/TSX attribute name when the literal is that attribute's value, directly or inside
    /// `{...}`; `None` otherwise.
    pub jsx_attr: Option<String>,
    /// Inside a TS union/literal type, enum member initializer, `as const` tuple used as a type
    /// source, Python `Literal[...]`, or a Python `Enum` class body assignment.
    pub type_position: bool,
    /// Inside a Rust `#[...]` attribute or a Python decorator expression.
    pub attribute: bool,
    /// Direct operand of a binary arithmetic expression (`* / + - %`) or compound assignment.
    pub arithmetic: bool,
    /// The literal is the whole initializer, or sits inside an object/array initializer reached
    /// without passing a call, of a TOP-LEVEL `const`/`let`/`var` (ECMA), a module-level
    /// `NAME = ...` or `NAME: Final = ...` (Python), or a `const`/`static` item (Rust).
    pub module_const: Option<ModuleConst>,
}

pub struct ModuleConst { pub name: String, pub exported: bool }
```

Rules per dialect: ECMA emits `string` nodes (unescaped via the existing `string_literal_text`
fragment logic, extended to concatenate `string_fragment` and `escape_sequence` children) and
`template_string` nodes with NO `template_substitution` child (value = the raw text); numbers from
`number` nodes. Python emits `string` nodes with no `interpolation` child and no `b` prefix
(`string_start` text), `integer`/`float` nodes; adjacent-string concatenation is skipped. Rust
(`syn`): `Lit::Str` (incl. raw strings; not byte strings), `Lit::Float`, `Lit::Int`, visited with
a context stack; **macro invocation bodies are not descended** (a token stream has no positions, so
no guard could apply) and attribute token streams ARE walked for `Lit::Str` with `attribute =
true`. Never panics.

**Number normalization.** Parse to `f64` (Rust/Python `_` separators removed, `.15` → `0.15`,
`0.150` → `0.15`); `value` is the shortest round-trip text (`ryu`-style `{}` formatting of the
`f64`). Only numbers that pass N1 below are ever indexed.

**Step 2. Candidate occurrences.** Walk every file with `lang_for_path` ∈ ECMA/Python/Rust that
is NOT excluded by F1 to F5 (§3). For each literal, apply the value-shape test (S1/S2/S3/N1) then
the value-level and occurrence-level exclusions (§3). A surviving occurrence is `(family, value,
file, line, home: Option<ModuleConst>)`.

**Step 3. Index.** `BTreeMap<(Family, String), Vec<Occurrence>>`. Then, per value, apply the
*value-level disqualifiers* (§3, "any occurrence anywhere in the family" rules: i18n, class name,
env var, type member, import specifier, header name, logger name, key/subscript). A disqualified
value is removed entirely.

**Step 4. Homes and bare copies.** `homes` = occurrences with `module_const.is_some()` whose
`name` is SCREAMING_SNAKE or PascalCase (a module-level `const ORDERS_QUEUE = 'orders-high'` or
`export const Queues = { Orders: 'orders-high' } as const`; a lower-camel `const url = '/api/x'`
inside a module is also a home when top-level, so accept any name). `bare` = the rest, collapsed
to distinct files, excluding files that hold a home.

**Step 5. Verdict.** Flag when `homes.is_empty() && distinct_bare_files ≥ 2`, or `!homes.is_empty()
&& distinct_bare_files ≥ 1`. `file`/`line` = the first bare occurrence by `(file, line)`;
`object = Some(value)`; severity `SEVERITY_MEDIUM` (clamped to low). Sort by `(file, line,
message)`.

### 3. Every false-positive trap found, each with its guard

**The domain definition (what qualifies at all).** A value qualifies only by passing exactly one
of these shapes; everything else is out of scope by construction:

- **S1 separated lowercase token** (queue, topic, channel, bucket, flag, cache-key prefix, custom
  element, analytics event): 4 to 64 characters; only `[a-z0-9]` and the separators `- : /`
  plus `.` inside a segment; split on `- : /` into ≥ 2 non-empty segments; at least one
  letter overall; no segment all digits when the separator is `:` (host:port); must not start or
  end with a separator. Matches `orders-high`, `user:signup`, `uploads/avatars`, `feature-new-
  checkout`, `svc-orders.v2`. Does not match `order_created` (snake only), `orders.created`
  (dot only), `Content-Type` (uppercase), `localhost:3000`, `2024-01-01`, `127.0.0.1`.
- **S2 route template**: `^/[a-z][a-z0-9_-]*(/([a-z][a-z0-9_-]*|:[a-z_][a-z0-9_]*|\{[a-z_]+\}|
  \[[a-z_]+\]|\*))+$`, so ≥ 2 segments. Matches `/api/v1/orders`, `/orders/:id`,
  `/users/{id}/avatar`. Does not match `/`, `/tmp/x.txt` (extension guard), `/usr/bin`.
- **S3 multi-word SCREAMING_SNAKE**: `^[A-Z][A-Z0-9]*(_[A-Z0-9]+)+$`, 5 to 64 chars. Matches
  `ORDER_CREATED`, `IN_PROGRESS`, `ADD_TODO`. Does not match `PENDING`, `GET`, `UTF8`.
- **N1 decimal rate**: a number with a fractional part, strictly in `(0, 1)`, not in `{0.5,
  0.25, 0.75}`, whose EVERY counted occurrence has `ctx.arithmetic == true`. Matches a `0.12`
  fee in `total * 0.12`. Does not match `opacity: 0.8`, `scale(0.95)`, `threshold = 0.5`.

Dropped on purpose from the work order's list, each a false-positive class with no deterministic
guard: dotted keys (i18n keys, lodash/yup/react-hook-form paths, module paths, logger names,
hostnames, versions all share the shape), snake_case strings (dict and JSON field names), single
words (`'pending'`, `'orders'`: table names, statuses, CSS keywords), byte-size integers (`1048576`
is also a buffer size, a timeout and a port in different files) and every other integer (time
units `60 1000 3600 86400`, status codes, ports, powers of two, `100`, `255`). Numeric v1 is N1
only.

**File-level exclusions (never a counted occurrence):**
- **F1 Test, fixture, mock, story, e2e paths**: `extract::paths::is_test_path` plus `conftest.py`,
  `seeds/`, `seed/`, `fixtures/` (test data, work order safe twin).
- **F2 Config-shaped files**: the DEAD-CODE G3 set (root-level files, dot-dirs, `*.config.*`,
  `*rc.*`, `*.setup.*`), plus `settings.py`, `settings/**`, `config/**`, `conf/**`, `env/**`,
  `environments/**` (Angular), `deploy/**`, `infra/**`, `terraform/**`, `k8s/**`, `helm/**`,
  `.github/**`. Per-environment config is SUPPOSED to repeat values across environments; a
  `'django.contrib.auth'` in `settings/dev.py` and `settings/prod.py` is not a missing constant.
- **F3 Migrations and generated code**: `migrations/ migration/ alembic/ versions/ generated/`
  segments; the generated-marker header (DEAD-CODE G11); emitted `.js` twins (G17);
  declaration files `.d.ts`.
- **F4 Scripts and docs**: `scripts/ tools/ docs/ examples/ samples/ benchmarks/` segments (one-off
  code that repeats an app value on purpose; the fix "import the constant" is often impossible
  from a standalone script).
- **F5 Storybook, Cypress, Playwright** folders (already G1/F1).

**Occurrence-level exclusions (this occurrence is not counted; the value survives):**
- **O1** `ctx.import_specifier` (`'react-dom/client'`, `'node:fs'`, `'lodash/fp'` are modules).
- **O2** `ctx.attribute && !S2` (Rust `#[serde(rename = "created-at")]`, `#[cfg(feature =
  "x-y")]`, Python `@pytest.mark.skipif`): wire-format names and build features are the
  protocol. A route-shaped string in an attribute (`#[get("/api/orders")]`, `@app.route('/api/
  orders')`) IS counted; that is a route definition.
- **O3** `ctx.object_key || ctx.subscript` (`{'x-request-id': v}`, `row['a-b']`): keys are the
  data's schema.
- **O4** `ctx.value_of_key` is a CSS property name (camelCase or kebab; the standard property
  list, ~190 names, as a `const` slice): `{ justifyContent: 'space-between' }` values are CSS.

**Value-level disqualifiers (one occurrence anywhere in the family removes the value):**
- **V1 i18n keys**: any occurrence with `callee` ∈ `{t, $t, i18n, i18next, translate, tr, gettext,
  ngettext, pgettext, _, __, formatMessage, defineMessage, useTranslation}`; or the value is
  found as a key in any JSON/YAML file under a `locales|i18n|lang|langs|translations|messages|
  intl` directory (cheap text scan for `"<value>"` followed by `:`).
- **V2 CSS classes and Tailwind**: any occurrence with `jsx_attr` ∈ `{className, class, classList,
  id, htmlFor, for}` or `callee` ∈ `{cn, cx, clsx, classnames, classNames, cva, tv, twMerge,
  twJoin, tw, css, styled, keyframes, createGlobalStyle, sx}`; or the value starts with a Tailwind
  variant or utility prefix (the list: `sm: md: lg: xl: 2xl: hover: focus: active: disabled:
  dark: group-hover: peer- first: last: odd: even: before: after: placeholder: print:` and
  `text- bg- border- p- px- py- pt- pb- pl- pr- m- mx- my- mt- mb- ml- mr- w- h- min-w- min-h-
  max-w- max-h- gap- space-x- space-y- rounded- font- items- justify- self- place- grid- col-
  row- flex- shadow- ring- opacity- z- top- right- bottom- left- inset- overflow- leading-
  tracking- transition- duration- ease- delay- animate- scale- rotate- translate- cursor- select-
  outline- divide- fill- stroke- object- aspect- basis- grow- shrink- order- whitespace- break-
  line-clamp- list- decoration- align- content- backdrop- blur- accent- caret- resize- scroll-
  snap- touch- will-change-`); or the value is a CSS keyword value or property name (the O4 list
  plus `flex-start flex-end space-between space-around space-evenly inline-block inline-flex
  border-box content-box sans-serif pre-wrap break-word ease-in ease-out ease-in-out
  row-reverse column-reverse no-repeat`).
- **V3 Environment variable names** (S3 shape): any occurrence with `callee` ∈ `{getenv, env,
  var, var_os, environ, option_env}`, or with `callee_chain` ending in `environ.get`, or with
  `subscript` and a `subscript_receiver` ending in `env` or `environ` (`process.env['X_Y']`,
  `os.environ['X_Y']`), or `value_of_key` equal to `env`.
- **V4 Typed enum members** (S3 and S1): any occurrence with `type_position` (a TS union `'IN_
  PROGRESS' | 'DONE'`, an `enum E { A = 'IN_PROGRESS' }`, Python `Literal['IN_PROGRESS']` or an
  `Enum` body). The type IS the home; repeating the literal at use sites is what a string union is
  for. (A Redux-style `'ADD_TODO'` used in an action creator and a reducer with NO union type
  stays flagged; that is the textbook case.)
- **V5 Import specifiers**: any occurrence with `import_specifier` (a path alias used both as an
  import and as a string elsewhere is a module name, not a constant).
- **V6 HTTP protocol tokens**: header names (any occurrence with `callee` ∈ `{setHeader, getHeader,
  header, headers}`, or with `callee` ∈ `{get, set, has, append, delete, insert}` and a
  `callee_chain` containing `header`), MIME types (`^(application|text|image|audio|video|multipart|font|message)/`),
  encodings (`utf-8 utf8 base64 base64url hex ascii latin1 binary ucs-2 utf-16le`), cache and
  fetch tokens (`no-cache no-store must-revalidate same-origin same-site cross-origin
  strict-origin strict-origin-when-cross-origin no-referrer unsafe-url include omit cors
  no-cors navigate force-cache only-if-cached`), auth schemes (`bearer basic digest`), a
  `^[a-z]{2}(-[a-z]{2,4})?$` locale, a semver `^v?\d+(\.\d+)*(-[a-z0-9.]+)?$`, a UUID, an
  `^(x-|sec-|if-|accept-|content-|access-control-|cache-|proxy-|www-)` prefix, and the standard
  header-name list (~70 lowercase names, `const` slice).
- **V7 Platform tokens**: `linux-x64 darwin-arm64 win32-x64 x86_64 aarch64 wasm32-unknown-
  unknown` and the `^(linux|darwin|win32|freebsd)-` prefix; `^(aria|data|xmlns|xlink)-` HTML
  attribute prefixes.
- **V8 Paths and files**: a value ending in `.<ext>` with `ext` in the scan's `CODE_EXTS` ∪
  `{html css scss svg png jpg jpeg gif webp ico pdf csv md txt xml lock map woff woff2 ttf}`,
  or starting with `./ ../ ~/ /tmp/ /usr/ /etc/ /var/ /opt/ /home/ /dev/ /proc/ /mnt/ /srv/`
  (a filesystem path, not a route), or containing `://`, `*`, `?`, `=`, `@`, `#`, `%`, `(`,
  whitespace or a control character.
- **V9 Logger names**: any occurrence with `callee` ∈ `{getLogger, get_logger, child, logger,
  span, instrument, trace_span, info_span, debug_span}`.
- **V10 Query-builder and ORM identifiers**: any occurrence with `callee` ∈ `{from, table,
  collection, model, rpc, channel, bucket, topic, queue}` is NOT excluded (that is the rule's
  target), but one with `callee` ∈ `{sql, raw, query, execute, prepare, select, insert, update,
  delete, where, orderBy, groupBy}` is (an SQL fragment or column path).
- **V11 Event names** (DOM and runtime): a `callee` ∈ `{addEventListener, removeEventListener,
  on, once, off, emit, process.on}` with a value in the DOM/Node event list (`click change
  input submit keydown keyup resize scroll load error exit SIGTERM SIGINT uncaughtException
  unhandledRejection message connection close data end`). Custom colon/kebab event names
  (`'order:created'`) stay in scope: they are channel names, the rule's own example.

**Position-independent traps and their guards:**
- **Cross-language repeats**: never grouped (family is part of the index key).
- **A value that is both a home and bare in the same file**: the home file's own bare occurrences
  are ignored (a module using its own constant's value next to it is not cross-module).
- **Two homes, zero bare copies** (`packages/web` and `packages/api` each `export const QUEUE =
  'orders-high'`): not flagged in v1 (see §6 and §7).
- **Two different values for "the same" fee** (`0.12` and `0.15`): not this rule; identical values
  only. The work order's "medium when the repeated values differ" is ARCH-SINGLE-SOURCE-LOGIC-1's
  job and is not attempted here.
- **The same literal twice in one file**: not flagged (one module, no sharing problem).
- **`as const` objects and TS `satisfies`**: `module_const` is set through object/array
  initializers, so `export const Routes = { orders: '/api/orders' } as const` is a home.
- **Rust macro bodies** (`format!`, `json!`, `sqlx::query!`, `info!`): not extracted (no
  positions), so a route written inside `json!({ "path": "/api/orders" })` is a false negative,
  never a mis-guarded positive.
- **Interpolated strings** (`\`/api/${v}\``, f-strings): skipped; their value is not a constant.
- **Concatenations** (`'orders-' + tier`): the `'orders-'` fragment ends in a separator and fails
  S1; skipped.
- **Tsconfig/vite/webpack alias strings in JS config files**: F2.
- **Django `INSTALLED_APPS`, `MIDDLEWARE`, Celery `task_routes`**: dotted (not S1) and in
  `settings` (F2); doubly out.
- **GraphQL operation names, SQL table names, enum string values in Prisma/SQL**: single words,
  out by shape.

### 4. Tests (positive and safe twins), by name

Positive:
- `kebab_queue_name_bare_in_two_files_is_flagged`: `src/api/orders.ts` (`publish('orders-high',
  msg)`), `src/worker/consume.ts` (`subscribe('orders-high', h)`). One finding, `object =
  Some("orders-high")`, `file = "src/api/orders.ts"`, message lists both `file:line` and contains
  `bare literal in 2 files` and `one home`.
- `colon_channel_name_across_three_files_lists_all_locations`: three files with
  `emit('user:signup')`/`on('user:signup')`; message names all three.
- `route_template_bare_in_client_and_server_is_flagged`: `src/client/api.ts` (`fetch('/api/v1/
  orders')`), `src/server/routes.ts` (`router.get('/api/v1/orders', h)`). One finding.
- `route_in_python_decorator_is_counted`: `app/orders.py` (`@app.route('/api/v1/orders')`),
  `app/client.py` (`client.get('/api/v1/orders')`). One finding: a route-shaped string inside a
  decorator counts (O2's exception).
- `route_in_rust_attribute_is_counted`: `src/a.rs` (`#[get("/api/v1/orders")] async fn a() {}`),
  `src/b.rs` (`client.get("/api/v1/orders")`). One finding.
- `screaming_snake_action_type_without_a_union_is_flagged`: `src/actions.ts` (`return { type:
  'ADD_TODO' }`), `src/reducer.ts` (`case 'ADD_TODO':`). One finding.
- `decimal_rate_in_arithmetic_in_two_files_is_flagged`: `src/cart.ts` (`total * 0.12`),
  `src/invoice.ts` (`subtotal * 0.12`). One finding, `object = Some("0.12")`.
- `number_spellings_normalize_to_one_value`: `.12` in one file, `0.120` in another (both
  arithmetic). One finding.
- `existing_home_with_one_bare_copy_elsewhere_is_flagged`: `src/queues.ts` (`export const
  ORDERS_QUEUE = 'orders-high'`), `src/worker.ts` (`subscribe('orders-high')`). One finding;
  message contains `ORDERS_QUEUE`, `src/queues.ts:1`, `1 other file`.
- `as_const_object_member_is_a_home`: `src/routes.ts` (`export const Routes = { orders: '/api/
  orders' } as const`), `src/a.ts` (`fetch('/api/orders')`). One finding naming `Routes`.
- `python_module_level_constant_is_a_home`: `app/const.py` (`ORDERS_QUEUE: Final = 'orders-
  high'`), `app/worker.py` (`q.subscribe('orders-high')`). One finding.
- `rust_const_item_is_a_home`: `src/cfg.rs` (`pub const BUCKET: &str = "user-uploads";`),
  `src/upload.rs` (`client.bucket("user-uploads")`). One finding.
- `findings_are_sorted_and_stable`: two values; assert `(file, line)` order.

Safe twins (zero findings unless stated):
- `twin_same_value_in_two_languages_is_not_grouped`: `src/api.ts` and `app/server.py` both bare
  `'/api/v1/orders'`.
- `twin_same_literal_twice_in_one_file_is_silent`.
- `twin_test_and_fixture_files_are_not_counted`: `src/a.ts` + `src/a.test.ts` + `fixtures/x.ts`.
- `twin_config_and_settings_files_are_not_counted`: `config/settings/dev.py` + `config/
  settings/prod.py` both `'cache-orders'`; `vite.config.ts` + `vitest.config.ts`.
- `twin_migrations_scripts_and_generated_are_not_counted`: `migrations/0001.py` + `migrations/
  0002.py`; `scripts/a.ts` + `scripts/b.ts`; a `@generated` file + one source file.
- `twin_i18n_key_via_t_call_is_disqualified_everywhere`: `src/a.tsx` (`t('errors-required')`),
  `src/b.tsx` (`label('errors-required')`): the `t` call disqualifies the value globally.
- `twin_i18n_key_present_in_locales_json_is_disqualified`: `locales/en.json`
  (`{"errors-required": "Required"}`), two source files using it bare.
- `twin_class_name_attribute_and_cn_call_are_disqualified`: `<div className="items-center">` in
  one file, `cn('items-center')` in another.
- `twin_tailwind_prefixed_token_is_disqualified_by_shape`: `'text-gray-500'` in two plain string
  positions.
- `twin_css_keyword_value_under_a_css_property_key_is_excluded`: `{ justifyContent: 'space-
  between' }` in two files.
- `twin_env_var_name_is_disqualified`: `process.env['ORDERS_QUEUE']` and `os.environ['ORDERS_
  QUEUE']` plus a bare `'ORDERS_QUEUE'` elsewhere in the same family.
- `twin_screaming_snake_member_of_a_union_type_is_disqualified`: `src/types.ts` (`type Status =
  'IN_PROGRESS' | 'DONE'`), `src/a.ts` + `src/b.ts` (`s === 'IN_PROGRESS'`).
- `twin_enum_initializer_is_disqualified`: `enum S { InProgress = 'IN_PROGRESS' }` + two bare uses.
- `twin_python_literal_and_enum_are_disqualified`: `Status = Literal['IN_PROGRESS']`; `class
  Status(Enum): IN_PROGRESS = 'IN_PROGRESS'`; two bare uses each.
- `twin_import_specifier_shaped_string_is_disqualified`: `import x from 'react-dom/client'` and
  `jest.mock('react-dom/client')`.
- `twin_mime_header_encoding_and_cache_tokens_are_excluded`: `'application/json'`,
  `'content-type'`, `'x-request-id'` via `setHeader`, `'utf-8'`, `'no-cache'`, `'same-origin'`,
  `'en-us'`, `'1.2.0-beta'` each in two files.
- `twin_host_port_date_ip_and_uuid_fail_the_shape`: `'localhost:3000'`, `'2024-01-01'`,
  `'127.0.0.1'`, `'0.0.0.0'`, a UUID, each in two files.
- `twin_filesystem_paths_and_urls_are_excluded`: `'./data/seed.json'`, `'/tmp/cache'`,
  `'config/default.yaml'`, `'https://api.example.com/v1'`, each in two files.
- `twin_object_keys_and_subscripts_are_not_counted`: `{ 'x-tenant': 1 }` and `row['x-tenant']` in
  two files.
- `twin_dotted_only_and_snake_only_strings_are_out_of_scope`: `'orders.created'` and
  `'order_created'` each in two files.
- `twin_single_word_strings_are_out_of_scope`: `'pending'`, `'orders'`, `'GET'` in two files.
- `twin_logger_name_is_disqualified`: `getLogger('svc-orders')` + `tag('svc-orders')`.
- `twin_dom_event_names_are_excluded_but_custom_channels_count`: `addEventListener('click')` x2
  (zero); `on('order:created')` + `emit('order:created')` (one finding).
- `twin_serde_rename_and_cfg_feature_attributes_are_excluded`: `#[serde(rename = "created-
  at")]` in two structs; `#[cfg(feature = "extra-fast")]` in two files.
- `twin_rust_macro_bodies_are_not_extracted`: `json!({"q": "orders-high"})` in two files.
- `twin_interpolated_and_concatenated_strings_are_skipped`: `` `/api/${v}` `` and
  `'orders-' + tier` in two files.
- `twin_decimal_outside_arithmetic_is_not_counted`: `opacity: 0.8` and `{ scale: 0.8 }` in two
  files; `0.5` in arithmetic in two files (excluded set).
- `twin_integers_and_time_units_are_out_of_scope`: `60`, `1000`, `86400`, `1048576`, `404`,
  `8080` each in arithmetic in two files.
- `twin_two_homes_and_no_bare_copies_is_silent`: two files each `export const Q = 'orders-high'`.
- `twin_home_file_own_bare_use_is_ignored`: `src/q.ts` (`export const Q = 'orders-high'; log(
  'orders-high')`), one other file bare → the finding counts 1 other file, not 2.
- `twin_sql_fragment_callees_are_excluded`: `query('orders-high')` in one file and
  `raw('orders-high')` in another → zero (V10).
- `twin_empty_and_malformed_inputs_never_panic`: empty files; binary noise with a `.ts` name; a
  `.rs` that does not parse. Zero, no panic.

### 5. Severity, confidence, domain, message, corpus TOML

- **Severity.** `SEVERITY_MEDIUM` emitted, corpus `max_severity = "low"` clamps to low. Always
  low: "medium when the repeated values differ" is undecidable here (see §3).
- **Confidence.** One tier, deterministic, never `needs-review`.
- **Domain.** `universal`; proposed-rules test for TS, Python and Rust repos.
- **`interest_globs`.** Source only: `**/*.ts **/*.tsx **/*.js **/*.jsx **/*.mjs **/*.cjs
  **/*.py **/*.rs`, plus `**/*.json **/*.yaml **/*.yml` under locale directories for V1 (listed
  as `**/locales/**/*.json` and the sibling names). `checker_applies` is then correct as-is.
- **Message shape** (client voice, no product names, no em-dashes):
  - No home: "`orders-high` is written as a bare literal in 3 files: `src/api/orders.ts:12`,
    `src/worker/consume.ts:40` and `src/jobs/retry.ts:9`. Give it one home: define it once as a
    named, exported constant and import that constant at every use, so that changing the value is
    one edit and a search for the constant's name finds every user."
  - With a home: "`orders-high` is defined once as the constant `ORDERS_QUEUE` in
    `src/config/queues.ts:4`, but is written as a bare literal in 2 other files:
    `src/worker/consume.ts:40` and `src/jobs/retry.ts:9`. Import the constant there instead of
    repeating the value."
  - `object = Some(value)`; the file list is complete (no "and N more"; a finding with 40 files
    is itself the point). Numbers render as their canonical text.
- **Corpus TOML** `crates/rules/principles/universal/arch-shared-constant-1.toml`: `id =
  "ARCH-SHARED-CONSTANT-1"`, `version = 1`, `domain = "universal"`, `layer = "universal"`,
  `enforcement = "architectural"`, `kind = "defect"`, `inspectable = true`, `max_severity =
  "low"`, `default = true`, `verification = "policy"` with internal sources (this section; the
  work order §3; the 2026-10-21 go). Related controls for the TOML comment, not `[[sources]]`:
  SonarSource S1192 ("string literals should not be duplicated", per file, threshold 3) and
  eslint `no-magic-numbers` (numbers only, per file); neither states the cross-file domain-value
  rule. `qualifies` must state the shapes (separated lowercase names, route templates,
  multi-word upper-case tokens, decimal rates in arithmetic), the one-language-family grouping,
  the home detection, and the exclusions (protocol tokens, i18n keys, class names, environment
  variable names, typed enum members, keys, paths, config, tests, migrations, scripts) in client
  voice. Options: `domain-literals-have-one-exported-home` (default) and
  `domain-literals-are-repeated-as-bare-values` (`violates = true`). `remediation`: define the
  constant once in the module that owns the concept, import it at every listed location, and
  verify by searching for the quoted value and finding only the definition.

**Wiring.** Standard checklist only: register in `all_checkers`, the proposed-rules test, the
tests above, the TOML under the recorded go, `camerata corpus lock`. No server change is needed
(source files only).

### 6. Known false-negative limits (stated plainly)

- Dotted keys, snake_case strings, single-word strings and every integer are never flagged, so
  `'orders.created'`, `'order_created'`, `'pending'`, `86400` and `10485760` repeated across
  modules are missed. Only the four shapes in §3 are in scope.
- Cross-language repeats are not findings.
- A value that already has two or more homes and no bare copies is not flagged.
- Values inside Rust macro bodies, template literals with substitutions, f-strings and string
  concatenations are invisible.
- Any value touched once by an i18n, class-name, env, logger or header call is excluded
  everywhere, even where the other uses are genuine domain uses.
- Values in test, config, settings, migration, script, docs and generated files are never
  counted, even when one of them is the only other copy.
- Decimal rates are flagged only when every copy is in arithmetic; a rate assigned to a variable
  first (`const fee = 0.12; total * fee`) is a home in that file and a bare copy only where it is
  also in arithmetic.
- The CSS, header, Tailwind, platform and event lists are finite; an unlisted protocol token of
  the right shape repeated across files would be flagged (the residual risk), bounded by the
  separator rule, the position guards and the file exclusions.

### 7. Open questions for the owner

1. **The narrow domain definition.** The work order describes domain literals by meaning (queue,
   channel, bucket, flag, fee, size limit, route). This spec replaces meaning with four
   deterministic shapes (separated lowercase names with `-`, `:` or `/`; route templates;
   multi-word UPPER_SNAKE tokens; decimal rates in arithmetic) and drops dotted keys, snake_case,
   single words and integers because no deterministic guard separates their domain uses from
   their protocol, schema and styling uses. That is a real narrowing: a repo whose queue names are
   `orders.created` gets no findings from this rule. Recommendation: ship the narrow rule; widen
   only per shape, each with its own safe twins, when a client repo shows a shape the rule misses.
2. **Two homes, no bare copies.** Two modules each defining their own constant with the same value
   is also "more than one home". It is verifiable, but in a monorepo two apps may define it
   separately on purpose because they cannot share a package. Recommendation: leave it out of v1
   and record the count internally; revisit if it turns out to be the common shape in client code.

## Scope note
Dead-code, one-import-path, shared-constant, state-shape, needless-complexity are **deterministic**
(no model, no token cost). Single-source/naming/shared-primitive add ONE shared clone pass and an
advisory re-rank. React-hooks reuses the existing tool tier. So the only substantial NEW
infrastructure is the clone detector + the literal extractor; everything else composes existing
`extract`/`resolver`/`ArchChecker`/eslint pieces.
