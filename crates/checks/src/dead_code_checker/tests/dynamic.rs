//! Computed loads (`G9`) and loader calls that name a directory by literal (`G10`): the ways a
//! file is loaded that no import edge shows.

use super::*;

// ── G9: computed import / require / glob ───────────────────────────────────────

#[test]
fn twin_computed_import_with_literal_prefix_keeps_that_directory_live() {
    let f = files(vec![
        ("src/app.ts", "import './i18n';\n"),
        (
            "src/i18n.ts",
            "export const load = (lang) => import(`./locales/${lang}`);\n",
        ),
        ("src/locales/german.ts", "export default {};\n"),
        ("src/locales/french.ts", "export default {};\n"),
        ("src/other.ts", "export const other = 1;\n"),
    ]);
    // The prefix scopes the load to `src/locales/`; `other.ts` is outside it.
    assert_flags_only(&f, "src/other.ts");
}

#[test]
fn a_prefix_that_ends_mid_name_scopes_to_its_directory() {
    let f = files(vec![
        ("src/app.ts", "import './loader';\n"),
        (
            "src/loader.ts",
            "export const l = (n) => require('./plugins/plugin-' + n);\n",
        ),
        ("src/plugins/plugin-red.ts", "export default 1;\n"),
        ("src/plugins/plugin-blue.ts", "export default 1;\n"),
        ("src/elsewhere/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/elsewhere/stranded.ts");
}

#[test]
fn twin_computed_require_without_prefix_keeps_the_package_live() {
    let f = files(vec![
        ("packages/host/package.json", r#"{"private":true}"#),
        (
            "packages/host/src/load.ts",
            "export const l = (name) => require(name);\n",
        ),
        ("packages/host/src/plugin-a.ts", "export default 1;\n"),
        ("packages/other/package.json", r#"{"private":true}"#),
        ("packages/other/src/dead.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "packages/other/src/dead.ts");
}

#[test]
fn a_computed_load_with_an_alias_prefix_keeps_the_whole_package_live() {
    // `@/plugins/` is a bare alias: where it points is not knowable here.
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        (
            "src/load.ts",
            "export const l = (n) => import(`@/plugins/${n}`);\n",
        ),
        ("src/plugins/plugin-a.ts", "export default 1;\n"),
        ("src/features/plugin-b.ts", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_require_context_and_import_meta_glob_keep_directory_live() {
    let f = files(vec![
        ("src/app.ts", "import './registry';\nimport './iconset';\n"),
        (
            "src/registry.ts",
            "export const mods = import.meta.glob('./plugins/*.ts');\n",
        ),
        ("src/plugins/alpha.ts", "export default 1;\n"),
        (
            "src/iconset.ts",
            "export const ctx = require.context('./icons', true, /\\.svg$/);\n",
        ),
        ("src/icons/star.ts", "export default 1;\n"),
        ("src/unrelated/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/unrelated/stranded.ts");
}

#[test]
fn twin_python_pkgutil_walk_keeps_package_live() {
    let f = files(vec![
        ("app/pyproject.toml", "[project]\nname = \"app\"\n"),
        (
            "app/plugins/__init__.py",
            "import pkgutil\nmods = pkgutil.iter_modules(__path__)\n",
        ),
        ("app/plugins/foo.py", "x = 1\n"),
        ("other/pyproject.toml", "[project]\nname = \"other\"\n"),
        ("other/stranded.py", "x = 1\n"),
    ]);
    assert_flags_only(&f, "other/stranded.py");
}

#[test]
fn a_python_import_module_of_a_variable_keeps_its_package_live() {
    let f = files(vec![
        ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
        (
            "svc/runner.py",
            "import importlib\ndef run(n):\n    return importlib.import_module(n)\n",
        ),
        ("svc/jobs/nightly.py", "x = 1\n"),
        ("lib2/pyproject.toml", "[project]\nname = \"lib2\"\n"),
        ("lib2/stranded.py", "x = 1\n"),
    ]);
    let got = flagged(&f);
    // `svc/runner.py` is itself unreferenced, but it sits in the package it can load from.
    assert_eq!(got, ["lib2/stranded.py"]);
}

#[test]
fn a_rust_directory_macro_roots_the_files_under_its_directory() {
    let f = files(vec![
        ("crates/x/Cargo.toml", "[package]\nname = \"x\"\n"),
        (
            "crates/x/src/lib.rs",
            "static D: Dir = include_dir!(\"$CARGO_MANIFEST_DIR/web\");\n",
        ),
        ("crates/x/web/bundle_loader.js", "console.log(1);\n"),
        ("crates/x/other/stranded_loader.js", "console.log(1);\n"),
    ]);
    assert_flags_only(&f, "crates/x/other/stranded_loader.js");
}

#[test]
fn a_literal_load_is_not_a_computed_load() {
    // `import('./lazy')` is an ordinary edge: it must not silence the rest of the package.
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/app.ts", "const m = import('./lazy');\n"),
        ("src/lazy.ts", "export const lazy = 1;\n"),
        ("src/stranded.ts", "export const stranded = 1;\n"),
    ]);
    assert_flags_only(&f, "src/stranded.ts");
}

// ── G10: loader calls with a literal directory ─────────────────────────────────

#[test]
fn twin_loader_call_with_a_literal_directory_keeps_it_live() {
    let f = files(vec![
        ("src/app.ts", "import './scan';\n"),
        (
            "src/scan.ts",
            "import fs from 'fs';\nexport const all = fs.readdirSync('./handlers');\n",
        ),
        ("src/handlers/billing_handler.ts", "export default 1;\n"),
        ("src/features/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/features/stranded.ts");
}

#[test]
fn glob_patterns_keep_their_literal_head_directory_live() {
    let f = files(vec![
        ("src/app.ts", "import './scan';\n"),
        (
            "src/scan.ts",
            "import fg from 'fast-glob';\nexport const a = fg.sync(\"./routes/**/*.ts\");\nexport const b = glob(`./jobs/${kind}/*.ts`);\n",
        ),
        ("src/routes/orders_route.ts", "export default 1;\n"),
        ("src/jobs/nightly_job.ts", "export default 1;\n"),
        ("src/features/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/features/stranded.ts");
}

#[test]
fn python_loader_calls_resolve_from_the_callers_directory() {
    let f = files(vec![
        (
            "app/loader.py",
            "import glob\nfiles = glob.glob(\"plugins/*.py\")\nrest = os.listdir('extras/')\n",
        ),
        ("app/plugins/first_plugin.py", "x = 1\n"),
        ("app/extras/second_extra.py", "x = 1\n"),
        ("app/stranded_mod.py", "x = 1\n"),
    ]);
    let got = flagged(&f);
    assert!(got.contains(&"app/stranded_mod.py".to_string()), "{got:?}");
    assert!(
        !got.iter()
            .any(|p| p.contains("plugins/") || p.contains("extras/")),
        "{got:?}"
    );
}

#[test]
fn loader_call_resolves_from_the_repository_root_too() {
    let f = files(vec![
        (
            "tools/gen/run_all.ts",
            "export const all = glob('src/jobs/*.ts');\n",
        ),
        ("src/jobs/nightly_job.ts", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_a_directory_literal_in_a_non_loader_call_is_not_a_load() {
    let f = files(vec![
        ("src/app.ts", "import './scan';\n"),
        (
            "src/scan.ts",
            "console.log('./handlers');\nconst x = globber('./handlers');\n",
        ),
        ("src/handlers/billing_handler.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/handlers/billing_handler.ts");
}

#[test]
fn positive_sibling_a_directory_literal_that_is_not_the_first_argument_is_not_a_load() {
    let f = files(vec![
        ("src/app.ts", "import './scan';\n"),
        (
            "src/scan.ts",
            "export const x = readdir(opts, './handlers');\n",
        ),
        ("src/handlers/billing_handler.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/handlers/billing_handler.ts");
}

#[test]
fn positive_sibling_a_literal_without_a_slash_names_no_directory() {
    let f = files(vec![
        ("src/app.ts", "import './scan';\n"),
        ("src/scan.ts", "export const x = readdirSync('handlers');\n"),
        ("src/handlers/billing_handler.ts", "export default 1;\n"),
    ]);
    // `handlers` carries no slash, so it is not read as a path (the mention guard still sees the
    // word, but the file's own name `billing_handler` is not in it).
    assert_flags_only(&f, "src/handlers/billing_handler.ts");
}

#[test]
fn twin_tailwind_content_glob_is_not_a_loader() {
    let f = files(vec![
        (
            "tailwind.config.js",
            "module.exports = { content: [\"./src/**/*.tsx\"] };\n",
        ),
        ("src/dead.tsx", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/dead.tsx");
}

#[test]
fn a_loader_name_inside_a_longer_identifier_is_not_a_loader() {
    let f = files(vec![
        ("src/app.ts", "import './scan';\n"),
        (
            "src/scan.ts",
            "export const x = myreaddir('./handlers');\nconst y = dirty('./handlers');\n",
        ),
        ("src/handlers/billing_handler.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/handlers/billing_handler.ts");
}

#[test]
fn a_dot_slash_prefix_roots_the_callers_own_folder() {
    let f = files(vec![
        ("src/app.ts", "import './features/loader';\n"),
        (
            "src/features/loader.ts",
            "export const l = (n) => import('./' + n);\n",
        ),
        ("src/features/sibling_feature.ts", "export default 1;\n"),
        ("src/elsewhere/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/elsewhere/stranded.ts");
}

#[test]
fn a_parent_relative_prefix_roots_the_folder_it_names() {
    let f = files(vec![
        ("src/app.ts", "import './features/loader';\n"),
        (
            "src/features/loader.ts",
            "export const l = (n) => require(`../shared/${n}`);\n",
        ),
        ("src/shared/shared_widget.ts", "export default 1;\n"),
        ("src/elsewhere/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/elsewhere/stranded.ts");
}

#[test]
fn a_prefix_naming_a_folder_the_scan_lacks_roots_nothing() {
    let f = files(vec![
        ("src/app.ts", "import './loader';\n"),
        (
            "src/loader.ts",
            "export const l = (n) => import(`./pruned/${n}`);\n",
        ),
        ("src/elsewhere/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "src/elsewhere/stranded.ts");
}

#[test]
fn rails_importmap_pin_all_from_roots_the_pinned_folder() {
    let f = files(vec![
        ("Gemfile", "gem 'importmap-rails'\n"),
        (
            "config/importmap.rb",
            "pin \"application\"\npin_all_from \"app/javascript/controllers\", under: \"controllers\"\n",
        ),
        ("app/javascript/application.js", "import \"controllers\"\n"),
        ("app/javascript/controllers/hello_controller.js", "export default class {}\n"),
        ("app/javascript/controllers/clipboard_copy_controller.js", "export default class {}\n"),
        ("app/javascript/stranded/stranded_widget.js", "export default class {}\n"),
    ]);
    assert_flags_only(&f, "app/javascript/stranded/stranded_widget.js");
}

#[test]
fn pin_all_from_with_parentheses_and_other_commands_without_them_are_distinguished() {
    let f = files(vec![
        (
            "config/importmap.rb",
            "pin_all_from(\"app/javascript/widgets\")\n",
        ),
        (
            "app/javascript/widgets/gauge_widget.js",
            "export default 1;\n",
        ),
    ]);
    assert_silent(&f);
    // Only `pin_all_from` is a loader without parentheses; a bare `glob "..."` in prose is not.
    let prose = files(vec![
        (
            "notes.txt",
            "We should glob \"app/javascript/widgets\" someday.\n",
        ),
        (
            "app/javascript/widgets/gauge_widget.js",
            "export default 1;\n",
        ),
    ]);
    assert_flags_only(&prose, "app/javascript/widgets/gauge_widget.js");
}

// ── glob strings with a literal part: TypeORM entities, Nest modules, handlers ──

#[test]
fn twin_typeorm_entity_glob_in_a_config_object_keeps_the_entities_live() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/main.ts", "import './data-source';\n"),
        (
            "src/data-source.ts",
            "export const dataSource = { migrations: ['src/migrations/*.ts'], entities: ['src/**/*.entity.ts'] };\n",
        ),
        ("src/users/user.entity.ts", "export class User {}\n"),
        ("src/orders/deep/order.entity.ts", "export class Order {}\n"),
        ("src/users/stranded_service.ts", "export class S {}\n"),
    ]);
    assert_flags_only(&f, "src/users/stranded_service.ts");
}

#[test]
fn positive_sibling_an_entity_file_outside_the_globs_root_is_still_flagged() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/main.ts", "import './data-source';\n"),
        (
            "src/data-source.ts",
            "export const dataSource = { entities: ['src/**/*.entity.ts'] };\n",
        ),
        ("src/users/user.entity.ts", "export class User {}\n"),
        ("lib/outside.entity.ts", "export class Outside {}\n"),
    ]);
    assert_flags_only(&f, "lib/outside.entity.ts");
}

#[test]
fn twin_module_and_brace_basename_globs_keep_their_matches_live() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        (
            "app.config.ts",
            "export default { modules: ['**/*.module.ts'], resolvers: [\"src/**/*.resolver.{ts,js}\"] };\n",
        ),
        ("src/billing/billing.module.ts", "export class B {}\n"),
        ("src/billing/billing.resolver.ts", "export class R {}\n"),
        ("src/billing/ledger.resolver.js", "module.exports = {};\n"),
        ("src/billing/stranded_service.ts", "export class S {}\n"),
    ]);
    assert_flags_only(&f, "src/billing/stranded_service.ts");
}

#[test]
fn a_literal_directory_below_a_wildcard_makes_the_glob_meaningful() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/app.ts", "export const all = ['**/handlers/*.ts'];\n"),
        ("src/orders/handlers/create_order.ts", "export {};\n"),
        ("src/orders/stranded_service.ts", "export {};\n"),
    ]);
    assert_flags_only(&f, "src/orders/stranded_service.ts");
}

#[test]
fn pure_extension_globs_stay_ignored_so_a_tailwind_repo_is_still_checked() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        (
            "tailwind.config.js",
            "module.exports = { content: ['./src/**/*.{js,ts,jsx,tsx}', './app/**/*.tsx', './pages/*.{ts,tsx}', '**/*.html'] };\n",
        ),
        ("src/dead_widget.tsx", "export default 1;\n"),
        ("app/dead_screen.tsx", "export default 1;\n"),
        ("pages/dead_page.ts", "export default 1;\n"),
    ]);
    let got = flagged(&f);
    assert_eq!(got.len(), 3, "{got:?}");
}

#[test]
fn a_glob_resolves_from_the_nearest_manifest_folder_as_well_as_the_repository_root() {
    let f = files(vec![
        ("packages/api/package.json", r#"{"private":true}"#),
        ("packages/api/src/main.ts", "import './ormconfig';\n"),
        (
            "packages/api/src/ormconfig.ts",
            "export default { entities: ['dist/**/*.entity.js', 'src/**/*.entity.ts'] };\n",
        ),
        (
            "packages/api/src/users/user.entity.ts",
            "export class U {}\n",
        ),
    ]);
    assert_silent(&f);
}

#[test]
fn tsconfig_style_files_are_not_a_source_of_glob_strings() {
    // `exclude` lists name files a project does NOT want built; they must not keep them alive.
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        (
            "tsconfig.json",
            r#"{"exclude": ["src/legacy/**/*.legacy_unit.ts"]}"#,
        ),
        (
            "tsconfig.build.json",
            r#"{"exclude": ["src/old/**/*.old_unit.ts"]}"#,
        ),
        ("src/app.ts", "export {};\n"),
        ("src/legacy/a/thing.legacy_unit.ts", "export {};\n"),
        ("src/old/b/thing.old_unit.ts", "export {};\n"),
    ]);
    let text: String = run(&f).iter().map(|v| v.message.clone()).collect();
    assert!(
        text.contains("legacy_unit") && text.contains("old_unit"),
        "{text}"
    );
}

#[test]
fn glob_helpers_expand_braces_and_judge_meaning() {
    use super::super::dynamic::{expand_braces, is_meaningful_glob};
    assert_eq!(expand_braces("a/*.{ts,js}"), ["a/*.ts", "a/*.js"]);
    assert_eq!(
        expand_braces("{a,b}/{c,d}.ts"),
        ["a/c.ts", "a/d.ts", "b/c.ts", "b/d.ts"]
    );
    assert_eq!(expand_braces("plain/*.ts"), ["plain/*.ts"]);
    assert_eq!(expand_braces("broken/{a,b"), ["broken/{a,b"]);
    for yes in [
        "src/**/*.entity.ts",
        "**/*.module.ts",
        "src/**/*.resolver.{ts,js}",
        "**/handlers/*.ts",
        "*.entity.ts",
    ] {
        assert!(is_meaningful_glob(yes), "{yes}");
    }
    for no in [
        "./src/**/*.{js,ts,jsx,tsx}",
        "./src/**/*.tsx",
        "**/*.html",
        "src/components/**/*.ts",
        "*.ts",
        "!**/*.entity.ts",
        "src/no_glob.entity.ts",
        "a b/**/*.entity.ts",
    ] {
        assert!(!is_meaningful_glob(no), "{no}");
    }
}
