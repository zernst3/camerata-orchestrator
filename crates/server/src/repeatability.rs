//! H5 item 2: compare two runs of the same scan. `camerata repeatability` runs the full AI
//! pass twice with every answer cache bypassed and reports the difference through
//! [`diff_runs`]; the three-pass consensus option (`crate::consensus`) matches findings across
//! passes with the same [`finding_match_key`], so "the same finding" means one thing in both.
//!
//! Pure: no I/O, no model, deterministic output order (every list is sorted), so the text and
//! JSON a caller renders from a [`RunDiff`] read the same for the same two inputs.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::onboard::Finding;

/// The identity two runs agree on: repo, path and the content fingerprint of the finding's
/// rule id and snippet ([`crate::suppression::fingerprint`], the one baselines use). The line
/// is left out on purpose: the fingerprint survives a line moving, and a model that quotes the
/// same code at a neighbouring line has found the same thing.
pub fn finding_match_key(f: &Finding) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        f.repo,
        f.path,
        crate::suppression::fingerprint(&f.rule_id, &f.snippet)
    )
}

/// Which engine produced `f`: `"preview"` (a scan-time tool pass), `"ai"` (the model tier,
/// [`crate::report_export::is_ai_tier`]) or `"deterministic"` (the floor).
pub fn finding_tier(f: &Finding) -> &'static str {
    if f.preview_tool.is_some() {
        "preview"
    } else if crate::report_export::is_ai_tier(f) {
        "ai"
    } else {
        "deterministic"
    }
}

fn unmatched(f: &Finding) -> UnmatchedFinding {
    UnmatchedFinding {
        fingerprint: crate::suppression::fingerprint(&f.rule_id, &f.snippet),
        tier: finding_tier(f).to_string(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        rule_id: f.rule_id.clone(),
        severity: f.severity.to_ascii_lowercase(),
    }
}

/// Count `first` and `second` by `key`, one row per key seen in either run, sorted by key.
fn count_rows(first: &[Finding], second: &[Finding], key: fn(&Finding) -> String) -> Vec<CountRow> {
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for f in first {
        counts.entry(key(f)).or_default().0 += 1;
    }
    for f in second {
        counts.entry(key(f)).or_default().1 += 1;
    }
    counts
        .into_iter()
        .map(|(key, (first, second))| CountRow { key, first, second })
        .collect()
}

/// One count, in both runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CountRow {
    pub key: String,
    pub first: usize,
    pub second: usize,
}

impl CountRow {
    /// `second - first`.
    pub fn delta(&self) -> i64 {
        self.second as i64 - self.first as i64
    }
}

/// A finding reported by one run and not the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnmatchedFinding {
    pub fingerprint: String,
    pub tier: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
}

/// The difference between two runs' findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunDiff {
    pub first_total: usize,
    pub second_total: usize,
    /// Findings present in both runs (matched by [`finding_match_key`], counting repeats).
    pub matched: usize,
    pub by_tier: Vec<CountRow>,
    pub by_rule: Vec<CountRow>,
    pub by_severity: Vec<CountRow>,
    pub only_in_first: Vec<UnmatchedFinding>,
    pub only_in_second: Vec<UnmatchedFinding>,
}

impl RunDiff {
    /// True when every finding of each run matched one in the other.
    pub fn identical(&self) -> bool {
        self.only_in_first.is_empty() && self.only_in_second.is_empty()
    }
}

/// Compare two runs' findings. See the module docs.
pub fn diff_runs(first: &[Finding], second: &[Finding]) -> RunDiff {
    // Group each run by match key, keeping run order inside a key so the unmatched copies
    // reported are always the later ones.
    let mut groups: BTreeMap<String, (Vec<&Finding>, Vec<&Finding>)> = BTreeMap::new();
    for f in first {
        groups.entry(finding_match_key(f)).or_default().0.push(f);
    }
    for f in second {
        groups.entry(finding_match_key(f)).or_default().1.push(f);
    }
    let mut matched = 0;
    let mut only_in_first = Vec::new();
    let mut only_in_second = Vec::new();
    for (a, b) in groups.values() {
        let pairs = a.len().min(b.len());
        matched += pairs;
        only_in_first.extend(a[pairs..].iter().map(|f| unmatched(f)));
        only_in_second.extend(b[pairs..].iter().map(|f| unmatched(f)));
    }
    let order = |x: &UnmatchedFinding, y: &UnmatchedFinding| {
        (&x.repo, &x.path, x.line, &x.rule_id).cmp(&(&y.repo, &y.path, y.line, &y.rule_id))
    };
    only_in_first.sort_by(order);
    only_in_second.sort_by(order);
    RunDiff {
        first_total: first.len(),
        second_total: second.len(),
        matched,
        by_tier: count_rows(first, second, |f| finding_tier(f).to_string()),
        by_rule: count_rows(first, second, |f| f.rule_id.clone()),
        by_severity: count_rows(first, second, |f| f.severity.to_ascii_lowercase()),
        only_in_first,
        only_in_second,
    }
}

#[cfg(test)]
mod tests;
