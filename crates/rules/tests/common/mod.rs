//! Shared helper for the client-facing house-style gates.
//!
//! `no_em_dash_client_facing.rs` and `no_serial_comma_client_facing.rs` both gate the same set of
//! rule fields, so the field walk lives here once and the two gates cannot drift apart. The set
//! is the one `w25_no_vendor_name_client_facing.rs` treats as client-facing:
//! - the rule `title` and every `[[sources]].title` (the citation block);
//! - every option's `directive`, `remediation`, `label`, `finding_headline` and `finding_detail`;
//! - a `violates` option's `why` (interpolated into the client's defect detail);
//! - the rule `summary`, but ONLY for a rule with no options, because
//!   `resolved_directive` falls back to it when there is no directive to show.
//!
//! Deliberately OUT of scope (author/operator-facing, never shown to a client): `qualifies`,
//! `[decision].why`, `[decision].question`, `[[sources]].relevance`, and a NON-`violates`
//! option's `why`.

// Each gate uses a different subset of these helpers.
#![allow(dead_code)]

use camerata_rules::RuleSet;

/// One client-facing text slot of one rule.
pub struct ClientField<'a> {
    /// The rule id, e.g. `SEC-LOGGING-1`.
    pub rule_id: &'a str,
    /// Where in the rule the text lives, e.g. `title` or `<option id>.directive`.
    pub loc: String,
    /// The text itself.
    pub text: &'a str,
}

/// Every client-facing field of every rule in `corpus`.
pub fn client_facing_fields(corpus: &RuleSet) -> Vec<ClientField<'_>> {
    let mut fields: Vec<ClientField<'_>> = Vec::new();
    for rule in corpus.iter() {
        let rid = rule.id.0.as_str();
        fields.push(ClientField {
            rule_id: rid,
            loc: "title".into(),
            text: &rule.title,
        });
        if rule.options.is_empty() {
            fields.push(ClientField {
                rule_id: rid,
                loc: "summary (option-less rule)".into(),
                text: &rule.summary,
            });
        }
        for s in &rule.sources {
            fields.push(ClientField {
                rule_id: rid,
                loc: format!("source `{}`", s.title.chars().take(40).collect::<String>()),
                text: &s.title,
            });
        }
        for o in &rule.options {
            fields.push(ClientField {
                rule_id: rid,
                loc: format!("{}.directive", o.id),
                text: &o.directive,
            });
            if let Some(t) = o.remediation.as_deref() {
                fields.push(ClientField {
                    rule_id: rid,
                    loc: format!("{}.remediation", o.id),
                    text: t,
                });
            }
            fields.push(ClientField {
                rule_id: rid,
                loc: format!("{}.label", o.id),
                text: &o.label,
            });
            if let Some(t) = o.finding_headline.as_deref() {
                fields.push(ClientField {
                    rule_id: rid,
                    loc: format!("{}.finding_headline", o.id),
                    text: t,
                });
            }
            if let Some(t) = o.finding_detail.as_deref() {
                fields.push(ClientField {
                    rule_id: rid,
                    loc: format!("{}.finding_detail", o.id),
                    text: t,
                });
            }
            if o.violates {
                fields.push(ClientField {
                    rule_id: rid,
                    loc: format!("{}.why (violates)", o.id),
                    text: &o.why,
                });
            }
        }
    }
    fields
}

/// One text slot that can reach a client anywhere (see [`client_reaching_fields`]).
pub struct ReachingField<'a> {
    /// The rule id.
    pub rule_id: &'a str,
    /// Where in the rule the text lives.
    pub loc: String,
    /// The text itself.
    pub text: &'a str,
    /// True for a `[[sources]].title`: the name of an external standard or document, which may
    /// keep the source's own spelling (but never a dash).
    pub external_title: bool,
}

/// EVERY rule text slot that can reach a client, by any route. This is the single source for
/// the dash and spelling gates; it is wider than [`client_facing_fields`] on purpose.
///
/// How the set was established (following the corpus type in `src/lib.rs` into the server):
/// - `title`: Rules checked sheet, coverage.json and coverage.csv, the PDF, the report headline.
/// - `summary` (derived from `qualifies`, else `decision.why`, else `title`): the scope-limited
///   rules sentence, the clone advisory "Scope:" line, the rule detail shown to the client.
/// - `decision_question` and `decision_why`: shown in the rule detail and the cockpit, and sent
///   to the model with the rule; the model copies rule wording into a row's notes (the
///   SEC-NO-OPEN-REDIRECT-1 incident), and from there it reaches the package.
/// - every option's `label` (Rules checked "alternative applied"), `directive`, `remediation`
///   ("Fix:" line), `finding_headline`, `finding_detail`, and `why` (the alternatives pass sends
///   every option's `why` to the model, and a `violates` option's `why` is interpolated into the
///   defect detail), plus the option's `escalation.condition`.
/// - every `[[sources]].title` (citation block, Standard cited column).
/// - every stack exception `note` and `current_recommendation` (client-visible suppression and
///   staleness notes).
///
/// Not included: `[[sources]].relevance` and `[[sources]].url` (never in the `Rule` type; author
/// facing only).
pub fn client_reaching_fields(corpus: &RuleSet) -> Vec<ReachingField<'_>> {
    fn fld<'a>(
        rule_id: &'a str,
        loc: String,
        text: &'a str,
        external_title: bool,
    ) -> ReachingField<'a> {
        ReachingField {
            rule_id,
            loc,
            text,
            external_title,
        }
    }
    let mut out: Vec<ReachingField<'_>> = Vec::new();
    for rule in corpus.iter() {
        let rid = rule.id.0.as_str();
        out.push(fld(rid, "title".into(), &rule.title, false));
        out.push(fld(rid, "summary".into(), &rule.summary, false));
        if let Some(t) = rule.decision_question.as_deref() {
            out.push(fld(rid, "decision.question".into(), t, false));
        }
        if let Some(t) = rule.decision_why.as_deref() {
            out.push(fld(rid, "decision.why".into(), t, false));
        }
        if let Some(t) = rule.current_recommendation.as_deref() {
            out.push(fld(rid, "current_recommendation".into(), t, false));
        }
        for s in &rule.sources {
            let loc = format!("source `{}`", s.title.chars().take(40).collect::<String>());
            out.push(fld(rid, loc, &s.title, true));
        }
        for se in &rule.stack_exceptions {
            out.push(fld(
                rid,
                format!("stack_exception {}.note", se.framework),
                &se.note,
                false,
            ));
        }
        for o in &rule.options {
            out.push(fld(rid, format!("{}.label", o.id), &o.label, false));
            out.push(fld(rid, format!("{}.directive", o.id), &o.directive, false));
            out.push(fld(rid, format!("{}.why", o.id), &o.why, false));
            for (name, t) in [
                ("remediation", o.remediation.as_deref()),
                ("finding_headline", o.finding_headline.as_deref()),
                ("finding_detail", o.finding_detail.as_deref()),
            ] {
                if let Some(t) = t {
                    out.push(fld(rid, format!("{}.{name}", o.id), t, false));
                }
            }
            if let Some(e) = o.escalation.as_ref() {
                out.push(fld(
                    rid,
                    format!("{}.escalation.condition", o.id),
                    &e.condition,
                    false,
                ));
            }
        }
    }
    out
}
