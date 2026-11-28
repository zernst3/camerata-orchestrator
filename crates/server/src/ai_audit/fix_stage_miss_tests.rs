//! Why a finding has no fix, when its batch was never sent: the spend limit and the outage stop
//! (#582) each name themselves.

use super::*;

fn finding(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/acme-api".to_string(),
        path: path.to_string(),
        line,
        rule_id: "AI-UNCHECKED-INPUT".to_string(),
        severity: "high".to_string(),
        ..Finding::default()
    }
}

#[test]
fn a_batch_the_outage_stop_refused_says_so_and_not_the_spend_limit() {
    let findings = vec![finding("src/a.rs", 1), finding("src/b.rs", 2)];
    let mut ledger = FixLedger::default();
    ledger.miss(&[0], FixMiss::CapStopped);
    ledger.miss(&[1], FixMiss::OutageStopped);
    let why = fix_rejections(
        &findings,
        &[0, 1],
        &std::collections::HashMap::new(),
        &ledger,
    );
    assert_eq!(
        why,
        vec![
            "src/a.rs:1 AI-UNCHECKED-INPUT: the spend limit stopped the call".to_string(),
            "src/b.rs:2 AI-UNCHECKED-INPUT: the run stopped because the model provider could not \
             be reached, so the call was not sent"
                .to_string(),
        ]
    );
    // Neither was sent, so neither counts as a transport failure.
    let tally = ledger.outcome(&[0, 1], 0);
    assert_eq!(tally.cap_stopped, 2);
    assert_eq!(tally.transport_failed, 0);
}
