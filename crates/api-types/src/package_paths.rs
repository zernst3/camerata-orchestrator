//! The path of every file in the client package (the export ZIP), spelled once.
//!
//! The package has a fixed layout that outside tools read, so each path is a public contract:
//! `manifest.json` and `README.txt` at the top level (the manifest describes the whole ZIP, so
//! it cannot sit inside a folder of it), the PDF and the workbook beside them, every JSON file
//! under `json/` and every CSV file under `csv/`.
//!
//! Every crate that names a package file, whether to write it, find it in an archive, check it
//! or mention it in text for a client, names it through this module and never spells the path
//! as a literal (ARCH-SHARED-CONSTANT-1). Two forms exist because prose is written in
//! `concat!`/`format!` string literals, which cannot take a `const`: a `pkg_*!` macro that
//! expands to the literal (usable inside `concat!`), and a `const` of the same value for code.
//! The `const` is always built from the macro, so the two cannot differ. A source-grep test in
//! the server crate (`package_paths_guard`) fails the build if a path is spelled any other way.

/// The folder every JSON file of the package sits in, with its trailing slash.
#[macro_export]
macro_rules! pkg_json_dir {
    () => {
        "json/"
    };
}

/// The folder every CSV file of the package sits in, with its trailing slash.
#[macro_export]
macro_rules! pkg_csv_dir {
    () => {
        "csv/"
    };
}

/// The package index with a checksum for each file. Top level on purpose.
#[macro_export]
macro_rules! pkg_manifest_json {
    () => {
        "manifest.json"
    };
}

/// The plain-text guide to the files. Top level.
#[macro_export]
macro_rules! pkg_readme_txt {
    () => {
        "README.txt"
    };
}

/// The whole inspection in one file: the full nested export (rows, provenance, counts, the
/// grouping tree and the fixes).
#[macro_export]
macro_rules! pkg_inspection_json {
    () => {
        "json/inspection.json"
    };
}

/// Every decision record the report was reviewed with.
#[macro_export]
macro_rules! pkg_decisions_json {
    () => {
        "json/decisions.json"
    };
}

/// The flat table of every row that was not ruled out, as JSON.
#[macro_export]
macro_rules! pkg_all_findings_json {
    () => {
        "json/all-findings.json"
    };
}

/// The same flat table as CSV.
#[macro_export]
macro_rules! pkg_all_findings_csv {
    () => {
        "csv/all-findings.csv"
    };
}

/// One ticket for each fix, for an issue-tracker importer.
#[macro_export]
macro_rules! pkg_tickets_csv {
    () => {
        "csv/tickets.csv"
    };
}

/// The operator's triage log (internal package only).
#[macro_export]
macro_rules! pkg_triage_log_csv {
    () => {
        "csv/triage-log.csv"
    };
}

/// See [`pkg_json_dir!`].
pub const JSON_DIR: &str = pkg_json_dir!();
/// See [`pkg_csv_dir!`].
pub const CSV_DIR: &str = pkg_csv_dir!();
/// See [`pkg_manifest_json!`].
pub const MANIFEST_JSON: &str = pkg_manifest_json!();
/// See [`pkg_readme_txt!`].
pub const README_TXT: &str = pkg_readme_txt!();
/// See [`pkg_inspection_json!`].
pub const INSPECTION_JSON: &str = pkg_inspection_json!();
/// See [`pkg_decisions_json!`].
pub const DECISIONS_JSON: &str = pkg_decisions_json!();
/// See [`pkg_all_findings_json!`].
pub const ALL_FINDINGS_JSON: &str = pkg_all_findings_json!();
/// See [`pkg_all_findings_csv!`].
pub const ALL_FINDINGS_CSV: &str = pkg_all_findings_csv!();
/// See [`pkg_tickets_csv!`].
pub const TICKETS_CSV: &str = pkg_tickets_csv!();
/// See [`pkg_triage_log_csv!`].
pub const TRIAGE_LOG_CSV: &str = pkg_triage_log_csv!();

/// The JSON file of each workbook table that code names on its own.
pub const DEVIATIONS_JSON: &str = concat!(pkg_json_dir!(), "deviations.json");
/// See [`DEVIATIONS_JSON`].
pub const FALSE_POSITIVES_JSON: &str = concat!(pkg_json_dir!(), "false-positives.json");
/// See [`DEVIATIONS_JSON`].
pub const DEPENDENCIES_JSON: &str = concat!(pkg_json_dir!(), "dependencies.json");

/// The path of the JSON file for a workbook table (`dataset` is the table's name, such as
/// `deviations`).
pub fn json_path(dataset: &str) -> String {
    format!("{JSON_DIR}{dataset}.json")
}

/// The path of the CSV file for a workbook table.
pub fn csv_path(dataset: &str) -> String {
    format!("{CSV_DIR}{dataset}.csv")
}

/// Every file a package can hold besides the PDF, the workbook and the per-table machine files
/// (`json_path`, `csv_path`), by path, for code that must name them all.
pub const NAMED_FILES: [&str; 5] = [
    INSPECTION_JSON,
    DECISIONS_JSON,
    TICKETS_CSV,
    MANIFEST_JSON,
    README_TXT,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_every_json_file_under_json_and_only_the_manifest_and_readme_at_the_top() {
        assert_eq!(INSPECTION_JSON, "json/inspection.json");
        assert_eq!(DECISIONS_JSON, "json/decisions.json");
        assert_eq!(ALL_FINDINGS_JSON, "json/all-findings.json");
        assert_eq!(MANIFEST_JSON, "manifest.json");
        assert_eq!(README_TXT, "README.txt");
        assert_eq!(json_path("coverage"), "json/coverage.json");
        assert_eq!(csv_path("coverage"), "csv/coverage.csv");
        for file in NAMED_FILES {
            let top_level = !file.contains('/');
            assert_eq!(
                top_level,
                file == MANIFEST_JSON || file == README_TXT,
                "{file}"
            );
        }
    }
}
