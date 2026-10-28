//! Rank 7: the real corpus's `applies_to` declarations. Every scoped rule is a possible recall
//! loss, so the set is pinned: adding a scope to another rule fails this test until the rule is
//! reviewed and added to `REVIEWED`, with a path that must still be sent to it.
//!
//! No model, no network: this drives the production router (`scan_routing`) over the production
//! corpus loader.

use camerata_server::scan_routing::{audit_rules, files_for_rule};

/// Every rule that declares `applies_to`, with a path the rule MUST still be sent (a file the
/// rule's own text says it reads) and a path it must NOT be sent (the saving).
const REVIEWED: &[(&str, &str, &str)] = &[
    // SQL statement rules: CREATE POLICY / FUNCTION / VIEW / TABLE statements in SQL files.
    (
        "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
        "db/migrations/0003_fix.sql",
        "src/app.tsx",
    ),
    (
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/20240101_init.sql",
        "src/app.tsx",
    ),
    (
        "SUPABASE-RLS-INITPLAN-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-RLS-NO-POLICY-1",
        "supabase/schemas/public.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-RLS-PERMISSIVE-TRUE-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-RLS-POLICY-DISABLED-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-RLS-USER-METADATA-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-RLS-VIEW-INVOKER-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-EXPOSURE-MATVIEW-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-AUTH-USERS-EXPOSED-1",
        "supabase/migrations/20240101_init.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
        "supabase/migrations/20240101_fn.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
        "supabase/migrations/20240101_fn.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
        "supabase/migrations/20240101_fn.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-FUNC-SEARCH-PATH-1",
        "supabase/migrations/20240101_fn.sql",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-STORAGE-OBJECT-POLICY-1",
        "supabase/migrations/20240101_storage.sql",
        "src/lib/db.ts",
    ),
    // Config rules: a value in supabase/config.toml.
    (
        "SUPABASE-AUTH-SESSION-LIFETIME-1",
        "supabase/config.toml",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
        "supabase/config.toml",
        "src/lib/db.ts",
    ),
    (
        "SUPABASE-EXPOSURE-SCHEMAS-1",
        "apps/web/supabase/config.toml",
        "src/lib/db.ts",
    ),
    // GitHub Actions rules: workflow and action files.
    (
        "CICD-PIN-THIRD-PARTY-ACTIONS-1",
        ".github/workflows/ci.yml",
        "src/lib/db.ts",
    ),
    (
        "CICD-LEAST-PRIVILEGE-TOKEN-1",
        ".github/workflows/release.yaml",
        "src/lib/db.ts",
    ),
    (
        "CICD-NO-SECRETS-ON-FORK-PR-1",
        ".github/actions/setup/action.yml",
        "src/lib/db.ts",
    ),
];

async fn corpus() -> camerata_rules::RuleSet {
    let (set, errors) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(
        errors.is_empty(),
        "the corpus must load cleanly, so no applies_to value is invalid: {errors:?}"
    );
    set
}

#[tokio::test]
async fn the_scoped_rules_are_exactly_the_reviewed_set() {
    let set = corpus().await;
    let mut declared: Vec<String> = set
        .iter()
        .filter(|r| r.applies_to.is_some())
        .map(|r| r.id.0.clone())
        .collect();
    declared.sort();
    let mut reviewed: Vec<String> = REVIEWED
        .iter()
        .map(|(id, _, _)| (*id).to_string())
        .collect();
    reviewed.sort();
    assert_eq!(
        declared, reviewed,
        "a rule gained or lost `applies_to`: review its recall risk, then update REVIEWED \
         (and the report's list of scoped rules)"
    );
}

#[tokio::test]
async fn every_scoped_rule_is_still_sent_the_files_its_text_reads_and_not_unrelated_code() {
    let set = corpus().await;
    for (id, must_send, must_skip) in REVIEWED {
        let rule = audit_rules(&[((*id).to_string(), "d".to_string())], Some(&set))
            .pop()
            .unwrap();
        assert!(
            rule.applies_to.is_some(),
            "{id} must carry its declared scope"
        );
        let files = vec![
            ((*must_send).to_string(), String::new()),
            ((*must_skip).to_string(), String::new()),
        ];
        let sent: Vec<&str> = files_for_rule(&rule, true, &files)
            .iter()
            .map(|f| f.0.as_str())
            .collect();
        assert_eq!(
            sent,
            vec![*must_send],
            "{id}: must be sent {must_send} (recall) and not {must_skip} (the saving)"
        );
        // Switch off: the same rule is sent everything.
        assert_eq!(files_for_rule(&rule, false, &files).len(), 2, "{id}");
    }
}

#[tokio::test]
async fn a_declared_scope_never_contradicts_the_rules_own_id_prefix_language() {
    // A `RUST-` rule scoped to only Python paths would match nothing: refuse that shape.
    let set = corpus().await;
    for rule in set.iter() {
        let Some(applies) = &rule.applies_to else {
            continue;
        };
        let by_id = camerata_server::scan_routing::rule_scope(&rule.id.0);
        if let camerata_server::scan_routing::Scope::Language(prefix) = by_id {
            assert!(
                applies.languages.is_empty() || applies.languages.iter().any(|l| l == prefix),
                "{}: id-prefix language {prefix} is not in its declared languages",
                rule.id.0
            );
        }
    }
}

#[tokio::test]
async fn no_scoped_rule_is_an_absence_rule_whose_finding_is_that_a_file_kind_is_missing() {
    // A rule scoped to workflow files gets zero files in a repo with none; it is then reported
    // not-assessed instead of reaching a finding about the absence. So no scoped rule may be one
    // whose conformance is "a scan, scanner or check EXISTS".
    const ABSENCE_RULES: &[&str] = &[
        "CICD-CODEQL-SECURITY-SCAN-1",
        "CICD-SEMGREP-SECURITY-SCAN-1",
        "CICD-DEPENDENCY-AUDIT-1",
        "CICD-PRIVILEGED-DEPLOY-GATE-1",
        "CICD-BUILD-PROVENANCE-1",
        "SUPABASE-AUTH-PASSWORD-POLICY-1",
        "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
    ];
    let set = corpus().await;
    for id in ABSENCE_RULES {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} missing from corpus"));
        assert!(
            rule.applies_to.is_none(),
            "{id} reasons from what is absent; scoping it would turn a missing-file finding into a \
             not-assessed row"
        );
    }
}
