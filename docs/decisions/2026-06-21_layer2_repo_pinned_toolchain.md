# 2026-06-21 Layer-2 runners use repo-pinned toolchain via lockfile

## Status

Implemented — `fix(checks): layer-2 runners use repo-pinned toolchain via lockfile` on
`fix/layer2-repo-pinned-deps`.

## Principle

**Linter versions come from the REPO's lockfile/manifest, never baked into Camerata.**

The old `PythonCheckRunner` called global `ruff` and `pytest`; the old `JsCheckRunner`
ran `npm run lint` / `npm run test` without ensuring the repo's `node_modules` were
installed. Both patterns let Camerata silently use whatever global binary happened to be
on the agent's PATH — a version the repo's authors never tested against and never pinned.

The fix: each runner FIRST installs the repo's deps into an isolated environment that the
repo's own lockfile controls, THEN invokes linters from that environment.

---

## Per-language install strategy

### JavaScript / TypeScript (`JsCheckRunner`)

1. Read `package.json` and verify at least one of `lint` / `test` scripts is declared
   (fail closed if neither exists).
2. Check if `node_modules/` exists at the worktree root.
   - **Present**: skip install (cached; idempotent).
   - **Absent**: detect which lockfile is present and run the appropriate install command:
     | Lockfile | Command |
     |---|---|
     | `pnpm-lock.yaml` | `pnpm install --frozen-lockfile` |
     | `yarn.lock` | `yarn install --frozen-lockfile` |
     | `package-lock.json` | `npm ci` |
     | (none) | `npm install` |
3. Run `npm run lint` and/or `npm run test`. These scripts resolve their binaries through
   the repo's `node_modules/.bin`, which contains the exact versions declared in the
   lockfile.

`--frozen-lockfile` / `npm ci` guarantee the installed tree matches the lockfile exactly;
they refuse to write an updated lockfile. This makes the gate deterministic and audit-able.

### Python (`PythonCheckRunner`)

1. Detect the dep manifest (precedence: `requirements.txt` > `pyproject.toml` > `setup.py`).
   - `Pipfile` only: fail closed (pipenv invocation is complex and not yet supported).
   - No manifest at all: fail closed.
2. Check if `.camerata-venv/` exists at the worktree root.
   - **Present**: skip venv creation (cached; idempotent).
   - **Absent**: `python3 -m venv .camerata-venv`.
3. Install deps from the repo's manifest into the venv:
   - `requirements.txt` → `.camerata-venv/bin/pip install -r requirements.txt`
   - `pyproject.toml` or `setup.py` → `.camerata-venv/bin/pip install -e .`
4. Run linters from the venv's `bin/`:
   - `.camerata-venv/bin/ruff check .`
   - `.camerata-venv/bin/pytest -q`

No global `ruff` or `pytest` binary is ever called. The venv is local to the worktree
(`.camerata-venv` is excluded from the worktree's source control by the project's
`.gitignore`; Camerata does not add it to version control).

### Go (`GoCheckRunner`) — unchanged

`go.mod` pins the module dependency graph; the `go` toolchain version is set by
`go.mod`'s `go` directive or a `toolchain` line. The runner calls `gofmt`, `go vet`,
and `go test` directly without any install step. No change needed.

### Rust (`RustCheckRunner`) — unchanged

`Cargo.lock` pins the dependency graph; `rust-toolchain.toml` pins the compiler.
`cargo fmt`, `cargo clippy`, and `cargo test` already use the repo's own lockfile.
No change needed.

---

## Caching and amortization

Fresh-worktree installs are fast in practice:
- **JavaScript**: npm, pnpm, and Yarn all maintain a global content-addressable cache
  (`~/.npm`, `~/.pnpm-store`, `~/.yarn/cache`). After the first install of a given
  package+version tuple, subsequent installs are local file copies, not network fetches.
  A `pnpm install --frozen-lockfile` on a warm cache typically takes 2-5 seconds.
- **Python**: pip maintains a wheel cache (`~/.cache/pip`). Installing from a warm cache
  is also fast (seconds, not minutes).

The `node_modules` / `.camerata-venv` presence check means that if the same worktree is
reused across multiple `check` calls (e.g. multiple rule violations in the same task),
the install step is skipped entirely on subsequent calls.

---

## Fail-closed stance

| Failure mode | Outcome |
|---|---|
| Install step fails (non-zero exit) | `Err` — worktree cannot be verified |
| Venv creation fails | `Err` |
| Package manager binary not on PATH | `Err` (spawn fails) |
| No lint or test script in `package.json` | `Err` — "could-not-run" |
| No Python manifest file | `Err` — "could-not-run" |
| Pipfile only (pipenv not supported) | `Err` — "could-not-run" |
| Lint or test tool exits non-zero | `Ok(vec![rule_id])` — violation bounced |
| Unknown worktree (no manifest) | `NoopChecks` (logged loudly) |

Install failure never produces a false clean. The coordinator treats `Err` from `check`
as a hard failure, not a green light.

---

## Test seam

`JsCheckRunner.install_program_override` and `PythonCheckRunner.python_bin_override` /
`pip_bin_override` are `#[cfg(test)]`-only fields. In tests, fake shell scripts replace
the real binaries; markers written by the scripts let tests assert "was called" /
"was skipped" without hitting the network.

This seam is invisible at compile time in non-test builds (the fields do not exist).
