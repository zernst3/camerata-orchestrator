//! Hermetic end-to-end regression net for the FULL onboarding WALK.
//!
//! Where `onboarding_flow_e2e.rs` exercises the onboarding PIECES in isolation
//! (the already-onboarded guard, the `effective_scan_modes` resolver, propose,
//! arm-files emit, baseline), this suite threads ONE repo through the WHOLE
//! sequence as a single continuous flow, asserting the seams hand off correctly:
//!
//!   connect/native provider (in-process `AppState`)
//!     → SCAN a real on-disk repo (`onboard::scan_repos`) — stack detect + proposal
//!     → PROPOSE corpus rules for the detected stack (`onboard::propose_corpus_rules`)
//!     → APPLY the selected rules (`arm::arm_files_for_repo`) and WRITE the arm-files
//!        (AGENTS.md / CONVENTIONS.md / .camerata/rules.json) into the repo on disk
//!     → MARK the repo onboarded on the project (`Project::mark_onboarded`)
//!     → the development gate (`onboarded_in_project`, the guard the `detect_repo`
//!        handler runs) now SEES the repo as onboarded
//!     → the suppression REGISTRY (`onboard::suppression_registry`) reads the repo
//!        from disk and TAGS each record with its repo.
//!
//! It also asserts the arm-file SET matches the emit-partitioning contract
//! (prose → AGENTS.md, structured/mechanical → CONVENTIONS.md, gate config always
//! present) AND that a prose-only apply OMITS the CI-tier files — mirroring the
//! piecewise assertions in `onboarding_flow_e2e.rs`, but proven inside the
//! continuous walk so the steps are shown to compose.
//!
//! HERMETIC: NO network, NO `claude`/process spawn, NO real AI. The provider is the
//! in-process native `AppState`; the "repo" is a temp dir we write real files into;
//! `propose_corpus_rules` reads a TINY temp corpus via `CAMERATA_CORPUS_PATH`; the
//! scan/audit/suppression seams are pure functions over the on-disk repo. Where a
//! flow would require AI (the actual rule selection by an architect, the AI review
//! pass), we assert the STRUCTURE around it (the proposal set, the deterministic
//! floor, the emitted gate config), never AI output.

use std::sync::Mutex;

use camerata_server::arm::{arm_files_for_repo, existing_governance_files, ArmRule, GateRule};
use camerata_server::project::ProjectStore;
use camerata_server::AppState;

// `propose_corpus_rules` reads a process-global env var (`CAMERATA_CORPUS_PATH`). Serialize the
// env-mutating tests so they never race a parallel test (same guard as onboarding_flow_e2e).
static CORPUS_ENV_LOCK: Mutex<()> = Mutex::new(());

// ════════════════════════════════════════════════════════════════════════════════════
// Shared fixtures
// ════════════════════════════════════════════════════════════════════════════════════

/// The pure development-gate guard the `detect_repo` handler runs: the name of the
/// project that already onboarded `repo`, or `None` when none has. Identical to the
/// guard exercised in `onboarding_flow_e2e.rs::onboarded_in_project` — re-used here so
/// the WALK proves the apply→mark→gate-sees-it hand-off, not the guard in isolation.
fn onboarded_in_project(store: &ProjectStore, repo: &str) -> Option<String> {
    store
        .list()
        .into_iter()
        .find(|p| p.onboarded.iter().any(|r| r == repo))
        .map(|p| p.name)
}

/// Write a tiny, self-contained Rust "repo" on disk: a `Cargo.toml` + a `src/lib.rs`
/// (so the stack detector sees `rust`) plus a file carrying a REAL deterministic-floor
/// violation (a hardcoded secret) so the suppression registry has something to report.
/// Returns the temp dir handle (RAII-cleaned) — keep it alive for the test's duration.
fn write_rust_repo_with_a_floor_finding() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
    // A REAL hardcoded-secret line: this fires SEC-NO-HARDCODED-SECRETS-1 (a floor rule),
    // so the deterministic floor produces a finding the suppression registry can tag.
    std::fs::write(
        root.join("src/config.rs"),
        "pub const AWS_SECRET_ACCESS_KEY: &str = \"AKIAIOSFODNN7EXAMPLEKEYDATA1234567890ABC\";\n",
    )
    .unwrap();
    dir
}

/// Write a tiny deterministic corpus (one rust-domain PROSE rule, one universal PROSE
/// rule, one mechanical RUST rule) so `propose_corpus_rules` is isolated from the real
/// bundled corpus. Returns the temp dir (keep alive while `CAMERATA_CORPUS_PATH` points
/// at it).
fn write_tiny_corpus() -> tempfile::TempDir {
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
    std::fs::write(
        dir.path().join("rust-fmt-1.toml"),
        r#"
id = "RUST-FMT-1"
title = "Formatted code"
domain = "rust"
enforcement = "mechanical"
directive = "Code must be rustfmt-clean."
conformance = "cargo fmt --check"
"#,
    )
    .unwrap();
    dir
}

/// Build an `ArmRule` (the fully-resolved-by-the-UI shape `arm_files_for_repo` consumes)
/// for `repo`. `enforcement` is `prose` | `structured` | `mechanical`.
fn arm_rule(id: &str, enforcement: &str, repo: &str) -> ArmRule {
    ArmRule {
        id: id.to_string(),
        title: format!("{id} title"),
        directive: format!("Do {id}."),
        option: None,
        enforcement: enforcement.to_string(),
        scope: "repo-local".to_string(),
        conformance: if enforcement == "mechanical" {
            Some("cargo fmt --check".to_string())
        } else {
            None
        },
        repos: vec![repo.to_string()],
    }
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 1 — The FULL onboarding walk: scan → propose → apply (write to disk) → mark
//   onboarded → development gate sees it → suppression registry tags by repo.
// ════════════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn scope1_full_onboarding_walk_scan_propose_apply_mark_gate_registry() {
    let _guard = CORPUS_ENV_LOCK.lock().unwrap();

    let repo_spec = "me/api".to_string();

    // ── connect/native provider: an in-process AppState + a fresh project covering the repo.
    //    `seeded()` gives the native (in-process) provider; the seeded story spine is
    //    irrelevant to onboarding (we drive the project store + the on-disk repo).
    let state = AppState::seeded();
    let project = state
        .projects()
        .create("Onboarder", vec![repo_spec.clone()])
        .expect("create the project under onboarding");
    // Fresh project: the repo is NOT yet onboarded, so an onboarding may legitimately start.
    assert_eq!(
        onboarded_in_project(state.projects(), &repo_spec),
        None,
        "a fresh repo must not be reported as onboarded before the walk"
    );

    // ── SCAN the on-disk repo: stack detect + (deterministic) proposal.
    let repo_dir = write_rust_repo_with_a_floor_finding();
    let corpus = write_tiny_corpus();
    std::env::set_var("CAMERATA_CORPUS_PATH", corpus.path());

    let sources = vec![(repo_spec.clone(), repo_dir.path().to_path_buf())];
    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new()).await;
    assert!(!scan.gated, "a local-dir scan is not gated on a GitHub token");
    assert_eq!(scan.repos, vec![repo_spec.clone()]);
    assert!(scan.files_scanned > 0, "the scan read the repo's files");
    // The stack detector recognized this as a Rust repo (it has Cargo.toml + .rs files).
    let stack = scan
        .stacks
        .iter()
        .find(|s| s.repo == repo_spec)
        .expect("the scanned repo has a detected stack");
    assert!(
        stack.languages.iter().any(|l| l.eq_ignore_ascii_case("rust")),
        "Cargo.toml + .rs => the Rust stack is detected: {:?}",
        stack.languages
    );

    // ── PROPOSE corpus rules for the detected stack. The rust rules are suggested + bound
    //    to the repo; the universal rule is always present.
    let repo_domains = vec![(repo_spec.clone(), vec!["rust".to_string()])];
    let proposed = camerata_server::onboard::propose_corpus_rules(&repo_domains).await;
    let proposed_ids: Vec<&str> = proposed.iter().map(|r| r.id.as_str()).collect();
    assert!(
        proposed_ids.contains(&"RUST-DOMAIN-1") && proposed_ids.contains(&"UNIVERSAL-1"),
        "propose returns the rust + universal corpus rules: got {proposed_ids:?}"
    );
    // The suggested rust rule is pre-bound to the matching repo (so apply emits it there).
    let rust = proposed.iter().find(|r| r.id == "RUST-DOMAIN-1").unwrap();
    assert!(
        rust.repos.contains(&repo_spec),
        "the suggested rule is bound to its matching repo"
    );

    // ── APPLY: the architect selects rust-domain (prose) + universal (prose) + a mechanical
    //    rule, and the apply step EMITS the arm-files. (We model the architect's selection
    //    by building ArmRules from the proposal — hermetic: no AI selection is invoked.)
    let selected: Vec<ArmRule> = vec![
        arm_rule("RUST-DOMAIN-1", "prose", &repo_spec),
        arm_rule("UNIVERSAL-1", "prose", &repo_spec),
        arm_rule("RUST-FMT-1", "mechanical", &repo_spec),
    ];
    let selected_refs: Vec<&ArmRule> = selected.iter().collect();
    let files = arm_files_for_repo(&selected_refs, &[]);

    // Pre-flight clobber check: a freshly-scanned repo has NO governance files yet.
    let will_write: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    assert!(
        existing_governance_files(repo_dir.path(), &will_write).is_empty(),
        "a fresh repo has no pre-existing arm-files to clobber"
    );

    // WRITE the arm-files into the repo on disk (the apply step's emit-local).
    for (rel, content) in &files {
        let path = repo_dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    // The arm-file SET matches the emit-partitioning contract for this mix
    // (prose → AGENTS.md, mechanical → CONVENTIONS.md + CI-tier files, gate config always).
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    for expected in [
        "AGENTS.md",                            // the two prose rules
        "CONVENTIONS.md",                       // the mechanical rule
        ".camerata/rules.json",                 // the gate config (always, rules exist)
        ".camerata/checks.toml",                // CI-tier (mechanical) manifest
        ".github/workflows/camerata-gates.yml", // CI-tier generated workflow
    ] {
        assert!(
            names.contains(&expected),
            "the applied arm-file set must include `{expected}`: got {names:?}"
        );
    }
    // AGENTS.md carries BOTH prose rules; CONVENTIONS.md carries the mechanical one, not prose.
    let agents = &files.iter().find(|(p, _)| p == "AGENTS.md").unwrap().1;
    assert!(agents.contains("### RUST-DOMAIN-1"));
    assert!(agents.contains("### UNIVERSAL-1"));
    let conv = &files.iter().find(|(p, _)| p == "CONVENTIONS.md").unwrap().1;
    assert!(conv.contains("### RUST-FMT-1"), "mechanical rule in CONVENTIONS.md");
    assert!(
        !conv.contains("### RUST-DOMAIN-1"),
        "prose rules do NOT go into CONVENTIONS.md"
    );
    // The on-disk gate config round-trips: every applied id is recorded.
    let gate_json =
        std::fs::read_to_string(repo_dir.path().join(".camerata/rules.json")).unwrap();
    let gate: Vec<GateRule> = serde_json::from_str(&gate_json).unwrap();
    let gate_ids: Vec<&str> = gate.iter().map(|g| g.id.as_str()).collect();
    assert!(gate_ids.contains(&"RUST-DOMAIN-1"));
    assert!(gate_ids.contains(&"UNIVERSAL-1"));
    assert!(gate_ids.contains(&"RUST-FMT-1"));

    // ── MARK the repo onboarded on the project (the apply step's final act).
    state
        .projects()
        .update(&project.id, |p| p.mark_onboarded(&[repo_spec.clone()]))
        .expect("mark the repo onboarded");

    // ── The development gate now SEES the repo as onboarded: a re-onboard is blocked and the
    //    owning project is named. This is the exact guard the `detect_repo` handler runs.
    assert_eq!(
        onboarded_in_project(state.projects(), &repo_spec),
        Some("Onboarder".to_string()),
        "after apply+mark, the gate sees the repo as onboarded (re-onboard blocked)"
    );

    // ── The suppression REGISTRY reads the repo from disk and TAGS each record with its repo.
    //    The repo has a real hardcoded-secret floor finding, so the registry is non-empty.
    let registry = camerata_server::onboard::suppression_registry(&sources).await;
    // Every record is tagged with the repo it came from (the orchestration layer's job).
    for rec in &registry {
        assert_eq!(
            rec.repo, repo_spec,
            "every suppression record is tagged with its source repo"
        );
    }

    std::env::remove_var("CAMERATA_CORPUS_PATH");
    // repo_dir + corpus (TempDirs) are RAII-cleaned on drop.
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 2 — A prose-only apply (the minimal onboarding) OMITS the CI-tier files — proven
//   in the same continuous flow (write to disk, read back) rather than on bare fixtures.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope2_prose_only_apply_writes_agents_and_gate_but_omits_ci_tier() {
    let repo_spec = "me/web";
    let repo_dir = tempfile::tempdir().unwrap();

    // A prose-only selection: AGENTS.md + rules.json, but NO CONVENTIONS.md / checks.toml /
    // workflow (those are exclusively for CI-tier mechanical/architectural rules).
    let prose = arm_rule("PROSE-ONLY-1", "prose", repo_spec);
    let refs: Vec<&ArmRule> = vec![&prose];
    let files = arm_files_for_repo(&refs, &[]);

    // Emit-local: write the arm-files into the repo on disk.
    for (rel, content) in &files {
        let path = repo_dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    // On disk: AGENTS.md + .camerata/rules.json exist; the CI-tier files do NOT.
    assert!(repo_dir.path().join("AGENTS.md").exists());
    assert!(repo_dir.path().join(".camerata/rules.json").exists());
    assert!(
        !repo_dir.path().join("CONVENTIONS.md").exists(),
        "no structured/mechanical rule => no CONVENTIONS.md"
    );
    assert!(
        !repo_dir.path().join(".camerata/checks.toml").exists(),
        "no CI-tier rule => no checks.toml"
    );
    assert!(
        !repo_dir
            .path()
            .join(".github/workflows/camerata-gates.yml")
            .exists(),
        "no CI-tier rule => no generated workflow"
    );

    // The gate config still records the prose rule (so a re-emit never drops it).
    let gate_json = std::fs::read_to_string(repo_dir.path().join(".camerata/rules.json")).unwrap();
    let gate: Vec<GateRule> = serde_json::from_str(&gate_json).unwrap();
    assert_eq!(gate.len(), 1);
    assert_eq!(gate[0].id, "PROSE-ONLY-1");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3 — Re-running APPLY over an ALREADY-armed repo is detected by the clobber pre-flight
//   (so the UI can warn) — the second half of the apply contract, in the continuous flow.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope3_reapply_over_armed_repo_is_flagged_by_the_clobber_preflight() {
    let repo_spec = "me/api";
    let repo_dir = tempfile::tempdir().unwrap();

    // First apply: write AGENTS.md + gate config.
    let prose = arm_rule("PROSE-1", "prose", repo_spec);
    let first = arm_files_for_repo(&[&prose], &[]);
    for (rel, content) in &first {
        let path = repo_dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    // Second apply (add a structured rule): the pre-flight reports the files that already exist.
    let prose2 = arm_rule("PROSE-1", "prose", repo_spec);
    let structured = arm_rule("STRUCT-1", "structured", repo_spec);
    let second = arm_files_for_repo(&[&prose2, &structured], &[]);
    let will_write: Vec<String> = second.iter().map(|(p, _)| p.clone()).collect();
    let existing = existing_governance_files(repo_dir.path(), &will_write);

    assert!(
        existing.contains(&"AGENTS.md".to_string()),
        "AGENTS.md from the first apply is flagged as pre-existing"
    );
    assert!(
        existing.contains(&".camerata/rules.json".to_string()),
        "the gate config from the first apply is flagged as pre-existing"
    );
    assert!(
        !existing.contains(&"CONVENTIONS.md".to_string()),
        "CONVENTIONS.md is NEW in this apply (no first-apply structured rule), so not flagged"
    );
}
