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

Camerata is a single Rust workspace of 14 crates. All load-bearing code is Rust;
the only optional non-Rust piece is a future TypeScript AST sidecar described in
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
| `camerata-server` | lib + bin | Axum HTTP/WS server the cockpit talks to. Embedded in the UI process. Contains the onboarding scan pipeline, arm/emit, workspace/git controls, UoW, and all HTTP routes. |
| `camerata-cli` | bin (`camerata`) | CLI binary entry point wiring everything together, including `live-demo`. |
| `camerata-ui` | bin (`camerata-ui`) | Dioxus desktop cockpit. Separate process; talks to the embedded `camerata-server` over localhost HTTP. |
| `camerata-worktracker` | lib | `WorkItemProvider` port: canonical story shapes, sync policy, native provider, and adapters for GitHub Issues, GitHub Projects v2, Jira Cloud, Azure DevOps Boards. |
| `camerata-maintenance` | lib | Tier-2 standing post-publish ops agent (dependency upgrades, security patches, secret rotation). |
| `camerata-deploy` | lib | Tier-2 BYO-infra publish: `DeployTarget` seam + local + Azure adapter. |

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

Gate latency is sub-3 ms over a 71-rule subset (`GOV-1` path check: ~161 µs;
content rules: ~2 ms). The ~8.5 s `claude -p` round-trip is model inference; the
gate adds no perceptible latency.

### Fail-closed behavior

An unknown session (`SessionId` not in the `GovernedGateway` map) returns
`Decision::Deny { rule: gov1_rule(), reason: "no role bound..." }`. There is no
silent allow path for an unregistered session.

---

## 3. Layer-2 post-task checks

`crates/checks/src/lib.rs` implements `camerata_core::CheckRunner` for Rust
worktrees. Three concrete runners, composed by `RustCheckRunner`:

- `FmtCheckRunner` — shells out to `cargo fmt --check`; maps failure to
  `RuleId("RUST-FMT")`.
- `ClippyCheckRunner` — shells out to `cargo clippy -- -D warnings`; maps
  warnings/errors to `RuleId("RUST-CLIPPY")`.
- `TestCheckRunner` — shells out to `cargo test --no-fail-fast`; maps a failed
  test or compile failure to `RuleId("RUST-TEST")`.

`RustCheckRunner::check` runs them sequentially, cheapest-first (fmt errors make
clippy noisy; a compile failure makes tests redundant). It deduplicates the
resulting `Vec<RuleId>` so the bounce-back message is clean.

The subprocess invocation layer (`crates/checks/src/subprocess.rs`) and the
output-to-`RuleId` mapping layer (`crates/checks/src/parse.rs`) are kept separate
so the mapping logic is unit-testable without spawning real subprocesses.

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
    Prose,       // human-readable rationale only; no generated artifact
    Structured,  // emits a structured section (e.g. a CONVENTIONS.md entry)
    Mechanical,  // emits a runnable check (linter, CI gate, etc.)
}
```

This drives the emit partitioning in `crates/server/src/arm.rs`: prose rules go
into `AGENTS.md`; structured/mechanical rules go into `CONVENTIONS.md`.

### RuleSet and selection

`RuleSet` holds rules in load order with two indexes: `by_id: HashMap<String, usize>`
and `by_domain: HashMap<String, Vec<usize>>`. The `select(rule_set, filter)`
function is pure; `Filter` variants include `ByIds`, `ByDomain`, `ByDomains`,
`ByEnforcement`, `Or`, `And`, and `All`.

`select_for_domains(rule_set, domains)` always includes rules with `domain = "*"`
(universal rules) regardless of what domains are requested.

### Role building from corpus

`role_from_corpus(corpus_path, role_name, domains, rule_ids)` loads the corpus
and selects: universal rules + domain-matched rules + any explicit id overrides.
The resulting `rule_subset` is sorted alphabetically by id for deterministic
ordering. `allowed_paths` is derived from the domain list (e.g. `"rust"` →
`"**/*.rs"`). For the live `Backend` role over `["rust", "rust:seaorm",
"rust:dioxus", "sql", "agentic"]`, the subset currently contains **71 rules**.

### Deterministic gate rules vs. LLM-semantic rules

The six rules in `RULE_REGISTRY` (`crates/gateway/src/lib.rs`) have executable
layer-1 enforcement. All other corpus rule ids are carried in the subset,
delivered to the agent as context, but have no `apply_rule` arm and no
`CheckRunner` mapping today. They are honest no-ops: the gate is permissive about
rules it has not implemented, and adding enforcement is purely additive.

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
| `uow.json` | `UowStore` (`crates/server/src/uow.rs`) | `HashMap<story_id, UnitOfWork>`. Each UoW holds `branch`, `DevStatus` (New/InProgress/Done), and `history: Vec<HistoryEntry>`. |
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

## 10. Unit of Work

`crates/server/src/uow.rs` implements the UoW store (issue #39).

`UnitOfWork` holds per-story dev context:

```rust
pub struct UnitOfWork {
    pub story_id: String,
    pub branch: Option<String>,     // git branch this work lives on
    pub dev_status: DevStatus,      // New | InProgress | Done
    pub history: Vec<HistoryEntry>, // every governed run, note, gate action
    pub updated: String,            // RFC 3339 last-mutation timestamp
}
```

`DevStatus` is orthogonal to the worktracker story's own status: a story can be
`Planned` (product) while its UoW is `InProgress` (dev started). The cockpit
renders both.

`HistoryEntry` has `ts: String` (RFC 3339), `kind: String` (e.g. `"run"`,
`"note"`, `"gate_deny"`, `"gate_allow"`), and `text: String`.

The `UowStore` persists to `<data_dir>/camerata/uow.json` via an
`Arc<Mutex<HashMap<String, UnitOfWork>>>` with best-effort write-through. API
routes: `GET /api/uow`, `GET /api/uow/:story_id`, `POST .../status`,
`POST .../branch`, `POST .../history`.

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
the active tab:

| Variant | Nav label | What it shows |
|---|---|---|
| `Stories` | (default) | Story spine (left rail) + center stage (swaps by story status) + inspector (enforced rules, right). |
| `Onboard` | "Onboard repos" | Brownfield onboarding wizard: scan → audit → propose rules → arm → apply. |
| `Rules` | (Rules) | Active project ruleset management after onboarding. |
| `Routines` | (Routines) | Scheduled-routine dashboard. |
| `Workspace` | (Workspace) | Local workspace: clone repos, checkout status, ship (push + PR). |

`CockpitNav` (`fn CockpitNav(view: Signal<CockpitView>)`) renders the tab bar;
clicking sets `view.set(...)`.

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

## 12. Worktracker and stories

`crates/worktracker/src/lib.rs` defines the provider port the rest of the stack
depends on.

### Canonical shapes

`CanonicalStory` is the normalized story shape the spine renders:

```rust
pub struct CanonicalStory {
    pub id: String,
    pub external_ref: Option<ExternalRef>,
    pub title: String,
    pub status: FeatureStatus,
    pub description: Option<String>,
    pub repo_targets: Vec<RepoTarget>,
    // ... etc.
}
```

`FeatureStatus` is Camerata's canonical lifecycle vocabulary (Intake,
Investigating, AwaitingClarification, Planned, Executing, Gating, AwaitingQa,
SignedOff, Done, Blocked, Rejected). Every provider adapter maps to/from this;
provider-specific status names never leak into the spine.

### StoryStore trait

`StoryStore` is the async trait `camerata-server` depends on. The implementations
are:
- `InMemoryStoryStore` — the in-process store used for demos, tests, and when no
  external tracker is configured.
- Provider adapters: `GithubProvider` (GitHub Issues), `GithubProjectsSource`
  (GitHub Projects v2), `JiraProvider`, `AdoProvider` (Azure DevOps Boards).

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
