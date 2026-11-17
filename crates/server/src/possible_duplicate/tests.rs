//! Which rows the possible duplicate hint pairs, and what it leaves alone. Synthetic data only.

use camerata_api_types::audit::FindingKind;

use super::*;

/// A problem row in `sub` (a sub-group id) of `path` at `line`; the Fix is read from `sub` by
/// [`fix_of`].
fn row(path: &str, line: usize, sub: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-A-1".to_string(),
        severity: "high".to_string(),
        site_key: format!("{path}:{line}:{sub}"),
        group_path: vec![
            "cat:a".to_string(),
            "rule:SEC-A-1".to_string(),
            sub.to_string(),
        ],
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

/// The Fix of a row in these tests: the sub-group's own id, one Fix for each sub-group.
fn fix_of(f: &Finding) -> String {
    f.group_path.get(2).cloned().unwrap_or_default()
}

fn index(rows: &[Finding]) -> DupIndex {
    DupIndex::with_fix_ids(rows, fix_of, |_| None)
}

#[test]
fn two_causes_two_lines_apart_in_one_file_pair() {
    let rows = [row("a.rs", 10, "sub:x/1"), row("a.rs", 12, "sub:y/1")];
    let idx = index(&rows);
    assert_eq!(idx.fix_ids_of(&rows[0]), vec!["sub:y/1"]);
    assert_eq!(idx.fix_ids_of(&rows[1]), vec!["sub:x/1"]);
    let link = &idx.of(&rows[0])[0];
    assert_eq!(link.key, rows[1].site_key);
    assert_eq!(link.group_id, "sub:y/1");
    assert_eq!(link.distance, 2);
}

#[test]
fn three_lines_apart_pairs_and_four_does_not() {
    let near = [row("a.rs", 10, "sub:x/1"), row("a.rs", 13, "sub:y/1")];
    assert_eq!(index(&near).of(&near[0]).len(), 1);
    let far = [row("a.rs", 10, "sub:x/1"), row("a.rs", 14, "sub:y/1")];
    assert!(index(&far).of(&far[0]).is_empty());
    let farther = [row("a.rs", 10, "sub:x/1"), row("a.rs", 20, "sub:y/1")];
    assert!(index(&farther).of(&farther[0]).is_empty());
}

#[test]
fn the_same_place_pairs_too() {
    let rows = [row("a.rs", 10, "sub:x/1"), row("a.rs", 10, "sub:y/1")];
    assert_eq!(index(&rows).fix_ids_of(&rows[0]), vec!["sub:y/1"]);
}

#[test]
fn one_cause_at_two_nearby_places_does_not_pair_with_itself() {
    let rows = [row("a.rs", 10, "sub:x/1"), row("a.rs", 11, "sub:x/1")];
    let idx = index(&rows);
    assert!(idx.of(&rows[0]).is_empty());
    assert!(idx.of(&rows[1]).is_empty());
}

#[test]
fn two_files_never_pair() {
    let rows = [row("a.rs", 10, "sub:x/1"), row("b.rs", 10, "sub:y/1")];
    assert!(index(&rows).of(&rows[0]).is_empty());
    let mut other_repo = row("a.rs", 10, "sub:y/1");
    other_repo.repo = "acme/other".to_string();
    let rows = [row("a.rs", 10, "sub:x/1"), other_repo];
    assert!(index(&rows).of(&rows[0]).is_empty());
}

#[test]
fn a_residual_sub_group_counts_each_place_as_its_own_cause() {
    // Two places of one rule's residual sub-group share a Fix, yet are two causes.
    let rows = [
        row("a.rs", 10, "sub:SEC-A-1/0"),
        row("a.rs", 12, "sub:SEC-A-1/0"),
    ];
    let idx = index(&rows);
    assert_eq!(idx.of(&rows[0]).len(), 1);
    assert_eq!(idx.of(&rows[1]).len(), 1);
    // The other place shares the row's own Fix, so the export names the place with the Fix id.
    assert_eq!(idx.fix_ids_of(&rows[0]), vec!["sub:SEC-A-1/0 line 12"]);
    assert_eq!(idx.fix_ids_of(&rows[1]), vec!["sub:SEC-A-1/0 line 10"]);
    // A different Fix is named by its id alone.
    let mixed = [row("a.rs", 10, "sub:SEC-A-1/0"), row("a.rs", 12, "sub:y/1")];
    assert_eq!(index(&mixed).fix_ids_of(&mixed[0]), vec!["sub:y/1"]);
    // The same place twice in a residual sub-group is one cause.
    let mut again = row("a.rs", 10, "sub:SEC-A-1/0");
    again.site_key = "second".to_string();
    let rows = [row("a.rs", 10, "sub:SEC-A-1/0"), again];
    assert!(index(&rows).of(&rows[0]).is_empty());
}

#[test]
fn a_row_pairs_with_more_than_one_cause_each_listed_once_nearest_first() {
    let rows = [
        row("a.rs", 10, "sub:x/1"),
        row("a.rs", 12, "sub:y/1"),
        row("a.rs", 13, "sub:y/1"),
        row("a.rs", 11, "sub:z/1"),
    ];
    let idx = index(&rows);
    assert_eq!(idx.fix_ids_of(&rows[0]), vec!["sub:z/1", "sub:y/1"]);
    // The link to a cause is its nearest row.
    assert_eq!(idx.of(&rows[0])[1].key, rows[1].site_key);
}

#[test]
fn observations_whole_file_rows_and_rows_in_no_fix_never_pair() {
    let mut observation = row("a.rs", 11, "sub:y/1");
    observation.kind = Some(FindingKind::ConventionObserved(
        camerata_api_types::audit::ConventionObservedKind {
            convention_option_id: "x".to_string(),
            convention_label: "x".to_string(),
        },
    ));
    let whole_file = row("a.rs", 0, "sub:z/1");
    let no_fix = row("a.rs", 12, "");
    let rows = [row("a.rs", 10, "sub:x/1"), observation, whole_file, no_fix];
    assert!(index(&rows).of(&rows[0]).is_empty());
}

#[test]
fn working_out_the_hint_changes_nothing_on_the_rows() {
    let rows = [row("a.rs", 10, "sub:x/1"), row("a.rs", 12, "sub:y/1")];
    let before = rows.clone();
    let _ = index(&rows);
    assert_eq!(rows, before);
}

#[test]
fn a_row_the_engine_threw_out_never_pairs() {
    let mut thrown = row("a.rs", 12, "sub:y/1");
    crate::engine_discard::mark(&mut thrown, "its headline is a placeholder");
    let rows = [row("a.rs", 10, "sub:x/1"), thrown];
    assert!(index(&rows).of(&rows[0]).is_empty());
    assert!(index(&rows).of(&rows[1]).is_empty());
}

#[test]
fn a_row_already_merged_into_another_never_pairs() {
    let mut merged = row("a.rs", 12, "sub:y/1");
    merged.captures.insert(
        crate::report_export::overlay::MERGED_INTO_CAPTURE.to_string(),
        "some-other-key".to_string(),
    );
    let rows = [row("a.rs", 10, "sub:x/1"), merged];
    assert!(index(&rows).of(&rows[0]).is_empty());
    assert!(index(&rows).of(&rows[1]).is_empty());
}

#[test]
fn a_rule_carried_at_the_same_place_under_another_fix_never_pairs() {
    // The carrier and the row it carries are one place: the same line, two rules, two Fixes.
    let rows = [row("a.rs", 10, "sub:x/1"), row("a.rs", 10, "sub:y/1")];
    let carried = DupIndex::with_fix_ids(&rows, fix_of, |_| Some("place-1".to_string()));
    assert!(carried.of(&rows[0]).is_empty());
    assert!(carried.of(&rows[1]).is_empty());
    // A third cause two lines away is still a pair with each of them.
    let rows = [
        row("a.rs", 10, "sub:x/1"),
        row("a.rs", 10, "sub:y/1"),
        row("a.rs", 12, "sub:z/1"),
    ];
    let idx = DupIndex::with_fix_ids(&rows, fix_of, |f| {
        (f.line == 10).then(|| "place-1".to_string())
    });
    assert_eq!(idx.fix_ids_of(&rows[0]), vec!["sub:z/1"]);
    assert_eq!(idx.fix_ids_of(&rows[2]), vec!["sub:x/1", "sub:y/1"]);
}
