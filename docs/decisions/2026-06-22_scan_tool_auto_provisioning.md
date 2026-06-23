# Scan tool auto-provisioning

**Date:** 2026-06-22

## Problem

The deterministic preview pass (`scan_tools.rs`) calls semgrep and eslint as
external binaries.  The previous implementation required those tools to already
exist on the user's PATH — if they were absent the scan emitted a `CoverageNote`
("could not preview") and continued without findings.  This meant a fresh
install of Camerata on a clean machine produced no deterministic preview output
at all, even for repos with obvious semgrep or eslint findings.

The pass also invoked `semgrep --config p/ci`, which requires outbound network
access to the semgrep registry on every run.  On air-gapped machines or under
a proxy that blocks semgrep.dev the scan silently emitted a coverage note.

## Decision

Auto-provision semgrep and eslint into a stable, app-owned cache directory on
first use.  Subsequent scans skip the install (binary health-probe short-circuits).
Bundle the semgrep ruleset inside the binary assets so the scan runs fully
offline after provisioning.

## Cache-dir location

Follows the existing Camerata data-dir convention: `dirs::data_dir()` returns
the platform-standard user data directory; every Camerata store lives under
`<data_dir>/camerata/`.  The tooling sub-tree extends that pattern:

```
<data_dir>/camerata/tooling/
├── semgrep-venv/          Python venv; semgrep installed inside
│   └── bin/semgrep
└── eslint/
    ├── node_modules/
    │   └── .bin/eslint
    ├── package.json
    └── camerata.config.mjs   bundled flat config (copied from assets at install)
```

On macOS `<data_dir>` is `~/Library/Application Support`; on Linux it is
`~/.local/share`; on Windows it is `%APPDATA%\Roaming`.  So on macOS the full
path is `~/Library/Application Support/camerata/tooling/`.

This mirrors the `dirs::data_dir()` call already in `lib.rs:from_env()`, where
`projects.json`, `settings.json`, `uow.json`, etc. all live.  No new crate
dependency: `dirs` is already in `crates/server/Cargo.toml`.

## semgrep provisioning

- Requires `python3` on PATH (Camerata cannot self-bootstrap without a Python
  interpreter).
- Creates a venv: `python3 -m venv <tooling>/semgrep-venv`.
- Installs via: `<venv>/bin/pip install --quiet semgrep`.
- Health probe: `<venv>/bin/semgrep --version` must exit 0.
- Idempotent: the probe short-circuits on subsequent calls; only a failing
  probe (absent or broken binary) triggers re-install.

## eslint provisioning

- Requires `npm` on PATH.
- Creates `<tooling>/eslint/` with a minimal `package.json`.
- Installs: `npm install --save-dev eslint @typescript-eslint/parser
  @microsoft/eslint-formatter-sarif`.
- Copies the bundled flat config (`camerata.config.mjs`) into the workspace.
- Health probe: `<workspace>/node_modules/.bin/eslint --version` must exit 0.

## Bundled ruleset (offline semgrep)

The previous invocation `semgrep --config p/ci` pulled a ruleset from
`semgrep.dev` on every run.  The replacement bundles a curated YAML ruleset
at `crates/server/assets/semgrep-rules/security.yml`, covering:

- Hardcoded secrets (password/token/api_key literals assigned in source)
- `eval()` / `exec()` injection (Python, JS/TS)
- SQL string concatenation / f-string interpolation (Python, JS/TS)
- Weak or broken hash functions (MD5, SHA-1 via `hashlib` and `crypto.createHash`)
- Path traversal (file open with a variable path argument, Python)
- `subprocess(shell=True)` injection (Python)
- Unsafe `yaml.load()` without a SafeLoader (Python)

Rules target Python, JavaScript, TypeScript, Go, Java, and Ruby — the same
languages the corpus covers for security rules.  The semgrep invocation
becomes `<venv>/bin/semgrep --sarif --config <assets>/semgrep-rules --quiet .`
— fully offline, no registry call.

## Bundled eslint config

`crates/server/assets/eslint/camerata.config.mjs` is a minimal ESLint v9 flat
config that:

- Enables the rules that appear in the corpus with an `eslint:` linter source
  (eqeqeq, no-eval, no-implied-eval, no-var, prefer-const, no-console,
  no-unused-vars, no-throw-literal, no-prototype-builtins, no-cond-assign,
  no-duplicate-case, no-empty).
- Configures the `@typescript-eslint/parser` for TS files so TS-specific rule
  previews work without needing the repo's own parser config.
- Uses `try/await import()` for the parser so the config is valid (no hard
  error) even when the TS parser is absent (graceful degradation).

The scan pass uses `--config <workspace>/camerata.config.mjs` (ESLint v9 flat
config style) instead of `--no-eslintrc` (v8 legacy flag).  Per-rule overrides
(`--rule '{"id":"error"}'`) still apply on top to enable exactly the selected
rules.

## Fail-soft behavior when python3 / node is absent

The provisioning module returns `Result<PathBuf, ProvisionError>` — never
panics.  `run_one_tool` in `scan_tools.rs` maps `Err` to `anyhow::Error`, which
`run_scan_tools` catches and converts to a `CoverageNote`:

```
"Could not preview N rule(s) with semgrep: semgrep provisioning: base interpreter
 not available: python3 (required to provision semgrep via pip). It enforces once
 wired into CI."
```

The scan continues with all other tools unaffected.  The coverage note appears
in the UI's preview panel so the gap is visible, not silently swallowed.

## What is NOT changed

- **Ruff and Clippy** are not provisioned: ruff is a single static binary that
  Rust developers commonly have, and clippy ships with the Rust toolchain.
  Adding provisioning for them is a future extension if needed.
- **First-scan latency**: the first scan on a clean machine may take several
  minutes while semgrep/eslint are installed.  This is inherent to the approach;
  it is acceptable because it is one-time.
- **No app-startup blocking**: provisioning is lazy — triggered by `run_one_tool`
  inside the async scan, not during server startup.

## Module

New module: `crates/server/src/tool_provisioning.rs`.  Public surface:
`tooling_dir()`, `ensure_semgrep()`, `ensure_eslint()`, `bundled_semgrep_rules_dir()`,
`eslint_config_path()`, `eslint_workspace_dir()`, plus the probe helpers
`semgrep_is_provisioned()` and `eslint_is_provisioned()`.

Tests cover: path-resolution purity, absent-binary probe, stub-binary probe
(healthy and broken), idempotency of `ensure_*`, and bundled-asset existence.
