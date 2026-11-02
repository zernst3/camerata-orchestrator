//! The stable site key: the name of one finding row (one place in one file) that survives a
//! rerun, defined once for every crate that needs it.
//!
//! A decision or an edit a reviewer makes on a site must still find that site after the scan is
//! run again, so the key cannot contain the line number: an edit above the site moves the line
//! and would orphan the decision. It is the repo, the path and the [`fingerprint`] of the rule
//! id plus the whitespace-collapsed snippet (the identity the suppression baseline and
//! `repeatability` already use), plus an ordinal that tells apart the rare case of the same
//! fingerprint appearing more than once in one file in one run (a repeated line, or dependency
//! advisories whose snippet is `package@version`).
//!
//! The ordinal is assigned in line order, then advisory id, then the order the rows arrived in,
//! so the same set of rows always gets the same ordinals no matter which order they are given
//! in (see [`assign_ordinals`]). The server's `Finding`, the review UI's `FindingView` and the
//! export code all build their key through this module, so the three can never disagree.

use std::collections::BTreeMap;

use crate::fingerprint::fingerprint;

/// The key of one site. `ordinal` is 0 unless the same (repo, path, fingerprint) occurs more
/// than once in a run; see [`assign_ordinals`].
pub fn site_key(repo: &str, path: &str, rule_id: &str, snippet: &str, ordinal: usize) -> String {
    format!(
        "{repo}\u{0}{path}\u{0}{}\u{0}{ordinal}",
        fingerprint(rule_id, snippet)
    )
}

/// What the ordinal assignment needs to know about one row.
#[derive(Debug, Clone, Copy)]
pub struct SiteKeyRow<'a> {
    pub repo: &'a str,
    pub path: &'a str,
    pub rule_id: &'a str,
    pub snippet: &'a str,
    /// The row's line. Used only to order rows that share a fingerprint; never part of the key.
    pub line: usize,
    /// The advisory id of a dependency row (empty for every other row). Orders two advisories
    /// on one package version in one lockfile.
    pub advisory: &'a str,
}

/// The ordinal of each row, in the order the rows were given. Rows that share (repo, path,
/// fingerprint) are numbered 0, 1, 2 ... by line, then advisory id; rows that tie on both keep
/// the order they arrived in. A row that is alone in its group gets 0.
pub fn assign_ordinals(rows: &[SiteKeyRow<'_>]) -> Vec<usize> {
    let mut groups: BTreeMap<(&str, &str, String), Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        groups
            .entry((r.repo, r.path, fingerprint(r.rule_id, r.snippet)))
            .or_default()
            .push(i);
    }
    let mut ordinals = vec![0; rows.len()];
    for members in groups.values_mut() {
        // `sort_by_key` is stable, so rows tied on line and advisory keep their arrival order.
        members.sort_by_key(|&i| (rows[i].line, rows[i].advisory));
        for (ordinal, &i) in members.iter().enumerate() {
            ordinals[i] = ordinal;
        }
    }
    ordinals
}

/// The key of each row, in the order the rows were given.
pub fn site_keys(rows: &[SiteKeyRow<'_>]) -> Vec<String> {
    assign_ordinals(rows)
        .into_iter()
        .zip(rows)
        .map(|(ordinal, r)| site_key(r.repo, r.path, r.rule_id, r.snippet, ordinal))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(snippet: &'a str, line: usize, advisory: &'a str) -> SiteKeyRow<'a> {
        SiteKeyRow {
            repo: "acme/app",
            path: "src/a.rs",
            rule_id: "SEC-1",
            snippet,
            line,
            advisory,
        }
    }

    #[test]
    fn site_key_is_unchanged_when_the_line_moves() {
        let before = site_keys(&[row("let t = 1;", 10, "")]);
        let after = site_keys(&[row("let t = 1;", 57, "")]);
        assert_eq!(before, after);
        // Reformatting the snippet (whitespace) is not a change to the site either.
        assert_eq!(before, site_keys(&[row("  let   t = 1;\n", 10, "")]));
    }

    #[test]
    fn site_key_changes_when_the_snippet_changes() {
        let a = site_key("acme/app", "src/a.rs", "SEC-1", "let t = 1;", 0);
        assert_ne!(
            a,
            site_key("acme/app", "src/a.rs", "SEC-1", "let t = 2;", 0)
        );
        assert_ne!(
            a,
            site_key("acme/app", "src/a.rs", "SEC-2", "let t = 1;", 0)
        );
        assert_ne!(
            a,
            site_key("acme/app", "src/b.rs", "SEC-1", "let t = 1;", 0)
        );
        assert_ne!(
            a,
            site_key("acme/other", "src/a.rs", "SEC-1", "let t = 1;", 0)
        );
        assert_ne!(
            a,
            site_key("acme/app", "src/a.rs", "SEC-1", "let t = 1;", 1)
        );
    }

    #[test]
    fn site_key_has_the_documented_shape() {
        assert_eq!(
            site_key("acme/app", "src/a.rs", "SEC-X", "let t = \"abc\";", 2),
            "acme/app\u{0}src/a.rs\u{0}61727d97b8fb274a\u{0}2"
        );
    }

    #[test]
    fn site_key_ordinal_is_assigned_in_line_order_and_is_stable() {
        // The same text on lines 30, 4 and 12, given in a scrambled order.
        let rows = [
            row("x()", 30, ""),
            row("x()", 4, ""),
            row("x()", 12, ""),
            row("y()", 99, ""),
        ];
        assert_eq!(assign_ordinals(&rows), vec![2, 0, 1, 0]);
        // Any other arrival order gives each row the same key.
        let keys = site_keys(&rows);
        let reversed: Vec<_> = rows.iter().rev().copied().collect();
        let mut reversed_keys = site_keys(&reversed);
        reversed_keys.reverse();
        assert_eq!(keys, reversed_keys);
        // Advisories on one package version are told apart by advisory id after the line.
        let deps = [
            row("lodash@4.0.0", 1, "GHSA-b"),
            row("lodash@4.0.0", 1, "GHSA-a"),
        ];
        assert_eq!(assign_ordinals(&deps), vec![1, 0]);
        // Rows that tie on everything keep their arrival order.
        let ties = [row("z()", 5, ""), row("z()", 5, "")];
        assert_eq!(assign_ordinals(&ties), vec![0, 1]);
        // A run is deterministic.
        assert_eq!(site_keys(&rows), site_keys(&rows));
        // Different files or repos never share a group, so each starts at 0.
        let mut other = row("x()", 4, "");
        other.path = "src/b.rs";
        assert_eq!(assign_ordinals(&[row("x()", 4, ""), other]), vec![0, 0]);
    }
}
