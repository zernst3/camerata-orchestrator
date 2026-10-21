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

## UPDATE (2026-10-21, resumed on "keep going"): Track 2 exports BUILT
- Machine-readable exports + manifest.json: DONE and wired end-to-end into the product ZIP
  (both HTTP export_product + CLI inspect). 8 json/*.json + 8 csv/*.csv + manifest.json
  (file index + sha256 + schema_version + provenance). Each a serializer over the SAME
  partition_rows pass (parity gate: all-findings.json == findings.json). Sample ZIP regenerated
  with the new files. Gates: json/csv file-set + parity + RFC4180 round-trip + manifest integrity.
- Fixed two PRE-EXISTING masked failures surfaced along the way: W32 Rule test-fixture literals
  (server test compile) and integrity_gate_cmd fixtures (empty_input/model_unrecorded). Both
  were from earlier this-session commits; now green.
- Known remaining red: ONE test, scan_tools::live_eslint_detects_a_real_exhaustive_deps_violation
  — the eslint-provisioning residual (W30), environmental (leaky skip-guard on a half-provisioned
  machine), NOT a regression. Needs the W30 eslint tool-manifest version-pinning.

## W30 eslint residual — DIAGNOSED (2026-10-21), not my change, real product bug
The failing test `live_eslint_detects_a_real_exhaustive_deps_violation` was NOT modified this
session (verified git log -L) and is NOT a regression from this session's code. Root cause chain,
reproduced:
1. ESLINT_NPM_PACKAGES (tool_provisioning.rs:336) pins NO versions -> npm install pulls LATEST ->
   this machine had eslint 10.12.0 + eslint-plugin-react-hooks 7.1.1 (major-version drift).
2. Under eslint 10 / react-hooks 7 the bundled flat config cannot register the plugin -> raw
   eslint errors "could not find plugin react-hooks" -> ZERO findings. (Reproduced directly.)
   => This is a PRODUCT bug: a fresh-provisioned machine silently zeroes react-hooks (likely all
      plugin) preview findings in real client scans, not just the test.
3. Pinning the WORKSPACE to eslint 9.39.5 + react-hooks 5.2.0 makes RAW eslint (Camerata's exact
   --config/--format json/--rule/. invocation) produce the finding correctly. So eslint 9 / rh 5
   is the right pin.
4. BUT: even with 9/5 installed + config intact + raw eslint working, the Rust `run_scan_tools`
   path STILL returns findings=[] (eslint IS attempted; no error note). So there is a SECOND
   factor inside run_scan_tools' provision/invoke/parse path, beyond the version drift, still to
   find. NOT chased (context limit).
FIX (W30, focused next session):
  a. Pin ESLINT_NPM_PACKAGES to versioned specs (eslint@^9, eslint-plugin-react-hooks@^5,
     @typescript-eslint/*@^8, eslint-plugin-jest@^28) AND make eslint_is_provisioned version-aware
     (presence-only probe won't force a drifted workspace to re-pin).
  b. Debug why run_scan_tools gets [] while raw eslint at 9/5 gets the finding (candidates:
     eslint_bin resolution, a transient re-provision, file-selection/extension filtering, or the
     JSON parse path). eslint_args + ESLINT_FORMATTER="json" match the working manual run.
  c. NOTE: I left the local workspace at eslint 9.39.5 / react-hooks 5.2.0 (npm --no-save), which
     is better than the drifted 10/7, but it is not pinned anywhere yet.
Do NOT bandaid the test to skip-on-empty: that would hide the real finding-zeroing in client scans.

## W30 eslint residual — FIXED (2026-10-21), both factors
Investigated + fixed per Zach ("as long as it is investigated and fixed"):
1. VERSION DRIFT (first factor): ESLINT_NPM_PACKAGES was unpinned -> npm pulled eslint 10 +
   react-hooks 7 -> bundled flat config can't register the plugin -> zero findings. FIX: pinned
   to eslint@^9 / @typescript-eslint/*@^8 / react-hooks@^5 / jest@^28, and made the provisioning
   probe VERSION-AWARE so a drifted workspace re-provisions (presence-only check left 10/7 in
   place forever). Helpers + scoped-package parsing unit-tested.
2. REPO-SCOPE SYMLINK BUG (second factor, the real surprise): is_in_repo_scope compared a tool's
   reported finding path against repo_root with a raw textual starts_with. Tools canonicalize
   their paths, so on macOS eslint emits /private/var/folders/... while repo_root is the
   symlinked /var/folders/... -> every finding judged "outside the repo" and DROPPED, zeroing
   the whole tier. A real latent product bug for ANY repo under a symlinked path on macOS, not
   just the test. FIX: canonicalize both sides before the containment check, fall back to raw on
   failure (synthetic test paths unchanged).
Result: live_eslint_detects_a_real_exhaustive_deps_violation passes; all scan_tools (73) +
tool_provisioning (31) tests green. Did NOT bandaid the test. Full-suite confirmation pending.
