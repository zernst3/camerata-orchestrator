//! `DynamicSqlExecInjectionChecker`: detects `SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1` —
//! a SQL/plpgsql `FUNCTION` (or `PROCEDURE`) whose body `EXECUTE`s a dynamically-assembled
//! query string built via NON-QUOTING interpolation — `||` string concatenation, or a
//! `format()` call using the non-escaping `%s` placeholder — rather than `format()`'s own
//! escaping placeholders (`%L` quotes a literal, `%I` quotes an identifier) or a bound
//! `EXECUTE '...' USING ...` argument.
//!
//! # Why this is a distinct gap from `SEC-NO-QUERY-GRAMMAR-INJECTION-1`
//!
//! `query_grammar_injection_checker` catches raw-SQL concatenation in APPLICATION code
//! (`.query(...)`, `.execute(...)`, `.raw(...)` calls in TS/JS/Python/Go/Ruby/Java/C#) — but
//! it only scans those language extensions, never `.sql` files, and it has no concept of a
//! plpgsql function body at all. A migration that defines a `SECURITY DEFINER` (or plain)
//! Postgres function whose body dynamically builds and `EXECUTE`s a query string from one of
//! its own parameters is a DIFFERENT shape of the same CWE-89 defect, invisible to every
//! existing checker — this was the one detection miss found on an unseen hold-out repo (see
//! `docs/plans/` for the hardening-cycle writeup this closes).
//!
//! # The mechanism, precisely
//!
//! A finding requires BOTH:
//! 1. the function body contains an `EXECUTE` statement (plpgsql's dynamic-SQL execution
//!    statement — covers `EXECUTE '...'`, `EXECUTE format(...)`, and `RETURN QUERY EXECUTE
//!    ...`, since the search is for the bare keyword wherever it appears), AND
//! 2. the executed expression is assembled via NON-QUOTING interpolation: a `format()` call
//!    whose template string contains the literal `%s` placeholder (which does NOT escape its
//!    argument — unlike `%L`, which quotes a literal, or `%I`, which quotes an identifier)
//!    paired with at least one NON-LITERAL argument, OR a top-level `||` concatenation chain
//!    with at least one non-literal segment.
//!
//! Mirroring `query_grammar_injection_checker`'s own documented discipline (see that module's
//! doc comment): this does NOT attempt to prove the non-literal operand is actually
//! request-derived, nor does it require it to textually match one of the function's declared
//! parameter names. ANY non-literal expression flowing into the executed string through one of
//! these two non-escaping channels is flagged — the defect is the STRING being built this way
//! at all, not the specific provenance of the value. A hand-rolled local variable that looks
//! "internal" today can become caller-derived after the next refactor without this rule's
//! match-set needing to change, and the fix (use `%L`/`%I`, or bind via `USING`) is the same
//! either way.
//!
//! # Severity: the `GRANT` drives it
//!
//! A deterministic-but-not-reachable-from-outside dynamic-SQL function is still a real defect
//! (HIGH) — but a function additionally reachable by an anonymous/public-facing API caller
//! (a `GRANT EXECUTE ON FUNCTION ... TO anon|public|authenticated` anywhere in the same scanned
//! SQL corpus) is CRITICAL: an unauthenticated caller can reach the injection directly. This
//! mirrors `rls_checker`'s own reachability-driven severity key (see its `SEVERITY_INFO`
//! doc comment) — the technical defect is identical either way; what changes is who can trigger
//! it.
//!
//! # Safe twins — the class boundary
//!
//! - `format('... %L ...', p)` / `format('... %I ...', p)` — the ESCAPING placeholders. A
//!   template with no `%s` at all is never flagged, regardless of its arguments.
//! - `EXECUTE '... $1 ...' USING p` — a bound argument; `USING` is Postgres's own
//!   parameterized dynamic-SQL form, exactly analogous to a prepared-statement placeholder.
//! - A fully static query string with no interpolation at all.
//! - `format()`/`||` where every operand is a literal (a quoted string, a number, `NULL`) —
//!   carries no runtime-determined content, so there is nothing to inject.
//!
//! # What this deliberately does NOT do
//!
//! Only DOLLAR-QUOTED function bodies (`AS $$ ... $$` / `AS $tag$ ... $tag$`) are parsed — a
//! function body supplied as an ordinary single-quoted string (rare for plpgsql, since the
//! body would need to double every embedded quote) is not inspected. No cross-statement
//! dataflow tracking: a `v_sql := '...' || p; EXECUTE v_sql;` two-statement pattern is not
//! connected (the `EXECUTE` keyword's own argument is examined, not a variable fed to it
//! earlier) — this is a conscious floor-detector scope line, matching
//! `query_grammar_injection_checker`'s "false negative over false positive from a more
//! aggressive parser" discipline. No positional mapping between a `format()` call's `%s`
//! occurrences and its argument list — ANY non-literal argument alongside ANY `%s` in the
//! template is flagged, even if that particular argument's position corresponds to a `%L`/`%I`
//! placeholder instead.

use super::splitter::{parse_dollar_tag, split_statements, SqlStatement};
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_CRITICAL, SEVERITY_HIGH};

pub const RULE_DYNAMIC_SQL_EXEC_INJECTION: &str = "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1";

const RULE_IDS: &[&str] = &[RULE_DYNAMIC_SQL_EXEC_INJECTION];

/// `.sql` anywhere in the repo — see `search_path_checker`'s identical choice: the
/// vulnerability is general Postgres, not Supabase-specific, so this is NOT scoped to
/// `supabase/migrations/*.sql`.
const INTEREST_GLOBS: &[&str] = &["**/*.sql"];

const DEFAULT_SCHEMA: &str = "public";

/// Roles a `GRANT EXECUTE` to which makes a function reachable by an anonymous or
/// unauthenticated-by-default API caller. `anon`/`authenticated` are Supabase/PostgREST's own
/// built-in roles; `public` is Postgres's own pseudo-role meaning "every role."
const SENSITIVE_GRANT_ROLES: &[&str] = &["anon", "public", "authenticated"];

pub struct DynamicSqlExecInjectionChecker;

impl ArchChecker for DynamicSqlExecInjectionChecker {
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
        // Deterministic iteration order across runs (matches `timeline::build_timeline_from_globs`'s
        // own filename-sort discipline) — doesn't affect WHICH functions/grants are found (both
        // maps are keyed by identity, last-write-wins for functions), only finding ORDER.
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
                let hit = unsafe_dynamic_exec(&f.body)?;
                let key = (f.schema.clone(), f.name.clone());
                let severity = if granted.contains(&key) {
                    SEVERITY_CRITICAL
                } else {
                    SEVERITY_HIGH
                };
                Some(ArchViolation {
                    rule_id: RULE_DYNAMIC_SQL_EXEC_INJECTION.to_string(),
                    file: f.file.clone(),
                    line: f.line,
                    object: Some(format!("{}.{}", f.schema, f.name)),
                    severity,
                    message: message_for(f, hit, severity),
                })
            })
            .collect()
    }
}

/// One `CREATE [OR REPLACE] FUNCTION`/`PROCEDURE` this checker was able to parse: enough to
/// scan its body and attribute a finding. Parameter names are deliberately NOT modeled (see
/// the module doc's "what this deliberately does NOT do") — the body scan flags by SHAPE
/// (non-literal operand through a non-escaping channel), not by matching a specific declared
/// parameter name.
struct CandidateFunction {
    schema: String,
    name: String,
    /// The dollar-quoted body's verbatim interior text (comments NOT yet stripped — see
    /// [`unsafe_dynamic_exec`], which strips them before scanning).
    body: String,
    file: String,
    line: usize,
}

fn message_for(f: &CandidateFunction, hit: &str, severity: &'static str) -> String {
    let name = if f.schema == "public" {
        format!("`{}`", f.name)
    } else {
        format!("`{}.{}`", f.schema, f.name)
    };
    let reach = if severity == SEVERITY_CRITICAL {
        " and is GRANTed EXECUTE to an anonymous/public-facing role (anon/public/authenticated) \
         in this same SQL corpus, so an unauthenticated API caller can trigger it directly"
    } else {
        ""
    };
    format!(
        "One of your database functions ({name}) runs a dynamically-assembled query string via \
         `EXECUTE` built with {hit} rather than an escaping/bound channel{reach}. This is SQL \
         injection (CWE-89): an attacker-controlled value flowing into this expression can \
         terminate the intended query and append arbitrary SQL, exactly as string-concatenated \
         SQL in application code does. Defined at {}:{}. Fix: use `format()`'s escaping \
         placeholders — `%I` to quote an identifier (table/column name), `%L` to quote a literal \
         value — instead of `%s`, or pass the value as a bound `USING` argument to `EXECUTE` \
         instead of concatenating it into the string. This reflects the migration history in this \
         repository only — confirm the deployed function definition matches before treating this \
         as settled.",
        f.file, f.line,
    )
}

// ── CREATE FUNCTION / PROCEDURE candidate parsing ───────────────────────────────────────
//
// Deliberately duplicated (not shared via `pub(crate)`) from the analogous lexical helpers in
// `sql_parse.rs` and `query_grammar_injection_checker.rs` — this mirrors those two modules'
// own documented convention (see e.g. `weak_randomness_checker`'s precedent, cited in
// `query_grammar_injection_checker`'s module doc) of each lexical checker owning its own
// small parsing primitives rather than threading a shared dependency through the tree.

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Match a sequence of literal (lowercase) keywords against `chars` (expected pre-lowered)
/// starting at `i`, requiring whitespace BETWEEN words and a non-identifier boundary
/// immediately before the first word and after the last. Mirrors `sql_parse::match_kw_seq`.
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

/// Parse one identifier at `i`: a double-quoted identifier (case preserved) or a bareword
/// (folded to lowercase, matching Postgres's own case-folding). Mirrors `sql_parse::parse_ident`.
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

/// Parse a possibly schema-qualified name, defaulting the schema to `public`. Mirrors
/// `sql_parse::parse_qualified_name`.
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

/// Extract the content between a `(` at `open_idx` and its matching `)`, quote-aware
/// (`'`/`"` are opaque) and depth-aware (nested parens don't close early). Returns
/// `(inner_chars, close_idx)`.
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

/// If a dollar-quote begins at or after `from`, return `(after_open_idx, close_start_idx)` —
/// the body's interior span (exclusive of both delimiters). Scans forward past any `$` that
/// doesn't actually open a valid dollar-quote tag (e.g. a stray `$` in a default-value
/// expression).
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
/// ...` statement into a [`CandidateFunction`]. Returns `None` for anything else (including a
/// function whose body isn't dollar-quoted — see the module doc's scope note), never panics.
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

    Some(CandidateFunction {
        schema,
        name,
        body,
        file: file.to_string(),
        line: stmt.line,
    })
}

// ── GRANT EXECUTE ... TO <sensitive role> parsing ───────────────────────────────────────

/// Parse one statement as `GRANT EXECUTE ON {FUNCTION|PROCEDURE} name1[(...)], name2[(...)],
/// ... TO role1, role2, ...` and return every named function/procedure identity, IF the role
/// list contains at least one of [`SENSITIVE_GRANT_ROLES`]. Returns an empty vec for any
/// other statement shape (including a `GRANT EXECUTE` whose roles are all ordinary,
/// non-public application roles — that grant exists, but doesn't drive severity up).
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

/// Whether `word` occurs in `haystack` as a standalone token (non-identifier characters, or
/// the string boundary, on both sides).
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

// ── function-body scan: EXECUTE + non-quoting interpolation ────────────────────────────

/// Strip `--` line comments and `/* */` block comments (non-nesting) from a plpgsql body,
/// treating `'...'` and `"..."` as opaque. Deliberately does NOT handle a nested dollar-quoted
/// string inside the body (e.g. a function that itself builds a `$sql$...$sql$` literal) — see
/// the module doc's scope note; this is a conscious simplification, not an oversight.
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

/// Find the next occurrence of `word` (a standalone token) in `chars`, starting the search at
/// `from`.
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

/// From `from` (just past the `EXECUTE` keyword), find the end of this one dynamic-SQL
/// statement: the next top-level `;` (quote/paren-depth aware), or the end of the body.
/// Returns `(expr_end_exclusive, next_search_from)`.
fn find_statement_end(chars: &[char], from: usize) -> (usize, usize) {
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut i = from;
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
                ';' if depth <= 0 => return (i, i + 1),
                _ => {}
            },
        }
        i += 1;
    }
    (chars.len(), chars.len())
}

/// Whether `s` (already trimmed) is a pure literal: a single-quoted string, `NULL`
/// (case-insensitive), or a bare numeric literal. Mirrors
/// `query_grammar_injection_checker::is_literal_segment`'s discipline, adapted to SQL's own
/// quoting (`'...'`, not `` `...` ``/`"..."`).
fn is_literal_segment(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        return true;
    }
    if s.eq_ignore_ascii_case("null") {
        return true;
    }
    s.chars()
        .enumerate()
        .all(|(i, c)| c.is_ascii_digit() || c == '.' || c == '_' || (i == 0 && c == '-'))
}

/// Split `s` on TOP-LEVEL `||` operators — quote-aware and paren-depth-aware. Mirrors
/// `query_grammar_injection_checker::split_top_level_plus`'s discipline, for SQL's
/// concatenation operator instead of `+`.
fn split_top_level_concat(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut segments = Vec::new();
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
                '|' if depth == 0 && chars.get(i + 1) == Some(&'|') => {
                    segments.push(
                        chars[start..i]
                            .iter()
                            .collect::<String>()
                            .trim()
                            .to_string(),
                    );
                    i += 2;
                    start = i;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    segments.push(chars[start..].iter().collect::<String>().trim().to_string());
    segments
}

/// Split `chars` into top-level comma-separated segments — quote-aware and depth-aware.
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

/// Whether `template` (a `format()` call's first argument, verbatim including its surrounding
/// quotes if any) contains the literal, non-escaping `%s` placeholder. `%L`/`%I` are the
/// ESCAPING placeholders and are deliberately not what this checks for.
fn template_has_percent_s(template: &str) -> bool {
    template.contains("%s")
}

/// Classify one `EXECUTE`'d expression. Returns `Some(mechanism description)` when it is built
/// via a `format()` call using `%s` with a non-literal argument, or a top-level `||`
/// concatenation with a non-literal segment; `None` for every safe shape (escaping-only
/// `format()`, a bound `USING` call, a fully static/literal string).
fn classify_execute_expr(expr: &str) -> Option<&'static str> {
    let chars: Vec<char> = expr.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();

    if let Some(format_kw) = find_word_from(&lower, 0, "format") {
        let paren_idx = skip_ws(&lower, format_kw + "format".chars().count());
        if lower.get(paren_idx) == Some(&'(') {
            if let Some((inner, _close)) = extract_paren_inner(&chars, paren_idx) {
                let args = split_top_level_commas(&inner);
                if let Some(template) = args.first() {
                    if template_has_percent_s(template) {
                        let has_nonliteral_arg = args[1..].iter().any(|a| !is_literal_segment(a));
                        if has_nonliteral_arg {
                            return Some("a `format()` call using the non-escaping `%s` placeholder with a non-literal argument");
                        }
                    }
                }
            }
            // A recognized format() call that isn't unsafe by the %s+non-literal test above —
            // don't ALSO run the `||` check over text that merely surrounds a safe format()
            // call (e.g. `EXECUTE format('%I', col);` has no top-level `||` anyway, but being
            // explicit here keeps the two branches mutually exclusive by construction).
            return None;
        }
    }

    let segments = split_top_level_concat(expr);
    if segments.len() > 1 && segments.iter().any(|s| !is_literal_segment(s)) {
        return Some("string concatenation (`||`)");
    }
    None
}

/// Scan a function body for the FIRST unsafe `EXECUTE` statement (see [`classify_execute_expr`]).
/// Comments are stripped before scanning (see [`strip_sql_comments`]) so a `-- EXECUTE
/// format(...)` mention inside a comment is never mistaken for live code.
fn unsafe_dynamic_exec(body: &str) -> Option<&'static str> {
    let stripped = strip_sql_comments(body);
    let chars: Vec<char> = stripped.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();

    let mut from = 0usize;
    while let Some(kw_idx) = find_word_from(&lower, from, "execute") {
        let expr_start = skip_ws(&chars, kw_idx + "execute".chars().count());
        let (expr_end, next_from) = find_statement_end(&chars, expr_start);
        let expr: String = chars[expr_start..expr_end.max(expr_start)].iter().collect();
        if let Some(hit) = classify_execute_expr(&expr) {
            return Some(hit);
        }
        from = next_from.max(kw_idx + 1);
    }
    None
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
            .filter(|v| v.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
            .collect()
    }

    // ── positives ────────────────────────────────────────────────────────────────────

    #[test]
    fn format_percent_s_of_a_parameter_with_anon_grant_is_critical() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.run_report(p text) returns void as $$ \
             begin execute format('select * from reports where name = %s', p); end; \
             $$ language plpgsql;\n\
             grant execute on function public.run_report(text) to anon;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_CRITICAL);
        assert_eq!(vs[0].object.as_deref(), Some("public.run_report"));
        assert!(vs[0].message.contains("anon"), "{}", vs[0].message);
    }

    #[test]
    fn format_percent_s_of_a_parameter_without_grant_is_high() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.run_report(p text) returns void as $$ \
             begin execute format('select * from reports where name = %s', p); end; \
             $$ language plpgsql;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn double_pipe_concatenation_of_a_parameter_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.search_users(p text) returns void as $$ \
             begin execute 'select * from users where name = ' || p; end; \
             $$ language plpgsql;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn create_or_replace_function_dialect_variant_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create or replace function public.search_users(p text) returns void as $$ \
             begin execute 'select * from users where name = ' || p; end; \
             $$ language plpgsql;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn clauses_after_the_dollar_quoted_body_dialect_variant_still_fires() {
        // Signature options (LANGUAGE, SECURITY DEFINER, ...) can legally appear AFTER the
        // dollar-quoted body in Postgres — the body-boundary parser must still find the body.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.search_users(p text) returns void as $$ \
             begin execute 'select * from users where name = ' || p; end; \
             $$ language plpgsql security definer set search_path = public, pg_temp;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn stored_procedure_variant_fires() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create procedure public.search_users(p text) as $$ \
             begin execute 'select * from users where name = ' || p; end; \
             $$ language plpgsql;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    // ── safe twins: same-file discrimination ────────────────────────────────────────

    #[test]
    fn percent_l_placeholder_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.search_users(p text) returns void as $$ \
             begin execute format('select * from users where name = %L', p); end; \
             $$ language plpgsql;\n",
        )]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn percent_i_placeholder_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.sort_table(col text) returns void as $$ \
             begin execute format('select * from t order by %I', col); end; \
             $$ language plpgsql;\n",
        )]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn execute_using_bound_argument_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.search_users(p text) returns void as $$ \
             begin execute 'select * from users where name = $1' using p; end; \
             $$ language plpgsql;\n",
        )]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn fully_static_query_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.cleanup() returns void as $$ \
             begin execute 'delete from sessions where expired = true'; end; \
             $$ language plpgsql;\n",
        )]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn all_five_shapes_in_one_file_discriminate_correctly() {
        // One file, one unsafe function, four safe twins — proves the checker isn't simply
        // flagging every function containing the word EXECUTE.
        let f = files(vec![(
            "supabase/migrations/0001_fns.sql",
            "create function public.unsafe_fn(p text) returns void as $$ \
             begin execute 'select * from t where x = ' || p; end; \
             $$ language plpgsql;\n\
             create function public.safe_l(p text) returns void as $$ \
             begin execute format('select * from t where x = %L', p); end; \
             $$ language plpgsql;\n\
             create function public.safe_i(p text) returns void as $$ \
             begin execute format('select * from t order by %I', p); end; \
             $$ language plpgsql;\n\
             create function public.safe_using(p text) returns void as $$ \
             begin execute 'select * from t where x = $1' using p; end; \
             $$ language plpgsql;\n\
             create function public.safe_static() returns void as $$ \
             begin execute 'select 1'; end; \
             $$ language plpgsql;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(
            vs.len(),
            1,
            "expected exactly the one unsafe function: {vs:#?}"
        );
        assert_eq!(vs[0].object.as_deref(), Some("public.unsafe_fn"));
    }

    #[test]
    fn concatenation_of_only_literal_segments_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.f() returns void as $$ \
             begin execute 'select ' || '* from t'; end; \
             $$ language plpgsql;\n",
        )]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn non_security_definer_function_is_still_in_scope() {
        // This rule is orthogonal to SECURITY DEFINER/INVOKER — a plain invoker-rights
        // function that builds unsafe dynamic SQL is still exploitable by whatever role can
        // call it at all.
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.search_users(p text) returns void as $$ \
             begin execute 'select * from users where name = ' || p; end; \
             $$ language plpgsql;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    // ── GRANT correlation across files ──────────────────────────────────────────────

    #[test]
    fn grant_in_a_different_file_in_the_same_scan_still_drives_critical() {
        let f = files(vec![
            (
                "supabase/migrations/0001_fn.sql",
                "create function public.run_report(p text) returns void as $$ \
                 begin execute format('select * from reports where name = %s', p); end; \
                 $$ language plpgsql;\n",
            ),
            (
                "supabase/migrations/0002_grants.sql",
                "grant execute on function public.run_report(text) to authenticated;\n",
            ),
        ]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_CRITICAL);
    }

    #[test]
    fn grant_to_an_ordinary_non_public_role_does_not_drive_critical() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.run_report(p text) returns void as $$ \
             begin execute format('select * from reports where name = %s', p); end; \
             $$ language plpgsql;\n\
             grant execute on function public.run_report(text) to service_role_internal;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].severity, SEVERITY_HIGH,
            "a grant to an ordinary non-public role must not escalate severity: {vs:#?}"
        );
    }

    #[test]
    fn grant_on_an_unrelated_function_does_not_drive_this_ones_severity() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.run_report(p text) returns void as $$ \
             begin execute format('select * from reports where name = %s', p); end; \
             $$ language plpgsql;\n\
             create function public.other_fn() returns void as $$ begin null; end; $$ language plpgsql;\n\
             grant execute on function public.other_fn() to anon;\n",
        )]);
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].severity, SEVERITY_HIGH,
            "the grant targets a DIFFERENT function and must not leak severity: {vs:#?}"
        );
    }

    // ── scoping / hygiene ────────────────────────────────────────────────────────────

    #[test]
    fn no_supabase_files_never_applies() {
        let f = files(vec![("README.md", "hello")]);
        assert!(!crate::arch_checker::checker_applies(
            &DynamicSqlExecInjectionChecker,
            &f
        ));
    }

    #[test]
    fn plain_postgres_layout_outside_supabase_folder_is_inspected() {
        let f = files(vec![(
            "db/migrations/0001_fn.sql",
            "create function public.search_users(p text) returns void as $$ \
             begin execute 'select * from users where name = ' || p; end; \
             $$ language plpgsql;\n",
        )]);
        assert!(crate::arch_checker::checker_applies(
            &DynamicSqlExecInjectionChecker,
            &f
        ));
        let violations = DynamicSqlExecInjectionChecker.check(&view(&f));
        let vs = rule_hits(&violations);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn execute_mentioned_only_inside_a_comment_is_not_flagged() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.f(p text) returns void as $$ \
             begin \
             -- execute format('select %s', p); \
             null; end; \
             $$ language plpgsql;\n",
        )]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn empty_file_does_not_panic() {
        let f = files(vec![("a.sql", "")]);
        assert!(rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn unterminated_function_body_does_not_panic() {
        let f = files(vec![(
            "a.sql",
            "create function public.f(p text) returns void as $$ begin execute 'x' || p",
        )]);
        let _ = DynamicSqlExecInjectionChecker.check(&view(&f)); // must not panic
    }

    #[test]
    fn create_or_replace_supersedes_prior_definition_last_write_wins() {
        let f = files(vec![(
            "supabase/migrations/0001_fn.sql",
            "create function public.f(p text) returns void as $$ \
             begin execute 'x' || p; end; \
             $$ language plpgsql;\n\
             create or replace function public.f(p text) returns void as $$ \
             begin execute format('select %L', p); end; \
             $$ language plpgsql;\n",
        )]);
        assert!(
            rule_hits(&DynamicSqlExecInjectionChecker.check(&view(&f))).is_empty(),
            "the LATER (safe) definition must win, mirroring timeline's CREATE OR REPLACE semantics"
        );
    }
}
