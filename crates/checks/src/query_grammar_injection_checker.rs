//! `QueryGrammarInjectionChecker`: LEXICAL (comment-aware, not a bare regex) detection of
//! `SEC-NO-QUERY-GRAMMAR-INJECTION-1` — user-controlled data CONCATENATED or INTERPOLATED
//! into a query/filter/expression GRAMMAR string, rather than passed as a bound argument. See
//! `docs/plans/2026-09-29_codebase-inspection-hardening.md` D3 and
//! `crates/rules/principles/universal/sec-no-query-grammar-injection-1.toml` (P3 already
//! grounded this rule id with CWE-943/74 + the PostgREST operators docs — this checker emits
//! under that SAME id rather than inventing a new one, exactly as D1/D2 reuse their own
//! P3-grounded ids).
//!
//! # Why this is an injection class, not "validate the param" hygiene
//!
//! A PostgREST `.or()`/`.filter()` string, a raw SQL query, a MongoDB `$where` clause, and an
//! LDAP filter string each have their OWN grammar — commas, parentheses, operator keywords —
//! distinct from ordinary data. Concatenating or interpolating user input into that grammar
//! lets an attacker inject additional clauses (an extra comma or closing paren reopens the
//! expression), exactly as string-built SQL does, even when every underlying call is otherwise
//! fully parameterized. **This is not contained by Row Level Security**: RLS restricts which
//! ROWS a correctly-formed query can see, but a grammar injection can forge a DIFFERENT query
//! entirely before RLS is ever evaluated — so "RLS probably contains it" is never a reason to
//! downgrade or hold this finding for review. Because this checker is deterministic (a floor
//! detector, not an LLM verdict), its findings never enter the AI calibration/citation-gate
//! pipeline in the first place (`report_export::is_ai_tier` keys on `confidence.is_some()` or
//! an `AI-` rule-id prefix, neither of which this checker's output carries) — see the module
//! doc on `weak_randomness_checker` for the identical D2 precedent, and the scan-path e2e test
//! in `crates/server/tests/query_grammar_injection_executor_e2e.rs` for the regression proof
//! that a co-present, CLEAN Row Level Security setup does not move this finding's severity or
//! bucket.
//!
//! # The four grammar families this checker recognizes
//!
//! - **PostgREST** `.or(...)` / `.or_(...)` (the Python client's underscore-suffixed form,
//!   since `or` is a reserved word there) / `.filter(...)` with a SINGLE string argument built
//!   via template-literal interpolation (`` `id.eq.${x}` ``) or `+` concatenation.
//! - **Raw SQL** issued via `.query(...)` / `.execute(...)` / `.raw(...)` whose first argument
//!   is a SQL string (starts with `select`/`insert`/`update`/`delete`) built the same way.
//! - **MongoDB `$where`**: an object property `$where: <expr>` whose value is interpolated or
//!   concatenated.
//! - **LDAP filters**: a `filter:` property passed to a `.search(...)` call whose value looks
//!   like an LDAP filter (`(attr=value)`) and is interpolated or concatenated.
//!
//! # The safe twin — bound arguments are never flagged
//!
//! `.eq(column, value)` and an `.ilike(column, pattern)` with a bound parameter are never
//! matched at all (this checker doesn't scan those method names — passing the value as a bound
//! argument the client library escapes is exactly the correct pattern). A `.filter(column, op,
//! value)` bound 3-argument form IS scanned (it shares a method name with the string-grammar
//! form) but is spared because it has MORE THAN ONE top-level argument — see
//! [`classify_call_args`]. A `.or()`/`.filter()` call whose single string argument is fully
//! STATIC (no `${...}`, no `+` concatenation) is also spared — a hand-written static filter
//! expression carries no user input at all.
//!
//! # What this deliberately does NOT do
//!
//! No string-literal awareness beyond simple quote-toggling, no cross-file identifier
//! tracking, and no attempt to prove the concatenated/interpolated variable is ACTUALLY
//! request-derived — ANY non-literal operand flowing into a query grammar string is flagged,
//! because the defect is the GRAMMAR being built from an expression at all, not the specific
//! provenance of that expression (a variable that looks internal today can become
//! request-derived after the next refactor, and the fix — bind, don't concatenate — is the
//! same either way). This mirrors `weak_randomness_checker`'s documented discipline: a false
//! negative on multi-line call arguments or cross-file expressions is explicitly preferred over
//! a false positive from a more aggressive parser.

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};

pub const RULE_QUERY_GRAMMAR_INJECTION: &str = "SEC-NO-QUERY-GRAMMAR-INJECTION-1";

const RULE_IDS: &[&str] = &[RULE_QUERY_GRAMMAR_INJECTION];

const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.py",
    "**/*.go",
    "**/*.rb",
    "**/*.java",
    "**/*.cs",
];

const SKIP_PATH_FRAGMENTS: &[&str] = &[
    "node_modules/",
    "/dist/",
    "/build/",
    "/.next/",
    ".min.js",
    "vendor/",
];

/// PostgREST-style grammar-building calls: the client's own `.or()`/`.filter()` string DSL.
/// `.or_(` is the Python `supabase-py` client's spelling (`or` is a reserved word there).
const POSTGREST_NEEDLES: &[&str] = &[".or(", ".or_(", ".filter("];

/// Calls that issue a raw query string as their first argument.
const RAW_QUERY_NEEDLES: &[&str] = &[".query(", ".execute(", ".raw("];

const SQL_KEYWORDS: &[&str] = &["select", "insert", "update", "delete"];

pub struct QueryGrammarInjectionChecker;

impl ArchChecker for QueryGrammarInjectionChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        repo.files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .filter(|(path, _)| !SKIP_PATH_FRAGMENTS.iter().any(|frag| path.contains(frag)))
            .flat_map(|(path, content)| violations_in_file(path, content))
            .collect()
    }
}

/// Mirrors `weak_randomness_checker::is_hash_comment_language` exactly (duplicated per this
/// repo's existing convention of each lexical checker owning its own comment-stripping —
/// see that module's doc comment on why it isn't shared).
fn is_hash_comment_language(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".rb")
}

/// Byte-identical port of `weak_randomness_checker::strip_comments` — see that function's doc
/// comment for the full rationale (duplicated rather than shared: it is private to its own
/// module, and this checker needs the exact same `//`/`/* */`/`#` discipline).
fn strip_comments(line: &str, mut in_block_comment: bool, hash_comments: bool) -> (String, bool) {
    if hash_comments {
        let code = match line.find('#') {
            Some(pos) => &line[..pos],
            None => line,
        };
        return (code.to_string(), false);
    }

    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if in_block_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            break;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, in_block_comment)
}

/// How a candidate grammar-string argument/value was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExprShape {
    /// A fully literal expression (a static string, a static `+`-join of literals, a plain
    /// number) — carries no runtime-determined content at all.
    Static,
    /// A template literal containing at least one `${...}` interpolation.
    Interpolated,
    /// A `+`-joined chain where at least one segment is NOT a literal (a variable, a call, a
    /// property access, ...).
    ConcatenatedWithVariable,
    /// Not a string-shaped expression at all (an identifier alone, a callback/arrow function,
    /// an object literal, ...) — the common case for `Array.prototype.filter`/`.find`.
    NotAString,
}

/// Whether `s` (already trimmed of surrounding whitespace) is a single literal: a fully
/// quoted/backtick string with no embedded unescaped matching quote, or a bare numeric
/// literal. Used to tell `'a' + 'b'` (static) apart from `'a' + userId` (a real
/// concatenation).
fn is_literal_segment(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let quoted = |q: char| s.starts_with(q) && s.ends_with(q) && s.len() >= 2;
    if quoted('\'') || quoted('"') || quoted('`') {
        return true;
    }
    // A bare numeric literal: digits, an optional single leading '-', '.', or '_' separators.
    s.chars()
        .enumerate()
        .all(|(i, c)| c.is_ascii_digit() || c == '.' || c == '_' || (i == 0 && c == '-'))
}

/// Split `e` on TOP-LEVEL `+` operators — quote-aware (a `+` inside a string literal, including
/// inside a template literal's `${...}` interpolation, is opaque and never a split point) and
/// depth-aware (a `+` inside a nested call/array/object argument is not top-level either).
/// Always returns at least one segment (the whole trimmed input) when no top-level `+` exists.
fn split_top_level_plus(e: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;

    for (idx, ch) in e.char_indices() {
        if escaped {
            escaped = false;
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
                '\'' | '"' | '`' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                '+' if depth == 0 => {
                    segments.push(e[start..idx].trim().to_string());
                    start = idx + ch.len_utf8();
                }
                _ => {}
            },
        }
    }
    segments.push(e[start..].trim().to_string());
    segments
}

/// Classify a single candidate expression (already isolated as one call argument, or one
/// object-property value) into an [`ExprShape`].
fn classify_expr(expr: &str) -> ExprShape {
    let e = expr.trim();
    if e.is_empty() {
        return ExprShape::NotAString;
    }
    // A template literal spanning the whole expression — the common PostgREST/SQL shape.
    if e.starts_with('`') && e.ends_with('`') && e.len() >= 2 {
        return if e.contains("${") {
            ExprShape::Interpolated
        } else {
            ExprShape::Static
        };
    }
    let segments = split_top_level_plus(e);
    if segments.len() > 1 {
        return if segments.iter().all(|s| is_literal_segment(s)) {
            ExprShape::Static
        } else {
            ExprShape::ConcatenatedWithVariable
        };
    }
    if is_literal_segment(e) {
        ExprShape::Static
    } else {
        ExprShape::NotAString
    }
}

/// Extract the raw content between a call's opening `(` (at byte offset `open_paren_byte` in
/// `code`, which MUST point at a `(` character) and its matching closing `)`, treating
/// `'`/`"`/`` ` `` as OPAQUE string delimiters (everything between a matching pair, including
/// any nested parens from a `${...}` interpolation, is ignored for depth-counting purposes —
/// the true call boundary is always OUTSIDE the string). Returns `(inner_content,
/// close_paren_byte)`, or `None` if the call is unterminated on this line (a bounded, per-line
/// lexical checker never scans across lines for this — see the module doc).
fn extract_call_inner(code: &str, open_paren_byte: usize) -> Option<(String, usize)> {
    if code.as_bytes().get(open_paren_byte) != Some(&b'(') {
        return None;
    }
    let mut depth: i32 = 1;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut end: Option<usize> = None;

    for (idx, ch) in code.char_indices() {
        if idx <= open_paren_byte {
            continue;
        }
        if escaped {
            escaped = false;
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
                '\'' | '"' | '`' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(idx);
                        break;
                    }
                }
                _ => {}
            },
        }
    }

    end.map(|e| (code[open_paren_byte + 1..e].to_string(), e))
}

/// Split a call's inner content into TOP-LEVEL comma-separated arguments — quote-aware (a comma
/// inside a string is never a split point) and depth-aware (a comma inside a nested
/// call/array/object argument is not top-level). Returns an empty vec for an empty (whitespace-
/// only) call.
fn split_top_level_args(inner: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;

    for (idx, ch) in inner.char_indices() {
        if escaped {
            escaped = false;
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
                '\'' | '"' | '`' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    args.push(inner[start..idx].trim().to_string());
                    start = idx + 1;
                }
                _ => {}
            },
        }
    }
    let tail = inner[start..].trim().to_string();
    if !tail.is_empty() || !args.is_empty() {
        args.push(tail);
    }
    args
}

/// Find the first occurrence (lowest byte offset, searching from `from`) of any needle in
/// `needles`, returning the matched needle and its byte offset.
fn find_first_needle<'a>(code: &str, needles: &[&'a str], from: usize) -> Option<(&'a str, usize)> {
    if from > code.len() {
        return None;
    }
    let mut best: Option<(&str, usize)> = None;
    for &needle in needles {
        if let Some(rel) = code[from..].find(needle) {
            let pos = from + rel;
            if best.map(|(_, bp)| pos < bp).unwrap_or(true) {
                best = Some((needle, pos));
            }
        }
    }
    best
}

/// One detected grammar-injection call site on a single line.
struct Hit {
    /// The API surface named in the message (`".or("`, `".query("`, `"$where"`, `"filter"`).
    api: &'static str,
    /// The offending expression, verbatim (for the `object` field and the detail text).
    expr: String,
    /// How the expression was built — feeds the message's mechanism clause.
    mechanism: &'static str,
}

fn mechanism_for(shape: ExprShape) -> &'static str {
    match shape {
        ExprShape::Interpolated => "template-literal interpolation (`${...}`)",
        ExprShape::ConcatenatedWithVariable => "string concatenation (`+`)",
        _ => "an unbound expression",
    }
}

/// PostgREST `.or()`/`.or_()`/`.filter()` with a SINGLE string argument built via interpolation
/// or concatenation. A call with more than one top-level argument is the BOUND form
/// (`.filter(column, operator, value)`) and is never flagged, regardless of content.
fn postgrest_violation(code: &str) -> Option<Hit> {
    let mut search_from = 0usize;
    loop {
        let (needle, pos) = find_first_needle(code, POSTGREST_NEEDLES, search_from)?;
        let open_idx = pos + needle.len() - 1;
        match extract_call_inner(code, open_idx) {
            Some((inner, close_idx)) => {
                let args = split_top_level_args(&inner);
                if args.len() == 1 {
                    let shape = classify_expr(&args[0]);
                    if matches!(
                        shape,
                        ExprShape::Interpolated | ExprShape::ConcatenatedWithVariable
                    ) {
                        return Some(Hit {
                            api: needle,
                            expr: args[0].clone(),
                            mechanism: mechanism_for(shape),
                        });
                    }
                }
                search_from = close_idx + 1;
            }
            None => search_from = pos + needle.len(),
        }
    }
}

fn starts_with_sql_keyword(expr: &str) -> bool {
    let trimmed = expr
        .trim()
        .trim_start_matches(['`', '\'', '"'])
        .trim_start();
    let lower = trimmed.to_ascii_lowercase();
    SQL_KEYWORDS
        .iter()
        .any(|k| lower.starts_with(&format!("{k} ")))
}

/// A raw SQL string issued via `.query(`/`.execute(`/`.raw(` whose FIRST argument is a SQL
/// string built via interpolation or concatenation. A parameterized call
/// (`.query("SELECT ... WHERE id = $1", [id])`) has no `${...}`/`+`-with-a-variable in its
/// first argument at all, so `classify_expr` reports it `Static` and it is spared.
fn raw_sql_violation(code: &str) -> Option<Hit> {
    let mut search_from = 0usize;
    loop {
        let (needle, pos) = find_first_needle(code, RAW_QUERY_NEEDLES, search_from)?;
        let open_idx = pos + needle.len() - 1;
        match extract_call_inner(code, open_idx) {
            Some((inner, close_idx)) => {
                let args = split_top_level_args(&inner);
                if let Some(first) = args.first() {
                    let shape = classify_expr(first);
                    if matches!(
                        shape,
                        ExprShape::Interpolated | ExprShape::ConcatenatedWithVariable
                    ) && starts_with_sql_keyword(first)
                    {
                        return Some(Hit {
                            api: needle,
                            expr: first.clone(),
                            mechanism: mechanism_for(shape),
                        });
                    }
                }
                search_from = close_idx + 1;
            }
            None => search_from = pos + needle.len(),
        }
    }
}

/// Extract the value expression starting at `s` (already positioned right after a property's
/// `:`), stopping at the first TOP-LEVEL `,`, `}`, or `)` — quote- and depth-aware, mirroring
/// [`split_top_level_args`]'s discipline for a single value rather than a full argument list.
fn extract_property_value(s: &str) -> String {
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for (idx, ch) in s.char_indices() {
        if escaped {
            escaped = false;
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
                '\'' | '"' | '`' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => return s[..idx].trim().to_string(),
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => return s[..idx].trim().to_string(),
                _ => {}
            },
        }
    }
    s.trim().to_string()
}

/// A MongoDB `$where` clause (a raw JS-expression escape hatch in the Mongo query language)
/// whose value is interpolated or concatenated. Scans the whole line for the `$where:` literal
/// rather than requiring a specific enclosing call name — `$where` is a rare, unambiguous
/// token that only ever appears as this Mongo property key.
fn mongo_where_violation(code: &str) -> Option<Hit> {
    let idx = code.find("$where")?;
    let rest = &code[idx + "$where".len()..];
    let colon_rel = rest.find(':')?;
    let value = extract_property_value(rest[colon_rel + 1..].trim_start());
    let shape = classify_expr(&value);
    if matches!(
        shape,
        ExprShape::Interpolated | ExprShape::ConcatenatedWithVariable
    ) {
        Some(Hit {
            api: "$where",
            expr: value,
            mechanism: mechanism_for(shape),
        })
    } else {
        None
    }
}

/// A loose LDAP-filter shape check: a parenthesized `(attr=value)` expression — the LDAP filter
/// grammar's own delimiter. Checked against the value's literal content with its own quoting
/// stripped, since interpolation/concatenation is expected inside.
fn looks_like_ldap_filter(expr: &str) -> bool {
    let inner = expr.trim().trim_matches(['`', '\'', '"']);
    let inner = inner.trim();
    inner.starts_with('(') && inner.contains('=')
}

/// An LDAP `filter:` property passed to a `.search(...)` call, built via interpolation or
/// concatenation, whose value looks like an LDAP filter expression. Gated on `.search(`
/// appearing on the same line so the common English word "filter" doesn't fire on unrelated
/// object literals.
fn ldap_filter_violation(code: &str) -> Option<Hit> {
    if !code.contains(".search(") {
        return None;
    }
    let idx = code.find("filter:").or_else(|| code.find("filter :"))?;
    let after = &code[idx..];
    let colon_rel = after.find(':')?;
    let value = extract_property_value(after[colon_rel + 1..].trim_start());
    let shape = classify_expr(&value);
    if matches!(
        shape,
        ExprShape::Interpolated | ExprShape::ConcatenatedWithVariable
    ) && looks_like_ldap_filter(&value)
    {
        Some(Hit {
            api: "filter",
            expr: value,
            mechanism: mechanism_for(shape),
        })
    } else {
        None
    }
}

fn message_for(hit: &Hit) -> String {
    let api = hit.api.trim_end_matches('(');
    format!(
        "`{api}` is built via {mechanism} rather than a bound argument, so user-controlled data \
         can inject additional clauses into the query/filter GRAMMAR itself — an extra comma or \
         closing paren reopens the expression to add arbitrary conditions or columns, exactly as \
         string-concatenated SQL does. This is an injection class (CWE-943/CWE-74), not an \
         input-validation nicety, and it is NOT contained by Row Level Security: RLS restricts \
         which rows a correctly-formed query can see, but a grammar injection forges a DIFFERENT \
         query before RLS is ever evaluated. Rewrite this using the client library's bound-\
         argument methods (e.g. `.eq(column, value)` chains instead of a string-built `.or()`/\
         `.filter()`, a parameterized query instead of an interpolated SQL string) or escape the \
         grammar's reserved characters if a literal string is unavoidable. Offending expression: \
         `{}`.",
        hit.expr,
        mechanism = hit.mechanism,
    )
}

fn violations_in_file(path: &str, content: &str) -> Vec<ArchViolation> {
    let hash_comments = is_hash_comment_language(path);
    let lines: Vec<&str> = content.lines().collect();
    let mut violations = Vec::new();
    let mut in_block_comment = false;

    for (idx, raw_line) in lines.iter().enumerate() {
        let (code, next_in_block) = strip_comments(raw_line, in_block_comment, hash_comments);
        in_block_comment = next_in_block;

        let hits = [
            postgrest_violation(&code),
            raw_sql_violation(&code),
            mongo_where_violation(&code),
            ldap_filter_violation(&code),
        ];
        for hit in hits.into_iter().flatten() {
            violations.push(ArchViolation {
                rule_id: RULE_QUERY_GRAMMAR_INJECTION.to_string(),
                file: path.to_string(),
                line: idx + 1,
                object: Some(hit.expr.clone()),
                severity: SEVERITY_HIGH,
                message: message_for(&hit),
            });
        }
    }

    violations
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
            .filter(|v| v.rule_id == RULE_QUERY_GRAMMAR_INJECTION)
            .collect()
    }

    // ── positives: PostgREST ────────────────────────────────────────────────

    #[test]
    fn flags_or_with_template_interpolation_as_high() {
        let f = files(vec![(
            "src/api/orders.ts",
            "const { data } = await supabase.from('orders').or(`user_id.eq.${req.query.userId},status.eq.${req.query.status}`);\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].severity, SEVERITY_HIGH,
            "not medium/needs-review: {vs:#?}"
        );
        assert_eq!(vs[0].line, 1);
        assert!(vs[0].message.contains("Row Level Security"));
    }

    #[test]
    fn flags_python_or_underscore_with_concatenation_as_high() {
        let f = files(vec![(
            "app/orders.py",
            "query = query.or_('status.eq.active,user_id.eq.' + user_id)\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn flags_filter_with_single_interpolated_argument_as_high() {
        let f = files(vec![(
            "src/api/search.ts",
            "const rows = await table.filter(`name.ilike.*${term}*`);\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    // ── positives: raw SQL / Mongo / LDAP ──────────────────────────────────

    #[test]
    fn flags_raw_sql_template_interpolation_as_high() {
        let f = files(vec![(
            "src/db/orders.ts",
            "const rows = await db.query(`SELECT * FROM orders WHERE id = ${orderId}`);\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn flags_raw_sql_concatenation_as_high() {
        let f = files(vec![(
            "src/db/orders.ts",
            "await connection.execute('SELECT * FROM orders WHERE id = ' + orderId);\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn flags_mongo_where_interpolation_as_high() {
        let f = files(vec![(
            "src/db/users.js",
            "db.users.find({ $where: `this.name == '${username}'` });\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn flags_ldap_filter_interpolation_as_high() {
        let f = files(vec![(
            "src/directory/lookup.js",
            "client.search(base, { filter: `(uid=${username})` }, cb);\n",
        )]);
        let hits = QueryGrammarInjectionChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    // ── safe twins: bound arguments ─────────────────────────────────────────

    #[test]
    fn eq_bound_argument_is_never_flagged() {
        // `.eq(` isn't even a scanned method name — a bound value is always safe.
        let f = files(vec![(
            "src/api/orders.ts",
            "const rows = await table.eq('status', status);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ilike_bound_argument_is_never_flagged() {
        let f = files(vec![(
            "src/api/search.ts",
            "const rows = await table.ilike('name', pattern);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn filter_bound_three_argument_form_is_never_flagged() {
        let f = files(vec![(
            "src/api/orders.ts",
            "const rows = await table.filter('status', 'eq', status);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn or_with_a_static_string_is_never_flagged() {
        let f = files(vec![(
            "src/api/orders.ts",
            "const rows = await table.or('status.eq.active,status.eq.pending');\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn or_with_a_static_template_literal_is_never_flagged() {
        let f = files(vec![(
            "src/api/orders.ts",
            "const rows = await table.or(`status.eq.active,status.eq.pending`);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn parameterized_sql_with_numbered_placeholder_is_never_flagged() {
        let f = files(vec![(
            "src/db/orders.ts",
            "const rows = await db.query('SELECT * FROM orders WHERE id = $1', [orderId]);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn parameterized_sql_via_template_literal_with_no_interpolation_is_never_flagged() {
        let f = files(vec![(
            "src/db/orders.ts",
            "const rows = await db.query(`SELECT * FROM orders WHERE id = $1`, [orderId]);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn array_prototype_filter_callback_is_never_flagged() {
        let f = files(vec![(
            "src/lib/util.ts",
            "const active = items.filter(x => x.status === 'active');\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn array_prototype_find_is_not_scanned_at_all() {
        let f = files(vec![(
            "src/lib/util.ts",
            "const item = items.find(x => x.id === id);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ldap_search_with_a_static_filter_is_never_flagged() {
        let f = files(vec![(
            "src/directory/lookup.js",
            "client.search(base, { filter: '(uid=service-account)' }, cb);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn mongo_find_without_where_is_never_flagged() {
        let f = files(vec![(
            "src/db/users.js",
            "db.users.find({ status: `${status}` });\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    // ── scoping / hygiene ────────────────────────────────────────────────────

    #[test]
    fn non_matching_extension_is_not_scoped_in() {
        let f = files(vec![(
            "README.md",
            "table.or(`user_id.eq.${req.query.id}`)\n",
        )]);
        assert!(!crate::arch_checker::checker_applies(
            &QueryGrammarInjectionChecker,
            &f
        ));
    }

    #[test]
    fn vendor_and_build_output_paths_are_skipped() {
        let f = files(vec![
            (
                "node_modules/pkg/index.js",
                "table.or(`user_id.eq.${req.query.id}`)\n",
            ),
            (
                "apps/ui/dist/bundle.js",
                "table.or(`user_id.eq.${req.query.id}`)\n",
            ),
        ]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ignores_a_line_comment_occurrence() {
        let f = files(vec![(
            "a.ts",
            "// table.or(`user_id.eq.${req.query.id}`); is banned\nconst x = 1;\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ignores_a_block_comment_occurrence() {
        let f = files(vec![(
            "a.ts",
            "/* table.or(`user_id.eq.${req.query.id}`); */\nconst x = 1;\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ignores_a_python_hash_comment_occurrence() {
        let f = files(vec![(
            "app.py",
            "# query.or_('status.eq.' + status)\nx = 1\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn empty_file_does_not_panic() {
        let f = files(vec![("a.ts", "")]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn unterminated_call_does_not_panic() {
        let f = files(vec![("a.ts", "table.or(`user_id.eq.${req.query.id}\n")]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn non_ascii_content_does_not_panic() {
        let f = files(vec![(
            "a.ts",
            "// 日本語のコメント\nconst rows = await table.eq('status', status);\n",
        )]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn empty_or_call_does_not_panic_or_flag() {
        let f = files(vec![("a.ts", "table.or();\n")]);
        assert!(rule_hits(&QueryGrammarInjectionChecker.check(&view(&f))).is_empty());
    }
}
