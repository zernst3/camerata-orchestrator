//! The comparison of two runs for the operator: what one run found that the other did not.
//! Build 4 fills in the bodies.

use crate::onboard::Finding;

/// One problem that appears in only one of two runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffEntry {
    pub rule_id: String,
    pub path: String,
    pub line: usize,
    pub severity: String,
    pub title: String,
}

/// The difference between run A and run B.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunDiff {
    /// Problems present in A and absent from B.
    pub only_in_a: Vec<DiffEntry>,
    /// Problems present in B and absent from A.
    pub only_in_b: Vec<DiffEntry>,
    /// How many problems both runs hold.
    pub in_both: usize,
}

/// Compare the problems of two runs.
#[allow(dead_code)]
pub fn diff_runs(a: &[Finding], b: &[Finding]) -> RunDiff {
    let _ = (a, b);
    unimplemented!("build 4")
}

/// The note shown to the operator for a difference between two runs (never silent).
#[allow(dead_code)]
pub fn operator_note(diff: &RunDiff) -> String {
    let _ = diff;
    unimplemented!("build 4")
}
