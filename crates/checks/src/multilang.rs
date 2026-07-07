//! Per-language layer-2 [`CheckRunner`]s and the worktree language selector.
//!
//! The Rust runner ([`crate::RustCheckRunner`]) was historically the ONLY
//! layer-2 gate, hardcoded at every fleet/po-demo injection site. That meant a
//! JavaScript, Python, or Go worktree got no meaningful bounce-and-revise: the
//! coordinator either ran `cargo` against a non-Cargo tree (spurious) or fell
//! back to `NoopChecks` (silent pass). This module closes that gap.
//!
//! # Shape (mirrors [`crate::RustCheckRunner`])
//!
//! Each runner shells out to that language's standard *format / lint / test*
//! tools in the worktree and maps a tool failure to a violated [`RuleId`] so the
//! coordinator bounces ONCE. The Rust runner already lives in the crate root;
//! here we add:
//!
//! - [`JsCheckRunner`]   — lockfile-pinned `npm`/`pnpm`/`yarn` install + `npm run lint` + `npm run test`.
//! - [`PythonCheckRunner`] — `.camerata-venv` isolation + lockfile-pinned `ruff check` + `pytest`.
//! - [`GoCheckRunner`]   — `gofmt -l` + `go vet` + `go test ./...`.
//! - [`RubyCheckRunner`] — `Gemfile.lock`-pinned `bundle install` + `bundle exec rubocop` + `bundle exec rspec`/`rake test`.
//! - [`JavaCheckRunner`] — wrapper-pinned `./mvnw -q verify` (Maven) or `./gradlew check` (Gradle).
//! - [`CSharpCheckRunner`] — `global.json`-pinned `dotnet format --verify-no-changes` + `dotnet build` + `dotnet test`.
//!
//! Together with [`crate::RustCheckRunner`] these cover all SEVEN languages the
//! rule corpus ships rules for; the layer-2 language gap is closed.
//!
//! # Repo-pinned toolchain principle
//!
//! Linter versions come from the REPO's lockfile/manifest, never baked into
//! Camerata. A fresh per-task worktree installs deps once via the package-manager
//! global cache (fast, offline-capable after first run). The lockfile is the
//! source of truth and the change detector. See
//! `docs/decisions/2026-06-21_layer2_repo_pinned_toolchain.md`.
//!
//! # Honesty stance (fail-closed, mirrors the Rust runner + the layer-1 gate)
//!
//! [`crate::RustCheckRunner`] shells out to `cargo`; if `cargo` cannot be
//! spawned, [`crate::subprocess::run_command`] returns an `Err`, which the
//! coordinator surfaces as a `Check` error — it does NOT report a clean
//! worktree. The multi-language runners take the same stance, on TWO axes:
//!
//! 1. **Toolchain missing** (the binary is not on PATH): the spawn `Err`
//!    propagates as an `Err` from `check`. The work is "not verified", never
//!    "clean".
//! 2. **No check defined** (e.g. a package.json with no `lint`/`test` script):
//!    we likewise return an `Err`. A configured-but-absent check is a
//!    "could-not-run", not a pass. Silently treating it as clean is exactly the
//!    false-clean the gate exists to prevent.
//! 3. **Install failure**: if dep install fails, we return an `Err` (fail
//!    closed). A worktree whose deps cannot be installed cannot be verified.
//!
//! All three cases fail closed. The coordinator treats a `Check` error as a
//! hard failure of the run, not a green light.
//!
//! Rule mapping is intentionally COARSE for now (one `LAYER2-<LANG>-CHECKS-1`
//! per language): the point is that a real check runs and a failure bounces.
//! Fine-grained per-tool rule ids can be layered in later without touching the
//! coordinator contract.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use async_trait::async_trait;
use camerata_liveness::HeartbeatFn;
use camerata_core::{CheckOutcome, CheckRunner, Role, RuleId};

use crate::diagnostics_for;
use crate::subprocess::{run_command, CommandOutput};

/// Directory names pruned while walking a worktree for manifests. These are
/// build outputs, vendored deps, VCS metadata, and virtualenvs — scanning them
/// would (a) be slow and (b) misclassify a vendored `package.json` deep inside
/// `node_modules/` as a separate JS project. Kept in one place so the prune list
/// and the docs stay in sync.
const PRUNED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".camerata-venv",
    "vendor",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    ".gradle",
    "obj",
    "bin",
];

// ─── coarse per-language layer-2 rule ids ────────────────────────────────────

/// Coarse layer-2 rule id for a JavaScript / TypeScript worktree.
pub fn js_checks_rule() -> RuleId {
    RuleId("LAYER2-JS-CHECKS-1".to_string())
}

/// Coarse layer-2 rule id for a Python worktree.
pub fn python_checks_rule() -> RuleId {
    RuleId("LAYER2-PY-CHECKS-1".to_string())
}

/// Coarse layer-2 rule id for a Go worktree.
pub fn go_checks_rule() -> RuleId {
    RuleId("LAYER2-GO-CHECKS-1".to_string())
}

/// Coarse layer-2 rule id for a Ruby worktree.
pub fn ruby_checks_rule() -> RuleId {
    RuleId("LAYER2-RUBY-CHECKS-1".to_string())
}

/// Coarse layer-2 rule id for a Java worktree.
pub fn java_checks_rule() -> RuleId {
    RuleId("LAYER2-JAVA-CHECKS-1".to_string())
}

/// Coarse layer-2 rule id for a C# worktree.
pub fn csharp_checks_rule() -> RuleId {
    RuleId("LAYER2-CSHARP-CHECKS-1".to_string())
}

// ─── shared helpers ──────────────────────────────────────────────────────────

/// Map a [`CommandOutput`] to a violation: a non-zero exit yields `[rule]`,
/// a clean exit yields `[]`. Pure; unit-testable without spawning a process.
pub fn map_command_to_rule(output: &CommandOutput, rule: RuleId) -> Vec<RuleId> {
    if output.success {
        vec![]
    } else {
        vec![rule]
    }
}

// ─── JavaScript / TypeScript runner ──────────────────────────────────────────

/// Which package manager to use for a JS/TS worktree, detected from the
/// lockfile present at the worktree root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsPackageManager {
    /// `pnpm-lock.yaml` detected → `pnpm install --frozen-lockfile`
    Pnpm,
    /// `yarn.lock` detected → `yarn install --frozen-lockfile`
    Yarn,
    /// `package-lock.json` detected → `npm ci`
    NpmCi,
    /// No lockfile detected → `npm install` (fallback, not recommended)
    NpmInstall,
}

impl JsPackageManager {
    /// Detect which package manager to use from the lockfiles present in
    /// `worktree`. Precedence: pnpm > yarn > npm (package-lock.json > no
    /// lockfile).
    pub fn detect(worktree: &Path) -> Self {
        if worktree.join("pnpm-lock.yaml").is_file() {
            return Self::Pnpm;
        }
        if worktree.join("yarn.lock").is_file() {
            return Self::Yarn;
        }
        if worktree.join("package-lock.json").is_file() {
            return Self::NpmCi;
        }
        Self::NpmInstall
    }

    /// The program + args for the install step.
    pub fn install_command(&self) -> (&'static str, Vec<&'static str>) {
        match self {
            Self::Pnpm => ("pnpm", vec!["install", "--frozen-lockfile"]),
            Self::Yarn => ("yarn", vec!["install", "--frozen-lockfile"]),
            Self::NpmCi => ("npm", vec!["ci"]),
            Self::NpmInstall => ("npm", vec!["install"]),
        }
    }
}

/// Layer-2 gate for a `package.json` worktree.
///
/// # Repo-pinned toolchain
///
/// Linter versions come from the REPO's lockfile/manifest, never baked into
/// Camerata. A fresh per-task worktree installs deps once via the package-manager
/// global cache (fast, offline-capable after first run). The lockfile is the
/// source of truth and the change detector.
///
/// # Install step
///
/// Before running lint/test, checks whether `node_modules/` exists in the
/// worktree root. If absent, detects which lockfile is present and runs the
/// appropriate install command:
/// - `pnpm-lock.yaml` → `pnpm install --frozen-lockfile`
/// - `yarn.lock` → `yarn install --frozen-lockfile`
/// - `package-lock.json` → `npm ci`
/// - No lockfile → `npm install`
///
/// If `node_modules/` already exists, the install is skipped (cached).
///
/// # Honesty
///
/// If the install step fails, `check` returns `Err` (fail closed) — a
/// worktree that cannot install its deps cannot be verified. If a script is
/// not defined in `package.json`, that check "could-not-run" and we return
/// an `Err` — never a false clean.
///
/// `npm run lint` / `npm run test` resolve through the REPO's `node_modules`
/// binaries, so the exact versions declared in the repo's lockfile are used.
///
/// # Liveness / heartbeat (Phase 1b)
///
/// When constructed via [`JsCheckRunner::with_heartbeat`], every `run_command`
/// call (install + lint + test) passes the callback as `on_progress` so each
/// stdout line fires a heartbeat. Construct via [`JsCheckRunner::new`] for the
/// no-heartbeat path (unchanged behaviour).
pub struct JsCheckRunner {
    /// Optional per-line heartbeat fired on every stdout line from subprocesses.
    on_progress: Option<HeartbeatFn>,
    /// Override for the install program (used in tests to inject a fake binary).
    /// `None` means auto-detect from the worktree lockfiles.
    #[cfg(test)]
    pub install_program_override: Option<String>,
}

impl JsCheckRunner {
    /// Create a standard `JsCheckRunner` with no heartbeat (auto-detect package
    /// manager from lockfiles, backwards-compatible).
    pub fn new() -> Self {
        Self {
            on_progress: None,
            #[cfg(test)]
            install_program_override: None,
        }
    }

    /// Create a `JsCheckRunner` that fires `cb` on every stdout line from every
    /// subprocess it spawns (install, lint, test). Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self {
            on_progress: Some(cb),
            #[cfg(test)]
            install_program_override: None,
        }
    }
}

impl Default for JsCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl JsCheckRunner {
    /// Returns the set of npm `scripts` keys declared in `worktree/package.json`.
    /// An unreadable or malformed manifest is an `Err` (fail closed).
    fn declared_scripts(worktree: &Path) -> anyhow::Result<Vec<String>> {
        let manifest = worktree.join("package.json");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let json: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;
        let scripts = json
            .get("scripts")
            .and_then(|s| s.as_object())
            .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(scripts)
    }

    /// Run the package manager install step if `node_modules/` is absent.
    ///
    /// Returns `Ok(true)` if install was run (and succeeded), `Ok(false)` if
    /// `node_modules/` was already present and install was skipped, or `Err` if
    /// the install failed (fail closed).
    async fn ensure_deps_installed(&self, worktree: &Path) -> anyhow::Result<bool> {
        let node_modules = worktree.join("node_modules");
        if node_modules.is_dir() {
            // Already installed — skip (idempotent).
            return Ok(false);
        }

        let pm = JsPackageManager::detect(worktree);
        let (program, args) = pm.install_command();

        // In tests, we allow the program to be overridden to a fake binary.
        #[cfg(test)]
        let program = self
            .install_program_override
            .as_deref()
            .unwrap_or(program);
        #[cfg(not(test))]
        let program = program;

        let out = run_command(worktree, program, &args.iter().map(|s| *s).collect::<Vec<_>>(), self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{program} {}`", args.join(" ")))?;

        if !out.success {
            anyhow::bail!(
                "JsCheckRunner: dep install failed (`{program} {}`)\n{}",
                args.join(" "),
                out.combined
            );
        }

        Ok(true)
    }
}

#[async_trait]
impl CheckRunner for JsCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        let scripts = Self::declared_scripts(worktree)?;
        let has_lint = scripts.iter().any(|s| s == "lint");
        let has_test = scripts.iter().any(|s| s == "test");

        // Honesty stance: a JS worktree with NEITHER a lint nor a test script
        // cannot be layer-2 verified. Fail closed rather than report clean.
        if !has_lint && !has_test {
            anyhow::bail!(
                "JsCheckRunner could not verify {}: package.json declares no `lint` or `test` script (fail-closed: not reporting clean)",
                worktree.display()
            );
        }

        // Install deps into node_modules if absent; fail closed if install fails.
        self.ensure_deps_installed(worktree).await?;

        let mut outcome = CheckOutcome::clean();

        if has_lint {
            let out = run_command(worktree, "npm", &["run", "lint"], self.on_progress.as_ref())
                .await
                .context("running `npm run lint`")?;
            let hits = map_command_to_rule(&out, js_checks_rule());
            outcome.push_diagnostics(&diagnostics_for("npm run lint", &out.combined, &hits));
            outcome.violated.extend(hits);
        }

        if has_test {
            let out = run_command(worktree, "npm", &["run", "test"], self.on_progress.as_ref())
                .await
                .context("running `npm run test`")?;
            let hits = map_command_to_rule(&out, js_checks_rule());
            outcome.push_diagnostics(&diagnostics_for("npm run test", &out.combined, &hits));
            outcome.violated.extend(hits);
        }

        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

// ─── Python runner ───────────────────────────────────────────────────────────

/// Layer-2 gate for a Python worktree (`pyproject.toml` / `requirements.txt` /
/// `setup.py` / `Pipfile`).
///
/// # Repo-pinned toolchain
///
/// Linter versions come from the REPO's lockfile/manifest, never baked into
/// Camerata. A fresh per-task worktree installs deps once via the package-manager
/// global cache (fast, offline-capable after first run). The lockfile is the
/// source of truth and the change detector.
///
/// # Venv strategy
///
/// 1. Checks if `.camerata-venv/` exists at the worktree root. If not, creates
///    it via `python3 -m venv .camerata-venv`.
/// 2. Installs the repo's deps:
///    - `requirements.txt` → `.camerata-venv/bin/pip install -r requirements.txt`
///    - `pyproject.toml` or `setup.py` → `.camerata-venv/bin/pip install -e .`
///    - No manifest → `Err` (fail closed; nothing to install)
/// 3. Runs ruff from the venv: `.camerata-venv/bin/ruff check .`
/// 4. Runs pytest from the venv: `.camerata-venv/bin/pytest`
///
/// If venv creation or install fails, returns `Err` (fail closed).
///
/// No global `ruff` or `pytest` binary is used — only the venv-local ones.
///
/// # Liveness / heartbeat (Phase 1b)
///
/// When constructed via [`PythonCheckRunner::with_heartbeat`], every `run_command`
/// call (venv creation, pip install, ruff, pytest) passes the callback as
/// `on_progress` so each stdout line fires a heartbeat. Use [`PythonCheckRunner::new`]
/// for the no-heartbeat path (unchanged behaviour).
pub struct PythonCheckRunner {
    /// Optional per-line heartbeat fired on every stdout line from subprocesses.
    on_progress: Option<HeartbeatFn>,
    /// Override for the `python3` binary path (used in tests to inject a fake binary).
    #[cfg(test)]
    pub python_bin_override: Option<String>,
    /// Override for the `pip` binary path (used in tests to inject a fake binary).
    #[cfg(test)]
    pub pip_bin_override: Option<String>,
}

impl PythonCheckRunner {
    /// Create a standard `PythonCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self {
            on_progress: None,
            #[cfg(test)]
            python_bin_override: None,
            #[cfg(test)]
            pip_bin_override: None,
        }
    }

    /// Create a `PythonCheckRunner` that fires `cb` on every stdout line from every
    /// subprocess it spawns (venv, install, ruff, pytest). Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self {
            on_progress: Some(cb),
            #[cfg(test)]
            python_bin_override: None,
            #[cfg(test)]
            pip_bin_override: None,
        }
    }

    /// Detect which manifest file provides the dep list. Returns the path
    /// relative to the worktree root and the install style.
    fn detect_manifest(worktree: &Path) -> anyhow::Result<PythonManifest> {
        if worktree.join("requirements.txt").is_file() {
            return Ok(PythonManifest::RequirementsTxt);
        }
        if worktree.join("pyproject.toml").is_file() {
            return Ok(PythonManifest::PyprojectToml);
        }
        if worktree.join("setup.py").is_file() {
            return Ok(PythonManifest::SetupPy);
        }
        // Pipfile is an edge case — fail closed: we don't want to auto-invoke
        // pipenv without knowing whether it's installed or how it interacts with
        // the existing environment.
        if worktree.join("Pipfile").is_file() {
            anyhow::bail!(
                "PythonCheckRunner: Pipfile detected but pipenv install is not supported; \
                 add a requirements.txt or pyproject.toml (fail-closed: not reporting clean)"
            );
        }
        anyhow::bail!(
            "PythonCheckRunner could not verify {}: no requirements.txt / pyproject.toml / setup.py found (fail-closed: not reporting clean)",
            worktree.display()
        )
    }

    /// Ensure `.camerata-venv` exists in `worktree`, creating it if absent.
    ///
    /// Returns `Err` if venv creation fails (fail closed).
    async fn ensure_venv(&self, worktree: &Path) -> anyhow::Result<()> {
        let venv_dir = worktree.join(".camerata-venv");
        if venv_dir.is_dir() {
            return Ok(());
        }

        #[cfg(test)]
        let python = self
            .python_bin_override
            .as_deref()
            .unwrap_or("python3");
        #[cfg(not(test))]
        let python = "python3";

        let out = run_command(worktree, python, &["-m", "venv", ".camerata-venv"], self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{python} -m venv .camerata-venv`"))?;

        if !out.success {
            anyhow::bail!(
                "PythonCheckRunner: venv creation failed (`{python} -m venv .camerata-venv`)\n{}",
                out.combined
            );
        }

        Ok(())
    }

    /// Install deps into the venv using the detected manifest.
    async fn install_deps(&self, worktree: &Path, manifest: &PythonManifest) -> anyhow::Result<()> {
        // The pip binary lives inside the venv we just created/verified.
        let venv_pip = worktree
            .join(".camerata-venv")
            .join("bin")
            .join("pip");

        #[cfg(test)]
        let pip = self
            .pip_bin_override
            .as_deref()
            .map(|s| std::borrow::Cow::Borrowed(s))
            .unwrap_or_else(|| std::borrow::Cow::Owned(venv_pip.to_string_lossy().into_owned()));
        #[cfg(not(test))]
        let pip = venv_pip.to_string_lossy().into_owned();

        let args: Vec<&str> = match manifest {
            PythonManifest::RequirementsTxt => vec!["install", "-r", "requirements.txt"],
            PythonManifest::PyprojectToml | PythonManifest::SetupPy => vec!["install", "-e", "."],
        };

        let out = run_command(worktree, &pip, &args, self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `pip {}`", args.join(" ")))?;

        if !out.success {
            anyhow::bail!(
                "PythonCheckRunner: dep install failed (`pip {}`)\n{}",
                args.join(" "),
                out.combined
            );
        }

        Ok(())
    }
}

impl Default for PythonCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Which manifest file describes Python dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonManifest {
    RequirementsTxt,
    PyprojectToml,
    SetupPy,
}

#[async_trait]
impl CheckRunner for PythonCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        // Step 1: fail closed if no manifest.
        let manifest = Self::detect_manifest(worktree)?;

        // Step 2: ensure the venv exists (creates if absent).
        self.ensure_venv(worktree).await?;

        // Step 3: install deps from the repo's manifest into the venv.
        self.install_deps(worktree, &manifest).await?;

        // Step 4: run ruff + pytest from the venv's bin/ — NEVER the global ones.
        let venv_bin = worktree.join(".camerata-venv").join("bin");
        let ruff = venv_bin.join("ruff").to_string_lossy().into_owned();
        let pytest = venv_bin.join("pytest").to_string_lossy().into_owned();

        let mut outcome = CheckOutcome::clean();

        let lint = run_command(worktree, &ruff, &["check", "."], self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{ruff} check .`"))?;
        let lint_hits = map_command_to_rule(&lint, python_checks_rule());
        outcome.push_diagnostics(&diagnostics_for("ruff check .", &lint.combined, &lint_hits));
        outcome.violated.extend(lint_hits);

        let test = run_command(worktree, &pytest, &["-q"], self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{pytest} -q`"))?;
        let test_hits = map_command_to_rule(&test, python_checks_rule());
        outcome.push_diagnostics(&diagnostics_for("pytest -q", &test.combined, &test_hits));
        outcome.violated.extend(test_hits);

        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

// ─── Go runner ─────────────────────────────────────────────────────────────

/// Layer-2 gate for a Go module (`go.mod`).
///
/// Runs `gofmt -l .` (format — any listed file means unformatted), `go vet ./...`
/// (lint), and `go test ./...` (test). A failing tool maps to [`go_checks_rule`].
///
/// `gofmt -l` is special: it exits 0 even when files are unformatted, listing
/// them on stdout instead. So we treat NON-EMPTY stdout as the violation signal,
/// not the exit code.
///
/// Go and Rust are already pinned via `go.mod`/`rust-toolchain` respectively,
/// so no additional install step is needed here.
///
/// Honesty: if `gofmt`/`go` is not installed, the spawn `Err` propagates as an
/// `Err` from `check` (fail closed) — never a false clean.
///
/// # Liveness / heartbeat (Phase 1b)
///
/// When constructed via [`GoCheckRunner::with_heartbeat`], every `run_command`
/// call fires `cb` on each stdout line. Use [`GoCheckRunner::new`] for the
/// no-heartbeat path (backwards-compatible).
pub struct GoCheckRunner {
    /// Optional per-line heartbeat fired on every stdout line from subprocesses.
    on_progress: Option<HeartbeatFn>,
}

impl GoCheckRunner {
    /// Create a `GoCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self { on_progress: None }
    }

    /// Create a `GoCheckRunner` that fires `cb` on every stdout line from every
    /// subprocess it spawns (gofmt, go vet, go test). Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self { on_progress: Some(cb) }
    }
}

impl Default for GoCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for GoCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        let mut outcome = CheckOutcome::clean();

        // gofmt -l lists unformatted files on stdout and exits 0; treat any
        // non-whitespace output as a violation.
        let fmt = run_command(worktree, "gofmt", &["-l", "."], self.on_progress.as_ref())
            .await
            .context("running `gofmt -l .` (is gofmt installed?)")?;
        if !fmt.combined.trim().is_empty() {
            let hits = vec![go_checks_rule()];
            outcome.push_diagnostics(&diagnostics_for(
                "gofmt -l . (unformatted files)",
                &fmt.combined,
                &hits,
            ));
            outcome.violated.extend(hits);
        }

        let vet = run_command(worktree, "go", &["vet", "./..."], self.on_progress.as_ref())
            .await
            .context("running `go vet ./...` (is go installed?)")?;
        let vet_hits = map_command_to_rule(&vet, go_checks_rule());
        outcome.push_diagnostics(&diagnostics_for("go vet ./...", &vet.combined, &vet_hits));
        outcome.violated.extend(vet_hits);

        let test = run_command(worktree, "go", &["test", "./..."], self.on_progress.as_ref())
            .await
            .context("running `go test ./...`")?;
        let test_hits = map_command_to_rule(&test, go_checks_rule());
        outcome.push_diagnostics(&diagnostics_for("go test ./...", &test.combined, &test_hits));
        outcome.violated.extend(test_hits);

        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

// ─── Ruby runner ─────────────────────────────────────────────────────────────

/// Layer-2 gate for a Ruby worktree (`Gemfile`).
///
/// # Repo-pinned toolchain
///
/// Linter and test-tool versions come from the REPO's `Gemfile.lock` via
/// bundler, never baked into Camerata. We install the locked gem set with
/// `bundle install` and then invoke every check through `bundle exec`, so the
/// exact rubocop / rspec / rake versions the repo's CI uses are the ones that
/// run. A fresh per-task worktree installs gems once via bundler's shared cache.
///
/// # Steps
///
/// 1. `bundle install` to materialise the locked gem set (fail closed if it
///    fails — a worktree whose deps cannot install cannot be verified).
/// 2. Lint: `bundle exec rubocop` — only run if the repo declares a rubocop
///    config (`.rubocop.yml`). If no rubocop config AND no test command exists,
///    fail closed (nothing to verify).
/// 3. Test: `bundle exec rspec` if a `spec/` dir is present, else
///    `bundle exec rake test` if a `Rakefile` is present. Whichever the repo
///    defines.
///
/// # Honesty (fail-closed)
///
/// - `bundle` missing → spawn `Err` propagates (toolchain missing).
/// - `bundle install` fails → `Err` (install failure).
/// - Neither a rubocop config nor a runnable test command is defined → `Err`
///   ("could-not-run", never a silent clean).
///
/// # Liveness / heartbeat (Phase 1b)
///
/// When constructed via [`RubyCheckRunner::with_heartbeat`], every `run_command`
/// call (bundle install, rubocop, rspec/rake) passes the callback as `on_progress`
/// so each stdout line fires a heartbeat. Use [`RubyCheckRunner::new`] for the
/// no-heartbeat path (unchanged behaviour).
pub struct RubyCheckRunner {
    /// Optional per-line heartbeat fired on every stdout line from subprocesses.
    on_progress: Option<HeartbeatFn>,
    /// Override for the `bundle` binary path (used in tests to inject a fake binary).
    #[cfg(test)]
    pub bundle_bin_override: Option<String>,
}

impl RubyCheckRunner {
    /// Create a standard `RubyCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self {
            on_progress: None,
            #[cfg(test)]
            bundle_bin_override: None,
        }
    }

    /// Create a `RubyCheckRunner` that fires `cb` on every stdout line from every
    /// subprocess it spawns (install, rubocop, rspec/rake). Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self {
            on_progress: Some(cb),
            #[cfg(test)]
            bundle_bin_override: None,
        }
    }

    /// The `bundle` program to invoke (override-aware in tests).
    fn bundle_program(&self) -> String {
        #[cfg(test)]
        {
            self.bundle_bin_override
                .clone()
                .unwrap_or_else(|| "bundle".to_string())
        }
        #[cfg(not(test))]
        {
            "bundle".to_string()
        }
    }

    /// Does the repo declare a rubocop config at the worktree root?
    fn has_rubocop(worktree: &Path) -> bool {
        worktree.join(".rubocop.yml").is_file() || worktree.join(".rubocop.yaml").is_file()
    }

    /// The repo's test command, if any: rspec (a `spec/` dir) takes precedence
    /// over rake (`Rakefile`). Returns the `bundle exec` sub-args.
    fn test_args(worktree: &Path) -> Option<Vec<&'static str>> {
        if worktree.join("spec").is_dir() {
            return Some(vec!["exec", "rspec"]);
        }
        if worktree.join("Rakefile").is_file() {
            return Some(vec!["exec", "rake", "test"]);
        }
        None
    }

    /// Run `bundle install` (fail closed if it fails).
    async fn ensure_gems_installed(&self, worktree: &Path) -> anyhow::Result<()> {
        let bundle = self.bundle_program();
        let out = run_command(worktree, &bundle, &["install"], self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{bundle} install` (is bundler installed?)"))?;
        if !out.success {
            anyhow::bail!(
                "RubyCheckRunner: `{bundle} install` failed (fail-closed: not reporting clean)\n{}",
                out.combined
            );
        }
        Ok(())
    }
}

impl Default for RubyCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for RubyCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        let has_rubocop = Self::has_rubocop(worktree);
        let test_args = Self::test_args(worktree);

        // Honesty stance: a Ruby worktree with NEITHER a rubocop config nor a
        // runnable test command cannot be layer-2 verified. Fail closed.
        if !has_rubocop && test_args.is_none() {
            anyhow::bail!(
                "RubyCheckRunner could not verify {}: no `.rubocop.yml` and no `spec/` (rspec) or `Rakefile` (rake test) found (fail-closed: not reporting clean)",
                worktree.display()
            );
        }

        // Install the locked gem set; fail closed on failure.
        self.ensure_gems_installed(worktree).await?;

        let bundle = self.bundle_program();
        let mut outcome = CheckOutcome::clean();

        if has_rubocop {
            let lint = run_command(worktree, &bundle, &["exec", "rubocop"], self.on_progress.as_ref())
                .await
                .with_context(|| format!("running `{bundle} exec rubocop`"))?;
            let hits = map_command_to_rule(&lint, ruby_checks_rule());
            outcome.push_diagnostics(&diagnostics_for(
                &format!("{bundle} exec rubocop"),
                &lint.combined,
                &hits,
            ));
            outcome.violated.extend(hits);
        }

        if let Some(args) = test_args {
            let test = run_command(worktree, &bundle, &args, self.on_progress.as_ref())
                .await
                .with_context(|| format!("running `{bundle} {}`", args.join(" ")))?;
            let hits = map_command_to_rule(&test, ruby_checks_rule());
            outcome.push_diagnostics(&diagnostics_for(
                &format!("{bundle} {}", args.join(" ")),
                &test.combined,
                &hits,
            ));
            outcome.violated.extend(hits);
        }

        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

// ─── Java runner ─────────────────────────────────────────────────────────────

/// Which build tool a Java worktree uses, and whether the repo ships a wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaBuildTool {
    /// `pom.xml` present → Maven. `wrapper` is true if `./mvnw` exists.
    Maven { wrapper: bool },
    /// `build.gradle`/`build.gradle.kts` present → Gradle. `wrapper` is true if
    /// `./gradlew` exists.
    Gradle { wrapper: bool },
}

impl JavaBuildTool {
    /// Detect the build tool from the manifests at the worktree root. Maven
    /// (`pom.xml`) takes precedence over Gradle when both are present.
    pub fn detect(worktree: &Path) -> Option<Self> {
        if worktree.join("pom.xml").is_file() {
            return Some(Self::Maven {
                wrapper: worktree.join("mvnw").is_file(),
            });
        }
        if worktree.join("build.gradle").is_file()
            || worktree.join("build.gradle.kts").is_file()
        {
            return Some(Self::Gradle {
                wrapper: worktree.join("gradlew").is_file(),
            });
        }
        None
    }

    /// The program + args to run the build's verify/check + tests. Prefers the
    /// repo's wrapper (`./mvnw` / `./gradlew`) for toolchain pinning; falls back
    /// to a global `mvn`/`gradle`.
    ///
    /// - Maven: `verify` (runs the full lifecycle incl. test + any configured
    ///   checkstyle/spotbugs bound to the build).
    /// - Gradle: `check` (the standard aggregate task: test + any configured
    ///   verification plugins).
    pub fn check_command(&self) -> (String, Vec<String>) {
        match self {
            Self::Maven { wrapper } => {
                let program = if *wrapper { "./mvnw" } else { "mvn" };
                (program.to_string(), vec!["-q".into(), "verify".into()])
            }
            Self::Gradle { wrapper } => {
                let program = if *wrapper { "./gradlew" } else { "gradle" };
                (program.to_string(), vec!["check".into()])
            }
        }
    }
}

/// Layer-2 gate for a Java worktree (`pom.xml` for Maven, or
/// `build.gradle`/`build.gradle.kts` for Gradle).
///
/// # Repo-pinned toolchain
///
/// Prefers the repo's OWN build wrapper (`./mvnw` / `./gradlew`), which pins the
/// exact Maven/Gradle version the repo's CI uses. Only when no wrapper is present
/// does it fall back to a global `mvn`/`gradle`. Plugin/linter versions
/// (checkstyle, spotbugs, etc.) come from the repo's build config and run as part
/// of the verify/check lifecycle — they are not baked into Camerata.
///
/// # Steps
///
/// - Maven: `./mvnw -q verify` (or `mvn -q verify`). `verify` runs compile +
///   test + any verification plugins the repo binds to the build.
/// - Gradle: `./gradlew check` (or `gradle check`). `check` is Gradle's standard
///   aggregate verification task (test + configured plugins).
///
/// # Honesty (fail-closed)
///
/// - No `pom.xml`/`build.gradle`/`build.gradle.kts` at the root → `Err`
///   (could-not-run).
/// - The build tool binary (wrapper or global) cannot be spawned → spawn `Err`
///   propagates (toolchain missing).
/// - A non-zero build/test exit maps to [`java_checks_rule`].
///
/// # Liveness / heartbeat (Phase 1b)
///
/// When constructed via [`JavaCheckRunner::with_heartbeat`], the `run_command`
/// call passes the callback as `on_progress` so each stdout line fires a
/// heartbeat. Use [`JavaCheckRunner::new`] for the no-heartbeat path (unchanged
/// behaviour).
pub struct JavaCheckRunner {
    /// Optional per-line heartbeat fired on every stdout line from subprocesses.
    on_progress: Option<HeartbeatFn>,
    /// Override for the build-tool program (used in tests to inject a fake binary).
    /// `None` means auto-detect (wrapper-preferred) from the worktree.
    #[cfg(test)]
    pub program_override: Option<String>,
}

impl JavaCheckRunner {
    /// Create a standard `JavaCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self {
            on_progress: None,
            #[cfg(test)]
            program_override: None,
        }
    }

    /// Create a `JavaCheckRunner` that fires `cb` on every stdout line from the
    /// Maven/Gradle build. Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self {
            on_progress: Some(cb),
            #[cfg(test)]
            program_override: None,
        }
    }
}

impl Default for JavaCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for JavaCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        let tool = JavaBuildTool::detect(worktree).ok_or_else(|| {
            anyhow::anyhow!(
                "JavaCheckRunner could not verify {}: no pom.xml / build.gradle / build.gradle.kts found (fail-closed: not reporting clean)",
                worktree.display()
            )
        })?;

        let (program, args) = tool.check_command();

        // In tests, allow the program to be overridden to a fake binary.
        #[cfg(test)]
        let program = self.program_override.clone().unwrap_or(program);

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = run_command(worktree, &program, &arg_refs, self.on_progress.as_ref())
            .await
            .with_context(|| {
                format!(
                    "running `{program} {}` (is the build tool / wrapper available?)",
                    args.join(" ")
                )
            })?;

        let mut violated = map_command_to_rule(&out, java_checks_rule());
        violated.dedup_by(|a, b| a.0 == b.0);
        let diagnostics = diagnostics_for(
            &format!("{program} {}", args.join(" ")),
            &out.combined,
            &violated,
        );
        Ok(CheckOutcome::new(violated, diagnostics))
    }
}

// ─── C# runner ───────────────────────────────────────────────────────────────

/// Layer-2 gate for a C# / .NET worktree (`*.csproj` or `*.sln`).
///
/// # Repo-pinned toolchain
///
/// The SDK version is pinned by the repo's `global.json` if present; `dotnet`
/// honours it automatically when invoked from the worktree, so the SDK the
/// repo's CI uses is the one that runs. Analyzer + formatter rules come from the
/// repo's project files (`.editorconfig`, package references), not from Camerata.
///
/// # Steps
///
/// 1. `dotnet format --verify-no-changes` (lint: fails if any file would be
///    reformatted).
/// 2. `dotnet build` (compiles + runs the Roslyn analyzers the repo configures).
/// 3. `dotnet test` (the repo's test suite).
///
/// # Honesty (fail-closed)
///
/// - No `*.csproj`/`*.sln` at the root → `Err` (could-not-run).
/// - `dotnet` cannot be spawned → spawn `Err` propagates (toolchain missing).
/// - A non-zero exit on any step maps to [`csharp_checks_rule`].
///
/// # Liveness / heartbeat (Phase 1b)
///
/// When constructed via [`CSharpCheckRunner::with_heartbeat`], every `run_command`
/// call (format, build, test) passes the callback as `on_progress` so each stdout
/// line fires a heartbeat. Use [`CSharpCheckRunner::new`] for the no-heartbeat path
/// (unchanged behaviour).
pub struct CSharpCheckRunner {
    /// Optional per-line heartbeat fired on every stdout line from subprocesses.
    on_progress: Option<HeartbeatFn>,
    /// Override for the `dotnet` binary path (used in tests to inject a fake binary).
    #[cfg(test)]
    pub dotnet_bin_override: Option<String>,
}

impl CSharpCheckRunner {
    /// Create a standard `CSharpCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self {
            on_progress: None,
            #[cfg(test)]
            dotnet_bin_override: None,
        }
    }

    /// Create a `CSharpCheckRunner` that fires `cb` on every stdout line from every
    /// subprocess it spawns (dotnet format, build, test). Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self {
            on_progress: Some(cb),
            #[cfg(test)]
            dotnet_bin_override: None,
        }
    }

    /// The `dotnet` program to invoke (override-aware in tests).
    fn dotnet_program(&self) -> String {
        #[cfg(test)]
        {
            self.dotnet_bin_override
                .clone()
                .unwrap_or_else(|| "dotnet".to_string())
        }
        #[cfg(not(test))]
        {
            "dotnet".to_string()
        }
    }

    /// Does the worktree root hold a `*.csproj` or `*.sln`?
    fn has_project(worktree: &Path) -> bool {
        let entries = match std::fs::read_dir(worktree) {
            Ok(e) => e,
            Err(_) => return false,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".csproj") || name.ends_with(".sln") {
                return true;
            }
        }
        false
    }
}

impl Default for CSharpCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for CSharpCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        if !Self::has_project(worktree) {
            anyhow::bail!(
                "CSharpCheckRunner could not verify {}: no *.csproj or *.sln found (fail-closed: not reporting clean)",
                worktree.display()
            );
        }

        let dotnet = self.dotnet_program();
        let mut outcome = CheckOutcome::clean();

        let fmt = run_command(worktree, &dotnet, &["format", "--verify-no-changes"], self.on_progress.as_ref())
            .await
            .with_context(|| {
                format!("running `{dotnet} format --verify-no-changes` (is dotnet installed?)")
            })?;
        let fmt_hits = map_command_to_rule(&fmt, csharp_checks_rule());
        outcome.push_diagnostics(&diagnostics_for(
            &format!("{dotnet} format --verify-no-changes"),
            &fmt.combined,
            &fmt_hits,
        ));
        outcome.violated.extend(fmt_hits);

        let build = run_command(worktree, &dotnet, &["build"], self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{dotnet} build`"))?;
        let build_hits = map_command_to_rule(&build, csharp_checks_rule());
        outcome.push_diagnostics(&diagnostics_for(
            &format!("{dotnet} build"),
            &build.combined,
            &build_hits,
        ));
        outcome.violated.extend(build_hits);

        let test = run_command(worktree, &dotnet, &["test"], self.on_progress.as_ref())
            .await
            .with_context(|| format!("running `{dotnet} test`"))?;
        let test_hits = map_command_to_rule(&test, csharp_checks_rule());
        outcome.push_diagnostics(&diagnostics_for(
            &format!("{dotnet} test"),
            &test.combined,
            &test_hits,
        ));
        outcome.violated.extend(test_hits);

        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

// ─── language detection + selector ───────────────────────────────────────────

/// The language a worktree was detected as, by its manifest file(s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeLanguage {
    Rust,
    JavaScript,
    Python,
    Go,
    Ruby,
    Java,
    CSharp,
    /// No recognised manifest. The loop degrades to [`NoopChecks`]; the
    /// selector logs that no layer-2 runner matched.
    Unknown,
}

/// A no-op layer-2 runner: reports NO violations.
///
/// Used ONLY for [`WorktreeLanguage::Unknown`] worktrees, where no language was
/// detected and there is nothing to run. This is the ONE place the gate
/// degrades to a pass, and the selector logs it loudly. It is NOT the
/// fail-closed path: an unrecognised tree has no toolchain to be "missing", so
/// there is no check to fail closed on.
///
/// (Distinct from `camerata_fleet::NoopChecks`, which exists for the demos'
/// final-cargo-gate flow; this one is the selector's explicit "no match" sink.)
pub struct NoopChecks;

#[async_trait]
impl CheckRunner for NoopChecks {
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<CheckOutcome> {
        Ok(CheckOutcome::clean())
    }
}

/// Detect the worktree's language from the manifest files in its ROOT only.
///
/// Precedence is by manifest specificity. `Cargo.toml` -> Rust, `package.json`
/// -> JavaScript/TypeScript, `go.mod` -> Go, any of
/// `pyproject.toml`/`requirements.txt`/`Pipfile` -> Python. Rust is checked
/// first because a polyglot repo with a Cargo.toml is, for our fleet's
/// purposes, a Rust build.
///
/// This is the single-language, root-only helper. The selector now uses
/// [`detect_languages`] (recursive, every-language) instead; this helper is
/// retained for callers that genuinely want the single best-guess language of a
/// directory (e.g. precedence-ordered classification of one directory's
/// manifests).
pub fn detect_language(worktree: &Path) -> WorktreeLanguage {
    if worktree.join("Cargo.toml").is_file() {
        return WorktreeLanguage::Rust;
    }
    if worktree.join("package.json").is_file() {
        return WorktreeLanguage::JavaScript;
    }
    if worktree.join("go.mod").is_file() {
        return WorktreeLanguage::Go;
    }
    if worktree.join("pyproject.toml").is_file()
        || worktree.join("requirements.txt").is_file()
        || worktree.join("Pipfile").is_file()
    {
        return WorktreeLanguage::Python;
    }
    if worktree.join("Gemfile").is_file() {
        return WorktreeLanguage::Ruby;
    }
    if worktree.join("pom.xml").is_file()
        || worktree.join("build.gradle").is_file()
        || worktree.join("build.gradle.kts").is_file()
    {
        return WorktreeLanguage::Java;
    }
    if dir_has_extension(worktree, ".csproj") || dir_has_extension(worktree, ".sln") {
        return WorktreeLanguage::CSharp;
    }
    WorktreeLanguage::Unknown
}

/// True if `dir` directly holds a file whose name ends with `ext` (e.g. a
/// `*.csproj`/`*.sln`). Used for languages whose manifest is a glob, not a fixed
/// filename. Unreadable dirs return false (best-effort, like the manifest walk).
fn dir_has_extension(dir: &Path, ext: &str) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    entries.flatten().any(|entry| {
        entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
            && entry.file_name().to_string_lossy().ends_with(ext)
    })
}

/// The manifest filenames that mark each language, in precedence order within a
/// single directory. The DIRECTORY is the unit of a "project": a directory that
/// holds several manifests for the same language (e.g. `pyproject.toml` +
/// `requirements.txt`) yields ONE entry for that language; a directory that
/// holds manifests for different languages yields one entry per language.
fn language_for_manifest(file_name: &str) -> Option<WorktreeLanguage> {
    match file_name {
        "Cargo.toml" => Some(WorktreeLanguage::Rust),
        "package.json" => Some(WorktreeLanguage::JavaScript),
        "go.mod" => Some(WorktreeLanguage::Go),
        "pyproject.toml" | "requirements.txt" | "Pipfile" => Some(WorktreeLanguage::Python),
        "Gemfile" => Some(WorktreeLanguage::Ruby),
        "pom.xml" | "build.gradle" | "build.gradle.kts" => Some(WorktreeLanguage::Java),
        // C# manifests are globs, not fixed names; matched by extension.
        other if other.ends_with(".csproj") || other.ends_with(".sln") => {
            Some(WorktreeLanguage::CSharp)
        }
        _ => None,
    }
}

/// Recursively scan `worktree` for every language present, pairing each detected
/// language with the DIRECTORY whose manifest declared it.
///
/// # Why every-language
///
/// A polyglot monorepo (e.g. `apps/ui/package.json`, `services/api/pyproject.toml`,
/// and `tools/x/go.mod`) is one worktree but several projects. The old
/// [`detect_language`] returned a single precedence-winning language and the
/// selector ran exactly one runner, silently skipping the rest. This function
/// detects ALL of them so the selector can run a runner per project.
///
/// # Pruning
///
/// While walking, the directories in [`PRUNED_DIRS`] are skipped entirely. This
/// keeps the walk fast and — crucially — prevents a vendored manifest deep in
/// `node_modules/` (or `vendor/`, `target/`, etc.) from being misread as a
/// separate project.
///
/// # Dedup
///
/// Results are deduped on `(language, dir)`: a directory with several manifests
/// for the SAME language yields one entry; a directory with manifests for
/// DIFFERENT languages yields one entry each. Order is deterministic: entries
/// are sorted by directory path, then by language, so callers and tests see a
/// stable sequence regardless of filesystem iteration order.
pub fn detect_languages(worktree: &Path) -> Vec<(WorktreeLanguage, PathBuf)> {
    let mut found: Vec<(WorktreeLanguage, PathBuf)> = Vec::new();
    walk_for_manifests(worktree, &mut found);

    // Dedup on (language, dir). Sort first so dedup collapses adjacents and the
    // output order is stable.
    found.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| lang_ord(a.0).cmp(&lang_ord(b.0))));
    found.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    found
}

/// Total order over languages for deterministic sorting of detection results.
fn lang_ord(lang: WorktreeLanguage) -> u8 {
    match lang {
        WorktreeLanguage::Rust => 0,
        WorktreeLanguage::JavaScript => 1,
        WorktreeLanguage::Python => 2,
        WorktreeLanguage::Go => 3,
        WorktreeLanguage::Ruby => 4,
        WorktreeLanguage::Java => 5,
        WorktreeLanguage::CSharp => 6,
        WorktreeLanguage::Unknown => 7,
    }
}

/// Recursive helper for [`detect_languages`]. Reads `dir`, records any manifest
/// files it holds as `(language, dir)` pairs, then descends into non-pruned
/// subdirectories.
///
/// Unreadable directories are skipped silently rather than aborting the whole
/// scan — a permission error on one subtree must not blind the gate to the rest
/// of the worktree. (The fail-closed honesty stance lives in the runners: if a
/// detected project cannot be VERIFIED, its runner returns `Err`. Detection
/// itself is best-effort breadth.)
fn walk_for_manifests(dir: &Path, out: &mut Vec<(WorktreeLanguage, PathBuf)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut subdirs: Vec<PathBuf> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if PRUNED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            subdirs.push(path);
        } else if file_type.is_file() {
            let name = entry.file_name();
            if let Some(lang) = language_for_manifest(&name.to_string_lossy()) {
                out.push((lang, dir.to_path_buf()));
            }
        }
    }

    for sub in subdirs {
        walk_for_manifests(&sub, out);
    }
}

/// Composite layer-2 [`CheckRunner`] that runs one sub-runner per detected
/// `(language, dir)` project and UNIONS their violations.
///
/// # Semantics
///
/// - Each sub-runner runs against ITS directory (the manifest's subtree), not
///   the worktree root. A `services/api/pyproject.toml` project is checked at
///   `services/api`, so `ruff`/`pytest` see the right tree.
/// - ALL sub-runners run; none aborts the others early. The composite collects
///   every result before deciding the verdict.
/// - **Fail-closed aggregation**: if ANY sub-runner returns `Err`
///   (could-not-run / toolchain missing / install failure), the composite
///   returns `Err` too, with a message naming every language/dir that could not
///   be verified. It NEVER reports clean just because the other projects passed
///   — a half-verified polyglot tree is not a verified one.
/// - Otherwise it returns the UNION of every sub-runner's violated [`RuleId`]s
///   (deduped). Empty means every project was clean.
pub struct PolyglotCheckRunner {
    /// One `(language, dir, runner)` per detected project.
    sub: Vec<(WorktreeLanguage, PathBuf, Box<dyn CheckRunner>)>,
}

impl PolyglotCheckRunner {
    /// Build a composite from detected `(language, dir)` pairs, constructing the
    /// matching runner for each. `Unknown` pairs are skipped (they never appear
    /// from [`detect_languages`], but the match stays exhaustive).
    pub fn from_detected(detected: Vec<(WorktreeLanguage, PathBuf)>) -> Self {
        Self::from_detected_impl(detected, None)
    }

    /// Like [`from_detected`], but wires `cb` into every sub-runner (Rust and
    /// non-Rust alike) so subprocess stdout fires heartbeats during a tracked
    /// dev run. Each stdout line from npm/ruff/pytest/gofmt/bundle/mvnw/dotnet
    /// calls `cb()`, keeping `last_activity_ms` fresh.
    pub fn from_detected_with_heartbeat(
        detected: Vec<(WorktreeLanguage, PathBuf)>,
        cb: HeartbeatFn,
    ) -> Self {
        Self::from_detected_impl(detected, Some(cb))
    }

    fn from_detected_impl(
        detected: Vec<(WorktreeLanguage, PathBuf)>,
        on_progress: Option<HeartbeatFn>,
    ) -> Self {
        let sub = detected
            .into_iter()
            .filter_map(|(lang, dir)| {
                let runner: Box<dyn CheckRunner> = match lang {
                    WorktreeLanguage::Rust => match &on_progress {
                        Some(cb) => Box::new(crate::RustCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(crate::RustCheckRunner::new()),
                    },
                    WorktreeLanguage::JavaScript => match &on_progress {
                        Some(cb) => Box::new(JsCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(JsCheckRunner::new()),
                    },
                    WorktreeLanguage::Python => match &on_progress {
                        Some(cb) => Box::new(PythonCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(PythonCheckRunner::new()),
                    },
                    WorktreeLanguage::Go => match &on_progress {
                        Some(cb) => Box::new(GoCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(GoCheckRunner::new()),
                    },
                    WorktreeLanguage::Ruby => match &on_progress {
                        Some(cb) => Box::new(RubyCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(RubyCheckRunner::new()),
                    },
                    WorktreeLanguage::Java => match &on_progress {
                        Some(cb) => Box::new(JavaCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(JavaCheckRunner::new()),
                    },
                    WorktreeLanguage::CSharp => match &on_progress {
                        Some(cb) => Box::new(CSharpCheckRunner::with_heartbeat(cb.clone())),
                        None => Box::new(CSharpCheckRunner::new()),
                    },
                    WorktreeLanguage::Unknown => return None,
                };
                Some((lang, dir, runner))
            })
            .collect();
        Self { sub }
    }

    /// Number of detected projects this composite will check. Exposed for tests.
    pub fn project_count(&self) -> usize {
        self.sub.len()
    }
}

#[async_trait]
impl CheckRunner for PolyglotCheckRunner {
    async fn check(&self, role: &Role, _worktree: &Path) -> anyhow::Result<CheckOutcome> {
        let mut outcome = CheckOutcome::clean();
        // Aggregate could-not-run failures across ALL sub-runners; we run every
        // one before deciding the verdict (fail-closed, but never abort-early).
        let mut failures: Vec<String> = Vec::new();

        for (lang, dir, runner) in &self.sub {
            // Each sub-runner checks ITS subtree, not the worktree root.
            match runner.check(role, dir).await {
                Ok(sub) => {
                    // Prefix each project's diagnostics with its language so a
                    // polyglot bounce stays attributable in the merged tail.
                    if !sub.diagnostics.is_empty() {
                        outcome.push_diagnostics(&format!(
                            "=== {lang:?} @ {} ===\n{}",
                            dir.display(),
                            sub.diagnostics
                        ));
                    }
                    outcome.violated.extend(sub.violated);
                }
                Err(e) => failures.push(format!("{lang:?} @ {}: {e}", dir.display())),
            }
        }

        if !failures.is_empty() {
            anyhow::bail!(
                "PolyglotCheckRunner: {} of {} sub-runner(s) could not verify their project \
                 (fail-closed: NOT reporting clean despite any that passed):\n  - {}",
                failures.len(),
                self.sub.len(),
                failures.join("\n  - ")
            );
        }

        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

/// Combined runner: language-tier checks (fmt/clippy/test/polyglot) FOLLOWED BY
/// manifest-tier checks (`.camerata/checks.toml`, `in_loop = true`).
///
/// This is the runner returned by [`runner_for_worktree`]. It ensures:
///
/// 1. Built-in language checks always run first (cheapest signal first, same
///    ordering the existing `RustCheckRunner` uses internally).
/// 2. Manifest checks run AFTER — they are ADDITIVE, never replacing built-ins.
/// 3. If the language runner produces violations the manifest runner still runs,
///    so the agent gets the full picture in a single bounce-back pass.
///
/// If either sub-runner returns `Err`, the combined runner propagates it. This
/// is the fail-closed stance: a half-verified worktree is not a verified one.
pub struct CombinedCheckRunner {
    /// Handles built-in language checks (fmt/clippy/test/polyglot or noop).
    pub language: Box<dyn CheckRunner>,
    /// Handles manifest checks (`.camerata/checks.toml` `in_loop = true`).
    pub manifest: crate::manifest_runner::ManifestCheckRunner,
}

#[async_trait::async_trait]
impl CheckRunner for CombinedCheckRunner {
    async fn check(&self, role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        // Run language-tier first. On Err, propagate immediately (fail-closed).
        let mut outcome = self.language.check(role, worktree).await?;

        // Run manifest-tier. On Err, propagate (fail-closed). Manifest diagnostics
        // land AFTER the language diagnostics (additive tail).
        let manifest = self.manifest.check(role, worktree).await?;
        outcome.push_diagnostics(&manifest.diagnostics);
        outcome.violated.extend(manifest.violated);

        // Deduplicate so the bounce-back message is clean.
        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

/// Pick the right layer-2 [`CheckRunner`] for `worktree` by detecting EVERY
/// language present (recursively). This is the single injection point the fleet
/// and the po-demo use in place of the old hardcoded `RustCheckRunner::new()`.
///
/// - Zero languages detected -> a [`CombinedCheckRunner`] over [`NoopChecks`]
///   + manifest runner AND a logged warning: the loop degrades for language
///   checks, but manifest checks still run if a manifest is present.
/// - One or more -> a [`CombinedCheckRunner`] over a [`PolyglotCheckRunner`] +
///   manifest runner. A single-language repo has one polyglot entry.
///
/// The fleet wiring is untouched: this still returns `Box<dyn CheckRunner>`.
pub fn runner_for_worktree(worktree: &Path) -> Box<dyn CheckRunner> {
    runner_for_worktree_impl(worktree, None)
}

/// Like [`runner_for_worktree`], but wires a heartbeat callback into every
/// sub-runner (Rust and non-Rust alike) so every subprocess stdout line during
/// a tracked dev run fires `cb()` — keeping `last_activity_ms` fresh for all
/// seven languages. Use this at call sites where a
/// [`camerata_server::run::RunStore`] run id is in scope (i.e. the
/// `_and_activity` fleet functions).
pub fn runner_for_worktree_with_heartbeat(worktree: &Path, cb: HeartbeatFn) -> Box<dyn CheckRunner> {
    runner_for_worktree_impl(worktree, Some(cb))
}

fn runner_for_worktree_impl(worktree: &Path, on_progress: Option<HeartbeatFn>) -> Box<dyn CheckRunner> {
    let detected = detect_languages(worktree);
    let language: Box<dyn CheckRunner> = if detected.is_empty() {
        eprintln!(
            "[camerata-checks] no layer-2 runner matched worktree {} \
             (no Cargo.toml / package.json / go.mod / pyproject.toml|requirements.txt|Pipfile / \
             Gemfile / pom.xml|build.gradle|build.gradle.kts / *.csproj|*.sln \
             found anywhere outside pruned dirs {PRUNED_DIRS:?}); \
             degrading to NoopChecks — built-in layer-2 bounce-and-revise is INACTIVE, \
             manifest checks still run if .camerata/checks.toml is present",
            worktree.display()
        );
        Box::new(NoopChecks)
    } else {
        eprintln!(
            "[camerata-checks] layer-2 detected {} project(s) in worktree {}: {}",
            detected.len(),
            worktree.display(),
            detected
                .iter()
                .map(|(l, d)| format!("{l:?}@{}", d.display()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        match on_progress {
            Some(cb) => Box::new(PolyglotCheckRunner::from_detected_with_heartbeat(detected, cb)),
            None => Box::new(PolyglotCheckRunner::from_detected(detected)),
        }
    };

    let manifest = crate::manifest_runner::ManifestCheckRunner::load_from(worktree);

    Box::new(CombinedCheckRunner { language, manifest })
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn tmp() -> std::path::PathBuf {
        // Counter + PID makes each call unique even under parallel test threads
        // running at the same nanosecond (nanos alone can collide).
        static COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cam-checks-test-{}-{}-{}",
            std::process::id(),
            seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a fake executable script to `bin_dir/<name>` that exits 0 and
    /// optionally writes a marker file so tests can assert it was called.
    /// Returns the path to the script.
    ///
    /// The marker file path is `<worktree>/.fake-<name>-called`.
    #[cfg(unix)]
    fn write_fake_bin(
        bin_dir: &std::path::Path,
        name: &str,
        worktree: &std::path::Path,
        exit_code: i32,
    ) -> std::path::PathBuf {
        let marker = worktree.join(format!(".fake-{name}-called"));
        let script_path = bin_dir.join(name);
        let script = format!(
            "#!/bin/sh\ntouch '{}'\nexit {exit_code}\n",
            marker.display()
        );
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        script_path
    }

    // ── language detection / selection ────────────────────────────────────────

    #[test]
    fn detect_rust_from_cargo_toml() {
        let dir = tmp();
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Rust);
    }

    #[test]
    fn detect_js_from_package_json() {
        let dir = tmp();
        fs::write(dir.join("package.json"), "{}").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::JavaScript);
    }

    #[test]
    fn detect_go_from_go_mod() {
        let dir = tmp();
        fs::write(dir.join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Go);
    }

    #[test]
    fn detect_python_from_pyproject() {
        let dir = tmp();
        fs::write(dir.join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Python);
    }

    #[test]
    fn detect_python_from_requirements_txt() {
        let dir = tmp();
        fs::write(dir.join("requirements.txt"), "pytest\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Python);
    }

    #[test]
    fn detect_python_from_pipfile() {
        let dir = tmp();
        fs::write(dir.join("Pipfile"), "[packages]\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Python);
    }

    #[test]
    fn detect_ruby_from_gemfile() {
        let dir = tmp();
        fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Ruby);
    }

    #[test]
    fn detect_java_from_pom_xml() {
        let dir = tmp();
        fs::write(dir.join("pom.xml"), "<project></project>\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Java);
    }

    #[test]
    fn detect_java_from_build_gradle() {
        let dir = tmp();
        fs::write(dir.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Java);
    }

    #[test]
    fn detect_java_from_build_gradle_kts() {
        let dir = tmp();
        fs::write(dir.join("build.gradle.kts"), "plugins { java }\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Java);
    }

    #[test]
    fn detect_csharp_from_csproj() {
        let dir = tmp();
        fs::write(dir.join("App.csproj"), "<Project></Project>\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::CSharp);
    }

    #[test]
    fn detect_csharp_from_sln() {
        let dir = tmp();
        fs::write(dir.join("App.sln"), "Microsoft Visual Studio Solution File\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::CSharp);
    }

    #[test]
    fn detect_unknown_when_no_manifest() {
        let dir = tmp();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Unknown);
    }

    #[test]
    fn rust_takes_precedence_over_other_manifests() {
        let dir = tmp();
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Rust);
    }

    // ── command-to-rule mapping (pure) ────────────────────────────────────────

    #[test]
    fn passing_command_maps_to_no_violation() {
        let out = CommandOutput {
            combined: "ok\n".into(),
            success: true,
        };
        assert_eq!(map_command_to_rule(&out, js_checks_rule()), vec![]);
    }

    #[test]
    fn failing_command_maps_to_the_rule() {
        let out = CommandOutput {
            combined: "error\n".into(),
            success: false,
        };
        assert_eq!(
            map_command_to_rule(&out, js_checks_rule()),
            vec![js_checks_rule()]
        );
    }

    // ── JS lockfile / package-manager detection ───────────────────────────────

    #[test]
    fn js_pm_detects_pnpm_lock() {
        let dir = tmp();
        fs::write(dir.join("pnpm-lock.yaml"), "lockfileVersion: 6.0\n").unwrap();
        assert_eq!(JsPackageManager::detect(&dir), JsPackageManager::Pnpm);
    }

    #[test]
    fn js_pm_detects_yarn_lock() {
        let dir = tmp();
        fs::write(dir.join("yarn.lock"), "# yarn lockfile\n").unwrap();
        assert_eq!(JsPackageManager::detect(&dir), JsPackageManager::Yarn);
    }

    #[test]
    fn js_pm_detects_package_lock_json() {
        let dir = tmp();
        fs::write(dir.join("package-lock.json"), "{}").unwrap();
        assert_eq!(JsPackageManager::detect(&dir), JsPackageManager::NpmCi);
    }

    #[test]
    fn js_pm_falls_back_to_npm_install_when_no_lockfile() {
        let dir = tmp();
        assert_eq!(JsPackageManager::detect(&dir), JsPackageManager::NpmInstall);
    }

    #[test]
    fn js_pm_pnpm_takes_precedence_over_yarn() {
        let dir = tmp();
        fs::write(dir.join("pnpm-lock.yaml"), "lockfileVersion: 6.0\n").unwrap();
        fs::write(dir.join("yarn.lock"), "# yarn lockfile\n").unwrap();
        assert_eq!(JsPackageManager::detect(&dir), JsPackageManager::Pnpm);
    }

    // ── JS: install skipped when node_modules already exists ─────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn js_install_skipped_when_node_modules_present() {
        let dir = tmp();
        // Set up a valid package.json with lint + test.
        fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"lint":"true","test":"true"}}"#,
        )
        .unwrap();

        // Pre-create node_modules so install should be skipped.
        fs::create_dir(dir.join("node_modules")).unwrap();

        // Write a fake install binary that marks itself called.
        let bin_dir = tmp();
        let _fake_npm = write_fake_bin(&bin_dir, "fake-pm", &dir, 0);

        let runner = JsCheckRunner {
            on_progress: None,
            install_program_override: Some(bin_dir.join("fake-pm").to_string_lossy().into_owned()),
        };

        // The install logic should see node_modules and skip the fake pm.
        let result = runner.ensure_deps_installed(&dir).await;
        assert!(result.is_ok(), "ensure_deps_installed should succeed: {result:?}");
        assert_eq!(result.unwrap(), false, "should report skipped (false = not installed)");

        // The marker file should NOT exist because the fake pm was never called.
        let marker = dir.join(".fake-fake-pm-called");
        assert!(!marker.exists(), "install binary must NOT be called when node_modules is present");
    }

    // ── JS: install invoked when node_modules absent ──────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn js_install_invoked_when_node_modules_absent() {
        let dir = tmp();
        // package-lock.json → npm ci would be auto-detected.
        fs::write(dir.join("package-lock.json"), "{}").unwrap();

        // No node_modules present.

        let bin_dir = tmp();
        write_fake_bin(&bin_dir, "fake-npm-ci", &dir, 0);

        let runner = JsCheckRunner {
            on_progress: None,
            install_program_override: Some(
                bin_dir.join("fake-npm-ci").to_string_lossy().into_owned(),
            ),
        };

        let result = runner.ensure_deps_installed(&dir).await;
        assert!(result.is_ok(), "install should succeed: {result:?}");
        assert_eq!(result.unwrap(), true, "should report installed (true = ran install)");

        // The marker file MUST exist: the fake binary was called.
        let marker = dir.join(".fake-fake-npm-ci-called");
        assert!(marker.exists(), "install binary must be called when node_modules is absent");
    }

    // ── JS: install failure fails closed ─────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn js_install_failure_fails_closed() {
        let dir = tmp();
        fs::write(dir.join("package-lock.json"), "{}").unwrap();

        let bin_dir = tmp();
        // exit code 1 → install failure.
        write_fake_bin(&bin_dir, "fake-fail-pm", &dir, 1);

        let runner = JsCheckRunner {
            on_progress: None,
            install_program_override: Some(
                bin_dir.join("fake-fail-pm").to_string_lossy().into_owned(),
            ),
        };

        let result = runner.ensure_deps_installed(&dir).await;
        assert!(result.is_err(), "install failure must propagate as Err (fail closed)");
        assert!(
            result.unwrap_err().to_string().contains("dep install failed"),
            "error message must mention install failure"
        );
    }

    // ── honesty: JS with no lint/test script fails closed ─────────────────────

    #[tokio::test]
    async fn js_no_lint_or_test_script_fails_closed() {
        let dir = tmp();
        // A package.json with scripts but neither `lint` nor `test`.
        fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"build":"tsc"}}"#,
        )
        .unwrap();
        let role = Role {
            name: "Frontend".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        };
        let err = JsCheckRunner::new().check(&role, &dir).await.unwrap_err();
        // Fail-closed: an Err, not a (false) clean Ok(vec![]).
        assert!(
            err.to_string().contains("could not verify"),
            "expected fail-closed error, got: {err}"
        );
    }

    #[tokio::test]
    async fn js_missing_manifest_fails_closed() {
        let dir = tmp(); // no package.json at all
        let role = Role {
            name: "Frontend".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        };
        // declared_scripts() can't read the manifest -> Err (fail closed).
        let err = JsCheckRunner::new().check(&role, &dir).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("reading"));
    }

    // ── Python: manifest detection ────────────────────────────────────────────

    #[test]
    fn python_detects_requirements_txt() {
        let dir = tmp();
        fs::write(dir.join("requirements.txt"), "pytest\n").unwrap();
        let m = PythonCheckRunner::detect_manifest(&dir).unwrap();
        assert_eq!(m, PythonManifest::RequirementsTxt);
    }

    #[test]
    fn python_detects_pyproject_toml() {
        let dir = tmp();
        fs::write(dir.join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        let m = PythonCheckRunner::detect_manifest(&dir).unwrap();
        assert_eq!(m, PythonManifest::PyprojectToml);
    }

    #[test]
    fn python_detects_setup_py() {
        let dir = tmp();
        fs::write(dir.join("setup.py"), "from setuptools import setup\nsetup()\n").unwrap();
        let m = PythonCheckRunner::detect_manifest(&dir).unwrap();
        assert_eq!(m, PythonManifest::SetupPy);
    }

    #[test]
    fn python_requirements_txt_takes_precedence_over_pyproject() {
        let dir = tmp();
        fs::write(dir.join("requirements.txt"), "pytest\n").unwrap();
        fs::write(dir.join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        let m = PythonCheckRunner::detect_manifest(&dir).unwrap();
        assert_eq!(m, PythonManifest::RequirementsTxt);
    }

    #[test]
    fn python_missing_manifest_fails_closed() {
        let dir = tmp(); // no manifest at all
        let err = PythonCheckRunner::detect_manifest(&dir).unwrap_err();
        assert!(
            err.to_string().contains("fail-closed"),
            "expected fail-closed error, got: {err}"
        );
    }

    #[test]
    fn python_pipfile_only_fails_closed() {
        let dir = tmp();
        fs::write(dir.join("Pipfile"), "[packages]\n").unwrap();
        let err = PythonCheckRunner::detect_manifest(&dir).unwrap_err();
        assert!(
            err.to_string().contains("pipenv"),
            "expected pipenv-specific error, got: {err}"
        );
    }

    // ── Python: venv created when absent, skipped when present ───────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn python_venv_created_when_absent() {
        let dir = tmp();
        let bin_dir = tmp();
        // A fake python3 that creates the .camerata-venv dir (simulating `python3 -m venv`).
        let venv_path = dir.join(".camerata-venv");
        let venv_str = venv_path.to_string_lossy().into_owned();
        let script_path = bin_dir.join("fake-python3");
        let script = format!(
            "#!/bin/sh\nmkdir -p '{}'\ntouch '{}/fake-venv-created'\nexit 0\n",
            venv_str,
            venv_str
        );
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = PythonCheckRunner {
            on_progress: None,
            python_bin_override: Some(script_path.to_string_lossy().into_owned()),
            pip_bin_override: None,
        };

        runner.ensure_venv(&dir).await.unwrap();

        assert!(venv_path.is_dir(), ".camerata-venv should be created");
        assert!(
            venv_path.join("fake-venv-created").exists(),
            "fake python3 must have been called"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn python_venv_skipped_when_present() {
        let dir = tmp();
        let bin_dir = tmp();

        // Pre-create the venv dir.
        let venv_path = dir.join(".camerata-venv");
        fs::create_dir_all(&venv_path).unwrap();

        // A fake python3 that writes a marker — should NOT be called.
        let script_path = bin_dir.join("fake-python3-skip");
        let marker = dir.join(".fake-python3-called");
        let script = format!(
            "#!/bin/sh\ntouch '{}'\nexit 0\n",
            marker.display()
        );
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = PythonCheckRunner {
            on_progress: None,
            python_bin_override: Some(script_path.to_string_lossy().into_owned()),
            pip_bin_override: None,
        };

        runner.ensure_venv(&dir).await.unwrap();

        assert!(!marker.exists(), "python3 must NOT be called when venv already exists");
    }

    // ── Python: pip install called with correct manifest ──────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn python_pip_install_uses_requirements_txt() {
        let dir = tmp();
        let bin_dir = tmp();

        // Fake pip that records its arguments.
        let args_file = dir.join(".fake-pip-args");
        let script_path = bin_dir.join("fake-pip");
        let script = format!(
            "#!/bin/sh\necho \"$@\" > '{}'\nexit 0\n",
            args_file.display()
        );
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = PythonCheckRunner {
            on_progress: None,
            python_bin_override: None,
            pip_bin_override: Some(script_path.to_string_lossy().into_owned()),
        };

        // Pre-create the venv dir so ensure_venv skips the python3 call.
        fs::create_dir_all(dir.join(".camerata-venv").join("bin")).unwrap();

        runner
            .install_deps(&dir, &PythonManifest::RequirementsTxt)
            .await
            .unwrap();

        let recorded = fs::read_to_string(&args_file).unwrap();
        assert!(
            recorded.contains("requirements.txt"),
            "pip must be invoked with requirements.txt, got: {recorded}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn python_pip_install_uses_editable_for_pyproject() {
        let dir = tmp();
        let bin_dir = tmp();

        let args_file = dir.join(".fake-pip-args-pyproject");
        let script_path = bin_dir.join("fake-pip-pyproject");
        let script = format!(
            "#!/bin/sh\necho \"$@\" > '{}'\nexit 0\n",
            args_file.display()
        );
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = PythonCheckRunner {
            on_progress: None,
            python_bin_override: None,
            pip_bin_override: Some(script_path.to_string_lossy().into_owned()),
        };

        fs::create_dir_all(dir.join(".camerata-venv").join("bin")).unwrap();

        runner
            .install_deps(&dir, &PythonManifest::PyprojectToml)
            .await
            .unwrap();

        let recorded = fs::read_to_string(&args_file).unwrap();
        assert!(
            recorded.contains("-e") && recorded.contains("."),
            "pip must be invoked with -e . for pyproject.toml, got: {recorded}"
        );
    }

    // ── Python: venv creation failure fails closed ────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn python_venv_creation_failure_fails_closed() {
        let dir = tmp();
        let bin_dir = tmp();

        // Fake python3 that exits 1 (venv creation fails).
        let script_path = bin_dir.join("fake-python3-fail");
        let script = "#!/bin/sh\nexit 1\n";
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = PythonCheckRunner {
            on_progress: None,
            python_bin_override: Some(script_path.to_string_lossy().into_owned()),
            pip_bin_override: None,
        };

        let result = runner.ensure_venv(&dir).await;
        assert!(result.is_err(), "venv creation failure must propagate as Err (fail closed)");
        assert!(
            result.unwrap_err().to_string().contains("venv creation failed"),
            "error must mention venv creation failure"
        );
    }

    // ── Ruby runner ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ruby_no_rubocop_and_no_tests_fails_closed() {
        let dir = tmp();
        // A Gemfile but no .rubocop.yml, no spec/, no Rakefile.
        fs::write(dir.join("Gemfile"), "source 'x'\n").unwrap();
        let err = RubyCheckRunner::new()
            .check(&role(), &dir)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("could not verify"),
            "expected fail-closed error, got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ruby_passes_when_bundle_exec_succeeds() {
        let dir = tmp();
        fs::write(dir.join("Gemfile"), "source 'x'\n").unwrap();
        fs::write(dir.join(".rubocop.yml"), "{}\n").unwrap();
        fs::create_dir(dir.join("spec")).unwrap();

        // Fake bundle that exits 0 for every subcommand (install/exec rubocop/exec rspec).
        let bin_dir = tmp();
        write_fake_bin(&bin_dir, "fake-bundle", &dir, 0);
        let runner = RubyCheckRunner {
            on_progress: None,
            bundle_bin_override: Some(bin_dir.join("fake-bundle").to_string_lossy().into_owned()),
        };

        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(violations.is_empty(), "clean bundle run -> no violations: {violations:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ruby_bounces_when_a_check_fails() {
        let dir = tmp();
        fs::write(dir.join("Gemfile"), "source 'x'\n").unwrap();
        fs::write(dir.join(".rubocop.yml"), "{}\n").unwrap();

        // install (exit 0) would normally be needed, but our fake exits non-zero
        // for everything. install runs first and would fail-close. Instead use a
        // fake that fails only after install by exiting non-zero throughout, and
        // assert it surfaces as Err (install fail) — that is the fail-closed path.
        let bin_dir = tmp();
        write_fake_bin(&bin_dir, "fake-bundle-fail", &dir, 1);
        let runner = RubyCheckRunner {
            on_progress: None,
            bundle_bin_override: Some(
                bin_dir.join("fake-bundle-fail").to_string_lossy().into_owned(),
            ),
        };

        let err = runner.check(&role(), &dir).await.unwrap_err();
        assert!(
            err.to_string().contains("install` failed"),
            "bundle install failure must fail closed: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ruby_install_passes_but_rubocop_bounces() {
        let dir = tmp();
        fs::write(dir.join("Gemfile"), "source 'x'\n").unwrap();
        fs::write(dir.join(".rubocop.yml"), "{}\n").unwrap();

        // A bundle that exits 0 for `install` but 1 for `exec ...`. The first
        // positional arg distinguishes them.
        let bin_dir = tmp();
        let script_path = bin_dir.join("fake-bundle-rubocop");
        let script = "#!/bin/sh\ncase \"$1\" in\n  install) exit 0 ;;\n  *) exit 1 ;;\nesac\n";
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = RubyCheckRunner {
            on_progress: None,
            bundle_bin_override: Some(script_path.to_string_lossy().into_owned()),
        };

        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(
            violations.contains(&ruby_checks_rule()),
            "failing rubocop should bounce: {violations:?}"
        );
    }

    // ── Java runner ───────────────────────────────────────────────────────────

    #[test]
    fn java_build_tool_detects_maven_with_wrapper() {
        let dir = tmp();
        fs::write(dir.join("pom.xml"), "<project></project>\n").unwrap();
        fs::write(dir.join("mvnw"), "#!/bin/sh\n").unwrap();
        assert_eq!(
            JavaBuildTool::detect(&dir),
            Some(JavaBuildTool::Maven { wrapper: true })
        );
    }

    #[test]
    fn java_build_tool_falls_back_to_global_when_no_wrapper() {
        let dir = tmp();
        fs::write(dir.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
        let tool = JavaBuildTool::detect(&dir).unwrap();
        assert_eq!(tool, JavaBuildTool::Gradle { wrapper: false });
        let (program, _args) = tool.check_command();
        assert_eq!(program, "gradle", "no wrapper -> global gradle");
    }

    #[test]
    fn java_maven_prefers_wrapper_program() {
        let tool = JavaBuildTool::Maven { wrapper: true };
        let (program, args) = tool.check_command();
        assert_eq!(program, "./mvnw");
        assert!(args.contains(&"verify".to_string()));
    }

    #[tokio::test]
    async fn java_missing_manifest_fails_closed() {
        let dir = tmp(); // no pom.xml / build.gradle
        let err = JavaCheckRunner::new()
            .check(&role(), &dir)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("could not verify"),
            "expected fail-closed error, got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn java_passes_when_build_succeeds() {
        let dir = tmp();
        fs::write(dir.join("pom.xml"), "<project></project>\n").unwrap();
        let bin_dir = tmp();
        write_fake_bin(&bin_dir, "fake-mvn", &dir, 0);
        let runner = JavaCheckRunner {
            on_progress: None,
            program_override: Some(bin_dir.join("fake-mvn").to_string_lossy().into_owned()),
        };
        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(violations.is_empty(), "clean build -> no violations: {violations:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn java_bounces_when_build_fails() {
        let dir = tmp();
        fs::write(dir.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
        let bin_dir = tmp();
        write_fake_bin(&bin_dir, "fake-gradle-fail", &dir, 1);
        let runner = JavaCheckRunner {
            on_progress: None,
            program_override: Some(
                bin_dir.join("fake-gradle-fail").to_string_lossy().into_owned(),
            ),
        };
        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(
            violations.contains(&java_checks_rule()),
            "failing build should bounce: {violations:?}"
        );
    }

    // ── C# runner ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn csharp_missing_project_fails_closed() {
        let dir = tmp(); // no *.csproj / *.sln
        let err = CSharpCheckRunner::new()
            .check(&role(), &dir)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("could not verify"),
            "expected fail-closed error, got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn csharp_passes_when_dotnet_succeeds() {
        let dir = tmp();
        fs::write(dir.join("App.csproj"), "<Project></Project>\n").unwrap();
        let bin_dir = tmp();
        // Fake dotnet: exit 0 for format/build/test.
        write_fake_bin(&bin_dir, "fake-dotnet", &dir, 0);
        let runner = CSharpCheckRunner {
            on_progress: None,
            dotnet_bin_override: Some(bin_dir.join("fake-dotnet").to_string_lossy().into_owned()),
        };
        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(violations.is_empty(), "clean dotnet run -> no violations: {violations:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn csharp_bounces_when_a_step_fails() {
        let dir = tmp();
        fs::write(dir.join("App.sln"), "Solution\n").unwrap();
        let bin_dir = tmp();
        // dotnet that fails `format` (exit 1 for everything is fine — we just need a bounce).
        write_fake_bin(&bin_dir, "fake-dotnet-fail", &dir, 1);
        let runner = CSharpCheckRunner {
            on_progress: None,
            dotnet_bin_override: Some(
                bin_dir.join("fake-dotnet-fail").to_string_lossy().into_owned(),
            ),
        };
        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(
            violations.contains(&csharp_checks_rule()),
            "failing dotnet step should bounce: {violations:?}"
        );
    }

    // ── polyglot composite includes the new languages ─────────────────────────

    #[tokio::test]
    async fn polyglot_composite_includes_ruby_java_csharp() {
        let dir = tmp();
        let rb = dir.join("rb");
        let jv = dir.join("jv");
        let cs = dir.join("cs");
        fs::create_dir_all(&rb).unwrap();
        fs::create_dir_all(&jv).unwrap();
        fs::create_dir_all(&cs).unwrap();
        fs::write(rb.join("Gemfile"), "source 'x'\n").unwrap();
        fs::write(jv.join("pom.xml"), "<project></project>\n").unwrap();
        fs::write(cs.join("App.csproj"), "<Project></Project>\n").unwrap();

        let composite = PolyglotCheckRunner::from_detected(detect_languages(&dir));
        assert_eq!(
            composite.project_count(),
            3,
            "composite should include Ruby + Java + C#"
        );
    }

    // ── selector returns the right runner; unknown -> noop reports clean ──────

    #[tokio::test]
    async fn unknown_worktree_selector_returns_noop_that_reports_clean() {
        let dir = tmp(); // unknown
        let runner = runner_for_worktree(&dir);
        let role = Role {
            name: "x".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        };
        let violations = runner.check(&role, &dir).await.unwrap().violated;
        assert_eq!(violations, vec![], "noop reports no violations");
    }

    // ── Go: a passing real check returns no violation; a failing one bounces ──
    // These run the real toolchain when present; skipped (asserted trivially)
    // when the tool is absent so the suite stays green on machines without Go.

    #[tokio::test]
    async fn go_runner_reports_violation_on_unformatted_file() {
        if which("gofmt").is_none() {
            eprintln!("skipping: gofmt not installed");
            return;
        }
        let dir = tmp();
        fs::write(dir.join("go.mod"), "module example.com/x\n\ngo 1.21\n").unwrap();
        // Deliberately unformatted Go (extra spaces gofmt will flag).
        fs::write(
            dir.join("main.go"),
            "package main\nfunc  main()  {\n}\n",
        )
        .unwrap();
        let role = Role {
            name: "Backend".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        };
        let violations = GoCheckRunner::new().check(&role, &dir).await.unwrap().violated;
        assert!(
            violations.contains(&go_checks_rule()),
            "unformatted Go should bounce, got: {violations:?}"
        );
    }

    /// Minimal PATH probe so the real-tool tests can self-skip.
    fn which(bin: &str) -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|p| p.join(bin))
            .find(|p| p.is_file())
    }

    // ── polyglot detection (recursive, every-language, pruned) ────────────────

    fn role() -> Role {
        Role {
            name: "x".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        }
    }

    #[test]
    fn detect_languages_finds_all_three_with_correct_dirs() {
        let dir = tmp();
        let ui = dir.join("apps").join("ui");
        let api = dir.join("services").join("api");
        let tool = dir.join("tools").join("x");
        fs::create_dir_all(&ui).unwrap();
        fs::create_dir_all(&api).unwrap();
        fs::create_dir_all(&tool).unwrap();
        fs::write(ui.join("package.json"), "{}").unwrap();
        fs::write(api.join("pyproject.toml"), "[project]\nname=\"a\"\n").unwrap();
        fs::write(tool.join("go.mod"), "module x\n").unwrap();

        let detected = detect_languages(&dir);
        assert_eq!(detected.len(), 3, "should detect all three: {detected:?}");
        assert!(detected.contains(&(WorktreeLanguage::JavaScript, ui)));
        assert!(detected.contains(&(WorktreeLanguage::Python, api)));
        assert!(detected.contains(&(WorktreeLanguage::Go, tool)));
    }

    #[test]
    fn detect_languages_finds_ruby_java_csharp() {
        let dir = tmp();
        let svc = dir.join("svc");
        let api = dir.join("api");
        let app = dir.join("app");
        fs::create_dir_all(&svc).unwrap();
        fs::create_dir_all(&api).unwrap();
        fs::create_dir_all(&app).unwrap();
        fs::write(svc.join("Gemfile"), "source 'x'\n").unwrap();
        fs::write(api.join("pom.xml"), "<project></project>\n").unwrap();
        fs::write(app.join("App.csproj"), "<Project></Project>\n").unwrap();

        let detected = detect_languages(&dir);
        assert_eq!(detected.len(), 3, "should detect all three: {detected:?}");
        assert!(detected.contains(&(WorktreeLanguage::Ruby, svc)));
        assert!(detected.contains(&(WorktreeLanguage::Java, api)));
        assert!(detected.contains(&(WorktreeLanguage::CSharp, app)));
    }

    #[test]
    fn detect_languages_dedups_multiple_python_manifests_in_one_dir() {
        let dir = tmp();
        // Same dir, two Python manifests -> exactly ONE Python entry.
        fs::write(dir.join("pyproject.toml"), "[project]\nname=\"a\"\n").unwrap();
        fs::write(dir.join("requirements.txt"), "pytest\n").unwrap();

        let detected = detect_languages(&dir);
        let py: Vec<_> = detected
            .iter()
            .filter(|(l, _)| *l == WorktreeLanguage::Python)
            .collect();
        assert_eq!(py.len(), 1, "two Python manifests in one dir -> one entry: {detected:?}");
        assert_eq!(py[0].1, dir);
    }

    #[test]
    fn detect_languages_one_dir_multiple_languages_yields_one_each() {
        let dir = tmp();
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("go.mod"), "module x\n").unwrap();

        let detected = detect_languages(&dir);
        assert_eq!(detected.len(), 2, "{detected:?}");
        assert!(detected.contains(&(WorktreeLanguage::JavaScript, dir.clone())));
        assert!(detected.contains(&(WorktreeLanguage::Go, dir.clone())));
    }

    #[test]
    fn detect_languages_prunes_node_modules() {
        let dir = tmp();
        fs::write(dir.join("package.json"), "{}").unwrap();
        // A nested package.json inside node_modules must NOT be detected.
        let nm = dir.join("node_modules").join("some-dep");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("package.json"), "{}").unwrap();

        let detected = detect_languages(&dir);
        let js: Vec<_> = detected
            .iter()
            .filter(|(l, _)| *l == WorktreeLanguage::JavaScript)
            .collect();
        assert_eq!(js.len(), 1, "node_modules nested manifest must be pruned: {detected:?}");
        assert_eq!(js[0].1, dir, "only the root package.json should be detected");
    }

    #[test]
    fn detect_languages_prunes_all_noise_dirs() {
        let dir = tmp();
        // Real project at root.
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        // Plant a manifest inside each pruned dir; none should be detected.
        for pruned in PRUNED_DIRS {
            let p = dir.join(pruned).join("nested");
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("package.json"), "{}").unwrap();
        }

        let detected = detect_languages(&dir);
        assert_eq!(detected.len(), 1, "only root Cargo.toml: {detected:?}");
        assert_eq!(detected[0], (WorktreeLanguage::Rust, dir));
    }

    #[test]
    fn detect_languages_single_language_repo_one_entry() {
        let dir = tmp();
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let detected = detect_languages(&dir);
        assert_eq!(detected, vec![(WorktreeLanguage::Rust, dir)]);
    }

    #[test]
    fn detect_languages_no_manifest_is_empty() {
        let dir = tmp();
        assert!(detect_languages(&dir).is_empty());
    }

    // ── composite runner: runs all, unions, fail-closed ───────────────────────

    /// A fake sub-runner that records the path it was checked against and returns
    /// a fixed result (violations or an error).
    struct FakeRunner {
        result: std::sync::Mutex<Option<anyhow::Result<CheckOutcome>>>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    }

    impl FakeRunner {
        fn ok(rules: Vec<RuleId>, seen: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>) -> Self {
            Self {
                result: std::sync::Mutex::new(Some(Ok(CheckOutcome::new(rules, "")))),
                seen,
            }
        }
        /// Like [`ok`], but with diagnostics text (to exercise diagnostics merging).
        fn ok_with_diag(
            rules: Vec<RuleId>,
            diagnostics: &str,
            seen: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
        ) -> Self {
            Self {
                result: std::sync::Mutex::new(Some(Ok(CheckOutcome::new(rules, diagnostics)))),
                seen,
            }
        }
        fn err(msg: &str, seen: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>) -> Self {
            Self {
                result: std::sync::Mutex::new(Some(Err(anyhow::anyhow!(msg.to_string())))),
                seen,
            }
        }
    }

    #[async_trait]
    impl CheckRunner for FakeRunner {
        async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
            self.seen.lock().unwrap().push(worktree.to_path_buf());
            self.result.lock().unwrap().take().unwrap()
        }
    }

    fn composite(
        subs: Vec<(WorktreeLanguage, PathBuf, Box<dyn CheckRunner>)>,
    ) -> PolyglotCheckRunner {
        PolyglotCheckRunner { sub: subs }
    }

    #[tokio::test]
    async fn composite_runs_all_subruns_over_their_subtrees_and_unions() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ui = PathBuf::from("/work/apps/ui");
        let api = PathBuf::from("/work/services/api");
        let tool = PathBuf::from("/work/tools/x");

        let runner = composite(vec![
            (
                WorktreeLanguage::JavaScript,
                ui.clone(),
                Box::new(FakeRunner::ok(vec![js_checks_rule()], seen.clone())),
            ),
            (
                WorktreeLanguage::Python,
                api.clone(),
                Box::new(FakeRunner::ok(vec![python_checks_rule()], seen.clone())),
            ),
            (
                WorktreeLanguage::Go,
                tool.clone(),
                Box::new(FakeRunner::ok(vec![], seen.clone())),
            ),
        ]);

        let violations = runner.check(&role(), Path::new("/work")).await.unwrap().violated;

        // Union of all sub-runner violations.
        assert!(violations.contains(&js_checks_rule()));
        assert!(violations.contains(&python_checks_rule()));
        assert_eq!(violations.len(), 2, "go was clean: {violations:?}");

        // Each sub-runner was checked against ITS own subtree, not the root.
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&ui));
        assert!(seen.contains(&api));
        assert!(seen.contains(&tool));
        assert!(!seen.contains(&PathBuf::from("/work")), "must not run against root");
    }

    #[tokio::test]
    async fn polyglot_merges_subrunner_diagnostics_attributed_by_language() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ui = PathBuf::from("/work/apps/ui");
        let api = PathBuf::from("/work/services/api");

        let runner = composite(vec![
            (
                WorktreeLanguage::JavaScript,
                ui.clone(),
                Box::new(FakeRunner::ok_with_diag(
                    vec![js_checks_rule()],
                    "TS2322: Type 'string' is not assignable to type 'number'.",
                    seen.clone(),
                )),
            ),
            (
                WorktreeLanguage::Python,
                api.clone(),
                Box::new(FakeRunner::ok_with_diag(
                    vec![python_checks_rule()],
                    "ruff: F401 imported but unused",
                    seen.clone(),
                )),
            ),
        ]);

        let outcome = runner.check(&role(), Path::new("/work")).await.unwrap();
        // Both sub-runners' verbatim diagnostics survive into the merged tail...
        assert!(outcome.diagnostics.contains("TS2322"), "js diag missing: {:?}", outcome.diagnostics);
        assert!(outcome.diagnostics.contains("F401 imported but unused"), "py diag missing: {:?}", outcome.diagnostics);
        // ...each attributed to its language so a polyglot bounce stays legible.
        assert!(outcome.diagnostics.contains("JavaScript"));
        assert!(outcome.diagnostics.contains("Python"));
    }

    #[tokio::test]
    async fn composite_fails_closed_if_one_subrun_fails_and_still_runs_others() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ui = PathBuf::from("/work/apps/ui");
        let api = PathBuf::from("/work/services/api");
        let tool = PathBuf::from("/work/tools/x");

        let runner = composite(vec![
            (
                WorktreeLanguage::JavaScript,
                ui.clone(),
                Box::new(FakeRunner::ok(vec![], seen.clone())),
            ),
            (
                WorktreeLanguage::Python,
                api.clone(),
                Box::new(FakeRunner::err("ruff not installed", seen.clone())),
            ),
            (
                WorktreeLanguage::Go,
                tool.clone(),
                Box::new(FakeRunner::ok(vec![], seen.clone())),
            ),
        ]);

        let err = runner.check(&role(), Path::new("/work")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("fail-closed"), "{msg}");
        assert!(msg.contains("Python"), "names the failing language: {msg}");
        assert!(msg.contains("ruff not installed"), "carries the cause: {msg}");

        // Critically: the OTHER sub-runners still ran (no abort-early).
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&ui), "JS sub-runner must still have run");
        assert!(seen.contains(&tool), "Go sub-runner must still have run");
        assert!(seen.contains(&api));
        assert_eq!(seen.len(), 3, "all three ran despite the failure");
    }

    #[tokio::test]
    async fn composite_all_clean_returns_empty() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = composite(vec![
            (
                WorktreeLanguage::JavaScript,
                PathBuf::from("/a"),
                Box::new(FakeRunner::ok(vec![], seen.clone())),
            ),
            (
                WorktreeLanguage::Go,
                PathBuf::from("/b"),
                Box::new(FakeRunner::ok(vec![], seen.clone())),
            ),
        ]);
        let violations = runner.check(&role(), Path::new("/")).await.unwrap().violated;
        assert!(violations.is_empty(), "{violations:?}");
    }

    // ── selector: polyglot -> composite; single -> one entry; none -> noop ────

    #[tokio::test]
    async fn selector_builds_composite_over_all_detected() {
        let dir = tmp();
        let ui = dir.join("apps").join("ui");
        let api = dir.join("services").join("api");
        fs::create_dir_all(&ui).unwrap();
        fs::create_dir_all(&api).unwrap();
        fs::write(ui.join("package.json"), "{}").unwrap();
        fs::write(api.join("pyproject.toml"), "[project]\nname=\"a\"\n").unwrap();

        let detected = detect_languages(&dir);
        let composite = PolyglotCheckRunner::from_detected(detected);
        assert_eq!(composite.project_count(), 2, "composite over both projects");
    }

    #[tokio::test]
    async fn selector_single_language_repo_unchanged() {
        let dir = tmp();
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let composite = PolyglotCheckRunner::from_detected(detect_languages(&dir));
        assert_eq!(composite.project_count(), 1, "single Rust project");
    }

    #[tokio::test]
    async fn selector_no_manifest_returns_noop_reporting_clean() {
        let dir = tmp(); // no manifest anywhere
        let runner = runner_for_worktree(&dir);
        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert_eq!(violations, vec![], "noop reports clean for no-manifest tree");
    }

    // ── multilang heartbeat forwarding (Phase 1b) ─────────────────────────────
    //
    // Each non-Rust runner constructed with `with_heartbeat(cb)` must pass `Some`
    // to every `run_command` call so the heartbeat fires on subprocess output.
    // We verify this by running a real (fake-binary) subprocess and asserting
    // the counter increments — the same technique used for the Rust path.

    /// Build a counter-based `HeartbeatFn` and return the counter + fn.
    fn make_heartbeat() -> (std::sync::Arc<std::sync::atomic::AtomicU64>, HeartbeatFn) {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let count2 = count.clone();
        let cb: HeartbeatFn = std::sync::Arc::new(move || {
            count2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        (count, cb)
    }

    /// A fake binary that echoes one line to stdout and exits 0.
    #[cfg(unix)]
    fn write_echo_bin(bin_dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = bin_dir.join(name);
        fs::write(&path, "#!/bin/sh\necho heartbeat-line\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// `JsCheckRunner::with_heartbeat` forwards `Some(&cb)` to `run_command`;
    /// the heartbeat fires on each stdout line emitted by the install/lint/test
    /// subprocesses.
    #[cfg(unix)]
    #[tokio::test]
    async fn js_runner_with_heartbeat_fires_on_subprocess_output() {
        let dir = tmp();
        let bin_dir = tmp();

        // package.json with a lint + test script that each echo one line.
        let lint_bin = write_echo_bin(&bin_dir, "fake-lint");
        let test_bin = write_echo_bin(&bin_dir, "fake-test");
        let pkg = format!(
            r#"{{"scripts":{{"lint":"{}","test":"{}"}}}}"#,
            lint_bin.display(),
            test_bin.display()
        );
        fs::write(dir.join("package.json"), pkg).unwrap();
        // Pre-create node_modules so install is skipped (install is the first run_command).
        fs::create_dir(dir.join("node_modules")).unwrap();

        let (count, cb) = make_heartbeat();
        let runner = JsCheckRunner {
            on_progress: Some(cb),
            install_program_override: None,
        };

        let _ = runner.check(&role(), &dir).await;

        assert!(
            count.load(std::sync::atomic::Ordering::Relaxed) >= 2,
            "expected at least 2 heartbeat ticks (one per lint/test output line), got {}",
            count.load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    /// `JsCheckRunner::new()` passes `None` — no heartbeat fires (no-op path).
    #[cfg(unix)]
    #[tokio::test]
    async fn js_runner_new_passes_none_to_run_command() {
        let dir = tmp();
        fs::write(dir.join("package.json"), r#"{"scripts":{"lint":"true","test":"true"}}"#).unwrap();
        fs::create_dir(dir.join("node_modules")).unwrap();

        // Runner with NO heartbeat — this simply asserts it compiles + runs clean.
        let runner = JsCheckRunner::new();
        let violations = runner.check(&role(), &dir).await.unwrap().violated;
        assert!(violations.is_empty(), "no-heartbeat path should run clean: {violations:?}");
    }

    /// `from_detected_with_heartbeat` wires the heartbeat into every non-Rust
    /// sub-runner. Verify by building a JS-only detected list and asserting the
    /// runner's sub-runner holds a heartbeat (via project_count sanity + a
    /// live check that fires the counter).
    #[cfg(unix)]
    #[tokio::test]
    async fn polyglot_from_detected_with_heartbeat_wires_multilang_runners() {
        let dir = tmp();
        let js_dir = dir.join("ui");
        fs::create_dir_all(&js_dir).unwrap();
        fs::write(js_dir.join("package.json"), r#"{"scripts":{"lint":"true","test":"true"}}"#).unwrap();
        fs::create_dir(js_dir.join("node_modules")).unwrap();

        let detected = detect_languages(&dir);
        assert_eq!(detected.len(), 1, "should detect JS: {detected:?}");

        let (count, cb) = make_heartbeat();
        let composite = PolyglotCheckRunner::from_detected_with_heartbeat(detected, cb);

        // Run — the JsCheckRunner inside should have Some(cb) and fire it.
        let _ = composite.check(&role(), &dir).await;

        // `npm run lint` and `npm run test` each produce at least one line, so at
        // least 2 ticks if the heartbeat was wired in.
        assert!(
            count.load(std::sync::atomic::Ordering::Relaxed) >= 2,
            "expected >= 2 heartbeat ticks from the wired JS sub-runner, got {}",
            count.load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    /// `from_detected` (no-heartbeat) still constructs runners that pass `None` —
    /// no heartbeat fires, and the run completes normally.
    #[cfg(unix)]
    #[tokio::test]
    async fn polyglot_from_detected_no_heartbeat_still_works() {
        let dir = tmp();
        let js_dir = dir.join("ui");
        fs::create_dir_all(&js_dir).unwrap();
        fs::write(js_dir.join("package.json"), r#"{"scripts":{"lint":"true","test":"true"}}"#).unwrap();
        fs::create_dir(js_dir.join("node_modules")).unwrap();

        let detected = detect_languages(&dir);
        let composite = PolyglotCheckRunner::from_detected(detected);
        // Should run clean; no panic / no false stall.
        let violations = composite.check(&role(), &js_dir).await.unwrap().violated;
        assert!(violations.is_empty(), "no-heartbeat polyglot should run clean: {violations:?}");
    }
}
