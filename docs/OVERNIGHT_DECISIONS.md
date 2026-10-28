# Overnight autonomous run: decisions needed and progress log

## Morning summary, 2026-10-28 (read this first)

**Where things stand.** The working branch `feat/audit-report-export` is green and pushed: the
last full gate passed 9,694 tests with 0 failures, and GitHub CI is green again. 64 merges
landed overnight (260 commits). A final gate on the prompt-injection merge was running at
08:00; its result is in the status log of the overnight plan.

**Merged overnight (each reviewed by Fable, fixed, then gated):**
- Repo hardening work order: hygiene (H1), corpus phase 0 (H0), client lock and execution
  isolation with DATA_HANDLING.md (H2), prompt-injection defense (H3), no silent loss (H4),
  honest citations and `camerata repeatability` (H5), README and PLATFORM split (H6).
- Token efficiency ranks 2, 4, 7, 8, 9 and 10, item 21, and a Fable integration review of the
  switches together (one metering hole and untested joins fixed).
- PDF rebuild (W27) plus a fix-text polish (paragraphs, code in mono), Wave D, KEV/EPSS, the
  model registry, rule sources (22f), the mutation-testing kills, the GitHub CI fix.
- Development console (V2): console gaps 1 and 2, boards optional (native units), the per-rule
  PR planner, the adversarial reviewer contract and structured investigation decisions.
- Corpus: 441 rules (one new prompt-injection rule). No em dashes and no serial commas in
  client-facing rule text (both now enforced by tests); eight rules honestly relabelled policy.
- A real speedup: every report build re-hashed all 440 rules; now cached (one test went from
  24.5 minutes to 3.7 seconds).
- Two test flakes root-caused: macOS security-checks every newly written executable on first
  run (2.2 seconds measured); the fake-CLI helper now warms each fake before timing it.

**Still running (local, none in the cloud):** false-negative batches A and B, review fix-ups
for false-negative batch C and for findings-to-units of work (V2 2c). Every cloud session has
finished; new ones start only from a message typed in the desktop app.

**Numbers for the site.** 441 rules across 22 domains. 416 cite an outside standard (the rule
format's "grounded") and 25 are project policy. A Fable audit found that about 323 of the 440
(now about 324 of 441 with the new LLM01 rule) cite a source that itself states the rule's
directive, plus or minus 10; the earlier "419 strict" figure was wrong. Suggested sentence:
"323 of Camerata's 440 rules trace to an outside standard that states the rule itself, not just
its topic." The full list is in `_workspace/brownfield-audits/2026-10-28_strict-source-audit.txt`.

**Decisions made overnight (all reversible; details in the status log):** temperature 0 stays
on Haiku 4.5; the prompt-injection checker skips test files and lint-style "do not flag this"
needs an AI word on the line; a reviewer finding closes only on an explicit confirmed fix;
merged findings stay in one list with a `merged_into` key; bundled-only PDF fonts were NOT
forced (they would drop CJK and emoji from client PDFs); a retest whose AI pass failed never
marks AI findings fixed; the walkthrough's step 6 points at the real spend limit field.

### Needs Zach

1. **Keychain approval for the API sweep** (the morning sweep runbook is ready; see the
   overnight plan). The candidates pre-filter (rank 8) ships ON: the sweep's per-rule recall
   check is a release blocker; any finding lost with it on means turning it off by default.
2. **Site numbers:** which count to publish (above).
3. **`.claude/settings.json`:** move the machine-specific deny rules to your user settings and
   keep only generic globs in the tracked file (the personal-path check excludes it until then).
4. **Branch protection on `main` (D3)** and **the release tag** with the PR into main (drafted,
   not merged).
5. **Defaults that cost money or change behaviour, all left at today's behaviour:** Layer 3
   review on by default, one fix round after a bounce, reviewer on the strongest tier, fail
   closed on reviewer error, remediation from Confirmed findings only, dependency ordering
   advisory only, which finding id the PR body shows.
6. **Client lock side effects:** every saved project, yours included, becomes a client
   engagement on upgrade; the chat then refuses the `cli` backend until you switch it to the API
   or turn the flag off for your own projects; `.env.example` defaults to `api`; client
   README.txt prints "Client engagement: yes/no"; DATA_HANDLING.md is a public claims document.
7. **Severity decisions:** which severity wins when findings merge at one location (H5) and the
   self-hedge cap (H7). Builders did only the safe half (keep absorbed findings visible).
8. **Disclosure choices:** the engine's short build commit is printed in client PDFs and
   findings.json; temperature 0 on Haiku 4.5.
9. **Licences and names:** MPL-2.0 exceptions in `deny.toml` (CI green with them); whether the
   benchmark fixture names (tutorly, lumen, hearth, membersclub, faktura, supabase-portal) are
   private.
10. **Machine:** run `gh auth login`; boot out the old `dev.zachernst.cargo-disk-guard` launchd
    job after reinstalling the renamed one; consider excluding the repo's `target` folders from
    XProtect and Spotlight (a security setting, so your call; it was the biggest load source).
11. **Security notes (pre-existing, not urgent while the app is local-only):** agent runs pass
    the Claude CLI `--dangerously-skip-permissions`, so tool allowlists are advisory and only
    deny lists bind (the new investigation tools are now denied to other roles); the uow routes
    have no auth or project-membership layer. A placeholder-word rule hides a random key that
    contains a run like "9fake" (false-negative list).


> ## ▶ RESUME HERE (paused 2026-10-26 at Zach's request, after code viewer step 4)
> **CURRENT (newest first; older entries below are history):** 2026-10-27 (Opus, from Fable
> reviews of the cloud branches): (1) a checker-emitted `info` finding is an observation, not a
> live defect, so the corpus `min_severity` floor never raises it (it was flooring the Supabase
> platform-owned RLS note to critical). (2) The synchronous desktop audit endpoint never runs
> batch mode (no persisted batch id there); it runs real-time and says so; batch runs only on the
> job path. (3) A batch whose submit succeeded is never resubmitted after a poll/fetch failure;
> its items are disclosed as failed with the batch id. (4) A GHSA that aliases a KEV CVE counts
> as known exploited (OSV treats aliases as the same vulnerability). (5) Rank 7 routing: Supabase
> SQL rules also go to every migration/db/schema path and to any file containing SQL markers
> (`contains_any`), and routing OFF restores the old batch behaviour exactly. Cloud branches are
> merged locally by Sonnet builders after Fable review (cloud sessions cannot receive fixes).
> 2026-10-27 (Opus): Camerata's
> fallback model defaults (`crates/ui/src/cockpit.rs` ~353-361: Haiku 4.5, Sonnet 5, Opus 5) move
> to the current generation (Haiku 5.5, Sonnet 5.5, Opus 5.5) AFTER the API sweep; the sweep stays
> pinned to `claude-sonnet-5` so its results compare with sweep5b. Camerata uses no Fable model by
> default and sends `tool_choice: auto` everywhere; the per-model request shapes only guard the
> models a user can pick. 2026-10-27 (Opus): 22f Part A
> BUILT (`camerata corpus check-sources`, new crate `camerata-source-check`, sidecar
> `crates/rules/CORPUS_SOURCE_HASHES.json`). Decisions: hash only cited anchors (whole pages are
> noise); a changed section stays flagged until accepted explicitly (`--accept`), never silently
> re-baselined; confidence lowered for dead sources and unaccepted changes, and for 100 days after
> an accepted change. Live smoke run: 619 URLs, 10 dead (docs.rs sqlx, ruff, FastAPI among them),
> 15 cited anchors not found; source URL repairs queued with the corpus text work (22g), not done
> ad hoc. Wave 1 (ranks 2, 4, 7, 10) moves to cloud sessions on `wip/rank9-base` so the Mac keeps
> four local builders; the spike docs now live in `docs/plans/2026-10-25_spike-token-efficiency*`.
> 2026-10-27 (Opus): viewer 7d
> (command palette) BUILT, in Fable review. Decisions: bulk and selection-based writes stay out of
> the palette (a mistyped filter must never be one Enter from a bulk write); Ctrl/Cmd+K must work
> anywhere on the results screen, done by extending the timer's existing document bridge (no second
> bridge). 7c (advisory findings grouped by corpus rule) is BLOCKED on a server field: calibration
> sets `mapped_rule_id` (closest adopted rule) for every advisory finding, keeps the model's own
> id as `model_rule_id`, falls back to `category`; built after ranks 4 and 7 merge (same
> calibration code), then the small UI change. 2026-10-27 (Opus): three more
> cloud builders on branches `cloud/w27-pdf` (W27 PDF + confidentiality + 22c + 22d + export
> gates 2/6/7; owns export code), `cloud/w29-wave-d` (Wave D + 22e; Fable reviews locally before
> merge since cloud sessions cannot run Fable), `cloud/item21` (2b follow-ups). Decision:
> `camerata-check` treats low-severity consistency findings as advisory (exit 0); medium and above
> fail the build; one constant. 2026-10-27 (Zach, standing
> approval): Opus decides and acts without asking, including commit and push; record each decision
> here with its reason. Ask Zach only for truly irreversible actions or deletion of real work or
> data (throwaway builds and worktrees are fine). 2026-10-27 (Zach): W27 PDF brand
> assets supplied: the Cantus Works website draft is the style guide (not final, but the final
> site will closely match). Extracted to `docs/design/2026-10-27_pdf-brand-guide.md` with the logo
> marks in `docs/design/brand/`. W27 is no longer blocked on assets. Font decided (Zach): Inter for
> text, JetBrains Mono for code (SF fonts cannot be embedded). Cloud
> builders now run alongside local ones (first: KEV/EPSS on branch `cloud/kev-epss`).
> 2026-10-27 (Opus, wave plan):
> Zach re-saved the Anthropic key (Keychain entry verified, app reports it set) and set the $100
> Console limit; he is moving to the 20x plan, so the remaining ranks build in parallel. After
> rank 9 merges (it adds `EfficiencyConfig`), wave 1 = ranks 2, 4, 7, 10 at once; rank 8 after
> rank 7 merges (same `Rule` struct, `run_routed_passes`, corpus lock). Routing inside batch mode
> moves from rank 2 to rank 7 (same dispatch block). Rank 2 and rank 10 defaults ship ON before
> grading because the API sweep grades them before any client run; any recall loss there flips
> the switch off. Then: **PRIORITY
> CHANGE: finish ALL token-efficiency work first, then test it on the API** with the $100 credit
> (expires 2026-11-02). Zach's GO on the protected ranks 4 (terse scan output; calibration and
> merge inputs), 7 (routing on `applies_to`; corpus field), 8 (deterministic `candidates`
> pre-filter; corpus field) and 9 (CLI pre-flight estimate + `--max-cost-usd` stop-loudly cap).
> Rank 2 (batch default + per-item resubmission + routing and truncation retry in batch) also
> to build. Each rank ships behind its own setting (default on) so the fixture sweep can isolate
> a regression without a rebuild. Order (Zach): the Keychain bug first, then every efficiency
> rank (Opus adds rank 10, fix stage on Haiku, so ALL efficiency work is tested on the credit),
> then one API sweep. Viewer 7a finishes; 7b to 7d wait. FOUND 2026-10-27: the
> credential store has never persisted (keyring 3 built with no platform feature = mock store;
> Zach's saved key was lost); fix in flight with the Keychain-first key resolver. Earlier
> 2026-10-27: viewer step 6 (B.5
> decision memory + B.7 counts strip) MERGED. Pre-fill is SAME-STATE only (an Ignored reason is a
> false statement under False positive); memory is shared across repos (it only pre-fills, never
> applies); Undo closes an open reason prompt first. Step-6 manual checks: caret position in a
> pre-filled prompt; look of the "Previously on this shape" chip and the counts strip; screen
> reader does not re-read the strip on each decision. Next: step 7a (internal review timer, stats
> view, ETA), then 7b (B.6 reviewer notes into xlsx / JSON / PDF), 7c (advisory findings grouped
> by the mapped corpus rule), 7d (B.8 palette); all approved 2026-10-23. Earlier: viewer step 5 (B.2
> stacked group view) MERGED: `B` / "Review all N stacked", ±3-line cards from the batch
> endpoint, J/K/Space/Enter/C/F/T/I, paging past 200. Fable's HIGH fixed (mouse doors were live
> under an open reason prompt; paging then committed off-screen rows): every decision door is now
> gated on one `stack_focus`. Also: one Queue write path (`apply_rows`, with the five-second undo)
> for the pane, stack and every toolbar button (`bulk_confirm` / `bulk_false_positive` deleted);
> a pane row click is refused while its reason prompt is open; "Move to Ignored" from Tech debt
> now requires a reason; a debug-only Dioxus borrow panic in the pane prompt commit fixed. Step-5
> manual checks: disabled-button look under a prompt; focus stays in the reason field when a card
> or row is clicked; card follow-scroll on J/K; stack under the viewer; undo countdown in the
> stack. V2 roadmap gained item 6 (pre-pen-test readiness) and item 7 (PHP / Laravel rules).
> Next: viewer step 6 (B.5 decision memory + B.7 counts strip). Earlier: code viewer steps 1 to 4 MERGED
> and pushed 2026-10-26 (step 4: batch finding-context endpoint, pane ±3 lines context, Queue
> order presets; Fable's 3 medium + 13 low fixed, plus the GIT_CONFIG test race made impossible).
> No agent worktrees are in flight. **Resume with viewer step 5** (B.2 stacked group view; the
> batch endpoint it needs already exists). Step-4 manual checks: the table scrolls to the cursor
> row after a preset switch; the arrows and `C` work right after choosing in the Order select;
> look of the select and the pane context block; feel of the flat Critical-first cut on a few
> hundred real rows; pane context matches `git show <sha>:<path>`, and a scan with no recorded
> commit shows none. Known soft spot: the pane's stale-answer test uses real delays with wide
> margins (not a flake so far). Item 24 Firebase/Firestore
> coverage is MERGED (13 rules, rules-file reader, apiKey twin; all 11 Fable review items fixed).
> Token-efficiency ranks 1, 3, 5, 6, the 22a API backend default (CLI and UI halves), eslint
> EINVAL hardening, and the CLI `bookkeeping_is_never_written...` flake fix are all merged. No
> agent worktrees are in flight. **WAITING ON ZACH:** (1) the manual check of delete/re-fetch on a
> THROWAWAY clone (TECHNICAL.md section 7, USER_GUIDE 5a); (2) go/no-go on token-efficiency
> ranks 4, 7, 8, 9 (protected changes) and the live API fixture runs (they spend tokens); rank 2
> (batch default) needs grading on those live runs; (3) move the settings JSON out of the repo
> (see Disk note below); (4) review the Firebase "merge invariant" call in DECISIONS MADE.
> **NEXT V1 WORK (no approval needed):** review speed-ups (WORK_ORDER_REVIEW_SPEED; timer
> internal-only with auto-pause and manual override). Build-order step 1 (source-file endpoint +
> highlighter), step 2 (viewer overlay, navigation, in-viewer dispositions) and step 3 (A.5
> popover, A.6 search / symbol refs / permalink, Rule Hub key; Fable review fixed: honest search
> failure status, one isolated git command builder, a 4-slot git read gate, shared key-body
> redaction) and step 4 (B.3 pane context + B.4 order presets) MERGED 2026-10-26; next is 5
> (B.2 stacked groups), 6 (B.5 decision memory + B.7 counts strip), 7 (B.6 notes + ETA, then B.8
> palette). Step-3 manual checks: `Down` past the visible rows of a long references list keeps
> the selection in view; the Rule Hub opened from the pane, closed with `Esc`, leaves `C` and the
> arrows working without a click; `Esc` closes the hub after clicking inside it. **Viewer manual check for
> Zach** (only a real window can confirm; add to the delete/re-fetch session): `O` and the three
> click doors open it; Esc returns focus to the pane on the last-viewed finding; a row click after
> closing lands (ghost-click); scroll feel and the upper-third landing; sticky gutter, minimap, look;
> five-second undo from inside the viewer; paging a 400 KB to 4 MB file while scrolling; Esc closes
> the `?` key list first; Esc from the viewer with the pane's reason prompt open keeps the prompt. Worktree agents start from stale `main`: every brief must reset to
> `feat/audit-report-export` first. Then W29 Wave D + 22e, corpus text work
> (items 5, 11, 12, 19) + 22g, engine follow-ups (22c, 22d, 22f, KEV/EPSS + data-as-of stamp, model
> registry, CI-gate severity bounds, staleness nag, W30 residuals), W27 PDF rebuild (needs Zach's
> brand assets), 19f grader revision, then a **whole-workspace mutation run** (Zach, 2026-10-26:
> after all dev work, every crate, not just checks; machine time only), then the graded accuracy
> run (item 20). Mutation run: `cargo mutants --workspace --jobs 4` from a clean merged checkout,
> detached with `nohup caffeinate -i`, log to `~/Library/Logs/cargo-mutants-workspace.log`. About
> 24,600 mutants (server 8.7k, checks 8.3k, ui 1.9k, ui-core 1.3k, rest 4.4k); the checks run did
> 6,380 in 6 h, so plan on roughly 24 to 36 h, no cargo builds in the main checkout meanwhile.
> Each mutant runs its own crate's tests (the default), not all 8,100: `--test-workspace=true`
> would take weeks for few extra catches (Zach confirmed 2026-10-26: own-crate tests now; maybe a
> full cross-crate run after V2). Then triage `missed.txt`: each
> real gap gets a test (or a fix if it hides a bug), equivalent mutants get ledgered.
>
> (History) Branch `feat/audit-report-export` is pushed. No agent worktrees are in flight.
> **Disk:** Full Disk Access granted 2026-10-22 to `/bin/bash` and `cargo-sweep`; the 30-minute
> guard verified running ("start; free=" in `~/Library/Logs/cargo-disk-guard.log`). OPEN: Zach
> pasted the settings JSON (hook + deny list) into the REPO's `.claude/settings.json` instead of
> `~/.claude/settings.json`. It is uncommitted and must NOT be committed (personal paths). Once he
> moves it to `~/.claude/settings.json`, restore the repo file with `git checkout .claude/settings.json`.
> **W29 done:** Wave A (ONE-IMPORT-PATH, SHARED-CONSTANT, DEAD-CODE), Wave B
> (UI-REACT-HOOKS-LINT-1 + the eslint baseline-leak fix + exact eslint pins), and Wave C (clone
> detector + SINGLE-SOURCE-LOGIC, NAMING-ONE-CONCEPT, SHARED-PRIMITIVE + the advisory judge seam).
> Every detector was Fable-reviewed, fixed, and merged. **V1 scope** (Zach, DECISIONS MADE below):
> everything in `docs/plans/2026-10-19_MASTER-SEQUENCE.md` plus the graded accuracy tests (the
> outreach go-signal); no new scope except defect fixes; rule `stages` is out of V1.
> **Overnight mutation run (started 2026-10-22, no model tokens):** `cargo mutants --package
> camerata-checks --jobs 4` (6,380 mutants, est. 12 to 14 h), detached with `nohup caffeinate -i`.
> Log: `~/Library/Logs/cargo-mutants-checks.log`. Results (written as it goes):
> `mutants.out/` in the repo root (gitignored): `caught.txt`, `missed.txt`, `timeout.txt`,
> `unviable.txt`, `outcomes.json`. Report: caught / (caught + missed) as the fault-catch rate;
> triage `missed.txt` into "add a test" vs "equivalent mutant". Only the checks crate's own tests
> run against each mutant (server-level tests that exercise checkers are not counted). Check it is
> finished (`pgrep -fl cargo-mutants`) before rebuilding in the main checkout.
> **DONE 2026-10-25 (later):** 22a is complete (new app projects default to the API backend;
> stored and legacy projects keep their value; no key = Blocked, never a CLI fallback) and eslint
> runs from the canonical generation path (immune to the macOS symlink-swap EINVAL). OPEN DEFECT:
> `camerata` (cli) test `inspect_cmd::tests::bookkeeping_is_never_written_into_the_scanned_repo`
> flaked once in a full run; it reads process env without the `process_env` serial key (the env
> discipline from item 19a). Fix: give it the key or inject the state dir. Then: Zach's manual
> check, and token-efficiency build ranks 1, 2, 3, 5, 6 (no protected change); ranks 4, 7, 8, 9
> and the live API fixture runs await Zach's go.
> **NEW V1 ITEM 2026-10-25 (Zach):** token efficiency on the API, MASTER-SEQUENCE item 23
> (spike `_workspace/brownfield-audits/SPIKE_TOKEN_EFFICIENCY.md`, recall first). Slotted right
> after 22a. Start with the Fable spike (read-only), then build the gaps.
> **DONE 2026-10-25:** provisioning test flake fixed (macOS transient EINVAL on a lookup racing
> the atomic symlink swap; 150/150 under load) and 22a's CLI half: `camerata inspect` defaults to
> the API backend, no key = clear error (never a CLI fallback), backend + provider recorded in the
> manifest and README and checked by the release gate. **OPEN, do next:** (1) 22a UI half: the app's
> `ProjectBackend::default()` is still `Cli`, so client inspections run from the app still use the
> subscription; make client projects default to Api (new projects), decide/migrate existing ones,
> same no-key rule. (2) Harden eslint against the same macOS EINVAL: canonicalize the workspace
> path once before spawning eslint. Then Zach's manual check of delete/re-fetch.
> **DONE 2026-10-24 (end of budget):** item 22b `camerata count` (reuses the inspection's
> `read_local_repo_files`; fixed a double count of tracked noise files) and the 26 major mutation
> gaps (25 killed, 1 documented equivalent) are MERGED. OPEN DEFECT: the test
> `tool_provisioning::tests::a_reader_never_sees_a_missing_tree_while_eslint_reprovisions` flaked
> once under the full suite (passed on rerun); fix the root cause next session. Next: Zach's manual
> check, then 22a (API backend default).
> (Superseded) **IN FLIGHT 2026-10-24 (end of the weekly budget):** a Sonnet agent in a worktree under
> `.claude/worktrees/agent-*` (find it with `git worktree list`) is building item 22b
> `camerata count`, then the 26 major mutation gaps, committing each green step. It may have been
> cut off when the budget ran out. NEXT SESSION: check that worktree's commits, run the full gate,
> merge what is green (`scripts/remove-merged-worktree.sh <id>` after), and resume the rest.
> **STATUS 2026-10-24 (stopped for Zach's manual check):** BOTH client-trust blockers are done
> and pushed: secret redaction + release gate, and delete + re-fetch client source (Fable-reviewed:
> 3 HIGH defects fixed, incl. emit-local resurrection; deletion refuses during scans and governed
> runs; 7,577 tests green). Exports now survive a restart (persisted InspectionStore).
> **WAITING ON ZACH:** the manual check of delete / re-fetch in the running app on a THROWAWAY
> clone (steps in TECHNICAL.md section 7 and USER_GUIDE 5a). **Next after that:** item 22a (API
> backend default for client inspections) and 22b (`camerata count`), then the 26 mutation gaps,
> the review speed-ups, W29 Wave D (+ 22e), corpus text (+ 22g), engine follow-ups (+ 22f, KEV/EPSS,
> 22c, 22d), W27 PDF (+ confidentiality marking + visual track), 19f grader, graded run.
> **STATUS 2026-10-23 (paused for the night):** secret redaction + export release gate DONE and
> pushed (7,273 tests green). The old leaky exports are NOT regenerated separately: the graded
> accuracy run (item 20) reruns them (Zach). Mutation run done (97.9%; 26 major gaps ledgered for
> V1). **Next session starts with the delete + re-fetch client source blocker** (decisions below).
> Then: 26 mutation gaps, W29 Wave D, corpus text, item-21 engine follow-ups, W27 PDF (needs
> Zach's eye), 19f grader revision, graded run (item 20).
> (Historical) **FIRST, before anything else (Zach, 2026-10-22):** the secret-redaction work order
> (`_workspace/brownfield-audits/WORK_ORDER_SECRET_REDACTION.md`) plus the pluggable export release
> gate; decisions recorded under DECISIONS MADE. Process: Fable spec, Sonnet build, Fable leak hunt.
> **SECOND (Zach, 2026-10-22):** delete client source after delivery (per-repo destructive action,
> project stays fully usable without the repo); decisions under DECISIONS MADE.
> **Then, in order (sequential, to conserve tokens):** W29 Wave D (UI-STATE-SHAPE-1,
> STYLE-NEEDLESS-COMPLEXITY-1: Fable spec, Sonnet build, Fable review); then corpus text work
> (MASTER-SEQUENCE items 5, 11, 12, 19 corpus sub-items); then the engine follow-ups in item 21
> (CI-gate severity bounds, stale model registry with prices verified from Anthropic's pricing page,
> staleness nag at load, W30 residuals); then W27 PDF rebuild (needs Zach's eye); then 19f grader
> revision; then the graded sweep (item 20). First, a cheap Haiku pass to bring the
> MASTER-SEQUENCE status table current (several items it shows OPEN are done).
> Process: Opus directs; Fable 5.1 writes trap-heavy specs and adversarially reviews every detector
> before merge; Sonnet builds; Haiku does mechanical work. Remove each agent worktree right after
> merge (`scripts/remove-merged-worktree.sh <id>`); the 30-minute cargo disk guard must stay
> installed. Single-purpose shell commands; edits via the Edit tool; questions to Zach only for real
> product decisions, in plain language. Standing directive: keep developing; nothing dropped; fix
> every defect.


Started 2026-10-20 (Zach logged off; autonomous continuation of the master sequence).
Zach reviews this in the morning. I commit locally; I cannot push unattended, so every
commit sits on the branch ready for review + push.

## DECISIONS NEEDED (Zach)

- W32 part (b) (`superseded_since` / `current_recommendation`: per-rule fields saying "the
  ecosystem moved off this pattern in year X; current guidance is <link>"). The earlier "dropped"
  ruling was inferred from Zach asking for more context, not stated by him. Zach asked for a
  plain-language re-explanation before deciding (2026-10-21). Until then: status UNDECIDED, not
  dropped. Note the corpus staleness sweep design already routes confirmed-stale findings into
  these fields, so (b) as a standalone chore and (b) as sweep output are separate questions.

## DECISIONS MADE

- 2026-10-26 (Opus, code viewer step 1 review calls): (1) ONE status vocabulary for
  `finding-context` and `source-file` (`FindingContextOutcome`), following the SHIPPED
  finding-context semantics: no recorded commit = `ok` + `source: "working_tree"`; missing object =
  `commit_unavailable`; the UI keys its amber banner on `source == "working_tree"` (the work order's
  A.2 step 3 wording is superseded). (2) Both endpoints now refuse any `repo` that is not an exact
  `owner/name` owned by a project (closed a pre-existing workspace escape that could read any file
  on the machine). (3) Shared bracket-depth guard before every recursive parse of client files
  (Rust over 64, JS/TS/Python over 400 = treated as unparseable; text checks still run). Measured
  headroom: the deepest of this repo's 613 Rust files is 35. Scan behaviour change: such a file
  yields no AST extraction instead of aborting the whole process. Follow-up to verify in the
  graded run: that an unparseable file is visible in Coverage, not silently clean.
  (4) Symlink and submodule blobs at the inspected sha read as `file_missing` (shared).
- 2026-10-26 (Zach): README screenshots deferred, not vital. Note for any retry: this Mac blocks
  screen capture from the session, so it needs Zach's screenshots or a Screen Recording grant.

- 2026-10-25 (Opus, Firebase review calls, all false-positive-safe): (1) auth-only update/delete
  on shared data stays an ASSERTED defect (true and worth fixing); auth-only writes on a public
  Storage/Firestore path (Firebase's own docs example) become Review. (2) Rules files that
  `firebase.json` does not reference, files in repos with no `firebase.json`, and files under
  examples/samples/docs paths are Review-graded with the reason stated, never asserted critical.
  (3) The apiKey twin tightens: rule (c) only when the key sits inside the same object/call span as
  the init call or siblings; rule (b) needs a sibling or init marker and excludes
  GENAI/GEMINI/MAPS/PLACES/TRANSLATE/VISION/YOUTUBE names; rule (a) keeps `api_key` only alongside
  google-services.json-only sibling names. (4) FIELD-VALIDATION is silent on writes gated by
  `request.auth.token.` claims or a `get(`/`exists(` lookup. (5) `camerata-check --strict` failing
  on console-managed rules is intended; say so in the release note. **Protected-area defect fix
  (merge/dedup, flagged for Zach to overrule):** an asserted (non-hedged) finding is never fused
  into or ranked beneath a held (needs-review) row; the narrowest fix that restores that invariant.

- 2026-10-25 (Opus, Firebase spec questions): (1) a Google API key OUTSIDE a Firebase web config
  keeps today's critical floor finding in V1 (changing an existing floor rule is not V1 scope);
  making it review-graded high ("restrictions not visible in the repository") is logged as a
  post-V1 refinement. (2) Dart takes the default language row (proposes api-layer, permissions,
  integration) like Kotlin and Swift, for consistency. Spec: `docs/design/2026-10-25_firebase-rules.md`.

- 2026-10-24 (Opus, delete/re-fetch review calls): (a) a `repo_path` override may aim deletion at
  any clone whose origin matches the project repo, by design ("it IS that client's code"); the
  dirty/unpushed refusals still apply; USER_GUIDE says so plainly. (b) The deletion record
  discloses generically what it cannot cover: Claude Code sessions from the chat assistant and
  governed-dev agents that read client code, sessions from inspections run before session ids
  were recorded, and CLI caches left under an old path if the checkout was moved. Honest residual
  sentences, never silent. (c) The source gate covers a deleted repo named in a request even when
  it belongs to a project other than the active one.

- 2026-10-24 (Zach): the untracked IMPLEMENTER_HANDOFF items join V1 as MASTER-SEQUENCE item 22
  (22a API backend for client inspections and 22b `camerata count` are blocking and go right after
  the delete/re-fetch blocker; 22c manifest fields; 22d reconciliation by severity and kind; 22e
  TESTING-MEANINGFUL-ASSERTION-1 with Wave D; 22f corpus freshness routine with the engine
  follow-ups; 22g showcase rule-file cleanup with the corpus text work).

- 2026-10-24 (Zach, `_workspace/brownfield-audits/IMPLEMENTER_HANDOFF_2026-10-24.md` item 8):
  **Confidentiality marking is V1** (protected: report template; Zach passed it on for building,
  recorded as his go). Every PDF page: "Confidential, prepared for <client> on <date>". First page:
  a short handling note (who the report is for; do not forward outside the company without
  consent). Every workbook sheet: the same line in its header or footer. The client name, today an
  export-time option (`ReportOptions.client_name`, cover line "Prepared for ..."), becomes a field
  of the inspection setup saved with the project, so every surface fills in automatically; an empty
  client name blocks the export with a clear message rather than printing "prepared for ." The
  marking text goes through the release gate like every other string. Slotted inside the W27 PDF
  rebuild (same template) with the workbook header/footer and the setup field built alongside.

- 2026-10-24 (delete + re-fetch spec questions): (1, Zach) Claude Code's own session files under
  `~/.claude/projects/` that hold client source from Camerata's CLI-backend AI passes are deleted
  during source deletion: Camerata tags each call it makes per inspection (`--session-id`, ids
  recorded on the inspection entry) and deletes exactly those files, listing them in the deletion
  record. Zach approved this outside-repo deletion. (2, Opus engineering call) Deletion refuses
  while the checkout has uncommitted or unpushed work unless the operator ticks "Delete
  uncommitted work too". Spec: `docs/design/2026-10-24_delete-and-refetch-client-source.md`.
  BUILT (2026-10-24, Sonnet): both decisions are implemented as written; the decision record, the
  as-built deviations from the spec, and the manual checks are in
  `docs/decisions/2026-10-24_delete-and-refetch-client-source.md`.

- 2026-10-24 (Zach): **Vulnerability intelligence, pieces 1 and 3 are V1.** (1) Enrich the
  existing osv-scanner dependency findings with CISA KEV (Known Exploited Vulnerabilities, JSON
  feed): a CVE on KEV is labelled "known exploited" and floored to the top severity; show the
  FIRST.org EPSS exploit-probability score. (3) Stamp "Vulnerability data as of <date>" (KEV, EPSS,
  OSV snapshot dates) into the package provenance (README + manifest). Deterministic feeds only;
  NO agent scraping news or social media (unverifiable, violates the verifiable-finding law).
  Piece 2 (runtime/framework version detection + end-of-life via endoflife.date + runtime CVEs via
  NVD/KEV) is the FIRST V2 item. Slotted after the item-21 engine follow-ups. Zach: the goal is to
  finish V1; if marketing is still the bottleneck after V1, start adding V2 features.

- 2026-10-23 (Zach, executive decision): the review speed-ups are V1.
  `_workspace/brownfield-audits/WORK_ORDER_REVIEW_SPEED.md` (code viewer A.1-A.10; B.2 stacked
  group view; B.3 pane context; B.4 order presets; B.5 decision memory; B.7 progress counts + ETA;
  B.6 reviewer notes; B.8 command palette) plus its companion `WORK_ORDER_REVIEW_TIME.md` (items
  1-3). Slotted (Opus) BEFORE the graded accuracy run so that review uses them and the timer turns
  it into measured review hours. Order: delete/re-fetch blocker, 26 mutation gaps, review
  speed-ups, W29 Wave D, corpus text, item-21 engine follow-ups, W27 PDF, 19f grader, graded run.
  The Phase-2 docked "code mode" layout is decided after one real review; the retest diff view
  stays V2 (rejected by the order). Opus engineering calls: add `tree-sitter-rust`; when client
  source has been deleted the viewer says "Source deleted on <date>, re-fetch to view" and never
  silently falls back to a missing working tree; `symbol-refs` passes the symbol to `git grep`
  with `-e` (or after `--`) so a leading `-` cannot be parsed as an option.
  Zach's protected-area answers (2026-10-23): (1) APPROVED: AI-advisory findings are mapped at
  calibration to the closest corpus rule (stable class label when none matches), the model's id
  kept as a secondary field, and the queue groups by the mapped id. (2) Review timer and the
  per-row "reviewed" flag are INTERNAL ONLY, never in any client export ("might raise more
  questions than necessary"); shown in an internal stats view as data for pricing. Timer behaviour
  (Opus, per Zach "automatic as much as it can, with overriding buttons"): starts automatically
  on opening a group or acting on a finding; auto-pauses after 3 minutes with no key or click and
  whenever the Camerata window is not focused, and the idle stretch before the pause is NOT
  counted; an always-visible Pause / Resume button in the progress strip; the ETA uses only
  active time. (3) Reviewer notes: one note per group (optional per row) exported as a
  `reviewer_note` column in the xlsx and JSON exports AND as the italic sentence under the group
  heading in the PDF; redacted and covered by the release gate.

- 2026-10-23 (Opus, Fable leak-hunt owner calls; asymmetric risk favours redaction): (a) T1
  placeholder words in VENDOR bodies must be delimited (`_`/`-`/start/end/case change) or the body
  single-case, so a random key containing "mock"/"fake"/"your" is still redacted; substring
  matching stays for context shapes. (b) Supabase secret bodies may contain `-` (first char
  alnum). (c) `model_prompt_redaction` comes from an environment variable only, never from any
  file in a client repository; the CLI prints the warning on stderr. (d) Bodies of 20 chars or
  fewer show length only, no tail. (e) Readability twins: lowercase snake_case values with 2+
  underscores and at most 2 digits are not secrets; public keys (`sb_publishable_`, `pk_live_`,
  `pk_test_`) after `Bearer` stay readable. (f) Common vendor shapes added now: SendGrid, Twilio,
  Slack `xapp-`, Shopify, Mailgun, npm, Discord, Mapbox. Follow-up (Opus): the ReadableName twin
  never applies to identifiers containing password/passwd/pwd/passphrase/secret/credential
  (passphrases are real secrets); `passphrase` added to the S13 credential names.
- 2026-10-23 (Sonnet build of the secret redaction + release gate, branch from `0ef7065`):
  built per `docs/design/2026-10-23_secret-redaction-and-release-gate.md`; see the commit series
  and `docs/TECHNICAL.md` section 6c. ACCEPTANCE (`camerata release-gate` over the sweep5b
  exports, which predate the gate; redacted forms only): faktura FLAGGED, one Supabase secret key
  (51 chars) at `findings[0].snippet`, `findings[0].fix_specific`,
  `findings[0].also_locations[1].snippet`, `findings[43].fix_specific`, workbook `All Findings`
  T2/V2/V45 and `Universal` T2/V2/V8, and the PDF text; hearth FLAGGED, Supabase (42 chars) plus
  two Stripe live keys (77 chars) and a webhook secret (38 chars) on `findings[1]`; portal CLEAN
  (placeholders readable, no secrets problem); lumen and tutorly also flagged (a Supabase key,
  a real-looking `sk_test_` key, webhook secrets). The old zips also report the CSV/JSON datasets
  and manifest as missing (they predate those exports). STILL OWED (needs a live model run, so
  not done here): regenerate the three exports with the new build and run
  `camerata release-gate` on them, expecting zero problems and the redacted forms present in
  `findings.json`. Steps: delete the stale `.camerata-scan-cache.json` and
  `.camerata-scan-ledger.json` from the hearth fixture checkout, then for each fixture
  `camerata inspect --repo <fixture> --full --export <out>/<name>.zip` (this spends model tokens),
  then `cargo run -p camerata --features pdf-text -- release-gate <out>/<name>.zip` (exit 0).

- 2026-10-23 (Opus, secret-redaction spec questions; asymmetric risk: over-redaction costs only
  readability, under-redaction leaks): Q1 real Stripe TEST-mode keys (`sk_test_`/`rk_test_`) are
  redacted like live keys; only Stripe's published documentation key stays readable via the
  published-key allowlist. Q2 placeholder values (all-caps DO_NOT/FAKE bodies, `your...here`
  style) stay readable per the work order's safe-twin rule, so the portal fixture's acceptance
  result is "clean, placeholders readable". Spec: `docs/design/2026-10-23_secret-redaction-and-release-gate.md`.

- 2026-10-22 (Zach): **Delete client source after delivery** is a V1 blocker, built right after
  secret redaction. A destructive "Delete source code from this machine" action per repo, typed
  confirmation (the repo's name), styled as a danger action. The project stays fully usable with no
  repo on disk: findings, history, exports and package regeneration keep working; the repo shows
  "Source deleted on <date>"; rescans are disabled with an explanation; re-clone restores it.
  Remove every "repo must exist" block that is not about scanning itself. Deletion purges every
  on-disk copy of client code (the checkout, scan cache, AI/judge transcripts, any worktree), and
  KEEPS the code excerpts inside findings (part of the delivered product; Zach confirmed). Safety:
  delete only a folder that is a git repo whose remote matches the project's recorded GitHub repo;
  otherwise refuse with the reason. A deletion record (what, where, when, file count) is kept in
  the project as proof for the client. Exports must never re-read code from disk. Deletion is
  ordinary removal (FileVault-encrypted SSD), not a forensic wipe; the record says so. Zach
  (2026-10-22): it does not matter how the code got into the project (cloned in the UI or by hand);
  whatever codebase the project points to is what gets deleted, guarded by the remote-match check.
  **Re-fetch (revised by Zach, 2026-10-22):** the real reasons to re-fetch are remediation and the
  governance layer's monthly delta retest, both of which need the LATEST code. One "Re-fetch
  source" button opens a dialog: "Latest from <default branch>" (pre-selected) or "The inspected
  commit <sha>, <date>" (rare: reproduce or dispute). Re-fetch clones FULL history so the inspected
  commit is always reachable. Afterwards the repo card shows "Source at <sha>, N commits newer than
  the inspection (<sha>)" with a primary "Rescan changes" action (the delta retest between the
  inspected commit and now; same flow the monthly retest uses). Finding code views read from the
  INSPECTED commit via git objects, not the working tree, so old findings stay accurate while the
  checkout sits at latest; old findings carry a "References commit <sha>; source on disk is newer"
  note. Clears the deleted state. Uses Camerata's existing GitHub token; a revoked/renamed/deleted
  origin is reported plainly. Process:
  Fable spec (incl. locating every repo-must-exist assumption and every place client code lands),
  Sonnet build, Fable adversarial review.

- 2026-10-22 (Zach): **Secret redaction is the next work, before anything else** (work order
  `_workspace/brownfield-audits/WORK_ORDER_SECRET_REDACTION.md`; blocks the first client; Zach's
  go on the protected export/template area). Secrets are redacted BEFORE any code is sent to a
  model, by default. Review additions adopted: redact once at finding construction (every
  surface inherits), one shared secret detector for scan + redactor + release check, JWT role
  claim separates service_role from anon, short secrets show length only, the release check reads
  the Typst input data plus a PDF text smoke test, failure messages never print the secret,
  fingerprints computed from redacted text (no clients yet), acceptance = flag the known leaks in
  the existing faktura/hearth/portal exports, then regenerate clean.
- 2026-10-22 (Zach, V1 scope): the secret release check is built as a pluggable **export release
  gate** that runs on every real export, and reuses the existing export invariants (cross-format
  parity, manifest hashes, provenance agreement, no missing datasets) as runtime checks. That is
  enough QC for V1. An AI reviewer that reads each package after generation is V2:
  `docs/design/2026-10-22_ai-package-qc-target-design.md`.

- 2026-10-22 (Opus, Wave C review, all false-positive-safe narrowings): (1) the three clone
  rules (SINGLE-SOURCE-LOGIC, NAMING-ONE-CONCEPT, SHARED-PRIMITIVE) emit NO convention/"clean"
  row in V1; the detector cannot prove absence. (2) SINGLE-SOURCE escalates to HIGH only on
  disagreements in decision tokens (operator, number, property inside the decision test); string
  and template differences are a trailing clause. (3) The composed-variant guard fires only when
  the helper call dominates the shared sequence (call covers >= 50% of its tokens, or every
  decision token lies inside the call's arguments). (4) Functions that render JSX are excluded
  from NAMING when UI-SHARED-PRIMITIVE-1 is armed.

- 2026-10-21 (Zach, V1 scope): **Camerata V1** = the version that runs the first paid client
  inspection. Scope = EVERYTHING already in `docs/plans/2026-10-19_MASTER-SEQUENCE.md` (all W29
  rules, W27, item-21 follow-ups, 19e/19f/20) plus the graded accuracy tests, which are the go
  signal for outreach. NO new scope except fixes for bugs or real product defects. The rule
  `stages` field is explicitly out of V1. Development is not the outreach blocker (website and
  marketing materials are), so finishing the phased work is fine.

- 2026-10-21 (Opus, on the Fable review's evidence): ARCH-DEAD-CODE-1 does NOT run at the Layer-2
  write-time gate in v1 (new `ArchChecker::runs_at_write_time`, false for dead-code). A story may
  legitimately end with scaffolding or an externally consumed file, and a bounce would teach agents
  to paste `keep-file:` markers. Scan tier and CI only. Re-evaluate once scan-tier false-positive
  data exists. Zach agreed (2026-10-21): dead-code belongs in the inspection, not during
  development; a future home is Layer 4 or a per-file override comment (the `keep-file:` marker,
  guard G12, already exists). Not needed now.
  Design debt noted (Zach asked how "inspection-only" is decided): the switch is a code-level
  `ArchChecker::runs_at_write_time()`, invisible in the corpus and UI, while the opposite control
  ("leave out of inspection") lives elsewhere. When a second rule needs stage control, promote both
  to one corpus field (e.g. `stages = ["inspection", "ci", "write-time"]`) read by every runner,
  covered by CORPUS_LOCK like any other rule field. Zach (2026-10-21): this is the target design
  for Camerata, documented in `docs/design/2026-10-21_rule-stages-target-design.md`; future
  phase, NOT V1.

- 2026-10-21 (Opus, within Zach's "narrow" ruling, false-positive-safe narrowings after the
  Fable review of ARCH-SHARED-CONSTANT-1): N1 rates require >= 2 significant digits (keeps 0.12,
  0.075; drops 0.9, 0.001, 1e-9); Django URL-name callees (reverse, reverse_lazy, redirect,
  resolve_url) are excluded; grouping is scoped to the nearest package root (package.json,
  pyproject.toml, Cargo.toml) so "import the constant" is always possible. Router path vs
  <Link>/href repeats stay IN scope (legitimate under the S2 ruling).
- 2026-10-21 (Zach, W29 scope): ARCH-DEAD-CODE-1 ships FILE-level in v1; unused individual
  exports become a separate future rule once the tooling is false-positive-safe.
  ARCH-SHARED-CONSTANT-1 uses the narrow four-shape "domain literal" definition (dashed/colon
  names, route templates, multi-word SCREAMING_SNAKE, decimals in arithmetic); widen one shape at
  a time with its own safe twins. "Two homes, no bare copies" is NOT a finding in v1.
- 2026-10-21: commit trailers from this session's Sonnet subagents read "Sonnet 4.6" but the
  agents ran Sonnet 5.5 (a brief error). Pushed history is left as is (no force-push); trailers
  are correct from 1f3c4f3's successor onward.
- 2026-10-21: ARCH-ONE-IMPORT-PATH-1 flags a split between a symbol's origin file and an index
  barrel (Zach: "that is the rule working as intended"). Work order §7's "non-index modules"
  wording does not exempt index barrels from the one-home rule.
- 2026-10-21: **Zach's go for all nine W29 corpus rules** (work order §91 protected area). Each
  rule's TOML lands with its detector, registration, positive + safe-twin tests, homed per the
  design doc's "Domain home per rule" table.
- 2026-10-21: `stash@{0}` (09-28 dead-P1 partial) dropped on Zach's OK after verifying every
  symbol and test it added already exists at HEAD.
- 2026-10-21: Camerata memory to be decoupled out of the agora-mono project memory dir (Zach:
  it should never have been there).

## PROGRESS (newest first)

- 2026-10-21: Fable 5.1 audit of the Opus 4.8 stretch. Verdict: no lost work, test claim true,
  assertion edits legitimate, every "Zach ruled X" traced to a real message. Real gaps: lockfile
  not committed (CI `--locked` red), CI red for months on fmt/clippy and never ran on branches, W29
  design keyed ONE-IMPORT-PATH on `to_file` (would never fire) and named nonexistent `arch`/`style`
  domains, 3 export gates unbuilt and unrecorded, master sequence stale, Camerata memory filed under
  the agora-mono project dir, an unrecorded 09-28 stash (checked: superseded). All recorded in
  MASTER-SEQUENCE item 21; lockfile, recent lints, W29 design, reconciliation done. One open
  confirmation for Zach: the W32(b) "dropped" ruling was inferred from his request for more context.

- 2026-10-20: Design docs extended per Zach's mid-session asks: machine-readable exports now
  include a root `manifest.json` (index + sha256 + schema versions + provenance incl.
  client-visible `rules_last_reviewed` from the staleness watermark). README confirmed to carry
  only PARTIAL metadata today (purposes/repos/generated); will mirror the new provenance lines.
  Design committed; BUILD sequenced after W27.
- 2026-10-20: Zach sequencing ruling — Track 2 (exports+manifest, staleness, W23/W24) folds in
  BEFORE the next graded sweep, and the GRADER gets a thoroughness/strictness revision for the
  new product type (package, not just scan+PDF) before the sweep. Grader MODEL stays fixed; the
  RUBRIC gets stricter. Recorded as plan items 19e + 19f, gating item 20 (the sweep).
- 2026-10-20: W27 Phase-A sample-regen blocker RESOLVED. The curated_total 6->5 drift was
  confirmed-correct behavior (SUPABASE-AUTH-EDGE-JWT-1 has confidence=needs-review -> now held,
  not curated). Updated the generator's stale expectation + locked the reconciliation. Generator
  green, regenerates the (gitignored) local sample against the current contract.
  NOTE: the heavy W27 work left = the Typst TEMPLATE rewrite (visual, per 2026-10-16_pdf-design-final.md
  §2-6). That needs your EYE on the rendered PDF, so I am NOT doing it blind overnight; I will do
  the testable/non-visual items first (staleness mechanism, W23/W24) and leave the visual template
  rewrite + the exports-that-depend-on-the-final-xlsx for a session where you can review layout.
- 2026-10-20: Corpus staleness MECHANISM built + tested (commits on branch): freshness.rs data
  layer (overdue/never_confirmed/unparseable, pure date math), confirm mutation (comment-preserving),
  nag formatter, CORPUS_FRESHNESS.toml (empty confirmed, honest), typo-guard integration test.
  189 rules tests pass. Remaining staleness: thin nag-wiring (print at corpus load / digest),
  a confirm CLI command, and the LLM sweep agent (heavy) — consumer/integration layers.
- 2026-10-20: Staleness feature now operator-complete end-to-end: `camerata corpus check`
  (nag) + `camerata corpus confirm <domain> <date>` (advance watermark, validated). Smoke-tested.
  The committed CORPUS_FRESHNESS.toml stays empty/honest (reverted the smoke-test write). Only
  the LLM SWEEP AGENT remains (the finding generator) — deferred: it needs a live model run,
  not suitable for unattended, and is its own focused build.

## OVERNIGHT RUN SUMMARY (2026-10-20 -> 10-21)

All work below is COMMITTED LOCALLY on the branch, green. I cannot push unattended, so
nothing is on origin yet - review `git log` and push in the morning.

### Shipped (green + committed)
1. W25: camerata:allow removed from the product package (fa88f36) + gate tightened.
2. Group 4: 72 mid-word-truncated option ids de-truncated, lockstep refs, generator gate (383ea21).
3. W26 corpus-half: no high+ without fix+citation (155cab1) + effort-tier magnitude gate (7bee157).
4. W32 part (a): CWE metadata schema + cwe on all 14 SEC rules + 2 gates (05d94df). Part (b) dropped.
5. W27 Phase-A unblock: sample-regen fixture reconciled to needs-review-is-held routing (curated 6->5).
6. Corpus staleness feature, END TO END: freshness.rs data+confirm+nag (16 tests) + CORPUS_FRESHNESS.toml
   (honest empty) + typo-guard test + `camerata corpus check`/`confirm` CLI (smoke-tested).
7. Design docs: machine-readable exports (CSV+JSON per sheet + manifest.json), corpus staleness,
   each with a turnkey execution plan. Plan + memory + this log maintained.

### NOT done, and WHY (not blockers - boundaries of safe unattended work)
- **Machine-readable exports + manifest BUILD**: large multi-crate change with real per-sheet
  design (two-table Coverage sheet, per-field JSON key contracts, a new `csv` dep, manifest
  hashing, build_product_zip signature across HTTP+CLI, sample regen, 7 gates). Turnkey plan is
  in docs/design/2026-10-20_machine-readable-exports.md "Execution plan". A focused session.
- **Staleness LLM sweep agent**: needs a live model run; not suitable unattended.
- **W27 PDF template rewrite**: inherently VISUAL (page layout); needs your eye on the rendered
  PDF. The data-layer prerequisite (sample regen) is unblocked.
- **W23 remainder**: LATENT - the cross-tier convention-contradiction path (`any_trace` sees only
  the AI tier) has nothing currently reaching it; fixing it is invasive for zero live benefit.
  Leave recorded until something hits it.
- **W24 remainder (platform-owned-table severity anchor)**: NOT a rule_id-class check like the
  test-coverage anchor; needs the RLS checker to expose "platform-owned table" (auth.*/storage.*)
  as finding metadata + a new Finding field + anchor + fixture - a vertical slice across checks +
  server, too big to land clean blind.
- **W29 (9 consistency rules + clone detector + 18 tests)**: large; focused session.
- **19f grader rubric revision + 20 graded sweep**: live-model + money ($~63) + rubric judgment;
  gated on Track 2 anyway.

### Recommended next session order
Exports+manifest build (turnkey plan ready) -> W24 vertical slice -> W29 -> 19f grader -> 20 sweep.
Plus the gated pre-first-client full staleness sweep before client #1.

## UPDATE (2026-10-21, resumed on "keep going"): Track 2 exports BUILT
- Machine-readable exports + manifest.json: DONE and wired end-to-end into the product ZIP
  (both HTTP export_product + CLI inspect). 8 json/*.json + 8 csv/*.csv + manifest.json
  (file index + sha256 + schema_version + provenance). Each a serializer over the SAME
  partition_rows pass (parity gate: all-findings.json == findings.json). Sample ZIP regenerated
  with the new files. Gates: json/csv file-set + parity + RFC4180 round-trip + manifest integrity.
- Fixed two PRE-EXISTING masked failures surfaced along the way: W32 Rule test-fixture literals
  (server test compile) and integrity_gate_cmd fixtures (empty_input/model_unrecorded). Both
  were from earlier this-session commits; now green.
- Known remaining red: ONE test, scan_tools::live_eslint_detects_a_real_exhaustive_deps_violation
  — the eslint-provisioning residual (W30), environmental (leaky skip-guard on a half-provisioned
  machine), NOT a regression. Needs the W30 eslint tool-manifest version-pinning.

## W30 eslint residual — DIAGNOSED (2026-10-21), not my change, real product bug
The failing test `live_eslint_detects_a_real_exhaustive_deps_violation` was NOT modified this
session (verified git log -L) and is NOT a regression from this session's code. Root cause chain,
reproduced:
1. ESLINT_NPM_PACKAGES (tool_provisioning.rs:336) pins NO versions -> npm install pulls LATEST ->
   this machine had eslint 10.12.0 + eslint-plugin-react-hooks 7.1.1 (major-version drift).
2. Under eslint 10 / react-hooks 7 the bundled flat config cannot register the plugin -> raw
   eslint errors "could not find plugin react-hooks" -> ZERO findings. (Reproduced directly.)
   => This is a PRODUCT bug: a fresh-provisioned machine silently zeroes react-hooks (likely all
      plugin) preview findings in real client scans, not just the test.
3. Pinning the WORKSPACE to eslint 9.39.5 + react-hooks 5.2.0 makes RAW eslint (Camerata's exact
   --config/--format json/--rule/. invocation) produce the finding correctly. So eslint 9 / rh 5
   is the right pin.
4. BUT: even with 9/5 installed + config intact + raw eslint working, the Rust `run_scan_tools`
   path STILL returns findings=[] (eslint IS attempted; no error note). So there is a SECOND
   factor inside run_scan_tools' provision/invoke/parse path, beyond the version drift, still to
   find. NOT chased (context limit).
FIX (W30, focused next session):
  a. Pin ESLINT_NPM_PACKAGES to versioned specs (eslint@^9, eslint-plugin-react-hooks@^5,
     @typescript-eslint/*@^8, eslint-plugin-jest@^28) AND make eslint_is_provisioned version-aware
     (presence-only probe won't force a drifted workspace to re-pin).
  b. Debug why run_scan_tools gets [] while raw eslint at 9/5 gets the finding (candidates:
     eslint_bin resolution, a transient re-provision, file-selection/extension filtering, or the
     JSON parse path). eslint_args + ESLINT_FORMATTER="json" match the working manual run.
  c. NOTE: I left the local workspace at eslint 9.39.5 / react-hooks 5.2.0 (npm --no-save), which
     is better than the drifted 10/7, but it is not pinned anywhere yet.
Do NOT bandaid the test to skip-on-empty: that would hide the real finding-zeroing in client scans.

## W30 eslint residual — FIXED (2026-10-21), both factors
Investigated + fixed per Zach ("as long as it is investigated and fixed"):
1. VERSION DRIFT (first factor): ESLINT_NPM_PACKAGES was unpinned -> npm pulled eslint 10 +
   react-hooks 7 -> bundled flat config can't register the plugin -> zero findings. FIX: pinned
   to eslint@^9 / @typescript-eslint/*@^8 / react-hooks@^5 / jest@^28, and made the provisioning
   probe VERSION-AWARE so a drifted workspace re-provisions (presence-only check left 10/7 in
   place forever). Helpers + scoped-package parsing unit-tested.
2. REPO-SCOPE SYMLINK BUG (second factor, the real surprise): is_in_repo_scope compared a tool's
   reported finding path against repo_root with a raw textual starts_with. Tools canonicalize
   their paths, so on macOS eslint emits /private/var/folders/... while repo_root is the
   symlinked /var/folders/... -> every finding judged "outside the repo" and DROPPED, zeroing
   the whole tier. A real latent product bug for ANY repo under a symlinked path on macOS, not
   just the test. FIX: canonicalize both sides before the containment check, fall back to raw on
   failure (synthetic test paths unchanged).
Result: live_eslint_detects_a_real_exhaustive_deps_violation passes; all scan_tools (73) +
tool_provisioning (31) tests green. Did NOT bandaid the test. Full-suite confirmation pending.

CORRECTION (2026-10-22, UI-REACT-HOOKS-LINT-1 review): "pinned" above meant CARET MAJORS
(`eslint@^9` and friends). That stopped the eslint 10 / react-hooks 7 drift but not minor, patch
or transitive drift, and the graded accuracy tests need repeatable tool behavior. It is now truly
pinned: `ESLINT_NPM_PACKAGES` are exact versions (eslint 9.39.5, @typescript-eslint/parser and
eslint-plugin 8.72.0, eslint-plugin-react-hooks 5.2.0, eslint-plugin-jest 28.14.0, the versions
that were installed and verified), `eslint_packages_installed` compares the exact installed
version (a right-major, wrong-minor workspace re-provisions), and `crates/server/assets/eslint/
package-lock.json` (with the matching `package.json`) is committed and installed with
`npm ci --prefer-offline`, so the transitive tree is fixed too. A test holds pins, manifest and
lockfile together. Falls back to installing the exact pins if the lockfile cannot be read.
Bumping eslint now means editing the three together and the bundled config; an `#[ignore]`d test
(`fresh_eslint_provisioning_installs_the_pinned_tree_from_the_lockfile`) exercises a real fresh
install and needs npm plus the registry or a warm cache.

## MILESTONE (2026-10-21): full workspace suite 100% GREEN
`cargo test --workspace --no-fail-fast` = 5857 passed, 0 failed, 0 compile errors. The one
red test (W30 eslint) is now green by a real two-factor fix, not a bandaid. Also caught+fixed
a missed build_product_zip caller (inspect_export_equivalence integration test) during the
final green sweep. Everything committed + pushed (…4177244). No known red tests remain.
