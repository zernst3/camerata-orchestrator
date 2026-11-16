//! Reading the agent's output files (design section 2): `problems-*.json`, `checklist.json`,
//! `leads.json`, `card.md` and `unread.json`, each checked against a schema in code, and the
//! turn of the problems into the engine's `Finding` through `parse_ai_findings`.

use std::collections::{BTreeMap, HashSet};

use super::checklist::{AnswerKind, ChecklistFile, Citation, OptionCount, Place, RuleAnswer};
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
}

/// A problem the agent saw in a file outside its piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lead {
    pub path: String,
    pub line: usize,
    pub what: String,
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
}

/// The most problems one file may hold.
pub const MAX_PROBLEMS_PER_FILE: usize = 40;

const SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];
const EFFORTS: [&str; 3] = ["low", "medium", "high"];

/// A path the way the room names it.
fn plain_path(path: &str) -> String {
    path.trim_start_matches("./").to_string()
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
            "yes" => true,
            "unsure" => false,
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
    })
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
        let kind = match one_of(a, "answer", &["problems", "clean", "not_applicable"])
            .map_err(|e| format!("answer {} {e}", i + 1))?
            .as_str()
        {
            "problems" => AnswerKind::Problems,
            "clean" => AnswerKind::Clean,
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
        let places = parse_places(a.get("places")).map_err(|e| format!("answer {} {e}", i + 1))?;
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
            path: plain_path(text_field(l, "path", false).map_err(at)?.trim()),
            line: l
                .get("line")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0),
            what: text_field(l, "what", false).map_err(|e| format!("lead {} {e}", i + 1))?,
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
        if problems_file_number(name).is_some() {
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
            if items.len() > MAX_PROBLEMS_PER_FILE {
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
            for p in parsed {
                seen_numbers.insert(p.n);
                if in_piece.contains(&p.path) {
                    out.problems.push(p);
                } else {
                    out.leads.push(Lead {
                        path: p.path,
                        line: p.line,
                        what: format!("{}. {}", p.title, p.what),
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
            "card.md" => out.card = Some(crate::client_text::plain(text).into_owned()),
            _ => {}
        }
    }
    out
}

/// The scan reply (`{"findings": [...]}`) for `parse_ai_findings`. `citations` maps a problem
/// number to the checklist rule it breaks; a problem without one is keyed by its label. The
/// detail is what is wrong, then why it matters.
pub fn scan_reply(problems: &[Problem], citations: &BTreeMap<u32, String>) -> String {
    let findings: Vec<serde_json::Value> = problems
        .iter()
        .map(|p| {
            let rule = citations.get(&p.n).map_or(p.label.as_str(), String::as_str);
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

/// Turn problems into findings. `adopted` are the checklist's rule ids (uppercase). The fix, the
/// end line, the piece key, `sure`, `effort` and the problem number ride on each finding as
/// operator-only captures (`finder-*`); no report template reads them.
pub fn to_findings(
    repo: &str,
    piece_key: &str,
    problems: &[Problem],
    citations: &BTreeMap<u32, String>,
    adopted: &HashSet<String>,
) -> (Vec<Finding>, Vec<ProposedRule>) {
    let reply = scan_reply(problems, citations);
    let (mut findings, proposed) = crate::ai_audit::parse_ai_findings(repo, &reply, adopted);
    for (finding, p) in findings.iter_mut().zip(problems) {
        let c = &mut finding.captures;
        let fix = crate::client_text::plain(p.fix.trim()).into_owned();
        if !fix.is_empty() {
            c.insert("finder-fix".to_string(), fix);
        }
        if let Some(end) = p.end_line {
            c.insert("finder-end-line".to_string(), end.to_string());
        }
        c.insert("finder-piece".to_string(), piece_key.to_string());
        c.insert(
            "finder-sure".to_string(),
            if p.sure { "yes" } else { "unsure" }.to_string(),
        );
        c.insert("finder-effort".to_string(), p.effort.clone());
        c.insert("finder-problem".to_string(), p.n.to_string());
    }
    (findings, proposed)
}
