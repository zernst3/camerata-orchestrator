use super::*;

fn finding(rule_id: &str, path: &str, line: usize, severity: &str, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: snippet.to_string(),
        detail: format!("detail for {rule_id}"),
        ..Finding::default()
    }
}

fn ai(rule_id: &str, path: &str, line: usize, severity: &str, snippet: &str) -> Finding {
    let mut f = finding(rule_id, path, line, severity, snippet);
    f.confidence = Some("high".to_string());
    f
}

fn row(rows: &[CountRow], key: &str) -> (usize, usize) {
    rows.iter()
        .find(|r| r.key == key)
        .map(|r| (r.first, r.second))
        .unwrap_or_else(|| panic!("no row {key} in {rows:?}"))
}

#[test]
fn the_match_key_ignores_line_drift_and_whitespace_but_not_the_rule_or_file() {
    let a = finding("SEC-X-1", "src/a.rs", 10, "high", "let t =  token();");
    let mut moved = a.clone();
    moved.line = 12;
    moved.snippet = "let t = token();".to_string();
    assert_eq!(finding_match_key(&a), finding_match_key(&moved));

    let mut other_rule = a.clone();
    other_rule.rule_id = "SEC-Y-1".to_string();
    assert_ne!(finding_match_key(&a), finding_match_key(&other_rule));

    let mut other_file = a.clone();
    other_file.path = "src/b.rs".to_string();
    assert_ne!(finding_match_key(&a), finding_match_key(&other_file));
}

#[test]
fn the_tier_names_the_engine() {
    assert_eq!(
        finding_tier(&finding("SEC-X-1", "a", 1, "high", "x")),
        "deterministic"
    );
    assert_eq!(finding_tier(&ai("SEC-X-1", "a", 1, "high", "x")), "ai");
    assert_eq!(
        finding_tier(&finding("AI-INVENTED-1", "a", 1, "high", "x")),
        "ai"
    );
    let mut preview = finding("eslint/no-eval", "a", 1, "high", "x");
    preview.preview_tool = Some("eslint".to_string());
    assert_eq!(finding_tier(&preview), "preview");
}

#[test]
fn identical_runs_report_no_difference() {
    let run = vec![
        finding("SEC-X-1", "src/a.rs", 1, "high", "a()"),
        ai("ARCH-Y-1", "src/b.rs", 2, "medium", "b()"),
    ];
    let diff = diff_runs(&run, &run);
    assert!(diff.identical());
    assert_eq!(diff.matched, 2);
    assert!(diff.by_tier.iter().all(|r| r.delta() == 0));
    assert!(diff.by_rule.iter().all(|r| r.delta() == 0));
    assert!(diff.by_severity.iter().all(|r| r.delta() == 0));
}

#[test]
fn a_finding_in_one_run_only_is_listed_with_its_side_and_counted_in_every_table() {
    let shared = finding("SEC-X-1", "src/a.rs", 1, "high", "a()");
    let first = vec![
        shared.clone(),
        ai("ARCH-Y-1", "src/b.rs", 2, "medium", "b()"),
    ];
    let second = vec![
        shared,
        ai("ARCH-Z-1", "src/c.rs", 3, "critical", "c()"),
        ai("ARCH-Z-1", "src/d.rs", 4, "critical", "d()"),
    ];
    let diff = diff_runs(&first, &second);
    assert!(!diff.identical());
    assert_eq!(
        (diff.first_total, diff.second_total, diff.matched),
        (2, 3, 1)
    );

    assert_eq!(diff.only_in_first.len(), 1);
    assert_eq!(diff.only_in_first[0].rule_id, "ARCH-Y-1");
    assert_eq!(diff.only_in_first[0].tier, "ai");
    assert_eq!(
        diff.only_in_first[0].fingerprint,
        crate::suppression::fingerprint("ARCH-Y-1", "b()")
    );
    let second_only: Vec<&str> = diff
        .only_in_second
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        second_only,
        vec!["src/c.rs", "src/d.rs"],
        "sorted by repo, path, line"
    );

    assert_eq!(row(&diff.by_tier, "deterministic"), (1, 1));
    assert_eq!(row(&diff.by_tier, "ai"), (1, 2));
    assert_eq!(row(&diff.by_rule, "ARCH-Y-1"), (1, 0));
    assert_eq!(row(&diff.by_rule, "ARCH-Z-1"), (0, 2));
    assert_eq!(row(&diff.by_severity, "critical"), (0, 2));
    assert_eq!(row(&diff.by_severity, "medium"), (1, 0));
    let rule_keys: Vec<&str> = diff.by_rule.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(rule_keys, vec!["ARCH-Y-1", "ARCH-Z-1", "SEC-X-1"], "sorted");
}

#[test]
fn repeats_are_matched_one_for_one() {
    let f = ai("ARCH-Y-1", "src/b.rs", 2, "medium", "b()");
    let mut again = f.clone();
    again.line = 9;
    let diff = diff_runs(&[f.clone(), again], &[f]);
    assert_eq!(diff.matched, 1);
    assert_eq!(
        diff.only_in_first.len(),
        1,
        "the second copy has no partner"
    );
    assert!(diff.only_in_second.is_empty());
}

#[test]
fn the_diff_serializes_every_table() {
    let diff = diff_runs(&[finding("SEC-X-1", "a", 1, "High", "a()")], &[]);
    let json = serde_json::to_value(&diff).unwrap();
    for key in [
        "first_total",
        "second_total",
        "matched",
        "by_tier",
        "by_rule",
        "by_severity",
        "only_in_first",
        "only_in_second",
    ] {
        assert!(json.get(key).is_some(), "missing {key}: {json}");
    }
    assert_eq!(
        json["by_severity"][0]["key"], "high",
        "severity is normalised to lowercase"
    );
}
