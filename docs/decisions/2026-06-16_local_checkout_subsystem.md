# Local-checkout subsystem: run it locally before you push

Status: accepted (2026-06-16)

## Context

The brownfield onboarding/arm flow writes governance metadata (`AGENTS.md`,
`CONVENTIONS.md`, `.camerata/rules.json`) straight into a PR via the GitHub Contents
API, with no local working copy. That is correct for *metadata* — a tiny reviewable
config change nobody needs to run.

It is **wrong** for *code* changes. When the governed fleet edits real source, the
developer must be able to run and test the app locally with those changes **before**
anything is pushed. Generating code straight into a GitHub PR with no local checkout
removes the "run it first" step entirely.

## Decision

Repo CONTENTS live on disk in a local working copy; only project configs/pointers
persist server-side (see the project-container ADR). The lifecycle is:

**clone (or pull) → fleet edits on a working branch → developer runs/tests locally →
explicit ship (push + open PR).** Nothing auto-merges.

### Where checkouts live

A single **visible workspace folder the architect picks** (chosen via a native OS
folder dialog, `rfd`). Repos clone at `<workspace_root>/<owner>/<repo>` so the
developer can `cd` in, open an editor, and run the app the normal way. The choice
persists in `settings.json` in the per-user data dir, next to `projects.json`.

Rejected: a hidden app-data dir (buried, awkward to open) and per-project folders
(more prompts, little benefit for a single-user desktop tool).

### Git mechanism

Shell out to the system `git` (the desktop dev tool assumes git is installed; it
inherits the user's credentials/SSH config). Async via `tokio::process::Command`.

The token is injected ONLY into transient network commands (clone / fetch / push) via
an `x-access-token:<token>@github.com/...` URL, and the persisted `origin` is rewritten
to the tokenless URL immediately after clone — the secret never lands in
`.git/config` on disk.

`fetch` + `merge --ff-only` for updates, so a dirty/diverged local tree is never
clobbered; it is reported as-is.

## Surface

Server (`camerata-server`):
- `settings.rs` — `SettingsStore` (persisted `workspace_root`).
- `workspace.rs` — `clone_or_pull`, `checkout_status`, `create_branch`, `ship`
  (push + `open_pr`). `RepoCheckout { repo, cloned, path, branch, dirty, detail }`.
- Endpoints: `GET/POST /api/settings`, `POST /api/settings/workspace`,
  `GET|POST /api/projects/:id/checkout`, `POST /api/projects/:id/branch`,
  `POST /api/projects/:id/ship`.

UI (`camerata-ui`): a **Workspace** cockpit tab — folder picker, per-repo checkout
cards (status / branch / dirty / path), "Clone / update all repos", "Start branch",
and "Ship (push + PR)" with a returned PR link.

## Boundary with arm/emit

Governance metadata stays API-direct-to-PR (no checkout needed). The local-checkout
path is exclusively for code work. The two are deliberately separate.

## Tests

`settings.rs`: set/get/clear, persistence across reload. `workspace.rs`: path
nesting, token inject/scrub, not-cloned status, and a full local git round-trip
(clone → branch → status → dirty) against a file-based "remote", no network.
