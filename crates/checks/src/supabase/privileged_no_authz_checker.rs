//! `PrivilegedFunctionNoAuthzChecker`: detects `SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1` —
//! a `SECURITY DEFINER` SQL/plpgsql function that performs a mutating/privileged write
//! (`INSERT`/`UPDATE`/`DELETE`/`TRUNCATE`/`MERGE`), is `GRANT EXECUTE`d to a broad,
//! anonymous-or-any-authenticated-caller role (`anon`/`public`/`authenticated`), and whose
//! body contains NO server-side role/ownership/secret predicate gating that write.
//!
//! # Why this is a distinct gap from the sibling checkers
//!
//! `search_path_checker` (`SUPABASE-FUNC-SEARCH-PATH-1`) and `dynamic_sql_exec_checker`
//! (`SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1`) both inspect `SECURITY DEFINER` function bodies,
//! but for entirely different defects (a missing `search_path` pin; unsafe dynamic-SQL
//! construction). Neither has any concept of "this function's body never checks WHO is
//! calling it before performing a write" — a `SECURITY DEFINER` function runs with the
//! DEFINER's privileges regardless of caller, so a mutating one with no caller-identity check,
//! reachable by `anon`/`authenticated`, is a privilege-escalation primitive (CWE-862: Missing
//! Authorization) even though its SQL is perfectly well-formed and injection-free. This was
//! the ONE class only the AI tier caught (hedged, uncited, no concrete fix) on an unseen
//! hold-out repo — this checker closes it deterministically.
//!
//! # The mechanism, precisely
//!
//! A finding requires ALL FOUR of:
//! 1. **Privileged**: the function is declared `SECURITY DEFINER` (anywhere in its signature —
//!    before OR after the dollar-quoted body, mirroring `sql_parse::classify_statement`'s own
//!    "options can appear on either side of `AS $$...$$`" discipline), AND
//! 2. **Mutating**: the body contains a standalone `INSERT`/`UPDATE`/`DELETE`/`TRUNCATE`/`MERGE`
//!    keyword — Postgres's write-statement vocabulary (`MERGE` is PG15+; included for forward
//!    compatibility even though the rest of this corpus assumes no specific PG version), AND
//! 3. **Broadly reachable**: a `GRANT EXECUTE ON FUNCTION <this function> TO
//!    anon|public|authenticated` exists anywhere in the same scanned SQL corpus — reusing the
//!    EXACT cross-file grant-correlation mechanism `dynamic_sql_exec_checker` introduced
//!    (`parse_grant_execute_sensitive`, duplicated here per this module's own stated
//!    convention of each lexical checker owning its small parsing primitives), AND
//! 4. **No authorization predicate**: the body contains none of the recognized
//!    identity/role/ownership/secret-check idioms — see [`has_authorization_predicate`].
//!
//! Severity is always HIGH — reachability (condition 3) is baked into the trigger condition
//! itself here, unlike `dynamic_sql_exec_checker` where the grant only modulates severity; a
//! privileged mutating function with NO broad grant is simply out of this rule's scope (it may
//! still be unauthorized in principle, but nothing makes it reachable by an anonymous/any-
//! authenticated caller, so it isn't the CWE-862-via-PostgREST shape this rule targets).
//!
//! # The "no predicate" check, precisely — and the compensating-control exemption
//!
//! [`has_authorization_predicate`] looks for THREE independent signal shapes, ANY of which is
//! sufficient to treat the function as authorized (never flagged):
//!
//! 1. **Session/JWT identity accessors** — `auth.uid(`, `auth.jwt(`, `auth.role(` (Supabase's
//!    own PostgREST helpers), `current_user`, `session_user`, or a `current_setting('request.
//!    jwt...')` custom-claims read. These are the standard "who is calling me" primitives; a
//!    function that reads one of them and goes on to compare it against an owner/role column is
//!    doing exactly the role/ownership check this rule wants.
//! 2. **A role/ownership/permission CHECK FUNCTION CALL** — an identifier immediately followed
//!    by `(` whose name contains `role`, `owner`, `permission`, `authoriz` (authorize/
//!    authorise/authorization), `admin`, or `verify` (e.g. `has_role(`, `is_owner(`,
//!    `check_permission(`, `verify_caller(`). Scoped to CALL FORM specifically (name directly
//!    followed by `(`) rather than "this word appears anywhere" — a mutating function that
//!    merely assigns a `role` COLUMN (`update users set role = p_new_role ...`) must NOT be
//!    exempted by the bare word "role" sitting in an UPDATE's target-list; only an actual
//!    function CALL whose name encodes a role/ownership check counts.
//! 3. **A shared-secret/signature compensating control** — any identifier token anywhere in the
//!    body containing `secret`, `token`, or `signature` (e.g. a parameter `p_webhook_secret`
//!    compared against a stored value, or a call to a signature-verification helper). Per the
//!    hold-out's do-not-break list, a function gated by a caller-supplied shared secret/HMAC
//!    signature IS a legitimate server-side authorization control (the UI never sees the
//!    secret), even though it isn't a role/ownership check in the RLS sense — so it must not be
//!    flagged. This signal is intentionally broader than (2)'s call-form restriction: these
//!    words are specific enough (unlike `role`/`owner`, which collide with ordinary business
//!    columns) that a plain-comparison form (`if p_token <> stored_token then raise ...`), not
//!    just a function call, is accepted as evidence of the check.
//!
//! This mirrors `dynamic_sql_exec_checker`'s own "false negative over false positive" floor-
//! detector discipline: being generous about what counts as a predicate trades a handful of
//! theoretically-missed true positives (a function whose ONLY mention of `role` happens to be
//! an unrelated column assignment inside an otherwise-unauthorized body) for zero false alarms
//! on the overwhelmingly more common case of a genuinely-guarded privileged function.
//!
//! # What this deliberately does NOT do
//!
//! Comments are stripped before scanning (so a `-- insert into ... ` mention inside a comment
//! never counts as the mutating statement, and a commented-out `-- auth.uid()` never counts as
//! the authorization predicate) — see [`strip_sql_comments`]. String-literal content is NOT
//! masked out before the mutating-write scan: a function that performs its write via dynamic
//! SQL (`EXECUTE 'DELETE FROM sessions WHERE id = ' || p_id`) is still mutating, exactly as
//! `dynamic_sql_exec_checker`'s own `EXECUTE` scan does not special-case string content either.
//! No cross-statement dataflow tracking, and no attempt to verify the predicate actually GATES
//! the write (e.g. appears in the same `IF` branch) — its mere presence anywhere in the body is
//! treated as evidence of authorization-awareness, matching this corpus's documented floor-tier
//! scope line.

use super::splitter::{parse_dollar_tag, split_statements, SqlStatement};
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};

pub const RULE_PRIVILEGED_NO_AUTHZ: &str = "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1";

const RULE_IDS: &[&str] = &[RULE_PRIVILEGED_NO_AUTHZ];

/// `.sql` anywhere in the repo — see `dynamic_sql_exec_checker`'s identical choice: the
/// vulnerability is general Postgres/PostgREST, not tied to Supabase's folder convention.
const INTEREST_GLOBS: &[&str] = &["**/*.sql"];

const DEFAULT_SCHEMA: &str = "public";

/// Roles a `GRANT EXECUTE` to which makes a function reachable by an anonymous or
/// unauthenticated-by-default API caller. Identical list to `dynamic_sql_exec_checker`'s own
/// `SENSITIVE_GRANT_ROLES` — `anon`/`authenticated` are Supabase/PostgREST's own built-in
/// roles; `public` is Postgres's own pseudo-role meaning "every role."
const SENSITIVE_GRANT_ROLES: &[&str] = &["anon", "public", "authenticated"];

/// Write-statement keywords that make a function body "mutating" for this rule's purposes.
/// `MERGE` is PG15+; included for forward compatibility.
const MUTATING_KEYWORDS: &[&str] = &["insert", "update", "delete", "truncate", "merge"];

/// Function-CALL name substrings (see module doc, signal 2) that count as a role/ownership/
/// permission check — ONLY when the identifier is directly followed by `(`.
const ROLE_CHECK_CALL_SUBSTRINGS: &[&str] =
    &["role", "owner", "permission", "authoriz", "admin", "verify"];

/// Plain-text identifier substrings (see module doc, signal 3) that count as a shared-secret/
/// signature compensating control wherever they appear in the body, not just in call form.
const SECRET_CHECK_SUBSTRINGS: &[&str] = &["secret", "token", "signature"];

pub struct PrivilegedFunctionNoAuthzChecker;

impl ArchChecker for PrivilegedFunctionNoAuthzChecker {
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
                let stripped = strip_sql_comments(&f.body);
                if !is_mutating(&stripped) {
                    return None;
                }
                let key = (f.schema.clone(), f.name.clone());
                if !granted.contains(&key) {
                    return None;
                }
                if has_authorization_predicate(&stripped) {
                    return None;
                }
                Some(ArchViolation {
                    rule_id: RULE_PRIVILEGED_NO_AUTHZ.to_string(),
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
/// `dynamic_sql_exec_checker::CandidateFunction`, plus the `security_definer` flag this rule
/// additionally needs.
struct CandidateFunction {
    schema: String,
    name: String,
    security_definer: bool,
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
        "One of your database functions ({name}) is `SECURITY DEFINER` (runs with the \
         function owner's privileges regardless of caller), performs a mutating write \
         (INSERT/UPDATE/DELETE/TRUNCATE/MERGE), and is GRANTed EXECUTE to an anonymous or \
         any-authenticated-user role (anon/public/authenticated) in this same SQL corpus — but \
         its body never checks who the caller is before writing. This is CWE-862 (Missing \
         Authorization): any caller matching the broad grant can trigger the privileged write, \
         regardless of whether they actually own or are otherwise entitled to affect the \
         row(s) involved. Defined at {}:{}. Fix: add a server-side identity/ownership check \
         inside the function body BEFORE the write — e.g. `if auth.uid() <> owner_id then \
         raise exception ...;` or a `has_role(auth.uid(), 'admin')`-style call — or, if no \
         caller should invoke this directly, revoke the broad grant and invoke it only from a \
         trusted server-side path. A client-side/UI-only check is never a substitute: it \
         constrains nothing once the caller can reach the function directly (e.g. via the \
         PostgREST RPC endpoint), so it provides no actual authorization. This reflects the \
         migration history in this repository only — confirm the deployed function definition \
         matches before treating this as settled.",
        f.file, f.line,
    )
}

// ── CREATE FUNCTION / PROCEDURE candidate parsing ───────────────────────────────────────
//
// Deliberately duplicated (not shared via `pub(crate)`) from `dynamic_sql_exec_checker.rs` —
// mirrors that module's own documented convention of each lexical checker owning its small
// parsing primitives rather than threading a shared dependency through the tree. The only
// addition versus that module's `parse_create_function_candidate` is capturing whether
// `SECURITY DEFINER` appears anywhere in the signature (header or trailer around the body),
// mirroring `sql_parse::classify_statement`'s own "either side of AS $$...$$" scan.

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

/// Whether `words` occurs anywhere in `chars` (already uppercased/lowercased to match `words`'
/// own case) as a standalone keyword sequence — used to scan the signature's header/trailer
/// for `SECURITY DEFINER`, which can appear at any position, not just the start.
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

/// Parse one `CREATE [OR REPLACE] FUNCTION|PROCEDURE schema.name(...) ... AS $tag$ body $tag$
/// ...` statement into a [`CandidateFunction`], including whether `SECURITY DEFINER` appears
/// anywhere in the signature (before or after the body). Returns `None` for anything else
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
    if lower.get(after_params) == Some(&'(') {
        if let Some((_, close_idx)) = extract_paren_inner(&raw, after_params) {
            after_params = close_idx + 1;
        }
    }

    let (body_start, body_end) = find_dollar_quoted_body(&raw, after_params)?;
    let body: String = raw[body_start..body_end].iter().collect();

    // SECURITY DEFINER can appear in the header (between the param list and the body) or the
    // trailer (after the closing dollar-quote, e.g. `$$ language plpgsql security definer;`) —
    // scan both, mirroring `sql_parse::classify_statement`'s identical two-sided scan.
    let header = lower.get(after_params..body_start).unwrap_or(&[]);
    let trailer = lower.get(body_end..).unwrap_or(&[]);
    let security_definer = contains_kw_seq(header, &["security", "definer"])
        || contains_kw_seq(trailer, &["security", "definer"]);

    Some(CandidateFunction {
        schema,
        name,
        security_definer,
        body,
        file: file.to_string(),
        line: stmt.line,
    })
}

// ── GRANT EXECUTE ... TO <sensitive role> parsing ───────────────────────────────────────
//
// Byte-for-byte the same mechanism as `dynamic_sql_exec_checker::parse_grant_execute_sensitive`
// — duplicated per this module's stated convention (see the module doc).

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

// ── function-body scan: mutating write + authorization predicate ───────────────────────

/// Strip `--` line comments and `/* */` block comments (non-nesting) from a plpgsql body,
/// treating `'...'` and `"..."` as opaque. Byte-for-byte the same mechanism as
/// `dynamic_sql_exec_checker::strip_sql_comments` — duplicated per this module's convention.
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

/// Whether a comment-stripped body contains a standalone write-statement keyword (see
/// [`MUTATING_KEYWORDS`]). String-literal content is deliberately NOT masked out — a write
/// performed via dynamic SQL (`EXECUTE 'DELETE FROM ...' || p`) is still a real write; see the
/// module doc's "what this deliberately does NOT do."
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
/// CALL, not a bare word/column reference. See module doc signal 2.
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
            // Only the trailing dotted segment matters for a schema-qualified call
            // (`public.has_role(` should match on `has_role`, not the whole dotted string
            // happening to contain one of the substrings via an unrelated schema name).
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

/// Scan `lower` (a comment-stripped, already-lowercased body) for ANY identifier token
/// (anywhere — call form or plain reference) containing one of `substrings`. See module doc
/// signal 3: deliberately broader than [`has_check_function_call`] because these particular
/// words are specific enough not to collide with ordinary business-data column names.
fn has_identifier_containing_any(lower: &[char], substrings: &[&str]) -> bool {
    let mut i = 0usize;
    while i < lower.len() {
        let c = lower[i];
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < lower.len() && (lower[i].is_alphanumeric() || lower[i] == '_') {
                i += 1;
            }
            let ident: String = lower[start..i].iter().collect();
            if ident_contains_any(&ident, substrings) {
                return true;
            }
            continue;
        }
        i += 1;
    }
    false
}

/// Whether a comment-stripped body contains ANY recognized authorization-predicate signal —
/// see the module doc's three-signal breakdown (identity accessors, role/ownership CALL form,
/// shared-secret/signature compensating control).
fn has_authorization_predicate(stripped_body: &str) -> bool {
    let lower_str = stripped_body.to_ascii_lowercase();

    // Signal 1: session/JWT identity accessors — plain substrings are precise enough (these
    // are fixed Supabase/Postgres idioms, not generic words), so no identifier/call-form
    // restriction is needed here.
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

    // Signal 2: a role/ownership/permission CHECK FUNCTION CALL (call form only).
    if has_check_function_call(&lower, ROLE_CHECK_CALL_SUBSTRINGS) {
        return true;
    }

    // Signal 3: a shared-secret/signature compensating control (any identifier form).
    if has_identifier_containing_any(&lower, SECRET_CHECK_SUBSTRINGS) {
        return true;
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
            .filter(|v| v.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
            .collect()
    }

    // ── positives ────────────────────────────────────────────────────────────────────

    #[test]
    fn definer_mutating_broad_grant_no_predicate_fires_high() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.promote_user(p_user_id uuid, p_new_role text) \
             returns void as $$ \
             begin \
             update users set role = p_new_role where id = p_user_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.promote_user(uuid, text) to authenticated;\n",
        )]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert_eq!(vs[0].object.as_deref(), Some("public.promote_user"));
        assert!(
            vs[0].message.contains("authenticated")
                || vs[0].message.contains("anon")
                || vs[0].message.contains("public")
        );
    }

    #[test]
    fn create_or_replace_dialect_variant_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create or replace function public.delete_account(p_id uuid) \
             returns void as $$ \
             begin \
             delete from accounts where id = p_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.delete_account(uuid) to anon;\n",
        )]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn clauses_after_the_dollar_quoted_body_dialect_variant_still_fires() {
        // SECURITY DEFINER placed AFTER the body (Supabase's own recommended style) must
        // still be detected — mirrors `sql_parse`'s own two-sided scan.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.grant_credit(p_user_id uuid, p_amount numeric) \
             returns void as $$ \
             begin \
             insert into ledger (user_id, amount) values (p_user_id, p_amount); \
             end; \
             $$ language plpgsql security definer set search_path = public, pg_temp;\n\
             grant execute on function public.grant_credit(uuid, numeric) to authenticated;\n",
        )]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn truncate_and_merge_keyword_variants_fire() {
        let f = files(vec![
            (
                "supabase/migrations/0001_fn.sql",
                "create function public.reset_sessions() returns void as $$ \
                 begin truncate table sessions; end; \
                 $$ language plpgsql security definer;\n\
                 grant execute on function public.reset_sessions() to anon;\n",
            ),
            (
                "supabase/migrations/0002_fn.sql",
                "create function public.sync_balance(p_id uuid, p_amount numeric) \
                 returns void as $$ \
                 begin \
                 merge into balances b using (select p_id as id, p_amount as amount) s \
                 on b.id = s.id \
                 when matched then update set b.amount = s.amount \
                 when not matched then insert (id, amount) values (s.id, s.amount); \
                 end; \
                 $$ language plpgsql security definer;\n\
                 grant execute on function public.sync_balance(uuid, numeric) to public;\n",
            ),
        ]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 2, "{vs:#?}");
    }

    // ── safe twins (do-not-break list) ──────────────────────────────────────────────

    #[test]
    fn same_function_with_auth_uid_ownership_predicate_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.update_profile(p_id uuid, p_bio text) \
             returns void as $$ \
             begin \
             if auth.uid() <> (select owner_id from profiles where id = p_id) then \
               raise exception 'not authorized'; \
             end if; \
             update profiles set bio = p_bio where id = p_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.update_profile(uuid, text) to authenticated;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn same_function_with_has_role_call_predicate_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.ban_user(p_id uuid) returns void as $$ \
             begin \
             if not public.has_role(auth.uid(), 'admin') then \
               raise exception 'not authorized'; \
             end if; \
             update users set banned = true where id = p_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.ban_user(uuid) to authenticated;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn granted_only_to_privileged_service_role_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.adjust_ledger(p_id uuid, p_amount numeric) \
             returns void as $$ \
             begin \
             update ledger set amount = amount + p_amount where id = p_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.adjust_ledger(uuid, numeric) to service_role_internal;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn security_definer_read_only_body_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.get_report(p_id uuid) returns text as $$ \
             begin \
             return (select contents from reports where id = p_id); \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.get_report(uuid) to authenticated;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn invoker_rights_function_even_if_broadly_granted_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.update_self(p_id uuid, p_bio text) \
             returns void as $$ \
             begin \
             update profiles set bio = p_bio where id = p_id; \
             end; \
             $$ language plpgsql;\n\
             grant execute on function public.update_self(uuid, text) to authenticated;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn shared_secret_compensating_control_is_never_flagged() {
        // The hold-out's do-not-break case: a webhook-style handler gated by a caller-supplied
        // shared secret compared against a stored config value — a legitimate server-side
        // authorization control even though it isn't a role/ownership check.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.handle_webhook(p_secret text, p_order_id uuid) \
             returns void as $$ \
             declare v_expected text; \
             begin \
             v_expected := current_setting('app.settings.webhook_secret'); \
             if p_secret <> v_expected then \
               raise exception 'invalid secret'; \
             end if; \
             update orders set status = 'paid' where id = p_order_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.handle_webhook(text, uuid) to anon;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn signature_verification_call_compensating_control_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.handle_stripe_event(p_payload text, p_signature text) \
             returns void as $$ \
             begin \
             if not public.verify_signature(p_payload, p_signature) then \
               raise exception 'bad signature'; \
             end if; \
             insert into payment_events (payload) values (p_payload); \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.handle_stripe_event(text, text) to anon;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn bare_role_column_assignment_without_a_real_check_still_fires() {
        // Regression guard for the exact false-exemption risk the module doc calls out: the
        // word "role" appearing only as an UPDATE target-list column (not a function CALL)
        // must NOT be treated as an authorization predicate.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.set_role(p_user_id uuid, p_role text) \
             returns void as $$ \
             begin \
             update users set role = p_role where id = p_user_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.set_role(uuid, text) to authenticated;\n",
        )]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(
            vs.len(),
            1,
            "a bare 'role' column reference must not exempt an otherwise-unguarded write: {vs:#?}"
        );
    }

    #[test]
    fn all_safe_twins_plus_one_unsafe_in_one_file_discriminate_correctly() {
        let f = files(vec![(
            "supabase/migrations/0001_fns.sql",
            "create function public.unsafe_fn(p_id uuid) returns void as $$ \
             begin update accounts set balance = 0 where id = p_id; end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_predicate(p_id uuid) returns void as $$ \
             begin \
             if auth.uid() <> p_id then raise exception 'no'; end if; \
             update accounts set balance = 0 where id = p_id; \
             end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_service_role(p_id uuid) returns void as $$ \
             begin update accounts set balance = 0 where id = p_id; end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_readonly(p_id uuid) returns numeric as $$ \
             begin return (select balance from accounts where id = p_id); end; \
             $$ language plpgsql security definer;\n\
             create function public.safe_invoker(p_id uuid) returns void as $$ \
             begin update accounts set balance = 0 where id = p_id; end; \
             $$ language plpgsql;\n\
             grant execute on function public.unsafe_fn(uuid) to anon;\n\
             grant execute on function public.safe_predicate(uuid) to anon;\n\
             grant execute on function public.safe_service_role(uuid) to service_role_internal;\n\
             grant execute on function public.safe_readonly(uuid) to anon;\n\
             grant execute on function public.safe_invoker(uuid) to anon;\n",
        )]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
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
                "create function public.delete_record(p_id uuid) returns void as $$ \
                 begin delete from records where id = p_id; end; \
                 $$ language plpgsql security definer;\n",
            ),
            (
                "supabase/migrations/0002_grants.sql",
                "grant execute on function public.delete_record(uuid) to authenticated;\n",
            ),
        ]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn grant_on_an_unrelated_function_does_not_cause_a_false_positive() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.delete_record(p_id uuid) returns void as $$ \
             begin delete from records where id = p_id; end; \
             $$ language plpgsql security definer;\n\
             create function public.other_fn() returns void as $$ begin null; end; $$ language plpgsql;\n\
             grant execute on function public.other_fn() to anon;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn no_grant_at_all_is_never_flagged() {
        // Reachability is baked into the trigger condition itself for this rule — unlike
        // `dynamic_sql_exec_checker`, there is no "High without a grant" tier.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.delete_record(p_id uuid) returns void as $$ \
             begin delete from records where id = p_id; end; \
             $$ language plpgsql security definer;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    // ── scoping / hygiene ────────────────────────────────────────────────────────────

    #[test]
    fn no_supabase_files_never_applies() {
        let f = files(vec![("README.md", "hello")]);
        assert!(!crate::arch_checker::checker_applies(
            &PrivilegedFunctionNoAuthzChecker,
            &f
        ));
    }

    #[test]
    fn plain_postgres_layout_outside_supabase_folder_is_inspected() {
        let f = files(vec![(
            "db/migrations/0001_fn.sql",
            "create function public.delete_record(p_id uuid) returns void as $$ \
             begin delete from records where id = p_id; end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.delete_record(uuid) to anon;\n",
        )]);
        assert!(crate::arch_checker::checker_applies(
            &PrivilegedFunctionNoAuthzChecker,
            &f
        ));
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn mutating_keyword_mentioned_only_inside_a_comment_is_not_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.f(p_id uuid) returns void as $$ \
             begin \
             -- delete from records where id = p_id; \
             null; end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.f(uuid) to anon;\n",
        )]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn empty_file_does_not_panic() {
        let f = files(vec![("a.sql", "")]);
        assert!(rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn unterminated_function_body_does_not_panic() {
        let f = files(vec![(
            "a.sql",
            "create function public.f(p_id uuid) returns void as $$ begin delete from t where id = p_id",
        )]);
        let _ = PrivilegedFunctionNoAuthzChecker.check(&view(&f)); // must not panic
    }

    #[test]
    fn create_or_replace_supersedes_prior_definition_last_write_wins() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.f(p_id uuid) returns void as $$ \
             begin delete from t where id = p_id; end; \
             $$ language plpgsql security definer;\n\
             create or replace function public.f(p_id uuid) returns void as $$ \
             begin \
             if auth.uid() <> p_id then raise exception 'no'; end if; \
             delete from t where id = p_id; \
             end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.f(uuid) to anon;\n",
        )]);
        assert!(
            rule_hits(&PrivilegedFunctionNoAuthzChecker.check(&view(&f))).is_empty(),
            "the LATER (safe) definition must win, mirroring timeline's CREATE OR REPLACE semantics"
        );
    }

    #[test]
    fn stored_procedure_variant_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create procedure public.delete_record(p_id uuid) as $$ \
             begin delete from records where id = p_id; end; \
             $$ language plpgsql security definer;\n\
             grant execute on function public.delete_record(uuid) to anon;\n",
        )]);
        let violations = PrivilegedFunctionNoAuthzChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }
}
