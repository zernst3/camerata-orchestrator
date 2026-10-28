//! Reading ESLint configuration without running it.
//!
//! Everything here answers one question with three possible answers, and the third is the
//! important one: does this piece of configuration ENABLE the React Hooks rules, definitely NOT
//! enable them, or is it something this module cannot decide? Only a configuration that is plain
//! JSON, uses nothing but keys and presets this module knows, can be [`Part::DoesNot`]. Anything
//! else is [`Part::Unknown`], and the checker reports nothing for a repository with an unknown
//! part. A false negative costs a missed finding; a false positive tells a team its lint setup is
//! broken when it is not.

use serde_json::Value;

/// What one piece of configuration says about the two React Hooks rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    /// At least one of `react-hooks/rules-of-hooks` and `react-hooks/exhaustive-deps` is on.
    Enables,
    /// Read in full, and neither rule is on.
    DoesNot,
    /// Not decidable here: computed, not parseable, or built from a preset we cannot see into.
    Unknown,
}

impl Part {
    /// The combination of two parts of ONE configuration: enabling anywhere enables; otherwise
    /// one unreadable part makes the whole unreadable; otherwise it does not enable.
    fn combine(self, other: Part) -> Part {
        match (self, other) {
            (Part::Enables, _) | (_, Part::Enables) => Part::Enables,
            (Part::Unknown, _) | (_, Part::Unknown) => Part::Unknown,
            (Part::DoesNot, Part::DoesNot) => Part::DoesNot,
        }
    }
}

const HOOKS_RULES: [&str; 2] = ["react-hooks/rules-of-hooks", "react-hooks/exhaustive-deps"];

/// `extends` entries known to bundle the hooks rules.
fn extend_enables(entry: &str) -> bool {
    entry.starts_with("plugin:react-hooks/")
        // eslint-config-react-app (Create React App), including its `react-app/jest` addition.
        || entry == "react-app"
        || entry.starts_with("react-app/")
        // eslint-config-next, including `next/core-web-vitals` and `next/typescript`.
        || entry == "next"
        || entry.starts_with("next/")
        // eslint-config-airbnb's hooks add-on.
        || entry == "airbnb/hooks"
}

/// `extends` entries known NOT to contain the hooks rules. Deliberately short: an entry on this
/// list is the only thing that lets a repository be reported, so it must be certain.
fn extend_is_known_without_hooks(entry: &str) -> bool {
    matches!(
        entry,
        "eslint:recommended" | "eslint:all" | "prettier" | "plugin:prettier/recommended"
    ) || entry.starts_with("prettier/")
        || entry.starts_with("plugin:react/")
        || entry.starts_with("plugin:@typescript-eslint/")
        || entry.starts_with("plugin:jsx-a11y/")
        || entry.starts_with("plugin:import/")
}

fn extends_part(value: &Value) -> Part {
    let entries: Vec<&Value> = match value {
        Value::String(_) => vec![value],
        Value::Array(items) => items.iter().collect(),
        _ => return Part::Unknown,
    };
    entries.into_iter().fold(Part::DoesNot, |acc, entry| {
        let part = match entry.as_str() {
            Some(s) if extend_enables(s) => Part::Enables,
            Some(s) if extend_is_known_without_hooks(s) => Part::DoesNot,
            // A shareable config, a relative path, anything else: cannot be seen into here.
            _ => Part::Unknown,
        };
        acc.combine(part)
    })
}

/// Whether a `rules` value switches a rule on: `Some(true)` on, `Some(false)` off, `None` for a
/// shape this module does not understand.
fn level_is_on(value: &Value) -> Option<bool> {
    match value {
        Value::String(s) => match s.as_str() {
            "off" => Some(false),
            "warn" | "error" => Some(true),
            _ => None,
        },
        Value::Number(n) => match n.as_u64() {
            Some(0) => Some(false),
            Some(1 | 2) => Some(true),
            _ => None,
        },
        Value::Array(items) => items.first().and_then(level_is_on),
        _ => None,
    }
}

fn rules_part(value: &Value) -> Part {
    let Value::Object(rules) = value else {
        return Part::Unknown;
    };
    let mut part = Part::DoesNot;
    for key in HOOKS_RULES {
        let Some(level) = rules.get(key) else {
            continue;
        };
        part = part.combine(match level_is_on(level) {
            Some(true) => Part::Enables,
            Some(false) => Part::DoesNot,
            None => Part::Unknown,
        });
    }
    part
}

/// Keys of a legacy (`.eslintrc`) configuration object that cannot switch the hooks rules on by
/// themselves. A key outside this list and outside `extends`, `rules` and `overrides` makes the
/// object unreadable: it may not be an ESLint configuration at all.
const INERT_KEYS: &[&str] = &[
    "$schema",
    "env",
    "excludedFiles",
    "files",
    "globals",
    "ignorePatterns",
    "noInlineConfig",
    "parser",
    "parserOptions",
    "plugins",
    "processor",
    "reportUnusedDisableDirectives",
    "root",
    "settings",
];

/// Evaluate a parsed legacy configuration object (a `.eslintrc.json` body or a `package.json`
/// `eslintConfig` field), `overrides` included.
pub(super) fn evaluate_legacy_config(config: &Value) -> Part {
    let Value::Object(map) = config else {
        return Part::Unknown;
    };
    let mut part = Part::DoesNot;
    for (key, value) in map {
        let next = match key.as_str() {
            "extends" => extends_part(value),
            "rules" => rules_part(value),
            "overrides" => match value {
                Value::Array(items) => items.iter().fold(Part::DoesNot, |acc, item| {
                    acc.combine(evaluate_legacy_config(item))
                }),
                _ => Part::Unknown,
            },
            other if INERT_KEYS.contains(&other) => Part::DoesNot,
            _ => Part::Unknown,
        };
        part = part.combine(next);
    }
    part
}

/// Parse JSON that may carry `//` and `/* */` comments and trailing commas (ESLint reads
/// `.eslintrc` and `.eslintrc.json` leniently). `None` for anything that is still not JSON.
pub(super) fn parse_lenient_json(text: &str) -> Option<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str(&clean_lenient_json(text)).ok()
}

/// Remove comments and trailing commas outside string literals.
fn clean_lenient_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if prev == '*' && next == '/' {
                        break;
                    }
                    if next == '\n' {
                        out.push('\n');
                    }
                    prev = next;
                }
                // A comment separates tokens the way whitespace does.
                out.push(' ');
            }
            '}' | ']' => {
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// What a `package.json` says that matters here.
#[derive(Debug, Default)]
pub(super) struct PackageFacts {
    /// `react` is a dependency, dev dependency or peer dependency.
    pub declares_react: bool,
    /// The manifest points at lint setup this module cannot see into (a shareable ESLint config,
    /// the hooks plugin itself, another linter, or a lint script with its own config path).
    pub hides_lint_setup: bool,
}

const DEPENDENCY_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// Linters that replace ESLint, with their own hooks rules and their own configuration.
const OTHER_LINTERS: &[&str] = &[
    "@biomejs/biome",
    "biome",
    "oxlint",
    "rome",
    "xo",
    "standard",
    "semistandard",
    "ts-standard",
    "neostandard",
    "deno",
];

/// Frameworks that run ESLint with `eslint-config-react-app` themselves when the project has no
/// configuration of its own (Gatsby), or did in older majors (Create React App 3.x through
/// `react-scripts`). A project on one of them has lint setup the repository's files do not show.
const FRAMEWORKS_WITH_BUILT_IN_LINT: &[&str] = &["gatsby", "react-scripts"];

fn dependency_hides_lint_setup(name: &str) -> bool {
    if name == "eslint-plugin-react-hooks"
        || OTHER_LINTERS.contains(&name)
        || FRAMEWORKS_WITH_BUILT_IN_LINT.contains(&name)
    {
        return true;
    }
    // `eslint-config-prettier` only turns rules off, so it cannot be the thing enabling them.
    if let Some(rest) = name.strip_prefix("eslint-config-") {
        return rest != "prettier";
    }
    // `@scope/eslint-config` and `@scope/eslint-config-react`.
    if let Some((scope, rest)) = name.split_once('/') {
        return scope.starts_with('@')
            && (rest == "eslint-config" || rest.starts_with("eslint-config-"));
    }
    false
}

/// Whether a command line hands ESLint a configuration (or rules, plugins, a rules folder or a
/// plugin resolution root) that no file in the repository shows.
fn command_gives_eslint_its_own_setup(command: &str) -> bool {
    let s = command.to_ascii_lowercase();
    s.contains("eslint")
        && [
            " -c ",
            "--config",
            "--no-eslintrc",
            "--rule",
            "--plugin",
            "--rulesdir",
            "--resolve-plugins-relative-to",
        ]
        .iter()
        .any(|flag| s.contains(flag))
}

/// Whether a `scripts` entry replaces ESLint outright with another linter, or gives ESLint its
/// own setup.
fn script_hides_lint_setup(script: &str) -> bool {
    let s = script.to_ascii_lowercase();
    command_gives_eslint_its_own_setup(&s)
        || s.contains("biome")
        || s.contains("oxlint")
        || s.contains("rome ")
}

/// Whether `test` holds for any string anywhere in `value` (object keys excluded).
fn any_string(value: &Value, test: &dyn Fn(&str) -> bool) -> bool {
    match value {
        Value::String(s) => test(s),
        Value::Array(items) => items.iter().any(|v| any_string(v, test)),
        Value::Object(map) => map.values().any(|v| any_string(v, test)),
        _ => false,
    }
}

pub(super) fn package_facts(package: &Value) -> PackageFacts {
    let mut facts = PackageFacts::default();
    for section in DEPENDENCY_SECTIONS {
        let Some(Value::Object(deps)) = package.get(section) else {
            continue;
        };
        if section != "optionalDependencies" && deps.contains_key("react") {
            facts.declares_react = true;
        }
        if deps.keys().any(|name| dependency_hides_lint_setup(name)) {
            facts.hides_lint_setup = true;
        }
    }
    if let Some(Value::Object(scripts)) = package.get("scripts") {
        if scripts
            .values()
            .filter_map(Value::as_str)
            .any(script_hides_lint_setup)
        {
            facts.hides_lint_setup = true;
        }
    }
    // ESLint is also run from `lint-staged`, husky, `nano-staged`, `simple-git-hooks` and other
    // tool sections, with the same flags. Any string in the manifest that runs ESLint with a
    // setup of its own hides the setup.
    if any_string(package, &command_gives_eslint_its_own_setup) {
        facts.hides_lint_setup = true;
    }
    facts
}
