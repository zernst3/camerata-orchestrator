//! Conventions from answers (design section 3). The alternatives stage is skipped under the agent
//! finder, so its job is done here from the checklist answers: the option counts of a repository's
//! pieces are added, an option with at least two thirds of the counted places is the convention,
//! and a decision rule with no such option has no convention. The result is the same
//! [`RuleRecommendation`] record the alternatives stage writes, so the existing functions
//! (`apply_alternative_decision`, `assign_deviation_kinds`, `conventions_observed_findings`,
//! `no_convention_findings`) run unchanged on it.
//!
//! One [`RuleConvention`] record is built for each rule with options (convention model design,
//! section 5): the options, the places the agent listed for each (checked against the file here),
//! and the standard (the observed two thirds convention, else none; a rule with a correct default
//! falls back to it). The departure rows follow from the record and the finder's own rows
//! ([`departure_and_held_rows`]); the conventions module works out which places depart.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::super::checklist::{combine_for_unit, Effective, EffectiveAnswer, Place};
use camerata_api_types::audit::{
    CensusPlace, OptionCensus, PlaceStatus, Proposal, ProposalBasis, RuleConvention,
    StandardSource, StandardState,
};

use crate::ai_audit::{RuleAlternatives, RuleRecommendation};
use crate::onboard::Finding;
use crate::scan_routing::AuditRule;

/// What the conventions step produced.
#[derive(Debug, Default)]
pub(super) struct ConventionResult {
    pub recommendations: Vec<RuleRecommendation>,
    /// One convention record for each rule with options that was counted.
    pub records: Vec<RuleConvention>,
    /// The defect problem of a rule whose majority option is marked `violates`.
    pub violates: Vec<Finding>,
    /// Operator-only remarks (a listed place that could not be checked).
    pub notes: Vec<String>,
}

/// A place checked against the repository: the path, the line and the text of the line.
pub(super) fn checked_place(
    files: &HashMap<&str, Vec<&str>>,
    place: &Place,
) -> Option<(String, usize, String)> {
    let line = place.line?;
    let lines = files.get(place.path.trim_start_matches("./"))?;
    let text = lines.get(line.checked_sub(1)?)?;
    // A blank line cites no code: it cannot be checked.
    if text.trim().is_empty() {
        return None;
    }
    Some((
        place.path.trim_start_matches("./").to_string(),
        line,
        (*text).to_string(),
    ))
}

/// The problem needing a decision for a further place of a problem that could not be checked
/// against the file (no such line, or a blank one).
pub(super) fn held_problem_place_row(
    repo: &str,
    title: &str,
    place: &super::super::reply::ProblemPlace,
) -> Finding {
    let title = crate::client_text::plain(title.trim());
    let mut held = Finding {
        repo: repo.to_string(),
        path: place.path.trim_start_matches("./").to_string(),
        line: place.line,
        rule_id: "AI-OPEN-QUESTION".to_string(),
        severity: "low".to_string(),
        // The title is in the snippet so the later same-place merge, which compares snippets,
        // keeps one held row for each problem that listed the place.
        snippet: format!("The place listed for \"{title}\" could not be checked"),
        detail: format!(
            "A place could not be checked{}A reviewer listed this place as another place of \"{title}\", but it could not be found in the file. It needs a decision.",
            crate::ai_audit::digest_parse::HEADLINE_SEP,
        ),
        needs_review: true,
        located: false,
        ..Finding::default()
    };
    crate::onboard::redact::redact_finding(&mut held);
    held
}

/// Build the recommendations, departures and defect rows from the answers of a repository's
/// pieces. `piece_answers` holds the effective answers of each piece that gave a checklist.
pub(super) fn conventions_from_answers(
    repo: &str,
    alternatives: &[RuleAlternatives],
    piece_answers: &[Vec<EffectiveAnswer>],
    files: &[(String, String)],
    selected: &[AuditRule],
    corpus: &camerata_rules::RuleSet,
    problem_rows: &std::collections::HashSet<(usize, u32)>,
) -> ConventionResult {
    let mut result = ConventionResult::default();
    let by_path: HashMap<&str, Vec<&str>> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.lines().collect::<Vec<_>>()))
        .collect();
    // Keep the options that exist on the rule, from answers that were accepted.
    let valid_options = |rule: &str, id: &str| -> bool {
        alternatives
            .iter()
            .find(|a| a.rule_id.eq_ignore_ascii_case(rule))
            .is_some_and(|a| a.options.iter().any(|o| o.id == id))
    };
    let filtered: Vec<Vec<EffectiveAnswer>> = piece_answers
        .iter()
        .map(|answers| {
            answers
                .iter()
                // Option counts of a `not_a_problem_here` answer are dropped on purpose: a place the
                // model judged harmless is not evidence of how the unit does the thing.
                .filter(|a| matches!(a.state, Effective::Problems(_) | Effective::Clean(_)))
                .map(|a| {
                    let mut a = a.clone();
                    a.options.retain(|o| valid_options(&a.rule, &o.id));
                    a
                })
                .collect()
        })
        .collect();
    // The listed places of each option, over the pieces.
    let mut places: BTreeMap<(String, String), Vec<Place>> = BTreeMap::new();
    for answers in &filtered {
        for a in answers {
            for o in &a.options {
                let list = places
                    .entry((a.rule.to_ascii_uppercase(), o.id.clone()))
                    .or_default();
                for p in &o.places {
                    if !list.contains(p) && list.len() < 10 {
                        list.push(p.clone());
                    }
                }
            }
        }
    }
    // A rule whose answer cites a problem the model wrote: that problem is a finder row of the
    // rule. Only a problem that has a row can carry the places; with none, the engine's own rows
    // stand so no place vanishes.
    let mut cited_by_rule: HashSet<String> = HashSet::new();
    for (i, answers) in filtered.iter().enumerate() {
        for a in answers {
            if let Effective::Problems(nums) = &a.state {
                if nums.iter().any(|n| problem_rows.contains(&(i, *n))) {
                    cited_by_rule.insert(a.rule.to_ascii_uppercase());
                }
            }
        }
    }
    let mut effective_selected: Vec<AuditRule> = selected.to_vec();
    for uc in combine_for_unit(&filtered) {
        let Some(alt) = alternatives
            .iter()
            .find(|a| a.rule_id.eq_ignore_ascii_case(&uc.rule))
        else {
            continue;
        };
        if uc.total == 0 {
            continue;
        }
        let label_of = |id: &str| -> String {
            alt.options
                .iter()
                .find(|o| o.id == id)
                .map_or_else(|| id.to_string(), |o| o.label.clone())
        };
        let decision = alt.kind == Some(camerata_rules::RuleKind::Decision);
        let default_option = corpus
            .get_by_id(&uc.rule)
            .and_then(|r| r.default_option.clone());
        let plurality = uc
            .counts
            .iter()
            .max_by_key(|(id, n)| (**n, std::cmp::Reverse((*id).clone())))
            .map(|(id, _)| id.clone());
        let observed: Vec<camerata_api_types::audit::ObservedOptionUsage> = uc
            .counts
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(id, n)| camerata_api_types::audit::ObservedOptionUsage {
                option_id: id.clone(),
                label: label_of(id),
                sites: Some(*n),
            })
            .collect();
        // The standard (D1, A4): the observed two thirds option, else none. A rule that is not a
        // pure decision has a correct default, so with no observed option it falls back to the
        // project's chosen option, else the most common one, and is never "no convention".
        let (standard_id, source) = match &uc.convention {
            Some(id) => (Some(id.clone()), StandardSource::Observed),
            None if decision => (None, StandardSource::None),
            // `selected_option_id` is the project's chosen option, else the rule's default. The
            // source says which: the default itself, an earlier choice that is a different
            // option, or (neither exists) the option most places follow.
            None => match alt.selected_option_id.clone() {
                Some(id) if default_option.as_deref() == Some(id.as_str()) => {
                    (Some(id), StandardSource::RuleDefault)
                }
                Some(id) => (Some(id), StandardSource::EarlierChoice),
                None => (plurality.clone(), StandardSource::MostCommon),
            },
        };
        let mut record = RuleConvention {
            repo: repo.to_string(),
            rule_id: uc.rule.clone(),
            default_option_id: default_option.clone(),
            // What the finder's directive was: the project's chosen option, else the default,
            // for every kind of rule. (A rule with a default is judged against it even when
            // most of the code follows another option, so its rows are not all departures from
            // the observed standard.)
            judged_option_id: alt.selected_option_id.clone(),
            observed_option_id: uc.convention.clone(),
            assessed_places: Some(uc.total),
            places_recorded: true,
            options: alt
                .options
                .iter()
                .map(|o| OptionCensus {
                    option_id: o.id.clone(),
                    label: o.label.clone(),
                    violates: o.violates,
                    count: Some(uc.counts.get(&o.id).copied().unwrap_or(0)),
                    places: places
                        .get(&(uc.rule.clone(), o.id.clone()))
                        .map(|list| {
                            list.iter()
                                .map(|p| census_place(&by_path, p))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
            standard: StandardState {
                option_id: standard_id.clone(),
                source,
                differs_from_default: standard_id
                    .as_deref()
                    .zip(default_option.as_deref())
                    .filter(|(s, d)| s != d)
                    .map(|(_, d)| label_of(d)),
                proposal: if standard_id.is_none() {
                    let chosen_earlier = alt
                        .selected_option_id
                        .clone()
                        .filter(|c| Some(c) != default_option.as_ref());
                    match (chosen_earlier, default_option.clone()) {
                        (Some(c), _) => Some(Proposal {
                            option_id: c,
                            basis: ProposalBasis::EarlierChoice,
                        }),
                        (None, Some(d)) => Some(Proposal {
                            option_id: d,
                            basis: ProposalBasis::RuleDefault,
                        }),
                        (None, None) => None,
                    }
                } else {
                    None
                },
                ..StandardState::default()
            },
            finder_rows: Vec::new(),
        };
        // Everything the agent listed is kept honestly: the total is the sum of the counts.
        let first_place = |option: &str| -> Option<(String, usize, String)> {
            places
                .get(&(uc.rule.clone(), option.to_string()))
                .and_then(|list| list.iter().find_map(|p| checked_place(&by_path, p)))
        };
        let Some(chosen) = standard_id else {
            // No convention on a decision rule: one information row, no departures. The
            // recommendation names no option (the proposal lives in the record).
            result.recommendations.push(RuleRecommendation {
                repo: repo.to_string(),
                rule_id: uc.rule.clone(),
                recommended_option_id: String::new(),
                recommendation_reasoning: format!(
                    "The {} places checked are split between the options, and no option holds \
                     two thirds of them.",
                    uc.total
                ),
                no_convention: true,
                observed_options: observed,
                assessed_sites: Some(uc.total),
                ..RuleRecommendation::default()
            });
            result.records.push(record);
            continue;
        };
        let count = uc.counts.get(&chosen).copied().unwrap_or(0);
        let evidence = first_place(&chosen).map(|(p, l, _)| format!("{p}:{l}"));
        let reasoning = format!(
            "{count} of the {} places checked follow \"{}\".",
            uc.total,
            label_of(&chosen)
        );
        result.recommendations.push(RuleRecommendation {
            repo: repo.to_string(),
            rule_id: uc.rule.clone(),
            recommended_option_id: chosen.clone(),
            recommendation_reasoning: reasoning.clone(),
            evidence: evidence.clone(),
            assessed_sites: Some(uc.total),
            // Only an affirmative count licenses "the codebase follows it".
            currently_followed: Some(count > 0),
            ..RuleRecommendation::default()
        });
        let Some(opt) = alt.options.iter().find(|o| o.id == chosen) else {
            result.records.push(record);
            continue;
        };
        if opt.violates {
            // The codebase follows the rule's own named defect: one defect problem, unless a
            // problem the model wrote already reports the rule.
            if !cited_by_rule.contains(&uc.rule) {
                if let Some(f) = crate::ai_audit::routing::apply_alternative_decision(
                    &mut effective_selected,
                    repo,
                    &uc.rule,
                    opt,
                    &reasoning,
                    evidence.as_deref(),
                ) {
                    result.violates.push(f);
                }
            }
            record.standard.proposal = None;
            result.records.push(record);
            continue;
        }
        crate::ai_audit::routing::apply_alternative_decision(
            &mut effective_selected,
            repo,
            &uc.rule,
            opt,
            &reasoning,
            evidence.as_deref(),
        );
        result.records.push(record);
    }
    result
}

/// A place the agent listed, checked against the repository: the line of code redacted and capped
/// at 200 characters, or unchecked when the file or line is not there.
fn census_place(files: &HashMap<&str, Vec<&str>>, place: &Place) -> CensusPlace {
    match checked_place(files, place) {
        Some((path, line, text)) => CensusPlace {
            path,
            line,
            text: crate::onboard::redact::snippet_from_line(&text, 200),
            checked: true,
            status: PlaceStatus::NoStandard,
        },
        None => CensusPlace {
            path: place.path.trim_start_matches("./").to_string(),
            line: place.line.unwrap_or(0),
            text: String::new(),
            checked: false,
            status: PlaceStatus::NoStandard,
        },
    }
}

/// The rows the records ask for beside the finder's own: a departure row for each place of an
/// option that departs from the standard (unless a finder row covers it), and a problem needing a
/// decision for each such place that could not be checked. `rows` are all rows so far.
pub(super) fn departure_and_held_rows(
    records: &[RuleConvention],
    rows: &[Finding],
    corpus: &camerata_rules::RuleSet,
) -> (Vec<Finding>, Vec<Finding>, Vec<String>) {
    let mut departures = Vec::new();
    let mut held = Vec::new();
    let mut notes = Vec::new();
    for c in records {
        departures.extend(crate::conventions::departure_rows(c, rows));
        if c.standard.option_id.is_none() {
            continue;
        }
        let title = corpus
            .get_by_id(&c.rule_id)
            .map_or_else(|| c.rule_id.clone(), |r| r.title.clone());
        for (option, row) in crate::conventions::held_place_rows(c, rows, &title) {
            notes.push(format!(
                "{}: a listed place of the option \"{option}\" ({}{}) could not be checked against the file",
                c.rule_id,
                row.path,
                if row.line > 0 { format!(":{}", row.line) } else { String::new() },
            ));
            held.push(row);
        }
    }
    (departures, held, notes)
}
