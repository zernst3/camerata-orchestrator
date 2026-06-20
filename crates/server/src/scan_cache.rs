//! Incremental-scan cache: skip the expensive AI audit on files that haven't changed.
//!
//! The AI audit re-reads the codebase every run, and on a large repo that's the bulk of the
//! token bill (see `estimate_audit_cost`). But most files don't change between scans. This
//! module persists a per-project **scan manifest** — a content fingerprint for every audited
//! file plus the AI findings from the last scan — so a re-scan can:
//!
//!   1. **partition** the current files into *changed* (new or edited) vs *unchanged*,
//!   2. run the AI audit on the *changed* set only, and
//!   3. **carry forward** the cached findings for unchanged files that are still present.
//!
//! The deterministic security floor (`audit_files`) is token-free and always runs over the
//! whole tree, so the floor is never stale; only the AI pass is short-circuited. A file whose
//! content changes gets a new fingerprint and is re-audited (its stale cached findings are
//! dropped); a deleted file's findings fall away because we only carry findings for paths that
//! are still in the working tree.
//!
//! The manifest is a local cost-optimization cache (it lives in the app data dir, NOT in the
//! repo and NOT in git) — losing it only means the next scan is a full scan. The user can force
//! a full scan at any time (the "Full scan (ignore incremental cache)" control), which ignores
//! the prior manifest and rewrites it from a clean pass.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::onboard::Finding;

/// Bump if the fingerprint scheme or manifest shape changes in a way that should invalidate
/// every existing manifest (forcing one clean full scan after an upgrade).
const MANIFEST_VERSION: u32 = 1;

/// Fingerprint a file's EXACT content with the stable FNV-1a hash the suppression baseline uses.
///
/// Unlike the baseline's snippet fingerprint, this is byte-exact (no whitespace normalization):
/// a cached finding carries a line number, and even a whitespace-only reformat shifts lines, so
/// "unchanged" must mean byte-identical for carried findings to stay accurate. A reformatted file
/// is therefore treated as changed and re-audited — the safe, correct trade.
pub fn content_fingerprint(content: &str) -> String {
    format!("{:016x}", crate::suppression::fnv1a(content))
}

/// One project's incremental-scan manifest: every audited file's fingerprint, keyed by
/// `repo` → `path`, plus the AI findings produced for that file set on the last scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanManifest {
    /// Manifest schema version (see [`MANIFEST_VERSION`]).
    #[serde(default)]
    pub version: u32,
    /// Fingerprint of the selected RULE SET this manifest's findings were produced under. If the
    /// architect changes which rules are selected, carried findings would reflect the OLD rules,
    /// so a manifest whose rules fingerprint differs from the current scan is treated as no cache
    /// (a clean full scan), guaranteeing findings always match the current rule selection.
    #[serde(default)]
    pub rules_fingerprint: String,
    /// `repo` (`owner/repo`) → (`path` → content fingerprint) for every file audited last run.
    #[serde(default)]
    pub files: BTreeMap<String, BTreeMap<String, String>>,
    /// The AI (advisory) findings from the last scan. Each finding carries its own `repo` +
    /// `path`, so carry-forward is a filter over this list. Deterministic floor findings are
    /// NOT cached here — they are cheap to recompute and must never go stale.
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// Fingerprint the selected rule set (ids + their repo bindings) so a change to the selection
/// invalidates the incremental cache. Order-independent: ids and bindings are sorted first.
pub fn rules_fingerprint<'a>(rules: impl Iterator<Item = (&'a str, &'a [String])>) -> String {
    let mut parts: Vec<String> = rules
        .map(|(id, repos)| {
            let mut r: Vec<&str> = repos.iter().map(String::as_str).collect();
            r.sort_unstable();
            format!("{}@{}", id, r.join(","))
        })
        .collect();
    parts.sort_unstable();
    format!("{:016x}", crate::suppression::fnv1a(&parts.join("|")))
}

impl ScanManifest {
    /// A manifest is usable only if it matches the current schema version AND was produced under
    /// the same rule selection. A version mismatch (post-upgrade) or rule-set change is treated
    /// as "no cache" → a clean full scan.
    pub fn is_current(&self) -> bool {
        self.version == MANIFEST_VERSION
    }

    /// Whether this manifest can be reused for a scan running under `rules_fp`.
    pub fn matches_rules(&self, rules_fp: &str) -> bool {
        self.rules_fingerprint == rules_fp
    }

    /// The fingerprint recorded for a file on the last scan, if any.
    fn fingerprint_of(&self, repo: &str, path: &str) -> Option<&str> {
        self.files
            .get(repo)
            .and_then(|m| m.get(path))
            .map(String::as_str)
    }
}

/// The result of partitioning a repo's current files against a prior manifest.
#[derive(Debug, Clone, Default)]
pub struct Partition {
    /// Files whose content is new or changed since the last scan — the AI audit runs on these.
    pub changed: Vec<(String, String)>,
    /// Cached AI findings carried forward for UNCHANGED files that are still present.
    pub carried: Vec<Finding>,
    /// How many of the repo's files were unchanged (and so skipped by the AI audit).
    pub unchanged_count: usize,
}

impl Partition {
    /// Total files considered (changed + unchanged) — for the report's "scanned N, reused M".
    pub fn total(&self) -> usize {
        self.changed.len() + self.unchanged_count
    }
}

/// Partition `files` (one repo's current working-tree files, `(path, content)`) against the
/// prior manifest for `repo`.
///
/// - A file is **changed** when it is new (no prior fingerprint) or its fingerprint differs.
/// - A file is **unchanged** when its fingerprint matches the manifest; its cached AI findings
///   are carried forward.
/// - A file that existed before but is gone now simply isn't in `files`, so its cached findings
///   are not carried (they fall away — the finding can't apply to code that no longer exists).
///
/// When `prior` is `None` (no cache, or a forced full scan) everything is "changed" and nothing
/// is carried — i.e. a full scan.
/// `prior` must already be rule-compatible (the caller filters via [`ScanManifest::matches_rules`])
/// and current; an incompatible or absent manifest yields a full scan.
pub fn partition(
    prior: Option<&ScanManifest>,
    repo: &str,
    files: &[(String, String)],
) -> Partition {
    let Some(prior) = prior.filter(|m| m.is_current()) else {
        return Partition {
            changed: files.to_vec(),
            carried: Vec::new(),
            unchanged_count: 0,
        };
    };

    let mut changed = Vec::new();
    let mut unchanged_paths = std::collections::HashSet::new();
    for (path, content) in files {
        let fp = content_fingerprint(content);
        match prior.fingerprint_of(repo, path) {
            Some(prev) if prev == fp => {
                unchanged_paths.insert(path.clone());
            }
            _ => changed.push((path.clone(), content.clone())),
        }
    }

    // Carry forward only findings for THIS repo's unchanged, still-present files.
    let carried: Vec<Finding> = prior
        .findings
        .iter()
        .filter(|f| f.repo == repo && unchanged_paths.contains(&f.path))
        .cloned()
        .collect();

    Partition {
        changed,
        carried,
        unchanged_count: unchanged_paths.len(),
    }
}

/// Accumulates a fresh manifest across a multi-repo scan, then finalizes it for persistence.
///
/// For each repo the caller records (a) the current fingerprints of EVERY file that was in the
/// working tree this run (both changed and unchanged), and (b) the AI findings that now apply
/// to that repo (fresh findings from the changed set ∪ carried-forward findings). Calling
/// [`finish`](ManifestBuilder::finish) stamps the version and yields the manifest to save.
#[derive(Debug, Default)]
pub struct ManifestBuilder {
    rules_fingerprint: String,
    files: BTreeMap<String, BTreeMap<String, String>>,
    findings: Vec<Finding>,
}

impl ManifestBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamp the rule-set fingerprint this scan ran under (see [`rules_fingerprint`]) so a later
    /// scan under a different selection invalidates this manifest.
    pub fn with_rules_fingerprint(mut self, fp: String) -> Self {
        self.rules_fingerprint = fp;
        self
    }

    /// Record one repo's full current file set (fingerprinting each) and the AI findings that
    /// apply to it after this scan. `ai_findings` must already be the post-merge AI set for the
    /// repo (fresh ∪ carried); deterministic-floor findings should NOT be passed here.
    pub fn record_repo(&mut self, repo: &str, files: &[(String, String)], ai_findings: &[Finding]) {
        let entry = self.files.entry(repo.to_string()).or_default();
        for (path, content) in files {
            entry.insert(path.clone(), content_fingerprint(content));
        }
        self.findings
            .extend(ai_findings.iter().filter(|f| f.repo == repo).cloned());
    }

    /// Finalize into a persistable manifest stamped with the current schema version.
    pub fn finish(self) -> ScanManifest {
        ScanManifest {
            version: MANIFEST_VERSION,
            rules_fingerprint: self.rules_fingerprint,
            files: self.files,
            findings: self.findings,
        }
    }
}

// ── persistence ─────────────────────────────────────────────────────────────

/// All projects' manifests, keyed by project id, persisted as one JSON file in the app data
/// dir (mirrors `ProjectStore`). A write failure never breaks a scan — the cache is best-effort.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheState {
    /// project id → that project's scan manifest.
    by_project: BTreeMap<String, ScanManifest>,
}

/// Thread-safe, file-backed store of per-project scan manifests.
#[derive(Clone, Default)]
pub struct ScanCacheStore {
    inner: std::sync::Arc<Mutex<CacheState>>,
    /// `None` = in-memory only (tests).
    path: Option<std::sync::Arc<std::path::PathBuf>>,
}

impl ScanCacheStore {
    /// An empty, non-persisted store (tests / in-memory use).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from `path` (or start empty), persisting every change back to it.
    pub fn load_or_new(path: std::path::PathBuf) -> Self {
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<CacheState>(&s).ok())
            .unwrap_or_default();
        Self {
            inner: std::sync::Arc::new(Mutex::new(state)),
            path: Some(std::sync::Arc::new(path)),
        }
    }

    /// The stored manifest for a project, if any (and only if it's the current schema version).
    pub fn get(&self, project_id: &str) -> Option<ScanManifest> {
        let s = self.inner.lock().ok()?;
        s.by_project
            .get(project_id)
            .filter(|m| m.is_current())
            .cloned()
    }

    /// Replace a project's manifest and persist.
    pub fn put(&self, project_id: &str, manifest: ScanManifest) {
        if let Ok(mut s) = self.inner.lock() {
            s.by_project.insert(project_id.to_string(), manifest);
        }
        self.save();
    }

    /// Drop a project's manifest (e.g. on a forced full scan the next run rebuilds it anyway,
    /// but an explicit clear is available) and persist.
    pub fn clear(&self, project_id: &str) {
        if let Ok(mut s) = self.inner.lock() {
            s.by_project.remove(project_id);
        }
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(state) = self.inner.lock() else {
            return;
        };
        if let Ok(json) = serde_json::to_string_pretty(&*state) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path.as_path(), json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> (String, String) {
        (path.to_string(), content.to_string())
    }

    fn finding(repo: &str, path: &str, rule: &str) -> Finding {
        Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line: 1,
            rule_id: rule.to_string(),
            severity: "medium".to_string(),
            snippet: "x".to_string(),
            detail: "d".to_string(),
            status: "active".to_string(),
            also_matches: Vec::new(),
        }
    }

    #[test]
    fn fingerprint_is_byte_exact() {
        assert_eq!(
            content_fingerprint("fn a() {}"),
            content_fingerprint("fn a() {}"),
            "identical content fingerprints identically"
        );
        // Byte-exact: even a whitespace-only change flips it, because a reformat shifts the
        // line numbers carried findings depend on, so the file must be re-audited.
        assert_ne!(
            content_fingerprint("fn a() {}"),
            content_fingerprint("fn   a()   {}"),
            "a whitespace reformat counts as changed (line numbers shift)"
        );
        assert_ne!(
            content_fingerprint("fn a() {}"),
            content_fingerprint("fn b() {}"),
            "a code edit flips the fingerprint"
        );
    }

    #[test]
    fn partition_with_no_prior_treats_everything_as_changed() {
        let files = vec![file("a.rs", "1"), file("b.rs", "2")];
        let p = partition(None, "me/api", &files);
        assert_eq!(p.changed.len(), 2);
        assert_eq!(p.unchanged_count, 0);
        assert!(p.carried.is_empty());
        assert_eq!(p.total(), 2);
    }

    #[test]
    fn partition_carries_unchanged_and_reaudits_changed() {
        let files = vec![file("a.rs", "code a"), file("b.rs", "code b")];
        // Build a manifest as if a.rs + b.rs were scanned and each produced one AI finding.
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &files,
            &[
                finding("me/api", "a.rs", "ARCH-1"),
                finding("me/api", "b.rs", "ARCH-2"),
            ],
        );
        let prior = b.finish();

        // b.rs changes; a.rs is identical.
        let next = vec![file("a.rs", "code a"), file("b.rs", "code b CHANGED")];
        let p = partition(Some(&prior), "me/api", &next);

        assert_eq!(p.changed.len(), 1, "only b.rs is re-audited");
        assert_eq!(p.changed[0].0, "b.rs");
        assert_eq!(p.unchanged_count, 1, "a.rs is unchanged");
        assert_eq!(p.carried.len(), 1, "a.rs's cached finding carries forward");
        assert_eq!(p.carried[0].path, "a.rs");
        assert_eq!(p.carried[0].rule_id, "ARCH-1");
    }

    #[test]
    fn partition_does_not_carry_findings_for_deleted_files() {
        let files = vec![file("a.rs", "code a"), file("gone.rs", "old")];
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &files,
            &[
                finding("me/api", "a.rs", "ARCH-1"),
                finding("me/api", "gone.rs", "ARCH-9"),
            ],
        );
        let prior = b.finish();

        // gone.rs is deleted; a.rs unchanged.
        let next = vec![file("a.rs", "code a")];
        let p = partition(Some(&prior), "me/api", &next);

        assert_eq!(p.unchanged_count, 1);
        assert_eq!(
            p.carried.len(),
            1,
            "only the surviving file's finding carries"
        );
        assert_eq!(p.carried[0].path, "a.rs");
        assert!(
            !p.carried.iter().any(|f| f.path == "gone.rs"),
            "a deleted file's findings must not be carried forward"
        );
    }

    #[test]
    fn partition_scopes_carry_forward_by_repo() {
        let api = vec![file("a.rs", "shared")];
        let web = vec![file("a.rs", "shared")];
        let mut b = ManifestBuilder::new();
        b.record_repo("me/api", &api, &[finding("me/api", "a.rs", "API-RULE")]);
        b.record_repo("me/web", &web, &[finding("me/web", "a.rs", "WEB-RULE")]);
        let prior = b.finish();

        let p = partition(Some(&prior), "me/api", &api);
        assert_eq!(p.carried.len(), 1);
        assert_eq!(
            p.carried[0].rule_id, "API-RULE",
            "must not pull the other repo's finding"
        );
    }

    #[test]
    fn rules_fingerprint_is_order_independent_and_change_sensitive() {
        let a = rules_fingerprint(
            [("ARCH-1", &[][..]), ("SEC-2", &["me/api".to_string()][..])].into_iter(),
        );
        // Same rules, different order + different repo order → same fingerprint.
        let b = rules_fingerprint(
            [("SEC-2", &["me/api".to_string()][..]), ("ARCH-1", &[][..])].into_iter(),
        );
        assert_eq!(a, b, "fingerprint must be order-independent");
        // Adding a rule changes it.
        let c = rules_fingerprint(
            [
                ("ARCH-1", &[][..]),
                ("SEC-2", &["me/api".to_string()][..]),
                ("NEW-3", &[][..]),
            ]
            .into_iter(),
        );
        assert_ne!(a, c, "a changed rule selection must change the fingerprint");
        // Changing a binding changes it.
        let d = rules_fingerprint(
            [("ARCH-1", &[][..]), ("SEC-2", &["me/web".to_string()][..])].into_iter(),
        );
        assert_ne!(a, d, "a changed repo binding must change the fingerprint");
    }

    #[test]
    fn manifest_with_mismatched_rules_is_rejected_by_caller_check() {
        let mut b = ManifestBuilder::new().with_rules_fingerprint("RULESET-A".to_string());
        b.record_repo(
            "me/api",
            &[file("a.rs", "x")],
            &[finding("me/api", "a.rs", "R1")],
        );
        let m = b.finish();
        assert!(m.matches_rules("RULESET-A"));
        assert!(
            !m.matches_rules("RULESET-B"),
            "a different rule set must not match"
        );
        // The caller filters on matches_rules before partition; simulate that: a mismatched
        // manifest is treated as no cache → full scan.
        let prior = Some(&m).filter(|m| m.matches_rules("RULESET-B"));
        let p = partition(prior, "me/api", &[file("a.rs", "x")]);
        assert_eq!(p.changed.len(), 1, "rule-set change forces a full re-scan");
        assert!(p.carried.is_empty());
    }

    #[test]
    fn version_mismatch_forces_full_scan() {
        let files = vec![file("a.rs", "code a")];
        let mut stale = ScanManifest {
            version: 0, // pretend a pre-upgrade manifest
            ..Default::default()
        };
        stale
            .files
            .entry("me/api".into())
            .or_default()
            .insert("a.rs".into(), content_fingerprint("code a"));
        let p = partition(Some(&stale), "me/api", &files);
        assert_eq!(
            p.changed.len(),
            1,
            "a stale-version manifest is ignored → full scan"
        );
        assert_eq!(p.unchanged_count, 0);
    }

    #[test]
    fn manifest_builder_records_all_files_and_repo_scoped_findings() {
        let files = vec![file("a.rs", "x"), file("b.rs", "y")];
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &files,
            &[
                finding("me/api", "a.rs", "R1"),
                finding("me/web", "z.rs", "OTHER"), // wrong repo — must be filtered out
            ],
        );
        let m = b.finish();
        assert!(m.is_current());
        assert_eq!(
            m.files["me/api"].len(),
            2,
            "every current file is fingerprinted"
        );
        assert_eq!(
            m.findings.len(),
            1,
            "only this repo's findings are recorded"
        );
        assert_eq!(m.findings[0].rule_id, "R1");
    }

    #[test]
    fn store_round_trips_a_manifest() {
        let store = ScanCacheStore::new();
        assert!(store.get("proj-1").is_none());
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &[file("a.rs", "x")],
            &[finding("me/api", "a.rs", "R1")],
        );
        store.put("proj-1", b.finish());

        let got = store.get("proj-1").expect("manifest persisted");
        assert_eq!(got.files["me/api"]["a.rs"], content_fingerprint("x"));
        assert_eq!(got.findings.len(), 1);

        store.clear("proj-1");
        assert!(store.get("proj-1").is_none(), "clear drops the manifest");
    }
}
