# Every in-project agent is grounded in the project's repo + rules

**Date:** 2026-06-25
**Status:** Implemented (branch `fix/universal-agent-grounding`)

## The invariant

**Every agent invoked on behalf of a project MUST be grounded in (a) the
project's repo context and (b) the project's rule context — no exceptions,
regardless of which feature invokes it.**

An agent is the thing "use an agent to do X" promises understands the actual
codebase. An ungrounded agent is a bare LLM call with a generic mental model;
it cannot honor that promise.

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

## Follow-on (not in this change)

Full live code-reading tool access per agent (a generalization of issue #87) is
the deeper follow-on: bare-LLM agents would gain the ability to read arbitrary
repo files on demand rather than only the up-front digest. The digest is the
floor that makes every agent grounded today; live tool access raises the
ceiling later.
