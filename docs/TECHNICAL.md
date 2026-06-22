# TECHNICAL.md — Camerata Under the Hood

> **Audience:** developers and architects who want to understand HOW Camerata
> works in code, not what it does for end users. For the user-facing feature
> reference, see `docs/USER_GUIDE.md`.
>
> **Accuracy discipline:** this document must be kept in sync with the code as
> things change. If you rename a crate, move a module, add an enforcement arm,
> or change a store's persistence path, update the relevant section here in the
> same commit. A stale technical reference is worse than no reference.

---

## 1. System overview and crate map

Camerata is a single Rust workspace. The shipped app is 15 crates under
`crates/` (plus one maintainer-only tool, `tools/corpus-verifier`, that is NOT a
dependency of any app crate). All load-bearing code is Rust; the only optional
non-Rust piece is a future TypeScript AST sidecar described in
`docs/ARCHITECTURE.md` that does not exist yet.

### Workspace members (`Cargo.toml`)

| Crate | Binary / Lib | One-line purpose |
|---|---|---|
| `camerata-core` | lib | Orchestrator brain: roles, tasks, DAG, coordination. Makes ZERO model calls. Defines the shared traits (`GovernanceGateway`, `AgentDriver`, `CheckRunner`) the rest of the stack depends on. |
| `camerata-intake` | lib | PO-mode intake form schema + LeadEngineer (architect-abstracted second level). |
| `camerata-gateway` | lib + bin (`camerata-gateway`) | Layer-1 real-time governance gate. A Rust MCP server; also usable in-process. |
| `camerata-agent` | lib | Agent runtime: drives `claude -p` subprocesses, parses stream-json. Implements `AgentDriver`. |
| `camerata-rules` | lib | Rule corpus loader, `EnforcementKind` classifier, rule-subset selection. |
| `camerata-persistence` | lib | SQLite state store (`sqlx`) + JSON provenance. |
| `camerata-checks` | lib | Layer-2 post-task gate: `CheckRunner` + `cargo fmt`/`cargo clippy`/`cargo test` subprocess runners. |
| `camerata-fleet` | lib | Reusable governed-fleet build logic, shared by CLI and UI. |
| `camerata-server` | lib + bin | Axum HTTP/WS server the cockpit talks to. Embedded in the UI process. Contains the onboarding scan pipeline, arm/emit, workspace/git controls, the WorkItem/UoW layer, and all HTTP routes. |
| `camerata-cli` | bin (`camerata`) | CLI binary entry point wiring everything together, including `live-demo`. |
| `camerata-ui` | bin (`camerata-ui`) | Dioxus desktop cockpit. Separate process; talks to the embedded `camerata-server` over localhost HTTP. |
| `camerata-worktracker` | lib | `WorkItemProvider` port: canonical story shapes, sync policy, native provider, and adapters for GitHub Issues, GitHub Projects v2, Jira Cloud, Azure DevOps Boards. |
| `camerata-maintenance` | lib | Tier-2 standing post-publish ops agent (dependency upgrades, security patches, secret rotation). |
| `camerata-deploy` | lib | Tier-2 BYO-infra publish: `DeployTarget` seam + local + Azure adapter. |
| `camerata-linter-registry` | lib | Citation validator: canonical linter rule-id lists per tool, plus a corpus-scan report used to ground `mechanical` rules to real linter ids (`Verification::Grounded`). |

> The maintainer-only `tools/corpus-verifier` (a separate workspace member,
> not a `crates/` member) promotes rules `grounded → verified` via a branch + PR.
> It is the only write path to `verified` and is never a dependency of the
> shipped app.

### Process and runtime model

When you run `cargo run -p camerata-ui`:

1. The Dioxus desktop process starts. In `crates/ui/src/main.rs`, `use_hook`
   spawns a dedicated OS thread that starts a `tokio::Runtime` and calls
   `camerata_server::serve("127.0.0.1:8787")`. This is the **embedded BFF**.
2. The BFF binds Axum to `127.0.0.1:8787`. All cockpit data flows over
   localhost HTTP/WebSocket. The cockpit never calls backend crates directly.
3. When a governed run is triggered, the orchestrator inside `camerata-server`
   spawns short-lived `claude -p` subprocesses (one per task/role), each locked
   to the MCP governance gate.
4. The MCP governance gate runs either as an in-process `GovernedGateway` (the
   `GovernanceGateway` trait implementation in `crates/gateway/src/lib.rs`) or
   as a separate stdio MCP server binary (`crates/gateway/src/main.rs`). Both
   call the same pure `evaluate_call` function — no divergence.

### Language boundary

Everything load-bearing is Rust. The cockpit UI (`camerata-ui`) is Dioxus (Rust
targeting desktop via WebView). `camerata-server` is Axum (Rust). The governance
gate is `rmcp`-based Rust. The agent runtime drives `claude -p` (the Claude Code
CLI) as a subprocess. There is no Node.js or TypeScript in the active runtime.
The historical `TECH_DESIGN.md` describes a TypeScript orchestrator that was
replaced; disregard it for current code.

---

## 2. Layer-1 MCP gate

### What the gate is

`crates/gateway` is a Rust MCP server that implements the
`camerata_core::GovernanceGateway` trait. It is the **deny-before-execute**
checkpoint: every tool call an agent attempts is evaluated against the role's
rule-subset BEFORE the side effect happens. A denied write never touches the
filesystem.

### How it is defined

`crates/gateway/src/lib.rs` is the library half. The key types:

- `GovernedGateway` — holds a `HashMap<SessionId, Role>`. Each `Role` carries
  its `rule_subset: Vec<RuleId>` (assigned when an agent is spawned). `bind()`
  registers a session; `evaluate()` looks up the session's role and calls
  `evaluate_call`.
- `evaluate_call(rule_subset, call)` — the single source of truth for layer-1
  governance. It iterates the subset, calling `apply_rule` on each. The **first
  rule that denies wins** (fail-closed, cheapest explanation in the bounce-back
  message). Pure function: same inputs always yield the same `Decision`.
- `apply_rule(rule, call)` — dispatches through `RULE_REGISTRY` by rule-id
  string. Unknown ids are safe no-ops: the gate is permissive about rules it
  has not implemented, never about calls.
- `RULE_REGISTRY: &[RuleEntry]` — static ordered list of all implemented rule
  arms. Each entry is `(id, description, arm: RuleArmFn)` where `RuleArmFn` is
  `fn(path: &str, content: &str) -> Result<(), String>`.

The gate currently enforces six rules (all arms in `RULE_REGISTRY`):

| Rule ID | Keys off | What it denies |
|---|---|---|
| `GOV-1` | path | Writes whose path contains the substring `"forbidden"`. |
| `SEC-NO-PATH-ESCAPE-1` | path | Writes with a `..` traversal segment or a `.git`/`.ssh` directory component (segment-split, not substring). |
| `SEC-NO-SECRET-FILES-1` | path (filename) | Writing a real `.env`, private-key files (`.pem`/`.key`/`.p12`/`.pfx`/`id_rsa`/etc.), or keystores. |
| `SEC-NO-HARDCODED-SECRETS-1` | content | Content with a hardcoded credential literal: GitHub tokens (`ghp_`/`gho_`/`ghu_`/`ghs_`/`github_pat_`), Slack tokens (`xox[baprs]-`), AWS keys (`AKIA`), OpenAI/Stripe `sk-` keys, Google API keys (`AIza`), PEM private-key headers, or a long opaque literal assigned to a secret-named identifier. Uses a `OnceLock<Regex>` compiled once. |
| `SEC-NO-RAW-SQL-CONCAT-1` | content | Content building SQL via string concatenation or format interpolation; requires DML keyword + confirming SQL clause + interpolation marker to fire. |
| `ARCH-NO-SECRETS-IN-URL-1` | content | A URL carrying a secret in its query string (`api_key`/`token`/`secret`/`password`/`access_token`). |

The full registry is derived from `RULE_REGISTRY` by `enforced_gate_rules()`,
so adding an arm automatically propagates it everywhere (live demo, fleet, CI).

### How the gate is wired to agents

`crates/agent/src/lib.rs` builds the `claude -p` argv in `ClaudeCliDriver::build_args`:

- `--allowedTools` is set to `Read Glob Grep LS mcp__camerata__gated_write` only.
- `--disallowedTools` explicitly lists `Bash Write Edit MultiEdit NotebookEdit Task`.
- `--strict-mcp-config` ensures only the Camerata MCP server's tools are available.
- `--mcp-config <path>` points to the JSON file that tells Claude Code to connect
  to the gate (the stdio transport in `crates/gateway/src/main.rs`).

The net effect: an agent's **only mutation path** is `mcp__camerata__gated_write`,
which routes every byte through `evaluate_call` before touching disk. The `Task`
tool (subagent spawning) is explicitly denied to prevent a child agent regaining
`Write`/`Bash`.

Gate evaluation is a pure, in-memory pass over the role's rule subset: a path
check (`GOV-1`) is a substring test; the content rules are pre-compiled regexes
(`OnceLock<Regex>`). Even over a multi-domain subset of dozens of rule ids the
cost is dominated by the few content regexes, not the iteration. The `claude -p`
round-trip is model inference, orders of magnitude larger; the gate adds no
perceptible latency. (The specific microbenchmark figures previously quoted here
were point-in-time and are not reproduced from code — treat the relative
ordering, not absolute numbers, as the durable claim.)

### Fail-closed behavior

An unknown session (`SessionId` not in the `GovernedGateway` map) returns
`Decision::Deny { rule: gov1_rule(), reason: "no role bound..." }`. There is no
silent allow path for an unregistered session.

---

## 3. Layer-2 post-task checks

Layer-2 is the deterministic gate that runs on the agent's OUTPUT after a task
finishes, in the coordinator's bounce-and-revise loop. It is **cross-language,
polyglot, repo-pinned, and fail-closed** — no longer Rust-only-hardcoded.

### The Rust runner (`crates/checks/src/lib.rs`)

`RustCheckRunner` implements `camerata_core::CheckRunner` and composes three
concrete sub-runners:

- `FmtCheckRunner` — shells out to `cargo fmt --check`; maps failure to
  `RuleId("RUST-FMT")`.
- `ClippyCheckRunner` — shells out to `cargo clippy -- -D warnings`; maps
  warnings/errors to `RuleId("RUST-CLIPPY")`.
- `TestCheckRunner` — shells out to `cargo test --no-fail-fast`; maps a failed
  test or compile failure to `RuleId("RUST-TEST")`.

`RustCheckRunner::check` runs them sequentially, cheapest-first (fmt errors make
clippy noisy; a compile failure makes tests redundant) and deduplicates the
resulting `Vec<RuleId>`. The subprocess invocation layer
(`crates/checks/src/subprocess.rs`) and the output-to-`RuleId` mapping layer
(`crates/checks/src/parse.rs`) are kept separate so the mapping logic is
unit-testable without spawning real subprocesses.

### The cross-language runners and the worktree selector (`crates/checks/src/multilang.rs`)

The Rust runner was historically the ONLY layer-2 gate, hardcoded at every
fleet/po-demo injection site, so a JS, Python, or Go worktree got no meaningful
bounce-and-revise. `multilang.rs` closes that gap. It adds, mirroring the Rust
runner's shape:

- `JsCheckRunner` — lockfile-pinned install (`npm ci` / `pnpm install
  --frozen-lockfile` / `yarn install --frozen-lockfile`, detected via
  `JsPackageManager::detect`; falls back to `npm install` with no lockfile) into
  `node_modules` if absent, then the repo's own `npm run lint` and `npm run test`
  scripts. Both failures map to `LAYER2-JS-CHECKS-1`.
- `PythonCheckRunner` — isolates deps in a `.camerata-venv` (`python3 -m venv`),
  installs from the repo's manifest (`pip install -r requirements.txt`, or
  `pip install -e .` for `pyproject.toml`/`setup.py`), then runs the venv-local
  `ruff check .` and `pytest`. Failures map to `LAYER2-PY-CHECKS-1`. (A
  `Pipfile`-only tree fails closed: pipenv is not auto-invoked.)
- `GoCheckRunner` — `gofmt -l .` (non-empty stdout = unformatted), `go vet
  ./...`, `go test ./...`. Failures map to `LAYER2-GO-CHECKS-1`.

**Repo-pinned toolchain.** Linter/test versions come from the REPO's
lockfile/manifest, never baked into Camerata: `npm run lint` resolves the repo's
`node_modules` binaries, `ruff`/`pytest` are the venv-local ones, Go and Rust are
pinned by `go.mod` / `rust-toolchain`. The effect is that layer-2 runs the SAME
toolchain as the repo's CI (layer-3). See
`docs/decisions/2026-06-21_layer2_repo_pinned_toolchain.md`.

**Polyglot composition.** `runner_for_worktree(worktree)` is the single
injection point the fleet and po-demo use in place of the old hardcoded
`RustCheckRunner::new()`. It calls `detect_languages(worktree)`, which
recursively walks the tree (pruning `node_modules`, `target`, `.git`, vendored
dirs, etc.) and pairs EVERY detected language with the directory whose manifest
declared it. It then builds a `PolyglotCheckRunner` that runs one sub-runner per
`(language, dir)` project — each against ITS subtree, not the worktree root — and
returns the UNION of their violations (deduped). A single-language repo simply
yields one sub-runner; a polyglot monorepo gets one per project.

**Fail-closed, on every axis.** A runner that CANNOT run returns `Err`, never a
false clean:
- toolchain missing on PATH → spawn `Err` propagates;
- no check defined (e.g. a `package.json` with neither a `lint` nor a `test`
  script) → `Err` ("could-not-run" is not a pass);
- dep install / venv creation failure → `Err`.

`PolyglotCheckRunner` runs ALL sub-runners (it never aborts early) and then, if
ANY returned `Err`, the composite itself returns `Err` naming every project that
could not be verified — a half-verified polyglot tree is not a verified one. The
ONE explicit pass-through is `NoopChecks`: when `detect_languages` finds zero
recognised manifests, the selector degrades to a no-op AND logs a loud warning.
That is not the fail-closed path — an unrecognised tree has no toolchain to be
"missing", so there is no check to fail closed on.

The fleet wiring lives at `crates/fleet/src/lib.rs` (`use
camerata_checks::runner_for_worktree;`, called where the coordinator's
`CheckRunner` is constructed).

**Bounce-and-revise loop:** when the coordinator (in `crates/core/`) receives
violations from the `CheckRunner`, it re-runs the agent with the violated rule ids
appended to the task, then re-checks. A rule still violated after the revise pass
becomes a residual in `RunReport::final_violations`; escalation is the caller's
policy. There is currently one bounce pass.

**Gate-probe — the end-to-end go/no-go (`crates/fleet/src/gate_probe.rs`).** `run_gate_probe()` is
the deterministic, hermetic proof that BOTH gate layers are wired — no `claude`, no network, no
tokens, so it runs in CI. It drives a story through the real engine: **Layer 1** has the real
`GovernedGateway` evaluate one planted violation for **every rule in `enforced_gate_rules()`** (the
full security floor) plus a clean control — every violation must `Decision::Deny` before it touches
disk and the control must `Allow` (proves the gate isn't deny-all). **Layer 2** runs the real
`FleetCoordinator` with a `BounceThenCleanDriver` + `DirtyThenCleanChecks` so the stage bounces
exactly once and resolves on the revise pass. `GateProbeResult::go()` is the conjunction (whole floor
denied ∧ control allowed ∧ bounced ∧ revise clean). Surfaced three ways: the CLI `camerata
gate-probe` (exit 1 on NO-GO), the `gate_probe_is_go_end_to_end` CI test, and the in-app **Gate
self-check** panel (`POST /api/gate-probe`) in Governed Development. Where `acceptance` proves a few
rules in isolation and the coordinator unit tests prove layer 2, this runs the WHOLE loop and reports
one verdict; `live-demo` is its non-hermetic twin (a real `claude -p` through the MCP gateway).

There is also a VCS-action gate (`crates/checks/src/vcs_action.rs`) that applies
deterministic process rules (`PROCESS-*`) over commit/PR/branch metadata — the
fourth enforcement point distinct from the content-layer `CheckRunner`. This gates
the metadata of the commit or PR Camerata is about to perform.

---

## 4. Agent runtime

`crates/agent/src/lib.rs` contains `ClaudeCliDriver`, which implements
`camerata_core::AgentDriver`. It drives `claude -p` as a subprocess.

Key behavior:

- `ClaudeCliDriver::new(mcp_config_path)` — stores the path to the MCP config
  JSON. The config tells Claude Code where to connect for the governed write tool.
- `with_worktree(path)` — binds the agent to a git worktree: `current_dir` +
  `--add-dir` in the CLI invocation, scoping the agent to its isolated working
  directory.
- `build_args(role, task)` — pure (does not spawn) function that constructs the
  full `claude` argv: `-p <task>`, `--strict-mcp-config`, `--mcp-config`,
  `--allowedTools`, `--disallowedTools`, `--dangerously-skip-permissions`,
  `--output-format json`.
- `run(role, task)` — spawns the process, captures stdout, and parses the JSON
  output via `serde_json`. Fields extracted: `session_id`, `result`,
  `total_cost_usd`, `permission_denials`. Returns an `AgentOutcome`.

`GenericCliDriver` (`crates/agent/src/generic.rs`) is a more general variant.

`SessionSpawn` (`crates/agent/src/session.rs`) handles the per-session prep:
`prepare_session` writes the MCP config JSON and the rules file to temp paths;
`RULES_FILE_ENV` is the env var name (`CAMERATA_RULES_FILE`) passed to the gate
process.

---

## 5. Rule corpus

`crates/rules/src/lib.rs` is the rule corpus loader and subset selector.

### TOML loading

`load_corpus(corpus_dir)` walks the directory recursively (all `.toml` files,
sorted for deterministic order), parsing each into a `Rule`. The corpus lives at
`crates/rules/principles/` by default (resolved from `CARGO_MANIFEST_DIR` so it
is self-contained); override with the `CAMERATA_CORPUS_PATH` env var.

Each corpus file has fields: `id`, `title`, `enforcement`, `domain`,
`qualifies` (optional summary), `[decision]` (question + why + default), and
`[[option]]` blocks. Unknown fields are silently ignored (no
`deny_unknown_fields`) so future corpus fields don't break the loader.

### EnforcementKind

```rust
pub enum EnforcementKind {
    Prose,         // human-readable rationale only; no generated artifact
    Structured,    // a binary design contract reviewable by a human, but not lint-able
    Mechanical,    // maps to an existing, named linter rule
    Architectural, // deterministically checkable, but needs a bespoke AST/static check
}
```

This drives the emit partitioning in `crates/server/src/arm.rs`: prose rules go
into `AGENTS.md`; structured / mechanical / architectural rules go into
`CONVENTIONS.md`. The four-variant model is the precise reference in §5a — read
that for the conformance-test distinctions.

### RuleSet and selection

`RuleSet` holds rules in load order with two indexes: `by_id: HashMap<String, usize>`
and `by_domain: HashMap<String, Vec<usize>>`. The `select(rule_set, filter)`
function is pure; `Filter` variants include `ByIds`, `ByDomain`, `ByDomains`,
`ByEnforcement`, `Or`, `And`, and `All`.

`select_for_domains(rule_set, domains)` always includes rules with `domain = "*"`
(universal rules) regardless of what domains are requested.

### Role building from corpus

`role_from_corpus(corpus_path, role_name, domains, rule_ids)` loads the corpus
and selects: universal rules (`domain = "*"`) + domain-matched rules + any
explicit id overrides. Sub-domain variants like `"rust:dioxus"`/`"rust:seaorm"`
resolve to the primary component's glob (`domain_to_glob` → `"**/*.rs"`). The
resulting `rule_subset` is sorted alphabetically by id for deterministic
ordering; `allowed_paths` is derived from the domain list. For a live multi-domain
role (e.g. `Backend` over `["rust", "rust:seaorm", "rust:dioxus", "sql",
"agentic"]`) the subset is the union of universal + those domains' rules — dozens
of rule ids. The exact count tracks corpus size and is not asserted in code;
treat the selection RULE, not a fixed number, as the durable claim.

### Deterministic gate rules vs. LLM-semantic rules

The six rules in `RULE_REGISTRY` (`crates/gateway/src/lib.rs`) have executable
layer-1 enforcement. All other corpus rule ids are carried in the subset,
delivered to the agent as context, but have no `apply_rule` arm and no
`CheckRunner` mapping today. They are honest no-ops: the gate is permissive about
rules it has not implemented, and adding enforcement is purely additive.

---

## 5a. Rule type model: two axes

This section is the precise reference for the rule type system. The in-app assistant is grounded on it;
`chat.rs` includes both this file and `USER_GUIDE.md` at compile time via `include_str!`. The user-facing
overview is in `USER_GUIDE.md §13`.

### Axis A — the corpus `enforcement` field (what KIND of conformance check the rule needs)

`EnforcementKind` in `crates/rules/src/lib.rs` has four variants. The `enforcement` field in each corpus
TOML file maps to one of them:

| Value | `EnforcementKind` variant | Conformance test | Plain-English meaning | Render target |
|---|---|---|---|---|
| `prose` | `Prose` | Human judgment / matter of degree | A principle or idiom where reasonable engineers weigh conformance (e.g. "interfaces are small and cohesive," "optimization by default"). | `AGENTS.md` |
| `structured` | `Structured` | Human, binary, but not machine-automatable | A concrete design contract with a clear conform/violate answer (e.g. "repositories return domain types," "API version lives in the URL prefix," "cursor not offset pagination"). Objectively reviewable; not lint-able. | `CONVENTIONS.md` |
| `mechanical` | `Mechanical` | An **existing linter** decides it | Maps to a real, named linter rule in a per-language tool (clippy, ruff/bandit, eslint, ts-eslint, golangci-lint, rubocop, checkstyle/spotbugs, roslyn). Every mechanical rule in the current corpus cites a concrete linter rule; rules with no off-the-shelf match are reclassified to `architectural` or `structured`. | `CONVENTIONS.md` + CI / check-runner |
| `architectural` | `Architectural` | Machine-decidable but needs a **bespoke AST check** | Deterministic in principle, but no off-the-shelf linter expresses it (e.g. `handler_no_direct_db` — "handlers never touch the DB"). Camerata ships or builds a custom checker; falls back to an agent directive while the checker is being written. See `docs/decisions/2026-06-19_ast_architectural_rule_tier.md`. | `CONVENTIONS.md` + custom CI check |

**The unifying insight:** the four modalities are one spectrum of *how objectively conformance can be determined*. That single property decides both where the rule is written and how it is enforced.

| Modality | Conformance test | Written to | Enforced by |
|---|---|---|---|
| prose | human judgment / degree | `AGENTS.md` | PR review (human) |
| structured | human, binary contract | `CONVENTIONS.md` | PR review (human) |
| mechanical | existing linter | `CONVENTIONS.md` + CI | layer-2 check runner + CI |
| architectural | bespoke AST check | `CONVENTIONS.md` + CI | custom check |

**Prose vs. structured — the exact line** (the most common source of confusion; the chatbot must get this right):
- **Prose** = a human has to *judge* it. Conformance is a matter of degree; no single fact settles it. Emitted to `AGENTS.md` as spirit/principles the agent reads.
- **Structured** = a human can *verify* it against a clear binary contract. Any engineer can look at the code and give a definite yes/no — the contract just cannot be expressed as a lint rule. Emitted to `CONVENTIONS.md` as citable conventions.

Both carry identical TOML shape (`[decision]` + `[[option]]` blocks). The difference is the judgment required, not the file format.

**Custom (architect-authored) rules are an exception to Axis A.** A `CustomRule` (`crates/server/src/project.rs`) carries only `name`, `body`, and `domain` — there is no `enforcement` field and no `[decision]`/`[[option]]` shape; it emits as a `### CUSTOM-{name}` directive block. So a custom rule is, in practice, only ever **prose** or **structured** (an advisory directive that is followed and human-reviewed). It can never be `mechanical` or `architectural` by authorship alone, because those modalities require an existing linter mapping or a bespoke AST checker that does not exist for a user-invented rule. Promoting a custom rule into a deterministic tier is a development task (write the linter mapping or the custom checker), not a property the author can set.

**Current corpus counts** (counts drift as rules are added; describe kinds, not hard numbers, when citing):
prose ~84, structured ~190, mechanical ~57, architectural ~9.

**Render-target routing source of truth:** `crates/server/src/arm.rs` (the module-doc routing note around lines 8–9 and the partition in `arm_files_for_repo`, `enforcement == "prose"` vs `!= "prose"`, around lines 136–160) — `prose` → `AGENTS.md`; `structured | mechanical | architectural` → `CONVENTIONS.md`. This is also confirmed in `crates/server/src/onboard.rs` at the `ProposedRule.enforcement` comment (around lines 102–103): "prose -> AGENTS.md, the rest -> CONVENTIONS.md, matching camerata-ai's emit partitioning."

### Axis B — where/when rules are actually enforced (the enforcement points)

Axis B is a deployment fact, not a corpus field. The same rule can be enforced at multiple points.

1. **MCP gate (layer-1) — pre-execution, deny-before-write.** A hardwired set of rule-ids in
   `crates/gateway/src/lib.rs` (`RULE_REGISTRY`). Membership criterion: decidable from one file's
   path or content with a regex, no build needed. The six current gate rules are:
   `GOV-1`, `SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`,
   `ARCH-NO-SECRETS-IN-URL-1`, `SEC-NO-PATH-ESCAPE-1`, `SEC-NO-SECRET-FILES-1`.

   **The gate is a deployment point, not a rule type.** Proof: 5 of these 6 rule-ids are NOT in the
   corpus at all — they are gate-internal primitives. Only `ARCH-NO-SECRETS-IN-URL-1` is also a
   corpus rule (tagged `structured`). The gate enforces a rule by its id string; a corpus rule with
   the same id gets layer-1 enforcement automatically. Adding a gate arm is one `check_*` fn +
   one `RuleEntry` in `RULE_REGISTRY`; it propagates everywhere via `enforced_gate_rules()`.

   The verified runtime default subset is `["GOV-1"]` unless configured. `evaluate_call` is
   fail-closed: unknown session → deny; unknown rule id → permissive no-op (not a false deny).

2. **Layer-2 post-task check runner — deterministic, on the agent's output.** `CheckRunner` trait
   (`crates/core/src/lib.rs`), run by the `Coordinator` after the agent finishes. If a rule is
   violated, ONE bounce-and-revise pass runs. The runner is now polyglot:
   `crates/checks/src/multilang.rs` implements `JsCheckRunner`, `PythonCheckRunner`,
   `GoCheckRunner`, `RubyCheckRunner`, `JavaCheckRunner`, `CSharpCheckRunner`, and the existing
   `RustCheckRunner` (`crates/checks/src/lib.rs`). The
   `runner_for_worktree(worktree)` function detects every **supported** language present in the
   worktree — Rust, JS/TS, Python, Go, Ruby, Java, C# (recursively, via `detect_languages`) —
   constructs a
   `PolyglotCheckRunner` that runs one sub-runner per detected `(language, dir)` project, unions
   their violations, and is **fail-closed**: if any sub-runner cannot run (missing toolchain, no
   lint/test script, install
   failure), the composite returns `Err` — it never reports clean for a half-verified tree. Each
   runner uses the REPO's own pinned toolchain, so layer-2 == the repo's CI toolchain.
   Unknown worktrees degrade to `NoopChecks` with a logged warning; this is the one explicit
   exception, not the fail-closed path (there is no toolchain to be missing for an unrecognised tree).
   **Coverage:** all SEVEN languages the corpus ships rules for now have a layer-2 runner. The new
   three pin and run, respectively: `RubyCheckRunner` (manifest `Gemfile`) → `bundle install` +
   `bundle exec rubocop` + `bundle exec rspec`/`rake test`, pinned by `Gemfile.lock`;
   `JavaCheckRunner` (manifest `pom.xml` for Maven, `build.gradle`/`build.gradle.kts` for Gradle) →
   `./mvnw -q verify` / `./gradlew check`, preferring the repo's wrapper for pinning and falling
   back to global `mvn`/`gradle`; `CSharpCheckRunner` (manifest `*.csproj`/`*.sln`) →
   `dotnet format --verify-no-changes` + `dotnet build` + `dotnet test`, SDK pinned by `global.json`.
   Each maps a failure to a coarse `LAYER2-<LANG>-CHECKS-1` rule and fails closed when its toolchain
   is missing or no check is defined. See
   `docs/decisions/2026-06-22_layer2_ruby_java_csharp_runners.md`.

   Layer-2 is **fast and in-loop** (runs against the agent's draft, before commit). Layer-3 is the
   authoritative backstop. This is intentional redundancy: client-side validation (layer-2) catches
   violations immediately so the agent can self-correct; server-side validation (layer-3) catches
   anything that bypassed the agent, including human commits and other tools. Neither substitutes
   for the other.

3. **Layer-3 CI — the target repo's own pipeline.** Language-agnostic. Onboarding grounds each
   mechanical rule to the repo's real linter and files a GitHub issue to wire it into CI. The CI
   config itself is not generated — Camerata files the story; the dev layer does the wiring work.
   Layer-3 persists even if Camerata is removed from the project.

4. **Agent directive — in-context.** prose + structured rules are injected into the agent's context
   at spawn. The agent follows them. Drift is low with concise directives but not zero; PR review is
   the human backstop.

5. **Human review — the backstop for prose and structured, and the only path to `verified`.**
   See the verification ladder below.

### The verification ladder

Every rule carries a `verification` field:

| Value | Meaning |
|---|---|
| `draft` | AI-generated rule; no supporting citation was found. Advisory only; never auto-recommended during onboarding. |
| `grounded` | The onboarding agents found at least one citation from a trusted source (language docs, style guides, real linter rule ids). Linter-id existence is validated by `crates/linter-registry/`. |
| `verified` | A human has checked the cited findings and approved the rule. **No agent may set this, by design.** This is a deliberate trust boundary: the machine can ground a claim, only a human certifies it. |
| `needs_recheck` | A rule that WAS `verified`, but the cited source or linter it was verified against has since drifted (e.g. a version bump moved the rule id). Still backed by a citation and usable, but no longer carries the strongest assertion until a human re-checks it. |

(`Verification` in `crates/rules/src/lib.rs` has these four variants. `grounded`,
`verified`, and `needs_recheck` are all "backed by a citation"; only `draft` is
not.)

Current state (honest): the mechanical rules in the corpus are grounded (each maps to a real linter rule). Zero are `verified` yet — that is a human-only step the maintainer has not yet completed. Grounded is the shippable baseline. Verified is the gold standard for citing a rule in a compliance context. The app surfaces these as read-only badges; the maintainer-side verifier tool is the only write path to `verified`.

### Chatbot grounding confirmation

`crates/ui/src/chat.rs` includes both documentation files at compile time:

```rust
const TECHNICAL_DOC: &str = include_str!("../../../docs/TECHNICAL.md");
const USER_GUIDE: &str    = include_str!("../../../docs/USER_GUIDE.md");
```

Both are baked into the unified system prompt assembled for every chat turn (layer-1 of the prompt, static and cache-eligible). A doc change recompiles `camerata-ui` but does not require any other wiring change. The chatbot's canonical probe — "what is the difference between a prose and a structured rule?" — is answerable from this section: prose requires human judgment (matter of degree); structured requires human verification against a binary contract. Both live outside CI; the difference is judgment, not format.

---

## 6. Onboarding scan pipeline

The brownfield onboarding pipeline lives in two files in `crates/server/src/`:

**Already-onboarded guard.** `detect_repo` checks the open project's persisted state and returns
`RepoDetect::Found { repo, path, onboarded_in }` when the target is a repo the project has already
onboarded; the handler refuses to start a fresh onboarding session for it (onboarding is one-time per
repo — rule changes go through the Rules view, not a re-scan).

### File source — local-first (never GitHub)

Onboarding reads code from the repo's **local working tree on disk**, never from GitHub.
`read_local_repo_files(dir)` (`onboard.rs`) walks the directory, pruning noise dirs during
descent (`.git`, `node_modules`, `target`, build/cache/generated dirs, lockfiles, minified/codegen
files) and applying the code-extension filter, per-file size cap, and `HARD_CAP_FILES` safety net —
returning the same `ExtractedRepo { files, truncated, excluded_noise }` shape the whole pipeline
consumes. (The old GitHub-tarball reader was removed.) The HTTP handlers resolve each repo's local
dir with `resolve_local_sources(state, repos)` → `workspace::resolve_repo_dir` (per-repo path
override, else `<workspace_root>/<owner>/<repo>`); a repo with no local clone surfaces a
"browse to the repo's folder" note instead of being scanned. `scan_repos` and `audit_repos` take the
resolved `(spec, dir)` sources and need **no GitHub token** — the token is only used later for
arm-push and PR.

### `onboard.rs` — deterministic scan

`audit_files(files, rules)` is the deterministic floor. It runs the gate's own
rule arms (via `camerata_gateway::lookup_arm`) over every file in the repo, so
"what the gate would deny on a new write" and "what's already wrong in your repo"
are the same check. The content rules are pure functions over file content;
path-based write-time rules (`GOV-1`, `SEC-NO-PATH-ESCAPE-1`) are not applicable
to existing content and are excluded. The audit rules are:
`SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`,
`ARCH-NO-SECRETS-IN-URL-1`.

Line numbers are resolved via `content_match_lines(rule_id, content)` which
returns 1-based line numbers of all regex matches (scanning the whole file at
once so multi-line constructs are caught).

`Finding` and `ProposedRule` are the output shapes. Both the deterministic and AI
audit tiers emit the same shapes so the cockpit table renders them uniformly.

### `ai_audit.rs` — LLM architectural audit

`audit_repo(llm, repo, files, selected, model, calibration_model, mode, ...)` is
the AI audit pass. It finds genuine architectural/security violations that are not
line-level lint — layering violations, N+1 patterns, missing auth on write paths,
god objects, etc.

> **Stable vs. drifting findings.** The deterministic floor (`onboard.rs`,
> `audit_files`) is repeatable: same code → same finding, same rule-id, same line —
> and those ids are canonical (they're the gate's own arms). The AI audit, by
> contrast, **invents the rule-id per finding** (e.g. `AI-HANDLER-DIRECT-DB-ACCESS`)
> and re-runs the model each scan, so the rule-ids, severities, and exact finding set
> **drift run-to-run**. Treat AI findings as advisory ("the model flagged this
> pattern") and rely on exact rule-ids only for the deterministic floor. UI/prose
> should label AI findings as advisory and not present their ids as fixed rules.

**Chunking:** files are packed into contiguous chunks at most `CHUNK_DIGEST_CHARS`
(350,000 raw chars) each via `chunk_files`. Each chunk is audited in its own model
call so the whole repo is covered regardless of size. A file larger than the
budget becomes its own chunk.

**Digest format:** each chunk is rendered via `build_digest`, which emits
`// ===== FILE: <path> =====` headers with every line numbered as `NNNN| line`
so the model can cite accurate line numbers.

**Repo map:** every chunk also receives the whole-repo symbol map from
`build_repo_map` (every file path + its public symbols, from a cheap line scan).
This gives every chunk architectural context (which dirs are repositories vs.
services) without needing every file body.

**Scan modes.** The audit picker offers THREE choices, but they are two orthogonal
dimensions — the `ScanMode` enum has only two variants (the batching algorithm), and
"Background job" is a separate EXECUTION dimension (foreground vs detached), not a third
`ScanMode`. This is the common point of confusion.

`ScanMode` (`ai_audit.rs`) — how the LLM calls are batched; `tuning()` returns
`(max_concurrent_calls, rules_per_batch)`:
- `Sequential` — `(1, usize::MAX)`: one call per file-chunk with ALL rules at once,
  chunks one after another. Simplest, gentlest on rate limits — the debug/fallback floor.
- `Parallel` (default) — `(PARALLEL_CONCURRENCY=6, RULE_BATCH_SIZE=15)`: rule-batches ×
  file-chunks run concurrently (capped). Wall-clock is the slowest batch, not the sum.

The picker's third option, **"Background job"**, runs the audit (Parallel batching)
SERVER-SIDE as a detached `JobStore` job instead of inline in the request: the UI gets a
job id and polls `JobState` (status / done / total / live `findings` preview / final
`report`), so the architect can leave and watch findings stream in. Best for huge /
multi-repo scans where a foreground request would be long-lived. Foreground Parallel and
Sequential block until the audit returns. `from_wire("sequential") -> Sequential`, else
`Parallel`; the "job" choice selects detached execution and still uses Parallel batching.

**Resolution round:** passes may defer a judgment by returning `needs_files`. A
single bounded resolution round re-runs those requested files together. The
resolution round's own `needs_files` are ignored (bounded to prevent loops).

**Calibration pass:** after all chunk passes complete, `verify_findings(.., thorough,
files_count)` runs a separate LLM call (selectable model) over the aggregated findings. It
recalibrates severity and flags low-confidence findings. It never drops a finding — the
architect triages. `verify_system_prompt` is hardened toward humility (an explicit rubric +
"prefer downgrading over inventing") so the pass tightens rather than inflates severity.

**Thorough calibration (opt-in consensus).** When the caller passes `thorough = true` (the UI
checkbox), the calibration runs as a **multi-vote consensus** instead of a single pass:
`verify_findings` issues several independent verdict calls and `consensus_verdicts()` reconciles
them, taking the **conservative** outcome on disagreement (covered by
`consensus_is_conservative_on_disagreement`). It also applies a **proportionality** bound scaled by
`files_count` so a tiny repo can't be flooded with criticals. Thorough costs more model calls, so
`estimate_audit_cost(.., thorough)` scales the calibration token estimate by `~3×` when it's on
(the pre-scan estimate the UI shows reflects this).

**Dedup and merge:** findings are deduplicated by `(path, line, rule_id)` then
`merge_by_location` collapses all findings at the same `(path, line)` into one
row. The primary is the adopted corpus rule id (over an invented `AI-` id); the
others become `also_matches`. Line 0 (file-level/uncited) findings are never
merged. Verbatim snippet-based line resolution (`resolve_finding_lines`) corrects
the model's approximate line estimates to actual line numbers from the code.

**Suppression (the baseline ratchet):** a finding is suppressed when it matches an
accepted-debt record; only `Active` findings drive gate enforcement. `suppression.rs`
(`camerata-server`) owns this, and `classify_one(finding, inline_waivers, baseline)`
computes a finding's status:

- **`suppressed-inline`** — a `camerata:allow` comment (WITH a reason) at the site. A
  reason-less waiver does NOT suppress; the waiver itself becomes a violation.
- **`suppressed-baseline`** — a `.camerata/baseline.json` entry matches. The match is
  `entry.rule_id == finding.rule_id && entry.fingerprint == fingerprint(finding)`, where
  `fingerprint(rule_id, snippet)` is an FNV-1a hash of `rule_id | whitespace-normalized
  snippet`. So matching is by **rule + offending code content**, NOT line number:
  - It **survives line drift / reformatting** (content-based, whitespace-insensitive) — a
    finding stays suppressed when surrounding code moves or is reindented.
  - It **ratchets on edit**: changing the offending code changes the fingerprint, so the
    baseline entry no longer matches and the finding **re-surfaces as `Active`**. Touch the
    debt and you own it.
- Otherwise **`Active`** — the gate enforces it.

**Where the baseline comes from:** the onboarding **Apply** step (writes the governance
files to the `camerata/onboard-governance` branch, `arm::ARM_BRANCH`) snapshots EVERY
currently-active finding into `.camerata/baseline.json` as "pre-existing at onboarding"
(`baselines_from_findings`) — accepting the whole pre-existing debt set, not only the ones
triaged "Ignored". Triaging a single finding "Ignore (with reason)" later appends just that
entry to the committed baseline (the per-finding suppress endpoint). The file is
version-controlled and auditable; future scans read it from the default branch
(`fetch_baseline`) and classify against it.

### Mechanical rules are re-tiered OUT of the code scan

`split_scannable_rules` (`lib.rs`, both audit handlers) drops `EnforcementKind::Mechanical`
rules from the AI code-only audit. Mechanical rules are enforced in CI from build/runtime/DB
context (query-plan inspection, migration/index audit, AST lint) — e.g. `SQL-DB-INDEX-2`,
whose `qualifies` defines it as an `EXPLAIN`/`pg_stat_statements` check on a live DB. Judging
those from a static code digest yields only weak, low-confidence findings, so the scan skips
them and they ride `.camerata/ci-checks.json` instead. The excluded ids are surfaced on
`ScanReport.excluded_mechanical_rules` (shown in the scan header). The corpus is the source of
each rule's tier; rules absent from the corpus (custom) default to scannable.

### Onboarding emits stories; the dev layer does the work

Onboarding never launches a governed dev run. Triage **Process** turns dispositions into
durable artifacts: ignores → baseline waivers; **all tech-debt items → GitHub issues**
(`create_issue`/`create_tech_debt_ticket`), with resolve-now issues titled for dev-engine
pickup. The "wire mechanical rules into CI" step likewise files a GitHub issue
(`onboard_ci_rules` → `create_issue`), not a run. Actually *running* a resolve-now or CI story
through the governed pipeline (the ingest) is Pillar 2. (The old `onboard_fix` endpoint and the
"Fix the audited items" panel — which launched runs from onboarding — were removed.)

---

## 7. Persistence

Camerata is local-first. All state lives on the user's machine.

### JSON stores

JSON files in `dirs::data_dir()/camerata/`:

| File | Store type | Contents |
|---|---|---|
| `projects.json` | `ProjectStore` (`crates/server/src/project.rs`) | All projects: id, name, repos list, `ProjectRuleset` (selections/cross-repo/process/custom), onboarded repo set. |
| `settings.json` | `SettingsStore` (`crates/server/src/settings.rs`) | `workspace_root` (the dir under which repos are cloned) + `repo_paths` (machine-local per-repo path overrides; never travels in export). |
| `onboarding-draft.json` | `DraftStore` (`crates/server/src/draft.rs`) | In-flight brownfield onboarding state, a `{ project_id: draft }` map (one draft PER PROJECT, opaque JSON the UI owns) — opening a project with a draft prompts continue/start-over. Survives a restart; lost only if the scan hasn't produced output yet. |
| `uow.json` | `UowStore` (`crates/server/src/uow.rs`) | `HashMap<story_id, UnitOfWork>`. Each UoW holds `branch`, `DevStatus`, the precise `UowStage` lifecycle, `history`, `gate_provenance`, `sign_off`, `evidence`, and a `decisions` read-cache (durable home is the `ArtifactStore`). See §10. |
| `routines.json` | `RoutineStore` (`crates/server/src/routine.rs`) | Scheduled routines: name, schedule, intent, operational prompt, scope, model, enabled/provisioned, last_run/last_fired, project_id. The auto-fire scheduler (`auto_fire.rs`) ticks over these. |
| `escalations.json` | `EscalationStore` (`crates/server/src/escalation.rs`) | Routine escalations: a blocked run's reason/options/suggestions, the human↔lead-engineer conversation, status (open/resolved), and the translated resume directive. |

Each store type follows the same pattern: `Arc<Mutex<T>>` in-memory mirror,
optional `Arc<PathBuf>` for disk persistence, load-or-default on startup,
best-effort write-through on mutation. `Clone` is a shallow handle (shared `Arc`)
so stores live in `AppState`.

### SQLite (`camerata-persistence`)

`crates/persistence` uses `sqlx` with the `sqlite` feature. It provides
structured storage for provenance/audit artifacts (`artifacts.rs`), the run log,
and task/story state. The in-memory and JSON stores handle live session state
(fast, no schema migration needed); SQLite handles longer-lived audit provenance.

### AppState composition

`camerata_server::AppState` (`crates/server/src/lib.rs`) assembles all stores
into the Axum state. `AppState::from_env()` is the real runtime path: it resolves
`dirs::data_dir()` and passes store paths there; it also selects the worktracker
provider from the environment (`CAMERATA_GITHUB_TOKEN` present → GitHub; else
native in-memory). `AppState::seeded()` is the test/demo path.

### Feature flags (`crates/server/src/feature_flags.rs`)

`FeatureFlags` is an **opt-out** model: every field defaults to `true` and is OFF
only when explicitly set to `false`. Sources, highest-priority first: env override
(`CAMERATA_FEATURE_<UPPER_NAME>=false`), then `.camerata/features.toml` (or the
`feature_flags` section of `settings.json`), then the `true` default.

The one shipped flag is `soc2` (`CAMERATA_FEATURE_SOC2`) — the SOC-2 gap-analysis
lens in the deep audit tier (`run_deep_tier`). Although the field's CODE default is
`true`, **Camerata ships with SOC-2 OFF**: the repo's `.camerata/features.toml`
contains `soc2 = false`. The SOC-2 lens code is retained; only its runtime
execution is gated. Exposed read-only over `GET /api/feature-flags`. Nothing in
Camerata treats SOC-2 as on-by-default in the shipped build.

---

## 8. Apply / PR / git layer

### `arm.rs` — governance file emit

`crates/server/src/arm.rs` renders the project's adopted rules into the files an
agent reads: `AGENTS.md` (prose rules) and `CONVENTIONS.md`
(structured/mechanical rules), plus a `.camerata/rules.json` gate config listing
the armed rule ids.

`render_rule(r: &ArmRule)` emits a block in the camerata-ai format:
`### {id} — {title}`, then the directive, then (mechanical only) a
`_Conformance:_ <test>` line. Architect-only fields (options, decision rationale)
are never emitted — the agent sees one unambiguous instruction, not the curation
surface.

Scope partitioning: only `scope = "repo-local"` rules are emitted into repo
files. Cross-repo and process rules live in the project store and are read by the
integration/VCS-action gates directly.

### `workspace.rs` — local checkout and git controls

`crates/server/src/workspace.rs` manages the local working copies under the
workspace root. Each repo clones to `<workspace_root>/<owner>/<repo>`. Git is
driven by shelling out to the system `git` binary (gets the user's credentials
and SSH config for free).

Token safety: `authed_url` (for transient clone/fetch/push) embeds the
`x-access-token` in the URL but is never written to `.git/config`. `clean_url`
(the token-free HTTPS remote) is what persists on disk.

Key functions:
- `repo_dir(root, repo)` — `root.join(repo)` (so `<root>/owner/repo`).
- `checkout_status(root, repo)` — reads branch + dirty state without hitting the
  network.
- `apply_local_and_push` — creates a local branch and pushes it; no PR is opened.
- `open_branch_pr` — creates a GitHub PR for the pushed branch.

Git controls exposed via the API routes (issue #37): `git/branches`, `git/log`,
`git/checkout`, `git/commit`, `git/push`, `git/pull`, `git/cherry-pick`.

---

## 9. Project portability

A project can be exported as a single JSON blob (`GET /api/projects/:id/export`)
and imported on another machine (`POST /api/projects/import`, which upserts).

What travels: the project id, name, repos list, `ProjectRuleset` (all rule
selections + custom rules), and the `onboarded` set.

What does NOT travel: `settings.json` (`workspace_root` and `repo_paths` are
machine-local). After import on a new machine, the architect must set the
workspace root and optionally override per-repo paths if repos live outside the
standard `<workspace_root>/<owner>/<repo>` convention.

Repo health (`GET /api/projects/:id/repo-health`) checks which repos are cloned
and reachable on the current machine, so path issues are surfaced immediately.

---

## 10. Issue Management → WorkItem → Unit of Work

The Governed Development surface is built in three layers, all provider-agnostic
at the core with a thin per-provider adapter on top.

### WorkItem — the normalized requirement (`crates/server/src/workitems.rs`)

A `WorkItem` is the normalized requirement/story shape the UI consumes, mapped
from ANY provider. It is the surface-level name for what the worktracker's
canonical story type IS; the underlying code type is still
`camerata_worktracker::CanonicalStory` and the full rename across the codebase is
deferred (cosmetic) — see the `from_canonical_story` note in `workitems.rs`. The
DTO:

```rust
pub struct WorkItem {
    pub id: String,        // stable, provider-namespaced: "github:OWNER/REPO#123"
    pub provider: String,  // "github" (the shipped adapter)
    pub repo: String,      // "OWNER/REPO"
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,     // "open" | "closed"
    pub url: String,
    pub labels: Vec<String>,
}
```

`workitems.rs` is the pure (no-I/O) mapping + identity layer:
`WorkItem::from_github_issue`, `WorkItem::from_canonical_story`, and the
`work_item_id_to_story_id` / `story_id_for` bridge that strips the `github:`
provider prefix so a work-item id `github:OWNER/REPO#N` maps to the UoW/story key
`OWNER/REPO#N`. Dedup-by-external-ref is thus a pure string identity on that key.

### The endpoints (registered in `crates/server/src/lib.rs`)

The old inline `/api/stories/adopt-issue` flow — where the UI typed an owner/repo
and an issue number — is **superseded** as the Governed Development surface. (The
`/api/stories/adopt-issue` route still exists in `lib.rs` as a token-free,
idempotent spine upsert primitive, but it is no longer the UI's adoption path.)
The new flow is: pull all open issues across the active project's repos, then
project a chosen work item onto a UoW. The handlers (in `lib.rs`, using the
`workitems` DTO/bridge) are:

- `POST /api/workitems/pull` — pull ALL open issues across ALL the active
  project's repos via the GitHub adapter (`github_issues::list_open_issues`),
  normalized to `WorkItem[]`. **Manual** (user-triggered), **no cache** — every
  pull is a full refresh. Degrades gracefully: no token / no active project / no
  repos returns an empty list with a hint message (never an error); a per-repo
  failure is skipped and the union of repos that resolved is returned.
- `POST /api/workitems/refresh` `{ work_item_id }` — re-pull ONE item from its
  source. Needs the token.
- `POST /api/workitems/comment` `{ work_item_id, body }` — comment back onto the
  source issue. Needs the token. (Echo suppression for write-back loops lives in
  the worktracker sync layer; see §12.)
- `GET /api/uows` — list all Units of Work, each resolved with the `WorkItem` it
  references (from the story spine) and its lifecycle `stage`.
- `POST /api/uow/from-workitem` `{ work_item_id }` — create a UoW referencing the
  work item, **deduped by external ref**: if a UoW already exists for that work
  item it is returned with `created=false` (never a duplicate). The work item is
  also ensured on the canonical story spine (idempotent upsert) so `/api/uows`
  resolves it and the governed-dev endpoints have a story to run against.

GitHub Issues is the shipped adapter. Jira, Azure DevOps, and GitHub Projects v2
adapters exist in the worktracker port (§12) but are NOT yet wired as UX
adapters here — they are FUTURE.

### Unit of Work — the dev lifecycle (`crates/server/src/uow.rs`)

`UnitOfWork` is the dev-side projection of a story, keyed by `story_id` (the
external-ref-derived key above). It has grown well beyond a simple status; the
shipped struct carries:

```rust
pub struct UnitOfWork {
    pub story_id: String,
    pub branch: Option<String>,                 // git branch this work lives on
    pub dev_status: DevStatus,                   // New | InProgress | Done (coarse badge)
    pub stage: UowStage,                         // precise lifecycle (see below)
    pub decisions: Vec<DecisionRecord>,          // read cache; durable home is the ArtifactStore
    pub history: Vec<HistoryEntry>,              // every governed run, note, gate/stage action
    pub gate_provenance: Option<GateProvenance>, // frozen gate accounting from the last run
    pub sign_off: Option<SignOff>,               // architect's explicit QA sign-off (issue #21)
    pub evidence: Option<UowEvidenceRecord>,     // SOC-2 evidence from the last run (issue #53)
    pub updated: String,                         // RFC 3339 last-mutation timestamp
}
```

- `DevStatus` (New / InProgress / Done) is the coarse badge, orthogonal to the
  story's own tracker status: a story can be `Planned` (product) while its UoW is
  `InProgress` (dev started). The cockpit renders both.
- `stage: UowStage` is the precise governed-development lifecycle (Pillar 2):
  `Intake → Investigating → DecisionsApproved → Development → AwaitingQa →
  SignedOff`. It is mutated ONLY through the transition methods on `UowStore`
  (`begin_investigation`, `approve_decisions`, `start_development`,
  `finish_development`, and `sign_off`), each running the pure state machine in
  `crates/server/src/lifecycle.rs`. The decision gate (`approve_decisions` /
  `start_development`) blocks the move into development until every decision
  record is approved.
- `decisions` is a READ CACHE: the durable, version-tracked home for decision
  records and investigation notes is the central `ArtifactStore` (SQLite,
  `crates/persistence`), keyed by story id, when one is attached via
  `with_artifacts`. The inline field stays in sync as the back-compat carrier so
  an older `uow.json` still loads and is migrated into the store on first
  store-backed write.
- `sign_off` is recorded only by the deliberate architect action — Camerata never
  signs work off on its own. A critical SOC-2 evidence finding
  (`is_sign_off_blocked`) blocks the `AwaitingQa → SignedOff` transition until an
  explicit waive-with-reason.

`HistoryEntry` has `ts` (RFC 3339), `kind` (e.g. `"run"`, `"note"`,
`"gate_deny"`, `"gate_allow"`, `"stage"`, `"sign_off"`, `"provenance"`,
`"evidence"`), and `text`.

`UowStore` persists to `<data_dir>/camerata/uow.json` via an
`Arc<Mutex<HashMap<String, UnitOfWork>>>` with best-effort write-through; an
optional `ArtifactStore` handle backs decision/investigation history, and an
optional `PostStoryHook` (PROC-STORY-DOCS-1) can emit per-story docs at sign-off.
UoW API routes include `GET /api/uow`, `GET /api/uow/:story_id`,
`POST .../status`, `POST .../branch`, `POST .../history`, plus the lifecycle
transition and sign-off endpoints.

### Config vs. data storage separation

Project **config** (transferable) and project **data** (local) are kept in separate stores:

| Category | Store / file | Transfers in export? |
|---|---|---|
| Project config | `projects.json` — repos, ruleset, onboarded state, `tier_map` | YES — the export is config-only |
| Units of Work | `uow.json` | NO — local to each developer |
| Story spine | `stories.json` (`InMemoryStoryStore`) | NO |
| Onboarding draft | `onboarding-draft.json` | NO |
| Local repo paths | `settings.json` (`repo_paths`) | NO |

UoWs carry in-progress dev-lifecycle state (stage, branch, gate provenance, decision records, run
history, sign-off). Transferring them would cause two developers who import the same project to
inherit each other's half-finished work. Export stays config-only by design.

See `docs/decisions/2026-06-21_project_config_vs_data_separation.md`.

### Investigation run (`POST /api/uow/:story_id/begin-investigation`)

The investigation run transitions the UoW from **Intake → Investigating** and then runs a single
gated investigation agent.

**Request:** `{ "model": "<id>" }` (optional body; `null`/blank/absent defaults to the active
project's `tier_map.strongest`).

**Response:** `{ "run_id": "<id>", "story_id": "<id>" }` so the UI can poll `GET /api/runs/:id`.

**Behavior:**
1. `state.uow.begin_investigation(story_id)` runs the lifecycle state machine. If the UoW is not at
   Intake, the handler returns `409` with the transition error and starts no run.
2. The model is resolved: caller → project `tier_map.strongest` → shipped strongest default.
3. A run entry (`mode = "investigation"`) is created in the `RunStore` and
   `investigation_run::execute_investigation_run(...)` is spawned.

The investigation runner (`crates/server/src/investigation_run.rs`) drives a **single** gated
`claude -p` agent built from the same fleet machinery as the dev run
(`camerata_fleet::governed_role("Investigator")`). It is NOT the multi-stage development fleet. The
agent's allowed tools are `gated_write` only; `Task`, `Write`, `Bash`, etc. are on the disallowed
list. The agent reads the issue/story, surfaces decisions and tradeoffs, and records its output
verbatim as an `InvestigationArtifact` note on the UoW. Decision-record extraction from that note
remains an architect action through the existing `POST /api/uow/:story_id/decisions` endpoint.

Without `CAMERATA_LIVE_BUILD=1` the runner records a clearly-labelled placeholder note; no real
`claude` process is spawned, keeping CI token-free.

### Tiered development run (`POST /api/stories/:id/run` with `tier_map`)

**Request (extended, back-compatible):**
```jsonc
{
  "model": "<string|null>",          // single-model path (existing back-compat)
  "tier_map": {                       // NEW: three-tier orchestrator path
    "strongest": "<model-id>",
    "balanced":  "<model-id>",
    "fast":      "<model-id>"
  }
}
```
Absent body, absent `tier_map`, or `tier_map: null` takes the existing single-`model` path. When
`tier_map` is present it takes precedence over `model`. The no-code-first gate runs before either
path is chosen; a `tier_map` does not bypass it.

**Tiered fleet wiring** (`crates/server/src/live_fleet.rs` →
`camerata_fleet::build_from_plan_with_tier_map`):

`execute_live_run_tiered` builds a two-stage plan: a **Lead implementer** task classified
`TaskKind::Backend` (→ `CapabilityBand::Strongest`) and a **Tester** task classified
`TaskKind::Test` (→ `CapabilityBand::Fast`). `build_from_plan_with_tier_map` resolves each task's
model from the `TierMap` via `tier::model_for_task`, then:

1. Identifies the **lead stage** — the first task that maps to `Strongest`
   (`orchestrator::lead_stage_index`).
2. Prepares an **orchestrator session** for the lead stage only
   (`orchestrator::prepare_orchestrator_session`): the lead's MCP config carries
   `CAMERATA_DELEGATE_ENABLED=1` and the tier→model JSON so the gateway boots in orchestrator mode
   and the `delegate` tool is live.
3. All other stages (including delegate children) receive a standard non-orchestrator MCP config.
   Their `--allowedTools` excludes `delegate`.

### Governed `delegate` MCP tool (`mcp__camerata__delegate`)

**What it is.** The lead (orchestrator) agent has access to one additional tool:
`mcp__camerata__delegate`. It is registered on the gateway ONLY when the gateway boots in
orchestrator mode (i.e. for the lead stage's gateway process). Non-lead gateways refuse `delegate`
calls at the handler level.

**Input:** `{ "subtask": "<instruction>", "tier": "fast" | "balanced" }`.

**What the handler does** (`crates/gateway/src/delegate.rs::run_delegated`):
1. Checks the explicit **depth guard** (`depth < max_depth`; default `max_depth = 1`). Refuses with
   `DelegateError::DepthExceeded` if tripped, without spawning.
2. Resolves `tier → model` from the orchestrator config's tier-model map (case-insensitive).
   Refuses for unknown tiers without spawning.
3. Writes a per-child session (rules file + child MCP config). The child MCP config does NOT carry
   `CAMERATA_DELEGATE_ENABLED`, so the child's gateway is not in orchestrator mode.
4. Builds a `ClaudeCliDriver` with `orchestrator = false` (so `delegate` is absent from its
   `--allowedTools`) pinned to the tier's model and the shared worktree.
5. Spawns one `claude -p` child, captures its output, and returns it to the orchestrator.

**Depth guarantee — two independent layers:**
- **Structural (primary).** A delegate child is spawned with `orchestrator = false` and its gateway
  lacks `CAMERATA_DELEGATE_ENABLED`. The child structurally cannot delegate; depth is inherently 1.
- **Explicit counter (belt-and-suspenders).** `OrchestratorConfig` tracks `depth` / `max_depth`.
  `run_delegated` refuses at `depth >= max_depth` and threads `depth + 1` into the child's env.

**Escalation is parent-driven.** A child either completes its subtask or returns a message starting
with `INCOMPLETE:`. The orchestrator reads the tool result and decides to re-handle the work or
re-delegate to a higher tier. No child-to-parent callback exists.

**Gate preservation.** Every delegate child carries:
- `--allowedTools` = `gated_write` only (same as the orchestrator, minus `delegate`).
- `--disallowedTools` includes `Task`, `Bash`, `Write`, `Edit`, `MultiEdit`, `NotebookEdit`
  (unchanged from every other agent in the fleet).
- Same worktree jail (`CAMERATA_WORKTREE_ROOT`) and same rule subset as the orchestrator.
The raw `Task` tool stays disallowed for every agent, including the orchestrator. The `delegate` tool
is a governed spawn, not a bypass.

See `docs/decisions/2026-06-21_uow_be_increment1.md` and
`docs/decisions/2026-06-21_uow_delegate_tool_increment2.md`.

---

## 11. Cockpit UI

### Process model

`camerata-ui` is the Dioxus desktop binary (`crates/ui/src/main.rs`). It calls
`dioxus::launch(App)`. The `App` component uses `use_hook` to spawn a background
OS thread that runs a separate `tokio::Runtime` and calls
`camerata_server::serve("127.0.0.1:8787")`. This makes the UI binary
self-contained: it brings its own BFF. If the port is already in use (standalone
`camerata-server`), the bind fails harmlessly and the cockpit uses the external
server.

### Views and navigation

The cockpit's top-level state machine is `CockpitScreen` (Projects / InProject).
`CockpitShell` renders `ProjectGate` until a project is opened, then `CockpitApp`.

Inside a project, `CockpitView` (an enum in `crates/ui/src/cockpit.rs`) selects
the active tab. The nav order is: **Onboard repos · Governed Development · Rules ·
Routines · Repository Workspace · Docs.**

| Variant | Nav label | What it shows |
|---|---|---|
| `Onboard` | "Onboard repos" | Brownfield onboarding wizard: scan → audit → propose rules → arm → apply. |
| `Stories` | "Governed Development" (the default landing tab) | The Issue Management + WorkItem table + UoW cards + dev-controls surface. Renders `GovernedDevPage`. |
| `Rules` | "Rules" | Active project ruleset management after onboarding. |
| `Routines` | "Routines" | Scheduled-routine dashboard. |
| `Workspace` | "Repository Workspace" | Local workspace: clone repos, checkout status, ship (push + PR). |
| `Docs` | "Docs" | In-app documentation viewer: `USER_GUIDE.md` and `TECHNICAL.md` rendered as markdown. |

`CockpitNav` (`fn CockpitNav(view: Signal<CockpitView>)`) renders the tab bar;
clicking sets `view.set(...)`.

### The Governed Development page

`GovernedDevPage` (`crates/ui/src/cockpit.rs`) is the work-item / UoW surface
reached via the "Governed Development" tab. Its left nav (`GovDevSel`) is an
"Issue Management" entry plus a card per UoW:

- `IssueManagementPanel` — the GitHub-specific piece (connection summary + the
  manual "pull" action: the adapter seam), then a provider-agnostic
  `WorkItemTable` + `WorkItemDetail` that operate purely on the `WorkItem` DTO.
- Selecting a UoW renders `UowDevControls` — the step-bound governed-dev surface
  keyed by the UoW's story id. The old standalone "Run this work (governed)" button is gone;
  runs are now bound to the UoW lifecycle stage. `UowDevControls` contains:
  - `UowStepRunControls` — the lifecycle strip (`UowLifecycleStrip`: Intake → Investigating →
    Decisions Approved → Development → Awaiting QA → Signed Off), with the run control for the
    active phase rendered inline. At Intake: model select + **▶ Begin investigation** (single-model).
    At Decisions Approved: Strongest/Balanced/Fast tier selects + **▶ Run development (governed)**
    (three-tier, orchestrator-led). At Investigating: **Approve decisions** transition only.
  - The **Add comment to issue** box with @-mention support (replaces the removed "Ask the team"
    clarify panel).
  - **Pull latest work item**, gate self-check, loop-guard control, agent activity, `UowPanel`
    (post-run read-out), live run + provenance + sign-off.
  This is the surface that replaced the retired adopt-issue flow (§10).

### Chorale tables

The brownfield audit findings table and the proposed-rules table use
`chorale-core` and `chorale-dioxus` (the headless table library from
`github.com/zernst3/rust-chorale`). Columns are defined with `ColumnDef`,
rendered with `Table` / `CellRenderer` / `RowCellRenderer`. The cockpit overrides
chorale's CSS variables to match the app's warm terracotta palette.

### Chat + terminal popups

The `ChatBubble` component (`crates/ui/src/chat.rs`) is a floating overlay added
in `crates/ui/src/main.rs` alongside `CockpitShell`. It is always rendered (not
inside the cockpit nav) so it persists across tab switches.

The terminal popup (`crates/ui/src/terminal.rs`) connects to
`GET /api/terminal/ws?cwd=<path>` (issue #38 PTY+ws endpoint). Each WebSocket
connection spawns a PTY-backed shell via `portable_pty`. The client uses
`xterm.js` inside the Dioxus desktop webview; the PTY reader runs on a
`spawn_blocking` thread and forwards bytes through an `mpsc` channel to the
WebSocket sink.

### Style

All CSS is one `pub const GLOBAL_CSS: &str` in `crates/ui/src/style.rs`, injected
as `style { dangerous_inner_html: GLOBAL_CSS }` in `App`. The markdown CSS class
is `.chat-turn-text.md` and styles rendered HTML from `pulldown-cmark` (tables,
code, headings, lists).

---

## 12. Worktracker port (WorkItemProvider + the story spine)

`crates/worktracker/src/lib.rs` defines the ports the rest of the stack depends
on. This is the layer the §10 WorkItem/UoW surface is built on top of.

### Canonical shapes

`CanonicalStory` is the normalized story shape the spine renders — the code-level
type the surface calls a "WorkItem" (the rename is deferred; see §10). Its actual
fields:

```rust
pub struct CanonicalStory {
    pub id: String,                         // Camerata's own canonical spine id
    pub external_ref: Option<ExternalRef>,  // the SOURCE (where it is tracked)
    pub title: String,
    pub description: String,                // long-form markdown, NOT Option
    pub status: FeatureStatus,
    pub created_by: String,
    pub targets: Vec<RepoTarget>,           // BUILD targets (repos the code lands in)
}
```

Note `external_ref` (the source/tracker) is independent of `targets` (the build
repos). `FeatureStatus` is Camerata's canonical lifecycle vocabulary (Intake,
Investigating, AwaitingClarification, Planned, Executing, Gating, AwaitingQa,
SignedOff, Done, Blocked, Rejected). Every provider adapter maps to/from this;
provider-specific status names never leak into the spine.

### Two distinct traits

These are separate ports — do not conflate them:

- `StoryStore` — the async spine store `camerata-server` holds as
  `Arc<dyn StoryStore>` (list/get/upsert canonical stories). The ONLY
  implementation is `InMemoryStoryStore` (used for demos, tests, and as the live
  spine cache; `AppState::from_env` selects it).
- `WorkItemProvider` — the per-item sync port (pull one item, write one item
  back, post a comment). Implemented by `NativeProvider` (in-process,
  greenfield/solo) and the external adapters: `GithubProvider` (GitHub Issues),
  `GithubProjectsSource` (GitHub Projects v2), `JiraProvider` (Jira Cloud),
  `AdoProvider<T>` (Azure DevOps Boards). GitHub Issues is the only adapter wired
  into the shipped Governed Development UX (§10); the others are FUTURE.

### Sync policy

`crates/worktracker/src/sync.rs` contains the loop-avoidance engine:
- `apply_inbound` — Guard 1: per-field source-of-truth enforcement (some fields
  are owned by the tracker; some by Camerata). Only tracker-owned fields are
  applied on inbound sync.
- `ExpectedEchoTable` — Guard 2: echo suppression. When Camerata writes a status
  back to the tracker, it records the expected value; the next inbound event with
  that value is suppressed to prevent the sync loop.

### ClarifyBridge

`crates/worktracker/src/clarify_bridge.rs` posts the lead engineer's clarifying
questions to the PO's board and polls for the PO's answer. Provider-agnostic;
adapters implement it per tracker.

---

## Appendix: key invariants

1. **The coordinator makes zero model calls.** All model interaction goes through
   the injected `AgentDriver`, keeping the brain deterministic and unit-testable
   with a fake driver.
2. **One enforced gate.** `evaluate_call` is the single source of truth for
   layer-1 governance. The MCP stdio transport and the in-process
   `GovernedGateway` both call it; they cannot diverge.
3. **Fail-closed everywhere.** Unknown session → deny. Unknown rule id →
   no-op (permissive about unimplemented rules, not about calls). Unbound path →
   deny.
4. **Additive enforcement.** Adding a gate arm is one `check_*` fn + one
   `RuleEntry` in `RULE_REGISTRY`. It propagates to the live demo, the fleet,
   and `enforced_gate_rules()` automatically.
5. **Token never hits `.git/config`.** The `authed_url` is used only for the
   transient network call; `clean_url` is the persisted remote.
6. **The UI never calls backend crates in-process.** All cockpit data flows
   through the embedded BFF over localhost HTTP. This is the seam that makes the
   same server cloud-hostable.
