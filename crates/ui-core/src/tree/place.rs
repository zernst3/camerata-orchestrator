//! The Place in the lanes (D2): what a row that carries other rules shows and decides.
//!
//! A place with two or more problem rows is one decision, `place:<id>`, written as one record per
//! rule. The pieces here are the pure judgments the lanes draw: the "+N also match" chip, the
//! plan a place decision makes (which rules it writes, which it leaves out and why), the sentence
//! that names them all, and the ticks that leave a carried rule out.

use std::collections::BTreeSet;

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review_view::{CarriedRuleView, SiteRow};

use super::batch::{BatchPlan, LeftOutWhy, PlanRule};
use super::{base_name, thousands, Chip};

/// The kind tag a rule is named with in a prompt.
fn kind_tag(kind: Option<&camerata_api_types::audit::FindingKind>) -> &'static str {
    camerata_api_types::audit::FindingKind::tag_or_unspecified(kind)
}

/// The "+N also match" chip of a row that carries other rules at its place. The tooltip lists
/// every carried rule with its severity.
pub fn also_match_chip(row: &SiteRow) -> Option<Chip> {
    if row.carries.is_empty() {
        return None;
    }
    let n = row.carries.len();
    Some(Chip {
        class: "mark-also",
        label: format!("+{n} also match"),
        tip: row
            .carries
            .iter()
            .map(|c| format!("{} ({})", c.rule_id, c.severity))
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// The line a carried row shows in place of a headline: `carried by SEC-A at a.rs:5`.
pub fn carried_pointer(row: &SiteRow) -> Option<String> {
    let by = row.carried_by.as_ref()?;
    Some(format!(
        "carried by {} at {}:{}",
        by.rule_id,
        base_name(&by.path),
        by.line
    ))
}

/// The sentence under a carried row's Decision cell when it holds no decision of its own yet.
pub fn carried_decision_note(row: &SiteRow) -> Option<String> {
    let by = row.carried_by.as_ref()?;
    row.decision
        .is_none()
        .then(|| format!("decided with {} unless alone", by.rule_id))
}

/// One carried rule as the carrier's pane lists it, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedLine {
    /// `Y`, `N`, `?` or `unread`: stage 1's answer about the rule.
    pub stage1: &'static str,
    /// Why it is an escalation candidate and its rank, when it is one.
    pub escalation: Option<String>,
    /// The decision the rule stands under, or `Unresolved`; `inherited` when it came from the
    /// place's own decision.
    pub decision: String,
    pub inherited: bool,
    /// Why the gate holds it, in words.
    pub hold: Option<String>,
}

/// The words of a carried rule's line in "Also matches at this place".
pub fn carried_line(rule: &CarriedRuleView) -> CarriedLine {
    CarriedLine {
        stage1: match rule.stage1_real.as_str() {
            "yes" => "Y",
            "no" => "N",
            "unclear" => "?",
            _ => "unread",
        },
        escalation: rule.escalation_reason.map(|r| match rule.escalation_rank {
            Some(rank) => format!("escalation candidate: {} #{rank}", r.phrase()),
            None => format!("escalation candidate: {}", r.phrase()),
        }),
        decision: rule.decision.as_ref().map_or_else(
            || "Unresolved".to_string(),
            |d| super::decision_label(&d.state).to_string(),
        ),
        inherited: rule
            .decision
            .as_ref()
            .is_some_and(|d| d.inherited_from.is_some()),
        hold: rule.held.map(|h| h.phrase().to_string()),
    }
}

/// The ticks of the rules a place decision covers. A carried rule starts ticked; unticking it
/// leaves it out. A held rule is locked unticked: the gate needs a ruling on it by name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaceTicks {
    pub unticked: BTreeSet<String>,
}

impl PlaceTicks {
    /// Whether the carried rule would be covered by the next place decision.
    pub fn is_ticked(&self, rule: &CarriedRuleView) -> bool {
        rule.held.is_none() && !self.unticked.contains(&rule.site_key)
    }

    /// Tick or untick a carried rule. A held rule cannot be ticked: returns false.
    pub fn toggle(&mut self, rule: &CarriedRuleView) -> bool {
        if rule.held.is_some() {
            return false;
        }
        if !self.unticked.remove(&rule.site_key) {
            self.unticked.insert(rule.site_key.clone());
        }
        true
    }
}

/// The plan for deciding `state` at the place `carrier` heads. Pure: it reads only the carrier's
/// own row and the rules it lists, so it needs no page of sites.
pub fn place_plan(
    carrier: &SiteRow,
    state: &DecisionState,
    unticked: &BTreeSet<String>,
) -> BatchPlan {
    let mut plan = BatchPlan {
        node: carrier.place_id.clone(),
        state: state.clone(),
        keys: Vec::new(),
        excluded_outliers: Vec::new(),
        excluded_held: Vec::new(),
        excluded_merged: Vec::new(),
        already_recorded: Vec::new(),
        ticked_included: 0,
        written: Vec::new(),
        left_out: Vec::new(),
        excluded_unticked: Vec::new(),
        carried_elsewhere: 0,
    };
    // The carrier, then what it carries, each as (key, rule, held, decision, may be unticked).
    struct Member<'a> {
        key: &'a str,
        rule: PlanRule,
        held: Option<camerata_api_types::review_view::HoldReason>,
        decision: Option<&'a DecisionState>,
        carried: bool,
    }
    let mut members = vec![Member {
        key: &carrier.site_key,
        rule: PlanRule {
            rule_id: carrier.rule_id.clone(),
            severity: carrier.severity.clone(),
            kind: kind_tag(carrier.kind.as_ref()),
        },
        held: carrier.held,
        decision: carrier.decision.as_ref().map(|d| &d.state),
        carried: false,
    }];
    members.extend(carrier.carries.iter().map(|c| Member {
        key: &c.site_key,
        rule: PlanRule {
            rule_id: c.rule_id.clone(),
            severity: c.severity.clone(),
            kind: kind_tag(c.kind.as_ref()),
        },
        held: c.held,
        decision: c.decision.as_ref().map(|d| &d.state),
        carried: true,
    }));
    for m in members {
        let key = m.key.to_string();
        if let Some(hold) = m.held {
            plan.excluded_held.push(key);
            plan.left_out
                .push((m.rule, LeftOutWhy::Held(hold.phrase().to_string())));
        } else if matches!(m.decision, Some(DecisionState::MergedInto(_))) {
            plan.excluded_merged.push(key);
            plan.left_out.push((m.rule, LeftOutWhy::Merged));
        } else if m.carried && unticked.contains(&key) {
            plan.excluded_unticked.push(key);
            plan.left_out.push((m.rule, LeftOutWhy::Unticked));
        } else if m.decision == Some(state) {
            plan.already_recorded.push(key);
            plan.left_out.push((m.rule, LeftOutWhy::Already));
        } else {
            plan.keys.push(key);
            plan.written.push(m.rule);
        }
    }
    plan
}

fn rule_text(r: &PlanRule) -> String {
    format!("{} ({}, {})", r.rule_id, r.severity, r.kind)
}

fn list(rules: &[&PlanRule]) -> String {
    rules
        .iter()
        .map(|r| rule_text(r))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The prompt sentence of a place decision. It names every rule it writes with its severity and
/// kind, and every rule it leaves out with the reason, so a mixed place is read before it is
/// decided.
pub fn place_sentence(plan: &BatchPlan) -> String {
    let n = plan.written.len();
    let mut s = if n == 0 {
        "This writes no rule records at this place.".to_string()
    } else {
        format!(
            "This writes {} rule {} at this place: {}.",
            thousands(n),
            if n == 1 { "record" } else { "records" },
            list(&plan.written.iter().collect::<Vec<_>>())
        )
    };
    for (what, pick) in [
        ("held", 0usize),
        ("unticked", 1),
        ("merged", 2),
        ("already", 3),
    ] {
        let rules: Vec<&PlanRule> = plan
            .left_out
            .iter()
            .filter(|(_, why)| {
                matches!(
                    (pick, why),
                    (0, LeftOutWhy::Held(_))
                        | (1, LeftOutWhy::Unticked)
                        | (2, LeftOutWhy::Merged)
                        | (3, LeftOutWhy::Already)
                )
            })
            .map(|(r, _)| r)
            .collect();
        if rules.is_empty() {
            continue;
        }
        match what {
            "held" => {
                let detail = plan
                    .left_out
                    .iter()
                    .filter_map(|(r, why)| match why {
                        LeftOutWhy::Held(h) => Some(format!("{} ({h})", rule_text(r))),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                s.push_str(&format!(
                    " Held and left out, each needs a ruling by name: {detail}."
                ));
            }
            "unticked" => s.push_str(&format!(" You unticked: {}.", list(&rules))),
            "merged" => s.push_str(&format!(
                " Merged into another row and left as it is: {}.",
                list(&rules)
            )),
            _ => s.push_str(&format!(
                " Already holds this decision and is left as it is: {}.",
                list(&rules)
            )),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::batch::plan_sentence;
    use crate::tree::tests::data;
    use camerata_api_types::review_view::{CarriedByView, DecisionView, HoldReason, Marker};

    fn carried_rule(n: usize, severity: &str) -> CarriedRuleView {
        CarriedRuleView {
            rule_id: format!("SEC-{n}"),
            site_key: format!("key{n}"),
            node_id: format!("sub:SEC-{n}/0"),
            severity: severity.to_string(),
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            held: None,
            stage1_real: "yes".to_string(),
            escalation_reason: None,
            escalation_rank: None,
            decision: None,
        }
    }

    fn carrier() -> SiteRow {
        let mut r = data()
            .rows
            .iter()
            .find(|r| r.markers.is_empty() && r.decision.is_none() && r.stage2.is_some())
            .cloned()
            .unwrap();
        r.site_key = "key0".to_string();
        r.rule_id = "SEC-0".to_string();
        r.severity = "critical".to_string();
        r.kind = Some(camerata_api_types::audit::FindingKind::Defect);
        r.place_id = "place:abc".to_string();
        r.carries = vec![
            carried_rule(1, "high"),
            carried_rule(2, "medium"),
            carried_rule(3, "low"),
        ];
        r
    }

    fn decided(state: DecisionState) -> DecisionView {
        DecisionView {
            id: "d".to_string(),
            state,
            reason: "r".to_string(),
            scope: camerata_api_types::decisions::Scope::Site,
            inherited_from: None,
            decided_by: "zach".to_string(),
            decided_at: "t".to_string(),
            needed_me: None,
            confirmed_by_client: None,
        }
    }

    #[test]
    fn a_carrier_shows_a_chip_counting_what_it_carries_and_a_lone_row_shows_none() {
        let row = carrier();
        let chip = also_match_chip(&row).expect("a chip");
        assert_eq!(chip.label, "+3 also match");
        assert_eq!(chip.class, "mark-also");
        assert_eq!(chip.tip, "SEC-1 (high)\nSEC-2 (medium)\nSEC-3 (low)");
        let mut alone = row;
        alone.carries.clear();
        assert!(also_match_chip(&alone).is_none());
    }

    #[test]
    fn a_carried_row_points_at_its_carrier_and_says_where_it_is_decided_until_it_is() {
        let mut row = carrier();
        row.carries.clear();
        row.carried_by = Some(CarriedByView {
            site_key: "key0".to_string(),
            node_id: "sub:SEC-0/0".to_string(),
            rule_id: "SEC-0".to_string(),
            path: "src/deep/a.rs".to_string(),
            line: 5,
        });
        row.markers = vec![Marker::Carried];
        assert_eq!(
            carried_pointer(&row).as_deref(),
            Some("carried by SEC-0 at a.rs:5")
        );
        assert_eq!(
            carried_decision_note(&row).as_deref(),
            Some("decided with SEC-0 unless alone")
        );
        // Once the place is decided, the Decision column reads the inherited record.
        let mut inherited = decided(DecisionState::Confirmed);
        inherited.inherited_from = Some("dec-1".to_string());
        row.decision = Some(inherited);
        assert!(carried_decision_note(&row).is_none());
        let cell = crate::tree::decision_cell(&row);
        assert_eq!(cell.label, "Confirmed");
        assert!(cell.inherited);
    }

    #[test]
    fn a_place_plan_writes_every_ticked_rule_and_names_what_it_leaves_out_and_why() {
        let mut row = carrier();
        row.carries[1].held = Some(HoldReason::EvidenceUnverified);
        let unticked: BTreeSet<String> = ["key3".to_string()].into();
        let plan = place_plan(&row, &DecisionState::Confirmed, &unticked);
        assert_eq!(plan.node, "place:abc");
        assert_eq!(plan.keys, ["key0", "key1"]);
        assert_eq!(plan.excluded_held, ["key2"]);
        assert_eq!(plan.excluded_unticked, ["key3"]);
        assert_eq!(plan.except(), ["key2", "key3"]);
        assert!(plan.is_place_plan());
        let s = plan_sentence(&plan);
        assert_eq!(
            s,
            "This writes 2 rule records at this place: SEC-0 (critical, defect), SEC-1 (high, defect). \
             Held and left out, each needs a ruling by name: SEC-2 (medium, defect) (evidence unverified). \
             You unticked: SEC-3 (low, defect)."
        );
        // The request is a node decision on the place with the left-out keys listed.
        let req = crate::tree::batch::decision_request(&plan, "one secret", "run").unwrap();
        assert_eq!(
            req.scope,
            camerata_api_types::decisions::Scope::Node("place:abc".to_string())
        );
        assert_eq!(req.except, ["key2", "key3"]);
        assert!(crate::tree::batch::decision_request(&plan, " ", "run").is_err());
    }

    #[test]
    fn a_carried_rules_line_says_what_stage_1_the_hold_the_candidacy_and_the_decision_are() {
        use camerata_api_types::review::EscalationReason;
        let mut rule = carried_rule(1, "high");
        let line = carried_line(&rule);
        assert_eq!(line.stage1, "Y");
        assert_eq!(line.decision, "Unresolved");
        assert!(line.escalation.is_none() && line.hold.is_none() && !line.inherited);
        rule.stage1_real = String::new();
        rule.held = Some(HoldReason::EvidenceUnverified);
        rule.escalation_reason = Some(EscalationReason::SecretExposure);
        rule.escalation_rank = Some(7);
        let mut d = decided(DecisionState::Ignored);
        d.inherited_from = Some("dec-1".to_string());
        rule.decision = Some(d);
        let line = carried_line(&rule);
        assert_eq!(line.stage1, "unread");
        assert_eq!(line.hold.as_deref(), Some("evidence unverified"));
        assert!(line.escalation.unwrap().ends_with("#7"));
        assert_eq!((line.decision.as_str(), line.inherited), ("Ignored", true));
    }

    #[test]
    fn a_held_rule_is_locked_unticked_and_any_other_may_be_unticked_and_ticked_again() {
        let mut ticks = PlaceTicks::default();
        let mut held = carried_rule(1, "high");
        held.held = Some(HoldReason::JunkContent);
        assert!(!ticks.is_ticked(&held));
        assert!(!ticks.toggle(&held), "a held rule cannot be ticked");
        let free = carried_rule(2, "low");
        assert!(ticks.is_ticked(&free));
        assert!(ticks.toggle(&free));
        assert!(!ticks.is_ticked(&free));
        assert!(ticks.toggle(&free));
        assert!(ticks.is_ticked(&free));
    }

    #[test]
    fn a_node_batch_says_how_many_of_its_rows_are_carried_elsewhere() {
        let mut rows: Vec<SiteRow> = (0..3)
            .map(|n| {
                let mut r = carrier();
                r.site_key = format!("row{n}");
                r.carries.clear();
                r.place_id = String::new();
                r
            })
            .collect();
        rows[1].carried_by = Some(CarriedByView {
            site_key: "key0".to_string(),
            node_id: "sub:SEC-0/0".to_string(),
            rule_id: "SEC-0".to_string(),
            path: "a.rs".to_string(),
            line: 5,
        });
        let plan = crate::tree::batch::batch_plan(
            "sub:SEC-X/0",
            &DecisionState::Confirmed,
            &rows,
            &BTreeSet::new(),
        );
        assert_eq!(plan.keys.len(), 3, "a carried row is still a member");
        assert_eq!(plan.carried_elsewhere, 1);
        assert_eq!(
            plan_sentence(&plan),
            "This writes 3 site records. 1 row here is carried by another row at its place."
        );
    }

    #[test]
    fn a_rule_that_already_holds_the_decision_or_was_merged_is_left_as_it_is() {
        let mut row = carrier();
        row.carries[0].decision = Some(decided(DecisionState::Confirmed));
        row.carries[1].decision = Some(decided(DecisionState::MergedInto("x".to_string())));
        let plan = place_plan(&row, &DecisionState::Confirmed, &BTreeSet::new());
        assert_eq!(plan.keys, ["key0", "key3"]);
        assert_eq!(plan.already_recorded, ["key1"]);
        assert_eq!(plan.excluded_merged, ["key2"]);
        let s = plan_sentence(&plan);
        assert!(
            s.contains("Merged into another row and left as it is: SEC-2"),
            "{s}"
        );
        assert!(
            s.contains("Already holds this decision and is left as it is: SEC-1"),
            "{s}"
        );
    }
}
