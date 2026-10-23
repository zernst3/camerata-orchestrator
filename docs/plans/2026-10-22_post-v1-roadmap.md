# Post-V1 roadmap (recorded 2026-10-22)

**V1** = the inspection: the first paid client deliverable, gated by the graded accuracy tests.
Its scope is `docs/plans/2026-10-19_MASTER-SEQUENCE.md` plus the two client-trust blockers (secret
redaction with the export release gate; delete-and-re-fetch client source). Nothing below is V1.

After V1, Camerata keeps developing toward the rest of what Cantus Works offers. Zach's read
(80/20): the pieces below are roughly 80% there; the remaining 20% is fleshed out after V1.

## 1. Governance: inspection history and the monthly delta retest

The governance offering includes a monthly retest of what changed. It needs real UI/UX work.

- **Inspection history.** The original inspection and every monthly inspection are saved and
  stay accessible in the project: each with its commit, date, findings, ledger, and the exact
  client package that was delivered. Browsable as a timeline; any past package can be reopened
  or re-downloaded as delivered.
- **Delta retest.** "Rescan changes" scans what changed between the last inspection's commit and
  the current one (the same mechanism the re-fetch flow exposes). The re-inspection reports the
  findings in the deltas, period. Tracking what was fixed or is still open is the CLIENT's
  responsibility in V2 (Zach, 2026-10-22: no capacity to own that for multiple clients).
  A fix-tracking feature is a possible V3 item.
- **Full re-inspection.** An explicit option to run the complete inspection again instead of the
  delta (scope or corpus changed, client asks, or a periodic full pass).
- Builds on what exists: scan manifests and incremental scan cache, finding fingerprints, the
  rule-version lock and drift detection, the product package and its manifest.

## 2. Remediation and general development: the development console

The development console works well today but is tightly coupled to GitHub Issues. Client
contracts will usually NOT include access to the client's board, so it must be board-agnostic.

- Work items can originate inside Camerata (from findings, from a free-text request, from a
  remediation plan) with no external board at all; an external board becomes an optional sync
  target, not a dependency.
- Keep the existing GitHub Issues path as one adapter among others, not the backbone.
- Refine the console's remaining rough edges (to be listed when this phase starts).
- **The crux (Zach, 2026-10-22):** rules are enforced DURING development, with checks along the
  way, following the same strict guardrails the inspection looks for, down to the details. Most of
  this exists (Layer 1 MCP deny-before-write gate, Layer 2 post-task checks, Layer 4 CI).

## 3. Dev-time enforcement parity with the inspection (Layer 1 / Layer 2)

The inspection's deterministic coverage has outgrown Layer 1. The gate has 11 regex/path arms
(the 7-rule security floor plus structural guards); the inspection now also runs AST checkers
(Supabase migrations, weak randomness, query-grammar injection, the W29 detectors) and the tool
tier (eslint). `TECHNICAL.md`'s matrix says AST rules are "No (needs AST)" at Layer 1, but
single-file tree-sitter/syn extractors now exist.

- **Parity audit first:** a matrix of every deterministic rule the inspection runs against its
  dev-time enforcement point (L1 per write, L2 per task worktree, L4 CI); every gap listed.
- **One implementation per rule:** a rule decidable from one file runs at Layer 1 through the
  SAME code the inspection uses (no second regex copy). A whole-repo rule (one-import-path,
  shared-constant) runs at Layer 2. Dead-code stays inspection-only (decision 2026-10-21).
- **Only selected rules run:** the Layer-1 rule subset and Layer-2 checker set derive from the
  project's selected rules (no React lint on a non-React repo).
- **Check first, possible defect:** `TECHNICAL.md` §2 says the verified runtime default subset is
  `["GOV-1"]` "unless configured". If a dev run can reach the gate without the project's selected
  rules (including the security floor) in its subset, that is a defect to fix, not new scope.
  Verify this first; if confirmed, it may belong before V2.

## Keep V1 forward-compatible (no extra V1 work)

When V1 work touches the project data model (the deletion record, re-fetch, provenance), store
per-inspection facts (commit, date, package) in a shape that can become one entry of the
inspection history above, rather than a single "last scan" slot. Not a V1 feature; just do not
paint the data model into a corner.
