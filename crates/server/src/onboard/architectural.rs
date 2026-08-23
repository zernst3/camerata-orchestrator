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

/// Convert one [`ArchViolation`] into a `Finding`, tagged so the UI/CSV/report can
/// distinguish it from every other finding source. `status` defaults to `active`
/// (`Finding::default()`) — the caller (`audit_repos`) still runs `classify_repo_findings`
/// over the combined finding set afterward, so an architectural finding is just as
/// waivable via `camerata:allow` / the baseline as a floor finding.
pub fn arch_violation_to_finding(repo: &str, violation: &ArchViolation) -> Finding {
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
