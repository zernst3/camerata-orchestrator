use super::*;
use crate::test_support::review_fixtures::dataset_base;
use camerata_api_types::review::EscalationBudget;
use camerata_api_types::review_view::{EscalationBudgetOutcome, ReviewSettingsView};

fn view() -> ReviewSettingsView {
    dataset_base().settings
}

fn draft(v: &ReviewSettingsView) -> SettingsDraft {
    SettingsDraft::from_view(v)
}

/// The formula written out again on whole numbers, as the oracle for the grid: the rate to three
/// decimals, then a ceiling division, never a float multiply.
fn reference(lines: usize, rate: f32, floor: usize) -> usize {
    let milli = (f64::from(rate) * 1000.0).round() as u128;
    let by_rate = (lines as u128 * milli).div_ceil(1_000_000) as usize;
    floor.max(by_rate)
}

#[test]
fn budget_preview_matches_server_formula() {
    let lines = [
        0, 1, 999, 1_000, 1_001, 10_000, 12_345, 99_999, 300_000, 1_234_567,
    ];
    let rates = [0.0_f32, 0.1, 0.3, 0.5, 1.0, 1.5, 2.0, 2.25, 7.7, 10.0];
    let floors = [0_usize, 1, 20, 300, 1_000];
    let mut checked = 0;
    for l in lines {
        for r in rates {
            for f in floors {
                let server = EscalationBudget::compute(r, f, l).budget;
                assert_eq!(budget_for(l, r, f), server, "lines {l} rate {r} floor {f}");
                assert_eq!(
                    budget_for(l, r, f),
                    reference(l, r, f),
                    "lines {l} rate {r} floor {f}"
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 500, "the whole grid ran");
    // The float trap the server's own formula closes: 0.3 per 1,000 lines of 10,000 lines is 3.
    assert_eq!(budget_for(10_000, 0.3, 0), 3);
    // The design's example: the default rate over 300,000 lines.
    assert_eq!(budget_for(300_000, 1.0, 20), 300);
}

#[test]
fn rates_print_as_a_person_types_them() {
    assert_eq!(format_rate(1.0), "1.0");
    assert_eq!(format_rate(0.0), "0.0");
    assert_eq!(format_rate(2.0), "2.0");
    assert_eq!(format_rate(0.5), "0.5");
    assert_eq!(format_rate(2.25), "2.25");
    assert_eq!(format_rate(0.125), "0.125");
    assert_eq!(format_rate(10.0), "10.0");
}

#[test]
fn counts_group_their_thousands() {
    assert_eq!(group_thousands(0), "0");
    assert_eq!(group_thousands(999), "999");
    assert_eq!(group_thousands(1_000), "1,000");
    assert_eq!(group_thousands(300_000), "300,000");
    assert_eq!(group_thousands(1_234_567), "1,234,567");
}

#[test]
fn the_draft_starts_as_the_saved_settings() {
    let v = view();
    let d = draft(&v);
    assert_eq!(d.rate, format_rate(v.escalation.rate_per_kloc));
    assert_eq!(d.floor, v.escalation.floor.to_string());
    assert_eq!(d.sample, v.engine_check.sample_size_per_run.to_string());
    assert_eq!(d.reasons, v.escalation.over_budget_reasons);
}

#[test]
fn a_typed_request_becomes_the_formula_of_the_draft() {
    let v = view();
    let req = EscalationBudgetReq {
        rate_per_kloc: 2.5,
        floor: 7,
        over_budget_reasons: vec![EscalationReason::DataLoss],
    };
    let d = SettingsDraft::with_req(&v, &req);
    assert_eq!(
        (d.rate.as_str(), d.floor.as_str()),
        ("2.5", "7"),
        "the formula is the request's"
    );
    assert_eq!(d.reasons, vec![EscalationReason::DataLoss]);
    assert_eq!(
        d.sample,
        v.engine_check.sample_size_per_run.to_string(),
        "the sample size is untouched"
    );
}

#[test]
fn the_rate_runs_from_0_to_10_and_anything_else_has_a_hint() {
    let v = view();
    let hint = "The rate must be between 0 and 10 escalations per 1,000 lines.";
    for ok in ["0", "0.0", "1", "1.0", "2.25", "10", "10.0", " 3 "] {
        let d = SettingsDraft {
            rate: ok.to_string(),
            ..draft(&v)
        };
        assert_eq!(field_hint(Field::Rate, &d, &v.limits), None, "{ok:?}");
    }
    for bad in [
        "-0.1", "10.01", "11", "abc", "", "NaN", "inf", "1,5", "1e1x",
    ] {
        let d = SettingsDraft {
            rate: bad.to_string(),
            ..draft(&v)
        };
        assert_eq!(
            field_hint(Field::Rate, &d, &v.limits).as_deref(),
            Some(hint),
            "{bad:?}"
        );
    }
}

#[test]
fn the_floor_runs_from_0_to_1000_in_whole_numbers_and_anything_else_has_a_hint() {
    let v = view();
    let hint = "The floor must be a whole number between 0 and 1,000.";
    for ok in ["0", "20", "1000", " 7 "] {
        let d = SettingsDraft {
            floor: ok.to_string(),
            ..draft(&v)
        };
        assert_eq!(field_hint(Field::Floor, &d, &v.limits), None, "{ok:?}");
    }
    for bad in ["1001", "-1", "2.5", "abc", "", "1e3"] {
        let d = SettingsDraft {
            floor: bad.to_string(),
            ..draft(&v)
        };
        assert_eq!(
            field_hint(Field::Floor, &d, &v.limits).as_deref(),
            Some(hint),
            "{bad:?}"
        );
    }
}

#[test]
fn the_sample_size_runs_from_0_to_1000_in_whole_numbers_and_anything_else_has_a_hint() {
    let v = view();
    let hint = "The sample size must be a whole number between 0 and 1,000.";
    for ok in ["0", "50", "1000"] {
        let d = SettingsDraft {
            sample: ok.to_string(),
            ..draft(&v)
        };
        assert_eq!(field_hint(Field::Sample, &d, &v.limits), None, "{ok:?}");
        assert!(sample_size(&d, &v.limits).is_ok());
    }
    for bad in ["1001", "-1", "0.5", "abc", ""] {
        let d = SettingsDraft {
            sample: bad.to_string(),
            ..draft(&v)
        };
        assert_eq!(
            field_hint(Field::Sample, &d, &v.limits).as_deref(),
            Some(hint),
            "{bad:?}"
        );
        assert_eq!(sample_size(&d, &v.limits), Err(hint.to_string()));
    }
}

#[test]
fn the_formula_request_carries_the_typed_rate_floor_and_ticked_reasons() {
    let v = view();
    let d = SettingsDraft {
        rate: "2.0".into(),
        floor: "35".into(),
        reasons: vec![EscalationReason::SecretExposure],
        ..draft(&v)
    };
    let req = escalation_req(&d, &v.limits).unwrap();
    assert_eq!(req.rate_per_kloc, 2.0);
    assert_eq!(req.floor, 35);
    assert_eq!(
        req.over_budget_reasons,
        vec![
            EscalationReason::SecretExposure,
            EscalationReason::EvidenceUnverified
        ],
        "evidence unverified rides along: it is always escalated (D5)"
    );
    let bad = SettingsDraft {
        floor: "x".into(),
        ..d
    };
    assert!(escalation_req(&bad, &v.limits).is_err());
}

#[test]
fn invalid_setting_disables_apply_with_a_hint() {
    let v = view();
    // A rate past the limit.
    let d = SettingsDraft {
        rate: "12.5".into(),
        ..draft(&v)
    };
    let state = formula_apply_state(&v, &d, false, false);
    assert!(!state.enabled());
    match &state {
        ApplyState::Invalid(hint) => {
            assert!(hint.contains("between 0 and 10"), "{hint}");
        }
        other => panic!("expected a hint, got {other:?}"),
    }
    // A floor past the limit, with a good rate: the floor's hint.
    let d = SettingsDraft {
        floor: "5000".into(),
        ..draft(&v)
    };
    match formula_apply_state(&v, &d, false, false) {
        ApplyState::Invalid(hint) => assert!(hint.contains("floor"), "{hint}"),
        other => panic!("expected a hint, got {other:?}"),
    }
    // A sample size past the limit disables the sample save, not the formula.
    let d = SettingsDraft {
        sample: "5000".into(),
        ..draft(&v)
    };
    assert!(matches!(
        sample_apply_state(&v, &d, false, false),
        ApplyState::Invalid(_)
    ));
    assert_eq!(
        formula_apply_state(&v, &d, false, false),
        ApplyState::Unchanged,
        "the formula is untouched by a bad sample size"
    );
}

#[test]
fn apply_is_ready_only_for_a_valid_change_and_never_during_a_write_or_a_run() {
    let v = view();
    let saved = draft(&v);
    assert_eq!(
        formula_apply_state(&v, &saved, false, false),
        ApplyState::Unchanged
    );
    let changed = SettingsDraft {
        rate: "2.0".into(),
        ..saved.clone()
    };
    assert_eq!(
        formula_apply_state(&v, &changed, false, false),
        ApplyState::Ready
    );
    assert_eq!(
        formula_apply_state(&v, &changed, true, false),
        ApplyState::Saving
    );
    assert_eq!(
        formula_apply_state(&v, &changed, false, true),
        ApplyState::RunInFlight
    );
    assert_eq!(
        formula_apply_state(&v, &changed, true, true),
        ApplyState::RunInFlight,
        "a run in flight is the reason shown first"
    );
    // A tick of a reason is a change too.
    let reasons = SettingsDraft {
        reasons: vec![EscalationReason::DataLoss],
        ..saved.clone()
    };
    assert_eq!(
        formula_apply_state(&v, &reasons, false, false),
        ApplyState::Ready
    );
    // The same rate typed with a different spelling is no change.
    let same = SettingsDraft {
        rate: format!("{}0", saved.rate),
        ..saved.clone()
    };
    assert_eq!(
        formula_apply_state(&v, &same, false, false),
        ApplyState::Unchanged
    );
    // Sample size.
    let sample = SettingsDraft {
        sample: "75".into(),
        ..saved.clone()
    };
    assert_eq!(
        sample_apply_state(&v, &sample, false, false),
        ApplyState::Ready
    );
    assert_eq!(
        sample_apply_state(&v, &sample, false, true),
        ApplyState::RunInFlight
    );
    assert_eq!(
        sample_apply_state(&v, &sample, true, false),
        ApplyState::Saving
    );
    assert_eq!(
        sample_apply_state(&v, &saved, false, false),
        ApplyState::Unchanged
    );
}

#[test]
fn enter_applies_when_apply_is_ready_and_escape_always_reverts() {
    assert_eq!(
        field_action("Enter", &ApplyState::Ready),
        FieldAction::Apply
    );
    for not_ready in [
        ApplyState::Unchanged,
        ApplyState::Invalid("x".into()),
        ApplyState::Saving,
        ApplyState::RunInFlight,
    ] {
        assert_eq!(
            field_action("Enter", &not_ready),
            FieldAction::Nothing,
            "{not_ready:?}"
        );
        assert_eq!(
            field_action("Escape", &not_ready),
            FieldAction::Revert,
            "{not_ready:?}"
        );
    }
    assert_eq!(field_action("a", &ApplyState::Ready), FieldAction::Nothing);
    assert_eq!(
        field_action("Tab", &ApplyState::Ready),
        FieldAction::Nothing
    );
}

#[test]
fn escape_puts_the_field_back_to_its_saved_value_and_leaves_the_others() {
    let v = view();
    let typed = SettingsDraft {
        rate: "9".into(),
        floor: "99".into(),
        sample: "999".into(),
        reasons: vec![EscalationReason::DataLoss],
    };
    let saved = draft(&v);
    let r = revert_field(&typed, &v, Field::Rate);
    assert_eq!(
        (&r.rate, &r.floor, &r.sample),
        (&saved.rate, &typed.floor, &typed.sample)
    );
    let r = revert_field(&typed, &v, Field::Floor);
    assert_eq!(
        (&r.rate, &r.floor, &r.sample),
        (&typed.rate, &saved.floor, &typed.sample)
    );
    let r = revert_field(&typed, &v, Field::Sample);
    assert_eq!(
        (&r.rate, &r.floor, &r.sample),
        (&typed.rate, &typed.floor, &saved.sample)
    );
    assert_eq!(r.reasons, typed.reasons, "reasons are not a typed field");
}

#[test]
fn ticking_a_reason_adds_or_removes_it_in_priority_order() {
    let v = view();
    let none = SettingsDraft {
        reasons: Vec::new(),
        ..draft(&v)
    };
    let a = toggle_reason(&none, EscalationReason::EvidenceUnverified);
    let b = toggle_reason(&a, EscalationReason::SecretExposure);
    assert_eq!(
        b.reasons,
        vec![
            EscalationReason::SecretExposure,
            EscalationReason::EvidenceUnverified
        ],
        "kept in the closed list's order whatever the click order"
    );
    let c = toggle_reason(&b, EscalationReason::SecretExposure);
    assert_eq!(c.reasons, vec![EscalationReason::EvidenceUnverified]);
}

#[test]
fn the_lane_offers_the_seven_default_reasons_and_any_other_the_project_holds() {
    let mut v = view();
    assert_eq!(
        reason_choices(&v),
        EscalationReason::default_over_budget_set()
    );
    v.escalation
        .over_budget_reasons
        .push(EscalationReason::RuleMisfit);
    let choices = reason_choices(&v);
    assert!(choices.contains(&EscalationReason::RuleMisfit));
    assert_eq!(choices.len(), 8);
}

#[test]
fn only_a_valid_unsaved_formula_is_worth_a_preview_request() {
    let v = view();
    let saved = draft(&v);
    assert_eq!(preview_request(&v, &saved), None, "nothing changed");
    let bad = SettingsDraft {
        rate: "x".into(),
        ..saved.clone()
    };
    assert_eq!(preview_request(&v, &bad), None, "invalid");
    let changed = SettingsDraft {
        rate: "2.0".into(),
        ..saved
    };
    let req = preview_request(&v, &changed).expect("a changed valid formula");
    assert_eq!(req.rate_per_kloc, 2.0);
    assert_eq!(req.floor, v.escalation.floor);
}

#[test]
fn the_budget_line_recomputes_from_the_typed_rate_and_floor() {
    let v = view();
    let d = SettingsDraft {
        rate: "1.0".into(),
        floor: "20".into(),
        ..draft(&v)
    };
    assert_eq!(
        budget_line(300_000, &d, &v.limits),
        "Counted lines 300,000 \u{2192} budget 300"
    );
    let d = SettingsDraft {
        rate: "2.0".into(),
        ..d
    };
    assert_eq!(
        budget_line(300_000, &d, &v.limits),
        "Counted lines 300,000 \u{2192} budget 600"
    );
    let d = SettingsDraft {
        rate: "0".into(),
        floor: "35".into(),
        ..d
    };
    assert_eq!(
        budget_line(300_000, &d, &v.limits),
        "Counted lines 300,000 \u{2192} budget 35",
        "the floor holds when the rate is 0"
    );
    let bad = SettingsDraft {
        rate: "x".into(),
        ..d
    };
    assert_eq!(
        budget_line(300_000, &bad, &v.limits),
        "Counted lines 300,000, budget not computed",
        "no budget is claimed for a value that cannot be applied, and the line says so"
    );
}

fn outcome(applied: bool, above: usize, below: usize) -> EscalationBudgetOutcome {
    let b = EscalationBudget::compute(1.0, 20, 10_000);
    EscalationBudgetOutcome {
        applied,
        before: b,
        after: b,
        moved_above: above,
        moved_below: below,
    }
}

#[test]
fn the_effect_sentence_says_no_model_call_was_made_once_applied_and_would_before() {
    assert_eq!(
        effect_sentence(&outcome(true, 103, 0)),
        "103 candidates moved above the line. No model call was made."
    );
    assert_eq!(
        effect_sentence(&outcome(true, 1, 0)),
        "1 candidate moved above the line. No model call was made."
    );
    assert_eq!(
        effect_sentence(&outcome(true, 0, 12)),
        "12 candidates moved below the line. No model call was made."
    );
    assert_eq!(
        effect_sentence(&outcome(true, 4, 2)),
        "4 candidates moved above the line and 2 moved below it. No model call was made."
    );
    assert_eq!(
        effect_sentence(&outcome(true, 0, 0)),
        "No candidate crossed the line. No model call was made."
    );
    assert_eq!(
        effect_sentence(&outcome(false, 103, 0)),
        "103 candidates would move above the line. Applying makes no model call."
    );
    assert_eq!(
        effect_sentence(&outcome(false, 0, 0)),
        "No candidate would cross the line. Applying makes no model call."
    );
}

#[test]
fn the_sample_hint_says_what_zero_does_and_the_suggested_size() {
    assert_eq!(
        sample_hint(50),
        "0 turns the lane off; suggested 50 for early engagements. Enter saves, Esc reverts."
    );
}

#[test]
fn no_sentence_the_lane_builds_has_a_dash_or_names_the_environment() {
    let v = view();
    let d = draft(&v);
    let mut all = vec![
        budget_line(300_000, &d, &v.limits),
        sample_hint(50),
        effect_sentence(&outcome(true, 3, 1)),
        effect_sentence(&outcome(false, 3, 1)),
        effect_sentence(&outcome(true, 0, 0)),
    ];
    for f in [Field::Rate, Field::Floor, Field::Sample] {
        let bad = SettingsDraft {
            rate: "x".into(),
            floor: "x".into(),
            sample: "x".into(),
            ..d.clone()
        };
        all.extend(field_hint(f, &bad, &v.limits));
    }
    for s in all {
        assert!(!s.contains('\u{2014}'), "{s}");
        let lower = s.to_lowercase();
        assert!(!lower.contains("env"), "{s}");
    }
}

#[test]
fn each_stage_names_the_step_the_server_route_takes_and_reads_its_own_saved_model() {
    let keys: Vec<&str> = Stage::ALL.iter().map(|s| s.step_key()).collect();
    assert_eq!(keys, ["audit", "calibration", "group_review", "fix"]);
    let m = StageModelsView {
        scan: "a".into(),
        calibration: "b".into(),
        group_review: "c".into(),
        fix: String::new(),
    };
    let current: Vec<&str> = Stage::ALL.iter().map(|s| s.current(&m)).collect();
    assert_eq!(current, ["a", "b", "c", ""]);
    let labels: Vec<&str> = Stage::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(
        labels,
        [
            "Scan model",
            "Calibration (stage 1)",
            "Group review (stage 2)",
            "Fix model"
        ]
    );
    assert_eq!(
        Stage::GroupReview.hint(),
        "one call per rule group, real time"
    );
    assert_eq!(Stage::Calibration.hint(), "per site, batched");
    assert!(Stage::Fix.can_be_unset());
    assert!(!Stage::Scan.can_be_unset() && !Stage::GroupReview.can_be_unset());
}

fn ids(groups: &[(String, Vec<ModelOption>)]) -> Vec<String> {
    groups
        .iter()
        .flat_map(|(_, o)| o.iter().map(|m| m.id.clone()))
        .collect()
}

#[test]
fn a_stage_select_always_offers_the_models_it_currently_holds() {
    let registry = Some(ModelsResp {
        models: vec![
            ModelOption {
                label: "Opus".into(),
                id: "claude-opus-5-5".into(),
                provider: "claude".into(),
            },
            ModelOption {
                label: "Fable".into(),
                id: "claude-fable-5-1".into(),
                provider: "claude".into(),
            },
        ],
        default: String::new(),
        backend: String::new(),
    });
    assert_eq!(
        ids(&model_groups(&registry, "claude-fable-5-1")),
        ["claude-opus-5-5", "claude-fable-5-1"]
    );
    assert_eq!(
        ids(&model_groups(&registry, "my-own-model")),
        ["claude-opus-5-5", "claude-fable-5-1", "my-own-model"],
        "a model the registry does not list is still shown"
    );
    assert_eq!(
        ids(&model_groups(&registry, "")),
        ["claude-opus-5-5", "claude-fable-5-1"],
        "an unset stage adds nothing"
    );
    assert_eq!(
        ids(&model_groups(&None, "claude-opus-5-5")),
        ["claude-opus-5-5"],
        "no registry: the current model alone, never an empty select"
    );
}

#[test]
fn reasons_are_labelled_as_the_spec_words_them() {
    let labels: Vec<&str> = EscalationReason::default_over_budget_set()
        .into_iter()
        .map(reason_label)
        .collect();
    assert_eq!(
        labels,
        [
            "unauthenticated reach",
            "secret exposure",
            "data loss",
            "cross tenant",
            "authorization bypass",
            "injection",
            "evidence unverified"
        ]
    );
    assert_eq!(reason_label(EscalationReason::RuleMisfit), "a rule misfit");
    assert!(reasons_note().contains("critical"));
}

#[test]
fn evidence_unverified_stays_ticked_and_locked() {
    let v = view();
    let all = draft(&v);
    assert!(all.reasons.contains(&EscalationReason::EvidenceUnverified));
    assert!(reason_locked(EscalationReason::EvidenceUnverified));
    assert!(!reason_locked(EscalationReason::SecretExposure));
    // A click on it changes nothing.
    assert_eq!(
        toggle_reason(&all, EscalationReason::EvidenceUnverified),
        all,
        "it cannot be unticked"
    );
    // Whatever the draft holds, the request carries it (the server refuses a set without it).
    let none = SettingsDraft {
        reasons: Vec::new(),
        ..draft(&v)
    };
    let req = escalation_req(&none, &v.limits).unwrap();
    assert_eq!(
        req.over_budget_reasons,
        vec![EscalationReason::EvidenceUnverified]
    );
}

#[test]
fn a_preview_answer_is_kept_only_for_the_request_the_draft_still_describes() {
    let v = view();
    let at_two = SettingsDraft {
        rate: "2.0".into(),
        ..draft(&v)
    };
    let at_three = SettingsDraft {
        rate: "3.0".into(),
        ..draft(&v)
    };
    let invalid = SettingsDraft {
        rate: "x".into(),
        ..draft(&v)
    };
    let asked = preview_request(&v, &at_two).unwrap();
    assert!(preview_is_current(&v, &at_two, &asked), "same draft");
    assert!(
        !preview_is_current(&v, &at_three, &asked),
        "a slow reply for 2.0 must not overwrite the preview of 3.0"
    );
    assert!(
        !preview_is_current(&v, &invalid, &asked),
        "a reply must not come back after the draft turned invalid"
    );
    assert!(
        !preview_is_current(&v, &draft(&v), &asked),
        "nor after the draft went back to the saved formula"
    );
}

#[test]
fn the_fixture_settings_hold_the_seven_default_over_budget_reasons() {
    assert_eq!(
        view().escalation.over_budget_reasons,
        EscalationReason::default_over_budget_set(),
        "the fixture mirrors the server's default set, evidence unverified included"
    );
}
