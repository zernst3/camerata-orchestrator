# Product Export: ZIP of PDF (narrative) + Excel (working dataset)

Design pass for `feat/audit-report-export` on camerata-orchestrator. Scope: product definition + implementation-ready spec. No code changes in this pass.

---

## 1. Product definition

**The deliverable is one ZIP: `camerata-audit-{repo-slug}-{short-sha}.zip`** containing:

| File | Role | Audience |
|---|---|---|
| `camerata-audit-{repo-slug}-{short-sha}.pdf` | The curated NARRATIVE: cover, exec summary, three-things box, scorecard, severity x effort matrix, curated findings w/ citations, what's healthy, dep snapshot, methodology. Stays lean; FPs excluded. | Board / buyer / decision-maker |
| `camerata-audit-findings-{repo-slug}-{short-sha}.xlsx` | The COMPLETE working dataset: every finding, one row each, sortable/filterable, per-category sheets, top-tier formatting. Nothing curated out (FPs get their own sheet, not silence). | Client's engineers doing remediation |
| `README.txt` (~15 lines) | Manifest: what each file is, who it's for, repo + SHA + generated-at, the advisory disclaimer paragraph (reuse `AUDIT_REPORT_DISCLAIMER`). | Whoever unzips it |

**Partition principle:** the PDF answers "what should we do and why should we trust it"; the Excel answers "show me ALL findings by category so I can work the list." Anything that would bloat the PDF (full finding tables, low-severity noise, FP bookkeeping) lives in the Excel. The PDF never grows a findings appendix again; the Excel never tries to tell a story.

Both artifacts are serializers over the SAME data: `last_scan` + the POSTed dispositions + the corpus join. One `build_report_json`-family pass, no second scan, no LLM call in the export path (preserves report_export.rs's "deliberately TINY" contract).

---

## 2. Existing-mechanism audit

Verified by grep across the workspace (`xlsx|excel|spreadsheet|zip|csv` in all crates + Cargo.tomls):

| Mechanism | Exists? | Where | Reusable? |
|---|---|---|---|
| CSV export | YES | `crates/ui/src/cockpit/scan.rs:144` `findings_csv` (client-side, flat, 11 cols: repo, severity, status, rule_id, also_matches, path, line, snippet, detail, preview, preview_tool). Button at scan.rs:1535. | Superseded by the Excel for the product; keep as the quick in-app export. |
| PDF export | YES | `POST /api/projects/:id/audit-report` (lib.rs:1228, handler ~13899) -> `report_export::build_report_json` + `compile_pdf` (Typst). UI: `AuditReportExportPanel` (scan.rs ~3690) -> `save_bytes`. | FULLY reusable; the product export wraps it. |
| Excel (.xlsx) | **NO** | Nothing. The owner's memory of an existing Excel export is wrong; only CSV exists. | Net-new. |
| ZIP assembly | NO | No `zip` crate dependency anywhere (the only "zip" hit is a gunzip comment in server/Cargo.toml). | Net-new dep. |
| Save-to-disk UI | YES | `save_bytes` (scan.rs:132, rfd native dialog) already handles arbitrary bytes. | Reused as-is for the ZIP. |

**xlsx library (ROUTED, new dependency):** recommend **`rust_xlsxwriter`**.
- Pure Rust, zero unsafe, actively maintained by jmcnamara (author of the canonical Python XlsxWriter). Covers everything this spec needs: multiple worksheets, cell/row formats, conditional formatting, autofilter, freeze panes, column widths, internal hyperlinks, worksheet tab colors, `save_to_buffer()` (in-memory bytes, ideal for the HTTP response / zip entry).
- Alternatives rejected: `xlsxwriter` (C FFI bindings, unsafe, awkward error model); `simple_excel_writer` (unmaintained, no formatting to speak of); `umya-spreadsheet` (read+write generality we don't need, heavier, less ergonomic write API); CSV-in-a-zip (fails the "top-tier formatting" requirement outright).
- **zip crate:** `zip` (the de-facto standard) with `deflate` feature for the outer ZIP. rust_xlsxwriter bundles its own zip internally; the `zip` dep is only for the product envelope.

---

## 3. Excel workbook structure

### Sheets (in tab order)

1. **`Index`** (tab color: neutral dark)
   - Title block: project title / client name / prepared by (from `ReportOptions`).
   - Provenance block (mirrors `CoverJson`): repos, per-repo SHA + branch + dirty flag (from `audited_refs`), audit model, calibration model, camerata version, generated-at (UTC), files scanned / excluded, code chars.
   - **Counts matrix: category x severity** (critical/high/medium/low columns, one row per category, totals row), plus a disposition summary line (open / do-now / do-next / plan / accepted / FP-excluded) matching the PDF's numbers exactly. Derive both from the same partition so they can never disagree.
   - **Legend:** severity color swatches; disposition vocabulary (Open / Accepted risk / Needs client confirmation / Tech debt now / Tech debt later / Baseline-accepted / False positive); provenance vocabulary (Deterministic / Preview:{tool} / AI-advisory); confidence (`high` / `needs-review`).
   - **Hyperlinked table of contents:** one row per sheet via `Url::new("internal:'Sheet Name'!A1")`, with that sheet's finding count and worst severity.

2. **`All Findings`** (the master working sheet) - every non-FP code finding across all categories, full column set, default-sorted severity desc. This is where "sort everything by severity" and cross-category filtering happen.

3. **One sheet per category** - category = `report_export::category_for(rule_id, corpus)` (the corpus rule's `domain`, prettified; rule-id fallback), i.e. exactly the PDF scorecard's grouping, so PDF and workbook categories always agree. Sheet name = the prettified label (e.g. `Supabase RLS`, `Security`), sanitized: strip `[]:*?/\`, truncate to 31 chars, dedupe with ` (2)` suffix. Order sheets by scorecard rank (Action-needed, Attention, Clean), matching the PDF. Tab color by worst OPEN severity in the sheet (red/orange/yellow/green).
   - Categories with zero findings this run get NO sheet (they appear in the Index counts matrix and Coverage sheet instead); an empty formatted sheet reads as broken.

4. **`Dependencies`** - the `DEP_AUDIT_RULE_ID` carve-out, same as the PDF's §7: columns Package (`snippet`), Advisory (`detail`), Severity, Repo; plus the osv-scanner coverage notes as a footer block. Kept out of the category sheets for the same reason the PDF separates them (edit-code vs bump-a-version remediation).

5. **`False Positives`** - every finding the auditor dispositioned FP: same columns as All Findings plus **FP Reason** (from `DispositionWire.reason`). This is the reconciliation with the PDF: the PDF excludes FPs everywhere and counts them once in methodology; the workbook excludes them from every WORKING sheet but preserves them here with the auditor's reasoning, so the client's engineers can audit the exclusions. Nothing is silently dropped.

6. **`Coverage`** - the "what's healthy" story in data form, UNCAPPED (the PDF caps at 10; the workbook doesn't need to): one row per `provenance.audited_rule_ids` entry with Rule ID, Title, Category, Citation kind + label, Findings count this run (0 = verified clean). Also list `excluded_mechanical_rules` in a footer block. This is the sheet that proves what was checked, not just what was found.

### Column set (All Findings + category sheets, in order)

| # | Column | Source | Width | Notes |
|---|---|---|---|---|
| A | Severity | `normalize_severity(f.severity)` | 10 | colored fill (below) |
| B | Headline | `defect_headline(detail, rule title)` | 50, wrap | the defect-first sentence, same derivation as the PDF |
| C | Repo | `f.repo` | 18 | |
| D | File | `f.path` | 40 | |
| E | Line | `f.line` | 7 | number format |
| F | Rule ID | `f.rule_id` | 28 | |
| G | Category | `category_for(...)` | 16 | on All Findings only; redundant on category sheets, keep anyway for copy-paste fidelity |
| H | Action | `matrix_bucket(...)` -> `Do now` / `Do next` / `Plan` / `Accepted` | 10 | the matrix cell, verbatim from the PDF logic |
| I | Disposition | `disposition_label(...)` | 30, wrap | identical wording to the PDF (incl. the never-fabricate `confirmed_by_client` gating) |
| J | Effort | `f.effort` (`low`/`medium`/`high` or blank) | 9 | |
| K | Est. hours | `effort_hours_bounds(...).1` label | 16 | "2 to 4 hours" / "not yet estimated" |
| L | Confidence | `f.confidence` or blank | 12 | |
| M | Needs review | `f.needs_review` -> `Yes`/blank | 12 | |
| N | In test scope | `f.in_test` -> `Yes`/blank | 12 | |
| O | Provenance | derived: preview -> `Preview: {tool}`; corpus-grounded -> `Deterministic`; else `AI-advisory` (reuse `resolve_citation().kind`) | 16 | |
| P | Citation | `resolve_citation().label` | 34, wrap | |
| Q | Citation URLs | joined `sources[].url`, newline-separated | 30, wrap | plain text, not hyperlink objects (multi-URL cells) |
| R | Also matches | `f.also_matches.join(", ")` | 24 | |
| S | Status | `f.status` (`active` / `suppressed-inline` / `suppressed-baseline`) | 18 | |
| T | Snippet | `f.snippet` UNCAPPED (workbook has room; cap at ~2k chars defensively vs Excel's 32,767 cell limit) | 55, wrap, monospace 9pt | |
| U | Detail | `f.detail`, full | 70, wrap | the complete checker explanation, not the headline |

~~Note: there is **no per-finding `fix` field** in the data model today... Do NOT add a Fix column in MVP...~~ **SUPERSEDED (2026-07-27 build pass) — see §7.** Zach approved a Recommended-Fix column after this design pass: rather than a new per-finding field (which would require a calibration-pass output that doesn't exist yet), it's a serialization-time join — `rule_id` → the corpus rule's own DEFAULT `[[option]].directive` — computed identically to the citation join (`resolve_citation`) at write time in both `report_export::build_report_json` (a `CuratedSiteJson.fix` field, rendered as the PDF's own "Fix:" line) and `xlsx_export` (the "Recommended Fix" column). Empty, never fabricated, when the rule has no corpus entry/option/directive. See `resolve_fix` in `crates/server/src/report_export.rs`.

### Formatting spec (implementable as written)

- **Header row:** bold white on dark slate fill (`#1F2937`), 11pt, row height 28, `freeze_panes(1, 0)` (freeze row 1 only; frozen columns fight horizontal scanning on a 21-col sheet).
- **Autofilter:** `autofilter(0, 0, last_row, last_col)` on every findings sheet + Dependencies + FP + Coverage.
- **Severity coloring, direct format at write time** (not conditional-formatting rules; the value set is closed and direct formats survive every Excel/LibreOffice/Numbers viewer identically): critical = white on `#B91C1C`; high = white on `#EA580C`; medium = black on `#FCD34D`; low = black on `#E5E7EB`. Applied to the Severity CELL only, plus a light row tint (`#FEF2F2`) across the full row for critical findings (mirrors the UI's `finding-row-critical`).
- **Additionally** one conditional-formatting rule per sheet: cell-value `Needs review == "Yes"` -> italic amber, so the flag survives re-sorting by the user.
- **Default sort:** rows pre-sorted at write time severity desc (critical, high, medium, low), then repo, path, line. (xlsx stores no sort order; pre-sorting + autofilter is the correct idiom.)
- **Wrap:** columns B, I, P, Q, T, U get `set_text_wrap()`; snippet in a monospace font (Courier New 9). All cells `vertical_align: top`.
- **Zebra banding** on non-severity-colored columns via alternating row format (`#F9FAFB`), skipped on critical-tinted rows.
- **Consistent theme:** one `Formats` struct built once (header, sev x 4, mono, wrap, banded, index-title, legend-swatch) and passed to every sheet writer; no ad-hoc `Format::new()` at call sites (lint-friendly, keeps the theme actually consistent).

---

## 4. Product-export flow

**Server (net-new module `crates/server/src/xlsx_export.rs`, ~mirror of report_export.rs's shape):**

- `pub fn build_workbook(report: &ScanReport, dispositions: &HashMap<String, DispositionWire>, corpus: Option<&RuleSet>, opts: &ReportOptions) -> anyhow::Result<Vec<u8>>` - pure except the in-memory buffer; unit-testable.
- It performs the SAME partition loop as `build_report_json` (FP split, `classify`, `normalize_severity`, dep carve-out). Make the needed helpers `pub(crate)` in report_export.rs (`classify`, `normalize_severity`, `category_for`, `matrix_bucket`, `disposition_label`, `defect_headline`, `resolve_citation`, `effort_hours_bounds`, `finding_key`) rather than duplicating them - the workbook must be able to disagree with the PDF in exactly zero places. (Alternative considered: feed the workbook from `AuditReportJson`. Rejected: that type already excludes FPs, caps snippets, and caps what's-healthy; the workbook needs the uncut data.)
- **Route:** `POST /api/projects/:id/product-export`, same body as `AuditReportReq` (`{ dispositions, options }`), same 404/500 contract. Handler: load last_scan + corpus once -> `build_report_json` -> `compile_pdf` -> `build_workbook` -> assemble zip in memory (`zip::ZipWriter` over `Cursor<Vec<u8>>`, Deflate; entries: pdf, xlsx, README.txt) -> respond `application/zip` + `Content-Disposition: attachment; filename="camerata-audit-{repo-slug}-{short-sha}.zip"` (reuse the existing slug/sha derivation verbatim).
- If `compile_pdf` fails (typst missing), fail the whole export with the same 500 message the PDF route uses today. A half-product (xlsx-only zip) is a support headache; the error message already tells the user to install typst.

**UI (`crates/ui/src/cockpit/scan.rs`):**

- `AuditReportExportPanel` keeps its exact fields (client name / title / prepared by / summary override) and disposition-snapshot POST idiom; the button becomes **"Export product (ZIP: PDF + Excel)"**, hitting the new route, saving via the existing `save_bytes` with the `.zip` filename. Panel label/hint updated to describe both artifacts.
- **Replace, don't coexist** (owner: "instead of a pdf export... a product export"): the standalone PDF button is removed; the PDF is always inside the zip, and one artifact = one thing to attach to the client email. The old `/audit-report` route can stay for one release (it costs nothing, useful for scripting) but grows no features. The in-table "Export CSV" button stays untouched; it's a quick in-app tool, not the deliverable.

---

## 5. MVP vs later

**MVP delivers the full "everything, by category" value with zero new instrumentation.** Every column in §3 is populated from fields that exist on `Finding` today (repo, path, line, rule_id, severity, snippet, detail, status, also_matches, preview, preview_tool, in_test, needs_review, confidence, effort) plus joins that already exist (corpus citation, category, matrix bucket, disposition label). Nothing waits on review instrumentation.

Future columns, once their upstream data exists (add to the right of the current set so client macros/filters don't break):
- **Review tier / sampled FP-rate** - needs the review-instrumentation work; today there is no per-rule FP-rate to print honestly.
- **Fix suggestion** - needs a per-finding remediation output from the calibration pass; no such field exists today (see §3 note).
- **Client-confirmed** as a first-class column - meaningful once `confirmed_by_client` is actually being set by a client-conversation workflow; today it is almost always false and would read as a column of noise. (The Disposition column already carries the "Needs client confirmation" wording where it matters.)

---

## 6. Routed decisions, estimate, build order

**Routed to Zach (per ROUTE-1 / new-dependency policy):**
1. **New deps:** `rust_xlsxwriter` + `zip` in `crates/server`. Recommendation: approve; rationale in §2.
2. **Replace vs coexist:** recommend REPLACE the standalone PDF button with the product-export button (route stays one release). Owner's "instead of" language points here, but it deletes a shipped UI affordance, so it's his call.
3. **ZIP contents:** recommend PDF + XLSX + README.txt, and NOT bundling the CSV (the Excel strictly supersedes it; a third findings format invites "which one is canonical" questions). Confirm README.txt inclusion + wording.
4. Minor: workbook filename `camerata-audit-findings-...xlsx` vs matching the PDF stem exactly; recommend the `-findings` infix so the two files sort adjacently but are distinguishable in a downloads folder.

**Effort (5x-corrected per the estimation rule):** first-instinct estimate is 4-6 days; corrected to **roughly one focused day of orchestrated work**. The xlsx module is mechanical serialization over an already-proven data pass; the only genuinely new surfaces are the rust_xlsxwriter API and zip assembly, both well-documented.

**Build order:**
1. `xlsx_export.rs` + `pub(crate)` promotions in report_export.rs + unit tests (test the intermediate row model - columns, ordering, FP partition, category sheet split - not the binary; plus one smoke test that `build_workbook` returns non-empty bytes with the xlsx magic `PK`).
2. `/product-export` route + zip assembly + handler test (404s, zip contains 3 entries with expected names).
3. UI button swap in `AuditReportExportPanel` + label/hint copy.
4. Docs: extend `docs/design/2026-07-23_brownfield-audit-report.md` (or a sibling doc) with the product-export section; ships with the PR per ship_with_docs_and_tests.

---

## 7. What shipped (2026-07-27 build pass)

All four routed decisions in §6 were approved as recommended: `rust_xlsxwriter` (0.96.0) + `zip`
(8.6.0, `default-features = false, features = ["deflate"]`) added to `crates/server/Cargo.toml`;
REPLACE (not coexist) for the export button; PDF + XLSX + README.txt zip contents, no CSV; the
`-findings` infix on the workbook filename.

**Deps.** `rust_xlsxwriter` pulls its own internal `zip` (v7) for its own xlsx-as-zip container;
the top-level `zip` v8 dependency is only for the OUTER product-export envelope — no conflict,
Cargo resolves both independently as the design doc anticipated.

**Sharing, not duplicating, the partition logic.** `crates/server/src/report_export.rs` promoted
`Disposition` (the enum) and `classify`, `normalize_severity`, `category_for`, `matrix_bucket`,
`disposition_label`, `bucket_title`, `defect_headline`, `resolve_citation`, `effort_hours_bounds`
to `pub(crate)`. `xlsx_export::partition_rows` runs its OWN loop over `report.findings` (per the
design's rejection of feeding the workbook from `AuditReportJson`), but every unit of
classification math inside that loop calls one of these shared functions — a finding's severity,
category, matrix bucket, disposition wording, and citation are computed by the EXACT SAME code the
PDF calls, so the two artifacts cannot independently drift on what a finding "is." Only the false
positive partition differs by design: the PDF drops FPs during its own loop; the workbook's
`partition_rows` keeps them (tagged `is_fp: true`, with `fp_reason`) so they can get their own
sheet.

**Recommended-Fix column / PDF "Fix:" line — the corpus-directive join.** New `resolve_fix(rule_id,
corpus) -> String` in `report_export.rs`: joins `rule_id` to the corpus rule's `resolved_option(None)`
(the rule's own DEFAULT option, since no per-project chosen-option exists on a `Finding` today) and
returns its `directive`, or an empty string when the corpus is absent, the rule is unknown, has no
options, or has no default option/directive. Wired in exactly two places, both reusing the SAME
function: `CuratedGroupJson::sites[].fix` (new field on `CuratedSiteJson`, populated in
`build_report_json`) and `xlsx_export::FindingRow.fix` (populated in `partition_rows`). The Typst
template (`crates/server/templates/audit_report.typ`, `render_site`) renders `Fix: {site.fix}` as
its own line, conditionally (`#if site.fix != ""`) — never an empty "Fix:" label when there's
nothing to say. The Excel column is the 22nd/last column ("Recommended Fix") on the All Findings
and every category sheet, appended after `Detail` per the design's own guidance for future columns
("add to the right... so client macros/filters don't break").

**Workbook shape — exactly as designed**, six sheets in tab order: `Index` (provenance block,
category×severity counts matrix + a disposition-summary line reconciling exactly with the PDF's
own do-now/do-next/plan/accepted/open/FP-excluded/dependency counts, a legend, and a hyperlinked
table of contents via `internal:'Sheet Name'!A1` `Url`s), `All Findings`, one sheet per category
(`category_for` — the PDF scorecard's own grouping, sanitized/deduped/truncated to Excel's 31-char
limit, ordered worst-open-severity-first, tab-colored red/orange/amber/green), `Dependencies`,
`False Positives` (the 22-column schema plus an FP Reason column), and `Coverage` (every audited
rule id, uncapped, with a findings-this-run count — 0 reads as verified clean). Categories with
zero findings get NO sheet (per the design's explicit call-out) but still appear in the Index
matrix and the Coverage sheet. Formatting matches §3's spec: frozen header row (`set_freeze_panes(1,
0)`), autofilter on every findings/Dependencies/FP/Coverage sheet, direct severity cell coloring
(critical/high/medium/low), a full-row critical tint, zebra banding on non-critical rows, one
conditional-formatting rule per findings sheet (Needs-review "Yes" → italic amber), and rows
pre-sorted severity-desc/repo/path/line at write time (xlsx has no native sort-order to persist).

**Product-export route.** `POST /api/projects/:id/product-export` (same `AuditReportReq` body,
same 404/500 contract as `/audit-report`) builds `AuditReportJson` once, compiles the PDF, builds
the workbook, and zips both plus a `README.txt` (reusing `AUDIT_REPORT_DISCLAIMER` verbatim) via
an in-memory `zip::ZipWriter` over `Cursor<Vec<u8>>` (Deflate). Filename derivation
(`camerata-audit-{repo-slug}-{short-sha}`) was extracted into one shared `report_filename_stem`
function so the PDF-only route and the product-export route can never name the same scan two
different ways. `/audit-report` is left in place, unmodified in behavior, for the one-release
back-compat window the design calls for.

**UI.** `AuditReportExportPanel` keeps its exact props and disposition-snapshot POST idiom; its
internal POST helper was renamed `export_product_zip` and now targets `/product-export`; the
button label is "Export product (ZIP: PDF + Excel)" and the panel hint describes both artifacts.
The in-table "Export CSV" button (`findings_csv`/`save_csv`) is untouched, as scoped.

**Tests.** `report_export.rs`: +5 tests for `resolve_fix`/the PDF `fix` line (real-corpus-directive
join, absent-corpus, unknown-rule-id, populated-on-a-curated-site, empty-not-fabricated). All 53
`report_export` tests green (48 pre-existing + 5 new), including the real-typst PDF compile test
and the backtick-interpolation regression guard. `xlsx_export.rs`: 15 new tests — header-schema
order, `PK` magic, sheet-name presence/order (incl. category-name matching + zero-finding
categories getting no sheet), FP-reason recoverability, the Recommended-Fix join (populated /
absent-corpus-empty), autofilter/freeze/conditional-formatting presence (verified by opening the
produced xlsx as a zip and grepping its worksheet XML — `<autoFilter`, `state="frozen"`,
`<conditionalFormatting`), sheet-name sanitization/dedup, and four degenerate-input cases (zero
findings, all-false-positive, no dependency findings, no corpus at all) — none panic, all produce
a valid `PK`-prefixed workbook. `lib.rs`: +3 tests for the route (404 no-project, 404 no-scan,
and — gated on `typst` being on PATH, matching the project's existing skip-don't-fail convention —
a full round trip asserting the zip contains a `%PDF`-prefixed PDF entry, a `PK`-prefixed xlsx
entry, and a README.txt naming both). `cargo test -p camerata-server` (lib): 1226 passed, 0 failed.
`cargo check --workspace`: green. `cargo test -p camerata-ui --bin camerata-ui cockpit::scan::`:
50 passed, 0 failed.

**Sample regeneration.** `crates/server/tests/generate_sample_report.rs` (still `#[ignore]`d, run
via `cargo test -p camerata-server --test generate_sample_report -- --ignored --nocapture`) now
also calls `xlsx_export::build_workbook` on the exact same hand-built `ScanReport` fixture used for
the PDF, writes `sample-report/camerata-sample-audit-findings.xlsx`, sanity-checks its `xl/workbook.xml`
contains all five always-present sheet names, then assembles `sample-report/camerata-sample-audit.zip`
(inlining the same three-entry Deflate-zip logic the server route uses — the route's own
zip-assembly helper is private to `camerata-server`, so this doesn't call it directly, but the PDF
and xlsx BYTES zipped are the real serializer output). The PDF and its preview PNGs were also
regenerated (`typst compile sample-report/audit_report.typ "sample-report/page-{p}.png" --ppi 130`)
since the new "Fix:" line changed pagination (10 → 12 pages).

**Judgment calls not explicitly pre-decided:**
- Provenance-column derivation (`Deterministic` / `Preview: {tool}` / `AI-advisory`) reuses
  `resolve_citation(...).kind` exactly as §3's column table specifies, rather than introducing a
  parallel classification.
- Category sheet tab color uses four tiers (red for an open critical, orange for open-high,
  amber for open-medium, green for clean) rather than the three-tier PDF scorecard status,
  since a spreadsheet tab strip has room to distinguish critical from high at a glance where the
  PDF's three-way Action-needed/Attention/Clean chip does not need to.
- The Index sheet's disposition-summary line includes an explicit "open" count (the PDF's own
  narrative deliberately DROPPED that as a redundant subset restatement — see the Item 2 decision
  in `docs/design/2026-07-23_brownfield-audit-report.md`); a spreadsheet summary line is a data
  readout, not board prose, so the redundancy tradeoff differs and the count is easy to verify
  against the category matrix's own totals.
- Snippet capping in the workbook uses a fresh ~2,000-char cap (`WORKBOOK_SNIPPET_MAX_CHARS`,
  distinct from the PDF's ~800-char/12-line `cap_snippet`) per the design's own defensive-only
  rationale (Excel's 32,767-char cell ceiling) — this is a presentation-width choice, not
  partition logic, so it does not violate the no-duplicate-classification rule.
