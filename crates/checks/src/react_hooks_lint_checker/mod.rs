//! `ReactHooksLintChecker`: the deterministic detector for `UI-REACT-HOOKS-LINT-1` ("a React
//! project's own lint setup enables the React Hooks rules"). The design and the traps are in
//! `docs/design/2026-10-21_w29-detector-implementation.md` ("UI-REACT-HOOKS-LINT-1: the
//! algorithm and its traps").
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/javascript/react/ui-react-hooks-lint-1.toml` (`javascript:react`).
//!
//! # What this checker is, and what it is not
//!
//! It reads the repository's OWN lint configuration and reports one gap: a React project in
//! which neither `react-hooks/rules-of-hooks` nor `react-hooks/exhaustive-deps` is enabled. It
//! does not run ESLint. The violations those two rules report in code (a hook called in a
//! condition, an effect missing a dependency) come from the provisioned-eslint scan tier
//! (`camerata-server`'s `scan_tools`) and are reported under the existing
//! `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` and `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` ids; this rule
//! deliberately claims none of them, so a violation is never reported twice.
//!
//! # The law: false-negative, never false-positive
//!
//! "The configuration does not enable the rules" is only said when the whole of the repository's
//! lint setup was read and none of it enables them. So the answer is [`Verdict::Unknown`], and
//! nothing is reported, when any of these holds anywhere in the repository (outside example,
//! fixture, documentation and vendored folders): a flat config (`eslint.config.*`, JavaScript
//! that computes its configuration); a JavaScript or YAML legacy config; a JSON config that does
//! not parse, uses a key this reader does not know, or extends a preset it cannot see into; a
//! manifest that depends on a shareable ESLint config, the hooks plugin itself or another linter,
//! or whose scripts hand ESLint a config path of their own; another linter's config file; a
//! manifest that does not parse. The checker declares such a repository unanswered through
//! [`ArchChecker::config_unsatisfied_for`], so the scan ledger does not credit a clean run.
//!
//! # How it decides
//!
//! 1. Collect the manifests and lint configuration files outside auxiliary folders.
//! 2. No manifest declares `react` (dependencies, dev dependencies or peer dependencies): not a
//!    React repository, nothing to say.
//! 3. Evaluate every readable legacy config and every manifest `eslintConfig` field
//!    ([`config::evaluate_legacy_config`]). Any of them enabling a hooks rule answers the
//!    question: enabled.
//! 4. Otherwise any unreadable piece (above) makes the answer unknown.
//! 5. Otherwise every piece was read and none enables the rules, or there was no ESLint
//!    configuration at all: one finding, on the first configuration that was read, or on the
//!    manifest line that declares React when there was none.
//!
//! # Documented limits (all false-negative, by design)
//!
//! - A config that enables one of the two rules but not the other is not reported.
//! - A rule enabled in an override for a narrow set of files counts as enabled.
//! - Severity is not inspected: `warn` passes, though a warning does not fail a build.
//! - Whether CI runs lint is not inspected.
//! - A repository whose only configuration is a flat config (the current ESLint default) is
//!   never reported, because that file is JavaScript.
//!
//! # Not a write-time rule
//!
//! [`ArchChecker::runs_at_write_time`] is `false`. Closing the gap installs a dependency and
//! chooses its version, which the project's own rules send to a human; a bounced agent cannot
//! finish that, and a bounce on every React task would be about something the task did not
//! touch. Code that breaks the rules IS caught at write time when the repository's own lint
//! enables them: the Layer-2 JavaScript runner runs the repository's `npm run lint`. This
//! checker is the check that the precondition holds.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`]: the checker contract forbids emitting `low`. The corpus rule
//! declares `max_severity = "low"`, which the report export clamps down to.

mod config;

use serde_json::Value;

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};
use crate::extract::paths::is_test_path;

use config::Part;

pub const RULE_REACT_HOOKS_LINT: &str = "UI-REACT-HOOKS-LINT-1";

const RULE_IDS: &[&str] = &[RULE_REACT_HOOKS_LINT];

/// Every file shape the checker reads. The Layer-2 runner and the standalone CI binary load only
/// the files matching these globs, so a configuration shape missing here is invisible there and a
/// repository would read as having none.
const INTEREST_GLOBS: &[&str] = &[
    "**/package.json",
    "**/eslint.config.*",
    "**/.eslintrc*",
    "**/biome.json",
    "**/biome.jsonc",
    "**/.oxlintrc.json",
    "**/oxlint.json",
    "**/rome.json",
];

/// Folder names whose manifests and lint configuration describe something other than the
/// project: examples, documentation, scaffolding templates, vendored and installed code.
/// Test and fixture folders are covered by [`is_test_path`].
const AUXILIARY_DIRS: &[&str] = &[
    "examples",
    "example",
    "docs",
    "doc",
    "demo",
    "demos",
    "sample",
    "samples",
    "playground",
    "playgrounds",
    "templates",
    "template",
    "sandbox",
    "vendor",
    "third_party",
    "node_modules",
    "dist",
];

fn is_auxiliary(path: &str) -> bool {
    if is_test_path(path) {
        return true;
    }
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    segments.iter().any(|s| AUXILIARY_DIRS.contains(s))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 1-based line of the first line that opens the JSON key `key`, or 1 when it is not found.
fn line_of_key(content: &str, key: &str) -> usize {
    let needle = format!("\"{key}\"");
    content
        .lines()
        .position(|line| {
            line.trim_start()
                .strip_prefix(&needle)
                .is_some_and(|rest| rest.trim_start().starts_with(':'))
        })
        .map_or(1, |i| i + 1)
}

/// A piece of configuration that was read in full and does not enable the rules.
struct Examined {
    file: String,
    line: usize,
}

enum Verdict {
    /// Not a React repository, or one the checker cannot answer for.
    Unanswered,
    /// A configuration enables a hooks rule.
    Enabled,
    /// React is declared and the lint setup, read in full, enables neither rule.
    Gap {
        /// The shallowest manifest that declares React, and the line that declares it.
        react_manifest: String,
        react_line: usize,
        /// The configuration read and found not to enable the rules; empty when there is none.
        examined: Vec<Examined>,
    },
}

fn analyze(files: &[(String, String)]) -> Verdict {
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut react_manifests: Vec<(&str, usize)> = Vec::new();
    let mut examined: Vec<Examined> = Vec::new();
    let mut enabled = false;
    let mut unreadable = false;

    for (path, content) in sorted {
        if is_auxiliary(path) {
            continue;
        }
        let name = basename(path);
        if name == "package.json" {
            let text = content.strip_prefix('\u{feff}').unwrap_or(content);
            let Ok(package) = serde_json::from_str::<Value>(text) else {
                unreadable = true;
                continue;
            };
            let facts = config::package_facts(&package);
            if facts.declares_react {
                react_manifests.push((path, line_of_key(content, "react")));
            }
            unreadable |= facts.hides_lint_setup;
            if let Some(field) = package.get("eslintConfig") {
                match config::evaluate_legacy_config(field) {
                    Part::Enables => enabled = true,
                    Part::Unknown => unreadable = true,
                    Part::DoesNot => examined.push(Examined {
                        file: path.clone(),
                        line: line_of_key(content, "eslintConfig"),
                    }),
                }
            }
        } else if name == ".eslintrc" || name == ".eslintrc.json" || name == ".eslintrc.jsonc" {
            match config::parse_lenient_json(content)
                .map_or(Part::Unknown, |v| config::evaluate_legacy_config(&v))
            {
                Part::Enables => enabled = true,
                Part::Unknown => unreadable = true,
                Part::DoesNot => examined.push(Examined {
                    file: path.clone(),
                    line: 1,
                }),
            }
        } else if name.starts_with("eslint.config.")
            || name.starts_with(".eslintrc.")
            || matches!(
                name,
                "biome.json" | "biome.jsonc" | ".oxlintrc.json" | "oxlint.json" | "rome.json"
            )
        {
            // Computed (JavaScript), not decidable (YAML) or another linter's: unreadable here.
            unreadable = true;
        }
    }

    // The shallowest manifest declares the project; ties break on path order (already sorted).
    react_manifests.sort_by_key(|(path, _)| path.matches('/').count());
    let Some(&(react_manifest, react_line)) = react_manifests.first() else {
        return Verdict::Unanswered;
    };
    if enabled {
        return Verdict::Enabled;
    }
    if unreadable {
        return Verdict::Unanswered;
    }
    Verdict::Gap {
        react_manifest: react_manifest.to_string(),
        react_line,
        examined,
    }
}

fn gap_message(react_manifest: &str, examined: &[Examined]) -> String {
    let cause = if examined.is_empty() {
        format!(
            "This project uses React (`react` is a dependency in `{react_manifest}`), but no \
             ESLint configuration in the repository enables the React Hooks rules."
        )
    } else {
        let names: Vec<String> = examined.iter().map(|e| format!("`{}`", e.file)).collect();
        let (subject, verb) = if names.len() == 1 {
            ("The ESLint configuration in", "does")
        } else {
            ("The ESLint configurations in", "do")
        };
        format!(
            "{subject} {} {verb} not enable the React Hooks rules `react-hooks/rules-of-hooks` \
             and `react-hooks/exhaustive-deps` (React is a dependency in `{react_manifest}`).",
            names.join(", ")
        )
    };
    format!(
        "{cause} A hook called inside a condition or a loop, or an effect, callback or memo that \
         reads a value it leaves out of its dependency array, then passes review and the build \
         and fails only when the app runs, as a crash or as stale data. Install \
         `eslint-plugin-react-hooks`, enable `react-hooks/rules-of-hooks` and \
         `react-hooks/exhaustive-deps` at error severity in the project's ESLint configuration, \
         and run lint in CI so that a violation fails the build."
    )
}

pub struct ReactHooksLintChecker;

impl ArchChecker for ReactHooksLintChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let Verdict::Gap {
            react_manifest,
            react_line,
            examined,
        } = analyze(repo.files)
        else {
            return Vec::new();
        };
        let (file, line) = match examined.first() {
            Some(first) => (first.file.clone(), first.line),
            None => (react_manifest.clone(), react_line),
        };
        vec![ArchViolation {
            rule_id: RULE_REACT_HOOKS_LINT.to_string(),
            file,
            line,
            object: None,
            message: gap_message(&react_manifest, &examined),
            severity: SEVERITY_MEDIUM,
        }]
    }

    /// A scan and CI rule only. Closing the gap installs a dependency and picks its version, a
    /// decision the project's rules route to a human, so a write-time bounce could not be
    /// satisfied by the agent it bounced.
    fn runs_at_write_time(&self) -> bool {
        false
    }

    /// Unanswered (not a React repository, or a lint setup this checker cannot read) stays with
    /// the advisory tier for this repository; only an enabled or a gap verdict is a real answer
    /// the scan ledger may credit.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        matches!(analyze(repo.files), Verdict::Unanswered)
    }
}

#[cfg(test)]
mod tests;
