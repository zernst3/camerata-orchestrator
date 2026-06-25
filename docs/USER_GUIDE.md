# Camerata — end-to-end user guide

This is the "I have a repo new to Camerata — where do I start, and how do I take it all
the way through?" walkthrough, and the canonical source the **in-app assistant** (the chat bubble) answers from. Keep it accurate to what's actually shipped — an
assistant that describes a feature that doesn't exist undercuts the whole point.

> Status (updated 2026-06-23): the brownfield **onboarding flow is built and live** — per-repo
> stack detection + rule selection, **custom rules** (per-repo + project-global), an optional code
> audit you can scope with a **scan-type selector** (AI review and/or deterministic scans), four scan
> modes, an opt-in **thorough-calibration** consensus pass, a **scan-time deterministic preview** that
> runs your selected mechanical linters during the scan, a three-table finding triage, and an **Apply**
> step that writes governance onto a local branch and pushes it (no PR until you ask). Two opt-in
> **CI/CD security rules** (Semgrep, CodeQL) are available but **never auto-recommended or pre-checked**.
> Re-onboarding an already-onboarded repo is **blocked**. Projects are **exportable/importable**, repos
> resolve to **local folders** with a health check, and onboarding state **auto-saves**. Governed
> Development adds a project-settings **gear popup** (loop guard + tier-map + per-project step-model
> config + **stall thresholds**), **blank UoWs you author with AI**, an **AI-assisted Update-branch**
> control, a work-item modal with comments + @-mention autocomplete, a one-time **layer-2 bootstrap
> bypass** for installing tooling, a one-click **Gate self-check** (go/no-go), **multiple concurrent
> UoWs** (each runs in its own isolated git worktree), **PR lifecycle buttons** per UoW (push, open PR,
> pull PR info, resolve with agent), and **structured clarifications** that auto-save at pause points.
> Dev runs and onboarding scans show **run liveness**: an amber **stall warning** appears when a run
> makes no progress for the watched threshold, and a **Stop button** is always available to cancel any
> running dev run or scan at any time (the run ends in a **Cancelled** state). Scan findings now
> include a **Test badge** for test-scope violations, a separate **Scan coverage** section (tools that
> didn't run), and scan tools (Semgrep/ESLint) **auto-install on first use**. The in-app assistant
> retains **conversation context** across messages and is grounded on your active project and pulled
> issues. A persistent **token usage meter** tracks 5-hour and session-wide spend. The **check
> manifest** (`.camerata/checks.toml`) is the single source of truth for custom deterministic gates:
> one entry wires a check into BOTH the in-loop dev gate and the generated CI workflow. The two things
> you connect are a **GitHub token** and **Claude** (the `claude` CLI).

---

## 0. The two connections (the only setup)

1. **GitHub** — a token via environment variable. One token serves every repo it can reach:
   ```bash
   export CAMERATA_GITHUB_TOKEN=github_pat_xxx      # Contents + PR write to apply/open PRs; Issues R/W for tickets
   ```
2. **Claude** — the `claude` CLI on your PATH, logged in (subscription) or with `ANTHROPIC_API_KEY`
   set (metered API). This is what the audit and the governed fleet run as. Set
   `CAMERATA_LIVE_BUILD=1` to run a real `claude -p` fleet for governed dev work.

Launch:
```bash
CAMERATA_GITHUB_TOKEN=github_pat_xxx cargo run -p camerata-ui
```
The desktop app opens with its embedded server; the topbar shows the live connection.

**Local-first:** Camerata stores only configs + pointers (JSON in your OS app-data dir —
`projects.json`, `settings.json`, `onboarding-draft.json`); your repo code lives on your own
machine. Each repo a project references must resolve to a local git checkout (see §5).

---

## 1. Projects (the container for everything)

A **project** holds its repos, ruleset, and onboarded state. From the **Projects** home you can:
- **Create** a project (just a name to start).
- **Open** one (the cockpit's four+1 views only appear inside a project).
- **Export** the open project — a single, **path-free** `camerata-project-<name>.json` containing
  only that project's repos (`owner/repo`), ruleset, and which repos are onboarded. It does **not**
  include local paths or your workspace settings.
- **Import** a project config — it **upserts** into your local projects. If you already have a
  project with the same name, Camerata **warns before overwriting**. Your own workspace settings are
  never touched. After import the repos have no local paths yet — resolve them in the Rules view (§5).

**Project config vs. project data:** the exported JSON contains only the project's transferable
config (repos, ruleset, onboarded state, tier map). Units of Work, the story spine, onboarding
drafts, and local repo paths are **local to each developer** and are never included in an export.
UoWs represent in-progress dev-lifecycle state (branch, stage, run history, decisions, sign-off) and
are machine-local — transferring them would produce overlapping work across developers. When you
import a project on a new machine you start with no UoWs; they accumulate as you work.

---

## 2. The cockpit views

Inside a project the nav shows: **Onboard repos · Governed Development · Rules · Routines ·
Repository Workspace · Docs**.
- **Onboard repos** — bring a repo under governance (§3).
- **Governed Development** — the work control surface: pull work items from a tracker (or author a new
  story from a blank UoW with AI), create a Unit of Work (UoW) from one, then run governed development
  on it with the human↔AI clarify loop, comment back, and sign off (§6). A project-settings **gear
  popup** at the top holds the loop guard + default tier-map + per-step model config.
- **Rules** — manage the project's ruleset after onboarding + the repo-path health check (§4, §5).
- **Routines** — schedule governed runs.
- **Repository Workspace** — the local clones: clone status, branch, and ship (push + PR) for dev work.
- **Docs** — the in-app documentation viewer (this guide and the technical reference).

---

## 3. Onboarding a repo (the main flow)

Open **Onboard repos**. The flow: **scan → pick rules → apply → (optional audit → triage → CI step)
→ onboarded.** Onboarding state **auto-saves** continuously, so you can quit and reopen without
re-scanning (a fresh scan starts a new session; a crash mid-scan just re-runs the scan).

1. **Point at the repo(s)** — **browse to each repo's local folder.** Onboarding is **local-first**:
   it reads the code on disk, so a repo must already be cloned locally. Camerata derives the
   `owner/repo` from the folder's git origin (used later only for push / PR) and **records the local
   path**, which makes it a workspace repo immediately — "add a repo to onboard" and "add a repo to
   the workspace" are the same act. **No GitHub connection is needed to onboard**; a token is only
   used later, to push the governance branch and open a PR.

   **Already-onboarded guard:** if you point at a repo this project has already onboarded, Camerata
   **refuses with an error** rather than silently re-running. Onboarding is a one-time act per repo;
   to change a repo's rules after the fact, edit them in the **Rules** view (§4), don't re-onboard.
2. **Scan + propose per-repo rules** — Camerata reads each repo's **local working tree** (it never
   downloads code from GitHub) and detects its stack: languages from extensions, frameworks from
   manifests, **IaC** (Terraform, Terragrunt, Bicep, Pulumi, CloudFormation) and **CI/CD** (GitHub
   Actions, GitLab CI, CircleCI, Azure Pipelines, Travis, Bitbucket, Drone, Jenkins). It proposes a
   starter ruleset **per repo**.

   **The scan follows your `.gitignore`** (gitignore-aware walking). For git repos, Camerata walks
   the working tree with ripgrep's gitignore engine, which honours `.gitignore` at the root and in
   subdirectories, `.git/info/exclude`, and your global gitignore. A file that is gitignored is
   **skipped** — no findings are generated for it. A file that is **committed to the repo** (not
   gitignored) is **still scanned**, even if it has a sensitive name like `.env`. This is deliberate:
   a committed `.env` with real credentials is an actual leak because it is version-controlled and
   visible to anyone with repo access. The scan is enforcing what git sees, not what the filename
   suggests. For directories without `.git`, Camerata falls back to its built-in noise denylist
   (`node_modules`, `target`, build/cache/generated dirs, lockfiles, …).
3. **Pick rules** — each repo has its **own** recommended-rule table; a repo single-select switches
   which repo you're editing. Selection is **per repo**: a rule ticked for repo A is bound to A only.
   **Project-level rules** apply to every repo. Click any rule to read its decision question, the
   options, the default, and each option's rationale, and to choose an alternative. **Option choices
   are also per repo** — adopting an alternative for a rule while viewing one repo does not change
   another repo's choice. A recommended rule that still needs an alternative chosen is **highlighted
   amber and blocks Audit / Add-rules** until you pick one (or deselect it).

   **Opt-in CI security rules are never pre-checked.** The two security-scan rules
   (`CICD-SEMGREP-SECURITY-SCAN-1`, `CICD-CODEQL-SECURITY-SCAN-1`) appear in the list as
   **"Available"** (no recommended badge, no pre-checked checkbox). You opt in deliberately. See §3
   step 6 for details.

4. **Audit (optional) + triage** — optionally scan the existing code to surface violations. Each repo
   is scanned only against **its own selected rules** plus the always-on **deterministic security
   floor** (hardcoded secrets, raw-SQL concatenation, secrets in URLs, private-key blocks, vendor
   credential tokens, secret-bearing file paths, TLS verification disabled — ranked Critical, free +
   instant).

   **Scan-type selector — what to run.** At audit-start two checkboxes decide which passes run, both
   **on by default** (today's behavior):
   - **AI architectural review** — the LLM scan of architectural/structured/prose rules (and the deep
     tier). Unticking it skips **every** model call: **no tokens** are spent.
   - **Deterministic scans (floor + linters)** — the always-on security floor **plus** the scan-time
     mechanical preview (below). Local, **no LLM, no tokens**.

   You need at least one ticked; if you untick both, Camerata runs both rather than nothing. **Pick
   deterministic-only** for a fast, free pass (it's also the cleanest way to sanity-check the linter
   findings); **pick AI-only** for just the judgment review. The deep-report toggle is hidden when AI
   review is off (the deep tier is itself an LLM pass). The scan-mode picker below (Parallel /
   Sequential / Background / Batch) is unchanged.

   **Three kinds of finding, and the difference matters:**
   - **Deterministic floor** (`SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`,
     `ARCH-NO-SECRETS-IN-URL-1`, `SEC-NO-PRIVATE-KEY-1`, `SEC-NO-VENDOR-TOKEN-1`,
     `SEC-NO-SECRET-FILE-1`, `SEC-NO-DISABLED-TLS-1`) — pure regex/logic, no LLM. **Repeatable**
     (same code → same result, same rule-id, same line), and these are the exact checks the layer-1
     gate enforces on new writes. Treat their rule-ids as **stable/canonical**. In the triage table
     they carry a green **"Rule · enforced"** badge.
   - **Deterministic preview** — findings from the **scan-time preview** (below): your selected
     mechanical rules' own linters, run by Camerata during the scan. **Deterministic** (stable
     tool rule-ids) but **advisory** — they are **not enforced until the CI story wires them**. They
     carry a purple **"Preview · not enforced until wired"** badge.
   - **AI-suggested (architectural)** — model-inferred issues regex can't catch (layering violations,
     N+1, missing auth on writes, god objects, GET-with-side-effects). These are **advisory**, and the
     model **invents the rule-id** per finding, so the id, severity, and exact set can **vary run to
     run**. Read them as "the model flagged this pattern," not as a fixed rule. They carry a blue
     **"AI · advisory"** badge. The calibration pass recalibrates severity and flags low-confidence ones
     but never drops any — you make the final call.

   The triage table's **Authority** column shows these three tiers and is **filterable** (enforced /
   preview / advisory); the CSV export carries the `preview` / `preview_tool` columns.

   **Test badge, Needs-review state, and Self-referential findings.** Three extra flags appear in the
   finding table:
   - **Test badge** (yellow) — the finding is in test code (a test file path, or a line inside a
     `#[cfg(test)]` block). Test-scope violations are down-ranked to low severity. The nuance: a real
     secret in production code in the same file stays Critical even if the file also contains a test
     block — classification is per-finding-by-line, not per-file.
   - **Needs review** (orange) — the calibration pass flagged this finding as uncertain; read the
     reason and decide yourself.
   - **Self-referential** (gray badge, status `suppressed-self-reference`) — Camerata's own rule
     descriptions contain the very patterns it looks for. When the scanner reads a governed repo's
     `CONVENTIONS.md` or `AGENTS.md` and encounters a rule directive such as "Do not set
     `verify=False` in TLS configuration," it would otherwise flag that line as a TLS-verification
     finding. Findings like this are marked Self-referential and do NOT count toward the active
     violation tally. They are visible in the report so you can confirm they are noise — but they do
     not drive the gate, and they do not contribute to the CI gate count.

     **The safety property:** this suppression applies ONLY when BOTH the file is a Camerata-emitted
     governance artifact (`AGENTS.md`/`CONVENTIONS.md` with the Camerata header, a `.camerata/` file,
     or a corpus TOML) AND the matched snippet is traceable to a rule's description text. A real
     credential pasted into `CONVENTIONS.md` that is not part of any rule description still flags as
     `active`. Ordinary source files (`app/config.py`, `src/main.rs`, etc.) are never suppressed
     by this mechanism regardless of what text appears in them.

   **Scan coverage section.** Below the violations table, a separate **"Scan coverage"** section
   lists tools that didn't run (missing binary, unrouted rule, etc.) as informational notes. These
   are **not violations** — they tell you where coverage has a gap, so you know what the scan did
   and didn't check.

   **Scan tools auto-install on first use.** Camerata auto-provisions Semgrep and ESLint into its
   own cache directory (`~/Library/Application Support/camerata/tooling/` on macOS;
   `~/.local/share/camerata/tooling/` on Linux) the first time a scan needs them. The bundled
   Semgrep ruleset runs fully offline after the one-time install. If `python3` or `npm` is not on
   your PATH, Camerata degrades gracefully — you get a coverage note explaining the gap, and the
   rest of the scan continues unaffected.

   Pick the **model** and the **scan mode** (four options; Camerata auto-selects a recommended one by
   the codebase's size):
   - **Parallel** (default) — runs rule-batches concurrently; fastest. Wall-clock is the slowest
     batch, not the sum of every call.
   - **Sequential (slower, gentlest)** — one chunk at a time, all rules together; slower but gentlest
     on rate limits (a fallback when Parallel hits throttling).
   - **Background job (walk away)** — the Parallel scan, but it runs server-side and detached: you get
     a progress view and can walk away while findings stream in. Best for huge / multi-repo scans
     where a foreground scan would tie up the page. (Parallel and Sequential run in the foreground and
     block until they finish; Background job is the same work, just detached.)
   - **Batch (50% off — async, API key required)** — submits all passes as a single **Anthropic
     Message Batch**, for a flat **50% discount on all scan tokens**. Requires `ANTHROPIC_API_KEY` and
     the `api` backend; results arrive asynchronously (seconds to minutes, up to 24h on very large
     scans). Best when latency is acceptable in exchange for cost.

   **Thorough calibration (opt-in checkbox).** Off by default. When ticked, the calibration pass
   that recalibrates AI-suggested severities runs as a **multi-vote consensus** instead of a single
   pass: each finding is judged several times and the **conservative** verdict wins on disagreement
   (a finding two votes call real and one calls noise stays). It catches more borderline cases and
   reduces flaky severity, **at the cost of more AI calls** — so the **pre-scan cost estimate rises
   (~3× the calibration portion)** the moment you tick it. Leave it off for a quick pass; turn it on
   when you want the audit's severities to be trustworthy enough to act on directly. It never drops
   findings either way — it only re-ranks and flags.
   Findings land in three tables you switch between: **Unresolved · Ignored · Tech debt** — select and
   **Ignore (with reason)** or **Save as tech debt**, re-bucket freely. A **Needs-review** column shows
   the calibration pass's flag + reason (and the Test badge for test-scope findings). In the Tech-debt
   table mark items **resolve later** or **resolve now**, then **Process**: ignores become baseline
   waivers, and **every tech-debt item is filed as a GitHub issue** (the story). Resolve-now issues
   are titled for pickup by the dev engine (the actual dev work is Pillar 2; onboarding only *writes
   the story*). There is no separate "fix the findings" button — fixing a finding is just its
   resolve-now story flowing into the dev layer. Triage/Process is **not required** to finish
   onboarding.

   **Scan-time deterministic preview.** For each **mechanical** rule you selected that maps to a tool
   Camerata can drive (clippy, ruff, eslint, semgrep), the deterministic scan **runs that tool itself**
   with a Camerata-supplied config and folds the results into triage as **preview findings** — even if
   the rule isn't wired into the repo yet. You select it, you see findings. A preview is **indicative,
   not enforcement**: it uses Camerata's tool version (which may differ from what the repo eventually
   pins), and the CI story still has to wire the rule for the gate to block on it. Honest stance: a
   missing tool, an unparseable result, or a linter Camerata doesn't drive end-to-end
   (golangci-lint, rubocop, Checkstyle, Roslyn, …) yields a note in the **Scan coverage** section,
   never a false clean. Mechanical rules **stay out of the AI/LLM review** — a deterministic tool
   runs them instead, which saves tokens. **Architectural rules are never attempted in the preview**;
   they need a bespoke checker and are covered by the AI review instead.

   **Excluded from the preview by design:** **CodeQL** and the **paid cloud tiers** (`layer3_only` —
   too heavy a whole-program build to run locally) never preview; they are CI-story-only (step 6).
   The scan header still shows how many rules were excluded from the AI scan for being mechanical.

   **Deterministic-scan progress.** A **"Deterministic scan" progress indicator** renders **above the
   AI agent-activity drawer**, with an overall done/total bar plus a per-tool row (the floor and each
   preview linter: starting → running → done, with a findings count). It's the primary progress view
   in deterministic-only mode, where the AI drawer is empty.

   **Stop button and stall warning during the scan.** A **Stop** button is always visible while the
   scan is running — you do not have to wait for a stall to stop it. Clicking Stop ends the scan in a
   **Cancelled** state. Separately, if the scan makes no progress for approximately 2 minutes, an
   amber **"No progress — possible stall"** warning appears above the progress indicator. The warning
   is informational; the scan continues until you click Stop or it finishes normally.

5. **Add rules to repo(s)** — writes the governance files onto a `camerata/onboard-governance` branch
   in each repo's **local clone AND pushes that branch to origin — no pull request is opened.** Each
   repo's local clone is resolved from its recorded path (or, as a fallback, a workspace folder). The
   branch is Camerata-managed and regenerated each run (force-pushed), so re-applying is safe. Edit
   the working copy freely, then click **Open governance PR** (a separate, optional button) when ready
   — **Camerata never opens a PR automatically.** **Applying marks the repo onboarded.**

   The files written on apply:

   | File | What it contains |
   |---|---|
   | `AGENTS.md` | Prose rules — the agent's in-context directives. |
   | `CONVENTIONS.md` | Structured, mechanical, and architectural rules — conventions + CI conformance notes. |
   | `.camerata/rules.json` | Armed rule ids — the gate config read by Layer 1. |
   | `.camerata/baseline.json` | Accepted pre-existing debt (the full active-finding set at apply time). |
   | **`.camerata/checks.toml`** | **The SSOT check manifest** read by both Layer 2 (dev-loop runner) and Layer 3 (CI). Each applied CI-tier rule becomes one `[[check]]` entry; mechanical rules carry a concrete command, architectural rules carry a TODO placeholder for the team to fill in. This is the single file you edit to add, remove, or change a custom gate check — one edit covers both the dev loop and CI. |
   | **`.github/workflows/camerata-gates.yml`** | **The generated CI workflow** — the real Layer-3 CI gate, not a placeholder. It is derived directly from `.camerata/checks.toml`, so it is always consistent with the manifest. Regenerate it any time by clicking **Regenerate CI workflow** in the Rules view. |

   The apply loop is now closed end-to-end: apply writes `.camerata/checks.toml` → Layer 2 reads it →
   Layer 3 is generated from it, all from the same file. See §14 for the full SSOT picture.

6. **Wire CI rules (two separate stories)** — the final step files **two GitHub-issue stories** per
   repo, one per CI enforcement tier:
   - **Create mechanical-rules CI story** — wire the selected **mechanical** rules into that repo's CI
     as enforced lint gates. Mechanical rules map to an existing off-the-shelf linter, so the
     implementation is straightforward: fill in a manifest entry for each rule and you're done.
   - **Create architectural-rules CI story** — wire the selected **architectural** rules into CI.
     Architectural rules need a **custom checker** (no off-the-shelf linter expresses them, e.g.
     "handlers never touch the DB directly") plus team design and scoping before implementing.

   Each story body is **self-sufficient**: it carries a full explanation of the `.camerata/checks.toml`
   single source of truth, the manifest schema with all fields, and (for architectural rules) a
   step-by-step how-to with a worked example. A developer picking up either story has everything they
   need with no additional hand-holding. See §14 for the full SSOT picture.

   Both stories are filed separately so the mechanical track (easy, done in a single sprint) is never
   blocked on the architectural design phase.

   **CI-wiring covers both gate layers, not just CI.** Each story instructs wiring the check into
   `.camerata/checks.toml`, which the Layer-2 dev-loop runner AND the Layer-3 CI workflow both read.
   One entry serves both; there is no "wire it twice" step.

   **Optional CI security rules (opt-in, never auto-recommended).** Two CI/CD-domain rules can
   generate their own security-scan CI stories. They are **never pre-checked and never badged as
   recommended** during onboarding — you opt in deliberately:
   - **`CICD-SEMGREP-SECURITY-SCAN-1`** — **Community Edition** (free, LGPL-2.1 OSS CLI; runs on any
     repo public or private; single-file, light enough for the scan preview, layer-2, and CI) **vs.**
     **AppSec Platform / Pro** (paid, cross-file taint analysis + the Pro rule set; CI / platform
     tier).
   - **`CICD-CODEQL-SECURITY-SCAN-1`** — **public-repo (free)** **vs.** **GitHub Advanced Security
     (paid, per active committer, for private repos)**. CodeQL's free entitlement is **public/open-
     source repos only**; private code requires the paid GHAS license. Either way its whole-program
     database build is heavy, so CodeQL is **CI / layer-3 ONLY** — it never runs at the scan preview
     or in the dev loop (which is also why CodeQL never appears in the scan-time preview).

   Both rules have **no default option** — selecting one immediately shows the amber "must choose"
   state until you pick a tier explicitly.

**Greenfield (a new repo):** name → pick starter ruleset → scaffold the repo with the rules baked in
from commit zero.

Design rationale: [`decisions/2026-06-15_brownfield_onboarding_flow.md`](decisions/2026-06-15_brownfield_onboarding_flow.md).

---

## 4. The Rules view (manage the ruleset)

Two tables:
- **Project rules** — the rules the project has selected, in one table, **filterable by repo** (a repo
  single-select), with project-level rules shown too. Click a rule to switch its chosen option; remove
  a rule from a repo. Edits persist to the project's ruleset.
- **All rules** — the full corpus, viewable even when unassigned. Each row shows **which repos it's
  applied to**, with **"Add to repo"** (add a rule to any repo it's not yet on — directly here) and a
  jump to the project-rules table for editing.

The Rules view also hosts re-emit, suppressions, custom rules, and the repo-path **health check** (§5).

**Opt-in rules are not pre-checked.** Rules with the `opt_in_only` flag (currently the two security-
scan rules) appear in rule tables as **"Available"** only — no pre-checked checkbox, no recommended
badge. You opt in deliberately by checking them.

**Custom rules.** Beyond the built-in corpus you can author your own rules in two scopes:
- **Custom** — a rule that applies to a single repo (it joins that repo's selected set).
- **Custom Global** — a project-level rule that applies to every repo, alongside the built-in
  project-level rules.

A custom rule is a **free-text directive**, not a corpus-shaped rule: it carries only a short
**name**, a **directive body**, and its **scope** (the domain it routes to). It has none of the
corpus decision/options/enforcement-kind shape, so there is no alternative to choose and no amber
needs-choice gate — it simply emits as a `### CUSTOM-{name}` block alongside the selected rules. You
can **create, edit, and delete** custom rules from the Rules view; deleting one removes it from any
repo it was on.

**In practice a custom rule is always a prose or structured rule** — an advisory directive the agent
follows and a human reviews. It can never be `mechanical` or `architectural`, because Camerata has no
off-the-shelf linter mapping or bespoke checker for a rule you just invented. To make a custom rule
deterministically enforced, you would open a story / development task to build that enforcement (a
linter mapping or a custom AST checker); until then it is guidance, not a gate. (See §13.)

---

## 5. Repo paths (resolution + health)

Each repo resolves to a **local folder** — repos can live anywhere; the path is machine-local and is
**never exported**. The Rules view runs a continuous **health check**: any repo that doesn't point at
a valid local git checkout (common right after importing a project) is flagged with a warning + a
per-repo **Resolve…** button to browse to its folder. A repo is "broken" when its path is unset,
missing, or not a git checkout whose origin matches `owner/repo`.

---

## 6. Governed Development (work items → Units of Work → governed dev)

The **Governed Development** view is built around two objects:
- A **WorkItem** is the requirement/story pulled from a tracker (the normalized model). Today the
  provider is **GitHub Issues**; the WorkItem model is provider-agnostic, and Jira / Azure DevOps /
  GitHub Projects are **planned per-provider adapters, not yet shipped**.
- A **Unit of Work (UoW)** is the dev lifecycle that references a WorkItem.

### Project settings (the gear popup)

A small **Settings** button (gear icon) sits at the top of the Governed Development left nav and is
always visible regardless of which UoW is selected. It opens a popup holding the **project-wide**
settings:
- **Loop guard** — the maximum number of revise iterations a governed run may take before it stops.
- **Default tier-map** — the project's default Fast / Balanced / Strongest model ids.
- **Step models** — the AI model to use for each non-fleet step (audit, calibration, research chat,
  story authoring, decomposition, escalation, clarification). Each step has its own model selector;
  they default to `claude-sonnet-4-6` when a project is created. Per-project isolation: a change to
  project A never touches project B's step models. See §16 for more detail.
- **Stall thresholds** — two numeric fields (in seconds) that control how long a run can be idle
  before Camerata considers it stalled:
  - **Watched (interactive)** — default 120 s. Applies to dev runs you are actively watching. On
    stall, an amber warning appears in the run panel; the run keeps going and you decide what to do.
  - **Routine (autonomous)** — default 600 s. Applies to walk-away autonomous runs (scheduled
    routines). On stall, the run is **auto-cancelled** and transitions to **Failed** with the stall
    reason recorded — the failure reason is the operator signal for an unattended job. Two separate
    thresholds exist because a human-watched run warrants a shorter patience window, while a walk-away
    routine warrants more room and should fail explicitly rather than hang indefinitely. Both values
    must be positive integers greater than zero; saving zero is blocked.

These are project defaults, not per-UoW knobs. (The tier-map is also still editable in the Rules view
for discoverability; both surfaces save to the same project row.) Per-run model overrides stay on the
UoW card — they default *from* this tier-map but override only that one run.

### Issue Management — pull work items

At the top of the view, an **Issue Management** panel shows the GitHub connection status (`● GitHub
connected`, or a no-token notice). Click **Pull work items** to do a **manual** pull (there is **no
auto-poll**) that pulls **all open issues across every repo in the active project** into a
**WorkItem table with a Repo column**. Click any row to read the full work item (title, body, state,
labels, and an **Open issue ↗** link to the source).

### Create a Unit of Work from a work item

From a work item's detail, click **Create Unit of Work from this issue**. This is **deduped by
external reference**: if a UoW already exists for that item the button reads **Open Unit of Work**
and selects the existing one instead of making a duplicate.

### Author a story from a blank UoW with AI

You don't have to start from an existing issue. The left nav's **New authored story** button creates a
**blank draft UoW** and opens an **authoring panel**:
- A **clarification chat** — describe what you want; the AI drafts an issue title + body and, when the
  requirements are ambiguous, **asks one clarifying question** back (as a structured question with
  options and benefits/drawbacks — see Structured clarifications below). Keep chatting to refine it.
- A **live draft preview** of the title and body as they take shape.
- A **target-repo picker** (the active project's repos) and a **Push to board & link** button.

Clicking **Push to board & link** opens the story as a **GitHub issue** in the chosen repo and **links**
the draft UoW to it; the draft then becomes a normal linked UoW and the standard dev controls take
over. This path is **LLM text generation only** — it drafts an issue, it does **not** write code — so
the governance gate isn't involved here (same class as the chat assistant); the gate stays on the
governed dev run after the UoW is linked. Pushing to the board needs a GitHub token; without a token
(or with Claude unavailable) the chat still saves your turns and tells you AI drafting is unavailable
rather than failing.

### Multiple concurrent Units of Work

You can run **multiple Units of Work at once** — even on the same repo. Each UoW operates in its own
**isolated git worktree** (a separate working directory off the repo's shared `.git` object store),
keyed by its branch. Worktrees live at `<clone>/.camerata-worktrees/<branch-name>`.

What this means in practice: two UoWs on the same repo can run development, update-branch, and ship
operations simultaneously without git checkout conflicts. The gate and all governance rules are
unchanged — worktrees change WHERE the agent works, not WHETHER it's gated. Two UoWs editing the
same lines still produce a normal merge conflict at PR/merge time (expected, resolved at merge).

Worktrees are cleaned up automatically on sign-off. A startup sweep reclaims any that leaked through
crashes. A **disk headroom guard** (default: requires 10 GB free; override with
`CAMERATA_MIN_DISK_HEADROOM_GB`) refuses to create a new worktree if disk space is low — this
protects against the `target/` multiplier when developing a large Rust project with several concurrent
UoWs.

### Structured clarifications (auto-saved, resumable)

Whenever Camerata or the AI needs input from you — during story authoring, investigation, and other
lifecycle phases — it presents a **structured question**: multiple options each with a short
benefit/drawback description, an "Other" free-text escape, and optional multi-select. This mirrors
the `AskUserQuestion` design: you pick from concrete options rather than typing free-text into a chat
box.

Everything is **auto-saved**: open questions and your answers survive a restart. You can close the
app, come back later, and resume exactly where you left off. The cross-UoW **"Needs you" queue** in
the Governed Development view lists every open question across all stories, so you never miss a
waiting pause point.

The investigation phase can **pause mid-run** when the agent raises a question, park the run at
"Awaiting clarification," and **resume** (re-spawn the gated agent with the Q+A in context) once you
answer. Dev-phase mid-write pause/resume is planned but not yet shipped.

### The UoW dev controls

Below the table is a list of **UoW cards**. Open one to get the governed dev controls.

**All AI runs are step-bound on the UoW lifecycle** — there is no standalone "run" button separate
from the lifecycle strip. Each phase of the lifecycle has its own control, shown inline with that
step. The lifecycle stages are:

> **Intake → Investigating → Decisions Approved → Development → Awaiting QA → Signed Off**

The lifecycle strip shows the stages as a progress bar with the current stage highlighted. The
control for the active phase renders directly beneath it.

#### Intake: Begin investigation

At the **Intake** stage, a single **model select** and a **▶ Begin investigation** button appear.

- The model defaults to the active project's strongest tier; you can change it for this run.
- Clicking the button runs a **single gated investigation agent** that reads the issue/story,
  surfaces decisions and tradeoffs, and records an investigation note onto the UoW. The stage
  advances to **Investigating** as the run begins.
- Without `CAMERATA_LIVE_BUILD=1`, the investigation run completes with a placeholder note; with
  it set and `claude` connected, a real `claude -p` investigation agent runs.

#### Investigating: Approve decisions

At the **Investigating** stage, the architect reviews the investigation note and the decisions the
agent surfaced. When all decision records are approved, click **Approve decisions** to advance the
stage to **Decisions Approved**. The server enforces this gate: the development run is blocked until
every decision record is marked approved (a `409` is returned if you try to skip ahead).

#### Decisions Approved: Run development (governed)

At the **Decisions Approved** stage, three per-tier model selects appear — **Strongest**, **Balanced**,
and **Fast** — each defaulting from the active project's tier map and editable for this run without
changing the saved project defaults. Click **▶ Run development (governed)** to start the build.

**Brownfield vs. greenfield.** When the UoW's repo has a local clone (the normal case for a real
story), the agent edits the **existing codebase in-place on the UoW's branch** in its isolated
worktree. When there is no local clone yet, the runner scaffolds a new app from the plan in a
temporary directory (greenfield). Camerata picks the path automatically — you see the same controls
either way.

**How the tiered run works:** the **Strongest-tier agent is the orchestrator and lead.** It does the
complex, one-way-door work itself. For well-scoped simpler subtasks it can use the governed
`mcp__camerata__delegate` tool to hand the task to the Balanced or Fast tier. The gate stays
universal across all tiers: every delegate child is spawned gated (`gated_write` only, `Task`
disallowed), delegation is only one level deep (children cannot re-delegate), and escalation is
parent-driven (a child returns an `INCOMPLETE:` signal and the orchestrator re-handles it). The raw
`Task` tool stays disallowed for every agent — delegation goes through `delegate`, not `Task`.

Without `CAMERATA_LIVE_BUILD=1` the run is token-free/scripted and the gate enforcement is still
real. With it set and `claude` connected, a real multi-tier `claude -p` fleet runs.

**Bootstrap run — skip layer-2 checks (the chicken-and-egg escape hatch).** The development-run control
carries a **default-OFF** "bootstrap run — skip layer-2 checks" toggle. Layer 2 is fail-closed: a repo
with a manifest but no lint/test wired fails as "could-not-run," which is correct governance but creates
a deadlock — the very run that would *install* the linters fails layer 2 because the tools aren't there
yet. Enabling this toggle skips **only** the post-task layer-2 lint/test bounce for that one
tool-installing run, so you can land the tooling, then turn it back off. **Layer 1 (deny-before-write)
and the decisions gate still apply** — you never bypass the security gate. It's deliberate and visible,
never silent or sticky.

#### Run liveness: stall warnings and the Stop button

Camerata watches for **lack of progress**, not elapsed time. A legitimately long build or agent step
that keeps emitting output is never flagged. A process that goes silent is.

**Stop button — always available.** A **■ Stop** button appears in the run panel bar as long as the
run is in a non-terminal state. You do not need to wait for a stall warning to stop a run; clicking
Stop at any point ends the run and transitions it to **Cancelled**.

**Stall warning.** If a dev run produces no progress for the project's configured **Watched** threshold
(default 120 s), an amber banner appears above the live-events stream:

> ⚠ No progress for Xm — possible stall

The banner shows the idle duration and the last progress label. It is a warning, not an automatic
kill — for an interactive dev run you remain in control. Dismiss the concern by clicking Stop, or
wait to see if the agent resumes.

**Failed vs. Cancelled.** These two terminal states mean different things:
- **Cancelled** — you (or another operator action) explicitly stopped the run. Normal for "I changed
  my mind" or "something looked wrong."
- **Failed (with reason)** — the run stopped due to an error, including an automatic stall-cancel
  for a routine/autonomous run where `StallPolicy` is `Cancel`. The failure reason (e.g. "Stall
  timeout exceeded") is displayed in the run panel and recorded on the UoW history. For autonomous
  routines, the recorded reason is the actionable diagnostic — it surfaces in the Routines view and
  any configured escalation path.

The stall threshold and policy are per project (see "Project settings" above); the Stop button
behavior is identical regardless of threshold.

#### Later stages (Development → Awaiting QA → Signed Off)

Once a development run starts, the remaining stage transitions are engine-driven:

- **Development → Awaiting QA** is set by the fleet when the run finishes.
- **Signed Off** is the architect's explicit act after reviewing the run's diff + gate results.

**Other controls on every UoW card:**

- **PR lifecycle** — a dedicated panel on each UoW card for the push/PR/feedback loop:
  - **Push & open PR** — pushes the UoW branch and opens a PR with a **user-selected base branch**
    (picker in the console). The resulting PR number + URL are stored on the UoW.
  - **Pull PR info** — fetches current PR state, CI check status (passed/failed/pending), and
    comments. Camerata first checks its stored PR number; if none is stored, it searches by head
    branch — so a PR opened directly in GitHub is found automatically and its number is backfilled.
  - **Resolve with agent (gated)** — feeds open review comments and failing CI check names to a
    gated agent that edits the worktree to address the feedback. Same gate as the dev run; the
    agent cannot commit or push itself.
  - **Comment** — posts a comment on the PR/issue from the console.

- **Open work item** — opens the full work-item modal right from inside the UoW (next to the
  retained **Open issue ↗** link). The modal shows the title, body, and a **Comments** section that
  fetches and renders every comment on the source issue (author + timestamp + body). The
  create/open-UoW affordance is hidden here (you're already in the UoW).
- **Add comment to issue** — write a comment posted back onto the source issue via the tracker
  adapter. The comment box has **GitHub-style @-mention autocomplete**: type `@` and a dropdown of the
  repo's assignable users appears; click one to insert `@login`. This replaces the old "Ask the team"
  clarify panel, which has been removed. (The mention set is GitHub's repo **assignees** — the
  practical mention set, provider-specific; a per-provider user search is the future generalization.)
- **Update branch (AI-assisted)** — Camerata's equivalent of GitHub's PR "Update branch": pick a source
  branch (grouped **Local** / **Origin**) and merge it **into** this UoW's branch. A clean merge commits
  automatically; on a conflict a **single gated agent** resolves the conflict markers and stages the
  files, and the server completes the merge commit. The agent runs behind the same gate as every
  Camerata agent (gated tools only, no `git`, can't spawn sub-agents) — it never commits or pushes
  itself. It's **fail-closed**: with live build off, conflicts abort the merge with an honest "needs the
  AI resolver" message (a clean merge still succeeds); and any path left conflicted aborts the whole
  merge (`git merge --abort`), so a model claiming success without resolving is caught.
- **Pull latest work item** — re-pull just this one item from the tracker (a full refresh, no cache).
- **Sign off this run** — review the run's diff + gate results (rules in force, deny/allow tallies,
  total bounces) and **✓ Sign off this run**; provenance is written back. Sign-off also triggers
  worktree cleanup.

### Gate self-check (GO / NO-GO)

The view hosts a one-click **Run gate self-check** that proves the gate loop is actually wired
*before* you trust it with a work item. It runs the deterministic end-to-end probe (no model call, no
tokens): it plants one violation for **every enforced gate rule** (the security floor), confirms
**Layer 1 denies each one** before it can touch disk, confirms a clean write is **allowed** (the gate
isn't deny-all), and confirms **Layer 2 bounces once on a planted violation and resolves on the
revise pass**. It reports a single **GO / NO-GO** verdict with the floor count (e.g. "6/6 floor rules
enforced"). GO means deny-before-execute + bounce-and-revise are both live. The same probe runs in CI
and as `camerata gate-probe` on the CLI.

> **Note on enforcement transparency:** today the gate blocks and bounces the agent without a
> human-visible audit log of each denial. A visible enforcement record (showing which rule fired, on
> which file, during which run) is planned but not yet built.

---

## 7. The rules that govern it

Four enforcement points, all deterministic (binary pass/fail, no LLM judgement):

| Point | Enforces on | Example |
|---|---|---|
| **Layer 1** (MCP tool gate) | one write's file content, before it executes | no hardcoded secret reaches disk |
| **Layer 2** (CheckRunner) | one task's diff, after | the repo's own format/lint/test (e.g. `cargo fmt`/`clippy`/`test`, `ruff`/`pytest`, `npm run lint`/`test`, `gofmt`/`go vet`/`go test`, `bundle exec rubocop`/`rspec`, `./mvnw verify`/`./gradlew check`, `dotnet format`/`build`/`test`) |
| **Integration gate** | the assembled tree (cross-agent) | API contract between two agents agrees |
| **VCS-action gate** | commit/PR/branch metadata | the PR title + commit subject carry the ticket id |

**Layer 2 is cross-language and polyglot — across all seven supported languages.** It is no longer
Rust-only: for each worktree it runs the checks for every **supported** language present — **Rust,
JavaScript/TypeScript, Python, Go, Ruby, Java, and C#** — using the **repo's own pinned toolchain**
(the same tool versions the repo's CI uses, taken from the repo's lockfile/wrapper/SDK pin, never
baked into Camerata). It is **fail-closed**: if a toolchain is missing, a check isn't defined, or dep
install fails, the task is treated as **not verified** (an error), never a clean pass. So code is
pre-linted at dev time across all seven languages.

> Each language uses its own pinned tooling: Rust via `cargo`, JS/TS via the lockfile-detected
> package manager (`npm`/`pnpm`/`yarn`), Python in an isolated venv from `requirements.txt`/
> `pyproject.toml`, Go via `go.mod`, **Ruby** via `Gemfile.lock` + bundler (`bundle exec rubocop` +
> `rspec`/`rake test`), **Java** via the repo's `./mvnw verify` / `./gradlew check` wrapper, and
> **C#** via `dotnet format`/`build`/`test` (SDK pinned by `global.json`). This matches the seven
> languages the rule corpus ships rules for — the layer-2 language gap is closed.

Rule scopes: **corpus-global**, **repo-local** (from onboarding), **cross-repo** (contracts),
**process** (workflow conventions). The agent has no `git`, so Camerata is the sole committer.

---

## 8. The in-app assistant

The floating chat bubble is a single, context-rich assistant. There are no modes to pick. Every turn it is grounded in all of its sources at once, and your prompt decides which it leans on: ask "how do I onboard a repo" and it draws on the docs; ask "what did my last audit find" and it draws on the live project state; ask "where are we at" and it draws on the development state across every Unit of Work.

**Conversation context is retained** across messages — you can ask follow-up questions in the same session and the assistant remembers what was said earlier in the thread. The assistant is also **grounded on your active project** (its repos, ruleset, onboarded state) and on **any issues you've pulled in** via the Issue Management panel.

### What the assistant can see

A **"What this assistant can see"** strip at the top of the panel lists its context sources and whether each is currently loaded:

1. **Technical reference** (`docs/TECHNICAL.md`), baked in at compile time: how Camerata works.
2. **User guide** (`docs/USER_GUIDE.md`), baked in at compile time: flows, how-to steps, feature descriptions.
3. **Governance rules catalog** (the live corpus from `GET /api/corpus-rules`, fetched once per session): every rule with its domain, scope, and alternatives.
4. **Development state**, fetched from `GET /api/uow` and refreshed each turn: every Unit of Work with its lifecycle stage, gate/bounce status, and sign-off state, so a "where are we at" question returns a real cross-project status report.

A fifth row, **Focused finding**, appears only when you click **"Ask"** on a specific audit finding; it injects that finding's rule-id, path, and line additively so the assistant can zoom into one violation without losing the rest of the context.

The technical reference, user guide, and rules catalog form a stable prefix that is cached for cheap reuse; the development snapshot refreshes each turn.

### Honesty guardrail

The assistant answers **only** from those sources. When a question is covered by none of them, it says so verbatim rather than guessing, so it never invents features or facts that do not exist. That is the same honesty line drawn everywhere else in Camerata.

---

## 9. Update detection and rule drift

Camerata surfaces two update signals:

### App updates

When a new version of Camerata is available, a **banner appears at the top of the UI** with the release notes and an "Update" button. This is a one-click reminder, not an auto-update.

### External drift signals (distinct from verification)

Camerata watches for changes that originate OUTSIDE your ruleset. These are separate from the
verification ladder below: they concern a rule's presence and version relative to the corpus and your
local clones, not whether its verification is still trustworthy.

- **Repo path health check** (issue #33): the Rules view shows, per repo, whether its recorded local
  path still resolves to a git checkout, and lets you re-resolve a broken one.
- **Update detection**: an app-update banner appears when a newer Camerata release is available, and the
  Rules view can surface when a rule you have already APPLIED to a repo has since changed upstream in the
  corpus, so you can review and re-apply the new version.

### Verification badges

Every rule carries a provenance badge. These are **read-only in the app**: the `verified` tier is set only
by the maintainer-side verifier tool, never from the product.

| Badge | Meaning |
|---|---|
| ✓ **Verified** (green) | A human explicitly approved this rule, via the maintainer verifier tool. The gold standard. |
| **Grounded** (blue) | Rule is cited in a corpus source (linter, doc, framework best practice). The cited source(s) are listed in the rule's **detail panel**: open the rule and read the **Sources** section (a rule may cite several). |
| **Draft** (gray, italic) | AI-generated rule, not yet grounded. Advisory only, and never auto-recommended during onboarding. |

Re-verification is a deliberate maintainer act, done with the repo-side verifier tool (which lands as a
reviewed PR), not from inside the app.

> **Planned (not yet built):** detecting when a rule you have *applied* has since changed in the corpus
> (because you updated Camerata to a version carrying a newer version of that rule), then offering a per-rule
> "update to current version" diff so you decide whether to take it. It is informational and never
> auto-updates an applied rule. Tracked in [#66](https://github.com/zernst3/camerata-orchestrator/issues/66).

---

## 10. Single-rule editing (project and repo scope)

The **Rules view** (§4) allows fine-grained rule editing at three scopes:

### Editing a single rule

Click any rule in the **Project rules** table to open its **detail modal**. You can:
- **Switch its chosen option** — if the rule has multiple alternatives (e.g., "no hardcoded secrets" vs. "secrets vaulted"), select a different one. The choice is per-repo.
- **Add custom sub-options** — for rules that support local overrides, you can tack on a custom directive (e.g., "allow X in this repo only").
- **View the rule's full definition** — the decision question, all available options + rationale, the sources it's grounded in, and the enforcement kind.

### Repo-scoped overrides

A rule applies to **all repos in the project by default**. To override for a single repo:
- **Remove the rule from just that repo** — a checkbox per repo in the applied-rules table.
- **Add a custom rule that applies only to that repo** — a rule you author locally (e.g., "house style for tests in this codebase").

### Project-level rules (always apply everywhere)

Some rules are marked as **project-level** and apply to **every repo** in the project:
- Examples: process rules like commit format (`AB#{id}`), cross-repo API contracts, per-project security floor (baseline tech debt).
- These rules are immutable at the repo level — you can only edit them in the **Project rules** table, and the change flows to all repos on the next emit.

### Custom rules

Beyond the corpus you can author **two kinds of custom rules**:
- **Repo-scoped** — applies to a single repo; lives in its `.camerata/AGENTS.md`.
- **Project-scoped** — applies to every repo; lives in the project store (cross-repo rules read it).

Both custom types flow through the Rules view editor and the emission system. A custom rule is a free-text directive (name + body + scope) and carries **no enforcement kind**, so it is **prose or structured in practice** — never mechanical or architectural unless you open a development task to build a linter mapping or custom checker for it (see §13). Deleting a custom rule removes it from any repo it was on. Editing one changes only that rule.

---

## 11. Deep-report Markdown export

When you run an **onboarding audit** with the **"Deep report"** checkbox enabled, Camerata runs three advanced analysis lenses over each repo:

1. **SOC-2 gap analysis** — maps detectable practices onto SOC-2 Common-Criteria controls and reports **gaps** (what controls appear to lack implementation). This is a **gap analysis, never a compliance report or certification** — it is advisory and model-inferred. **Note:** the SOC-2 lens is behind the `soc2` feature flag, which ships **OFF** (§12); it runs only if you re-enable the flag.
2. **Deep security audit** — a layer deeper than the deterministic floor: authorization on write paths, sensitive-data handling, secret flows, trust boundaries. Findings flow into the same triage + tech-debt workflow as the standard audit.
3. **Threat model** — a structured STRIDE-flavored view: entry points, trust boundaries, data stores, sensitive-data paths, and threats + mitigations.

### Cost and timing

The deep tier is the **most expensive audit option** (~3× the standard audit cost, run on the strong model). It is **strictly opt-in** because of the cost; baseline audits (deterministic floor + AI architectural scan) run by default and are much cheaper.

### The deep-report export

After the audit, the **deep report is rendered as structured Markdown**. You can **export it** (download as `.md`) from the findings screen. The markdown includes:

- **Advisory notices** — prominent labels stating the output is model-inferred, a static-code analysis (not a pen test), and not externally validated.
- **Scoped-scan table** — deterministic security findings from the changed files (hardcoded secrets, raw SQL, etc.) — the same floor that gates are enforced on.
- **SOC-2 gap summary** — controls it appears the codebase leaves gaps in, with severity rankings.
- **Deep security findings** — architectural + authorization issues, trust-boundary violations, sensitive-data handling gaps.
- **Threat model** — entry points, data stores, trust boundaries, and the threats that apply to each.
- **SOC-2 control index** — a deduped cross-reference of all Trust Services Criteria controls touched by the scan.

The export is markdown, so you can **paste it into a GitHub issue, a team wiki, or a compliance readiness doc** for review and discussion.

### Important: the SOC-2 output is advisory

The SOC-2 lens produces a **gap analysis** — a conversation-starter about what controls may be underimplemented, not an audit-grade report. For true SOC-2 compliance work, pair Camerata's gap analysis with a real readiness review and, if aiming for certification, an accredited assessor. Camerata is a **governance tool**, not a compliance tool.

---

## 12. Feature flags (opt-out)

Camerata uses **feature flags** to gate features that are optional or under evaluation without code
branching. Every flag defaults **ON** (opt-out model): a flag absent from all sources is enabled. A
flag is turned off only by setting it to `false` explicitly.

### Setting flags

Flags are read from a **`.camerata/features.toml`** file at startup, and can be overridden per-flag by
an environment variable. Only an explicit `false` disables a flag; an absent or any other value leaves
it at its configured default.

```toml
# .camerata/features.toml
soc2 = false
```

```bash
export CAMERATA_FEATURE_SOC2=false   # env override; disables the flag
cargo run -p camerata-ui
```

### Current flags

There is currently **one** runtime flag:

| Flag | Env override | Default | Shipped value | What it controls |
|---|---|---|---|---|
| `soc2` | `CAMERATA_FEATURE_SOC2` | ON (opt-out) | **OFF** (the repo's `.camerata/features.toml` sets `soc2 = false`) | The SOC-2 gap-analysis lens in the deep audit tier (§11). |

So although the flag *defaults* on under the opt-out model, **SOC-2 is shipped OFF**: the checked-in
`.camerata/features.toml` sets `soc2 = false`, so the SOC-2 lens does not run unless you re-enable it.
When `soc2` is false, only the SOC-2 lens is skipped — the deep-security and threat-model lenses of the
deep tier still run, and the deep report is valid with an empty SOC-2 section. (Deep-security and
threat-model are part of the deep tier and are not separately flag-gated today.)

### Why flags?

Flags let us:
- **Ship features on day one** without inflating defaults (e.g., SOC-2 analysis is optional until validated).
- **A/B test** features with real users before committing to permanent defaults.
- **Kill or pivot** features fast if they don't land (no messy deprecation dance).
- **Support air-gapped or offline deployments** that opt out of certain AI passes.

---

## 13. Understanding rule types

Every rule in Camerata's corpus carries an `enforcement` field that answers one question: **how objectively can conformance be checked?** That single property decides where the rule is written and how it's enforced.

![Camerata enforcement model — rule sources feeding the check layers](enforcement-model.svg)

The diagram above is the canonical **stage** model — L1 Security · L2 Mechanical · L3 AI code review · L4 Origin/CI — and how rules feed each layer (full write-up: [`ENFORCEMENT_MODEL.md`](ENFORCEMENT_MODEL.md)). The table just below is a **different, related axis**: the rule *enforcement modalities* (how objectively a rule can be checked), which determine *what kind of wiring* a rule needs.

> **Numbering note:** this section (and §14, and the `layer3_only` flag) use "Layer 3" for **CI**, which is **L4** in the canonical stage model above; the AI code reviewer is the new **L3**. The two numberings are being reconciled.

### The four-layer model

| Rule tier | Blocks the agent's write (gate) | In-loop dev checks | Your repo's CI | Scan report |
|---|---|---|---|---|
| **Deterministic floor** (built-in secret/key/SQL/TLS checks) | Yes | No | No dedicated job | Yes |
| **Mechanical** (off-the-shelf linter, e.g. ESLint rule, Clippy lint) | Some | Yes (built-ins + your manifest checks) | Yes (generated) | Yes |
| **Architectural** (you build a custom checker, e.g. API layering via dependency-cruiser) | No | Once you register your checker in `.camerata/checks.toml` | Yes, once you build it | AI review only |
| **Prose / Structured** (advisory) | No | No | No | AI review |

**Security gate rules** are not a corpus category you author into. They are a small, hardwired set of rule-ids built directly into the MCP gate (see §7). They run before any write touches disk, require no build, and are always on. You do not select them; they cannot be turned off per-repo. The audit surfaces them as "deterministic floor" findings because they are the exact same checks the gate enforces on new writes. There are currently seven gate rules (`SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`, `ARCH-NO-SECRETS-IN-URL-1`, `SEC-NO-PRIVATE-KEY-1`, `SEC-NO-VENDOR-TOKEN-1`, `SEC-NO-SECRET-FILE-1`, `SEC-NO-DISABLED-TLS-1`).

The remaining four are the corpus enforcement modalities, from most human-judgment to most automated:

| Bucket | What it means for you | Where it's enforced |
|---|---|---|
| **Mechanical** | An existing linter catches it. Every mechanical rule maps to a real, named linter rule in a per-language tool (clippy, ruff, eslint, golangci-lint, etc.). | Local layer-2 check runner (fast, in the dev loop, across all detected languages) **and** CI (authoritative backstop). |
| **Architectural** | Machine-decidable but needs a custom AST check — no off-the-shelf linter expresses it (e.g. "handlers never touch the DB directly"). | Custom CI check (or agent directive as a fallback while the checker is being built). |
| **Structured** | A concrete design contract with a clear conform/violate answer — but not lint-able. Examples: "repositories return domain types," "API version lives in the URL prefix," "cursor not offset pagination." A human can verify it objectively; a linter cannot. | PR review (human, binary yes/no). |
| **Prose** | A principle or idiom where a human must judge conformance: "interfaces are small and cohesive," "optimization by default," "errors are wrapped with context." Reasonable engineers may weigh these differently on the margin. | PR review (human judgment). |

### Opt-in CI security rules (never auto-recommended)

A few `mechanical` rules carry an **`opt_in_only`** flag: they are grounded against a real tool but are
**never pre-checked** during onboarding — you opt in deliberately. The two security-scan rules
(`CICD-SEMGREP-SECURITY-SCAN-1`, `CICD-CODEQL-SECURITY-SCAN-1`, §3 step 6) are the current examples;
they exist to generate a security-scan **CI story** for a DevOps engineer, not to constrain the agent's
code, and they have **no default option** so selecting one forces a conscious tier choice. CodeQL also
carries **`layer3_only`** — its whole-program database build is too heavy to run at the scan preview or
in the dev loop, so it is enforced at CI / layer-3 only and never appears in the scan-time preview.

### The objectivity spectrum

One way to read the four modalities is as a single spectrum: how far can the conformance check shift from human to machine?

| Modality | Conformance test | Written to | Enforced by |
|---|---|---|---|
| prose | human judgment / matter of degree | `AGENTS.md` | PR review |
| structured | human, binary contract | `CONVENTIONS.md` | PR review |
| mechanical | existing linter | `CONVENTIONS.md` + CI | local check runner + CI |
| architectural | bespoke AST check | `CONVENTIONS.md` + CI | custom check |

### Prose vs. structured: the line that matters most

Both prose and structured rules carry the same TOML shape and both live outside CI — they are directives the agent follows and that engineers review. The difference is one of judgment:

- **Prose** rules require a human to *judge* conformance. They live in `AGENTS.md` because they are principles the agent reads and applies by spirit.
- **Structured** rules require a human to *verify* conformance against a clear binary contract. They live in `CONVENTIONS.md` because they are concrete, citable conventions.

"The API version lives in the URL prefix" is structured: you can check any endpoint and give a definite yes/no. "Interfaces are small and cohesive" is prose: engineers weigh it and the answer is a matter of degree.

### Where rules are written

`arm.rs` routes rules to files at emit time: `prose` → `AGENTS.md`, everything else (`structured`, `mechanical`, `architectural`) → `CONVENTIONS.md`. That routing is the live source of truth (see `crates/server/src/arm.rs`). The format string at the top of each generated file explains this for anyone reading the repo without Camerata open.

### Custom rules are always prose or structured

The four modalities above describe **corpus** rules. A rule *you* author (a custom rule, §10) is a
free-text directive with no `enforcement` field, so in practice it is only ever **prose** or
**structured** — an advisory directive the agent follows and a human reviews. A custom rule cannot be
`mechanical` or `architectural` on its own: those require an existing linter mapping or a bespoke
checker, which Camerata can't conjure for a rule it has never seen. To enforce a custom rule
deterministically, open a development task to build that linter mapping or custom check — then it
graduates out of the advisory tier.

### Rule provenance badges

Every rule also carries a `verification` badge (shown in the Rules view):

| Badge | Meaning |
|---|---|
| **Verified** | A human explicitly checked the rule and its cited sources. No agent may set this. |
| **Grounded** | The onboarding agents found and cited at least one real source (linter rule id, language spec, framework best practice). |
| **Draft** | AI-generated, not yet grounded. Advisory only; never auto-recommended during onboarding. |

The mechanical rules in the current corpus are grounded (each maps to a real, named linter rule that was validated to exist). None are verified yet — that is a deliberate human-only step the maintainer keeps. Grounded is the baseline for a shippable rule; verified is the gold standard you can cite to an auditor.

---

## 14. Wiring CI gates: the check manifest (SSOT)

When you click the onboarding buttons to create CI stories (§3 step 6), the stories teach a single
wiring model: **one entry in `.camerata/checks.toml` enforces a check at BOTH the in-loop dev gate
(Layer 2) and the generated CI workflow (Layer 3).** There is no "wire it twice" step.

### Why a single source of truth matters

Without a shared definition, Layer 2 and Layer 3 can drift: a custom linter you add to CI never
reaches the dev loop, so the agent gets no early feedback and can produce code that passes locally
but fails in CI. The manifest eliminates this structurally — both consumers read the same file.

### The manifest schema

```toml
# .camerata/checks.toml
[[check]]
id       = "ARCH-API-LAYERING-1"         # stable rule id; used as the violation id on nonzero exit
name     = "API layering"                 # short human label shown in bounce-back messages
command  = "scripts/check_layering.sh"   # shell command, runs from the repo root (sh -c)
severity = "high"                         # "high" | "medium" | "low" — informational; all severities block
in_loop  = true                           # true = run at Layer 2 AND Layer 3; false = CI-only
```

All five fields are required. A missing field is a parse error, not a silent misconfiguration. A
missing manifest is never fatal — the built-in language runners (cargo/clippy/eslint/ruff/etc.)
are always unaffected.

**`in_loop` decides when the check runs:**

| `in_loop` | Runs at | Use when |
|---|---|---|
| `true` | Layer 2 (dev loop) AND Layer 3 (CI) | Check is fast (under 30s), needs no external secrets or services. The agent gets early feedback. |
| `false` | Layer 3 (CI) only | Check needs secrets, an external service, or takes too long to run mid-loop. CI is always the authoritative backstop. |

**The manifest is agent-protected.** The gate rule `SEC-NO-CAMERATA-CONFIG-1` blocks any agent write
to the `.camerata/` directory. Editing the manifest is always a human/operator commit. This prevents
the canonical gate-weakening attack: an agent cannot disable the rules that govern it.

### Mechanical vs. architectural: two different paths

The onboarding flow files **two separate CI stories** — one per enforcement tier — because the
implementation work is fundamentally different:

**Mechanical CI story** ("Wire mechanical rules"): each selected mechanical rule maps to an
off-the-shelf linter. The story shows a per-rule manifest entry template. The implementation is:
fill in the correct command + pinned version, commit the manifest, done. No custom checker design
needed.

**Architectural CI story** ("Wire architectural rules"): there is NO off-the-shelf linter for
architectural rules (e.g. "service layer never touches the DB directly"). The story walks through
a four-step process:
1. Design a deterministic checker (options: a shell script, a custom Semgrep rule, an AST pass,
   a dependency-cruiser config — anything that exits 0 for clean and nonzero for a violation).
2. Add the manifest entry with pinned tool + version + install command.
3. Regenerate the CI workflow.
4. Verify the check at both Layer 2 and Layer 3.

The story includes a worked example for API-layering enforcement via `dependency-cruiser`, as this
is the most common architectural check pattern.

Scope each architectural rule as its own sub-task; do not block the mechanical story on this design
phase.

### What apply writes — the closed loop

When you click **Add rules to repo(s)** (§3 step 5), the apply step is now the upstream source for
everything Layer 2 and Layer 3 consume. The loop that was previously open is now closed:

```
Apply (Camerata UI)
  └─ writes  .camerata/checks.toml   (the SSOT manifest)
                 |
       +---------+---------+
       |                   |
  Layer 2               Layer 3
  (dev loop)             (CI)
  ManifestCheckRunner    camerata-gates.yml
  reads checks.toml      generated from checks.toml
```

Before this was wired, the apply step wrote a `.camerata/ci-checks.json` file and a placeholder
`camerata-governance.yml` that nothing in the runtime consumed. Those are gone. Now apply emits the
real `.camerata/checks.toml` (the exact format `load_manifest` parses) and the real
`.github/workflows/camerata-gates.yml` (identical to what the Regenerate CI workflow button
produces). There is no "wire it twice" step and no divergence between what the apply step wrote and
what the runtime executes.

**`.camerata/checks.toml` is the single file you edit to manage custom gate checks.** One change
there is automatically reflected in both Layer 2 (next dev run) and Layer 3 (next CI run after
commit). The `camerata-gates.yml` workflow is regenerated from it on demand — it is never the
authoritative source, only the derived output.

### Regenerating the CI workflow

After editing `.camerata/checks.toml`, regenerate the CI workflow by clicking the **Regenerate CI
workflow** button in the Rules view (or calling `POST /api/projects/active/generate-ci-workflow`).
The workflow is derived entirely from the manifest, so regenerating it is always safe to repeat.

---

## 15. Tool-version pinning and the drift error

Even with a single manifest definition, a check can still disagree between Layer 2 and Layer 3 if
the two environments run **different versions of the same tool**. For example, a `dependency-cruiser`
rule that was valid under version 5.x may emit different findings under 6.x. The manifest solves
this with three optional pinning fields:

```toml
[[check]]
id       = "DEP-CRUISER-LAYERING-1"
name     = "dependency-cruiser layering"
tool     = "dependency-cruiser"                        # the binary name
version  = "6.3.0"                                     # EXACT version — no ranges or carets
install  = "npm install -g dependency-cruiser@6.3.0"   # the exact install command
command  = "depcruise --config .dependency-cruiser.cjs src"
severity = "high"
in_loop  = true
```

**`version` must be an exact version string, not a range (`^6.3.0` or `>=6`) .** Ranges allow the
two environments to land on different patch releases and disagree. Pinning an exact version is what
makes the SSOT property hold end-to-end.

### What happens at Layer 3 (CI)

The generated CI workflow emits a **dedicated install step immediately before the check step**:

```yaml
- name: "install dependency-cruiser (6.3.0)"
  run: npm install -g dependency-cruiser@6.3.0

- name: "dependency-cruiser layering (DEP-CRUISER-LAYERING-1)"
  run: depcruise --config .dependency-cruiser.cjs src
```

CI always installs the pinned version, so the check runs against the exact tool version you declared.

### What happens at Layer 2 (your machine)

Layer 2 does NOT install tools — installing in the dev loop is too heavy. Instead, before running a
pinned check, Camerata verifies that your local tool version matches the pinned version. If it does,
the check runs. If it doesn't, you get a clear error like:

```
local dependency-cruiser is 5.1.0 but manifest pins 6.3.0 —
install the pinned version: npm install -g dependency-cruiser@6.3.0
```

The check is **skipped** (not silently run on the wrong version) until you reconcile. This is a
violation, not a warning — a warning would still let the loop complete "green" on the wrong tool,
which defeats the whole point.

**To fix a drift error:** run the `install` command shown in the error message (it's always the exact
command from your manifest's `install` field), then re-run your dev loop. This protects you from the
"passes locally, fails in CI" class of surprises.

**Checks without pinning fields** run normally at both layers with no version check — you get the
system-installed version at each environment. Add pinning when your check's output is sensitive to
the exact tool version.

---

## 16. Token usage meter

A compact, persistent **usage meter** is pinned to the right of the cockpit nav row. It shows your
cumulative token and dollar spend for the current session: `<tokens> tok · $<cost> · <calls> calls`.
Click it to expand a by-model breakdown table.

When the provider is rate-limiting requests, the meter swaps to an amber pulsing **"Rate-limited —
retrying"** badge instead of the normal readout. This clears automatically when the next request
succeeds.

The meter accumulates spend across ALL model calls — the audit, calibration, research chat, story
authoring, decomposition, clarification, and fleet runs — not just the last audit. It is purely
observational: it does not change model selection, retry behavior, or the gate.

---

## 17. Per-project model settings

Every AI step in Camerata has a configurable model, set **per project** via the **Step models**
section in the project-settings gear popup (§6). One labeled selector per step; the available options
come from the connected provider.

**Steps you can configure per project:**

| Step | When it runs |
|---|---|
| Audit | The onboarding code audit scan |
| Calibration | The severity-calibration pass during the audit |
| Research chat | The in-app assistant |
| Story authoring | Drafting an issue from a blank UoW |
| Decomposition | Breaking a story into sub-tasks |
| Escalation | Translating escalation decisions into prose |
| Clarification | Generating structured clarification questions |

All steps default to `claude-sonnet-4-6` when a project is created. The defaults are seeded at
creation time — there is no "unset" state. A change to one project never touches another project's
step models.

For steps where you also pick a model per run (audit, calibration, research chat), your per-run
choice wins over the project default for that run only. The project default is what you see when you
open a fresh run.

The governed development fleet (investigation, dev run, update-branch, PR resolve) is configured
separately via the **tier-map** (Strongest / Balanced / Fast) in the same gear popup — those runs
are orchestrated differently and belong to a different configuration axis.

---

## The whole loop, in one line

**Create/open a project → onboard each repo (browse to its local folder → scan the local code,
choosing AI review and/or deterministic scans → pick per-repo rules (opt-in rules are never
pre-checked) → Add rules to repo(s): local branch+push → optionally audit + triage (check Test
badge + Scan coverage section) + wire CI via two separate stories (mechanical and architectural)) →
manage the ruleset in the Rules view → in Governed Development, pull work items (or author a new
story from a blank UoW with AI), create a Unit of Work from one → Begin investigation (Intake,
single-model run; may pause for structured clarifications) → Approve decisions (Investigating) →
Run development governed (Decisions Approved, three-tier orchestrator-led run, brownfield in-place
or greenfield scaffold) → use PR lifecycle buttons to push, open PR, pull CI status, and resolve
feedback with a gated agent → review → sign off.**

Onboarding is local-first (no GitHub needed); connect GitHub + Claude for the push/PR and the AI
audit + governed dev. Export/import a project (config only; UoWs stay local) to move it between
machines; resolve local repo paths on the receiving side. Multiple UoWs can run concurrently, each
in its own isolated git worktree. Wire custom checks via `.camerata/checks.toml` (the SSOT for
both the dev loop and CI); pin exact tool versions to prevent drift. Watch the token usage meter in
the nav for cumulative spend. Use the chat bubble to ask data-driven questions about your active
project — it retains context across messages.
