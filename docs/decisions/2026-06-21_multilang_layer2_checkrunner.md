# Per-language layer-2 CheckRunners + a worktree language selector

Date: 2026-06-21
Status: Accepted (implemented).
Deciders: Zach (architect), Claude (orchestrated).

Companion docs: [`ENFORCEMENT.md`](../ENFORCEMENT.md) (the enforcement tiers/locations),
[`2026-06-19_ast_architectural_rule_tier.md`](2026-06-19_ast_architectural_rule_tier.md)
(the AST tier), [`2026-06-15_process_rules_and_vcs_action_gate.md`](2026-06-15_process_rules_and_vcs_action_gate.md)
(the VCS-action gate).

## Context: the cross-language layer-2 gap

The layer-2 (post-task) enforcement seam is `camerata_core::CheckRunner`:

```rust
async fn check(&self, role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>>;
```

The coordinator's contract (`crates/core/src/coordinator.rs`,
`crates/core/src/fleet.rs`) is: after an agent finishes, run the `CheckRunner`
against the worktree; if it reports ANY violated `RuleId`, bounce the work back
to the agent ONCE with the violated ids appended, then re-check.

The ONLY concrete runner that did real work was `camerata_checks::RustCheckRunner`
(`cargo fmt --check` + `cargo clippy -D warnings` + `cargo test`). And it was
**hardcoded** at every injection site:

- `crates/fleet/src/lib.rs` ~407 and ~549 (`let checks = RustCheckRunner::new();`)
- `crates/cli/src/po_demo.rs` (the layer-2 wiring assertion + the summary line)

So a JavaScript, Python, or Go worktree got NO meaningful layer-2 bounce: the
fleet either ran `cargo` against a non-Cargo tree (spurious) or the demos fell
back to `NoopChecks` (a silent pass). The gate that exists to catch
"generated code that does not lint/test" was Rust-only.

## Decision

Add per-language `CheckRunner` implementations mirroring `RustCheckRunner`'s
shape, plus a selector that detects the worktree language from its manifest
files and injects the matching runner. Wire the selector in at all three
hardcoded sites. No change to the `CheckRunner` trait or the coordinator
contract — this is purely new implementations behind the existing seam.

### The trait seam (unchanged)

`CheckRunner` is the seam. Every runner is one impl of it. The coordinator only
ever sees `&dyn CheckRunner`, so adding languages is additive: the brain stays
deterministic and model-free.

### The per-language runners (`crates/checks/src/multilang.rs`)

Each shells out to that language's standard *format / lint / test* tools in the
worktree and maps a tool failure to a violated `RuleId` so the bounce fires.
They reuse a shared `subprocess::run_command(worktree, program, args)` helper
(added to `crates/checks/src/subprocess.rs`) that mirrors the Rust runner's
"non-zero exit is a signal, spawn failure is an error" split.

| Language | Manifest | Tools run | Violation -> RuleId |
|---|---|---|---|
| Rust | `Cargo.toml` | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` | `RUST-FMT` / `RUST-CLIPPY` / `RUST-TEST` (existing `RustCheckRunner`) |
| JS/TS | `package.json` | `npm run lint`, `npm run test` (whatever the project declares) | `LAYER2-JS-CHECKS-1` |
| Python | `pyproject.toml` / `requirements.txt` / `Pipfile` | `ruff check .`, `pytest -q` | `LAYER2-PY-CHECKS-1` |
| Go | `go.mod` | `gofmt -l .`, `go vet ./...`, `go test ./...` | `LAYER2-GO-CHECKS-1` |

The JS runner honours the project's OWN scripts (`npm run lint` / `npm run test`)
rather than guessing a toolchain, so eslint/tsc/vitest/jest all work without
per-project config in Camerata.

`gofmt -l` is special-cased: it exits 0 even when files are unformatted, listing
them on stdout. The Go runner treats non-empty output as the violation signal,
not the exit code.

### Coarse vs. fine rule mapping (for now)

Rule mapping is deliberately COARSE: one `LAYER2-<LANG>-CHECKS-1` per non-Rust
language. The point of layer-2 is that **a real check runs and a failure
bounces** — fine-grained per-tool ids (e.g. a distinct id for "eslint failed"
vs. "tests failed") can be layered in later without touching the coordinator
contract or the selector. Rust keeps its existing fine-grained ids because they
were already there.

### The selector (`runner_for_worktree`)

`detect_language(worktree) -> WorktreeLanguage` checks manifest files by
precedence: `Cargo.toml` -> Rust, `package.json` -> JS, `go.mod` -> Go,
`pyproject.toml`|`requirements.txt`|`Pipfile` -> Python, else `Unknown`. Rust is
checked first: a polyglot repo with a `Cargo.toml` is, for the fleet's purposes,
a Rust build.

`runner_for_worktree(worktree) -> Box<dyn CheckRunner>` returns the matching
runner. The injection sites do `FleetCoordinator::new(&*checks, &worktree)` —
the `Box<dyn CheckRunner>` coerces to the `&dyn CheckRunner` the coordinator
already takes, so no signature changed.

## The honesty stance: fail closed on "could not run"

This is the load-bearing decision. The runner's failure mode must NEVER be a
false "clean". It mirrors `RustCheckRunner` (a missing `cargo` makes
`run_fmt_check` return `Err`, which the coordinator surfaces as a `Check` error,
not a pass) and the layer-1 gate's fail-closed posture.

Two distinct "could not verify" cases, both returning `Err` from `check`:

1. **Toolchain missing** — the tool binary is not on PATH. The spawn `Err` from
   `run_command` propagates. The work is "not verified", never "clean".
2. **No check defined** — e.g. a `package.json` with neither a `lint` nor a
   `test` script. A configured-but-absent check is a "could-not-run", so the JS
   runner pre-checks the manifest and `bail!`s with a precise message rather than
   silently passing.

The coordinator treats a `Check` error as a hard failure of the run, not a green
light. The ONE place the gate degrades to a pass is `WorktreeLanguage::Unknown`:
a tree with no recognised manifest has no toolchain to be "missing", so there is
nothing to fail closed on. That path uses a `NoopChecks` runner AND logs a loud
`eprintln!` that no layer-2 runner matched and bounce-and-revise is inactive for
that tree — the degradation is visible, not silent.

(`multilang::NoopChecks` is the selector's explicit "no match" sink; it is
distinct from `camerata_fleet::NoopChecks`, which exists for the demos'
final-cargo-gate flow.)

## Consequences

- A JS/TS, Python, or Go worktree now gets a real layer-2 bounce-and-revise.
- A worktree whose toolchain is missing fails closed (an `Err`), so the fleet
  never reports a phantom-clean build.
- Adding a language is additive: one `CheckRunner` impl + one `detect_language`
  arm + one selector arm. No coordinator or trait change.
- Coarse rule ids today; fine-grained mapping is a non-breaking follow-up.

## Wired sites

- `crates/fleet/src/lib.rs` — both fleet runners: `runner_for_worktree(&worktree)`.
- `crates/cli/src/po_demo.rs` — the layer-2 wiring test now asserts the selector
  path; the summary line reflects the language-matched runner.

## Tests

- `runner_for_worktree`/`detect_language` pick the right language per manifest
  fixture (temp dirs with `Cargo.toml` / `package.json` / `go.mod` /
  `pyproject.toml` / `requirements.txt` / `Pipfile` / none), incl. Rust
  precedence in a polyglot tree.
- The pure `map_command_to_rule` returns a violation on a failing command and
  none on a passing one.
- Honesty: a JS `package.json` with no `lint`/`test` script fails closed (`Err`,
  not a false clean); a missing manifest fails closed.
- The Unknown selector returns a `NoopChecks` that reports clean.
- A real-tool Go test self-skips when `gofmt` is absent, else asserts an
  unformatted file bounces (`LAYER2-GO-CHECKS-1`).
