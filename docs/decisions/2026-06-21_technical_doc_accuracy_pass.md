# 2026-06-21 — TECHNICAL.md accuracy pass

A comprehensive, section-by-section accuracy pass on `docs/TECHNICAL.md`. The doc
is both the precise architecture reference and the in-app chatbot's grounding
(`crates/ui/src/chat.rs` `include_str!`s it), so every claim must match the
SHIPPED code. Each claim below was verified against the cited source before being
trusted or changed. Scope was confined to `docs/TECHNICAL.md` plus this decision
doc; no code or other docs were touched.

## Changes made

### §1 — System overview and crate map
- Crate count corrected: "14 crates" → 15 crates under `crates/` (verified
  against `Cargo.toml` `members`), plus a note that `tools/corpus-verifier` is a
  separate, maintainer-only workspace member (not a `crates/` member, never an app
  dependency).
- Added the missing `camerata-linter-registry` crate to the table (citation
  validator for grounding mechanical rules).

### §2 — Layer-1 MCP gate
- The six hardwired `RULE_REGISTRY` rules verified unchanged against
  `crates/gateway/src/lib.rs` (GOV-1, SEC-NO-HARDCODED-SECRETS-1,
  SEC-NO-RAW-SQL-CONCAT-1, ARCH-NO-SECRETS-IN-URL-1, SEC-NO-PATH-ESCAPE-1,
  SEC-NO-SECRET-FILES-1) — no change needed.
- Latency paragraph rewritten: the stale "sub-3 ms over a 71-rule subset
  (~161 µs / ~2 ms / ~8.5 s)" microbenchmark figures are not reproducible from
  code, so they were replaced with the durable relative claim (pure in-memory
  pass, dominated by a few pre-compiled regexes; model round-trip is orders of
  magnitude larger). See FLAGS below.

### §3 — Layer-2 post-task checks (MAJOR REWRITE)
- Was "implements CheckRunner for Rust worktrees. Three concrete runners." Now
  describes the cross-language / polyglot / repo-pinned / fail-closed runner,
  consistent with §5a. Verified against `crates/checks/src/multilang.rs`
  (`JsCheckRunner` / `PythonCheckRunner` / `GoCheckRunner`, `detect_languages`,
  `PolyglotCheckRunner`, `runner_for_worktree`, `NoopChecks`) and the fleet wiring
  at `crates/fleet/src/lib.rs` (`use camerata_checks::runner_for_worktree;`).
- Documented: repo-lockfile-pinned toolchain (so layer-2 == repo CI), recursive
  every-language detection with pruning, per-subtree sub-runners, union of
  violations, and the three fail-closed axes (toolchain missing / no check defined
  / install failure) plus the single `NoopChecks` pass-through for unknown trees.
- The Rust runner (`FmtCheckRunner`/`ClippyCheckRunner`/`TestCheckRunner` →
  RUST-FMT/RUST-CLIPPY/RUST-TEST) retained and verified against
  `crates/checks/src/lib.rs`.
- Gate-probe + VCS-action paragraphs left intact (verified: `gate_probe.rs`
  exists, `/api/gate-probe` route + CLI `gate-probe` exist, `vcs_action.rs`
  exists).

### §5 — Rule corpus
- `EnforcementKind` updated from 3 variants to 4: added `Architectural` (verified
  in `crates/rules/src/lib.rs`); fixed the inline comments for Structured /
  Mechanical to match the §5a model.
- Emit-partitioning line updated: `structured | mechanical | architectural` →
  `CONVENTIONS.md`.
- Stale "subset currently contains 71 rules" softened to the selection RULE
  (universal + domain-matched + overrides; dozens for a multi-domain role; count
  tracks corpus size and is not asserted in code). Verified `domain_to_glob`
  sub-domain handling.

### §5a — Rule type model
- Verified the corpus counts exactly match the live corpus: prose 84,
  structured 190, mechanical 57, architectural 9. The doc already says "describe
  kinds, not numbers"; the ~ figures match, left as-is.
- Verified EVERY mechanical rule cites a linter (scan of
  `crates/rules/principles/`: zero uncited mechanical rules).
- Verified the gate's "5 of 6 not in corpus" fact: only ARCH-NO-SECRETS-IN-URL-1
  is also a corpus rule (the other five are gate-internal). Verified the
  `["GOV-1"]` default subset against `crates/gateway/src/main.rs`.
- Added the 4th `verification` value `needs_recheck` (verified `Verification`
  enum in `crates/rules/src/lib.rs`) to the ladder table.
- Tightened the arm.rs / onboard.rs render-routing line-number citations to
  reflect the current file (arm.rs module-doc ~8–9 + partition ~136–160;
  onboard.rs ~102–103).

### §7 — Persistence
- Updated the `uow.json` row to the current UoW shape (stage, gate_provenance,
  sign_off, evidence, decisions read-cache).
- Added a "Feature flags" subsection: opt-out model (default true), `soc2` is the
  one shipped flag, and Camerata **ships SOC-2 OFF** via `.camerata/features.toml`
  `soc2 = false` (verified the file contents and `feature_flags.rs`). Explicitly
  states nothing treats SOC-2 as on-by-default in the shipped build.

### §10 — Unit of Work (RETITLED + MAJOR REWRITE)
- Retitled "Issue Management → WorkItem → Unit of Work" to match the rebuilt
  Governed Development surface.
- Documented the WorkItem DTO (verified `crates/server/src/workitems.rs`) and the
  pure mapping/bridge functions (`from_github_issue`, `from_canonical_story`,
  `work_item_id_to_story_id`/`story_id_for`).
- Documented the endpoints (verified registered in `crates/server/src/lib.rs`,
  NOT in workitems.rs — corrected vs. the task brief): `POST /api/workitems/pull`
  (all-repos, manual, no cache), `/refresh`, `/comment`, `GET /api/uows`,
  `POST /api/uow/from-workitem` (dedup by external ref).
- Corrected the UoW struct to the shipped shape (added `stage`, `decisions`,
  `gate_provenance`, `sign_off`, `evidence`; documented `UowStage` lifecycle and
  the `lifecycle.rs` state machine, the ArtifactStore-backed decision history, and
  the human-only sign-off gate). Verified against `crates/server/src/uow.rs` +
  `crates/server/src/lifecycle.rs`.
- adopt-issue: corrected from "RETIRED" to "superseded as the surface flow." The
  `/api/stories/adopt-issue` ROUTE + handler still exist in `lib.rs` (a token-free
  idempotent spine-upsert primitive); it is just no longer the UI's adoption path.
- Noted the WorkItem ↔ CanonicalStory rename is deferred/cosmetic; documented the
  model as WorkItem per the brief while keeping the code-type name honest.

### §11 — Cockpit UI
- Views/navigation rewritten to the shipped nav: **Onboard repos · Governed
  Development · Rules · Routines · Repository Workspace · Docs** (verified
  `CockpitView` enum + `CockpitNav` in `crates/ui/src/cockpit.rs`). Corrected the
  labels (`Stories` → "Governed Development", `Workspace` → "Repository
  Workspace") and added the missing `Docs` view.
- Added a "Governed Development page" subsection (verified `GovernedDevPage`,
  `GovDevSel`, `IssueManagementPanel`, `WorkItemTable`/`WorkItemDetail`,
  `UowDevControls`).

### §12 — Worktracker (RETITLED + corrected)
- Corrected the `CanonicalStory` struct to the actual fields (verified
  `crates/worktracker/src/lib.rs`): `description: String` (not `Option`),
  `created_by`, `targets` (not `repo_targets`); removed the fabricated
  `// ... etc.`
- Fixed the trait confusion: the provider adapters implement **WorkItemProvider**
  (the per-item sync port), NOT `StoryStore`. `StoryStore` is implemented only by
  `InMemoryStoryStore`. Listed `GithubProvider`, `GithubProjectsSource`,
  `JiraProvider`, `AdoProvider`, `NativeProvider` correctly; noted GitHub Issues
  is the only adapter wired into the shipped UX, the rest FUTURE.
- Sync policy (`apply_inbound`, `ExpectedEchoTable`) and `ClarifyBridge`
  paragraphs verified against `sync.rs` / `clarify_bridge.rs` — no change needed.

## Unresolved flags (claims NOT verifiable from code)

1. **Gate latency microbenchmarks (§2).** The original "~161 µs / ~2 ms / ~8.5 s /
   sub-3 ms over a 71-rule subset" numbers have no source in the repo (no bench
   asserting them). Replaced with a qualitative, code-grounded statement rather
   than inventing or retaining unverifiable figures. If a benchmark is later added
   to the repo, restore concrete numbers with a citation.
2. **"71 rules" Backend subset (§5).** Not asserted anywhere in code; corpus-size
   dependent and now stale (corpus has grown). Softened to the selection rule.
3. **Corpus counts (§5a).** prose 84 / structured 190 / mechanical 57 /
   architectural 9 verified TODAY by scanning `crates/rules/principles/`. These
   drift; the doc already instructs citing kinds not numbers.

## Confirmation

Every section was checked against the cited source files. After this pass,
`docs/TECHNICAL.md` matches the shipped code, with the three unverifiable
quantitative claims above replaced by code-grounded or kind-level statements
rather than invented numbers.
