# Master sequence — everything pending before the next graded sweep

**This is the item ledger** (what is pending, what is done). The single "where to resume" pointer is
the RESUME HERE banner at the top of `docs/OVERNIGHT_DECISIONS.md`; this doc is what it points into.
Written 2026-10-19 at the owner's request: *"I just want to be
sure all the necessary work that is pending is KNOWN about. We will just work through
everything in sequence until we are ready to start the cycle again."*

> ### Reconciliation against the commit log (2026-10-21, after the Fable audit)
> The original line here said "nothing below has been started unless marked DONE"; by 10-21 much
> had landed without being marked. Status per item, verified against `git log`:
>
> | item | status | commits |
> |---|---|---|
> | 1 W19 gate | DONE | `f785cad` |
> | 2 W21 artifact gate | DONE | `b632df7` |
> | 3 model pinning | DONE | `36d5799` |
> | 4 W22 kind integrity | DONE | `436a5a3` |
> | 5 Corpus Group 3 (A5.7 roster) | PARTIAL: 2 of 4 | `84e8f4f`; the two needing option rewrites remain |
> | 6 W23 | PARTIAL | `d88872f`; cross-tier remainder recorded as latent in OVERNIGHT_DECISIONS |
> | 7 W24 | PARTIAL, then REFRAMED | `14fc75e`, `ac7359f`, `89ea732`; platform-owned remainder became W33 (approved + built, `69d40e1`..`dff2ae9`) |
> | 8 W28 | DONE | `c9ca239` |
> | 9 W31 | DONE | `865e66e` |
> | 10 W25 rendered artifacts | DONE (camerata:allow + product name) | `fa88f36`, `7ee10a6` |
> | 11 corpus wording batches | PARTIAL | supabase `12e8f69`; non-supabase directives `2a2d1e9`; process/integration `b45258a`; source titles `39f6f9a`. Not yet: slogan labels, hard-newline directives, ~54 template questions, upside-first `why`s, api-layer payment directives (verify each before starting) |
> | 12 Corpus Group 1 | OPEN | |
> | 13 Group 4 | DONE | `383ea21` |
> | 14 W26 | corpus half DONE | `155cab1`, `7bee157`; runtime half rides with W27 |
> | 15 W27 PDF rebuild | Phase-A unblocked (`f6df35d`); template rewrite OPEN (needs Zach's eye) | |
> | 16 W32 | (a) DONE `05d94df`; (b) dropped, see note | |
> | 16b exports + manifest | BUILT (`524a29b`, `54ac30f`, `9209482`, `b2d8524`, `4177244`); 3 of 7 design gates still missing, see 21 | |
> | 16c corpus staleness | BUILT incl. CLI; LLM sweep agent + load/digest nag OPEN | `41fcb1d` et al. |
> | 17 W29 | NEXT. Design corrected 2026-10-21 (`2f45aae`) | |
> | 18 W30 residuals | eslint pin: caret majors `f52fb48`, EXACT versions + committed lockfile (`npm ci`) 2026-10-22; other residuals OPEN | |
> | 19 engine/doc items | `Rejected:` strip DONE `a140459`; `resolve_fix` comment DONE (correction note in place); rest OPEN | |
> | 19a env-var flake | OPEN (40 env mutations, 3 `#[serial]`) | |
> | 19b eslint residual | DONE: real cause was version drift + macOS symlink canonicalization | `f52fb48`, `b8941a9`, `b76e8b1` |
> | 19c supabase option rewrite | DONE | `910697d` |
> | 19d per-branch severity | OPEN (schema question) | |
> | 19e / 19f / 20 | OPEN, in order | |
> | 21 audit follow-ups (new) | OPEN, see section 2b | |

Source documents, none superseded:
- `2026-10-16_comprehensive-remediation-design.md` — W19-W32 definitions, the three waves
- `2026-10-18_w19-regression-evidence.md` — the retraction and the controlled re-run result
- `2026-10-18_w19-controlled-rerun-preregistration.md` — the experiment and its verdict
- `2026-10-19_corpus-audit-work-order.md` — all 417 rules audited, ~220 edits enumerated
- `2026-10-16_pdf-design-final.md` — the ~16-page PDF rebuild
- `docs/RULE_AUTHORING.md` — the prescriptive standard (rewritten this session)

---

## 0. What is already DONE

| item | commit | note |
|---|---|---|
| W20 scan scope, both halves | `bced2d3`, `dcb9f9d` | engine neither reads nor writes bookkeeping in the client's repo; confirmed in a live run |
| W19 restoration | — | **scope is ZERO.** The controlled re-run returned 4/4 target rules and 4/4 variance controls. There was never a detection regression; it was the engine auditing its own 212KB cache |
| W21 path sources | `f03575f` | 32 absolute auditor paths + 10 prose-as-path rows, fixed at source |
| Convention synthesiser (part of W22) | `46984f9` | `currently_followed` — a convention row now needs the model to affirm the convention is followed |
| `ARCH-HOT-READ-CACHE-1` Fix lines (a W26 instance) | `d0ea2ea` | three defect Fix lines told the client to implement the fault |
| The authoring standard | `fcbab7c` → `c97780c` | audience taxonomy, voice standards, LABEL-1..4, field interactions, three self-corrections |
| Disk: profile + weekly `cargo-sweep` | `c65cdd8` | 222 GiB incident |

**Five code/corpus fixes, sixteen docs commits.** That ratio is accurate and worth knowing: a
large part of this stretch was establishing what correct means before changing anything.

---

## 1. Decisions already made — do not re-litigate

| # | decision | date |
|---|---|---|
| D1 | Semantic-tier regression checking is **advisory-only** (Option A), not N-run | 10-18 |
| D2 | The gate watches **INPUT invariants** as the failing layer; output diffs are secondary | 10-18 |
| D3 | R-ids are **retired, never reused**; survivors never renumber; gaps named by a withdrawal line | 10-17 |
| D4 | The PDF's reader is **non-technical**; the workbook is the engineer's artifact | 10-17 |
| D5 | Untriaged `could_be_intentional` rows **ship as "your call"**; persistence deferred | 10-16 |
| D6 | W32 rule metadata (`cwe`/`asvs`, `superseded_since`) **approved** | 10-18 |
| D7 | Strip the `Rejected:` token **at render**, not by corpus migration | 10-19 |
| D8 | Labels judged on **verifiability, not grammar** (LABEL-4) | 10-19 |
| D9 | **Fix the truncated option ids** — no client scan has run, so no selections to migrate | 10-19 |
| D10 | **Fix the mis-declared layering rules** (corpus Group 2) | 10-19 |
| D11 | **A5.7 hold LIFTED** for the rostered rules (corpus Group 3) | 10-19 |
| D12 | One graded sweep **after all waves**, not per wave | 10-16 |

---

## 2. THE SEQUENCE

Ordering principle, unchanged from the original design: **restore what broke before adding
anything, and fix what the client sees before what only we see.** The corpus work is slotted
in rather than parked, per the owner, but it is slotted where its dependencies allow.

> ### STANDING STEP — every phase ends by updating the documentation (owner directive, 2026-10-21)
> No phase is done until the docs reflect it. At minimum: refresh **`docs/OVERVIEW.md`** (the
> high-level human map — keep its crate map / pipeline / concept axes current and bump its "last
> refreshed" date) AND the relevant DEEP doc (`TECHNICAL.md` for code/crate changes,
> `ARCHITECTURE.md` for layering, `RULE_AUTHORING.md`/`RULES_REFERENCE.md` for corpus-schema
> changes). The rationale: the code is too large to understand by reading it, so the high-level
> docs are the only human-scale view — a stale one is actively misleading. This step is listed
> explicitly on each phase below; treat it as a gate on "phase complete," not an afterthought.

### PHASE 1 — finish Wave 1 (two gates)
_Doc step (standing): update `OVERVIEW.md` + the relevant deep doc before marking this phase done._

1. **W19 gate.** Input invariants as the failing layer: no engine-authored bytes in the
   digest, file count sane, `audit_model` recorded. Semantic-tier output diffs advisory (D1).
   *Note: the defect this gate most needed to catch was an INPUT defect. An output-diffing
   floor would have reported twelve rule regressions and sent someone to read twelve rules —
   which is exactly what happened to me, twice.*
2. **W21 artifact gate.** No absolute path and no prose-shaped path reaches an export. Needs a
   seam: `integrity_gate::evaluate` is pure over a `ScanLedger` and has no row set to inspect.
3. **Model pinning becomes standard** for any measured run. Precondition for the sweep, not a
   nicety — the whole before/after comparison this week was confounded by an unrecorded model.

### PHASE 2 — Wave 2, the mid-layer (where the all-five-repo defects live)
_Doc step (standing): update `OVERVIEW.md` + the relevant deep doc before marking this phase done._

4. **W22 kind integrity**, including **corpus Group 2** (7 layering rules declared `decision`,
   so their fat-controller option cannot be flagged; a repo that genuinely puts logic in
   controllers is told it "consistently follows" that). Two sibling rules already have the
   correct shape as a template. **This is kind integrity, not wording — it belongs here.**
5. **Corpus Group 3** — the 4 A5.7-rostered rules (hold lifted, D11). Roster edits follow the
   worked precedent already in `w14_corpus_schema_gates.rs`. Here because they are also kind
   changes.
6. **W23 `not_assessed` honesty.** A rule contradicted by another row in the same export
   becomes `not_assessed`, never Clean and never "What's healthy". Depends on W22.
7. **W24 merge and ranking.** Same-root-cause merge; severity anchors; top-3 selects three
   distinct root causes. Depends on W22.
8. **W28 field agreement** at the export boundary (the 38 baseline-suppressed-but-Open rows).
9. **W31 `could_be_intentional`** flag, severity cap, curator disposition, its own
   client-facing section (D5).

### PHASE 3 — Wave 3, the deliverable
_Doc step (standing): update `OVERVIEW.md` + the relevant deep doc before marking this phase done._

10. **W25 client-facing text hygiene — rendered artifacts.** Pipeline vocabulary, retry
    narration, CI-state text, calibration notes, placeholder tokens.
11. **Corpus wording batches (11 of them, ~167 fields).** The upstream half of W25. Fully
    enumerated in the work order with proposed replacement text. Delegable in parallel.
    - supabase directives (35) · supabase labels (~26) · 31 upside-first `violates` `why`s ·
      `integration/` (3) · `process/` (4) · `universal/` gate-mechanics sentences ·
      5 api-layer payment directives · ~15 slogan labels · 6 graded source titles ·
      13 hard-newline directives · ~54 template `[decision].question`s
    - **Fix the guide's orientation snippet FIRST** or the template questions regrow.
    - **Caveat:** severity-transcription comments quote directive prose these rewrites delete.
12. **Corpus Group 1** — 17 inert `violates` options describing Camerata's detector design.
    Verified runtime-inert; safe anywhere, parked here to batch with the wording.
13. **Corpus Group 4** — truncated option ids. ✅ DONE (`383ea21`). 72 mid-word-truncated ids
    de-truncated to full/clean-trimmed label slugs; 55 `[decision].default` + only 4 remaining
    hardcoded Rust refs updated in lockstep (b4fb2ab had already cut the 18 to 4); zero persisted
    selections so no migration. Generator fixed mechanically: `w_group4_no_truncated_option_ids`
    gate fails any mid-word-prefix id, so the 48-char cap cannot recur. Boundary-complete short
    ids left alone.
14. **W26 fix and estimate quality.** Corpus-static half DONE (`155cab1`, `7bee157`):
    - ✅ **No high+ row without a fix and a citation** — `w26_high_plus_fix_and_citation`, with a
      faithful `rule_can_emit_high_plus` (floor / violates-option / high ceiling / defect-kind;
      a plain decision rule whose deviations clamp to medium is excluded). One real gap fixed:
      GO-HANDLER-SERVICE-REPOSITORY-1 got its three authorities as `[[sources]]`.
    - ✅ **Magnitude sanity** — `options_with_invalid_effort` locks effort to low/medium/high
      (0 invalid). Function `effort_hours_bounds` already monotonic. High+ options without an
      authored effort are left alone — effort is optional by design, "not yet estimated" is
      honest, forcing a number is fabrication.
    - ⏳ **Contradiction gate, placeholder gate, floor-rows-always-estimated** — RUNTIME/export
      concerns (they operate on a rendered row with its resolved option + fix text), so they
      move to W27 where the export path is reopened. A runtime non-contradiction self-check on
      `fix_specific` already exists; the one authored-corpus instance (hot-read-cache) was fixed
      in `d0ea2ea`. Predicate-aware fix TEMPLATES is an authoring-ergonomics item, deferred.
15. **W27 PDF rebuild** per `2026-10-16_pdf-design-final.md`. Last, because restructuring a
    renderer over wrong data bakes the wrong shape in. Carries:
    - R-id persistence at first export + read-back (D3). **A recomputed counter renumbers by
      construction and only diverges on the SECOND export** — the test must export, kill a
      middle finding, re-export, assert.
    - The withdrawal line, data-driven, with the retired set on the wire.
    - `sample-report/data.json` predates the `kind` field and needs regenerating.
    - **Phase-A entry finding (2026-10-20):** running the regenerator
      (`cargo test -p camerata-server --test generate_sample_report -- --ignored`) now panics on
      its own sanity assertion `executive_summary.curated_total == 6` (actual **5**) BEFORE it
      writes `data.json` (line 388 < 395), so the sample is still the stale Sep-22 file. One of
      the six curated findings reclassified out of the open-action set since Sep 22 — accumulated
      drift from prior corpus work (the supabase 14-rule rewrites in `910697d`, kind
      reclassifications), NOT this turn's edits (Group 4 touched no SUPABASE-/SEC- rule; `cwe`
      and remediation text don't change classification). First W27 task: identify which finding
      dropped, confirm 5 is the correct current behaviour, and update the fixture expectation as
      part of the regen — do NOT flip 6→5 blind.
16. **W32 rule metadata** (D6). Part (a) DONE (`05d94df`): schema added (`cwe`/`asvs`/
    `superseded_since`/`current_recommendation` on RuleToml+Rule, both load sites, unknown-field
    allowlist, RULE_AUTHORING), `cwe` authored on all 14 SEC- rules from their own source CWE
    citations, two gates (`w32_rule_metadata`: every SEC- rule has a code + well-formedness).
    **Part (b) DROPPED (Zach, 2026-10-20):** `superseded_since` was a nice-to-have enrichment,
    not load-bearing; no clients, build phase, no rule is a confirmed-stale pattern worth
    flagging. The schema field stays (harmless, zero rules use it); we revisit only if a
    specific rule is ever found to teach something the ecosystem abandoned.

16b. ✅ DONE **Machine-readable exports (CSV + JSON per dataset) in the product ZIP** (Zach, 2026-10-20).
    Every xlsx DATA worksheet (All Findings, Deviations, Conventions, Dependencies, False
    Positives, Coverage; NOT Index or the per-category duplicates) emitted as BOTH a CSV and a
    JSON, in nested `csv/` and `json/` folders inside the export ZIP, PLUS a root `manifest.json` (file index + per-file sha256 + schema versions + provenance: commit, engine version, corpus-content digest, and the client-visible rules-last-reviewed staleness date), as additional serializers
    over the SAME row structs the xlsx builds from (never a new data source). Closes the
    asymmetry where `findings.json` mirrored only All Findings. `findings.json` stays at root as
    the provenance-wrapped headline view. Design: `docs/design/2026-10-20_machine-readable-exports.md`.
    **Built before W27** (the design's Sequencing escape hatch). It ships EIGHT datasets, not the
    six listed above. When W27 reshapes worksheets, re-check the set and the per-sheet gates.

16c. **Corpus staleness check** (Zach, 2026-10-20). A `crates/rules/CORPUS_FRESHNESS.toml`
    watermark (per-domain last-CONFIRMED-current date), a non-fatal nag when a domain is past
    its `warn_after_days` (quarterly default), a human confirm-then-watermark flow (running a
    sweep never advances it), and an LLM sweep that checks each rule's pattern against current
    ecosystem guidance. This is the process that ever sets W32's `superseded_since` /
    `current_recommendation`. **Gated pre-first-client action: a full sweep + confirm of every
    domain** before the first paid deliverable. Design:
    `docs/design/2026-10-20_corpus-staleness-check.md`.

### PHASE 4 — W29, coverage expansion
_Doc step (standing): update `OVERVIEW.md` + the relevant deep doc before marking this phase done._

> Turnkey build plan: `docs/plans/2026-10-21_w29-execution-plan.md` (per-rule detector+wiring+tests, build order; rule-8 eslint blocker CLEARED by the W30 fix).

> **W24 reframed (2026-10-21):** the platform-owned-table item is the first instance of a NEW FINDING CLASS — real but not client-fixable. Design (Zach signed off 2026-10-21; built as W33): a structured `ownership` axis + its own PDF section + budget exclusion, which SUPERSEDES the original severity-anchor idea (do not fake-cap severity; route by ownership). See `docs/design/2026-10-21_unfixable-findings-class.md`. (per-rule detector+wiring+tests, build order; rule-8 eslint blocker CLEARED by the W30 fix).

17. **The nine consistency rules + clone detector + 18 safe-twin tests.** TWO hard
    dependencies, both now satisfiable: W19's gate must exist first, and the convention
    synthesiser must be fixed (done, `46984f9`) because source §83 wires all nine straight
    into it.
    - Order within: dead-code / shared-constant / one-import-path → hooks lint (needs the
      eslint fix) → single-source-logic (builds the clone detector) → naming / shared-primitive
      / state-shape → needless-complexity.

### PHASE 5 — residuals, then measure
_Doc step (standing): update `OVERVIEW.md` + the relevant deep doc before marking this phase done._

18. **W30 residuals.** PIN eslint versions in the tool manifest (primary) + ETARGET retry;
    §9.1 zero-finding rule surface so `RuleAlternativesPanel` can go; Conventions tab count;
    `is_code_auditable_rule` gating on `inspectable` rather than a name prefix; D6/D7 severity
    **ceiling** tables still using raw `contains` (the same shape as the fixed D5 floor bug,
    where `urls` matched `rls`); R1 `cargo fmt`; R2.
19d. **Per-branch severity has no structural home (found 2026-10-20, W26 item).**

    `SEC-NO-SENSITIVE-DATA-IN-LOGS-1` wants two severities by branch — a logged credential is
    HIGH (a live, reusable secret), logged regulated PII is MEDIUM (a serious exposure but not
    itself a credential). `min_severity`/`max_severity` are whole-rule bounds, so only the
    floor (MEDIUM) is structural; the HIGH-for-credentials branch is model-proposed with
    nothing enforcing it. The W25 directive revoicing removed the literal "HIGH finding" /
    "MEDIUM finding" words (severity words never belong in a client-emitted directive) and
    kept the substance as a blast-radius sentence, but the KEYING is still unenforced. This is
    the general shape of a W26 gap: a rule whose branches differ in severity cannot say so in
    the schema. If per-branch severity matters for more than this one rule, it is a schema
    question (a per-option or per-predicate severity), not a wording fix. Also: this rule has
    no `qualifies` field at all, so its detection vocabulary had nowhere to move and stayed in
    the directive as where-to-look specifics (permitted by the guide).

19c. **[DONE 2026-10-20, commit 910697d] Fourteen supabase rules needed an OPTION REWRITE, not a revoicing.**

    The supabase revoicing pass (`12e8f69`) deliberately left fourteen `violates` options
    alone, and the reasoning should not be rediscovered: they describe Camerata's own REPORT
    POLICY rather than a state of the client's code — "Flag every SECURITY DEFINER function
    regardless of grants", "Grep each migration file independently", "Do not author a
    storage-specific rule", "Skip reporting on buckets with safe-sounding names".

    Wording cannot fix them. LABEL-2 requires the label to name the bad STATE, and there is no
    bad client state in these options to name. Re-voicing the label while the `why` still
    argues about scanner behaviour would ship an option whose two client-facing halves
    disagree — worse than the current honest tool voice.

    They need the same judgement-per-rule treatment `SUPABASE-AUTH-GETSESSION-SERVER-1` and
    `SUPABASE-RLS-INITPLAN-1` received in `7cdaa9c`, new option ids included. Note those two
    resolved DIFFERENTLY from each other (one a `defect`, one staying a `decision` with no
    `violates`), so this is fourteen decisions, not one batch.

    `AUTH-EDGE-JWT-1`, `FUNC-DEFINER-CROSS-TENANT-READ-1`, `FUNC-DYNAMIC-SQL-INJECTION-1`,
    `FUNC-PRIVILEGED-NO-AUTHZ-1`, `FUNC-SEARCH-PATH-1`, `EXPOSURE-MATVIEW-1`,
    `EXPOSURE-SCHEMAS-1`, `KEY-SERVICE-ROLE-CLIENT-1`, `RLS-DISABLED-NOT-RESTORED-1`,
    `RLS-ENABLED-1`, `RLS-PERMISSIVE-TRUE-1`, `RLS-POLICY-DISABLED-1`,
    `STORAGE-OBJECT-POLICY-1`, `STORAGE-PUBLIC-BUCKET-1`.

    Their CONFORMING options are already rewritten, so the emitted governance and the scan
    prompt are both correct today; only the rejected-option half is outstanding.

    Also noted in the same pass: `SUPABASE-STORAGE-SIGNED-URL-1` has NO severity bounds at
    all, and a relative severity ordering that survives only as prose in `[decision].why`. It
    is the one place in the pack where a severity claim has no structured home.

19b. **[DONE 2026-10-21; the narrative below is SUPERSEDED.]** The real root cause had two factors:
    eslint plugin version drift (fixed by pinning, `f52fb48`) and macOS symlink canonicalization in
    the repo-scope check (`b8941a9`). The "selector-to-package mapping" suspicion below was wrong.
    Kept for the record of disproved theories.

    **The eslint residual, narrowed 2026-10-20 — it is NOT provisioning.**

    `live_eslint_detects_a_real_exhaustive_deps_violation` has been the single failing test
    all session. Established by direct experiment:

    - All five packages ARE provisioned (eslint 10.12.0, @typescript-eslint 8.71.2,
      react-hooks 7.1.1, jest 29.17.0) and the bundled flat config IS copied into the
      workspace, byte-identical to `assets/eslint/camerata.config.mjs`.
    - The config loads, imports `eslint-plugin-react-hooks` successfully, and registers it.
    - **Running Camerata's EXACT argv by hand produces the expected finding**: `--config
      <workspace>/camerata.config.mjs --rule '{"react-hooks/exhaustive-deps":"error"}'
      --format json .` with cwd = the scanned repo returns
      `['no-console', 'no-unused-vars', 'react-hooks/exhaustive-deps']`.

    So the tooling is healthy and the failure is inside the Rust path — most likely the
    selector-to-package mapping (`scan_tools.rs:296-301` / `:337`) leaving `runnable` empty,
    so eslint runs with no `--rule` overrides. NOT YET CONFIRMED; that is the next thing to
    check.

    **Two theories I pursued and disproved, recorded so nobody re-runs them.** (1) "ESLint 10
    ignores files outside the config's base path" — real message, real behaviour, but only
    when an ABSOLUTE target path is passed, and Camerata passes `.`. (2) "The config's
    `catch { reactHooksPlugin = undefined }` silently swallows a failed plugin import" — the
    swallow is real and worth hardening, but the import succeeds in the real workspace. The
    first theory survived twenty minutes because I read a truncated `head -c 600` of the JSON
    that cut off before the react-hooks message. **Do not diagnose this one from truncated
    output.**

    Separately and still wanted regardless: PIN the eslint package versions in the tool
    manifest. `package.json` currently carries `"eslint":"^10.12.0"`, so an unpinned
    reinstall can move a major version under the engine with no signal.

19a. **Test-isolation residual (found 2026-10-20).**
    `rescan_alternatives_blocked_when_project_is_api_backend_with_no_key` fails intermittently
    under the parallel harness because it calls `std::env::remove_var("ANTHROPIC_API_KEY")`.
    There are **40 process-global env mutations in `crates/server/src/lib.rs` alone**. Zach
    flagged this class earlier in the effort ("three tests mutating one process-global env var
    under a parallel harness - this should be fixed if it wasn't already"); the three named
    then were fixed, the class was not. Verified pre-existing by stashing the W31 change and
    reproducing on baseline. The fix is a serial-execution guard or a per-test env sandbox, not
    another one-off.

19. **Engine/doc items surfaced by the corpus audit:**
    - strip `Rejected:` at render (D7)
    - `report_export.rs:2042-2050` — `resolve_fix` doc comment still carries the repudiated
      "internal, agent-facing instructions" framing
    - `RULES_REFERENCE.md` §5.3 — same stale sentence; an author reading only §5.3 reproduces
      the original defect
    - `onboard.rs:1366-1373` and `:1425-1427` — two comments claim governance/process families
      are dropped from the code audit; they are not
    - three field-interaction rules to ADD to the guide: `enforcement` × multi-option (the most
      consequential and entirely absent), `remediation` × `violates`, `inspectable` scope
      (working tree vs VCS metadata)
    - `supabase-key-service-role-client-1` has no severity bound but says "critical" twice
    - 3 inert `deviation_ceiling` fields on `defect`-kind rules
    - `proc-pr-concurrency-1` is `grounded` with one non-http source → resolves zero citations
    - `javascript-angular-subscription-cleanup-1` flags a state its own conforming option permits
    - `testing-one-assertion-per-test-1 / strict-one-physical-assert` — `violates` on an
      over-application, not a fault
    - `proc-permission-config-1` — `inspectable = false` with `kind = "defect"` and two
      `violates` options is dead machinery; one of the two declarations is wrong
19e. **Fold in Track 2 BEFORE the sweep (Zach, 2026-10-20).** The graded sweep does not run until
    the client-readiness items are in the product: machine-readable exports + `manifest.json`
    (`16b`), corpus staleness check (`16c`), and the W23/W24 latent remainders. Rationale: the
    sweep must grade the ACTUAL shipped product, and the product is now bigger than scan+PDF.

19f. **Revise the grader for the new product type BEFORE the sweep (Zach, 2026-10-20).** The
    grader will "likely need to be more thorough and more strict based on the new product type."
    The product is no longer just a scan + narrative PDF; it is a package (PDF + xlsx + per-set
    CSV/JSON + manifest + provenance + client-visible staleness date). The grader must assess
    those surfaces too - manifest integrity/accuracy, cross-artifact agreement, provenance
    correctness, the exports' completeness - not only finding recall/precision. Keep the grader
    MODEL fixed across the before/after (per the refinement-loop rule); it is the RUBRIC that
    gets stricter, not the judge. Scope this as its own step before item 20.

20. **ONE graded sweep, all five repos** (D12), AFTER 19e + 19f. **Hearth MUST be re-graded** —
    its 30/46 is invalid, it measured a run whose prompt was 63% engine JSON. All five answer
    keys exist. Expect roughly $63 and most of a day.

---

### 2b. Item 21: follow-ups from the 2026-10-21 Fable audit (nothing dropped)

- **CI gate.** HEAD failed fmt (220 files), clippy (11 `unwrap_used` denials + ~64 warnings), and
  `test --locked` (lockfile, fixed `4eb3276`). CI never ran on feature branches. Being fixed:
  rustfmt config + one format commit, all clippy diagnostics, feature-branch push trigger.
- **Export gates 2, 6 (corrupted-byte clause), 7** from `2026-10-20_machine-readable-exports.md`
  §Testing are unbuilt. Also update that design's Status line and dataset count (8).
- **Staleness nag** at corpus load and in the digest (only `camerata corpus check` exists).
- **`is_in_repo_scope` symlink fix** has no direct unit test with a real symlinked root.
- **Ownership:** `Generated`/`Vendored` have no producer; `PlatformManaged` dormant by design.
- **C5-6** is tracked only in an `#[ignore]` reason (`report_export.rs`); give it a ledger line.
- ~~W29 corpus rules need Zach's go~~ GIVEN 2026-10-21 (all nine).
- **`domain_to_glob("ui")` excludes `.jsx`**; fix with the first `ui` W29 rule.
- **Memory location:** Camerata memory lives under the agora-mono project dir, so a session
  started in this repo does not auto-load it.
- **`stash@{0}`** (09-28 dead-P1 partial): checked 2026-10-21, fully superseded (every function
  and test it adds exists at HEAD; the rest is clippy --fix churn). DROPPED 2026-10-21 on Zach's OK.

- **Live severity is unclamped** (Fable review 2026-10-21, confirmed): `onboard/architectural.rs`
  copies a checker's severity into the live Finding; corpus `max_severity`/`min_severity`/
  `deviation_ceiling` are applied only at export (`report_export.rs` `build_report_json`,
  `xlsx_export.rs`). The app's triage view and the billed PDF can disagree for all 12 rules
  that declare a ceiling. Fix: one `apply_corpus_severity_bounds` after `classify_repo_findings`
  in `onboard.rs`, export clamps kept as idempotent backstops, plus a live-report ceiling test.
- **ONE-IMPORT-PATH known false negative:** the union barrel exclusion hides intra-package splits
  in monorepos where every package has a root barrel. A per-chain exclusion that keeps the
  barrel-internal safe twin silent is an open design question.
- ~~Owner confirmation wanted~~ CONFIRMED 2026-10-21 (Zach): an origin-plus-index-barrel split
  IS flagged; "that is the rule working as intended." Two homes for one symbol is the defect
  whether or not the second home is an index file.

- **Applied-rule drift has no baseline until first "accept"** (schema build, 2026-10-21): the
  applied hash/version is written only by `accept_rule_drift`; `save_armed_to_project` creates
  selections with no baseline and `upsert_base_rules` replaces the list, erasing baselines on
  re-apply. So "a rule you applied changed upstream" silently covers nothing for a freshly applied
  rule. The June feature spec intends detection from apply time. Decision (Opus, per the
  never-defer-hardening directive): baseline at apply time and preserve baselines across
  re-apply. Queued.

- **CI gate binary (`camerata-check`) ignores corpus severity bounds** (found 2026-10-21 merging
  the W29 detectors): it reported ARCH-DEAD-CODE-1 at `medium` though the corpus ceiling is `low`,
  so the repo-installed gate disagrees with the scan and the client artifacts. Route its findings
  through the same bounds function (`apply_corpus_severity_bounds`, now in camerata-server) or
  hoist that function into `camerata-rules` so both crates share it. Also decide, per the
  `stages` target design, whether low-severity consistency rules should HARD-FAIL a client's CI
  build or report advisory; today any deterministic finding fails `camerata-check`.

- **Model registry is stale** (found by the Wave C spec, 2026-10-22): `crates/llm/src/model_registry.rs`
  has no entry for the current fast tier (Haiku 5.5) and its Sonnet prices may be outdated (a test
  pins $3/$15). Verify every price against Anthropic's official pricing page (never from memory)
  and update the registry + its tests; the Wave C clone judge should default to the current
  cheapest capable tier once the registry is correct.
- **Every extractor re-parses each file** (5 to 6 parses per file across imports/functions/
  exports/literals/computed_loads/clone tokens). A shared per-file parse cache is a performance
  win; not a defect, ledgered for after V1 unless scan time on a large repo becomes a problem.

- **Mutation-test gaps in `camerata-checks` (overnight run 2026-10-22/23, cargo-mutants v27.1.0,
  6h, no model tokens).** 6,380 mutants: 2,062 caught, 44 missed, 93 timeouts (effectively
  caught), 4,181 unviable. Fault-catch rate 97.9% (caught / caught+missed), counting the checks
  crate's own tests only. Full list: `mutants.out/missed.txt` (gitignored; rerun to regenerate).
  **MAJOR, V1 (after the two client-trust blockers): 26 misses in client-facing checkers.**
  query_grammar_injection (10: split_top_level_plus, classify_expr, extract_call_inner,
  split_top_level_args, find_first_needle, extract_property_value), weak_randomness (7:
  forward_taint_identifiers, find_word, find_prng_call, assignment_target, strip_comments),
  handler_no_db contains_ident_call (2), resource_lifecycle is_true_literal + pat_ident Pat::Type
  arm (2), ui_dates strip_comments (1), one_import_path Model::canonicalize `!` (1),
  shared_constant call_receiver boundary (1), and interest_globs emptied without detection on 4
  checkers (python_testing, ui_dates, weak_randomness, query_grammar_injection; a checker could
  silently stop applying). For each: decide equivalent-vs-real, add a killing test for every real
  one, rerun `cargo mutants --file <path>` to confirm caught.
  **MINOR, V2: 18 misses.** Build-runner disk headroom guard (9; no test proves it refuses on low
  disk), Layer-2 runners (PythonCheckRunner::check -> default, RustCheckRunner ==, FmtCheckRunner
  heartbeat, diagnostics_for `||`; fold into the V2 dev-time parity audit), cosmetic
  (LayerConflict Display, describe_path boundary, clone_rules skip_count).

### 2c. Item 22: implementer-handoff items not yet built (added 2026-10-24, Zach)

From `_workspace/brownfield-audits/IMPLEMENTER_HANDOFF_2026-10-24.md`. Its header said these were
"already handed off", but none had landed in this repo's code or plan, so Zach added them to V1.
Slotted RIGHT AFTER the delete/re-fetch blocker, blocking items first, all before the graded run.

- **22a (handoff item 2, BLOCKING before any client): client inspections run on the Anthropic API,
  not the Claude Code subscription.** Today every inspection logs `backend: cli`. Make an API backend
  the default for client inspections (the CLI backend may stay for internal benchmark runs). Record
  the backend and provider in the export (manifest + README). If credits later come through AWS
  Bedrock, Bedrock is acceptable but must be recorded the same way. Side benefit: the API backend
  writes no Claude Code session files, so source deletion has less to purge.
- **22b (handoff item 3, BLOCKING): `camerata count`.** Runs ONLY the inspection's own file filter
  (`crates/server/src/onboard/files.rs`: gitignore + noise denylist + extension allowlist + git
  ls-files union) on a repo and prints counted lines and files per language plus the
  excluded-noise count. No model, no tokens, seconds to run. It is the number quotes are priced on,
  so it MUST call the same filter function the inspection uses (DRY; a client-side `cloc` count
  includes generated files and differs). Output plain text and JSON.
- **22c (handoff item 4, protected: export format; the remaining fields):** the manifest already
  exists; add client name, corpus rule count applied, model backend and provider (22a), counted
  lines and excluded-noise count (22b). Confirm the corpus version, rules-last-reviewed, inspected
  commit and engine version fields are populated.
- **22d (handoff item 6, check):** confirm the release gate's reconciliation covers PDF summary
  counts = workbook rows = JSON rows = CSV rows BY SEVERITY AND KIND; extend it if it only checks
  row parity.
- **22e (handoff item 14, protected: corpus): TESTING-MEANINGFUL-ASSERTION-1.** No rule catches a
  test that asserts nothing or only asserts a stub (hearth Q10 class). Deterministic for "no
  assertion in a test body"; judgment for "asserts only a stub". Positive test + safe twin; built
  with the W29 process (Fable spec, Sonnet build, Fable review). Slotted with W29 Wave D.
- **22f (handoff item 15, protected: corpus): the corpus freshness routine.** Per
  `_workspace/brownfield-audits/CORPUS_FRESHNESS_WORK_ORDER.md` (monthly source check command;
  quarterly review). `camerata corpus check` and the due-soon warning exist; build what the work
  order still needs (the source-check command / sweep). Slotted with the item-21 engine follow-ups.
- **22g (handoff item 16, protected: corpus, Zach already asked): clean three showcase rule files**
  of internal notes for the website overlay: SEC-NO-HARDCODED-SECRETS-1 (drop the internal
  docs/ENFORCEMENT.md source or relabel it), ARCH-IDEMPOTENCY-KEYS-1 (replace the boilerplate
  decision question), ARCH-OBSERVABILITY-LOG-CONSISTENCY-1 (move internal module notes out of
  `qualifies` and the comment). Version bumps + CORPUS_LOCK. Slotted with the corpus text work.
- Handoff item 12 (re-fetch at a specific commit) is covered by the delete/re-fetch build.
  Handoff items 1, 5, 7 are done; item 9 (five severity labels stable) needs no change.

### 2d. Item 23: token efficiency on the Anthropic API (added 2026-10-25, Zach, V1)

Spec: `_workspace/brownfield-audits/SPIKE_TOKEN_EFFICIENCY.md`. Goal: client inspections on the
API at the lowest cost that does not reduce recall. **Recall first: no saving is accepted if any
fixture's recall drops.** Evidence: output ~60% of cost; ~480K fixed input per run plus ~310
tokens per counted line (each line sent ~25 to 30 times across rule groups); cache economics
inverted (faktura wrote 661K, read 80K; hearth wrote 1.34M, read 80K); rule routing only 3%;
fix-specific retries paid with no output kept.
- **Part 1 (spike, read-only):** the 14-item EXISTS / PARTIAL / MISSING table with expected
  savings, written to `_workspace/brownfield-audits/SPIKE_TOKEN_EFFICIENCY_RESULTS.md`. Fable
  (judgment on recall trade-offs and cache economics).
- **Part 2 (build), priority order unless the spike says otherwise:** batch API default for client
  runs; cache placement and TTL fixed (or stop caching single-use content); defer prose and fix
  generation until after calibration and dedup; result caching by content hash (also makes the
  delta retest cheap); better routing / merged rule groups; deterministic pre-filtering; telemetry,
  pre-flight estimate, budget cap and a cost-regression check. Fable spec where recall is at
  stake, Sonnet builds, Fable review.
- **Protected (needs Zach's go when hit):** calibration, dedup/merge, severity, export format,
  the corpus.
- **Acceptance needs live model runs** on faktura, tutorly, lumen, hearth and supabase-portal:
  recall and decoy results unchanged or better; cost per 1K counted lines before and after each
  change. These runs spend API tokens.
- **Slot:** right after 22a (the API backend default), before the review speed-ups, so every
  later live run (including the graded run) is measured at the lower cost.
- **Spike DONE 2026-10-25** (Fable, read-only): `_workspace/brownfield-audits/SPIKE_TOKEN_EFFICIENCY_RESULTS.md`.
  Key correction: every sweep ran `backend: cli`, so the "cache writes >> reads" numbers are the
  Claude Code harness's caching, not Camerata's; one API-backend fixture run is needed for a true
  baseline. Ranked build plan: (1) per-stage telemetry + one API fixture baseline run; (2) batch
  API default with per-item re-submission, routing honoured, 1h TTL in batch mode (~50%); (3) cache
  scheduling fix: warm-then-fan-out, cache the rules block, drop the group label from the prefix
  (20 to 30% of input, no recall risk); (4) terse scan output (25 to 35%, PROTECTED); (5) result
  cache keyed with model + prompt version (80 to 90% on reruns); (6) fix-retry pre-filter; (7)
  domain routing + larger rule batches (PROTECTED, corpus); (8) deterministic pre-filtering
  (PROTECTED, corpus); (9) pre-flight estimate + budget cap (export touch PROTECTED); (10) Haiku
  for the fix stage. Ranks 2, 3, 5, 6 need no protected change: roughly 60 to 70% off a first
  inspection and ~90% off a rerun. Awaiting Zach: go on ranks 4, 7, 8, 9 and on the live API
  fixture runs (they spend tokens).
- **Part 2 built (2026-10-25), ranks 1, 3, 5, 6, no protected area touched, no live model call:**
  rank 1 per-stage telemetry (operator ledger + end-of-`inspect` table, never in the export; a
  test pins that) and `camerata cost-regression`; rank 3 warm-then-fan-out scheduling
  (`warm_schedule`), the group label moved out of the cached prefix, 1h TTL for batch items;
  rank 5 `AiScanKey` (audit model, calibration model, `AUDIT_PROMPT_SCHEMA_VERSION`, rule
  directives) in the manifest, `MANIFEST_VERSION` 3; rank 6 truncation-only scan retry and the
  no-code fix pre-filter. Rules-first prompt order deliberately not built (see
  `build_pass_prompt`). Still to do with a key: the API-backend fixture baseline run, and
  recall/decoy confirmation on the five fixtures for ranks 3 and 6. Rank 2 (batch default) and
  the rest remain unbuilt.

## 3. Explicitly NOT doing

- `ARCH-OBSERVABILITY-CORRELATION-ID-1` stays `decision` — prior ruling, recorded in the roster.
- `SEC-NO-RAW-SQL-CONCAT-1 / escape-then-concatenate` stays — the roster's own documented
  "genuine finding the 244-rule audit missed".
- Option-id renames were previously recommended AGAINST by two auditors; **D9 overrides that**
  on the grounds that no client scan has run. If that ever stops being true, the recommendation
  stands again.
- Durable per-finding state across engagements — deferred twice now (`Confirmed`, then
  `could_be_intentional`). **A third request is the signal to build it properly.**

## 4. Open questions — none blocking

Every question raised this session has been answered. The two that remain are observations
rather than blockers:

- Whether `[decision].question` boilerplate (~160 corpus-wide) is acceptable or is sized debt.
  Operator-only, so low stakes; the work order proposes replacements.
- Whether the corpus should adopt `testing/` as the explicit worked example to edit toward. It
  is the healthiest pack by a clear margin (6 of 7 clean, hand-authored ids, real decision
  questions, no truncation).
