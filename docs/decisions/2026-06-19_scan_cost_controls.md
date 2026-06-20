# Scan cost controls: incremental scan, rule-routing, paced partial scan

Date: 2026-06-19
Status: incremental scan SHIPPED; rule-routing core SHIPPED + wiring ROUTE-1 (pending review);
paced partial scan DESIGNED (scaffold pending).

## Problem

The AI audit re-reads the codebase every run, and on a large repo that is the bulk of the token
bill. Measured cold full scans (Haiku scan + Sonnet 4.6 calibration), through the actual
`estimate_audit_cost` model over the real file sets:

| Repo | files / chars | full parallel | + thorough | total tokens |
|---|---|---|---|---|
| linkding (Django) | 279 / 1.37M | $3.00 | $5.37 | ~1.7–1.9M |
| umami (Next.js) | 1,052 / 4.0M | $8.74 | $15.72 | ~4.9–5.6M |
| **Rivet** (Rust+TS monorepo) | 4,372 / 21M | $46 | $81–108 | **26–40M** |

The dominant term is `code_tokens × rule_batches`: every rule-batch re-sends the whole codebase.
Three independent levers attack that — implemented here.

## Lever 1 — Incremental scan (SHIPPED)

Skip the AI audit on files that haven't changed since the last scan; carry forward their cached
findings. The deterministic security floor (`audit_files`) is token-free and always runs over the
whole tree, so the floor never goes stale; only the AI pass is short-circuited.

- `crates/server/src/scan_cache.rs`: a per-project **scan manifest** — a byte-exact FNV-1a
  fingerprint per audited file (byte-exact, NOT whitespace-normalized: a cached finding carries a
  line number, and a reformat shifts lines, so "unchanged" must mean byte-identical) + the AI
  findings from the last scan. `partition()` splits a repo's files into changed vs unchanged and
  carries forward findings for unchanged, still-present files; deleted files' findings fall away.
- **Rule-set invalidation:** the manifest stores a fingerprint of the rule SELECTION (order-
  independent over ids + repo bindings). A prior manifest is reused only when its rule fingerprint
  matches the current scan, so changing the rule selection auto-invalidates the cache (carried
  findings always reflect the current rules). A manifest-version bump invalidates too.
- The AI audit of the changed set still receives the WHOLE repo as repo-MAP context (cheap symbol
  list, via `audit_repo`'s new `map_files` override) so cross-file rules keep their architectural
  view even when only changed bodies are sent.
- Wired in `audit_repos` (returns `(ScanReport, ScanManifest)`); both audit handlers load the prior
  manifest and persist the fresh one per active project (`ScanCacheStore`, `scan-cache.json`).
- UI: a **"Full scan (ignore incremental cache)"** checkbox (off by default → re-scans are
  incremental); sends `incremental=false` to force a clean pass. The first scan of a project is
  always full (no cache yet).

Effect: after the first scan, a re-run costs AI only for changed files. Editing a handful of files
in Rivet drops a re-scan from ~26M tokens to a few hundred K.

## Lever 2 — Rule-routing (CORE SHIPPED; wiring ROUTE-1)

Send each rule only the files it could apply to. A `RUST-*` rule can't be violated by a `.ts` file;
routing it to `.rs` files only is the big lever on a **polyglot** repo (e.g. Rivet's Rust backend +
TS frontend: Rust rules skip ~7.7M chars of TS/JSON, TS rules skip ~8.4M of Rust).

- `crates/server/src/scan_routing.rs` (PURE core, 7 tests): conservative classification — a rule is
  routed to a language ONLY when its id carries a recognized single-language prefix (`RUST-`, `PY-`,
  `REACT-`, `GO-`, …). Cross-cutting families (`ARCH-`, `SEC-`, `SQL-`, `DB-`, `API-`, `PROC-`,
  unknown) are `Scope::All` and audit every file. **Routing must never cause a missed finding**, so
  when in doubt a rule audits everything. `plan_routes()` groups rules by scope and estimates the
  saved input fraction.

### Why the wiring is ROUTE-1 (not auto-applied tonight)

Wiring routing into `audit_repo` interacts with the **advisory pass**. `run_passes` gates the "flag
novel issues beyond the adopted rules" task to the FIRST rule-batch of each file chunk, precisely so
one `.expect()` isn't re-flagged under N invented names. Routing groups rules by scope and would run
the engine per group, so a `.rs` file would appear in BOTH the `rust` group and the `All` group and
get advisory'd twice — re-introducing the duplicate-novel-finding problem.

The safe wiring (to land with review): run the advisory novel-issue pass exactly ONCE per file
chunk across the whole scan (e.g. only in the `All` group, or a dedicated advisory pass over the
full tree), and run each language group's batches with advisory disabled. This is a real change to
the core scan loop and to finding aggregation, so it lands reviewed, not unattended. The pure core
+ savings estimate are shipped so the UI can SHOW the potential saving before the wiring lands.

## Lever 3 — Paced partial scan (DESIGNED)

Doesn't reduce total tokens; spreads them under a usage cap (e.g. a Max-plan 5-hour window). The
background-job infra (`crates/server/src/jobs.rs`: `JobStore`/`JobState` with done/total + live
findings) already runs the audit detached. Paced scanning adds: checkpoint the per-(chunk×batch)
pass progress to the job (and/or the manifest), a "scan next N passes / pause" control, and resume
from the last checkpoint. Because incremental already shrinks the work-list to changed files, pacing
is the fallback for the rare case where even the changed-file scan exceeds one window. Scaffold +
the checkpoint shape are the next step.

## Cross-cutting

- All three compose: incremental shrinks the file list, routing shrinks each rule's file list,
  pacing spreads whatever remains. Incremental + routing shrink the bill; pacing fits the window.
- The cost estimate stays a conservative full-scan ceiling for now (it doesn't yet subtract the
  incremental/routing savings) — over-quoting is the safe error. A follow-up can feed `RoutePlan`
  and the changed-file set into `estimate_audit_cost` for a tighter quote.
