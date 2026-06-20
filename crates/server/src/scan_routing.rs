//! Rule-routing: send each rule only the files it could possibly apply to.
//!
//! The AI audit's dominant cost is re-sending the codebase once per rule-batch (see
//! `estimate_audit_cost`). Many rules are **language-specific** — a `RUST-*` convention can't be
//! violated by a `.ts` file, a `REACT-*` rule can't fire on a `.rs` file — so auditing those rules
//! against the whole tree pays to read code the rule provably cannot match. Routing each
//! language-scoped rule to only its language's files collapses that waste, which is the big lever
//! on a **polyglot** repo (e.g. a Rust backend + a TypeScript frontend).
//!
//! ## Safety first: routing must never cause a MISSED finding
//!
//! Excluding a file from a rule is only sound when the rule provably cannot apply to it. This
//! module is therefore **conservative**: it routes ONLY rules whose id carries a recognized
//! single-language prefix (`RUST-`, `PY-`, `REACT-`, …). Cross-cutting families — architectural
//! (`ARCH-`), security (`SEC-`), SQL/DB (`SQL-`, `DB-`), API/process (`API-`, `PROC-`, `ORCH-`) —
//! are language-agnostic (a raw-SQL or layering rule can live in any language's files), so they get
//! [`Scope::All`] and audit every file. When in doubt, a rule audits everything.
//!
//! This module is the PURE core (classification + grouping + a savings estimate). Wiring it into
//! the audit pass loop interacts with the advisory "flag novel issues" pass (which is gated to one
//! pass per file chunk and must not be re-run per language group), so that wiring is tracked as a
//! reviewed change — see `docs/decisions/2026-06-19_rule_routing.md`.

use std::collections::BTreeMap;

/// Which files a rule should be audited against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// A recognized single programming language — audit only that language's files.
    Language(&'static str),
    /// Cross-cutting / unknown — audit every file (the safe default; never causes a miss).
    All,
}

/// Map a file extension (lowercase, no dot) to its canonical language key, if known.
pub fn lang_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" | "js" | "jsx" => "web", // the JS/TS/React ecosystem shares frontend rules
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "swift" => "swift",
        "c" | "h" | "cpp" => "cpp",
        _ => return None, // toml/json/yaml/sql/sh/etc. — config/data, not a routed language
    })
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
    ("SPRING", "java"),
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
/// process, or unknown) is [`Scope::All`] so it audits every file.
pub fn rule_scope(rule_id: &str) -> Scope {
    let head = rule_id
        .split(|c: char| c == '-' || c == '_' || c == ':')
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

/// Whether a file is in a rule's scope. [`Scope::All`] matches everything; a language scope
/// matches only files of that language. Files of an UNKNOWN language (config/data — toml, json,
/// sql, sh) are matched by [`Scope::All`] but NOT by any language scope (a `RUST-` rule has no
/// business in a `.sql` file).
pub fn file_in_scope(path: &str, scope: &Scope) -> bool {
    match scope {
        Scope::All => true,
        Scope::Language(lang) => file_language(path) == Some(lang),
    }
}

/// The files a single rule should be audited against (a view into `files`).
pub fn files_for_rule<'a>(
    rule_id: &str,
    files: &'a [(String, String)],
) -> Vec<&'a (String, String)> {
    let scope = rule_scope(rule_id);
    files.iter().filter(|(p, _)| file_in_scope(p, &scope)).collect()
}

/// A group of rules that share one scope (so they share one file subset), ready to feed the audit
/// engine as a unit.
#[derive(Debug, Clone)]
pub struct RouteGroup {
    pub scope: Scope,
    /// The rules in this group: `(id, directive)`.
    pub rules: Vec<(String, String)>,
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
/// order: languages alphabetically, then `All` last) and computes the chars-audited estimate.
pub fn plan_routes(selected: &[(String, String)], files: &[(String, String)]) -> RoutePlan {
    let total_chars: u64 = files.iter().map(|(_, c)| c.len() as u64).sum();
    // chars per language (for the routed estimate) + the full set.
    let mut lang_chars: BTreeMap<&'static str, u64> = BTreeMap::new();
    for (p, c) in files {
        if let Some(lang) = file_language(p) {
            *lang_chars.entry(lang).or_default() += c.len() as u64;
        }
    }

    let mut by_scope: BTreeMap<String, RouteGroup> = BTreeMap::new();
    let mut routed_chars = 0u64;
    let full_chars = total_chars.saturating_mul(selected.len() as u64);

    for (id, directive) in selected {
        let scope = rule_scope(id);
        routed_chars += match &scope {
            Scope::All => total_chars,
            Scope::Language(lang) => lang_chars.get(lang).copied().unwrap_or(0),
        };
        // Group key: "0:<lang>" for languages (sort first, alphabetically), "1" for All (last).
        let key = match &scope {
            Scope::Language(l) => format!("0:{l}"),
            Scope::All => "1".to_string(),
        };
        by_scope
            .entry(key)
            .or_insert_with(|| RouteGroup { scope: scope.clone(), rules: Vec::new() })
            .rules
            .push((id.clone(), directive.clone()));
    }

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

    #[test]
    fn language_prefixed_rules_get_a_language_scope() {
        assert_eq!(rule_scope("RUST-DIOXUS-2"), Scope::Language("rust"));
        assert_eq!(rule_scope("DIOXUS-NO-CLONE-1"), Scope::Language("rust"));
        assert_eq!(rule_scope("PY-NO-MUTABLE-DEFAULT-1"), Scope::Language("python"));
        assert_eq!(rule_scope("REACT-HOOKS-1"), Scope::Language("web"));
        assert_eq!(rule_scope("TS-STRICT-1"), Scope::Language("web"));
        assert_eq!(rule_scope("GO-ERRCHECK-1"), Scope::Language("go"));
    }

    #[test]
    fn cross_cutting_rules_audit_every_file() {
        // The families that can live in ANY language must never be routed away from a file.
        for id in ["ARCH-STRICT-LAYERING-1", "SEC-NO-RAW-SQL-CONCAT-1", "SQL-DB-INDEX-2", "API-IDEMPOTENT-1", "PROC-CITE-CONVENTION-1", "WHATEVER-NEW-1"] {
            assert_eq!(rule_scope(id), Scope::All, "{id} must audit all files");
        }
    }

    #[test]
    fn file_in_scope_respects_language_and_all() {
        assert!(file_in_scope("src/main.rs", &Scope::Language("rust")));
        assert!(!file_in_scope("src/app.tsx", &Scope::Language("rust")));
        assert!(!file_in_scope("schema.sql", &Scope::Language("rust")));
        // All matches everything, including config/data files a language scope would skip.
        assert!(file_in_scope("schema.sql", &Scope::All));
        assert!(file_in_scope("src/main.rs", &Scope::All));
    }

    #[test]
    fn files_for_rule_filters_a_rust_rule_to_rust_files() {
        let files = vec![f("a.rs", 10), f("b.tsx", 10), f("c.sql", 10)];
        let routed = files_for_rule("RUST-1", &files);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].0, "a.rs");
        // A cross-cutting rule keeps everything.
        assert_eq!(files_for_rule("ARCH-1", &files).len(), 3);
    }

    #[test]
    fn plan_groups_by_scope_and_estimates_savings_on_polyglot() {
        // 1000 chars of Rust, 1000 of web, 200 of sql config.
        let files = vec![f("a.rs", 1000), f("b.tsx", 1000), f("c.sql", 200)];
        // A rust rule, a web rule, and a cross-cutting arch rule.
        let rules = vec![
            ("RUST-1".to_string(), "d".to_string()),
            ("REACT-1".to_string(), "d".to_string()),
            ("ARCH-1".to_string(), "d".to_string()),
        ];
        let plan = plan_routes(&rules, &files);

        // Full (naive): 3 rules × 2200 chars = 6600.
        assert_eq!(plan.full_chars, 6600);
        // Routed: RUST-1 sees 1000 (rs), REACT-1 sees 1000 (tsx), ARCH-1 sees 2200 (all) = 4200.
        assert_eq!(plan.routed_chars, 4200);
        assert!((plan.saved_fraction() - (1.0 - 4200.0 / 6600.0)).abs() < 1e-9);
        assert!(plan.saved_fraction() > 0.0, "routing saves input on a polyglot repo");

        // Groups: rust, web, and All (3 distinct scopes).
        assert_eq!(plan.groups.len(), 3);
    }

    #[test]
    fn no_savings_for_single_language_repo_with_cross_cutting_rules() {
        // A pure-Python repo audited by cross-cutting rules: routing changes nothing (safe no-op).
        let files = vec![f("a.py", 500), f("b.py", 500)];
        let rules = vec![("ARCH-1".to_string(), "d".to_string()), ("SEC-1".to_string(), "d".to_string())];
        let plan = plan_routes(&rules, &files);
        assert_eq!(plan.routed_chars, plan.full_chars, "no language pruning possible");
        assert_eq!(plan.saved_fraction(), 0.0);
    }

    #[test]
    fn python_rule_in_polyglot_skips_non_python() {
        // A Python rule in a repo that also has Rust: the Rust files are pruned for it.
        let files = vec![f("a.py", 300), f("b.rs", 700)];
        let rules = vec![("PY-1".to_string(), "d".to_string())];
        let plan = plan_routes(&rules, &files);
        assert_eq!(plan.routed_chars, 300, "the PY rule audits only the .py file");
        assert_eq!(plan.full_chars, 1000);
    }
}
