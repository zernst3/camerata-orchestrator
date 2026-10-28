//! W16: the corpus-load gate for [`camerata_rules::Rule::inspectable`], plus the pin on the
//! exact `inspectable = false` population.
//!
//! The field answers one question per rule: **can the scan assess this rule from a repository
//! at all?** `false` means EMITTED, NEVER SCANNED — the rule is still proposed, armed and
//! installed into the client's `AGENTS.md`/`CONVENTIONS.md` as a directive governing future
//! work, but no scan surface may claim anything about it. The behaviour that makes this
//! necessary, and the end-to-end proof that it holds, live in
//! `crates/server/tests/w16_emit_only_rules_e2e.rs`; this file is the CORPUS half.
//!
//! Three things are asserted, and the third is the one that matters most:
//!
//! 1. **The gate** — every rule in the real shipped corpus declares the field
//!    (`rules_missing_inspectable` is empty). Required, no serde default: a new rule is forced
//!    to decide.
//! 2. **The fire proof** — the gate is shown to actually REPORT a rule that omits the field,
//!    against a synthetic corpus. A gate that cannot fail is the failure class this corpus has
//!    already shipped three times.
//! 3. **The vacuity guard + population pin** — the corpus genuinely EXERCISES the interesting
//!    value, and the exact set of ids carrying it is pinned, so the population cannot drift in
//!    either direction without a deliberate edit here. A rule quietly flipped to `false` stops
//!    being checked for a paying client; a rule quietly flipped to `true` puts an
//!    unassessable claim back into the deliverable. Both are silent today; neither is after
//!    this test.
//!
//! Lives in an integration test rather than `crates/rules/src/lib.rs`'s `mod tests` for the
//! same reason `w14_corpus_schema_gates.rs` and `w11_kind_taxonomy.rs` do.

use camerata_rules::{
    load_corpus, load_corpus_lenient, rules_declaring_emit_only, rules_missing_inspectable,
    RuleSet, DEFAULT_CORPUS_PATH,
};

/// Load the real shipped corpus, or `None` when it is not checked out — mirrors the skip guard
/// every sibling corpus gate test uses.
async fn corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

/// The COMPLETE, pinned set of corpus rules declared `inspectable = false` — "emitted, never
/// scanned". Each entry carries the one-line reason the scan cannot assess it, taken from the
/// rule's own text, not from a family prefix.
///
/// Note how mixed the families are. That is the whole argument for a per-rule field: the old
/// `ORCH-`/`SPIRIT-`/`PROC-` prefix gate W4 removed would have been wrong about
/// `SPIRIT-FILE-SIZE-1`, `PROC-PR-CONCURRENCY-1`, `PROC-HIDE-DEAD-END-1` and four `ORCH-*`
/// pipeline-config rules, all of which stay scannable here.
const EMIT_ONLY_RULES: &[(&str, &str)] = &[
    (
        "SEC-NO-END-OF-LIFE-RUNTIME-1",
        "Its findings come from a dedicated deterministic pass over the versions a repository \
         declares, checked against endoflife.date, never from the rule-scan loop or the AI pass; \
         a rule-scan result would be a verified-clean claim about a feed that pass may not have \
         reached.",
    ),
    (
        "SEC-NO-KNOWN-VULNERABLE-RUNTIME-1",
        "Its findings come from a dedicated deterministic pass that looks declared runtime \
         versions up in the NVD, never from the rule-scan loop or the AI pass; a rule-scan result \
         would be a verified-clean claim about a feed that pass may not have reached.",
    ),
    (
        "AGENTIC-NO-TEST-TAMPER-1",
        "Its real detector inspects an agent's unified diff inside Camerata's own pipeline and \
         never runs during a client scan; the rule's own `qualifies` says the in-repo layer-4 \
         CI check is optional, so its absence is not a violation.",
    ),
    (
        "ORCH-AUTOCALLS-LEDGER-1",
        "A decision ledger and weekly judgment review inside the orchestration harness; no \
         artifact of it exists in the repository being scanned.",
    ),
    (
        "ORCH-BUDGET-MONITOR-1",
        "Spend tracking and per-session model-invocation caps are harness runtime behaviour.",
    ),
    (
        "ORCH-CLEAR-WINNER-1",
        "An in-flight decide-or-stop procedure for an agent; the artifact records the outcome, \
         never whether the test was applied.",
    ),
    (
        "ORCH-CONFLICTING-ROBUSTNESS-1",
        "An escalation rule about recognising a tradeoff during a run; nothing observable \
         afterwards.",
    ),
    (
        "ORCH-CONTEXT-OVERRIDE-1",
        "\"Pause and escalate before overriding a documented rule\" is agent behaviour mid-run.",
    ),
    (
        "ORCH-MODEL-TIERING-1",
        "Which model tier ran which work is a property of how the orchestrator executes, \
         invisible in the client's code. The owner's own example.",
    ),
    (
        "ORCH-NO-NATURAL-BREAK-1",
        "A stop-condition policy for autonomous routines; no repository artifact.",
    ),
    (
        "ORCH-NOVELTY-1",
        "A confidence-calibration policy for agent decisions; no repository artifact.",
    ),
    (
        "ORCH-ONE-WAY-DOOR-1",
        "An escalation-routing policy keyed on blast radius; the repo shows outcomes, never \
         whether the routing happened.",
    ),
    (
        "ORCH-OUTPUT-DIGEST-1",
        "Notification and digest discipline for routines; entirely outside the repository.",
    ),
    (
        "ORCH-PRECISION-RECALL-1",
        "A detector-tuning policy for autonomous signals; no repository artifact.",
    ),
    (
        "ORCH-REVIEWER-SPLIT-1",
        "Author/reviewer agent separation is orchestration configuration; nothing in the \
         client's tree records it.",
    ),
    (
        "ORCH-TIERED-ESCALATION-1",
        "Soft-flag versus hard-pause is harness behaviour only.",
    ),
    (
        "ORCH-TRAINING-CUTOFF-1",
        "A route-to-human policy keyed on decision category; no repository trace.",
    ),
    (
        "PROC-AUTO-MERGE-1",
        "Auto-merge eligibility lives in forge settings and label conventions, not repository \
         content.",
    ),
    (
        "PROC-CITE-CONVENTION-ID-1",
        "Its subject is commit-message and PR-description body text. The scan reads the working \
         tree and no commit messages at all.",
    ),
    (
        "PROC-PERMISSION-CONFIG-1",
        "A triage policy for how a reported access gap is classified before anyone reaches for \
         code; compliance is how humans route reports.",
    ),
    (
        "PROC-REGRESSION-TEST-1",
        "\"A test that fails before the fix and passes after\" is a per-change property \
         requiring diff and history correlation a tree snapshot cannot provide.",
    ),
    (
        "PROC-STORY-DOCS-1",
        "Its trigger is a story completing through the pipeline. A repo that has completed no \
         stories since adopting it is fully compliant and has no docs, so absence is not \
         evidence of violation — the same shape as SPIRIT-DOC-DECISIONS-1.",
    ),
    (
        "PROCESS-ADO-LINK-1",
        "Subject is the commit subject and PR title (`VcsTarget::CommitSubject`/`PrTitle`), \
         never read by the scan. Enforced deterministically at the VCS-action chokepoint \
         instead (`crates/checks/src/vcs_action.rs`).",
    ),
    (
        "PROCESS-BRANCH-NAMING-1",
        "Subject is the branch name; the scan reads files, not refs. Enforced at the VCS-action \
         chokepoint.",
    ),
    (
        "PROCESS-COMMIT-DOC-1",
        "Subject is the commit body and PR body. Enforced at the VCS-action chokepoint.",
    ),
    (
        "PROCESS-CONVENTIONAL-COMMIT-1",
        "Subject is the commit subject line. Enforced at the VCS-action chokepoint.",
    ),
    (
        "SPIRIT-DOC-DECISIONS-1",
        "Its substance is \"written the moment it is made\". A repo with a stale decisions \
         folder passes the proxy while violating the rule; a young repo with no decisions yet \
         fails it while compliant.",
    ),
    (
        "SPIRIT-ROBUSTNESS-1",
        "A tie-break between two CORRECT implementations at authoring time. The scan sees only \
         the one that was written, never the rejected alternative.",
    ),
];

/// Rules in these families that deliberately STAY scannable, with the facet that makes them so.
/// Pinned as the counter-population: if the gate were ever reduced to a family prefix again,
/// this list is what would silently disappear.
const DELIBERATELY_SCANNABLE: &[(&str, &str)] = &[
    (
        "SPIRIT-FILE-SIZE-1",
        "File length is a measurable fact of the tree, and the auto-generated exemption is \
         decidable from path and content. W4's own counter-example to the prefix gate.",
    ),
    (
        "SPIRIT-OPTIMIZE-1",
        "Its remediation is written at a concrete code location (\"bake the performant pattern \
         into THIS code's default shape\"), and N+1 queries, missing indexes and sequential \
         awaits are all observable.",
    ),
    (
        "ORCH-CONFORMANCE-1",
        "Its own `qualifies` makes the in-repo half explicit: \"conformance holds when the \
         PIPELINE CONFIGURATION names that gate\".",
    ),
    (
        "ORCH-ENV-GATED-QUALITY-1",
        "Its own `qualifies`: \"conformance holds when the CI CONFIGURATION shows both the \
         pre-merge gate and an automated promotion gate\".",
    ),
    (
        "ORCH-NEW-PATH-TESTS-1",
        "A per-diff patch-coverage gate is visible in CI configuration when present.",
    ),
    (
        "ORCH-PREREVIEW-1",
        "Its own `qualifies`: \"conformance holds when the PIPELINE CONFIGURATION shows that \
         required reviewer status\".",
    ),
    (
        "PROC-PR-CONCURRENCY-1",
        "The PR template IS a repository file. W4's named example.",
    ),
    (
        "PROC-HIDE-DEAD-END-1",
        "A UI rendering an action the viewer cannot perform is observable in the code. W4's \
         other named example.",
    ),
    (
        "PROC-CI-MIGRATION-HYGIENE-1",
        "A CI migration-lint step and the migrations it lints are both repository content.",
    ),
    (
        "PROC-FEATURE-FLAGS-1",
        "The flag mechanism (a single runtime-checked helper versus environment-conditional \
         code) is plain code shape.",
    ),
    (
        "PROC-MIGRATION-ROLE-GRANTS-1",
        "Its subject is what a migration file contains, and the rule itself makes the \
         external-tool case decidable in-repo: a deferred grant must be DECLARED in the \
         migration so a reviewer can tell it from an omission.",
    ),
];

/// A minimal, valid rule TOML. `inspectable_line` is parameterized so the fire proof can omit
/// exactly that one field and nothing else; everything else is held at a value the other
/// corpus gates accept.
fn rule_toml(id: &str, inspectable_line: &str) -> String {
    format!(
        r#"
id = "{id}"
title = "W16 synthetic fixture rule"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
{inspectable_line}
default = true
verification = "grounded"

[decision]
question = "Synthetic?"
default = "the-correct-option"

[[option]]
id = "the-correct-option"
label = "The correct option"
directive = "Do the correct thing."
why = "The natural fit for the synthetic fixture."
remediation = "Do the correct thing. Verify by observing the correct thing."
effort = "low"
"#
    )
}

fn synthetic_corpus(files: &[(&str, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    for (name, body) in files {
        std::fs::write(universal.join(name), body).expect("write fixture");
    }
    dir
}

// ────────────────────────────────────────────────────────────────────────────
// The gate
// ────────────────────────────────────────────────────────────────────────────

/// req_w16_every_corpus_rule_declares_inspectable: the gate, against the REAL shipped corpus.
#[tokio::test]
async fn req_w16_every_corpus_rule_declares_inspectable() {
    let Some(set) = corpus().await else {
        return;
    };
    let offenders = rules_missing_inspectable(&set);
    assert!(
        offenders.is_empty(),
        "every corpus rule must declare `inspectable` — can the scan assess this rule from a \
         repository at all? There is no default: guessing `true` ships an unassessable rule as \
         \"verified clean\" in a paid deliverable, and guessing `false` silently stops a real \
         check from running. Undeclared:\n  {}",
        offenders.join("\n  ")
    );
}

/// req_w16_the_inspectable_gate_fires: the gate is proven to REPORT, not merely to pass.
#[tokio::test]
async fn req_w16_the_inspectable_gate_fires_on_a_rule_that_omits_the_field() {
    let dir = synthetic_corpus(&[
        (
            "declared.toml",
            rule_toml("W16-FIXTURE-DECLARED-1", "inspectable = true"),
        ),
        ("undeclared.toml", rule_toml("W16-FIXTURE-UNDECLARED-1", "")),
    ]);
    let (set, errs) = load_corpus_lenient(dir.path()).await;
    assert!(errs.is_empty(), "the synthetic corpus must parse: {errs:?}");
    assert_eq!(set.iter().count(), 2, "both fixture rules must load");

    let offenders = rules_missing_inspectable(&set);
    assert_eq!(
        offenders,
        vec!["W16-FIXTURE-UNDECLARED-1".to_string()],
        "the gate must report exactly the rule that omits the field — and NOT fail the parse, \
         because a scan must never refuse to run over corpus state"
    );

    // The runtime fail-safe for an undeclared rule is to KEEP LOOKING, not to go quiet.
    let undeclared = set.get_by_id("W16-FIXTURE-UNDECLARED-1").expect("loaded");
    assert!(
        undeclared.is_inspectable(),
        "an undeclared rule must still be scanned at runtime — the build is what refuses it, \
         not the scan"
    );
}

/// req_w16_inspectable_false_actually_parses: the interesting value round-trips.
#[tokio::test]
async fn req_w16_inspectable_false_round_trips_from_toml() {
    let dir = synthetic_corpus(&[(
        "emit_only.toml",
        rule_toml("W16-FIXTURE-EMIT-ONLY-1", "inspectable = false"),
    )]);
    let (set, errs) = load_corpus_lenient(dir.path()).await;
    assert!(errs.is_empty(), "{errs:?}");
    let rule = set.get_by_id("W16-FIXTURE-EMIT-ONLY-1").expect("loaded");
    assert_eq!(rule.inspectable, Some(false));
    assert!(!rule.is_inspectable());
    assert_eq!(
        rules_declaring_emit_only(&set),
        vec!["W16-FIXTURE-EMIT-ONLY-1".to_string()]
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Vacuity guard + population pin
// ────────────────────────────────────────────────────────────────────────────

/// req_w16_the_emit_only_population_is_pinned: the corpus genuinely exercises the field, and
/// the exact set of rules excluded from the scan is pinned by id.
///
/// This is the test that stops the population drifting. Adding a rule to, or removing one
/// from, `EMIT_ONLY_RULES` is a deliberate edit with a written reason beside it — which is the
/// standard a claim in a paid deliverable has to meet.
#[tokio::test]
async fn req_w16_the_emit_only_population_is_pinned() {
    let Some(set) = corpus().await else {
        return;
    };
    let actual = rules_declaring_emit_only(&set);

    // Vacuity guard: the mechanism is not inert.
    assert!(
        !actual.is_empty(),
        "the corpus must actually USE `inspectable = false`, or the gate above passes \
         vacuously over a field nothing exercises — the `max_severity` lesson"
    );

    let mut expected: Vec<String> = EMIT_ONLY_RULES
        .iter()
        .map(|(id, _)| id.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "the emit-only population drifted. Every id here is EXCLUDED FROM THE SCAN and can \
         never be reported as verified clean, so adding or removing one is a client-facing \
         change: update EMIT_ONLY_RULES with the rule's own reason beside it."
    );
}

/// req_w16_scannable_counter_population_is_pinned: the rules in the same families that
/// deliberately stay scannable. Without this, a future change could quietly collapse the
/// per-rule judgment back into a family prefix and nothing would notice.
#[tokio::test]
async fn req_w16_the_deliberately_scannable_governance_rules_stay_scannable() {
    let Some(set) = corpus().await else {
        return;
    };
    for (id, why) in DELIBERATELY_SCANNABLE {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must exist in the corpus"));
        assert!(
            rule.is_inspectable(),
            "{id} must stay SCANNABLE: {why}\nW16 narrows the W4 ruling for rules the scan \
             cannot assess; it does not resurrect the prefix gate W4 correctly removed."
        );
    }
}

/// req_w16_no_rule_is_in_both_lists: the two pins above must partition, not overlap.
#[test]
fn req_w16_the_two_pinned_populations_are_disjoint() {
    let emit_only: std::collections::HashSet<&str> =
        EMIT_ONLY_RULES.iter().map(|(id, _)| *id).collect();
    let scannable: std::collections::HashSet<&str> =
        DELIBERATELY_SCANNABLE.iter().map(|(id, _)| *id).collect();
    assert!(
        emit_only.is_disjoint(&scannable),
        "a rule cannot be both emit-only and deliberately scannable: {:?}",
        emit_only.intersection(&scannable).collect::<Vec<_>>()
    );
    assert_eq!(
        emit_only.len(),
        EMIT_ONLY_RULES.len(),
        "duplicate id in EMIT_ONLY_RULES"
    );
    for (_, why) in EMIT_ONLY_RULES.iter().chain(DELIBERATELY_SCANNABLE.iter()) {
        assert!(
            why.len() > 40,
            "every pinned rule carries a real reason, not a placeholder: {why:?}"
        );
    }
}
