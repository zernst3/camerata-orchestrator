# Comprehensive remediation design (2026-10-16)

Scope: everything the 2026-10-16 five-repo graded sweep surfaced, plus the nine proposed
consistency rules, plus the residuals carried from the Pass 1-3 effort. Zach: *"I want a total,
comprehensive fix for EVERYTHING."*

**Nothing in here is built yet.** This document exists to be approved, amended, or re-ordered first.

---

## 1. The evidence base

Five repos scanned at engine `50d03a7`, all five integrity gates PASS, all five graded by
independent contextless processes. Artifacts in `_runs/sweep5b-2026-10-16/`.

| Repo | Planted recall | Gate | Cost |
|---|---|---|---|
| faktura | 12 of 12, 0 missed | PASS | $11.11 |
| supabase-portal | 7 of 7, 0 missed | PASS | $10.52 |
| lumen | 11 of 12 shared, +1 new class | PASS | $13.96 |
| tutorly | 11 of 12 shared | PASS | $11.50 |
| hearth | 23 strict + 4 partial + 7 kind faults + 12 missed, of 46 | PASS | $16.44 |

All four grader axes read NOT MET on all five repos.

### The single most important reading

**Detection is not the bottleneck.** Three of five repos found every planted defect. Hearth's
misses are overwhelmingly *regressions*: the grader records that the auth-configuration class
"fired as three exact rows last run and fires on nothing now", the role-scoped SELECT policy
"was a critical last run and is absent now", and money-as-float "was found last run and is
absent now, with its owning rules Clean and one of them listed under What's healthy."

So the work is **restore and repair**, not **author**. That distinction drives the sequencing
in section 4.

---

## 2. What is actually wrong, in three categories

### 2A. Regressions (highest recall value, no new rules)

Classes that produced rows on 2026-10-09 and produce none at engine `50d03a7`:

- Auth configuration: wildcard redirect allowlist, month-long token lifetime, anonymous
  sign-ins while policies assume vetted users. Three exact rows then, zero now, owners "Clean".
- Role-only SELECT policy on a participant table. Critical then, absent now.
- Money stored as floating point, plus missing uniqueness on a bookable slot. Owner now
  appears under "What's healthy".
- CI step printing the environment with secrets in scope. High do-now then; now a
  "zero violations" convention row whose own text says "this is an active violation".
- JWT-off function with no compensating check. Lost its root-cause row, keeps two MEDIUM facets.
- A config-block iterator that previously skipped the first block now reports nothing on any block.
- Tutorly: serverless CORS class absent outright.
- Lumen: private-bucket read policy scoped to "any signed-in user"; owning rule prints Clean.

Also not strictly a regression but the same shape: raw SQL through the driver's unsafe method
with an interpolated request value was **read and reported as a missing-index performance
issue**. The engine saw it and framed it wrong. Sixth time this class has been asked for.

### 2B. Kind and honesty integrity (the mid-layer)

- **13 of 26 convention rows on hearth are not conventions the codebase follows.** Seven of
  them are planted defects reported as "convention the codebase consistently follows, zero
  violations" with the violation described in the same row.
- A convention row may never endorse a site another row reports as a defect (all five repos).
- `not_assessed` is **0 on every repo**, while owners print "Clean" at sites the same export
  reports as critical. The honesty kind exists and is never emitted.
- Kind must be derived from the fault, not from the rule's option list. Objective faults are
  being classed as deviations, which also caps their severity.
- Deviation hygiene: no 1-of-1 deviations; a denominator every time; the convention must not
  equal the deviation; one row per convention, not two.
- Scorecard and "What's healthy" must be computed from the rows in the export, not from rule
  self-reports.

### 2C. Presentation and product (client-facing)

- **The auditor's absolute filesystem path appears in the client PDF on all five repos**:
  64 (hearth), 60 (lumen), 32 (supabase-portal), 22 (tutorly), 16 (faktura). Also 30-32
  occurrences in the xlsx.
- **The engine scanned its own scan-cache file**, written into the repository root, and
  re-detected a snippet inside it as a CRITICAL at queue position #1.
- Pipeline vocabulary, retry narration, CI-state text and bracketed calibration notes in
  client-facing fields (42, 44, 42 occurrences).
- Convention rows render as held findings with "Fix:" lines (19 on faktura, 28 on hearth)
  rather than as their own section.
- Ranking: 31 lint preview rows at HIGH outranking every planted HIGH; platform-owned tables
  at CRITICAL in the top-3; "confirm intent" public policies raised LOW to HIGH; missing-test
  rows at CRITICAL; top-3 listing one root cause three times.
- Merge: 26 sites with two or more rows; config + handler, preview + deterministic, and
  consequence rows all unmerged.
- Fix quality: 9 to 11 contradictory fix pairs per repo, templates fired at the wrong target
  ("set strict mode" on a strict project, "remove pagination" on an unpaginated list),
  7 placeholder-token fixes, 34 unestimated rows, 3 held critical rows with no fix, 6 uncited.
- Field agreement: 38 rows with a baseline-suppressed status but an Open disposition;
  status/kind/disposition triples inconsistent; provenance mislabels.

---

## 3. Proposed workstreams

Each is independently shippable and testable. Dependencies are stated.

### W19 - Regression restoration and a regression gate
Restore every class in 2A. **The durable half is the gate**: capture the 2026-10-09 export as a
baseline and fail the sweep when a class that fired at severity >= medium on a prior export of
the same commit goes to zero. Without that gate this recurs silently, which is exactly how it
happened.
*Depends on: nothing. Highest recall value.*

### W20 - Scan scope and engine artifacts
The engine must never write artifacts into the scanned tree, and must never scan its own
output. Covers the scan cache at repo root and the ledger file.
*Depends on: nothing. Smallest, and it removes a CRITICAL from position #1.*

### W21 - Repository-relative paths everywhere
Every location in every artifact is repo-relative. The row's path is the faulty file, never a
sentence, never the auditor's home directory. One normalisation point, enforced by a gate that
fails on any absolute path or path separator mismatch reaching an artifact.
*Depends on: nothing. This is the single most client-visible defect.*

### W22 - Kind integrity
Kind derived from the fault, not the option list. A convention row may not contradict a defect
row on the same site, carry counter-evidence, carry a fix, or carry an estimate. No 1-of-1
deviations. Denominator required. Convention may not equal deviation.
*Depends on: nothing, but shares files with W23.*

### W23 - `not_assessed` and honesty surfaces
A rule contradicted by another row in the same export becomes `not_assessed`, never Clean and
never "What's healthy". Scorecard and What's healthy computed from rows.
*Depends on: W22 (same code paths). Ship together or in order.*

### W24 - Merge and ranking
Same-root-cause merge: config + handler, preview + deterministic of one rule id, consequence
rows. Preview rows inherit the deterministic twin's severity and merge *under* it. Severity
anchors: platform-owned tables, intentional-public resources and "confirm intent" rows capped
at INFO/LOW and never do-now; missing-test rows never CRITICAL. Top-3 selects three distinct
root causes.
*Depends on: W22 (kind-aware merge already exists; this extends it).*

### W25 - Client-facing text hygiene
No pipeline vocabulary, retry narration, CI-state text, calibration notes, internal index
references or placeholder tokens in any client-facing field. Enforced by a gate over the
rendered artifacts, not by review.
*Depends on: W21 (same artifact-scanning gate can host both).*

### W26 - Fix and estimate quality
Predicate-aware fix templates. A contradiction gate: a fix may not contradict its own finding
or a sibling row. A placeholder gate. Magnitude sanity on estimates. No row at high or above
without a fix and a citation. Floor rows always estimated.
*Depends on: nothing.*

### W27 - PDF restructure
Conventions in their own section, rendered as a compact table, never as findings with fixes.
Deviation denominators rendered. Curated contains only open rows ordered by severity, with no
held, INFO or convention rows inside it. Held never opens with an uncited row.
*Depends on: W22, W25. **This reopens the Pass-3 PDF exemption** (section 5, question 1).*

### W28 - Field agreement at export
status/disposition, provenance, and kind/bucket reconciled at the export boundary, with a gate.
Covers the 38 baseline-suppressed-but-Open rows.
*Depends on: nothing.*

### W29 - The nine consistency rules plus the clone detector
Dead code, shared constants for domain literals, one import path per symbol, React hooks
correctness, needless complexity, single-source application logic, one concept one name,
repeated UI wiring, one status one variable. Five fully mechanical; three share a clone-detector
pass; one is judgment with a deterministic edge.
*Depends on: the eslint provisioning fix (below) for the hooks rule. **Additive coverage, not a
fix for anything the sweep measured** - see section 5, question 2.*

### W30 - Carried residuals
- `--prefer-offline` turns a stale npm cache into a permanent provisioning failure. Retry
  without it on ETARGET. **Blocks W29's hooks rule.**
- §9.1: a surface enumerating multi-option rules the scan found nothing for, so
  `RuleAlternativesPanel` can finally be removed.
- Conventions tab count can read lower than its own list.
- `is_code_auditable_rule` still gates on a name prefix rather than the `inspectable` field.
- D6/D7 severity *ceiling* tables still use raw `contains`, the same shape as the D5 floor bug
  that was inflating severity on anything mentioning signed URLs.
- R1 (`cargo fmt`) and R2 (the formatter directive as a Camerata custom rule).

---

## 4. Recommended sequencing

Three waves. The ordering is driven by one principle: **restore what broke before adding
anything, and fix what the client sees before what only we see.**

**Wave 1 - stop the bleeding.** W19 (regressions + gate), W20 (scan scope), W21 (paths).
Highest recall recovery, removes the worst client-facing leak, and W19's gate prevents the
whole category from recurring.

**Wave 2 - the mid-layer.** W22, W23, W24, W28. These share code paths and should land in
order, not in parallel. This is where the kind faults and the ranking inversions live.

**Wave 3 - the deliverable.** W25, W26, W27. The PDF restructure lands last because it depends
on kind integrity being correct first; restructuring a renderer over wrong data would bake the
wrong shape in.

**Then W29**, as the coverage expansion it is, once the mid-layer is stable enough to bisect
against. W30 items slot in wherever they unblock something, with the npm fix before W29.

**Re-measure after each wave, not after each workstream.** Per Zach's standing rule, no
incremental graded runs; but a wave boundary is a legitimate measurement point and each wave is
large enough to move the number.

---

## 5. Outstanding questions

**Q1. The PDF exemption.** Pass 3 was told "the PDF is the summary, already correct in shape;
leave it alone." Grader v2 measured it for the first time and it is not correct in shape. W27
assumes that ruling is lifted. Confirm.

**Q2. Where do the nine rules sit?** They address refactor-commit coverage, a real and separate
gap, and would have caught **none** of hearth's misses. Wave 3 as written, or earlier because
the coverage gap matters independently? My recommendation is after Wave 2.

**Q3. The regression baseline.** W19's gate needs a reference export. The 2026-10-09 run is the
obvious candidate, but it was produced by an engine with a dark semantic tier and a broken
eslint path, so it is a *low* bar in places and a *correct* bar in others. Options: use 10-09
as the floor only for classes it detected; or re-baseline from this sweep and accept that the
regressed classes stay lost until W19 restores them. I lean toward the former.

**Q4. Severity anchors.** W24 proposes capping platform-owned tables and intentional-public
resources at INFO/LOW. That is a judgement about what is a finding at all. Should an
intentionally-public bucket produce a row the client sees, or no row?

**Q5. Convention rows in the deliverable.** All five graders objected to conventions rendering
as findings with fixes. W27 moves them to their own table. Should adopted conventions appear in
the client PDF at all, or only in the workbook and `AGENTS.md`?

**Q6. Budget and cadence.** This sweep cost $63.53 and 6h48m of scanning plus 1h34m of grading.
Three waves with a measurement after each is roughly $200 and three working days of machine
time. Confirm that is acceptable, or tell me to measure only after Wave 3.

**Q7. The one thing I would push back on.** W29's nine rules are good work, but the independent
review's warning from earlier in this effort applies directly: *"adding detection capacity while
the mid-layer eats existing recall is how the last two cycles were spent."* Hearth lost five
working detector classes between two runs. I would not add nine rules until the gate in W19
exists to catch the next five.

---

## 6. Zach's rulings (2026-10-16)

**Q1 PDF exemption: LIFTED.** "Yes, please correct the pdf." Superseded by a full redesign -
see `docs/plans/2026-10-16_pdf-redesign-report.md`, commissioned the same day against his
investor-side report research (six additions, three deliberate omissions, no score, no
go/no-go verdict, no bus-factor analysis).

**Q2 The nine rules: in scope, timing flexible.** "When is not important, as long as it is part
of the total package." W29 stays after Wave 2 as recommended, but it ships.

**Q3 Regression baseline: delegated, with a constraint.** "Most importantly is that we restore
this regression while also not introducing new ones, that is paramount." Taking the 2026-10-09
export as a floor ONLY for the classes it actually detected, since that engine had a dark
semantic tier and a broken eslint path and is therefore not a valid ceiling. The gate asserts
no class regresses from whichever prior export last detected it.

**Q6 Budget: deferred.** "We can worry about this later. Let's get all the changes and fixes in
before we grade again." So a single graded sweep after all three waves, not one per wave.

### [x] Q4 ANSWERED, and Zach's answer is better than the proposal

The question was how "intentionally public" could be derived from code. It splits:

- **Platform-owned objects ARE deterministic.** Supabase creates `storage.objects` and similar
  with RLS already enabled; the engine can know the platform's own schema namespace. The hearth
  case was the RLS-policy-without-RLS rule firing CRITICAL on exactly such a table, with its own
  headline reading "informational only" and its own fix saying "no new migration is needed".
- **"Intentionally public" is NOT derivable** and must not be guessed. Nothing in the code
  separates a deliberately public catalog-photo bucket from an accidentally public
  user-document bucket, and inferring it from names is the prose-matching this effort removed.

I proposed a client-declared allowlist. **Zach proposed better:**

> "maybe another flag to add to the finding, instead of just plain text. A flag that says 'this
> could be intentional'. And then, for those in particular, I can mark as 'violation' in the case
> that the agent's guess was wrong. OR I can flag it as 'genuinely up to the client to decide if
> intentional' and that latter one would put it in a bucket that would then make it to the
> product as 'for your visibility, this was found and could likely be intentional, that is your
> call'."

Why it is better: an allowlist requires the client to pre-declare something they will never
think to declare, whereas this puts the judgement with the curator at review time, where it
actually lives, and converts an awkward silence into a deliverable section.

It also fixes the defect at its source. The engine currently writes "very likely intentional"
into **detail prose**, which no renderer and no severity pass can read, so three such rows
shipped at HIGH held. A structured flag is readable by the severity ceiling, the ranker and the
renderer - the same prose-to-structure correction applied everywhere else in this effort.

**Three orthogonal axes, not one overloaded one:** `FindingKind` says what kind of statement a
row is; `could_be_intentional` is the engine's read on whether the author meant it; the
disposition is what the curator decided.

**Severity consequence:** a flagged row is capped at INFO/LOW until the curator marks it a
violation, at which point it takes its authored severity. That single rule removes the
"HIGH held, detail says very likely intentional" contradiction the grader raised on three repos.

**New client-facing outcome.** Today a finding is an accusation or it is absent. This adds a
third: "we found this, it may well be deliberate, your call." That is honest in a way neither
of the other two is, and it belongs in the PDF redesign as its own section.

#### [ ] Open sub-questions
- **Untriaged rows.** Does an unresolved `could_be_intentional` ship as "your call", or hold the
  sign-off like an open finding? Recommendation: ship it. The client-visible outcome is honest
  either way, and holding sign-off would punish the curator for the engine's uncertainty.
- **Persistence.** Should a curator ruling survive into the next scan, or is the client re-asked
  every time? Durable per-finding state across engagements is the backend surface deliberately
  deferred when `Confirmed` was kept local (Pass 2 Q2). This is now the SECOND feature to want
  it, which starts to make the case for building it properly.

#### Workstream placement
New **W31 - the `could_be_intentional` flag**: schema field, severity cap, curator disposition,
and its own client-facing section. Depends on W22 (kind integrity) since it shares the finding
model, and feeds the PDF redesign. Sits in Wave 2.
