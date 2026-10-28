//! `TestAssertionChecker`: the deterministic half of `TESTING-MEANINGFUL-ASSERTION-1` (master
//! sequence item 22e, "a test asserts something"). The design, the traps and the safe twins are
//! in `docs/design/2026-10-21_w29-detector-implementation.md` ("Wave D and 22e: the specs",
//! TESTING-MEANINGFUL-ASSERTION-1).
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/testing/testing-meaningful-assertion-1.toml` (`testing` domain).
//!
//! # What it proves
//!
//! A test whose body holds no assertion at all: nothing that checks a result, expects an error
//! or calls a helper that might. Three ecosystems are read, the ones the extractor layer
//! parses: [`ecma`] (Jest, Vitest, Mocha, Jasmine, `node:test`, Bun), [`python`] (pytest,
//! unittest) and [`rust`] (`#[test]`). Go, Java, C#, Ruby and the other corpus languages are left
//! to the semantic pass.
//!
//! # The judgment half goes to the semantic pass
//!
//! "The test asserts only on a value its own stub produced" needs a reader. The checker opts
//! into [`ArchChecker::advisory_coexisting`], so the rule id stays in the semantic prompt and the
//! model reads tests against the rule's text, while [`ArchChecker::defects_need_review`] is
//! `false`: a test with no assertion at all is a fact CI may fail on.
//!
//! # The law: false-negative, never false-positive
//!
//! Anything that might check is an assertion: a call whose name reads as a check, a panicking
//! call, a throw, a helper defined in the same file that asserts, and any helper from a
//! test-support module the checker cannot see into. Tests that do not run (skipped, todo) and
//! tests whose framework hooks or fixtures may assert for them are never reported.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`]; the corpus rule's ceiling is also `medium`.

mod ecma;
mod python;
mod rust;
mod words;

#[cfg(test)]
mod tests;

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_MEDIUM};
use crate::extract::clone::is_vendored_path;
use crate::extract::ecma_walk::dialect_for;
use crate::extract::paths::{emitted_js_paths, is_test_path};

pub const RULE_MEANINGFUL_ASSERTION: &str = "TESTING-MEANINGFUL-ASSERTION-1";

const RULE_IDS: &[&str] = &[RULE_MEANINGFUL_ASSERTION];

/// Every source file of the three ecosystems: test files are recognised by path inside the
/// checker, and Rust tests live in any file.
const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts", "**/*.tsx", "**/*.mts", "**/*.cts", "**/*.js", "**/*.jsx", "**/*.mjs", "**/*.cjs",
    "**/*.py", "**/*.rs",
];

/// One test with no assertion, as an ecosystem module reports it.
pub(super) struct Finding {
    pub(super) line: usize,
    /// The test's name or title.
    pub(super) name: String,
    pub(super) message: String,
}

pub struct TestAssertionChecker;

impl ArchChecker for TestAssertionChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let emitted = emitted_js_paths(repo.files);
        let mut out = Vec::new();
        for (path, source) in repo.files {
            if is_vendored_path(path) || emitted.contains(path.as_str()) {
                continue;
            }
            let findings = if dialect_for(path).is_some() {
                if !is_test_path(path) {
                    continue;
                }
                ecma::check(path, source)
            } else if path.ends_with(".py") {
                if !python::is_test_module(path) {
                    continue;
                }
                let fixtures = python::visible_fixtures(path, source, repo.files);
                if fixtures.autouse_asserts {
                    continue;
                }
                python::check(source, &fixtures)
            } else if path.ends_with(".rs") {
                rust::check(source)
            } else {
                continue;
            };
            out.extend(findings.into_iter().map(|f| ArchViolation {
                grade: ViolationGrade::Defect,
                rule_id: RULE_MEANINGFUL_ASSERTION.to_string(),
                file: path.clone(),
                line: f.line,
                object: Some(f.name),
                message: f.message,
                severity: SEVERITY_MEDIUM,
            }));
        }
        out.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.message.as_str()).cmp(&(
                b.file.as_str(),
                b.line,
                b.message.as_str(),
            ))
        });
        out
    }

    /// "Asserts only on its own stub" is a judgment the semantic pass makes.
    fn advisory_coexisting(&self) -> bool {
        true
    }

    /// A test with no assertion at all is a fact: CI may fail on it.
    fn defects_need_review(&self) -> bool {
        false
    }
}
