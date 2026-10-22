//! The structured, bounded input one candidate hands the judge. Never a whole file: at most three
//! members of at most thirty lines each, plus the deterministic facts and short lists of names.
//! Every list is capped, so the input to one candidate is bounded by construction.

use serde::Serialize;

use crate::extract::{functions, lang_for_path};

use super::facts::{Candidate, NameSite, NamedDisagreement, RuleFacts, Site};

/// At most this many members are shown; the rest are counted in `member_count`.
pub const MAX_JUDGE_MEMBERS: usize = 3;
/// At most this many source lines are shown per member.
pub const MAX_JUDGE_LINES: usize = 30;
/// A single source line is cut at this many characters.
const MAX_LINE_CHARS: usize = 120;

const MAX_DECISION_TOKENS: usize = 24;
const MAX_DISAGREEMENTS: usize = 8;
const MAX_NAMES: usize = 12;
const MAX_LIST: usize = 12;

/// One member shown to the judge.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JudgeMember {
    pub path: String,
    /// `41-52`.
    pub lines: String,
    /// `function applyDiscount`, when the member sits in a named function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing: Option<String>,
    /// The span's source lines, de-indented, at most [`MAX_JUDGE_LINES`].
    pub code: String,
    /// More lines existed than are shown.
    pub truncated: bool,
}

/// One position where a copy differs from the first.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JudgeDisagreement {
    pub a: String,
    pub b: String,
    pub line_a: u32,
    pub line_b: u32,
}

/// A declared function name (ARCH-NAMING-ONE-CONCEPT-1).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JudgeNameSite {
    pub name: String,
    pub path: String,
    pub lines: String,
}

/// What the judge is asked about one candidate.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JudgeInput {
    pub id: String,
    pub rule: String,
    pub token_len: u32,
    pub member_count: usize,
    pub members: Vec<JudgeMember>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub decision_tokens: Vec<String>,
    pub agree: bool,
    pub disagreements: Vec<JudgeDisagreement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<JudgeNameSite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_pair: Option<[String; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<[JudgeNameSite; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub jsx_tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mechanism_marks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differing_strings: Vec<JudgeDisagreement>,
}

fn disagreement(d: &NamedDisagreement) -> JudgeDisagreement {
    JudgeDisagreement {
        a: d.a.clone(),
        b: d.b.clone(),
        line_a: d.line_a,
        line_b: d.line_b,
    }
}

fn name_site(n: &NameSite) -> JudgeNameSite {
    JudgeNameSite {
        name: n.name.clone().unwrap_or_else(|| "<anonymous>".to_string()),
        path: n.site.path.clone(),
        lines: n.site.lines(),
    }
}

/// The sites shown: the first three, except that a module overlap shows the first pairs side by
/// side (the two functions of a pair are what the judge compares).
fn shown_sites(c: &Candidate) -> Vec<Site> {
    if let RuleFacts::Naming {
        module: Some(module),
        ..
    } = &c.facts.rule
    {
        let mut out: Vec<Site> = Vec::new();
        for pair in &module.pairs {
            for site in [&pair.a.site, &pair.b.site] {
                if out.len() < MAX_JUDGE_MEMBERS && !out.contains(site) {
                    out.push(site.clone());
                }
            }
        }
        return out;
    }
    c.facts
        .sites
        .iter()
        .take(MAX_JUDGE_MEMBERS)
        .cloned()
        .collect()
}

/// Lines `start..=end` (1-based) of `source`, de-indented, capped in count and width.
fn excerpt(source: &str, start: u32, end: u32) -> (String, bool) {
    let first = (start.max(1) - 1) as usize;
    let wanted = (end.max(start) - start.max(1) + 1) as usize;
    let lines: Vec<&str> = source.lines().skip(first).take(wanted).collect();
    let truncated = lines.len() > MAX_JUDGE_LINES;
    let shown: Vec<&str> = lines.into_iter().take(MAX_JUDGE_LINES).collect();
    let indent = shown
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let code = shown
        .iter()
        .map(|l| {
            let body = l.get(indent..).unwrap_or(l).trim_end();
            body.chars().take(MAX_LINE_CHARS).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (code, truncated)
}

/// `function name` for the smallest named function containing the lines.
fn enclosing_function(path: &str, source: &str, start: u32, end: u32) -> Option<String> {
    let lang = lang_for_path(path)?;
    let (start, end) = (start as usize, end as usize);
    functions(lang, source)
        .into_iter()
        .filter(|f| f.start_line <= start && end <= f.end_line)
        .min_by_key(|f| f.end_line - f.start_line)
        .map(|f| format!("function {}", f.name))
}

fn member_of(site: &Site, files: &[(String, String)]) -> JudgeMember {
    let source = files
        .iter()
        .find(|(path, _)| *path == site.path)
        .map_or("", |(_, content)| content.as_str());
    let (code, truncated) = excerpt(source, site.start_line, site.end_line);
    JudgeMember {
        path: site.path.clone(),
        lines: site.lines(),
        enclosing: enclosing_function(&site.path, source, site.start_line, site.end_line),
        code,
        truncated,
    }
}

/// The bounded judge input for one candidate. `files` is the scan's file set (the same one the
/// candidates were built from); a member whose file is missing shows no code.
pub fn judge_input(c: &Candidate, files: &[(String, String)]) -> JudgeInput {
    let mut input = JudgeInput {
        id: c.id.clone(),
        rule: c.rule.id().to_string(),
        token_len: c.facts.token_len,
        member_count: c.member_count(),
        members: shown_sites(c).iter().map(|s| member_of(s, files)).collect(),
        decision_tokens: Vec::new(),
        agree: c.facts.agree,
        disagreements: c
            .facts
            .disagreements
            .iter()
            .take(MAX_DISAGREEMENTS)
            .map(disagreement)
            .collect(),
        names: Vec::new(),
        module_pair: None,
        pairs: Vec::new(),
        jsx_tags: Vec::new(),
        hooks: Vec::new(),
        mechanism_marks: Vec::new(),
        differing_strings: Vec::new(),
    };
    match &c.facts.rule {
        RuleFacts::Logic {
            decision_tokens, ..
        } => {
            input.decision_tokens = decision_tokens
                .iter()
                .take(MAX_DECISION_TOKENS)
                .cloned()
                .collect();
        }
        RuleFacts::Naming { names, module } => {
            input.names = names.iter().take(MAX_NAMES).map(name_site).collect();
            if let Some(module) = module {
                input.module_pair = Some(module.files.clone());
                input.pairs = module
                    .pairs
                    .iter()
                    .take(MAX_LIST)
                    .map(|p| [name_site(&p.a), name_site(&p.b)])
                    .collect();
            }
        }
        RuleFacts::Primitive {
            mechanism_marks,
            jsx_tags,
            hooks,
            differing_strings,
        } => {
            input.mechanism_marks = mechanism_marks.iter().take(MAX_LIST).cloned().collect();
            input.jsx_tags = jsx_tags.iter().take(MAX_LIST).cloned().collect();
            input.hooks = hooks.iter().take(MAX_LIST).cloned().collect();
            input.differing_strings = differing_strings
                .iter()
                .take(MAX_DISAGREEMENTS)
                .map(disagreement)
                .collect();
        }
    }
    input
}
