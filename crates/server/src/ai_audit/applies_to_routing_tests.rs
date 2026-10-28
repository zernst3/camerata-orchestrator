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

/// A polyglot repo: SQL migration, TSX, Rust, a markdown file, an extensionless file.
fn files() -> Vec<(String, String)> {
    vec![
        file("supabase/migrations/1.sql"),
        file("src/app.tsx"),
        file("src/main.rs"),
        file("README.md"),
        file("Makefile"),
    ]
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
    })
}

/// The `(chunk's file paths, rule id)` pairs in the scan prompts the stub received.
fn sent_pairs(rec: &Recorder) -> BTreeSet<(Vec<String>, String)> {
    let mut out = BTreeSet::new();
    for prompt in rec.prompts.lock().unwrap().iter() {
        let paths: Vec<String> = prompt
            .lines()
            .filter_map(|l| l.strip_prefix("// ===== FILE: "))
            .filter_map(|l| l.strip_suffix(" ====="))
            .map(str::to_string)
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
    // work from `runnable_units`; `routed_pair_set` is that product. The batch path itself needs
    // the live Message Batches API, so this pins the half that can run offline: what real-time
    // actually sent is exactly the planned set.
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
