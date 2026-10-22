//! What reaches a client: the finding for a confirmed candidate and the not-assessed row. There is
//! no convention ("clean") row: a search that found nothing, or whose candidates were all judged
//! not to be the thing, does not claim the absence of anything (orchestrator decision D1, W29
//! Wave C review).
//!
//! Every sentence about a candidate is a fact the producer computed by token comparison (where
//! the copies are, how long, whether they agree and what differs). The only clause the judge
//! writes is the concept, and it is phrased as an appearance ("appears to be"). No em-dashes, no
//! product names: this text is a client deliverable.

use std::collections::BTreeMap;

use camerata_api_types::audit::{FindingKind, NotAssessedKind, NotAssessedReason};
use camerata_checks::clone_rules::{
    Candidate, CloneRule, NameSite, NamedDisagreement, RuleFacts, Site,
};

use crate::onboard::architectural::ARCH_PREVIEW_TOOL;
use crate::onboard::{Finding, MergedLocation};

/// The default (adopted) option of each rule: the convention a clean repo follows.
pub fn default_option_id(rule: CloneRule) -> &'static str {
    match rule {
        CloneRule::SingleSourceLogic => "each-decision-is-implemented-once",
        CloneRule::NamingOneConcept => "one-concept-has-one-name",
        CloneRule::SharedPrimitive => "repeated-ui-mechanisms-are-one-shared-component",
    }
}

/// The `violates` option of each rule: the state a finding reports the code to be in.
pub fn violating_option_id(rule: CloneRule) -> &'static str {
    match rule {
        CloneRule::SingleSourceLogic => "the-same-decision-is-written-in-several-places",
        CloneRule::NamingOneConcept => "the-same-helper-exists-under-several-names",
        CloneRule::SharedPrimitive => "ui-mechanisms-are-hand-written-per-component",
    }
}

/// A token text safe to put between backticks in a sentence.
fn quoted(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '`' || c.is_control() { '\'' } else { c })
        .take(40)
        .collect();
    format!("`{flat}`")
}

fn place(path: &str, line: u32) -> String {
    quoted(&format!("{path}:{line}"))
}

fn site_ref(site: &Site) -> String {
    quoted(&format!("{}:{}", site.path, site.lines()))
}

/// `a`, `a and b`, `a, b and c`.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// "at `a:44` the code has `0.2` while `b:18` has `0.15`", the first three, then "and N more".
/// Decision differences are named first.
fn disagreement_clause(ds: &[NamedDisagreement]) -> String {
    let mut ordered: Vec<&NamedDisagreement> = ds.iter().filter(|d| d.decision).collect();
    ordered.extend(ds.iter().filter(|d| !d.decision));
    let named: Vec<String> = ordered
        .iter()
        .take(3)
        .map(|d| {
            format!(
                "at {} the code has {} while {} has {}",
                place(&d.path_a, d.line_a),
                quoted(&d.a),
                place(&d.path_b, d.line_b),
                quoted(&d.b)
            )
        })
        .collect();
    let mut text = named.join("; ");
    if ds.len() > 3 {
        text.push_str(&format!(", and {} more", ds.len() - 3));
    }
    text
}

/// The concept without its leading article, for "the same ___".
fn bare(concept: &str) -> &str {
    let lower = concept.to_ascii_lowercase();
    for article in ["the ", "a ", "an "] {
        if lower.starts_with(article) {
            return &concept[article.len()..];
        }
    }
    concept
}

/// The first source line of a site, trimmed and capped, or empty when the file is missing.
fn first_line(site: &Site, files: &[(String, String)]) -> String {
    files
        .iter()
        .find(|(path, _)| *path == site.path)
        .and_then(|(_, content)| {
            content
                .lines()
                .nth((site.start_line as usize).saturating_sub(1))
        })
        .map(|line| line.trim().chars().take(200).collect())
        .unwrap_or_default()
}

fn name_of(n: &NameSite) -> String {
    quoted(n.name.as_deref().unwrap_or("<anonymous>"))
}

fn logic_detail(c: &Candidate, concept: &str) -> String {
    let n = c.member_count();
    let sites: Vec<String> = c.facts.sites.iter().map(site_ref).collect();
    let appears = if concept.is_empty() {
        String::new()
    } else {
        format!(", which appears to be {concept}")
    };
    let block = format!(
        "{} are the same {}-token block apart from variable names{appears}",
        join_and(&sites),
        c.facts.token_len
    );
    if c.facts.agree {
        format!(
            "The same decision is written in {n} places and the copies currently agree. \
             {block}. Implement it once and import it, so the copies cannot drift. Verify by \
             diffing the listed lines."
        )
    } else if decision_differs(c) {
        format!(
            "The same decision is written in {n} places and the copies do not agree. {block}, \
             but {}. Implement it once: one function in one module, imported at every listed \
             site, so a change to any of these values is one edit. Verify by diffing the listed \
             lines: the only differences are variable names and the differences described here.",
            disagreement_clause(&c.facts.disagreements)
        )
    } else {
        // The copies differ, but not in any operator, number or tested property: the decision
        // itself is the same, and what differs is named after it.
        format!(
            "The same decision is written in {n} places. {block}. The operators, numbers and \
             tested properties are the same in every copy, and the copies differ only where \
             {}. Implement it once and import it, so the copies cannot drift. Verify by diffing \
             the listed lines: the only differences are variable names and the differences \
             described here.",
            disagreement_clause(&c.facts.disagreements)
        )
    }
}

/// Whether the copies differ in an operator, a number or a property inside a decision test.
fn decision_differs(c: &Candidate) -> bool {
    c.facts.disagreements.iter().any(|d| d.decision)
}

fn naming_detail(c: &Candidate) -> String {
    let RuleFacts::Naming { names, module } = &c.facts.rule else {
        return String::new();
    };
    let agreement = if c.facts.agree {
        "and they currently agree".to_string()
    } else {
        format!(
            "and they do not agree: {}",
            disagreement_clause(&c.facts.disagreements)
        )
    };
    if let Some(module) = module {
        let pairs: Vec<String> = module
            .pairs
            .iter()
            .map(|p| {
                format!(
                    "{} matches {} ({} / {})",
                    name_of(&p.a),
                    name_of(&p.b),
                    site_ref(&p.a.site),
                    site_ref(&p.b.site)
                )
            })
            .collect();
        let overlap = if c.facts.agree {
            "The overlapping bodies currently agree.".to_string()
        } else {
            format!(
                "The overlapping bodies do not all agree: {}.",
                disagreement_clause(&c.facts.disagreements)
            )
        };
        return format!(
            "{} and {} overlap: {}. {overlap} Keep one module; re-export from the other during \
             the move, then delete it.",
            quoted(&module.files[0]),
            quoted(&module.files[1]),
            join_and(&pairs)
        );
    }
    let called: Vec<String> = names
        .iter()
        .map(|n| format!("{} in {}", name_of(n), site_ref(&n.site)))
        .collect();
    format!(
        "The same thing is called {}: the {} bodies are the same {}-token block apart from \
         variable names, {agreement}. Keep one, re-export or delete the other, and give it the \
         name that says what it does. Verify by diffing the {} bodies.",
        join_and(&called),
        names.len(),
        c.facts.token_len,
        names.len()
    )
}

fn primitive_detail(c: &Candidate, concept: &str) -> String {
    let RuleFacts::Primitive {
        differing_strings, ..
    } = &c.facts.rule
    else {
        return String::new();
    };
    let n = c.member_count();
    let sites: Vec<String> = c.facts.sites.iter().map(site_ref).collect();
    let what = if concept.is_empty() {
        "UI mechanism".to_string()
    } else {
        bare(concept).to_string()
    };
    let strings = if differing_strings.is_empty() {
        String::new()
    } else {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for d in differing_strings {
            if !seen.contains(&(d.a.as_str(), d.b.as_str())) {
                seen.push((d.a.as_str(), d.b.as_str()));
            }
        }
        let shown: Vec<String> = seen
            .iter()
            .take(4)
            .map(|(a, b)| format!("{} and {}", quoted(a), quoted(b)))
            .collect();
        format!(
            " The strings that differ between the copies are {}.",
            shown.join("; ")
        )
    };
    // Anything that differs and is not a string (a callee, a property, a number) is named too:
    // "apart from variable names and strings" is said only of what the copies share.
    let others: Vec<NamedDisagreement> = c
        .facts
        .disagreements
        .iter()
        .filter(|d| !differing_strings.contains(d))
        .cloned()
        .collect();
    let also = if others.is_empty() {
        String::new()
    } else {
        format!(
            " The copies also differ where {}.",
            disagreement_clause(&others)
        )
    };
    format!(
        "{n} components hand-write the same {what}: {} contain the same {}-token block apart \
         from variable names and strings.{strings}{also} Make it one hook or component in the \
         shared UI module and use it in each listed place. Verify by diffing the listed ranges.",
        join_and(&sites),
        c.facts.token_len
    )
}

/// The client finding for a candidate the judge confirmed.
pub fn confirmed_finding(
    repo: &str,
    c: &Candidate,
    concept: &str,
    files: &[(String, String)],
) -> Finding {
    // High only when the copies disagree on the decision itself (an operator, a number, a tested
    // property). Other differences (a string, a template name, an action type, a callee) are
    // named in the detail and do not raise the severity.
    let severity = match c.rule {
        CloneRule::SingleSourceLogic if decision_differs(c) => "high",
        CloneRule::SingleSourceLogic => "medium",
        CloneRule::NamingOneConcept | CloneRule::SharedPrimitive => "low",
    };
    let concept_with_article = if concept.is_empty() {
        String::new()
    } else if concept.to_ascii_lowercase().starts_with("the ") {
        concept.to_string()
    } else {
        format!("the {concept}")
    };
    let detail = match c.rule {
        CloneRule::SingleSourceLogic => logic_detail(c, &concept_with_article),
        CloneRule::NamingOneConcept => naming_detail(c),
        CloneRule::SharedPrimitive => primitive_detail(c, &concept_with_article),
    };
    let mut captures: BTreeMap<String, String> = BTreeMap::new();
    if !concept.is_empty() {
        let key = if c.rule == CloneRule::SharedPrimitive {
            "mechanism"
        } else {
            "concept"
        };
        captures.insert(key.to_string(), concept.to_string());
    }
    // The family id both rules share when the same block is also a naming candidate: lets a
    // reader (and the exporter) link the two rows without merging them.
    let family =
        c.id.strip_prefix(&format!("{}:", c.rule.id()))
            .unwrap_or(&c.id);
    captures.insert("family".to_string(), family.to_string());

    let mut sites = c.facts.sites.iter();
    let (path, line, snippet) = match sites.next() {
        Some(first) => (
            first.path.clone(),
            first.start_line as usize,
            first_line(first, files),
        ),
        None => (String::new(), 0, String::new()),
    };
    let also_locations: Vec<MergedLocation> = sites
        .map(|site| MergedLocation {
            repo: repo.to_string(),
            path: site.path.clone(),
            line: site.start_line as usize,
            rule_id: c.rule.id().to_string(),
            snippet: first_line(site, files),
            consequence: false,
        })
        .collect();
    Finding {
        repo: repo.to_string(),
        path,
        line,
        rule_id: c.rule.id().to_string(),
        severity: severity.to_string(),
        snippet,
        detail,
        // A deterministic-tier row: the facts are token comparisons, the judge only gates which
        // candidates are shown. Never calibrated, never held for review.
        preview: true,
        preview_tool: Some(ARCH_PREVIEW_TOOL.to_string()),
        confidence: Some("high".to_string()),
        captures,
        also_locations,
        // No `evaluated_option_id`, like every other deterministic checker's defect: the fix text
        // lives on the rule's default option (the violating option carries none), and the export
        // resolves a finding's Fix through `evaluated_option_id` first. Naming the violating
        // option here would make the Fix line vanish and the row be held for want of one.
        evaluated_option_id: None,
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

/// One `not_assessed` row for a rule that was armed and found candidates but could not decide
/// them (never a finding, never clean).
pub fn not_assessed_row(
    repo: &str,
    rule: CloneRule,
    reason: NotAssessedReason,
    reason_detail: String,
    detail: String,
) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: String::new(),
        line: 0,
        rule_id: rule.id().to_string(),
        severity: "info".to_string(),
        snippet: reason_detail.chars().take(200).collect(),
        detail,
        located: false,
        preview: true,
        preview_tool: Some(ARCH_PREVIEW_TOOL.to_string()),
        kind: Some(FindingKind::NotAssessed(NotAssessedKind {
            reason,
            reason_detail,
        })),
        ..Finding::default()
    }
}
