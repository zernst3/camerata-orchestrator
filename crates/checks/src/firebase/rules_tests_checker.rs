//! [`FirebaseRulesTestsChecker`]: FIREBASE-RULES-TESTS-1 (rule 13).
//!
//! A repository that deploys security rules but depends on no rules unit-testing library has no
//! rules tests. The absence is a plain fact, graded as a defect and emitted at a low severity.
//! Tests written against the emulator REST API in another language, or kept in a separate
//! repository, are invisible; the message says how to close the gap.

use super::config::resolve;
use super::functions::manifest_depends_on;
use super::RULE_RULES_TESTS;
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_MEDIUM};

const INTEREST_GLOBS: &[&str] = &[
    "**/package.json",
    "**/*.rules",
    "**/firebase.json",
    "**/database.rules.json",
];

/// The libraries Firebase provides to run rules against the emulator.
const TESTING_LIBRARIES: &[&str] = &["@firebase/rules-unit-testing", "@firebase/testing"];

/// The checker for the absence of rules tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct FirebaseRulesTestsChecker;

impl ArchChecker for FirebaseRulesTestsChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        &[RULE_RULES_TESTS]
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let set = resolve(repo.files);
        let Some(first) = set.assessed.first() else {
            return Vec::new();
        };
        let has_tests = repo.files.iter().any(|(path, content)| {
            path.rsplit('/').next() == Some("package.json")
                && !path.split('/').any(|s| s == "node_modules")
                && TESTING_LIBRARIES
                    .iter()
                    .any(|lib| manifest_depends_on(content, lib))
        });
        if has_tests {
            return Vec::new();
        }
        let deployed: Vec<String> = set
            .assessed
            .iter()
            .map(|a| format!("`{}`", a.path))
            .collect();
        vec![ArchViolation {
            grade: ViolationGrade::Defect,
            rule_id: RULE_RULES_TESTS.to_string(),
            file: first.path.clone(),
            line: 0,
            object: None,
            message: format!(
                "This repository deploys security rules ({}) but has no rules tests: no package depends on `@firebase/rules-unit-testing`, the library Firebase provides to run rules against the emulator. If the rules are tested another way, for example against the emulator from another language or from a separate repository, that is not visible here.",
                deployed.join(", ")
            ),
            severity: SEVERITY_MEDIUM,
        }]
    }

    /// Absence-based and advisory: a scan and CI rule, never a write-time bounce.
    fn runs_at_write_time(&self) -> bool {
        false
    }

    /// No assessed rules file: there is nothing to test.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        resolve(repo.files).assessed.is_empty()
    }
}
