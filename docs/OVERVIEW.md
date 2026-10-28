# Camerata: system overview (the human map)

> **Audience & purpose.** The codebase is too large to read end to end; its current size is in
> the README's "By the numbers" table, so this map carries no line or crate counts of its own.
> This is the HIGH-LEVEL map a human reads to understand the whole system in one sitting: what
> the parts are, how they fit and the handful of concepts everything else is built on. For
> code-level depth see `TECHNICAL.md`; for the layering/runtime view see `ARCHITECTURE.md`; for
> the rule schema see `RULE_AUTHORING.md`. **Keep this doc current - every work pass updates it.**
> Last refreshed: 2026-10-21.

## What Camerata is

A single all-Rust workspace that does two things, sharing one governance core:

1. **Brownfield Codebase Inspection**: point it at a repo; it produces a client-facing audit:
   a narrative PDF, a full workbook and machine-readable data. This is the product most of the
   recent work has hardened.
2. **Governed Development**: orchestrate AI coding agents through a lifecycle with a
   deny-before-execute gate that blocks a forbidden write before it touches disk (binary
   pass/fail, not an LLM grading an LLM).

Both rest on the **rule corpus** (441 rules) and **deterministic gates**.

## The crate map, grouped by role

```
CONTRACTS / CORE
  api-types     the pure serde wire contract (FindingKind, Ownership, KindOnWire, …) under everything
  core          shared primitives (RuleId, …)
  secrets       leaf crate: the ONE secret-shape detector + redactor (`prefix…tail (N chars)`), shared by
                the gateway SEC arms, the scan floor, the chat scrubber and the export release gate
  app-core      framework-agnostic application core (projects, readiness)

THE CORPUS + ITS ENFORCEMENT
  rules         the 441-rule corpus loader + schema + corpus gates + the CORPUS_FRESHNESS watermark
                + per-rule `version` and the content-hash lock (CORPUS_LOCK.toml)
  checks        the deterministic CHECKERS: arch/import/layering/RLS/weak-rand/one-import-path/
                dead-code/react-hooks-lint (does the project's OWN eslint config enable the hooks
                rules; reads config, runs no eslint)/… + the AST extractors (extract::ecma|python|rust_syn incl. `mod_decls`
                and `computed_loads`, resolver, shared `paths` and the literal index behind the
                cross-file ARCH-SHARED-CONSTANT-1 check, and the shared token clone detector
                `extract::clone` with the Wave C candidate producer `clone_rules`) + integration gate
  linter-registry  stack→linter mapping
  camerata-check   thin check CLI

THE INSPECTION ENGINE
  server        the largest crate: the scan pipeline, calibration, classification, the
                report/xlsx/csv/json/manifest exporters, the integrity gate, dep-audit (osv),
                ownership derivation, the BFF HTTP API
  preview       preview-scan helpers

THE AGENT / GOVERNANCE SIDE
  gateway       the deny-before-execute governance gate (fail-closed, provider-neutral)
  llm           the LlmPort provider seam (Anthropic/OpenRouter/…)
  agent, fleet, orchestrator   agent drivers + governed multi-agent fleets
  intake, worktracker          PO-mode intake + tracker (GitHub Projects) integration
  maintenance, liveness, mcp, deploy, scaffold, client   standing ops, health, MCP, BYO-infra, HTTP client

THE UI + CLI
  ui, ui-core   the Dioxus "Enterprise Cockpit" desktop app + its framework-agnostic core
  cli           `camerata` - headless inspect/export, demos, `corpus check/confirm/lock`, integrity-gate,
                release-gate (the export secret/consistency gate over any product zip)
```

## The inspection pipeline (end to end)

```
repo ─▶ ONBOARD/DETECT        stack detection (languages, frameworks) → which corpus domains apply
     ─▶ SCAN TIERS (parallel, each disclosed if it fails)
          • deterministic floor   content-rule + architectural checkers (checks/)      no model
                                  (the architectural tier also gets a separate reference-text
                                  list: HTML, components, docs; never the AI digest or the floor)
          • clone advisory (Wave C) repeated-decision / one-concept / UI-mechanism CANDIDATES
                                  (checks/clone_rules, deterministic) judged one closed question
                                  at a time on the fast model tier (server/onboard/clone_advisory);
                                  only an affirmative verdict ships, no model = "not assessed"   fast model
          • external-tool preview  eslint / ruff / semgrep / clippy (SARIF/JSON)        no model
                                  (React Hooks: the violations come from here, under the two
                                  JAVASCRIPT-REACT-* rules; whether the repo's own lint config
                                  turns the rules on is the checker UI-REACT-HOOKS-LINT-1)
          • dependency audit       osv-scanner over lockfiles → CVEs (+ fixed version)  no model
          • semantic AI audit      the LLM pass over the repo, grounded in the corpus    model
          • integration gate       cross-component seam reconciliation (assembled tree)  no model
     ─▶ CALIBRATE + MERGE        severity anchors; same-root-cause dedup
     ─▶ CLASSIFY                 kind · severity · disposition · OWNERSHIP (who can fix it)
     ─▶ TRIAGE                   curator accepts / waives / marks false-positive
     ─▶ INTEGRITY GATE           pre-export: input invariants must hold (see below)
     ─▶ PRODUCT PACKAGE (the deliverable ZIP)
          {stem}.pdf             narrative for a decision-maker (non-technical reader)
          {stem}-findings.xlsx   full workbook: All Findings, Deviations, Conventions,
                                 Dependencies, False Positives, Coverage (+ per-category)
          findings.json          machine-readable All-Findings + provenance/summary
          csv/ , json/           every data sheet as a flat table (one file per sheet)
          manifest.json          index + per-file sha256 + schema versions + provenance
          README.txt             the human manifest
```

## The handful of concepts everything is built on

Each FINDING carries a set of ORTHOGONAL axes - learn these and the rest follows:

| Axis | Values | Answers |
|------|--------|---------|
| **enforcement** | prose / structured / mechanical / architectural | how the rule is enforced (emit vs runnable check vs AST pass) |
| **kind** | defect / deviation / convention_observed / no_convention / not_assessed | what the finding IS |
| **severity** | info / low / medium / high / critical (+ anchors) | how bad - anchors keep e.g. test-coverage below critical, floor-inject SEC/ARCH |
| **disposition** | open / accepted / false-positive / held-for-review | the triage call on it this run |
| **ownership** (W33) | client_owned / platform_managed / external_dependency / generated / vendored | WHO can fix it - non-client-owned routes out of the action tiers + budget |
| **inspectable** | true / false | can a working-tree scan assess it? `false` = emit-only (a convention, never scanned) |

And two kinds of GATE guard quality:

- **The integrity gate** (`server/src/integrity_gate.rs`, runnable as `camerata integrity-gate`):
  pre-export INPUT invariants - no engine self-ingestion, files actually scanned, the audit
  model recorded, repo-relative paths. Run it BEFORE spending a graded sweep.
- **The corpus gates** (`crates/rules/tests/w*.rs`): ESLint-flat-config-style structural
  enforcement ON THE CORPUS ITSELF - schema, sources, client-facing voice, no vendor name, no
  high+ finding without a fix+citation, no truncated option ids, CWE on security rules, etc.
  These are why the 441 rules stay consistent as they change.

## The rule corpus in one paragraph

441 TOML rules under `crates/rules/principles/<domain>/<id>.toml`. Each declares its
enforcement tier, kind, severity bounds, `inspectable`, authoritative `[[sources]]` and one or
more `[[option]]`s (the directive emitted to the client's `AGENTS.md`/`CONVENTIONS.md`, a
client-facing remediation, and finding text). Rules are selected per detected stack by DOMAIN.
Every rule carries a `version` (an integer, bumped on any change) that is enforced, not trusted:
`CORPUS_LOCK.toml` records each rule's version plus a content hash of the whole rule, and a
build gate fails a rule that changed without a bump (`camerata corpus lock` regenerates it and
refuses a missed bump). The same hash is the baseline the applied-rule drift detector compares
against. Freshness is tracked per domain in `CORPUS_FRESHNESS.toml` (advanced only by a
human-confirmed staleness sweep); a rule may also carry its own `last_reviewed` date. A domain
approaching its window shows as "due soon" (`camerata corpus check`, one startup log line, and
the cockpit banner), strictly operator-only and never in a client artifact. Authoring standard:
`RULE_AUTHORING.md`.

## Where to go deeper
- **Code-level HOW:** `TECHNICAL.md` (the full reference).
- **Layering / runtime stack:** `ARCHITECTURE.md`.
- **Rule schema + authoring:** `RULE_AUTHORING.md`, `RULES_REFERENCE.md`.
- **In-flight design:** `docs/design/` (e.g. the machine-readable exports, the ownership/unfixable
  class, the corpus staleness check) and `docs/plans/` (the remediation sequence, W29).
- **What happened recently / decisions:** `docs/OVERNIGHT_DECISIONS.md`, `docs/decisions/`.
