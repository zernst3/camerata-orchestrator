//! [`FirebaseFunctionsChecker`]: FIREBASE-FUNCTIONS-AUTH-1 (rule 10) and FIREBASE-APP-CHECK-1
//! (rule 12), the two rules about the project's Cloud Functions and client SDK.
//!
//! Both are review-graded and neither runs at write time: a public HTTP function or callable is a
//! legitimate pattern (webhooks, health checks, public counters), so a true fact ("nothing
//! verifies the caller, and the handler writes") is held for the client to confirm, never
//! asserted as a defect, and a bounce would punish unfinished work.

use super::functions::{
    auth_wrapper, calls_auth_helper, discover, handles_webhooks, invoker_is_restricted,
    name_is_exempt, references_auth, uses_caller_supplied_id, verifies_caller,
    writes_with_admin_sdk, Discovery, Handler, HandlerKind,
};
use super::{RULE_APP_CHECK, RULE_FUNCTIONS_AUTH};
use crate::arch_checker::{
    ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_INFO, SEVERITY_MEDIUM,
};
use crate::extract::paths::is_test_path;
use crate::extract::{imports, lang_for_path};

const RULE_IDS: &[&str] = &[RULE_FUNCTIONS_AUTH, RULE_APP_CHECK];

const INTEREST_GLOBS: &[&str] = &[
    "**/package.json",
    "**/firebase.json",
    "**/requirements.txt",
    "**/pyproject.toml",
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.mjs",
    "**/*.cjs",
    "**/*.dart",
];

/// The checker for Cloud Functions authentication and App Check.
#[derive(Debug, Clone, Copy, Default)]
pub struct FirebaseFunctionsChecker;

impl ArchChecker for FirebaseFunctionsChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let discovery = discover(repo.files);
        let mut out = python_rows(&discovery);
        for package in &discovery.packages {
            let dir_label = if package.dir.is_empty() {
                "this package".to_string()
            } else {
                format!("the `{}` package", package.dir)
            };
            let package_text: String = package
                .files
                .iter()
                .map(|(_, c)| c.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            for handler in &package.handlers {
                if let Some(row) = function_row(handler, &package_text, &dir_label, package) {
                    out.push(row);
                }
            }
        }
        out.extend(app_check_row(repo.files, &discovery));
        out.sort_by(|a, b| (&a.file, a.line, &a.rule_id).cmp(&(&b.file, b.line, &b.rule_id)));
        out
    }

    /// Both rules reason from the absence of a call; the second is a judgment about whether
    /// abuse matters, which the semantic pass must still see.
    fn advisory_coexisting(&self) -> bool {
        true
    }

    fn defects_need_review(&self) -> bool {
        false
    }

    /// Review-graded and absence-based: a scan and CI rule, never a write-time bounce.
    fn runs_at_write_time(&self) -> bool {
        false
    }

    /// No functions package and no client initialisation: nothing here for this checker to answer.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        let discovery = discover(repo.files);
        discovery.packages.is_empty()
            && discovery.python_dirs.is_empty()
            && client_init(repo.files).is_none()
    }
}

/// Cloud Functions written in Python are reported as not assessed, never as clean (G13).
fn python_rows(discovery: &Discovery) -> Vec<ArchViolation> {
    discovery
        .python_dirs
        .iter()
        .map(|dir| {
            let label = if dir.is_empty() { "." } else { dir.as_str() };
            ArchViolation {
                grade: ViolationGrade::NotAssessed,
                rule_id: RULE_FUNCTIONS_AUTH.to_string(),
                file: label.to_string(),
                line: 0,
                object: None,
                message: format!(
                    "The Cloud Functions in `{label}` are written in Python, which is not inspected in this version, so the functions rules were not run on them."
                ),
                severity: SEVERITY_INFO,
            }
        })
        .collect()
}

/// The finding for one handler, or `None` when a guard says it is not a candidate.
fn function_row(
    handler: &Handler,
    package_text: &str,
    dir_label: &str,
    package: &super::functions::FunctionsPackage,
) -> Option<ArchViolation> {
    if name_is_exempt(&handler.name, &handler.file) {
        return None;
    }
    let source = package
        .files
        .iter()
        .find(|(p, _)| *p == handler.file)
        .map_or("", |(_, c)| c.as_str());
    let (what, why) = match handler.kind {
        HandlerKind::Request => {
            if invoker_is_restricted(handler.options.as_deref())
                || handles_webhooks(source)
                || verifies_caller(package_text)
            {
                return None;
            }
            let writes = if handler.express_app {
                package.files.iter().any(|(_, c)| writes_with_admin_sdk(c))
            } else {
                writes_with_admin_sdk(&handler.body)
            };
            if !writes {
                return None;
            }
            (
                "HTTP function",
                format!(
                    "is reachable by anyone who knows its URL. Nothing in {dir_label} verifies a Firebase ID token (`verifyIdToken`) or an App Check token, and the handler writes with the Admin SDK, which bypasses security rules. If the function is meant to be public, no change is needed; otherwise read the `Authorization: Bearer` ID token, verify it and refuse with 403 before acting."
                ),
            )
        }
        HandlerKind::Call => {
            if handler.wrapper.as_deref().is_some_and(auth_wrapper)
                || references_auth(&handler.params, &handler.body)
                || calls_auth_helper(&handler.body)
            {
                return None;
            }
            let writes = writes_with_admin_sdk(&handler.body);
            let by_caller_id = uses_caller_supplied_id(&handler.body);
            let action = match (writes, by_caller_id) {
                (true, true) => {
                    "it writes with the Admin SDK to a path built from the caller's input"
                }
                (true, false) => "it writes with the Admin SDK",
                (false, true) => "it builds a document path from the caller's input",
                (false, false) => return None,
            };
            (
                "callable function",
                format!(
                    "never checks `request.auth`, so an unauthenticated client can call it, and {action}. Firebase's callable guide shows the check (`if (!request.auth) {{ throw new HttpsError(...) }}`)."
                ),
            )
        }
    };
    Some(ArchViolation {
        grade: ViolationGrade::Review,
        rule_id: RULE_FUNCTIONS_AUTH.to_string(),
        file: handler.file.clone(),
        line: handler.line,
        object: Some(handler.name.clone()),
        message: format!(
            "The {what} `{}` (`{}`, line {}) {why}",
            handler.name, handler.file, handler.line
        ),
        severity: SEVERITY_MEDIUM,
    })
}

/// A client initialisation of the Firebase SDK: `(path, line)` of the first, by path.
fn client_init(files: &[(String, String)]) -> Option<(String, usize)> {
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, source) in sorted {
        if is_test_path(path) || path.split('/').any(|s| s == "node_modules") {
            continue;
        }
        let needle = if path.to_ascii_lowercase().ends_with(".dart") {
            if !source.contains("Firebase.initializeApp(") {
                continue;
            }
            "Firebase.initializeApp("
        } else {
            let Some(lang) = lang_for_path(path) else {
                continue;
            };
            let from_client_sdk = imports(lang, source).iter().any(|i| {
                i.specifier == "firebase"
                    || i.specifier == "@firebase/app"
                    || i.specifier.starts_with("firebase/")
            });
            if !from_client_sdk || !source.contains("initializeApp(") {
                continue;
            }
            "initializeApp("
        };
        let line = source
            .lines()
            .position(|l| l.contains(needle))
            .map_or(1, |i| i + 1);
        return Some((path.clone(), line));
    }
    None
}

/// App Check appears somewhere in the repository's source (outside tests).
fn app_check_present(files: &[(String, String)]) -> bool {
    const MARKERS: &[&str] = &[
        "initializeAppCheck(",
        "FirebaseAppCheck.instance.activate(",
        "enforceAppCheck",
        "X-Firebase-AppCheck",
        "getAppCheck()",
    ];
    files.iter().any(|(path, source)| {
        let lower = path.to_ascii_lowercase();
        let code = [".ts", ".tsx", ".js", ".mjs", ".cjs", ".dart"]
            .iter()
            .any(|e| lower.ends_with(e));
        code && !is_test_path(path)
            && !path.split('/').any(|s| s == "node_modules")
            && (MARKERS.iter().any(|m| source.contains(m))
                || member_app(source, "request")
                || member_app(source, "context"))
    })
}

/// `request.app` / `context.app` as a whole member access (not `request.application`).
fn member_app(source: &str, object: &str) -> bool {
    let needle = format!("{object}.app");
    source.match_indices(&needle).any(|(i, _)| {
        let after = source[i + needle.len()..].chars().next();
        after.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    })
}

fn app_check_row(files: &[(String, String)], discovery: &Discovery) -> Option<ArchViolation> {
    let (path, line) = client_init(files)?;
    if app_check_present(files) {
        return None;
    }
    let callables = discovery
        .packages
        .iter()
        .flat_map(|p| &p.handlers)
        .filter(|h| h.kind == HandlerKind::Call)
        .count();
    let enforced_on = match callables {
        0 => "enforced in any Cloud Function in this repository".to_string(),
        1 => "enforced on its callable function".to_string(),
        n => format!("enforced on any of its {n} callable functions"),
    };
    Some(ArchViolation {
        grade: ViolationGrade::Review,
        rule_id: RULE_APP_CHECK.to_string(),
        file: path.clone(),
        line,
        object: None,
        message: format!(
            "This Firebase app initialises the client SDK (`{path}`, line {line}) but App Check is neither initialised in the client nor {enforced_on}. Firebase's callable guide says to enable App Check before launch so that only your apps can reach your function endpoints. Whether abuse matters for this app is your call; enforcement for Firestore, Storage and Realtime Database is switched on in the Firebase console, which a repository inspection cannot see."
        ),
        severity: SEVERITY_MEDIUM,
    })
}
