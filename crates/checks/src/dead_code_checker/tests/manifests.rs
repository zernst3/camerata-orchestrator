//! Package manifests: the entry points they name (`R2`), the public-surface rule (`R4`), the
//! invisible-entry rule (`R7`), and the framework-dependency skip (`G19`).

use super::super::manifests::{manifest_roots, Packages};
use super::*;

fn roots_of(f: &[(String, String)]) -> super::super::roots::RootSet {
    manifest_roots(f, &Packages::load(f))
}

// ── R2: entry fields, resolved like an import ──────────────────────────────────

#[test]
fn entry_fields_resolve_with_extension_and_index_inference() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"main":"./src/server","module":"lib","types":"./src/types/entry.ts"}"#,
        ),
        ("src/server.ts", ""),
        ("lib/index.ts", ""),
        ("src/types/entry.ts", ""),
        ("src/unrelated.ts", ""),
    ]);
    let r = roots_of(&f);
    assert!(r.contains("src/server.ts"));
    assert!(r.contains("lib/index.ts"));
    assert!(r.contains("src/types/entry.ts"));
    assert!(!r.contains("src/unrelated.ts"));
}

#[test]
fn bin_and_browser_accept_a_string_or_a_map_of_paths() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"bin":{"a":"./bin/a.js","b":"bin/b.js"},"browser":{"./x.js":"./web/x.js","./y.js":false}}"#,
        ),
        ("bin/a.js", ""),
        ("bin/b.js", ""),
        ("web/x.js", ""),
    ]);
    let r = roots_of(&f);
    for p in ["bin/a.js", "bin/b.js", "web/x.js"] {
        assert!(r.contains(p), "{p}");
    }
    let one = files(vec![
        ("package.json", r#"{"private":true,"bin":"cli/run.js"}"#),
        ("cli/run.js", ""),
    ]);
    assert!(roots_of(&one).contains("cli/run.js"));
}

#[test]
fn exports_leaves_are_read_recursively_through_conditions() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"exports":{".":{"import":"./esm/index.js","require":{"default":"./cjs/index.js"}},"./util":"./src/util.ts","./hidden":null}}"#,
        ),
        ("esm/index.js", ""),
        ("cjs/index.js", ""),
        ("src/util.ts", ""),
    ]);
    let r = roots_of(&f);
    for p in ["esm/index.js", "cjs/index.js", "src/util.ts"] {
        assert!(r.contains(p), "{p}");
    }
}

#[test]
fn a_nested_package_resolves_entries_relative_to_its_own_directory() {
    let f = files(vec![
        (
            "packages/ui/package.json",
            r#"{"private":true,"main":"src/index.ts"}"#,
        ),
        ("packages/ui/src/index.ts", ""),
        ("src/index.ts", ""),
    ]);
    let r = roots_of(&f);
    assert!(r.contains("packages/ui/src/index.ts"));
    assert!(!r.contains("src/index.ts"));
}

#[test]
fn malformed_and_non_object_manifests_yield_no_roots() {
    for body in ["{ not json", "[]", "\"str\"", "null", ""] {
        let f = files(vec![("package.json", body), ("src/a.ts", "")]);
        let r = roots_of(&f);
        assert!(!r.contains("src/a.ts"), "{body}");
    }
}

#[test]
fn pyproject_script_targets_resolve_to_their_module_file() {
    let f = files(vec![
        (
            "pyproject.toml",
            "[project.scripts]\nx = \"pkg.cli:main\"\n\n[project.entry-points.\"myapp.plugins\"]\np = \"pkg.plug:Plugin\"\n\n[tool.poetry.scripts]\ny = \"pkg.job:run\"\n",
        ),
        ("pkg/__init__.py", ""),
        ("pkg/cli.py", ""),
        ("pkg/plug.py", ""),
        ("pkg/job.py", ""),
        ("pkg/other.py", ""),
    ]);
    let r = roots_of(&f);
    for p in ["pkg/cli.py", "pkg/plug.py", "pkg/job.py"] {
        assert!(r.contains(p), "{p}");
    }
    assert!(!r.contains("pkg/other.py"));
}

#[test]
fn an_ambiguous_python_entry_module_is_dropped_not_guessed() {
    let f = files(vec![
        (
            "pyproject.toml",
            "[project.scripts]\nx = \"svc.cli:main\"\n",
        ),
        ("a/svc/cli.py", ""),
        ("b/svc/cli.py", ""),
    ]);
    let r = roots_of(&f);
    assert!(!r.contains("a/svc/cli.py") && !r.contains("b/svc/cli.py"));
}

// ── end to end ─────────────────────────────────────────────────────────────────

#[test]
fn twin_package_json_main_bin_and_exports_targets_are_live() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"main":"src/server.ts","bin":{"x":"src/cli.ts"},"exports":{".":{"import":"./src/index.ts"},"./util":"./src/util/index.ts"}}"#,
        ),
        ("src/server.ts", "export {};\n"),
        ("src/cli.ts", "export {};\n"),
        ("src/index.ts", "export {};\n"),
        ("src/util/index.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_exports_subpath_pattern_keeps_the_target_dir_live() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"exports":{"./*":"./src/*.ts"}}"#,
        ),
        ("src/anything.ts", "export const anything = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_a_file_outside_the_pattern_directory_is_still_flagged() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"exports":{"./*":"./src/*.ts"}}"#,
        ),
        ("src/anything.ts", "export const anything = 1;\n"),
        ("lib/outside.ts", "export const outside = 1;\n"),
    ]);
    assert_flags_only(&f, "lib/outside.ts");
}

#[test]
fn twin_public_package_without_exports_is_deep_importable() {
    let f = files(vec![
        (
            "package.json",
            r#"{"name":"@acme/lib","main":"src/index.ts"}"#,
        ),
        ("src/index.ts", "export {};\n"),
        ("src/deep.ts", "export const deepthing = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn private_package_without_exports_still_flags_unreached_file() {
    let f = files(vec![
        (
            "package.json",
            r#"{"name":"@acme/lib","private":true,"main":"src/index.ts"}"#,
        ),
        ("src/index.ts", "export {};\n"),
        ("src/deep.ts", "export const deepthing = 1;\n"),
    ]);
    assert_flags_only(&f, "src/deep.ts");
}

#[test]
fn a_public_package_exposes_only_its_own_subtree() {
    let f = files(vec![
        ("packages/pub/package.json", r#"{"name":"pub"}"#),
        ("packages/pub/src/deepthing.ts", "export {};\n"),
        (
            "packages/priv/package.json",
            r#"{"name":"priv","private":true}"#,
        ),
        ("packages/priv/src/deepthing.ts", "export {};\n"),
    ]);
    // The homonymous name is mentioned by nothing; only the private copy is flagged.
    assert_flags_only(&f, "packages/priv/src/deepthing.ts");
}

#[test]
fn public_package_with_an_exports_map_is_not_deep_importable() {
    let f = files(vec![
        (
            "package.json",
            r#"{"name":"@acme/lib","exports":{".":"./src/index.ts"}}"#,
        ),
        ("src/index.ts", "export {};\n"),
        ("src/deep.ts", "export const deepthing = 1;\n"),
    ]);
    assert_flags_only(&f, "src/deep.ts");
}

#[test]
fn twin_invisible_manifest_entry_disables_the_package() {
    let f = files(vec![
        ("package.json", r#"{"private":true,"bin":"bin/cli.js"}"#),
        ("src/cli-impl.ts", "export const impl = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_a_visible_manifest_entry_does_not_disable_the_package() {
    let f = files(vec![
        ("package.json", r#"{"private":true,"bin":"bin/cli.js"}"#),
        ("bin/cli.js", "console.log(1);\n"),
        ("src/cli-impl.ts", "export const impl = 1;\n"),
    ]);
    assert_flags_only(&f, "src/cli-impl.ts");
}

#[test]
fn an_invisible_entry_only_silences_its_own_package() {
    let f = files(vec![
        (
            "packages/a/package.json",
            r#"{"private":true,"main":"dist/index.js"}"#,
        ),
        ("packages/a/src/inner.ts", "export const inner = 1;\n"),
        ("packages/b/package.json", r#"{"private":true}"#),
        ("packages/b/src/stranded.ts", "export const stranded = 1;\n"),
    ]);
    assert_flags_only(&f, "packages/b/src/stranded.ts");
}

#[test]
fn a_pattern_entry_whose_directory_is_absent_is_an_invisible_entry() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"exports":{"./*":"./dist/*.js"}}"#,
        ),
        ("src/impl.ts", "export const impl = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_pyproject_script_entry_is_live() {
    let f = files(vec![
        (
            "pyproject.toml",
            "[project.scripts]\nx = \"pkg.cli:main\"\n",
        ),
        ("pkg/__init__.py", ""),
        ("pkg/cli.py", "def main(): pass\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_python_main_guard_and_shebang_are_entry_points() {
    let f = files(vec![
        (
            "tools/run.py",
            "def f(): pass\nif __name__ == \"__main__\":\n    f()\n",
        ),
        ("jobs/nightly.py", "#!/usr/bin/env python3\nprint(1)\n"),
        ("jobs/runner.js", "#!/usr/bin/env node\nconsole.log(1);\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_the_same_script_without_a_guard_is_flagged() {
    let f = files(vec![("jobs/runit.py", "def f(): pass\n")]);
    assert_flags_only(&f, "jobs/runit.py");
}

#[test]
fn positive_sibling_a_main_guard_far_from_the_name_check_does_not_count() {
    let mut body = String::from("if __name__ == name_of_thing:\n");
    body.push_str(&" ".repeat(60));
    body.push_str("pass  # \"__main__\"\n");
    let f = files(vec![
        ("jobs/pkg_init.py", "x = 1\n"),
        ("jobs/far.py", body.as_str()),
    ]);
    let got = flagged(&f);
    assert!(got.contains(&"jobs/far.py".to_string()), "{got:?}");
}

// ── G19: Vue, Svelte, Astro and MDX importers are invisible to the graph ───────

#[test]
fn twin_vue_package_is_skipped_when_sfcs_are_not_fed() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"dependencies":{"vue":"3"}}"#,
        ),
        (
            "src/composables/useThing.ts",
            "export const useThing = 1;\n",
        ),
    ]);
    assert_silent(&f);
}

#[test]
fn every_sfc_framework_dependency_skips_its_package_when_sfcs_are_not_fed() {
    for dep in [
        "vue",
        "nuxt",
        "svelte",
        "@sveltejs/kit",
        "astro",
        "@mdx-js/react",
        "@mdx-js/loader",
    ] {
        let manifest = format!(r#"{{"private":true,"devDependencies":{{"{dep}":"1"}}}}"#);
        let f = files(vec![
            ("package.json", manifest.as_str()),
            ("src/thing.ts", "export const stranded = 1;\n"),
        ]);
        assert_silent(&f);
    }
}

#[test]
fn positive_sibling_a_vue_package_with_its_sfcs_fed_is_checked_normally() {
    let f = files(vec![
        (
            "package.json",
            r#"{"private":true,"dependencies":{"vue":"3"}}"#,
        ),
        ("src/App.vue", "<template><div/></template>\n"),
        (
            "src/composables/useThing.ts",
            "export const useThing = 1;\n",
        ),
    ]);
    assert_flags_only(&f, "src/composables/useThing.ts");
}

#[test]
fn a_framework_skip_is_scoped_to_the_package_that_depends_on_it() {
    let f = files(vec![
        (
            "apps/web/package.json",
            r#"{"private":true,"dependencies":{"vue":"3"}}"#,
        ),
        ("apps/web/src/skipped.ts", "export const skipped = 1;\n"),
        ("apps/api/package.json", r#"{"private":true}"#),
        ("apps/api/src/stranded.ts", "export const stranded = 1;\n"),
    ]);
    assert_flags_only(&f, "apps/api/src/stranded.ts");
}

// ── G24 ────────────────────────────────────────────────────────────────────────

#[test]
fn twin_monorepo_bare_import_reaches_sibling_package_internals() {
    let f = files(vec![
        (
            "packages/ui/package.json",
            r#"{"name":"@acme/ui","main":"src/index.ts","private":true}"#,
        ),
        ("packages/ui/src/index.ts", "export * from './button';\n"),
        ("packages/ui/src/button.ts", "export const Button = 1;\n"),
        (
            "packages/app/src/a.ts",
            "import { Button } from '@acme/ui';\n",
        ),
    ]);
    assert_silent(&f);
}
