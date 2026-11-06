use std::collections::BTreeSet;

use super::*;

fn finding(rule_id: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        ..Finding::default()
    }
}

fn keyed(mut rows: Vec<Finding>) -> Vec<Finding> {
    backfill_site_keys(&mut rows);
    rows
}

fn key_of(rows: &[Finding], line: usize, snippet: &str) -> String {
    rows.iter()
        .find(|f| f.line == line && f.snippet == snippet)
        .unwrap_or_else(|| panic!("no row at {line}"))
        .site_key
        .clone()
}

#[test]
fn match_runs_reports_exact_moved_and_gone_without_losing_a_key() {
    let prev = keyed(vec![
        // Stays put except for a line shift: exact.
        finding("SEC-1", "src/a.rs", 10, "let a = secret();"),
        // The offending code was edited a line away: moved.
        finding("SEC-2", "src/b.rs", 40, "run(user_input)"),
        // Removed in the new run: gone.
        finding("SEC-3", "src/c.rs", 5, "eval(x)"),
    ]);
    let next = keyed(vec![
        finding("SEC-1", "src/a.rs", 22, "let a = secret();"),
        finding("SEC-2", "src/b.rs", 41, "run(sanitize_badly(user_input))"),
        // Brand new site.
        finding("SEC-4", "src/d.rs", 1, "danger()"),
    ]);

    let m = match_runs(&prev, &next);

    assert_eq!(
        m.exact,
        vec![KeyLink {
            old_key: key_of(&prev, 10, "let a = secret();"),
            new_key: key_of(&next, 22, "let a = secret();"),
            how: LinkHow::Exact,
        }]
    );
    assert_eq!(m.exact[0].old_key, m.exact[0].new_key);
    assert_eq!(
        m.moved,
        vec![KeyLink {
            old_key: key_of(&prev, 40, "run(user_input)"),
            new_key: key_of(&next, 41, "run(sanitize_badly(user_input))"),
            how: LinkHow::Moved,
        }]
    );
    assert_eq!(m.gone, vec![key_of(&prev, 5, "eval(x)")]);
    assert_eq!(m.added, vec![key_of(&next, 1, "danger()")]);

    // No key lost: every old key is exact, moved or gone, every new key exact, moved or added.
    let old_seen: BTreeSet<String> = m
        .exact
        .iter()
        .chain(&m.moved)
        .map(|l| l.old_key.clone())
        .chain(m.gone.iter().cloned())
        .collect();
    let all_old: BTreeSet<String> = prev.iter().map(|f| f.site_key.clone()).collect();
    assert_eq!(old_seen, all_old);
    let new_seen: BTreeSet<String> = m
        .exact
        .iter()
        .chain(&m.moved)
        .map(|l| l.new_key.clone())
        .chain(m.added.iter().cloned())
        .collect();
    let all_new: BTreeSet<String> = next.iter().map(|f| f.site_key.clone()).collect();
    assert_eq!(new_seen, all_new);
}

#[test]
fn a_neighbour_on_another_rule_or_beyond_the_window_is_not_a_move() {
    let prev = keyed(vec![finding("SEC-1", "src/a.rs", 10, "old()")]);
    let far = crate::ai_audit::SEMANTIC_MERGE_WINDOW + 1;
    let next = keyed(vec![
        finding("SEC-1", "src/a.rs", 10 + far, "new()"),
        finding("SEC-9", "src/a.rs", 10, "other()"),
    ]);
    let m = match_runs(&prev, &next);
    assert!(m.exact.is_empty() && m.moved.is_empty());
    assert_eq!(m.gone.len(), 1);
    assert_eq!(m.added.len(), 2);
}

#[test]
fn deleting_the_first_of_two_identical_lines_stales_the_survivor_but_loses_no_key() {
    let prev = keyed(vec![
        finding("SEC-1", "src/a.rs", 10, "x()"),
        finding("SEC-1", "src/a.rs", 20, "x()"),
    ]);
    let next = keyed(vec![finding("SEC-1", "src/a.rs", 19, "x()")]);
    let m = match_runs(&prev, &next);
    // The survivor is now ordinal 0, so it exactly matches the old ordinal 0 (the deleted
    // line's key); the old ordinal 1 key is gone and its decision is kept for history.
    assert_eq!(m.exact.len(), 1);
    assert_eq!(m.exact[0].new_key, prev[0].site_key);
    assert_eq!(m.gone, vec![prev[1].site_key.clone()]);
    assert!(m.moved.is_empty() && m.added.is_empty());
}

#[test]
fn match_runs_does_not_depend_on_row_order_or_on_keys_being_assigned_already() {
    let prev = vec![
        finding("SEC-1", "src/a.rs", 30, "x()"),
        finding("SEC-1", "src/a.rs", 4, "x()"),
        finding("SEC-2", "src/b.rs", 8, "y()"),
    ];
    let mut shuffled = prev.clone();
    shuffled.reverse();
    let next = prev.clone();
    assert_eq!(match_runs(&prev, &next), match_runs(&shuffled, &next));
    let m = match_runs(&prev, &next);
    assert_eq!(m.exact.len(), 3);
    assert!(m.moved.is_empty() && m.gone.is_empty() && m.added.is_empty());
}

#[test]
fn key_links_json_holds_the_moved_links_and_is_written_atomically() {
    let prev = keyed(vec![finding("SEC-2", "src/b.rs", 40, "run(a)")]);
    let next = keyed(vec![finding("SEC-2", "src/b.rs", 41, "run(b)")]);
    let m = match_runs(&prev, &next);
    let dir = tempfile::tempdir().unwrap();
    let path = m.write_key_links(dir.path()).unwrap();
    assert_eq!(path.file_name().unwrap(), KEY_LINKS_FILE);
    let back: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0]["how"], "moved");
    assert_eq!(back[0]["old_key"], prev[0].site_key);
    assert_eq!(back[0]["new_key"], next[0].site_key);
    assert!(!dir.path().join("key_links.json.tmp").exists());
}
