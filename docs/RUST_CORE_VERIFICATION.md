# RUST_CORE_VERIFICATION.md

**Decision: GO for a full-stack Rust core.**

> This supersedes the earlier NO-GO verdict (Haiku 4.5, 2026-06-13). That
> verdict was wrong. Its two empirical facts were correct (no official Rust
> *Agent* SDK; Claude Code CLI `PreToolUse` hooks are static), but the
> conclusion drawn from them ("the orchestrator core must be TypeScript") does
> not follow. The load-bearing governance gate runs in Rust, proven below.

**Verified by:** Opus 4.8, 2026-06-13, with an end-to-end empirical slice (a real
Rust MCP server gating live `claude -p` agents). Confidence: HIGH (working code +
two passing test runs, not doc reasoning).

---

## The decision in one paragraph

The orchestrator core, the governance gate, the persistence layer, and the UI are
all Rust. Agents run as `claude -p` CLI subprocesses (the CLI is fully
headless-capable, which the prior doc itself confirmed). The real-time governance
gate is implemented as a **Camerata-owned MCP server written in Rust**: agents are
locked to only the gateway's tools, and every tool call routes through Rust code
that evaluates the active rule-subset and allows or denies before any side effect.
This is the `GovernanceGateway` "MCP tool-gateway binding" that `TECH_DESIGN.md`
already specifies as the model-agnostic path. The only non-Rust piece is an
*optional* `ts-morph` sidecar for TypeScript-AST-specific post-task checks, which
is a subprocess (callable from anything), only relevant when the *governed target
code* is TypeScript, and not needed for Phase 0.

---

## What was empirically verified

All of the following was run on this machine (Claude Code CLI v2.1.123), not
inferred from docs.

### 1. The official Rust MCP SDK exists and works

`cargo add rmcp` resolved **rmcp v1.7.0** (the official `modelcontextprotocol`
Rust SDK) and a minimal MCP server compiled in ~7s. Contrast with the *Agent* SDK,
which has no Rust binding. The distinction matters: governance routes through
**MCP**, which has first-party Rust support, not through the Agent SDK.

### 2. A Rust MCP server can gate an agent's tool calls, in-process, by rule

A ~90-line Rust server (`rmcp` 1.7) exposed a single `gated_write` tool. Its
handler evaluates a rule (`GOV-1`: deny writes to paths containing "forbidden")
*before* touching the filesystem, and returns allow/deny. This is the governance
gate.

### 3. `claude -p` can be locked to ONLY the gateway's tools

Agents were spawned with:

```
claude -p "<task>" \
  --strict-mcp-config --mcp-config gateway.json \
  --disallowedTools "Bash Write Edit MultiEdit NotebookEdit" \
  --dangerously-skip-permissions --output-format json
```

`--strict-mcp-config` restricts the agent to the configured MCP server;
`--disallowedTools` removes every built-in file-writing tool. The agent's ONLY way
to write a file is the gated MCP tool. There is no escape hatch.

### 4. The gate enforces, deny-before-execute, with negligible latency

| Test | Agent acted via | Rust gate (GOV-1) | Gate latency | Filesystem result |
|---|---|---|---|---|
| Allowed write | `gated_write` | allow | **653µs** | file written |
| Forbidden write | `gated_write` | **deny** | **7µs** | file never created |

In the deny case the agent itself reported: *"The write was denied. The Camerata
governance gateway blocked it with rule GOV-1... The file was never written."* The
gate decision is sub-millisecond; total run wall-clock (~11s) is dominated by model
inference, not the gate.

---

## Why the prior NO-GO was wrong

The earlier doc reasoned: the gate needs *in-process `PreToolUse` hooks with
closure access to role context*; only the TS/Python Agent SDK provides those;
therefore the core must be TS. The errors:

1. **It elevated a sequencing choice into a necessity.** `TECH_DESIGN.md` §2 Q2
   states plainly: *"The gate LOGIC (rule → allow/deny) is written against the
   `GovernanceGateway` interface and MUST NOT assume Claude hooks. The PreToolUse
   hook is one binding, not the gate."* It then names the **MCP tool-gateway
   binding** as the model-agnostic path that *"validates every call in-process
   before executing it, regardless of provider... closes the subagent-deny gap."*
   The NO-GO doc fixated on the "Claude PreToolUse binding ships first" option and
   treated it as the only option.
2. **It missed that MCP has an official Rust SDK.** The blocker it found was "no
   Rust Agent SDK." But governance does not need the Agent SDK; it needs MCP, and
   `rmcp` is first-party and mature.
3. **It dismissed the right pattern as a "workaround."** Its three rejected
   workarounds all tried to push the *rules* out to a static hook (env var, temp
   file, hardcode). The correct inversion pushes the *question* to the
   orchestrator: the gateway is the orchestrator's own process, it already knows
   `session_id → role → rule-subset`, so the tool call is evaluated against live,
   in-memory, data-driven rules. No static config, no closure gymnastics.

The MCP-gateway approach is not a downgrade. It is **stronger** than SDK hooks: the
agent can act *only* through gated tools, so even nested/subagent calls route
through the gate (the prior doc flagged the subagent-bypass hole for SDK hooks).

---

## Residual unknowns (small, and how to close them)

The slice proved feasibility and per-call latency. Two things it did NOT exercise,
worth a second short slice before locking the Phase-0 plan:

1. **Per-role rule-subset lookup.** The slice hardcoded one rule. The real gateway
   keys the rule-subset by `session_id` (the orchestrator assigns it at spawn).
   This is a hashmap lookup the orchestrator owns; low risk, but verify the
   `session_id` reaches the gateway on each call (the CLI passes it; confirm via
   the MCP request context).
2. **Behavior under parallel agents + many tool calls.** The slice was sequential.
   Confirm the gateway handles concurrent sessions (one MCP server instance vs.
   one-per-agent) and that latency holds under load.

Neither is expected to be a blocker. Both are measurable.

---

## Architecture implication

Going all-Rust **removes** the cross-stack seam the old design carried: the Rust UI
no longer needs a separate BFF to reach a TypeScript core, because the core is
Rust. One language, one process tree, one type system from the cockpit to the gate.
See `ARCHITECTURE.md` for the full map.

---

## Evidence

Working slice preserved at `/tmp/camerata-verify/` (the `rmcp` gateway crate + the
two recorded `claude -p` runs and the gateway decision log). The gateway's gate is
the `Gateway::evaluate` + `gated_write` handler; reproduce with the two
`claude -p` commands above pointed at `gateway.json`.

**Verification sources:** live `claude -p` runs (v2.1.123); `rmcp` v1.7.0 from
crates.io (built locally); `TECH_DESIGN.md` §2 Q2 (the `GovernanceGateway` seam and
MCP-gateway binding the verification relies on).
