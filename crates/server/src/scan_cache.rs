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
///
/// Version 2: findings are redacted at construction (docs/design/2026-10-23_secret-redaction-
/// and-release-gate.md). A version 1 manifest holds findings whose snippets were stored BEFORE
/// that, so a secret value sits in it on disk; [`ScanCacheStore::load_or_new`] drops every
/// non-current manifest and rewrites the file at once, so the raw content is gone on first
/// start rather than on the next scan.
///
/// Version 3: carried AI findings are keyed by the question they answered ([`AiScanKey`]: the
/// model, the calibration model, the prompt-schema version and the rule directives), not only
/// by file fingerprint and rule ids. A version 2 manifest has no such key, so it cannot be shown
/// to still answer the current question and is dropped, which costs one full scan after the
/// upgrade.
pub(crate) const MANIFEST_VERSION: u32 = 3;

/// The question a set of AI findings answered: everything besides the file content and the rule
/// ids that decides what the model would say today. A carried-forward finding is reusable only
/// when today's key equals the key it was produced under; otherwise the file is re-scanned.
///
/// Before this key the cache matched on the file fingerprint and the rule ids alone, so
/// switching the model, editing the audit prompt, or choosing a different option (a different
/// directive) for the same rule id all silently served the previous answer.
///
/// Lives on the manifest, never on a client export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiScanKey {
    /// The RESOLVED audit model (see [`crate::ai_audit::resolve_audit_model`]).
    pub audit_model: String,
    /// The calibration model pick; empty when the audit model calibrates. Carried findings keep
    /// the severity and confidence calibration gave them.
    pub calibration_model: String,
    /// [`crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION`] when the findings were produced. `0` only
    /// for a manifest that predates the key, which never matches a real one.
    pub prompt_schema_version: u32,
    /// Order-independent digest of every selected rule's id AND directive text, which is what the
    /// audit prompt actually asks.
    pub directives_fingerprint: String,
    /// Whether the findings were produced under the `terse_output` measure (a one-sentence scan
    /// `detail`, the full one regenerated afterwards). It changes what the model is asked and so
    /// what it reports, which is not a pure saving like the scheduling measures, so a baseline
    /// from the other setting is not reused: flipping the switch to find a regression must not
    /// be answered from the other setting's cache. Absent in a key stored before the measure
    /// existed, which reads as `false`: those findings were produced the old way.
    #[serde(default)]
    pub terse_output: bool,
    /// Whether the findings were produced with `applies_to` routing on (a rule that declares
    /// where it applies is sent only with matching files). It changes which (chunk, rule) pairs
    /// the model is asked about, so one setting's cache must not answer the other's. Absent in a
    /// key stored before rank 7, which reads as `false`: those findings were produced without it.
    #[serde(default)]
    pub applies_to_routing: bool,
}

impl AiScanKey {
    /// The key a scan running now would produce. `audit_model` must already be resolved.
    pub fn new(
        audit_model: &str,
        calibration_model: Option<&str>,
        rules: &[crate::onboard::SelectedRule],
    ) -> Self {
        let mut parts: Vec<String> = rules
            .iter()
            .map(|r| format!("{}\u{1f}{}", r.id, r.directive))
            .collect();
        parts.sort_unstable();
        Self {
            audit_model: audit_model.to_string(),
            calibration_model: calibration_model.unwrap_or_default().to_string(),
            prompt_schema_version: crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
            directives_fingerprint: format!(
                "{:016x}",
                crate::suppression::fnv1a(&parts.join("\u{1e}"))
            ),
            terse_output: false,
            applies_to_routing: false,
        }
    }

    /// The key of a scan running with the `terse_output` measure `on` or off.
    pub fn with_terse_output(mut self, on: bool) -> Self {
        self.terse_output = on;
        self
    }

    /// The key of a scan running with `applies_to` routing `on` or off.
    pub fn with_applies_to_routing(mut self, on: bool) -> Self {
        self.applies_to_routing = on;
        self
    }
}

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
    /// Whether the AI (semantic) review actually ran and completed on THIS manifest's scan —
    /// i.e. `run_ai_review` was on AND no fatal AI error occurred. An AI-off run (the toggle was
    /// off), a compliance-`Blocked` run, or a run where the AI pass errored out still stamps a
    /// manifest (file fingerprints are always worth caching), but with `ai_covered = false` — its
    /// `findings` are empty/stale-shaped AI-wise and must never be treated as a real "AI found
    /// nothing" baseline.
    ///
    /// `#[serde(default)]` makes the zero value `false`, which is the SAFE default and doubles as
    /// a self-heal for every manifest written before this field existed: those legacy JSON blobs
    /// have no `ai_covered` key at all, so they deserialize with `ai_covered = false` and are
    /// automatically treated as "no AI baseline" by the next AI-on scan (see `matches_rules`'s
    /// caller in `onboard::audit_repos`, which additionally requires `ai_covered` before reusing
    /// a prior manifest) — no migration or manual cache-bust needed.
    #[serde(default)]
    pub ai_covered: bool,
    /// The question [`Self::findings`] answered (see [`AiScanKey`]). Defaults to the empty key,
    /// which matches no real scan, so a manifest without it is simply not reused.
    #[serde(default)]
    pub ai_key: AiScanKey,
    /// The clone-candidate judge's verdicts (Wave C, `onboard::clone_advisory`), keyed by the
    /// CONTENT of the question so an unchanged family on a re-scan is never re-billed. Persisted
    /// beside the file fingerprints in the app data directory, never in the client's repository.
    /// `serde(default)` and additive: a manifest written before this field existed loads with an
    /// empty cache and `MANIFEST_VERSION` is not bumped (the verdicts are an optimisation, never
    /// evidence, so a missing cache only costs one scan's judge calls).
    #[serde(default)]
    pub clone_verdicts: crate::onboard::clone_advisory::VerdictCache,
    /// H11: `repo` -> the commit that scan audited (`HEAD` of the repo), for a repo with a
    /// source identity. An incremental run reads it to say which inspection its carried findings
    /// came from. `serde(default)` and additive, like `clone_verdicts`: a manifest written before
    /// this existed carries no commits, and the carried rows then say "an earlier inspection".
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

/// Fingerprint the selected rule set (ids + their repo bindings) so a change to the selection
/// invalidates the incremental cache. Order-independent: ids and bindings are sorted first.
pub fn rules_fingerprint<'a>(rules: impl Iterator<Item = (&'a str, &'a [String])>) -> String {
    rules_fingerprint_with_content(rules.map(|(id, repos)| (id, repos, None)))
}

/// [`rules_fingerprint`] with each rule's corpus content hash (`Rule::content_hash`) folded in,
/// so a change to a rule's own content (its scope, options, wording) changes the fingerprint and
/// forces a full rescan, not only a change to WHICH rules are selected. `None` for a rule the
/// corpus does not know (or no corpus), which fingerprints as before.
pub fn rules_fingerprint_with_content<'a>(
    rules: impl Iterator<Item = (&'a str, &'a [String], Option<String>)>,
) -> String {
    let mut parts: Vec<String> = rules
        .map(|(id, repos, content_hash)| {
            let mut r: Vec<&str> = repos.iter().map(String::as_str).collect();
            r.sort_unstable();
            match content_hash {
                Some(hash) => format!("{}@{}#{}", id, r.join(","), hash),
                None => format!("{}@{}", id, r.join(",")),
            }
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

    /// Whether this manifest's carried AI findings may be reused by a scan running under
    /// `rules_fp` and `ai_key`: current schema, same rule selection, the AI review actually
    /// completed, and the findings answered the same question (model, prompt, rule text). The one
    /// place the reuse decision is made.
    pub fn is_reusable_baseline(&self, rules_fp: &str, ai_key: &AiScanKey) -> bool {
        self.is_current()
            && self.matches_rules(rules_fp)
            && self.ai_covered
            && self.ai_key == *ai_key
    }

    /// The fingerprint recorded for a file on the last scan, if any.
    fn fingerprint_of(&self, repo: &str, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(repo))
            .and_then(|(_, m)| m.get(path))
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
        .filter(|f| f.repo.eq_ignore_ascii_case(repo) && unchanged_paths.contains(&f.path))
        .cloned()
        .collect();

    Partition {
        changed,
        carried,
        unchanged_count: unchanged_paths.len(),
    }
}

/// What a carried finding says when its source commit is unknown (a manifest saved before commits
/// were recorded, or a repo with no source identity).
pub const CARRIED_FROM_UNKNOWN: &str = "an earlier inspection";

/// H11: mark each of `carried` with the commit it was last found at (`captures["carried_from"]`,
/// read by `report_export::carried_from`). A finding that already carries the mark was carried on
/// from an earlier incremental run and keeps its commit: the commit where its file was last
/// actually audited, not the commit of the run that merely passed it along.
pub fn mark_carried(carried: &mut [Finding], prior_commit: Option<&str>) {
    let source = prior_commit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(CARRIED_FROM_UNKNOWN);
    for f in carried {
        f.captures
            .entry(crate::report_export::CARRIED_FROM_CAPTURE.to_string())
            .or_insert_with(|| source.to_string());
    }
}

/// H11: sums what an incremental scan re-audited and carried forward across its repos, then
/// yields the one [`crate::scan_ledger::IncrementalCarry`] the ledger records.
#[derive(Debug, Default)]
pub struct CarryTally {
    changed: usize,
    carried_files: usize,
    carried_findings: usize,
    commits: Vec<String>,
}

impl CarryTally {
    /// Add one repo's partition. A repo that carried no file adds nothing: it was a full scan.
    pub fn add_repo(
        &mut self,
        changed: usize,
        carried_files: usize,
        carried_findings: usize,
        prior_commit: Option<String>,
    ) {
        if carried_files == 0 {
            return;
        }
        self.changed += changed;
        self.carried_files += carried_files;
        self.carried_findings += carried_findings;
        if let Some(sha) = prior_commit {
            if !self.commits.contains(&sha) {
                self.commits.push(sha);
            }
        }
    }

    /// `None` when no repo carried a file (a full scan). The source commit is named only when
    /// every carrying repo came from the same one; with several, the cover says "an earlier
    /// inspection" rather than naming one commit for rows that came from different repos.
    pub fn finish(self) -> Option<crate::scan_ledger::IncrementalCarry> {
        (self.carried_files > 0).then(|| crate::scan_ledger::IncrementalCarry {
            changed: self.changed,
            carried_files: self.carried_files,
            carried_findings: self.carried_findings,
            prior_sha: match self.commits.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            },
        })
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
    ai_covered: bool,
    ai_key: AiScanKey,
    clone_verdicts: crate::onboard::clone_advisory::VerdictCache,
    commits: BTreeMap<String, String>,
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

    /// Stamp whether the AI review actually ran and completed on this scan (see
    /// [`ScanManifest::ai_covered`]'s doc comment for exactly what that means). Defaults to
    /// `false` (the safe value) when never called, so a caller that forgets this stamp produces a
    /// manifest that is correctly treated as "no AI baseline" rather than silently trusted.
    pub fn with_ai_covered(mut self, covered: bool) -> Self {
        self.ai_covered = covered;
        self
    }

    /// Stamp the question this scan's AI findings answered (see [`AiScanKey`]). Defaults to the
    /// empty key when never called, which no real scan matches, so a caller that forgets this
    /// stamp produces a manifest that is never reused rather than one that is wrongly trusted.
    pub fn with_ai_key(mut self, key: AiScanKey) -> Self {
        self.ai_key = key;
        self
    }

    /// Carry the clone judge's verdict cache (the prior manifest's verdicts plus this scan's) into
    /// the manifest being built, so the next scan can reuse them.
    pub fn with_clone_verdicts(
        mut self,
        verdicts: crate::onboard::clone_advisory::VerdictCache,
    ) -> Self {
        self.clone_verdicts = verdicts;
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
        self.findings.extend(
            ai_findings
                .iter()
                .filter(|f| f.repo.eq_ignore_ascii_case(repo))
                .cloned(),
        );
    }

    /// Record the commit `repo` was audited at (see [`ScanManifest::commits`]). Skipped for a
    /// blank sha, so a repo without a source identity simply has none.
    pub fn record_commit(&mut self, repo: &str, sha: &str) {
        let sha = sha.trim();
        if !sha.is_empty() {
            self.commits.insert(repo.to_string(), sha.to_string());
        }
    }

    /// Finalize into a persistable manifest stamped with the current schema version.
    pub fn finish(self) -> ScanManifest {
        ScanManifest {
            version: MANIFEST_VERSION,
            rules_fingerprint: self.rules_fingerprint,
            files: self.files,
            findings: self.findings,
            ai_covered: self.ai_covered,
            ai_key: self.ai_key,
            clone_verdicts: self.clone_verdicts,
            commits: self.commits,
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

/// What [`ScanCacheStore::remove_repo`] took out of the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemovedFromCache {
    /// The cache held something for the repo.
    pub had_entry: bool,
    /// Fingerprinted files dropped.
    pub files: u64,
    /// Serialized size of what was dropped (fingerprints and findings).
    pub bytes: u64,
}

/// How much of `manifest` belongs to `repo` (case-insensitive): fingerprinted files plus cached
/// findings, sized by their serialized form.
fn repo_slice_stats(manifest: &ScanManifest, repo: &str) -> RemovedFromCache {
    let mut stats = RemovedFromCache::default();
    for (key, files) in &manifest.files {
        if key.eq_ignore_ascii_case(repo) {
            stats.had_entry = true;
            stats.files += files.len() as u64;
            stats.bytes += serde_json::to_vec(files).map_or(0, |b| b.len() as u64);
        }
    }
    let findings: Vec<&Finding> = manifest
        .findings
        .iter()
        .filter(|f| f.repo.eq_ignore_ascii_case(repo))
        .collect();
    if !findings.is_empty() {
        stats.had_entry = true;
        stats.bytes += serde_json::to_vec(&findings).map_or(0, |b| b.len() as u64);
    }
    stats
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
        let mut state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<CacheState>(&s).ok())
            .unwrap_or_default();
        // Drop every manifest from an older schema (see MANIFEST_VERSION) and, when any was
        // dropped, rewrite the file now: serving nothing from it is not enough, the raw
        // snippets it holds must not stay on disk until some later scan happens to overwrite it.
        let before = state.by_project.len();
        state.by_project.retain(|_, manifest| manifest.is_current());
        let purged = state.by_project.len() != before;
        let store = Self {
            inner: std::sync::Arc::new(Mutex::new(state)),
            path: Some(std::sync::Arc::new(path)),
        };
        if purged {
            if let Ok(state) = store.inner.lock() {
                store.save_locked(&state);
            }
        }
        store
    }

    /// The stored manifest for a project, if any (and only if it's the current schema version).
    pub fn get(&self, project_id: &str) -> Option<ScanManifest> {
        let s = self.inner.lock().ok()?;
        s.by_project
            .get(project_id)
            .filter(|m| m.is_current())
            .cloned()
    }

    /// Replace a project's manifest and persist atomically.
    ///
    /// The mutex is held for the ENTIRE operation — insert + write — so no concurrent
    /// `put` can interleave between the in-memory update and the file write (BUG-11).
    pub fn put(&self, project_id: &str, manifest: ScanManifest) {
        if let Ok(mut s) = self.inner.lock() {
            s.by_project.insert(project_id.to_string(), manifest);
            // Save while the lock is still held to prevent TOCTOU / lost-update races.
            self.save_locked(&s);
        }
    }

    /// Drop a project's manifest (e.g. on a forced full scan the next run rebuilds it anyway,
    /// but an explicit clear is available) and persist atomically (BUG-11: lock held through
    /// save, same as `put`).
    pub fn clear(&self, project_id: &str) {
        if let Ok(mut s) = self.inner.lock() {
            s.by_project.remove(project_id);
            self.save_locked(&s);
        }
    }

    /// Remove everything this cache holds about ONE repo of a project: its file fingerprints and
    /// every cached finding (snippets included) that belongs to it. The project's manifest is
    /// dropped entirely when no repo is left in it, so the file does not keep an empty shell.
    /// Persists atomically with the lock held (same discipline as [`Self::put`]).
    ///
    /// Source deletion calls this: the manifest is a copy of client code (excerpts) that must go
    /// with the checkout, while the rest of the project's entries must survive.
    ///
    /// Returns what was removed (`had_entry == false` when the cache held nothing for the repo).
    pub fn remove_repo(&self, project_id: &str, repo: &str) -> RemovedFromCache {
        let Ok(mut s) = self.inner.lock() else {
            return RemovedFromCache::default();
        };
        let Some(manifest) = s.by_project.get_mut(project_id) else {
            return RemovedFromCache::default();
        };
        let removed = repo_slice_stats(manifest, repo);
        manifest.files.retain(|k, _| !k.eq_ignore_ascii_case(repo));
        manifest
            .commits
            .retain(|k, _| !k.eq_ignore_ascii_case(repo));
        manifest
            .findings
            .retain(|f| !f.repo.eq_ignore_ascii_case(repo));
        if manifest.files.is_empty() {
            s.by_project.remove(project_id);
        }
        if removed.had_entry {
            self.save_locked(&s);
        }
        removed
    }

    /// What [`Self::remove_repo`] WOULD remove, without removing it (the deletion preview).
    pub fn peek_repo(&self, project_id: &str, repo: &str) -> RemovedFromCache {
        self.inner
            .lock()
            .ok()
            .and_then(|s| {
                s.by_project
                    .get(project_id)
                    .map(|m| repo_slice_stats(m, repo))
            })
            .unwrap_or_default()
    }

    /// Write `state` to disk while the caller already holds the mutex guard.
    ///
    /// Separated from `save` so `put` / `clear` can call it while they own the lock,
    /// avoiding the TOCTOU window that arose when the lock was released before the write
    /// (BUG-11: concurrent `put` could write a stale snapshot if the first thread's write
    /// completed after the second thread's combined write).
    fn save_locked(&self, state: &CacheState) {
        let Some(path) = &self.path else {
            return;
        };
        if let Ok(json) = serde_json::to_string_pretty(state) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path.as_path(), json);
        }
    }

    /// Flush in-memory state to disk by re-acquiring the lock.
    ///
    /// This is intentionally dead-code in production (all mutations go through
    /// `put`/`clear` which call `save_locked`). Retained so tests can call it
    /// directly if needed.
    #[allow(dead_code)]
    fn save(&self) {
        if let Ok(state) = self.inner.lock() {
            self.save_locked(&state);
        }
    }
}

pub mod state_dir;

#[cfg(test)]
mod ai_key_tests;
#[cfg(test)]
mod redaction_tests;
#[cfg(test)]
mod remove_repo_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> (String, String) {
        (path.to_string(), content.to_string())
    }

    fn finding(repo: &str, path: &str, rule: &str) -> Finding {
        Finding {
            could_be_intentional: false,
            repo: repo.to_string(),
            kind: None,
            path: path.to_string(),
            line: 1,
            rule_id: rule.to_string(),
            severity: "medium".to_string(),
            snippet: "x".to_string(),
            detail: "d".to_string(),
            status: "active".to_string(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
            confidence: None,
            effort: None,
            category: None,
            located: true,
            captures: Default::default(),
            evaluated_option_id: None,
            also_locations: Vec::new(),
            fix_specific: None,
            calibration_rationale: None,
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

    /// The same repo spelled with different case (a project's `Acme/App`, an origin's `acme/app`)
    /// is one repo: its unchanged files and their cached findings carry forward either way,
    /// exactly as `repo_delta` and the cache purge already treat it.
    #[test]
    fn partition_matches_the_repo_case_insensitively() {
        let files = vec![file("a.rs", "code a")];
        let mut b = ManifestBuilder::new();
        b.record_repo("Me/Api", &files, &[finding("Me/Api", "a.rs", "ARCH-1")]);
        let prior = b.finish();

        let p = partition(Some(&prior), "me/api", &files);

        assert_eq!(p.unchanged_count, 1, "the file is recognised as unchanged");
        assert_eq!(p.carried.len(), 1, "and its finding carries forward");
    }

    #[test]
    fn recording_a_repo_keeps_findings_spelled_in_another_case() {
        let files = vec![file("a.rs", "code a")];
        let mut b = ManifestBuilder::new();
        b.record_repo("me/api", &files, &[finding("Me/Api", "a.rs", "ARCH-1")]);
        assert_eq!(b.finish().findings.len(), 1);
    }

    #[test]
    fn rules_fingerprint_changes_when_a_rules_content_hash_changes() {
        let repos: &[String] = &[];
        let with = |hash: Option<&str>| {
            rules_fingerprint_with_content(
                [("ARCH-1", repos, hash.map(str::to_string))].into_iter(),
            )
        };
        assert_ne!(with(Some("sha256:a")), with(Some("sha256:b")));
        assert_ne!(with(Some("sha256:a")), with(None));
        assert_eq!(
            with(None),
            rules_fingerprint([("ARCH-1", repos)].into_iter()),
            "a rule with no known content fingerprints as before"
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

    // ── ai_covered: stamping + self-heal ────────────────────────────────────────

    #[test]
    fn manifest_builder_defaults_ai_covered_to_false() {
        // A caller that forgets to call `with_ai_covered` must get the SAFE default (false),
        // not an implicitly-trusted manifest.
        let mut b = ManifestBuilder::new();
        b.record_repo("me/api", &[file("a.rs", "x")], &[]);
        let m = b.finish();
        assert!(
            !m.ai_covered,
            "ai_covered must default to false when the builder never stamps it"
        );
    }

    #[test]
    fn manifest_builder_stamps_ai_covered_true_when_told() {
        let mut b = ManifestBuilder::new().with_ai_covered(true);
        b.record_repo(
            "me/api",
            &[file("a.rs", "x")],
            &[finding("me/api", "a.rs", "R1")],
        );
        let m = b.finish();
        assert!(m.ai_covered, "with_ai_covered(true) must stamp true");
    }

    #[test]
    fn manifest_builder_stamps_ai_covered_false_when_told() {
        // Simulates an AI-off / blocked / errored run: the manifest is still built (file
        // fingerprints recorded) but explicitly marked as having no AI baseline.
        let mut b = ManifestBuilder::new().with_ai_covered(false);
        b.record_repo("me/api", &[file("a.rs", "x")], &[]);
        let m = b.finish();
        assert!(
            !m.ai_covered,
            "an AI-off run's manifest must be stamped ai_covered = false"
        );
        assert_eq!(
            m.files["me/api"].len(),
            1,
            "file fingerprints are still recorded even when the AI didn't run"
        );
    }

    #[test]
    fn ai_covered_round_trips_through_json_serde() {
        let mut b = ManifestBuilder::new().with_ai_covered(true);
        b.record_repo(
            "me/api",
            &[file("a.rs", "x")],
            &[finding("me/api", "a.rs", "R1")],
        );
        let m = b.finish();
        let json = serde_json::to_string(&m).expect("serialize");
        let back: ScanManifest = serde_json::from_str(&json).expect("deserialize");
        assert!(
            back.ai_covered,
            "ai_covered = true must round-trip through JSON"
        );
    }

    #[test]
    fn legacy_manifest_json_without_ai_covered_self_heals_to_false() {
        // A manifest persisted BEFORE the `ai_covered` field existed has no such key in its
        // JSON at all. `#[serde(default)]` must deserialize that as `false` — the safe
        // value — so every already-written (potentially AI-poisoned) manifest on disk
        // self-heals the moment it's loaded, with no migration step required.
        let legacy_json = r#"{
            "version": 1,
            "rules_fingerprint": "abc123",
            "files": {"me/api": {"a.rs": "deadbeefdeadbeef"}},
            "findings": []
        }"#;
        let m: ScanManifest = serde_json::from_str(legacy_json)
            .expect("legacy manifest JSON (no ai_covered key) must still deserialize");
        assert!(
            !m.ai_covered,
            "a legacy manifest with no ai_covered key must self-heal to false"
        );
        assert_eq!(
            m.rules_fingerprint, "abc123",
            "other fields still parse normally"
        );
    }

    #[test]
    fn legacy_manifest_without_ai_covered_is_ignored_by_ai_on_gate() {
        // End-to-end proof of the self-heal: a legacy manifest (no ai_covered key, so it
        // deserializes false) must fail the caller's `ai_covered` gate exactly like a
        // freshly-stamped ai_covered=false manifest would — full AI audit, not a reused
        // empty result.
        let legacy_json = r#"{
            "version": 1,
            "rules_fingerprint": "RULES-V1",
            "files": {"me/api": {"a.rs": "deadbeefdeadbeef"}},
            "findings": []
        }"#;
        let legacy: ScanManifest = serde_json::from_str(legacy_json).expect("deserialize");

        // Mirrors onboard::audit_repos's `effective_prior` gate.
        let effective_prior = Some(&legacy).filter(|m| m.matches_rules("RULES-V1") && m.ai_covered);
        assert!(
            effective_prior.is_none(),
            "a legacy (ai_covered=false) manifest must be treated as no AI baseline"
        );

        let files = vec![file("a.rs", "unchanged content")];
        let p = partition(effective_prior, "me/api", &files);
        assert_eq!(
            p.changed.len(),
            1,
            "with no usable AI baseline, the file is treated as changed → re-audited"
        );
        assert_eq!(p.unchanged_count, 0);
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

    #[test]
    fn partition_with_mixed_unchanged_and_changed_in_same_repo() {
        // Test edge case: some files unchanged, some changed in the same partition.
        // This ensures the partition correctly splits the set rather than treating
        // the whole repo as changed if ANY file changed.
        let files = vec![
            file("a.rs", "code a"),
            file("b.rs", "code b"),
            file("c.rs", "code c"),
        ];
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &files,
            &[
                finding("me/api", "a.rs", "ARCH-1"),
                finding("me/api", "b.rs", "ARCH-2"),
                finding("me/api", "c.rs", "ARCH-3"),
            ],
        );
        let prior = b.finish();

        // b.rs changes; a.rs and c.rs are identical.
        let next = vec![
            file("a.rs", "code a"),
            file("b.rs", "code b CHANGED"),
            file("c.rs", "code c"),
        ];
        let p = partition(Some(&prior), "me/api", &next);

        assert_eq!(p.changed.len(), 1, "only b.rs is re-audited");
        assert_eq!(p.changed[0].0, "b.rs");
        assert_eq!(p.unchanged_count, 2, "a.rs and c.rs are unchanged");
        assert_eq!(p.carried.len(), 2, "two findings carried forward");
        let carried_paths: std::collections::HashSet<String> =
            p.carried.iter().map(|f| f.path.clone()).collect();
        assert!(carried_paths.contains("a.rs"));
        assert!(carried_paths.contains("c.rs"));
        assert!(!carried_paths.contains("b.rs"));
    }

    #[test]
    fn partition_with_empty_file_list() {
        // Edge case: current working tree has no files (e.g. after deletion).
        let prior_files = vec![file("a.rs", "x"), file("b.rs", "y")];
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &prior_files,
            &[
                finding("me/api", "a.rs", "R1"),
                finding("me/api", "b.rs", "R2"),
            ],
        );
        let prior = b.finish();

        // No files remain.
        let p = partition(Some(&prior), "me/api", &[]);

        assert_eq!(p.changed.len(), 0);
        assert_eq!(p.unchanged_count, 0);
        assert!(p.carried.is_empty(), "no files, no carried findings");
        assert_eq!(p.total(), 0);
    }

    #[test]
    fn rules_fingerprint_handles_multiple_repos_per_rule() {
        // Edge case: a single rule bound to multiple repos. Fingerprint must be stable
        // regardless of input order and repo order within the binding.
        let a = rules_fingerprint(
            [("ARCH-1", &["me/api".to_string(), "me/web".to_string()][..])].into_iter(),
        );
        // Reverse repo order in the binding.
        let b = rules_fingerprint(
            [("ARCH-1", &["me/web".to_string(), "me/api".to_string()][..])].into_iter(),
        );
        assert_eq!(a, b, "fingerprint must be stable regardless of repo order");
    }

    #[test]
    fn manifest_builder_round_trip_with_multiple_repos() {
        // Edge case: manifest records multiple repos in a single scan. Each repo
        // has its own file set and findings; ensure they're all preserved.
        let files_api = vec![file("a.rs", "x"), file("b.rs", "y")];
        let files_web = vec![file("c.ts", "z"), file("d.tsx", "w")];

        let mut b = ManifestBuilder::new().with_rules_fingerprint("RULES-V1".to_string());
        b.record_repo(
            "me/api",
            &files_api,
            &[
                finding("me/api", "a.rs", "RUST-1"),
                finding("me/api", "b.rs", "RUST-2"),
            ],
        );
        b.record_repo(
            "me/web",
            &files_web,
            &[
                finding("me/web", "c.ts", "REACT-1"),
                finding("me/web", "d.tsx", "REACT-2"),
            ],
        );
        let m = b.finish();

        // Verify both repos are recorded.
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files["me/api"].len(), 2);
        assert_eq!(m.files["me/web"].len(), 2);
        // Verify findings are separated by repo.
        assert_eq!(m.findings.iter().filter(|f| f.repo == "me/api").count(), 2);
        assert_eq!(m.findings.iter().filter(|f| f.repo == "me/web").count(), 2);
    }

    #[test]
    fn partition_rejects_stale_version_manifest() {
        // Edge case: version mismatch (e.g. upgrade happened). Should treat as no cache.
        let files = vec![file("a.rs", "unchanged")];
        let stale = ScanManifest {
            version: 99, // bogus future version
            rules_fingerprint: "".to_string(),
            files: {
                let mut m = BTreeMap::new();
                m.insert("me/api".to_string(), {
                    let mut inner = BTreeMap::new();
                    inner.insert("a.rs".to_string(), content_fingerprint("unchanged"));
                    inner
                });
                m
            },
            findings: vec![],
            ai_covered: true,
            ai_key: AiScanKey::default(),
            clone_verdicts: Default::default(),
            commits: Default::default(),
        };

        let p = partition(Some(&stale), "me/api", &files);
        assert_eq!(p.changed.len(), 1, "stale version forces full scan");
        assert_eq!(p.unchanged_count, 0);
        assert!(p.carried.is_empty());
    }

    // ── BUG-11 regression: ScanCacheStore::put atomicity ────────────────────────

    /// Before BUG-11 fix, `put` released the mutex BEFORE calling `save`, so a concurrent
    /// thread could insert a second manifest, save the combined state, and then the first
    /// thread's `save` would overwrite with just the first manifest.
    ///
    /// The regression test verifies that PUT then GET round-trips correctly within the same
    /// thread (the race is not directly observable without real concurrency, but the structural
    /// fix — save_locked called while the guard is held — is what prevents it). We also verify
    /// that `save_locked` is reachable (no dead-code panic) and that `clear` is similarly atomic.
    #[test]
    fn bug11_put_is_atomic_in_memory() {
        let store = ScanCacheStore::new();

        // Two sequential puts must both be visible and not overwrite each other.
        let mut b1 = ManifestBuilder::new();
        b1.record_repo(
            "me/api",
            &[file("a.rs", "x")],
            &[finding("me/api", "a.rs", "R1")],
        );
        store.put("proj-alpha", b1.finish());

        let mut b2 = ManifestBuilder::new();
        b2.record_repo(
            "me/web",
            &[file("b.ts", "y")],
            &[finding("me/web", "b.ts", "R2")],
        );
        store.put("proj-beta", b2.finish());

        // Both projects must be present after the two puts.
        let alpha = store
            .get("proj-alpha")
            .expect("proj-alpha must be present after put");
        assert!(
            alpha.files.contains_key("me/api"),
            "proj-alpha must have me/api fingerprints"
        );
        assert_eq!(alpha.findings.len(), 1);
        assert_eq!(alpha.findings[0].rule_id, "R1");

        let beta = store
            .get("proj-beta")
            .expect("proj-beta must be present after put");
        assert!(
            beta.files.contains_key("me/web"),
            "proj-beta must have me/web fingerprints"
        );
        assert_eq!(beta.findings.len(), 1);
        assert_eq!(beta.findings[0].rule_id, "R2");

        // After clear, the cleared project is gone but the other survives.
        store.clear("proj-alpha");
        assert!(
            store.get("proj-alpha").is_none(),
            "proj-alpha must be absent after clear"
        );
        assert!(
            store.get("proj-beta").is_some(),
            "proj-beta must survive clear of proj-alpha"
        );
    }

    /// Verify that `put` after `clear` restores the manifest without any stale state.
    #[test]
    fn bug11_put_after_clear_restores_cleanly() {
        let store = ScanCacheStore::new();
        let mut b = ManifestBuilder::new();
        b.record_repo(
            "me/api",
            &[file("a.rs", "x")],
            &[finding("me/api", "a.rs", "R1")],
        );
        store.put("proj-x", b.finish());
        store.clear("proj-x");
        assert!(
            store.get("proj-x").is_none(),
            "clear must remove the manifest"
        );

        // Re-put must work correctly after a clear.
        let mut b2 = ManifestBuilder::new();
        b2.record_repo(
            "me/api",
            &[file("a.rs", "x2")],
            &[finding("me/api", "a.rs", "R2")],
        );
        store.put("proj-x", b2.finish());
        let got = store.get("proj-x").expect("re-put must restore manifest");
        assert_eq!(
            got.findings[0].rule_id, "R2",
            "re-put must store the new manifest, not the cleared one"
        );
    }

    // ── H11: carried findings and the incremental record ─────────────────────────────────

    #[test]
    fn mark_carried_names_the_commit_and_keeps_an_earlier_mark() {
        let mut fresh = Finding::default();
        let mut older = Finding::default();
        older
            .captures
            .insert("carried_from".to_string(), "aaa1111".to_string());
        let mut both = vec![fresh.clone(), older];
        mark_carried(&mut both, Some("bbb2222"));
        assert_eq!(both[0].captures["carried_from"], "bbb2222");
        assert_eq!(both[1].captures["carried_from"], "aaa1111");
        let mut unknown = vec![std::mem::take(&mut fresh)];
        mark_carried(&mut unknown, None);
        assert_eq!(unknown[0].captures["carried_from"], CARRIED_FROM_UNKNOWN);
    }

    #[test]
    fn the_carry_tally_sums_repos_and_names_one_commit_only_when_it_is_one() {
        let mut one = CarryTally::default();
        one.add_repo(2, 8, 3, Some("abc".to_string()));
        one.add_repo(5, 0, 0, Some("zzz".to_string())); // a full scan: adds nothing
        let carry = one.finish().expect("a repo carried files");
        assert_eq!(
            (carry.changed, carry.carried_files, carry.carried_findings),
            (2, 8, 3)
        );
        assert_eq!(carry.prior_sha.as_deref(), Some("abc"));

        let mut two = CarryTally::default();
        two.add_repo(1, 4, 1, Some("abc".to_string()));
        two.add_repo(1, 4, 1, Some("def".to_string()));
        assert_eq!(two.finish().unwrap().prior_sha, None);

        assert!(CarryTally::default().finish().is_none(), "a full scan");
    }

    #[test]
    fn a_manifest_records_commits_and_a_repo_removal_drops_its_commit() {
        let mut b = ManifestBuilder::new();
        b.record_commit("me/api", " abc123 ");
        b.record_commit("me/web", "   ");
        let m = b.finish();
        assert_eq!(m.commits.get("me/api").map(String::as_str), Some("abc123"));
        assert!(!m.commits.contains_key("me/web"));
        // A manifest saved before commits existed still loads.
        let old: ScanManifest = serde_json::from_str(r#"{"version":3}"#).unwrap();
        assert!(old.commits.is_empty());
    }
}
