//! `Rule::content_hash()` and `CORPUS_LOCK.toml`: making `version` mechanical, not a convention
//! (design: `docs/design/2026-10-20_corpus-staleness-check.md`, "Amendment 2026-10-21", part 2).
//!
//! A `version` field nobody is forced to bump rots. So every rule has a canonical content hash
//! (the WHOLE rule, `version` and `last_reviewed` excluded), the lock records `(version, hash)`
//! per rule, and the lock gate fails a rule whose content changed while its version did not.
//!
//! Three things are asserted:
//!
//! 1. **The hash** is sensitive to every field that is part of the rule, insensitive to the two
//!    fields that are not, and pinned by a golden value computed by an independent oracle (Python
//!    `json.dumps`), so the serialization cannot drift silently between builds or releases.
//! 2. **The gate** fails for each way a lock can disagree with the corpus, and passes only when
//!    everything is consistent.
//! 3. **The real lock** matches the real corpus, and is in the exact form the CLI writes.

use camerata_rules::{
    bare_rule, check_lock, corpus_lock_path, corpus_version, load_corpus, CorpusLock,
    EnforcementKind, EscalationSeverity, EscalationSpec, LockEntry, LockError, LockProblem, Rule,
    RuleKind, RuleOption, RuleSet, RuleSource, StackException, Verification, VerifiedProvenance,
    DEFAULT_CORPUS_PATH, RULE_HASH_PREFIX,
};

// ────────────────────────────────────────────────────────────────────────────
// Fixtures
// ────────────────────────────────────────────────────────────────────────────

fn option(id: &str) -> RuleOption {
    RuleOption {
        id: id.to_string(),
        label: format!("Label {id}"),
        directive: format!("Do {id}."),
        why: "Because.".to_string(),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }
}

/// A rule with every field populated, so a per-field mutation test can prove each one counts.
fn full_rule() -> Rule {
    let mut r = bare_rule("FIX-FULL-1", "rust:seaorm");
    r.title = "Full fixture".to_string();
    r.enforcement = EnforcementKind::Structured;
    r.summary = "Summary.".to_string();
    r.decision_question = Some("Which?".to_string());
    r.decision_why = Some("Because.".to_string());
    r.options = vec![
        RuleOption {
            remediation: Some("Fix a.".to_string()),
            finding_headline: Some("Headline a".to_string()),
            finding_detail: Some("Detail a".to_string()),
            escalation: Some(EscalationSpec {
                condition: "cond".to_string(),
                severity: EscalationSeverity::SoftFlag,
            }),
            effort: Some("low".to_string()),
            ..option("a")
        },
        RuleOption {
            violates: true,
            ..option("b")
        },
    ];
    r.default_option = Some("a".to_string());
    r.verification = Verification::Grounded;
    r.sources = vec![RuleSource {
        url: "https://example.com".to_string(),
        title: "Example".to_string(),
        linter: Some("lint: x".to_string()),
        relevance: Some("Why it matters.".to_string()),
    }];
    r.verified = Some(VerifiedProvenance {
        by: "zach".to_string(),
        at: "2026-06-20".to_string(),
        against: vec!["clippy 1.83".to_string()],
    });
    r.opt_in_only = true;
    r.layer3_only = true;
    r.stack_exceptions = vec![StackException {
        framework: "Supabase".to_string(),
        path_glob: "supabase/**".to_string(),
        note: "Idiomatic.".to_string(),
    }];
    r.extra_domains = vec!["sql".to_string()];
    r.kind = Some(RuleKind::Decision);
    r.inspectable = Some(true);
    r.max_severity = Some("high".to_string());
    r.min_severity = Some("low".to_string());
    r.deviation_ceiling = Some("medium".to_string());
    r.cwe = vec!["CWE-89".to_string()];
    r.asvs = vec!["V5.3.4".to_string()];
    r.superseded_since = Some("2021".to_string());
    r.current_recommendation = Some("Use X instead.".to_string());
    r.applies_to = Some(
        camerata_rules::AppliesTo::new(vec!["**/*.sql".to_string()], vec!["rust".to_string()])
            .expect("a valid scope"),
    );
    r.candidates = Some(camerata_rules::Candidates::new("sql_routines").expect("a valid locator"));
    r.version = Some(4);
    r.last_reviewed = Some("2026-10-01".to_string());
    r
}

fn set_of(rules: Vec<Rule>) -> RuleSet {
    let mut set = RuleSet::default();
    for r in rules {
        set.push(r);
    }
    set
}

fn versioned(id: &str, version: u32) -> Rule {
    let mut r = bare_rule(id, "universal");
    r.version = Some(version);
    r
}

// ────────────────────────────────────────────────────────────────────────────
// 1. The hash
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn the_hash_is_prefixed_sha256_hex() {
    let h = full_rule().content_hash();
    let hex = h.strip_prefix(RULE_HASH_PREFIX).expect("sha256: prefix");
    assert_eq!(RULE_HASH_PREFIX, "sha256:");
    assert_eq!(hex.len(), 64, "{h}");
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "lowercase hex only: {h}"
    );
}

#[test]
fn the_hash_is_deterministic() {
    assert_eq!(full_rule().content_hash(), full_rule().content_hash());
}

/// The defect the old server hash had: it covered id/title/summary/enforcement only, so a
/// changed option or directive never showed as drift.
#[test]
fn a_changed_option_changes_the_hash() {
    let base = full_rule();
    let base_hash = base.content_hash();

    let mut directive = full_rule();
    directive.options[0].directive = "Do something else.".to_string();
    assert_ne!(directive.content_hash(), base_hash, "option directive");

    let mut why = full_rule();
    why.options[1].why = "Different rationale.".to_string();
    assert_ne!(why.content_hash(), base_hash, "option why");

    let mut flag = full_rule();
    flag.options[0].violates = true;
    assert_ne!(flag.content_hash(), base_hash, "option violates flag");

    let mut esc = full_rule();
    esc.options[0].escalation = Some(EscalationSpec {
        condition: "cond".to_string(),
        severity: EscalationSeverity::HardPause,
    });
    assert_ne!(esc.content_hash(), base_hash, "escalation severity");

    let mut added = full_rule();
    added.options.push(option("c"));
    assert_ne!(added.content_hash(), base_hash, "an added option");

    let mut removed = full_rule();
    removed.options.pop();
    assert_ne!(removed.content_hash(), base_hash, "a removed option");
}

#[test]
fn a_version_bump_alone_does_not_change_the_hash() {
    let base = full_rule();
    let mut bumped = full_rule();
    bumped.version = Some(5);
    assert_eq!(bumped.content_hash(), base.content_hash());

    let mut unversioned = full_rule();
    unversioned.version = None;
    assert_eq!(unversioned.content_hash(), base.content_hash());
}

#[test]
fn a_review_date_alone_does_not_change_the_hash() {
    let base = full_rule();
    let mut reviewed = full_rule();
    reviewed.last_reviewed = Some("2026-10-21".to_string());
    assert_eq!(reviewed.content_hash(), base.content_hash());

    reviewed.last_reviewed = None;
    assert_eq!(reviewed.content_hash(), base.content_hash());
}

/// Every field that is part of the rule moves the hash. The destructuring below has NO `..`, so
/// adding a field to `Rule` is a compile error here until someone decides whether it is part of
/// the rule's content; and the table must then gain (or knowingly omit) a mutation for it.
#[test]
fn every_field_of_the_rule_except_version_and_last_reviewed_changes_the_hash() {
    let Rule {
        id: _,
        title: _,
        enforcement: _,
        domain: _,
        summary: _,
        decision_question: _,
        decision_why: _,
        options: _,
        default_option: _,
        verification: _,
        sources: _,
        verified: _,
        opt_in_only: _,
        layer3_only: _,
        stack_exceptions: _,
        extra_domains: _,
        kind: _,
        inspectable: _,
        max_severity: _,
        min_severity: _,
        deviation_ceiling: _,
        cwe: _,
        asvs: _,
        superseded_since: _,
        current_recommendation: _,
        applies_to: _,
        candidates: _,
        version: _,
        last_reviewed: _,
    } = full_rule();

    type Mutation = (&'static str, fn(&mut Rule));
    let mutations: Vec<Mutation> = vec![
        ("id", |r| r.id.0.push('X')),
        ("title", |r| r.title.push('X')),
        ("enforcement", |r| r.enforcement = EnforcementKind::Prose),
        ("domain", |r| r.domain = "go".to_string()),
        ("summary", |r| r.summary.push('X')),
        ("decision_question", |r| r.decision_question = None),
        ("decision_why", |r| r.decision_why = None),
        ("default_option", |r| r.default_option = None),
        ("verification", |r| r.verification = Verification::Policy),
        ("sources.url", |r| r.sources[0].url.push('X')),
        ("sources.title", |r| r.sources[0].title.push('X')),
        ("sources.linter", |r| r.sources[0].linter = None),
        ("sources.relevance", |r| r.sources[0].relevance = None),
        ("sources (added)", |r| {
            let s = r.sources[0].clone();
            r.sources.push(s);
        }),
        ("verified.by", |r| {
            if let Some(v) = r.verified.as_mut() {
                v.by.push('X');
            }
        }),
        ("verified.at", |r| {
            if let Some(v) = r.verified.as_mut() {
                v.at.push('X');
            }
        }),
        ("verified.against", |r| {
            if let Some(v) = r.verified.as_mut() {
                v.against.clear();
            }
        }),
        ("verified (removed)", |r| r.verified = None),
        ("opt_in_only", |r| r.opt_in_only = false),
        ("layer3_only", |r| r.layer3_only = false),
        ("stack_exceptions.framework", |r| {
            r.stack_exceptions[0].framework.push('X')
        }),
        ("stack_exceptions.path_glob", |r| {
            r.stack_exceptions[0].path_glob.push('X')
        }),
        ("stack_exceptions.note", |r| {
            r.stack_exceptions[0].note.push('X')
        }),
        ("extra_domains", |r| r.extra_domains.clear()),
        ("kind", |r| r.kind = Some(RuleKind::Defect)),
        ("inspectable", |r| r.inspectable = Some(false)),
        ("max_severity", |r| r.max_severity = None),
        ("min_severity", |r| r.min_severity = None),
        ("deviation_ceiling", |r| r.deviation_ceiling = None),
        ("cwe", |r| r.cwe.clear()),
        ("asvs", |r| r.asvs.clear()),
        ("superseded_since", |r| r.superseded_since = None),
        ("current_recommendation", |r| {
            r.current_recommendation = None
        }),
        ("applies_to.paths", |r| {
            if let Some(a) = r.applies_to.as_mut() {
                a.paths.push("extra/**".to_string());
            }
        }),
        ("applies_to.languages", |r| {
            if let Some(a) = r.applies_to.as_mut() {
                a.languages.clear();
            }
        }),
        ("applies_to (removed)", |r| r.applies_to = None),
        ("candidates.locator", |r| {
            r.candidates = Some(camerata_rules::Candidates::new("sql_tables").expect("valid"))
        }),
        ("candidates (removed)", |r| r.candidates = None),
        ("option.id", |r| r.options[0].id.push('X')),
        ("option.label", |r| r.options[0].label.push('X')),
        ("option.directive", |r| r.options[0].directive.push('X')),
        ("option.why", |r| r.options[0].why.push('X')),
        ("option.remediation", |r| r.options[0].remediation = None),
        ("option.finding_headline", |r| {
            r.options[0].finding_headline = None
        }),
        ("option.finding_detail", |r| {
            r.options[0].finding_detail = None
        }),
        ("option.escalation", |r| r.options[0].escalation = None),
        ("option.effort", |r| r.options[0].effort = None),
        ("option.violates", |r| r.options[1].violates = false),
        ("options (reordered)", |r| r.options.reverse()),
    ];

    let base = full_rule().content_hash();
    let mut seen = std::collections::BTreeSet::new();
    for (name, mutate) in mutations {
        let mut r = full_rule();
        mutate(&mut r);
        let h = r.content_hash();
        assert_ne!(h, base, "changing `{name}` must change the content hash");
        assert!(
            seen.insert(h),
            "changing `{name}` collided with another mutation's hash"
        );
    }
}

/// Field boundaries are unambiguous: moving text between adjacent fields is a change. A naive
/// concatenation hash (`a + b`) would score these as identical.
#[test]
fn text_moved_between_adjacent_fields_changes_the_hash() {
    let mut a = full_rule();
    a.title = "AB".to_string();
    a.summary = "C".to_string();
    let mut b = full_rule();
    b.title = "A".to_string();
    b.summary = "BC".to_string();
    assert_ne!(a.content_hash(), b.content_hash());
}

/// An unset optional field and one set to the empty string are the same rule as far as the
/// loader is concerned, so they hash the same. This is also what lets a future optional field
/// that no existing rule sets be added without churning every hash in the corpus.
#[test]
fn an_absent_optional_field_and_an_empty_one_hash_the_same() {
    let mut absent = full_rule();
    absent.decision_why = None;
    absent.max_severity = None;
    absent.options[0].remediation = None;
    absent.options[0].effort = None;

    let mut empty = full_rule();
    empty.decision_why = Some(String::new());
    empty.max_severity = Some(String::new());
    empty.options[0].remediation = Some(String::new());
    empty.options[0].effort = Some(String::new());

    assert_eq!(absent.content_hash(), empty.content_hash());
}

/// `false` flags and empty lists are omitted, so adding a new defaulted field later (a flag
/// that is `false` for every existing rule) leaves every existing hash untouched.
#[test]
fn default_valued_fields_are_omitted_from_the_canonical_form() {
    let r = bare_rule("FIX-BARE-1", "universal");
    let canon = r.canonical_form();
    for absent in [
        "opt_in_only",
        "layer3_only",
        "options",
        "sources",
        "cwe",
        "asvs",
        "extra_domains",
        "stack_exceptions",
        "verified",
        "kind",
        "inspectable",
        "version",
        "last_reviewed",
    ] {
        assert!(
            !canon.contains(&format!("\"{absent}\"")),
            "`{absent}` should be omitted from a bare rule: {canon}"
        );
    }
}

/// The golden value: pins the canonical serialization byte for byte. Both strings below were
/// produced by an independent oracle (Python `json.dumps(..., ensure_ascii=False,
/// sort_keys=True, separators=(",", ":"))` over the same logical fixture, then SHA-256 over
/// `"camerata-rule-hash/v1\n" + canonical`), NOT by this implementation. If this test fails,
/// the hash of every rule in the corpus has changed: that is a deliberate format migration (a new
/// `/vN` tag and a full lock regeneration), never an incidental side effect.
#[test]
fn the_canonical_form_and_hash_match_the_independent_oracle() {
    let mut r = bare_rule("FIX-GOLDEN-1", "rust:seaorm");
    r.title = "Golden \"fixture\"\nline \u{e9}".to_string();
    r.enforcement = EnforcementKind::Structured;
    r.summary = "Summary \u{e9}".to_string();
    r.default_option = Some("a".to_string());
    r.verification = Verification::Grounded;
    r.kind = Some(RuleKind::Decision);
    r.inspectable = Some(true);
    r.cwe = vec!["CWE-89".to_string()];
    r.options = vec![
        RuleOption {
            remediation: Some("Fix A.".to_string()),
            escalation: Some(EscalationSpec {
                condition: "cond".to_string(),
                severity: EscalationSeverity::SoftFlag,
            }),
            effort: Some("low".to_string()),
            ..option("a")
        },
        RuleOption {
            directive: String::new(),
            why: "Rejected: bad.".to_string(),
            violates: true,
            ..option("b")
        },
    ];
    r.sources = vec![RuleSource {
        url: "https://example.com".to_string(),
        title: "Ex".to_string(),
        linter: None,
        relevance: None,
    }];
    // Excluded fields, set to prove they are not in the preimage.
    r.version = Some(9);
    r.last_reviewed = Some("2026-10-01".to_string());

    assert_eq!(
        r.canonical_form(),
        concat!(
            r#"{"cwe":["CWE-89"],"default_option":"a","domain":"rust:seaorm","#,
            r#""enforcement":"structured","id":"FIX-GOLDEN-1","inspectable":true,"#,
            r#""kind":"decision","options":[{"directive":"Do a.","effort":"low","#,
            r#""escalation":{"condition":"cond","severity":"soft-flag"},"id":"a","#,
            r#""label":"Label a","remediation":"Fix A.","why":"Because."},"#,
            r#"{"id":"b","label":"Label b","violates":true,"why":"Rejected: bad."}],"#,
            r#""sources":[{"title":"Ex","url":"https://example.com"}],"#,
            "\"summary\":\"Summary \u{e9}\",",
            "\"title\":\"Golden \\\"fixture\\\"\\nline \u{e9}\",",
            r#""verification":"grounded"}"#,
        )
    );
    assert_eq!(
        r.content_hash(),
        "sha256:e5aa8d8f0774275737b264589f20fc38f668471b85ae6f4420c26c180a35f92e"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// 2. The lock: parse / render
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn the_lock_round_trips_through_render_and_parse() {
    let set = set_of(vec![versioned("B-RULE-1", 2), versioned("A-RULE-1", 1)]);
    let lock = CorpusLock::from_corpus(&set).expect("all versioned");
    let text = lock.render();
    assert_eq!(CorpusLock::parse(&text).expect("parses"), lock);
    // Sorted by id regardless of load order, so the diff of a regeneration is minimal.
    let a = text.find("\"A-RULE-1\"").expect("a present");
    let b = text.find("\"B-RULE-1\"").expect("b present");
    assert!(a < b, "entries sorted by rule id:\n{text}");
    assert!(
        text.starts_with('#'),
        "the file opens with a header comment explaining the gate"
    );
}

#[test]
fn from_corpus_refuses_an_unversioned_rule() {
    let set = set_of(vec![versioned("OK-1", 1), bare_rule("NO-VERSION-1", "go")]);
    assert_eq!(
        CorpusLock::from_corpus(&set).unwrap_err(),
        vec![LockProblem::Unversioned {
            id: "NO-VERSION-1".to_string()
        }]
    );
}

#[test]
fn parse_rejects_a_malformed_hash_and_unknown_keys() {
    let bad_hash = "[rules]\n\"A-1\" = { version = 1, hash = \"md5:abc\" }\n";
    assert!(matches!(
        CorpusLock::parse(bad_hash),
        Err(LockError::MalformedEntry { .. })
    ));
    let short = "[rules]\n\"A-1\" = { version = 1, hash = \"sha256:abc\" }\n";
    assert!(matches!(
        CorpusLock::parse(short),
        Err(LockError::MalformedEntry { .. })
    ));
    let zero = format!(
        "[rules]\n\"A-1\" = {{ version = 0, hash = \"sha256:{}\" }}\n",
        "0".repeat(64)
    );
    assert!(matches!(
        CorpusLock::parse(&zero),
        Err(LockError::MalformedEntry { .. })
    ));
    let extra = format!(
        "[rules]\n\"A-1\" = {{ version = 1, hash = \"sha256:{}\", note = \"x\" }}\n",
        "0".repeat(64)
    );
    assert!(matches!(CorpusLock::parse(&extra), Err(LockError::Toml(_))));
    assert!(matches!(
        CorpusLock::parse("this is not toml ["),
        Err(LockError::Toml(_))
    ));
}

// ────────────────────────────────────────────────────────────────────────────
// 3. The gate
// ────────────────────────────────────────────────────────────────────────────

fn locked(set: &RuleSet) -> CorpusLock {
    CorpusLock::from_corpus(set).expect("all versioned")
}

fn edit(rule: &mut Rule) {
    rule.title.push_str(" (edited)");
}

#[test]
fn the_gate_passes_when_rules_and_lock_agree() {
    let set = set_of(vec![versioned("A-1", 1), versioned("B-1", 3)]);
    assert_eq!(check_lock(&set, &locked(&set)), Vec::<LockProblem>::new());
}

#[test]
fn the_gate_fails_a_changed_rule_whose_version_was_not_bumped() {
    let before = set_of(vec![versioned("A-1", 1), versioned("B-1", 3)]);
    let lock = locked(&before);

    let mut changed = versioned("B-1", 3); // same version
    edit(&mut changed);
    let after = set_of(vec![versioned("A-1", 1), changed]);

    let problems = check_lock(&after, &lock);
    assert_eq!(
        problems,
        vec![LockProblem::ChangedWithoutBump {
            id: "B-1".to_string(),
            version: 3
        }]
    );
    let msg = problems[0].to_string();
    assert!(msg.contains("B-1"), "{msg}");
    assert!(
        msg.contains("bump `version`") && msg.contains("camerata corpus lock"),
        "the message must say what to do: {msg}"
    );
    assert!(problems[0].blocks_regeneration());
}

#[test]
fn the_gate_fails_a_bumped_rule_until_the_lock_is_regenerated() {
    let before = set_of(vec![versioned("A-1", 1)]);
    let lock = locked(&before);

    let mut bumped = versioned("A-1", 2);
    edit(&mut bumped);
    let after = set_of(vec![bumped]);

    let problems = check_lock(&after, &lock);
    assert_eq!(
        problems,
        vec![LockProblem::StaleVersion {
            id: "A-1".to_string(),
            locked: 1,
            current: 2
        }]
    );
    assert!(problems[0].to_string().contains("camerata corpus lock"));
    assert!(
        !problems[0].blocks_regeneration(),
        "a bump is exactly what regeneration is for"
    );
}

#[test]
fn the_gate_fails_a_version_bump_with_no_content_change_until_the_lock_moves() {
    let lock = locked(&set_of(vec![versioned("A-1", 1)]));
    let after = set_of(vec![versioned("A-1", 2)]);
    assert_eq!(
        check_lock(&after, &lock),
        vec![LockProblem::StaleVersion {
            id: "A-1".to_string(),
            locked: 1,
            current: 2
        }],
        "the lock never moves on its own, even for a bump that changed nothing"
    );
}

#[test]
fn the_gate_fails_a_rule_missing_from_the_lock() {
    let lock = locked(&set_of(vec![versioned("A-1", 1)]));
    let after = set_of(vec![versioned("A-1", 1), versioned("NEW-1", 1)]);
    assert_eq!(
        check_lock(&after, &lock),
        vec![LockProblem::Unlocked {
            id: "NEW-1".to_string()
        }]
    );
}

#[test]
fn the_gate_fails_a_lock_entry_for_a_rule_that_no_longer_exists() {
    let lock = locked(&set_of(vec![versioned("A-1", 1), versioned("GONE-1", 1)]));
    let after = set_of(vec![versioned("A-1", 1)]);
    let problems = check_lock(&after, &lock);
    assert_eq!(
        problems,
        vec![LockProblem::Orphaned {
            id: "GONE-1".to_string()
        }]
    );
    assert!(problems[0].to_string().contains("camerata corpus lock"));
    assert!(!problems[0].blocks_regeneration());
}

#[test]
fn the_gate_fails_a_version_that_went_backwards() {
    let lock = locked(&set_of(vec![versioned("A-1", 5)]));
    let after = set_of(vec![versioned("A-1", 4)]);
    assert_eq!(
        check_lock(&after, &lock),
        vec![LockProblem::VersionRegressed {
            id: "A-1".to_string(),
            locked: 5,
            current: 4
        }]
    );
}

#[test]
fn the_gate_fails_an_unversioned_rule() {
    let lock = locked(&set_of(vec![versioned("A-1", 1)]));
    let after = set_of(vec![bare_rule("A-1", "universal")]);
    assert_eq!(
        check_lock(&after, &lock),
        vec![LockProblem::Unversioned {
            id: "A-1".to_string()
        }]
    );
}

#[test]
fn the_gate_reports_every_problem_not_just_the_first() {
    let lock = locked(&set_of(vec![
        versioned("CHANGED-1", 1),
        versioned("GONE-1", 1),
        versioned("STALE-1", 1),
    ]));
    let mut changed = versioned("CHANGED-1", 1);
    edit(&mut changed);
    let after = set_of(vec![
        changed,
        versioned("STALE-1", 2),
        versioned("NEW-1", 1),
    ]);
    let problems = check_lock(&after, &lock);
    assert_eq!(problems.len(), 4, "{problems:?}");
}

// ────────────────────────────────────────────────────────────────────────────
// 4. Regeneration (the engine of `camerata corpus lock`)
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn regeneration_refuses_a_changed_rule_with_no_bump() {
    let before = set_of(vec![versioned("A-1", 1)]);
    let existing = locked(&before);
    let mut changed = versioned("A-1", 1);
    edit(&mut changed);
    let after = set_of(vec![changed]);

    let refused = CorpusLock::regenerate(&after, Some(&existing)).unwrap_err();
    assert_eq!(
        refused,
        vec![LockProblem::ChangedWithoutBump {
            id: "A-1".to_string(),
            version: 1
        }],
        "the CLI must not launder a missed bump into the lock"
    );
}

#[test]
fn regeneration_refuses_a_version_that_went_backwards() {
    let existing = locked(&set_of(vec![versioned("A-1", 5)]));
    let after = set_of(vec![versioned("A-1", 4)]);
    assert!(matches!(
        CorpusLock::regenerate(&after, Some(&existing)).unwrap_err()[0],
        LockProblem::VersionRegressed { .. }
    ));
}

#[test]
fn regeneration_accepts_bumps_new_rules_and_removals() {
    let existing = locked(&set_of(vec![
        versioned("BUMPED-1", 1),
        versioned("SAME-1", 2),
        versioned("GONE-1", 1),
    ]));
    let mut bumped = versioned("BUMPED-1", 2);
    edit(&mut bumped);
    let after = set_of(vec![bumped, versioned("SAME-1", 2), versioned("NEW-1", 1)]);

    let fresh = CorpusLock::regenerate(&after, Some(&existing)).expect("allowed transitions");
    assert_eq!(fresh, locked(&after));
    assert_eq!(fresh.rules.get("BUMPED-1").map(|e| e.version), Some(2));
    assert!(fresh.rules.contains_key("NEW-1"));
    assert!(!fresh.rules.contains_key("GONE-1"));
    assert_eq!(
        check_lock(&after, &fresh),
        Vec::<LockProblem>::new(),
        "a regenerated lock satisfies the gate"
    );
}

#[test]
fn regeneration_with_no_existing_lock_locks_everything() {
    let after = set_of(vec![versioned("A-1", 1), versioned("B-1", 7)]);
    let fresh = CorpusLock::regenerate(&after, None).expect("first lock");
    assert_eq!(fresh.rules.len(), 2);
    assert_eq!(
        fresh.rules.get("B-1"),
        Some(&LockEntry {
            version: 7,
            hash: after.get_by_id("B-1").expect("present").content_hash(),
        })
    );
}

#[test]
fn regeneration_refuses_an_unversioned_rule_even_with_no_existing_lock() {
    let after = set_of(vec![bare_rule("A-1", "universal")]);
    assert_eq!(
        CorpusLock::regenerate(&after, None).unwrap_err(),
        vec![LockProblem::Unversioned {
            id: "A-1".to_string()
        }]
    );
}

// ────────────────────────────────────────────────────────────────────────────
// 5. The real lock against the real corpus
// ────────────────────────────────────────────────────────────────────────────

/// The gate itself. Fails with one line per problem, each saying what to run.
#[tokio::test]
async fn req_the_bundled_lock_matches_the_bundled_corpus() {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return;
    }
    let set = load_corpus(path).await.expect("corpus loads");
    let lock = CorpusLock::load(&corpus_lock_path()).expect("CORPUS_LOCK.toml parses");

    let problems = check_lock(&set, &lock);
    assert!(
        problems.is_empty(),
        "CORPUS_LOCK.toml disagrees with the corpus:\n  {}",
        problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The checked-in file is byte-identical to what `camerata corpus lock` would write, so a
/// hand-edit (or a merge that interleaved entries badly) is caught, and regenerating never
/// produces a noisy diff.
#[tokio::test]
async fn req_the_bundled_lock_is_in_the_canonical_form_the_cli_writes() {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return;
    }
    let set = load_corpus(path).await.expect("corpus loads");
    let on_disk = std::fs::read_to_string(corpus_lock_path()).expect("CORPUS_LOCK.toml exists");
    let regenerated = CorpusLock::from_corpus(&set)
        .expect("every rule versioned")
        .render();
    assert!(
        on_disk == regenerated,
        "CORPUS_LOCK.toml is not in canonical form; run `camerata corpus lock`"
    );
}

/// Anti-vacuity: the real lock actually covers the corpus, so the gate above is not comparing
/// two empty sets.
#[tokio::test]
async fn req_the_bundled_lock_covers_every_corpus_rule() {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return;
    }
    let set = load_corpus(path).await.expect("corpus loads");
    let lock = CorpusLock::load(&corpus_lock_path()).expect("CORPUS_LOCK.toml parses");
    assert_eq!(lock.rules.len(), set.iter().count());
    assert!(lock.rules.len() > 400, "{} entries", lock.rules.len());
}

// ────────────────────────────────────────────────────────────────────────────
// 6. The corpus digest (`corpus_version`)
// ────────────────────────────────────────────────────────────────────────────

fn is_corpus_digest(s: &str) -> bool {
    s.strip_prefix(RULE_HASH_PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

#[test]
fn corpus_version_is_a_sha256_digest() {
    let v = corpus_version(&set_of(vec![versioned("A-1", 1)]));
    assert!(is_corpus_digest(&v), "{v}");
    // Even an empty corpus has a well-formed (and stable) digest.
    let empty = corpus_version(&RuleSet::default());
    assert!(is_corpus_digest(&empty), "{empty}");
    assert_eq!(empty, corpus_version(&RuleSet::default()));
}

/// Two machines that load the same rules in a different order must print the same corpus id.
#[test]
fn corpus_version_does_not_depend_on_load_order() {
    let forward = set_of(vec![
        versioned("A-1", 1),
        versioned("B-1", 2),
        versioned("C-1", 3),
    ]);
    let reversed = set_of(vec![
        versioned("C-1", 3),
        versioned("B-1", 2),
        versioned("A-1", 1),
    ]);
    assert_eq!(corpus_version(&forward), corpus_version(&reversed));
}

/// A rule's content change, a version bump, an added rule and a removed rule each move the id.
#[test]
fn corpus_version_moves_when_the_corpus_content_moves() {
    let base = set_of(vec![versioned("A-1", 1), versioned("B-1", 1)]);
    let before = corpus_version(&base);

    let mut edited = versioned("B-1", 1);
    edited.summary = "A different summary.".to_string();
    assert_ne!(
        before,
        corpus_version(&set_of(vec![versioned("A-1", 1), edited])),
        "a content change must move the corpus version"
    );
    assert_ne!(
        before,
        corpus_version(&set_of(vec![versioned("A-1", 1), versioned("B-1", 2)])),
        "a version bump must move the corpus version"
    );
    assert_ne!(
        before,
        corpus_version(&set_of(vec![
            versioned("A-1", 1),
            versioned("B-1", 1),
            versioned("C-1", 1)
        ])),
        "an added rule must move the corpus version"
    );
    assert_ne!(
        before,
        corpus_version(&set_of(vec![versioned("A-1", 1)])),
        "a removed rule must move the corpus version"
    );
}

/// Recording a review is not a content change: the corpus id a client sees must not churn every
/// time someone confirms a rule is still current.
#[test]
fn corpus_version_ignores_review_dates() {
    let plain = set_of(vec![versioned("A-1", 1)]);
    let mut reviewed = versioned("A-1", 1);
    reviewed.last_reviewed = Some("2026-10-01".to_string());
    assert_eq!(
        corpus_version(&plain),
        corpus_version(&set_of(vec![reviewed]))
    );
}

/// An unversioned rule still contributes (the digest is total), and is distinguishable from the
/// same rule at any real version.
#[test]
fn corpus_version_counts_an_unversioned_rule_and_tells_it_from_a_versioned_one() {
    let unversioned = corpus_version(&set_of(vec![bare_rule("A-1", "universal")]));
    assert!(is_corpus_digest(&unversioned));
    assert_ne!(unversioned, corpus_version(&RuleSet::default()));
    assert_ne!(
        unversioned,
        corpus_version(&set_of(vec![versioned("A-1", 1)]))
    );
}

/// The digest IS the lock's content: recomputed independently here from the bundled
/// `CORPUS_LOCK.toml` (the sorted `(id, version, hash)` lines, framed with the tag), it equals
/// `corpus_version` of the corpus the lock describes. This is the property the export relies on
/// ("two packages built from the same rules carry the same corpus id"), and it ties the digest
/// to the gated file rather than to a second source of truth.
#[tokio::test]
async fn req_the_corpus_version_is_the_digest_of_the_bundled_lock_content() {
    use sha2::{Digest, Sha256};
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return;
    }
    let set = load_corpus(path).await.expect("corpus loads");
    let lock = CorpusLock::load(&corpus_lock_path()).expect("CORPUS_LOCK.toml parses");
    assert!(!lock.rules.is_empty(), "anti-vacuity: the lock has rules");

    let mut hasher = Sha256::new();
    hasher.update(b"camerata-corpus-digest/v1\n");
    // BTreeMap iteration is sorted by id.
    for (id, entry) in &lock.rules {
        hasher.update(format!("{id}\t{}\t{}\n", entry.version, entry.hash).as_bytes());
    }
    let expected = format!(
        "{RULE_HASH_PREFIX}{}",
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(corpus_version(&set), expected);
}
