//! The reader for Cloud Firestore and Cloud Storage Security Rules.
//!
//! Both are the same small language (Firebase: Storage "leverages a superset of the Common
//! Expression Language"): `rules_version`, a `service` block, nested `match` blocks, `allow`
//! statements with an optional `if` condition, and `function` definitions. This module reads the
//! block and statement structure; it does not evaluate conditions (that is [`super::rules_model`]).
//!
//! # Hardening contract
//!
//! The input is untrusted, hand-edited and possibly truncated, so, like
//! `supabase/splitter.rs`: this never panics, every loop advances, it works on a `Vec<char>`
//! (never a byte index), and an unterminated construct runs to EOF. One addition the
//! false-negative law requires: every place the reader could not account for the input is
//! recorded in [`RulesFile::errors`], and a file with any error is **degraded**. A degraded file
//! produces a "not assessed" row and never a clean result (see `mod.rs`).

use std::collections::BTreeSet;

/// One problem met while reading: the 1-based line and a short phrase for a client sentence
/// ("line 41: a statement this reader does not understand").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub what: String,
}

/// The `service` a rules file declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    /// `cloud.firestore`.
    Firestore,
    /// `firebase.storage`, and the legacy `cloud.storage` the Storage docs mention.
    Storage,
    /// Anything else: not a Firestore or Storage rules file.
    Other(String),
}

/// One request method an `allow` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Method {
    Read,
    Write,
    Get,
    List,
    Create,
    Update,
    Delete,
}

impl Method {
    fn parse(word: &str) -> Option<Method> {
        Some(match word {
            "read" => Method::Read,
            "write" => Method::Write,
            "get" => Method::Get,
            "list" => Method::List,
            "create" => Method::Create,
            "update" => Method::Update,
            "delete" => Method::Delete,
            _ => return None,
        })
    }

    /// The word as written in a rules file.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Read => "read",
            Method::Write => "write",
            Method::Get => "get",
            Method::List => "list",
            Method::Create => "create",
            Method::Update => "update",
            Method::Delete => "delete",
        }
    }
}

/// One path segment of a `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    /// `{name}` (one segment) or `{name=**}` (recursive).
    Wildcard {
        name: String,
        recursive: bool,
    },
    /// A segment holding `$(...)`.
    Interpolated(String),
}

impl Segment {
    /// The segment as written (`/orders`, `{id}`, `{document=**}`).
    pub fn render(&self) -> String {
        match self {
            Segment::Literal(s) | Segment::Interpolated(s) => s.clone(),
            Segment::Wildcard { name, recursive } => {
                if *recursive {
                    format!("{{{name}=**}}")
                } else {
                    format!("{{{name}}}")
                }
            }
        }
    }
}

/// Render a path as `/a/{b}/c`.
pub fn render_path(segments: &[Segment]) -> String {
    let mut out = String::new();
    for s in segments {
        out.push('/');
        out.push_str(&s.render());
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// One `allow` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allow {
    pub methods: BTreeSet<Method>,
    /// The condition text after `if` (comments removed). `None` means no condition: Firebase
    /// says a missing condition "means the request is always allowed".
    pub condition: Option<String>,
    /// 1-based line of the `allow` keyword.
    pub line: usize,
}

/// One `function` definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<String>,
    pub lets: Vec<(String, String)>,
    pub ret: Option<String>,
    pub line: usize,
}

/// One `match` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchBlock {
    /// This block's own path segments.
    pub segments: Vec<Segment>,
    /// The ancestors' segments followed by this block's own (Firebase: "the path of the inner
    /// `match` statement is always relative to the path of the outer `match` statement").
    pub effective_path: Vec<Segment>,
    pub allows: Vec<Allow>,
    pub functions: Vec<FnDef>,
    pub children: Vec<MatchBlock>,
    pub line: usize,
}

/// A rules file as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesFile {
    /// `rules_version = '2'` gives `Some(2)`; absent means the `v1` engine (`None`).
    pub version: Option<u32>,
    pub service: Option<Service>,
    pub root: Vec<MatchBlock>,
    /// Functions defined at service level.
    pub functions: Vec<FnDef>,
    /// Everything the reader could not account for. Non-empty means degraded.
    pub errors: Vec<ParseError>,
}

impl RulesFile {
    /// Whether the reader understood the whole file.
    pub fn is_degraded(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Every match block with the functions and wildcard names in scope, depth first.
    pub fn flatten(&self) -> Vec<FlatBlock<'_>> {
        let mut out = Vec::new();
        let scope = Scope {
            functions: self.functions.iter().collect(),
            wildcards: Vec::new(),
        };
        for block in &self.root {
            flatten_into(block, &scope, &mut out);
        }
        out
    }
}

/// What is in scope inside a block: the functions it may call (innermost first) and the wildcard
/// names its path binds.
#[derive(Debug, Clone, Default)]
pub struct Scope<'a> {
    pub functions: Vec<&'a FnDef>,
    pub wildcards: Vec<String>,
}

/// A match block with its scope.
#[derive(Debug, Clone)]
pub struct FlatBlock<'a> {
    pub block: &'a MatchBlock,
    pub scope: Scope<'a>,
}

fn flatten_into<'a>(block: &'a MatchBlock, parent: &Scope<'a>, out: &mut Vec<FlatBlock<'a>>) {
    let mut scope = Scope {
        functions: block.functions.iter().collect(),
        wildcards: parent.wildcards.clone(),
    };
    scope.functions.extend(parent.functions.iter().copied());
    for segment in &block.segments {
        if let Segment::Wildcard { name, .. } = segment {
            scope.wildcards.push(name.clone());
        }
    }
    out.push(FlatBlock {
        block,
        scope: scope.clone(),
    });
    for child in &block.children {
        flatten_into(child, &scope, out);
    }
}

// ── lexing: events ──────────────────────────────────────────────────────────────────────────

enum Event {
    /// A header that opened a block (`service x`, `match /p`, `function f(a)`).
    Open(String, usize),
    Close(usize),
    /// A statement ended by `;`.
    Stmt(String, usize),
}

struct Lexed {
    events: Vec<Event>,
    errors: Vec<ParseError>,
}

fn lex(text: &str) -> Lexed {
    let chars: Vec<char> = text.replace("\r\n", "\n").chars().collect();
    let n = chars.len();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let mut cur = String::new();
    let mut cur_line: Option<usize> = None;
    let mut line = 1usize;
    let mut paren = 0usize;
    let mut i = 0usize;

    // A single collapsing space once a statement has started.
    fn sep(cur: &mut String, started: bool) {
        if started && !cur.is_empty() && !cur.ends_with(' ') {
            cur.push(' ');
        }
    }

    while i < n {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                sep(&mut cur, cur_line.is_some());
                i += 1;
            }
            c if c.is_whitespace() => {
                sep(&mut cur, cur_line.is_some());
                i += 1;
            }
            '/' if i + 1 < n && chars[i + 1] == '/' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                sep(&mut cur, cur_line.is_some());
            }
            '/' if i + 1 < n && chars[i + 1] == '*' => {
                let start_line = line;
                i += 2;
                let mut closed = false;
                while i < n {
                    if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                if !closed {
                    errors.push(ParseError {
                        line: start_line,
                        what: "a comment that is never closed".to_string(),
                    });
                }
                sep(&mut cur, cur_line.is_some());
            }
            '\'' | '"' => {
                let quote = c;
                let start_line = line;
                cur_line.get_or_insert(line);
                cur.push(c);
                i += 1;
                let mut closed = false;
                while i < n {
                    let d = chars[i];
                    cur.push(d);
                    i += 1;
                    if d == '\\' && i < n {
                        if chars[i] == '\n' {
                            line += 1;
                        }
                        cur.push(chars[i]);
                        i += 1;
                    } else if d == '\n' {
                        line += 1;
                    } else if d == quote {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    errors.push(ParseError {
                        line: start_line,
                        what: "a string that is never closed".to_string(),
                    });
                }
            }
            '(' | '[' => {
                paren += 1;
                cur_line.get_or_insert(line);
                cur.push(c);
                i += 1;
            }
            ')' | ']' => {
                paren = paren.saturating_sub(1);
                cur_line.get_or_insert(line);
                cur.push(c);
                i += 1;
            }
            '{' => {
                if paren > 0 {
                    cur.push(c);
                    i += 1;
                } else if cur.starts_with("match ") && cur.ends_with('/') {
                    // A path wildcard (`/{userId}`, `/{doc=**}`), not a block.
                    cur_line.get_or_insert(line);
                    cur.push('{');
                    i += 1;
                    let mut closed = false;
                    while i < n {
                        let d = chars[i];
                        i += 1;
                        if d == '\n' {
                            line += 1;
                        }
                        cur.push(d);
                        if d == '}' {
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        errors.push(ParseError {
                            line,
                            what: "a path wildcard that is never closed".to_string(),
                        });
                    }
                } else {
                    events.push(Event::Open(
                        cur.trim().to_string(),
                        cur_line.unwrap_or(line),
                    ));
                    cur.clear();
                    cur_line = None;
                    i += 1;
                }
            }
            '}' => {
                if paren > 0 {
                    cur.push(c);
                    i += 1;
                } else {
                    if !cur.trim().is_empty() {
                        errors.push(ParseError {
                            line: cur_line.unwrap_or(line),
                            what: "a statement with no closing ';'".to_string(),
                        });
                        events.push(Event::Stmt(
                            cur.trim().to_string(),
                            cur_line.unwrap_or(line),
                        ));
                    }
                    cur.clear();
                    cur_line = None;
                    events.push(Event::Close(line));
                    i += 1;
                }
            }
            ';' => {
                if paren > 0 {
                    cur.push(c);
                } else {
                    if !cur.trim().is_empty() {
                        events.push(Event::Stmt(
                            cur.trim().to_string(),
                            cur_line.unwrap_or(line),
                        ));
                    }
                    cur.clear();
                    cur_line = None;
                }
                i += 1;
            }
            other => {
                cur_line.get_or_insert(line);
                cur.push(other);
                i += 1;
            }
        }
    }
    if !cur.trim().is_empty() {
        errors.push(ParseError {
            line: cur_line.unwrap_or(line),
            what: "a statement with no closing ';'".to_string(),
        });
    }
    Lexed { events, errors }
}

// ── parsing: events to tree ─────────────────────────────────────────────────────────────────

enum Frame {
    Service,
    Match(MatchBlock),
    Function(FnDef),
    /// A block the reader does not understand: its contents are skipped, balanced.
    Skip,
}

/// Read `text` as a Firestore or Storage rules file. Never panics.
pub fn parse_rules(text: &str) -> RulesFile {
    let Lexed { events, errors } = lex(text);
    let mut file = RulesFile {
        version: None,
        service: None,
        root: Vec::new(),
        functions: Vec::new(),
        errors,
    };
    let mut stack: Vec<(Frame, usize)> = Vec::new();

    for event in events {
        match event {
            Event::Open(header, line) => open_block(&mut file, &mut stack, &header, line),
            Event::Close(line) => close_block(&mut file, &mut stack, line),
            Event::Stmt(stmt, line) => statement(&mut file, &mut stack, &stmt, line),
        }
    }
    for (_, line) in stack.drain(..) {
        file.errors.push(ParseError {
            line,
            what: "a block that is never closed".to_string(),
        });
    }
    let parents: Vec<Segment> = Vec::new();
    for block in &mut file.root {
        set_effective_paths(block, &parents);
    }
    file.errors.sort_by_key(|e| e.line);
    file
}

fn set_effective_paths(block: &mut MatchBlock, parent: &[Segment]) {
    let mut path = parent.to_vec();
    path.extend(block.segments.iter().cloned());
    block.effective_path = path.clone();
    for child in &mut block.children {
        set_effective_paths(child, &path);
    }
}

/// Deepest block nesting read. Real rules nest a handful of levels; the cap keeps the recursive
/// passes below bounded on a hostile file.
const MAX_DEPTH: usize = 64;

fn open_block(file: &mut RulesFile, stack: &mut Vec<(Frame, usize)>, header: &str, line: usize) {
    if stack.len() >= MAX_DEPTH {
        file.errors.push(ParseError {
            line,
            what: "blocks nested too deeply".to_string(),
        });
        stack.push((Frame::Skip, line));
        return;
    }
    let (word, rest) = header
        .split_once(' ')
        .map_or((header, ""), |(w, r)| (w, r.trim()));
    match word {
        "service" => {
            if !stack.is_empty() || file.service.is_some() {
                file.errors.push(ParseError {
                    line,
                    what: "a second or nested service block".to_string(),
                });
                stack.push((Frame::Skip, line));
                return;
            }
            file.service = Some(match rest {
                "cloud.firestore" => Service::Firestore,
                "firebase.storage" | "cloud.storage" => Service::Storage,
                other => Service::Other(other.to_string()),
            });
            stack.push((Frame::Service, line));
        }
        "match" => {
            let inside_service = matches!(
                stack.last().map(|(f, _)| f),
                Some(Frame::Service | Frame::Match(_))
            );
            if !inside_service {
                file.errors.push(ParseError {
                    line,
                    what: "a match block outside a service".to_string(),
                });
                stack.push((Frame::Skip, line));
                return;
            }
            let segments = parse_path(rest, line, &mut file.errors);
            stack.push((
                Frame::Match(MatchBlock {
                    segments,
                    effective_path: Vec::new(),
                    allows: Vec::new(),
                    functions: Vec::new(),
                    children: Vec::new(),
                    line,
                }),
                line,
            ));
        }
        "function" => {
            let Some((name, params)) = parse_function_header(rest) else {
                file.errors.push(ParseError {
                    line,
                    what: "a function header this reader does not understand".to_string(),
                });
                stack.push((Frame::Skip, line));
                return;
            };
            stack.push((
                Frame::Function(FnDef {
                    name,
                    params,
                    lets: Vec::new(),
                    ret: None,
                    line,
                }),
                line,
            ));
        }
        _ => {
            file.errors.push(ParseError {
                line,
                what: "a block this reader does not understand".to_string(),
            });
            stack.push((Frame::Skip, line));
        }
    }
}

fn close_block(file: &mut RulesFile, stack: &mut Vec<(Frame, usize)>, line: usize) {
    let Some((frame, _)) = stack.pop() else {
        file.errors.push(ParseError {
            line,
            what: "a '}' with no matching '{'".to_string(),
        });
        return;
    };
    match frame {
        Frame::Service | Frame::Skip => {}
        Frame::Match(block) => match stack.last_mut() {
            Some((Frame::Match(parent), _)) => parent.children.push(block),
            _ => file.root.push(block),
        },
        Frame::Function(def) => {
            if def.ret.is_none() {
                file.errors.push(ParseError {
                    line: def.line,
                    what: "a function with no return statement".to_string(),
                });
            }
            match stack.last_mut() {
                Some((Frame::Match(parent), _)) => parent.functions.push(def),
                _ => file.functions.push(def),
            }
        }
    }
}

fn statement(file: &mut RulesFile, stack: &mut [(Frame, usize)], stmt: &str, line: usize) {
    match stack.last_mut().map(|(f, _)| f) {
        None => top_level_statement(file, stmt, line),
        Some(Frame::Match(block)) => {
            if let Some(rest) = stmt.strip_prefix("allow ") {
                match parse_allow(rest.trim(), line) {
                    Ok(allow) => block.allows.push(allow),
                    Err(what) => file.errors.push(ParseError { line, what }),
                }
            } else {
                file.errors.push(ParseError {
                    line,
                    what: "a statement this reader does not understand".to_string(),
                });
            }
        }
        Some(Frame::Function(def)) => {
            if let Some(rest) = stmt.strip_prefix("return ") {
                if def.ret.is_some() {
                    file.errors.push(ParseError {
                        line,
                        what: "a function with more than one return statement".to_string(),
                    });
                } else {
                    def.ret = Some(rest.trim().to_string());
                }
            } else if let Some(rest) = stmt.strip_prefix("let ") {
                match rest.split_once('=') {
                    Some((name, expr)) if is_identifier(name.trim()) => {
                        def.lets
                            .push((name.trim().to_string(), expr.trim().to_string()));
                    }
                    _ => file.errors.push(ParseError {
                        line,
                        what: "a let statement this reader does not understand".to_string(),
                    }),
                }
            } else {
                file.errors.push(ParseError {
                    line,
                    what: "a statement this reader does not understand".to_string(),
                });
            }
        }
        Some(Frame::Skip) => {}
        Some(Frame::Service) => file.errors.push(ParseError {
            line,
            what: "a statement this reader does not understand".to_string(),
        }),
    }
}

fn top_level_statement(file: &mut RulesFile, stmt: &str, line: usize) {
    let compact: String = stmt.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(value) = compact.strip_prefix("rules_version=") {
        match value.trim_matches(['\'', '"']).parse::<u32>() {
            Ok(v) => file.version = Some(v),
            Err(_) => file.errors.push(ParseError {
                line,
                what: "a rules_version this reader does not understand".to_string(),
            }),
        }
    } else {
        file.errors.push(ParseError {
            line,
            what: "a statement this reader does not understand".to_string(),
        });
    }
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `name(a, b)` to `("name", ["a", "b"])`.
fn parse_function_header(rest: &str) -> Option<(String, Vec<String>)> {
    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    if close < open || !rest[close + 1..].trim().is_empty() {
        return None;
    }
    let name = rest[..open].trim();
    if !is_identifier(name) {
        return None;
    }
    let params: Vec<String> = rest[open + 1..close]
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if !params.iter().all(|p| is_identifier(p)) {
        return None;
    }
    Some((name.to_string(), params))
}

/// `read, write: if <expr>` or `get;`-style (the `;` is already gone).
fn parse_allow(rest: &str, line: usize) -> Result<Allow, String> {
    let (methods_text, condition) = match rest.split_once(':') {
        Some((m, c)) => {
            let c = c.trim();
            let expr = c
                .strip_prefix("if")
                .filter(|r| r.starts_with(|ch: char| ch.is_whitespace() || ch == '('))
                .map(str::trim)
                .ok_or_else(|| "an allow with something other than 'if' after ':'".to_string())?
                .to_string();
            if expr.is_empty() {
                return Err("an allow with an empty condition".to_string());
            }
            (m, Some(expr))
        }
        None => (rest, None),
    };
    let mut methods = BTreeSet::new();
    for word in methods_text.split(',') {
        match Method::parse(word.trim()) {
            Some(m) => {
                methods.insert(m);
            }
            None => return Err("an allow with a method this reader does not know".to_string()),
        }
    }
    if methods.is_empty() {
        return Err("an allow that names no method".to_string());
    }
    Ok(Allow {
        methods,
        condition,
        line,
    })
}

/// Split a match path on `/` outside `{}` and `$()`.
fn parse_path(path: &str, line: usize, errors: &mut Vec<ParseError>) -> Vec<Segment> {
    let mut raw: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut brace, mut paren) = (0usize, 0usize);
    for c in path.chars() {
        match c {
            '{' => brace += 1,
            '}' => brace = brace.saturating_sub(1),
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            _ => {}
        }
        if c == '/' && brace == 0 && paren == 0 {
            raw.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    raw.push(cur);
    let mut out = Vec::new();
    for (index, seg) in raw.into_iter().enumerate() {
        let seg = seg.trim().to_string();
        if seg.is_empty() {
            // The leading '/' of an absolute path yields one empty first segment.
            if index != 0 {
                errors.push(ParseError {
                    line,
                    what: "an empty path segment".to_string(),
                });
            }
            continue;
        }
        if seg.contains("$(") {
            out.push(Segment::Interpolated(seg));
        } else if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let (name, recursive) = match inner.split_once('=') {
                Some((n, "**")) => (n.trim(), true),
                Some(_) => {
                    errors.push(ParseError {
                        line,
                        what: "a path wildcard this reader does not understand".to_string(),
                    });
                    continue;
                }
                None => (inner.trim(), false),
            };
            if is_identifier(name) {
                out.push(Segment::Wildcard {
                    name: name.to_string(),
                    recursive,
                });
            } else {
                errors.push(ParseError {
                    line,
                    what: "a path wildcard this reader does not understand".to_string(),
                });
            }
        } else if seg.contains('{') || seg.contains('}') {
            errors.push(ParseError {
                line,
                what: "a path wildcard this reader does not understand".to_string(),
            });
        } else {
            out.push(Segment::Literal(seg));
        }
    }
    out
}
