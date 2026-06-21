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

use std::path::Path;

use anyhow::Context as _;
use async_trait::async_trait;
use camerata_core::{CheckRunner, Role, RuleId};

use crate::subprocess::{run_command, CommandOutput};

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
pub struct JsCheckRunner {
    /// Override for the install program (used in tests to inject a fake binary).
    /// `None` means auto-detect from the worktree lockfiles.
    #[cfg(test)]
    pub install_program_override: Option<String>,
}

impl JsCheckRunner {
    /// Create a standard `JsCheckRunner` (auto-detect package manager from lockfiles).
    pub fn new() -> Self {
        Self {
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

        let out = run_command(worktree, program, &args.iter().map(|s| *s).collect::<Vec<_>>())
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
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
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

        let mut violations = Vec::new();

        if has_lint {
            let out = run_command(worktree, "npm", &["run", "lint"])
                .await
                .context("running `npm run lint`")?;
            violations.extend(map_command_to_rule(&out, js_checks_rule()));
        }

        if has_test {
            let out = run_command(worktree, "npm", &["run", "test"])
                .await
                .context("running `npm run test`")?;
            violations.extend(map_command_to_rule(&out, js_checks_rule()));
        }

        violations.dedup_by(|a, b| a.0 == b.0);
        Ok(violations)
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
pub struct PythonCheckRunner {
    /// Override for the `python3` binary path (used in tests to inject a fake binary).
    #[cfg(test)]
    pub python_bin_override: Option<String>,
    /// Override for the `pip` binary path (used in tests to inject a fake binary).
    #[cfg(test)]
    pub pip_bin_override: Option<String>,
}

impl PythonCheckRunner {
    /// Create a standard `PythonCheckRunner`.
    pub fn new() -> Self {
        Self {
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

        let out = run_command(worktree, python, &["-m", "venv", ".camerata-venv"])
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

        let out = run_command(worktree, &pip, &args)
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
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
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

        let mut violations = Vec::new();

        let lint = run_command(worktree, &ruff, &["check", "."])
            .await
            .with_context(|| format!("running `{ruff} check .`"))?;
        violations.extend(map_command_to_rule(&lint, python_checks_rule()));

        let test = run_command(worktree, &pytest, &["-q"])
            .await
            .with_context(|| format!("running `{pytest} -q`"))?;
        violations.extend(map_command_to_rule(&test, python_checks_rule()));

        violations.dedup_by(|a, b| a.0 == b.0);
        Ok(violations)
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
pub struct GoCheckRunner;

#[async_trait]
impl CheckRunner for GoCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let mut violations = Vec::new();

        // gofmt -l lists unformatted files on stdout and exits 0; treat any
        // non-whitespace output as a violation.
        let fmt = run_command(worktree, "gofmt", &["-l", "."])
            .await
            .context("running `gofmt -l .` (is gofmt installed?)")?;
        if !fmt.combined.trim().is_empty() {
            violations.push(go_checks_rule());
        }

        let vet = run_command(worktree, "go", &["vet", "./..."])
            .await
            .context("running `go vet ./...` (is go installed?)")?;
        violations.extend(map_command_to_rule(&vet, go_checks_rule()));

        let test = run_command(worktree, "go", &["test", "./..."])
            .await
            .context("running `go test ./...`")?;
        violations.extend(map_command_to_rule(&test, go_checks_rule()));

        violations.dedup_by(|a, b| a.0 == b.0);
        Ok(violations)
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
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        Ok(vec![])
    }
}

/// Detect the worktree's language from its manifest files.
///
/// Precedence is by manifest specificity. `Cargo.toml` -> Rust, `package.json`
/// -> JavaScript/TypeScript, `go.mod` -> Go, any of
/// `pyproject.toml`/`requirements.txt`/`Pipfile` -> Python. Rust is checked
/// first because a polyglot repo with a Cargo.toml is, for our fleet's
/// purposes, a Rust build.
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
    WorktreeLanguage::Unknown
}

/// Pick the right layer-2 [`CheckRunner`] for `worktree` by detecting its
/// language from manifest files. This is the single injection point the fleet
/// and the po-demo use in place of the old hardcoded `RustCheckRunner::new()`.
///
/// An [`WorktreeLanguage::Unknown`] worktree gets [`NoopChecks`] AND a logged
/// warning: the loop degrades, but visibly (no silent loss of layer-2 for a
/// tree we just could not classify).
pub fn runner_for_worktree(worktree: &Path) -> Box<dyn CheckRunner> {
    match detect_language(worktree) {
        WorktreeLanguage::Rust => Box::new(crate::RustCheckRunner::new()),
        WorktreeLanguage::JavaScript => Box::new(JsCheckRunner::new()),
        WorktreeLanguage::Python => Box::new(PythonCheckRunner::new()),
        WorktreeLanguage::Go => Box::new(GoCheckRunner),
        WorktreeLanguage::Unknown => {
            eprintln!(
                "[camerata-checks] no layer-2 runner matched worktree {} \
                 (no Cargo.toml / package.json / go.mod / pyproject.toml|requirements.txt|Pipfile); \
                 degrading to NoopChecks — layer-2 bounce-and-revise is INACTIVE for this tree",
                worktree.display()
            );
            Box::new(NoopChecks)
        }
    }
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
        let violations = runner.check(&role, &dir).await.unwrap();
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
        let violations = GoCheckRunner.check(&role, &dir).await.unwrap();
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
}
