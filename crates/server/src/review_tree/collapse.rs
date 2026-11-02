//! Collapse on mechanical identity only (design 3A.1, with amendment A1).
//!
//! Two rows collapse when ALL of these hold:
//!
//! 1. the same repo and path;
//! 2. the same rule family ([`super::mapping::family_of`]): the same native id, an external
//!    tool id the mapping assigns to that native rule, or the floor row for the same rule. An
//!    `AI-` id is never in a native family;
//! 3. the same place ([`same_place`]): the same resolved line, or lines within
//!    [`crate::ai_audit::SEMANTIC_MERGE_WINDOW`] where the snippets share a captured object or
//!    one contains the other.
//!
//! The surviving row takes the highest tier's text (deterministic floor, then the model, then an
//! external tool) and the maximum severity. Every collapsed source stays on the survivor in
//! [`FlaggedBy`] with its OWN path, line and site key (A1), so a decision on the survivor covers
//! the source by key and the Sites sheet can still list the source at its own line.
//!
//! Everything else that looks similar is NOT collapsed here: it is linked by the grouping pass.
//! A row held for review is never collapsed into an asserted one and an informational row is
//! never collapsed into a defect ([`crate::ai_audit::kinds_merge_compatible`]); two secrets are
//! one only when they are the same credential.

use std::collections::HashMap;

use super::mapping::{family_of, is_external_tool_row, ToolMap};
use super::types::FlaggedBy;
use crate::ai_audit::{
    asserted_rank, cap_placeholder_severity, class_rank, confidence_rank, dedup_snippet_key,
    finding_class, finding_origin, inherit_fix, is_placeholder_secret_row, is_secret_rule,
    kind_actionability_rank, kinds_merge_compatible, record_severity_adjustment, same_credential,
    same_snippet, severity_rank, shared_captured_object, Origin, MIN_MERGE_SNIPPET,
    SEMANTIC_MERGE_WINDOW,
};
use crate::onboard::evidence_check::{
    EVIDENCE_UNVERIFIED_CAPTURE, EVIDENCE_UNVERIFIED_SITES_CAPTURE,
};
use crate::onboard::Finding;

/// How many later rows one row is compared with inside its window. A pile of more rows than this
/// on a handful of adjacent lines is not a real report; the cap only keeps a hostile one linear.
/// A missed comparison can only leave two rows apart, never fuse two that should not be.
const MAX_FOLLOWERS: usize = 64;

/// One row that was collapsed, for the stage's accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapsedRow {
    /// The rule id the collapsed row was flagged under.
    pub rule_id: String,
    /// The rule id of the row it was collapsed into.
    pub into_rule_id: String,
}

/// What [`collapse_identical`] returns. `rows_in == rows.len() + collapsed.len()`, always.
#[derive(Debug, Clone, Default)]
pub struct CollapseOutcome {
    pub rows: Vec<Finding>,
    pub collapsed: Vec<CollapsedRow>,
}

impl CollapseOutcome {
    /// The ledger accounting of the stage: one merge entry per collapsed row.
    pub fn accounting(&self) -> crate::scan_ledger::StageAccounting {
        let mut acc = crate::scan_ledger::StageAccounting::default();
        for c in &self.collapsed {
            acc.push_merge(c.rule_id.clone(), c.into_rule_id.clone());
        }
        acc
    }
}

/// Set [`Finding::located`] for every row: the row cites real code present in its file (a
/// presence-type violation) rather than a description. The merge pass used to compute it as a
/// side effect; the informational-bucketing predicate reads it. A row whose path has no content
/// in `files` is not located.
pub fn mark_located(findings: &mut [Finding], files: &[(String, String)]) {
    let by_path: HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    for f in findings.iter_mut() {
        let snippet = f.snippet.trim();
        f.located = f.line != 0
            && snippet.len() >= MIN_MERGE_SNIPPET
            && by_path
                .get(f.path.as_str())
                .is_some_and(|c| c.contains(snippet));
    }
}

/// A union-find over row indices. The root of a set is always its LOWEST index, so a component's
/// identity does not depend on the order unions were made in.
#[derive(Debug)]
pub(super) struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    pub(super) fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    pub(super) fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    pub(super) fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (low, high) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.parent[high] = low;
    }
}

/// Whether a row may collapse at all: it cites a real line of real code, and it is not an
/// informational statement (those are one per rule by construction and are never fused).
fn eligible(f: &Finding) -> bool {
    f.line != 0 && f.located && kind_actionability_rank(f) != 0
}

/// Condition 3 of 3A.1 (the one the repeatability pass shares): the same resolved line, or lines
/// within the window where the snippets share a captured object or one contains the other.
pub fn same_place(a: &Finding, b: &Finding) -> bool {
    if a.repo != b.repo || a.path != b.path || a.line == 0 || b.line == 0 {
        return false;
    }
    if a.line == b.line {
        return true;
    }
    if a.line.abs_diff(b.line) > SEMANTIC_MERGE_WINDOW {
        return false;
    }
    if shared_captured_object(a, b) {
        return true;
    }
    // Two rows that cite no code prove nothing about being the same site, so an empty snippet
    // is never "contained" in anything.
    let (ka, kb) = (dedup_snippet_key(&a.snippet), dedup_snippet_key(&b.snippet));
    !ka.is_empty() && !kb.is_empty() && same_snippet(&ka, &kb)
}

/// Whether two rows of one family are mechanically the same site.
fn identical(a: &Finding, b: &Finding) -> bool {
    if !kinds_merge_compatible(a, b) || !same_place(a, b) {
        return false;
    }
    // Two secrets are one finding only when they are the same credential: "both are a leaked
    // secret" and "a few lines apart" prove nothing, and a collapse lets one decision dispose of
    // both.
    if is_secret_rule(&a.rule_id) && is_secret_rule(&b.rule_id) && !same_credential(a, b) {
        return false;
    }
    true
}

/// Which tier produced a row, for choosing whose text survives: the deterministic floor (3), the
/// model (2), an external tool (1).
fn tier_rank(f: &Finding) -> u8 {
    if is_external_tool_row(f) {
        1
    } else if finding_origin(f) == Origin::Deterministic {
        3
    } else {
        2
    }
}

/// The ranking that picks the survivor of a collapse; higher wins. The same keys as the primary of
/// a sub-group, with the design's tier order (floor, model, tool) in the place of origin.
fn survivor_rank(f: &Finding, index: usize, total: usize) -> (u8, u8, u8, u8, u8, u8, u8, usize) {
    (
        kind_actionability_rank(f),
        asserted_rank(f),
        u8::from(!is_placeholder_secret_row(f)),
        class_rank(finding_class(f)),
        tier_rank(f),
        severity_rank(&f.severity),
        confidence_rank(f.confidence.as_deref()),
        total - index,
    )
}

/// A collapse must never launder a row the evidence check held: when a collapsed source carries
/// the evidence-unverified mark and the survivor does not, the survivor takes the mark and names
/// the source's site, so it stays held. A survivor already marked names the extra sites too.
fn carry_evidence_marks(survivor: &mut Finding, sources: &[Finding]) {
    if sources.is_empty() {
        return;
    }
    let mut sites: Vec<String> = survivor
        .captures
        .get(EVIDENCE_UNVERIFIED_SITES_CAPTURE)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    for src in sources {
        let own = format!("{}:{}", src.path, src.line);
        if !sites.contains(&own) {
            sites.push(own);
        }
        if let Some(listed) = src.captures.get(EVIDENCE_UNVERIFIED_SITES_CAPTURE) {
            for l in listed.lines() {
                if !sites.iter().any(|s| s == l) {
                    sites.push(l.to_string());
                }
            }
        }
    }
    if !survivor.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE) {
        let reason = sources[0]
            .captures
            .get(EVIDENCE_UNVERIFIED_CAPTURE)
            .cloned()
            .unwrap_or_default();
        survivor
            .captures
            .insert(EVIDENCE_UNVERIFIED_CAPTURE.to_string(), reason);
    }
    survivor.captures.insert(
        EVIDENCE_UNVERIFIED_SITES_CAPTURE.to_string(),
        sites.join("\n"),
    );
}

/// Collapse every set of mechanically identical rows into one row, keeping every source in
/// [`Finding::flagged_by`]. Pure and linear: rows are bucketed by repo, path and rule family
/// first, and only compared inside a line window. The output keeps the input's order (a survivor
/// stays where it was); `rows.len() + collapsed.len() == findings.len()`.
pub fn collapse_identical(mut findings: Vec<Finding>, tools: &ToolMap) -> CollapseOutcome {
    let n = findings.len();
    // What each row would be named had it stayed a row, taken before anything is folded.
    let keys = crate::inspections::site_keys_of(&findings);
    findings.iter_mut().for_each(cap_placeholder_severity);

    let mut buckets: HashMap<(String, String, String), Vec<usize>> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        if eligible(f) {
            buckets
                .entry((f.repo.clone(), f.path.clone(), family_of(f, tools)))
                .or_default()
                .push(i);
        }
    }
    let mut uf = UnionFind::new(n);
    for members in buckets.values_mut() {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|&i| (findings[i].line, i));
        for (k, &a) in members.iter().enumerate() {
            for &b in members[k + 1..].iter().take(MAX_FOLLOWERS) {
                if findings[b].line.abs_diff(findings[a].line) > SEMANTIC_MERGE_WINDOW {
                    break;
                }
                if uf.find(a) != uf.find(b) && identical(&findings[a], &findings[b]) {
                    uf.union(a, b);
                }
            }
        }
    }

    // Components with more than one member, by root.
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = uf.find(i);
        components.entry(root).or_default().push(i);
    }
    let mut survivor_of: HashMap<usize, usize> = HashMap::new(); // member index -> survivor index
    let mut folded: HashMap<usize, Vec<usize>> = HashMap::new(); // survivor index -> sources
    for members in components.values() {
        if members.len() < 2 {
            continue;
        }
        let survivor = members
            .iter()
            .copied()
            .max_by_key(|&i| survivor_rank(&findings[i], i, n))
            .unwrap_or(members[0]);
        let mut sources: Vec<usize> = members.iter().copied().filter(|&i| i != survivor).collect();
        sources.sort_unstable();
        for &s in &sources {
            survivor_of.insert(s, survivor);
        }
        folded.insert(survivor, sources);
    }

    let mut rows: Vec<Finding> = Vec::with_capacity(n - survivor_of.len());
    let mut collapsed: Vec<CollapsedRow> = Vec::with_capacity(survivor_of.len());
    let mut sources_taken: Vec<Option<Finding>> = findings.into_iter().map(Some).collect();
    for i in 0..n {
        if survivor_of.contains_key(&i) {
            continue;
        }
        let Some(mut row) = sources_taken[i].take() else {
            continue;
        };
        if let Some(sources) = folded.get(&i) {
            let max_severity = std::iter::once(row.severity.clone())
                .chain(
                    sources
                        .iter()
                        .filter_map(|&s| sources_taken[s].as_ref().map(|f| f.severity.clone())),
                )
                .max_by_key(|s| severity_rank(s))
                .unwrap_or_else(|| row.severity.clone());
            let mut donor: Option<Finding> = None;
            let mut folded_rows: Vec<Finding> = Vec::new();
            for &s in sources {
                let Some(src) = sources_taken[s].take() else {
                    continue;
                };
                collapsed.push(CollapsedRow {
                    rule_id: src.rule_id.clone(),
                    into_rule_id: row.rule_id.clone(),
                });
                row.flagged_by.push(FlaggedBy {
                    rule_id: src.rule_id.clone(),
                    tool: src.preview_tool.clone(),
                    site_key: keys[s].clone(),
                    path: src.path.clone(),
                    line: src.line,
                });
                row.flagged_by.extend(src.flagged_by.iter().cloned());
                if src.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE) {
                    folded_rows.push(src.clone());
                }
                if donor.is_none() && src.fix_specific.is_some() {
                    donor = Some(src);
                }
            }
            carry_evidence_marks(&mut row, &folded_rows);
            if severity_rank(&max_severity) > severity_rank(&row.severity) {
                let from = row.severity.clone();
                record_severity_adjustment(
                    &mut row,
                    &from,
                    &max_severity,
                    "a row collapsed into this one at the same place was rated higher",
                );
                row.severity = max_severity;
            }
            if row.fix_specific.is_none() {
                if let Some(donor) = &donor {
                    inherit_fix(&mut row, donor);
                }
            }
            crate::onboard::redact::redact_finding(&mut row);
        }
        rows.push(row);
    }
    CollapseOutcome { rows, collapsed }
}
