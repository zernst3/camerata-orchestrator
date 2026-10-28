//! The manifest version bump that purges raw-snippet caches (docs/design/2026-10-23, paths E1/E2).
//! A v1 manifest holds findings whose snippets were written BEFORE construction-time redaction,
//! so a secret value sits on disk in the app data directory. Loading must drop it AND rewrite the
//! file at once, so the raw content is gone on first start, not on the next scan.

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::*;

fn v1_manifest_json(raw_snippet: &str) -> String {
    let finding = Finding {
        repo: "acme/app".to_string(),
        path: ".env".to_string(),
        line: 3,
        rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
        severity: "critical".to_string(),
        snippet: raw_snippet.to_string(),
        ..Finding::default()
    };
    let manifest = ScanManifest {
        version: 1,
        findings: vec![finding],
        ai_covered: true,
        ..ScanManifest::default()
    };
    let mut by_project = serde_json::Map::new();
    by_project.insert(
        "proj-1".to_string(),
        serde_json::to_value(&manifest).expect("manifest serializes"),
    );
    serde_json::json!({ "by_project": by_project }).to_string()
}

// Version 1 manifests carry raw snippets; the bump is what purges them. A compile-time check, so
// lowering the constant fails the build, not just a test run.
const _: () = assert!(MANIFEST_VERSION >= 2);

#[test]
fn scan_cache_manifest_v1_is_dropped_and_the_file_rewritten_on_load() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("scan-cache.json");
    std::fs::write(&path, v1_manifest_json(&p.text)).expect("write v1 cache");
    assert!(std::fs::read_to_string(&path)
        .expect("read")
        .contains(&p.value));

    let store = ScanCacheStore::load_or_new(path.clone());

    assert!(store.get("proj-1").is_none(), "a v1 manifest is not served");
    let on_disk = std::fs::read_to_string(&path).expect("the file still exists");
    assert!(
        !on_disk.contains(&p.value),
        "the raw value must be gone from disk on first load, not on the next scan"
    );
}

#[test]
fn a_current_manifest_survives_load_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("scan-cache.json");
    let store = ScanCacheStore::load_or_new(path.clone());
    let mut builder = ManifestBuilder::new();
    builder.record_repo(
        "acme/app",
        &[("a.rs".to_string(), "fn a() {}".to_string())],
        &[],
    );
    store.put("proj-1", builder.finish());

    let reloaded = ScanCacheStore::load_or_new(path);
    assert!(
        reloaded.get("proj-1").is_some(),
        "a current manifest is kept"
    );
}
