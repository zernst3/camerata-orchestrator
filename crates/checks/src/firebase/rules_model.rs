//! Conditions: normalisation, helper-function inlining and the exact-shape recognisers.
//!
//! The governing law is the resolver's: false-negative, never false-positive. A condition is
//! "open", "auth only" or "test mode" ONLY when its normalised, inlined text matches a listed
//! form exactly. Anything else (an extra conjunct, a call this module does not inline, an
//! operator it does not know) is "not recognised", and every rule treats "not recognised" as the
//! safe direction: no finding.

use std::collections::HashMap;

use chrono::NaiveDate;

use super::rules_parse::{FnDef, Scope};

/// How many times helper calls are expanded. The language forbids recursion and caps call depth
/// at 20; 5 covers every real helper chain and bounds a hostile file.
const MAX_INLINE_PASSES: usize = 5;

/// A condition after comments are gone, whitespace is dropped, RTDB `===`/`!==` are mapped, and
/// helper functions are inlined. `None` condition (no `if`) is the text `true`: Firebase says a
/// missing condition "means the request is always allowed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cond(String);

/// A comparison target of `request.auth.uid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A wildcard of the enclosing path (`userId`).
    Wildcard(String),
    /// A field of the INCOMING document (`request.resource.data.<f>`).
    Incoming(String),
    /// A field of the STORED document (`resource.data.<f>`).
    Existing(String),
    Other,
}

/// A role read from a document keyed on the caller's own uid:
/// `get(/databases/$(database)/documents/<collection>/$(request.auth.uid)).data.<field>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleLookup {
    pub collection: String,
    pub field: String,
}

impl Cond {
    /// Build a Firestore or Storage condition, inlining the helpers in `scope`.
    pub fn new(raw: Option<&str>, scope: &Scope<'_>) -> Cond {
        let Some(raw) = raw else {
            return Cond("true".to_string());
        };
        let normal = normalise(raw);
        let inlined = inline_calls(&normal, &scope.functions);
        Cond(normalise(&inlined))
    }

    /// Build a Realtime Database condition (`auth` in place of `request.auth`; no functions).
    pub fn rtdb(raw: &str) -> Cond {
        Cond(normalise(raw))
    }

    /// The normalised, inlined text.
    pub fn text(&self) -> &str {
        &self.0
    }

    /// Unconditional: `true`, or a disjunction with a `true` member (`x || true`).
    pub fn is_true(&self) -> bool {
        truthy(&self.0)
    }

    /// Never grants: `false`, or a conjunction with a `false` member.
    pub fn is_false(&self) -> bool {
        falsy(&self.0)
    }

    /// Exactly "the caller is signed in", and nothing narrower. Any extra conjunct makes it not
    /// auth-only (a rule that also checks `email_verified` is narrower than the docs'
    /// "any authenticated user" shape).
    pub fn is_auth_only(&self) -> bool {
        auth_only(&self.0)
    }

    /// The `request.time < timestamp.date(Y, M, D)` scaffold, alone or as a disjunct. Returns the
    /// expiry date.
    pub fn test_mode_expiry(&self) -> Option<NaiveDate> {
        split_top(strip_parens(&self.0), "||")
            .iter()
            .find_map(|member| time_limit(strip_parens(member)))
    }

    /// Whether `token` occurs as a whole token (`request.resource.data`, a wildcard name).
    pub fn references(&self, token: &str) -> bool {
        contains_token(&self.0, token)
    }

    /// Whether any `||` appears outside a string. An ownership comparison that is one disjunct of
    /// several is not recognised.
    pub fn has_disjunction(&self) -> bool {
        !outside_strings_find(&self.0, "||").is_empty()
    }

    /// Every comparison of `request.auth.uid` and what it is compared to.
    pub fn uid_targets(&self, wildcards: &[String]) -> Vec<Target> {
        let mut atoms = Vec::new();
        collect_atoms(&self.0, &mut atoms);
        atoms
            .iter()
            .filter_map(|a| uid_target(a, wildcards))
            .collect()
    }

    /// The condition constrains the shape of the incoming document: `keys().hasOnly(`, `.hasAll(`,
    /// `.diff(resource.data).affectedKeys()` or a `request.resource.data.<f> is <type>` check.
    pub fn validates_fields(&self) -> bool {
        let t = &self.0;
        t.contains("request.resource.data.keys().hasOnly(")
            || t.contains("request.resource.data.keys().hasAll(")
            || t.contains(".diff(resource.data).affectedKeys()")
            || t.contains("request.resource.data.keys().hasAny(")
            || incoming_type_check(t)
    }

    /// The condition keeps field `f` from being changed by the caller.
    pub fn pins_field(&self, f: &str) -> bool {
        let t = &self.0;
        if t.contains(&format!("request.resource.data.{f}==resource.data.{f}"))
            || t.contains(&format!("resource.data.{f}==request.resource.data.{f}"))
        {
            return true;
        }
        // `!('f' in request.resource.data)` and the `.keys()` spelling.
        for q in ['\'', '"'] {
            if t.contains(&format!("!({q}{f}{q} in request.resource.data"))
                || t.contains(&format!("!({q}{f}{q}in request.resource.data"))
            {
                return true;
            }
        }
        // `affectedKeys().hasOnly([...])` or `keys().hasOnly([...])` with a list that excludes f.
        for marker in ["affectedKeys().hasOnly([", "keys().hasOnly(["] {
            let mut from = 0;
            while let Some(rel) = t[from..].find(marker) {
                let start = from + rel + marker.len();
                if let Some(end) = t[start..].find(']') {
                    let items = list_items(&t[start..start + end]);
                    if !items.iter().any(|i| i == f) {
                        return true;
                    }
                }
                from = start;
            }
        }
        false
    }

    /// Roles read from documents keyed on the caller's own uid.
    pub fn role_lookups(&self) -> Vec<RoleLookup> {
        role_lookups(&self.0)
    }
}

// ── normalisation ───────────────────────────────────────────────────────────────────────────

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// A character after which a space between two operands must be kept (`'admin' in x`, `a is b`).
fn is_wordish(c: char) -> bool {
    is_ident_char(c) || c == '\'' || c == '"'
}

/// Drop whitespace outside strings (keeping one space between two word-ish neighbours), map the
/// RTDB `===`/`!==` to `==`/`!=`, and strip redundant outer parentheses.
pub fn normalise(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    let mut pending_space = false;
    while i < n {
        let c = chars[i];
        if c == '\'' || c == '"' {
            if pending_space && out.chars().last().is_some_and(is_wordish) && is_wordish(c) {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
            i += 1;
            while i < n {
                let d = chars[i];
                out.push(d);
                i += 1;
                if d == '\\' && i < n {
                    out.push(chars[i]);
                    i += 1;
                } else if d == c {
                    break;
                }
            }
            continue;
        }
        if c.is_whitespace() {
            pending_space = true;
            i += 1;
            continue;
        }
        if pending_space && out.chars().last().is_some_and(is_wordish) && is_wordish(c) {
            out.push(' ');
        }
        pending_space = false;
        if c == '=' && chars.get(i + 1) == Some(&'=') && chars.get(i + 2) == Some(&'=') {
            out.push_str("==");
            i += 3;
        } else if c == '!' && chars.get(i + 1) == Some(&'=') && chars.get(i + 2) == Some(&'=') {
            out.push_str("!=");
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    strip_parens(&out).to_string()
}

/// `s` with redundant outer parentheses removed, repeatedly.
fn strip_parens(s: &str) -> &str {
    let mut s = s;
    while s.starts_with('(') && s.ends_with(')') && matching_close(s, 0) == Some(s.len() - 1) {
        s = &s[1..s.len() - 1];
    }
    s
}

/// Byte index of the bracket closing the one opened at byte `open`, outside strings.
fn matching_close(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s[open..].char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Byte offsets of `needle` outside strings and outside any bracket.
fn top_level_find(s: &str, needle: &str) -> Vec<usize> {
    scan_find(s, needle, true)
}

/// Byte offsets of `needle` outside strings (at any bracket depth).
fn outside_strings_find(s: &str, needle: &str) -> Vec<usize> {
    scan_find(s, needle, false)
}

fn scan_find(s: &str, needle: &str, top_only: bool) -> Vec<usize> {
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if (!top_only || depth == 0) && s[i..].starts_with(needle) {
            found.push(i);
        }
    }
    found
}

/// Split on a top-level two-character operator (`||`, `&&`).
fn split_top(s: &str, op: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut last = 0;
    let mut skip_until = 0;
    for at in top_level_find(s, op) {
        if at < skip_until {
            continue;
        }
        parts.push(s[last..at].to_string());
        last = at + op.len();
        skip_until = last;
    }
    parts.push(s[last..].to_string());
    parts
}

/// Split on top-level commas.
fn split_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut last = 0;
    for at in top_level_find(s, ",") {
        parts.push(s[last..at].to_string());
        last = at + 1;
    }
    parts.push(s[last..].to_string());
    parts
}

/// Whether `token` occurs in `s` as a whole token: not glued to a preceding identifier character
/// or `.`, and not followed by an identifier character.
fn contains_token(s: &str, token: &str) -> bool {
    s.match_indices(token).any(|(at, _)| {
        let before_ok = s[..at]
            .chars()
            .last()
            .is_none_or(|c| !is_ident_char(c) && c != '.');
        let after_ok = s[at + token.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        before_ok && after_ok
    })
}

// ── inlining ────────────────────────────────────────────────────────────────────────────────

fn inline_calls(text: &str, functions: &[&FnDef]) -> String {
    let mut current = text.to_string();
    for _ in 0..MAX_INLINE_PASSES {
        let next = inline_once(&current, functions);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

fn inline_once(text: &str, functions: &[&FnDef]) -> String {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut idx = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    while idx < chars.len() {
        let (at, c) = chars[idx];
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            out.push(c);
            idx += 1;
            continue;
        }
        let starts_ident = (c.is_ascii_alphabetic() || c == '_')
            && out
                .chars()
                .last()
                .is_none_or(|p| !is_ident_char(p) && p != '.');
        if !starts_ident {
            out.push(c);
            idx += 1;
            continue;
        }
        let mut end = idx;
        while end < chars.len() && is_ident_char(chars[end].1) {
            end += 1;
        }
        let name_end = chars.get(end).map_or(text.len(), |(b, _)| *b);
        let name = &text[at..name_end];
        let def = functions.iter().find(|f| f.name == name);
        if let (Some(def), Some((open_at, '('))) = (def, chars.get(end).copied()) {
            if let Some(close_at) = matching_close(text, open_at) {
                let inner = &text[open_at + 1..close_at];
                let args: Vec<String> = if inner.trim().is_empty() {
                    Vec::new()
                } else {
                    split_commas(inner)
                };
                if args.len() == def.params.len() {
                    let body = normalise(&expand_body(def, &args));
                    let preceded_by_operator =
                        out.chars().last().is_some_and(|p| "!=<>+-*/%".contains(p));
                    let compound = !top_level_find(&body, "||").is_empty()
                        || !top_level_find(&body, "?").is_empty();
                    if preceded_by_operator || compound {
                        out.push('(');
                        out.push_str(&body);
                        out.push(')');
                    } else {
                        out.push_str(&body);
                    }
                    // Resume after the closing parenthesis.
                    idx = chars
                        .iter()
                        .position(|(b, _)| *b > close_at)
                        .unwrap_or(chars.len());
                    continue;
                }
            }
        }
        out.push_str(name);
        idx = end;
    }
    out
}

/// The function's return expression with its `let`s and parameters substituted.
fn expand_body(def: &FnDef, args: &[String]) -> String {
    let mut env: HashMap<String, String> = HashMap::new();
    for (param, arg) in def.params.iter().zip(args) {
        env.insert(param.clone(), wrap_if_compound(&normalise(arg)));
    }
    for (name, expr) in &def.lets {
        let value = wrap_if_compound(&normalise(&substitute(expr, &env)));
        env.insert(name.clone(), value);
    }
    substitute(def.ret.as_deref().unwrap_or("false"), &env)
}

fn wrap_if_compound(s: &str) -> String {
    let compound = s.chars().any(|c| {
        matches!(
            c,
            '&' | '|' | '=' | '<' | '>' | '!' | '+' | '-' | '*' | '/' | '%' | '?'
        )
    });
    // A bare path such as `request.auth.uid` or a quoted string is not compound.
    if compound && !(s.starts_with('\'') || s.starts_with('"')) && !s.starts_with('(') {
        format!("({s})")
    } else {
        s.to_string()
    }
}

/// Simultaneous textual substitution of identifiers (not after a `.`, not inside strings).
fn substitute(text: &str, env: &HashMap<String, String>) -> String {
    if env.is_empty() {
        return text.to_string();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut idx = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    while idx < chars.len() {
        let (at, c) = chars[idx];
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            out.push(c);
            idx += 1;
            continue;
        }
        let starts_ident = (c.is_ascii_alphabetic() || c == '_')
            && out
                .chars()
                .last()
                .is_none_or(|p| !is_ident_char(p) && p != '.');
        if !starts_ident {
            out.push(c);
            idx += 1;
            continue;
        }
        let mut end = idx;
        while end < chars.len() && is_ident_char(chars[end].1) {
            end += 1;
        }
        let name_end = chars.get(end).map_or(text.len(), |(b, _)| *b);
        let name = &text[at..name_end];
        match env.get(name) {
            Some(value) => out.push_str(value),
            None => out.push_str(name),
        }
        idx = end;
    }
    out
}

// ── recognisers ─────────────────────────────────────────────────────────────────────────────

fn truthy(t: &str) -> bool {
    let t = strip_parens(t);
    if t == "true" {
        return true;
    }
    let ors = split_top(t, "||");
    if ors.len() > 1 {
        return ors.iter().any(|m| truthy(m));
    }
    let ands = split_top(t, "&&");
    if ands.len() > 1 {
        return ands.iter().all(|m| truthy(m));
    }
    false
}

fn falsy(t: &str) -> bool {
    let t = strip_parens(t);
    if t == "false" {
        return true;
    }
    let ors = split_top(t, "||");
    if ors.len() > 1 {
        return ors.iter().all(|m| falsy(m));
    }
    let ands = split_top(t, "&&");
    if ands.len() > 1 {
        return ands.iter().any(|m| falsy(m));
    }
    false
}

/// The exact "caller is signed in" atoms, Firestore/Storage and Realtime Database spellings.
const AUTH_ATOMS: &[&str] = &[
    "request.auth!=null",
    "null!=request.auth",
    "request.auth.uid!=null",
    "request.auth.uid!=\"\"",
    "request.auth.uid!=''",
    "request.auth.token!=null",
    "auth!=null",
    "null!=auth",
    "auth.uid!=null",
    "auth.uid!=\"\"",
    "auth.uid!=''",
];

fn auth_only(t: &str) -> bool {
    let t = strip_parens(t);
    let ors = split_top(t, "||");
    if ors.len() > 1 {
        return ors.iter().all(|m| auth_only(m));
    }
    let ands = split_top(t, "&&");
    if ands.len() > 1 {
        return ands.iter().all(|m| AUTH_ATOMS.contains(&strip_parens(m)));
    }
    AUTH_ATOMS.contains(&t)
}

/// `request.time<timestamp.date(Y,M,D)` (or `<=`, or `timestamp.value(ms)`), and the mirrored
/// `X>request.time`. Arguments must be integer literals.
fn time_limit(t: &str) -> Option<NaiveDate> {
    let rhs = t
        .strip_prefix("request.time<=")
        .or_else(|| t.strip_prefix("request.time<"));
    if let Some(rhs) = rhs {
        return timestamp_literal(rhs);
    }
    let lhs = t
        .strip_suffix(">=request.time")
        .or_else(|| t.strip_suffix(">request.time"));
    lhs.and_then(timestamp_literal)
}

fn timestamp_literal(s: &str) -> Option<NaiveDate> {
    let s = strip_parens(s);
    if let Some(args) = s
        .strip_prefix("timestamp.date(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let nums: Vec<u32> = args
            .split(',')
            .map(|a| a.parse::<u32>().ok())
            .collect::<Option<Vec<u32>>>()?;
        let [y, m, d] = nums[..] else {
            return None;
        };
        return NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m, d);
    }
    if let Some(ms) = s
        .strip_prefix("timestamp.value(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let ms: i64 = ms.parse().ok()?;
        return chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.date_naive());
    }
    None
}

fn collect_atoms(t: &str, out: &mut Vec<String>) {
    let t = strip_parens(t);
    for op in ["||", "&&"] {
        let parts = split_top(t, op);
        if parts.len() > 1 {
            for p in parts {
                collect_atoms(&p, out);
            }
            return;
        }
    }
    out.push(t.to_string());
}

fn uid_target(atom: &str, wildcards: &[String]) -> Option<Target> {
    let rhs = atom
        .strip_prefix("request.auth.uid==")
        .or_else(|| atom.strip_suffix("==request.auth.uid"))
        .or_else(|| atom.strip_prefix("request.auth.uid in "))?;
    Some(classify_target(rhs, wildcards))
}

fn classify_target(rhs: &str, wildcards: &[String]) -> Target {
    let rhs = strip_parens(rhs);
    if let Some(rest) = rhs.strip_prefix("request.resource.data.") {
        return Target::Incoming(leading_ident(rest));
    }
    if let Some(rest) = rhs
        .strip_prefix("resource.data.")
        .or_else(|| rhs.strip_prefix("resource.data["))
    {
        return Target::Existing(leading_ident(rest));
    }
    let bare = rhs
        .strip_prefix("string(")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or(rhs);
    if wildcards.iter().any(|w| w == bare) {
        return Target::Wildcard(bare.to_string());
    }
    Target::Other
}

fn leading_ident(s: &str) -> String {
    s.trim_start_matches(['\'', '"'])
        .chars()
        .take_while(|c| is_ident_char(*c))
        .collect()
}

/// `request.resource.data.<f> is <type>`.
fn incoming_type_check(t: &str) -> bool {
    let marker = "request.resource.data.";
    t.match_indices(marker).any(|(at, _)| {
        let rest = &t[at + marker.len()..];
        let after_field = rest.trim_start_matches(|c: char| is_ident_char(c));
        after_field.starts_with(" is ")
    })
}

/// The quoted items of a list body (`'a','b'`).
fn list_items(body: &str) -> Vec<String> {
    split_commas(body)
        .iter()
        .map(|i| i.trim().trim_matches(['\'', '"']).to_string())
        .collect()
}

fn role_lookups(t: &str) -> Vec<RoleLookup> {
    let mut found = Vec::new();
    for (at, _) in t.match_indices("get(/databases/") {
        let open = at + 3;
        let Some(close) = matching_close(t, open) else {
            continue;
        };
        let path = &t[open + 1..close];
        let Some((_, after_documents)) = path.split_once("/documents/") else {
            continue;
        };
        let segments: Vec<&str> = after_documents.split('/').collect();
        let [collection, key] = segments[..] else {
            continue;
        };
        if key != "$(request.auth.uid)" || collection.contains('$') {
            continue;
        }
        let tail = &t[close + 1..];
        let field = tail
            .strip_prefix(".data.")
            .map(leading_ident)
            .or_else(|| {
                tail.strip_prefix(".data['")
                    .or_else(|| tail.strip_prefix(".data[\""))
                    .map(leading_ident)
            })
            .filter(|f| !f.is_empty());
        if let Some(field) = field {
            found.push(RoleLookup {
                collection: collection.to_string(),
                field,
            });
        }
    }
    found
}
