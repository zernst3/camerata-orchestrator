//! Reading the agent's output files (design section 2): `problems-*.json`, `checklist.json`,
//! `leads.json`, `card.md` and `unread.json`, each checked against a schema in code, and the
//! turn of the problems into the engine's `Finding` through `parse_ai_findings`.

use std::collections::{BTreeMap, HashSet};

use super::checklist::{
    AnswerKind, Checklist, ChecklistFile, Citation, OptionCount, Place, RuleAnswer,
};
use crate::onboard::{Finding, ProposedRule};

/// One problem as the agent wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub n: u32,
    pub path: String,
    pub line: usize,
    pub end_line: Option<usize>,
    /// `critical`, `high`, `medium` or `low`.
    pub severity: String,
    pub title: String,
    pub code: String,
    pub what: String,
    pub why: String,
    pub fix: String,
    /// False when the agent marked it `unsure`.
    pub sure: bool,
    /// `low`, `medium` or `high`.
    pub effort: String,
    pub label: String,
    /// The other places where the same thing is wrong (the main place is `path`/`line`).
    pub places: Vec<ProblemPlace>,
}

/// One further place of a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemPlace {
    pub path: String,
    pub line: usize,
    pub end_line: Option<usize>,
}

/// The captures a finder problem carries for the operator. All of them are bookkeeping keys
/// (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`): none names an object, and the same value on two
/// problems (the same piece, the same `sure`) must never read as one cause.
pub const FINDER_PIECE_CAPTURE: &str = "finder-piece";
pub const FINDER_SURE_CAPTURE: &str = "finder-sure";
pub const FINDER_EFFORT_CAPTURE: &str = "finder-effort";
pub const FINDER_FIX_CAPTURE: &str = "finder-fix";
pub const FINDER_PROBLEM_CAPTURE: &str = "finder-problem";
pub const FINDER_END_LINE_CAPTURE: &str = "finder-end-line";
/// Every checklist rule id the problem was cited to, comma separated, strictest first.
pub const FINDER_RULES_CAPTURE: &str = "finder-rules";

/// A problem the agent saw in a file outside its piece (a lead). Numbered within the piece's
/// output, from 1, in the order found: the problems moved from a problems file first, then
/// `leads.json`. A joints call that gathers the leads of several pieces numbers them again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lead {
    pub n: u32,
    pub path: String,
    pub line: usize,
    pub what: String,
    /// The number of the agent's problem this lead was made from, when it was a problem written
    /// for a file outside the piece. `None` for an entry of `leads.json`.
    pub source_problem: Option<u32>,
    /// The severity of the problem this lead was made from; `None` for an entry of `leads.json`.
    pub severity: Option<String>,
}

/// An output file that could not be used, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unusable {
    pub file: String,
    pub reason: String,
}

/// Everything read from one piece's output files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PieceOutput {
    pub problems: Vec<Problem>,
    pub checklist: Option<ChecklistFile>,
    /// Leads the agent wrote, plus problems it put in a file outside its piece.
    pub leads: Vec<Lead>,
    pub card: Option<String>,
    /// The agent's own list of files it did not read in full. Never used for coverage.
    pub unread: Option<Vec<String>>,
    pub unusable: Vec<Unusable>,
    /// Operator-only remarks: a further place of a problem that was dropped, and why.
    pub notes: Vec<String>,
    /// How many `problems-<n>.json` files were seen, usable or not. A run that wrote none has
    /// nothing to report and the runner fails the piece (output never vanishes silently).
    pub problems_files: usize,
    /// The text of `leads-answers.json` (the joints call), read by [`parse_leads_answers`].
    pub leads_answers: Option<String>,
}

/// The most problems one file may hold.
pub const MAX_PROBLEMS_PER_FILE: usize = 40;

const SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];
const EFFORTS: [&str; 3] = ["low", "medium", "high"];

/// A path the way the room names it.
fn plain_path(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

/// Text of a lead the way problem text is treated: dashes rewritten, then redacted.
fn lead_text(raw: &str) -> String {
    let mut text = crate::client_text::plain(raw.trim()).into_owned();
    crate::onboard::redact::redact_string(&mut text);
    text
}

/// The file name of an output path (`_camerata/out/card.md` and `card.md` both give `card.md`).
fn file_name(key: &str) -> &str {
    key.strip_prefix(super::room::OUT_DIR).unwrap_or(key)
}

/// The number in `problems-<n>.json`, when `name` has that shape.
fn problems_file_number(name: &str) -> Option<u64> {
    name.strip_prefix("problems-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// Whether `name` is a stray problems file name (`problems_2.json`, `problems.json`) whose `text`
/// parses as a problems list. The agent has no delete tool, so a stray file it already wrote could
/// never be repaired; one that holds a usable list is read as a problems file.
fn stray_problems_list(name: &str, text: &str, cap: usize) -> bool {
    if !(name.starts_with("problems") && name.ends_with(".json")) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let Ok(items) = array_of(&value, "problems") else {
        return false;
    };
    items.len() <= cap && items.iter().all(|item| parse_problem(item).is_ok())
}

fn field<'a>(v: &'a serde_json::Value, name: &str) -> Result<&'a serde_json::Value, String> {
    v.get(name)
        .filter(|x| !x.is_null())
        .ok_or_else(|| format!("is missing the field `{name}`"))
}

fn text_field(v: &serde_json::Value, name: &str, allow_empty: bool) -> Result<String, String> {
    let s = field(v, name)?
        .as_str()
        .ok_or_else(|| format!("has a `{name}` that is not text"))?;
    if !allow_empty && s.trim().is_empty() {
        return Err(format!("has an empty `{name}`"));
    }
    Ok(s.to_string())
}

fn number_field(v: &serde_json::Value, name: &str) -> Result<usize, String> {
    field(v, name)?
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("has a `{name}` that is not a whole number"))
}

fn one_of(v: &serde_json::Value, name: &str, allowed: &[&str]) -> Result<String, String> {
    let s = text_field(v, name, false)?.trim().to_ascii_lowercase();
    if allowed.contains(&s.as_str()) {
        Ok(s)
    } else {
        Err(format!("has {name} `{s}`; use {}", allowed.join(", ")))
    }
}

fn parse_problem(v: &serde_json::Value) -> Result<Problem, String> {
    if !v.is_object() {
        return Err("is not an object".to_string());
    }
    let n = u32::try_from(number_field(v, "n")?)
        .map_err(|_| "has an `n` that is too large".to_string())?;
    let end_line = match v.get("end_line") {
        None | Some(serde_json::Value::Null) => None,
        Some(x) => Some(
            x.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| "has an `end_line` that is not a whole number".to_string())?,
        ),
    };
    let sure = match field(v, "sure")? {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" | "high" | "sure" | "certain" => true,
            "unsure" | "no" | "false" | "low" | "medium" | "unclear" => false,
            other => return Err(format!("has sure `{other}`; use yes or unsure")),
        },
        _ => return Err("has a `sure` that is not yes or unsure".to_string()),
    };
    Ok(Problem {
        n,
        path: plain_path(text_field(v, "path", false)?.trim()),
        line: number_field(v, "line")?,
        end_line,
        severity: one_of(v, "severity", &SEVERITIES)?,
        title: text_field(v, "title", false)?,
        code: text_field(v, "code", true)?,
        what: text_field(v, "what", false)?,
        why: text_field(v, "why", true)?,
        fix: text_field(v, "fix", true)?,
        sure,
        effort: one_of(v, "effort", &EFFORTS)?,
        label: text_field(v, "label", false)?,
        places: Vec::new(),
    })
}

/// The further places of a problem, read tolerantly: a missing or non-list `places` is none, and
/// an entry with no path is dropped with a remark, never failing the file.
fn parse_problem_places(
    v: &serde_json::Value,
    n: u32,
    notes: &mut Vec<String>,
) -> Vec<ProblemPlace> {
    let items = match v.get("places") {
        None | Some(serde_json::Value::Null) => return Vec::new(),
        Some(serde_json::Value::Array(items)) => items,
        Some(_) => {
            notes.push(format!(
                "problem {n}: `places` is not a list, so it was ignored"
            ));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let (path, line, end_line) = match item {
            serde_json::Value::String(text) => split_place_text(text),
            _ => {
                let number = |key: &str| {
                    item.get(key)
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                };
                (
                    item.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(|s| plain_path(s.trim()))
                        .filter(|s| !s.is_empty()),
                    number("line"),
                    number("end_line"),
                )
            }
        };
        match path {
            // A path with no usable line is a place at line 0, which the runner holds as a
            // problem needing a decision, so nothing the model listed vanishes.
            Some(path) => out.push(ProblemPlace {
                path,
                line: line.unwrap_or(0),
                end_line,
            }),
            None => notes.push(format!(
                "problem {n}: place {} has no path, so it was dropped",
                i + 1
            )),
        }
    }
    out
}

/// A place written as text: `path:line`, `path:line-end` or just `path`. The path is everything
/// before the first colon (a Windows drive colon is skipped); a line part that is not a number
/// leaves line 0, so the real path is kept and the runner holds the place for a decision.
pub(super) fn split_place_text(text: &str) -> (Option<String>, Option<usize>, Option<usize>) {
    let text = text.trim();
    let bytes = text.as_bytes();
    let drive = bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let from = if drive { 2 } else { 0 };
    let (path, rest) = match text[from..].find(':') {
        Some(i) => (&text[..from + i], &text[from + i + 1..]),
        None => (text, ""),
    };
    let leading_digits = |s: &str| -> (Option<usize>, usize) {
        let n = s.chars().take_while(char::is_ascii_digit).count();
        (s[..n].parse().ok(), n)
    };
    let (line, used) = leading_digits(rest.trim_start());
    let after = &rest.trim_start()[used..];
    let end = match (line, after.strip_prefix('-')) {
        (Some(_), Some(tail)) => leading_digits(tail).0,
        _ => None,
    };
    let path = plain_path(path.trim());
    ((!path.is_empty()).then_some(path), line, end)
}

/// The array in a file that holds either an array or an object with the array under `key`.
fn array_of<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a Vec<serde_json::Value>, String> {
    v.as_array()
        .or_else(|| v.get(key).and_then(serde_json::Value::as_array))
        .ok_or_else(|| format!("must be a JSON array (of {key})"))
}

fn parse_place(v: &serde_json::Value) -> Result<Place, String> {
    match v {
        serde_json::Value::String(s) => {
            let s = s.trim();
            match s.rsplit_once(':') {
                Some((path, line))
                    if line.chars().all(|c| c.is_ascii_digit()) && !line.is_empty() =>
                {
                    Ok(Place {
                        path: plain_path(path),
                        line: line.parse().ok(),
                    })
                }
                _ => Ok(Place {
                    path: plain_path(s),
                    line: None,
                }),
            }
        }
        serde_json::Value::Object(_) => Ok(Place {
            path: plain_path(text_field(v, "path", false)?.trim()),
            line: v
                .get("line")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok()),
        }),
        _ => Err("has a place that is neither text nor an object".to_string()),
    }
}

fn parse_places(v: Option<&serde_json::Value>) -> Result<Vec<Place>, String> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items.iter().map(parse_place).collect(),
        Some(_) => Err("has `places` that is not a list".to_string()),
    }
}

fn parse_checklist(v: &serde_json::Value) -> Result<ChecklistFile, String> {
    if !v.is_object() {
        return Err("must be a JSON object with `citations` and `answers`".to_string());
    }
    let mut file = ChecklistFile::default();
    if let Some(cites) = v.get("citations") {
        let list = cites
            .as_array()
            .ok_or_else(|| "has `citations` that is not a list".to_string())?;
        for (i, c) in list.iter().enumerate() {
            let problem = number_field(c, "n").map_err(|e| format!("citation {} {e}", i + 1))?;
            let rule =
                text_field(c, "rule", false).map_err(|e| format!("citation {} {e}", i + 1))?;
            file.citations.push(Citation {
                problem: u32::try_from(problem).unwrap_or(u32::MAX),
                rule: rule.trim().to_ascii_uppercase(),
            });
        }
    }
    let answers = v
        .get("answers")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "is missing the list `answers`".to_string())?;
    for (i, a) in answers.iter().enumerate() {
        let at = |e: String| format!("answer {} {e}", i + 1);
        let rule = text_field(a, "rule", false)
            .map_err(at)?
            .trim()
            .to_ascii_uppercase();
        let kind = match one_of(
            a,
            "answer",
            &["problems", "clean", "not_applicable", "not_a_problem_here"],
        )
        .map_err(|e| format!("answer {} {e}", i + 1))?
        .as_str()
        {
            "problems" => AnswerKind::Problems,
            "clean" => AnswerKind::Clean,
            "not_a_problem_here" => AnswerKind::NotAProblemHere,
            _ => AnswerKind::NotApplicable,
        };
        let problems = match a.get("problems") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|x| x.as_u64().and_then(|n| u32::try_from(n).ok()))
                .collect(),
            Some(_) => {
                return Err(format!(
                    "answer {} has `problems` that is not a list",
                    i + 1
                ))
            }
        };
        let mut places =
            parse_places(a.get("places")).map_err(|e| format!("answer {} {e}", i + 1))?;
        let mut reason = None;
        if kind == AnswerKind::NotAProblemHere {
            // One place (`place`, or the first of `places`) and one sentence of reason.
            let place = match a.get("place") {
                Some(p) if !p.is_null() => {
                    parse_place(p).map_err(|e| format!("answer {} {e}", i + 1))?
                }
                _ => places.first().cloned().ok_or_else(|| {
                    format!(
                        "answer {} is `not_a_problem_here` and needs a `place`",
                        i + 1
                    )
                })?,
            };
            places = vec![place];
            reason = Some(
                text_field(a, "reason", false)
                    .map_err(|e| format!("answer {} {e}", i + 1))?
                    .trim()
                    .to_string(),
            );
        }
        if kind == AnswerKind::NotApplicable {
            // An optional one-sentence note for the reviewer; it makes no row.
            reason = a
                .get("reason")
                .and_then(|r| r.as_str())
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty());
        }
        let mut options = Vec::new();
        match a.get("options") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Array(items)) => {
                for o in items {
                    let id = text_field(o, "id", false)
                        .map_err(|e| format!("answer {} option {e}", i + 1))?;
                    let count = number_field(o, "count")
                        .map_err(|e| format!("answer {} option {e}", i + 1))?;
                    let mut places = parse_places(o.get("places"))
                        .map_err(|e| format!("answer {} option {e}", i + 1))?;
                    places.truncate(10);
                    options.push(OptionCount { id, count, places });
                }
            }
            Some(_) => return Err(format!("answer {} has `options` that is not a list", i + 1)),
        }
        file.answers.push(RuleAnswer {
            rule,
            kind,
            problems,
            places,
            reason,
            options,
        });
    }
    Ok(file)
}

fn parse_leads(v: &serde_json::Value) -> Result<Vec<Lead>, String> {
    let items = array_of(v, "leads")?;
    let mut out = Vec::new();
    for (i, l) in items.iter().enumerate() {
        let at = |e: String| format!("lead {} {e}", i + 1);
        out.push(Lead {
            n: 0,
            path: plain_path(text_field(l, "path", false).map_err(at)?.trim()),
            line: l
                .get("line")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0),
            what: lead_text(
                &text_field(l, "what", false).map_err(|e| format!("lead {} {e}", i + 1))?,
            ),
            source_problem: None,
            severity: None,
        });
    }
    Ok(out)
}

fn parse_unread(v: &serde_json::Value) -> Result<Vec<String>, String> {
    let items = array_of(v, "unread")?;
    items
        .iter()
        .map(|x| {
            x.as_str()
                .map(|s| plain_path(s.trim()))
                .ok_or_else(|| "must hold file paths as text".to_string())
        })
        .collect()
}

/// Read and check the output files. `piece_files` are the piece's own paths: a problem in any
/// other file is moved to the leads. A file that cannot be used is listed with the reason (the
/// caller gives the agent one repair turn); its entries are not used.
pub fn parse_outputs(outputs: &BTreeMap<String, String>, piece_files: &[String]) -> PieceOutput {
    parse_outputs_joined(outputs, piece_files, &HashSet::new())
}

/// [`parse_outputs`] where the files named in `joined` hold the engine's own joined text (a base
/// plus a later rewrite): those are not held to [`MAX_PROBLEMS_PER_FILE`], which limits what the
/// agent writes in one go, not what the engine puts together from several steps.
pub fn parse_outputs_joined(
    outputs: &BTreeMap<String, String>,
    piece_files: &[String],
    joined: &HashSet<String>,
) -> PieceOutput {
    let mut out = PieceOutput::default();
    let in_piece: HashSet<String> = piece_files.iter().map(|p| plain_path(p)).collect();
    let mut named: Vec<(&String, &str)> = outputs.keys().map(|k| (k, file_name(k))).collect();
    named.sort_by_key(|(key, name)| {
        (
            problems_file_number(name).unwrap_or(u64::MAX),
            (*key).clone(),
        )
    });
    let mut seen_numbers: HashSet<u32> = HashSet::new();
    for (key, name) in named {
        let text = &outputs[key];
        let unusable = |reason: String| Unusable {
            file: key.clone(),
            reason,
        };
        let cap = if joined.contains(key) {
            usize::MAX
        } else {
            MAX_PROBLEMS_PER_FILE
        };
        if problems_file_number(name).is_some() || stray_problems_list(name, text, cap) {
            out.problems_files += 1;
            let value: serde_json::Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(e) => {
                    out.unusable
                        .push(unusable(format!("is not valid JSON: {e}")));
                    continue;
                }
            };
            let items = match array_of(&value, "problems") {
                Ok(items) => items,
                Err(e) => {
                    out.unusable.push(unusable(e));
                    continue;
                }
            };
            if items.len() > cap {
                out.unusable.push(unusable(format!(
                    "holds {} problems; at most {MAX_PROBLEMS_PER_FILE} to a file",
                    items.len()
                )));
                continue;
            }
            let mut parsed = Vec::new();
            let mut failure = None;
            for (i, item) in items.iter().enumerate() {
                match parse_problem(item) {
                    Ok(p) => parsed.push(p),
                    Err(e) => {
                        failure = Some(format!("entry {} {e}", i + 1));
                        break;
                    }
                }
            }
            if failure.is_none() {
                let mut local: HashSet<u32> = HashSet::new();
                for p in &parsed {
                    if seen_numbers.contains(&p.n) || !local.insert(p.n) {
                        failure = Some(format!("problem number {} is used twice", p.n));
                        break;
                    }
                }
            }
            if let Some(reason) = failure {
                out.unusable.push(unusable(reason));
                continue;
            }
            for (mut p, item) in parsed.into_iter().zip(items.iter()) {
                seen_numbers.insert(p.n);
                if in_piece.contains(&p.path) {
                    let listed = parse_problem_places(item, p.n, &mut out.notes);
                    for place in listed {
                        if in_piece.contains(&place.path) {
                            if !p.places.contains(&place) {
                                p.places.push(place);
                            }
                        } else {
                            // A place outside the part is a lead, as a problem whose own path is
                            // outside the part is.
                            out.leads.push(Lead {
                                n: 0,
                                path: place.path,
                                line: place.line,
                                what: lead_text(&format!("{}. {}", p.title, p.what)),
                                source_problem: Some(p.n),
                                severity: Some(p.severity.clone()),
                            });
                        }
                    }
                    out.problems.push(p);
                } else {
                    out.leads.push(Lead {
                        n: 0,
                        path: p.path,
                        line: p.line,
                        what: lead_text(&format!("{}. {}", p.title, p.what)),
                        source_problem: Some(p.n),
                        severity: Some(p.severity),
                    });
                }
            }
            continue;
        }
        match name {
            "checklist.json" => match serde_json::from_str::<serde_json::Value>(text) {
                Ok(v) => match parse_checklist(&v) {
                    Ok(file) => out.checklist = Some(file),
                    Err(e) => out.unusable.push(unusable(e)),
                },
                Err(e) => out
                    .unusable
                    .push(unusable(format!("is not valid JSON: {e}"))),
            },
            "leads.json" => match serde_json::from_str::<serde_json::Value>(text) {
                Ok(v) => match parse_leads(&v) {
                    Ok(leads) => out.leads.extend(leads),
                    Err(e) => out.unusable.push(unusable(e)),
                },
                Err(e) => out
                    .unusable
                    .push(unusable(format!("is not valid JSON: {e}"))),
            },
            "unread.json" => match serde_json::from_str::<serde_json::Value>(text) {
                Ok(v) => match parse_unread(&v) {
                    Ok(list) => out.unread = Some(list),
                    Err(e) => out.unusable.push(unusable(e)),
                },
                Err(e) => out
                    .unusable
                    .push(unusable(format!("is not valid JSON: {e}"))),
            },
            "leads-answers.json" => out.leads_answers = Some(text.clone()),
            "card.md" => out.card = Some(crate::client_text::plain(text).into_owned()),
            // A file nobody reads is visible, never ignored: the agent may have meant it as its
            // problems file (`problems.json`).
            _ => out.unusable.push(unusable(format!(
                "is not a file this review uses; write problems to {}problems-1.json, then problems-2.json and so on",
                super::room::OUT_DIR
            ))),
        }
    }
    for (i, lead) in out.leads.iter_mut().enumerate() {
        lead.n = u32::try_from(i + 1).unwrap_or(u32::MAX);
    }
    out
}

/// The rule text a problem is keyed by in the scan reply. Only an exact rule id cites a rule: a
/// citation from the checklist step, else a label that is exactly an adopted rule id (the
/// checklist asks the agent to write the rule id as the label of a problem a rule turned up).
/// Any other label is free text and stays free text.
fn rule_for<'a>(
    p: &'a Problem,
    citations: &'a BTreeMap<u32, Vec<String>>,
    adopted: Option<&HashSet<String>>,
) -> (&'a str, bool) {
    if let Some(first) = citations.get(&p.n).and_then(|c| c.first()) {
        return (first.as_str(), true);
    }
    let label = p.label.trim();
    let exact = adopted.is_some_and(|a| a.contains(&label.to_ascii_uppercase()));
    (label, exact)
}

/// The scan reply (`{"findings": [...]}`) for `parse_ai_findings`. `citations` maps a problem
/// number to the checklist rules it breaks, strictest first (see [`citation_map`]); a problem without one is keyed by
/// its label. The detail is what is wrong, then why it matters.
pub fn scan_reply(problems: &[Problem], citations: &BTreeMap<u32, Vec<String>>) -> String {
    let findings: Vec<serde_json::Value> = problems
        .iter()
        .map(|p| {
            let (rule, _) = rule_for(p, citations, None);
            let detail = [p.what.trim(), p.why.trim()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            serde_json::json!({
                "path": p.path,
                "line": p.line,
                "severity": p.severity,
                "rule": rule,
                "title": p.title,
                "code": p.code,
                "detail": detail,
            })
        })
        .collect();
    serde_json::json!({ "findings": findings }).to_string()
}

/// How strict a rule's severity range is, for ordering a problem's cited rules: a rule with no
/// maximum before one with a maximum, then the higher minimum, then the id. A rule that is not
/// on the checklist sorts after every listed one.
fn strictness_key(rule: &str, checklist: &Checklist) -> (u8, u8, u8, String) {
    let min_rank = |s: &Option<String>| match s.as_deref().map(str::to_ascii_lowercase) {
        Some(m) if m == "critical" => 4,
        Some(m) if m == "high" => 3,
        Some(m) if m == "medium" => 2,
        Some(m) if m == "low" => 1,
        _ => 0,
    };
    match checklist
        .rules
        .iter()
        .find(|r| r.id.eq_ignore_ascii_case(rule))
    {
        Some(r) => (
            0,
            u8::from(r.max_severity.is_some()),
            5 - min_rank(&r.min_severity),
            rule.to_string(),
        ),
        None => (1, 0, 0, rule.to_string()),
    }
}

/// The citations of a checked piece as `scan_reply` wants them: for each problem the rule ids it
/// was cited to (uppercase, none twice), strictest first, so the first rule (the finding's rule,
/// the one `severity_bounds` reads) never depends on the order the model wrote its citations in.
/// Strictest is: a rule with no maximum before one with a maximum, then the higher minimum, then
/// the id.
pub fn citation_map(checklist: &Checklist, citations: &[Citation]) -> BTreeMap<u32, Vec<String>> {
    let mut map: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for c in citations {
        let rule = c.rule.trim().to_ascii_uppercase();
        let list = map.entry(c.problem).or_default();
        if !list.contains(&rule) {
            list.push(rule);
        }
    }
    for list in map.values_mut() {
        list.sort_by_cached_key(|r| strictness_key(r, checklist));
    }
    map
}

/// Turn problems into findings. `adopted` are the checklist's rule ids (uppercase). The fix, the
/// end line, the piece key, `sure`, `effort`, the problem number and every cited rule id ride on
/// each finding as operator-only captures (`finder-*`, all bookkeeping keys); no report template
/// reads them. The first cited rule is the finding's rule and the others are its
/// `also_matches`. A label that is free text is never matched to a rule, however close it is to
/// one: the shared parser's near-match is undone for it and the finding keeps an `AI-` id.
pub fn to_findings(
    repo: &str,
    piece_key: &str,
    problems: &[Problem],
    citations: &BTreeMap<u32, Vec<String>>,
    adopted: &HashSet<String>,
) -> (Vec<Finding>, Vec<ProposedRule>) {
    let reply = scan_reply(problems, citations);
    let (mut findings, proposed) = crate::ai_audit::parse_ai_findings(repo, &reply, adopted);
    for (finding, p) in findings.iter_mut().zip(problems) {
        let (_, cites) = rule_for(p, citations, Some(adopted));
        if !cites && adopted.contains(&finding.rule_id) {
            let label = crate::ai_audit::digest_parse::normalize_model_rule_label(&p.label);
            finding.rule_id = format!(
                "AI-{}",
                if label.is_empty() {
                    "ARCHITECTURE"
                } else {
                    &label
                }
            );
        }
        let cited = citations.get(&p.n).cloned().unwrap_or_default();
        if cites && cited.len() > 1 {
            for other in cited.iter().skip(1) {
                if *other != finding.rule_id && !finding.also_matches.contains(other) {
                    finding.also_matches.push(other.clone());
                }
            }
        }
        let c = &mut finding.captures;
        let fix = crate::client_text::plain(p.fix.trim()).into_owned();
        if !fix.is_empty() {
            c.insert(FINDER_FIX_CAPTURE.to_string(), fix);
        }
        if let Some(end) = p.end_line {
            c.insert(FINDER_END_LINE_CAPTURE.to_string(), end.to_string());
        }
        c.insert(FINDER_PIECE_CAPTURE.to_string(), piece_key.to_string());
        c.insert(
            FINDER_SURE_CAPTURE.to_string(),
            if p.sure { "yes" } else { "unsure" }.to_string(),
        );
        c.insert(FINDER_EFFORT_CAPTURE.to_string(), p.effort.clone());
        c.insert(FINDER_PROBLEM_CAPTURE.to_string(), p.n.to_string());
        if !cited.is_empty() {
            c.insert(FINDER_RULES_CAPTURE.to_string(), cited.join(","));
        }
    }
    (findings, proposed)
}

/// How the joints agent answered one lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadOutcome {
    /// A problem already reported covers it (and that problem is at the lead's file).
    CoveredBy(u32),
    /// The agent wrote this new problem for it (and it exists).
    NewProblem(u32),
}

/// One answer of `leads-answers.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadAnswer {
    pub lead: u32,
    pub outcome: LeadOutcome,
}

/// `leads-answers.json` checked against the leads and the problems.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LeadAnswers {
    /// Answers that hold up.
    pub answered: Vec<LeadAnswer>,
    /// Leads with no answer at all.
    pub unanswered: Vec<u32>,
    /// Leads whose answer does not hold up, with the reason (a `covered_by` problem that does
    /// not exist or is at another file, a `new_problem` that does not exist, neither field).
    pub invalid: Vec<(u32, String)>,
    /// Answers that name no lead, with the number they gave.
    pub unknown_leads: Vec<u32>,
    /// Why the whole file could not be read (not JSON, not a list); every lead is then
    /// unanswered.
    pub unusable: Option<String>,
}

impl LeadAnswers {
    /// The leads the runner turns into a problem needing a decision: those with no answer and
    /// those whose answer is invalid, sorted and without repeats.
    pub fn unresolved(&self) -> Vec<u32> {
        let mut all: Vec<u32> = self
            .unanswered
            .iter()
            .copied()
            .chain(self.invalid.iter().map(|(n, _)| *n))
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}

/// Read `leads-answers.json` (`text`; `None` when the agent wrote none). Every lead must be
/// answered, in one of two ways: `covered_by` a problem number (that problem must exist and be
/// at the lead's file) or `new_problem` (that problem must exist). `problems` are those the
/// joints agent wrote together with the ones already reported.
pub fn parse_leads_answers(
    text: Option<&str>,
    leads: &[Lead],
    problems: &[(u32, String)],
) -> LeadAnswers {
    let mut out = LeadAnswers::default();
    let all_unanswered = |out: &mut LeadAnswers, why: Option<String>| {
        out.unanswered = leads.iter().map(|l| l.n).collect();
        out.unusable = why;
    };
    let Some(text) = text else {
        all_unanswered(&mut out, None);
        return out;
    };
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            all_unanswered(&mut out, Some(format!("is not valid JSON: {e}")));
            return out;
        }
    };
    let items = match array_of(&value, "answers") {
        Ok(items) => items,
        Err(e) => {
            all_unanswered(&mut out, Some(e));
            return out;
        }
    };
    let mut answered_leads: HashSet<u32> = HashSet::new();
    for item in items {
        let Some(lead_n) = item
            .get("lead")
            .or_else(|| item.get("n"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
        else {
            out.invalid
                .push((0, "an answer has no lead number".to_string()));
            continue;
        };
        let Some(lead) = leads.iter().find(|l| l.n == lead_n) else {
            out.unknown_leads.push(lead_n);
            continue;
        };
        let number = |key: &str| {
            item.get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        let outcome = match (number("covered_by"), number("new_problem")) {
            (Some(p), None) => match problems.iter().find(|(n, _)| *n == p) {
                Some((_, path)) if plain_path(path) == plain_path(&lead.path) => {
                    Ok(LeadOutcome::CoveredBy(p))
                }
                Some(_) => Err(format!(
                    "covered_by problem {p}, which is not at {}",
                    lead.path
                )),
                None => Err(format!("covered_by problem {p}, which does not exist")),
            },
            (None, Some(p)) => {
                if problems.iter().any(|(n, _)| *n == p) {
                    Ok(LeadOutcome::NewProblem(p))
                } else {
                    Err(format!("new_problem {p}, which does not exist"))
                }
            }
            (Some(_), Some(_)) => Err("gives both covered_by and new_problem".to_string()),
            (None, None) => Err("gives neither covered_by nor new_problem".to_string()),
        };
        match outcome {
            Ok(outcome) => {
                if answered_leads.insert(lead_n) {
                    out.answered.push(LeadAnswer {
                        lead: lead_n,
                        outcome,
                    });
                }
            }
            Err(reason) => out.invalid.push((lead_n, reason)),
        }
    }
    // A lead that got a good answer after a bad one is answered.
    out.invalid.retain(|(n, _)| !answered_leads.contains(n));
    let invalid_leads: HashSet<u32> = out.invalid.iter().map(|(n, _)| *n).collect();
    out.unanswered = leads
        .iter()
        .map(|l| l.n)
        .filter(|n| !answered_leads.contains(n) && !invalid_leads.contains(n))
        .collect();
    out
}
