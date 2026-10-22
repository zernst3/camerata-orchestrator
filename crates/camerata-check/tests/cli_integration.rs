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

fn bin() -> Command {
    Command::cargo_bin("camerata-check").expect("camerata-check binary must build")
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
