//! Unit tests for the small predicates the checker is built from.

use super::super::exclusions::{
    has_generated_marker, has_keep_marker, is_config_shaped, is_declaration_file, is_test_file,
};
use super::super::mentions::{is_always_mentioned, key_of, MentionIndex};

#[test]
fn key_is_the_basename_minus_the_final_language_extension() {
    assert_eq!(key_of("src/foo.service.ts"), "foo.service");
    assert_eq!(key_of("src/useThing.ts"), "useThing");
    assert_eq!(key_of("app/stale.py"), "stale");
    assert_eq!(key_of("src/old.rs"), "old");
    assert_eq!(key_of("legacy-helper.ts"), "legacy-helper");
}

#[test]
fn index_like_basenames_are_keyed_by_the_parent_directory() {
    assert_eq!(key_of("src/billing/index.ts"), "billing");
    assert_eq!(key_of("src/billing/index.tsx"), "billing");
    assert_eq!(key_of("src/billing/mod.rs"), "billing");
    assert_eq!(key_of("src/billing/lib.rs"), "billing");
    assert_eq!(key_of("src/billing/main.rs"), "billing");
    assert_eq!(key_of("app/billing/__init__.py"), "billing");
    assert_eq!(key_of("src/billing/mod.ts"), "billing");
    // No parent directory: fall back to the file stem.
    assert_eq!(key_of("index.ts"), "index");
}

#[test]
fn short_and_generic_keys_are_always_mentioned() {
    for k in [
        "a", "db", "x", "utils", "index", "types", "config", "api", "user", "setup",
    ] {
        assert!(is_always_mentioned(k), "{k}");
    }
    for k in [
        "orphan",
        "legacy-helper",
        "foo.service",
        "migrate-users",
        "abc",
    ] {
        assert!(!is_always_mentioned(k), "{k}");
    }
    // The stoplist is compared case-insensitively.
    assert!(is_always_mentioned("Utils"));
    // A key that cannot be searched as a whole word is never reported either.
    assert!(is_always_mentioned("@scoped-thing"));
}

fn index_of(pairs: &[(&str, &str)], keys: &[&str]) -> MentionIndex {
    let files: Vec<(String, String)> = pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    MentionIndex::build(&files, &keys)
}

fn mentioners(idx: &MentionIndex, key: &str) -> Vec<usize> {
    let mut v = idx.mentioners(key).to_vec();
    v.sort_unstable();
    v
}

#[test]
fn whole_word_matching_respects_word_characters_on_both_sides() {
    let idx = index_of(
        &[
            ("a.txt", "see orphan here"),
            ("b.txt", "see orphans here"),
            ("c.txt", "see my_orphan here"),
            ("d.txt", "see $orphan here"),
            ("e.txt", "path/orphan.ts and 'orphan'"),
            ("f.txt", "ORPHAN"),
            ("g.txt", "orphan"),
        ],
        &["orphan"],
    );
    // a: yes. b: no (trailing word char). c: no (leading `_`). d: no (leading `$`).
    // e: yes (`/` and `.` are boundaries). f: yes (case-insensitive). g: yes (whole file).
    assert_eq!(mentioners(&idx, "orphan"), vec![0, 4, 5, 6]);
}

#[test]
fn keys_with_dots_and_dashes_match_across_those_characters() {
    let idx = index_of(
        &[
            ("a.yml", "run: node src/migrate-users.ts"),
            ("b.yml", "import './foo.service.js'"),
            ("c.yml", "migrate users foo service"),
            ("d.yml", "migrate-usersx foo.servicex"),
        ],
        &["migrate-users", "foo.service"],
    );
    assert_eq!(mentioners(&idx, "migrate-users"), vec![0]);
    assert_eq!(mentioners(&idx, "foo.service"), vec![1]);
}

#[test]
fn a_key_is_found_after_a_failed_partial_match_on_the_same_word() {
    // The first `foo` is followed by `bar` (no match for key `foo-baz`); the later one matches.
    let idx = index_of(&[("a.txt", "foo-bar then foo-baz")], &["foo-baz"]);
    assert_eq!(mentioners(&idx, "foo-baz"), vec![0]);
}

#[test]
fn several_keys_are_indexed_in_one_pass_and_unknown_keys_have_no_mentioners() {
    let idx = index_of(&[("a.txt", "one two")], &["one", "two"]);
    assert_eq!(mentioners(&idx, "one"), vec![0]);
    assert_eq!(mentioners(&idx, "two"), vec![0]);
    assert!(idx.mentioners("absent").is_empty());
}

#[test]
fn test_file_recognition_adds_the_pytest_and_nest_conventions() {
    for p in [
        "src/a.test.ts",
        "src/a.spec.tsx",
        "tests/x.ts",
        "src/__tests__/x.ts",
        "app/conftest.py",
        "app/test_models.py",
        "src/app.e2e-spec.ts",
        "src/__snapshots__/x.snap",
    ] {
        assert!(is_test_file(p), "{p}");
    }
    for p in ["src/testing_utils.py", "src/contest.py", "src/latest.ts"] {
        assert!(!is_test_file(p), "{p}");
    }
}

#[test]
fn declaration_files_are_recognised() {
    for p in ["a.d.ts", "src/a.d.mts", "src/a.d.cts", "stubs/x.pyi"] {
        assert!(is_declaration_file(p), "{p}");
    }
    for p in ["src/a.ts", "src/d.ts", "src/add.ts", "src/a.py"] {
        assert!(!is_declaration_file(p), "{p}");
    }
}

#[test]
fn config_shaped_paths_cover_root_level_dot_dirs_and_tool_basenames() {
    for p in [
        "top.js",
        ".eslintrc.js",
        ".storybook/main.ts",
        "pkg/.husky/pre.js",
        "vite.config.ts",
        "src/a.conf.js",
        "src/prettierrc.mjs",
        "src/jest.setup.ts",
        "src/setupTests.ts",
        "src/test-setup.ts",
        "src/a.workspace.ts",
        "src/instrumentation.ts",
        "src/middleware.ts",
        "src/proxy.ts",
        "src/service-worker.ts",
        "src/sw.ts",
        "src/manifest.ts",
        "src/reportWebVitals.ts",
        "src/polyfills.ts",
        "src/preload.ts",
        "src/env.ts",
        "src/global.ts",
        "src/globals.ts",
        "src/register.ts",
        "src/vite-env.ts",
        "src/next-env.ts",
    ] {
        assert!(is_config_shaped(p), "{p}");
    }
    for p in [
        "src/configurator.ts",
        "src/env_loader.ts",
        "src/swarm.ts",
        "src/globalize.ts",
        "src/a/b.ts",
    ] {
        assert!(!is_config_shaped(p), "{p}");
    }
}

#[test]
fn generated_marker_is_read_from_the_first_ten_lines_only() {
    for head in [
        "// @generated",
        "/* Code generated by x */",
        "# DO NOT EDIT",
        "# do not edit",
        "// auto-generated",
        "// automatically generated",
        "// AUTOGENERATED",
    ] {
        assert!(
            has_generated_marker(&format!("{head}\nconst x = 1;")),
            "{head}"
        );
    }
    assert!(has_generated_marker(&format!(
        "{}// @generated\n",
        "\n".repeat(9)
    )));
    assert!(!has_generated_marker(&format!(
        "{}// @generated\n",
        "\n".repeat(10)
    )));
    assert!(!has_generated_marker(
        "const x = 1; // regenerate with make\n"
    ));
}

#[test]
fn keep_marker_needs_text_after_the_colon_within_the_first_ten_lines() {
    assert!(has_keep_marker("// keep-file: loaded by the host\n"));
    assert!(has_keep_marker("# keep-file: x\n"));
    assert!(has_keep_marker(&format!(
        "{}// keep-file: late but ok\n",
        "\n".repeat(9)
    )));
    assert!(!has_keep_marker(&format!(
        "{}// keep-file: too late\n",
        "\n".repeat(10)
    )));
    assert!(!has_keep_marker("// keep-file:\n"));
    assert!(!has_keep_marker("// keep-file:   \n"));
    assert!(!has_keep_marker("// keep file: nope\n"));
}
