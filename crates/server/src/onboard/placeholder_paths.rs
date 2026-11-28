//! The paths a row carries when it is about no file: a project-level finding or a scan preview.
//! Defined once. The producers that write them (`scan_tools`, `ai_audit::digest_parse`) and the
//! evidence check that reads them (`evidence_check`) all use these, so a row on one of them is
//! never mistaken for a row on a file that is missing.

/// A finding about the repository as a whole, not any one file.
pub const REPO_PATH: &str = "(repo)";

/// A note about the scan-time preview pass itself.
pub const SCAN_PREVIEW_PATH: &str = "(scan preview)";

/// The dependency scanner's path for a result block that names no lockfile.
pub const UNKNOWN_LOCKFILE_PATH: &str = "(unknown lockfile)";

/// Every placeholder path.
pub const ALL: [&str; 3] = [REPO_PATH, SCAN_PREVIEW_PATH, UNKNOWN_LOCKFILE_PATH];

/// Whether `path` names no file: empty, or one of the placeholders.
pub fn is_placeholder(path: &str) -> bool {
    let path = path.trim();
    path.is_empty() || ALL.contains(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholders_and_an_empty_path_name_no_file_and_nothing_else_does() {
        assert!(is_placeholder(""));
        assert!(is_placeholder("  "));
        assert!(is_placeholder(REPO_PATH));
        assert!(is_placeholder(SCAN_PREVIEW_PATH));
        assert!(is_placeholder(UNKNOWN_LOCKFILE_PATH));
        assert!(!is_placeholder("src/app.ts"));
        assert!(!is_placeholder("(other)"));
    }
}
