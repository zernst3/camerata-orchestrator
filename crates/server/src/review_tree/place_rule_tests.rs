//! The one place rule (`places_of`): the whole-file key and the total tie break. Synthetic rows.

use super::*;

struct R {
    repo: &'static str,
    path: &'static str,
    line: usize,
    rule: &'static str,
    severity: &'static str,
    bucket: &'static str,
}

impl PlaceRow for R {
    fn place_repo(&self) -> &str {
        self.repo
    }
    fn place_path(&self) -> &str {
        self.path
    }
    fn place_line(&self) -> usize {
        self.line
    }
    fn place_rule_id(&self) -> &str {
        self.rule
    }
    fn place_severity(&self) -> &str {
        self.severity
    }
    fn place_bucket(&self) -> &str {
        self.bucket
    }
}

fn r(line: usize, rule: &'static str, severity: &'static str, bucket: &'static str) -> R {
    R {
        repo: "acme/portal",
        path: "src/a.rs",
        line,
        rule,
        severity,
        bucket,
    }
}

#[test]
fn whole_file_problems_of_three_rules_in_one_file_are_three_places() {
    let rows = [
        r(0, "SYN-A-1", "medium", "plan"),
        r(0, "SYN-B-1", "medium", "plan"),
        r(0, "SYN-C-1", "medium", "plan"),
    ];
    assert_eq!(places_of(rows.iter()).len(), 3);
}

#[test]
fn whole_file_problems_of_one_rule_in_one_file_are_one_place() {
    let rows = [
        r(0, "SYN-A-1", "medium", "plan"),
        r(0, "SYN-A-1", "high", "plan"),
    ];
    let places = places_of(rows.iter());
    assert_eq!(places.len(), 1);
    assert_eq!(places[0].members.len(), 2);
}

#[test]
fn a_whole_file_row_and_a_line_row_never_share_a_place() {
    let rows = [
        r(0, "SYN-A-1", "medium", "plan"),
        r(7, "SYN-A-1", "medium", "plan"),
    ];
    assert_eq!(places_of(rows.iter()).len(), 2);
}

#[test]
fn line_rows_of_different_rules_at_one_line_are_still_one_place() {
    let rows = [
        r(7, "SYN-A-1", "medium", "plan"),
        r(7, "SYN-B-1", "medium", "plan"),
    ];
    assert_eq!(places_of(rows.iter()).len(), 1);
}

fn head_rule(rows: &[R]) -> &'static str {
    let places = places_of(rows.iter());
    assert_eq!(places.len(), 1);
    places[0].head.rule
}

#[test]
fn the_head_never_depends_on_the_order_the_rows_arrive_in() {
    // Equal severity: the earliest fix order wins whatever the order.
    let a = || r(7, "SYN-LATER", "medium", "plan");
    let b = || r(7, "SYN-FIRST", "medium", "do_now");
    assert_eq!(head_rule(&[a(), b()]), "SYN-FIRST");
    assert_eq!(head_rule(&[b(), a()]), "SYN-FIRST");
    // Equal severity and fix order: the stable key (rule id) decides.
    let c = || r(7, "SYN-B-1", "medium", "plan");
    let d = || r(7, "SYN-A-1", "medium", "plan");
    assert_eq!(head_rule(&[c(), d()]), "SYN-A-1");
    assert_eq!(head_rule(&[d(), c()]), "SYN-A-1");
    // Severity still outranks everything.
    let e = || r(7, "SYN-Z-9", "high", "plan");
    assert_eq!(head_rule(&[d(), e()]), "SYN-Z-9");
    assert_eq!(head_rule(&[e(), d()]), "SYN-Z-9");
}
