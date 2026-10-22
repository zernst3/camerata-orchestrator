//! Tokenization per dialect: the normalization table (spec section 2), the excluded items and
//! the positions.

use super::*;

/// `class:text` of every token of every item, Exact mode.
fn exact(lang: SourceLang, source: &str) -> Vec<String> {
    tokens(lang, source, TokenMode::Exact)
        .iter()
        .flat_map(|item| {
            item.tokens
                .iter()
                .zip(&item.texts)
                .map(|(t, text)| format!("{:?}:{text}", t.class))
        })
        .collect()
}

/// The Structural ids of every token, flattened across items.
fn structural_ids(lang: SourceLang, source: &str) -> Vec<u32> {
    tokens(lang, source, TokenMode::Structural)
        .iter()
        .flat_map(|item| item.tokens.iter().map(|t| t.id))
        .collect()
}

fn item_count(lang: SourceLang, source: &str) -> usize {
    tokens(lang, source, TokenMode::Structural).len()
}

#[test]
fn structural_collapses_identifiers_properties_literals_and_operator_families() {
    let src = "function f(a) { return a.b + 1 ? 'x' : null; }\n";
    let other = "function g(zz) { return zz.qq - 7 ? 'yy' : undefined; }\n";
    assert_eq!(
        structural_ids(SourceLang::JavaScript, src),
        structural_ids(SourceLang::JavaScript, other)
    );
}

#[test]
fn exact_keeps_property_names_literal_values_and_operator_text() {
    let src = "function f(a) { return a.total * 0.12 + 'x'; }\n";
    let stream = exact(SourceLang::JavaScript, src);
    for expected in [
        "Prop:total",
        "Num:0.12",
        "BinOp:*",
        "BinOp:+",
        "Str:x",
        "Ident:$ID",
    ] {
        assert!(
            stream.iter().any(|t| t == expected),
            "{expected} in {stream:?}"
        );
    }
}

#[test]
fn free_identifiers_stay_collapsed_in_exact_mode() {
    let a = exact(
        SourceLang::JavaScript,
        "function f(alpha) { return alpha; }\n",
    );
    let b = exact(
        SourceLang::JavaScript,
        "function g(beta) { return beta; }\n",
    );
    assert_eq!(a, b);
}

#[test]
fn binary_operator_families_collapse_in_structural_and_differ_in_exact() {
    let nullish = "function f(a, b) { return a.title ?? b; }\n";
    let or = "function f(a, b) { return a.title || b; }\n";
    assert_eq!(
        structural_ids(SourceLang::JavaScript, nullish),
        structural_ids(SourceLang::JavaScript, or)
    );
    assert_ne!(
        exact(SourceLang::JavaScript, nullish),
        exact(SourceLang::JavaScript, or)
    );
    let ge = structural_ids(SourceLang::JavaScript, "function f(a) { return a >= 1; }\n");
    let gt = structural_ids(SourceLang::JavaScript, "function f(a) { return a > 1; }\n");
    assert_eq!(ge, gt);
}

#[test]
fn arithmetic_and_comparison_are_different_families() {
    let plus = structural_ids(SourceLang::JavaScript, "function f(a) { return a + 1; }\n");
    let less = structural_ids(SourceLang::JavaScript, "function f(a) { return a < 1; }\n");
    assert_ne!(plus, less);
}

#[test]
fn unary_operators_collapse_but_await_and_not_are_kept() {
    let neg = structural_ids(SourceLang::JavaScript, "function f(a) { return -a; }\n");
    let not = structural_ids(SourceLang::JavaScript, "function f(a) { return !a; }\n");
    assert_eq!(neg, not);
    let awaited = structural_ids(
        SourceLang::JavaScript,
        "async function f(a) { return await a; }\n",
    );
    let plain = structural_ids(
        SourceLang::JavaScript,
        "async function f(a) { return -a; }\n",
    );
    assert_ne!(awaited, plain);
    let py_not = structural_ids(SourceLang::Python, "def f(a):\n    return not a\n");
    let py_neg = structural_ids(SourceLang::Python, "def f(a):\n    return -a\n");
    assert_ne!(py_not, py_neg);
}

#[test]
fn assignment_operators_collapse_to_one_class() {
    let set = structural_ids(SourceLang::JavaScript, "function f(a) { a = 1; }\n");
    let add = structural_ids(SourceLang::JavaScript, "function f(a) { a += 1; }\n");
    assert_eq!(set, add);
}

#[test]
fn keywords_this_and_punctuation_are_kept() {
    let a = structural_ids(SourceLang::JavaScript, "function f() { return this.x; }\n");
    let b = structural_ids(SourceLang::JavaScript, "function f() { throw this.x; }\n");
    assert_ne!(a, b);
}

#[test]
fn jsx_tag_and_attribute_names_are_kept_in_both_modes() {
    let div = "export const A = () => <div className=\"a\"><span/></div>;\n";
    let list = "export const A = () => <ul className=\"a\"><li/></ul>;\n";
    assert_ne!(
        structural_ids(SourceLang::Tsx, div),
        structural_ids(SourceLang::Tsx, list)
    );
    let attr = "export const A = () => <div id=\"a\"><span/></div>;\n";
    assert_ne!(
        structural_ids(SourceLang::Tsx, div),
        structural_ids(SourceLang::Tsx, attr)
    );
    let stream = exact(SourceLang::Tsx, div);
    assert!(stream.iter().any(|t| t == "JsxTag:div"), "{stream:?}");
    assert!(
        stream.iter().any(|t| t == "JsxAttr:className"),
        "{stream:?}"
    );
    // The label and the class name differ in value only: one family.
    let relabelled = "export const A = () => <div className=\"b\"><span/></div>;\n";
    assert_eq!(
        structural_ids(SourceLang::Tsx, div),
        structural_ids(SourceLang::Tsx, relabelled)
    );
}

#[test]
fn jsx_text_is_a_string_token_and_whitespace_is_not() {
    let stream = exact(
        SourceLang::Tsx,
        "export const A = () => <p>\n  Loading\n</p>;\n",
    );
    assert!(stream.iter().any(|t| t == "Str:Loading"), "{stream:?}");
    assert_eq!(stream.iter().filter(|t| t.starts_with("Str:")).count(), 1);
}

#[test]
fn a_template_literal_is_one_string_token_with_substitutions_inside() {
    let stream = exact(
        SourceLang::JavaScript,
        "function f(a) { return `x${a + 1}y`; }\n",
    );
    let joined = stream.join(" ");
    assert!(
        joined.contains("Str:x${}y Punct:${ Ident:$ID BinOp:+ Num:1 Punct:}"),
        "{joined}"
    );
}

#[test]
fn escaped_quotes_do_not_make_strings_differ() {
    let a = exact(
        SourceLang::JavaScript,
        "function f() { return 'it\\'s'; }\n",
    );
    let b = exact(
        SourceLang::JavaScript,
        "function f() { return \"it's\"; }\n",
    );
    assert_eq!(a, b);
}

#[test]
fn number_spellings_are_canonical_in_exact_mode() {
    let a = exact(SourceLang::JavaScript, "function f() { return .12; }\n");
    let b = exact(SourceLang::JavaScript, "function f() { return 0.120; }\n");
    assert_eq!(a, b);
    let py = exact(SourceLang::Python, "def f():\n    return 1_000.50\n");
    assert!(py.iter().any(|t| t == "Num:1000.5"), "{py:?}");
}

#[test]
fn literals_true_false_null_undefined_none_collapse_in_structural() {
    let js = structural_ids(
        SourceLang::JavaScript,
        "function f() { return [true, null]; }\n",
    );
    let js2 = structural_ids(
        SourceLang::JavaScript,
        "function f() { return [false, undefined]; }\n",
    );
    assert_eq!(js, js2);
    let py = structural_ids(SourceLang::Python, "def f():\n    return [True, None]\n");
    let py2 = structural_ids(SourceLang::Python, "def f():\n    return [False, None]\n");
    assert_eq!(py, py2);
}

#[test]
fn ecma_imports_type_declarations_and_reexports_contribute_no_tokens() {
    let src = "import a from 'a';\nimport type { T } from './t';\nexport * from './x';\nexport { y } from './y';\nexport { z };\ninterface I { a: number }\ntype U = 'a' | 'b';\nenum E { A, B }\ndeclare module 'm' { export const q: number; }\nexport interface J { b: string }\nexport type V = string;\n// comment\n";
    assert_eq!(item_count(SourceLang::TypeScript, src), 0);
}

#[test]
fn ecma_directives_and_function_signatures_contribute_no_tokens() {
    let src = "'use client';\nexport function f(a: number): void;\nexport function f(a: any) { return a; }\n";
    let items = tokens(SourceLang::TypeScript, src, TokenMode::Structural);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name.as_deref(), Some("f"));
}

#[test]
fn export_and_export_default_prefixes_do_not_change_the_stream() {
    let bare = structural_ids(SourceLang::JavaScript, "function f(a) { return a; }\n");
    let exported = structural_ids(
        SourceLang::JavaScript,
        "export function f(a) { return a; }\n",
    );
    let default = structural_ids(
        SourceLang::JavaScript,
        "export default function f(a) { return a; }\n",
    );
    assert_eq!(bare, exported);
    assert_eq!(bare, default);
}

#[test]
fn decorators_are_kept_as_tokens() {
    let with = structural_ids(
        SourceLang::TypeScript,
        "class A { @Get('/x') list() { return 1; } }\n",
    );
    let without = structural_ids(SourceLang::TypeScript, "class A { list() { return 1; } }\n");
    assert!(with.len() > without.len());
}

#[test]
fn python_imports_and_docstrings_contribute_no_tokens() {
    let src = "\"\"\"module doc\"\"\"\nimport os\nfrom a import b\nfrom __future__ import annotations\n# c\n";
    assert_eq!(item_count(SourceLang::Python, src), 0);
    let with_doc = structural_ids(
        SourceLang::Python,
        "def f():\n    \"\"\"doc\"\"\"\n    return 1\n",
    );
    let without = structural_ids(SourceLang::Python, "def f():\n    return 1\n");
    assert_eq!(with_doc, without);
}

#[test]
fn python_blocks_emit_synthetic_braces_so_dedent_is_visible() {
    let inside = "def f(a):\n    if a:\n        return 1\n        return 2\n";
    let after = "def f(a):\n    if a:\n        return 1\n    return 2\n";
    assert_ne!(
        structural_ids(SourceLang::Python, inside),
        structural_ids(SourceLang::Python, after)
    );
    let stream = exact(SourceLang::Python, after);
    assert_eq!(stream.iter().filter(|t| *t == "Block:{").count(), 2);
    assert_eq!(stream.iter().filter(|t| *t == "Block:}").count(), 2);
}

#[test]
fn python_attribute_and_keyword_argument_names_are_properties() {
    let stream = exact(
        SourceLang::Python,
        "def f(o):\n    return g(o.price, rate=2)\n",
    );
    assert!(stream.iter().any(|t| t == "Prop:price"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "Prop:rate"), "{stream:?}");
    let a = structural_ids(
        SourceLang::Python,
        "def f(o):\n    return g(o.price, rate=2)\n",
    );
    let b = structural_ids(
        SourceLang::Python,
        "def f(o):\n    return g(o.cost, scale=3)\n",
    );
    assert_eq!(a, b);
}

#[test]
fn python_comparison_words_are_one_operator_each() {
    let stream = exact(
        SourceLang::Python,
        "def f(a, b):\n    return a not in b or a is not b\n",
    );
    assert!(stream.iter().any(|t| t == "BinOp:not in"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "BinOp:is not"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "BinOp:or"), "{stream:?}");
}

#[test]
fn python_fstrings_emit_their_substitutions() {
    let stream = exact(SourceLang::Python, "def f(a):\n    return f'x{a}y'\n");
    let joined = stream.join(" ");
    assert!(
        joined.contains("Str:x{}y Punct:{ Ident:$ID Punct:}"),
        "{joined}"
    );
}

#[test]
fn rust_use_struct_enum_type_and_bodyless_trait_items_contribute_no_tokens() {
    let src = "use std::fmt;\nextern crate foo;\nmod a;\npub struct S { x: u32 }\nenum E { A }\nunion U { a: u32 }\ntype T = u32;\ntrait Tr { fn a(&self); }\n#![allow(dead_code)]\n";
    // `#![..]` after items is a parse error in a file; keep the check to the valid prefix.
    let valid = src.replace("#![allow(dead_code)]\n", "");
    assert_eq!(item_count(SourceLang::Rust, &valid), 0);
}

#[test]
fn rust_trait_with_a_default_body_and_impl_blocks_are_items() {
    let src =
        "trait Tr { fn a(&self) -> u32 { 1 } }\nimpl Tr for S { fn b(&self) {} }\nfn c() {}\n";
    let items = tokens(SourceLang::Rust, src, TokenMode::Structural);
    let kinds: Vec<&str> = items.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, ["trait", "impl", "function"]);
}

#[test]
fn rust_visibility_prefixes_and_doc_comments_are_dropped() {
    let plain = structural_ids(SourceLang::Rust, "fn f(a: u32) -> u32 { a }\n");
    let public = structural_ids(SourceLang::Rust, "pub fn f(a: u32) -> u32 { a }\n");
    let crate_visible = structural_ids(SourceLang::Rust, "pub(crate) fn f(a: u32) -> u32 { a }\n");
    let documented = structural_ids(
        SourceLang::Rust,
        "/// Adds nothing.\n#[doc = \"more\"]\nfn f(a: u32) -> u32 { a }\n",
    );
    assert_eq!(plain, public);
    assert_eq!(plain, crate_visible);
    assert_eq!(plain, documented);
}

#[test]
fn rust_attributes_other_than_docs_are_kept() {
    let with = structural_ids(SourceLang::Rust, "#[inline]\nfn f() {}\n");
    let without = structural_ids(SourceLang::Rust, "fn f() {}\n");
    assert!(with.len() > without.len());
}

#[test]
fn rust_cfg_test_modules_and_test_functions_are_not_items() {
    let src = "fn real() -> u32 { 1 }\n#[cfg(test)]\nmod tests { fn t() { assert!(true); } }\n#[test]\nfn also_a_test() {}\n#[cfg(not(test))]\nfn production() {}\n";
    let items = tokens(SourceLang::Rust, src, TokenMode::Structural);
    let names: Vec<_> = items.iter().filter_map(|i| i.name.as_deref()).collect();
    assert_eq!(names, ["real", "production"]);
}

#[test]
fn rust_inline_modules_are_flattened_into_their_items() {
    let src = "mod inner { fn a() {} fn b() {} }\n";
    let names: Vec<_> = tokens(SourceLang::Rust, src, TokenMode::Structural)
        .iter()
        .filter_map(|i| i.name.clone())
        .collect();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn rust_macro_bodies_are_tokenized() {
    let stream = exact(
        SourceLang::Rust,
        "fn f(x: u32) { let v = json!({ \"rate\": 0.2, \"n\": x }); }\n",
    );
    assert!(stream.iter().any(|t| t == "Str:rate"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "Num:0.2"), "{stream:?}");
}

#[test]
fn rust_joint_punctuation_fuses_into_operators() {
    let stream = exact(
        SourceLang::Rust,
        "fn f(a: u32, b: u32) -> bool { a == b && (a..=b).contains(&a) || a != b }\n",
    );
    for expected in [
        "Punct:->",
        "BinOp:==",
        "BinOp:&&",
        "Punct:..=",
        "BinOp:||",
        "BinOp:!=",
    ] {
        assert!(
            stream.iter().any(|t| t == expected),
            "{expected} in {stream:?}"
        );
    }
}

#[test]
fn rust_unary_and_binary_minus_and_star_are_told_apart() {
    let stream = exact(
        SourceLang::Rust,
        "fn f(a: &u32, b: u32) -> i64 { -(*a as i64) - b as i64 * 2 }\n",
    );
    let unary = stream.iter().filter(|t| t.starts_with("UnOp:")).count();
    let binary = stream.iter().filter(|t| t.starts_with("BinOp:")).count();
    assert_eq!(unary, 3, "{stream:?}"); // `&` in the type, `-` negation, `*` deref
    assert_eq!(binary, 2, "{stream:?}"); // `-` subtraction, `*` multiplication
}

#[test]
fn rust_generic_brackets_are_not_comparisons() {
    let generic = exact(
        SourceLang::Rust,
        "fn f(a: Vec<Vec<u32>>) -> Option<u32> { let b = a.iter().collect::<Vec<_>>(); None }\n",
    );
    assert!(
        !generic.iter().any(|t| t.starts_with("BinOp:")),
        "{generic:?}"
    );
    let compare = exact(
        SourceLang::Rust,
        "fn f(a: u32, b: u32) -> bool { a < b || b > a }\n",
    );
    assert_eq!(
        compare.iter().filter(|t| t.starts_with("BinOp:")).count(),
        3,
        "{compare:?}"
    );
}

#[test]
fn rust_method_and_field_names_after_a_dot_are_properties() {
    let a = structural_ids(
        SourceLang::Rust,
        "fn f(o: &O) -> u32 { o.price.max(o.tax) }\n",
    );
    let b = structural_ids(
        SourceLang::Rust,
        "fn g(p: &P) -> u32 { p.cost.min(p.fee) }\n",
    );
    assert_eq!(a, b);
    let stream = exact(
        SourceLang::Rust,
        "fn f(o: &O) -> u32 { o.price.max(o.tax) }\n",
    );
    assert!(stream.iter().any(|t| t == "Prop:price"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "Prop:max"), "{stream:?}");
}

#[test]
fn rust_numeric_suffixes_and_underscores_are_canonical_in_exact_mode() {
    let stream = exact(
        SourceLang::Rust,
        "fn f() -> f64 { 1_000.50_f64 + 2u32 as f64 }\n",
    );
    assert!(stream.iter().any(|t| t == "Num:1000.5"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "Num:2"), "{stream:?}");
}

#[test]
fn both_modes_tokenize_the_same_stream_shape() {
    let sources = [
        (SourceLang::TypeScript, ts_discount_default("0.2")),
        (SourceLang::Python, py_status("status", "order")),
        (SourceLang::Rust, rs_fee("fee", "order")),
        (
            SourceLang::Tsx,
            "export const A = () => <div onClick={() => go(`a${b}`)}>Hi</div>;\n".to_string(),
        ),
    ];
    for (lang, source) in &sources {
        let structural = tokens(*lang, source, TokenMode::Structural);
        let exact = tokens(*lang, source, TokenMode::Exact);
        assert_eq!(structural.len(), exact.len(), "{lang:?}");
        for (s, e) in structural.iter().zip(&exact) {
            assert_eq!((s.item, s.kind, &s.name), (e.item, e.kind, &e.name));
            assert_eq!((s.start_line, s.end_line), (e.start_line, e.end_line));
            let shape = |i: &ItemTokens| -> Vec<(TokenClass, u32, Range<u32>)> {
                i.tokens
                    .iter()
                    .map(|t| (t.class, t.line, t.raw.clone()))
                    .collect()
            };
            assert_eq!(shape(s), shape(e), "{lang:?}");
            assert!(s.texts.is_empty());
            assert_eq!(e.texts.len(), e.tokens.len());
        }
    }
}

#[test]
fn item_numbers_count_only_items_that_have_tokens() {
    let src =
        "import a from 'a';\nfunction f() { return 1; }\n// note\nfunction g() { return 2; }\n";
    let items = tokens(SourceLang::JavaScript, src, TokenMode::Structural);
    let numbers: Vec<u32> = items.iter().map(|i| i.item).collect();
    assert_eq!(numbers, [0, 1]);
}

#[test]
fn item_kind_and_name_are_reported() {
    let src = "export const rate = () => 1;\nclass K {}\nconst limit = 5;\nfunction f() {}\n";
    let items = tokens(SourceLang::TypeScript, src, TokenMode::Structural);
    let heads: Vec<(&str, Option<&str>)> =
        items.iter().map(|i| (i.kind, i.name.as_deref())).collect();
    assert_eq!(
        heads,
        [
            ("function", Some("rate")),
            ("class", Some("K")),
            ("variable", Some("limit")),
            ("function", Some("f")),
        ]
    );
}

#[test]
fn lookup_tables_are_data_items() {
    let ts = tokens(
        SourceLang::TypeScript,
        "export const routes = [{ path: '/a' }, { path: '/b' }] as const;\nmodule.exports = { a: 1 };\n",
        TokenMode::Structural,
    );
    assert!(ts.iter().all(|i| i.kind == "data"), "{ts:?}");
    let py = tokens(
        SourceLang::Python,
        "TABLE = {'a': 1, 'b': 2}\n",
        TokenMode::Structural,
    );
    assert_eq!(py[0].kind, "data");
    let rs = tokens(
        SourceLang::Rust,
        "const TABLE: [(u32, u32); 2] = [(1, 2), (3, 4)];\nconst N: u32 = 5;\n",
        TokenMode::Structural,
    );
    let kinds: Vec<&str> = rs.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, ["data", "const"]);
}

#[test]
fn lines_and_byte_ranges_point_at_the_source_tokens() {
    let ts = "function f(a) {\n  return a.total * 12;\n}\n";
    let rs = "fn f(a: u32) -> u32 {\n    // héllo wörld\n    let s = \"é\"; a * 12\n}\n";
    for (lang, source) in [(SourceLang::TypeScript, ts), (SourceLang::Rust, rs)] {
        let item = &tokens(lang, source, TokenMode::Exact)[0];
        for (token, text) in item.tokens.iter().zip(&item.texts) {
            let slice = &source[token.raw.start as usize..token.raw.end as usize];
            let line = 1 + source[..token.raw.start as usize].matches('\n').count() as u32;
            assert_eq!(token.line, line, "{lang:?} {text}");
            if matches!(
                token.class,
                TokenClass::Keyword | TokenClass::Punct | TokenClass::BinOp
            ) {
                assert_eq!(slice, text, "{lang:?}");
            }
        }
    }
    let item = &tokens(SourceLang::TypeScript, ts, TokenMode::Exact)[0];
    assert_eq!((item.start_line, item.end_line), (1, 3));
}

#[test]
fn token_ids_are_content_derived_and_class_sensitive() {
    assert_eq!(
        token_id(TokenClass::Keyword, "if"),
        token_id(TokenClass::Keyword, "if")
    );
    assert_ne!(
        token_id(TokenClass::Keyword, "if"),
        token_id(TokenClass::Prop, "if")
    );
    let ids = structural_ids(SourceLang::JavaScript, "function f(a) { return a; }\n");
    assert!(ids.contains(&token_id(TokenClass::Ident, TEXT_IDENT)));
    assert!(ids.contains(&token_id(TokenClass::Keyword, "return")));
}

#[test]
fn malformed_truncated_and_empty_inputs_never_panic() {
    let hostile = [
        "",
        "   \n\n",
        "function (((",
        "export default {{{{",
        "def f(:\n  \t\x00",
        "fn f( { let x = ;",
        "\u{feff}\u{0}\u{1}binary\u{7f}",
        "<div><span></div>",
        "/* never closed",
        "'unterminated",
        "r#\"raw",
    ];
    for lang in [
        SourceLang::Rust,
        SourceLang::TypeScript,
        SourceLang::Tsx,
        SourceLang::JavaScript,
        SourceLang::Python,
    ] {
        for source in hostile {
            for mode in [TokenMode::Structural, TokenMode::Exact] {
                let _ = tokens(lang, source, mode);
            }
        }
    }
}

#[test]
fn very_deep_and_very_long_inputs_do_not_overflow_the_stack() {
    let chain = format!(
        "function f(a) {{ return {}; }}\n",
        vec!["a"; 20_000].join(" + ")
    );
    assert_eq!(item_count(SourceLang::JavaScript, &chain), 1);
    let py_chain = format!("def f(a):\n    return {}\n", vec!["a"; 20_000].join(" + "));
    let _ = tokens(SourceLang::Python, &py_chain, TokenMode::Structural);
    let nested = format!(
        "function f() {{ return {}1{}; }}\n",
        "(".repeat(3000),
        ")".repeat(3000)
    );
    let _ = tokens(SourceLang::JavaScript, &nested, TokenMode::Structural);
    let rust_nested = format!(
        "fn f() {{ foo!{}1{}; }}\n",
        "(".repeat(3000),
        ")".repeat(3000)
    );
    // Rust groups nested past the limit are refused (`syn` would overflow its own stack), not
    // guessed at; moderate nesting is tokenized.
    assert!(tokens(SourceLang::Rust, &rust_nested, TokenMode::Structural).is_empty());
    let moderate = format!(
        "fn f() {{ foo!{}1{}; }}\n",
        "(".repeat(100),
        ")".repeat(100)
    );
    assert_eq!(item_count(SourceLang::Rust, &moderate), 1);
}

#[test]
fn rust_tokenization_is_repeatable_and_positions_survive_the_span_reset() {
    // `proc_macro2` forgets its spans after each call; a second file must still get its own
    // lines and byte ranges right.
    let first = "fn a(x: u32) -> u32 {\n    x + 1\n}\n";
    let second = "\n\n\nfn b(y: u32) -> u32 {\n    y * 2\n}\n";
    let once = tokens(SourceLang::Rust, first, TokenMode::Exact);
    let other = tokens(SourceLang::Rust, second, TokenMode::Exact);
    let again = tokens(SourceLang::Rust, first, TokenMode::Exact);
    assert_eq!(once, again);
    assert_eq!((other[0].start_line, other[0].end_line), (4, 6));
    let token = &other[0].tokens[0];
    assert_eq!(
        &second[token.raw.start as usize..token.raw.end as usize],
        "fn"
    );
}

#[test]
fn rust_string_forms_compare_by_content() {
    let plain = exact(SourceLang::Rust, "fn f() -> &'static str { \"a b\" }\n");
    let raw = exact(SourceLang::Rust, "fn f() -> &'static str { r\"a b\" }\n");
    let hashed = exact(SourceLang::Rust, "fn f() -> &'static str { r#\"a b\"# }\n");
    let bytes = exact(SourceLang::Rust, "fn f() -> &'static [u8] { b\"a b\" }\n");
    assert!(plain.iter().any(|t| t == "Str:a b"), "{plain:?}");
    assert!(raw.iter().any(|t| t == "Str:a b"), "{raw:?}");
    assert!(hashed.iter().any(|t| t == "Str:a b"), "{hashed:?}");
    assert!(bytes.iter().any(|t| t == "Str:a b"), "{bytes:?}");
    let ch = exact(SourceLang::Rust, "fn f() -> char { 'x' }\n");
    assert!(ch.iter().any(|t| t == "Str:x"), "{ch:?}");
}

#[test]
fn javascript_jsx_with_a_class_attribute_is_tokenized_through_the_tsx_retry() {
    let src = "export function A() { return <div class=\"a\"><label for=\"x\"/></div>; }\n";
    let stream = exact(SourceLang::JavaScript, src);
    assert!(stream.iter().any(|t| t == "JsxAttr:class"), "{stream:?}");
    assert!(stream.iter().any(|t| t == "JsxTag:label"), "{stream:?}");
}

#[test]
fn rust_shebang_line_is_ignored() {
    let with = "#!/usr/bin/env run-cargo-script\nfn f(a: u32) -> u32 { a + 1 }\n";
    let without = "\nfn f(a: u32) -> u32 { a + 1 }\n";
    assert_eq!(
        structural_ids(SourceLang::Rust, with),
        structural_ids(SourceLang::Rust, without)
    );
    assert_eq!(item_count(SourceLang::Rust, with), 1);
}

#[test]
fn a_source_with_a_parse_error_drops_only_the_broken_item() {
    let src = "function good(a) { return a + 1; }\nfunction bad( { return ; \nfunction also(a) { return a * 2; }\n";
    let names: Vec<String> = tokens(SourceLang::JavaScript, src, TokenMode::Structural)
        .iter()
        .filter_map(|i| i.name.clone())
        .collect();
    assert!(names.contains(&"good".to_string()), "{names:?}");
}
