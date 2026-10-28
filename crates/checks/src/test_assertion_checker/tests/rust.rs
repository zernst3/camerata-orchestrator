use super::reported;

fn one(src: &str) -> Vec<String> {
    reported("src/orders.rs", src)
}

// ── positives ──────────────────────────────────────────────────────────────

#[test]
fn empty_and_build_only_tests_are_reported() {
    let src = "\
pub fn total(x: &[u32]) -> u32 { x.iter().sum() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {}

    #[test]
    fn builds_a_value() {
        let _ = Order::new(vec![1, 2]);
        let n = usize::from(3u8);
        let _ = Some(n);
    }

    #[tokio::test]
    async fn awaits_without_checking() {
        let _ = total(&[1]);
        drop(Order::default());
    }
}
";
    assert_eq!(
        one(src),
        vec!["empty", "builds_a_value", "awaits_without_checking"]
    );
}

#[test]
fn tests_in_integration_files_are_read() {
    assert_eq!(
        reported("tests/api.rs", "#[test]\nfn t() { let _ = 1; }\n"),
        vec!["t"]
    );
}

// ── safe twins and traps ───────────────────────────────────────────────────

#[test]
fn safe_twin_assertions_and_panicking_calls_are_assertions() {
    for body in [
        "assert_eq!(total(&[1]), 1);",
        "assert!(ok);",
        "debug_assert!(ok);",
        "pretty_assertions::assert_eq!(a, b);",
        "insta::assert_snapshot!(render());",
        "panic!(\"no\");",
        "let v = parse(\"1\").unwrap();",
        "let v = parse(\"1\").expect(\"parses\");",
        "parse(\"x\").unwrap_err();",
        "let v = vec![parse(\"1\").unwrap()];",
        "println!(\"{}\", parse(\"1\").unwrap());",
        "unknown_macro!(x);",
        "check_total(1);",
        "order.verify();",
    ] {
        let src = format!("#[test]\nfn t() {{ {body} }}\n");
        assert!(one(&src).is_empty(), "{body}");
    }
}

#[test]
fn the_question_mark_in_a_result_test_is_an_assertion() {
    let src = "#[test]\nfn t() -> Result<(), Error> {\n    let _ = parse(\"1\")?;\n    Ok(())\n}\n";
    assert!(one(src).is_empty());
}

#[test]
fn should_panic_ignored_and_case_tests_are_not_read() {
    for attrs in [
        "#[test]\n#[should_panic]",
        "#[test]\n#[should_panic(expected = \"x\")]",
        "#[test]\n#[ignore]",
        "#[test_case(1 => 2)]\n#[test]",
        "#[rstest]\n#[test]",
    ] {
        let src = format!("{attrs}\nfn t() {{ let _ = Order::new(); }}\n");
        assert!(one(&src).is_empty(), "{attrs}");
    }
}

#[test]
fn a_helper_in_this_file_is_followed_and_one_elsewhere_may_assert() {
    let src = "\
fn confirm(x: u32) { assert_eq!(x, 1); }
fn quiet(x: u32) -> u32 { x }

#[test]
fn a() { confirm(1); }

#[test]
fn b() { let _ = quiet(1); }

#[test]
fn c() { crate::run_ui_tests(\"debug\"); }

#[test]
fn d() { support::scenario(); }

#[test]
fn e() { run_case(); }

#[test]
fn f() { let _ = std::mem::size_of::<u8>(); }
";
    assert_eq!(one(src), vec!["b", "f"]);
}

#[test]
fn functions_without_a_test_attribute_are_not_tests() {
    assert!(one("fn test_like() {}\n#[cfg(test)]\nfn helper() {}\n").is_empty());
}
