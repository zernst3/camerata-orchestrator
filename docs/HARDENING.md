# Agent-cage hardening backlog

> The headline claim is "the model cannot touch disk except through the gate." That is
> only as strong as the cage around the agent. This tracks the seams in that cage (from
> a 2026-06-15 architecture review) with honest status, so the claim is defended, not
> assumed.

## 1. Denylist completeness (the escape-hatch surface) — PARTIALLY HARDENED, LIVE-PROBED

"No escape hatch" rests on a denylist (`--disallowedTools`), not a pure allowlist, so it
is fragile by construction: the guarantee is "these tools are denied," not "only these
are permitted."

**Live probe (2026-06-15, CLI 2.1.123), the structural-vs-maintenance question, answered.**
Two `claude -p ... --dangerously-skip-permissions` runs in `/tmp` (the flag our driver
passes, [`lib.rs:131`](../crates/agent/src/lib.rs)):

1. `--allowedTools "Read"`, prompt "run `echo` via Bash." → **Bash ran.** Result was
   `CAMERATA_PROBE_EXEC_OK`, `permission_denials: []`. So under `--dangerously-skip-permissions`
   the allowlist is **NOT exclusive** — a tool absent from `--allowedTools` is still live
   and auto-approved. The allowlist buys us nothing in headless mode.
2. `--allowedTools "Read" --disallowedTools "Bash"`, same prompt. → **Bash was gone.** The
   model reported "the Bash tool is not available in this session." `--disallowedTools`
   *removes* the tool from the session entirely; it is structural, not a runtime gate.

Conclusion: the cage is **structurally safe for every tool on the denylist** (removed, not
gated) and the allowlist is decorative under our flags. Therefore cage integrity rests
entirely on the denylist being **complete**, which makes the items below load-bearing, not
hygiene.

- **Done:** `Task` (subagent spawning) is on the explicit denylist alongside
  `Bash`/`Write`/`Edit`/`MultiEdit`/`NotebookEdit`, with a unit test
  (`escape_tools_are_explicitly_denied_and_never_allowed`) asserting the escape set is
  denied AND never on the allowlist. A subagent that could regain `Write`/`Bash` is
  blocked even though `--allowedTools` is not exclusive (now empirically confirmed).
- **Done:** the live probe above confirms `--allowedTools` is non-exclusive under
  `--dangerously-skip-permissions` and `--disallowedTools` is structural.
- **Open (now load-bearing, not optional):**
  - **Pin the Claude Code CLI version**; re-audit the denylist on every upgrade. Because
    the allowlist is non-exclusive, any new write/exec/spawn tool the CLI ships that we do
    NOT add to `--disallowedTools` is live and auto-approved on the next version bump.
    This is the single largest standing risk to the cage.
  - Add a live end-to-end test in the app's own harness: a "write via `Bash`" and a
    "spawn a subagent that writes" attempt, both asserted blocked, run against the pinned
    CLI in CI so a denylist regression or a CLI tool-surface change trips the build.

## 2. Guard the guard (config tamper) — CLOSED via code-level jail

The agent's own `rules.json` / `gateway.json` must not be writable by the agent, or the
thing being governed could edit its own governance.

**Why the first answer was wrong.** The earlier "confirmed safe" leaned on `--add-dir`.
But `--add-dir` only scopes the `claude` process's built-in tools, and those are all on
the denylist anyway. The agent's *only* live write path is `gated_write`, and that write
is performed by the **gateway** process, which Camerata launches with its own full
filesystem permissions. `--add-dir` does not constrain the gateway. Nor does the
rule-level `SEC-NO-PATH-ESCAPE-1` close the gap: it matches `..` segments only and misses
absolute paths (`/work/session-1/rules.json`, `/etc/passwd`) entirely.

- **Closed:** the worktree jail is now a structural invariant in `gated_write`'s code,
  independent of any rule. The gateway reads `CAMERATA_WORKTREE_ROOT`; `within_jail()`
  resolves the target (joining relative paths onto the root), lexically normalizes it,
  and asserts it `starts_with` the canonicalized root. It runs FIRST, before rule
  evaluation, and denies with `DENIED [JAIL: outside the worktree]`. The session config
  dir is a sibling of the worktree (`root/session-N` vs `root/crate`), so a write to
  `rules.json` / `gateway.json` is structurally outside the jail and refused — whether
  the agent reaches for it via `..` OR an absolute path.
- **Regression guard (done):** `jail_tests` asserts a `gated_write` targeting the session
  config path (`/work/session-1/rules.json`, `/work/session-1/gateway.json`) is jailed,
  alongside `/etc/passwd`, sibling-crate, and `..`-climb cases; absolute-inside and
  relative-under-root remain allowed. The invariant cannot silently regress on a refactor.

**Multi-repo read scope rides on this jail.** Since `--add-dir` only widens the
`claude` process's read scope (its built-in writers are denylisted) and never
constrains or widens the gateway, adding the OTHER repos in a multi-repo project to
an agent's read scope (the "every agent reads all the project's repos" invariant —
see `docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md`) is safe by
the SAME argument that closed this gap. The agent can READ every project repo, but
`gated_write` is still jailed to its single `CAMERATA_WORKTREE_ROOT` worktree, so a
write to a sibling repo is structurally outside the jail and refused.
`session.rs::prepare_session_adds_sibling_repos_to_read_scope_but_not_write_jail`
asserts exactly this: the sibling repo is in the read scope while the write jail
stays the single worktree.

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
