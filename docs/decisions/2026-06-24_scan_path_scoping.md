# Scan path scoping: filter out-of-root preview findings

**Date:** 2026-06-24
**Status:** Implemented (commit 7558d7b)
**Issue:** #78

## Problem

The scan preview pass (clippy, ruff, eslint, semgrep) was surfacing findings
for files that are NOT part of the user's source tree:

- Rust stdlib: `/rustc/<hash>/library/core/src/macros/mod.rs:881`
- Cargo build output: `<repo>/target/debug/build/<pkg>/out/v2_generated.rs`
- Generated protocol code under `target/` and `OUT_DIR`

These appear because:
- `cargo clippy` emits absolute paths for diagnostics that originate in
  expanded macros or type-checking of dependencies, including stdlib source.
- Code-generation build scripts write Rust under `target/…/out/`, and
  clippy then reports lints on that output.

Real example from a `rivet` scan: ~40% of clippy preview findings were on
stdlib or generated files, not user code.

## Decision

Add a single pure predicate `is_in_repo_scope(repo_root: &Path, finding_path: &str) -> bool`
applied uniformly at the `run_one_tool` level — after each parser returns
its `Vec<Finding>` — so ALL four tools (clippy, ruff, eslint, semgrep) are
filtered by the same rules in one place.

### Exclusion rules (applied in order; first match wins)

1. **Synthetic placeholders** (`"(repo)"`, `"(scan preview)"`, any path
   starting with `(`) — always KEPT. Our parsers inject these when no real
   path was present; they must never be dropped.

2. **Relative paths** — resolved against `repo_root`. A linter run with
   `current_dir = repo_root` emits repo-relative paths, so any relative
   path is in-scope by construction (then subject to rules 4-6 below).

3. **Absolute outside `repo_root`** — DROPPED. Covers `/rustc/<hash>/…`,
   `~/.cargo/registry/…`, `~/.rustup/…`, `/usr/…`, and any other
   system path or foreign repo.

4. **`target/` build output** — DROPPED. First component is `"target"`,
   which is Cargo's default build directory.

5. **OUT_DIR paths** — DROPPED if the path contains an `/out/` segment
   (Cargo OUT_DIR convention: build scripts write to `target/.../out/`).

6. **Generated file names** — DROPPED if the filename ends with
   `_generated.rs` (proto/codegen Rust convention) or the file stem ends
   with `.generated` (e.g. `schema.generated.ts`).

7. **Bundled dist** — DROPPED if path contains `/dist/` or `/dist-server/`
   components (Next.js / Vite output, Express SSR bundles).

8. **Everything else** — KEPT (within `repo_root`, not excluded above).

### Why filter at `run_one_tool`, not in each parser

The parsers (`parse_sarif`, `parse_ruff_json`, `parse_clippy_json`) are
pure, fixture-tested functions that need no knowledge of where the tool ran.
`run_one_tool` already has `dir: &Path` (the repo root). Filtering here in a
single `.map()` after the match ensures new tool arms automatically inherit
the filter without needing per-parser changes.

### Why not filter security FLOOR findings

The deterministic FLOOR findings from `lib.rs::finding_security_category`
are repo-relative by construction (they come from Camerata's own AST walk
over the checked-out tree). The helper is a no-op for in-root relative paths
(rule 2 resolves them to within `repo_root`), so they pass through safely.

## Implementation

- **File:** `crates/server/src/scan_tools.rs`
- **Public function added:** `pub fn is_in_repo_scope(repo_root: &Path, finding_path: &str) -> bool`
- **Application point:** end of `run_one_tool` (private async fn) via
  `.map(|mut findings| { findings.retain(|f| is_in_repo_scope(dir, &f.path)); findings })`
- **Tests added:** 13 unit tests in `scan_tools::tests` covering every
  exclusion class and both keep paths (absolute in-root, relative).

## Test coverage

| Test | Rule | Expected |
|------|------|----------|
| `scope_excludes_rustc_stdlib_path` | Rule 3 | DROP |
| `scope_excludes_target_directory` | Rule 4 | DROP |
| `scope_excludes_target_relative` | Rule 4 | DROP |
| `scope_keeps_src_main_rs` | Rule 7 | KEEP |
| `scope_excludes_dist_server` | Rule 7 | DROP |
| `scope_excludes_dist` | Rule 7 | DROP |
| `scope_excludes_generated_rs_suffix` | Rule 6 | DROP |
| `scope_excludes_dot_generated_ext` | Rule 6 | DROP |
| `scope_excludes_out_dir_segment` | Rule 5 | DROP |
| `scope_keeps_synthetic_placeholders` | Rule 1 | KEEP |
| `scope_excludes_different_repo` | Rule 3 | DROP |
