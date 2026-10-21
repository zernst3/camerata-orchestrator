//! `DeadCodeChecker`: the deterministic detector for `ARCH-DEAD-CODE-1` (a source file that
//! nothing reaches). The algorithm, every false-positive trap and its guard (`G1` to `G30`)
//! are specified in `docs/design/2026-10-21_w29-detector-implementation.md` ("ARCH-DEAD-CODE-1:
//! the algorithm and its traps"); this module implements that version.
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/universal/arch-dead-code-1.toml` (`universal` domain).
//!
//! # Scope: whole files, not symbols
//!
//! Owner ruling, 2026-10-21 (`docs/OVERNIGHT_DECISIONS.md`, DECISIONS MADE): v1 reports a whole
//! SOURCE FILE that nothing reaches. Unused individual exports are a separate future rule with
//! its own id, because proving a symbol unused needs member-access analysis for namespace
//! imports, a framework-export allowlist and decorator awareness before it can meet the
//! standing law below. A finding here is a fact a reader confirms with one `grep` for the
//! file's name.
//!
//! # The law: false-negative, never false-positive
//!
//! Dead-code detection INVERTS the usual direction. Every other checker benefits from the
//! import resolver dropping the edges it cannot resolve; this one flags on UN-reachability, so
//! a file reached only through a dropped edge would look dead. Hence the rule never flags on
//! graph-unreachability alone. A file is reported only when it is unreachable from every
//! entry point AND its name appears in no live file's text AND no convention, manifest,
//! computed load or marker explains it.
//!
//! # How it decides
//!
//! 1. Candidates: TypeScript, JavaScript and Python source files that no exclusion covers
//!    (tests, declaration files, root-level and config-shaped files, generated files, files
//!    carrying a `keep-file:` marker, committed build output). Everything else is context:
//!    it carries edges and mentions, and is alive by definition.
//! 2. Roots: every context file, plus each candidate that is alive by itself (short or
//!    generic name, shebang or `__main__` guard).
//! 3. The mention guard: a candidate whose name appears as a whole word in a live file is
//!    alive. Run to a fixed point, so a mention inside an unreachable file proves nothing.
//! 4. Reachability over the import graph from the roots; the rest of the candidates are dead.
//! 5. Emission: one finding per dead file, rolled up to one finding per folder when every
//!    source file in the folder is dead.
//!
//! # Documented limits (all false-negative, by design)
//!
//! - Symbol-level findings (unused exports, dead private functions, unreferenced CSS classes).
//! - A file imported only by tests, stories or fixtures counts as live.
//! - A file whose name is short (under three characters) or generic, or whose stem coincides
//!   with any word in a live file, is never reported.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`]: the checker contract forbids emitting `low`. The corpus rule
//! declares `max_severity = "low"`, which the report export clamps down to (see
//! `report_export::clamp_to_rule_max_severity`).

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView};
use crate::extract::{self, SourceLang};

mod analysis;
mod conventions;
mod dynamic;
mod emit;
mod exclusions;
mod manifests;
mod mentions;
mod roots;
mod rust_pass;

#[cfg(test)]
mod tests;

pub const RULE_DEAD_CODE: &str = "ARCH-DEAD-CODE-1";

const RULE_IDS: &[&str] = &[RULE_DEAD_CODE];

/// Source files the checker reads, then the context files that can keep a source file alive
/// (the Layer-2 runner only loads files matching the armed checkers' interest globs, and the
/// glob matcher has no brace expansion, so each extension is listed).
const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.mjs",
    "**/*.cjs",
    "**/*.py",
    "**/*.pyi",
    "**/*.rs",
    "**/*.json",
    "**/*.yaml",
    "**/*.yml",
    "**/*.toml",
    "**/*.html",
    "**/*.htm",
    "**/*.vue",
    "**/*.svelte",
    "**/*.astro",
    "**/*.md",
    "**/*.mdx",
    "**/*.css",
    "**/*.scss",
    "**/*.less",
    "**/*.sh",
    "**/*.txt",
    "**/*.cfg",
    "**/*.ini",
    "**/*.env",
    "**/.env*",
    "**/*.tf",
    "**/*.xml",
    "**/*.ejs",
    "**/*.hbs",
    "**/*.pug",
    "**/*.njk",
    "**/*.j2",
    "**/*.jinja",
    "**/*.jinja2",
    "**/*.sql",
    "**/Dockerfile*",
    "**/Makefile",
    "**/Procfile",
    "**/Justfile",
];

/// The language family of a source file, which decides how liveness is computed: ECMAScript and
/// Python by reachability over the import graph, Rust by the `mod` declaration closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Ecma,
    Python,
    Rust,
}

fn family_of(path: &str) -> Option<Family> {
    match extract::lang_for_path(path)? {
        SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => Some(Family::Ecma),
        SourceLang::Python => Some(Family::Python),
        SourceLang::Rust => Some(Family::Rust),
    }
}

/// The extensions that make a repository one this checker can answer for. `.mts`, `.cts` and
/// `.pyi` are deliberately absent: they never appear without one of these beside them.
fn has_candidate_language(path: &str) -> bool {
    [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".rs"]
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(ext))
}

pub struct DeadCodeChecker;

impl ArchChecker for DeadCodeChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        analysis::analyze(repo.files)
    }

    /// The context globs make [`crate::arch_checker::checker_applies`] true for a repository
    /// with no source in a language this checker reads (a Java service with YAML config), where
    /// "no findings" would read as "no dead code". Such a repository stays advisory.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        !repo.files.iter().any(|(p, _)| has_candidate_language(p))
    }
}
