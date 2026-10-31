//! Rank 8: the candidate prefilter. What does a scan send a rule that names a candidate locator,
//! with the switch on and off, when the locator succeeds, finds nothing, fails, hangs, or meets a
//! file it cannot parse? No network: the model is the rank-7 recording stub, and the prompts are
//! read back out of what it was sent.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// The delimiter token of the one run every test here drives.
fn run_nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef")
}

/// The opening line of `path`'s frame in a prompt built under [`run_nonce`].
fn opening(path: &str) -> String {
    format!("<<<UNTRUSTED {} \"{path}\">>>\n", run_nonce())
}

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
        clone_judge_model: None,
        consensus: false,
    });
    let plan = plan_routes(rules, files, true);
    let repo_map = build_repo_map(files, &run_nonce());
    let adopted = rules.iter().map(|r| r.id.to_ascii_uppercase()).collect();
    let mut skipped = Vec::new();
    let (_f, _p, _r, _ok, err) = run_routed_passes(
        &rec,
        REPO,
        files,
        rules,
        &plan,
        &repo_map,
        &run_nonce(),
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

fn failing(
    _: CandidateLocator,
    path: &str,
    _: &str,
    _: &AtomicBool,
) -> Result<FileCandidates, LocateError> {
    Err(LocateError(format!("cannot read {path}")))
}

fn hanging(
    l: CandidateLocator,
    path: &str,
    content: &str,
    cancel: &AtomicBool,
) -> Result<FileCandidates, LocateError> {
    std::thread::sleep(Duration::from_secs(1));
    camerata_checks::candidates::locate_until(l, path, content, cancel)
}

fn env_with(locate: LocateFn, timeout: Duration) -> CandidateEnv {
    CandidateEnv {
        locate,
        timeout,
        ..CandidateEnv::production()
    }
}

#[tokio::test]
async fn a_candidate_prompt_carries_only_the_sites_with_context_and_the_unread_files_whole() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let (prompts, m) = run(&rules, &supabase_files(true), true).await;

    let fn_prompts = carrying(&prompts, "SUPA-FN-1");
    assert_eq!(fn_prompts.len(), 1, "one chunk, one candidate pass");
    let p = fn_prompts[0];
    assert!(p.contains("// ===== CANDIDATE SITES (locator: sql_routines) ====="));
    assert!(p.contains(&opening(MIGRATION)));
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
    assert!(p.contains(&format!("{}   1| [api]\n", opening(CONFIG))));
    // The repo map still leads the prompt.
    assert!(p.contains(&build_repo_map(&supabase_files(true), &run_nonce())));
    // No advisory on excerpts, and no other rule rides along.
    assert!(p.contains("against ONLY the ADOPTED rules"), "{p}");
    assert!(!p.contains("- [ARCH-1] "));

    // The whole-file prompt (ARCH-1, with the advisory pass) no longer carries the rule.
    let arch = carrying(&prompts, "ARCH-1");
    assert_eq!(arch.len(), 1);
    assert!(arch[0].contains(&opening(MIGRATION)));
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
    assert!(carrying(&failed, "SUPA-FN-1")[0].contains(&opening(MIGRATION)));

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
    assert!(fn_prompts[0].contains(&opening("supabase/migrations/20240102_fn.sql")));

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
    assert!(fn_prompts[0].contains(&opening(CONFIG)));
    assert!(fn_prompts[0].contains("CANDIDATE SITES"));
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
    assert!(advisory[0].contains(&opening(MIGRATION)));
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

// ── the locator's work is bounded, and cancelled when the scan stops waiting ──────────────

static SPIN_SAW_CANCEL: AtomicBool = AtomicBool::new(false);

/// Runs until the scan raises the cancel flag, the way a locator stuck in a huge file would.
fn spinning(
    _: CandidateLocator,
    _: &str,
    _: &str,
    cancel: &AtomicBool,
) -> Result<FileCandidates, LocateError> {
    while !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(5));
    }
    SPIN_SAW_CANCEL.store(true, Ordering::SeqCst);
    Err(LocateError("cancelled".to_string()))
}

#[tokio::test]
async fn a_timeout_cancels_the_blocking_work_instead_of_leaving_it_running() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = supabase_files(true);
    let (prompts, m) = run_with(
        &rules,
        &files,
        true,
        env_with(spinning, Duration::from_millis(100)),
    )
    .await;
    let (off, _) = run(&rules, &files, false).await;
    assert_eq!(sorted(prompts), sorted(off), "a timeout sends whole files");
    let t = m.stage_telemetry(0).candidates.expect("recorded");
    assert!(
        t.fallbacks[0].reason.contains("did not finish"),
        "{:?}",
        t.fallbacks
    );
    // The thread was told to stop: it saw the flag and returned, rather than spinning forever.
    for _ in 0..200 {
        if SPIN_SAW_CANCEL.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the locator never saw the cancel flag: the blocking thread is still running");
}

static COUNTED_CALLS: AtomicUsize = AtomicUsize::new(0);

fn counting(
    l: CandidateLocator,
    path: &str,
    content: &str,
    cancel: &AtomicBool,
) -> Result<FileCandidates, LocateError> {
    COUNTED_CALLS.fetch_add(1, Ordering::SeqCst);
    camerata_checks::candidates::locate_until(l, path, content, cancel)
}

#[tokio::test]
async fn a_chunk_over_the_input_cap_falls_back_without_running_the_locator() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = supabase_files(true);
    let env = CandidateEnv {
        locate: counting,
        timeout: Duration::from_secs(5),
        max_input_bytes: 100,
    };
    let (prompts, m) = run_with(&rules, &files, true, env).await;
    let (off, _) = run(&rules, &files, false).await;
    assert_eq!(sorted(prompts), sorted(off));
    assert_eq!(
        COUNTED_CALLS.load(Ordering::SeqCst),
        0,
        "no file of an oversized chunk reaches the locator"
    );
    let t = m.stage_telemetry(0).candidates.expect("recorded");
    assert_eq!(t.rules["SUPA-FN-1"].chunks_fallback, 1);
    assert!(
        t.fallbacks[0].reason.contains("byte limit"),
        "{:?}",
        t.fallbacks
    );
}

#[tokio::test]
async fn a_fallback_still_sends_whole_files_when_no_meter_is_attached() {
    // The fallback notice is printed whether or not a meter records it; the behaviour (whole
    // files) must not depend on the meter either.
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = supabase_files(true);
    let rec = Recorder::default();
    let plan = plan_routes(&rules, &files, true);
    let repo_map = build_repo_map(&files, &run_nonce());
    let adopted = rules.iter().map(|r| r.id.to_ascii_uppercase()).collect();
    let mut skipped = Vec::new();
    let (_f, _p, _r, ok, failures) = run_routed_passes(
        &rec,
        REPO,
        &files,
        &rules,
        &plan,
        &repo_map,
        &run_nonce(),
        &adopted,
        None,
        None,
        None,
        1,
        RULE_BATCH_SIZE,
        None,
        &mut skipped,
        env_with(failing, Duration::from_secs(5)),
    )
    .await;
    assert!(failures.is_empty(), "{failures:?}");
    assert!(ok > 0);
    let prompts = rec.prompts.lock().unwrap().clone();
    assert!(prompts.iter().all(|p| !p.contains("CANDIDATE SITES")));
    assert_eq!(carrying(&prompts, "SUPA-FN-1").len(), 1);
}

// ── several chunks: a rule can be narrowed in one and kept whole in another ───────────────

const CHUNK_A: &str = "supabase/migrations/20240101_tables.sql";
const CHUNK_B: &str = "supabase/migrations/20240102_functions.sql";

/// Two SQL files that cannot share a chunk (each is over half of the chunk budget). The first is
/// 3000 tables with one security-definer function in the middle (a tiny share of the file); the
/// second is 2200 functions and nothing else (all of it is a site).
fn two_chunk_files() -> Vec<(String, String)> {
    let mut a = migration(true);
    let table =
        |i: usize| format!("create table public.t{i} (\n  id uuid primary key,\n  note text\n);\n");
    a.extend((120..3000).map(table));
    let b: String = (0..2200)
        .map(|i| {
            format!(
                "create function public.f{i}() returns int\nlanguage sql\nsecurity definer\nas $$ select {i} $$;\n"
            )
        })
        .collect();
    vec![(CHUNK_A.to_string(), a), (CHUNK_B.to_string(), b)]
}

#[tokio::test]
async fn a_rule_narrowed_in_one_chunk_and_kept_whole_in_another_sees_each_chunk_exactly_once() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = two_chunk_files();
    assert_eq!(
        chunk_files(&files, CHUNK_DIGEST_CHARS).len(),
        2,
        "the fixture must span two chunks"
    );
    let (prompts, m) = run(&rules, &files, true).await;
    let (off, _) = run(&rules, &files, false).await;

    let header = |path: &str| opening(path);
    let fn_prompts = carrying(&prompts, "SUPA-FN-1");
    assert_eq!(
        fn_prompts.len(),
        2,
        "one pass per chunk, no chunk twice or never"
    );
    let candidate: Vec<&&String> = fn_prompts
        .iter()
        .filter(|p| p.contains("CANDIDATE SITES"))
        .collect();
    let whole: Vec<&&String> = fn_prompts
        .iter()
        .filter(|p| !p.contains("CANDIDATE SITES"))
        .collect();
    assert_eq!((candidate.len(), whole.len()), (1, 1), "{fn_prompts:#?}");

    // Chunk 1: excerpts of the tables file only.
    assert!(candidate[0].contains(&opening(CHUNK_A)));
    assert!(!candidate[0].contains(&opening(CHUNK_B)));
    // Chunk 2: the functions file whole, exactly the prompt the switch-off scan sends for it.
    assert!(whole[0].contains(&header(CHUNK_B)));
    assert!(!whole[0].contains(&opening(CHUNK_A)));
    // It is the switch-off prompt for that chunk, down to the byte, except for the pass label's
    // position counter (this pass covers one chunk, the switch-off scan's covers chunk 2 of 2).
    assert!(
        off.iter()
            .any(|p| p.replace("(pass[scoped] 2/2)", "(pass[scoped] 1/1)") == **whole[0]),
        "the kept-whole chunk's prompt differs from the switch-off prompt"
    );

    let u = &m.stage_telemetry(0).candidates.expect("recorded").rules["SUPA-FN-1"];
    assert_eq!(
        (u.chunks_narrowed, u.chunks_not_narrowed, u.chunks_fallback),
        (1, 1, 0)
    );
    // The other rule is untouched in both chunks.
    assert_eq!(carrying(&prompts, "ARCH-1").len(), 2);
}

// ── batch mode sends what real-time sends ─────────────────────────────────────────────────

/// A Message Batches API that answers `{}` to every item and keeps each item's prompt text.
struct PromptBatch {
    realtime: Recorder,
    items: std::sync::Mutex<Vec<crate::llm::BatchItem>>,
}

fn batch_response() -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        text: "{}".to_string(),
        model: "stub".to_string(),
        backend: "api/batch".to_string(),
        cost_usd: Some(0.0),
        input_tokens: Some(10),
        output_tokens: Some(1),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

#[async_trait::async_trait]
impl batch_mode::BatchApi for PromptBatch {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn crate::llm::LlmPort {
        &self.realtime
    }
    async fn count_input_tokens(&self, _req: &crate::llm::LlmRequest, _model: &str) -> Option<u64> {
        Some(1_000)
    }
    async fn submit(&self, items: Vec<crate::llm::BatchItem>) -> anyhow::Result<String> {
        let mut all = self.items.lock().unwrap();
        let id = format!("msgbatch_{}", all.len());
        all.extend(items);
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<crate::llm::BatchStatus> {
        Ok(crate::llm::BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: crate::llm::BatchRequestCounts::default(),
        })
    }
    async fn fetch(
        &self,
        _batch_id: &str,
        _ttl: crate::llm::CacheTtl,
    ) -> anyhow::Result<Vec<crate::llm::BatchResultRow>> {
        // Every item submitted so far answers; re-answering an earlier round is harmless because
        // the batch path only reads the ids of the round it submitted.
        Ok(self
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|i| crate::llm::BatchResultRow {
                custom_id: i.custom_id.clone(),
                response: Some(batch_response()),
                error: None,
                billed: None,
            })
            .collect())
    }
}

/// The prompt text of a batch item: its content is a string, or blocks split at cache breakpoints.
fn batch_prompt(item: &crate::llm::BatchItem) -> String {
    let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
    match content {
        serde_json::Value::String(s) => s,
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
        other => panic!("unexpected batch content: {other}"),
    }
}

/// Run the routed BATCH passes (production path below `audit_repo`) and return every prompt.
async fn run_batch(
    rules: &[AuditRule],
    files: &[(String, String)],
    prefilter: bool,
    env: CandidateEnv,
) -> Vec<String> {
    let api = PromptBatch {
        realtime: Recorder::default(),
        items: std::sync::Mutex::new(Vec::new()),
    };
    let m = UsageMeter::with_controls(RunControls {
        efficiency: efficiency(prefilter),
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    });
    let plan = plan_routes(rules, files, true);
    let repo_map = build_repo_map(files, &run_nonce());
    let adopted = rules.iter().map(|r| r.id.to_ascii_uppercase()).collect();
    let mut skipped = Vec::new();
    let (_f, _p, _r, _ok, failures) = run_routed_passes_batch(
        &api,
        REPO,
        files,
        rules,
        &plan,
        &repo_map,
        &run_nonce(),
        &adopted,
        None,
        None,
        RULE_BATCH_SIZE,
        Some(&m),
        &mut skipped,
        env,
    )
    .await
    .expect("the batch path runs");
    assert!(failures.is_empty(), "{failures:?}");
    assert!(skipped.is_empty(), "{skipped:?}");
    let items = api.items.lock().unwrap();
    items.iter().map(batch_prompt).collect()
}

#[tokio::test(start_paused = true)]
async fn batch_mode_sends_exactly_the_prompts_real_time_sends() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    for (name, files) in [
        ("one chunk", supabase_files(true)),
        ("two chunks, narrowed then whole", two_chunk_files()),
    ] {
        let (real_time, _) = run(&rules, &files, true).await;
        let batch = run_batch(&rules, &files, true, CandidateEnv::production()).await;
        assert!(
            batch.iter().any(|p| p.contains("CANDIDATE SITES")),
            "{name}: batch mode narrows too"
        );
        assert_eq!(
            sorted(batch),
            sorted(real_time),
            "{name}: batch and real-time prompts differ"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn batch_mode_falls_back_to_whole_files_exactly_as_real_time_does() {
    let rules = vec![plain("ARCH-1"), fn_rule()];
    let files = supabase_files(true);
    let env = env_with(failing, Duration::from_secs(5));
    let (real_time, _) = run_with(&rules, &files, true, env).await;
    let batch = run_batch(&rules, &files, true, env).await;
    let off = run_batch(&rules, &files, false, CandidateEnv::production()).await;
    assert!(batch.iter().all(|p| !p.contains("CANDIDATE SITES")));
    assert_eq!(sorted(batch.clone()), sorted(real_time));
    assert_eq!(
        sorted(batch),
        sorted(off),
        "a failed locator = the switch-off batch"
    );
}
