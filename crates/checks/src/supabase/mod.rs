//! Supabase-stack architectural checkers: the first consumers of the
//! [`crate::arch_checker::ArchChecker`] seam. See
//! `docs/design/2026-07-26_architectural-executor-feasibility.md` §3 and
//! `docs/design/2026-07-26_supabase-stack-rules.md` §4 for the design rationale.
//!
//! # Module layout
//!
//! - [`splitter`]: the dollar-quote/comment-aware SQL statement splitter — the "genuinely
//!   careful part" per the design memo.
//! - [`sql_parse`]: a panic-free, shallow DDL statement classifier over already-split
//!   statement text (`CREATE/DROP/RENAME TABLE`, `ALTER ... ROW LEVEL SECURITY`,
//!   `CREATE/DROP POLICY`, `CREATE [OR REPLACE] FUNCTION`).
//! - [`timeline`]: the shared migration-timeline replay fold both checkers below build on.
//! - [`config`]: `supabase/config.toml` `[api].schemas` exposed-schema parse.
//! - [`rls_checker`]: [`rls_checker::SupabaseRlsChecker`] — the three RLS rule ids.
//! - [`search_path_checker`]: [`search_path_checker::SupabaseFnSearchPathChecker`] —
//!   `SUPABASE-FUNC-SEARCH-PATH-1`.

pub mod config;
pub mod rls_checker;
pub mod search_path_checker;
pub mod splitter;
pub mod sql_parse;
pub mod timeline;
