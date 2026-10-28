#![allow(clippy::unwrap_used)]
//! `camerata inspect --export` and the release gate (docs/design/2026-10-23_secret-redaction-and-
//! release-gate.md section 7.4): when the gate refuses the package, the command fails with exit
//! code 3, prints the gate's block (which never carries the value), and writes NO zip.
//!
//! A real way for a value to reach the package: the scanned file's PATH. Findings redact their
//! snippet, detail and fix text at construction, but a path is client text no producer rewrites,
//! so a file NAMED after a key carries it into every artifact. The gate is what stops that.

use std::path::Path;

use camerata::inspect_cmd::{
    run_inspect_with_key_presence, InspectArgs, InspectError, EXIT_RELEASE_BLOCKED,
};
use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;
use camerata_server::llm::ProjectBackend;

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

fn git(dest: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dest)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn cli_inspect_export_exits_3_and_writes_nothing_when_blocked() {
    // Test isolation: with the dependency audit disabled no osv-scanner run happens, so neither
    // the CISA KEV nor the FIRST EPSS feed is fetched and nothing is written to the real user
    // data dir's vuln-intel cache. (`vuln_intel::enrich_dependency_findings` honours the same
    // variable.)
    let _no_dep_audit = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    if !typst_on_path() {
        eprintln!("skipping cli_inspect_export_exits_3_and_writes_nothing_when_blocked: no typst");
        return;
    }
    let p = planted(SecretShape::StripeLiveSecretKey);
    let repo = tempfile::tempdir().expect("repo dir");
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    // A source file whose NAME is the key, holding a hardcoded credential.
    std::fs::write(
        repo.path().join(format!("src/{}.ts", p.value)),
        format!("export const stripeApiKey = \"{}\";\n", p.value),
    )
    .unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(
        repo.path(),
        &["config", "user.email", "gate@camerata.local"],
    );
    git(repo.path(), &["config", "user.name", "Gate Test"]);
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &["commit", "-q", "-m", "a file named after a key"],
    );

    let export_dir = tempfile::tempdir().expect("export dir");
    let export_path = export_dir.path().join("inspect.zip");
    let args = InspectArgs {
        repo: repo.path().to_path_buf(),
        export: export_path.clone(),
        backend: ProjectBackend::Api,
        batch: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
    };

    let err = run_inspect_with_key_presence(args, false)
        .await
        .expect_err("the gate must refuse a package whose path carries a key");

    assert!(matches!(err, InspectError::ReleaseBlocked(_)), "{err}");
    assert_eq!(err.exit_code(), EXIT_RELEASE_BLOCKED);
    assert_eq!(EXIT_RELEASE_BLOCKED, 3);
    let shown = err.to_string();
    assert!(
        shown.starts_with("EXPORT BLOCKED by the release gate"),
        "{shown}"
    );
    assert!(
        !shown.contains(&p.value),
        "the block printed to stderr must never carry the value"
    );
    assert!(
        !export_path.exists(),
        "a blocked export must leave NOTHING on disk"
    );
}
