//! Architectural (AST-tier) governance checks.
//!
//! The `Architectural` enforcement tier (see
//! `crates/rules/src/lib.rs::EnforcementKind` and
//! `docs/decisions/2026-06-19_ast_architectural_rule_tier.md`) covers rules that
//! are *deterministically* checkable but require reasoning over code STRUCTURE,
//! not text patterns: "a handler does not touch the DB directly", "a service does
//! not bypass the repository", "no cross-boundary imports". No regex can express
//! these reliably (a regex cannot tell whether `db.query(...)` sits inside a
//! handler function or a comment three scopes over), and no LLM is needed to
//! judge them (the answer is a hard yes/no over the parse tree).
//!
//! # Production shape (proposed, routed)
//!
//! The intended production design is an `ArchitecturalCheck` trait evaluated
//! over a parsed module. For Rust that parse is `syn::File`; a language-agnostic
//! layer would sit behind the same trait per language. Introducing the `syn`
//! dependency and the cross-crate trait surface is a STRUCTURAL change and is
//! therefore ROUTED to the architect in the design doc, not auto-applied here.
//!
//! # What this module ships
//!
//! A minimal, self-contained PROOF of the tier: one architectural rule
//! ([`handler_no_direct_db`]) implemented as a pure function over a Rust source
//! string. It does genuine structural reasoning — it tracks function boundaries
//! and brace depth so a DB call is only flagged when it lexically sits inside a
//! handler function body — rather than a flat regex match. It deliberately avoids
//! pulling `syn` in until the trait/dependency decision is made; the limitations
//! of the lexical approach (documented on [`handler_no_direct_db`]) are exactly
//! the argument for the `syn`-backed production version.

/// One architectural violation found by a checker: which rule, which function,
/// the 1-based line of the offending call, and a human explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchViolation {
    /// The rule id this violation maps to (e.g. `ARCH-HANDLER-NO-DB-1`).
    pub rule_id: String,
    /// The name of the function the violation was found in (best-effort).
    pub function: String,
    /// The 1-based source line of the offending call.
    pub line: usize,
    /// A human-readable explanation of what was found and why it violates.
    pub message: String,
}

/// The rule id the [`handler_no_direct_db`] checker reports under. Matches the
/// corpus TOML in `crates/rules/principles/api-layer/arch-handler-no-db-1.toml`.
pub const HANDLER_NO_DIRECT_DB_RULE_ID: &str = "ARCH-HANDLER-NO-DB-1";

/// Default substrings that identify a "handler" / "controller" function by name.
/// A function whose name contains any of these (case-insensitive) is treated as a
/// request handler whose body must not touch the database directly.
const DEFAULT_HANDLER_MARKERS: &[&str] = &["handler", "handle_", "controller", "_route", "endpoint"];

/// Default identifiers that name a direct database handle. A call of the form
/// `<marker>.<method>(...)` inside a handler body is a direct DB access.
const DEFAULT_DB_HANDLE_MARKERS: &[&str] = &["db", "pool", "conn", "tx", "executor"];

/// Detect handler/controller functions that touch a database handle DIRECTLY,
/// in a single Rust source string.
///
/// # What "direct DB access" means here
///
/// A method call `receiver.method(...)` where `receiver` is one of the
/// configured DB-handle identifiers (`db`, `pool`, `conn`, ...), occurring inside
/// the body of a function whose name marks it a handler (`*handler*`,
/// `*controller*`, ...). The proper layering (ARCH-STRICT-LAYERING / repo-per-
/// aggregate) is that handlers call services, services call repositories, and
/// only repositories hold a DB handle.
///
/// # Why this is structural, not a regex
///
/// The checker tracks function boundaries and brace depth: a `db.query(...)` in a
/// `repository` function, a free function, or a comment is NOT flagged — only one
/// lexically nested inside a handler function body is. A flat regex
/// (`db\.\w+\(`) cannot make that distinction.
///
/// # Known limitations (the argument for the `syn` production version)
///
/// This is a lexical approximation, not a real parse:
/// - It does not strip string/char literals or `//`-... comments, so a DB-handle
///   token inside a string could in principle be miscounted (mitigated: we only
///   match the `<ident>.<ident>(` call shape, which is rare in prose strings).
/// - It identifies handlers by NAME, not by attribute/route-macro or trait impl.
/// - It does not resolve types, so it cannot distinguish a `db` field of an
///   unrelated struct from a real DB handle.
///
/// The `syn`-backed `ArchitecturalCheck` production design (routed in the
/// design doc) removes all three by reasoning over the real AST.
pub fn handler_no_direct_db(source: &str) -> Vec<ArchViolation> {
    handler_no_direct_db_with(source, DEFAULT_HANDLER_MARKERS, DEFAULT_DB_HANDLE_MARKERS)
}

/// [`handler_no_direct_db`] with explicit marker lists, for testability and reuse.
pub fn handler_no_direct_db_with(
    source: &str,
    handler_markers: &[&str],
    db_handle_markers: &[&str],
) -> Vec<ArchViolation> {
    let mut violations = Vec::new();

    // The function we are currently lexically inside, if any: (name, body_brace_depth).
    // body_brace_depth is the brace depth at which the function body OPENED; when
    // depth falls back below it, the function has closed.
    let mut current_fn: Option<(String, i32)> = None;
    let mut brace_depth: i32 = 0;

    for (idx, raw_line) in source.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_line_comment(raw_line);

        // Detect a function declaration on this line BEFORE counting its braces,
        // so the opening `{` of the body is attributed to entering the function.
        if current_fn.is_none() {
            if let Some(name) = parse_fn_name(line) {
                // The body opens at the first `{` on (or after) this line. We
                // record the depth the body sits at: current depth + 1 once the
                // opening brace is consumed below.
                let is_handler = name_matches(&name, handler_markers);
                // Only track handler functions; non-handler fns we skip entirely
                // so their DB calls (e.g. in a repository) are never flagged.
                if is_handler {
                    // The body brace opens at depth `brace_depth` -> after the
                    // `{` is counted, body statements live at brace_depth+1.
                    current_fn = Some((name, brace_depth + 1));
                }
            }
        }

        // If we are inside a tracked handler body, scan for direct DB calls.
        if let Some((fn_name, body_depth)) = &current_fn {
            // Only scan once we are actually inside the body (depth has reached
            // body_depth), which is true for every line after the opening brace.
            if brace_depth >= *body_depth - 1 {
                for marker in db_handle_markers {
                    if line_has_db_call(line, marker) {
                        violations.push(ArchViolation {
                            rule_id: HANDLER_NO_DIRECT_DB_RULE_ID.to_string(),
                            function: fn_name.clone(),
                            line: line_no,
                            message: format!(
                                "handler `{fn_name}` accesses the database directly via `{marker}.…(…)`; \
                                 handlers must delegate to a service/repository (ARCH-HANDLER-NO-DB-1)"
                            ),
                        });
                        break; // one violation per line is enough to flag it
                    }
                }
            }
        }

        // Now account for this line's braces and close the function if it ended.
        let (opens, closes) = count_braces(line);
        brace_depth += opens as i32;
        brace_depth -= closes as i32;
        if brace_depth < 0 {
            brace_depth = 0;
        }
        if let Some((_, body_depth)) = &current_fn {
            // The handler body has closed once depth drops below the body level.
            if brace_depth < *body_depth {
                current_fn = None;
            }
        }
    }

    violations
}

/// Strip a trailing `//` line comment (best effort; does not handle `//` inside a
/// string literal, an accepted limitation of the lexical approach).
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Count `{` and `}` occurrences on a line.
fn count_braces(line: &str) -> (usize, usize) {
    let opens = line.bytes().filter(|&b| b == b'{').count();
    let closes = line.bytes().filter(|&b| b == b'}').count();
    (opens, closes)
}

/// Extract a function name from a line if it declares one: `fn NAME` (allowing
/// `pub`, `pub(crate)`, `async`, `const`, `unsafe`, `extern "C"` qualifiers).
fn parse_fn_name(line: &str) -> Option<String> {
    let idx = line.find("fn ")?;
    // Guard against matching inside an identifier (e.g. `transform`): require the
    // char before `fn` to be a word boundary.
    if idx > 0 {
        let prev = line.as_bytes()[idx - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    let after = &line[idx + 3..];
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Whether a (lowercased) function name contains any of the markers.
fn name_matches(name: &str, markers: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    markers.iter().any(|m| lower.contains(&m.to_ascii_lowercase()))
}

/// Whether the line contains a `marker.<ident>(` method call: the marker as a
/// whole identifier (a word boundary before it) directly followed by `.`, an
/// identifier, and `(`. This is what distinguishes a real DB-handle call from an
/// incidental occurrence of the token (a longer identifier, a struct field name).
fn line_has_db_call(line: &str, marker: &str) -> bool {
    let bytes = line.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find(marker) {
        let start = search_from + rel;
        let end = start + marker.len();

        // Word boundary BEFORE the marker.
        let boundary_before = start == 0 || {
            let p = bytes[start - 1];
            !(p.is_ascii_alphanumeric() || p == b'_')
        };

        // Directly followed by `.` then an identifier then `(`.
        let followed_by_call = end < bytes.len()
            && bytes[end] == b'.'
            && {
                let rest = &line[end + 1..];
                let method: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                !method.is_empty() && {
                    let after_method = &rest[method.len()..];
                    after_method.starts_with('(')
                }
            };

        if boundary_before && followed_by_call {
            return true;
        }
        search_from = end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_handler_touching_db_directly() {
        let src = r#"
            async fn list_orgs_handler(db: &Db) -> Result<Vec<Org>> {
                let rows = db.query("select * from orgs").await?;
                Ok(rows)
            }
        "#;
        let v = handler_no_direct_db(src);
        assert_eq!(v.len(), 1, "exactly one violation expected: {v:#?}");
        assert_eq!(v[0].rule_id, HANDLER_NO_DIRECT_DB_RULE_ID);
        assert_eq!(v[0].function, "list_orgs_handler");
        assert!(v[0].message.contains("db.…"));
    }

    #[test]
    fn allows_handler_that_delegates_to_a_service() {
        let src = r#"
            async fn list_orgs_handler(svc: &OrgService) -> Result<Vec<Org>> {
                let orgs = svc.list_orgs().await?;
                Ok(orgs)
            }
        "#;
        let v = handler_no_direct_db(src);
        assert!(v.is_empty(), "delegating handler must not be flagged: {v:#?}");
    }

    #[test]
    fn does_not_flag_db_access_in_a_repository_fn() {
        // A repository function legitimately holds the DB handle. Its name is not
        // a handler marker, so a `db.query` there is correct, not a violation.
        let src = r#"
            async fn fetch_all_orgs(db: &Db) -> Result<Vec<Org>> {
                let rows = db.query("select * from orgs").await?;
                Ok(rows)
            }
        "#;
        let v = handler_no_direct_db(src);
        assert!(v.is_empty(), "repository DB access is allowed: {v:#?}");
    }

    #[test]
    fn does_not_flag_db_call_outside_any_handler_body() {
        // A top-level statement (not inside a handler fn) must not be flagged,
        // proving the check is scope-aware rather than a flat regex.
        let src = r#"
            fn helper() {
                println!("no db here");
            }
            let _ = db.query("select 1");
        "#;
        let v = handler_no_direct_db(src);
        assert!(v.is_empty(), "non-handler scope DB call not flagged: {v:#?}");
    }

    #[test]
    fn flags_only_the_handler_when_both_handler_and_repo_present() {
        let src = r#"
            async fn fetch_all_orgs(db: &Db) -> Result<Vec<Org>> {
                let rows = db.query("select * from orgs").await?;
                Ok(rows)
            }

            async fn list_orgs_handler(db: &Db) -> Result<Vec<Org>> {
                let rows = db.query("select * from orgs").await?;
                Ok(rows)
            }
        "#;
        let v = handler_no_direct_db(src);
        assert_eq!(v.len(), 1, "only the handler is flagged: {v:#?}");
        assert_eq!(v[0].function, "list_orgs_handler");
        assert_eq!(v[0].line, 8, "reports the line of the offending call");
    }

    #[test]
    fn respects_brace_depth_for_nested_blocks_in_handler() {
        // A DB call nested several blocks deep inside a handler is still flagged.
        let src = r#"
            async fn create_org_handler(db: &Db) -> Result<()> {
                if true {
                    for _ in 0..3 {
                        db.execute("insert ...").await?;
                    }
                }
                Ok(())
            }
        "#;
        let v = handler_no_direct_db(src);
        assert_eq!(v.len(), 1, "nested DB call inside handler is flagged: {v:#?}");
        assert_eq!(v[0].function, "create_org_handler");
    }

    #[test]
    fn ignores_db_token_in_a_comment() {
        let src = r#"
            async fn list_orgs_handler(svc: &OrgService) -> Result<Vec<Org>> {
                // do not call db.query here, delegate instead
                svc.list_orgs().await
            }
        "#;
        let v = handler_no_direct_db(src);
        assert!(v.is_empty(), "commented-out db call not flagged: {v:#?}");
    }

    #[test]
    fn does_not_flag_substring_of_a_longer_identifier() {
        // `dbg` / `database_url` contain "db" but are not a `db.` call.
        let src = r#"
            async fn ping_handler(req: Req) -> Result<()> {
                let database_url = req.config();
                dbg!(&database_url);
                Ok(())
            }
        "#;
        let v = handler_no_direct_db(src);
        assert!(v.is_empty(), "word-boundary respected: {v:#?}");
    }

    #[test]
    fn custom_markers_are_honoured() {
        let src = r#"
            fn my_special_endpoint(store: &Store) -> Result<()> {
                store.write("x");
                Ok(())
            }
        "#;
        // With default markers `store` is not a DB handle -> no violation.
        assert!(handler_no_direct_db(src).is_empty());
        // With custom markers it is.
        let v = handler_no_direct_db_with(src, &["endpoint"], &["store"]);
        assert_eq!(v.len(), 1, "custom db marker flagged: {v:#?}");
        assert_eq!(v[0].function, "my_special_endpoint");
    }
}
