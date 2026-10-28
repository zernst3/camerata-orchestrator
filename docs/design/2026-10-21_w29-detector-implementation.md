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
(Superseded by the full build spec "Clone detector: the algorithm and its traps" and the three
rule sections that follow it, 2026-10-22; kept as the origin sketch.)
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
| 8 | UI-REACT-HOOKS-LINT-1 | D (tool) | the eslint tier (exists) for the violations, which the two `JAVASCRIPT-REACT-*` rules already own; a new config reader for the gap | amended 2026-10-22: the new rule owns only the lint-configuration gap (see "UI-REACT-HOOKS-LINT-1: the algorithm and its traps") | effects with complete deps, hooks at the top level; a config that enables the rules |

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

- **The H rules (single-source / naming / shared-primitive) — the candidate-vs-finding line.**
  (Superseded by the full build specs below, 2026-10-22; kept as the origin of the guard.) The
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
- **Layer 2: decided, not armed (orchestrator, 2026-10-21).** `ArchChecker::runs_at_write_time`
  (default `true`) is `false` for the dead-code checker, and `NativeArchCheckRunner` skips such
  checkers, so the write-time gate never reports ARCH-DEAD-CODE-1. A story may legitimately end
  with scaffolding that nothing imports yet, and the bounce would teach agents to paste
  `keep-file:` markers to make the rule go away. The scan and the CI binary still run it.
- **Review fixes (2026-10-21).** Tool folders `migrations`, `seeders`, `seeds` and `deploy` are
  never candidates at any depth; Rails importmap `pin_all_from` and an Ember `app/` folder are
  loaded by convention; `.mjs/.cjs/.mts/.cts` are audited code and generated code
  (`.gen.*`, `.generated.*`, `generated/`, `__generated__/`) is handed to the checker as reference
  text instead of dropped, since codegen is often the only importer; a folder holding a non-source
  file that live code still names is never rolled up; `instrumentation-client.*` and
  `mdx-components.*` are config-shaped; `spec/`, `__fixtures__/` and `*_spec` are test paths;
  ERB and other-language sources are in the interest globs; `bin/x64/Debug` and
  `bin/Any CPU/Release` are pruned as .NET output; liveness is one linear worklist; a main-guard
  window no longer splits a multibyte character; a rule withheld on a truncated scan carries
  its own ledger note ("the scan stopped at its file limit"), not the config-gated one.
- **Glob strings with a literal part (added 2026-10-21).** A quoted glob whose basename has a
  literal beyond the extension list (`'src/**/*.entity.ts'`, `'**/*.module.ts'`,
  `'**/handlers/*.ts'`) roots every file matching the whole pattern, resolved from the repository
  root and the nearest manifest folder; `tsconfig*.json`/`jsconfig*.json` are not read. Pure
  extension lists (Tailwind `content`, `include`) still root nothing.
- **Coverage note (recorded, not changed).** R7 roots a whole package when a manifest entry
  (`main`, `bin`, `exports`, `types`) names a file the scan cannot see, which includes any
  `main: "dist/index.js"` whose output is not committed. That makes the rule silent on many
  `npm init` style repositories. This is the safe direction (false negative) and is accepted for
  v1; a later version could resolve a missing `dist/` entry to its `src/` source.

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
    `src/queues.ts:4`, but is written as a bare literal in 2 other files:
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

### 7. Open questions for the owner: answered

Both questions were put to the owner and answered on 2026-10-21 (recorded in
`docs/OVERNIGHT_DECISIONS.md`, DECISIONS MADE). The rule is built to these rulings.

1. **The narrow domain definition: ship the narrow rule.** Only the four shapes in section 3 (a
   separated lowercase name with `-`, `:` or `/`; a route template; a multi-word UPPER_SNAKE token;
   a decimal rate in arithmetic) qualify. Dotted keys, snake_case strings, single words and
   integers stay out. A repo whose queue names are `orders.created` gets no findings from this
   rule. Widening happens later, one shape at a time, each with its own safe twins, when a client
   repo shows a shape the rule misses.
2. **Two homes, no bare copies: not a finding in v1.** Two modules each defining their own
   constant with the same value is verifiable, but in a monorepo two apps may define it separately
   on purpose because they cannot share a package. Leave it out; revisit if it turns out to be the
   common shape in client code. (Two homes WITH a bare copy elsewhere is flagged, with a message
   that names both homes; see "Build notes" below.)

### 8. Build notes: where the build clarified or amended the spec above

Written when the checker was built (`crates/checks/src/shared_constant_checker.rs`,
`extract::literals`). Each item is a place the text above was silent, ambiguous or wrong, and what
the build does.

- **A constant under `config/` cannot be a home.** The first draft of the message example in
  section 5 cited `src/config/queues.ts`, but F2 excludes `config/` at ANY depth (Angular's
  `src/environments/` shows the globs are not root-anchored), so a constant under any `config`
  directory is never read. The example now cites `src/queues.ts`. If a project keeps its constants
  under `config/`, the rule cannot see them.
- **N1's "every counted occurrence".** Read as: every BARE copy (after the home file's own copies
  are set aside) must be an operand of arithmetic; a module-level constant that holds the rate is
  exempt from the check. One bare copy that is not an operand (`opacity: 0.12`, `pick(0.12)`)
  removes the value everywhere. A CSS-property value (O4) is not counted, so it neither supports
  nor removes the value.
- **`ModuleConst.exported` is carried but unused.** Home detection accepts any top-level constant
  (spec step 4), and `const Q = 'x'; export { Q };` is exported without the declaration saying so,
  so the message never tells the client to "export" it. The field is there for a later rule.
- **Two or more homes plus a bare copy** is flagged (step 5 says `!homes.is_empty()`), but the
  "defined once as" sentence would be false, so the message names every home: "is defined as the
  constants `A` in `x:1` and `B` in `y:2`, but is also written as a bare literal in N other files".
  Homes are deduplicated by `(file, name)`: a constant that lists the value twice is one home.
- **V3 (environment names) applies to the SCREAMING_SNAKE shape only**, as its heading says;
  V4 (typed members) to the token and SCREAMING_SNAKE shapes, not routes.
- **ECMA decorators do not set `attribute`.** The ctx field is defined for Rust attributes and
  Python decorators only; a TypeScript `@Processor('orders-high')` is therefore counted as a bare
  use (it is the queue name). Revisit if decorators turn out to be noisy.
- **"`as const` tuple used as a type source"** is detected within the declaring file only
  (`typeof NAME` in a type in the same file). A tuple whose derived type lives in another file is
  not recognised and its members stay candidates.
- **The nearest-call walk** (ctx `callee`) also passes through ternaries, binary and `as`/
  `satisfies`/`!`/`await` wrappers, and `new` expressions and tagged templates count as calls. It
  stops at function boundaries and at a method call's receiver. The extra cases only ever add
  exclusions.
- **Python `//` counts as division** for N1, and `Literal[..]` is recognised both as a subscript
  and as an annotation's generic type.
- **The JavaScript grammar cannot parse JSX attributes named `class` or `for`** and recovers with
  an error node. A declarator the grammar had to recover around never counts as a home; TSX files
  parse these correctly.
- **Value lists collide with real names.** The Tailwind prefix list (`order-`, `list-`, `content-`,
  `select-`, `grid-`), the header prefixes (`cache-`, `content-`, `if-`, `x-`) and the locale rule
  mean that a queue called `order-events` or a key prefix called `cache-orders` is excluded by
  shape. The lists are the spec's; the cost is false negatives only.
- **Root-level, config-shaped and tool-loaded file names** (DEAD-CODE G3) are re-implemented
  privately in the checker, because the two checkers were built in parallel; hoist them into
  `extract::paths` when both have landed.

### 8a. Adversarial review round (2026-10-21): what changed and what was left

An adversarial review of the built checker found false positives; each is fixed with a failing
test first. The scope decisions below were made by the orchestrator (recorded in
`docs/OVERNIGHT_DECISIONS.md` on the orchestrator's branch) and are built as stated.

Fixes (case ids are the review harness's):

- **Test paths** (17, 18, 51, 98): `is_test_path` also matches the stems `tests`, `test` and
  `test_*` (Django's `tests.py`, pytest's `test_*.py`). Shared with ARCH-ONE-IMPORT-PATH-1.
- **Rust test code** (27, 56, 78): `#[cfg(test)]` modules and items, `#[test]`, `#[tokio::test]`
  and `#[rstest]` functions are not read (`cfg(not(test))` is read).
- **More home shapes** (19, 20, 21, 22, 23, 24, 25, 26, 74, 75, 84, 85, 92): `Object.freeze({..})`,
  `export default`, CommonJS `module.exports` / `exports.X`, TypeScript enum members (for every
  shape; an S1 or S3 member is still removed by V4, a route member is a home), namespace members,
  static fields of a module-level class, Python class-body attributes of a module-level class
  (`Class.ATTR`), Django `TextChoices` / `IntegerChoices` / `Choices` as enum bases, `LazyLock` /
  `Lazy` / `LazyCell` / `OnceLock::from` initializers, `lazy_static!` bodies (parsed as `static
  ref` items) and a Rust function whose whole body is the literal. A class body the walker cannot
  name (nested, or in a function) sets `LiteralCtx.unattributed` and is neither home nor bare
  (fail-closed).
- **Environment names** (11, 12, 13, 14, 15, 73): V3 matches when the callee chain's receiver ends
  with `env`, `environ` or `META`, for any verb, and `set_var`, `remove_var`, `setdefault`,
  `putenv`, `unsetenv` and `config` join the callees.
- **HTML attribute values** (01, 02, 03, 10, 65, 72, 88, 60): any occurrence that is the value of
  a JSX attribute, or of a key in a Django widget's `attrs={..}` dict, is not counted, except a
  route (`href`, `to`). `ctx.jsx_attr` carries the attribute name for both.
- **Recovered parses** (07): a literal under a tree-sitter error node is dropped, and a JavaScript
  file that does not parse cleanly is re-parsed with the TSX grammar (which handles `class=` and
  `for=`).
- **Type positions** (54): an occurrence in a type position is not counted unless it is itself a
  home (an enum member).
- **`classList`** (08, 71): V2 also applies when the callee chain contains `classList`.
- **String concatenation** (83, 31): `+` next to a string or template literal is not arithmetic.
- **Comments before the first argument** (76, 94, 57): the first named argument that is not a
  comment is the specifier.
- **Config-file homes** (58): a configuration file (F2) contributes the constants it defines but
  never a copy, so the message no longer says "Give it one home" when a home exists under
  `config/`. Tests, fixtures, migrations, scripts, docs, generated and declaration files
  contribute nothing, so a constant in a test file is not a home (97).

Scope decisions:

- **A. Rates need two significant digits.** A significant digit is a non-zero digit after the
  leading zeros of the fraction. `0.12` and `0.075` stay; `0.9`, `0.001` and `1e-9` are scales and
  epsilons (28, 29, 30).
- **B. URL names.** String arguments of `reverse`, `reverse_lazy`, `redirect` and `resolve_url`
  are not domain values (16). Applied to the token shape only, because a route passed to
  `res.redirect('/login')` is a route.
- **C. Grouping by package.** A value is compared only inside one package: the nearest directory
  with a `package.json`, `pyproject.toml` or `Cargo.toml` below the repository root, else the
  child of a conventional monorepo container (`apps`, `packages`, `services`, `libs`, `crates`,
  `modules`, `projects`), else the whole repository (46). The container rule is an extension so a
  monorepo without per-app manifests still separates; it only ever hides findings. The manifests
  join the checker's interest globs.
- **D. No change.** A router definition repeated in a `<Link to>` / `href` stays flagged (06, 82).

DOM lookups (09): string arguments of `getElementById`, `querySelector`, `querySelectorAll`,
`closest`, `matches`, `getElementsByClassName` and `getElementsByName` are not domain values, and
a `$()` / `jQuery()` first argument starting with `#`, `.` or `[` is a selector (such a string has
no domain shape, so that half is a backstop).

Cheap protocol tokens added from the review's plausible list: cipher and digest names (`aes-`,
`sha-`, `rsa-`, `hmac-`, ...), model identifiers of the common hosted families (`gpt-`, `claude-`,
`gemini-`, ...), Node `ERR_*` codes, date-format tokens (`hh:mm:ss`, `yyyy-mm-dd`), cloud regions
(`us-east-1`, `us-gov-west-1`), `get`/`set` on a `req`/`res`/`request`/`response`/`headers`
receiver as header verbs, and `start_as_current_span`, `startSpan`, `startActiveSpan`, `setTag`,
`set_tag`, `setAttribute`, `set_attribute` as logger-like callees.

Residual risk, left as is (all would be reported):

- Firebase and similar SDK error codes (`auth/user-not-found`, case 33), git subcommands passed to
  a subprocess (36), unlisted Tailwind-like classes in plain objects (37), Redux action types
  shared between a slice and a middleware (38), react-query keys (39), Kubernetes label keys (40),
  Celery task names (41), clap argument names (42), Rust match-arm strings against a variant name
  (43), GraphQL documents (45), vendor analytics events (47), Django `choices` tuples against a
  filter (48), Sentry tags beyond `setTag` (61), and `ab-1`-shaped
  values with a digit segment (95).
- A `typeof NAME` type derived in another file from an `as const` tuple (53): the tuple's members
  stay candidates, so a bare use elsewhere is reported against the tuple as its home.
- Python tuple-unpacking and `self.x = ...` assignments are not homes (59, 87).
- An ECMA decorator argument such as Nest's `@Processor('orders-high')` is counted (67).

## Clone detector: the algorithm and its traps

Written 2026-10-22 against branch tip `d8afd50`, after reading `extract/{mod,ecma,python,rust_syn,
paths,ancestry,enclosing}.rs`, `arch_checker.rs`, `onboard/{files,architectural}.rs`,
`onboard.rs` (`audit_repos`), `ai_audit.rs` (`verify_findings`, `conventions_observed_findings`,
`UsageMeter`, `FailedPass`), `scan_ledger.rs`, `scan_cache.rs`, `mechanical_gate.rs`, the
`LlmPort` seam and its test stubs, `model_registry.rs` and `fleet/src/tier.rs`. Build-ready; every
"decide" below is decided. Same law as the three built rules: **false-negative, never
false-positive.** The clone detector itself never produces a finding. It produces a CANDIDATE
INDEX that three rules read, and the only sentences that reach a client are facts the detector can
prove by token comparison.

### 1. API and types

New module `crates/checks/src/extract/clone.rs` (a sibling of `literals`; the extractors stay
in `camerata-checks`, which must not depend on the server or the model).

```
/// How a source token is normalized before hashing. See the table in section 2.
pub enum TokenMode {
    /// Identifier-insensitive: free identifiers, property names, literals and operator
    /// families are each collapsed to a class token, so `title ?? slug` and `name || id`
    /// hash the same. The mode the FAMILY search runs in.
    Structural,
    /// Free identifiers still collapse; property names, literal values and operator text are
    /// kept verbatim. Never used to find families; used to decide whether the members of a
    /// found family AGREE (ARCH-SINGLE-SOURCE-LOGIC-1).
    Exact,
}

pub enum TokenClass { Ident, Prop, JsxTag, JsxAttr, Str, Num, Lit, BinOp, UnOp, Assign, Keyword, Punct, Block }

pub struct Token {
    pub id: u32,          // interned text of the NORMALIZED token (per mode)
    pub class: TokenClass,
    pub line: u32,        // 1-based line of the source token
    pub raw: Range<u32>,  // byte range in the source, for Exact-mode diffs and messages
}

/// One top-level item's token sequence. Windows never cross an item boundary.
pub struct ItemTokens { pub item: u32, pub kind: &'static str, pub name: Option<String>,
                        pub start_line: u32, pub end_line: u32, pub tokens: Vec<Token> }

pub fn tokens(lang: SourceLang, source: &str, mode: TokenMode) -> Vec<ItemTokens>;

pub struct CodeSpan { pub file: String, pub item: u32, pub token_start: u32, pub token_len: u32,
                      pub start_line: u32, pub end_line: u32 }

pub struct CloneFamily {
    pub key: u64,                 // FNV-1a of the shared Structural token ids; stable across runs
    pub token_len: u32,           // length of the shared sequence (every member has exactly it)
    pub members: Vec<CodeSpan>,   // sorted by (file, start_line); >= 2 distinct files
    pub distinct_files: u32,
}

pub struct CloneOptions { pub min_tokens: u32 /* 40 */, pub max_members: u32 /* 50 */,
                          pub max_pairs: u32 /* 200_000 */, pub max_index_bytes: usize /* 64 MiB */ }

pub enum SkipReason { NotSource, TestPath, Generated, Vendored, Minified, I18nCatalog, TooLarge, ParseFailed }

pub struct CloneIndex {
    pub families: Vec<CloneFamily>,          // sorted by (key); deterministic
    pub files_read: u32,
    pub skipped: Vec<(String, SkipReason)>,
    pub boilerplate_buckets_skipped: u32,    // buckets over max_members (section 4, B1)
    pub truncated: bool,                     // a cap fired; rules treat as "not fully indexed"
}

pub fn detect(files: &[(String, String)], opts: &CloneOptions) -> CloneIndex;

pub struct Disagreement { pub position: u32, pub line_a: u32, pub line_b: u32, pub a: String, pub b: String }
/// Exact-mode comparison of two spans known to be Structural-equal. Empty when they agree.
pub fn exact_diff(a: &ItemTokens, a_span: &CodeSpan, b: &ItemTokens, b_span: &CodeSpan) -> Vec<Disagreement>;
```

`detect` is pure over `(path, content)` pairs, never panics (a file that fails to parse is
skipped with `ParseFailed`), and runs ONCE per repo for all three rules (section 7 explains the
single-checker shape). `min_tokens = 40` is the index floor; each rule applies its own, higher,
threshold on `token_len` (section 3).

### 2. Tokenization per dialect

The source parses already exist: tree-sitter for TS/TSX/JS (`ecma.rs`, grammar chosen by
`lang_for_path`, with the JS-to-TSX re-parse the literal extractor already does for `class=`
attributes) and Python (`python.rs`); `syn` for Rust. `ecma::parse` and `python::parse` become
`pub(crate)`. Rust is tokenized with `proc_macro2` (already a dependency with `span-locations`,
`Cargo.toml:32`), not `syn`: a token stream carries every macro body (`json!`, `format!`,
`sqlx::query!`) that a `syn` AST walk hides, and macro bodies are code the client wrote.

**Leaves.** For tree-sitter, the token stream is the pre-order sequence of leaf nodes (named
and anonymous), except: a `string`/`template_string` node is ONE token (its substitutions'
tokens are emitted inside it between a `${` and `}` pair); a `comment` is dropped; a Python
`block` emits a synthetic `{` on entry and `}` on exit (tree-sitter-python has no indent
leaves, and without these `if a:\n  return 1\nreturn 2` and `if a:\n  return 1\n  return 2`
would tokenize identically). For `proc_macro2`: `Ident`, `Punct` (joint puncts fused into one
operator: `==`, `&&`, `..=`, `->`), `Literal`, and `Group` as open-delimiter, contents,
close-delimiter. Doc comments arrive as `#[doc = "..."]` attributes and are dropped.

**Excluded items.** The following top-level items contribute NO tokens, so an import block or
a type-only file can never be a clone: ECMA `import_statement`, `export_statement` whose only
child is a re-export (`export ... from`), `interface_declaration`, `type_alias_declaration`,
`enum_declaration`, `declare` statements; Python `import_statement`, `import_from_statement`;
Rust `use`, `extern crate`, `mod x;`, `struct`/`enum`/`union`/`trait` items with no function
bodies, `type` aliases, `#![...]` inner attributes. An `export` / `export default` / `pub` /
`pub(crate)` prefix on a declaration is dropped (one token of visibility must not split a
family). Decorators and attributes are kept as tokens.

**Normalization table.** "Kept" means the token's own text is the interned string.

| Token | Structural | Exact |
|---|---|---|
| Free identifier (variable, parameter, function/class/type name, callee) | `$ID` | `$ID` |
| Property/field/attribute name after `.` or `?.` (`property_identifier`, Python `attribute.attribute`, Rust field after `.`, a method name after `.`) | `$PROP` | kept |
| JSX element tag name (`jsx_opening_element`/`jsx_self_closing_element` `name`) | kept | kept |
| JSX attribute name | kept | kept |
| String / template literal (fragments only; substitutions tokenized inside) | `$STR` | kept (unescaped value) |
| Number literal | `$NUM` | kept (`canonical_number`) |
| `true false null undefined None True False` | `$LIT` | kept |
| Binary operators (`+ - * / % ** == === != !== < > <= >= && \|\| ?? and or in is not instanceof \| & ^ << >> >>>`), `??=` etc. | one class token per family: `$ARITH`, `$CMP`, `$LOGIC`, `$BIT` | kept |
| Unary operators (`! - + ~ typeof void await not`) | `$UNOP` (`await`/`not` kept) | kept |
| Assignment operators (`= += -= ...`) | `$ASSIGN` | kept |
| Keywords and `this`/`self`/`super`/`cls` | kept | kept |
| Punctuation (`( ) { } [ ] , ; : . ?. => -> ...`) and the synthetic Python block braces | kept | kept |

Why property names collapse in Structural but not in Exact: the family search must land
`user.isPremium ? price * 0.2 : 0` and `account.premium ? amount * 0.2 : 0` in one family (a
renamed field is still the same decision shape), while the agree check must say that
`order.total * 0.12` and `order.subtotal * 0.12` apply the rate to different fields. Both are
facts a client can check by reading the two lines the finding cites. JSX tag and attribute names
stay kept in both modes because a `<div><span/></div>` matching a `<ul><li/></ul>` is a false
family with no deterministic defence.

**Positions.** Every token carries its line; a span's `start_line`/`end_line` are its first and
last token lines. Messages cite `file:start-end`.

### 3. Window size and thresholds, with the justification

- **Hash unit `K = 20` tokens**, fixed. A 20-token k-gram is about two statements; smaller
  k-grams collide constantly in an identifier-insensitive stream (`$ID ( $ID , $ID ) ;` is
  everywhere), larger ones miss short matches the winnowing guarantee below needs.
- **Index floor `min_tokens = 40`** (the winnowing guarantee length `T`). Every pair of spans
  sharing at least 40 consecutive Structural tokens is found (section 4). 40 tokens is roughly
  four to six lines of logic or one small JSX block; below that, structural coincidence between
  unrelated code is common and no rule here wants it.
- **Per-rule thresholds on `token_len`:** ARCH-SINGLE-SOURCE-LOGIC-1 and
  ARCH-NAMING-ONE-CONCEPT-1 require `token_len >= 60`; UI-SHARED-PRIMITIVE-1 requires `>= 40`.
  jscpd's default is 50 tokens with identifiers KEPT; this stream collapses identifiers, so the
  logic rules sit above jscpd's default on purpose. The UI rule sits at the floor because JSX is
  punctuation-dense (a spinner block is ~35 tokens, an outside-click effect ~60) and that rule
  carries a further structural requirement (a mechanism mark, section "UI-SHARED-PRIMITIVE-1").
- **Distinct token kinds >= 8** inside the shared sequence (a window of `$ID , $ID , $ID` has
  two kinds). Guards tiny-vocabulary clones that pass the length test.
- **Members must span >= 2 distinct files.** A sequence repeated only inside one file is not a
  sharing problem for any of the three rules (the SHARED-CONSTANT ruling). Same-file members of
  a cross-file family ARE listed.

### 4. Rolling hash, winnowing, verification, merging, determinism

**Phase 1, streaming fingerprints.** Files are processed in path order. For each file:
tokenize (Structural), then for each item compute the polynomial rolling hash of every
K-gram over interned ids (`h = sum id_k * B^(K-1-k) mod 2^64`, `B` a fixed odd constant; O(1)
per step). Winnow (Schleimer, Wilkerson, Aiken 2003): over each window of `W = T - K + 1 = 21`
consecutive k-gram hashes keep the minimum (rightmost on ties); any two spans sharing `T`
tokens share at least one selected fingerprint. Push `(hash u64, file u32, item u32,
token_index u32)` (16 bytes) and DISCARD the tokens. Expected density `2/(W+1)`, about 9% of
k-grams.

**Phase 2, buckets.** Sort fingerprints by hash. A bucket is a run of equal hashes. Drop
buckets whose fingerprints come from one file. Drop and count (`boilerplate_buckets_skipped`)
buckets with more than `max_members` (50) distinct files: a sequence present in more than fifty
files is a framework or house pattern, not a copy anybody made (B1 below). Every surviving
bucket yields the candidate pairs of its members (first by file order), subject to
`max_pairs`.

**Phase 3, verification and extension.** For each candidate pair `(A: file,item,i)`,
`(B: file,item,j)`: re-tokenize both files (an LRU of 256 tokenized files bounds this; a file
is tokenized at most once per eviction), compare the K tokens by id (a hash collision is
rejected here), then extend left and right while tokens are equal and both cursors stay inside
their items. The result is a maximal matched pair with length `L >= K`; keep it when `L >=
min_tokens`. A pair whose spans overlap each other (same file, same item, ranges intersect) is
discarded. Each maximal pair is recorded once per `(A span, B span)`; later fingerprints inside
an already-recorded pair are skipped (the pair set is keyed on the spans).

**Phase 4, families.** A maximal pair's two spans have identical token ids by construction, so
both carry the same `key = FNV-1a(token ids)` and the same `token_len`. A family is the set of
distinct spans sharing `(key, token_len)`. Then subsumption: family `F` is dropped when every
member of `F` lies inside some member of a family `F'` with `F'.token_len > F.token_len` and
`|F'.members| >= |F.members|` (the longer family already says everything `F` says). Two families
that overlap partially both survive; a span may appear in more than one family, and the rules
below cite it twice when that happens.

**Determinism.** Files are read in path order; buckets, pairs and members are sorted; the
LRU affects only time, never output; `key` is content-derived so a family has the same id on
every run of the same tree. No randomness, no model, no clock.

### 5. Complexity bounds and the memory budget on a 20k-file repo

Assume the scan's hard cap (`HARD_CAP_FILES = 20_000`, `files.rs:12`), a 150-line average
source file, 8 tokens per line: `N = 24M` tokens.

- Tokenizing is one extra parse per file (the existing extractors already parse each file once
  per call: `imports`, `functions`, `exports`, `literals`, `computed_loads` are five parses; a
  shared parse cache is a later win, noted in "found wrong"). O(N).
- Fingerprints: `0.09 * N = 2.2M` entries at 16 bytes: 35 MB, sorted in place. The
  `max_index_bytes` cap (64 MiB) stops phase 1 and sets `truncated = true` before memory grows
  past the budget; a truncated index is still correct for everything it found, and every rule
  reports "not fully indexed" (section 7) instead of claiming cleanliness.
- Verification: bounded by `max_pairs` (200k extensions, each O(L), L typically < 400 tokens);
  the LRU holds at most 256 tokenized files (a 400 KB file is ~60k tokens at 16 bytes: 1 MB
  worst case, 256 MB absolute worst, in practice a few tens of MB; the LRU capacity is also
  byte-bounded at 64 MiB and evicts by size).
- Families: output only. Tens of thousands at most, each a few spans.

Budget: index 64 MiB + LRU 64 MiB + output; wall time dominated by parsing, the same order as
the existing extractor passes on the same tree. No model tokens are spent by this pass (work
order section 91).

### 6. Traps and their guards (the detector level)

File-level skips (never tokenized; listed in `skipped` with a reason):
- **D1 Generated code.** DEAD-CODE G11 markers in the first 10 lines; `.gen.* .generated.*
  .pb.* _pb2.* .d.ts` basenames; `generated/ __generated__/` segments. The scan prunes most of
  these (`files.rs`); the detector re-checks because the CI tree differs (G30).
- **D2 Vendored code.** Segments `vendor third_party third-party external deps node_modules
  bower_components dist build out`.
- **D3 Tests, fixtures, mocks, stories, e2e, snapshots.** `extract::paths::is_test_path`,
  plus `conftest.py`, `__snapshots__/`, `.snap`.
- **D4 Minified or bundled.** Any line longer than 1,000 bytes, or more than 60 tokens per
  line on average, or a basename containing `.min.` or `.bundle.`.
- **D5 i18n catalogs and data files.** Segments `locales i18n lang langs translations messages
  intl`; files whose every top-level item is a single object/array/dict literal (a data
  module: a lookup table, a fixture list, a routes table).
- **D6 Migrations and seeds.** Segments `migrations migration alembic versions seeds seeders
  seed db/migrate`.
- **D7 Size.** Over `MAX_FILE_BYTES` never arrives; a file over 20k tokens is skipped as
  `TooLarge` (it is a bundle or a generated table).

Stream-level guards (applied while tokenizing):
- **D8 Imports, exports, type declarations, license headers.** Excluded items and dropped
  comments (section 2). A file that is only imports has an empty stream.
- **D9 Boilerplate items.** Items whose kind is a React `propTypes`/`defaultProps` assignment,
  a `module.exports = {...}` object, a Django `Meta` class, a Rust `impl Default`/`From`/
  `Display` with a single-expression body, or a getter/setter pair (`get x() { return
  this._x }`) are tokenized (they are code) but can never clear the per-rule filters: no
  decision (SINGLE-SOURCE), not a function body under two names (NAMING), no JSX/hook
  (SHARED-PRIMITIVE). No special case is needed; the rule filters are the guard and the tests
  in each rule section prove it.
- **D10 Data windows.** A window where `$STR + $NUM + $LIT + , : [ ] { }` tokens are 60% or
  more of the tokens is a table, not logic; it is not fingerprinted. (Catches an inline
  translation map, a country list, a test matrix, a Tailwind variant map.)

Bucket-level guards:
- **B1 Ubiquity.** A bucket in more than 50 files is dropped and counted (section 4). A family
  present in more than 25% of the files of its language family AND in at least 5 files is
  flagged `house_pattern = true`; SINGLE-SOURCE and NAMING skip such families (a Next.js route
  handler skeleton, a Django class-view override, an Express error wrapper, a Rust `impl`
  skeleton all repeat by framework design); SHARED-PRIMITIVE keeps them up to 40 members
  (repetition across components is that rule's point).
- **B2 Tiny clones.** `min_tokens`, the distinct-kinds floor, and the per-rule thresholds.
- **B3 Nested self-overlap.** A span never pairs with itself or an overlapping span.

What the detector does NOT decide: whether a family is a business decision, the same concept,
or the same mechanism. Those are the three rules' judgments (sections below), and the
candidate-versus-finding line from the trap list above holds: a family is never a finding.

### 7. Shape: one candidate producer, three rules, one advisory seam

The three H rules are produced by ONE module, `crates/checks/src/clone_rules.rs`, so the index
is built once per repo:

```
pub const RULE_SINGLE_SOURCE: &str = "ARCH-SINGLE-SOURCE-LOGIC-1";
pub const RULE_NAMING: &str = "ARCH-NAMING-ONE-CONCEPT-1";
pub const RULE_SHARED_PRIMITIVE: &str = "UI-SHARED-PRIMITIVE-1";
pub const RULE_IDS: &[&str] = &[RULE_SINGLE_SOURCE, RULE_NAMING, RULE_SHARED_PRIMITIVE];
pub const INTEREST_GLOBS: &[&str] = &["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.mjs",
                                      "**/*.cjs", "**/*.py", "**/*.rs"];

pub enum CloneRule { SingleSourceLogic, NamingOneConcept, SharedPrimitive }

pub struct Candidate {
    pub rule: CloneRule,
    pub id: String,            // "<RULE>:<family key hex>:<token_len>"; stable across scans
    pub family: CloneFamily,
    pub facts: CandidateFacts, // the deterministic facts the finding will state (per rule)
    pub rank: u32,             // 0 = judged first (per-rule ranking, section "Cost")
}

pub struct CandidateSet {
    pub by_rule: BTreeMap<CloneRule, Vec<Candidate>>,  // ranked, capped (section "Cost")
    pub over_cap: BTreeMap<CloneRule, u32>,            // candidates NOT judged this scan
    pub index: IndexSummary,                           // files_read, skipped counts, truncated
}

/// Deterministic. `armed` selects which rules' candidates are built; an unarmed rule costs
/// nothing. Never panics.
pub fn candidates(files: &[(String, String)], armed: &HashSet<&str>) -> CandidateSet;

/// The structured, bounded input one candidate hands the judge (section "Wiring"): never a
/// whole file.
pub fn judge_input(c: &Candidate, files: &[(String, String)]) -> JudgeInput;
```

`clone_rules` is NOT registered in `arch_checker::all_checkers`. A registered checker's
`check()` output becomes a `Defect` finding in `arch_violation_to_finding` with no judgment in
between, and the write-time runner and the `camerata-check` CI binary (which have no model) run
every registered checker and would report an empty `check()` as clean. Instead:

- `mechanical_gate::detector_channel` gains an arm: `clone_rules::RULE_IDS` → `Some("clone_advisory")`,
  so the ledger loop in `audit_repos` (around `onboard.rs:1700`) has a named channel for these
  ids and does NOT record them there (the advisory seam records them; "Wiring").
- `camerata-check --rule-ids ARCH-SINGLE-SOURCE-LOGIC-1` reports the id as unsupported ("needs
  the scan's advisory tier"), never clean.
- The three rules keep `enforcement = "architectural"` (the W26/W4 gates only inspect
  `mechanical` for a missing channel; `is_ci_tier_rule` routes them to the ledger loop, which
  now knows the channel).

### 8. Clone detector: build notes (2026-10-22)

Built as `crates/checks/src/extract/clone.rs` plus `extract/clone/{builder,cache,families,index,
rust_tokens,skip,ts_tokens,verify}.rs` and tests under `extract/clone/tests/`. The API of
section 1 is as written except for the additions in 8.1. Every place the spec was silent or
wrong, and what was decided:

**8.1 API additions and one signature note.** `detect` takes `opts: &CloneOptions` (as section 1
writes it). Additions: `CloneFamily.house_pattern: bool` (section 6 B1 names the flag, section 1
omits the field; set when the family spans at least 5 files and more than 25% of the indexed
files of its language family); `ItemTokens.texts: Vec<String>` (the Exact text of each token,
parallel to `tokens`, empty in Structural mode: `exact_diff` receives no source and no interner,
so a `Disagreement` can only name a token if the item carries its text); `pub const K`;
`pub fn token_id(TokenClass, &str) -> u32` and the `TEXT_*` constants (a rule asks whether a
sequence holds a `$CMP` by comparing ids, it has no other way to read a Structural token);
`pub fn span_tokens(&ItemTokens, &CodeSpan) -> &[Token]`; `impl Default for CloneOptions`.
`Token.id` is not an interner index: it is the 32-bit FNV-1a of the class and the normalized
text, because the family `key` must be stable across runs and files without a shared interner.
Equality always compares id AND class, which makes a 32-bit collision need a collision inside
one class and a 30-token window; Exact-mode ids are compared the same way.

**8.2 Item numbering.** `ItemTokens.item` and `CodeSpan.item` are the index in the `Vec` that
`tokens()` returns. An item with no tokens (an import block, a comment) takes no number. Both
modes tokenize the same stream, so item numbers, token counts, lines and byte ranges are
identical, and a `CodeSpan` from the Structural index addresses the same tokens in an Exact
tokenization (tested: `both_modes_tokenize_the_same_stream_shape`). `ItemTokens.kind` is one of
`function class variable statement namespace data impl impl-empty trait const static macro other`;
`data` marks a lookup-table item (D5), and a file made only of `data` items is skipped.

**8.3 Tokenizers.**
- *tree-sitter dialects.* One cursor loop (no recursion; a 20,000-term `a + a + ...` chain is
  thousands of levels deep). An item holding a parse-error node is dropped, the rest of the file
  is kept (the spec says "ParseFailed" for the whole file; a file with one broken item is still
  evidence). A JavaScript file with errors is re-parsed with the TSX grammar, as the literal
  extractor does. Method and class-field NAMES are `$ID` (they declare, they are not read through
  `.`); an object-literal key is `$PROP`. TS type annotations stay in the stream (only top-level
  type items are excluded). Python docstrings are dropped like comments; Python keyword-argument
  names and attribute names are `$PROP`; `self`/`cls` are kept. `not in` and `is not` are one
  comparison token. A JSX text node is a string token (whitespace-only text is dropped).
- *Rust.* The file is lexed by `proc_macro2` and parsed by `syn::parse2`; `syn` supplies item
  boundaries and kinds, the tokens come from each item's token stream, so macro bodies are
  tokenized. Inline `mod x { .. }` is flattened (each function inside is an item; a window must
  not run across a module). Items marked `#[cfg(test)]` (and not `not(test)`), `#[test]` or
  `#[tokio::test]` are excluded as test code. `pub`/`pub(..)` is dropped wherever it appears, not
  only at top level (a `pub fn` inside an `impl` would otherwise split a family). Rust has no
  token-level unary/binary or generic/comparison distinction, so both are decided by the previous
  token (a name or literal before `-`/`*`/`&` makes it binary; a type-like name, `::`, `impl`, or a
  non-operand before `<` makes it a generic bracket). A wrong guess moves a token between classes
  consistently in every copy; it can make two sequences differ, never invent a match. Group
  nesting deeper than 200 is refused as `ParseFailed` (`syn`'s token buffer is recursive and a
  stack overflow aborts the process, so `foo!((((...` would kill a scan). `proc_macro2` keeps a
  per-thread source map of everything it lexed; each Rust tokenization ends by calling
  `proc_macro2::extra::invalidate_current_thread_spans`, so 20,000 files do not accumulate (and
  its 32-bit offsets do not wrap). Any `Span` another piece of code holds on the same thread is
  invalidated by that call, so nothing in this crate may keep a span across a `tokens()` call.
- *Numbers and strings (Exact).* Numbers go through `canonical_number` (Rust: after dropping a
  type suffix); strings are their content with delimiters and prefix removed and `\'`, `\"`, `\\`
  resolved, so a quote-style change is not a disagreement. A template or f-string substitution
  appears as `${}` / `{}` inside the string text and its tokens follow the string token.

**8.4 Skips.** Order: not a source language, test path (`dead_code_checker::exclusions::
is_test_file`, a superset of `paths::is_test_path`), generated (`is_generated_path`, the marker
list of DEAD-CODE G11, `.pb.`/`_pb2.`/`_pb2_grpc.` names, `.d.ts`/`.pyi`, a committed `.js` beside
its `.ts` via `paths::emitted_js_paths`), migrations and seeds, vendored segments (D2 plus
`site-packages`, `venv`, `.venv`), minified (`.min.`/`.bundle.`, a line over 1,000 bytes, over 60
tokens per line), i18n segments, then the token-level guards (over 20,000 tokens, data module).
`SkipReason` has no variant for D6, so migrations and seeds report `Generated`, and a data module
reports `I18nCatalog`; the enum is exactly the spec's. The three predicates in `exclusions.rs`
became `pub(crate)` rather than copied.

**8.5 Hashing and the index.**
- *The fingerprint is really 16 bytes.* The spec's `(hash u64, file u32, item u32, token_index
  u32)` is 20 bytes of fields and 24 as a Rust struct. The build stores `(hash u64, file u32,
  position u32)`, `position` being the K-gram's token offset across all of the file's items; phase 3
  recovers `(item, index)` from the re-tokenized file (a test pins `size_of == 16`, so
  `max_index_bytes` is the true memory).
- *D10 is per window and per match.* A K-gram whose tokens are 60% or more data (`$STR $NUM $LIT`
  and `, : [ ] { }`) hashes to "never select" (winnowing skips it, the guarantee holds for every
  window that has a usable K-gram), and a verified match that is itself 60% data is dropped.
- *Boilerplate.* A bucket is dropped (and counted) when it spans more than `max_members` files
  OR holds more than `4 * max_members` fingerprints: one bucket of hundreds of fingerprints in a
  few files would otherwise spend the whole verification budget on one pattern.
- *Overlap.* Two spans of one item that overlap are trimmed to one period (a block repeated three
  times in a function yields two non-overlapping copies), not discarded as the spec says; the
  claim stays true and a repeated block is not lost. Family members that still overlap each other
  are reduced to the earliest.
- *Language families.* TypeScript, TSX and JavaScript are one family; Python and Rust are their
  own. A per-family salt is mixed into every fingerprint, and verification would reject a mismatch
  anyway, so a TS and a Python sequence of identical tokens never meet.
- *Determinism.* Inputs are sorted by `(path, content)` and exact duplicates dropped; fingerprints
  are totally ordered; phases 1 and 3 run on up to `available_parallelism` threads but write
  per-file or per-group results that are merged and sorted, so the thread count never shows (a
  property test shuffles the files; another compares repeated runs). A panic inside a parser is
  caught and becomes a `ParseFailed` skip (or an empty group), so `detect` cannot take a scan down.

**8.6 The pair cap counts extensions, in rounds.** The spec bounds "candidate pairs" at 200,000
and says each is an extension. Measured at K = 20 on this repository's `crates/` plus `agora-mono`
(1,400 indexed files), a clone region of 100 tokens has about seven fingerprints, so the same
two files produce seven candidates and one extension (41,511 of 130,130 candidates were inside a
region an earlier extension had matched), and about three quarters of the extensions that remain
(67,133 of 88,614) are 20 to 39 token coincidences that die after the extension. Counting
generated candidates would truncate a 3,000-file repository. The build therefore produces candidates a round at a time
(at most 100,000, and no more than `max_pairs`), skips any candidate inside a region an earlier
extension already matched (no tokenizing, not counted), and counts only extensions attempted.
`truncated` is set when the cap is spent and a not-yet-matched candidate remains; a spent cap
with only matched candidates left is not a truncation. The consequence is that candidates are
never all in memory and a larger cap costs only time.

**8.7 Cache.** Spec section 4 says an LRU of 256 files and section 5 says 64 MiB. A tokenized
150-line file is 25 to 50 KB (20 bytes a token), so 256 files is at most a fifth of the byte
budget. The cache is bounded by
the 64 MiB budget (split across the workers) with an entry guard of 16,384, evicting least
recently used in O(log n). A file larger than the whole budget is used once and not kept.

**8.8 Measured (release, 8 cores, 2026-10-22).**
- Tokenizing one core: TypeScript 6.5 MB/s, Python 5.9, Rust 7.1 (the tree-sitter parse alone is
  7.4 MB/s on the same TypeScript: the walk adds under 15%).
- Real code (`camerata-orchestrator/crates` plus `agora-mono`, 2,540 files, 26.6 MB, 1,400
  indexed), at the spec's K = 20: 3.7 s, 90,425 fingerprints, 88,614 extensions, 4,082 families, not truncated (K = 30: table in 8.10).
- Synthetic 20,000 files, 131 MB, 1.77M lines (`tests/perf.rs`, `--ignored`), K = 30 defaults
  (8.10): 95.7 s, 19,990 files indexed, 9,968 families, 87 boilerplate buckets, NOT truncated;
  peak resident memory 416 MB against 217 MB for the generated corpus alone. With the pair cap
  lifted the run needs 340,622 extensions and takes 104 to 114 s (about 690 CPU-seconds; each
  extension is tokenization-bound). At the spec's K = 20 the same corpus needed far more: with
  the cap at 200,000 it was spent on 20 to 39 token coincidences (526 families, 96 s,
  `truncated = true`), and lifted it took 659 s (3,902 CPU-seconds) and 630 MB.
- Raising `max_pairs` is safe memory-wise (8.6); it costs about 0.3 ms of CPU per extension when
  the files are cached and about 2 ms when they are not.

**8.10 K = 30, W = 11 (orchestrator decision, 2026-10-22).** The consumer rules only use families
of 40 tokens or more (40 for UI-SHARED-PRIMITIVE-1, 60 for the two logic rules), so a K-gram below
the 40-token floor buys nothing. `K = 30`, `W = T - K + 1 = 11`, `T = 40`: the detection floor is
unchanged and a test pins it (`the_winnowing_parameters_guarantee_a_floor_no_rule_threshold_
undercuts`: `T = W + K - 1`, `T <= 40`). Defaults moved with it: `max_index_bytes` 64 MiB to
96 MiB (K = 30 keeps 1.8x the fingerprints), `max_pairs` 200,000 to 400,000 (the 20,000-file
synthetic repository needs 340,622 extensions and finishes in about 96 s with the default).

Measured on the real corpus (`camerata-orchestrator/crates` plus `agora-mono`, 1,400 indexed
files), families dumped from both builds and diffed by (key, length, members):

| | K = 20, W = 21 | K = 30, W = 11 |
|---|---|---|
| Fingerprints | 90,425 | 160,417 |
| Index bytes (16 each) | 1.38 MiB | 2.45 MiB |
| Extensions attempted | 88,614 | 41,834 (0.47x) |
| Wall time | 3.77 s | 1.86 s |
| Families | 4,082 | 4,275 |

Recall of the 4,082 K = 20 families (all of length >= 40): 4,059 are identical under K = 30; 22
are strict supersets (same key and length, K = 30 lists copies K = 20 missed); 1 is lost; K = 30
also finds 216 families K = 20 did not. The lost family is key `5dca05b5d8be9cd2`, 53 tokens,
`QuoteService.ts:69-78` against `auth/mePayload.ts:27-36`: a `key: row.field,` object-literal
mapping. It is not a winnowing failure: the only fingerprint the two copies share under K = 30
falls in a bucket spanning more than 50 files, which B1 drops as boilerplate (K = 20 found the pair
only because it happened to select a different, non-ubiquitous fingerprint; with `max_members`
lifted K = 30 finds it again). Decision: accept it. Dropping a ubiquitous pattern is the B1 guard
working as specified and errs toward a false negative; neither K covers that pattern reliably;
and no consumer rule would act on it (a pure field mapping has no decision token for
SINGLE-SOURCE-LOGIC, and NAMING needs different names for one body, which the advisory judge
decides anyway).


**8.9 Tests.** `extract/clone/tests/`: `tokenize` (the normalization table row by row, exclusions,
positions, malformed and hostile input), `detect` (families, sorting, keys, subsumption, house
pattern, ubiquity, floors), `skips` (every D1 to D7 guard with the same body indexed in an
ordinary path), `exact` (named disagreements with both lines), `caps` (index and pair caps, cache
eviction, winnowing and verification internals), `props` (seeded property tests; the workspace
has no property-testing crate), `perf` (`#[ignore]` harness, see its header for the commands).

## ARCH-SINGLE-SOURCE-LOGIC-1: the algorithm and its traps

### 1. What a CANDIDATE is (deterministic)

A clone family from the index such that ALL of the following hold:

1. `token_len >= 60`, `distinct_files >= 2`, not `house_pattern`.
2. **Decision test.** The shared sequence contains (a) at least one of: a `$ARITH`/`$CMP`/
   `$LOGIC` token, an `if`/`elif`/`else if`/`match`/`switch`/`case`/`when` keyword, or a
   ternary (`?` followed later by `:` at the same nesting depth; Python `if`/`else` expression);
   AND (b) at least one of: a `$NUM`, a `$STR`, a `$LIT`, or a `$PROP`. (The work order's
   "conditional on a domain identifier": a comparison that touches a field or a literal.)
3. **Not an error wrapper.** Excluding sequences whose only decision tokens sit inside a
   `catch`/`except` clause or a `throw`/`raise` statement (a retry/log/rethrow skeleton has
   comparisons and literals but is not a business decision; the advisory would reject most of
   these, and the cheaper deterministic cut keeps them off the bill).
4. Every member is inside a function, method or arrow body (`extract::functions` spans), or
   is a top-level expression statement in a module that is not a config-shaped file
   (DEAD-CODE G3 basenames). Module-level constant tables are D5/D10.

For a candidate, the detector then computes the facts the finding will state:

- `agree: bool` and `disagreements: Vec<Disagreement>`: the Exact-mode token sequence of every
  member is compared with the first member's (members sorted by path). **"Agree" means: the
  Exact-mode token sequences of all members are identical.** Since Structural equality already
  holds, the only tokens that can differ are property names, literal values and operator text;
  a disagreement therefore always reads as one of "`??` versus `||`", "`0.12` versus `0.15`",
  "`total` versus `subtotal`", "`>=` versus `>`", "`'pending'` versus `'PENDING'`", each with
  both lines. The family does not agree iff any member differs from the first.
- `decision_tokens`: the Exact text of the operator/literal/property tokens in the shared
  sequence (for ranking and for the judge).

### 2. What the advisory judgment decides, and the exact structured input

The judge answers ONE question per candidate: **is this one application decision implemented
in each listed place** (a price, fee, status derivation, display name, permission check, tier
or bucket computation), as opposed to boilerplate, a framework shape, or a coincidence? It does
not decide agreement (deterministic), locations (deterministic), or severity (deterministic
from `agree`).

Input per candidate (`JudgeInput`, serialized as JSON in the prompt; never a whole file):

```
{
  "id": "ARCH-SINGLE-SOURCE-LOGIC-1:9f3a...:74",
  "rule": "ARCH-SINGLE-SOURCE-LOGIC-1",
  "token_len": 74,
  "member_count": 3,
  "members": [                       // at most 3 shown, the rest counted in member_count
    { "path": "src/cart/total.ts", "lines": "41-52",
      "enclosing": "function applyDiscount",          // extract::functions name, if any
      "code": "  if (user.isPremium) {\n    ..." },   // the span's raw source lines,
                                                      // at most 30 lines, trimmed
    ...
  ],
  "decision_tokens": ["isPremium", "?", "*", "0.2", ":", "0"],
  "agree": false,
  "disagreements": [{ "a": "0.2", "b": "0.15", "line_a": 44, "line_b": 18 }]
}
```

Output per candidate, a closed vocabulary parsed with `extract_json_object` exactly like the
calibration pass's verdicts:

```
{"verdicts":[{"id":"...","verdict":"same_decision|different|boilerplate|unsure","concept":"<= 8 words naming the decision, or empty"}]}
```

Only `same_decision` produces a finding. `different` and `boilerplate` are retained as judged
candidates (transcript + ledger counts), `unsure` and any unparsed or missing verdict are
UNJUDGED (never a finding; counted; disclosed). An id in the response that was not in the
request is ignored.

### 3. Never dropped, never shipped unjudged

- The full ranked candidate list (section 1) and every verdict are appended to the scan
  transcript (`TranscriptStore`, the same `feedback` channel `verify_findings` records into)
  under an agent named `clone-judge:<rule>:<repo>`, and the ledger records per rule: candidates
  found, judged, confirmed, unjudged, over cap. An operator can see every candidate; a client
  never sees an unjudged one.
- A candidate becomes a client finding only through a `same_decision` verdict. No model, no
  key, a failed batch, a parse failure, a verdict for an unknown id, `unsure`: none of these
  can promote a candidate. The degradation is a `not_assessed` row (Wiring, section "Offline").
- The verdict cache (Wiring) is keyed on the candidate's content, so an unchanged family on a
  re-scan is judged by its cached verdict, never re-billed and never re-guessed.

### 4. What becomes a client finding, and the verifiable fact it states

One finding per confirmed family. `path`/`line` = first member by path; `snippet` = the first
member's first source line; `also_locations` = every other member as a `MergedLocation` (so the
export lists every site and the ledger's location accounting reconciles); `captures = {"concept":
<judge concept>}`; `kind = Defect`; `preview = true`, `preview_tool = ARCH_PREVIEW_TOOL`;
`confidence = Some("high")` (section 6 explains); `evaluated_option_id = Some(violating option)`.

The fact stated, in two forms:

- **Copies do not agree (severity `high`):** "The same decision is written in 3 places and the
  copies do not agree. `src/cart/total.ts:41-52`, `src/checkout/summary.ts:15-26` and
  `src/api/quote.ts:70-81` are the same 74-token block apart from variable names, which appears to
  compute the premium discount, but at `src/cart/total.ts:44` the rate is `0.2` while
  `src/checkout/summary.ts:18` uses `0.15`. Implement it once: one function in one module,
  imported at every listed site, so a change to the rate is one edit. Verify by diffing the
  listed lines: the only differences are variable names and the differences described here."
- **Copies differ only outside the decision (severity `medium`; Wave C review):** "The same
  decision is written in 3 places. ... are the same 117-token block apart from variable names.
  The operators, numbers and tested properties are the same in every copy, and the copies
  differ only in these places: at `app/api/items/route.ts:13` the code has `item` while
  `app/api/orders/route.ts:13` has `order`. Implement it once and import it, so the copies
  cannot drift. ..." A difference is a DECISION difference only when it sits on an operator, a
  number, or a property name inside a decision test (the head of an `if`, `elif`, `while`,
  `switch`, `match` or `when`, or the condition of a ternary); strings, template names, action
  types, callee names, renamed-inconsistently variables and a property that is only copied
  through are named after the decision differences and never raise the severity.
- **Copies agree (severity `medium`):** "The same decision is written in 2 places and the
  copies currently agree. `src/a.ts:10-21` and `src/b.ts:33-44` are the same 66-token block
  apart from variable names, which appears to derive the order status. Implement it once and
  import it, so the two cannot drift. Verify by diffing the listed lines."

"Appears to compute <concept>" is the judge's phrase and is the only non-deterministic clause;
every location, count, token length and disagreement is machine-verified. If the judge returns
an empty concept the clause is omitted. "Do not agree" is stated ONLY from a non-empty
`disagreements` list, with the first three disagreements named and "and N more" for the rest.

### 5. False-positive traps, each with a guard

- **Boilerplate with a decision in it** (client factories with a `?? default`, Express/Next
  error wrappers, retry loops): guard 3 (error wrappers) and B1 (house patterns) remove most;
  the judge's `boilerplate` verdict removes the rest; `unsure` never ships.
- **Intentional per-surface variants** (work order safe twin: "documented in a shared
  helper's options"): if every member CALLS the same helper (a `$ID (` token sequence whose
  Exact-mode callee name is identical across members and resolves, via `imports()`, to one
  module), the repeated code is the call site plus its options, not a second implementation.
  Guard: a family whose shared sequence contains a call whose callee is imported from the same
  resolved module in every member file is dropped before the judge (`composed_variant`) ONLY
  WHEN that call dominates the sequence (Wave C review, D2): the call expression (callee through
  its closing parenthesis) covers at least 50% of the sequence's tokens, or every
  decision-bearing token of the sequence lies inside the call's argument list. A decision
  written around an unrelated imported call (`render(`, `auth(`, `useState(`, `t(`) is still a
  decision written in two places and stays a candidate; the judge's `boilerplate` verdict is
  what removes a framework shape.
- **Test arrange blocks**: D3.
- **A property-name difference that is not a disagreement about the rule** (`user.id` versus
  `account.id`): it IS reported as a disagreement, and the message names both tokens with
  their lines, so the claim is checkable in one look. The alternative (collapsing property
  names in Exact mode) would make "the copies agree" false for `total` versus `subtotal`.
  Decided: a disagreement names what differs; it never characterizes it beyond that.
- **Operator-only differences that are semantically equal** (`x !== null` versus `x != null`
  on a value that is never `undefined`): reported as a disagreement (they are different code);
  severity `high` is per the work order; the message is still true.
- **Cross-language copies** (a TS client and a Python server computing the same fee): never
  grouped; the grammars tokenize differently and the fix "one helper" does not exist across
  languages (the SHARED-CONSTANT ruling).
- **Generated, vendored, minified, migration, i18n, data**: D1 to D7, D10.
- **Same-file repetition only**: not a family.
- **Truncated scan or truncated index**: the rule still runs (a found family is a found
  family; absence is not claimed). No "clean" row is ever emitted by these rules (decision D1,
  below), so there is nothing to withhold.

### 6. Severity, confidence, domain, message, corpus TOML

- **Severity.** `high` when the copies disagree on a DECISION token (an operator, a number, or a
  property name inside a decision test; see section 4), `medium` otherwise: copies that agree,
  and copies that differ only in a string, a template name, an action type, a callee or a
  property that is merely copied through (the work order's "drops one level; the finding
  stays"). Orchestrator decision 3 (2026-10-22): the Redux and Django repros differ only in
  strings and must not read `high`. TOML `max_severity = "high"`. W26 applies: the rule carries
  `[[sources]]` and a `remediation` with `effort`.
- **Confidence.** `Some("high")`, `needs_review = false`. The model's verdict is a GATE on
  which families are shown, not a claim inside the row; every sentence except "appears to
  compute X" is a token-comparison fact, and that clause is marked as an appearance. A
  `needs-review` grade would route every row to the held bucket (`is_held_for_review`, C5-4)
  and `group_structural_needs_review` (P7) would collapse the medium rows of this rule into one
  grouped row, erasing the per-family disagreement the rule exists to state. This is the
  decision; it is reversible by one line in the seam if scan data shows the judge confirming
  coincidences.
- **Domain.** `universal` (Domain home table). Proposed-rules tests: proposed for a TS, a
  Python and a Rust repo; `recommended` and `is_auto_recommended` true (`policy`).
- **Message shape**: section 4. Client voice, no product names, no em-dashes. The site list
  is complete (every member; no "and N more" for sites; disagreements cap at three named).
- **Corpus TOML** `crates/rules/principles/universal/arch-single-source-logic-1.toml`:
  `id = "ARCH-SINGLE-SOURCE-LOGIC-1"`, `version = 1`, `domain = "universal"`, `layer =
  "universal"`, `enforcement = "architectural"`, `kind = "defect"`, `inspectable = true`,
  `max_severity = "high"`, `default = true`, `verification = "policy"`, `[[sources]]` internal
  only (this section; work order section 2; `docs/OVERNIGHT_DECISIONS.md` 2026-10-21 go). No
  external authority states the rule as specified; related controls for a TOML comment, not
  `[[sources]]`: jscpd and PMD CPD (token clone detectors; they report any duplication, not
  decisions, and never compare copies), SonarSource "duplicated blocks". `qualifies` states, in
  client voice: the token threshold, the one-language-family grouping, the decision test, that
  copies are compared for agreement after local names are set aside, that the "same decision"
  judgment is a reviewed assessment of the listed code, and every exclusion (tests, generated,
  vendored, migrations, i18n, data tables, framework-wide patterns, composed variants). Options:
  `each-decision-is-implemented-once` (default; directive: a price, fee, status derivation,
  display name, permission check or bucket computation is written in one function or module
  and imported wherever it is needed; `remediation`: move the logic into one exported function
  in the module that owns the concept, call it at every listed site, delete the copies, and
  verify by searching for the distinctive operator or value and finding only the one
  implementation; `effort = "medium"`) and `the-same-decision-is-written-in-several-places`
  (`violates = true`; fault-first `why`: copies drift, and a corrected rule in one place leaves
  the old rule running in the others).

### 7. Cost per scan, ranking and caps

- **Ranking** (deterministic, before any model call): `!agree` first; then `member_count`
  descending; then decision density (`decision_tokens.len() / token_len`) descending; then
  `token_len` descending; then first member path ascending.
- **Cap:** at most 160 candidates judged per rule per scan (10 batches of 16). Candidates
  beyond the cap are counted in `over_cap`, disclosed in the ledger coverage note and in the
  report's methodology ("N further repeated-decision candidates were found and not assessed
  this scan"), and never shipped. The cap is a constant (`CLONE_JUDGE_MAX_PER_RULE`), not a
  config knob, so cost is bounded by construction.
- **Tokens (measured, Wave C review D7):** a candidate is at most 3 members x 30 lines, each
  line cut at 120 characters (`MAX_LINE_CHARS`), plus capped fact lists. Measured on the widest
  possible candidate (thirty 200-character lines in every copy) the serialized judge input is
  10.9 KB, about 2,700 input tokens; the stated bound is 14,000 bytes (about 3,500 tokens) per
  candidate, asserted by `the_widest_judge_input_stays_inside_the_stated_per_candidate_bound`
  and, for a full batch, `a_full_batch_of_the_widest_candidates_stays_inside_the_stated_bound`.
  Typical code (lines of about 40 characters) is about 1,000 tokens per candidate. A batch of
  16 is therefore at most about 44k input tokens (typically about 16k) plus a stable prefix
  (system prompt, rule text, output schema; marked as the single cache breakpoint, so it is
  served from cache after the first batch). The earlier figure of "under 1,200 tokens per
  candidate, under 20k per batch" held only for short lines and is withdrawn. Output:
  `scaled_max_tokens(16, 80)` = the 4,096 floor; actual output about 60 tokens per candidate.
- **Overlap pass (Wave C review, D4):** before the cap, a candidate whose every site lies
  within a site of an already-kept candidate of the same rule is dropped, the wider candidate
  being considered first whatever its rank. One function copied in three files is found as a
  family for the whole function and for each periodic sub-block of it; it is one candidate.
- **Model calls:** `ceil(judged / 16)` per rule; worst case 10. On the registry's fast tier
  (`claude-haiku-4-5-20251001`, $1 in / $5 out per million, `model_registry.rs`): worst case
  for this rule 10 x 44k = 440k input + 10k output = about $0.50; a typical 3k-file repo with
  10 to 20 candidates is one or two calls of about 16k to 32k input, about $0.03. Zero on a
  re-scan whose families are unchanged (verdict cache).

### 8. Tests, by name (checks crate: no model; server crate: stub model)

Positive (`clone_rules.rs`, `candidates(files, armed)` with only this rule armed; expect ONE
candidate unless stated, with the stated facts):
- `premium_discount_written_twice_with_different_rates_is_a_disagreeing_candidate`:
  `src/cart.ts` and `src/quote.ts` each hold a 12-line function applying `user.isPremium ?
  subtotal * 0.2 : 0` plus a tax branch; the second uses `0.15`. Expect `agree == false`,
  `disagreements == [{a:"0.2", b:"0.15", ..}]`, `token_len >= 60`.
- `identical_copies_under_renamed_locals_agree`: same bodies, every local renamed. `agree ==
  true`, `disagreements` empty.
- `nullish_versus_or_is_a_named_disagreement`: `title ?? slug` block in one file, `title ||
  slug` in another, padded to 60 tokens with identical surrounding logic. One disagreement
  `??`/`||`.
- `property_name_difference_is_a_named_disagreement`: `order.total * rate` versus
  `order.subtotal * rate`. One disagreement `total`/`subtotal`.
- `three_files_are_all_listed_and_sorted`: three copies; members sorted by path.
- `python_status_derivation_copied_across_modules_is_a_candidate`: two `.py` modules with the
  same `if/elif` ladder over `order.state` and string literals.
- `rust_fee_computation_in_two_modules_is_a_candidate`: two `.rs` files with the same `match`
  over a field with numeric arms.
- `ranking_puts_disagreeing_families_first`: one agreeing, one disagreeing; `rank` order.
- `judge_input_shows_at_most_three_members_and_thirty_lines_each`.

Safe twins (zero candidates unless stated):
- `twin_client_factory_boilerplate_without_a_decision_is_not_a_candidate`: two identical
  `createClient(url, key, { auth: {...} })` blocks (no operator/conditional).
- `twin_error_wrapper_skeleton_is_not_a_candidate`: identical `try { ... } catch (e) { if (e
  instanceof X) throw ...; log(e) }` in two files (decision only inside catch).
- `twin_test_arrange_blocks_are_never_read`: the copies live in `*.test.ts`.
- `twin_composed_twin_whose_call_and_options_dominate_is_not_a_candidate` and
  `twin_composed_twin_whose_every_decision_is_inside_the_call_arguments_is_not_a_candidate`:
  both files import a helper from one module and the call dominates the sequence. The
  positives beside them: `composed_variant_with_its_own_decisions_around_the_helper_is_a_candidate`
  (the earlier twin, whose fixture has its own premium and coupon tests around the helper call,
  was over-broad and is now a candidate) and
  `an_unrelated_imported_call_beside_the_decision_is_still_a_candidate` (`render`, `auth`,
  `useState`, `t`).
- `twin_house_pattern_in_a_quarter_of_files_is_skipped`: the same route-handler skeleton with a
  `status === 401` check in 6 of 20 files.
- `twin_same_file_repetition_is_not_a_family`.
- `twin_generated_vendored_minified_and_migration_files_are_skipped`: one copy in `src/`, the
  other in `vendor/`, then in a `@generated` file, then in a `.min.js`, then in `migrations/`.
- `twin_data_table_window_is_not_fingerprinted`: two identical 80-token object literals of
  strings and numbers.
- `twin_cross_language_copies_are_not_grouped`: the same logic in `a.ts` and `a.py`.
- `twin_type_declarations_and_imports_contribute_no_tokens`: two files identical in imports and
  interfaces, different bodies.
- `twin_tiny_clone_below_threshold_is_not_a_candidate`: a 40-token decision block (index finds
  it; rule threshold 60 excludes it).
- `twin_malformed_and_empty_inputs_never_panic`.

Advisory seam (`crates/server/src/onboard/clone_advisory.rs`, with a `StubJudge: LlmPort`
whose `complete_streaming` returns scripted verdict JSON, the `StubModel` pattern of
`config_gated_architectural_rule_semantic_coverage_e2e.rs`; no live model, no key):
- `same_decision_verdict_becomes_a_high_finding_when_copies_disagree`: message contains "do not
  agree", both values, every path; `severity == "high"`, `confidence == Some("high")`,
  `also_locations.len() == members - 1`.
- `same_decision_verdict_becomes_a_medium_finding_when_copies_agree`.
- `different_and_boilerplate_verdicts_produce_no_finding_but_are_counted`.
- `unsure_and_missing_verdicts_never_produce_a_finding`.
- `an_unparseable_response_leaves_the_batch_unjudged_and_emits_not_assessed`.
- `a_verdict_for_an_unknown_id_is_ignored`.
- `no_model_emits_one_not_assessed_row_per_armed_rule_with_candidates_and_no_findings`.
- `zero_candidates_emits_no_not_assessed_row_and_credits_the_rule_as_run`.
- `cached_verdict_is_reused_without_a_model_call` (the stub counts calls).
- `over_cap_candidates_are_counted_and_disclosed_not_judged`.
- `judge_model_is_the_fast_tier_unless_overridden` and `judge_model_is_recorded_in_provenance`.
- `candidate_list_and_verdicts_are_written_to_the_transcript`.

### 9. Known false-negative limits (stated plainly)

- Copies shorter than 60 Structural tokens are never candidates; a two-line fee formula
  copied ten times is missed (ARCH-SHARED-CONSTANT-1 catches its literal when it has a domain
  shape).
- Copies that differ structurally (an `if` ladder versus a `switch`; a loop versus `.map`) are
  not the same family and are missed.
- Copies across languages, in tests, in generated or vendored code, in migrations, in config
  files, or in a pattern present in a quarter of the files are never reported.
- A candidate the judge marks `unsure`, a batch that fails, and every candidate past the
  160-per-rule cap are not reported this scan.
- "Do not agree" compares the shared sequence only; a difference OUTSIDE the shared block
  (an extra guard before one copy) is not seen.

## ARCH-NAMING-ONE-CONCEPT-1: the algorithm and its traps

### 1. What a CANDIDATE is (deterministic)

The work order lists three shapes. Two are built; the third is a stated limit.

**(a) Near-identical function bodies under different names.** A clone family with `token_len
>= 60`, `distinct_files >= 2`, not `house_pattern`, where every member span covers at least 90%
of the tokens of ONE function span (`extract::functions`; the smallest span containing the
member) and the set of declared names of those functions has at least two members whose
**name words differ**. Name words: split on `_`, `-`, camel and Pascal boundaries, lower-case,
drop the leading verb when it is one of `get set fetch load compute calc calculate format
render build make create to from as is has should` and the trailing noise words `impl helper
util utils fn func`. Two names are "different" iff the resulting word multisets differ
(`formatPrice`/`formatMoney` → `{price}`/`{money}`: different; `getUser`/`fetchUser` → `{user}`/
`{user}`: NOT different, that is a verb choice, not a concept split; `formatPrice`/
`format_price`: same). Members whose names are `<anonymous>` or `default` are kept in the
family listing but do not count toward "two different names".

**(b) Module overlap.** When two files are linked by two or more (a)-candidates (two function
pairs cloned between the same two files) the pair candidates are ROLLED UP into one module
candidate naming both files and every pair; the per-function candidates are removed. This is
the work order's `lib/format` versus `utils/format`; the rule states which functions overlap
instead of guessing at purpose from folder names.

**(c) Misleading names** (a `refresh` never called, a `public` fetch returning private fields):
no deterministic candidate exists without data-flow analysis; out of v1 (section 7).

Facts computed per candidate: the name pairs with their files and lines; the Exact-mode
diff (as in SINGLE-SOURCE; a naming family is also a logic family, and the message says whether
the bodies agree); the module roll-up when (b) applies.

### 2. What the advisory decides and its input

ONE question: **do these differently named functions implement the same concept** (one helper
that should have one name), as opposed to domain homonyms or a deliberate adapter at a
boundary? Input per candidate: `JudgeInput` as above with `"rule": "ARCH-NAMING-ONE-CONCEPT-1"`,
`"names": [{"name": "formatPrice", "path": "lib/format.ts", "lines": "4-19"}, ...]`, the member
code (at most 3 x 30 lines), `"agree"` and `"disagreements"`; for a roll-up, `"module_pair":
["lib/format.ts", "utils/format.ts"]` and the pair list. Output vocabulary: `same_concept |
different_concept | adapter | unsure`; only `same_concept` ships. `adapter` is retained as a
judged candidate (a boundary rename is the work order's safe twin).

### 3. Never dropped, never shipped unjudged
Identical to SINGLE-SOURCE section 3 (same seam, same transcript, same cache, same counts).

### 4. The client finding and its verifiable fact

Severity `low`, `kind = Defect`, `confidence = Some("high")`, `preview = true`, `captures =
{"concept": ...}`, `also_locations` for the other members.

- Function pair: "The same thing is called `formatPrice` in `lib/format.ts:4-19` and
  `formatMoney` in `utils/money.ts:11-26`: the two bodies are the same 68-token block apart from
  local names, and they currently agree. Keep one, re-export or delete the other, and give it
  the name that says what it does. Verify by diffing the two bodies."
- With a disagreement: "... and they do not agree: at `lib/format.ts:9` the fallback is `'USD'`
  while `utils/money.ts:16` uses `'EUR'`." (The disagreement is stated as in SINGLE-SOURCE; the
  severity stays `low`, because the rule's subject is the name. When the same family is also a
  SINGLE-SOURCE finding, that finding carries the severity and this one links to it by sharing
  `captures["family"]`; the exporter's location merge does not collapse them because the rule
  ids differ.)
- Module roll-up: "`lib/format.ts` and `utils/format.ts` overlap: `formatDate` matches
  `fmtDate` (`lib/format.ts:4-19` / `utils/format.ts:3-18`) and `formatNumber` matches
  `numberToText` (...). Keep one module; re-export from the other during the move, then delete
  it."

### 5. False-positive traps and guards

- **Domain homonyms** (two `normalize` functions for two different concepts): they are not in
  one family unless the bodies are token-identical; and if they are, the judge's
  `different_concept` keeps them out. `unsure` never ships.
- **Adapters that rename at a boundary** (`toApiUser` wrapping `mapUser`): the composed-variant
  guard from SINGLE-SOURCE applies (a member that calls the other member's function, resolved
  through `imports()`, is a wrapper, not a copy); what remains goes to the judge with the
  `adapter` verdict available.
- **Verb-only differences** (`getX`/`fetchX`, `buildX`/`makeX`): not "different names" by the
  word rule; never a candidate.
- **Overloads and dialect twins** (`foo.ts` and `foo.js` emitted copy): `emitted_js_paths`
  excludes the twin; a `.d.ts` is D1.
- **Framework-required duplicate names** (Next `page.tsx` files each exporting `default`,
  Django views each defining `get`): `default`/`<anonymous>` do not count; identical method
  names (`get` and `get`) are the same name, not a naming split.
- **Generated, vendored, test, migration, data**: D1 to D7.

### 6. Severity, domain, message, corpus TOML

- `low` always (`max_severity = "low"`); "misleading names at medium" is not attempted (c).
- Domain `universal`; proposed-rules tests for TS, Python, Rust repos.
- TOML `crates/rules/principles/universal/arch-naming-one-concept-1.toml`: `kind = "defect"`,
  `enforcement = "architectural"`, `inspectable = true`, `default = true`, `verification =
  "policy"`, internal `[[sources]]` (this section, work order section 5, the 2026-10-21 go).
  Options `one-concept-has-one-name` (default; directive: a helper that does one thing exists
  once under one name, and modules do not duplicate each other's helpers under different names;
  `remediation`: keep one implementation, re-export it from the old location while callers
  move, then delete the copy, and verify by searching for both names and finding one
  definition; `effort = "low"`) and `the-same-helper-exists-under-several-names` (`violates =
  true`). `qualifies` names the 60-token threshold, the whole-body requirement, the name-word
  rule, the agreement statement, the reviewed "same concept" judgment, and the exclusions.
  Related controls in a comment: none states this rule (eslint `no-dupe-*` rules are intra-file).

### 7. Cost, ranking, caps

Ranking: module roll-ups first (most sites per call); then `!agree`; then `member_count`
descending; then `token_len` descending; then path. Cap 160 judged per scan, batches of 16;
worst case about $0.25 on the fast tier, typically one call.

### 8. Tests, by name

Positive:
- `two_functions_with_identical_bodies_and_different_nouns_are_a_candidate`: `formatPrice` in
  `lib/format.ts`, `formatMoney` in `utils/money.ts`, 70-token identical bodies.
- `python_helpers_under_two_names_are_a_candidate`: `to_display_name` / `display_label`.
- `rust_free_functions_under_two_names_are_a_candidate`.
- `two_cloned_pairs_between_two_files_roll_up_to_one_module_candidate`.
- `body_disagreement_is_carried_into_the_naming_facts`.
- `name_words_ignore_verbs_separators_and_case`: unit test on the word rule (`getUser` /
  `fetch_user` same; `formatPrice` / `formatMoney` different; `userName` / `user_name` same).

Safe twins:
- `twin_same_name_in_two_modules_is_not_a_naming_candidate`: two `formatPrice` (that is
  SINGLE-SOURCE's job, not this rule's).
- `twin_verb_only_difference_is_not_a_candidate`.
- `twin_wrapper_that_calls_the_other_function_is_not_a_candidate`.
- `twin_partial_body_overlap_below_ninety_percent_is_not_a_naming_candidate`: two functions
  sharing a 60-token middle but with different prologues (SINGLE-SOURCE may still see it).
- `twin_default_exports_do_not_count_as_different_names`: two Next pages.
- `twin_emitted_js_twin_is_ignored`.
- `twin_test_generated_vendored_files_are_skipped`.

Seam (stub model): `same_concept_verdict_becomes_a_low_finding_listing_both_names`,
`different_concept_and_adapter_verdicts_produce_no_finding`, `module_rollup_finding_lists_every_pair`,
plus the shared seam tests (no model, cache, cap, provenance, transcript) parameterized over the
rule.

### 9. Known false-negative limits

Misleading names (c) are not detected. Two helpers for one concept whose bodies differ
structurally are missed. Names differing only by verb are never reported. Bodies under 60
tokens, or in files D1 to D7 exclude, are never read. Module overlap is reported only through
cloned function pairs, never from export names or folder names alone.

## UI-SHARED-PRIMITIVE-1: the algorithm and its traps

### 1. What a CANDIDATE is (deterministic)

Scope: files with `lang_for_path` in `{Tsx, JavaScript (.jsx only), TypeScript (.ts), JavaScript
(.js)}` under a package whose nearest `package.json` depends on `react`, `preact`, `next`,
`react-native` or `expo` (detected from manifest text; the `ui` domain is proposed for those
stacks). Rust `rsx!`/Leptos `view!` and Vue/Svelte templates are out of v1 (section 9).

A clone family with `token_len >= 40`, `distinct_files >= 2`, at most 40 members (B1's
ubiquity skip does NOT apply; the 50-file bucket cap does), such that:

1. **Every member lies inside a UI unit:** a `jsx_element`/`jsx_self_closing_element`/
   `jsx_fragment` subtree (the span's first and last tokens are inside one JSX node), OR the
   body of a function passed to `useEffect`/`useLayoutEffect`/`useCallback`/`useMemo`/
   `useImperativeHandle`, OR the body of a function whose name matches `^use[A-Z]` (a custom
   hook).
2. **Mechanism mark:** the shared sequence contains at least one of: a conditional (`&&`
   inside JSX, a ternary, an `if`), a JSX attribute whose name starts with `on` followed by an
   upper-case letter, a `use[A-Z]...` call, `addEventListener`/`removeEventListener`, a
   `ref.current` access, `setTimeout`/`requestAnimationFrame`, `document.`/`window.`. Pure
   static markup (a layout with no behaviour) is the "one-off layout" safe twin and is not a
   mechanism.
3. **Not already composed:** when the outermost JSX node of every member's span is a single
   element whose tag is an identifier IMPORTED in that file (`imports()` names it) and the
   tokens outside that element are fewer than 40, the family is use of a shared component, not
   a hand-written mechanism; dropped (`composed_variant`). A wrapper with its own conditional
   or handler around an imported component stays a candidate.

Facts: `mechanism_marks` (which of the marks fired, with Exact text: `onClick`, `useEffect`,
`addEventListener('mousedown'...)`), `jsx_tags` (the kept tag names in order), `hooks` (the
`use*` callees), member count and files, and for the message the first member's first line.

### 2. What the advisory decides and its input

ONE question: **is this one UI mechanism hand-written in each listed component** (a spinner,
retry control, menu host, fade latch, empty state, focus trap, outside-click, scroll lock),
as opposed to unrelated markup that happens to share shape? Input: `JudgeInput` with `"rule":
"UI-SHARED-PRIMITIVE-1"`, members (at most 3 x 30 lines), `"jsx_tags"`, `"hooks"`,
`"mechanism_marks"`. Output: `same_mechanism | different | unsure`; only `same_mechanism` ships;
the `concept` field names the mechanism (`"outside-click dismiss"`, `"loading spinner"`).

### 3. Never dropped, never shipped unjudged
As above.

### 4. The client finding and its verifiable fact

Severity `low`, `kind = Defect`, `confidence = Some("high")`, `captures = {"mechanism": ...}`,
`also_locations` for the other members: "4 components hand-write the same outside-click
dismiss: `components/Menu.tsx:22-31`, `components/Popover.tsx:40-49`,
`features/filters/Dropdown.tsx:15-24` and `app/(shop)/CartDrawer.tsx:58-67` contain the same
58-token effect apart from variable names and strings. Make it one hook or component in the
shared UI module and use it in each listed place. Verify by diffing the listed ranges."
Strings are set aside explicitly in the sentence because labels and class names are
normalized (the work order: "with only labels differing").

### 5. False-positive traps and guards

- **One-off layouts**: the mechanism mark (1.2).
- **Variants already composed from the primitive**: 1.3, plus the judge.
- **Design-system wrappers** (`<Button variant="ghost">` everywhere): a single imported element
  is composed; a family of attribute lists is under 40 tokens or fails the mark.
- **Framework-mandated wiring** (Next `layout.tsx` shells, Storybook stories, Remix route
  exports): stories are D3; a `layout`/`page` shell with no mechanism fails 1.2; one with a
  mechanism repeated in 10 routes is a real finding.
- **Hook rule boilerplate** (`useEffect(() => { ... }, [])` subscribe/unsubscribe pairs for
  DIFFERENT events): tag/attribute/hook names are kept, strings are not, so
  `addEventListener('scroll')` and `addEventListener('resize')` effects DO match structurally;
  the judge decides whether a "window event subscription" is one mechanism. The message names
  the Exact text of the differing string in a trailing clause ("the subscribed events differ:
  `scroll`, `resize`") so the client sees the difference the rule set aside.
- **Tests, stories, generated, vendored, minified**: D1 to D7.
- **Non-React JSX (Solid, Qwik)**: the manifest gate keeps the rule off packages without a React
  family dependency (false negative).

### 6. Severity, domain, message, corpus TOML

- `low` (`max_severity = "low"`).
- Domain `ui`. **Carries the recorded `domain_to_glob("ui")` fix**: `crates/rules/src/lib.rs:3288`
  still maps `ui` to `**/*.{tsx,css}` (this feeds an emitted `Role.allowed_paths`, not the scan;
  the scan uses `INTEREST_GLOBS`). Widen to `**/*.{jsx,tsx,ts,js,css}` with a test. The
  proposed-rules test for this rule uses the `src/app.jsx` + React manifest fixture already in
  `propose.rs`.
- TOML `crates/rules/principles/ui/ui-shared-primitive-1.toml`: `domain = "ui"`, `layer =
  "universal"`, `enforcement = "architectural"`, `kind = "defect"`, `inspectable = true`,
  `default = true`, `verification = "policy"`, internal `[[sources]]` (this section, work order
  section 6, the go). Options `repeated-ui-mechanisms-are-one-shared-component` (default;
  `remediation`: extract the mechanism into one hook or component in the shared UI module,
  replace each listed copy with a use of it, and verify by searching for the mechanism's
  distinctive call and finding it only in the shared module; `effort = "low"`) and
  `ui-mechanisms-are-hand-written-per-component` (`violates = true`). `qualifies` states the
  JSX/hook scope, the 40-token threshold, the mechanism marks, the composed-variant exclusion,
  the React-family package gate and the reviewed "same mechanism" judgment. Related control in
  a comment: the existing per-subject rules (`UI-IMAGE-COMPONENT-1`, `UI-UTC-DATES-1`) which
  this generalizes.

### 7. Cost, ranking, caps

Ranking: `member_count` descending; then number of mechanism marks descending; then
`token_len`; then path. Cap 160, batches of 16, worst case about $0.25 on the fast tier.

### 8. Tests, by name

Positive:
- `outside_click_effect_in_three_components_is_a_candidate`: three `.tsx` files with the same
  `useEffect` subscribe/unsubscribe/contains block.
- `conditional_spinner_block_in_two_components_is_a_candidate`: `{loading && (<div ...><Spinner
  /><span>Loading</span></div>)}` padded with a retry button `onClick`.
- `empty_state_with_retry_handler_is_a_candidate`.
- `custom_hook_body_duplicated_across_files_is_a_candidate`: `useScrollLock` in two files.
- `jsx_file_under_react_package_is_in_scope`.
- `differing_labels_and_classnames_still_match_and_are_named`: message names the differing
  strings.

Safe twins:
- `twin_static_layout_without_a_mechanism_is_not_a_candidate`: identical 60-token markup, no
  conditional/handler/hook.
- `twin_single_imported_component_with_props_is_composed`: `<Modal open={x} onClose={f}>`
  usage in five files.
- `twin_wrapper_with_its_own_conditional_around_an_imported_component_is_still_a_candidate`
  (positive sibling of the above).
- `twin_stories_and_tests_are_skipped`.
- `twin_package_without_react_family_dependency_is_out_of_scope`: Solid JSX.
- `twin_vue_and_svelte_files_are_never_read`.
- `twin_below_forty_tokens_is_not_a_candidate`.

Seam (stub model): `same_mechanism_verdict_becomes_a_low_finding_counting_components`,
`different_verdict_produces_no_finding`, plus the shared seam tests.

### 9. Known false-negative limits

Rust `rsx!`/`view!`, Vue, Svelte, Angular templates and CSS-only mechanisms are not read.
Mechanisms split across a component and a hook file are not one span. Static markup is never
reported. Packages without a React-family dependency are never reported. Anything below 40
tokens, or structurally different (a class component versus a hook), is missed.

## Wiring the H rules: the advisory seam

**Where.** New module `crates/server/src/onboard/clone_advisory.rs` (server side: it needs
`LlmPort`). Called from `audit_repos` immediately after `audit_architectural` and the ledger
loop (`onboard.rs:1642` to `1744`), inside the `run_deterministic` block (candidates are
deterministic), with the judge call gated on `run_ai_review` (the same gate as every other
model pass; `Blocked` has already forced it off at `onboard.rs:1306`). It uses the SAME `llm`
(`onboard.rs:1396`) and folds usage into the SAME `meter`, so the cockpit's actual-versus-
estimated readout sees it.

```
pub struct JudgeOutcome { pub findings: Vec<Finding>, pub not_assessed: Vec<Finding>,
                          pub counts: BTreeMap<CloneRule, JudgeCounts>, pub failed: Vec<FailedPass> }
pub struct JudgeCounts { pub found: u32, pub judged: u32, pub confirmed: u32, pub unjudged: u32, pub over_cap: u32, pub cached: u32 }

pub async fn judge_clone_candidates(
    llm: Option<&dyn LlmPort>,        // None: no model this scan (deterministic-only or blocked)
    repo: &str, files: &[(String, String)], set: CandidateSet, armed: &HashSet<&str>,
    judge_model: &str, cache: &mut VerdictCache, meter: Option<&UsageMeter>,
    feedback: Option<(&TranscriptStore, &str)>, index_truncated: bool,
) -> JudgeOutcome;
```

**Prompt shape** (mirrors `verify_findings`): system prompt = the judge's role and the closed
output vocabulary; user prompt = `Repository: {repo}\n` + the rule's `directive` and `qualifies`
text from the corpus (so the model judges against the client-facing definition) + the batch's
`JudgeInput` JSON array. One cache breakpoint after the rule text (`with_cache_prefix_len`).
`max_tokens = scaled_max_tokens(batch_len, 80)`. Streamed via `complete_streaming` so the
transcript shows real output. One batch failure loses only that batch's candidates (unjudged),
never the scan.

**Model choice.** Classification on bounded structured input is the fast band of the project's
tier map (`docs/decisions/2026-06-20_deterministic_model_tiering.md`; the owner's rule that
tiering applies to Camerata's own calls, and the session process line "Haiku does mechanical
work"). `resolve_clone_judge_model(caller_pick)`: the caller's explicit pick, else
`CAMERATA_CLONE_JUDGE_MODEL`, else the project's `TierMap.fast` primary
(`fleet::tier::default_fast_model()` = `claude-haiku-4-5-20251001`). Never the audit model by
default. The resolved id is stamped into a new `ScanProvenance.clone_judge_model:
Option<String>` (beside `audit_model`/`calibration_model`), so a before/after comparison can
attribute a change to the judge; the graded sweep's GRADER model is a separate harness setting
and is not touched by this work (MASTER-SEQUENCE 19f: grader model fixed, rubric revised).
On the CLI backend the id is passed as `--model` exactly as today (`llm.rs:1323`).

**Cache.** `VerdictCache`: a sidecar JSON beside the project's scan manifest (same app-data
location as `scan_cache`, NOT in the repo), `serde(default)` additive so `MANIFEST_VERSION` is
not bumped. Key = `fnv1a(rule_id | judge_model | PROMPT_SCHEMA_VERSION | candidate.id |
normalized Structural token ids of the family | the Exact-mode disagreement list)`; value =
`{verdict, concept, judged_at}`. Keyed on content, not paths, so a moved file hits. A verdict of
`unsure` is NOT cached (it is retried next scan). Invalidated wholesale by a judge model change
or a prompt schema bump. The clone index itself is recomputed every scan (deterministic,
token-free); only verdicts are cached.

**Offline / no key / deterministic-only.** `llm == None` (or `run_ai_review == false`): no call
is made. For every armed H rule with `found > 0`: zero findings, ONE `not_assessed` row
(`FindingKind::NotAssessed { reason: PassFailed, reason_detail: "advisory judgment did not run
(<ai_blocked_reason | 'deterministic-only scan'>); N candidate families found, 0 judged" }`,
severity `info`, `path` empty, `located = false`, `preview = true`), and a ledger entry
`record_rule(id, RuleTier::Architectural, ran = false, skip_reason = Some(ADVISORY_UNAVAILABLE_NOTE),
files_read, 0)` plus `note_advisory_unavailable_rule(id)` (a new queue beside
`note_incomplete_scan_rule`, constant `ADVISORY_UNAVAILABLE_NOTE = "withheld: the deterministic
pass found N candidate sites but the reviewed judgment that decides which are real did not run
this scan, so the rule is reported as not assessed rather than clean"`). Candidates are NEVER
promoted. With `found == 0` the rule ran and found nothing: `ran = true`, no row. A partial
failure (some batches judged) ships the judged findings and ONE `not_assessed` row for the
unjudged remainder with the count; `FailedPass { pass: "clone-candidate judgment" }` carries
the error text to the methodology disclosure.

**Index truncated / scan incomplete.** Findings still ship (a found family is a fact).

**Ledger.** The seam records each armed H rule once per repo under `RuleTier::Architectural`
with `files_evaluated = index.files_read` and `findings_emitted = confirmed`. The loop at
`onboard.rs:1700` skips ids whose `detector_channel` is `"clone_advisory"`. The transcript
agent per rule records the full ranked candidate list and raw verdict JSON.

**No convention row (orchestrator decision D1, 2026-10-22; replaces work order section 83 for
these three rules).** The three clone rules emit NO `ConventionObserved` or "clean" row in V1. A
candidate search that finds nothing, or whose candidates were all judged not to be the thing,
proves nothing: the detector sees repeated code of one shape, in one language family, above a
threshold, and a house pattern or a family of five disagreeing copies is dropped before the judge.
A row saying "no application decision was found written in more than one file" would claim an
absence that the Django, Next and house-pattern repros show to be false. A rule with zero
candidates is still credited in the ledger as having run (`ran = true`, nothing found), and that
is the whole statement the scan makes about it. Tests:
`no_repro_repo_gets_a_convention_row_through_the_real_pipeline` (Django, Next, house pattern and a
truly clean repo, through `audit_repos_with_llm`),
`a_repo_with_no_repeated_code_gets_no_rows_but_the_rules_are_credited_as_run`,
`a_clean_rule_is_credited_in_the_ledger_but_claims_nothing_to_the_client`.

**Coexistence with the semantic tier.** Every rule, including these three, is also in the
whole-file semantic prompt by the owner's 2026-10-09 ruling. An AI-tier row under an H id is an
ordinary AI finding: calibrated, cited, merged by `merge_by_location` when it lands on a site
the seam also reports. The seam's rows are deterministic-tier (`preview = true`) and are never
calibrated. No new merge logic.

**How a needs-review grade is presented.** It is not used for H rows (decided above, each
rule's section 6): the judge gates, the row states token facts, `confidence = Some("high")`.
The ONLY hedged outputs of this seam are the `not_assessed` rows, which the existing exporters
already render as "armed but not evaluated" (`report_export.rs:6036`, `xlsx_export.rs:3516`) and
which `is_informational` keeps out of the action tiers. Should scan data show the judge
confirming coincidences, the one-line change is `confidence = Some("needs-review")` in
`findings_from_verdicts`, which routes the rows to the held bucket via the existing
`is_held_for_review` gate; the P7 grouping consequence for the low rows is noted in
SINGLE-SOURCE section 6.

**Checklist per rule** (the orphan-rule list, applied): (1) TOML under the recorded go, `camerata
corpus lock`; (2) `clone_rules.rs` candidates + facts; (3) `detector_channel` arm and the
`camerata-check` unsupported notice; (4) proposed-rules test (universal x3 stacks; `ui` for the
React fixture, with the `domain_to_glob` fix); (5) positive + safe-twin tests (checks crate) and
the seam tests with the stub model (server crate); (6) `ScanProvenance.clone_judge_model`;
(7) `VerdictCache` sidecar; (8) `ADVISORY_UNAVAILABLE_NOTE` + `note_advisory_unavailable_rule`;
(9) `ecma::parse`/`python::parse` to `pub(crate)`; (10) transcript agent names
`clone-judge:<rule>:<repo>`.

**Total model cost of Wave C per scan.** Three rules x at most 10 batches = at most 30 fast-tier
calls, about 1.3M input at the measured worst case (the stable prefix is cache-served after the first batch) and 30k output: under $1.50
worst case at the registry's Haiku 4.5 price, about $0.05 to $0.10 on a typical repo, $0 on an
unchanged re-scan. For comparison the audit pass runs on the audit model over every chunk of
the repo; the judge never sees a file.

## Wave C build notes: the producer, the seam and the corpus rules (2026-10-22)

Built as `crates/checks/src/clone_rules.rs` plus `clone_rules/{bindings,compare,composed,context,
decision,facts,judge,names,naming,primitive,single_source}.rs` and tests under
`clone_rules/tests/`; `crates/server/src/onboard/clone_advisory.rs` plus `clone_advisory/{cache,
findings,prompt}.rs` and tests under `clone_advisory/tests/`; the three corpus TOMLs; an end-to-end
test, `crates/server/tests/clone_advisory_e2e.rs`, that runs the whole `audit_repos` pipeline with a
scripted model. The sections above stand; this list is every place the build decided something the
spec left open or departed from it, and why. The detector API is used as built (section 8).

**W.1 Producer shape.**
- `Candidate.facts` is one `CandidateFacts { token_len, sites, agree, disagreements, rule }` with a
  `RuleFacts` enum (`Logic`, `Naming`, `Primitive`) for what only one rule knows, instead of per-rule
  fact structs. `sites` is every place a candidate lives, which is the one list the finding, the judge
  input and the transcript read; for a naming module roll-up it is the union of every pair's sites, and
  `family` is the longest pair's family.
- `CandidateSet.by_rule` is ranked and capped (as specified) and gains `found(rule)` (judged plus
  `over_cap`). `IndexSummary` gains `files_skipped`, `families` and `ui_files` (see W.5). An ARMED rule
  always has an entry, empty when it found nothing, so the seam can tell "ran, nothing" from "not armed".
- Disagreements are deduplicated per member and per pair of texts (a block that repeats `total` versus
  `subtotal` five times is one named difference at its first position), and a difference is a named
  token pair with both lines, never a characterization.
- Rust `?` is not a ternary: the ternary test is skipped for Rust, and for other languages needs at
  least one token between the `?` and the `:` (`x?: T` is an optional member, not a branch).

**W.2 ARCH-SINGLE-SOURCE-LOGIC-1.**
- The decision test (a decision keyword, operator family or ternary, AND a literal or property name)
  counts only tokens OUTSIDE `catch`/`except` clauses and `throw`/`raise` statements. The spec excludes
  a sequence whose only decision tokens sit there; requiring both halves outside is the stricter,
  false-negative-leaning reading. The `throw`/`raise` region ends at a `;` at its own depth, the close of
  the enclosing block, or (Python) the end of its line; over-extending only removes tokens from the test.
- "Distinct token kinds" is read as distinct normalized token ids in the shared sequence (at least 8).
- "Every member is inside a function" is: the member's item is a function, or an `extract::functions`
  span contains the member's lines, or it is a module-level statement in a file that is not config
  shaped (`is_config_shaped` became `pub(crate)`).
- `composed_variant` uses its own import reader (`clone_rules/bindings.rs`): `extract::imports` records no
  name for a default import (`import Modal from '..'`), which would have made the guard blind to the
  commonest component import. The guard fires when the shared sequence has an `$ID (` call at the same
  offset in every copy, the same callee text in every copy, imported in every copy's file from one module
  after resolving relative specifiers against the importing file (`./pricing` in `src/a.ts` and
  `../src/pricing` elsewhere are one module). A Rust `self::`/`super::` path is made unique to its file so
  it can never compare equal.
  The guard fires only when that call also DOMINATES the sequence (W.9, D2).

**W.3 ARCH-NAMING-ONE-CONCEPT-1.**
- "Covers at least 90% of one function span" is read as at least 90% of one TOP-LEVEL function item of
  the detector's tokenization (`ItemTokens.kind == "function"`, which includes `const f = () => {}`).
  Methods inside a class or an `impl` block are one `class`/`impl` item and are not read in v1; that is a
  false negative added to section 9.
- The wrapper guard is: a member's span calls (an `$ID (` token) a name another member declares, from
  another file, and its file imports that name. The roll-up links only candidates that span exactly two
  files; a candidate that spans three or more stays a function candidate so no file is lost. A roll-up's
  `token_len` is the sum of its pairs'.
- Name words treat an acronym run as one word (`HTTPResponse` is `http`, `response`); a name that the
  verb/noise removal would empty keeps its words (`get` and `set` stay different).

**W.4 UI-SHARED-PRIMITIVE-1.**
- A UI unit also includes a component: a function whose body renders JSX. Without it a copy-pasted
  component (the clone is the whole function, so no sub-region is a separate family) would never be a
  candidate, which would miss the rule's plainest case. The mechanism mark and the judge still decide.
- Marks that depend on a name (a hook call, `addEventListener`, `ref.current`, `document.`) are read from
  each copy's source, because Structural mode collapses those names, and the mechanism must fire in EVERY
  copy (the intersection of the copies' marks). A `function useX(` declaration is not a hook call.
- A copy in a package outside the React-family scope is dropped from the candidate, not the whole family:
  the remaining copies must still span two files and at most 40 places. The package gate reads the
  nearest `package.json` among the scan's files (`dependencies`, `devDependencies`, `peerDependencies`).
- The composed-variant exclusion counts a default-imported tag as imported (W.2's reader).

**W.5 The seam.**
- `audit_repos` is now a thin wrapper that builds the real `Llm` and calls `audit_repos_with_llm(llm:
  &dyn LlmPort, ...)`. The signature of `audit_repos` and every caller are unchanged; the new function is
  what lets a test run the whole pipeline with a scripted model, which the spec's "audit_repos end-to-end
  test with the stub model" requires.
- The judge takes a `JudgeRequest` struct instead of the spec's long argument list, and reports
  `rejected` (a definite non-affirmative verdict) and `model_calls` in addition to the spec's counts.
- **The verdict cache is a field of the scan manifest**, `ScanManifest.clone_verdicts` (`serde(default)`,
  `MANIFEST_VERSION` unchanged), not a separate sidecar file. It lives in the same app-data file the
  manifest already does, never in the repo; it needs no new store, no new project-id plumbing and no
  change to `audit_repos`' signature, and "Full scan (ignore incremental cache)" correctly re-asks.
  It is seeded from the prior manifest even when that manifest is unusable as an AI baseline (the keys are
  content). Capped at 4,000 verdicts, oldest dropped first.
- The cache key is path-independent AND order-independent: the exact-mode differences are an unordered
  set of text pairs (a moved file can swap which copy sorts first), and the single-source key includes the
  sorted multiset of EVERY copy's Exact decision tokens, whether or not the copies agree (Wave C review,
  D5): a tested property renamed in all copies leaves `agree` and every difference unchanged but is a
  different decision, and must be judged again. `PROMPT_SCHEMA_VERSION` is 2.
- Offline never reads the cache: "zero findings" is absolute when no model is available.
- `resolve_clone_judge_model(None)` is what `audit_repos` calls; no caller pick is plumbed through the
  scan request (that would change a signature used by about 30 callers for a knob nobody has asked for).
  The environment override works today. `ScanProvenance.clone_judge_model` is stamped when the judge made
  at least one call this scan (cache hits make none), the way `audit_model` is stamped only when a model ran.
  It is on `ScanProvenance` (so it is in the persisted report and any machine read of it); the PDF cover
  and the workbook print only the audit and calibration models today and were not changed, because the
  three artifacts are held equal by cross-artifact gates and a new cover row is a presentation decision.
- Not-assessed rows: the spec emits one when no model ran or a batch failed. The build also emits the one
  row when candidates were left `unsure`, got no verdict, or lay beyond the 160 cap, with the counts, so a
  rule with candidates it could not decide never reads clean. Reason: `PassFailed` for no model or a
  failure, `ModelDeclined` for undecided.
- Ledger: a rule is `ran` only when every candidate found got a definite verdict (a rule with nothing
  found ran clean). `ADVISORY_UNAVAILABLE_NOTE` has no `N` (the count is in the row; a note that varies
  could not be matched by the integrity gate); `ADVISORY_PARTIAL_NOTE` and `note_advisory_partial_rule`
  cover a judgment that ran and fell short. The integrity gate classifies the unavailable note as
  `ScopedOut` (it is only recorded when no model was available, the same disclosed scope choice as
  "AI/semantic review not requested"); a partial judgment's reason is deliberately unrecognised, so it
  stays a defect if the semantic pass did not settle the rule.
- The ledger loop skips the clone ids by arm (`CLONE_ADVISORY_CHANNEL`), as specified.
- **There is no convention row** (D1, section "Wiring the H rules"). The first build emitted one after
  the semantic pass; the Wave C review withdrew it. `convention_rows`, `convention_row` and the
  `onboard.rs` call are removed.
- **`evaluated_option_id` is `None`, not the violating option.** The export resolves a finding's Fix
  through `evaluated_option_id` first, and the violating options carry no remediation (the fix text is on
  the default option), so naming the violating option made the Fix line vanish and, under C5-8, the row be
  held. Every other deterministic checker's defect leaves it `None`. Tested end to end through
  `build_report_json` (`a_judged_scan_ships_the_confirmed_finding_and_stamps_the_judge`).
- The concept clause reads "which appears to be the premium discount calculation" (the prompt asks for a
  noun phrase beginning with "the"); the spec's example verbs ("compute", "derive") cannot be chosen
  without knowing the concept. The phrase is scrubbed because it is model output reaching a client
  sentence (Wave C review, D6): at most the first 8 whitespace-separated words are read, quotes and
  backticks become apostrophes and dashes hyphens, and the WHOLE phrase is rejected (the sentence then
  omits the clause) if anything outside letters, digits, spaces, commas, apostrophes, parentheses and
  hyphens remains, so a sentence smuggled through `;`, `:`, a path or markup is dropped, not repaired.
- Live-preview progress: the seam streams its findings into the job (`add_findings`) but does not emit
  `det_tool_*` events; the UI's tool-name map has no label for it and the step is dominated by one model call.
- The transcript session is `clone-judge:<rule id>:<repo>` (Wave C review): a multi-repo scan shares one
  transcript key, and a session named for the rule alone was replaced by every later repo.

**W.6 `camerata-check`.** The three ids are reported in a new `unsupported_rule_ids` field (`serde(default)`,
so older output still parses), the human output says `NOT CLEAN` and names them, and `clean` is `false`.
The default gate still exits 0 (a CI file that lists every corpus rule id must not be locked out of the
binary), and `--strict` exits 1: a strict gate must not pass on a rule it never checked. They are not folded
into `unmatched_rule_ids`, which means "no checker answers this id at all" (a typo, or a deferred rule). A run
with no `--rule-id` is unchanged: it states what its registered checkers did.

**W.7 Corpus.** All three rules are `enforcement = "architectural"`, `kind = "defect"`, `verification =
"policy"` with internal sources, and each `qualifies` states that the result is a reviewed assessment on top of
a deterministic search, with every exclusion and the 160-candidate limit. Single-source is `max_severity =
"high"`; the other two `"low"`. The pinned layer histogram moves `universal` 133 to 136 (the ui rule keeps
`layer = "universal"`). `domain_to_glob("ui")` is `**/*.{jsx,tsx,ts,js,css}`.

**W.8 Measured.** On this repository's `crates/` (493 files read, 350 indexed, 866 families), release build,
the producer takes 2.0 s: 94 single-source, 7 naming and 0 shared-primitive candidates (no React package), none
over the cap. The 20,000-file budget in section 8.8 is dominated by the detector; the funnel adds one
tokenization per file that belongs to a family.

**W.9 Adversarial review fixes (2026-10-22).** An adversarial review of Wave C found the defects
below; the owner's decisions D1 to D7 (docs/OVERNIGHT_DECISIONS.md, DECISIONS MADE, 2026-10-22)
ruled on them. Each probe scenario is a committed test.
- **D1 No convention row.** Removed (section "Wiring the H rules", W.5). A zero-candidate rule is
  still credited as run in the ledger.
- **D2 Composed-variant guard.** Fires only when the shared imported call dominates (at least 50% of
  the sequence's tokens, or every decision-bearing token inside its arguments). The earlier twin test
  had its own decisions around the helper call and is now a positive; a true composed twin and an
  unrelated-call positive pin the boundary. A consequence worth knowing: a React form pair whose only
  shared imported calls are `useState(` is now also an ARCH-SINGLE-SOURCE-LOGIC-1 candidate (the judge's
  `boilerplate` verdict decides), where before the hook call hid it.
- **Severity.** `high` only when a DECISION token differs (`NamedDisagreement.decision`: an operator or a
  number anywhere in the shared sequence, or a property name inside a decision test, computed by
  `decision_test_mask`). Strings, callees, a property outside any test and inconsistent variable use are
  named in a trailing clause and leave the severity at `medium`. A number outside a test (a rate passed to
  a helper) counts: it is the plainest "the copies disagree" there is.
- **Naming and the UI rule.** When UI-SHARED-PRIMITIVE-1 is armed, ARCH-NAMING-ONE-CONCEPT-1 drops a
  candidate any member of which is a function rendering JSX in a package the UI rule reads (React
  family); JSX in a package the UI rule does not read stays a naming candidate, because nothing else
  would report it.
- **D3 Callee names.** In Exact mode the name of a called function (ECMAScript and Python `call`, a
  `new` constructor, a Rust name directly followed by a parenthesis that is not a declaration) is kept,
  so a changed callee is a named disagreement. Structural mode, which finds families, is unchanged.
  "Apart from local names" is now "apart from variable names", and the claim is checked: free identifiers
  must map one to one between the two copies (`inconsistent_renames`), or the place where they do not is
  a named difference.
- **D4 Overlapping families.** `rank_and_cap` drops a candidate wholly inside a kept candidate of the
  same rule (see section 7).
- **D5 Cache key.** The sorted multiset of every copy's decision tokens (W.5).
- **D6 Concept scrub.** Eight words, whole-phrase rejection (W.5).
- **D7 Cost.** `MAX_LINE_CHARS` is 120; section 7 states the measured bounds and a test asserts them.
- **Transcript.** `clone-judge:<rule>:<repo>` (W.5).
- **UI end to end.** `ui_shared_primitive_ships_a_finding_for_hand_written_components_end_to_end` runs
  a React manifest through `audit_repos_with_llm` with the stub model.

## Owner questions for Wave C

None that change what a rule means. Every choice above is decided and justified in place; the
ones an owner might want to overturn later are called out as one-line reversals (the H-row
confidence grade; the 60/40-token thresholds; property names counting as disagreements).
## UI-REACT-HOOKS-LINT-1: the algorithm and its traps

Built 2026-10-22 on top of `feat/audit-report-export` at `d8afd50`, after tracing the eslint path
end to end (`scan_tools.rs`, `tool_provisioning.rs`, `lib.rs::eslint_floor_category`,
`report_export::corpus_rule_for`), reading `multilang.rs` (Layer 2) and running the live eslint
tier on this machine. This section replaces the one-line "corpus rule + the two tests; detector
already green" in the nine-checker table above, because the premise behind that line was wrong in
one important way.

### 1. What already existed, and the split that follows

The table's "detector already exists" is true of the VIOLATIONS and untrue of the rule id. Two
corpus rules already own them:

- `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` and `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1`
  (`javascript/react/`, `enforcement = "mechanical"`, `verification = "grounded"`, one
  `linter = "react-hooks: ..."` source each). `tool_for_linter` routes them to
  `ScanTool::Eslint`, `selector_for_linter` yields `react-hooks/<rule>`, the provisioned eslint
  (pinned `eslint@^9`, `eslint-plugin-react-hooks@^5`) runs with the BUNDLED flat config plus one
  `--rule {"react-hooks/...": "error"}` per selector, `parse_eslint_json` turns each message into
  a preview `Finding` (rule id is the raw eslint id, line from eslint, severity capped at medium,
  `kind = Defect`, `preview_tool = "eslint"`, detail = the lint message verbatim), and
  `eslint_floor_category` grounds the raw id back to the owning corpus rule for citation,
  remediation and effort. The live test `live_eslint_detects_a_real_exhaustive_deps_violation`
  runs for real on a machine with a cached workspace and passes.
- So "every reported missing dependency or conditional hook is a finding at its line, fix = the
  lint message, instantiated" (work order section 8, first clause) is ALREADY TRUE, and ships
  under the two existing ids. Rule ids are forever (referenced by saved projects), so they cannot
  be retired into a new id, and a third rule that also claimed the eslint selectors would be run
  twice and report every violation under two ids.

Decision: `UI-REACT-HOOKS-LINT-1` owns only the second clause of the work order, the CONFIGURATION
gap ("a repo that does not enable the rules gets one finding for the gap"), and carries NO
`linter` source. `enforcement = "architectural"`, so `group_by_tool` skips it (no ungrouped note,
no second eslint selector). Two pinned tests hold the split: the new rule has no `linter` sources
and `tool_for_rule` is `None`; exactly the two owners claim a `react-hooks/*` selector against the
real corpus; and all three selected together hand eslint each owner once with nothing unrouted. A
live test over the real corpus rules and real eslint shows one finding per violation, none under
the new id.

### 2. Why the configuration gap is a separate fact worth a finding

The scan's eslint run deliberately ignores the repo's own lint setup (bundled config, `--rule`
overrides). That makes it a good audit oracle and a poor statement about the repo: it finds the
hook bugs whether or not the repo's own lint would ever have caught them. The gap finding is the
other half of the picture, "your own lint would not have caught these, and will not catch the next
one". It also names the precondition for write-time enforcement (section 6).

### 3. The algorithm

`crates/checks/src/react_hooks_lint_checker/` (`mod.rs` verdict and message, `config.rs` the
configuration reader, `tests.rs`). One finding per repository at most.

1. Skip every path in an auxiliary folder: test and fixture folders (`is_test_path`), `examples`,
   `example`, `docs`, `doc`, `demo(s)`, `sample(s)`, `playground(s)`, `template(s)`, `sandbox`,
   `vendor`, `third_party`, `node_modules`, `dist`. Their manifests and configs describe something
   other than the project.
2. Parse each remaining `package.json` (BOM tolerated). An unparseable one makes the repository
   unanswered. A manifest declares React when `react` is a key of `dependencies`,
   `devDependencies` or `peerDependencies`. No such manifest: not a React repository, nothing to
   say.
3. Read lint configuration: `.eslintrc`, `.eslintrc.json`, `.eslintrc.jsonc` and a manifest's
   `eslintConfig` field are parsed leniently (comments and trailing commas, as ESLint allows) and
   evaluated; everything else that is lint configuration is "unreadable" (section 4).
4. Evaluate a parsed legacy config as a tree of `Part`s: `Enables`, `DoesNot`, `Unknown`.
   `extends` entries `plugin:react-hooks/*`, `react-app`, `react-app/*`, `next`, `next/*` and
   `airbnb/hooks` enable; `eslint:recommended`, `eslint:all`, `prettier`, `prettier/*`,
   `plugin:prettier/recommended`, `plugin:react/*`, `plugin:@typescript-eslint/*`,
   `plugin:jsx-a11y/*` and `plugin:import/*` are known to contain no hooks rule; any other entry is
   `Unknown`. A `rules` entry for either hooks rule enables unless its level is off (`"off"`, `0`;
   the array form is read by its first element; an unknown shape is `Unknown`). `overrides` are
   evaluated recursively. A key outside `extends`/`rules`/`overrides` and a short inert list
   (`root`, `env`, `parser`, `plugins`, `settings`, ...) is `Unknown`. Combination: any `Enables`
   wins, else any `Unknown` wins, else `DoesNot`.
5. Verdict: some part enables, answer enabled (silent, credited). Else anything unreadable anywhere
   in the repository, answer unanswered (silent, not credited). Else a gap: one finding, medium
   (the corpus ceiling brings it to low), on the first configuration read (named in the message
   with every other one), or, when there is no ESLint configuration at all, on the line of the
   manifest that declares `react`.

### 4. Every trap found, and its guard

- **Double reporting** (section 1). The new rule has no linter source and the split is pinned.
- **Flat config is JavaScript.** `eslint.config.*` is the current ESLint default and computes its
  configuration (imports, spreads, helper calls, `FlatCompat`). Textual search cannot say a rule
  is NOT enabled. Guard: any flat config, `.eslintrc.js`/`.cjs`, or YAML config anywhere makes the
  repository unanswered. Cost, stated plainly: a modern Vite or Next 15 repo with a flat config is
  never reported. That is the largest false-negative surface of this rule. A later tree-sitter
  reader that proves a flat config's `rules` and plugin imports statically would shrink it; it
  needs its own safe twins and is not built.
- **Bundled presets.** `next`, `react-app`, Expo, React Native, Remix, many team configs and
  `eslint-config-airbnb/hooks` include the hooks rules without naming them. Guard: an `extends`
  entry is only "without hooks" if it is on the short known list; every other preset, and every
  relative path, is `Unknown`.
- **Config hidden in the manifest.** A dependency on `eslint-config-*` (except
  `eslint-config-prettier`, which only switches rules off), `@scope/eslint-config[-*]`,
  `eslint-plugin-react-hooks` itself, or another linter (Biome, oxlint, xo, rome, standard,
  neostandard, deno), and a script that gives ESLint a config path (`-c`, `--config`,
  `--no-eslintrc`) or runs Biome/oxlint, each make the repository unanswered.
- **Another linter's config file** (`biome.json(c)`, `.oxlintrc.json`, `oxlint.json`,
  `rome.json`): unanswered. These tools have their own hooks rules.
- **Auxiliary folders** (section 3, step 1): an example app's config neither enables nor hides the
  project's gap.
- **Absence needs a complete scan.** "No configuration enables the rules" is false if the
  configuration is among the files a truncated scan dropped. `RULES_NEEDING_COMPLETE_SCAN` now
  holds both this rule and dead code; the rule is withheld and falls to the incomplete-scan
  disclosure. Pinned end to end with a 20,000-file repo.
- **Ledger honesty.** `config_unsatisfied_for` is true exactly for "unanswered", so the scan ledger
  does not credit a clean detector run for a repository the checker could not read (a flat-config
  repo), while an enabled configuration and a gap are both real, creditable answers. Pinned in the
  checks crate and end to end through `audit_repos`.
- **File feed.** The checker reads dotfiles (`.eslintrc.json`) and `package.json`. The scan feed
  admits dotfiles; the Layer-2 runner and the CI binary load only files matching a checker's
  interest globs, so every shape the checker reads is in `INTEREST_GLOBS` (pinned, plus a
  `camerata-check` CLI test that exercises the dotfile through the real file walk).
- **Partial enablement.** One rule on, the other missing or off: not reported. Reporting it needs a
  judgment about what the team meant to turn off. False negative by design.
- **Version drift.** `eslint-plugin-react-hooks` 6 and 7 add many React Compiler rules to the
  `recommended` preset and 7 cannot be registered by the bundled flat config (the W30 failure).
  The message therefore tells the client to enable the two named rules, not "the recommended
  preset", and Camerata's own tier stays pinned to `^5`.

### 5. Tests, by name

Checker (`react_hooks_lint_checker/tests.rs`, 48): positives for no configuration at all (anchored
on the `react` line), a legacy JSON config without the rules (named, line 1), an `eslintConfig`
field (anchored on the field), JSON with comments and trailing commas, an extensionless
`.eslintrc`, a registered plugin with no rules enabled, both rules off, overrides that do not
enable, `react` in dev and peer dependencies, a monorepo package, several non-enabling configs
(one finding naming each), a BOM, `eslint-config-prettier` alone, and an example-folder config.
Safe twins: enabled by `plugin:react-hooks/recommended` (string and array), by either rule in
`rules` in every severity spelling, by `react-app`, `next`, `next/core-web-vitals`,
`airbnb/hooks`, in an override, and in `eslintConfig`; one enabled outweighs one not; flat configs
in six extensions, `.eslintrc.js/.cjs/.yml/.yaml`, JSON that does not parse, unseen presets,
unknown keys, an unreadable severity, an invalid manifest, ten manifest dependencies and four
scripts that hide the setup, another linter's config files, a Node API, no manifest, React only in
example/fixture/docs/template/node_modules folders, a manifest that only names `react` in a value;
plus the lenient JSON reader, interest globs, determinism, no dashes or internal vocabulary, and
`runs_at_write_time` / `config_unsatisfied_for`.
Wiring: registry (`arch_checker`), write-time runner never reports it while the scan tier does
(`arch_check_runner`), `audit_architectural` seam and complete-scan withholding and ledger credit
(`onboard/architectural.rs`), proposed for React on JS, TS and Next and not for a Node API, Vue
or Python repo, always beside the two owner rules (`onboard/propose.rs`), six end-to-end scans
through `audit_repos` (`tests/react_hooks_lint_scan_e2e.rs`), the CI binary
(`camerata-check/tests/cli_integration.rs`).
Lint violations (`scan_tools.rs`): the no-second-route pin, `parse_eslint_json` on the real
message shapes (line, message, medium, Defect, grounds to an owner), and a LIVE test over the
real corpus rules and real eslint: a conditional hook and a missing dependency each reported once
at their line, a correct file silent, nothing under the new id. It skips (never fails, never
provisions) on a machine without a cached eslint workspace with the hooks plugin; this machine has
one, so it ran.

### 6. The Layer-2 decision

Two different things, two answers.

- The VIOLATIONS are caught at write time already, by the repo's own tooling: the Layer-2
  `JsCheckRunner` runs `npm run lint` in the worktree, which uses the repo's own configuration. It
  catches a conditional hook or a missing dependency if, and only if, the repo's lint enables the
  rules. Camerata's provisioned eslint stays a scan-time preview: it is a network `npm install`
  into the data directory, it ignores the repo's config, and adding it to the per-write loop would
  bounce an agent on code (and noise rules from the bundled baseline) it did not write.
- The GAP checker is a scan and CI rule (`runs_at_write_time = false`). A bounce cannot be
  satisfied by the agent: closing the gap installs a dependency and chooses its version, which
  `ORCH-TRAINING-CUTOFF-1` routes to a human, and a bounce on every React task is about something
  the task did not touch. The gap finding is exactly the check that the `npm run lint`
  precondition holds.

### 6a. Adversarial review round (2026-10-22): what changed

- **The eslint tier leaked its baseline (pre-existing, client-visible).** The bundled config's
  warn-level baseline (`no-console`, `no-unused-vars`, `eqeqeq`, `no-var`, `prefer-const`, ...)
  became preview findings whether or not selected; `eqeqeq`, `no-var` and `prefer-const` even
  grounded to `JAVASCRIPT-STRICT-EQUALITY-1`, `JAVASCRIPT-NO-VAR-1` and `JAVASCRIPT-CONST-DEFAULT-1`
  with citations. Section 6's remark about "noise rules from the bundled baseline" understated it.
  Fixed separately: `keep_selected_eslint_findings` (only rule ids in the runnable selector set),
  `--quiet`, the one error-level baseline rule demoted to warn, an end-to-end test (fails with the
  filter removed). The test that pinned the leak now asserts the filter.
- **More hidden lint setup**: `gatsby` and `react-scripts` (CRA 3.x run `eslint-config-react-app`
  themselves); ESLint given `--config`/`-c`/`--no-eslintrc`/`--rule`/`--plugin`/`--rulesdir`/
  `--resolve-plugins-relative-to` in ANY string of the manifest, not only `scripts` (lint-staged,
  husky, nano-staged, simple-git-hooks).
- **React code gate** (orchestrator decision): the repo must have React CODE, a `.jsx`/`.tsx` file
  or a module importing `react`/`react-dom`, outside auxiliary, test and story folders. A `react`
  peer dependency in a plugin, Storybook-only use, react-email, or a bundler alias with no React
  code is unanswered (silent, recorded as not assessed). The interest globs now include the module
  extensions so the Layer-2 and CI file walks load them. Known cost: a Preact/Solid repo that uses
  `.tsx` and also lists `react` passes the gate.
- **Auxiliary folder names are case-insensitive** (`Docs/`, `Examples/`).
- **Exact eslint pins**: `ESLINT_NPM_PACKAGES` are exact versions, the probe compares the exact
  installed version, and a committed `assets/eslint/package-lock.json` is installed with
  `npm ci --prefer-offline`. W30's earlier "pinned" meant caret majors.

### 7. Known false-negative limits (stated plainly)

Flat configs and every other JavaScript or YAML configuration are never reported; partial
enablement is not reported; severity (`warn` versus `error`) and whether CI runs lint are not
inspected, although the work order lists CI; a rule enabled in an override for a narrow file set
counts as enabled; a monorepo is judged as one repository (one enabling or unreadable config
anywhere silences the whole); a manifest whose lint setup lives in a parent directory of the
scanned root is invisible; a repository linting only through Biome or oxlint is silent, not
checked for their equivalent rules.

## Scope note
Dead-code, one-import-path, shared-constant, state-shape, needless-complexity are **deterministic**
(no model, no token cost). Single-source/naming/shared-primitive add ONE shared clone pass and an
advisory re-rank. React-hooks reuses the existing tool tier. So the only substantial NEW
infrastructure is the clone detector + the literal extractor; everything else composes existing
`extract`/`resolver`/`ArchChecker`/eslint pieces.
