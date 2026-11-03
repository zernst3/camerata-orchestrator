//! A10: a finding the engine throws out is kept, marked and left out of the client export, never
//! deleted.
//!
//! Every engine path that used to drop a row on a judgment now calls [`mark`] instead and keeps
//! the row. Marked rows are split from the rows that ship ([`split_discarded`]) and stored in
//! `ScanReport::discarded`, so every exporter, which reads `ScanReport::findings`, leaves them
//! out by default, and nothing downstream (the merges, the fix stage, the evidence check, the
//! release gate) ever sees them. They are NOT held: a discarded row never blocks an export. The
//! scan ledger counts them by reason ([`crate::scan_ledger::ScanLedger::record_discarded`]) and
//! the sealed report reconciles that tally against the rows.
//!
//! The mark is a capture on the row ([`ENGINE_DISCARDED_CAPTURE`], whose value is the reason in
//! words), so a row is self-describing wherever it travels. A reviewer who finds the engine was
//! wrong clears the mark and puts the row back into normal review.
//!
//! # The paths that used to delete a finding
//!
//! 1. Scan parsing refused a row filled with placeholder text ([`PLACEHOLDER_ROW`]).
//! 2. After calibration, a model-invented row whose own text said to discard it, or whose rule id
//!    or headline is a placeholder ([`DISCARD_VERDICT`], [`PLACEHOLDER_RULE_ID`],
//!    [`PLACEHOLDER_HEADLINE`]).
//! 3. After calibration, every row under a rule judged not applicable to the codebase
//!    ([`RULE_NOT_APPLICABLE`]).
//! 4. After calibration, every row under a rule with no established convention, which one
//!    informational row now stands for ([`NO_CONVENTION`]).
//! 5. A stack exception for the row's rule that applies to the repository's stack and path
//!    ([`STACK_EXCEPTION`]).
//! 6. The seal's safety net: a row that reached the seal as one of the above (a carried row from
//!    an older run, say) is moved aside the same way.

use crate::onboard::Finding;

/// The `captures` key that marks a thrown-out row. The value is the reason, in words. A
/// bookkeeping key (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`).
pub const ENGINE_DISCARDED_CAPTURE: &str = "engine_discarded";

/// The model's own text says to discard the row.
pub const DISCARD_VERDICT: &str = "the model's own text says to discard it";
/// The row's rule id names a placeholder.
pub const PLACEHOLDER_RULE_ID: &str = "its rule id names a placeholder";
/// The row's headline is a placeholder.
pub const PLACEHOLDER_HEADLINE: &str = "its headline is a placeholder";
/// The model's entry named a place and a rule but carried no text at all (no title, no code, no
/// detail).
pub const EMPTY_ENTRY: &str = "empty_entry";
/// The row is placeholder text the model wrote in place of a finding. The scan parser appends why.
pub const PLACEHOLDER_ROW: &str = "the model wrote placeholder text in place of a finding";
/// The row's rule was judged not applicable to this codebase.
pub const RULE_NOT_APPLICABLE: &str = "its rule was judged not applicable to this codebase";
/// The row's rule has no established convention here; one informational row stands for it.
pub const NO_CONVENTION: &str =
    "its rule has no established convention here, so one informational row stands for it";
/// A stack exception for the row's rule applies to this repository's stack and path.
pub const STACK_EXCEPTION: &str =
    "a stack exception for its rule applies to this repository's stack and path";

/// Mark `f` as thrown out by the engine, for `reason`.
pub fn mark(f: &mut Finding, reason: &str) {
    f.captures
        .insert(ENGINE_DISCARDED_CAPTURE.to_string(), reason.to_string());
}

/// The reason `f` was thrown out, or `None` for a row that ships.
pub fn reason_of(f: &Finding) -> Option<&str> {
    f.captures.get(ENGINE_DISCARDED_CAPTURE).map(String::as_str)
}

/// Whether the engine threw `f` out.
pub fn is_discarded(f: &Finding) -> bool {
    f.captures.contains_key(ENGINE_DISCARDED_CAPTURE)
}

/// Clear the mark: the row goes back into normal review. Returns the reason it had.
///
/// Clearing the mark is not enough on its own: the seal re-reads a row's text and would throw it
/// out again. A restore is therefore also a decision in the store
/// ([`crate::decisions::DecisionState::EngineRestored`], A12 item 2), and every throw-out step
/// skips a row whose key carries one ([`RestoredKeys`]).
pub fn restore(f: &mut Finding) -> Option<String> {
    f.captures.remove(ENGINE_DISCARDED_CAPTURE)
}

/// The `captures` key a row carries once the reviewer has put it back after the engine threw it
/// out (the value is why it was thrown out). The row is held until the reviewer decides it. A
/// bookkeeping key (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`).
pub const ENGINE_RESTORED_CAPTURE: &str = "engine_restored";

/// The site keys of the rows the reviewer restored after the engine threw them out, with the
/// ordinal left off. Every throw-out step asks [`Self::covers`] before it marks or moves a row.
///
/// The ordinal is left off because a throw-out step can run before the rows have their final
/// keys (a fresh scan keys its rows at the seal): a row is recognised by its repo, file and the
/// fingerprint of its rule and snippet. If a file holds the same code twice, restoring one
/// protects both, which only ever keeps a row, never loses one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredKeys(std::collections::HashSet<String>);

impl RestoredKeys {
    /// The restored keys of `keys` (full site keys, ordinal included).
    pub fn from_keys<'a>(keys: impl IntoIterator<Item = &'a str>) -> Self {
        Self(keys.into_iter().map(without_ordinal).collect())
    }

    /// Whether the reviewer restored this row.
    pub fn covers(&self, f: &Finding) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let key = if f.site_key.is_empty() {
            camerata_api_types::site_key::site_key(&f.repo, &f.path, &f.rule_id, &f.snippet, 0)
        } else {
            f.site_key.clone()
        };
        self.0.contains(&without_ordinal(&key))
    }

    /// Nothing is restored.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A site key without its trailing ordinal (`{repo}\0{path}\0{fingerprint}\0{ordinal}`).
fn without_ordinal(key: &str) -> String {
    key.rsplit_once('\0')
        .map_or(key, |(head, _)| head)
        .to_string()
}

/// Put the rows of `thrown` that the reviewer restored back among the kept rows: the mark is
/// cleared and the row carries [`ENGINE_RESTORED_CAPTURE`] (the reason it had). Returns the rows
/// that stay thrown out and the rows put back, each in its original order. No row is lost.
pub fn reinstate(thrown: Vec<Finding>, restored: &RestoredKeys) -> (Vec<Finding>, Vec<Finding>) {
    let mut stay = Vec::new();
    let mut back = Vec::new();
    for mut f in thrown {
        if restored.covers(&f) {
            let reason = restore(&mut f).unwrap_or_default();
            f.captures
                .insert(ENGINE_RESTORED_CAPTURE.to_string(), reason);
            back.push(f);
        } else {
            stay.push(f);
        }
    }
    (stay, back)
}

/// Split `rows` into `(thrown_out, kept)`, each in its original order. No row is lost: the two
/// halves together are exactly `rows`.
pub fn split_discarded(rows: Vec<Finding>) -> (Vec<Finding>, Vec<Finding>) {
    rows.into_iter().partition(is_discarded)
}

/// How many of `rows` are thrown out, by reason.
pub fn tally(rows: &[Finding]) -> std::collections::BTreeMap<String, usize> {
    let mut out = std::collections::BTreeMap::new();
    for reason in rows.iter().filter_map(reason_of) {
        *out.entry(reason.to_string()).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rule: &str) -> Finding {
        Finding {
            rule_id: rule.to_string(),
            ..Finding::default()
        }
    }

    #[test]
    fn a_split_loses_no_row_and_keeps_the_order() {
        let mut a = row("A");
        mark(&mut a, DISCARD_VERDICT);
        let b = row("B");
        let mut c = row("C");
        mark(&mut c, RULE_NOT_APPLICABLE);
        let (thrown, kept) = split_discarded(vec![a, b, c]);
        assert_eq!(
            thrown
                .iter()
                .map(|f| f.rule_id.as_str())
                .collect::<Vec<_>>(),
            ["A", "C"]
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(tally(&thrown).values().sum::<usize>(), 2);
    }

    #[test]
    fn clearing_the_mark_puts_the_row_back() {
        let mut a = row("A");
        mark(&mut a, STACK_EXCEPTION);
        assert!(is_discarded(&a));
        assert_eq!(restore(&mut a).as_deref(), Some(STACK_EXCEPTION));
        assert!(!is_discarded(&a));
    }

    fn at(repo: &str, rule: &str, snippet: &str) -> Finding {
        Finding {
            repo: repo.to_string(),
            path: "src/a.rs".to_string(),
            line: 3,
            rule_id: rule.to_string(),
            snippet: snippet.to_string(),
            detail: format!("{snippet} \u{2014} explained"),
            ..Finding::default()
        }
    }

    /// A rescan replaces the old rows of a rule only in the repos whose audit returned Ok. A
    /// repo whose rescan failed keeps its old rows for the rule (the failure stays disclosed).
    #[test]
    fn a_rescan_that_failed_in_one_repo_keeps_that_repos_old_rows() {
        let mut report = crate::onboard::ScanReport::gated(&[]);
        report.findings = vec![
            at("me/ok", "MULTI-RULE-1", "old ok"),
            at("me/failed", "MULTI-RULE-1", "old failed"),
            at("me/ok", "OTHER-RULE-1", "other rule"),
        ];
        let mut thrown = at("me/ok", "MULTI-RULE-1", "old discarded ok");
        mark(&mut thrown, NO_CONVENTION);
        let mut thrown_failed = at("me/failed", "MULTI-RULE-1", "old discarded failed");
        mark(&mut thrown_failed, NO_CONVENTION);
        report.discarded = vec![thrown, thrown_failed];
        report.ledger.set_discarded(&report.discarded);

        let refreshed = vec![("me/ok".to_string(), vec!["MULTI-RULE-1".to_string()])];
        report.fold_rescan_rows(
            &refreshed,
            vec![at("me/ok", "MULTI-RULE-1", "new ok")],
            Vec::new(),
        );
        let snippets: Vec<&str> = report.findings.iter().map(|f| f.snippet.as_str()).collect();
        assert!(snippets.contains(&"old failed"), "{snippets:?}");
        assert!(snippets.contains(&"new ok"), "{snippets:?}");
        assert!(snippets.contains(&"other rule"), "{snippets:?}");
        assert!(!snippets.contains(&"old ok"), "{snippets:?}");
        let thrown: Vec<&str> = report
            .discarded
            .iter()
            .map(|f| f.snippet.as_str())
            .collect();
        assert_eq!(thrown, ["old discarded failed"]);
        assert!(report.discarded_reconciles());
    }

    /// A model entry with a place and a rule but no text is KEPT, marked `empty_entry`, and the
    /// ledger reconciles once the report is sealed. Adopted rules included.
    #[test]
    fn an_entry_with_no_text_is_kept_marked_and_reconciles() {
        let raw = r#"{"findings":[
            {"path":"a.rs","line":1,"rule":"made-up-rule"},
            {"path":"b.rs","line":2,"rule":"SEC-ADOPTED-1","title":"","code":"","detail":""},
            {"path":"c.rs","line":3,"rule":"made-up-rule","title":"Real","code":"x.y()","detail":"Real text."}
        ],"proposed_rules":[]}"#;
        let adopted: std::collections::HashSet<String> =
            ["SEC-ADOPTED-1".to_string()].into_iter().collect();
        let (rows, _) = crate::ai_audit::parse_ai_findings("me/api", raw, &adopted);
        assert_eq!(rows.len(), 3, "no entry is dropped: {rows:?}");
        let (thrown, kept) = split_discarded(rows.clone());
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(thrown.len(), 2, "{thrown:?}");
        assert!(thrown.iter().all(|f| reason_of(f) == Some(EMPTY_ENTRY)));

        let mut report = crate::onboard::ScanReport::gated(&[]);
        report.findings = rows;
        let sealed = report.seal();
        assert_eq!(sealed.discarded.len(), 2);
        assert!(
            sealed.discarded_reconciles(),
            "{:?}",
            sealed.ledger.discarded()
        );
    }

    /// After a rescan fold, every shipped and thrown-out row carries a site key and the tree names
    /// exactly the keys of the shipped rows and the thrown-out rows (no key of a replaced row,
    /// none missing; A12: every row of the report is in exactly one node).
    #[test]
    fn a_rescan_fold_leaves_every_row_keyed_and_the_tree_in_step() {
        let mut report = crate::onboard::ScanReport::gated(&[]);
        report.findings = vec![
            at("me/ok", "MULTI-RULE-1", "old ok"),
            at("me/ok", "OTHER-RULE-1", "other rule"),
        ];
        report.assign_site_keys();
        let mut thrown = at("me/ok", "MULTI-RULE-1", "new discarded");
        mark(&mut thrown, NO_CONVENTION);
        let refreshed = vec![("me/ok".to_string(), vec!["MULTI-RULE-1".to_string()])];
        report.fold_rescan_rows(
            &refreshed,
            vec![at("me/ok", "MULTI-RULE-1", "new ok")],
            vec![thrown],
        );
        assert!(report.findings.iter().all(|f| !f.site_key.is_empty()));
        assert_eq!(report.discarded.len(), 1);
        assert!(report.discarded.iter().all(|f| !f.site_key.is_empty()));
        let mut row_keys: Vec<String> = report
            .findings
            .iter()
            .chain(report.discarded.iter())
            .map(|f| f.site_key.clone())
            .collect();
        row_keys.sort();
        let mut member_keys: Vec<String> = report
            .tree
            .as_ref()
            .expect("a tree")
            .nodes
            .iter()
            .flat_map(|n| n.members.clone())
            .collect();
        member_keys.sort();
        assert_eq!(member_keys, row_keys);
    }
}
