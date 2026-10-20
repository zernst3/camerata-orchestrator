//! W22 — no option in these 14 rules describes a CAMERATA DETECTION DESIGN instead of a state
//! of the client's code. 14 rules: 9 rewritten to a client-state fault, 5 reduced to a
//! single-option defect.
//!
//! # The shape being removed
//!
//! Each of the 14 was a 2-option `kind = "defect"` rule whose `violates = true` option named a
//! scanner/corpus design that had been considered and rejected — "Flag every SECURITY DEFINER
//! function regardless of grants", "Grep each migration file independently", "Do not author a
//! storage rule; rely on another", "Report both as one finding type". An option is meant to
//! answer "what does this repository do"; those answered "what should the scanner do". The
//! flag is unreachable on a defect-kind rule (excluded from the alternatives pass at
//! `onboard/audit.rs:413`), so this never reached a client — it is an authoring-integrity
//! defect, confusing to the next author and contradicting the guide.
//!
//! # The two resolutions, chosen per rule (Fable-verified, 2026-10-20)
//!
//! - **Rewrite (9):** a concrete, repo-detectable fault state exists that is distinct from
//!   "did not follow the default". The `violates` option is rewritten to NAME that client
//!   state. The rule stays a 2-option defect.
//! - **Delete (5):** the default option already fully defines the required state, so the fault
//!   is simply its absence (and for PERMISSIVE-TRUE / PUBLIC-BUCKET the read-side call is a
//!   deliberate needs_review a violates option would wrongly assert). The `violates` option is
//!   removed, leaving a single-option defect — a proven shape (5 ship today) — with the
//!   rejected-design rationale moved to `[decision].why` / a TOML comment.

use camerata_rules::{corpus_path, load_corpus_lenient, RuleKind};

/// The 9 rewritten rules: `(rule id, the client-state option id its violates option now
/// carries)`.
const REWRITTEN: &[(&str, &str)] = &[
    ("SUPABASE-AUTH-EDGE-JWT-1", "edge-function-disables-jwt-without-compensating-check"),
    (
        "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
        "definer-read-trusts-caller-supplied-id",
    ),
    (
        "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
        "dynamic-sql-assembled-by-concatenation",
    ),
    (
        "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
        "privileged-mutating-function-without-authz-check",
    ),
    ("SUPABASE-FUNC-SEARCH-PATH-1", "definer-function-without-pinned-search-path"),
    ("SUPABASE-KEY-SERVICE-ROLE-CLIENT-1", "service-role-identity-in-client-bundle"),
    // Moved here from DELETED after the pre-existing W12 security gate
    // (`req_w12_security_rules_are_defect_kind_with_exactly_one_violates_option`) caught that
    // these three are security rules required to KEEP a violates option so they can report a
    // fault. Fable's delete classification had not accounted for that invariant; the test did.
    ("SUPABASE-RLS-DISABLED-NOT-RESTORED-1", "table-rls-disabled-and-never-restored"),
    ("SUPABASE-RLS-PERMISSIVE-TRUE-1", "anon-or-public-policy-grants-write-via-true-predicate"),
    ("SUPABASE-STORAGE-OBJECT-POLICY-1", "unscoped-storage-write-policy-for-anon-or-public"),
];

/// The 5 reduced to a single-option defect.
const DELETED: &[&str] = &[
    "SUPABASE-EXPOSURE-MATVIEW-1",
    "SUPABASE-EXPOSURE-SCHEMAS-1",
    "SUPABASE-RLS-ENABLED-1",
    "SUPABASE-RLS-POLICY-DISABLED-1",
    "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
];

/// A label addressed to the SCANNER rather than describing a client-code state. This is the
/// tell the whole shape is defined by; no option in any of the 14 may match it after the fix.
fn reads_as_scanner_design(label: &str) -> bool {
    let l = label.trim_start();
    const OPENERS: &[&str] = &[
        "Flag ", "Grep ", "Regex-grep", "Treat every", "Treat materialized", "Treat the mere",
        "Report ", "Scan ", "Hardcode ", "Ignore ", "Skip reporting", "Do not ", "Rely on",
        "Rely solely", "Apply only", "Apply a name",
    ];
    OPENERS.iter().any(|o| l.starts_with(o))
}

#[tokio::test]
async fn req_w22_rewritten_rules_name_a_client_state() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    for (rule_id, new_opt) in REWRITTEN {
        let rule = corpus
            .get_by_id(rule_id)
            .unwrap_or_else(|| panic!("{rule_id} missing"));
        assert_eq!(rule.kind, Some(RuleKind::Defect), "{rule_id} stays a defect");

        let opt = rule
            .options
            .iter()
            .find(|o| o.id == *new_opt)
            .unwrap_or_else(|| panic!("{rule_id} must carry the rewritten option `{new_opt}`"));
        assert!(
            opt.violates,
            "{rule_id} / {new_opt} is the fault state and must keep `violates = true`"
        );
        assert!(
            !reads_as_scanner_design(&opt.label),
            "{rule_id} / {new_opt} still reads as a scanner instruction: {:?}",
            opt.label
        );
    }
}

#[tokio::test]
async fn req_w22_deleted_rules_are_single_option_defects() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    for rule_id in DELETED {
        let rule = corpus
            .get_by_id(rule_id)
            .unwrap_or_else(|| panic!("{rule_id} missing"));
        assert_eq!(rule.kind, Some(RuleKind::Defect), "{rule_id} stays a defect");
        assert_eq!(
            rule.options.len(),
            1,
            "{rule_id} should be a single-option defect after the rejected-design option is \
             removed; found {}",
            rule.options.len()
        );
        assert!(
            !rule.options[0].violates,
            "{rule_id}'s surviving option is the required state, never the fault"
        );
    }
}

/// The invariant the whole fix exists to establish, checked across all 14 at once: not one
/// option, in any of them, reads as an instruction to Camerata's scanner.
#[tokio::test]
async fn req_w22_no_option_in_the_fourteen_reads_as_scanner_design() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let all: Vec<&str> = REWRITTEN
        .iter()
        .map(|(r, _)| *r)
        .chain(DELETED.iter().copied())
        .collect();
    for rule_id in all {
        let rule = corpus.get_by_id(rule_id).expect("present");
        for opt in &rule.options {
            assert!(
                !reads_as_scanner_design(&opt.label),
                "{rule_id} / {} still describes a scanner design, not a client state: {:?}",
                opt.id,
                opt.label
            );
        }
    }
}
