//! The Fix as the unit of the plan (design D16, item #1): what the Fix plan sheet, the ticket CSV
//! and the PDF's patterns each print about one change.
//!
//! A sub-group is one rule's places with one cause ([`SubgroupFacts`]). A Fix ([`FixFacts`]) is
//! the sub-groups whose places are one change (`review_tree::merge_fixes`), folded: every rule it
//! satisfies is listed with its citation, the effort is the largest band once, the severity the
//! worst, the fix order the earliest any member would have had alone. Nothing is dropped: a rule
//! that covers only some of the Fix's places says which.
//!
//! Everything here is worked out ONCE from the same rows the other sheets are written from
//! ([`super::partition_rows_for`]) and the tree of the reviewed report, so a plan row, a ticket and
//! a pattern in the PDF agree by construction: the same sites, the same severity, the same fix.
//!
//! - Sites the reviewer marked a false positive, and sites ignored as accepted risk, are not in a
//!   plan row or a ticket. A sub-group with nothing left to fix has none.
//! - The fix is written once: the corpus remediation resolved with the sub-group's primary row's
//!   captures. The worked example is the primary's own `fix_specific`.
//! - The text that came from the group reviewer (label, cause, summary) has passed the prose checks
//!   ([`crate::report_export::prose_checks`]); a reviewer's reason for a split never appears, only
//!   the label they gave the sub-group.

use std::collections::BTreeMap;

use super::{severity_rank, title_case, vocabulary, DepRow, FindingRow};
use crate::report_export::prose_checks::client_text;
use crate::report_export::{Disposition, ReviewedReport};
use crate::review_tree::{is_thrown_out_id, NodeLevel};

/// What the plan, the tickets and the PDF's patterns say for a report that has findings and no
/// review tree (one stored before the tree existed): there are no sub-groups to plan, and
/// printing nothing would read as "nothing to fix".
pub(crate) const NO_TREE_NOTE: &str =
    "This report has no review tree, so there are no fixes to plan. Every problem is on the \
     All problems sheet.";

/// [`NO_TREE_NOTE`] when `reviewed` has findings and no tree, empty otherwise.
pub(crate) fn no_tree_note(reviewed: &ReviewedReport) -> &'static str {
    let report = reviewed.report();
    if report.tree.is_none() && !report.findings.is_empty() {
        NO_TREE_NOTE
    } else {
        ""
    }
}

/// One site of a sub-group, as a ticket and a plan row name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SiteFact {
    pub site_id: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub severity: String,
    pub headline: String,
    /// The places the row stands for (an older report's merge counts each).
    pub count: usize,
    /// The row's review level wire name.
    pub review_level: String,
    pub escalated: bool,
    /// What a reviewer decided about the row, in words; empty while it is open.
    pub decided: String,
    /// Why the row waits for a reviewer, empty when it does not.
    pub waiting: String,
    /// The rule that flagged the row, its title and the standard it cites. A sub-group can hold
    /// rows of several rules, and a Fix lists every one.
    pub rule_id: String,
    pub rule_title: String,
    /// Whether the corpus has the rule; one it lacks is named by id beside its plain name.
    pub rule_known: bool,
    pub citation: String,
    /// Other rules that flagged this place and were collapsed into the row: `(rule id, title,
    /// citation)`. A Fix lists them as rules of its own, so a collapse never hides a rule.
    pub also_rules: Vec<(String, String, String, bool)>,
}

impl SiteFact {
    /// The place, as the key that tells places apart.
    pub fn place(&self) -> (String, String, usize) {
        (self.repo.clone(), self.path.clone(), self.line)
    }

    /// The place as a cell prints it: the file alone for a whole-file place.
    pub fn label(&self, multi_repo: bool) -> String {
        let at = if self.line > 0 {
            format!("{}:{}", self.path, self.line)
        } else {
            self.path.clone()
        };
        if multi_repo && !self.repo.is_empty() {
            format!("{}/{at}", self.repo)
        } else {
            at
        }
    }
}

/// When a sub-group's fix is scheduled (design 3B item 2, from the severity x effort matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Sequence {
    /// Critical, high, or medium at low effort.
    First,
    /// The rest of medium.
    Next,
    /// Low and informational.
    Later,
}

impl Sequence {
    /// The fix order word, from the vocabulary.
    pub fn label(self) -> &'static str {
        match self {
            Self::First => vocabulary::FIX_ORDER_FIRST,
            Self::Next => vocabulary::FIX_ORDER_NEXT,
            Self::Later => vocabulary::FIX_ORDER_LATER,
        }
    }

    /// The fix order of a fix whose worst problem has `severity` and whose usual effort is
    /// `effort`: critical and high first, medium at low effort first, the rest of medium next,
    /// everything else later.
    pub fn of(severity: &str, effort: &str) -> Self {
        sequence_of(severity, effort)
    }
}

/// One sub-group with something to fix: one rule's places that share one cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubgroupFacts {
    /// The tree node id of the sub-group.
    pub id: String,
    /// The Fix the sub-group is in.
    pub fix_id: String,
    pub label: String,
    pub cause: String,
    pub summary: String,
    pub rule_id: String,
    pub rule_title: String,
    /// The standard the rule cites, as the workbook prints it.
    pub citation: String,
    pub category: String,
    /// The worst severity among the places still to fix, in title case.
    pub severity: String,
    /// The places still to fix: not a false positive and not accepted risk. Sorted by place.
    pub open: Vec<SiteFact>,
    /// Every place of the sub-group that is not a false positive (open and ignored).
    pub live: Vec<SiteFact>,
    /// The effort most of the open places carry: `low`, `medium`, `high` or empty.
    pub effort: String,
    /// The fix, written once.
    pub fix: String,
    /// The primary row's own fix for its code, as the worked example.
    pub worked_example: String,
    pub sequence: Sequence,
    pub is_dependency: bool,
}

fn effort_majority<'a>(efforts: impl Iterator<Item = &'a str>) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in efforts.filter(|e| !e.is_empty()) {
        *counts.entry(e).or_default() += 1;
    }
    // The most common effort; on a tie the larger effort, so a plan never promises less than the
    // evidence supports.
    let order = |e: &str| match e {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    };
    counts
        .into_iter()
        .max_by_key(|(e, n)| (*n, order(e)))
        .map(|(e, _)| e.to_string())
        .unwrap_or_default()
}

/// `src/handlers` for `src/handlers/users.rs`: at most the first two directories.
fn folder_of(repo: &str, path: &str, multi_repo: bool) -> String {
    let dirs: Vec<&str> = path.split('/').collect();
    let dir = if dirs.len() <= 1 {
        "(top level)".to_string()
    } else {
        dirs[..dirs.len() - 1]
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>()
            .join("/")
    };
    if multi_repo && !repo.is_empty() {
        format!("{repo}/{dir}")
    } else {
        dir
    }
}

fn top_folders<'a>(
    places: impl Iterator<Item = (&'a str, &'a str)>,
    multi_repo: bool,
) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (repo, path) in places {
        *counts.entry(folder_of(repo, path, multi_repo)).or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(3)
        .map(|(dir, n)| format!("{dir} ({n})"))
        .collect()
}

fn sequence_of(severity: &str, effort: &str) -> Sequence {
    match severity.to_ascii_lowercase().as_str() {
        "critical" | "high" => Sequence::First,
        "medium" if effort == "low" => Sequence::First,
        "medium" => Sequence::Next,
        _ => Sequence::Later,
    }
}

fn severity_title(rank: u8) -> &'static str {
    match rank {
        0 => "Critical",
        1 => "High",
        2 => "Medium",
        _ => "Low",
    }
}

/// The fix every dependency sub-group states once.
const DEPENDENCY_FIX: &str = "Upgrade each package to the patched version listed on the \
     Dependencies sheet. Where no patched version exists yet, the Dependencies sheet says what to \
     do instead.";

/// The sub-groups of `reviewed` with something to fix, one per rule group.
fn subgroup_facts_with(
    reviewed: &ReviewedReport,
    rows: &[FindingRow],
    deps: &[DepRow],
    corpus: Option<&camerata_rules::RuleSet>,
    include_closed: bool,
    fixes: &crate::review_tree::FixIndex,
) -> Vec<SubgroupFacts> {
    let report = reviewed.report();
    let Some(tree) = report.tree.as_ref() else {
        return Vec::new();
    };
    // Which row is the sub-group's primary: the row the grouping put first.
    let primary_ids: std::collections::HashSet<String> = report
        .findings
        .iter()
        .filter(|f| f.group_role == crate::review_tree::GroupRole::Primary)
        .map(|f| crate::report_export::site_id(&f.site_key))
        .collect();

    let mut out = Vec::new();
    for node in tree
        .nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Subgroup && !is_thrown_out_id(&n.id))
    {
        let in_group: Vec<&FindingRow> = rows.iter().filter(|r| r.facts.group == node.id).collect();
        let dep_in_group: Vec<&DepRow> = deps.iter().filter(|d| d.facts.group == node.id).collect();
        let label = client_text("label", &node.label);
        let cause = client_text("cause", node.cause.as_deref().unwrap_or_default());
        let summary = client_text("summary", node.summary.as_deref().unwrap_or_default());
        let label = if label.flags.is_empty() {
            label.text
        } else {
            vocabulary::H_FIX.to_string()
        };
        let fix_id = fixes
            .fix_of_subgroup(&node.id)
            .map(str::to_string)
            .unwrap_or_default();

        if !dep_in_group.is_empty() {
            let rank = dep_in_group
                .iter()
                .map(|d| severity_rank(&d.severity))
                .min()
                .unwrap_or(3);
            let severity = severity_title(rank).to_string();
            let mut sites: Vec<SiteFact> = dep_in_group
                .iter()
                .map(|d| SiteFact {
                    site_id: d.facts.site_id.clone(),
                    repo: d.repo.clone(),
                    path: d.path.clone(),
                    line: 0,
                    severity: title_case(&d.severity),
                    headline: format!("{}: {}", d.package, d.advisory),
                    count: 1,
                    review_level: d.facts.review_level.clone(),
                    escalated: d.facts.escalated,
                    decided: String::new(),
                    waiting: String::new(),
                    rule_id: crate::dep_audit::DEP_AUDIT_RULE_ID.to_string(),
                    rule_title: "Known vulnerable dependency".to_string(),
                    rule_known: true,
                    citation: String::new(),
                    also_rules: Vec::new(),
                })
                .collect();
            sites.sort_by(|a, b| {
                (&a.repo, &a.path, &a.headline).cmp(&(&b.repo, &b.path, &b.headline))
            });
            let example = dep_in_group
                .iter()
                .find_map(|d| {
                    d.fixed_version
                        .as_ref()
                        .map(|v| format!("Upgrade {} to {v}.", d.package))
                })
                .unwrap_or_default();
            out.push(SubgroupFacts {
                id: node.id.clone(),
                fix_id,
                label,
                cause: cause.text,
                summary: summary.text,
                rule_id: crate::dep_audit::DEP_AUDIT_RULE_ID.to_string(),
                rule_title: "Known vulnerable dependency".to_string(),
                citation: String::new(),
                category: "Dependencies".to_string(),
                sequence: sequence_of(&severity, ""),
                severity,
                live: sites.clone(),
                open: sites,
                effort: String::new(),
                fix: DEPENDENCY_FIX.to_string(),
                worked_example: example,
                is_dependency: true,
            });
            continue;
        }

        let live: Vec<&FindingRow> = in_group.iter().copied().filter(|r| !r.is_fp).collect();
        let open: Vec<&FindingRow> = live
            .iter()
            .copied()
            .filter(|r| {
                !matches!(
                    r.disposition_kind,
                    Some(
                        Disposition::Ignored
                            | Disposition::BaselineAccepted
                            | Disposition::WaivedInline
                    )
                )
            })
            .collect();
        if open.is_empty() && (!include_closed || live.is_empty()) {
            continue;
        }
        // A sub-group with nothing left to fix describes itself from its ignored sites.
        let basis: &[&FindingRow] = if open.is_empty() { &live } else { &open };
        let primary = basis
            .iter()
            .copied()
            .find(|r| primary_ids.contains(&r.facts.site_id))
            .unwrap_or(basis[0]);
        let rank = basis
            .iter()
            .map(|r| severity_rank(&r.severity))
            .min()
            .unwrap_or(3);
        let severity = severity_title(rank).to_string();
        let effort = effort_majority(basis.iter().map(|r| r.effort.as_str()));
        let site_of = |r: &&FindingRow| SiteFact {
            site_id: r.facts.site_id.clone(),
            repo: r.repo.clone(),
            path: r.path.clone(),
            line: r.line,
            severity: title_case(&r.severity),
            headline: r.headline.clone(),
            count: r.site_count.max(1),
            review_level: r.facts.review_level.clone(),
            escalated: r.facts.escalated,
            decided: super::sheets::decision_so_far(r),
            waiting: if r.needs_review {
                r.why_waiting.clone().unwrap_or_default()
            } else {
                String::new()
            },
            rule_id: r.rule_id.clone(),
            rule_title: own_name(&r.rule_id, corpus).0,
            rule_known: own_name(&r.rule_id, corpus).1,
            citation: r.citation_label.clone(),
            also_rules: also_rules_of(r, corpus),
        };
        let by_place = |a: &SiteFact, b: &SiteFact| {
            (&a.repo, &a.path, a.line, &a.site_id).cmp(&(&b.repo, &b.path, b.line, &b.site_id))
        };
        let mut sites: Vec<SiteFact> = open.iter().map(site_of).collect();
        sites.sort_by(by_place);
        let mut live_sites: Vec<SiteFact> = live.iter().map(site_of).collect();
        live_sites.sort_by(by_place);
        out.push(SubgroupFacts {
            id: node.id.clone(),
            fix_id,
            label,
            cause: cause.text,
            summary: summary.text,
            rule_id: primary.rule_id.clone(),
            rule_title: own_name(&primary.rule_id, corpus).0,
            citation: primary.citation_label.clone(),
            category: primary.category.clone(),
            sequence: sequence_of(&severity, &effort),
            severity,
            open: sites,
            live: live_sites,
            effort,
            fix: primary.fix.clone().unwrap_or_default(),
            worked_example: primary.fix_specific.clone().unwrap_or_default(),
            is_dependency: false,
        });
    }
    out
}

/// A rule's own name and whether the corpus knows it. A rule the corpus lacks (a rule the model
/// invented) is named by its id written as a plain name, never by the title of the rule that
/// grounds its citation, so a Fix never says it closes a rule it does not.
fn own_name(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> (String, bool) {
    match corpus.and_then(|c| c.get_by_id(rule_id)) {
        Some(rule) => (rule.title.clone(), true),
        None => (crate::client_text::plain_rule_name(rule_id), false),
    }
}

/// The rules other than the row's own that flagged its place: the sources collapsed into it and
/// the sites absorbed into it, each once, with title and citation.
fn also_rules_of(
    row: &FindingRow,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<(String, String, String, bool)> {
    let mut ids: Vec<&str> = Vec::new();
    for id in row
        .flagged_by
        .iter()
        .map(|b| b.rule_id.as_str())
        .chain(row.also_locations.iter().map(|l| l.rule_id.as_str()))
        .chain(row.also_match_ids.iter().map(String::as_str))
    {
        if !id.is_empty() && id != row.rule_id && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.into_iter()
        .map(|id| {
            let (title, known) = own_name(id, corpus);
            (
                id.to_string(),
                title,
                crate::report_export::resolve_citation(id, None, corpus).label,
                known,
            )
        })
        .collect()
}

/// One rule a Fix satisfies, with where it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixRule {
    pub rule_id: String,
    pub title: String,
    /// Whether the corpus has the rule. One it lacks is written with its id beside its name.
    pub known: bool,
    /// The standard the rule cites.
    pub citation: String,
    /// The worst severity of the rule's places in the Fix, in title case.
    pub severity: String,
    /// How many of the Fix's places the rule covers.
    pub places: usize,
    /// The rule's own places, named, when it covers only some of the Fix's places; empty when it
    /// covers them all.
    pub only_at: Vec<String>,
    /// Open while one place of the rule is undecided; otherwise how it was decided.
    pub state: RuleState,
    /// Why the rule's open places wait for a reviewer, when they do.
    pub waiting: String,
}

impl FixRule {
    /// The rule as a line of the Rules column: its title, and where, when it is not everywhere.
    pub fn line(&self, fix_places: usize) -> String {
        let name = if self.known {
            self.title.clone()
        } else {
            format!("{} ({})", self.title, self.rule_id)
        };
        if self.only_at.is_empty() {
            name
        } else {
            vocabulary::rule_at_places(&name, &self.only_at, fix_places)
        }
    }
}

/// What a reviewer has decided about one rule's places in a Fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleState {
    /// At least one place is undecided.
    Open,
    /// Every place was decided alike (the words), or the rule has nothing left to decide.
    Decided(String),
    /// Decided, and not alike.
    Mixed,
}

/// A Fix: the sub-groups that are one change, folded into one plan row, one ticket and one
/// pattern. The fields every consumer read from a sub-group before are here under the same names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixFacts {
    /// The Fix id (`review_tree::FixNode::id`).
    pub id: String,
    /// The ids of the rule groups folded into it, the primary's first.
    pub members: Vec<String>,
    /// The primary member's label, cause and summary.
    pub label: String,
    pub cause: String,
    pub summary: String,
    /// The primary rule.
    pub rule_id: String,
    pub rule_title: String,
    /// Every rule the Fix satisfies, worst severity first. Never empty.
    pub rules: Vec<FixRule>,
    /// The areas of the rules, once each, comma separated.
    pub category: String,
    /// The worst severity among the places still to fix, in title case.
    pub severity: String,
    /// The places still to fix, each once. Sorted by place.
    pub open: Vec<SiteFact>,
    /// Places the reviewer ignored as accepted risk.
    pub ignored: usize,
    /// Every place of the Fix that is not a false positive (open and ignored).
    pub live_sites: usize,
    /// The places still to fix, counted the same way.
    pub open_site_total: usize,
    /// Live places by review level wire name.
    pub review_levels: BTreeMap<String, usize>,
    /// Live places the group reviewer escalated.
    pub escalated: usize,
    /// The largest effort among the members: `low`, `medium`, `high` or empty. Counted once.
    pub effort: String,
    /// The fix, one entry for each distinct text, the primary's first.
    pub fix: String,
    /// The primary's own fix for its code, as the worked example.
    pub worked_example: String,
    /// The three folders holding most of the open places, with their counts.
    pub folders: Vec<String>,
    pub sequence: Sequence,
    pub is_dependency: bool,
    /// Where the Fix stands with the reviewer, in words: Open with counts, the state all its
    /// rules share, or Mixed. Empty when there is nothing to decide.
    pub decision: String,
    /// The other fixes this one shares places with without containing them.
    pub shares: Vec<String>,
}

fn effort_rank(effort: &str) -> u8 {
    match effort {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn level_strength(level: &str) -> u8 {
    if level == "personally_reviewed" {
        3
    } else if level.starts_with("reviewed_as_pattern") {
        2
    } else if level == "engine_verified" {
        1
    } else {
        0
    }
}

/// `sites`, each place once: the worst severity's row stands for it, the largest count is kept
/// and the place is escalated when any row at it is.
fn union_places<'a>(lists: impl Iterator<Item = &'a [SiteFact]>) -> Vec<SiteFact> {
    let mut by_place: BTreeMap<(String, String, usize), SiteFact> = BTreeMap::new();
    for site in lists.flatten() {
        match by_place.get_mut(&site.place()) {
            None => {
                by_place.insert(site.place(), site.clone());
            }
            Some(kept) => {
                let (count, escalated) =
                    (kept.count.max(site.count), kept.escalated || site.escalated);
                let stronger =
                    level_strength(&site.review_level) > level_strength(&kept.review_level);
                let level = if stronger {
                    site.review_level.clone()
                } else {
                    kept.review_level.clone()
                };
                if severity_rank(&site.severity.to_ascii_lowercase())
                    < severity_rank(&kept.severity.to_ascii_lowercase())
                {
                    *kept = site.clone();
                }
                kept.count = count;
                kept.escalated = escalated;
                kept.review_level = level;
            }
        }
    }
    by_place.into_values().collect()
}

fn fold_fix(
    id: &str,
    mut members: Vec<SubgroupFacts>,
    node: Option<&crate::review_tree::FixNode>,
    multi_repo: bool,
) -> FixFacts {
    // The primary: the worst severity, then the earliest fix order, then the id.
    members.sort_by_key(|m| {
        (
            severity_rank(&m.severity.to_ascii_lowercase()),
            m.sequence,
            m.id.clone(),
        )
    });
    let primary = &members[0];
    let is_dependency = primary.is_dependency;
    let live_all: Vec<SiteFact> = if is_dependency {
        members.iter().flat_map(|m| m.live.clone()).collect()
    } else {
        union_places(members.iter().map(|m| m.live.as_slice()))
    };
    let open_all: Vec<SiteFact> = if is_dependency {
        members.iter().flat_map(|m| m.open.clone()).collect()
    } else {
        union_places(members.iter().map(|m| m.open.as_slice()))
    };
    let open_keys: std::collections::HashSet<(String, String, usize)> =
        open_all.iter().map(SiteFact::place).collect();
    let mut review_levels: BTreeMap<String, usize> = BTreeMap::new();
    for s in &live_all {
        *review_levels.entry(s.review_level.clone()).or_default() += 1;
    }
    let severity = severity_title(
        members
            .iter()
            .map(|m| severity_rank(&m.severity.to_ascii_lowercase()))
            .min()
            .unwrap_or(3),
    )
    .to_string();
    let effort = members
        .iter()
        .map(|m| m.effort.as_str())
        .max_by_key(|e| effort_rank(e))
        .unwrap_or_default()
        .to_string();
    let sequence = members
        .iter()
        .map(|m| m.sequence)
        .min()
        .unwrap_or(Sequence::Later);

    // Every rule, once, with the places it covers.
    struct Acc {
        title: String,
        known: bool,
        citation: String,
        rank: u8,
        order: usize,
        places: std::collections::BTreeSet<(String, String, usize)>,
        labels: Vec<(String, String, usize, String)>,
        open: bool,
        decided: Vec<String>,
        waiting: String,
    }
    let basis = |m: &SubgroupFacts| -> Vec<SiteFact> {
        if m.open.is_empty() {
            m.live.clone()
        } else {
            m.open.clone()
        }
    };
    let mut accs: BTreeMap<String, Acc> = BTreeMap::new();
    let mut extra: BTreeMap<String, Acc> = BTreeMap::new();
    fn blank(title: &str, known: bool, citation: &str, order: usize) -> Acc {
        Acc {
            title: title.to_string(),
            known,
            citation: citation.to_string(),
            rank: 9,
            order,
            places: Default::default(),
            labels: Vec::new(),
            open: false,
            decided: Vec::new(),
            waiting: String::new(),
        }
    }
    fn note(acc: &mut Acc, site: &SiteFact, multi_repo: bool, in_open: bool) {
        acc.rank = acc
            .rank
            .min(severity_rank(&site.severity.to_ascii_lowercase()));
        acc.places.insert(site.place());
        acc.labels.push((
            site.repo.clone(),
            site.path.clone(),
            site.line,
            site.label(multi_repo),
        ));
        if !in_open {
            return;
        }
        if site.decided.is_empty() {
            acc.open = true;
            if acc.waiting.is_empty() {
                acc.waiting.clone_from(&site.waiting);
            }
        } else {
            acc.decided.push(site.decided.clone());
        }
    }
    for (order, m) in members.iter().enumerate() {
        let in_open = !m.open.is_empty();
        for site in basis(m) {
            let acc = accs
                .entry(site.rule_id.clone())
                .or_insert_with(|| blank(&site.rule_title, site.rule_known, &site.citation, order));
            note(acc, &site, multi_repo, in_open);
            // A rule collapsed into the row is a rule of the Fix at the row's place.
            for (rule_id, title, citation, known) in &site.also_rules {
                let other = extra
                    .entry(rule_id.clone())
                    .or_insert_with(|| blank(title, *known, citation, order + members.len()));
                note(other, &site, multi_repo, in_open);
            }
        }
    }
    // A rule that was also collapsed into another keeps the entry of its own sub-group.
    for (rule_id, acc) in extra {
        accs.entry(rule_id).or_insert(acc);
    }
    let fix_places = if open_all.is_empty() {
        live_all.len()
    } else {
        open_all.len()
    };
    let mut rules: Vec<(u8, usize, String, FixRule)> = accs
        .into_iter()
        .map(|(rule_id, mut acc)| {
            acc.labels.sort();
            acc.labels.dedup();
            let only_at = if acc.places.len() < fix_places {
                acc.labels.iter().map(|l| l.3.clone()).collect()
            } else {
                Vec::new()
            };
            let state = if acc.open {
                RuleState::Open
            } else if acc.decided.is_empty() {
                RuleState::Decided(String::new())
            } else if acc.decided.iter().all(|d| *d == acc.decided[0]) {
                RuleState::Decided(acc.decided[0].clone())
            } else {
                RuleState::Mixed
            };
            (
                acc.rank,
                acc.order,
                rule_id.clone(),
                FixRule {
                    rule_id,
                    title: acc.title,
                    known: acc.known,
                    citation: acc.citation,
                    severity: severity_title(acc.rank).to_string(),
                    places: acc.places.len(),
                    only_at,
                    state,
                    waiting: acc.waiting,
                },
            )
        })
        .collect();
    rules.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let rules: Vec<FixRule> = rules.into_iter().map(|r| r.3).collect();
    let decision = decision_of(&rules, !open_all.is_empty() && !is_dependency);

    let mut fix_texts: Vec<String> = Vec::new();
    for m in &members {
        if !m.fix.is_empty() && !fix_texts.contains(&m.fix) {
            fix_texts.push(m.fix.clone());
        }
    }
    let worked_example = members
        .iter()
        .map(|m| m.worked_example.clone())
        .find(|w| !w.is_empty())
        .unwrap_or_default();
    let mut categories: Vec<String> = Vec::new();
    for m in &members {
        if !m.category.is_empty() && !categories.contains(&m.category) {
            categories.push(m.category.clone());
        }
    }
    let shares = node
        .map(|n| {
            n.shares
                .iter()
                .map(|s| vocabulary::shares_places_with(&s.fix, s.places))
                .collect()
        })
        .unwrap_or_default();
    let folder_basis: &[SiteFact] = if open_all.is_empty() {
        &live_all
    } else {
        &open_all
    };
    FixFacts {
        id: id.to_string(),
        members: members.iter().map(|m| m.id.clone()).collect(),
        label: primary.label.clone(),
        cause: primary.cause.clone(),
        summary: primary.summary.clone(),
        rule_id: primary.rule_id.clone(),
        rule_title: primary.rule_title.clone(),
        rules,
        category: categories.join(", "),
        severity,
        ignored: live_all
            .iter()
            .filter(|s| !open_keys.contains(&s.place()))
            .count(),
        live_sites: live_all.iter().map(|s| s.count).sum(),
        open_site_total: open_all.iter().map(|s| s.count).sum(),
        review_levels,
        escalated: live_all.iter().filter(|s| s.escalated).count(),
        effort,
        fix: fix_texts.join("\n\n"),
        worked_example,
        folders: top_folders(
            folder_basis
                .iter()
                .map(|s| (s.repo.as_str(), s.path.as_str())),
            multi_repo,
        ),
        sequence,
        is_dependency,
        decision,
        shares,
        open: open_all,
    }
}

/// Where a Fix stands with the reviewer, from its rules' states. Empty when `decidable` is false
/// (nothing is left to fix, or it is a dependency).
fn decision_of(rules: &[FixRule], decidable: bool) -> String {
    if !decidable || rules.is_empty() {
        return String::new();
    }
    let open = rules.iter().filter(|r| r.state == RuleState::Open).count();
    let decided: Vec<&RuleState> = rules
        .iter()
        .map(|r| &r.state)
        .filter(|s| **s != RuleState::Open)
        .collect();
    let alike = |states: &[&RuleState]| -> Option<String> {
        match states.first() {
            Some(RuleState::Decided(words))
                if states
                    .iter()
                    .all(|s| **s == RuleState::Decided(words.clone())) =>
            {
                Some(words.clone())
            }
            _ => None,
        }
    };
    let mut text = if open == 0 {
        match alike(&decided) {
            Some(words) if !words.is_empty() => words,
            Some(_) => String::new(),
            None => vocabulary::MIXED_SEE_RULES.to_string(),
        }
    } else if decided.is_empty() {
        vocabulary::OPEN.to_string()
    } else {
        let state = alike(&decided).unwrap_or_else(|| vocabulary::DECIDED.to_string());
        vocabulary::open_with_counts(decided.len(), rules.len(), &state, open)
    };
    for r in rules
        .iter()
        .filter(|r| r.state == RuleState::Open && !r.waiting.is_empty())
    {
        text.push_str(&format!(
            "\n{}: {}: {}",
            vocabulary::WAITING,
            r.title,
            r.waiting
        ));
    }
    text
}

/// The Fixes of `reviewed` with something to fix, folded from the report's rule groups (see
/// `review_tree::merge_fixes`); optionally keeping the Fixes with nothing left to fix (every place
/// ignored as accepted risk), which the PDF's patterns keep: an accepted risk is still a pattern the
/// report describes. The plan and the tickets do not. Unordered; [`in_plan_order`] orders.
pub(crate) fn fix_facts(
    reviewed: &ReviewedReport,
    rows: &[FindingRow],
    deps: &[DepRow],
    corpus: Option<&camerata_rules::RuleSet>,
    include_closed: bool,
) -> Vec<FixFacts> {
    let report = reviewed.report();
    let fixes = crate::review_tree::FixIndex::new(report.tree.as_ref(), &report.findings);
    let multi_repo = report.repos.len() > 1;
    let subs = subgroup_facts_with(reviewed, rows, deps, corpus, include_closed, &fixes);
    let mut by_fix: BTreeMap<String, Vec<SubgroupFacts>> = BTreeMap::new();
    for sub in subs {
        let id = if sub.fix_id.is_empty() {
            sub.id.clone()
        } else {
            sub.fix_id.clone()
        };
        by_fix.entry(id).or_default().push(sub);
    }
    by_fix
        .into_iter()
        .map(|(id, members)| {
            let node = fixes.fixes().iter().find(|n| n.id == id);
            fold_fix(&id, members, node, multi_repo)
        })
        .collect()
}

/// The Fixes of the plan, in plan order: the Fixes with something to fix that are fixes (see
/// [`super::sheets::fixes_with_problems`]). The Fix plan sheet and the ticket file are both this
/// list, so one ticket is one plan row.
pub(crate) fn plan_facts(
    reviewed: &ReviewedReport,
    rows: &[FindingRow],
    deps: &[DepRow],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<FixFacts> {
    let with_problems = super::sheets::fixes_with_problems(rows, deps);
    in_plan_order(
        fix_facts(reviewed, rows, deps, corpus, false)
            .into_iter()
            .filter(|g| with_problems.contains(&g.id))
            .collect(),
    )
}

/// `facts` in plan order. The order comes from [`crate::remediation_plan::plan`], fed with one
/// finding per place to fix and the Fix id as its rule, so a unit of the plan is exactly one Fix
/// (no repository split, no directory split, no shared-fix merge): the tickets and the
/// remediation units are the same list. The sheet then leads with what to fix first.
pub(crate) fn in_plan_order(facts: Vec<FixFacts>) -> Vec<FixFacts> {
    use crate::remediation_plan::{plan, PlanFinding, PlanOptions};
    let findings: Vec<PlanFinding> = facts
        .iter()
        .flat_map(|g| {
            g.open.iter().map(|s| PlanFinding {
                key: s.site_id.clone(),
                fingerprint: s.site_id.clone(),
                repo: String::new(),
                path: s.path.clone(),
                line: s.line,
                rule_id: g.id.clone(),
                severity: g.severity.to_ascii_lowercase(),
                snippet: String::new(),
                also_matches: Vec::new(),
            })
        })
        .collect();
    let options = PlanOptions {
        max_files_per_unit: usize::MAX,
        max_findings_per_unit: usize::MAX,
        group_shared_fix: false,
        ..PlanOptions::default()
    };
    let planned = plan(findings, &std::collections::HashMap::new(), &options);
    let mut by_id: BTreeMap<String, FixFacts> =
        facts.into_iter().map(|g| (g.id.clone(), g)).collect();
    let mut ordered: Vec<FixFacts> = planned
        .units
        .iter()
        .filter_map(|u| by_id.remove(&u.rule_ids[0]))
        .collect();
    // A Fix the plan did not carry (it cannot happen: every Fix here has a place) is kept rather
    // than lost.
    ordered.extend(by_id.into_values());
    // Fix first, then next, then later; the plan's order (worst severity, biggest) holds inside
    // each.
    ordered.sort_by_key(|g| g.sequence);
    ordered
}

/// What the PDF says of one Fix beyond the pattern: the rules it closes, its places and its effort
/// counted once. The top three reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdfFixFacts {
    /// The rule ids the Fix closes, worst severity first.
    pub rules: Vec<String>,
    pub places: usize,
    pub effort: String,
    pub severity: String,
}

/// The escalated places as the PDF prints them, one entry for each place whatever the number of
/// rules at it. `rows` are all rows of the export.
fn escalated_places(rows: &[FindingRow]) -> Vec<crate::report_export::EscalatedSiteJson> {
    let mut by_place: BTreeMap<(String, String, usize), Vec<&FindingRow>> = BTreeMap::new();
    for r in rows.iter().filter(|r| !r.is_fp) {
        by_place
            .entry((r.repo.clone(), r.path.clone(), r.line))
            .or_default()
            .push(r);
    }
    let mut out = Vec::new();
    for (_, at) in by_place {
        if !at.iter().any(|r| r.facts.escalated) {
            continue;
        }
        // The row that heads the place: the worst severity, then the order the rows came in.
        let Some(head) = at
            .iter()
            .copied()
            .min_by_key(|r| severity_rank(&r.severity))
        else {
            continue;
        };
        // The rules are named by id in the PDF: a rule's title is a sentence, and a place can have
        // six of them.
        let mut rules: Vec<String> = Vec::new();
        let mut decisions: Vec<String> = Vec::new();
        for r in &at {
            if !rules.contains(&r.rule_id) {
                rules.push(r.rule_id.clone());
            }
            // The place's decision, without the fix order a row recommends (that is the Fix's):
            // "Open", "Confirmed by the reviewer", "Needs a decision".
            let state = r
                .disposition_label
                .split(" (")
                .next()
                .unwrap_or_default()
                .to_string();
            if !decisions.contains(&state) {
                decisions.push(state);
            }
        }
        out.push(crate::report_export::EscalatedSiteJson {
            place: vocabulary::place_text(&head.repo, &head.path, head.line),
            rules_line: format!("{}: {}", vocabulary::H_RULES, rules.join(", ")),
            // The fix written for this place (its head row's own), else the rule's.
            what_to_do: crate::report_export::first_sentences(
                head.fix_specific
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .or(head.fix.as_deref())
                    .unwrap_or_default(),
                2,
                400,
            ),
            effort: String::new(),
            group: head.facts.group.clone(),
            fix_id: head.facts.fix_id.clone(),
            headline: head.headline.clone(),
            severity: title_case(&head.severity),
            repo: head.repo.clone(),
            path: head.path.clone(),
            line: head.line,
            rule_id: head.rule_id.clone(),
            rules,
            decision: decisions.join("; "),
            review_level: at
                .iter()
                .map(|r| r.facts.review_level.as_str())
                .max_by_key(|l| level_strength(l))
                .unwrap_or_default()
                .to_string(),
        });
    }
    // The worst first, so the PDF's cap keeps the most serious.
    out.sort_by_key(|e| severity_rank(&e.severity.to_ascii_lowercase()));
    out
}

/// What the PDF states from the reviewed report beyond its own row lists, worked out once from one
/// partition of the rows.
pub(crate) struct PdfSections {
    /// The escalated places with their decisions, worst first.
    pub escalated: Vec<crate::report_export::EscalatedSiteJson>,
    /// One pattern for each Fix.
    pub patterns: Vec<crate::report_export::PatternJson>,
    /// The facts of each Fix by id, for the top three.
    pub fixes: std::collections::HashMap<String, PdfFixFacts>,
    /// "Your conventions".
    pub conventions: crate::report_export::ConventionsSummaryJson,
    /// Problems by area and severity (critical, high, medium, low), one for each place: the
    /// scorecard and the cover, counted the way the workbook's scorecard counts.
    pub problem_counts: BTreeMap<String, [usize; 4]>,
    /// The same by severity alone, each place once: the cover.
    pub problem_totals: [usize; 4],
}

/// What the PDF prints from the reviewed report beyond the per-site sections (design 3B item 1):
/// the escalated places with their decisions, one pattern for each Fix, and the facts of each Fix
/// by id for the top three.
pub(crate) fn pdf_sections(
    reviewed: &ReviewedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_options: &std::collections::HashMap<String, String>,
) -> PdfSections {
    let (rows, deps) = super::partition_rows_for(reviewed, corpus, chosen_options, false);
    let mut escalated = escalated_places(&rows);
    let mut facts = fix_facts(reviewed, &rows, &deps, corpus, true);
    facts.sort_by_key(|g| (g.open.is_empty(), g.sequence));
    // What each escalated place's Fix asks of the reader, in two sentences, and its effort.
    for entry in &mut escalated {
        if let Some(g) = facts.iter().find(|g| g.id == entry.fix_id) {
            // Last resort: the Fix's own text.
            if entry.what_to_do.is_empty() {
                let text = if g.worked_example.is_empty() {
                    &g.fix
                } else {
                    &g.worked_example
                };
                entry.what_to_do = crate::report_export::first_sentences(text, 2, 400);
            }
            entry.effort = g.effort.clone();
        }
    }
    let conventions = super::pdf_conventions_of(&rows, corpus);
    let problem_counts = super::sheets::problem_counts(rows.iter());
    let problem_totals = super::sheets::problem_totals(rows.iter());
    let by_id = facts
        .iter()
        .map(|g| {
            (
                g.id.clone(),
                PdfFixFacts {
                    rules: g.rules.iter().map(|r| r.rule_id.clone()).collect(),
                    places: g.open_site_total,
                    effort: g.effort.clone(),
                    severity: g.severity.clone(),
                },
            )
        })
        .collect();
    // A Fix that holds only conventions, or only rules that could not be completed, is no fix: it
    // is not on the Fix plan sheet and has no line in the PDF.
    let with_problems = super::sheets::fixes_with_problems(&rows, &deps);
    let patterns = facts
        .into_iter()
        .filter(|g| with_problems.contains(&g.id))
        .map(|g| crate::report_export::PatternJson {
            pointer: format!(
                "{} {}, see the All problems sheet, filter Fix id = {}",
                g.live_sites,
                vocabulary::places_word(g.live_sites),
                g.id
            ),
            rules: g.rules.iter().map(|r| r.rule_id.clone()).collect(),
            area: g
                .category
                .split(", ")
                .next()
                .unwrap_or_default()
                .to_string(),
            name: if g.label.is_empty() {
                g.rule_title.clone()
            } else {
                g.label.clone()
            },
            group: g.id,
            rule_id: g.rule_id,
            rule_title: g.rule_title,
            label: g.label,
            cause: g.cause,
            summary: g.summary,
            severity: g.severity,
            site_count: g.live_sites,
            open_sites: g.open_site_total,
            ignored_sites: g.ignored,
            review_levels: g.review_levels,
            escalated: g.escalated,
            effort: g.effort,
            fix: g.fix,
            worked_example: g.worked_example,
            folders: g.folders,
            sequence: g.sequence.label().to_string(),
        })
        .collect();
    PdfSections {
        escalated,
        patterns,
        fixes: by_id,
        conventions,
        problem_counts,
        problem_totals,
    }
}
