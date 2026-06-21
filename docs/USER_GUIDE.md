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

---

## 2. The cockpit views

Inside a project the nav shows: **Onboard repos · Governed Development · Rules · Routines ·
Repository Workspace**.
- **Onboard repos** — bring a repo under governance (§3).
- **Governed Development** — the story control surface: adopt stories, run governed development with
  the human↔AI clarify loop, review + sign off (§6).
- **Rules** — manage the project's ruleset after onboarding + the repo-path health check (§4, §5).
- **Routines** — schedule governed runs.
- **Repository Workspace** — the local clones: clone status, branch, and ship (push + PR) for dev work.

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

   Pick the **model** and the **scan mode**:
   - **Parallel** (default) — runs rule-batches concurrently; fastest. Wall-clock is the slowest
     batch, not the sum of every call.
   - **Sequential** — one call at a time, all rules together; slower but gentlest on rate limits
     (a fallback when Parallel hits throttling).
   - **Background job** — same Parallel scan, but it runs server-side and detached: you get a
     progress view and can walk away while findings stream in. Best for huge / multi-repo scans
     where a foreground scan would tie up the page. (Parallel and Sequential run in the
     foreground and block until they finish; Background job is the same work, just detached.)

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
6. **Wire mechanical rules into CI** — the final step: file a **story (GitHub issue)** per repo to add
   the selected mechanical rules to that repo's existing CI as enforced lint gates (checks what's
   already enforced, adds the rest). Like resolve-now, onboarding *writes the story*; the dev layer
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

Each custom rule carries the same shape as a corpus rule (id, the decision question, options +
default + rationale, enforcement kind) so it flows through selection, the amber needs-choice
highlight, and Apply exactly like a built-in. You can **create, edit, and delete** custom rules from
the Rules view; deleting one removes it from any repo it was on. Custom rules are advisory unless you
give them a mechanical/CI enforcement kind.

---

## 5. Repo paths (resolution + health)

Each repo resolves to a **local folder** — repos can live anywhere; the path is machine-local and is
**never exported**. The Rules view runs a continuous **health check**: any repo that doesn't point at
a valid local git checkout (common right after importing a project) is flagged with a warning + a
per-repo **Resolve…** button to browse to its folder. A repo is "broken" when its path is unset,
missing, or not a git checkout whose origin matches `owner/repo`.

---

## 6. Steer a story through its lifecycle (Governed Development)

Select a story in the spine. The center stage has clickable stage tabs:
1. **Intake** — adopted into the spine.
2. **Investigation** — the lead engineer raises clarifying questions via the bridge (posts a comment
   on the tracker item; the owner answers; the answer comes back). Review before posting.
3. **Plan** — decompose the story into child stories, each independently governable.
4. **Status (execution & gating)** — "Run this story (governed)". The fleet runs under the gate:
   **Layer 1** denies a forbidden write before it touches disk; **Layer 2** re-checks each task
   (`fmt`/`clippy`/`test`); the **worktree jail** confines every write. Without `CAMERATA_LIVE_BUILD=1`
   this runs token-free/scripted (the gate deciding is still real); with it set + `claude` connected,
   a real `claude -p` fleet.

   **Gate self-check (go/no-go).** This view also hosts a one-click **Gate self-check** that proves
   the gate loop is actually wired *before* you trust it with a story. It runs the deterministic
   end-to-end probe (no model call, no tokens): it plants one violation for **every rule in the
   security floor**, confirms **Layer 1 denies each one** before it can touch disk, confirms a clean
   write is **allowed** (the gate isn't deny-all), and confirms **Layer 2 bounces once on a planted
   violation and resolves on the revise pass**. It reports a single **GO / NO-GO** verdict with the
   floor count (e.g. "6/6 floor rules enforced"). GO means deny-before-execute + bounce-and-revise
   are both live. The same probe runs in CI and as `camerata gate-probe` on the CLI.
5. **QA & sign-off** — review the diff + gate results, sign off; provenance is written back.

---

## 7. The rules that govern it

Four enforcement points, all deterministic (binary pass/fail, no LLM judgement):

| Point | Enforces on | Example |
|---|---|---|
| **Layer 1** (MCP tool gate) | one write's file content, before it executes | no hardcoded secret reaches disk |
| **Layer 2** (CheckRunner) | one task's diff, after | `fmt`/`clippy`/`test` |
| **Integration gate** | the assembled tree (cross-agent) | API contract between two agents agrees |
| **VCS-action gate** | commit/PR/branch metadata | the PR title + commit subject carry the ticket id |

Rule scopes: **corpus-global**, **repo-local** (from onboarding), **cross-repo** (contracts),
**process** (workflow conventions). The agent has no `git`, so Camerata is the sole committer.

---

## 8. The in-app assistant

The floating chat bubble is a single, context-rich assistant. There are no modes to pick. Every turn it is grounded in all of its sources at once, and your prompt decides which it leans on: ask "how do I onboard a repo" and it draws on the docs; ask "what did my last audit find" and it draws on the live project state; ask "where are we at" and it draws on the development state across every Unit of Work.

### What the assistant can see

A **"What this assistant can see"** strip at the top of the panel lists its four context sources and whether each is currently loaded:

1. **Canonical docs** (`USER_GUIDE.md`, `TECHNICAL.md`), baked in: the source of truth for features and how things work.
2. **Project rules** (the corpus plus the active project's selections): what is actually in scope.
3. **Live development state**, fetched from the development-context endpoint: every Unit of Work with its lifecycle stage, gate/bounce status, and sign-off state, so a "where are we at" question returns a real cross-project status report.
4. **Active finding**, injected additively when you click **"Ask"** on a specific audit finding, to zoom into one violation without losing the rest of the context.

The docs and rules form a stable prefix that is cached for cheap reuse; the development snapshot refreshes each turn.

### Honesty guardrail

The assistant answers **only** from those sources. When a question is covered by none of them, it says so verbatim rather than guessing, so it never invents features or facts that do not exist. That is the same honesty line drawn everywhere else in Camerata.

---

## 9. Update detection and rule drift

Camerata surfaces two update signals:

### App updates

When a new version of Camerata is available, a **banner appears at the top of the UI** with the release notes and an "Update" button. This is a one-click reminder, not an auto-update.

### Applied-rule drift

The **Rules** view runs a continuous **health check** on applied rules. Any rule that was applied to a repo but is no longer in the project's ruleset is flagged as **"Needs re-check"** with an amber badge. This happens when:
- A rule was deleted from the corpus (rare; the corpus is stable).
- The project's ruleset was edited and the rule was deselected.
- A repo was exported/imported and the ruleset drifted between machines.

The Rules view also shows **verification badges** on every rule:

| Badge | Meaning |
|---|---|
| ✓ **Verified** (green) | A human explicitly approved this rule; gold standard |
| **Grounded** (blue) | Rule is cited in a corpus source (linter, doc, framework best practice). The cited source(s) are listed in the rule's **detail panel** — open the rule and read the **Sources** section (a rule may cite several). |
| **Draft** (gray, italic) | AI-generated rule not yet grounded; advisory only |
| **Needs re-check** (amber) | Was verified but its source drifted; review before relying on it |

To **update a rule** when drift is detected: click the amber "Needs re-check" badge, review the rule's rationale and source, and either **re-verify** it (mark it trusted) or **delete** it from the ruleset. The Rules view will regenerate the affected repos' `.camerata/AGENTS.md` and `CONVENTIONS.md` on the next emit.

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

Both custom types flow through the Rules view editor and the emission system exactly like corpus rules. Deleting a custom rule removes it from any repo it was on. Editing one changes only that rule.

---

## 11. Deep-report Markdown export

When you run an **onboarding audit** with the **"Deep report"** checkbox enabled, Camerata runs three advanced analysis lenses over each repo:

1. **SOC-2 gap analysis** — maps detectable practices onto SOC-2 Common-Criteria controls and reports **gaps** (what controls appear to lack implementation). This is a **gap analysis, never a compliance report or certification** — it is advisory and model-inferred.
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

## 12. Feature flags (opt-in/opt-out)

Camerata uses **feature flags** to ship features that are optional or under evaluation without requiring code branching. Flags default **ON** (features enabled) unless otherwise noted.

### Enabling/disabling flags

Flags are controlled via **environment variables**, set before launching the app:

```bash
export CAMERATA_FEATURE_<NAME>=false   # Disable the feature
export CAMERATA_FEATURE_<NAME>=true    # Enable it (or omit if default is on)
cargo run -p camerata-ui
```

Alternatively, add them to your `.env` file (in the repo root, gitignored):

```env
# .env (auto-loaded at startup)
CAMERATA_FEATURE_SOC2_ANALYSIS=false
CAMERATA_FEATURE_DEEP_SECURITY=true
```

### Current flags

| Flag | Default | What it controls | Note |
|---|---|---|---|
| `CAMERATA_FEATURE_SOC2_ANALYSIS` | **OFF** | SOC-2 gap-analysis lens in the deep audit. | Off by default until gap analysis is externally validated (Phase 2). Safe to turn ON; advisory label is mandatory. |
| `CAMERATA_FEATURE_DEEP_SECURITY` | ON | Deep-security lens (trust boundaries, auth, secrets) in the deep audit. | Always safe. |
| `CAMERATA_FEATURE_THREAT_MODEL` | ON | Threat-model lens (STRIDE-flavored) in the deep audit. | Always safe. |

When a flag is OFF, the corresponding **scan option is hidden from the UI** and the feature **is not run**, so no cost is incurred and no noise is added to the findings.

### Why flags?

Flags let us:
- **Ship features on day one** without inflating defaults (e.g., SOC-2 analysis is optional until validated).
- **A/B test** features with real users before committing to permanent defaults.
- **Kill or pivot** features fast if they don't land (no messy deprecation dance).
- **Support air-gapped or offline deployments** that opt out of certain AI passes.

---

## The whole loop, in one line

**Create/open a project → onboard each repo (browse to its local folder → scan the local code → pick
per-repo rules → Add rules to repo(s): local branch+push → optionally audit + triage + wire CI) →
manage the ruleset in the Rules view → adopt stories and run governed work in Governed Development →
review → sign off.** Onboarding is local-first (no GitHub needed); connect GitHub + Claude for the
push/PR and the AI audit + governed dev. Export/import a project to move it between machines; resolve
local repo paths on the receiving side. Use the chat bubble to ask data-driven questions
about your active project.
