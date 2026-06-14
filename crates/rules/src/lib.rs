//! camerata-rules: rule corpus loader, enforcement-kind classifier, and
//! per-task rule-subset selection.
//!
//! # Responsibilities
//!
//! 1. Recursively load TOML rule files from a corpus directory
//!    (`/path/to/camerata-ai/principles` by default).
//! 2. Parse each file into a [`Rule`] with the fields that the orchestrator
//!    cares about: `id`, `title`, `enforcement`, `domain`, `summary`.
//! 3. Index all loaded rules into a [`RuleSet`] — queryable by id or domain.
//! 4. Expose a pure [`select`] function: given a filter, return a
//!    `Vec<Rule>` — the per-task rule-subset.
//!
//! All I/O is `async`; pure selection helpers are synchronous.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use camerata_core::RuleId;
use serde::Deserialize;
use thiserror::Error;

// ────────────────────────────────────────────────────────────────────────────
// Error type (RUST-DOMAIN-4 / RUST-DOMAIN-6)
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum RulesError {
    #[error("I/O error reading corpus at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("TOML parse error in {path}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("rule file {path} is missing a required field: {field}")]
    MissingField { path: PathBuf, field: &'static str },
}

// ────────────────────────────────────────────────────────────────────────────
// Enforcement kind
// ────────────────────────────────────────────────────────────────────────────

/// The three emission tiers for a camerata rule (from CAMERATA-ANATOMY-1).
///
/// - `Prose`      — human-readable rationale only; no generated artifact.
/// - `Structured` — emits a structured section (e.g. a CONVENTIONS.md entry).
/// - `Mechanical` — emits a runnable check (linter, CI gate, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnforcementKind {
    Prose,
    Structured,
    Mechanical,
}

impl std::fmt::Display for EnforcementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnforcementKind::Prose => f.write_str("prose"),
            EnforcementKind::Structured => f.write_str("structured"),
            EnforcementKind::Mechanical => f.write_str("mechanical"),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Raw TOML shape (private; only the fields we need)
// ────────────────────────────────────────────────────────────────────────────

/// The raw deserialization target for a principle TOML file.
///
/// Optional fields that the orchestrator does not use are intentionally
/// omitted; `serde(deny_unknown_fields)` is NOT set so future corpus fields
/// are silently ignored rather than breaking the loader.
#[derive(Debug, Deserialize)]
struct RuleToml {
    id: String,
    title: String,
    enforcement: EnforcementKind,
    domain: String,
    /// Optional short summary. We derive it from the `decision.why` field when
    /// `qualifies` is absent, or fall back to the title.
    #[serde(default)]
    qualifies: Option<String>,
    #[serde(default)]
    decision: Option<DecisionToml>,
}

#[derive(Debug, Deserialize)]
struct DecisionToml {
    #[serde(default)]
    why: Option<String>,
}

// ────────────────────────────────────────────────────────────────────────────
// Public domain types
// ────────────────────────────────────────────────────────────────────────────

/// A single camerata principle loaded from the corpus.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Stable, traceable rule id — mapped from [`camerata_core::RuleId`].
    pub id: RuleId,
    /// Short human-readable title.
    pub title: String,
    /// Enforcement tier for this rule.
    pub enforcement: EnforcementKind,
    /// Domain tag from the TOML file (e.g. `"rust"`, `"agentic"`, `"*"`).
    pub domain: String,
    /// A one-paragraph summary — sourced from `qualifies`, then
    /// `decision.why`, then `title` as a final fallback.
    pub summary: String,
}

impl Rule {
    /// Convenience: the string form of the rule id.
    pub fn id_str(&self) -> &str {
        &self.id.0
    }
}

// ────────────────────────────────────────────────────────────────────────────
// RuleSet
// ────────────────────────────────────────────────────────────────────────────

/// All loaded rules, indexed for fast lookup by id and by domain.
#[derive(Debug, Default)]
pub struct RuleSet {
    /// Ordered list preserving load order (stable iteration).
    rules: Vec<Rule>,
    /// Fast lookup: rule id string → index into `rules`.
    by_id: HashMap<String, usize>,
    /// Fast lookup: domain string → list of indices into `rules`.
    by_domain: HashMap<String, Vec<usize>>,
}

impl RuleSet {
    /// Number of rules in this set.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Look up a rule by its id string (e.g. `"RUST-DOMAIN-4"`).
    pub fn get_by_id(&self, id: &str) -> Option<&Rule> {
        self.by_id.get(id).map(|&i| &self.rules[i])
    }

    /// All rules whose `domain` field matches `domain` exactly.
    ///
    /// Note: the corpus uses `"*"` for universal rules. Pass `"*"` to retrieve
    /// them, or use [`RuleSet::select`] with [`Filter::AllDomains`] to include
    /// universals automatically.
    pub fn get_by_domain(&self, domain: &str) -> Vec<&Rule> {
        match self.by_domain.get(domain) {
            Some(indices) => indices.iter().map(|&i| &self.rules[i]).collect(),
            None => vec![],
        }
    }

    /// Iterate every rule in load order.
    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter()
    }

    /// All distinct domain strings present in the set (including `"*"`).
    pub fn domains(&self) -> impl Iterator<Item = &str> {
        self.by_domain.keys().map(String::as_str)
    }

    fn push(&mut self, rule: Rule) {
        let idx = self.rules.len();
        self.by_domain
            .entry(rule.domain.clone())
            .or_default()
            .push(idx);
        self.by_id.insert(rule.id.0.clone(), idx);
        self.rules.push(rule);
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Loader (async I/O — RUST-DOMAIN-5)
// ────────────────────────────────────────────────────────────────────────────

/// Default corpus path shipped with the camerata-ai repo.
pub const DEFAULT_CORPUS_PATH: &str =
    "/Users/zacharyernst/Documents/Repos/camerata-ai/principles";

/// Load the full rule corpus from `corpus_dir`, walking it recursively.
///
/// Files that fail to parse as valid TOML rule files are returned as errors;
/// the caller may choose to collect-and-continue or fail-fast.
///
/// Files whose names do not end in `.toml` are silently ignored.
pub async fn load_corpus(corpus_dir: &Path) -> Result<RuleSet, RulesError> {
    let paths = collect_toml_paths(corpus_dir).await?;
    let mut set = RuleSet::default();
    for path in paths {
        let rule = load_one(&path).await?;
        set.push(rule);
    }
    Ok(set)
}

/// Load the corpus and silently skip any file that fails to parse.
///
/// Returns the successfully loaded [`RuleSet`] and a list of (path, error)
/// pairs for files that were skipped. Useful when the corpus is evolving and
/// some files may temporarily be malformed.
pub async fn load_corpus_lenient(
    corpus_dir: &Path,
) -> (RuleSet, Vec<(PathBuf, RulesError)>) {
    let paths = match collect_toml_paths(corpus_dir).await {
        Ok(p) => p,
        Err(e) => return (RuleSet::default(), vec![(corpus_dir.to_path_buf(), e)]),
    };

    let mut set = RuleSet::default();
    let mut errors = Vec::new();

    for path in paths {
        match load_one(&path).await {
            Ok(rule) => set.push(rule),
            Err(e) => errors.push((path, e)),
        }
    }

    (set, errors)
}

/// Walk `corpus_dir` recursively and collect all `.toml` file paths.
async fn collect_toml_paths(corpus_dir: &Path) -> Result<Vec<PathBuf>, RulesError> {
    // Use a sync walkdir wrapped in spawn_blocking so we stay async-safe.
    let dir = corpus_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut paths = Vec::new();
        collect_toml_paths_sync(&dir, &mut paths)?;
        // Sort for deterministic load order across platforms.
        paths.sort();
        Ok(paths)
    })
    .await
    .unwrap_or_else(|join_err| {
        Err(RulesError::Io {
            path: corpus_dir.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::Other, join_err.to_string()),
        })
    })
}

fn collect_toml_paths_sync(
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), RulesError> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| RulesError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|e| RulesError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_toml_paths_sync(&path, out)?;
        } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse a single TOML file into a [`Rule`].
async fn load_one(path: &Path) -> Result<Rule, RulesError> {
    let bytes = tokio::fs::read(path).await.map_err(|e| RulesError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let text = String::from_utf8_lossy(&bytes).into_owned();
    let raw: RuleToml = toml::from_str(&text).map_err(|e| RulesError::TomlParse {
        path: path.to_path_buf(),
        source: e,
    })?;

    if raw.id.is_empty() {
        return Err(RulesError::MissingField {
            path: path.to_path_buf(),
            field: "id",
        });
    }

    // Derive summary: qualifies > decision.why > title.
    let summary = raw
        .qualifies
        .filter(|s| !s.is_empty())
        .or_else(|| {
            raw.decision
                .as_ref()
                .and_then(|d| d.why.as_deref())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
        })
        .unwrap_or_else(|| raw.title.clone());

    Ok(Rule {
        id: RuleId(raw.id),
        title: raw.title,
        enforcement: raw.enforcement,
        domain: raw.domain,
        summary,
    })
}

// ────────────────────────────────────────────────────────────────────────────
// Rule-subset selection (pure — RUST-PURE-STATE-TRANSITIONS-1)
// ────────────────────────────────────────────────────────────────────────────

/// Criteria for selecting a rule subset from a [`RuleSet`].
///
/// Filters compose with OR semantics: a rule matches if it satisfies
/// **any** active criterion. Use [`Filter::And`] for AND semantics.
#[derive(Debug, Clone)]
pub enum Filter<'a> {
    /// Match rules whose id is in this list.
    ByIds(&'a [RuleId]),
    /// Match rules whose `domain` field equals this value exactly.
    ByDomain(&'a str),
    /// Match rules belonging to any of these domains.
    ByDomains(&'a [&'a str]),
    /// Match rules with a specific enforcement kind.
    ByEnforcement(EnforcementKind),
    /// All rules in the set.
    All,
    /// OR of two sub-filters.
    Or(Box<Filter<'a>>, Box<Filter<'a>>),
    /// AND of two sub-filters.
    And(Box<Filter<'a>>, Box<Filter<'a>>),
}

/// Select a rule subset from `rule_set` according to `filter`.
///
/// Returns rules in corpus load order; does not deduplicate (if a rule
/// matches multiple branches of an `Or`, it appears once because we iterate
/// the full set once and test each rule).
///
/// This is a pure function: given the same `rule_set` + `filter`, it always
/// returns the same result.
pub fn select<'a>(rule_set: &'a RuleSet, filter: &Filter<'_>) -> Vec<&'a Rule> {
    rule_set.iter().filter(|r| matches_filter(r, filter)).collect()
}

/// Pure predicate — does `rule` satisfy `filter`?
fn matches_filter(rule: &Rule, filter: &Filter<'_>) -> bool {
    match filter {
        Filter::All => true,
        Filter::ByIds(ids) => ids.iter().any(|id| id == &rule.id),
        Filter::ByDomain(d) => rule.domain == *d,
        Filter::ByDomains(ds) => ds.iter().any(|d| rule.domain == *d),
        Filter::ByEnforcement(kind) => rule.enforcement == *kind,
        Filter::Or(a, b) => matches_filter(rule, a) || matches_filter(rule, b),
        Filter::And(a, b) => matches_filter(rule, a) && matches_filter(rule, b),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Convenience: select by owned ids (useful when callers hold Vec<RuleId>)
// ────────────────────────────────────────────────────────────────────────────

/// Select rules matching any of `ids` from `rule_set`.
///
/// Shorthand for `select(rule_set, &Filter::ByIds(ids))`.
pub fn select_by_ids<'a>(rule_set: &'a RuleSet, ids: &[RuleId]) -> Vec<&'a Rule> {
    select(rule_set, &Filter::ByIds(ids))
}

/// Select rules for the given domains (including universal `"*"` rules).
///
/// The `"*"` wildcard domain is always included so callers get universal
/// rules automatically without needing to mention `"*"` explicitly.
pub fn select_for_domains<'a>(rule_set: &'a RuleSet, domains: &[&str]) -> Vec<&'a Rule> {
    rule_set
        .iter()
        .filter(|r| {
            r.domain == "*" || domains.iter().any(|d| r.domain == *d)
        })
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────
// Tests (ORCH-NEW-PATH-TESTS-1)
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_rule(id: &str, domain: &str, enforcement: EnforcementKind) -> Rule {
        Rule {
            id: RuleId(id.to_owned()),
            title: format!("Rule {id}"),
            enforcement,
            domain: domain.to_owned(),
            summary: format!("Summary of {id}"),
        }
    }

    fn populated_set() -> RuleSet {
        let mut set = RuleSet::default();
        set.push(make_rule("RUST-DOMAIN-1", "rust", EnforcementKind::Structured));
        set.push(make_rule("RUST-DOMAIN-4", "rust", EnforcementKind::Structured));
        set.push(make_rule(
            "ORCH-NEW-PATH-TESTS-1",
            "agentic",
            EnforcementKind::Mechanical,
        ));
        set.push(make_rule("SPIRIT-OPTIMIZE-1", "*", EnforcementKind::Prose));
        set.push(make_rule(
            "ARCH-STRICT-LAYERING-1",
            "api-layer",
            EnforcementKind::Mechanical,
        ));
        set
    }

    // ── RuleSet indexing ─────────────────────────────────────────────────────

    #[test]
    fn ruleset_get_by_id_found() {
        let set = populated_set();
        let rule = set.get_by_id("RUST-DOMAIN-4").expect("should find rule");
        assert_eq!(rule.id_str(), "RUST-DOMAIN-4");
    }

    #[test]
    fn ruleset_get_by_id_missing() {
        let set = populated_set();
        assert!(set.get_by_id("DOES-NOT-EXIST").is_none());
    }

    #[test]
    fn ruleset_get_by_domain_returns_correct_subset() {
        let set = populated_set();
        let rust_rules = set.get_by_domain("rust");
        assert_eq!(rust_rules.len(), 2);
        assert!(rust_rules.iter().all(|r| r.domain == "rust"));
    }

    #[test]
    fn ruleset_get_by_domain_missing_returns_empty() {
        let set = populated_set();
        assert!(set.get_by_domain("nonexistent-domain").is_empty());
    }

    #[test]
    fn ruleset_len_reflects_all_rules() {
        let set = populated_set();
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn ruleset_domains_includes_all_expected() {
        let set = populated_set();
        let mut domains: Vec<&str> = set.domains().collect();
        domains.sort_unstable();
        assert!(domains.contains(&"rust"));
        assert!(domains.contains(&"agentic"));
        assert!(domains.contains(&"*"));
        assert!(domains.contains(&"api-layer"));
    }

    // ── Filter::All ──────────────────────────────────────────────────────────

    #[test]
    fn select_all_returns_full_set() {
        let set = populated_set();
        let result = select(&set, &Filter::All);
        assert_eq!(result.len(), set.len());
    }

    // ── Filter::ByIds ────────────────────────────────────────────────────────

    #[test]
    fn select_by_ids_exact_match() {
        let set = populated_set();
        let ids = vec![RuleId("RUST-DOMAIN-1".to_owned())];
        let result = select(&set, &Filter::ByIds(&ids));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id_str(), "RUST-DOMAIN-1");
    }

    #[test]
    fn select_by_ids_no_match_returns_empty() {
        let set = populated_set();
        let ids = vec![RuleId("NONEXISTENT".to_owned())];
        let result = select(&set, &Filter::ByIds(&ids));
        assert!(result.is_empty());
    }

    #[test]
    fn select_by_ids_multiple_ids() {
        let set = populated_set();
        let ids = vec![
            RuleId("RUST-DOMAIN-1".to_owned()),
            RuleId("ORCH-NEW-PATH-TESTS-1".to_owned()),
        ];
        let result = select(&set, &Filter::ByIds(&ids));
        assert_eq!(result.len(), 2);
    }

    // ── Filter::ByDomain ─────────────────────────────────────────────────────

    #[test]
    fn select_by_domain_returns_matching() {
        let set = populated_set();
        let result = select(&set, &Filter::ByDomain("agentic"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id_str(), "ORCH-NEW-PATH-TESTS-1");
    }

    // ── Filter::ByDomains ────────────────────────────────────────────────────

    #[test]
    fn select_by_domains_combines_multiple() {
        let set = populated_set();
        let result = select(&set, &Filter::ByDomains(&["rust", "agentic"]));
        assert_eq!(result.len(), 3);
    }

    // ── Filter::ByEnforcement ────────────────────────────────────────────────

    #[test]
    fn select_by_enforcement_mechanical() {
        let set = populated_set();
        let result = select(&set, &Filter::ByEnforcement(EnforcementKind::Mechanical));
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .all(|r| r.enforcement == EnforcementKind::Mechanical));
    }

    #[test]
    fn select_by_enforcement_prose() {
        let set = populated_set();
        let result = select(&set, &Filter::ByEnforcement(EnforcementKind::Prose));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id_str(), "SPIRIT-OPTIMIZE-1");
    }

    // ── Filter::Or ───────────────────────────────────────────────────────────

    #[test]
    fn select_or_combines_results_without_duplication() {
        let set = populated_set();
        let filter = Filter::Or(
            Box::new(Filter::ByDomain("rust")),
            Box::new(Filter::ByDomain("agentic")),
        );
        let result = select(&set, &filter);
        // rust=2, agentic=1 — no overlap → 3
        assert_eq!(result.len(), 3);
    }

    // ── Filter::And ──────────────────────────────────────────────────────────

    #[test]
    fn select_and_narrows_results() {
        let set = populated_set();
        let filter = Filter::And(
            Box::new(Filter::ByDomain("rust")),
            Box::new(Filter::ByEnforcement(EnforcementKind::Structured)),
        );
        let result = select(&set, &filter);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|r| r.domain == "rust"));
    }

    // ── select_by_ids convenience ─────────────────────────────────────────────

    #[test]
    fn select_by_ids_convenience_fn() {
        let set = populated_set();
        let ids = vec![RuleId("SPIRIT-OPTIMIZE-1".to_owned())];
        let result = select_by_ids(&set, &ids);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].domain, "*");
    }

    // ── select_for_domains (universal inclusion) ──────────────────────────────

    #[test]
    fn select_for_domains_includes_universal_rules() {
        let set = populated_set();
        // ask for "agentic" only — universal "*" should appear too
        let result = select_for_domains(&set, &["agentic"]);
        let ids: Vec<&str> = result.iter().map(|r| r.id_str()).collect();
        assert!(ids.contains(&"ORCH-NEW-PATH-TESTS-1"), "agentic rule present");
        assert!(ids.contains(&"SPIRIT-OPTIMIZE-1"), "universal rule included");
        // rust and api-layer rules must NOT appear
        assert!(!ids.contains(&"RUST-DOMAIN-1"));
        assert!(!ids.contains(&"ARCH-STRICT-LAYERING-1"));
    }

    #[test]
    fn select_for_domains_empty_domain_list_returns_only_universals() {
        let set = populated_set();
        let result = select_for_domains(&set, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].domain, "*");
    }

    // ── EnforcementKind Display ────────────────────────────────────────────

    #[test]
    fn enforcement_kind_display() {
        assert_eq!(EnforcementKind::Prose.to_string(), "prose");
        assert_eq!(EnforcementKind::Structured.to_string(), "structured");
        assert_eq!(EnforcementKind::Mechanical.to_string(), "mechanical");
    }

    // ── Async corpus loader (integration test against real corpus) ────────────

    #[tokio::test]
    async fn load_corpus_loads_real_corpus() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            // Skip if corpus not present (CI without the camerata-ai checkout).
            return;
        }
        let set = load_corpus(path).await.expect("corpus should load");
        // We know the corpus has 107 files; assert a reasonable lower bound.
        assert!(
            set.len() >= 50,
            "expected at least 50 rules, got {}",
            set.len()
        );
        // Every rule should have a non-empty id.
        for rule in set.iter() {
            assert!(!rule.id.0.is_empty(), "empty id in rule {:?}", rule.title);
        }
    }

    #[tokio::test]
    async fn load_corpus_lenient_skips_bad_files_but_loads_good_ones() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let (set, errors) = load_corpus_lenient(path).await;
        // Lenient load should succeed on every well-formed file.
        assert!(
            set.len() >= 50,
            "expected at least 50 rules in lenient load, got {}",
            set.len()
        );
        // The real corpus should be clean.
        assert!(
            errors.is_empty(),
            "unexpected parse errors: {errors:#?}"
        );
    }

    #[tokio::test]
    async fn load_corpus_domain_index_consistent_with_iter() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        // Every rule returned by iter() must be reachable via get_by_id.
        for rule in set.iter() {
            let found = set
                .get_by_id(rule.id_str())
                .expect("iter rule must be in id index");
            assert_eq!(found.id.0, rule.id.0);
        }
        // Every rule returned by iter() must appear in its domain bucket.
        for rule in set.iter() {
            let bucket = set.get_by_domain(&rule.domain);
            let in_bucket = bucket.iter().any(|r| r.id.0 == rule.id.0);
            assert!(
                in_bucket,
                "rule {} (domain={}) not found in domain bucket",
                rule.id.0, rule.domain
            );
        }
    }
}
