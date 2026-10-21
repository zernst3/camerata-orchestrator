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
  `domains_for_stack`): a new rule in an ALREADY-mapped domain is proposed automatically — no
  per-rule wiring. Only a NEW domain needs `domains_for_stack`/`domain_to_glob` wiring.

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
| 1 | ARCH-ONE-IMPORT-PATH-1 | D | `imports()` | cross-file `symbol -> {paths}`; flag any symbol with >1 path | one `index.ts` barrel (one canonical path) |
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

## Wiring per checker (the orphan-rule checklist)
For each: (1) the corpus TOML rule in its domain (passing every corpus gate — kind, inspectable,
sources, W25 voice/no-vendor, W26 fix+citation if high+); (2) the `ArchChecker` impl in
`crates/checks/src`; (3) register it in `arch_check_runner`; (4) confirm its domain is already in
`domains_for_stack` (arch/ui/javascript all are) — only a brand-new domain needs
`domains_for_stack`/`domain_to_glob` additions + a proposed-rules test. (5) the positive +
safe-twin tests (the standing rule: 9 rules ⇒ ≥18 tests).

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

- **ARCH-ONE-IMPORT-PATH-1 — the relative-position trap.** `foo` imported as `./utils` from dir A
  and `../a/utils` from dir B are the SAME file via two spellings that are each FORCED by relative
  position — flagging that is a false positive. And two different symbols that happen to share a
  name imported from different modules are NOT the same symbol. **Guard:** key the group on the
  RESOLVED `to_file` (so same-name/different-file never collides), and only flag when the distinct
  specifiers are not merely relative-position variants — i.e. a barrel/alias path coexisting with a
  deep path, or distinct non-relative specifiers for the same target. Pure relative-position
  variance is expected and ignored.

- **ARCH-SHARED-CONSTANT-1 — the trivial/infrastructure-literal trap.** `0`, `1`, `""`, HTTP status
  codes, protocol strings, and i18n keys repeat everywhere legitimately. **Guard:** an allowlist of
  trivial/protocol/i18n shapes; only flag a DOMAIN value repeated across ≥2 modules.

- **The H rules (single-source / naming / shared-primitive) — the candidate-vs-finding line.** The
  clone detector's output is a CANDIDATE list, not findings. Presenting candidates directly would
  flag coincidental structural similarity as a defect. **Guard:** the advisory tier judges "same
  business decision / same concept / same mechanism"; only confirmed candidates become findings.
  The candidate list is never dropped (it is re-ranked), but it is never shipped unjudged.

**Build consequence:** each rule's positive test must be paired with a safe-twin test that exercises
its specific trap above (a dynamically-referenced file for dead-code; a relative-position variant
for one-import-path; an i18n/protocol literal for shared-constant; a coincidental clone for the H
rules). That is what "correct to the QA boundary" means for this phase.

## Scope note
Dead-code, one-import-path, shared-constant, state-shape, needless-complexity are **deterministic**
(no model, no token cost). Single-source/naming/shared-primitive add ONE shared clone pass and an
advisory re-rank. React-hooks reuses the existing tool tier. So the only substantial NEW
infrastructure is the clone detector + the literal extractor; everything else composes existing
`extract`/`resolver`/`ArchChecker`/eslint pieces.
