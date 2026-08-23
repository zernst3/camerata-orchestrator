# Scan-Quality Refinement Design — Benchmark Follow-up (Bugs 3, 4, I1, I3)

Repo: `camerata-orchestrator`, branch `feat/audit-report-export`.
Scope: general mechanisms only — no fixture-specific suppressions, no hardcoded paths, no per-rule-id kill switches. The benchmark harness (`crates/server/tests/benchmark_supabase_fixture.rs`) verifies; the "what worked" list is the regression contract.

Grounding (verified against source):

| Fact | Anchor |
|---|---|
| Location merge is exact `(path, line)` keyed, snippet-presence-guarded | `ai_audit.rs:1589` `merge_by_location`, `MIN_MERGE_SNIPPET=8` at `:1634` |
| Primary selection: adopted (non-`AI-`) > severity > earliest; max severity kept; rest → `also_matches` | `ai_audit.rs:1499` `merge_location_group` |
| Snippet-anchored line resolution already exists | `ai_audit.rs:1549` `resolve_finding_lines` |
| AI findings DO pass suppression classification | `onboard.rs:856` → `onboard/audit.rs:150` `classify_repo_findings` |
| Inline waiver reach is same-line or line-directly-above only | `suppression.rs:221` `inline_applies` |
| `suppressed-inline` findings classify as `Unresolved` in the curated report (only `suppressed-baseline` → `BaselineAccepted`) | `report_export.rs:152` |
| Matrix: critical→do_now; high+low-effort→do_now; high→do_next; medium/low→plan | `report_export.rs:689` `matrix_bucket` |
| Calibration verdict JSON already carries severity/confidence/effort per finding | `ai_audit.rs:569` `apply_verdicts` |
| Proportionality ("judge findings proportionally to {files_count} code files") exists but only in `thorough` mode | `ai_audit.rs:654-663` `verify_findings` |
| RLS checker demotes non-exposed-schema no-RLS to `medium` instead of omitting | `checks/supabase/rls_checker.rs:98-113` |
| Test-scope downgrade (low + `in_test` + `needs_review`) is floor-side | `onboard/audit.rs:85-104` |

---

## Built (2026-08-23)

All four items shipped on `feat/audit-report-export`, in the planned build order, each its own commit:

- **Item 4 / I3** (`378c2a5`) — exposure-gate missing-RLS: a non-exposed schema with no RLS policy is an informational note, not a defect. RLS intent rules stay ungated (owner decision 7: only missing-RLS is exposure-gated).
- **Item 3 / I1** (`046434f`) — inline waiver reach as a contiguous non-blank run capped at `MAX_WAIVER_REACH = 10`, `also_matches` waiver matching, and a `WaivedInline` disposition so a suppressed-inline finding lands in the Accepted cell instead of reading as still-open.
- **Item 1 / Bug 3** (`3ec4554`) — cross-family semantic dedup: `Finding.category` (closed taxonomy) + `located` bool, a `category` field on the calibration verdict schema, and `merge_semantic_groups` (only same-category findings fuse; `None`-category never merges — fail-open to over-telling).
- **Item 2 / Bug 4** (`626c7d3`) — informational bucketing: new `info` severity tier, a `MatrixJson.informational` appendix cell, and the `is_informational` predicate (info-severity, `testing-style` below `MIN_STYLE_CORPUS_FILES = 3`, `needs-review` at ≤medium, and absence-type `structured` stance rules in the universal/framework layer). Hard invariants: never a critical/high finding, never a dispositioned one; informational is excluded from `curated_total` so `do_now+do_next+plan+accepted == curated_total` still holds. Proportionality prompt is now **unconditional** (was thorough-only) with a real repo-shape sentence (detected stack + code-file count) instead of a hardcoded threshold.

Owner decisions applied: dedup window ±5 / same-construct; informational findings and needs-review are **visible, never hidden** (decision 2 — the appendix is report-JSON-level; needs-review is re-bucketed, not excluded from what curation sees, softening 2c); `MIN_STYLE_CORPUS_FILES = 3` (4); `MAX_WAIVER_REACH = 10` (5). **Deferred:** the foreign-suppression recognizer (decision 6 — `camerata:allow`-only this pass) and any change to prose stance-rule I1 handling; RLS intent-rule exposure-gating (7) is confirmed out of scope.

Verification: the deterministic sub-tests are green and `cargo test -p camerata-server -p camerata-checks -p camerata-rules` + `cargo check --workspace` pass; the `#[ignore]`d full-grade run (live model key) was not executed here.

---

## 1. Bug 3 — Cross-family semantic dedup

**Problem.** `merge_by_location` only collapses findings at the *identical* `(path, line)`. The same defect flagged by two rule families a few lines apart survives as two rows: `AI-RLS-PERMISSIVE-TRUE-POLICY` (AI attribution, line ~21) + `SUPABASE-RLS-PERMISSIVE-TRUE-1` (native checker, line 24); `SERVICE-ROLE-BYPASS` + `FETCH-THEN-AUTHORIZE` on the same handler body.

**Mechanism: a second, semantic merge pass** — `merge_semantic_groups`, run AFTER `resolve_finding_lines` + `merge_by_location` (so exact-location merging and snippet anchoring have already happened, and the pass operates on the reduced set).

### 1a. Semantic category (the new key component)

Add `category: Option<String>` to `Finding` (serde-defaulted, back-compatible), drawn from a **closed taxonomy** (~12 values): `authorization`, `authentication`, `secret-exposure`, `injection`, `transport-security`, `rls-policy`, `resource-exposure`, `input-validation`, `error-handling`, `arch-conformance`, `testing-style`, `performance`.

Assignment, in priority order:
1. **Deterministic sources** (floor rules, native checkers, preview tools): a static `rule-id → category` table in the rule registry / checker constants. These are our own finite rule sets; the table is exhaustive by construction and repo-agnostic.
2. **AI findings**: the calibration verdict JSON gains a `category` field (same closed set, listed in the calibration prompt). `apply_verdicts` accepts only known values — mis-shaped → fall through to 3.
3. **Fallback heuristic**: token map over the rule id (e.g. tokens `RLS`,`POLICY` → `rls-policy`; `AUTH`,`AUTHORIZE`,`BYPASS` → `authorization`; `TLS`,`SSL` → `transport-security`). No match → `None`.

`category == None` on either side ⇒ **never semantically merged** (fail-open to over-telling, per the wrong-merge-hides-a-finding rule).

### 1b. The merge predicate

Two findings `a`, `b` (post location-merge) collapse iff ALL of:

1. `a.path == b.path`
2. `a.category == b.category` (both `Some`)
3. **Overlap window** — one of:
   - `|a.line - b.line| <= SEMANTIC_MERGE_WINDOW` (**proposed: 5**; covers the benchmark's observed 3-line drift and stays inside the harness's own ±2 attribution tolerance with margin), OR
   - both lines fall inside the same enclosing construct span (cheap brace/indent-depth scan of the file finding the smallest top-level block containing each line — this is what unifies `SERVICE-ROLE-BYPASS` + `FETCH-THEN-AUTHORIZE` on one handler body when they're >5 lines apart). Construct-span resolution failure ⇒ window rule only.
4. **Wrong-fusion guards** (each independently blocks the merge):
   - Both findings are deterministic (floor/checker/preview): **never merge**. Deterministic rows are exact and distinct by construction — two checker rows are two defects. (This preserves the existing "SECURITY-HEADERS fused with API-VERSIONING" fix: the located-snippet guard in `merge_by_location` stays untouched; this pass adds a parallel guard.)
   - Both carry a structural `object` (the checker's `schema.table` / policy name) and the objects differ: never merge — same category, adjacent lines, different tables are distinct defects.
   - AI+AI pairs additionally require snippet corroboration: one snippet contains the other, or both snippets resolve (`resolve_finding_lines` succeeded — real code, not paraphrase) inside the same enclosing construct. Two AI findings citing *different real code* in the same category that merely sit near each other stay separate — this is the cross-line conceptual-dupe boundary Zach explicitly wants kept separate.

### 1c. Merge semantics

Reuse `merge_location_group`'s shape with one extension to the primary ordering: **deterministic origin beats adopted beats `AI-`**, then severity, then earliest. The primary keeps its own `line` (the deterministic side's exact anchor — this is what provably preserves D1/D2/D7 exact-line grading), max severity is kept, every demoted distinct rule id lands in the existing `also_matches`, and the demoted finding's detail is NOT concatenated (structure over prose, consistent with `ai_audit.rs:610-613`).

### 1d. Benchmark verification

- New harness checks (fixture-string-free, real-signal): for each graded defect D1–D7, **exactly one** finding row exists in its expected file+range ("SINGLE-ROW-Dn" checks). Today D2 and D5 produce ≥2 rows across families; post-fix they must be 1 with the sibling id present in `also_matches`.
- Existing D1/D2/D6/D7 line+severity assertions are untouched and must stay green — the deterministic-primary rule guarantees the graded rule id and line survive as the primary.
- PAIR-RLS-PERMISSIVE stays green structurally: the spared INSERT policy has no finding, so there is nothing within the window to fuse; if a future regression flags it, the differing-object guard prevents it hiding inside the SELECT row.
- Unit tests in `ai_audit.rs`: (a) det+AI 3-lines-apart same-category merges, det primary+line wins; (b) AI+AI same construct merges; (c) same category different `object` does NOT merge; (d) `category: None` never merges; (e) two deterministic rows never merge.

---

## 2. Bug 4 — Taming generic-rule low-tier over-firing

**Problem.** Stance/architecture rules (`ARCH-SERVICE-DI-1`, `ARCH-REPO-PER-AGGREGATE-1`, `ARCH-MONOLITH-FIRST-1`, `ARCH-MIDDLEWARE-FIRST-1`, `ARCH-HOT-READ-CACHE-1`, `ARCH-CURSOR-PAGINATION-1`, `UI-QUERY-LIBRARY-1`, `JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1`, `TESTING-*`) fire "the project hasn't adopted X" findings against a small Next.js app. These land in plan/low and inflate the curation pass. Goal: lighter curation, not a silenced low tier.

**Key real signal already in the pipeline:** `merge_by_location` already computes `located` — whether a finding's snippet is real code present in the file (`ai_audit.rs:1610`). Stance-rule noise is overwhelmingly **absence-type**: line 0 / unresolvable snippet / "no X exists anywhere". Presence-type violations (a service constructing its own repo client, at a real line) are real signal even in a small repo. This distinction is general and needs no per-rule list.

**Recommended combination** (weighed below):

### 2a. New `informational` bucket (option b, scoped by the absence/presence signal)

- Add severity value `"info"` (`severity_rank`: 0) and a `MatrixJson.informational` cell rendered as a report appendix ("Conventions to consider"), **excluded from do_now/do_next/plan** and from the curation pass's default view.
- Auto-bucket a finding as informational when ALL hold:
  1. its rule's corpus entry is a stance rule — TOML `enforcement = "structured"` AND `layer` ∈ {`universal`, `framework`} (i.e. the decision-shaped generic-arch/style family; floor and checker rules are never structured/universal), and
  2. it is **absence-type**: `located == false` after `resolve_finding_lines` (expose the bool on the finding rather than recomputing), and
  3. severity after calibration is ≤ medium.
- **Invariant: a critical or high finding is never auto-bucketed informational**, whatever its rule family — preserves "do_now/do_next hold the true defects".

### 2b. Proportionality context-gate at judgment time (option a, softened)

Do NOT gate whether the rule enters the prompt (a located DI violation in a 20-file app is still real; silencing the rule class loses it). Instead:
- Make the existing `verify_findings` proportionality paragraph (`ai_audit.rs:657`) **unconditional**, not `thorough`-only — it already instructs "over-engineering/YAGNI notes on a small codebase = low confidence, capped severity", which feeds 2a/2c. Zero new mechanism, one `if` removed.
- Add one repo-shape line to the same prompt from signals already computed in `audit_repos`: code-file count and detected stack (`detect_stack`, `onboard.rs:740`). E.g. "This is a {frameworks} repo with {n} code files." The model gates `ARCH-MONOLITH-FIRST-1`-style demands with real context instead of a hardcoded threshold deciding firing.

### 2c. `needs-review` → curated default exclusion (option c)

`confidence == Some("needs-review")` findings with severity ≤ medium move to the same informational appendix (not deleted: still in raw findings, CSV, and the JSON). Today they sit in curated groups with only a chip (`report_export.rs:1267`). High/critical needs-review rows STAY in the matrix (flagged) — a debatable critical is exactly what curation is for.

### 2d. Style-family minimum-corpus gate (the 1-file test suite)

A style rule needs an established corpus before deviations are findings. General signal: for `testing-style` category rules, count test files (`is_test_or_fixture_path`, already available). If the repo has `< MIN_STYLE_CORPUS_FILES` (**proposed: 3**) test files, testing-style findings are auto-bucketed informational regardless of located-ness. Same mechanism generalizes later to other style families keyed on the file population they style-check; only testing is in scope now.

**Why this combination:** (b)+(c) directly cut curation load without deleting anything (over-tell preserved — every row still exists, honestly bucketed); (a-softened) improves calibration generally without a firing kill-switch that could hide a located violation; (d) fixes the 1-file-test-suite case by a population signal, not a rule suppression.

### Benchmark verification

- All D1–D7 severity/tier assertions unchanged (none are stance rules; invariant in 2a protects criticals/highs categorically).
- New harness check (real-signal, fixture-agnostic): **no absence-type structured/universal-layer finding appears in do_now/do_next/plan** — assert over the report matrix, not over rule-id lists.
- New noise-budget check: the matrix (do_now ∪ do_next ∪ plan) contains no finding with `severity == "info"`, and I-file paths contribute zero matrix rows (I4 already asserted; extend to the fixture's app files that GROUND_TRUTH marks clean if it enumerates them).
- Unit tests: bucketing predicate truth-table (structured×located×severity), critical-never-info invariant, test-file-count gate.

---

## 3. I1 — Honoring explicit in-test suppression

**Problem.** The fixture's test-only TLS disable carries an explicit suppression comment (lines 8–13) yet still surfaces as a `low` row. Two general defects found:

1. **Waiver reach is too short.** `inline_applies` (`suppression.rs:221`) covers only the marker's own line or the line directly below. A waiver above a multi-line construct (comment line 8, offending line 13) misses.
2. **Inline-suppressed ≠ handled in the report.** `report_export.rs:152` maps only `suppressed-baseline` to `BaselineAccepted`; a `suppressed-inline` finding classifies `Unresolved` and lands in the matrix as live work. Even a correctly matched waiver would still show in plan/low today.

**Design (general policy):**

- **Reach**: a reasoned `camerata:allow RULE-ID -- reason` waiver on its own line applies to the contiguous non-blank line run that follows it (the statement/block it annotates), capped at `MAX_WAIVER_REACH` lines (**proposed: 10**). Trailing same-line waivers unchanged. This is the standard linter-directive convention generalized to multi-line constructs; cap prevents a stray comment silencing half a file.
- **Report treatment**: `classify` maps `status == "suppressed-inline"` (with no fresh wire disposition) to the existing **accepted** matrix cell — visible, labeled ("waived inline: {reason}"), never in do_now/do_next/plan, never in the curation default view. It remains in the suppression registry (`onboard.rs:479`) with stale-detection intact. Excluding it entirely would violate over-tell; `accepted` is the honest bucket.
- **Foreign/informal acknowledgments** (an `eslint-disable`, `#[allow]`, or prose "intentional for tests" comment adjacent to a test-scoped finding): NOT full suppression (un-auditable, no reason contract). General rule: `in_test == true` AND an adjacent foreign-suppression/ack comment (small recognizer: known linter-directive prefixes within the waiver-reach window) ⇒ auto-bucket **informational** (Item 2's appendix). Un-acknowledged in-test findings keep today's behavior (low + `needs_review`) — a real secret in a test file still merits a look.
- Interaction with dedup (Item 1): suppression classification runs after merging (it already does — `onboard.rs:856`); a merged row is suppressed only if the waiver names the **primary** rule id or any id in `also_matches` (extend `inline_applies`' rule-id match to include `also_matches` — otherwise a merge could resurrect a waived finding under a sibling id).

**Benchmark verification.** The harness's I1 check (`benchmark_supabase_fixture.rs:600-611`) changes deliberately — it currently asserts present-at-low; the new ground truth is "fully honored." New assertion, keyed on which comment the fixture actually carries:
- if it is a reasoned `camerata:allow`: the finding exists with `status == "suppressed-inline"` and appears in NO matrix cell except accepted;
- if it is a foreign/informal ack: the finding is informational (severity `info`, absent from do_now/do_next/plan) and still `in_test == true`.
Deterministic sub-test updated in the same commit as the reach fix (answer-key change is part of the design, not a silent assertion edit). Unit tests: reach cap, blank-line termination, reason-less waiver still non-suppressing, `also_matches` waiver match.

---

## 4. I3 — Unexposed-schema RLS false positive

**Problem.** `rls_checker.rs:98-113` emits a `medium` "defense-in-depth note" for a no-RLS table in a schema absent from `supabase/config.toml`'s `[api].schemas`. GROUND_TRUTH: must not be flagged. The AI tier already spares it by reasoning over `api.schemas` (D1 cross-file reasoning — the must-preserve item); the native checker is the outlier.

**General rule: exposed-schema membership is a reachability precondition, not a severity modifier.** `SUPABASE-RLS-ENABLED-1`'s threat model is "anyone holding the shipped anon key can read/write via PostgREST" — a schema PostgREST does not serve cannot violate it. So:

- The non-exposed branch stops emitting a `Finding`. The defense-in-depth observation moves to the **informational channel** (Item 2's appendix / report notes) as severity `"info"` — same text, honestly bucketed as a suggestion, not a defect. (Full omission also satisfies the benchmark; informational is preferred per over-tell. If Item 2's info bucket ships later, interim behavior = route to report `notes`, not `findings`.)
- Exposure set semantics unchanged: `parse_exposed_schemas` result; missing/unparseable `config.toml` keeps its current default (Supabase's `public`-exposed default) so absence of config never silently marks everything unreachable.
- `SUPABASE-RLS-NO-POLICY-1` (enabled, zero policies) and `SUPABASE-RLS-POLICY-DISABLED-1` (policies written, RLS off) are **not** exposure-gated in this pass: both signal broken *intent* (a feature is broken, or service_role is bypassing) independent of PostgREST reachability. Flagged as a follow-up question, not changed here.

**Benchmark verification.** Harness I3 (`:550-556`, currently an encoded known-gap failure) flips to pass: zero findings for `0001_init.sql`. D1 (exposed `public.member_contacts`, critical, exact line) is asserted unchanged — the exposed branch is untouched. Existing checker unit tests updated: `non_exposed_schema_is_demoted_not_critical` (`rls_checker.rs:196`) becomes `non_exposed_schema_emits_no_finding` (+ asserts the informational note exists); `config_toml_multi_schema_widens_exposure_scope` unchanged.

---

## Must-preserve audit (how each mechanism provably cannot regress the list)

| Must-preserve | Why safe |
|---|---|
| Exact file:line anchoring (D1/D2/D4/D6/D7) | Semantic merge keeps the deterministic/most-specific primary AND its line; `resolve_finding_lines` untouched. |
| Cross-file reasoning (D1 / `api.schemas`) | Item 4 changes only the checker's non-exposed branch; AI-tier prompt/context unchanged. |
| Discrimination (charge/notify, SELECT/INSERT, server-only service_role, anon key) | No mechanism ADDS findings; gates only demote/merge/bucket. Differing-object + snippet-corroboration guards prevent a spared site being fused into a flagged row. |
| Triage tiering (do_now/do_next hold true defects) | Hard invariant: critical/high never auto-bucketed informational; `matrix_bucket` unchanged for them. |
| getSession→getUser (D6) | AI finding with real located snippet: never absence-bucketed; merges only with same-category same-construct rows (none in fixture). |
| I1 correctly downgraded (not lost) | Reclassified to accepted/informational — still present in report JSON + registry; never deleted. |

## Build order

1. **Item 4** (smallest, isolated in `crates/checks`): non-exposed branch → note; update checker tests + flip harness I3. No dependency on the rest.
2. **Item 3 reach + classify fix** (`suppression.rs`, `report_export.rs:152`): both are 1-function changes; update harness I1 + deterministic I1 sub-test in the same commit.
3. **Item 1** (`Finding.category`, calibration schema field, `merge_semantic_groups`): largest new mechanism; lands with SINGLE-ROW-Dn harness checks + the unit-test battery.
4. **Item 2** (info severity, matrix cell, bucketing predicate, unconditional proportionality, test-corpus gate, needs-review exclusion): depends on Item 1's `category` (for 2d) and benefits from 1's dedup shrinking the population first; lands with the matrix-purity harness checks.

Each step ships with docs + tests (standing rule), and each is independently green on the deterministic harness before the next starts; the `#[ignore]`d full-grade run re-scores after steps 3 and 4.

## Decisions routed to Zach

1. **`SEMANTIC_MERGE_WINDOW` = 5** and the enclosing-construct fallback for AI+AI pairs — accept, or window-only (more conservative, leaves the same-handler pair unmerged when >5 apart)?
2. **Informational-vs-excluded policy**: recommended = visible appendix (over-tell). Confirm the appendix also appears in the exported PDF/CSV, or report-JSON only?
3. **`needs-review` ≤ medium default-excluded from curated view** (2c) — this is the one lever that reduces what curation *sees* rather than re-bucketing; confirm.
4. **`MIN_STYLE_CORPUS_FILES` = 3** for the testing-style gate.
5. **`MAX_WAIVER_REACH` = 10** lines for block waivers.
6. **Foreign-suppression recognizer** (eslint-disable/`#[allow]`/etc. → informational for in-test findings): in scope now, or camerata:allow-only for this pass?
7. **Exposure-gating `RLS-NO-POLICY`/`RLS-POLICY-DISABLED`** (Item 4 follow-up question) — deliberately not changed; confirm defer.
