//! Cloud Functions: discovery (design section 8.3) and FIREBASE-FUNCTIONS-AUTH-1 (section 7.10).

use super::files;
use crate::arch_checker::{
    ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_INFO, SEVERITY_MEDIUM,
};
use crate::firebase::functions::{discover, HandlerKind};
use crate::firebase::{FirebaseFunctionsChecker, RULE_FUNCTIONS_AUTH};

const MANIFEST: &str =
    r#"{ "dependencies": { "firebase-functions": "^5.0.0", "firebase-admin": "^12.0.0" } }"#;

const HEAD_V2: &str = "import { onRequest, onCall } from \"firebase-functions/v2/https\";\nimport { getFirestore } from \"firebase-admin/firestore\";\nconst db = getFirestore();\n";

fn package(source: &str) -> Vec<(String, String)> {
    files(&[
        ("functions/package.json", MANIFEST),
        ("functions/src/index.ts", source),
    ])
}

fn run_functions(f: &[(String, String)]) -> Vec<ArchViolation> {
    FirebaseFunctionsChecker.check(&RepoView {
        spec: "t/r",
        files: f,
    })
}

fn auth_rows(all: &[ArchViolation]) -> Vec<&ArchViolation> {
    all.iter()
        .filter(|v| v.rule_id == RULE_FUNCTIONS_AUTH && v.grade == ViolationGrade::Review)
        .collect()
}

fn handlers(f: &[(String, String)]) -> Vec<(String, HandlerKind)> {
    discover(f)
        .packages
        .iter()
        .flat_map(|p| p.handlers.iter().map(|h| (h.name.clone(), h.kind)))
        .collect()
}

// ── discovery ───────────────────────────────────────────────────────────────────────────────

#[test]
fn finds_v2_on_request_and_on_call_with_and_without_options() {
    let src = format!(
        "{HEAD_V2}export const a = onRequest((req, res) => {{ res.send(1); }});\nexport const b = onRequest({{ cors: true }}, (req, res) => {{ res.send(1); }});\nexport const c = onCall((request) => 1);\nexport const d = onCall({{ region: \"eu\" }}, (request) => 1);\n"
    );
    let found = handlers(&package(&src));
    assert_eq!(found.len(), 4, "{found:?}");
    assert_eq!(
        found
            .iter()
            .filter(|(_, k)| *k == HandlerKind::Request)
            .count(),
        2
    );
    assert_eq!(
        found
            .iter()
            .filter(|(_, k)| *k == HandlerKind::Call)
            .count(),
        2
    );
}

#[test]
fn finds_v1_chained_https_handlers() {
    let src = "import * as functions from \"firebase-functions\";\nexport const a = functions.https.onRequest((req, res) => res.send(1));\nexport const b = functions.region(\"eu\").https.onCall((data, context) => 1);\nexport const c = functions.runWith({ memory: \"1GB\" }).https.onRequest((req, res) => res.send(1));\nexport const sched = functions.pubsub.schedule(\"every 5 minutes\").onRun(() => null);\n";
    let found = handlers(&package(src));
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"], "{found:?}");
}

#[test]
fn finds_express_app_handlers_and_searches_the_package() {
    let src = format!(
        "{HEAD_V2}import express from \"express\";\nconst app = express();\napp.post(\"/orders\", async (req, res) => {{ await db.collection(\"orders\").add(req.body); res.end(); }});\nexport const api = onRequest(app);\n"
    );
    let d = discover(&package(&src));
    let h = &d.packages[0].handlers[0];
    assert!(h.express_app, "{h:?}");
    assert_eq!(h.name, "api");
    // The write is in the route, not in the handler argument: the package-wide search finds it.
    let rows = run_functions(&package(&src));
    assert_eq!(auth_rows(&rows).len(), 1, "{rows:#?}");
}

#[test]
fn exported_names_are_resolved_for_const_exports_and_module_exports() {
    let src = "const { onRequest } = require(\"firebase-functions/v2/https\");\nconst one = onRequest((req, res) => res.send(1));\nexports.createUser = onRequest((req, res) => res.send(1));\nmodule.exports = { twoOfThem: onRequest((req, res) => res.send(2)) };\nonRequest((req, res) => res.send(3));\n";
    let found = handlers(&package(src));
    let mut names: Vec<String> = found.into_iter().map(|(n, _)| n).collect();
    names.sort();
    assert_eq!(names, vec!["<anonymous>", "createUser", "one", "twoOfThem"]);
}

#[test]
fn compiled_lib_and_dist_folders_are_not_read() {
    let f = files(&[
        ("functions/package.json", MANIFEST),
        (
            "functions/lib/index.js",
            "const { onRequest } = require(\"firebase-functions/v2/https\");\nexports.a = onRequest((q, s) => s.send(1));\n",
        ),
        (
            "functions/dist/index.js",
            "const { onRequest } = require(\"firebase-functions/v2/https\");\nexports.b = onRequest((q, s) => s.send(1));\n",
        ),
    ]);
    assert!(handlers(&f).is_empty());
}

#[test]
fn a_functions_folder_without_a_manifest_is_not_a_package() {
    let f = files(&[(
        "functions/src/index.ts",
        &format!("{HEAD_V2}export const a = onRequest((req, res) => res.send(1));\n"),
    )]);
    assert!(discover(&f).packages.is_empty());
}

#[test]
fn multiple_codebases_from_firebase_json_are_all_found() {
    let one = format!("{HEAD_V2}export const a = onRequest((req, res) => res.send(1));\n");
    let f = files(&[
        (
            "firebase.json",
            r#"{ "functions": [ { "source": "fn-a", "codebase": "a" }, { "source": "fn-b", "codebase": "b" } ] }"#,
        ),
        ("fn-a/package.json", MANIFEST),
        ("fn-a/src/index.ts", &one),
        ("fn-b/package.json", MANIFEST),
        ("fn-b/src/index.ts", &one),
    ]);
    let d = discover(&f);
    assert_eq!(d.packages.len(), 2);
    assert!(d.packages.iter().all(|p| p.handlers.len() == 1));
}

#[test]
fn an_unknown_handler_binding_is_not_a_candidate() {
    let src = format!(
        "{HEAD_V2}import {{ handler }} from \"./elsewhere\";\nexport const a = onRequest(handler);\n"
    );
    assert!(handlers(&package(&src)).is_empty());
}

// ── FIREBASE-FUNCTIONS-AUTH-1 ───────────────────────────────────────────────────────────────

#[test]
fn http_function_that_writes_with_admin_sdk_and_no_verify_is_review_graded() {
    let src = format!(
        "{HEAD_V2}\nexport const createOrder = onRequest(async (req, res) => {{\n  await db.collection(\"orders\").add(req.body);\n  res.send(\"ok\");\n}});\n"
    );
    let all = run_functions(&package(&src));
    let rows = auth_rows(&all);
    assert_eq!(rows.len(), 1, "{all:#?}");
    assert_eq!(rows[0].grade, ViolationGrade::Review);
    assert_eq!(rows[0].severity, SEVERITY_MEDIUM);
    assert_eq!(rows[0].file, "functions/src/index.ts");
    assert_eq!(rows[0].line, 5);
    assert_eq!(rows[0].object.as_deref(), Some("createOrder"));
    assert!(
        rows[0].message.contains("HTTP function `createOrder`"),
        "{}",
        rows[0].message
    );
    assert!(rows[0].message.contains("verifyIdToken"));
    assert!(rows[0].message.contains("bypasses security rules"));
}

#[test]
fn http_function_with_verify_id_token_middleware_anywhere_in_the_package_is_silent() {
    let src = format!(
        "{HEAD_V2}export const createOrder = onRequest(async (req, res) => {{ await db.collection(\"orders\").add(req.body); res.send(1); }});\n"
    );
    let mut f = package(&src);
    f.push((
        "functions/src/auth.ts".into(),
        "export const guard = async (t: string) => getAuth().verifyIdToken(t);\n".into(),
    ));
    assert!(auth_rows(&run_functions(&f)).is_empty());
}

#[test]
fn http_function_verifying_app_check_is_silent() {
    let src = format!(
        "{HEAD_V2}export const createOrder = onRequest(async (req, res) => {{ await getAppCheck().verifyToken(req.header(\"X-Firebase-AppCheck\")); await db.collection(\"orders\").add(req.body); res.send(1); }});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
}

#[test]
fn twin_stripe_webhook_is_silent() {
    let src = format!(
        "{HEAD_V2}export const stripeEvents = onRequest(async (req, res) => {{ const event = stripe.webhooks.constructEvent(req.rawBody, req.headers[\"stripe-signature\"], secret); await db.collection(\"payments\").add(event); res.send(1); }});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
}

#[test]
fn twin_twilio_webhook_is_silent() {
    // Twilio's signature check is `validateRequest` (the SDK) over the `X-Twilio-Signature` header.
    for check in [
        "twilio.validateRequest(token, req.headers[\"x-twilio-signature\"], url, req.body)",
        "const sig = req.header(\"X-Twilio-Signature\");",
    ] {
        let src = format!(
            "{HEAD_V2}export const smsReply = onRequest(async (req, res) => {{ {check} await db.collection(\"messages\").add(req.body); res.send(1); }});\n"
        );
        assert!(
            auth_rows(&run_functions(&package(&src))).is_empty(),
            "{check}"
        );
    }
}

#[test]
fn twin_private_invoker_is_silent() {
    let src = format!(
        "{HEAD_V2}export const internalSync = onRequest({{ invoker: \"private\" }}, async (req, res) => {{ await db.collection(\"x\").add(req.body); res.send(1); }});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
    // An explicitly public invoker is still a candidate.
    let public = src.replace("private", "public");
    assert_eq!(auth_rows(&run_functions(&package(&public))).len(), 1);
}

#[test]
fn twin_health_endpoint_is_silent() {
    let src = format!(
        "{HEAD_V2}export const healthCheck = onRequest(async (req, res) => {{ await db.collection(\"pings\").add({{ at: 1 }}); res.send(1); }});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
}

#[test]
fn twin_read_only_http_function_is_silent() {
    let src = format!(
        "{HEAD_V2}export const listItems = onRequest(async (req, res) => {{ const snap = await db.collection(\"items\").get(); res.json(snap.docs.map((d) => d.data())); }});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
}

#[test]
fn callable_without_an_auth_check_that_deletes_by_caller_id_is_review_graded() {
    let src = format!(
        "{HEAD_V2}\nexport const deleteAccount = onCall(async (request) => {{\n  await db.doc(`users/${{request.data.uid}}`).delete();\n}});\n"
    );
    let all = run_functions(&package(&src));
    let rows = auth_rows(&all);
    assert_eq!(rows.len(), 1, "{all:#?}");
    assert_eq!(rows[0].line, 5);
    assert!(
        rows[0]
            .message
            .contains("callable function `deleteAccount`"),
        "{}",
        rows[0].message
    );
    assert!(rows[0].message.contains("never checks `request.auth`"));
    assert!(rows[0].message.contains("caller's input"));
}

#[test]
fn callable_checking_request_auth_is_silent() {
    let src = format!(
        "{HEAD_V2}export const deleteAccount = onCall(async (request) => {{\n  if (!request.auth) {{ throw new HttpsError(\"unauthenticated\", \"x\"); }}\n  await db.doc(`users/${{request.auth.uid}}`).delete();\n}});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
    // Destructured `auth`.
    let destructured = format!(
        "{HEAD_V2}export const deleteAccount = onCall(async ({{ auth, data }}) => {{ await db.doc(`users/${{data.uid}}`).delete(); }});\n"
    );
    assert!(auth_rows(&run_functions(&package(&destructured))).is_empty());
}

#[test]
fn callable_checking_context_auth_is_silent() {
    let src = "import * as functions from \"firebase-functions\";\nimport { getFirestore } from \"firebase-admin/firestore\";\nconst db = getFirestore();\nexport const del = functions.https.onCall(async (data, context) => {\n  if (!context.auth) { throw new functions.https.HttpsError(\"unauthenticated\", \"x\"); }\n  await db.doc(`users/${data.uid}`).delete();\n});\n";
    assert!(auth_rows(&run_functions(&package(src))).is_empty());
    // The same v1 callable without the check is a candidate.
    let bare = src.replace(
        "if (!context.auth) { throw new functions.https.HttpsError(\"unauthenticated\", \"x\"); }",
        "",
    );
    assert_eq!(auth_rows(&run_functions(&package(&bare))).len(), 1);
}

#[test]
fn callable_using_a_require_auth_helper_is_silent() {
    let src = format!(
        "{HEAD_V2}export const deleteAccount = onCall(async (request) => {{\n  requireAuth(request);\n  await db.doc(`users/${{request.data.uid}}`).delete();\n}});\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
}

#[test]
fn callable_wrapped_by_with_auth_is_silent() {
    let src = format!(
        "{HEAD_V2}export const deleteAccount = onCall(withAuth(async (request) => {{ await db.doc(`users/${{request.data.uid}}`).delete(); }}));\n"
    );
    assert!(auth_rows(&run_functions(&package(&src))).is_empty());
    let unwrapped = src.replace("withAuth", "withLogging");
    assert_eq!(auth_rows(&run_functions(&package(&unwrapped))).len(), 1);
}

#[test]
fn twin_scheduled_and_firestore_triggers_are_never_candidates() {
    let src = "import { onSchedule } from \"firebase-functions/v2/scheduler\";\nimport { onDocumentWritten } from \"firebase-functions/v2/firestore\";\nimport { getFirestore } from \"firebase-admin/firestore\";\nconst db = getFirestore();\nexport const nightly = onSchedule(\"every day 00:00\", async () => { await db.collection(\"a\").add({}); });\nexport const mirror = onDocumentWritten(\"a/{id}\", async () => { await db.collection(\"b\").add({}); });\n";
    assert!(auth_rows(&run_functions(&package(src))).is_empty());
}

#[test]
fn a_python_functions_package_is_not_assessed_rather_than_clean() {
    let f = files(&[
        (
            "functions/requirements.txt",
            "firebase-functions\nfirebase-admin\n",
        ),
        (
            "functions/main.py",
            "from firebase_functions import https_fn\n",
        ),
    ]);
    let all = run_functions(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
    assert_eq!(all[0].severity, SEVERITY_INFO);
    assert!(all[0].message.contains("Python"), "{}", all[0].message);
    assert!(!FirebaseFunctionsChecker.config_unsatisfied_for(&RepoView {
        spec: "t/r",
        files: &f,
    }));
}

#[test]
fn test_files_inside_the_package_are_skipped() {
    let f = files(&[
        ("functions/package.json", MANIFEST),
        (
            "functions/src/index.test.ts",
            &format!("{HEAD_V2}export const a = onRequest(async (req, res) => {{ await db.collection(\"x\").add(1); }});\n"),
        ),
    ]);
    assert!(handlers(&f).is_empty());
}

#[test]
fn the_functions_checker_is_a_scan_rule_not_a_write_time_bounce() {
    assert!(!FirebaseFunctionsChecker.runs_at_write_time());
    assert!(FirebaseFunctionsChecker.advisory_coexisting());
    assert!(!FirebaseFunctionsChecker.defects_need_review());
}
