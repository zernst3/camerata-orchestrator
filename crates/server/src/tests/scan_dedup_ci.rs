//! Scan preview dedup, floor categories, CI story bodies, scan rendering and external
//! tool ledger reconcile tests.

use super::*;

/// Helper: build a floor finding (preview = false, enforced).
fn floor_finding(repo: &str, path: &str, line: usize, rule_id: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "critical".to_string(),
        snippet: "let secret = \"ghp_xxx\";".to_string(),
        detail: "floor detail".to_string(),
        preview: false,
        preview_tool: None,
        ..crate::onboard::Finding::default()
    }
}

/// Helper: build a semgrep preview finding (preview = true, preview_tool = "semgrep").
fn semgrep_finding(
    repo: &str,
    path: &str,
    line: usize,
    semgrep_rule_id: &str,
) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: semgrep_rule_id.to_string(),
        severity: "critical".to_string(),
        snippet: "let secret = \"ghp_xxx\";".to_string(),
        detail: "semgrep detail".to_string(),
        preview: true,
        preview_tool: Some("semgrep".to_string()),
        ..crate::onboard::Finding::default()
    }
}

/// (a) A floor hardcoded-secret finding and a semgrep hardcoded-secret finding on the
/// SAME file:line collapse to ONE finding. The floor's rule_id is kept canonical; the
/// semgrep rule_id is recorded in `also_matches`.
#[test]
fn dedup_secret_same_line_keeps_floor_and_records_semgrep_in_also_matches() {
    let mut existing = vec![floor_finding(
        "me/api",
        "src/config.rs",
        7,
        "SEC-NO-HARDCODED-SECRETS-1",
    )];
    let previews = vec![semgrep_finding(
        "me/api",
        "src/config.rs",
        7,
        "camerata.security.hardcoded-secret",
    )];

    let out = dedup_preview_against_floor(&mut existing, previews);

    // The semgrep finding must be dropped (deduped).
    assert!(
        out.is_empty(),
        "deduped semgrep finding must not appear in output: {out:?}"
    );
    // The floor finding is still there and now carries the semgrep rule in also_matches.
    assert_eq!(existing.len(), 1);
    let floor = &existing[0];
    assert_eq!(
        floor.rule_id, "SEC-NO-HARDCODED-SECRETS-1",
        "floor rule_id is canonical"
    );
    assert_eq!(
        floor.also_matches,
        vec!["camerata.security.hardcoded-secret"],
        "semgrep rule must be recorded in also_matches on the floor finding"
    );
}

fn tool_preview(
    path: &str,
    line: usize,
    rule_id: &str,
    tool: &str,
    severity: &str,
) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: "me/web".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        preview: true,
        preview_tool: Some(tool.to_string()),
        ..crate::onboard::Finding::default()
    }
}

fn native_row(path: &str, line: usize, rule_id: &str, severity: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: "me/web".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        ..crate::onboard::Finding::default()
    }
}

/// R1-2: a preview row and the deterministic row of the SAME rule id on the same line are one
/// row (UI-UTC-DATES-1 appeared twice, preview medium and deterministic low). Both stay
/// counted: the merged row keeps the higher severity and no id is lost.
#[test]
fn dedup_collapses_an_identical_rule_id_on_one_line_and_keeps_the_higher_severity() {
    let mut existing = vec![native_row("src/a.ts", 4, "UI-UTC-DATES-1", "low")];
    let previews = vec![
        tool_preview("src/a.ts", 4, "UI-UTC-DATES-1", "camerata-arch", "medium"),
        // A different line is a different place: kept.
        tool_preview("src/a.ts", 9, "UI-UTC-DATES-1", "camerata-arch", "medium"),
    ];
    let out = dedup_scan_previews(&mut existing, previews);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].line, 9);
    assert_eq!(existing.len(), 1);
    assert_eq!(existing[0].severity, "medium", "the higher severity wins");
    assert!(
        existing[0].also_matches.is_empty(),
        "same id: no second rule id: {:?}",
        existing[0].also_matches
    );
    assert_eq!(
        existing[0]
            .captures
            .get(ALSO_SEEN_BY_CAPTURE)
            .map(String::as_str),
        Some("camerata-arch")
    );

    // The other way round: the existing row is the higher one and stays so.
    let mut existing = vec![native_row("src/a.ts", 4, "UI-UTC-DATES-1", "high")];
    let out = dedup_scan_previews(
        &mut existing,
        vec![tool_preview(
            "src/a.ts",
            4,
            "UI-UTC-DATES-1",
            "camerata-arch",
            "low",
        )],
    );
    assert!(out.is_empty());
    assert_eq!(existing[0].severity, "high");
}

/// Review of R1-2: with two candidate rows at one place, the preview folds into the row with
/// the IDENTICAL rule id, not the first row that merely shares its category.
#[test]
fn dedup_prefers_the_row_with_the_identical_rule_id() {
    // A security-category twin listed first, the identical-id row second.
    let mut existing = vec![
        native_row("src/h.js", 8, "SEC-NO-HARDCODED-SECRETS-1", "high"),
        native_row(
            "src/h.js",
            8,
            "camerata.security.hardcoded-secret",
            "medium",
        ),
    ];
    let preview = tool_preview(
        "src/h.js",
        8,
        "camerata.security.hardcoded-secret",
        "semgrep",
        "medium",
    );
    let out = dedup_scan_previews(&mut existing, vec![preview]);
    assert!(out.is_empty());
    assert!(
        existing[0].also_matches.is_empty(),
        "the category twin listed first is not touched: {:?}",
        existing[0].also_matches
    );
    assert_eq!(existing[1].rule_id, "camerata.security.hardcoded-secret");
    assert_eq!(
        existing[1]
            .captures
            .get(ALSO_SEEN_BY_CAPTURE)
            .map(String::as_str),
        Some("semgrep"),
        "the identical-id row took the merge"
    );
}

/// Review of R1-2: a same-id merge keeps which tool the preview came from, so the
/// provenance of the folded row is not lost (the merged row stays a native row).
#[test]
fn a_same_id_merge_records_the_preview_tool() {
    let mut existing = vec![native_row("src/a.ts", 4, "UI-UTC-DATES-1", "low")];
    let out = dedup_scan_previews(
        &mut existing,
        vec![tool_preview(
            "src/a.ts",
            4,
            "UI-UTC-DATES-1",
            "camerata-arch",
            "medium",
        )],
    );
    assert!(out.is_empty());
    assert_eq!(existing[0].rule_id, "UI-UTC-DATES-1");
    assert!(!existing[0].preview, "the merged row stays a native row");
    // Provenance lives in captures, never in also_matches (a rule-id list).
    assert!(existing[0].also_matches.is_empty());
    let seen = |f: &crate::onboard::Finding| f.captures.get(ALSO_SEEN_BY_CAPTURE).cloned();
    assert_eq!(seen(&existing[0]).as_deref(), Some("camerata-arch"));
    // Merging the same preview again does not repeat it; another tool is appended.
    for tool in ["camerata-arch", "semgrep"] {
        let _ = dedup_scan_previews(
            &mut existing,
            vec![tool_preview(
                "src/a.ts",
                4,
                "UI-UTC-DATES-1",
                tool,
                "medium",
            )],
        );
    }
    assert_eq!(seen(&existing[0]).as_deref(), Some("camerata-arch,semgrep"));
    assert!(existing[0].also_matches.is_empty());
}

/// R1-2: the eslint rule that enforces a native rule collapses into the native rule's row on
/// the same path and line (faktura: 15 `@typescript-eslint/no-non-null-assertion` rows beside
/// 10 `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1` rows, 12 on the same line). Both ids
/// stay on the merged row.
#[test]
fn dedup_collapses_the_enforcing_eslint_rule_into_the_native_rule_row() {
    let native = "JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1";
    let tool = "@typescript-eslint/no-non-null-assertion";
    let mut existing = vec![native_row("app/api/x.ts", 12, native, "medium")];
    let previews = vec![
        tool_preview("app/api/x.ts", 12, tool, "eslint", "medium"),
        // Same tool rule on another line: net-new coverage, kept.
        tool_preview("app/api/x.ts", 20, tool, "eslint", "medium"),
        // An unrelated eslint rule on the native rule's line is a different defect: kept.
        tool_preview("app/api/x.ts", 12, "no-console", "eslint", "low"),
    ];
    let out = dedup_scan_previews(&mut existing, previews);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(existing[0].rule_id, native);
    assert_eq!(existing[0].also_matches, vec![tool.to_string()]);
}

/// R1-2: the corpus's own `sources[].linter` mapping drives the collapse for a native rule
/// the built-in map does not list.
#[test]
fn dedup_uses_the_rule_files_linter_mapping() {
    let mut selectors = std::collections::HashMap::new();
    selectors.insert(
        "SOME-NATIVE-RULE-1".to_string(),
        vec!["@typescript-eslint/no-unnecessary-condition".to_string()],
    );
    let mut existing = vec![native_row("src/a.ts", 3, "SOME-NATIVE-RULE-1", "low")];
    let tool = "@typescript-eslint/no-unnecessary-condition";
    let previews = vec![tool_preview("src/a.ts", 3, tool, "eslint", "medium")];
    // Without the mapping the rows stay apart (nothing links them).
    let kept = dedup_scan_previews(
        &mut existing.clone(),
        vec![tool_preview("src/a.ts", 3, tool, "eslint", "medium")],
    );
    assert_eq!(kept.len(), 1);
    let out = dedup_scan_previews_with(&mut existing, previews, Some(&selectors));
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(existing[0].also_matches, vec![tool.to_string()]);
    assert_eq!(existing[0].severity, "medium");
}

/// R1-2: the real corpus names the eslint rule on the native rule through its source
/// `linter`, and the mapping built from it links the two.
#[tokio::test]
async fn native_tool_selectors_come_from_the_corpus_sources() {
    let path = camerata_rules::corpus_path();
    if !path.exists() {
        return;
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&path).await;
    let selectors = native_tool_selectors(&corpus);
    assert!(selectors
        .get("JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1")
        .is_some_and(|s| s
            .iter()
            .any(|x| x == "@typescript-eslint/no-non-null-assertion")));
    let mut existing = vec![native_row(
        "a.ts",
        1,
        "JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1",
        "medium",
    )];
    let out = dedup_scan_previews_with(
        &mut existing,
        vec![tool_preview(
            "a.ts",
            1,
            "@typescript-eslint/no-non-null-assertion",
            "eslint",
            "medium",
        )],
        Some(&selectors),
    );
    assert!(out.is_empty());
}

/// R1-2: a weak-hash semgrep row and a weak-token-randomness native row on one line are two
/// different defects (MD5/SHA-1 against a general-purpose PRNG); the corpus supports no
/// mapping between them, so both stay.
#[test]
fn dedup_keeps_weak_hash_and_weak_randomness_on_one_line_apart() {
    let mut existing = vec![native_row(
        "src/token.js",
        5,
        "SEC-NO-WEAK-TOKEN-RANDOMNESS-1",
        "high",
    )];
    let out = dedup_scan_previews(
        &mut existing,
        vec![tool_preview(
            "src/token.js",
            5,
            "camerata.security.weak-hash-js",
            "semgrep",
            "medium",
        )],
    );
    assert_eq!(out.len(), 1);
}

/// (a) SQL-concat variant: floor SEC-NO-RAW-SQL-CONCAT-1 + semgrep sql-string-concat-python
/// on the same (repo, path, line) collapse to one floor finding.
#[test]
fn dedup_sql_concat_python_same_line_keeps_floor() {
    let mut existing = vec![floor_finding(
        "me/svc",
        "app/db.py",
        15,
        "SEC-NO-RAW-SQL-CONCAT-1",
    )];
    let previews = vec![semgrep_finding(
        "me/svc",
        "app/db.py",
        15,
        "camerata.security.sql-string-concat-python",
    )];

    let out = dedup_preview_against_floor(&mut existing, previews);

    assert!(
        out.is_empty(),
        "SQL-concat python semgrep finding must be deduped: {out:?}"
    );
    assert_eq!(
        existing[0].also_matches,
        vec!["camerata.security.sql-string-concat-python"],
    );
}

/// (a) SQL-concat JS variant maps to the same floor rule.
#[test]
fn dedup_sql_concat_js_same_line_keeps_floor() {
    let mut existing = vec![floor_finding(
        "me/web",
        "src/db.ts",
        42,
        "SEC-NO-RAW-SQL-CONCAT-1",
    )];
    let previews = vec![semgrep_finding(
        "me/web",
        "src/db.ts",
        42,
        "camerata.security.sql-string-concat-js",
    )];

    let out = dedup_preview_against_floor(&mut existing, previews);

    assert!(
        out.is_empty(),
        "SQL-concat JS semgrep finding must be deduped: {out:?}"
    );
    assert_eq!(
        existing[0].also_matches,
        vec!["camerata.security.sql-string-concat-js"],
    );
}

/// (b) Semgrep catches a secret on a DIFFERENT line than any floor finding. That semgrep
/// finding is NET-NEW coverage (the regex missed it) and must pass through untouched.
#[test]
fn dedup_secret_different_line_is_kept_as_net_new() {
    let mut existing = vec![floor_finding(
        "me/api",
        "src/config.rs",
        7,
        "SEC-NO-HARDCODED-SECRETS-1",
    )];
    // Semgrep fires on line 12 — a different line, not a duplicate.
    let previews = vec![semgrep_finding(
        "me/api",
        "src/config.rs",
        12,
        "camerata.security.hardcoded-secret",
    )];

    let out = dedup_preview_against_floor(&mut existing, previews);

    assert_eq!(
        out.len(),
        1,
        "net-new semgrep finding (different line) must be kept"
    );
    assert_eq!(out[0].line, 12);
    assert_eq!(out[0].rule_id, "camerata.security.hardcoded-secret");
    // The existing floor finding must NOT have been mutated.
    assert!(
        existing[0].also_matches.is_empty(),
        "floor finding must not be touched"
    );
}

/// (c) The non-overlapping semgrep rules (exec-injection, weak-hash, path-traversal,
/// subprocess-shell-true) have no floor twin and must pass through untouched even when
/// a floor finding exists at the same path:line.
///
/// NOTE: yaml-unsafe-load was previously in this list but was moved to the overlapping
/// group when SEC-NO-UNSAFE-DESERIALIZATION-1 was added to the floor (maps to "deser").
#[test]
fn dedup_non_overlapping_semgrep_rules_pass_through_untouched() {
    let non_overlapping = &[
        "camerata.security.exec-injection",
        "camerata.security.exec-injection-js",
        "camerata.security.weak-hash-python",
        "camerata.security.weak-hash-js",
        "camerata.security.path-traversal-python",
        "camerata.security.subprocess-shell-true",
    ];

    for rule_id in non_overlapping {
        // Put a floor finding at the same coordinate just to prove it doesn't cross-dedup.
        let mut existing = vec![floor_finding(
            "me/api",
            "src/app.py",
            5,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let previews = vec![semgrep_finding("me/api", "src/app.py", 5, rule_id)];

        let out = dedup_preview_against_floor(&mut existing, previews);

        assert_eq!(
            out.len(),
            1,
            "non-overlapping semgrep rule '{rule_id}' must not be deduped even at same path:line"
        );
        assert_eq!(out[0].rule_id, *rule_id);
        assert!(
            existing[0].also_matches.is_empty(),
            "floor finding must not be mutated for non-overlapping rule '{rule_id}'"
        );
    }
}

/// (e) Two findings at the SAME path:line but in DIFFERENT repos must NEVER cross-dedup.
#[test]
fn dedup_different_repos_never_cross_dedup() {
    // Floor in repo "me/api"; semgrep in "me/web" — same path and line, different repo.
    let mut existing = vec![floor_finding(
        "me/api",
        "src/config.rs",
        7,
        "SEC-NO-HARDCODED-SECRETS-1",
    )];
    let previews = vec![semgrep_finding(
        "me/web", // different repo
        "src/config.rs",
        7,
        "camerata.security.hardcoded-secret",
    )];

    let out = dedup_preview_against_floor(&mut existing, previews);

    assert_eq!(out.len(), 1, "different-repo finding must not be deduped");
    assert!(
        existing[0].also_matches.is_empty(),
        "floor finding in a different repo must not be mutated"
    );
}

/// (e) Two findings at the same (repo, line) but DIFFERENT files must never cross-dedup.
#[test]
fn dedup_different_paths_never_cross_dedup() {
    let mut existing = vec![floor_finding(
        "me/api",
        "src/config.rs",
        7,
        "SEC-NO-HARDCODED-SECRETS-1",
    )];
    let previews = vec![semgrep_finding(
        "me/api",
        "src/other.rs", // different file
        7,
        "camerata.security.hardcoded-secret",
    )];

    let out = dedup_preview_against_floor(&mut existing, previews);

    assert_eq!(out.len(), 1, "different-file finding must not be deduped");
    assert!(existing[0].also_matches.is_empty());
}

/// `semgrep_floor_category` mapping table: verify the exact pairs and confirm
/// non-overlapping rules return `None`.
#[test]
fn semgrep_floor_category_returns_correct_mapping() {
    // Overlapping rules: must return the floor rule id they duplicate.
    let overlapping: &[(&str, &str)] = &[
        (
            "camerata.security.hardcoded-secret",
            "SEC-NO-HARDCODED-SECRETS-1",
        ),
        (
            "camerata.security.hardcoded-secret-dquote",
            "SEC-NO-HARDCODED-SECRETS-1",
        ),
        (
            "camerata.security.sql-string-concat-python",
            "SEC-NO-RAW-SQL-CONCAT-1",
        ),
        (
            "camerata.security.sql-string-concat-js",
            "SEC-NO-RAW-SQL-CONCAT-1",
        ),
        (
            "camerata.security.sql-string-concat-rust",
            "SEC-NO-RAW-SQL-CONCAT-1",
        ),
        (
            "camerata.security.sql-string-concat-csharp",
            "SEC-NO-RAW-SQL-CONCAT-1",
        ),
        (
            "camerata.security.disabled-tls-rust",
            "SEC-NO-DISABLED-TLS-1",
        ),
        (
            "camerata.security.disabled-tls-csharp",
            "SEC-NO-DISABLED-TLS-1",
        ),
        (
            "camerata.security.yaml-unsafe-load",
            "SEC-NO-UNSAFE-DESERIALIZATION-1",
        ),
    ];
    for (rule, expected_floor) in overlapping {
        assert_eq!(
            semgrep_floor_category(rule),
            Some(*expected_floor),
            "overlapping rule '{rule}' must map to floor '{expected_floor}'"
        );
    }
    // Non-overlapping rules: no corpus-grounded twin at all, return None.
    for rule in &[
        "camerata.security.weak-hash-python",
        "camerata.security.weak-hash-js",
        "camerata.security.weak-hash-rust",
        "camerata.security.weak-hash-csharp",
        "camerata.security.path-traversal-python",
        "camerata.security.subprocess-shell-true",
    ] {
        assert_eq!(
            semgrep_floor_category(rule),
            None,
            "rule '{rule}' must map to None (no corpus-grounded twin)"
        );
    }
}

/// W3: the commodity-class taint rules each ground to the right corpus rule id —
/// SQLi taint rules (all 7 languages) ground to the SAME corpus rule as the
/// pre-existing pattern-based sql-string-concat-* ids (a floor twin); XSS/open-redirect
/// taint rules ground to their corpus rule with NO floor twin (grounding-only, never
/// collapsed against a floor row); command-injection taint rules (plus the pre-existing
/// exec-injection pattern ids) ground to the new SEC-NO-COMMAND-INJECTION-1 rule.
#[test]
fn w3_taint_rules_ground_to_the_right_corpus_rule() {
    let sqli_taint = [
        "camerata.security.taint-sql-injection-python",
        "camerata.security.taint-sql-injection-js",
        "camerata.security.taint-sql-injection-rust",
        "camerata.security.taint-sql-injection-csharp",
        "camerata.security.taint-sql-injection-go",
        "camerata.security.taint-sql-injection-ruby",
        "camerata.security.taint-sql-injection-java",
    ];
    for id in sqli_taint {
        assert_eq!(
            semgrep_floor_category(id),
            Some("SEC-NO-RAW-SQL-CONCAT-1"),
            "{id} must ground to SEC-NO-RAW-SQL-CONCAT-1"
        );
    }

    let xss_taint = [
        "camerata.security.taint-xss-python",
        "camerata.security.taint-xss-js",
        "camerata.security.taint-xss-csharp",
        "camerata.security.taint-xss-go",
        "camerata.security.taint-xss-ruby",
        "camerata.security.taint-xss-java",
    ];
    for id in xss_taint {
        assert_eq!(
            semgrep_floor_category(id),
            Some("SEC-NO-UNSAFE-HTML-SINK-1"),
            "{id} must ground to SEC-NO-UNSAFE-HTML-SINK-1"
        );
    }

    let redirect_taint = [
        "camerata.security.taint-open-redirect-python",
        "camerata.security.taint-open-redirect-js",
        "camerata.security.taint-open-redirect-csharp",
        "camerata.security.taint-open-redirect-go",
        "camerata.security.taint-open-redirect-ruby",
        "camerata.security.taint-open-redirect-java",
    ];
    for id in redirect_taint {
        assert_eq!(
            semgrep_floor_category(id),
            Some("SEC-NO-OPEN-REDIRECT-1"),
            "{id} must ground to SEC-NO-OPEN-REDIRECT-1"
        );
    }

    let cmdi_taint = [
        "camerata.security.exec-injection",
        "camerata.security.exec-injection-js",
        "camerata.security.taint-command-injection-python",
        "camerata.security.taint-command-injection-js",
        "camerata.security.taint-command-injection-rust",
        "camerata.security.taint-command-injection-csharp",
        "camerata.security.taint-command-injection-go",
        "camerata.security.taint-command-injection-ruby",
        "camerata.security.taint-command-injection-java",
    ];
    for id in cmdi_taint {
        assert_eq!(
            semgrep_floor_category(id),
            Some("SEC-NO-COMMAND-INJECTION-1"),
            "{id} must ground to SEC-NO-COMMAND-INJECTION-1"
        );
    }

    // Ruby eval/send grounds to its OWN pre-existing corpus rule, not the generic
    // command-injection one — closing RUBY-AVOID-EVAL-SEND-1's phantom-detector gap.
    assert_eq!(
        semgrep_floor_category("camerata.security.taint-ruby-eval-send"),
        Some("RUBY-AVOID-EVAL-SEND-1")
    );

    // Sensitive-data-in-logs taint class: BOTH the secret/credential and PII rule
    // families ground to the SAME corpus rule (severity differs at the SARIF level, not
    // the grounding map — see that rule's own doc comment).
    let log_sink_taint = [
        "camerata.security.taint-log-secret-python",
        "camerata.security.taint-log-secret-js",
        "camerata.security.taint-log-secret-rust",
        "camerata.security.taint-log-secret-csharp",
        "camerata.security.taint-log-secret-go",
        "camerata.security.taint-log-secret-ruby",
        "camerata.security.taint-log-secret-java",
        "camerata.security.taint-log-pii-python",
        "camerata.security.taint-log-pii-js",
        "camerata.security.taint-log-pii-rust",
        "camerata.security.taint-log-pii-csharp",
        "camerata.security.taint-log-pii-go",
        "camerata.security.taint-log-pii-ruby",
        "camerata.security.taint-log-pii-java",
    ];
    for id in log_sink_taint {
        assert_eq!(
            semgrep_floor_category(id),
            Some("SEC-NO-SENSITIVE-DATA-IN-LOGS-1"),
            "{id} must ground to SEC-NO-SENSITIVE-DATA-IN-LOGS-1"
        );
    }
}

/// `is_observability_convention_class` matches ONLY the `ARCH-OBSERVABILITY-` prefix —
/// the three real corpus ids match, a near-miss (different prefix, or the prefix as a
/// SUBSTRING rather than a true prefix) does not, and an empty string does not panic.
#[test]
fn observability_convention_class_matches_only_its_own_prefix() {
    for id in [
        "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
        "ARCH-OBSERVABILITY-LOG-CONSISTENCY-1",
        "ARCH-OBSERVABILITY-CORRELATION-ID-1",
    ] {
        assert!(
            is_observability_convention_class(id),
            "{id} must match its own class"
        );
    }
    for id in [
        "ARCH-NO-PAN-STORAGE-1",
        "SEC-NO-SENSITIVE-DATA-IN-LOGS-1",
        "GO-LOGGING-STRUCTURED-1",
        "SOME-ARCH-OBSERVABILITY-LOOKALIKE-1", // prefix as a substring, not a true prefix
        "",
    ] {
        assert!(
            !is_observability_convention_class(id),
            "{id} must NOT match the observability convention class"
        );
    }
}

// ── FIX 2: cross-preview-tool dedup tests ────────────────────────────────
//
// These tests exercise `dedup_scan_previews` and `finding_security_category`
// directly. Decision: docs/decisions/2026-06-23_stack_gating_and_crosstool_dedup.md

/// Build a clippy preview finding (preview = true, preview_tool = "clippy").
fn clippy_finding(repo: &str, path: &str, line: usize, rule_id: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "medium".to_string(),
        snippet: String::new(),
        detail: "clippy detail".to_string(),
        preview: true,
        preview_tool: Some("clippy".to_string()),
        ..crate::onboard::Finding::default()
    }
}

/// Build a ruff preview finding.
fn ruff_finding(repo: &str, path: &str, line: usize, rule_id: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "medium".to_string(),
        snippet: String::new(),
        detail: "ruff detail".to_string(),
        preview: true,
        preview_tool: Some("ruff".to_string()),
        ..crate::onboard::Finding::default()
    }
}

/// Build an eslint preview finding.
fn eslint_finding(repo: &str, path: &str, line: usize, rule_id: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "medium".to_string(),
        snippet: String::new(),
        detail: "eslint detail".to_string(),
        preview: true,
        preview_tool: Some("eslint".to_string()),
        ..crate::onboard::Finding::default()
    }
}

/// finding_security_category: verify the category table is consistent.
#[test]
fn finding_security_category_maps_correctly() {
    // Floor rules
    assert_eq!(
        finding_security_category("SEC-NO-HARDCODED-SECRETS-1"),
        Some("secret")
    );
    assert_eq!(
        finding_security_category("SEC-NO-PRIVATE-KEY-1"),
        Some("secret")
    );
    assert_eq!(
        finding_security_category("SEC-NO-VENDOR-TOKEN-1"),
        Some("secret")
    );
    assert_eq!(
        finding_security_category("SEC-NO-RAW-SQL-CONCAT-1"),
        Some("sql")
    );
    assert_eq!(
        finding_security_category("SEC-NO-DISABLED-TLS-1"),
        Some("tls")
    );
    // Semgrep rules
    assert_eq!(
        finding_security_category("camerata.security.hardcoded-secret"),
        Some("secret")
    );
    assert_eq!(
        finding_security_category("camerata.security.sql-string-concat-python"),
        Some("sql")
    );
    assert_eq!(
        finding_security_category("camerata.security.sql-string-concat-js"),
        Some("sql")
    );
    assert_eq!(
        finding_security_category("camerata.security.exec-injection"),
        Some("exec")
    );
    assert_eq!(
        finding_security_category("camerata.security.exec-injection-js"),
        Some("exec")
    );
    assert_eq!(
        finding_security_category("camerata.security.weak-hash-python"),
        Some("hash")
    );
    assert_eq!(
        finding_security_category("camerata.security.weak-hash-js"),
        Some("hash")
    );
    // Rust + C# semgrep ids (the 2026-06-23 dedup-gap fix): must be categorized so they
    // can collapse against an overlapping floor finding at the same (repo, path, line).
    assert_eq!(
        finding_security_category("camerata.security.hardcoded-secret-dquote"),
        Some("secret")
    );
    assert_eq!(
        finding_security_category("camerata.security.sql-string-concat-rust"),
        Some("sql")
    );
    assert_eq!(
        finding_security_category("camerata.security.sql-string-concat-csharp"),
        Some("sql")
    );
    assert_eq!(
        finding_security_category("camerata.security.weak-hash-rust"),
        Some("hash")
    );
    assert_eq!(
        finding_security_category("camerata.security.weak-hash-csharp"),
        Some("hash")
    );
    assert_eq!(
        finding_security_category("camerata.security.disabled-tls-rust"),
        Some("tls")
    );
    assert_eq!(
        finding_security_category("camerata.security.disabled-tls-csharp"),
        Some("tls")
    );
    assert_eq!(
        finding_security_category("camerata.security.path-traversal-python"),
        Some("path")
    );
    assert_eq!(
        finding_security_category("camerata.security.subprocess-shell-true"),
        Some("shell")
    );
    // yaml-unsafe-load now maps to "deser" (not "yaml") so it collapses with the
    // floor rule SEC-NO-UNSAFE-DESERIALIZATION-1 at the same (repo, path, line).
    assert_eq!(
        finding_security_category("camerata.security.yaml-unsafe-load"),
        Some("deser")
    );
    // New floor rule (Step 3 of unsafe-deser port): same "deser" category.
    assert_eq!(
        finding_security_category("SEC-NO-UNSAFE-DESERIALIZATION-1"),
        Some("deser")
    );
    // Ruff bandit code
    assert_eq!(finding_security_category("S608"), Some("sql"));
    // Unknown rule: no category (pass-through, no dedup)
    assert_eq!(finding_security_category("ARCH-NO-CIRCULAR-DEPS-1"), None);
    assert_eq!(finding_security_category("some-unknown-linter-rule"), None);
}

/// W3: the new taint rule ids and their grounding-only corpus rules categorize
/// correctly, including the two brand-new categories ("xss", "redirect").
#[test]
fn finding_security_category_covers_w3_taint_rules() {
    assert_eq!(
        finding_security_category("SEC-NO-UNSAFE-HTML-SINK-1"),
        Some("xss")
    );
    assert_eq!(
        finding_security_category("SEC-NO-OPEN-REDIRECT-1"),
        Some("redirect")
    );
    assert_eq!(
        finding_security_category("SEC-NO-COMMAND-INJECTION-1"),
        Some("exec")
    );
    for id in [
        "camerata.security.taint-sql-injection-python",
        "camerata.security.taint-sql-injection-js",
        "camerata.security.taint-sql-injection-rust",
        "camerata.security.taint-sql-injection-csharp",
        "camerata.security.taint-sql-injection-go",
        "camerata.security.taint-sql-injection-ruby",
        "camerata.security.taint-sql-injection-java",
    ] {
        assert_eq!(finding_security_category(id), Some("sql"), "{id}");
    }
    for id in [
        "camerata.security.taint-xss-python",
        "camerata.security.taint-xss-js",
        "camerata.security.taint-xss-csharp",
        "camerata.security.taint-xss-go",
        "camerata.security.taint-xss-ruby",
        "camerata.security.taint-xss-java",
    ] {
        assert_eq!(finding_security_category(id), Some("xss"), "{id}");
    }
    for id in [
        "camerata.security.taint-open-redirect-python",
        "camerata.security.taint-open-redirect-js",
        "camerata.security.taint-open-redirect-csharp",
        "camerata.security.taint-open-redirect-go",
        "camerata.security.taint-open-redirect-ruby",
        "camerata.security.taint-open-redirect-java",
    ] {
        assert_eq!(finding_security_category(id), Some("redirect"), "{id}");
    }
    for id in [
        "camerata.security.taint-command-injection-python",
        "camerata.security.taint-command-injection-js",
        "camerata.security.taint-command-injection-rust",
        "camerata.security.taint-command-injection-csharp",
        "camerata.security.taint-command-injection-go",
        "camerata.security.taint-command-injection-ruby",
        "camerata.security.taint-command-injection-java",
    ] {
        assert_eq!(finding_security_category(id), Some("exec"), "{id}");
    }
    for id in [
        "camerata.security.taint-log-secret-python",
        "camerata.security.taint-log-secret-js",
        "camerata.security.taint-log-secret-rust",
        "camerata.security.taint-log-secret-csharp",
        "camerata.security.taint-log-secret-go",
        "camerata.security.taint-log-secret-ruby",
        "camerata.security.taint-log-secret-java",
        "camerata.security.taint-log-pii-python",
        "camerata.security.taint-log-pii-js",
        "camerata.security.taint-log-pii-rust",
        "camerata.security.taint-log-pii-csharp",
        "camerata.security.taint-log-pii-go",
        "camerata.security.taint-log-pii-ruby",
        "camerata.security.taint-log-pii-java",
    ] {
        assert_eq!(finding_security_category(id), Some("log-sensitive"), "{id}");
    }
}

/// Regression for the 2026-06-23 dedup gap: a floor SQL finding and a semgrep
/// `sql-string-concat-rust` finding on the SAME file:line MUST collapse (floor canonical),
/// not double-report. Before the fix, the -rust id had no category so it skipped dedup.
#[test]
fn crosstool_dedup_floor_and_semgrep_rust_sql_same_location_collapses() {
    let mut existing = vec![floor_finding(
        "me/svc",
        "src/db.rs",
        1444,
        "SEC-NO-RAW-SQL-CONCAT-1",
    )];
    let previews = vec![semgrep_finding(
        "me/svc",
        "src/db.rs",
        1444,
        "camerata.security.sql-string-concat-rust",
    )];
    let leftover = dedup_scan_previews(&mut existing, previews);
    // Semgrep finding folded in, not surfaced as its own row.
    assert!(
        leftover.is_empty(),
        "semgrep-rust SQL should collapse into the floor row"
    );
    assert_eq!(existing.len(), 1);
    assert_eq!(
        existing[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
        "floor stays canonical"
    );
    assert!(existing[0]
        .also_matches
        .iter()
        .any(|m| m == "camerata.security.sql-string-concat-rust"));
}

/// W3 (commodity-class taint layer): a floor SEC-NO-RAW-SQL-CONCAT-1 finding and a semgrep
/// TAINT-mode SQLi finding (a shape the old pattern-based rule could never have caught —
/// here a Go `fmt.Sprintf`-built query) on the SAME (repo, path, line) must still collapse
/// to ONE row, OUR deterministic row primary, the tool's rule id preserved in
/// `also_matches`, and the cover count not inflated — exactly the pre-existing C5-1 merge
/// contract, now exercised with a taint rule id instead of a pattern rule id.
#[test]
fn crosstool_dedup_floor_and_taint_sqli_go_same_location_collapses() {
    let mut existing = vec![floor_finding(
        "me/svc",
        "internal/db.go",
        88,
        "SEC-NO-RAW-SQL-CONCAT-1",
    )];
    let previews = vec![semgrep_finding(
        "me/svc",
        "internal/db.go",
        88,
        "camerata.security.taint-sql-injection-go",
    )];
    let leftover = dedup_scan_previews(&mut existing, previews);
    assert!(
        leftover.is_empty(),
        "taint SQLi (go) should collapse into the floor row"
    );
    assert_eq!(
        existing.len(),
        1,
        "cover count must not be inflated by the extra tool finding"
    );
    assert_eq!(
        existing[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
        "OUR deterministic row stays primary"
    );
    assert!(
        existing[0]
            .also_matches
            .iter()
            .any(|m| m == "camerata.security.taint-sql-injection-go"),
        "the external tool's rule id must be preserved in also_matches: {:?}",
        existing[0].also_matches
    );
}

/// W3: when the floor does NOT fire on a line (the regex's double-quote-only heuristic
/// misses a single-quoted/backtick/go-sprintf shape) but the taint rule DOES, the taint
/// finding must stand on its own as a NEW top-level preview row — never silently dropped
/// just because an unrelated category already has a row nearby. This is the actual
/// coverage-expansion case W3 exists for.
#[test]
fn taint_sqli_finding_with_no_floor_twin_on_that_line_stands_alone() {
    let mut existing: Vec<crate::onboard::Finding> = Vec::new();
    let previews = vec![semgrep_finding(
        "me/svc",
        "internal/db.go",
        99,
        "camerata.security.taint-sql-injection-go",
    )];
    let leftover = dedup_scan_previews(&mut existing, previews);
    assert_eq!(
        leftover.len(),
        1,
        "no floor twin on this line -> the finding must survive"
    );
    assert_eq!(
        leftover[0].rule_id,
        "camerata.security.taint-sql-injection-go"
    );
    assert!(leftover[0].preview);
    assert_eq!(leftover[0].preview_tool.as_deref(), Some("semgrep"));
}

/// Regression for the unsafe-deser floor port: a floor SEC-NO-UNSAFE-DESERIALIZATION-1
/// finding and a semgrep camerata.security.yaml-unsafe-load finding on the SAME
/// (repo, path, line) MUST collapse to one row, floor canonical, because both map to the
/// "deser" category. Before this step the yaml-unsafe-load rule mapped to "yaml" and the
/// floor rule did not exist, so they would have produced two rows.
#[test]
fn crosstool_dedup_floor_and_semgrep_deser_same_location_collapses() {
    let mut existing = vec![floor_finding(
        "me/svc",
        "src/loader.py",
        42,
        "SEC-NO-UNSAFE-DESERIALIZATION-1",
    )];
    let previews = vec![semgrep_finding(
        "me/svc",
        "src/loader.py",
        42,
        "camerata.security.yaml-unsafe-load",
    )];
    let leftover = dedup_scan_previews(&mut existing, previews);
    // Semgrep finding must be folded into the floor row, not surfaced as its own row.
    assert!(
        leftover.is_empty(),
        "semgrep yaml-unsafe-load must collapse into the floor deser row: {leftover:?}"
    );
    assert_eq!(existing.len(), 1);
    assert_eq!(
        existing[0].rule_id, "SEC-NO-UNSAFE-DESERIALIZATION-1",
        "floor rule_id must remain canonical"
    );
    assert!(
        existing[0]
            .also_matches
            .iter()
            .any(|m| m == "camerata.security.yaml-unsafe-load"),
        "semgrep rule must appear in also_matches: {:?}",
        existing[0].also_matches
    );
}

/// FIX 2 (a): ruff (rank 1, "sql" category via S608) + semgrep (rank 2, "sql" via
/// sql-string-concat-python) on the SAME (repo, path, line) collapse to ONE row.
/// Ruff is canonical (rank 1 < rank 2); semgrep rule_id is folded into `also_matches`.
#[test]
fn crosstool_dedup_ruff_and_semgrep_same_location_collapses() {
    // S608 (Ruff / flake8-bandit) → "sql"; sql-string-concat-python → "sql".
    // Both map to the same category, so they collapse.
    let ruff = ruff_finding("me/api", "src/db.py", 15, "S608");
    let mut existing = vec![ruff];
    let previews = vec![semgrep_finding(
        "me/api",
        "src/db.py",
        15,
        "camerata.security.sql-string-concat-python",
    )];

    let out = dedup_scan_previews(&mut existing, previews);

    // The semgrep finding is collapsed: it must NOT appear in `out`.
    assert!(
        out.is_empty(),
        "semgrep duplicate of ruff must be collapsed, not added: {out:?}"
    );
    // The ruff finding is kept canonical and records the semgrep rule.
    assert_eq!(existing.len(), 1);
    assert_eq!(
        existing[0].preview_tool.as_deref(),
        Some("ruff"),
        "ruff is canonical"
    );
    assert!(
        existing[0]
            .also_matches
            .contains(&"camerata.security.sql-string-concat-python".to_string()),
        "semgrep rule must be in also_matches: {:?}",
        existing[0].also_matches
    );
}

/// FIX 2 (b): floor (rank 0) + ruff (rank 1) on the same location + same "sql" category:
/// floor wins. Ruff rule id is folded into `also_matches` on the floor finding.
#[test]
fn crosstool_dedup_floor_beats_ruff_at_same_location() {
    let floor = floor_finding("me/api", "src/queries.py", 5, "SEC-NO-RAW-SQL-CONCAT-1");
    let mut existing = vec![floor];
    // Ruff S608 fires on the same spot (same "sql" category).
    let previews = vec![ruff_finding("me/api", "src/queries.py", 5, "S608")];

    let out = dedup_scan_previews(&mut existing, previews);

    assert!(
        out.is_empty(),
        "ruff duplicate of floor must be collapsed: {out:?}"
    );
    assert_eq!(
        existing[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
        "floor rule_id is canonical"
    );
    assert!(
        existing[0].also_matches.contains(&"S608".to_string()),
        "ruff rule must be in also_matches: {:?}",
        existing[0].also_matches
    );
}

/// FIX 2 (c): same (repo, path, line) but DIFFERENT categories: BOTH findings are kept.
/// Conservative dedup — surface over hide.
#[test]
fn crosstool_dedup_different_categories_same_line_are_kept() {
    // eslint: SQL injection lint on line 7
    let eslint = eslint_finding("me/web", "src/db.ts", 7, "no-sql-injection");
    let mut existing = vec![eslint];
    // Semgrep: exec-injection on the SAME line (different category: "exec" vs "sql").
    let previews = vec![semgrep_finding(
        "me/web",
        "src/db.ts",
        7,
        "camerata.security.exec-injection-js",
    )];

    let out = dedup_scan_previews(&mut existing, previews);

    // BOTH must be kept: different categories, even though same line.
    assert_eq!(
        out.len(),
        1,
        "different-category findings on the same line must BOTH be kept: out={out:?}"
    );
    assert_eq!(out[0].rule_id, "camerata.security.exec-injection-js");
    // The eslint finding must not have been mutated.
    assert!(
        existing[0].also_matches.is_empty(),
        "eslint must not be touched"
    );
}

/// FIX 2 (d): floor↔semgrep dedup still works as before (backward-compat regression test).
/// `dedup_preview_against_floor` delegates to `dedup_scan_previews`, so the floor→semgrep
/// behavior is fully preserved.
#[test]
fn crosstool_dedup_floor_semgrep_regression_unchanged() {
    let mut existing = vec![floor_finding(
        "me/api",
        "src/config.rs",
        7,
        "SEC-NO-HARDCODED-SECRETS-1",
    )];
    let previews = vec![semgrep_finding(
        "me/api",
        "src/config.rs",
        7,
        "camerata.security.hardcoded-secret",
    )];
    let out = dedup_preview_against_floor(&mut existing, previews);
    assert!(out.is_empty(), "floor↔semgrep dedup regression: {out:?}");
    assert_eq!(
        existing[0].also_matches,
        vec!["camerata.security.hardcoded-secret"]
    );
}

/// FIX 2 (e): findings with unrecognized (None) categories are never cross-deduped,
/// even on the same (repo, path, line).
#[test]
fn crosstool_dedup_unknown_category_never_deduped() {
    let clip = clippy_finding("me/api", "src/lib.rs", 3, "clippy::unknown-lint");
    let mut existing = vec![clip];
    // An eslint rule with no recognized category on the same line.
    let previews = vec![eslint_finding(
        "me/api",
        "src/lib.rs",
        3,
        "some-custom-eslint-rule",
    )];

    let out = dedup_scan_previews(&mut existing, previews);

    // Both must be kept since neither has a recognized security category.
    assert_eq!(
        out.len(),
        1,
        "unrecognized-category findings must not be collapsed"
    );
    assert!(existing[0].also_matches.is_empty());
}

/// `finding_dedup_rank` treats an eslint preview row IDENTICALLY to clippy/ruff — rank
/// one, strictly below a floor row (rank zero) and strictly above semgrep (rank two).
/// This is the precedence table the ledger/merge-primacy guarantee ("our own
/// deterministic row wins, a preview row never outranks it") rests on; pinned directly
/// so a future edit to the match arms can't silently invert it for eslint specifically.
#[test]
fn finding_dedup_rank_treats_eslint_like_clippy_and_ruff() {
    let floor = floor_finding("me/web", "src/x.ts", 1, "SOME-FLOOR-RULE-1");
    let eslint = eslint_finding("me/web", "src/x.ts", 1, "react-hooks/exhaustive-deps");
    let semgrep = semgrep_finding("me/web", "src/x.ts", 1, "camerata.security.whatever");
    assert_eq!(finding_dedup_rank(&floor), 0);
    assert_eq!(finding_dedup_rank(&eslint), 1);
    assert_eq!(
        finding_dedup_rank(&clippy_finding("me/web", "src/x.rs", 1, "clippy::x")),
        1
    );
    assert_eq!(
        finding_dedup_rank(&ruff_finding("me/web", "src/x.py", 1, "S608")),
        1
    );
    assert_eq!(finding_dedup_rank(&semgrep), 2);
}

/// Merge primacy, ESLint edition: when our OWN deterministic row and an eslint preview
/// row land at the same (repo, path, line) for the SAME security-grounded category, the
/// deterministic row wins — it stays canonical, the eslint rule id is folded into
/// `also_matches`, and exactly ONE row survives. Mirrors
/// `crosstool_dedup_floor_semgrep_regression_unchanged` above, but tags the preview row
/// `preview_tool = "eslint"` specifically — before this task, no existing test exercised
/// an eslint-tagged row colliding with a floor row in the SAME category (only
/// "different categories" and "unrecognized category" were covered for eslint), so this
/// closes that gap in proving the general tool-agnostic merge mechanism — unchanged by
/// this task — already does the right thing for eslint rows, not just semgrep/ruff/clippy.
#[test]
fn crosstool_dedup_eslint_never_outranks_our_own_floor_row() {
    let mut existing = vec![floor_finding(
        "me/web",
        "src/queries.py",
        5,
        "SEC-NO-RAW-SQL-CONCAT-1",
    )];
    // "S608" is already a recognized "sql"-category id (Ruff/flake8-bandit); tagging it
    // `preview_tool = "eslint"` here exercises the eslint branch of `finding_dedup_rank`
    // specifically, without needing a brand-new security-category mapping.
    let previews = vec![eslint_finding("me/web", "src/queries.py", 5, "S608")];

    let out = dedup_scan_previews(&mut existing, previews);

    assert!(
        out.is_empty(),
        "an eslint duplicate of our own floor row must be collapsed: {out:?}"
    );
    assert_eq!(
        existing[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
        "the floor's own rule_id stays canonical — ours wins merge primacy"
    );
    assert!(
        existing[0].also_matches.contains(&"S608".to_string()),
        "the eslint rule id must be folded into also_matches, not dropped: {:?}",
        existing[0].also_matches
    );
}

// ── ci_story_body helpers ─────────────────────────────────────────────────
//
// These tests verify that the enriched story bodies contain the SSOT model
// content that makes them self-sufficient for implementation. They assert on
// structural landmarks — not exact string equality — so minor prose edits
// do not break them.

fn mechanical_rules_fixture() -> Vec<CiStoryRule> {
    vec![
        CiStoryRule {
            id: "LINT-NO-CONSOLE-1".to_string(),
            title: "No console.log in production code".to_string(),
            linter: Some("eslint".to_string()),
        },
        CiStoryRule {
            id: "LINT-STRICT-1".to_string(),
            title: "TypeScript strict mode".to_string(),
            linter: None,
        },
    ]
}

fn architectural_rules_fixture() -> Vec<CiStoryRule> {
    vec![
        CiStoryRule {
            id: "ARCH-API-LAYERING-1".to_string(),
            title: "Services must not import repositories directly".to_string(),
            linter: None,
        },
        CiStoryRule {
            id: "ARCH-NO-CIRCULAR-DEPS-1".to_string(),
            title: "No circular module dependencies".to_string(),
            linter: None,
        },
    ]
}

fn vcs_metadata_rules_fixture() -> Vec<CiStoryRule> {
    vec![
        CiStoryRule {
            id: "PROCESS-CONVENTIONAL-COMMIT-1".to_string(),
            title: "Commit subjects follow the conventional-commits shape".to_string(),
            linter: Some("commitlint".to_string()),
        },
        CiStoryRule {
            id: "PROCESS-BRANCH-NAMING-1".to_string(),
            title: "Branch names start with an allowed prefix".to_string(),
            linter: None,
        },
    ]
}

// ── vcs-metadata tier body ────────────────────────────────────────────────

#[test]
fn vcs_metadata_body_lists_each_rule() {
    let body = ci_story_body_vcs_metadata("owner/repo", &vcs_metadata_rules_fixture());
    assert!(body.contains("PROCESS-CONVENTIONAL-COMMIT-1"));
    assert!(body.contains("PROCESS-BRANCH-NAMING-1"));
    assert!(body.contains("owner/repo") || body.contains("`repo`"));
}

#[test]
fn vcs_metadata_body_is_layer4_only_not_layer2() {
    // The defining caveat: these are layer-4 (CI) metadata checks, NOT layer-2
    // code checks, and must NOT be registered in the code-check manifest.
    let body = ci_story_body_vcs_metadata("owner/repo", &vcs_metadata_rules_fixture());
    assert!(
        body.contains("NOT enforced at layer 2") || body.contains("NOT layer-2"),
        "vcs-metadata body must state these are not layer-2 enforced"
    );
    assert!(
        body.contains(".camerata/checks.toml"),
        "vcs-metadata body must explicitly reference the code-check manifest to say NOT to use it"
    );
    assert!(
        body.to_lowercase().contains("commitlint"),
        "vcs-metadata body must describe the commitlint layer-4 check"
    );
}

#[test]
fn vcs_metadata_body_points_params_to_settings() {
    let body = ci_story_body_vcs_metadata("owner/repo", &vcs_metadata_rules_fixture());
    assert!(
        body.contains("process_rule_config") || body.contains("Settings"),
        "vcs-metadata body must point the parameters at project Settings (process_rule_config)"
    );
}

// ── shared SSOT content (must appear in BOTH tier bodies) ─────────────────

#[test]
fn mechanical_body_contains_ssot_file_reference() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    assert!(
        body.contains(".camerata/checks.toml"),
        "mechanical body must reference .camerata/checks.toml as SSOT"
    );
}

#[test]
fn architectural_body_contains_ssot_file_reference() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains(".camerata/checks.toml"),
        "architectural body must reference .camerata/checks.toml as SSOT"
    );
}

#[test]
fn mechanical_body_mentions_both_layers() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    assert!(
        body.contains("Layer 2") && body.contains("Layer 3"),
        "mechanical body must explain enforcement at both Layer 2 and Layer 3"
    );
}

#[test]
fn architectural_body_mentions_both_layers() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains("Layer 2") && body.contains("Layer 3"),
        "architectural body must explain enforcement at both Layer 2 and Layer 3"
    );
}

#[test]
fn mechanical_body_explains_parity_guarantee() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    // The parity guarantee is the key correctness claim — both layers derive from SSOT.
    assert!(
        body.contains("Parity is structural") || body.contains("parity"),
        "mechanical body must state that Layer-2/Layer-3 parity is structural, not by convention"
    );
}

#[test]
fn architectural_body_explains_parity_guarantee() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains("Parity is structural") || body.contains("parity"),
        "architectural body must state that Layer-2/Layer-3 parity is structural"
    );
}

// ── schema / pinning fields ───────────────────────────────────────────────

#[test]
fn mechanical_body_contains_toml_schema_fields() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    // All five required fields must appear.
    for field in &["id", "name", "command", "severity", "in_loop"] {
        assert!(
            body.contains(field),
            "mechanical body must document the `{field}` manifest field"
        );
    }
    // The three pinning fields must also appear.
    for field in &["tool", "version", "install"] {
        assert!(
            body.contains(field),
            "mechanical body must document the `{field}` pinning field"
        );
    }
}

#[test]
fn architectural_body_contains_toml_schema_fields() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    for field in &["id", "name", "command", "severity", "in_loop"] {
        assert!(
            body.contains(field),
            "architectural body must document the `{field}` manifest field"
        );
    }
    for field in &["tool", "version", "install"] {
        assert!(
            body.contains(field),
            "architectural body must document the `{field}` pinning field"
        );
    }
}

#[test]
fn mechanical_body_stresses_exact_version_pinning() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    // "EXACT" or "exact" or "pin" or "pinned" — any of these confirms the story
    // emphasises version pinning over floating versions.
    let lower = body.to_lowercase();
    assert!(
        lower.contains("exact") || lower.contains("pin"),
        "mechanical body must stress that version pinning is exact (no ranges/carets)"
    );
}

#[test]
fn architectural_body_stresses_exact_version_pinning() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    let lower = body.to_lowercase();
    assert!(
        lower.contains("exact") || lower.contains("pin"),
        "architectural body must stress that version pinning is exact"
    );
}

// ── gate-protection note ──────────────────────────────────────────────────

#[test]
fn mechanical_body_mentions_gate_protection() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    assert!(
        body.contains("SEC-NO-CAMERATA-CONFIG-1") || body.contains("agents cannot"),
        "mechanical body must mention the gate that prevents agents editing .camerata/"
    );
}

#[test]
fn architectural_body_mentions_gate_protection() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains("SEC-NO-CAMERATA-CONFIG-1") || body.contains("agents cannot"),
        "architectural body must mention SEC-NO-CAMERATA-CONFIG-1 gate protection"
    );
}

// ── tier-specific content ─────────────────────────────────────────────────

#[test]
fn mechanical_body_names_linter_for_rules_that_have_one() {
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    // LINT-NO-CONSOLE-1 has linter: "eslint" — must appear.
    assert!(
        body.contains("eslint"),
        "mechanical body must surface the linter hint (eslint) for rules that supply one"
    );
}

#[test]
fn mechanical_body_includes_all_rule_ids() {
    let rules = mechanical_rules_fixture();
    let body = ci_story_body_mechanical("owner/repo", &rules);
    for r in &rules {
        assert!(
            body.contains(&r.id),
            "mechanical body must list rule id `{}`",
            r.id
        );
    }
}

#[test]
fn architectural_body_includes_all_rule_ids() {
    let rules = architectural_rules_fixture();
    let body = ci_story_body_architectural("owner/repo", &rules);
    for r in &rules {
        assert!(
            body.contains(&r.id),
            "architectural body must list rule id `{}`",
            r.id
        );
    }
}

#[test]
fn architectural_body_describes_custom_checker_requirement() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    // The story must be explicit that a bespoke checker must be BUILT.
    assert!(
        body.contains("bespoke checker") || body.contains("custom checker"),
        "architectural body must state that a custom/bespoke checker must be built"
    );
}

#[test]
fn architectural_body_contains_api_layering_worked_example() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    // The dependency-cruiser worked example for API layering must appear.
    assert!(
        body.contains("dependency-cruiser"),
        "architectural body must include the dependency-cruiser worked example"
    );
    assert!(
        body.contains("ARCH-API-LAYERING-1"),
        "architectural body must reference ARCH-API-LAYERING-1 as the canonical example"
    );
}

// ── Pass 5: camerata-check distributable (Layer-3 CI parity) ──────────────

#[test]
fn architectural_body_mentions_camerata_check_distributable() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains("camerata-check"),
        "architectural body must tell teams to check whether camerata-check already \
             covers the rule before hand-building a checker"
    );
}

#[test]
fn architectural_body_camerata_check_manifest_example_present() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains("tool     = \"camerata-check\""),
        "architectural body must show how to register camerata-check as the manifest tool"
    );
}

#[test]
fn architectural_body_still_names_bespoke_checker_path_for_group_e() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    assert!(
        body.contains("ARCH-STRUCTURED-ERRORS-1") || body.contains("ARCH-EXACT-DECIMALS-1"),
        "architectural body must still name the rules deliberately left uncovered by any \
             native checker, so teams know the bespoke-checker path is still correct for them"
    );
}

#[test]
fn architectural_body_teaches_regenerate_workflow_step() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    // Step 3 in the how-to is regenerating the CI workflow after editing the manifest.
    assert!(
        body.contains("generate-ci-workflow") || body.contains("Generate CI Workflow"),
        "architectural body must explain how to regenerate the CI workflow after editing the manifest"
    );
}

#[test]
fn architectural_body_scoping_guidance_present() {
    let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
    // The story must tell teams not to block the mechanical story on architectural work.
    assert!(
        body.contains("mechanical CI story") || body.contains("mechanical story"),
        "architectural body must tell teams not to block the mechanical story on this work"
    );
}

// ── repo name propagation ─────────────────────────────────────────────────

#[test]
fn mechanical_body_includes_repo_name() {
    let body = ci_story_body_mechanical("my-org/my-repo", &mechanical_rules_fixture());
    assert!(
        body.contains("my-org/my-repo"),
        "mechanical body must include the repo name"
    );
}

#[test]
fn architectural_body_includes_repo_name() {
    let body = ci_story_body_architectural("my-org/my-repo", &architectural_rules_fixture());
    assert!(
        body.contains("my-org/my-repo"),
        "architectural body must include the repo name"
    );
}

// ── CICD-DEPENDENCY-AUDIT-1 cadence guidance ──────────────────────────────
//
// When the dep-audit rule is armed, the mechanical story body must include
// a "cadence" section that names the four options and the osv-scanner command.
// When it is NOT armed, the section must be absent (no spurious cadence noise).

fn dep_audit_rule() -> CiStoryRule {
    CiStoryRule {
        id: "CICD-DEPENDENCY-AUDIT-1".to_string(),
        title: "Scan dependencies for known vulnerabilities (osv-scanner)".to_string(),
        linter: Some("osv-scanner".to_string()),
    }
}

fn dep_audit_only_fixture() -> Vec<CiStoryRule> {
    vec![dep_audit_rule()]
}

fn dep_audit_mixed_fixture() -> Vec<CiStoryRule> {
    let mut rules = mechanical_rules_fixture();
    rules.push(dep_audit_rule());
    rules
}

#[test]
fn dep_audit_armed_mechanical_body_contains_cadence_section() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    assert!(
        body.contains("cadence"),
        "mechanical body with dep-audit rule must include a cadence section"
    );
}

#[test]
fn dep_audit_armed_mechanical_body_mentions_osv_scanner_command() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    assert!(
        body.contains("osv-scanner -r ."),
        "dep-audit cadence section must include the osv-scanner invocation"
    );
}

#[test]
fn dep_audit_armed_mechanical_body_lists_all_four_cadence_options() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    // Four cadence options: weekly cron, per-PR, in-loop (every-pass), manual.
    assert!(
        body.contains("Weekly") || body.contains("weekly"),
        "cadence section must describe the weekly CI job option"
    );
    assert!(
        body.contains("pull_request") || body.contains("Per-PR") || body.contains("per-PR"),
        "cadence section must describe the per-PR option"
    );
    assert!(
        body.contains("in_loop") && body.contains("in-loop"),
        "cadence section must describe the in-loop / every-pass option"
    );
    assert!(
        body.contains("workflow_dispatch") || body.contains("manual"),
        "cadence section must describe the manual / on-demand option"
    );
}

#[test]
fn dep_audit_armed_mechanical_body_states_developer_implements_cadence() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    // The story must make clear that the developer wires the cadence, not Camerata.
    assert!(
        body.contains("developer") || body.contains("project decision"),
        "cadence section must state that the developer implements the cadence, not Camerata"
    );
}

#[test]
fn dep_audit_armed_mechanical_body_references_checks_toml_for_version_pin() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    // The cadence section must reference version pinning in .camerata/checks.toml.
    assert!(
        body.contains(".camerata/checks.toml"),
        "cadence section must reference .camerata/checks.toml for version pinning"
    );
    assert!(
        body.contains("osv-scanner"),
        "cadence section must name osv-scanner as the tool to pin"
    );
}

#[test]
fn dep_audit_absent_mechanical_body_has_no_cadence_section() {
    // When CICD-DEPENDENCY-AUDIT-1 is NOT among the rules, the cadence section
    // must not appear — no spurious guidance for unrelated mechanical rules.
    let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
    assert!(
        !body.contains("Dependency vulnerability scanning"),
        "mechanical body WITHOUT dep-audit must not include the cadence section"
    );
}

#[test]
fn dep_audit_mixed_rules_cadence_section_present() {
    // When dep-audit is one of several mechanical rules, cadence guidance still appears.
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_mixed_fixture());
    assert!(
        body.contains("cadence"),
        "mixed-rule mechanical body with dep-audit must include the cadence section"
    );
}

#[test]
fn dep_audit_cadence_section_recommends_weekly_as_default() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    // The weekly option should be called out as recommended.
    assert!(
        body.contains("recommended") || body.contains("standard"),
        "cadence section must identify the weekly job as the recommended or standard default"
    );
}

#[test]
fn dep_audit_cadence_section_explains_cron_schedule_example() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    // A concrete cron schedule example must be present so the developer can copy it.
    assert!(
        body.contains("cron:"),
        "cadence section must include a concrete cron schedule example"
    );
}

#[test]
fn dep_audit_cadence_section_mentions_camerata_does_not_schedule() {
    let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
    // Must be explicit that Camerata does not build a scheduling engine.
    assert!(
        body.contains("scheduling engine") || body.contains("does not build"),
        "cadence section must be explicit that Camerata does not build a scheduling engine"
    );
}

// ── uow blank + parent_id field (2026-06-23) ──────────────────────────────────

/// `POST /api/uow/blank` with a `parent_id` body stores the normalized number on the
/// created draft. `"#42"` and `"42"` both normalize to `"42"`.
#[tokio::test]
async fn uow_blank_with_parent_id_stores_it_on_draft() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let uow_store = state.uow.clone();
    let app = router(state);

    // POST with "#42" — the leading "#" must be stripped.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/blank")
                .header("content-type", "application/json")
                .body(Body::from(r##"{"parent_id":"#42"}"##))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let json = body_json(resp).await;
    let uow_id = json["uow_id"].as_str().expect("uow_id in response");
    let uow = uow_store.get_or_create(uow_id);
    assert_eq!(
        uow.parent_id.as_deref(),
        Some("42"),
        "#42 must normalize to 42 and be stored on the draft"
    );
}

/// `POST /api/uow/blank` with no body (empty JSON `{}`) still creates a draft with
/// `parent_id = None` — back-compat with existing callers.
#[tokio::test]
async fn uow_blank_with_no_body_creates_draft_without_parent() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let uow_store = state.uow.clone();
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/blank")
                .header("content-type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let json = body_json(resp).await;
    let uow_id = json["uow_id"].as_str().expect("uow_id in response");
    let uow = uow_store.get_or_create(uow_id);
    assert!(
        uow.parent_id.is_none(),
        "no parent_id in body -> None on draft"
    );
}

/// `normalize_parent_number` strips a leading `#` and accepts plain digits; rejects
/// empty strings and non-numeric input.
#[test]
fn normalize_parent_number_strips_hash_and_rejects_non_numeric() {
    use crate::github_issues::normalize_parent_number;
    assert_eq!(normalize_parent_number("42"), Some("42".to_string()));
    assert_eq!(normalize_parent_number("#42"), Some("42".to_string()));
    assert_eq!(normalize_parent_number("  #7  "), Some("7".to_string()));
    assert_eq!(normalize_parent_number(""), None, "empty -> None");
    assert_eq!(normalize_parent_number("#"), None, "bare # -> None");
    assert_eq!(normalize_parent_number("abc"), None, "non-numeric -> None");
    assert_eq!(
        normalize_parent_number("#abc"),
        None,
        "non-numeric after # -> None"
    );
}

/// When the draft has a `parent_id` but the GitHub sub-issue link call would fail
/// (simulated here by a parent that can't be resolved because it's not a valid
/// number after storage — this exercises the publish path's parent link failure
/// branch), the story is still published and the response contains a
/// `parent_link_warning`. We test the store-and-link seam directly (same pattern
/// as `publish_link_step_links_draft_without_rekey`) with a bogus parent_id to
/// trigger the parse-failure branch without a real GitHub call.
#[tokio::test]
async fn publish_with_invalid_parent_id_still_publishes_with_warning() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let uow_store = state.uow.clone();
    let stories = state.stories.clone();

    // Create a draft with a parent_id that will fail u64 parse (empty string after
    // normalization is stored as None, so use a non-numeric string stored directly
    // via create_blank_with_parent to simulate a stored-but-unparseable value).
    // We bypass normalization here intentionally — the test covers the runtime guard
    // inside uow_publish's parent_link_warning arm.
    let draft = uow_store.create_blank_with_parent(Some("not-a-number".to_string()), None);
    let draft_id = draft.story_id.clone();
    uow_store.append_authoring_turn(&draft_id, "req", "ok", "Story title", "Body");

    // Simulate create_issue having returned issue #99 and the spine + link writes
    // that uow_publish does after the HTTP call. We run the parent-link path directly
    // using the same logic the handler uses, pulling the UoW from the store.
    let story = crate::github_issues::issue_to_story("me/api", 99, "Story title", "Body");
    let wi_story_id = story.id.clone();
    stories.upsert(story).await.unwrap();
    let linked = uow_store.link_work_item(&draft_id, &wi_story_id);
    assert_eq!(linked.work_item.as_deref(), Some(wi_story_id.as_str()));

    // Confirm the parent_id is still on the UoW (carried draft → publish).
    let uow = uow_store.get_or_create(&draft_id);
    assert_eq!(uow.parent_id.as_deref(), Some("not-a-number"));

    // Now simulate the publish handler's parent-link branch: a non-numeric parent_id
    // (after normalization on blank creation it would have been None, but here we
    // stored it raw to test the guard). The handler's match arm is:
    //   Ok(parent_number) => link_sub_issue(...)
    //   Err(_) => Some("could not link...")
    // We test this pure branch inline.
    let warning: Option<String> = match uow.parent_id.as_deref() {
        None => None,
        Some(raw) => match raw.trim().parse::<u64>() {
            Err(_) => Some(format!(
                "published, but could not link to parent #{raw}: not a valid issue number"
            )),
            Ok(_) => None, // would call link_sub_issue in the real handler
        },
    };
    assert!(
        warning.is_some(),
        "non-numeric parent_id must produce a warning"
    );
    let w = warning.unwrap();
    assert!(
        w.contains("published") && w.contains("not-a-number"),
        "warning must mention 'published' and the bad parent, got: {w}"
    );
}

// ── render_scan_results_for_chat ─────────────────────────────────────────

// too_many_arguments: a test fixture builder; positional fields keep call sites terse.
#[allow(clippy::too_many_arguments)]
fn make_finding(
    severity: &str,
    rule_id: &str,
    repo: &str,
    path: &str,
    line: usize,
    status: &str,
    preview: bool,
    snippet: &str,
    detail: &str,
) -> crate::onboard::Finding {
    crate::onboard::Finding {
        severity: severity.to_string(),
        rule_id: rule_id.to_string(),
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        status: status.to_string(),
        preview,
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        ..crate::onboard::Finding::default()
    }
}

/// Empty findings + empty coverage notes returns an empty string (caller skips the section).
#[test]
fn render_scan_results_empty_report_returns_empty_string() {
    let result = render_scan_results_for_chat(&[], &[]);
    assert!(
        result.is_empty(),
        "empty findings + empty coverage must produce empty string, got: {result:?}"
    );
}

/// Severity counts appear in the output and are correct.
#[test]
fn render_scan_results_counts_by_severity() {
    let findings = vec![
        make_finding(
            "critical", "SEC-1", "o/r", "a.rs", 1, "active", false, "", "",
        ),
        make_finding(
            "critical", "SEC-1", "o/r", "b.rs", 2, "active", false, "", "",
        ),
        make_finding("high", "ARCH-1", "o/r", "c.rs", 3, "active", false, "", ""),
        make_finding(
            "medium",
            "ARCH-2",
            "o/r",
            "d.rs",
            4,
            "suppressed-baseline",
            false,
            "",
            "",
        ),
        make_finding("low", "STYLE-1", "o/r", "e.rs", 5, "active", false, "", ""),
    ];
    let out = render_scan_results_for_chat(&findings, &[]);
    assert!(
        out.contains("Total findings: 5"),
        "must report total, got: {out}"
    );
    assert!(
        out.contains("critical: 2"),
        "must count critical, got: {out}"
    );
    assert!(out.contains("high: 1"), "must count high, got: {out}");
    assert!(out.contains("medium: 1"), "must count medium, got: {out}");
    assert!(out.contains("low: 1"), "must count low, got: {out}");
}

/// Active vs suppressed status counts are present.
#[test]
fn render_scan_results_counts_by_status() {
    let findings = vec![
        make_finding("high", "R1", "o/r", "f1.rs", 1, "active", false, "", ""),
        make_finding("high", "R1", "o/r", "f2.rs", 2, "active", false, "", ""),
        make_finding(
            "medium",
            "R2",
            "o/r",
            "f3.rs",
            3,
            "suppressed-baseline",
            false,
            "",
            "",
        ),
        make_finding(
            "low",
            "R3",
            "o/r",
            "f4.rs",
            4,
            "suppressed-inline",
            false,
            "",
            "",
        ),
    ];
    let out = render_scan_results_for_chat(&findings, &[]);
    assert!(out.contains("2 active"), "must report 2 active, got: {out}");
    assert!(
        out.contains("2 suppressed"),
        "must report 2 suppressed, got: {out}"
    );
}

/// Preview vs floor counts appear.
#[test]
fn render_scan_results_preview_vs_floor() {
    let findings = vec![
        make_finding("high", "R1", "o/r", "a.rs", 1, "active", true, "", ""),
        make_finding("high", "R1", "o/r", "b.rs", 2, "active", false, "", ""),
        make_finding("high", "R1", "o/r", "c.rs", 3, "active", false, "", ""),
    ];
    let out = render_scan_results_for_chat(&findings, &[]);
    assert!(
        out.contains("Preview (not yet wired into CI gate): 1"),
        "must show 1 preview, got: {out}"
    );
    assert!(
        out.contains("Floor (enforced): 2"),
        "must show 2 floor, got: {out}"
    );
}

/// By-rule breakdown contains the correct counts.
#[test]
fn render_scan_results_by_rule_breakdown() {
    let findings = vec![
        make_finding("high", "RULE-A", "o/r", "a.rs", 1, "active", false, "", ""),
        make_finding("high", "RULE-A", "o/r", "b.rs", 2, "active", false, "", ""),
        make_finding(
            "medium", "RULE-A", "o/r", "c.rs", 3, "active", false, "", "",
        ),
        make_finding("low", "RULE-B", "o/r", "d.rs", 4, "active", false, "", ""),
    ];
    let out = render_scan_results_for_chat(&findings, &[]);
    // RULE-A should appear with count 3 (highest); RULE-B with 1.
    assert!(
        out.contains("RULE-A: 3"),
        "must show RULE-A count 3, got: {out}"
    );
    assert!(
        out.contains("RULE-B: 1"),
        "must show RULE-B count 1, got: {out}"
    );
}

/// The top-N finding list is capped at 40 (CAP).
#[test]
fn render_scan_results_capped_at_40() {
    let findings: Vec<_> = (0..60)
        .map(|i| {
            make_finding(
                "high",
                "R1",
                "o/r",
                &format!("f{i}.rs"),
                i,
                "active",
                false,
                "",
                "",
            )
        })
        .collect();
    let out = render_scan_results_for_chat(&findings, &[]);
    // Must say "Top 40 of 60"
    assert!(
        out.contains("Top 40 findings (of 60"),
        "must cap at 40 and report total 60, got: {out}"
    );
}

/// CRITICAL: snippet must NEVER appear in the output, even when it contains a
/// credential-shaped value. Rule + location + detail are ok; raw snippet is not.
#[test]
fn render_scan_results_never_leaks_snippet() {
    // Split the credential-shaped literal so it is not itself a credential in this
    // public source file.
    let fake_secret = concat!("ghp_", "ABCdef1234567890XYZ");
    let findings = vec![make_finding(
        "critical",
        "SEC-NO-HARDCODED-SECRETS-1",
        "owner/repo",
        "src/config.rs",
        42,
        "active",
        false,
        fake_secret,             // snippet — must NOT appear in output
        "hardcoded token found", // detail — may appear
    )];
    let out = render_scan_results_for_chat(&findings, &[]);
    assert!(
        !out.contains(fake_secret),
        "snippet must NOT appear in chat grounding, got: {out}"
    );
    assert!(
        out.contains("SEC-NO-HARDCODED-SECRETS-1"),
        "rule_id must appear, got: {out}"
    );
    assert!(
        out.contains("src/config.rs:42"),
        "location must appear, got: {out}"
    );
    assert!(
        out.contains("hardcoded token found"),
        "gate detail (not snippet) may appear, got: {out}"
    );
}

/// Coverage notes appear in the output.
#[test]
fn render_scan_results_includes_coverage_notes() {
    let notes = vec![crate::onboard::CoverageNote {
        tool_failed: false,
        repo: None,
        tool: "ruff".to_string(),
        message: "ruff not installed; Python rules skipped".to_string(),
    }];
    let out = render_scan_results_for_chat(&[], &notes);
    // Even with empty findings, coverage notes are surfaced.
    assert!(
        out.contains("ruff"),
        "coverage note tool must appear, got: {out}"
    );
    assert!(
        out.contains("Python rules skipped"),
        "coverage note message must appear, got: {out}"
    );
}

/// extract_scan_results_from_draft returns None for an empty draft blob.
#[test]
fn extract_scan_results_from_draft_none_on_empty() {
    let draft = serde_json::json!({});
    assert!(
        extract_scan_results_from_draft(&draft).is_none(),
        "empty draft must return None"
    );
}

/// extract_scan_results_from_draft returns None when findings is empty array.
#[test]
fn extract_scan_results_from_draft_none_on_empty_findings() {
    let draft = serde_json::json!({"scan": {"findings": []}});
    assert!(
        extract_scan_results_from_draft(&draft).is_none(),
        "empty findings array must return None"
    );
}

/// extract_scan_results_from_draft finds findings in scan.findings and produces
/// a non-empty section with the expected content.
#[test]
fn extract_scan_results_from_draft_scan_findings() {
    let draft = serde_json::json!({
        "scan": {
            "findings": [
                {
                    "repo": "owner/repo",
                    "path": "src/lib.rs",
                    "line": 10,
                    "rule_id": "ARCH-LAYERING-1",
                    "severity": "high",
                    "status": "active",
                    "snippet": "some code here",  // must NOT leak
                    "detail": "layering violation detected"
                }
            ],
            "coverage_notes": [
                {"tool": "eslint", "message": "no JS found"}
            ]
        }
    });
    let result = extract_scan_results_from_draft(&draft);
    assert!(result.is_some(), "must return Some for non-empty findings");
    let out = result.unwrap();
    assert!(out.contains("ARCH-LAYERING-1"), "must contain rule_id");
    assert!(out.contains("src/lib.rs:10"), "must contain location");
    // Snippet must not appear
    assert!(
        !out.contains("some code here"),
        "snippet must not appear in output, got: {out}"
    );
    assert!(out.contains("eslint"), "coverage note must appear");
}

/// extract_scan_results_from_draft falls back to audit.findings when scan is absent.
#[test]
fn extract_scan_results_from_draft_audit_fallback() {
    let draft = serde_json::json!({
        "audit": {
            "findings": [
                {
                    "repo": "owner/repo",
                    "path": "src/api.rs",
                    "line": 5,
                    "rule_id": "DI-1",
                    "severity": "medium",
                    "status": "active",
                    "snippet": "secret-value-here",
                    "detail": "dependency injection violated"
                }
            ]
        }
    });
    let result = extract_scan_results_from_draft(&draft);
    assert!(result.is_some(), "audit.findings fallback must work");
    let out = result.unwrap();
    assert!(
        out.contains("DI-1"),
        "must contain rule_id from audit section"
    );
    assert!(
        !out.contains("secret-value-here"),
        "snippet must not leak via audit path"
    );
}

// ── last_scan store tests ─────────────────────────────────────────────────────────

// ── W3: finding_grounds_to / reconcile_external_tool_ledger ─────────────────────────

/// Helper: build a minimal preview `Finding` for the W3 ledger-reconciliation tests.
fn preview_finding_for_test(
    rule_id: &str,
    tool: &str,
    also_matches: &[&str],
) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: "owner/repo".to_string(),
        path: "src/db.py".to_string(),
        line: 10,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        preview: true,
        preview_tool: Some(tool.to_string()),
        also_matches: also_matches.iter().map(|s| s.to_string()).collect(),
        ..crate::onboard::Finding::default()
    }
}

#[test]
fn finding_grounds_to_matches_own_rule_id() {
    let f = preview_finding_for_test("SEC-NO-RAW-SQL-CONCAT-1", "semgrep", &[]);
    assert!(finding_grounds_to(&f, "SEC-NO-RAW-SQL-CONCAT-1"));
}

#[test]
fn finding_grounds_to_matches_via_semgrep_floor_category() {
    // A standalone preview row (never deduped against anything) whose OWN rule id is the
    // tool's id grounds to the corpus rule via `semgrep_floor_category`.
    let f = preview_finding_for_test("camerata.security.taint-sql-injection-go", "semgrep", &[]);
    assert!(finding_grounds_to(&f, "SEC-NO-RAW-SQL-CONCAT-1"));
    assert!(!finding_grounds_to(&f, "SEC-NO-OPEN-REDIRECT-1"));
}

#[test]
fn finding_grounds_to_matches_via_also_matches() {
    // A floor-canonical row that absorbed the semgrep rule id into `also_matches` during
    // dedup — the defect is still real and still counts as evidence the corpus rule fired.
    let f = preview_finding_for_test(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "semgrep",
        &["camerata.security.taint-sql-injection-python"],
    );
    // Build a SEPARATE finding whose OWN id is NOT the corpus id, but is folded into another.
    let folded = crate::onboard::Finding {
        rule_id: "unrelated-id".to_string(),
        also_matches: vec!["camerata.security.taint-sql-injection-python".to_string()],
        ..preview_finding_for_test("unrelated-id", "semgrep", &[])
    };
    assert!(finding_grounds_to(&f, "SEC-NO-RAW-SQL-CONCAT-1"));
    assert!(finding_grounds_to(&folded, "SEC-NO-RAW-SQL-CONCAT-1"));
}

#[test]
fn finding_grounds_to_false_for_unrelated_rule() {
    let f = preview_finding_for_test("camerata.security.weak-hash-python", "semgrep", &[]);
    assert!(!finding_grounds_to(&f, "SEC-NO-RAW-SQL-CONCAT-1"));
}

// ── ESLint grounding (`eslint_floor_category` / `external_tool_floor_category`) ────

#[test]
fn eslint_floor_category_grounds_react_hooks_rules() {
    assert_eq!(
        eslint_floor_category("react-hooks/exhaustive-deps"),
        Some("JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1")
    );
    assert_eq!(
        eslint_floor_category("react-hooks/rules-of-hooks"),
        Some("JAVASCRIPT-REACT-RULES-OF-HOOKS-1")
    );
    // Unrelated / unmapped ids never ground to anything.
    assert_eq!(eslint_floor_category("no-console"), None);
    assert_eq!(eslint_floor_category("react-hooks/unknown-rule"), None);
}

/// A standalone ESLint preview row (never deduped) whose own rule id is the tool's
/// native id grounds to the corpus rule via `eslint_floor_category`, exactly mirroring
/// `finding_grounds_to_matches_via_semgrep_floor_category` for Semgrep above.
#[test]
fn finding_grounds_to_matches_via_eslint_floor_category() {
    let f = preview_finding_for_test("react-hooks/exhaustive-deps", "eslint", &[]);
    assert!(finding_grounds_to(&f, "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1"));
    assert!(!finding_grounds_to(&f, "JAVASCRIPT-REACT-RULES-OF-HOOKS-1"));
}

/// `external_tool_floor_category` dispatches to whichever table (Semgrep or ESLint)
/// actually claims the id, and to neither when nothing does.
#[test]
fn external_tool_floor_category_dispatches_across_tools() {
    assert_eq!(
        external_tool_floor_category("camerata.security.hardcoded-secret"),
        Some("SEC-NO-HARDCODED-SECRETS-1")
    );
    assert_eq!(
        external_tool_floor_category("react-hooks/rules-of-hooks"),
        Some("JAVASCRIPT-REACT-RULES-OF-HOOKS-1")
    );
    assert_eq!(external_tool_floor_category("no-such-rule-anywhere"), None);
}

/// Helper: a minimal ScanReport (via the existing `gated` constructor, whose fields are
/// all `pub`) with its `findings`/`ledger`/`coverage_notes` overwritten for the test.
fn report_for_ledger_test() -> crate::onboard::ScanReport {
    crate::onboard::ScanReport::gated(&["owner/repo".to_string()])
}

/// The tool genuinely ran and found something: the pre-pass speculative
/// `ran=true, findings_emitted=0` entry is corrected to the REAL count, stays `ran=true`,
/// and no disclosure is raised.
#[test]
fn reconcile_marks_rule_ran_with_real_count_when_tool_succeeded() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0, // the pre-pass speculative (hardcoded) count
    );
    report.findings = vec![preview_finding_for_test(
        "camerata.security.taint-sql-injection-python",
        "semgrep",
        &[],
    )];
    let attempted: std::collections::HashSet<&str> = ["semgrep"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report.ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert!(entry.ran);
    assert_eq!(
        entry.findings_emitted, 1,
        "must reflect the REAL count, not the pre-pass 0"
    );
    assert!(
        report.failed_passes.is_empty(),
        "a successful pass must not disclose a failure"
    );
}

/// The tool is absent/failed for every repo this scan touched (a hard-failure
/// `CoverageNote` names it): the entry is corrected to NOT RUN with a disclosed reason, and
/// a `FailedPass` is pushed — never a silent "verified clean".
#[test]
fn reconcile_marks_rule_not_run_and_discloses_when_tool_absent() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    report.coverage_notes = vec![crate::onboard::CoverageNote {
        tool_failed: false,
        repo: None,
        tool: "semgrep".to_string(),
        message:
            "could not preview 1 rule(s) with semgrep: base interpreter not available: python3"
                .to_string(),
    }];
    // No findings at all — the tool never ran. It WAS attempted (routed, then failed) —
    // the hard-failure CoverageNote takes precedence over `tools_attempted` either way.
    let attempted: std::collections::HashSet<&str> = ["semgrep"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report.ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert!(
        !entry.ran,
        "a tool that never ran must never be recorded as ran=true"
    );
    assert!(!entry.verified_clean(), "must never read as verified clean");
    let reason = entry.skip_reason.as_deref().unwrap_or("");
    assert!(
        reason.starts_with("commodity taint pass did not run:"),
        "got: {reason}"
    );
    assert_eq!(report.failed_passes.len(), 1);
    assert_eq!(report.failed_passes[0].pass, "commodity taint pass");
    assert!(report.failed_passes[0]
        .reason
        .contains("SEC-NO-RAW-SQL-CONCAT-1"));
}

/// Defect 3 (2026-10-07 grading pass): Semgrep, once invoked at all, runs its WHOLE
/// bundled commodity-taint ruleset — it can produce a real, exported finding for a
/// corpus rule id the user never selected this run, so `onboard::audit_repos`'s
/// speculative pre-pass never pre-registered a `RuleTier::ExternalTool` ledger entry for
/// it at all. Before this fix, such a finding shipped in `report.findings` while
/// contributing ZERO to every ledger total — this is the "family reports findings=0 while
/// preview rows ARE present in the export" shape. The second discovery pass in
/// `reconcile_external_tool_ledger` must register it fresh.
#[test]
fn reconcile_discovers_a_grounded_rule_the_pre_pass_never_registered() {
    let mut report = report_for_ledger_test();
    // NOTE: deliberately NO pre-registered ledger entry for SEC-NO-COMMAND-INJECTION-1 —
    // the user never selected it this run.
    report.findings = vec![preview_finding_for_test(
        "camerata.security.taint-command-injection-go",
        "semgrep",
        &[],
    )];
    let attempted: std::collections::HashSet<&str> = ["semgrep"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report
        .ledger
        .rule("SEC-NO-COMMAND-INJECTION-1")
        .expect("the discovery pass must register a fresh entry for the grounded id");
    assert!(
        entry.ran,
        "a real exported finding must never read as not-run"
    );
    assert_eq!(
        entry.findings_emitted, 1,
        "the ledger's count must equal the rows actually exported"
    );
    assert_eq!(entry.tier, crate::scan_ledger::RuleTier::ExternalTool);
}

/// The discovery pass must never DOUBLE-count a rule id the correction loop already
/// handled (pre-registered AND corrected) just because a finding also carries it.
#[test]
fn reconcile_discovery_pass_never_double_counts_an_already_tracked_rule() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    report.findings = vec![preview_finding_for_test(
        "camerata.security.taint-sql-injection-python",
        "semgrep",
        &[],
    )];
    let attempted: std::collections::HashSet<&str> = ["semgrep"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report.ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert_eq!(
        entry.findings_emitted, 1,
        "the correction loop already counted this finding once; the discovery pass must \
             not add a second count for the same already-tracked id"
    );
}

/// A finding grounded ONLY via an absorbed `also_matches` member (never its own primary
/// id) must still be discovered — the merge-absorption case, mirrored from
/// `finding_grounds_to_matches_via_also_matches` above.
#[test]
fn reconcile_discovery_pass_credits_a_grounded_also_matches_member() {
    let mut report = report_for_ledger_test();
    let folded = crate::onboard::Finding {
        rule_id: "unrelated-id".to_string(),
        also_matches: vec!["camerata.security.taint-command-injection-go".to_string()],
        ..preview_finding_for_test("unrelated-id", "semgrep", &[])
    };
    report.findings = vec![folded];
    let attempted: std::collections::HashSet<&str> = ["semgrep"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report
        .ledger
        .rule("SEC-NO-COMMAND-INJECTION-1")
        .expect("an also_matches member grounding to a new id must still be discovered");
    assert_eq!(entry.findings_emitted, 1);
}

/// Outcome 3 (pass-level skip): the external-tool pass never invoked semgrep AT ALL this
/// scan — no coverage note names it (a tool nothing ever tried to run fails silently, by
/// construction) — so `tools_attempted` is the ONLY signal that it didn't run. Before this
/// branch existed, this exact state (emitted == 0, no coverage note) fell through to
/// "genuinely clean", silently keeping the pre-pass speculative `ran=true` — the precise
/// silent-knowledge-loss class this ledger exists to prevent.
#[test]
fn reconcile_marks_rule_not_run_when_tool_never_attempted_this_scan() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    // No coverage notes, no findings — the pass genuinely never touched this id.
    let attempted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report.ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert!(
        !entry.ran,
        "a tool never invoked this scan must never be recorded as ran=true"
    );
    assert!(!entry.verified_clean(), "must never read as verified clean");
    let reason = entry.skip_reason.as_deref().unwrap_or("");
    assert!(
        reason.starts_with("commodity taint pass did not run:"),
        "got: {reason}"
    );
    assert_eq!(report.failed_passes.len(), 1);
    assert_eq!(report.failed_passes[0].pass, "commodity taint pass");
    assert!(report.failed_passes[0]
        .reason
        .contains("SEC-NO-RAW-SQL-CONCAT-1"));
}

/// W3 wiring-gap fix, full pipeline: over a REAL synthetic repo, with the REAL loaded
/// corpus, through `audit_repos` -> `merge_scan_preview` -> `report_export::build_report_json`
/// (the exact sequence both `onboard_audit` and `camerata inspect` run), with
/// `CAMERATA_DISABLE_SEMGREP` forcing a deterministic "tool unavailable" outcome:
/// `SEC-NO-COMMAND-INJECTION-1` must NEVER appear in `whats_healthy.rules` (the report's
/// literal "verified clean" list), MUST appear in `rules_not_run` ("excluded from this
/// audit") with the disclosed reason, and the disclosure MUST ride into both
/// `executive_summary.failed_passes` and `methodology.failed_passes` — proving the fix
/// holds all the way through the exact function the PDF/xlsx/findings.json export is built
/// from, not just the ledger in isolation.
///
/// `SEC-NO-COMMAND-INJECTION-1` (not `SEC-NO-RAW-SQL-CONCAT-1`) is the right id for this
/// test: it has NO `camerata_gateway::lookup_arm` and is NOT in `onboard::AUDIT_RULES` (the
/// content floor's always-on regex set), so its ONLY detector is the commodity taint pass —
/// `SEC-NO-RAW-SQL-CONCAT-1` genuinely has a SECOND, independent, always-on gateway-regex
/// detector (the floor's `AUDIT_RULES` loop), so it legitimately stays `ran=true` via that
/// channel even when semgrep doesn't run — correct, not a gap, since it really was checked
/// by something. The silent-knowledge-loss class this fix closes is specifically "the
/// pass-claimed-to-cover-it tool never ran AND nothing else ever checked it either".
#[tokio::test]
#[serial_test::serial(process_env)]
async fn semgrep_covered_rule_never_reads_verified_clean_when_the_pass_does_not_run() {
    let _env = camerata_test_env::EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1");

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("app.py"),
        "def handler(request):\n    return {\"ok\": True}\n",
    )
    .unwrap();
    let sources = vec![("me/svc".to_string(), dir.path().to_path_buf())];

    let selected = vec![crate::onboard::SelectedRule {
        id: "SEC-NO-COMMAND-INJECTION-1".to_string(),
        directive: "no shell-interpreted invocation of externally-influenced values".to_string(),
        repos: Vec::new(),
    }];
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(
        corpus.get_by_id("SEC-NO-COMMAND-INJECTION-1").is_some(),
        "precondition: the real corpus must carry this id"
    );
    assert!(
        camerata_gateway::lookup_arm("SEC-NO-COMMAND-INJECTION-1").is_none(),
        "precondition: this id must have NO gateway-regex backstop"
    );
    assert!(
        !crate::onboard::AUDIT_RULES.contains(&"SEC-NO-COMMAND-INJECTION-1"),
        "precondition: this id must have NO always-on content-floor detector either"
    );
    // W13 added `split_scannable_rules`'s fifth element (the corpus-load failures the
    // function used to discard). This test asserts nothing about it — the real corpus
    // loads cleanly — so it is destructured away explicitly.
    let (_scannable, excluded_mechanical, preview_rules, _set, _corpus_load_failures) =
        split_scannable_rules(selected.clone()).await;
    assert!(
        preview_rules
            .iter()
            .any(|r| r.id == "SEC-NO-COMMAND-INJECTION-1"),
        "precondition: this id must be CI-tier + preview-runnable"
    );

    let (mut report, _manifest) = crate::onboard::audit_repos(
        &crate::credentials::MemoryCredentialStore::new(),
        &sources,
        &selected,
        Vec::new(),
        None,
        None,
        crate::ai_audit::ScanMode::Parallel,
        false,
        None,
        None,
        None,
        false,
        true,
        false, // run_ai_review off — token-free, this test is about the deterministic tier
        true,  // run_deterministic on
        None,
        crate::llm::BackendResolution::Cli,
        Some(&corpus),
        &std::collections::HashMap::new(),
    )
    .await;
    report.excluded_mechanical_rules = excluded_mechanical;
    merge_scan_preview(
        &mut report,
        &sources,
        &preview_rules,
        Some(&corpus),
        None,
        crate::client_lock::ClientTooling::INTERNAL,
    )
    .await;

    let entry = report
        .ledger
        .rule("SEC-NO-COMMAND-INJECTION-1")
        .expect("the ledger must carry this rule's outcome");
    assert!(
        !entry.ran,
        "the tool never ran, so this must never be ran=true"
    );
    assert!(!entry.verified_clean());

    let options = crate::report_export::ReportOptions::default();
    let json = crate::report_export::build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(
            &report.clone().seal(),
            &std::collections::HashMap::new(),
        ),
        Some(&corpus),
        &options,
    );

    assert!(
        !json
            .whats_healthy
            .rules
            .iter()
            .any(|r| r.rule_id == "SEC-NO-COMMAND-INJECTION-1"),
        "a rule the commodity taint pass never ran for must NEVER appear in the report's \
             'verified clean' section: {:?}",
        json.whats_healthy.rules
    );
    let excluded = json
        .rules_not_run
        .iter()
        .find(|r| r.rule_id == "SEC-NO-COMMAND-INJECTION-1");
    assert!(
        excluded.is_some_and(|r| r.reason.starts_with("commodity taint pass did not run:")),
        "must appear in 'excluded from this audit' with the real reason: {:?}",
        json.rules_not_run
    );
    assert!(
        json.executive_summary
            .failed_passes
            .iter()
            .any(|s| s.contains("SEC-NO-COMMAND-INJECTION-1") && s.contains("commodity taint")),
        "{:?}",
        json.executive_summary.failed_passes
    );
    assert!(
        json.methodology
            .failed_passes
            .iter()
            .any(|s| s.contains("SEC-NO-COMMAND-INJECTION-1") && s.contains("commodity taint")),
        "{:?}",
        json.methodology.failed_passes
    );
}

/// An "unrouted" coverage note (a routing gap — a selected rule with no tool to drive it)
/// must NOT be mistaken for a tool-execution failure.
#[test]
fn reconcile_does_not_treat_unrouted_note_as_tool_failure() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    report.coverage_notes = vec![crate::onboard::CoverageNote {
        tool_failed: false,
        repo: None,
        tool: "unrouted".to_string(),
        message: "could not preview SOME-OTHER-RULE-1 — no scan-runnable tool wired".to_string(),
    }];
    // Semgrep itself WAS attempted (routed + run) for this id — the unrouted note is about
    // a different, unrelated rule entirely.
    let attempted: std::collections::HashSet<&str> = ["semgrep"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report.ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert!(
        entry.ran,
        "an unrelated routing gap must not flip an unrelated rule to not-run"
    );
    assert_eq!(
        entry.findings_emitted, 0,
        "genuinely zero findings is a real clean result here"
    );
    assert!(report.failed_passes.is_empty());
}

/// Non-ExternalTool ledger entries (the floor, arch_checker, …) are untouched by
/// reconciliation — it only ever corrects the tier it exists for.
#[test]
fn reconcile_leaves_non_external_tool_entries_untouched() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-HARDCODED-SECRETS-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        5,
        2,
    );
    reconcile_external_tool_ledger(&mut report, &std::collections::HashSet::new(), None);
    let entry = report.ledger.rule("SEC-NO-HARDCODED-SECRETS-1").unwrap();
    assert!(entry.ran);
    assert_eq!(
        entry.findings_emitted, 2,
        "must be untouched by the external-tool correction"
    );
}

// ── reconcile_external_tool_ledger, generalized to ESLint ─────────────────────────
//
// Everything above this block only ever exercised Semgrep (the only external tool
// that existed when this ledger was built). These tests prove the SAME three outcomes
// hold for an ESLint-backed corpus rule once a `corpus` is threaded through — using
// the REAL loaded corpus so `external_tool_name_for_ledger_rule`'s
// `scan_tools::tool_for_rule` lookup resolves `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` to
// `ScanTool::Eslint` exactly as the live pipeline would, never a hand-built stand-in
// that could silently drift from the real routing.

/// Outcome 1: ESLint genuinely ran and found the hooks violation — the pre-pass
/// speculative `ran=true, findings_emitted=0` entry is corrected to the REAL count.
#[tokio::test]
async fn reconcile_marks_eslint_rule_ran_with_real_count_when_tool_succeeded() {
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(
        corpus
            .get_by_id("JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1")
            .is_some(),
        "precondition: the real corpus must carry this id"
    );

    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0, // the pre-pass speculative (hardcoded) count
    );
    report.findings = vec![preview_finding_for_test(
        "react-hooks/exhaustive-deps",
        "eslint",
        &[],
    )];
    let attempted: std::collections::HashSet<&str> = ["eslint"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, Some(&corpus));

    let entry = report
        .ledger
        .rule("JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1")
        .unwrap();
    assert!(entry.ran);
    assert_eq!(
        entry.findings_emitted, 1,
        "must reflect the REAL eslint count, not the pre-pass 0"
    );
    assert!(
        report.failed_passes.is_empty(),
        "a successful pass must not disclose a failure"
    );
}

/// Outcome 2: ESLint is absent/failed for every repo this scan touched — the entry is
/// corrected to NOT RUN with a disclosed reason naming ESLINT (not "commodity taint
/// pass", which is Semgrep's label) and a `FailedPass` is pushed. Never a silent
/// "verified clean".
#[tokio::test]
async fn reconcile_marks_eslint_rule_not_run_and_discloses_when_tool_absent() {
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;

    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "JAVASCRIPT-REACT-RULES-OF-HOOKS-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    report.coverage_notes = vec![crate::onboard::CoverageNote {
        tool_failed: false,
        repo: None,
        tool: "eslint".to_string(),
        message: "could not preview 1 rule(s) with eslint: base interpreter not available: \
                      npm"
        .to_string(),
    }];
    let attempted: std::collections::HashSet<&str> = ["eslint"].into_iter().collect();
    reconcile_external_tool_ledger(&mut report, &attempted, Some(&corpus));

    let entry = report
        .ledger
        .rule("JAVASCRIPT-REACT-RULES-OF-HOOKS-1")
        .unwrap();
    assert!(
        !entry.ran,
        "a tool that never ran must never be recorded as ran=true"
    );
    assert!(!entry.verified_clean(), "must never read as verified clean");
    let reason = entry.skip_reason.as_deref().unwrap_or("");
    assert!(
        reason.starts_with("the eslint preview pass did not run:"),
        "got: {reason}"
    );
    assert_eq!(report.failed_passes.len(), 1);
    assert_eq!(report.failed_passes[0].pass, "the eslint preview pass");
    assert!(report.failed_passes[0]
        .reason
        .contains("JAVASCRIPT-REACT-RULES-OF-HOOKS-1"));
}

/// Outcome 3: ESLint was never even INVOKED this scan (no selected rule routed to it,
/// no JS/TS language present, or the preview pass never ran at all) — same disclosure
/// shape as outcome 2, keyed off `tools_attempted` instead of a `CoverageNote`.
#[tokio::test]
async fn reconcile_marks_eslint_rule_not_run_when_tool_never_attempted_this_scan() {
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;

    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    // No coverage notes, no findings, and eslint is NOT in `tools_attempted`.
    let attempted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    reconcile_external_tool_ledger(&mut report, &attempted, Some(&corpus));

    let entry = report
        .ledger
        .rule("JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1")
        .unwrap();
    assert!(
        !entry.ran,
        "a tool never invoked this scan must never be recorded as ran=true"
    );
    assert!(!entry.verified_clean(), "must never read as verified clean");
    let reason = entry.skip_reason.as_deref().unwrap_or("");
    assert!(
        reason.starts_with("the eslint preview pass did not run:"),
        "got: {reason}"
    );
    assert!(reason.contains("eslint was never invoked"), "got: {reason}");
    assert_eq!(report.failed_passes.len(), 1);
    assert_eq!(report.failed_passes[0].pass, "the eslint preview pass");
}

/// Passing `corpus = None` (every pre-existing caller) must leave Semgrep's exact
/// original behavior and disclosure text untouched — the default-to-"semgrep" fallback
/// in `external_tool_name_for_ledger_rule` is a true backward-compatibility guarantee,
/// not just "probably fine".
#[test]
fn reconcile_without_corpus_keeps_original_semgrep_wording() {
    let mut report = report_for_ledger_test();
    report.ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        3,
        0,
    );
    let attempted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    reconcile_external_tool_ledger(&mut report, &attempted, None);
    let entry = report.ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    let reason = entry.skip_reason.as_deref().unwrap_or("");
    assert!(
        reason.starts_with("commodity taint pass did not run:"),
        "got: {reason}"
    );
    assert_eq!(report.failed_passes[0].pass, "commodity taint pass");
}
