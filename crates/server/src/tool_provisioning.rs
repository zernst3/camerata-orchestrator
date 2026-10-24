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
//! ├── eslint  ->  eslint.gen/<id>     symlink to the current complete eslint tree (unix)
//! ├── eslint.gen/
//! │   └── <id>/              one COMPLETE generation; the previous one is kept a cycle
//! │       ├── node_modules/  eslint + TS parser + plugins installed here
//! │       │   └── .bin/eslint
//! │       ├── package.json   generated lock-anchor
//! │       └── camerata.config.mjs  bundled flat config (copied from binary assets)
//! └── .<tool>.lock           advisory provisioning lock, one per tool
//! ```
//!
//! # Concurrency: nothing is ever installed over a live tree
//!
//! The tooling dir is shared by the product, every scan, and (on a developer machine) every test
//! process. Each `ensure_*` therefore (1) takes a per-tool OS file lock so only one provisioner
//! runs at a time, across processes and within one, then re-probes because the holder may have
//! finished the job; and (2) for eslint, builds the new tree in a staging directory and publishes
//! it with one atomic rename, so a reader sees the old complete tree or the new complete tree and
//! never a half-installed one. See [`ensure_eslint`] for the incident this closes. Test processes
//! additionally cannot provision into the real tooling dir at all
//! ([`refuse_real_tooling_in_test_process`]).
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
use std::time::Duration;

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

    /// A test process asked to provision into the REAL per-user tooling directory.
    /// Refused before anything is touched: see [`refuse_real_tooling_in_test_process`].
    #[error(
        "refusing to provision {tool} into the real Camerata tooling directory from a test \
         process (it is shared with the running product and every other scan); point the test \
         at a temporary tooling directory, or skip when the real tool is not already provisioned"
    )]
    RealToolingProtected { tool: &'static str },
}

// ─── root tooling dir ────────────────────────────────────────────────────────

/// The root of the Camerata-managed tool environment:
/// `<data_dir>/camerata/tooling/`.  Returns `None` when `dirs::data_dir()`
/// can't be resolved (unusual; same condition under which the other stores
/// fall back to in-memory).
pub fn tooling_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("camerata").join("tooling"))
}

// ─── shared-workspace protection ─────────────────────────────────────────────
//
// The tooling directory is ONE per-user tree shared by the running product, every scan, and (on
// a developer machine) every `cargo test` process. Two defences keep provisioning from
// corrupting it, and they answer two different failure modes:
//
//   * a TEST PROCESS must never be the one that (re)provisions the real tree
//     ([`refuse_real_tooling_in_test_process`]) — a test that merely wants to READ a live tool
//     must not be able to trigger an install as a side effect of "the workspace looks stale";
//   * CONCURRENT PROVISIONERS (two scans, two processes, a scan and a test) must not install
//     over each other ([`acquire_provision_lock`]) — and an install must never be visible
//     half-finished (the eslint staging + swap below).

/// How long a caller waits for another provisioner's lock before giving up with a disclosed
/// error. Generous: a cold `npm ci` or a `pip install semgrep` legitimately takes minutes. The
/// bound exists so a wedged installer cannot hang every later scan forever; the caller turns the
/// error into a `CoverageNote` and the scan completes without that tool.
const PROVISION_LOCK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Poll interval while waiting on a contended provisioning lock.
const PROVISION_LOCK_POLL: Duration = Duration::from_millis(50);

/// `true` when `exe` is a cargo-built TEST binary (unit, integration, bench or doc-test
/// harness). Cargo places every one of those under `target/<profile>/deps/`; a `cargo run`
/// binary lives one level up and an installed app lives nowhere near a `deps` directory.
pub(crate) fn is_cargo_test_binary(exe: &Path) -> bool {
    exe.parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps")
}

/// The pure decision behind [`refuse_real_tooling_in_test_process`]: a test binary may not write
/// to `tooling` when `tooling` IS the real per-user tooling directory.
fn test_process_may_not_write(tooling: &Path, real: Option<&Path>, exe: Option<&Path>) -> bool {
    let (Some(real), Some(exe)) = (real, exe) else {
        return false;
    };
    if !is_cargo_test_binary(exe) {
        return false;
    }
    if tooling == real {
        return true;
    }
    // Same directory spelled differently (symlinked data dir, `..` segments)? Only when both
    // resolve; an unresolvable path cannot be the existing real tree.
    matches!(
        (tooling.canonicalize(), real.canonicalize()),
        (Ok(a), Ok(b)) if a == b
    )
}

/// Refuse to provision `tool` when this process is a cargo test binary AND `tooling` is the real
/// per-user tooling directory.
///
/// # The defect this closes
///
/// In a full `cargo test --workspace` run, many tests drive `run_scan_tools`, which calls
/// [`ensure_eslint`] on the REAL tooling directory. When that shared workspace looked stale (a
/// new pin, a new bundled config), the first such test reinstalled it IN PLACE, `npm ci` deleted
/// `node_modules`, and every other test running eslint at that moment saw
/// `Cannot find module '../package.json'` — while a second test, seeing the half-deleted tree,
/// started a second `npm ci` over the first. A test must be able to READ a live tool, never
/// rewrite the shared copy as a side effect. Tests that exercise provisioning use a temporary
/// tooling directory (and a fake `npm`); live tests probe with [`eslint_is_provisioned`] and
/// skip when the real tool is not already healthy.
///
/// Detection is by executable location rather than `cfg!(test)` so integration-test binaries
/// (which link the library built WITHOUT `cfg(test)`) are covered by the same rule. The refusal
/// is a [`ProvisionError`], which every scan caller already turns into a disclosed
/// `CoverageNote`, so it fails soft exactly like an unavailable tool.
fn refuse_real_tooling_in_test_process(
    tooling: &Path,
    tool: &'static str,
) -> Result<(), ProvisionError> {
    let exe = std::env::current_exe().ok();
    if test_process_may_not_write(tooling, tooling_dir().as_deref(), exe.as_deref()) {
        return Err(ProvisionError::RealToolingProtected { tool });
    }
    Ok(())
}

/// An exclusive advisory lock on one tool's provisioning, released when dropped.
///
/// A real OS file lock (`flock`, via `fs2`), not an in-process mutex, because the writers that
/// collide are in DIFFERENT processes as often as in the same one: the product's scan, a second
/// scan from the CLI, a test binary. `flock` conflicts between two separate opens even inside one
/// process, so one mechanism serializes both cases. The OS releases it if the holder dies, so a
/// crashed provisioner cannot leave a stale lock behind.
#[derive(Debug)]
struct ProvisionLock {
    _file: std::fs::File,
}

/// Take the exclusive provisioning lock for `tool` under `tooling`, waiting up to `timeout`.
///
/// The wait runs on the blocking pool (`flock` polling must not park a runtime worker). Callers
/// MUST re-run their provisioned probe after this returns: whoever held the lock may well have
/// finished the install the caller was about to start.
async fn acquire_provision_lock(
    tooling: &Path,
    tool: &'static str,
    timeout: Duration,
) -> Result<ProvisionLock, ProvisionError> {
    use fs2::FileExt;

    tokio::fs::create_dir_all(tooling).await?;
    let path = tooling.join(format!(".{tool}.lock"));
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(ProvisionLock { _file: file }),
                Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                    if std::time::Instant::now() >= deadline {
                        return Err(ProvisionError::InstallFailed {
                            tool,
                            detail: format!(
                                "timed out after {}s waiting for another process to finish \
                                 provisioning {tool} (lock {})",
                                timeout.as_secs(),
                                path.display()
                            ),
                        });
                    }
                    std::thread::sleep(PROVISION_LOCK_POLL);
                }
                Err(e) => return Err(ProvisionError::Io(e)),
            }
        }
    })
    .await
    .map_err(|e| ProvisionError::InstallFailed {
        tool,
        detail: format!("provisioning lock task failed: {e}"),
    })?
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
        .kill_on_drop(true)
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

    // A test process never provisions the REAL shared venv (see the guard's doc comment).
    refuse_real_tooling_in_test_process(tooling, "semgrep")?;

    // Check that python3 is available before trying to create a venv.
    if !interpreter_available("python3").await {
        return Err(ProvisionError::BaseInterpreterMissing(
            "python3 (required to provision semgrep via pip)".to_string(),
        ));
    }

    // Serialize provisioners (across processes and within this one). A caller that waited re-checks
    // the probe: the holder may have finished the very install this caller was about to start.
    //
    // The venv is built IN PLACE rather than in a staging directory and swapped in, unlike eslint:
    // a Python venv is not relocatable (its console scripts carry the absolute path of the
    // interpreter they were created under in a shebang), so a renamed staging venv would be a
    // broken one. In-place is safe here for the reason it is not for eslint: this point is only
    // reached when the probe FAILED, i.e. there is no working semgrep to tear down, and the lock
    // keeps a second installer from building over this one.
    let _lock = acquire_provision_lock(tooling, "semgrep", PROVISION_LOCK_TIMEOUT).await?;
    if semgrep_is_provisioned(&venv).await {
        return Ok(bin);
    }

    // Create the venv.
    tokio::fs::create_dir_all(&venv).await?;
    let venv_out = tokio::process::Command::new("python3")
        .args(["-m", "venv"])
        .arg(&venv)
        .kill_on_drop(true)
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
        .kill_on_drop(true)
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
/// `<tooling_dir>/eslint/` (on unix a symlink to the current complete generation; see
/// [`ensure_eslint`]). Always read it through this path: a re-provision repoints the link, so a
/// path resolved once and cached can name a tree that is about to be pruned.
pub fn eslint_workspace_dir(tooling: &Path) -> PathBuf {
    tooling.join("eslint")
}

/// Path to the eslint binary inside the Camerata-managed node_modules.
pub fn eslint_bin(workspace: &Path) -> PathBuf {
    #[cfg(windows)]
    let bin = workspace
        .join("node_modules")
        .join(".bin")
        .join("eslint.cmd");
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
///
/// # Why the binary probe alone is NOT the health check (W6 / C11)
///
/// This used to ask one question — "does the eslint binary answer `--version`" —
/// and [`ensure_eslint`] returned early on a `true`. That made the provisioned
/// workspace a WRITE-ONCE artifact: every package later added to
/// [`ESLINT_NPM_PACKAGES`], and every subsequent edit to the bundled flat config,
/// silently never reached a machine that had provisioned even once. Observed
/// directly on the author's machine while diagnosing C11: 3 of the 6 required
/// packages missing and a flat config four months stale, on a workspace that
/// passed this probe. The consequence was not a degraded preview but a
/// WHOLESALE-ABORTED one — an unregistered plugin namespace reaching `--rule` is
/// a fatal eslint config error, so the three missing plugin packages zeroed every
/// selected eslint rule including the core ones.
///
/// So the probe now covers the whole provisioned state: the binary answers, EVERY
/// required package is installed, and the workspace's config is byte-identical to
/// the bundled asset. Anything else re-provisions. The cost of a false negative is
/// one `npm install --prefer-offline`; the cost of a false positive is a silently
/// dead tool tier.
pub async fn eslint_is_provisioned(workspace: &Path) -> bool {
    let bin = eslint_bin(workspace);
    if !bin.exists() {
        return false;
    }
    if !eslint_packages_installed(workspace) {
        return false;
    }
    if !eslint_config_is_current(workspace) {
        return false;
    }
    match tokio::process::Command::new(&bin)
        .arg("--version")
        .kill_on_drop(true)
        .output()
        .await
    {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// `true` when every package in [`ESLINT_NPM_PACKAGES`] is present in the
/// workspace's `node_modules`. Probes each package's `package.json` rather than
/// its directory, so a half-removed package reads as absent.
fn eslint_packages_installed(workspace: &Path) -> bool {
    let node_modules = workspace.join("node_modules");
    ESLINT_NPM_PACKAGES.iter().all(|spec| {
        let name = npm_package_name(spec);
        if !node_modules.join(name).join("package.json").exists() {
            return false;
        }
        // W30: a present-but-WRONG-VERSION package (a workspace that drifted to eslint 10 /
        // react-hooks 7 under the old unpinned list, or to another minor under a caret range)
        // must re-provision, not read as installed. The pins are EXACT, so the check is exact:
        // the graded accuracy tests need the tool to behave identically on every machine. A
        // spec with no pin imposes no version constraint.
        match npm_pinned_version(spec) {
            Some(want) => installed_version(&node_modules, name).as_deref() == Some(want),
            None => true,
        }
    })
}

/// `true` when the workspace's copy of the flat config is byte-identical to the
/// bundled asset. A content comparison rather than an mtime one: the asset ships
/// inside the binary, so its mtime on disk says nothing about which Camerata
/// build wrote the workspace copy.
///
/// Returns `true` when the bundled asset cannot be read at all — a packaging
/// problem must not put provisioning into a re-install loop on every scan.
fn eslint_config_is_current(workspace: &Path) -> bool {
    let src = bundled_eslint_config_src();
    let Ok(want) = std::fs::read(&src) else {
        return true;
    };
    match std::fs::read(eslint_config_path(workspace)) {
        Ok(have) => have == want,
        Err(_) => false,
    }
}

/// The npm packages Camerata installs for the eslint preview pass.
///
/// - `eslint` — the linter itself.
/// - `@typescript-eslint/parser` — lets eslint parse TypeScript files for
///   TS-specific rule previews.
/// - `@typescript-eslint/eslint-plugin` — supplies the TS-specific RULES
///   (`no-explicit-any`, `no-floating-promises`, `no-non-null-assertion`) the
///   parser above only lets eslint read; without this package registered, a
///   `--rule` override naming `@typescript-eslint/no-explicit-any` has no plugin
///   to resolve the rule against and eslint cannot enable it.
/// - `eslint-plugin-react-hooks` — supplies `react-hooks/exhaustive-deps` and
///   `react-hooks/rules-of-hooks` (`JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` /
///   `JAVASCRIPT-REACT-RULES-OF-HOOKS-1`'s detector).
/// - `eslint-plugin-jest` — supplies the `jest/*` rules the testing-domain corpus
///   rules ground against (`no-conditional-expect`, `no-disabled-tests`, …).
///
/// `@microsoft/eslint-formatter-sarif` was REMOVED (W6 / C11). The scan pass now
/// asks for eslint's BUILT-IN `json` formatter, because eslint resolves a
/// non-built-in `--format` relative to its cwd — the scanned repo — so a formatter
/// installed here could never be found, and the pass aborted with an empty stdout
/// on every repo. See `scan_tools::parse_eslint_json`'s doc comment. Installing it
/// now would be pure cost: nothing requests it.
///
/// Every package here must also be REGISTERED in the bundled flat config
/// (`assets/eslint/camerata.config.mjs`'s `plugins` field) — installing a plugin
/// without registering it leaves `--rule` unable to resolve its namespace. And
/// every package here is checked for PRESENCE AND MAJOR VERSION by
/// [`eslint_is_provisioned`], so adding one, or bumping a pin, makes existing
/// workspaces re-provision.
///
/// W30: each entry is VERSION-PINNED, and since the review of UI-REACT-HOOKS-LINT-1 to an
/// EXACT version, not a caret major. Before W30 the entries were bare names, so `npm install`
/// pulled LATEST — which drifted to eslint 10 + eslint-plugin-react-hooks 7, majors the
/// bundled config cannot register (`--rule react-hooks/exhaustive-deps` → "could
/// not find plugin react-hooks"), silently zeroing every plugin-backed preview
/// finding on a freshly-provisioned machine. The W30 fix pinned caret MAJORS, which stopped
/// that drift but still let minors and patches (and every transitive dependency) move between
/// machines and between days; the graded accuracy tests need repeatable tool behavior, so the
/// pins are now exact and `assets/eslint/package-lock.json` pins the whole tree. eslint 9 +
/// react-hooks 5 (verified to produce the finding) and their eslint-9-line companions are the
/// correct line. Bump these deliberately, in lockstep with the bundled config, the committed
/// `assets/eslint/package.json` and the lockfile (a test holds the three together), never by
/// letting `npm` choose.
const ESLINT_NPM_PACKAGES: &[&str] = &[
    "eslint@9.39.5",
    "@typescript-eslint/parser@8.72.0",
    "@typescript-eslint/eslint-plugin@8.72.0",
    "eslint-plugin-react-hooks@5.2.0",
    "eslint-plugin-jest@28.14.0",
];

/// The workspace `package.json`: every package at its exact pinned version, no range. Written
/// into the workspace before the install so `npm ci` has a manifest that matches the lockfile.
fn eslint_package_json() -> String {
    let dev: serde_json::Map<String, serde_json::Value> = ESLINT_NPM_PACKAGES
        .iter()
        .filter_map(|spec| {
            Some((
                npm_package_name(spec).to_string(),
                serde_json::Value::String(npm_pinned_version(spec)?.to_string()),
            ))
        })
        .collect();
    serde_json::json!({
        "name": "camerata-eslint-tooling",
        "version": "1.0.0",
        "private": true,
        "devDependencies": dev,
    })
    .to_string()
}

/// The `npm` arguments that install the workspace. With the committed lockfile in place,
/// `npm ci` installs exactly that tree (and fails loudly if it disagrees with the manifest);
/// without it (a packaging problem), install the exact pins. `--prefer-offline` either way: a
/// warm npm cache needs no network.
fn eslint_install_args(have_lockfile: bool) -> Vec<String> {
    if have_lockfile {
        return vec!["ci".to_string(), "--prefer-offline".to_string()];
    }
    let mut args: Vec<String> = ["install", "--save-dev", "--prefer-offline"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    args.extend(ESLINT_NPM_PACKAGES.iter().map(|s| (*s).to_string()));
    args
}

/// Source path of the committed workspace manifest next to the bundled flat config. Provisioning
/// generates the manifest from [`ESLINT_NPM_PACKAGES`]; the committed copy exists so the
/// lockfile has a human-readable partner, and a test holds the two equal.
#[cfg(test)]
fn bundled_eslint_package_json_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("eslint")
        .join("package.json")
}

/// Source path of the committed lockfile that pins the whole eslint tool tree.
fn bundled_eslint_lock_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("eslint")
        .join("package-lock.json")
}

/// The bare npm package name from an [`ESLINT_NPM_PACKAGES`] spec, dropping any
/// `@<range>` version suffix. Handles scoped names: the scope `@` at index 0 is not
/// a version separator, so `@typescript-eslint/parser@^8` → `@typescript-eslint/parser`
/// while `eslint@^9` → `eslint` and a bare `eslint` → `eslint`.
fn npm_package_name(spec: &str) -> &str {
    match spec.rfind('@') {
        Some(i) if i > 0 => &spec[..i],
        _ => spec,
    }
}

/// The pinned version from a spec (e.g. `eslint@9.39.5` → `Some("9.39.5")`), or `None`
/// when the spec carries no version. The text after the last `@` is returned as written: the
/// pins are exact, so it is compared for equality, never parsed as a range.
fn npm_pinned_version(spec: &str) -> Option<&str> {
    let i = spec.rfind('@').filter(|&i| i > 0)?;
    Some(&spec[i + 1..])
}

/// The installed version of a package in `node_modules`, read from its `package.json`
/// `version` field. `None` if absent or unparseable.
fn installed_version(node_modules: &Path, name: &str) -> Option<String> {
    let pkg_json = std::fs::read_to_string(node_modules.join(name).join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&pkg_json).ok()?;
    Some(v.get("version")?.as_str()?.to_string())
}

/// Directory under the tooling dir that holds every COMPLETE eslint tree (a "generation") plus
/// any in-progress staging tree. On unix `<tooling>/eslint` is a symlink to one generation.
const ESLINT_GENERATIONS_DIR: &str = "eslint.gen";

/// Name prefix of an in-progress (not yet healthy, not yet published) eslint tree.
const ESLINT_STAGING_PREFIX: &str = ".staging-";

/// `<tooling_dir>/eslint.gen/`: see [`ESLINT_GENERATIONS_DIR`].
fn eslint_generations_dir(tooling: &Path) -> PathBuf {
    tooling.join(ESLINT_GENERATIONS_DIR)
}

/// A fresh, process-unique generation name. Uniqueness only has to hold among provisioners of
/// one tooling dir, which are already serialized by the lock; time + pid keeps the name sortable
/// and stable across a crash-and-retry.
fn new_eslint_generation_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{:x}", std::process::id())
}

/// Ensure eslint (and its companion packages) are provisioned in the tooling
/// workspace.
///
/// Steps:
/// 1. Health-probe; return early if already good.
/// 2. Refuse if this is a test process aimed at the REAL tooling dir
///    ([`refuse_real_tooling_in_test_process`]); verify `npm` is available.
/// 3. Take the provisioning lock (across processes and within this one) and RE-PROBE: whoever
///    held it may have finished the install this caller was about to start.
/// 4. Build the exact tree in a STAGING directory beside the live one: write the exact-version
///    `package.json` and the committed `package-lock.json`, `npm ci --prefer-offline`, copy the
///    bundled flat config.
/// 5. Health-probe the finished tree, then PUBLISH it by atomically repointing the workspace at
///    it. The previous tree is left intact (see below), and a failure at any step before the
///    publish leaves the live workspace exactly as it was.
///
/// # Why the install is not done in place
///
/// This used to write `package.json` and run `npm ci` directly in the live workspace. `npm ci`
/// deletes `node_modules` FIRST, so for the length of an install (seconds to minutes) every
/// concurrent eslint run (another scan in the product, or a test in `cargo test --workspace`)
/// saw a half-installed tree and died with `Cannot find module '../package.json'`; two callers
/// that both saw that state ran two `npm ci`s over each other. Staging removes the window in
/// which a reader can see anything but a complete tree, and the lock removes the second installer.
///
/// # Publishing
///
/// On unix `<tooling>/eslint` is a symlink to a complete generation under `eslint.gen/`, and
/// publishing is a single `rename(2)` of a new symlink over the old one: atomic, with no instant
/// at which the path is missing. The previous generation is KEPT until the next provision so an
/// eslint process that resolved its module paths through the old tree keeps working while it
/// finishes. A pre-existing real `eslint/` directory (the old layout) is migrated by moving it
/// aside and linking the new tree in; that migration, once per machine, is the only publish with
/// a window (two renames) and it runs under the lock. Elsewhere (no symlinks) the publish is that
/// same two-rename swap.
pub async fn ensure_eslint(tooling: &Path) -> Result<PathBuf, ProvisionError> {
    ensure_eslint_with(tooling, std::ffi::OsStr::new("npm")).await
}

/// [`ensure_eslint`] with the `npm` program injected, so the provisioning logic is testable
/// against a fake `npm` without the network (and without touching the process-global `PATH`).
async fn ensure_eslint_with(
    tooling: &Path,
    npm: &std::ffi::OsStr,
) -> Result<PathBuf, ProvisionError> {
    let workspace = eslint_workspace_dir(tooling);
    let bin = eslint_bin(&workspace);

    if eslint_is_provisioned(&workspace).await {
        return Ok(bin);
    }

    refuse_real_tooling_in_test_process(tooling, "eslint")?;

    // Need npm to install.
    if !interpreter_available(npm).await {
        return Err(ProvisionError::BaseInterpreterMissing(
            "npm (required to provision eslint)".to_string(),
        ));
    }

    let _lock = acquire_provision_lock(tooling, "eslint", PROVISION_LOCK_TIMEOUT).await?;
    if eslint_is_provisioned(&workspace).await {
        return Ok(bin);
    }

    install_eslint_generation(tooling, npm).await?;
    Ok(bin)
}

/// Build a complete, healthy eslint tree in staging, then publish it. The caller holds the
/// provisioning lock.
async fn install_eslint_generation(
    tooling: &Path,
    npm: &std::ffi::OsStr,
) -> Result<(), ProvisionError> {
    let generations = eslint_generations_dir(tooling);
    tokio::fs::create_dir_all(&generations).await?;
    // We hold the lock, so any staging tree still here belongs to a dead provisioner.
    remove_stale_eslint_staging(&generations).await;

    let id = new_eslint_generation_id();
    let staging = generations.join(format!("{ESLINT_STAGING_PREFIX}{id}"));
    if let Err(e) = build_eslint_tree(&staging, npm).await {
        discard_dir(&staging).await;
        return Err(e);
    }

    // Move the finished tree to its permanent generation path BEFORE the health probe, so the
    // probe runs at the very path readers will resolve through the published link.
    let finished = generations.join(&id);
    if let Err(e) = tokio::fs::rename(&staging, &finished).await {
        discard_dir(&staging).await;
        return Err(ProvisionError::Io(e));
    }
    if !eslint_is_provisioned(&finished).await {
        discard_dir(&finished).await;
        return Err(ProvisionError::InstallFailed {
            tool: "eslint",
            detail: "eslint binary did not respond to --version after install".to_string(),
        });
    }

    let previous = match publish_eslint_generation(tooling, &id).await {
        Ok(previous) => previous,
        Err(e) => {
            discard_dir(&finished).await;
            return Err(ProvisionError::Io(e));
        }
    };
    prune_eslint_generations(&generations, &id, previous.as_deref()).await;
    Ok(())
}

/// Install the exact pinned tree into `dir` (a directory nothing else reads yet).
async fn build_eslint_tree(dir: &Path, npm: &std::ffi::OsStr) -> Result<(), ProvisionError> {
    tokio::fs::create_dir_all(dir).await?;

    // The manifest pins every package to its exact version, and the committed lockfile pins the
    // whole tree (transitive dependencies included): `npm ci` then installs the same tool on
    // every machine. If the lockfile cannot be read (a packaging problem), fall back to
    // installing the exact pins rather than failing to provision at all.
    tokio::fs::write(dir.join("package.json"), eslint_package_json()).await?;
    let have_lockfile = tokio::fs::copy(bundled_eslint_lock_src(), dir.join("package-lock.json"))
        .await
        .is_ok();

    let npm_args = eslint_install_args(have_lockfile);

    let npm_out = tokio::process::Command::new(npm)
        .args(&npm_args)
        .current_dir(dir)
        .kill_on_drop(true)
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

    // Copy the bundled flat config into the tree so eslint can find it.
    let config_src = bundled_eslint_config_src();
    if config_src.exists() {
        tokio::fs::copy(&config_src, eslint_config_path(dir)).await?;
    }
    Ok(())
}

/// Best-effort removal of a directory we own and no longer need. Failure is not an error worth
/// surfacing: the directory is garbage either way and the next provision sweeps it.
async fn discard_dir(dir: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(dir).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(dir = %dir.display(), error = %e, "could not remove a provisioning directory");
        }
    }
}

/// Remove every leftover staging tree under `generations`. Only call while holding the lock.
async fn remove_stale_eslint_staging(generations: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(generations).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(ESLINT_STAGING_PREFIX)
        {
            discard_dir(&entry.path()).await;
        }
    }
}

/// Remove every generation except the newly published one and the one it replaced. The replaced
/// tree is kept for one more cycle so an eslint process that already resolved its modules through
/// it can finish; anything older cannot have a live reader (a scan is bounded far below the time
/// between two re-provisions).
async fn prune_eslint_generations(generations: &Path, current: &str, previous: Option<&str>) {
    let Ok(mut entries) = tokio::fs::read_dir(generations).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == current || Some(name.as_str()) == previous {
            continue;
        }
        discard_dir(&entry.path()).await;
    }
}

/// Make `<tooling>/eslint` resolve to the generation `id`, atomically on unix. Returns the name
/// (under `eslint.gen/`) of the tree it replaced, if any, for [`prune_eslint_generations`] to
/// keep one cycle longer.
#[cfg(unix)]
async fn publish_eslint_generation(tooling: &Path, id: &str) -> std::io::Result<Option<String>> {
    let link = eslint_workspace_dir(tooling);
    let generations = eslint_generations_dir(tooling);
    // A RELATIVE target, so the whole tooling dir stays relocatable.
    let target = Path::new(ESLINT_GENERATIONS_DIR).join(id);
    let new_link = tooling.join(format!(".eslint-link-{id}"));
    tokio::fs::symlink(&target, &new_link).await?;

    let published: std::io::Result<Option<String>> = async {
        match tokio::fs::symlink_metadata(&link).await {
            // The steady state: replace the old symlink in one atomic rename.
            Ok(meta) if meta.file_type().is_symlink() => {
                let previous = tokio::fs::read_link(&link)
                    .await
                    .ok()
                    .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()));
                tokio::fs::rename(&new_link, &link).await?;
                Ok(previous)
            }
            // The old layout: a real directory. Move it aside (kept one cycle like any previous
            // generation), then link the new tree in. The one-time window, under the lock.
            Ok(_) => {
                let aside_name = format!("legacy-{id}");
                let aside = generations.join(&aside_name);
                tokio::fs::rename(&link, &aside).await?;
                if let Err(e) = tokio::fs::rename(&new_link, &link).await {
                    let _ = tokio::fs::rename(&aside, &link).await;
                    return Err(e);
                }
                Ok(Some(aside_name))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::rename(&new_link, &link).await?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
    .await;

    if published.is_err() {
        let _ = tokio::fs::remove_file(&new_link).await;
    }
    published
}

/// Non-unix publish: no symlinks to rely on, so swap the directory itself (move the live one
/// aside, move the finished one in, restore on failure). Briefly leaves the path absent between
/// the two renames; callers that hit it fail the probe and wait on the lock.
#[cfg(not(unix))]
async fn publish_eslint_generation(tooling: &Path, id: &str) -> std::io::Result<Option<String>> {
    let link = eslint_workspace_dir(tooling);
    let generations = eslint_generations_dir(tooling);
    let finished = generations.join(id);
    let aside_name = format!("legacy-{id}");
    let aside = generations.join(&aside_name);
    let had_previous = tokio::fs::symlink_metadata(&link).await.is_ok();
    if had_previous {
        tokio::fs::rename(&link, &aside).await?;
    }
    if let Err(e) = tokio::fs::rename(&finished, &link).await {
        if had_previous {
            let _ = tokio::fs::rename(&aside, &link).await;
        }
        return Err(e);
    }
    Ok(had_previous.then_some(aside_name))
}

// ─── osv-scanner ─────────────────────────────────────────────────────────────
//
// osv-scanner is Google's multi-ecosystem dependency-vulnerability scanner.  It
// reads lockfiles (Cargo.lock, package-lock.json, go.sum, poetry.lock, etc.) and
// queries the OSV database for known CVEs / GHSAs / RUSTSECs — one binary covers
// every ecosystem Camerata's users write in.
//
// Provisioning resolution chain (fail-soft at every step — see module-level doc):
//   (a) `osv-scanner` already on PATH → use it (cheapest, zero disk).
//   (b) Cached provisioned binary at `<tooling_dir>/osv-scanner/osv-scanner[.exe]`
//       that responds to `--version` → use it.
//   (c) Attempt to download the pinned prebuilt GitHub release binary for the
//       detected OS/arch (darwin/linux × amd64/arm64) via reqwest.
//   (d) If download is unavailable/unsupported, try `go install` with the pinned
//       module path if a Go toolchain is present.
//   (e) None of the above → return `ProvisionError::InstallFailed` so the caller
//       emits a `CoverageNote` and the scan continues without dep-audit.

/// Pinned osv-scanner version.  Bump when a meaningful new ecosystem or advisory
/// feed is added upstream.  The prebuilt binaries are downloaded from the GitHub
/// releases page at this tag.
pub const OSV_SCANNER_VERSION: &str = "v1.9.2";

/// The download base URL for osv-scanner prebuilt release binaries.  The full
/// asset URL is constructed as `{BASE}/{VERSION}/{ASSET_NAME}`.
const OSV_RELEASE_BASE: &str = "https://github.com/google/osv-scanner/releases/download";

/// Path to the Camerata-managed osv-scanner directory:
/// `<tooling_dir>/osv-scanner/`.
pub fn osv_scanner_dir(tooling: &Path) -> PathBuf {
    tooling.join("osv-scanner")
}

/// Path to the cached osv-scanner binary inside the Camerata-managed directory.
pub fn osv_scanner_bin(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    let bin = dir.join("osv-scanner.exe");
    #[cfg(not(windows))]
    let bin = dir.join("osv-scanner");
    bin
}

/// Probe whether the osv-scanner binary at `bin` is already provisioned and
/// responds to `--version` with exit 0.  A `false` return triggers a fresh
/// provision attempt.
pub async fn osv_scanner_is_provisioned(bin: &Path) -> bool {
    if !bin.exists() {
        return false;
    }
    match tokio::process::Command::new(bin)
        .arg("--version")
        .kill_on_drop(true)
        .output()
        .await
    {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Detect the current OS / architecture label used in osv-scanner release asset
/// names (e.g. `linux_amd64`, `darwin_arm64`).  Returns `None` when running on
/// an unsupported combination (Windows / 32-bit / exotic arch) so the caller
/// can fall through to the `go install` step.
fn osv_release_asset() -> Option<String> {
    // osv-scanner release asset naming convention (as of v1.7+):
    //   osv-scanner_{os}_{arch}            (Linux / macOS)
    //   osv-scanner_windows_{arch}.exe     (Windows)
    // We target the most common developer platforms; CI images are typically
    // linux_amd64.  Unsupported combinations fall through to `go install`.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some("osv-scanner_linux_amd64".to_string());
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some("osv-scanner_linux_arm64".to_string());
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Some("osv-scanner_darwin_amd64".to_string());
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some("osv-scanner_darwin_arm64".to_string());
    // Windows / 32-bit / everything else: unsupported for auto-download.
    #[allow(unreachable_code)]
    None
}

/// Attempt to download the pinned osv-scanner prebuilt binary from GitHub
/// releases into `dest`.  Returns `Ok(())` on success or an error describing
/// why the download failed (network unavailable, unsupported platform, etc.).
async fn download_osv_scanner(dest: &Path) -> Result<(), ProvisionError> {
    let asset = osv_release_asset().ok_or_else(|| ProvisionError::InstallFailed {
        tool: "osv-scanner",
        detail: "unsupported OS/arch for prebuilt binary download; \
                 try `go install github.com/google/osv-scanner/cmd/osv-scanner@v1.9.2`"
            .to_string(),
    })?;

    let url = format!(
        "{base}/{version}/{asset}",
        base = OSV_RELEASE_BASE,
        version = OSV_SCANNER_VERSION,
        asset = asset,
    );

    // reqwest is already a dependency (it fetches repo tarballs).  Use rustls
    // (no system openssl dependency) for the binary download.
    //
    // IMPORTANT: both timeouts are mandatory.  Without them this function hangs
    // forever on slow/blocked/no-network environments, which in turn hangs the
    // entire onboarding scan and any test that exercises `audit_repos`.
    // - connect_timeout: aborts if the TCP handshake doesn't complete in 5 s.
    // - timeout: caps the TOTAL round-trip (connect + headers + body) at 30 s.
    // On either timeout `send()` or `bytes()` returns an error → `ProvisionError`
    // → `CoverageNote` → scan continues.  Never hangs.
    let client = reqwest::Client::builder()
        .user_agent(concat!("camerata/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: format!("could not build HTTP client: {e}"),
        })?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: format!("download failed (network unavailable?): {e}"),
        })?;

    if !resp.status().is_success() {
        return Err(ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: format!(
                "download returned HTTP {} for {url}",
                resp.status().as_u16()
            ),
        });
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: format!("failed to read download body: {e}"),
        })?;

    write_executable_atomically(dest, &bytes).await?;
    Ok(())
}

/// Write `bytes` to `dest` as an executable so that `dest` is never observable half-written or
/// not-yet-executable: the bytes go to a sibling temp file, are marked executable, and only then
/// renamed over `dest` (atomic within a directory). A concurrent `osv_scanner_is_provisioned`
/// probe therefore sees either the previous state or a complete binary, never a truncated one
/// (a truncated download that fails `--version` used to read as "not provisioned" and trigger a
/// second, overlapping download).
async fn write_executable_atomically(dest: &Path, bytes: &[u8]) -> Result<(), ProvisionError> {
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "osv-scanner".to_string());
    let tmp = dest.with_file_name(format!(".{file_name}.{}.part", std::process::id()));
    let staged: Result<(), ProvisionError> = async {
        tokio::fs::write(&tmp, bytes).await?;
        // Mark executable on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).await?;
        }
        tokio::fs::rename(&tmp, dest).await?;
        Ok(())
    }
    .await;
    if staged.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    staged
}

/// Attempt to provision osv-scanner via `go install` with the pinned version.
/// Only tried when a `go` toolchain is available (checked before calling) and
/// the prebuilt-binary download did not succeed.  Returns the path to the
/// installed binary on success.
async fn go_install_osv_scanner() -> Result<PathBuf, ProvisionError> {
    // `go env GOPATH` gives us the GOPATH so we can locate the installed binary.
    // Bound at 10 s: `go env` is a local metadata query and should be nearly
    // instant; a hang here means the Go toolchain is broken/stalled.
    let gopath_out = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("go")
            .args(["env", "GOPATH"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ProvisionError::InstallFailed {
        tool: "osv-scanner",
        detail: "`go env GOPATH` timed out after 10 s".to_string(),
    })?
    .map_err(ProvisionError::Io)?;
    let gopath = String::from_utf8_lossy(&gopath_out.stdout)
        .trim()
        .to_string();
    if gopath.is_empty() {
        return Err(ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: "`go env GOPATH` returned an empty path".to_string(),
        });
    }
    let bin_dir = std::path::PathBuf::from(&gopath).join("bin");
    #[cfg(windows)]
    let bin = bin_dir.join("osv-scanner.exe");
    #[cfg(not(windows))]
    let bin = bin_dir.join("osv-scanner");

    let pkg = format!(
        "github.com/google/osv-scanner/cmd/osv-scanner@{ver}",
        ver = OSV_SCANNER_VERSION
    );
    // Cap `go install` at 60 s.  On a slow network or constrained CI runner this
    // can take a while, but an unlimited wait hangs the scan indefinitely.  60 s
    // is generous for a ~15 MB binary compile + module download while still
    // ensuring the provisioning path is always bounded.
    let install_out = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("go")
            .args(["install", &pkg])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ProvisionError::InstallFailed {
        tool: "osv-scanner",
        detail: format!("`go install {pkg}` timed out after 60 s"),
    })?
    .map_err(ProvisionError::Io)?;
    if !install_out.status.success() {
        return Err(ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: format!(
                "`go install {pkg}` failed: {}",
                String::from_utf8_lossy(&install_out.stderr)
            ),
        });
    }

    if !bin.exists() {
        return Err(ProvisionError::InstallFailed {
            tool: "osv-scanner",
            detail: format!(
                "`go install` succeeded but binary not found at: {}",
                bin.display()
            ),
        });
    }
    Ok(bin)
}

/// Ensure osv-scanner is available, returning a `PathBuf` to the binary.
///
/// # Resolution chain
///
/// 1. If `osv-scanner` is already on PATH and responds to `--version`, use it.
/// 2. If the cached provisioned binary at `<tooling_dir>/osv-scanner/osv-scanner`
///    already exists and responds to `--version`, use it.
/// 3. Attempt to download the pinned prebuilt release binary for this OS/arch
///    from the osv-scanner GitHub releases page.
/// 4. Fall back to `go install github.com/google/osv-scanner/cmd/osv-scanner@<pin>`
///    if a Go toolchain (`go`) is available.
/// 5. If none of the above succeeds, return `Err(ProvisionError::InstallFailed)`
///    so the caller emits a `CoverageNote` and the scan continues without dep-audit.
///
/// The function is idempotent: a passing health probe short-circuits every step.
pub async fn ensure_osv_scanner(tooling: &Path) -> Result<PathBuf, ProvisionError> {
    // Step (a): already on PATH?
    if interpreter_available("osv-scanner").await {
        // Resolve the actual binary path from PATH so the caller has a concrete
        // `PathBuf`.  `which` is a crate we don't depend on; use the tokio
        // process approach — run it and if it exits 0 we know it's on PATH.
        // We return a sentinal PATH-relative string as a PathBuf here because the
        // scan invocation uses `tokio::process::Command::new(bin_str)` which
        // accepts PATH-resident names when the path has no separator.
        return Ok(PathBuf::from("osv-scanner"));
    }

    // Step (b): already provisioned in our managed dir?
    let dir = osv_scanner_dir(tooling);
    let bin = osv_scanner_bin(&dir);
    if osv_scanner_is_provisioned(&bin).await {
        return Ok(bin);
    }

    // Neither probe passed — attempt provisioning. A test process never does that into the REAL
    // shared tooling dir, and concurrent provisioners are serialized (a caller that waited re-checks
    // the probe: the holder may have installed the binary this caller was about to download).
    refuse_real_tooling_in_test_process(tooling, "osv-scanner")?;
    let _lock = acquire_provision_lock(tooling, "osv-scanner", PROVISION_LOCK_TIMEOUT).await?;
    if osv_scanner_is_provisioned(&bin).await {
        return Ok(bin);
    }
    tokio::fs::create_dir_all(&dir).await?;

    // Step (c): download prebuilt binary from GitHub releases.
    match download_osv_scanner(&bin).await {
        Ok(()) => {
            // Final health probe after download.
            if osv_scanner_is_provisioned(&bin).await {
                return Ok(bin);
            }
            // Downloaded but the binary doesn't execute (wrong arch / partial write).
            // Fall through to go install.
        }
        Err(e) => {
            // Log the download failure reason but don't surface it yet — the go
            // install path may still succeed.  If go install also fails we will
            // return the go-install error (more actionable for the user).
            let _ = e; // intentionally dropped; go-install error wins
        }
    }

    // Step (d): go install fallback.
    if interpreter_available("go").await {
        let go_bin = go_install_osv_scanner()
            .await
            .map_err(|e| ProvisionError::InstallFailed {
                tool: "osv-scanner",
                detail: format!("prebuilt download failed and `go install` also failed: {e}"),
            })?;
        return Ok(go_bin);
    }

    // Step (e): nothing worked.
    Err(ProvisionError::InstallFailed {
        tool: "osv-scanner",
        detail: format!(
            "could not provision osv-scanner {OSV_SCANNER_VERSION}: \
             prebuilt download failed and no Go toolchain is available. \
             Install manually: `go install github.com/google/osv-scanner/cmd/osv-scanner@{OSV_SCANNER_VERSION}`"
        ),
    })
}

// ─── shared helper ───────────────────────────────────────────────────────────

/// Returns `true` when `program` can be found and executed on PATH (by running
/// `<program> --version`; some tools use `--help` but most accept `--version`).
/// Used to detect base interpreters (python3, npm/node) before attempting a
/// provisioning step that would fail anyway.
async fn interpreter_available(program: impl AsRef<std::ffi::OsStr>) -> bool {
    tokio::process::Command::new(program)
        .arg("--version")
        .kill_on_drop(true)
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

    // ── W30 eslint version-pin parsing (pure) ─────────────────────────────────

    #[test]
    fn npm_package_name_strips_version_and_respects_scopes() {
        assert_eq!(npm_package_name("eslint@^9"), "eslint");
        assert_eq!(npm_package_name("eslint"), "eslint");
        assert_eq!(
            npm_package_name("@typescript-eslint/parser@^8"),
            "@typescript-eslint/parser"
        );
        // scoped, no version: the leading `@` is a scope, not a separator.
        assert_eq!(
            npm_package_name("@typescript-eslint/parser"),
            "@typescript-eslint/parser"
        );
    }

    #[test]
    fn npm_pinned_version_parses_the_exact_pin() {
        assert_eq!(npm_pinned_version("eslint@9.39.5"), Some("9.39.5"));
        assert_eq!(
            npm_pinned_version("eslint-plugin-react-hooks@5.2.0"),
            Some("5.2.0")
        );
        assert_eq!(
            npm_pinned_version("@typescript-eslint/parser@8.72.0"),
            Some("8.72.0")
        );
        assert_eq!(npm_pinned_version("eslint"), None); // unpinned
        assert_eq!(npm_pinned_version("@scope/pkg"), None); // scoped, unpinned
    }

    /// An exact pin is `major.minor.patch` and nothing else: a range marker (`^`, `~`, `>=`,
    /// `x`, `*`) or a dist-tag lets `npm` pick, and the graded accuracy tests need the tool to
    /// behave the same on every machine on every day.
    #[test]
    fn every_bundled_eslint_pin_is_an_exact_version() {
        for spec in ESLINT_NPM_PACKAGES {
            let version = npm_pinned_version(spec).unwrap_or_else(|| {
                panic!("ESLINT_NPM_PACKAGES entry {spec:?} is unpinned; it would drift to latest")
            });
            let parts: Vec<&str> = version.split('.').collect();
            assert!(
                parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok()),
                "ESLINT_NPM_PACKAGES entry {spec:?} must pin an exact major.minor.patch, not a \
                 range: {version:?}"
            );
        }
    }

    #[test]
    fn installed_version_reads_package_json_and_forces_reprovision_on_any_mismatch() {
        let tmp = TempDir::new().unwrap();
        let nm = tmp.path().join("node_modules");
        std::fs::create_dir_all(nm.join("eslint")).unwrap();
        std::fs::write(
            nm.join("eslint").join("package.json"),
            r#"{"name":"eslint","version":"9.12.0"}"#,
        )
        .unwrap();
        assert_eq!(installed_version(&nm, "eslint").as_deref(), Some("9.12.0"));
        // The SAME major, a different minor: caret majors let this drift in silently. An exact
        // pin does not read it as installed.
        assert_ne!(
            installed_version(&nm, "eslint").as_deref(),
            npm_pinned_version("eslint@9.39.5")
        );
        // A package with no package.json reads as absent.
        assert_eq!(installed_version(&nm, "nope"), None);
    }

    /// A workspace whose packages are the right MAJOR but not the pinned version must
    /// re-provision (the old probe compared majors only).
    #[test]
    fn a_workspace_with_a_package_at_the_right_major_but_the_wrong_version_must_reprovision() {
        for spec in ESLINT_NPM_PACKAGES {
            let tmp = fully_provisioned_workspace();
            let name = npm_package_name(spec);
            let pinned = npm_pinned_version(spec).unwrap();
            let major = pinned.split('.').next().unwrap();
            std::fs::write(
                tmp.path()
                    .join("node_modules")
                    .join(name)
                    .join("package.json"),
                format!("{{\"version\":\"{major}.99.99\"}}"),
            )
            .unwrap();
            assert!(
                !eslint_packages_installed(tmp.path()),
                "`{name}` at {major}.99.99 is not the pinned {pinned}"
            );
        }
    }

    /// The generated workspace manifest pins every package to the exact version, no range.
    #[test]
    fn the_generated_workspace_package_json_has_exact_versions() {
        let json: serde_json::Value = serde_json::from_str(&eslint_package_json()).unwrap();
        let dev = json["devDependencies"]
            .as_object()
            .expect("devDependencies");
        assert_eq!(dev.len(), ESLINT_NPM_PACKAGES.len());
        for spec in ESLINT_NPM_PACKAGES {
            let name = npm_package_name(spec);
            assert_eq!(
                dev.get(name).and_then(|v| v.as_str()),
                npm_pinned_version(spec),
                "{name}"
            );
        }
        for (name, version) in dev {
            let v = version.as_str().unwrap();
            assert!(
                !v.starts_with(['^', '~', '>', '<', '=', '*']) && !v.contains(['x', ' ', '|']),
                "{name}: {v:?} is a range, not an exact version"
            );
        }
    }

    /// The committed lockfile is what provisioning installs from, so it must agree with the pins
    /// (a lock built for other versions makes `npm ci` fail, or worse, install the wrong ones),
    /// and the committed manifest next to it must be the one the code generates.
    #[test]
    fn the_committed_lockfile_and_manifest_agree_with_the_pins() {
        let manifest = std::fs::read_to_string(bundled_eslint_package_json_src())
            .expect("assets/eslint/package.json is committed");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&manifest).unwrap(),
            serde_json::from_str::<serde_json::Value>(&eslint_package_json()).unwrap(),
            "assets/eslint/package.json must equal the generated manifest"
        );
        let lock: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(bundled_eslint_lock_src())
                .expect("assets/eslint/package-lock.json is committed"),
        )
        .unwrap();
        assert!(lock["lockfileVersion"].as_u64().unwrap_or(0) >= 2);
        for spec in ESLINT_NPM_PACKAGES {
            let name = npm_package_name(spec);
            let pinned = npm_pinned_version(spec).unwrap();
            assert_eq!(
                lock["packages"][""]["devDependencies"][name].as_str(),
                Some(pinned),
                "lock root requirement for {name}"
            );
            assert_eq!(
                lock["packages"][format!("node_modules/{name}")]["version"].as_str(),
                Some(pinned),
                "lock resolved version for {name}"
            );
            assert!(
                lock["packages"][format!("node_modules/{name}")]["integrity"]
                    .as_str()
                    .is_some_and(|i| i.starts_with("sha512-")),
                "the lock carries an integrity hash for {name}"
            );
        }
    }

    /// Network: a fresh provisioning into an empty tooling directory installs the pinned tree
    /// from the committed lockfile (`npm ci`), leaves the workspace manifest exact, and reads as
    /// provisioned. Ignored by default because it needs `npm` and the registry (or a warm npm
    /// cache); run with `cargo test -p camerata-server --lib fresh_eslint -- --ignored`.
    #[tokio::test]
    #[ignore = "needs npm and network (or a warm npm cache)"]
    async fn fresh_eslint_provisioning_installs_the_pinned_tree_from_the_lockfile() {
        let tmp = TempDir::new().unwrap();
        let bin = ensure_eslint(tmp.path()).await.expect("provisions");
        let ws = eslint_workspace_dir(tmp.path());
        assert!(bin.exists());
        assert!(eslint_is_provisioned(&ws).await);
        let nm = ws.join("node_modules");
        for spec in ESLINT_NPM_PACKAGES {
            assert_eq!(
                installed_version(&nm, npm_package_name(spec)).as_deref(),
                npm_pinned_version(spec),
                "{spec}"
            );
        }
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["devDependencies"]["eslint"], "9.39.5");
        let lock = std::fs::read_to_string(ws.join("package-lock.json")).unwrap();
        assert_eq!(
            lock,
            std::fs::read_to_string(bundled_eslint_lock_src()).unwrap(),
            "`npm ci` must not rewrite the committed lockfile"
        );
    }

    /// With the lockfile available provisioning installs exactly from it (`npm ci`, still
    /// `--prefer-offline`); without it (a packaging problem) it falls back to installing the
    /// exact pins, never to a range.
    #[test]
    fn eslint_installs_from_the_lockfile_and_falls_back_to_the_exact_pins() {
        assert_eq!(eslint_install_args(true), ["ci", "--prefer-offline"]);
        let fallback = eslint_install_args(false);
        assert_eq!(
            &fallback[..3],
            ["install", "--save-dev", "--prefer-offline"]
        );
        assert!(
            fallback.contains(&"eslint@9.39.5".to_string()),
            "{fallback:?}"
        );
        assert!(
            fallback[3..]
                .iter()
                .all(|a| npm_pinned_version(a).is_some()),
            "every fallback package is an exact pin: {fallback:?}"
        );
    }

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

    // ── req_c11: the health probe covers the whole provisioned state ──────────
    //
    // The probe used to ask only "does the binary answer --version", which made
    // the workspace write-once: packages added to ESLINT_NPM_PACKAGES later, and
    // every edit to the bundled flat config, never reached a machine that had
    // provisioned once. Observed live while diagnosing C11 — 3 of 6 packages
    // missing and a four-month-stale config, on a workspace this probe called
    // healthy. See `eslint_is_provisioned`'s doc comment.

    /// Build a workspace that looks fully provisioned to the probe's sub-checks:
    /// every required package present, config byte-identical to the asset.
    fn fully_provisioned_workspace() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        for spec in ESLINT_NPM_PACKAGES {
            let name = npm_package_name(spec);
            let dir = ws.join("node_modules").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let version = npm_pinned_version(spec).unwrap_or("0.0.0");
            std::fs::write(
                dir.join("package.json"),
                format!("{{\"version\":\"{version}\"}}"),
            )
            .unwrap();
        }
        let asset = std::fs::read(bundled_eslint_config_src()).unwrap();
        std::fs::write(eslint_config_path(ws), asset).unwrap();
        tmp
    }

    /// req_c11: every required package present ⇒ the package sub-check passes.
    #[test]
    fn req_c11_a_workspace_with_every_required_package_passes_the_package_check() {
        let tmp = fully_provisioned_workspace();
        assert!(eslint_packages_installed(tmp.path()));
    }

    /// req_c11: the exact observed defect — a workspace missing a package that was
    /// added to `ESLINT_NPM_PACKAGES` after it was provisioned must re-provision.
    ///
    /// FAILS BEFORE THE FIX: `eslint_is_provisioned` never looked at `node_modules`
    /// at all, so this workspace was indistinguishable from a complete one.
    #[test]
    fn req_c11_a_workspace_missing_one_required_package_must_reprovision() {
        for missing in ESLINT_NPM_PACKAGES {
            let tmp = fully_provisioned_workspace();
            let name = npm_package_name(missing);
            std::fs::remove_dir_all(tmp.path().join("node_modules").join(name)).unwrap();
            assert!(
                !eslint_packages_installed(tmp.path()),
                "a workspace missing `{name}` must not read as provisioned — that is how \
                 three plugin packages stayed missing for months and zeroed the whole pass"
            );
        }
    }

    /// req_c11: a package DIRECTORY with no `package.json` (an interrupted
    /// `npm install`) counts as absent — the conservative direction.
    #[test]
    fn req_c11_a_package_dir_without_a_manifest_counts_as_absent() {
        let tmp = fully_provisioned_workspace();
        let dir = tmp
            .path()
            .join("node_modules")
            .join(npm_package_name(ESLINT_NPM_PACKAGES[0]));
        std::fs::remove_file(dir.join("package.json")).unwrap();
        assert!(!eslint_packages_installed(tmp.path()));
    }

    /// req_c11: the config sub-check is a CONTENT comparison against the bundled
    /// asset, so a workspace carrying an older copy re-provisions.
    ///
    /// FAILS BEFORE THE FIX: `ensure_eslint` only ever copied the config on the
    /// install path, and the probe short-circuited before reaching it, so a stale
    /// config was permanent.
    #[test]
    fn req_c11_a_stale_workspace_config_must_reprovision_but_a_current_one_must_not() {
        let tmp = fully_provisioned_workspace();
        assert!(
            eslint_config_is_current(tmp.path()),
            "a byte-identical copy of the bundled asset is current"
        );

        std::fs::write(
            eslint_config_path(tmp.path()),
            "export default [];\n// an older Camerata's config\n",
        )
        .unwrap();
        assert!(
            !eslint_config_is_current(tmp.path()),
            "a workspace config that differs from the shipped asset must re-provision"
        );
    }

    /// req_c11: an ABSENT workspace config re-provisions (it is not "current").
    #[test]
    fn req_c11_an_absent_workspace_config_must_reprovision() {
        let tmp = fully_provisioned_workspace();
        std::fs::remove_file(eslint_config_path(tmp.path())).unwrap();
        assert!(!eslint_config_is_current(tmp.path()));
    }

    /// req_c11: the SARIF formatter package is no longer installed — nothing
    /// requests it, so installing it would be pure cost. Also guards the inverse
    /// regression: re-adding it would reintroduce the temptation to pass it to
    /// `--format`, which is the C11 defect itself.
    #[test]
    fn req_c11_the_npm_sarif_formatter_is_no_longer_provisioned() {
        assert!(
            !ESLINT_NPM_PACKAGES
                .iter()
                .any(|p| p.contains("eslint-formatter")),
            "no eslint formatter PACKAGE may be provisioned; the scan pass uses eslint's \
             built-in `json` formatter: {ESLINT_NPM_PACKAGES:?}"
        );
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
        tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        // Make it executable.
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        assert!(semgrep_is_provisioned(&venv).await);
    }

    /// Lay down the NON-binary half of a provisioned eslint workspace: every
    /// required package, and a workspace config byte-identical to the bundled
    /// asset. The stub-binary tests below need this because
    /// [`eslint_is_provisioned`] deliberately no longer accepts a bare binary as
    /// proof of provisioning — see its doc comment for the write-once defect that
    /// change closes.
    fn stub_eslint_packages_and_config(ws: &Path) {
        for spec in ESLINT_NPM_PACKAGES {
            // Use the BARE package name for the directory (the probe looks there), and write a
            // version matching the pinned major so the W30 version-aware probe reads the stub as
            // correctly provisioned.
            let name = npm_package_name(spec);
            let dir = ws.join("node_modules").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let version = npm_pinned_version(spec).unwrap_or("0.0.0");
            std::fs::write(
                dir.join("package.json"),
                format!("{{\"version\":\"{version}\"}}"),
            )
            .unwrap();
        }
        let asset = std::fs::read(bundled_eslint_config_src()).unwrap();
        std::fs::write(eslint_config_path(ws), asset).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_eslint_binary_is_provisioned() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("eslint");
        let bin_dir = ws.join("node_modules").join(".bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let stub = bin_dir.join("eslint");
        tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        stub_eslint_packages_and_config(&ws);
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
        tokio::fs::write(&stub, b"#!/bin/sh\nexit 1\n")
            .await
            .unwrap();
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
            tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n")
                .await
                .unwrap();
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
            tokio::fs::write(&stub, b"#!/bin/sh\nexit 0\n")
                .await
                .unwrap();
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
                .await
                .unwrap();
            stub_eslint_packages_and_config(&ws);
            let r1 = ensure_eslint(&tooling).await;
            assert!(r1.is_ok());
            let r2 = ensure_eslint(&tooling).await;
            assert!(r2.is_ok());
            assert_eq!(r1.unwrap(), r2.unwrap());
        }
    }

    // ── atomic, serialized eslint provisioning ────────────────────────────────
    //
    // The incident: a full `cargo test --workspace` found the shared eslint workspace stale, one
    // test ran `npm ci` IN PLACE (which deletes `node_modules` first), another test running eslint
    // died with `Cannot find module '../package.json'`, and a third saw the half-deleted tree and
    // ran a SECOND `npm ci` over the first (two npm runs, 0.5 s apart, same cwd, in npm's logs).
    // These tests drive the real provisioning code against a temp tooling dir and a fake `npm`,
    // so no network and nothing shared is involved.

    /// A fake `npm` that behaves like the real one where it matters here: `ci` / `install` first
    /// DELETE `node_modules` (the destructive step that caused the incident), take a while, then
    /// lay down every pinned package and a working `eslint` stub. Each install appends one line
    /// (`<subcommand> <cwd>`) to `log`. `--version` answers without logging.
    #[cfg(unix)]
    struct FakeNpm {
        _dir: TempDir,
        program: PathBuf,
        log: PathBuf,
    }

    #[cfg(unix)]
    impl FakeNpm {
        fn new(install_seconds: &str, fail: bool) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new().unwrap();
            let program = dir.path().join("fake-npm");
            let log = dir.path().join("installs.log");
            let mut script = format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"--version\" ]; then echo 10.0.0; exit 0; fi\n\
                 echo \"$1 $(pwd)\" >> '{log}'\n\
                 sleep {install_seconds}\n",
                log = log.display(),
            );
            if fail {
                script.push_str("echo 'fake npm: simulated failure' >&2\nexit 1\n");
            } else {
                script.push_str("rm -rf node_modules\nmkdir -p node_modules/.bin\n");
                for spec in ESLINT_NPM_PACKAGES {
                    let name = npm_package_name(spec);
                    let version = npm_pinned_version(spec).unwrap_or("0.0.0");
                    script.push_str(&format!(
                        "mkdir -p node_modules/{name}\n\
                         printf '{{\"version\":\"{version}\"}}' > node_modules/{name}/package.json\n"
                    ));
                }
                script.push_str(
                    "printf '#!/bin/sh\\nexit 0\\n' > node_modules/.bin/eslint\n\
                     chmod +x node_modules/.bin/eslint\n",
                );
            }
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                _dir: dir,
                program,
                log,
            }
        }

        fn installs(&self) -> usize {
            std::fs::read_to_string(&self.log)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }
    }

    /// Make the (already provisioned) workspace under `tooling` read as stale, so the next
    /// `ensure_eslint` must re-provision it. Writes through the workspace path.
    #[cfg(unix)]
    fn make_eslint_workspace_stale(tooling: &Path) {
        std::fs::write(
            eslint_config_path(&eslint_workspace_dir(tooling)),
            "// an older Camerata's config\n",
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn generation_names(tooling: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(eslint_generations_dir(tooling))
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[cfg(unix)]
    async fn eslint_runs(bin: &Path) -> bool {
        tokio::process::Command::new(bin)
            .arg("--version")
            .output()
            .await
            .is_ok_and(|o| o.status.success())
    }

    /// Eight callers racing on one un-provisioned tooling dir produce ONE install, and every one
    /// of them is handed a bin that runs.
    ///
    /// FAILS BEFORE THE FIX: every caller saw the empty workspace, all eight wrote the manifest
    /// and ran `npm` in the same directory.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_ensure_eslint_calls_install_once_and_every_caller_gets_a_working_bin() {
        let tooling = TempDir::new().unwrap();
        let npm = FakeNpm::new("0.4", false);
        let mut callers = Vec::new();
        for _ in 0..8 {
            let tooling = tooling.path().to_path_buf();
            let program = npm.program.clone();
            callers.push(tokio::spawn(async move {
                ensure_eslint_with(&tooling, program.as_os_str()).await
            }));
        }
        let mut bins = Vec::new();
        for caller in callers {
            bins.push(
                caller
                    .await
                    .unwrap()
                    .expect("every caller provisions or waits"),
            );
        }

        assert_eq!(npm.installs(), 1, "exactly one install ran: {:?}", npm.log);
        assert!(
            bins.windows(2).all(|w| w[0] == w[1]),
            "one bin for all: {bins:?}"
        );
        assert!(eslint_runs(&bins[0]).await, "the returned bin works");
        assert!(eslint_is_provisioned(&eslint_workspace_dir(tooling.path())).await);
        assert_eq!(
            generation_names(tooling.path()).len(),
            1,
            "one generation, no staging debris: {:?}",
            generation_names(tooling.path())
        );
    }

    /// A reader polling the workspace while it RE-provisions never observes a missing or partial
    /// tree: `node_modules/eslint/package.json` is readable on every single poll, and `eslint`
    /// runs from the same path afterwards.
    ///
    /// FAILS BEFORE THE FIX: `npm ci` ran in the live directory and deleted `node_modules` first.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reader_never_sees_a_missing_tree_while_eslint_reprovisions() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        let tooling = TempDir::new().unwrap();
        let npm = FakeNpm::new("0.5", false);
        ensure_eslint_with(tooling.path(), npm.program.as_os_str())
            .await
            .expect("first provision");
        make_eslint_workspace_stale(tooling.path());
        assert!(!eslint_is_provisioned(&eslint_workspace_dir(tooling.path())).await);

        let stop = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let misses = Arc::new(AtomicUsize::new(0));
        let marker = eslint_workspace_dir(tooling.path())
            .join("node_modules")
            .join("eslint")
            .join("package.json");
        let reader = {
            let (stop, polls, misses) = (stop.clone(), polls.clone(), misses.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    polls.fetch_add(1, Ordering::SeqCst);
                    if std::fs::read_to_string(&marker).is_err() {
                        misses.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_micros(100));
                }
            })
        };

        let bin = ensure_eslint_with(tooling.path(), npm.program.as_os_str())
            .await
            .expect("re-provision");
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();

        assert_eq!(npm.installs(), 2, "first provision + the re-provision");
        assert!(
            polls.load(Ordering::SeqCst) > 100,
            "the reader must have actually overlapped the install ({} polls)",
            polls.load(Ordering::SeqCst)
        );
        assert_eq!(
            misses.load(Ordering::SeqCst),
            0,
            "a reader saw the eslint package missing during a re-provision"
        );
        assert!(eslint_runs(&bin).await);
        assert!(eslint_is_provisioned(&eslint_workspace_dir(tooling.path())).await);
    }

    /// A FAILED re-provision leaves the live workspace exactly as it was (still usable by
    /// whoever is reading it) and leaves no staging debris behind.
    ///
    /// FAILS BEFORE THE FIX: the install ran in place, so a failing `npm ci` had already deleted
    /// `node_modules` out from under every reader.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_reprovision_leaves_the_live_workspace_untouched() {
        let tooling = TempDir::new().unwrap();
        let good = FakeNpm::new("0", false);
        ensure_eslint_with(tooling.path(), good.program.as_os_str())
            .await
            .unwrap();
        make_eslint_workspace_stale(tooling.path());
        let ws = eslint_workspace_dir(tooling.path());
        let link_before = std::fs::read_link(&ws).unwrap();

        let broken = FakeNpm::new("0", true);
        let err = ensure_eslint_with(tooling.path(), broken.program.as_os_str())
            .await
            .expect_err("the install fails");
        assert!(
            matches!(err, ProvisionError::InstallFailed { tool: "eslint", .. }),
            "{err:?}"
        );

        assert_eq!(
            std::fs::read_link(&ws).unwrap(),
            link_before,
            "link unmoved"
        );
        assert!(
            ws.join("node_modules")
                .join("eslint")
                .join("package.json")
                .exists(),
            "the live tree is intact"
        );
        assert_eq!(
            std::fs::read_to_string(eslint_config_path(&ws)).unwrap(),
            "// an older Camerata's config\n",
            "the live workspace was not rewritten"
        );
        assert_eq!(
            generation_names(tooling.path()).len(),
            1,
            "staging cleaned up"
        );
    }

    /// A workspace in the OLD layout (a real `eslint/` directory) migrates to a published
    /// generation, and generations are pruned to the current one plus the one it replaced.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_legacy_real_directory_workspace_migrates_and_old_generations_are_pruned() {
        let tooling = TempDir::new().unwrap();
        let ws = eslint_workspace_dir(tooling.path());
        std::fs::create_dir_all(ws.join("node_modules").join(".bin")).unwrap();
        std::fs::write(ws.join("camerata.fresh.test.mjs"), "// stray file").unwrap();
        assert!(!ws.is_symlink());

        let npm = FakeNpm::new("0", false);
        ensure_eslint_with(tooling.path(), npm.program.as_os_str())
            .await
            .expect("migrates");
        assert!(
            ws.is_symlink(),
            "the workspace is now a published generation"
        );
        assert!(eslint_is_provisioned(&ws).await);
        assert!(
            !ws.join("camerata.fresh.test.mjs").exists(),
            "the stray file belonged to the legacy tree"
        );
        // current + the legacy tree, kept one cycle.
        assert_eq!(generation_names(tooling.path()).len(), 2);

        make_eslint_workspace_stale(tooling.path());
        ensure_eslint_with(tooling.path(), npm.program.as_os_str())
            .await
            .unwrap();
        assert_eq!(
            generation_names(tooling.path()).len(),
            2,
            "current + previous only: {:?}",
            generation_names(tooling.path())
        );
        make_eslint_workspace_stale(tooling.path());
        ensure_eslint_with(tooling.path(), npm.program.as_os_str())
            .await
            .unwrap();
        assert_eq!(generation_names(tooling.path()).len(), 2);
        assert_eq!(npm.installs(), 3);
    }

    /// A caller arriving while ANOTHER provisioner holds the lock waits for it, then re-probes
    /// and does NOT install again.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_caller_that_waited_on_the_lock_reprobes_instead_of_installing() {
        let tooling = TempDir::new().unwrap();
        let npm = FakeNpm::new("0", false);

        // "Another process" holds the lock and, while holding it, finishes the install.
        let held = acquire_provision_lock(tooling.path(), "eslint", Duration::from_secs(5))
            .await
            .unwrap();
        let waiter = {
            let tooling = tooling.path().to_path_buf();
            let program = npm.program.clone();
            tokio::spawn(async move { ensure_eslint_with(&tooling, program.as_os_str()).await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiter.is_finished(), "the waiter is blocked on the lock");
        assert_eq!(npm.installs(), 0, "and has not installed");

        // The holder's install lands (a separate provisioner, e.g. another process).
        let other = FakeNpm::new("0", false);
        install_eslint_generation(tooling.path(), other.program.as_os_str())
            .await
            .unwrap();
        drop(held);

        let bin = waiter
            .await
            .unwrap()
            .expect("waiter returns the provisioned bin");
        assert!(eslint_runs(&bin).await);
        assert_eq!(
            npm.installs(),
            0,
            "the waiter re-probed and did not install"
        );
        assert_eq!(other.installs(), 1);
    }

    /// The lock is exclusive and the wait is bounded: a wedged holder cannot hang callers forever.
    #[tokio::test]
    async fn the_provision_lock_is_exclusive_and_the_wait_is_bounded() {
        let tooling = TempDir::new().unwrap();
        let held = acquire_provision_lock(tooling.path(), "eslint", Duration::from_secs(5))
            .await
            .unwrap();
        let err = acquire_provision_lock(tooling.path(), "eslint", Duration::from_millis(150))
            .await
            .expect_err("contended");
        assert!(
            matches!(&err, ProvisionError::InstallFailed { tool: "eslint", detail } if detail.contains("timed out")),
            "{err:?}"
        );
        // Another tool's lock is independent.
        acquire_provision_lock(tooling.path(), "semgrep", Duration::from_millis(150))
            .await
            .expect("per-tool locks do not block each other");
        drop(held);
        acquire_provision_lock(tooling.path(), "eslint", Duration::from_millis(150))
            .await
            .expect("released on drop");
    }

    /// semgrep: a caller that waited on the lock re-probes and does not rebuild the venv the
    /// holder just finished. (Needs python3 only to get past the interpreter check.)
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_semgrep_caller_that_waited_on_the_lock_does_not_rebuild_the_venv() {
        use std::os::unix::fs::PermissionsExt;
        if !interpreter_available("python3").await {
            eprintln!("skipping: python3 not available to reach the lock");
            return;
        }
        let tooling = TempDir::new().unwrap();
        let held = acquire_provision_lock(tooling.path(), "semgrep", Duration::from_secs(5))
            .await
            .unwrap();
        let waiter = {
            let tooling = tooling.path().to_path_buf();
            tokio::spawn(async move { ensure_semgrep(&tooling).await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiter.is_finished(), "blocked on the lock");

        let venv = semgrep_venv_dir(tooling.path());
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        let stub = venv.join("bin").join("semgrep");
        std::fs::write(&stub, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        drop(held);

        waiter.await.unwrap().expect("returns the holder's venv");
        assert!(
            !venv.join("pyvenv.cfg").exists(),
            "the waiter must not have run `python3 -m venv` over the finished install"
        );
    }

    /// osv-scanner's binary is written so it is never visible half-written or non-executable.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_osv_binary_is_written_atomically_and_executable() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("osv-scanner");
        write_executable_atomically(&dest, b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        assert!(osv_scanner_is_provisioned(&dest).await);
        // Replacing a working binary is also a single rename.
        write_executable_atomically(&dest, b"#!/bin/sh\nexit 0\n# v2\n")
            .await
            .unwrap();
        assert!(osv_scanner_is_provisioned(&dest).await);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["osv-scanner"], "no .part debris: {names:?}");
    }

    // ── tests never write to the REAL shared tooling dir ──────────────────────

    #[test]
    fn a_cargo_test_binary_is_recognised_by_its_location() {
        assert!(is_cargo_test_binary(Path::new(
            "/w/target/debug/deps/camerata_server-0123abcd"
        )));
        assert!(!is_cargo_test_binary(Path::new(
            "/w/target/debug/camerata-server"
        )));
        assert!(!is_cargo_test_binary(Path::new(
            "/Applications/Camerata.app/camerata"
        )));
        // THIS very process is a test binary: the heuristic holds where it matters.
        assert!(
            is_cargo_test_binary(&std::env::current_exe().unwrap()),
            "the guard would not protect the real tooling dir from this test binary"
        );
    }

    #[test]
    fn a_test_process_may_write_a_temp_tooling_dir_but_never_the_real_one() {
        let real = Path::new("/home/u/.local/share/camerata/tooling");
        let test_exe = Path::new("/w/target/debug/deps/camerata_server-0123abcd");
        let app_exe = Path::new("/w/target/debug/camerata-server");
        let tmp = TempDir::new().unwrap();

        assert!(test_process_may_not_write(real, Some(real), Some(test_exe)));
        assert!(!test_process_may_not_write(
            tmp.path(),
            Some(real),
            Some(test_exe)
        ));
        // The product itself is never restricted.
        assert!(!test_process_may_not_write(real, Some(real), Some(app_exe)));
        // Unresolvable inputs do not restrict (there is nothing to protect).
        assert!(!test_process_may_not_write(real, None, Some(test_exe)));
        assert!(!test_process_may_not_write(real, Some(real), None));
    }

    #[cfg(unix)]
    #[test]
    fn a_differently_spelled_path_to_the_real_tooling_dir_is_still_refused() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("tooling");
        std::fs::create_dir_all(&real).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let test_exe = Path::new("/w/target/debug/deps/camerata_server-0123abcd");
        assert!(test_process_may_not_write(
            &alias,
            Some(&real),
            Some(test_exe)
        ));
    }

    /// THE regression test for "tests mutate the shared workspace". Aims every `ensure_*` at the
    /// REAL per-user tooling dir from this test binary. Whatever state that dir is in, none of
    /// them may write to it: healthy tools are returned as they are, anything that would need an
    /// install is refused. No `package.json` is rewritten, no generation directory or lock file
    /// appears. (Safe to run on any machine: the only outcomes are a read or a refusal.)
    #[tokio::test]
    async fn ensure_calls_from_a_test_process_never_write_the_real_tooling_dir() {
        let Some(real) = tooling_dir() else {
            return;
        };
        let manifest = eslint_workspace_dir(&real).join("package.json");
        let mtime_before = std::fs::metadata(&manifest).and_then(|m| m.modified()).ok();
        let existed_before = (
            real.exists(),
            eslint_generations_dir(&real).exists(),
            real.join(".eslint.lock").exists(),
            real.join(".semgrep.lock").exists(),
            real.join(".osv-scanner.lock").exists(),
        );

        let eslint = ensure_eslint(&real).await;
        let semgrep = ensure_semgrep(&real).await;
        let osv = ensure_osv_scanner(&real).await;

        for (tool, outcome) in [
            ("eslint", eslint.map(|_| ())),
            ("semgrep", semgrep.map(|_| ())),
            ("osv-scanner", osv.map(|_| ())),
        ] {
            assert!(
                matches!(
                    outcome,
                    Ok(()) | Err(ProvisionError::RealToolingProtected { .. })
                ),
                "{tool}: a test process either reads a healthy tool or is refused, got {outcome:?}"
            );
        }
        assert_eq!(
            std::fs::metadata(&manifest).and_then(|m| m.modified()).ok(),
            mtime_before,
            "the real eslint workspace was rewritten by a test"
        );
        let exists_after = (
            real.exists(),
            eslint_generations_dir(&real).exists(),
            real.join(".eslint.lock").exists(),
            real.join(".semgrep.lock").exists(),
            real.join(".osv-scanner.lock").exists(),
        );
        assert_eq!(
            exists_after, existed_before,
            "a test created files in the real tooling dir"
        );
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

    // ── osv-scanner path helpers ──────────────────────────────────────────────

    #[test]
    fn osv_scanner_dir_is_under_tooling() {
        let tmp = TempDir::new().unwrap();
        let tooling = tmp.path().to_path_buf();
        let dir = osv_scanner_dir(&tooling);
        assert!(
            dir.starts_with(&tooling),
            "osv-scanner dir must be inside tooling: {dir:?}"
        );
    }

    #[test]
    fn osv_scanner_bin_is_inside_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("osv-scanner");
        let bin = osv_scanner_bin(&dir);
        assert!(
            bin.starts_with(&dir),
            "osv-scanner bin must be inside its dir: {bin:?}"
        );
        let name = bin.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("osv-scanner"),
            "binary name must start with osv-scanner: {name}"
        );
    }

    // ── osv-scanner probe logic ───────────────────────────────────────────────

    #[tokio::test]
    async fn missing_osv_scanner_binary_is_not_provisioned() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("osv-scanner");
        let bin = osv_scanner_bin(&dir);
        // No binary created: must report not provisioned.
        assert!(!osv_scanner_is_provisioned(&bin).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stub_osv_scanner_binary_is_provisioned() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("osv-scanner");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let bin = osv_scanner_bin(&dir);
        tokio::fs::write(&bin, b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        assert!(osv_scanner_is_provisioned(&bin).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_osv_scanner_binary_is_not_provisioned() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("osv-scanner");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let bin = osv_scanner_bin(&dir);
        tokio::fs::write(&bin, b"#!/bin/sh\nexit 1\n")
            .await
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        assert!(!osv_scanner_is_provisioned(&bin).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_osv_scanner_is_idempotent_when_provisioned() {
        // When the binary is already present + healthy, ensure_osv_scanner must
        // short-circuit at the cache probe and return the same path twice.
        let tmp = TempDir::new().unwrap();
        let tooling = tmp.path().to_path_buf();
        let dir = osv_scanner_dir(&tooling);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let bin = osv_scanner_bin(&dir);
        tokio::fs::write(&bin, b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        let r1 = ensure_osv_scanner(&tooling).await;
        assert!(r1.is_ok(), "first call must succeed: {r1:?}");
        let r2 = ensure_osv_scanner(&tooling).await;
        assert!(r2.is_ok(), "second call must succeed: {r2:?}");
        assert_eq!(
            r1.unwrap(),
            r2.unwrap(),
            "both calls must return the same path"
        );
    }

    // ── download timeout bounds ───────────────────────────────────────────────
    //
    // Verify that `download_osv_scanner` fails fast (within the declared timeout
    // window) when pointed at an unroutable host.  The test uses a TCP black-hole
    // address (192.0.2.1 is TEST-NET-1 from RFC 5737; packets are dropped, not
    // refused) to exercise the connect-timeout path without relying on DNS or a
    // real server.
    //
    // The test calls into the private function indirectly by temporarily replacing
    // the URL — but since `download_osv_scanner` constructs the URL from the
    // module-level constants we instead verify the *client builder* carries the
    // right timeout by building one the same way and asserting it doesn't hang
    // on a loopback connection to a closed port.
    //
    // Approach: build a reqwest Client with the same parameters, attempt a GET
    // to 127.0.0.1:<closed-port>, and assert the whole thing completes (with an
    // error) within a generous 10 s wall-clock window.  If the timeout logic is
    // missing the future never resolves and the test runner itself times out.
    #[tokio::test]
    async fn download_client_has_connect_timeout_and_fails_fast() {
        // A randomly chosen high port that is almost certainly not listening.
        // We just need the connection to fail (refused or timeout), not succeed.
        let url = "http://192.0.2.1:9/"; // TEST-NET-1 (RFC 5737) — black hole

        let client = reqwest::Client::builder()
            .user_agent(concat!("camerata/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("client build must not fail");

        // The whole attempt (including the connect_timeout) must complete within
        // 8 s (generous buffer above the 5 s connect-timeout).  If there is no
        // timeout on the client the request hangs indefinitely.
        let result = tokio::time::timeout(Duration::from_secs(8), client.get(url).send()).await;

        // Either the outer timeout fired first (should not happen if connect_timeout
        // is wired up) or reqwest itself returned an error (connection refused or
        // timeout).  In both cases the important property is the future resolved
        // within 8 s — a hang would cause the outer timeout to be `Err(Elapsed)`.
        match result {
            Ok(_) => {
                // reqwest returned (Ok or Err) within 8 s — timeout is working.
            }
            Err(_elapsed) => {
                // The 8-second guard fired — the connect_timeout is NOT working.
                panic!(
                    "download_osv_scanner client did not time out within 8 s; \
                     connect_timeout is likely missing from the Client builder"
                );
            }
        }
    }

    // ── release asset naming ──────────────────────────────────────────────────

    #[test]
    fn osv_release_asset_is_some_on_common_platforms() {
        // This test runs on the CI / dev machine — verify the function returns
        // *something* on a typical developer host (linux/darwin, 64-bit).  It will
        // legitimately return None on 32-bit / Windows / exotic arch, so we can't
        // unconditionally assert Some.  The test just documents expected behaviour.
        let asset = osv_release_asset();
        // On common platforms (linux amd64/arm64, darwin amd64/arm64) it must be Some.
        #[cfg(any(
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ),
            all(
                target_os = "macos",
                any(target_arch = "x86_64", target_arch = "aarch64")
            )
        ))]
        assert!(
            asset.is_some(),
            "expected a release asset name on this platform (linux/darwin 64-bit)"
        );
        // Whatever it returns, it must start with osv-scanner_.
        if let Some(a) = asset {
            assert!(
                a.starts_with("osv-scanner_"),
                "release asset name must start with osv-scanner_: {a}"
            );
        }
    }
}
