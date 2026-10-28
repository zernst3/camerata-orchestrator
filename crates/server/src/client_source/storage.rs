//! Every place source deletion may touch besides the checkout, injected rather than discovered,
//! and the guard that keeps a TEST from ever deleting a real one.
//!
//! # Why the roots are injected
//!
//! Deleting client source means removing files in the operator's real home: the server's scan
//! cache, the CLI's cache directory, and Claude Code's own session folder under
//! `~/.claude/projects`. A test that reached those through `dirs::home_dir()` would delete real
//! files on the machine running `cargo test`. So no deletion code looks anything up: it works from
//! a [`StorageRoots`] carried by the `AppState`. A fresh `AppState::new()` carries NONE (nothing
//! outside the checkout is known, so nothing outside it can be touched); only `AppState::from_env()`
//! installs the real ones; tests install a sandbox under a temp directory.
//!
//! # The backstop
//!
//! Injection is the design; [`ensure_not_real_location_in_test`] is the proof it holds. It mirrors
//! the tool-provisioning guard (`RealToolingProtected`): when the running executable is a cargo
//! test binary, a path under the REAL home directory, the real per-user data or cache directory,
//! or `~/.claude` is refused, whatever the injected roots say. Every removal in the purge runs it
//! first, so a wrongly wired test fails loudly instead of deleting.

use std::path::{Path, PathBuf};

use crate::onboard::finding_context::canonicalize_existing_prefix;

/// What a location is, for the completeness test and the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Treatment {
    /// Holds client code and is purged by deletion.
    Purged,
    /// Holds client code excerpts that are part of the delivered product and are KEPT.
    Kept,
    /// Checked and found to hold no client source (identity, ids, rule ids, tool binaries).
    NoClientCode,
}

/// Locations deletion may touch besides the checkout. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageRoots {
    /// The user's home directory (for the protected-path check).
    pub home: Option<PathBuf>,
    /// `<data_dir>/camerata`: where `scan-cache.json`, the inspections and the records live.
    pub data_dir: Option<PathBuf>,
    /// The CLI's cache directory ([`crate::scan_cache::state_dir::dir`]).
    pub cli_state_dir: Option<PathBuf>,
    /// Claude Code's `~/.claude/projects`.
    pub claude_projects_dir: Option<PathBuf>,
    /// The running executable's directory (never deletable: it may be the Camerata repo itself).
    pub exe_dir: Option<PathBuf>,
}

impl StorageRoots {
    /// The real locations on this machine. Production only; see the module docs.
    pub fn from_environment() -> Self {
        let home = dirs::home_dir();
        Self {
            claude_projects_dir: home.as_ref().map(|h| h.join(".claude").join("projects")),
            home,
            data_dir: crate::app_dirs::camerata_dir(),
            cli_state_dir: Some(crate::scan_cache::state_dir::dir()),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(Path::to_path_buf)),
        }
    }

    /// A complete set of roots confined to `root` (a directory the caller owns): fake home,
    /// data dir, CLI cache dir and Claude projects dir, none of them created. For tests.
    pub fn sandboxed(root: &Path) -> Self {
        let home = root.join("home");
        Self {
            claude_projects_dir: Some(home.join(".claude").join("projects")),
            home: Some(home),
            data_dir: Some(root.join("data")),
            cli_state_dir: Some(root.join("cli-state").join("camerata")),
            exe_dir: Some(root.join("bin")),
        }
    }
}

/// The names of the stores under `<data_dir>/camerata`, and what each one holds. The completeness
/// test proves this table covers every store `AppState` opens, so a new store cannot be added
/// without someone deciding whether it can hold client code.
pub const DATA_DIR_ENTRIES: &[(&str, Treatment)] = &[
    // Findings, review state: the delivered product, excerpts included. KEPT.
    ("onboarding-draft.json", Treatment::Kept),
    ("inspections", Treatment::Kept),
    // A copy of client excerpts the engine used to skip unchanged files. PURGED per repo.
    ("scan-cache.json", Treatment::Purged),
    // Identity, paths, ids, descriptions and rule ids only.
    ("projects.json", Treatment::NoClientCode),
    ("settings.json", Treatment::NoClientCode),
    ("source-records.json", Treatment::NoClientCode),
    // Governed-development state: agent chat text and ids, no source files.
    ("uow.json", Treatment::NoClientCode),
    ("stories.json", Treatment::NoClientCode),
    ("clarifications.json", Treatment::NoClientCode),
    ("clarify-resume.json", Treatment::NoClientCode),
    ("routines.json", Treatment::NoClientCode),
    ("escalations.json", Treatment::NoClientCode),
    ("checkpoints.json", Treatment::NoClientCode),
    // SQLite logs: ids, rule ids, descriptions; feedback rows are scrubbed.
    ("artifacts.db", Treatment::NoClientCode),
    ("enforcement_catches.db", Treatment::NoClientCode),
    ("governance_events.db", Treatment::NoClientCode),
    ("orchestrator_decisions.db", Treatment::NoClientCode),
    ("feedback.db", Treatment::NoClientCode),
    // Tool binaries and Camerata's bundled configs, never client files.
    ("tooling", Treatment::NoClientCode),
];

/// One concrete location with its treatment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownLocation {
    pub path: PathBuf,
    pub treatment: Treatment,
}

/// Every location that could hold client code, concretely, for `roots`:
/// the data-dir entries, the CLI cache directory (purged per repo) and Claude Code's projects
/// directory (purged per recorded session). The completeness test walks exactly these.
pub fn known_storage_locations(roots: &StorageRoots) -> Vec<KnownLocation> {
    let mut out = Vec::new();
    if let Some(data) = &roots.data_dir {
        for (name, treatment) in DATA_DIR_ENTRIES {
            out.push(KnownLocation {
                path: data.join(name),
                treatment: *treatment,
            });
        }
    }
    if let Some(cli) = &roots.cli_state_dir {
        out.push(KnownLocation {
            path: cli.clone(),
            treatment: Treatment::Purged,
        });
    }
    if let Some(claude) = &roots.claude_projects_dir {
        out.push(KnownLocation {
            path: claude.clone(),
            treatment: Treatment::Purged,
        });
    }
    out
}

/// The real per-user locations a test must never delete under: the home directory (which
/// contains `~/.claude`, `~/.semgrep` and, on Linux, the data and cache directories), plus the
/// platform's data and cache directories where they are elsewhere.
pub fn real_user_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".claude"));
        out.push(home.join(".semgrep"));
        out.push(home);
    }
    out.extend(dirs::data_dir());
    out.extend(dirs::cache_dir());
    out
}

/// The pure decision behind [`ensure_not_real_location_in_test`]: `true` when `exe` is a cargo
/// test binary AND `path` lies under one of the `real` locations. A path under the OS temp
/// directory is never "real user data" and is exempt, unless the temp directory IS one of the
/// real locations (a `TMPDIR` pointed at the home directory exempts nothing).
pub fn test_process_may_not_delete(
    path: &Path,
    real: &[PathBuf],
    exe: Option<&Path>,
    temp_dir: &Path,
) -> bool {
    let Some(exe) = exe else {
        return false;
    };
    if !crate::tool_provisioning::is_cargo_test_binary(exe) {
        return false;
    }
    let path = canonicalize_existing_prefix(path);
    let real: Vec<PathBuf> = real
        .iter()
        .map(|r| canonicalize_existing_prefix(r))
        .collect();
    let temp = canonicalize_existing_prefix(temp_dir);
    let temp_is_real = real.contains(&temp);
    if !temp_is_real && path.starts_with(&temp) {
        return false;
    }
    real.iter().any(|r| path.starts_with(r))
}

/// A test process tried to delete under a real user location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealLocationProtected {
    pub path: PathBuf,
}

impl std::fmt::Display for RealLocationProtected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to delete {} from a test process: it is under a real user location (home, \
             application data, caches or ~/.claude). Point the test at a temporary directory.",
            self.path.display()
        )
    }
}

/// Refuse to delete `path` when this process is a cargo test binary and `path` is under a real
/// user location. See the module docs for why this exists alongside injected roots.
pub fn ensure_not_real_location_in_test(path: &Path) -> Result<(), RealLocationProtected> {
    let exe = std::env::current_exe().ok();
    if test_process_may_not_delete(
        path,
        &real_user_locations(),
        exe.as_deref(),
        &std::env::temp_dir(),
    ) {
        return Err(RealLocationProtected {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_EXE: &str = "/w/target/debug/deps/camerata_server-0123abcd";
    const APP_EXE: &str = "/Applications/Camerata.app/Contents/MacOS/camerata";

    fn real() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/Users/auditor/.claude"),
            PathBuf::from("/Users/auditor"),
            PathBuf::from("/Users/auditor/Library/Application Support"),
            PathBuf::from("/Users/auditor/Library/Caches"),
        ]
    }

    fn blocked(path: &str, exe: &str, temp: &str) -> bool {
        test_process_may_not_delete(
            Path::new(path),
            &real(),
            Some(Path::new(exe)),
            Path::new(temp),
        )
    }

    #[test]
    fn a_test_binary_may_not_delete_under_the_real_home_data_cache_or_claude_dirs() {
        let temp = "/var/folders/xx/T";
        for path in [
            "/Users/auditor",
            "/Users/auditor/Documents/Repos/client/app",
            "/Users/auditor/.claude/projects/-Users-auditor/abc.jsonl",
            "/Users/auditor/Library/Application Support/camerata/scan-cache.json",
            "/Users/auditor/Library/Caches/camerata/app-1.cache.json",
        ] {
            assert!(blocked(path, TEST_EXE, temp), "{path} must be protected");
        }
    }

    #[test]
    fn a_test_binary_may_delete_inside_its_own_temp_directory() {
        let temp = "/var/folders/xx/T";
        assert!(!blocked(
            "/var/folders/xx/T/.tmpAbC/work/acme/app",
            TEST_EXE,
            temp
        ));
        assert!(!blocked("/private/tmp/scratch/x", TEST_EXE, "/private/tmp"));
    }

    #[test]
    fn the_product_binary_is_never_restricted_by_this_guard() {
        // The real app deletes real locations: that is the feature. Only test binaries are held.
        assert!(!blocked(
            "/Users/auditor/.claude/projects/-x/abc.jsonl",
            APP_EXE,
            "/var/folders/xx/T"
        ));
        assert!(!test_process_may_not_delete(
            Path::new("/Users/auditor/x"),
            &real(),
            None,
            Path::new("/tmp")
        ));
    }

    #[test]
    fn a_temp_dir_that_is_inside_home_does_not_exempt_home_itself() {
        // TMPDIR=/Users/auditor/tmp: paths under it are still exempt (it is scratch)...
        assert!(!blocked(
            "/Users/auditor/tmp/job/x",
            TEST_EXE,
            "/Users/auditor/tmp"
        ));
        // ...but the rest of home is not.
        assert!(blocked(
            "/Users/auditor/Documents/x",
            TEST_EXE,
            "/Users/auditor/tmp"
        ));
        // And a TMPDIR that IS the home directory exempts nothing at all.
        assert!(blocked(
            "/Users/auditor/Documents/x",
            TEST_EXE,
            "/Users/auditor"
        ));
    }

    /// The proof the backstop is live in THIS test binary: with the real locations, a path under
    /// the real home is refused, and nothing is created or removed by asking.
    #[test]
    fn this_test_process_is_refused_a_path_under_the_real_home() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        // Deliberately a path that does not exist: the guard decides from the path alone.
        let probe = home
            .join("camerata-guard-probe-that-does-not-exist")
            .join("acme");
        let refused = ensure_not_real_location_in_test(&probe)
            .expect_err("a test process must be refused under the real home");
        assert_eq!(refused.path, probe);
        assert!(!probe.exists());

        let claude = home
            .join(".claude")
            .join("projects")
            .join("-x")
            .join("s.jsonl");
        assert!(ensure_not_real_location_in_test(&claude).is_err());
    }

    #[test]
    fn this_test_process_may_delete_inside_a_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_not_real_location_in_test(&tmp.path().join("work/acme/app")).is_ok());
    }

    #[test]
    fn sandboxed_roots_live_entirely_under_the_given_root() {
        let root = Path::new("/sandbox");
        let roots = StorageRoots::sandboxed(root);
        for p in [
            &roots.home,
            &roots.data_dir,
            &roots.cli_state_dir,
            &roots.claude_projects_dir,
            &roots.exe_dir,
        ] {
            assert!(p.as_ref().unwrap().starts_with(root), "{p:?}");
        }
    }

    /// A new store cannot be added to `AppState` without someone deciding whether it can hold
    /// client code: every file the state opens under its data directory must be named in
    /// [`DATA_DIR_ENTRIES`], and every entry (bar the tooling directory, opened elsewhere) must
    /// still be opened by something.
    #[test]
    fn known_storage_locations_covers_every_store_opened_in_from_env() {
        let lib = include_str!("../lib.rs");
        let start = lib
            .find("fn open_json_stores")
            .expect("lib.rs defines open_json_stores");
        let end = lib[start..]
            .find("/// One enforced gate rule")
            .map(|i| start + i)
            .expect("the AppState impl is followed by RuleDto");
        let region = &lib[start..end];

        let mut opened = std::collections::BTreeSet::new();
        for part in region.split("dir.join(\"").skip(1) {
            if let Some((name, _)) = part.split_once('"') {
                opened.insert(name.to_string());
            }
        }
        assert!(
            opened.len() >= 15,
            "the scan found too few stores ({opened:?}); did the opening code move?"
        );
        let known: std::collections::BTreeSet<String> = DATA_DIR_ENTRIES
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();
        for name in &opened {
            assert!(
                known.contains(name),
                "AppState opens `{name}` but DATA_DIR_ENTRIES does not say whether it can hold \
                 client code; add it with a Treatment"
            );
        }
        for name in &known {
            assert!(
                name == "tooling" || opened.contains(name),
                "DATA_DIR_ENTRIES lists `{name}` but AppState no longer opens it"
            );
        }
    }

    #[test]
    fn the_default_roots_know_nothing_outside_the_checkout() {
        let roots = StorageRoots::default();
        assert!(known_storage_locations(&roots).is_empty());
    }

    #[test]
    fn known_locations_cover_the_data_dir_table_the_cli_cache_and_claude_sessions() {
        let roots = StorageRoots::sandboxed(Path::new("/sandbox"));
        let known = known_storage_locations(&roots);
        assert_eq!(known.len(), DATA_DIR_ENTRIES.len() + 2);
        let treatment = |p: &str| {
            known
                .iter()
                .find(|k| k.path == Path::new(p))
                .map(|k| k.treatment)
        };
        assert_eq!(
            treatment("/sandbox/data/onboarding-draft.json"),
            Some(Treatment::Kept)
        );
        assert_eq!(
            treatment("/sandbox/data/scan-cache.json"),
            Some(Treatment::Purged)
        );
        assert_eq!(
            treatment("/sandbox/data/projects.json"),
            Some(Treatment::NoClientCode)
        );
        assert_eq!(
            treatment("/sandbox/cli-state/camerata"),
            Some(Treatment::Purged)
        );
    }
}
