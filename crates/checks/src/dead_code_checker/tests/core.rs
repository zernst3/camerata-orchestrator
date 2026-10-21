//! The core algorithm: candidates, roots, the mention guard, reachability, emission and the
//! directory roll-up, plus the guards that need no manifest, framework or language pass.

use super::*;

// ── positives ──────────────────────────────────────────────────────────────────

#[test]
fn ts_file_with_no_importer_and_no_mention_is_flagged() {
    let f = files(vec![
        ("package.json", r#"{"private":true,"main":"src/app.ts"}"#),
        ("src/app.ts", "import './used';\n"),
        ("src/used.ts", "export const used = 1;\n"),
        ("src/orphan.ts", "export const x = 1;\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.file, "src/orphan.ts");
    assert_eq!(v.line, 1);
    assert_eq!(v.object.as_deref(), Some("src/orphan.ts"));
    for needle in ["orphan", "nowhere else", "keep-file:"] {
        assert!(v.message.contains(needle), "{needle}: {}", v.message);
    }
}

#[test]
fn python_module_with_no_importer_and_no_mention_is_flagged() {
    let f = files(vec![
        ("app/__init__.py", ""),
        ("app/main.py", "from . import api\n"),
        ("app/api.py", "x = 1\n"),
        ("app/stale.py", "y = 2\n"),
    ]);
    assert_flags_only(&f, "app/stale.py");
}

#[test]
fn file_reachable_only_through_dead_files_is_listed_with_its_importer() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/legacy.ts", "import './legacy-helper';\n"),
        ("src/legacy-helper.ts", "export const h = 1;\n"),
    ]);
    let vs = run(&f);
    let got: Vec<&str> = vs.iter().map(|v| v.file.as_str()).collect();
    assert_eq!(got, ["src/legacy-helper.ts", "src/legacy.ts"], "{vs:#?}");
    let helper = &vs[0];
    assert!(
        helper.message.contains("imported only by"),
        "{}",
        helper.message
    );
    assert!(
        helper.message.contains("src/legacy.ts"),
        "{}",
        helper.message
    );
    assert!(
        helper.message.contains("itself unreachable"),
        "{}",
        helper.message
    );
    // The importer has no importer and nothing mentions it: the plain form.
    assert!(vs[1].message.contains("nowhere else"), "{}", vs[1].message);
}

#[test]
fn name_mentioned_only_in_a_dead_file_is_still_dead_and_says_so() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/orphan.ts", "// see also zombie-config\nexport {};\n"),
        ("src/zombie-config.ts", "export const z = 1;\n"),
    ]);
    let vs = run(&f);
    let zombie = vs
        .iter()
        .find(|v| v.file == "src/zombie-config.ts")
        .expect("zombie-config is flagged");
    assert!(
        zombie.message.contains("src/orphan.ts"),
        "{}",
        zombie.message
    );
    assert!(
        !zombie.message.contains("nowhere else"),
        "the name does appear elsewhere, so the message must not say it does not: {}",
        zombie.message
    );
}

#[test]
fn whole_dead_directory_rolls_up_to_one_finding() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        (
            "src/old/index.ts",
            "export * from './alpha';\nexport * from './beta';\n",
        ),
        ("src/old/alpha.ts", "export const a = 1;\n"),
        ("src/old/beta.ts", "export const b = 1;\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.object.as_deref(), Some("src/old/"));
    assert_eq!(v.line, 0);
    assert_eq!(v.file, "src/old/alpha.ts");
    for needle in ["src/old/index.ts", "src/old/alpha.ts", "src/old/beta.ts"] {
        assert!(v.message.contains(needle), "{needle}: {}", v.message);
    }
    assert!(v.message.contains("Delete the folder"), "{}", v.message);
}

#[test]
fn roll_up_lists_a_few_files_and_counts_the_rest() {
    let mut pairs = vec![("src/app.ts", "export {};\n".to_string())];
    let names: Vec<String> = (0..6).map(|i| format!("src/legacy/mod{i}x.ts")).collect();
    for n in &names {
        pairs.push((n.as_str(), "export const v = 1;\n".to_string()));
    }
    let f: Vec<(String, String)> = pairs.into_iter().map(|(p, c)| (p.to_string(), c)).collect();
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert!(vs[0].message.contains("6 files"), "{}", vs[0].message);
    assert!(vs[0].message.contains("and 3 more"), "{}", vs[0].message);
}

#[test]
fn roll_up_takes_the_outermost_eligible_directory() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/old/alpha.ts", "export const a = 1;\n"),
        ("src/old/inner/beta.ts", "export const b = 1;\n"),
        ("src/old/inner/gamma.ts", "export const c = 1;\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("src/old/"));
}

#[test]
fn dead_file_in_a_live_directory_is_reported_alone() {
    let f = files(vec![
        ("src/app.ts", "import './lib/live';\n"),
        ("src/lib/live.ts", "export const live = 1;\n"),
        ("src/lib/dead.ts", "export const dead = 1;\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].file, "src/lib/dead.ts");
    assert_eq!(vs[0].line, 1);
    assert_eq!(vs[0].object.as_deref(), Some("src/lib/dead.ts"));
}

#[test]
fn single_dead_file_in_a_directory_is_not_a_roll_up() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/old/only.ts", "export const o = 1;\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("src/old/only.ts"));
}

#[test]
fn a_test_file_in_the_directory_blocks_the_roll_up_so_the_message_stays_true() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/old/alpha.ts", "export const a = 1;\n"),
        ("src/old/beta.ts", "export const b = 1;\n"),
        ("src/old/keepme.test.ts", "it('x', () => {});\n"),
    ]);
    let got = flagged(&f);
    assert_eq!(got, ["src/old/alpha.ts", "src/old/beta.ts"]);
}

#[test]
fn findings_are_sorted_and_stable() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/zzz/zulu.ts", "export const z = 1;\n"),
        ("src/mmm/mike.ts", "export const m = 1;\n"),
        ("src/aaa/alfa.ts", "export const a = 1;\n"),
    ]);
    let first = flagged(&f);
    assert_eq!(
        first,
        ["src/aaa/alfa.ts", "src/mmm/mike.ts", "src/zzz/zulu.ts"]
    );
    assert_eq!(first, flagged(&f), "a second run gives the same order");
}

// ── G1: tests, fixtures, mocks, stories, e2e ───────────────────────────────────

#[test]
fn twin_file_imported_only_by_a_test_is_live() {
    let f = files(vec![
        ("src/helper.ts", "export const helper = 1;\n"),
        ("src/helper.test.ts", "import './helper';\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_test_fixture_and_mock_files_are_never_candidates() {
    let f = files(vec![
        ("__mocks__/fs.ts", "export {};\n"),
        ("fixtures/a.ts", "export {};\n"),
        ("e2e/x.ts", "export {};\n"),
        ("stories/b.stories.tsx", "export {};\n"),
        ("cypress/support.ts", "export {};\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_pytest_conventions_are_never_candidates() {
    let f = files(vec![
        ("app/__init__.py", ""),
        ("app/conftest.py", "import pytest\n"),
        ("app/test_models_extra.py", "def test_x(): pass\n"),
        ("app/helpers_test.py", "def test_y(): pass\n"),
        ("src/app.e2e-spec.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_non_test_python_name_is_still_flagged() {
    let f = files(vec![
        ("app/__init__.py", ""),
        ("app/testing_utils_extra.py", "x = 1\n"),
    ]);
    assert_flags_only(&f, "app/testing_utils_extra.py");
}

// ── G2: declaration files ──────────────────────────────────────────────────────

#[test]
fn twin_declaration_file_is_never_a_candidate() {
    let f = files(vec![
        ("src/types/global.d.ts", "declare const x: number;\n"),
        ("src/types/legacy.d.mts", "export {};\n"),
        ("src/types/legacy.d.cts", "export {};\n"),
        ("stubs/third_party_stub.pyi", "def f() -> int: ...\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

// ── G3: root-level, dot-dir and config-shaped ──────────────────────────────────

#[test]
fn twin_root_level_and_dot_dir_files_are_never_candidates() {
    let f = files(vec![
        ("tailwind.config.ts", "export default {};\n"),
        (".storybook/main.ts", "export default {};\n"),
        (".eslintrc.js", "module.exports = {};\n"),
        ("rootlevel_tool.js", "console.log(1);\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_config_shaped_basenames_anywhere_are_never_candidates() {
    let f = files(vec![
        ("src/jest.setup.ts", "export {};\n"),
        ("apps/web/vitest.config.ts", "export default {};\n"),
        ("src/middleware.ts", "export {};\n"),
        ("src/instrumentation.ts", "export {};\n"),
        ("src/setupTests.ts", "export {};\n"),
        ("src/service-worker.ts", "export {};\n"),
        ("src/babel-plugin.rc.js", "module.exports = {};\n"),
        ("src/lintrc.cjs", "module.exports = {};\n"),
        ("src/polyfills.ts", "export {};\n"),
        ("src/vite-env.ts", "export {};\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_a_near_miss_config_name_is_still_flagged() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/configurator.ts", "export const c = 1;\n"),
    ]);
    assert_flags_only(&f, "src/configurator.ts");
}

// ── G8: the mention guard ──────────────────────────────────────────────────────

#[test]
fn twin_mention_in_manifest_or_ci_keeps_file_live() {
    let f = files(vec![
        (
            ".github/workflows/ci.yml",
            "run: node src/migrate-users.ts\n",
        ),
        ("src/migrate-users.ts", "export {};\n"),
        ("Dockerfile", "CMD [\"node\",\"src/worker.js\"]\n"),
        ("src/worker.js", "console.log(1);\n"),
        ("serverless.yml", "handler: src/handlers/orders.handler\n"),
        ("src/handlers/orders.ts", "export const handler = 1;\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_html_script_tag_is_a_mention() {
    let f = files(vec![
        (
            "index.html",
            "<script type=\"module\" src=\"/src/bootstrap.tsx\"></script>\n",
        ),
        ("src/bootstrap.tsx", "export {};\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_vue_sfc_import_is_a_mention() {
    let f = files(vec![
        (
            "src/App.vue",
            "<script setup>\nimport { useThing } from './composables/useThing'\n</script>\n",
        ),
        (
            "src/composables/useThing.ts",
            "export const useThing = 1;\n",
        ),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_unresolvable_alias_import_is_a_mention() {
    let f = files(vec![
        (
            "webpack.config.js",
            "module.exports = { resolve: { alias: { '@lib': path.resolve('src/lib') } } };\n",
        ),
        ("src/app.ts", "import { x } from '@lib/orders';\n"),
        ("src/lib/orders.ts", "export const x = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_nodenext_js_suffix_import_is_a_mention() {
    let f = files(vec![
        ("src/app.ts", "import './orders.js';\n"),
        ("src/orders.ts", "export const o = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_python_ambiguous_absolute_import_is_a_mention() {
    // The resolver drops this edge (two files match the dotted path), so only the mention
    // guard keeps both copies alive.
    let f = files(vec![
        ("app/main.py", "import services.orders\n"),
        ("app/services/orders.py", "x = 1\n"),
        ("vendor_copy/services/orders.py", "x = 2\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_literal_dynamic_import_is_an_edge() {
    let f = files(vec![
        ("src/app.ts", "const m = await import('./lazy');\n"),
        ("src/lazy.ts", "export const lazy = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn mention_match_is_case_insensitive_and_whole_word() {
    let live = files(vec![
        ("src/app.ts", "// uses OrderService somewhere\n"),
        ("src/orderservice.ts", "export {};\n"),
    ]);
    assert_silent(&live);
    // `orders` is only a prefix of the word `ordersheet`: not a mention.
    let dead = files(vec![
        ("src/app.ts", "// ordersheet\n"),
        ("src/orders.ts", "export {};\n"),
    ]);
    assert_flags_only(&dead, "src/orders.ts");
}

#[test]
fn a_file_mentioning_its_own_name_is_not_kept_alive_by_it() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        (
            "src/selfish.ts",
            "// selfish: a note about selfish\nexport {};\n",
        ),
    ]);
    assert_flags_only(&f, "src/selfish.ts");
}

#[test]
fn twin_short_or_stoplisted_key_is_never_flagged() {
    let f = files(vec![
        ("src/db.ts", "export {};\n"),
        ("src/utils.ts", "export {};\n"),
        ("src/x.ts", "export {};\n"),
        ("src/helpers.ts", "export {};\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn index_files_are_keyed_by_their_directory_name() {
    // `src/billing/index.ts` is named `billing`: `import './billing'` mentions it.
    let live = files(vec![
        ("src/app.ts", "import './billing';\n"),
        ("src/billing/index.ts", "export {};\n"),
    ]);
    assert_silent(&live);
    let dead = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/billing/index.ts", "export {};\n"),
    ]);
    assert_flags_only(&dead, "src/billing/index.ts");
}

#[test]
fn twin_homonymous_stem_elsewhere_keeps_file_live() {
    let f = files(vec![
        ("a/orders.ts", "export const a = 1;\n"),
        ("b/orders.ts", "export const b = 1;\n"),
        ("b/app.ts", "import './orders';\n"),
    ]);
    assert_silent(&f);
}

// ── G11, G12, G17 ──────────────────────────────────────────────────────────────

#[test]
fn twin_generated_marker_file_is_never_a_candidate() {
    let f = files(vec![
        ("src/schema.ts", "// @generated by codegen\nexport {};\n"),
        (
            "src/models_pb.ts",
            "/* Code generated by protoc. DO NOT EDIT. */\n",
        ),
        (
            "src/tables.ts",
            "// This file is auto-generated\nexport {};\n",
        ),
        ("app/gen.py", "# do not edit: written by the schema tool\n"),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_generated_marker_after_line_ten_does_not_count() {
    let mut body = String::new();
    for _ in 0..10 {
        body.push_str("// filler\n");
    }
    body.push_str("// @generated\nexport {};\n");
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/lateflag.ts", body.as_str()),
    ]);
    assert_flags_only(&f, "src/lateflag.ts");
}

#[test]
fn twin_keep_file_marker_is_honoured() {
    let f = files(vec![
        (
            "src/plugin.ts",
            "// keep-file: loaded by name from the host app\nexport {};\n",
        ),
        (
            "app/hook.py",
            "# keep-file: imported by the deploy tool\nx = 1\n",
        ),
        ("src/app.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_empty_keep_file_marker_is_not_a_reason() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/plugin.ts", "// keep-file:\nexport {};\n"),
    ]);
    assert_flags_only(&f, "src/plugin.ts");
}

#[test]
fn twin_emitted_js_beside_ts_is_ignored() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/orphan.ts", "export const o = 1;\n"),
        ("src/orphan.js", "exports.o = 1;\n"),
    ]);
    let vs = run(&f);
    let got: Vec<&str> = vs.iter().map(|v| v.file.as_str()).collect();
    assert_eq!(got, ["src/orphan.ts"], "{vs:#?}");
    assert!(
        vs.iter().all(|v| !v.message.contains("orphan.js")),
        "{vs:#?}"
    );
}

#[test]
fn emitted_js_does_not_keep_other_files_alive() {
    // The compiled copy imports `./extra`, but a build-output copy is not an importer.
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/orphan.ts", "export {};\n"),
        ("src/orphan.js", "require('./extra');\n"),
        ("src/extra.ts", "export {};\n"),
    ]);
    let got = flagged(&f);
    assert_eq!(got, ["src/extra.ts", "src/orphan.ts"]);
}

// ── G13 to G15, G21, G27: edges that must stay edges ───────────────────────────

#[test]
fn twin_type_only_import_keeps_types_file_live() {
    let f = files(vec![
        ("src/app.ts", "import type { T } from './types/order';\n"),
        ("src/types/order.ts", "export type T = number;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_side_effect_import_keeps_file_live() {
    let f = files(vec![
        ("src/app.ts", "import './polyfills/intl';\n"),
        ("src/polyfills/intl.ts", "export {};\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_star_re_export_chain_keeps_origin_live() {
    let f = files(vec![
        ("src/app.ts", "import { a } from './lib';\n"),
        ("src/lib/index.ts", "export * from './alpha';\n"),
        ("src/lib/alpha.ts", "export const a = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_two_way_dead_cycle_is_reported_once_each_without_looping() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/alpha.ts", "import './bravo';\n"),
        ("src/bravo.ts", "import './alpha';\n"),
    ]);
    let vs = run(&f);
    // Two dead files sharing one folder with the live `app.ts`: two per-file findings.
    let got: Vec<&str> = vs.iter().map(|v| v.file.as_str()).collect();
    assert_eq!(got, ["src/alpha.ts", "src/bravo.ts"], "{vs:#?}");
    assert!(
        vs[0].message.contains("imported only by"),
        "{}",
        vs[0].message
    );
    assert!(vs[0].message.contains("src/bravo.ts"), "{}", vs[0].message);
}

// ── input robustness and applicability ─────────────────────────────────────────

#[test]
fn twin_empty_and_malformed_inputs_never_panic() {
    assert_silent(&[]);
    let f = files(vec![
        ("package.json", "{ not json"),
        ("Cargo.toml", "[[["),
        ("pyproject.toml", "= = ="),
        ("src/empty.rs", ""),
        (
            "src/app.ts",
            "\u{0}\u{1}\u{FFFD} not code at all {{{ import(",
        ),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_non_source_repo_has_no_candidates() {
    let f = files(vec![
        ("src/Main.java", "class Main {}\n"),
        ("config/app.yml", "a: 1\n"),
    ]);
    assert_silent(&f);
    assert!(DeadCodeChecker.config_unsatisfied_for(&RepoView {
        spec: "t/r",
        files: &f,
    }));
}

#[test]
fn a_repo_with_source_files_is_not_config_unsatisfied() {
    for path in [
        "a.ts", "a.tsx", "a.js", "a.jsx", "a.mjs", "a.cjs", "a.py", "a.rs",
    ] {
        let f = files(vec![(path, "")]);
        assert!(
            !DeadCodeChecker.config_unsatisfied_for(&RepoView {
                spec: "t/r",
                files: &f,
            }),
            "{path}"
        );
    }
}

#[test]
fn registers_its_rule_id_and_source_plus_context_globs() {
    assert_eq!(DeadCodeChecker.rule_ids(), [RULE_DEAD_CODE]);
    let globs = DeadCodeChecker.interest_globs();
    for needle in [
        "**/*.ts",
        "**/*.py",
        "**/*.rs",
        "**/*.json",
        "**/*.html",
        "**/*.vue",
        "**/*.md",
        "**/Dockerfile*",
        "**/Makefile",
    ] {
        assert!(globs.contains(&needle), "{needle} missing from {globs:?}");
    }
}
