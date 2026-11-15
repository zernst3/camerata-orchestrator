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
//!   host recorded one) and an internal-only note, [`UNFOLDED_FROM_CAPTURE`], that names the host;
//! * its severity starts as the host's severity before the ruling; the exporters then apply the
//!   carried rule's own bounds (its floor, among them) like any other row;
//! * its site key is built from the host's key, the carried rule and the place's own key, so a
//!   decision on it sticks across exports, and a ruling that rules it out leaves nothing to unfold
//!   again;
//! * a place that already has a row of its own (same rule and same site key, or the same rule at
//!   the same repo, path and line) gets no second row.
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
pub const UNFOLD_CLIENT_NOTE: &str = "Was merged into a row ruled out; still open.";

/// One place a ruled-out row carried for another rule.
struct Carried {
    repo: String,
    path: String,
    line: usize,
    rule_id: String,
    snippet: String,
    headline: String,
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
    let mut sited: HashSet<(String, String)> = HashSet::new();
    let mut placed: HashSet<(String, String, usize, String)> = HashSet::new();
    for f in &report.findings {
        let rule = f.rule_id.to_ascii_uppercase();
        keys.insert(finding_key(f));
        sited.insert((rule.clone(), finding_key(f)));
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
        for l in std::mem::take(&mut host.also_locations) {
            if is_other_rules_place(host, &l.repo, &l.rule_id) {
                carried.push(Carried {
                    repo: l.repo,
                    path: l.path,
                    line: l.line,
                    rule_id: l.rule_id,
                    snippet: l.snippet,
                    headline: l.headline,
                    consequence: l.consequence,
                });
            } else {
                host.also_locations.push(l);
            }
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
            if sited.contains(&(rule.clone(), own_key.clone())) || placed.contains(&place) {
                continue;
            }
            let base = format!("{host_key}\u{0}unfolded\u{0}{}\u{0}{own_key}", c.rule_id);
            let mut key = base.clone();
            let mut n = 1;
            while keys.contains(&key) {
                key = format!("{base}\u{0}{n}");
                n += 1;
            }
            keys.insert(key.clone());
            sited.insert((rule, own_key));
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
        "Merged into {} at {}:{} in {} (row {host_key}), which was ruled out.",
        host.rule_id, host.path, host.line, host.repo
    );
    if c.snippet.is_empty() {
        note.push_str(" The host held no snippet for this place.");
    }
    Finding {
        repo: c.repo,
        path: c.path,
        line: c.line,
        rule_id: c.rule_id,
        severity: host.severity.clone(),
        snippet: c.snippet,
        detail,
        status: "active".to_string(),
        needs_review: true,
        confidence: Some("needs-review".to_string()),
        located: !c.consequence,
        kind: Some(FindingKind::Defect),
        site_key: key,
        captures: BTreeMap::from([(UNFOLDED_FROM_CAPTURE.to_string(), note)]),
        ..Finding::default()
    }
}
