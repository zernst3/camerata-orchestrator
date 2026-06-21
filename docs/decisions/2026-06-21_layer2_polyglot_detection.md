# 2026-06-21 — Layer-2 runs EVERY language in a worktree (polyglot detection)

## Status

Accepted. Implemented in `crates/checks/src/multilang.rs`.

## Context

Camerata's layer-2 [`CheckRunner`] is the post-task structural gate: after an
agent finishes, it runs that language's standard format / lint / test tools in
the worktree and maps a failure to a violated `RuleId` so the coordinator
bounces the agent once for revision.

The selector that wired this up, `runner_for_worktree`, used
`detect_language`, which returned a SINGLE language by precedence
(Rust > JS > Go > Python, root-only) and built exactly ONE
`Box<dyn CheckRunner>`.

That is a real gap for a polyglot monorepo. A repo like:

```
apps/ui/package.json          # JavaScript / TypeScript
services/api/pyproject.toml    # Python
tools/x/go.mod                 # Go
```

is one worktree but several projects. The old path detected one language
(whichever won precedence, and only if its manifest sat at the worktree root)
and ran one runner. The other languages were SILENTLY skipped: no layer-2
bounce-and-revise ran for them at all. A silent skip on a verification gate is
the worst failure mode, it reports a green light it did not earn.

## Decision

Layer-2 detects and runs EVERY language present in the worktree, each scoped to
its own subtree, and aggregates fail-closed.

### 1. `detect_languages(worktree) -> Vec<(WorktreeLanguage, PathBuf)>`

Recursively walks the worktree for manifests, pairing each detected language
with the DIRECTORY whose manifest declared it:

- `Cargo.toml` -> Rust
- `package.json` -> JavaScript
- `go.mod` -> Go
- `pyproject.toml` | `requirements.txt` | `Pipfile` -> Python

Dedup is on `(language, dir)`:

- A directory with several manifests for the SAME language (e.g.
  `pyproject.toml` + `requirements.txt`) yields ONE Python entry.
- A directory with manifests for DIFFERENT languages yields one entry per
  language.

Output is sorted by directory path then language, so the sequence is
deterministic regardless of filesystem iteration order.

The old `detect_language` (single, root-only, precedence-based) is retained: it
is still the right helper for "the one best-guess language of a directory." The
selector no longer uses it.

### 2. Pruning while walking

These directories are skipped entirely during the walk:

```
node_modules, target, .git, .camerata-venv, vendor,
dist, build, .next, __pycache__, .venv
```

This keeps the walk fast and, crucially, prevents a VENDORED manifest deep in
`node_modules/` (or `vendor/`, `target/`, etc.) from being misread as a separate
project. The list lives in one `PRUNED_DIRS` const so code, logs, and this doc
stay in sync.

Unreadable subdirectories are skipped silently rather than aborting the scan: a
permission error on one subtree must not blind the gate to the rest of the
worktree. Detection is best-effort breadth; the fail-closed honesty stance lives
in the runners (a detected project that cannot be VERIFIED returns `Err`).

### 3. `PolyglotCheckRunner` — composite, subtree-scoped, union, fail-closed

A composite `CheckRunner` holds one sub-runner per detected `(language, dir)`
project. Its `check()`:

- Runs EACH sub-runner against ITS directory (the manifest's subtree), NOT the
  worktree root, so `ruff`/`pytest`/`go test`/`cargo` see the right tree.
- Runs ALL of them; it never aborts early on the first failure.
- **Fail-closed aggregation**: if ANY sub-runner returned `Err`
  (could-not-run / toolchain missing / install failure), the composite returns
  `Err` too, with a message naming every language/dir that could not be
  verified. It NEVER reports clean just because the other projects passed. A
  half-verified polyglot tree is not a verified tree. This mirrors the existing
  per-runner fail-closed stance (toolchain missing, no check defined, install
  failure all return `Err`).
- Otherwise returns the UNION of every sub-runner's violated `RuleId`s
  (deduped). Empty means every project was clean.

### 4. `runner_for_worktree` selector change

Built from `detect_languages`:

- Zero languages -> existing `NoopChecks` plus a loud log that no layer-2 runner
  matched (the one place the gate degrades to a pass, and it does so visibly).
- One or more -> a `PolyglotCheckRunner` over all detected `(language, dir)`
  pairs, plus a log naming the detected projects.

A single-language repo simply has one entry, so its behavior is UNCHANGED. The
fleet wiring is untouched: the selector still returns `Box<dyn CheckRunner>`.

## Consequences

- Polyglot monorepos get full layer-2 coverage: every project's lint/test runs,
  violations are unioned, and any could-not-run fails the whole check closed.
- Manifests in subdirectories are now found (the old path was root-only).
- Vendored / build-output manifests cannot masquerade as projects.
- The change is confined to `multilang.rs`; no fleet or coordinator contract
  changed.

## Tests

In `multilang.rs` `#[cfg(test)]`, using the runners' existing override seams (no
network / real tools needed):

- A polyglot fixture (`apps/ui/package.json` + `services/api/pyproject.toml` +
  `tools/x/go.mod`) detects all three with the correct directories.
- Two Python manifests in one dir collapse to one Python entry; one dir with two
  different-language manifests yields one entry each.
- A `package.json` nested inside `node_modules/` is NOT detected; a manifest
  planted inside every pruned dir is ignored.
- The composite runs all sub-runners over their subtrees (not the root) and
  unions their violations.
- If one sub-runner fails-closed, the composite fails-closed AND still ran the
  others (asserted via a recording fake runner).
- A single-language repo yields a one-project composite (unchanged behavior).
- A no-manifest tree -> `NoopChecks` reporting clean.
