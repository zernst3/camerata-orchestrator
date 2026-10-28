//! Tests for `UI-REACT-HOOKS-LINT-1`. Every positive (a React repo whose own lint setup does not
//! enable the hooks rules) has safe twins: a config that enables them by each recognised route,
//! a config the checker cannot read, and a repo that is not a React repo. The law is
//! false-negative, never false-positive, so the silent cases outnumber the reporting ones.

use super::*;
use crate::arch_checker::{checker_applies, SEVERITY_MEDIUM};

/// Exactly the files given.
fn raw_files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
    pairs
        .into_iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect()
}

/// The files given, plus one React component unless the repo already has a `.jsx` / `.tsx`
/// file: the rule only speaks about a repository that has React CODE (see the tests under
/// "React code" for the repositories that do not).
fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
    let has_component = pairs
        .iter()
        .any(|(p, _)| p.ends_with(".jsx") || p.ends_with(".tsx"));
    let mut all = raw_files(pairs);
    if !has_component {
        all.push((
            "src/App.jsx".to_string(),
            "export const App = () => null;\n".to_string(),
        ));
    }
    all
}

fn run(f: &[(String, String)]) -> Vec<ArchViolation> {
    ReactHooksLintChecker.check(&RepoView {
        spec: "test/repo",
        files: f,
    })
}

fn unsatisfied(f: &[(String, String)]) -> bool {
    ReactHooksLintChecker.config_unsatisfied_for(&RepoView {
        spec: "test/repo",
        files: f,
    })
}

fn assert_silent(f: &[(String, String)]) {
    let vs = run(f);
    assert!(vs.is_empty(), "expected no findings: {vs:#?}");
}

const REACT_PKG: &str = r#"{
  "name": "app",
  "dependencies": {
    "react": "^18.2.0",
    "react-dom": "^18.2.0"
  }
}"#;

/// A React `package.json` with extra top-level members spliced in (a JSON fragment such as
/// `"devDependencies": {...}`).
fn react_pkg_with(extra: &str) -> String {
    format!(
        "{{\n  \"name\": \"app\",\n  \"dependencies\": {{ \"react\": \"^18.2.0\" }},\n  {extra}\n}}"
    )
}

/// The one finding a gap produces.
fn the_finding(f: &[(String, String)]) -> ArchViolation {
    let mut vs = run(f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    vs.remove(0)
}

// ── positives: the lint setup does not enable the hooks rules ───────────────────────────────

#[test]
fn a_react_repo_with_no_eslint_configuration_at_all_is_flagged() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        ("src/App.jsx", "export {};\n"),
    ]);
    let v = the_finding(&f);
    assert_eq!(v.rule_id, RULE_REACT_HOOKS_LINT);
    assert_eq!(v.severity, SEVERITY_MEDIUM);
    assert_eq!(v.file, "package.json");
    // Anchored on the dependency line, not the top of the file.
    assert_eq!(v.line, 4);
    assert!(
        v.message.contains("no ESLint configuration"),
        "{}",
        v.message
    );
    for needle in [
        "react-hooks/rules-of-hooks",
        "react-hooks/exhaustive-deps",
        "eslint-plugin-react-hooks",
        "`package.json`",
    ] {
        assert!(v.message.contains(needle), "{needle}: {}", v.message);
    }
}

#[test]
fn a_legacy_json_config_without_the_hooks_rules_is_flagged_and_named() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            ".eslintrc.json",
            r#"{ "extends": ["eslint:recommended", "plugin:react/recommended"], "rules": { "no-console": "warn" } }"#,
        ),
    ]);
    let v = the_finding(&f);
    assert_eq!(v.file, ".eslintrc.json");
    assert_eq!(v.line, 1);
    assert!(v.message.contains("`.eslintrc.json`"), "{}", v.message);
    assert!(
        !v.message.contains("no ESLint configuration"),
        "{}",
        v.message
    );
    assert!(v.message.contains("does not enable"), "{}", v.message);
}

#[test]
fn a_config_with_no_extends_and_unrelated_rules_is_flagged() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            ".eslintrc.json",
            r#"{ "root": true, "rules": { "eqeqeq": "error" } }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc.json");
}

#[test]
fn an_extensionless_eslintrc_holding_json_is_read() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (".eslintrc", r#"{ "extends": "eslint:recommended" }"#),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc");
}

#[test]
fn comments_and_trailing_commas_in_a_json_config_do_not_hide_the_gap() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            ".eslintrc.json",
            "{\n  // legacy config\n  \"extends\": [\"eslint:recommended\", /* base */ \"prettier\",],\n  \"rules\": { \"no-console\": \"warn\", },\n}\n",
        ),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc.json");
}

#[test]
fn an_eslint_config_field_in_package_json_without_the_rules_is_flagged_on_that_field() {
    let pkg = react_pkg_with(r#""eslintConfig": { "extends": ["eslint:recommended"] }"#);
    let f = files(vec![("package.json", &pkg)]);
    let v = the_finding(&f);
    assert_eq!(v.file, "package.json");
    assert_eq!(v.line, 4, "the line of the eslintConfig field");
    assert!(v.message.contains("does not enable"), "{}", v.message);
}

#[test]
fn a_registered_plugin_with_no_rules_enabled_is_still_a_gap() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            ".eslintrc.json",
            r#"{ "plugins": ["react-hooks"], "rules": { "no-console": "warn" } }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc.json");
}

#[test]
fn both_rules_switched_off_is_a_gap() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            ".eslintrc.json",
            r#"{ "rules": { "react-hooks/rules-of-hooks": "off", "react-hooks/exhaustive-deps": 0 } }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc.json");
}

#[test]
fn overrides_that_do_not_enable_the_rules_do_not_change_the_verdict() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            ".eslintrc.json",
            r#"{ "extends": "eslint:recommended", "overrides": [ { "files": ["*.ts"], "extends": ["plugin:@typescript-eslint/recommended"] } ] }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc.json");
}

#[test]
fn react_in_dev_or_peer_dependencies_makes_the_repo_a_react_repo() {
    for section in ["devDependencies", "peerDependencies"] {
        let pkg = format!(r#"{{ "name": "lib", "{section}": {{ "react": "^18" }} }}"#);
        let f = files(vec![("package.json", &pkg)]);
        assert_eq!(the_finding(&f).file, "package.json", "{section}");
    }
}

#[test]
fn a_monorepo_package_with_react_and_no_config_anywhere_is_flagged_on_its_manifest() {
    let f = files(vec![
        (
            "package.json",
            r#"{ "name": "root", "private": true, "workspaces": ["apps/*"] }"#,
        ),
        ("apps/web/package.json", REACT_PKG),
        (
            "apps/api/package.json",
            r#"{ "name": "api", "dependencies": { "express": "4" } }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, "apps/web/package.json");
}

#[test]
fn several_configs_none_enabling_still_produce_exactly_one_finding_naming_each() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (".eslintrc.json", r#"{ "extends": "eslint:recommended" }"#),
        ("apps/web/.eslintrc.json", r#"{ "extends": "prettier" }"#),
    ]);
    let v = the_finding(&f);
    assert_eq!(v.file, ".eslintrc.json");
    assert!(v.message.contains("`.eslintrc.json`"), "{}", v.message);
    assert!(
        v.message.contains("`apps/web/.eslintrc.json`"),
        "{}",
        v.message
    );
}

#[test]
fn a_byte_order_mark_on_package_json_is_tolerated() {
    let pkg = format!("\u{feff}{REACT_PKG}");
    let f = files(vec![("package.json", &pkg)]);
    assert_eq!(the_finding(&f).file, "package.json");
}

#[test]
fn prettier_config_alone_does_not_hide_a_gap() {
    // `eslint-config-prettier` only switches rules off; it cannot enable the hooks rules, so it
    // is the one shareable config that does not silence the check.
    let pkg =
        react_pkg_with(r#""devDependencies": { "eslint": "^8", "eslint-config-prettier": "^9" }"#);
    let f = files(vec![
        ("package.json", &pkg),
        (
            ".eslintrc.json",
            r#"{ "extends": ["eslint:recommended", "prettier"] }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, ".eslintrc.json");
}

#[test]
fn an_example_app_config_does_not_stand_in_for_the_project_config() {
    // A config under `examples/` configures the example, not the project that declares React.
    let f = files(vec![
        ("package.json", REACT_PKG),
        (
            "examples/demo/.eslintrc.json",
            r#"{ "extends": ["plugin:react-hooks/recommended"] }"#,
        ),
    ]);
    assert_eq!(the_finding(&f).file, "package.json");
}

// ── safe twins: the hooks rules are enabled, by every recognised route ──────────────────────

fn assert_legacy_config_enables(config: &str) {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (".eslintrc.json", config),
    ]);
    assert_silent(&f);
    assert!(
        !unsatisfied(&f),
        "an enabling config is a verified answer, not an unanswered one: {config}"
    );
}

#[test]
fn the_recommended_preset_enables_the_rules() {
    assert_legacy_config_enables(r#"{ "extends": ["plugin:react-hooks/recommended"] }"#);
    assert_legacy_config_enables(r#"{ "extends": "plugin:react-hooks/recommended" }"#);
    assert_legacy_config_enables(
        r#"{ "extends": ["eslint:recommended", "plugin:react/recommended", "plugin:react-hooks/recommended"] }"#,
    );
}

#[test]
fn naming_both_rules_in_the_rules_block_enables_them_in_every_severity_spelling() {
    assert_legacy_config_enables(
        r#"{ "plugins": ["react-hooks"], "rules": { "react-hooks/rules-of-hooks": "error", "react-hooks/exhaustive-deps": "warn" } }"#,
    );
    assert_legacy_config_enables(
        r#"{ "rules": { "react-hooks/rules-of-hooks": 2, "react-hooks/exhaustive-deps": 1 } }"#,
    );
    assert_legacy_config_enables(
        r#"{ "rules": { "react-hooks/rules-of-hooks": ["error"], "react-hooks/exhaustive-deps": ["warn", { "additionalHooks": "useMyEffect" }] } }"#,
    );
}

#[test]
fn one_rule_enabled_is_not_reported() {
    // The rule asks whether the hooks rules are enabled at all. A config that enables one of
    // the two is not reported: a partial setup is a real shortfall, but reporting it needs a
    // judgment about which rules the team meant to turn off, so it stays unreported.
    assert_legacy_config_enables(r#"{ "rules": { "react-hooks/rules-of-hooks": "error" } }"#);
    assert_legacy_config_enables(
        r#"{ "rules": { "react-hooks/rules-of-hooks": "off", "react-hooks/exhaustive-deps": "warn" } }"#,
    );
}

#[test]
fn shareable_configs_that_bundle_the_hooks_rules_enable_them() {
    for extends in [
        r#""react-app""#,
        r#"["react-app", "react-app/jest"]"#,
        r#""next""#,
        r#""next/core-web-vitals""#,
        r#"["next/core-web-vitals", "next/typescript"]"#,
        r#"["airbnb", "airbnb/hooks"]"#,
    ] {
        assert_legacy_config_enables(&format!(r#"{{ "extends": {extends} }}"#));
    }
}

#[test]
fn an_override_that_enables_the_rules_counts() {
    assert_legacy_config_enables(
        r#"{ "extends": "eslint:recommended", "overrides": [ { "files": ["*.tsx"], "extends": ["plugin:react-hooks/recommended"] } ] }"#,
    );
    assert_legacy_config_enables(
        r#"{ "overrides": [ { "files": ["*.tsx"], "rules": { "react-hooks/exhaustive-deps": "error" } } ] }"#,
    );
}

#[test]
fn an_eslint_config_field_in_package_json_that_enables_the_rules_is_read() {
    // CRA: `"eslintConfig": { "extends": ["react-app"] }` in package.json.
    let pkg = react_pkg_with(r#""eslintConfig": { "extends": ["react-app", "react-app/jest"] }"#);
    let f = files(vec![("package.json", &pkg)]);
    assert_silent(&f);
    assert!(!unsatisfied(&f));
}

#[test]
fn one_enabling_config_outweighs_a_non_enabling_one_elsewhere() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        (".eslintrc.json", r#"{ "extends": "eslint:recommended" }"#),
        (
            "apps/web/.eslintrc.json",
            r#"{ "extends": ["next/core-web-vitals"] }"#,
        ),
    ]);
    assert_silent(&f);
}

// ── safe twins: a config the checker cannot read is never reported ──────────────────────────

fn assert_unreadable(config_path: &str, config: &str) {
    let f = files(vec![("package.json", REACT_PKG), (config_path, config)]);
    assert_silent(&f);
    assert!(
        unsatisfied(&f),
        "{config_path}: an unanswered repo must stay with the advisory tier, not read as clean"
    );
}

#[test]
fn a_flat_config_is_never_read_as_a_gap_whatever_it_contains() {
    // JavaScript computes its configuration: spreads, imports of local files, helper calls.
    for path in [
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.cjs",
        "eslint.config.ts",
        "eslint.config.mts",
        "apps/web/eslint.config.js",
    ] {
        assert_unreadable(
            path,
            "import js from '@eslint/js';\nexport default [js.configs.recommended];\n",
        );
    }
}

#[test]
fn a_javascript_or_yaml_legacy_config_is_never_read_as_a_gap() {
    assert_unreadable(
        ".eslintrc.js",
        "module.exports = { extends: ['eslint:recommended'] };\n",
    );
    assert_unreadable(".eslintrc.cjs", "module.exports = { root: true };\n");
    assert_unreadable(".eslintrc.yml", "extends:\n  - eslint:recommended\n");
    assert_unreadable(".eslintrc.yaml", "root: true\n");
}

#[test]
fn a_json_config_that_does_not_parse_is_never_read_as_a_gap() {
    assert_unreadable(".eslintrc.json", "{ \"extends\": [ \"eslint:recommended\" ");
    assert_unreadable(".eslintrc.json", "extends: eslint:recommended\n");
    // A YAML body in the extensionless file.
    assert_unreadable(
        ".eslintrc",
        "extends: eslint:recommended\nrules:\n  no-console: warn\n",
    );
}

#[test]
fn a_json_config_that_extends_something_the_checker_cannot_see_into_is_never_a_gap() {
    for extends in [
        r#""standard""#,
        r#"["eslint:recommended", "@acme/eslint-config"]"#,
        r#""./base.eslintrc.json""#,
        r#""../../.eslintrc.base.json""#,
        r#"["airbnb"]"#,
        r#""plugin:somethingnew/recommended""#,
        r#""eslint-config-acme""#,
    ] {
        assert_unreadable(".eslintrc.json", &format!(r#"{{ "extends": {extends} }}"#));
    }
}

#[test]
fn a_json_config_with_a_key_the_checker_does_not_know_is_never_a_gap() {
    assert_unreadable(
        ".eslintrc.json",
        r#"{ "extends": "eslint:recommended", "customThing": true }"#,
    );
}

#[test]
fn a_hooks_rule_with_a_severity_the_checker_cannot_read_is_never_a_gap() {
    assert_unreadable(
        ".eslintrc.json",
        r#"{ "rules": { "react-hooks/exhaustive-deps": { "level": "error" } } }"#,
    );
}

#[test]
fn an_invalid_package_json_makes_the_repo_unanswerable() {
    let f = files(vec![
        ("package.json", REACT_PKG),
        ("apps/web/package.json", "{ \"name\": \"web\", "),
    ]);
    assert_silent(&f);
    assert!(unsatisfied(&f));
}

#[test]
fn another_linter_or_a_hidden_shareable_config_in_the_manifest_is_never_a_gap() {
    for deps in [
        r#""devDependencies": { "@biomejs/biome": "1.9.0" }"#,
        r#""devDependencies": { "oxlint": "0.9.0" }"#,
        r#""devDependencies": { "xo": "0.58.0" }"#,
        r#""devDependencies": { "eslint-config-next": "14.2.0" }"#,
        r#""devDependencies": { "eslint-config-react-app": "7.0.1" }"#,
        r#""devDependencies": { "eslint-config-airbnb": "19.0.4" }"#,
        r#""devDependencies": { "@acme/eslint-config": "1.0.0" }"#,
        r#""devDependencies": { "@acme/eslint-config-react": "1.0.0" }"#,
        r#""devDependencies": { "@antfu/eslint-config": "2.0.0" }"#,
        r#""devDependencies": { "eslint-plugin-react-hooks": "^5.0.0" }"#,
    ] {
        let pkg = react_pkg_with(deps);
        let f = files(vec![("package.json", &pkg)]);
        assert_silent(&f);
        assert!(unsatisfied(&f), "{deps}");
    }
}

#[test]
fn a_manifest_script_that_points_eslint_at_a_config_elsewhere_is_never_a_gap() {
    for script in [
        "eslint -c tools/lint/eslintrc.json src",
        "eslint --config ./tools/eslint.config.js .",
        "biome check .",
        "oxlint src",
    ] {
        let pkg = react_pkg_with(&format!(r#""scripts": {{ "lint": "{script}" }}"#));
        let f = files(vec![("package.json", &pkg)]);
        assert_silent(&f);
        assert!(unsatisfied(&f), "{script}");
    }
}

#[test]
fn a_plain_lint_script_does_not_hide_a_gap() {
    let pkg = react_pkg_with(r#""scripts": { "lint": "eslint src" }"#);
    let f = files(vec![("package.json", &pkg)]);
    assert_eq!(the_finding(&f).file, "package.json");
}

#[test]
fn another_linters_config_file_is_never_a_gap() {
    for path in [
        "biome.json",
        "biome.jsonc",
        ".oxlintrc.json",
        "oxlint.json",
        "rome.json",
    ] {
        assert_unreadable(path, "{}");
    }
}

// ── safe twins: not a React repo ────────────────────────────────────────────────────────────

#[test]
fn a_repo_that_does_not_depend_on_react_is_never_flagged() {
    let f = files(vec![
        (
            "package.json",
            r#"{ "name": "api", "dependencies": { "express": "^4", "react-query-lookalike": "1" } }"#,
        ),
        ("src/server.js", "module.exports = {};\n"),
    ]);
    assert_silent(&f);
    assert!(unsatisfied(&f), "not a React repo: nothing was answered");
}

#[test]
fn a_repo_with_no_manifest_is_never_flagged() {
    let f = files(vec![("src/App.jsx", "export {};\n"), ("README.md", "hi")]);
    assert_silent(&f);
}

#[test]
fn react_declared_only_inside_example_fixture_or_doc_folders_is_not_a_react_repo() {
    for path in [
        "examples/demo/package.json",
        "example/package.json",
        "docs/package.json",
        "test/fixtures/app/package.json",
        "e2e/app/package.json",
        "templates/react/package.json",
        "packages/ui/node_modules/react/package.json",
    ] {
        let f = files(vec![
            (
                "package.json",
                r#"{ "name": "tool", "dependencies": { "commander": "1" } }"#,
            ),
            (path, REACT_PKG),
        ]);
        assert_silent(&f);
    }
}

#[test]
fn a_package_json_that_merely_names_react_in_a_value_is_not_a_react_repo() {
    let f = files(vec![(
        "package.json",
        r#"{ "name": "react", "description": "the react docs", "dependencies": { "left-pad": "1" } }"#,
    )]);
    assert_silent(&f);
}

// ── the seam: registration, interest globs, write-time opt-out ──────────────────────────────

#[test]
fn the_checker_answers_exactly_its_own_rule() {
    assert_eq!(ReactHooksLintChecker.rule_ids(), ["UI-REACT-HOOKS-LINT-1"]);
}

#[test]
fn the_checker_applies_to_a_repo_with_a_manifest_and_not_to_one_without() {
    let with = files(vec![("package.json", "{}")]);
    let nested = files(vec![("apps/web/package.json", "{}")]);
    let without = raw_files(vec![("README.md", ""), ("src/main.rs", "")]);
    assert!(checker_applies(&ReactHooksLintChecker, &with));
    assert!(checker_applies(&ReactHooksLintChecker, &nested));
    assert!(!checker_applies(&ReactHooksLintChecker, &without));
}

#[test]
fn every_config_shape_the_checker_reads_is_in_its_interest_globs() {
    for path in [
        "package.json",
        "apps/web/package.json",
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.ts",
        ".eslintrc",
        ".eslintrc.json",
        ".eslintrc.cjs",
        ".eslintrc.yml",
        "apps/web/.eslintrc.json",
        "biome.json",
        "biome.jsonc",
        ".oxlintrc.json",
        "oxlint.json",
        "rome.json",
        // The React-code gate reads component and module files.
        "src/App.jsx",
        "src/App.tsx",
        "src/hooks.js",
        "src/hooks.ts",
        "src/main.mjs",
        "src/main.cjs",
        "src/main.mts",
        "src/main.cts",
    ] {
        let f = raw_files(vec![(path, "")]);
        assert!(
            checker_applies(&ReactHooksLintChecker, &f),
            "{path} must be loaded for the checker (the Layer-2 and CI file walks load only these)"
        );
    }
}

#[test]
fn the_checker_is_a_scan_and_ci_rule_not_a_write_time_gate() {
    // Fixing the gap installs a dependency and picks its version: a human decision, not
    // something a bounced agent can finish.
    assert!(!ReactHooksLintChecker.runs_at_write_time());
}

#[test]
fn a_verified_answer_is_credited_and_an_unanswered_one_is_not() {
    let gap = files(vec![("package.json", REACT_PKG)]);
    assert!(!unsatisfied(&gap), "a gap is an answer");
    let not_react = files(vec![("package.json", r#"{ "name": "x" }"#)]);
    assert!(unsatisfied(&not_react));
}

#[test]
fn the_message_uses_no_dashes_and_no_internal_vocabulary() {
    let f = files(vec![("package.json", REACT_PKG)]);
    let v = the_finding(&f);
    assert!(!v.message.contains('\u{2014}'), "{}", v.message);
    assert!(!v.message.contains('\u{2013}'), "{}", v.message);
    for banned in ["Camerata", "needs-review", "needs_review"] {
        assert!(!v.message.contains(banned), "{banned}: {}", v.message);
    }
}

#[test]
fn findings_are_deterministic_whatever_the_file_order() {
    let a = files(vec![
        ("apps/b/package.json", REACT_PKG),
        ("apps/a/package.json", REACT_PKG),
        (".eslintrc.json", r#"{ "extends": "eslint:recommended" }"#),
    ]);
    let mut b = a.clone();
    b.reverse();
    assert_eq!(run(&a), run(&b));
}

// ── the lenient JSON reader ─────────────────────────────────────────────────────────────────

#[test]
fn the_lenient_reader_keeps_comment_lookalikes_inside_strings() {
    let v = config::parse_lenient_json(
        "{ \"url\": \"https://example.com/a//b\", \"glob\": \"/* not a comment */\", \"q\": \"say \\\"hi\\\" // still text\" }",
    )
    .expect("parses");
    assert_eq!(v["url"], "https://example.com/a//b");
    assert_eq!(v["glob"], "/* not a comment */");
    assert_eq!(v["q"], "say \"hi\" // still text");
}

#[test]
fn the_lenient_reader_drops_comments_and_trailing_commas_only() {
    let v = config::parse_lenient_json(
        "\u{feff}{ // line\n \"a\": [1, 2, /* mid */ 3,], /* tail */ \"b\": { \"c\": true, },\n }",
    )
    .expect("parses");
    assert_eq!(v["a"], serde_json::json!([1, 2, 3]));
    assert_eq!(v["b"]["c"], true);
}

#[test]
fn the_lenient_reader_refuses_what_is_not_json() {
    for text in [
        "",
        "extends: eslint:recommended",
        "{ \"a\": }",
        "module.exports = {};",
        "{ a: 1 }",
    ] {
        assert!(config::parse_lenient_json(text).is_none(), "{text}");
    }
}

#[test]
fn a_config_under_an_example_folder_neither_enables_nor_hides_the_projects_gap() {
    // Auxiliary folders are skipped for every purpose: an unreadable flat config in an example
    // does not make the project's own missing lint setup unanswerable either.
    let f = files(vec![
        ("package.json", REACT_PKG),
        ("examples/demo/eslint.config.js", "export default [];\n"),
    ]);
    assert_eq!(the_finding(&f).file, "package.json");
}

// ── review round: frameworks that lint internally ───────────────────────────────────────────

#[test]
fn a_framework_that_runs_eslint_config_react_app_itself_is_never_a_gap() {
    // Gatsby runs eslint-config-react-app internally when the project has no config of its own;
    // Create React App 3.x does the same through react-scripts.
    for deps in [
        r#""devDependencies": { "gatsby": "^5.13.0" }"#,
        r#""devDependencies": { "react-scripts": "3.4.4" }"#,
        r#""peerDependencies": { "react-scripts": "5.0.1" }"#,
    ] {
        let pkg = react_pkg_with(deps);
        let f = files(vec![("package.json", &pkg)]);
        assert_silent(&f);
        assert!(unsatisfied(&f), "{deps}");
    }
}

#[test]
fn a_package_that_only_resembles_a_lint_hiding_framework_does_not_hide_a_gap() {
    // Exact names only: `gatsby-plugin-image` is a plugin, not the framework.
    let pkg = react_pkg_with(
        r#""devDependencies": { "gatsby-plugin-image": "3", "react-scripts-ts-lookalike": "1" }"#,
    );
    let f = files(vec![("package.json", &pkg)]);
    assert_eq!(the_finding(&f).file, "package.json");
}

// ── review round: ESLint given a config outside `scripts` ───────────────────────────────────

#[test]
fn eslint_given_its_own_config_or_rules_anywhere_in_the_manifest_is_never_a_gap() {
    for fragment in [
        r#""lint-staged": { "*.{js,jsx}": "eslint --config tools/eslintrc.json --fix" }"#,
        r#""lint-staged": { "*.js": ["eslint -c tools/e.json --fix", "prettier --write"] }"#,
        r#""husky": { "hooks": { "pre-commit": "eslint --no-eslintrc -c e.json src" } }"#,
        r#""nano-staged": { "*.js": "eslint --rulesdir tools/rules" }"#,
        r#""simple-git-hooks": { "pre-commit": "npx eslint --plugin react-hooks --rule 'react-hooks/rules-of-hooks: error' src" }"#,
        r#""scripts": { "lint": "eslint --resolve-plugins-relative-to ./tools ." }"#,
        r#""scripts": { "lint": "eslint --rule 'no-console: error' ." }"#,
        r#""scripts": { "lint": "eslint --plugin react-hooks ." }"#,
        r#""scripts": { "lint": "eslint --rulesdir rules ." }"#,
    ] {
        let pkg = react_pkg_with(fragment);
        let f = files(vec![("package.json", &pkg)]);
        assert_silent(&f);
        assert!(unsatisfied(&f), "{fragment}");
    }
}

#[test]
fn plain_eslint_invocations_anywhere_in_the_manifest_do_not_hide_a_gap() {
    for fragment in [
        r#""lint-staged": { "*.{js,jsx}": "eslint --fix" }"#,
        r#""husky": { "hooks": { "pre-commit": "lint-staged && eslint src" } }"#,
        r#""simple-git-hooks": { "pre-commit": "npx eslint --max-warnings 0 src" }"#,
    ] {
        let pkg = react_pkg_with(fragment);
        let f = files(vec![("package.json", &pkg)]);
        assert_eq!(the_finding(&f).file, "package.json", "{fragment}");
    }
}

// ── review round: React code ────────────────────────────────────────────────────────────────

fn react_code_gap(extra: Vec<(&str, &str)>) -> Vec<(String, String)> {
    let mut pairs = vec![("package.json", REACT_PKG)];
    pairs.extend(extra);
    raw_files(pairs)
}

#[test]
fn a_react_dependency_with_no_react_code_is_not_assessed() {
    // A plugin that lists react as a peer, a Storybook-only use, a mail-template package, a
    // build tool: the dependency is there, the React code is not.
    let f = react_code_gap(vec![
        ("src/index.ts", "export const plugin = () => 1;\n"),
        ("src/util.js", "module.exports = {};\n"),
        ("README.md", "a plugin\n"),
    ]);
    assert_silent(&f);
    assert!(
        unsatisfied(&f),
        "the ledger records not assessed, never verified clean"
    );
}

#[test]
fn react_code_is_a_component_file_or_a_file_importing_from_react() {
    for (path, body) in [
        ("src/App.jsx", "export const App = () => null;\n"),
        ("src/App.tsx", "export const App = () => null;\n"),
        ("src/hooks.js", "import { useState } from 'react';\n"),
        ("src/hooks.ts", "import { useState } from \"react\";\n"),
        ("src/legacy.js", "const React = require('react');\n"),
        ("src/legacy.cjs", "const R = require(\"react\");\n"),
        ("src/main.mjs", "import ReactDOM from 'react-dom/client';\n"),
        ("src/jsx.ts", "import { jsx } from 'react/jsx-runtime';\n"),
        ("src/side.js", "import 'react';\n"),
    ] {
        let f = react_code_gap(vec![(path, body)]);
        assert_eq!(the_finding(&f).file, "package.json", "{path}");
        assert!(!unsatisfied(&f), "{path}");
    }
}

#[test]
fn files_that_only_mention_react_in_other_words_are_not_react_code() {
    for (path, body) in [
        ("src/a.js", "import x from 'react-query';\n"),
        ("src/b.js", "import x from './react';\n"),
        ("src/d.ts", "import { z } from 'preact';\n"),
        ("src/e.js", "import x from 'reactive-streams';\n"),
    ] {
        let f = react_code_gap(vec![(path, body)]);
        assert_silent(&f);
    }
}

#[test]
fn component_files_in_test_story_or_example_folders_are_not_react_code() {
    for path in [
        "src/App.test.tsx",
        "src/Button.stories.tsx",
        "stories/Button.tsx",
        "examples/demo/App.jsx",
        "docs/App.tsx",
        "__tests__/App.jsx",
        "test/App.tsx",
    ] {
        let f = react_code_gap(vec![(path, "export const A = () => null;\n")]);
        assert_silent(&f);
        assert!(unsatisfied(&f), "{path}");
    }
}

// ── review round: folder names are case-insensitive ─────────────────────────────────────────

#[test]
fn auxiliary_folder_names_match_in_any_case() {
    for path in [
        "Docs/package.json",
        "Examples/demo/package.json",
        "EXAMPLE/package.json",
        "Templates/react/package.json",
        "Vendor/app/package.json",
    ] {
        let f = files(vec![
            (
                "package.json",
                r#"{ "name": "tool", "dependencies": { "commander": "1" } }"#,
            ),
            (path, REACT_PKG),
        ]);
        assert_silent(&f);
    }
    // And a config under such a folder is skipped just the same.
    let f = files(vec![
        ("package.json", REACT_PKG),
        ("Examples/demo/eslint.config.js", "export default [];\n"),
    ]);
    assert_eq!(the_finding(&f).file, "package.json");
}
