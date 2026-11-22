//! The convention model for rules with options (design 2026-11-20, sections 3 to 5 and 9).
//!
//! One [`RuleConvention`] record holds the facts of a (repo, rule with options): the options, the
//! places the scan recorded for each, and what the finder's directive was. One pure resolver turns
//! those facts plus the reviewer's rulings into rows and labels. The scan runs it with no rulings;
//! the overlay runs it again only for a rule that has a ruling, so an inspection nobody ruled on is
//! unchanged. Nothing here reads a file, calls a model or touches a store.
//!
//! Words used everywhere: place, option, observed convention, standard, proposal, departure, open
//! question.

use std::collections::{BTreeMap, HashSet};

use camerata_api_types::audit::{
    CensusPlace, ConventionObservedKind, DeviationKind, FinderRowRef, FindingKind,
    NoConventionKind, ObservedOptionUsage, OptionBasis, OptionCensus, PlaceStatus, Proposal,
    ProposalBasis, RuleConvention, StandardSource, StandardState,
};
use camerata_api_types::decisions::{StandardChoice, StandardRuling};

use crate::ai_audit::agent_finder::reply::{
    FINDER_END_LINE_CAPTURE, FINDER_PROBLEM_CAPTURE, FINDER_RULES_CAPTURE,
};
use crate::onboard::Finding;

/// The capture an engine departure row carries: the option id of the place it stands for. A
/// remainder row (places that were counted but not listed) carries `option~K`. A bookkeeping key
/// (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`) and an overlay key
/// (`report_export::overlay::OVERLAY_CAPTURE_KEYS`).
pub const CONVENTION_PLACE_CAPTURE: &str = "convention-place";

/// What [`resolve`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolveOutcome {
    /// Engine rows removed (departure rows and information rows that no longer hold).
    pub removed: usize,
    /// Rows added.
    pub added: usize,
    /// Whether anything about the rows changed (a row added or removed, a kind rewritten).
    pub changed: bool,
}

fn same_rule(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn plain_path(p: &str) -> &str {
    p.trim_start_matches("./")
}

/// A row an agent wrote (the finder's own problem or a place of one).
pub fn is_finder_row(f: &Finding) -> bool {
    f.captures.contains_key(FINDER_PROBLEM_CAPTURE)
}

/// Whether the finder row names `rule` among the rules it was cited to, or is keyed to it.
pub fn cites_rule(f: &Finding, rule: &str) -> bool {
    same_rule(&f.rule_id, rule)
        || f.captures
            .get(FINDER_RULES_CAPTURE)
            .is_some_and(|c| c.split(',').any(|r| same_rule(r.trim(), rule)))
}

/// The lines a finder row covers: its line to its end line.
fn row_range(f: &Finding) -> (usize, usize) {
    let end = f
        .captures
        .get(FINDER_END_LINE_CAPTURE)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(f.line)
        .max(f.line);
    (f.line, end)
}

/// The place a convention-place capture stands for: the option and, for a remainder row, the
/// number of places it stands for.
fn place_capture(f: &Finding) -> Option<(&str, Option<usize>)> {
    let v = f.captures.get(CONVENTION_PLACE_CAPTURE)?;
    match v.split_once('~') {
        Some((option, k)) => Some((option, k.parse().ok())),
        None => Some((v.as_str(), None)),
    }
}

/// Which option a finder row's place follows, worked out and never stored as a guess:
/// 1. `Listed`: a recorded place of an option has the same path and its line inside the row's
///    line range.
/// 2. `OnlyOther`: no match, and exactly one option other than the judged one has a count above
///    zero. A place that breaks the judged option must follow that one.
/// 3. `Unknown`: neither.
pub fn option_of_row(row: &Finding, c: &RuleConvention) -> (Option<String>, OptionBasis) {
    let (lo, hi) = row_range(row);
    let path = plain_path(&row.path);
    for o in &c.options {
        if o.places
            .iter()
            .any(|p| plain_path(&p.path) == path && p.line >= lo && p.line <= hi)
        {
            return (Some(o.option_id.clone()), OptionBasis::Listed);
        }
    }
    if let Some(judged) = &c.judged_option_id {
        let others: Vec<&OptionCensus> = c
            .options
            .iter()
            .filter(|o| &o.option_id != judged && o.count.unwrap_or(0) > 0)
            .collect();
        if let [only] = others.as_slice() {
            return (Some(only.option_id.clone()), OptionBasis::OnlyOther);
        }
    }
    (None, OptionBasis::Unknown)
}

/// Whether a recorded place of `option` conforms to the standard, departs from it, or follows
/// the rule's default while the standard is only observed and differs from it (A2).
pub fn option_status(c: &RuleConvention, s: &StandardState, option: &str) -> PlaceStatus {
    let Some(standard) = &s.option_id else {
        return PlaceStatus::NoStandard;
    };
    if option == standard {
        PlaceStatus::Conforms
    } else if s.source == StandardSource::Observed && c.default_option_id.as_deref() == Some(option)
    {
        PlaceStatus::FollowsRuleDefault
    } else {
        PlaceStatus::Departs
    }
}

/// Whether a finder row citing the rule departs from the standard. A row whose option is not
/// known is never a departure unless the standard IS the judged option (then it broke it by
/// definition): it is the finder's own problem.
pub fn finder_status(c: &RuleConvention, s: &StandardState, option: Option<&str>) -> PlaceStatus {
    let Some(standard) = &s.option_id else {
        return PlaceStatus::NoStandard;
    };
    if c.judged_option_id.as_deref() == Some(standard.as_str()) {
        return PlaceStatus::Departs;
    }
    match option {
        Some(o) => option_status(c, s, o),
        None => PlaceStatus::NoStandard,
    }
}

/// Whether a finder row citing the rule departs from the standard, judged from the row itself.
/// A row whose line range holds recorded places of more than one option cannot be said to follow
/// the standard just because one of them does: when any of those options departs, the row is a
/// departure, so the departing place inside it is never taken as covered by a conforming row.
pub fn row_status(c: &RuleConvention, s: &StandardState, row: &Finding) -> PlaceStatus {
    let (option, _) = option_of_row(row, c);
    if s.option_id.is_some() {
        let (lo, hi) = row_range(row);
        let path = plain_path(&row.path);
        let any_departs = c
            .options
            .iter()
            .filter(|o| {
                o.places
                    .iter()
                    .any(|p| plain_path(&p.path) == path && p.line >= lo && p.line <= hi)
            })
            .any(|o| option_status(c, s, &o.option_id) == PlaceStatus::Departs);
        if any_departs {
            return PlaceStatus::Departs;
        }
    }
    finder_status(c, s, option.as_deref())
}

fn label_of<'a>(c: &'a RuleConvention, option: &'a str) -> &'a str {
    c.options
        .iter()
        .find(|o| o.option_id == option)
        .map_or(option, |o| o.label.as_str())
}

/// The standard's option, when it is a real option that does not describe the rule's own defect.
fn usable_standard(c: &RuleConvention) -> Option<&OptionCensus> {
    let id = c.standard.option_id.as_deref()?;
    c.options
        .iter()
        .find(|o| o.option_id == id)
        .filter(|o| !o.violates)
}

fn standard_sentence(label: &str, source: StandardSource) -> String {
    match source {
        StandardSource::Reviewer => format!("The convention set by the reviewer is \"{label}\"."),
        StandardSource::RuleDefault => format!("The rule's default is \"{label}\"."),
        StandardSource::EarlierChoice => {
            format!("The option your project chose earlier is \"{label}\".")
        }
        StandardSource::MostCommon => format!("Most places follow \"{label}\"."),
        _ => format!("Most of the code follows \"{label}\"."),
    }
}

/// The departure row of one place: the place's own path, line and line of code. A scan row and an
/// overlay row for one place have the same rule id and snippet, hence the same site key.
pub fn departure_row(
    repo: &str,
    rule_id: &str,
    place: &CensusPlace,
    option_id: &str,
    option_label: &str,
    standard_label: &str,
    source: StandardSource,
) -> Finding {
    let mut f = Finding {
        repo: repo.to_string(),
        path: plain_path(&place.path).to_string(),
        line: place.line,
        rule_id: rule_id.to_string(),
        severity: "low".to_string(),
        snippet: place.text.trim().to_string(),
        detail: format!(
            "This place uses \"{option_label}\". {}",
            standard_sentence(standard_label, source)
        ),
        located: true,
        ..Finding::default()
    };
    f.captures
        .insert(CONVENTION_PLACE_CAPTURE.to_string(), option_id.to_string());
    crate::onboard::redact::redact_finding(&mut f);
    f
}

/// A departure row made at the place of a finder row that departs but is keyed to another rule.
fn departure_row_at_finder(
    c: &RuleConvention,
    row: &Finding,
    option: Option<&str>,
    standard_label: &str,
) -> Finding {
    let detail = match option {
        Some(o) => format!(
            "This place uses \"{}\". {}",
            label_of(c, o),
            standard_sentence(standard_label, c.standard.source)
        ),
        None => format!(
            "This place does not follow \"{standard_label}\". {}",
            standard_sentence(standard_label, c.standard.source)
        ),
    };
    let mut f = Finding {
        repo: c.repo.clone(),
        path: row.path.clone(),
        line: row.line,
        rule_id: c.rule_id.clone(),
        severity: "low".to_string(),
        snippet: row.snippet.clone(),
        detail,
        located: row.located,
        ..Finding::default()
    };
    f.captures.insert(
        CONVENTION_PLACE_CAPTURE.to_string(),
        option.unwrap_or_default().to_string(),
    );
    f
}

/// The one row with no location for places that were counted but not listed.
fn remainder_row(
    c: &RuleConvention,
    option: &OptionCensus,
    k: usize,
    standard_label: &str,
) -> Finding {
    let places = if k == 1 { "place" } else { "places" };
    let verb = if k == 1 { "follows" } else { "follow" };
    let mut f = Finding {
        repo: c.repo.clone(),
        path: String::new(),
        line: 0,
        rule_id: c.rule_id.clone(),
        severity: "low".to_string(),
        snippet: format!(
            "{k} more {places} {verb} \"{}\". This scan did not record where.",
            option.label
        ),
        detail: format!(
            "{k} more {places} {verb} \"{}\". This scan did not record where. {}",
            option.label,
            standard_sentence(standard_label, c.standard.source)
        ),
        located: false,
        ..Finding::default()
    };
    f.captures.insert(
        CONVENTION_PLACE_CAPTURE.to_string(),
        format!("{}~{k}", option.option_id),
    );
    f
}

/// The snippet of the problem needing a decision for a listed place that could not be checked.
const HELD_PLACE_SNIPPET: &str = "A listed place could not be checked";

/// The problem needing a decision for a listed place that could not be checked against the file.
/// An empty `title` names the rule by its id alone.
pub fn held_place_row(
    repo: &str,
    rule: &str,
    title: &str,
    option_label: &str,
    place: &CensusPlace,
) -> Finding {
    let rule_name = if title.is_empty() {
        rule.to_string()
    } else {
        format!("{title} ({rule})")
    };
    let mut row = Finding {
        repo: repo.to_string(),
        path: plain_path(&place.path).to_string(),
        line: place.line,
        rule_id: "AI-OPEN-QUESTION".to_string(),
        severity: "low".to_string(),
        snippet: HELD_PLACE_SNIPPET.to_string(),
        detail: format!(
            "A place could not be checked{}A reviewer listed this place as using \"{option_label}\" for {rule_name}, but it could not be found in the file. It needs a decision.",
            crate::ai_audit::HEADLINE_SEP,
        ),
        needs_review: true,
        located: false,
        ..Finding::default()
    };
    crate::onboard::redact::redact_finding(&mut row);
    row
}

/// Whether `rows` already hold the question for this unchecked place of `rule`.
fn has_held_place_row(rows: &[Finding], repo: &str, rule: &str, place: &CensusPlace) -> bool {
    rows.iter().any(|r| {
        r.repo == repo
            && r.rule_id == "AI-OPEN-QUESTION"
            && r.snippet == HELD_PLACE_SNIPPET
            && plain_path(&r.path) == plain_path(&place.path)
            && r.line == place.line
            && r.detail.contains(rule)
    })
}

/// A held question for every listed place that could not be checked and whose option departs from
/// the standard of `c`, unless `rows` already hold it. A place that cannot be checked has no code
/// to show, so it cannot be a departure row: it is a question for the reviewer, and it is made
/// from the standard in force (not only at scan time), so a place that only departs once the
/// reviewer rules on the rule is never left out (no open violations). `title` is the rule's title,
/// or empty.
pub fn held_place_rows(
    c: &RuleConvention,
    rows: &[Finding],
    title: &str,
) -> Vec<(String, Finding)> {
    let mut out = Vec::new();
    if c.standard.option_id.is_none() {
        return out;
    }
    for o in &c.options {
        if option_status(c, &c.standard, &o.option_id) != PlaceStatus::Departs {
            continue;
        }
        for p in o.places.iter().filter(|p| !p.checked) {
            let made: Vec<Finding> = out
                .iter()
                .map(|(_, f): &(String, Finding)| f.clone())
                .collect();
            if has_held_place_row(rows, &c.repo, &c.rule_id, p)
                || has_held_place_row(&made, &c.repo, &c.rule_id, p)
            {
                continue;
            }
            out.push((
                o.label.clone(),
                held_place_row(&c.repo, &c.rule_id, title, &o.label, p),
            ));
        }
    }
    out
}

fn is_engine_departure(f: &Finding) -> bool {
    f.captures.contains_key(CONVENTION_PLACE_CAPTURE)
}

fn is_info_kind(f: &Finding) -> bool {
    matches!(
        f.kind,
        Some(FindingKind::ConventionObserved(_) | FindingKind::NoConvention(_))
    )
}

/// The departure rows the standard of `c` asks for, given the rows now in the report
/// (section 3). Deterministic order: option, path, line.
pub fn departure_rows(c: &RuleConvention, rows: &[Finding]) -> Vec<Finding> {
    let Some(standard) = usable_standard(c) else {
        return Vec::new();
    };
    let standard_label = standard.label.clone();
    let s = &c.standard;
    let citing: Vec<&Finding> = rows
        .iter()
        .filter(|r| is_finder_row(r) && r.repo == c.repo && cites_rule(r, &c.rule_id))
        .collect();
    let mut out: Vec<(String, String, usize, Finding)> = Vec::new();
    // Departing finder rows keyed to another rule: a departure row at their own place.
    let mut finder_by_option: BTreeMap<String, usize> = BTreeMap::new();
    for row in &citing {
        let (option, _) = option_of_row(row, c);
        if row_status(c, s, row) != PlaceStatus::Departs {
            continue;
        }
        if let Some(o) = &option {
            let listed = c
                .options
                .iter()
                .find(|x| &x.option_id == o)
                .is_some_and(|x| {
                    let (lo, hi) = row_range(row);
                    x.places.iter().any(|p| {
                        plain_path(&p.path) == plain_path(&row.path) && p.line >= lo && p.line <= hi
                    })
                });
            if !listed {
                *finder_by_option.entry(o.clone()).or_default() += 1;
            }
        }
        if !same_rule(&row.rule_id, &c.rule_id) {
            let f = departure_row_at_finder(c, row, option.as_deref(), &standard_label);
            out.push((
                option.clone().unwrap_or_default(),
                f.path.clone(),
                f.line,
                f,
            ));
        }
    }
    // Recorded places that depart and no finder row covers.
    for o in &c.options {
        if option_status(c, s, &o.option_id) != PlaceStatus::Departs {
            continue;
        }
        for p in &o.places {
            if !p.checked {
                // The scan held an unchecked place as a question of its own.
                continue;
            }
            // Only a finder row that itself departs covers a departing place: a conforming row
            // that happens to span it must not hide it.
            let covered = citing.iter().any(|r| {
                let (lo, hi) = row_range(r);
                plain_path(&r.path) == plain_path(&p.path)
                    && p.line >= lo
                    && p.line <= hi
                    && row_status(c, s, r) == PlaceStatus::Departs
            });
            if covered {
                continue;
            }
            let f = departure_row(
                &c.repo,
                &c.rule_id,
                p,
                &o.option_id,
                &o.label,
                &standard_label,
                s.source,
            );
            out.push((o.option_id.clone(), f.path.clone(), f.line, f));
        }
        // Places counted but not listed (over the cap, or an old report): one row with no
        // location for each option, so nothing departs out of sight.
        if let Some(n) = o.count {
            let accounted =
                o.places.len() + finder_by_option.get(&o.option_id).copied().unwrap_or(0);
            let k = n.saturating_sub(accounted);
            if k > 0 {
                let f = remainder_row(c, o, k, &standard_label);
                out.push((o.option_id.clone(), String::new(), 0, f));
            }
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1, a.2, &a.3.snippet).cmp(&(&b.0, &b.1, b.2, &b.3.snippet)));
    out.into_iter().map(|(_, _, _, f)| f).collect()
}

/// Set the kinds, counts and place statuses of every rule's rows from its standard. The scan and
/// the overlay both end here. Replaces `assign_deviation_kinds` for the agent finder.
pub fn tag(rows: &mut [Finding], conventions: &mut [RuleConvention]) {
    for c in conventions.iter_mut() {
        let s = c.standard.clone();
        // Place statuses.
        for o in &mut c.options {
            let status = option_status_of(&s, c.default_option_id.as_deref(), &o.option_id);
            for p in &mut o.places {
                p.status = status;
            }
        }
        // Every finder row citing the rule, with what was worked out for it.
        let mut refs: Vec<FinderRowRef> = Vec::new();
        let mut departing: Vec<usize> = Vec::new();
        let mut not_departing: Vec<usize> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if !is_finder_row(row) || row.repo != c.repo || !cites_rule(row, &c.rule_id) {
                continue;
            }
            let (option, basis) = option_of_row(row, c);
            let status = row_status(c, &s, row);
            refs.push(FinderRowRef {
                site_key: row.site_key.clone(),
                path: row.path.clone(),
                line: row.line,
                note: row.snippet.clone(),
                keyed_rule_id: row.rule_id.clone(),
                option_id: option,
                option_basis: basis,
                status,
            });
            if same_rule(&row.rule_id, &c.rule_id) {
                if status == PlaceStatus::Departs {
                    departing.push(i);
                } else {
                    not_departing.push(i);
                }
            }
        }
        c.finder_rows = refs;
        // A keyed finder row that does not depart is the finder's own problem again.
        for i in not_departing {
            if matches!(rows[i].kind, Some(FindingKind::Deviation(_))) {
                rows[i].kind = Some(FindingKind::Defect);
            }
        }
        let Some(standard) = usable_standard(c).cloned() else {
            c.standard.departing_places = 0;
            c.standard.conforming_places = 0;
            continue;
        };
        // The numerator: every departing row keyed to the rule, a remainder row for the places it
        // stands for, plus each absorbed place.
        let mut deviating = 0usize;
        let mut counted: Vec<usize> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if row.repo != c.repo || is_info_kind(row) {
                continue;
            }
            let own = same_rule(&row.rule_id, &c.rule_id);
            let absorbed = row
                .also_locations
                .iter()
                .filter(|l| same_rule(&l.rule_id, &c.rule_id))
                .count();
            if !own {
                deviating += absorbed;
                continue;
            }
            if is_finder_row(row) && !departing.contains(&i) {
                deviating += absorbed;
                continue;
            }
            let weight = place_capture(row).and_then(|(_, k)| k).unwrap_or(1);
            deviating += weight + absorbed;
            counted.push(i);
        }
        c.standard.departing_places = deviating;
        c.standard.conforming_places = standard.count.unwrap_or(0);
        if deviating == 0 {
            continue;
        }
        let assessed = c.assessed_places.map(|t| t.max(deviating));
        for i in counted {
            if crate::ai_audit::is_objective_fault_row(&rows[i]) {
                continue;
            }
            rows[i].kind = Some(FindingKind::Deviation(DeviationKind {
                deviating_sites: deviating,
                assessed_sites: assessed,
                convention_option_id: Some(standard.option_id.clone()),
                convention_label: standard.label.clone(),
                standard_source: Some(s.source),
                differs_from_default: s.differs_from_default.clone(),
                following_sites: standard.count,
            }));
            rows[i].evaluated_option_id = Some(standard.option_id.clone());
        }
    }
}

fn option_status_of(s: &StandardState, default: Option<&str>, option: &str) -> PlaceStatus {
    let Some(standard) = &s.option_id else {
        return PlaceStatus::NoStandard;
    };
    if option == standard {
        PlaceStatus::Conforms
    } else if s.source == StandardSource::Observed && default == Some(option) {
        PlaceStatus::FollowsRuleDefault
    } else {
        PlaceStatus::Departs
    }
}

/// The ruling in force for (repo, rule): the latest one not withdrawn. The caller has already
/// narrowed the list to the inspection being read.
pub fn effective_ruling<'a>(
    rulings: &'a [StandardRuling],
    repo: &str,
    rule_id: &str,
) -> Option<&'a StandardRuling> {
    rulings
        .iter()
        .rev()
        .find(|r| r.is_active() && r.repo == repo && same_rule(&r.rule_id, rule_id))
}

/// The rulings of one inspection: a ruling never applies to another.
pub fn rulings_for_run(rulings: &[StandardRuling], run_id: &str) -> Vec<StandardRuling> {
    rulings
        .iter()
        .filter(|r| r.is_active() && r.run_id == run_id)
        .cloned()
        .collect()
}

fn site_key_free(f: &Finding, taken: &HashSet<String>) -> String {
    (0..)
        .map(|ordinal| {
            camerata_api_types::site_key::site_key(
                &f.repo, &f.path, &f.rule_id, &f.snippet, ordinal,
            )
        })
        .find(|k| !taken.contains(k))
        .unwrap_or_default()
}

/// The standard a ruling sets for `c`, or `None` when the ruling does not hold for the rule (an
/// option that is not on it, or that describes its defect).
fn standard_of_ruling(c: &RuleConvention, ruling: &StandardRuling) -> Option<StandardState> {
    match &ruling.choice {
        StandardChoice::LeaveToClient => Some(StandardState {
            left_to_client: true,
            ruling_id: Some(ruling.id.clone()),
            ..StandardState::default()
        }),
        StandardChoice::Accept { option_id } => {
            let o = c
                .options
                .iter()
                .find(|o| &o.option_id == option_id && !o.violates)?;
            let differs = c
                .default_option_id
                .as_deref()
                .filter(|d| *d != o.option_id)
                .map(|d| label_of(c, d).to_string());
            Some(StandardState {
                option_id: Some(o.option_id.clone()),
                source: StandardSource::Reviewer,
                ruling_id: Some(ruling.id.clone()),
                differs_from_default: differs,
                ..StandardState::default()
            })
        }
    }
}

/// Apply the reviewer's rulings to the rows. Only a rule that has a ruling is touched. For each:
/// set the standard, remove the engine's departure rows and information rows for the rule, add
/// the departure rows and the information row the new standard asks for, then tag.
pub fn resolve(
    rows: &mut Vec<Finding>,
    conventions: &mut [RuleConvention],
    rulings: &[StandardRuling],
) -> ResolveOutcome {
    let mut outcome = ResolveOutcome::default();
    let keyed = rows.iter().any(|r| !r.site_key.is_empty());
    let mut ruled: Vec<usize> = Vec::new();
    for (i, c) in conventions.iter().enumerate() {
        if effective_ruling(rulings, &c.repo, &c.rule_id).is_some() {
            ruled.push(i);
        }
    }
    if ruled.is_empty() {
        return outcome;
    }
    // Deterministic order: repo, rule.
    ruled.sort_by(|a, b| {
        (&conventions[*a].repo, &conventions[*a].rule_id)
            .cmp(&(&conventions[*b].repo, &conventions[*b].rule_id))
    });
    for i in ruled {
        let c = &mut conventions[i];
        let Some(ruling) = effective_ruling(rulings, &c.repo, &c.rule_id) else {
            continue;
        };
        let Some(new_standard) = standard_of_ruling(c, ruling) else {
            continue;
        };
        c.standard = new_standard;
        let before = rows.len();
        rows.retain(|r| {
            !(r.repo == c.repo
                && same_rule(&r.rule_id, &c.rule_id)
                && (is_engine_departure(r) || is_info_kind(r)))
        });
        outcome.removed += before - rows.len();
        let mut new_rows = departure_rows(c, rows);
        // An unchecked place that departs under this ruling and that the scan did not hold (it
        // had no standard then, or the place followed the rule's default) becomes a held
        // question now, so no departure is left out of sight.
        let held: Vec<Finding> = held_place_rows(c, rows, "")
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        let held_any = has_departing_unchecked_place(c);
        new_rows.extend(held);
        if let Some(standard) = usable_standard(c) {
            let any_departure =
                !new_rows.is_empty() || held_any || has_departing_finder_row(c, rows);
            if !any_departure && standard.count.unwrap_or(0) > 0 {
                new_rows.push(observed_row(c, standard));
            }
        } else if c.standard.left_to_client {
            new_rows.push(open_question_row(c));
        }
        if keyed {
            let mut taken: HashSet<String> = rows.iter().map(|r| r.site_key.clone()).collect();
            for f in &mut new_rows {
                f.site_key = site_key_free(f, &taken);
                taken.insert(f.site_key.clone());
            }
        }
        outcome.added += new_rows.len();
        rows.extend(new_rows);
    }
    tag(rows, conventions);
    outcome.changed = true;
    outcome
}

/// Whether a listed place that could not be checked belongs to an option that departs.
fn has_departing_unchecked_place(c: &RuleConvention) -> bool {
    c.options.iter().any(|o| {
        option_status(c, &c.standard, &o.option_id) == PlaceStatus::Departs
            && o.places.iter().any(|p| !p.checked)
    })
}

/// Whether a finder row keyed to the rule departs from the rule's current standard.
fn has_departing_finder_row(c: &RuleConvention, rows: &[Finding]) -> bool {
    rows.iter().any(|r| {
        is_finder_row(r)
            && r.repo == c.repo
            && same_rule(&r.rule_id, &c.rule_id)
            && row_status(c, &c.standard, r) == PlaceStatus::Departs
    })
}

/// The one information row of a rule whose standard is followed with no departure.
fn observed_row(c: &RuleConvention, standard: &OptionCensus) -> Finding {
    let place = standard.places.iter().find(|p| p.checked);
    let (path, line, snippet, located) = match place {
        Some(p) => (
            plain_path(&p.path).to_string(),
            p.line,
            p.text.trim().to_string(),
            true,
        ),
        None => (String::new(), 0, standard.label.clone(), false),
    };
    let reason = match c.standard.source {
        StandardSource::Reviewer => format!(
            "Convention set by the reviewer: \"{}\". No exceptions found.",
            standard.label
        ),
        _ => {
            // Places of the rule's default that sit beside an observed standard are no departure
            // (A2), but they are not "consistent" either: say what is true.
            let by_default: usize = c
                .options
                .iter()
                .filter(|o| {
                    option_status(c, &c.standard, &o.option_id) == PlaceStatus::FollowsRuleDefault
                })
                .map(|o| o.count.unwrap_or(0))
                .sum();
            if by_default == 0 {
                format!(
                    "The codebase consistently follows \"{}\" for {}. Zero violations were observed this scan.",
                    standard.label, c.rule_id
                )
            } else {
                let (places, verb) = if by_default == 1 {
                    ("place", "follows")
                } else {
                    ("places", "follow")
                };
                format!(
                    "The codebase mostly follows \"{}\" for {}. {by_default} {places} {verb} the rule's default instead, which is not counted as a departure.",
                    standard.label, c.rule_id
                )
            }
        }
    };
    let mut f = Finding {
        repo: c.repo.clone(),
        path,
        line,
        rule_id: c.rule_id.clone(),
        severity: "info".to_string(),
        snippet,
        detail: reason,
        located,
        evaluated_option_id: Some(standard.option_id.clone()),
        kind: Some(FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: standard.option_id.clone(),
            convention_label: standard.label.clone(),
            standard_source: Some(c.standard.source),
            differs_from_default: c.standard.differs_from_default.clone(),
        })),
        ..Finding::default()
    };
    crate::onboard::redact::redact_finding(&mut f);
    f
}

/// The one row of a rule the reviewer left to the client: every option and how many places follow
/// each, none suggested.
fn open_question_row(c: &RuleConvention) -> Finding {
    let observed: Vec<ObservedOptionUsage> = c
        .options
        .iter()
        .map(|o| ObservedOptionUsage {
            option_id: o.option_id.clone(),
            label: o.label.clone(),
            sites: o.count,
        })
        .collect();
    let rec = crate::ai_audit::RuleRecommendation {
        rule_id: c.rule_id.clone(),
        repo: c.repo.clone(),
        recommended_option_id: String::new(),
        recommendation_reasoning: String::new(),
        applicable: true,
        no_convention: true,
        observed_options: observed.clone(),
        assessed_sites: c.assessed_places,
        ..crate::ai_audit::RuleRecommendation::default()
    };
    let mut rows = crate::ai_audit::no_convention_findings(&c.repo, &[rec]);
    let mut f = rows.pop().unwrap_or_default();
    f.kind = Some(FindingKind::NoConvention(NoConventionKind {
        observed_options: observed,
        left_to_client: true,
    }));
    f
}

/// The rules that are split with no standard and no choice yet (A3, which Zach ruled a hard
/// hold on 2026-11-20). One function, so the export dialog's count, the release gate's hold and
/// the problem text all read the same set.
pub fn split_rules_without_choice(conventions: &[RuleConvention]) -> Vec<&RuleConvention> {
    conventions
        .iter()
        .filter(|c| {
            c.standard.option_id.is_none()
                && !c.standard.left_to_client
                && c.options
                    .iter()
                    .filter(|o| o.count.unwrap_or(0) > 0)
                    .count()
                    >= 2
        })
        .collect()
}

/// Build the records of a report saved before the convention model from what it has (section 9),
/// then tag its rows. A report with records already, or with no rule that has options, is left
/// alone. The rows keep their rule id, site key and any decision; only their kind can change.
pub fn upgrade(report: &mut crate::onboard::ScanReport) {
    if !report.conventions.is_empty() {
        return;
    }
    let recs: Vec<crate::ai_audit::RuleRecommendation> =
        if report.recommendations_by_repo.is_empty() {
            let mut v: Vec<_> = report.recommendations.values().cloned().collect();
            v.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
            v
        } else {
            report.recommendations_by_repo.clone()
        };
    if recs.is_empty() {
        return;
    }
    let mut out: Vec<RuleConvention> = Vec::new();
    for rec in recs.iter().filter(|r| r.applicable) {
        let Some(rule) = report
            .proposed_rules
            .iter()
            .find(|p| same_rule(&p.id, &rec.rule_id))
        else {
            continue;
        };
        if rule.options.len() < 2 {
            continue;
        }
        let repo = if rec.repo.is_empty() {
            report.repos.first().cloned().unwrap_or_default()
        } else {
            rec.repo.clone()
        };
        let counts: BTreeMap<&str, Option<usize>> = rec
            .observed_options
            .iter()
            .map(|o| (o.option_id.as_str(), o.sites))
            .collect();
        let options: Vec<OptionCensus> = rule
            .options
            .iter()
            .map(|o| OptionCensus {
                option_id: o.id.clone(),
                label: o.label.clone(),
                violates: o.violates,
                count: counts.get(o.id.as_str()).copied().flatten(),
                places: Vec::new(),
            })
            .collect();
        let default = rule.default_option.clone();
        let observed = (!rec.no_convention && !rec.recommended_option_id.is_empty())
            .then(|| rec.recommended_option_id.clone());
        let mut c = RuleConvention {
            repo,
            rule_id: rec.rule_id.clone(),
            default_option_id: default.clone(),
            judged_option_id: default.clone(),
            observed_option_id: observed.clone(),
            assessed_places: rec.assessed_sites,
            places_recorded: false,
            options,
            standard: StandardState::default(),
            finder_rows: Vec::new(),
        };
        c.standard = match &observed {
            Some(o) => StandardState {
                option_id: Some(o.clone()),
                source: StandardSource::Observed,
                differs_from_default: default
                    .as_deref()
                    .filter(|d| *d != o)
                    .map(|d| label_of(&c, d).to_string()),
                ..StandardState::default()
            },
            None => StandardState {
                proposal: default.as_ref().map(|d| Proposal {
                    option_id: d.clone(),
                    basis: ProposalBasis::RuleDefault,
                }),
                ..StandardState::default()
            },
        };
        out.push(c);
    }
    tag(&mut report.findings, &mut out);
    report.conventions = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::decisions::StandardRuling;

    const RULE: &str = "ARCH-PARALLEL-INDEPENDENT-1";
    const REPO: &str = "acme/app";

    fn opt(id: &str, label: &str, count: Option<usize>, places: &[(&str, usize)]) -> OptionCensus {
        OptionCensus {
            option_id: id.into(),
            label: label.into(),
            violates: false,
            count,
            places: places
                .iter()
                .map(|(p, l)| CensusPlace {
                    path: (*p).into(),
                    line: *l,
                    text: format!("code at {p}:{l}"),
                    checked: true,
                    status: PlaceStatus::NoStandard,
                })
                .collect(),
        }
    }

    /// The Hearth parallel rule: default and judged "par", observed "seq", 4 of 5.
    fn hearth(places_recorded: bool) -> RuleConvention {
        RuleConvention {
            repo: REPO.into(),
            rule_id: RULE.into(),
            default_option_id: Some("par".into()),
            judged_option_id: Some("par".into()),
            observed_option_id: Some("seq".into()),
            assessed_places: Some(5),
            places_recorded,
            options: vec![
                opt("par", "Run in parallel", Some(1), &[]),
                opt("seq", "Sequential awaits by default", Some(4), &[]),
            ],
            standard: StandardState {
                option_id: Some("seq".into()),
                source: StandardSource::Observed,
                differs_from_default: Some("Run in parallel".into()),
                ..StandardState::default()
            },
            finder_rows: Vec::new(),
        }
    }

    /// A finder row at `path:line` cited to the rule, keyed to `keyed`.
    fn finder(path: &str, line: usize, keyed: &str, n: u32) -> Finding {
        let mut f = Finding {
            repo: REPO.into(),
            path: path.into(),
            line,
            rule_id: keyed.into(),
            severity: "medium".into(),
            snippet: format!("problem {n} at {path}"),
            detail: "d".into(),
            located: true,
            ..Finding::default()
        };
        f.captures
            .insert(FINDER_PROBLEM_CAPTURE.into(), n.to_string());
        f.captures
            .insert(FINDER_RULES_CAPTURE.into(), format!("{RULE},{keyed}"));
        f.site_key = camerata_api_types::site_key::site_key(REPO, path, keyed, &f.snippet, 0);
        f
    }

    fn four_rows() -> Vec<Finding> {
        vec![
            finder("app/listings/page.tsx", 17, "SPIRIT-OPTIMIZE-1", 34),
            finder("app/admin/page.tsx", 27, "SPIRIT-OPTIMIZE-1", 67),
            finder("app/bookings/[id]/page.tsx", 24, "SPIRIT-OPTIMIZE-1", 68),
            finder("app/api/checkout/route.ts", 16, "SPIRIT-OPTIMIZE-1", 69),
        ]
    }

    fn ruling(choice: StandardChoice) -> StandardRuling {
        StandardRuling {
            id: "std-1".into(),
            repo: REPO.into(),
            rule_id: RULE.into(),
            choice,
            reason: String::new(),
            decided_by: "z".into(),
            decided_at: "t".into(),
            run_id: "run".into(),
            engine_version: "1".into(),
            revoked_at: None,
        }
    }

    fn accept(id: &str) -> StandardRuling {
        ruling(StandardChoice::Accept {
            option_id: id.into(),
        })
    }

    fn departures(rows: &[Finding]) -> Vec<&Finding> {
        rows.iter()
            .filter(|f| f.rule_id == RULE && matches!(f.kind, Some(FindingKind::Deviation(_))))
            .collect()
    }

    #[test]
    fn option_of_row_is_listed_then_only_other_then_unknown() {
        let mut c = hearth(true);
        c.options[1].places = opt("seq", "s", Some(4), &[("a.ts", 10)]).places;
        let mut row = finder("a.ts", 9, "X", 1);
        row.captures
            .insert(FINDER_END_LINE_CAPTURE.into(), "11".into());
        assert_eq!(
            option_of_row(&row, &c),
            (Some("seq".into()), OptionBasis::Listed)
        );
        let elsewhere = finder("b.ts", 3, "X", 2);
        assert_eq!(
            option_of_row(&elsewhere, &c),
            (Some("seq".into()), OptionBasis::OnlyOther)
        );
        // Three options in use: no single other one.
        c.options.push(opt("third", "Third", Some(2), &[]));
        assert_eq!(option_of_row(&elsewhere, &c), (None, OptionBasis::Unknown));
    }

    #[test]
    fn a_finder_row_at_a_place_that_follows_the_observed_standard_is_not_a_departure() {
        let mut c = vec![hearth(false)];
        let mut rows = four_rows();
        tag(&mut rows, &mut c);
        assert!(departures(&rows).is_empty());
        assert_eq!(c[0].finder_rows.len(), 4);
        assert!(
            c[0].finder_rows
                .iter()
                .all(|r| r.status == PlaceStatus::Conforms
                    && r.option_basis == OptionBasis::OnlyOther)
        );
        assert!(departure_rows(&c[0], &rows).is_empty());
    }

    #[test]
    fn a_place_following_the_rule_default_is_never_a_departure_while_the_standard_is_only_observed()
    {
        let mut c = hearth(true);
        c.options[0].places = opt("par", "p", Some(1), &[("a.ts", 1)]).places;
        c.options[1].places = opt("seq", "s", Some(4), &[("b.ts", 2)]).places;
        let mut cs = vec![c];
        let mut rows = Vec::new();
        tag(&mut rows, &mut cs);
        assert_eq!(
            cs[0].options[0].places[0].status,
            PlaceStatus::FollowsRuleDefault
        );
        assert_eq!(cs[0].options[1].places[0].status, PlaceStatus::Conforms);
        assert!(departure_rows(&cs[0], &rows)
            .iter()
            .all(|r| r.path != "a.ts"));
        // Once the reviewer accepts "seq", the default departs.
        let mut rows = Vec::new();
        resolve(&mut rows, &mut cs, &[accept("seq")]);
        assert!(rows
            .iter()
            .any(|r| r.path == "a.ts" && matches!(r.kind, Some(FindingKind::Deviation(_)))));
    }

    #[test]
    fn accepting_the_judged_option_makes_the_four_finder_places_departures() {
        let mut cs = vec![hearth(false)];
        let mut rows = four_rows();
        let out = resolve(&mut rows, &mut cs, &[accept("par")]);
        assert!(out.changed);
        let dep = departures(&rows);
        assert_eq!(dep.len(), 4, "{rows:?}");
        for d in &dep {
            let FindingKind::Deviation(k) = d.kind.clone().unwrap() else {
                panic!()
            };
            assert_eq!(k.standard_source, Some(StandardSource::Reviewer));
            assert_eq!(k.convention_label, "Run in parallel");
            assert_eq!((k.deviating_sites, k.assessed_sites), (4, Some(5)));
        }
        // The four finder rows are unchanged.
        assert!(
            rows.iter()
                .filter(|f| f.rule_id == "SPIRIT-OPTIMIZE-1")
                .count()
                == 4
        );
        // Applying twice gives the same keys.
        let mut again = four_rows();
        let mut cs2 = vec![hearth(false)];
        resolve(&mut again, &mut cs2, &[accept("par")]);
        let keys =
            |r: &[Finding]| -> Vec<String> { r.iter().map(|f| f.site_key.clone()).collect() };
        assert_eq!(keys(&rows), keys(&again));
    }

    #[test]
    fn with_no_rulings_nothing_changes_and_a_withdrawn_one_equals_none() {
        let mut cs = vec![hearth(false)];
        let mut rows = four_rows();
        let out = resolve(&mut rows, &mut cs, &[]);
        assert!(!out.changed);
        assert_eq!(rows, four_rows());
        let mut withdrawn = accept("par");
        withdrawn.revoked_at = Some("t".into());
        let out = resolve(&mut rows, &mut cs, &[withdrawn]);
        assert!(!out.changed);
        assert_eq!(rows, four_rows());
    }

    fn trigger_env() -> RuleConvention {
        RuleConvention {
            repo: REPO.into(),
            rule_id: "ARCH-TRIGGER-ENV-1".into(),
            default_option_id: Some("git-ref".into()),
            judged_option_id: Some("git-ref".into()),
            observed_option_id: None,
            assessed_places: Some(2),
            places_recorded: true,
            options: vec![
                opt("git-ref", "Git ref", Some(1), &[("ci.yml", 3)]),
                opt("manual", "Manual", Some(1), &[("deploy.yml", 8)]),
            ],
            standard: StandardState {
                proposal: Some(Proposal {
                    option_id: "git-ref".into(),
                    basis: ProposalBasis::RuleDefault,
                }),
                ..StandardState::default()
            },
            finder_rows: Vec::new(),
        }
    }

    #[test]
    fn accepting_on_a_split_swaps_the_open_row_for_one_departure_and_leave_to_client_keeps_one_question(
    ) {
        let mut no_conv = Finding {
            repo: REPO.into(),
            rule_id: "ARCH-TRIGGER-ENV-1".into(),
            severity: "info".into(),
            snippet: "x".into(),
            kind: Some(FindingKind::NoConvention(NoConventionKind {
                observed_options: vec![],
                left_to_client: false,
            })),
            ..Finding::default()
        };
        no_conv.site_key = "nk".into();
        let mut rows = vec![no_conv.clone()];
        let mut cs = vec![trigger_env()];
        let mut r = accept("git-ref");
        r.rule_id = "ARCH-TRIGGER-ENV-1".into();
        resolve(&mut rows, &mut cs, &[r]);
        assert!(!rows
            .iter()
            .any(|f| matches!(f.kind, Some(FindingKind::NoConvention(_)))));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].path, "deploy.yml");
        assert_eq!(cs[0].standard.conforming_places, 1);

        let mut rows = vec![no_conv];
        let mut cs = vec![trigger_env()];
        let mut l = ruling(StandardChoice::LeaveToClient);
        l.rule_id = "ARCH-TRIGGER-ENV-1".into();
        resolve(&mut rows, &mut cs, &[l]);
        assert_eq!(rows.len(), 1);
        match &rows[0].kind {
            Some(FindingKind::NoConvention(k)) => assert!(k.left_to_client),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn places_counted_but_not_listed_become_one_row_with_no_location() {
        let mut c = hearth(true);
        c.options[1] = opt(
            "seq",
            "Sequential",
            Some(15),
            &[
                ("a.ts", 1),
                ("a.ts", 2),
                ("a.ts", 3),
                ("a.ts", 4),
                ("a.ts", 5),
                ("a.ts", 6),
                ("a.ts", 7),
                ("a.ts", 8),
                ("a.ts", 9),
                ("a.ts", 10),
            ],
        );
        c.assessed_places = Some(16);
        let mut cs = vec![c];
        let mut rows = Vec::new();
        resolve(&mut rows, &mut cs, &[accept("par")]);
        let rem: Vec<_> = rows.iter().filter(|f| f.path.is_empty()).collect();
        assert_eq!(rem.len(), 1);
        assert!(
            rem[0].snippet.starts_with("5 more places follow"),
            "{}",
            rem[0].snippet
        );
        assert_eq!(cs[0].standard.departing_places, 15);
    }

    fn old_report() -> crate::onboard::ScanReport {
        let mut report = crate::onboard::ScanReport::gated(&[REPO.to_string()]);
        let view = |id: &str, label: &str| crate::onboard::RuleOptionView {
            id: id.into(),
            label: label.into(),
            directive: "d".into(),
            why: String::new(),
            violates: false,
        };
        let mut p = |id: &str, def: &str, opts: Vec<crate::onboard::RuleOptionView>| {
            let mut v = serde_json::json!({"id": id, "title": id, "kind": "review", "verification": "draft", "scope": "repo-local", "enforcement_point": "content", "repos": [], "placement": "p", "finding_count": 0, "recommended": false});
            v["default_option"] = serde_json::json!(def);
            let mut pr: crate::onboard::ProposedRule = serde_json::from_value(v).unwrap();
            pr.options = opts;
            report.proposed_rules.push(pr);
        };
        p(
            RULE,
            "par",
            vec![
                view("par", "Run in parallel"),
                view("seq", "Sequential awaits by default"),
            ],
        );
        p(
            "ARCH-TRIGGER-ENV-1",
            "git-ref",
            vec![view("git-ref", "Git ref"), view("manual", "Manual")],
        );
        report.recommendations_by_repo = vec![
            crate::ai_audit::RuleRecommendation {
                rule_id: RULE.into(),
                repo: REPO.into(),
                recommended_option_id: "seq".into(),
                assessed_sites: Some(5),
                ..crate::ai_audit::RuleRecommendation::default()
            },
            crate::ai_audit::RuleRecommendation {
                rule_id: "ARCH-TRIGGER-ENV-1".into(),
                repo: REPO.into(),
                recommended_option_id: "git-ref".into(),
                no_convention: true,
                assessed_sites: Some(2),
                observed_options: vec![
                    ObservedOptionUsage {
                        option_id: "git-ref".into(),
                        label: "Git ref".into(),
                        sites: Some(1),
                    },
                    ObservedOptionUsage {
                        option_id: "manual".into(),
                        label: "Manual".into(),
                        sites: Some(1),
                    },
                ],
                ..crate::ai_audit::RuleRecommendation::default()
            },
        ];
        // The saved rows: keyed to the rule, tagged as departures from "sequential".
        for mut f in four_rows() {
            f.rule_id = RULE.into();
            f.site_key = camerata_api_types::site_key::site_key(REPO, &f.path, RULE, &f.snippet, 0);
            f.kind = Some(FindingKind::Deviation(DeviationKind {
                deviating_sites: 4,
                assessed_sites: Some(5),
                convention_option_id: Some("seq".into()),
                convention_label: "Sequential awaits by default".into(),
                standard_source: None,
                differs_from_default: None,
                following_sites: None,
            }));
            report.findings.push(f);
        }
        report
    }

    #[test]
    fn upgrading_an_old_report_keeps_the_keys_and_stops_calling_the_rows_deviations() {
        let mut report = old_report();
        let keys: Vec<String> = report.findings.iter().map(|f| f.site_key.clone()).collect();
        upgrade(&mut report);
        assert_eq!(report.conventions.len(), 2);
        assert!(report.conventions.iter().all(|c| !c.places_recorded));
        assert_eq!(
            keys,
            report
                .findings
                .iter()
                .map(|f| f.site_key.clone())
                .collect::<Vec<_>>()
        );
        assert!(report
            .findings
            .iter()
            .all(|f| f.kind == Some(FindingKind::Defect)));
        let hearth = report
            .conventions
            .iter()
            .find(|c| c.rule_id == RULE)
            .unwrap();
        assert!(hearth
            .finder_rows
            .iter()
            .all(|r| r.option_basis == OptionBasis::Unknown));
        let env = report
            .conventions
            .iter()
            .find(|c| c.rule_id == "ARCH-TRIGGER-ENV-1")
            .unwrap();
        assert_eq!(
            env.standard.proposal.as_ref().unwrap().basis,
            ProposalBasis::RuleDefault
        );
    }

    #[test]
    fn upgrade_then_accept_makes_the_same_four_keys_departures() {
        let mut report = old_report();
        let keys: Vec<String> = report.findings.iter().map(|f| f.site_key.clone()).collect();
        upgrade(&mut report);
        let mut rows = report.findings.clone();
        resolve(&mut rows, &mut report.conventions, &[accept("par")]);
        let dep: Vec<String> = departures(&rows)
            .iter()
            .map(|f| f.site_key.clone())
            .collect();
        assert_eq!(dep.len(), 4);
        for k in &keys {
            assert!(dep.contains(k), "{k}");
        }
    }

    #[test]
    fn upgrade_of_the_trigger_env_shape_plus_accept_gives_one_row_with_no_location() {
        let mut report = old_report();
        report.findings.clear();
        upgrade(&mut report);
        let mut rows = Vec::new();
        let mut r = accept("git-ref");
        r.rule_id = "ARCH-TRIGGER-ENV-1".into();
        resolve(&mut rows, &mut report.conventions, &[r]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].path.is_empty());
        assert!(rows[0]
            .snippet
            .starts_with("1 more place follows \"Manual\""));
    }

    /// Design section 3: a finder row whose option is unknown departs only when the standard IS the
    /// judged option (it broke that option by definition); against any other standard it is the
    /// finder's own problem and stays so.
    #[test]
    fn a_finder_row_with_an_unknown_option_departs_only_from_the_judged_option() {
        let three = || {
            let mut c = hearth(false);
            c.observed_option_id = None;
            c.standard = StandardState::default();
            c.options = vec![
                opt("par", "Run in parallel", Some(1), &[]),
                opt("seq", "Sequential awaits by default", Some(2), &[]),
                opt("third", "Third way", Some(2), &[]),
            ];
            c
        };
        let row = || vec![finder("elsewhere.ts", 5, RULE, 1)];
        assert_eq!(
            option_of_row(&row()[0], &three()),
            (None, OptionBasis::Unknown)
        );
        // The standard is the judged option: the row departs.
        let (mut rows, mut cs) = (row(), vec![three()]);
        resolve(&mut rows, &mut cs, &[accept("par")]);
        let is_departure = |rows: &[Finding]| {
            rows.iter().any(|r| {
                r.path == "elsewhere.ts" && matches!(r.kind, Some(FindingKind::Deviation(_)))
            })
        };
        assert!(is_departure(&rows), "{rows:?}");
        assert_eq!(cs[0].finder_rows[0].status, PlaceStatus::Departs);
        // The standard is another option: the row is the finder's own problem again.
        let (mut rows, mut cs) = (row(), vec![three()]);
        resolve(&mut rows, &mut cs, &[accept("seq")]);
        assert!(!is_departure(&rows), "{rows:?}");
        assert_eq!(cs[0].finder_rows[0].status, PlaceStatus::NoStandard);
    }

    #[test]
    fn a_split_with_no_choice_is_counted_once_by_the_shared_function() {
        let split = vec![trigger_env(), hearth(false)];
        assert_eq!(split_rules_without_choice(&split).len(), 1);
        let mut left = trigger_env();
        left.standard.left_to_client = true;
        assert!(split_rules_without_choice(&[left]).is_empty());
    }

    fn unchecked(mut o: OptionCensus) -> OptionCensus {
        for p in &mut o.places {
            p.checked = false;
            p.text = String::new();
        }
        o
    }

    fn held_for<'a>(rows: &'a [Finding], path: &str, line: usize) -> Vec<&'a Finding> {
        rows.iter()
            .filter(|f| f.rule_id == "AI-OPEN-QUESTION" && f.path == path && f.line == line)
            .collect()
    }

    /// Review item 1: a split has no standard at scan time, so a listed place that could not be
    /// checked got no held question. Once the reviewer accepts the other option it departs, and
    /// it must come back as a held question (not vanish into the remainder count).
    #[test]
    fn an_unchecked_place_that_departs_after_accepting_on_a_split_becomes_a_held_question() {
        let mut c = trigger_env();
        c.options[1] = unchecked(c.options[1].clone());
        let mut rows = Vec::new();
        let mut cs = vec![c];
        let mut r = accept("git-ref");
        r.rule_id = "ARCH-TRIGGER-ENV-1".into();
        resolve(&mut rows, &mut cs, &[r]);
        assert_eq!(held_for(&rows, "deploy.yml", 8).len(), 1, "{rows:?}");
        assert!(
            rows.iter().all(|f| !f.path.is_empty()),
            "no remainder: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|f| matches!(f.kind, Some(FindingKind::ConventionObserved(_)))),
            "a departure exists, so the rule is not 'followed with zero violations': {rows:?}"
        );
    }

    /// Review item 1, the A2 case: a place of the rule's default is no departure while the standard
    /// is only observed; the reviewer then accepts the observed option and it departs.
    #[test]
    fn an_unchecked_rule_default_place_becomes_a_held_question_after_accepting_the_observed_option()
    {
        let mut c = hearth(true);
        c.options[0] = unchecked(opt("par", "Run in parallel", Some(1), &[("x.ts", 3)]));
        c.options[1] = opt(
            "seq",
            "Sequential awaits by default",
            Some(4),
            &[("a.ts", 1)],
        );
        let mut rows = Vec::new();
        let mut cs = vec![c];
        resolve(&mut rows, &mut cs, &[accept("seq")]);
        assert_eq!(held_for(&rows, "x.ts", 3).len(), 1, "{rows:?}");
        // Asked twice (the scan already holds it): still one.
        let mut again = rows.clone();
        let mut cs2 = cs.clone();
        resolve(&mut again, &mut cs2, &[accept("seq")]);
        assert_eq!(held_for(&again, "x.ts", 3).len(), 1, "{again:?}");
    }

    /// Review item 2: a conforming finder row whose line range also holds a place of a departing
    /// option must not hide that place.
    #[test]
    fn a_conforming_finder_row_does_not_swallow_a_departing_place_in_its_range() {
        let mut c = hearth(true);
        c.options = vec![
            opt(
                "seq",
                "Sequential awaits by default",
                Some(1),
                &[("a.ts", 10)],
            ),
            opt("par", "Run in parallel", Some(1), &[("a.ts", 11)]),
        ];
        c.assessed_places = Some(2);
        let mut row = finder("a.ts", 10, "SPIRIT-OPTIMIZE-1", 1);
        row.captures.insert(
            crate::ai_audit::agent_finder::reply::FINDER_END_LINE_CAPTURE.into(),
            "12".into(),
        );
        let mut rows = vec![row];
        let mut cs = vec![c];
        resolve(&mut rows, &mut cs, &[accept("seq")]);
        assert_eq!(
            cs[0].finder_rows[0].status,
            PlaceStatus::Departs,
            "{:?}",
            cs[0].finder_rows
        );
        assert!(
            rows.iter().any(|f| f.rule_id == RULE
                && f.captures.contains_key(CONVENTION_PLACE_CAPTURE)
                && f.path == "a.ts"),
            "the departing place is still a row: {rows:?}"
        );
    }

    /// Review item 14: the information row of an observed standard says "consistently, zero
    /// violations" only when no place follows the rule's default instead.
    #[test]
    fn the_observed_row_does_not_claim_consistency_beside_places_of_the_rule_default() {
        let c = hearth(true);
        let standard = c.options[1].clone();
        let text = observed_row(&c, &standard).detail;
        assert!(
            !text.contains("consistently") && !text.contains("Zero violations"),
            "{text}"
        );
        assert!(
            text.contains("1 place follows the rule's default instead"),
            "{text}"
        );
        let mut clean = hearth(true);
        clean.options[0].count = Some(0);
        let text = observed_row(&clean, &clean.options[1].clone()).detail;
        assert!(text.contains("consistently follows"), "{text}");
    }
}
