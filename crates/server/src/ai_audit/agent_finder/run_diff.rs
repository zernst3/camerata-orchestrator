//! The comparison of two runs for the operator: what one run found that the other did not.
//!
//! Problems are matched by repository, rule and place. The place is the same file with the line
//! within [`LINE_SLACK`] lines, or the same quoted code in the same file. A problem in only one
//! run is never silent: it is listed in the note. The note is for the operator-only folder and is
//! never written into a client file.

use crate::onboard::Finding;

/// How far apart (in lines) two problems may sit in the same file and still be one problem.
pub const LINE_SLACK: usize = 3;

/// The shortest quoted code that counts as the same place on its own. A bare brace or a short
/// keyword would match anywhere.
const MIN_QUOTE_CHARS: usize = 8;

/// One problem that appears in only one of two runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffEntry {
    pub rule_id: String,
    pub path: String,
    pub line: usize,
    pub severity: String,
    pub title: String,
}

/// A problem both runs hold, rated differently.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeverityChange {
    pub rule_id: String,
    pub path: String,
    pub line: usize,
    pub title: String,
    /// The severity in run A.
    pub in_a: String,
    /// The severity in run B.
    pub in_b: String,
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
    /// Problems both runs hold whose severity differs.
    pub severity_changed: Vec<SeverityChange>,
}

impl RunDiff {
    /// Whether the two runs hold the same problems at the same severities.
    pub fn is_empty(&self) -> bool {
        self.only_in_a.is_empty() && self.only_in_b.is_empty() && self.severity_changed.is_empty()
    }
}

fn entry(f: &Finding) -> DiffEntry {
    DiffEntry {
        rule_id: f.rule_id.clone(),
        path: f.path.clone(),
        line: f.line,
        severity: f.severity.clone(),
        title: title_of(f),
    }
}

/// The problem's title: the first line of its explanation, else its quoted code.
fn title_of(f: &Finding) -> String {
    let first = f.detail.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        f.snippet.trim().to_string()
    } else {
        first.to_string()
    }
}

fn quote_of(f: &Finding) -> &str {
    f.snippet.trim()
}

/// How well `b` fits `a` as the same problem: `None` when it is not the same, else the distance
/// (0 is the best) used to pick the nearest candidate.
fn fit(a: &Finding, b: &Finding) -> Option<usize> {
    if a.repo != b.repo || a.rule_id != b.rule_id || a.path != b.path {
        return None;
    }
    let apart = a.line.abs_diff(b.line);
    if apart <= LINE_SLACK {
        return Some(apart);
    }
    let (qa, qb) = (quote_of(a), quote_of(b));
    if qa.chars().count() >= MIN_QUOTE_CHARS && qa == qb {
        return Some(apart);
    }
    None
}

/// Compare the problems of two runs. Each problem is matched with at most one problem of the
/// other run, the nearest one first.
pub fn diff_runs(a: &[Finding], b: &[Finding]) -> RunDiff {
    let mut taken = vec![false; b.len()];
    let mut diff = RunDiff::default();
    for fa in a {
        let best = b
            .iter()
            .enumerate()
            .filter(|(i, _)| !taken[*i])
            .filter_map(|(i, fb)| fit(fa, fb).map(|d| (d, i)))
            .min();
        match best {
            Some((_, i)) => {
                taken[i] = true;
                diff.in_both += 1;
                if b[i].severity != fa.severity {
                    diff.severity_changed.push(SeverityChange {
                        rule_id: fa.rule_id.clone(),
                        path: fa.path.clone(),
                        line: fa.line,
                        title: title_of(fa),
                        in_a: fa.severity.clone(),
                        in_b: b[i].severity.clone(),
                    });
                }
            }
            None => diff.only_in_a.push(entry(fa)),
        }
    }
    for (i, fb) in b.iter().enumerate() {
        if !taken[i] {
            diff.only_in_b.push(entry(fb));
        }
    }
    diff
}

fn place(e: &DiffEntry) -> String {
    format!("{} line {}", e.path, e.line)
}

/// The note shown to the operator for a difference between two runs (never silent). Plain words.
/// For the operator-only folder; never written into a client file.
pub fn operator_note(diff: &RunDiff) -> String {
    let mut out = String::from(
        "# Difference between two runs\n\nFor the operator only. This note is never part of a client file.\n\n",
    );
    if diff.is_empty() {
        out.push_str(&format!(
            "The two runs found the same problems at the same severities ({} in each).\n",
            diff.in_both
        ));
        return out;
    }
    out.push_str(&format!(
        "{} problem(s) are in both runs. The rest differ.\n\n",
        diff.in_both
    ));
    if !diff.only_in_a.is_empty() {
        out.push_str(&format!(
            "## Only in the first run ({})\n\nThe second run did not report these.\n\n",
            diff.only_in_a.len()
        ));
        for e in &diff.only_in_a {
            out.push_str(&format!(
                "- {} at {}, rated {}: {}\n",
                e.rule_id,
                place(e),
                e.severity,
                e.title
            ));
        }
        out.push('\n');
    }
    if !diff.only_in_b.is_empty() {
        out.push_str(&format!(
            "## Only in the second run ({})\n\nThe first run did not report these.\n\n",
            diff.only_in_b.len()
        ));
        for e in &diff.only_in_b {
            out.push_str(&format!(
                "- {} at {}, rated {}: {}\n",
                e.rule_id,
                place(e),
                e.severity,
                e.title
            ));
        }
        out.push('\n');
    }
    if !diff.severity_changed.is_empty() {
        out.push_str(&format!(
            "## Rated differently ({})\n\nBoth runs report these, with a different severity.\n\n",
            diff.severity_changed.len()
        ));
        for c in &diff.severity_changed {
            out.push_str(&format!(
                "- {} at {} line {}: {} in the first run, {} in the second: {}\n",
                c.rule_id, c.path, c.line, c.in_a, c.in_b, c.title
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "run_diff_tests.rs"]
mod tests;
