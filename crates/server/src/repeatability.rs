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

/// Which engine produced `f`: `"preview"` (a scan-time external tool pass), `"convention"` (a
/// convention-observed, no-convention or not-assessed row: they carry no calibration
/// confidence but cite model-chosen evidence, so they are not the deterministic floor), `"ai"`
/// (the model tier, [`crate::report_export::is_ai_tier`]) or `"deterministic"` (the floor,
/// which includes the native architectural checkers: their rows carry
/// [`crate::onboard::ARCH_PREVIEW_TOOL`] as their `preview_tool` but are exact by construction).
pub fn finding_tier(f: &Finding) -> &'static str {
    use camerata_api_types::audit::FindingKind;
    // Convention-class kinds come first: a not-assessed architectural row also carries
    // `preview_tool = camerata-arch`, but it is an honesty row, never a deterministic result.
    if matches!(
        f.kind,
        Some(
            FindingKind::ConventionObserved(_)
                | FindingKind::NoConvention(_)
                | FindingKind::NotAssessed(_)
        )
    ) {
        "convention"
    } else if f.preview_tool.as_deref() == Some(crate::onboard::ARCH_PREVIEW_TOOL) {
        "deterministic"
    } else if f.preview_tool.is_some() {
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
    /// Findings the fingerprint pass left unmatched that the second pass paired by site: the
    /// same repo, path and rule id within [`crate::ai_audit::SEMANTIC_MERGE_WINDOW`] lines.
    /// The model quoting the same defect on a neighbouring line changes the snippet and so
    /// the fingerprint; these pairs are one defect. Reported separately from [`Self::matched`]
    /// and removed from the two unmatched lists.
    pub matched_by_site: usize,
    pub by_tier: Vec<CountRow>,
    pub by_rule: Vec<CountRow>,
    pub by_severity: Vec<CountRow>,
    pub only_in_first: Vec<UnmatchedFinding>,
    pub only_in_second: Vec<UnmatchedFinding>,
}

impl RunDiff {
    /// True when every finding of each run matched one in the other (by fingerprint or by
    /// site).
    pub fn identical(&self) -> bool {
        self.only_in_first.is_empty() && self.only_in_second.is_empty()
    }
}

/// How two runs' findings pair up, as indices into the two slices given to [`pair_runs`]. The
/// one pairing both [`diff_runs`] (which counts it) and `review_tree::match_runs` (which names
/// the pairs) are built on, so "the same finding" means one thing in both.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RunPairing {
    /// Pairs the fingerprint pass matched: same repo, path and [`finding_match_key`].
    pub exact: Vec<(usize, usize)>,
    /// Pairs the second pass matched by site: same repo, path and rule id, within
    /// [`crate::ai_audit::SEMANTIC_MERGE_WINDOW`] lines.
    pub by_site: Vec<(usize, usize)>,
    /// Indices into the first slice that nothing in the second matched, ordered by
    /// (repo, path, line).
    pub only_first: Vec<usize>,
    /// Indices into the second slice that nothing in the first matched, ordered by
    /// (repo, path, line).
    pub only_second: Vec<usize>,
}

/// Pair the findings of two runs. Inside one match key, rows pair in the order they appear in
/// the slices, so the unmatched copies are always the later ones; a caller that needs the pairs
/// to follow line order (the site key's ordinals do) passes slices already in line order.
pub(crate) fn pair_runs(first: &[&Finding], second: &[&Finding]) -> RunPairing {
    // Group each run by match key, keeping run order inside a key.
    let mut groups: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, f) in first.iter().enumerate() {
        groups.entry(finding_match_key(f)).or_default().0.push(i);
    }
    for (i, f) in second.iter().enumerate() {
        groups.entry(finding_match_key(f)).or_default().1.push(i);
    }
    let mut pairing = RunPairing::default();
    let mut left_first: Vec<usize> = Vec::new();
    let mut left_second: Vec<usize> = Vec::new();
    for (a, b) in groups.values() {
        let pairs = a.len().min(b.len());
        pairing
            .exact
            .extend(a.iter().copied().zip(b.iter().copied()));
        left_first.extend_from_slice(&a[pairs..]);
        left_second.extend_from_slice(&b[pairs..]);
    }
    left_first.sort_by_key(|&i| (&first[i].repo, &first[i].path, first[i].line));
    left_second.sort_by_key(|&i| (&second[i].repo, &second[i].path, second[i].line));
    // Second pass: pair leftovers by site (repo, path, rule id, nearest line within the
    // window). Each leftover pairs at most once; the closest line wins, the earlier one on a tie.
    let mut second_used = vec![false; left_second.len()];
    for &fi in &left_first {
        let f = first[fi];
        let best = left_second
            .iter()
            .enumerate()
            .filter(|(k, &si)| {
                let g = second[si];
                !second_used[*k]
                    && g.repo == f.repo
                    && g.path == f.path
                    && g.rule_id == f.rule_id
                    && g.line.abs_diff(f.line) <= crate::ai_audit::SEMANTIC_MERGE_WINDOW
            })
            .min_by_key(|(_, &si)| second[si].line.abs_diff(f.line))
            .map(|(k, _)| k);
        match best {
            Some(k) => {
                second_used[k] = true;
                pairing.by_site.push((fi, left_second[k]));
            }
            None => pairing.only_first.push(fi),
        }
    }
    pairing.only_second = left_second
        .iter()
        .zip(&second_used)
        .filter(|(_, used)| !**used)
        .map(|(&si, _)| si)
        .collect();
    pairing
}

/// Compare two runs' findings. See the module docs.
pub fn diff_runs(first: &[Finding], second: &[Finding]) -> RunDiff {
    let first_refs: Vec<&Finding> = first.iter().collect();
    let second_refs: Vec<&Finding> = second.iter().collect();
    let pairing = pair_runs(&first_refs, &second_refs);
    let mut only_in_first: Vec<UnmatchedFinding> = pairing
        .only_first
        .iter()
        .map(|&i| unmatched(first_refs[i]))
        .collect();
    let mut only_in_second: Vec<UnmatchedFinding> = pairing
        .only_second
        .iter()
        .map(|&i| unmatched(second_refs[i]))
        .collect();
    let order = |x: &UnmatchedFinding, y: &UnmatchedFinding| {
        (&x.repo, &x.path, x.line, &x.rule_id).cmp(&(&y.repo, &y.path, y.line, &y.rule_id))
    };
    only_in_first.sort_by(order);
    only_in_second.sort_by(order);
    RunDiff {
        first_total: first.len(),
        second_total: second.len(),
        matched: pairing.exact.len(),
        matched_by_site: pairing.by_site.len(),
        by_tier: count_rows(first, second, |f| finding_tier(f).to_string()),
        by_rule: count_rows(first, second, |f| f.rule_id.clone()),
        by_severity: count_rows(first, second, |f| f.severity.to_ascii_lowercase()),
        only_in_first,
        only_in_second,
    }
}

#[cfg(test)]
mod tests;
