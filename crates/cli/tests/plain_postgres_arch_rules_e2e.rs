//! C3-3: proves `SUPABASE-FUNC-SEARCH-PATH-1` and `SUPABASE-RLS-DISABLED-NOT-RESTORED-1` are
//! actually ARMED and actually FIRE for a plain-Postgres repo (no `supabase/` layout at all)
//! through the REAL selection path — not a unit test that hand-constructs a `SelectedRule` or
//! calls `propose_corpus_rules` in isolation.
//!
//! Both rules regressed the same way, for the same reason, previously (see the doc comment on
//! `camerata_server::onboard::audit::is_ci_tier_rule`): every real caller
//! (`onboard_audit`/`onboard_audit_start`/`camerata inspect`) fed `audit_repos` the OUTPUT of
//! `split_scannable_rules` — a rule list with every CI-tier (mechanical/architectural) id
//! already stripped out, because that's what the AI code-audit prompt needs. But
//! `audit_repos` ALSO derives `repo_selected_ids` (the deterministic architectural engine's
//! arming gate) from that SAME parameter, so every corpus-sourced architectural rule —
//! `SUPABASE-RLS-ENABLED-1` included, not just the two rules this file names — was silently
//! never armed in a real scan, regardless of how correctly `domains_for_stack` /
//! `propose_corpus_rules` / `extra_domains` computed its selection upstream. This file drives
//! the EXACT real entry point (`camerata inspect`'s `run_inspect_with_key_presence`:
//! `scan_repos` -> `curated_rule_selection` -> `split_scannable_rules` -> `audit_repos` ->
//! export) end to end, deterministic-only (zero API spend), over an in-line synthetic repo —
//! never the external benchmark fixture.

use std::io::Read;
use std::path::Path;

use camerata::inspect_cmd::{run_inspect_with_key_presence, InspectArgs};
use camerata_server::llm::ProjectBackend;

const RULE_SEARCH_PATH: &str = "SUPABASE-FUNC-SEARCH-PATH-1";
const RULE_RLS_DISABLED_NOT_RESTORED: &str = "SUPABASE-RLS-DISABLED-NOT-RESTORED-1";

fn stage_git_repo(dest: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let full = dest.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "plain-pg-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Plain-Postgres E2E Test"]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: definer-without-search_path + RLS disabled-not-restored, no supabase/ layout",
    ]);
}

/// The synthetic repo this whole file exercises: a `db/migrations/` layout (NOT
/// `supabase/migrations/`, no `supabase/config.toml`, no `@supabase/*` dependency anywhere) —
/// unambiguously "plain Postgres" by `detect_frameworks`'s own Supabase-detection rules. Two
/// planted defects, matching the GENERAL class each rule answers (not a benchmark's specific
/// shape):
///   - `grant_temporary_access`: `SECURITY DEFINER` with no `SET search_path` clause.
///   - `orders`: RLS explicitly enabled, then explicitly disabled, and never restored.
fn plain_postgres_files() -> Vec<(&'static str, &'static str)> {
    vec![(
        "db/migrations/0001_init.sql",
        "create table public.orders (id uuid primary key);\n\
         alter table public.orders enable row level security;\n\
         alter table public.orders disable row level security;\n\
         create function public.grant_temporary_access() returns void security definer as $$ begin end; $$ language plpgsql;\n",
    )]
}

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

/// STEP 1 — arming, at the real selection seam: `scan_repos` (stack detection +
/// `propose_corpus_rules`) followed by `curated_rule_selection` (the same "nothing manually
/// overridden" default a fresh onboarding scan pre-checks) must select BOTH rule ids for a
/// repo with no Supabase layout at all, purely on the strength of each rule's `extra_domains =
/// ["sql"]` arming against the generic `sql` domain `domains_for_stack` emits for any `.sql`
/// file. This is the propose-level half of the regression: if this fails, the rule was never
/// even offered to the real pipeline in the first place, independent of the split/audit_repos
/// bug the rest of this file guards.
#[tokio::test]
async fn plain_postgres_repo_selects_both_rules_via_the_real_curated_selection() {
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture repo");
    stage_git_repo(repo_dir.path(), &plain_postgres_files());
    let repo_spec = "e2e/plain-postgres-fixture".to_string();
    let sources = vec![(repo_spec.clone(), repo_dir.path().to_path_buf())];

    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new()).await;
    let selected = camerata::inspect_cmd::curated_rule_selection(&scan.proposed_rules);
    let selected_ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();

    assert!(
        selected_ids.contains(&RULE_SEARCH_PATH),
        "SUPABASE-FUNC-SEARCH-PATH-1 must be auto-selected for a plain-Postgres repo (via \
         extra_domains=[\"sql\"]): selected ids were {selected_ids:?}"
    );
    assert!(
        selected_ids.contains(&RULE_RLS_DISABLED_NOT_RESTORED),
        "SUPABASE-RLS-DISABLED-NOT-RESTORED-1 must be auto-selected for a plain-Postgres repo \
         (via extra_domains=[\"sql\"]): selected ids were {selected_ids:?}"
    );
}

/// STEP 2 — the full real pipeline, end to end: `camerata inspect`'s own production entry
/// point (`run_inspect_with_key_presence`), deterministic-only (zero API spend via the real
/// compliance backend gate — `--backend api` with no key resolves to `Blocked`, exactly like
/// `inspect_e2e.rs`'s own deterministic-only test), proving BOTH rules are armed all the way
/// through `split_scannable_rules` + `audit_repos`'s `repo_selected_ids` gate, that their
/// checkers (`SupabaseFnSearchPathChecker` / `SupabaseRlsChecker`) actually FIRE against the
/// synthetic repo's real content, and that both findings reach the written product-export
/// ZIP's `findings.json` with a grounded citation (never the bare "preview, not
/// corpus-documented" label an armed-but-uncited deterministic finding would carry) — this is
/// exactly what the C3-3 real-path arming bug silently broke.
#[tokio::test]
async fn plain_postgres_repo_fires_both_rules_through_camerata_inspect_end_to_end() {
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture repo");
    stage_git_repo(repo_dir.path(), &plain_postgres_files());
    let export_dir = tempfile::tempdir().expect("create temp dir for the export zip");
    let export_path = export_dir.path().join("inspect.zip");

    let args = InspectArgs {
        repo: repo_dir.path().to_path_buf(),
        export: export_path,
        // `Api` + `has_api_key: false` resolves to `Blocked` — the real compliance gate, not a
        // test-only bypass (mirrors `inspect_e2e.rs`). The deterministic floor AND the
        // architectural engine both still run; only the AI review is skipped.
        backend: ProjectBackend::Api,
        batch: false,
        model: None,
        calibration_model: None,
        full: true,
    };

    let outcome = run_inspect_with_key_presence(args, false)
        .await
        .expect("a floor-only headless inspection must succeed with zero model calls");

    let calls = outcome.actual_usage.as_ref().map(|u| u.calls).unwrap_or(0);
    assert_eq!(
        calls, 0,
        "no AI review ran (compliance-blocked), so zero model calls must be recorded: {:?}",
        outcome.actual_usage
    );

    assert!(
        export_path_is_zip(&outcome.export_path),
        "the export must be a real zip"
    );
    let findings_json = read_zip_entry(&outcome.export_path, "findings.json");

    for (rule_id, label) in [
        (RULE_SEARCH_PATH, "SUPABASE-FUNC-SEARCH-PATH-1"),
        (
            RULE_RLS_DISABLED_NOT_RESTORED,
            "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
        ),
    ] {
        assert!(
            findings_json.contains(rule_id),
            "{label} must fire and reach findings.json for a plain-Postgres repo with its \
             planted defect — this is the C3-3 real-path arming regression this file guards. \
             findings.json:\n{findings_json}"
        );
    }
    // Neither finding may carry the bare "preview, not corpus-documented" citation label an
    // armed-but-uncited deterministic finding gets — both rules ship real citations in the
    // bundled corpus (see their TOML files' `[[sources]]`).
    assert!(
        !findings_json.contains("not corpus-documented"),
        "both rules carry real corpus citations; neither finding should fall back to the \
         uncited-preview label: {findings_json}"
    );

    if !typst_on_path() {
        eprintln!(
            "skipping PDF-bytes assertion: typst not on PATH (zip/findings.json assertions above \
             already cover the regression)"
        );
        return;
    }
    let bytes = std::fs::read(&outcome.export_path).expect("read the written export zip");
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the export must be a valid zip");
    let mut pdf_found = false;
    for i in 0..archive.len() {
        let f = archive.by_index(i).expect("zip entry");
        if f.name().ends_with(".pdf") {
            pdf_found = true;
        }
    }
    assert!(
        pdf_found,
        "the product export must still include a compiled PDF"
    );
}

fn export_path_is_zip(path: &Path) -> bool {
    let bytes = std::fs::read(path).expect("read the written export zip");
    bytes.len() >= 2 && &bytes[0..2] == b"PK"
}

fn read_zip_entry(zip_path: &Path, entry_name: &str) -> String {
    let bytes = std::fs::read(zip_path).expect("read the written export zip");
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the export must be a valid zip");
    let mut f = archive
        .by_name(entry_name)
        .unwrap_or_else(|e| panic!("{entry_name} must be present in the export zip: {e}"));
    let mut s = String::new();
    f.read_to_string(&mut s).expect("entry must be valid UTF-8");
    s
}
