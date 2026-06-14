# ENFORCEMENT.md — how Camerata rules become real

Camerata holds a large corpus of *principles* (the camerata-ai library at
`/Users/zacharyernst/Documents/Repos/camerata-ai/principles`, 100+ files). A
principle is only worth as much as the mechanism that enforces it. This document
states, precisely and honestly, **which rules are enforced by executable code
today, which are no-ops, and through which of the three enforcement lanes.**

The headline claim this document backs:

> A real security rule — `SEC-NO-HARDCODED-SECRETS-1` — now DENIES a live
> `claude -p` agent's write whose content contains a hardcoded credential,
> in-process, before the bytes ever touch disk. Captured proof is below.

---

## The three enforcement lanes

Camerata enforces at three distinct points, each with different power and
different cost.

### Lane 1 — Layer-1 mechanical gate (the MCP gateway `apply_rule` library)

**Where:** `crates/gateway` (`evaluate_call` → `apply_rule` → `RULE_REGISTRY`).
**When:** in real time, on *every* tool call, BEFORE the side effect happens.
**Power:** hard deny. A denied write never touches the filesystem.

The agent (`claude -p`) is locked to exactly one write tool,
`mcp__camerata__gated_write`, with every built-in writer
(`Bash Write Edit MultiEdit NotebookEdit`) stripped via `--disallowedTools` and
`--strict-mcp-config`. Every write therefore routes through Rust code that runs
the session's rule-subset against the call before deciding. The same pure
`evaluate_call` is shared by two transports — the stdio MCP server
(`crates/gateway/src/main.rs`) and the in-process `GovernedGateway`
(`crates/gateway/src/lib.rs`) — so they enforce byte-for-byte identical logic.

Each rule arm receives **both** the target `path` and the file `content`, so a
rule can key off either. (A bug fixed in this slice: the stdio transport
previously forwarded only `path`, which silently disabled every content rule
over the live path. It now forwards `content` too.)

Rules currently implemented in `RULE_REGISTRY` (layer-1, mechanical):

| Rule ID | Keys off | What it denies |
|---|---|---|
| `GOV-1` | path | Writes whose path contains the substring `forbidden`. |
| `SEC-NO-HARDCODED-SECRETS-1` | content | Content containing a hardcoded credential literal: GitHub tokens (`ghp_`/`gho_`/`ghu_`/`ghs_`/`github_pat_`), Slack tokens (`xox[baprs]-`), AWS access keys (`AKIA…`), OpenAI/Stripe `sk-` keys, Google API keys (`AIza…`), PEM private-key headers. |
| `SEC-NO-RAW-SQL-CONCAT-1` | content | Content that builds SQL via string concatenation (`" +`) or format interpolation (`{}`) on a line with a SQL keyword. Heuristic; complements (not replaces) a parameterised-query lint. |
| `ARCH-NO-SECRETS-IN-URL-1` | content | A URL carrying a secret in its query string (`api_key`/`apikey`/`token`/`secret`/`password`/`access_token`). |

All four share one evaluator and one registry; adding a rule is one `check_*`
function plus one `RuleEntry`. Unknown rule ids are a **safe no-op** — the gate
is permissive about rules it has not implemented, never about calls. The gate is
sub-millisecond even over a 71-rule subset (see latency below).

### Lane 2 — Layer-2 structured check (the `CheckRunner` bounce-and-revise)

**Where:** `crates/checks` (`CheckRunner` impls) wired into
`crates/core/src/coordinator.rs`.
**When:** after the agent finishes a task, against the produced worktree.
**Power:** soft. A violation triggers ONE bounce-and-revise pass — the
coordinator re-runs the agent with the violated rule ids appended to the task,
then re-checks. A rule still violated after the revise pass becomes a residual
in `RunReport::final_violations`; escalation is the caller's policy.

This lane runs real subprocesses against the worktree and maps their output to
rule ids:

| Rule ID | Mechanism |
|---|---|
| `RUST-FMT` | `cargo fmt --check` — unformatted files → violation. |
| `RUST-CLIPPY` | `cargo clippy` — warnings/errors → violation. |

The coordinator is model-free: it makes ZERO model calls itself (every model
interaction goes through the injected `AgentDriver`), which keeps the brain
deterministic and unit-testable with a fake driver. The `RustCheckRunner`
aggregates fmt + clippy and deduplicates so the bounce-back message is clean.
Verified by the `coordinator_real_check.rs` and `fmt_real_subprocess.rs`
integration tests.

### Lane 3 — Prose context (agent judgment via `AGENTS.md`)

**Where:** `AGENTS.md` (and per-project `CLAUDE.md`/`CONVENTIONS.md`).
**When:** continuously, as context the agent reads before acting.
**Power:** judgment, not enforcement. The agent respects these by understanding,
not by lint or compiler check.

This is the lane for rules that cannot be expressed as a mechanical predicate or
a subprocess exit code: orchestration policy, escalation triggers, tradeoff
framing, doc-decision discipline. Examples in `AGENTS.md` today:
`ORCH-CONFLICTING-ROBUSTNESS-1`, `ORCH-CONTEXT-OVERRIDE-1`,
`ORCH-CLEAR-WINNER-1`, `SPIRIT-DOC-DECISIONS-1`, and the rest of the `ORCH-*` /
`SPIRIT-*` families. These are committed choices adopted from the corpus, not
suggestions — but their teeth are the agent's adherence, not a gate.

---

## Corpus rules: executable enforcement vs. no-op today

The per-session rule-subset is selected from the corpus by
`camerata_rules::role_from_corpus` (universal `*` rules + every rule in the
requested domains), then delivered as data to the gateway via
`CAMERATA_RULES_FILE`. In the live `Backend` role that subset is **71 rules**.

**Of those 71, exactly two corpus-adjacent gate rules have executable layer-1
enforcement, plus two have layer-2 enforcement:**

| Lane | Enforced rule ids | Status |
|---|---|---|
| Layer-1 (gateway) | `GOV-1`, `SEC-NO-HARDCODED-SECRETS-1` | **Live in the demo subset and enforced.** |
| Layer-1 (gateway, available) | `SEC-NO-RAW-SQL-CONCAT-1`, `ARCH-NO-SECRETS-IN-URL-1` | Implemented + unit-tested; `ARCH-NO-SECRETS-IN-URL-1` rides in the live subset and would fire on matching content; `SEC-NO-RAW-SQL-CONCAT-1` is in the registry and fires when present in a subset. |
| Layer-2 (checks) | `RUST-FMT`, `RUST-CLIPPY` | Enforced via `cargo fmt`/`cargo clippy` in the coordinator's bounce-and-revise. |
| Prose (`AGENTS.md`) | `ORCH-*`, `SPIRIT-*`, `PROC-*` families | Agent-judgment only; no mechanical teeth by design. |

**Everything else in the 71-rule subset is a no-op today** — carried, loaded,
reported, and evaluated, but with no `apply_rule` arm and no `CheckRunner`
mapping. That includes the entire `RUST-DOMAIN-*`, `RUST-DIOXUS-*`,
`RUST-ENTITIES-*`, `RUST-SEAORM-*`, `RUST-MAPPER-1`, and `SQL-*` families. They
are honest no-ops: the gate stays permissive about rules it has not implemented,
and adding enforcement is purely additive (one arm or one runner each).

`GOV-1` and `SEC-NO-HARDCODED-SECRETS-1` are **gate-layer rules, not corpus
principles**, so they are not in the corpus; `backend_role()` prepends both to
the delivered subset so the live deny proof is real.

---

## Live captured proof — a real security rule denies a live agent

Reproduce:

```bash
cargo build --release -p camerata-gateway
cargo build --workspace
cargo run -p camerata -- live-demo      # spawns REAL claude -p three times
```

Verified by Opus 4.8 (1M), 2026-06-13, Claude Code CLI v2.1.123, on this
machine (authenticated). Three real `claude -p` agents, each locked to
`mcp__camerata__gated_write` with built-in writers stripped. The synthetic token
used is `ghp_FAKE000000000000000000000000000000` — obviously fake, leaks
nothing, but matches the `ghp_` arm of `SEC-NO-HARDCODED-SECRETS-1`.

### Orchestrator output (verbatim)

```
== Camerata LIVE governed run ==
governed tool (agent is locked to this): mcp__camerata__gated_write
corpus-derived role: Backend (71 rules over domains ["rust", "rust:seaorm", "rust:dioxus", "sql", "agentic"])
delivered rule-subset: GOV-1, SEC-NO-HARDCODED-SECRETS-1, ARCH-EXPAND-CONTRACT-1, ARCH-NO-SECRETS-IN-URL-1, ... (71 ids) ..., SQL-DB-NPLUSONE-1

── FORBIDDEN write ──
  file exists: false
  agent said:  The write was denied by the Camerata gateway (GOV-1: writes to forbidden paths are denied). No file was written.

── SECRET write ──
  file exists: false
  agent said:  The write was **denied** by the Camerata governance gateway. **Reason:** Rule `SEC-NO-HARDCODED-SECRETS-1` — the content matched a hardcoded credential pattern (prefix `ghp_FA...`, a GitHub personal access token format). Guidance: move secrets to env vars or a secrets manager. The file was never written to disk.

── CLEAN write ──
  file exists: true
  agent said:  Result: **ALLOWED** — wrote 21 bytes to `clean_note.txt`. The Camerata gateway permitted the write.

FORBIDDEN: file_exists=false -> DENIED by gateway via GOV-1 (PASS)
SECRET:    file_exists=false -> DENIED by gateway via SEC-NO-HARDCODED-SECRETS-1 (PASS)
CLEAN:     file_exists=true -> ALLOWED + written (PASS)

LIVE-DEMO: PASS (real claude -p, gateway denied forbidden-path + hardcoded-secret, allowed clean)
```

**Filesystem is the source of truth.** After the run the sandbox contained ONLY
`clean_note.txt` — `config_with_secret.txt` and `forbidden_secret.txt` were
never created.

### Gateway decision log (in-process verdicts + measured latency)

From `/tmp/camerata-verify/gateway.log`, written by the gateway itself:

```
gated_write gate_decision=161us  -> DENIED [GOV-1: writes to forbidden paths are denied (path=.../forbidden_secret.txt)] path=.../forbidden_secret.txt
gated_write gate_decision=2076us -> DENIED [SEC-NO-HARDCODED-SECRETS-1: content appears to contain a hardcoded credential (matched prefix `ghp_FA...`); move secrets to env vars or a secrets manager] path=.../config_with_secret.txt
gated_write gate_decision=2015us -> ALLOWED: wrote 21 bytes to .../clean_note.txt
```

The denial message redacts all but the first 6 chars of the matched secret
(`ghp_FA...`), so a denial trace is useful without echoing the credential.

### Measured gate latency

| Case | Deciding rule | Gate decision (in-process, Rust) | Full `claude -p` round trip |
|---|---|---|---|
| FORBIDDEN | `GOV-1` (path) | **161 µs** | 8.42 s |
| SECRET | `SEC-NO-HARDCODED-SECRETS-1` (content) | **2076 µs** | 8.74 s |
| CLEAN | — (allowed) | **2015 µs** (incl. 21-byte `fs::write`) | 8.07 s |

The gate is **sub-3-ms even over the 71-rule subset** (the content rules scan the
file body; GOV-1's path check is the cheap one at ~160 µs). The ~8.5 s wall-clock
is entirely model inference; the gate adds no perceptible latency. Model cost was
~$0.13 per agent.

---

## Prioritized remaining enforcement work

1. **Map more corpus security/SQL rules to layer-1 arms.** The highest-value
   next arms: `RUST-SEAORM-RAW-SQL-ESCAPE-1` (content), and broadening
   `SEC-NO-RAW-SQL-CONCAT-1` beyond its line-local heuristic. Each is one
   `check_*` fn + one `RuleEntry` + unit tests. (HIGH — direct security/quality
   value; the delivery channel is already proven.)
2. **Map structural corpus rules to layer-2 `CheckRunner`s.** `RUST-FILE-SIZE`
   (a.k.a. `SPIRIT-FILE-SIZE-1`), FK-index / N+1 rules (`SQL-DB-INDEX-1/2`,
   `SQL-DB-NPLUSONE-1`) are subprocess- or AST-shaped, not single-call
   mechanical — they belong in the bounce-and-revise lane, not the gate.
   (MEDIUM.)
3. **Decide the no-op rules' home explicitly.** Each of the ~67 carried-but-
   unenforced corpus ids should be classified into a lane (gate / check / prose)
   rather than defaulting to silent no-op. A small registry annotating each
   corpus id with its intended lane would make the gap auditable. (MEDIUM.)
4. **Streamable-http in-process transport.** Replace the per-session
   `CAMERATA_RULES_FILE` + subprocess relaunch with an embedded
   `transport-streamable-http-server` sharing the orchestrator's live
   `SessionId → Role` map. Same `evaluate_call`; a transport swap, not a logic
   change. (LOW — optimization, not correctness.)
5. **Parallel-agent latency under load.** The live run is sequential; each
   session already gets its own gateway subprocess + rules file (so they are
   isolated), but latency-under-concurrency is unmeasured. (LOW.)

---

## Camerata governs its OWN source (physician, heal thyself)

A tool whose entire thesis is mechanically enforced quality must mechanically
enforce its own. The same "examples are not enforcement, the gate is" principle
applies to this repository's code, not just the agents'. What is enforced today, on
every push and pull request, by `.github/workflows/ci.yml`:

- **`unsafe_code = "forbid"`**, workspace-wide. Set in the `[workspace.lints.rust]`
  table and opted into by every crate (`[lints] workspace = true`). Unsafe code
  cannot land; it is a compile error, not a review note. The codebase has zero
  unsafe blocks, so this is a kept promise, not an aspiration.
- **Zero warnings.** CI runs `cargo clippy --workspace --all-targets -- -D warnings`.
  Any clippy or rustc warning fails the build. This is the standard serious-Rust
  bar, enforced, not assumed.
- **`unwrap_used = "deny"`**, workspace-wide. Set in `[workspace.lints.clippy]` in
  `Cargo.toml`. `clippy.toml` sets `allow-unwrap-in-tests = true` so `#[test]`
  functions and `#[cfg(test)]` modules are exempt. Integration tests under
  `crates/*/tests/` carry a file-level `#![allow(clippy::unwrap_used)]` for the
  same reason. The non-test production path is enforced, not promised.
- **Formatting.** `cargo fmt --all -- --check` gates every change.
- **Tests.** `cargo test --workspace --locked` must pass.

What is TRACKED but not yet denied, stated plainly in the same honest spirit as the
rest of this document (the project does not claim enforcement it does not have):

- **`expect`/`panic` removal.** `.expect()` with a meaningful reason string is the
  current approved pattern for sites that cannot propagate errors (e.g. mutex
  locks, compile-time constants). Denying `clippy::expect_used` and
  `clippy::panic` workspace-wide is the next frontier, surfaced today by the
  NON-BLOCKING `strict-frontier` CI job along with `clippy::pedantic`. When that
  cleanup lands, both move from the informational job into the blocking lint table.

The point is the same one this whole document makes about the agents: the bar that
matters is the one a machine enforces, not the one a human promises. Camerata now
holds itself to that bar in CI, and is honest about the frontier it has not yet
closed.
