# Corpus staleness check (2026-10-20)

## Status

Design, not yet built. No urgency: every rule was authored in spring 2026 or later
(the Supabase pack within the last few months), so nothing is stale today. This exists
to PREPARE - a sweep before the first client, and a recurring sweep thereafter - and
to give Camerata a way to nag me when the corpus is overdue for one.

## The problem

Camerata's corpus (~417 rules) encodes "what good looks like" for a stack. "Good"
drifts: an ecosystem moves off a pattern (class components, a deprecated serializer, a
superseded API shape), and a rule that still teaches the old way silently becomes wrong
guidance. Nothing in the system notices the clock. The risk is low now and grows with
every month the corpus sits untouched while the outside world moves.

Two needs:

1. **A sweep** - a periodic pass that checks each rule's described pattern against
   current ecosystem guidance and surfaces candidates that may have aged out.
2. **A staleness nag** - Camerata tracks when the corpus was last confirmed current and
   warns me when that was too long ago, so a sweep never silently lapses.

## The load-bearing distinction: swept vs confirmed

The watermark does NOT advance when a sweep RUNS. It advances when I REVIEW the sweep's
findings and confirm I am confident the corpus is current. A sweep produces candidates;
confirmation is a human act; the watermark records the confirmation.

This matters because an automated "sweep ran, stamp today" would mark the corpus fresh
on the strength of a pass I never looked at - exactly the false-confidence the feature
exists to prevent. Running the sweep and advancing the watermark are two separate acts,
same as "shipping code" and "changing infrastructure" are kept separate elsewhere in
this repo.

## Where the watermark lives

A version-controlled file beside the corpus:

```
crates/rules/CORPUS_FRESHNESS.toml
```

```toml
# When each rule domain was last SWEPT and CONFIRMED current against the ecosystem.
# Advanced only by a human confirmation after reviewing a sweep (see the staleness
# design doc). Running a sweep does NOT touch this file.

warn_after_days = 90   # quarterly default; override per domain below if needed

[confirmed]
# domain = "YYYY-MM-DD"  — the date the domain was last confirmed current
universal  = "2026-10-20"
supabase   = "2026-08-15"
javascript = "2026-05-01"
# ... one line per domain/pack
```

Rationale for this location:

- It travels WITH the corpus. The thing whose freshness we track and the freshness
  record are in one repo, one commit history.
- Every confirmation becomes a git commit - a free, auditable "who confirmed what,
  when" trail, no database needed.
- Camerata already loads the corpus from `crates/rules/principles/`
  (`camerata_rules::corpus_path`); reading a sibling file is trivial and needs no new
  storage.

**Granularity: per-domain, not one corpus-level date.** Ecosystem churn is wildly
uneven - a JavaScript/React pattern ages in a year, a SQL or security-primitive rule in
a decade. A single corpus-level watermark would either nag constantly (paced to the
fastest-moving pack) or lull (paced to the slowest). Per-domain lets the JS pack demand
a quarterly look while `universal`/`sec` ride a yearly one. MVP can start with a single
`default` entry and split per-domain when the first sweep shows which packs move.

## The staleness nag

On corpus load (and as a line in the daily-digest / morning-consolidator routine),
Camerata computes `today - confirmed[domain]` for each domain and, for any that exceed
that domain's `warn_after_days`, emits a warning:

```
CORPUS STALENESS: javascript last confirmed 2026-05-01 (172 days ago, threshold 90).
Due for a staleness sweep.
```

Surfaced in two places:

- A non-fatal banner when the engine loads the corpus (visible to me, never in a client
  artifact).
- A line in the morning digest, so an overdue pack reaches me even when I am not running
  a scan.

It never blocks a scan or an export. Stale guidance is a quality risk to flag, not a
hard gate - and a client engagement must not be stopped because a pack is a week past
its review window.

## The sweep itself

An LLM-driven research pass, one agent per domain/pack (the orchestrator's existing
fan-out). For each rule it takes the rule's `title`, the option `directive`s, and the
`[[sources]]`, and asks: *is this still current best practice as of today, or has the
ecosystem moved?* It returns, per rule, one of:

- CURRENT - no change, cite why it is still the consensus.
- SHIFTED - the ecosystem moved; what replaced it, since when, with a current-guidance
  source.
- UNCERTAIN - route to me; the research was inconclusive.

Output is a review queue (raw, over-inclusive, erring toward showing me more - matching
the loop-grades-the-raw-product philosophy), never an auto-edit of the corpus.

## The confirm-then-watermark flow

1. Run the sweep for a domain (or all). It writes a findings report, touches nothing
   else.
2. I review. For each SHIFTED finding I either:
   - agree it is stale -> author `superseded_since` + `current_recommendation` on that
     rule (the W32 part (b) schema fields - this sweep is precisely the process that
     ever sets them), or revise/retire the rule; or
   - disagree -> leave it, note why.
3. When I am confident the domain is current, I advance its watermark - a single
   command / UI action that sets `confirmed[domain] = today` and commits
   `CORPUS_FRESHNESS.toml`. This is the ONLY thing that advances the watermark.

So W32 part (b), dropped as a standalone chore, re-enters here as the natural output of
a confirmed sweep finding, not a field authored speculatively.

## Recurrence

- **Before the first client: a full sweep of every domain, all confirmed**, so the
  first paid deliverable rests on a corpus I have just verified is current. This is a
  gated pre-client action, tracked in memory.
- **Thereafter: a recurring routine**, monthly or quarterly (quarterly is the default
  `warn_after_days`), that runs the sweep and drops its queue into my review inbox. The
  nag is the backstop if the routine ever lapses.

## Build order (when it is scheduled - after W27)

1. `CORPUS_FRESHNESS.toml` + a `camerata_rules` reader (parse, compute per-domain age).
2. The nag: a staleness check wired into corpus load + the digest routine.
3. The confirm command/action that advances a domain's watermark and commits the file.
4. The sweep agent (reuses the orchestrator fan-out), writing a review queue.
5. The recurring routine that runs step 4 on a schedule.

Steps 1-3 are small and self-contained (the mechanism). Step 4-5 are the heavier agent
work and can follow.

## Open questions for Zach

1. **Default threshold: quarterly (90d)?** Or monthly for fast packs (js/frameworks)
   and quarterly/yearly for slow ones (sec/universal)? Recommendation: 90d default,
   with per-domain overrides authored as the first sweep reveals which packs move.
2. **Where does the recurring sweep run** - a cloud routine (like the existing nightly
   ones) or a local on-demand command I invoke? Recommendation: on-demand to start
   (I control when I have review bandwidth), promote to a scheduled routine once the
   output quality is proven.

## Non-goals

- The sweep never auto-edits the corpus. It produces a review queue; I author the
  changes and advance the watermark.
- The nag never blocks a scan or export.
- Not a per-RULE timestamp. Freshness is tracked per domain; a single rule's currency
  is established by the sweep's finding, not a date field on every rule.

---

## Amendment 2026-10-21: schema readiness, rule versions, and the "due soon" warning

**Zach's direction (2026-10-21).** The rules were all authored spring 2026 or later, so no
freshness sweep runs now. One runs before the first client and then roughly quarterly. The
SCHEMA must be ready for that today: per-rule fields to manage freshness, and a rule version.
The in-app warning must fire as a domain APPROACHES its threshold, not only after it passes.

**Status check of what was asked for before this amendment:**
- In-app warning: designed above ("non-fatal banner when the engine loads the corpus" + a
  morning-digest line) but NOT built. Only `camerata corpus check` (CLI) exists. The "morning
  digest" is Zach's external routine, not Camerata code, so Camerata's in-app surface is the
  cockpit banner below; the routine can call `camerata corpus check`.
- An "approaching" warning was never designed. Only `overdue` (past the threshold) exists.
- `superseded_since` + `current_recommendation` are ALREADY in the schema (W32 part (a) added
  them, with a well-formedness gate), unused by any rule. So W32 part (b) is resolved as:
  **schema ready, populated only by a confirmed sweep finding.** Not dropped.

### 1. Per-rule fields (all on `RuleToml`/`Rule`, both load sites, unknown-field allowlist,
`RULE_AUTHORING.md`, `RULES_REFERENCE.md`)

| field | type | required | meaning |
|---|---|---|---|
| `version` | integer ≥ 1 | yes (backfill every rule to `1`) | bumped whenever anything about the rule changes |
| `last_reviewed` | `"YYYY-MM-DD"` | no | this rule individually confirmed current on that date (a single-rule review between full sweeps) |
| `superseded_since` | `"YYYY"` | no (exists) | the ecosystem moved off this pattern that year |
| `current_recommendation` | string | no (exists) | what replaced it, with a source |

A rule's **effective review date** = the later of its own `last_reviewed` and its domain's
`[confirmed]` watermark. Gates: `last_reviewed` parses and is not in the future;
`superseded_since` set ⇒ `current_recommendation` set (and vice versa); `version ≥ 1`.

### 2. Making `version` mechanical, not a convention

A version field nobody is forced to bump rots. So:
- `Rule::content_hash()` in `camerata-rules`: SHA-256 over a canonical serialization of the
  WHOLE rule (every field, options included, `version`/`last_reviewed` excluded so a bump does
  not change the hash), rendered `"sha256:<hex>"`.
- `crates/rules/CORPUS_LOCK.toml`: `[rules] "<ID>" = { version = N, hash = "sha256:..." }`.
- Gate (rules-crate test): for each rule, if the hash differs from the lock while the version is
  unchanged → FAIL "rule X changed; bump `version`, then run `camerata corpus lock`". If the
  version differs from the lock, or a rule is missing from the lock, or the lock names a rule
  that no longer exists → FAIL "run `camerata corpus lock`". The lock never moves on its own.
- `camerata corpus lock`: regenerates the lock from the current corpus (refuses if any rule's
  hash changed without a version bump, so the CLI cannot launder a missed bump).

### 3. Fix the applied-rule drift detector (two defects found 2026-10-21)

`crates/server/src/lib.rs::rule_content_hash` (a) hashes only id/title/summary/enforcement, so a
changed option or directive never shows as drift, and (b) uses `DefaultHasher`, which Rust does
not guarantee stable across builds, so an upgrade can report every applied rule as changed (its
doc comment also wrongly claims SHA-256). Replace with `Rule::content_hash()`. A stored applied
hash without the `sha256:` prefix is LEGACY: report it as `changed: null` + `legacy_baseline:
true` (re-baselined on next apply), never as a false "changed". Add `applied_version` /
`current_version` to the `rule_drift` response when known.

### 4. The "due soon" warning

- `CORPUS_FRESHNESS.toml` gains `warn_lead_days = 14` (per-domain overrides allowed, like
  `warn_after_days`). New `CorpusFreshness::due_soon(today)`: confirmed domains whose age is
  within `warn_lead_days` of the threshold but not past it.
- `camerata corpus check` prints three groups: OVERDUE, DUE SOON (with days remaining), NEVER
  CONFIRMED.
- Server: one non-fatal log line at corpus load summarizing the same; `/api/updates/check`
  gains `corpus_freshness: { overdue: [...], due_soon: [...], never_confirmed_count }`.
- Cockpit: the existing update banner shows a corpus-freshness line when anything is overdue or
  due soon (never-confirmed alone shows a quieter one-line note, since that is the honest
  pre-first-sweep state and should not shout).
- Operator-only. A gate asserts no freshness text reaches any client artifact (PDF, xlsx,
  CSV/JSON, README). The client-visible "rules last reviewed" date in the manifest is separate
  and unchanged.

### Built 2026-10-21: implementation notes

All four parts are built (rules schema + gates, content hash + lock + `camerata corpus lock`,
the drift-hash fix, the due-soon warning). Where the amendment was wrong or underspecified, and
what was chosen:

- **The hash is the whole `Rule`, not the raw TOML.** `layer` and `[[emits]]` are parsed but not
  carried onto `Rule`, so an edit to only those does not move the hash. `domain` (folder-derived)
  and the derived `summary` are in it. Format: canonical JSON, sorted keys, omit-when-empty,
  behind a `camerata-rule-hash/v1` tag. `Rule::canonical_form` destructures `Rule` with no `..`,
  so a new field is a compile error until its hash treatment is decided.
- **The lock gate has two cases the amendment did not list.** A `version` BELOW the locked one
  (`VersionRegressed`) and two rules sharing an id (`DuplicateId`) both fail, and both block
  `camerata corpus lock`. A bump with no content change still fails until the lock is
  regenerated, as specified ("the lock never moves on its own").
- **`camerata corpus lock` always uses the bundled corpus**, ignoring `CAMERATA_CORPUS_PATH`,
  and loads it strictly: a rule file that fails to parse is an error, never a silent removal.
- **The applied hash is NOT stored at apply time.** The amendment assumed it was. In fact
  `RuleSelection.applied_hash` is written only by `accept_rule_drift` (the drift "Update this
  rule" action); `save_armed_to_project` creates selections with no baseline and
  `upsert_base_rules` replaces the whole list, so a re-apply also erases existing baselines.
  Drift detection therefore covers only rules the architect has accepted through the drift
  action. `applied_version` is stored beside the hash in that one place. Baselining at apply time
  is a separate, behaviour-changing follow-up and was deliberately not done here.
- **Legacy baselines are listed, not hidden.** A hash without `sha256:` appears in `rule_drift`
  as `changed: null` + `legacy_baseline: true`; the cockpit drift notice words it as "older
  baseline" (nothing is known to have changed) rather than as a corpus update.
- **"One log line at server corpus load"**: the server has no single corpus load (it loads per
  request), so the line is logged once at startup from `serve()`, off the critical path.
- **`corpus_freshness` is `null` when freshness is unknown** (no corpus loaded, or the watermark
  file is absent or unparseable), never an all-clear and never "every domain unconfirmed". A
  packaged binary running away from its source tree has no watermark to read.
- **`parse_ymd` is now strict** (4-2-2 digits, real calendar day), shared by the watermark file,
  the per-rule `last_reviewed` gate and `corpus confirm`. It previously accepted `2026-02-30`.
- **Lead override naming** mirrors the window override: `warn_lead_days_overrides`. The key
  `warn_lead_days` must sit above the first `[table]` header in `CORPUS_FRESHNESS.toml` (a test
  inspects the raw TOML, because the default of 14 would hide a misplacement).
- **Package name:** the CLI crate is `camerata`, not `camerata-cli`.
