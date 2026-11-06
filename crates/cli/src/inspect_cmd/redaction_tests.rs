//! The CLI's incremental-scan cache (docs/design/2026-10-23_secret-redaction-and-release-gate.md,
//! path E2). A version 1 manifest holds findings stored before construction-time redaction, so a
//! secret value sits in `~/Library/Caches/camerata/<repo-key>.json`. Loading it must drop it AND
//! delete the file, so the raw content is gone on first run, not whenever a scan overwrites it.

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;
use camerata_server::onboard::Finding;
use camerata_server::scan_cache::ScanManifest;
use camerata_test_env::EnvGuard;

use super::*;

fn v1_manifest(raw_snippet: &str) -> ScanManifest {
    ScanManifest {
        version: 1,
        ai_covered: true,
        findings: vec![Finding {
            repo: "acme/app".to_string(),
            path: ".env".to_string(),
            line: 3,
            rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
            snippet: raw_snippet.to_string(),
            ..Finding::default()
        }],
        ..ScanManifest::default()
    }
}

#[test]
#[serial_test::serial(process_env)]
fn load_prior_manifest_rejects_v1_and_deletes_it() {
    let state = tempfile::tempdir().expect("state dir");
    let _home = EnvGuard::set("HOME", state.path());
    let _xdg = EnvGuard::set("XDG_CACHE_HOME", state.path());
    let repo = tempfile::tempdir().expect("repo dir");

    let p = planted(SecretShape::SupabaseSecretKey);
    save_manifest(&manifest_cache_path(repo.path()), &v1_manifest(&p.text));
    let cache = manifest_cache_path(repo.path());
    assert!(
        std::fs::read_to_string(&cache)
            .expect("the v1 cache was written")
            .contains(&p.value),
        "precondition: the raw value is on disk"
    );

    assert!(
        load_prior_manifest(&manifest_cache_path(repo.path())).is_none(),
        "a v1 manifest is never reused"
    );
    assert!(
        !cache.exists(),
        "the stale cache, raw snippets and all, is removed on first load"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn load_prior_manifest_keeps_a_current_manifest() {
    let state = tempfile::tempdir().expect("state dir");
    let _home = EnvGuard::set("HOME", state.path());
    let _xdg = EnvGuard::set("XDG_CACHE_HOME", state.path());
    let repo = tempfile::tempdir().expect("repo dir");

    let current = camerata_server::scan_cache::ManifestBuilder::new().finish();
    save_manifest(&manifest_cache_path(repo.path()), &current);
    assert!(load_prior_manifest(&manifest_cache_path(repo.path())).is_some());
    assert!(manifest_cache_path(repo.path()).exists());
}
