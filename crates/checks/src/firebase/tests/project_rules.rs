//! FIREBASE-ADMIN-SDK-CLIENT-1 (7.11), FIREBASE-APP-CHECK-1 (7.12) and FIREBASE-RULES-TESTS-1
//! (7.13).

use super::files;
use crate::arch_checker::{
    ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_HIGH, SEVERITY_MEDIUM,
};
use crate::firebase::{
    FirebaseAdminSdkClientChecker, FirebaseFunctionsChecker, FirebaseRulesTestsChecker,
    RULE_ADMIN_SDK_CLIENT, RULE_APP_CHECK, RULE_RULES_TESTS,
};

fn run_with(checker: &dyn ArchChecker, f: &[(String, String)]) -> Vec<ArchViolation> {
    checker.check(&RepoView {
        spec: "t/r",
        files: f,
    })
}

// ── ADMIN-SDK-CLIENT ────────────────────────────────────────────────────────────────────────

const VITE_APP: &str =
    r#"{ "dependencies": { "react": "18" }, "devDependencies": { "vite": "5" } }"#;

fn admin(f: &[(String, String)]) -> Vec<ArchViolation> {
    run_with(&FirebaseAdminSdkClientChecker, f)
}

#[test]
fn admin_sdk_imported_in_a_vite_app_is_high() {
    let f = files(&[
        ("package.json", VITE_APP),
        (
            "src/lib/db.ts",
            "import { x } from \"react\";\nimport admin from \"firebase-admin\";\nexport const a = admin;\n",
        ),
    ]);
    let all = admin(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].rule_id, RULE_ADMIN_SDK_CLIENT);
    assert_eq!(all[0].grade, ViolationGrade::Defect);
    assert_eq!(all[0].severity, SEVERITY_HIGH);
    assert_eq!(all[0].file, "src/lib/db.ts");
    assert_eq!(all[0].line, 2);
    assert!(
        all[0].message.contains("`vite` in `package.json`"),
        "{}",
        all[0].message
    );
    assert!(all[0].message.contains("service-account credentials"));
}

#[test]
fn admin_sdk_imported_in_a_use_client_component_is_high() {
    let f = files(&[
        ("package.json", r#"{ "dependencies": { "next": "14", "react": "18" } }"#),
        (
            "app/page.tsx",
            "'use client';\nimport { getApps } from \"firebase-admin/app\";\nexport default function P() { return null; }\n",
        ),
    ]);
    let all = admin(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(all[0].message.contains("use client"), "{}", all[0].message);
}

#[test]
fn admin_sdk_under_a_public_folder_is_high() {
    let f = files(&[(
        "public/js/app.js",
        "import admin from \"firebase-admin\";\n",
    )]);
    assert_eq!(admin(&f).len(), 1);
}

#[test]
fn twin_admin_sdk_in_a_next_server_file_is_silent() {
    let f = files(&[
        ("package.json", r#"{ "dependencies": { "next": "14", "react": "18" } }"#),
        (
            "app/api/route.ts",
            "import { getApps } from \"firebase-admin/app\";\nexport const GET = () => getApps();\n",
        ),
    ]);
    assert!(admin(&f).is_empty());
}

#[test]
fn twin_admin_sdk_in_the_functions_package_is_silent() {
    let f = files(&[
        ("package.json", VITE_APP),
        (
            "functions/package.json",
            r#"{ "dependencies": { "firebase-functions": "5", "firebase-admin": "12" } }"#,
        ),
        (
            "functions/src/index.ts",
            "import admin from \"firebase-admin\";\n",
        ),
    ]);
    assert!(admin(&f).is_empty());
}

#[test]
fn twin_admin_sdk_in_an_express_api_is_silent() {
    let f = files(&[
        ("package.json", r#"{ "dependencies": { "express": "4" } }"#),
        ("src/server.ts", "import admin from \"firebase-admin\";\n"),
    ]);
    assert!(admin(&f).is_empty());
}

#[test]
fn twin_type_only_import_is_silent() {
    let f = files(&[
        ("package.json", VITE_APP),
        (
            "src/types.ts",
            "import type { App } from \"firebase-admin/app\";\nexport type A = App;\n",
        ),
    ]);
    assert!(admin(&f).is_empty());
}

#[test]
fn twin_fullstack_single_package_is_silent() {
    let f = files(&[
        (
            "package.json",
            r#"{ "dependencies": { "express": "4" }, "devDependencies": { "vite": "5" } }"#,
        ),
        ("src/db.ts", "import admin from \"firebase-admin\";\n"),
    ]);
    assert!(admin(&f).is_empty());
}

#[test]
fn twin_admin_sdk_in_a_test_or_declaration_file_is_silent() {
    let f = files(&[
        ("package.json", VITE_APP),
        ("src/db.test.ts", "import admin from \"firebase-admin\";\n"),
        ("src/db.d.ts", "import admin from \"firebase-admin\";\n"),
    ]);
    assert!(admin(&f).is_empty());
}

#[test]
fn the_admin_sdk_checker_is_not_applicable_without_an_import() {
    let f = files(&[("package.json", VITE_APP), ("src/a.ts", "export {};\n")]);
    assert!(
        FirebaseAdminSdkClientChecker.config_unsatisfied_for(&RepoView {
            spec: "t/r",
            files: &f,
        })
    );
}

// ── APP-CHECK ───────────────────────────────────────────────────────────────────────────────

const CLIENT_INIT: &str = "import { initializeApp } from \"firebase/app\";\n\nconst config = { projectId: \"x\" };\nexport const app = initializeApp(config);\n";

fn app_check(f: &[(String, String)]) -> Vec<ArchViolation> {
    run_with(&FirebaseFunctionsChecker, f)
        .into_iter()
        .filter(|v| v.rule_id == RULE_APP_CHECK)
        .collect()
}

#[test]
fn client_init_without_app_check_is_one_low_review_row() {
    let f = files(&[("src/firebase.ts", CLIENT_INIT)]);
    let all = app_check(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::Review);
    assert_eq!(all[0].severity, SEVERITY_MEDIUM);
    assert_eq!(all[0].file, "src/firebase.ts");
    assert_eq!(all[0].line, 4);
    assert!(
        all[0].message.contains("App Check is neither initialised"),
        "{}",
        all[0].message
    );
}

#[test]
fn app_check_row_counts_the_callable_functions() {
    let f = files(&[
        ("src/firebase.ts", CLIENT_INIT),
        (
            "functions/package.json",
            r#"{ "dependencies": { "firebase-functions": "5" } }"#,
        ),
        (
            "functions/src/index.ts",
            "import { onCall } from \"firebase-functions/v2/https\";\nexport const a = onCall((r) => 1);\nexport const b = onCall((r) => 2);\n",
        ),
    ]);
    let all = app_check(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(
        all[0].message.contains("any of its 2 callable functions"),
        "{}",
        all[0].message
    );
}

#[test]
fn twin_initialize_app_check_anywhere_is_silent() {
    let f = files(&[
        ("src/firebase.ts", CLIENT_INIT),
        (
            "src/appcheck.ts",
            "import { initializeAppCheck } from \"firebase/app-check\";\ninitializeAppCheck(app, { provider });\n",
        ),
    ]);
    assert!(app_check(&f).is_empty());
}

#[test]
fn twin_flutter_activate_is_silent() {
    let f = files(&[(
        "lib/main.dart",
        "void main() async {\n  await Firebase.initializeApp();\n  await FirebaseAppCheck.instance.activate();\n}\n",
    )]);
    assert!(app_check(&f).is_empty());
    // Without the activation a Flutter app is flagged at its initialisation.
    let bare = files(&[(
        "lib/main.dart",
        "void main() async {\n  await Firebase.initializeApp();\n}\n",
    )]);
    let rows = app_check(&bare);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line, 2);
}

#[test]
fn twin_enforce_app_check_on_callables_is_silent() {
    let f = files(&[
        ("src/firebase.ts", CLIENT_INIT),
        (
            "functions/package.json",
            r#"{ "dependencies": { "firebase-functions": "5" } }"#,
        ),
        (
            "functions/src/index.ts",
            "import { onCall } from \"firebase-functions/v2/https\";\nexport const a = onCall({ enforceAppCheck: true }, (r) => 1);\n",
        ),
    ]);
    assert!(app_check(&f).is_empty());
}

#[test]
fn twin_backend_only_repo_is_silent() {
    let f = files(&[
        (
            "functions/package.json",
            r#"{ "dependencies": { "firebase-functions": "5", "firebase-admin": "12" } }"#,
        ),
        (
            "functions/src/index.ts",
            "import { initializeApp } from \"firebase-admin/app\";\ninitializeApp();\n",
        ),
    ]);
    assert!(app_check(&f).is_empty());
}

#[test]
fn twin_app_check_in_a_test_file_does_not_count_and_a_test_init_is_not_a_client() {
    let f = files(&[
        ("src/firebase.ts", CLIENT_INIT),
        ("src/appcheck.test.ts", "initializeAppCheck(app, {});\n"),
    ]);
    assert_eq!(app_check(&f).len(), 1);
}

// ── RULES-TESTS ─────────────────────────────────────────────────────────────────────────────

fn rules_tests(f: &[(String, String)]) -> Vec<ArchViolation> {
    run_with(&FirebaseRulesTestsChecker, f)
}

const RULES: &str =
    "rules_version = '2';\nservice cloud.firestore { match /databases/{d}/documents { } }\n";

#[test]
fn rules_files_without_the_testing_library_is_one_low_row() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", RULES),
        ("package.json", r#"{ "devDependencies": { "jest": "29" } }"#),
    ]);
    let all = rules_tests(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].rule_id, RULE_RULES_TESTS);
    assert_eq!(all[0].grade, ViolationGrade::Defect);
    assert_eq!(all[0].severity, SEVERITY_MEDIUM);
    assert_eq!(all[0].file, "firestore.rules");
    assert!(
        all[0].message.contains("@firebase/rules-unit-testing"),
        "{}",
        all[0].message
    );
}

#[test]
fn twin_rules_unit_testing_in_dev_dependencies_is_silent() {
    let f = files(&[
        ("firestore.rules", RULES),
        (
            "package.json",
            r#"{ "devDependencies": { "@firebase/rules-unit-testing": "^3.0.0" } }"#,
        ),
    ]);
    assert!(rules_tests(&f).is_empty());
}

#[test]
fn twin_deprecated_firebase_testing_is_silent() {
    let f = files(&[
        ("firestore.rules", RULES),
        (
            "package.json",
            r#"{ "devDependencies": { "@firebase/testing": "^0.20.0" } }"#,
        ),
    ]);
    assert!(rules_tests(&f).is_empty());
}

#[test]
fn twin_no_rules_files_is_silent() {
    let f = files(&[("package.json", r#"{ "devDependencies": { "jest": "29" } }"#)]);
    assert!(rules_tests(&f).is_empty());
    assert!(FirebaseRulesTestsChecker.config_unsatisfied_for(&RepoView {
        spec: "t/r",
        files: &f,
    }));
}

#[test]
fn the_rules_tests_checker_is_a_scan_rule_not_a_write_time_bounce() {
    assert!(!FirebaseRulesTestsChecker.runs_at_write_time());
}
