//! Rank 7: the real corpus's `applies_to` declarations. Every scoped rule is a possible recall
//! loss, so the set is pinned: adding a scope to another rule fails this test until the rule is
//! reviewed and added to `REVIEWED`, with files that must still be sent to it.
//!
//! No model, no network: this drives the production router (`scan_routing`) over the production
//! corpus loader.

use camerata_server::scan_routing::{audit_rules, files_for_rule};

/// The three families of scoped rule, each with the files it MUST still be sent (the surfaces the
/// rule's own text reads, including the non-SQL migration tools that embed SQL) and a file it must
/// NOT be sent (the saving).
#[derive(Clone, Copy)]
enum Family {
    /// CREATE POLICY / FUNCTION / VIEW / TABLE statements.
    SqlStatements,
    /// A value in supabase/config.toml (or the Terraform that sets it).
    SupabaseConfig,
    /// Pipeline definition files of any CI system.
    CiPipelines,
}

impl Family {
    /// `(path, content)` pairs that must be sent.
    fn must_send(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Family::SqlStatements => vec![
                ("db/migrations/0003_fix.sql", "select 1;"),
                ("supabase/schemas/public.sql", ""),
                ("apps/web/supabase/config.toml", ""),
                // TypeORM / Knex / Kysely migration in TypeScript.
                (
                    "src/migrations/1700000000000-AddPolicy.ts",
                    "await queryRunner.query(`CREATE POLICY p ON t USING (true)`);",
                ),
                // Rails migration with a heredoc.
                (
                    "db/migrate/20240101120000_add_rls.rb",
                    "execute <<~SQL\n  ALTER TABLE t ENABLE ROW LEVEL SECURITY;\nSQL\n",
                ),
                // Alembic / Django RunSQL.
                (
                    "alembic/versions/0004_fn.py",
                    "op.execute(\"CREATE OR REPLACE FUNCTION f() RETURNS int SECURITY DEFINER ...\")",
                ),
                // A supabase-js admin script in a path no glob names: only the text admits it.
                (
                    "scripts/bootstrap.mjs",
                    "await sql`GRANT ALL ON storage.objects TO anon`;",
                ),
                // A Rust sqlx migration helper and a Go embed.
                ("src/store/boot.rs", "sqlx::query(\"create view v as select 1\")"),
                ("internal/db/setup.go", "const q = `CREATE MATERIALIZED VIEW m AS SELECT 1`"),
            ],
            Family::SupabaseConfig => vec![
                ("supabase/config.toml", ""),
                ("apps/web/supabase/config.toml", ""),
                ("infra/main.tf", "resource \"supabase_settings\" \"s\" {}"),
                ("infra/supabase.hcl", ""),
            ],
            Family::CiPipelines => vec![
                (".github/workflows/ci.yml", ""),
                (".github/actions/setup/action.yml", ""),
                (".gitea/workflows/ci.yaml", ""),
                (".forgejo/workflows/ci.yaml", ""),
                (".gitlab-ci.yml", ""),
                (".gitlab/ci/build.yml", ""),
                ("azure-pipelines.yml", ""),
                ("ci/azure-pipelines-release.yml", ""),
                (".circleci/config.yml", ""),
                ("bitbucket-pipelines.yml", ""),
                (".buildkite/pipeline.yml", ""),
                (".drone.yml", ""),
                (".woodpecker/build.yaml", ""),
                ("Jenkinsfile", "pipeline {}"),
            ],
        }
    }

    /// `(path, content)` that must NOT be sent.
    fn must_skip(self) -> (&'static str, &'static str) {
        (
            "src/lib/format.ts",
            "export const money = (n: number) => n;",
        )
    }
}

/// Every rule that declares `applies_to`, with its family.
const REVIEWED: &[(&str, Family)] = &[
    (
        "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
        Family::SqlStatements,
    ),
    ("SUPABASE-RLS-ENABLED-1", Family::SqlStatements),
    ("SUPABASE-RLS-INITPLAN-1", Family::SqlStatements),
    ("SUPABASE-RLS-NO-POLICY-1", Family::SqlStatements),
    ("SUPABASE-RLS-PERMISSIVE-TRUE-1", Family::SqlStatements),
    ("SUPABASE-RLS-POLICY-DISABLED-1", Family::SqlStatements),
    ("SUPABASE-RLS-USER-METADATA-1", Family::SqlStatements),
    ("SUPABASE-RLS-VIEW-INVOKER-1", Family::SqlStatements),
    ("SUPABASE-EXPOSURE-MATVIEW-1", Family::SqlStatements),
    ("SUPABASE-AUTH-USERS-EXPOSED-1", Family::SqlStatements),
    (
        "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
        Family::SqlStatements,
    ),
    (
        "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
        Family::SqlStatements,
    ),
    ("SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1", Family::SqlStatements),
    ("SUPABASE-FUNC-SEARCH-PATH-1", Family::SqlStatements),
    ("SUPABASE-STORAGE-OBJECT-POLICY-1", Family::SqlStatements),
    ("SUPABASE-AUTH-SESSION-LIFETIME-1", Family::SupabaseConfig),
    ("SUPABASE-AUTH-REDIRECT-ALLOWLIST-1", Family::SupabaseConfig),
    ("SUPABASE-EXPOSURE-SCHEMAS-1", Family::SupabaseConfig),
    ("CICD-PIN-THIRD-PARTY-ACTIONS-1", Family::CiPipelines),
    ("CICD-LEAST-PRIVILEGE-TOKEN-1", Family::CiPipelines),
    ("CICD-NO-SECRETS-ON-FORK-PR-1", Family::CiPipelines),
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
    let mut reviewed: Vec<String> = REVIEWED.iter().map(|(id, _)| (*id).to_string()).collect();
    reviewed.sort();
    assert_eq!(
        declared, reviewed,
        "a rule gained or lost `applies_to`: review its recall risk, then update REVIEWED \
         (and the report's list of scoped rules)"
    );
}

#[tokio::test]
async fn every_scoped_rule_is_still_sent_every_surface_its_text_reads_and_not_unrelated_code() {
    let set = corpus().await;
    for (id, family) in REVIEWED {
        let rule = audit_rules(&[((*id).to_string(), "d".to_string())], Some(&set))
            .pop()
            .unwrap();
        assert!(
            rule.applies_to.is_some(),
            "{id} must carry its declared scope"
        );
        for (path, content) in family.must_send() {
            let files = vec![((*path).to_string(), (*content).to_string())];
            assert_eq!(
                files_for_rule(&rule, true, &files).len(),
                1,
                "{id}: must be sent {path} (recall)"
            );
        }
        let (skip_path, skip_content) = family.must_skip();
        let skip = vec![(skip_path.to_string(), skip_content.to_string())];
        assert!(
            files_for_rule(&rule, true, &skip).is_empty(),
            "{id}: must not be sent {skip_path} (the saving)"
        );
        // Switch off: the same rule is sent everything.
        assert_eq!(files_for_rule(&rule, false, &skip).len(), 1, "{id}");
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
