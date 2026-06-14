# PHASE0_RUST.md

Re-map of the Phase-0 task plan (`PHASE0_TASKS.md`, T0-T14) onto the
verified all-Rust architecture (`ARCHITECTURE.md`, `RUST_CORE_VERIFICATION.md`).

The task LOGIC and sequencing survive unchanged. What changes is the
implementation language (Rust throughout), the module locations (crates instead
of `src/` subdirectories), and, in two cases, the gate mechanism (MCP gateway
replaces `PreToolUse` hooks). The dependency graph and the thesis-proving
sequence (T0 -> T4 -> T5 -> T8 -> T9) are identical.

---

## First vertical slice (minimal end-to-end proof)

The five tasks that prove the engine works, in critical-path order:

```
T0  (scaffold + auth + persistence)
  |
T4  (GovernanceGateway — DONE: verified, promoted to crates/gateway)
  |
T5  (AgentDriver — STARTED: ClaudeCliDriver stub in crates/agent)
  |
T8  (post-task gate + bounce loop)
  |
T9  (planted-violation acceptance run — PROVES ENFORCEMENT)
```

Nothing in T1-T3 / T6-T7 / T10-T14 is required for the gate to fire. Build
this slice first; dress the rest after T9 passes.

---

## Task re-map

| Task | Status | Crate(s) | What changes vs the TS plan |
|------|--------|----------|-----------------------------|
| **T0** Scaffold + auth smoke test + persistence | CHANGED | `core`, `persistence`, `cli` | No `package.json`; `Cargo.toml` workspace already exists. `persistence/store.ts` becomes `camerata-persistence` (sqlx + SQLite). Config layout is `.camerata/config.toml` (unchanged) and `~/.config/camerata/` (unchanged). Auth smoke test is still `ANTHROPIC_API_KEY` fed to `claude -p` (no SDK call from Rust itself; that is how the Rust stack works). |
| **T1** Corpus loader + 106-rule index | LANGUAGE-AGNOSTIC | `rules` | `rules/corpus.ts` -> `camerata-rules` crate. TOML parsing: `toml` crate. Same logic: load all 106 files, build `RuleIndexEntry` per Q5, assert count == 106. Configured corpus root, not hardcoded path. |
| **T2** Three-state bucket classifier | LANGUAGE-AGNOSTIC | `rules` | `rules/bucket.ts` -> module in `camerata-rules`. Same three states, same collapse of the `qualifies` field, same off-by-one comment. |
| **T3** CheckRunner + ESLint reuse + rule-id map | CHANGED | `checks` | `checks/CheckRunner.ts` -> `CheckRunner` trait already defined in `camerata-core`. `TypeScriptCheckRunner.ts` -> `EslintCheckRunner` in `camerata-checks`: shells `npx eslint --format json` from a worktree (unchanged), parses violations, maps to Camerata ids (unchanged). `RustCheckRunner` stub registered. The optional `ts-morph` sidecar (TS-AST checks that lint cannot express) is a P2+ subprocess; it is NOT part of the engine and never was. |
| **T4** Layer-1 governance gate | DONE | `gateway` | `agents/hooks.ts` (`PreToolUse` binding) -> **Rust MCP server** in `crates/gateway` (verified, promoted, `rmcp` v1.7). The `GovernanceGateway` trait is in `camerata-core/lib.rs`. The `claude -p` agents are locked to `--strict-mcp-config` + `--disallowedTools` (proven in `RUST_CORE_VERIFICATION.md`). `PreToolUse` hooks are NOT used; the MCP gateway is stronger (closes the subagent-bypass hole). The two hard Frontend boundaries (no raw DB commands, path confinement) are the first real rules implemented here beyond the GOV-1 proof-of-concept. |
| **T5** Agent session wrapper | STARTED | `agent` | `agents/session.ts` -> `ClaudeCliDriver` in `crates/agent/src/lib.rs` (stub exists, implements `AgentDriver` trait from `core`). TODOs in the stub: wire per-role `allowed_tools`, `cwd` to role worktree, rule-subset injection into the system prompt, `stream-json` for live status. Session auth is still `ANTHROPIC_API_KEY` env var. Model seam: nothing outside `crates/agent` may open a model session. |
| **T6** Role entity + scoping + boundaries | LANGUAGE-AGNOSTIC | `core`, `rules` | `roles/role.ts` -> `Role` struct already in `camerata-core/lib.rs` (`name`, `rule_subset`, `allowed_paths`). `roles/scoping.ts` -> module in `camerata-rules`: `domain -> role_scope` lookup, rule-subset slicing per role. Two concrete roles (Backend, Frontend) defined here. Identical semantics to the TS plan. |
| **T7** Worktree coordinator (sequential) | LANGUAGE-AGNOSTIC | `server` or `core` | `coordinator/*.ts` -> modules in `camerata-server` (or a `coordinator` module in `camerata-core`). `git worktree add/remove` is still a subprocess call (`std::process::Command` / `tokio::process::Command`). Contract artifact pattern (single typed contract as the FE/BE shared surface) is unchanged. Sequential scheduling is unchanged. |
| **T8** Post-task gate + bounce loop | LANGUAGE-AGNOSTIC | `checks`, `server` | `gate/postTask.ts` + `gate/bounce.ts` -> `camerata-checks` (runs `CheckRunner::check`) + coordinator logic in `camerata-server`. Configurable max-revision ceiling from `.camerata/config.toml` is unchanged. Bounce format (rule id + file:line + fix suggestion) is unchanged. |
| **T9** Planted-violation acceptance run | LANGUAGE-AGNOSTIC | `cli`, `checks` | Same scenario: Frontend agent, planted `db.select` (ARCH-STRICT-LAYERING-1) + planted `next/image` (UI-IMAGE-COMPONENT-1). Same assertion: (a) MCP gateway denies the live DB command; (b) post-task gate catches the structural violation; (c) agent revises; (d) re-run passes; (e) diff integrates clean. Fixture/worktree teardown unchanged. |
| **T10** Investigation driver + rule-selection pass | LANGUAGE-AGNOSTIC | `server`, `rules` | `investigation/runner.ts` -> module in `camerata-server`. Opens one `claude -p` session via `AgentDriver`. Blast-radius triage, findings, recommended RuleSet, clarifying questions: all identical. `investigation/ruleSelection.ts` -> `camerata-rules`: deterministic post-processing (validate ids, re-derive enforcement_kind, slice by role_scope): identical. |
| **T11** Provenance line per change | LANGUAGE-AGNOSTIC | `persistence` | `provenance/trail.ts` -> `camerata-persistence`. One record per change: `task_id`, `role`, `session_id`, `rules_passed[]`. Stored in SQLite (was "SQLite or flat file" in the TS plan; Rust nails it to SQLite via sqlx). |
| **T12** Human-QA presentation | LANGUAGE-AGNOSTIC | `cli` | `cli/main.ts` QA output -> `crates/cli/src/main.rs`. Same content: governed diff, provenance line, `deterministic-declared` + `review-heuristic` rules surfaced for human attention. |
| **T13** Brownfield onboarding | LANGUAGE-AGNOSTIC | `server`, `rules` | `onboarding/brownfield.ts` -> module in `camerata-server`. MAP + PROPOSE + INSTALL steps unchanged. The generated scaffolding (lint config, CI steps, agent rules, hooks) is identical. The install is still emitted as a human-approvable diff, never applied silently. |
| **T14** End-to-end CLI wire-up | LANGUAGE-AGNOSTIC | `cli` | `cli/main.ts` -> `crates/cli/src/main.rs`. Full flow: Story intake -> brownfield onboarding (T13) -> investigation (T10) -> two-role sequential run (T7) -> planted-violation gate (T9 path) -> provenance (T11) -> QA (T12). One run on `ANTHROPIC_API_KEY` against Agora. |

---

## Status counts

| Status | Tasks | Notes |
|--------|-------|-------|
| DONE | T4 | Gateway verified + promoted; the MCP gate runs in Rust with sub-ms latency |
| STARTED | T5 | `ClaudeCliDriver` stub in `crates/agent`; per-role wiring is the remaining TODO |
| CHANGED | T0, T3 | T0: persistence moves from a stub to `camerata-persistence` (sqlx); T3: `TypeScriptCheckRunner` replaces the TS hook-based `CheckRunner.ts`, no `ts-morph` in Phase 0 |
| LANGUAGE-AGNOSTIC | T1, T2, T6-T14 | Same logic, same interfaces, same acceptance criteria — Rust crate instead of TS module |

---

## Crate-to-task mapping (reference)

| Crate | Tasks that land here |
|-------|----------------------|
| `camerata-core` | T0 (types), T4 (GovernanceGateway/CheckRunner/AgentDriver traits), T6 (Role struct) |
| `camerata-gateway` | T4 (MCP server, DONE) |
| `camerata-agent` | T5 (ClaudeCliDriver, STARTED) |
| `camerata-rules` | T1 (corpus loader), T2 (bucket classifier), T6 (scoping + rule-subset slicing), T10 (rule-selection post-processing) |
| `camerata-checks` | T3 (EslintCheckRunner, RustCheckRunner stub), T8 (post-task gate, bounce loop) |
| `camerata-persistence` | T0 (SQLite store schema), T11 (provenance records) |
| `camerata-server` | T7 (worktree coordinator, handoff, integrate), T8 (coordinator side of bounce), T10 (investigation runner), T13 (brownfield onboarding) |
| `camerata-cli` | T0 (entry point), T9 (acceptance run harness), T12 (QA presentation), T14 (full wire-up) |
| `camerata-ui` | Phase 0 out of scope (cockpit is a Phase 1 deliverable; Phase 0 is CLI-only) |

---

## Key architectural deltas vs the TS plan

1. **No `PreToolUse` hooks.** Layer-1 gate is the Rust MCP server, not a static
   hook script. Stronger: closes the subagent-bypass hole; verified empirically.
   Every reference to `agents/hooks.ts` in the TS plan maps to `crates/gateway`.

2. **No Agent SDK.** `session.ts` wrapped `query()` from the TS Agent SDK. In
   Rust there is no Agent SDK; `ClaudeCliDriver` shells `claude -p` directly.
   The seam is the `AgentDriver` trait in `camerata-core`; the effect is identical
   (one spawned agent per role, scoped, gated).

3. **No BFF.** The old TS design needed a BFF to bridge a TS core to a Rust UI.
   All-Rust: the Axum HTTP/WS server (`camerata-server`) is embedded in the same
   process as the orchestrator. Layer collapses; no separate process to deploy.

4. **`ts-morph` is a P2+ subprocess, not Phase 0.** T3 noted it as a stub
   (`RustCheckRunner` registered, not implemented). Nothing changes; the stub
   registration is still correct. When Phase 0 governs TypeScript target code,
   `EslintCheckRunner` is sufficient; AST-level checks that need `ts-morph` are
   deferred.

5. **SQLite is definite, not "SQLite-or-flat-file".** `camerata-persistence`
   uses `sqlx`. The schema from VISION section 8 (Stories, RuleSets, Provenance,
   FeatureStatus) is unchanged.
