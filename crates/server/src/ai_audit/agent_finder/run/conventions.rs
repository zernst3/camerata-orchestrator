//! Conventions from answers (design section 3). The alternatives stage is skipped under the agent
//! finder, so its job is done here from the checklist answers: the option counts of a repository's
//! pieces are added, an option with at least two thirds of the counted places is the convention,
//! and a decision rule with no such option has no convention. The result is the same
//! [`RuleRecommendation`] record the alternatives stage writes, so the existing functions
//! (`apply_alternative_decision`, `assign_deviation_kinds`, `conventions_observed_findings`,
//! `no_convention_findings`) run unchanged on it.
//!
//! The listed places of an option that is not the convention become departures: one problem for
//! each place, from the agent's own listing and checked against the file here.

use std::collections::{BTreeMap, HashMap};

use super::super::checklist::{combine_for_unit, Effective, EffectiveAnswer, Place};
use crate::ai_audit::{RuleAlternatives, RuleRecommendation};
use crate::onboard::Finding;
use crate::scan_routing::AuditRule;

/// What the conventions step produced.
#[derive(Debug, Default)]
pub(super) struct ConventionResult {
    pub recommendations: Vec<RuleRecommendation>,
    /// One problem for each listed place of an option that is not the convention.
    pub departures: Vec<Finding>,
    /// The defect problem of a rule whose majority option is marked `violates`.
    pub violates: Vec<Finding>,
    /// Operator-only remarks (a listed place that could not be checked).
    pub notes: Vec<String>,
}

/// A place checked against the repository: the path, the line and the text of the line.
fn checked_place(
    files: &HashMap<&str, Vec<&str>>,
    place: &Place,
) -> Option<(String, usize, String)> {
    let line = place.line?;
    let lines = files.get(place.path.trim_start_matches("./"))?;
    let text = lines.get(line.checked_sub(1)?)?;
    Some((
        place.path.trim_start_matches("./").to_string(),
        line,
        (*text).to_string(),
    ))
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
        // The option the rule is judged against: the two thirds majority, else (for a rule that
        // has a correct default) the project's chosen option or the most common one.
        let chosen: Option<String> = match &uc.convention {
            Some(id) => Some(id.clone()),
            None if decision => None,
            None => alt.selected_option_id.clone().or(plurality.clone()),
        };
        let first_place = |option: &str| -> Option<(String, usize, String)> {
            places
                .get(&(uc.rule.clone(), option.to_string()))
                .and_then(|list| list.iter().find_map(|p| checked_place(&by_path, p)))
        };
        let Some(chosen) = chosen else {
            // No convention on a decision rule: one information row, no departures.
            result.recommendations.push(RuleRecommendation {
                repo: repo.to_string(),
                rule_id: uc.rule.clone(),
                recommended_option_id: alt
                    .selected_option_id
                    .clone()
                    .or_else(|| alt.options.first().map(|o| o.id.clone()))
                    .unwrap_or_default(),
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
            continue;
        };
        if opt.violates {
            // The codebase follows the rule's own named defect: one defect problem.
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
        let title = corpus
            .get_by_id(&uc.rule)
            .map_or_else(|| uc.rule.clone(), |r| r.title.clone());
        for ((rule, option), listed) in &places {
            if rule != &uc.rule || option == &chosen {
                continue;
            }
            for place in listed {
                let Some((path, line, text)) = checked_place(&by_path, place) else {
                    result.notes.push(format!(
                        "{}: a listed place of the option \"{}\" ({}{}) could not be checked against the file",
                        uc.rule,
                        label_of(option),
                        place.path,
                        place.line.map_or(String::new(), |l| format!(":{l}")),
                    ));
                    continue;
                };
                let mut finding = Finding {
                    repo: repo.to_string(),
                    path,
                    line,
                    rule_id: uc.rule.clone(),
                    severity: "low".to_string(),
                    snippet: text.trim().to_string(),
                    detail: format!(
                        "This place uses \"{}\" for {title}. The rest of the codebase follows \"{}\".",
                        label_of(option),
                        label_of(&chosen)
                    ),
                    located: true,
                    ..Finding::default()
                };
                crate::onboard::redact::redact_finding(&mut finding);
                result.departures.push(finding);
            }
        }
    }
    result
}
