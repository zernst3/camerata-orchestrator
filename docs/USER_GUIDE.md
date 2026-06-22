# Camerata — end-to-end user guide

This is the "I have a repo new to Camerata — where do I start, and how do I take it all
the way through?" walkthrough, and the canonical source the **in-app assistant** (the chat bubble) answers from. Keep it accurate to what's actually shipped — an
assistant that describes a feature that doesn't exist undercuts the whole point.

> Status (updated 2026-06-19): the brownfield **onboarding flow is built and live** — per-repo
> stack detection + rule selection, **custom rules** (per-repo + project-global), an optional code
> audit with three scan modes and an opt-in **thorough-calibration** consensus pass, a three-table
> finding triage, and an **Apply** step that writes governance onto a local branch and pushes it
> (no PR until you ask). Re-onboarding an already-onboarded repo is **blocked**. Projects are
> **exportable/importable**, repos resolve to **local folders** with a health check, and onboarding
> state **auto-saves**. Governed Development carries a one-click **Gate self-check** (go/no-go) that
> proves the deny-before-execute floor + bounce-and-revise loop are wired. The two things you connect
> are a **GitHub token** and **Claude** (the `claude` CLI).

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
- **Governed Development** — the work control surface: pull work items from a tracker, create a Unit
  of Work (UoW) from one, then run governed development on it with the human↔AI clarify loop, comment
  back, and sign off (§6).
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
   Actions, GitLab CI, CircleCI, Azure Pipelines, Travis, Bitbucket, Drone, Jenkins). Build/dependency/
   generated dirs (`node_modules`, `target`, `.git`, lockfiles, …) are pruned. It proposes a starter
   ruleset **per repo**.
3. **Pick rules** — each repo has its **own** recommended-rule table; a repo single-select switches
   which repo you're editing. Selection is **per repo**: a rule ticked for repo A is bound to A only.
   **Project-level rules** apply to every repo. Click any rule to read its decision question, the
   options, the default, and each option's rationale, and to choose an alternative. **Option choices
   are also per repo** — adopting an alternative for a rule while viewing one repo does not change
   another repo's choice. A recommended rule that still needs an alternative chosen is **highlighted
   amber and blocks Audit / Add-rules** until you pick one (or deselect it).
4. **Audit (optional) + triage** — optionally scan the existing code to surface violations. Each repo
   is scanned only against **its own selected rules** plus the always-on **deterministic security
   floor** (hardcoded secrets, raw-SQL concatenation, secrets in URLs — ranked Critical, free +
   instant).

   **Two kinds of finding, and the difference matters:**
   - **Deterministic floor** (`SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`,
     `ARCH-NO-SECRETS-IN-URL-1`) — pure regex/logic, no LLM. **Repeatable** (same code → same result,
     same rule-id, same line), and these are the exact checks the layer-1 gate enforces on new writes.
     Treat their rule-ids as **stable/canonical**.
   - **AI-suggested (architectural)** — model-inferred issues regex can't catch (layering violations,
     N+1, missing auth on writes, god objects, GET-with-side-effects). These are **advisory**, and the
     model **invents the rule-id** per finding, so the id, severity, and exact set can **vary run to
     run**. Read them as "the model flagged this pattern," not as a fixed rule. The calibration pass
     recalibrates severity and flags low-confidence ones but never drops any — you make the final call.

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
   the calibration pass's flag + reason. In the Tech-debt table mark items **resolve later** or
   **resolve now**, then **Process**: ignores become baseline waivers, and **every tech-debt item is
   filed as a GitHub issue** (the story). Resolve-now issues are titled for pickup by the dev engine
   (the actual dev work is Pillar 2; onboarding only *writes the story*). There is no separate "fix the
   findings" button — fixing a finding is just its resolve-now story flowing into the dev layer.
   Triage/Process is **not required** to finish onboarding.

   **Note:** **mechanical** rules (CI/runtime/DB-context checks like a query-plan/index audit) are NOT
   run by this code scan — they can't be judged from a static digest, so they're enforced in CI instead
   (step 6). The scan header shows how many were excluded for that reason.
5. **Add rules to repo(s)** — writes the governance files onto a `camerata/onboard-governance` branch
   in each repo's **local clone AND pushes that branch to origin — no pull request is opened.** Each
   repo's local clone is resolved from its recorded path (or, as a fallback, a workspace folder). The
   files: `AGENTS.md` (prose rules), `CONVENTIONS.md` (structured/mechanical rules), a CI workflow (for
   mechanical rules), and `.camerata/baseline.json` (accepted pre-existing debt). The branch is
   Camerata-managed and regenerated each run (force-pushed), so re-applying is safe. Edit the working
   copy freely, then click **Open governance PR** (a separate, optional button) when ready —
   **Camerata never opens a PR automatically.** **Applying marks the repo onboarded.**
6. **Add CI-enforced rules** — the final step files **two GitHub-issue stories** per repo, one per
   deterministic CI-tier track:
   - **Create mechanical-rules CI story** — wire the selected **mechanical** rules into that repo's CI
     as enforced lint gates. Mechanical rules map to an existing off-the-shelf linter, so this is the
     simpler track to wire.
   - **Create architectural-rules CI story** — wire the selected **architectural** rules into CI.
     Architectural rules need a **custom checker** (no off-the-shelf linter expresses them) plus team
     refinement before implementing.

   Each story carries a preamble explaining that both tracks are deterministic (mechanical = off-the-
   shelf linter; architectural = bespoke custom checker), and the two are filed as separate issues so
   they can be scheduled independently. Like resolve-now, onboarding *writes the story*; the dev layer
   (Pillar 2) does the work. Separate from the tech-debt issues above.

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
  (`POST /api/uow/:id/begin-investigation { "model": "<id>" }` → `{ "run_id", "story_id" }`.)
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

**How the tiered run works:** the **Strongest-tier agent is the orchestrator and lead.** It does the
complex, one-way-door work itself. For well-scoped simpler subtasks it can use the governed
`mcp__camerata__delegate` tool to hand the task to the Balanced or Fast tier. The gate stays
universal across all tiers: every delegate child is spawned gated (`gated_write` only, `Task`
disallowed), delegation is only one level deep (children cannot re-delegate), and escalation is
parent-driven (a child returns an `INCOMPLETE:` signal and the orchestrator re-handles it). The raw
`Task` tool stays disallowed for every agent — delegation goes through `delegate`, not `Task`.

Without `CAMERATA_LIVE_BUILD=1` the run is token-free/scripted and the gate enforcement is still
real. With it set and `claude` connected, a real multi-tier `claude -p` fleet runs.

#### Later stages (Development → Awaiting QA → Signed Off)

Once a development run starts, the remaining stage transitions are engine-driven:

- **Development → Awaiting QA** is set by the fleet when the run finishes.
- **Signed Off** is the architect's explicit act after reviewing the run's diff + gate results.

**Other controls on every UoW card:**

- **Add comment to issue** — write a comment posted back onto the source issue via the tracker
  adapter. Use @-mentions to loop a teammate in; GitHub resolves the handle. This replaces the old
  "Ask the team" clarify panel, which has been removed.
- **Pull latest work item** — re-pull just this one item from the tracker (a full refresh, no cache).
- **Sign off this run** — review the run's diff + gate results (rules in force, deny/allow tallies,
  total bounces) and **✓ Sign off this run**; provenance is written back.

### Gate self-check (GO / NO-GO)

The view hosts a one-click **Run gate self-check** that proves the gate loop is actually wired
*before* you trust it with a work item. It runs the deterministic end-to-end probe (no model call, no
tokens): it plants one violation for **every enforced gate rule** (the security floor), confirms
**Layer 1 denies each one** before it can touch disk, confirms a clean write is **allowed** (the gate
isn't deny-all), and confirms **Layer 2 bounces once on a planted violation and resolves on the
revise pass**. It reports a single **GO / NO-GO** verdict with the floor count (e.g. "6/6 floor rules
enforced"). GO means deny-before-execute + bounce-and-revise are both live. The same probe runs in CI
and as `camerata gate-probe` on the CLI.

---

## 7. The rules that govern it

Four enforcement points, all deterministic (binary pass/fail, no LLM judgement):

| Point | Enforces on | Example |
|---|---|---|
| **Layer 1** (MCP tool gate) | one write's file content, before it executes | no hardcoded secret reaches disk |
| **Layer 2** (CheckRunner) | one task's diff, after | the repo's own format/lint/test (e.g. `cargo fmt`/`clippy`/`test`, `ruff`/`pytest`, `npm run lint`/`test`, `gofmt`/`go vet`/`go test`) |
| **Integration gate** | the assembled tree (cross-agent) | API contract between two agents agrees |
| **VCS-action gate** | commit/PR/branch metadata | the PR title + commit subject carry the ticket id |

**Layer 2 is cross-language and polyglot — across four supported languages.** It is no longer
Rust-only: for each worktree it runs the checks for every **supported** language present — **Rust,
JavaScript/TypeScript, Python, and Go** — using the **repo's own lockfile-pinned toolchain** (the
same tool versions the repo's CI uses, installed from the repo's lockfile, never baked into
Camerata). It is **fail-closed**: if a toolchain is missing, a check isn't defined, or dep install
fails, the task is treated as **not verified** (an error), never a clean pass. So code is pre-linted
at dev time across those four languages.

> The corpus also ships **Ruby, Java, and C#** rules, but those languages **do not have a layer-2
> runner yet** — their rules ride as agent directives and CI (layer 3) until a runner is added. A
> worktree whose only language is Ruby/Java/C# currently gets no layer-2 check (it falls back to a
> logged no-op). Adding a runner is a clean follow-up on the same `CheckRunner` seam.

Rule scopes: **corpus-global**, **repo-local** (from onboarding), **cross-repo** (contracts),
**process** (workflow conventions). The agent has no `git`, so Camerata is the sole committer.

---

## 8. The in-app assistant

The floating chat bubble is a single, context-rich assistant. There are no modes to pick. Every turn it is grounded in all of its sources at once, and your prompt decides which it leans on: ask "how do I onboard a repo" and it draws on the docs; ask "what did my last audit find" and it draws on the live project state; ask "where are we at" and it draws on the development state across every Unit of Work.

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

Every rule in Camerata's corpus carries an `enforcement` field that answers one question: **how objectively can conformance be checked?** That single property decides where the rule is written and how it's enforced. Five buckets are useful for everyday work; the first is special.

### The five buckets

**Security gate rules** are not a corpus category you author into. They are a small, hardwired set of rule-ids built directly into the MCP gate (see §7). They run before any write touches disk, require no build, and are always on. You do not select them; they cannot be turned off per-repo. The audit surface them as "deterministic floor" findings because they are the same checks the gate enforces on new writes. There are currently six gate rules; only one of them (`ARCH-NO-SECRETS-IN-URL-1`) also lives in the corpus as a `structured` rule — the rest are gate-internal primitives.

The remaining four are the corpus enforcement modalities, from most human-judgment to most automated:

| Bucket | What it means for you | Where it's enforced |
|---|---|---|
| **Mechanical** | An existing linter catches it. Every mechanical rule maps to a real, named linter rule in a per-language tool (clippy, ruff, eslint, golangci-lint, etc.). | Local layer-2 check runner (fast, in the dev loop, across all detected languages) **and** CI (authoritative backstop). |
| **Architectural** | Machine-decidable but needs a custom AST check — no off-the-shelf linter expresses it (e.g. "handlers never touch the DB directly"). | Custom CI check (or agent directive as a fallback while the checker is being built). |
| **Structured** | A concrete design contract with a clear conform/violate answer — but not lint-able. Examples: "repositories return domain types," "API version lives in the URL prefix," "cursor not offset pagination." A human can verify it objectively; a linter cannot. | PR review (human, binary yes/no). |
| **Prose** | A principle or idiom where a human must judge conformance: "interfaces are small and cohesive," "optimization by default," "errors are wrapped with context." Reasonable engineers may weigh these differently on the margin. | PR review (human judgment). |

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

## The whole loop, in one line

**Create/open a project → onboard each repo (browse to its local folder → scan the local code → pick
per-repo rules → Add rules to repo(s): local branch+push → optionally audit + triage + wire CI) →
manage the ruleset in the Rules view → in Governed Development, pull work items, create a Unit of Work
from one → Begin investigation (Intake, single-model run) → Approve decisions (Investigating) → Run
development governed (Decisions Approved, three-tier orchestrator-led run) → review → sign off.**
Onboarding is local-first (no GitHub needed); connect GitHub + Claude for the push/PR and the AI
audit + governed dev. Export/import a project (config only; UoWs stay local) to move it between
machines; resolve local repo paths on the receiving side. Use the chat bubble to ask data-driven
questions about your active project.
