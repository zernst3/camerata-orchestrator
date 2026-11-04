use std::collections::BTreeSet;

use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use camerata_api_types::edits::{Edit, EditField};
use camerata_api_types::review::EscalationState;
use camerata_api_types::review_view::{
    CorrectionRef, EngineCheckError, EngineCheckVerdict, EngineCheckVerdictReq,
};

use super::*;
use crate::onboard::{Finding, ScanReport};

const RUN: &str = "insp-1";

fn row(category: &str, n: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: format!("src/{category}/{n}.rs"),
        line: n + 1,
        rule_id: format!("RULE-{category}"),
        site_key: format!("key-{category}-{n}"),
        group_path: vec![format!("cat:{category}")],
        ..Finding::default()
    }
}

fn report(spec: &[(&str, usize)], thrown_out: usize) -> ScanReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    for (category, n) in spec {
        report.findings.extend((0..*n).map(|i| row(category, i)));
    }
    report.discarded = (0..thrown_out)
        .map(|i| Finding {
            site_key: format!("key-thrown-{i}"),
            group_path: vec![format!(
                "cat:authorization{}",
                crate::review_tree::THROWN_OUT_SUFFIX
            )],
            ..row("authorization", 900 + i)
        })
        .collect();
    report
}

fn per_category(s: &Sample) -> Vec<(String, usize)> {
    s.kept.iter().map(|(c, k)| (c.clone(), k.len())).collect()
}

#[test]
fn sample_total_is_the_setting_and_is_allocated_by_unread_share_with_a_floor_of_three() {
    let spec = [
        ("authorization", 900),
        ("data-loss", 90),
        ("secrets", 9),
        ("tiny", 1),
        ("small", 3),
    ];
    let r = report(&spec, 0);
    let s = draw_sample(&r, 50, RUN);
    let got = per_category(&s);
    let take = |name: &str| got.iter().find(|(c, _)| c == name).unwrap().1;

    assert_eq!(s.drawn(), 50, "the total is the setting: {got:?}");
    // The floor: three each, or every row a category has when it has fewer.
    assert!(take("secrets") >= CATEGORY_FLOOR);
    assert!(take("small") >= CATEGORY_FLOOR);
    assert_eq!(take("tiny"), 1);
    // Proportional to unread rows, never more than a category has.
    assert!(take("authorization") > take("data-loss"));
    assert!(take("data-loss") > take("secrets"));
    for (category, n) in spec {
        assert!(take(category) <= n);
    }
    assert_eq!(s.unread_total, 1_003);
    assert_eq!(s.unread["secrets"], 9);

    // The same rows give the same sample whatever order they are stored in.
    let mut shuffled = r.clone();
    shuffled.findings.reverse();
    assert_eq!(draw_sample(&shuffled, 50, RUN), s);
    // A different run draws different rows from the same pool.
    assert_ne!(draw_sample(&r, 50, "insp-2").kept, s.kept);
}

#[test]
fn a_floor_is_honoured_even_when_it_puts_the_total_above_the_setting() {
    // Twenty categories of ten: the floors alone are sixty.
    let names: Vec<String> = (0..20).map(|i| format!("cat{i:02}")).collect();
    let spec: Vec<(&str, usize)> = names.iter().map(|n| (n.as_str(), 10)).collect();
    let s = draw_sample(&report(&spec, 0), 50, RUN);
    assert!(per_category(&s).iter().all(|(_, n)| *n == CATEGORY_FLOOR));
    assert_eq!(s.drawn(), 60);
}

#[test]
fn sample_always_draws_the_thrown_out_minimum() {
    let r = report(&[("authorization", 400)], 30);
    // 50: max(5, 10 percent) = 5. 100: 10. 20: still 5.
    for (size, thrown) in [(50, 5), (100, 10), (20, 5)] {
        let s = draw_sample(&r, size, RUN);
        assert_eq!(s.thrown_out.len(), thrown, "size {size}");
        assert_eq!(s.drawn(), size, "the kept rows take the rest of {size}");
        assert_eq!(s.thrown_out_total, 30);
    }
    // All of them when fewer exist; the kept rows take the rest.
    let few = report(&[("authorization", 400)], 3);
    let s = draw_sample(&few, 50, RUN);
    assert_eq!(s.thrown_out.len(), 3);
    assert_eq!(s.drawn(), 50);
    // Thrown-out rows are their own stratum in the cards, after the kept ones.
    let cards = s.cards();
    assert!(cards[..47]
        .iter()
        .all(|(_, _, st)| *st == SampleStratum::Kept));
    assert!(cards[47..]
        .iter()
        .all(|(_, _, st)| *st == SampleStratum::ThrownOut));
}

#[test]
fn sample_size_zero_draws_nothing_and_an_escalated_row_is_never_in_the_kept_pool() {
    let mut r = report(&[("authorization", 40)], 5);
    let s = draw_sample(&r, 0, RUN);
    assert_eq!((s.drawn(), s.unread_total), (0, 40));
    assert!(s.cards().is_empty());

    for f in r.findings.iter_mut().take(10) {
        f.review.escalation_state = EscalationState::Escalated;
    }
    let escalated: BTreeSet<String> = r
        .findings
        .iter()
        .take(10)
        .map(|f| f.site_key.clone())
        .collect();
    let s = draw_sample(&r, 40, RUN);
    assert_eq!(s.unread_total, 30);
    assert!(s.cards().iter().all(|(_, k, _)| !escalated.contains(k)));
}

#[test]
fn the_category_of_a_row_is_its_tree_category_without_the_thrown_out_suffix() {
    let mut f = row("authorization", 1);
    assert_eq!(category_of(&f), "authorization");
    f.group_path = vec!["cat:authorization/thrown-out".to_string()];
    assert_eq!(category_of(&f), "authorization");
    f.group_path.clear();
    f.category = Some("injection".to_string());
    assert_eq!(category_of(&f), "injection");
    f.category = None;
    assert_eq!(category_of(&f), "uncategorised");
}

// ── the hold rule ─────────────────────────────────────────────────────────────────────────

fn verdict(
    key: &str,
    right: bool,
    at: &str,
    correction: Option<CorrectionRef>,
) -> EngineCheckVerdict {
    EngineCheckVerdict {
        key: key.to_string(),
        run_id: RUN.to_string(),
        right,
        wrong: (!right).then_some(EngineCheckError::WrongSeverity),
        correction,
        by: "zach".to_string(),
        at: at.to_string(),
    }
}

fn decision(id: &str, key: &str, at: &str) -> Decision {
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state: DecisionState::Confirmed,
        reason: String::new(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "zach".to_string(),
        decided_at: at.to_string(),
        run_id: RUN.to_string(),
        engine_version: "0".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

fn edit(id: &str, key: &str, at: &str) -> Edit {
    Edit {
        id: id.to_string(),
        key: key.to_string(),
        field: EditField::Severity,
        old: "high".to_string(),
        new: "low".to_string(),
        reason: "r".to_string(),
        by: "zach".to_string(),
        at: at.to_string(),
        run_id: RUN.to_string(),
        fingerprint_at_edit: "f".to_string(),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

#[test]
fn a_no_holds_the_row_until_a_correction_a_decision_or_an_edit_follows_it() {
    let t0 = "2026-11-04T09:00:00Z";
    let t1 = "2026-11-04T09:05:00Z";
    let no = |key| verdict(key, false, t0, None);

    // A Yes holds nothing. A No with nothing after it holds the row.
    assert!(uncorrected_keys(&[verdict("a", true, t0, None)], RUN, &[], &[]).is_empty());
    assert_eq!(uncorrected_keys(&[no("a")], RUN, &[], &[]), vec!["a"]);

    // A correction the verdict names, a decision at or after it, an edit at or after it.
    let named = verdict(
        "a",
        false,
        t0,
        Some(CorrectionRef::Decision("dec-1".into())),
    );
    assert!(uncorrected_keys(&[named], RUN, &[decision("dec-1", "a", t1)], &[]).is_empty());
    let named = verdict("a", false, t0, Some(CorrectionRef::Edit("e-1".into())));
    assert!(uncorrected_keys(&[named], RUN, &[], &[edit("e-1", "a", t1)]).is_empty());
    assert!(uncorrected_keys(&[no("a")], RUN, &[decision("d", "a", t1)], &[]).is_empty());
    assert!(uncorrected_keys(&[no("a")], RUN, &[], &[edit("e", "a", t1)]).is_empty());

    // A record in the same second as the No, or one whose time cannot be read, does not correct it:
    // a doubt never lets a wrong finding through.
    assert_eq!(
        uncorrected_keys(&[no("a")], RUN, &[decision("d", "a", t0)], &[]),
        vec!["a"]
    );
    assert_eq!(
        uncorrected_keys(&[no("a")], RUN, &[decision("d", "a", "later")], &[]),
        vec!["a"]
    );

    // A decision made BEFORE the No does not correct it, and one on another site does not either.
    assert_eq!(
        uncorrected_keys(
            &[no("a")],
            RUN,
            &[decision("d", "a", "2026-11-04T08:00:00Z")],
            &[]
        ),
        vec!["a"]
    );
    assert_eq!(
        uncorrected_keys(&[no("a")], RUN, &[decision("d", "b", t1)], &[]),
        vec!["a"]
    );

    // The latest verdict on a site is the one that counts, and another run's does not.
    let flipped = [no("a"), verdict("a", true, t1, None)];
    assert!(uncorrected_keys(&flipped, RUN, &[], &[]).is_empty());
    assert!(uncorrected_keys(&[no("a")], "insp-2", &[], &[]).is_empty());
}

// ── the store ─────────────────────────────────────────────────────────────────────────────

fn req(right: bool, wrong: Option<EngineCheckError>) -> EngineCheckVerdictReq {
    EngineCheckVerdictReq {
        right,
        wrong,
        correction: None,
        by: String::new(),
    }
}

#[test]
fn the_store_refuses_a_malformed_verdict_and_keeps_the_latest_per_site() {
    let store = EngineCheckStore::new();
    let now = "2026-11-04T09:00:00Z";
    assert_eq!(
        store
            .record("p", "a", &req(false, None), RUN, now)
            .unwrap_err()
            .code(),
        "wrong_required"
    );
    assert_eq!(
        store
            .record(
                "p",
                "a",
                &req(true, Some(EngineCheckError::WrongSeverity)),
                RUN,
                now
            )
            .unwrap_err()
            .code(),
        "yes_carries_nothing"
    );
    assert_eq!(
        store
            .record("p", " ", &req(true, None), RUN, now)
            .unwrap_err()
            .code(),
        "key_required"
    );
    assert!(
        store.list("p").is_empty(),
        "a refused verdict wrote nothing"
    );

    store
        .record(
            "p",
            "a",
            &req(false, Some(EngineCheckError::WrongSeverity)),
            RUN,
            now,
        )
        .unwrap();
    let v = store
        .record("p", "a", &req(true, None), RUN, "2026-11-04T09:01:00Z")
        .unwrap();
    assert_eq!(v.by, DEFAULT_CHECKER);
    assert_eq!(store.list("p").len(), 2, "append only");
    let latest = store.latest_for_run("p", RUN);
    assert_eq!(latest.len(), 1);
    assert!(latest[0].right);
}

#[test]
fn the_store_survives_a_restart_and_moves_a_damaged_file_aside() {
    let dir = std::env::temp_dir().join(format!("camerata-engine-check-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let store = EngineCheckStore::at(dir.clone());
    store
        .record(
            "p",
            "a",
            &req(false, Some(EngineCheckError::NotARealIssue)),
            RUN,
            "2026-11-04T09:00:00Z",
        )
        .unwrap();
    let reopened = EngineCheckStore::at(dir.clone());
    assert_eq!(reopened.list("p"), store.list("p"));

    // A file that does not parse is moved aside and never written over.
    let path = store.file_for("p").unwrap();
    std::fs::write(&path, b"{ not json").unwrap();
    let damaged = EngineCheckStore::at(dir.clone());
    assert!(damaged.is_corrupt("p"));
    assert_eq!(
        damaged
            .record("p", "b", &req(true, None), RUN, "2026-11-04T09:00:00Z")
            .unwrap_err(),
        EngineCheckStoreError::Corrupt
    );
    assert!(path.with_extension("json.corrupt").exists());
    assert!(!path.exists());
}
