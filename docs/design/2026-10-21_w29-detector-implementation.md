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

- **ARCH-DEAD-CODE-1 — the reachability INVERSION trap (the big one).** `resolver::reachable_from`
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

- **ARCH-SHARED-CONSTANT-1 — the trivial/infrastructure-literal trap.** `0`, `1`, `""`, HTTP status
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

## Scope note
Dead-code, one-import-path, shared-constant, state-shape, needless-complexity are **deterministic**
(no model, no token cost). Single-source/naming/shared-primitive add ONE shared clone pass and an
advisory re-rank. React-hooks reuses the existing tool tier. So the only substantial NEW
infrastructure is the clone detector + the literal extractor; everything else composes existing
`extract`/`resolver`/`ArchChecker`/eslint pieces.
