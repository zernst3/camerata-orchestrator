# UoW delegate tool — Increment 2 (live, gate-preserving delegation)

**Date:** 2026-06-21 · Status: implemented (branch `feat/uow-delegate`).
Builds on `2026-06-21_uow_dev_orchestrator_tier_delegation.md` (TARGET design B)
and Increment 1 (tiered dev run + per-stage model resolution).

## What this adds

A live, governed `delegate` MCP tool. At run time the orchestrator (the lead, on
the strongest tier) can hand a well-scoped subtask to a lower tier; the gateway
spawns a single GATED child `claude -p` on that tier's model, in the same
worktree, runs it synchronously, and returns the child's full output to the
orchestrator. Escalation is parent-driven. The gate stays universal.

## The architecture fork, and the choice

Two wirings for "where does the spawn happen":

- **(A) route the delegate request back to the fleet process** — the gateway
  signals the fleet, which spawns the child. Needs cross-process coordination
  (the gateway is a separate stdio subprocess from the fleet) and a request/reply
  channel.
- **(B) the gateway spawns a gated child itself** — self-contained, no
  cross-process coordination.

**Chosen: B.** The gateway already owns the rule subset and the worktree jail, and
it already knows how to gate. Reusing `camerata-agent`'s spawn machinery
(`ClaudeCliDriver`, `allowed_tools_for_role`) to launch the child from inside the
gateway keeps the whole thing in one place, so the gate is trivially preserved:
the child is constructed gated, by the same code that gates the orchestrator.
(`camerata-agent` depends only on `camerata-core` + `camerata-worktracker`, so
`camerata-gateway → camerata-agent` adds no normal-build cycle.)

## The tool

- Constant: `camerata_agent::DELEGATE_TOOL = "mcp__camerata__delegate"` (same
  `camerata` server key as `gated_write`).
- Input: `{ "subtask": "<clear instruction>", "tier": "fast" | "balanced" }`.
- Registered on the gateway's `tool_router` unconditionally, but the HANDLER
  refuses unless the gateway booted in **orchestrator mode** (`self.orchestrator`
  is `Some`). Non-lead gateways are not in orchestrator mode, so they refuse.
- Handler (`crates/gateway/src/delegate.rs::run_delegated`): check the depth
  guard → resolve `tier → model` → write a per-child session (rules + child
  mcp-config) → build a `ClaudeCliDriver` that is **NOT** an orchestrator (so its
  `--allowedTools` excludes `delegate`), pinned to the tier model and the shared
  worktree → run `claude -p` → return the child's output.

## Orchestrator-mode gating (two independent layers)

1. **`--allowedTools` (primary).** `allowed_tools_for_role_with_mode(role,
   orchestrator)` adds `delegate` only when `orchestrator == true`. The fleet sets
   `as_orchestrator(true)` on the lead driver ONLY. Delegate children are spawned
   with `as_orchestrator(false)`, so the CLI never even offers them the tool.
2. **Per-process handler refusal.** The gateway reads orchestrator-mode config
   from env (`CAMERATA_DELEGATE_ENABLED`, `CAMERATA_DELEGATE_MODELS`,
   `CAMERATA_GATEWAY_BIN`, `CAMERATA_DELEGATE_DEPTH`, `CAMERATA_DELEGATE_MAX_DEPTH`)
   via `OrchestratorConfig::from_env()`. Only the LEAD stage's mcp-config carries
   `CAMERATA_DELEGATE_ENABLED=1`, so only its gateway is in orchestrator mode;
   every other gateway's `delegate` handler refuses.

The fleet only writes the orchestrator env into the LEAD session's mcp-config
(`crates/fleet/src/orchestrator.rs::render_orchestrator_mcp_config`); the lead is
the FIRST task classified into the `Strongest` band (`lead_stage_index`). If no
task is strongest, no agent is the orchestrator and the run simply has no
delegation — fully back-compatible.

## Depth guarantee + explicit guard

- **Structural depth-1 (primary).** A delegate child is spawned with
  `orchestrator = false`, so `delegate` is absent from its `--allowedTools` AND its
  child gateway is launched without `CAMERATA_DELEGATE_ENABLED`. It cannot
  re-delegate. Depth is inherently 1.
- **Explicit counter (belt-and-suspenders).** `OrchestratorConfig` carries
  `depth` / `max_depth` (default `0` / `1`). `run_delegated` refuses with
  `DelegateError::DepthExceeded` once `depth >= max_depth`, and threads `depth + 1`
  into the child's gateway env (`CAMERATA_DELEGATE_DEPTH`). So even a
  misconfiguration that re-enabled orchestrator mode on a child would see depth=1
  and refuse at the cap. Two independent reasons recursion cannot happen.

## Parent-driven escalation

A delegate child NEVER calls "up." It either completes the subtask through
`gated_write`, or returns a message starting with `INCOMPLETE:` (the child prompt
instructs this; a failed child run is also wrapped with an `INCOMPLETE:` line).
The orchestrator reads the returned tool result and decides: do it itself, or
re-delegate to a higher tier. No child→parent callbacks exist, so there is no
up-call surface to secure.

## Gate-preservation proof

1. The raw CLI `Task` tool stays on `disallowed_builtins` for EVERY agent
   (unchanged in `crates/agent/src/lib.rs`; asserted by
   `escape_tools_are_explicitly_denied_and_never_allowed` and by the orchestrator
   test that confirms `Task` is still disallowed even for the lead).
2. The ONLY spawn path is `delegate`, and the gateway — not the agent — performs
   the spawn. The agent cannot spawn anything itself (Task denied, Bash denied).
3. Every spawned child is born gated: same `gated_write`-only tool surface, same
   worktree jail (`CAMERATA_WORKTREE_ROOT`), same rule subset as the orchestrator
   (the gateway passes its own `rule_subset` to the child). The child's gateway is
   the same binary, so identical `evaluate_call` logic runs.
4. `delegate` is granted to exactly one agent (the lead) and that agent's children
   get only `gated_write`. The capability does not propagate.

Therefore no path through `delegate` weakens the gate, and no path creates an
ungoverned writer.

## Tests (token-free, no real spawning in CI)

- agent: `delegate` absent for non-orchestrator agents; present only in
  orchestrator mode; `build_args` includes it only for the orchestrator driver;
  `Task` still disallowed for the orchestrator.
- gateway/delegate: tier→model resolution (case-insensitive), depth guard
  permits-below/refuses-at-max, `run_delegated` refuses unknown tier AND refuses
  at the depth cap WITHOUT spawning (no token spend), child role/allowlist has no
  delegate, child mcp-config disables delegate + increments depth.
- fleet/orchestrator: lead = first strongest task (and `None` when no strongest);
  orchestrator mcp-config enables delegate with the full env; the per-tier models
  JSON has all three tiers; prompt suffix mentions delegate + escalation.

## Files

- `crates/agent/src/lib.rs` — `DELEGATE_TOOL`, `allowed_tools_for_role_with_mode`,
  `ClaudeCliDriver::orchestrator` + `as_orchestrator`, re-export `WORKTREE_ROOT_ENV`.
- `crates/gateway/src/delegate.rs` — orchestrator config, child spawn, depth guard.
- `crates/gateway/src/main.rs` — the `delegate` `#[tool]`, orchestrator field.
- `crates/fleet/src/orchestrator.rs` — lead selection, orchestrator session +
  mcp-config, prompt suffix.
- `crates/fleet/src/lib.rs` — `build_from_plan_with_tier_map` spawns the lead in
  orchestrator mode.
