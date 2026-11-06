//! Black-box CLI integration tests for the compiled `camerata-check` binary — driven via
//! `assert_cmd`, exactly as a client's CI would invoke it (a real subprocess, real argv, real
//! exit code). Complements the unit tests in `src/lib.rs` that call `camerata_check::run`
//! directly.
//!
//! Fixture repos:
//! - `tests/fixtures/clean_repo/` — new to this crate: a fully compliant Supabase repo.
//! - `tests/fixtures/adversarial_repo/` — new to this crate: malformed SQL, a malformed
//!   `.camerata/architecture.toml`, and a non-UTF8 "SQL" file, all in one repo.
//! - `../server/tests/fixtures/supabase_rls_repo/` — REUSED (not duplicated) from the
//!   Pass 1 scan e2e suite: a planted, known `SUPABASE-RLS-ENABLED-1` violation.
//! - `../server/tests/fixtures/handler_no_db_unconfigured_repo/` — REUSED from the Pass 4c
//!   e2e suite: a direct-DB-call handler shape with NO `.camerata/architecture.toml` at all,
//!   already proven (inside Camerata) to produce zero deterministic findings (D3). Reused here
//!   to prove the STANDALONE binary honors the identical degradation.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The `supabase_rls_repo` fixture lives one directory up, in `camerata-server`'s own test
/// fixtures — reused here rather than duplicated (see module doc).
fn server_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../server/tests/fixtures")
        .join(name)
}

/// The binary, pointed explicitly at this repo's bundled rule corpus, so the corpus severity
/// bounds a run applies do not depend on the developer's `CAMERATA_CORPUS_PATH`.
fn bin() -> Command {
    let mut cmd = Command::cargo_bin("camerata-check").expect("camerata-check binary must build");
    cmd.env(
        "CAMERATA_CORPUS_PATH",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rules/principles"),
    );
    cmd
}

/// The config-gated rule ids: checkers that consult `.camerata/architecture.toml` and abstain
/// without it (D3). The D3 parity tests scope to exactly these via `--rule-id`, so they
/// exercise only the checkers they are about and not always-on checkers (e.g.
/// ARCH-DEAD-CODE-1) that legitimately report on a tiny fixture repo with no entry point.
const CONFIG_GATED_RULE_IDS: [&str; 4] = [
    "ARCH-HANDLER-NO-DB-1",
    "ARCH-STRICT-LAYERING-1",
    "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1",
    "ARCH-API-DTOS-1",
];

/// `bin()` with `--rule-id` already applied for every config-gated rule.
fn config_gated_bin() -> Command {
    let mut cmd = bin();
    for id in CONFIG_GATED_RULE_IDS {
        cmd.arg("--rule-id").arg(id);
    }
    cmd
}

// ── known violation: non-zero exit, finding present in both output formats ──

#[test]
fn known_violation_fixture_fails_with_human_output() {
    let repo = server_fixture("supabase_rls_repo");
    bin()
        .arg(&repo)
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("SUPABASE-RLS-ENABLED-1"))
        .stdout(predicate::str::contains("profiles"))
        .stdout(predicate::str::contains("deterministic"));
}

#[test]
fn known_violation_fixture_fails_with_json_output() {
    let repo = server_fixture("supabase_rls_repo");
    let output = bin()
        .arg(&repo)
        .arg("--format")
        .arg("json")
        .output()
        .expect("must run");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert_eq!(value["clean"], false);
    assert!(value["deterministic_count"].as_u64().unwrap() >= 1);
    let violations = value["violations"]
        .as_array()
        .expect("violations must be an array");
    assert!(
        violations
            .iter()
            .any(|v| v["rule_id"] == "SUPABASE-RLS-ENABLED-1"),
        "expected SUPABASE-RLS-ENABLED-1 in JSON violations: {stdout}"
    );
}

// ── clean fixture: zero exit ────────────────────────────────────────────────

#[test]
fn clean_fixture_passes_with_zero_exit() {
    let repo = fixture("clean_repo");
    bin()
        .arg(&repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("clean"));
}

#[test]
fn clean_fixture_json_reports_clean_true() {
    let repo = fixture("clean_repo");
    let output = bin()
        .arg(&repo)
        .arg("--format")
        .arg("json")
        .output()
        .expect("must run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert_eq!(value["clean"], true);
    assert_eq!(value["violations"].as_array().unwrap().len(), 0);
}

// ── D3 parity: no .camerata config -> config-gated rules don't hard-fail ───

#[test]
fn no_config_repo_config_gated_rules_do_not_hard_fail() {
    let repo = server_fixture("handler_no_db_unconfigured_repo");
    config_gated_bin().arg(&repo).assert().success();
}

// ── dead code runs in the CI gate (design: "scan tier and CI only") ─────────

#[test]
fn ci_gate_reports_dead_code_for_an_orphaned_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let write = |rel: &str, body: &str| {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    // A real entry point (package.json `main`) that imports one live module, plus one file
    // nothing imports or names anywhere: a genuine orphan.
    write("package.json", r#"{"private":true,"main":"src/app.ts"}"#);
    write("src/app.ts", "import './used';\n");
    write("src/used.ts", "export const used = 1;\n");
    write("src/orphan.ts", "export const orphan = 1;\n");

    let output = bin()
        .arg(tmp.path())
        .arg("--format")
        .arg("json")
        .output()
        .expect("must run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    let dead: Vec<&serde_json::Value> = value["violations"]
        .as_array()
        .expect("violations must be an array")
        .iter()
        .filter(|v| v["rule_id"] == "ARCH-DEAD-CODE-1")
        .collect();
    assert_eq!(
        dead.len(),
        1,
        "exactly the orphan must be reported, not the entry point or its import: {stdout}"
    );
    assert_eq!(dead[0]["file"], "src/orphan.ts", "{stdout}");
    // The corpus caps ARCH-DEAD-CODE-1 at `low`; the gate reports it there, as the scan and the
    // client's report do, and a low finding is advisory (exit 0), never a failed build.
    assert_eq!(value["corpus_bounds_applied"], true, "{stdout}");
    assert_eq!(dead[0]["severity"], "low", "{stdout}");
    assert_eq!(value["blocking_count"], 0, "{stdout}");
    assert_eq!(output.status.code(), Some(0), "{stdout}");
}

/// Without a loadable corpus the gate cannot know a rule's authored bounds: it keeps the
/// checker's severity, says the corpus bounds were not applied, and the human output tells the
/// reader how to point it at the corpus.
#[test]
fn ci_gate_without_a_corpus_says_its_bounds_were_not_applied() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"private":true,"main":"src/app.ts"}"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("src/app.ts"), "export const app = 1;\n").unwrap();
    std::fs::write(
        tmp.path().join("src/orphan.ts"),
        "export const orphan = 1;\n",
    )
    .unwrap();
    let missing = tmp.path().join("no-corpus-here");

    let output = bin()
        .env("CAMERATA_CORPUS_PATH", &missing)
        .arg(tmp.path())
        .arg("--format")
        .arg("json")
        .output()
        .expect("must run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert_eq!(value["corpus_bounds_applied"], false, "{stdout}");

    bin()
        .env("CAMERATA_CORPUS_PATH", &missing)
        .arg(tmp.path())
        .assert()
        .stdout(predicate::str::contains("CAMERATA_CORPUS_PATH"));
}

// ── React Hooks lint config gap runs in the CI gate (scan tier and CI only) ──

/// The CI binary loads only the files matching each checker's interest globs, so this pins that
/// the lint-configuration shapes (a dotfile included) reach the checker there.
#[test]
fn ci_gate_reports_a_react_repo_whose_lint_config_does_not_enable_the_hooks_rules() {
    let hooks_lint_violations = |eslintrc: Option<&str>| -> Vec<serde_json::Value> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let write = |rel: &str, body: &str| {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        write(
            "package.json",
            r#"{"name":"web","private":true,"main":"src/App.jsx","dependencies":{"react":"^18"}}"#,
        );
        write("src/App.jsx", "export const App = () => null;\n");
        if let Some(body) = eslintrc {
            write(".eslintrc.json", body);
        }
        let output = bin()
            .arg(tmp.path())
            .arg("--rule-id")
            .arg("UI-REACT-HOOKS-LINT-1")
            .arg("--format")
            .arg("json")
            .output()
            .expect("must run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: serde_json::Value =
            serde_json::from_str(&stdout).expect("stdout must be valid JSON");
        value["violations"]
            .as_array()
            .expect("violations must be an array")
            .clone()
    };

    let gap = hooks_lint_violations(Some(r#"{"extends":["eslint:recommended"]}"#));
    assert_eq!(gap.len(), 1, "{gap:?}");
    assert_eq!(gap[0]["rule_id"], "UI-REACT-HOOKS-LINT-1");
    assert_eq!(gap[0]["file"], ".eslintrc.json");

    let none = hooks_lint_violations(None);
    assert_eq!(none.len(), 1, "{none:?}");
    assert_eq!(none[0]["file"], "package.json");

    let enabled = hooks_lint_violations(Some(r#"{"extends":["plugin:react-hooks/recommended"]}"#));
    assert!(enabled.is_empty(), "{enabled:?}");
}

// ── adversarial: malformed SQL / malformed config / binary file never panics ─

#[test]
fn adversarial_fixture_never_panics_and_exits_cleanly_or_with_findings() {
    let repo = fixture("adversarial_repo");
    // The only hard contract: the process must exit 0 or 1 (a clean gate result or a real
    // finding) — NEVER crash (which `assert_cmd` would surface as a signal/abort, not a
    // regular exit code) and never exit 2 (this crate's own "run error" code).
    let assert = bin().arg(&repo).assert();
    let output = assert.get_output();
    let code = output.status.code();
    assert!(
        matches!(code, Some(0) | Some(1)),
        "adversarial input must degrade to a clean or ordinary-violation exit code, not crash: \
         status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn adversarial_fixture_json_output_is_still_valid_json() {
    let repo = fixture("adversarial_repo");
    let output = bin()
        .arg(&repo)
        .arg("--format")
        .arg("json")
        .output()
        .expect("must run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("adversarial run must still emit valid JSON: {e}\n{stdout}"));
}

// ── --rule-id filter ─────────────────────────────────────────────────────────

#[test]
fn rule_id_filter_narrows_to_selected_checker() {
    let repo = server_fixture("supabase_rls_repo");
    bin()
        .arg(&repo)
        .arg("--rule-id")
        .arg("UI-UTC-DATES-1") // unrelated to the RLS violation this fixture plants
        .assert()
        .success() // the RLS finding must not fire when only an unrelated rule id is selected
        .stdout(predicate::str::contains("clean"));
}

#[test]
fn unmatched_rule_id_surfaces_a_note_and_still_exits_clean() {
    let repo = fixture("clean_repo");
    bin()
        .arg(&repo)
        .arg("--rule-id")
        .arg("ARCH-STRUCTURED-ERRORS-1") // Group E: deliberately deferred, no native checker
        .assert()
        .success()
        .stdout(predicate::str::contains("ARCH-STRUCTURED-ERRORS-1"));
}

#[test]
fn clone_based_rule_ids_are_reported_unsupported_and_never_clean() {
    let repo = fixture("clean_repo");
    for id in [
        "ARCH-SINGLE-SOURCE-LOGIC-1",
        "ARCH-NAMING-ONE-CONCEPT-1",
        "UI-SHARED-PRIMITIVE-1",
    ] {
        // Human output: not clean, names the id, says why. The default gate does not fail on a
        // rule it cannot evaluate (a CI author who lists every corpus id must not be locked
        // out), but it never says "clean".
        bin()
            .arg(&repo)
            .arg("--rule-id")
            .arg(id)
            .assert()
            .success()
            .stdout(predicate::str::contains("NOT CLEAN"))
            .stdout(predicate::str::contains(id))
            .stdout(predicate::str::contains("advisory tier"))
            .stdout(predicate::str::contains("camerata-check: clean").not());

        // --strict: a rule that was never checked fails the gate.
        bin()
            .arg(&repo)
            .arg("--rule-id")
            .arg(id)
            .arg("--strict")
            .assert()
            .failure()
            .code(1);

        // JSON: `clean` is false and the id is listed as unsupported, not as unmatched.
        let out = bin()
            .arg(&repo)
            .arg("--rule-id")
            .arg(id)
            .arg("--format")
            .arg("json")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&out).expect("valid json");
        assert_eq!(value["clean"], false, "{id}");
        assert_eq!(
            value["unsupported_rule_ids"],
            serde_json::json!([id]),
            "{id}"
        );
        assert_eq!(value["unmatched_rule_ids"], serde_json::json!([]), "{id}");
    }
}

// ── --strict ─────────────────────────────────────────────────────────────────

#[test]
fn strict_flag_fails_on_needs_review_only_findings() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("main.rs"),
        "fn f() { tokio::process::Command::new(\"x\").spawn().unwrap(); }\n",
    )
    .unwrap();

    // Default: needs-review-only findings must not fail the gate.
    bin().arg(tmp.path()).assert().success();

    // --strict: the same findings must now fail the gate.
    bin()
        .arg(tmp.path())
        .arg("--strict")
        .assert()
        .failure()
        .code(1);
}

// ── --config override ────────────────────────────────────────────────────────

#[test]
fn config_override_flag_is_honored() {
    let repo = server_fixture("handler_no_db_unconfigured_repo");
    let cfg_dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = cfg_dir.path().join("architecture.toml");
    std::fs::write(
        &cfg_path,
        "version = 1\n[layers]\nhandlers = [\"src/routes/**\"]\n[imports]\nhandlers = []\n\
         [db]\nhandles = [\"db\"]\nallowed_in = []\n",
    )
    .unwrap();

    // Without the override: unconfigured repo -> clean (D3).
    config_gated_bin().arg(&repo).assert().success();

    // With the override: the same repo now has a config that makes HandlerNoDbChecker
    // deterministic, so the direct-DB-call handler must now fail the gate.
    config_gated_bin()
        .arg(&repo)
        .arg("--config")
        .arg(&cfg_path)
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("ARCH-HANDLER-NO-DB-1"));
}

#[test]
fn missing_config_override_path_is_a_run_error_exit_code_2() {
    let repo = fixture("clean_repo");
    bin()
        .arg(&repo)
        .arg("--config")
        .arg("/does/not/exist/architecture.toml")
        .assert()
        .failure()
        .code(2);
}

// ── Firebase rules through the real walk (item 24) ──────────────────────────

/// The Firebase rules checker reads `firestore.rules`, `firebase.json` and `.firebaserc`:
/// dot and non-code extensions the pruned file walk must reach.
#[test]
fn a_firestore_rules_file_is_checked_through_the_real_walk_and_fails_ci() {
    let repo = server_fixture("firebase-app");
    let output = bin()
        .arg(&repo)
        .arg("--format")
        .arg("json")
        .arg("--rule-id")
        .arg("FIREBASE-RULES-OPEN-1")
        .arg("--rule-id")
        .arg("FIREBASE-RULES-OWNERSHIP-1")
        .output()
        .expect("must run");
    assert_eq!(output.status.code(), Some(1), "defects are deterministic");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let violations = value["violations"].as_array().unwrap();
    let open = violations
        .iter()
        .find(|v| v["rule_id"] == "FIREBASE-RULES-OPEN-1")
        .unwrap_or_else(|| panic!("open rule missing: {stdout}"));
    assert_eq!(open["file"], "firestore.rules");
    assert_eq!(open["line"], 5);
    assert_eq!(open["needs_review"], false);
    assert!(violations
        .iter()
        .any(|v| v["rule_id"] == "FIREBASE-RULES-OWNERSHIP-1"));
}

/// A Realtime Database rules file `firebase.json` renames is reached by the real walk even
/// though the checker's own globs no longer take every `.json` file.
#[test]
fn a_renamed_realtime_database_rules_file_is_checked_through_the_real_walk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("firebase.json"),
        r#"{ "database": { "rules": "config/db-rules.json" } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("config")).unwrap();
    std::fs::write(
        dir.path().join("config/db-rules.json"),
        r#"{ "rules": { ".read": true, ".write": true } }"#,
    )
    .unwrap();
    let output = bin()
        .arg(dir.path())
        .arg("--format")
        .arg("json")
        .arg("--rule-id")
        .arg("FIREBASE-RTDB-RULES-OPEN-1")
        .output()
        .expect("must run");
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert!(value["violations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["file"] == "config/db-rules.json"));
}

#[test]
fn an_unreadable_rules_file_is_listed_as_not_assessed_and_is_never_clean() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("firebase.json"),
        r#"{ "firestore": { "rules": "firestore.rules" } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("firestore.rules"),
        "service cloud.firestore {\n  match /databases/{d}/documents {\n    deny read;\n  }\n}\n",
    )
    .unwrap();
    let run = |strict: bool| {
        let mut cmd = bin();
        cmd.arg(dir.path())
            .arg("--format")
            .arg("json")
            .arg("--rule-id")
            .arg("FIREBASE-RULES-OPEN-1");
        if strict {
            cmd.arg("--strict");
        }
        cmd.output().expect("must run")
    };
    let default_run = run(false);
    assert_eq!(default_run.status.code(), Some(0), "not a violation");
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&default_run.stdout)).unwrap();
    assert_eq!(value["clean"], false, "an unassessed input is never clean");
    assert_eq!(value["violations"].as_array().unwrap().len(), 0);
    let not_assessed = value["not_assessed"].as_array().unwrap();
    assert_eq!(not_assessed.len(), 1);
    assert_eq!(not_assessed[0]["file"], "firestore.rules");
    assert_eq!(run(true).status.code(), Some(1), "strict fails on it");

    bin()
        .arg(dir.path())
        .arg("--rule-id")
        .arg("FIREBASE-RULES-OPEN-1")
        .assert()
        .success()
        .stdout(predicate::str::contains("Not assessed"))
        .stdout(predicate::str::contains("NOT CLEAN"));
}

/// One malformed rule file must not disable every bound: the gate loads the corpus leniently,
/// names the skipped file on stderr, still applies the bounds of the rules that did load (the
/// dead-code ceiling), and does NOT claim the corpus was fully applied.
#[test]
fn ci_gate_with_one_malformed_corpus_file_still_bounds_but_reports_a_partial_corpus() {
    let corpus = tempfile::tempdir().expect("tempdir");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rules/principles/universal/arch-dead-code-1.toml"),
        corpus.path().join("arch-dead-code-1.toml"),
    )
    .unwrap();
    std::fs::write(corpus.path().join("broken.toml"), "this is = not [a rule").unwrap();

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"private":true,"main":"src/app.ts"}"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("src/app.ts"), "export const app = 1;\n").unwrap();
    std::fs::write(
        tmp.path().join("src/orphan.ts"),
        "export const orphan = 1;\n",
    )
    .unwrap();

    let output = bin()
        .env("CAMERATA_CORPUS_PATH", corpus.path())
        .arg(tmp.path())
        .arg("--format")
        .arg("json")
        .output()
        .expect("must run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert_eq!(value["corpus_bounds_applied"], false, "{stdout}");
    assert!(stderr.contains("broken.toml"), "{stderr}");
    let dead = value["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["rule_id"] == "ARCH-DEAD-CODE-1")
        .unwrap_or_else(|| panic!("dead code must be reported: {stdout}"));
    assert_eq!(
        dead["severity"], "low",
        "the loaded rule's cap still applies"
    );
}
