# Brownfield Audit Hardening + PDF Report Export — Design/Build Spec (2026-07-23)

Source: Fable investigation. Goal: make the brownfield audit + its output client-facing /
board-forwardable quality (contractor deliverable). The curation engine is already strong
(dedup → merge → calibrate → human triage); the work is a small provenance stamp + four data
additions + a deliberately TINY serializer+template. Paths under repo root.

## Guiding constraints
- The PDF export is a **serializer + one Typst template, NOT a subsystem**. Scope-guard hard.
- No LLM calls in the export path (deterministic, instant, reproducible).
- Triage state stays client-local until Process; the export POST carries the dispositions.
- Implementation is Sonnet (not Fable); agents do work DIRECTLY (no sub-agents); ship with tests + docs.

## Build order
1. **`ScanProvenance` stamp** on `ScanReport` (server) — the biggest credibility gap.
2. **`FalsePositive` disposition** (the explicit ask) — 4th triage state.
3. **Structured `confidence` + `effort`** on `Finding` (via calibration).
4. **`report_export.rs`** serializer (`AuditReportJson` + pure `build_report_json`).
5. **Typst template + `compile_pdf` + route + UI button.**
6. (optional) dep-audit fixed-in version + CWE refs on SEC-family corpus TOMLs.
Steps 1-3 = pass A; 4-5 = pass B.

---

## Part 1 — Data-model hardening (pass A)

### 1. ScanProvenance (highest leverage)
Today NONE of the audit's own provenance is captured on `ScanReport` (onboard.rs:289). A
board-forwardable report must state exactly what was audited. Add `provenance: ScanProvenance`:
```
ScanProvenance {
  audited_refs: Vec<{ repo, sha, branch, dirty: bool }>,  // git rev-parse HEAD + git status --porcelain per source dir
  audit_model, calibration_model,                          // resolved at onboard_audit_start (lib.rs:4941-4949), currently discarded
  mode, thorough, deep,
  rules_fingerprint,                                       // already computed (onboard.rs:525) — just stamp it
  audited_rule_ids: Vec<String>,                           // the `selected` set actually audited (NOT proposed_rules)
  camerata_version: env!("CARGO_PKG_VERSION"),
  osv_scanner_version: Option<String>,
  started_at, finished_at,
}
```
Capture the git ref in `resolve_local_sources`/`audit_repos` (git already shelled out in
greenfield.rs:118/129, workspace.rs:104). A dirty tree does NOT block the scan, but the report
prints "working tree contained uncommitted changes" next to the SHA. `audited_rule_ids` is what
powers the "what's healthy" derivation (clean rules) — capture it.

### 2. FalsePositive disposition (the explicit ask)
Current `TriageState { Unresolved, Ignored, TechDebt }` (ui-core/src/triage.rs:21). Ignored =
"real but accepted risk" → committed to `.camerata/baseline.json` at Process. There is NO way to
say "this finding is WRONG"; today a FP gets shoehorned into Ignored, which is dishonest twice
(pollutes the baseline of accepted-real-debt, and would read as "accepted risk" in a client report).
Add `FalsePositive` as a 4th `TriageState`:
- `triage.rs:21`: add variant + `label()` "False positive"; serde additive-safe. `counts` → 4-tuple;
  add `mark_false_positive(findings, reason)` mirroring `ignore` (keep the require-reason invariant —
  the reason is the audit trail + feeds the methodology count).
- UI (scan.rs:3130): a 4th table/tab.
- **Process semantics (CRITICAL, differs from Ignored):** FP findings do NOT go to the baseline and
  do NOT create tickets — at Process they are a server-side no-op. Do NOT wire into baseline.json (a
  durable machine suppression for "the tool was wrong" is exactly the invisible-hole the reasonless-
  waiver rule prevents, audit.rs:181). If the same code trips again next scan, that's correct — the
  reviewer re-judges.
- **Report semantics:** excluded from the findings section entirely; counted once in Methodology
  ("M candidate findings reviewed; N dispositioned as false positives by the auditor and excluded").
  That sentence IS the curation differentiator made visible.

### 3. Structured confidence + effort on Finding
- **confidence**: today string-embedded — `apply_verdicts` appends `[needs review: reason]` into
  `detail` (ai_audit.rs:575) and never sets the structured `needs_review` for AI findings; the UI
  regex-parses it back (`split_needs_review`, ui-core/src/rules.rs:21). Promote to
  `confidence: Option<String>` on `Finding` + set `needs_review` server-side in `apply_verdicts`
  (keep the `[needs review]` detail tag one release for UI back-compat).
- **effort**: absent entirely; blocks the severity×effort matrix. Add `effort: Option<String>`
  ("low|medium|high") emitted by extending the calibration verdict JSON schema (ai_audit.rs:456) —
  calibration already reads every finding with path/snippet/detail (ai_audit.rs:620). Thread through
  `apply_verdicts` + `consensus_verdicts`.

### 4. Citation join (serializer-internal, no schema change)
A `Finding` carries only `rule_id`; the CWE/OWASP/linter sources live in the corpus
(`RuleSource`, rules/src/lib.rs:265; e.g. sec-* TOMLs cite OWASP). Join `rule_id → corpus sources`
at serialization time (the corpus is already loaded at onboard_audit_start via split_scannable_rules,
lib.rs:4934). For preview findings use `preview_tool` + rule_id (the real linter rule); for AI- ids
label "AI-advisory, model-inferred." This join is THE feature that turns the Grounded ladder into the
client-facing trust artifact. (Optionally add CWE refs to the SEC-family corpus TOMLs — grounded, one-time.)

### (optional) dep-audit fixed-in
`parse_osv_json` (dep_audit.rs:155) captures advisory/aliases/summary/severity but not
`affected[].ranges[].events[].fixed` — the most actionable datum ("bump `time` to 0.3.36"). Add it +
structured advisory fields for the §7 table (detail is deterministic so the serializer CAN re-parse,
but structured is cleaner if you touch dep_audit anyway).

---

## Part 2 — The PDF export (pass B; serializer + template, nothing more)

Typst 0.15 installed at /opt/homebrew/bin/typst; repo has zero typst refs (clean slate).

### The whole feature (one module + one template + one route + one button)
**`crates/server/src/report_export.rs`** (~300-400 lines):
- `struct AuditReportJson` (serde::Serialize) — the serializer's one output type, mirroring the §
  sections below.
- **pure** `fn build_report_json(report: &ScanReport, dispositions: &HashMap<String, DispositionWire>,
  corpus: Option<&Corpus>, opts: &ReportOptions) -> AuditReportJson` — unit-testable, no I/O.
  `DispositionWire = {state, reason, bucket}` keyed by the existing `finding_key` (triage.rs:110)
  the client already computes.
- `async fn compile_pdf(json: &AuditReportJson) -> Result<Vec<u8>>`: temp dir, write `data.json` +
  `report.typ` (`include_str!("../templates/audit_report.typ")` so the binary is self-contained), run
  `typst compile report.typ report.pdf --root <tmp>` (~30s timeout), read bytes, cleanup. Template
  loads data via Typst's `#let d = json("data.json")` — no `--input` plumbing. Fail-soft: if `typst`
  isn't on PATH, return a clear "Install Typst: brew install typst" (dep-audit coverage-note tone).

**Route** (lib.rs, next to deep-report export ~lib.rs:13683):
`POST /api/projects/:id/audit-report` body `{ dispositions: {...}, options: { client_name,
project_title, prepared_by, executive_summary_override: Option<String> } }`. Handler: 404 if no
project; load `state.last_scan.get(pid)`; build JSON; compile; respond `application/pdf` +
`Content-Disposition: attachment; filename="camerata-audit-{repo}-{shortsha}.pdf"`. Dispositions come
from the client (triage is local until Process).

**UI button** (scan.rs, beside the CSV export ~scan.rs:1413): "Export audit report (PDF)" → POST
`dispositions.read()` + a small options modal → save bytes via the same rfd path `save_csv` uses
(scan.rs:118).

**Template** `crates/server/templates/audit_report.typ` — ONE file.

### §4 report anatomy → data → Typst
1. **Cover**: repos, files_scanned, code_chars + SHA/branch/dirty (P1), date, models, camerata version.
2. **Executive summary (1 page)**: deterministic template text over the counts + `executive_summary_
   override` for a hand-written narrative (NOT an LLM call): "N curated findings: X do-now, Y do-next…;
   Z candidates reviewed and excluded as false positives." + top-3 do-now one-liners.
3. **Category scorecard**: rule_id prefix + corpus `domain` → categories; severity counts; status chip
   (Clean / Attention / Action-needed by threshold — NO letter grades). "Checked vs no-findings" needs
   audited_rule_ids (P1).
4. **Severity × effort matrix** (the money page): 2×2 grid — Do now (high-sev×low-effort), Do next
   (high×med/high), Plan (med/low×med/high), Accepted (Ignored disposition). Needs effort (P3) +
   disposition overlay. Cells list finding ids.
5. **Curated findings**: grouped by rule within repo (heading: title + citation block + count), then
   per-site rows (path:line, snippet block, impact/fix, effort + confidence chips, disposition
   annotation). Needs citation join (P4), confidence (P3), effort (P3), FP-excluded filter.
6. **What's healthy** (anti-FUD): derive from audited_rule_ids (P1) — audited rules with zero findings
   (with their grounded citations: "No raw SQL concat across 214 files (OWASP: SQL Injection)"), floor-
   clean statements, clean dep ecosystems, deep-tier "met" controls if present. NO new LLM pass. Frame:
   "verified absent in this scan," never "guaranteed absent."
7. **Dependency/CVE snapshot**: DEP-AUDIT-1 findings → table (package@version, advisory+CVE aliases,
   severity, fixed-in [P6]) + coverage-note honesty line. Clean = a §6 positive.
8. **Methodology & limitations**: the two-tier engine (deterministic floor = proven defects, always
   critical; AI tier = advisory, calibrated, human-triaged), the curation pipeline + the M-reviewed/
   N-excluded numbers, what was NOT done (no pen test / runtime / org-controls — reuse
   DEEP_ADVISORY_DISCLAIMER), the bimodal severity-scale explanation.
9. **Disclaimer**: a general-audit variant of DEEP_REPORT_ADVISORY (lib.rs:13666); final page + footer.

**FP exclusion in the serializer:** partition by `dispositions.get(finding_key(f))`: FalsePositive →
excluded+counted; Ignored → included in §5 as "accepted risk"+reason (matrix "Accepted"); TechDebt{Now}
→ do-now; TechDebt{Later} → planned; Unresolved → "open" (warn-not-block: a draft mid-engagement report
is legitimate). Suppressed-baseline (status) → pre-existing accepted debt, not new findings.

## Scope-guard — what NOT to build
No report subsystem / template engine / theming / multi-format. No LLM in the export path. No report
persistence/versioning/history (re-export regenerates from last_scan + dispositions). No FP-telemetry
feedback into rules (count only). No letter grades / posture index. No charts needing assets (Typst-
native tables/grids, default fonts). No new deep-tier lenses (SOC-2 stays "gap analysis"). No server-
side disposition store (POST carries it).
