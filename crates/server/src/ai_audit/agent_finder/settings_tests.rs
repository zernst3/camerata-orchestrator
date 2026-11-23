//! The finder switch on the run's settings (design section 6) and its stage names.

use super::{FinderChoice, DEFAULT_FINDER_MODEL};
use crate::cost_cap::{models_the_run_will_use, RunControls};
use crate::stage_telemetry::Stage;

#[test]
fn the_default_finder_is_the_engine() {
    assert_eq!(FinderChoice::default(), FinderChoice::Engine);
    let c = RunControls::default();
    assert_eq!(c.finder, FinderChoice::Engine);
    assert_eq!(c.finder_model, None);
    assert_eq!(c.finder_force_pieces, None);
}

#[test]
fn todays_model_list_is_unchanged_with_the_engine_finder() {
    let controls = RunControls::default();
    let list = models_the_run_will_use(None, None, false, &[], &controls);
    let roles: Vec<&str> = list.iter().map(|(r, _)| r.as_str()).collect();
    let joined = roles.join("|");
    assert!(joined.contains("the scan"), "{joined}");
    assert!(joined.contains("calibration"), "{joined}");
    assert!(joined.contains("group review"), "{joined}");
    assert!(joined.contains("fix generation"), "{joined}");
    assert!(!joined.contains("the finder"), "{joined}");
    assert!(list
        .iter()
        .all(|(_, m)| m != DEFAULT_FINDER_MODEL || joined.contains("scan")));
}

#[test]
fn the_agent_finder_lists_the_finder_model_and_leaves_out_scan_and_calibration() {
    let controls = RunControls {
        scope: Default::default(),
        finder: FinderChoice::Agent,
        ..RunControls::default()
    };
    let list = models_the_run_will_use(
        Some("claude-scan-pick"),
        Some("claude-cal-pick"),
        false,
        &[],
        &controls,
    );
    let joined: String = list.iter().map(|(r, m)| format!("{r}={m};")).collect();
    assert!(joined.contains("the finder"), "{joined}");
    assert!(
        list.iter()
            .any(|(r, m)| r.contains("the finder") && m == DEFAULT_FINDER_MODEL),
        "{joined}"
    );
    assert!(!joined.contains("the scan"), "{joined}");
    assert!(!joined.contains("calibration"), "{joined}");
    assert!(
        !joined.contains("claude-scan-pick") && !joined.contains("claude-cal-pick"),
        "{joined}"
    );
    assert!(
        joined.contains("group review") && joined.contains("fix generation"),
        "{joined}"
    );
}

#[test]
fn an_explicit_finder_model_is_used_and_a_blank_one_falls_back_to_the_default() {
    let named = RunControls {
        scope: Default::default(),
        finder: FinderChoice::Agent,
        finder_model: Some("  claude-other-model ".to_string()),
        ..RunControls::default()
    };
    assert_eq!(named.finder_model_for(), "claude-other-model");
    let blank = RunControls {
        scope: Default::default(),
        finder_model: Some("   ".to_string()),
        ..RunControls::default()
    };
    assert_eq!(blank.finder_model_for(), DEFAULT_FINDER_MODEL);
}

#[test]
fn the_spend_limits_price_check_sees_the_finder_model() {
    let controls = RunControls {
        scope: Default::default(),
        finder: FinderChoice::Agent,
        finder_model: Some("claude-not-in-the-registry".to_string()),
        max_cost_usd: Some(10.0),
        ..RunControls::default()
    };
    let list = models_the_run_will_use(None, None, false, &[], &controls);
    let refusal = controls
        .refuse_if_cap_unenforceable(&list)
        .expect("an unpriced finder model is refused");
    assert!(refusal.contains("claude-not-in-the-registry"), "{refusal}");
    assert!(refusal.contains("the finder"), "{refusal}");

    let priced = RunControls {
        scope: Default::default(),
        finder: FinderChoice::Agent,
        max_cost_usd: Some(10.0),
        ..RunControls::default()
    };
    let list = models_the_run_will_use(None, None, false, &[], &priced);
    assert_eq!(priced.refuse_if_cap_unenforceable(&list), None);
}

#[test]
fn the_finder_is_a_stage_with_its_own_label_and_meter_name() {
    assert_eq!(Stage::Finder.label(), "finder");
    assert!(Stage::ALL.contains(&Stage::Finder));
    assert_eq!(crate::ai_audit::STAGE_FINDER, "agent finder");
}
