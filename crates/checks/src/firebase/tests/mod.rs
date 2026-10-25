//! Tests for the Firebase module, one file per concern (design section 5.6 and section 7).

mod auth_only;
mod checker;
mod edges;
mod functions;
mod open;
mod parse;
mod project_rules;
mod recursive_wildcard;
mod rtdb;
mod storage;
mod test_mode;

use chrono::NaiveDate;

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade};
use crate::firebase::FirebaseRulesChecker;

/// `(path, content)` pairs, the shape every checker input takes.
pub(crate) fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect()
}

/// The date every test pins as "today".
pub(crate) fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()
}

/// Run the rules checker over `f` with the pinned date.
pub(crate) fn run(f: &[(String, String)]) -> Vec<ArchViolation> {
    FirebaseRulesChecker {
        today: Some(today()),
    }
    .check(&RepoView {
        spec: "t/r",
        files: f,
    })
}

/// The opening lines of a Firestore rules file: the first body line is line 4.
const FIRESTORE_HEAD: &str =
    "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n";
const FIRESTORE_TAIL: &str = "  }\n}\n";

/// A repository whose `firebase.json` names `firestore.rules`, holding `body` inside the
/// `documents` match. The first body line is line 4.
pub(crate) fn firestore(body: &str) -> Vec<(String, String)> {
    files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        (
            "firestore.rules",
            &format!("{FIRESTORE_HEAD}{body}\n{FIRESTORE_TAIL}"),
        ),
    ])
}

/// A repository whose `firebase.json` names `storage.rules`, holding `body` inside the
/// `b/{bucket}/o` match. The first body line is line 4.
pub(crate) fn storage(body: &str) -> Vec<(String, String)> {
    files(&[
        (
            "firebase.json",
            r#"{ "storage": { "rules": "storage.rules" } }"#,
        ),
        (
            "storage.rules",
            &format!(
                "rules_version = '2';\nservice firebase.storage {{\n  match /b/{{bucket}}/o {{\n{body}\n  }}\n}}\n"
            ),
        ),
    ])
}

/// Only the rows graded `grade`, for the rule `id`.
pub(crate) fn rows<'a>(
    all: &'a [ArchViolation],
    id: &str,
    grade: ViolationGrade,
) -> Vec<&'a ArchViolation> {
    all.iter()
        .filter(|v| v.rule_id == id && v.grade == grade)
        .collect()
}

/// Every Defect or Review row (the findings), leaving out not-assessed notices.
pub(crate) fn findings(all: &[ArchViolation]) -> Vec<&ArchViolation> {
    all.iter()
        .filter(|v| v.grade != ViolationGrade::NotAssessed)
        .collect()
}

/// A repository whose `firebase.json` names `database.rules.json`, holding `json`.
pub(crate) fn rtdb(json: &str) -> Vec<(String, String)> {
    files(&[
        (
            "firebase.json",
            r#"{ "database": { "rules": "database.rules.json" } }"#,
        ),
        ("database.rules.json", json),
    ])
}

/// The findings (Defect or Review rows) of one rule: a rules file can legitimately raise several
/// rules at once, so each rule's tests look at their own.
pub(crate) fn of<'a>(all: &'a [ArchViolation], rule: &str) -> Vec<&'a ArchViolation> {
    all.iter()
        .filter(|v| v.rule_id == rule && v.grade != ViolationGrade::NotAssessed)
        .collect()
}
