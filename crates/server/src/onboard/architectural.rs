//! The scan-time plug point for the deterministic architectural-rule executor
//! (`camerata_checks::arch_checker`). See
//! `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.2 — this is "plug
//! point A" (the brownfield scan), wired into `audit_repos` right after the floor call.
//! Plug point B (the Layer-2 `NativeArchCheckRunner`) is explicitly OUT of scope here; see
//! that file's own doc for what's left.
//!
//! `camerata-checks` owns the trait/registry/checkers and knows nothing of `Finding` (the
//! server's wire type) — this module is the adapter side of that boundary: it runs the
//! registry over one repo's files and converts every `ArchViolation` into a `Finding` that
//! rides the EXISTING pipeline (suppression classification, test-scope down-ranking,
//! report/CSV/UI) completely unchanged, exactly like the floor's own findings do.

use std::collections::HashSet;

use camerata_checks::arch_checker::{all_checkers, checker_applies, ArchViolation, RepoView};

use super::Finding;

/// The provenance tag every architectural finding carries in `Finding.preview_tool`, so the
/// UI can badge these distinctly ("deterministic — replayed end-state") from both an
/// enforced floor hit and an AI-advisory finding. Deliberately reuses the EXISTING
/// `preview`/`preview_tool` mechanism (Part B) rather than adding a new field: an
/// architectural finding is, today, exactly what "preview" already means for this
/// enforcement tier — deterministic and stable, but not yet wired into a write-time gate
/// (Layer-2 wiring is Pass 2, not built here). See `ui_core::scan::det_tool_label` for the
/// UI-facing label this tag maps to.
pub const ARCH_PREVIEW_TOOL: &str = "camerata-arch";

/// Run every registered [`camerata_checks::arch_checker::ArchChecker`] whose `rule_ids`
/// intersect `armed_rule_ids` (this repo's selected/bound ruleset — the same per-repo
/// binding `audit_repos` already applies to the floor and the AI-audit prompt) over `files`,
/// converting every violation into a `Finding`. A checker with zero files matching its own
/// `interest_globs` is skipped entirely (never a false "clean" — see
/// [`camerata_checks::arch_checker::checker_applies`]).
pub fn audit_architectural(repo: &str, files: &[(String, String)], armed_rule_ids: &HashSet<&str>) -> Vec<Finding> {
    let view = RepoView { spec: repo, files };
    let mut findings = Vec::new();
    for checker in all_checkers() {
        let armed = checker.rule_ids().iter().any(|id| armed_rule_ids.contains(id));
        if !armed {
            continue;
        }
        if !checker_applies(checker.as_ref(), files) {
            continue;
        }
        for violation in checker.check(&view) {
            findings.push(arch_violation_to_finding(repo, &violation));
        }
    }
    findings
}

/// Which placeholder TOKEN NAME (no angle brackets — matches a rule's own
/// `directive`/`remediation` text, see `report_export::resolve_fix`) a given rule id's
/// `ArchViolation::object` should be captured under, when that checker names a schema-qualified
/// object we understand. `None` for a rule id whose checker doesn't name an object in a shape
/// this function knows how to map (including every non-Supabase arch checker) — those findings
/// simply carry empty `captures`, and the report's generic-fallback wording covers them; this is
/// deliberately a small, explicit allowlist rather than a guess, so a capture is only ever wired
/// when we're sure it names the same kind of object the rule's authored text expects.
fn capture_token_for(rule_id: &str) -> Option<&'static str> {
    use camerata_checks::supabase::{rls_checker, search_path_checker};
    match rule_id {
        id if id == rls_checker::RULE_RLS_ENABLED
            || id == rls_checker::RULE_RLS_NO_POLICY
            || id == rls_checker::RULE_RLS_POLICY_DISABLED =>
        {
            Some("table")
        }
        id if id == search_path_checker::RULE_FUNC_SEARCH_PATH => Some("function-name"),
        _ => None,
    }
}

/// The buyer-facing bare object name from a checker's schema-qualified `"schema.object"`
/// violation object string: the bare name in the (overwhelmingly common) `public` schema,
/// schema-qualified otherwise — mirrors `SupabaseRlsChecker::display_name`'s own framing
/// (without the backticks the authored remediation text already supplies around the
/// placeholder token itself, e.g. `` "`<table>`" ``).
fn bare_object_name(object: &str) -> String {
    match object.split_once('.') {
        Some(("public", rest)) if !rest.is_empty() => rest.to_string(),
        _ => object.to_string(),
    }
}

/// Convert one [`ArchViolation`] into a `Finding`, tagged so the UI/CSV/report can
/// distinguish it from every other finding source. `status` defaults to `active`
/// (`Finding::default()`) — the caller (`audit_repos`) still runs `classify_repo_findings`
/// over the combined finding set afterward, so an architectural finding is just as
/// waivable via `camerata:allow` / the baseline as a floor finding.
///
/// Also populates `Finding.captures` (Fix 1's placeholder-instantiation input, see
/// `report_export::resolve_fix`) from `violation.object` when this rule id's checker names an
/// object in a shape [`capture_token_for`] understands — e.g. `SUPABASE-RLS-ENABLED-1`'s
/// `"public.profiles"` becomes `{"table": "profiles"}`, so the rule's authored `<table>`
/// remediation token names the actual table in THIS codebase instead of falling back to the
/// generic "the affected table".
pub fn arch_violation_to_finding(repo: &str, violation: &ArchViolation) -> Finding {
    let mut captures = std::collections::BTreeMap::new();
    if let (Some(token), Some(object)) =
        (capture_token_for(&violation.rule_id), violation.object.as_deref())
    {
        captures.insert(token.to_string(), bare_object_name(object));
    }
    Finding {
        repo: repo.to_string(),
        path: violation.file.clone(),
        line: violation.line,
        rule_id: violation.rule_id.clone(),
        severity: violation.severity.to_string(),
        snippet: violation.object.clone().unwrap_or_default(),
        detail: violation.message.clone(),
        preview: true,
        preview_tool: Some(ARCH_PREVIEW_TOOL.to_string()),
        captures,
        ..Finding::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    #[test]
    fn arch_violation_to_finding_carries_the_provenance_tag() {
        let v = ArchViolation {
            rule_id: "SUPABASE-RLS-ENABLED-1".to_string(),
            file: "supabase/migrations/20240101000000_init.sql".to_string(),
            line: 3,
            object: Some("public.profiles".to_string()),
            message: "no RLS".to_string(),
            severity: "critical",
        };
        let f = arch_violation_to_finding("acme/app", &v);
        assert_eq!(f.repo, "acme/app");
        assert_eq!(f.path, "supabase/migrations/20240101000000_init.sql");
        assert_eq!(f.line, 3);
        assert_eq!(f.rule_id, "SUPABASE-RLS-ENABLED-1");
        assert_eq!(f.severity, "critical");
        assert_eq!(f.snippet, "public.profiles");
        assert_eq!(f.detail, "no RLS");
        assert!(f.preview);
        assert_eq!(f.preview_tool.as_deref(), Some(ARCH_PREVIEW_TOOL));
        assert_eq!(f.status, "active");
    }

    #[test]
    fn arch_violation_to_finding_captures_the_bare_table_name_for_public_schema() {
        let v = ArchViolation {
            rule_id: "SUPABASE-RLS-ENABLED-1".to_string(),
            file: "supabase/migrations/20240101000000_init.sql".to_string(),
            line: 3,
            object: Some("public.profiles".to_string()),
            message: "no RLS".to_string(),
            severity: "critical",
        };
        let f = arch_violation_to_finding("acme/app", &v);
        assert_eq!(f.captures.get("table").map(String::as_str), Some("profiles"));
    }

    #[test]
    fn arch_violation_to_finding_captures_a_schema_qualified_table_name_for_non_public_schema() {
        let v = ArchViolation {
            rule_id: "SUPABASE-RLS-NO-POLICY-1".to_string(),
            file: "supabase/migrations/20240101000000_init.sql".to_string(),
            line: 3,
            object: Some("billing.invoices".to_string()),
            message: "zero policies".to_string(),
            severity: "medium",
        };
        let f = arch_violation_to_finding("acme/app", &v);
        assert_eq!(
            f.captures.get("table").map(String::as_str),
            Some("billing.invoices")
        );
    }

    #[test]
    fn arch_violation_to_finding_captures_the_function_name_for_search_path() {
        let v = ArchViolation {
            rule_id: "SUPABASE-FUNC-SEARCH-PATH-1".to_string(),
            file: "supabase/migrations/20240101000000_init.sql".to_string(),
            line: 8,
            object: Some("public.charge_membership".to_string()),
            message: "no search_path".to_string(),
            severity: "high",
        };
        let f = arch_violation_to_finding("acme/app", &v);
        assert_eq!(
            f.captures.get("function-name").map(String::as_str),
            Some("charge_membership")
        );
    }

    /// Every Supabase corpus rule whose authored remediation text uses a domain placeholder
    /// (`<table>`, `<view>`, `<bucket>`, `<matview>`, `<schema>`, `<function-name>`) OTHER than
    /// the four `capture_token_for` already maps is NOT answered by any deterministic
    /// `ArchChecker` today (verified against `crates/checks/src/supabase/`: only
    /// `SupabaseRlsChecker` and `SupabaseFnSearchPathChecker` exist, and between them they only
    /// answer RLS-ENABLED / RLS-NO-POLICY / RLS-POLICY-DISABLED / FUNC-SEARCH-PATH). Wiring a
    /// deterministic-checker token for one of these would be a lie about provenance — they are
    /// AI-audit findings (see `ai_audit::parse_finding_captures` for that path instead) or, for
    /// `SUPABASE-RLS-INITPLAN-1`/`SUPABASE-RLS-PERMISSIVE-TRUE-1`/`SUPABASE-RLS-VIEW-INVOKER-1`,
    /// facets `SupabaseRlsChecker` genuinely does not inspect (policy predicate content /
    /// `security_invoker`). This test pins `capture_token_for` to `None` for all of them so a
    /// future edit that "helpfully" wires one without also adding a real checker fails loudly.
    #[test]
    fn capture_token_for_stays_none_for_rules_with_no_deterministic_checker() {
        let no_deterministic_checker_yet = [
            "SUPABASE-RLS-INITPLAN-1",
            "SUPABASE-RLS-PERMISSIVE-TRUE-1",
            "SUPABASE-RLS-VIEW-INVOKER-1",
            "SUPABASE-AUTH-EDGE-JWT-1",
            "SUPABASE-STORAGE-OBJECT-POLICY-1",
            "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
            "SUPABASE-EXPOSURE-MATVIEW-1",
            "SUPABASE-EXPOSURE-SCHEMAS-1",
        ];
        for id in no_deterministic_checker_yet {
            assert_eq!(
                capture_token_for(id),
                None,
                "{id} has no deterministic ArchChecker — capture_token_for must not claim one"
            );
        }
    }

    /// Pins the FULL currently-known-deterministic set to its exact token — the flip side of
    /// the `_stays_none_` test above. If a new `ArchChecker` starts answering one of the
    /// currently-AI-only rule ids above, this pair of tests is where that migration shows up:
    /// the id moves from the `None` list to this list with its real token.
    #[test]
    fn capture_token_for_maps_every_currently_deterministic_rule() {
        let expected = [
            ("SUPABASE-RLS-ENABLED-1", "table"),
            ("SUPABASE-RLS-NO-POLICY-1", "table"),
            ("SUPABASE-RLS-POLICY-DISABLED-1", "table"),
            ("SUPABASE-FUNC-SEARCH-PATH-1", "function-name"),
        ];
        for (id, token) in expected {
            assert_eq!(capture_token_for(id), Some(token), "rule id: {id}");
        }
    }

    #[test]
    fn arch_violation_to_finding_leaves_captures_empty_for_an_unmapped_rule() {
        let v = ArchViolation {
            rule_id: "UI-UTC-DATES-1".to_string(),
            file: "src/lib.rs".to_string(),
            line: 1,
            object: Some("some_fn".to_string()),
            message: "uses local time".to_string(),
            severity: "low",
        };
        let f = arch_violation_to_finding("acme/app", &v);
        assert!(f.captures.is_empty());
    }

    #[test]
    fn audit_architectural_skips_checkers_not_in_the_armed_set() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);",
        )]);
        let armed: HashSet<&str> = HashSet::new(); // nothing armed
        let findings = audit_architectural("acme/app", &f, &armed);
        assert!(findings.is_empty(), "no rule armed -> no findings, even though the file matches");
    }

    #[test]
    fn audit_architectural_fires_when_rule_is_armed_and_files_match() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);",
        )]);
        let armed: HashSet<&str> = ["SUPABASE-RLS-ENABLED-1"].into_iter().collect();
        let findings = audit_architectural("acme/app", &f, &armed);
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].rule_id, "SUPABASE-RLS-ENABLED-1");
        assert_eq!(findings[0].preview_tool.as_deref(), Some(ARCH_PREVIEW_TOOL));
    }

    #[test]
    fn audit_architectural_skips_checkers_with_zero_interest_files() {
        let f = files(vec![("README.md", "hello")]);
        let armed: HashSet<&str> = ["SUPABASE-RLS-ENABLED-1", "SUPABASE-FUNC-SEARCH-PATH-1"]
            .into_iter()
            .collect();
        let findings = audit_architectural("acme/app", &f, &armed);
        assert!(findings.is_empty(), "no supabase/ files present -> no checker applies");
    }
}
