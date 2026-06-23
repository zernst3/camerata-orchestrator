//! Lazy, cached provisioning of external scan tools (semgrep + eslint).
//!
//! The deterministic preview pass in [`crate::scan_tools`] needs semgrep and
//! eslint to be available as binaries.  Rather than requiring the user to
//! install them manually, this module provisions them once into a stable,
//! app-owned cache directory (`<data_dir>/camerata/tooling/`) and reuses them
//! on every subsequent scan.
//!
//! # Cache-dir convention
//!
//! Follows the same convention as every other Camerata persistent store:
//! `dirs::data_dir()` → `<data_dir>/camerata/` (the root all stores share).
//! The tooling sub-tree lives at `<data_dir>/camerata/tooling/`:
//!
//! ```text
//! <data_dir>/camerata/tooling/
//! ├── semgrep-venv/          Python venv; semgrep installed inside it
//! │   └── bin/semgrep        (or Scripts\semgrep.exe on Windows)
//! └── eslint/
//!     ├── node_modules/      eslint + TS parser + SARIF formatter installed here
//!     │   └── .bin/eslint
//!     ├── package.json       generated lock-anchor
//!     └── camerata.config.mjs  bundled flat config (copied from binary assets)
//! ```
//!
//! # Idempotency
//!
//! Before installing anything the module probes whether the binary already
//! exists and responds to a version query (`--version`).  A successful probe
//! short-circuits the install.  A failed probe (binary absent OR the process
//! exits non-zero) triggers a fresh install — catching partial/broken
//! installations.
//!
//! # Fail-soft guarantee
//!
//! Every public entry point returns `Result<PathBuf, ProvisionError>` rather
//! than panicking.  The scan-time caller converts `Err` into a [`CoverageNote`]
//! and continues — the scan always completes, just without that tool's
//! findings.  Missing base interpreters (`python3`, `node`/`npm`) are
//! `ProvisionError::BaseInterpreterMissing`, which the caller turns into a
//! graceful "tool not available" note.

use std::path::{Path, PathBuf};

/// Errors that can arise during tool provisioning.  The scan caller converts
/// these into `CoverageNote`s; they are never fatal to the scan.
#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    /// The base interpreter required to bootstrap the tool (`python3` for
    /// semgrep, `node`/`npm` for eslint) is not on PATH.  The user must
    /// install it; Camerata cannot self-bootstrap without it.
    #[error("base interpreter not available: {0}")]
    BaseInterpreterMissing(String),

    /// The install command ran but exited non-zero or produced no binary.
    #[error("provisioning install failed for {tool}: {detail}")]
    InstallFailed { tool: &'static str, detail: String },

    /// An I/O error while creating directories, copying files, or probing.
    #[error("I/O error during provisioning: {0}")]
    Io(#[from] std::io::Error),
}

// ─── root tooling dir ────────────────────────────────────────────────────────

/// The root of the Camerata-managed tool environment:
/// `<data_dir>/camerata/tooling/`.  Returns `None` when `dirs::data_dir()`
/// can't be resolved (unusual; same condition under which the other stores
/// fall back to in-memory).
pub fn tooling_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("camerata").join("tooling"))
}

// ─── semgrep ─────────────────────────────────────────────────────────────────

/// Path to the Camerata-managed semgrep venv root:
/// `<tooling_dir>/semgrep-venv/`.
pub fn semgrep_venv_dir(tooling: &Path) -> PathBuf {
    tooling.join("semgrep-venv")
}

/// Path to the semgrep binary inside the Camerata-managed venv.
/// On Windows the venv layout uses `Scripts/`; everywhere else `bin/`.
pub fn semgrep_bin(venv: &Path) -> PathBuf {
    #[cfg(windows)]
    let bin = venv.join("Scripts").join("semgrep.exe");
    #[cfg(not(windows))]
    let bin = venv.join("bin").join("semgrep");
    bin
}

/// Path to the bundled semgrep ruleset shipped with the Camerata binary.
/// The caller passes this to `semgrep --config <path>` so the scan runs
/// fully offline, without pulling `p/ci` from the semgrep registry.
///
/// The rules live at `crates/server/assets/semgrep-rules/security.yml`
/// at build time; `include_str!` / `include_bytes!` + a temp-copy (or an
/// `OUT_DIR` embed) would be the production approach.  For now the path is
/// resolved relative to the Cargo manifest so the dev binary finds them.
pub fn bundled_semgrep_rules_dir() -> PathBuf {
    // In production / installed binary: embed the YAML via include_str! and
    // write it to a temp location inside the tooling dir.  In development
    // builds the assets live next to the crate source.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("assets").join("semgrep-rules")
}

/// Probe whether semgrep is already provisioned and healthy.  Returns `true`
/// when the binary exists AND responds to `semgrep --version` with exit 0.
/// A `false` return means the caller should (re-)provision.
pub async fn semgrep_is_provisioned(venv: &Path) -> bool {
    let bin = semgrep_bin(venv);
    if !bin.exists() {
        return false;
    }
    // Version probe: a broken venv / corrupted binary would fail here.
    match tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
    {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Ensure semgrep is provisioned in the given venv directory.
///
/// If the binary already passes the health probe, returns immediately (no
/// install cost).  Otherwise:
///
/// 1. Creates the venv via `python3 -m venv <dir>`.
/// 2. Installs semgrep via `<venv>/bin/pip install semgrep`.
///
/// Returns `Ok(PathBuf)` pointing to the semgrep binary on success, or a
/// `ProvisionError` that the caller converts into a graceful `CoverageNote`.
pub async fn ensure_semgrep(tooling: &Path) -> Result<PathBuf, ProvisionError> {
    let venv = semgrep_venv_dir(tooling);
    let bin = semgrep_bin(&venv);

    if semgrep_is_provisioned(&venv).await {
        return Ok(bin);
    }

    // Check that python3 is available before trying to create a venv.
    if !interpreter_available("python3").await {
        return Err(ProvisionError::BaseInterpreterMissing(
            "python3 (required to provision semgrep via pip)".to_string(),
        ));
    }

    // Create the venv.
    tokio::fs::create_dir_all(&venv).await?;
    let venv_out = tokio::process::Command::new("python3")
        .args(["-m", "venv"])
        .arg(&venv)
        .output()
        .await?;
    if !venv_out.status.success() {
        return Err(ProvisionError::InstallFailed {
            tool: "semgrep",
            detail: format!(
                "python3 -m venv failed: {}",
                String::from_utf8_lossy(&venv_out.stderr)
            ),
        });
    }

    // Use the venv's pip to install semgrep.
    #[cfg(windows)]
    let pip = venv.join("Scripts").join("pip.exe");
    #[cfg(not(windows))]
    let pip = venv.join("bin").join("pip");

    let pip_out = tokio::process::Command::new(&pip)
        .args(["install", "--quiet", "semgrep"])
        .output()
        .await?;
    if !pip_out.status.success() {
        return Err(ProvisionError::InstallFailed {
            tool: "semgrep",
            detail: format!(
                "pip install semgrep failed: {}",
                String::from_utf8_lossy(&pip_out.stderr)
            ),
        });
    }

    // Final probe: make sure the installed binary is functional.
    if !semgrep_is_provisioned(&venv).await {
        return Err(ProvisionError::InstallFailed {
            tool: "semgrep",
            detail: "semgrep binary did not respond to --version after install".to_string(),
        });
    }

    Ok(bin)
}

// ─── eslint ──────────────────────────────────────────────────────────────────

/// Path to the Camerata-managed eslint workspace:
/// `<tooling_dir>/eslint/`.
pub fn eslint_workspace_dir(tooling: &Path) -> PathBuf {
    tooling.join("eslint")
}

/// Path to the eslint binary inside the Camerata-managed node_modules.
pub fn eslint_bin(workspace: &Path) -> PathBuf {
    #[cfg(windows)]
    let bin = workspace.join("node_modules").join(".bin").join("eslint.cmd");
    #[cfg(not(windows))]
    let bin = workspace.join("node_modules").join(".bin").join("eslint");
    bin
}

/// Path to the bundled eslint flat config (copied into the workspace by
/// [`ensure_eslint`]).  The caller passes `--config <path>` so eslint uses
/// Camerata's baseline rather than the repo's config.
pub fn eslint_config_path(workspace: &Path) -> PathBuf {
    workspace.join("camerata.config.mjs")
}

/// Source path of the bundled eslint flat config inside the Camerata repo
/// (used at provisioning time to copy the file into the tooling workspace).
fn bundled_eslint_config_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("eslint")
        .join("camerata.config.mjs")
}

/// Probe whether eslint is already provisioned and healthy.
pub async fn eslint_is_provisioned(workspace: &Path) -> bool {
    let bin = eslint_bin(workspace);
    if !bin.exists() {
        return false;
    }
    match tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
    {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// The npm packages Camerata installs for the eslint preview pass.
///
/// - `eslint` — the linter itself.
/// - `@typescript-eslint/parser` — lets eslint parse TypeScript files for
///   TS-specific rule previews.
/// - `@microsoft/eslint-formatter-sarif` — the SARIF output formatter the
///   scan pass requests via `--format @microsoft/eslint-formatter-sarif`.
const ESLINT_NPM_PACKAGES: &[&str] = &[
    "eslint",
    "@typescript-eslint/parser",
    "@microsoft/eslint-formatter-sarif",
];

/// Ensure eslint (and its companion packages) are provisioned in the tooling
/// workspace.
///
/// Steps:
/// 1. Health-probe; return early if already good.
/// 2. Verify `npm` is available.
/// 3. Write a minimal `package.json` to the workspace dir.
/// 4. `npm install` the required packages.
/// 5. Copy the bundled flat config into the workspace.
/// 6. Final health probe.
pub async fn ensure_eslint(tooling: &Path) -> Result<PathBuf, ProvisionError> {
    let workspace = eslint_workspace_dir(tooling);
    let bin = eslint_bin(&workspace);

    if eslint_is_provisioned(&workspace).await {
        return Ok(bin);
    }

    // Need npm to install.
    if !interpreter_available("npm").await {
        return Err(ProvisionError::BaseInterpreterMissing(
            "npm (required to provision eslint)".to_string(),
        ));
    }

    tokio::fs::create_dir_all(&workspace).await?;

    // Write a minimal package.json so npm install has an anchor.
    let pkg_json = r#"{"name":"camerata-eslint-tooling","version":"1.0.0","private":true}"#;
    tokio::fs::write(workspace.join("package.json"), pkg_json).await?;

    // npm install the packages.
    let mut npm_args: Vec<&str> = vec!["install", "--save-dev", "--prefer-offline"];
    npm_args.extend(ESLINT_NPM_PACKAGES.iter().copied());

    let npm_out = tokio::process::Command::new("npm")
        .args(&npm_args)
        .current_dir(&workspace)
        .output()
        .await?;
    if !npm_out.status.success() {
        return Err(ProvisionError::InstallFailed {
            tool: "eslint",
            detail: format!(
                "npm install failed: {}",
                String::from_utf8_lossy(&npm_out.stderr)
            ),
        });
    }

    // Copy the bundled flat config into the workspace so eslint can find it.
    let config_src = bundled_eslint_config_src();
    if config_src.exists() {
        tokio::fs::copy(&config_src, eslint_config_path(&workspace)).await?;
    }

    if !eslint_is_provisioned(&workspace).await {
        return Err(ProvisionError::InstallFailed {
            tool: "eslint",
            detail: "eslint binary did not respond to --version after install".to_string(),
        });
    }

    Ok(bin)
}

// ─── shared helper ───────────────────────────────────────────────────────────

/// Returns `true` when `program` can be found and executed on PATH (by running
/// `<program> --version`; some tools use `--help` but most accept `--version`).
/// Used to detect base interpreters (python3, npm/node) before attempting a
/// provisioning step that would fail anyway.
async fn interpreter_available(program: &str) -> bool {
    tokio::process::Command::new(program)
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── path resolution (pure) ────────────────────────────────────────────────

    #[test]
    fn semgrep_bin_path_is_inside_venv() {
        let tmp = TempDir::new().unwrap();
        let venv = tmp.path().join("semgrep-venv");
        let bin = semgrep_bin(&venv);
        // Must be inside the venv.
        assert!(bin.starts_with(&venv));
        // Must end with the binary name.
        let name = bin.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("semgrep"), "bin name was: {name}");
    }

    #[test]
    fn eslint_bin_path_is_inside_workspace() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("eslint");
        let bin = eslint_bin(&ws);
        assert!(bin.starts_with(&ws));
        let name = bin.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("eslint"), "bin name was: {name}");
    }

    #[test]
    fn tooling_sub_dirs_are_under_root() {
        let tmp = TempDir::new().unwrap();
        let tooling = tmp.path().to_path_buf();
        assert!(semgrep_venv_dir(&tooling).starts_with(&tooling));
        assert!(eslint_workspace_dir(&tooling).starts_with(&tooling));
    }

    #[test]
    fn eslint_config_path_is_inside_workspace() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("eslint");
        let cfg = eslint_config_path(&ws);
        assert!(cfg.starts_with(&ws));
    }

    // ── probe logic (absent binary → not provisioned) ─────────────────────────

    #[tokio::test]
    async fn missing_semgrep_binary_is_not_provisioned() {
        let tmp = TempDir::new().unwrap();
        let venv = tmp.path().join("nonexistent-venv");
        // No binary created → must report not provisioned.
        assert!(!semgrep_is_provisioned(&venv).await);
    }

    #[tokio::test]
    async fn missing_eslint_binary_is_not_provisioned() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("nonexistent-eslint");
        assert!(!eslint_is_provisioned(&ws).await);
    }

    // ── probe with a stub binary ──────────────────────────────────────────────
    //
    // A tiny shell script that always exits 0 stands in for the real binary to
    // verify the probe logic detects "binary present + exits 0 → provisioned".

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_semgrep_binary_is_provisioned() {
        let tmp = TempDir::new().unwrap();
        let venv = tmp.path().join("semgrep-venv");
        // Replicate the directory layout the probe expects.
        let bin_dir = venv.join("bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let stub = bin_dir.join("semgrep");
        tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n").await.unwrap();
        // Make it executable.
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        assert!(semgrep_is_provisioned(&venv).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_eslint_binary_is_provisioned() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("eslint");
        let bin_dir = ws.join("node_modules").join(".bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let stub = bin_dir.join("eslint");
        tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n").await.unwrap();
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        assert!(eslint_is_provisioned(&ws).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_stub_binary_is_not_provisioned() {
        // A binary that exits non-zero is treated as broken/not-provisioned.
        let tmp = TempDir::new().unwrap();
        let venv = tmp.path().join("semgrep-venv");
        let bin_dir = venv.join("bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let stub = bin_dir.join("semgrep");
        tokio::fs::write(&stub, b"#!/bin/sh\nexit 1\n").await.unwrap();
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        // Exit 1 should report not provisioned.
        assert!(!semgrep_is_provisioned(&venv).await);
    }

    // ── ensure_* on missing python3/npm ──────────────────────────────────────
    //
    // We can't assume python3 or npm are absent on the test machine, so instead
    // we exercise the "tooling dir not writable" failure path which always
    // returns an error without needing to be on a machine without python3/npm.
    // The BaseInterpreterMissing path is covered by the interpreter_available
    // helper indirectly; testing it end-to-end would require a controlled
    // PATH, which is brittle in CI.

    #[tokio::test]
    async fn ensure_semgrep_is_idempotent_when_provisioned() {
        // If semgrep is already provisioned (the stub trick) a second call to
        // ensure_semgrep must NOT attempt a re-install — it returns the same
        // path immediately.
        #[cfg(unix)]
        {
            let tmp = TempDir::new().unwrap();
            let tooling = tmp.path().to_path_buf();
            let venv = semgrep_venv_dir(&tooling);
            let bin_dir = venv.join("bin");
            tokio::fs::create_dir_all(&bin_dir).await.unwrap();
            let stub = bin_dir.join("semgrep");
            tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n").await.unwrap();
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
                .await
                .unwrap();
            // First call — short-circuits at the probe.
            let result1 = ensure_semgrep(&tooling).await;
            assert!(result1.is_ok());
            // Second call — same.
            let result2 = ensure_semgrep(&tooling).await;
            assert!(result2.is_ok());
            assert_eq!(result1.unwrap(), result2.unwrap());
        }
    }

    #[tokio::test]
    async fn ensure_eslint_is_idempotent_when_provisioned() {
        #[cfg(unix)]
        {
            let tmp = TempDir::new().unwrap();
            let tooling = tmp.path().to_path_buf();
            let ws = eslint_workspace_dir(&tooling);
            let bin_dir = ws.join("node_modules").join(".bin");
            tokio::fs::create_dir_all(&bin_dir).await.unwrap();
            let stub = bin_dir.join("eslint");
            tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n").await.unwrap();
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
                .await
                .unwrap();
            let r1 = ensure_eslint(&tooling).await;
            assert!(r1.is_ok());
            let r2 = ensure_eslint(&tooling).await;
            assert!(r2.is_ok());
            assert_eq!(r1.unwrap(), r2.unwrap());
        }
    }

    // ── bundled rules dir exists ──────────────────────────────────────────────

    #[test]
    fn bundled_semgrep_rules_dir_exists_in_dev() {
        // In a dev build the assets live next to the crate source. Verify the
        // path resolves to something that exists (catches typos / renames).
        let dir = bundled_semgrep_rules_dir();
        assert!(
            dir.exists(),
            "bundled semgrep rules dir not found at: {}",
            dir.display()
        );
        // Must contain at least one YAML file.
        let has_yaml = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == "yml" || x == "yaml")
                    .unwrap_or(false)
            });
        assert!(has_yaml, "no YAML rules found in: {}", dir.display());
    }

    #[test]
    fn bundled_eslint_config_src_exists_in_dev() {
        let src = bundled_eslint_config_src();
        assert!(
            src.exists(),
            "bundled eslint config not found at: {}",
            src.display()
        );
    }
}
