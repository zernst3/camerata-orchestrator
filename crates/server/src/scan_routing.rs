//! Rule-routing: send each rule only the files it could possibly apply to.
//!
//! The AI audit's dominant cost is re-sending the codebase once per rule-batch (see
//! `estimate_audit_cost`). Many rules are **language-specific** — a `RUST-*` convention can't be
//! violated by a `.ts` file, a `REACT-*` rule can't fire on a `.rs` file — so auditing those rules
//! against the whole tree pays to read code the rule provably cannot match. Routing each
//! language-scoped rule to only its language's files collapses that waste, which is the big lever
//! on a **polyglot** repo (e.g. a Rust backend + a TypeScript frontend).
//!
//! ## Two sources of scope
//!
//! 1. **The rule id's language prefix** (`RUST-`, `PY-`, `REACT-`, …): always on, unchanged.
//! 2. **The rule's declared `applies_to`** (token-efficiency rank 7): file globs and languages
//!    the rule's author wrote in the rule TOML (`camerata_rules::AppliesTo`). Only honoured when
//!    the `applies_to_routing` efficiency switch is on; off, the field is ignored entirely and
//!    routing is exactly what it was before the field existed.
//!
//! A rule with both is routed by BOTH (the language prefix AND the declared scope).
//!
//! ## Safety first: routing must never cause a MISSED finding
//!
//! Excluding a file from a rule is only sound when the rule provably cannot apply to it. This
//! module is therefore **conservative**:
//!
//! * a rule with no recognized language prefix and no declared `applies_to` gets [`Scope::All`]
//!   and audits every file (architectural `ARCH-`, security `SEC-`, SQL/DB, API/process rules can
//!   live in any language's files). When in doubt, a rule audits everything;
//! * a declared `applies_to` is a UNION (any listed glob OR any listed language), and a file
//!   whose language cannot be determined is treated as a match for the language part: an unknown
//!   or ambiguous attribute SENDS the rule (false-negative bias). The loader already refuses a
//!   scope it cannot honour, so a declared scope never silently matches nothing;
//! * a rule that ends up with zero files is not dropped quietly: the caller records it as
//!   `not_assessed` (see `run_routed_passes`'s `skipped_rules`).
//!
//! ## The unit that is routed
//!
//! Routing is decided per FILE, before chunking: a group's files are re-chunked on their own, so
//! a chunk only contains files its rules can apply to. The (chunk, rule) pairs the model sees are
//! therefore exactly "this chunk's files all matched this rule's scope".
//!
//! This module is the PURE core (classification + grouping + a savings estimate). The advisory
//! "flag novel issues" pass is gated to one pass per file chunk, which `run_routed_passes` keeps
//! intact — see `docs/decisions/2026-06-19_rule_routing.md`.

use std::collections::BTreeMap;

use camerata_rules::{AppliesTo, Candidates};

/// One rule as the audit passes and the router see it: the id and chosen directive the prompt is
/// built from, plus the routing metadata the corpus declares for it. Passed through `audit_repo`,
/// `plan_routes` and `run_routed_passes` whole (not as `(id, directive)` pairs) so a new piece of
/// per-rule routing metadata is one more field here, not a signature change down the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRule {
    /// The rule id (`ARCH-STRICT-LAYERING-1`).
    pub id: String,
    /// The directive text the audit prompt carries for this rule.
    pub directive: String,
    /// The rule's declared scope (`applies_to` in its TOML). `None` = every file.
    pub applies_to: Option<AppliesTo>,
    /// The rule's declared candidate locator (`candidates` in its TOML, token-efficiency rank 8).
    /// `None` = the rule is sent whole files. Read by the audit's candidate prefilter, not by the
    /// router: it changes what a chunk's prompt shows the rule, never which files it is routed.
    pub candidates: Option<Candidates>,
}

impl AuditRule {
    /// A rule with no declared scope (every file): what every pre-rank-7 caller passed.
    pub fn new(id: impl Into<String>, directive: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            directive: directive.into(),
            applies_to: None,
            candidates: None,
        }
    }

    /// The same rule carrying `applies_to`.
    #[must_use]
    pub fn with_applies_to(mut self, applies_to: Option<AppliesTo>) -> Self {
        self.applies_to = applies_to;
        self
    }

    /// The same rule carrying `candidates`.
    #[must_use]
    pub fn with_candidates(mut self, candidates: Option<Candidates>) -> Self {
        self.candidates = candidates;
        self
    }

    /// The `(id, directive)` pair the prompt builders take.
    pub fn pair(&self) -> (String, String) {
        (self.id.clone(), self.directive.clone())
    }
}

impl From<(String, String)> for AuditRule {
    fn from((id, directive): (String, String)) -> Self {
        Self::new(id, directive)
    }
}

/// Rules with no declared scope from `(id, directive)` pairs: for a caller (or test) that has
/// only the pairs.
pub fn rules_from_pairs(pairs: &[(String, String)]) -> Vec<AuditRule> {
    pairs.iter().cloned().map(AuditRule::from).collect()
}

/// Rules from `(id, directive)` pairs, each carrying the `applies_to` and `candidates` the loaded
/// `corpus` declares for it. A rule the corpus does not know (or no corpus at all) gets neither,
/// i.e. every file, whole: the safe fallback.
pub fn audit_rules(
    pairs: &[(String, String)],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<AuditRule> {
    pairs
        .iter()
        .map(|(id, directive)| {
            let rule = corpus.and_then(|c| {
                c.get_by_id(id)
                    .or_else(|| c.get_by_id(&id.to_ascii_uppercase()))
            });
            AuditRule::new(id.clone(), directive.clone())
                .with_applies_to(rule.and_then(|r| r.applies_to.clone()))
                .with_candidates(rule.and_then(|r| r.candidates))
        })
        .collect()
}

/// `(id, directive)` pairs of `rules`, in order.
pub fn rule_pairs(rules: &[AuditRule]) -> Vec<(String, String)> {
    rules.iter().map(AuditRule::pair).collect()
}

/// Which files a rule should be audited against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// A recognized single programming language — audit only that language's files.
    Language(&'static str),
    /// The rule declares `applies_to`: audit the files that scope matches. `language` is the
    /// id-prefix language when the rule also has one (both must hold).
    Declared {
        /// The id-prefix language, when the rule has one.
        language: Option<&'static str>,
        /// The rule's declared scope.
        applies: AppliesTo,
    },
    /// Cross-cutting / unknown — audit every file (the safe default; never causes a miss).
    All,
}

/// Map a file extension (lowercase, no dot) to its canonical language key, if known. The one
/// resolver lives in `camerata-rules` so the `applies_to.languages` vocabulary and this cannot
/// drift.
pub fn lang_for_extension(ext: &str) -> Option<&'static str> {
    camerata_rules::applies_to::lang_for_extension(ext)
}

/// The scope key for rules that apply to every JVM language (Java and Kotlin).
const JVM_SCOPE: &str = "jvm";

/// Whether a scope's language key covers a file whose own language is `file_lang`: the same
/// language, or the JVM scope over a Java or Kotlin file.
fn language_covers(scope_lang: &str, file_lang: &str) -> bool {
    scope_lang == file_lang || (scope_lang == JVM_SCOPE && matches!(file_lang, "java" | "kotlin"))
}

/// The canonical language a file belongs to, from its path's extension.
fn file_language(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    lang_for_extension(&ext)
}

/// Recognized id-prefix tokens that pin a rule to a single language. Matched against the rule
/// id's leading segment (case-insensitive). Only HIGH-CONFIDENCE language pins live here — a
/// token here means "a rule whose id starts with this provably targets that language's code".
const LANGUAGE_PREFIXES: &[(&str, &str)] = &[
    ("RUST", "rust"),
    ("DIOXUS", "rust"), // Dioxus is a Rust UI framework
    ("LEPTOS", "rust"),
    ("AXUM", "rust"),
    ("SEAORM", "rust"),
    ("PY", "python"),
    ("PYTHON", "python"),
    ("DJANGO", "python"),
    ("FLASK", "python"),
    ("FASTAPI", "python"),
    ("TS", "web"),
    ("TYPESCRIPT", "web"),
    ("JS", "web"),
    ("JAVASCRIPT", "web"),
    ("REACT", "web"),
    ("NEXT", "web"),
    ("NEXTJS", "web"),
    ("VUE", "web"),
    ("SVELTE", "web"),
    ("ANGULAR", "web"),
    ("NODE", "web"),
    ("GO", "go"),
    ("GOLANG", "go"),
    ("JAVA", "java"),
    // Spring runs on both JVM languages: a Spring Boot service in Kotlin is as common as one in
    // Java, and a Spring rule routed to `java` alone never saw it. `jvm` is a scope key, not an
    // `applies_to` language: it covers Java and Kotlin files (see [`language_covers`]).
    ("SPRING", JVM_SCOPE),
    ("KOTLIN", "kotlin"),
    ("RUBY", "ruby"),
    ("RAILS", "ruby"),
    ("PHP", "php"),
    ("LARAVEL", "php"),
    ("CSHARP", "csharp"),
    ("DOTNET", "csharp"),
    ("SWIFT", "swift"),
];

/// Classify a rule's scope from its id. Returns [`Scope::Language`] only when the id's leading
/// segment is a recognized single-language token; everything else (architectural, security, SQL,
/// process, or unknown) is [`Scope::All`] so it audits every file. Ignores `applies_to`; see
/// [`scope_of`] for the full classification.
pub fn rule_scope(rule_id: &str) -> Scope {
    let head = rule_id
        .split(['-', '_', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    for (tok, lang) in LANGUAGE_PREFIXES {
        if head == *tok {
            return Scope::Language(lang);
        }
    }
    Scope::All
}

/// The full scope of `rule`: its id-prefix language, narrowed by its declared `applies_to` when
/// `applies_to_routing` is on. With the switch off the declared scope is not even read, so the
/// result is exactly [`rule_scope`] of the id (the pre-rank-7 behavior).
pub fn scope_of(rule: &AuditRule, applies_to_routing: bool) -> Scope {
    let by_id = rule_scope(&rule.id);
    match (&rule.applies_to, applies_to_routing) {
        (Some(applies), true) => Scope::Declared {
            language: match by_id {
                Scope::Language(l) => Some(l),
                _ => None,
            },
            applies: applies.clone(),
        },
        _ => by_id,
    }
}

/// Whether a file is in a declared scope: its path matches a declared glob, or its text contains
/// a declared marker, or it is in a declared language. A file whose language cannot be
/// determined counts as in scope for the language part (an unknown attribute SENDS the rule); a
/// scope that declares no `languages` has no language part, so an unmatched file is out.
///
/// The text check reads the file content the scan already holds; it is the last test tried, so a
/// file the paths already admit is never lower-cased.
fn declared_scope_matches(path: &str, content: &str, applies: &AppliesTo) -> bool {
    let lower = path.to_ascii_lowercase();
    if applies
        .paths
        .iter()
        .any(|g| camerata_checks::arch_checker::glob_match(&g.to_ascii_lowercase(), &lower))
    {
        return true;
    }
    if !applies.languages.is_empty() {
        match file_language(path) {
            Some(lang) if applies.languages.iter().any(|l| l == lang) => return true,
            None => return true,
            Some(_) => {}
        }
    }
    if applies.contains_any.is_empty() {
        return false;
    }
    let text = content.to_lowercase();
    applies
        .contains_any
        .iter()
        .any(|m| text.contains(m.as_str()))
}

/// Whether a file is in a rule's scope. [`Scope::All`] matches everything; a language scope
/// matches only files of that language. Files of an UNKNOWN language (config/data — toml, json,
/// sql, sh) are matched by [`Scope::All`] but NOT by any id-prefix language scope (a `RUST-` rule
/// has no business in a `.sql` file). A declared scope follows [`declared_scope_matches`].
pub fn file_in_scope(path: &str, content: &str, scope: &Scope) -> bool {
    match scope {
        Scope::All => true,
        Scope::Language(lang) => file_language(path).is_some_and(|f| language_covers(lang, f)),
        Scope::Declared { language, applies } => {
            language.is_none_or(|l| file_language(path).is_some_and(|f| language_covers(l, f)))
                && declared_scope_matches(path, content, applies)
        }
    }
}

/// Build and configuration files a language-scoped rule needs to see beside its language's
/// source: the dependency manifest names the framework and versions (`Cargo.toml`,
/// `package.json`) and the application config holds the settings Spring and Django rules judge
/// (`application.yml`). They have no routed language, so an id-prefix language scope used to skip
/// them. Matched on the file name, at any depth.
const MANIFEST_BASENAMES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "composer.json",
    "Gemfile",
    "Package.swift",
    "application.yml",
    "application.yaml",
    "application.properties",
    "settings.py",
];

fn is_manifest_file(path: &str) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    MANIFEST_BASENAMES.contains(&basename)
        || (basename.starts_with("application-")
            && [".yml", ".yaml", ".properties"]
                .iter()
                .any(|ext| basename.ends_with(ext)))
        || basename.ends_with(".csproj")
}

/// The files of `files` a scope covers, owned (what a group's passes are chunked from).
///
/// H10: a language scope that matched at least one source file also receives the repo's manifest
/// and application config files ([`MANIFEST_BASENAMES`]). With no source file of its language the
/// group stays empty, so it is still reported as not assessed rather than "ran, found nothing" on
/// a lone `package.json`.
pub fn files_in_scope(files: &[(String, String)], scope: &Scope) -> Vec<(String, String)> {
    let covered: Vec<(String, String)> = files
        .iter()
        .filter(|(p, c)| file_in_scope(p, c, scope))
        .cloned()
        .collect();
    if matches!(scope, Scope::Language(_)) && !covered.is_empty() {
        return files
            .iter()
            .filter(|(p, c)| file_in_scope(p, c, scope) || is_manifest_file(p))
            .cloned()
            .collect();
    }
    covered
}

/// The files a single rule should be audited against (a view into `files`).
pub fn files_for_rule<'a>(
    rule: &AuditRule,
    applies_to_routing: bool,
    files: &'a [(String, String)],
) -> Vec<&'a (String, String)> {
    let scope = scope_of(rule, applies_to_routing);
    let language_hit = files.iter().any(|(p, c)| file_in_scope(p, c, &scope));
    let with_manifests = matches!(scope, Scope::Language(_)) && language_hit;
    files
        .iter()
        .filter(|(p, c)| file_in_scope(p, c, &scope) || (with_manifests && is_manifest_file(p)))
        .collect()
}

/// A group of rules that share one scope (so they share one file subset), ready to feed the audit
/// engine as a unit.
#[derive(Debug, Clone)]
pub struct RouteGroup {
    pub scope: Scope,
    /// The rules in this group.
    pub rules: Vec<AuditRule>,
}

impl RouteGroup {
    /// The group's `(id, directive)` pairs, in order (what the prompt builders take).
    pub fn rule_pairs(&self) -> Vec<(String, String)> {
        rule_pairs(&self.rules)
    }
}

/// The full routing plan for a scan: rules grouped by scope, plus a chars-audited estimate that
/// quantifies the saving vs. the naive "every rule sees every file".
#[derive(Debug, Clone)]
pub struct RoutePlan {
    pub groups: Vec<RouteGroup>,
    /// Sum over rules of (chars of the files THAT rule will audit) — the routed input volume.
    pub routed_chars: u64,
    /// Sum over rules of (chars of ALL files) — the un-routed input volume (every rule × every file).
    pub full_chars: u64,
}

impl RoutePlan {
    /// Fraction of per-rule input bytes routing avoids (0.0 = no saving, 1.0 = everything skipped).
    pub fn saved_fraction(&self) -> f64 {
        if self.full_chars == 0 {
            return 0.0;
        }
        1.0 - (self.routed_chars as f64 / self.full_chars as f64)
    }
}

/// Build the routing plan for `selected` rules over `files`. Groups rules by scope (deterministic
/// order: id-prefix languages alphabetically, then declared scopes by their canonical key, then
/// `All` last) and computes the chars-audited estimate. `applies_to_routing` is the efficiency
/// switch: off, every rule's declared `applies_to` is ignored.
pub fn plan_routes(
    selected: &[AuditRule],
    files: &[(String, String)],
    applies_to_routing: bool,
) -> RoutePlan {
    let total_chars: u64 = files.iter().map(|(_, c)| c.len() as u64).sum();
    let mut by_scope: BTreeMap<String, RouteGroup> = BTreeMap::new();
    let full_chars = total_chars.saturating_mul(selected.len() as u64);

    for rule in selected {
        let scope = scope_of(rule, applies_to_routing);
        // Group key: "0:<lang>" for id-prefix languages (sort first, alphabetically), "1:..."
        // for declared scopes, "2" for All (last).
        let key = match &scope {
            Scope::Language(l) => format!("0:{l}"),
            Scope::Declared { language, applies } => {
                format!("1:{}:{}", language.unwrap_or("-"), applies.canonical_key())
            }
            Scope::All => "2".to_string(),
        };
        by_scope
            .entry(key)
            .or_insert_with(|| RouteGroup {
                scope: scope.clone(),
                rules: Vec::new(),
            })
            .rules
            .push(rule.clone());
    }

    // Rules in a group share one file subset, so the routed volume is per group.
    let routed_chars = by_scope
        .values()
        .map(|g| {
            let group_chars: u64 = files_in_scope(files, &g.scope)
                .iter()
                .map(|(_, c)| c.len() as u64)
                .sum();
            group_chars.saturating_mul(g.rules.len() as u64)
        })
        .sum();

    RoutePlan {
        groups: by_scope.into_values().collect(),
        routed_chars,
        full_chars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, n: usize) -> (String, String) {
        (path.to_string(), "x".repeat(n))
    }

    fn r(id: &str) -> AuditRule {
        AuditRule::new(id, "d")
    }

    #[test]
    fn language_prefixed_rules_get_a_language_scope() {
        assert_eq!(rule_scope("RUST-DIOXUS-2"), Scope::Language("rust"));
        assert_eq!(rule_scope("DIOXUS-NO-CLONE-1"), Scope::Language("rust"));
        assert_eq!(
            rule_scope("PY-NO-MUTABLE-DEFAULT-1"),
            Scope::Language("python")
        );
        assert_eq!(rule_scope("REACT-HOOKS-1"), Scope::Language("web"));
        assert_eq!(rule_scope("TS-STRICT-1"), Scope::Language("web"));
        assert_eq!(rule_scope("GO-ERRCHECK-1"), Scope::Language("go"));
    }

    #[test]
    fn cross_cutting_rules_audit_every_file() {
        // The families that can live in ANY language must never be routed away from a file.
        for id in [
            "ARCH-STRICT-LAYERING-1",
            "SEC-NO-RAW-SQL-CONCAT-1",
            "SQL-DB-INDEX-2",
            "API-IDEMPOTENT-1",
            "PROC-CITE-CONVENTION-1",
            "WHATEVER-NEW-1",
        ] {
            assert_eq!(rule_scope(id), Scope::All, "{id} must audit all files");
        }
    }

    #[test]
    fn file_in_scope_respects_language_and_all() {
        assert!(file_in_scope("src/main.rs", "", &Scope::Language("rust")));
        assert!(!file_in_scope("src/app.tsx", "", &Scope::Language("rust")));
        assert!(!file_in_scope("schema.sql", "", &Scope::Language("rust")));
        // All matches everything, including config/data files a language scope would skip.
        assert!(file_in_scope("schema.sql", "", &Scope::All));
        assert!(file_in_scope("src/main.rs", "", &Scope::All));
    }

    #[test]
    fn files_for_rule_filters_a_rust_rule_to_rust_files() {
        let files = vec![f("a.rs", 10), f("b.tsx", 10), f("c.sql", 10)];
        let routed = files_for_rule(&r("RUST-1"), true, &files);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].0, "a.rs");
        // A cross-cutting rule keeps everything.
        assert_eq!(files_for_rule(&r("ARCH-1"), true, &files).len(), 3);
    }

    #[test]
    fn plan_groups_by_scope_and_estimates_savings_on_polyglot() {
        // 1000 chars of Rust, 1000 of web, 200 of sql config.
        let files = vec![f("a.rs", 1000), f("b.tsx", 1000), f("c.sql", 200)];
        // A rust rule, a web rule, and a cross-cutting arch rule.
        let rules = vec![r("RUST-1"), r("REACT-1"), r("ARCH-1")];
        let plan = plan_routes(&rules, &files, true);

        // Full (naive): 3 rules × 2200 chars = 6600.
        assert_eq!(plan.full_chars, 6600);
        // Routed: RUST-1 sees 1000 (rs), REACT-1 sees 1000 (tsx), ARCH-1 sees 2200 (all) = 4200.
        assert_eq!(plan.routed_chars, 4200);
        assert!((plan.saved_fraction() - (1.0 - 4200.0 / 6600.0)).abs() < 1e-9);
        assert!(
            plan.saved_fraction() > 0.0,
            "routing saves input on a polyglot repo"
        );

        // Groups: rust, web, and All (3 distinct scopes).
        assert_eq!(plan.groups.len(), 3);
    }

    #[test]
    fn no_savings_for_single_language_repo_with_cross_cutting_rules() {
        // A pure-Python repo audited by cross-cutting rules: routing changes nothing (safe no-op).
        let files = vec![f("a.py", 500), f("b.py", 500)];
        let rules = vec![r("ARCH-1"), r("SEC-1")];
        let plan = plan_routes(&rules, &files, true);
        assert_eq!(
            plan.routed_chars, plan.full_chars,
            "no language pruning possible"
        );
        assert_eq!(plan.saved_fraction(), 0.0);
    }

    #[test]
    fn python_rule_in_polyglot_skips_non_python() {
        // A Python rule in a repo that also has Rust: the Rust files are pruned for it.
        let files = vec![f("a.py", 300), f("b.rs", 700)];
        let rules = vec![r("PY-1")];
        let plan = plan_routes(&rules, &files, true);
        assert_eq!(
            plan.routed_chars, 300,
            "the PY rule audits only the .py file"
        );
        assert_eq!(plan.full_chars, 1000);
    }

    #[test]
    fn rule_id_with_unusual_separators_and_casing() {
        // Edge case: rule IDs can use dashes, underscores, or colons as separators.
        // Ensure all variants are handled correctly and case-insensitively.
        assert_eq!(
            rule_scope("rust-something-1"),
            Scope::Language("rust"),
            "lowercase prefix with dashes"
        );
        assert_eq!(
            rule_scope("RUST_SOMETHING_1"),
            Scope::Language("rust"),
            "uppercase prefix with underscores"
        );
        assert_eq!(
            rule_scope("Rust:Something:1"),
            Scope::Language("rust"),
            "mixed case prefix with colons (should uppercase and match)"
        );
        assert_eq!(
            rule_scope("RuSt-WeirdCase-1"),
            Scope::Language("rust"),
            "mixed case normalized to uppercase"
        );
    }

    #[test]
    fn polyglot_with_unknown_language_files() {
        // Edge case: repo contains files in unknown languages (e.g., .md, .json, .sql).
        // A RUST-* rule must still NOT match .sql or .md, but Scope::All must match all.
        let files = vec![
            f("a.rs", 100),
            f("b.md", 50),
            f("c.sql", 75),
            f("d.json", 25),
        ];

        let rust_rule = vec![r("RUST-1")];
        let arch_rule = vec![r("ARCH-1")];

        // RUST-1 sees only the .rs file.
        let plan1 = plan_routes(&rust_rule, &files, true);
        assert_eq!(plan1.routed_chars, 100);

        // ARCH-1 sees ALL files (even unknown languages).
        let plan2 = plan_routes(&arch_rule, &files, true);
        assert_eq!(plan2.routed_chars, 250); // 100 + 50 + 75 + 25

        // Files_for_rule filters correctly.
        let rust_files = files_for_rule(&r("RUST-1"), true, &files);
        assert_eq!(rust_files.len(), 1);
        assert_eq!(rust_files[0].0, "a.rs");

        let arch_files = files_for_rule(&r("ARCH-1"), true, &files);
        assert_eq!(arch_files.len(), 4);
    }

    #[test]
    fn plan_routes_with_all_cross_cutting_set() {
        // Edge case: all rules are cross-cutting (Scope::All). No language pruning.
        // Saved fraction should be exactly 0.0.
        let files = vec![f("a.rs", 200), f("b.py", 300)];
        let rules = vec![r("ARCH-1"), r("SEC-1"), r("API-1")];
        let plan = plan_routes(&rules, &files, true);

        // Full: 3 rules × 500 chars = 1500.
        assert_eq!(plan.full_chars, 1500);
        // Routed: each rule sees all 500 chars = 1500.
        assert_eq!(plan.routed_chars, 1500);
        // No savings.
        assert_eq!(plan.saved_fraction(), 0.0);
        // Only one group (All scope).
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].scope, Scope::All);
        assert_eq!(plan.groups[0].rules.len(), 3);
    }

    #[test]
    fn plan_routes_deterministic_group_ordering() {
        // Edge case: multiple language-scoped groups should be ordered deterministically
        // (languages alphabetically, then All last). This prevents flaky test output.
        let files = vec![f("a.rs", 100), f("b.ts", 100), f("c.py", 100)];
        let rules = vec![
            r("REACT-1"), // web
            r("ARCH-1"),  // All
            r("PY-1"),    // python
            r("RUST-1"),  // rust
        ];
        let plan = plan_routes(&rules, &files, true);

        // Should have 4 groups (rust, python, web, All).
        assert_eq!(plan.groups.len(), 4);

        // Verify order: languages alphabetically (python, rust, web), then All.
        match &plan.groups[0].scope {
            Scope::Language(l) => assert_eq!(*l, "python"),
            _ => panic!("first group should be python"),
        }
        match &plan.groups[1].scope {
            Scope::Language(l) => assert_eq!(*l, "rust"),
            _ => panic!("second group should be rust"),
        }
        match &plan.groups[2].scope {
            Scope::Language(l) => assert_eq!(*l, "web"),
            _ => panic!("third group should be web"),
        }
        assert_eq!(plan.groups[3].scope, Scope::All);
    }

    #[test]
    fn empty_file_list_with_cross_cutting_rules() {
        // Edge case: no files to audit, but cross-cutting rules. Should have 0 routed chars.
        let files: Vec<(String, String)> = vec![];
        let rules = vec![r("ARCH-1"), r("SEC-1")];
        let plan = plan_routes(&rules, &files, true);

        assert_eq!(plan.full_chars, 0);
        assert_eq!(plan.routed_chars, 0);
        // Saved fraction on empty set should be 0.0 (safe default).
        assert_eq!(plan.saved_fraction(), 0.0);
    }

    #[test]
    fn rule_scope_detects_all_recognized_language_prefixes() {
        // Spot check: ensure a sample of prefixes are recognized.
        // Full list is in LANGUAGE_PREFIXES; this verifies the leading ones.
        assert_eq!(rule_scope("SEAORM-QUERY-1"), Scope::Language("rust"));
        assert_eq!(rule_scope("LEPTOS-COMPONENT-1"), Scope::Language("rust"));
        assert_eq!(rule_scope("DJANGO-SETTINGS-1"), Scope::Language("python"));
        assert_eq!(rule_scope("FASTAPI-ROUTE-1"), Scope::Language("python"));
        assert_eq!(rule_scope("NEXTJS-LAYOUT-1"), Scope::Language("web"));
        assert_eq!(rule_scope("ANGULAR-COMPONENT-1"), Scope::Language("web"));
        assert_eq!(rule_scope("SPRING-BOOT-1"), Scope::Language("jvm"));
        assert_eq!(rule_scope("DOTNET-ASYNC-1"), Scope::Language("csharp"));
    }

    // ── Declared scope (`applies_to`, rank 7) ────────────────────────────────────────

    fn scoped(id: &str, paths: &[&str], languages: &[&str]) -> AuditRule {
        AuditRule::new(id, "d").with_applies_to(Some(
            AppliesTo::new(
                paths.iter().map(|p| (*p).to_string()).collect(),
                languages.iter().map(|l| (*l).to_string()).collect(),
                vec![],
            )
            .unwrap(),
        ))
    }

    #[test]
    fn a_declared_path_glob_selects_matching_files_case_insensitively() {
        let rule = scoped("SUPA-1", &["**/*.sql", "supabase/**"], &[]);
        let files = vec![
            f("db/migrations/1.sql", 1),
            f("db/MIGRATIONS/2.SQL", 1),
            f("supabase/config.toml", 1),
            f("src/main.rs", 1),
            f("README.md", 1),
        ];
        let got: Vec<&str> = files_for_rule(&rule, true, &files)
            .iter()
            .map(|f| f.0.as_str())
            .collect();
        assert_eq!(
            got,
            vec![
                "db/migrations/1.sql",
                "db/MIGRATIONS/2.SQL",
                "supabase/config.toml"
            ]
        );
    }

    #[test]
    fn a_declared_scope_is_a_union_of_paths_and_languages() {
        let rule = scoped("X-1", &["**/*.sql"], &["rust"]);
        assert!(file_in_scope("a/b.sql", "", &scope_of(&rule, true)));
        assert!(file_in_scope("a/b.rs", "", &scope_of(&rule, true)));
        assert!(
            !file_in_scope("a/b.py", "", &scope_of(&rule, true)),
            "a file known to be another language, matching no glob, is out"
        );
    }

    #[test]
    fn a_file_with_an_unknown_language_is_sent_a_language_scoped_rule() {
        let rule = scoped("X-1", &[], &["rust"]);
        for unknown in ["Makefile", "README.md", "config/app.yaml", ".env", ""] {
            assert!(
                file_in_scope(unknown, "", &scope_of(&rule, true)),
                "`{unknown}` has no recognized language: the rule must be sent"
            );
        }
        assert!(!file_in_scope("ui/app.tsx", "", &scope_of(&rule, true)));
    }

    #[test]
    fn a_paths_only_scope_does_not_send_unmatched_unknown_files() {
        let rule = scoped("X-1", &["**/*.sql"], &[]);
        assert!(!file_in_scope("README.md", "", &scope_of(&rule, true)));
        assert!(!file_in_scope("Makefile", "", &scope_of(&rule, true)));
    }

    #[test]
    fn the_id_prefix_language_and_the_declared_scope_both_must_hold() {
        let rule = scoped("RUST-X-1", &["crates/**"], &[]);
        let s = scope_of(&rule, true);
        assert!(file_in_scope("crates/a/src/lib.rs", "", &s));
        assert!(!file_in_scope("crates/a/Cargo.toml", "", &s), "not Rust");
        assert!(
            !file_in_scope("tools/x.rs", "", &s),
            "outside the declared path"
        );
    }

    #[test]
    fn the_switch_off_ignores_the_declared_scope_entirely() {
        let rule = scoped("SEC-X-1", &["**/*.sql"], &[]);
        assert_eq!(scope_of(&rule, false), Scope::All);
        let files = vec![f("a.sql", 10), f("b.rs", 10)];
        assert_eq!(files_for_rule(&rule, false, &files).len(), 2);
        let rust = scoped("RUST-X-1", &["**/*.sql"], &[]);
        assert_eq!(scope_of(&rust, false), Scope::Language("rust"));
        let plan = plan_routes(&[rule, rust], &files, false);
        assert!(plan
            .groups
            .iter()
            .all(|g| !matches!(g.scope, Scope::Declared { .. })));
    }

    #[test]
    fn rules_sharing_a_declared_scope_share_a_group_whatever_the_authored_order() {
        let a = scoped("A-1", &["x/**", "y/**"], &[]);
        let b = scoped("B-1", &["y/**", "x/**"], &[]);
        let c = scoped("C-1", &["z/**"], &[]);
        let plan = plan_routes(&[a, b, c, r("ARCH-1")], &[f("x/a.rs", 1)], true);
        assert_eq!(plan.groups.len(), 3, "two declared groups + All");
        assert_eq!(plan.groups[0].rules.len(), 2);
        assert_eq!(plan.groups[2].scope, Scope::All, "All stays last");
    }

    #[test]
    fn a_declared_scope_cuts_the_routed_volume() {
        let files = vec![f("a.sql", 100), f("b.rs", 900)];
        let plan = plan_routes(&[scoped("SUPA-1", &["**/*.sql"], &[])], &files, true);
        assert_eq!(plan.full_chars, 1000);
        assert_eq!(plan.routed_chars, 100);
    }

    #[test]
    fn audit_rules_carry_the_corpus_declared_scope_and_default_to_none() {
        let mut set = camerata_rules::RuleSet::default();
        let mut declared = camerata_rules::bare_rule("SUPA-RLS-1", "supabase:rls");
        declared.applies_to =
            Some(AppliesTo::new(vec!["**/*.sql".into()], vec![], vec![]).unwrap());
        set.push(declared);
        set.push(camerata_rules::bare_rule("ARCH-1", "universal"));
        let pairs = vec![
            ("SUPA-RLS-1".to_string(), "d".to_string()),
            ("ARCH-1".to_string(), "d".to_string()),
            ("NOT-IN-CORPUS-1".to_string(), "d".to_string()),
        ];
        let rules = audit_rules(&pairs, Some(&set));
        assert!(rules[0].applies_to.is_some());
        assert!(rules[1].applies_to.is_none());
        assert!(
            rules[2].applies_to.is_none(),
            "an unknown rule gets no scope"
        );
        assert!(audit_rules(&pairs, None)
            .iter()
            .all(|r| r.applies_to.is_none()));
    }

    fn with_markers(id: &str, paths: &[&str], markers: &[&str]) -> AuditRule {
        AuditRule::new(id, "d").with_applies_to(Some(
            AppliesTo::new(
                paths.iter().map(|p| (*p).to_string()).collect(),
                vec![],
                markers.iter().map(|m| (*m).to_string()).collect(),
            )
            .unwrap(),
        ))
    }

    #[test]
    fn a_content_marker_sends_the_rule_with_a_file_whatever_its_path_or_language() {
        let rule = with_markers(
            "SQL-1",
            &["**/*.sql"],
            &["create policy", "security definer"],
        );
        let s = scope_of(&rule, true);
        // A TypeORM migration, a Rails migration and a Python RunSQL, none under a SQL path.
        assert!(file_in_scope(
            "src/data/setup.ts",
            r#"await queryRunner.query("CREATE POLICY p ON t USING (true)")"#,
            &s
        ));
        assert!(file_in_scope(
            "lib/boot.rb",
            "execute <<~SQL\n  create function f() returns int security definer\nSQL",
            &s
        ));
        assert!(file_in_scope("x/y.py", "RunSQL('Create Policy p ...')", &s));
        // The path alone still admits a file with no marker.
        assert!(file_in_scope("db/1.sql", "select 1;", &s));
        // Neither path nor marker: out.
        assert!(!file_in_scope("src/app.tsx", "export const x = 1;", &s));
    }

    #[test]
    fn a_marker_scope_alone_has_no_language_part_so_unmatched_unknown_files_are_out() {
        let rule = with_markers("SQL-1", &[], &["create policy"]);
        assert!(!file_in_scope("README.md", "hello", &scope_of(&rule, true)));
        assert!(file_in_scope(
            "README.md",
            "see CREATE POLICY",
            &scope_of(&rule, true)
        ));
    }

    #[test]
    fn content_markers_are_ignored_when_the_switch_is_off() {
        let rule = with_markers("SQL-1", &[], &["create policy"]);
        assert_eq!(scope_of(&rule, false), Scope::All);
    }

    #[test]
    fn module_and_razor_extensions_resolve_to_their_languages() {
        for (ext, lang) in [
            ("mjs", "web"),
            ("cjs", "web"),
            ("mts", "web"),
            ("cts", "web"),
            ("cshtml", "csharp"),
            ("razor", "csharp"),
        ] {
            assert_eq!(lang_for_extension(ext), Some(lang), "{ext}");
        }
        let react = r("REACT-1");
        assert!(file_in_scope("src/x.mjs", "", &scope_of(&react, true)));
        assert!(file_in_scope("src/x.cts", "", &scope_of(&react, true)));
        let cs = r("CSHARP-X-1");
        assert!(file_in_scope(
            "Views/Home/Index.cshtml",
            "",
            &scope_of(&cs, true)
        ));
    }

    // ── H10: language scopes that dropped files a rule needs ─────────────────────────────

    #[test]
    fn a_spring_rule_is_routed_to_java_and_kotlin_alike() {
        let spring = r("SPRING-NO-FIELD-INJECTION-1");
        let scope = scope_of(&spring, true);
        assert!(file_in_scope("src/A.java", "", &scope));
        assert!(file_in_scope("src/B.kt", "", &scope));
        assert!(file_in_scope(
            "build.gradle.kts",
            "",
            &scope_of(&r("KOTLIN-1"), true)
        ));
        assert!(!file_in_scope("src/c.py", "", &scope));
        // A plain Java rule stays Java-only.
        assert!(!file_in_scope(
            "src/B.kt",
            "",
            &scope_of(&r("JAVA-1"), true)
        ));
    }

    #[test]
    fn a_language_group_also_receives_the_manifests_and_application_config() {
        let files = vec![
            f("src/main.rs", 10),
            f("Cargo.toml", 10),
            f("web/package.json", 10),
            f("src/main/resources/application.yml", 10),
            f("README.md", 10),
        ];
        let rust = r("RUST-1");
        let picked: Vec<&str> = files_for_rule(&rust, true, &files)
            .into_iter()
            .map(|(p, _)| p.as_str())
            .collect();
        assert_eq!(
            picked,
            [
                "src/main.rs",
                "Cargo.toml",
                "web/package.json",
                "src/main/resources/application.yml"
            ]
        );
        let scope = scope_of(&rust, true);
        let group: Vec<String> = files_in_scope(&files, &scope)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(group.len(), 4, "{group:?}");
        let plan = plan_routes(&[rust], &files, true);
        assert_eq!(plan.routed_chars, 40);
    }

    #[test]
    fn a_language_group_with_no_source_file_stays_empty_even_with_a_manifest() {
        // A Rust rule in a TypeScript repo must still be "not assessed", not "ran, found nothing"
        // on a lone package.json.
        let files = vec![f("src/app.ts", 10), f("package.json", 10)];
        let scope = scope_of(&r("RUST-1"), true);
        assert!(files_in_scope(&files, &scope).is_empty());
        assert!(files_for_rule(&r("RUST-1"), true, &files).is_empty());
    }

    #[test]
    fn single_file_components_and_more_c_family_extensions_have_a_language() {
        for (ext, lang) in [
            ("vue", "web"),
            ("svelte", "web"),
            ("astro", "web"),
            ("kts", "kotlin"),
            ("cc", "cpp"),
            ("hpp", "cpp"),
            ("cxx", "cpp"),
        ] {
            assert_eq!(lang_for_extension(ext), Some(lang), "{ext}");
        }
    }
}
