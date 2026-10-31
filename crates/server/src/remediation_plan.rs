//! The remediation planner (roadmap 2d, Part A): pure functions that turn a set of confirmed
//! findings into a plan of units of work, and render the traceable PR body for one unit.
//!
//! No git, no HTTP, no state. The caller (gap 6) builds [`PlanFinding`]s from the report and
//! the operator's triage, calls [`plan`], and creates one native unit per [`PlannedUnit`]. The
//! PR open and Ship paths call [`pr_body`] with the unit's `FindingsSource`.
//!
//! The planner works in six steps, each a private pure function:
//!
//! 1. partition by repository (a PR is per repository);
//! 2. collapse findings at one location under several rules into one group;
//! 3. one unit per rule;
//! 4. split a rule that is too big by directory (never one file);
//! 5. order by severity and link units that edit the same file;
//! 6. name each unit's branch and title.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use camerata_api_types::uow::{validate_branch_name, FindingsSource, SourceFinding};
use serde::{Deserialize, Serialize};

/// One eligible finding. Gap 6 builds these from the report's `Finding`s.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFinding {
    pub key: String,
    pub fingerprint: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    pub snippet: String,
    /// Other rules the same location also violates (the scanner folds those into one finding).
    #[serde(default)]
    pub also_matches: Vec<String>,
}

/// What the planner and the PR body need to know about a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleInfo {
    pub id: String,
    pub title: String,
    /// The outside standards behind the rule, as `(title, url)`.
    pub sources: Vec<(String, String)>,
}

fn default_max_files() -> usize {
    20
}
fn default_max_findings() -> usize {
    60
}
fn default_true() -> bool {
    true
}
fn default_prefix() -> String {
    "camerata/remediation".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanOptions {
    /// A unit whose findings touch more files than this is split by directory.
    #[serde(default = "default_max_files")]
    pub max_files_per_unit: usize,
    /// A unit with more findings than this is split by directory.
    #[serde(default = "default_max_findings")]
    pub max_findings_per_unit: usize,
    /// Merge findings at one location under several rules into one unit.
    #[serde(default = "default_true")]
    pub group_shared_fix: bool,
    /// Branch prefix; the rule slug (and area slug) are appended.
    #[serde(default = "default_prefix")]
    pub branch_prefix: String,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            max_files_per_unit: default_max_files(),
            max_findings_per_unit: default_max_findings(),
            group_shared_fix: default_true(),
            branch_prefix: default_prefix(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedUnit {
    pub repo: String,
    /// Primary rule first, the rest sorted.
    pub rule_ids: Vec<String>,
    /// Directory label when the rule was split.
    pub area: Option<String>,
    /// Sorted by path then line.
    pub findings: Vec<PlanFinding>,
    /// Findings a shared-fix group absorbed (they sit at the same location as one of `findings`
    /// under another rule). The unit closes them too, so the caller keeps them traceable.
    /// Sorted by path, then line, then key.
    #[serde(default)]
    pub merged: Vec<PlanFinding>,
    /// The worst finding's severity.
    pub severity: String,
    /// Indexes into [`RemediationPlan::units`], all smaller than this unit's own index.
    pub depends_on: Vec<usize>,
    /// Valid, and unique among the units of one repository.
    pub branch: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemediationPlan {
    pub units: Vec<PlannedUnit>,
    pub notes: Vec<String>,
}

/// Plan the remediation of `findings`. Pure and deterministic: the same input gives the same
/// units in the same order.
#[must_use]
pub fn plan(
    findings: Vec<PlanFinding>,
    rules: &HashMap<String, RuleInfo>,
    options: &PlanOptions,
) -> RemediationPlan {
    let mut notes = Vec::new();
    let findings = dedupe(findings, &mut notes);
    let mut units = Vec::new();
    for (repo, repo_findings) in partition_by_repo(findings) {
        let (repo_findings, mut absorbed) = if options.group_shared_fix {
            collapse_shared_fix(repo_findings, &mut notes)
        } else {
            (repo_findings, BTreeMap::new())
        };
        for (rule, group) in group_by_rule(repo_findings) {
            for mut unit in split_group(&repo, &rule, group, options, &mut notes) {
                unit.merged = take_absorbed(&unit.findings, &mut absorbed);
                units.push(unit);
            }
        }
        // Every absorbed finding sits at a location a kept finding also sits at, and that
        // finding is in exactly one unit, so nothing is left over.
        debug_assert!(absorbed.is_empty(), "absorbed findings left without a unit");
    }
    order_units(&mut units);
    link_dependencies(&mut units, &mut notes);
    name_units(&mut units, rules, options);
    RemediationPlan { units, notes }
}

/// Rank of a severity, lower is worse. The one ranking (`camerata_rules::severity_bounds`),
/// applied to the trimmed, lowercased text so `High`, ` high ` and `high` agree.
fn rank(severity: &str) -> u8 {
    camerata_rules::severity_bounds::severity_rank(&severity.trim().to_ascii_lowercase())
}

/// The same finding listed twice (same key, repository, path, line and rule) is planned once,
/// so it can never be counted, listed or closed twice. The kept copy has the worst severity and
/// the union of the `also_matches`, which makes the result independent of input order.
fn dedupe(findings: Vec<PlanFinding>, notes: &mut Vec<String>) -> Vec<PlanFinding> {
    let total = findings.len();
    let mut kept: BTreeMap<(String, String, String, usize, String), PlanFinding> = BTreeMap::new();
    for f in findings {
        let id = (
            f.key.clone(),
            f.repo.clone(),
            f.path.clone(),
            f.line,
            f.rule_id.clone(),
        );
        match kept.get_mut(&id) {
            None => {
                kept.insert(id, f);
            }
            Some(have) => {
                if rank(&f.severity) < rank(&have.severity) {
                    have.severity = f.severity;
                }
                have.also_matches.extend(f.also_matches);
                have.also_matches.sort();
                have.also_matches.dedup();
            }
        }
    }
    let dropped = total - kept.len();
    if dropped > 0 {
        notes.push(if dropped == 1 {
            "1 duplicate finding was planned once.".to_string()
        } else {
            format!("{dropped} duplicate findings were planned once.")
        });
    }
    kept.into_values().collect()
}

/// Step 1. A unit never spans repositories (a PR is per repository). Sorted by repo name so the
/// plan does not depend on input order.
fn partition_by_repo(findings: Vec<PlanFinding>) -> BTreeMap<String, Vec<PlanFinding>> {
    let mut out: BTreeMap<String, Vec<PlanFinding>> = BTreeMap::new();
    for f in findings {
        out.entry(f.repo.clone()).or_default().push(f);
    }
    out
}

/// Step 2. Findings at one `(path, line)` that violate several rules are one fix. The group
/// keeps the rule with the worst severity as its primary (ties: alphabetical id) and names every
/// rule involved, including rules the scanner folded into `also_matches`. The findings under
/// the other rules at that location are absorbed into it.
fn collapse_shared_fix(
    findings: Vec<PlanFinding>,
    notes: &mut Vec<String>,
) -> (Vec<PlanFinding>, Absorbed) {
    let mut absorbed: Absorbed = BTreeMap::new();
    let mut by_location: BTreeMap<(String, usize), Vec<PlanFinding>> = BTreeMap::new();
    for f in findings {
        by_location
            .entry((f.path.clone(), f.line))
            .or_default()
            .push(f);
    }
    let mut out = Vec::new();
    for ((path, line), group) in by_location {
        let mut involved: BTreeSet<String> = BTreeSet::new();
        for f in &group {
            involved.insert(f.rule_id.clone());
            involved.extend(f.also_matches.iter().filter(|m| !m.is_empty()).cloned());
        }
        if involved.len() < 2 {
            out.extend(group);
            continue;
        }
        let primary = group
            .iter()
            .min_by(|a, b| {
                (rank(&a.severity), &a.rule_id, &a.key).cmp(&(
                    rank(&b.severity),
                    &b.rule_id,
                    &b.key,
                ))
            })
            .map(|f| f.rule_id.clone())
            .unwrap_or_default();
        let others: Vec<String> = involved
            .iter()
            .filter(|r| **r != primary)
            .cloned()
            .collect();
        for mut f in group {
            if f.rule_id == primary {
                f.also_matches = others.clone();
                out.push(f);
            } else {
                absorbed.entry((path.clone(), line)).or_default().push(f);
            }
        }
        notes.push(format!(
            "{path}:{line} violates {} rules; one unit covers them.",
            involved.len()
        ));
    }
    (out, absorbed)
}

/// The findings step 2 absorbed, by `(path, line)` (one repository at a time).
type Absorbed = BTreeMap<(String, usize), Vec<PlanFinding>>;

/// The absorbed findings that belong with `kept`: those at a location a kept finding sits at.
/// Taking them out of the map means each lands on exactly one unit.
fn take_absorbed(kept: &[PlanFinding], absorbed: &mut Absorbed) -> Vec<PlanFinding> {
    let mut merged = Vec::new();
    for f in kept {
        if let Some(found) = absorbed.remove(&(f.path.clone(), f.line)) {
            merged.extend(found);
        }
    }
    merged.sort_by(|a, b| (&a.path, a.line, &a.key).cmp(&(&b.path, b.line, &b.key)));
    merged
}

/// Step 3. One group per primary rule (Zach's default: one unit of work per violated rule).
fn group_by_rule(findings: Vec<PlanFinding>) -> BTreeMap<String, Vec<PlanFinding>> {
    let mut out: BTreeMap<String, Vec<PlanFinding>> = BTreeMap::new();
    for f in findings {
        out.entry(f.rule_id.clone()).or_default().push(f);
    }
    out
}

fn distinct_paths(findings: &[PlanFinding]) -> usize {
    findings
        .iter()
        .map(|f| f.path.as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn too_big(findings: &[PlanFinding], options: &PlanOptions) -> bool {
    distinct_paths(findings) > options.max_files_per_unit
        || findings.len() > options.max_findings_per_unit
}

/// The directory label of `path` at `depth` segments (`src/a/b.rs` is `src` at depth 1 and
/// `src/a` at depth 2). A file with no directory is `root`.
fn directory_label(path: &str, depth: usize) -> String {
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    let dirs = &parts[..parts.len().saturating_sub(1)];
    if dirs.is_empty() {
        "root".to_string()
    } else {
        dirs[..depth.min(dirs.len())].join("/")
    }
}

/// Split by the first `depth` directory segments; a part that is still too big and spans more
/// than one file is split again one level deeper (two levels at most). A part that cannot be
/// divided any further keeps the coarser label.
fn split_by_directory(
    findings: Vec<PlanFinding>,
    depth: usize,
    options: &PlanOptions,
) -> Vec<(String, Vec<PlanFinding>)> {
    let mut parts: BTreeMap<String, Vec<PlanFinding>> = BTreeMap::new();
    for f in findings {
        parts
            .entry(directory_label(&f.path, depth))
            .or_default()
            .push(f);
    }
    let mut out = Vec::new();
    for (label, part) in parts {
        if depth < 2 && too_big(&part, options) && distinct_paths(&part) > 1 {
            let deeper = split_by_directory(part.clone(), depth + 1, options);
            if deeper.len() > 1 {
                out.extend(deeper);
                continue;
            }
        }
        out.push((label, part));
    }
    out
}

/// Step 4. A rule whose findings touch too many files or number too many is split by
/// directory. A single file is never split. Returns the units for one `(repo, rule)`.
fn split_group(
    repo: &str,
    rule: &str,
    findings: Vec<PlanFinding>,
    options: &PlanOptions,
    notes: &mut Vec<String>,
) -> Vec<PlannedUnit> {
    if !too_big(&findings, options) {
        return vec![build_unit(
            repo,
            rule,
            None,
            findings,
            options.group_shared_fix,
        )];
    }
    let total = findings.len();
    let files = distinct_paths(&findings);
    let first_path = findings.first().map(|f| f.path.clone()).unwrap_or_default();
    let parts = split_by_directory(findings.clone(), 1, options);
    if parts.len() < 2 {
        notes.push(if files == 1 {
            format!(
                "{rule} has {total} findings in one file ({first_path}); a single file is never split."
            )
        } else {
            format!(
                "{rule} has {total} findings in {files} files that share one directory; kept as one unit."
            )
        });
        return vec![build_unit(
            repo,
            rule,
            None,
            findings,
            options.group_shared_fix,
        )];
    }
    notes.push(format!(
        "{rule} has {total} findings in {files} files; split into {} units by directory.",
        parts.len()
    ));
    for (label, part) in &parts {
        if too_big(part, options) {
            let n = part.len();
            let f = distinct_paths(part);
            let noun = if f == 1 { "file" } else { "files" };
            notes.push(format!(
                "{rule} in {label} still has {n} findings in {f} {noun} after the split; it stays one unit."
            ));
        }
    }
    parts
        .into_iter()
        .map(|(label, part)| build_unit(repo, rule, Some(label), part, options.group_shared_fix))
        .collect()
}

fn build_unit(
    repo: &str,
    primary: &str,
    area: Option<String>,
    mut findings: Vec<PlanFinding>,
    merge_rules: bool,
) -> PlannedUnit {
    findings.sort_by(|a, b| (&a.path, a.line, &a.key).cmp(&(&b.path, b.line, &b.key)));
    let mut rule_ids = vec![primary.to_string()];
    // With shared-fix grouping off, a unit answers for its own rule only.
    let rest: BTreeSet<&String> = findings
        .iter()
        .filter(|_| merge_rules)
        .flat_map(|f| f.also_matches.iter())
        .filter(|m| m.as_str() != primary && !m.is_empty())
        .collect();
    rule_ids.extend(rest.into_iter().cloned());
    let severity = findings
        .iter()
        .min_by_key(|f| rank(&f.severity))
        .map(|f| f.severity.clone())
        .unwrap_or_default();
    PlannedUnit {
        repo: repo.to_string(),
        rule_ids,
        area,
        findings,
        severity,
        merged: Vec::new(),
        depends_on: Vec::new(),
        branch: String::new(),
        title: String::new(),
    }
}

/// Step 5a. Worst severity first, then the bigger unit, then rule id (repo and area last so the
/// order never depends on input order).
fn order_units(units: &mut [PlannedUnit]) {
    units.sort_by(|a, b| {
        (
            rank(&a.severity),
            std::cmp::Reverse(a.findings.len()),
            &a.rule_ids[0],
            &a.repo,
            &a.area,
        )
            .cmp(&(
                rank(&b.severity),
                std::cmp::Reverse(b.findings.len()),
                &b.rule_ids[0],
                &b.repo,
                &b.area,
            ))
    });
}

/// Step 5b. Two units of one repository that edit a common file must land in order: the later
/// one in the sorted order depends on each earlier one it shares a file with. Direct edges only.
fn link_dependencies(units: &mut [PlannedUnit], notes: &mut Vec<String>) {
    let files: Vec<HashSet<String>> = units
        .iter()
        .map(|u| u.findings.iter().map(|f| f.path.clone()).collect())
        .collect();
    for later in 0..units.len() {
        let mut edges = Vec::new();
        for earlier in 0..later {
            if units[earlier].repo == units[later].repo
                && !files[earlier].is_disjoint(&files[later])
            {
                edges.push(earlier);
                notes.push(format!(
                    "uow for {b} edits files also edited by {a}; land {a} first.",
                    a = units[earlier].rule_ids[0],
                    b = units[later].rule_ids[0]
                ));
            }
        }
        units[later].depends_on = edges;
    }
}

/// Step 6. A valid branch per unit (unique within its repository) and a plain title.
fn name_units(units: &mut [PlannedUnit], rules: &HashMap<String, RuleInfo>, options: &PlanOptions) {
    let mut used: HashMap<String, HashSet<String>> = HashMap::new();
    for (index, unit) in units.iter_mut().enumerate() {
        let taken = used.entry(unit.repo.clone()).or_default();
        let slug = rule_slug(&unit.rule_ids[0]);
        let mut base = format!("{}/{slug}", options.branch_prefix);
        if let Some(area) = unit.area.as_deref() {
            let area = area_slug(area);
            if !area.is_empty() {
                base = format!("{base}-{area}");
            }
        }
        let mut branch = unique_name(&base, taken);
        if validate_branch_name(&branch).is_err() {
            let fallback = format!("{}/rule-{}", options.branch_prefix, index + 1);
            branch = unique_name(&fallback, taken);
        }
        if validate_branch_name(&branch).is_err() {
            let fallback = format!("{}/rule-{}", default_prefix(), index + 1);
            branch = unique_name(&fallback, taken);
        }
        taken.insert(branch.clone());
        unit.branch = branch;

        let id = &unit.rule_ids[0];
        let rule_title = rules.get(id).map_or(id.as_str(), |r| r.title.as_str());
        let n = unit.findings.len();
        let noun = if n == 1 { "finding" } else { "findings" };
        let area = unit
            .area
            .as_deref()
            .map(|a| format!(", {a}"))
            .unwrap_or_default();
        unit.title = format!("Fix {id}: {rule_title} ({n} {noun} in {}{area})", unit.repo);
    }
}

/// `base`, or `base-2`, `base-3` and so on, whichever is first not taken.
fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    let mut n: usize = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Lowercase, `[a-z0-9-]` only, runs collapsed, no leading or trailing `-`.
#[must_use]
pub fn rule_slug(rule_id: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in rule_id.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// The slug of a directory label (`crates/server` becomes `crates-server`).
#[must_use]
pub fn area_slug(area: &str) -> String {
    rule_slug(area)
}

/// What only the caller knows about the unit, so the body can name it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrContext {
    /// The unit's story id, for the closing line.
    pub story_id: Option<String>,
    /// The units this one should land after, as `(uow-N, title)`, resolved by the caller.
    pub depends_on: Vec<(String, String)>,
}

/// The traceable PR body with no caller context.
#[must_use]
pub fn pr_body(source: &FindingsSource, rules: &HashMap<String, RuleInfo>) -> String {
    pr_body_with(source, rules, &PrContext::default())
}

/// The traceable PR body, with the unit's story id and resolved dependencies.
#[must_use]
pub fn pr_body_with(
    source: &FindingsSource,
    rules: &HashMap<String, RuleInfo>,
    context: &PrContext,
) -> String {
    let mut out = String::from("## What this PR fixes\n");
    let rule_ids: Vec<String> = if source.rule_ids.is_empty() {
        let mut seen = Vec::new();
        for f in &source.findings {
            if !seen.contains(&f.rule_id) {
                seen.push(f.rule_id.clone());
            }
        }
        seen
    } else {
        source.rule_ids.clone()
    };
    for id in &rule_ids {
        let (title, standards) = match rules.get(id) {
            Some(r) => (r.title.as_str(), r.sources.as_slice()),
            None => (id.as_str(), [].as_slice()),
        };
        let standard = if standards.is_empty() {
            "no outside source listed".to_string()
        } else {
            standards
                .iter()
                .map(|(t, u)| format!("{} ({})", table_cell(t), table_cell(u)))
                .collect::<Vec<_>>()
                .join("; ")
        };
        out.push_str(&format!(
            "Rule {id}: {}. Standard: {standard}.\n",
            table_cell(title).trim_end_matches('.')
        ));
    }

    out.push_str("\n## Findings closed\n");
    let mut repos: Vec<&str> = Vec::new();
    for f in &source.findings {
        if !repos.contains(&f.repo.as_str()) {
            repos.push(f.repo.as_str());
        }
    }
    if repos.is_empty() {
        out.push_str(&format!("Inspection {}.\n", source.inspection_id));
    }
    let mut number = 0usize;
    for (i, repo) in repos.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let sha = source
            .audited
            .iter()
            .find(|a| a.repo == *repo)
            .and_then(|a| a.sha.as_deref())
            .filter(|s| !s.is_empty());
        match sha {
            Some(sha) => {
                out.push_str(&format!(
                    "Inspection {}; {repo} at {sha}.\n",
                    source.inspection_id
                ));
            }
            None => out.push_str(&format!(
                "Inspection {}; {repo} (commit not recorded).\n",
                source.inspection_id
            )),
        }
        // Markdown only starts a table after a blank line.
        out.push('\n');
        out.push_str("| # | File | Line | Rule | Severity | Finding |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- |\n");
        // A finding a shared-fix group absorbed (`merged_into`) is listed straight under the
        // finding that stands for the group; one whose stand-in is not in this PR gets a row
        // after the others, so every finding the PR closes has a row.
        let in_repo: Vec<&SourceFinding> =
            source.findings.iter().filter(|f| f.repo == *repo).collect();
        let stands_for = |key: &str| {
            in_repo
                .iter()
                .any(|f| f.merged_into.is_none() && f.key == key)
        };
        for f in in_repo.iter().filter(|f| f.merged_into.is_none()) {
            number += 1;
            push_row(&mut out, number, f, None);
            for m in in_repo
                .iter()
                .filter(|m| m.merged_into.as_deref() == Some(f.key.as_str()))
            {
                number += 1;
                push_row(&mut out, number, m, Some("merged into the row above"));
            }
        }
        for m in in_repo
            .iter()
            .filter(|m| m.merged_into.as_deref().is_some_and(|k| !stands_for(k)))
        {
            number += 1;
            push_row(
                &mut out,
                number,
                m,
                Some("merged into another finding of this PR"),
            );
        }
    }

    let n = source.findings.len();
    let noun = if n == 1 { "finding" } else { "findings" };
    out.push_str(&format!(
        "\n{n} {noun}. A finding counts as closed when the next delta retest no longer reports it.\n"
    ));
    if !context.depends_on.is_empty() {
        let named: Vec<String> = context
            .depends_on
            .iter()
            .map(|(label, title)| format!("{} ({})", table_cell(label), table_cell(title)))
            .collect();
        out.push_str(&format!("\nDepends on: {}\n", named.join(", ")));
    } else if !source.depends_on.is_empty() {
        let ids: Vec<String> = source.depends_on.iter().map(|d| table_cell(d)).collect();
        out.push_str(&format!("\nDepends on: {}\n", ids.join(", ")));
    }
    if let Some(story) = context.story_id.as_deref() {
        out.push_str(&format!(
            "\nOpened by Camerata for unit {}.\n",
            table_cell(story)
        ));
    }
    out.truncate(out.trim_end().len());
    out
}

/// One table row. The note, when there is one, follows the fingerprint in the last cell.
fn push_row(out: &mut String, number: usize, f: &SourceFinding, note: Option<&str>) {
    let finding = match note {
        Some(note) => format!("{} ({note})", table_cell(&f.fingerprint)),
        None => table_cell(&f.fingerprint),
    };
    out.push_str(&format!(
        "| {number} | {} | {} | {} | {} | {finding} |\n",
        table_cell(&f.path),
        f.line,
        table_cell(&f.rule_id),
        table_cell(&f.severity),
    ));
}

/// One Markdown table cell: a pipe or a line break in the text cannot break the row.
fn table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::fingerprint::fingerprint;
    use camerata_api_types::uow::AuditedCommit;

    fn pf(repo: &str, path: &str, line: usize, rule: &str, sev: &str) -> PlanFinding {
        let snippet = format!("let secret_{rule}_{line} = \"hunter2\";");
        PlanFinding {
            key: format!("{repo}\0{rule}\0{path}\0{line}\0{snippet}"),
            fingerprint: fingerprint(rule, &snippet),
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: rule.to_string(),
            severity: sev.to_string(),
            snippet,
            also_matches: Vec::new(),
        }
    }

    fn info(id: &str, title: &str, sources: &[(&str, &str)]) -> RuleInfo {
        RuleInfo {
            id: id.to_string(),
            title: title.to_string(),
            sources: sources
                .iter()
                .map(|(t, u)| ((*t).to_string(), (*u).to_string()))
                .collect(),
        }
    }

    fn rules(list: Vec<RuleInfo>) -> HashMap<String, RuleInfo> {
        list.into_iter().map(|r| (r.id.clone(), r)).collect()
    }

    fn primary(u: &PlannedUnit) -> &str {
        u.rule_ids[0].as_str()
    }

    #[test]
    fn one_unit_per_rule_per_repo_by_default() {
        let findings = vec![
            pf("o/a", "src/one.rs", 1, "R1", "high"),
            pf("o/a", "src/two.rs", 2, "R1", "high"),
            pf("o/a", "src/three.rs", 3, "R1", "high"),
            pf("o/a", "src/four.rs", 9, "R2", "medium"),
            pf("o/b", "lib/x.rs", 4, "R1", "high"),
        ];
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        assert_eq!(p.units.len(), 3, "R1 in a, R2 in a, R1 in b");
        for u in &p.units {
            assert!(u.area.is_none(), "nothing is big enough to split");
            assert!(
                u.findings.iter().all(|f| f.repo == u.repo),
                "a unit never spans repos"
            );
            assert!(u.findings.iter().all(|f| f.rule_id == primary(u)));
        }
        let r1_a = p
            .units
            .iter()
            .find(|u| u.repo == "o/a" && primary(u) == "R1")
            .expect("R1 unit in o/a");
        assert_eq!(r1_a.findings.len(), 3);
        assert!(p
            .units
            .iter()
            .any(|u| u.repo == "o/b" && primary(u) == "R1"));
        assert!(
            p.notes.is_empty(),
            "a plain plan has nothing to explain: {:?}",
            p.notes
        );
    }

    #[test]
    fn a_location_violating_three_rules_becomes_one_unit_naming_all_three_with_the_worst_rule_primary(
    ) {
        // The scanner kept one finding for the location (rule B) and folded rule C into its
        // `also_matches`; a second finding at the same line sits under rule A, the worst.
        let mut b = pf("o/a", "src/auth.rs", 10, "B-RULE", "medium");
        b.also_matches = vec!["C-RULE".to_string()];
        let a = pf("o/a", "src/auth.rs", 10, "A-RULE", "critical");
        let other = pf("o/a", "src/other.rs", 3, "B-RULE", "medium");
        let p = plan(vec![b, a, other], &HashMap::new(), &PlanOptions::default());

        let grouped = p
            .units
            .iter()
            .find(|u| u.findings.iter().any(|f| f.path == "src/auth.rs"))
            .expect("a unit for the shared location");
        assert_eq!(grouped.rule_ids, vec!["A-RULE", "B-RULE", "C-RULE"]);
        assert_eq!(grouped.severity, "critical");
        assert_eq!(
            grouped.findings.len(),
            1,
            "one group at the location, not two findings"
        );
        assert_eq!(grouped.findings[0].rule_id, "A-RULE");
        assert_eq!(grouped.findings[0].also_matches, vec!["B-RULE", "C-RULE"]);
        assert!(
            p.notes
                .iter()
                .any(|n| n == "src/auth.rs:10 violates 3 rules; one unit covers them."),
            "{:?}",
            p.notes
        );
        // The unrelated B-RULE finding keeps its own unit; nothing is filed under C-RULE alone.
        assert_eq!(p.units.len(), 2);
        assert!(p
            .units
            .iter()
            .any(|u| primary(u) == "B-RULE" && u.findings.len() == 1));

        // With the option off the same input is one unit per rule, nothing merged.
        let mut b = pf("o/a", "src/auth.rs", 10, "B-RULE", "medium");
        b.also_matches = vec!["C-RULE".to_string()];
        let a = pf("o/a", "src/auth.rs", 10, "A-RULE", "critical");
        let off = PlanOptions {
            group_shared_fix: false,
            ..PlanOptions::default()
        };
        let p = plan(vec![b, a], &HashMap::new(), &off);
        assert_eq!(p.units.len(), 2);
        assert!(p.units.iter().all(|u| u.rule_ids.len() == 1));
    }

    #[test]
    fn a_rule_with_more_files_than_the_limit_is_split_by_top_level_directory_and_never_splits_one_file(
    ) {
        let mut findings = Vec::new();
        for i in 0..10 {
            findings.push(pf("o/a", &format!("src/m{i}.rs"), 1, "BIG", "high"));
        }
        for i in 0..8 {
            findings.push(pf("o/a", &format!("tests/t{i}.rs"), 1, "BIG", "high"));
        }
        for i in 0..7 {
            findings.push(pf("o/a", &format!("migrations/{i}.sql"), 1, "BIG", "high"));
        }
        // A different rule: 70 findings in one file. Too many findings, but one file.
        for i in 0..70 {
            findings.push(pf("o/a", "src/huge.rs", i + 1, "ONEFILE", "high"));
        }
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());

        let big: Vec<&PlannedUnit> = p.units.iter().filter(|u| primary(u) == "BIG").collect();
        assert_eq!(big.len(), 3);
        let mut areas: Vec<&str> = big.iter().filter_map(|u| u.area.as_deref()).collect();
        areas.sort_unstable();
        assert_eq!(areas, vec!["migrations", "src", "tests"]);
        assert_eq!(big.iter().map(|u| u.findings.len()).sum::<usize>(), 25);
        for u in &big {
            let dir = u.area.as_deref().expect("split units carry an area");
            assert!(u
                .findings
                .iter()
                .all(|f| f.path.starts_with(&format!("{dir}/"))));
        }
        assert!(
            p.notes
                .iter()
                .any(|n| n == "BIG has 25 findings in 25 files; split into 3 units by directory."),
            "{:?}",
            p.notes
        );

        let one: Vec<&PlannedUnit> = p.units.iter().filter(|u| primary(u) == "ONEFILE").collect();
        assert_eq!(one.len(), 1, "a single file is never split");
        assert_eq!(one[0].findings.len(), 70);
        assert!(one[0].area.is_none());
        assert!(
            p.notes
                .iter()
                .any(|n| n.contains("ONEFILE") && n.contains("one file")),
            "{:?}",
            p.notes
        );
    }

    #[test]
    fn units_are_ordered_critical_first_then_by_count_then_by_rule_id() {
        let mut findings = vec![pf("o/a", "z/only.rs", 1, "ZED", "critical")];
        for i in 0..5 {
            findings.push(pf("o/a", &format!("b/f{i}.rs"), 1, "BETA", "high"));
            findings.push(pf("o/a", &format!("a/f{i}.rs"), 1, "ALPHA", "high"));
        }
        findings.push(pf("o/a", "c/one.rs", 1, "GAMMA", "high"));
        findings.push(pf("o/a", "c/two.rs", 1, "GAMMA", "high"));
        for i in 0..9 {
            findings.push(pf("o/a", &format!("d/f{i}.rs"), 1, "DELTA", "medium"));
        }
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        let order: Vec<&str> = p.units.iter().map(primary).collect();
        assert_eq!(order, vec!["ZED", "ALPHA", "BETA", "GAMMA", "DELTA"]);
    }

    #[test]
    fn a_unit_editing_a_file_another_unit_edits_depends_on_the_earlier_one() {
        let findings = vec![
            pf("o/a", "src/shared.rs", 1, "FIRST", "critical"),
            pf("o/a", "src/shared.rs", 20, "SECOND", "high"),
            pf("o/a", "src/own.rs", 2, "SECOND", "high"),
            pf("o/a", "src/lonely.rs", 3, "THIRD", "medium"),
            // The same path in another repository is another file.
            pf("o/b", "src/shared.rs", 5, "FOURTH", "medium"),
        ];
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        let idx = |rule: &str| {
            p.units
                .iter()
                .position(|u| primary(u) == rule)
                .expect("unit")
        };
        assert!(p.units[idx("FIRST")].depends_on.is_empty());
        assert_eq!(p.units[idx("SECOND")].depends_on, vec![idx("FIRST")]);
        assert!(idx("FIRST") < idx("SECOND"));
        assert!(p.units[idx("THIRD")].depends_on.is_empty());
        assert!(
            p.units[idx("FOURTH")].depends_on.is_empty(),
            "other repo, other file"
        );
        for (i, u) in p.units.iter().enumerate() {
            assert!(
                u.depends_on.iter().all(|d| *d < i),
                "dependencies point backwards"
            );
        }
        assert!(
            p.notes
                .iter()
                .any(|n| n == "uow for SECOND edits files also edited by FIRST; land FIRST first."),
            "{:?}",
            p.notes
        );
    }

    #[test]
    fn branch_names_are_valid_unique_and_carry_the_area_suffix() {
        let mut findings = vec![
            pf("o/a", "src/a.rs", 1, "SEC_Auth.Token", "high"),
            // Two ids that slug to the same name must still get distinct branches.
            pf("o/a", "lib/b.rs", 1, "SEC_X", "high"),
            pf("o/a", "lib/c.rs", 1, "SEC-X", "high"),
            // An id with nothing usable in it falls back to a numbered name.
            pf("o/a", "lib/d.rs", 1, "!!!", "high"),
        ];
        for i in 0..21 {
            findings.push(pf("o/a", &format!("src/s{i}.rs"), 1, "SPLITME", "medium"));
            findings.push(pf("o/a", &format!("tests/t{i}.rs"), 1, "SPLITME", "medium"));
        }
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());

        let mut seen = HashSet::new();
        for u in &p.units {
            validate_branch_name(&u.branch)
                .unwrap_or_else(|e| panic!("`{}` must be valid: {e}", u.branch));
            assert!(
                u.branch.starts_with("camerata/remediation/"),
                "{}",
                u.branch
            );
            assert!(
                seen.insert((u.repo.clone(), u.branch.clone())),
                "branch `{}` repeats within {}",
                u.branch,
                u.repo
            );
        }
        let branch_of = |rule: &str| {
            p.units
                .iter()
                .find(|u| primary(u) == rule)
                .map(|u| u.branch.clone())
                .expect("unit")
        };
        assert_eq!(
            branch_of("SEC_Auth.Token"),
            "camerata/remediation/sec-auth-token"
        );
        let mut sec_x = [branch_of("SEC_X"), branch_of("SEC-X")];
        sec_x.sort();
        assert_eq!(
            sec_x,
            [
                "camerata/remediation/sec-x".to_string(),
                "camerata/remediation/sec-x-2".to_string()
            ]
        );
        assert!(branch_of("!!!").starts_with("camerata/remediation/rule-"));
        let split_branches: Vec<String> = p
            .units
            .iter()
            .filter(|u| primary(u) == "SPLITME")
            .map(|u| u.branch.clone())
            .collect();
        assert_eq!(split_branches.len(), 2);
        assert!(split_branches.contains(&"camerata/remediation/splitme-src".to_string()));
        assert!(split_branches.contains(&"camerata/remediation/splitme-tests".to_string()));

        // The same rule in two repositories shares a name: the branch is per repository.
        let two = plan(
            vec![
                pf("o/a", "x.rs", 1, "R", "high"),
                pf("o/b", "x.rs", 1, "R", "high"),
            ],
            &HashMap::new(),
            &PlanOptions::default(),
        );
        assert_eq!(two.units[0].branch, two.units[1].branch);
    }

    #[test]
    fn plan_options_default_from_an_empty_json_object() {
        let o: PlanOptions = serde_json::from_str("{}").expect("empty object");
        assert_eq!(o.max_files_per_unit, 20);
        assert_eq!(o.max_findings_per_unit, 60);
        assert!(o.group_shared_fix);
        assert_eq!(o.branch_prefix, "camerata/remediation");
        let d = PlanOptions::default();
        assert_eq!(
            (
                d.max_files_per_unit,
                d.max_findings_per_unit,
                d.group_shared_fix,
                d.branch_prefix
            ),
            (
                o.max_files_per_unit,
                o.max_findings_per_unit,
                o.group_shared_fix,
                o.branch_prefix
            )
        );
        let partial: PlanOptions =
            serde_json::from_str(r#"{"max_files_per_unit": 3}"#).expect("partial");
        assert_eq!(partial.max_files_per_unit, 3);
        assert_eq!(partial.max_findings_per_unit, 60);
    }

    fn sf(path: &str, line: usize, rule: &str, sev: &str) -> SourceFinding {
        let snippet = format!("password = \"hunter2-{line}\"");
        SourceFinding {
            key: format!("o/a\0{rule}\0{path}\0{line}\0{snippet}"),
            fingerprint: fingerprint(rule, &snippet),
            repo: "o/a".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule.to_string(),
            severity: sev.to_string(),
            snippet,
            merged_into: None,
        }
    }

    fn source() -> FindingsSource {
        FindingsSource {
            inspection_id: "insp-proj-1700000000-ab12cd".to_string(),
            audited: vec![
                AuditedCommit {
                    repo: "o/a".to_string(),
                    sha: Some("0123abcd".to_string()),
                },
                AuditedCommit {
                    repo: "o/other".to_string(),
                    sha: Some("ffff0000".to_string()),
                },
            ],
            rule_ids: vec!["SEC-1".to_string(), "SEC-2".to_string()],
            area: None,
            findings: vec![
                sf("src/auth.rs", 12, "SEC-1", "critical"),
                sf("src/db.rs", 40, "SEC-2", "high"),
            ],
            depends_on: Vec::new(),
            rules_fingerprint: String::new(),
        }
    }

    fn body_rules() -> HashMap<String, RuleInfo> {
        rules(vec![
            info(
                "SEC-1",
                "No hard-coded secrets",
                &[("OWASP ASVS 2.10.4", "https://owasp.org/asvs")],
            ),
            info("SEC-2", "Parameterise queries", &[]),
        ])
    }

    #[test]
    fn pr_body_lists_every_finding_with_file_line_rule_severity_fingerprint_the_inspection_id_and_commit_and_the_standard(
    ) {
        let src = source();
        let body = pr_body(&src, &body_rules());
        let expected_head = "## What this PR fixes\n\
Rule SEC-1: No hard-coded secrets. Standard: OWASP ASVS 2.10.4 (https://owasp.org/asvs).\n\
Rule SEC-2: Parameterise queries. Standard: no outside source listed.\n\
\n\
## Findings closed\n\
Inspection insp-proj-1700000000-ab12cd; o/a at 0123abcd.\n\
\n\
| # | File | Line | Rule | Severity | Finding |\n\
| --- | --- | --- | --- | --- | --- |\n";
        assert!(body.starts_with(expected_head), "{body}");
        for (n, f) in src.findings.iter().enumerate() {
            let row = format!(
                "| {} | {} | {} | {} | {} | {} |",
                n + 1,
                f.path,
                f.line,
                f.rule_id,
                f.severity,
                f.fingerprint
            );
            assert!(body.contains(&row), "missing row `{row}` in\n{body}");
        }
        assert!(
            body.contains(
                "\n2 findings. A finding counts as closed when the next delta retest no longer reports it."
            ),
            "{body}"
        );
        assert!(
            !body.contains("ffff0000"),
            "only the unit's own repo commit is named"
        );
        assert!(!body.contains("Depends on"), "no dependency, no line");
        assert!(
            !body.contains("Opened by"),
            "no story id known to the bare renderer"
        );

        // One finding reads in the singular.
        let mut one = source();
        one.findings.truncate(1);
        assert!(pr_body(&one, &body_rules()).contains("\n1 finding. A finding counts"));

        // The caller's context adds the two lines only it can know.
        let ctx = PrContext {
            story_id: Some("story-77".to_string()),
            depends_on: vec![("uow-12".to_string(), "Fix SEC-9".to_string())],
        };
        let with = pr_body_with(&src, &body_rules(), &ctx);
        assert!(
            with.contains("\nDepends on: uow-12 (Fix SEC-9)\n"),
            "{with}"
        );
        assert!(
            with.ends_with("Opened by Camerata for unit story-77."),
            "{with}"
        );

        // Dependencies the caller could not resolve are still named, by story id.
        let mut dep = source();
        dep.depends_on = vec!["story-5".to_string()];
        assert!(pr_body(&dep, &body_rules()).contains("\nDepends on: story-5"));

        // Unknown rules fall back to the id; an empty unit still renders both headings.
        let bare = FindingsSource {
            inspection_id: "insp-x".to_string(),
            ..Default::default()
        };
        let empty = pr_body(&bare, &HashMap::new());
        assert!(empty.contains("## What this PR fixes"));
        assert!(empty.contains("## Findings closed"));
        assert!(empty.contains("0 findings."));
    }

    #[test]
    fn pr_body_has_no_snippet_and_no_local_path() {
        let src = source();
        let body = pr_body(&src, &body_rules());
        for f in &src.findings {
            assert!(!body.contains(&f.snippet), "snippet leaked: {body}");
        }
        assert!(!body.contains("hunter2"));
        // The macOS home prefix is assembled so this file never spells a home path itself
        // (scripts/check-personal-paths.sh refuses any tracked file that does).
        for local in [concat!("/Use", "rs/"), "/home/", "C:\\", "/tmp/", "/var/"] {
            assert!(!body.contains(local), "local path `{local}` in:\n{body}");
        }
        assert!(
            !body.contains('\u{2014}'),
            "no em dashes in a client-facing body"
        );
        // A pipe in a path cannot break the table.
        let mut piped = source();
        piped.findings[0].path = "src/a|b.rs".to_string();
        let body = pr_body(&piped, &body_rules());
        assert!(body.contains("src/a\\|b.rs"), "{body}");
    }

    #[test]
    fn slugs_keep_only_lowercase_letters_digits_and_single_dashes() {
        assert_eq!(rule_slug("SEC_Auth.Token"), "sec-auth-token");
        assert_eq!(rule_slug("  --A__B--  "), "a-b");
        assert_eq!(rule_slug("già"), "gi");
        assert_eq!(rule_slug("!!!"), "");
        assert_eq!(area_slug("crates/server"), "crates-server");
        assert_eq!(area_slug("root"), "root");
    }

    #[test]
    fn a_directory_still_too_big_after_the_first_split_is_split_again_by_two_segments() {
        let mut findings = Vec::new();
        for i in 0..15 {
            findings.push(pf(
                "o/a",
                &format!("crates/server/f{i}.rs"),
                1,
                "DEEP",
                "high",
            ));
            findings.push(pf(
                "o/a",
                &format!("crates/client/f{i}.rs"),
                1,
                "DEEP",
                "high",
            ));
        }
        findings.push(pf("o/a", "build.rs", 1, "DEEP", "high"));
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        let mut areas: Vec<String> = p.units.iter().filter_map(|u| u.area.clone()).collect();
        areas.sort();
        assert_eq!(areas, vec!["crates/client", "crates/server", "root"]);
        assert_eq!(p.units.len(), 3);
        let branches: Vec<&str> = p.units.iter().map(|u| u.branch.as_str()).collect();
        assert!(branches.contains(&"camerata/remediation/deep-crates-server"));
        assert!(branches.contains(&"camerata/remediation/deep-root"));
    }

    #[test]
    fn a_rule_over_the_finding_limit_splits_even_when_the_files_are_few() {
        let opts = PlanOptions {
            max_findings_per_unit: 4,
            ..PlanOptions::default()
        };
        let mut findings = Vec::new();
        for i in 0..3 {
            findings.push(pf("o/a", "api/x.rs", i + 1, "MANY", "high"));
            findings.push(pf("o/a", "web/y.rs", i + 1, "MANY", "high"));
        }
        let p = plan(findings, &HashMap::new(), &opts);
        assert_eq!(p.units.len(), 2);
        assert!(p.units.iter().all(|u| u.findings.len() == 3));
    }

    #[test]
    fn findings_inside_a_unit_are_sorted_by_path_then_line_and_severity_is_the_worst() {
        let findings = vec![
            pf("o/a", "b.rs", 2, "R", "medium"),
            pf("o/a", "a.rs", 9, "R", "high"),
            pf("o/a", "a.rs", 3, "R", "medium"),
        ];
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        let order: Vec<(&str, usize)> = p.units[0]
            .findings
            .iter()
            .map(|f| (f.path.as_str(), f.line))
            .collect();
        assert_eq!(order, vec![("a.rs", 3), ("a.rs", 9), ("b.rs", 2)]);
        assert_eq!(p.units[0].severity, "high");
    }

    #[test]
    fn titles_name_the_rule_the_count_the_repo_and_the_area() {
        let rs = rules(vec![info("R1", "No eval", &[])]);
        let one = plan(
            vec![pf("o/a", "x.rs", 1, "R1", "high")],
            &rs,
            &PlanOptions::default(),
        );
        assert_eq!(one.units[0].title, "Fix R1: No eval (1 finding in o/a)");
        let unknown = plan(
            vec![pf("o/a", "x.rs", 1, "R9", "high")],
            &rs,
            &PlanOptions::default(),
        );
        assert_eq!(unknown.units[0].title, "Fix R9: R9 (1 finding in o/a)");
        let mut many = Vec::new();
        for i in 0..21 {
            many.push(pf(
                "o/a",
                &format!("crates/server/f{i}.rs"),
                1,
                "R1",
                "high",
            ));
            many.push(pf("o/a", &format!("tests/f{i}.rs"), 1, "R1", "high"));
        }
        let p = plan(many, &rs, &PlanOptions::default());
        let titles: Vec<&str> = p.units.iter().map(|u| u.title.as_str()).collect();
        assert!(
            titles.contains(&"Fix R1: No eval (21 findings in o/a, crates)"),
            "{titles:?}"
        );
    }

    #[test]
    fn an_empty_input_is_an_empty_plan_and_the_plan_is_deterministic() {
        let p = plan(Vec::new(), &HashMap::new(), &PlanOptions::default());
        assert!(p.units.is_empty() && p.notes.is_empty());
        let make = || {
            vec![
                pf("o/b", "x.rs", 1, "B", "high"),
                pf("o/a", "y.rs", 2, "A", "high"),
                pf("o/a", "x.rs", 1, "B", "high"),
            ]
        };
        let one = plan(make(), &HashMap::new(), &PlanOptions::default());
        let mut reversed = make();
        reversed.reverse();
        let two = plan(reversed, &HashMap::new(), &PlanOptions::default());
        assert_eq!(one.units, two.units);
        assert_eq!(one.notes, two.notes);
    }

    /// Three findings at one location under two rules: A (critical) once and B (medium) twice.
    fn three_at_one_location() -> Vec<PlanFinding> {
        let a = pf("o/a", "src/auth.rs", 10, "A-RULE", "critical");
        let b1 = pf("o/a", "src/auth.rs", 10, "B-RULE", "medium");
        let mut b2 = pf("o/a", "src/auth.rs", 10, "B-RULE", "medium");
        b2.snippet = "another offending expression".to_string();
        b2.key = format!("o/a\0B-RULE\0src/auth.rs\x0010\0{}", b2.snippet);
        b2.fingerprint = fingerprint("B-RULE", &b2.snippet);
        vec![a, b1, b2]
    }

    #[test]
    fn findings_absorbed_by_a_shared_fix_stay_on_the_unit_as_merged() {
        let input = three_at_one_location();
        let want: BTreeSet<(String, String)> = input
            .iter()
            .map(|f| (f.key.clone(), f.fingerprint.clone()))
            .collect();
        assert_eq!(want.len(), 3, "the three findings are distinct");
        let p = plan(input, &HashMap::new(), &PlanOptions::default());
        assert_eq!(p.units.len(), 1);
        let u = &p.units[0];
        assert_eq!(u.findings.len(), 1);
        assert_eq!(u.findings[0].rule_id, "A-RULE");
        assert_eq!(u.merged.len(), 2);
        assert!(u.merged.iter().all(|f| f.rule_id == "B-RULE"));
        let got: BTreeSet<(String, String)> = u
            .findings
            .iter()
            .chain(u.merged.iter())
            .map(|f| (f.key.clone(), f.fingerprint.clone()))
            .collect();
        assert_eq!(
            got, want,
            "findings plus merged hold all three keys and fingerprints"
        );

        // With grouping off nothing is absorbed, so nothing is merged.
        let off = PlanOptions {
            group_shared_fix: false,
            ..PlanOptions::default()
        };
        let p = plan(three_at_one_location(), &HashMap::new(), &off);
        assert!(p.units.iter().all(|u| u.merged.is_empty()));
        assert_eq!(p.units.iter().map(|u| u.findings.len()).sum::<usize>(), 3);
    }

    #[test]
    fn merged_findings_follow_their_location_into_the_directory_part() {
        let mut input = three_at_one_location();
        // A-RULE is big enough to split: 21 files in src/ and 21 in tests/.
        for i in 0..21 {
            input.push(pf("o/a", &format!("src/s{i}.rs"), 1, "A-RULE", "critical"));
            input.push(pf(
                "o/a",
                &format!("tests/t{i}.rs"),
                1,
                "A-RULE",
                "critical",
            ));
        }
        let p = plan(input, &HashMap::new(), &PlanOptions::default());
        let with_merged: Vec<&PlannedUnit> =
            p.units.iter().filter(|u| !u.merged.is_empty()).collect();
        assert_eq!(
            with_merged.len(),
            1,
            "the absorbed findings are on exactly one unit"
        );
        assert_eq!(with_merged[0].area.as_deref(), Some("src"));
        assert_eq!(with_merged[0].merged.len(), 2);
    }

    #[test]
    fn the_pr_body_lists_merged_findings_next_to_the_finding_they_were_merged_into() {
        let main = sf("src/auth.rs", 10, "A-RULE", "critical");
        let mut m1 = sf("src/auth.rs", 10, "B-RULE", "medium");
        let mut m2 = sf("src/auth.rs", 10, "B-RULE-2", "medium");
        m1.merged_into = Some(main.key.clone());
        m2.merged_into = Some(main.key.clone());
        let src = FindingsSource {
            inspection_id: "insp-x".to_string(),
            audited: vec![AuditedCommit {
                repo: "o/a".to_string(),
                sha: Some("abc123".to_string()),
            }],
            rule_ids: vec!["A-RULE".to_string(), "B-RULE".to_string()],
            // The merged findings sit at the end of the list; the body places them under theirs.
            findings: vec![
                main.clone(),
                sf("src/other.rs", 3, "A-RULE", "high"),
                m1.clone(),
                m2.clone(),
            ],
            ..Default::default()
        };
        let body = pr_body(&src, &HashMap::new());
        for f in [&main, &m1, &m2] {
            assert!(
                body.contains(&f.fingerprint),
                "missing {} in\n{body}",
                f.fingerprint
            );
        }
        let lines: Vec<&str> = body.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains(&main.fingerprint))
            .expect("main row");
        assert!(lines[at + 1].contains(&m1.fingerprint), "{body}");
        assert!(
            lines[at + 1].contains("merged into the row above"),
            "{body}"
        );
        assert!(lines[at + 2].contains(&m2.fingerprint), "{body}");
        assert!(lines[at + 3].contains("src/other.rs"), "{body}");
        assert!(
            body.contains("\n4 findings."),
            "merged findings count: {body}"
        );
        assert!(!body.contains(&m1.snippet), "still no snippets");
        assert!(!body.contains('\u{2014}'));
    }

    #[test]
    fn a_merged_finding_with_no_row_to_sit_under_still_gets_a_row_and_is_counted() {
        let src = FindingsSource {
            inspection_id: "insp-x".to_string(),
            audited: vec![AuditedCommit {
                repo: "o/a".to_string(),
                sha: Some("abc".to_string()),
            }],
            findings: vec![sf("src/a.rs", 1, "A-RULE", "high")],
            ..Default::default()
        };
        let mut stray = sf("src/elsewhere.rs", 9, "B-RULE", "low");
        stray.merged_into = Some("a key no row has".to_string());
        let mut src = src;
        src.findings.push(stray.clone());
        let body = pr_body(&src, &HashMap::new());
        let row = body
            .lines()
            .find(|l| l.contains(&stray.fingerprint))
            .unwrap_or_else(|| panic!("the stray merged finding has no row:\n{body}"));
        assert!(
            row.contains("merged into another finding of this PR"),
            "{row}"
        );
        assert!(
            row.starts_with("| 2 |"),
            "numbered after the main rows: {row}"
        );
        assert!(body.contains("\n2 findings."), "{body}");
    }

    #[test]
    fn a_blank_line_precedes_each_table_and_the_depends_on_and_opened_by_lines() {
        let mut src = source();
        src.audited.push(AuditedCommit {
            repo: "o/b".to_string(),
            sha: None,
        });
        src.findings.push({
            let mut f = sf("lib/x.rs", 2, "SEC-2", "high");
            f.repo = "o/b".to_string();
            f
        });
        let ctx = PrContext {
            story_id: Some("story-77".to_string()),
            depends_on: vec![("uow-12".to_string(), "Fix SEC-9".to_string())],
        };
        let body = pr_body_with(&src, &body_rules(), &ctx);
        let lines: Vec<&str> = body.lines().collect();
        let headers: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("| # |"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(headers.len(), 2, "one table per repository:\n{body}");
        for i in headers {
            assert_eq!(
                lines[i - 1],
                "",
                "header row must follow an empty line:\n{body}"
            );
        }
        for starts in ["Depends on: ", "Opened by Camerata"] {
            let i = lines
                .iter()
                .position(|l| l.starts_with(starts))
                .expect(starts);
            assert_eq!(
                lines[i - 1],
                "",
                "`{starts}` must follow an empty line:\n{body}"
            );
        }
    }

    #[test]
    fn titles_and_dependency_names_cannot_break_the_body_layout() {
        let rs = rules(vec![info(
            "SEC-1",
            "Bad|title\nsecond line",
            &[("Std|1\n2", "http://x/a|b")],
        )]);
        let ctx = PrContext {
            story_id: None,
            depends_on: vec![("uow-3".to_string(), "Fix|it\nnow".to_string())],
        };
        let mut src = source();
        src.rule_ids = vec!["SEC-1".to_string()];
        let body = pr_body_with(&src, &rs, &ctx);
        assert!(
            body.contains(
                "Rule SEC-1: Bad\\|title second line. Standard: Std\\|1 2 (http://x/a\\|b)."
            ),
            "{body}"
        );
        assert!(body.contains("Depends on: uow-3 (Fix\\|it now)"), "{body}");
    }

    #[test]
    fn a_unit_sharing_files_with_two_earlier_units_depends_on_both() {
        let findings = vec![
            pf("o/a", "src/one.rs", 1, "FIRST", "critical"),
            pf("o/a", "src/two.rs", 1, "SECOND", "high"),
            pf("o/a", "src/one.rs", 5, "THIRD", "medium"),
            pf("o/a", "src/two.rs", 5, "THIRD", "medium"),
        ];
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        let idx = |r: &str| p.units.iter().position(|u| primary(u) == r).expect("unit");
        assert_eq!(
            p.units[idx("THIRD")].depends_on,
            vec![idx("FIRST"), idx("SECOND")]
        );
        assert!(
            p.units[idx("SECOND")].depends_on.is_empty(),
            "FIRST and SECOND share nothing"
        );
        let edges = p
            .notes
            .iter()
            .filter(|n| n.starts_with("uow for THIRD edits files"))
            .count();
        assert_eq!(edges, 2);
    }

    #[test]
    fn severity_ranks_ignore_case_and_surrounding_spaces() {
        let mut findings = vec![pf("o/a", "t.rs", 1, "TOP", " High ")];
        findings.push(pf("o/a", "t2.rs", 1, "TOP", " High "));
        for i in 0..5 {
            findings.push(pf("o/a", &format!("m{i}.rs"), 1, "MID", "medium"));
        }
        findings.push(pf("o/a", "c.rs", 1, "CRIT", "CRITICAL"));
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        let order: Vec<&str> = p.units.iter().map(primary).collect();
        assert_eq!(
            order,
            vec!["CRIT", "TOP", "MID"],
            "High beats medium however it is written"
        );
    }

    #[test]
    fn a_duplicate_finding_is_planned_once_and_the_plan_says_so() {
        let one = pf("o/a", "src/a.rs", 3, "R1", "high");
        let mut again = one.clone();
        again.also_matches = vec!["R2".to_string()];
        let other = pf("o/a", "src/b.rs", 4, "R1", "high");
        let p = plan(
            vec![one.clone(), other, again, one.clone()],
            &HashMap::new(),
            &PlanOptions::default(),
        );
        assert_eq!(p.units.len(), 1);
        assert_eq!(p.units[0].findings.len(), 2, "src/a.rs:3 is listed once");
        assert!(
            p.notes
                .iter()
                .any(|n| n == "2 duplicate findings were planned once."),
            "{:?}",
            p.notes
        );
        // The same input in another order plans the same way.
        let reordered = plan(
            vec![
                one.clone(),
                one.clone(),
                pf("o/a", "src/b.rs", 4, "R1", "high"),
                {
                    let mut a = one;
                    a.also_matches = vec!["R2".to_string()];
                    a
                },
            ],
            &HashMap::new(),
            &PlanOptions::default(),
        );
        assert_eq!(reordered.units, p.units);
    }

    #[test]
    fn a_part_still_over_the_limit_after_the_split_is_named_in_a_note() {
        let mut findings = Vec::new();
        for i in 0..70 {
            findings.push(pf("o/a", "src/huge.rs", i + 1, "BIG", "high"));
        }
        findings.push(pf("o/a", "tests/t.rs", 1, "BIG", "high"));
        let p = plan(findings, &HashMap::new(), &PlanOptions::default());
        assert_eq!(p.units.len(), 2);
        assert!(
            p.notes.iter().any(|n| {
                n
                == "BIG in src still has 70 findings in 1 file after the split; it stays one unit."
            }),
            "{:?}",
            p.notes
        );
        assert!(
            !p.notes.iter().any(|n| n.contains("in tests still")),
            "{:?}",
            p.notes
        );
    }
}
