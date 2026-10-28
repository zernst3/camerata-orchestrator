# False-negative audit of the inspection pipeline (2026-10-28)

Base: `origin/feat/audit-report-export` at `7bc1057`. Read-only audit; no product code changed.

The question asked at every stage: can a real finding be dropped, downgraded, merged into a
different finding, moved to a non-actionable bucket or left out of an export WITHOUT a visible
disclosure in the client report? A disclosure that lands only in the console, the operator log,
`report.message`, stage telemetry or the on-disk scan ledger counts as silent here, because none
of those reach the zip (PDF, xlsx, CSV, JSON, README).

Method: the pipeline was traced from `camerata inspect` (`crates/cli/src/inspect_cmd.rs:575`)
through `onboard::audit_repos_with_llm_controlled` (`crates/server/src/onboard.rs:1453`), the AI
tier (`ai_audit.rs`), the preview tools and dependency audit (`lib.rs`, `scan_tools.rs`,
`dep_audit.rs`), the bounds (`severity_bounds.rs`), the seal and the exporters
(`report_export.rs`, `xlsx_export.rs`, `templates/audit_report.typ`, `release_gate*`). Five
stage reads ran in parallel; every HIGH item and the main MEDIUM items were then re-read at the
cited lines. Line numbers are on the base commit above.

Counts: **12 HIGH, 26 MEDIUM, 22 LOW.**

## The four channels that carry (or fail to carry) a disclosure

Most findings below reduce to one of these facts, so they are stated once:

1. `report.message` (the joined `notes`), `report.ai_error` and `report.ai_blocked_reason` are
   not read by any exporter. `grep` over `report_export.rs`, `xlsx_export.rs` and
   `templates/audit_report.typ` finds them only in test fixtures. Every note pushed at
   `onboard.rs:2144-2180`, `2343-2346`, `2352`, `2463-2466`, `2467` and `2580-2585` is
   therefore console-only.
2. `report.failed_passes` IS rendered (executive summary and methodology,
   `report_export.rs:4707-4753`; `audit_report.typ:352-360`, `672-678`). It is the one reliable
   client channel.
3. The ledger reaches the client only three ways: a stage with `unaccounted > 0`
   (`scan_ledger::stage_disclosure`, `scan_ledger.rs:1099-1121`), one "N rules could not be
   mechanically checked" sentence (`report_export.rs:4728-4748`) and the rules-not-run list
   built at `report_export.rs:4563-4579`. That list is rendered ONLY on the xlsx Coverage sheet
   (`xlsx_export.rs:2306-2321`); the Typst template never reads `rules_not_run`. A stage whose
   drops are fully accounted (merges, dedups, stack exceptions, P7 filters) produces no client
   line at all.
4. "What's healthy" is `ran && findings_emitted == 0` from the ledger
   (`report_export.rs:4528-4535`), so any path that records a rule as ran while it saw less than
   the whole repo (or saw nothing) turns into a positive "verified clean" claim, which is worse
   than silence.

`calibration_rationale` and any pre-bound severity are never exported (not a field of
`CuratedSiteJson` at `report_export.rs:1171-1245` or of the xlsx `FindingRow`), so no severity
change made after detection is ever visible to the client.

## HIGH

### H1. A cut-off or unparseable scan reply is a successful pass with zero findings (real-time)
- Where: `ai_audit.rs:4483-4503` (one truncation retry, then the reply is used as is),
  `ai_audit.rs:4506-4513` (returns `Ok(parse_ai_findings(..))` even when `resp.truncated` or
  `scan_output_is_malformed` is true; only `record_wasted` telemetry), `ai_audit.rs:1092-1097`
  (`parse_ai_findings` returns empty vecs when the slice will not parse). The pass then counts as
  ok in `run_passes`.
- Trigger: a 350k-character chunk with a few dozen violations against a 15-rule batch plus the
  advisory task exceeds 8192 output tokens, and the 16384 retry can be cut off too; or
  `truncation_retry` is switched off; or the model appends prose containing `}`. On the `cli`
  backend `truncated` is always false (`crates/llm/src/llm.rs:601`) and the salvage path returns
  a partial stream as `Ok` (`llm.rs:580-590`).
- Why silent: the whole pass is lost, not just its tail. No `PassFailure` is raised; the
  semantic rules are recorded `ran` with 0 findings (`onboard.rs:2268-2303`) and can be listed
  under "What's healthy". Densely broken code (the worst repos) is the most likely to trigger it.
- Minimal fix: when the final reply is still truncated or malformed, return a pass error so it
  becomes a `FailedPass`; salvage complete finding objects from a cut-off array and keep them.
  H4 brief item 1 covers malformed JSON; the truncated case is not in that brief.

### H2. Batch mode never checks truncation or malformed output, and packs every rule into one item
- Where: `reassemble_batch_passes`, `ai_audit.rs:5240-5256` (parses every response, never reads
  `resp.truncated`, counts it `ok`), items built `with_max_tokens(8192)` (`ai_audit.rs:5047`),
  `BATCH_RULE_BATCH_SIZE = usize::MAX` (`ai_audit.rs:4631`).
- Trigger: any `--batch` scan with dozens of selected rules over a large chunk: one 8192-token
  answer must hold every rule's findings for that chunk. Batch is opt-in on this base, but it is
  the cheaper path offered for client work.
- Why silent: same as H1, with no retry and not even wasted-token telemetry.
- Minimal fix: treat a truncated or malformed batch item as a `PassFailure` and re-run it in real
  time with a split rule batch; scale `max_tokens` with rule count.

### H3. A partly failed AI pass is cached as "scanned, 0 findings" and never re-audited
- Where: `onboard.rs:2367` (`manifest_builder.record_repo` fingerprints EVERY file),
  `scan_cache.rs:331-344` (`record_repo`), `onboard.rs:2677`
  (`ai_covered = run_ai_review && report.ai_error.is_none()`; `ai_error` is set only when every
  pass failed), `scan_cache.rs:185-189` (`is_reusable_baseline`), `scan_cache.rs:250-264`
  (`partition` treats a matching fingerprint as unchanged) and the same test in
  `client_source/rescan.rs:101-106`.
- Trigger: run 1 has one failed chunk (timeout, refusal, 529, H1-style empty pass) or the spend
  cap stops some passes; run 1's report correctly lists the failed pass. Run 2 is incremental
  (the CLI default when a manifest exists) with those files unchanged: they are not sent to the
  model, nothing is carried for them; if the repo is fully unchanged every semantic rule is
  recorded `ran` (`onboard.rs:2185-2198`).
- Why silent: run 2's report has no failed pass and no incomplete line; the disclosure from run 1
  is gone and the rules can read as verified clean.
- Minimal fix: set `ai_covered = false` whenever `failed_passes` is non-empty or a cost-cap record
  stopped anything; better, fingerprint into the manifest only files whose passes answered.

### H4. A total AI failure (or an AI block) for a repo is not disclosed in the PDF
- Where: the `Err(e)` arm at `onboard.rs:2323-2348` pushes no `FailedPass`; it records the
  semantic rules not-run and writes `first_ai_error` and a note (channel 1, never exported). The
  blocked path forces `run_ai_review = false` (`onboard.rs:1536-1548`) and records
  "AI/semantic review not requested" (`onboard.rs:2350-2361`). The rules-not-run list is
  xlsx-only (channel 3), and `record_rule` ORs `ran` (`scan_ledger.rs:745`), so the not-run entry
  is erased when (a) another repo in the same scan ran that rule, or (b) a deterministic channel
  already recorded the rule `ran` (`onboard.rs:1878-1887`, `1937-1975`; under W4 every selected
  rule is also in the semantic set).
- Trigger: a model outage, bad key or rate-limit for repo B of a two-repo inspection; or a
  single-repo run where every pass errors.
- Why silent: every AI finding is missing, the methodology still says unconditionally that a
  calibrated model review ran (`report_export.rs:3439-3457`), and nothing in the PDF says it did
  not. The `Err` path also discards the `violates` rows and earlier sub-pass `FailedPass`es
  collected inside `audit_repo` (`ai_audit.rs:8009-8085`, returned away at `8190-8198`).
- Minimal fix: push `FailedPass { pass: "AI review", reason }` in the `Err` arm and for
  `ai_blocked_reason`; key ledger `ran` per (repo, rule); make the methodology AI sentence
  conditional on the AI tier having run.

### H5. The location merge buries a co-located higher-severity AI finding
- Where: `merge_location_group` and `merged_severity` (`ai_audit.rs:5603-5667`),
  `finding_origin` (`ai_audit.rs:5953-5968`), `finding_class` and `SECURITY_RULE_TOKENS`
  (`ai_audit.rs:5368-5467`), called before calibration at `ai_audit.rs:8294-8296`; absorbed ids
  rendered through `also_matches_titles` (`report_export.rs:2046-2056`).
- Mechanism: before calibration every AI finding has `confidence == None`, so an AI finding
  filed under an ADOPTED rule id is classed `Origin::Deterministic`; an `AI-` id is `InventedAi`.
  The group is "mixed tier", so `merged_severity` keeps the primary's own severity instead of the
  max. Class is token-based and `IDOR`, `TRAVERSAL`, `MASS-ASSIGNMENT` and `UNAUTHENTICATED` (which
  does not start with `AUTH`) are not tokens, so the security finding does not win primacy. The
  absorbed member keeps only its rule id in `also_matches`: no `also_locations` entry, no
  severity, no detail. `also_matches_titles` then drops every id without a corpus title, which is
  every `AI-` id.
- Trigger: `const o = await db.orders.findById(req.params.id)` flagged by adopted
  `ARCH-STRICT-LAYERING-1` (medium) and `AI-IDOR-ORDER-READ` (critical) at the same line. Output:
  one medium layering row; the IDOR text, severity and name are gone from every export. If the
  primary is `ARCH-OBSERVABILITY-*`, D8 and `is_informational` (`report_export.rs:3049`) move the
  row out of every action bucket.
- Why silent: only the ledger's `ai-location-merge` accounting records it.
- Minimal fix: take the max severity whenever the merged rule ids differ; push every absorbed
  member into `also_locations` with its severity and headline; fall back to the absorbed
  finding's own headline when an id has no corpus title.

### H6. Merge-absorbed sites never appear in the PDF, and the scorecard calls their rule clean
- Where: `templates/audit_report.typ:208-264` (`render_site` has no `also_locations` or
  `site_count`; `report_export.rs:1220-1228` says "The template does not render it yet"),
  `typ:515` ("(N sites)" counts `group.sites.len()`) and the scorecard's matched set
  (`report_export.rs:4371-4386`) folds `also_matches` but not `also_locations`, so a rule whose
  only hit was absorbed counts clean (`report_export.rs:4462-4465`).
- Trigger: `merge_semantic_group` absorbs a CRITICAL `SEC-NO-SECRET-FILE-1` site
  (`.env.production`) into another primary, the exact shape the code comment at
  `report_export.rs:4508-4512` records from a real export.
- Why silent: the PDF shows the primary's path and line only; the absorbed rule's name appears
  nowhere and its scorecard row reads Clean. The xlsx "Also found at" column has path:line only
  (`xlsx_export.rs:1186-1198`); only `findings.json` has the full entry.
- Minimal fix: render `also_locations` (path, line, absorbed rule title, severity) in
  `render_site`, use `site_count` and chain `also_locations` rule ids into the scorecard's
  matched set as "What's healthy" already does (`report_export.rs:4513-4519`).

### H7. The D6 R1 self-hedge cap turns a Critical into a held Low, triggered by calibration's own words
- Where: `ai_audit.rs:2383-2396` (R1), phrase tables `ai_audit.rs:2057-2090`, scan text
  `ai_audit.rs:1742-1750` (`detail` + `snippet` + `category`), and `detail` already carries the
  calibration model's appended reason (`ai_audit.rs:1592-1612`). Unlike R3 and R4, R1 is not
  gated on `floor_earned_critical`.
- Trigger: an unauthenticated delete-account endpoint, raised to Critical by the D5 floor, whose
  calibration reason reads "real issue; confirm whether the route is publicly mounted".
  "confirm whether", "intended behavior" and "if this is intentional" are ordinary model prose;
  the negation guard misses "not the intended behavior" and "unless this is intended".
- Why silent: the client sees a Low row held for review; the cap and the prior severity live
  only in `calibration_rationale`, which is never exported.
- Minimal fix: gate R1 on `!floor_earned_critical`, scan only the producer's text (not the
  calibration reason) and export the original severity with the rationale.

### H8. Secret-bearing and some code file types are never read, and the secret rules are credited
- Where: `onboard/files.rs:18-33` (`CODE_EXTS`), `49-63` (`CONFIG_TEXT_EXTS`), `236-255`
  (`is_admissible_text`: extensionless non-dotfiles rejected), `264-267` (`vue`, `svelte`,
  `astro`, `html` are reference text only). The floor records every `AUDIT_RULES` id `ran` over
  `files.len()` (`onboard.rs:1878-1887`), while the gateway arm itself names `.key` and the
  `id_rsa` family (`crates/gateway/src/lib.rs:1210-1216`).
- Trigger: a committed `deploy/server.key`, `.ssh/id_rsa`, `infra/prod.tfvars`,
  `terraform.tfstate`, `AuthKey_ABC.p8`, `.aws/credentials`, `kubeconfig`, a `build.gradle` or
  `.kts` with `storePassword`, `settings.xml`, `web.config`; or a Vue/Svelte app whose components
  hold an `sk_live_` key or the only data-access code. Scala, `.cc`, `.hpp`, `.ps1`, `.bat` and
  `.ipynb` are also never read.
- Why silent: these files are not counted as excluded; `SEC-NO-SECRET-FILE-1` and
  `SEC-NO-HARDCODED-SECRETS-1` can appear under "What's healthy"; a whole Vue front end can
  receive no AI review with no line saying so.
- Minimal fix: admit these names and extensions at least to the floor (and evaluate path-only
  rules over every walked path); add a coverage line "N files not scanned (unsupported type)" by
  extension.

### H9. Non-UTF-8, binary-keystore and oversized files are skipped with no record
- Where: `onboard/files.rs:584-594`, `629-637`, `732-743` (a bare `continue` for
  `> MAX_FILE_BYTES` (400 KB, line 14) and for `read_to_string` failure; nothing is counted).
  `.p12`, `.pfx`, `.jks`, `.keystore` are admitted by `CONFIG_TEXT_EXTS` but are binary, so the
  path-based `SEC-NO-SECRET-FILE-1` arm (`onboard/audit.rs:226-233`) never sees them.
- Trigger: an Android `app/release.jks`; legacy Latin-1 PHP or Java with one accented comment;
  SQL Server scripts saved as UTF-16 (the SSMS default); a 450 KB `views.py`; a large
  `remote_schema.sql` migration that the RLS replay then reads partially.
- Why silent: absent from "files scanned" and from "excluded as noise"; the rules are credited.
- Minimal fix: evaluate path rules before reading content; decode lossily (BOM and UTF-16
  detection) for the floor; list skipped paths with a reason under Coverage (H4 brief item 1
  covers the size half only).

### H10. Language routing never sends some files to their language's rules
- Where: `scan_routing.rs:36-49` (`lang_for_extension` has no `mjs`, `cjs`, `mts`, `cts`, which
  `onboard/files.rs:19` collects as code), `scan_routing.rs:87` (`SPRING` maps to `java` only),
  config files (`toml`, `yaml`, `json`) never in a language group; filter applied in
  `run_routed_passes`, `ai_audit.rs:7300-7304`. Only a group with zero files is disclosed
  (`ai_audit.rs:7306-7317`).
- Trigger: a Node ESM backend in `src/*.mjs` or `.mts`: no `NODE-`, `JS-`, `TS-` or `NEXT-` rule
  ever sees it. A Kotlin Spring Boot app: `SPRING-*` rules see only `.java` files.
  `SPRING-*` never sees `application.yml`; `RUST-*` never sees `Cargo.toml`.
- Why silent: the ledger records each rule `ran` over `files.len()` (`onboard.rs:2296-2303`)
  with no partial note, so the rule can read as verified clean.
- Minimal fix: map the four ESM and TS module extensions to `web`; let a scope cover several
  languages (Spring: java and kotlin); include manifests and config in language groups; add a
  partial-coverage note when a group ran on a strict subset.

### H11. Incremental carry-forward is never disclosed, and carried files are not re-checked when their dependencies change
- Where: the notes "incremental: X changed, Y reused from cache" and "fully cached"
  (`onboard.rs:2154-2160`, `2176-2180`) go to `report.message` (channel 1). Only `part.changed`
  bodies go to the model (`onboard.rs:2201-2207`). The README provenance (`lib.rs:17354-17390`)
  has no carry line.
- Trigger: `api/orders.ts` is unchanged and calls `lib/sanitize.ts`; `sanitize.ts` changes and
  drops escaping. The injection that now exists in `orders.ts` is never evaluated, and its old
  findings are carried as current. `camerata inspect` is incremental by default whenever a
  manifest exists.
- Why silent: the client cannot tell which findings, or which files, reflect the current code.
- Minimal fix: a cover and methodology line ("N files re-audited, M findings carried from the
  inspection at SHA"), a carried-from column in xlsx and CSV; optionally re-audit files that
  import a changed file.

### H12. "Rescan changes" drops AI findings filed under CI-tier rule ids
- Where: `client_source/rescan.rs:112-120` carries a finding only when
  `ledger.rule(id).tier == RuleTier::Semantic`; the ledger keeps the tier of the FIRST
  `record_rule` call (`scan_ledger.rs:742-768`), and deterministic and architectural records come
  first (`onboard.rs:1878-1887`, `1937-1975`).
- Trigger: the AI found a hard-coded secret the regex floor misses, or an RLS gap the checker
  misses, under `SEC-NO-HARDCODED-SECRETS-1` or `SUPABASE-RLS-ENABLED-1`, in an unchanged file.
  After the desktop's delta rescan it is neither carried nor re-audited; the floor re-runs but
  cannot recreate it.
- Why silent: the row disappears from the replacement inspection and its exports.
- Minimal fix: stamp the producing tier on each `Finding` and carry by that, or carry every
  non-preview finding and dedupe against the fresh floor.

## MEDIUM

### M1. The D6 R2 CORS ceiling overrides a floor Critical on a substring match
`ai_audit.rs:2333-2343`, phrases `ai_audit.rs:2033-2052`. Any "cors" or "cross-origin" in
detail, snippet or category sets exactly Medium and returns early, before the
`floor_earned_critical` check. Trigger: "unauthenticated /api/users returns all PII; wildcard
CORS lets any site read it", or a snippet `app.use(cors(), adminRouter)`. Silent: rationale not
exported. Fix: apply R2 only when CORS is the finding's subject (rule id or category) and never
over a floor-earned Critical.

### M2. No severity change after detection is disclosed, and corpus ceilings clamp model findings
`apply_verdicts` `ai_audit.rs:1517-1549` (a high-confidence "low" verdict replaces "critical"
with no rationale when the reason is empty; the calibration model does not see code), the D5,
D6, D7, D8, W24 and W31 adjustments (`ai_audit.rs:1540`, `1998`, `2276-2396`, one rationale slot
that later rules overwrite), `severity_bounds.rs:83-87` (`max_severity`). Example:
`FIREBASE-RULES-FIELD-VALIDATION-1` carries `max_severity = "low"`; an AI finding filed under
it for `allow update: if request.auth.uid == userId` on a user document with a `role` field
(self-promotion to admin) is clamped to Low. Fix: carry `original_severity` and a rationale list
on every finding; render "severity adjusted from X: why" in PDF, xlsx and CSV.

### M3. Severity matching is case-sensitive at the producer and in calibration
`ai_audit.rs:1116-1121` (scan parser), `1521-1526` (verdicts), `2730-2735` (consensus):
`"Critical"`, `"HIGH"`, `"severe"` become `medium`. Fix: lowercase and trim, map synonyms, keep
the current severity on an unknown token.

### M4. Weak rule-id normalisation files an adopted rule's violation under an invented id
`ai_audit.rs:1102-1115` uppercases and replaces spaces only. The prompt lists rules as
`- [RULE-ID] ...` (`build_rules_block`, `ai_audit.rs:2841`), so `[ARCH-STRICT-LAYERING-1]`,
`ARCH-STRICT-LAYERING-1:` or `ARCH_STRICT_LAYERING_1` become `AI-...`; the key `rule_id` instead
of `rule` gives `AI-ARCHITECTURE`. The adopted rule is then recorded with 0 findings
(`onboard.rs:2244`, `2268`) and can be listed healthy. Fix: strip brackets and trailing text, map
`_` to `-`, accept `rule_id` and `id`, match adopted ids as a token prefix.

### M5. Line parsing plus the exact (path, line, rule) dedup collapses distinct violations
`ai_audit.rs:1142` (`as_u64().unwrap_or(0)`: `"42"`, `42.0`, `"42-45"` become 0),
`resolve_finding_lines` `ai_audit.rs:5685-5705` (recovers only from a verbatim snippet), then
`retain` on `(path, line, rule_id)` at `ai_audit.rs:8294` keeps the first of several same-rule
line-0 findings in a file, first-seen severity winning. Same at the deep tier
(`ai_audit.rs:9049`). Fix: parse numeric strings and ranges; include a snippet and detail hash
in the dedup key and keep the max severity.

### M6. Deferred judgments (`needs_files`) are dropped when paths do not match exactly
`ai_audit.rs:8217-8221` (exact `requested.contains(p)` against `files`, which on an incremental
run is only `part.changed`), `parse_needs_files` `ai_audit.rs:4203-4219` (trim only) and the
resolution round's own deferrals are discarded (`_rn`, `ai_audit.rs:8251`). The prompt tells the
model to defer instead of reporting (`ai_audit.rs:922`, `934`). Fix: normalise paths, resolve
against the full map file set, disclose any unresolved deferral as a `FailedPass`.

### M7. The not-applicable verdict is taken from chunk 0 and then deletes the rule's findings repo-wide
`recommend_alternatives` uses `file_chunks.first()` (`ai_audit.rs:3357-3361`); `audit_repo`
drops the rule from every pass (`ai_audit.rs:8039-8047`) and hard-filters findings under it,
plus the A5.3 no-convention filter (`ai_audit.rs:8378-8415`), with only ledger `other` buckets.
Trigger: a repo over 350k characters whose pagination or versioning code sorts after chunk 0.
Partially disclosed: xlsx shows "Applicable: No" (`xlsx_export.rs:261`, `1676`); the PDF does
not. Fix: decide per chunk and OR the results; disclose how many emitted findings a
not-applicable verdict removed.

### M8. Unlocated findings are lowered to info without saying so
`downgrade_unlocated_above_informational`, `report_export.rs:700-731`, inside
`bounded_severity` (`severity_bounds.rs:81-85`) and applied in place in the live scan. A
repo-wide AI finding, an absence finding or an evidence-gate row whose citation would not parse
(`report_export.rs:3598-3612`) at high or critical becomes "Convention to consider". Fix: keep
the original severity and prefix "severity lowered from X: no location established"; count these
rows in the executive summary.

### M9. One false-positive mark hides several findings that share a disposition key
`finding_key` is repo, rule, path, line, snippet (`report_export.rs:259-264`); dependency rows
use path = lockfile, line 0, snippet = `pkg@version` (`dep_audit.rs:246-257`). Marking one CVE on
`lodash@4.17.15` false positive removes every advisory on that version from the PDF
(`report_export.rs:3898-3901`), counted only in `excluded_fp`; the xlsx dependency rows ignore
dispositions (`xlsx_export.rs:323-337`), so formats disagree. Merged rows also inherit the
primary's key. Fix: add the advisory id to dependency keys; key by a stable finding id.

### M10. Dependency advisories with only a CVSS vector are reported as medium
`dep_audit.rs:317-353`: the vector string (OSV's normal `severity[].score` form) is never parsed,
`CVSS_V4` and `groups[].max_severity` are ignored, unknown words map to medium
(`dep_audit.rs:307-315`). RUSTSEC, GO and PYSEC records often lack
`database_specific.severity`, so a CVSS 9.8 ships as medium with no note. The KEV floor that
would lift known-exploited CVEs is not on this base (it is only on `origin/cloud/kev-epss`,
`crates/server/src/vuln_intel.rs`). Fix: compute the base score from the vector or use
`max_severity`; otherwise label "severity unknown" and hold for review. Needs Zach or a human to
confirm current osv-scanner JSON fields (outside this repo's ground truth).

### M11. "No known vulnerable dependencies" is printed when osv-scanner did not run
`dependency_clean = dep_rows.is_empty()` (`report_export.rs:4597`) ignores osv notes;
`tool_coverage_disclosures` filters osv out (`report_export.rs:1431`), so the failure appears
only as a small footnote in section 7 (`audit_report.typ:619-623`) while "What's healthy"
(`typ:586-588`) claims clean. Trigger: OSV unreachable, provisioning failure, the 60 s desktop or
120 s tool timeout. Fix: clean only when there are no rows and no osv failure note; add the osv
failure to `failed_passes`.

### M12. Partial dependency-ecosystem coverage is never disclosed
On success the osv stderr is discarded (`dep_audit.rs:456-499`). A repo with a
`package-lock.json` plus Gradle without a lockfile, a `.csproj` without `packages.lock.json` or a
`pyproject.toml` without a lock gets no note for the unaudited part; the "no lockfiles" wording
claims "no dependency exposure detected". Fix: compare manifests found by the file walk with
`results[].source.path` and disclose the rest. Which manifests osv-scanner reads needs a human
check.

### M13. Deep-tier findings and lens errors are absent from the product export
`report.deep` (`onboard.rs:2498`; `DeepLensResult` at `ai_audit.rs:8652-8664`) is not read by
`report_export.rs`, `xlsx_export.rs` or the release gate; it reaches only
`GET /deep-report` (`lib.rs:16731-16772`). Its own dedup and merge have no ledger stage
(`ai_audit.rs:9046-9050`). Fix: include deep findings (or a count and pointer) and lens errors as
`failed_passes` in the zip.

### M14. The HTTP export reloads the corpus at export time and discards load failures
`lib.rs:16878-16882`, `17013-17017` (`load_corpus_lenient(..).await.0`; missing path gives
`None`). With an empty or partial corpus every calibrated finding becomes uncited and
`effective_bucket` (`report_export.rs:2690-2696`) moves high and critical rows to held and the
rest to informational; ceilings are re-applied from a corpus that may differ from the scan's.
The CLI uses the scan's corpus (`inspect_cmd.rs:452-481`). Fix: surface `.1` as a `FailedPass`
or refuse to export; export against the scan-time corpus.

### M15. A baseline suppression matches in any file
`suppression.rs:274-280` matches `rule_id` and `fingerprint(rule, snippet)` only;
`BaselineEntry.path` (`suppression.rs:158`) is never compared. A baselined
`requests.get(u, verify=False)` in `scripts/seed.py` also suppresses the same new line in
`app/payments.py`, which ships as "Pre-existing accepted debt" in the Accepted bucket. An AI
finding whose snippet is its title (`ai_audit.rs:1131`) suppresses every future same-title
finding. Fix: require `e.path == f.path`.

### M16. An inline waiver for a sibling rule suppresses the merged primary
`onboard/audit.rs:513-528`: a waiver naming ANY `also_matches` id suppresses the row. Combined
with H5, `// camerata:allow STYLE-NAMING-1 -- legacy name` above a line where a critical
`SEC-*` row absorbed `STYLE-NAMING-1` moves the critical to Accepted with the fixed label
"Waived in code" (`report_export.rs:517`), no rule or reason shown. Fix: suppress only when the
waiver names the primary; render the waiver's rule, reason and line.

### M17. Shared-object merges ignore category, and the primary's ceiling caps the absorbed finding
`ai_audit.rs:6554`, `6579-6588` (a shared captured object bypasses the category and
`objects_conflict` checks), `ai_audit.rs:6975` (single-linkage grouping chains), then
`bounded_severity` clamps the merged row to the PRIMARY rule's `max_severity`
(`severity_bounds.rs:83-87`). Trigger: deterministic `SUPABASE-RLS-INITPLAN-1` (performance,
max medium, table `orders`) plus an AI "orders policy USING (true): any authenticated user reads
every order" on the same table: one Medium performance row. Fix: require the same category for
shared-object merges; never let a primary's ceiling cap an absorbed higher-severity member.

### M18. A failed Clippy run reads as "ran, clean"
`scan_tools.rs:1610-1620` discards the exit status (`_ok`); `parse_clippy_json`
(`scan_tools.rs:870-890`) skips non-JSON lines and cannot fail on empty output; then
`lib.rs:6841-6845` records every clippy rule ran with 0. Trigger: `backend/Cargo.toml` with no
root manifest, clippy not installed, a build script needing openssl or protoc, offline. Fix:
non-success exit or no `build-finished success:true` is an error with a coverage note (note H2
brief item 2 will skip client tooling for client work; that skip also needs a client line).

### M19. Partial Semgrep and ESLint failures, and client suppression comments for external tools, are silent
Semgrep exit status and SARIF `invocations` errors are ignored (`scan_tools.rs:1558-1567`,
`657-718`); ESLint discloses only when every file failed to parse (`scan_tools.rs:742-744`,
`1686-1712`). `eslint_args` has no `--no-inline-config` (`scan_tools.rs:361-379`),
`semgrep_args` no `--disable-nosem` (`1209-1221`), `ruff_args` no `--isolated` (`1189-1206`), so
`// eslint-disable-next-line no-eval`, `# nosemgrep`, `# noqa: S608` suppress findings with no
record, unlike Camerata's own disclosed waivers. Fix: disclose partial failures; run with inline
config disabled and report such hits as "waived by client comment". Semgrep's SARIF on fatal
error needs a human check.

### M20. The test-path heuristic lowers production findings to held Low
`crates/gateway/src/lib.rs:590-621` (`test`, `fixtures` segments; `test_*.py`; `*_test.*`;
`*.spec.*`), applied at `onboard/audit.rs:288-300` (severity `TEST_PATH_SEVERITY`,
`needs_review`). `app/models/test_result.py`, `experiments/ab_test.ts`, `lib/ab_test.rb`,
`openapi.spec.json`, a Django `fixtures/` folder loaded in production: a live AWS key drops from
critical to a held low. Partially disclosed by the suffix "(in test/fixture code ...)", which
then states a false fact. Fix: `test_*`/`*_test` only under a test directory; never lower a
live-shaped secret below high.

### M21. First-party directories are pruned by name, and the paths are thrown away
`NOISE_DIRS` (`onboard/files.rs:79-131`: `target`, `build`, `out`, `obj`, `coverage`, `vendor`,
`.idea`, `.vscode`, `generated`) matched on any segment (`files.rs:439-446`); paths discarded at
`onboard.rs:1761` (`excluded_noise_paths: _`). Disclosure is a bare count on the cover
(`audit_report.typ:302`). Misrouted examples: a Java package `.../marketing/target/`,
`scripts/build/deploy.sh` with a credential, `src/coverage/policy.ts`, a committed
`build/config.json` or `dist/env.js` with live keys, `.vscode/launch.json` with `API_KEY`. The
secrets floor skips them too. Fix: always run the floor over noise files; list excluded paths by
directory in a dataset.

### M22. Rules not run, partial runs and skip reasons are never rendered in the PDF
`rules_not_run` (`report_export.rs:4563-4579`) is only in the xlsx Coverage sheet; the spend-cap
partial note (`onboard.rs:2296-2302`), "AI/semantic review not requested", corpus load failures
and clone "not assessed" reach no PDF line. Fix: a "Not assessed in this inspection" section and
a rules-not-run dataset.

### M23. Repo-level rows (NotAssessed, NoConvention) are dropped on incremental runs
`scan_cache.rs:258-264` carries a finding only when `unchanged_paths.contains(&f.path)`;
NotAssessed and NoConvention rows have an empty path (`ai_audit.rs:7735-7737`, `7779-7781`), and
rows citing README or a directory miss too. On a fully unchanged rerun the "could not be
evaluated" honesty row vanishes and the rule is recorded ran with 0 (`onboard.rs:2185-2198`):
a not-assessed disclosure becomes a verified-clean claim. Fix: carry rows whose path is not a
file when the repo has no changes, or regenerate them from persisted recommendations.

### M24. The release gate's row parity is weaker than its name
`release_gate/consistency.rs:274-321`: a `BTreeSet` over (rule, repo, path, line) collapses
duplicates; it compares matrix buckets, not the rendered `curated_findings` and
`held_for_review_findings`; it ignores dependency rows (the PDF excludes false positives and uses
`bounded_severity`, `dependencies.json` does neither); `audit_zip` (`release_gate.rs:229-256`)
skips it. Two line-0 findings of one rule in one file: drop either from one side and it still
passes. Fix: a multiset over a full identity plus snippet hash, compared against the sealed
`report.findings`, including dependency rows and per-sheet xlsx counts.

### M25. THOROUGH-mode consensus can land below the median vote
`ai_audit.rs:2730` (a missing severity counts as a medium vote), `2780-2795` (ties break low):
critical, high, medium gives medium; with one pass failed, critical versus low gives low.
Partially disclosed (confidence low, row held); the downgrade is not. Fix: median of real votes.

### M26. Rust files the `syn` parser cannot read are skipped but counted as evaluated
`extract/rust_syn.rs:28-33` returns `None` on a parse error or nesting deeper than 64
(`extract/nesting.rs:28`); `resource_lifecycle_checker.rs:103-105` and the dead-code, import
boundary, handler-no-db and one-import-path checkers then see nothing for that file while the
rule is credited over `files.len()`. Fix: a not-assessed row with a count.

## LOW

- **L1.** The 20,000-file cap note goes only to `report.message` (`onboard.rs:2462-2466`;
  `files.rs:12`, `595-598`); absence rules are withheld, other rules are silently partial. (H4
  brief item 1 plans this.)
- **L2.** A repo whose file read fails gets only "audit failed" in `report.message`
  (`onboard.rs:2468`); with an empty ledger "What's healthy" falls back to every selected rule
  (`report_export.rs:4528-4532`), so a single-repo read failure can list every rule as clean.
- **L3.** The stack-exception filter drops findings with only an accounted ledger bucket
  (`onboard.rs:2370-2397`, `ai_audit.rs:6738-6760`); one rule declares an exception today
  (`arch-monolith-first-1`).
- **L4.** `group_structural_needs_review` lowers medium to low and replaces each detail with a
  generic sentence (`ai_audit.rs:6788-6905`); narrow trigger.
- **L5.** `strip_headline_cross_references` deletes whole detail sentences that name another
  rule id or a position phrase (`ai_audit.rs:7075-7097`), which can remove the exploit sentence.
- **L6.** `dedup_scan_previews` drops the absorbed preview's severity and detail, and dropped
  previews never enter the ledger (`lib.rs:6539-6556`; `scan_ledger.rs:1080-1097` counts net
  additions only).
- **L7.** `build_digest` packs by raw size but numbering adds characters; past 600k the rest of
  the chunk is skipped with the notice only in the prompt (`ai_audit.rs:854-880`).
- **L8.** A model finding with empty `title`, `detail` and `code` (for example the model used
  `description` or `message`) is dropped (`ai_audit.rs:1125-1127`); valid JSON, so not even
  wasted-token telemetry.
- **L9.** Over 100k batch items, an error in sub-batch N discards sub-batches 1 to N-1 via `?`
  (`ai_audit.rs:5150`, `5184`, `5187`).
- **L10.** Deep-tier security passes repeat H1 (truncated equals clean, no retry,
  `ai_audit.rs:9136-9160`) and its whole-repo digest is capped at 600k (`ai_audit.rs:9223`).
- **L11.** With only language-scoped rules selected there is no cross-cutting group, so `.sql`,
  `.yaml`, `.tf`, `.sh` and Dockerfiles get no model review (`ai_audit.rs:7284-7304`).
- **L12.** `normalize_severity` maps any unknown word to low (`report_export.rs:540-551`).
- **L13.** The priority grid has no `info` row, so a Confirmed or TechDebt info row in an action
  bucket is missing from it (`report_export.rs:1087`, `1144-1149`); the cover strip omits low
  and info (`audit_report.typ:313-315`); the scorecard counts info as low
  (`report_export.rs:4405`).
- **L14.** `ARCH-OBSERVABILITY-*` is informational at any severity (`report_export.rs:3049-3051`)
  with no "capped by policy" note.
- **L15.** CSV cells starting with `=`, `+`, `-` or `@` are not neutralised
  (`xlsx_export.rs:2483-2522`); Excel shows a formula or `#NAME?` instead of the text.
- **L16.** No cap against the 32,767-character xlsx cell limit except on the snippet
  (`xlsx_export.rs:1110-1207`); a long merged row blocks the whole export (fails closed).
- **L17.** The README says the xlsx has every finding as its own row (`lib.rs:17150-17162`);
  Conventions and NotAssessed rows are on other sheets and merged sites share one row. NotAssessed
  rows lose severity, path and line on the Coverage sheet (`xlsx_export.rs:884-932`).
- **L18.** `CAMERATA_DISABLE_DEP_AUDIT` returns no note (`dep_audit.rs:383-389`); no `results`
  key parses as zero advisories even on a failing exit (`dep_audit.rs:151-157`); an osv-scanner
  already on PATH is used whatever its version while provenance stamps the pinned one
  (`tool_provisioning.rs:1252-1261`, `996`).
- **L19.** `is_in_repo_scope` drops any `out/` or `dist/` segment and any path that cannot be
  canonicalised (relative `--repo .`, percent-encoded SARIF URIs), uncounted
  (`scan_tools.rs:1096-1148`).
- **L20.** Without `.git` the walk never descends into noise directories, so the excluded count
  reads 0 (`files.rs:706-715`); with no `git` on PATH the tracked-but-ignored union is skipped
  (`files.rs:664-680`); `.git_global(true)` applies the operator's own global gitignore to the
  client repo (`files.rs:538`).
- **L21.** The clone skip lists route first-party folders (`messages`, `external`, `deps`,
  `versions`, `seed`) out of the clone rules (`checks/src/extract/clone/skip.rs:26-62`), and
  `IndexSummary.files_skipped` has no consumer; a `UI-SHARED-PRIMITIVE-1` family over 40 members
  is dropped entirely (`clone_rules/primitive.rs:311`).
- **L22.** The export's own `report-build-partition` ledger stage is recorded on a clone and
  discarded, and balances by construction (`report_export.rs:4044-4089`), so it cannot catch an
  export-time loss.

## Verified safe

These paths were traced and found to keep the finding or disclose the gap in the client report:

- **Spend cap.** Real-time passes refused at admission and batch submissions refused before
  submit become `CostCapRecord::failed_passes` (`ai_audit.rs:4753-4770`, `5128-5148`;
  `onboard.rs:2576-2586`), rendered in summary and methodology; per-rule NotRun and Partial reach
  the ledger (`onboard.rs:2266-2303`). `stage_blocked` records skipped stages
  (`ai_audit.rs:479-494`). An unknown-cost call stops later admissions (`ai_audit.rs:708-722`).
  The gap is only the cache carry-over in H3.
- **Partial pass failures** (HTTP error, timeout, API refusal via `refusal_error`,
  `llm.rs:928-947`) and errored, expired or missing batch items become `PassFailure` and then
  `failed_passes` (`ai_audit.rs:141-155`, `5257-5278`, `8204`).
- **Calibration fails open.** `apply_verdicts` never drops a row (`ai_audit.rs:1612`);
  unparseable verdict JSON returns findings unchanged (`ai_audit.rs:1508-1516`); a failed or
  capped calibration chunk passes through (`ai_audit.rs:2541-2546`, `2650-2651`, `8334-8340`).
  The D5 floor only raises (`ai_audit.rs:1993-2001`).
- **Evidence gate** only adds rows and accounts them (`report_export.rs:3646-3649`); a
  sites-versus-rows shortfall becomes a visible held row (`report_export.rs:3709-3756`).
- **Merge scope.** `merge_by_location` never groups line-0, unlocated or informational rows
  (`ai_audit.rs:5745-5762`) and runs per repo; the cross-family merge is per repo and its window
  requires non-zero lines (`ai_audit.rs:6480-6486`); `merge_semantic_group` keeps every member's
  site in `also_locations` (`ai_audit.rs:6676-6690`). "What's healthy" excludes rules seen in
  `also_matches` or `also_locations` (`report_export.rs:4513-4536`).
- **Exports keep every row.** Every code finding becomes a curated or held site
  (`report_export.rs:4099-4116`, `4319-4333`); the held appendix is not truncated; the "Three
  things" box and the "What's healthy" cap of 10 are disclosed selections
  (`report_export.rs:4550-4551`, `4660-4697`). Snippet caps carry a "(truncated)" marker
  (`report_export.rs:741-763`, `xlsx_export.rs:506-516`).
- **Typst** reads all strings from `data.json` as content or `raw()`, with no eval, so `#`, `]`,
  quotes and newlines cannot break the document (`audit_report.typ:58-71`).
- **CSV and JSON parity** is proven cell by cell (`release_gate/consistency.rs:142-268`); the
  dataset set check fails closed (`consistency.rs:41-109`).
- **Secrets in the release gate** block the export rather than strip it
  (`release_gate.rs:311-358`); redaction replaces the value and keeps the finding
  (`onboard/redact.rs:55-78`).
- **Triage.** False positives are counted in the methodology and listed with reasons on the xlsx
  False Positives sheet and dataset (`xlsx_export.rs:403-411`, `2393-2418`); an unknown triage
  state falls back to the row's status, never to Open (`report_export.rs:392-405`); UI and server
  disposition keys match (`ui-core/src/triage.rs:1040-1045`).
- **Waivers.** A reason-less inline waiver does not suppress and raises a high
  `CAM-WAIVER-NEEDS-REASON` finding (`suppression.rs:267-272`); suppressed rows keep a status and
  are never deleted (`onboard/audit.rs:529-534`).
- **Cache invalidation.** A changed rule selection, model, calibration model, prompt-schema
  version, directive text or manifest version forces a full re-scan with a note
  (`scan_cache.rs:185-189`; `onboard.rs:2139-2152`); fingerprints are byte-exact; deleted paths
  are not carried. An AI-off, blocked or totally failed run is never a baseline
  (`onboard.rs:2677`). Carried findings pass through stack exceptions, the cross-family merge,
  suppression and bounds again (`onboard.rs:2367-2427`).
- **External tools missing or crashing.** Semgrep, ESLint, Ruff or cargo failing to provision or
  spawn, or emitting empty or unparseable output, produce a coverage note and a `FailedPass` per
  rule (`scan_tools.rs:1303-1336`, `1476-1486`; `lib.rs:6799-6839`). osv-scanner provisioning,
  spawn, timeout and parse errors produce a section 7 note (`dep_audit.rs:391-500`; see M11 for
  the misleading healthy bullet). Lockfiles in subfolders are found with `-r`. Advisories without
  a fix are kept with "no patched version available yet" (`dep_audit.rs:236-240`).
- **Absence rules on a truncated scan** are withheld with a ledger note
  (`onboard.rs:1812-1817`), and clone candidates over the cap become a not-assessed row
  (`clone_rules.rs:274-290`).
- **Ownership** does not hide anything because it is effectively unused on this base:
  `ownership_of` never returns Generated or Vendored and `report_export.rs` never reads
  ownership. The section described in `ownership.rs:1-9` does not exist in the PDF; generated and
  vendored code is instead excluded at file collection (M21).

## Overlap with other briefs

- H4 brief item 1 (malformed JSON retry, size skips, file cap listing) overlaps H1, H9 and L1;
  H1's truncated case and H2's batch path are not in that brief.
- H2 brief item 2 (skip client tooling) will turn M18's Clippy pass into a deliberate skip, which
  needs its own client line.
- The KEV floor (M10) exists only on `origin/cloud/kev-epss` and was reviewed there: it raises
  matched CVEs to critical and discloses feed failures; the CVSS-vector downgrade remains for
  CVEs not in KEV.

## Needs a human

- M10, M12, M19 and L18 depend on current osv-scanner and Semgrep output shapes, which ship
  outside this repo and move faster than the auditor's training data (ORCH-TRAINING-CUTOFF-1).
- H5's fix ("max severity whenever rule ids differ") partly reverses the C5-1 decision recorded
  at `ai_audit.rs:5593-5602`; that is a tradeoff between AI over-grading and buried defects and
  should be Zach's call (ORCH-CONFLICTING-ROBUSTNESS-1). Exporting the absorbed member's
  severity and headline is safe either way.
