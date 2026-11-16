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
