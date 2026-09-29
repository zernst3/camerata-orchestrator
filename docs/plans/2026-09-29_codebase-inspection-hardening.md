# Codebase Inspection — product-hardening plan (faktura run feedback)

Date: 2026-09-29
Status: planned, awaiting go
Source: the faktura HARD-MODE run review (packaging P1-P7 + detection D1-D6) + five
prior queued fixes. Branch: `feat/audit-report-export`.

## The bar (every output state is measured against these — a state that falsifies one is a bug)
1. One person reads every finding and signs the report (engine reads all; Zach reviews).
2. The fix rate is written into the report before the client decides.
3. Every finding cites the published standard it breaks + the hours to fix.
4. The report discloses how many candidates were thrown out as false positives.
5. A clean report is still the product; nothing is inflated to create work.

Product is now **"Codebase Inspection"** (client-facing strings; internal identifiers may stay).

## Non-negotiable constraints
- **Repo governs itself:** `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, no
  `unsafe`, `cargo test --workspace` all green after every commit.
- **Benchmark hygiene:** the faktura answer key is OUT OF SCOPE — do NOT open/search/ask for
  it. Fix CLASSES of behavior generally; NO fixture-specific pattern-matching (no rules/prompts
  keyed to faktura file/table/string names). A fix that only greens faktura is a failure.
  (Reinforces the general-fixes-not-fixture-bandaids rule.)
- **Extensive testing per item (Zach's explicit emphasis):** every item ships unit tests for
  its specific behavior AND an e2e test through the export pipeline where it touches output,
  each with a positive case and its safe twin. Verification re-runs on faktura +
  `supabase-portal-fixture` + one unseen repo (`membersclub`).
- **Small commits, one concern each.**

## Order of work (from the handoff; P1/P5 first — they make the report say false things)
P1 → P5 → P2 → P3 → D1 → D2 → D3 → P4 → P6 → P7 → D4 → D5, then the 5 queued fixes fold in
where they touch the same files.

---

## PART 1 — Packaging

### P1 — Deduplication + cross-tier merge  🔴  [design: Opus · impl: Sonnet]
Merge overlapping candidates (same file+adjacent lines, OR same root cause across files — a
config flag + its handler, a policy + the page relying on it) into ONE finding. Primary chosen
by: security-class > hygiene/structure-class, then higher calibrated severity, then confidence,
then rule specificity. **A structural/style row must never absorb a security row** (the
reflected-origin-CORS-credentials defect being hidden behind `ARCH-MIDDLEWARE-FIRST-1` is the
canonical failure). Merged finding keeps the union of file:line evidence, best headline/detail,
all citations, absorbed ids in `also_matches` (only rules describing the SAME defect).
Consequence rows fold as "also affects" unless they have an independent fix. ALL summary counts
/ cover / severity×effort matrix / "three things" computed AFTER merge.
- Files: the curation/partition layer in `report_export.rs` + `onboard.rs`; `also_matches` logic.
- Tests: merge-priority unit tests (security>structure, sev, confidence, specificity); cross-file
  root-cause merge; consequence-fold; `also_matches` correctness (no unrelated links); a
  before/after distinct-root-cause count (drop nothing). e2e: faktura-shaped fixture → cover
  count == distinct criticals; CORS defect survives as its own >=medium security finding.

### P5 — Review-state honesty + reconciling narrative  🔴  [design: Opus · impl: Sonnet]
Explicit export review state. **Raw** (no auditor pass): every PDF page carries a "DRAFT: not
yet reviewed by the auditor" banner; narrative says what the ENGINE did, never "reviewed/
dispositioned by the auditor." **Reviewed:** narrative reflects real dispositions. Narrative
ALWAYS reconciles: candidates = curated + held-for-review + excluded-false-positive
(+ accepted-risk), each number shown. Never emit "reviewed by the auditor" unless dispositions
exist. (Fixes the current "41 reviewed; 0 excluded… 18 curated" where 41-0≠18 and nobody
reviewed.)
- Files: `report_export.rs` narrative builder + the Typst template (draft banner).
- Tests: raw export → draft banner + reconciling numbers; simulated-reviewed export → auditor
  numbers reconcile; assert the "reviewed by the auditor" phrase never appears without dispositions.

### P2 — `fix_specific` for every curated finding + consistency self-check  🔴  [design: Opus · impl: Sonnet]
Generate `fix_specific` for EVERY curated finding: concrete to this codebase (names the real
file/symbol/column; where the repo already has the correct pattern, points to it — "use
`safeInternalPath` from lib/redirect.ts as app/auth/signout/route.ts does"). 1-4 sentences,
repo-style snippet ok. Rule's generic remediation becomes secondary; report/xlsx show
`fix_specific` first. **Self-check:** reject+regenerate a fix that references identifiers absent
from the evidence or asserts a state the detail contradicts (e.g. telling a strict-mode repo to
enable strict mode). NEVER emit an empty fix — on failure, mark needs-review "fix not
generated," and it cannot be in do_now. Do NOT leak detection methodology into the fix.
- Files: the fix-generation path (a new AI generation step + validator) in `ai_audit.rs`/
  `report_export.rs`; the `resolve_fix`/`fix_specific` seam.
- Tests: empty-fix guard (no curated finding with empty `fix_specific`); self-check rejects a
  fix naming an absent identifier + one contradicting the detail (strict-mode case); methodology
  never leaks; do_now gating on presence of a fix.

### P3 — Citation gate (no model-inferred citations in curated)  🔴  [impl: Sonnet]
AI-tier findings map to a grounded corpus rule / authority BEFORE curation, inheriting real
citations (RLS→Supabase guide+OWASP A01; stored XSS→CWE-79+React dangerouslySetInnerHTML; open
redirect→CWE-601; weak token→CWE-330/338; filter/query-grammar injection→CWE-943/74+PostgREST;
CORS+credentials→CWE-942+MDN/OWASP). No grounded mapping → stays needs-review "uncited," cannot
be curated. Prefer ADDING a grounded rule over inventing a citation (grounding = published
standard / real linter rule; a Camerata doc is not grounding).
- Files: the AI-finding→rule mapping + the curation gate; corpus rule additions.
- Tests: zero curated findings with model-inferred/empty citation; an unmapped AI finding is
  held out; citation URLs resolve (format/known-authority check).

### D1 — `SECURITY DEFINER` w/o pinned `search_path` (regression)  🔴  [impl: Sonnet]
`search_path_checker` exists in `crates/checks/src/supabase/`; confirm it runs in the brownfield
scan path, on which globs, and that its output reaches the curated set (the older
supabase-portal-fixture caught this; faktura regressed). Wire it end-to-end. Severity High.
Citation: PostgreSQL "Writing SECURITY DEFINER Functions Safely" + Supabase linter function
search-path check.
- Tests: positive (definer w/o search_path flagged High) + safe twin (invoker w/ search_path
  spared); a scan-path test that the checker's output reaches curation.

### D2 — Insecure randomness for security tokens  🔴  [impl: Sonnet]
Floor rule: non-crypto PRNG (`Math.random`, `random.random`, `rand()`…) building a value whose
name/usage marks it a token/secret/access-id/password/nonce/share-link. Discriminate vs
non-security use (UI jitter, sampling). Severity Medium (High if the token alone grants access).
Citation CWE-338/330 + OWASP.
- Tests: positive (Math.random → share-token, flagged) + safe twins (crypto.randomUUID spared;
  Math.random for UI jitter spared).

### D3 — Query-grammar injection is an injection class, not hygiene  🟠  [impl: Sonnet]
Treat user-controlled data concatenated into a query/filter/expression GRAMMAR (PostgREST
`.or()`/`.filter()` strings, raw SQL, Mongo `$where`, LDAP filters) as INJECTION at High —
contrast the safe bound-argument pattern (`.eq(col,value)`, escaped `.ilike`). Not downgraded by
"RLS probably contains it" (defense-in-depth failing at the boundary). `fix_specific`: rewrite
with bound methods / escape reserved chars. Citation CWE-943/74 + PostgREST docs.
- Tests: positive (interpolated `.or()` → High injection) + safe twin (bound `.eq`); assert not
  hedged to needs-review by an RLS-present signal.

### P4 — Floor findings get finding-level treatment  🟠  [impl: Sonnet]
Deterministic/floor findings get the SAME shape as AI findings: specific headline (what's wrong
in THIS repo), one plain-language founder line, detail (what/where/impact), `est_hours` + effort,
`fix_specific`. The gate's "Deny…" phrasing NEVER appears client-facing. Carry cheap context
facts (committed secret: is it `.gitignore`d, what key kind, is it in git history).
- Tests: no client string starts with "Deny"; every curated finding has `est_hours`; a
  committed-secret floor finding renders a real headline + context facts.

### P6 — Cover, branding, product name  🟠  [impl: Sonnet]
Title "Codebase Inspection Report"; replace client-facing "audit"/"Audit" (cover, headers,
README.txt, methodology); footer "(advisory, not a certification)" stays. "Prepared by"
configurable, default "Zachary Ernst, Cantus Works"; a signature block exists (promise 1). Real
code volume (files, scannable SLOC excl. vendored/generated, by language); never render "0
characters" (hide if unknown). Counts from post-merge data.
- Tests: cover shows new title + real preparer + real code volume + merged counts; no
  client-facing "audit"; "0 characters" never renders.

### P7 — Needs-review noise + evidence-based alternative selection + stack exceptions  🟠  [design: Opus · impl: Sonnet]
Alternative selection records per rule: chosen alternative, the EVIDENCE (file:line) the repo
actually follows/needs, confidence. No evidence the concern applies (no pagination anywhere) →
rule **not applicable**, emits nothing; NEVER describe a convention as "adopted" without citing
where. Surface selections to Zach (UI + xlsx Coverage "Rules applied" appendix: rule / chosen /
evidence / applicable). **Stack-awareness:** rules whose premise conflicts with an idiomatic
platform pattern (Supabase Edge Functions, Next.js route handlers) carry a stack exception or
are suppressed for that stack. Group needs-review structural rows into one "Structure and
consistency observations" finding per rule (all locations, informational, outside curated by
default). Fix/disable `JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1` per the July corpus audit.
- Tests: no finding asserts "adopted" without an evidence line; not-applicable rules emit
  nothing; monolith-first spares Supabase Edge Functions; route-placement fixed/suppressed;
  grouped structural finding; needs-review volume drops without losing a security finding.

### D4 — Wrong-table re-enable narrative  🟢  [impl: Sonnet]
When a migration disables a protection on table X and later enables it on Y≠X in the same file,
say so in detail+fix ("meant to turn it back on, turned it on for the wrong table"). Generalise
to triggers/constraints.
- Tests: disable-X-enable-Y detected + narrated; disable-X-enable-X not mis-narrated.

### D5 — Severity calibration rules of thumb  🟢  [impl: Sonnet]
Encode in calibration (not per-run model whim): critical = unauthenticated or full-account
compromise; authenticated cross-tenant read = High unless data class escalates (payment/creds);
record the rationale. Floor findings not under-rated because "RLS probably contains it."
- Tests: cross-tenant authed read → High; a payment-data cross-tenant → escalates; rationale recorded.

### D6 — Test-code signals  🟢  KEEP (regression-protect: in-test flags stay informational + `in_test`; "test asserts a stub" class stays).

---

## The 5 previously-queued fixes (fold in where they touch the same files)
- **Cache `ai_covered`** (incremental poisoning; serde-default false self-heal). [Sonnet]
- **Cost estimator** (count recommendation+resolution passes; backend-aware cache; widen output). [Sonnet] — revisit AFTER P1 (counts change).
- **Calibration transcript** (thread `feedback` into `verify_findings`; record prompt/output). [Sonnet]
- **Rule-alternatives panel** (fixed-height internal scroll + uniform card layout). [Sonnet] — pairs with P7's UI surfacing.
- **Bombe animation** (capture `LoadingCount` in-scope, pass into the spawn's guard; async-spawn test; live-verify). [Sonnet]

## PART 3 — Do NOT break (regression-protect with tests)
Committed-secret dotfile detection; cross-file joins; sparing the look-alikes (non-exposed
schema, server-only service key, signature-verified webhook, owner-scoped write policies,
JSX-escaped output, validated redirect, intended public bucket); plain-language AI openers;
"three things" + what's-healthy; the real extra findings (sequence-then-insert race, stub-
asserting test, FK-index gap, webhook dedup).

## Model tiering
- **Opus (orchestrator, me):** overall architecture; DESIGN of P1 (merge semantics), P2
  (fix-gen + self-check), P5 (review-state), P7 (evidence/stack) ; verification + integration.
- **Sonnet:** all implementation + the mechanical detection items (D1-D5) + P3/P4/P6.
- **Fable:** reserved — offer only if P1's merge-priority algorithm proves the single hardest
  design (otherwise unused, per the reserve-Fable discipline).
- **Haiku:** not used (every item has correctness stakes).

## Verification (run before "done")
`cargo fmt --check` + `clippy -D warnings` + `cargo test --workspace` green; the per-item unit +
e2e tests above; re-run the inspection export on faktura, supabase-portal-fixture (no
regression — must still find D1's search-path class), and one unseen repo (membersclub). Report:
findings.json summary; curated list (rule/sev/file:line/headline); counts of empty
`fix_specific`, uncited curated, "Deny…" strings (all 0); cover text; not-applicable rules +
evidence. Zach re-grades against the key himself; do NOT self-grade against planted answers.
