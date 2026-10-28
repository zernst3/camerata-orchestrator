//! Client-facing report text: labels, titles, snippets, business-impact lines, placeholder
//! filling, category labels, headlines and the executive narrative.

use super::*;

// ---- disposition labels and bucket titles -------------------------------------------------

#[test]
fn an_open_row_in_the_held_bucket_reads_held_for_review_not_open() {
    let label = disposition_label(Disposition::Unresolved, "", "held", false);
    assert!(
        label.starts_with("Held for review (a human reviewer's judgment call"),
        "{label}"
    );
}

#[test]
fn an_open_row_in_the_informational_bucket_reads_as_a_convention_to_consider() {
    assert_eq!(
        disposition_label(Disposition::Unresolved, "", "informational", false),
        "Convention to consider (informational)"
    );
}

#[test]
fn every_bucket_has_its_own_reader_facing_title() {
    for (bucket, title) in [
        ("do_now", "Do now"),
        ("do_next", "Do next"),
        ("plan", "Plan"),
        ("informational", "Informational"),
        ("held", "Held for review"),
        ("accepted", "Accepted"),
    ] {
        assert_eq!(bucket_title(bucket), title, "{bucket}");
    }
}

// ---- snippet capping ---------------------------------------------------------------------

#[test]
fn a_snippet_at_exactly_the_line_cap_is_not_marked_truncated() {
    let twelve = (1..=SNIPPET_MAX_LINES)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(cap_snippet(&twelve), twelve);
    let thirteen = format!("{twelve}\nline 13");
    assert!(cap_snippet(&thirteen).ends_with("(truncated)"));
}

#[test]
fn a_snippet_at_exactly_the_char_cap_is_not_marked_truncated() {
    assert_eq!(cap_snippet("short"), "short");
    let exact = "x".repeat(SNIPPET_MAX_CHARS);
    assert_eq!(cap_snippet(&exact), exact);
    let over = "x".repeat(SNIPPET_MAX_CHARS + 1);
    let capped = cap_snippet(&over);
    assert!(capped.starts_with(&exact) && capped.ends_with("(truncated)"));
}

// ---- business impact lines ---------------------------------------------------------------

/// Every rule id with an authored business-impact line. A rule renamed in the corpus would
/// silently lose its line (the map falls through to `None` and the PDF omits it), so each id
/// must also still exist in the bundled corpus.
const IMPACT_RULE_IDS: &[&str] = &[
    "SUPABASE-RLS-ENABLED-1",
    "SUPABASE-RLS-NO-POLICY-1",
    "SUPABASE-RLS-POLICY-DISABLED-1",
    "SUPABASE-RLS-PERMISSIVE-TRUE-1",
    "SUPABASE-RLS-USER-METADATA-1",
    "SUPABASE-RLS-VIEW-INVOKER-1",
    "SUPABASE-RLS-INITPLAN-1",
    "SUPABASE-AUTH-EDGE-JWT-1",
    "SUPABASE-AUTH-GETSESSION-SERVER-1",
    "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1",
    "SUPABASE-AUTH-USERS-EXPOSED-1",
    "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
    "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
    "SUPABASE-STORAGE-OBJECT-POLICY-1",
    "SUPABASE-EXPOSURE-MATVIEW-1",
    "SUPABASE-EXPOSURE-SCHEMAS-1",
    "SUPABASE-FUNC-SEARCH-PATH-1",
    "ARCH-NO-SECRETS-IN-URL-1",
    "SEC-NO-PRIVATE-KEY-1",
    "SEC-NO-SECRET-FILE-1",
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-DISABLED-TLS-1",
    "SEC-NO-UNSAFE-DESERIALIZATION-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
];

#[tokio::test]
async fn every_authored_business_impact_line_is_reachable_and_names_a_real_rule() {
    let corpus =
        camerata_rules::load_corpus(std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH))
            .await
            .expect("bundled corpus loads");
    let mut seen = std::collections::HashSet::new();
    for id in IMPACT_RULE_IDS {
        let line = business_impact_for_rule(id)
            .unwrap_or_else(|| panic!("{id} lost its business-impact line"));
        assert!(seen.insert(line), "{id} repeats another rule's line");
        assert!(corpus.get_by_id(id).is_some(), "{id} is not a corpus rule");
    }
    assert_eq!(business_impact_for_rule("NOT-A-RULE-1"), None);
}

// ---- authored floor text and placeholder filling ------------------------------------------

#[test]
fn a_floor_template_with_a_blank_detail_half_is_not_used() {
    let mut rule = camerata_rules::bare_rule("FLOOR-TEXT-1", "test-fixture");
    rule.default_option = Some("default".to_string());
    rule.options = vec![camerata_rules::RuleOption {
        id: "default".to_string(),
        label: "Default".to_string(),
        directive: "d".to_string(),
        why: "w".to_string(),
        remediation: None,
        finding_headline: Some("A headline".to_string()),
        finding_detail: Some("   ".to_string()),
        escalation: None,
        effort: None,
        violates: false,
    }];
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(rule);
    let finding = Finding {
        rule_id: "FLOOR-TEXT-1".to_string(),
        ..Finding::default()
    };
    assert_eq!(
        resolve_floor_finding_text("FLOOR-TEXT-1", Some(&corpus), &finding, None),
        None,
        "headline and detail are authored as a pair; half a pair is never rendered"
    );
}

#[test]
fn every_uncaptured_placeholder_gets_its_own_readable_filler() {
    let empty = std::collections::BTreeMap::new();
    for (token, filler) in [
        ("table", "the affected table"),
        ("function-name", "the affected function"),
        ("function", "the affected function"),
        ("bucket", "the affected bucket"),
        ("view", "the affected view"),
        ("matview", "the affected materialized view"),
        ("schema", "the affected schema"),
        ("policy", "the affected policy"),
        ("path", "the affected file"),
        ("file", "the affected file"),
        ("secret-kind", "hardcoded credential"),
        (
            "gitignore-status",
            "not confirmed against this repository's `.gitignore`",
        ),
        (
            "history-status",
            "not checked against this repository's commit history",
        ),
        ("something-new", "the affected resource"),
    ] {
        assert_eq!(
            instantiate_remediation(&format!("Fix <{token}>."), "", &empty),
            format!("Fix {filler}."),
            "<{token}>"
        );
    }
}

#[test]
fn the_article_follows_the_sound_of_an_acronym_and_the_letter_of_a_word() {
    for (phrase, article) in [
        ("SSH private key", "an"),
        ("HTML page", "an"),
        ("AWS access key", "an"),
        ("GCP service key", "a"),
        ("Stripe secret key", "a"),
        ("S", "a"),
        ("hardcoded credential", "a"),
        ("unrestricted key", "an"),
    ] {
        assert_eq!(indefinite_article(phrase), article, "{phrase}");
    }
}

// ---- category labels -------------------------------------------------------------------------

#[test]
fn a_numeric_segment_never_becomes_part_of_a_category_label() {
    assert_eq!(category_display_label("web:2:auth"), "Web Auth");
    assert_eq!(category_display_label("supabase:rls"), "Supabase RLS");
}

#[test]
fn a_findings_own_category_wins_over_its_rule_id_and_a_blank_one_does_not() {
    let with_category = Finding {
        rule_id: "AI-ZZZ-1".to_string(),
        category: Some("injection".to_string()),
        ..Finding::default()
    };
    assert_eq!(category_for_finding(&with_category, None), "Injection");
    let blank = Finding {
        category: Some("   ".to_string()),
        ..with_category
    };
    assert_eq!(
        category_for_finding(&blank, None),
        category_for("AI-ZZZ-1", None)
    );
}

// ---- hedging and headlines -----------------------------------------------------------------

#[test]
fn each_hedge_signal_alone_hedges_a_row() {
    let plain = Finding::default();
    assert!(!is_hedged(&plain, "plan"));
    let calibrated_doubt = Finding {
        confidence: Some("needs-review".to_string()),
        ..Finding::default()
    };
    assert!(is_hedged(&calibrated_doubt, "plan"));
    assert!(is_hedged(&plain, "informational"));
    assert!(is_hedged(&plain, "held"));
}

#[test]
fn only_an_unrated_deterministic_row_is_reported_as_high_confidence() {
    let deterministic = Finding {
        rule_id: "SEC-NO-PRIVATE-KEY-1".to_string(),
        ..Finding::default()
    };
    assert_eq!(
        hedge_confidence(&deterministic, "do_now").as_deref(),
        Some("high")
    );
    let rated = Finding {
        confidence: Some("medium".to_string()),
        ..deterministic
    };
    assert_eq!(
        hedge_confidence(&rated, "do_now").as_deref(),
        Some("medium")
    );
    let unrated_ai = Finding {
        rule_id: "AI-SOMETHING-1".to_string(),
        ..Finding::default()
    };
    assert_eq!(hedge_confidence(&unrated_ai, "do_now"), None);
}

#[test]
fn a_headline_at_exactly_the_length_cap_is_used_whole() {
    let exact = "a".repeat(HEADLINE_LENGTH_CAP);
    assert_eq!(defect_headline(&exact, "fallback"), format!("{exact}."));
    let over = "a".repeat(HEADLINE_LENGTH_CAP + 1);
    assert_eq!(defect_headline(&over, "fallback"), format!("{exact}..."));
}

#[test]
fn a_short_headline_without_a_period_gets_exactly_one() {
    assert_eq!(
        defect_headline("no trailing period", "f"),
        "no trailing period."
    );
}

#[test]
fn a_period_inside_backticks_or_parentheses_is_not_a_sentence_end() {
    assert_eq!(
        defect_headline("Call `config.Load` first. Then more.", "f"),
        "Call `config.Load` first."
    );
    assert_eq!(
        defect_headline("Use a key (see Docs. Section 2) here. Next part.", "f"),
        "Use a key (see Docs. Section 2) here."
    );
}

#[test]
fn nested_parentheses_keep_their_periods_inside_the_sentence() {
    assert_eq!(
        defect_headline("Note (see a. Also (b) then c. Done) here. Next part.", "f"),
        "Note (see a. Also (b) then c. Done) here."
    );
}

// ---- the executive narrative -----------------------------------------------------------------

#[test]
fn the_raw_narrative_agrees_in_number_and_mentions_dependencies_only_when_present() {
    let singular = default_narrative(ReviewState::Raw, 1, 1, 1, 1, 1, 0, 1, 0, 0, 0);
    assert_eq!(
        singular,
        "This is a draft export: no finding in this run has had a human triage pass. The engine \
         analyzed 1 file and produced 1 candidate finding. 1 is curated for action (1 do now, 0 \
         do next, 0 planned, 0 already carried as accepted risk from a prior baseline or in-code \
         waiver), 1 is held for a human reviewer's judgment call, and 1 was auto-excluded as \
         likely false positives."
    );
    let plural = default_narrative(ReviewState::Raw, 2, 9, 2, 2, 2, 2, 1, 1, 0, 0);
    assert!(
        plural.contains("analyzed 2 files and produced 9 candidate findings. 2 are curated"),
        "{plural}"
    );
    assert!(plural.contains("2 are held") && plural.contains("2 were auto-excluded"));
    assert!(
        plural.ends_with(
            " A further 2 dependency advisories are tracked separately in the dependency snapshot."
        ),
        "{plural}"
    );
}

#[test]
fn the_reviewed_narrative_agrees_in_number() {
    let singular = default_narrative(ReviewState::Reviewed, 5, 3, 1, 1, 1, 1, 1, 0, 0, 0);
    assert_eq!(
        singular,
        "3 candidate findings were reviewed by a human reviewer; 1 was dispositioned as false \
         positives and excluded entirely from this report. Of the remaining 2 findings, 1 is \
         curated for action (1 do now, 0 do next, 0 planned, 0 accepted as risk) and 1 is held \
         for further review. A further 1 dependency advisory is tracked separately in the \
         dependency snapshot."
    );
    let plural = default_narrative(ReviewState::Reviewed, 5, 6, 2, 2, 2, 0, 2, 0, 0, 0);
    assert!(plural.contains("2 were dispositioned"), "{plural}");
    assert!(plural.contains("2 are curated") && plural.contains("2 are held"));
    assert!(!plural.contains("dependency"));
}

#[test]
fn an_empty_run_has_its_own_narrative() {
    assert_eq!(
        default_narrative(ReviewState::Raw, 10, 0, 0, 0, 0, 3, 0, 0, 0, 0),
        "The scan surfaced no candidate findings to review in this run."
    );
}
