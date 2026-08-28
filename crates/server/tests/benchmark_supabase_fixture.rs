//! BENCHMARK GRADER: the acceptance harness for camerata's own scan quality, graded against
//! the externally-maintained `supabase-portal-fixture` (a seeded Next.js + Supabase repo with
//! 7 planted defects D1-D7 and 4 innocent look-alikes I1-I4). This file is the ANSWER-KEY
//! encoding, not a scan-quality fix — it exists so future scan-quality passes have a fixed,
//! honest target to move against. See the design principle in the module's own header before
//! editing an assertion: this harness grades the scan by its REAL signal (rule id + file +
//! line + severity), never by special-casing this fixture's exact strings.
//!
//! # Where the fixture and its answer key live
//!
//! Fixture repo (real git repo, external to this workspace):
//!   `CAMERATA_BENCHMARK_FIXTURE` env var, or (default)
//!   `/Users/zacharyernst/Documents/Repos/Camerata tests/supabase-portal-fixture`
//!
//! `GROUND_TRUTH.md` inside that repo is the authoritative answer key: 7 defects (D1-D7) with
//! expected file:line + severity, and 4 innocents (I1-I4) with the rationale for why a
//! correct scanner must NOT flag them. Every assertion below traces back to a specific
//! GROUND_TRUTH.md entry — read that file first if an assertion here looks arbitrary.
//!
//! Every test in this file calls [`fixture_dir`] first and returns early (skip, not fail)
//! when the fixture isn't checked out on this machine — this file must never break
//! `cargo test` in an environment that doesn't have the (private, external) fixture.
//!
//! # The two-tier split
//!
//! - **Deterministic sub-tests** (this file's `#[tokio::test]` functions with no `#[ignore]`)
//!   — always run, zero API cost (`run_ai_review: false`). They cover exactly the defects the
//!   deterministic security floor (`onboard::audit_files`) and the native architectural
//!   checkers (`camerata_checks::supabase::{rls_checker, search_path_checker}`) can answer
//!   TODAY without a model call:
//!     - D1 (member_contacts has no RLS) — `SupabaseRlsChecker` / `SUPABASE-RLS-ENABLED-1`.
//!     - D7 (SECURITY DEFINER with no search_path) — `SupabaseFnSearchPathChecker` /
//!       `SUPABASE-FUNC-SEARCH-PATH-1`.
//!     - I1 (in-test TLS suppression) — the deterministic floor's `SEC-NO-DISABLED-TLS-1` arm
//!       DOES fire on the literal match, but the test-path downgrade correctly demotes it to
//!       `low` + `in_test: true` rather than a live critical exposure. "Handled" here means
//!       correctly downgraded, not absent.
//!     - I4 (billing_accounts, the clean RLS control) — a negative control: the checker must
//!       emit zero findings for a correctly-scoped table.
//!     - D3 (committed `service_role`/`sb_secret_` value on a `NEXT_PUBLIC_` var in
//!       `.env.production`) — as of the file-collector fix (`onboard::files::is_admissible_text`)
//!       plus the `sb_secret_` match-set extension on `SEC-NO-VENDOR-TOKEN-1`,
//!       `.env.production` is admitted into the scanned file list and its secret is matched by
//!       the floor. Graded here as "found, critical, at the right file:line" rather
//!       than pinned to a specific rule id — a DEDICATED `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1`
//!       floor rule (the NEXT_PUBLIC-prefix + secret-shape correlation as its own rule id,
//!       matching the decision already recorded in
//!       `crates/rules/principles/supabase/secrets/supabase-key-service-role-client-1.toml`)
//!       is a separate, later port; see the full-grade test's D3 comment.
//!   D2, D4, D5, D6 and the I3 decoy have NO deterministic path today (no native checker
//!   answers RLS-permissive-USING(true), public storage buckets, edge-function JWT bypass, or
//!   `getSession()` trust — see the full-grade test's own comments for exactly why each is
//!   AI-tier-only right now). Asserting them here would make `cargo test` red on a clean
//!   checkout, which the task explicitly rules out — they are graded ONLY in the `#[ignore]`d
//!   full-grade test below, where a currently-failing assertion is expected and documents the
//!   gap rather than being hidden.
//!
//! - **Full-grade test** (`full_grade_supabase_fixture_scan`, `#[ignore]`d) — runs the REAL
//!   full scan (deterministic floor + architectural checkers + the AI semantic/architectural
//!   review) through the actual product entry point (`onboard::scan_repos` for Phase-1 stack
//!   detection + rule proposal, then `onboard::audit_repos` for the audit itself — the same
//!   two calls the cockpit's onboarding flow makes) and grades ALL 7 defects + the 4 decoys +
//!   several discrimination pairs pulled directly from GROUND_TRUTH.md's "grader notes". It
//!   prints a readable SCORECARD (every check, PASS/FAIL, with the actual finding — or its
//!   absence — as evidence) before asserting, so a human run shows the whole gap at once even
//!   though the underlying `assert!` necessarily stops at the first Rust panic message.
//!
//!   Costs real LLM tokens and is model-nondeterministic — never runs as part of the normal
//!   test gate. Run it explicitly:
//!
//!     cargo test -p camerata-server --test benchmark_supabase_fixture -- --ignored --nocapture
//!
//!   Requires whatever `camerata_server::llm::Llm::from_env()` needs (a live model
//!   credential) and, optionally, `CAMERATA_AUDIT_MODEL` to pin a specific model.
//!
//! # Baseline (recorded 2026-08-22, before any scan-quality fix lands; updated same day once
//! # the file-collector + `sb_secret_` fix landed)
//!
//! Deterministic-only pass over the real fixture originally found exactly 2 of the 7 defects
//! at their exact GROUND_TRUTH file:line:severity (D1, D7), correctly downgraded I1, and was
//! clean on I4. It also produced (and still produces) a MEDIUM finding for
//! `internal.audit_log` (I3) — a real false positive relative to GROUND_TRUTH's "must NOT be
//! flagged" (the checker only demotes non-exposed-schema tables from critical to medium; it
//! doesn't yet omit them) — and, before this fix, never saw `.env.production` at all: that
//! filename's extension by naive last-dot splitting (`production`) wasn't in
//! `onboard::files::CODE_EXTS`, so the file was pruned before any content was read, by EITHER
//! tier. `onboard::files::is_admissible_text` (broadened file admission: non-code config
//! extensions + a general dotfile rule that recognizes the WHOLE `.env.*` family, not just a
//! literal `.env.production` carve-out) plus a `sb_secret_` match-set extension on
//! `SEC-NO-VENDOR-TOKEN-1` together make D3 deterministically detectable — it now has its own
//! always-run sub-test below (`deterministic_d3_env_production_service_role_secret_is_found`)
//! rather than living only in the AI-tier-only bucket. D2, D4, D5, D6 still have no
//! deterministic path and depend entirely on the (not yet benchmarked-to-green) AI
//! architectural review. This is the baseline the full-grade test documents; upcoming
//! scan-quality passes are graded by how much of the remainder turns green.

use std::path::{Path, PathBuf};

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, Finding, SelectedRule};

const DEFAULT_FIXTURE_PATH: &str =
    "/Users/zacharyernst/Documents/Repos/Camerata tests/supabase-portal-fixture";
const FIXTURE_ENV_VAR: &str = "CAMERATA_BENCHMARK_FIXTURE";
const FIXTURE_REPO_SPEC: &str = "benchmark/supabase-portal-fixture";

/// Locate the `supabase-portal-fixture` benchmark repo: the `CAMERATA_BENCHMARK_FIXTURE` env
/// var when set, else the well-known path on the machine this benchmark was authored on.
/// Returns `None` (never panics) when the fixture isn't present — every test below skips
/// gracefully rather than failing, so this file is safe to ship into a `cargo test` gate that
/// runs on machines without the (private, external) fixture checked out.
fn fixture_dir() -> Option<PathBuf> {
    let path =
        std::env::var(FIXTURE_ENV_VAR).unwrap_or_else(|_| DEFAULT_FIXTURE_PATH.to_string());
    let path = PathBuf::from(path);
    if path.is_dir() && path.join(".git").exists() && path.join("GROUND_TRUTH.md").exists() {
        Some(path)
    } else {
        None
    }
}

/// Skip (not fail) the calling test when the fixture isn't present on this machine.
macro_rules! require_fixture {
    () => {
        match fixture_dir() {
            Some(dir) => dir,
            None => {
                eprintln!(
                    "skipping: supabase-portal-fixture not found — set {} or check it out at {}",
                    FIXTURE_ENV_VAR, DEFAULT_FIXTURE_PATH
                );
                return;
            }
        }
    };
}

/// Build the SAME rule selection the product's own onboarding flow would produce for this
/// fixture: Phase-1 stack detection + corpus proposal (`onboard::scan_repos`), filtered to
/// `recommended` (the domain-matched subset for the detected stack — TypeScript/JavaScript/
/// SQL + Supabase, per `onboard::propose::domains_for_stack`), each bound project-level. This
/// is the real "select the recommended starter set" action a user takes in the cockpit —
/// nothing here is hand-picked to fit this fixture's specific defects. Shared by both the
/// deterministic and full-grade scans so they audit the identical rule set (differing only in
/// whether the AI tier is switched on).
async fn recommended_selection(sources: &[(String, PathBuf)]) -> Vec<SelectedRule> {
    let phase1 = onboard::scan_repos(sources, Vec::new()).await;
    assert!(
        !phase1.stacks.is_empty(),
        "Phase-1 stack detection must see the fixture's files: {phase1:?}"
    );
    phase1
        .proposed_rules
        .into_iter()
        .filter(|r| r.recommended)
        .map(|r| {
            let directive = r
                .default_option
                .as_deref()
                .and_then(|def_id| r.options.iter().find(|o| o.id == def_id))
                .map(|o| o.directive.clone())
                .or_else(|| r.options.first().map(|o| o.directive.clone()))
                .or_else(|| r.decision_why.clone())
                .unwrap_or_else(|| r.title.clone());
            SelectedRule {
                id: r.id,
                directive,
                repos: Vec::new(), // project-level: applies to the one fixture repo
            }
        })
        .collect()
}

/// Run the real scan entry point over the fixture with the recommended selection,
/// deterministic-only (`run_ai_review: false` — zero API spend).
async fn run_deterministic_scan(dir: &Path) -> onboard::ScanReport {
    let sources = vec![(FIXTURE_REPO_SPEC.to_string(), dir.to_path_buf())];
    let selected = recommended_selection(&sources).await;
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
        true,  // run_deterministic — the floor AND the architectural checkers both run
        None,  // ledger
        camerata_server::llm::BackendResolution::Api, // backend gate: not under test here
    )
    .await;
    report
}

/// Run the real scan entry point over the fixture with the recommended selection, FULL tier
/// (`run_ai_review: true` — real model calls, real cost, nondeterministic). Only called from
/// the `#[ignore]`d full-grade test.
async fn run_full_scan(dir: &Path) -> onboard::ScanReport {
    let sources = vec![(FIXTURE_REPO_SPEC.to_string(), dir.to_path_buf())];
    let selected = recommended_selection(&sources).await;
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(),
        None, // model — resolved from CAMERATA_AUDIT_MODEL / the Llm client's default
        None, // calibration_model — same resolution
        ScanMode::Sequential,
        false, // thorough
        None,  // feedback
        None,  // job
        None,  // incremental_prior — full scan
        false, // deep — the opt-in compliance tier stays off; out of scope for this benchmark
        false, // soc2_enabled
        true,  // run_ai_review — THE real-money switch
        true,  // run_deterministic — floor + architectural checkers also run
        None,  // ledger
        camerata_server::llm::BackendResolution::Api, // backend gate: real-spend test, always allowed
    )
    .await;
    report
}

fn find_one<'a>(findings: &'a [Finding], rule_id: &str, path: &str) -> Option<&'a Finding> {
    findings
        .iter()
        .find(|f| f.rule_id == rule_id && f.path == path)
}

fn any_finding_for_path<'a>(findings: &'a [Finding], path: &str) -> Option<&'a Finding> {
    findings.iter().find(|f| f.path == path)
}

fn any_finding_in_range<'a>(
    findings: &'a [Finding],
    path: &str,
    lo: usize,
    hi: usize,
) -> Option<&'a Finding> {
    findings
        .iter()
        .find(|f| f.path == path && f.line >= lo && f.line <= hi)
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// DETERMINISTIC SUB-TESTS — always run, zero API cost. Only defects/decoys the deterministic
// floor + native architectural checkers can answer TODAY (see module doc for the full split).
// ─────────────────────────────────────────────────────────────────────────────────────────

/// D1 (GROUND_TRUTH.md): `public.member_contacts` (created at
/// `supabase/migrations/0002_profiles.sql:31`) holds PII and never enables RLS anywhere in
/// the migration timeline. Must fire `SUPABASE-RLS-ENABLED-1`, critical, at exactly that
/// file:line. The grader note explicitly warns against confusing this with `public.profiles`
/// (same file, RLS correctly enabled at line 14) — assert that table produces NO finding.
#[tokio::test]
async fn deterministic_d1_rls_enabled_flags_member_contacts_not_profiles() {
    let dir = require_fixture!();
    let report = run_deterministic_scan(&dir).await;
    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");

    let profiles_migration = "supabase/migrations/0002_profiles.sql";
    let rls_findings: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == "SUPABASE-RLS-ENABLED-1" && f.path == profiles_migration)
        .collect();
    assert_eq!(
        rls_findings.len(),
        1,
        "expected exactly one SUPABASE-RLS-ENABLED-1 finding in {profiles_migration} (member_contacts) \
         and none for the clean public.profiles table in the same file: {:?}",
        rls_findings
    );
    let f = rls_findings[0];
    assert_eq!(f.line, 31, "must point at member_contacts' CREATE TABLE statement: {f:?}");
    assert_eq!(f.severity, "critical", "an exposed PII table with no RLS is critical: {f:?}");
    assert!(
        f.snippet.contains("member_contacts") || f.detail.contains("member_contacts"),
        "finding must name member_contacts, not profiles: {f:?}"
    );
}

/// D7 (GROUND_TRUTH.md): `public.handle_new_user` (`supabase/migrations/0006_functions.sql`,
/// `create function` at line 8, `security definer` at line 11) never sets `search_path`. Must
/// fire `SUPABASE-FUNC-SEARCH-PATH-1`, high, at the function's defining line.
#[tokio::test]
async fn deterministic_d7_search_path_flags_handle_new_user() {
    let dir = require_fixture!();
    let report = run_deterministic_scan(&dir).await;
    assert!(!report.gated);

    let path = "supabase/migrations/0006_functions.sql";
    let f = find_one(&report.findings, "SUPABASE-FUNC-SEARCH-PATH-1", path)
        .unwrap_or_else(|| panic!("expected SUPABASE-FUNC-SEARCH-PATH-1 in {path}: {:?}", report.findings));
    assert_eq!(f.line, 8, "must point at the CREATE FUNCTION statement: {f:?}");
    assert_eq!(f.severity, "high", "{f:?}");
    assert!(f.snippet.contains("handle_new_user") || f.detail.contains("handle_new_user"), "{f:?}");
}

/// I1 (GROUND_TRUTH.md): `tests/integration.test.ts` disables TLS verification, but only
/// under Jest against a local self-signed stack, in a file that ships in no build. The
/// deterministic floor's `SEC-NO-DISABLED-TLS-1` arm DOES match the literal
/// `NODE_TLS_REJECT_UNAUTHORIZED = "0"` — "handled" here means the test-path downgrade
/// correctly demotes it to `low` + `in_test: true`, not that no finding exists at all. A
/// scanner that leaves this at `critical` in test-file scope would be the false positive.
#[tokio::test]
async fn deterministic_i1_test_tls_suppression_is_downgraded_not_critical() {
    let dir = require_fixture!();
    let report = run_deterministic_scan(&dir).await;
    assert!(!report.gated);

    let path = "tests/integration.test.ts";
    let matches: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == "SEC-NO-DISABLED-TLS-1" && f.path == path)
        .collect();
    assert_eq!(matches.len(), 1, "expected exactly one SEC-NO-DISABLED-TLS-1 finding in {path}: {:?}", matches);
    let f = matches[0];
    assert_eq!(f.severity, "low", "a test-scope TLS suppression must be downgraded, not critical: {f:?}");
    assert!(f.in_test, "must be tagged in_test so the report doesn't alarm on it: {f:?}");
}

/// I4 (GROUND_TRUTH.md): `public.billing_accounts` (`supabase/migrations/0005_billing.sql`)
/// is the clean, correctly owner-scoped RLS control. Negative control: the deterministic
/// engine must emit ZERO findings of any rule for this file.
#[tokio::test]
async fn deterministic_i4_billing_accounts_control_produces_no_findings() {
    let dir = require_fixture!();
    let report = run_deterministic_scan(&dir).await;
    assert!(!report.gated);

    let path = "supabase/migrations/0005_billing.sql";
    let hits: Vec<&Finding> = report.findings.iter().filter(|f| f.path == path).collect();
    assert!(
        hits.is_empty(),
        "public.billing_accounts is the clean RLS control (GROUND_TRUTH I4) — the deterministic \
         engine must not flag it, got: {:?}",
        hits
    );
}

/// D3 (GROUND_TRUTH.md): `.env.production:9` commits a `service_role`/`sb_secret_` value on a
/// `NEXT_PUBLIC_`-prefixed variable — a full RLS-bypassing credential inlined straight into
/// the client bundle. Before this fix, `.env.production` was pruned before ANY tier read its
/// content (its naive last-dot "extension" is `production`, not a recognized code extension —
/// see the module doc's baseline note); now admitted via `onboard::files::is_admissible_text`
/// and matched by the `sb_secret_`-extended `SEC-NO-VENDOR-TOKEN-1` floor arm, which (per the
/// "exposed high-privilege credential" escalation class — a committed/client-exposed
/// service_role or vendor secret) fires at CRITICAL. Asserted here as FOUND + critical (not
/// pinned to a specific rule id) — a dedicated
/// `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` floor rule correlating the `NEXT_PUBLIC_` prefix with
/// the secret shape as its own rule id is a separate, later port (see the full-grade test's D3
/// comment). The GROUND_TRUTH I2/anon-key pair is graded in the same test: the anon key on
/// line 5 is correctly public and must never be flagged.
#[tokio::test]
async fn deterministic_d3_env_production_service_role_secret_is_found() {
    let dir = require_fixture!();
    let report = run_deterministic_scan(&dir).await;
    assert!(!report.gated);

    let path = ".env.production";
    let hit = any_finding_in_range(&report.findings, path, 9, 9);
    assert!(
        hit.is_some_and(|f| f.severity == "critical"),
        "expected a CRITICAL finding at {path}:9 (a committed/client-exposed service_role \
         credential is an exposed high-privilege secret — the general class the floor reserves \
         for critical): {:?}",
        report.findings.iter().filter(|f| f.path == path).collect::<Vec<_>>()
    );
    let f = hit.expect("checked above");
    assert!(
        f.snippet.contains("NEXT_PUBLIC") && f.snippet.contains("sb_secret_"),
        "the finding must identify the NEXT_PUBLIC_ + sb_secret_ combination on the offending \
         line: {f:?}"
    );

    // I2 (grader note on D3): the anon/publishable key on line 5 is correctly public and must
    // NEVER be flagged, even though it shares the fixture's file and general vicinity.
    let anon_key_flagged = any_finding_in_range(&report.findings, path, 4, 6);
    assert!(
        anon_key_flagged.is_none(),
        "the anon/publishable key (.env.production:5) must NOT be flagged — only the \
         service_role key on line 9 is a defect: {:?}",
        anon_key_flagged
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// FULL-GRADE TEST — #[ignore]d. Runs the real deterministic + AI scan and grades every
// GROUND_TRUTH.md defect/decoy/discrimination pair. RED today by design (see module doc's
// baseline note) — this is the acceptance target for upcoming scan-quality fixes, not a test
// meant to pass on this commit.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// One graded line item in the SCORECARD: a GROUND_TRUTH-traceable id, what it checks, whether
/// it currently passes, and the finding (or its absence) that decided it — printed so a human
/// `--nocapture` run sees the whole gap in one screen, not just the first panic.
struct GradeItem {
    id: &'static str,
    description: &'static str,
    pass: bool,
    evidence: String,
}

fn grade(id: &'static str, description: &'static str, pass: bool, evidence: impl Into<String>) -> GradeItem {
    GradeItem { id, description, pass, evidence: evidence.into() }
}

fn print_scorecard(items: &[GradeItem]) {
    eprintln!("\n════════════════════════════ BENCHMARK SCORECARD ════════════════════════════");
    for item in items {
        eprintln!(
            "[{}] {:<8} {}\n         evidence: {}",
            if item.pass { "PASS" } else { "FAIL" },
            item.id,
            item.description,
            item.evidence
        );
    }
    let passed = items.iter().filter(|i| i.pass).count();
    eprintln!("────────────────────────────────────────────────────────────────────────────");
    eprintln!("TOTAL: {passed}/{} checks passing", items.len());
    eprintln!("════════════════════════════════════════════════════════════════════════════\n");
}

#[tokio::test]
#[ignore = "spends real LLM tokens — run manually: \
            cargo test -p camerata-server --test benchmark_supabase_fixture -- --ignored --nocapture"]
async fn full_grade_supabase_fixture_scan() {
    let dir = require_fixture!();
    let report = run_full_scan(&dir).await;
    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token: {:?}", report.message);

    let f = &report.findings;
    let mut scorecard: Vec<GradeItem> = Vec::new();

    // ═══ D1-D7: each defect must fire its GROUND_TRUTH rule id, at (or within a documented ═══
    // small tolerance of) the expected file:line, at the expected severity. D1/D7 mirror the
    // deterministic sub-tests above (must still hold under the full AI-augmented report); the
    // rest (D2-D6) are exclusively AI-tier today (see module doc).

    let d1 = find_one(f, "SUPABASE-RLS-ENABLED-1", "supabase/migrations/0002_profiles.sql");
    scorecard.push(grade(
        "D1",
        "member_contacts PII table has no RLS -> SUPABASE-RLS-ENABLED-1 critical @ 0002_profiles.sql:31",
        d1.is_some_and(|x| x.line == 31 && x.severity == "critical"),
        format!("{d1:?}"),
    ));

    // D2: SELECT policy `using (true)` on public.messages, line 24. AI-tier only (no native
    // permissive-policy checker exists yet) — small +/-2 line tolerance for AI attribution.
    let d2 = any_finding_in_range(f, "supabase/migrations/0003_messages.sql", 22, 26)
        .filter(|x| x.rule_id == "SUPABASE-RLS-PERMISSIVE-TRUE-1");
    scorecard.push(grade(
        "D2",
        "messages SELECT policy `using (true)` -> SUPABASE-RLS-PERMISSIVE-TRUE-1 high @ 0003_messages.sql:~24",
        d2.is_some_and(|x| x.severity == "high"),
        format!("{d2:?}"),
    ));

    // D3: NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY committed at .env.production:9. The prior
    // structural blocker (`.env.production` pruned before ANY tier read it, because its naive
    // last-dot extension `production` wasn't in `onboard::files::CODE_EXTS`) is fixed —
    // `onboard::files::is_admissible_text` admits the file, so it's visible to every tier now,
    // including this one. What's assigned specifically to the AI tier (not yet built) is the
    // DEDICATED `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` rule id — correlating the `NEXT_PUBLIC_`
    // prefix with the secret shape as its own finding, distinct from the generic
    // `SEC-NO-VENDOR-TOKEN-1` floor hit the always-run deterministic sub-test
    // (`deterministic_d3_env_production_service_role_secret_is_found`) already asserts. A
    // floor port of this exact rule id (per the decision already recorded in
    // `crates/rules/principles/supabase/secrets/supabase-key-service-role-client-1.toml`) is a
    // separate, later change.
    let d3 = find_one(f, "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1", ".env.production");
    scorecard.push(grade(
        "D3",
        "service_role key on a NEXT_PUBLIC_ var -> SUPABASE-KEY-SERVICE-ROLE-CLIENT-1 critical @ .env.production:9 \
         (blocked today: CODE_EXTS excludes the .env.production filename entirely)",
        d3.is_some_and(|x| x.line == 9 && x.severity == "critical"),
        format!("{d3:?}"),
    ));

    // D4: public storage bucket for member documents, lines 32-34.
    let d4 = any_finding_in_range(f, "supabase/migrations/0004_documents.sql", 32, 34)
        .filter(|x| x.rule_id == "SUPABASE-STORAGE-PUBLIC-BUCKET-1");
    scorecard.push(grade(
        "D4",
        "member-documents storage bucket created public=true -> SUPABASE-STORAGE-PUBLIC-BUCKET-1 high @ 0004_documents.sql:32-34",
        d4.is_some_and(|x| x.severity == "high"),
        format!("{d4:?}"),
    ));

    // D5: verify_jwt=false + no compensating auth in the charge function. May be attributed to
    // either the config.toml declaration (line 37) or the handler itself (lines 30-46).
    let d5 = any_finding_in_range(f, "supabase/config.toml", 36, 38)
        .or_else(|| any_finding_in_range(f, "supabase/functions/charge/index.ts", 28, 48))
        .filter(|x| x.rule_id == "SUPABASE-AUTH-EDGE-JWT-1");
    scorecard.push(grade(
        "D5",
        "charge edge function: verify_jwt=false + no compensating auth check -> \
         SUPABASE-AUTH-EDGE-JWT-1 critical @ config.toml:37 or functions/charge/index.ts:30-46",
        d5.is_some_and(|x| x.severity == "critical"),
        format!("{d5:?}"),
    ));

    // D6: getSession() trusted for identity in a Server Action, line 17.
    let d6 = any_finding_in_range(f, "app/profile/actions.ts", 15, 23)
        .filter(|x| x.rule_id == "SUPABASE-AUTH-GETSESSION-SERVER-1");
    scorecard.push(grade(
        "D6",
        "profile Server Action trusts getSession() for identity -> \
         SUPABASE-AUTH-GETSESSION-SERVER-1 high @ app/profile/actions.ts:~17",
        d6.is_some_and(|x| x.severity == "high"),
        format!("{d6:?}"),
    ));

    let d7 = find_one(f, "SUPABASE-FUNC-SEARCH-PATH-1", "supabase/migrations/0006_functions.sql");
    scorecard.push(grade(
        "D7",
        "handle_new_user is SECURITY DEFINER with no search_path -> SUPABASE-FUNC-SEARCH-PATH-1 high @ 0006_functions.sql:8",
        d7.is_some_and(|x| x.line == 8 && x.severity == "high"),
        format!("{d7:?}"),
    ));

    // ═══ Aggregate: the three criticals (D1, D3, D5) must all be present AND critical. ═══
    let criticals_ok = d1.is_some_and(|x| x.severity == "critical")
        && d3.is_some_and(|x| x.severity == "critical")
        && d5.is_some_and(|x| x.severity == "critical");
    scorecard.push(grade(
        "CRITICALS",
        "all three critical-severity defects (D1, D3, D5) are present at critical severity",
        criticals_ok,
        format!(
            "D1={} D3={} D5={}",
            d1.map(|x| x.severity.as_str()).unwrap_or("<missing>"),
            d3.map(|x| x.severity.as_str()).unwrap_or("<missing>"),
            d5.map(|x| x.severity.as_str()).unwrap_or("<missing>"),
        ),
    ));

    // ═══ Discrimination pairs (GROUND_TRUTH "grader notes") ═══

    // I3: internal.audit_log has no RLS but lives in a non-exposed schema — must NOT be
    // flagged as an ACTIONABLE defect. Corrected behavior (scan-quality refinement Item 4):
    // `SupabaseRlsChecker` no longer emits a medium "defense-in-depth" finding for a
    // non-exposed schema; exposed-schema membership is a reachability precondition for
    // `SUPABASE-RLS-ENABLED-1`, so a table PostgREST cannot serve is not a defect. The
    // observation is not dropped (over-tell) — it is emitted at `info` severity and routed to
    // the informational channel, never into do_now/do_next/plan. So the assertion is: no
    // actionable (non-`info`) finding for this file.
    let i3_actionable = f
        .iter()
        .find(|x| x.path == "supabase/migrations/0001_init.sql" && x.severity != "info");
    scorecard.push(grade(
        "I3",
        "internal.audit_log (non-exposed schema) must NOT be flagged as an actionable defect \
         (only an informational note is allowed)",
        i3_actionable.is_none(),
        format!("{i3_actionable:?}"),
    ));

    // charge (D5) flagged / notify (the JWT-verified, getUser()-checked control) spared.
    let notify_flagged = any_finding_for_path(f, "supabase/functions/notify/index.ts");
    scorecard.push(grade(
        "PAIR-EDGE-JWT",
        "charge (verify_jwt=false, no auth) flagged / notify (verify_jwt=true + getUser()) spared",
        d5.is_some() && notify_flagged.is_none(),
        format!("charge_finding_present={} notify_finding={:?}", d5.is_some(), notify_flagged),
    ));

    // messages SELECT policy (D2) flagged / the correctly-scoped INSERT policy (lines 27-30)
    // spared.
    let insert_policy_flagged = any_finding_in_range(f, "supabase/migrations/0003_messages.sql", 27, 30);
    scorecard.push(grade(
        "PAIR-RLS-PERMISSIVE",
        "messages SELECT policy (using(true)) flagged / the owner-scoped INSERT policy spared",
        d2.is_some() && insert_policy_flagged.is_none(),
        format!("select_finding_present={} insert_policy_finding={:?}", d2.is_some(), insert_policy_flagged),
    ));

    // The anon (public) key on .env.production:5 must never be flagged — only the
    // service_role key on line 9 (D3) is the defect.
    let anon_key_flagged = any_finding_in_range(f, ".env.production", 4, 6);
    scorecard.push(grade(
        "PAIR-ANON-KEY",
        "the anon key (.env.production:5) is NOT flagged (only the service_role key on line 9 is)",
        anon_key_flagged.is_none(),
        format!("{anon_key_flagged:?}"),
    ));

    // I2: scripts/seed.ts + lib/supabase/admin.ts legitimately use the server-only
    // (non-NEXT_PUBLIC_) service_role key — must not be flagged.
    let seed_flagged = any_finding_for_path(f, "scripts/seed.ts")
        .filter(|x| x.rule_id.contains("KEY-SERVICE-ROLE") || x.rule_id.contains("SECRET"));
    let admin_flagged = any_finding_for_path(f, "lib/supabase/admin.ts")
        .filter(|x| x.rule_id.contains("KEY-SERVICE-ROLE") || x.rule_id.contains("SECRET"));
    scorecard.push(grade(
        "I2",
        "server-only service_role usage (scripts/seed.ts, lib/supabase/admin.ts) is NOT flagged as a secret exposure",
        seed_flagged.is_none() && admin_flagged.is_none(),
        format!("seed={seed_flagged:?} admin={admin_flagged:?}"),
    ));

    // I1: the in-test TLS suppression must still be present-but-downgraded under the full
    // (deterministic + AI) report, exactly as the deterministic-only suite already proves.
    let i1 = report
        .findings
        .iter()
        .find(|x| x.rule_id == "SEC-NO-DISABLED-TLS-1" && x.path == "tests/integration.test.ts");
    scorecard.push(grade(
        "I1",
        "in-test TLS suppression (tests/integration.test.ts) stays downgraded (low, in_test) under the full report",
        i1.is_some_and(|x| x.severity == "low" && x.in_test),
        format!("{i1:?}"),
    ));

    // I4: the clean billing_accounts control must stay clean even with the AI tier active
    // (the AI tier is where a false positive could newly appear that the deterministic-only
    // suite above can't catch).
    let i4 = any_finding_for_path(f, "supabase/migrations/0005_billing.sql");
    scorecard.push(grade(
        "I4",
        "billing_accounts (the clean RLS control) produces no finding even under the full AI-augmented scan",
        i4.is_none(),
        format!("{i4:?}"),
    ));

    print_scorecard(&scorecard);

    let failed: Vec<&str> = scorecard.iter().filter(|g| !g.pass).map(|g| g.id).collect();
    assert!(
        failed.is_empty(),
        "benchmark FAILED {}/{} checks: {:?} — see the SCORECARD above (run with --nocapture) for the evidence \
         behind each one",
        failed.len(),
        scorecard.len(),
        failed
    );
}
