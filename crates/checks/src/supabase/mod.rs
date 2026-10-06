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
//! - [`dynamic_sql_exec_checker`]: [`dynamic_sql_exec_checker::DynamicSqlExecInjectionChecker`]
//!   — `SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1` (a function body `EXECUTE`-ing a
//!   dynamically-assembled, non-quoted query string — including the build-then-EXECUTE
//!   two-statement idiom, tracked via intra-function dataflow).
//! - [`privileged_no_authz_checker`]:
//!   [`privileged_no_authz_checker::PrivilegedFunctionNoAuthzChecker`] —
//!   `SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1` (a `SECURITY DEFINER`, mutating, broadly-granted
//!   function whose body never checks who the caller is — the WRITE shape).
//! - [`cross_tenant_definer_read_checker`]:
//!   [`cross_tenant_definer_read_checker::CrossTenantDefinerReadChecker`] —
//!   `SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1` (the READ shape of the same missing-
//!   authorization defect: a `SECURITY DEFINER` function returning rows filtered only by a
//!   caller-supplied parameter, with no predicate tying them to the caller's own identity).

pub mod config;
pub mod cross_tenant_definer_read_checker;
pub mod dynamic_sql_exec_checker;
pub mod privileged_no_authz_checker;
pub mod rls_checker;
pub mod search_path_checker;
pub mod splitter;
pub mod sql_parse;
pub mod timeline;
