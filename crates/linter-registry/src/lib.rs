//! camerata-linter-registry: canonical linter rule-id registry and citation validator.
//!
//! # Purpose
//!
//! Rule sources in the camerata corpus cite real linter rule IDs (e.g.
//! `clippy::unwrap_used`, `Ruff E722`, `@typescript-eslint/no-explicit-any`).
//! This crate answers the question: **does this rule ID actually exist in the
//! named tool?**
//!
//! The registry is curated static data, not a live tool query. It covers the
//! IDs the corpus actually cites; the coverage gaps section below lists what
//! is intentionally omitted.
//!
//! # Supported tools
//!
//! | Tool key (passed to [`validate_citation`]) | Description |
//! |---|---|
//! | `"clippy"` | Rust Clippy lints (clippy:: prefix) |
//! | `"ruff"` | Ruff Python linter (all rule categories) |
//! | `"eslint"` | ESLint core rules |
//! | `"typescript-eslint"` | @typescript-eslint/ rules |
//! | `"react-hooks"` | eslint-plugin-react-hooks rules |
//! | `"golangci-lint"` | golangci-lint linter names |
//! | `"rubocop"` | RuboCop cop names |
//! | `"checkstyle"` | Checkstyle check names |
//! | `"spotbugs"` | SpotBugs bug pattern IDs |
//! | `"roslyn"` | Roslyn CA quality rules |
//! | `"roslyn-style"` | Roslyn IDE style rules |
//! | `"bandit"` | Bandit Python security tool |
//! | `"sqlfluff"` | SQLFluff SQL linter rules |
//!
//! # Coverage gaps (intentional)
//!
//! - **Clippy**: Only the ~30 lints the corpus cites are listed here. The full
//!   registry contains 700+ lints. Source: <https://rust-lang.github.io/rust-clippy/master/>
//! - **Ruff**: Only the E/W/B/S/BLE categories that appear in the corpus. The
//!   full ruleset has 800+ codes. Source: <https://docs.astral.sh/ruff/rules/>
//! - **ESLint**: Only the `no-*` and `react-hooks/*` rules cited. The full
//!   ESLint core has 280+ rules. Source: <https://eslint.org/docs/latest/rules/>
//! - **golangci-lint**: Linter names only (errcheck, staticcheck). Staticcheck
//!   SA-codes (SA1xxx, SA4xxx, etc.) are not enumerated. Source:
//!   <https://golangci-lint.run/usage/linters/>
//! - **RuboCop**: Only Brakeman-backed rules cited. The full RuboCop registry
//!   has 400+ cops. Source: <https://docs.rubocop.org/rubocop/>
//! - **Checkstyle**: Only the 7 checks cited in the corpus. Source:
//!   <https://checkstyle.sourceforge.io/checks.html>
//! - **SpotBugs**: Only resource-leak and null-dereference patterns cited.
//!   Source: <https://spotbugs.readthedocs.io/en/latest/bugDescriptions.html>
//! - **Roslyn CA / IDE**: Only the 5 IDs cited in the corpus. Source:
//!   <https://learn.microsoft.com/dotnet/fundamentals/code-analysis/quality-rules/>
//! - **Bandit**: B105/B106/B107 (hardcoded password). Source:
//!   <https://bandit.readthedocs.io/en/latest/plugins/>
//! - **sqlfluff**: Rule names only; the corpus cites no specific sqlfluff IDs
//!   yet, so the registry contains a representative sample. Source:
//!   <https://docs.sqlfluff.com/en/stable/reference/rules.html>

pub mod registry;
pub mod report;

pub use registry::{CitationStatus, LinterRegistry};
pub use report::generate_report;

/// Validate a single linter citation.
///
/// # Arguments
///
/// * `tool`    — The tool key (case-insensitive). See the table in the crate
///               docs for accepted keys.
/// * `rule_id` — The rule identifier within that tool. Compared
///               case-insensitively after normalisation (leading/trailing
///               whitespace stripped).
///
/// # Returns
///
/// A [`CitationStatus`] indicating whether the (tool, rule_id) pair resolves
/// to a known rule, is not found in the known-good list, or names an
/// unrecognised tool.
///
/// # Example
///
/// ```rust
/// use camerata_linter_registry::{validate_citation, CitationStatus};
///
/// assert_eq!(validate_citation("clippy", "unwrap_used"), CitationStatus::Resolves);
/// assert_eq!(validate_citation("clippy", "made_up_lint"), CitationStatus::NotFound);
/// assert_eq!(validate_citation("blarg", "anything"), CitationStatus::UnknownTool);
/// ```
pub fn validate_citation(tool: &str, rule_id: &str) -> CitationStatus {
    LinterRegistry::global().validate(tool, rule_id)
}
