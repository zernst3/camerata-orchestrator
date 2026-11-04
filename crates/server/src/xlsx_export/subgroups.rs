//! The sub-group as the unit of a fix (design D16): what the Remediation plan sheet, the ticket
//! CSV and the PDF's patterns each print about one violation class with one cause.
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

use super::{severity_rank, title_case, DepRow, FindingRow};
use crate::report_export::prose_checks::client_text;
use crate::report_export::{Disposition, ReviewedReport};
use crate::review_tree::{is_thrown_out_id, NodeLevel};

/// What the plan, the tickets and the PDF's patterns say for a report that has findings and no
/// review tree (one stored before the tree existed): there are no sub-groups to plan, and
/// printing nothing would read as "nothing to fix".
pub(crate) const NO_TREE_NOTE: &str =
    "This report has no review tree, so there are no sub-groups to plan. Every site is on the \
     Sites sheet.";

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
}

/// When a sub-group's fix is scheduled (design 3B item 2, from the severity x effort matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Sequence {
    /// Critical, high, or medium at low effort.
    FixFirst,
    /// The rest of medium.
    Next,
    /// Low and informational.
    Backlog,
}

impl Sequence {
    pub fn label(self) -> &'static str {
        match self {
            Self::FixFirst => "Fix first",
            Self::Next => "Next",
            Self::Backlog => "Backlog",
        }
    }
}

/// One sub-group with something to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubgroupFacts {
    /// The tree node id; the Sites sheet filter is `Group = <id>`.
    pub id: String,
    pub label: String,
    pub cause: String,
    pub summary: String,
    pub rule_id: String,
    pub rule_title: String,
    pub category: String,
    /// The worst severity among the sites still to fix, in title case.
    pub severity: String,
    /// The sites still to fix: not a false positive and not accepted risk. Sorted by place.
    pub open: Vec<SiteFact>,
    /// Sites the reviewer ignored as accepted risk.
    pub ignored: usize,
    /// Every site of the sub-group that is not a false positive (open and ignored). A row that
    /// still stands for several sites (an older report's merge) counts each.
    pub live_sites: usize,
    /// The sites still to fix, counted the same way.
    pub open_site_total: usize,
    /// Live sites by review level wire name.
    pub review_levels: BTreeMap<String, usize>,
    /// Live sites the group reviewer escalated.
    pub escalated: usize,
    /// The effort most of the open sites carry: `low`, `medium`, `high` or empty.
    pub effort: String,
    /// The fix, written once.
    pub fix: String,
    /// The primary row's own fix for its code, as the worked example.
    pub worked_example: String,
    /// The three folders holding most of the open sites, with their counts.
    pub folders: Vec<String>,
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
        "critical" | "high" => Sequence::FixFirst,
        "medium" if effort == "low" => Sequence::FixFirst,
        "medium" => Sequence::Next,
        _ => Sequence::Backlog,
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

/// The sub-groups of `reviewed` with something to fix, in plan order: fix first, then next, then
/// backlog; within one, the worst severity first, then the sub-group id.
pub(crate) fn subgroup_facts(
    reviewed: &ReviewedReport,
    rows: &[FindingRow],
    deps: &[DepRow],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<SubgroupFacts> {
    subgroup_facts_with(reviewed, rows, deps, corpus, false)
}

/// [`subgroup_facts`], optionally keeping the sub-groups with nothing left to fix (every live
/// site ignored as accepted risk). The PDF's patterns keep them: an accepted risk is still a
/// pattern the report describes. The plan and the tickets do not.
pub(crate) fn subgroup_facts_with(
    reviewed: &ReviewedReport,
    rows: &[FindingRow],
    deps: &[DepRow],
    corpus: Option<&camerata_rules::RuleSet>,
    include_closed: bool,
) -> Vec<SubgroupFacts> {
    let report = reviewed.report();
    let Some(tree) = report.tree.as_ref() else {
        return Vec::new();
    };
    let multi_repo = report.repos.len() > 1;
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
            "Sub-group".to_string()
        };

        if !dep_in_group.is_empty() {
            let rank = dep_in_group
                .iter()
                .map(|d| severity_rank(&d.severity))
                .min()
                .unwrap_or(3);
            let severity = severity_title(rank).to_string();
            let mut levels = BTreeMap::new();
            for d in &dep_in_group {
                *levels.entry(d.facts.review_level.clone()).or_default() += 1;
            }
            let mut sites: Vec<SiteFact> = dep_in_group
                .iter()
                .map(|d| SiteFact {
                    site_id: d.facts.site_id.clone(),
                    repo: d.repo.clone(),
                    path: d.path.clone(),
                    line: 0,
                    severity: title_case(&d.severity),
                    headline: format!("{}: {}", d.package, d.advisory),
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
                label,
                cause: cause.text,
                summary: summary.text,
                rule_id: crate::dep_audit::DEP_AUDIT_RULE_ID.to_string(),
                rule_title: "Known vulnerable dependency".to_string(),
                category: "Dependencies".to_string(),
                folders: top_folders(
                    sites.iter().map(|s| (s.repo.as_str(), s.path.as_str())),
                    multi_repo,
                ),
                sequence: sequence_of(&severity, ""),
                severity,
                live_sites: sites.len(),
                open_site_total: sites.len(),
                open: sites,
                ignored: 0,
                review_levels: levels,
                escalated: dep_in_group.iter().filter(|d| d.facts.escalated).count(),
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
        let mut levels = BTreeMap::new();
        for r in &live {
            *levels.entry(r.facts.review_level.clone()).or_default() += 1;
        }
        let mut sites: Vec<SiteFact> = open
            .iter()
            .map(|r| SiteFact {
                site_id: r.facts.site_id.clone(),
                repo: r.repo.clone(),
                path: r.path.clone(),
                line: r.line,
                severity: title_case(&r.severity),
                headline: r.headline.clone(),
            })
            .collect();
        sites.sort_by(|a, b| {
            (&a.repo, &a.path, a.line, &a.site_id).cmp(&(&b.repo, &b.path, b.line, &b.site_id))
        });
        out.push(SubgroupFacts {
            id: node.id.clone(),
            label,
            cause: cause.text,
            summary: summary.text,
            rule_id: primary.rule_id.clone(),
            rule_title: rule_title(&primary.rule_id, corpus),
            category: primary.category.clone(),
            folders: top_folders(
                basis.iter().map(|r| (r.repo.as_str(), r.path.as_str())),
                multi_repo,
            ),
            sequence: sequence_of(&severity, &effort),
            severity,
            ignored: live.len() - open.len(),
            live_sites: live.iter().map(|r| r.site_count).sum(),
            open_site_total: open.iter().map(|r| r.site_count).sum(),
            open: sites,
            review_levels: levels,
            escalated: live.iter().filter(|r| r.facts.escalated).count(),
            effort,
            fix: primary.fix.clone().unwrap_or_default(),
            worked_example: primary.fix_specific.clone().unwrap_or_default(),
            is_dependency: false,
        });
    }
    out.sort_by(|a, b| {
        (
            a.sequence,
            severity_rank(&a.severity.to_ascii_lowercase()),
            &a.id,
        )
            .cmp(&(
                b.sequence,
                severity_rank(&b.severity.to_ascii_lowercase()),
                &b.id,
            ))
    });
    out
}

fn rule_title(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> String {
    super::rule_title(rule_id, corpus)
}

/// `facts` in plan order. The order comes from [`crate::remediation_plan::plan`], fed with one
/// finding per site to fix and the sub-group id as its rule, so a unit of the plan is exactly one
/// sub-group (no repository split, no directory split, no shared-fix merge): the tickets and the
/// remediation units are the same list. The sheet then leads with what to fix first.
pub(crate) fn in_plan_order(facts: Vec<SubgroupFacts>) -> Vec<SubgroupFacts> {
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
    let mut by_id: BTreeMap<String, SubgroupFacts> =
        facts.into_iter().map(|g| (g.id.clone(), g)).collect();
    let mut ordered: Vec<SubgroupFacts> = planned
        .units
        .iter()
        .filter_map(|u| by_id.remove(&u.rule_ids[0]))
        .collect();
    // A sub-group the plan did not carry (it cannot happen: every sub-group here has a site) is
    // kept rather than lost.
    ordered.extend(by_id.into_values());
    // Fix first, then next, then backlog; the plan's order (worst severity, biggest) holds inside
    // each.
    ordered.sort_by_key(|g| g.sequence);
    ordered
}

/// What the PDF prints from the reviewed report beyond the per-site sections (design 3B item 1):
/// the escalated sites with their decisions, and one pattern for each sub-group.
pub(crate) fn pdf_sections(
    reviewed: &ReviewedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_options: &std::collections::HashMap<String, String>,
) -> (
    Vec<crate::report_export::EscalatedSiteJson>,
    Vec<crate::report_export::PatternJson>,
) {
    let (rows, deps) = super::partition_rows_for(reviewed, corpus, chosen_options, false);
    let escalated = rows
        .iter()
        .filter(|r| r.facts.escalated && !r.is_fp)
        .map(|r| crate::report_export::EscalatedSiteJson {
            group: r.facts.group.clone(),
            headline: r.headline.clone(),
            severity: title_case(&r.severity),
            repo: r.repo.clone(),
            path: r.path.clone(),
            line: r.line,
            rule_id: r.rule_id.clone(),
            decision: r.disposition_label.clone(),
            review_level: r.facts.review_level.clone(),
        })
        .collect();
    let mut facts = subgroup_facts_with(reviewed, &rows, &deps, corpus, true);
    facts.sort_by_key(|g| (g.open.is_empty(), g.sequence));
    let patterns = facts
        .into_iter()
        .map(|g| crate::report_export::PatternJson {
            pointer: format!(
                "{} {}, see Sites sheet, filter Group = {}",
                g.live_sites,
                if g.live_sites == 1 { "site" } else { "sites" },
                g.id
            ),
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
    (escalated, patterns)
}
