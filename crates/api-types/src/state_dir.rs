//! Where Camerata keeps its own per-user bookkeeping, and the test seam that moves it.
//!
//! The one implementation of that location (the CLI's scan cache, the run store and the
//! per-call system prompt files all live under it). `camerata-server` re-exports it from
//! `scan_cache::state_dir`.
//!
//! STUB: the red half of issue 342. The defaults below are still the platform cache folder.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

mod migrate;

pub use migrate::{migrate, MigrationOutcome, MoveMethod};

/// Environment variable that replaces the platform base directory. See the module docs.
pub const STATE_DIR_ENV: &str = "CAMERATA_STATE_DIR";

/// Which platform convention a default follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// The process environment the resolution reads, captured once so it is testable.
#[derive(Debug, Clone, Default)]
pub struct StateEnv {
    pub state_override: Option<OsString>,
    pub home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub xdg_cache_home: Option<OsString>,
}

impl StateEnv {
    pub fn from_process() -> Self {
        Self {
            state_override: std::env::var_os(STATE_DIR_ENV),
            home: std::env::var_os("HOME"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            xdg_cache_home: std::env::var_os("XDG_CACHE_HOME"),
        }
    }
}

/// What the environment resolves to before any migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Override(PathBuf),
    Default { new: PathBuf, old: PathBuf },
}

pub fn resolve(platform: Platform, env: &StateEnv) -> Resolved {
    let _ = platform;
    Resolved::Override(dir_from(
        env.state_override.as_deref(),
        env.home.as_deref(),
        env.xdg_cache_home.as_deref(),
    ))
}

pub fn dir() -> PathBuf {
    dir_from(
        std::env::var_os(STATE_DIR_ENV).as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
    )
}

pub fn cache_dir() -> PathBuf {
    dir()
}

pub fn dir_from(
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
) -> PathBuf {
    default_dir_for(Platform::current(), state_override, home, xdg_data_home)
}

pub fn default_dir_for(
    platform: Platform,
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
) -> PathBuf {
    cache_dir_for(platform, state_override, home, xdg_data_home)
}

pub fn cache_dir_for(
    platform: Platform,
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
) -> PathBuf {
    let non_blank = |v: &OsStr| !v.to_string_lossy().trim().is_empty();
    let base = state_override.filter(|v| non_blank(v)).map(PathBuf::from);
    let base = match platform {
        Platform::MacOs => base.or_else(|| home.map(|h| PathBuf::from(h).join("Library/Caches"))),
        Platform::Other => base
            .or_else(|| xdg_cache_home.filter(|v| non_blank(v)).map(PathBuf::from))
            .or_else(|| home.map(|h| PathBuf::from(h).join(".cache"))),
    };
    base.unwrap_or_else(std::env::temp_dir).join("camerata")
}

#[allow(dead_code)]
fn unused(_: &Path) {}

#[cfg(test)]
mod tests;
