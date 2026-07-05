# Camerata audit AUTO-fixes (2026-07-04 Fable 5 audit)

Autonomous overnight routine. Fixed AUTO-classified findings on per-area local
branches off origin/main. No pushes, no PRs (the wrapper does that). All ZACH
items skipped.

Note: the routine could not write to `~/.claude/inbox/audit-autofix-summary.md`
(that path is permission-guarded in the autonomous context), so this summary
lives in the repo on `fix/audit-docs` instead. The completion sentinel
`~/Library/Logs/claude/audit-autofix.done` was written successfully.

## Landed (5 areas, local branches ready to push)

| Branch | Findings | Verify | Head commit |
|---|---|---|---|
| fix/audit-ui-contract | cockpit-F1/F4/F5/F6, UI-1..14 | camerata-ui green | 67ce56c |
| fix/audit-workspace | workspace-F1..F8 | 543 camerata-ui tests green | 445c0bc |
| fix/audit-design | ROUTES-3 / UI-12 (Add-child double node) | 542 camerata-ui tests green | 15c3c7c |
| fix/audit-server-contract | ROUTES-2, ROUTES-6 | worktracker + camerata-ui green | b21a1a7 |
| fix/audit-docs | GAP-9, GAP-5 (doc) | docs only | (this branch) |

Each branch has a code commit plus a tracker-update commit. fix/audit-server-contract
also stamps the worktracker RevisionProvenance Default so the manual Add-decision
422 is fixed on both the UI and server sides.

## Deferred to you

Server-contract partials (on fix/audit-server-contract, tracker row):
- ROUTES-7 (AppError all-500): per-handler 4xx status-code judgment across ~40 sites.
- ROUTES-8 (side-effectful GETs): per-handler read-vs-write classification of ~25
  get_or_create sites plus a new non-creating store getter.
- ROUTES-5 (arbitrary deep report): needs a JobState data-model change (completion
  timestamp + project id).
- ROUTES-9 (runtime set_var in handlers): concurrency / env semantics.

Two whole areas not started, deferred for budget (both crates/server, expensive
compiles, judgment-sensitive):
- Lifecycle minor: LIFECYCLE-11, LIFECYCLE-13 (safe subset). Server runtime edits
  need a green camerata-server build to verify; remaining budget could not cover it.
- GitHub token + publish + emit: ROUTES-1/PUBLISH-1..8, GATE-F6. Keychain/env split
  and publish fail-soft semantics are risk-bearing; wanted a verified build.

## Notes
- All ZACH items (GATE-F1..F7, LIFECYCLE-1..10/12, GAP-1..8) untouched by design.
- I intentionally did NOT start any server edit I could not verify with a green
  build, per the routine protocol (keep the build green; revert if not).
- Budget ran to roughly 4.6 of 30 USD remaining at wrap-up.
