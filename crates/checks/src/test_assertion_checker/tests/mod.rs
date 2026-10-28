use super::*;

mod ecma;
mod python;
mod rust;

pub(super) fn run(pairs: &[(&str, &str)]) -> Vec<ArchViolation> {
    let files: Vec<(String, String)> = pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    TestAssertionChecker.check(&RepoView {
        spec: "test/repo",
        files: &files,
    })
}

/// The test names reported for one file.
pub(super) fn reported(path: &str, src: &str) -> Vec<String> {
    run(&[(path, src)])
        .into_iter()
        .map(|v| v.object.unwrap_or_default())
        .collect()
}

#[test]
fn checker_contract() {
    let checker = TestAssertionChecker;
    assert_eq!(checker.rule_ids(), &[RULE_MEANINGFUL_ASSERTION]);
    assert!(checker.advisory_coexisting());
    assert!(!checker.defects_need_review());
    assert!(checker.runs_at_write_time());
    for path in [
        "src/a.test.ts",
        "src/a.spec.tsx",
        "test/a.js",
        "tests/test_a.py",
        "src/lib.rs",
        "tests/it.rs",
    ] {
        assert!(
            crate::arch_checker::matches_any_glob(checker.interest_globs(), path),
            "{path}"
        );
    }
}

#[test]
fn a_finding_names_the_test_and_states_the_fact() {
    let vs = run(&[(
        "src/app.test.js",
        "test('renders without crashing', () => {\n  render(<App />);\n});\n",
    )]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.rule_id, RULE_MEANINGFUL_ASSERTION);
    assert_eq!(v.grade, ViolationGrade::Defect);
    assert_eq!(v.severity, SEVERITY_MEDIUM);
    assert_eq!(v.line, 1);
    assert_eq!(v.object.as_deref(), Some("renders without crashing"));
    assert!(
        v.message
            .starts_with("The test \"renders without crashing\" (line 1) has no assertion"),
        "{}",
        v.message
    );
    assert!(!v.message.contains('\u{2014}') && !v.message.contains('\u{2013}'));
    assert!(!v.message.to_ascii_lowercase().contains("camerata"));
}

#[test]
fn vendored_and_build_output_tests_are_not_read() {
    let src = "test('x', () => { run(); });\n";
    for path in [
        "node_modules/lib/x.test.js",
        "vendor/lib/x.test.js",
        "dist/x.test.js",
    ] {
        assert!(run(&[(path, src)]).is_empty(), "{path}");
    }
    assert!(run(&[(
        "venv/lib/site-packages/pkg/test_x.py",
        "def test_x():\n    run()\n"
    )])
    .is_empty());
}

#[test]
fn output_is_deterministic() {
    let files = [
        ("b/test_b.py", "def test_b():\n    pass\n"),
        ("a/a.test.ts", "it('a', () => {});\n"),
        ("src/lib.rs", "#[test]\nfn c() {}\n"),
    ];
    let first = run(&files);
    let second = run(&[files[2], files[0], files[1]]);
    assert_eq!(first, second);
    assert_eq!(first.len(), 3);
}

#[test]
fn malformed_input_does_not_panic() {
    for src in ["", "it(", "def test_x(:\n", "#[test] fn", "\u{0}}{"] {
        let _ = run(&[("a.test.ts", src), ("test_a.py", src), ("a.rs", src)]);
    }
}
