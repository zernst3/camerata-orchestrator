//! Provisioning of the scan tools that are not a single downloaded file: the eslint package
//! tree (installed by the pinned Node.js's npm) and the shared machinery every provisioner uses
//! (the tooling folder, the per-tool lock, the test-process guard).
//!
//! The downloaded artifacts (ruff, Node.js, CPython, osv-scanner) and semgrep (installed into a
//! venv of the pinned CPython) live in [`crate::pinned_tools`], which also decides an
//! inspection's tool set and provisions it before the first model call (#587). Nothing here runs
//! a program from the user's `PATH`: npm is the pinned Node.js running npm's own script.
//!
//! # Cache-dir convention
//!
//! `dirs::data_dir()` → `<data_dir>/camerata/tooling/`:
//!
//! ```text
//! <data_dir>/camerata/tooling/
//! ├── pinned/
//! │   ├── ruff-<version>/         ruff release archive, unpacked
//! │   ├── node-<version>/         Node.js release archive, unpacked (npm inside)
//! │   ├── python-<version>/       python-build-standalone CPython, unpacked
//! │   ├── semgrep-<version>/venv  venv of the pinned CPython; semgrep installed by hash
//! │   └── osv-scanner-<version>/  the release binary
//! ├── eslint  ->  eslint.gen/<id>     symlink to the current complete eslint tree (unix)
//! ├── eslint.gen/
//! │   └── <id>/              one COMPLETE generation; the previous one is kept a cycle
//! │       ├── node_modules/  eslint + TS parser + plugins installed here
//! │       ├── package.json   generated lock-anchor
//! │       └── camerata.config.mjs  bundled flat config (copied from binary assets)
//! └── .<tool>.lock           advisory provisioning lock, one per tool
//! ```
//!
//! # Concurrency: nothing is ever installed over a live tree
//!
//! The tooling dir is shared by the product, every scan, and (on a developer machine) every test
//! process. Each provisioner therefore (1) takes a per-tool OS file lock so only one provisioner
//! runs at a time, across processes and within one, then re-probes because the holder may have
//! finished the job; and (2) for eslint, builds the new tree in a staging directory and publishes
//! it with one atomic rename, so a reader sees the old complete tree or the new complete tree and
//! never a half-installed one. See [`ensure_eslint_with`] for the incident this closes. Test
//! processes additionally cannot provision into the real tooling dir at all
//! ([`refuse_real_tooling_in_test_process`]).
//!
//! # Failure is never silent
//!
//! Every entry point returns `Result<_, ProvisionError>`. Before a run, a failure refuses the
//! inspection ([`crate::pinned_tools::provision_tool_set`]); during a run it stops the run
//! ([`crate::scan_tools::ToolProvisioningStop`]).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Errors that can arise during tool provisioning. Before a run they refuse the inspection; during
/// a run they stop it.
#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    /// Camerata has no build of this tool for this machine.
    #[error("{tool} cannot be provisioned on this machine: {detail}")]
    UnsupportedPlatform { tool: &'static str, detail: String },

    /// The pin table has no file for this platform.
    #[error("{tool} has no pinned build for {platform}")]
    NoArtifactForPlatform {
        tool: &'static str,
        platform: &'static str,
    },

    /// The download did not complete.
    #[error("downloading {tool} failed: {detail}")]
    DownloadFailed { tool: &'static str, detail: String },

    /// The downloaded file is not the pinned one. Nothing was unpacked.
    #[error(
        "the downloaded {tool} does not match its pinned checksum (expected sha256 {expected}, \
         got {actual}); it was not installed"
    )]
    ChecksumMismatch {
        tool: &'static str,
        expected: String,
        actual: String,
    },

    /// An install step ran but exited non-zero or produced no program.
    #[error("provisioning install failed for {tool}: {detail}")]
    InstallFailed { tool: &'static str, detail: String },

    /// The tool is not installed and Camerata is offline ([`crate::pinned_tools::OFFLINE_ENV`]):
    /// nothing was downloaded.
    #[error(
        "{tool} is not installed in {folder} and Camerata is offline ({env}=1), so nothing was \
         downloaded"
    )]
    Offline {
        tool: &'static str,
        folder: String,
        env: &'static str,
    },

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

/// The root of the Camerata-managed tool environment: [`TOOLING_DIR_ENV`] when it is set, else
/// `<data_dir>/camerata/tooling/`. Returns `None` when neither resolves (unusual; same condition
/// under which the other stores fall back to in-memory).
pub fn tooling_dir() -> Option<PathBuf> {
    match std::env::var_os(TOOLING_DIR_ENV) {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => default_tooling_dir(),
    }
}

/// The per-user tooling folder, ignoring [`TOOLING_DIR_ENV`]: the shared tree the test-process
/// guard protects ([`refuse_real_tooling_in_test_process`]).
pub fn default_tooling_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("camerata").join("tooling"))
}

/// The environment variable that points the tooling folder somewhere else (a test, a sandbox).
pub const TOOLING_DIR_ENV: &str = "CAMERATA_TOOLING_DIR";

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
pub(crate) const PROVISION_LOCK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
/// [`ensure_eslint_with`] on the REAL tooling directory. When that shared workspace looked stale (a
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
pub(crate) fn refuse_real_tooling_in_test_process(
    tooling: &Path,
    tool: &'static str,
) -> Result<(), ProvisionError> {
    let exe = std::env::current_exe().ok();
    if test_process_may_not_write(tooling, default_tooling_dir().as_deref(), exe.as_deref()) {
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
pub(crate) struct ProvisionLock {
    _file: std::fs::File,
}

/// Take the exclusive provisioning lock for `tool` under `tooling`, waiting up to `timeout`.
///
/// The wait runs on the blocking pool (`flock` polling must not park a runtime worker). Callers
/// MUST re-run their provisioned probe after this returns: whoever held the lock may well have
/// finished the install the caller was about to start.
pub(crate) async fn acquire_provision_lock(
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

/// Path to the semgrep binary inside the Camerata-managed venv.
/// On Windows the venv layout uses `Scripts/`; everywhere else `bin/`.
pub fn semgrep_bin(venv: &Path) -> PathBuf {
    #[cfg(windows)]
    let bin = venv.join("Scripts").join("semgrep.exe");
    #[cfg(not(windows))]
    let bin = venv.join("bin").join("semgrep");
    bin
}

/// The eslint flat config, embedded in the binary: an installed Camerata has no source tree to
/// read it from (review round 1 on #587).
pub(crate) const ESLINT_CONFIG: &str = include_str!("../assets/eslint/camerata.config.mjs");

/// The committed lockfile that pins the whole eslint tool tree, embedded for the same reason.
pub(crate) const ESLINT_LOCK: &str = include_str!("../assets/eslint/package-lock.json");

/// The bundled semgrep rules, embedded in the binary as `(file name, contents)`. The list is the
/// manifest of `assets/semgrep-rules/`: a test holds it to exactly that folder's files, so a rule
/// file added there without a line here fails the build's tests rather than going missing at run
/// time. (A build-script manifest was not used: `build.rs` is shared with the app crate.)
pub const SEMGREP_RULES: &[(&str, &str)] = &[
    (
        "security.yml",
        include_str!("../assets/semgrep-rules/security.yml"),
    ),
    (
        "taint-security.yml",
        include_str!("../assets/semgrep-rules/taint-security.yml"),
    ),
];

/// The folder `semgrep --config` is given: the embedded rules, written into the tooling folder
/// (`<tooling>/semgrep-rules/<hash>/`, the hash of the rules, so a build with other rules never
/// reads an older copy). Written to a staging folder and renamed into place, so a concurrent
/// scan never reads half a ruleset. The scan runs offline against it: no registry pack is pulled.
pub fn semgrep_rules_dir(tooling: &Path) -> std::io::Result<PathBuf> {
    let mut all = String::new();
    for (name, text) in SEMGREP_RULES {
        all.push_str(name);
        all.push('\n');
        all.push_str(text);
    }
    let hash = crate::pinned_tools::fetch::sha256_hex(all.as_bytes());
    let root = tooling.join("semgrep-rules");
    let dir = root.join(&hash[..16]);
    let complete = |d: &Path| {
        SEMGREP_RULES
            .iter()
            .all(|(n, t)| std::fs::read_to_string(d.join(n)).is_ok_and(|have| have == *t))
    };
    if complete(&dir) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&root)?;
    let staging = root.join(format!(".staging-{}-{:x}", &hash[..16], std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    for (name, text) in SEMGREP_RULES {
        std::fs::write(staging.join(name), text)?;
    }
    if std::fs::rename(&staging, &dir).is_err() {
        // Another scan published the same rules first; ours is not needed.
        let _ = std::fs::remove_dir_all(&staging);
    }
    if complete(&dir) {
        Ok(dir)
    } else {
        Err(std::io::Error::other(format!(
            "the semgrep rules could not be written to {}",
            dir.display()
        )))
    }
}

// ─── eslint ──────────────────────────────────────────────────────────────────

/// Path to the Camerata-managed eslint workspace:
/// `<tooling_dir>/eslint/` (on unix a symlink to the current complete generation; see
/// [`ensure_eslint_with`]). Probe and provision through this path; a process that RUNS eslint
/// resolves it once via [`eslint_run_paths`] and uses the concrete paths for the whole run.
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
/// [`ensure_eslint_with`]).  The caller passes `--config <path>` so eslint uses
/// Camerata's baseline rather than the repo's config.
pub fn eslint_config_path(workspace: &Path) -> PathBuf {
    workspace.join("camerata.config.mjs")
}

/// The concrete paths one eslint run uses, all under the canonical generation directory (never
/// the `eslint` symlink). See [`eslint_run_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EslintRunPaths {
    /// The canonical generation directory (`eslint.gen/<id>` on unix).
    pub workspace: PathBuf,
    /// The eslint binary inside [`Self::workspace`].
    pub bin: PathBuf,
    /// The bundled flat config inside [`Self::workspace`].
    pub config: PathBuf,
    /// eslint's own entry script, run as `<pinned node> <script>` so no `node` is looked up on
    /// `PATH` (the `.bin/eslint` launcher's `#!/usr/bin/env node` would look one up).
    pub script: PathBuf,
}

/// eslint's entry script inside a workspace.
pub fn eslint_script(workspace: &Path) -> PathBuf {
    workspace
        .join("node_modules")
        .join("eslint")
        .join("bin")
        .join("eslint.js")
}

/// Resolve the eslint workspace link ONCE to its concrete generation directory and derive every
/// path a run needs from it. A re-provision atomically swaps the `eslint` symlink; a run that
/// kept spelling its paths through the link could see a lookup fail transiently (`EINVAL` on
/// macOS while the link is replaced) or land in a half-swapped state. Holding canonical paths
/// makes the run immune: the previous generation is kept one cycle, so they stay valid.
///
/// The resolution itself can hit that transient `EINVAL`; it is retried a few times (a missing
/// tree fails with `NotFound` every time and is returned immediately).
pub async fn eslint_run_paths(tooling: &Path) -> std::io::Result<EslintRunPaths> {
    const ATTEMPTS: usize = 5;
    let link = eslint_workspace_dir(tooling);
    let mut attempt = 0;
    let workspace = loop {
        attempt += 1;
        match tokio::fs::canonicalize(&link).await {
            Ok(p) => break p,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput && attempt < ATTEMPTS => {
                tokio::task::yield_now().await;
            }
            Err(e) => return Err(e),
        }
    };
    Ok(EslintRunPaths {
        bin: eslint_bin(&workspace),
        config: eslint_config_path(&workspace),
        script: eslint_script(&workspace),
        workspace,
    })
}

/// Probe whether eslint is already provisioned and healthy.
///
/// # Why the binary probe alone is NOT the health check (W6 / C11)
///
/// This used to ask one question — "does the eslint binary answer `--version`" —
/// and [`ensure_eslint_with`] returned early on a `true`. That made the provisioned
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
    eslint_is_provisioned_with(workspace, None).await
}

/// [`eslint_is_provisioned`] with the `PATH` the version probe runs with. The product passes the
/// child `PATH` ([`crate::pinned_tools::ToolEnv::child_path`]), whose first folder is the pinned
/// Node.js: the `.bin/eslint` launcher finds `node` through it, never through the user's `PATH`.
pub async fn eslint_is_provisioned_with(workspace: &Path, path_env: Option<&OsStr>) -> bool {
    if !eslint_workspace_is_current(workspace) {
        return false;
    }
    let mut probe = tokio::process::Command::new(eslint_bin(workspace));
    probe.arg("--version").kill_on_drop(true);
    if let Some(path) = path_env {
        probe.env("PATH", path);
    }
    match probe.output().await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// The disk half of [`eslint_is_provisioned`]: the launcher exists, every pinned package is
/// installed at its version, and the config is the bundled one. Starts no process, so the tool
/// list (`--plan-only`, the pre-flight) can use it.
pub fn eslint_workspace_is_current(workspace: &Path) -> bool {
    eslint_bin(workspace).exists()
        && eslint_packages_installed(workspace)
        && eslint_config_is_current(workspace)
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

/// `true` when the workspace's copy of the flat config is byte-identical to the embedded
/// [`ESLINT_CONFIG`]. A content comparison rather than an mtime one: the config ships inside the
/// binary, so a file's mtime says nothing about which Camerata build wrote it.
fn eslint_config_is_current(workspace: &Path) -> bool {
    std::fs::read(eslint_config_path(workspace)).is_ok_and(|have| have == ESLINT_CONFIG.as_bytes())
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
pub(crate) const ESLINT_NPM_PACKAGES: &[&str] = &[
    "eslint@9.39.5",
    "@typescript-eslint/parser@8.72.0",
    "@typescript-eslint/eslint-plugin@8.72.0",
    "eslint-plugin-react-hooks@5.2.0",
    "eslint-plugin-jest@28.14.0",
];

/// The eslint version in [`ESLINT_NPM_PACKAGES`], for the tool list. A test holds the two equal.
pub const ESLINT_VERSION: &str = "9.39.5";

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

/// The `npm` arguments that install the workspace: `npm ci` from the embedded lockfile, exactly
/// that tree (it fails loudly if the lockfile disagrees with the manifest). There is no fallback to
/// an unpinned `npm install`. `--ignore-scripts`: no package's install script runs.
/// `--prefer-offline`: a warm npm cache needs no network.
fn eslint_install_args_now() -> Vec<String> {
    [
        "ci",
        "--prefer-offline",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
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

/// The bare npm package name from an [`ESLINT_NPM_PACKAGES`] spec, dropping any
/// `@<range>` version suffix. Handles scoped names: the scope `@` at index 0 is not
/// a version separator, so `@typescript-eslint/parser@^8` → `@typescript-eslint/parser`
/// while `eslint@^9` → `eslint` and a bare `eslint` → `eslint`.
pub(crate) fn npm_package_name(spec: &str) -> &str {
    match spec.rfind('@') {
        Some(i) if i > 0 => &spec[..i],
        _ => spec,
    }
}

/// The pinned version from a spec (e.g. `eslint@9.39.5` → `Some("9.39.5")`), or `None`
/// when the spec carries no version. The text after the last `@` is returned as written: the
/// pins are exact, so it is compared for equality, never parsed as a range.
pub(crate) fn npm_pinned_version(spec: &str) -> Option<&str> {
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
pub(crate) async fn ensure_eslint_with(
    tooling: &Path,
    npm: &NpmCommand,
) -> Result<PathBuf, ProvisionError> {
    let workspace = eslint_workspace_dir(tooling);
    let bin = eslint_bin(&workspace);
    let path_env = npm.path_env.as_deref();

    if eslint_is_provisioned_with(&workspace, path_env).await {
        return Ok(bin);
    }

    if npm.offline {
        return Err(crate::pinned_tools::fetch::offline("eslint", tooling));
    }
    refuse_real_tooling_in_test_process(tooling, "eslint")?;

    npm.prepare_home().await?;
    if !npm.answers_version().await {
        return Err(ProvisionError::InstallFailed {
            tool: "eslint",
            detail: "the pinned Node.js npm did not answer --version".to_string(),
        });
    }

    let _lock = acquire_provision_lock(tooling, "eslint", PROVISION_LOCK_TIMEOUT).await?;
    if eslint_is_provisioned_with(&workspace, path_env).await {
        return Ok(bin);
    }

    install_eslint_generation(tooling, npm).await?;
    Ok(bin)
}

/// How npm is started: the pinned Node.js running npm's own script, with the child `PATH`
/// ([`crate::pinned_tools::ToolEnv::npm_command`]). Never a bare `npm` from the user's `PATH`.
/// Tests use [`NpmCommand::program`] with a fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmCommand {
    pub launcher: OsString,
    /// Arguments before npm's own (npm's script for the pinned Node.js).
    pub prefix: Vec<OsString>,
    /// The `PATH` npm and everything it starts get. `None` only for a test fake.
    pub path_env: Option<OsString>,
    /// True when Camerata is offline: npm would reach its registry, so a missing eslint tree is a
    /// refusal instead.
    pub offline: bool,
    /// npm's own folder in the tooling folder: its user and global config files (empty) and its
    /// cache live here, never the user's `~/.npmrc` or `~/.npm`. `None` only for a test fake.
    pub home: Option<PathBuf>,
}

/// The registry eslint and its plugins install from: npm's official one.
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org/";

impl NpmCommand {
    /// A single program that behaves as npm (a test fake).
    pub fn program(program: impl Into<OsString>) -> Self {
        Self {
            launcher: program.into(),
            prefix: Vec::new(),
            path_env: None,
            offline: false,
            home: None,
        }
    }

    /// The npm command: none of the user's `npm_config_*` variables (any case), Camerata's own
    /// config files and cache, the official registry.
    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.launcher);
        cmd.args(&self.prefix).kill_on_drop(true);
        if let Some(path) = &self.path_env {
            cmd.env("PATH", path);
        }
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("npm_config_")
            {
                cmd.env_remove(&key);
            }
        }
        cmd.env("npm_config_registry", NPM_REGISTRY)
            .env("npm_config_update_notifier", "false")
            .env("npm_config_fund", "false")
            .env("npm_config_audit", "false");
        if let Some(home) = &self.home {
            cmd.env("npm_config_userconfig", home.join("npmrc"))
                .env("npm_config_globalconfig", home.join("global-npmrc"))
                .env("npm_config_cache", home.join("cache"));
        }
        cmd
    }

    /// Create npm's own folder and its empty config files.
    async fn prepare_home(&self) -> std::io::Result<()> {
        let Some(home) = &self.home else {
            return Ok(());
        };
        tokio::fs::create_dir_all(home.join("cache")).await?;
        for file in ["npmrc", "global-npmrc"] {
            tokio::fs::write(home.join(file), "").await?;
        }
        Ok(())
    }

    async fn answers_version(&self) -> bool {
        self.command()
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Build a complete, healthy eslint tree in staging, then publish it. The caller holds the
/// provisioning lock.
async fn install_eslint_generation(tooling: &Path, npm: &NpmCommand) -> Result<(), ProvisionError> {
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
    if !eslint_is_provisioned_with(&finished, npm.path_env.as_deref()).await {
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
async fn build_eslint_tree(dir: &Path, npm: &NpmCommand) -> Result<(), ProvisionError> {
    tokio::fs::create_dir_all(dir).await?;

    // The manifest pins every package to its exact version, and the embedded lockfile pins the
    // whole tree (transitive dependencies included): `npm ci` then installs the same tool on
    // every machine, and nothing else.
    tokio::fs::write(dir.join("package.json"), eslint_package_json()).await?;
    tokio::fs::write(dir.join("package-lock.json"), ESLINT_LOCK).await?;

    let npm_args = eslint_install_args_now();

    let npm_out = npm
        .command()
        .args(&npm_args)
        .current_dir(dir)
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

    // The embedded flat config, so eslint can find it.
    tokio::fs::write(eslint_config_path(dir), ESLINT_CONFIG).await?;
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

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
