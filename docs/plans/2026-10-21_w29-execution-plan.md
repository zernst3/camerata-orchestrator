# W29 execution plan — the nine consistency rules (2026-10-21)

Turnkey plan for the Phase-4 coverage expansion. Authoritative source:
`~/Documents/Repos/_workspace/brownfield-audits/CORPUS_WORK_ORDER_CONSISTENCY_RULES.md`.
This file translates it into the build order + the per-rule deliverables, grounded against the
current tree. W29 is a multi-detector build (tree-sitter passes + a cross-file clone detector),
so it is sequenced as its own focused effort, not folded into a tail-of-session.

## Dependencies — both now satisfied
- The convention synthesiser fix (`46984f9`) must precede W29 (source §83 wires each consistency
  rule's inferred convention — "money flows through one helper in 9 of 11 places" — into the
  conventions-adopted output). DONE.
- **Rule 8 (UI-REACT-HOOKS-LINT-1) depended on eslint provisioning working. That was the W30
  bug, now FIXED (2026-10-21): versions pinned + the repo-scope canonicalize fix.** So the hooks
  rule is unblocked and is the cheapest of the nine to land (it reuses the eslint tier that
  already produces `react-hooks/exhaustive-deps` — verified in
  `live_eslint_detects_a_real_exhaustive_deps_violation`).

## The standing rule (non-negotiable)
Every rule ships with a POSITIVE unit test AND a SAFE-TWIN test before it ships (source §4).
Nine rules ⇒ eighteen tests minimum. The safe twin per rule is named in the source; do not skip it.

## Wiring checklist (per rule — the orphan-rule trap)
Per `memory/feedback/rulepack_requires_detector_wiring.md`: a corpus rule without detector wiring
is never proposed (orphaned). For EACH inspectable rule, wire all of:
1. the corpus TOML rule (`crates/rules/principles/<domain>/<id>.toml`) — passing every corpus gate
   (kind, inspectable, sources, W25 voice/no-vendor, W26 fix+citation, W32 if security);
2. the detector (see per-rule below) in `crates/checks/src/...`;
3. `propose.rs`: detection + `domains_for_stack`;
4. `domain_to_glob`;
5. a proposed-rules test asserting the rule is actually proposed for its stack.

## Build order (source §86-91) and per-rule deliverables

### Wave A — deterministic, small, highest signal per token
1. **ARCH-ONE-IMPORT-PATH-1** (D) — a symbol has one import path. Detector: tree-sitter import
   extraction + cross-file symbol→path index; flag a symbol imported via >1 path. Safe twin: a
   single `index.ts` barrel re-export (one canonical path). Cheapest deterministic rule; good first.
2. **ARCH-SHARED-CONSTANT-1** (D) — domain literals have one home. Detector: tree-sitter literal
   extraction + cross-file index; flag a domain literal repeated across modules. Safe twin:
   protocol literals + i18n strings (not domain constants). Pairs with rule-2/SINGLE-SOURCE.
3. **ARCH-DEAD-CODE-1** (D) — dead code removed, not kept "just in case". Detector: unreferenced
   exported/module symbols. Safe twin: a public package's exported API (referenced externally,
   not dead).

### Wave B — the tool-oracle rule (now unblocked)
8. **UI-REACT-HOOKS-LINT-1** (D, layer-2) — react-hooks correctness via eslint. Detector: ALREADY
   EXISTS (the eslint tier + `react-hooks/*` selectors). Mostly corpus-rule authoring + wiring +
   the two tests; the live detection path is proven green post-W30. Land early — lowest cost now.

### Wave C — the clone detector (the differentiator) and its reuse
4. **ARCH-SINGLE-SOURCE-LOGIC-1** (H) — logic implemented once. Build the token-based cross-file
   clone-detector pass here (jscpd-class); deterministic filter keeps clone families containing a
   decision (operator / domain literal / conditional); advisory tier judges "business decision?"
   and "do the copies agree?". Headline on disagreement: "The same rule is implemented N ways and
   they do not agree." Safe twin: already-composed variants (not a real clone). This pass is
   REUSED by rules 5 and 6.
5. **ARCH-NAMING-ONE-CONCEPT-1** (H) — one concept, one name. Candidates from the clone detector +
   module-export overlap; judgment = same concept? Safe twin: domain homonyms (same name,
   different concept — legitimately distinct).
6. **UI-SHARED-PRIMITIVE-1** (H) — repeated UI wiring → one component. Clone detector restricted to
   JSX + hook bodies; judgment "same mechanism." Safe twin: already-composed variants.

### Wave D — judgment-with-edge + the last deterministic
7. **UI-STATE-SHAPE-1** (J, D edge) — one status, one variable. Deterministic candidates:
   `useState<boolean>` pairs set in one handler; `useEffect` bodies that only call a setter from
   props/state (derived-state-in-effect). Safe twin: independent booleans (genuinely separate).
9. **STYLE-NEEDLESS-COMPLEXITY-1** (D) — negated ternaries / needless optional props. Detector:
   tree-sitter (the call-site check needs the import graph from rule 1). Safe twin: a ternary that
   is genuinely clearer than the un-negated form.

## Cost note (source §91)
The clone-detector and import-graph passes are one deterministic pass each per repo and cost no
tokens; they give the advisory tier candidates instead of whole files, REDUCING advisory input.

## W32 metadata note (source addendum)
W32 fields (cwe/asvs; superseded_since) are data-only and NOT part of W29. W32(a) CWE is already
shipped; W32(b) was dropped. These consistency rules are arch/style/ui domains — no cwe.
