//! `eval` — the precision/recall eval harness for the deterministic audit FLOOR (issue #11).
//!
//! The harness itself lives in `camerata_server::eval` so the same labeled corpus and
//! scoring drive both the in-crate regression test and this CLI command. This module just
//! runs it and prints the metrics table; it exits non-zero if the floor's recall/precision
//! invariants regressed, so it can also serve as a CI gate.

use camerata_server::eval::{run_eval, MIN_PRECISION};

/// Run the deterministic-floor eval, print the per-rule + overall metrics table, and
/// enforce the floor invariants (recall == 1.0 per rule; precision >= the documented
/// bound). Exit 1 on regression so CI can gate on it.
pub fn run_eval_cmd() -> anyhow::Result<()> {
    let report = run_eval();

    println!("== Camerata deterministic-floor precision/recall eval (#11) ==");
    println!("Floor rules (no model, no network): SEC-NO-HARDCODED-SECRETS-1, SEC-NO-RAW-SQL-CONCAT-1, ARCH-NO-SECRETS-IN-URL-1");
    println!();
    print!("{}", report.render_table());
    println!();

    let mut ok = true;
    for m in &report.per_rule {
        if m.recall < 1.0 {
            eprintln!(
                "REGRESSION: {} recall {:.3} < 1.0 ({} planted violation(s) missed)",
                m.rule_id, m.recall, m.false_negatives
            );
            ok = false;
        }
    }
    if report.overall.precision < MIN_PRECISION {
        eprintln!(
            "REGRESSION: overall precision {:.3} < documented bound {:.3} ({} false positive(s))",
            report.overall.precision, MIN_PRECISION, report.overall.false_positives
        );
        ok = false;
    }

    if ok {
        println!(
            "EVAL: PASS — floor recall {:.3}, precision {:.3}, F1 {:.3}",
            report.overall.recall, report.overall.precision, report.overall.f1
        );
        Ok(())
    } else {
        eprintln!("EVAL: FAIL");
        std::process::exit(1);
    }
}
