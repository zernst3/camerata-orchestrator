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
   clone it next to this repository as camerata-internal, copy the working notes in (session logs,
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

## Needs Zach (paused for sign-off; collected through the night)

- `.claude/settings.json` is still tracked and holds his username, the private fixture folder,
  benchmark repo names and answer-key file names. Proposed: keep only generic glob deny rules
  (e.g. `**/*-ANSWER-KEY.md`, `**/GROUND_TRUTH.md`) in the tracked file and move the
  machine-specific absolute-path rules to his user-level `~/.claude/settings.json` (the agent is
  blocked from editing permission settings). The personal-path CI check excludes this file until
  then.
- `deny.toml` allows MPL-2.0 (file-level copyleft) for a few crates used unmodified; CI is green
  with the exception in place. Confirm or reject.
- Are the benchmark fixture names (tutorly, lumen, hearth, membersclub, faktura,
  supabase-portal) private? They appear in about 20 public docs and tests (synthetic secret
  shapes in release_gate_acceptance.rs). If private, a scrub pass follows.
- Branch protection on `main` (D3), the release tag, the API sweep's Keychain approval.

## Status log (newest first)

- 02:12: GATE GREEN on console gaps, README, W27, sources, mutants and the efficiency-join fixes: 9,346 passed, 0 failed; pushed (the one-off mutation_kills exit did not recur). Walkthrough step 6 merged (gates with the next branch).
- 02:00: walkthrough step 6 now points at the real spend limit field (ui 1,039 + ui-core 551 passed); merges after the gate. Corpus em-dash cleanup and the eight policy relabels split out of the PDF polish brief and started now (corpus files only); the template half still waits for coverage.
- 02:00: false-negative audit rechecked against today's code: of 12 HIGH, 4 partly fixed
  (truncated replies, batch truncation, skipped files, language routing; some by coverage), 8
  still open; M1 to M12 mostly open. Fable wrote fix specs in three builder batches that do not
  share functions (AI tier, intake and ledger, exporters and dependency audit); each starts once
  the branches it overlaps (coverage, rank 8, citations, client-lock, injection) have merged.
  Needs Zach: which severity wins when findings merge at one location (H5) and the self-hedge
  cap (H7); builders do only the safe half (keep absorbed findings visible).
- 01:55: gate on sources + mutants: 9,307 passed, 0 failed, but the mutation_kills test program exited with no output once (26 of 26 pass alone, six runs out of six); not pushed on that evidence. Merged the efficiency-join fixes; full gate re-running over both.
- 01:50: strict-source audit (Fable, 20 sources fetched, the rest judged from source titles and
  notes, about plus or minus 10): 323 of 440 rules cite an outside source that itself states what
  the rule requires; 100 grounded rules cite a source that sets the direction while the project
  sets the specifics. The earlier "419 strict" figure was wrong. Eight rules whose own sources
  concede project policy get relabelled policy (grounded 423 -> 415), folded into the PDF polish
  corpus pass. Needs Zach: which number the site publishes (it says 410). Proposed sentence:
  "323 of Camerata's 440 rules trace to an outside standard that states the rule itself, not just
  its topic." List at scratchpad notstrict_final.txt.
- 01:45: efficiency-join fixes done in their worktree (all 7 items; regeneration failures now
  metered against the cap; a failed routing group no longer discards other groups' paid
  replies; the estimate includes regeneration); merges after the running gate. Walkthrough step 6
  re-pointed at the real spend limit input (builder started). Viewer 7c deferred: the server has
  no mapped_rule_id yet and ai_audit.rs is held by four queued merges. Fable re-verifying the
  false-negative audit against today's code to plan the HIGH fixes.
- 01:40: gate re-run: 9,277 passed, 2 failed: a CLI parse test W27 left without --client (fixed on the sources branch) and the local-only sample-report template copy (refreshed). Merged sources and mutants plus docs/MUTATION_TESTING.md; gate re-running.
- 01:35: sources merged and fixed in its worktree: 423 of 440 rules grounded (cite an outside
  standard), 17 policy. spirit-optimize-1 relabelled policy (no CWE says build optimisations in
  from day one; it cites the three member CWEs with notes). Strict count (the source states the
  directive) is being audited by Fable before any number goes on the site. Boards builder started.
- 01:30: mutants reviewed: merge after one fix (the docs file its tests cite). Its secrets fix closes a false negative (a Google key next to a Firebase config was hidden as a public key). Docs file written; merges after the running gate.
- 01:23: mutants finished (tests killing surviving mutants in rules and secrets, one secrets fix); Fable review running.
- 01:25: client-lock (H2) reviewed: merge after fixes. The lock fails closed on every inspection
  path; two holes to close (chat and draft steps can reach a non-Anthropic model on a client
  engagement; a scan with no active project is treated as non-client) and DATA_HANDLING.md
  overclaims (dev loop sends tools; OSV.dev gets dependency names). Merge order: coverage ->
  client-lock -> citations -> injection (folds its section into DATA_HANDLING.md). Needs Zach:
  DATA_HANDLING.md is a public claims document; the default backend in .env.example becomes the
  API; every saved project, his own included, becomes a client engagement on upgrade and refuses
  until switched off in Settings; the client README.txt prints "Client engagement: yes/no".
- 01:20: gate on console + README + W27 failed to compile two test literals (fields W27 added, tests from the integration predate them); fixed in cbcca7ac (workspace clippy clean), gate re-running; builders told to merge feat.
- 01:08: client-lock (H2) finished after its 2-hour silence; Fable review running.
- 01:05: merged console gaps, README split and W27 PDF rebuild into feat; full gate running.
  Coverage builder started. Checked the regenerated sample PDF by eye: the Fix text keeps the
  rule file's hard line wraps and prints backticks literally, and 227 rule files carry em dashes
  that reach client PDFs. Brief queued (after coverage): soft-wrap and inline code in the
  template, a corpus test banning em dashes in client-facing rule text, and the cleanup.
- 01:05: GATE GREEN on the integration + hygiene + Rust 1.99 merges: 9,177 passed, 0 failed;
  pushed. W27 PDF rebuild fixed (5,016 tests); bundled-only fonts NOT forced because it drops CJK
  and emoji from client PDFs (the test pinning that stays; decision recorded).
- 01:00: rank 8 builder started (rank 7 is now on feat).
- 01:00: README split merged and fixed in its worktree (camerata-rules 293 passed): README now
  says a person reviews findings in the desktop app and the headless export is untriaged (PDF
  stamped as an unreviewed draft); stale 417/437 lines fixed. Decision: the five linked technical
  docs get a whole-corpus count check (any 100+ rule count must equal the real one) instead of the
  strict check, which would have flagged about 50 correct subset counts. DATA_HANDLING.md exists
  only on cloud/client-lock: it must land before the PR to main. Queued to merge after the gate.
- 00:55: Fable on the integration's conflict resolutions: faithful, nothing dropped, spend cap
  holds on every new path. Found one metering hole (detail regeneration drops errors and does
  not charge billed refusals to the cap) and untested joins (batch + routing, batch + terse,
  estimate without regeneration). Builder fixing all 7 items (4 compile-heavy builders now).
- 00:47: console gaps leftovers closed (4,986 tests): Intake scope and every worktree path refuse
  repos outside the active project (409); the console shows the server's refusal message.
  Queued to merge after the running gate, then boards. Sources and README builders started.
- 00:43: coverage (H4) reviewed: merge after fixes, queued after the W27 PDF merge (same
  template, workbook tail and manifest). Merge order fixed as W27 -> coverage -> citations ->
  injection, one at a time. Needs Zach: the engine's build commit (short hash) is printed in
  client PDFs and findings.json.
- 00:40: injection (H3) reviewed: merge after fixes, queued after citations (shared hunks).
  Decided: clone-advisory judge gets the frame too; the injection checker skips test files
  (prefer false negatives, and its own remediation says move fixtures into tests); lint-style
  "do not flag this" fires only with an AI word on the line. Accepted cost: no prefix-cache
  hits across runs and a one-time re-scan of cached AI findings after the schema bump.
- 00:35: merged into feat: the five-branch integration (rank 4, rank 2 + item 21, rank 7, Wave D,
  corpus Phase 0: 440 rules, 3 new, 211 bumped; showcased rules at v3/v2/v2 unchanged), repo
  hygiene, and Rust 1.99 prep (reviewed clean; toolchain stays 1.95.0). Full gate running.
  Freed 30 GB of main incremental cache plus two finished worktrees (disk 38 -> 65 GB); builders
  now build with CARGO_INCREMENTAL=0. Citations reviewed: merge after fixes (brief queued).
  Fable checking the integration's conflict resolutions. Needs Zach: temperature 0 on Haiku 4.5
  (citations), old dev.zachernst.cargo-disk-guard launchd job must be booted out by hand after
  reinstalling under the new label.
- 00:40: cloud citations (H5), coverage (H4), injection (H3) and rust199 finished; four Fable
  reviews running. client-lock (H2) and mutants have pushed no commits after about 2 hours (the
  cloud sessions are not visible from here; check them in the app in the morning). Disk at 44 GB
  with four builders active; each finished builder's build folder is freed on completion.
- 00:35: console gaps merged in its worktree with all 5 review fixes (4,980 tests); one shared
  branch validator, containment check before any worktree delete, project-membership refusal.
  Sent back for two leftovers (membership on Intake/multi-repo setup; show rejected branch errors
  in the UI). Decision: Ship pushes the recorded branch (the one holding the commits).
- 00:25 (04:23 UTC): GitHub CI GREEN on the working branch for the first time (run 40629518781,
  "Camerata governs its own code" success). Disk 49 GB: all four active builders (W27, console,
  integration, hygiene) hold 60 to 75 GB build folders each; no idle folder left to clear;
  watching (the disk guard's tier 3 at 40 GB only touches idle repos).
- 00:30: outside-standard sources (cloud) reviewed: no HIGH; grounded 410 -> 421 accurate (strict
  419 until two principle-level groundings are tightened). Brief queued
  (scratchpad/sources_merge_brief.md), runs after the integration merge. Follow-up noted: about
  700 source titles corpus-wide still carry em dashes, internal paths or rule ids (e.g.
  orch-conformance-1 "AGENTS.md — ..."): a separate corpus title pass.
- 00:25: boards optional (cloud) reviewed: 2 HIGH (native refs render as `#n` and GitHub
  autolinks them to real issues; merge conflict with console gaps), 3 MEDIUM (repo not
  validated before paths and push URLs; phantom units; numbers reused after delete). Brief
  queued (scratchpad/boards_merge_brief.md), runs after console gaps merge. Hygiene merge+fix
  builder running; settings.json move, MPL-2.0 and fixture-name privacy added to Needs Zach.
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
