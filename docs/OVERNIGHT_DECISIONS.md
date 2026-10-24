# Overnight autonomous run — decisions needed + progress log

> ## ▶ RESUME HERE (paused 2026-10-22 at Zach's request, after Wave C)
> Branch `feat/audit-report-export` is pushed. No agent worktrees are in flight.
> **Disk:** Full Disk Access granted 2026-10-22 to `/bin/bash` and `cargo-sweep`; the 30-minute
> guard verified running ("start; free=" in `~/Library/Logs/cargo-disk-guard.log`). OPEN: Zach
> pasted the settings JSON (hook + deny list) into the REPO's `.claude/settings.json` instead of
> `~/.claude/settings.json`. It is uncommitted and must NOT be committed (personal paths). Once he
> moves it to `~/.claude/settings.json`, restore the repo file with `git checkout .claude/settings.json`.
> **W29 done:** Wave A (ONE-IMPORT-PATH, SHARED-CONSTANT, DEAD-CODE), Wave B
> (UI-REACT-HOOKS-LINT-1 + the eslint baseline-leak fix + exact eslint pins), and Wave C (clone
> detector + SINGLE-SOURCE-LOGIC, NAMING-ONE-CONCEPT, SHARED-PRIMITIVE + the advisory judge seam).
> Every detector was Fable-reviewed, fixed, and merged. **V1 scope** (Zach, DECISIONS MADE below):
> everything in `docs/plans/2026-10-19_MASTER-SEQUENCE.md` plus the graded accuracy tests (the
> outreach go-signal); no new scope except defect fixes; rule `stages` is out of V1.
> **Overnight mutation run (started 2026-10-22, no model tokens):** `cargo mutants --package
> camerata-checks --jobs 4` (6,380 mutants, est. 12 to 14 h), detached with `nohup caffeinate -i`.
> Log: `~/Library/Logs/cargo-mutants-checks.log`. Results (written as it goes):
> `mutants.out/` in the repo root (gitignored): `caught.txt`, `missed.txt`, `timeout.txt`,
> `unviable.txt`, `outcomes.json`. Report: caught / (caught + missed) as the fault-catch rate;
> triage `missed.txt` into "add a test" vs "equivalent mutant". Only the checks crate's own tests
> run against each mutant (server-level tests that exercise checkers are not counted). Check it is
> finished (`pgrep -fl cargo-mutants`) before rebuilding in the main checkout.
> **STATUS 2026-10-23 (paused for the night):** secret redaction + export release gate DONE and
> pushed (7,273 tests green). The old leaky exports are NOT regenerated separately: the graded
> accuracy run (item 20) reruns them (Zach). Mutation run done (97.9%; 26 major gaps ledgered for
> V1). **Next session starts with the delete + re-fetch client source blocker** (decisions below).
> Then: 26 mutation gaps, W29 Wave D, corpus text, item-21 engine follow-ups, W27 PDF (needs
> Zach's eye), 19f grader revision, graded run (item 20).
> (Historical) **FIRST, before anything else (Zach, 2026-10-22):** the secret-redaction work order
> (`_workspace/brownfield-audits/WORK_ORDER_SECRET_REDACTION.md`) plus the pluggable export release
> gate; decisions recorded under DECISIONS MADE. Process: Fable spec, Sonnet build, Fable leak hunt.
> **SECOND (Zach, 2026-10-22):** delete client source after delivery (per-repo destructive action,
> project stays fully usable without the repo); decisions under DECISIONS MADE.
> **Then, in order (sequential, to conserve tokens):** W29 Wave D (UI-STATE-SHAPE-1,
> STYLE-NEEDLESS-COMPLEXITY-1: Fable spec, Sonnet build, Fable review); then corpus text work
> (MASTER-SEQUENCE items 5, 11, 12, 19 corpus sub-items); then the engine follow-ups in item 21
> (CI-gate severity bounds, stale model registry with prices verified from Anthropic's pricing page,
> staleness nag at load, W30 residuals); then W27 PDF rebuild (needs Zach's eye); then 19f grader
> revision; then the graded sweep (item 20). First, a cheap Haiku pass to bring the
> MASTER-SEQUENCE status table current (several items it shows OPEN are done).
> Process: Opus directs; Fable 5.1 writes trap-heavy specs and adversarially reviews every detector
> before merge; Sonnet builds; Haiku does mechanical work. Remove each agent worktree right after
> merge (`scripts/remove-merged-worktree.sh <id>`); the 30-minute cargo disk guard must stay
> installed. Single-purpose shell commands; edits via the Edit tool; questions to Zach only for real
> product decisions, in plain language. Standing directive: keep developing; nothing dropped; fix
> every defect.


Started 2026-10-20 (Zach logged off; autonomous continuation of the master sequence).
Zach reviews this in the morning. I commit locally; I cannot push unattended, so every
commit sits on the branch ready for review + push.

## DECISIONS NEEDED (Zach)

- W32 part (b) (`superseded_since` / `current_recommendation`: per-rule fields saying "the
  ecosystem moved off this pattern in year X; current guidance is <link>"). The earlier "dropped"
  ruling was inferred from Zach asking for more context, not stated by him. Zach asked for a
  plain-language re-explanation before deciding (2026-10-21). Until then: status UNDECIDED, not
  dropped. Note the corpus staleness sweep design already routes confirmed-stale findings into
  these fields, so (b) as a standalone chore and (b) as sweep output are separate questions.

## DECISIONS MADE

- 2026-10-24 (delete + re-fetch spec questions): (1, Zach) Claude Code's own session files under
  `~/.claude/projects/` that hold client source from Camerata's CLI-backend AI passes are deleted
  during source deletion: Camerata tags each call it makes per inspection (`--session-id`, ids
  recorded on the inspection entry) and deletes exactly those files, listing them in the deletion
  record. Zach approved this outside-repo deletion. (2, Opus engineering call) Deletion refuses
  while the checkout has uncommitted or unpushed work unless the operator ticks "Delete
  uncommitted work too". Spec: `docs/design/2026-10-24_delete-and-refetch-client-source.md`.

- 2026-10-24 (Zach): **Vulnerability intelligence, pieces 1 and 3 are V1.** (1) Enrich the
  existing osv-scanner dependency findings with CISA KEV (Known Exploited Vulnerabilities, JSON
  feed): a CVE on KEV is labelled "known exploited" and floored to the top severity; show the
  FIRST.org EPSS exploit-probability score. (3) Stamp "Vulnerability data as of <date>" (KEV, EPSS,
  OSV snapshot dates) into the package provenance (README + manifest). Deterministic feeds only;
  NO agent scraping news or social media (unverifiable, violates the verifiable-finding law).
  Piece 2 (runtime/framework version detection + end-of-life via endoflife.date + runtime CVEs via
  NVD/KEV) is the FIRST V2 item. Slotted after the item-21 engine follow-ups. Zach: the goal is to
  finish V1; if marketing is still the bottleneck after V1, start adding V2 features.

- 2026-10-23 (Zach, executive decision): the review speed-ups are V1.
  `_workspace/brownfield-audits/WORK_ORDER_REVIEW_SPEED.md` (code viewer A.1-A.10; B.2 stacked
  group view; B.3 pane context; B.4 order presets; B.5 decision memory; B.7 progress counts + ETA;
  B.6 reviewer notes; B.8 command palette) plus its companion `WORK_ORDER_REVIEW_TIME.md` (items
  1-3). Slotted (Opus) BEFORE the graded accuracy run so that review uses them and the timer turns
  it into measured review hours. Order: delete/re-fetch blocker, 26 mutation gaps, review
  speed-ups, W29 Wave D, corpus text, item-21 engine follow-ups, W27 PDF, 19f grader, graded run.
  The Phase-2 docked "code mode" layout is decided after one real review; the retest diff view
  stays V2 (rejected by the order). Opus engineering calls: add `tree-sitter-rust`; when client
  source has been deleted the viewer says "Source deleted on <date>, re-fetch to view" and never
  silently falls back to a missing working tree; `symbol-refs` passes the symbol to `git grep`
  with `-e` (or after `--`) so a leading `-` cannot be parsed as an option.
  Zach's protected-area answers (2026-10-23): (1) APPROVED: AI-advisory findings are mapped at
  calibration to the closest corpus rule (stable class label when none matches), the model's id
  kept as a secondary field, and the queue groups by the mapped id. (2) Review timer and the
  per-row "reviewed" flag are INTERNAL ONLY, never in any client export ("might raise more
  questions than necessary"); shown in an internal stats view as data for pricing. Timer behaviour
  (Opus, per Zach "automatic as much as it can, with overriding buttons"): starts automatically
  on opening a group or acting on a finding; auto-pauses after 3 minutes with no key or click and
  whenever the Camerata window is not focused, and the idle stretch before the pause is NOT
  counted; an always-visible Pause / Resume button in the progress strip; the ETA uses only
  active time. (3) Reviewer notes: one note per group (optional per row) exported as a
  `reviewer_note` column in the xlsx and JSON exports AND as the italic sentence under the group
  heading in the PDF; redacted and covered by the release gate.

- 2026-10-23 (Opus, Fable leak-hunt owner calls; asymmetric risk favours redaction): (a) T1
  placeholder words in VENDOR bodies must be delimited (`_`/`-`/start/end/case change) or the body
  single-case, so a random key containing "mock"/"fake"/"your" is still redacted; substring
  matching stays for context shapes. (b) Supabase secret bodies may contain `-` (first char
  alnum). (c) `model_prompt_redaction` comes from an environment variable only, never from any
  file in a client repository; the CLI prints the warning on stderr. (d) Bodies of 20 chars or
  fewer show length only, no tail. (e) Readability twins: lowercase snake_case values with 2+
  underscores and at most 2 digits are not secrets; public keys (`sb_publishable_`, `pk_live_`,
  `pk_test_`) after `Bearer` stay readable. (f) Common vendor shapes added now: SendGrid, Twilio,
  Slack `xapp-`, Shopify, Mailgun, npm, Discord, Mapbox. Follow-up (Opus): the ReadableName twin
  never applies to identifiers containing password/passwd/pwd/passphrase/secret/credential
  (passphrases are real secrets); `passphrase` added to the S13 credential names.
- 2026-10-23 (Sonnet build of the secret redaction + release gate, branch from `0ef7065`):
  built per `docs/design/2026-10-23_secret-redaction-and-release-gate.md`; see the commit series
  and `docs/TECHNICAL.md` section 6c. ACCEPTANCE (`camerata release-gate` over the sweep5b
  exports, which predate the gate; redacted forms only): faktura FLAGGED, one Supabase secret key
  (51 chars) at `findings[0].snippet`, `findings[0].fix_specific`,
  `findings[0].also_locations[1].snippet`, `findings[43].fix_specific`, workbook `All Findings`
  T2/V2/V45 and `Universal` T2/V2/V8, and the PDF text; hearth FLAGGED, Supabase (42 chars) plus
  two Stripe live keys (77 chars) and a webhook secret (38 chars) on `findings[1]`; portal CLEAN
  (placeholders readable, no secrets problem); lumen and tutorly also flagged (a Supabase key,
  a real-looking `sk_test_` key, webhook secrets). The old zips also report the CSV/JSON datasets
  and manifest as missing (they predate those exports). STILL OWED (needs a live model run, so
  not done here): regenerate the three exports with the new build and run
  `camerata release-gate` on them, expecting zero problems and the redacted forms present in
  `findings.json`. Steps: delete the stale `.camerata-scan-cache.json` and
  `.camerata-scan-ledger.json` from the hearth fixture checkout, then for each fixture
  `camerata inspect --repo <fixture> --full --export <out>/<name>.zip` (this spends model tokens),
  then `cargo run -p camerata --features pdf-text -- release-gate <out>/<name>.zip` (exit 0).

- 2026-10-23 (Opus, secret-redaction spec questions; asymmetric risk: over-redaction costs only
  readability, under-redaction leaks): Q1 real Stripe TEST-mode keys (`sk_test_`/`rk_test_`) are
  redacted like live keys; only Stripe's published documentation key stays readable via the
  published-key allowlist. Q2 placeholder values (all-caps DO_NOT/FAKE bodies, `your...here`
  style) stay readable per the work order's safe-twin rule, so the portal fixture's acceptance
  result is "clean, placeholders readable". Spec: `docs/design/2026-10-23_secret-redaction-and-release-gate.md`.

- 2026-10-22 (Zach): **Delete client source after delivery** is a V1 blocker, built right after
  secret redaction. A destructive "Delete source code from this machine" action per repo, typed
  confirmation (the repo's name), styled as a danger action. The project stays fully usable with no
  repo on disk: findings, history, exports and package regeneration keep working; the repo shows
  "Source deleted on <date>"; rescans are disabled with an explanation; re-clone restores it.
  Remove every "repo must exist" block that is not about scanning itself. Deletion purges every
  on-disk copy of client code (the checkout, scan cache, AI/judge transcripts, any worktree), and
  KEEPS the code excerpts inside findings (part of the delivered product; Zach confirmed). Safety:
  delete only a folder that is a git repo whose remote matches the project's recorded GitHub repo;
  otherwise refuse with the reason. A deletion record (what, where, when, file count) is kept in
  the project as proof for the client. Exports must never re-read code from disk. Deletion is
  ordinary removal (FileVault-encrypted SSD), not a forensic wipe; the record says so. Zach
  (2026-10-22): it does not matter how the code got into the project (cloned in the UI or by hand);
  whatever codebase the project points to is what gets deleted, guarded by the remote-match check.
  **Re-fetch (revised by Zach, 2026-10-22):** the real reasons to re-fetch are remediation and the
  governance layer's monthly delta retest, both of which need the LATEST code. One "Re-fetch
  source" button opens a dialog: "Latest from <default branch>" (pre-selected) or "The inspected
  commit <sha>, <date>" (rare: reproduce or dispute). Re-fetch clones FULL history so the inspected
  commit is always reachable. Afterwards the repo card shows "Source at <sha>, N commits newer than
  the inspection (<sha>)" with a primary "Rescan changes" action (the delta retest between the
  inspected commit and now; same flow the monthly retest uses). Finding code views read from the
  INSPECTED commit via git objects, not the working tree, so old findings stay accurate while the
  checkout sits at latest; old findings carry a "References commit <sha>; source on disk is newer"
  note. Clears the deleted state. Uses Camerata's existing GitHub token; a revoked/renamed/deleted
  origin is reported plainly. Process:
  Fable spec (incl. locating every repo-must-exist assumption and every place client code lands),
  Sonnet build, Fable adversarial review.

- 2026-10-22 (Zach): **Secret redaction is the next work, before anything else** (work order
  `_workspace/brownfield-audits/WORK_ORDER_SECRET_REDACTION.md`; blocks the first client; Zach's
  go on the protected export/template area). Secrets are redacted BEFORE any code is sent to a
  model, by default. Review additions adopted: redact once at finding construction (every
  surface inherits), one shared secret detector for scan + redactor + release check, JWT role
  claim separates service_role from anon, short secrets show length only, the release check reads
  the Typst input data plus a PDF text smoke test, failure messages never print the secret,
  fingerprints computed from redacted text (no clients yet), acceptance = flag the known leaks in
  the existing faktura/hearth/portal exports, then regenerate clean.
- 2026-10-22 (Zach, V1 scope): the secret release check is built as a pluggable **export release
  gate** that runs on every real export, and reuses the existing export invariants (cross-format
  parity, manifest hashes, provenance agreement, no missing datasets) as runtime checks. That is
  enough QC for V1. An AI reviewer that reads each package after generation is V2:
  `docs/design/2026-10-22_ai-package-qc-target-design.md`.

- 2026-10-22 (Opus, Wave C review, all false-positive-safe narrowings): (1) the three clone
  rules (SINGLE-SOURCE-LOGIC, NAMING-ONE-CONCEPT, SHARED-PRIMITIVE) emit NO convention/"clean"
  row in V1; the detector cannot prove absence. (2) SINGLE-SOURCE escalates to HIGH only on
  disagreements in decision tokens (operator, number, property inside the decision test); string
  and template differences are a trailing clause. (3) The composed-variant guard fires only when
  the helper call dominates the shared sequence (call covers >= 50% of its tokens, or every
  decision token lies inside the call's arguments). (4) Functions that render JSX are excluded
  from NAMING when UI-SHARED-PRIMITIVE-1 is armed.

- 2026-10-21 (Zach, V1 scope): **Camerata V1** = the version that runs the first paid client
  inspection. Scope = EVERYTHING already in `docs/plans/2026-10-19_MASTER-SEQUENCE.md` (all W29
  rules, W27, item-21 follow-ups, 19e/19f/20) plus the graded accuracy tests, which are the go
  signal for outreach. NO new scope except fixes for bugs or real product defects. The rule
  `stages` field is explicitly out of V1. Development is not the outreach blocker (website and
  marketing materials are), so finishing the phased work is fine.

- 2026-10-21 (Opus, on the Fable review's evidence): ARCH-DEAD-CODE-1 does NOT run at the Layer-2
  write-time gate in v1 (new `ArchChecker::runs_at_write_time`, false for dead-code). A story may
  legitimately end with scaffolding or an externally consumed file, and a bounce would teach agents
  to paste `keep-file:` markers. Scan tier and CI only. Re-evaluate once scan-tier false-positive
  data exists. Zach agreed (2026-10-21): dead-code belongs in the inspection, not during
  development; a future home is Layer 4 or a per-file override comment (the `keep-file:` marker,
  guard G12, already exists). Not needed now.
  Design debt noted (Zach asked how "inspection-only" is decided): the switch is a code-level
  `ArchChecker::runs_at_write_time()`, invisible in the corpus and UI, while the opposite control
  ("leave out of inspection") lives elsewhere. When a second rule needs stage control, promote both
  to one corpus field (e.g. `stages = ["inspection", "ci", "write-time"]`) read by every runner,
  covered by CORPUS_LOCK like any other rule field. Zach (2026-10-21): this is the target design
  for Camerata, documented in `docs/design/2026-10-21_rule-stages-target-design.md`; future
  phase, NOT V1.

- 2026-10-21 (Opus, within Zach's "narrow" ruling, false-positive-safe narrowings after the
  Fable review of ARCH-SHARED-CONSTANT-1): N1 rates require >= 2 significant digits (keeps 0.12,
  0.075; drops 0.9, 0.001, 1e-9); Django URL-name callees (reverse, reverse_lazy, redirect,
  resolve_url) are excluded; grouping is scoped to the nearest package root (package.json,
  pyproject.toml, Cargo.toml) so "import the constant" is always possible. Router path vs
  <Link>/href repeats stay IN scope (legitimate under the S2 ruling).
- 2026-10-21 (Zach, W29 scope): ARCH-DEAD-CODE-1 ships FILE-level in v1; unused individual
  exports become a separate future rule once the tooling is false-positive-safe.
  ARCH-SHARED-CONSTANT-1 uses the narrow four-shape "domain literal" definition (dashed/colon
  names, route templates, multi-word SCREAMING_SNAKE, decimals in arithmetic); widen one shape at
  a time with its own safe twins. "Two homes, no bare copies" is NOT a finding in v1.
- 2026-10-21: commit trailers from this session's Sonnet subagents read "Sonnet 4.6" but the
  agents ran Sonnet 5.5 (a brief error). Pushed history is left as is (no force-push); trailers
  are correct from 1f3c4f3's successor onward.
- 2026-10-21: ARCH-ONE-IMPORT-PATH-1 flags a split between a symbol's origin file and an index
  barrel (Zach: "that is the rule working as intended"). Work order §7's "non-index modules"
  wording does not exempt index barrels from the one-home rule.
- 2026-10-21: **Zach's go for all nine W29 corpus rules** (work order §91 protected area). Each
  rule's TOML lands with its detector, registration, positive + safe-twin tests, homed per the
  design doc's "Domain home per rule" table.
- 2026-10-21: `stash@{0}` (09-28 dead-P1 partial) dropped on Zach's OK after verifying every
  symbol and test it added already exists at HEAD.
- 2026-10-21: Camerata memory to be decoupled out of the agora-mono project memory dir (Zach:
  it should never have been there).

## PROGRESS (newest first)

- 2026-10-21: Fable 5.1 audit of the Opus 4.8 stretch. Verdict: no lost work, test claim true,
  assertion edits legitimate, every "Zach ruled X" traced to a real message. Real gaps: lockfile
  not committed (CI `--locked` red), CI red for months on fmt/clippy and never ran on branches, W29
  design keyed ONE-IMPORT-PATH on `to_file` (would never fire) and named nonexistent `arch`/`style`
  domains, 3 export gates unbuilt and unrecorded, master sequence stale, Camerata memory filed under
  the agora-mono project dir, an unrecorded 09-28 stash (checked: superseded). All recorded in
  MASTER-SEQUENCE item 21; lockfile, recent lints, W29 design, reconciliation done. One open
  confirmation for Zach: the W32(b) "dropped" ruling was inferred from his request for more context.

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

CORRECTION (2026-10-22, UI-REACT-HOOKS-LINT-1 review): "pinned" above meant CARET MAJORS
(`eslint@^9` and friends). That stopped the eslint 10 / react-hooks 7 drift but not minor, patch
or transitive drift, and the graded accuracy tests need repeatable tool behavior. It is now truly
pinned: `ESLINT_NPM_PACKAGES` are exact versions (eslint 9.39.5, @typescript-eslint/parser and
eslint-plugin 8.72.0, eslint-plugin-react-hooks 5.2.0, eslint-plugin-jest 28.14.0, the versions
that were installed and verified), `eslint_packages_installed` compares the exact installed
version (a right-major, wrong-minor workspace re-provisions), and `crates/server/assets/eslint/
package-lock.json` (with the matching `package.json`) is committed and installed with
`npm ci --prefer-offline`, so the transitive tree is fixed too. A test holds pins, manifest and
lockfile together. Falls back to installing the exact pins if the lockfile cannot be read.
Bumping eslint now means editing the three together and the bundled config; an `#[ignore]`d test
(`fresh_eslint_provisioning_installs_the_pinned_tree_from_the_lockfile`) exercises a real fresh
install and needs npm plus the registry or a warm cache.

## MILESTONE (2026-10-21): full workspace suite 100% GREEN
`cargo test --workspace --no-fail-fast` = 5857 passed, 0 failed, 0 compile errors. The one
red test (W30 eslint) is now green by a real two-factor fix, not a bandaid. Also caught+fixed
a missed build_product_zip caller (inspect_export_equivalence integration test) during the
final green sweep. Everything committed + pushed (…4177244). No known red tests remain.
