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
fn an_architectural_checker_row_tallies_as_deterministic_not_preview() {
    let mut arch = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical", "public.t");
    arch.preview = true;
    arch.preview_tool = Some(crate::onboard::ARCH_PREVIEW_TOOL.to_string());
    assert_eq!(finding_tier(&arch), "deterministic");

    let mut tool = finding("eslint/no-eval", "a", 1, "high", "x");
    tool.preview_tool = Some("eslint".to_string());
    assert_eq!(finding_tier(&tool), "preview");
}

#[test]
fn a_not_assessed_architectural_row_tallies_as_convention_not_deterministic() {
    use camerata_api_types::audit::{FindingKind, NotAssessedKind, NotAssessedReason};
    let mut arch = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "info", "public.t");
    arch.preview = true;
    arch.preview_tool = Some(crate::onboard::ARCH_PREVIEW_TOOL.to_string());
    arch.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::PassFailed,
        reason_detail: "pass failed".to_string(),
    }));
    assert_eq!(finding_tier(&arch), "convention");

    // The needs-review architectural grade stays a deterministic row.
    let mut needs_review = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "high", "public.t");
    needs_review.preview_tool = Some(crate::onboard::ARCH_PREVIEW_TOOL.to_string());
    needs_review.confidence = Some("needs-review".to_string());
    assert_eq!(finding_tier(&needs_review), "deterministic");
}

#[test]
fn convention_rows_with_no_confidence_are_not_tallied_as_deterministic() {
    use camerata_api_types::audit::{
        ConventionObservedKind, FindingKind, NoConventionKind, NotAssessedKind, NotAssessedReason,
    };
    let kinds = [
        FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: "opt".to_string(),
            convention_label: "Option".to_string(),
        }),
        FindingKind::NoConvention(NoConventionKind {
            observed_options: Vec::new(),
        }),
        FindingKind::NotAssessed(NotAssessedKind {
            reason: NotAssessedReason::ToolFailed,
            reason_detail: "tool failed".to_string(),
        }),
    ];
    for kind in kinds {
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "a", 1, "info", "x");
        f.kind = Some(kind);
        assert!(f.confidence.is_none());
        assert_eq!(finding_tier(&f), "convention");
    }
    // A real deviation or defect with no confidence is still the floor.
    let mut defect = finding("SEC-X-1", "a", 1, "high", "x");
    defect.kind = Some(FindingKind::Defect);
    assert_eq!(finding_tier(&defect), "deterministic");
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
        "matched_by_site",
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

#[test]
fn the_same_defect_quoted_two_lines_apart_matches_by_site() {
    let first = vec![ai("SUPA-X-1", "src/a.ts", 15, "high", "const k = key;")];
    let second = vec![ai(
        "SUPA-X-1",
        "src/a.ts",
        17,
        "high",
        "export const k = key",
    )];
    let diff = diff_runs(&first, &second);
    assert_eq!(diff.matched, 0, "the fingerprint count is unchanged");
    assert_eq!(diff.matched_by_site, 1);
    assert!(diff.only_in_first.is_empty());
    assert!(diff.only_in_second.is_empty());
    assert!(diff.identical());
}

#[test]
fn a_site_match_needs_the_same_rule_path_and_a_line_inside_the_window() {
    let base = ai("SUPA-X-1", "src/a.ts", 15, "high", "one");
    let window = crate::ai_audit::SEMANTIC_MERGE_WINDOW;

    let one = std::slice::from_ref(&base);

    let mut edge = ai("SUPA-X-1", "src/a.ts", 15 + window, "high", "two");
    assert_eq!(diff_runs(one, &[edge.clone()]).matched_by_site, 1);
    edge.line = 15 + window + 1;
    let beyond = diff_runs(one, &[edge]);
    assert_eq!(beyond.matched_by_site, 0);
    assert_eq!(beyond.only_in_first.len(), 1);
    assert_eq!(beyond.only_in_second.len(), 1);

    let other_rule = ai("SUPA-Y-1", "src/a.ts", 16, "high", "two");
    assert_eq!(diff_runs(one, &[other_rule]).matched_by_site, 0);
    let other_path = ai("SUPA-X-1", "src/b.ts", 16, "high", "two");
    assert_eq!(diff_runs(one, &[other_path]).matched_by_site, 0);
}

#[test]
fn each_leftover_pairs_by_site_at_most_once_and_the_closest_line_wins() {
    let first = vec![
        ai("SUPA-X-1", "src/a.ts", 10, "high", "a"),
        ai("SUPA-X-1", "src/a.ts", 20, "high", "b"),
    ];
    let second = vec![ai("SUPA-X-1", "src/a.ts", 12, "high", "c")];
    let diff = diff_runs(&first, &second);
    assert_eq!(diff.matched_by_site, 1);
    assert_eq!(diff.only_in_first.len(), 1);
    assert_eq!(diff.only_in_first[0].line, 20);
    assert!(diff.only_in_second.is_empty());
}

// ── pair_runs: how two runs pair, as indices ────────────────────────────────────────────────

fn pair(first: &[Finding], second: &[Finding]) -> RunPairing {
    let a: Vec<&Finding> = first.iter().collect();
    let b: Vec<&Finding> = second.iter().collect();
    pair_runs(&a, &b)
}

#[test]
fn pair_runs_pairs_the_same_fingerprint_exactly_wherever_the_line_is() {
    let first = vec![
        finding("SEC-X-1", "src/a.rs", 10, "high", "danger()"),
        finding("SEC-X-1", "src/b.rs", 5, "high", "other()"),
    ];
    // The first moved 200 lines and was reformatted; the second is where it was.
    let second = vec![
        finding("SEC-X-1", "src/b.rs", 5, "high", "other()"),
        finding("SEC-X-1", "src/a.rs", 210, "high", "  danger()  "),
    ];
    let p = pair(&first, &second);
    let mut exact = p.exact.clone();
    exact.sort();
    assert_eq!(exact, vec![(0, 1), (1, 0)]);
    assert!(p.by_site.is_empty() && p.only_first.is_empty() && p.only_second.is_empty());
}

#[test]
fn pair_runs_pairs_by_site_only_when_the_snippet_changed_within_the_window() {
    let near = crate::ai_audit::SEMANTIC_MERGE_WINDOW;
    let first = vec![
        // Same rule and file, the snippet quoted differently, a few lines away: by site.
        finding("SEC-X-1", "src/a.rs", 40, "high", "run(user_input)"),
        // Nothing in the second run comes near it.
        finding("SEC-X-1", "src/c.rs", 3, "high", "gone()"),
    ];
    let second = vec![
        finding(
            "SEC-X-1",
            "src/a.rs",
            40 + near,
            "high",
            "run(escape(user_input))",
        ),
        // A different rule at the same place is not the same defect.
        finding("SEC-Y-1", "src/c.rs", 3, "high", "gone()"),
    ];
    let p = pair(&first, &second);
    assert!(p.exact.is_empty());
    assert_eq!(p.by_site, vec![(0, 0)]);
    assert_eq!(p.only_first, vec![1]);
    assert_eq!(p.only_second, vec![1]);
}

#[test]
fn pair_runs_does_not_pair_by_site_beyond_the_window() {
    let far = crate::ai_audit::SEMANTIC_MERGE_WINDOW + 1;
    let first = vec![finding("SEC-X-1", "src/a.rs", 40, "high", "one()")];
    let second = vec![finding("SEC-X-1", "src/a.rs", 40 + far, "high", "two()")];
    let p = pair(&first, &second);
    assert!(p.exact.is_empty() && p.by_site.is_empty());
    assert_eq!(p.only_first, vec![0]);
    assert_eq!(p.only_second, vec![0]);
}

#[test]
fn pair_runs_pairs_each_row_once_and_takes_the_closest_line_first() {
    // One row in the first run, two candidates in the second: the nearer one pairs, the other is
    // left over. Repeats of one fingerprint pair in order, the later copy is the one left over.
    let first = vec![
        finding("SEC-X-1", "src/a.rs", 50, "high", "old()"),
        finding("SEC-Z-1", "src/z.rs", 1, "high", "x()"),
        finding("SEC-Z-1", "src/z.rs", 9, "high", "x()"),
    ];
    let second = vec![
        finding("SEC-X-1", "src/a.rs", 56, "high", "new_far()"),
        finding("SEC-X-1", "src/a.rs", 52, "high", "new_near()"),
        finding("SEC-Z-1", "src/z.rs", 2, "high", "x()"),
    ];
    let p = pair(&first, &second);
    assert_eq!(p.by_site, vec![(0, 1)], "the nearer line wins");
    assert_eq!(p.only_second, vec![0], "the farther candidate is left over");
    assert_eq!(p.exact, vec![(1, 2)], "repeats pair in order");
    assert_eq!(
        p.only_first,
        vec![2],
        "the later repeat is the one left over"
    );
}

#[test]
fn diff_runs_counts_are_the_pairing_counts() {
    let first = vec![
        finding("SEC-X-1", "src/a.rs", 10, "high", "same()"),
        finding("SEC-X-1", "src/a.rs", 40, "high", "edited(1)"),
        finding("SEC-X-1", "src/b.rs", 1, "high", "fixed()"),
    ];
    let second = vec![
        finding("SEC-X-1", "src/a.rs", 10, "high", "same()"),
        finding("SEC-X-1", "src/a.rs", 41, "high", "edited(2)"),
        finding("SEC-X-1", "src/c.rs", 1, "high", "new()"),
    ];
    let p = pair(&first, &second);
    let d = diff_runs(&first, &second);
    assert_eq!(d.matched, p.exact.len());
    assert_eq!(d.matched_by_site, p.by_site.len());
    assert_eq!(d.only_in_first.len(), p.only_first.len());
    assert_eq!(d.only_in_second.len(), p.only_second.len());
}
