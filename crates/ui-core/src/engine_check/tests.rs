//! The Engine Check lane's judgments, over the review fixtures.

use camerata_api_types::review_view::{
    BreadcrumbItem, EngineCheckVerdict, FlaggedByView, OutlierView, StratumRate,
    SMALL_CATEGORY_DRAWN,
};

use super::*;
use crate::test_support::review_fixtures::{
    build, dataset_base, EngineMode, Params, ReviewDataSet, SEED,
};

fn data(mode: EngineMode) -> ReviewDataSet {
    let mut p = Params::base(SEED);
    p.engine_mode = mode;
    build(&p)
}

fn lane(mode: EngineMode) -> EngineCheckLane {
    data(mode).engine
}

fn card0() -> EngineCheckCard {
    dataset_base().engine.cards[0].clone()
}

fn verdict(right: bool, carried: Option<&str>) -> EngineCheckVerdict {
    EngineCheckVerdict {
        key: "k".to_string(),
        run_id: "insp-2".to_string(),
        right,
        wrong: (!right).then_some(EngineCheckError::WrongSeverity),
        correction: None,
        by: "architect".to_string(),
        at: "2026-11-04T09:00:00Z".to_string(),
        carried_from: carried.map(str::to_string),
    }
}

// ── The closed list ───────────────────────────────────────────────────────────────────────

#[test]
fn the_list_offers_every_choice_once_with_a_digit_and_words() {
    let mut digits = String::new();
    for e in EngineCheckError::ALL {
        let d = error_digit(e).expect("every listed choice has a digit");
        digits.push(d);
        assert_eq!(error_for_digit(&d.to_string()), Some(e));
        assert!(!error_label(e).is_empty());
    }
    assert_eq!(digits, "12345678");
    assert_eq!(error_for_digit("0"), None);
    assert_eq!(error_for_digit("9"), None);
    assert_eq!(error_for_digit("12"), None);
    assert_eq!(error_for_digit("a"), None);
    // The thrown-out answer is not in the list the reviewer picks from.
    assert_eq!(error_digit(EngineCheckError::ThrownOutRealProblem), None);
    assert_eq!(
        error_label(EngineCheckError::GroupSummaryDoesNotDescribe),
        "Group summary does not describe this finding"
    );
}

// ── Honest labels ─────────────────────────────────────────────────────────────────────────

#[test]
fn engine_check_rates_never_say_accuracy() {
    // Run over every state the lane draws, with verdicts in, and over the words themselves.
    let word = ["accu", "racy"].concat();
    for mode in [
        EngineMode::Pending,
        EngineMode::NoWithoutCorrection,
        EngineMode::NoCorrected,
        EngineMode::AllVerdicts,
    ] {
        let l = lane(mode);
        let mut texts: Vec<String> = vec![
            sample_line(&l),
            verdicts_line(&l),
            allocation_line(&l),
            allocation_note(&l).unwrap_or_default(),
        ];
        texts.extend(l.allocation.iter().map(category_tip));
        for r in rate_lines(&l) {
            texts.push(format!("{}: {}", r.label, r.text));
        }
        for card in &l.cards {
            texts.push(card_state_text(&card_state(card)));
        }
        for t in texts {
            assert!(!t.to_lowercase().contains(&word), "{t}");
        }
    }
    // The two rates carry the labels the design gives them, apart from each other.
    let l = lane(EngineMode::AllVerdicts);
    let lines = rate_lines(&l);
    let kept = lines
        .iter()
        .find(|r| r.stratum == SampleStratum::Kept)
        .expect("the kept rate");
    assert_eq!(kept.label, "Error rate among reported findings");
    let thrown = l
        .strata
        .iter()
        .find(|s| s.stratum == SampleStratum::ThrownOut);
    if thrown.is_some_and(|s| s.drawn > 0) {
        let t = lines
            .iter()
            .find(|r| r.stratum == SampleStratum::ThrownOut)
            .expect("the thrown-out rate, apart");
        assert_eq!(t.label, "Real problems among thrown-out findings");
        assert_ne!(t.text, kept.text, "never pooled into one rate");
    }
}

#[test]
fn a_rate_carries_its_margin_and_a_small_category_says_indicative_only() {
    assert_eq!(rate_text(Some(12.9), Some(11.8), false), "13% \u{b1} 12");
    assert_eq!(
        rate_text(Some(50.0), Some(50.0), true),
        "50% \u{b1} 50, indicative only"
    );
    assert_eq!(rate_text(None, None, false), "no verdicts yet");
    let l = lane(EngineMode::AllVerdicts);
    let small = l
        .allocation
        .iter()
        .find(|a| a.indicative_only && a.verdicts > 0)
        .expect("the fixture has a small category with verdicts");
    assert!(
        category_tip(small).ends_with("indicative only"),
        "{}",
        category_tip(small)
    );
}

// ── The lane header ───────────────────────────────────────────────────────────────────────

#[test]
fn lane_header_states_the_floor_when_it_exceeds_the_sample_size() {
    // The spec's ruling-2 case: 60 categories, a sample size of 50, the floor wins.
    let mut l = lane(EngineMode::Pending);
    l.setting = 50;
    l.drawn = 180;
    l.allocation = (0..60)
        .map(|i| CategoryAllocation {
            category: format!("Category {i}"),
            drawn: 3,
            unread: 40,
            verdicts: 0,
            wrong: 0,
            rate_percent: None,
            margin_percent: None,
            indicative_only: true,
        })
        .collect();
    assert_eq!(
        sample_line(&l),
        "180 cards: at least 3 from each of 60 categories, above your sample size of 50"
    );
    let note = allocation_note(&l).unwrap_or_default();
    assert!(
        !note.contains("Allocated by unread share"),
        "the floor sentence already says how it was shared: {note}"
    );

    // Within the setting, the plain line.
    let mut plain = lane(EngineMode::Pending);
    plain.setting = plain.drawn;
    assert_eq!(
        sample_line(&plain),
        format!(
            "Sample {} of this run's {} unread rows (setting: {})",
            thousands(plain.drawn),
            thousands(plain.unread_total),
            thousands(plain.setting)
        )
    );
    assert!(allocation_note(&plain)
        .unwrap_or_default()
        .starts_with("Allocated by unread share, at least 3 per category"));
}

#[test]
fn the_fixture_lanes_header_reads_the_numbers_it_has() {
    let l = lane(EngineMode::Pending);
    assert!(sample_line(&l).contains(&thousands(l.drawn)));
    assert!(!allocation_line(&l).is_empty());
    assert_eq!(
        verdicts_line(&l),
        format!("Verdicts 0 of {}", l.cards.len())
    );
    assert!(!all_verdicts_in(&l));
    let full = lane(EngineMode::AllVerdicts);
    assert!(all_verdicts_in(&full));
    assert_eq!(
        verdicts_line(&full),
        format!("Verdicts {n} of {n}", n = full.cards.len())
    );
    assert_eq!(card_position(31, 50), "Card 32 of 50");
}

// ── The card ──────────────────────────────────────────────────────────────────────────────

#[test]
fn engine_check_no_without_correction_marks_held() {
    let l = lane(EngineMode::NoWithoutCorrection);
    assert_eq!(held_count(&l), 1);
    let held = l
        .cards
        .iter()
        .find(|c| matches!(card_state(c), CardState::Held { .. }))
        .expect("a held card");
    assert_eq!(
        held.site.held,
        Some(HoldReason::EngineCheckNoWithoutCorrection)
    );
    let text = card_state_text(&card_state(held));
    assert!(
        text.starts_with("No: held from export until corrected"),
        "{text}"
    );
    // The fresh lane holds nothing; a corrected No holds nothing.
    assert_eq!(held_count(&lane(EngineMode::Pending)), 0);
    assert_eq!(held_count(&lane(EngineMode::NoCorrected)), 0);
    // A No with no correction posts the verdict the server holds the row on.
    let req = no_request(EngineCheckError::WrongSeverity, None);
    assert!(!req.right && req.correction.is_none());
    assert_eq!(req.wrong, Some(EngineCheckError::WrongSeverity));
}

#[test]
fn a_carried_no_is_labelled_as_carried() {
    let mut card = card0();
    card.site.held = Some(HoldReason::EngineCheckNoWithoutCorrection);
    card.verdict = Some(verdict(false, Some("insp-1")));
    let state = card_state(&card);
    assert_eq!(
        state,
        CardState::Held {
            error: Some(EngineCheckError::WrongSeverity),
            carried_from: Some("insp-1".to_string())
        }
    );
    let text = card_state_text(&state);
    assert!(
        text.starts_with("No from an earlier run, still uncorrected"),
        "{text}"
    );
    assert!(
        !answered(&card),
        "an earlier run's No is not an answer given here"
    );

    // The same No given on this run is a plain held No.
    card.verdict = Some(verdict(false, None));
    assert!(card_state_text(&card_state(&card)).starts_with("No: held"));
    assert!(answered(&card));

    // And once the row is corrected the carried No reads as corrected, not as pending.
    card.verdict = Some(verdict(false, Some("insp-1")));
    card.site.held = None;
    assert!(matches!(card_state(&card), CardState::Corrected { .. }));
}

#[test]
fn the_four_states_read_as_the_spec_names_them() {
    let pending = card_state_text(&CardState::Pending);
    assert_eq!(pending, "No verdict yet");
    assert!(card_state_text(&CardState::Yes).starts_with("Yes"));
    let corrected = card_state_text(&CardState::Corrected { error: None });
    assert_eq!(
        corrected,
        "No, corrected: the corrected version is what ships"
    );
    let l = lane(EngineMode::NoCorrected);
    assert!(l
        .cards
        .iter()
        .any(|c| matches!(card_state(c), CardState::Corrected { .. })));
    let mut card = card0();
    assert_eq!(card_state(&card), CardState::Pending);
    card.verdict = Some(verdict(true, None));
    assert_eq!(card_state(&card), CardState::Yes);
}

#[test]
fn the_engines_side_of_a_card_shows_every_decision_it_made() {
    let mut card = card0();
    card.stage1.real = "yes".to_string();
    card.stage1.severity = "high".to_string();
    card.stage1.effort = Some("low".to_string());
    assert_eq!(stage1_text(&card), "high \u{b7} real yes \u{b7} effort low");
    card.stage1.real.clear();
    assert_eq!(stage1_text(&card), "Stage 1 did not read this row");
    card.placement.node_path = vec![
        BreadcrumbItem {
            id: "cat:a".into(),
            label: "Authorization".into(),
        },
        BreadcrumbItem {
            id: "rule:R".into(),
            label: "SEC-RLS-1".into(),
        },
        BreadcrumbItem {
            id: "sub:R/1".into(),
            label: "sub 1".into(),
        },
    ];
    assert_eq!(placed_text(&card), "Authorization > SEC-RLS-1 > sub 1");
    assert_eq!(
        escalation_text(&EscalationStatus::NotCandidate),
        "not a candidate"
    );
    assert_eq!(
        escalation_text(&EscalationStatus::BelowLine { rank: 1_212 }),
        "a candidate below the line (rank 1,212)"
    );
    assert!(escalation_text(&EscalationStatus::OverBudget {
        reason: camerata_api_types::review::EscalationReason::SecretExposure,
        rank: 310
    })
    .starts_with("escalated past the budget for secret exposure"));
    card.flagged_by = vec![FlaggedByView {
        rule_id: "no-anon-select".into(),
        tool: Some("eslint".into()),
        site_key: "k".into(),
        path: "a.ts".into(),
        line: 14,
    }];
    assert_eq!(collapsed_text(&card), "eslint/no-anon-select (a.ts:14)");
    card.flagged_by.clear();
    assert_eq!(collapsed_text(&card), "none");
    card.outliers = vec![OutlierView {
        reason: camerata_api_types::review::OutlierReason::DifferentObject,
        why: "a different table".into(),
    }];
    assert_eq!(outlier_text(&card), "different object: a different table");
    card.outliers.clear();
    assert_eq!(outlier_text(&card), "none");
    assert_eq!(
        flags_text(&card),
        if card.review_flags.is_empty() {
            "none".to_string()
        } else {
            card.review_flags.join("; ")
        }
    );
    assert!(!duplicate_text(&card).is_empty());
    card.duplicate_link = None;
    assert_eq!(duplicate_text(&card), "none");
}

#[test]
fn a_thrown_out_card_asks_its_own_question() {
    let mut card = card0();
    assert_eq!(question(&card), "Did the engine get this right?");
    assert_eq!(stratum_label(card.stratum), None);
    card.stratum = SampleStratum::ThrownOut;
    assert_eq!(question(&card), "Was the engine right to throw this out?");
    assert_eq!(
        stratum_label(card.stratum),
        Some("thrown out by the engine")
    );
}

// ── The correction ────────────────────────────────────────────────────────────────────────

#[test]
fn every_choice_opens_a_correction_that_builds_a_request_or_says_what_is_missing() {
    let card = card0();
    let run = "insp-1";
    let key = card.site.site_key.clone();

    // Not a real issue: a false positive, with a reason.
    let mut c = Correction::start(EngineCheckError::NotARealIssue, &card);
    assert_eq!(
        correction_request(&c, &card, run).unwrap_err(),
        "A reason is required to call this row a false positive."
    );
    c.reason = "  the policy is on a view  ".to_string();
    let CorrectionRequest::Decision(d) = correction_request(&c, &card, run).unwrap() else {
        panic!("a decision");
    };
    assert_eq!(
        (
            d.key.as_str(),
            d.state,
            d.reason.as_str(),
            d.run_id.as_str()
        ),
        (
            key.as_str(),
            DecisionState::FalsePositive,
            "the policy is on a view",
            run
        )
    );
    assert_eq!(d.scope, Scope::Site);

    // Wrong severity: an edit of the severity, to one that differs.
    let mut c = Correction::start(EngineCheckError::WrongSeverity, &card);
    assert_eq!(c.value, card.site.severity);
    c.reason = "reachable without sign-in".to_string();
    assert!(correction_request(&c, &card, run)
        .unwrap_err()
        .contains("already"));
    let other = SEVERITIES
        .iter()
        .find(|s| **s != card.site.severity)
        .unwrap();
    c.value = (*other).to_string();
    let CorrectionRequest::Edit(e) = correction_request(&c, &card, run).unwrap() else {
        panic!("an edit");
    };
    assert_eq!(
        (e.field, e.new.as_str(), e.key.as_str()),
        (EditField::Severity, *other, key.as_str())
    );
    c.value = "catastrophic".to_string();
    assert_eq!(
        correction_request(&c, &card, run).unwrap_err(),
        "Pick a severity."
    );

    // Duplicate: a merge into another row, never into itself.
    let mut c = Correction::start(EngineCheckError::DuplicateOfAnotherRow, &card);
    c.reason = "the same query".to_string();
    c.value = String::new();
    assert_eq!(
        correction_request(&c, &card, run).unwrap_err(),
        "Pick the row this one duplicates."
    );
    c.value = key.clone();
    assert_eq!(
        correction_request(&c, &card, run).unwrap_err(),
        "A row cannot be merged into itself."
    );
    c.value = "target-key".to_string();
    let CorrectionRequest::Decision(d) = correction_request(&c, &card, run).unwrap() else {
        panic!("a decision");
    };
    assert_eq!(d.state, DecisionState::MergedInto("target-key".to_string()));

    // Should have been escalated: a site decision that says the reviewer needed it; no reason.
    // A row with no ruling is confirmed; one with a ruling keeps it (see
    // `should_have_been_escalated_keeps_the_ruling_the_row_already_has`).
    let mut unruled = card.clone();
    unruled.site.decision = None;
    let c = Correction::start(EngineCheckError::ShouldHaveBeenEscalated, &unruled);
    assert!(!c.needs_reason());
    let CorrectionRequest::Decision(d) = correction_request(&c, &unruled, run).unwrap() else {
        panic!("a decision");
    };
    assert_eq!(
        (d.state, d.needed_me),
        (DecisionState::Confirmed, Some(true))
    );

    // Should not have been: needed_me false, keeping the ruling the row already has.
    let mut ruled = card.clone();
    ruled.site.decision = dataset_base().rows.iter().find_map(|r| r.decision.clone());
    let d0 = ruled
        .site
        .decision
        .clone()
        .expect("a decided row in the fixture");
    let c = Correction::start(EngineCheckError::ShouldNotHaveBeenEscalated, &ruled);
    let CorrectionRequest::Decision(d) = correction_request(&c, &ruled, run).unwrap() else {
        panic!("a decision");
    };
    assert_eq!(d.needed_me, Some(false));
    if matches!(
        d0.state,
        DecisionState::Confirmed
            | DecisionState::Ignored
            | DecisionState::FalsePositive
            | DecisionState::TechDebtNow
            | DecisionState::TechDebtLater
    ) {
        assert_eq!(d.state, d0.state, "the ruling the row already has is kept");
    }

    // Wrong grouping: a sub-group edit to a name that differs.
    let mut c = Correction::start(EngineCheckError::WrongGrouping, &card);
    c.reason = "different table".to_string();
    assert_eq!(
        correction_request(&c, &card, run).unwrap_err(),
        "The row is already in that sub-group."
    );
    c.value = "orders table".to_string();
    let CorrectionRequest::Edit(e) = correction_request(&c, &card, run).unwrap() else {
        panic!("an edit");
    };
    assert_eq!(
        (e.field, e.new.as_str()),
        (EditField::Subgroup, "orders table")
    );

    // Wording or fix wrong: an edit of the chosen field, changed.
    let mut c = Correction::start(EngineCheckError::WordingOrFixWrong, &card);
    c.reason = "it names the wrong table".to_string();
    assert!(correction_request(&c, &card, run)
        .unwrap_err()
        .contains("unchanged"));
    c = c.with_field(EditField::Fix, &card);
    assert_eq!(c.value, card.site.fix);
    c.value = "Add a policy for role authenticated.".to_string();
    let CorrectionRequest::Edit(e) = correction_request(&c, &card, run).unwrap() else {
        panic!("an edit");
    };
    assert_eq!((e.field, e.key.as_str()), (EditField::Fix, key.as_str()));

    // Group summary: an edit of the sub-group's summary, keyed by the node.
    let mut with_node = card.clone();
    with_node.placement.node_path = vec![BreadcrumbItem {
        id: "sub:R/1".into(),
        label: "sub 1".into(),
    }];
    with_node.placement.summary = Some("Every site here is fine.".to_string());
    let mut c = Correction::start(EngineCheckError::GroupSummaryDoesNotDescribe, &with_node);
    assert_eq!(c.value, "Every site here is fine.");
    c.value = "Most sites are fine; this one is not.".to_string();
    c.reason = "it hides the exception".to_string();
    let CorrectionRequest::Edit(e) = correction_request(&c, &with_node, run).unwrap() else {
        panic!("an edit");
    };
    assert_eq!((e.field, e.key.as_str()), (EditField::Summary, "sub:R/1"));

    // A thrown-out row: put back, with a reason.
    let mut c = Correction::start(EngineCheckError::ThrownOutRealProblem, &card);
    assert_eq!(c.kind, CorrectionKind::Restore);
    assert!(correction_request(&c, &card, run).is_err());
    c.reason = "a real problem: the query is built from input".to_string();
    let CorrectionRequest::Decision(d) = correction_request(&c, &card, run).unwrap() else {
        panic!("a decision");
    };
    assert_eq!(d.state, DecisionState::EngineRestored);
}

#[test]
fn a_committed_correction_names_itself_in_the_verdict() {
    let card = card0();
    let mut c = Correction::start(EngineCheckError::NotARealIssue, &card);
    c.reason = "r".to_string();
    let req = correction_request(&c, &card, "insp-1").unwrap();
    assert_eq!(
        correction_ref(&req, "dec-000007"),
        CorrectionRef::Decision("dec-000007".to_string())
    );
    let mut c = Correction::start(EngineCheckError::WrongSeverity, &card);
    c.value = SEVERITIES
        .iter()
        .find(|s| **s != card.site.severity)
        .unwrap()
        .to_string();
    c.reason = "r".to_string();
    let req = correction_request(&c, &card, "insp-1").unwrap();
    assert_eq!(
        correction_ref(&req, "edit-000003"),
        CorrectionRef::Edit("edit-000003".to_string())
    );
    let v = no_request(
        EngineCheckError::WrongSeverity,
        Some(CorrectionRef::Edit("edit-000003".into())),
    );
    assert_eq!(
        v.correction,
        Some(CorrectionRef::Edit("edit-000003".to_string()))
    );
    let y = yes_request();
    assert!(y.right && y.wrong.is_none() && y.correction.is_none());
}

#[test]
fn merge_targets_lead_with_the_engines_link_and_leave_the_row_itself_out() {
    let mut card = card0();
    let own = card.site.site_key.clone();
    let siblings = vec![
        (own.clone(), "me".to_string()),
        ("b".to_string(), "b.rs:2".to_string()),
        ("c".to_string(), "c.rs:3".to_string()),
    ];
    card.duplicate_link = None;
    let t = merge_targets(&card, &siblings);
    assert_eq!(
        t.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["b", "c"]
    );
    card.duplicate_link = dataset_base()
        .rows
        .iter()
        .find_map(|r| r.possible_duplicate_of.clone());
    let link_key = card.duplicate_link.as_ref().unwrap().key.clone();
    let t = merge_targets(&card, &siblings);
    assert_eq!(t[0].0, link_key);
    assert!(t[0].1.ends_with("(the engine's link)"));
}

// ── The flow ──────────────────────────────────────────────────────────────────────────────

#[test]
fn the_card_flow_walks_yes_no_pick_correct_and_leaves_a_no_uncorrected_on_esc_or_next() {
    let l = lane(EngineMode::Pending);
    let n = l.cards.len();
    let card = &l.cards[0];
    let mut s = LaneState::default();
    assert_eq!((s.card, s.phase.clone()), (0, Phase::Viewing));

    s.open_list();
    assert_eq!(s.phase, Phase::Picking { highlighted: 0 });
    s.move_highlight(FocusMove::Down);
    s.move_highlight(FocusMove::Down);
    assert_eq!(s.phase, Phase::Picking { highlighted: 2 });
    for _ in 0..20 {
        s.move_highlight(FocusMove::Down);
    }
    assert_eq!(
        s.phase,
        Phase::Picking { highlighted: 7 },
        "stops at the last choice"
    );
    s.move_highlight(FocusMove::Up);
    s.pick_highlighted(card);
    let Phase::Correcting(c) = &s.phase else {
        panic!("a correction opens: {:?}", s.phase);
    };
    assert_eq!(c.error, EngineCheckError::ALL[6]);
    assert_eq!(s.pending_no(), Some(EngineCheckError::ALL[6]));

    // Esc leaves the No to be stored with no correction.
    assert_eq!(s.cancel(), Some(EngineCheckError::ALL[6]));
    assert_eq!(s.phase, Phase::Viewing);
    assert_eq!(s.cancel(), None, "nothing is pending any more");

    // A list that was opened and abandoned stores nothing.
    s.open_list();
    assert_eq!(s.cancel(), None);

    // Next and Prev stop at the ends and restart the flow.
    s.pick(EngineCheckError::WrongSeverity, card);
    s.next(n);
    assert_eq!((s.card, s.phase.clone()), (1, Phase::Viewing));
    s.go(n + 10, n);
    assert_eq!(s.card, n - 1);
    s.next(n);
    assert_eq!(s.card, n - 1);
    s.go(0, n);
    s.prev(n);
    assert_eq!(s.card, 0);
}

#[test]
fn an_export_panel_door_to_an_engine_check_row_focuses_that_card() {
    let l = lane(EngineMode::NoWithoutCorrection);
    let at = l
        .cards
        .iter()
        .position(|c| matches!(card_state(c), CardState::Held { .. }))
        .expect("a held card");
    let door = Door::EngineCard {
        key: l.cards[at].site.site_key.clone(),
    };
    assert_eq!(card_for_door(&l, &door), Some(at));
    let mut s = LaneState::default();
    s.open_list();
    assert!(s.focus_door(&l, &door));
    assert_eq!((s.card, s.phase.clone()), (at, Phase::Viewing));
    // A door to a row the sample does not hold, or a tree door, focuses nothing.
    let missing = Door::EngineCard {
        key: "nope".to_string(),
    };
    assert_eq!(card_for_door(&l, &missing), None);
    assert!(!s.focus_door(&l, &missing));
    assert_eq!(s.card, at);
    assert_eq!(card_for_door(&l, &Door::Node { id: "sub:x".into() }), None);
    assert_eq!(door.lane(), crate::tree::shell::Lane::EngineCheck);
    assert_eq!(
        Door::Node { id: "x".into() }.lane(),
        crate::tree::shell::Lane::Tree
    );
}

// ── Keys ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_lane_keys_fire_only_with_the_lane_focused_and_no_modifier() {
    let none = KeyMods::none();
    for phase in [KeyPhase::Viewing, KeyPhase::Picking, KeyPhase::Correcting] {
        for focus in [PaneFocus::Elsewhere, PaneFocus::ReasonPrompt] {
            for key in ["y", "n", "1", "ArrowDown", "Enter", "Escape", "g", "m"] {
                assert_eq!(engine_command(focus, key, none, phase), None, "{key}");
            }
        }
        let ctrl = KeyMods { ctrl: true, ..none };
        assert_eq!(engine_command(PaneFocus::Cursor, "y", ctrl, phase), None);
    }
    assert!(engine_keys_armed(PaneFocus::Cursor));
    assert!(!engine_keys_armed(PaneFocus::ReasonPrompt));

    let v = |key| engine_command(PaneFocus::Cursor, key, none, KeyPhase::Viewing);
    assert_eq!(v("y"), Some(EngineCommand::Yes));
    assert_eq!(v("N"), Some(EngineCommand::No));
    assert_eq!(v("ArrowDown"), Some(EngineCommand::Next));
    assert_eq!(v("ArrowUp"), Some(EngineCommand::Prev));
    assert_eq!(v("g"), Some(EngineCommand::GoToNode));
    assert_eq!(v("m"), Some(EngineCommand::Merge));
    assert_eq!(v("1"), None, "digits pick only in the open list");
    assert_eq!(v("Escape"), None, "nothing to cancel on a card");

    let p = |key| engine_command(PaneFocus::Cursor, key, none, KeyPhase::Picking);
    assert_eq!(
        p("1"),
        Some(EngineCommand::Pick(EngineCheckError::NotARealIssue))
    );
    assert_eq!(
        p("8"),
        Some(EngineCommand::Pick(
            EngineCheckError::GroupSummaryDoesNotDescribe
        ))
    );
    assert_eq!(p("9"), None);
    assert_eq!(p("y"), None, "Y does not answer while the list is open");
    assert_eq!(
        p("ArrowDown"),
        Some(EngineCommand::Highlight(FocusMove::Down))
    );
    assert_eq!(p("Enter"), Some(EngineCommand::PickHighlighted));
    assert_eq!(p("Escape"), Some(EngineCommand::Cancel));

    let c = |key| engine_command(PaneFocus::Cursor, key, none, KeyPhase::Correcting);
    assert_eq!(c("Enter"), Some(EngineCommand::Commit));
    assert_eq!(c("Escape"), Some(EngineCommand::Cancel));
    assert_eq!(c("y"), None);
    assert_eq!(c("ArrowDown"), None);
}

#[test]
fn the_key_hint_names_only_keys_that_act() {
    for phase in [KeyPhase::Viewing, KeyPhase::Picking, KeyPhase::Correcting] {
        assert!(keys_hint(true, phase).starts_with("Keys:"));
        assert!(keys_hint(false, phase).starts_with("Keys are off"));
    }
    assert!(keys_hint(true, KeyPhase::Viewing).contains("Y yes"));
}

// ── The key in the address ────────────────────────────────────────────────────────────────

#[test]
fn a_site_key_with_nul_bytes_and_slashes_is_one_path_segment() {
    let key = "acme/app\0src/a.rs\0f1ngerprint\x001";
    let seg = path_segment(key);
    assert!(
        seg.chars().all(|c| c.is_ascii_hexdigit() || c == '%'),
        "{seg}"
    );
    assert!(!seg.contains('/') && !seg.contains('\0'));
    assert_eq!(seg.matches("%00").count(), 3);
    assert_eq!(seg.matches("%2F").count(), 2);
    assert_eq!(path_segment(""), "");
    // Decoding it again gives the key back.
    let back: Vec<u8> = seg
        .split('%')
        .skip(1)
        .map(|h| u8::from_str_radix(h, 16).unwrap())
        .collect();
    assert_eq!(String::from_utf8(back).unwrap(), key);
}

#[test]
fn no_engine_check_text_has_an_em_dash_or_a_raw_nul() {
    let l = lane(EngineMode::AllVerdicts);
    let mut texts = vec![
        SUBTITLE.to_string(),
        OFF_TEXT.to_string(),
        NO_CARDS.to_string(),
        sample_line(&l),
        verdicts_line(&l),
        allocation_line(&l),
        allocation_note(&l).unwrap_or_default(),
    ];
    texts.extend(
        EngineCheckError::ALL
            .iter()
            .map(|e| error_label(*e).to_string()),
    );
    texts.extend(l.allocation.iter().map(category_tip));
    for card in &l.cards {
        texts.push(card_state_text(&card_state(card)));
        texts.push(stage1_text(card));
        texts.push(placed_text(card));
        texts.push(escalation_text(&card.escalation));
        texts.push(collapsed_text(card));
        texts.push(duplicate_text(card));
        texts.push(outlier_text(card));
        texts.push(flags_text(card));
        for e in EngineCheckError::ALL {
            texts.push(Correction::start(e, card).title().to_string());
        }
    }
    for t in texts {
        assert!(!t.contains('\u{2014}') && !t.contains('\0'), "{t:?}");
    }
}

// ── D1: a No is "corrected" only when something corrected it ────────────────────────────

fn no_card(correction: Option<CorrectionRef>) -> EngineCheckCard {
    let mut card = card0();
    let mut v = verdict(false, None);
    v.correction = correction;
    card.verdict = Some(v);
    card.site.held = None;
    card.site.decision = None;
    card.site.edits.clear();
    card
}

fn a_decision_at(at: &str) -> camerata_api_types::review_view::DecisionView {
    let mut d = dataset_base()
        .rows
        .iter()
        .find_map(|r| r.decision.clone())
        .expect("a decided row in the fixture");
    d.decided_at = at.to_string();
    d
}

#[test]
fn a_thrown_out_no_with_no_correction_is_not_corrected() {
    // Esc after N on a thrown-out card stores a No with no correction. The server holds such a
    // row (see the next test); a card that arrives without the hold must still not read
    // "corrected: the corrected version is what ships".
    let mut card = no_card(None);
    card.stratum = SampleStratum::ThrownOut;
    card.verdict.as_mut().unwrap().wrong = Some(EngineCheckError::ThrownOutRealProblem);
    let state = card_state(&card);
    assert!(
        !matches!(state, CardState::Corrected { .. }),
        "nothing was corrected: {state:?}"
    );
    let text = card_state_text(&state);
    assert!(
        text.starts_with("No: still thrown out, put it back to correct"),
        "{text}"
    );
    assert!(!text.contains("what ships"), "{text}");
    assert!(answered(&card), "the No was given on this run");
    assert_eq!(
        held_count(&EngineCheckLane {
            cards: vec![card],
            ..lane(EngineMode::Pending)
        }),
        0,
        "a card without the gate's hold is not counted as held"
    );
}

#[test]
fn a_thrown_out_no_the_gate_holds_reads_as_held_and_is_counted() {
    // The gate holds a thrown-out row the Engine Check said No to until it is put back or a
    // correction confirms the throw-out, and the card carries that hold.
    let mut card = no_card(None);
    card.stratum = SampleStratum::ThrownOut;
    card.site.held = Some(HoldReason::EngineCheckNoWithoutCorrection);
    card.verdict.as_mut().unwrap().wrong = Some(EngineCheckError::ThrownOutRealProblem);
    let state = card_state(&card);
    assert!(matches!(state, CardState::Held { .. }), "{state:?}");
    assert!(card_state_text(&state).starts_with("No: held from export until corrected"));
    assert_eq!(
        held_count(&EngineCheckLane {
            cards: vec![card],
            ..lane(EngineMode::Pending)
        }),
        1
    );
}

#[test]
fn a_restored_row_is_held_until_the_reviewer_decides_it() {
    // A committed restore names the restore decision, and the gate holds the row until a ruling.
    let mut card = no_card(Some(CorrectionRef::Decision("dec-1".to_string())));
    card.stratum = SampleStratum::ThrownOut;
    card.site.held = Some(HoldReason::RestoredWithoutDecision);
    card.verdict.as_mut().unwrap().wrong = Some(EngineCheckError::ThrownOutRealProblem);
    let state = card_state(&card);
    assert!(
        !matches!(state, CardState::Corrected { .. }),
        "a restored row is not yet what ships: {state:?}"
    );
    let text = card_state_text(&state);
    assert!(
        text.contains("put back into review, held until you decide it"),
        "{text}"
    );
    assert!(!text.contains("what ships"), "{text}");
}

#[test]
fn a_no_is_corrected_only_by_a_named_correction_or_a_later_decision_or_edit() {
    // A named correction.
    let named = no_card(Some(CorrectionRef::Decision("dec-1".to_string())));
    assert!(matches!(card_state(&named), CardState::Corrected { .. }));
    // Nothing named, nothing since: an uncorrected No, whatever the hold says.
    let bare = no_card(None);
    assert!(
        !matches!(card_state(&bare), CardState::Corrected { .. }),
        "{:?}",
        card_state(&bare)
    );
    assert!(matches!(card_state(&bare), CardState::Held { .. }));
    // A decision made after the verdict (09:00:00) corrects it; one made before does not.
    let mut later = no_card(None);
    later.site.decision = Some(a_decision_at("2026-11-04T09:30:00Z"));
    assert!(matches!(card_state(&later), CardState::Corrected { .. }));
    let mut earlier = no_card(None);
    earlier.site.decision = Some(a_decision_at("2026-11-04T08:00:00Z"));
    assert!(!matches!(card_state(&earlier), CardState::Corrected { .. }));
    // An edit made after the verdict corrects it too.
    let mut edited = no_card(None);
    let mut e = camerata_api_types::review_view::EditView {
        id: "edit-1".to_string(),
        field: EditField::Headline,
        old: "a".to_string(),
        new: "b".to_string(),
        reason: "r".to_string(),
        by: "architect".to_string(),
        at: "2026-11-04T10:00:00Z".to_string(),
        stale: false,
        current_value: "b".to_string(),
        reapplied_from: None,
    };
    edited.site.edits = vec![e.clone()];
    assert!(matches!(card_state(&edited), CardState::Corrected { .. }));
    e.at = "2026-11-04T08:59:59Z".to_string();
    edited.site.edits = vec![e];
    assert!(!matches!(card_state(&edited), CardState::Corrected { .. }));
}

// ── D2: escalating keeps the row's ruling ───────────────────────────────────────────────

#[test]
fn should_have_been_escalated_keeps_the_ruling_the_row_already_has() {
    let run = "insp-1";
    for state in [DecisionState::Ignored, DecisionState::FalsePositive] {
        let mut card = card0();
        let mut d = a_decision_at("2026-11-04T08:00:00Z");
        d.state = state.clone();
        d.reason = "not reachable from the internet".to_string();
        card.site.decision = Some(d);
        let c = Correction::start(EngineCheckError::ShouldHaveBeenEscalated, &card);
        let CorrectionRequest::Decision(req) = correction_request(&c, &card, run).unwrap() else {
            panic!("a decision");
        };
        assert_eq!(
            req.state, state,
            "never a ruling the reviewer did not choose"
        );
        assert_eq!(req.needed_me, Some(true));
        assert_eq!(req.reason, "not reachable from the internet");
    }
    // A row with no ruling is confirmed, as before.
    let mut card = card0();
    card.site.decision = None;
    let c = Correction::start(EngineCheckError::ShouldHaveBeenEscalated, &card);
    let CorrectionRequest::Decision(req) = correction_request(&c, &card, run).unwrap() else {
        panic!("a decision");
    };
    assert_eq!(
        (req.state, req.needed_me),
        (DecisionState::Confirmed, Some(true))
    );
}

// ── D4: a rate over a handful of verdicts says so ───────────────────────────────────────

fn kept_mut(l: &mut EngineCheckLane) -> &mut StratumRate {
    l.strata
        .iter_mut()
        .find(|s| s.stratum == SampleStratum::Kept)
        .expect("the kept stratum")
}

#[test]
fn a_stratum_rate_over_few_verdicts_says_indicative_only() {
    let mut l = lane(EngineMode::AllVerdicts);
    // One verdict, wrong: 100 percent, and not a certainty.
    let kept = kept_mut(&mut l);
    kept.drawn = 50;
    kept.verdicts = 1;
    kept.wrong = 1;
    kept.rate_percent = Some(100.0);
    kept.margin_percent = Some(39.7);
    let line = rate_lines(&l)
        .into_iter()
        .find(|r| r.stratum == SampleStratum::Kept)
        .expect("the kept rate line");
    assert_eq!(
        line.text,
        "100% \u{b1} 40, indicative only (1 of 1 checked)"
    );
    // Just under the small-category line is still indicative; at it, the label goes.
    let kept = kept_mut(&mut l);
    kept.verdicts = SMALL_CATEGORY_DRAWN - 1;
    kept.wrong = 2;
    assert!(rate_lines(&l)[0].text.contains("indicative only"));
    kept_mut(&mut l).verdicts = SMALL_CATEGORY_DRAWN;
    assert!(!rate_lines(&l)[0].text.contains("indicative only"));
}

// ── D5: the correction summary ──────────────────────────────────────────────────────────

#[test]
fn a_corrected_card_summarises_the_ruling_and_the_edits_that_corrected_it() {
    // Nothing corrected, nothing to summarise.
    assert!(correction_lines(&card0()).is_empty());
    assert!(correction_lines(&no_card(None)).is_empty());
    // A named decision: its state and its reason.
    let mut ruled = no_card(Some(CorrectionRef::Decision("dec-9".to_string())));
    let mut d = a_decision_at("2026-11-04T09:30:00Z");
    d.id = "dec-9".to_string();
    d.state = DecisionState::FalsePositive;
    d.reason = "the policy is on a view".to_string();
    ruled.site.decision = Some(d);
    assert_eq!(
        correction_lines(&ruled),
        vec!["False positive: the policy is on a view".to_string()]
    );
    // A decision that is neither named nor later is not the correction.
    let mut other = ruled.clone();
    other.verdict.as_mut().unwrap().correction = None;
    other.site.decision.as_mut().unwrap().decided_at = "2026-11-04T08:00:00Z".to_string();
    assert!(correction_lines(&other).is_empty());
    // A named edit: the field, old to new, and why.
    let mut edited = no_card(Some(CorrectionRef::Edit("edit-3".to_string())));
    edited.site.edits = vec![camerata_api_types::review_view::EditView {
        id: "edit-3".to_string(),
        field: EditField::Severity,
        old: "high".to_string(),
        new: "medium".to_string(),
        reason: "behind sign-in".to_string(),
        by: "architect".to_string(),
        at: "2026-11-04T09:10:00Z".to_string(),
        stale: false,
        current_value: "medium".to_string(),
        reapplied_from: None,
    }];
    assert_eq!(
        correction_lines(&edited),
        vec!["Severity: high to medium (behind sign-in)".to_string()]
    );
    // A Yes is not a correction.
    let mut yes = ruled;
    yes.verdict = Some(verdict(true, None));
    assert!(correction_lines(&yes).is_empty());
}

#[test]
fn the_corrected_fixture_names_the_error_its_correction_fixed() {
    let l = lane(EngineMode::NoCorrected);
    let card = l
        .cards
        .iter()
        .find(|c| matches!(card_state(c), CardState::Corrected { .. }))
        .expect("a corrected card");
    let wrong = card.verdict.as_ref().and_then(|v| v.wrong);
    let lines = correction_lines(card);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("False positive"), "{lines:?}");
    assert_eq!(
        wrong,
        Some(EngineCheckError::NotARealIssue),
        "a false-positive ruling corrects 'not a real issue', not 'wrong severity'"
    );
}
