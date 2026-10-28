//! Severity and routing decisions: the min-severity floor, the priority grid, the AI-finding
//! taxonomy, citation fallback, action-row gates, ordering ranks and informational routing.

use super::*;

// ---- the min-severity floor --------------------------------------------------------------

fn corpus_with_floor(rule_id: &str, floor: &str) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(rule_id, "test-fixture");
    rule.min_severity = Some(floor.to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

#[test]
fn the_floor_applies_to_a_file_level_finding_with_a_path_and_no_line() {
    let corpus = corpus_with_floor("FLOOR-1", "high");
    let file_level = Finding {
        rule_id: "FLOOR-1".to_string(),
        path: "src/config.rs".to_string(),
        line: 0,
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    };
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &file_level, Some(&corpus)),
        "high",
        "a path alone is a real location; only no path AND no line declines the floor"
    );
    let unlocated = Finding {
        path: String::new(),
        ..file_level
    };
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &unlocated, Some(&corpus)),
        "low"
    );
}

// ---- the priority grid -------------------------------------------------------------------

fn grid_ref(severity: &str, effort: Option<&str>, line: usize) -> FindingRefJson {
    FindingRefJson {
        rule_id: format!("GRID-{line}"),
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        severity: severity.to_string(),
        headline: format!("headline {line}"),
        effort: effort.map(str::to_string),
        bucket: "do_now".to_string(),
        precondition_count: 0,
        confidence_rank: 0,
        root_cause_keys: Vec::new(),
        kind: camerata_api_types::audit::KindOnWire(None),
    }
}

#[test]
fn the_priority_grid_places_each_finding_in_its_severity_row_and_effort_column() {
    let matrix = MatrixJson {
        do_now: vec![grid_ref("high", Some("high"), 1)],
        do_next: vec![grid_ref("medium", Some("low"), 2)],
        plan: vec![grid_ref("high", None, 3)],
        accepted: Vec::new(),
        informational: Vec::new(),
        held: Vec::new(),
    };
    let grid = build_priority_grid(&matrix);
    assert_eq!(grid.columns, vec!["low", "high", "unscoped"]);
    let rows: Vec<(&str, Vec<Vec<usize>>)> = grid
        .rows
        .iter()
        .map(|row| {
            let cells = row
                .cells
                .iter()
                .map(|cell| cell.findings.iter().map(|f| f.line).collect())
                .collect();
            (row.severity.as_str(), cells)
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("high", vec![vec![], vec![1], vec![3]]),
            ("medium", vec![vec![2], vec![], vec![]]),
        ]
    );
}

// ---- the AI-finding class taxonomy -------------------------------------------------------

/// One row per signal, each carrying ONLY that signal, so every alternative in
/// `classify_ai_finding`'s chains is shown to be sufficient on its own. A chain that silently
/// lost an alternative would leave a real finding uncited (held) instead of grounded.
#[test]
fn every_classification_signal_is_sufficient_on_its_own() {
    use AiFindingClass::*;
    let neutral = "the handler does something";
    let rows: &[(&str, Option<&str>, &str, Option<AiFindingClass>)] = &[
        // RLS
        ("AI-X-1", Some("rls-policy"), neutral, Some(Rls)),
        ("AI-RLS-1", None, neutral, Some(Rls)),
        ("AI-ROW-LEVEL-1", None, neutral, Some(Rls)),
        ("AI-X-1", None, "table lacks row level security", Some(Rls)),
        ("AI-X-1", None, "table lacks row-level security", Some(Rls)),
        // XSS
        ("AI-XSS-1", None, neutral, Some(Xss)),
        ("AI-INNERHTML-1", None, neutral, Some(Xss)),
        (
            "AI-X-1",
            None,
            "renders via dangerouslySetInnerHTML",
            Some(Xss),
        ),
        ("AI-X-1", None, "assigns input to el.innerHTML", Some(Xss)),
        ("AI-X-1", None, "a cross-site scripting sink", Some(Xss)),
        ("AI-X-1", None, "a cross site scripting sink", Some(Xss)),
        ("AI-X-1", None, "stored xss in comments", Some(Xss)),
        ("AI-X-1", None, "reflected xss in search", Some(Xss)),
        // Open redirect
        ("AI-OPEN-REDIRECT-1", None, neutral, Some(OpenRedirect)),
        ("AI-X-1", None, "an open redirect", Some(OpenRedirect)),
        (
            "AI-X-1",
            None,
            "an unvalidated redirect",
            Some(OpenRedirect),
        ),
        (
            "AI-X-1",
            None,
            "redirect chosen by an attacker",
            Some(OpenRedirect),
        ),
        (
            "AI-X-1",
            None,
            "redirect to an untrusted url",
            Some(OpenRedirect),
        ),
        (
            "AI-X-1",
            None,
            "redirect to an arbitrary url",
            Some(OpenRedirect),
        ),
        (
            "AI-X-1",
            None,
            "redirect to a user-controlled url",
            Some(OpenRedirect),
        ),
        (
            "AI-X-1",
            None,
            "redirect target is unvalidated",
            Some(OpenRedirect),
        ),
        ("AI-X-1", None, "a redirect after login", None),
        ("AI-X-1", None, "an attacker can read the page", None),
        // Weak token randomness
        (
            "AI-X-1",
            None,
            "ids from Math.random()",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "ids from random.random()",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "uses insecure random ids",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "uses weak random ids",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "a predictable token",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "a prng seeded by time",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "a non-cryptographic generator",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-RANDOM-TOKEN-1",
            None,
            neutral,
            Some(WeakTokenRandomness),
        ),
        (
            "AI-PRNG-SESSION-1",
            None,
            neutral,
            Some(WeakTokenRandomness),
        ),
        (
            "AI-RANDOM-NONCE-1",
            None,
            neutral,
            Some(WeakTokenRandomness),
        ),
        (
            "AI-RANDOM-SECRET-1",
            None,
            neutral,
            Some(WeakTokenRandomness),
        ),
        ("AI-RANDOM-1", None, neutral, None),
        ("AI-X-1", None, "a random token", Some(WeakTokenRandomness)),
        ("AI-X-1", None, "a random nonce", Some(WeakTokenRandomness)),
        (
            "AI-X-1",
            None,
            "a random session id",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "a random password reset code",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "a random api key",
            Some(WeakTokenRandomness),
        ),
        (
            "AI-X-1",
            None,
            "a random share link",
            Some(WeakTokenRandomness),
        ),
        ("AI-X-1", None, "a random colour", None),
        // Query grammar injection
        (
            "AI-FILTER-INJECT-1",
            None,
            neutral,
            Some(QueryGrammarInjection),
        ),
        (
            "AI-QUERY-GRAMMAR-1",
            None,
            neutral,
            Some(QueryGrammarInjection),
        ),
        (
            "AI-GRAMMAR-INJECT-1",
            None,
            neutral,
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "built with .or( and input",
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "built with .filter( and input",
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "input in the query grammar",
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "input in the filter grammar",
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "a postgrest call",
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "a $where clause",
            Some(QueryGrammarInjection),
        ),
        (
            "AI-X-1",
            None,
            "an ldap filter",
            Some(QueryGrammarInjection),
        ),
        // Permissive CORS (needs both halves)
        ("AI-CORS-1", None, "sends a cookie", Some(PermissiveCors)),
        (
            "AI-X-1",
            None,
            "cors with credentials",
            Some(PermissiveCors),
        ),
        (
            "AI-X-1",
            None,
            "access-control-allow-origin echoes; cookie sent",
            Some(PermissiveCors),
        ),
        (
            "AI-CREDENTIAL-1",
            None,
            "a cors policy",
            Some(PermissiveCors),
        ),
        (
            "AI-X-1",
            None,
            "cors sets allow-credentials",
            Some(PermissiveCors),
        ),
        ("AI-X-1", None, "a cors policy", None),
        ("AI-X-1", None, "a session cookie", None),
    ];
    for (rule_id, category, detail, expected) in rows {
        assert_eq!(
            classify_ai_finding(rule_id, *category, detail),
            *expected,
            "{rule_id} / {category:?} / {detail:?}"
        );
    }
}

// ---- citation fallback ---------------------------------------------------------------------

#[tokio::test]
async fn only_an_ai_tier_finding_borrows_its_class_citation() {
    let corpus = bundled_corpus().await;
    let ai = Finding {
        rule_id: "AI-TENANT-READ-1".to_string(),
        detail: "row level security is off on the orders table".to_string(),
        ..Finding::default()
    };
    assert_eq!(citation_for_finding(&ai, Some(&corpus)).kind, "grounded");
    // The same words on a deterministic finding with no citation of its own stay advisory: the
    // class fallback is for the AI tier only.
    let deterministic = Finding {
        rule_id: "LOCAL-CHECK-1".to_string(),
        ..ai
    };
    assert_eq!(
        citation_for_finding(&deterministic, Some(&corpus)).kind,
        "advisory"
    );
}

// ---- action-row gates and ordering ranks -------------------------------------------------

#[test]
fn a_row_already_held_for_review_is_never_also_reported_as_missing_a_fix() {
    // A recognized rule with no authored fix, on a critical row the calibrator flagged: the row
    // is held for review, so the separate "fix not generated" reason must not apply to it.
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(camerata_rules::bare_rule("NOFIX-1", "test-fixture"));
    let held = Finding {
        rule_id: "NOFIX-1".to_string(),
        path: "src/a.rs".to_string(),
        line: 3,
        needs_review: true,
        ..Finding::default()
    };
    assert!(!is_action_row_missing_a_fix(
        &held,
        Disposition::Unresolved,
        "critical",
        Some(&corpus),
        0,
        None
    ));
    let open = Finding {
        needs_review: false,
        ..held
    };
    assert!(is_action_row_missing_a_fix(
        &open,
        Disposition::Unresolved,
        "critical",
        Some(&corpus),
        0,
        None
    ));
}

#[test]
fn curated_ordering_ranks_are_strictly_ordered() {
    let severities: Vec<u8> = ["critical", "high", "medium", "low", "info"]
        .iter()
        .map(|s| severity_rank(s))
        .collect();
    assert_eq!(severities, vec![0, 1, 2, 3, 3]);
    let buckets: Vec<u8> = [
        "do_now",
        "do_next",
        "plan",
        "accepted",
        "informational",
        "held",
    ]
    .iter()
    .map(|b| curated_bucket_rank(b))
    .collect();
    assert_eq!(buckets, vec![0, 1, 2, 3, 4, 4]);
    let confidences: Vec<u8> = [
        Some("high"),
        Some("medium"),
        Some("low"),
        Some("needs-review"),
        None,
    ]
    .iter()
    .map(|c| curated_confidence_rank(*c))
    .collect();
    assert_eq!(confidences, vec![0, 1, 2, 3, 1]);
}

#[test]
fn provenance_tiers_rank_grounded_then_preview_then_everything_else() {
    let ranks: Vec<u8> = ["grounded", "preview", "advisory", "unknown"]
        .iter()
        .map(|k| provenance_tier_rank(k))
        .collect();
    assert_eq!(ranks, vec![0, 1, 2, 2]);
}

// ---- informational routing and confidence rank ---------------------------------------------

#[test]
fn only_a_structured_stance_layer_rule_routes_an_unlocated_note_to_informational() {
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(camerata_rules::bare_rule("STANCE-1", "universal"));
    corpus.push(camerata_rules::bare_rule("NOT-STANCE-1", "test-fixture"));
    let mut mechanical = camerata_rules::bare_rule("MECH-STANCE-1", "universal");
    mechanical.enforcement = camerata_rules::EnforcementKind::Mechanical;
    corpus.push(mechanical);
    let unlocated = |rule_id: &str| Finding {
        rule_id: rule_id.to_string(),
        located: false,
        ..Finding::default()
    };
    let informational = |rule_id: &str| {
        is_informational(
            &unlocated(rule_id),
            Disposition::Unresolved,
            "medium",
            Some(&corpus),
            100,
        )
    };
    assert!(informational("STANCE-1"));
    assert!(!informational("NOT-STANCE-1"), "not a stance-layer domain");
    assert!(!informational("MECH-STANCE-1"), "not a structured rule");
}

#[test]
fn either_calibration_doubt_signal_sets_the_confidence_rank() {
    assert_eq!(finding_confidence_rank(&Finding::default()), 0);
    let flagged = Finding {
        needs_review: true,
        ..Finding::default()
    };
    assert_eq!(finding_confidence_rank(&flagged), 1);
    let hedged = Finding {
        confidence: Some("needs-review".to_string()),
        ..Finding::default()
    };
    assert_eq!(finding_confidence_rank(&hedged), 1);
    let confident = Finding {
        confidence: Some("high".to_string()),
        ..Finding::default()
    };
    assert_eq!(finding_confidence_rank(&confident), 0);
}
