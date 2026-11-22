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
/// - rule ids and option ids: stored in client selections and shown in the package, so they
///   follow the same spelling rules as prose.
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
/// - every `[[sources]].title` (citation block, Standard cited column) and every
///   `[[sources]].linter` (the enforcing tool and rule id, exported in the citations).
/// - every stack exception `note` and `current_recommendation` (client-visible suppression and
///   staleness notes).
///
/// Not included: `[[sources]].relevance` (it IS in the `RuleSource` type, but nothing outside the
/// rules crate reads it: it is author-facing provenance) and `[[sources]].url`.
///
/// `escalation.condition` stays in: `dev_implement_run.rs` builds the escalation reason from it
/// and `routes/runs.rs` serves it to the run screen, so the operator reading a development run
/// sees it. That is not a client package, but it is a consumer outside the rules crate, so the
/// "drop it if nothing consumes it" test does not apply.
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
        out.push(fld(rid, "rule id".into(), rid, false));
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
            if let Some(l) = s.linter.as_deref() {
                out.push(fld(rid, format!("source linter `{l}`"), l, true));
            }
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
            out.push(fld(rid, format!("option id {}", o.id), &o.id, false));
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

/// A synthetic rule with `plant` written into EVERY field of the wide list, for the gates'
/// "can it fail" tests. The `relevance` line is planted too: it is not client-reaching, so a gate
/// that reports it is wrong. Returns the TOML for `universal/synth-plant-1.toml`.
#[allow(dead_code)]
pub fn planted_rule_toml(plant: &str) -> String {
    format!(
        r#"id = "SYNTH-PLANT-1"
title = "Title {plant}"
enforcement = "prose"
kind = "decision"
inspectable = true
default = true
verification = "policy"
qualifies = "Qualifies {plant}"
superseded_since = "2020"
current_recommendation = "Recommendation {plant}"

[[sources]]
url = "https://example.com/x"
title = "Source {plant}"
linter = "tool: rule {plant}"
relevance = "Relevance {plant}"

[[stack_exception]]
framework = "Synth"
path_glob = "**"
note = "Exception {plant}"

[decision]
question = "Question {plant}"
default = "the-correct-option"
why = "Decision why {plant}"

[[option]]
id = "the-correct-option"
label = "Label {plant}"
directive = "Directive {plant}"
remediation = "Remediation {plant}"
finding_headline = "Headline {plant}"
finding_detail = "Detail {plant}"
why = "Option why {plant}"
escalation = {{ condition = "Condition {plant}", severity = "soft-flag" }}
"#
    )
}

/// Write [`planted_rule_toml`] into a temp corpus and load it.
#[allow(dead_code)]
pub async fn load_planted(plant: &str) -> (tempfile::TempDir, RuleSet) {
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    std::fs::write(
        universal.join("synth-plant-1.toml"),
        planted_rule_toml(plant),
    )
    .expect("write fixture");
    let (set, errors) = camerata_rules::load_corpus_lenient(dir.path()).await;
    assert!(errors.is_empty(), "planted fixture must load: {errors:?}");
    (dir, set)
}

/// What every planted gate must report: one offender naming each of these locations.
#[allow(dead_code)]
pub const PLANTED_LOCATIONS: &[&str] = &[
    "SYNTH-PLANT-1: title",
    "SYNTH-PLANT-1: summary",
    "SYNTH-PLANT-1: decision.question",
    "SYNTH-PLANT-1: decision.why",
    "SYNTH-PLANT-1: current_recommendation",
    "SYNTH-PLANT-1: stack_exception Synth.note",
    "SYNTH-PLANT-1: the-correct-option.label",
    "SYNTH-PLANT-1: the-correct-option.directive",
    "SYNTH-PLANT-1: the-correct-option.why",
    "SYNTH-PLANT-1: the-correct-option.remediation",
    "SYNTH-PLANT-1: the-correct-option.finding_headline",
    "SYNTH-PLANT-1: the-correct-option.finding_detail",
    "SYNTH-PLANT-1: the-correct-option.escalation.condition",
];

/// The source locations (external titles): the dash gate reports these, the spelling gate does not.
#[allow(dead_code)]
pub const PLANTED_SOURCE_LOCATIONS: &[&str] = &[
    "SYNTH-PLANT-1: source `Source",
    "SYNTH-PLANT-1: source linter",
];
