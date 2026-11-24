//! The diagnostic bundle's tests (design section 8). Every fixture names the made-up client
//! `acme`; a test that plants something asserts it never reaches the bundle JSON, the preview or
//! the written files.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Value};

use super::decisions::{DecisionTable, Node, ValueClass};
use super::leak_check::LeakKind;
use super::sanitize::{
    engine_location, hash_path, text_skeleton, BundleSalt, PathTokens, RepoInfo,
};
use super::shape::{probe, LineEnding};
use super::{build, build_with, write, BuildOptions, DiagnosticBundle, WriteError};

const RUN: &str = include_str!("fixtures/run.json");
const LEDGER: &str = include_str!("fixtures/ledger.json");
const FAILURES: &str = include_str!("fixtures/failures.json");

/// The path the design plants: a client file under the operator's home.
const CLIENT_PATH: &str = "/Users/zach/clients/acme/acme-web/src/billing/charge.ts";
/// The code line test 2 plants, and every identifier it holds.
const CODE_LINE: &str = "const invoiceTotal = applyDiscount(cartItems, couponCode);";
const CODE_IDENTIFIERS: [&str; 5] = [
    "const",
    "invoicetotal",
    "applydiscount",
    "cartitems",
    "couponcode",
];

fn fixtures() -> (Value, Value, Value) {
    (
        serde_json::from_str(RUN).expect("run fixture"),
        serde_json::from_str(LEDGER).expect("ledger fixture"),
        serde_json::from_str(FAILURES).expect("failures fixture"),
    )
}

fn opts() -> BuildOptions {
    BuildOptions {
        named_paths: false,
        shapes: false,
        known_rule_ids: ["RUST-DOMAIN-2", "RUST-DOMAIN-5"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        home_folder: Some("zach".to_string()),
        lookup_dir: None,
    }
}

fn bundle_of(
    run: &Value,
    ledger: &Value,
    failures: &Value,
    opts: &BuildOptions,
) -> DiagnosticBundle {
    build(run, Some(ledger), Some(failures), opts)
}

/// The JSON and the preview as one lower-cased text, for "never appears" assertions.
fn everything(bundle: &DiagnosticBundle) -> String {
    format!(
        "{}\n{}",
        serde_json::to_string(bundle.json()).unwrap(),
        bundle.preview()
    )
    .to_lowercase()
}

/// Every alphanumeric token of `text`, lower-cased.
fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn set(value: &mut Value, pointer: &str, new: Value) {
    *value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("fixture has no {pointer}")) = new;
}

fn repos() -> Vec<RepoInfo> {
    vec![
        RepoInfo {
            name: "acme-web".to_string(),
            dir: "/Users/zach/clients/acme/acme-web".to_string(),
        },
        RepoInfo {
            name: "acme-api".to_string(),
            dir: "/Users/zach/clients/acme/acme-api".to_string(),
        },
    ]
}

/// Every string value and key of `v`.
fn all_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| all_strings(v, out)),
        Value::Object(m) => {
            for (k, v) in m {
                out.push(k.clone());
                all_strings(v, out);
            }
        }
        _ => {}
    }
}

/// Write `bundle` into a fresh folder and return the folder with every file's text.
fn written(bundle: &DiagnosticBundle) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let paths = write(bundle, dir.path()).expect("the bundle is written");
    let mut text = std::fs::read_to_string(&paths.json).unwrap();
    text.push_str(&std::fs::read_to_string(&paths.preview).unwrap());
    (dir, text.to_lowercase())
}

// ── The pure functions ───────────────────────────────────────────────────────────────────────

#[test]
fn the_design_example_becomes_its_skeleton() {
    let salt = BundleSalt::from_bytes([7; 32]);
    let repos = repos();
    let mut paths = PathTokens::new(&salt, &repos, false);
    let got = text_skeleton(
        "failed to parse acme-web/src/billing/charge.ts: unexpected token \
         `const apiKey = \"sk_live_...\"` at line 41",
        &mut paths,
        1,
    );
    let re = regex::Regex::new(
        r"^failed to parse p:[0-9a-f]{12}\.ts unexpected token \[quoted \d+\] at line 41$",
    )
    .unwrap();
    assert!(re.is_match(&got), "skeleton was: {got}");
    // The path in the message hashes like the repo-relative path in a `files` list.
    assert!(got.contains(&paths.token("src/billing/charge.ts", 1)));
}

#[test]
fn unknown_words_collapse_and_the_skeleton_is_capped() {
    let salt = BundleSalt::from_bytes([1; 32]);
    let repos = repos();
    let mut paths = PathTokens::new(&salt, &repos, false);
    assert_eq!(
        text_skeleton("Zorblax Quuxington Frobnitz failed", &mut paths, 0),
        "_x3 failed"
    );
    assert_eq!(text_skeleton("Zorblax failed", &mut paths, 0), "_ failed");
    // A number of up to six digits stays, a longer one does not.
    assert_eq!(
        text_skeleton("HTTP 529 1234567", &mut paths, 0),
        "http 529 _"
    );
    let long = "error ".repeat(100);
    assert!(text_skeleton(&long, &mut paths, 0).chars().count() <= 200);
}

#[test]
fn a_secret_in_a_text_becomes_a_secret_marker() {
    let salt = BundleSalt::from_bytes([2; 32]);
    let repos = repos();
    let mut paths = PathTokens::new(&salt, &repos, false);
    for planted in camerata_secrets::fixtures::planted_all() {
        let got = text_skeleton(
            &format!("request failed with key {} and status 401", planted.value),
            &mut paths,
            0,
        );
        assert!(!got.contains(&planted.value), "{:?}: {got}", planted.shape);
        assert!(
            camerata_secrets::detect_secrets(&got).is_empty(),
            "{:?}: {got}",
            planted.shape
        );
        assert!(
            got.ends_with("and status 401"),
            "{:?}: {got}",
            planted.shape
        );
    }
}

#[test]
fn a_path_hashes_by_salt_repo_and_path() {
    let a = BundleSalt::from_bytes([3; 32]);
    let b = BundleSalt::from_bytes([4; 32]);
    let p = "src/billing/charge.ts";
    assert_eq!(hash_path(&a, 1, p), hash_path(&a, 1, p));
    assert_ne!(hash_path(&a, 1, p), hash_path(&b, 1, p));
    assert_ne!(hash_path(&a, 1, p), hash_path(&a, 2, p));
    assert_eq!(hash_path(&a, 1, p).len(), 12);
    assert!(hash_path(&a, 1, p).chars().all(|c| c.is_ascii_hexdigit()));
    // The salt's own bytes never show in its debug form.
    assert!(!format!("{a:?}").contains("3, 3"));
}

#[test]
fn engine_location_keeps_only_camerata_code() {
    assert_eq!(
        engine_location("crates/server/src/diagnostic_bundle/sanitize.rs:41").as_deref(),
        Some("crates/server/src/diagnostic_bundle/sanitize.rs:41")
    );
    for refused in [
        "/Users/zach/clients/acme/crates/server/src/x.rs",
        "/Users/zach/clients/acme/crates/server/src/x.rs:3",
        "crates/acme_billing/src/charge.rs",
        "crates/acme_billing/src/charge.rs:7",
        "crates/server/src/../../acme/x.rs:1",
        "src/billing/charge.ts:41",
        "crates/server/src/x.ts:3",
    ] {
        assert_eq!(engine_location(refused), None, "{refused}");
    }
}

// ── Design tests 1 to 9 ──────────────────────────────────────────────────────────────────────

/// Design test 1.
#[test]
fn a_planted_secret_never_reaches_the_bundle() {
    for planted in camerata_secrets::fixtures::planted_all() {
        let (mut run, mut ledger, mut failures) = fixtures();
        let text = format!("the provider said: {} (status 401)", planted.text);
        set(&mut failures, "/entries/0/message", json!(text));
        set(&mut failures, "/entries/0/attempts/0/message", json!(text));
        set(
            &mut failures,
            "/entries/0/attempts/1/stderr_tail",
            json!(text),
        );
        set(&mut run, "/plan/0/note", json!(text));
        set(&mut ledger, "/runner_notes/0", json!(text));
        set(&mut failures, "/runner_notes/0", json!(text));
        let bundle = bundle_of(&run, &ledger, &failures, &opts());
        let all = everything(&bundle);
        assert!(
            !all.contains(&planted.value.to_lowercase()),
            "{:?} reached the bundle",
            planted.shape
        );
        // Every string and key of the bundle, each as a reader sees it. (The serialized text as
        // one string is not searched: it joins the bundle's own field names into
        // `"secret_searched":3` pairs, which read as named secret literals.)
        let mut texts = Vec::new();
        all_strings(bundle.json(), &mut texts);
        for t in &texts {
            assert!(
                camerata_secrets::detect_secrets(t).is_empty(),
                "{:?}: a secret shape in `{t}`",
                planted.shape
            );
        }
        let (_dir, files) = written(&bundle);
        assert!(
            !files.contains(&planted.value.to_lowercase()),
            "{:?}",
            planted.shape
        );
    }
}

/// Design test 2.
#[test]
fn a_planted_code_line_in_an_error_message_never_reaches_the_bundle() {
    let quoted = format!("unexpected token `{CODE_LINE}` at line 41");
    let unquoted = format!("unexpected token {CODE_LINE} at line 41");
    for message in [quoted, unquoted] {
        let (mut run, mut ledger, mut failures) = fixtures();
        set(&mut failures, "/entries/0/message", json!(message));
        set(
            &mut failures,
            "/entries/0/attempts/0/message",
            json!(message),
        );
        set(
            &mut failures,
            "/entries/0/attempts/1/stderr_tail",
            json!(message),
        );
        set(&mut run, "/plan/0/note", json!(message));
        set(&mut ledger, "/rules/0/skip_reason", json!(message));
        let bundle = bundle_of(&run, &ledger, &failures, &opts());
        let seen = tokens(&everything(&bundle));
        for id in CODE_IDENTIFIERS {
            assert!(
                !seen.contains(id),
                "`{id}` of the code line reached the bundle"
            );
        }
        // The skeleton still says what happened and where.
        assert!(
            bundle.preview().contains("at line 41"),
            "{}",
            bundle.preview()
        );
        let (_dir, files) = written(&bundle);
        let seen = tokens(&files);
        for id in CODE_IDENTIFIERS {
            assert!(!seen.contains(id), "`{id}` reached a written file");
        }
    }
}

/// Design test 3.
#[test]
fn a_planted_client_path_never_reaches_the_bundle() {
    let (mut run, ledger, mut failures) = fixtures();
    set(
        &mut failures,
        "/entries/0/message",
        json!(format!("could not read {CLIENT_PATH}: permission denied")),
    );
    set(&mut failures, "/entries/0/files", json!([CLIENT_PATH]));
    set(&mut run, "/repos/0/dir", json!(CLIENT_PATH));
    let bundle = bundle_of(&run, &ledger, &failures, &opts());
    let (_dir, files) = written(&bundle);
    for text in [everything(&bundle), files] {
        assert!(!text.contains(&CLIENT_PATH.to_lowercase()));
        for word in ["acme", "billing", "charge", "zach"] {
            assert!(!text.contains(word), "`{word}` reached the bundle");
        }
    }
}

/// Design test 4.
#[test]
fn the_salt_is_not_in_the_bundle_and_two_bundles_hash_differently() {
    let (run, ledger, failures) = fixtures();
    let salt_bytes = [0x5a_u8; 32];
    let salt_hex: String = salt_bytes.iter().map(|b| format!("{b:02x}")).collect();
    let one = build_with(
        &run,
        Some(&ledger),
        Some(&failures),
        &opts(),
        &DecisionTable::standard(),
        BundleSalt::from_bytes(salt_bytes),
    );
    let all = everything(&one);
    assert!(!all.contains(&salt_hex));
    assert!(!all.contains("salt"), "the bundle names no salt at all");
    let (_dir, files) = written(&one);
    assert!(!files.contains(&salt_hex));

    let two = bundle_of(&run, &ledger, &failures, &opts());
    let token = |b: &DiagnosticBundle| {
        b.json()
            .pointer("/failures/entries/0/files/0")
            .and_then(Value::as_str)
            .expect("a hashed file")
            .to_string()
    };
    assert!(token(&one).starts_with("p:"), "{}", token(&one));
    assert_ne!(token(&one), token(&two));
    // Inside one bundle the same path gets the same token.
    let again = one
        .json()
        .pointer("/ledger/standard_fixes")
        .map(|_| ())
        .is_none();
    assert!(
        again,
        "standard fixes belong to the failure report, not the ledger"
    );
    let fix_path = one
        .json()
        .pointer("/failures/standard_fixes/0/path")
        .and_then(Value::as_str)
        .unwrap();
    assert_eq!(fix_path, token(&one));
}

/// Design test 5.
#[test]
fn named_paths_shows_paths_and_still_strips_secrets_and_code() {
    let (run, ledger, mut failures) = fixtures();
    let secret = camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::AnthropicKey);
    set(
        &mut failures,
        "/entries/0/message",
        json!(format!(
            "could not parse {CLIENT_PATH}: unexpected token `{CODE_LINE}` with key {}",
            secret.value
        )),
    );
    let mut named = opts();
    named.named_paths = true;
    let bundle = bundle_of(&run, &ledger, &failures, &named);
    let json = serde_json::to_string(bundle.json()).unwrap();
    assert!(json.contains("\"named_paths\":true"));
    assert_eq!(
        bundle.json().pointer("/failures/entries/0/files/0"),
        Some(&json!("src/billing/charge.ts"))
    );
    let message = bundle
        .json()
        .pointer("/failures/entries/0/message")
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("src/billing/charge.ts"), "{message}");
    let all = everything(&bundle);
    assert!(
        !all.contains("/users/zach"),
        "an absolute path is shown repo-relative"
    );
    assert!(!all.contains(&secret.value.to_lowercase()));
    let seen = tokens(&all);
    for id in CODE_IDENTIFIERS {
        assert!(!seen.contains(id), "`{id}` reached the bundle");
    }
    let (_dir, files) = written(&bundle);
    assert!(files.contains("src/billing/charge.ts"));
    // A path that climbs out of the repo is hashed even here.
    let salt = BundleSalt::from_bytes([8; 32]);
    let repos = repos();
    let mut named_tokens = PathTokens::new(&salt, &repos, true);
    assert!(named_tokens
        .token("../../Users/zach/notes.txt", 1)
        .starts_with("p:"));
    assert_eq!(named_tokens.token("src/a.ts", 1), "src/a.ts");
    assert!(!files.contains(&secret.value.to_lowercase()));
}

/// Every leaf of `value` as a key path: indices as `[]`, keys as they are.
fn leaf_paths(prefix: &str, value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                leaf_paths(&format!("{prefix}.{k}"), v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                leaf_paths(&format!("{prefix}[]"), v, out);
            }
        }
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

fn leaf_count(value: &Value) -> u64 {
    match value {
        Value::Object(map) => map.values().map(leaf_count).sum(),
        Value::Array(items) => items.iter().map(leaf_count).sum(),
        _ => 1,
    }
}

/// Design test 6, with the hand-written JSON fixtures the design allows before step 6.
#[test]
fn every_field_of_the_saved_run_types_has_a_decision() {
    let (run, ledger, failures) = fixtures();

    // The fixtures cover every field the saved types write: a field the types gained since makes
    // the round trip show a key path the fixture lacks.
    let round_trips: [(&str, &Value, Value); 3] = [
        (
            "run",
            &run,
            serde_json::to_value(
                serde_json::from_value::<crate::run_store::RunRecord>(run.clone()).unwrap(),
            )
            .unwrap(),
        ),
        (
            "ledger",
            &ledger,
            serde_json::to_value(
                serde_json::from_value::<crate::scan_ledger::ScanLedger>(ledger.clone()).unwrap(),
            )
            .unwrap(),
        ),
        (
            "failures",
            &failures,
            serde_json::to_value(
                serde_json::from_value::<crate::failure_report::FailureReport>(failures.clone())
                    .unwrap(),
            )
            .unwrap(),
        ),
    ];
    for (name, fixture, typed) in &round_trips {
        let mut in_fixture = BTreeSet::new();
        leaf_paths(name, fixture, &mut in_fixture);
        let mut in_type = BTreeSet::new();
        leaf_paths(name, typed, &mut in_type);
        let missing: Vec<_> = in_type.difference(&in_fixture).collect();
        assert!(
            missing.is_empty(),
            "the {name} fixture lacks fields the type writes (add them to the fixture and \
             give them a decision): {missing:?}"
        );
    }

    let bundle = bundle_of(&run, &ledger, &failures, &opts());
    let undecided: Vec<String> = bundle
        .json()
        .get("undecided")
        .and_then(Value::as_array)
        .expect("an undecided list")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    println!("undecided: {undecided:#?}");
    let unexpected: Vec<&String> = undecided
        .iter()
        .filter(|p| !(p.ends_with(".origin") || p.ends_with(".cause_origin")))
        .collect();
    assert!(
        unexpected.is_empty(),
        "leaves with no decision: {unexpected:?}"
    );
    for expected in [
        "failures.entries[].origin",
        "failures.entries[].cause_origin",
        "failures.entries[].attempts[].origin",
        "failures.entries[].attempts[].cause_origin",
    ] {
        assert!(
            undecided.iter().any(|p| p == expected),
            "{expected} is undecided"
        );
    }
    // The run's own origin (where it was started) is decided.
    assert_eq!(bundle.json().pointer("/run/origin"), Some(&json!("cli")));

    // Every leaf was visited.
    let coverage = bundle.json().get("coverage").expect("a coverage block");
    let total = leaf_count(&run) + leaf_count(&ledger) + leaf_count(&failures);
    assert_eq!(coverage.get("leaves"), Some(&json!(total)));
    let kept = coverage["kept"].as_u64().unwrap();
    let dropped = coverage["dropped"].as_u64().unwrap();
    let rejected = coverage["rejected"].as_u64().unwrap();
    let undecided_leaves = coverage["undecided"].as_u64().unwrap();
    assert_eq!(kept + dropped + rejected + undecided_leaves, total);
    assert_eq!(undecided_leaves, 4);
    assert!(bundle.leak_result().is_ok(), "{:?}", bundle.leak_result());
}

/// Design test 7.
#[test]
fn leak_check_refuses_the_write() {
    let (run, ledger, failures) = fixtures();
    // A table that wrongly lets the repo folder through.
    let wrong = DecisionTable::standard().with("run.repos[].dir", Node::Leaf(ValueClass::Allowed));
    let bundle = build_with(
        &run,
        Some(&ledger),
        Some(&failures),
        &opts(),
        &wrong,
        BundleSalt::generate(),
    );
    let dir = tempfile::tempdir().unwrap();
    match write(&bundle, dir.path()) {
        Err(WriteError::Leak(refusal)) => {
            assert_eq!(refusal.key_path, "run.repos[0].dir");
            assert_ne!(refusal.kind, LeakKind::Secret);
            // The refusal names where, never what.
            assert!(!format!("{refusal}").to_lowercase().contains("acme"));
        }
        other => panic!("the write was not refused: {other:?}"),
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing is written"
    );

    // A table that wrongly lets a message through verbatim, with a secret in it.
    let (run, ledger, mut failures) = fixtures();
    let secret = camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::GitHubToken);
    set(&mut failures, "/entries/1/message", json!(secret.text));
    let wrong = DecisionTable::standard().with(
        "failures.entries[].message",
        Node::Leaf(ValueClass::Verbatim),
    );
    let bundle = build_with(
        &run,
        Some(&ledger),
        Some(&failures),
        &opts(),
        &wrong,
        BundleSalt::generate(),
    );
    match write(&bundle, dir.path()) {
        Err(WriteError::Leak(refusal)) => {
            assert_eq!(refusal.key_path, "failures.entries[1].message");
            assert_eq!(refusal.kind, LeakKind::Secret);
        }
        other => panic!("the write was not refused: {other:?}"),
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// Design test 10.
#[test]
fn a_client_path_in_an_origin_field_is_dropped() {
    let (run, ledger, mut failures) = fixtures();
    let planted_a = "/Users/zach/clients/acme/crates/server/src/x.rs";
    let planted_b = "crates/acme_billing/src/charge.rs";
    set(
        &mut failures,
        "/entries/0/origin",
        json!(format!("{planted_a}:12")),
    );
    set(
        &mut failures,
        "/entries/0/cause_origin",
        json!({"file": planted_b, "line": 7}),
    );
    set(
        &mut failures,
        "/entries/0/attempts/0/origin",
        json!(planted_b),
    );
    set(
        &mut failures,
        "/entries/0/attempts/0/cause_origin",
        json!(planted_a),
    );
    let bundle = bundle_of(&run, &ledger, &failures, &opts());
    let (_dir, files) = written(&bundle);
    for text in [everything(&bundle), files] {
        assert!(!text.contains(&planted_a.to_lowercase()));
        assert!(!text.contains(planted_b));
        assert!(!text.contains("acme"));
        assert!(!text.contains("crates/server/src/x.rs"));
    }
    assert_eq!(engine_location(planted_a), None);
    assert_eq!(engine_location(planted_b), None);
}

/// Design test 13.
#[test]
fn the_shape_probe_returns_numbers_only() {
    let dir = tempfile::tempdir().unwrap();
    let secret =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey);
    let body = format!("{}\r\n{CODE_LINE}\r\n", secret.text);
    std::fs::create_dir_all(dir.path().join("src/billing")).unwrap();
    std::fs::write(dir.path().join("src/billing/charge.ts"), &body).unwrap();

    let shapes = probe(
        dir.path(),
        &[
            "src/billing/charge.ts".to_string(),
            "src/gone.ts".to_string(),
        ],
    );
    assert_eq!(shapes.len(), 2);
    let shape = &shapes[0];
    assert!(shape.present);
    assert_eq!(shape.bytes, Some(body.len() as u64));
    assert_eq!(shape.lines, Some(2));
    assert_eq!(shape.line_ending, Some(LineEnding::Crlf));
    assert_eq!(shape.utf8, Some(true));
    assert_eq!(shape.bom, Some(false));
    assert_eq!(shape.ends_with_newline, Some(true));
    assert_eq!(
        shape.longest_line,
        Some(secret.text.len().max(CODE_LINE.len()) as u64)
    );
    assert!(!shapes[1].present);

    let serialized = serde_json::to_string(&shapes).unwrap();
    // Every string the shapes hold is a line-ending word.
    let parsed: Value = serde_json::from_str(&serialized).unwrap();
    fn strings(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
            Value::Object(m) => m.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    strings(&parsed, &mut found);
    for s in &found {
        assert!(["lf", "crlf", "mixed", "none"].contains(&s.as_str()), "{s}");
    }
    // No word of the file shows.
    let keys: BTreeSet<String> = tokens(
        &serde_json::to_string(&json!({
            "present": 1, "bytes": 1, "lines": 1, "longest_line": 1, "line_ending": 1,
            "utf8": 1, "bom": 1, "ends_with_newline": 1
        }))
        .unwrap(),
    );
    for word in tokens(&body) {
        if word.len() >= 3 && !keys.contains(&word) && word.parse::<u64>().is_err() {
            assert!(
                !tokens(&serialized).contains(&word),
                "`{word}` of the file shows"
            );
        }
    }
    assert!(!serialized.contains(&secret.value));
}

#[test]
fn the_shapes_of_a_failed_part_ride_with_its_failure() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("acme-web");
    std::fs::create_dir_all(repo.join("src/billing")).unwrap();
    std::fs::write(
        repo.join("src/billing/charge.ts"),
        "let a = 1;\nlet b = 2;\n",
    )
    .unwrap();
    let (mut run, ledger, failures) = fixtures();
    set(&mut run, "/repos/0/dir", json!(repo.to_string_lossy()));
    let mut with_shapes = opts();
    with_shapes.shapes = true;
    let bundle = bundle_of(&run, &ledger, &failures, &with_shapes);
    let shapes = bundle
        .json()
        .pointer("/failures/entries/0/shapes")
        .expect("shapes for the failed part");
    assert_eq!(shapes["status"], json!("measured"));
    let files = shapes["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(
        Some(&files[0]["path"]),
        bundle.json().pointer("/failures/entries/0/files/0")
    );
    assert_eq!(files[0]["lines"], json!(2));
    assert_eq!(files[1]["present"], json!(false));
    // A resolved entry is history: it is not probed.
    assert!(bundle
        .json()
        .pointer("/failures/entries/1/shapes")
        .is_none());

    // The checkout is gone.
    set(
        &mut run,
        "/repos/0/dir",
        json!(dir.path().join("gone").to_string_lossy()),
    );
    let bundle = bundle_of(&run, &ledger, &failures, &with_shapes);
    assert_eq!(
        bundle.json().pointer("/failures/entries/0/shapes/status"),
        Some(&json!("unavailable"))
    );
    // `--no-shapes`.
    let bundle = bundle_of(&run, &ledger, &failures, &opts());
    assert_eq!(
        bundle.json().pointer("/failures/entries/0/shapes/status"),
        Some(&json!("skipped"))
    );
}

// ── The rest of the contract ─────────────────────────────────────────────────────────────────

#[test]
fn the_bundle_keeps_what_a_developer_needs() {
    let (run, ledger, failures) = fixtures();
    let bundle = bundle_of(&run, &ledger, &failures, &opts());
    let j = bundle.json();
    assert_eq!(j.pointer("/run/status"), Some(&json!("incomplete")));
    assert_eq!(j.pointer("/run/attempt"), Some(&json!(2)));
    assert_eq!(
        j.pointer("/run/models/audit"),
        Some(&json!("model-audit-1"))
    );
    assert_eq!(j.pointer("/run/repos/0/repo"), Some(&json!("repo-1")));
    assert_eq!(j.pointer("/run/repos/0/tree_changed"), Some(&json!(true)));
    assert_eq!(j.pointer("/run/repos/1/tree_changed"), Some(&json!(false)));
    assert_eq!(j.pointer("/run/plan/0/id"), Some(&json!("repo-1:scan")));
    assert_eq!(j.pointer("/run/plan/1/id"), Some(&json!("merge")));
    assert_eq!(j.pointer("/run/args/repo"), Some(&json!(33)));
    assert_eq!(j.pointer("/run/args/verbose"), Some(&json!(false)));
    assert!(j.pointer("/run/run_id").is_none());
    assert!(j.pointer("/run/nonce_seed").is_none());
    assert!(j.pointer("/run/project_key").is_none());
    assert_eq!(
        j.pointer("/ledger/rules/0/rule_id"),
        Some(&json!("RUST-DOMAIN-2"))
    );
    let custom = j
        .pointer("/ledger/rules/1/rule_id")
        .and_then(Value::as_str)
        .unwrap();
    assert!(custom.starts_with("rule:"), "{custom}");
    assert_eq!(j.pointer("/ledger/rules/1/repo"), Some(&json!("repo-2")));
    assert_eq!(
        j.pointer("/failures/entries/0/attempts/0/error_type"),
        Some(&json!("overloaded_error"))
    );
    assert_eq!(
        j.pointer("/failures/entries/0/attempts/0/http_status"),
        Some(&json!(529))
    );
    assert!(j
        .pointer("/failures/entries/0/attempts/0/request_id")
        .is_none());
    assert_eq!(
        j.pointer("/failures/entries/0/message"),
        Some(&json!("anthropic api http 529 overloaded"))
    );
    assert_eq!(
        j.pointer("/failures/entries/0/class"),
        Some(&json!("transport"))
    );
    assert_eq!(j.pointer("/bundle/named_paths"), Some(&json!(false)));
    assert_eq!(j.pointer("/leak_check"), Some(&json!("passed")));
    let preview = bundle.preview();
    for heading in [
        "Engine",
        "Parts",
        "Failures",
        "Text values",
        "Undecided",
        "Leak check: passed",
    ] {
        assert!(
            preview.contains(heading),
            "the preview has no `{heading}`:\n{preview}"
        );
    }
}

#[test]
fn every_enum_list_names_real_serde_variants() {
    use crate::run_store::{PartKind, PartStatus, RunOrigin, RunStatus};
    fn check<T: serde::de::DeserializeOwned>(values: &[&str]) {
        for v in values {
            assert!(
                serde_json::from_value::<T>(json!(v)).is_ok(),
                "`{v}` is not a {}",
                std::any::type_name::<T>()
            );
        }
    }
    use super::decisions::enums;
    check::<RunStatus>(enums::RUN_STATUS);
    check::<RunOrigin>(enums::RUN_ORIGIN);
    check::<PartKind>(enums::PART_KIND);
    check::<PartStatus>(enums::PART_STATUS);
    check::<crate::scan_ledger::RuleTier>(enums::RULE_TIER);
    check::<crate::scan_ledger::SkipReason>(enums::SKIP_REASON);
    check::<crate::stage_telemetry::Stage>(enums::STAGE);
    check::<camerata_llm::failure::FailureClass>(enums::FAILURE_CLASS);
    check::<crate::possible_duplicate::check::Verdict>(enums::VERDICT);
    check::<camerata_api_types::project::ProjectBackend>(enums::BACKEND);
}

#[test]
fn the_lookup_file_maps_tokens_to_real_paths_and_stays_out_of_the_bundle() {
    let run_dir = tempfile::tempdir().unwrap();
    let (run, ledger, failures) = fixtures();
    let mut with_lookup = opts();
    with_lookup.lookup_dir = Some(run_dir.path().to_path_buf());
    let bundle = bundle_of(&run, &ledger, &failures, &with_lookup);
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("diagnostics");
    let paths = write(&bundle, &out).unwrap();
    let lookup_path = paths.lookup.expect("a lookup file");
    assert_eq!(lookup_path.parent(), Some(run_dir.path()));
    assert!(lookup_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&format!("diagnostic-lookup-{}", bundle.id())));
    let lookup: Value = serde_json::from_slice(&std::fs::read(&lookup_path).unwrap()).unwrap();
    let token = bundle
        .json()
        .pointer("/failures/entries/0/files/0")
        .and_then(Value::as_str)
        .unwrap();
    let bare = token.split('@').next().unwrap();
    assert_eq!(lookup.get(bare), Some(&json!("src/billing/charge.ts")));
    // The bundle folder holds the bundle and its preview, nothing else.
    let names: BTreeSet<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "a folder the bundle creates is private");
        let file_mode = std::fs::metadata(&paths.json).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
    }
}

#[test]
fn a_bundle_without_ledger_or_failures_says_so() {
    let (run, _, _) = fixtures();
    let bundle = build(&run, None, None, &opts());
    assert_eq!(bundle.json().get("ledger"), Some(&json!("absent")));
    assert_eq!(bundle.json().get("failures"), Some(&json!("absent")));
    assert!(bundle.leak_result().is_ok());
}

#[test]
fn the_preview_renders_again_from_a_written_bundle() {
    let (run, ledger, failures) = fixtures();
    let bundle = bundle_of(&run, &ledger, &failures, &opts());
    let out = tempfile::tempdir().unwrap();
    let paths = write(&bundle, out.path()).unwrap();
    let again = super::preview::render(
        &serde_json::from_slice(&std::fs::read(&paths.json).unwrap()).unwrap(),
    );
    assert_eq!(again, bundle.preview());
    let _ = Path::new(&paths.preview);
}
