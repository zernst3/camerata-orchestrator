# Every in-project agent is grounded in the project's repo + rules

**Date:** 2026-06-25
**Status:** Implemented (branch `fix/universal-agent-grounding`), then
**corrected** (branch `fix/agents-full-repo-access`) to add on-demand full-repo
read, then **corrected again** (same branch) to make the read scope the UNION of
ALL of the project's repos (multi-repo) — see "Correction" and
"Correction (multi-repo)" below.

## The invariant (corrected, multi-repo)

**Every agent invoked on behalf of a project MUST have (a) on-demand READ access
to the ENTIRE of EVERY repo in the active project — a project contains MULTIPLE
distinct repos — and (b) the project's rule context.**

A project is NOT one repo; it is a set of repos (`project.repos: Vec<String>`,
each `owner/repo`), e.g. a separate frontend and backend. Each resolves to its
own local clone. The read scope of any in-project agent is the UNION of all those
clones: the agent MUST be able to scan/read any file in any of the project's repos
(e.g. a frontend unit-of-work reading the backend's API surface). A fixed digest
(rules + per-repo key-file summary) is kept as the cheap always-on BASELINE, but
it is NOT sufficient on its own: the authoritative window is the agent reading the
actual code across all repos. This is QUINTUPLY important for the **developer
agent**, which writes code and must read the real codebase.

An agent is the thing "use an agent to do X" promises understands the actual
codebase. A digest-only agent has a generic mental model of a fixed summary; it
cannot honor that promise the way one that can open any file can.

## Read is ungated; the write gate is unchanged

Reads are safe and ungated. The gated write path (`gated_write` via the MCP
gateway) remains the ONLY write path, unchanged: no built-in writer/exec/spawn
tool (`Write`/`Edit`/`Bash`/`Task`/…) is ever added to any agent's allowlist, and
every governed agent's worktree write-jail (`CAMERATA_WORKTREE_ROOT`) is intact.
This change ADDS read access and fixes the agents' working directory; it does not
loosen writes.

**Multi-repo read does not widen writes.** Adding every project repo to an agent's
read scope is done with `--add-dir`, which grants READ ONLY. A write-class agent
bound to a unit of work keeps its cwd AND its write jail
(`CAMERATA_WORKTREE_ROOT`) on its OWN single worktree; the sibling repos are
added as additional `--add-dir` read windows only. `gated_write` still refuses any
target outside that one worktree, so the agent can READ all the project's repos
but WRITE only to its worktree. This is asserted by a session-level test
(`prepare_session_adds_sibling_repos_to_read_scope_but_not_write_jail`).

## Why (the bug that motivated this)

The story-drafting / clarification agent (`POST /api/uow/:story_id/author`)
behaved like a context-less product owner. For the Camerata repo itself — a
Rust **Dioxus + Axum + SQLx** app with **no authentication** — it asked the
user where to persist a preference across "logged-in users / devices / auth."
That question is incoherent for a no-auth app; it proved the agent had zero
knowledge of the repo or its rules. The root cause was structural: agent call
sites were bare LLM/CLI invocations whose prompts carried only the immediate
task (a form, a story, an escalation), never the project itself.

## Decision

A single shared server-side helper produces, for the active project, a compact
**grounding block** that all agent call sites inject into their prompt:

- **Rule context** — the project's committed ruleset summary plus (pre-onboard)
  the in-progress selected rules. Reuses the chat's renderers
  (`build_ruleset_summary`, `render_selected_rules_for_chat`) so agents and the
  in-app chat see the SAME rule picture.
- **Repo context (digest)** — for each of the project's local repo clones:
  detected stack (languages + frameworks, e.g. "Dioxus + Axum + SQLx, and no
  auth crate" is visible from the manifests), the workspace + member dependency
  manifests (verbatim, truncated), the high-signal docs (`README*`, `CLAUDE.md`,
  `AGENTS.md`, `CONVENTIONS.md`, truncated), and a shallow file/dir tree.

## Where the helper lives

- `crates/server/src/grounding.rs` — pure render functions
  (`render_rule_context`, `render_repo_digest`, `assemble`), secret-file
  redaction, and bounded token budgets (digest/doc/manifest/tree caps).
- `AppState::project_grounding()` (in `crates/server/src/lib.rs`) — resolves the
  active project's local clones via `crate::workspace::resolve_repo_dir`
  (machine-local override path or `<workspace_root>/<owner>/<repo>`), digests
  them off-thread (`spawn_blocking`), and assembles the block.

## Isolation invariants (preserved)

The digest reads ONLY the active project's repos. It never reads another
project's clone, mirroring `list_for_project` / `list_for_repos` /
project-scoped paths. Obvious secret files (`.env*`, `*.pem`, `*.key`,
`id_rsa`, `credentials`, anything containing "secret") are redacted from the
file tree, on top of the gitignore + noise denylist the file reader already
honors. The whole block is size-bounded so it cannot blow an agent's context
window.

## Call sites wired

Bare-LLM agents (the grounding block is their only window onto the repo):

- Story authoring — `uow_author` (the agent that was blind; **highest priority**).
- Story decomposition — `decompose::propose_ai`.
- Escalation review chat + answer translation — `chat_escalation`,
  `escalation::translate_answer_ai`.
- PO-mode intake — `intake::ClaudeLeadEngineer` and
  `intake::ClaudeRefinementReviewer` (new `with_grounding` builders; their
  `build_prompt` takes an `Option<&str>` grounding block).

Agents that run the `claude` CLI inside the repo clone (they can read files
directly, but still receive the rule context + a repo summary and an explicit
instruction to consult the actual repo code):

- Investigation — `investigation_run::investigation_prompt`.
- Brownfield dev-implement — `dev_implement_run::implement_prompt`.
- Update-branch conflict resolution — `update_branch_run::conflict_prompt`.
- PR-feedback resolution — `pr_resolve_run::resolve_prompt`.

## Correction (branch `fix/agents-full-repo-access`)

The original change shipped the digest as the floor and explicitly deferred
"full live code-reading tool access per agent" as a follow-on. That deferral was
wrong: the digest alone left agents unable to read the real code, and the
investigation/intake/bare-LLM paths even ran with the WRONG working directory
(the orchestrator's dir, not the project repo). This correction delivers the
on-demand full-repo read the invariant requires:

- **`prepare_session(.., Some(dir))`** now binds the driver's cwd + `--add-dir`
  to the worktree (previously it set only the gateway write-jail env). Every
  governed agent already carries the read-only built-ins (`Read`/`Grep`/`Glob`/
  `LS`) in its allowlist; binding the cwd is what makes those tools resolve
  against the repo. This single fix gives full-repo read to the worktree runners
  (`dev_implement_run`, `update_branch_run`, `pr_resolve_run`, the fleet).
- **`investigation_run`** now resolves the active project's local clone
  (`AppState::active_repo_dir`) and runs the agent there (cwd + `--add-dir` +
  read tools), so it can read the codebase it analyzes. Its resume path does the
  same. Previously it ran with no worktree and the orchestrator cwd.
- **Intake** (`ClaudeLeadEngineer`, `ClaudeRefinementReviewer`) replaced their
  `--allowedTools ""` lockdown with the read-only tools and a new
  `with_repo_dir` builder that sets cwd + `--add-dir`. These are ungoverned
  planning calls with no write tool, so read access adds no write path.
- **Bare-LLM agents on the `Llm` CLI backend** gained an opt-in
  `LlmRequest::with_repo_read(dir)`: it swaps the hardened `--tools ""` lockdown
  for the read-only built-ins + cwd/`--add-dir`, staying non-agentic (no
  write/exec, slash-commands off). Wired into **`uow_author`** (the reported
  failure) and **`decompose::propose_ai`**. The API backend (no filesystem) and
  any call that does not opt in keep the original `--tools ""` lockdown.
- **`grounding.rs`** dropped the symptom-specific anti-hallucination text (the
  auth/multi-user warnings) in favor of neutral framing: the digest is a cheap
  orientation, the agent has READ ACCESS to the full repo, and it must consult
  the actual code/config rather than assume capabilities or structure.

### Deferred in this correction

- **Escalation** (`escalation::translate_answer_ai`, `chat_escalation`) went
  through the `LlmTranslator` trait whose `complete(system, prompt, model)`
  signature did not carry a repo dir, so on-demand read was deferred here. This
  is now resolved in the multi-repo correction below (the `LlmTranslator` struct
  carries the repo dirs; no trait-signature change was needed).

The original digest path remains the always-on baseline; this correction adds the
on-demand read window the invariant demands.

## Correction (multi-repo) — `fix/agents-full-repo-access`

The first correction wired a SINGULAR `AppState::active_repo_dir` as the agent
read scope — the FIRST resolvable clone of the project's repos. That is wrong for
a multi-repo project: a project contains several distinct repos, and an agent
working on one (e.g. a frontend unit of work) must be able to READ the others
(e.g. the backend's API). This correction makes the read scope the UNION of ALL
of the active project's resolvable repo clones.

- **`AppState::active_repo_dirs() -> Vec<PathBuf>`** resolves the local clone of
  EVERY repo in the active project (`project.repos`), via
  `crate::workspace::resolve_repo_dir`, skipping any that do not resolve to an
  existing folder. Isolation-preserved: it resolves ONLY the active project's
  repos, never another project's. `active_repo_dir()` is retained as a thin
  "primary" accessor (the first of the dirs) for cwd selection.
- **`prepare_session(gateway_bin, role, worktree, read_dirs)`** gained a
  `read_dirs: &[PathBuf]` parameter. The `worktree` still sets the cwd AND the
  `CAMERATA_WORKTREE_ROOT` write jail; each entry in `read_dirs` is added as a
  read-only `--add-dir` (deduped against the cwd). `ClaudeCliDriver` carries
  `extra_read_dirs` / `with_read_dirs` to emit one `--add-dir` per sibling repo.
- **Write-class UoW agents** (`dev_implement_run`, `update_branch_run`,
  `pr_resolve_run`) keep cwd + write jail on their single worktree and pass the
  OTHER project repos (`active_repo_dirs()` minus their own dir) as read-only
  `--add-dir`. They can read across all repos; they write only to their worktree.
- **Project-level agents not tied to one repo** (`investigation_run`, intake
  `ClaudeLeadEngineer` / `ClaudeRefinementReviewer`, `uow_author` / story-author,
  `decompose::propose_ai`, escalation `LlmTranslator`) run with the primary clone
  as cwd and `--add-dir` ALL the project's clones. The CLI-backend `LlmRequest`
  gained `with_repo_read_dirs(..)` (first = cwd, rest = extra `--add-dir`); the
  intake builders gained `with_repo_dirs(..)`.
- **Escalation now wired** (was deferred): `LlmTranslator` carries `repo_dirs:
  Vec<PathBuf>` set from `active_repo_dirs()` and threads it through
  `with_repo_read_dirs` — no trait-signature widening was required.
- **The fleet greenfield builds** pass an empty read scope: they scaffold into a
  throwaway worktree and have no other project-repo clones to read across.

The write gate is unchanged throughout: `--add-dir` is read-only, the single
worktree remains the only writable surface, and `gated_write` stays the only write
tool. See the gate-check test noted in "Read is ungated; the write gate is
unchanged" above.
