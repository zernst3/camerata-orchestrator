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
1. **`ScanProvenance` stamp** on `ScanReport` (server) — the biggest credibility gap. **DONE.**
2. **`FalsePositive` disposition** (the explicit ask) — 4th triage state. **DONE.**
3. **Structured `confidence` + `effort`** on `Finding` (via calibration). **DONE.**
4. **`report_export.rs`** serializer (`AuditReportJson` + pure `build_report_json`). **DONE.**
5. **Typst template + `compile_pdf` + route + UI button.** **DONE.**
6. (optional) dep-audit fixed-in version + CWE refs on SEC-family corpus TOMLs. Deferred —
   not trivial (touches `dep_audit.rs`'s OSV parser + corpus TOMLs), out of Pass B's scope-guard.
Steps 1-3 = pass A; 4-5 = pass B.

### Pass B landed (2026-07-23)

Steps 4-5 + the §4.4 citation join are built, tested, and committed:

- `crates/server/src/report_export.rs` (~700 lines incl. tests): `AuditReportJson` + pure
  `build_report_json` (partitions on `DispositionWire`/`Finding.status`, joins `rule_id` →
  `camerata_rules::RuleSet` for citations, derives what's-healthy from `audited_rule_ids`
  minus rule ids with any non-FP finding) + async `compile_pdf` (temp dir, `include_str!`-
  embedded template, 30s-timeout `typst compile`, fail-soft "Install Typst: brew install
  typst" when the binary isn't on PATH). `finding_key` is a byte-for-byte mirror of
  `camerata_ui_core::triage::finding_key`'s wire format (server doesn't depend on
  ui-core), pinned by a round-trip test.
- `crates/server/templates/audit_report.typ`: one Typst 0.15 file, all 9 §4 sections,
  Typst-native tables/grids + default fonts only. Validated by directly compiling it
  (`typst compile`) against hand-built full and empty-edge-case `data.json` fixtures before
  wiring it into `compile_pdf` via `include_str!`.
- Route: `POST /api/projects/:id/audit-report` (`lib.rs`, next to `export_deep_report`) —
  404 on no project / no `last_scan`; loads the corpus best-effort (same fallback as
  `split_scannable_rules`); responds `application/pdf` with
  `Content-Disposition: attachment; filename="camerata-audit-{repo}-{shortsha}.pdf"`.
- UI: `AuditReportExportPanel` (`crates/ui/src/cockpit/scan.rs`, right after the triage
  Process step) — client-name/project-title/prepared-by/exec-summary-override fields (all
  optional) + one button. POSTs `dispositions.read()` verbatim (`Disposition`'s derived
  `Serialize` already matches `DispositionWire`'s wire shape) and saves the returned bytes
  via a new `save_bytes` (byte-based sibling of `save_csv`).
- DEP-AUDIT-1 findings are carved out of the scorecard/matrix/curated-findings sections
  entirely and shown ONLY in §7 (dependency snapshot) — mixing "fix this SQL-concat bug"
  and "bump this package version" into one table would blur two different remediation
  types; they also aren't part of `audited_rule_ids` (dep-audit is a separate always-on
  pass, not an architect-selected content rule).
- Tests: 20 new `report_export` unit tests (FP exclusion/counting, Ignored→accepted,
  TechDebt Now/Later→do-now/plan, suppressed-baseline reconciliation, citation join
  incl. a real corpus rule id, what's-healthy derivation, dependency carve-out, exec-
  summary override) + 1 real end-to-end `compile_pdf` test gated on `typst` being on PATH
  (skips with a stderr note, never hard-fails, when absent). `cargo check --workspace` and
  `cargo test -p camerata-server -p camerata-ui` are green (1160 + 580 passing).

### Pass A landed (2026-07-23)

Steps 1-3 are built, tested, and committed. Notes for pass B (the PDF serializer):

- `ScanProvenance`/`AuditedRef` live in `crates/server/src/onboard.rs` (next to `Finding`/
  `ScanReport`). `ScanReport::provenance` is populated inside `audit_repos` (onboard.rs) —
  git ref capture is a new private `capture_audited_ref` helper there (async, `tokio::process::
  Command`, fail-soft), NOT added to `resolve_local_sources`/`greenfield.rs`/`workspace.rs` as
  the doc's phrasing suggested; `audit_repos` already loops over every source dir, so capturing
  there (once, per run) avoids a second traversal. `osv_scanner_version` is stamped as the
  PINNED `tool_provisioning::OSV_SCANNER_VERSION` (dep-audit itself runs AFTER `audit_repos`
  returns, in `lib.rs`, so the actual invocation's version isn't observable at stamp time).
- `TriageState::FalsePositive` + `TriageModel::mark_false_positive` are in `crates/ui-core/src/
  triage.rs`; the UI wiring (4th tab, reason textarea, Process no-op) is in `crates/ui/src/
  cockpit/scan.rs`. The Process handler's per-finding match has an explicit `TriageState::
  FalsePositive => {}` arm — pass B's serializer should partition on the SAME `Disposition.state`
  (excluded + counted, never baseline/ticket) rather than re-deriving the semantics.
- `Finding.confidence`/`Finding.effort` are set in `apply_verdicts` and threaded through
  `consensus_verdicts` (thorough mode) in `crates/server/src/ai_audit.rs`. Effort's tie-break is
  "medium" (neutral), unlike severity's "always break to the lower/humbler value" — see the
  doc comment on `consensus_verdicts`. The `[needs review: reason]` detail-string tag is still
  emitted (one-release UI back-compat); pass B's serializer should prefer the structured
  `confidence`/`needs_review` fields and only fall back to `split_needs_review` for very old
  persisted reports that predate this stamp.
- Both `Finding` and `ScanReport` are mirrored on the UI side (`FindingView` in `ui-core`,
  `ScanReportView`/`ScanProvenanceView`/`AuditedRefView` in `crates/ui/src/cockpit/scan.rs`) —
  serde silently drops unmirrored fields, so any FUTURE server-side field for the PDF export
  must get a matching mirror field too, or the cockpit will silently stop seeing it. Each side
  has round-trip / JSON-shape tests pinning the wire contract (see
  `scan_provenance_round_trip_including_dirty_flag` in onboard.rs and
  `scan_report_view_mirrors_server_provenance_shape` in scan.rs).

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
