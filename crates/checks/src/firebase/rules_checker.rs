//! [`FirebaseRulesChecker`]: the checker for the security-rules rules (design sections 7.1 to
//! 7.9), over Firestore, Storage and Realtime Database rules files.
//!
//! One checker answers all of them because they share the file set ([`super::config`]), the
//! reader and the recognisers, and because one `allow` must be reported by exactly one rule (G7).
//! It also owns the **degradation contract** (design section 5.5):
//!
//! - a file the reader cannot fully account for produces one NOT ASSESSED row and no rule
//!   findings, never a clean result;
//! - a rules file that exists but is not the deployed one (dev, draft) produces one NOT ASSESSED
//!   row, never a defect;
//! - a repository whose deployed rules are not in it (console only) produces one NOT ASSESSED
//!   row, and [`ArchChecker::config_unsatisfied_for`] keeps the rules out of the ledger credit.

use chrono::NaiveDate;

use super::config::{resolve, AssessedFile, FileSet, RulesKind};
use super::edge_rules;
use super::grant_rules::{self, Owned};
use super::grants::{sites, LoadedRules};
use super::rtdb::parse_rtdb;
use super::rtdb_rules::{self, LoadedRtdb};
use super::rules_parse::{parse_rules, RulesFile, Service, MAX_ALLOWS_PER_FILE};
use super::{
    RULE_ADMIN_CLAIMS, RULE_AUTH_ONLY, RULE_FIELD_VALIDATION, RULE_OPEN, RULE_OWNERSHIP,
    RULE_RECURSIVE_WILDCARD, RULE_RTDB_OPEN, RULE_STORAGE_OPEN, RULE_TEST_MODE,
};
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_INFO};

/// Every rule id this checker answers. Kept in step with the rules landed in this module.
const RULE_IDS: &[&str] = &[
    RULE_OPEN,
    RULE_TEST_MODE,
    RULE_AUTH_ONLY,
    RULE_RECURSIVE_WILDCARD,
    RULE_STORAGE_OPEN,
    RULE_RTDB_OPEN,
    RULE_OWNERSHIP,
    RULE_FIELD_VALIDATION,
    RULE_ADMIN_CLAIMS,
];

/// Path globs the checker needs to see: rules files, the project config, and the two manifests
/// whose Firebase entries show that a repository uses Firebase. A renamed Realtime Database rules
/// file is named by `firebase.json`, not by its own name, and is fetched through
/// [`ArchChecker::extra_interest_paths`] rather than by taking every `.json` file.
const INTEREST_GLOBS: &[&str] = &[
    "**/*.rules",
    "**/firebase.json",
    "**/.firebaserc",
    "**/database.rules.json",
    "**/*.rules.json",
    "**/google-services.json",
    "**/package.json",
];

/// The checker for the Firebase security-rules rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct FirebaseRulesChecker {
    /// The date test-mode expiry is judged against. `None` reads the system clock; tests pin it.
    pub today: Option<NaiveDate>,
}

impl FirebaseRulesChecker {
    fn today(&self) -> NaiveDate {
        self.today
            .unwrap_or_else(|| chrono::Utc::now().date_naive())
    }
}

impl ArchChecker for FirebaseRulesChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    /// The rules files `firebase.json` names that the glob walk did not collect.
    fn extra_interest_paths(&self, files: &[(String, String)]) -> Vec<String> {
        resolve(files)
            .missing_named
            .into_iter()
            .map(|m| m.path)
            .collect()
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let set = resolve(repo.files);
        let mut out = coverage_rows(&set, repo.files);
        let mut loaded = load(repo.files, &set);
        out.append(&mut loaded.rows);

        let all_sites: Vec<_> = loaded.rules.iter().flat_map(sites).collect();
        let today = self.today();
        let (mut wildcard, owned): (Vec<ArchViolation>, Owned) =
            grant_rules::recursive_wildcard(&all_sites, today);
        out.append(&mut wildcard);
        out.append(&mut grant_rules::open(&all_sites, &owned));
        out.append(&mut grant_rules::test_mode(&all_sites, &owned, today));
        out.append(&mut grant_rules::auth_only(&all_sites, &owned));
        out.append(&mut grant_rules::storage(&all_sites, &owned));
        out.append(&mut edge_rules::ownership(&all_sites));
        out.append(&mut edge_rules::field_validation(&all_sites));
        out.append(&mut edge_rules::admin_claims(&all_sites));
        for rtdb in &loaded.rtdb {
            out.append(&mut rtdb_rules::check(rtdb));
        }

        out.sort_by(|a, b| {
            (&a.file, a.line, &a.rule_id, &a.message)
                .cmp(&(&b.file, b.line, &b.rule_id, &b.message))
        });
        out
    }

    /// Rules 4, 5, 6 and 9 have judgment halves the semantic pass must still see.
    fn advisory_coexisting(&self) -> bool {
        true
    }

    /// Unlike an advisory checker's findings, this checker's `Defect` findings are deterministic
    /// facts: CI must fail on them.
    fn defects_need_review(&self) -> bool {
        false
    }

    /// The deployed rules are not in the repository (or none of them could be read), so this
    /// checker cannot answer for it and must not be credited as having run clean.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        let set = resolve(repo.files);
        let loaded = load(repo.files, &set);
        loaded.rules.is_empty() && loaded.rtdb.is_empty()
    }
}

/// A NOT ASSESSED row.
fn not_assessed(rule: &'static str, file: &str, line: usize, message: String) -> ArchViolation {
    ArchViolation {
        grade: ViolationGrade::NotAssessed,
        rule_id: rule.to_string(),
        file: file.to_string(),
        line,
        object: None,
        message,
        severity: SEVERITY_INFO,
    }
}

/// The rows about the file set itself: unreferenced files, unreadable or incomplete
/// `firebase.json`, and a repository whose deployed rules are not in it.
fn coverage_rows(set: &FileSet, files: &[(String, String)]) -> Vec<ArchViolation> {
    let mut rows = Vec::new();
    for path in &set.unreadable_configs {
        rows.push(not_assessed(
            RULE_OPEN,
            path,
            0,
            format!("`{path}` could not be read, so the deployed rules files could not be confirmed; the Firebase rules checks ran only on files found by their conventional names."),
        ));
    }
    for missing in &set.missing_named {
        rows.push(not_assessed(
            RULE_OPEN,
            &missing.firebase_json,
            0,
            format!(
                "`{}` names `{}` but that file was not among the files read, so it was not assessed.",
                missing.firebase_json, missing.path
            ),
        ));
    }
    for file in &set.unreferenced {
        let product = match file.service {
            Service::Storage => "Storage",
            _ => "Firestore",
        };
        let reference = if set.has_firebase_json {
            "that `firebase.json` does not reference"
        } else {
            "that no `firebase.json` in this repository references"
        };
        rows.push(not_assessed(
            RULE_OPEN,
            &file.path,
            0,
            format!(
                "`{}` is a {product} rules file {reference}, so it was not assessed as the deployed ruleset.",
                file.path
            ),
        ));
    }
    // "No rules file here" is only worth saying about a repository that shows it uses Firebase:
    // a rule armed for a whole project must not annotate every other repository in it.
    if set.assessed.is_empty() && shows_firebase_use(set, files) {
        rows.push(not_assessed(
            RULE_OPEN,
            "",
            0,
            "No Firestore, Storage or Realtime Database rules file that the project deploys was found in this repository, so the Firebase rules checks were not run. If the rules are managed in the Firebase console, they are not visible to a repository inspection.".to_string(),
        ));
    }
    rows
}

/// Evidence in `files` that the repository uses Firebase, short of a rules file: a project
/// config, a platform config file, a FlutterFire options file, or a Firebase dependency.
fn shows_firebase_use(set: &FileSet, files: &[(String, String)]) -> bool {
    set.has_firebase_json
        || files.iter().any(|(path, content)| {
            let name = path.rsplit('/').next().unwrap_or(path);
            match name {
                ".firebaserc"
                | "google-services.json"
                | "GoogleService-Info.plist"
                | "firebase_options.dart" => true,
                "package.json" => ["firebase", "firebase-admin", "firebase-functions"]
                    .iter()
                    .any(|dep| super::functions::manifest_depends_on(content, dep)),
                "pubspec.yaml" => content.contains("firebase_core"),
                _ => false,
            }
        })
}

/// The assessed files that read cleanly, and one NOT ASSESSED row for every one that did not.
struct Loaded {
    rules: Vec<LoadedRules>,
    rtdb: Vec<LoadedRtdb>,
    rows: Vec<ArchViolation>,
}

/// Read every assessed file. Files the reader fully understood are returned; every other one
/// becomes a NOT ASSESSED row.
fn load(files: &[(String, String)], set: &FileSet) -> Loaded {
    let mut loaded = Loaded {
        rules: Vec::new(),
        rtdb: Vec::new(),
        rows: Vec::new(),
    };
    for assessed in &set.assessed {
        let Some((_, content)) = files.iter().find(|(p, _)| *p == assessed.path) else {
            continue;
        };
        if assessed.kind == RulesKind::Rtdb {
            let parsed = parse_rtdb(content);
            match parsed.errors.first() {
                None => loaded.rtdb.push(LoadedRtdb {
                    path: assessed.path.clone(),
                    label: assessed.label.clone(),
                    config_unreadable: assessed.config_unreadable,
                    doubts: assessed.doubts.clone(),
                    parsed,
                }),
                Some(first) => {
                    let (line, what) = (first.line, first.what.clone());
                    loaded
                        .rows
                        .push(unreadable_row(assessed, line, &what, RULE_RTDB_OPEN));
                }
            }
            continue;
        }
        let parsed = parse_rules(content);
        match readable(&parsed) {
            Ok(service) => loaded.rules.push(LoadedRules {
                path: assessed.path.clone(),
                label: assessed.label.clone(),
                config_unreadable: assessed.config_unreadable,
                doubts: assessed.doubts.clone(),
                service,
                parsed,
            }),
            Err((line, what)) => loaded
                .rows
                .push(unreadable_row(assessed, line, &what, RULE_OPEN)),
        }
    }
    loaded
}

/// The service when the file was fully understood, else the first problem.
fn readable(parsed: &RulesFile) -> Result<Service, (usize, String)> {
    if let Some(first) = parsed.errors.first() {
        return Err((first.line, first.what.clone()));
    }
    if parsed.allow_count() > MAX_ALLOWS_PER_FILE {
        return Err((
            1,
            format!("more than {MAX_ALLOWS_PER_FILE} allow statements"),
        ));
    }
    match &parsed.service {
        Some(s @ (Service::Firestore | Service::Storage)) => Ok(s.clone()),
        Some(Service::Other(name)) => Err((
            1,
            format!("a service ({name}) that is not Cloud Firestore or Cloud Storage"),
        )),
        None => Err((1, "no service block".to_string())),
    }
}

fn unreadable_row(
    file: &AssessedFile,
    line: usize,
    what: &str,
    rule: &'static str,
) -> ArchViolation {
    let kind = if file.kind == RulesKind::Rtdb {
        "Realtime Database rules"
    } else {
        "Firebase rules file"
    };
    not_assessed(
        rule,
        &file.path,
        line,
        format!(
            "`{}` could not be read as a {kind} (line {line}: {what}), so the Firebase rules checks were not run on it.",
            file.path
        ),
    )
}
