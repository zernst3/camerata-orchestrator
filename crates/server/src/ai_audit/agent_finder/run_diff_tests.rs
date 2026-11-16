//! The run difference note: a problem in only one run is never silent.

use super::*;

fn f(rule: &str, path: &str, line: usize, severity: &str, snippet: &str) -> Finding {
    Finding {
        repo: "acme/api".into(),
        path: path.into(),
        line,
        rule_id: rule.into(),
        severity: severity.into(),
        snippet: snippet.into(),
        detail: format!("{rule} problem at {path}"),
        ..Finding::default()
    }
}

#[test]
fn a_problem_absent_from_the_second_run_is_listed() {
    let a = vec![
        f("SEC-1", "src/a.rs", 10, "high", "let q = format!(sql)"),
        f("SEC-2", "src/b.rs", 5, "medium", "unsafe { x }"),
    ];
    let b = vec![f("SEC-1", "src/a.rs", 10, "high", "let q = format!(sql)")];
    let diff = diff_runs(&a, &b);
    assert_eq!(diff.only_in_a.len(), 1);
    assert_eq!(diff.only_in_a[0].rule_id, "SEC-2");
    assert!(diff.only_in_b.is_empty());
    assert_eq!(diff.in_both, 1);
    let note = operator_note(&diff);
    assert!(note.contains("Only in the first run"), "{note}");
    assert!(
        note.contains("SEC-2") && note.contains("src/b.rs line 5"),
        "{note}"
    );
    assert!(!diff.is_empty());
}

#[test]
fn a_problem_only_in_the_second_run_is_listed() {
    let a: Vec<Finding> = Vec::new();
    let b = vec![f("SEC-9", "src/c.rs", 1, "low", "eval(input)")];
    let note = operator_note(&diff_runs(&a, &b));
    assert!(note.contains("Only in the second run"), "{note}");
    assert!(note.contains("SEC-9"), "{note}");
}

#[test]
fn a_problem_that_moved_two_lines_is_matched() {
    let a = vec![f("SEC-1", "src/a.rs", 10, "high", "x")];
    let b = vec![f("SEC-1", "src/a.rs", 12, "high", "y")];
    let diff = diff_runs(&a, &b);
    assert_eq!(diff.in_both, 1);
    assert!(diff.is_empty(), "{diff:?}");
}

#[test]
fn a_problem_that_moved_far_but_quotes_the_same_code_is_matched() {
    let a = vec![f("SEC-1", "src/a.rs", 10, "high", "let q = format!(sql);")];
    let b = vec![f("SEC-1", "src/a.rs", 90, "high", "let q = format!(sql);")];
    assert_eq!(diff_runs(&a, &b).in_both, 1);
}

#[test]
fn a_short_quote_alone_does_not_make_two_far_apart_problems_one() {
    let a = vec![f("SEC-1", "src/a.rs", 10, "high", "}")];
    let b = vec![f("SEC-1", "src/a.rs", 90, "high", "}")];
    let diff = diff_runs(&a, &b);
    assert_eq!(diff.in_both, 0);
    assert_eq!(diff.only_in_a.len(), 1);
    assert_eq!(diff.only_in_b.len(), 1);
}

#[test]
fn another_rule_or_file_or_repository_is_another_problem() {
    let base = f("SEC-1", "src/a.rs", 10, "high", "same code here");
    for other in [
        f("SEC-2", "src/a.rs", 10, "high", "same code here"),
        f("SEC-1", "src/z.rs", 10, "high", "same code here"),
        Finding {
            repo: "acme/web".into(),
            ..base.clone()
        },
    ] {
        let diff = diff_runs(std::slice::from_ref(&base), &[other]);
        assert_eq!(diff.in_both, 0);
        assert_eq!(diff.only_in_a.len() + diff.only_in_b.len(), 2);
    }
}

#[test]
fn a_severity_change_is_listed() {
    let a = vec![f("SEC-1", "src/a.rs", 10, "high", "x")];
    let b = vec![f("SEC-1", "src/a.rs", 11, "medium", "x")];
    let diff = diff_runs(&a, &b);
    assert_eq!(diff.in_both, 1);
    assert_eq!(diff.severity_changed.len(), 1);
    assert_eq!(diff.severity_changed[0].in_a, "high");
    assert_eq!(diff.severity_changed[0].in_b, "medium");
    let note = operator_note(&diff);
    assert!(note.contains("Rated differently"), "{note}");
    assert!(
        note.contains("high in the first run, medium in the second"),
        "{note}"
    );
}

#[test]
fn two_problems_are_not_both_matched_to_one() {
    let a = vec![
        f("SEC-1", "src/a.rs", 10, "high", "x"),
        f("SEC-1", "src/a.rs", 11, "high", "x"),
    ];
    let b = vec![f("SEC-1", "src/a.rs", 10, "high", "x")];
    let diff = diff_runs(&a, &b);
    assert_eq!(diff.in_both, 1);
    assert_eq!(diff.only_in_a.len(), 1, "the second twin is reported");
}

#[test]
fn two_identical_runs_give_an_empty_note_that_says_so() {
    let a = vec![f("SEC-1", "src/a.rs", 10, "high", "x")];
    let diff = diff_runs(&a, &a.clone());
    assert!(diff.is_empty());
    let note = operator_note(&diff);
    assert!(
        note.contains("found the same problems at the same severities"),
        "{note}"
    );
    assert!(!note.contains("Only in"), "{note}");
}

#[test]
fn the_note_has_no_dashes_and_marks_itself_operator_only() {
    let a = vec![f("SEC-1", "src/a.rs", 10, "high", "x")];
    let note = operator_note(&diff_runs(&a, &[]));
    assert!(
        !note.contains('\u{2014}') && !note.contains('\u{2013}'),
        "{note}"
    );
    assert!(note.contains("For the operator only"), "{note}");
}

/// The note is handed to the operator-only folder by its caller. No part of the package
/// assembly or the release gate refers to it, so no client file can carry it.
#[test]
fn nothing_in_the_package_or_the_gate_refers_to_the_note() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0;
    let mut stack = vec![root.join("release_gate"), root.join("report_export")];
    for single in ["release_gate.rs", "report_export.rs"] {
        let p = root.join(single);
        if p.is_file() {
            stack.push(p);
        }
    }
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            for e in std::fs::read_dir(&path).unwrap().flatten() {
                stack.push(e.path());
            }
        } else if path.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for name in ["operator_note", "run_diff", "diff_runs"] {
                assert!(!text.contains(name), "{path:?} refers to {name}");
            }
            checked += 1;
        }
    }
    assert!(checked > 10, "the scan found only {checked} files");
}

#[test]
fn a_sample_note_for_the_report() {
    let a = vec![
        f("SEC-1", "src/a.rs", 10, "high", "let q = format!(sql);"),
        f("SEC-2", "src/b.rs", 5, "medium", "unsafe { x }"),
        f(
            "ARCH-4",
            "src/c.rs",
            40,
            "high",
            "handler calls the database directly",
        ),
    ];
    let b = vec![
        f("SEC-1", "src/a.rs", 12, "medium", "let q = format!(sql);"),
        f(
            "ARCH-4",
            "src/c.rs",
            40,
            "high",
            "handler calls the database directly",
        ),
        f("SEC-9", "src/d.rs", 3, "low", "eval(input)"),
    ];
    eprintln!("{}", operator_note(&diff_runs(&a, &b)));
}
