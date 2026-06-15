# Agent-cage hardening backlog

> The headline claim is "the model cannot touch disk except through the gate." That is
> only as strong as the cage around the agent. This tracks the seams in that cage (from
> a 2026-06-15 architecture review) with honest status, so the claim is defended, not
> assumed.

## 1. Denylist completeness (the escape-hatch surface) — PARTIALLY HARDENED

"No escape hatch" rests on a denylist (`--disallowedTools`), not a pure allowlist, so it
is fragile by construction: the guarantee is "these tools are denied," not "only these
are permitted."

- **Done:** `Task` (subagent spawning) is now on the explicit denylist alongside
  `Bash`/`Write`/`Edit`/`MultiEdit`/`NotebookEdit`, with a unit test
  (`escape_tools_are_explicitly_denied_and_never_allowed`) asserting the escape set is
  denied AND never on the allowlist. A subagent that could regain `Write`/`Bash` is now
  blocked even if `--allowedTools` is not strictly exclusive.
- **Open:**
  - Verify with a LIVE test that `--allowedTools` is exclusive under
    `--dangerously-skip-permissions` (an unlisted tool is denied, not auto-approved).
    Needs the CLI + a token.
  - Pin the Claude Code CLI version; re-audit the denylist on every upgrade. A new
    write/exec/spawn tool added by the CLI must be added to the denylist.
  - Add a live test: an explicit "write via `Bash`" and "spawn a subagent that writes"
    attempt is blocked end to end.

## 2. Guard the guard (config tamper) — CONFIRMED SAFE

The agent's own `rules.json` / `gateway.json` must not be writable by the agent, or the
thing being governed could edit its own governance.

- **Confirmed:** `prepare_session` writes both files into a per-session `session_dir`,
  a SIBLING of the worktree (`root/session-N` vs `root/crate`). Only the worktree is
  `--add-dir` scoped, so the config dir is outside the agent's writable scope.
  Additionally, `SEC-NO-PATH-ESCAPE-1` denies `..` traversal through `gated_write`.
- **Open (regression guard):** add a test asserting a `gated_write` targeting the
  session config path is denied / out of scope, so the invariant cannot silently
  regress on a refactor.

## 3. Two "blocked" signals are distinct — OPEN

A `permission_denial` (the model reached for `Bash`/`Write`/`Task` — an ESCAPE attempt)
is a different signal from a gateway deny (it used `gated_write`, but the content
violated a rule).

- `AgentOutcome` already captures `denials` (Claude's `permission_denials`) separately
  from gateway denies (which surface as tool errors).
- **Open:** track and surface them separately. The escape-attempt rate (how hard the
  model fights the cage) is independently valuable, for UX and for the "is the gate
  doing real work" story.

## 4. Layer 2 is model-agnostic but LANGUAGE-specific — OPEN (scoping)

- Layer 1 (MCP deny-before-execute) governs any MCP-speaking agent; Layer 2
  (`cargo fmt`/`clippy`/`test`) governs any model, but only for RUST.
- A multi-language product needs per-language Layer-2 toolchains (eslint / prettier /
  tsc for JS/TS, etc.) as additional `CheckRunner` implementations.
- "Model-agnostic" must not be heard as "language-agnostic", it is not yet. The corpus
  side of this is tracked in [`RULE_COVERAGE.md`](RULE_COVERAGE.md).
