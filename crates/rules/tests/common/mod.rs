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
