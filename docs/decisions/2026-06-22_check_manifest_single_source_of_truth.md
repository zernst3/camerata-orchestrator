# Decision: Check Manifest Single Source of Truth

**Date:** 2026-06-22
**Status:** Implemented (`feat/check-manifest-ssot`)
**Crates touched:** `camerata-checks`, `camerata-server`, `camerata-gateway`

---

## Context

Camerata enforces in three layers:

- **Layer 1** — MCP gate (deny-before-write, in `camerata-gateway`): real-time,
  per-call, synchronous. Blocks disallowed writes before they happen.
- **Layer 2** — post-task `CheckRunner` (in `camerata-checks`): deterministic gate
  in the governed dev loop, run after each agent task. Today: cargo fmt / clippy /
  test for Rust; per-language equivalents via `PolyglotCheckRunner`.
- **Layer 3** — GitHub Actions CI (`.github/workflows/`): the backstop that runs on
  every PR, including non-Camerata changes.

The problem: Layer 2 and Layer 3 could DRIFT. A user's custom deterministic linter
(e.g. an API-layering checker) only ever lands in CI, not in the governed dev loop.
The agent gets no early feedback. Worse, there was no single place to declare what
the custom checks ARE.

---

## Decision

Introduce `.camerata/checks.toml` as the single source of truth for custom
deterministic gate checks. Both Layer 2 and Layer 3 derive from this file, making
drift structurally impossible.

---

## Manifest schema

```toml
# .camerata/checks.toml
[[check]]
id       = "ARCH-API-LAYERING-1"   # rule id (matches corpus where applicable)
name     = "API layering"           # human-readable label
command  = "scripts/check_layering.sh"  # shell command, cwd = repo root
severity = "high"                   # "high" | "medium" | "low"
in_loop  = true                     # true = Layer 2; false = CI-only
```

Fields:

| Field      | Type    | Required | Semantics |
|------------|---------|----------|-----------|
| `id`       | string  | yes      | Stable rule id; used as violation id on nonzero exit |
| `name`     | string  | yes      | Short human label for bounce-back messages |
| `command`  | string  | yes      | Shell command (`sh -c <command>`, cwd = worktree root) |
| `severity` | string  | yes      | `"high"` / `"medium"` / `"low"` (informational; all severities block) |
| `in_loop`  | bool    | yes      | `true` = run at Layer 2 AND Layer 3; `false` = CI-only (Layer 3 only) |

**All fields are required** (no serde defaults). A missing field is a parse error,
not a silent mis-configuration.

A missing manifest is NEVER fatal. An unparseable manifest logs a warning and
yields zero custom checks. The built-in runners (fmt/clippy/test) are always
unaffected.

---

## Parity guarantee: Layer-2 command set SUBSET OF Layer-3 command set

The critical invariant: every command Layer 2 runs also appears in the generated
CI workflow.

**Structural enforcement (not just convention):**

Both the Layer-2 `ManifestCheckRunner` and the Layer-3 `generate_gates_workflow`
function consume the SAME shared functions in `crates/server/src/workflow_gen.rs`:

- `layer2_commands(stack, manifest)` — returns the exact commands Layer 2 runs:
  built-in stack commands + `in_loop = true` manifest commands.
- `all_ci_commands(stack, manifest)` — returns the superset Layer 3 runs:
  built-in stack commands + ALL manifest commands (in_loop + ci-only).

Because `layer2_commands` is a strict subset of `all_ci_commands` BY CONSTRUCTION
(same built-ins, same in_loop checks, plus ci-only extras), the invariant holds
without any runtime assertion. A unit test in `workflow_gen.rs` asserts
`layer2_commands ⊆ all_ci_commands` for all stack variants.

---

## in_loop vs ci-only: when to use each

| `in_loop` | Description | Use when |
|-----------|-------------|----------|
| `true`    | Runs at Layer 2 AND Layer 3 | Check is fast (< 30s), needs no secrets or external services. The agent gets early feedback. |
| `false`   | CI-only (Layer 3 backstop only) | Check needs secrets (API keys, tokens), external services, or has a long runtime that would stall the agent loop. |

**CI is always the superset.** Even `in_loop = false` checks appear in the generated
workflow so CI remains the authoritative backstop for all deterministic checks.

---

## Additive relationship to built-in runners

The manifest is ADDITIVE on top of the built-in language runners. The composition is:

```
CombinedCheckRunner
  ├── language runner (PolyglotCheckRunner or NoopChecks)
  │     ├── RustCheckRunner (fmt + clippy + test)
  │     ├── JsCheckRunner / PythonCheckRunner / ... (per detected language)
  └── ManifestCheckRunner (in_loop checks from .camerata/checks.toml)
```

The manifest runner runs AFTER the language runner. If the language runner finds
violations, the manifest runner still runs — the agent gets the full picture in one
bounce-back pass. The manifest NEVER replaces built-ins.

`runner_for_worktree` in `crates/checks/src/multilang.rs` now returns a
`CombinedCheckRunner` instead of a bare `PolyglotCheckRunner`. The call site in
`dev_implement_run.rs:321` (`checks.check(&role, &dir)`) is unchanged.

---

## Agent-edit hard-guard (SEC-NO-CAMERATA-CONFIG-1)

**The manifest defines what Layer 2 enforces. An agent that can edit
`.camerata/checks.toml` can silently disable its own gates.** This is the
canonical gate-weakening attack.

Prevention: `SEC-NO-CAMERATA-CONFIG-1` is added to the Layer-1 gateway
(`crates/gateway/src/lib.rs` `RULE_REGISTRY`). It denies any `gated_write` tool
call whose target path contains a `.camerata` segment (split on `/` and `\`).

Scope: covers the ENTIRE `.camerata/` directory — not just `checks.toml`, but also
`features.toml` and any future operator config. The same rationale that keeps `.git`
and `.ssh` off-limits keeps `.camerata/` off-limits.

**This rule is NOT included in any role's `rule_subset` by default.** Roles must
explicitly include `SEC-NO-CAMERATA-CONFIG-1` in their rule subset to enforce it.
The Layer-1 gate only fires rules that are in the session's role subset; the new
rule enters the `RULE_REGISTRY` (so it can be assigned) but is not automatically
added to existing roles. Operators assign it alongside their other governance rules.

---

## Trust model: repo-authored shell commands

Manifest commands are repo-authored shell that Layer 2 executes in the worktree.
This is the SAME trust model as running the project's own clippy/test/CI scripts:

1. The repo operator writes `.camerata/checks.toml` (a human commit to the repo).
2. The `SEC-NO-CAMERATA-CONFIG-1` hard-guard ensures the AGENT cannot author or
   modify the manifest. Only operator commits can change what gates run.
3. Camerata executes `sh -c <command>` in the worktree, the same way CI does.

This is not a new trust boundary — it's exactly the same trust you grant when you
run `cargo clippy` or `npm run lint`. The hard-guard simply ensures the trust
decision remains with the operator.

---

## Implementation summary

| File | Change |
|------|--------|
| `crates/checks/src/manifest.rs` | NEW — `ManifestCheck` + `CheckManifest` structs, `load_manifest` loader, `in_loop_checks()` / `all_checks()` shared iterators |
| `crates/checks/src/manifest_runner.rs` | NEW — `ManifestCheckRunner` impl of `CheckRunner` |
| `crates/checks/src/multilang.rs` | ADD — `CombinedCheckRunner` struct; update `runner_for_worktree` to return it |
| `crates/checks/src/lib.rs` | ADD — module declarations + re-exports |
| `crates/checks/Cargo.toml` | ADD — `toml = "0.8"` dependency |
| `crates/server/src/workflow_gen.rs` | NEW — `generate_gates_workflow`, `layer2_commands`, `all_ci_commands`, `RepoStack` |
| `crates/server/src/lib.rs` | ADD — `pub mod workflow_gen`, `generate_ci_workflow` handler, route `/api/projects/active/generate-ci-workflow` |
| `crates/gateway/src/lib.rs` | ADD — `sec_no_camerata_config_1_rule()`, `arm_sec_no_camerata_config_1`, `RULE_REGISTRY` entry |
| `docs/decisions/2026-06-22_check_manifest_single_source_of_truth.md` | THIS FILE |

---

## Test coverage

| Test location | What it proves |
|---------------|----------------|
| `crates/checks/src/manifest.rs::tests` | Valid parse, absent → `None`, malformed → `Err`, empty → valid, `in_loop` filtering |
| `crates/checks/src/manifest_runner.rs::tests` | No manifest → zero violations; exit 0 → clean; exit nonzero → violation under check id; ci-only skipped; multiple violations collected; unspawnable cmd → no panic |
| `crates/server/src/workflow_gen.rs::tests` | Parity (L2 ⊆ L3) for all stacks; ci-only in L3 not L2; YAML contains built-in commands; YAML contains all manifest checks; TODO block for non-Rust stacks |
| `crates/gateway/src/lib.rs::adversarial` | `.camerata/checks.toml` → Deny; `.camerata/features.toml` → Deny; paths outside `.camerata/` → Allow; Windows backslash separator → Deny |

---

## Tool-version pinning

**Added in `feat/manifest-version-pinning`.** Extends the manifest schema with three optional fields on `ManifestCheck` that together pin the exact external tool version a check depends on.

### Problem

The manifest SSOT eliminated rule-definition drift. But a user-wired external linter (e.g. dependency-cruiser 5.x vs 6.x, or Semgrep 1.x vs 1.y) can return DIFFERENT results on the SAME ruleset across versions. This produces "green at Layer 2, red at Layer 3" even with a single, stable rule definition — the SSOT property breaks at the tool-version boundary.

### Schema additions (manifest.rs)

Three optional fields added to `ManifestCheck` with `#[serde(default)]` for back-compat:

| Field     | Type            | Semantics |
|-----------|-----------------|-----------|
| `tool`    | `Option<String>` | Tool/binary name (`"dependency-cruiser"`, `"semgrep"`). Required when `version` is set. |
| `version` | `Option<String>` | EXACT pinned version (`"6.3.0"`). No ranges or carets — determinism requires an exact match. |
| `install` | `Option<String>` | Exact install command (`"npm install -g dependency-cruiser@6.3.0"`). Explicit because install mechanisms span pip/npm/cargo/go and guessing is fragile. |

Back-compat: any existing manifest entry that omits these fields parses unchanged — all three default to `None`. A missing field is NOT a parse error (unlike the required core fields).

Example full entry:

```toml
[[check]]
id       = "DEP-CRUISER-LAYERING-1"
name     = "dependency-cruiser layering"
tool     = "dependency-cruiser"
version  = "6.3.0"
install  = "npm install -g dependency-cruiser@6.3.0"
command  = "depcruise --config .dependency-cruiser.cjs src"
severity = "high"
in_loop  = true
```

### Layer 3: CI installs the exact version (workflow_gen.rs)

For each check with an `install` command, the generated CI workflow emits a dedicated install step IMMEDIATELY BEFORE the check's command step:

```yaml
- name: "install dependency-cruiser (6.3.0)"  # pinned install for DEP-CRUISER-LAYERING-1
  run: npm install -g dependency-cruiser@6.3.0

- name: "dependency-cruiser layering (DEP-CRUISER-LAYERING-1)"  # in_loop | severity: high
  run: depcruise --config .dependency-cruiser.cjs src
```

`all_ci_commands` and `layer2_commands` both interleave install commands (when present) immediately before their check commands in the returned list. This maintains the structural parity invariant: in_loop install+check pairs appear in both `layer2_commands` and `all_ci_commands`; ci-only install+check pairs appear only in `all_ci_commands`. The `layer2_commands ⊆ all_ci_commands` invariant holds by construction; the existing parity test confirms it.

### Layer 2: detects version drift, never installs (manifest_runner.rs)

Before running a check that declares `tool` + `version`, the runner calls `check_tool_version(tool, pinned)`, which runs `<tool> --version` and compares the output against the pinned version using the pure function `version_matches`.

**`version_matches(output, pinned)`** — pure, unit-testable:

- Scans `output` for the `pinned` string with word-boundary enforcement: the character immediately before/after the match must NOT be a digit or dot (the boundary chars of a version token). This ensures `"6.3.0"` does not match inside `"16.3.0"` (left boundary is digit `1`) but does match in `"v6.3.0"` (left boundary is `v`, a letter, not a version char) and `"tool 6.3.0\n"` (right boundary is `\n`).
- Byte-exact comparison — no semver range semantics. Pinning means pinning.

**On mismatch or tool absent:** a VIOLATION is reported under the check's `id` (not a warning). Rationale: a warning would still allow the agent loop to complete "green" on the wrong version, reproducing the exact failure mode being eliminated. The operator resolves it by running the `install` command from the manifest. The check command is NOT run on mismatch — its output would be untrustworthy and could produce a false-green result.

**Layer 2 does NOT install tools.** Installing tools in the agent dev loop is too heavy and side-effectful. Layer 2 verifies; Layer 3 installs. The diagnostic message always includes the `install` command (when set) so the operator knows exactly how to resolve the mismatch.

### New test coverage

| Test location | What it proves |
|---------------|----------------|
| `crates/checks/src/manifest.rs::tests` | Pinned check (all three fields) parses correctly; legacy check (no pinning fields) back-compat; mixed manifest (pinned + legacy) parses |
| `crates/checks/src/manifest_runner.rs::tests` | `version_matches`: exact match, with newline, with `v` prefix, mismatch, prefix false-positive prevention, suffix false-positive prevention, empty pin, absent in output, multiline; `absent_tool_produces_violation_not_crash`; `mismatched_version_produces_violation_and_skips_command`; `matching_version_runs_check_command` |
| `crates/server/src/workflow_gen.rs::tests` | Install step in YAML before check step (in_loop); no install step for unpinned check; L2 includes install+command for pinned in_loop check; ci-only pinned check install in L3 not L2; parity holds for mixed manifests; install step for ci-only pinned check in YAML |
