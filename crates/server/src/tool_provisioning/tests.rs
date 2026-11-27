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
    assert!(crate::pinned_tools::semgrep_bin(&tooling).starts_with(&tooling));
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
/// FAILS BEFORE THE FIX: `ensure_eslint` (now `ensure_eslint_with`) only ever copied the config on the
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
async fn missing_eslint_binary_is_not_provisioned() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("nonexistent-eslint");
    assert!(!eslint_is_provisioned(&ws).await);
}

// ── probe with a stub binary ──────────────────────────────────────────────
//
// A tiny shell script that always exits 0 stands in for the real binary to
// verify the probe logic detects "binary present + exits 0 → provisioned".

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
    // A launcher that exits non-zero is treated as broken/not-provisioned, even with every
    // package and the config in place.
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("eslint");
    let bin_dir = ws.join("node_modules").join(".bin");
    tokio::fs::create_dir_all(&bin_dir).await.unwrap();
    let stub = bin_dir.join("eslint");
    tokio::fs::write(&stub, b"#!/bin/sh\nexit 1\n")
        .await
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .await
        .unwrap();
    stub_eslint_packages_and_config(&ws);
    assert!(
        eslint_workspace_is_current(&ws),
        "the disk half is complete"
    );
    assert!(!eslint_is_provisioned(&ws).await);
}

// ── ensure_eslint_with on an already provisioned workspace ───────────────

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
        // An npm that cannot start proves the probe short-circuits: nothing is installed.
        let npm = NpmCommand::program("/nonexistent/acme-npm");
        let r1 = ensure_eslint_with(&tooling, &npm).await;
        assert!(r1.is_ok());
        let r2 = ensure_eslint_with(&tooling, &npm).await;
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
/// `ensure_eslint_with` must re-provision it. Writes through the workspace path.
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
            ensure_eslint_with(&tooling, &NpmCommand::program(program)).await
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

/// A real eslint run spawns from the CONCRETE generation directory, never through the
/// `eslint` symlink: the binary, the config and the workspace all live under the canonical
/// `eslint.gen/<id>` path, resolved once. A re-provision that swaps the link afterwards
/// cannot affect a run already holding these paths, and the previous generation (kept one
/// cycle) still serves them.
#[cfg(unix)]
#[tokio::test]
async fn eslint_run_paths_are_canonical_generation_paths_not_the_symlink() {
    let tooling = TempDir::new().unwrap();
    let npm = FakeNpm::new("0", false);
    ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
        .await
        .expect("first provision");
    let link = eslint_workspace_dir(tooling.path());
    let generation = std::fs::canonicalize(&link).unwrap();

    let paths = eslint_run_paths(tooling.path()).await.expect("resolve");
    assert_eq!(paths.workspace, generation);
    assert_ne!(paths.workspace, link, "must not be the symlink path");
    assert!(paths.bin.starts_with(&generation), "{:?}", paths.bin);
    assert!(paths.config.starts_with(&generation), "{:?}", paths.config);
    for p in [&paths.workspace, &paths.bin, &paths.config] {
        assert!(!p.starts_with(&link), "{p:?} goes through the symlink");
    }

    // A re-provision swaps the link to a new generation; the resolved paths still work
    // because the previous generation is kept for one cycle.
    make_eslint_workspace_stale(tooling.path());
    ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
        .await
        .expect("re-provision");
    assert_ne!(
        std::fs::canonicalize(&link).unwrap(),
        generation,
        "link swapped"
    );
    assert!(
        paths.config.exists(),
        "previous generation is kept one cycle"
    );
    assert!(eslint_runs(&paths.bin).await, "the resolved bin still runs");
}

/// Whether `path` is readable, tolerating the kernel's transient `EINVAL`.
///
/// On macOS, resolving a path THROUGH a symlink while `rename(2)` replaces that symlink can
/// fail one lookup with `EINVAL` (observed: ~5% of runs under CPU load). It is not a missing
/// tree: the very next lookup resolves, and a genuinely absent tree fails with `ENOENT`
/// every time. So only a PERSISTENT failure, or any `NotFound`, counts as a miss.
#[cfg(unix)]
fn readable_through_a_link_swap(path: &Path) -> bool {
    for _ in 0..5 {
        match std::fs::read_to_string(path) {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => std::thread::yield_now(),
            Err(_) => return false,
        }
    }
    false
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
    ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
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
                if !readable_through_a_link_swap(&marker) {
                    misses.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_micros(100));
            }
        })
    };

    let bin = ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
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
    ensure_eslint_with(tooling.path(), &NpmCommand::program(good.program.clone()))
        .await
        .unwrap();
    make_eslint_workspace_stale(tooling.path());
    let ws = eslint_workspace_dir(tooling.path());
    let link_before = std::fs::read_link(&ws).unwrap();

    let broken = FakeNpm::new("0", true);
    let err = ensure_eslint_with(tooling.path(), &NpmCommand::program(broken.program.clone()))
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
    ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
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
    ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
        .await
        .unwrap();
    assert_eq!(
        generation_names(tooling.path()).len(),
        2,
        "current + previous only: {:?}",
        generation_names(tooling.path())
    );
    make_eslint_workspace_stale(tooling.path());
    ensure_eslint_with(tooling.path(), &NpmCommand::program(npm.program.clone()))
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
        tokio::spawn(
            async move { ensure_eslint_with(&tooling, &NpmCommand::program(program)).await },
        )
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiter.is_finished(), "the waiter is blocked on the lock");
    assert_eq!(npm.installs(), 0, "and has not installed");

    // The holder's install lands (a separate provisioner, e.g. another process).
    let other = FakeNpm::new("0", false);
    install_eslint_generation(tooling.path(), &NpmCommand::program(other.program.clone()))
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

    let fakes = crate::pinned_tools::fixtures::FakeTools::new();
    let pins = fakes.env.clone();
    let env =
        crate::pinned_tools::ToolEnv::new(real.clone(), fakes.env.platform, fakes.source.clone())
            .with_pins(move |a| pins.pins(a));
    let eslint = ensure_eslint_with(&real, &NpmCommand::program("/nonexistent/acme-npm")).await;
    let semgrep = crate::pinned_tools::ensure_semgrep(&env).await;
    let osv = env.ensure_osv_scanner().await;

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
