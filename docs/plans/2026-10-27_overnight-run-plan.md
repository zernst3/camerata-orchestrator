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
- Zach (23:35): if any run hits something that genuinely needs his sign-off, pause that run,
  move on to the next item, and list it in the morning report under "Needs Zach".

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
I. Repo hardening work order (`_workspace/brownfield-audits/WORK_ORDER_REPO_HARDENING.md`, V1,
   added 23:30). Cloud starts were refused (Remote Control), so it runs on the Mac, slotted into
   free builder slots, in this order: H1 hygiene (after the CI branch merges), H0 corpus phase 0
   (with the corpus-text merge), H2 client lock + execution isolation + DATA_HANDLING, H3
   prompt-injection defense (after rank 4 merges), H5 citations + repeatability, H4 no silent loss
   + engine version + triage log (after the PDF rebuild merges), H6 README/PLATFORM split (after
   Wave D). Briefs: scratchpad `hardening_tasks.md` + `hardening_local_preamble.md`. Waiting on
   Zach may start H1 to H5 in the cloud from his phone (briefs on branch `briefs/hardening`,
   file CLOUD_BRIEFS.md): before starting any H section locally, check `git ls-remote origin
   'cloud/*'` for cloud/hygiene, cloud/client-lock, cloud/injection, cloud/coverage,
   cloud/citations and skip any that exists (it is running in the cloud). Waiting on
   Zach: D1 DECIDED 23:50 (Zach): option (a), a private companion repo stored on GitHub, not only
   locally. Created: github.com/zernst3/camerata-internal (private, verified 404 anonymously). The
   move happens in the morning wrap-up (this plan and the decisions log are in use overnight):
   clone it to ~/Documents/Repos/camerata-internal, copy the working notes in (session logs,
   overnight notes and this plan, TEST_WHEN_BACK.md, staged-issues/, dated plans/ and audits/,
   the decisions log), commit and push there, then remove them from the public tree in one
   commit; design docs, architecture, technical, rules reference, rule authoring, user guide and
   decision records stay public. Formerly: D3 branch protection (his GitHub
   step), the `.claude/settings.json` deny-list move (needs his user settings; the permission
   classifier blocks the agent from editing them), the release tag (with the morning PR).
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

- 00:10 (Oct 28 local 00:10 = 04:10 UTC): false-negative hunt (cloud) delivered: 12 HIGH, 26
  MEDIUM, 22 LOW on base 7bc10573 (before tonight's merges; some are already fixed by rank 2's
  batch truncation retry and H4's malformed-JSON retry). Report saved to
  scratchpad/fn_audit.md (it belongs in camerata-internal, not the public tree). Plan: once the
  integration and cloud branches land, a Fable pass marks which items still hold on the merged
  branch, then Sonnet builders fix the remaining HIGHs by area. Reviews started for hygiene,
  sources and boards.
- 00:20: README split (cloud) reviewed: counts and links verified; 2 HIGH (README claims every
  finding is reviewed before a deliverable, but `camerata inspect` exports untriaged; linked docs
  still say 417 and the count test skips them), 5 MEDIUM. Brief queued
  (scratchpad/readme_merge_brief.md), runs after the integration merge lands (count 440).
- 00:15: rank 10 + CI branch gated green (8,896 passed, 0 failed) and pushed; GitHub CI is
  running on the working branch with the new workflow (run 40629518781). Issue triage (cloud)
  verified: 11 closed, none wrongly; reopened #110 because its author's keep-open condition (a
  live run of the API agent transport) has not been met. Stale-branch list (21 branches,
  nothing deleted) is on `cloud/triage`; it goes to camerata-internal in the morning, not into
  the public tree. Integration merge (five branches) in progress.
- 00:10: corpus text + Phase 0 done in its worktree (190 rules bumped; 415 rules tests green);
  added as a fifth branch to the integration merge. Showcased rules final: log-consistency v3
  (approved em-dash removal changed two clauses vs the site's earlier word-for-word text, so the
  site must refresh from the file), hardcoded-secrets v2 (sources now CWE-798 + OWASP Secrets
  Management; the "secret-detection tool" edit landed in the option directive because the rule
  has no qualifies field), idempotency-keys v2. Left for later (outside the brief): ~14
  pre-existing em dashes in untouched directives, three rules' sources still citing an internal
  ADR, sibling observability rules' qualifies wording, a few Oxford commas in untouched lines.
- 00:05: Wave D merged in its worktree with all 9 review fixes (5,597 tests; 440 rules). One
  integration builder now combines rank 4, item 21 + rank 2, rank 7 and Wave D onto the working
  branch (resolving the overlapping ai_audit/batch/corpus conflicts once) so a single full gate
  covers all four. Disk 78 GB.
- 23:55: rank 4 fixes done (4,295 tests); item 21 + rank 2 merged with all review fixes in their
  worktree (5,764 tests). Both queued to merge after the running gate. PDF rebuild and console
  fixes builders started in the freed slots. Decision logged by the builder: a cap-refused
  resubmission notes that the item failed once before.
- 23:50: KEV/EPSS gated green (8,864 passed, 0 failed) and pushed. Rank 10 and the CI branch
  merged; full gate running. Next in the merge queue: rank 7 (then rank 8), item 21 + rank 2,
  Wave D, corpus text + Phase 0; the PDF rebuild and console fixes start when a slot frees.
- 23:45: PDF rebuild (cloud) reviewed: content guarantees and secrets scan intact, no Typst
  injection; 1 HIGH (PDF-proving tests silently skip without typst), 3 MEDIUM (Conventions/
  Coverage sheets outside the kind reconciliation; client name not keyed by project; PDF
  reconciliation checks data, not rendered groups). Brief queued (scratchpad/w27_merge_brief.md),
  runs after the CI branch merges. Decision: a project export file redacts the client name.
- 23:40: rank 8 (cloud) reviewed: guard structurally sound; 2 HIGH (SqlTables misses
  SELECT INTO and materialized-view columns; tests cannot detect missed shapes), 3 MEDIUM.
  Brief queued (scratchpad/rank8_merge_brief.md); runs after revised rank 7 merges into feat.
  Merge order queued: rank 10, CI branch, rank 7, then rank 8.
- 23:35: console gaps 1-2 (cloud) reviewed: 1 HIGH (branch names `.`/`..` escape the worktree
  root; `.` would delete every unit's worktree), 3 MEDIUM, LOW notes. Merge+fix brief queued
  (scratchpad/console_merge_brief.md); local slots full (rank 4, item21+rank2, Wave D,
  corpus+H0).
- 23:25: 12 more cloud sessions started (H1 to H6, mutants, sources, triage, rust199, boards,
  fn-hunt); 15 cloud sessions in all. Reviews started for w27-pdf, rank8, console-gaps. Disk was
  50 GB; cleared rank 7's idle build folder (63 GB free). Zach asked for a recovery check at
  04:31 in case the usage limit is hit (it resets 04:30): scheduled.
- 23:12: rank 10 merged onto the current base with all 7 review fixes (5,083 tests green in its
  worktree); waits for the current gate, then merges with the CI branch. Corpus text (cloud)
  finished, in Fable review. Disk 66 GB.
- 23:25: KEV/EPSS merged (all 8 review fixes) and in the full gate. CI fix branch has all 5
  review fixes, green on GitHub again (run 40623533174); merges after the current gate. Wave D
  merge+fix builder started. Left for later: actions/checkout Node 20 deprecation; Rust 1.99
  lint fixes before un-pinning `rust-toolchain.toml`; `workspace.rs` keeps its own reclaim copy.
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
