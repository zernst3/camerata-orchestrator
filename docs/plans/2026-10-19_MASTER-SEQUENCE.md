# Master sequence — everything pending before the next graded sweep

**This is the resume document.** Written 2026-10-19 at the owner's request: *"I just want to be
sure all the necessary work that is pending is KNOWN about. We will just work through
everything in sequence until we are ready to start the cycle again."*

Nothing is in progress. Nothing below has been started unless marked DONE.

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

### PHASE 1 — finish Wave 1 (two gates)

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
16. **W32 rule metadata** (D6). Part (a) DONE (`05d94df`): schema added (`cwe`/`asvs`/
    `superseded_since`/`current_recommendation` on RuleToml+Rule, both load sites, unknown-field
    allowlist, RULE_AUTHORING), `cwe` authored on all 14 SEC- rules from their own source CWE
    citations, two gates (`w32_rule_metadata`: every SEC- rule has a code + well-formedness).
    **Part (b) DEFERRED, schema-ready:** `superseded_since` + `current_recommendation` on
    pattern-level architecture rules needs per-rule judgment about which patterns genuinely aged
    out and the year/guidance — a credibility call to make deliberately with Zach, not bulk-guess.
    W27 renders what exists; a row simply omits the supersession line until (b) is authored.

### PHASE 4 — W29, coverage expansion

17. **The nine consistency rules + clone detector + 18 safe-twin tests.** TWO hard
    dependencies, both now satisfiable: W19's gate must exist first, and the convention
    synthesiser must be fixed (done, `46984f9`) because source §83 wires all nine straight
    into it.
    - Order within: dead-code / shared-constant / one-import-path → hooks lint (needs the
      eslint fix) → single-source-logic (builds the clone detector) → naming / shared-primitive
      / state-shape → needless-complexity.

### PHASE 5 — residuals, then measure

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

19b. **The eslint residual, narrowed 2026-10-20 — it is NOT provisioning.**

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
20. **ONE graded sweep, all five repos** (D12). **Hearth MUST be re-graded** — its 30/46 is
    invalid, it measured a run whose prompt was 63% engine JSON. All five answer keys exist.
    Expect roughly $63 and most of a day.

---

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
