//! The finder flags: the checks that refuse a bad combination before any spend, the settings a run
//! saves, and the comparison of two runs (build 4's functions stood in for).

use super::finder::*;
use super::*;
use camerata_server::ai_audit::agent_finder::run_diff::{DiffEntry, RunDiff};
use camerata_server::ai_audit::agent_finder::FinderChoice;

/// A valid maintainer run over a real folder, ready for the checks.
fn args(dir: &Path) -> InspectArgs {
    InspectArgs {
        repo: dir.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: dir.join("out.zip"),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: false,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
        finder: FinderArgs::default(),
    }
}

fn agent(args: &mut InspectArgs) {
    args.finder.finder = FinderChoice::Agent;
}

#[test]
fn the_finder_defaults_to_the_engine_and_the_controls_carry_the_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let c = a.run_controls().unwrap();
    assert_eq!(c.finder, FinderChoice::Engine);
    assert!(c.finder_model.is_none() && c.finder_force_pieces.is_none());
    agent(&mut a);
    a.finder.model = Some("claude-fable-5-2".to_string());
    a.finder.force_pieces = Some(4);
    let c = a.run_controls().unwrap();
    assert_eq!(c.finder, FinderChoice::Agent);
    assert_eq!(c.finder_model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(c.finder_force_pieces, Some(4));
}

#[test]
fn the_finder_flag_parses_agent_and_engine_and_refuses_anything_else() {
    assert_eq!(parse_finder("agent"), Ok(FinderChoice::Agent));
    assert_eq!(parse_finder(" Engine "), Ok(FinderChoice::Engine));
    let why = parse_finder("both").unwrap_err();
    assert!(why.contains("agent") && why.contains("engine"), "{why}");
}

#[test]
fn an_agent_run_with_the_consensus_option_is_refused_before_anything_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    agent(&mut a);
    a.consensus = true;
    let why = validate_args(&a, true).unwrap_err();
    assert!(
        why.contains("--finder agent") && why.contains("--consensus"),
        "{why}"
    );
}

#[test]
fn forcing_pieces_without_the_internal_flag_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    agent(&mut a);
    a.finder.force_pieces = Some(4);
    a.client_engagement = true;
    a.backend = ProjectBackend::Api;
    let why = validate_args(&a, true).unwrap_err();
    assert!(
        why.contains("--finder-force-pieces") && why.contains("--internal"),
        "{why}"
    );
    a.client_engagement = false;
    assert!(
        validate_args(&a, true).is_ok(),
        "an internal run may force pieces"
    );
}

#[test]
fn the_engine_with_or_without_the_other_flags_is_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.consensus = true;
    assert!(validate_args(&a, true).is_ok());
}

#[test]
fn a_run_saved_before_the_flags_existed_loads_with_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let mut saved = serde_json::to_value(&a).unwrap();
    saved.as_object_mut().unwrap().remove("finder");
    let loaded: InspectArgs = serde_json::from_value(saved).unwrap();
    assert_eq!(loaded.finder, FinderArgs::default());
    // And the flags a run was started with are saved with it, so a resume uses the same finder.
    let mut b = args(dir.path());
    agent(&mut b);
    b.finder.force_pieces = Some(3);
    let round: InspectArgs = serde_json::from_value(serde_json::to_value(&b).unwrap()).unwrap();
    assert_eq!(round.finder.finder, FinderChoice::Agent);
    assert_eq!(round.finder.force_pieces, Some(3));
}

fn finding(rule: &str, path: &str, line: usize) -> camerata_server::onboard::Finding {
    camerata_server::onboard::Finding {
        repo: "acme/app".into(),
        path: path.into(),
        line,
        rule_id: rule.into(),
        severity: "high".into(),
        ..Default::default()
    }
}

#[test]
fn a_run_is_recorded_beside_its_ledger_and_read_back_from_a_state_folder() {
    let state = tempfile::tempdir().unwrap();
    let ledger = state.path().join("abc123.ledger.json");
    let path = finder_run_path(&ledger);
    assert_eq!(path.file_name().unwrap(), "abc123.finder-run.json");
    write_finder_run(&path, &[finding("ARCH-ONE-1", "src/a.rs", 3)]).unwrap();
    let back = read_state_folder(state.path()).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!((back[0].rule_id.as_str(), back[0].line), ("ARCH-ONE-1", 3));
    assert_eq!(
        comparison_note_path(&ledger).file_name().unwrap(),
        "abc123.run-difference.md"
    );
}

#[test]
fn a_state_folder_with_no_earlier_run_or_that_is_missing_is_an_error_never_a_silent_pass() {
    let empty = tempfile::tempdir().unwrap();
    let why = read_state_folder(empty.path()).unwrap_err();
    assert!(why.contains("no earlier run"), "{why}");
    let missing = empty.path().join("nope");
    assert!(read_state_folder(&missing)
        .unwrap_err()
        .contains("cannot be read"));
}

#[test]
fn the_comparison_goes_through_the_two_functions_it_is_given() {
    // Stand-ins for build 4's `diff_runs` and `operator_note`.
    let earlier = vec![
        finding("ARCH-ONE-1", "src/a.rs", 3),
        finding("SEC-TWO-1", "src/b.rs", 1),
    ];
    let current = vec![finding("ARCH-ONE-1", "src/a.rs", 3)];
    let diff = |a: &[camerata_server::onboard::Finding],
                b: &[camerata_server::onboard::Finding]| {
        RunDiff {
            only_in_a: a
                .iter()
                .filter(|x| !b.iter().any(|y| y.rule_id == x.rule_id))
                .map(|x| DiffEntry {
                    rule_id: x.rule_id.clone(),
                    path: x.path.clone(),
                    line: x.line,
                    severity: x.severity.clone(),
                    title: String::new(),
                })
                .collect(),
            only_in_b: Vec::new(),
            in_both: 1,
        }
    };
    let note = |d: &RunDiff| {
        format!(
            "only in the earlier run: {}; in both: {}",
            d.only_in_a.len(),
            d.in_both
        )
    };
    assert_eq!(
        comparison_note(&earlier, &current, &diff, &note),
        "only in the earlier run: 1; in both: 1"
    );
}

#[test]
fn the_agent_cannot_be_built_without_a_key_on_the_api_backend() {
    let controls = camerata_server::cost_cap::RunControls::default();
    let rooms = tempfile::tempdir().unwrap();
    let why = build_agent(
        &controls,
        camerata_server::llm::Backend::Api,
        None,
        rooms.path(),
    )
    .err()
    .expect("no key, no agent");
    assert!(why.contains("key"), "{why}");
    assert!(build_agent(
        &controls,
        camerata_server::llm::Backend::Api,
        Some("test-key"),
        rooms.path()
    )
    .is_ok());
}
