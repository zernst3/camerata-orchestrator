//! The advisory's tests. Synthetic data only: every report here is built by hand.

use camerata_api_types::audit::{
    ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind,
};

use super::*;
use crate::ai_audit::{FailedPass, FailureClass};
use crate::review_tree::{FixNode, NodeKind, NodeLevel, ReviewTree, TextSource, TreeNode};

/// One Fix of a fixture: its id, effort, worst severity, how many places, and its category key.
struct Fx {
    id: &'static str,
    effort: Option<&'static str>,
    severity: &'static str,
    places: usize,
    category: &'static str,
}

fn fx(id: &'static str, effort: Option<&'static str>, places: usize) -> Fx {
    Fx {
        id,
        effort,
        severity: "medium",
        places,
        category: "injection",
    }
}

fn subgroup_node(id: &str, rule: &str) -> TreeNode {
    TreeNode {
        id: id.to_string(),
        level: NodeLevel::Subgroup,
        parent: Some(rule.to_string()),
        label: id.to_string(),
        rule_ids: Vec::new(),
        kind: NodeKind::Bulk,
        cause: None,
        members: Vec::new(),
        outliers: Vec::new(),
        summary: None,
        summary_source: TextSource::Model,
        escalations: Vec::new(),
        flags: Vec::new(),
    }
}

/// A report of `code_lines` lines whose tree holds `fixes`, each a sub-group of its own with one
/// defect row per place, plus `extra` rows (conventions and such) outside any Fix.
fn report_of(code_lines: usize, fixes: &[Fx], extra: Vec<Finding>) -> ScanReport {
    let mut report = crate::release_gate::test_scan_report();
    report.code_lines = code_lines;
    report.run_id = Some("run-1".to_string());
    report.provenance.finished_at = "2026-11-15T10:00:00Z".to_string();
    report.findings = Vec::new();
    let mut tree = ReviewTree::default();
    for f in fixes {
        let rule_id = format!("RULE-{}-1", f.id.to_ascii_uppercase());
        let sub = format!("sub:{}", f.id);
        let rule = format!("rule:{rule_id}");
        let cat = format!("cat:{}", f.category);
        let mut places = Vec::new();
        for p in 0..f.places {
            let place = FixPlace {
                repo: "acme/portal".to_string(),
                path: format!("src/{}.rs", f.id),
                line: p + 1,
            };
            report.findings.push(Finding {
                repo: place.repo.clone(),
                path: place.path.clone(),
                line: place.line,
                rule_id: rule_id.clone(),
                severity: f.severity.to_string(),
                effort: f.effort.map(str::to_string),
                kind: Some(FindingKind::Defect),
                group_path: vec![cat.clone(), rule.clone(), sub.clone()],
                ..Finding::default()
            });
            places.push(place);
        }
        tree.nodes.push(subgroup_node(&sub, &rule));
        tree.fixes.push(FixNode {
            id: f.id.to_string(),
            members: vec![sub],
            places,
            rules: vec![rule_id],
            shares: Vec::new(),
        });
    }
    report.findings.extend(extra);
    report.tree = Some(tree);
    report
}

/// `n` low-effort Fixes of one place each, named `l0`, `l1`, ...
fn lows(n: usize) -> Vec<Fx> {
    (0..n)
        .map(|i| fx(leak(format!("l{i}")), Some("low"), 1))
        .collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn advisory(report: &ScanReport) -> RewriteAdvisory {
    compute(report, None)
}

const BIG: usize = 1_000_000;

// ── Signal A ────────────────────────────────────────────────────────────────────────────────

/// 2,000 lines: 15 hours at the low end is 7.5 per 1,000 lines, so 30 hours is exactly 15.
#[test]
fn fix_hours_trip_only_over_fifteen_per_thousand_lines_at_the_low_end() {
    for (fixes, low_per_kloc, over) in [(14, 14.0, false), (15, 15.0, false), (16, 16.0, true)] {
        let a = advisory(&report_of(2_000, &lows(fixes), Vec::new()));
        let hours = &a.rewrite_signals.fix_hours_per_kloc;
        assert_eq!(hours.low_hours, 2 * fixes as u64);
        assert_eq!(hours.high_hours, 4 * fixes as u64);
        assert_eq!(hours.low, low_per_kloc, "{fixes} Fixes");
        assert_eq!(hours.high, low_per_kloc * 2.0, "{fixes} Fixes");
        assert_eq!(hours.over, over, "{fixes} Fixes: {hours:?}");
        assert_eq!(a.signals_over, usize::from(over));
    }
}

// ── Signal B ────────────────────────────────────────────────────────────────────────────────

fn structural_fixtures(structural: usize, other: usize) -> Vec<Fx> {
    let mut out: Vec<Fx> = (0..structural)
        .map(|i| Fx {
            category: ARCHITECTURE_CATEGORY_KEY,
            ..fx(leak(format!("s{i}")), Some("low"), 1)
        })
        .collect();
    out.extend((0..other).map(|i| fx(leak(format!("o{i}")), Some("low"), 1)));
    out
}

#[test]
fn the_structural_share_trips_only_over_forty_percent() {
    for (structural, other, pct, over) in
        [(1, 4, 20.0, false), (2, 3, 40.0, false), (3, 3, 50.0, true)]
    {
        let a = advisory(&report_of(
            BIG,
            &structural_fixtures(structural, other),
            Vec::new(),
        ));
        let s = &a.rewrite_signals.structural_share;
        assert_eq!(s.structural_low_hours, 2 * structural as u64);
        assert_eq!(s.low_hours, 2 * (structural + other) as u64);
        assert_eq!(s.percent, pct, "{s:?}");
        assert_eq!(
            s.over,
            over,
            "{structural} of {}: {s:?}",
            structural + other
        );
    }
}

/// The architecture category is the tree's `cat:arch-conformance`, which the export shows as
/// "Architecture": the name the design asked to confirm.
#[test]
fn the_architecture_category_is_the_one_the_export_calls_architecture() {
    assert_eq!(
        crate::report_export::category_display_label(ARCHITECTURE_CATEGORY_KEY),
        "Architecture"
    );
}

/// A rule whose enforcement tier is architectural is structural wherever the tree puts it.
#[test]
fn a_rule_of_the_architectural_tier_is_structural_in_any_category() {
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(architectural_rule("RULE-O0-1"));
    let report = report_of(BIG, &structural_fixtures(0, 2), Vec::new());
    let a = compute(&report, Some(&corpus));
    let s = &a.rewrite_signals.structural_share;
    assert_eq!((s.structural_low_hours, s.low_hours), (2, 4), "{s:?}");
    assert!(s.over, "50 percent is over 40: {s:?}");
    assert!(!advisory(&report).rewrite_signals.structural_share.over);
}

fn architectural_rule(id: &str) -> camerata_rules::Rule {
    let mut rule = camerata_rules::bare_rule(id, "test-fixture");
    rule.enforcement = camerata_rules::EnforcementKind::Architectural;
    rule
}

// ── Signal C ────────────────────────────────────────────────────────────────────────────────

fn convention_row(category: &str, rule: &str, kind: FindingKind) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "src/lib.rs".to_string(),
        line: 1,
        rule_id: rule.to_string(),
        severity: "info".to_string(),
        kind: Some(kind),
        group_path: vec![
            format!("cat:{category}"),
            format!("rule:{rule}"),
            format!("sub:{rule}/1"),
        ],
        ..Finding::default()
    }
}

fn observed() -> FindingKind {
    FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "a".to_string(),
        convention_label: "A".to_string(),
    })
}

fn departed() -> FindingKind {
    FindingKind::Deviation(DeviationKind {
        deviating_sites: 1,
        assessed_sites: Some(10),
        convention_option_id: Some("a".to_string()),
        convention_label: "A".to_string(),
    })
}

fn none_kept() -> FindingKind {
    FindingKind::NoConvention(NoConventionKind {
        observed_options: Vec::new(),
    })
}

/// An area of ten assessed rules, `unsettled` of them departing or with no convention.
fn area(category: &str, unsettled: usize) -> Vec<Finding> {
    (0..10)
        .map(|i| {
            let rule = format!("{}-R{i}", category.to_ascii_uppercase());
            let kind = match i {
                i if i < unsettled && i % 2 == 0 => departed(),
                i if i < unsettled => none_kept(),
                _ => observed(),
            };
            convention_row(category, &rule, kind)
        })
        .collect()
}

#[test]
fn an_area_is_unsettled_only_over_thirty_percent_of_its_assessed_rules() {
    for (unsettled, is_unsettled) in [(2, false), (3, false), (4, true)] {
        let a = advisory(&report_of(BIG, &[], area("testing-style", unsettled)));
        let c = &a.rewrite_signals.unsettled_areas;
        assert_eq!(c.areas.len(), 1);
        assert_eq!(c.areas[0].category, "Testing");
        assert_eq!(c.areas[0].assessed_rules, 10);
        assert_eq!(c.areas[0].unsettled_rules, unsettled);
        assert_eq!(c.areas[0].unsettled, is_unsettled, "{c:?}");
    }
}

#[test]
fn unsettled_areas_trip_only_over_half_of_the_assessed_areas() {
    let cats = [
        "injection",
        "authorization",
        "performance",
        "error-handling",
    ];
    for (unsettled_areas, over) in [(1, false), (2, false), (3, true)] {
        let mut rows = Vec::new();
        for (i, cat) in cats.iter().enumerate() {
            rows.extend(area(cat, if i < unsettled_areas { 5 } else { 0 }));
        }
        // A row that is not about a convention assesses nothing.
        rows.push(convention_row(
            "secret-exposure",
            "SEC-1",
            FindingKind::Defect,
        ));
        let a = advisory(&report_of(BIG, &[], rows));
        let c = &a.rewrite_signals.unsettled_areas;
        assert_eq!(c.assessed, 4, "{c:?}");
        assert_eq!(c.unsettled, unsettled_areas);
        assert_eq!(c.over, over, "{unsettled_areas} of 4: {c:?}");
    }
}

// ── Signal D ────────────────────────────────────────────────────────────────────────────────

/// 100 places: one high-effort Fix of `n` and the rest in low-effort Fixes of one place each.
fn one_high(n: usize) -> Vec<Fx> {
    let mut out = vec![fx("high", Some("high"), n)];
    out.extend(lows(100 - n));
    out
}

#[test]
fn one_high_effort_fix_trips_only_over_a_quarter_of_the_places() {
    for (n, over) in [(24, false), (25, false), (26, true)] {
        let a = advisory(&report_of(BIG, &one_high(n), Vec::new()));
        let d = &a.rewrite_signals.repeat_cause_share;
        assert_eq!(d.total_places, 100);
        let largest = d.largest_high.as_ref().expect("a high-effort Fix");
        assert_eq!((largest.fix.as_str(), largest.places), ("high", n));
        assert_eq!(largest.percent, n as f64);
        assert_eq!(d.over, over, "{n} of 100: {d:?}");
    }
}

/// 100 places: three medium Fixes of `n` places between them and the rest in low-effort Fixes,
/// plus a fourth, smaller medium Fix that is not in the top three.
fn three_medium(n: usize) -> Vec<Fx> {
    let a = n / 3;
    let mut out = vec![
        fx("m1", Some("medium"), n - 2 * a),
        fx("m2", Some("medium"), a),
        fx("m3", Some("medium"), a),
        fx("m4", Some("medium"), 1),
    ];
    out.extend(lows(100 - n - 1));
    out
}

#[test]
fn the_top_three_medium_or_high_fixes_trip_only_over_half_of_the_places() {
    for (n, over) in [(49, false), (50, false), (51, true)] {
        let a = advisory(&report_of(BIG, &three_medium(n), Vec::new()));
        let d = &a.rewrite_signals.repeat_cause_share;
        assert_eq!(d.total_places, 100);
        assert!(d.largest_high.is_none());
        assert_eq!(d.top_three.fixes, vec!["m1", "m2", "m3"], "{d:?}");
        assert_eq!(d.top_three.places, n);
        assert_eq!(d.over, over, "{n} of 100: {d:?}");
    }
}

#[test]
fn a_low_effort_fix_never_trips_it() {
    let mut fixes = vec![fx("big", Some("low"), 90)];
    fixes.extend(lows(10));
    let a = advisory(&report_of(BIG, &fixes, Vec::new()));
    let d = &a.rewrite_signals.repeat_cause_share;
    assert!(
        d.largest_high.is_none() && d.top_three.fixes.is_empty(),
        "{d:?}"
    );
    assert!(!d.over, "{d:?}");
}

// ── Firing ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn one_signal_over_does_not_fire_and_two_do() {
    // A alone: 16 hours per 1,000 lines.
    let one = advisory(&report_of(2_000, &lows(16), Vec::new()));
    assert_eq!(one.signals_over, 1);
    assert_eq!(one.outcome, Outcome::NotFired);
    assert_eq!(one.lines[0], "No rewrite signal this run.");
    assert!(
        one.lines[1].ends_with("Over its threshold."),
        "{:?}",
        one.lines
    );
    // A and B: the same hours, every Fix structural.
    let mut fixes = lows(16);
    for f in &mut fixes {
        f.category = ARCHITECTURE_CATEGORY_KEY;
    }
    let two = advisory(&report_of(2_000, &fixes, Vec::new()));
    assert_eq!(two.signals_over, 2);
    assert_eq!(two.outcome, Outcome::Fired);
    assert_eq!(
        two.lines[0],
        "Worth a conversation: fix or rewrite. Two of four signals are over their starting \
         thresholds."
    );
    assert_eq!(
        two.lines.last().map(String::as_str),
        Some(
            "These thresholds are guesses, not measured. Effort figures are rough bands. Every \
             Fix has an estimate. The decision is yours."
        )
    );
}

#[test]
fn the_text_has_one_line_per_signal_and_the_context_line() {
    let mut fixes = lows(16);
    for f in &mut fixes[..4] {
        f.severity = "high";
    }
    fixes[0].severity = "critical";
    let a = advisory(&report_of(2_000, &fixes, Vec::new()));
    assert_eq!(a.lines.len(), 7, "{:?}", a.lines);
    assert!(a.lines[1].starts_with("Fix hours against code size: 16.0 to 32.0 hours"));
    assert!(a.lines[2].starts_with("Structural share: 0.0 percent"));
    assert!(a.lines[3].starts_with("Areas without a settled habit: 0 of 0"));
    assert!(a.lines[4].starts_with("One cause repeated widely: no Fix has a high effort"));
    assert_eq!(a.context.critical_high_fixes, 4);
    assert_eq!(
        a.lines[5],
        "Context, not a signal: 2.0 critical or high Fixes per 1,000 counted lines (4 in all)."
    );
}

// ── Guards ──────────────────────────────────────────────────────────────────────────────────

fn assert_guarded(a: &RewriteAdvisory, reason: GuardReason) {
    assert_eq!(a.outcome, Outcome::NotEnoughToSay, "{a:?}");
    assert!(
        a.guards.iter().any(|g| g.reason == reason),
        "{:?}",
        a.guards
    );
    assert_eq!(
        a.lines[0],
        "Not enough to say whether there is a rewrite signal."
    );
    assert_eq!(a.lines.len(), 1 + a.guards.len(), "{:?}", a.lines);
}

#[test]
fn fewer_than_two_thousand_counted_lines_is_not_enough_to_say() {
    // Over A and B: the numbers are kept, the text says only why there is not enough.
    let mut fixes = lows(16);
    for f in &mut fixes {
        f.category = ARCHITECTURE_CATEGORY_KEY;
    }
    let a = advisory(&report_of(1_999, &fixes, Vec::new()));
    assert_guarded(&a, GuardReason::FewLines);
    assert_eq!(a.signals_over, 2, "the raw numbers are still saved");
    assert_eq!(
        a.lines[1],
        "Fewer than 2,000 counted lines: this run counted 1,999."
    );
    let enough = advisory(&report_of(2_000, &fixes, Vec::new()));
    assert!(enough.guards.is_empty(), "{:?}", enough.guards);
}

#[test]
fn over_a_fifth_of_the_fixes_without_an_estimate_is_not_enough_to_say() {
    // One of five is exactly 20 percent: not over.
    let mut fixes = lows(4);
    fixes.push(fx("none1", None, 1));
    assert!(advisory(&report_of(BIG, &fixes, Vec::new()))
        .guards
        .is_empty());
    fixes.push(fx("none2", None, 1));
    let a = advisory(&report_of(BIG, &fixes, Vec::new()));
    assert_guarded(&a, GuardReason::ManyUnestimated);
    assert_eq!(
        a.lines[1],
        "2 of 6 Fixes (33.3 percent) have no effort estimate, over the 20 percent limit."
    );
}

#[test]
fn an_incomplete_run_is_not_enough_to_say() {
    let mut report = report_of(BIG, &lows(3), Vec::new());
    report.ai_error = Some("Anthropic API HTTP 401".to_string());
    let a = advisory(&report);
    assert_guarded(&a, GuardReason::IncompleteRun);
    assert_eq!(
        a.lines[1],
        "The run did not complete: the AI review failed."
    );
    // The provider's message is not copied into the text.
    assert!(!a.to_json().contains("HTTP 401"));

    let mut stopped = report_of(BIG, &lows(3), Vec::new());
    stopped
        .failed_passes
        .push(failed_part(FailureClass::SpendLimit, &["src/a.rs"]));
    let a = advisory(&stopped);
    assert_guarded(&a, GuardReason::IncompleteRun);
    assert_eq!(
        a.lines[1],
        "The run did not complete: the spend limit or an outage stopped it."
    );
    assert_eq!(
        a.files_not_reviewed, 0,
        "a stopped part is the guard, not a gap"
    );

    let mut no_repo = report_of(BIG, &lows(3), Vec::new());
    no_repo.repos_not_inspected = vec![("acme/worker".to_string(), "gone".to_string())];
    assert_guarded(&advisory(&no_repo), GuardReason::IncompleteRun);
}

/// One incomplete-run cause alone gives the guard, with the sentence for that cause and no other.
fn assert_only_cause(report: &ScanReport, sentence: &str, what: &str) {
    let a = advisory(report);
    assert_eq!(a.outcome, Outcome::NotEnoughToSay, "{what}: {a:?}");
    assert_eq!(
        a.guards
            .iter()
            .map(|g| (g.reason, g.detail.as_str()))
            .collect::<Vec<_>>(),
        vec![(GuardReason::IncompleteRun, sentence)],
        "{what}"
    );
}

/// Each cause of an incomplete run holds the guard on its own: no scan (gated, then blocked),
/// the AI review not run, the ledger's outage stop and the ledger's spend cap.
#[test]
fn each_incomplete_run_cause_holds_the_guard_alone() {
    let base = || report_of(BIG, &lows(3), Vec::new());
    assert!(advisory(&base()).guards.is_empty());

    let mut gated = base();
    gated.gated = true;
    assert_only_cause(&gated, text::NO_SCAN, "gated");
    let mut blocked = base();
    blocked.blocked = true;
    assert_only_cause(&blocked, text::NO_SCAN, "blocked");

    let mut ai_blocked = base();
    ai_blocked.ai_blocked_reason = Some("no key".to_string());
    assert_only_cause(&ai_blocked, text::AI_NOT_RUN, "ai_blocked_reason");

    let mut outage = base();
    outage
        .ledger
        .record_outage(crate::run_health::OutageRecord {
            trigger: crate::run_health::OutageTrigger {
                class: FailureClass::OutageStop,
                detail: "provider unreachable".to_string(),
                at: "2026-11-15T10:00:00Z".to_string(),
            },
            not_run_passes: Vec::new(),
            not_run_stages: Vec::new(),
            repos_not_finished: Vec::new(),
        });
    assert!(outage.ledger.is_outage_stopped());
    assert!(!outage.ledger.is_cost_capped());
    assert_only_cause(&outage, text::RUN_STOPPED, "ledger outage stop");

    let mut capped = base();
    capped
        .ledger
        .record_cost_cap(crate::cost_cap::CostCapRecord {
            repos_not_finished: vec!["acme/api".to_string()],
            ..Default::default()
        });
    assert!(capped.ledger.is_cost_capped());
    assert!(!capped.ledger.is_outage_stopped());
    assert_only_cause(&capped, text::RUN_STOPPED, "ledger spend cap");
}

#[test]
fn a_run_with_no_review_tree_is_not_enough_to_say() {
    let mut report = report_of(BIG, &lows(3), Vec::new());
    report.tree = None;
    let a = advisory(&report);
    assert_guarded(&a, GuardReason::NoReviewTree);
    assert_eq!(a.fixes.counted, 0);
}

fn failed_part(class: FailureClass, files: &[&str]) -> FailedPass {
    FailedPass {
        repo: "acme/portal".to_string(),
        pass: "audit pass[all] 1/2".to_string(),
        reason: "overloaded".to_string(),
        files_not_reviewed: files.len(),
        class,
        files: files.iter().map(|f| (*f).to_string()).collect(),
        attempts: Vec::new(),
        scope_note: false,
    }
}

/// A part that failed is a coverage gap: it does not block the advisory, and the numbers say
/// they are understated by the files it did not review.
#[test]
fn a_coverage_gap_does_not_block_it_and_says_the_numbers_are_understated() {
    let mut report = report_of(2_000, &lows(16), Vec::new());
    report.failed_passes.push(failed_part(
        FailureClass::Transport,
        &["src/a.rs", "src/b.rs"],
    ));
    // The same file twice is one file.
    report
        .failed_passes
        .push(failed_part(FailureClass::Transport, &["src/b.rs"]));
    let mut scope = failed_part(FailureClass::Transport, &["vendor/x.rs"]);
    scope.scope_note = true;
    report.failed_passes.push(scope);
    let a = advisory(&report);
    assert!(a.guards.is_empty(), "{:?}", a.guards);
    assert_eq!(a.outcome, Outcome::NotFired);
    assert_eq!(a.files_not_reviewed, 2);
    assert!(
        a.lines
            .contains(&"Understated: 2 files not reviewed.".to_string()),
        "{:?}",
        a.lines
    );
}

#[test]
fn fixes_with_no_estimate_are_left_out_and_counted() {
    let mut fixes = lows(16);
    fixes.push(fx("unknown", None, 50));
    for f in &mut fixes {
        f.category = ARCHITECTURE_CATEGORY_KEY;
    }
    // 17 Fixes, one without an estimate (5.9 percent, under the guard).
    let a = advisory(&report_of(2_000, &fixes, Vec::new()));
    assert_eq!(
        a.fixes,
        FixCount {
            counted: 17,
            without_estimate: 1
        }
    );
    assert_eq!(a.rewrite_signals.fix_hours_per_kloc.low_hours, 32);
    assert_eq!(
        a.rewrite_signals.repeat_cause_share.total_places, 16,
        "its places are left out of signal D too"
    );
    assert_eq!(a.outcome, Outcome::Fired);
    assert!(
        a.lines
            .last()
            .is_some_and(|l| l.contains("1 Fix has no estimate and is left out.")),
        "{:?}",
        a.lines
    );
}

/// An effort that is not low, medium or high adds no hours, so it is no estimate: left out and
/// counted as having none, not counted as estimated.
#[test]
fn a_fix_with_an_unrecognised_effort_has_no_estimate() {
    let mut fixes = lows(4);
    fixes.push(fx("odd", Some("extreme"), 40));
    let a = advisory(&report_of(BIG, &fixes, Vec::new()));
    assert_eq!(
        a.fixes,
        FixCount {
            counted: 5,
            without_estimate: 1
        }
    );
    assert_eq!(a.rewrite_signals.repeat_cause_share.total_places, 4);
    assert_eq!(a.rewrite_signals.fix_hours_per_kloc.low_hours, 8);
}

/// The not-fired text carries the same caveats as the fired text: the Fixes left out and the
/// thresholds being guesses.
#[test]
fn the_not_fired_text_carries_the_left_out_count_and_the_caveat() {
    let caveat = "These thresholds are guesses, not measured. Effort figures are rough bands.";
    let none_left_out = advisory(&report_of(BIG, &lows(3), Vec::new()));
    assert_eq!(none_left_out.outcome, Outcome::NotFired);
    let last = none_left_out.lines.last().expect("text");
    assert!(last.contains(caveat), "{:?}", none_left_out.lines);
    assert!(!last.contains("have no estimate"), "{last}");

    let mut fixes = lows(8);
    fixes.push(fx("none1", None, 1));
    let one = advisory(&report_of(BIG, &fixes, Vec::new()));
    assert_eq!(one.outcome, Outcome::NotFired);
    let last = one.lines.last().expect("text");
    assert!(
        last.contains(caveat) && last.contains("1 Fix has no estimate and is left out."),
        "{:?}",
        one.lines
    );
    fixes.push(fx("none2", None, 1));
    let two = advisory(&report_of(BIG, &fixes, Vec::new()));
    assert_eq!(two.outcome, Outcome::NotFired);
    assert!(
        two.lines
            .last()
            .is_some_and(|l| l.contains("2 Fixes have no estimate and are left out.")),
        "{:?}",
        two.lines
    );
}

/// A Fix's effort is the Fix plan's: the most common effort of each sub-group's rows, then the
/// largest of those. A row with no estimate of its own takes none.
#[test]
fn a_fix_takes_the_most_common_effort_of_its_rows() {
    let mut report = report_of(BIG, &[fx("mixed", Some("low"), 3)], Vec::new());
    report.findings[0].effort = Some("high".to_string());
    let a = advisory(&report);
    assert_eq!(
        a.rewrite_signals.fix_hours_per_kloc.low_hours, 2,
        "low wins two to one"
    );
    report.findings[1].effort = Some("high".to_string());
    let a = advisory(&report);
    assert_eq!(
        a.rewrite_signals.fix_hours_per_kloc.low_hours, 24,
        "high wins two to one"
    );
}

// ── Determinism, words and keys ─────────────────────────────────────────────────────────────

fn fired_fixture() -> ScanReport {
    let mut fixes = one_high(30);
    for f in fixes.iter_mut().take(40) {
        f.category = ARCHITECTURE_CATEGORY_KEY;
    }
    let mut rows = area("injection", 5);
    rows.extend(area("performance", 5));
    report_of(2_000, &fixes, rows)
}

#[test]
fn the_same_run_gives_byte_identical_json() {
    let report = fired_fixture();
    let first = advisory(&report).to_json();
    for _ in 0..5 {
        assert_eq!(advisory(&report.clone()).to_json(), first);
    }
    // And the row order does not change it.
    let mut reversed = report.clone();
    reversed.findings.reverse();
    assert_eq!(advisory(&reversed).to_json(), first);
}

/// Every outcome's text and the Markdown: no word that advises, no dash, no serial comma.
#[test]
fn the_text_never_advises_and_keeps_the_house_style() {
    let fired = advisory(&fired_fixture());
    assert_eq!(fired.outcome, Outcome::Fired, "{:?}", fired.lines);
    let not_fired = advisory(&report_of(BIG, &lows(3), Vec::new()));
    let mut gap = report_of(1_000, &[fx("x", None, 1)], Vec::new());
    gap.ai_error = Some("e".to_string());
    gap.tree = None;
    let guarded = advisory(&gap);
    for a in [&fired, &not_fired, &guarded] {
        let text = format!("{}\n{}", a.lines.join("\n"), render_md(a));
        let lower = text.to_lowercase();
        for word in lower.split(|c: char| !c.is_alphanumeric()) {
            assert!(
                !matches!(
                    word,
                    "should" | "recommend" | "recommended" | "recommends" | "must"
                ),
                "{word:?} in {text}"
            );
        }
        for banned in ['\u{2014}', '\u{2013}', '\u{d7}'] {
            assert!(!text.contains(banned), "{banned:?} in {text}");
        }
        assert!(
            !text.contains(", and ") && !text.contains(", or "),
            "a serial comma in {text}"
        );
    }
}

/// The advisory's own sentences are tied to the deny list: the text of every outcome holds a
/// deny-listed phrase, so the gate would refuse the advisory itself if it reached a package.
#[test]
fn every_outcome_text_holds_a_deny_listed_term() {
    let fired = advisory(&fired_fixture());
    assert_eq!(fired.outcome, Outcome::Fired);
    let not_fired = advisory(&report_of(BIG, &lows(3), Vec::new()));
    assert_eq!(not_fired.outcome, Outcome::NotFired);
    let mut gap = report_of(1_000, &[fx("x", None, 1)], Vec::new());
    gap.ai_error = Some("e".to_string());
    let guarded = advisory(&gap);
    assert_eq!(guarded.outcome, Outcome::NotEnoughToSay);
    for (what, a) in [
        ("fired", &fired),
        ("not fired", &not_fired),
        ("not enough to say", &guarded),
    ] {
        assert!(
            !camerata_api_types::rewrite_advisory::terms_in(&a.lines.join("\n")).is_empty(),
            "{what}: no deny-listed term in {:?}",
            a.lines
        );
    }
}

/// Every key the advisory serializes is either deny-listed or one of the ordinary words, so a
/// field added without being listed fails here.
#[test]
fn every_serialized_key_is_on_the_deny_list_or_an_ordinary_word() {
    use camerata_api_types::rewrite_advisory::{GENERIC_KEYS, INTERNAL_ONLY_KEYS};
    fn keys(v: &serde_json::Value, out: &mut BTreeSet<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, item) in map {
                    out.insert(k.clone());
                    keys(item, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
            _ => {}
        }
    }
    let mut gap = fired_fixture();
    gap.code_lines = 10;
    let mut seen = BTreeSet::new();
    for a in [advisory(&fired_fixture()), advisory(&gap)] {
        let doc: serde_json::Value = serde_json::from_str(&a.to_json()).expect("json");
        keys(&doc, &mut seen);
    }
    assert!(seen.contains("largest_high") && seen.contains("guards") && seen.contains("areas"));
    for key in &seen {
        assert!(
            INTERNAL_ONLY_KEYS.contains(&key.as_str()) || GENERIC_KEYS.contains(&key.as_str()),
            "{key} is serialized but neither deny-listed nor an ordinary word"
        );
    }
    for key in INTERNAL_ONLY_KEYS {
        assert!(seen.contains(*key), "{key} is listed but never serialized");
    }
}

#[test]
fn the_json_round_trips() {
    let a = advisory(&fired_fixture());
    let back: AdvisoryDocument = serde_json::from_str(&a.to_json()).expect("parses");
    assert_eq!(back.rewrite_advisory, a);
}

// ── On disk ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn write_puts_both_files_next_to_the_ledger_and_keeps_each_runs_numbers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("portal-1234abcd.ledger.json");
    let a = advisory(&fired_fixture());
    let (json, md) = write(&ledger, &a).expect("written");
    assert!(
        json.ends_with("portal-1234abcd.rewrite_advisory.json"),
        "{json:?}"
    );
    assert!(
        md.ends_with("portal-1234abcd.rewrite_advisory.md"),
        "{md:?}"
    );
    assert_eq!(std::fs::read_to_string(&json).expect("json"), a.to_json());
    let text = std::fs::read_to_string(&md).expect("md");
    assert!(
        text.starts_with("# Rewrite advisory (internal)\n"),
        "{text}"
    );
    assert!(text.contains("Operator only: this file is never part of an export."));
    assert!(text.contains("For run run-1, finished 2026-11-15T10:00:00Z. 2,000 counted lines."));
    for line in &a.lines {
        assert!(text.contains(line.as_str()), "{line}");
    }
    let (_, _, history) = paths_next_to(&ledger);
    // The same run written again (a resume) replaces its own line; another run adds one.
    write(&ledger, &a).expect("written again");
    let mut other = a.clone();
    other.run_id = "run-2".to_string();
    write(&ledger, &other).expect("another run");
    let lines: Vec<String> = std::fs::read_to_string(&history)
        .expect("history")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    let ids: Vec<String> = lines.iter().filter_map(|l| run_id_of(l)).collect();
    assert_eq!(ids, vec!["run-1", "run-2"]);
    let first: AdvisoryDocument = serde_json::from_str(&lines[0]).expect("a history line parses");
    assert_eq!(first.rewrite_advisory, a, "the raw numbers are kept whole");
}
