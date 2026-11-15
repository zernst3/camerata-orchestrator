//! Three defect classes a dense Next.js and Supabase fixture planted and the corpus did not cover:
//! an unbounded list read, a React effect that never cleans up, and an async handler whose
//! rejection nothing handles. Each is a corpus rule now; this test pins the shape each one must
//! keep to do its job: it loads and arms, it is a `defect`, its severity bounds match the class,
//! every source is a published URL, its default option carries a Fix line, its fault option is
//! flagged, and it declares an `applies_to` scope (the routing itself is proved against the
//! production router in `crates/server/tests/applies_to_corpus.rs`).

use camerata_rules::{load_corpus, Rule, RuleKind, DEFAULT_CORPUS_PATH};

struct Expected {
    id: &'static str,
    domain: &'static str,
    min_severity: &'static str,
    max_severity: &'static str,
    default_option: &'static str,
    fault_option: &'static str,
}

const EXPECTED: &[Expected] = &[
    Expected {
        id: "ARCH-BOUNDED-LIST-READS-1",
        domain: "api-layer",
        min_severity: "medium",
        max_severity: "medium",
        default_option: "list-reads-are-bounded-by-a-limit-or-page",
        fault_option: "list-reads-return-the-whole-collection",
    },
    Expected {
        id: "JAVASCRIPT-REACT-EFFECT-CLEANUP-1",
        domain: "javascript:react",
        min_severity: "medium",
        max_severity: "medium",
        default_option: "effects-return-a-cleanup-for-what-they-start",
        fault_option: "effects-start-work-they-never-stop",
    },
    Expected {
        id: "JAVASCRIPT-ASYNC-REJECTION-HANDLED-1",
        domain: "javascript",
        min_severity: "medium",
        max_severity: "high",
        default_option: "async-handlers-catch-and-surface-rejections",
        fault_option: "async-handler-rejections-go-unhandled",
    },
];

async fn corpus() -> camerata_rules::RuleSet {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_CORPUS_PATH);
    load_corpus(&dir).await.expect("the corpus loads strictly")
}

fn rule<'a>(set: &'a camerata_rules::RuleSet, id: &str) -> &'a Rule {
    set.get_by_id(id)
        .unwrap_or_else(|| panic!("{id} must be in the corpus"))
}

#[tokio::test]
async fn each_new_rule_loads_as_an_armed_defect_with_its_severity_bounds() {
    let set = corpus().await;
    for e in EXPECTED {
        let r = rule(&set, e.id);
        assert_eq!(r.domain, e.domain, "{}: folder domain", e.id);
        assert_eq!(r.kind, Some(RuleKind::Defect), "{}: kind", e.id);
        assert_eq!(r.inspectable, Some(true), "{}: a scan can assess it", e.id);
        assert!(r.version.is_some_and(|v| v >= 1), "{}: version", e.id);
        assert!(
            r.is_auto_recommended(),
            "{}: must arm, not sit at draft",
            e.id
        );
        assert_eq!(
            r.min_severity.as_deref(),
            Some(e.min_severity),
            "{}: severity floor",
            e.id
        );
        assert_eq!(
            r.max_severity.as_deref(),
            Some(e.max_severity),
            "{}: severity ceiling",
            e.id
        );
        assert!(
            r.applies_to.is_some(),
            "{}: must declare where it applies so it is routed only to relevant files",
            e.id
        );
    }
}

#[tokio::test]
async fn each_new_rule_cites_published_sources_and_ships_a_fix_on_both_options() {
    let set = corpus().await;
    for e in EXPECTED {
        let r = rule(&set, e.id);
        assert!(!r.sources.is_empty(), "{}: at least one source", e.id);
        for s in &r.sources {
            assert!(
                s.url.starts_with("https://"),
                "{}: source {:?} must be a published URL",
                e.id,
                s.url
            );
            assert!(!s.title.trim().is_empty(), "{}: source title", e.id);
        }
        assert_eq!(
            r.default_option.as_deref(),
            Some(e.default_option),
            "{}: default option",
            e.id
        );
        let default = r
            .options
            .iter()
            .find(|o| o.id == e.default_option)
            .unwrap_or_else(|| panic!("{}: default option must exist", e.id));
        assert!(
            !default.violates,
            "{}: the default is the correct state",
            e.id
        );
        let fault = r
            .options
            .iter()
            .find(|o| o.id == e.fault_option)
            .unwrap_or_else(|| panic!("{}: fault option must exist", e.id));
        assert!(fault.violates, "{}: the fault option is flagged", e.id);
        for o in [default, fault] {
            assert!(
                o.remediation
                    .as_deref()
                    .is_some_and(|t| !t.trim().is_empty()),
                "{}: option {} needs a Fix line",
                e.id,
                o.id
            );
            assert!(
                matches!(o.effort.as_deref(), Some("low" | "medium" | "high")),
                "{}: option {} needs an effort band",
                e.id,
                o.id
            );
        }
        assert!(
            r.decision_question.is_some() && r.decision_why.is_some(),
            "{}: [decision] question and why",
            e.id
        );
    }
}

/// The effect-cleanup rule is the one whose precise half a deterministic check answers, so it is
/// architectural with a review-only checker (2026-11-15 ruling); the other two are judged by the
/// semantic read alone and must not claim a detector they do not have.
#[tokio::test]
async fn only_the_effect_cleanup_rule_claims_a_deterministic_check() {
    let set = corpus().await;
    for e in EXPECTED {
        let r = rule(&set, e.id);
        let ci = r.enforcement.is_ci_enforced();
        if e.id == "JAVASCRIPT-REACT-EFFECT-CLEANUP-1" {
            assert!(ci, "{}: backed by a native checker", e.id);
        } else {
            assert!(!ci, "{}: no detector, so not CI-tier", e.id);
            assert!(
                r.summary.contains("no deterministic check"),
                "{}: `qualifies` must say coverage is the semantic read",
                e.id
            );
        }
    }
}
