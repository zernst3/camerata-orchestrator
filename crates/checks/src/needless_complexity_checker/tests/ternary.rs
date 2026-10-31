use super::run;
use crate::arch_checker::{ViolationGrade, SEVERITY_MEDIUM};

fn one(src: &str) -> Vec<crate::arch_checker::ArchViolation> {
    run(&[("src/view.ts", src)])
}

#[test]
fn a_negated_ternary_on_a_name_is_flagged_with_the_swapped_form() {
    let vs = one("export const label = (isReady: boolean) => !isReady ? 'wait' : 'go';\n");
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.rule_id, super::RULE_NEEDLESS_COMPLEXITY);
    assert_eq!(v.grade, ViolationGrade::Defect);
    assert_eq!(v.severity, SEVERITY_MEDIUM);
    assert_eq!(v.line, 1);
    assert_eq!(v.object.as_deref(), Some("isReady"));
    assert!(
        v.message
            .contains("`!isReady ? 'wait' : 'go'` is `isReady ? 'go' : 'wait'` written backwards"),
        "{}",
        v.message
    );
}

#[test]
fn member_call_and_subscript_operands_and_jsx_files_are_covered() {
    for (path, src) in [
        ("src/a.js", "const x = !user.active ? 0 : 1;\n"),
        ("src/b.jsx", "const x = !isOpen() ? null : <Panel />;\n"),
        ("src/c.tsx", "const x = !flags[0] ? a : b;\n"),
        (
            "src/d.mjs",
            "export default (p) => (!p.ok ? p.error : p.value);\n",
        ),
    ] {
        assert_eq!(run(&[(path, src)]).len(), 1, "{path}");
    }
}

#[test]
fn a_long_ternary_is_described_without_quoting_it() {
    let long = format!("const x = !ready ? '{}' : 'b';\n", "a".repeat(120));
    let vs = one(&long);
    assert_eq!(vs.len(), 1);
    assert!(
        vs[0]
            .message
            .contains("This conditional on `!ready` is one on `ready`"),
        "{}",
        vs[0].message
    );
}

// ── safe twins and traps ───────────────────────────────────────────────────

#[test]
fn safe_twin_a_multi_line_guard_first_ternary_is_clearer_as_written() {
    let src = "\
export const View = ({ data }) =>
  !data ? (
    <Spinner />
  ) : (
    <Table rows={data.rows} columns={data.columns} />
  );
";
    assert!(run(&[("src/View.jsx", src)]).is_empty());
}

#[test]
fn comparisons_are_not_negations() {
    for src in [
        "const x = a !== b ? c : d;\n",
        "const x = a != null ? a : d;\n",
        "const x = !(a && b) ? c : d;\n",
        "const x = !(k in o) ? c : d;\n",
    ] {
        assert!(one(src).is_empty(), "{src}");
    }
}

#[test]
fn double_negation_is_a_coercion() {
    assert!(one("const x = !!a ? b : c;\n").is_empty());
}

#[test]
fn chains_are_left_alone() {
    for src in [
        "const x = !a ? b : c ? d : e;\n",
        "const x = c ? d : !a ? b : e;\n",
        "const x = c ? (!a ? b : e) : d;\n",
        "const x = !a ? (c ? d : e) : b;\n",
    ] {
        assert!(one(src).is_empty(), "{src}");
    }
}

#[test]
fn non_null_assertions_and_conditional_types_are_not_negations() {
    assert!(one("const x = maybe! ? a : b;\n").is_empty());
    assert!(one("type T<U> = U extends string ? 'a' : 'b';\n").is_empty());
}

#[test]
fn boolean_branches_are_a_different_smell() {
    assert!(one("const x = !a ? true : false;\n").is_empty());
    assert_eq!(one("const x = !a ? true : b;\n").len(), 1);
}

#[test]
fn if_statements_are_out_of_scope() {
    assert!(one("if (!a) { f(); } else { g(); }\n").is_empty());
}

#[test]
fn test_story_generated_vendored_and_minified_files_are_not_read() {
    let src = "const x = !a ? b : c;\n";
    for path in [
        "src/view.test.ts",
        "src/__tests__/view.ts",
        "src/view.stories.tsx",
        "src/generated/view.ts",
        "node_modules/lib/view.js",
        "dist/view.js",
        "src/view.min.js",
        "src/view.d.ts",
    ] {
        assert!(run(&[(path, src)]).is_empty(), "{path}");
    }
}

#[test]
fn committed_build_output_beside_its_source_is_not_read() {
    let vs = run(&[
        ("src/view.ts", "export const x = !a ? b : c;\n"),
        ("src/view.js", "export const x = !a ? b : c;\n"),
    ]);
    assert_eq!(vs.len(), 1);
    assert_eq!(vs[0].file, "src/view.ts");
}
