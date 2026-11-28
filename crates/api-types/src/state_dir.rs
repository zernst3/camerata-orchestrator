//! Where Camerata keeps its own per-user bookkeeping, and the test seam that moves it.
//!
//! The one implementation of that location (the CLI's scan cache, the run store and the
//! per-call system prompt files all live under it). `camerata-server` re-exports it from
//! `scan_cache::state_dir`.
//!
//! `CAMERATA_STATE_DIR`, when set and non-blank, replaces the platform cache directory
//! (`~/Library/Caches` on macOS, `$XDG_CACHE_HOME` or `~/.cache` elsewhere); `camerata/` is
//! still appended. It is read ONLY when set, so a normal install is unchanged.

use std::ffi::OsStr;
use std::path::PathBuf;

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
