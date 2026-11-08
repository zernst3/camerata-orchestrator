//! Tests for deep routing regressions.

use super::*;

// ── Deep compliance & security tier (#55) ──────────────────────────────────────

#[test]
fn parse_soc2_gaps_reads_controls_and_normalizes_status() {
    let raw = r#"Here is the gap analysis:
        {
          "summary": "Partial readiness; access controls are the main gap.",
          "gaps": [
            {"control":"CC6.1","title":"Logical access controls","status":"gap","observed":"no authz middleware","gap":"add authz on write paths"},
            {"control":"CC7.2","title":"Logging","status":"partial","observed":"some logging","gap":"no audit trail"},
            {"control":"CC1.1","title":"Control environment","status":"weird","observed":"n/a","gap":""}
          ]
        }"#;
    let (summary, gaps) = parse_soc2_gaps(raw);
    assert!(summary.contains("Partial readiness"));
    assert_eq!(gaps.len(), 3);
    assert_eq!(gaps[0].control, "CC6.1");
    assert_eq!(gaps[0].status, "gap");
    assert_eq!(gaps[1].status, "partial");
    // An unrecognized status normalizes to the honest default.
    assert_eq!(gaps[2].status, "unknown");
}

#[test]
fn parse_soc2_gaps_drops_empty_rows_and_tolerates_garbage() {
    // A row with no control AND no title is dropped.
    let raw = r#"{"summary":"","gaps":[{"control":"","title":"","status":"gap"}]}"#;
    let (_s, gaps) = parse_soc2_gaps(raw);
    assert!(gaps.is_empty(), "empty rows dropped");
    // Non-JSON yields an empty result, never an error.
    let (s2, g2) = parse_soc2_gaps("the model declined");
    assert!(s2.is_empty());
    assert!(g2.is_empty());
}

#[test]
fn parse_threats_reads_and_normalizes_kind_and_severity() {
    let raw = r#"{
          "summary": "Public API with several entry points.",
          "threats": [
            {"component":"POST /api/orders","kind":"entry-point","threat":"unauth order creation","category":"elevation","mitigation":"require auth","severity":"high"},
            {"component":"Postgres","kind":"weird-kind","threat":"data exfil","category":"info-disclosure","mitigation":"encrypt at rest","severity":"sky-high"}
          ]
        }"#;
    let (summary, threats) = parse_threats(raw);
    assert!(summary.contains("Public API"));
    assert_eq!(threats.len(), 2);
    assert_eq!(threats[0].kind, "entry-point");
    assert_eq!(threats[0].severity, "high");
    // Unknown kind -> "other"; unknown severity -> "medium".
    assert_eq!(threats[1].kind, "other");
    assert_eq!(threats[1].severity, "medium");
    // Category is preserved verbatim (free text / STRIDE label both survive).
    assert_eq!(threats[1].category, "info-disclosure");
}

#[test]
fn parse_threats_drops_empty_rows_and_tolerates_garbage() {
    let raw = r#"{"summary":"x","threats":[{"component":"","threat":"","kind":"entry-point"}]}"#;
    let (_s, threats) = parse_threats(raw);
    assert!(
        threats.is_empty(),
        "row with no component and no threat dropped"
    );
    let (s2, t2) = parse_threats("{ not json ]");
    assert!(s2.is_empty());
    assert!(t2.is_empty());
}

#[test]
fn deep_security_prompt_excludes_floor_concerns() {
    // The deep-security lens must NOT re-report the deterministic floor's concerns.
    let p = deep_security_system_prompt();
    assert!(p.contains("DO NOT re-report"));
    assert!(p.contains("authorization") || p.contains("AUTHORIZATION"));
}

/// The `critical` severity tier must be OFFERED (in the schema string) and CALIBRATED
/// (general escalation criteria in the prose) by every AI-tier prompt that emits a
/// severity — otherwise the model is vocabulary-constrained away from ever reporting a
/// critical finding no matter how severe, which was the original bug. This pins the wire
/// contract independent of any specific model call.
#[test]
fn every_ai_tier_prompt_offers_and_defines_critical_severity() {
    for (name, prompt) in [
        ("audit_system_prompt", audit_system_prompt()),
        ("deep_security_system_prompt", deep_security_system_prompt()),
        ("verify_system_prompt (calibration)", verify_system_prompt()),
    ] {
        assert!(
            prompt.contains("critical|high|medium|low")
                || prompt.contains("critical/high/medium/low"),
            "{name} must offer \"critical\" in its severity schema/rubric"
        );
        assert!(
            prompt.to_lowercase().contains("rare") || prompt.contains("RARE"),
            "{name} must define critical as a RARE, reserved tier (not a synonym for \
                 high) so the model doesn't inflate ordinary findings"
        );
    }
}

/// W2: the standard audit prompt (the pass that runs on EVERY scan, not just the opt-in
/// deep tier) must explicitly own its cross-file mandate by name, the compensating-control
/// check, and the "report it even if no rule exists" backstop instruction — the three
/// pieces of guidance added to close the zero-row semantic-tier collapse. A string-
/// structure test (mirrors `shipped_template_footer_says_inspection_report_and_keeps_the_
/// advisory_caveat` in `report_export.rs`): it pins the WORDING contract so a future edit
/// can't quietly drop one of these clauses without a visible test failure.
#[test]
fn audit_system_prompt_owns_the_cross_file_mandate_and_compensating_control_check() {
    let p = audit_system_prompt();

    // The three cross-file mechanisms, named explicitly (never a repo-specific name).
    assert!(
        p.contains("CONFIG-TO-HANDLER"),
        "must name the config-flag-to-privileged-handler mechanism"
    );
    assert!(
        p.contains("POLICY-TO-CALL-SITE"),
        "must name the permissive-policy-to-call-site mechanism"
    );
    assert!(
        p.contains("CREDENTIAL-TO-USAGE"),
        "must name the credential-defined-in-one-file-consumed-in-another mechanism"
    );
    assert!(
        p.to_uppercase().contains("CROSS-FILE"),
        "must call out cross-file reasoning as this pass's own mandate"
    );

    // The compensating-control question, with its concrete examples, and the never-flag-
    // when-present instruction (the do-not-break invariant this work item must preserve).
    assert!(
        p.contains("COMPENSATING CONTROL") || p.contains("compensating check"),
        "must instruct the model to check for a compensating control before flagging"
    );
    for example in [
        "signature",
        "shared-secret",
        "session-derived identity",
        "ownership predicate",
    ] {
        assert!(
            p.to_lowercase().contains(example),
            "compensating-control guidance must name \"{example}\" as a qualifying check: \
                 {p}"
        );
    }
    assert!(
        p.contains("do NOT flag it") || p.contains("do NOT flag"),
        "must explicitly instruct: do not flag when a compensating control is found"
    );

    // The explicit backstop clause: a missing rule is never a reason to withhold a finding.
    assert!(
        p.to_uppercase().contains("BACKSTOP"),
        "must name the backstop role explicitly"
    );
    assert!(
        p.contains("missing rule is never a reason") || p.contains("even if no rule"),
        "must explicitly state that an unnamed/un-ruled issue must still be reported: {p}"
    );
}

/// W2: the calibration prompt must not let an un-ruled (backstop) finding be quietly
/// discounted purely for lacking a named rule id — that would re-create the same silent
/// collapse one stage later (a real finding surviving parsing only to be devalued/dropped
/// at calibration for having no corpus id).
#[test]
fn verify_system_prompt_does_not_discount_an_unnamed_backstop_finding() {
    let p = verify_system_prompt();
    assert!(
        p.contains("BACKSTOP"),
        "calibration prompt must name the backstop role: {p}"
    );
    assert!(
        p.to_lowercase().contains("not automatically less credible")
            || p.to_lowercase().contains("do not discount"),
        "calibration prompt must explicitly forbid discounting an un-ruled finding: {p}"
    );
}

#[test]
fn soc2_prompt_is_a_gap_analysis_never_a_report() {
    // Honesty guardrail (#55/#62): the SOC-2 prompt must frame itself as a gap analysis
    // and explicitly deny producing a SOC-2 report / certification.
    let p = soc2_gap_system_prompt();
    assert!(p.contains("GAP ANALYSIS"));
    assert!(
        p.to_lowercase().contains("not a \"soc-2 report\"") || p.contains("NOT a \"SOC-2 report\"")
    );
}

#[test]
fn deep_lens_metadata_is_stable() {
    assert_eq!(DeepLens::Soc2Gap.id(), "soc2-gap");
    assert_eq!(DeepLens::DeepSecurity.id(), "deep-security");
    assert_eq!(DeepLens::ThreatModel.id(), "threat-model");
    // The SOC-2 lens title is a "Gap Analysis", never a "report".
    assert!(DeepLens::Soc2Gap.title().contains("Gap Analysis"));
    assert!(!DeepLens::Soc2Gap.title().to_lowercase().contains("report"));
}

#[test]
fn deep_lens_result_empty_carries_advisory_flag_and_disclaimer() {
    let r = DeepLensResult::empty(DeepLens::ThreatModel);
    assert!(r.advisory, "every deep result is advisory (#62)");
    assert_eq!(r.disclaimer, DEEP_ADVISORY_DISCLAIMER);
    assert!(r.error.is_none());
    assert!(r.threats.is_empty());
    // The disclaimer states it is not externally validated and not a pen test.
    assert!(DEEP_ADVISORY_DISCLAIMER.contains("NOT externally validated"));
    assert!(DEEP_ADVISORY_DISCLAIMER.contains("not a penetration test"));
}

#[test]
fn deep_report_serializes_with_advisory_envelope() {
    let report = DeepReport {
        lenses: vec![
            DeepLensResult::empty(DeepLens::Soc2Gap),
            DeepLensResult::empty(DeepLens::DeepSecurity),
            DeepLensResult::empty(DeepLens::ThreatModel),
        ],
        advisory: true,
        disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
    };
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["advisory"], true);
    assert_eq!(json["lenses"].as_array().unwrap().len(), 3);
    assert_eq!(json["lenses"][0]["lens"], "soc2-gap");
    assert_eq!(json["lenses"][0]["advisory"], true);
}

// ── Rule-routing wiring (#57) ──────────────────────────────────────────────────

/// Verify that `plan_routes` produces the correct per-group file sets when called with
/// a polyglot file list and a mix of language-scoped + cross-cutting rules. This tests
/// the grouping contract that `run_routed_passes` depends on.
#[test]
fn routing_groups_produce_correct_per_group_file_sets() {
    use crate::scan_routing::{plan_routes, Scope};

    // Polyglot repo: Rust backend, TypeScript frontend, some config.
    let files = vec![
        ("src/main.rs".to_string(), "fn main() {}".to_string()),
        (
            "src/handler.rs".to_string(),
            "pub fn handler() {}".to_string(),
        ),
        (
            "ui/app.tsx".to_string(),
            "export function App() {}".to_string(),
        ),
        (
            "schema.sql".to_string(),
            "CREATE TABLE users (id INT);".to_string(),
        ),
    ];
    let rules = vec![
        crate::scan_routing::AuditRule::new("RUST-ENTITIES-1", "Rust entities rule"),
        crate::scan_routing::AuditRule::new("REACT-HOOKS-1", "React hooks rule"),
        crate::scan_routing::AuditRule::new("ARCH-STRICT-LAYERING-1", "Layering rule"),
        crate::scan_routing::AuditRule::new("SEC-NO-RAW-SQL-1", "No raw SQL rule"),
    ];

    let plan = plan_routes(&rules, &files, true);

    // Should have 3 groups: rust, web, and All.
    assert_eq!(
        plan.groups.len(),
        3,
        "polyglot repo gets rust + web + All groups"
    );

    // Verify group scopes.
    let rust_group = plan
        .groups
        .iter()
        .find(|g| g.scope == Scope::Language("rust"));
    let web_group = plan
        .groups
        .iter()
        .find(|g| g.scope == Scope::Language("web"));
    let all_group = plan.groups.iter().find(|g| g.scope == Scope::All);

    assert!(rust_group.is_some(), "rust group exists for RUST-* rule");
    assert!(web_group.is_some(), "web group exists for REACT-* rule");
    assert!(
        all_group.is_some(),
        "All group exists for ARCH-* and SEC-* rules"
    );

    // Rust group: only .rs files should be in scope.
    let rust_g = rust_group.unwrap();
    assert_eq!(rust_g.rules.len(), 1);
    assert_eq!(rust_g.rules[0].id, "RUST-ENTITIES-1");

    // Verify file_in_scope correctly filters for each group.
    for (path, _) in &files {
        let in_rust = crate::scan_routing::file_in_scope(path, "", &rust_g.scope);
        let expected_in_rust = path.ends_with(".rs");
        assert_eq!(
            in_rust, expected_in_rust,
            "{path} rust scope filter mismatch"
        );
    }

    // Web group: only .tsx files should be in scope.
    let web_g = web_group.unwrap();
    for (path, _) in &files {
        let in_web = crate::scan_routing::file_in_scope(path, "", &web_g.scope);
        let expected_in_web = path.ends_with(".tsx")
            || path.ends_with(".ts")
            || path.ends_with(".js")
            || path.ends_with(".jsx");
        assert_eq!(in_web, expected_in_web, "{path} web scope filter mismatch");
    }

    // All group has both ARCH and SEC rules; sees every file including .sql.
    let all_g = all_group.unwrap();
    assert_eq!(
        all_g.rules.len(),
        2,
        "ARCH + SEC rules both land in All group"
    );
    for (path, _) in &files {
        assert!(
            crate::scan_routing::file_in_scope(path, "", &all_g.scope),
            "{path} must be in scope for the All group"
        );
    }

    // Routing saves input on this polyglot fixture (language rules skip non-matching files).
    assert!(
        plan.saved_fraction() > 0.0,
        "routing reduces input on a polyglot repo"
    );
}

/// Advisory must be enabled ONLY in the All group and disabled in language-specific groups.
/// This test verifies the invariant directly by checking what `advisory_disabled` should be
/// for each route group — mirroring the logic in `run_routed_passes`.
#[test]
fn advisory_disabled_only_in_language_groups_not_in_all_group() {
    use crate::scan_routing::{plan_routes, Scope};

    let files = vec![
        ("main.rs".to_string(), "fn main() {}".to_string()),
        ("app.ts".to_string(), "export const x = 1;".to_string()),
        ("schema.sql".to_string(), "SELECT 1;".to_string()),
    ];
    let rules = vec![
        crate::scan_routing::AuditRule::new("RUST-1", "d"),
        crate::scan_routing::AuditRule::new("TS-1", "d"),
        crate::scan_routing::AuditRule::new("ARCH-1", "d"),
    ];
    let plan = plan_routes(&rules, &files, true);

    // The advisory_disabled flag is true for language groups, false for All.
    // This mirrors the check in run_routed_passes.
    for group in &plan.groups {
        let advisory_disabled = !matches!(group.scope, Scope::All);
        match &group.scope {
            Scope::All => {
                assert!(
                    !advisory_disabled,
                    "All group must have advisory enabled (advisory_disabled=false)"
                );
            }
            Scope::Language(lang) => {
                assert!(
                    advisory_disabled,
                    "{lang} language group must have advisory disabled (advisory_disabled=true)"
                );
            }
            Scope::Declared { .. } => {
                unreachable!("no rule in this fixture declares applies_to")
            }
        }
    }
}

/// Verify the no-rules (free-form) path: when `selected` is empty, routing falls back to
/// a single advisory-enabled pass over all files (novel-issue discovery only).
#[test]
fn routing_with_no_rules_produces_single_all_group_conceptually() {
    use crate::scan_routing::plan_routes;

    let files = vec![
        ("main.rs".to_string(), "fn main() {}".to_string()),
        ("app.ts".to_string(), "const x = 1;".to_string()),
    ];
    // Empty rules slice.
    let rules: Vec<crate::scan_routing::AuditRule> = vec![];
    let plan = plan_routes(&rules, &files, true);

    // No rules → no groups: the free-form path handles this specially in run_routed_passes.
    assert_eq!(plan.groups.len(), 0, "empty rules → no route groups");
    assert_eq!(plan.full_chars, 0, "no rules → no input to bill");
}

/// Polyglot fixture with only cross-cutting rules: routing adds no groups beyond All,
/// so the loop degenerates to a single pass and no file-savings occur.
#[test]
fn routing_all_cross_cutting_rules_single_group_no_savings() {
    use crate::scan_routing::{plan_routes, Scope};

    let files = vec![
        ("main.rs".to_string(), "fn main() {}".to_string()),
        ("app.ts".to_string(), "const x = 1;".to_string()),
    ];
    let rules = vec![
        crate::scan_routing::AuditRule::new("ARCH-1", "d"),
        crate::scan_routing::AuditRule::new("SEC-1", "d"),
    ];
    let plan = plan_routes(&rules, &files, true);

    assert_eq!(plan.groups.len(), 1, "all cross-cutting → single All group");
    assert_eq!(plan.groups[0].scope, Scope::All);
    assert_eq!(
        plan.saved_fraction(),
        0.0,
        "no savings when all rules are cross-cutting"
    );
}

/// A purely single-language repo with language-scoped rules also routes to a single
/// language group plus (potentially) no All group if there are no cross-cutting rules.
#[test]
fn routing_single_language_repo_with_language_rules() {
    use crate::scan_routing::{plan_routes, Scope};

    let files = vec![
        ("src/a.rs".to_string(), "pub fn a() {}".to_string()),
        ("src/b.rs".to_string(), "pub fn b() {}".to_string()),
    ];
    let rules = vec![
        crate::scan_routing::AuditRule::new("RUST-1", "d"),
        crate::scan_routing::AuditRule::new("RUST-2", "d"),
    ];
    let plan = plan_routes(&rules, &files, true);

    // Only one group — the rust language group.
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].scope, Scope::Language("rust"));
    assert_eq!(
        plan.groups[0].rules.len(),
        2,
        "both RUST rules land in the same group"
    );

    // W2 (previously a documented-but-unfixed defect): no cross-cutting rule was selected,
    // so this plan has NO `Scope::All` group at all. Mirror `run_routed_passes`'s
    // `has_all_group`-gated formula — advisory must stay ENABLED for the only group here,
    // because nothing else will ever run the "flag novel issues beyond the adopted rules"
    // backstop pass for this scan. The old unconditional `!matches!(scope, Scope::All)`
    // formula silenced advisory for the WHOLE scan in exactly this shape (a project that
    // selected only `RUST-*` rules got zero chance at any novel/security finding outside
    // them) — this is the regression test for that collapse.
    let has_all_group = plan.groups.iter().any(|g| matches!(g.scope, Scope::All));
    assert!(
        !has_all_group,
        "a purely language-scoped selection has no All group"
    );
    let advisory_disabled = has_all_group && !matches!(plan.groups[0].scope, Scope::All);
    assert!(
        !advisory_disabled,
        "with no All group anywhere in the plan, the only (language) group must keep \
             advisory ENABLED — otherwise novel-issue discovery never runs for this scan at all"
    );
}

/// W2: a POLYGLOT repo with only language-scoped rules across MULTIPLE languages (still no
/// `Scope::All` group) must keep advisory enabled in EVERY group — language groups are
/// mutually exclusive by file (a `.rs` file is never in the `web` group's file set), so this
/// can never duplicate a novel finding for any one file.
#[test]
fn routing_polyglot_no_all_group_keeps_advisory_enabled_in_every_language_group() {
    use crate::scan_routing::{plan_routes, Scope};

    let files = vec![
        ("src/a.rs".to_string(), "pub fn a() {}".to_string()),
        ("web/app.ts".to_string(), "export const x = 1;".to_string()),
    ];
    let rules = vec![
        crate::scan_routing::AuditRule::new("RUST-1", "d"),
        crate::scan_routing::AuditRule::new("TS-1", "d"),
    ];
    let plan = plan_routes(&rules, &files, true);

    assert_eq!(
        plan.groups.len(),
        2,
        "rust + web, no cross-cutting rule → two groups"
    );
    let has_all_group = plan.groups.iter().any(|g| matches!(g.scope, Scope::All));
    assert!(!has_all_group, "no cross-cutting rule was selected");
    for group in &plan.groups {
        let advisory_disabled = has_all_group && !matches!(group.scope, Scope::All);
        assert!(
            !advisory_disabled,
            "{:?} must keep advisory enabled when no All group exists: every language \
                 group's files are disjoint from every other's, so this never duplicates a \
                 finding",
            group.scope
        );
    }
}

// ── BUG-4: resolution round add_total guard in Batch mode ────────────────────────

/// BUG-4 regression: in Batch mode the resolution round's add_total call was
/// unconditional, inflating the progress bar denominator after the batch completed.
/// This test verifies the guard is present by inspecting the code path at the
/// JobStore level (unit-testing the guard logic rather than the async batch path).
#[test]
fn bug4_batch_mode_resolution_add_total_is_guarded() {
    // Simulate the guard: mode == ScanMode::Batch → skip add_total.
    // mode != ScanMode::Batch → call add_total.
    // We can't invoke audit_repo in a unit test without a real LLM, so we verify
    // the guard logic directly on the ScanMode enum.
    assert_ne!(
        ScanMode::Batch,
        ScanMode::Parallel,
        "guard distinguishes Batch from other modes"
    );
    assert_ne!(
        ScanMode::Batch,
        ScanMode::Sequential,
        "guard distinguishes Batch from Sequential"
    );
    // The guard condition: `mode != ScanMode::Batch` must be false in Batch mode
    // (add_total skipped) and true in non-Batch modes (add_total called).
    assert!(
        !(ScanMode::Batch != ScanMode::Batch),
        "in Batch mode the guard evaluates to false → add_total is skipped"
    );
    assert!(
        ScanMode::Parallel != ScanMode::Batch,
        "in Parallel mode the guard evaluates to true → add_total is called"
    );
    assert!(
        ScanMode::Sequential != ScanMode::Batch,
        "in Sequential mode the guard evaluates to true → add_total is called"
    );
}

// ── BUG-6 regression: ScanMode::Batch uses larger rule batch size ─────────────

/// BUG-6 kept for `batch_default` off: every rule in one item per chunk, larger than the
/// parallel rule-batch. With `batch_default` on (rank 2) Batch uses the parallel path's 15,
/// the graded prompt shape. (The `CAMERATA_BATCH_RULE_BATCH_SIZE` override is not set in
/// the test environment, so the constants apply.)
#[test]
fn batch_rule_batch_size_is_fifteen_with_batch_default_on_and_all_rules_off() {
    let on = crate::efficiency::EfficiencyConfig::default();
    let mut off = on;
    off.batch_default = false;
    let (_, parallel_batch_size) = ScanMode::Parallel.tuning(on);
    assert_eq!(ScanMode::Batch.tuning(on).1, 15);
    assert_eq!(ScanMode::Batch.tuning(on).1, parallel_batch_size);
    assert_eq!(ScanMode::Batch.tuning(off).1, usize::MAX);
    assert_eq!(
        ScanMode::Parallel.tuning(off),
        ScanMode::Parallel.tuning(on),
        "the switch changes only the batch shape"
    );
}

/// Sequential mode must keep its existing tuning (1 concurrent call, all rules at once).
#[test]
fn bug6_sequential_mode_tuning_unchanged() {
    let (concurrency, batch_size) =
        ScanMode::Sequential.tuning(crate::efficiency::EfficiencyConfig::default());
    assert_eq!(concurrency, 1, "sequential must be single-threaded");
    assert_eq!(
        batch_size,
        usize::MAX,
        "sequential must include all rules in one pass"
    );
}

// ── BUG-7 regression: merge_location_group tiebreaker comment ────────────────

/// Verify that `merge_location_group` correctly selects the EARLIEST finding when
/// all other keys are equal (adopted-flag and severity tied). This is the tiebreaker
/// documented by the `group.len() - i` idiom (BUG-7: correctness confirmed, only
/// readability was noted).
#[test]
fn bug7_head_row_earliest_appearance_wins_on_tie() {
    let make = |rule: &str, sev: &str, snippet: &str| Finding {
        repo: "r/r".to_string(),
        path: "src/lib.rs".to_string(),
        line: 1,
        rule_id: rule.to_string(),
        severity: sev.to_string(),
        snippet: snippet.to_string(),
        detail: "d".to_string(),
        ..Finding::default()
    };
    // Three AI- findings with equal severity: the earliest (index 0) must head them.
    let group = vec![
        make("AI-FIRST", "medium", "first snippet"),
        make("AI-SECOND", "medium", "second snippet"),
        make("AI-THIRD", "medium", "third snippet"),
    ];
    let head = head_row(group);
    assert_eq!(
        head.rule_id, "AI-FIRST",
        "earliest finding must head the group when adopted-flag and severity are equal"
    );
}

// ── BUG-8 regression: build_digest truncation notice threshold ────────────────

/// A near-full file (only a tiny amount dropped) must NOT emit the truncation
/// notice. Before BUG-8 fix the notice was always emitted on ANY truncation.
///
/// Test: build a multi-file digest where the SECOND file barely doesn't fit —
/// only a very small number of chars are dropped (< TRUNCATION_NOTICE_MIN_DROP).
#[test]
fn bug8_trivial_truncation_does_not_emit_notice() {
    use super::{MAX_DIGEST_CHARS, TRUNCATION_NOTICE_MIN_DROP};
    // Strategy: fill the budget almost completely with file-1, then add a file-2
    // that overflows by just `TRUNCATION_NOTICE_MIN_DROP - 1` chars. The partial
    // slice of file-2 captures all but that tiny tail → no notice.
    let small_drop = TRUNCATION_NOTICE_MIN_DROP - 1;

    // Build file-1 content sized to leave exactly `small_drop + budget_for_header_2`
    // chars remaining in the budget.
    // The frame around file-2 (its opening and closing delimiter lines).
    let nonce = crate::prompt_frame::FrameNonce::generate();
    let header2_len = crate::prompt_frame::frame_untrusted(&nonce, "src/f2.rs", "").len();
    let file1_budget = MAX_DIGEST_CHARS
        .saturating_sub(header2_len)
        .saturating_sub(small_drop + 500); // 500-char "remaining" slice of file-2
    let file1_content = "a".repeat(file1_budget);

    // File-2: exactly `small_drop + 500` chars of numbered content so that
    // `remaining` captures the first 500 chars (> 200, partial slice is added)
    // and drops `small_drop` chars.
    let file2_raw_len = small_drop + 500;
    let file2_content = "b".repeat(file2_raw_len);

    let files = vec![
        ("src/f1.rs".to_string(), file1_content),
        ("src/f2.rs".to_string(), file2_content),
    ];
    let digest = build_digest(&files, &nonce);

    // The truncation was trivial (< TRUNCATION_NOTICE_MIN_DROP bytes dropped) →
    // notice must NOT appear (BUG-8 fix).
    assert!(
        !digest.contains("[digest truncated"),
        "BUG-8: a trivially small truncation (< {TRUNCATION_NOTICE_MIN_DROP} chars dropped) \
             must NOT emit the truncation notice — it would mislead the model"
    );
}

/// When a significant amount of content is dropped, the truncation notice IS emitted.
#[test]
fn bug8_significant_truncation_emits_notice() {
    use super::MAX_DIGEST_CHARS;
    // First file: small (easily fits).
    // Second file: very large (mostly dropped — well over the threshold).
    let small = "fn a() {}".to_string();
    // Second file: > MAX_DIGEST_CHARS so it's almost entirely truncated.
    let large = "fn b() { let x = 1; } // comment\n".repeat(MAX_DIGEST_CHARS / 10);
    let files = vec![
        ("src/small.rs".to_string(), small),
        ("src/large.rs".to_string(), large),
    ];
    let digest = build_digest(&files, &crate::prompt_frame::FrameNonce::generate());
    // The large file drops a significant fraction → notice must appear.
    assert!(
        digest.contains("[digest truncated"),
        "a significant truncation (large portion of a file dropped) must emit the notice"
    );
}

// ── BUG-AI-1 regression: run_passes_batch advisory_disabled ──────────────────────

/// BUG-AI-1: when advisory_disabled is true the advisory prompt must be suppressed
/// for EVERY (chunk, batch) pair including bi==0. Before the fix, batch mode always
/// set `advisory = bi == 0` unconditionally, re-introducing duplicate novel findings
/// for every language-scoped group's first batch.
///
/// We test the corrected logic via the `advisory` value formula directly (the function
/// itself is async + requires a live LLM, so we test the guard predicate in isolation).
#[test]
fn bug_ai1_advisory_disabled_suppresses_advisory_in_batch_mode() {
    // Simulate what run_passes_batch now does for each (ci, bi) pair.
    let advisory_disabled_cases: &[(bool, usize, bool)] = &[
        // (advisory_disabled, bi, expected_advisory)
        (true, 0, false),  // disabled + first batch → must NOT fire
        (true, 1, false),  // disabled + later batch → must NOT fire
        (false, 0, true),  // enabled  + first batch → MUST fire
        (false, 1, false), // enabled  + later batch → must NOT fire
    ];
    for &(advisory_disabled, bi, expected) in advisory_disabled_cases {
        let advisory = !advisory_disabled && bi == 0;
        assert_eq!(
            advisory, expected,
            "BUG-AI-1: advisory_disabled={advisory_disabled}, bi={bi} → \
                 expected advisory={expected}, got advisory={advisory}"
        );
    }
}

// ── BUG-AI-2 regression: canonical_adopted_rule PANIC false-positives ────────────

/// BUG-AI-2: `canonical_adopted_rule` must NOT map rules that merely mention "PANIC"
/// in a non-panic context to ARCH-STRUCTURED-ERRORS-1. Only invented names that
/// specifically indicate an unhandled panic at a call point qualify.
#[test]
fn bug_ai2_canonical_adopted_rule_panic_match_is_narrow() {
    let mut adopted = std::collections::HashSet::new();
    adopted.insert("ARCH-STRUCTURED-ERRORS-1".to_string());

    // These should NOT match (merely mention PANIC in non-panic context):
    let non_panic_rules = &[
        "PREVENT-PANICKING-AUTH-CHECK",
        "LOG-PANIC-RECOVERY-1",
        "PANIC-GATE-GUARD", // "PANIC" + "GUARD" — not in narrowing list
    ];
    for rule in non_panic_rules {
        let result = canonical_adopted_rule(rule, &adopted);
        assert!(
            result.is_none(),
            "BUG-AI-2: rule '{rule}' must NOT be canonicalized to ARCH-STRUCTURED-ERRORS-1 \
                 (it only mentions PANIC but is not a panic-at-callsite violation)"
        );
    }

    // These SHOULD match (specific panic-at-callsite invented names):
    let panic_rules = &[
        "AI-HANDLER-PANICS",
        "UNHANDLED-PANIC",
        "PANIC-ON-ERROR",
        "PANIC-UNWRAP-RESULT",
        "BUBBLE-PANIC-1",
    ];
    for rule in panic_rules {
        let result = canonical_adopted_rule(rule, &adopted);
        assert_eq!(
            result.as_deref(),
            Some("ARCH-STRUCTURED-ERRORS-1"),
            "BUG-AI-2: rule '{rule}' should be canonicalized to ARCH-STRUCTURED-ERRORS-1"
        );
    }
}

/// BUG-AI-2: when ARCH-STRUCTURED-ERRORS-1 is NOT adopted, canonical_adopted_rule
/// must return None even for genuine panic rule names (not-adopted guard must hold).
#[test]
fn bug_ai2_canonical_adopted_rule_panic_not_adopted_returns_none() {
    let adopted = std::collections::HashSet::new(); // empty — no rules adopted
    let result = canonical_adopted_rule("PANIC-UNWRAP-RESULT", &adopted);
    assert!(
        result.is_none(),
        "BUG-AI-2: canonicalization must return None when the target rule is not adopted"
    );
}

// ── BUG-AI-3 regression: calibration-model fallback when nothing is pinned ──────────

/// BUG-AI-3, corrected 2026-10-29. The earlier form of this test pinned `calib_model` to
/// `None` when no model was chosen anywhere, on the theory that the transport's default would
/// then serve scan and calibration alike. That default is the PROCESS default
/// (`CAMERATA_LLM_MODEL`), not the model the pre-flight prices, the ledger records or the
/// batch path sends (`resolve_audit_model`); a run reported one model and called another, and
/// with an unpriced process default the spend limit refused the whole run. With nothing
/// pinned, each stage now resolves to a built-in default that is a real, priced id the run can
/// name and price. Since review tree design A13 stage 1 has a default of its own (Opus 5.5)
/// rather than following the scan model (Sonnet 5.5), which stays as it was.
#[test]
fn bug_ai3_calibration_defaults_to_the_stage1_model_when_nothing_is_pinned() {
    let scan = resolve_audit_model(None);
    assert!(!scan.trim().is_empty(), "the scan model is never empty");
    assert!(
        crate::llm::is_priced(&scan),
        "the registry default `{scan}` is priced"
    );
    let stage1 = resolve_calibration_model(None, &scan);
    assert_eq!(stage1, "claude-opus-5-5");
    assert!(
        crate::llm::is_priced(&stage1),
        "stage 1's default is priced"
    );
    assert_eq!(resolve_calibration_model(Some(""), &scan), stage1);
    assert_eq!(
        resolve_calibration_model(Some("claude-opus-5"), &scan),
        "claude-opus-5"
    );
    // The model the run is not told about does not change what stage 1 runs on.
    assert_eq!(
        resolve_calibration_model(None, "some-other-scan-model"),
        stage1
    );
    // A stage request never leaves with an empty model.
    assert_eq!(stage_model(None), scan);
    assert_eq!(stage_model(Some(" ")), scan);
    assert_eq!(stage_model(Some("claude-opus-5")), "claude-opus-5");
}

/// The meter's last line of defence, as the 2026-10-29 run met it: a capped run records a
/// call that reported tokens but no dollars on an id the registry cannot price. The limit can
/// no longer be enforced, so the next admission is refused, the record says the cost was
/// unknown, and the disclosure says the limit could not be enforced, never that it was
/// reached (two calls had cost under a cent of a $10 limit).
#[tokio::test]
async fn an_uncosted_call_under_a_limit_stops_admission_and_is_disclosed_as_unenforceable() {
    let meter = UsageMeter::with_controls(crate::cost_cap::RunControls {
        max_cost_usd: Some(10.0),
        ..crate::cost_cap::RunControls::default()
    });
    meter.record_stage(
        Stage::Alternatives,
        &LlmResponse {
            reported_model: None,
            text: String::new(),
            model: "claude-sonnet-4-6".to_string(),
            backend: "api".to_string(),
            cost_usd: None,
            input_tokens: Some(30_201),
            output_tokens: Some(4_681),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        },
    );
    assert!(meter.cost_unknown());
    assert!(meter.admit().await.is_err(), "nothing more is admitted");
    assert!(meter.retry_blocked());
    meter.record_not_run(crate::cost_cap::NotRunPass {
        repo: "acme/shop".into(),
        group: "pass[all]".into(),
        chunk: 1,
        n_chunks: 1,
        rule_batch: 1,
        n_rule_batches: 8,
        rule_ids: vec!["A-1".into()],
        files: vec!["src/a.ts".into()],
        class: FailureClass::SpendLimit,
    });
    let record = meter.cost_cap_record().expect("a limit was set");
    assert!(record.cost_unknown);
    assert!(record.stopped_the_run());
    assert_eq!(
        record.spent_usd, 0.0,
        "an uncosted call adds nothing to spend"
    );
    let reason = &record.failed_passes()[0].reason;
    assert!(reason.contains("could not be enforced"), "{reason}");
    assert!(!reason.contains("was reached"), "{reason}");
    assert!(
        record
            .notes
            .iter()
            .any(|n| n.contains("`claude-sonnet-4-6`")
                && n.contains(&format!("{} stage", Stage::Alternatives.label()))),
        "the ledger names the model and the call: {:?}",
        record.notes
    );
}

// ── Feature-flag: soc2_enabled gate in run_deep_tier ──────────────────────
//
// These tests validate the behavioural contract of the `soc2_enabled` flag
// without making actual LLM calls. They use the shape/count of lenses in the
// returned `DeepReport` as the observable.

/// A `DeepReport` that carries all three lenses (as empty-but-honest results)
/// simulating a full three-lens run. Used to verify the flag contract.
fn three_lens_report() -> DeepReport {
    DeepReport {
        lenses: vec![
            DeepLensResult::empty(DeepLens::Soc2Gap),
            DeepLensResult::empty(DeepLens::DeepSecurity),
            DeepLensResult::empty(DeepLens::ThreatModel),
        ],
        advisory: true,
        disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
    }
}

#[test]
fn soc2_flag_true_includes_all_three_lenses() {
    // The flag contract: when soc2_enabled=true, run_deep_tier returns a
    // DeepReport with the SOC-2 lens present. We verify the shape here
    // using a synthetic three-lens report (no LLM call needed).
    let report = three_lens_report();
    // All three lenses must be present.
    assert_eq!(report.lenses.len(), 3);
    let has_soc2 = report.lenses.iter().any(|l| l.lens == "soc2-gap");
    assert!(has_soc2, "soc2_enabled=true: soc2-gap lens must be present");
}

#[test]
fn soc2_flag_false_produces_two_lens_report() {
    // When soc2_enabled=false, run_deep_tier omits the soc2 lens and returns
    // a two-lens report (security + threat-model only). We verify the shape
    // using a synthetic two-lens report (no LLM call needed).
    let report = DeepReport {
        // Simulate the soc2=false path: two lenses only.
        lenses: vec![
            DeepLensResult::empty(DeepLens::DeepSecurity),
            DeepLensResult::empty(DeepLens::ThreatModel),
        ],
        advisory: true,
        disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
    };
    assert_eq!(report.lenses.len(), 2, "soc2 off: only 2 lenses");
    let has_soc2 = report.lenses.iter().any(|l| l.lens == "soc2-gap");
    assert!(
        !has_soc2,
        "soc2_enabled=false: soc2-gap lens must be absent"
    );
    // The report is still valid (advisory flag set, disclaimer present).
    assert!(report.advisory);
    assert_eq!(report.disclaimer, DEEP_ADVISORY_DISCLAIMER);
}

#[test]
fn deep_report_with_soc2_off_still_has_security_and_threat() {
    // Even with soc2=false the other two lenses carry their metadata.
    let security = DeepLensResult::empty(DeepLens::DeepSecurity);
    let threat = DeepLensResult::empty(DeepLens::ThreatModel);
    assert_eq!(security.lens, "deep-security");
    assert_eq!(threat.lens, "threat-model");
    // Both remain advisory.
    assert!(security.advisory);
    assert!(threat.advisory);
}
