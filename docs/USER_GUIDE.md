# Camerata — end-to-end user guide

This is the "I have a repo new to Camerata — where do I start, and how do I take it all
the way through?" walkthrough, and the canonical source the **in-app assistant** (the chat
bubble's **Guide** mode) answers from. Keep it accurate to what's actually shipped — an
assistant that describes a feature that doesn't exist undercuts the whole point.

> Status (updated 2026-06-18): the brownfield **onboarding flow is built and live** — per-repo
> stack detection + rule selection, an optional code audit with three scan modes, a three-table
> finding triage, and an **Apply** step that writes governance onto a local branch and pushes it
> (no PR until you ask). Projects are **exportable/importable**, repos resolve to **local
> folders** with a health check, and onboarding state **auto-saves**. The two things you connect
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

1. **Point at the repo(s)** — add them to the project (one `owner/repo` per line, or browse to a
   local folder).
2. **Scan + propose per-repo rules** — Camerata downloads each repo and detects its stack: languages
   from extensions, frameworks from manifests, **IaC** (Terraform, Terragrunt, Bicep, Pulumi,
   CloudFormation) and **CI/CD** (GitHub Actions, GitLab CI, CircleCI, Azure Pipelines, Travis,
   Bitbucket, Drone, Jenkins). It proposes a starter ruleset **per repo**.
3. **Pick rules** — each repo has its **own** recommended-rule table; a repo single-select switches
   which repo you're editing. Selection is **per repo**: a rule ticked for repo A is bound to A only.
   **Project-level rules** apply to every repo. Click any rule to read its decision question, the
   options, the default, and each option's rationale, and to choose an alternative.
4. **Audit (optional) + triage** — optionally scan the existing code to surface violations. Each repo
   is scanned only against **its own selected rules** plus the always-on **deterministic security
   floor** (hardcoded secrets, raw-SQL concatenation, secrets in URLs — ranked Critical, free +
   instant). Pick the **model** and the **scan mode**:
   - **Parallel** (default) — runs rule-batches concurrently; fastest. Wall-clock is the slowest
     batch, not the sum of every call.
   - **Sequential** — one call at a time, all rules together; slower but gentlest on rate limits
     (a fallback when Parallel hits throttling).
   - **Background job** — same Parallel scan, but it runs server-side and detached: you get a
     progress view and can walk away while findings stream in. Best for huge / multi-repo scans
     where a foreground scan would tie up the page. (Parallel and Sequential run in the
     foreground and block until they finish; Background job is the same work, just detached.)
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
5. **Apply** — writes the governance files onto a `camerata/onboard-governance` branch in each repo's
   **local clone AND pushes that branch to origin — no pull request is opened.** The files: `AGENTS.md`
   (prose rules), `CONVENTIONS.md` (structured/mechanical rules), a CI workflow (for mechanical rules),
   and `.camerata/baseline.json` (accepted pre-existing debt). Edit the working copy freely, then click
   **Open governance PR** (a separate button) when ready. **Applying marks the repo onboarded.**
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

## 8. The in-app assistant (chat bubble)

The floating chat bubble has two modes:
- **Research** — open AI chat (any question), a live smoke test that the model backend works.
- **Guide** — answers "how do I do X in Camerata?" grounded in THIS user guide. It answers only from
  the guide and says when something isn't covered, so it won't invent features.

---

## The whole loop, in one line

**Connect GitHub + Claude → create/open a project → onboard each repo (scan → pick per-repo rules →
Apply local branch+push → optionally audit + triage + wire CI) → manage the ruleset in the Rules view →
adopt stories and run governed work in Governed Development → review → sign off.** Export/import a
project to move it between machines; resolve local repo paths on the receiving side.
