//! Killing tests for the mutants `cargo mutants -p camerata-rules` left alive on 2026-10-28
//! (docs/MUTATION_TESTING.md).
//!
//! Most survivors were CORPUS GATES (`high_plus_rules_without_citation`, `security_rules_without_cwe`
//! and the rest). Every existing test runs them over the real shipped corpus and asserts the
//! offender list is empty, which a gate replaced by `vec![]` also satisfies: the gate could stop
//! detecting anything and the suite would stay green. Each test here hands a gate a synthetic rule
//! that MUST be flagged next to a twin that must not, so the gate has to actually discriminate.

use camerata_rules::{
    bare_rule, confirm_domain, confirm_in_text, days_from_civil, decision_kind_rule_count,
    high_plus_rules_without_any_fix, high_plus_rules_without_citation, load_corpus,
    options_with_invalid_effort, parse_ymd, role_from_corpus, rule_can_emit_high_plus,
    rules_declaring_deviation_ceiling, rules_declaring_max_severity, rules_declaring_min_severity,
    rules_missing_kind, rules_with_malformed_credibility_metadata, security_rules_without_cwe,
    select, EnforcementKind, EscalationSeverity, EscalationSpec, Filter, LockProblem, Rule,
    RuleKind, RuleOption, RuleSet, RuleSource, Verification, DEFAULT_CORPUS_PATH,
};

// ── fixtures ─────────────────────────────────────────────────────────────────

fn option(id: &str) -> RuleOption {
    RuleOption {
        id: id.to_owned(),
        label: format!("Option {id}"),
        directive: format!("Directive for {id}."),
        why: format!("Why {id}."),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }
}

fn source() -> RuleSource {
    RuleSource {
        url: "https://cwe.mitre.org/data/definitions/89.html".to_owned(),
        title: "CWE-89".to_owned(),
        linter: None,
        relevance: None,
    }
}

/// A non-draft rule (drafts are exempt from the high+ gates).
fn grounded(id: &str) -> Rule {
    let mut rule = bare_rule(id, "universal");
    rule.verification = Verification::Grounded;
    rule
}

fn with_min_severity(id: &str, floor: &str) -> Rule {
    let mut rule = grounded(id);
    rule.min_severity = Some(floor.to_owned());
    rule
}

fn set_of(rules: Vec<Rule>) -> RuleSet {
    let mut set = RuleSet::default();
    for rule in rules {
        set.push(rule);
    }
    set
}

// ── Rule / RuleKind / RuleSet accessors ──────────────────────────────────────

#[test]
fn rule_kind_display_is_the_wire_string() {
    for kind in RuleKind::ALL {
        assert_eq!(kind.to_string(), kind.as_str());
    }
    assert_eq!(RuleKind::Defect.to_string(), "defect");
}

#[test]
fn has_escalating_option_is_false_without_an_escalation_and_true_with_one() {
    let mut rule = grounded("T-ESC-1");
    rule.options = vec![option("a"), option("b")];
    assert!(!rule.has_escalating_option());
    rule.options[1].escalation = Some(EscalationSpec {
        condition: "a payment path".to_owned(),
        severity: EscalationSeverity::default(),
    });
    assert!(rule.has_escalating_option());
}

#[test]
fn is_defect_kind_is_true_only_for_the_defect_kind() {
    let mut rule = grounded("T-KIND-1");
    assert!(
        !rule.is_defect_kind(),
        "an unclassified rule is not a defect"
    );
    rule.kind = Some(RuleKind::Decision);
    assert!(!rule.is_defect_kind());
    rule.kind = Some(RuleKind::Defect);
    assert!(rule.is_defect_kind());
}

#[test]
fn rule_set_is_empty_tracks_its_contents() {
    let mut set = RuleSet::default();
    assert!(set.is_empty());
    set.push(grounded("T-SET-1"));
    assert!(!set.is_empty());
}

#[test]
fn filter_and_requires_both_sides() {
    let mut prose = bare_rule("T-AND-1", "rust");
    prose.enforcement = EnforcementKind::Prose;
    let mut mechanical = bare_rule("T-AND-2", "rust");
    mechanical.enforcement = EnforcementKind::Mechanical;
    let mut other_domain = bare_rule("T-AND-3", "python");
    other_domain.enforcement = EnforcementKind::Mechanical;
    let set = set_of(vec![prose, mechanical, other_domain]);

    let filter = Filter::And(
        Box::new(Filter::ByDomain("rust")),
        Box::new(Filter::ByEnforcement(EnforcementKind::Mechanical)),
    );
    let ids: Vec<&str> = select(&set, &filter).iter().map(|r| r.id_str()).collect();
    assert_eq!(ids, vec!["T-AND-2"]);
}

#[test]
fn lock_problem_id_names_the_rule() {
    let problems = [
        LockProblem::Unversioned { id: "A-1".into() },
        LockProblem::Unlocked { id: "B-2".into() },
        LockProblem::Orphaned { id: "C-3".into() },
        LockProblem::ChangedWithoutBump {
            id: "D-4".into(),
            version: 2,
        },
    ];
    let ids: Vec<&str> = problems.iter().map(LockProblem::id).collect();
    assert_eq!(ids, vec!["A-1", "B-2", "C-3", "D-4"]);
}

#[test]
fn canonical_form_escapes_every_control_character_like_json_dumps() {
    let mut rule = grounded("T-CANON-1");
    rule.title = "a\u{01}b\u{1f}c".to_owned();
    let canonical = rule.canonical_form();
    assert!(
        canonical.contains(r#""a\u0001b\u001fc""#),
        "control characters must be \\u-escaped in the canonical form: {canonical}"
    );
}

// ── build gates that must flag a planted offender ────────────────────────────

#[test]
fn rules_missing_kind_flags_only_the_unclassified_rule() {
    let mut classified = grounded("T-KIND-OK");
    classified.kind = Some(RuleKind::Defect);
    let set = set_of(vec![classified, grounded("T-KIND-NONE")]);
    assert_eq!(rules_missing_kind(&set), vec!["T-KIND-NONE".to_string()]);
}

#[test]
fn decision_kind_rule_count_counts_only_decision_rules() {
    let mut decision = grounded("T-DEC-1");
    decision.kind = Some(RuleKind::Decision);
    let mut defect = grounded("T-DEC-2");
    defect.kind = Some(RuleKind::Defect);
    let set = set_of(vec![decision, defect, grounded("T-DEC-3")]);
    assert_eq!(decision_kind_rule_count(&set), 1);
}

#[test]
fn rules_declaring_severity_bounds_list_exactly_the_declaring_rules() {
    let mut floor = grounded("T-FLOOR");
    floor.min_severity = Some("high".to_owned());
    let mut ceiling = grounded("T-CEIL");
    ceiling.max_severity = Some("medium".to_owned());
    let mut deviation = grounded("T-DEV");
    deviation.deviation_ceiling = Some("high".to_owned());
    let set = set_of(vec![floor, ceiling, deviation, grounded("T-NONE")]);

    assert_eq!(
        rules_declaring_min_severity(&set),
        vec!["T-FLOOR (high)".to_string()]
    );
    assert_eq!(
        rules_declaring_max_severity(&set),
        vec!["T-CEIL (medium)".to_string()]
    );
    assert_eq!(
        rules_declaring_deviation_ceiling(&set),
        vec!["T-DEV (high)".to_string()]
    );
}

#[test]
fn a_severity_floor_reaches_high_plus_only_when_it_is_high_or_critical() {
    for floor in ["high", "critical", "HIGH", "something-unknown"] {
        assert!(
            rule_can_emit_high_plus(&with_min_severity("T-HP", floor)),
            "a {floor:?} floor forces high+"
        );
    }
    for floor in ["info", "informational", "low", "medium"] {
        assert!(
            !rule_can_emit_high_plus(&with_min_severity("T-HP", floor)),
            "a {floor:?} floor must not count as high+"
        );
    }
}

#[test]
fn a_deviation_ceiling_reaches_high_plus_only_when_it_is_high_or_above() {
    let mut rule = grounded("T-DEV-HP");
    rule.kind = Some(RuleKind::DefaultWithExceptions);
    rule.deviation_ceiling = Some("high".to_owned());
    assert!(rule_can_emit_high_plus(&rule));
    rule.deviation_ceiling = Some("medium".to_owned());
    assert!(!rule_can_emit_high_plus(&rule));
}

#[test]
fn a_defect_rule_reaches_high_plus_unless_capped_below_high() {
    let mut rule = grounded("T-DEFECT-HP");
    rule.kind = Some(RuleKind::Defect);
    assert!(
        rule_can_emit_high_plus(&rule),
        "an uncapped defect anchors high"
    );
    rule.max_severity = Some("high".to_owned());
    assert!(rule_can_emit_high_plus(&rule));
    rule.max_severity = Some("medium".to_owned());
    assert!(!rule_can_emit_high_plus(&rule));
}

#[test]
fn a_violates_option_reaches_high_plus() {
    let mut rule = grounded("T-VIOLATES-HP");
    rule.kind = Some(RuleKind::Decision);
    rule.options = vec![option("a")];
    assert!(!rule_can_emit_high_plus(&rule));
    rule.options[0].violates = true;
    assert!(rule_can_emit_high_plus(&rule));
}

#[test]
fn high_plus_rules_without_citation_flags_a_cited_less_non_draft_high_rule() {
    let uncited = with_min_severity("T-UNCITED", "high");
    let mut cited = with_min_severity("T-CITED", "high");
    cited.sources = vec![source()];
    let mut draft = with_min_severity("T-DRAFT", "high");
    draft.verification = Verification::Draft;
    let low = with_min_severity("T-LOW", "low");
    let set = set_of(vec![uncited, cited, draft, low]);
    assert_eq!(
        high_plus_rules_without_citation(&set),
        vec!["T-UNCITED".to_string()]
    );
}

#[test]
fn high_plus_rules_without_any_fix_flags_a_high_rule_with_no_remediation() {
    let mut no_fix = with_min_severity("T-NOFIX", "high");
    no_fix.options = vec![option("a")];
    let mut fixed = with_min_severity("T-FIXED", "high");
    fixed.options = vec![option("a"), option("b")];
    fixed.options[1].remediation = Some("Do the fix.".to_owned());
    let mut draft = with_min_severity("T-DRAFT", "high");
    draft.verification = Verification::Draft;
    let set = set_of(vec![no_fix, fixed, draft]);
    assert_eq!(
        high_plus_rules_without_any_fix(&set),
        vec!["T-NOFIX".to_string()]
    );
}

#[test]
fn options_with_invalid_effort_flags_a_value_outside_the_vocabulary() {
    let mut rule = grounded("T-EFFORT");
    rule.options = vec![option("good"), option("bad"), option("unset")];
    rule.options[0].effort = Some("medium".to_owned());
    rule.options[1].effort = Some("2 days".to_owned());
    let set = set_of(vec![rule]);
    assert_eq!(
        options_with_invalid_effort(&set),
        vec!["T-EFFORT / bad: '2 days'".to_string()]
    );
}

#[test]
fn security_rules_without_cwe_flags_a_sec_rule_with_no_code() {
    let missing = grounded("SEC-T-1");
    let mut blank = grounded("SEC-T-2");
    blank.cwe = vec!["  ".to_owned()];
    let mut present = grounded("SEC-T-3");
    present.cwe = vec!["CWE-89".to_owned()];
    let not_security = grounded("ARCH-T-1");
    let set = set_of(vec![missing, blank, present, not_security]);
    assert_eq!(
        security_rules_without_cwe(&set),
        vec!["SEC-T-1".to_string(), "SEC-T-2".to_string()]
    );
}

#[test]
fn malformed_credibility_metadata_is_flagged_and_well_formed_metadata_is_not() {
    let mut good = grounded("T-GOOD");
    good.cwe = vec!["CWE-89".to_owned()];
    good.superseded_since = Some("2021".to_owned());
    let mut bad = grounded("T-BAD");
    bad.cwe = vec![
        "CWE89".to_owned(),
        "CWE-".to_owned(),
        "CWE-8x".to_owned(),
        "cwe-79".to_owned(),
    ];
    let mut short_year = grounded("T-YEAR-SHORT");
    short_year.superseded_since = Some("21".to_owned());
    let mut word_year = grounded("T-YEAR-WORD");
    word_year.superseded_since = Some("20x1".to_owned());
    let set = set_of(vec![good, bad, short_year, word_year]);

    assert_eq!(
        rules_with_malformed_credibility_metadata(&set),
        vec![
            r#"T-BAD: cwe "CWE-""#.to_string(),
            r#"T-BAD: cwe "CWE-8x""#.to_string(),
            r#"T-BAD: cwe "CWE89""#.to_string(),
            r#"T-BAD: cwe "cwe-79""#.to_string(),
            r#"T-YEAR-SHORT: superseded_since "21""#.to_string(),
            r#"T-YEAR-WORD: superseded_since "20x1""#.to_string(),
        ]
    );
}

// ── role derivation (real bundled corpus) ────────────────────────────────────

#[tokio::test]
async fn role_from_corpus_takes_universal_rules_plus_the_named_domain_only() {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    let set = load_corpus(path).await.expect("bundled corpus loads");
    let universal = set
        .iter()
        .find(|r| r.domain == "universal")
        .expect("the corpus has universal rules")
        .id
        .clone();
    let foreign = set
        .iter()
        .find(|r| r.domain == "python")
        .expect("the corpus has python rules")
        .id
        .clone();

    let role = role_from_corpus(path, "ci", &["ci-cd"], &[])
        .await
        .expect("role builds");
    assert!(role.rule_subset.contains(&universal));
    assert!(!role.rule_subset.contains(&foreign));
    assert_eq!(
        role.allowed_paths,
        vec!["**/.github/**".to_string(), "**".to_string()]
    );
}

// ── freshness dates and the watermark editor ─────────────────────────────────

#[test]
fn days_from_civil_is_right_before_year_one() {
    // 0000-01-01 is 719,528 days before the epoch; 0000-03-01 is the start of era 0.
    assert_eq!(days_from_civil(0, 1, 1), -719_528);
    assert_eq!(days_from_civil(0, 3, 1), -719_468);
    assert_eq!(days_from_civil(-1, 12, 31), -719_529);
}

#[test]
fn parse_ymd_rejects_a_single_short_field() {
    assert_eq!(parse_ymd("2026-01-05"), Some((2026, 1, 5)));
    assert_eq!(parse_ymd("2026-1-05"), None);
    assert_eq!(parse_ymd("2026-01-5"), None);
    assert_eq!(parse_ymd("226-01-05"), None);
}

const WATERMARK: &str = "\
[meta]
rust = \"not-the-confirmed-table\"
[confirmed]
go = \"2026-01-01\"
javascript = \"2026-02-01\"

[overrides]
rust = 30
";

#[test]
fn confirm_in_text_inserts_a_new_domain_at_the_end_of_the_confirmed_table() {
    let out = confirm_in_text(WATERMARK, "rust", "2026-10-28").unwrap();
    assert_eq!(
        out,
        "\
[meta]
rust = \"not-the-confirmed-table\"
[confirmed]
go = \"2026-01-01\"
javascript = \"2026-02-01\"
rust = \"2026-10-28\"

[overrides]
rust = 30
"
    );
}

#[test]
fn confirm_in_text_replaces_an_existing_entry_in_place() {
    let out = confirm_in_text(WATERMARK, "go", "2026-10-28").unwrap();
    assert_eq!(
        out,
        WATERMARK.replace("go = \"2026-01-01\"", "go = \"2026-10-28\"")
    );
}

#[test]
fn confirm_in_text_handles_a_confirmed_table_on_the_first_line() {
    let out = confirm_in_text("[confirmed]\ngo = \"2026-01-01\"\n", "rust", "2026-10-28").unwrap();
    assert_eq!(
        out,
        "[confirmed]\ngo = \"2026-01-01\"\nrust = \"2026-10-28\"\n"
    );
}

#[test]
fn confirm_domain_writes_the_advanced_watermark_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CORPUS_FRESHNESS.toml");
    std::fs::write(&path, WATERMARK).unwrap();
    confirm_domain(&path, "go", "2026-10-28").unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("go = \"2026-10-28\""), "{written}");
}
