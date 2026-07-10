use std::path::PathBuf;

/// What [`crate::scaffold_skeleton`] actually did, for the caller (the orchestrator,
/// eventually the Part-2 server endpoint) to report back to a human.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScaffoldOutcome {
    /// Every file written, as absolute paths under `target_dir`, in the order they
    /// were materialized.
    pub files_written: Vec<PathBuf>,
    /// The derived snake_case package/crate name substituted into the skeleton
    /// (`Cargo.toml`'s `[package].name`, the `[[bin]]` name, `Dioxus.toml`'s
    /// `application.name`, and the Rust `mod`/crate references in the skeleton's own
    /// `src/main.rs`).
    pub package_name: String,
    /// Human-readable notes about what shipped and what didn't (e.g. that the
    /// auto-capture reporter posts to a capture endpoint the skeleton itself does not
    /// implement — that's Part 2).
    pub notes: Vec<String>,
}
