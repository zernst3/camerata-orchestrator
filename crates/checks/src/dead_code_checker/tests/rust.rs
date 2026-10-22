//! Rust: a `.rs` file is part of the build only when a `mod` declaration reaches it from a
//! crate root. The unit tests pin `rustc`'s directory-ownership and `#[path]` rules; the
//! end-to-end tests pin what the checker reports and what it leaves alone.

use super::super::rust_pass::RustPlan;
use super::*;

fn declared(pairs: Vec<(&str, &str)>, extra: &[&str]) -> Vec<String> {
    let f = files(pairs);
    let plan = RustPlan::build(&f);
    let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
    let mut v: Vec<String> = plan.declared(&extra).into_iter().collect();
    v.sort();
    v
}

// ── directory ownership and #[path] (unit) ─────────────────────────────────────

#[test]
fn a_crate_root_owns_its_directory() {
    let d = declared(
        vec![
            ("src/lib.rs", "mod a;\nmod b;\n"),
            ("src/a.rs", ""),
            ("src/b/mod.rs", ""),
            ("src/c.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/a.rs", "src/b/mod.rs", "src/lib.rs"]);
}

#[test]
fn a_plain_file_owns_a_directory_named_after_itself() {
    let d = declared(
        vec![
            ("src/lib.rs", "mod a;\n"),
            ("src/a.rs", "mod b;\n"),
            ("src/a/b.rs", ""),
            ("src/b.rs", ""),
        ],
        &[],
    );
    // `mod b;` inside `src/a.rs` loads `src/a/b.rs`, never the sibling `src/b.rs`.
    assert_eq!(d, ["src/a.rs", "src/a/b.rs", "src/lib.rs"]);
}

#[test]
fn a_mod_rs_file_owns_its_own_directory() {
    let d = declared(
        vec![
            ("src/lib.rs", "mod a;\n"),
            ("src/a/mod.rs", "mod inner;\n"),
            ("src/a/inner.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/a/inner.rs", "src/a/mod.rs", "src/lib.rs"]);
}

#[test]
fn name_dot_rs_wins_over_name_slash_mod_rs() {
    let d = declared(
        vec![
            ("src/lib.rs", "mod a;\n"),
            ("src/a.rs", ""),
            ("src/a/mod.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/a.rs", "src/lib.rs"]);
}

#[test]
fn inline_blocks_extend_the_search_directory() {
    let d = declared(
        vec![
            ("src/lib.rs", "mod a { mod b { mod c; } }\n"),
            ("src/a/b/c.rs", ""),
            ("src/c.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/a/b/c.rs", "src/lib.rs"]);
}

#[test]
fn a_path_attribute_is_relative_to_the_declaring_files_directory() {
    let d = declared(
        vec![
            ("src/lib.rs", "#[path = \"impl/real.rs\"]\nmod facade;\n"),
            ("src/impl/real.rs", ""),
            ("src/facade.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/impl/real.rs", "src/lib.rs"]);
    // In a plain file (not mod.rs) the path is still relative to the file's own directory.
    let d = declared(
        vec![
            ("src/lib.rs", "mod a;\n"),
            ("src/a.rs", "#[path = \"x.rs\"]\nmod y;\n"),
            ("src/x.rs", ""),
            ("src/a/x.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/a.rs", "src/lib.rs", "src/x.rs"]);
}

#[test]
fn a_path_attribute_inside_an_inline_block_is_relative_to_the_blocks_directory() {
    let d = declared(
        vec![
            (
                "src/lib.rs",
                "mod inl {\n    #[path = \"z.rs\"]\n    mod q;\n}\n",
            ),
            ("src/inl/z.rs", ""),
            ("src/z.rs", ""),
        ],
        &[],
    );
    assert_eq!(d, ["src/inl/z.rs", "src/lib.rs"]);
}

#[test]
fn a_declaration_naming_a_missing_file_is_ignored() {
    let d = declared(vec![("src/lib.rs", "mod ghost;\n")], &[]);
    assert_eq!(d, ["src/lib.rs"]);
}

#[test]
fn a_declaration_cycle_terminates() {
    let d = declared(
        vec![
            ("src/lib.rs", "#[path = \"lib.rs\"]\nmod again;\nmod a;\n"),
            ("src/a.rs", "#[path = \"a.rs\"]\nmod a;\n"),
        ],
        &[],
    );
    assert_eq!(d, ["src/a.rs", "src/lib.rs"]);
}

#[test]
fn cargo_target_roots_are_declared_and_own_their_directories() {
    let d = declared(
        vec![
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("src/lib.rs", ""),
            ("build.rs", ""),
            ("tests/it.rs", "mod common;\n"),
            ("tests/common/mod.rs", ""),
            ("tests/suite/main.rs", "mod helper;\n"),
            ("tests/suite/helper.rs", ""),
            ("examples/demo.rs", ""),
            ("benches/b.rs", ""),
            ("src/bin/tool.rs", "mod tool_support;\n"),
            ("src/bin/tool_support.rs", ""),
            ("src/bin/multi/main.rs", ""),
            ("tests/unrelated/helper.rs", ""),
        ],
        &[],
    );
    assert_eq!(
        d,
        [
            "benches/b.rs",
            "build.rs",
            "examples/demo.rs",
            "src/bin/multi/main.rs",
            "src/bin/tool.rs",
            "src/bin/tool_support.rs",
            "src/lib.rs",
            "tests/common/mod.rs",
            "tests/it.rs",
            "tests/suite/helper.rs",
            "tests/suite/main.rs",
        ]
    );
}

#[test]
fn cargo_path_entries_name_extra_roots() {
    let d = declared(
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\n\n[lib]\npath = \"src/engine.rs\"\n\n[[test]]\nname = \"t\"\npath = \"checks/t.rs\"\n",
            ),
            ("src/lib.rs", ""),
            ("src/engine.rs", "mod core_engine;\n"),
            ("src/core_engine.rs", ""),
            ("checks/t.rs", ""),
        ],
        &[],
    );
    assert_eq!(
        d,
        [
            "checks/t.rs",
            "src/core_engine.rs",
            "src/engine.rs",
            "src/lib.rs"
        ]
    );
}

#[test]
fn extra_roots_extend_the_closure() {
    let d = declared(
        vec![
            ("src/lib.rs", ""),
            ("src/loose.rs", "mod nested;\n"),
            ("src/loose/nested.rs", ""),
        ],
        &["src/loose.rs"],
    );
    assert_eq!(d, ["src/lib.rs", "src/loose.rs", "src/loose/nested.rs"]);
}

#[test]
fn sibling_crates_are_independent() {
    let d = declared(
        vec![
            ("crates/a/src/lib.rs", "mod one;\n"),
            ("crates/a/src/one.rs", ""),
            ("crates/b/src/lib.rs", ""),
            ("crates/b/src/one.rs", ""),
        ],
        &[],
    );
    assert_eq!(
        d,
        [
            "crates/a/src/lib.rs",
            "crates/a/src/one.rs",
            "crates/b/src/lib.rs"
        ]
    );
}

#[test]
fn crates_whose_roots_or_macros_the_checker_cannot_see_are_skipped() {
    let skipped = |pairs: Vec<(&str, &str)>, path: &str| {
        let f = files(pairs);
        !RustPlan::build(&f).is_checked(path)
    };
    // No lib.rs or main.rs in the scan.
    assert!(skipped(vec![("src/a.rs", "")], "src/a.rs"));
    // A [[bin]] target or a src/bin path in the manifest.
    assert!(skipped(
        vec![
            ("Cargo.toml", "[[bin]]\nname = \"x\"\n"),
            ("src/lib.rs", ""),
            ("src/a.rs", ""),
        ],
        "src/a.rs"
    ));
    assert!(skipped(
        vec![
            ("Cargo.toml", "[package]\nautobins = false\n# src/bin\n"),
            ("src/lib.rs", ""),
            ("src/a.rs", ""),
        ],
        "src/a.rs"
    ));
    // Macro-generated declarations.
    for dep in ["automod", "mod_use", "include_dir"] {
        let manifest = format!("[dependencies]\n{dep} = \"1\"\n");
        assert!(skipped(
            vec![
                ("Cargo.toml", manifest.as_str()),
                ("src/lib.rs", ""),
                ("src/a.rs", ""),
            ],
            "src/a.rs"
        ));
    }
    // And a plain crate is checked.
    let f = files(vec![("src/lib.rs", ""), ("src/a.rs", "")]);
    assert!(RustPlan::build(&f).is_checked("src/a.rs"));
}

// ── end to end: positives ──────────────────────────────────────────────────────

#[test]
fn rust_file_not_declared_by_any_mod_is_flagged() {
    let f = files(vec![
        ("crates/x/Cargo.toml", "[package]\nname=\"x\"\n"),
        ("crates/x/src/lib.rs", "mod a;\n"),
        ("crates/x/src/a.rs", "pub fn f() {}\n"),
        ("crates/x/src/old.rs", "pub fn g() {}\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].file, "crates/x/src/old.rs");
    assert_eq!(vs[0].line, 1);
    assert_eq!(vs[0].object.as_deref(), Some("crates/x/src/old.rs"));
    for needle in ["mod", "cargo", "old", "nowhere else"] {
        assert!(
            vs[0].message.contains(needle),
            "{needle}: {}",
            vs[0].message
        );
    }
}

#[test]
fn rust_nested_inline_mod_declaration_is_resolved() {
    let f = files(vec![
        ("src/lib.rs", "mod a { mod b; }\n"),
        ("src/a/b.rs", "pub fn f() {}\n"),
        ("src/cold.rs", "pub fn g() {}\n"),
    ]);
    assert_flags_only(&f, "src/cold.rs");
}

#[test]
fn rust_path_attribute_declares_the_named_file() {
    let f = files(vec![
        ("src/lib.rs", "#[path = \"impl/real.rs\"]\nmod real;\n"),
        ("src/impl/real.rs", "pub fn f() {}\n"),
        ("src/other.rs", "pub fn g() {}\n"),
    ]);
    assert_flags_only(&f, "src/other.rs");
}

#[test]
fn rust_cfg_test_and_feature_gated_mods_are_declared() {
    let f = files(vec![
        (
            "src/lib.rs",
            "#[cfg(test)]\nmod tests;\n#[cfg(feature = \"x\")]\nmod x;\n",
        ),
        ("src/tests.rs", "#[test] fn t() {}\n"),
        ("src/x.rs", "pub fn f() {}\n"),
        ("src/dead.rs", "pub fn g() {}\n"),
    ]);
    assert_flags_only(&f, "src/dead.rs");
}

#[test]
fn a_file_declared_only_by_an_uncompiled_file_is_dead_too_and_says_so() {
    let f = files(vec![
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/old.rs", "mod innerpart;\n"),
        ("src/old/innerpart.rs", "pub fn g() {}\n"),
    ]);
    let vs = run(&f);
    let got: Vec<&str> = vs.iter().map(|v| v.file.as_str()).collect();
    assert_eq!(got, ["src/old.rs", "src/old/innerpart.rs"], "{vs:#?}");
    let inner = &vs[1];
    assert!(
        inner.message.contains("declared only by"),
        "{}",
        inner.message
    );
    assert!(inner.message.contains("src/old.rs"), "{}", inner.message);
    assert!(inner.message.contains("cargo"), "{}", inner.message);
}

#[test]
fn a_dead_rust_folder_rolls_up() {
    let f = files(vec![
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/legacy/mod.rs", "mod alphapart;\nmod betapart;\n"),
        ("src/legacy/alphapart.rs", "pub fn a() {}\n"),
        ("src/legacy/betapart.rs", "pub fn b() {}\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("src/legacy/"));
    assert_eq!(vs[0].line, 0);
}

#[test]
fn a_mention_in_a_live_file_keeps_an_undeclared_rust_file() {
    // `include!` and `#[cfg_attr(.., path = ..)]` are literal strings: mentions.
    let f = files(vec![
        ("src/lib.rs", "include!(\"generated_tables.rs\");\n"),
        ("src/generated_tables.rs", "pub const T: u8 = 1;\n"),
        (
            "src/other_lib.rs",
            "#[cfg_attr(feature = \"x\", path = \"platform_unix.rs\")]\nmod platform;\n",
        ),
        ("src/platform_unix.rs", "pub fn f() {}\n"),
    ]);
    // `other_lib.rs` itself is undeclared, but `lib.rs` includes the tables and `other_lib`
    // is flagged; `platform_unix` is only mentioned by that dead file, so it is dead too.
    let got = flagged(&f);
    assert!(
        !got.contains(&"src/generated_tables.rs".to_string()),
        "{got:?}"
    );
    assert!(got.contains(&"src/other_lib.rs".to_string()), "{got:?}");
    assert!(got.contains(&"src/platform_unix.rs".to_string()), "{got:?}");
}

// ── end to end: safe twins ─────────────────────────────────────────────────────

#[test]
fn twin_rust_crate_without_visible_root_is_skipped() {
    let f = files(vec![
        ("crates/x/src/a_part.rs", "pub fn f() {}\n"),
        ("crates/x/src/b_part.rs", "pub fn g() {}\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_crate_with_bin_targets_is_skipped() {
    let f = files(vec![
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\n\n[[bin]]\nname = \"x\"\npath = \"src/cli.rs\"\n",
        ),
        ("src/lib.rs", "mod a_part;\n"),
        ("src/a_part.rs", "pub fn f() {}\n"),
        ("src/shared.rs", "pub fn g() {}\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_tests_examples_benches_and_build_rs_are_roots() {
    // Each file under `src/` below is declared only by a Cargo target root other than
    // lib.rs, and its name is not spelled by any live file (the declaring files are themselves
    // dead unless root detection works), so only the crate-root rules keep it alive.
    let f = files(vec![
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\n\n[[test]]\nname = \"suite\"\npath = \"src/suite/entry.rs\"\n",
        ),
        ("src/lib.rs", "pub fn f() {}\n"),
        // A Cargo [[test]] path under src/.
        ("src/suite/entry.rs", "mod harness_support;\n"),
        ("src/suite/harness_support.rs", "pub fn h() {}\n"),
        // Auto-discovered binaries: src/bin/*.rs and src/bin/*/main.rs.
        (
            "src/bin/tool.rs",
            "#[path = \"parts/body.rs\"]\nmod aliased;\nfn main() {}\n",
        ),
        ("src/bin/parts/body.rs", "pub fn b() {}\n"),
        ("src/bin/multi/main.rs", "mod multi_part;\nfn main() {}\n"),
        ("src/bin/multi/multi_part.rs", "pub fn p() {}\n"),
        // Outside src/: build script, integration tests, examples, benches.
        ("build.rs", "fn main() {}\n"),
        ("tests/it.rs", "mod common;\n"),
        ("tests/common/mod.rs", "pub fn c() {}\n"),
        ("examples/demo.rs", "fn main() {}\n"),
        ("benches/b.rs", "fn main() {}\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn positive_sibling_a_file_declared_only_by_a_non_root_dead_file_is_still_flagged() {
    let f = files(vec![
        ("src/lib.rs", "pub fn f() {}\n"),
        // Same shapes as above, but not in a root position.
        ("src/tools/tool.rs", "mod tool_support;\nfn main() {}\n"),
        ("src/tools/tool/tool_support.rs", "pub fn s() {}\n"),
    ]);
    let got = flagged(&f);
    assert_eq!(got.len(), 1, "one roll-up or per-file set: {got:?}");
    let vs = run(&f);
    let text: String = vs.iter().map(|v| v.message.clone()).collect();
    assert!(
        text.contains("tool_support") || text.contains("src/tools"),
        "{text}"
    );
}

#[test]
fn twin_rust_src_bin_files_are_roots_and_declare_their_modules() {
    let f = files(vec![
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/bin/tool.rs", "mod tool_support;\nfn main() {}\n"),
        ("src/bin/tool_support.rs", "pub fn s() {}\n"),
        ("src/bin/multi/main.rs", "mod multi_part;\nfn main() {}\n"),
        ("src/bin/multi/multi_part.rs", "pub fn p() {}\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_include_macro_is_a_mention() {
    let f = files(vec![
        ("src/lib.rs", "include!(\"generated_tables.rs\");\n"),
        ("src/generated_tables.rs", "pub const T: u8 = 1;\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_crates_using_directory_macros_are_skipped() {
    let f = files(vec![
        ("Cargo.toml", "[dependencies]\nautomod = \"1\"\n"),
        ("src/lib.rs", "automod::dir!(\"src/plugins\");\n"),
        ("src/plugins/first.rs", "pub fn f() {}\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_test_paths_generated_files_and_keep_markers_are_never_candidates() {
    let f = files(vec![
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/engine_test.rs", "fn t() {}\n"),
        (
            "src/schema_gen.rs",
            "// @generated by prost\npub struct S;\n",
        ),
        (
            "src/plugin_slot.rs",
            "// keep-file: included by the build script\n",
        ),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_file_outside_any_src_directory_is_left_alone() {
    let f = files(vec![
        ("src/lib.rs", "pub fn f() {}\n"),
        ("xtask/helper_task.rs", "fn main() {}\n"),
    ]);
    assert_silent(&f);
}

#[test]
fn twin_rust_use_without_mod_does_not_compile_a_file() {
    // `use crate::ghost::T;` with no `mod ghost;` is not a build edge. The file is only kept
    // when its name is mentioned by a live file, and here the `use` line is exactly that.
    let f = files(vec![
        ("src/lib.rs", "use crate::ghostmod::T;\n"),
        ("src/ghostmod.rs", "pub struct T;\n"),
    ]);
    assert_silent(&f);
}
