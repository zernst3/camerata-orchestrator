# Post-V1 roadmap (recorded 2026-10-22)

**V1** = the inspection: the first paid client deliverable, gated by the graded accuracy tests.
Its scope is `docs/plans/2026-10-19_MASTER-SEQUENCE.md` plus the two client-trust blockers (secret
redaction with the export release gate; delete-and-re-fetch client source). Nothing below is V1.

After V1, Camerata keeps developing toward the rest of what Cantus Works offers. Zach's read
(80/20): the pieces below are roughly 80% there; the remaining 20% is fleshed out after V1.

## 0. FIRST V2 item: runtime and framework end-of-life + runtime CVEs (decided 2026-10-24)

V1 ships CISA KEV + EPSS enrichment of dependency findings and the "vulnerability data as of"
stamp. V2 starts with the piece V1 cannot see: vulnerabilities in the RUNTIME itself (a PHP, Node,
Python or nginx CVE, which OSV covers poorly).

- Detect declared runtime and framework versions: Dockerfile `FROM`, `composer.json` `"php"`,
  `.nvmrc`, `package.json` `engines`, `.python-version`, `runtime.txt`, `rust-toolchain`,
  `.tool-versions`, framework versions from lockfiles.
- Flag end-of-life versions from endoflife.date ("PHP 7.2 has received no security fixes since
  2020") and known runtime CVEs from NVD, with KEV marking actively exploited ones.
- Deterministic feeds only; the feed snapshot date goes into provenance like V1's stamp.
- Example motivating case: a PHP runtime CVE exploited at scale within hours of disclosure, on
  sites still running PHP 5.6 and 7.2 (end-of-life alone was already the finding).

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

### 2a. HIGH PRIORITY: the orchestration process inside the console (Zach, 2026-10-27)

Zach's directive: the way Camerata itself is being built (one orchestrator directing tiered
models, adversarial review before every merge) becomes the console's development process, so client
remediation and development work run inside Camerata, under its rules and its write-time gate.
The console was built and has per-step model selection, but it has not been run live end to end
and its original requirements are not written down.

- **Step 1, measure before building.** Write down what the console does today (Fable-led gap
  analysis against this section), run it live on a fixture repo, and list what is missing. Only
  then refine. DONE (analysis, 2026-10-27): `docs/plans/2026-10-27_dev-console-gap-analysis.md`.
  Headline: the console is a tested single-agent "implement one story" pipeline, not the 2a loop;
  the UI never sets a unit's branch, so single-repo runs fall through to the greenfield
  scaffolder; the agent's write-time rule subset is a fixed compile-time set, not the project's
  selected rules; GitHub identity is structural (`OWNER/REPO#N` parsed at run, PR and commit
  sites); no merge step, no decision log, no findings-to-work handoff. Ten ranked gaps, blocking
  first. The live run (section 6 of the analysis) is still to do.
- **The roles, as practised in this repo:**
  - Orchestrator (top tier, today Opus): plans, writes briefs, decides, merges, never edits code
    directly except small fixes.
  - Builder (mid tier, today Sonnet): implements one brief in its own isolated worktree, runs the
    gate, commits, never pushes.
  - Mechanical (cheapest tier, today Haiku): searches, status tables, rote edits.
  - Reviewer (strongest judgment tier, today Fable): adversarial, read-only review of every change
    before merge; findings ranked HIGH / MEDIUM / LOW with a concrete failure scenario; every
    finding fixed or explicitly decided, never silently dropped.
  - Each role's model is a project setting (it already is per step); the defaults follow the tiers.
- **The loop:** brief (with time-box, files owned, files off limits) -> build in a worktree ->
  gate -> adversarial review -> fix round -> merge -> full gate -> push -> remove the worktree.
  Independent units run in parallel up to a machine-aware limit; units that touch the same files
  are sequenced, not parallelised.
- **The decision log:** every decision the orchestrator makes on the client's behalf is written to
  a project decisions log with its reason (as `docs/OVERNIGHT_DECISIONS.md` does here), and only
  irreversible or destructive steps stop for the human.
- **What Camerata adds that this repo does by hand:** the corpus rules and the Layer 1 MCP gate
  block a violating write before it reaches disk; the reviewer is given the rules the change must
  satisfy; the same release checks run before anything leaves.
- **Resource guards:** the lessons from running this at speed: cap parallel local builders, seed
  build folders from a shared clone, clear idle worktrees' build output, keep disk headroom so a
  cleanup job never deletes artifacts a running gate needs, and fail loudly on a flaky test
  rather than retrying it.

### 2b. Units of work without a board (Zach, 2026-10-27)

- Board access (GitHub Issues today) becomes optional. Units of work are created and tracked inside
  Camerata; a board is an optional two-way sync adapter.
- A unit of work carries: its source (inspection finding set, free text, plan), the rules it must
  satisfy, acceptance checks, branch, PR, status.

### 2c. Inspection report -> development console handoff (Zach, 2026-10-27)

- One action turns a delivered inspection (its confirmed findings, after review) into a
  remediation plan of units of work in the console, keeping every link back to the finding, rule,
  file and line, and to the inspection's commit.
- A finding marked false positive or ignored in review never becomes work. Tech debt becomes
  work only when chosen.
- After a fix merges, a delta re-inspection (item 1) confirms the finding is gone, and the unit
  records that evidence.

### 2d. Branch and pull-request strategy for remediation

Zach's inclination: one branch and one PR per violated rule, containing every violation of that
rule, instead of one per finding (findings can number in the hundreds). Opus's view: per rule is
the right default, with four refinements, to be settled when this is built:

1. **Split a rule that is too big for one review.** When one rule has more violations than a
   reviewer can check in one sitting (a threshold, e.g. about 20 files or 400 changed lines),
   split that rule's PR by area (package, service or directory), one PR per area, still one rule
   each.
2. **Group findings that share one fix.** When one change resolves findings under several rules
   (e.g. a missing auth middleware behind three rules), make that one unit with all the rules
   named, so two PRs do not edit the same lines.
3. **Order by severity and dependency.** Critical and high rules first; a rule whose fix changes
   code another rule's fix touches goes first, and the second rebases on it.
4. **Traceability.** Each PR description lists the rule, its standard, and every finding it
   closes (file, line, inspection id), so the client's reviewer can check the PR against the
   report line by line.

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
- **DRY is the governing principle (Zach, 2026-10-22).** Everything is built first for the
  inspection; anything development runs that the inspection also runs must call the EXACT same
  code. No second implementation of any rule, detector, extractor, severity bound or exclusion
  list. One rule, one implementation, many enforcement points. This is what keeps Camerata
  maintainable and consistent: a fix to a rule fixes it everywhere at once.
- **What the V2 audit must establish, per deterministic rule** (the checklist):
  1. Which function implements it for the inspection (crate, module, entry point).
  2. Whether it is decidable from one file (Layer-1 candidate) or needs the whole repo (Layer 2).
  3. Where it runs during development today (L1 arm, L2 checker, L4 CI job, or nowhere).
  4. Whether that dev-time path calls the SAME function as the inspection, or a separate copy
     (a separate copy is a DRY defect: delete it and route through the shared function).
  5. Whether the dev-time path is gated on the project's selected rules (it must be; an unselected
     rule never runs, and a selected rule always runs).
  6. Whether exclusions, test-scope policy, severity bounds and the corpus version match between
     the two paths (they must, by construction, if 4 holds).
  7. Whether the dev-time result is deny-before-write (L1), bounce-after-task (L2), or CI-only,
     and whether that placement is right for the rule (e.g. dead-code: inspection only).
  8. The tests proving parity: the same fixture judged identically by the inspection path and the
     dev-time path.
- **V1 is unaffected:** Zach will not develop through Camerata until all of this is checked, so
  the dev-time gate is not on the V1 critical path.
- **Check first, possible defect:** `TECHNICAL.md` §2 says the verified runtime default subset is
  `["GOV-1"]` "unless configured". If a dev run can reach the gate without the project's selected
  rules (including the security floor) in its subset, that is a defect to fix, not new scope.
  Verify this first; if confirmed, it may belong before V2.

## 4. Mutation testing (measure whether the tests catch bugs, not how many there are)

Zach, 2026-10-22, "at some point". `cargo-mutants` makes small deliberate changes to production
code (flip a condition, delete a line, swap an operator), reruns the tests, and reports which
changes no test caught. On `camerata-checks` that turns "7,000 tests" into "the tests catch N%
of injected faults", a much stronger claim, and the surviving mutants point at the exact gaps.

- Costs no model tokens: it is a CPU job. Slow, so it runs as an unattended overnight job.
- Start with `camerata-checks` (the deterministic checkers, where a silent miss is a false
  negative shipped to a client), then `rules`, then the export path in `server`.
- Disk: it rebuilds repeatedly; run it with the cargo disk guard installed and a dedicated
  `CARGO_TARGET_DIR` that is removed afterwards.
- Output to keep: the caught/missed percentage per crate over time, and the list of surviving
  mutants triaged into "add a test" or "equivalent mutant, ignore with a reason".

## 5. Themes (for fun; Zach, 2026-10-24)

Not needed for V1 or for clients; wanted because developer tools do not have to be boring. The
current look is deliberate: the WWII Bombe (Turing's codebreaking machine), mechanical and
distinctive. Add a theme picker and a second theme: **Windows XP** (Luna-style blue gradient
title bars, Tahoma-style type, bevelled buttons, green accents).

- Precondition: audit `crates/ui/src/style.rs` and components for hard-coded colours, fonts and
  borders, and route every one through theme variables (`--ink`, `--ink-soft`, `--accent`, and
  so on). Then a theme is one set of variable overrides plus a picker in settings, persisted per
  user. Dark mode (UI_BACKLOG) falls out of the same mechanism.
- Recreate the XP look in CSS. Do NOT ship Microsoft assets (the Bliss wallpaper, Windows logos,
  icon sets): they are copyrighted and trademarked.
- Bombe stays the default.
- **Design principle (Zach): a theme is a mood, not a colour swap.** Bombe feels mechanical and
  heavyweight as you work through an inspection. XP should feel nostalgic and optimistic, the
  early-2000s look that grew into the Frutiger Aero era of Vista: nature meets technology,
  saturated, glossy, hopeful. Every element (background, title bars, buttons, highlights,
  accents) serves that one feeling.
- **A Bliss stand-in, original:** an SVG landscape drawn in code (a rolling hill with a bright
  green gradient, a deep-to-pale saturated blue sky, a few soft blurred cumulus clouds), tuned to
  evoke Bliss without copying the photograph; a different hill shape and our own composition.
  Scales to any resolution, a few KB. The palette continues through glossy title bars, rounded
  aqua-style highlights and green/blue accents so the wallpaper and the chrome read as one era.

## 6. Pre-pen-test readiness (Zach, 2026-10-27)

Many rules already predict what a pen tester would hit first (boundary validation, RLS, auth gaps,
secrets), but the corpus does not say which: of 437 rules only 24 carry a CWE id, and `kind`
(defect / decision / default_with_exceptions) describes the rule, not its attack value.

- **No new finding kind or rule kind.** Security relevance is a second axis, not a third kind: a
  missing authorization check is both a defect and a likely pen-test finding, and splitting kinds
  would duplicate rules or force a false choice (one implementation per rule). Instead: a
  security tag on rules (CWE, OWASP Top 10 / ASVS category, attack class) and a DERIVED exposure
  rating on findings.
- **The prediction is reachability, not the rule.** Exposure = security tag + a reachable entry
  point + whether that route needs auth + how sensitive the data it touches is. "Unvalidated id
  on an unauthenticated public route" ranks far above the same pattern in an admin-only batch
  job. Builds on the endpoints the integration checker already extracts.
- **Frame it as readiness, never prediction.** The report keeps its "not a pen test" notice. A
  "predicted pen-test finding" the real pen test does not reproduce reads as a false positive.
  "Fix these before your pen test, ranked by what is reachable" is honest, maps to SOC 2 / PCI
  language buyers already use, and is a sellable offering (pen tests cost $15-40k per round).
- **Expand detection where code can show it:** IDOR (an id-taking route with no ownership
  check), mass assignment, SSRF sinks, missing rate limits on login and password reset, CORS,
  JWT verification settings, debug endpoints left on. State plainly what static analysis cannot
  see (TLS, live headers, business-logic abuse).
- **Ground truth before the report section.** The graded run measures detection, not
  exploitability. Benchmark on intentionally vulnerable apps with published exploit lists
  (OWASP Juice Shop and its challenge list): inspect, then score how many known exploits the
  readiness ranking puts near the top. Later, a real pen-test report on an inspected repo.

Order: (1) security-tag backfill across the corpus (data work, mostly Haiku, Fable spot-check);
(2) ASVS gap analysis and the missing rules; (3) exposure ranking from route reachability;
(4) Juice Shop benchmark; (5) only then the report section.

## 7. PHP rules, mostly Laravel (Zach, 2026-10-27)

No PHP support today (no `php` domain in the corpus, no PHP grammar, no PHP linters in the
registry). Follow the Ruby / Rails shape, which is the closest precedent (`ruby`, `ruby:rails`,
`ruby:testing` domains; rubocop in the linter registry).

- **Engine:** `tree-sitter-php` for extraction and for viewer highlighting (it must match the
  0.25 tree-sitter ABI the other grammars use; check before adding). Composer awareness:
  `composer.json` / `composer.lock` for the framework and version, `composer audit` (Packagist
  advisories) feeding the same dependency findings and KEV/EPSS enrichment as the other
  ecosystems, and PHP + Laravel end-of-life in V2 item 0.
- **Linters to register:** PHPStan with Larastan, Psalm, Laravel Pint / PHP-CS-Fixer, so rules
  can cite real tool checks the way rubocop cops are cited.
- **Domains:** `php`, `php:laravel`, `php:testing` (PHPUnit / Pest).
- **Laravel rule candidates:** mass assignment (`$guarded = []`, unguarded models, `$fillable`
  holding privileged fields); raw SQL (`DB::raw`, `whereRaw`, `selectRaw` with interpolation);
  unescaped Blade output (`{!! !!}`); `APP_DEBUG=true` or debug tooling in production config;
  `env()` called outside `config/` (breaks config caching); validation in FormRequests at the
  boundary; authorization via Policies / Gates, and route model binding without ownership
  scoping (IDOR); CSRF exemptions in `VerifyCsrfToken::$except`; N+1 Eloquent access without
  eager loading; reversible migrations; idempotent queued jobs; secrets committed in `.env`.
- **Ties to item 6:** mass assignment, unescaped Blade, debug mode and unscoped route binding are
  classic pen-test findings, so tag them for security from the start.
- **Benchmark:** a public Laravel app plus an intentionally vulnerable PHP app (DVWA) for recall,
  before claiming PHP coverage in a proposal.

## 8. Client CI gate refinement (Zach, 2026-10-27)

What exists: `camerata-check` (crates/camerata-check), the standalone deterministic checker
runner, plus the generated `.github/workflows/camerata-gates.yml` and `.camerata/checks.toml`
committed to the client's branch from the Rules view ("Regenerate CI workflow"). V1 only adds
the severity floor (low-severity consistency findings advisory, medium and above fail; one
constant; `cloud/item21`). Zach: it exists but warrants refinement. Candidates, to be scoped
with Zach before building:

- **Baseline / ratchet:** a client adopting the gate on an existing codebase fails on day one
  for every pre-existing finding. Record a baseline at install (the inspection's findings) and
  fail only on NEW findings, with the baseline shrinking as findings are fixed (never growing
  silently).
- **Per-client policy:** the severity floor and which rules block versus advise, set per project
  and written into `checks.toml`, following the `stages` target design
  (`docs/design/2026-10-21_rule-stages-target-design.md`), instead of one global constant.
- **Pull-request ergonomics:** check only changed files on PRs (full run on the default branch),
  SARIF output so findings show as GitHub code-scanning annotations on the diff, and a short
  job summary.
- **Packaging:** a published GitHub Action (and a prebuilt binary) instead of building
  `camerata-check` in the client's CI, so the job is fast and pinned to a version; GitLab CI and
  Bitbucket templates if a client needs them.
- **Corpus pinning and drift:** the workflow pins the corpus version the client agreed to;
  upgrading is an explicit PR with a changelog of rule changes, tied to the governance retainer
  (item 1).
- **Waivers:** an inline or file-based waiver with a reason and an expiry date, reported in the
  monthly governance delta, so the gate never pushes teams to delete checks.
- **Parity check:** the gate, the Layer-2 dev loop and the inspection must agree on the same code
  (item 3); add a test that runs all three on one fixture.

## Keep V1 forward-compatible (no extra V1 work)

When V1 work touches the project data model (the deletion record, re-fetch, provenance), store
per-inspection facts (commit, date, package) in a shape that can become one entry of the
inspection history above, rather than a single "last scan" slot. Not a V1 feature; just do not
paint the data model into a corner.
