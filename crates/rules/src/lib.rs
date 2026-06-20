//! camerata-rules: rule corpus loader, enforcement-kind classifier, and
//! per-task rule-subset selection.
//!
//! # Responsibilities
//!
//! 1. Recursively load TOML rule files from a corpus directory (the bundled
//!    `crates/rules/principles/` by default; override with `CAMERATA_CORPUS_PATH`).
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

/// The emission tiers for a camerata rule (from CAMERATA-ANATOMY-1).
///
/// - `Prose`         — human-readable rationale only; no generated artifact.
/// - `Structured`    — emits a structured section (e.g. a CONVENTIONS.md entry).
/// - `Mechanical`    — emits a runnable check (linter, regex, CI gate, etc.).
/// - `Architectural` — a *deterministically* checkable structural rule that no
///   regex can express and no LLM is needed to judge: it requires parsing the
///   code into an AST and reasoning over its structure (e.g. "a handler does
///   not touch the DB directly", "a service does not bypass the repository",
///   "no cross-boundary imports"). Like `Mechanical`, it is a hard, repeatable
///   check; unlike `Mechanical`, the check is an AST/static-analysis pass rather
///   than a lint pattern. See
///   `docs/decisions/2026-06-19_ast_architectural_rule_tier.md`.
///
/// Tier ordering by strictness/automation: `Prose` < `Structured` < `Mechanical`
/// < `Architectural`. `Architectural` is the most precise tier — it never
/// produces a false "probably" the way a regex digest scan can.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnforcementKind {
    Prose,
    Structured,
    Mechanical,
    Architectural,
}

impl EnforcementKind {
    /// The lowercase wire/TOML string for this tier. Inverse of [`EnforcementKind::from_tag`].
    pub fn as_str(&self) -> &'static str {
        match self {
            EnforcementKind::Prose => "prose",
            EnforcementKind::Structured => "structured",
            EnforcementKind::Mechanical => "mechanical",
            EnforcementKind::Architectural => "architectural",
        }
    }

    /// Parse a tier from its lowercase wire/TOML string. Inverse of
    /// [`EnforcementKind::as_str`]. Unknown strings return `None`.
    ///
    /// Named `from_tag` rather than `from_str` so it is not confused with the
    /// `std::str::FromStr` trait method (which would return a `Result`).
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "prose" => Some(EnforcementKind::Prose),
            "structured" => Some(EnforcementKind::Structured),
            "mechanical" => Some(EnforcementKind::Mechanical),
            "architectural" => Some(EnforcementKind::Architectural),
            _ => None,
        }
    }

    /// Whether this tier is enforced at the CI / integration stage rather than at
    /// the write-time gate. Both `Mechanical` (lint / query-plan / migration audit)
    /// and `Architectural` (AST static analysis) run in CI: they need the full
    /// parsed module (or build/DB context), which the write-time gate does not have.
    /// `Prose` and `Structured` are human-reviewed at PR.
    pub fn is_ci_enforced(&self) -> bool {
        matches!(
            self,
            EnforcementKind::Mechanical | EnforcementKind::Architectural
        )
    }

    /// Whether this tier emits into `CONVENTIONS.md` (citable by id) rather than
    /// `AGENTS.md` (agent-judged prose). Everything except `Prose` is citable.
    pub fn emits_to_conventions(&self) -> bool {
        !matches!(self, EnforcementKind::Prose)
    }
}

impl std::fmt::Display for EnforcementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
    /// Whether this rule ships an adopted default option. When `false`, the
    /// architect MUST choose an alternative at onboarding.
    #[serde(default)]
    default: bool,
    /// The alternatives the architect chooses among (`[[option]]` blocks).
    #[serde(default, rename = "option")]
    options: Vec<OptionToml>,
}

#[derive(Debug, Deserialize)]
struct DecisionToml {
    /// The decision this rule frames (e.g. "What position does the project take on …?").
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    why: Option<String>,
    /// The id of the default option, when one is adopted.
    #[serde(default)]
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OptionToml {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
    #[serde(default)]
    why: String,
}

// ────────────────────────────────────────────────────────────────────────────
// Public domain types
// ────────────────────────────────────────────────────────────────────────────

/// One alternative the architect can codify for a rule (a `[[option]]` block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOption {
    /// Stable option id (what gets codified as the choice).
    pub id: String,
    /// Human label.
    pub label: String,
    /// The concrete directive this alternative codifies.
    pub directive: String,
    /// Why this alternative (the rationale; the default option says so).
    pub why: String,
}

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
    /// The decision this rule frames (`[decision].question`), when present. The architect
    /// reads this to understand WHAT they are choosing between.
    pub decision_question: Option<String>,
    /// The rationale for the adopted default (`[decision].why`), when present.
    pub decision_why: Option<String>,
    /// The alternatives the architect chooses among. May be empty (a mechanical
    /// rule with no variants).
    pub options: Vec<RuleOption>,
    /// The default option id, when this rule adopts one. `None` means the
    /// architect MUST choose an alternative — there is no default to fall back on.
    pub default_option: Option<String>,
}

impl Rule {
    /// Whether this rule has an adopted default option.
    pub fn has_default(&self) -> bool {
        self.default_option.is_some()
    }
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

/// Default corpus path: the rule TOML bundled IN this repo, under
/// `crates/rules/principles/`. Resolved from the crate's manifest dir so it works from
/// any working directory and the repo is self-contained (no external checkout needed).
/// Override at runtime with `CAMERATA_CORPUS_PATH` (see [`corpus_path`]).
pub const DEFAULT_CORPUS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/principles");

/// The corpus directory to load: the `CAMERATA_CORPUS_PATH` env override if set and
/// non-empty, else the bundled [`DEFAULT_CORPUS_PATH`].
pub fn corpus_path() -> std::path::PathBuf {
    std::env::var("CAMERATA_CORPUS_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_CORPUS_PATH))
}

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
pub async fn load_corpus_lenient(corpus_dir: &Path) -> (RuleSet, Vec<(PathBuf, RulesError)>) {
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
            source: std::io::Error::other(join_err.to_string()),
        })
    })
}

fn collect_toml_paths_sync(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), RulesError> {
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

    // The default option id, only when the rule adopts one (`default = true`).
    let default_option = if raw.default {
        raw.decision
            .as_ref()
            .and_then(|d| d.default.clone())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    // The decision context the architect reads in the rule-detail view: the question being
    // decided and the rationale for the adopted default.
    let decision_question = raw
        .decision
        .as_ref()
        .and_then(|d| d.question.clone())
        .filter(|s| !s.is_empty());
    let decision_why = raw
        .decision
        .as_ref()
        .and_then(|d| d.why.clone())
        .filter(|s| !s.is_empty());

    let options = raw
        .options
        .into_iter()
        .map(|o| RuleOption {
            id: o.id,
            label: o.label,
            directive: o.directive,
            why: o.why,
        })
        .collect();

    Ok(Rule {
        id: RuleId(raw.id),
        title: raw.title,
        enforcement: raw.enforcement,
        domain: raw.domain,
        summary,
        decision_question,
        decision_why,
        options,
        default_option,
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
    rule_set
        .iter()
        .filter(|r| matches_filter(r, filter))
        .collect()
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
        .filter(|r| r.domain == "*" || domains.iter().any(|d| r.domain == *d))
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────
// Role builder
// ────────────────────────────────────────────────────────────────────────────

/// Build a [`camerata_core::Role`] from the corpus at `corpus_path`.
///
/// Rules are selected using OR semantics across two axes:
///
/// 1. **Domain match** — any rule whose `domain` field appears in `domains`
///    (e.g. `"rust"`, `"sql"`, `"agentic"`).  Universal rules (`domain = "*"`)
///    are always included regardless of what `domains` contains.
/// 2. **Explicit id override** — any rule whose id string appears in
///    `rule_ids` is included even if its domain was not requested.
///
/// `domains` and `rule_ids` may each be empty; an empty `domains` with an
/// empty `rule_ids` produces a role containing only universal rules.
///
/// # `allowed_paths` default
///
/// When no explicit domain-to-path mapping is needed the caller can pass an
/// empty slice.  The function derives a sensible default: one glob per domain
/// in `domains` (e.g. `"rust"` → `"**/*.rs"`), plus `"**"` for universal
/// coverage. Callers that need precise path restrictions should call
/// [`camerata_core::Role`] constructors directly after obtaining the
/// `rule_subset`.
///
/// # Errors
///
/// Propagates [`RulesError`] from the corpus loader (I/O or TOML parse
/// failures).  Consider [`load_corpus_lenient`] if you prefer to skip
/// malformed files rather than fail.
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use camerata_rules::{role_from_corpus, DEFAULT_CORPUS_PATH};
///
/// # async fn example() {
/// let role = role_from_corpus(
///     Path::new(DEFAULT_CORPUS_PATH),
///     "Backend",
///     &["rust", "sql", "agentic"],
///     &[],
/// )
/// .await
/// .unwrap();
///
/// assert!(!role.rule_subset.is_empty());
/// # }
/// ```
pub async fn role_from_corpus(
    corpus_path: &Path,
    role_name: &str,
    domains: &[&str],
    rule_ids: &[&str],
) -> Result<camerata_core::Role, RulesError> {
    let set = load_corpus(corpus_path).await?;

    // Collect matching rules: universal + domain-match + explicit-id override.
    let mut subset: Vec<RuleId> = set
        .iter()
        .filter(|r| {
            // Universal rules always included.
            if r.domain == "*" {
                return true;
            }
            // Domain match.
            if domains.iter().any(|d| r.domain == *d) {
                return true;
            }
            // Explicit id override — allows pulling in rules from foreign
            // domains when the caller knows the exact id.
            if rule_ids.iter().any(|id| r.id.0 == *id) {
                return true;
            }
            false
        })
        .map(|r| r.id.clone())
        .collect();

    // Stable sort by id string for deterministic ordering.
    subset.sort_by(|a, b| a.0.cmp(&b.0));

    // Derive sensible allowed_paths from domains.
    let allowed_paths = derive_allowed_paths(domains);

    Ok(camerata_core::Role {
        name: role_name.to_owned(),
        rule_subset: subset,
        allowed_paths,
    })
}

/// Derive a default `allowed_paths` glob list from a domain slice.
///
/// Maps well-known domains to file-extension globs; unknown domains fall back
/// to `"**"` (all files).  The list always includes `"**"` so the role is
/// never inadvertently restricted to zero paths.
fn derive_allowed_paths(domains: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = domains.iter().map(|d| domain_to_glob(d)).collect();

    // Always add a universal catch-all so the role is usable even if the
    // domain mapping is incomplete.
    if !paths.contains(&"**".to_owned()) {
        paths.push("**".to_owned());
    }

    paths
}

/// Map a single domain string to a file-glob pattern.
fn domain_to_glob(domain: &str) -> String {
    // Handle sub-domain variants (e.g. "rust:dioxus", "rust:seaorm") by
    // using the primary component only.
    let primary = domain.split(':').next().unwrap_or(domain);
    match primary {
        "rust" => "**/*.rs".to_owned(),
        "sql" => "**/*.sql".to_owned(),
        "javascript" => "**/*.{js,ts,jsx,tsx}".to_owned(),
        "ui" => "**/*.{tsx,css}".to_owned(),
        "iac" => "**/*.tf".to_owned(),
        "ci-cd" => "**/.github/**".to_owned(),
        _ => "**".to_owned(),
    }
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
            decision_question: None,
            decision_why: None,
            options: Vec::new(),
            default_option: None,
        }
    }

    #[tokio::test]
    async fn corpus_rule_loads_options_and_default() {
        // ARCH-BOUNDARY-VALIDATION-1 has a default + three [[option]] alternatives.
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the camerata-ai checkout
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let Some(rule) = set.get_by_id("ARCH-BOUNDARY-VALIDATION-1") else {
            return; // rule not in this corpus version
        };
        assert!(rule.has_default(), "this rule ships an adopted default");
        assert!(
            rule.options.len() >= 2,
            "it has alternatives to choose among: {}",
            rule.options.len()
        );
        let default_id = rule.default_option.clone().unwrap();
        assert!(
            rule.options.iter().any(|o| o.id == default_id),
            "the default option id resolves to a real option"
        );
    }

    fn populated_set() -> RuleSet {
        let mut set = RuleSet::default();
        set.push(make_rule(
            "RUST-DOMAIN-1",
            "rust",
            EnforcementKind::Structured,
        ));
        set.push(make_rule(
            "RUST-DOMAIN-4",
            "rust",
            EnforcementKind::Structured,
        ));
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
        assert!(
            ids.contains(&"ORCH-NEW-PATH-TESTS-1"),
            "agentic rule present"
        );
        assert!(
            ids.contains(&"SPIRIT-OPTIMIZE-1"),
            "universal rule included"
        );
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
        assert_eq!(EnforcementKind::Architectural.to_string(), "architectural");
    }

    #[test]
    fn enforcement_kind_str_round_trip() {
        for kind in [
            EnforcementKind::Prose,
            EnforcementKind::Structured,
            EnforcementKind::Mechanical,
            EnforcementKind::Architectural,
        ] {
            let s = kind.as_str();
            assert_eq!(
                EnforcementKind::from_tag(s),
                Some(kind.clone()),
                "round-trip for {s}"
            );
        }
        assert_eq!(EnforcementKind::from_tag("nonsense"), None);
    }

    #[test]
    fn enforcement_kind_toml_deserializes_architectural() {
        // The serde rename must accept the lowercase "architectural" tag from a
        // corpus TOML file.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            enforcement: EnforcementKind,
        }
        let w: Wrapper = toml::from_str(r#"enforcement = "architectural""#)
            .expect("architectural tier should deserialize");
        assert_eq!(w.enforcement, EnforcementKind::Architectural);
    }

    #[test]
    fn enforcement_kind_tier_partitioning() {
        // Architectural is CI-enforced (like mechanical) and citable in CONVENTIONS.md.
        assert!(EnforcementKind::Architectural.is_ci_enforced());
        assert!(EnforcementKind::Mechanical.is_ci_enforced());
        assert!(!EnforcementKind::Structured.is_ci_enforced());
        assert!(!EnforcementKind::Prose.is_ci_enforced());

        assert!(EnforcementKind::Architectural.emits_to_conventions());
        assert!(EnforcementKind::Mechanical.emits_to_conventions());
        assert!(EnforcementKind::Structured.emits_to_conventions());
        assert!(!EnforcementKind::Prose.emits_to_conventions());
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
    async fn corpus_loads_architectural_tier_rules() {
        // The bundled corpus ships the example Architectural-tier rules; loading
        // them exercises the serde rename round-trip against real files.
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let Some(rule) = set.get_by_id("ARCH-HANDLER-NO-DB-1") else {
            return; // rule not in this corpus version
        };
        assert_eq!(
            rule.enforcement,
            EnforcementKind::Architectural,
            "ARCH-HANDLER-NO-DB-1 must load as the Architectural tier"
        );
        assert!(rule.enforcement.is_ci_enforced());
        assert!(rule.enforcement.emits_to_conventions());

        // Confirm the tier participates in enforcement-based selection.
        let arch = select(&set, &Filter::ByEnforcement(EnforcementKind::Architectural));
        assert!(
            arch.iter().any(|r| r.id_str() == "ARCH-HANDLER-NO-DB-1"),
            "Architectural filter must surface the rule"
        );
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
        assert!(errors.is_empty(), "unexpected parse errors: {errors:#?}");
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

    // ── role_from_corpus (integration test against real corpus) ──────────────

    #[tokio::test]
    async fn role_from_corpus_backend_has_rust_domain_2() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            // Skip when corpus is not present (CI without camerata-ai checkout).
            return;
        }

        let role = role_from_corpus(path, "Backend", &["rust", "sql", "agentic"], &[])
            .await
            .expect("role_from_corpus should succeed");

        assert_eq!(role.name, "Backend");

        // rule_subset must be non-empty.
        assert!(
            !role.rule_subset.is_empty(),
            "expected non-empty rule_subset for Backend role"
        );

        // Must contain at least the well-known RUST-DOMAIN-2 rule.
        let known_id = RuleId("RUST-DOMAIN-2".to_owned());
        assert!(
            role.rule_subset.contains(&known_id),
            "expected RUST-DOMAIN-2 in Backend rule_subset; got {:?}",
            role.rule_subset
        );

        // allowed_paths must include at least one entry.
        assert!(
            !role.allowed_paths.is_empty(),
            "expected non-empty allowed_paths"
        );

        // Report the subset size for the caller.
        eprintln!(
            "[role_from_corpus test] Backend rule_subset size = {}",
            role.rule_subset.len()
        );
    }

    #[tokio::test]
    async fn role_from_corpus_explicit_rule_id_override() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }

        // Ask for no domains but pull in RUST-DOMAIN-2 by explicit id.
        let role = role_from_corpus(path, "Targeted", &[], &["RUST-DOMAIN-2"])
            .await
            .expect("role_from_corpus should succeed");

        let known_id = RuleId("RUST-DOMAIN-2".to_owned());
        assert!(
            role.rule_subset.contains(&known_id),
            "explicit rule_id override should include RUST-DOMAIN-2"
        );
    }

    #[tokio::test]
    async fn role_from_corpus_empty_args_returns_only_universals() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }

        let role = role_from_corpus(path, "Universal", &[], &[])
            .await
            .expect("role_from_corpus should succeed");

        // With no domains / ids the subset must consist only of universal rules.
        let set = load_corpus(path).await.expect("corpus loads");
        let universal_count = set.iter().filter(|r| r.domain == "*").count();
        assert_eq!(
            role.rule_subset.len(),
            universal_count,
            "expected only universal rules when no domains specified"
        );
    }

    // ── domain_to_glob (via derive_allowed_paths) ─────────────────────────────

    #[test]
    fn derive_allowed_paths_always_appends_star_star_catch_all() {
        // Even for a known domain, the list must include "**" at the end so the
        // role is never inadvertently path-restricted to zero files.
        let paths = derive_allowed_paths(&["rust"]);
        assert!(
            paths.contains(&"**".to_string()),
            "rust domain must still have ** catch-all: {paths:?}"
        );
        // For an unknown domain the only entry returned is "**".
        let unknown = derive_allowed_paths(&["unknown-lang"]);
        assert!(unknown.contains(&"**".to_string()));
    }

    #[test]
    fn derive_allowed_paths_maps_known_domains() {
        // Spot-check that well-known domains produce their expected glob.
        let paths = derive_allowed_paths(&["rust"]);
        assert!(paths.contains(&"**/*.rs".to_string()), "{paths:?}");

        let paths = derive_allowed_paths(&["sql"]);
        assert!(paths.contains(&"**/*.sql".to_string()), "{paths:?}");

        let paths = derive_allowed_paths(&["javascript"]);
        assert!(
            paths.contains(&"**/*.{js,ts,jsx,tsx}".to_string()),
            "{paths:?}"
        );

        let paths = derive_allowed_paths(&["iac"]);
        assert!(paths.contains(&"**/*.tf".to_string()), "{paths:?}");
    }

    #[test]
    fn domain_to_glob_subdomain_variant_strips_suffix() {
        // Sub-domains like "rust:dioxus" must map to the primary component's glob,
        // not fall through to the "**" catch-all.
        assert_eq!(domain_to_glob("rust:dioxus"), "**/*.rs");
        assert_eq!(domain_to_glob("rust:seaorm"), "**/*.rs");
        assert_eq!(domain_to_glob("sql:postgres"), "**/*.sql");
    }

    #[test]
    fn domain_to_glob_unknown_domain_returns_double_star() {
        assert_eq!(domain_to_glob("my-custom-domain"), "**");
        assert_eq!(domain_to_glob(""), "**");
    }

    #[test]
    fn derive_allowed_paths_does_not_duplicate_star_star() {
        // When the only domain is "unknown" (which maps to "**"), we should get
        // exactly one "**", not two.
        let paths = derive_allowed_paths(&["unknown-lang"]);
        let count = paths.iter().filter(|p| p.as_str() == "**").count();
        assert_eq!(count, 1, "should not duplicate the ** catch-all: {paths:?}");
    }

    #[test]
    fn derive_allowed_paths_empty_domains_returns_only_catch_all() {
        let paths = derive_allowed_paths(&[]);
        assert_eq!(paths, vec!["**".to_string()]);
    }

    // ── Rule::has_default + id_str convenience methods ───────────────────────

    #[test]
    fn rule_has_default_reflects_default_option_field() {
        let mut r = make_rule("R1", "rust", EnforcementKind::Structured);
        assert!(!r.has_default());
        r.default_option = Some("opt-a".to_string());
        assert!(r.has_default());
    }

    #[test]
    fn rule_id_str_returns_inner_string() {
        let r = make_rule("MY-RULE-1", "rust", EnforcementKind::Prose);
        assert_eq!(r.id_str(), "MY-RULE-1");
    }
}
