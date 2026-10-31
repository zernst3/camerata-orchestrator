//! Rank 7: `applies_to` routing. Which (chunk, rule) pairs does a scan send, with the switch on
//! and off, for rules with and without a declared scope? No network: the model is a stub that
//! records every prompt, and the pairs are read back out of what it was sent.

use std::collections::BTreeSet;
use std::sync::Mutex;

use super::*;
use crate::cost_cap::RunControls;
use crate::efficiency::EfficiencyConfig;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::scan_routing::{plan_routes, AuditRule};
use camerata_rules::AppliesTo;

const REPO: &str = "acme/widgets";

/// Answers `{}` to everything and keeps every prompt it was sent.
#[derive(Default)]
pub(super) struct Recorder {
    pub(super) prompts: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LlmPort for Recorder {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.prompts.lock().unwrap().push(req.prompt.clone());
        Ok(LlmResponse {
            text: "{}".to_string(),
            model: "stub".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(10),
            output_tokens: Some(1),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        on_delta("{}");
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn file(path: &str) -> (String, String) {
    (path.to_string(), format!("// body of {path}\n"))
}

/// A polyglot repo: SQL migration, TSX, Rust, a markdown file, an extensionless file. In
/// path order, the canonical order `audit_repo` puts its files in (H5) before it plans routes,
/// so a plan made here from these files is the plan the audit makes.
fn files() -> Vec<(String, String)> {
    let mut files = vec![
        file("supabase/migrations/1.sql"),
        file("src/app.tsx"),
        file("src/main.rs"),
        file("README.md"),
        file("Makefile"),
    ];
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn scoped(id: &str, paths: &[&str], languages: &[&str]) -> AuditRule {
    scoped_with(id, paths, languages, &[])
}

fn scoped_with(id: &str, paths: &[&str], languages: &[&str], contains: &[&str]) -> AuditRule {
    AuditRule::new(id, format!("directive of {id}")).with_applies_to(Some(
        AppliesTo::new(
            paths.iter().map(|p| p.to_string()).collect(),
            languages.iter().map(|l| l.to_string()).collect(),
            contains.iter().map(|m| m.to_string()).collect(),
        )
        .unwrap(),
    ))
}

fn plain(id: &str) -> AuditRule {
    AuditRule::new(id, format!("directive of {id}"))
}

fn efficiency(applies_to_routing: bool) -> EfficiencyConfig {
    let mut config = EfficiencyConfig::default();
    config
        .set("applies_to_routing", applies_to_routing)
        .unwrap();
    config
}

fn meter(applies_to_routing: bool) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        efficiency: efficiency(applies_to_routing),
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    })
}

/// The `(chunk's file paths, rule id)` pairs in the scan prompts the stub received.
fn sent_pairs(rec: &Recorder) -> BTreeSet<(Vec<String>, String)> {
    pairs_from_prompts(&rec.prompts.lock().unwrap())
}

/// [`sent_pairs`] for prompts however they were collected (a recorder, a fake batch API).
fn pairs_from_prompts(prompts: &[String]) -> BTreeSet<(Vec<String>, String)> {
    let mut out = BTreeSet::new();
    for prompt in prompts {
        // A file's frame opens with `<<<UNTRUSTED <token> "<path>">>>`; the repo map's frame is
        // labelled "repo map" and lists every file, so it is not a chunk member.
        let paths: Vec<String> = prompt
            .lines()
            .filter(|l| l.starts_with("<<<UNTRUSTED "))
            .filter_map(|l| {
                let label = l.split_once(" \"")?.1.strip_suffix("\">>>")?;
                (label != "repo map").then(|| label.to_string())
            })
            .collect();
        if paths.is_empty() {
            continue;
        }
        for line in prompt.lines() {
            if let Some(rest) = line.strip_prefix("- [") {
                if let Some((id, _)) = rest.split_once("] ") {
                    out.insert((paths.clone(), id.to_string()));
                }
            }
        }
    }
    out
}

/// The pairs flattened to `(file, rule id)`: which rule saw which file.
fn rule_file_pairs(pairs: &BTreeSet<(Vec<String>, String)>) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .flat_map(|(paths, id)| paths.iter().map(move |p| (p.clone(), id.clone())))
        .collect()
}

async fn run(rules: &[AuditRule], applies_to_routing: bool) -> (Recorder, UsageMeter) {
    let rec = Recorder::default();
    let m = meter(applies_to_routing);
    let (_f, _p, _r, failed, _s) = audit_repo(
        &rec,
        REPO,
        &files(),
        rules,
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        Some(&m),
        None,
    )
    .await
    .expect("the stub model answers every pass");
    assert!(failed.is_empty(), "{failed:?}");
    (rec, m)
}

fn has(pairs: &BTreeSet<(String, String)>, file: &str, rule: &str) -> bool {
    pairs.contains(&(file.to_string(), rule.to_string()))
}

#[tokio::test]
async fn a_rule_without_applies_to_routes_exactly_as_before() {
    let rules = vec![plain("ARCH-1"), plain("RUST-X-1")];
    let (on, _) = run(&rules, true).await;
    let (off, _) = run(&rules, false).await;
    assert_eq!(
        sent_pairs(&on),
        sent_pairs(&off),
        "no rule declares a scope, so the switch changes nothing"
    );
    let pairs = rule_file_pairs(&sent_pairs(&on));
    for f in files() {
        assert!(
            has(&pairs, &f.0, "ARCH-1"),
            "ARCH-1 sees every file: {}",
            f.0
        );
    }
    assert!(has(&pairs, "src/main.rs", "RUST-X-1"));
    assert!(
        !has(&pairs, "src/app.tsx", "RUST-X-1"),
        "the id-prefix language routing is unchanged"
    );
}

#[tokio::test]
async fn a_declared_scope_sends_the_rule_only_with_matching_files_when_the_switch_is_on() {
    let rules = vec![plain("ARCH-1"), scoped("SUPA-RLS-1", &["**/*.sql"], &[])];
    let (rec, _) = run(&rules, true).await;
    let pairs = rule_file_pairs(&sent_pairs(&rec));
    assert!(has(&pairs, "supabase/migrations/1.sql", "SUPA-RLS-1"));
    for other in ["src/app.tsx", "src/main.rs", "README.md", "Makefile"] {
        assert!(
            !has(&pairs, other, "SUPA-RLS-1"),
            "SUPA-RLS-1 must not be sent with {other}"
        );
        assert!(has(&pairs, other, "ARCH-1"), "ARCH-1 is unscoped");
    }
}

#[tokio::test]
async fn switch_off_ignores_applies_to_entirely() {
    let with_scope = vec![plain("ARCH-1"), scoped("SUPA-RLS-1", &["**/*.sql"], &[])];
    let without_scope = vec![plain("ARCH-1"), plain("SUPA-RLS-1")];
    let (off, _) = run(&with_scope, false).await;
    let (baseline, _) = run(&without_scope, false).await;
    assert_eq!(
        sent_pairs(&off),
        sent_pairs(&baseline),
        "switch off: a declared scope behaves as if the rule declared none"
    );
    let pairs = rule_file_pairs(&sent_pairs(&off));
    assert!(has(&pairs, "src/main.rs", "SUPA-RLS-1"));
    // The plan itself has no declared group either.
    let plan = plan_routes(&with_scope, &files(), false);
    assert!(plan
        .groups
        .iter()
        .all(|g| !matches!(g.scope, crate::scan_routing::Scope::Declared { .. })));
}

#[tokio::test]
async fn an_unknown_language_chunk_still_gets_a_language_scoped_rule() {
    // `languages = ["rust"]`: the .tsx file is known and not Rust (out); the .rs file is in; the
    // markdown and extensionless files have no recognized language, so the rule is SENT.
    let rules = vec![scoped("LANG-ONLY-1", &[], &["rust"])];
    let (rec, _) = run(&rules, true).await;
    let pairs = rule_file_pairs(&sent_pairs(&rec));
    assert!(has(&pairs, "src/main.rs", "LANG-ONLY-1"));
    assert!(
        has(&pairs, "Makefile", "LANG-ONLY-1"),
        "an extensionless file has an unknown language: the rule is sent"
    );
    assert!(
        has(&pairs, "README.md", "LANG-ONLY-1"),
        "an unrecognized extension is an unknown language: the rule is sent"
    );
    assert!(
        !has(&pairs, "src/app.tsx", "LANG-ONLY-1"),
        "a file known to be another language is not sent a Rust-only rule"
    );
}

#[tokio::test]
async fn real_time_sends_the_pairs_the_shared_route_plan_names_and_batch_reads_the_same_plan() {
    // Real-time (`run_routed_passes`) and batch (`run_routed_passes_batch`) both take their
    // work from `runnable_units`; `routed_pair_set` is that product. This pins the real-time
    // half: what it actually sent is exactly the planned set. The batch half is driven through
    // `BatchSeam` and a fake Message Batches API (`batch_default_tests::FakeBatch`) by the
    // `batch_mode_*` tests below, which read the submitted items back the same way.
    let rules = vec![
        plain("ARCH-1"),
        plain("RUST-X-1"),
        scoped("SUPA-RLS-1", &["**/*.sql"], &[]),
        scoped("LANG-ONLY-1", &[], &["rust"]),
    ];
    for on in [true, false] {
        let plan = plan_routes(&rules, &files(), on);
        let planned = pair_set_of(&runnable_units(&files(), &plan, None, &mut Vec::new()));
        let (rec, _) = run(&rules, on).await;
        assert_eq!(
            sent_pairs(&rec),
            planned,
            "applies_to_routing={on}: real-time sent something other than the plan"
        );
    }
}

#[test]
fn a_scoped_group_that_matches_no_file_is_reported_not_assessed_not_dropped() {
    let only_rust = vec![file("src/main.rs")];
    let rules = vec![plain("ARCH-1"), scoped("SUPA-RLS-1", &["**/*.sql"], &[])];
    let plan = plan_routes(&rules, &only_rust, true);
    let mut skipped = Vec::new();
    let units = runnable_units(&only_rust, &plan, None, &mut skipped);
    assert_eq!(units.len(), 1, "only the ARCH-1 group has files to run");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, "SUPA-RLS-1");
    assert!(skipped[0].1.contains("applies to"), "{}", skipped[0].1);
}

#[test]
fn a_scoped_group_has_advisory_disabled_only_when_an_all_group_also_runs() {
    let with_all = plan_routes(
        &[plain("ARCH-1"), scoped("SUPA-RLS-1", &["**/*.sql"], &[])],
        &files(),
        true,
    );
    let units = routed_units(&files(), &with_all);
    for u in &units {
        assert_eq!(
            u.advisory_disabled,
            u.label.as_str() != "pass[all]",
            "{}: only the All group runs the novel-issue pass",
            u.label
        );
    }
    let no_all = plan_routes(&[scoped("SUPA-RLS-1", &["**/*.sql"], &[])], &files(), true);
    assert!(
        routed_units(&files(), &no_all)
            .iter()
            .all(|u| !u.advisory_disabled),
        "with no All group the scoped group must keep the novel-issue pass"
    );
}

#[test]
fn routing_records_how_many_pairs_it_skipped() {
    // Three big TSX files (three chunks of the whole repo), then a small Rust file and a small
    // SQL file in the last chunk. RUST-X-1 is routed by its id prefix whatever the switch says;
    // SUPA-RLS-1 is routed by its declared scope only when the switch is on.
    let big = |p: &str| (p.to_string(), "x".repeat(200_000));
    let files = vec![
        big("src/a.tsx"),
        big("src/b.tsx"),
        big("src/c.tsx"),
        file("src/main.rs"),
        file("db/1.sql"),
    ];
    let rules = vec![
        plain("ARCH-1"),
        plain("RUST-X-1"),
        scoped("SUPA-RLS-1", &["**/*.sql"], &[]),
    ];
    let counts = |on: bool| {
        let plan = plan_routes(&rules, &files, on);
        let m = meter(on);
        let mut skipped = Vec::new();
        runnable_units(&files, &plan, Some(&m), &mut skipped);
        m.stage_telemetry(0).routing.expect("routing recorded")
    };
    let on = counts(true);
    let off = counts(false);
    assert_eq!(on.pairs_total, 9, "3 chunks x 3 rules");
    assert_eq!(on.pairs_total, off.pairs_total, "same scan, same total");
    assert_eq!(
        off.pairs_skipped, 2,
        "switch off: only the id-prefix rule routes (1 of 3 chunks)"
    );
    assert_eq!(
        on.pairs_skipped, 4,
        "switch on: the SQL rule also goes to 1 of 3 chunks"
    );
}

#[test]
fn batch_sends_the_real_time_pairs_when_on_and_every_pair_when_off() {
    let rules = vec![
        plain("ARCH-1"),
        plain("RUST-X-1"),
        scoped("SUPA-RLS-1", &["**/*.sql"], &[]),
        scoped_with("SQL-EMBED-1", &[], &[], &["create policy"]),
    ];
    let mut files = files();
    files.push((
        "src/data/setup.ts".to_string(),
        "await q('CREATE POLICY p ON t USING (true)')".to_string(),
    ));

    // ON: the batch units ARE the real-time units.
    let plan = plan_routes(&rules, &files, true);
    let real_time = pair_set_of(&runnable_units(&files, &plan, None, &mut Vec::new()));
    let batch = pair_set_of(&batch_units(
        &files,
        &rules,
        &plan,
        true,
        None,
        &mut Vec::new(),
    ));
    assert_eq!(
        batch, real_time,
        "ON: batch and real-time send the same pairs"
    );
    let flat = rule_file_pairs(&batch);
    assert!(
        has(&flat, "src/data/setup.ts", "SQL-EMBED-1"),
        "content marker"
    );
    assert!(!has(&flat, "src/app.tsx", "SUPA-RLS-1"));
    assert!(!has(&flat, "src/app.tsx", "RUST-X-1"));

    // OFF: one unrouted submission, every rule with every file, advisory on, label `pass`.
    let plan_off = plan_routes(&rules, &files, false);
    let units = batch_units(&files, &rules, &plan_off, false, None, &mut Vec::new());
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].label, "pass");
    assert!(!units[0].advisory_disabled);
    assert_eq!(units[0].files.len(), files.len());
    let off = pair_set_of(&units);
    let all_chunks: Vec<Vec<String>> = chunk_files(&files, CHUNK_DIGEST_CHARS)
        .iter()
        .map(|c| c.iter().map(|(p, _)| p.clone()).collect())
        .collect();
    let expected: std::collections::BTreeSet<(Vec<String>, String)> = all_chunks
        .iter()
        .flat_map(|paths| rules.iter().map(move |r| (paths.clone(), r.id.clone())))
        .collect();
    assert_eq!(
        off, expected,
        "OFF: batch sends every rule with every chunk"
    );
}

#[tokio::test]
async fn an_incremental_scan_does_not_report_a_rule_not_assessed_when_the_full_repo_has_a_match() {
    use camerata_api_types::audit::FindingKind;
    let changed = vec![file("src/main.rs")];
    let full = files(); // includes the .sql migration
    let rules = vec![plain("ARCH-1"), scoped("SUPA-RLS-1", &["**/*.sql"], &[])];
    let not_assessed = |findings: &[Finding]| {
        findings
            .iter()
            .filter(|f| matches!(f.kind, Some(FindingKind::NotAssessed(_))))
            .map(|f| f.rule_id.clone())
            .collect::<Vec<_>>()
    };
    let go = |map_files: Option<Vec<(String, String)>>| {
        let changed = changed.clone();
        let rules = rules.clone();
        async move {
            let rec = Recorder::default();
            let m = meter(true);
            let (findings, ..) = audit_repo(
                &rec,
                REPO,
                &changed,
                &rules,
                &[],
                &std::collections::HashMap::new(),
                None,
                None,
                ScanMode::Parallel,
                false,
                None,
                None,
                Some(&m),
                map_files.as_deref(),
            )
            .await
            .expect("stub answers");
            findings
        }
    };
    assert_eq!(
        not_assessed(&go(None).await),
        vec!["SUPA-RLS-1".to_string()],
        "a full scan with no SQL anywhere: reported"
    );
    assert!(
        not_assessed(&go(Some(full)).await).is_empty(),
        "the repo has SQL (unchanged): not reported"
    );
    let no_sql = vec![file("src/main.rs"), file("README.md")];
    assert_eq!(
        not_assessed(&go(Some(no_sql)).await),
        vec!["SUPA-RLS-1".to_string()],
        "no SQL in the full set either: reported"
    );
}

#[test]
fn the_switch_is_recorded_in_the_run_line() {
    assert!(EfficiencyConfig::default()
        .render()
        .contains("applies_to_routing=on"));
    assert!(efficiency(false)
        .render()
        .contains("applies_to_routing=off"));
}

// ── batch mode through the fake Message Batches API ─────────────────────────────────────────

use super::batch_default_tests::{FakeBatch, Reply};

/// `audit_repo` in `ScanMode::Batch` over [`files`], the model seam being a [`BatchSeam`] around
/// a fake Message Batches API that answers by `script` (submission round, custom id).
async fn run_batch(
    rules: &[AuditRule],
    applies_to_routing: bool,
    script: impl Fn(usize, &str) -> Reply + Send + Sync + 'static,
) -> (std::sync::Arc<FakeBatch>, Vec<FailedPass>) {
    // The provider keeps failing, so the end-of-run second try of an errored item fails as well
    // and the item stays a failed pass, as these tests expect.
    let api = std::sync::Arc::new(FakeBatch::new(script).with_realtime_failing());
    let seam = BatchSeam::new(api.clone());
    let m = meter(applies_to_routing);
    let (_f, _p, _r, failed, _s) = audit_repo(
        &seam,
        REPO,
        &files(),
        rules,
        &[],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Batch,
        false,
        None,
        None,
        Some(&m),
        None,
    )
    .await
    .expect("the fake batch API answers every item");
    (api, failed)
}

/// Every submitted item's prompt, over every submission.
fn submitted_prompts(api: &FakeBatch) -> Vec<String> {
    api.submitted_items()
        .into_iter()
        .flatten()
        .map(|(_, _, prompt)| prompt)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn batch_mode_submits_the_planned_pairs_with_routing_on_and_one_unrouted_unit_off() {
    let rules = vec![
        plain("ARCH-1"),
        plain("RUST-X-1"),
        scoped("SUPA-RLS-1", &["**/*.sql"], &[]),
        scoped("LANG-ONLY-1", &[], &["rust"]),
    ];

    // ON: exactly the (chunk paths, rule id) pairs `runnable_units` names, one submission per
    // route group.
    let plan = plan_routes(&rules, &files(), true);
    let planned = pair_set_of(&runnable_units(&files(), &plan, None, &mut Vec::new()));
    let (api, failed) = run_batch(&rules, true, |_, _| Reply::Empty).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(pairs_from_prompts(&submitted_prompts(&api)), planned);
    assert_eq!(
        api.submitted_items().len(),
        runnable_units(&files(), &plan, None, &mut Vec::new()).len(),
        "each route group is its own Message Batch"
    );
    let flat = rule_file_pairs(&planned);
    assert!(
        !has(&flat, "src/app.tsx", "SUPA-RLS-1"),
        "the routed plan really is narrower than the unrouted one"
    );

    // OFF: the single unrouted unit, every rule with every chunk, in ONE submission.
    let plan_off = plan_routes(&rules, &files(), false);
    let units = batch_units(&files(), &rules, &plan_off, false, None, &mut Vec::new());
    assert_eq!(units.len(), 1);
    let every_pair: BTreeSet<(Vec<String>, String)> = chunk_files(&files(), CHUNK_DIGEST_CHARS)
        .iter()
        .flat_map(|c| {
            let paths: Vec<String> = c.iter().map(|(p, _)| p.clone()).collect();
            rules.iter().map(move |r| (paths.clone(), r.id.clone()))
        })
        .collect();
    let (api, failed) = run_batch(&rules, false, |_, _| Reply::Empty).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(api.submitted_items().len(), 1, "one unrouted submission");
    let sent = pairs_from_prompts(&submitted_prompts(&api));
    assert_eq!(sent, pair_set_of(&units));
    assert_eq!(sent, every_pair, "OFF: every rule with every chunk");
}

/// Three groups, in plan order: the `rust` language group, the declared-scope group, and the
/// cross-cutting `All` group last.
fn three_group_rules() -> Vec<AuditRule> {
    vec![
        plain("ARCH-1"),
        plain("RUST-X-1"),
        scoped("SUPA-RLS-1", &["**/*.sql"], &[]),
    ]
}

#[tokio::test(start_paused = true)]
async fn an_errored_item_in_the_second_group_is_a_failed_pass_naming_that_group() {
    let rules = three_group_rules();
    let units = routed_units(&files(), &plan_routes(&rules, &files(), true));
    assert_eq!(units.len(), 3);
    let second = &units[1];
    // Submission round 0 is group 1, rounds 1 and 2 are group 2's first submission and its one
    // resubmission, round 3 is group 3.
    let (api, failed) = run_batch(&rules, true, |round, id| {
        if (1..=2).contains(&round) && id == "c0-b0" {
            Reply::Errored
        } else {
            Reply::Empty
        }
    })
    .await;
    assert_eq!(api.submitted_items().len(), 4, "3 groups + 1 resubmission");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].pass.contains(&second.label),
        "the row names the second group {}: {}",
        second.label,
        failed[0].pass
    );
    for other in [&units[0], &units[2]] {
        assert!(
            !failed[0].pass.contains(&other.label),
            "and not {}: {}",
            other.label,
            failed[0].pass
        );
    }
    assert!(
        failed[0]
            .reason
            .contains("resubmitted once and failed again"),
        "{}",
        failed[0].reason
    );
}

#[tokio::test(start_paused = true)]
async fn advisory_disabled_reaches_the_task_line_of_every_batch_group() {
    let rules = three_group_rules();
    let units = routed_units(&files(), &plan_routes(&rules, &files(), true));
    assert!(
        units[0].advisory_disabled && units[1].advisory_disabled && !units[2].advisory_disabled,
        "only the All group runs the novel-issue pass"
    );
    let (api, failed) = run_batch(&rules, true, |_, _| Reply::Empty).await;
    assert!(failed.is_empty(), "{failed:?}");
    let submissions = api.submitted_items();
    assert_eq!(submissions.len(), 3);
    for (unit, items) in units.iter().zip(&submissions) {
        for (_, _, prompt) in items {
            assert!(
                prompt.contains(&pass_task_line(!unit.advisory_disabled, 0, 1, true)),
                "{}: the task line follows advisory_disabled={}: {prompt}",
                unit.label,
                unit.advisory_disabled
            );
            assert_eq!(
                prompt.contains("ALSO flag any other genuine issues"),
                !unit.advisory_disabled,
                "{}",
                unit.label
            );
        }
    }
}

// ── batch_default off: a later group's failure keeps the earlier groups' paid replies ───────

/// `run_routed_passes_batch` over [`three_group_rules`] with `batch_default` OFF (where a failed
/// submit is fatal to its `run_passes_batch` call). Routing is on or off as asked.
async fn run_routed_batch_default_off(
    api: &FakeBatch,
    applies_to_routing: bool,
) -> anyhow::Result<PassesOut> {
    let mut config = efficiency(applies_to_routing);
    config.batch_default = false;
    let m = UsageMeter::with_controls(RunControls {
        efficiency: config,
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    });
    let rules = three_group_rules();
    let plan = plan_routes(&rules, &files(), applies_to_routing);
    run_routed_passes_batch(
        api,
        REPO,
        &files(),
        &rules,
        &plan,
        "",
        &crate::prompt_frame::FrameNonce::generate(),
        &std::collections::HashSet::new(),
        None,
        None,
        15,
        Some(&m),
        &mut Vec::new(),
        super::candidate_passes::CandidateEnv::production(),
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_later_groups_batch_failure_keeps_the_earlier_groups_paid_replies() {
    let units = routed_units(&files(), &plan_routes(&three_group_rules(), &files(), true));
    // Group 2's submission is refused; groups 1 and 3 answer with one finding each.
    let api = FakeBatch::new(|_, _| Reply::Ok).failing_submit_on(1);
    let (findings, _proposed, _requested, ok, failures) = run_routed_batch_default_off(&api, true)
        .await
        .expect("one group failing must not discard the others");
    assert_eq!(ok, 2, "groups 1 and 3 answered");
    assert_eq!(
        findings.len(),
        2,
        "the completed groups' findings are kept: {findings:?}"
    );
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].pass.contains(&units[1].label),
        "the failed row names group 2 ({}): {}",
        units[1].label,
        failures[0].pass
    );
    assert!(
        failures[0].error.contains("submission refused"),
        "and carries the real error: {}",
        failures[0].error
    );
}

#[tokio::test(start_paused = true)]
async fn when_no_group_completes_the_error_still_propagates() {
    // Routing off: one unrouted group, so its failure is every group's failure. Nothing was
    // paid for, so there is nothing to keep, and the caller gets the error as before.
    let api = FakeBatch::new(|_, _| Reply::Ok).failing_submit_on(0);
    let err = run_routed_batch_default_off(&api, false)
        .await
        .expect_err("no group completed");
    assert!(format!("{err:#}").contains("submission refused"), "{err:#}");
}
