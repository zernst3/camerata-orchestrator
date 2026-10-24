//! `ScanCacheStore::remove_repo`: source deletion drops ONE repo's slice of the incremental-scan
//! cache and leaves every other repo (and project) exactly as it was.

use std::collections::BTreeMap;

use super::{ScanCacheStore, ScanManifest, MANIFEST_VERSION};
use crate::onboard::Finding;

fn finding(repo: &str, path: &str) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        snippet: format!("let secret_marker = \"{repo}:{path}\";"),
        ..Finding::default()
    }
}

fn manifest(repos: &[(&str, &[&str])]) -> ScanManifest {
    let mut files = BTreeMap::new();
    let mut findings = Vec::new();
    for (repo, paths) in repos {
        let mut per_repo = BTreeMap::new();
        for p in *paths {
            per_repo.insert((*p).to_string(), "00ff".to_string());
            findings.push(finding(repo, p));
        }
        files.insert((*repo).to_string(), per_repo);
    }
    ScanManifest {
        version: MANIFEST_VERSION,
        files,
        findings,
        ..ScanManifest::default()
    }
}

#[test]
fn only_the_named_repo_is_removed_from_a_multi_repo_project() {
    let store = ScanCacheStore::new();
    store.put(
        "p1",
        manifest(&[("acme/app", &["a.rs", "b.rs"]), ("acme/lib", &["l.rs"])]),
    );

    let removed = store.remove_repo("p1", "acme/app");

    assert!(removed.had_entry);
    assert_eq!(removed.files, 2);
    assert!(removed.bytes > 0);
    let left = store.get("p1").expect("the other repo keeps the manifest");
    assert_eq!(left.files.keys().collect::<Vec<_>>(), ["acme/lib"]);
    assert_eq!(left.findings.len(), 1);
    assert_eq!(left.findings[0].repo, "acme/lib");
}

#[test]
fn peek_reports_what_would_go_without_removing_it() {
    let store = ScanCacheStore::new();
    store.put(
        "p1",
        manifest(&[("acme/app", &["a.rs", "b.rs"]), ("acme/lib", &["l.rs"])]),
    );

    let peeked = store.peek_repo("p1", "acme/app");
    let removed = store.remove_repo("p1", "acme/app");

    assert_eq!(peeked, removed, "the preview matches the removal");
    assert!(peeked.had_entry && peeked.files == 2);
    assert!(
        !store.peek_repo("p1", "acme/app").had_entry,
        "gone after removal"
    );
    assert!(!store.peek_repo("nope", "acme/app").had_entry);
}

#[test]
fn removing_the_last_repo_drops_the_whole_project_entry() {
    let store = ScanCacheStore::new();
    store.put("p1", manifest(&[("acme/app", &["a.rs"])]));
    store.put("p2", manifest(&[("acme/app", &["a.rs"])]));

    assert!(store.remove_repo("p1", "acme/app").had_entry);

    assert!(store.get("p1").is_none());
    assert!(store.get("p2").is_some(), "another project is untouched");
}

#[test]
fn the_repo_match_ignores_case() {
    let store = ScanCacheStore::new();
    store.put(
        "p1",
        manifest(&[("Acme/App", &["a.rs"]), ("acme/lib", &["l.rs"])]),
    );
    assert!(store.remove_repo("p1", "acme/app").had_entry);
    let left = store.get("p1").unwrap();
    assert!(left.files.keys().all(|k| k != "Acme/App"));
    assert!(left.findings.iter().all(|f| f.repo != "Acme/App"));
}

#[test]
fn an_unknown_project_or_repo_removes_nothing() {
    let store = ScanCacheStore::new();
    store.put("p1", manifest(&[("acme/app", &["a.rs"])]));
    assert!(!store.remove_repo("nope", "acme/app").had_entry);
    assert!(!store.remove_repo("p1", "acme/other").had_entry);
    assert_eq!(store.get("p1").unwrap().findings.len(), 1);
}

#[test]
fn the_removal_is_written_to_disk_and_the_excerpt_is_gone_from_the_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("scan-cache.json");
    let store = ScanCacheStore::load_or_new(path.clone());
    store.put(
        "p1",
        manifest(&[("acme/app", &["a.rs"]), ("acme/lib", &["l.rs"])]),
    );
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("acme/app:a.rs"));

    store.remove_repo("p1", "acme/app");

    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(!on_disk.contains("acme/app"), "{on_disk}");
    assert!(on_disk.contains("acme/lib:l.rs"));
    // A restart sees the same.
    let reloaded = ScanCacheStore::load_or_new(path);
    assert_eq!(reloaded.get("p1").unwrap().files.len(), 1);
}
