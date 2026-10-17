# PDF redesign report - the client-facing audit deliverable (2026-10-16)

**Status: DESIGN. No code, no template, no rule changes in this pass.**

Inputs read for this design: Zach's investor-side report conventions research
(`Cantus Works/Marketing/Drafts/Investor-side report conventions.md`, 2026-10-09), the
current sample PDF and its page renders (`sample-report/`), the shipping Typst template
(`crates/server/templates/audit_report.typ`), the five-repo graded sweep findings
(`docs/plans/2026-10-16_comprehensive-remediation-design.md`, section 2C), the finding
taxonomy (`crates/api-types/src/audit.rs`), the scan ledger
(`crates/server/src/scan_ledger.rs`), the export JSON contract
(`crates/server/src/report_export.rs`), and the Pass 2 curator design
(`docs/plans/2026-10-11_pass2-curator-ui-design.md`, section 3).

Relation to the remediation plan: this document is the design half of **W27 (PDF
restructure)** and deliberately goes further than W27's minimum. W21 (repo-relative
paths), W22 (kind integrity), W23 (honesty surfaces), W24 (merge and ranking) and W25
(text hygiene) are prerequisites for shipping the redesigned PDF to a client; they are
not re-designed here, only depended on.

---

## 1. The recommendation in one paragraph

Rebuild the report around the investor-side skeleton Zach's research identified, in this
order: cover, contents, a questions-first executive summary that answers the client's
own three intake questions with a yes or a no and a pointer, a remediation budget table
with one total, a scope and files-examined section built from the scan ledger, a risk
register (the current curated findings, relabelled and purged of non-defect rows), a
code quality and debt section (deviations and no-convention splits, rendered as their
own thing with numerator and denominator), an engineering-conventions-in-force section
(the client's own adopted conventions, closing the loop on each area), a dependency
health section extended with a licence table, a verified-clean section computed from the
ledger predicate, next steps, methodology, disclaimer and signature, and a citations
appendix. Cut the severity x effort matrix page, cut the standalone category scorecard
page, cut the held-for-review section from any signed deliverable, and strip every piece
of pipeline vocabulary (effort chips, confidence chips, disposition strings) from
client-facing fields. No score, no go or no-go verdict, no git-history team analysis,
anywhere, ever. The deterministic floor renders as labelled observations the client has
fact-checked; the advisory tier renders as labelled judgments that sit on those
observations.

This is a single recommended design, not a menu. Where a genuine fork exists it is in
section 10 as an open question; everything else is committed.

---

## 2. The reader model

Three readers, in decreasing order of time spent:

1. **The founder who commissioned the audit.** Reads the executive summary, the budget
   table, and the top of the risk register. Decides whether to buy the retest. The
   research is blunt that the executive summary is the only part most readers finish,
   so pages 3 to 5 must carry the entire story on their own.
2. **An investor reading the data room.** Reads the budget table first (it prices the
   deal adjustment), then the scope page (it tells them what the document is worth),
   then skims the risk register headlines. Never reads a fix body. This reader is why
   the budget table and the scope page each stand alone on their own pages with no
   forward references required to understand them.
3. **The engineer who will do the remediation.** The only reader of fix bodies,
   snippets, citations and the conventions section. Reads back to front. This reader is
   the current PDF's only well-served audience, and nothing in this redesign makes the
   document worse for them; the engineer-grade detail all survives, it just stops
   leading.

Zach himself is reader zero: he signs every finding. The fact-check procedure in
section 6 is what turns "one person read everything and signed" from an assertion into
a procedure the client took part in.

---

## 3. Page-by-page structure

Target length for a real engagement is about 25 pages, matching the research's
reference point (madewithlove's full report is about 25 pages and they call that
small). The sample fixture would produce fewer pages because every section gates on
empty, exactly as the template already does today; the page numbers below describe a
representative real engagement.

| Page | Section | What it is for | Primary reader |
|---|---|---|---|
| 1 | Cover | Target, commit pin per repo, date, prepared for, prepared by, brand, confidentiality line. No counts beyond the stat strip. | Everyone |
| 2 | Contents | Clickable outline, as today. | Everyone |
| 3 | Your questions, answered | The three intake questions, each answered yes or no (or honestly not assessed) with a one-sentence basis, a risk register pointer, and an hours figure. The single most important page in the document. | Founder, investor |
| 4 | Executive summary, continued | The engagement in numbers (candidates produced, false positives excluded, findings shipped, open versus accepted), plus the "three things this week" box with three distinct root causes (W24). | Founder |
| 5 | Remediation budget | The data-room page. Hours times the printed engagement rate, grouped by area, subtotals, one total at the bottom. | Investor, founder |
| 6-7 | Scope, limitations and files examined | What was read (repos, commit, file counts by language), what was excluded and why, which rules ran and which did not with the real reason, what a repository scan structurally cannot see. Built from the scan ledger. | Investor, engineer |
| 8 | How to read this report | Half a page: observations versus judgments (and that the client confirmed the observations), the severity scale, and the three client-facing groupings (risk register, code quality and debt, conventions in force). | All, once |
| 9 | Risk register summary | One row per open defect: id, headline, severity, area, hours, page pointer. Severity descending, nothing else interleaved. | Founder, investor |
| 10-16 | Risk register detail, by area | The current curated-findings rendering, relabelled, grouped under area openers (Access control, Secrets, Storage, Auth, Database, Build and deploy as applicable). Each area opens with its objective in one sentence and closes with a short method note naming the conventions observed in that area. Only Defect rows, only open or accepted, severity descending within area. | Engineer |
| 17-18 | Code quality and debt | Deviation rows rendered as the structured sentence ("2 of 47 assessed endpoints deviate from URI-versioned endpoints"), one block per rule with the deviating sites listed beneath; then no-convention splits as small option tables. No "Fix:" framing; the action column says "align or document the exception". | Engineer, investor skims |
| 19 | Engineering conventions in force | The adopted conventions as a compact table: area, the convention in the client's own codebase terms, and the basis (observed at N sites). This is the client's own rulebook, not a generic rubric, and it is the method note of the whole document. | Engineer, founder |
| 20 | Dependency health | The existing CVE table plus the new licence table: package, licence, classification (permissive, weak copyleft, strong copyleft, unknown), and a one-line statement when everything is permissive. | Investor, engineer |
| 21 | Verified clean | The current What's healthy section, computed strictly from the ledger's verified-clean predicate (ran, zero findings, not judged not-applicable; W23), with the existing honesty caveat. | Founder |
| 22 | Next steps | The existing retest ladder prose. | Founder |
| 23 | Methodology | The existing two-tier explanation, model names, version, reconciliation counts, severity scale note, not-performed list. | Engineer, diligence |
| 24 | Disclaimer and signature | As today, including the draft gating. | Legal, diligence |
| 25 | Appendix: citations and glossary | Rule id to citation index (absorbing the per-finding "Also violates" lines), plus a six-entry glossary (RLS, anon key, service role, copyleft, CVE, migration). | Engineer, diligence |

Ordering rationale, stated explicitly: the two pages a non-engineer acts on (questions,
budget) come before anything an engineer needs, because the research says investors
complain reports are "too long for the people who decide". Scope comes before the
register because a diligence reader prices the document's credibility before its
contents. Conventions close the findings sections because that is the investor-report
convention (each area closes with method), and because ending the findings on "here is
what your team already does well, written down" is the correct emotional shape for a
founder-commissioned document.

---

## 4. Layout sketches

### 4.1 Page 3: Your questions, answered

Intake captures up to three questions. When the client does not supply them, the
defaults come from the harvest fears: are the keys exposed, is row security right, can
anyone change anything, can the database be lost. Answers use a closed vocabulary:
YES / NO / NOT ASSESSED FROM CODE. "Not assessed" is an allowed, honest answer and
links to the scope page (see open question 3).

```
  Your questions, answered
  ------------------------------------------------------------------

  Before this audit began, you asked three questions. Here are the
  answers, each backed by a specific finding in this report.

  +----------------------------------------------------------------+
  | 1. Can member data be read by someone without an account?      |
  |                                                                |
  |    YES, TODAY.                                                 |
  |    The profiles table has no row security (R-01) and the       |
  |    member-documents bucket is public (R-04). Closing both is   |
  |    estimated at 4 to 8 hours.          Risk register, page 10. |
  +----------------------------------------------------------------+
  | 2. Are any secret keys exposed?                                |
  |                                                                |
  |    YES.                                                        |
  |    The service_role key is shipped to every browser and        |
  |    bypasses all row security (R-02). Estimated 2 to 4 hours.   |
  |                                        Risk register, page 10. |
  +----------------------------------------------------------------+
  | 3. Can the database be lost?                                   |
  |                                                                |
  |    NOT ASSESSED FROM CODE.                                     |
  |    Backup and point-in-time-recovery configuration lives in    |
  |    the Supabase dashboard, which a repository scan cannot      |
  |    see. Confirm it directly; the checklist is on page 7.       |
  |                                        Scope, page 6.          |
  +----------------------------------------------------------------+

  Everything else this audit found, and what fixing all of it
  costs, is on page 5.
```

Rendering notes: the YES or NO is the typographically heaviest element in each box, in
the severity colour family already used by `severity_chip` (red for a bad yes, green
for a good no, grey for not assessed). Each box cites finding ids, an hours range, and
a page. No box ever contains pipeline vocabulary or a rule id.

### 4.2 Page 5: Remediation budget

Grouped by area (the same areas as the risk register), one row per open finding,
subtotal per area, one total. Hours come from the per-finding estimate; cost is hours
times the printed engagement rate. Accepted findings appear below the total, priced but
excluded from it, so the total means "what you asked us to leave open costs nothing,
what remains costs this".

```
  Remediation budget
  ------------------------------------------------------------------

  Estimated at the engagement rate of $150/hour. Estimates assume an
  engineer already familiar with this codebase. Each estimate is for
  the fix described in the referenced finding, including verifying
  the fix as described there.

  | #    | What needs doing                     | Sev      | Hours    | Cost        |
  |------|--------------------------------------|----------|----------|-------------|
  | Access control                                                                  |
  | R-01 | Enable row security on profiles      | CRITICAL | 2 to 4   | $300-600    |
  | R-05 | Scope the messages read policy       | MEDIUM   | 1 to 2   | $150-300    |
  |      | Subtotal                             |          | 3 to 6   | $450-900    |
  | Secrets                                                                         |
  | R-02 | Rotate and remove service_role key   | CRITICAL | 2 to 4   | $300-600    |
  |      | Subtotal                             |          | 2 to 4   | $300-600    |
  | Storage                                                                         |
  | R-04 | Make member-documents private        | HIGH     | 2 to 4   | $300-600    |
  |      | Subtotal                             |          | 2 to 4   | $300-600    |
  | Auth                                                                            |
  | R-03 | Re-enable JWT verification           | HIGH     | 2 to 6   | $300-900    |
  |      | Subtotal                             |          | 2 to 6   | $300-900    |
  |------|--------------------------------------|----------|----------|-------------|
  |      | TOTAL, all open findings             |          | 9 to 20  | $1,350-3,000|

  Accepted by you and excluded from the total:
  | R-06 | Billing page session check           | HIGH     | 2 to 4   | accepted    |

  Code quality and debt items (pages 17-18) are not individually
  priced; aligning the 2 deviating endpoints is included in the
  engineering-conventions work described there.
```

Rendering notes: ranges, not points, matching the existing `hours_label` style. The
rate line is printed verbatim from the engagement options and must match the agreement
(the research is explicit: keep hours and the actual printed rate, never a generic
engineer-week price). When no rate is configured, the Cost column is omitted entirely
and the table prints hours only; it never prints a made-up rate.

### 4.3 Pages 6-7: Scope, limitations and files examined

Built from data the system already records: the cover's file counts, `code_volume` by
language, the methodology `not_done` list, and the scan ledger's per-rule facts
(`RuleLedgerEntry`: ran, skip reason, files evaluated, coverage notes, not-applicable
judgments). The export JSON already carries `rules_not_run`; the template simply never
renders it today.

```
  Scope and limitations
  ------------------------------------------------------------------

  What was examined
  | Repository              | Commit   | Branch | State            |
  |-------------------------|----------|--------|------------------|
  | harbor-portal           | a1b2c3d  | main   | clean            |
  | harbor-edge-functions   | 9f8e7d6  | main   | uncommitted      |
  |                         |          |        | changes present  |

  214 files were read in full: 31,842 lines of code
  (TypeScript: 24,118; SQL: 6,210; TOML/config: 1,514).
  1,188 files were excluded as noise: generated code, lockfiles
  (read separately by the dependency pass), vendored dependencies,
  binary assets.

  What was checked
  22 rules ran to completion across 7 areas. 14 produced no
  findings (page 21). 8 produced the findings in this report.

  What was not checked, and why
  | Rule or area                  | Reason                          |
  |-------------------------------|---------------------------------|
  | ARCH-STRICT-LAYERING-1        | Requires an architecture config |
  |                               | this repository does not define |
  | API versioning conventions    | No matching files in scan scope |
  | <tool-backed rule>            | External tool unavailable       |

  What a repository scan structurally cannot see
  - The live database. Row security findings reflect migration
    history; dashboard changes that never landed in a migration are
    invisible here and must be confirmed against production.
  - Backup and recovery configuration (dashboard-managed).
  - Runtime behaviour: no penetration testing, no live
    exploitation, no dynamic analysis was performed.
  - Organisational controls, identity and access management,
    infrastructure configuration.

  Facts in this report were confirmed by the client
  Every observation (file, line, contents) in the risk register was
  sent to Harbor Community Foundation for confirmation on
  2026-09-19 and confirmed on 2026-09-21, before this report was
  signed. Judgments and estimates are the auditor's own.
```

Rendering notes: the not-checked table's reason column is the ledger's `skip_reason`
and `coverage_note` rewritten through the W25 hygiene layer (client-facing words, never
"pass_failed" or retry narration). The deposition-grade point from the research is that
cheap reports lack this page; it must read as proud disclosure, not small print.

### 4.4 Risk register entry (the current finding block, relabelled)

The existing `render_site` block survives with three changes: a stable R-nn id, an
explicit Observation and Assessment split, and no effort, confidence or disposition
chips.

```
  R-02  The service_role key is shipped to every browser
  CRITICAL  ·  Secrets  ·  harbor-portal

  Observation (confirmed by client 2026-09-21)
    apps/web/.env.production:6
    SUPABASE_SERVICE_ROLE_KEY=eyJhbGciOi...[redacted]

  Assessment (auditor judgment)
    A service-role key shipped to the browser bypasses Row Level
    Security and grants full read and write access to every table
    to anyone who opens developer tools.

  Fix
    Rotate the key, move it to server-only configuration, confirm
    the old key is rejected.                 Estimated 2 to 4 hours.

  Basis: SUPABASE-KEY-SERVICE-ROLE-CLIENT-1 (citations: page 25)
```

### 4.5 Conventions in force (page 19)

```
  Engineering conventions in force
  ------------------------------------------------------------------

  These are the conventions this codebase was observed to follow
  consistently. They are stated in this codebase's own terms, not a
  generic rubric, and they are the standard the consistency items
  on pages 17-18 are measured against.

  | Area     | Convention                                | Basis            |
  |----------|-------------------------------------------|------------------|
  | API      | Endpoints are URI-versioned (/v1/...)     | 45 of 47 sites   |
  | Database | Every table change ships as a migration   | all 31 sites     |
  | Auth     | Server routes verify the session JWT      | 12 of 12 sites   |
```

---

## 5. The five kinds, mapped to client-facing presentation

The engine's taxonomy (`FindingKind` in `crates/api-types/src/audit.rs`) is the
internal truth; the client sees three groupings plus the scope page, in Zach's
investor-language labels. The Pass 2 curator design (section 3) already worked out what
each kind IS and what decision it needs; this table is the client-facing half of the
same model.

| Engine kind | Client-facing home | Label on the page | Rendering commitment |
|---|---|---|---|
| `Defect`, deterministic floor | Risk register (pages 9-16) | A risk, with an **Observation** block (file, line, contents; client-confirmed) and an **Assessment** block | Full block per section 4.4. Floor provenance means the Observation is machine-proven; the fact-check confirms it. |
| `Defect`, advisory tier | Risk register | Same block; the Assessment is explicitly "auditor judgment" | Identical layout. The tier distinction is carried by the Observation/Assessment labelling plus methodology, never by a "confidence: needs-review" chip; a needs-review row that survives Zach's sign-off has by definition been reviewed. |
| `Deviation` | Code quality and debt (pages 17-18) | A consistency item | The structured sentence first, always: "2 of 47 assessed endpoints deviate from URI-versioned endpoints", from `deviating_sites` / `assessed_sites` / `convention_label`. Numerator alone when the denominator is honestly absent, never a fabricated denominator. Deviating sites listed beneath as locations. Action phrasing is "align or document the exception", never a defect "Fix:" line. |
| `ConventionObserved` | Conventions in force (page 19) and each area's one-line method closer | A house convention | Compact table row, per section 4.5. Never a finding block, never a severity, never a fix, never an estimate (W22 makes that structural). |
| `NoConvention` | Code quality and debt | "No settled convention" | A small table of `observed_options` with per-option site counts when counted; the recommendation (when the curator made one) printed as a recommendation, otherwise the split reported as-is. |
| `NotAssessed` | Scope page (pages 6-7) | The not-checked table | Grouped by `NotAssessedReason`, reasons rewritten to client language. Never in any findings section, never counted in any total, never "Clean". |
| `unspecified` | Nowhere | n/a | An export gate fails the PDF build if any row reaches the renderer without a kind. The curator UI already treats unspecified as an alarm; the client artifact inherits that stance by refusing to render at all. |

The three-kinds mapping from the research, restated against this table: violations
(Defect) become the risk register; consistency issues (Deviation, NoConvention) become
code quality and debt; conventions adopted (ConventionObserved) become the method note
that closes each area and the page-19 table, stating the client's own conventions
rather than a generic rubric.

---

## 6. Observations versus judgments: the fact-check procedure in the document

The procedure (from the research, and it maps exactly onto the deterministic floor
versus the advisory tier):

1. After Zach's curation pass and before the signed report, export a **facts list**:
   one row per Observation in the would-be risk register, containing repository,
   repo-relative path, line, and the verbatim contents found there. Nothing else: no
   severity, no assessment, no fix, no hours. The existing xlsx exporter is the natural
   carrier (open question 6 covers the exact artifact).
2. The client confirms each fact or disputes it. A disputed fact goes back through
   curation; it either dies as a false positive or returns corrected.
3. The signed report then prints, on the scope page and once in the methodology, the
   confirmation sentence: facts sent on date X, confirmed on date Y. Judgments and
   estimates are labelled as the auditor's own and were never up for confirmation.

What this buys, stated plainly: "a concern nobody can dismiss, sitting on a fact
everybody has already agreed to." For a solo practitioner with no firm name behind him,
this is the strongest trust move available, and it costs one export artifact and one
line in the engagement agreement.

Labelling rule in the PDF: every risk register entry splits visually into Observation
(the fact block) and Assessment (the judgment paragraph), per sketch 4.4. Deviation
sentences are observations by construction (they are counted structurally, never
model-counted, per `DeviationKind`'s doc) and are included in the facts list as the
site lists under each sentence.

---

## 7. What is cut from the current PDF

Removal is part of the redesign. Each cut, with the reason:

1. **The severity x effort matrix page (current page 5).** Superseded outright by the
   remediation budget table, which carries the same prioritisation information with
   money attached, which is the form both the founder and the investor actually use.
   The matrix was already a repair of an earlier duplicate; the budget table is the
   final form of this idea. The underlying effort data stops rendering as a grid and
   starts rendering as the Hours column.
2. **The standalone category scorecard page (current page 4), including the
   heat-grid.** A grid of coloured counts is a score wearing a costume, and the sweep
   caught it badged Clean over rows the same export contradicts. The per-area status
   survives as one chip in each area opener in the risk register, computed from the
   ledger's `verified_clean` predicate (W23), which is the fail-loud ledger the
   research says a traffic light must sit on. The counts themselves are already on the
   cover stat strip and page 4's numbers; a second rendering of them earns no page.
3. **The held-for-review section, from any signed deliverable.** Zach reviews every
   finding; a signed report therefore contains zero held rows by procedure, and a
   section whose presence contradicts the signature cannot ship. Draft exports keep the
   section (it is useful to Zach mid-review), gated on the existing `review_state`
   signal exactly as the draft banner already is. This also closes the sweep's "held
   and convention rows inside the curated section" and "held sections opening with
   uncited rows" defects for the client artifact.
4. **Effort and confidence mini-chips, and the placeholders "not estimated this run"
   and "not evaluated this run".** Pipeline vocabulary in a client-facing field.
   Effort becomes the budget table's Hours column; confidence collapses into the
   Observation/Assessment labelling; a run-state placeholder has no business in front
   of a client at all (W26 makes floor rows always estimated, so the placeholder's
   legitimate case disappears).
5. **Disposition strings such as "Open (recommended: Do next)".** Curator vocabulary.
   The client sees priority words only: Fix now, Fix next, Planned, Accepted by you,
   rendered as the register's ordering and the budget's grouping rather than as a
   per-row status string.
6. **"Also violates:" lines on findings.** Registry traceability, not client
   information. Moves to the page-25 citations appendix keyed by R-nn id.
7. **Convention rows rendered as findings with "Fix:" lines.** The sweep's most
   unanimous objection (all five graders). Already restated structurally in section 5:
   conventions get a table, never a finding block.
8. **The auditor's absolute filesystem paths, retry narration, CI-state text and
   bracketed calibration notes.** Listed for completeness; these are W21/W25
   prerequisites, not template design, but the redesign is not shippable before they
   land.

What is deliberately NOT added, restating Zach's rulings so this document cannot drift:
no overall or per-area numeric score or letter grade (the existing
`severity_scale_note` already says why, and it stays); no go or no-go verdict (the
founder is the client; a verdict on the company is the investor's job); no team or
bus-factor analysis from git history (outside the engagement and hard to defend).

---

## 8. Cheap versus structural in the existing template, and build sequence

The template (`audit_report.typ`) is a single-file renderer over one `data.json`
contract built by `report_export::build_report_json`. "Cheap" below means template-only
against the existing JSON; "export" means a change to `report_export.rs`'s JSON
contract with no engine change; "engine" means new upstream capability.

**Cheap (template-only, existing JSON already carries the data):**

- Section renames, reordering and the page-level restructure itself. Typst sections
  are independent blocks; moving them is mechanical.
- Rendering `rules_not_run` on the scope page. The field exists in the JSON contract
  today (W1 work, `ExcludedRuleJson`) and the template simply never renders it; this is
  the cheapest high-value line in the whole redesign.
- Cutting the matrix page, the scorecard page, the chips, the disposition strings, the
  "Also violates" inline lines.
- Gating held-for-review on `review_state` (the draft banner already reads the same
  signal; this is one more `is_draft` conditional).
- The scope page's repository, commit, file-count and not-performed content (cover and
  methodology fields, re-homed).
- The R-nn ids (derivable at render time from register order, though export-side
  assignment is better for stability across re-exports; see sequencing).

**Export (report_export.rs changes, no engine change):**

- `FindingKind` onto the curated-site wire. `CuratedSiteJson` today carries no kind;
  deviation numerators and denominators, convention tables and the no-convention splits
  all need the kind payloads flattened into the JSON. This is the structural spine of
  the redesign and is already W22/W27 territory.
- The remediation budget: per-finding hour ranges (today only the top-3 carry
  `hours_label`; every open row needs one, which W26's "floor rows always estimated"
  requires anyway), an engagement rate field on `ReportOptions` threaded to the
  template, area grouping, subtotals and the total computed server-side so the template
  never does arithmetic.
- The intake questions block: three question strings plus per-question answer
  (yes / no / not-assessed), answering finding ids and hours, on `ReportOptions` or a
  sibling intake structure. Defaults synthesisable server-side from the harvest-fear
  rule families when no questions were supplied.
- The conventions-in-force section: the curator's gated convention commit already
  exists (Pass 2 build log); its adopted set needs routing into the report JSON.
- The fact-check fields: sent-on and confirmed-on dates on `ReportOptions`, plus the
  facts-list export mode (a filtered view over data the exporter already has).
- Export gates: fail the build on any `unspecified` kind, any absolute path, any
  pipeline-vocabulary token reaching a client-facing field (shares the W21/W25 gate).

**Engine (new capability):**

- The licence check: a deterministic pass over lockfiles (package-lock.json,
  Cargo.lock, and siblings as stacks demand), mapping each dependency to a licence and
  classifying permissive versus weak copyleft versus strong copyleft versus unknown. A
  candidate new rule class per the research, and the one item the shelved pass-3 note
  explicitly exempts from the shelf ("the licence-check class may enter the corpus
  roadmap whenever the implementer is between work orders, since it is data, not
  presentation"). The PDF section renders whatever the pass produces and renders a
  clearly-worded "licence review not performed this engagement" line until it exists,
  on the scope page's not-performed list, so the report never implies coverage it does
  not have.
- File-level exclusion reasons for the scope page, if the current exclusion accounting
  turns out to record only counts and not reason classes. If reasons are not currently
  recorded, the scope page ships with the honest aggregate sentence (as sketched) and
  the per-class breakdown follows.

**Sequence (each phase independently shippable):**

1. **Phase A, template-only:** the cuts, the renames, the reorder, `rules_not_run`
   rendering, held gating, the How-to-read page with static Observation/Assessment
   framing. Ships a visibly better PDF against today's data with zero server change.
   Must still wait on W21/W25 before any client sees it.
2. **Phase B, export contract:** kinds on the wire, the budget table, per-finding
   hours, the rate field, the conventions section, R-nn ids assigned at export, the
   export gates. This phase delivers items 2, 4 and 5 of Zach's six.
3. **Phase C, new surface:** the intake-questions block and its UI capture, the
   facts-list artifact and the confirmation dates, the licence pass and its section.
   Delivers items 1, 3 and 6.

The order inside Zach's value ranking survives this sequencing: the questions-first
summary (his number 1) lands in C only because it needs intake plumbing, but its
template shell (the page, the box layout, default questions synthesised server-side)
can land in B so that C is wiring, not design.

---

## 9. Dependencies on the remediation workstreams, stated once

- **W21 (repo-relative paths) and W25 (text hygiene) are shipping blockers** for the
  redesigned PDF reaching any client. A beautiful page printing the auditor's home
  directory 64 times is a worse deliverable than today's.
- **W22 (kind integrity)** is the precondition for section 5's mapping; a convention
  row carrying a fix must be impossible upstream, not merely unrendered.
- **W23 (honesty surfaces)** feeds the area status chips and the verified-clean page.
- **W24 (merge and ranking)** feeds page 4's three-distinct-root-causes box and the
  register's ordering (INFO and LOW must never outrank CRITICAL again).
- **W26 (fix and estimate quality)** feeds the budget table (always-estimated floor
  rows; no high-or-above row without a fix and citation).
- **W27** is this document; the W27 entry in the remediation plan should point here.

---

## 10. Open questions for Zach

Each of these genuinely needs his judgment; everything not listed here is committed
above.

1. **The printed rate and its source of truth.** The budget table prints an engagement
   rate and the research insists the number must match the agreement. Is the rate a
   per-engagement `ReportOptions` field Zach types at export time (recommended, lowest
   machinery), or does it live somewhere more durable per client? And when no rate is
   supplied, the table prints hours only; confirm that is acceptable in a data room or
   whether a rate should be mandatory for a signed export.
2. **Hard gate on held rows at sign-off.** Section 7 cuts held-for-review from signed
   deliverables. Should the export refuse to produce a signed (non-draft) PDF while any
   held row exists, forcing the disposition, or silently omit held rows from the signed
   artifact? Refusal is the honest mechanism and the recommendation, but it changes his
   sign-off workflow, so it is his call.
3. **"Not assessed" as a printed answer to a client's named question.** When a client's
   intake question cannot be answered from a repository scan (the backup question,
   typically), does the honest NOT ASSESSED FROM CODE answer ship as sketched, or does
   that question trigger manual out-of-engine work so the signed report always answers
   what was asked? The first is honest and cheap; the second is the better product and
   more billable hours; this is a positioning decision, not a technical one.
4. **Conventions section scope.** Page 19 as designed prints only the adopted
   conventions (curator-gated, per the Pass 2 opt-out default). Alternative: print
   every observed convention with adopted ones marked. Adopted-only is the
   recommendation (the client's rulebook should contain only what Zach endorses over
   his signature), but he may want the fuller picture in front of the client.
5. **Licence-pass timing against the outreach-first freeze.** The shelf note exempts
   the licence class as data work for an implementer between work orders, and the
   freeze scopes in-scope work tightly. Confirm the licence pass may be scheduled in
   Phase C, or whether it waits until after outreach regardless, with the scope page
   carrying the not-performed line in the interim.
6. **The fact-check artifact's form.** The facts list can ship as a filtered xlsx (the
   exporter exists), a short standalone PDF, or both. The xlsx is the recommendation
   (clients can annotate rows in place), but the artifact a founder forwards to their
   engineer is a client-experience decision. Also his call: the exact confirmation
   wording printed in the signed report, since it is a factual claim about what the
   client did.
7. **The cover's confidentiality line.** Investor-side covers carry one ("Prepared for
   X; confidential; not for distribution without consent"). The wording has light legal
   weight and should match the engagement agreement, so the sentence is his to write;
   the template just needs the final string.

---

## 11. Zach's answers (2026-10-16)

**Q1 Rate: operator-entered at export; hours-only when absent.** "Allow me to enter the rate,
otherwise just print the hours."

**Q2 Held rows: REFUSE the signed export.** "Force me to triage, that's the whole point of the
agency." A signed PDF cannot be produced while any held row exists.

**Q3 "Not assessed from code" ships as the printed answer.** "If the client wants me to review
their infrastructure... I would gladly review separately. Since it isn't something that can be
easily scanned it really isn't part of the Camerata scope." So the report answers honestly and
the follow-on is a separate engagement, not silent manual work folded into this one.

**Q5 Licence check: PARKED.** Zach did not recognise it as a requirement, though it was item 6
of his own investor-side research. Treat as lower priority than everything else here; the scope
page prints the not-performed line in the interim.

**Q7 Cover line: drafted, awaiting his word.** Proposed: "Prepared for [Client] by Cantus Works.
Confidential. Not for distribution outside [Client] and its professional advisors without
written consent." The advisors clause is load-bearing: it is what lets a founder hand the report
to an investor or lawyer without breaching it, which is the reason the page exists at all.

### [x] THE GOVERNING PRINCIPLE, and it outranks several choices in this document

> "The PDF is a written document, it is NOT for listing out every single row item, like every
> finding or every rule, etc. THAT belongs in the Excel. The PDF is more informational."

**This applies to the whole redesign, not only the conventions page.** The 25-page structure in
section 3 must be re-checked against it before any build: any page that is a table of rows is
probably an Excel sheet with a paragraph in the PDF pointing at it. The division is
PROSE AND JUDGMENT in the PDF, ROWS AND EVIDENCE in the workbook.

**Q4 Conventions page, resolved by that principle: a paragraph, not a list.** Do not print the
conventions. Print the count, name the two or three that matter, and point at the workbook.
Sketch:

> Your codebase follows 12 conventions consistently across API design, data access and error
> handling. Three are worth naming: every endpoint carries a version segment; every repository
> returns domain types rather than database rows; every error response carries a stable
> machine-readable code. The complete list, with the sites evidencing each one, is in the
> workbook.

Note Zach floated a third framing, "what the inspection ran against", which is the RULES APPLIED
rather than the conventions found. That is the scope page's job (section 3, pages 6-7), not this
page's, and keeping them separate avoids one page answering two questions.

**Q6 Fact-check artifact: xlsx only, one sentence in the PDF.** Same principle. The facts list is
inherently row-level (file, line, what is there), so it is a workbook sheet. The PDF carries the
claim and nothing else: "The 47 observations in this report were confirmed as factually accurate
by [Client] on [date]." The trust move survives in a form a reader absorbs; the evidence is one
click away.

### Consequence for the build
Section 3's page plan is now provisional. Before W27 starts, every page must be classified as
PROSE (stays in the PDF) or ROWS (moves to the workbook with a PDF paragraph pointing at it).
Expect the page count to fall.
