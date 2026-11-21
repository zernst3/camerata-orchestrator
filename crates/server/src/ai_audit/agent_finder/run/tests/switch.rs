//! Build item 8: the switch. The settings that cannot go together are refused before any spend.

use super::super::super::FinderChoice;
use super::super::refuse_unsupported_finder_settings as refuse;
use super::*;

fn agent_controls() -> RunControls {
    RunControls {
        finder: FinderChoice::Agent,
        ..RunControls::default()
    }
}

#[test]
fn the_default_finder_is_the_engine_and_asks_for_nothing() {
    assert_eq!(RunControls::default().finder, FinderChoice::Engine);
    assert_eq!(refuse(&RunControls::default(), true, false), Ok(()));
    assert_eq!(refuse(&RunControls::default(), false, true), Ok(()));
}

#[test]
fn the_agent_finder_refuses_the_consensus_option() {
    let mut c = agent_controls();
    c.consensus = true;
    let why = refuse(&c, false, true).unwrap_err();
    assert!(
        why.contains("--finder agent") && why.contains("--consensus"),
        "{why}"
    );
}

#[test]
fn the_agent_finder_refuses_an_incremental_re_inspection() {
    let why = refuse(&agent_controls(), true, true).unwrap_err();
    assert!(
        why.contains("full inspection") && why.contains("--full"),
        "{why}"
    );
    assert_eq!(refuse(&agent_controls(), false, false), Ok(()));
}

#[test]
fn forcing_pieces_needs_an_internal_run() {
    let mut c = agent_controls();
    c.finder_force_pieces = Some(4);
    let why = refuse(&c, false, false).unwrap_err();
    assert!(
        why.contains("--finder-force-pieces") && why.contains("--internal"),
        "{why}"
    );
    assert_eq!(refuse(&c, false, true), Ok(()));
}

#[test]
fn forcing_pieces_without_the_agent_finder_is_refused() {
    let c = RunControls {
        finder_force_pieces: Some(4),
        ..RunControls::default()
    };
    let why = refuse(&c, false, true).unwrap_err();
    assert!(why.contains("--finder agent"), "{why}");
}

#[test]
fn the_refusal_messages_use_no_dashes_or_serial_commas() {
    for (c, inc, internal) in [
        (
            {
                let mut c = agent_controls();
                c.consensus = true;
                c
            },
            false,
            true,
        ),
        (agent_controls(), true, true),
        (
            RunControls {
                finder_force_pieces: Some(2),
                ..agent_controls()
            },
            false,
            false,
        ),
    ] {
        let why = refuse(&c, inc, internal).unwrap_err();
        assert!(
            !why.contains('\u{2014}') && !why.contains('\u{2013}'),
            "{why}"
        );
        assert!(!why.contains(", and ") && !why.contains(", or "), "{why}");
    }
}

fn broken_rule() -> Vec<crate::CorpusLoadFailure> {
    vec![crate::CorpusLoadFailure {
        rule_id: "SEC-EXAMPLE-1".to_string(),
        path: std::path::PathBuf::from("corpus/sec/sec-example-1.toml"),
        error: "unknown variant `typo`".to_string(),
    }]
}

/// Item 4: an agent finder run, client or internal, stops before any spend when a rule file did
/// not load; the message names the rule, the file and the reason. An engine run goes on (the
/// failure is recorded in the ledger and the integrity gate).
#[test]
fn a_rule_file_that_did_not_load_stops_every_agent_run() {
    let why = super::super::refuse_unloadable_rules(&agent_controls(), true, &broken_rule())
        .expect_err("an agent run must not go on with a rule missing");
    assert!(why.contains("SEC-EXAMPLE-1"), "{why}");
    assert!(why.contains("sec-example-1.toml"), "{why}");
    assert!(why.contains("unknown variant"), "{why}");
    assert!(why.contains("Nothing was spent"), "{why}");
    let ok = |c: &RunControls, ai: bool, f: &[crate::CorpusLoadFailure]| {
        super::super::refuse_unloadable_rules(c, ai, f)
    };
    assert_eq!(ok(&RunControls::default(), true, &broken_rule()), Ok(()));
    // A deterministic-only scan runs no finder, so it is not stopped.
    assert_eq!(ok(&agent_controls(), false, &broken_rule()), Ok(()));
    assert_eq!(ok(&agent_controls(), true, &[]), Ok(()));
}
