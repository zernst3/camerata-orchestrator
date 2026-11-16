//! Places a ruled-out row carries for another rule become rows of their own.
//!
//! A merge puts other rules' problems on a row: sites of another rule or another service in its
//! `also_locations`, and rules it `also_matches` at its own place. A reviewer who rules that row
//! out has ruled on its own problem and nothing else (owner ruling, 2026-11-15): the places it
//! carried for other rules stay open until they are ruled on themselves. Left on the ruled-out
//! row they would leave `findings` with it and be counted nowhere, a finding merged away. So the
//! overlay, which every export and the cockpit read, unfolds each of them into a row of its own.
//!
//! # The row
//!
//! * rule id, repo, path and line are the carried place's; the snippet is the one the host held
//!   for that place (nothing is invented: a place the host held no snippet for carries none, and
//!   the internal note says so);
//! * it is held for a decision (`needs_review`), with the neutral client note
//!   [`UNFOLD_CLIENT_NOTE`] as its explanation (after the absorbed site's own headline, when the
//!   host recorded one) and an internal-only note, [`UNFOLDED_FROM_CAPTURE`], that names the host
//!   by its readable id;
//! * its severity is the higher of the place's own recorded severity and the host's before the
//!   ruling; the exporters then apply the
//!   carried rule's own bounds (its floor, among them) like any other row;
//! * its site key is built from the host's key, the carried rule and the place's own key, so a
//!   decision on it sticks across exports, and a ruling that rules it out leaves nothing to unfold
//!   again;
//! * a place that already has a row of its own (the same rule at the same repo, path and line, the
//!   identity the review tree's unfold uses) gets no second row.
//!
//! The carried places are removed from the host as they are unfolded, so a count that reads the
//! host and the new rows (a Patterns across services cell) counts each place once, and
//! `open_beside_ruled_out` stays empty in a normal export.

use std::collections::{BTreeMap, HashSet};

use camerata_api_types::audit::FindingKind;

use super::finding_key;
use crate::onboard::patterns_matrix::is_other_rules_place;
use crate::onboard::{Finding, ScanReport};

/// The `captures` key on an unfolded row: the internal-only note that names the ruled-out row
/// the place was merged into. A bookkeeping key; no client output reads it.
pub const UNFOLDED_FROM_CAPTURE: &str = "unfolded_from";
/// What a client reads on an unfolded row.
pub const UNFOLD_CLIENT_NOTE: &str = "Was grouped with a problem that was ruled out; still open.";

/// One place a ruled-out row carried for another rule.
struct Carried {
    repo: String,
    path: String,
    line: usize,
    rule_id: String,
    snippet: String,
    headline: String,
    /// The severity the place was rated before it was grouped; empty when none was recorded.
    severity: String,
    consequence: bool,
}

/// Unfold the places every row `ruled_out` names carries for another rule, in `report`. Returns
/// how many rows were added. The carried places leave the ruled-out row whether or not they get a
/// row (a place with a row of its own is already counted there).
pub(crate) fn unfold_ruled_out(
    report: &mut ScanReport,
    ruled_out: &dyn Fn(&Finding) -> bool,
) -> usize {
    // Most reports have no ruled-out row that carries anything: build nothing for them.
    if !report
        .findings
        .iter()
        .any(|f| (!f.also_locations.is_empty() || !f.also_matches.is_empty()) && ruled_out(f))
    {
        return 0;
    }
    let mut keys: HashSet<String> = HashSet::new();
    let mut placed: HashSet<(String, String, usize, String)> = HashSet::new();
    for f in &report.findings {
        let rule = f.rule_id.to_ascii_uppercase();
        keys.insert(finding_key(f));
        placed.insert((f.repo.clone(), f.path.clone(), f.line, rule));
        for b in &f.flagged_by {
            placed.insert((
                f.repo.clone(),
                b.path.clone(),
                b.line,
                b.rule_id.to_ascii_uppercase(),
            ));
        }
    }

    let mut added: Vec<Finding> = Vec::new();
    for host in report.findings.iter_mut().filter(|f| ruled_out(f)) {
        let host_key = finding_key(host);
        let mut carried: Vec<Carried> = Vec::new();
        let (carried_sites, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut host.also_locations)
            .into_iter()
            .partition(|l| is_other_rules_place(host, &l.repo, &l.rule_id));
        // The places that stay are put back as they were; nothing here makes a site.
        let mut kept = kept;
        std::mem::swap(&mut host.also_locations, &mut kept);
        for l in carried_sites {
            carried.push(Carried {
                repo: l.repo,
                path: l.path,
                line: l.line,
                rule_id: l.rule_id,
                snippet: l.snippet,
                headline: l.headline,
                severity: l.severity,
                consequence: l.consequence,
            });
        }
        for rule in std::mem::take(&mut host.also_matches) {
            if is_other_rules_place(host, &host.repo, &rule) {
                carried.push(Carried {
                    repo: host.repo.clone(),
                    path: host.path.clone(),
                    line: host.line,
                    rule_id: rule,
                    snippet: host.snippet.clone(),
                    headline: String::new(),
                    severity: String::new(),
                    consequence: false,
                });
            } else {
                host.also_matches.push(rule);
            }
        }
        for c in carried {
            let own_key =
                camerata_api_types::site_key::site_key(&c.repo, &c.path, &c.rule_id, &c.snippet, 0);
            let rule = c.rule_id.to_ascii_uppercase();
            let place = (c.repo.clone(), c.path.clone(), c.line, rule.clone());
            if placed.contains(&place) {
                continue;
            }
            let base = format!("{host_key}\u{0}unfolded\u{0}{}\u{0}{own_key}", c.rule_id);
            // Two places of one rule in one file can share a snippet (or have none): the place's
            // line tells them apart, then a counter for the rest.
            let mut key = base.clone();
            if keys.contains(&key) {
                key = format!("{base}\u{0}line{}", c.line);
            }
            let mut n = 1;
            while keys.contains(&key) {
                key = format!("{base}\u{0}{n}");
                n += 1;
            }
            keys.insert(key.clone());
            placed.insert(place);
            added.push(row_for(host, &host_key, c, key));
        }
    }
    let count = added.len();
    report.findings.extend(added);
    count
}

fn row_for(host: &Finding, host_key: &str, c: Carried, key: String) -> Finding {
    let detail = if c.headline.trim().is_empty() {
        UNFOLD_CLIENT_NOTE.to_string()
    } else {
        format!(
            "{}{}{UNFOLD_CLIENT_NOTE}",
            c.headline.trim(),
            crate::ai_audit::HEADLINE_SEP
        )
    };
    let mut note = format!(
        "Grouped with {} at {}:{} in {} (problem {}), which was ruled out.",
        host.rule_id,
        host.path,
        host.line,
        host.repo,
        super::site_id(host_key)
    );
    if c.snippet.is_empty() {
        note.push_str(" The host held no snippet for this place.");
    }
    Finding {
        repo: c.repo,
        path: c.path,
        line: c.line,
        rule_id: c.rule_id,
        severity: higher_severity(&c.severity, &host.severity),
        snippet: c.snippet,
        detail,
        status: "active".to_string(),
        needs_review: true,
        // The source stays the place's own: a tool's pass keeps its tool, and no confidence
        // is set that would relabel it. The hold is `needs_review`.
        preview_tool: host.preview_tool.clone(),
        located: !c.consequence,
        kind: Some(FindingKind::Defect),
        site_key: key,
        captures: BTreeMap::from([(UNFOLDED_FROM_CAPTURE.to_string(), note)]),
        ..Finding::default()
    }
}

/// The more severe of two severities (an empty one counts for nothing). The exporters then apply
/// the carried rule's own bounds.
fn higher_severity(own: &str, host: &str) -> String {
    use camerata_rules::severity_bounds::{normalize_severity, severity_rank};
    if own.trim().is_empty() {
        return host.to_string();
    }
    if host.trim().is_empty() {
        return own.to_string();
    }
    if severity_rank(&normalize_severity(own)) < severity_rank(&normalize_severity(host)) {
        own.to_string()
    } else {
        host.to_string()
    }
}
