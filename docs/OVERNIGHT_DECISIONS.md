# Overnight autonomous run — decisions needed + progress log

Started 2026-10-20 (Zach logged off; autonomous continuation of the master sequence).
Zach reviews this in the morning. I commit locally; I cannot push unattended, so every
commit sits on the branch ready for review + push.

## DECISIONS NEEDED (Zach)

(none yet)

## PROGRESS (newest first)

- 2026-10-20: Design docs extended per Zach's mid-session asks: machine-readable exports now
  include a root `manifest.json` (index + sha256 + schema versions + provenance incl.
  client-visible `rules_last_reviewed` from the staleness watermark). README confirmed to carry
  only PARTIAL metadata today (purposes/repos/generated); will mirror the new provenance lines.
  Design committed; BUILD sequenced after W27.
- 2026-10-20: Zach sequencing ruling — Track 2 (exports+manifest, staleness, W23/W24) folds in
  BEFORE the next graded sweep, and the GRADER gets a thoroughness/strictness revision for the
  new product type (package, not just scan+PDF) before the sweep. Grader MODEL stays fixed; the
  RUBRIC gets stricter. Recorded as plan items 19e + 19f, gating item 20 (the sweep).
- 2026-10-20: W27 Phase-A sample-regen blocker RESOLVED. The curated_total 6->5 drift was
  confirmed-correct behavior (SUPABASE-AUTH-EDGE-JWT-1 has confidence=needs-review -> now held,
  not curated). Updated the generator's stale expectation + locked the reconciliation. Generator
  green, regenerates the (gitignored) local sample against the current contract.
  NOTE: the heavy W27 work left = the Typst TEMPLATE rewrite (visual, per 2026-10-16_pdf-design-final.md
  §2-6). That needs your EYE on the rendered PDF, so I am NOT doing it blind overnight; I will do
  the testable/non-visual items first (staleness mechanism, W23/W24) and leave the visual template
  rewrite + the exports-that-depend-on-the-final-xlsx for a session where you can review layout.
- 2026-10-20: Corpus staleness MECHANISM built + tested (commits on branch): freshness.rs data
  layer (overdue/never_confirmed/unparseable, pure date math), confirm mutation (comment-preserving),
  nag formatter, CORPUS_FRESHNESS.toml (empty confirmed, honest), typo-guard integration test.
  189 rules tests pass. Remaining staleness: thin nag-wiring (print at corpus load / digest),
  a confirm CLI command, and the LLM sweep agent (heavy) — consumer/integration layers.
- 2026-10-20: Staleness feature now operator-complete end-to-end: `camerata corpus check`
  (nag) + `camerata corpus confirm <domain> <date>` (advance watermark, validated). Smoke-tested.
  The committed CORPUS_FRESHNESS.toml stays empty/honest (reverted the smoke-test write). Only
  the LLM SWEEP AGENT remains (the finding generator) — deferred: it needs a live model run,
  not suitable for unattended, and is its own focused build.

## OVERNIGHT RUN SUMMARY (2026-10-20 -> 10-21)

All work below is COMMITTED LOCALLY on the branch, green. I cannot push unattended, so
nothing is on origin yet - review `git log` and push in the morning.

### Shipped (green + committed)
1. W25: camerata:allow removed from the product package (fa88f36) + gate tightened.
2. Group 4: 72 mid-word-truncated option ids de-truncated, lockstep refs, generator gate (383ea21).
3. W26 corpus-half: no high+ without fix+citation (155cab1) + effort-tier magnitude gate (7bee157).
4. W32 part (a): CWE metadata schema + cwe on all 14 SEC rules + 2 gates (05d94df). Part (b) dropped.
5. W27 Phase-A unblock: sample-regen fixture reconciled to needs-review-is-held routing (curated 6->5).
6. Corpus staleness feature, END TO END: freshness.rs data+confirm+nag (16 tests) + CORPUS_FRESHNESS.toml
   (honest empty) + typo-guard test + `camerata corpus check`/`confirm` CLI (smoke-tested).
7. Design docs: machine-readable exports (CSV+JSON per sheet + manifest.json), corpus staleness,
   each with a turnkey execution plan. Plan + memory + this log maintained.

### NOT done, and WHY (not blockers - boundaries of safe unattended work)
- **Machine-readable exports + manifest BUILD**: large multi-crate change with real per-sheet
  design (two-table Coverage sheet, per-field JSON key contracts, a new `csv` dep, manifest
  hashing, build_product_zip signature across HTTP+CLI, sample regen, 7 gates). Turnkey plan is
  in docs/design/2026-10-20_machine-readable-exports.md "Execution plan". A focused session.
- **Staleness LLM sweep agent**: needs a live model run; not suitable unattended.
- **W27 PDF template rewrite**: inherently VISUAL (page layout); needs your eye on the rendered
  PDF. The data-layer prerequisite (sample regen) is unblocked.
- **W23 remainder**: LATENT - the cross-tier convention-contradiction path (`any_trace` sees only
  the AI tier) has nothing currently reaching it; fixing it is invasive for zero live benefit.
  Leave recorded until something hits it.
- **W24 remainder (platform-owned-table severity anchor)**: NOT a rule_id-class check like the
  test-coverage anchor; needs the RLS checker to expose "platform-owned table" (auth.*/storage.*)
  as finding metadata + a new Finding field + anchor + fixture - a vertical slice across checks +
  server, too big to land clean blind.
- **W29 (9 consistency rules + clone detector + 18 tests)**: large; focused session.
- **19f grader rubric revision + 20 graded sweep**: live-model + money ($~63) + rubric judgment;
  gated on Track 2 anyway.

### Recommended next session order
Exports+manifest build (turnkey plan ready) -> W24 vertical slice -> W29 -> 19f grader -> 20 sweep.
Plus the gated pre-first-client full staleness sweep before client #1.
