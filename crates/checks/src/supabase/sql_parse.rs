//! Tiny, panic-free DDL statement classifier over ALREADY-SPLIT statement text (see
//! `splitter.rs`). Deliberately shallow: per the design memo, "the architectural content is
//! the ordering and the fold, not deep SQL parsing" — this recognizes exactly the statement
//! shapes the RLS/search-path checkers need (`CREATE/DROP/RENAME TABLE`, `ALTER ... ROW
//! LEVEL SECURITY`, `CREATE/DROP POLICY`, `CREATE [OR REPLACE] FUNCTION`) and returns `None`
//! for everything else, including malformed input.
//!
//! All indexing here is bounds-checked (`.get(..)` or an explicit length comparison before
//! slicing) — this module runs over adversarial migration files and must never panic.

const DEFAULT_SCHEMA: &str = "public";

/// One DDL statement the timeline fold understands. Anything else classifies to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedStmt {
    CreateTable {
        schema: String,
        table: String,
        if_not_exists: bool,
    },
    DropTable {
        schema: String,
        table: String,
    },
    RenameTable {
        schema: String,
        from: String,
        to: String,
    },
    AlterRls {
        schema: String,
        table: String,
        enabled: bool,
    },
    CreatePolicy {
        schema: String,
        table: String,
        name: String,
    },
    DropPolicy {
        schema: String,
        table: String,
        name: String,
    },
    CreateFunction {
        schema: String,
        name: String,
        security_definer: bool,
        has_search_path: bool,
    },
}

/// Classify one whitespace-normalized statement (as produced by `splitter::split_statements`).
/// Never panics on any input, including truncated or nonsensical text.
pub fn classify_statement(text: &str) -> Option<ParsedStmt> {
    let raw: Vec<char> = text.chars().collect();
    let upper: Vec<char> = raw.iter().map(|c| c.to_ascii_uppercase()).collect();
    let i = skip_ws(&upper, 0);

    // CREATE TABLE [IF NOT EXISTS] name
    if let Some(j0) = match_kw_seq(&upper, i, &["CREATE", "TABLE"]) {
        let mut j = skip_ws(&upper, j0);
        let mut if_not_exists = false;
        if let Some(j2) = match_kw_seq(&upper, j, &["IF", "NOT", "EXISTS"]) {
            if_not_exists = true;
            j = skip_ws(&upper, j2);
        }
        return parse_qualified_name(&raw, j).map(|((schema, table), _)| ParsedStmt::CreateTable {
            schema,
            table,
            if_not_exists,
        });
    }

    // DROP TABLE [IF EXISTS] name
    if let Some(j0) = match_kw_seq(&upper, i, &["DROP", "TABLE"]) {
        let mut j = skip_ws(&upper, j0);
        if let Some(j2) = match_kw_seq(&upper, j, &["IF", "EXISTS"]) {
            j = skip_ws(&upper, j2);
        }
        return parse_qualified_name(&raw, j).map(|((schema, table), _)| ParsedStmt::DropTable { schema, table });
    }

    // ALTER TABLE [ONLY] name  { ENABLE|DISABLE ROW LEVEL SECURITY | RENAME TO name }
    if let Some(j0) = match_kw_seq(&upper, i, &["ALTER", "TABLE"]) {
        let mut j = skip_ws(&upper, j0);
        if let Some(j2) = match_kw_seq(&upper, j, &["ONLY"]) {
            j = skip_ws(&upper, j2);
        }
        let ((schema, table), after_name) = parse_qualified_name(&raw, j)?;
        let k = skip_ws(&upper, after_name);
        if match_kw_seq(&upper, k, &["ENABLE", "ROW", "LEVEL", "SECURITY"]).is_some() {
            return Some(ParsedStmt::AlterRls {
                schema,
                table,
                enabled: true,
            });
        }
        if match_kw_seq(&upper, k, &["DISABLE", "ROW", "LEVEL", "SECURITY"]).is_some() {
            return Some(ParsedStmt::AlterRls {
                schema,
                table,
                enabled: false,
            });
        }
        if let Some(k2) = match_kw_seq(&upper, k, &["RENAME", "TO"]) {
            let k2 = skip_ws(&upper, k2);
            if let Some((new_name, _)) = parse_ident(&raw, k2) {
                return Some(ParsedStmt::RenameTable {
                    schema,
                    from: table,
                    to: new_name,
                });
            }
        }
        return None;
    }

    // CREATE POLICY name ON table
    if let Some(j0) = match_kw_seq(&upper, i, &["CREATE", "POLICY"]) {
        let j = skip_ws(&upper, j0);
        let (name, after_name) = parse_ident(&raw, j)?;
        let k = skip_ws(&upper, after_name);
        let k2 = match_kw_seq(&upper, k, &["ON"])?;
        let k2 = skip_ws(&upper, k2);
        return parse_qualified_name(&raw, k2).map(|((schema, table), _)| ParsedStmt::CreatePolicy {
            schema,
            table,
            name,
        });
    }

    // DROP POLICY [IF EXISTS] name ON table
    if let Some(j0) = match_kw_seq(&upper, i, &["DROP", "POLICY"]) {
        let mut j = skip_ws(&upper, j0);
        if let Some(j2) = match_kw_seq(&upper, j, &["IF", "EXISTS"]) {
            j = skip_ws(&upper, j2);
        }
        let (name, after_name) = parse_ident(&raw, j)?;
        let k = skip_ws(&upper, after_name);
        let k2 = match_kw_seq(&upper, k, &["ON"])?;
        let k2 = skip_ws(&upper, k2);
        return parse_qualified_name(&raw, k2).map(|((schema, table), _)| ParsedStmt::DropPolicy {
            schema,
            table,
            name,
        });
    }

    // CREATE [OR REPLACE] FUNCTION name(...) ... [SECURITY DEFINER] [SET search_path ...] AS ...
    if let Some(j0) = match_kw_seq(&upper, i, &["CREATE"]) {
        let mut k = skip_ws(&upper, j0);
        if let Some(k2) = match_kw_seq(&upper, k, &["OR", "REPLACE"]) {
            k = skip_ws(&upper, k2);
        }
        let k = match_kw_seq(&upper, k, &["FUNCTION"])?;
        let k = skip_ws(&upper, k);
        let ((schema, name), after_name) = parse_qualified_name(&raw, k)?;
        // Only scan the SIGNATURE portion (up to the first dollar-quoted body, if any) for
        // SECURITY DEFINER / SET search_path — a match inside the function BODY (e.g. a
        // string literal the function builds) must never be mistaken for the real clause.
        let sig_end = first_dollar_quote_start(&raw, after_name).unwrap_or(raw.len());
        let sig_upper = upper.get(after_name.min(sig_end)..sig_end.max(after_name)).unwrap_or(&[]);
        let security_definer = contains_kw_seq(sig_upper, &["SECURITY", "DEFINER"]);
        let has_search_path = contains_kw_seq(sig_upper, &["SET", "SEARCH_PATH"]);
        return Some(ParsedStmt::CreateFunction {
            schema,
            name,
            security_definer,
            has_search_path,
        });
    }

    None
}

// ── shared lexical helpers ──────────────────────────────────────────────────────────

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Match a sequence of literal (already-uppercase) keywords against `chars` (itself
/// expected to be pre-uppercased) starting at `i`, requiring whitespace BETWEEN words and a
/// non-identifier boundary immediately BEFORE the first word and AFTER the last — so
/// `TABLESPACE` is never mistaken for `TABLE`, and a keyword embedded in a longer
/// identifier is never matched mid-scan (used by [`contains_kw_seq`]). Returns the index
/// just past the last matched word, or `None`.
fn match_kw_seq(chars: &[char], mut i: usize, words: &[&str]) -> Option<usize> {
    for (wi, w) in words.iter().enumerate() {
        if wi > 0 {
            let before = i;
            i = skip_ws(chars, i);
            if i == before {
                return None; // words must be whitespace-separated
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

/// Whether `words` (as a whitespace/boundary-respecting sequence) occurs ANYWHERE in
/// `chars`, scanning every start offset. Used for the CREATE FUNCTION signature scan, where
/// the clause can appear at an arbitrary position among the function's other options.
fn contains_kw_seq(chars: &[char], words: &[&str]) -> bool {
    (0..chars.len()).any(|i| match_kw_seq(chars, i, words).is_some())
}

/// Parse one identifier at `i` (skipping leading whitespace first): a double-quoted
/// identifier (`""` escapes a literal quote, case preserved) or a bareword (folded to
/// lowercase, matching Postgres's own unquoted-identifier case-folding). Returns the
/// identifier text and the index just past it. `None` if `i` is not at an identifier start
/// (EOF, or a character that can't begin one).
fn parse_ident(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut i = skip_ws(chars, i);
    let c = *chars.get(i)?;
    if c == '"' {
        let mut out = String::new();
        i += 1;
        loop {
            match chars.get(i) {
                None => break, // unterminated — best-effort, never panics
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

/// Parse a possibly schema-qualified name (`schema.table` or bare `table`, defaulting the
/// schema to `public` when unqualified — the Postgres default `search_path` for an
/// unqualified DDL target in the vast majority of Supabase migrations). Returns
/// `((schema, name), index_just_past_the_name)`.
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

/// If a dollar-quote (`$$` or `$tag$`) begins anywhere at or after `from`, return its start
/// index; else `None`. Delegates the actual tag grammar to the splitter's own parser so the
/// two modules agree on what counts as a dollar-quote open.
fn first_dollar_quote_start(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == '$' && super::splitter::parse_dollar_tag(chars, i).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_table_bare_defaults_to_public_schema() {
        let got = classify_statement("create table profiles (id uuid primary key)");
        assert_eq!(
            got,
            Some(ParsedStmt::CreateTable {
                schema: "public".into(),
                table: "profiles".into(),
                if_not_exists: false,
            })
        );
    }

    #[test]
    fn create_table_if_not_exists_and_schema_qualified() {
        let got = classify_statement("create table if not exists app.orders (id int)");
        assert_eq!(
            got,
            Some(ParsedStmt::CreateTable {
                schema: "app".into(),
                table: "orders".into(),
                if_not_exists: true,
            })
        );
    }

    #[test]
    fn create_table_does_not_match_tablespace() {
        assert_eq!(classify_statement("create tablespace fast location '/data'"), None);
    }

    #[test]
    fn drop_table_if_exists() {
        let got = classify_statement("drop table if exists public.profiles");
        assert_eq!(
            got,
            Some(ParsedStmt::DropTable {
                schema: "public".into(),
                table: "profiles".into(),
            })
        );
    }

    #[test]
    fn alter_table_enable_rls() {
        let got = classify_statement("alter table public.profiles enable row level security");
        assert_eq!(
            got,
            Some(ParsedStmt::AlterRls {
                schema: "public".into(),
                table: "profiles".into(),
                enabled: true,
            })
        );
    }

    #[test]
    fn alter_table_only_disable_rls() {
        let got = classify_statement("alter table only profiles disable row level security");
        assert_eq!(
            got,
            Some(ParsedStmt::AlterRls {
                schema: "public".into(),
                table: "profiles".into(),
                enabled: false,
            })
        );
    }

    #[test]
    fn alter_table_rename_to() {
        let got = classify_statement("alter table profiles rename to accounts");
        assert_eq!(
            got,
            Some(ParsedStmt::RenameTable {
                schema: "public".into(),
                from: "profiles".into(),
                to: "accounts".into(),
            })
        );
    }

    #[test]
    fn create_policy_on_table() {
        let got = classify_statement("create policy \"read own\" on public.profiles for select using (true)");
        assert_eq!(
            got,
            Some(ParsedStmt::CreatePolicy {
                schema: "public".into(),
                table: "profiles".into(),
                name: "read own".into(),
            })
        );
    }

    #[test]
    fn drop_policy_if_exists() {
        let got = classify_statement("drop policy if exists read_own on profiles");
        assert_eq!(
            got,
            Some(ParsedStmt::DropPolicy {
                schema: "public".into(),
                table: "profiles".into(),
                name: "read_own".into(),
            })
        );
    }

    #[test]
    fn create_function_security_definer_with_search_path() {
        let stmt = "create function public.set_role() returns void security definer set search_path = public as $$ begin end; $$ language plpgsql";
        let got = classify_statement(stmt);
        assert_eq!(
            got,
            Some(ParsedStmt::CreateFunction {
                schema: "public".into(),
                name: "set_role".into(),
                security_definer: true,
                has_search_path: true,
            })
        );
    }

    #[test]
    fn create_function_security_definer_without_search_path() {
        let stmt = "create or replace function public.set_role() returns void security definer as $$ begin end; $$ language plpgsql";
        let got = classify_statement(stmt);
        assert_eq!(
            got,
            Some(ParsedStmt::CreateFunction {
                schema: "public".into(),
                name: "set_role".into(),
                security_definer: true,
                has_search_path: false,
            })
        );
    }

    #[test]
    fn create_function_search_path_inside_body_is_not_mistaken_for_the_clause() {
        // "search_path" appearing inside the dollar-quoted BODY (e.g. as a string the
        // function builds) must not satisfy has_search_path — only the SIGNATURE counts.
        let stmt = "create function public.f() returns text security definer as $$ begin return 'set search_path = public'; end; $$ language plpgsql";
        let got = classify_statement(stmt);
        assert_eq!(
            got,
            Some(ParsedStmt::CreateFunction {
                schema: "public".into(),
                name: "f".into(),
                security_definer: true,
                has_search_path: false,
            })
        );
    }

    #[test]
    fn create_function_security_invoker_is_not_definer() {
        let stmt = "create function public.f() returns void as $$ begin end; $$ language plpgsql";
        let got = classify_statement(stmt);
        assert_eq!(
            got,
            Some(ParsedStmt::CreateFunction {
                schema: "public".into(),
                name: "f".into(),
                security_definer: false,
                has_search_path: false,
            })
        );
    }

    #[test]
    fn garbage_text_classifies_to_none_without_panicking() {
        assert_eq!(classify_statement(""), None);
        assert_eq!(classify_statement("   "), None);
        assert_eq!(classify_statement("select 1"), None);
        assert_eq!(classify_statement("create"), None);
        assert_eq!(classify_statement("create table"), None);
        assert_eq!(classify_statement("alter table"), None);
        assert_eq!(classify_statement("$$$\"'"), None);
    }

    #[test]
    fn quoted_identifier_with_unicode_table_name() {
        let got = classify_statement("create table \"café_users\" (id int)");
        assert_eq!(
            got,
            Some(ParsedStmt::CreateTable {
                schema: "public".into(),
                table: "café_users".into(),
                if_not_exists: false,
            })
        );
    }
}
