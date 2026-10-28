# Overnight run plan, 2026-10-27 night to 2026-10-28 09:00

Zach is away about 10.5 hours with standing approval to decide, commit and push (see
`docs/OVERNIGHT_DECISIONS.md`). Opus orchestrates; a recurring check every 15 minutes wakes the
session even when nothing reports. Every decision goes into `docs/OVERNIGHT_DECISIONS.md`.

## Hard rules for the night

- Every merge: Fable review first (read-only), fixes by a Sonnet builder, then merge into
  `feat/audit-report-export`, full gate (fmt, workspace clippy `-D warnings`, `cargo test
  --workspace --locked`), push only when green. One gate at a time.
- At most 4 compile-heavy local builders at once. Keep at least 60 GB free: delete the build
  folder of any idle builder worktree; remove merged worktrees right away.
- New cloud sessions cannot be started overnight (the app only starts them from a message Zach
  types). Cloud branches that finish are reviewed and merged locally.
- The API test run on the credits does NOT start overnight: the release binary needs a macOS
  Keychain approval click each time it is rebuilt. It is prepared for 09:00 instead.
- Never push or merge `main` overnight; the end-of-session PR into `main` waits for the morning.
- A failing test is fixed or explained, never retried until it passes.

## Work queue, in order

A. In flight on the Mac: rank 7 review fixes, rank 4 review fixes, rank 10 merge + fixes,
   KEV/EPSS merge + fixes, item 21 + rank 2 merge + fixes, GitHub CI fix (branch
   `fix/github-ci`). Each merges as soon as its report is checked.
B. Cloud sessions still running: `cloud/w27-pdf`, `cloud/w29-wave-d`, `cloud/rank8`,
   `cloud/corpus-text`, `cloud/console-gaps`. A branch counts as finished when it has commits
   ahead of the working branch and no new commit for 20 minutes. Then: Fable review, local
   merge + fix builder, gate.
C. When ranks 2, 4, 7, 8 and 10 are all merged: one Fable integration review of all efficiency
   switches together (interactions, OFF paths, cap accounting); fix what it finds. Then write
   the morning sweep runbook (below) and build the release binary last thing before 09:00.
D. After ranks 4 and 7 merge: viewer 7c (calibration sets `mapped_rule_id` / `model_rule_id`
   on advisory findings; the Queue groups by it).
E. After the PDF rebuild merges: viewer 7b (reviewer notes into xlsx / JSON / PDF), and
   re-point walkthrough steps 3 (client name) and 6 (spend limit) at the real controls.
F. When Wave D merges: the README count becomes 440 (the count test forces it).
G. Spare capacity: Fable read-only false-negative hunt across the inspection pipeline (every
   path where a real finding can be dropped, downgraded, merged away or hidden), fix the HIGHs.
H. By 08:30: morning summary at the top of `docs/OVERNIGHT_DECISIONS.md`; the PR from the
   working branch into `main` drafted but not merged.

## Morning sweep runbook (for 09:00 with Zach)

1. Zach approves the Keychain prompt for the fresh release binary ("Always Allow").
2. Smoke: one fixture (supabase-portal) real-time, `--max-cost-usd 10`.
3. Five fixtures real-time with all switches on, then batch; each with a cap; total under $95.
4. Grade with the existing grader; per switch, compare against sweep5b; isolate any recall
   loss by turning that one switch off and rerunning the affected fixture.
5. Record results in the spike results doc; switches that lose recall default to OFF.

## Status log (newest first)

- 23:05: Wave D (cloud) reviewed: 2 HIGH false-positive shapes (Rust method-call helpers incl.
  trybuild; React setters escaping via object shorthand), 3 MEDIUM, 4 LOW. Decision: render-only
  and compile-only tests are smoke tests, not flagged. Merge+fix brief ready
  (scratchpad/w29_merge_brief.md); starts when a local builder slot frees (5 busy).
- 22:50: GitHub CI green for the first time on `fix/github-ci` (run 40621036960, all four
  gates, about 17 min); in Fable review before merge. Queued follow-ups it found:
  `derive_shared_target_dir` returns `/.camerata-shared-target` instead of None
  (crates/checks/src/lib.rs ~325); repo scanners return files in filesystem order, so the audit
  digest is not deterministic across filesystems; Rust 1.99 clippy/rustc findings
  (`double_must_use` on async_trait traits in crates/core, an unused glob import in
  crates/ui/src/cockpit/pane_context.rs) block un-pinning the CI toolchain from 1.95.0.
- 22:45: cost-cap metering fixes gated green (8,827 passed, 0 failed) and pushed; this also
  confirms the docs commits pushed early with them.
- 2026-10-27 night: plan written; recurring check scheduled; Mac kept awake.
