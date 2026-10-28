//! Tests written against the mutants `cargo mutants -p camerata-server --file
//! crates/server/src/integrity_gate.rs` left alive on 2026-10-28 (docs/MUTATION_TESTING.md).

use super::*;
use crate::scan_ledger::PathHygiene;

fn ledger_with_hygiene(hygiene: PathHygiene) -> ScanLedger {
    let mut ledger = ScanLedger::new();
    ledger.record_path_hygiene(hygiene);
    ledger
}

fn path_hygiene_detail(hygiene: PathHygiene) -> String {
    evaluate(
        &ledger_with_hygiene(hygiene),
        &IntegrityGateOptions::default(),
    )
    .failures
    .into_iter()
    .find(|f| f.code == "path_hygiene")
    .map(|f| f.detail)
    .expect("a path_hygiene failure")
}

#[test]
fn the_path_hygiene_failure_names_only_the_offending_kinds_present() {
    let detail = path_hygiene_detail(PathHygiene {
        rows_total: 10,
        absolute: 2,
        backslash: 0,
        prose: 1,
        worst_example: Some("/Users/auditor/app/src/a.rs".to_string()),
        offending_rows: 3,
    });
    assert!(
        detail.starts_with(
            "3 of 10 sealed rows do not carry a repository-relative file path (2 absolute, 1 \
             carrying prose rather than a path); worst: `/Users/auditor/app/src/a.rs`."
        ),
        "{detail}"
    );
    let detail = path_hygiene_detail(PathHygiene {
        rows_total: 4,
        absolute: 0,
        backslash: 4,
        prose: 0,
        worst_example: None,
        offending_rows: 4,
    });
    assert!(
        detail.starts_with(
            "4 of 4 sealed rows do not carry a repository-relative file path (4 with backslash \
             separators). A client"
        ),
        "{detail}"
    );
}
