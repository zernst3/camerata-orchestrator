//! END-TO-END: Pass 4a "Group A" architectural checkers (python test-file naming,
//! UI-UTC-DATES-1) plus the PRODUCTION `ARCH-HANDLER-NO-DB-1` AST checker (Pass 4c,
//! `camerata_checks::handler_no_db_checker::HandlerNoDbChecker`), driven through the REAL
//! deterministic scan entry point (`onboard::audit_repos`) exactly like
//! `architectural_executor_e2e.rs` proves the Pass-1 Supabase checkers. See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group A / Group D.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout — no model call anywhere in this file.
//!
//! FIXTURE: `tests/fixtures/group_a_arch_repo/` —
//!   - `tests/test_widget.py` (compliant name — must NOT fire)
//!   - `tests/testwidgethelper.py` (stray `test*.py` pytest would silently skip — the
//!     planted `PYTHON-TESTING-FILE-NAMING-1` hole)
//!   - `src/date_label.ts` (a direct `toLocaleString()` call — the planted `UI-UTC-DATES-1`
//!     hole; must land `needs-review`)
//!   - `src/handler.rs` (a handler function touching a `db` handle directly — the planted
//!     `ARCH-HANDLER-NO-DB-1` hole; this fixture carries NO `.camerata/architecture.toml`, so
//!     the PRODUCTION checker falls back to its name-heuristic tier — same `needs-review`
//!     grade the deleted interim lexical checker emitted, proving the supersession is
//!     behavior-preserving for an unconfigured repo)

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};

const RULE_PYTHON_NAMING: &str = "PYTHON-TESTING-FILE-NAMING-1";
const RULE_UTC_DATES: &str = "UI-UTC-DATES-1";
const RULE_HANDLER_NO_DB: &str = "ARCH-HANDLER-NO-DB-1";

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `architectural_executor_e2e.rs::stage_fixture_as_git_repo` (duplicated per this repo's
/// existing convention of each e2e test file owning its own fixture-staging helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/group_a_arch_repo");
    copy_dir_recursive(&fixture_root, dest);

    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "group-a-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Group A E2E Test"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "e2e fixture: one stray test file, one UTC-dates hole, one handler-no-db hole"]);
}

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("failed to copy fixture file {from:?} -> {to:?}: {e}"));
        }
    }
}

fn selected_group_a_rules() -> Vec<SelectedRule> {
    [RULE_PYTHON_NAMING, RULE_UTC_DATES, RULE_HANDLER_NO_DB]
        .into_iter()
        .map(|id| SelectedRule {
            id: id.to_string(),
            directive: format!("group-a-architectural e2e: {id}"),
            repos: Vec::new(), // project-level: applies to every scanned repo
        })
        .collect()
}

async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_group_a_rules();
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(), // extra_notes
        None,       // model — unused, run_ai_review is false
        None,       // calibration_model — unused
        ScanMode::Sequential,
        false, // thorough — unused (no AI review)
        None,  // feedback
        None,  // job
        None,  // incremental_prior — full scan
        false, // deep
        false, // soc2_enabled
        false, // run_ai_review — zero API spend
        true,  // run_deterministic — the floor AND the architectural engine both run
        None,  // ledger
        camerata_server::llm::BackendResolution::Api, // backend gate: not under test here
    )
    .await;
    report
}

#[tokio::test]
async fn deterministic_scan_fires_all_three_group_a_checkers_on_their_planted_holes() {
    let repo_spec = "e2e/group-a-arch-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ PYTHON-TESTING-FILE-NAMING-1: exactly one finding, on the stray file only ═══
    let naming_findings: Vec<&onboard::Finding> =
        report.findings.iter().filter(|f| f.rule_id == RULE_PYTHON_NAMING).collect();
    assert_eq!(
        naming_findings.len(),
        1,
        "expected exactly one finding (the stray testwidgethelper.py) and none for the compliant \
         test_widget.py: {:?}",
        report.findings
    );
    let naming = naming_findings[0];
    assert_eq!(naming.path, "tests/testwidgethelper.py");
    assert!(naming.detail.contains("test_"), "{:?}", naming.detail);
    assert!(naming.preview, "an architectural finding uses the existing preview mechanism");
    assert_eq!(naming.preview_tool.as_deref(), Some("camerata-arch"));

    // ═══ UI-UTC-DATES-1: exactly one finding, needs-review (no config exists yet) ═══
    let utc_findings: Vec<&onboard::Finding> = report.findings.iter().filter(|f| f.rule_id == RULE_UTC_DATES).collect();
    assert_eq!(utc_findings.len(), 1, "{:?}", report.findings);
    let utc = utc_findings[0];
    assert_eq!(utc.path, "src/date_label.ts");
    assert_eq!(utc.line, 2, "must point at the toLocaleString() call line");
    assert!(
        utc.detail.contains("[needs review"),
        "unconfigured UI-UTC-DATES-1 must carry the needs-review marker: {:?}",
        utc.detail
    );

    // ═══ ARCH-HANDLER-NO-DB-1: exactly one finding, needs-review (no config -> attrs/name ═══
    // ═══ fallback tier — the production checker's degradation path, per D3)            ═══
    let handler_findings: Vec<&onboard::Finding> =
        report.findings.iter().filter(|f| f.rule_id == RULE_HANDLER_NO_DB).collect();
    assert_eq!(handler_findings.len(), 1, "{:?}", report.findings);
    let handler = handler_findings[0];
    assert_eq!(handler.path, "src/handler.rs");
    assert!(handler.snippet.contains("list_orgs_handler"), "{:?}", handler.snippet);
    assert!(
        handler.detail.contains("[needs review"),
        "no .camerata/architecture.toml in this fixture -> the production checker's name-heuristic \
         fallback tier, same needs-review grade the deleted interim lexical checker emitted: {:?}",
        handler.detail
    );

    // ═══ Provenance stamp sanity (mirrors architectural_executor_e2e.rs) ═══
    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0].sha.as_deref().expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

#[tokio::test]
async fn arch_handler_no_db_rule_id_stays_eligible_for_the_llm_prompt_per_d3_when_unconfigured() {
    // D3, Pass 4c version: the PRODUCTION `HandlerNoDbChecker` is no longer unconditionally
    // advisory (`advisory_coexisting`) the way the deleted interim lexical checker was — it
    // follows the SAME per-repo `config_unsatisfied_for` pattern `ImportBoundaryChecker`
    // established in Pass 4b-2. For THIS fixture's repo (no `.camerata/architecture.toml` at
    // all), the rule id must still stay LLM-advisory-eligible — checked here via the per-repo
    // `checker_rule_ids_for_repo`, not the static `all_checker_rule_ids` (which now DOES
    // contain ARCH-HANDLER-NO-DB-1, since the production checker only opts out per-repo, not
    // unconditionally — see `handler_no_db_checker`'s own registry tests for that distinction).
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/group_a_arch_repo");
    let files = collect_fixture_files(&fixture_root);
    let repo = camerata_checks::arch_checker::RepoView { spec: "e2e/group-a-arch-fixture", files: &files };

    let per_repo = camerata_checks::arch_checker::checker_rule_ids_for_repo(&repo);
    assert!(
        !per_repo.contains(RULE_HANDLER_NO_DB),
        "ARCH-HANDLER-NO-DB-1 must stay LLM-advisory-eligible for this unconfigured repo: {per_repo:?}"
    );
    // The other two Group-A rules in this fixture ARE fully deterministic and correctly
    // subtracted from the LLM prompt, with or without config.
    assert!(per_repo.contains(RULE_PYTHON_NAMING), "{per_repo:?}");
    assert!(per_repo.contains(RULE_UTC_DATES), "{per_repo:?}");
}

/// Read every file under `root` into the `(repo-relative path, content)` shape `RepoView`
/// expects — a minimal, test-local stand-in for the scan's own file walk (this test only needs
/// the file CONTENTS, not a real git repo, unlike `run_fixture_scan` above).
fn collect_fixture_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    collect_fixture_files_into(root, root, &mut out);
    out
}

fn collect_fixture_files_into(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect_fixture_files_into(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let content = std::fs::read_to_string(&path).unwrap();
            out.push((rel, content));
        }
    }
}
