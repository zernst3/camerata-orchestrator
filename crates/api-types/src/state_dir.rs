//! Where Camerata keeps its own per-user bookkeeping, and the test seam that moves it.
//!
//! The ONE implementation of that location. The CLI's scan cache and ledgers, the run store and
//! the agent finder's rooms live under [`dir`]; the per-call system prompt files live under
//! [`cache_dir`]. `camerata-server` re-exports it from `scan_cache::state_dir`.
//!
//! # Where it is
//!
//! [`dir`] is the platform's per-user application data folder plus `camerata/`:
//! `~/Library/Application Support/camerata` on macOS, `$XDG_DATA_HOME/camerata` or
//! `~/.local/share/camerata` elsewhere. Saved runs are days of paid model work, so they must not
//! sit in a cache folder the system may empty (macOS clears `~/Library/Caches` under disk
//! pressure, issue 342). On a default install this is the same folder as the cockpit's own data
//! root (`camerata-server`'s `app_dirs::camerata_dir`); none of the names below collide with its
//! stores.
//!
//! [`cache_dir`] is the platform cache folder plus `camerata/` (`~/Library/Caches/camerata` on
//! macOS, `$XDG_CACHE_HOME/camerata` or `~/.cache/camerata` elsewhere), where every earlier
//! version kept everything. Only files that are truly disposable stay there.
//!
//! # The override
//!
//! `CAMERATA_STATE_DIR`, when set and non-blank, replaces the platform base of BOTH folders;
//! `camerata/` is still appended. Nothing is ever migrated while it is set. It is read ONLY when
//! set, so a normal install is unchanged by it.
//!
//! # The move from the cache folder
//!
//! The first time a process resolves [`dir`] on the default location, the saved state of an
//! earlier version is moved out of the cache folder ([`migrate`]). The decision is made once
//! per process and kept, so every caller in the process agrees on one folder. When the move
//! cannot be made, the cache folder stays in use, untouched, with one log line saying why.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

mod migrate;

pub use migrate::{migrate, MigrationOutcome, MoveMethod};

/// Environment variable that replaces the platform base directory. See the module docs.
pub const STATE_DIR_ENV: &str = "CAMERATA_STATE_DIR";

/// Which platform convention a default follows. A parameter rather than a `cfg` so every branch
/// is tested on every machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    /// Linux and every other Unix: the XDG base directory convention.
    Other,
}

impl Platform {
    /// The platform this build runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// The process environment the resolution reads, captured once so the resolution is pure.
#[derive(Debug, Clone, Default)]
pub struct StateEnv {
    pub state_override: Option<OsString>,
    pub home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub xdg_cache_home: Option<OsString>,
}

impl StateEnv {
    /// The variables as this process sees them now.
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
    /// `CAMERATA_STATE_DIR` names the folder. Nothing is migrated.
    Override(PathBuf),
    /// The default: the app data folder, with the cache folder an earlier version used.
    Default { new: PathBuf, old: PathBuf },
}

/// The pure decision behind [`dir`].
pub fn resolve(platform: Platform, env: &StateEnv) -> Resolved {
    if let Some(base) = non_blank(env.state_override.as_deref()) {
        return Resolved::Override(PathBuf::from(base).join("camerata"));
    }
    Resolved::Default {
        new: default_dir_for(
            platform,
            None,
            env.home.as_deref(),
            env.xdg_data_home.as_deref(),
        ),
        old: cache_dir_for(
            platform,
            None,
            env.home.as_deref(),
            env.xdg_cache_home.as_deref(),
        ),
    }
}

/// The per-user directory where Camerata keeps its own bookkeeping: saved runs, the CLI's scan
/// cache and ledgers. See the module docs for the location and the one time move.
///
/// A missing `HOME` degrades to the temp dir (a working, if non-persistent, folder) rather than
/// failing the inspection.
pub fn dir() -> PathBuf {
    match resolve(Platform::current(), &StateEnv::from_process()) {
        Resolved::Override(dir) => dir,
        Resolved::Default { new, old } => migrate::settled(&new, &old),
    }
}

/// The per-user folder for files that are truly disposable (the per-call system prompt files):
/// the platform cache folder, or the override when it is set. Nothing here is ever migrated, and
/// a system that empties it loses nothing a run needs.
pub fn cache_dir() -> PathBuf {
    match resolve(Platform::current(), &StateEnv::from_process()) {
        Resolved::Override(dir) => dir,
        Resolved::Default { old, .. } => old,
    }
}

/// The pure default behind [`dir`] for this platform, without the move.
pub fn dir_from(
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
) -> PathBuf {
    default_dir_for(Platform::current(), state_override, home, xdg_data_home)
}

/// The app data folder for `platform`: the override, else
/// `~/Library/Application Support` (macOS) or an absolute `$XDG_DATA_HOME` or
/// `~/.local/share` (elsewhere), else the temp dir; `camerata/` appended.
pub fn default_dir_for(
    platform: Platform,
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
) -> PathBuf {
    let base = non_blank(state_override).map(PathBuf::from);
    let base = match platform {
        Platform::MacOs => base
            .or_else(|| home.map(|h| PathBuf::from(h).join("Library").join("Application Support"))),
        // The XDG spec: a relative value is invalid and is ignored.
        Platform::Other => base
            .or_else(|| {
                non_blank(xdg_data_home)
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
            })
            .or_else(|| home.map(|h| PathBuf::from(h).join(".local").join("share"))),
    };
    base.unwrap_or_else(std::env::temp_dir).join("camerata")
}

/// The cache folder for `platform`, exactly as every earlier version resolved its whole state
/// folder (the move reads from here, so this must not drift): the override, else
/// `~/Library/Caches` (macOS) or `$XDG_CACHE_HOME` or `~/.cache` (elsewhere), else the temp
/// dir; `camerata/` appended.
pub fn cache_dir_for(
    platform: Platform,
    state_override: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
) -> PathBuf {
    let base = non_blank(state_override).map(PathBuf::from);
    let base = match platform {
        Platform::MacOs => base.or_else(|| home.map(|h| PathBuf::from(h).join("Library/Caches"))),
        Platform::Other => base
            .or_else(|| non_blank(xdg_cache_home).map(PathBuf::from))
            .or_else(|| home.map(|h| PathBuf::from(h).join(".cache"))),
    };
    base.unwrap_or_else(std::env::temp_dir).join("camerata")
}

fn non_blank(v: Option<&OsStr>) -> Option<&OsStr> {
    v.filter(|v| !v.to_string_lossy().trim().is_empty())
}

/// True when `exe` is a cargo test binary (`target/<profile>/deps/<name>-<hash>`).
fn is_cargo_test_binary(exe: &Path) -> bool {
    exe.parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps")
}

#[cfg(test)]
mod tests;
