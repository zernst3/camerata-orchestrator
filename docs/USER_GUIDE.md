# Camerata — end-to-end user guide

This is the "I have a repo new to Camerata — where do I start, and how do I take it
all the way through?" walkthrough. It covers both **brownfield** (an existing repo)
and **greenfield** (a new repo), the two credentials you connect, and the full loop
from onboarding a repo to shipping a governed change.

> Status note (2026-06-15): the app runs end to end on a **clean slate** with no
> seeded data. The two things you connect are a **GitHub token** and **Claude**
> (the `claude` CLI). What each step needs is called out inline, honestly: some
> steps light up the moment the token is present; the brownfield *scan/audit
> engine* is the next backend build and is flagged where it applies.

---

## 0. The two connections (the only setup)

Everything else is wired. You provide:

1. **GitHub** — a token, via environment variables. One token serves **every repo
   it can reach** (Camerata never scopes itself below the token):
   ```bash
   export CAMERATA_GITHUB_TOKEN=github_pat_xxx      # Issues R/W; + read:project for boards
   # optional default repo (a convenience, never a ceiling):
   export CAMERATA_GITHUB_REPO=owner/repo
   ```
   See [`GITHUB_SETUP.md`](GITHUB_SETUP.md) for token scopes.

2. **Claude** — the `claude` CLI on your PATH, logged in (`claude` Code
   subscription or an API key). This is what the governed fleet runs as agents.
   Camerata locks each agent to a single gated write tool; it never gets `Bash`,
   `Write`, or subagents (see [`HARDENING.md`](HARDENING.md)).

   **CLI vs API.** Camerata's live driver shells out to the **`claude` CLI**
   (`claude -p`); a direct-Anthropic-API driver is a seam but not built. So you
   configure *how the CLI authenticates*: log in for **subscription** credits, or
   set `ANTHROPIC_API_KEY` for **metered API** billing (the CLI picks it up). Set
   `CAMERATA_LIVE_BUILD=1` to run a real `claude -p` fleet (otherwise runs are
   scripted/token-free, but the gate deciding is still the real one).

### Notifications & polling cadence (optional env)

Camerata polls the integrations (no webhooks needed). Intervals are env-configurable
with sensible defaults:

| Variable | Default | What it paces |
|---|---|---|
| `CAMERATA_POLL_TRACKER_SECS` | `45` | Server poll of tracker events (PO comments, status changes). |
| `CAMERATA_POLL_DEPLOY_SECS` | `5` | Deployment-status poll (fast; reserved until a deploy source is wired). |
| `CAMERATA_UI_NOTIFY_SECS` | `5` | How often the app drains the notification feed into toasts. |

The app shows **toasts**: a warning when no integration is connected (optional, not
an error), an error when a configured connection fails (401/403/5xx), and an info
toast when one recovers — re-checked every 45s. Tracker events (a PO answering a
comment) flow through the server poller into toasts. Note the background tracker
poll uses the connection's **default repo** (`CAMERATA_GITHUB_REPO`); set it for the
poller to have a repo to watch.

Set the token, then launch:
```bash
CAMERATA_GITHUB_TOKEN=github_pat_xxx cargo run -p camerata-ui
```
The desktop app opens with its embedded server. The cockpit topbar shows the live
connection (`github (token; …)` vs `native (in-process)`), so you can confirm the
token took.

---

## 1. Where you start: onboard the repo

Open the **Enterprise cockpit** edition, then the **"Onboard a repo"** tab. This is
the entry point for any repo new to Camerata. It is *separate* from a story's
Investigation phase — onboarding sets up the **repo's** rules and CI gate;
Investigation refines one **piece of work**.

Pick your path:

### Brownfield (an existing repo)
The flow is **scan → propose → approve → audit → arm**:

1. **Point at the repo** — `owner/repo` your token can reach.
2. **Scan + propose a starter ruleset** — Camerata maps the stack and conventions
   and proposes a starting RuleSet. You *review*, you don't author from scratch.
3. **Approve / edit** — adjust and approve. You own the final set.
4. **Audit** — scan the existing code against the approved rules and list what's
   already wrong. This is the five-minute payoff ("here are the 12 things wrong in
   your repo right now"). *Content rules (hardcoded secrets, raw-SQL-concat, secrets
   in URLs) audit today; the AST-level architecture rules follow.*
5. **Arm** — generate **one** governance PR: `CONVENTIONS.md`/`AGENTS.md`, an
   enforced CI workflow, and the gate's rule-subset config. Merge it and new
   violations are stopped at the gate going forward.

### Greenfield (a new repo)
**name → pick starter ruleset → scaffold + arm**: Camerata scaffolds the repo with
the rules baked in from commit zero, so it's governed from the first commit.

> The "Scan repo" / "Scaffold repo" button activates once GitHub is connected (it
> runs against your repo). Until a token is present, the view explains each step and
> the button is disabled — that is the honest gate, not a dead button.

Design rationale: [`decisions/2026-06-15_brownfield_onboarding_flow.md`](decisions/2026-06-15_brownfield_onboarding_flow.md).

---

## 2. Bring in work: stories from your tracker

A **story** is a unit of work. It has two independent axes (shown in the cockpit
topbar):

- **Source** — where it's tracked: a GitHub **Issue**, a **Projects v2** board card,
  later ADO/Jira. Projects/boards sit *above* the repo.
- **Build targets** — the repo(s) where its code lands. One story can span several
  repos.

Two ways to get stories into the spine:

- **Adopt a GitHub issue** — pull an issue in by id (and `owner/repo`); it appears in
  the spine as a story. The first live round-trip is the CLI canary:
  ```bash
  CAMERATA_GITHUB_TOKEN=… CAMERATA_GITHUB_REPO=owner/repo CAMERATA_GITHUB_ISSUE=1 \
    cargo run -p camerata -- worktracker-live
  ```
- **List a Projects v2 board** (spans repos):
  ```bash
  CAMERATA_GITHUB_TOKEN=… CAMERATA_GITHUB_PROJECT_OWNER=you \
    CAMERATA_GITHUB_PROJECT_NUMBER=1 cargo run -p camerata -- projects-live
  ```
  One board yields stories across different repos, each with its own source +
  target. (Surfacing the board as a cockpit view is the next UI step; the engine is
  built and proven by the CLI.)

Design rationale: [`decisions/2026-06-15_credential_delegated_scope_and_build_targets.md`](decisions/2026-06-15_credential_delegated_scope_and_build_targets.md).

---

## 3. Steer a story through its lifecycle (the cockpit)

Select a story in the spine. The center stage has five **clickable** tabs — click to
preview any stage; the highlighted one is where the story actually is:

1. **Intake** — adopted into the spine.
2. **Investigation** — the lead engineer raises clarifying questions via the
   **bridge** (posts a comment on the tracker item; the requirements owner answers
   there; the answer comes back). Review before posting.
3. **Plan** — **decompose** the story into component child stories per your practice;
   each child is independently governable and targets its own repo.
4. **Status (execution & gating)** — press **"Run this story (governed)"**. The
   governed fleet runs under the gate:
   - **Layer 1** denies a forbidden write *before* it touches disk (at the MCP tool
     boundary).
   - **Layer 2** re-checks each task after (`fmt`/`clippy`/`test`).
   - The **worktree jail** structurally confines every write to the story's worktree.
   Real deny/allow verdicts stream into the panel. *Without `CAMERATA_LIVE_BUILD=1`
   this runs in a token-free scripted mode — the agent is scripted but the gate
   deciding is the real one. With the flag set (and `claude` connected), it's a real
   `claude -p` fleet.*
5. **QA & sign-off** — review the diff + gate results, then sign off. Provenance (PR
   links, gate verdicts, sign-off) is written back to the tracker item.

The **Inspector** (right rail) lists the gate's actual enforced rules, live from the
engine.

---

## 4. The rules that govern it

Four enforcement points, all deterministic (binary pass/fail, no LLM judgement):

| Point | Enforces on | Example |
|---|---|---|
| **Layer 1** (MCP tool gate) | one write's file content, before it executes | no hardcoded secret reaches disk |
| **Layer 2** (CheckRunner) | one task's diff, after | `fmt`/`clippy`/`test` |
| **Integration gate** | the assembled tree (cross-agent) | API contract between two agents agrees |
| **VCS-action gate** | commit/PR/branch **metadata** | the PR title + commit subject carry `AB#<id>` |

Rule scopes: **corpus-global** (shipped), **repo-local** (from onboarding),
**cross-repo** (contracts), **process** (your workflow conventions, e.g. the
`AB#{id}` ticket link — `ProcessRule::ado_ticket_link()`). The process gate is
complete by construction: the agent has no `git`, so Camerata is the sole committer.

Design rationale:
[`decisions/2026-06-15_cross_agent_integration_gate.md`](decisions/2026-06-15_cross_agent_integration_gate.md),
[`decisions/2026-06-15_process_rules_and_vcs_action_gate.md`](decisions/2026-06-15_process_rules_and_vcs_action_gate.md).

---

## 5. The whole loop, in one line

**Connect GitHub + Claude → onboard the repo (brownfield audit + arm, or greenfield
scaffold) → adopt/list stories → investigate → decompose → run governed → review →
sign off → provenance written back.**

## What's live now vs. what's the next build

- **Live (clicks/works on a clean slate):** every cockpit view and tab, the onboarding
  view (connection-gated), adopting issues, listing Projects boards (CLI), governed
  runs (scripted or live), the four enforcement gates' deterministic cores,
  source/target story model.
- **Needs the connections to exercise:** anything hitting GitHub (adopt, scan,
  Projects) needs the token; a real `claude -p` fleet needs `claude` + `CAMERATA_LIVE_BUILD=1`.
- **Next backend builds (flagged, not faked):** the brownfield scan/audit/arm engine
  (the view + flow exist; the repo-scanning engine is the build), the Projects board
  as a cockpit view, and wiring the VCS-action gate into the live commit/PR path.
