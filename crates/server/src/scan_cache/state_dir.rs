//! Where the CLI keeps its per-repository bookkeeping, and the key that names it.
//!
//! `camerata inspect` caches an incremental-scan manifest and writes a pipeline-integrity ledger
//! BETWEEN headless runs. Both live in a per-user directory, never in the scanned repository
//! (the account of why is in the CLI's `inspect_cmd` module docs), under a key derived from the
//! repository's canonical path: `<state dir>/<name>-<fnv32>.cache.json` and `….ledger.json`.
//!
//! This module is the ONE implementation of that location and key. It lives in the server crate
//! (the CLI re-exports it) because source deletion has to find exactly the files the CLI wrote
//! for a checkout: two copies of the key function would be two chances to disagree, and a
//! disagreement means a client's findings stay on disk after "delete".
//!
//! # Test seam
//!
//! `CAMERATA_STATE_DIR`, when set and non-blank, replaces the platform cache directory
//! (`~/Library/Caches` on macOS, `$XDG_CACHE_HOME` or `~/.cache` elsewhere); `camerata/` is
//! still appended, mirroring `CAMERATA_DATA_DIR` (see [`crate::app_dirs`]). It is read ONLY when
//! set, so a normal install is unchanged.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Environment variable that replaces the platform cache directory. See the module docs.
pub const STATE_DIR_ENV: &str = "CAMERATA_STATE_DIR";

/// The per-user directory where Camerata keeps its own bookkeeping for headless runs.
///
/// Resolution follows the platform convention, with a temp-dir fallback so a missing `HOME`
/// degrades to a working (if non-persistent) cache rather than failing the inspection.
pub fn dir() -> PathBuf {
    dir_from(
        std::env::var_os(STATE_DIR_ENV).as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
    )
}

/// The pure resolution behind [`dir`], so every branch is testable without touching the
/// process environment.
pub fn dir_from(
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
) -> PathBuf {
    let non_blank = |v: &OsStr| !v.to_string_lossy().trim().is_empty();
    let base = state_override.filter(|v| non_blank(v)).map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = {
        let _ = xdg_cache_home;
        base.or_else(|| home.map(|h| PathBuf::from(h).join("Library/Caches")))
    };
    #[cfg(not(target_os = "macos"))]
    let base = base
        .or_else(|| xdg_cache_home.filter(|v| non_blank(v)).map(PathBuf::from))
        .or_else(|| home.map(|h| PathBuf::from(h).join(".cache")));

    base.unwrap_or_else(std::env::temp_dir).join("camerata")
}

/// A stable, filesystem-safe key identifying one scanned repository.
///
/// Shaped `<readable-name>-<hash>` (e.g. `hearth-3f2a9c14`) rather than a bare hash so the
/// cache directory stays human-debuggable: an operator wondering which cache belongs to which
/// repo can read it off the filename instead of re-deriving a digest.
///
/// The hash is FNV-1a over the absolute path, hand-rolled on purpose. `DefaultHasher` is
/// explicitly NOT stable across Rust releases, so using it would silently invalidate every
/// cache on a toolchain upgrade. A cache key needs determinism, not cryptographic strength.
///
/// The path is canonicalised when it exists; callers that delete the checkout must compute the
/// key BEFORE removing it, or the key would be derived from the un-canonicalised spelling.
pub fn repo_state_key(repo: &Path) -> String {
    let abs = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in abs.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        // The multiplier is kept byte-for-byte from the original CLI implementation so every
        // key already on disk stays valid (only its low 32 bits reach the key).
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    let name: String = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    // Only the low 32 bits are kept: the key is a readable label plus a disambiguator, not an
    // identity, and the truncation is what the existing on-disk files were named with.
    format!("{name}-{:08x}", hash as u32)
}

/// Where the incremental-scan manifest for `repo` is cached between headless runs.
pub fn manifest_cache_path(repo: &Path) -> PathBuf {
    dir().join(format!("{}.cache.json", repo_state_key(repo)))
}

/// Where the completed scan's pipeline-integrity ledger for `repo` is persisted. Counts only,
/// no code; sits beside [`manifest_cache_path`].
pub fn ledger_path(repo: &Path) -> PathBuf {
    dir().join(format!("{}.ledger.json", repo_state_key(repo)))
}

/// Both bookkeeping files for `repo` under an explicit state directory, for callers (source
/// deletion) that must name them without reading the process environment.
pub fn repo_state_files_in(state_dir: &Path, repo: &Path) -> [PathBuf; 2] {
    let key = repo_state_key(repo);
    [
        state_dir.join(format!("{key}.cache.json")),
        state_dir.join(format!("{key}.ledger.json")),
    ]
}

/// The key of a PROJECT of several repositories inspected together: one cache and one ledger for
/// the whole set, not one pair per repo. Shaped `project-<count>-<hash>`; the hash is FNV-1a over
/// the repos' canonical paths in sorted order, so the order the repos were named in does not
/// change it. One repo is NOT a project: use [`repo_state_key`] for that, so every key already
/// on disk stays valid.
pub fn project_state_key(repos: &[PathBuf]) -> String {
    let mut paths: Vec<String> = repos
        .iter()
        .map(|r| {
            r.canonicalize()
                .unwrap_or_else(|_| r.clone())
                .to_string_lossy()
                .to_string()
        })
        .collect();
    paths.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &paths {
        for byte in path.as_bytes().iter().chain(std::iter::once(&0u8)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }
    format!("project-{}-{:08x}", paths.len(), hash as u32)
}

/// Where the incremental-scan manifest of a multi-repo project is cached ([`project_state_key`]).
pub fn project_manifest_cache_path(repos: &[PathBuf]) -> PathBuf {
    dir().join(format!("{}.cache.json", project_state_key(repos)))
}

/// Where the pipeline-integrity ledger of a multi-repo project is persisted.
pub fn project_ledger_path(repos: &[PathBuf]) -> PathBuf {
    dir().join(format!("{}.ledger.json", project_state_key(repos)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_key_ignores_the_order_the_repos_were_named_in_and_differs_from_a_repo_key() {
        let a = PathBuf::from("/clients/acme/api");
        let b = PathBuf::from("/clients/acme/web");
        assert_eq!(
            project_state_key(&[a.clone(), b.clone()]),
            project_state_key(&[b.clone(), a.clone()])
        );
        assert_ne!(
            project_state_key(&[a.clone(), b.clone()]),
            project_state_key(&[a.clone()])
        );
        assert_ne!(
            project_state_key(&[a.clone(), b.clone()]),
            project_state_key(&[a.clone(), PathBuf::from("/clients/other/web")])
        );
        let key = project_state_key(&[a, b]);
        assert!(key.starts_with("project-2-"), "{key}");
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_ne!(
            project_manifest_cache_path(&[PathBuf::from("/x/a"), PathBuf::from("/x/b")]),
            project_ledger_path(&[PathBuf::from("/x/a"), PathBuf::from("/x/b")])
        );
    }

    #[test]
    fn the_override_replaces_the_platform_base_and_camerata_is_appended() {
        let got = dir_from(
            Some(OsStr::new("/sandbox/state")),
            Some(OsStr::new("/home/u")),
            Some(OsStr::new("/home/u/.xdg")),
        );
        assert_eq!(got, PathBuf::from("/sandbox/state/camerata"));
    }

    #[test]
    fn a_blank_override_is_ignored() {
        let with = dir_from(Some(OsStr::new("   ")), Some(OsStr::new("/home/u")), None);
        let without = dir_from(None, Some(OsStr::new("/home/u")), None);
        assert_eq!(with, without);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_is_library_caches() {
        assert_eq!(
            dir_from(
                None,
                Some(OsStr::new("/Users/auditor")),
                Some(OsStr::new("/ignored"))
            ),
            PathBuf::from("/Users/auditor/Library/Caches/camerata")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_default_prefers_xdg_cache_home_then_dot_cache() {
        assert_eq!(
            dir_from(
                None,
                Some(OsStr::new("/home/u")),
                Some(OsStr::new("/x/cache"))
            ),
            PathBuf::from("/x/cache/camerata")
        );
        assert_eq!(
            dir_from(None, Some(OsStr::new("/home/u")), None),
            PathBuf::from("/home/u/.cache/camerata")
        );
    }

    #[test]
    fn no_home_degrades_to_the_temp_dir() {
        assert_eq!(
            dir_from(None, None, None),
            std::env::temp_dir().join("camerata")
        );
    }

    /// The key is what names files already on users' disks, so its algorithm is pinned: a
    /// change here would orphan every existing cache and, worse, make deletion miss them.
    #[test]
    fn repo_state_key_is_pinned_to_the_published_algorithm() {
        assert_eq!(
            repo_state_key(Path::new("/clients/one/web")),
            "web-d646b970"
        );
    }

    #[test]
    fn repo_state_files_are_the_cache_and_the_ledger_under_the_given_dir() {
        let files =
            repo_state_files_in(Path::new("/state/camerata"), Path::new("/clients/one/web"));
        assert_eq!(
            files,
            [
                PathBuf::from("/state/camerata/web-d646b970.cache.json"),
                PathBuf::from("/state/camerata/web-d646b970.ledger.json"),
            ]
        );
    }
}
