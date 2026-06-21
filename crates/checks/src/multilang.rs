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
//! - [`JsCheckRunner`]   — `npm run lint` + `npm run test` (package.json).
//! - [`PythonCheckRunner`] — `ruff check` + `pytest`.
//! - [`GoCheckRunner`]   — `gofmt -l` + `go vet` + `go test ./...`.
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
//!
//! Both cases fail closed. The coordinator treats a `Check` error as a hard
//! failure of the run, not a green light.
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

/// Layer-2 gate for a `package.json` worktree.
///
/// Runs the project's own `lint` and `test` npm scripts (`npm run lint`,
/// `npm run test`) so the gate honours whatever the project already defines
/// (eslint, tsc, vitest, jest, etc.) rather than guessing a toolchain. A
/// failing script maps to [`js_checks_rule`].
///
/// Honesty: if a script is not defined in `package.json`, that check
/// "could-not-run" and we return an `Err` (fail closed) — never a false clean.
/// `npm run <missing>` itself exits non-zero, but we pre-check the manifest so
/// the error message is precise about WHY the gate could not verify.
pub struct JsCheckRunner;

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
/// `Pipfile`).
///
/// Runs `ruff check .` (lint) and `pytest` (test). A failing tool maps to
/// [`python_checks_rule`].
///
/// Honesty: if `ruff` or `pytest` is not installed, the spawn `Err` propagates
/// as an `Err` from `check` (fail closed) — the work is "not verified", never
/// "clean".
pub struct PythonCheckRunner;

#[async_trait]
impl CheckRunner for PythonCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let mut violations = Vec::new();

        let lint = run_command(worktree, "ruff", &["check", "."])
            .await
            .context("running `ruff check .` (is ruff installed?)")?;
        violations.extend(map_command_to_rule(&lint, python_checks_rule()));

        let test = run_command(worktree, "pytest", &["-q"])
            .await
            .context("running `pytest` (is pytest installed?)")?;
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
        WorktreeLanguage::JavaScript => Box::new(JsCheckRunner),
        WorktreeLanguage::Python => Box::new(PythonCheckRunner),
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

    fn tmp() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cam-checks-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
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
        let err = JsCheckRunner.check(&role, &dir).await.unwrap_err();
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
        let err = JsCheckRunner.check(&role, &dir).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("reading"));
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
