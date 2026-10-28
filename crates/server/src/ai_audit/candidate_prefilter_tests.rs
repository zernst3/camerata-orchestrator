//! Rank 8: the candidate prefilter. What does a scan send a rule that names a candidate locator,
//! with the switch on and off, when the locator succeeds, finds nothing, fails, hangs, or meets a
//! file it cannot parse? No network: the model is the rank-7 recording stub, and the prompts are
//! read back out of what it was sent.

use std::time::Duration;

use super::applies_to_routing_tests::Recorder;
use super::candidate_passes::{CandidateEnv, LocateFn};
use super::*;
use crate::cost_cap::RunControls;
use crate::efficiency::EfficiencyConfig;
use crate::scan_routing::{plan_routes, AuditRule};
use camerata_checks::candidates::{CandidateLocator, FileCandidates, LocateError};
use camerata_rules::{AppliesTo, Candidates};

const REPO: &str = "acme/notes";
const MIGRATION: &str = "supabase/migrations/20240101_init.sql";
const CONFIG: &str = "supabase/config.toml";

/// A migration of 120 tables with ONE security-definer function in the middle (lines 241-244),
/// so a candidate digest is a small share of the whole one.
fn migration(with_function: bool) -> String {
    let table =
        |i: usize| format!("create table public.t{i} (\n  id uuid primary key,\n  note text\n);\n");
    let mut s: String = (0..60).map(table).collect();
    if with_function {
        s.push_str(
            "create function public.leak() returns setof public.t1\nlanguage sql\nsecurity definer\nas $$ select * from public.t1 $$;\n",
        );
    }
    s.extend((60..120).map(table));
    s
}

fn supabase_files(with_function: bool) -> Vec<(String, String)> {
    vec![
        (
            CONFIG.to_string(),
            "[api]\nschemas = [\"public\"]\n".to_string(),
        ),
        (MIGRATION.to_string(), migration(with_function)),
    ]
}

/// A Supabase function rule: scoped to SQL plus the config file, located by `sql_routines`.
fn fn_rule() -> AuditRule {
    AuditRule::new("SUPA-FN-1", "SECURITY DEFINER functions pin search_path.")
        .with_applies_to(Some(
            AppliesTo::new(
                vec![
                    "**/*.sql".to_string(),
                    "**/supabase/config.toml".to_string(),
                ],
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        ))
        .with_candidates(Some(Candidates::new("sql_routines").unwrap()))
}

fn plain(id: &str) -> AuditRule {
    AuditRule::new(id, format!("directive of {id}"))
}

fn efficiency(prefilter: bool) -> EfficiencyConfig {
    let mut config = EfficiencyConfig::default();
    config.set("candidates_prefilter", prefilter).unwrap();
    config
}

/// Run the real-time routed passes (the production path below `audit_repo`) with `env`, one pass
/// at a time, and return every prompt in send order plus the meter.
async fn run_with(
    rules: &[AuditRule],
    files: &[(String, String)],
    prefilter: bool,
    env: CandidateEnv,
) -> (Vec<String>, UsageMeter) {
    let rec = Recorder::default();
    let m = UsageMeter::with_controls(RunControls {
        efficiency: efficiency(prefilter),
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
    });
    let plan = plan_routes(rules, files, true);
    let repo_map = build_repo_map(files);
    let adopted = rules.iter().map(|r| r.id.to_ascii_uppercase()).collect();
    let mut skipped = Vec::new();
    let (_f, _p, _r, _ok, err) = run_routed_passes(
        &rec,
        REPO,
        files,
        rules,
        &plan,
        &repo_map,
        &adopted,
        None,
        None,
        None,
        1,
        RULE_BATCH_SIZE,
        Some(&m),
        &mut skipped,
        env,
    )
    .await;
    assert!(err.is_empty(), "{err:?}");
    assert!(skipped.is_empty(), "{skipped:?}");
    let prompts = rec.prompts.lock().unwrap().clone();
    (prompts, m)
}

async fn run(
    rules: &[AuditRule],
    files: &[(String, String)],
    prefilter: bool,
) -> (Vec<String>, UsageMeter) {
    run_with(rules, files, prefilter, CandidateEnv::production()).await
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn carrying<'a>(prompts: &'a [String], rule: &str) -> Vec<&'a String> {
    let tag = format!("- [{rule}] ");
    prompts.iter().filter(|p| p.contains(&tag)).collect()
}

fn failing(_: CandidateLocator, path: &str, _: &str) -> Result<FileCandidates, LocateError> {
    Err(LocateError(format!("cannot read {path}")))
}

fn hanging(l: CandidateLocator, path: &str, content: &str) -> Result<FileCandidates, LocateError> {
    std::thread::sleep(Duration::from_secs(1));
    camerata_checks::candidates::locate(l, path, content)
}

fn env_with(locate: LocateFn, timeout: Duration) -> CandidateEnv {
    CandidateEnv { locate, timeout }
}

#[tokio::test]
async fn a_candidate_prompt_carries_only_the_sites_with_context_and_the_unread_files_whole() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let (prompts, m) = run(&rules, &supabase_files(true), true).await;

    let fn_prompts = carrying(&prompts, "SUPA-FN-1");
    assert_eq!(fn_prompts.len(), 1, "one chunk, one candidate pass");
    let p = fn_prompts[0];
    assert!(p.contains("// ===== CANDIDATE SITES (locator: sql_routines) ====="));
    assert!(p.contains(&format!("// ===== FILE: {MIGRATION} (excerpts) =====")));
    // The function (lines 241-244) with 8 lines of context on each side, real line numbers.
    assert!(p.contains(" 243| security definer\n"), "{p}");
    assert!(p.contains(" 233| ") && p.contains(" 252| "));
    assert!(!p.contains(" 232| ") && !p.contains(" 253| "));
    assert!(p.contains("     ... (lines 1-232 not shown)\n"));
    assert!(
        !p.contains("create table public.t5 ("),
        "unrelated statements are not sent"
    );
    // The config file is not SQL: the locator does not read it, so it is shown whole.
    assert!(p.contains(&format!(
        "// ===== FILE: {CONFIG} (whole) =====\n   1| [api]\n"
    )));
    // The repo map still leads the prompt.
    assert!(p.contains(&build_repo_map(&supabase_files(true))));
    // No advisory on excerpts, and no other rule rides along.
    assert!(p.contains("against ONLY the ADOPTED rules"), "{p}");
    assert!(!p.contains("- [ARCH-1] "));

    // The whole-file prompt (ARCH-1, with the advisory pass) no longer carries the rule.
    let arch = carrying(&prompts, "ARCH-1");
    assert_eq!(arch.len(), 1);
    assert!(arch[0].contains(&format!("// ===== FILE: {MIGRATION} =====")));
    assert!(arch[0].contains("ALSO flag any other genuine issues"));

    let t = m.stage_telemetry(0).candidates.expect("recorded");
    let u = &t.rules["SUPA-FN-1"];
    assert_eq!((u.chunks_narrowed, u.chunks_fallback), (1, 0));
    assert!(
        u.bytes_saved() * 2 > u.digest_bytes_whole,
        "most of the digest is not sent: {u:?}"
    );
    assert!(t.fallbacks.is_empty());
}

#[tokio::test]
async fn a_failing_locator_falls_back_to_the_whole_file_prompt_and_is_recorded() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = supabase_files(true);
    let (failed, m) = run_with(
        &rules,
        &files,
        true,
        env_with(failing, Duration::from_secs(5)),
    )
    .await;
    let (off, _) = run(&rules, &files, false).await;
    assert_eq!(
        sorted(failed.clone()),
        sorted(off),
        "a failed locator sends exactly the whole-file prompts the switch-off scan sends"
    );
    assert!(failed.iter().all(|p| !p.contains("CANDIDATE SITES")));
    assert!(
        carrying(&failed, "SUPA-FN-1")[0].contains(&format!("// ===== FILE: {MIGRATION} ====="))
    );

    let t = m.stage_telemetry(0).candidates.expect("recorded");
    assert_eq!(t.rules["SUPA-FN-1"].chunks_fallback, 1);
    assert_eq!(t.rules["SUPA-FN-1"].bytes_saved(), 0);
    assert_eq!(t.fallbacks.len(), 1);
    assert!(
        t.fallbacks[0].reason.contains("cannot read"),
        "{:?}",
        t.fallbacks
    );
    assert_eq!(t.fallbacks[0].locator, "sql_routines");
}

#[tokio::test]
async fn a_locator_that_times_out_falls_back_to_the_whole_file_prompt() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = supabase_files(true);
    let (slow, m) = run_with(
        &rules,
        &files,
        true,
        env_with(hanging, Duration::from_millis(100)),
    )
    .await;
    let (off, _) = run(&rules, &files, false).await;
    assert_eq!(sorted(slow), sorted(off));
    let t = m.stage_telemetry(0).candidates.expect("recorded");
    assert_eq!(t.fallbacks.len(), 1);
    assert!(
        t.fallbacks[0].reason.contains("did not finish"),
        "{:?}",
        t.fallbacks
    );
}

#[tokio::test]
async fn an_unparseable_sql_file_falls_back_to_the_whole_file_prompt_never_zero_sites() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    // The only routine is in a file whose dollar quote never closes: the splitter cannot say
    // where its statements end, so "no site" would be a guess.
    let mut files = supabase_files(false);
    files.push((
        "supabase/migrations/20240102_fn.sql".to_string(),
        "create function public.f() returns int\nsecurity definer\nas $$ select 1;\n".to_string(),
    ));
    let (prompts, m) = run(&rules, &files, true).await;
    let (off, _) = run(&rules, &files, false).await;
    assert_eq!(sorted(prompts.clone()), sorted(off));
    let fn_prompts = carrying(&prompts, "SUPA-FN-1");
    assert_eq!(
        fn_prompts.len(),
        1,
        "the rule is still sent, with whole files"
    );
    assert!(fn_prompts[0].contains("// ===== FILE: supabase/migrations/20240102_fn.sql ====="));

    let t = m.stage_telemetry(0).candidates.expect("recorded");
    assert_eq!(t.rules["SUPA-FN-1"].chunks_without_sites, 0);
    assert_eq!(t.rules["SUPA-FN-1"].chunks_fallback, 1);
    assert!(
        t.fallbacks[0]
            .reason
            .contains("20240102_fn.sql does not parse as SQL"),
        "{:?}",
        t.fallbacks
    );
}

#[tokio::test]
async fn switch_off_sends_byte_identical_prompts_to_a_rule_without_candidates() {
    let files = supabase_files(true);
    let with_candidates = vec![plain("ARCH-1"), fn_rule()];
    let without = vec![plain("ARCH-1"), fn_rule().with_candidates(None)];
    let (off, m) = run(&with_candidates, &files, false).await;
    let (rank7, _) = run(&without, &files, true).await;
    assert_eq!(
        sorted(off.clone()),
        sorted(rank7),
        "switch off: `candidates` is ignored entirely"
    );
    assert!(off.iter().all(|p| !p.contains("CANDIDATE SITES")));
    assert!(
        m.stage_telemetry(0).candidates.is_none(),
        "nothing is narrowed or recorded with the switch off"
    );
}

#[tokio::test]
async fn a_chunk_the_locator_read_with_no_site_sends_the_rule_nothing() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let sql_only = vec![(MIGRATION.to_string(), migration(false))];
    let (prompts, m) = run(&rules, &sql_only, true).await;
    assert!(
        carrying(&prompts, "SUPA-FN-1").is_empty(),
        "no routine statement anywhere: nothing for the rule to judge"
    );
    assert_eq!(carrying(&prompts, "ARCH-1").len(), 1);
    let t = m.stage_telemetry(0).candidates.expect("recorded");
    assert_eq!(t.rules["SUPA-FN-1"].chunks_without_sites, 1);

    // With the config file in the chunk the locator has NOT read everything: that file is never
    // "no site", so the rule is still sent it, whole, and nothing else.
    let (prompts, _) = run(&rules, &supabase_files(false), true).await;
    let fn_prompts = carrying(&prompts, "SUPA-FN-1");
    assert_eq!(fn_prompts.len(), 1);
    assert!(fn_prompts[0].contains(&format!("// ===== FILE: {CONFIG} (whole) =====")));
    assert!(!fn_prompts[0].contains("(excerpts)"));
}

#[tokio::test]
async fn the_advisory_pass_keeps_whole_files_when_every_rule_of_a_chunk_is_narrowed() {
    // No `All` group: the scoped group runs the advisory pass itself. Its only rule is narrowed,
    // so the chunk still gets one whole-file pass with no adopted rule, asking for novel issues.
    let (prompts, _) = run(&[fn_rule()], &supabase_files(true), true).await;
    assert_eq!(prompts.len(), 2, "{prompts:#?}");
    let advisory: Vec<&String> = prompts
        .iter()
        .filter(|p| p.contains("ALSO flag any other genuine issues"))
        .collect();
    assert_eq!(advisory.len(), 1);
    assert!(advisory[0].contains(&format!("// ===== FILE: {MIGRATION} =====")));
    assert!(!advisory[0].contains("- [SUPA-FN-1] "));
    assert!(!advisory[0].contains("CANDIDATE SITES"));
    assert_eq!(carrying(&prompts, "SUPA-FN-1").len(), 1);
}

#[tokio::test]
async fn a_chunk_mostly_of_unread_files_keeps_the_rule_in_the_whole_file_prompt() {
    // An unscoped candidate rule meets a large TypeScript file it cannot read: a candidate prompt
    // would carry that file whole and cost more than it saves, so nothing changes.
    let rules = vec![plain("ARCH-1"), fn_rule().with_applies_to(None)];
    let mut files = supabase_files(true);
    files.push((
        "src/app.ts".to_string(),
        "export const x = 1;\n".repeat(4_000),
    ));
    let (prompts, m) = run(&rules, &files, true).await;
    let (off, _) = run(&rules, &files, false).await;
    assert_eq!(sorted(prompts), sorted(off));
    let u = &m.stage_telemetry(0).candidates.expect("recorded").rules["SUPA-FN-1"];
    assert_eq!((u.chunks_not_narrowed, u.chunks_fallback), (1, 0));
}

#[tokio::test]
async fn the_telemetry_table_names_each_rule_its_saving_and_each_fallback() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let (_, m) = run_with(
        &rules,
        &supabase_files(true),
        true,
        env_with(failing, Duration::from_secs(5)),
    )
    .await;
    let table = m.stage_telemetry(0).render_table();
    assert!(table.contains("candidate prefilter: 1 rules"), "{table}");
    assert!(table.contains("SUPA-FN-1 [sql_routines]: narrowed 0, no sites 0, not narrowed 0, fallback 1; 0 bytes saved"), "{table}");
    assert!(
        table.contains("fallback: acme/notes pass[scoped] chunk 1 [sql_routines]: cannot read"),
        "{table}"
    );
    assert!(!table.contains('\u{2014}'));
}

#[test]
fn the_switch_is_on_by_default_and_recorded_in_the_run_line() {
    assert!(EfficiencyConfig::default().candidates_prefilter);
    assert!(efficiency(false)
        .render()
        .contains("candidates_prefilter=off"));
}
