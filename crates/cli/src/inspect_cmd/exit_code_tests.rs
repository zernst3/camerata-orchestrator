//! The process exit codes of `camerata inspect`: each way a run can end short of complete has its
//! own, so a script can tell them apart.

use super::*;

fn outcome() -> InspectOutcome {
    InspectOutcome {
        export_path: PathBuf::from("x.zip"),
        export_zip_bytes: 0,
        backend: ProjectBackend::Api,
        batch: false,
        actual_usage: None,
        findings_count: 0,
        ledger_summary: String::new(),
        ledger_path: PathBuf::from("ledger.json"),
        release_gate: String::new(),
        stage_telemetry: None,
        preflight: None,
        efficiency: camerata_server::efficiency::EfficiencyConfig::default(),
        cost_cap: None,
        outage: None,
        incomplete_reasons: Vec::new(),
    }
}

fn outage_record() -> camerata_server::run_health::OutageRecord {
    camerata_server::run_health::OutageRecord {
        trigger: camerata_server::run_health::OutageTrigger {
            class: camerata_server::cost_cap::FailureClass::Transport,
            detail: "3 model calls in a row could not reach the provider".to_string(),
            at: "2026-10-28T10:00:00Z".to_string(),
        },
        not_run_passes: Vec::new(),
        not_run_stages: Vec::new(),
        repos_not_finished: Vec::new(),
    }
}

#[test]
fn the_incomplete_run_exit_codes_are_all_different_and_nonzero() {
    let codes = [EXIT_RELEASE_BLOCKED, EXIT_COST_CAPPED, EXIT_OUTAGE_STOPPED];
    for (i, a) in codes.iter().enumerate() {
        assert_ne!(*a, 0);
        assert_ne!(*a, 1, "1 is a plain failure");
        for b in &codes[i + 1..] {
            assert_ne!(a, b);
        }
    }
    assert_eq!(EXIT_OUTAGE_STOPPED, 5);
}

#[test]
fn a_complete_outcome_exits_zero() {
    assert_eq!(outcome().exit_code(), 0);
}

#[test]
fn an_outage_stop_exits_with_its_own_code_and_says_so_in_the_report() {
    let mut o = outcome();
    o.outage = Some(outage_record());
    assert!(o.outage_stopped());
    assert_eq!(o.exit_code(), EXIT_OUTAGE_STOPPED);
    let report = format_outcome_report(&o);
    assert!(report.contains("RUN STOPPED"), "{report}");
    assert!(report.contains("INCOMPLETE"), "{report}");
}
