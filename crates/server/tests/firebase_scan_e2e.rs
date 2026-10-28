#![allow(clippy::unwrap_used)]
//! END-TO-END: Firebase / Firestore coverage (MASTER-SEQUENCE item 24) through the real brownfield
//! pipeline (`onboard::audit_repos_with_llm`: file admission, the secrets floor, stack detection,
//! the architectural checkers, the ledger and the coverage notes) with a stub model that FAILS
//! the test if it is called. ZERO API SPEND.
//!
//! The fixture (`tests/fixtures/firebase-app`) is a small Vite and Firebase web app with a
//! Firestore rules file, a Storage rules file, `firebase.json`, a Cloud Functions package and a
//! client `src/firebase.ts` whose web config (with its `apiKey`) is assembled at runtime so this
//! repository holds no key-shaped literal.
//!
//! What this pins that the unit tests cannot:
//!
//!   1. the web `apiKey` produces NO secrets-floor row (a Firebase key is public by design), while
//!      a Google key outside a Firebase config still does (the control that proves the floor ran);
//!   2. the rules rows land at the lines and grades the design assigns, through admission,
//!      `firebase.json` resolution and the adapter;
//!   3. the coverage sentence is present, in client voice;
//!   4. a rules file the reader cannot read is reported as not assessed and never credited as a
//!      clean detector run.
//!
//! An `#[ignore]`d sibling scans a real open-source Firebase app from disk when
//! `CAMERATA_FIREBASE_SAMPLE` points at a checkout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use camerata_api_types::audit::FindingKind;
use camerata_server::ai_audit::ScanMode;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, Finding, ScanReport, SelectedRule};

/// All thirteen rules, as a Firebase repo is proposed them.
const RULES: [&str; 13] = [
    "FIREBASE-RULES-OPEN-1",
    "FIREBASE-RULES-TEST-MODE-1",
    "FIREBASE-RULES-AUTH-ONLY-1",
    "FIREBASE-RULES-OWNERSHIP-1",
    "FIREBASE-RULES-FIELD-VALIDATION-1",
    "FIREBASE-RULES-RECURSIVE-WILDCARD-1",
    "FIREBASE-STORAGE-RULES-OPEN-1",
    "FIREBASE-RTDB-RULES-OPEN-1",
    "FIREBASE-ADMIN-CLAIMS-1",
    "FIREBASE-FUNCTIONS-AUTH-1",
    "FIREBASE-ADMIN-SDK-CLIENT-1",
    "FIREBASE-APP-CHECK-1",
    "FIREBASE-RULES-TESTS-1",
];

/// A model that must never be called.
struct NoModel;

#[async_trait]
impl LlmPort for NoModel {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        panic!("the Firebase acceptance scan must make no model call");
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/firebase-app")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A runtime-built Google API key: no 39-character literal lives in this source.
fn google_key() -> String {
    format!(
        "AIza{}",
        camerata_secrets::fixtures::body(
            35,
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            2026
        )
    )
}

/// The fixture copied to a temp dir, with the client config written in.
fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture_dir(), dir.path());
    write(
        dir.path(),
        "src/firebase.ts",
        &format!(
            "import {{ initializeApp }} from \"firebase/app\";\n\nconst firebaseConfig = {{\n  apiKey: \"{}\",\n  authDomain: \"orders-web.firebaseapp.com\",\n  projectId: \"orders-web\",\n  appId: \"1:123456789:web:abcdef\",\n}};\n\nexport const app = initializeApp(firebaseConfig);\n",
            google_key()
        ),
    );
    dir
}

fn selected() -> Vec<SelectedRule> {
    RULES
        .iter()
        .map(|id| SelectedRule {
            id: (*id).to_string(),
            directive: format!("firebase e2e: {id}"),
            repos: Vec::new(),
        })
        .collect()
}

async fn scan(dir: &Path, spec: &str) -> ScanReport {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let sources = vec![(spec.to_string(), dir.to_path_buf())];
    let (report, _manifest) = onboard::audit_repos_with_llm(
        &NoModel,
        &sources,
        &selected(),
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        false,
        false,
        false, // run_ai_review: zero model calls
        true,  // run_deterministic: the floor and the architectural engine
        BackendResolution::Api,
        Some(&corpus),
        &HashMap::new(),
    )
    .await;
    report
}

fn under<'a>(report: &'a ScanReport, id: &str) -> Vec<&'a Finding> {
    report.findings.iter().filter(|f| f.rule_id == id).collect()
}

fn is_not_assessed(f: &Finding) -> bool {
    matches!(f.kind, Some(FindingKind::NotAssessed(_)))
}

#[tokio::test]
async fn a_firebase_web_app_scans_end_to_end_with_no_false_critical_on_the_api_key() {
    let dir = fixture_repo();
    let report = scan(dir.path(), "acme/orders-web").await;

    // 1. The apiKey is public by design: no secrets-floor row anywhere.
    let sec: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.rule_id.starts_with("SEC-"))
        .collect();
    assert!(
        sec.is_empty(),
        "no SEC row for a Firebase web config: {sec:#?}"
    );

    // 2. The rules rows land where the design puts them.
    let open = under(&report, "FIREBASE-RULES-OPEN-1");
    // One asserted row (the open /orders write); the /public read is silent.
    assert_eq!(open.len(), 1, "{open:#?}");
    assert_eq!(open[0].path, "firestore.rules");
    assert_eq!(open[0].line, 5);
    assert_eq!(open[0].severity, "high");
    assert!(!open[0].needs_review, "a write grant is asserted, not held");

    let ownership = under(&report, "FIREBASE-RULES-OWNERSHIP-1");
    assert_eq!(ownership.len(), 1, "{ownership:#?}");
    assert_eq!(ownership[0].line, 8);
    assert_eq!(ownership[0].severity, "high");

    let storage = under(&report, "FIREBASE-STORAGE-RULES-OPEN-1");
    assert_eq!(storage.len(), 1, "{storage:#?}");
    assert_eq!(storage[0].path, "storage.rules");
    assert!(storage[0].needs_review);

    let functions = under(&report, "FIREBASE-FUNCTIONS-AUTH-1");
    assert_eq!(functions.len(), 1, "{functions:#?}");
    assert_eq!(functions[0].path, "functions/src/index.ts");
    assert!(functions[0].needs_review, "held, never asserted");

    assert_eq!(under(&report, "FIREBASE-APP-CHECK-1").len(), 1);
    assert_eq!(under(&report, "FIREBASE-RULES-TESTS-1").len(), 1);
    assert!(under(&report, "FIREBASE-ADMIN-SDK-CLIENT-1").is_empty());
    assert!(
        report.findings.iter().all(|f| !is_not_assessed(f)),
        "every file in the fixture is readable and deployed"
    );

    // 3. The coverage sentence is present, in client voice, and carries no dash.
    let note = report
        .coverage_notes
        .iter()
        .find(|n| n.tool == camerata_server::FIREBASE_SCOPE_COVERAGE_TOOL)
        .expect("a Firebase repo discloses what Firebase coverage means");
    assert!(note
        .message
        .contains("security rules files in the repository"));
    assert!(note
        .message
        .contains("not visible to a repository inspection"));
    assert!(!note.message.contains('\u{2014}') && !note.message.contains('\u{2013}'));

    // The detector ran and is credited.
    let entry = report
        .ledger
        .rule("FIREBASE-RULES-OPEN-1")
        .expect("in the ledger");
    assert!(entry.ran, "{entry:?}");
    assert_eq!(entry.findings_emitted, 1);
}

#[tokio::test]
async fn a_repo_without_firebase_gets_no_firebase_scope_sentence_and_no_firebase_rows() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{ "name": "api", "dependencies": { "express": "4.19.0" } }"#,
    );
    write(dir.path(), "src/index.ts", "export const a = 1;\n");
    let report = scan(dir.path(), "acme/api").await;
    assert!(
        report
            .coverage_notes
            .iter()
            .all(|n| n.tool != camerata_server::FIREBASE_SCOPE_COVERAGE_TOOL),
        "{:?}",
        report.coverage_notes
    );
    assert!(report
        .findings
        .iter()
        .all(|f| !f.rule_id.starts_with("FIREBASE-")));
}

#[tokio::test]
async fn a_google_key_outside_a_firebase_config_still_produces_a_floor_finding() {
    let dir = fixture_repo();
    write(
        dir.path(),
        "src/maps.ts",
        &format!("export const MAPS_KEY = \"{}\";\n", google_key()),
    );
    let report = scan(dir.path(), "acme/orders-web").await;
    let sec: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.rule_id.starts_with("SEC-"))
        .collect();
    assert!(
        !sec.is_empty(),
        "the control: the floor ran and still flags a bare key"
    );
    assert!(sec.iter().all(|f| f.path == "src/maps.ts"), "{sec:#?}");
}

#[tokio::test]
async fn a_rules_file_the_reader_cannot_read_is_not_assessed_and_not_credited_clean() {
    let dir = fixture_repo();
    // A statement the reader does not understand: it must not be read as "no findings".
    write(
        dir.path(),
        "firestore.rules",
        "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /orders/{id} { deny read; allow write: if true; }\n  }\n}\n",
    );
    let report = scan(dir.path(), "acme/orders-web").await;
    let firestore_rows: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.path == "firestore.rules" && f.rule_id == "FIREBASE-RULES-OPEN-1")
        .collect();
    assert_eq!(firestore_rows.len(), 1, "{firestore_rows:#?}");
    assert!(is_not_assessed(firestore_rows[0]), "{firestore_rows:#?}");
    assert_eq!(firestore_rows[0].severity, "info");
    assert!(firestore_rows[0].detail.contains("could not be read"));
    // The not-assessed row is not a finding, so the ledger counts none.
    let entry = report
        .ledger
        .rule("FIREBASE-RULES-OPEN-1")
        .expect("in the ledger");
    assert_eq!(entry.findings_emitted, 0, "{entry:?}");
}

/// Scans a real open-source Firebase app from disk. Point `CAMERATA_FIREBASE_SAMPLE` at a
/// checkout (a web app, a Flutter app or a Functions repo); the scan must complete, make no model
/// call and produce no secrets row for a Firebase web `apiKey`.
#[tokio::test]
#[ignore = "needs a checkout: set CAMERATA_FIREBASE_SAMPLE"]
async fn a_named_open_source_firebase_app_scans_from_disk() {
    let Ok(path) = std::env::var("CAMERATA_FIREBASE_SAMPLE") else {
        eprintln!("CAMERATA_FIREBASE_SAMPLE is not set; nothing to scan");
        return;
    };
    let report = scan(Path::new(&path), "sample/firebase-app").await;
    eprintln!(
        "{} findings, {} Firebase rows",
        report.findings.len(),
        report
            .findings
            .iter()
            .filter(|f| f.rule_id.starts_with("FIREBASE-"))
            .count()
    );
    assert!(
        report
            .findings
            .iter()
            .all(|f| !(f.rule_id.starts_with("SEC-")
                && f.detail.contains("Google API key")
                && f.snippet.contains("apiKey"))),
        "a Firebase web apiKey must not be reported as a secret"
    );
}
