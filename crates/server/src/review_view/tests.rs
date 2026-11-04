use std::time::Instant;

use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use camerata_api_types::review::{
    EscalationBudget, EscalationCandidate, EscalationReason, OutlierReason,
};
use camerata_api_types::review_view::{Marker, MovedFromView, TreeView};

use super::test_support::{compose, row, sealed, Stores, RUN};
use super::{decode_cursor, encode_cursor, severity_rank};
use crate::onboard::Finding;
use crate::report_export::apply_overlay;
use crate::review_tree::{NodeLevel, Outlier, OutlierBy};

fn decision(id: &str, key: &str, state: DecisionState, run: &str) -> Decision {
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state,
        reason: "because".to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "zach".to_string(),
        decided_at: "2026-11-04T09:00:00Z".to_string(),
        run_id: run.to_string(),
        engine_version: "0.0.0".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

fn candidate(
    key: &str,
    node: &str,
    reason: EscalationReason,
    severity: &str,
    rank: u32,
) -> EscalationCandidate {
    EscalationCandidate {
        key: key.to_string(),
        node_id: node.to_string(),
        reason,
        severity: severity.to_string(),
        stage2_rank: rank,
        global_rank: rank,
        escalated: false,
        over_budget_reason: None,
        why: "reachable without signing in".to_string(),
    }
}

/// Twelve rows of one rule and three of another, with stage 2 data: four candidates, an outlier on
/// the first rule's sub-group, and the line at the design's floor.
fn staged() -> crate::onboard::SealedReport {
    let mut rows: Vec<Finding> = (0..12)
        .map(|i| {
            row(
                "SEC-NO-RAW-SQL-CONCAT-1",
                &format!("src/q{i:02}.rs"),
                10 + i,
                "high",
            )
        })
        .collect();
    rows.extend((0..3).map(|i| row("SEC-AUTHZ-4", &format!("src/a{i}.rs"), 5, "critical")));
    sealed(rows, |report| {
        let keys: Vec<(String, String)> = report
            .findings
            .iter()
            .map(|f| (f.path.clone(), f.site_key.clone()))
            .collect();
        let key = |path: &str| keys.iter().find(|(p, _)| p == path).unwrap().1.clone();
        let tree = report.tree.as_mut().unwrap();
        let sql_rule = "rule:SEC-NO-RAW-SQL-CONCAT-1";
        tree.candidates = vec![
            candidate(
                &key("src/a0.rs"),
                "rule:SEC-AUTHZ-4",
                EscalationReason::UnauthenticatedReach,
                "critical",
                1,
            ),
            candidate(
                &key("src/a1.rs"),
                "rule:SEC-AUTHZ-4",
                EscalationReason::SecretExposure,
                "critical",
                2,
            ),
            candidate(
                &key("src/q01.rs"),
                sql_rule,
                EscalationReason::RuleMisfit,
                "high",
                3,
            ),
            candidate(
                &key("src/q02.rs"),
                sql_rule,
                EscalationReason::NeedsMoreCode,
                "low",
                4,
            ),
        ];
        tree.budget = EscalationBudget::compute(1.0, 20, 10_000);
        let allowed = EscalationReason::default_over_budget_set();
        camerata_api_types::review::re_mark(&mut tree.candidates, &mut tree.budget, &allowed);
        tree.group_reviewed = true;
        let sub = tree
            .nodes
            .iter_mut()
            .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&key("src/q00.rs")))
            .unwrap();
        sub.outliers.push(Outlier {
            key: key("src/q03.rs"),
            reason: OutlierReason::DifferentObject,
            by: OutlierBy::Model,
            why: "builds a table name".to_string(),
        });
        let tree = report.tree.take().unwrap();
        tree.sync_rows(&mut report.findings);
        report.tree = Some(tree);
    })
}

fn key_of(report: &crate::onboard::SealedReport, path: &str) -> String {
    report
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap()
        .site_key
        .clone()
}

#[test]
fn node_counts_come_from_the_tree_and_agree_with_the_trees_own_counts() {
    let sealed = staged();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let tree = sealed.tree.as_ref().unwrap();
    let view = compose(&reviewed, &Stores::default(), |c| c.tree());
    assert_eq!(view.nodes.len(), tree.nodes.len());
    for n in &view.nodes {
        let want = tree.counts(&n.id);
        assert_eq!(
            (
                n.counts.sites,
                n.counts.escalated,
                n.counts.below_the_line,
                n.counts.outliers,
                n.counts.unplaced
            ),
            (
                want.sites,
                want.escalated,
                want.below_the_line,
                want.outliers,
                want.unplaced
            ),
            "{}",
            n.id
        );
    }
    let sql = view
        .nodes
        .iter()
        .find(|n| n.id == "rule:SEC-NO-RAW-SQL-CONCAT-1")
        .unwrap();
    assert_eq!(
        (sql.counts.sites, sql.counts.escalated, sql.counts.outliers),
        (12, 2, 1)
    );
}

#[test]
fn the_line_is_marked_again_from_the_current_settings_and_never_stored() {
    let sealed = staged();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let mut stores = Stores::default();
    // The design's floor of 20 puts all four candidates above the line.
    let q = compose(&reviewed, &stores, |c| c.queue(true, 0, 100));
    assert_eq!((q.escalated.len(), q.below_total, q.below.len()), (4, 0, 0));

    // A floor of 1 and no rate: one above, three below. The stored candidates are untouched.
    stores.settings.escalation_rate_per_kloc = 0.0;
    stores.settings.escalation_floor = 1;
    stores.settings.over_budget_reasons = Vec::new();
    let q = compose(&reviewed, &stores, |c| c.queue(false, 0, 100));
    assert_eq!((q.escalated.len(), q.below_total), (1, 3));
    assert!(
        q.below.is_empty(),
        "below the line is listed only when asked"
    );
    assert_eq!(q.budget.budget, 1);
    let q = compose(&reviewed, &stores, |c| c.queue(true, 1, 1));
    assert_eq!((q.below.len(), q.below_offset), (1, 1));
    assert_eq!(
        q.below[0].candidate.global_rank, 3,
        "paged from the second candidate below the line"
    );
    assert!(!q.below[0].candidate.escalated);
    assert!(q.below[0].site.markers.contains(&Marker::Candidate));
    assert_eq!(
        sealed.tree.as_ref().unwrap().budget.escalated,
        4,
        "the sealed report keeps the scan's line"
    );

    // The reasons that may exceed the budget bring a candidate back over the line.
    stores.settings.over_budget_reasons = EscalationReason::default_over_budget_set();
    let q = compose(&reviewed, &stores, |c| c.queue(false, 0, 100));
    let over: Vec<_> = q
        .escalated
        .iter()
        .filter(|r| r.candidate.over_budget_reason.is_some())
        .collect();
    assert_eq!(
        over.len(),
        1,
        "the critical secret exposure goes over the budget of 1"
    );
    assert!(over[0].site.markers.contains(&Marker::OverBudget));
}

#[test]
fn a_site_row_lists_its_markers_in_precedence_order() {
    let sealed = staged();
    let q1 = key_of(&sealed, "src/q01.rs");
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let row = compose(&reviewed, &Stores::default(), |c| {
        c.sites_page("sub:SEC-NO-RAW-SQL-CONCAT-1/0", None, 500)
            .unwrap()
    })
    .rows
    .into_iter()
    .find(|r| r.site_key == q1)
    .unwrap();
    assert_eq!(row.markers, vec![Marker::Escalated]);
    // Whatever markers a row has come out in the precedence the screens draw them in.
    let pos = |m: &Marker| Marker::PRECEDENCE.iter().position(|x| x == m).unwrap();
    let all = compose(&reviewed, &Stores::default(), |c| {
        c.sites_page("rule:SEC-NO-RAW-SQL-CONCAT-1", None, 500)
            .unwrap()
    });
    for r in &all.rows {
        let order: Vec<usize> = r.markers.iter().map(pos).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{:?}", r.markers);
    }
    let outlier_row = all
        .rows
        .iter()
        .find(|r| r.site_key == key_of(&sealed, "src/q03.rs"))
        .unwrap();
    assert!(outlier_row.markers.contains(&Marker::Outlier));
}

#[test]
fn a_decision_from_an_earlier_run_shows_stale_and_moved_and_applies_nothing() {
    // Run 1 saw the snippet one way; the reviewer confirmed it. Run 2 finds the same place with
    // a changed snippet: a new key, next to the old one.
    let run1 = sealed(
        vec![row("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 12, "high")],
        |_| {},
    );
    let old = run1.findings[0].clone();
    let mut changed = row("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 14, "high");
    changed.snippet = "run(\"another shape\", 14)".to_string();
    let run2 = sealed(vec![changed], |_| {});
    let new = run2.findings[0].clone();
    assert_ne!(old.site_key, new.site_key);

    let links = crate::review_tree::match_runs(&run1.findings, &run2.findings).moved;
    assert_eq!(links.len(), 1, "the engine finds the move");
    let mut stores = Stores::default();
    stores.moved.insert(
        new.site_key.clone(),
        MovedFromView {
            key: old.site_key.clone(),
            line: Some(old.line),
        },
    );
    stores.links = links;
    let records = [decision(
        "dec-000001",
        &old.site_key,
        DecisionState::Confirmed,
        "insp-run-1",
    )];
    let reviewed = apply_overlay(&run2, &records, &[], &[]);
    let page = compose(&reviewed, &stores, |c| {
        c.sites_page(&run2.findings[0].group_path[2], None, 500)
            .unwrap()
    });
    let row = &page.rows[0];
    assert!(row.decision.is_none(), "nothing applies by itself");
    let stale = row
        .stale_decision
        .as_ref()
        .expect("the old decision is shown");
    assert_eq!(stale.state, DecisionState::Confirmed);
    assert_eq!(row.moved_from.as_ref().map(|m| m.line), Some(Some(12)));
    assert!(row.markers.contains(&Marker::Stale) && row.markers.contains(&Marker::Moved));

    let shell = compose(&reviewed, &stores, |c| c.shell());
    assert_eq!((shell.stale.stale_decisions, shell.stale.moved), (1, 1));
    assert_eq!(shell.counts.unresolved, 1);
}

#[test]
fn rows_the_engine_threw_out_are_counted_apart_from_the_rows_that_ship() {
    let mut report_rows: Vec<Finding> = (0..4)
        .map(|i| {
            row(
                "SEC-NO-RAW-SQL-CONCAT-1",
                &format!("src/q{i}.rs"),
                3 + i,
                "high",
            )
        })
        .collect();
    report_rows.push(row("SEC-AUTHZ-4", "src/a.rs", 5, "high"));
    let sealed = sealed(report_rows, |report| {
        let mut thrown = row("SEC-NO-RAW-SQL-CONCAT-1", "src/thrown.rs", 3, "high");
        crate::engine_discard::mark(&mut thrown, "its headline is a placeholder");
        report.discarded.push(thrown);
        report.attach_tree();
    });
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let view = compose(&reviewed, &Stores::default(), |c| c.tree());
    let sites = |id: &str| view.nodes.iter().find(|n| n.id == id).unwrap().counts.sites;
    assert_eq!(
        view.nodes.iter().filter(|n| n.thrown_out).count(),
        2,
        "a thrown-out node and its sub-group"
    );
    let thrown: Vec<_> = view.nodes.iter().filter(|n| n.thrown_out).collect();
    assert!(thrown.iter().all(|n| n.counts.sites == 1));
    assert_eq!(sites("rule:SEC-NO-RAW-SQL-CONCAT-1"), 4);
    // The shell counts the rows that ship.
    let shell = compose(&reviewed, &Stores::default(), |c| c.shell());
    assert_eq!(shell.counts.total, 5);
    // A page of a thrown-out node lists the thrown-out row, with its reason.
    let node = thrown
        .iter()
        .find(|n| n.parent.is_some() && n.id.starts_with("sub:"))
        .unwrap();
    let page = compose(&reviewed, &Stores::default(), |c| {
        c.sites_page(&node.id, None, 500).unwrap()
    });
    assert_eq!(page.rows.len(), 1);
    assert_eq!(
        page.rows[0].thrown_out_reason.as_deref(),
        Some("its headline is a placeholder")
    );
}

#[test]
fn a_cursor_names_a_row_and_survives_a_round_trip() {
    let f = row("RULE", "src/a b/c.rs", 42, "critical");
    let mut f = f;
    f.site_key = "acme/app\u{0}src/a b/c.rs\u{0}abc\u{0}0".to_string();
    let c = encode_cursor(&f);
    assert!(c.bytes().all(|b| b.is_ascii_hexdigit()), "{c}");
    assert_eq!(
        decode_cursor(&c),
        Some((0, "src/a b/c.rs".to_string(), 42, f.site_key.clone()))
    );
    for bad in ["", "zz", "abc", "00"] {
        assert_eq!(decode_cursor(bad), None, "{bad}");
    }
    assert!(severity_rank("critical") < severity_rank("high"));
    assert!(severity_rank("info") < severity_rank("whatever"));
}

#[test]
fn engine_check_rates_carry_their_margin_and_small_categories_say_indicative() {
    let (rate, margin) = super::lane::rate_and_margin_for_tests(4, 31);
    assert_eq!((rate, margin), (Some(12.9), Some(11.8)));
    assert_eq!(super::lane::rate_and_margin_for_tests(0, 0), (None, None));
    assert_eq!(
        super::lane::rate_and_margin_for_tests(0, 10),
        (Some(0.0), Some(0.0))
    );
}

// ── 30,000 sites ──────────────────────────────────────────────────────────────────────────

/// 30,000 rows over 60 categories, 8 rules each and 5 sub-groups each, every node with a summary
/// of realistic length.
fn thirty_thousand() -> crate::onboard::SealedReport {
    const ROWS: usize = 30_000;
    let rows: Vec<Finding> = (0..ROWS)
        .map(|i| {
            let s = i / 13;
            let (c, r, k) = (s / 40, (s / 5) % 8, s % 5);
            let rule = format!("RULE-{c:02}-{r}");
            let mut f = row(
                &rule,
                &format!("src/mod{}/file{}.rs", i % 97, i / 7),
                1 + i % 500,
                "medium",
            );
            f.group_path = vec![
                format!("cat:cat{c:02}"),
                format!("rule:{rule}"),
                format!("sub:{rule}/{k}"),
            ];
            f
        })
        .collect();
    sealed(rows, |report| {
        let tree = report.tree.as_mut().unwrap();
        for n in &mut tree.nodes {
            if n.level != NodeLevel::Category {
                n.summary = Some("Every site in this group builds a query from request text and passes it to the database driver without a bound parameter; the fix is the same at each: use the driver's parameter binding. None of the members differs.".to_string());
                n.cause = Some("the query is concatenated from a request field".to_string());
            }
        }
    })
}

#[test]
fn a_thirty_thousand_site_tree_view_is_under_2_mb_and_builds_in_under_2_s() {
    let sealed = thirty_thousand();
    assert_eq!(sealed.findings.len(), 30_000);
    let t = Instant::now();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let overlay_time = t.elapsed();

    let t = Instant::now();
    let (view, index_time, tree_time) = compose(&reviewed, &Stores::default(), |c| {
        let index = t.elapsed();
        let t2 = Instant::now();
        let view = c.tree();
        (view, index, t2.elapsed())
    });
    let build_time = t.elapsed();
    let t = Instant::now();
    let bytes = serde_json::to_vec(&view).unwrap();
    let serialize_time = t.elapsed();
    eprintln!(
        "30,000 sites: {} nodes, TreeView {} bytes; overlay {overlay_time:?}, index {index_time:?}, \
         tree {tree_time:?}, compose total {build_time:?}, serialize {serialize_time:?}",
        view.nodes.len(),
        bytes.len()
    );
    assert!(view.nodes.len() > 2_000, "{} nodes", view.nodes.len());
    assert!(bytes.len() < 2 * 1024 * 1024, "{} bytes", bytes.len());
    assert!(build_time.as_secs_f64() < 2.0, "built in {build_time:?}");
    // Every site is counted once under its category.
    let total: usize = view
        .nodes
        .iter()
        .filter(|n| n.level == camerata_api_types::review_view::TreeLevel::Category)
        .map(|n| n.counts.sites)
        .sum();
    assert_eq!(total, 30_000);
    let _: TreeView = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(RUN, "insp-test-1");
}
