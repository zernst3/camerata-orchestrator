//! Realtime Database rules: JSON with JavaScript-like string expressions.
//!
//! `database.rules.json` (or whatever `firebase.json` names) holds a tree under a top-level
//! `"rules"` key. At each node `.read`, `.write`, `.validate` and `.indexOn` are rule keys and
//! every other key is a child path segment (`$var` keys are wildcards). `.read` and `.write`
//! CASCADE (Firebase: ".read and .write rules cascade"): a shallower grant takes precedence over
//! a deeper rule, so a child can only grant more access, never less.

use super::json_lite::{self, Json};
use super::rules_model::Cond;
use super::rules_parse::ParseError;

/// Which grant a rule gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Read,
    Write,
}

impl Key {
    /// The key as written (`.read`).
    pub fn as_str(self) -> &'static str {
        match self {
            Key::Read => ".read",
            Key::Write => ".write",
        }
    }
}

/// One `.read` or `.write` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtdbRule {
    pub cond: Cond,
    /// The value as written: `true`, `false` or the expression string.
    pub raw: String,
    pub line: usize,
}

/// One node of the rules tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtdbNode {
    pub name: String,
    /// `/users/$uid`; `/` for the root.
    pub path: String,
    pub line: usize,
    pub read: Option<RtdbRule>,
    pub write: Option<RtdbRule>,
    pub children: Vec<RtdbNode>,
}

impl RtdbNode {
    /// The rule this node sets for `key`.
    pub fn rule(&self, key: Key) -> Option<&RtdbRule> {
        match key {
            Key::Read => self.read.as_ref(),
            Key::Write => self.write.as_ref(),
        }
    }
}

/// A Realtime Database rules file as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtdbFile {
    pub root: RtdbNode,
    pub errors: Vec<ParseError>,
}

/// A node with its ancestors (root first).
#[derive(Debug, Clone)]
pub struct RtdbFlat<'a> {
    pub node: &'a RtdbNode,
    pub ancestors: Vec<&'a RtdbNode>,
}

impl<'a> RtdbFlat<'a> {
    /// The SHALLOWEST node on the way down (ancestors first, then this node) whose rule for `key`
    /// satisfies `grants`: the one whose grant cascades to this node and takes precedence.
    pub fn granting(
        &self,
        key: Key,
        grants: impl Fn(&Cond) -> bool,
    ) -> Option<(&'a RtdbNode, &'a RtdbRule)> {
        self.ancestors
            .iter()
            .copied()
            .chain(std::iter::once(self.node))
            .find_map(|n| n.rule(key).filter(|r| grants(&r.cond)).map(|r| (n, r)))
    }
}

impl RtdbFile {
    pub fn is_degraded(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Every node, depth first, with its ancestors.
    pub fn flatten(&self) -> Vec<RtdbFlat<'_>> {
        let mut out = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut out);
        out
    }
}

fn walk<'a>(node: &'a RtdbNode, ancestors: &mut Vec<&'a RtdbNode>, out: &mut Vec<RtdbFlat<'a>>) {
    out.push(RtdbFlat {
        node,
        ancestors: ancestors.clone(),
    });
    ancestors.push(node);
    for child in &node.children {
        walk(child, ancestors, out);
    }
    ancestors.pop();
}

/// Read Realtime Database rules JSON. Never panics; a file that is not a JSON object with a
/// `rules` object, or whose JSON does not parse, is degraded.
pub fn parse_rtdb(text: &str) -> RtdbFile {
    let (value, json_errors) = json_lite::parse(text);
    let mut errors: Vec<ParseError> = json_errors
        .into_iter()
        .map(|e| ParseError {
            line: e.line,
            what: e.what,
        })
        .collect();
    let mut root = RtdbNode {
        name: String::new(),
        path: "/".to_string(),
        line: 1,
        read: None,
        write: None,
        children: Vec::new(),
    };
    match value.as_ref().and_then(|v| v.get("rules")) {
        Some(member) if matches!(member.value, Json::Object(_)) => {
            root.line = member.line;
            fill(&mut root, &member.value, &mut errors, 0);
        }
        _ => errors.push(ParseError {
            line: 1,
            what: "no top-level \"rules\" object".to_string(),
        }),
    }
    RtdbFile { root, errors }
}

/// Deepest rules tree read; real trees are a handful of levels.
const MAX_DEPTH: usize = 40;

fn fill(node: &mut RtdbNode, value: &Json, errors: &mut Vec<ParseError>, depth: usize) {
    let Json::Object(members) = value else {
        return;
    };
    if depth > MAX_DEPTH {
        errors.push(ParseError {
            line: node.line,
            what: "rules nested too deeply".to_string(),
        });
        return;
    }
    for member in members {
        match member.key.as_str() {
            ".read" | ".write" => {
                let cond = match &member.value {
                    Json::Bool(b) => {
                        let raw = if *b { "true" } else { "false" };
                        Some((Cond::rtdb(raw), raw.to_string()))
                    }
                    Json::Str(s) => Some((Cond::rtdb(s), s.clone())),
                    _ => {
                        errors.push(ParseError {
                            line: member.line,
                            what: format!("a {} value this reader does not understand", member.key),
                        });
                        None
                    }
                };
                if let Some((cond, raw)) = cond {
                    let rule = RtdbRule {
                        cond,
                        raw,
                        line: member.line,
                    };
                    if member.key == ".read" {
                        node.read = Some(rule);
                    } else {
                        node.write = Some(rule);
                    }
                }
            }
            // `.validate` never cascades and never grants; `.indexOn` is an index hint.
            ".validate" | ".indexOn" => {}
            key if key.starts_with('.') => {}
            key => {
                if !matches!(member.value, Json::Object(_)) {
                    errors.push(ParseError {
                        line: member.line,
                        what: "a child path that is not an object".to_string(),
                    });
                    continue;
                }
                let mut child = RtdbNode {
                    name: key.to_string(),
                    path: if node.path == "/" {
                        format!("/{key}")
                    } else {
                        format!("{}/{key}", node.path)
                    },
                    line: member.line,
                    read: None,
                    write: None,
                    children: Vec::new(),
                };
                fill(&mut child, &member.value, errors, depth + 1);
                node.children.push(child);
            }
        }
    }
}
