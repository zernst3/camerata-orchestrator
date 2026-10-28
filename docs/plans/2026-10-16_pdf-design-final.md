# PDF design, final and reconciled - the client-facing audit deliverable (2026-10-16)

**Status: DESIGN, FINAL. Supersedes section 3 (the page plan) of
`2026-10-16_pdf-redesign-report.md`; everything else in that document survives except
where this one explicitly overrides it. No code, no template, no rule changes in this
pass.**

Inputs: the first design report including Zach's answers in its section 11
(`docs/plans/2026-10-16_pdf-redesign-report.md`), Zach's investor-side report research
(`Cantus Works/Marketing/Drafts/Investor-side report conventions.md`), the five-repo
sweep findings (`docs/plans/2026-10-16_comprehensive-remediation-design.md` section 2C),
the shipping template (`crates/server/templates/audit_report.typ`), the current sample
PDF (`sample-report/camerata-sample-audit.pdf`), the export contract
(`crates/server/src/report_export.rs`), the workbook exporter
(`crates/server/src/xlsx_export.rs`), and the kind wire
(`crates/api-types/src/audit.rs`).

---

## 1. The governing principle, and the test every page must pass

Zach's directive, verbatim:

> "The PDF is a written document, it is NOT for listing out every single row item, like
> every finding or every rule, etc. THAT belongs in the Excel. The PDF is more
> informational."

Prose and judgment in the PDF. Rows and evidence in the workbook.

The test this document applies to every page of the first design: **is this table the
point, or is it evidence for the point?** A table whose shape IS the conclusion (a
total with its breakdown) may survive as a table. A table that enumerates instances so
a reader could verify or act on each one is evidence, and evidence lives in the
workbook, with a paragraph in the PDF that states the conclusion and points at the
sheet.

This is executable today because the workbook already carries everything the principle
sends to it. The findings sheets (`xlsx_export.rs`, `HEADERS`, 26 columns) carry
Severity, Headline, Repo, File, Line, Rule ID, Citation, Snippet, Detail, both fix
columns (the finding-specific fix and the rule's authored remediation), Kind, Sites and
absorbed locations. Separate sheets already exist for Deviations, Conventions,
Dependencies, Coverage (which already receives `rules_not_run`) and False Positives,
plus an Index. Nothing the PDF gives up falls on the floor; the only workbook addition
this design needs is an R-id column (section 7, Phase B).

Consequence for the reader model: the first design's reader 3, the engineer who will do
the remediation, **moves from the back of the PDF to the workbook**. The PDF stops
being the engineer's artifact and becomes the founder's and the investor's. This is the
single largest consequence of the principle and it is embraced, not resisted: the
workbook already serves the engineer better than any PDF page could (sortable,
filterable, annotatable, complete), and the research's complaint about diligence
reports is that they are "too long for the people who decide", never that they are too
short for engineers.

---

## 2. The reconciled page plan

Every page of the first design's 25-page plan, classified PROSE or ROWS, with its
disposition. "Representative engagement" means the same real-engagement scale the first
design priced at 25 pages.

| First-design page | Section | Class | Disposition |
|---|---|---|---|
| 1 | Cover | PROSE | Survives. Identification metadata, not rows. Carries the liability line (section 6). |
| 2 | Contents | PROSE | Survives. Navigation, not rows. |
| 3 | Your questions, answered | PROSE | Survives unchanged. The most important page in the document. Sketch 4.1 of the first design stands, including NOT ASSESSED FROM CODE as a printed answer (settled). |
| 4 | Executive summary in numbers + three things | PROSE | Survives, with the engagement-in-numbers sentence reworded into client language (section 5.1); "candidates" and "dispositioned" are curation vocabulary and never print. |
| 5 | Remediation budget | ROWS, but the table survives COLLAPSED | The one-row-per-finding table was rows. It collapses to one row per area plus the total; per-finding hours and cost move to the workbook. Full resolution of the data-room tension in section 3. |
| 6-7 | Scope, limitations and files examined | PROSE with two embedded row tables | The prose survives and the section grows (it absorbs three other pages, below). The "what was not checked, and why" table is ROWS and collapses to a paragraph; the repository identification table (one row per repo, two or three rows) survives because it is identification, not enumeration. |
| 8 | How to read this report | PROSE | Survives, folded into the scope section as its closing half-page rather than standing alone. |
| 9 | Risk register summary table | ROWS | CUT from the PDF. One row per open defect is the definition of a row listing. The executive summary's counts, the budget's area rows and the narrative's R-ids together do this page's job; the complete sortable list is the workbook's All Findings sheet. |
| 10-16 | Risk register detail blocks | ROWS | Reshaped into a per-area RISK NARRATIVE, about 3 to 4 pages instead of 7. Full treatment in section 4. The verbatim snippet, exact line, citation and full fix text move to the workbook; the finding survives as named prose. |
| 17-18 | Code quality and debt | PROSE wearing rows | The structured deviation sentence ("2 of 47 assessed endpoints deviate from URI-versioned endpoints") is already prose and survives as the lead of a short paragraph per deviation. The per-site lists beneath each sentence are ROWS and move to the workbook's Deviations sheet. The no-convention option tables collapse to a sentence each. One page, not two. |
| 19 | Engineering conventions in force | ROWS | Resolved by Zach already (his Q4 answer): a paragraph, not a list. Count, name two or three, point at the workbook's Conventions sheet. Folds onto the code-quality page. |
| 20 | Dependency health | ROWS | Becomes PROSE, redesigned from scratch in section 5.4 because the licence check is now unparked. CVE rows and per-package licence rows live in the workbook's Dependencies sheet. |
| 21 | Verified clean | ROWS | CUT as a standalone page. A list of rules that found nothing is a row listing. Collapses to one paragraph inside the scope section's "what was checked" prose: the count, two or three named areas, the Coverage sheet pointer. The ledger predicate (W23) still computes it; only the rendering changes. |
| 22 | Next steps | PROSE | Survives unchanged. |
| 23 | Methodology | PROSE | Survives; absorbs the glossary. |
| 24 | Disclaimer and signature | PROSE | Survives unchanged, including draft gating. |
| 25 | Citations appendix + glossary | ROWS + PROSE | CUT as a page. The rule-id-to-citation index is rows; the workbook findings sheets already carry Citation and Citation URLs per row, keyed by the new R-id column. The six-entry glossary is prose and folds into the methodology page. |

### The final page order

| Page | Section | Class |
|---|---|---|
| 1 | Cover, with the liability line | PROSE |
| 2 | Contents | PROSE |
| 3 | Your questions, answered | PROSE |
| 4 | Executive summary: the engagement in plain numbers, and the three things this week | PROSE |
| 5 | Remediation budget: area rows and one total | THE SURVIVING TABLE |
| 6-7 | Scope, coverage and confirmation (absorbs rules-not-run, verified-clean, how-to-read, the fact-check sentence) | PROSE |
| 8-11 | The risks, by area: a narrative, not a register | PROSE |
| 12 | Code quality, debt and conventions in force | PROSE |
| 13 | Dependency health: vulnerabilities and licences | PROSE |
| 14 | Next steps | PROSE |
| 15 | Methodology and glossary | PROSE |
| 16 | Disclaimer and signature | PROSE |

### The count

**About 16 pages for the representative engagement the first design priced at 25: a
delta of roughly minus 9 pages, about a third of the document.** The elastic section is
the risk narrative (pages 8-11): a heavy engagement with many CRITICAL and HIGH
findings grows it a page or two; a light one shrinks it. The sample fixture, which
gates empty sections off exactly as the template does today, would render at roughly 10
to 11 pages against today's 13, while saying more that a founder can use.

The research's "about 25 pages" is explicitly let go. It described madewithlove's
report, which predates the governing principle and carries its own row listings. The
principle wins, and 16 pages of prose a founder finishes beats 25 pages a founder
skims.

---

## 3. The budget table: the tension, resolved

The research calls the remediation budget "the page a founder puts in a data room", and
a budget table IS rows. The two must be reconciled, not averaged.

**Resolution: the table survives, collapsed from one row per finding to one row per
area, plus the total.** Justification, in full:

1. Apply the section-1 test. The point of the budget page is "fixing everything open
   costs 9 to 20 hours, 1,350 to 3,000 dollars, and most of that sits in access
   control". That conclusion IS a total with a breakdown; a four-to-eight-row area
   table is the clearest possible prose for it, in the same way a sentence with numbers
   in it is still a sentence. The per-finding rows, by contrast, are the derivation of
   that conclusion: evidence, workbook.
2. The data-room reader is served better, not worse. An investor pricing a deal
   adjustment wants the total and where the cost concentrates; they were never going to
   act on row 7 of 12. The two largest individual items still get named, in the prose
   line under the table, with page pointers into the risk narrative.
3. The founder forwarding the workbook loses nothing: every open finding's hours and
   cost print as columns on the All Findings sheet (Phase B adds the cost column
   alongside the existing Est. hours column).

Settled answers honoured on this page: the rate is operator-entered at export, and when
no rate was entered the Cost column is omitted entirely and the table prints hours
only, never a fabricated or generic rate. Accepted findings are excluded from the total
and acknowledged in one prose sentence, not a second table.

Sketch:

```
  Remediation budget
  ------------------------------------------------------------------

  Estimated at the engagement rate of $150/hour. Estimates assume an
  engineer already familiar with this codebase, and cover the fix
  and its verification as described in the pages that follow.

  | Area            | Open items | Hours   | Cost          |
  |-----------------|------------|---------|---------------|
  | Access control  | 2          | 3 to 6  | $450-900      |
  | Secrets         | 1          | 2 to 4  | $300-600      |
  | Storage         | 1          | 2 to 4  | $300-600      |
  | Auth            | 1          | 2 to 6  | $300-900      |
  |-----------------|------------|---------|---------------|
  | TOTAL           | 5          | 9 to 20 | $1,350-3,000  |

  The two largest single items are the missing row security on the
  profiles table (R-01, page 8) and the service-role key shipped to
  the browser (R-02, page 9). One further finding, the billing-page
  session check (R-06), was accepted by you as a known risk and is
  excluded from the total. Hours and cost against every individual
  finding are in the workbook's All Findings sheet.
```

When a licence remediation carries hours (section 5.4), it appears here as a
Dependencies area row, same as any other area.

---

## 4. The risk narrative: what replaced the register

This is the hardest reconciliation in the document, so it is specified precisely.

The first design gave every open defect a formatted block: headline, severity strip,
Observation block with path, line and verbatim snippet, Assessment paragraph, Fix
block, citation line. Seven pages. Under the governing principle that is a row listing
with generous typography, and the parts that made it row-like (the snippet, the exact
line, the full fix text, the citation) are exactly the parts only the engineer reads,
and the engineer now reads the workbook.

**What renders instead, per area (Access control, Secrets, Storage, Auth, Database,
Build and deploy, as applicable):**

1. **An area opener**: the area's objective in one sentence, exactly as the first
   design had it.
2. **One named paragraph per CRITICAL and per HIGH open finding.** Each paragraph
   carries, in order: the R-id and headline as a run-in heading with the severity word;
   one sentence of observation in client language, with the confirmation status
   woven in ("Observed, and confirmed by you on [date]: the migration that creates
   the profiles table never enables row security, so every row is readable and
   writable with the public anon key, with no login required"); one or two sentences
   of assessment, explicitly the auditor's judgment; one sentence of fix direction,
   not the full fix text; the hours range; and the workbook pointer by R-id. No
   snippet, no path-and-line citation, no rule id, no "Fix:" label.
3. **One sentence per MEDIUM open finding**: R-id, headline, hours, nothing else. The
   workbook carries the rest.
4. **One aggregate sentence for LOW and INFO**: "Three further low-severity
   observations in this area are in the workbook." Counts only, never names.
5. **An area closer**: the one-line method note naming what the area was assessed
   against and on how many sites, which is where ConventionObserved earns its
   per-area mention.

Ordering within the area is severity descending; INFO and LOW can never appear above
CRITICAL because they never appear as paragraphs at all (W24 still fixes the ranking
upstream; this is presentation defence in depth). Held rows cannot appear because a
signed export refuses to build while any exist (settled, Q2: "force me to triage,
that's the whole point of the agency"). Convention rows cannot appear because kind is
now on the wire and the narrative renders Defect rows only.

The Observation and Assessment split, which is the trust mechanism from the research,
survives as sentence structure instead of block structure: "Observed, and confirmed by
you on [date]: ..." followed by "Our assessment: ...". The mechanism loses nothing; it
was never the grey box that carried the trust, it was the labelled separation of fact
from judgment, and a sentence labels as well as a box.

Sketch of one area:

```
  Access control
  ------------------------------------------------------------------
  Objective: only the intended user can read or change each record.

  R-01  The profiles table has no row security.          CRITICAL
  Observed, and confirmed by you on 2026-09-21: the migration that
  creates the profiles table never enables row-level security, so
  every row is readable and writable by anyone holding the public
  anon key, with no login required. Our assessment: this is the
  most consequential issue in this report; member personal data is
  effectively public today. Fix direction: enable row security
  with owner-scoped policies, then verify an anonymous read fails.
  Estimated 2 to 4 hours. Workbook: R-01.

  R-05  The messages read policy is broader than intended. MEDIUM
  Any signed-in user can read any conversation. Estimated 1 to 2
  hours. Workbook: R-05.

  Two further low-severity observations in this area are in the
  workbook.

  Method: assessed against this codebase's own conventions, on 14
  tables across 31 migration files.
```

---

## 5. Layout sketches for the surviving prose pages

### 5.1 Page 4: Executive summary

The questions page (page 3) is unchanged from the first design's sketch 4.1 and is not
re-sketched. Page 4, with the engagement numbers reworded out of curation vocabulary:

```
  Executive summary
  ------------------------------------------------------------------

  We read this codebase in full: 214 files, 31,842 lines, at the
  commits named on the cover. We reviewed 10 candidate observations
  and excluded 3 that did not hold up under review; the 7 findings
  in this report all survived that review, and every factual
  observation behind them was confirmed by you before signature.
  5 findings are open, 1 is planned by your team, and 1 you have
  accepted as a known risk. Fixing everything open is estimated at
  9 to 20 hours; the budget on the next page breaks that down.

  If you only do three things this week
  +----------------------------------------------------------------+
  | 1  Enable row security on the profiles table        CRITICAL   |
  |    Member data is publicly readable and writable today.        |
  |    2 to 4 hours. Page 8.                                       |
  | 2  Rotate and remove the service-role key           CRITICAL   |
  |    The key in the browser bundle bypasses all row security.    |
  |    2 to 4 hours. Page 9.                                       |
  | 3  Make the member-documents bucket private         HIGH       |
  |    Every uploaded document is downloadable by URL.             |
  |    2 to 4 hours. Page 9.                                       |
  +----------------------------------------------------------------+
  Three distinct root causes (W24). Roughly 6 to 12 hours total.
```

### 5.2 Pages 6-7: Scope, coverage and confirmation

This section absorbed four pages of the first design (scope, rules-not-run, verified
clean, how-to-read) and is now the document's honesty core. Only the repository
identification table remains tabular; everything enumerable is a count plus a Coverage
sheet pointer.

```
  Scope, coverage and confirmation
  ------------------------------------------------------------------

  What was examined
  | Repository            | Commit  | Branch | State                |
  |-----------------------|---------|--------|----------------------|
  | harbor-portal         | a1b2c3d | main   | clean                |
  | harbor-edge-functions | 9f8e7d6 | main   | uncommitted changes  |

  214 files were read in full: 31,842 lines (TypeScript 24,118;
  SQL 6,210; config 1,514). 1,188 files were excluded as noise:
  generated code, lockfiles (read separately by the dependency
  pass), vendored dependencies, binary assets.

  What was checked, and what came back clean
  22 rules ran to completion across 7 areas. 14 produced no
  findings; among them, migration discipline, SQL injection
  surfaces and session handling came back clean. 8 produced the
  findings in this report. The rule-by-rule account, including
  every rule that found nothing, is in the workbook's Coverage
  sheet.

  What was not checked, and why
  Three rules could not run this engagement: two require
  configuration this repository does not define, and one depends on
  an external tool that was unavailable during the scan. Each
  skipped rule and its specific reason is in the Coverage sheet.
  [Until the licence pass ships: "A dependency licence review was
  not performed this engagement."]

  What a repository scan structurally cannot see
  - The live database. Row security findings reflect migration
    history; dashboard changes that never landed in a migration
    must be confirmed against production.
  - Backup and point-in-time-recovery configuration
    (dashboard-managed).
  - Runtime behaviour: no penetration testing, no live
    exploitation, no dynamic analysis.
  - Organisational controls, identity management, infrastructure.
  Reviewing infrastructure directly is available as a separate
  engagement.

  How to read this report
  Every finding separates what we observed (a fact: a file, a line,
  what is there) from what we concluded (our judgment). Severities
  run CRITICAL, HIGH, MEDIUM, LOW: words, not scores, because a
  number invites ranking debates a severity word does not.

  Confirmed by you
  The 47 observations in this report were confirmed as factually
  accurate by Harbor Community Foundation on 2026-09-21. Judgments
  and estimates are the auditor's own and were not up for
  confirmation.
```

The fact-check sentence is the settled Q6 form: xlsx is the only fact-check artifact,
and the PDF carries this one sentence (repeated once in the methodology, nowhere else).
The verified-clean paragraph is still computed strictly from the ledger's
verified-clean predicate (W23); a rule contradicted elsewhere in the export can never
be named in the clean sentence.

### 5.3 Page 12: Code quality, debt and conventions in force

```
  Code quality, debt and conventions
  ------------------------------------------------------------------

  Where the codebase deviates from itself
  2 of 47 assessed endpoints deviate from the URI-versioned
  endpoint convention the other 45 follow. The deviation is
  mechanical to close and carries no runtime risk; align the two
  or document the exception. The deviating sites are in the
  workbook's Deviations sheet.

  3 of 31 database queries bypass the repository layer the rest of
  the codebase routes through. Worth closing before the pattern
  spreads. Sites in the Deviations sheet.

  Where no convention has settled
  Error responses have no settled shape: 60 percent of handlers
  return a structured code, 40 percent return bare strings. We
  recommend standardising on the structured code; the split, per
  handler, is in the Deviations sheet.

  Conventions in force
  Your codebase follows 12 conventions consistently across API
  design, data access and error handling. Three are worth naming:
  every endpoint carries a version segment; every repository
  returns domain types rather than database rows; every table
  change ships as a migration. The complete list, with the sites
  evidencing each one, is in the workbook's Conventions sheet.
```

Commitments carried over intact from the first design: the structured sentence always
leads and always prints its real numerator and denominator (numerator alone when the
denominator is honestly absent, never fabricated); the action phrasing is "align or
document the exception", never a defect "Fix:" line; the conventions paragraph is
Zach's own Q4 sketch, lightly adapted. No deviation or convention content ever appears
inside the risk narrative; kind-on-wire makes the routing mechanical.

### 5.4 Page 13: Dependency health

Designed fresh, because the licence check is now unparked (Zach, on being shown it was
item 6 of his own research: "Oh, that's where it came from. Okay this one is easy, add
that one in, I think we can add this one quickly.").

**When everything is clean:**

```
  Dependency health
  ------------------------------------------------------------------

  This codebase depends on 212 packages (34 direct, 178 pulled in
  by those). Checked against the public vulnerability databases as
  of 2026-09-18, none carries a known vulnerability. Every licence
  in the tree is permissive (MIT, Apache-2.0, BSD, ISC): nothing
  here obligates you to publish your own source code or restricts
  a sale or an investment. The full package list, with each
  licence and its classification, is in the workbook's
  Dependencies sheet.
```

**On a copyleft hit, the hit is named in prose, because a rare and material fact is
exactly what prose is for:**

```
  One dependency requires attention. chart-render-pro (version
  2.1.0, pulled in directly) is licensed under the GPL-3.0, a
  strong copyleft licence. Distributing software that links it can
  obligate you to release your own source under the same terms,
  which is a standard red flag in investor diligence. Our
  assessment: replace it with a permissively licensed equivalent
  before any distribution or diligence event; isolating it behind
  a service boundary is the fallback if replacement is costly.
  Estimated 4 to 8 hours, carried in the budget under
  Dependencies. The remaining 211 packages are permissive.
```

**Rules of the section:**

- A CVE in the tree renders the same way: counts in prose, the worst advisory named
  and assessed, rows in the Dependencies sheet. Never a table of advisories in the
  PDF.
- Weak copyleft (LGPL, MPL) gets a one-sentence mention with a "usually fine as a
  dynamic dependency, listed in the workbook" framing, not a paragraph, unless the
  usage pattern makes it material.
- Unknown licences are named honestly as unknown and listed in the sheet; the clean
  sentence is only printed when every classification is permissive, same fail-loud
  stance as the verified-clean predicate.
- Until the licence pass exists, this page covers CVEs only, and the scope page
  carries "a dependency licence review was not performed this engagement". The
  section never implies coverage it does not have.
- A copyleft hit with assigned hours appears in the budget as a Dependencies area
  row. It does not additionally appear in the risk narrative; one home per fact.

**Workbook side:** the existing Dependencies sheet gains Licence and Classification
(permissive, weak copyleft, strong copyleft, unknown) columns when the pass lands.

**Engine side:** the pass is deterministic over lockfiles (package-lock.json,
Cargo.lock, siblings as stacks demand): map each package to its declared licence,
classify, emit. It is also a candidate new rule class for the corpus, which the
shelved pass-3 note already exempts from the shelf as data work. Build position in
section 7, Phase C.

---

## 6. The cover line

Settled shape (Zach's correction accepted): the clause limits RELIANCE, never
DISTRIBUTION, because a founder putting this report in a data room is the product
working as intended. Refined wording, keeping his shape and fixing the double
"prepared for" and the unpinned date:

> Prepared for [Client] by Cantus Works. This report describes the repositories at the
> commits named above, as of the date of signature. [Client] may share it at its own
> discretion. Cantus Works owes a duty of care to [Client] alone and accepts no
> responsibility to any other party who relies on this report.

The third sentence makes the sharing permission explicit instead of implied, which is
the data-room use case said out loud; the fourth keeps his liability clause with "duty
of care" added, which is the phrase a lawyer reading a data room expects to find. The
first design's section 11 draft ("not for distribution outside [Client] and its
professional advisors without written consent") is dead; it was a distribution
restriction and he rejected the shape.

---

## 7. Build order

### Verified facts first, one correction to the first design

Both of the first design's load-bearing technical claims were re-verified against the
current tree rather than trusted:

1. **`rules_not_run` exists and is never rendered: CONFIRMED, cheap win intact.**
   `ReportJson.rules_not_run: Vec<ExcludedRuleJson>` is built and populated in
   `crates/server/src/report_export.rs` (struct at line 1224, field at line 1336,
   population from the ledger's excluded rules around line 4458). A grep of
   `crates/server/templates/audit_report.typ` for `rules_not_run` returns nothing.
   The scope section's "what was not checked" paragraph is template-only work. Note
   the committed `sample-report/data.json` predates this field (its top-level keys
   stop at `dependency_snapshot`/`methodology`/`disclaimer`), so the sample must be
   regenerated before the new template can be exercised against it.
2. **"`FindingKind` is not on the `CuratedSiteJson` wire": NO LONGER TRUE. The first
   design's structural blocker has already been removed.** `CuratedSiteJson` now ends
   with `#[serde(flatten)] pub kind: camerata_api_types::audit::KindOnWire` (Pass-1
   W3, requirement A5.6, `report_export.rs` around line 1200). `KindOnWire`
   serialises a `kind` discriminator on every row, `"unspecified"` when absent, with
   the variant payloads flattened alongside: a Deviation row carries
   `deviating_sites`, `assessed_sites`, `convention_option_id` and
   `convention_label` directly on the site object (`crates/api-types/src/audit.rs`,
   `KindOnWire` at line 124, `DeviationKind` at line 155). The template simply does
   not read any of it yet. **Consequence: distinguishing Defect from Deviation from
   ConventionObserved in the PDF, rendering the structured deviation sentence, and
   counting conventions for the in-force paragraph are all template-only work.** The
   first design's Phase B loses its largest item to Phase A.

### Phase A: template-only, against the JSON contract as it exists today

Everything here needs no server change. It cannot reach a client before W21
(repo-relative paths) and W25 (text hygiene) land, but it can be built and reviewed
against regenerated sample data immediately.

- All the cuts: the severity x effort matrix page, the category scorecard page, the
  risk register summary table, the citations appendix page, the standalone
  verified-clean page, effort and confidence chips, disposition strings, inline
  "Also violates" lines, per-finding snippet blocks, deviation site lists,
  no-convention option tables.
- The reorder into the section-2 page plan.
- The risk narrative structure (section 4), rendering Defect rows only via the kind
  discriminator, with the CRITICAL/HIGH paragraph, MEDIUM sentence, LOW/INFO
  aggregate tiering.
- The deviation sentences and the no-convention sentences from the flattened kind
  payloads.
- The conventions-in-force paragraph, counting `convention_observed` rows from the
  wire. (Phase A counts OBSERVED conventions; if Zach's curator-adopted subset
  differs, Phase B refines the count to adopted-only when that set reaches the
  export. The paragraph's shape does not change.)
- The scope section's coverage and not-checked paragraphs from `rules_not_run` and
  the methodology fields.
- The how-to-read half page, static prose.
- Hiding held-for-review content in non-draft renders via the existing
  `review_state` signal (presentation-side only; the real refusal is Phase B).
- Regenerating `sample-report/` from the current contract so the template has
  truthful input.

### Phase B: export and server changes, no engine change

- **The signed-export refusal gate**: a non-draft export FAILS, with a message
  naming the held rows, while any held row exists. Server-side in the export path,
  not a template conditional; a template can only hide, and the settled decision is
  refuse. ("Force me to triage, that's the whole point of the agency.")
- The budget data: an operator-entered rate field on `ReportOptions` (absent means
  hours-only, never a default rate), per-finding hour ranges on every open row
  (W26's floor-rows-always-estimated feeds this), and the area subtotals and total
  computed server-side so the template never does arithmetic.
- Stable R-nn ids assigned at export, written to the PDF data and to a new R-id
  column on the workbook findings sheets, so PDF prose and workbook rows
  cross-reference by the same name. Re-export stability is settled by §9.2:
  **ids are retired, never reused, and the survivors never renumber**, so a
  re-exported report may read R-01, R-02, R-03, R-05. This has two build
  consequences beyond the id column:
  - The id assignment must be **persisted at first export and read back on
    re-export**, not recomputed from the surviving rows. A counter derived from
    the current row set renumbers by construction, which is precisely the
    behaviour §9.2 forbids. This is the part that is easy to get silently wrong:
    a recomputed id looks correct in every single-export test and only diverges
    on the second export, so it needs a test that **exports, kills a
    middle-ranked finding, re-exports, and asserts the survivors kept their
    original ids** — the gap is the assertion, not an artifact of it.
  - The withdrawal line is a **required, data-driven** element, not prose the
    operator remembers to add: when and only when the re-export has at least one
    retired id, the template prints `Findings R-04 and R-11 were withdrawn
    following your technical review.` immediately after the risk register, with
    the retired ids listed in ascending order. Rendering it needs the retired set
    on the wire, so the persisted id record has to carry withdrawn ids and not
    merely the surviving assignments.
- The intake-questions block on `ReportOptions` (three questions, per-question
  answer in the closed YES / NO / NOT ASSESSED FROM CODE vocabulary, answering
  R-ids, hours), with defaults synthesised server-side from the harvest-fear rule
  families when the client supplied none.
- The fact-check fields: sent-on and confirmed-on dates, the observation count for
  the one printed sentence, and the facts-list xlsx export mode (a filtered view
  over data the exporter already has; xlsx only, per the settled Q6).
- The curator-adopted conventions set routed into the report JSON, refining the
  Phase A count.
- The cost column added beside Est. hours on the workbook findings sheets.
- Export gates, shared with W21/W25 enforcement: fail the build on any
  `"unspecified"` kind reaching the renderer, any absolute path, any
  pipeline-vocabulary token in a client-facing field.

### Phase C: engine work

- **The licence pass** (unparked): deterministic over lockfiles, package to licence
  to classification, emitted as data the Dependencies sheet and the
  dependency-health section render. Also registered as a candidate rule class for
  the corpus. Until it lands, the scope page prints the not-performed line and the
  dependency section stays CVE-only.
- File-level exclusion reason classes for the scope paragraph, only if current
  accounting proves to record counts without reasons; the honest aggregate sentence
  ships either way.

Phases remain independently shippable in order A, B, C, and the questions page's
template shell still lands ahead of its plumbing (built in A against synthesised
defaults, wired in B), so B is wiring rather than design.

---

## 8. What this design cuts beyond the first design's cuts

The first design already cut the matrix page, the scorecard page, held-for-review from
signed deliverables, the chips, the disposition strings, the inline citation lines, and
conventions-as-findings. This design additionally cuts, all by force of the governing
principle:

1. **The risk register summary table** (first design page 9). Its job is done by the
   executive summary's counts, the budget's area rows, and the workbook's All
   Findings sheet.
2. **Per-finding evidence blocks** (pages 10-16's snippet, exact line, full fix text,
   per-finding citation). The finding survives as a named prose paragraph; the
   evidence lives in the workbook row with the same R-id.
3. **Per-finding budget rows** (page 5). Area subtotals and the total survive as the
   one table in the document; the per-item breakdown is two workbook columns.
4. **Deviation site lists and no-convention option tables** (pages 17-18). The
   sentences survive; the sites are the Deviations sheet.
5. **The standalone verified-clean page** (page 21), collapsed to one paragraph of
   the scope section, still ledger-predicate-computed.
6. **The citations appendix** (page 25). Citations are workbook columns; the glossary
   folds into methodology.
7. **The "not checked" table** on the scope page, collapsed to a paragraph with
   grouped reasons.
8. **Curation vocabulary in the executive summary** ("candidates", "dispositioned"):
   not a page but a cut, reworded per sketch 5.1.

Still deliberately NOT cut, restating the settled rulings so this document cannot
drift: no score, no go or no-go verdict, no bus-factor or git-history team analysis,
anywhere, ever. And nothing is cut from the WORKBOOK; the principle moves content
there, and the loop guidance stands that the complete picture errs toward showing
more, in the artifact built for rows.

---

## 9. Open questions

**None.** All seven of the first design's questions and the one question this design
raised are settled. The two rulings Zach gave on 2026-10-17 are recorded below as
committed requirements.

### 9.1 SETTLED (2026-10-17) — the engineer is not a reader of the PDF

Section 2's demotion of the risk register from seven pages of per-finding blocks to
3-4 pages of per-area narrative is **confirmed**. Zach:

> "I am fine with this, the report is a summary that can be used by non technical
> people, the workbook is for the granular items."

This is now the governing reader model for the whole document, not just the register,
and it settles future disputes about what belongs in the PDF: **the PDF's reader is
non-technical.** An engineer who needs per-finding evidence, fix text, citations or
site lists is a workbook reader. Any future proposal to restore row-shaped detail to
the PDF has to argue against this line first.

### 9.2 SETTLED (2026-10-17) — R-ids keep gaps, and the gaps are named

R-nn ids are assigned at export. When the client's fact-check kills a finding and the
report is re-exported before signature, the dead id's number is **retired, not reused,
and the remaining ids do not renumber.** A report may therefore read R-01, R-02, R-03,
R-05.

Rationale: the client sees the ids twice, once in the fact-check draft and once in the
signed report. Renumbering would make the signed report's R-04 a *different finding*
than the R-04 the client just approved removing, silently invalidating their reply
email, their margin notes and any ticket opened off the draft. The id still resolves;
it resolves to the wrong thing, with no error anywhere. A silent referent change is
the one failure this document cannot absorb, because trustworthiness is the entire
product.

**The gap must be named, not left to be noticed.** Immediately after the risk
register, when and only when at least one finding was withdrawn, the report prints a
line of the form:

> Findings R-04 and R-11 were withdrawn following your technical review.

This is a requirement, not a nicety, and it is what makes the ruling strictly better
than either branch considered. It converts the one real cost of gaps (a client counts
to eight, sees seven, and wonders what was hidden) into affirmative evidence that the
fact-check procedure ran and changed the output. That the engagement is not raw
scanner output with an invoice attached is close to the center of what is being sold,
and contiguous renumbering would erase the only visible proof of it.

Rejected alternative, recorded so it is not re-litigated: assigning ids *after* the
fact-check, which yields contiguous ids and no gaps ever. It fails because the
fact-check draft then has no stable handle for the client to reference, which moves
the same problem one document earlier and makes the draft harder to discuss.

**Out of scope, flagged for a future thread:** whether R-ids should be stable *across
engagements*, so a re-scan can say "R-07 from the March report is still open." That
needs a durable finding identity rather than a per-report counter, and is a different
and harder problem than this one.

---

## 10. Supersession note

`docs/plans/2026-10-16_pdf-redesign-report.md` remains the record of the reasoning,
the reader model, the five-kinds mapping, the fact-check procedure and Zach's answers.
Its section 3 page plan is superseded by section 2 here; its section 8 build sequence
is superseded by section 7 here (the kind-on-wire correction moves its largest Phase B
item into Phase A); its section 11 cover-line draft is superseded by section 6 here.
The W27 entry in the remediation plan should point at THIS document.

## Addendum 2026-10-24: the visual-design track (Zach)

The plan above settles CONTENT and STRUCTURE. Zach wants the PDF to also carry a visual identity:
bright, cheery and fun to read, not flat and mechanical. This is deliberate differentiation from
other contractors and consultants (a typical third-party pen-test report: ~130 grey pages with raw
request bodies and bearer tokens). The PDF's reader is non-technical (D4), so there is room for
style. The mood is early-2000s optimism (the XP-to-Vista, Frutiger Aero feeling: saturated,
glossy, sky and aqua tones), echoing the app's planned XP theme without copying it.

Scope (rides inside the W27 rebuild and its review sessions; not a new phase):
- **Cover:** the Cantus Works mark, a vivid gradient or original vector illustration, client name
  set large, the liability line (section 6) unchanged.
- **Visual summaries:** a severity chart, a findings-by-category bar and a one-glance scorecard
  near the front, so a non-technical reader gets the picture before any prose.
- **Section openers and callouts:** coloured section headers, a "what to do this week" callout,
  small category icons (security, architecture, dependencies). All vector (Typst-native shapes or
  original SVG), so the file stays small and sharp.

Guardrails:
- **Fun in the frame, not in the findings.** Colour and graphics live in the cover, openers,
  charts and layout; finding text stays plain and sober so a critical issue still reads serious.
- **Survives print and every reader.** Severity never relies on colour alone (an icon or label on
  every severity mark, a colour-blind-safe palette); high text contrast; readable in greyscale.
- **One brand with the website.** Palette, typefaces and logo come from Zach's Cantus Works brand
  (his website and marketing materials, in progress). Visual design work starts once those brand
  decisions are made, so prospect and client see one identity.
- **Original assets only;** no third-party logos, stock art or OS imagery.
- The release gate still runs over everything the template renders.
