//! The convention model as the review screens show it (design 2026-11-20, section 8): what the
//! engine found for one rule with options, where the standard came from, the places by status and
//! the controls that rule on it. Pure, so SSR renders every state and the words are tested here.
//! Words used: place, option, observed convention, standard, proposal, departure.

use camerata_api_types::audit::{
    OptionBasis, PlaceStatus, ProposalBasis, RuleConvention, StandardSource,
};

/// The effect text of "Accept" and of choosing another option.
pub const ACCEPT_EFFECT: &str = "Sets this option as the convention for this inspection. Places that use another option go to the Queue as departures.";
/// The label of the leave-it-to-the-client control.
pub const LEAVE_LABEL: &str = "No recommendation, leave it to the client";
/// What leaving it to the client does.
pub const LEAVE_EFFECT: &str =
    "The report lists the options and the places that follow each, with none suggested.";
/// Printed when the scan did not record places (a report saved before the model).
pub const PLACES_NOT_RECORDED: &str = "The places were not recorded for this scan.";

/// One place in a list: where it is and what is known about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlaceLine {
    pub repo: String,
    pub path: String,
    pub line: usize,
    /// The line of code, or the finder's note.
    pub note: String,
}

/// A titled list of places.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlaceGroup {
    pub title: String,
    pub places: Vec<PlaceLine>,
}

/// One control that rules on the standard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RulingControl {
    pub label: String,
    pub effect: &'static str,
    /// The option the ruling sets; `None` for leaving it to the client.
    pub option_id: Option<String>,
}

/// Everything the strip and the Rule alternatives row print for one rule with options.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConventionView {
    /// "Observed: N of M places follow ..." or "Split: ...".
    pub engine_line: String,
    /// Where the standard or proposal came from.
    pub origin: String,
    pub differs_line: Option<String>,
    pub groups: Vec<PlaceGroup>,
    /// Set when the scan did not record places.
    pub not_recorded: Option<&'static str>,
    /// "Accept: X" for the observed convention or the proposal.
    pub accept: Option<RulingControl>,
    /// The other options, each with the same effect.
    pub others: Vec<RulingControl>,
    pub leave: RulingControl,
    /// The live ruling to withdraw.
    pub withdraw: Option<String>,
}

fn label<'a>(c: &'a RuleConvention, id: &'a str) -> &'a str {
    c.options
        .iter()
        .find(|o| o.option_id == id)
        .map_or(id, |o| o.label.as_str())
}

/// Where the standard or proposal came from, in words ("AI recommended" is never one of them).
pub fn origin_label(c: &RuleConvention) -> String {
    if c.standard.left_to_client {
        return "left to the client".to_string();
    }
    match (
        &c.standard.option_id,
        c.standard.source,
        &c.standard.proposal,
    ) {
        (Some(_), StandardSource::Reviewer, _) => "set by you".to_string(),
        (Some(_), StandardSource::RuleDefault, _) => "the rule's default".to_string(),
        (Some(_), _, _) => "observed in the code".to_string(),
        (None, _, Some(p)) => match p.basis {
            ProposalBasis::RuleDefault => "proposal: the rule's default".to_string(),
            ProposalBasis::EarlierChoice => "proposal: your earlier choice".to_string(),
        },
        (None, _, None) => "no proposal".to_string(),
    }
}

/// The one plain line of what the engine found.
pub fn engine_line(c: &RuleConvention) -> String {
    let total = c.assessed_places;
    match &c.observed_option_id {
        Some(o) => {
            let n = c
                .options
                .iter()
                .find(|x| &x.option_id == o)
                .and_then(|x| x.count);
            match (n, total) {
                (Some(n), Some(m)) => {
                    format!("Observed: {n} of {m} places follow \"{}\"", label(c, o))
                }
                _ => format!("Observed: most places follow \"{}\"", label(c, o)),
            }
        }
        None => {
            let parts: Vec<String> = c
                .options
                .iter()
                .filter(|o| o.count.unwrap_or(0) > 0)
                .map(|o| format!("{} follow \"{}\"", o.count.unwrap_or(0), o.label))
                .collect();
            format!("Split: {}. No option holds two thirds.", parts.join(", "))
        }
    }
}

fn status_title(s: PlaceStatus) -> &'static str {
    match s {
        PlaceStatus::Conforms => "Follows the standard",
        PlaceStatus::Departs => "Departs from the standard",
        PlaceStatus::FollowsRuleDefault => "Follows the rule's default",
        PlaceStatus::NoStandard => "Places by option",
    }
}

/// The lists of places by status: follows the standard, departs, follows the rule's default and
/// reported by the finder with its note. A finder row appears under its status with the finder's
/// own note; one whose option was not recorded is listed under "Reported by the finder".
pub fn place_groups(c: &RuleConvention) -> Vec<PlaceGroup> {
    let mut groups: Vec<PlaceGroup> = Vec::new();
    let mut add =
        |title: String, place: PlaceLine| match groups.iter_mut().find(|g| g.title == title) {
            Some(g) => g.places.push(place),
            None => groups.push(PlaceGroup {
                title,
                places: vec![place],
            }),
        };
    let mut order: Vec<PlaceStatus> = vec![
        PlaceStatus::Conforms,
        PlaceStatus::Departs,
        PlaceStatus::FollowsRuleDefault,
        PlaceStatus::NoStandard,
    ];
    order.dedup();
    let has_standard = c.standard.option_id.is_some();
    for status in order {
        for o in &c.options {
            for p in o.places.iter().filter(|p| p.status == status) {
                let title = if has_standard {
                    status_title(status).to_string()
                } else {
                    format!("Follows \"{}\"", o.label)
                };
                add(
                    title,
                    PlaceLine {
                        repo: c.repo.clone(),
                        path: p.path.clone(),
                        line: p.line,
                        note: p.text.clone(),
                    },
                );
            }
        }
        for r in c.finder_rows.iter().filter(|r| r.status == status) {
            if status == PlaceStatus::NoStandard
                && r.option_basis != OptionBasis::Unknown
                && has_standard
            {
                continue;
            }
            let title = if r.option_basis == OptionBasis::Unknown {
                "Reported by the finder".to_string()
            } else if has_standard {
                status_title(status).to_string()
            } else {
                "Reported by the finder".to_string()
            };
            let note = if r.option_basis == OptionBasis::Unknown {
                let against = c
                    .judged_option_id
                    .as_deref()
                    .map_or(String::new(), |j| format!(" against \"{}\"", label(c, j)));
                format!("{} (reported{against}, option not recorded)", r.note)
            } else {
                r.note.clone()
            };
            add(
                title,
                PlaceLine {
                    repo: c.repo.clone(),
                    path: r.path.clone(),
                    line: r.line,
                    note,
                },
            );
        }
    }
    groups
}

/// The view of one rule's convention record.
pub fn convention_view(c: &RuleConvention) -> ConventionView {
    let accept_id = c
        .standard
        .option_id
        .clone()
        .or_else(|| c.standard.proposal.as_ref().map(|p| p.option_id.clone()))
        .filter(|id| c.options.iter().any(|o| &o.option_id == id && !o.violates));
    let reviewer = c.standard.source == StandardSource::Reviewer;
    let accept = accept_id
        .as_ref()
        .filter(|_| !reviewer)
        .map(|id| RulingControl {
            label: format!("Accept: {}", label(c, id)),
            effect: ACCEPT_EFFECT,
            option_id: Some(id.clone()),
        });
    let others = c
        .options
        .iter()
        .filter(|o| !o.violates && Some(&o.option_id) != accept_id.as_ref())
        .map(|o| RulingControl {
            label: o.label.clone(),
            effect: ACCEPT_EFFECT,
            option_id: Some(o.option_id.clone()),
        })
        .collect();
    ConventionView {
        engine_line: engine_line(c),
        origin: origin_label(c),
        differs_line: c
            .standard
            .differs_from_default
            .as_ref()
            .map(|d| format!("Differs from the rule's default: {d}")),
        groups: place_groups(c),
        not_recorded: (!c.places_recorded).then_some(PLACES_NOT_RECORDED),
        accept,
        others,
        leave: RulingControl {
            label: LEAVE_LABEL.to_string(),
            effect: LEAVE_EFFECT,
            option_id: None,
        },
        withdraw: c.standard.ruling_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::audit::{
        CensusPlace, FinderRowRef, OptionCensus, Proposal, StandardState,
    };

    fn opt(id: &str, label: &str, count: usize, places: Vec<CensusPlace>) -> OptionCensus {
        OptionCensus {
            option_id: id.into(),
            label: label.into(),
            violates: false,
            count: Some(count),
            places,
        }
    }

    fn place(path: &str, line: usize, status: PlaceStatus) -> CensusPlace {
        CensusPlace {
            path: path.into(),
            line,
            text: format!("code {path}:{line}"),
            checked: true,
            status,
        }
    }

    fn observed() -> RuleConvention {
        RuleConvention {
            repo: "r".into(),
            rule_id: "RULE-1".into(),
            default_option_id: Some("par".into()),
            judged_option_id: Some("par".into()),
            observed_option_id: Some("seq".into()),
            assessed_places: Some(5),
            places_recorded: true,
            options: vec![
                opt(
                    "par",
                    "Run in parallel",
                    1,
                    vec![place("a.ts", 1, PlaceStatus::FollowsRuleDefault)],
                ),
                opt(
                    "seq",
                    "Sequential",
                    4,
                    vec![place("b.ts", 2, PlaceStatus::Conforms)],
                ),
            ],
            standard: StandardState {
                option_id: Some("seq".into()),
                source: StandardSource::Observed,
                differs_from_default: Some("Run in parallel".into()),
                ..StandardState::default()
            },
            finder_rows: vec![FinderRowRef {
                path: "c.ts".into(),
                line: 9,
                note: "N+1 queries".into(),
                option_id: None,
                option_basis: OptionBasis::Unknown,
                status: PlaceStatus::NoStandard,
                ..FinderRowRef::default()
            }],
        }
    }

    /// Test 22 (pure part): the places are listed by status, each with its path and line so the
    /// screen can open the viewer on it, and the finder's note is kept.
    #[test]
    fn the_places_are_listed_by_status_with_path_line_and_the_finders_note() {
        let v = convention_view(&observed());
        let titles: Vec<&str> = v.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Follows the standard",
                "Follows the rule's default",
                "Reported by the finder"
            ]
        );
        assert_eq!(
            (
                v.groups[0].places[0].path.as_str(),
                v.groups[0].places[0].line
            ),
            ("b.ts", 2)
        );
        assert!(v.groups[2].places[0].note.contains("N+1 queries"));
        assert!(v.groups[2].places[0].note.contains("option not recorded"));
        assert_eq!(
            v.engine_line,
            "Observed: 4 of 5 places follow \"Sequential\""
        );
        assert_eq!(
            v.differs_line.as_deref(),
            Some("Differs from the rule's default: Run in parallel")
        );
    }

    /// Test 25: the origin never reads "AI recommended".
    #[test]
    fn the_origin_is_observed_set_by_you_or_a_labelled_proposal_never_ai_recommended() {
        let mut c = observed();
        assert_eq!(origin_label(&c), "observed in the code");
        c.standard.source = StandardSource::Reviewer;
        assert_eq!(origin_label(&c), "set by you");
        c.standard = StandardState {
            proposal: Some(Proposal {
                option_id: "par".into(),
                basis: ProposalBasis::RuleDefault,
            }),
            ..StandardState::default()
        };
        assert_eq!(origin_label(&c), "proposal: the rule's default");
        c.standard.proposal = Some(Proposal {
            option_id: "par".into(),
            basis: ProposalBasis::EarlierChoice,
        });
        assert_eq!(origin_label(&c), "proposal: your earlier choice");
        c.standard = StandardState {
            left_to_client: true,
            ..StandardState::default()
        };
        assert_eq!(origin_label(&c), "left to the client");
        for s in [
            origin_label(&c),
            engine_line(&c),
            convention_view(&c).engine_line,
        ] {
            assert!(!s.contains("AI recommended"), "{s}");
        }
    }

    /// Test 28: a report with no recorded places says so.
    #[test]
    fn an_old_report_says_the_places_were_not_recorded() {
        let mut c = observed();
        c.places_recorded = false;
        assert_eq!(
            convention_view(&c).not_recorded,
            Some("The places were not recorded for this scan.")
        );
        assert_eq!(convention_view(&observed()).not_recorded, None);
    }

    /// Tests 23 and 24 (the view side): "Accept: X" is the proposal or the observed convention, the
    /// other options follow, "No recommendation, leave it to the client" has its effect text, and
    /// a live ruling offers Withdraw.
    #[test]
    fn the_three_controls_and_withdraw() {
        let v = convention_view(&observed());
        assert_eq!(v.accept.as_ref().unwrap().label, "Accept: Sequential");
        assert_eq!(v.others.len(), 1);
        assert_eq!(v.others[0].option_id.as_deref(), Some("par"));
        assert_eq!(v.leave.label, "No recommendation, leave it to the client");
        assert_eq!(
            v.leave.effect,
            "The report lists the options and the places that follow each, with none suggested."
        );
        assert!(v.withdraw.is_none());
        let mut c = observed();
        c.standard.source = StandardSource::Reviewer;
        c.standard.ruling_id = Some("std-1".into());
        let v = convention_view(&c);
        assert!(v.accept.is_none(), "already set by the reviewer");
        assert_eq!(v.withdraw.as_deref(), Some("std-1"));
        // A split with a proposal offers it pre-selected.
        let mut split = observed();
        split.observed_option_id = None;
        split.standard = StandardState {
            proposal: Some(Proposal {
                option_id: "par".into(),
                basis: ProposalBasis::RuleDefault,
            }),
            ..StandardState::default()
        };
        let v = convention_view(&split);
        assert_eq!(v.accept.unwrap().label, "Accept: Run in parallel");
        assert!(v.engine_line.starts_with("Split: "), "{}", v.engine_line);
    }
}
