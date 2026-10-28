use super::*;

mod optional_props;
mod ternary;

pub(super) fn run(pairs: &[(&str, &str)]) -> Vec<ArchViolation> {
    let files: Vec<(String, String)> = pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    NeedlessComplexityChecker.check(&RepoView {
        spec: "test/repo",
        files: &files,
    })
}

#[test]
fn checker_contract() {
    let checker = NeedlessComplexityChecker;
    assert_eq!(checker.rule_ids(), &[RULE_NEEDLESS_COMPLEXITY]);
    assert!(!checker.advisory_coexisting());
    assert!(checker.runs_at_write_time());
    for path in [
        "a/b.tsx",
        "a/b.jsx",
        "a/b.ts",
        "a/b.js",
        "a/b.mjs",
        "package.json",
        "apps/web/package.json",
        "tsconfig.json",
        "docs/Button.mdx",
        "src/App.vue",
    ] {
        assert!(
            crate::arch_checker::matches_any_glob(checker.interest_globs(), path),
            "{path}"
        );
    }
}

#[test]
fn malformed_input_does_not_panic() {
    for src in [
        "",
        "!a ?",
        "<X a=",
        "function A(p: {a?: string}) { return <B",
        "\u{0}}{",
    ] {
        let _ = run(&[
            ("package.json", "{}"),
            ("src/x.tsx", src),
            ("src/y.tsx", src),
        ]);
    }
}
