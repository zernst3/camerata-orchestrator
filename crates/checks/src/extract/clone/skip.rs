//! File-level guards (spec section 6, D1 to D7): which files are never tokenized, and why.
//!
//! The scan prunes most of these before the detector sees a tree, but the detector re-checks:
//! the CI tree differs from the scan tree, and a detector that trusted its caller would index
//! a vendored copy of the very code it is looking for duplicates of.

use std::collections::HashSet;

use super::{ItemTokens, SkipReason};
use crate::dead_code_checker::exclusions::{
    has_generated_marker, is_declaration_file, is_generated_path, is_test_file,
};
use crate::extract::{lang_for_path, SourceLang};

/// A file over this many tokens is a bundle or a generated table (D7).
pub(super) const MAX_FILE_TOKENS: usize = 20_000;

/// A single line longer than this is minified or bundled (D4).
const MAX_LINE_BYTES: usize = 1_000;

/// More tokens per line than this, on average, is minified or bundled (D4).
const MAX_TOKENS_PER_LINE: usize = 60;

/// Directory names for code that someone else wrote or a build produced (D2). `site-packages`,
/// `venv` and `.venv` are additions: an interpreter's installed packages are vendored code.
const VENDORED_SEGMENTS: &[&str] = &[
    "vendor",
    "third_party",
    "third-party",
    "external",
    "deps",
    "node_modules",
    "bower_components",
    "dist",
    "build",
    "out",
    "site-packages",
    "venv",
    ".venv",
];

/// Translation catalogs (D5).
const I18N_SEGMENTS: &[&str] = &[
    "locales",
    "i18n",
    "lang",
    "langs",
    "translations",
    "messages",
    "intl",
];

/// Schema migrations and seed data (D6).
const MIGRATION_SEGMENTS: &[&str] = &[
    "migrations",
    "migration",
    "alembic",
    "versions",
    "seeds",
    "seeders",
    "seed",
];

/// The language family a file is grouped in. Families never mix: a TypeScript client and a
/// Python server computing the same fee are not one family, because "write it once and import
/// it" has no meaning across languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum LangFamily {
    Ecma,
    Python,
    Rust,
}

impl LangFamily {
    pub(super) fn of(lang: SourceLang) -> Self {
        match lang {
            SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => Self::Ecma,
            SourceLang::Python => Self::Python,
            SourceLang::Rust => Self::Rust,
        }
    }

    /// A fixed per-family value mixed into every fingerprint, so equal token sequences of two
    /// families never share a bucket.
    pub(super) fn salt(self) -> u64 {
        match self {
            Self::Ecma => 0x0100_0000_0000_0001,
            Self::Python => 0x0200_0000_0000_0003,
            Self::Rust => 0x0300_0000_0000_0007,
        }
    }
}

/// The language of a file that passes every path guard, or why it does not.
pub(crate) fn classify_path(
    path: &str,
    emitted_js: &HashSet<&str>,
) -> Result<SourceLang, SkipReason> {
    let Some(lang) = lang_for_path(path) else {
        return Err(SkipReason::NotSource);
    };
    if is_test_file(path) {
        return Err(SkipReason::TestPath);
    }
    let lower = path.to_ascii_lowercase();
    let mut segments: Vec<&str> = lower.split('/').collect();
    let file = segments.pop().unwrap_or("");
    if is_generated_path(path)
        || is_declaration_file(path)
        || is_generated_name(file)
        || emitted_js.contains(path)
    {
        return Err(SkipReason::Generated);
    }
    if segments.iter().any(|s| VENDORED_SEGMENTS.contains(s)) {
        return Err(SkipReason::Vendored);
    }
    if file.contains(".min.") || file.contains(".bundle.") {
        return Err(SkipReason::Minified);
    }
    if segments.iter().any(|s| I18N_SEGMENTS.contains(s)) {
        return Err(SkipReason::I18nCatalog);
    }
    let in_migrations = segments.iter().any(|s| MIGRATION_SEGMENTS.contains(s))
        || segments
            .windows(2)
            .any(|pair| pair[0] == "db" && pair[1] == "migrate");
    if in_migrations {
        return Err(SkipReason::Generated);
    }
    Ok(lang)
}

/// Protobuf and gRPC output by name (D1): `x.pb.go`-style and `x_pb2.py` / `x_pb2_grpc.py`.
fn is_generated_name(file: &str) -> bool {
    file.contains(".pb.") || file.contains("_pb2.") || file.contains("_pb2_grpc.")
}

/// The content guards that run before any parse: a generated marker (D1) and a line too long to
/// be written by a person (D4).
pub(super) fn classify_content(content: &str) -> Option<SkipReason> {
    if has_generated_marker(content) {
        return Some(SkipReason::Generated);
    }
    if content.lines().any(|line| line.len() > MAX_LINE_BYTES) {
        return Some(SkipReason::Minified);
    }
    None
}

/// The guards that need the token stream: too many tokens (D7), too dense (D4), or a data
/// module (D5).
pub(super) fn classify_tokens(content: &str, items: &[ItemTokens]) -> Option<SkipReason> {
    let total: usize = items.iter().map(|item| item.tokens.len()).sum();
    if total > MAX_FILE_TOKENS {
        return Some(SkipReason::TooLarge);
    }
    let lines = content.lines().count().max(1);
    if total / lines > MAX_TOKENS_PER_LINE {
        return Some(SkipReason::Minified);
    }
    // A file whose every top-level item is a single object, array or dict literal is a lookup
    // table, a fixture list or a routes table.
    if !items.is_empty() && items.iter().all(|item| item.kind == "data") {
        return Some(SkipReason::I18nCatalog);
    }
    None
}
