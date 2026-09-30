# Cycle 2 — queue-hardening design spec (2026-09-30)

Framing: the export is the RAW REVIEW QUEUE. **Completeness > precision. Nothing is ever
dropped.** Noise is handled ONLY by merge (keep both rule ids + all locations), re-rank,
re-badge, or route to a review tier. Auto-exclusion of "likely FPs" stays at zero.

Order of implementation (Zach's edit): **W1, W2 first** (missed defects can't be recovered by
curation), then MERGE, then R1/R2, then W3/W4/W5, then W6. Model tiering: Opus designed this;
Sonnet implements each section with positive + safe-twin unit tests on SYNTHETIC inputs (never
the fixture). Workspace stays green (fmt, clippy -D warnings, full tests). Small commits, one
concern each. Never read the keys/answer-key/GRADE-FULL/score.json/feedback-classes/scorecard;
never modify the fixture.

Architecture facts (from the read pass):
- Checker registry: `all_checkers()` in `crates/checks/src/arch_checker.rs:229`.
- Checker dispatch: `audit_architectural()` in `crates/server/src/onboard/architectural.rs:36`,
  gated on (1) rule id ∈ `repo_selected_ids`, (2) interest globs. Neither checker is orphaned.
- Floor scan driver: `audit_repos()` in `crates/server/src/onboard.rs:834` (arch at ~1163).
- Cross-tier merge: `merge_semantic_groups`/`semantic_pair_merges` in `ai_audit.rs:3987/3710`,
  called over the COMBINED floor+AI set at `onboard.rs:1278`.
- Severity calibration (RAISE-only floor): `apply_severity_calibration_rules`/
  `severity_calibration_floor` in `ai_audit.rs:1108/1037`.
- Bucketing + informational routing: `matrix_bucket`/`is_informational` in
  `report_export.rs:1597/1668`. A ≤medium finding with `confidence=="needs-review"` routes to
  the informational appendix (out of every action bucket); critical/high never can.

---

## W1 — definer-privileged DB function without pinned search_path (missed 2 runs)
Checker: `crates/checks/src/supabase/search_path_checker.rs` (`SupabaseFnSearchPathChecker`,
rule `SUPABASE-FUNC-SEARCH-PATH-1`). Detection logic (timeline replay:
`security_definer && !has_search_path`) is sound; the GAPS are breadth gates:
- **Glob too narrow:** `INTEREST_GLOBS = ["supabase/migrations/*.sql","supabase/schemas/*.sql"]`
  with single-segment `*`. Misses nested paths and non-supabase SQL layouts (`db/`, `sql/`,
  `migrations/**`, `prisma/migrations/**`). The vulnerability is general Postgres, not supabase.
- **Arming (CONFIRMED):** NOT armed for a generic Postgres repo. The rule's domain is
  `supabase:database-functions`, which `domains_for_stack` only emits when `detect_frameworks`
  recognizes Supabase (@supabase/* dep, `supabase/config.toml`, or `supabase/migrations/*.sql` —
  propose.rs:87-90,176-184). A plain-Postgres repo gets only the `sql` domain (propose.rs:288),
  so the rule is never suggested → `is_auto_recommended=false` → excluded by
  `curated_rule_selection` (inspect_cmd.rs:150). Even if armed, the supabase-only interest globs
  would miss non-supabase SQL layouts.
GENERAL FIX (not fixture-specific — the vuln is general Postgres, so generalize the detection):
1. **Re-scope so it arms for any Postgres SQL repo.** Add the generic `sql` domain to the rule's
   matched domains (or add a generic sibling rule under the `sql` domain) so a plain-Postgres repo
   proposes+arms it. Verify with a propose-level test: a repo with `.sql` containing a definer fn
   (no supabase layout) arms the rule.
2. **Broaden interest globs** to general SQL (`**/*.sql`) so SQL is inspected wherever it lives.
3. **Harden per-function attribution** in the timeline parser (sql_parse.rs/timeline.rs): a
   sibling function's `SET search_path` must NOT leak to a definer function that lacks it, and
   `SECURITY DEFINER` must be detected across CREATE / CREATE OR REPLACE and syntax variants. The
   safe-twin test (below) exercises exactly this.
This covers both possible root causes (not-armed AND armed-but-detection-buggy) without needing to
know the fixture's layout.
TESTS (synthetic): positive — a definer fn without `SET search_path` in a NON-supabase path
(`db/migrations/0001.sql`) fires exactly one finding; safe twin — same file also contains an
invoker-rights fn AND a definer fn WITH `SET search_path`, assert only the guilty one fires.
Protected: no (additive floor detection).

## W2 — token/secret from a non-crypto PRNG (missed 2 runs)
Checker: `crates/checks/src/weak_randomness_checker.rs` (`WeakTokenRandomnessChecker`, rule
`SEC-NO-WEAK-TOKEN-RANDOMNESS-1`, universal). ARMING CONFIRMED FINE: domain=universal → suggested
for every repo; grounded + not opt-in → `is_auto_recommended=true` for any repo; checker globs are
broad. So the gap is in DETECTION LOGIC, not wiring. Per completeness-over-precision, widen
aggressively (extra findings the human curates are acceptable; a miss is not).
GENERAL FIX (widen the CLASS; no fixture strings):
1. Broaden `PRNG_NEEDLES` with common non-crypto entropy sources the current list misses
   (e.g. `Date.now(`, `performance.now(`, `new Date().getTime(`, py `time.time(`, `os.getpid(`)
   when their result feeds a token/secret/id/link value.
2. Broaden token-identifier recognition and the taint window: follow a weak-PRNG value assigned to
   an INTERMEDIATE variable that is later used to build a token/secret/id within the same function
   (today it only checks same-line target, enclosing call, enclosing fn within 40 lines).
3. Keep the discriminator/suppression lists so the CSPRNG twin and benign jitter/shuffle uses are
   still spared.
TESTS (synthetic): positive — a weak-PRNG value used as an access/share token fires; safe twin —
same module has a CSPRNG-based token generator, assert it does NOT fire.
Protected: no (additive floor detection).

## MERGE — cross-tier duplicate merge (was #1 last cycle; MERGE never drop)
File: `ai_audit.rs`, `semantic_pair_merges` (3710) + helpers. Two gaps:
- **(ii) two deterministic secret-detectors on one file** are blocked by the hard guard at
  3713 ("two deterministic never merge") BEFORE `shared_captured_object` (3649) is checked.
  FIX: relax the det+det guard — allow a det+det merge WHEN `shared_captured_object(a,b)` is
  true (they name the same root object; e.g. the same committed secret/file). Keep the guard
  otherwise (two det findings with no shared object stay distinct — this preserves the
  distinct-defects invariant that `p1_merge_never_drops_a_distinct_root_cause` pins).
- **(i) det+AI describing one defect a few lines apart, near-identical prose.** Signal (a)
  requires same category + ≤5 lines; if category differs or lines are farther, it misses.
  FIX: add a GENERAL description-overlap signal to `semantic_pair_merges`: same file + strong
  normalized title/detail similarity (e.g. token-set overlap ≥ a high threshold) ⇒ merge, even
  across tiers and outside the 5-line window. Keep the `objects_conflict` veto so two findings
  that visibly name DIFFERENT objects never fuse on prose alone.
PRESERVE: the sink+call-site pair across TWO files stays two rows (different files ⇒ signal (a)
never fires; do not let the new description-overlap signal apply across different paths).
TESTS (synthetic, fixture-independent):
- two findings on adjacent lines with overlapping descriptions ⇒ exactly ONE exported row with a
  secondary-match annotation (both rule ids present in `also_matches`, both locations kept).
- det+det sharing a captured object ⇒ one row, both rule ids recorded.
- NEW: a sink finding + a call-site finding in two DIFFERENT files ⇒ stays TWO rows (proves not
  merged). Keep `p1_merge_never_drops_a_distinct_root_cause` green.
Protected: yes (dedup/merge priority) — Zach's explicit go given.

## R1 — "probably intentional" pattern rendered HIGH in an action bucket (regression)
The finding's own body concludes the pattern is likely intentional / confirmation-only, yet it
renders at HIGH in a top action bucket. FIX (re-route + badge, DO NOT DROP): add a DOWNWARD
calibration (new direction — the D5 floor only raises). A new pass after the floor:
- Detect a self-hedged "confirmation-only / likely-intentional" conclusion in the finding text
  (GENERAL phrase set: "likely intentional", "appears intentional", "probably intentional",
  "if this is intentional", "confirm whether", "confirmation only", "may be intentional" — must
  NOT match negations like "not intentional"). 
- On match: set `confidence = Some("needs-review")` AND cap `severity` to "low" (so
  `is_informational`'s ≤medium+needs-review rule routes it to the informational appendix, out of
  every action bucket). Record a `calibration_rationale`. It STAYS in the queue, visibly badged
  (needs-review) wherever it renders.
TESTS: a finding whose text is confirmation-only cannot land in an action bucket (asserts
`is_informational==true`) and carries the needs-review flag; an actionable twin at the same base
severity whose text is NOT hedged still buckets normally.
Protected: yes (calibration + bucketing + template) — Zach's go given.

## R2 — browser-mediated CORS misconfig rated CRITICAL (regression; two notches over)
FIX: add a bidirectional severity CEILING+FLOOR to exactly MEDIUM for the browser-mediated
cross-origin/header-misconfig class (GENERAL detection: category/text names CORS /
"cross-origin" / `Access-Control-Allow-Origin` reflected origin + allowed credentials). If
currently above medium ⇒ lower to medium; if below ⇒ raise to medium (do NOT re-bury at low —
that was the previous failure). Runs alongside the existing floor; the unauthenticated-direct-
exposure class STILL floors to critical (that path is untouched) and thus outranks CORS in
bucketing (medium ⇒ "plan"; critical ⇒ "do_now"). Ensure the CORS text does not trip the
unauthenticated floor.
TESTS (BIDIRECTIONAL): a CORS-credentials finding lands at medium — NOT low (guard re-burial),
NOT critical (guard inflation); an unauthenticated-export finding still lands critical and
buckets above the CORS row.
Protected: yes (severity calibration) — Zach's go given. HIGHEST CARE: do not reintroduce the
old under-rating anywhere.

## W3 — plain-language headline rewrite only reached the PDF, not JSON/xlsx
The client-facing headline must be the rewritten plain-language text in ALL THREE artifacts
(PDF already good). JSON + xlsx still emit raw rule text ("Deny …") as the headline. Route the
JSON (`build_findings_export`/`findings.json`) and xlsx (`xlsx_export`) headline fields through
the SAME rewrite used for the PDF (`client_headline_and_dtl`/`resolve_floor_finding_text` in
report_export.rs); keep the raw rule prose in an internal-only field.
TEST: export-level assert — no headline in ANY artifact matches the imperative rule-text pattern
(e.g. starts with "Deny "/"Require "/"Disallow ").
Protected: light (export format).

## W4 — headline-rewriter grammar bugs
Bugs: doubled article ("a live a <vendor> secret key") and self-referential substitution ("a
live .env file that is committed to this repository" — the file contains itself). These are
template-variable bugs in the rewriter. Fix the substitution so no doubled article is produced
and the object isn't described as containing itself. GOLDEN-STRING tests for the secret-file and
vendor-token cases.
Protected: light (client-facing wording).

## W5 — sign-off line renders on an unreviewed draft (contradicts DRAFT banner)
Gate the "Prepared and signed off by <name> <date>" line on an ACTUALLY RECORDED human-review
pass. On a draft (no review recorded): render "prepared by (unreviewed draft)" or nothing — never
a name+date sign-off. Template lives in `crates/server/templates/audit_report.typ` + the review-
state plumbing (P5). Keep `sample-report/audit_report.typ` byte-identical if it's a regression
fixture — adjust the sample generator if needed.
TESTS: render from an unreviewed run ⇒ no sign-off wording; render from a reviewed run ⇒ sign-off
present.
Protected: yes (client-facing wording + enforcement gate) — Zach's go given.

## W6 — a failed/timed-out pass must be disclosed, never omitted silently
Last run the alternative-recommendation pass hung to a 300s timeout and the export shipped with
NO mention of it. FIX: when any pass fails or times out, the export must SAY SO explicitly in the
methodology AND the summary (e.g. "rule-alternative recommendations: not computed — pass
failed"). Never omit silently. Also: give the alternatives pass resilience (streaming keepalive /
larger-or-configurable timeout / chunking) so a slow-but-live call isn't killed as a hang; add a
test that a slow-but-live backend call is not killed.
TEST: a run with a simulated failed pass renders the explicit "not computed" disclosure in both
methodology and summary.
Protected: no.
NOTE for the report: cycle 1 did NOT exercise automatic alternative selection at all (it hung),
so state whether cycle 3 will be the first run that does.

## Deferred to cycle 3 (in this order)
hedge-flag single source of truth + action-bucket exclusion; grounded citation required for
AI-tier findings at ≥high; estimate completeness gate + spread; category scorecard denominator;
name the visible mechanism in finding detail (wrong-object re-enable; ignore-file gap).

## Do NOT break
Committed-secret detection (value+rotation+CWE); cross-file reasoning + twin discrimination; all
three top-severity planted defects stay top-tier (do NOT reintroduce under-rating while fixing
R1/R2); the sink+call-site pair stays two rows; plain-language PDF headlines; reconciling summary
math; honest draft banner + zero-auto-excluded disclosure; specific per-finding fixes.
