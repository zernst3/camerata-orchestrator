//! Framework, tool and language conventions that load a file without importing it
//! (`G4` to `G6`). Every manifest here carries `"private": true`: a public package would root
//! all of its files (`R4`) and make these tests vacuous.

use super::*;

fn private_with(deps: &str) -> String {
    format!(r#"{{"private":true,"dependencies":{{{deps}}}}}"#)
}

// ── G4: file-system routers ────────────────────────────────────────────────────

#[test]
fn twin_next_pages_and_app_router_conventions_are_live() {
    let manifest = private_with(r#""next":"15""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("pages/about.tsx", "export default 1;\n"),
        ("pages/api/orders.ts", "export default 1;\n"),
        ("src/pages/contact.tsx", "export default 1;\n"),
        ("app/dashboard/page.tsx", "export default 1;\n"),
        ("app/dashboard/layout.tsx", "export default 1;\n"),
        ("app/dashboard/loading.tsx", "export default 1;\n"),
        ("app/dashboard/not-found.tsx", "export default 1;\n"),
        ("app/api/orders/route.ts", "export const GET = 1;\n"),
        ("app/opengraph-image.tsx", "export default 1;\n"),
        ("src/app/settings/page.tsx", "export default 1;\n"),
        ("middleware.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn next_app_dir_non_convention_file_is_still_a_candidate() {
    let manifest = private_with(r#""next":"15""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("app/dashboard/page.tsx", "export default 1;\n"),
        ("app/dashboard/_old.tsx", "export const old = 1;\n"),
    ]);
    assert_flags_only(&f, "app/dashboard/_old.tsx");
}

#[test]
fn next_conventions_are_detected_by_config_file_too() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("next.config.js", "module.exports = {};\n"),
        ("pages/about.tsx", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_pages_directory_without_a_router_is_still_checked() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("pages/stranded.tsx", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "pages/stranded.tsx");
}

#[test]
fn router_detection_is_scoped_to_the_package_that_uses_it() {
    let next = private_with(r#""next":"15""#);
    let f = files(vec![
        ("apps/web/package.json", next.as_str()),
        ("apps/web/pages/about.tsx", "export default 1;\n"),
        ("apps/api/package.json", r#"{"private":true}"#),
        ("apps/api/pages/stranded.tsx", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "apps/api/pages/stranded.tsx");
}

#[test]
fn twin_remix_routes_root_and_entry_are_live() {
    let manifest = private_with(r#""@remix-run/react":"2""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("app/root.tsx", "export default 1;\n"),
        ("app/routes/_index.tsx", "export default 1;\n"),
        ("app/entry.client.tsx", "export {};\n"),
        ("app/entry.server.tsx", "export {};\n"),
        ("app/routes.ts", "export default [];\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn react_router_framework_mode_needs_its_root_file() {
    let manifest = private_with(r#""react-router":"7""#);
    let with_root = files(vec![
        ("package.json", manifest.as_str()),
        ("app/root.tsx", "export default 1;\n"),
        ("app/routes/home.tsx", "export default 1;\n"),
    ]);
    assert_silent(&with_root);
    let without_root = files(vec![
        ("package.json", manifest.as_str()),
        ("app/routes/stranded.tsx", "export default 1;\n"),
    ]);
    assert_flags_only(&without_root, "app/routes/stranded.tsx");
}

#[test]
fn twin_sveltekit_routes_and_hooks_are_live() {
    let manifest = private_with(r#""@sveltejs/kit":"2""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("src/routes/+page.svelte", "<h1>hi</h1>\n"),
        ("src/routes/+page.ts", "export const load = 1;\n"),
        ("src/routes/api/+server.ts", "export const GET = 1;\n"),
        ("src/hooks.server.ts", "export {};\n"),
        ("src/hooks/auth.ts", "export {};\n"),
        ("src/params/id.ts", "export const match = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn sveltekit_lib_server_is_ordinary_code() {
    let manifest = private_with(r#""@sveltejs/kit":"2""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("src/routes/+page.svelte", "<h1>hi</h1>\n"),
        ("src/lib/server/stranded.ts", "export const s = 1;\n"),
    ]);
    assert_flags_only(&f, "src/lib/server/stranded.ts");
}

#[test]
fn twin_nuxt_autoload_dirs_are_live() {
    let f = files(vec![
        ("nuxt.config.ts", "export default {};\n"),
        ("composables/useThing.ts", "export const useThing = 1;\n"),
        ("components/Thing.ts", "export default 1;\n"),
        ("server/api/orders.ts", "export default 1;\n"),
        ("server/routes/health.ts", "export default 1;\n"),
        ("server/middleware/log.ts", "export default 1;\n"),
        ("server/plugins/boot.ts", "export default 1;\n"),
        ("server/utils/format.ts", "export const format = 1;\n"),
        ("middleware/auth.ts", "export default 1;\n"),
        ("plugins/x.ts", "export default 1;\n"),
        ("layouts/default.ts", "export default 1;\n"),
        ("stores/cart.ts", "export const cart = 1;\n"),
        ("utils/money.ts", "export const money = 1;\n"),
        ("app.ts", "export default 1;\n"),
        ("error.ts", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn nuxt_four_app_directory_is_used_when_app_vue_exists() {
    let manifest = private_with(r#""nuxt":"4""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("app/app.vue", "<template><div/></template>\n"),
        (
            "app/composables/useThing.ts",
            "export const useThing = 1;\n",
        ),
        ("composables/stranded.ts", "export const stranded = 1;\n"),
    ]);
    // `composables/` at the project root is not the app directory when `app/app.vue` exists.
    assert_flags_only(&f, "composables/stranded.ts");
}

#[test]
fn twin_astro_pages_and_content_config_are_live() {
    let manifest = private_with(r#""astro":"4""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("src/pages/index.astro", "---\n---\n<h1>hi</h1>\n"),
        ("src/pages/api/health.ts", "export const GET = 1;\n"),
        ("src/content/config.ts", "export const collections = {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_expo_router_app_dir_is_live() {
    let manifest = private_with(r#""expo-router":"3""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("app/(tabs)/index.tsx", "export default 1;\n"),
        ("app/(tabs)/profile.tsx", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_gatsby_and_docusaurus_conventions_are_live() {
    let gatsby = private_with(r#""gatsby":"5""#);
    let f = files(vec![
        ("package.json", gatsby.as_str()),
        ("src/pages/about.tsx", "export default 1;\n"),
        ("src/templates/post.tsx", "export default 1;\n"),
    ]);
    assert_silent(&f);
    let docusaurus = private_with(r#""@docusaurus/core":"3""#);
    let f = files(vec![
        ("package.json", docusaurus.as_str()),
        ("src/pages/landing.tsx", "export default 1;\n"),
        ("src/theme/Footer/index.tsx", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_netlify_and_vercel_function_folders_are_live() {
    let f = files(vec![
        ("vercel.json", "{}\n"),
        ("api/hello.ts", "export default 1;\n"),
        ("netlify/functions/hello.ts", "export const handler = 1;\n"),
        ("netlify/edge-functions/geo.ts", "export default 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_api_directory_without_vercel_json_is_still_checked() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("api/stranded.ts", "export default 1;\n"),
    ]);
    assert_flags_only(&f, "api/stranded.ts");
}

#[test]
fn cloud_function_folders_need_their_platform_file() {
    let f = files(vec![
        ("firebase.json", "{}\n"),
        ("functions/index_handler.ts", "export const h = 1;\n"),
    ]);
    assert_silent(&f);
    let bare = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("functions/stranded.ts", "export const h = 1;\n"),
    ]);
    assert_flags_only(&bare, "functions/stranded.ts");
}

// ── G5: folders people or tools run by hand ────────────────────────────────────

#[test]
fn twin_scripts_directory_is_run_by_hand_and_never_a_candidate() {
    let f = files(vec![
        ("scripts/build.mjs", "export {};\n"),
        ("package.json", r#"{"private":true}"#),
        ("tools/codegen/run.ts", "export {};\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_tool_and_data_folders_are_never_candidates() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("config/loader.ts", "export {};\n"),
        ("docs/example_snippet.ts", "export {};\n"),
        ("examples/demo_app.ts", "export {};\n"),
        ("example/demo_two.ts", "export {};\n"),
        ("samples/sample_one.ts", "export {};\n"),
        ("benchmarks/bench_one.ts", "export {};\n"),
        ("migrations/001_init.ts", "export {};\n"),
        ("seeds/seed_users.ts", "export {};\n"),
        ("db/connect_extra.ts", "export {};\n"),
        ("prisma/seed_extra.ts", "export {};\n"),
        ("supabase/functions_extra.ts", "export {};\n"),
        ("playwright/fixtures_extra.ts", "export {};\n"),
        ("public/sw_extra.js", "export {};\n"),
        ("static/embed_extra.js", "export {};\n"),
        ("assets/loader_extra.js", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn tool_folders_are_relative_to_the_nearest_package() {
    let f = files(vec![
        ("packages/ui/package.json", r#"{"private":true}"#),
        ("packages/ui/scripts/build_ui.ts", "export {};\n"),
        // `scripts` nested deeper than the package root is ordinary code.
        ("packages/ui/src/scripts/stranded.ts", "export {};\n"),
    ]);
    assert_flags_only(&f, "packages/ui/src/scripts/stranded.ts");
}

#[test]
fn rust_crate_examples_folder_is_never_a_candidate() {
    let f = files(vec![
        ("crates/x/Cargo.toml", "[package]\nname = \"x\"\n"),
        ("crates/x/src/lib.rs", ""),
        ("crates/x/examples/helper_extra.rs", "pub fn f() {}\n"),
    ]);
    assert_silent(&f);
}

// ── G6: Python conventions ─────────────────────────────────────────────────────

#[test]
fn twin_python_dunder_all_string_is_a_mention() {
    let f = files(vec![
        ("pkg/__init__.py", "__all__ = [\"legacy\"]\n"),
        ("pkg/legacy.py", "x = 1\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_python_init_re_export_surface_keeps_submodules_live() {
    let f = files(vec![
        (
            "pkg/__init__.py",
            "from .engine import Engine\nfrom . import helpers_extra\n",
        ),
        ("pkg/engine.py", "class Engine: pass\n"),
        ("pkg/helpers_extra.py", "x = 1\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_django_autodiscovered_modules_are_live() {
    let f = files(vec![
        ("shop/models.py", "x = 1\n"),
        ("shop/admin.py", "x = 1\n"),
        ("shop/apps.py", "x = 1\n"),
        ("shop/migrations/0001_initial.py", "x = 1\n"),
        ("shop/management/commands/seed.py", "x = 1\n"),
        ("shop/templatetags/money.py", "x = 1\n"),
        ("shop/tasks.py", "x = 1\n"),
        ("shop/forms.py", "x = 1\n"),
        ("shop/views.py", "x = 1\n"),
        ("shop/serializers.py", "x = 1\n"),
        ("config/settings/prod.py", "x = 1\n"),
        ("config/urls.py", "x = 1\n"),
        ("config/wsgi.py", "x = 1\n"),
        ("config/asgi.py", "x = 1\n"),
        ("manage.py", "x = 1\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn django_signals_module_nobody_imports_is_flagged() {
    let f = files(vec![
        ("shop/models.py", "x = 1\n"),
        ("shop/apps.py", "x = 1\n"),
        ("shop/signals.py", "x = 1\n"),
    ]);
    assert_flags_only(&f, "shop/signals.py");
}

#[test]
fn twin_airflow_scrapy_alembic_and_fixture_folders_are_live() {
    let f = files(vec![
        ("pipelines/dags/nightly_load.py", "x = 1\n"),
        ("crawl/spiders/quotes_spider.py", "x = 1\n"),
        ("db/alembic/versions/abc123_add_users.py", "x = 1\n"),
        ("app/fixtures/sample_orders.py", "x = 1\n"),
        ("tools/locustfile.py", "x = 1\n"),
        ("noxfile.py", "x = 1\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn python_conventions_do_not_apply_to_other_languages() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/models.ts", "export {};\n"),
        ("src/commands/stranded.ts", "export {};\n"),
    ]);
    // `models` is a stoplisted name; the `commands` folder is only a Python convention.
    assert_flags_only(&f, "src/commands/stranded.ts");
}

// ── tool folders at any depth ──────────────────────────────────────────────────

#[test]
fn migration_seed_and_deploy_folders_are_never_candidates_at_any_depth_in_any_language() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/app.ts", "export {};\n"),
        // TypeORM, Adonis, Knex, Hardhat-deploy layouts: loaded by a CLI by glob.
        (
            "src/migrations/1700000000001-AddOrders.ts",
            "export class A {}\n",
        ),
        (
            "database/migrations/1700000000002_create_posts_table.ts",
            "export {};\n",
        ),
        ("src/database/seeders/userSeeder.ts", "export {};\n"),
        ("apps/api/src/db/seeds/initial_rows.ts", "export {};\n"),
        ("deploy/001_deploy_token.ts", "export {};\n"),
        ("contracts/deploy/002_deploy_vault.ts", "export {};\n"),
        (
            "services/billing/migrations/0001_initial_schema.js",
            "exports.up = () => {};\n",
        ),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_a_folder_that_merely_contains_the_word_is_still_checked() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("src/app.ts", "export {};\n"),
        ("src/migrations_helpers/stranded_helper.ts", "export {};\n"),
        ("src/deployment/stranded_deployer.ts", "export {};\n"),
    ]);
    let got = flagged(&f);
    assert_eq!(got.len(), 2, "{got:?}");
}

#[test]
fn ember_app_directory_is_loaded_by_the_resolver() {
    for dep in ["ember-source", "ember-cli"] {
        let manifest = format!(r#"{{"private":true,"devDependencies":{{"{dep}":"5"}}}}"#);
        let f = files(vec![
            ("package.json", manifest.as_str()),
            ("app/routes/billing-history.js", "export default class {}\n"),
            (
                "app/services/session-tracker.js",
                "export default class {}\n",
            ),
            ("app/components/user-card.js", "export default class {}\n"),
            ("app/components/user-card.hbs", "<div/>\n"),
        ]);
        assert_silent(&f);
    }
}

#[test]
fn positive_sibling_an_app_directory_without_ember_is_still_checked() {
    let f = files(vec![
        ("package.json", r#"{"private":true}"#),
        ("app/routes/billing-history.js", "export default class {}\n"),
    ]);
    assert_flags_only(&f, "app/routes/billing-history.js");
}

#[test]
fn next_src_layout_special_files_are_never_candidates() {
    let manifest = private_with(r#""next":"15""#);
    let f = files(vec![
        ("package.json", manifest.as_str()),
        ("next.config.mjs", "export default {};\n"),
        ("src/middleware.ts", "export {};\n"),
        ("src/proxy.ts", "export {};\n"),
        ("src/instrumentation.ts", "export {};\n"),
        ("src/instrumentation-client.ts", "export {};\n"),
        ("src/mdx-components.tsx", "export const useMDXComponents = () => ({});\n"),
        ("src/app/layout.tsx", "export default 1;\n"),
        ("src/app/page.tsx", "export default 1;\n"),
    ]);
    assert_silent(&f);
}
