//! `CrossTenantDefinerReadChecker`: detects `SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1` — a
//! `SECURITY DEFINER` SQL/plpgsql function that RETURNS rows filtered ONLY by a value supplied
//! in its OWN parameter list (a parent/tenant/owner id the CALLER provides), is `GRANT
//! EXECUTE`d to a broad, anonymous-or-any-authenticated-caller role (`anon`/`public`/
//! `authenticated`), and whose body contains NO server-side predicate tying those rows back to
//! the CALLER's own identity.
//!
//! # Why this is a distinct gap from `PrivilegedFunctionNoAuthzChecker`
//!
//! `privileged_no_authz_checker` (`SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1`) answers the WRITE
//! shape of missing authorization on a `SECURITY DEFINER` function: a mutating body
//! (INSERT/UPDATE/DELETE/TRUNCATE/MERGE), broadly granted, with no identity check. It
//! deliberately treats a READ-only body as out of scope (see its own
//! `security_definer_read_only_body_is_never_flagged` test) — a `SECURITY DEFINER` function
//! that only reads is not a privilege-escalation WRITE primitive. But a read-only `SECURITY
//! DEFINER` function can still be the SAME class of defect in a different shape: if it accepts
//! a tenant/parent/owner id as a plain parameter and returns every row matching that id with no
//! check that the id actually belongs to the CALLER, any caller matching the broad grant can
//! substitute ANY id and read another tenant's rows — a `SECURITY DEFINER` function bypasses
//! Row Level Security exactly as it does for a write, so the RLS policies that would otherwise
//! block this read never run. This is CWE-639 (Authorization Bypass Through User-Controlled
//! Key) / IDOR: the "key" (the tenant/owner id) is the caller's own input, not derived from
//! their authenticated identity.
//!
//! # The mechanism, precisely
//!
//! A finding requires ALL FOUR of:
//! 1. **Privileged**: the function is declared `SECURITY DEFINER` (anywhere in its signature —
//!    before OR after the dollar-quoted body, mirroring `privileged_no_authz_checker`'s own
//!    two-sided scan), AND
//! 2. **Read-only**: the body contains NO standalone `INSERT`/`UPDATE`/`DELETE`/`TRUNCATE`/
//!    `MERGE` keyword (the sibling WRITE rule already owns that shape; this rule stays
//!    orthogonal to it rather than double-flagging the same body under two rule ids), AND
//! 3. **Filtered by the caller's own parameter**: the body contains an equality comparison (`=`)
//!    where one operand is a bare identifier EXACTLY matching one of the function's own
//!    declared parameter names — e.g. `where tenant_id = p_tenant_id`. This is the "cross-
//!    tenant" shape: the function hands back rows selected by whatever id the CALLER passes in,
//!    AND
//! 4. **Broadly reachable, with no caller-identity predicate**: a `GRANT EXECUTE ON FUNCTION
//!    <this function> TO anon|public|authenticated` exists anywhere in the same scanned SQL
//!    corpus (the exact cross-file grant-correlation mechanism `dynamic_sql_exec_checker`
//!    introduced and `privileged_no_authz_checker` reuses), and the body contains NONE of the
//!    recognized authorization-predicate idioms (the SAME three signals
//!    `privileged_no_authz_checker::has_authorization_predicate` answers, reproduced here with
//!    its item-2 CALL-OR-COMPARISON discipline for the secret/token/signature signal from the
//!    start — see that module's own doc comment for the full three-signal breakdown).
//!
//! Severity is always HIGH — reachability (condition 4's grant half) is baked into the trigger
//! condition itself here, mirroring `privileged_no_authz_checker`'s identical severity design
//! (a privileged read-only function with no broad grant is simply out of this rule's scope, not
//! a lower-severity finding).
//!
//! # Safe twins — the class boundary
//!
//! - An INVOKER-rights sibling with the identical body shape: Postgres's own Row Level Security
//!   runs normally for an invoker-rights function, so filtering by a caller-supplied id is not
//!   a bypass — the caller's own RLS policies (if any) still apply to every row the query
//!   touches.
//! - A `SECURITY DEFINER` function whose body DOES include a caller-identity predicate anywhere
//!   (`auth.uid()`, a role-check call, a genuine secret/signature comparison) — the exact same
//!   three signals that exempt the sibling WRITE rule.
//! - A `SECURITY DEFINER` function granted EXECUTE only to a non-broad, internal/service role —
//!   never reachable by an anonymous or ordinary authenticated caller in the first place.
//! - A `SECURITY DEFINER` function whose body never equates a column against one of its OWN
//!   parameters at all (e.g. a fixed aggregate read, or a read filtered only by a hardcoded
//!   condition) — nothing in the query is caller-controlled, so there is no cross-tenant key to
//!   substitute.
//!
//! # What this deliberately does NOT do
//!
//! No attempt to verify the matched parameter is SPECIFICALLY a tenant/parent/owner id as
//! opposed to some other caller-supplied filter (e.g. a `status` value) — like every other
//! floor-detector in this corpus, ANY equality between a column and a bare function parameter,
//! with zero authorization predicate anywhere in the body, is flagged; narrowing further would
//! require inferring column semantics this corpus's lexical scanners don't model. No positional
//! mapping between the parameter and which specific result column it filters, and no attempt to
//! confirm the filter is the query's ONLY condition (a predicate anywhere in the body — not
//! necessarily joined to the same WHERE clause — is accepted as exempting evidence, mirroring
//! the sibling WRITE rule's identical "presence, not proof-of-gating" floor-tier scope line). No
//! cross-function analysis: the function body is the unit of analysis, exactly as in every
//! other checker in this corpus.

use super::splitter::{parse_dollar_tag, split_statements, SqlStatement};
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};

pub const RULE_DEFINER_CROSS_TENANT_READ: &str = "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1";

const RULE_IDS: &[&str] = &[RULE_DEFINER_CROSS_TENANT_READ];

/// `.sql` anywhere in the repo — see `dynamic_sql_exec_checker`'s identical choice: the
/// vulnerability is general Postgres/PostgREST, not tied to Supabase's folder convention.
const INTEREST_GLOBS: &[&str] = &["**/*.sql"];

const DEFAULT_SCHEMA: &str = "public";

/// Roles a `GRANT EXECUTE` to which makes a function reachable by an anonymous or
/// unauthenticated-by-default API caller. Identical list to `dynamic_sql_exec_checker`'s own
/// `SENSITIVE_GRANT_ROLES`.
const SENSITIVE_GRANT_ROLES: &[&str] = &["anon", "public", "authenticated"];

/// Write-statement keywords that make a function body "mutating" — identical to
/// `privileged_no_authz_checker::MUTATING_KEYWORDS`. A mutating body is the sibling WRITE
/// rule's shape, not this one's.
const MUTATING_KEYWORDS: &[&str] = &["insert", "update", "delete", "truncate", "merge"];

/// Function-CALL name substrings that count as a role/ownership/permission check — ONLY when
/// the identifier is directly followed by `(`. Identical to
/// `privileged_no_authz_checker::ROLE_CHECK_CALL_SUBSTRINGS`.
const ROLE_CHECK_CALL_SUBSTRINGS: &[&str] =
    &["role", "owner", "permission", "authoriz", "admin", "verify"];

/// Identifier substrings that count as a shared-secret/signature compensating control — in
/// CALL form or as an operand of a genuine comparison guard. Identical to
/// `privileged_no_authz_checker::SECRET_CHECK_SUBSTRINGS`.
const SECRET_CHECK_SUBSTRINGS: &[&str] = &["secret", "token", "signature"];

pub struct CrossTenantDefinerReadChecker;

impl ArchChecker for CrossTenantDefinerReadChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let mut sql_files: Vec<&(String, String)> = repo
            .files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .collect();
        // Deterministic iteration order across runs — see `dynamic_sql_exec_checker`'s
        // identical discipline; doesn't affect WHICH functions/grants are found, only order.
        sql_files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut functions: std::collections::BTreeMap<(String, String), CandidateFunction> =
            std::collections::BTreeMap::new();
        let mut granted: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();

        for (path, content) in &sql_files {
            for stmt in split_statements(content) {
                if let Some(f) = parse_create_function_candidate(&stmt, path) {
                    functions.insert((f.schema.clone(), f.name.clone()), f);
                } else {
                    for target in parse_grant_execute_sensitive(&stmt.text) {
                        granted.insert(target);
                    }
                }
            }
        }

        functions
            .values()
            .filter_map(|f| {
                if !f.security_definer {
                    return None;
                }
                if f.params.is_empty() {
                    return None;
                }
                let stripped = strip_sql_comments(&f.body);
                if is_mutating(&stripped) {
                    return None;
                }
                let key = (f.schema.clone(), f.name.clone());
                if !granted.contains(&key) {
                    return None;
                }
                let lower: Vec<char> = stripped.to_ascii_lowercase().chars().collect();
                if !has_param_filter(&lower, &f.params) {
                    return None;
                }
                if has_authorization_predicate(&stripped) {
                    return None;
                }
                Some(ArchViolation {
                    rule_id: RULE_DEFINER_CROSS_TENANT_READ.to_string(),
                    file: f.file.clone(),
                    line: f.line,
                    object: Some(format!("{}.{}", f.schema, f.name)),
                    severity: SEVERITY_HIGH,
                    message: message_for(f),
                })
            })
            .collect()
    }
}

/// One `CREATE [OR REPLACE] FUNCTION`/`PROCEDURE` this checker was able to parse — mirrors
/// `privileged_no_authz_checker::CandidateFunction`, plus the declared parameter NAMES this
/// rule additionally needs (to recognize "filtered by the caller's own parameter").
struct CandidateFunction {
    schema: String,
    name: String,
    security_definer: bool,
    /// Lowercased declared parameter names, in signature order (mode keywords `IN`/`OUT`/
    /// `INOUT`/`VARIADIC` stripped — see [`parse_param_names`]).
    params: Vec<String>,
    /// The dollar-quoted body's verbatim interior text (comments NOT yet stripped — see
    /// [`strip_sql_comments`], applied by the caller before classification).
    body: String,
    file: String,
    line: usize,
}

fn message_for(f: &CandidateFunction) -> String {
    let name = if f.schema == "public" {
        format!("`{}`", f.name)
    } else {
        format!("`{}.{}`", f.schema, f.name)
    };
    format!(
        "One of your database functions ({name}) is `SECURITY DEFINER` (bypasses Row Level \
         Security regardless of caller), returns rows filtered by an id supplied in its OWN \
         parameter list, and is GRANTed EXECUTE to an anonymous or any-authenticated-user role \
         (anon/public/authenticated) in this same SQL corpus — but its body never checks that \
         id against the caller's own identity. This is CWE-639 (Authorization Bypass Through \
         User-Controlled Key): any caller matching the broad grant can substitute ANY id and \
         read another tenant's, owner's, or user's rows, exactly as an IDOR vulnerability does \
         in application code — except here the bypass happens INSIDE the database, where the \
         RLS policies that would otherwise block it never run because `SECURITY DEFINER` \
         executes with the function owner's privileges. Defined at {}:{}. Fix: add a \
         server-side identity check inside the function body BEFORE returning rows — e.g. \
         `if auth.uid() <> (select owner_id from <table> where id = p_id) then raise exception \
         ...;` — or, if no caller should pass an arbitrary id directly, derive the id from \
         `auth.uid()` instead of accepting it as a parameter at all. A client-side/UI-only \
         check is never a substitute: it constrains nothing once the caller can reach the \
         function directly (e.g. via the PostgREST RPC endpoint), so it provides no actual \
         authorization. This reflects the migration history in this repository only — confirm \
         the deployed function definition matches before treating this as settled.",
        f.file, f.line,
    )
}

// ── CREATE FUNCTION / PROCEDURE candidate parsing ───────────────────────────────────────
//
// Deliberately duplicated (not shared via `pub(crate)`) from `privileged_no_authz_checker.rs`
// and `dynamic_sql_exec_checker.rs` — mirrors those modules' own documented convention of each
// lexical checker owning its small parsing primitives rather than threading a shared dependency
// through the tree. The only addition versus `privileged_no_authz_checker`'s own candidate
// parser is also capturing the declared parameter NAMES (see [`parse_param_names`]).

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

fn match_kw_seq(chars: &[char], mut i: usize, words: &[&str]) -> Option<usize> {
    for (wi, w) in words.iter().enumerate() {
        if wi > 0 {
            let before = i;
            i = skip_ws(chars, i);
            if i == before {
                return None;
            }
        }
        let word_start = i;
        let wchars: Vec<char> = w.chars().collect();
        if word_start + wchars.len() > chars.len() {
            return None;
        }
        if chars[word_start..word_start + wchars.len()] != wchars[..] {
            return None;
        }
        if word_start > 0 {
            let prev = chars[word_start - 1];
            if prev.is_alphanumeric() || prev == '_' {
                return None;
            }
        }
        let after = word_start + wchars.len();
        if after < chars.len() {
            let c = chars[after];
            if c.is_alphanumeric() || c == '_' {
                return None;
            }
        }
        i = after;
    }
    Some(i)
}

/// Whether `words` occurs anywhere in `chars` as a standalone keyword sequence — used to scan
/// the signature's header/trailer for `SECURITY DEFINER`.
fn contains_kw_seq(chars: &[char], words: &[&str]) -> bool {
    (0..chars.len()).any(|i| match_kw_seq(chars, i, words).is_some())
}

fn parse_ident(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut i = skip_ws(chars, i);
    let c = *chars.get(i)?;
    if c == '"' {
        let mut out = String::new();
        i += 1;
        loop {
            match chars.get(i) {
                None => break,
                Some('"') => {
                    if chars.get(i + 1) == Some(&'"') {
                        out.push('"');
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                }
                Some(&ch) => {
                    out.push(ch);
                    i += 1;
                }
            }
        }
        return Some((out, i));
    }
    if c.is_alphabetic() || c == '_' {
        let start = i;
        while let Some(&ch) = chars.get(i) {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                i += 1;
            } else {
                break;
            }
        }
        let raw_ident: String = chars[start..i].iter().collect();
        return Some((raw_ident.to_lowercase(), i));
    }
    None
}

fn parse_qualified_name(chars: &[char], i: usize) -> Option<((String, String), usize)> {
    let (first, j) = parse_ident(chars, i)?;
    let k = skip_ws(chars, j);
    if chars.get(k) == Some(&'.') {
        if let Some((second, j2)) = parse_ident(chars, k + 1) {
            return Some(((first, second), j2));
        }
    }
    Some(((DEFAULT_SCHEMA.to_string(), first), j))
}

fn extract_paren_inner(chars: &[char], open_idx: usize) -> Option<(Vec<char>, usize)> {
    if chars.get(open_idx) != Some(&'(') {
        return None;
    }
    let mut depth: i32 = 1;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let start = open_idx + 1;
    let mut i = start;
    while i < chars.len() {
        let ch = chars[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match quote {
            Some(q) => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((chars[start..i].to_vec(), i));
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn find_dollar_quoted_body(chars: &[char], from: usize) -> Option<(usize, usize)> {
    let open_idx =
        (from..chars.len()).find(|&i| chars[i] == '$' && parse_dollar_tag(chars, i).is_some())?;
    let (tag, after_open) = parse_dollar_tag(chars, open_idx)?;
    let mut i = after_open;
    while i < chars.len() {
        if chars[i] == '$' {
            if let Some((tag2, after2)) = parse_dollar_tag(chars, i) {
                if tag2 == tag {
                    let _ = after2;
                    return Some((after_open, i));
                }
            }
        }
        i += 1;
    }
    None
}

/// Split `chars` into top-level comma-separated segments — quote-aware and depth-aware.
/// Identical mechanism to `dynamic_sql_exec_checker::split_top_level_commas`.
fn split_top_level_commas(chars: &[char]) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match quote {
            Some(q) => {
                if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    args.push(
                        chars[start..i]
                            .iter()
                            .collect::<String>()
                            .trim()
                            .to_string(),
                    );
                    start = i + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    let tail: String = chars[start..].iter().collect::<String>().trim().to_string();
    if !tail.is_empty() || !args.is_empty() {
        args.push(tail);
    }
    args
}

/// Parse a function's parameter-list text into its declared parameter NAMES, lowercased, in
/// signature order. Each top-level comma-separated segment is `[IN|OUT|INOUT|VARIADIC] name
/// type [DEFAULT expr]` — the leading mode keyword(s) are skipped and the first remaining
/// bareword token is taken as the name. A segment this shallow heuristic can't find a name in
/// (e.g. an unnamed positional parameter, just a type) is silently skipped — this rule only
/// needs to recognize a NAMED parameter being used as a filter value, so an unnamed one simply
/// never matches [`has_param_filter`], never a panic or a wrong attribution.
fn parse_param_names(inner: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    for seg in split_top_level_commas(inner) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = seg.split_whitespace().collect();
        let mut idx = 0usize;
        while idx < tokens.len()
            && matches!(
                tokens[idx].to_ascii_lowercase().as_str(),
                "in" | "out" | "inout" | "variadic"
            )
        {
            idx += 1;
        }
        let Some(name_tok) = tokens.get(idx) else {
            continue;
        };
        let name: String = name_tok
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name.to_lowercase());
        }
    }
    out
}

/// Parse one `CREATE [OR REPLACE] FUNCTION|PROCEDURE schema.name(...) ... AS $tag$ body $tag$
/// ...` statement into a [`CandidateFunction`], including whether `SECURITY DEFINER` appears
/// anywhere in the signature and the declared parameter names. Returns `None` for anything else
/// (including a function whose body isn't dollar-quoted), never panics.
fn parse_create_function_candidate(stmt: &SqlStatement, file: &str) -> Option<CandidateFunction> {
    let raw: Vec<char> = stmt.text.chars().collect();
    let lower: Vec<char> = raw.iter().map(|c| c.to_ascii_lowercase()).collect();

    let i = skip_ws(&lower, 0);
    let i = match_kw_seq(&lower, i, &["create"])?;
    let mut i = skip_ws(&lower, i);
    if let Some(i2) = match_kw_seq(&lower, i, &["or", "replace"]) {
        i = skip_ws(&lower, i2);
    }
    let i = match_kw_seq(&lower, i, &["function"])
        .or_else(|| match_kw_seq(&lower, i, &["procedure"]))?;
    let i = skip_ws(&lower, i);
    let ((schema, name), after_name) = parse_qualified_name(&raw, i)?;

    let mut after_params = skip_ws(&lower, after_name);
    let mut params: Vec<String> = Vec::new();
    if lower.get(after_params) == Some(&'(') {
        if let Some((inner, close_idx)) = extract_paren_inner(&raw, after_params) {
            params = parse_param_names(&inner);
            after_params = close_idx + 1;
        }
    }

    let (body_start, body_end) = find_dollar_quoted_body(&raw, after_params)?;
    let body: String = raw[body_start..body_end].iter().collect();

    let header = lower.get(after_params..body_start).unwrap_or(&[]);
    let trailer = lower.get(body_end..).unwrap_or(&[]);
    let security_definer = contains_kw_seq(header, &["security", "definer"])
        || contains_kw_seq(trailer, &["security", "definer"]);

    Some(CandidateFunction {
        schema,
        name,
        security_definer,
        params,
        body,
        file: file.to_string(),
        line: stmt.line,
    })
}

// ── GRANT EXECUTE ... TO <sensitive role> parsing ───────────────────────────────────────
//
// Byte-for-byte the same mechanism as `dynamic_sql_exec_checker::parse_grant_execute_sensitive`
// — duplicated per this module's stated convention.

fn parse_grant_execute_sensitive(text: &str) -> Vec<(String, String)> {
    let raw: Vec<char> = text.chars().collect();
    let lower: Vec<char> = raw.iter().map(|c| c.to_ascii_lowercase()).collect();

    let i = skip_ws(&lower, 0);
    let Some(i) = match_kw_seq(&lower, i, &["grant"]) else {
        return Vec::new();
    };
    let i = skip_ws(&lower, i);
    let Some(i) = match_kw_seq(&lower, i, &["execute"]) else {
        return Vec::new();
    };
    let i = skip_ws(&lower, i);
    let Some(i) = match_kw_seq(&lower, i, &["on"]) else {
        return Vec::new();
    };
    let i = skip_ws(&lower, i);
    let Some(mut pos) =
        match_kw_seq(&lower, i, &["function"]).or_else(|| match_kw_seq(&lower, i, &["procedure"]))
    else {
        return Vec::new();
    };
    pos = skip_ws(&lower, pos);

    let mut targets = Vec::new();
    while let Some(((schema, name), after_name)) = parse_qualified_name(&raw, pos) {
        let mut after = skip_ws(&lower, after_name);
        if lower.get(after) == Some(&'(') {
            if let Some((_, close_idx)) = extract_paren_inner(&raw, after) {
                after = close_idx + 1;
            }
        }
        targets.push((schema, name));
        let after_ws = skip_ws(&lower, after);
        if lower.get(after_ws) == Some(&',') {
            pos = skip_ws(&lower, after_ws + 1);
            continue;
        }
        pos = after_ws;
        break;
    }
    if targets.is_empty() {
        return Vec::new();
    }

    let Some(to_end) = match_kw_seq(&lower, pos, &["to"]) else {
        return Vec::new();
    };
    let roles_text: String = raw[to_end..].iter().collect();
    let roles_lower = roles_text.to_ascii_lowercase();
    let has_sensitive_role = SENSITIVE_GRANT_ROLES
        .iter()
        .any(|r| contains_word(&roles_lower, r));

    if has_sensitive_role {
        targets
    } else {
        Vec::new()
    }
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let chars: Vec<char> = haystack.chars().collect();
    let wchars: Vec<char> = word.chars().collect();
    if wchars.is_empty() || wchars.len() > chars.len() {
        return false;
    }
    for start in 0..=(chars.len() - wchars.len()) {
        if chars[start..start + wchars.len()] == wchars[..] {
            let before_ok =
                start == 0 || !(chars[start - 1].is_alphanumeric() || chars[start - 1] == '_');
            let end = start + wchars.len();
            let after_ok =
                end == chars.len() || !(chars[end].is_alphanumeric() || chars[end] == '_');
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

// ── function-body scan: read shape, param filter, authorization predicate ──────────────

/// Strip `--` line comments and `/* */` block comments (non-nesting) from a plpgsql body,
/// treating `'...'` and `"..."` as opaque. Byte-for-byte the same mechanism as
/// `privileged_no_authz_checker::strip_sql_comments` — duplicated per this module's convention.
fn strip_sql_comments(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            out.push(c);
            i += 1;
            continue;
        }
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Find the next occurrence of `word` (a standalone token) in `chars`, starting at `from`.
/// Byte-for-byte the same mechanism as `dynamic_sql_exec_checker::find_word_from`.
fn find_word_from(chars: &[char], from: usize, word: &str) -> Option<usize> {
    let w: Vec<char> = word.chars().collect();
    if w.is_empty() || from + w.len() > chars.len() {
        return None;
    }
    let mut i = from;
    while i + w.len() <= chars.len() {
        if chars[i..i + w.len()] == w[..] {
            let before_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            let end = i + w.len();
            let after_ok =
                end == chars.len() || !(chars[end].is_alphanumeric() || chars[end] == '_');
            if before_ok && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Whether a comment-stripped body contains a standalone write-statement keyword — identical
/// mechanism to `privileged_no_authz_checker::is_mutating`.
fn is_mutating(stripped_body: &str) -> bool {
    let chars: Vec<char> = stripped_body.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    MUTATING_KEYWORDS
        .iter()
        .any(|kw| find_word_from(&lower, 0, kw).is_some())
}

/// Whether `ident` (already lowercased) contains any of `substrings` (already lowercased).
fn ident_contains_any(ident: &str, substrings: &[&str]) -> bool {
    substrings.iter().any(|s| ident.contains(s))
}

/// Scan `lower` (a comment-stripped, already-lowercased body) for a bareword identifier
/// immediately followed by `(` whose name contains one of `call_substrings` — i.e. a FUNCTION
/// CALL, not a bare word/column reference. Identical mechanism to
/// `privileged_no_authz_checker::has_check_function_call`.
fn has_check_function_call(lower: &[char], call_substrings: &[&str]) -> bool {
    let mut i = 0usize;
    while i < lower.len() {
        let c = lower[i];
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < lower.len()
                && (lower[i].is_alphanumeric() || lower[i] == '_' || lower[i] == '.')
            {
                i += 1;
            }
            let ident: String = lower[start..i].iter().collect();
            let trailing = ident.rsplit('.').next().unwrap_or(&ident);
            let after = skip_ws(lower, i);
            if lower.get(after) == Some(&'(') && ident_contains_any(trailing, call_substrings) {
                return true;
            }
            continue;
        }
        i += 1;
    }
    false
}

/// Keywords whose presence, scanning BACKWARD from a comparison operator within the same
/// statement, mean the operator sits in a GUARD/conditional clause (`WHERE`, `IF`, `WHEN`, an
/// `ELSIF`, or a boolean `AND`/`OR` continuing one of those) rather than an assignment target.
/// Identical to `privileged_no_authz_checker::GUARD_CLAUSE_KEYWORDS`.
const GUARD_CLAUSE_KEYWORDS: &[&str] = &["where", "if", "when", "elsif", "and", "or"];

/// Keywords whose presence, scanning backward from a bare `=` within the same statement, mean
/// that `=` is an ASSIGNMENT rather than a comparison. Identical to
/// `privileged_no_authz_checker::ASSIGNMENT_CLAUSE_KEYWORDS`.
const ASSIGNMENT_CLAUSE_KEYWORDS: &[&str] = &["set", "values", "insert"];

/// Whether the comparison operator whose first character sits at `lower[op_pos]` is inside a
/// GUARD/conditional clause rather than an assignment target. Identical mechanism to
/// `privileged_no_authz_checker::is_guard_context` — see that module's doc comment for the full
/// rationale (this rule reuses it both for the authorization-predicate scan AND, separately,
/// for [`has_param_filter`]'s own equality scan).
fn is_guard_context(lower: &[char], op_pos: usize) -> bool {
    let mut i = op_pos;
    while i > 0 {
        i -= 1;
        if lower[i] == ';' {
            return false;
        }
        if lower[i].is_alphanumeric() || lower[i] == '_' {
            let mut start = i;
            while start > 0 && (lower[start - 1].is_alphanumeric() || lower[start - 1] == '_') {
                start -= 1;
            }
            let word: String = lower[start..=i].iter().collect();
            if GUARD_CLAUSE_KEYWORDS.contains(&word.as_str()) {
                return true;
            }
            if ASSIGNMENT_CLAUSE_KEYWORDS.contains(&word.as_str()) {
                return false;
            }
            if start == 0 {
                return false;
            }
            i = start;
        }
    }
    false
}

/// The identifier immediately preceding `pos` (skipping whitespace), if any. Identical
/// mechanism to `privileged_no_authz_checker::ident_before`.
fn ident_before(lower: &[char], pos: usize) -> Option<String> {
    let mut end = pos;
    while end > 0 && lower[end - 1].is_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && (lower[start - 1].is_alphanumeric() || lower[start - 1] == '_') {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some(lower[start..end].iter().collect())
}

/// The identifier immediately following `pos` (skipping whitespace), if any. Identical
/// mechanism to `privileged_no_authz_checker::ident_after`.
fn ident_after(lower: &[char], pos: usize) -> Option<String> {
    let mut start = pos;
    while start < lower.len() && lower[start].is_whitespace() {
        start += 1;
    }
    let begin = start;
    while start < lower.len() && (lower[start].is_alphanumeric() || lower[start] == '_') {
        start += 1;
    }
    if start == begin {
        return None;
    }
    Some(lower[begin..start].iter().collect())
}

/// Whether the LEFT or RIGHT operand of the comparison operator starting at `op_pos` (length
/// `op_len`) is an identifier containing one of `substrings`. Identical mechanism to
/// `privileged_no_authz_checker::operand_identifier_matches`.
fn operand_identifier_matches(
    lower: &[char],
    op_pos: usize,
    op_len: usize,
    substrings: &[&str],
) -> bool {
    if let Some(id) = ident_before(lower, op_pos) {
        if ident_contains_any(&id, substrings) {
            return true;
        }
    }
    if let Some(id) = ident_after(lower, op_pos + op_len) {
        if ident_contains_any(&id, substrings) {
            return true;
        }
    }
    false
}

/// Scan `lower` for a COMPARISON (`<>`, `!=`, or a guard-context bare `=`) whose operand on
/// either side is an identifier containing one of `substrings`. Identical mechanism to
/// `privileged_no_authz_checker::has_secret_guard_comparison`.
fn has_secret_guard_comparison(lower: &[char], substrings: &[&str]) -> bool {
    let mut i = 0usize;
    while i < lower.len() {
        if lower[i] == '<' && lower.get(i + 1) == Some(&'>') {
            if operand_identifier_matches(lower, i, 2, substrings) {
                return true;
            }
            i += 2;
            continue;
        }
        if lower[i] == '!' && lower.get(i + 1) == Some(&'=') {
            if operand_identifier_matches(lower, i, 2, substrings) {
                return true;
            }
            i += 2;
            continue;
        }
        if lower[i] == '=' {
            let prev_excludes = i > 0 && matches!(lower[i - 1], ':' | '<' | '>');
            if !prev_excludes
                && is_guard_context(lower, i)
                && operand_identifier_matches(lower, i, 1, substrings)
            {
                return true;
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    false
}

/// Whether a comment-stripped body contains ANY recognized authorization-predicate signal —
/// the SAME three signals `privileged_no_authz_checker::has_authorization_predicate` answers
/// (session/JWT identity accessors, a role/ownership CALL, a shared-secret/signature CALL-or-
/// COMPARISON). Duplicated per this module's own documented convention.
fn has_authorization_predicate(stripped_body: &str) -> bool {
    let lower_str = stripped_body.to_ascii_lowercase();

    const IDENTITY_SUBSTRINGS: &[&str] = &[
        "auth.uid(",
        "auth.jwt(",
        "auth.role(",
        "current_user",
        "session_user",
        "request.jwt",
    ];
    if IDENTITY_SUBSTRINGS.iter().any(|s| lower_str.contains(s)) {
        return true;
    }

    let lower: Vec<char> = lower_str.chars().collect();

    if has_check_function_call(&lower, ROLE_CHECK_CALL_SUBSTRINGS) {
        return true;
    }

    if has_check_function_call(&lower, SECRET_CHECK_SUBSTRINGS) {
        return true;
    }
    if has_secret_guard_comparison(&lower, SECRET_CHECK_SUBSTRINGS) {
        return true;
    }

    false
}

/// Whether `lower` (a comment-stripped, already-lowercased body) contains an equality
/// comparison (`=`, not `:=`/`<=`/`>=`) where one operand is an identifier EXACTLY matching one
/// of `param_names` (the function's own declared parameters) — the "filtered by the caller's
/// own parameter" shape this rule targets. Any occurrence anywhere in the body counts (not just
/// inside a `WHERE` clause) — a `RETURN QUERY SELECT ... WHERE col = p_id` and a `SELECT ...
/// INTO ... WHERE col = p_id; RETURN ...` are both covered by the same scan, matching this
/// corpus's "presence, not exact clause position" floor-tier discipline.
fn has_param_filter(lower: &[char], param_names: &[String]) -> bool {
    let mut i = 0usize;
    while i < lower.len() {
        if lower[i] == '=' {
            let prev_excludes = i > 0 && matches!(lower[i - 1], ':' | '<' | '>');
            let next_is_eq = lower.get(i + 1) == Some(&'=');
            if !prev_excludes && !next_is_eq {
                if let Some(left) = ident_before(lower, i) {
                    if param_names.iter().any(|p| p == &left) {
                        return true;
                    }
                }
                if let Some(right) = ident_after(lower, i + 1) {
                    if param_names.iter().any(|p| p == &right) {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView {
            spec: "test/repo",
            files,
        }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }

    fn rule_hits(vs: &[ArchViolation]) -> Vec<&ArchViolation> {
        vs.iter()
            .filter(|v| v.rule_id == RULE_DEFINER_CROSS_TENANT_READ)
            .collect()
    }

    // ── positives (shape variants) ─────────────────────────────────────────────────────

    #[test]
    fn definer_read_filtered_by_own_param_broad_grant_no_predicate_fires_high() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to authenticated;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert_eq!(vs[0].object.as_deref(), Some("public.get_org_invoices"));
        assert!(vs[0].message.contains("CWE-639"), "{}", vs[0].message);
    }

    #[test]
    fn create_or_replace_dialect_variant_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create or replace function public.get_tenant_records(p_tenant_id uuid) \
             returns setof records as $$ \
             begin \
             return query select * from records where tenant_id = p_tenant_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_tenant_records(uuid) to anon;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn clauses_after_the_dollar_quoted_body_dialect_variant_still_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_account_summary(p_account_id uuid) \
             returns text as $$ \
             begin \
             return (select summary from accounts where id = p_account_id); \
             end; \
             $$ language plpgsql security definer set search_path = public, pg_temp;\n\
             grant execute on function public.get_account_summary(uuid) to authenticated;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn select_into_then_return_shape_variant_fires() {
        // Shape variant on the READ construct itself: `SELECT ... INTO ...; RETURN ...;`
        // instead of `RETURN QUERY SELECT ...` or `RETURN (SELECT ...)`.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_profile_bio(p_profile_id uuid) returns text as $$ \
             declare v_bio text; \
             begin \
             select bio into v_bio from profiles where id = p_profile_id; \
             return v_bio; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_profile_bio(uuid) to anon;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn stored_procedure_variant_fires() {
        // Procedures can read too (e.g. via an OUT/INOUT parameter) — mirrors the sibling
        // WRITE rule's identical stored-procedure coverage.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create procedure public.fetch_org_name(p_org_id uuid, inout p_name text) as $$ \
             begin \
             select name into p_name from orgs where id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.fetch_org_name(uuid, text) to authenticated;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn reversed_operand_order_param_on_the_left_still_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where p_org_id = org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to authenticated;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    // ── safe twins (do-not-break list) ──────────────────────────────────────────────

    #[test]
    fn invoker_rights_sibling_filtering_on_caller_id_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql;\n\
             grant execute on function public.get_org_invoices(uuid) to authenticated;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn definer_with_caller_identity_predicate_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             if not exists ( \
               select 1 from org_members where org_id = p_org_id and user_id = auth.uid() \
             ) then \
               raise exception 'not authorized'; \
             end if; \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to authenticated;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn definer_with_role_check_call_predicate_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             if not public.has_role(auth.uid(), 'admin') then \
               raise exception 'not authorized'; \
             end if; \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to authenticated;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn granted_only_to_privileged_service_role_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to service_role_internal;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn no_grant_at_all_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn mutating_body_is_out_of_scope_for_this_rule() {
        // The sibling WRITE rule's shape — not double-flagged here even though the body also
        // filters by a caller-supplied parameter.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.delete_org_invoices(p_org_id uuid) returns void as $$ \
             begin \
             delete from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.delete_org_invoices(uuid) to authenticated;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn read_with_a_declared_param_never_used_as_a_filter_is_never_flagged() {
        // A declared parameter exists, but the body never equates any column against it — the
        // query's filter is a hardcoded literal instead. Distinct from the
        // `no_parameters_at_all` twin below: this one exercises `has_param_filter` actually
        // returning false, not the earlier `params.is_empty()` short-circuit.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_active_invoice_count(p_org_id uuid) returns bigint as $$ \
             begin \
             return (select count(*) from invoices where status = 'active'); \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_active_invoice_count(uuid) to anon;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn read_filtered_by_a_non_parameter_value_is_never_flagged() {
        // The equality compares two ordinary columns, not a declared function parameter.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_active_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where status = archived_status; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_active_invoices(uuid) to anon;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn no_parameters_at_all_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_latest_invoice() returns invoices as $$ \
             begin \
             return (select * from invoices order by created_at desc limit 1); \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_latest_invoice() to anon;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn all_safe_twins_plus_one_unsafe_in_one_file_discriminate_correctly() {
        let f = files(vec![(
            "supabase/migrations/0001_fns.sql",
            "create function public.unsafe_fn(p_org_id uuid) returns setof invoices as $$ \
             begin return query select * from invoices where org_id = p_org_id; end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_predicate(p_org_id uuid) returns setof invoices as $$ \
             begin \
             if auth.uid() is null then raise exception 'no'; end if; \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_service_role(p_org_id uuid) returns setof invoices as $$ \
             begin return query select * from invoices where org_id = p_org_id; end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_invoker(p_org_id uuid) returns setof invoices as $$ \
             begin return query select * from invoices where org_id = p_org_id; end; \
             $$ language plpgsql;\n\
             create function public.safe_no_filter() returns bigint as $$ \
             begin return (select count(*) from invoices); end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.unsafe_fn(uuid) to anon;\n\
             grant execute on function public.safe_predicate(uuid) to anon;\n\
             grant execute on function public.safe_service_role(uuid) to service_role_internal;\n\
             grant execute on function public.safe_invoker(uuid) to anon;\n\
             grant execute on function public.safe_no_filter() to anon;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(
            vs.len(),
            1,
            "expected exactly the one unsafe function: {vs:#?}"
        );
        assert_eq!(vs[0].object.as_deref(), Some("public.unsafe_fn"));
    }

    // ── GRANT correlation across files ──────────────────────────────────────────────

    #[test]
    fn grant_in_a_different_file_in_the_same_scan_still_fires() {
        let f = files(vec![
            (
                "supabase/migrations/0001_fn.sql",
                "create function public.get_org_invoices(p_org_id uuid) \
                 returns setof invoices as $$ \
                 begin \
                 return query select * from invoices where org_id = p_org_id; \
                 end; \
                 $$ language plpgsql security definer;\n",
            ),
            (
                "supabase/migrations/0002_grants.sql",
                "grant execute on function public.get_org_invoices(uuid) to authenticated;\n",
            ),
        ]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn grant_on_an_unrelated_function_does_not_cause_a_false_positive() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             create function public.other_fn() returns void as $$ begin null; end; $$ language plpgsql;\n\
             grant execute on function public.other_fn() to anon;\n",
        )]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    // ── scoping / hygiene ────────────────────────────────────────────────────────────

    #[test]
    fn no_supabase_files_never_applies() {
        let f = files(vec![("README.md", "hello")]);
        assert!(!crate::arch_checker::checker_applies(
            &CrossTenantDefinerReadChecker,
            &f
        ));
    }

    #[test]
    fn plain_postgres_layout_outside_supabase_folder_is_inspected() {
        let f = files(vec![(
            "db/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to anon;\n",
        )]);
        assert!(crate::arch_checker::checker_applies(
            &CrossTenantDefinerReadChecker,
            &f
        ));
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn mutating_keyword_mentioned_only_inside_a_comment_still_fires_as_a_read() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             -- update invoices set archived = true where org_id = p_org_id;\n\
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to anon;\n",
        )]);
        let violations = CrossTenantDefinerReadChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(
            vs.len(),
            1,
            "a commented-out mutating keyword must not pull the body into the sibling WRITE \
             rule's scope: {vs:#?}"
        );
    }

    #[test]
    fn empty_file_does_not_panic() {
        let f = files(vec![("a.sql", "")]);
        assert!(rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn unterminated_function_body_does_not_panic() {
        let f = files(vec![(
            "a.sql",
            "create function public.f(p_id uuid) returns setof t as $$ begin return query select * from t where id = p_id",
        )]);
        let _ = CrossTenantDefinerReadChecker.check(&view(&f)); // must not panic
    }

    #[test]
    fn create_or_replace_supersedes_prior_definition_last_write_wins() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_org_invoices(p_org_id uuid) returns setof invoices as $$ \
             begin return query select * from invoices where org_id = p_org_id; end; \
             $$ language plpgsql security definer;\n\
             create or replace function public.get_org_invoices(p_org_id uuid) \
             returns setof invoices as $$ \
             begin \
             if auth.uid() is null then raise exception 'no'; end if; \
             return query select * from invoices where org_id = p_org_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_org_invoices(uuid) to anon;\n",
        )]);
        assert!(
            rule_hits(&CrossTenantDefinerReadChecker.check(&view(&f))).is_empty(),
            "the LATER (safe) definition must win, mirroring timeline's CREATE OR REPLACE semantics"
        );
    }
}
