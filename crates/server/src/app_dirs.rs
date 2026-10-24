//! Where Camerata keeps its per-user stores, with one seam for tests and sandboxes.
//!
//! Every store (`projects.json`, `inspections/`, `scan-cache.json`, ...) lives under
//! `<data_dir>/camerata/`. `data_dir` is the platform's per-user application-data directory
//! (`dirs::data_dir()`), unless `CAMERATA_DATA_DIR` is set to a non-empty path, in which case
//! that path replaces it. The override is read ONLY when set, so a normal install is unchanged.
//!
//! The override exists so a process that must not touch the real stores (an end-to-end test, a
//! sandboxed run) can point the whole store tree at a temporary directory. The tooling
//! directory (`tool_provisioning::tooling_dir`) deliberately does NOT follow it: those are
//! shared tool binaries, never client data, and re-provisioning them per sandbox would cost
//! minutes.

use std::path::PathBuf;

/// Environment variable that replaces the platform data directory. See the module docs.
pub const DATA_DIR_ENV: &str = "CAMERATA_DATA_DIR";

/// `CAMERATA_DATA_DIR` when set and non-blank, otherwise `dirs::data_dir()`.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_from(std::env::var_os(DATA_DIR_ENV).as_deref(), dirs::data_dir())
}

/// The pure resolution behind [`data_dir`], so it is testable without touching the process
/// environment.
fn data_dir_from(
    override_dir: Option<&std::ffi::OsStr>,
    platform: Option<PathBuf>,
) -> Option<PathBuf> {
    match override_dir {
        Some(v) if !v.to_string_lossy().trim().is_empty() => Some(PathBuf::from(v)),
        _ => platform,
    }
}

/// `<data_dir>/camerata`, the root every store shares.
pub fn camerata_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("camerata"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn override_wins_when_set() {
        let got = data_dir_from(Some(OsStr::new("/tmp/x")), Some(PathBuf::from("/real")));
        assert_eq!(got, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn blank_override_falls_back_to_the_platform_dir() {
        let platform = Some(PathBuf::from("/real"));
        assert_eq!(
            data_dir_from(Some(OsStr::new("  ")), platform.clone()),
            platform
        );
        assert_eq!(
            data_dir_from(Some(OsStr::new("")), platform.clone()),
            platform
        );
        assert_eq!(data_dir_from(None, platform.clone()), platform);
    }

    #[test]
    fn no_platform_dir_and_no_override_is_none() {
        assert_eq!(data_dir_from(None, None), None);
    }
}
