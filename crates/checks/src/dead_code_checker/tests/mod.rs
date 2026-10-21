//! Tests for `ARCH-DEAD-CODE-1`, grouped by the part of the algorithm they pin down. Every
//! guard (`G1`..`G30` in the design spec) has a safe-twin test here; each positive sibling
//! proves the same fixture WITHOUT the guard's trigger is still flagged.

use super::*;
use crate::arch_checker::{ArchChecker, RepoView, SEVERITY_MEDIUM};

mod conventions;
mod core;
mod dynamic;
mod manifests;
mod rust;
mod units;

pub(super) fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
    pairs
        .into_iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect()
}

pub(super) fn run(f: &[(String, String)]) -> Vec<ArchViolation> {
    DeadCodeChecker.check(&RepoView {
        spec: "test/repo",
        files: f,
    })
}

/// The `file` of every violation, in emitted order.
pub(super) fn flagged(f: &[(String, String)]) -> Vec<String> {
    run(f).into_iter().map(|v| v.file).collect()
}

pub(super) fn assert_silent(f: &[(String, String)]) {
    let vs = run(f);
    assert!(vs.is_empty(), "expected no findings: {vs:#?}");
}

pub(super) fn assert_flags_only(f: &[(String, String)], expected: &str) {
    let vs = run(f);
    let got: Vec<&str> = vs.iter().map(|v| v.file.as_str()).collect();
    assert_eq!(got, [expected], "{vs:#?}");
}

#[test]
fn rule_id_and_severity_are_fixed() {
    let f = files(vec![
        ("src/app.ts", "export {};\n"),
        ("src/orphan.ts", "export const x = 1;\n"),
    ]);
    let vs = run(&f);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].rule_id, RULE_DEAD_CODE);
    assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
}
