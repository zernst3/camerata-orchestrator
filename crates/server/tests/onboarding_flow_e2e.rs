//! Hermetic end-to-end regression net for the ONBOARDING flow.
//!
//! THE CLAIMS under test:
//!   1. ALREADY-ONBOARDED GUARD (#50): onboarding is one-time per repo. When a project has
//!      already onboarded a repo, the onboarding-start path must REFUSE to start a fresh
//!      onboarding and instead report the repo as found/already-onboarded (so the UI routes
//!      the user to the workspace, not a duplicate set of issues + branch).
//!   2. `effective_scan_modes`: both-false coerces to both-true (NEVER a no-op scan); each
//!      flag independently gates the AI review vs the deterministic floor.
//!   3. propose → select → apply → baseline: the corpus proposal returns rules; applying
//!      selected rules writes the governance arm-files; the currently-active findings are
//!      snapshotted into the per-repo baseline (so the gate enforces only on NEW code).
//!
//! HERMETIC: NO network, NO `claude`/process spawn, NO real scan. `propose_corpus_rules`
//! reads a TINY temp corpus we write (via `CAMERATA_CORPUS_PATH`) so the result is
//! deterministic and isolated from the real bundled corpus. The arm-files + baseline pieces
//! are pure functions driven with hand-built fixtures.

use std::sync::Mutex;

use camerata_server::arm::{arm_files_for_repo, existing_governance_files, ArmRule, GateRule};
use camerata_server::effective_scan_modes;
use camerata_server::project::{CustomRule, ProjectStore};
use camerata_server::suppression::{
    baseline_entry, classify_one, fingerprint, Baseline, FindingRef, Status,
};

// `propose_corpus_rules` reads a process-global env var (`CAMERATA_CORPUS_PATH`) and the
// corpus on disk. Serialize the env-mutating test so it never races a parallel test.
static CORPUS_ENV_LOCK: Mutex<()> = Mutex::new(());

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 1 — Already-onboarded guard (#50)
//
//   The real `detect_repo` handler scans `state.projects.list()` for a project whose
//   `onboarded` set contains the detected repo, and returns `onboarded: true` +
//   `onboarded_project` when found (blocking a re-onboard). We drive that exact guard
//   through the public ProjectStore + `Project::mark_onboarded`.
// ════════════════════════════════════════════════════════════════════════════════════

/// The pure guard the `detect_repo` handler runs: the name of the project that already
/// onboarded `repo`, or `None` when no project has (i.e. a fresh onboarding may start).
fn onboarded_in_project(store: &ProjectStore, repo: &str) -> Option<String> {
    store
        .list()
        .into_iter()
        .find(|p| p.onboarded.iter().any(|r| r == repo))
        .map(|p| p.name)
}

#[test]
fn scope1_fresh_repo_is_not_blocked_onboarded_repo_is() {
    let store = ProjectStore::new();
    let proj = store
        .create("Onboarder", vec!["me/api".to_string()])
        .unwrap();

    // A repo that no project has onboarded: a fresh onboarding may start.
    assert_eq!(
        onboarded_in_project(&store, "me/api"),
        None,
        "an un-onboarded repo must NOT be blocked"
    );

    // Onboard the repo (the apply step marks it onboarded on the project).
    store
        .update(&proj.id, |p| p.mark_onboarded(&["me/api".to_string()]))
        .unwrap();

    // Now the guard REFUSES a fresh onboarding and names the owning project.
    assert_eq!(
        onboarded_in_project(&store, "me/api"),
        Some("Onboarder".to_string()),
        "an already-onboarded repo must be reported as onboarded (block the re-onboard)"
    );

    // A DIFFERENT repo is still free to onboard (the guard is per-repo, not per-project).
    assert_eq!(onboarded_in_project(&store, "me/other"), None);
}

#[test]
fn scope1_guard_sees_onboarded_repo_in_any_project() {
    // The guard scans ALL projects, not just the active one — a repo onboarded under
    // project A blocks a re-onboard even while project B is active.
    let store = ProjectStore::new();
    let a = store.create("A", vec!["shared/repo".to_string()]).unwrap();
    let _b = store.create("B", vec!["b/only".to_string()]).unwrap(); // B is now active
    store
        .update(&a.id, |p| p.mark_onboarded(&["shared/repo".to_string()]))
        .unwrap();

    assert_eq!(
        onboarded_in_project(&store, "shared/repo"),
        Some("A".to_string()),
        "a repo onboarded in any project blocks a re-onboard"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 2 — effective_scan_modes (the REAL pure resolver)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope2_both_false_coerces_to_both_true_never_a_noop_scan() {
    // (false, false) is a no-op scan; the resolver forces both ON and flags the coercion.
    assert_eq!(
        effective_scan_modes(false, false),
        (true, true, true),
        "both-false must coerce to both-true (coercion flag set), never a no-op scan"
    );
}

#[test]
fn scope2_each_flag_independently_gates_its_lane() {
    // AI review ON, deterministic floor OFF (token-spending AI only).
    assert_eq!(
        effective_scan_modes(true, false),
        (true, false, false),
        "ai-only: AI review on, deterministic off, no coercion"
    );
    // AI review OFF, deterministic floor ON (fast, token-free).
    assert_eq!(
        effective_scan_modes(false, true),
        (false, true, false),
        "deterministic-only: AI off, floor on, no coercion"
    );
    // Both ON: pass-through, no coercion.
    assert_eq!(
        effective_scan_modes(true, true),
        (true, true, false),
        "both-on passes through unchanged with no coercion"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3a — propose: a tiny deterministic corpus -> the proposal returns those rules
// ════════════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn scope3_propose_corpus_rules_returns_the_corpus_library() {
    let _guard = CORPUS_ENV_LOCK.lock().unwrap();

    // Write a tiny, self-contained corpus so the proposal is deterministic + isolated from
    // the real bundled corpus. (Hermetic: local temp dir, no network.)
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rust-domain-1.toml"),
        r#"
id = "RUST-DOMAIN-1"
title = "Newtype IDs"
domain = "rust"
enforcement = "prose"
directive = "Use newtype wrappers for entity ids."
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("universal-1.toml"),
        r#"
id = "UNIVERSAL-1"
title = "Explicit over terse"
domain = "universal"
enforcement = "prose"
directive = "Prefer explicit, robust code over terse code."
"#,
    )
    .unwrap();

    std::env::set_var("CAMERATA_CORPUS_PATH", dir.path());

    // Repos with a `rust` stack: the rust rule is SUGGESTED (recommended + bound to the repo),
    // the universal rule is always present. With repos provided, the full library is returned
    // (suggested + available) so the architect sees the whole corpus in one place.
    let proposed = camerata_server::onboard::propose_corpus_rules(&[(
        "me/api".to_string(),
        vec!["rust".to_string()],
    )])
    .await;

    let ids: Vec<&str> = proposed.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.contains(&"RUST-DOMAIN-1"),
        "the rust-domain rule must be proposed for a rust repo: got {ids:?}"
    );
    assert!(
        ids.contains(&"UNIVERSAL-1"),
        "the universal rule must always be present: got {ids:?}"
    );

    // The rust rule is bound to the matching repo (suggested -> pre-bound to its repo);
    // the proposal carries the full directive so the apply step can emit it.
    let rust = proposed.iter().find(|r| r.id == "RUST-DOMAIN-1").unwrap();
    assert!(
        rust.repos.contains(&"me/api".to_string()),
        "suggested rule is bound to its matching repo"
    );

    // No repos at all -> the WHOLE corpus library is still returned (un-suggested), which is
    // the handler's `corpus_rules` all-rules reference path. Same tiny corpus -> our 2 rules.
    let whole = camerata_server::onboard::propose_corpus_rules(&[]).await;
    std::env::remove_var("CAMERATA_CORPUS_PATH");
    let whole_ids: Vec<&str> = whole.iter().map(|r| r.id.as_str()).collect();
    assert!(
        whole_ids.contains(&"RUST-DOMAIN-1") && whole_ids.contains(&"UNIVERSAL-1"),
        "propose with no repos returns the whole corpus library (un-suggested): got {whole_ids:?}"
    );
    // With no scanned stack, the rust rule is NOT recommended (nothing matched the repo).
    let rust_whole = whole.iter().find(|r| r.id == "RUST-DOMAIN-1").unwrap();
    assert!(
        !rust_whole.recommended,
        "with no repos/stack, a domain rule is available but NOT recommended"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3b — apply: selected rules -> the governance ARM-FILES set (the real emit)
// ════════════════════════════════════════════════════════════════════════════════════

fn rule(id: &str, enforcement: &str, repos: &[&str]) -> ArmRule {
    ArmRule {
        id: id.to_string(),
        title: format!("{id} title"),
        directive: format!("Do {id}."),
        option: None,
        enforcement: enforcement.to_string(),
        scope: "repo-local".to_string(),
        conformance: if enforcement == "mechanical" {
            Some("cargo clippy -- -D warnings".to_string())
        } else {
            None
        },
        repos: repos.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn scope3_apply_writes_the_expected_arm_files_set() {
    // A prose rule, a structured rule, a mechanical (CI-tier) rule, and a custom rule.
    let prose = rule("PROSE-1", "prose", &["me/api"]);
    let structured = rule("STRUCT-1", "structured", &["me/api"]);
    let mechanical = rule("MECH-1", "mechanical", &["me/api"]);
    let rules: Vec<&ArmRule> = vec![&prose, &structured, &mechanical];
    let custom = CustomRule {
        name: "house-style".to_string(),
        body: "Prefer explicit.".to_string(),
        domain: "*".to_string(),
        repos: Vec::new(),
    };
    let customs: Vec<&CustomRule> = vec![&custom];

    let files = arm_files_for_repo(&rules, &customs);
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

    // The full expected arm-files set for this mix:
    for expected in [
        "AGENTS.md",                          // prose + custom rules
        "CONVENTIONS.md",                     // structured + mechanical rules
        ".camerata/rules.json",               // the gate config (always, when rules exist)
        ".camerata/checks.toml",              // CI-tier (mechanical) -> checks manifest
        ".github/workflows/camerata-gates.yml", // CI-tier -> generated workflow
    ] {
        assert!(
            names.contains(&expected),
            "the arm-files set must include `{expected}`: got {names:?}"
        );
    }

    // AGENTS.md carries the prose rule AND the custom rule.
    let agents = &files.iter().find(|(p, _)| p == "AGENTS.md").unwrap().1;
    assert!(agents.contains("### PROSE-1"), "prose rule emitted into AGENTS.md");
    assert!(
        agents.contains("CUSTOM-house-style"),
        "custom rule emitted into AGENTS.md"
    );

    // CONVENTIONS.md carries the structured + mechanical rules (NOT the prose one).
    let conv = &files.iter().find(|(p, _)| p == "CONVENTIONS.md").unwrap().1;
    assert!(conv.contains("### STRUCT-1"));
    assert!(conv.contains("### MECH-1"));
    assert!(!conv.contains("### PROSE-1"), "prose rules do NOT go in CONVENTIONS.md");

    // .camerata/rules.json (the SELECTION PERSISTENCE: every applied rule id incl. custom).
    let gate_json = &files.iter().find(|(p, _)| p == ".camerata/rules.json").unwrap().1;
    let gate: Vec<GateRule> = serde_json::from_str(gate_json).unwrap();
    let gate_ids: Vec<&str> = gate.iter().map(|g| g.id.as_str()).collect();
    assert!(gate_ids.contains(&"PROSE-1"));
    assert!(gate_ids.contains(&"STRUCT-1"));
    assert!(gate_ids.contains(&"MECH-1"));
    assert!(
        gate_ids.contains(&"CUSTOM-house-style"),
        "the custom rule is recorded in the gate config so a re-emit never drops it"
    );
}

#[test]
fn scope3_prose_only_apply_omits_the_ci_tier_files() {
    // A prose-only selection emits AGENTS.md + rules.json, but NO checks.toml / workflow
    // (those are only for CI-tier mechanical/architectural rules).
    let prose = rule("PROSE-ONLY", "prose", &["me/api"]);
    let rules: Vec<&ArmRule> = vec![&prose];
    let files = arm_files_for_repo(&rules, &[]);
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

    assert!(names.contains(&"AGENTS.md"));
    assert!(names.contains(&".camerata/rules.json"));
    assert!(
        !names.contains(&".camerata/checks.toml"),
        "no CI-tier rule => no checks.toml"
    );
    assert!(
        !names.contains(&".github/workflows/camerata-gates.yml"),
        "no CI-tier rule => no workflow"
    );
    // No structured rule => no CONVENTIONS.md.
    assert!(!names.contains(&"CONVENTIONS.md"));
}

#[test]
fn scope3_existing_governance_files_warns_before_clobbering() {
    // The apply pre-flight: report which arm-files already exist on disk (so the UI can warn).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "pre-existing").unwrap();
    std::fs::create_dir_all(dir.path().join(".camerata")).unwrap();
    std::fs::write(dir.path().join(".camerata/rules.json"), "[]").unwrap();

    let will_write = vec![
        "AGENTS.md".to_string(),
        "CONVENTIONS.md".to_string(),
        ".camerata/rules.json".to_string(),
    ];
    let existing = existing_governance_files(dir.path(), &will_write);

    assert!(existing.contains(&"AGENTS.md".to_string()), "AGENTS.md already exists");
    assert!(
        existing.contains(&".camerata/rules.json".to_string()),
        ".camerata/rules.json already exists"
    );
    assert!(
        !existing.contains(&"CONVENTIONS.md".to_string()),
        "CONVENTIONS.md does NOT exist yet, so it is not flagged"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3c — baseline: active findings are SNAPSHOTTED into the per-repo baseline, so the
//   ratchet enforces only on NEW code. (The real `baselines_from_findings` builds this via
//   `suppression::baseline_entry`; we exercise that exact mechanism + classification.)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope3_active_findings_snapshot_into_baseline_and_suppress_on_reclassify() {
    let now = "2026-06-25T00:00:00Z";

    // Three findings discovered at onboarding (the "active" set the apply step snapshots).
    let f1 = FindingRef {
        rule_id: "SEC-1".to_string(),
        path: "src/a.rs".to_string(),
        line: 10,
        snippet: "let x = unsafe_thing();".to_string(),
    };
    let f2 = FindingRef {
        rule_id: "SEC-1".to_string(),
        path: "src/b.rs".to_string(),
        line: 20,
        snippet: "another_violation()".to_string(),
    };

    // Snapshot them into the baseline (accepted pre-existing debt).
    let baseline = Baseline {
        entries: vec![
            baseline_entry(&f1, "zach", now, "pre-existing at onboarding"),
            baseline_entry(&f2, "zach", now, "pre-existing at onboarding"),
        ],
    };
    assert_eq!(baseline.entries.len(), 2);
    // Each entry carries a content fingerprint so the ratchet survives line drift.
    assert_eq!(
        baseline.entries[0].fingerprint,
        fingerprint("SEC-1", "let x = unsafe_thing();"),
        "the snapshot fingerprints the offending snippet"
    );

    // The SAME findings re-classify as SuppressedBaseline (they are accepted debt -> the gate
    // does NOT enforce on them). No inline waivers.
    assert_eq!(
        classify_one(&f1, &[], &baseline),
        Status::SuppressedBaseline,
        "a baselined finding is suppressed"
    );
    assert_eq!(classify_one(&f2, &[], &baseline), Status::SuppressedBaseline);

    // A NEW finding (not in the baseline) is ACTIVE -> the gate enforces on new code.
    let new_finding = FindingRef {
        rule_id: "SEC-1".to_string(),
        path: "src/c.rs".to_string(),
        line: 5,
        snippet: "freshly_added_violation()".to_string(),
    };
    assert_eq!(
        classify_one(&new_finding, &[], &baseline),
        Status::Active,
        "a new violation is NOT suppressed by the onboarding baseline (the ratchet works)"
    );

    // TOUCHING baselined code (changing the snippet) un-baselines it -> it becomes Active.
    let edited = FindingRef {
        rule_id: "SEC-1".to_string(),
        path: "src/a.rs".to_string(),
        line: 10,
        snippet: "let x = a_DIFFERENT_thing();".to_string(),
    };
    assert_eq!(
        classify_one(&edited, &[], &baseline),
        Status::Active,
        "editing the offending code changes its fingerprint -> un-baselined (ratchet)"
    );
}
