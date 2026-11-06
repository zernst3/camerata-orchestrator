//! `NeedlessComplexityChecker`: the deterministic detector for `STYLE-NEEDLESS-COMPLEXITY-1`
//! ("no negated ternaries, no needless optional props"). The design, the traps and the safe
//! twins are in `docs/design/2026-10-21_w29-detector-implementation.md` ("Wave D and 22e: the
//! specs", STYLE-NEEDLESS-COMPLEXITY-1).
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/javascript/style-needless-complexity-1.toml` (`javascript` domain).
//!
//! # Two shapes
//!
//! 1. [`ternary`]: `!x ? a : b`, which is `x ? b : a` written backwards. Every production
//!    ECMAScript file is read; only the plainest one-line shape is reported.
//! 2. [`optional_props`] and [`call_sites`]: a TSX component prop declared optional that every
//!    place rendering the component passes with a value that cannot be `undefined`. Call sites
//!    are found through the re-export graph ARCH-ONE-IMPORT-PATH-1 builds
//!    ([`crate::one_import_path_checker::Model`]), and a component is skipped whenever any
//!    reference to it cannot be accounted for.
//!
//! # The law: false-negative, never false-positive
//!
//! A conditional that might read better negated (a guard-first JSX block over several lines, a
//! chain) is left alone, and so is any component one of whose callers the check cannot see: a
//! package with published entry points, a spread at a call site, a value use of the component,
//! a namespace or dynamic import, a computed load, or a file it does not parse that names it.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`](crate::arch_checker::SEVERITY_MEDIUM): the checker contract forbids
//! emitting `low`. The corpus rule declares `max_severity = "low"`, which the report export
//! clamps down to.

mod call_sites;
mod optional_props;
mod ternary;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_MEDIUM};
use crate::extract::clone::{classify_path, SkipReason};
use crate::extract::ecma_walk::parse_path;
use crate::extract::paths::emitted_js_paths;
use crate::extract::SourceLang;

use call_sites::{attributes, passes_defined_value, Repo, Site};

pub const RULE_NEEDLESS_COMPLEXITY: &str = "STYLE-NEEDLESS-COMPLEXITY-1";

const RULE_IDS: &[&str] = &[RULE_NEEDLESS_COMPLEXITY];

/// The ECMAScript sources, the manifests and `tsconfig.json` (the import graph resolves path
/// aliases from it), and the template files whose mention of a component makes its callers
/// unknowable.
const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.mjs",
    "**/*.cjs",
    "**/package.json",
    "tsconfig.json",
    "**/tsconfig.json",
    "**/*.mdx",
    "**/*.vue",
    "**/*.svelte",
    "**/*.astro",
];

/// How many call sites a finding names before saying "and N more".
const SITES_SHOWN: usize = 4;

pub struct NeedlessComplexityChecker;

impl ArchChecker for NeedlessComplexityChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let emitted = emitted_js_paths(repo.files);
        let mut out = Vec::new();
        // Production files are reported on; test and story files are read only as callers.
        let mut production: Vec<usize> = Vec::new();
        let mut readable = Vec::new();
        let mut unparsed_code: HashSet<&str> = HashSet::new();
        for (path, source) in repo.files {
            let (is_production, is_read) = match classify_path(path, &emitted) {
                Ok(SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript) => {
                    (true, true)
                }
                Err(SkipReason::TestPath) => (false, true),
                Ok(_) | Err(SkipReason::NotSource) => continue,
                Err(_) => (false, false),
            };
            let tree = if is_read {
                parse_path(path, source)
            } else {
                None
            };
            let Some(tree) = tree else {
                if crate::extract::ecma_walk::dialect_for(path).is_some() {
                    unparsed_code.insert(path.as_str());
                }
                continue;
            };
            if is_production {
                out.extend(ternary::check(path, source, &tree));
                production.push(readable.len());
            }
            readable.push((path.as_str(), source.as_str(), tree));
        }
        out.extend(needless_optional_props(
            repo.files,
            readable,
            &production,
            &unparsed_code,
        ));
        out.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.message.as_str()).cmp(&(
                b.file.as_str(),
                b.line,
                b.message.as_str(),
            ))
        });
        out
    }
}

fn needless_optional_props<'a>(
    files: &'a [(String, String)],
    readable: Vec<(&'a str, &'a str, tree_sitter::Tree)>,
    production: &[usize],
    unparsed_code: &HashSet<&str>,
) -> Vec<ArchViolation> {
    let has_candidates = production.iter().any(|&i| {
        let (path, source, tree) = &readable[i];
        path.ends_with(".tsx") && !optional_props::candidates(tree, source).is_empty()
    });
    if !has_candidates {
        return Vec::new();
    }
    let repo = Repo::new(files, readable, unparsed_code);
    let mut out = Vec::new();
    for &home in production {
        let file = &repo.parsed[home];
        if !file.path.ends_with(".tsx") {
            continue;
        }
        for candidate in optional_props::candidates(&file.tree, file.source) {
            let Some(sites) = repo.sites(home, &candidate) else {
                continue;
            };
            if let Some(v) = judge(&repo, home, &candidate, &sites) {
                out.push(v);
            }
        }
    }
    out
}

/// The members every one of at least two call sites passes with a defined value.
fn judge(
    repo: &Repo<'_>,
    home: usize,
    candidate: &optional_props::Candidate<'_>,
    sites: &[Site<'_>],
) -> Option<ArchViolation> {
    if sites.len() < 2 {
        return None;
    }
    let mut passed: BTreeMap<&str, usize> = BTreeMap::new();
    for site in sites {
        let file = &repo.parsed[site.source_index];
        let attrs = attributes(site.element, file.source)?;
        for member in &candidate.members {
            let defined = attrs.iter().any(|(name, attr)| {
                name == &member.name
                    && passes_defined_value(*attr, file.tree.root_node(), file.source)
            });
            if defined {
                *passed.entry(member.name.as_str()).or_default() += 1;
            }
        }
    }
    let needless: Vec<&optional_props::OptionalMember> = candidate
        .members
        .iter()
        .filter(|m| passed.get(m.name.as_str()) == Some(&sites.len()))
        .collect();
    let first = needless.first()?;

    let home_path = repo.parsed[home].path;
    let names: Vec<String> = needless.iter().map(|m| format!("`{}`", m.name)).collect();
    let mut places: Vec<String> = sites
        .iter()
        .map(|s| format!("{}:{}", s.file, s.line))
        .collect();
    places.sort();
    let shown = places.iter().take(SITES_SHOWN).cloned().collect::<Vec<_>>();
    let more = places.len() - shown.len();
    let mut listed = crate::state_shape_checker::join_words(&shown);
    if more > 0 {
        listed = format!("{}, and {more} more", shown.join(", "));
    }
    let (subject, verb, them) = if needless.len() == 1 {
        (names[0].clone(), "is", "it")
    } else {
        (
            crate::state_shape_checker::join_words(&names),
            "are",
            "them",
        )
    };
    let message = format!(
        "{subject} {verb} declared optional in the props of `{component}` ({home_path}, line \
         {line}), but every one of the {count} places that render `{component}` passes {them} \
         with a value: {listed}. The component still carries the code and the types for a \
         missing value that no caller produces. Declare {them} required and remove the fallback \
         for the missing case.",
        component = candidate.name,
        line = first.line,
        count = sites.len(),
    );
    Some(ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_NEEDLESS_COMPLEXITY.to_string(),
        file: home_path.to_string(),
        line: first.line,
        object: Some(candidate.name.clone()),
        message,
        // The corpus rule's `max_severity = "low"` clamps this at report time; the checker
        // contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    })
}
