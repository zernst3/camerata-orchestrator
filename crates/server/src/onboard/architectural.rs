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

use std::borrow::Cow;
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
pub fn audit_architectural(
    repo: &str,
    files: &[(String, String)],
    armed_rule_ids: &HashSet<&str>,
) -> Vec<Finding> {
    let view = RepoView { spec: repo, files };
    let mut findings = Vec::new();
    for checker in all_checkers() {
        let armed = checker
            .rule_ids()
            .iter()
            .any(|id| armed_rule_ids.contains(id));
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

/// Armed rules that reason from the ABSENCE of a reference and so must see reference text
/// (HTML, single-file components, Markdown, stylesheets, templates, build-runner files) as well
/// as code: a `<script src>` or a component import is the only evidence a file is still used.
const RULES_NEEDING_REFERENCE_TEXT: &[&str] = &[camerata_checks::dead_code_checker::RULE_DEAD_CODE];

/// The file list the architectural tier is handed: the audited `files`, plus the scan's
/// `reference_files` when (and only when) an armed rule needs them. Every other consumer of the
/// scan (the AI digest, the secrets floor, stack detection) keeps reading `files` alone, so
/// adding reference text costs them nothing, and a scan that arms no such rule borrows `files`
/// without copying.
pub fn architectural_input<'a>(
    files: &'a [(String, String)],
    reference_files: &[(String, String)],
    armed_rule_ids: &HashSet<&str>,
) -> Cow<'a, [(String, String)]> {
    let needed = RULES_NEEDING_REFERENCE_TEXT
        .iter()
        .any(|id| armed_rule_ids.contains(id));
    if !needed || reference_files.is_empty() {
        return Cow::Borrowed(files);
    }
    let mut all = Vec::with_capacity(files.len() + reference_files.len());
    all.extend_from_slice(files);
    all.extend_from_slice(reference_files);
    Cow::Owned(all)
}

/// Armed rules that reason from the ABSENCE of a reference and so are unsound on a scan that
/// stopped at its file limit: an importer may be among the files that were dropped, and a file
/// that is in fact used then looks unreachable. The React Hooks lint rule is the second shape of
/// the same problem: "no lint configuration enables the rules" is false if the configuration is
/// among the dropped files.
const RULES_NEEDING_COMPLETE_SCAN: &[&str] = &[
    camerata_checks::dead_code_checker::RULE_DEAD_CODE,
    camerata_checks::react_hooks_lint_checker::RULE_REACT_HOOKS_LINT,
];

/// Whether `rule_id` is one of the rules an incomplete scan withholds.
pub fn is_withheld_on_incomplete_scan(rule_id: &str) -> bool {
    RULES_NEEDING_COMPLETE_SCAN.contains(&rule_id)
}

/// `ids` without the rules that cannot be answered on an incomplete scan. Apply it to the armed
/// set handed to [`audit_architectural`] AND to the set the ledger credits as a detector run, so
/// a withheld rule is neither run nor reported as "verified clean": with no detector channel for
/// the repo it falls to the config-gated disclosure instead.
pub fn withhold_on_incomplete_scan<'a>(
    ids: &HashSet<&'a str>,
    incomplete: bool,
) -> HashSet<&'a str> {
    ids.iter()
        .copied()
        .filter(|id| !(incomplete && RULES_NEEDING_COMPLETE_SCAN.contains(id)))
        .collect()
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
    use camerata_checks::supabase::{dynamic_sql_exec_checker, rls_checker, search_path_checker};
    match rule_id {
        id if id == rls_checker::RULE_RLS_ENABLED
            || id == rls_checker::RULE_RLS_NO_POLICY
            || id == rls_checker::RULE_RLS_POLICY_DISABLED
            || id == rls_checker::RULE_RLS_DISABLED_NOT_RESTORED =>
        {
            Some("table")
        }
        id if id == search_path_checker::RULE_FUNC_SEARCH_PATH => Some("function-name"),
        id if id == dynamic_sql_exec_checker::RULE_DYNAMIC_SQL_EXEC_INJECTION => {
            Some("function-name")
        }
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
    if let (Some(token), Some(object)) = (
        capture_token_for(&violation.rule_id),
        violation.object.as_deref(),
    ) {
        captures.insert(token.to_string(), bare_object_name(object));
    }
    let mut finding = Finding {
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
        // A5.6: a deterministic architectural-checker violation is an objective fault.
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    };
    // A checker's message or object can quote a literal from the client's code.
    crate::onboard::redact::redact_finding(&mut finding);
    finding
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
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
        assert_eq!(
            f.captures.get("table").map(String::as_str),
            Some("profiles")
        );
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
    /// the five `capture_token_for` already maps is NOT answered by any deterministic
    /// `ArchChecker` today (verified against `crates/checks/src/supabase/`: only
    /// `SupabaseRlsChecker` and `SupabaseFnSearchPathChecker` exist, and between them they only
    /// answer RLS-ENABLED / RLS-NO-POLICY / RLS-POLICY-DISABLED / RLS-DISABLED-NOT-RESTORED /
    /// FUNC-SEARCH-PATH). Wiring a deterministic-checker token for one of these would be a lie
    /// about provenance — they are AI-audit findings (see `ai_audit::parse_finding_captures`
    /// for that path instead) or, for
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
            ("SUPABASE-RLS-DISABLED-NOT-RESTORED-1", "table"),
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
        assert!(
            findings.is_empty(),
            "no rule armed -> no findings, even though the file matches"
        );
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
        assert!(
            findings.is_empty(),
            "no supabase/ files present -> no checker applies"
        );
    }

    // ── architectural_input: the reference-text tier reaches only the architectural tier ──

    fn dead_code_armed() -> HashSet<&'static str> {
        [camerata_checks::dead_code_checker::RULE_DEAD_CODE]
            .into_iter()
            .collect()
    }

    #[test]
    fn architectural_input_appends_reference_text_when_the_dead_code_rule_is_armed() {
        let code = files(vec![("src/a.ts", "export {};\n")]);
        let reference = files(vec![
            ("index.html", "<script src=\"/src/a.ts\">"),
            ("README.md", "hi"),
        ]);
        let input = architectural_input(&code, &reference, &dead_code_armed());
        let paths: Vec<&str> = input.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["src/a.ts", "index.html", "README.md"]);
        assert!(matches!(input, Cow::Owned(_)));
    }

    #[test]
    fn architectural_input_borrows_the_audited_files_when_no_rule_needs_reference_text() {
        let code = files(vec![("src/a.ts", "export {};\n")]);
        let reference = files(vec![("index.html", "x")]);
        let other: HashSet<&str> = ["UI-UTC-DATES-1"].into_iter().collect();
        for armed in [HashSet::new(), other] {
            let input = architectural_input(&code, &reference, &armed);
            assert!(matches!(input, Cow::Borrowed(_)), "no copy without a need");
            assert_eq!(&*input, code.as_slice());
        }
    }

    #[test]
    fn architectural_input_borrows_when_there_is_no_reference_text() {
        let code = files(vec![("src/a.ts", "export {};\n")]);
        let input = architectural_input(&code, &[], &dead_code_armed());
        assert!(matches!(input, Cow::Borrowed(_)));
    }

    // ── withhold_on_incomplete_scan: a truncated scan cannot prove absence ────────

    fn ids(list: &[&'static str]) -> HashSet<&'static str> {
        list.iter().copied().collect()
    }

    #[test]
    fn a_truncated_scan_withholds_the_dead_code_rule_and_keeps_every_other_rule() {
        let armed = ids(&[
            camerata_checks::dead_code_checker::RULE_DEAD_CODE,
            "ARCH-ONE-IMPORT-PATH-1",
            "SUPABASE-RLS-ENABLED-1",
        ]);
        let kept = withhold_on_incomplete_scan(&armed, true);
        assert_eq!(
            kept,
            ids(&["ARCH-ONE-IMPORT-PATH-1", "SUPABASE-RLS-ENABLED-1"])
        );
    }

    #[test]
    fn a_complete_scan_withholds_nothing() {
        let armed = ids(&[
            camerata_checks::dead_code_checker::RULE_DEAD_CODE,
            "ARCH-ONE-IMPORT-PATH-1",
        ]);
        assert_eq!(withhold_on_incomplete_scan(&armed, false), armed);
    }

    /// The reason for the withholding: with an importer missing from the scan, a file that is
    /// in fact used looks unreachable. Same files, one armed set with the rule and one without.
    #[test]
    fn a_withheld_dead_code_rule_does_not_run_and_so_cannot_report_a_used_file_as_dead() {
        let f = files(vec![
            ("src/app.ts", "export {};\n"),
            ("src/used_elsewhere.ts", "export const u = 1;\n"),
        ]);
        let armed = dead_code_armed();
        let ran = audit_architectural("acme/app", &f, &armed);
        assert_eq!(
            ran.len(),
            1,
            "the checker reports the file it cannot see used: {ran:#?}"
        );
        let withheld = withhold_on_incomplete_scan(&armed, true);
        assert!(audit_architectural("acme/app", &f, &withheld).is_empty());
    }

    /// The ledger must not credit a run that did not happen: with the rule withheld from the
    /// credited detector set, the rule has no detector channel for this repo and falls to the
    /// config-gated disclosure instead of reading as "verified clean".
    #[test]
    fn a_withheld_rule_is_not_credited_as_a_detector_run_in_the_ledger() {
        let id = camerata_checks::dead_code_checker::RULE_DEAD_CODE;
        let code = files(vec![("src/app.ts", "export {};\n")]);
        let view = RepoView {
            spec: "acme/app",
            files: &code,
        };
        let credited = camerata_checks::arch_checker::ledger_detector_rule_ids_for_repo(&view);
        let none = HashSet::new();
        assert_eq!(
            crate::mechanical_gate::detector_channel(id, &credited, &none, None),
            Some("arch_checker"),
            "a complete scan credits the checker"
        );
        let withheld = withhold_on_incomplete_scan(&credited, true);
        assert_eq!(
            crate::mechanical_gate::detector_channel(id, &withheld, &none, None),
            None,
            "a truncated scan does not"
        );
        assert!(camerata_checks::arch_checker::any_registered_checker_answers(id));
    }

    #[test]
    fn the_reference_text_tier_changes_what_the_dead_code_rule_can_prove() {
        let code = files(vec![
            ("src/app.ts", "export {};\n"),
            (
                "src/composables/useThing.ts",
                "export const useThing = 1;\n",
            ),
        ]);
        let reference = files(vec![(
            "src/App.vue",
            "<script setup>\nimport { useThing } from './composables/useThing'\n</script>\n",
        )]);
        let armed = dead_code_armed();
        // Without reference text the component's import is invisible: a false "dead".
        assert_eq!(audit_architectural("acme/app", &code, &armed).len(), 1);
        // With it, the file is mentioned by a live file and stays silent.
        let input = architectural_input(&code, &reference, &armed);
        assert!(audit_architectural("acme/app", &input, &armed).is_empty());
    }

    // ── UI-REACT-HOOKS-LINT-1: the lint-configuration gap, through the scan seam ──────────

    const HOOKS_LINT: &str = camerata_checks::react_hooks_lint_checker::RULE_REACT_HOOKS_LINT;
    const REACT_APP: &str = r#"{ "name": "app", "dependencies": { "react": "^18.2.0" } }"#;

    fn hooks_lint_armed() -> HashSet<&'static str> {
        ids(&[HOOKS_LINT])
    }

    #[test]
    fn a_react_repo_with_no_lint_configuration_is_reported_through_the_architectural_tier() {
        let f = files(vec![
            ("package.json", REACT_APP),
            ("src/App.jsx", "export {};\n"),
        ]);
        let findings = audit_architectural("acme/web", &f, &hooks_lint_armed());
        assert_eq!(findings.len(), 1, "{findings:#?}");
        let finding = &findings[0];
        assert_eq!(finding.rule_id, HOOKS_LINT);
        assert_eq!(finding.path, "package.json");
        assert_eq!(finding.preview_tool.as_deref(), Some(ARCH_PREVIEW_TOOL));
        assert_eq!(
            finding.kind,
            Some(camerata_api_types::audit::FindingKind::Defect)
        );
        assert!(finding.detail.contains("react-hooks/exhaustive-deps"));
    }

    #[test]
    fn the_hooks_lint_gap_is_not_reported_unless_the_rule_is_armed() {
        let f = files(vec![("package.json", REACT_APP)]);
        assert!(audit_architectural("acme/web", &f, &HashSet::new()).is_empty());
        assert!(audit_architectural("acme/web", &f, &dead_code_armed()).is_empty());
    }

    #[test]
    fn the_hooks_lint_gap_is_silent_for_enabled_unreadable_and_non_react_repos() {
        for f in [
            files(vec![
                ("package.json", REACT_APP),
                (
                    ".eslintrc.json",
                    r#"{ "extends": ["plugin:react-hooks/recommended"] }"#,
                ),
            ]),
            files(vec![
                ("package.json", REACT_APP),
                ("eslint.config.mjs", "export default [];\n"),
            ]),
            files(vec![(
                "package.json",
                r#"{ "name": "api", "dependencies": { "express": "4" } }"#,
            )]),
        ] {
            assert!(
                audit_architectural("acme/web", &f, &hooks_lint_armed()).is_empty(),
                "{f:?}"
            );
        }
    }

    /// The reason for the withholding: the lint configuration may be among the files a
    /// truncated scan dropped, and without it the repository looks like it has none.
    #[test]
    fn a_truncated_scan_withholds_the_hooks_lint_rule_which_would_otherwise_report_a_false_gap() {
        let armed = ids(&[HOOKS_LINT, "ARCH-ONE-IMPORT-PATH-1"]);
        assert!(is_withheld_on_incomplete_scan(HOOKS_LINT));
        assert_eq!(
            withhold_on_incomplete_scan(&armed, true),
            ids(&["ARCH-ONE-IMPORT-PATH-1"])
        );
        assert_eq!(withhold_on_incomplete_scan(&armed, false), armed);

        // The scan sees only the manifest (the config was past the file limit): a false gap.
        let f = files(vec![
            ("package.json", REACT_APP),
            ("src/App.jsx", "export {};\n"),
        ]);
        assert_eq!(audit_architectural("acme/web", &f, &armed).len(), 1);
        let withheld = withhold_on_incomplete_scan(&armed, true);
        assert!(audit_architectural("acme/web", &f, &withheld).is_empty());
    }

    /// An unreadable lint setup is not a verified answer: the ledger must not credit the rule
    /// as a clean detector run for it, while a gap and an enabled configuration are real answers.
    #[test]
    fn the_ledger_credits_the_hooks_lint_rule_only_for_a_repo_the_checker_could_answer() {
        let credited = |f: &[(String, String)]| {
            camerata_checks::arch_checker::ledger_detector_rule_ids_for_repo(&RepoView {
                spec: "acme/web",
                files: f,
            })
            .contains(HOOKS_LINT)
        };
        let component = ("src/App.jsx", "export {};\n");
        assert!(
            credited(&files(vec![("package.json", REACT_APP), component])),
            "a gap is an answer"
        );
        assert!(credited(&files(vec![
            ("package.json", REACT_APP),
            component,
            (".eslintrc.json", r#"{ "extends": "next/core-web-vitals" }"#),
        ])));
        assert!(!credited(&files(vec![
            ("package.json", REACT_APP),
            component,
            ("eslint.config.js", "export default [];\n"),
        ])));
        // A `react` dependency with no React code is not assessed, never "verified clean".
        assert!(!credited(&files(vec![
            ("package.json", REACT_APP),
            ("src/index.ts", "export const plugin = 1;\n"),
        ])));
        assert!(!credited(&files(vec![("src/main.rs", "fn main() {}\n")])));
    }
}
