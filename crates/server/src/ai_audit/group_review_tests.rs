//! Stage 2, the group reviewer (review tree design 4.2 to 4.4, 4.8 items 4 to 12, A5, A9, A13).
//!
//! Every test drives the real stage through a scripted model: no network and no spend. The
//! scripted reviewer reads the rule node and the site indexes out of the prompt it is sent and
//! answers with whatever the test wants the group reviewer to say.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use camerata_api_types::review::{EscalationReason, EscalationState, OutlierReason};

use super::group_review::*;
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::onboard::ScanReport;
use crate::review_tree::{NodeLevel, OutlierBy, RankSettings, ReviewTree, TreeNode};

const SQL: &str = "SEC-NO-RAW-SQL-CONCAT-1";
const SECRETS: &str = "SEC-NO-HARDCODED-SECRETS-1";
const LAYERING: &str = "ARCH-STRICT-LAYERING-1";

// ── The scripted group reviewer ────────────────────────────────────────────────────────────

/// What one call asked.
#[derive(Debug, Clone)]
struct Call {
    /// The rule node the call is about (`rule:SEC-NO-RAW-SQL-CONCAT-1`).
    node_id: String,
    /// The site indexes the call listed, in order.
    indices: Vec<usize>,
    /// The rule-level summary call made after a multi-chunk rule.
    summary_call: bool,
    /// Calls made so far for this node and kind, this one included (1 on the first).
    attempt: usize,
    prompt: String,
}

type Handler = Box<dyn Fn(&Call) -> anyhow::Result<String> + Send + Sync>;

struct Reviewer {
    handler: Handler,
    seen: Mutex<Vec<(Call, LlmRequest)>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    delay_ms: u64,
}

impl Reviewer {
    fn new(handler: impl Fn(&Call) -> anyhow::Result<String> + Send + Sync + 'static) -> Self {
        Self {
            handler: Box::new(handler),
            seen: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            delay_ms: 0,
        }
    }

    fn slow(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }

    fn calls(&self) -> Vec<Call> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(c, _)| c.clone())
            .collect()
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }

    fn parse(&self, req: &LlmRequest) -> Call {
        let prompt = req.prompt.clone();
        let node_id = prompt
            .lines()
            .find_map(|l| l.strip_prefix("Rule group node: "))
            .unwrap_or_default()
            .trim()
            .to_string();
        let summary_call = req.response_schema == Some(reply_schemas::rule_summary_reply_schema());
        let re = regex::Regex::new(r"^(\d+): ").unwrap();
        let indices = prompt
            .lines()
            .filter_map(|l| re.captures(l).and_then(|c| c[1].parse().ok()))
            .collect();
        let attempt = self
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| c.node_id == node_id && c.summary_call == summary_call)
            .count()
            + 1;
        Call {
            node_id,
            indices,
            summary_call,
            attempt,
            prompt,
        }
    }
}

fn response(text: &str, model: &str) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        text: text.to_string(),
        model: model.to_string(),
        backend: "stub".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

#[async_trait::async_trait]
impl LlmPort for Reviewer {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let call = self.parse(&req);
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.seen.lock().unwrap().push((call.clone(), req.clone()));
        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }
        let out = (self.handler)(&call);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        out.map(|text| response(&text, &req.model))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Reply builders ─────────────────────────────────────────────────────────────────────────

fn reply(
    summary: &str,
    subgroups: &[(&str, &str, Vec<usize>)],
    outliers: &[(usize, &str, &str)],
    escalations: &[(usize, &str, u32, &str)],
    disagreements: &[(usize, &str, &str, &str, &str)],
) -> String {
    serde_json::json!({
        "summary": summary,
        "subgroups": subgroups
            .iter()
            .map(|(l, c, m)| {
                serde_json::json!({
                    "label": l, "cause": c, "summary": format!("About {l}."), "members": m
                })
            })
            .collect::<Vec<_>>(),
        "outliers": outliers
            .iter()
            .map(|(i, r, w)| serde_json::json!({"index": i, "reason": r, "why": w}))
            .collect::<Vec<_>>(),
        "escalations": escalations
            .iter()
            .map(|(i, r, k, w)| serde_json::json!({"index": i, "reason": r, "rank": k, "why": w}))
            .collect::<Vec<_>>(),
        "disagreements": disagreements
            .iter()
            .map(|(i, f, s, m, w)| serde_json::json!({
                "index": i, "field": f, "stage1": s, "mine": m, "why": w
            }))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

/// A reply that puts every index of the call in one sub-group.
fn one_group(call: &Call, summary: &str) -> String {
    reply(
        summary,
        &[("Same cause", "one shared cause", call.indices.clone())],
        &[],
        &[],
        &[],
    )
}

fn summary_reply(text: &str) -> String {
    serde_json::json!({ "summary": text }).to_string()
}

// ── Fixtures ───────────────────────────────────────────────────────────────────────────────

fn stage1_capture(real: &str, reason: &str) -> String {
    serde_json::json!({"real": real, "reason": reason}).to_string()
}

/// A site of `rule`, the `i`-th. Paths sort in `i` order, so the group reviewer's index for the
/// site is `i`.
fn site(rule: &str, i: usize, severity: &str) -> Finding {
    let mut f = Finding {
        repo: "acme/app".to_string(),
        path: format!("src/{}/f{i:04}.rs", rule.to_ascii_lowercase()),
        line: 10 + i,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("call_{rule}_{i}();"),
        detail: format!("Headline {rule} {i} \u{2014} the handler skips the check."),
        effort: Some("low".to_string()),
        confidence: Some("high".to_string()),
        ..Finding::default()
    };
    f.captures.insert(
        STAGE1_CAPTURE.to_string(),
        stage1_capture("yes", &format!("stage 1 reason {i}")),
    );
    f
}

/// A report holding `n` sites of each `(rule, n)` and a tree built over them.
fn report_of(rules: &[(&str, usize)]) -> ScanReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.code_lines = 40_000;
    for (rule, n) in rules {
        for i in 0..*n {
            report.findings.push(site(rule, i, "medium"));
        }
    }
    report.assign_site_keys();
    report
}

fn rule_node(report: &ScanReport, rule: &str) -> String {
    let f = report
        .findings
        .iter()
        .find(|f| f.rule_id == rule)
        .expect("a site of the rule");
    f.group_path[1].clone()
}

/// The sites of `rule` in index order.
fn sites_of<'a>(report: &'a ScanReport, rule: &str) -> Vec<&'a Finding> {
    let mut rows: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == rule)
        .collect();
    rows.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    rows
}

fn tree(report: &ScanReport) -> &ReviewTree {
    report.tree.as_ref().expect("the report has a tree")
}

fn node<'a>(report: &'a ScanReport, id: &str) -> &'a TreeNode {
    tree(report)
        .node(id)
        .unwrap_or_else(|| panic!("no node {id}"))
}

fn subgroups_of<'a>(report: &'a ScanReport, rule_node: &str) -> Vec<&'a TreeNode> {
    tree(report)
        .nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Subgroup && n.parent.as_deref() == Some(rule_node))
        .collect()
}

fn cfg<'a>(rules: &'a [(String, String)]) -> GroupReviewConfig<'a> {
    GroupReviewConfig {
        model: "claude-fable-5-1",
        repo_shape: "This repository has 12 code files.",
        rules,
        rank: RankSettings::default(),
        max_in_flight: 4,
        cli_backend: false,
        job: None,
    }
}

async fn run(report: &mut ScanReport, llm: &Reviewer) -> GroupReviewOutcome {
    let rules: Vec<(String, String)> = Vec::new();
    review_groups(report, llm, &cfg(&rules), None).await
}

/// Every site of the report is a member of exactly one sub-group, the sub-group its own
/// `group_path` names, and no node lists a site that is not a row.
fn assert_every_row_in_one_subgroup(report: &ScanReport) {
    let t = tree(report);
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    for n in &t.nodes {
        for key in &n.members {
            assert!(
                owner.insert(key.as_str(), n.id.as_str()).is_none(),
                "{key:?} is in two nodes"
            );
        }
    }
    let rows: BTreeSet<&str> = report
        .findings
        .iter()
        .chain(report.discarded.iter())
        .map(|f| f.site_key.as_str())
        .collect();
    assert_eq!(owner.keys().copied().collect::<BTreeSet<_>>(), rows);
    for f in report.findings.iter().chain(report.discarded.iter()) {
        assert_eq!(
            owner.get(f.site_key.as_str()).copied(),
            f.group_path.get(2).map(String::as_str),
            "{} sits where its group_path says",
            f.site_key
        );
    }
}

// ── 4.8: member accounting ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn stage2_output_places_every_member_in_exactly_one_subgroup() {
    let mut report = report_of(&[(SQL, 6), (SECRETS, 3)]);
    let before = report.findings.len();
    let llm = Reviewer::new(|call| {
        if call.node_id.contains("SQL") {
            Ok(reply(
                "Raw SQL built from request input.",
                &[
                    (
                        "Request parameters",
                        "built from a query parameter",
                        vec![0, 1, 2],
                    ),
                    ("Header values", "built from a header", vec![3, 4, 5]),
                ],
                &[],
                &[],
                &[],
            ))
        } else {
            Ok(one_group(call, "Hardcoded keys."))
        }
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(outcome.rules_reviewed, 2);
    assert_eq!(report.findings.len(), before, "no site is removed");

    let sql = rule_node(&report, SQL);
    let subs = subgroups_of(&report, &sql);
    assert_eq!(subs.len(), 2, "{subs:#?}");
    let members = sites_of(&report, SQL);
    let by_label: BTreeMap<&str, &TreeNode> = subs.iter().map(|n| (n.label.as_str(), *n)).collect();
    let params = by_label["Request parameters"];
    let headers = by_label["Header values"];
    assert_eq!(
        params.cause.as_deref(),
        Some("built from a query parameter")
    );
    assert_eq!(
        params.members,
        members[..3]
            .iter()
            .map(|f| f.site_key.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        headers.members,
        members[3..]
            .iter()
            .map(|f| f.site_key.clone())
            .collect::<Vec<_>>()
    );
    assert_every_row_in_one_subgroup(&report);
    // The sub-group a site sits in is on the site's row too.
    assert_eq!(members[0].review.subgroup_label, "Request parameters");
    assert_eq!(
        members[4].review.subgroup_cause,
        "built from a header".to_string()
    );
}

#[tokio::test]
async fn stage2_cannot_remove_a_member_and_an_unplaced_index_is_recorded() {
    let mut report = report_of(&[(SQL, 5)]);
    let before: Vec<(String, String)> = report
        .findings
        .iter()
        .map(|f| (f.site_key.clone(), f.severity.clone()))
        .collect();
    // The reviewer places 0, 1 and 3 (1 twice), forgets 2 and 4, and invents 99.
    let llm = Reviewer::new(|_| {
        Ok(reply(
            "Raw SQL.",
            &[
                ("First", "first cause", vec![0, 1]),
                ("Second", "second cause", vec![1, 3, 99]),
            ],
            &[],
            &[],
            &[],
        ))
    });
    let outcome = run(&mut report, &llm).await;

    assert_eq!(report.findings.len(), 5, "stage 2 never removes a member");
    let after: Vec<(String, String)> = report
        .findings
        .iter()
        .map(|f| (f.site_key.clone(), f.severity.clone()))
        .collect();
    assert_eq!(before, after, "and changes no site's key or severity");

    let rule = rule_node(&report, SQL);
    let unplaced_id = format!("sub:{}/unplaced", rule.strip_prefix("rule:").unwrap());
    let unplaced = node(&report, &unplaced_id);
    let members = sites_of(&report, SQL);
    let want: BTreeSet<&str> = [2usize, 4]
        .iter()
        .map(|&i| members[i].site_key.as_str())
        .collect();
    assert_eq!(
        unplaced
            .members
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        want
    );
    assert_eq!(
        outcome
            .unplaced
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        want,
        "the outcome names them"
    );
    assert!(report.ledger.escalation().is_some(), "the ledger block");
    let block = report.ledger.escalation().unwrap();
    assert_eq!(
        block
            .unplaced
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        want,
        "and the ledger records them"
    );
    // Index 1 stays where it was first placed; 99 does not exist and is counted.
    let first = subgroups_of(&report, &rule)
        .into_iter()
        .find(|n| n.label == "First")
        .unwrap();
    assert!(first.members.contains(&members[1].site_key));
    assert_eq!(outcome.stray_indexes, 1);
    assert_every_row_in_one_subgroup(&report);
}

// ── 4.2: disagreements ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_escalation_block_counts_slots_not_rows() {
    // Three nominated rows of three rules at one site and one nominated row elsewhere: two
    // candidate sites. A budget of one leaves one of them below the line. The recorded block
    // states sites beside the escalated count, which is in sites too.
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.code_lines = 1_000;
    for rule in [SQL, SECRETS, LAYERING] {
        let mut f = site(rule, 0, "medium");
        f.path = "src/shared/site.rs".to_string();
        f.line = 7;
        report.findings.push(f);
    }
    report.findings.push(site(SQL, 1, "medium"));
    report.assign_site_keys();
    let llm = Reviewer::new(|call| {
        let escalations: Vec<(usize, &str, u32, &str)> = call
            .indices
            .iter()
            .map(|&i| (i, "unauthenticated_reach", i as u32 + 1, "Reachable."))
            .collect();
        Ok(reply(
            "All.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &escalations,
            &[],
        ))
    });
    let rules: Vec<(String, String)> = Vec::new();
    let config = GroupReviewConfig {
        rank: RankSettings {
            floor: 1,
            ..RankSettings::default()
        },
        ..cfg(&rules)
    };
    review_groups(&mut report, &llm, &config, None).await;
    let block = report.ledger.escalation().expect("block");
    assert_eq!(tree(&report).candidates.len(), 4, "four rows are candidates");
    assert_eq!(block.candidates, 2, "two candidate sites");
    assert_eq!(block.escalated, 1);
    assert_eq!(block.candidates.saturating_sub(block.escalated), 1);

    // The export's re-derivation from the tree agrees with the scan-time block.
    let derived = block.derived_from_tree(tree(&report), &report.findings);
    assert_eq!(derived.candidates, block.candidates);
    assert_eq!(derived.escalated, block.escalated);
    assert_eq!(derived.over_budget, block.over_budget);
    assert_eq!(derived.candidate_rows, block.candidate_rows);
    assert_eq!(derived.escalated_rows, block.escalated_rows);
    assert_eq!((block.candidate_rows, block.escalated_rows), (4, 3));
}

#[tokio::test]
async fn stage2_disagreement_is_recorded_and_stage1_is_not_overwritten() {
    let mut report = report_of(&[(SQL, 4)]);
    let llm = Reviewer::new(|call| {
        Ok(reply(
            "Raw SQL.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &[],
            &[
                // Two notches up from medium, and the model misquotes stage 1's value.
                (
                    0,
                    "severity",
                    "low",
                    "critical",
                    "It is reachable without a login.",
                ),
                // A real disagreement.
                (1, "real", "yes", "no", "The value is a constant."),
                // One notch only.
                (
                    2,
                    "severity",
                    "medium",
                    "high",
                    "A little worse than rated.",
                ),
            ],
        ))
    });
    run(&mut report, &llm).await;
    let rows = sites_of(&report, SQL);

    // Stage 1's answers are the row's own and are not touched.
    for f in &rows {
        assert_eq!(f.severity, "medium");
        let stage1 = stage1_of(f).expect("the stage 1 record");
        assert_eq!(stage1.real, Real::Yes);
    }
    let d0 = &rows[0].review.stage2_disagreement;
    assert_eq!(d0.len(), 1);
    assert_eq!(d0[0].field.as_str(), "severity");
    assert_eq!(
        d0[0].stage1, "medium",
        "stage 1's value is read from the row"
    );
    assert_eq!(d0[0].mine, "critical");
    assert_eq!(d0[0].why, "It is reachable without a login.");
    let d1 = &rows[1].review.stage2_disagreement;
    assert_eq!(d1[0].field.as_str(), "real");
    assert_eq!(d1[0].stage1, "yes");
    assert_eq!(d1[0].mine, "no");
    assert!(rows[3].review.stage2_disagreement.is_empty());

    // Two notches, or a `real` disagreement, is the escalation reason conflicting_verdicts; one
    // notch is not.
    let t = tree(&report);
    let reason_of = |i: usize| {
        t.candidates
            .iter()
            .find(|c| c.key == rows[i].site_key)
            .map(|c| c.reason)
    };
    assert_eq!(reason_of(0), Some(EscalationReason::ConflictingVerdicts));
    assert_eq!(reason_of(1), Some(EscalationReason::ConflictingVerdicts));
    assert_eq!(reason_of(2), None);
}

#[tokio::test]
async fn a_better_reason_than_conflicting_verdicts_is_kept() {
    let mut report = report_of(&[(SQL, 2)]);
    let llm = Reviewer::new(|call| {
        Ok(reply(
            "Raw SQL.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &[(0, "unauthenticated_reach", 1, "Anyone can call it.")],
            &[(0, "real", "yes", "no", "x")],
        ))
    });
    run(&mut report, &llm).await;
    let key = &sites_of(&report, SQL)[0].site_key;
    let found = tree(&report).candidates.iter().find(|c| &c.key == key);
    assert!(found.is_some(), "the site is a candidate");
    assert_eq!(
        found.unwrap().reason,
        EscalationReason::UnauthenticatedReach
    );
}

// ── A9: summaries ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn summary_counts_come_from_tree_data_not_the_model() {
    let mut report = report_of(&[(SQL, 3)]);
    // The model states a count the data contradicts.
    let llm = Reviewer::new(|call| {
        Ok(reply(
            "There are 12 sites here, all building SQL from input.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &[],
            &[],
        ))
    });
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    let n = node(&report, &rule);

    // The count the UI shows comes from the data.
    let counts = tree(&report).counts(&rule);
    assert_eq!(counts.sites, 3);
    // The prose is kept as written, and the contradiction is a review flag.
    assert!(n.summary.as_deref().unwrap().contains("12 sites"));
    assert!(
        n.flags
            .iter()
            .any(|f| f.starts_with("summary_count_mismatch")),
        "{:?}",
        n.flags
    );
    // A true count is not flagged.
    let mut report = report_of(&[(SQL, 3)]);
    let llm = Reviewer::new(|call| {
        Ok(reply(
            "All 3 sites build SQL from input.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &[],
            &[],
        ))
    });
    run(&mut report, &llm).await;
    let n = node(&report, &rule_node(&report, SQL));
    assert!(n.flags.is_empty(), "{:?}", n.flags);
}

#[tokio::test]
async fn a_summary_that_omits_a_differing_member_is_flagged() {
    let mut report = report_of(&[(SQL, 5)]);
    let paths: Vec<String> = sites_of(&report, SQL)
        .iter()
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect();
    let named = paths[4].clone();
    let llm = {
        let named = named.clone();
        Reviewer::new(move |call| {
            let summary = if call.attempt == 1 {
                // Describes only the majority; sites 3 and 4 differ and are marked outliers.
                "Raw SQL built from request input.".to_string()
            } else {
                format!("Raw SQL built from input, except {named}.")
            };
            Ok(reply(
                &summary,
                &[("All", "one cause", call.indices.clone())],
                &[
                    (3, "different_object", "A different table."),
                    (4, "different_construct", "A stored procedure call."),
                ],
                &[],
                &[],
            ))
        })
    };
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    let n = node(&report, &rule);
    let rows = sites_of(&report, SQL);
    let flags: BTreeSet<&str> = n.flags.iter().map(String::as_str).collect();
    // Neither differing site is named, so both are flagged.
    assert!(flags.contains(format!("summary_omits_member:{}", rows[3].site_key).as_str()));
    assert!(flags.contains(format!("summary_omits_member:{}", rows[4].site_key).as_str()));

    // Name one of them (by path and line) and only the other stays flagged.
    let mut report = report_of(&[(SQL, 5)]);
    let llm = {
        let named = named.clone();
        Reviewer::new(move |call| {
            Ok(reply(
                &format!("Raw SQL built from input, except {named}."),
                &[("All", "one cause", call.indices.clone())],
                &[
                    (3, "different_object", "A different table."),
                    (4, "different_construct", "A stored procedure call."),
                ],
                &[],
                &[],
            ))
        })
    };
    run(&mut report, &llm).await;
    let rows = sites_of(&report, SQL);
    let n = node(&report, &rule_node(&report, SQL));
    assert_eq!(
        n.flags,
        vec![format!("summary_omits_member:{}", rows[3].site_key)]
    );
}

#[test]
fn the_summary_check_reads_counts_and_names_against_the_facts() {
    let facts = SummaryFacts {
        sites: 10,
        subgroup_sizes: vec![6, 4],
        outliers: 2,
        escalations: 3,
        files: 7,
        unplaced: 0,
        must_name: vec![("k1".to_string(), "src/a.rs".to_string(), 12, 3)],
    };
    // Counts that match a known count are fine, whatever noun carries them.
    let ok = "Ten sites in 7 files; 6 sites share one cause and the other 4 sites another. \
              See src/a.rs:12.";
    assert!(
        check_summary(ok, &facts).is_empty(),
        "{:?}",
        check_summary(ok, &facts)
    );
    // A count nothing in the tree backs is a mismatch.
    let bad = check_summary("This pattern appears in 31 sites. See src/a.rs.", &facts);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with("summary_count_mismatch"));
    // A member that must be named and is not.
    let omitted = check_summary("Six sites share one cause.", &facts);
    assert_eq!(omitted, vec!["summary_omits_member:k1".to_string()]);
    // An index reference names it too.
    assert!(check_summary("Site #3 is different.", &facts).is_empty());
}

// ── A5: every field has a home ─────────────────────────────────────────────────────────────

fn schema_field_paths(schema: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
    if let Some(props) = schema["properties"].as_object() {
        for (name, sub) in props {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            match sub["type"].as_str() {
                Some("array") => {
                    let item = &sub["items"];
                    if item["type"].as_str() == Some("object") {
                        schema_field_paths(item, &format!("{path}[]"), out);
                    } else {
                        out.insert(format!("{path}[]"));
                    }
                }
                _ => {
                    out.insert(path);
                }
            }
        }
    }
}

#[test]
fn the_reply_schema_has_no_field_without_a_home_and_is_closed() {
    let schema = reply_schemas::stage2_reply_schema();
    let mut paths = BTreeSet::new();
    schema_field_paths(&schema, "", &mut paths);
    // The array of arrays is the sub-group's member list.
    let homes: BTreeSet<String> = STAGE2_FIELD_HOMES
        .iter()
        .map(|(path, _)| (*path).to_string())
        .collect();
    assert_eq!(
        paths, homes,
        "every schema field has a home and every home is a field"
    );
    assert!(STAGE2_FIELD_HOMES.iter().all(|(_, home)| !home.is_empty()));
    // The structured-output limits hold, and an extra field fails the check.
    assert!(
        reply_schemas::check::limit_violations(&schema).is_empty(),
        "{:?}",
        reply_schemas::check::limit_violations(&schema)
    );
    let good: serde_json::Value = serde_json::from_str(&reply(
        "s",
        &[("a", "b", vec![0])],
        &[(0, "different_object", "w")],
        &[(0, "rule_misfit", 1, "w")],
        &[(0, "real", "yes", "no", "w")],
    ))
    .unwrap();
    assert_eq!(reply_schemas::check::violation(&schema, &good), None);
    let mut extra = good.clone();
    extra["confidence"] = serde_json::json!("high");
    assert!(reply_schemas::check::violation(&schema, &extra).is_some());
    let mut extra_nested = good;
    extra_nested["outliers"][0]["severity"] = serde_json::json!("high");
    assert!(reply_schemas::check::violation(&schema, &extra_nested).is_some());
    // Counts are not the model's to write.
    assert!(!paths.iter().any(|p| p.contains("count")));
}

#[tokio::test]
async fn every_stage2_reply_field_is_stored_and_rendered_in_ui_workbook_and_json() {
    // The stored and JSON halves. The UI half waits for the tree screen (WO10) and the workbook
    // columns for the exports work order (WO9); both read what is stored here.
    let mut report = report_of(&[(SQL, 4)]);
    let llm = Reviewer::new(|_| {
        Ok(reply(
            "Raw SQL built from input.",
            &[
                ("Params", "a query parameter", vec![0, 1]),
                ("Headers", "a header", vec![2, 3]),
            ],
            &[(2, "different_construct", "Calls a procedure.")],
            &[(2, "rule_misfit", 1, "Not the pattern the rule covers.")],
            &[(
                1,
                "severity",
                "medium",
                "critical",
                "Reachable without login.",
            )],
        ))
    });
    run(&mut report, &llm).await;
    let rows = sites_of(&report, SQL);
    let rule = rule_node(&report, SQL);

    // summary -> the node's summary
    assert_eq!(
        node(&report, &rule).summary.as_deref(),
        Some("Raw SQL built from input.")
    );
    // subgroups[].label, .cause, .members -> the sub-group nodes and the rows' columns
    assert_eq!(rows[0].review.subgroup_label, "Params");
    assert_eq!(rows[0].review.subgroup_cause, "a query parameter");
    assert_eq!(rows[3].review.subgroup_label, "Headers");
    // outliers[] -> the node's outlier list (by the model) and the row's columns
    let headers = subgroups_of(&report, &rule)
        .into_iter()
        .find(|n| n.label == "Headers")
        .unwrap();
    let out = headers
        .outliers
        .iter()
        .find(|o| o.by == OutlierBy::Model)
        .expect("the model's outlier");
    assert_eq!(out.key, rows[2].site_key);
    assert_eq!(out.reason, OutlierReason::DifferentConstruct);
    assert_eq!(out.why, "Calls a procedure.");
    assert_eq!(
        rows[2].review.outlier_reason,
        Some(OutlierReason::DifferentConstruct)
    );
    assert_eq!(rows[2].review.outlier_why, "Calls a procedure.");
    // escalations[] -> the row's escalation columns and the candidate
    assert_eq!(
        rows[2].review.escalation_reason,
        Some(EscalationReason::RuleMisfit)
    );
    assert_eq!(
        rows[2].review.escalation_why,
        "Not the pattern the rule covers."
    );
    assert_eq!(rows[2].review.stage2_rank, Some(1));
    assert!(rows[2].review.global_rank.is_some());
    assert_ne!(
        rows[2].review.escalation_state,
        EscalationState::NotACandidate
    );
    // disagreements[] -> the row's disagreement columns
    let d = &rows[1].review.stage2_disagreement[0];
    assert_eq!(
        (
            d.field.as_str(),
            d.stage1.as_str(),
            d.mine.as_str(),
            d.why.as_str()
        ),
        ("severity", "medium", "critical", "Reachable without login.")
    );

    // Each home is where STAGE2_FIELD_HOMES says: every home names a place.
    for (field, home) in STAGE2_FIELD_HOMES {
        assert!(!home.is_empty(), "{field} has a home");
    }

    // The JSON half: the stored report serialises every one of them, and reads back whole.
    let json = serde_json::to_value(&report).unwrap();
    let row_json = json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["site_key"] == rows[2].site_key.as_str())
        .unwrap();
    let review = &row_json["review"];
    for column in [
        "escalation_reason",
        "escalation_why",
        "stage2_rank",
        "global_rank",
        "escalation_state",
        "outlier_reason",
        "outlier_why",
        "subgroup_label",
        "subgroup_cause",
    ] {
        assert!(
            !review[column].is_null(),
            "{column} is in the JSON: {review}"
        );
    }
    let disagree_json = json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["site_key"] == rows[1].site_key.as_str())
        .unwrap();
    assert_eq!(
        disagree_json["review"]["stage2_disagreement"][0]["mine"],
        "critical"
    );
    let tree_json = &json["tree"];
    assert!(tree_json["budget"]["budget"].is_u64());
    assert!(tree_json["candidates"]
        .as_array()
        .is_some_and(|c| !c.is_empty()));
    let back: ScanReport = serde_json::from_value(json).unwrap();
    assert_eq!(
        sites_of(&back, SQL)[2].review,
        rows[2].review,
        "the columns survive a round trip"
    );
    assert_eq!(
        back.tree.as_ref().unwrap().candidates,
        tree(&report).candidates
    );
}

#[tokio::test]
async fn stage2_columns_never_reach_the_client_json() {
    let mut report = report_of(&[(SQL, 3)]);
    let llm = Reviewer::new(|call| {
        Ok(reply(
            "Raw SQL.",
            &[("All", "one cause", call.indices.clone())],
            &[(1, "different_object", "Other table.")],
            &[(1, "rule_misfit", 1, "Odd one.")],
            &[(2, "real", "yes", "no", "A constant.")],
        ))
    });
    run(&mut report, &llm).await;
    let sealed = report.seal();
    let opts = crate::report_export::ReportOptions::default();
    let report_json = crate::report_export::build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(
            &sealed,
            &std::collections::HashMap::new(),
        ),
        None,
        &opts,
    );
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(
            &sealed,
            &std::collections::HashMap::new(),
        ),
        None,
        &report_json,
        &std::collections::HashMap::new(),
    );
    let text = serde_json::to_string(&export).unwrap();
    for internal in [
        "escalation_reason",
        "escalation_why",
        "global_rank",
        "stage2_disagreement",
        "outlier_why",
        "subgroup_cause",
        "conflicting_verdicts",
        "summary_omits_member",
    ] {
        assert!(
            !text.contains(internal),
            "the client export carries {internal}"
        );
    }
}

// ── The model: A3 and A13 ──────────────────────────────────────────────────────────────────

#[test]
fn the_stage_defaults_move_together_stage1_opus_stage2_fable() {
    let scan = resolve_audit_model(None);
    assert_eq!(
        scan, "claude-sonnet-5-5",
        "the scan default is kept as it is"
    );
    assert_eq!(resolve_calibration_model(None, &scan), "claude-opus-5-5");
    assert_eq!(resolve_group_review_model(None), "claude-fable-5-1");
    // The pick wins; a blank pick is none.
    assert_eq!(resolve_calibration_model(Some("picked"), &scan), "picked");
    assert_eq!(resolve_group_review_model(Some("picked")), "picked");
    assert_eq!(resolve_group_review_model(Some("  ")), "claude-fable-5-1");
}

#[test]
#[serial_test::serial(process_env)]
fn group_review_model_comes_from_project_setting_or_flag_only() {
    let _env = [
        "CAMERATA_GROUP_REVIEW_MODEL",
        "CAMERATA_LLM_MODEL",
        "CAMERATA_CALIBRATION_MODEL",
        "ANTHROPIC_MODEL",
    ]
    .map(|name| camerata_test_env::EnvGuard::set(name, "env-picked-model"));
    assert_eq!(resolve_group_review_model(None), "claude-fable-5-1");
    assert_eq!(resolve_group_review_model(Some("flag-model")), "flag-model");
    // The project setting is the pick the cockpit passes; unset means the default.
    let settings = camerata_api_types::project::ReviewSettings::default();
    assert_eq!(
        resolve_group_review_model(Some(settings.group_review_model.as_str())),
        "claude-fable-5-1"
    );
    let set = camerata_api_types::project::ReviewSettings {
        group_review_model: "claude-opus-5-5".to_string(),
        ..settings
    };
    assert_eq!(
        resolve_group_review_model(Some(set.group_review_model.as_str())),
        "claude-opus-5-5"
    );
    // The stage reads no environment variable at all.
    let source = include_str!("group_review.rs");
    let env_read = ["env", "::var"].concat();
    assert!(
        !source.contains(&env_read),
        "stage 2 must not read the environment"
    );
}

#[tokio::test]
async fn each_calibration_stage_records_its_model_in_the_ledger() {
    let mut report = report_of(&[(SQL, 3)]);
    // Stage 1 recorded its sample when it ran (WO4).
    report
        .ledger
        .record_stage_sample(crate::scan_ledger::StageSample {
            stage: "ai-calibration".to_string(),
            rows_in: 3,
            rows_out: 3,
            accounting: crate::scan_ledger::StageAccounting::default(),
            model: Some(resolve_calibration_model(None, &resolve_audit_model(None))),
        });
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    let rules: Vec<(String, String)> = Vec::new();
    let model = resolve_group_review_model(None);
    let config = GroupReviewConfig {
        model: &model,
        ..cfg(&rules)
    };
    review_groups(&mut report, &llm, &config, None).await;

    let by_stage: BTreeMap<&str, Option<&str>> = report
        .ledger
        .stages()
        .iter()
        .map(|s| (s.stage.as_str(), s.model.as_deref()))
        .collect();
    assert_eq!(by_stage["ai-calibration"], Some("claude-opus-5-5"));
    assert_eq!(by_stage[STAGE_SAMPLE_NAME], Some("claude-fable-5-1"));
    for req in llm.requests() {
        assert_eq!(
            req.model, "claude-fable-5-1",
            "the model asked is the model recorded"
        );
    }
    // Every site is accounted for by the stage: it removes none.
    let entry = report
        .ledger
        .stages()
        .iter()
        .find(|s| s.stage == STAGE_SAMPLE_NAME)
        .unwrap();
    assert_eq!((entry.rows_in, entry.rows_out), (3, 3));
    assert!(entry.is_integral());
    assert_eq!(
        report.ledger.escalation().unwrap().model.as_deref(),
        Some("claude-fable-5-1")
    );
}

// ── 4.4: the budget and the ledger ─────────────────────────────────────────────────────────

#[tokio::test]
async fn ledger_states_budget_escalated_and_over_budget() {
    // 12 sites of critical secrets and 12 of medium layering; the budget is 5 (floor).
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.code_lines = 3_000; // ceil(3000/1000 * 1.0) = 3, under the floor of 5
    for i in 0..12 {
        report.findings.push(site(SECRETS, i, "critical"));
        report.findings.push(site(LAYERING, i, "medium"));
    }
    report.assign_site_keys();
    let llm = Reviewer::new(|call| {
        if call.node_id.contains("SECRETS") {
            // Every secret is a secret exposure.
            let escalations: Vec<(usize, &str, u32, &str)> = call
                .indices
                .iter()
                .map(|&i| (i, "secret_exposure", i as u32 + 1, "A live key."))
                .collect();
            Ok(reply(
                "Hardcoded keys.",
                &[("Keys", "a key in source", call.indices.clone())],
                &[],
                &escalations,
                &[],
            ))
        } else {
            let escalations: Vec<(usize, &str, u32, &str)> = call
                .indices
                .iter()
                .map(|&i| (i, "rule_misfit", i as u32 + 1, "Not a layering break."))
                .collect();
            Ok(reply(
                "Layering.",
                &[("Layers", "a layer skip", call.indices.clone())],
                &[],
                &escalations,
                &[],
            ))
        }
    });
    let rules: Vec<(String, String)> = Vec::new();
    let config = GroupReviewConfig {
        rank: RankSettings {
            floor: 5,
            ..RankSettings::default()
        },
        ..cfg(&rules)
    };
    review_groups(&mut report, &llm, &config, None).await;

    assert!(report.ledger.escalation().is_some(), "the escalation block");
    let block = report.ledger.escalation().unwrap();
    assert_eq!(block.budget, 5);
    // The set that was in force is on record, in priority order.
    assert_eq!(
        block.over_budget_reasons,
        EscalationReason::default_over_budget_set()
            .iter()
            .map(|r| r.as_str().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(block.counted_lines, 3_000);
    assert_eq!(block.candidates, 24);
    // The 12 critical secrets are escalated (5 inside the budget, 7 past it); the layering
    // misfits stay below the line.
    assert_eq!(block.escalated, 12);
    assert_eq!(block.over_budget, 7);
    assert_eq!(block.by_reason.get("secret_exposure"), Some(&12));
    assert_eq!(block.over_budget_by_reason.get("secret_exposure"), Some(&7));
    assert!((block.share_escalated - 12.0 / 24.0).abs() < 1e-9);
    assert_eq!(
        block.sentence(),
        "Escalated 12 of a budget of 5 (7 over, all secret exposure at high or critical)"
    );
    // The tree says the same, and every over-budget candidate records its reason.
    let t = tree(&report);
    assert_eq!(t.budget.budget, 5);
    assert_eq!(t.budget.over_budget, 7);
    assert!(t
        .candidates
        .iter()
        .filter(|c| c.escalated && c.global_rank > 5)
        .all(|c| c.over_budget_reason == Some(EscalationReason::SecretExposure)));
    assert_eq!(t.candidates.len(), 24);
}

#[test]
fn the_escalation_sentence_reads_every_shape() {
    let mut block = crate::scan_ledger::EscalationBlock {
        budget: 300,
        escalated: 347,
        over_budget: 47,
        ..Default::default()
    };
    block
        .over_budget_by_reason
        .insert("unauthenticated_reach".to_string(), 30);
    block
        .over_budget_by_reason
        .insert("secret_exposure".to_string(), 17);
    assert_eq!(
        block.sentence(),
        "Escalated 347 of a budget of 300 (47 over, all unauthenticated reach at critical, and secret exposure at high or critical)"
    );
    block
        .over_budget_by_reason
        .insert("evidence_unverified".to_string(), 2);
    assert!(block.sentence().ends_with(
        "secret exposure at high or critical, and unverified evidence at any severity)"
    ));
    let within = crate::scan_ledger::EscalationBlock {
        budget: 20,
        escalated: 18,
        ..Default::default()
    };
    assert_eq!(
        within.sentence(),
        "Escalated 18 of a budget of 20 (none over)"
    );
}

#[tokio::test]
async fn changing_the_rate_after_a_review_re_marks_the_stored_candidates_with_no_model_call() {
    let mut report = report_of(&[(SQL, 8)]);
    report.code_lines = 1_000;
    let llm = Reviewer::new(|call| {
        let escalations: Vec<(usize, &str, u32, &str)> = call
            .indices
            .iter()
            .map(|&i| (i, "outlier_in_group", i as u32 + 1, "Odd."))
            .collect();
        Ok(reply(
            "Raw SQL.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &escalations,
            &[],
        ))
    });
    let rules: Vec<(String, String)> = Vec::new();
    let config = GroupReviewConfig {
        rank: RankSettings {
            floor: 2,
            ..RankSettings::default()
        },
        ..cfg(&rules)
    };
    review_groups(&mut report, &llm, &config, None).await;
    let calls_before = llm.calls().len();
    assert_eq!(tree(&report).budget.escalated, 2);

    let mut t = report.tree.take().unwrap();
    t.re_mark(
        &RankSettings {
            floor: 6,
            counted_lines: report.code_lines,
            ..RankSettings::default()
        },
        &mut report.findings,
    );
    assert_eq!(t.budget.escalated, 6);
    assert_eq!(llm.calls().len(), calls_before, "no model call");
    let escalated = report
        .findings
        .iter()
        .filter(|f| f.review.escalation_state == EscalationState::Escalated)
        .count();
    assert_eq!(escalated, 6, "the rows' columns follow the line");
}

// ── 4.2: chunks and the runner ─────────────────────────────────────────────────────────────

#[test]
fn a_big_node_splits_along_the_hints_into_chunks_of_at_most_150() {
    // Hint sub-groups stay whole when they fit.
    let plan = chunk_plan(
        &[vec![0; 100], vec![0; 100], vec![0; 100]],
        MAX_CHUNK_MEMBERS,
    );
    assert_eq!(
        plan.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![100, 100, 100]
    );
    // Small ones pack together, a whole sub-group at a time.
    let plan = chunk_plan(&[vec![0; 120], vec![0; 120], vec![0; 5]], MAX_CHUNK_MEMBERS);
    assert_eq!(
        plan.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![120, 125]
    );
    // A sub-group bigger than a chunk is cut in order.
    let plan = chunk_plan(&[vec![0; 200]], MAX_CHUNK_MEMBERS);
    assert_eq!(plan.iter().map(Vec::len).collect::<Vec<_>>(), vec![150, 50]);
    // Every member is in exactly one chunk, in order.
    let hints: Vec<Vec<usize>> = vec![(0..90).collect(), (90..260).collect(), (260..261).collect()];
    let plan = chunk_plan(&hints, MAX_CHUNK_MEMBERS);
    let flat: Vec<usize> = plan.iter().flatten().copied().collect();
    assert_eq!(flat, (0..261).collect::<Vec<_>>());
    assert!(plan
        .iter()
        .all(|c| !c.is_empty() && c.len() <= MAX_CHUNK_MEMBERS));
}

#[tokio::test]
async fn a_node_over_150_members_is_read_in_chunks_with_a_rule_level_summary_call() {
    let mut report = report_of(&[(SQL, 330)]);
    let llm = Reviewer::new(|call| {
        if call.summary_call {
            Ok(summary_reply("The whole rule: raw SQL everywhere."))
        } else {
            Ok(one_group(
                call,
                &format!("Chunk of {} sites.", call.indices.len()),
            ))
        }
    });
    let outcome = run(&mut report, &llm).await;
    let calls = llm.calls();
    let chunk_calls: Vec<&Call> = calls.iter().filter(|c| !c.summary_call).collect();
    let summary_calls: Vec<&Call> = calls.iter().filter(|c| c.summary_call).collect();
    let mut sizes: Vec<usize> = chunk_calls.iter().map(|c| c.indices.len()).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![30, 150, 150], "{sizes:?}");
    assert_eq!(summary_calls.len(), 1, "one rule-level summary call");
    // Indexes are the node's own, each in exactly one chunk.
    let all: BTreeSet<usize> = chunk_calls
        .iter()
        .flat_map(|c| c.indices.iter().copied())
        .collect();
    assert_eq!(all, (0..330).collect::<BTreeSet<_>>());
    assert_eq!(outcome.rules_reviewed, 1);
    let rule = rule_node(&report, SQL);
    assert_eq!(
        node(&report, &rule).summary.as_deref(),
        Some("The whole rule: raw SQL everywhere.")
    );
    assert_eq!(subgroups_of(&report, &rule).len(), 3);
    assert_every_row_in_one_subgroup(&report);
}

#[tokio::test]
async fn a_rule_in_one_chunk_makes_no_rule_level_summary_call() {
    let mut report = report_of(&[(SQL, 40)]);
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    run(&mut report, &llm).await;
    assert!(llm.calls().iter().all(|c| !c.summary_call));
    assert_eq!(llm.calls().len(), 1);
}

#[tokio::test]
async fn stage2_runs_through_the_shared_runner_at_bounded_concurrency() {
    let rules: Vec<(String, String)> = Vec::new();
    let mut report = report_of(&[
        (SQL, 3),
        (SECRETS, 3),
        (LAYERING, 3),
        ("ARCH-X-1", 3),
        ("ARCH-Y-1", 3),
        ("ARCH-Z-1", 3),
    ]);
    let llm = Reviewer::new(|call| Ok(one_group(call, "Fine."))).slow(20);
    let config = GroupReviewConfig {
        max_in_flight: 2,
        ..cfg(&rules)
    };
    let outcome = review_groups(&mut report, &llm, &config, None).await;
    assert_eq!(outcome.rules_reviewed, 6);
    let peak = llm.max_in_flight.load(Ordering::SeqCst);
    assert!(peak > 1, "the calls run side by side (peak {peak})");
    assert!(peak <= 2, "and never past the bound (peak {peak})");
}

#[tokio::test]
async fn a_transport_failure_is_sent_again_by_the_runner_not_dropped() {
    let mut report = report_of(&[(SQL, 3)]);
    let llm = Reviewer::new(|call| {
        if call.attempt == 1 {
            anyhow::bail!("connection reset")
        } else {
            Ok(one_group(call, "Raw SQL."))
        }
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(outcome.rules_reviewed, 1);
    assert_eq!(llm.calls().len(), 2);
    assert!(
        report.failed_passes.is_empty(),
        "{:?}",
        report.failed_passes
    );
}

// ── A7: every request is checked against the window ────────────────────────────────────────

#[tokio::test]
async fn every_stage2_request_passes_ensure_request_fits() {
    let limit_bytes = request_budget::input_limit_tokens("claude-fable-5-1", 4096)
        * repo_map::CHARS_PER_TOKEN_ESTIMATE;
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.code_lines = 40_000;
    // 100 sites whose paths are each a fortieth of the window: all together over it, 30 fit.
    let wide = "d".repeat(limit_bytes / 40);
    for i in 0..100 {
        let mut f = site(SQL, i, "medium");
        f.path = format!("{wide}/f{i:04}.rs");
        report.findings.push(f);
    }
    report.assign_site_keys();
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    let outcome = run(&mut report, &llm).await;
    let calls = llm.calls();
    assert!(
        calls.len() > 1,
        "the node was split to fit: {} calls",
        calls.len()
    );
    for req in llm.requests() {
        assert!(
            request_budget::ensure_request_fits(&req).is_ok(),
            "an oversized request was sent"
        );
    }
    let seen: BTreeSet<usize> = calls
        .iter()
        .flat_map(|c| c.indices.iter().copied())
        .collect();
    assert_eq!(
        seen,
        (0..100).collect::<BTreeSet<_>>(),
        "every site was asked about"
    );
    assert_eq!(outcome.rules_reviewed, 1);
    assert_eq!(report.findings.len(), 100);
}

#[tokio::test]
async fn a_request_that_cannot_be_split_to_fit_is_disclosed_and_never_sent() {
    let limit_bytes = request_budget::input_limit_tokens("claude-fable-5-1", 4096)
        * repo_map::CHARS_PER_TOKEN_ESTIMATE;
    let mut report = report_of(&[(SQL, 4)]);
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    // The rule's own text is over the window: no split of the sites can fit it.
    let rules = vec![(SQL.to_string(), "r".repeat(limit_bytes * 2))];
    let config = cfg(&rules);
    let outcome = review_groups(&mut report, &llm, &config, None).await;
    assert!(llm.calls().is_empty(), "nothing oversized was sent");
    assert_eq!(outcome.rules_failed, 1);
    assert_eq!(report.findings.len(), 4, "no site is removed");
    assert!(
        report
            .failed_passes
            .iter()
            .any(|p| p.pass == "group review"),
        "{:?}",
        report.failed_passes
    );
    assert_every_row_in_one_subgroup(&report);
}

// ── Replies ────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_extra_field_in_the_reply_is_refused_and_asked_again_once() {
    let mut report = report_of(&[(SQL, 3)]);
    let llm = Reviewer::new(|call| {
        if call.attempt == 1 {
            let mut v: serde_json::Value =
                serde_json::from_str(&one_group(call, "Raw SQL.")).unwrap();
            v["confidence"] = serde_json::json!("high");
            Ok(v.to_string())
        } else {
            Ok(one_group(call, "Raw SQL, second try."))
        }
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(llm.calls().len(), 2, "refused once, asked once more");
    assert_eq!(outcome.rules_reviewed, 1);
    let rule = rule_node(&report, SQL);
    assert_eq!(
        node(&report, &rule).summary.as_deref(),
        Some("Raw SQL, second try.")
    );
    // The second ask says why the first was refused.
    let second = &llm.requests()[1];
    assert!(second.prompt.contains("confidence"), "{}", second.prompt);
}

#[tokio::test]
async fn a_reason_outside_the_closed_lists_is_refused_and_a_second_refusal_is_a_failed_part() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 3)]);
    let llm = Reviewer::new(|call| {
        if call.node_id.contains("SQL") {
            Ok(reply(
                "Raw SQL.",
                &[("All", "one cause", call.indices.clone())],
                &[],
                &[(0, "because_i_said_so", 1, "no")],
                &[],
            ))
        } else {
            Ok(one_group(call, "Keys."))
        }
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(outcome.rules_reviewed, 1);
    assert_eq!(outcome.rules_failed, 1);
    assert_eq!(
        llm.calls()
            .iter()
            .filter(|c| c.node_id.contains("SQL"))
            .count(),
        2,
        "one ask and one retry"
    );
    let failed: Vec<&crate::ai_audit::FailedPass> = report
        .failed_passes
        .iter()
        .filter(|p| p.pass == "group review")
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(failed[0].reason.contains(SQL), "{}", failed[0].reason);
    // The sites of the failed rule keep the place the grouping gave them.
    assert_eq!(report.findings.len(), 6);
    assert_every_row_in_one_subgroup(&report);
    let block = report.ledger.escalation().unwrap();
    assert_eq!((block.rules_reviewed, block.rules_failed), (1, 1));
}

#[tokio::test]
async fn a_reviewer_edited_summary_is_kept_on_a_rerun() {
    let mut report = report_of(&[(SQL, 3)]);
    let rule = rule_node(&report, SQL);
    {
        let n = report
            .tree
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|n| n.id == rule)
            .unwrap();
        n.summary = Some("The architect's own words.".to_string());
        n.summary_source = crate::review_tree::TextSource::Reviewer;
    }
    let llm = Reviewer::new(|call| Ok(one_group(call, "The model's words.")));
    run(&mut report, &llm).await;
    let n = node(&report, &rule);
    assert_eq!(n.summary.as_deref(), Some("The architect's own words."));
    assert_eq!(n.summary_source, crate::review_tree::TextSource::Reviewer);
}

#[tokio::test]
async fn rows_the_engine_threw_out_are_never_sent_to_the_group_reviewer() {
    let mut report = report_of(&[(SQL, 3)]);
    let mut thrown = site(SQL, 50, "medium");
    thrown.detail = "THROWN-OUT-HEADLINE".to_string();
    crate::engine_discard::mark(&mut thrown, crate::engine_discard::DISCARD_VERDICT);
    report.discarded.push(thrown);
    report.assign_site_keys();
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    run(&mut report, &llm).await;
    for call in llm.calls() {
        assert!(!call.prompt.contains("THROWN-OUT-HEADLINE"));
        assert_eq!(call.indices.len(), 3);
    }
    assert_every_row_in_one_subgroup(&report);
}

// ── The prompt ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_system_prompt_states_the_architects_operating_model_in_plain_words() {
    let prompt = group_review_system_prompt();
    let lower = prompt.to_ascii_lowercase();
    // How the architect works.
    assert!(
        lower.contains("reviews patterns, not every site"),
        "{prompt}"
    );
    assert!(lower.contains("sees everything"), "{prompt}");
    assert!(
        lower.contains("decide what needs his attention most"),
        "{prompt}"
    );
    assert!(
        lower.contains("never remove a site") || lower.contains("cannot remove a site"),
        "{prompt}"
    );
    // The 4.2 framing.
    assert!(prompt
        .contains("You are the group reviewer, the architect's first reader of this rule group."));
    assert!(prompt.contains("You may disagree with the per-site reviewer and must say why."));
    // The closed lists are spelled out, every reason.
    for r in EscalationReason::ALL {
        assert!(prompt.contains(r.as_str()), "{} is missing", r.as_str());
    }
    for r in OutlierReason::ALL {
        assert!(prompt.contains(r.as_str()), "{} is missing", r.as_str());
    }
    // A9: exceptions are named, counts are not the model's.
    assert!(lower.contains("name every site"), "{prompt}");
    assert!(lower.contains("do not state how many"), "{prompt}");
    // Model-agnostic: it names no model and asks for no confidence.
    for word in ["claude", "opus", "sonnet", "fable", "haiku", "confidence"] {
        assert!(!lower.contains(word), "the prompt mentions {word}");
    }
}

#[tokio::test]
async fn the_user_prompt_lists_every_member_the_hints_and_the_flags() {
    let mut report = report_of(&[(SQL, 4)]);
    report.findings[1].captures.insert(
        crate::review_flags::REVIEW_FLAGS_CAPTURE.to_string(),
        r#"["contradiction"]"#.to_string(),
    );
    report.findings[2].captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found".to_string(),
    );
    report.assign_site_keys();
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    let rules = vec![(
        SQL.to_string(),
        "Never build SQL by concatenation.".to_string(),
    )];
    let config = cfg(&rules);
    review_groups(&mut report, &llm, &config, None).await;
    assert_eq!(llm.calls().len(), 1, "one call for one small rule group");
    let prompt = &llm.calls()[0].prompt;
    assert!(
        prompt.contains("Never build SQL by concatenation."),
        "{prompt}"
    );
    assert!(prompt.contains("This repository has 12 code files."));
    for i in 0..4 {
        assert!(
            prompt.contains(&format!("\n{i}: src/")),
            "member {i} is listed: {prompt}"
        );
    }
    assert!(
        prompt.contains("stage 1 reason 2"),
        "stage 1's reason is shown"
    );
    assert!(prompt.contains("real: yes"));
    assert!(prompt.contains("flags: contradiction"));
    assert!(prompt.contains("flags: evidence_unverified"));
    assert!(prompt.contains("Hint sub-groups"));
    // The client's text is framed as untrusted.
    assert!(prompt.contains("<<<UNTRUSTED"));
    let req = &llm.requests()[0];
    assert_eq!(
        req.response_schema,
        Some(reply_schemas::stage2_reply_schema())
    );
}

/// Not a check: prints the user prompt size of each node on the scripted fixture, which the work
/// order's report quotes.
#[tokio::test]
async fn stage2_prompt_size_per_node_on_the_scripted_fixture() {
    let mut report = report_of(&[
        (SQL, 12),
        (SECRETS, 40),
        (LAYERING, 150),
        ("ARCH-BIG-1", 330),
    ]);
    let llm = Reviewer::new(|call| {
        if call.summary_call {
            Ok(summary_reply("All of it."))
        } else {
            Ok(one_group(call, "Fine."))
        }
    });
    let outcome = run(&mut report, &llm).await;
    let system = group_review_system_prompt().len();
    for (node, bytes) in &outcome.prompt_bytes {
        eprintln!("stage 2 prompt size: {node}: user {bytes} bytes (+ system {system})");
    }
    for (call, req) in llm.calls().iter().zip(llm.requests()) {
        eprintln!(
            "stage 2 reply budget: {} {}: max_tokens {} ({} sites)",
            call.node_id,
            if call.summary_call {
                "rule summary"
            } else {
                "chunk"
            },
            req.max_tokens,
            call.indices.len()
        );
    }
    assert!(!outcome.prompt_bytes.is_empty());
    // A 150-member node stays far under the window.
    for (_, bytes) in &outcome.prompt_bytes {
        assert!(*bytes < 200_000, "{bytes}");
    }
}

// ── The spend limit and the run's usage ────────────────────────────────────────────────────

fn priced_reply(text: String, cost: f64) -> LlmResponse {
    let mut r = response(&text, "claude-fable-5-1");
    r.cost_usd = Some(cost);
    r
}

#[tokio::test]
async fn the_spend_limit_counts_what_the_scan_already_spent_and_stops_stage_2() {
    let mut report = report_of(&[(SQL, 3)]);
    // The scan has already spent the whole limit.
    report.actual_usage = Some(ActualUsage {
        input_tokens: 1_000,
        output_tokens: 100,
        cost_usd: 5.0,
        calls: 10,
        cost_complete: true,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        resumed_cost_usd: 0.0,
    });
    let llm = Reviewer::new(|call| Ok(one_group(call, "Raw SQL.")));
    let rules: Vec<(String, String)> = Vec::new();
    let controls = crate::cost_cap::RunControls {
        max_cost_usd: Some(5.0),
        ..crate::cost_cap::RunControls::default()
    };
    let outcome = review_groups_metered(&mut report, &llm, &cfg(&rules), &controls).await;
    assert!(
        llm.calls().is_empty(),
        "the limit was already reached: nothing is sent"
    );
    assert_eq!(outcome.rules_failed, 1);
    assert_eq!(report.findings.len(), 3, "and no site is removed");
    assert!(
        report
            .ledger
            .cost_cap()
            .is_some_and(|c| !c.not_run_stages.is_empty()),
        "the cap's record names the stage it stopped"
    );
    assert!(report
        .failed_passes
        .iter()
        .any(|p| p.pass == "group review"));
    assert!(report.ledger.is_cost_capped());
}

/// A meter with no spend limit that kept `notes`, as a batch run that lost a batch leaves it.
fn uncapped_meter_with(notes: &[&str]) -> UsageMeter {
    let meter = UsageMeter::with_controls(crate::cost_cap::RunControls::default());
    for n in notes {
        meter.record_note((*n).to_string());
    }
    meter
}

#[test]
fn the_batch_runners_notes_reach_the_ledger_when_no_spend_limit_was_set() {
    let mut report = report_of(&[(SQL, 3)]);
    let meter = uncapped_meter_with(&["Batch msgbatch_x no longer exists at the provider."]);
    assert!(meter.cost_cap_record().is_none(), "no limit was set");

    fold_spend_into_report(&mut report, &meter);

    assert!(report.ledger.cost_cap().is_none());
    assert_eq!(
        report.ledger.runner_notes(),
        ["Batch msgbatch_x no longer exists at the provider."]
    );
}

#[test]
fn the_group_reviews_notes_join_the_notes_the_scan_already_recorded() {
    let mut report = report_of(&[(SQL, 3)]);
    report
        .ledger
        .record_runner_notes(vec!["from the scan".to_string()]);
    let meter = uncapped_meter_with(&["from the group review"]);

    fold_spend_into_report(&mut report, &meter);

    assert_eq!(
        report.ledger.runner_notes(),
        ["from the scan", "from the group review"]
    );
}

#[tokio::test]
async fn the_metered_review_folds_its_usage_into_the_reports_usage_and_telemetry() {
    let mut report = report_of(&[(SQL, 3)]);
    report.actual_usage = Some(ActualUsage {
        input_tokens: 1_000,
        output_tokens: 100,
        cost_usd: 1.0,
        calls: 4,
        cost_complete: true,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        resumed_cost_usd: 0.0,
    });
    struct Priced(Reviewer);
    #[async_trait::async_trait]
    impl LlmPort for Priced {
        async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
            let resp = self.0.complete(req).await?;
            Ok(priced_reply(resp.text, 0.25))
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            let resp = self.complete(req).await?;
            on_delta(&resp.text);
            Ok(resp)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let llm = Priced(Reviewer::new(|call| Ok(one_group(call, "Raw SQL."))));
    let rules: Vec<(String, String)> = Vec::new();
    let controls = crate::cost_cap::RunControls::default();
    review_groups_metered(&mut report, &llm, &cfg(&rules), &controls).await;
    let usage = report.actual_usage.as_ref().expect("usage");
    assert_eq!(
        usage.calls, 5,
        "the scan's four calls and the one stage 2 call"
    );
    assert!((usage.cost_usd - 1.25).abs() < 1e-6, "{}", usage.cost_usd);
    let telemetry = report.ledger.stage_telemetry().expect("telemetry");
    let stage = telemetry
        .stages
        .iter()
        .find(|s| s.stage == Stage::GroupReview)
        .expect("a group review row");
    assert_eq!(stage.usage.calls, 1);
    assert_eq!(stage.usage.models.get("claude-fable-5-1"), Some(&1));
}

#[tokio::test]
async fn a_scan_report_is_reviewed_with_the_runs_model_and_escalation_settings() {
    let mut report = report_of(&[(SQL, 6)]);
    report.code_lines = 1_000;
    let llm = Reviewer::new(|call| {
        let escalations: Vec<(usize, &str, u32, &str)> = call
            .indices
            .iter()
            .map(|&i| (i, "outlier_in_group", i as u32 + 1, "Odd."))
            .collect();
        Ok(reply(
            "Raw SQL.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &escalations,
            &[],
        ))
    });
    // The project (or the flags) chose the model, the rate and the floor.
    let controls = crate::cost_cap::RunControls {
        review: camerata_api_types::project::ReviewSettings {
            escalation_rate_per_kloc: 1.0,
            escalation_floor: 3,
            group_review_model: "claude-opus-5-5".to_string(),
            ..Default::default()
        },
        ..crate::cost_cap::RunControls::default()
    };
    let rules = vec![(SQL.to_string(), "Never concatenate SQL.".to_string())];
    let outcome = review_scan_report(&mut report, &llm, &controls, &rules, false, None)
        .await
        .expect("it ran");
    assert_eq!(outcome.rules_reviewed, 1);
    assert_eq!(
        report.provenance.group_review_model.as_deref(),
        Some("claude-opus-5-5")
    );
    for req in llm.requests() {
        assert_eq!(req.model, "claude-opus-5-5");
    }
    assert!(llm.calls()[0].prompt.contains("Never concatenate SQL."));
    let block = report.ledger.escalation().expect("block");
    assert_eq!((block.floor, block.budget, block.escalated), (3, 3, 3));
    assert_eq!(block.model.as_deref(), Some("claude-opus-5-5"));

    // A report with no findings is not sent anywhere.
    let mut empty = ScanReport::gated(&[]);
    empty.gated = false;
    let llm = Reviewer::new(|call| Ok(one_group(call, "x")));
    assert!(
        review_scan_report(&mut empty, &llm, &controls, &rules, false, None)
            .await
            .is_none()
    );
    assert!(llm.calls().is_empty());
}

/// On the CLI backend the stage 2 system prompt is the same prompt plus the no-tools line
/// (`NO_TOOLS_LINE` on the base branch, about 260 characters, a suffix). The estimate has to
/// cover that variant too. The line is not on this branch, so its length is an upper bound here.
#[test]
fn the_estimate_covers_the_cli_backends_longer_system_prompt() {
    use camerata_ui_core::scan::GROUP_REVIEW_SYSTEM_TOKENS;
    const NO_TOOLS_LINE_CHARS_UPPER_BOUND: usize = 300;
    let nonce = crate::prompt_frame::FrameNonce::generate();
    let system = crate::prompt_frame::with_untrusted_clause(group_review_system_prompt(), &nonce);
    let cli_tokens = (system.len() + NO_TOOLS_LINE_CHARS_UPPER_BOUND) as f64 / 4.0;
    assert!(
        cli_tokens <= GROUP_REVIEW_SYSTEM_TOKENS,
        "the cli system prompt is up to {cli_tokens} tokens, over the estimate"
    );
}

/// The cockpit's and the pre-flight's estimate of stage 2 is built from these figures
/// (`camerata_ui_core::scan::GROUP_REVIEW_*`). They have to cover what the stage really sends, at
/// four characters a token, and not be wildly above it.
#[test]
fn the_estimators_figures_cover_the_real_prompt() {
    use camerata_ui_core::scan::{GROUP_REVIEW_INPUT_TOKENS_PER_SITE, GROUP_REVIEW_SYSTEM_TOKENS};
    let nonce = crate::prompt_frame::FrameNonce::generate();
    let system = crate::prompt_frame::with_untrusted_clause(group_review_system_prompt(), &nonce);
    let system_tokens = system.len() as f64 / 4.0;
    assert!(
        system_tokens <= GROUP_REVIEW_SYSTEM_TOKENS,
        "the system prompt is {system_tokens} tokens, over the estimate"
    );
    assert!(
        system_tokens >= GROUP_REVIEW_SYSTEM_TOKENS * 0.7,
        "the system prompt is {system_tokens} tokens, far under the estimate"
    );
    // A site at full width: an 80-character path, a 160-character headline, a 200-character
    // stage 1 reason.
    let mut f = site(SQL, 7, "medium");
    f.path = format!("{}.rs", "p".repeat(76));
    f.detail = format!("{} \u{2014} detail", "h".repeat(160));
    f.captures.insert(
        STAGE1_CAPTURE.to_string(),
        stage1_capture("unclear", &"r".repeat(200)),
    );
    let line = member_line(&f, 7, false);
    let line_tokens = line.len() as f64 / 4.0;
    assert!(
        line_tokens <= GROUP_REVIEW_INPUT_TOKENS_PER_SITE,
        "a full-width site line is {line_tokens} tokens, over the estimate"
    );
    assert!(
        line_tokens >= GROUP_REVIEW_INPUT_TOKENS_PER_SITE * 0.7,
        "a full-width site line is {line_tokens} tokens, far under the estimate"
    );
    // The reply budget per site is the one the request's cap is built from.
    assert_eq!(
        camerata_ui_core::scan::GROUP_REVIEW_OUTPUT_TOKENS_PER_SITE as u32,
        OUTPUT_TOKENS_PER_MEMBER
    );
    assert_eq!(
        camerata_ui_core::scan::GROUP_REVIEW_MAX_CHUNK_SITES as usize,
        MAX_CHUNK_MEMBERS
    );
}

#[tokio::test]
async fn what_stage_2_wrote_survives_the_seal_and_a_row_thrown_out_since_is_no_longer_a_candidate()
{
    let mut report = report_of(&[(SQL, 4)]);
    report.code_lines = 1_000;
    let llm = Reviewer::new(|call| {
        let escalations: Vec<(usize, &str, u32, &str)> = call
            .indices
            .iter()
            .map(|&i| (i, "rule_misfit", i as u32 + 1, "Odd."))
            .collect();
        Ok(reply(
            "Raw SQL built from input.",
            &[("All", "one cause", call.indices.clone())],
            &[],
            &escalations,
            &[],
        ))
    });
    run(&mut report, &llm).await;
    assert_eq!(tree(&report).candidates.len(), 4);
    let rule = rule_node(&report, SQL);
    // A later step throws one escalated row out (the seal moves it aside and keeps it).
    let thrown_key = sites_of(&report, SQL)[1].site_key.clone();
    let row = report
        .findings
        .iter_mut()
        .find(|f| f.site_key == thrown_key)
        .unwrap();
    crate::engine_discard::mark(row, crate::engine_discard::DISCARD_VERDICT);

    let sealed = report.seal();
    let t = sealed.tree.as_ref().expect("a tree");
    // The summary, the sub-groups and the line are what stage 2 made them.
    assert_eq!(
        t.node(&rule).and_then(|n| n.summary.as_deref()),
        Some("Raw SQL built from input.")
    );
    assert!(t.group_reviewed);
    assert_eq!(
        t.nodes
            .iter()
            .filter(|n| n.level == NodeLevel::Subgroup && n.label == "All")
            .count(),
        1
    );
    // The thrown-out row is in the thrown-out node and is no longer a candidate; the budget's
    // counts follow the candidates that are left.
    assert!(t.candidates.iter().all(|c| c.key != thrown_key));
    assert_eq!(t.candidates.len(), 3);
    assert_eq!(t.budget.candidates, 3);
    assert_eq!(
        t.budget.escalated,
        t.candidates.iter().filter(|c| c.escalated).count()
    );
    assert!(t
        .nodes
        .iter()
        .any(|n| crate::review_tree::is_thrown_out_id(&n.id) && n.members.contains(&thrown_key)));
}

#[tokio::test]
async fn a_failed_group_review_is_named_in_the_operator_failure_report() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    // The reviewer answers the secrets group, and fails the SQL group twice (the runner's retry
    // included).
    let llm = Reviewer::new(|call| {
        if call.node_id.contains("SQL") {
            Ok(r#"{"summary":"s","confidence":"high"}"#.to_string())
        } else {
            Ok(one_group(call, "Keys."))
        }
    });
    run(&mut report, &llm).await;
    let failure = crate::failure_report::build(&report);
    let entries: Vec<&crate::failure_report::FailureEntry> = failure
        .entries
        .iter()
        .filter(|e| e.part == "group review")
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "one entry per failed rule group: {failure:#?}"
    );
    let e = entries[0];
    assert!(
        e.message.contains("rule:SEC-NO-RAW-SQL-CONCAT-1"),
        "{}",
        e.message
    );
    assert!(e.message.contains("schema does not have"), "{}", e.message);
    assert_eq!(e.files.len(), 3, "the files of the sites left unread");
    assert!(failure.unresolved().any(|u| u.part == "group review"));
    let md = failure.render_md();
    assert!(md.contains("group review"), "{md}");
    // The run is incomplete, so the client export is refused.
    assert!(!report.incomplete_reasons().is_empty());
}

// ── A15: every sub-group has its own summary ───────────────────────────────────────────────

/// A reply with a summary of its own for each sub-group: `(label, cause, summary, members)`.
fn reply_with_summaries(
    summary: &str,
    subgroups: &[(&str, &str, &str, Vec<usize>)],
    outliers: &[(usize, &str, &str)],
) -> String {
    serde_json::json!({
        "summary": summary,
        "subgroups": subgroups
            .iter()
            .map(|(l, c, s, m)| serde_json::json!({
                "label": l, "cause": c, "summary": s, "members": m
            }))
            .collect::<Vec<_>>(),
        "outliers": outliers
            .iter()
            .map(|(i, r, w)| serde_json::json!({"index": i, "reason": r, "why": w}))
            .collect::<Vec<_>>(),
        "escalations": [],
        "disagreements": [],
    })
    .to_string()
}

fn sub_by_label<'a>(report: &'a ScanReport, rule: &str, label: &str) -> &'a TreeNode {
    subgroups_of(report, rule)
        .into_iter()
        .find(|n| n.label == label)
        .unwrap_or_else(|| panic!("no sub-group {label}"))
}

#[tokio::test]
async fn every_subgroup_has_its_own_summary_stored_on_its_node() {
    let mut report = report_of(&[(SQL, 6)]);
    let llm = Reviewer::new(|_| {
        Ok(reply_with_summaries(
            "Raw SQL built from input.",
            &[
                (
                    "Params",
                    "a query parameter",
                    "Built from a query parameter. Bind it instead.",
                    vec![0, 1, 2],
                ),
                (
                    "Headers",
                    "a header",
                    "Built from a header. Validate and bind it.",
                    vec![3, 4, 5],
                ),
            ],
            &[],
        ))
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(outcome.rules_reviewed, 1);
    let rule = rule_node(&report, SQL);
    assert_eq!(
        sub_by_label(&report, &rule, "Params").summary.as_deref(),
        Some("Built from a query parameter. Bind it instead.")
    );
    assert_eq!(
        sub_by_label(&report, &rule, "Headers").summary.as_deref(),
        Some("Built from a header. Validate and bind it.")
    );
    // The rule node keeps its own, different summary.
    assert_eq!(
        node(&report, &rule).summary.as_deref(),
        Some("Raw SQL built from input.")
    );
    // The counts beside a sub-group come from the tree.
    let params = sub_by_label(&report, &rule, "Params").id.clone();
    assert_eq!(tree(&report).counts(&params).sites, 3);

    // A sub-group with no summary is not a reply: the schema requires it.
    let mut no_summary: serde_json::Value =
        serde_json::from_str(&reply("s", &[("a", "b", vec![0])], &[], &[], &[])).unwrap();
    no_summary["subgroups"][0]
        .as_object_mut()
        .unwrap()
        .remove("summary");
    assert!(Stage2Reply::parse(&no_summary.to_string()).is_err());
    assert!(
        reply_schemas::check::violation(&reply_schemas::stage2_reply_schema(), &no_summary)
            .is_some()
    );
}

#[tokio::test]
async fn a_chunked_node_gives_each_chunks_subgroups_their_own_summaries() {
    let mut report = report_of(&[(SQL, 200)]);
    let llm = Reviewer::new(|call| {
        if call.summary_call {
            Ok(summary_reply("The whole rule."))
        } else {
            Ok(reply_with_summaries(
                "A part of the rule.",
                &[(
                    "Part",
                    "a part",
                    &format!("Covers {} sites of one part.", call.indices.len()),
                    call.indices.clone(),
                )],
                &[],
            ))
        }
    });
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    assert_eq!(
        node(&report, &rule).summary.as_deref(),
        Some("The whole rule.")
    );
    let subs = subgroups_of(&report, &rule);
    assert_eq!(subs.len(), 2);
    let mut summaries: Vec<&str> = subs.iter().filter_map(|n| n.summary.as_deref()).collect();
    summaries.sort_unstable();
    assert_eq!(
        summaries,
        vec![
            "Covers 150 sites of one part.",
            "Covers 50 sites of one part."
        ]
    );
    // The rule-level call returns a rule summary only.
    assert_eq!(
        llm.requests()
            .iter()
            .filter(|r| r.response_schema == Some(reply_schemas::rule_summary_reply_schema()))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_subgroup_summary_count_mismatch_is_flagged() {
    let mut report = report_of(&[(SQL, 4)]);
    let llm = Reviewer::new(|_| {
        Ok(reply_with_summaries(
            "Raw SQL.",
            &[
                (
                    "Wrong",
                    "c",
                    "The same flaw appears in 40 sites.",
                    vec![0, 1],
                ),
                (
                    "Right",
                    "c",
                    "Both of these 2 sites build SQL from input.",
                    vec![2, 3],
                ),
            ],
            &[],
        ))
    });
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    let wrong = sub_by_label(&report, &rule, "Wrong");
    assert!(
        wrong
            .flags
            .iter()
            .any(|f| f.starts_with("summary_count_mismatch")),
        "{:?}",
        wrong.flags
    );
    assert!(sub_by_label(&report, &rule, "Right").flags.is_empty());
}

#[tokio::test]
async fn a_subgroup_summary_that_omits_a_differing_member_is_flagged() {
    let mut report = report_of(&[(SQL, 4)]);
    let rows = sites_of(&report, SQL);
    let named = format!("{}:{}", rows[1].path, rows[1].line);
    let (key0, key1) = (rows[0].site_key.clone(), rows[1].site_key.clone());
    let llm = Reviewer::new(move |_| {
        Ok(reply_with_summaries(
            "Raw SQL.",
            &[(
                "Mixed",
                "c",
                &format!("Built from input, except {named}."),
                vec![0, 1, 2, 3],
            )],
            &[
                (0, "different_object", "A different table."),
                (1, "different_construct", "A procedure call."),
            ],
        ))
    });
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    let mixed = sub_by_label(&report, &rule, "Mixed");
    // Site 1 is named by path and line; site 0 is not.
    assert_eq!(
        mixed.flags,
        vec![format!("summary_omits_member:{key0}")],
        "{key1} is named"
    );
}

#[tokio::test]
async fn the_unplaced_subgroup_summary_is_mechanical() {
    let mut report = report_of(&[(SQL, 4)]);
    // The reply places 0 and 1 only.
    let llm = Reviewer::new(|_| {
        Ok(reply_with_summaries(
            "Raw SQL.",
            &[("Placed", "c", "These two are placed.", vec![0, 1])],
            &[],
        ))
    });
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    let unplaced = node(&report, &unplaced_sub_id(&rule));
    assert_eq!(
        unplaced.summary.as_deref(),
        Some(
            "2 sites in this rule group were not placed in a sub-group by the group reviewer; \
             they are kept together here for the architect to review."
        )
    );
    assert!(unplaced.flags.is_empty());
    // It follows the rows: after the seal throws one of them out, the sentence says 1.
    let thrown = unplaced.members[0].clone();
    let row = report
        .findings
        .iter_mut()
        .find(|f| f.site_key == thrown)
        .unwrap();
    crate::engine_discard::mark(row, crate::engine_discard::DISCARD_VERDICT);
    let sealed = report.seal();
    let t = sealed.tree.as_ref().unwrap();
    assert_eq!(
        t.node(&unplaced_sub_id(&rule))
            .and_then(|n| n.summary.as_deref()),
        Some(
            "1 site in this rule group was not placed in a sub-group by the group reviewer; \
             it is kept here for the architect to review."
        )
    );
}

#[tokio::test]
async fn a_reviewer_edited_subgroup_summary_is_kept_on_a_rerun_when_its_members_are_unchanged() {
    let mut report = report_of(&[(SQL, 4)]);
    let reply_text = |summary: &'static str| {
        Reviewer::new(move |_| {
            Ok(reply_with_summaries(
                "Raw SQL.",
                &[
                    ("One", "c", summary, vec![0, 1]),
                    ("Two", "c", summary, vec![2, 3]),
                ],
                &[],
            ))
        })
    };
    run(&mut report, &reply_text("The model's first words.")).await;
    let rule = rule_node(&report, SQL);
    let one_id = sub_by_label(&report, &rule, "One").id.clone();
    {
        let n = report
            .tree
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|n| n.id == one_id)
            .unwrap();
        n.summary = Some("The architect's words for One.".to_string());
        n.summary_source = crate::review_tree::TextSource::Reviewer;
    }
    run(&mut report, &reply_text("The model's second words.")).await;
    let one = sub_by_label(&report, &rule, "One");
    assert_eq!(
        one.summary.as_deref(),
        Some("The architect's words for One.")
    );
    assert_eq!(one.summary_source, crate::review_tree::TextSource::Reviewer);
    assert_eq!(one.id, one_id, "the same node, so the edit is keyed to it");
    // The other sub-group takes the new model summary.
    assert_eq!(
        sub_by_label(&report, &rule, "Two").summary.as_deref(),
        Some("The model's second words.")
    );
}

#[tokio::test]
async fn an_outlier_mark_in_the_stage_2_reply_never_makes_an_escalation() {
    let mut report = report_of(&[(SQL, 4)]);
    // The reply marks two outliers and escalates nothing, and records no disagreement.
    let llm = Reviewer::new(|call| {
        Ok(reply(
            "Raw SQL.",
            &[("All", "one cause", call.indices.clone())],
            &[
                (1, "different_object", "A different table."),
                (2, "could_be_intentional", "Looks deliberate."),
            ],
            &[],
            &[],
        ))
    });
    run(&mut report, &llm).await;
    let rule = rule_node(&report, SQL);
    assert!(node(&report, &rule).escalations.is_empty());
    assert!(tree(&report).candidates.is_empty());
    for f in sites_of(&report, SQL) {
        assert_eq!(f.review.escalation_state, EscalationState::NotACandidate);
        assert_eq!(f.review.escalation_reason, None);
    }
    // The outliers themselves are recorded.
    let rows = sites_of(&report, SQL);
    assert_eq!(
        rows[1].review.outlier_reason,
        Some(OutlierReason::DifferentObject)
    );
    assert_eq!(
        rows[2].review.outlier_reason,
        Some(OutlierReason::CouldBeIntentional)
    );
}

// ── Resume (design 6.2, A15) ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_re_asks_only_the_failed_group_review_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let rules: Vec<(String, String)> = Vec::new();

    // First process: the secrets group cannot be reached, the SQL group is read.
    let first = Reviewer::new(|c| {
        if c.node_id.contains(SECRETS) {
            Err(anyhow::anyhow!("connection reset by peer"))
        } else {
            Ok(one_group(c, "Read."))
        }
    });
    let mut report = report_of(&[(SQL, 3), (SECRETS, 3)]);
    let meter = UsageMeter::default();
    let out = crate::checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        review_groups(&mut report, &first, &cfg(&rules), Some(&meter)),
    )
    .await;
    assert_eq!(out.rules_failed, 1, "the secrets group is a failed part");
    assert_eq!(out.rules_reviewed, 1);

    // Second process, the same checkpoint: only the chunk with no result is asked.
    let second = Reviewer::new(|c| Ok(one_group(c, "Read.")));
    let mut report = report_of(&[(SQL, 3), (SECRETS, 3)]);
    let meter = UsageMeter::default();
    let out = crate::checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        review_groups(&mut report, &second, &cfg(&rules), Some(&meter)),
    )
    .await;
    let asked: Vec<String> = second.calls().iter().map(|c| c.node_id.clone()).collect();
    assert_eq!(asked.len(), 1, "one call, not two: {asked:?}");
    assert!(asked[0].contains(SECRETS), "{asked:?}");
    assert_eq!(out.rules_failed, 0);
    assert_eq!(out.rules_reviewed, 2, "both groups are read in the end");
    assert_eq!(
        meter.snapshot().input_tokens,
        2_000,
        "the usage of the chunk read before the restart is reported with the one just read"
    );
    // The SQL group's reply was applied from the checkpoint: its sites carry its sub-group.
    assert!(subgroups_of(&report, &rule_node(&report, SQL)).len() == 1);
}

// ── Output-side redaction of everything the reviewer writes ───────────────────────────────

/// The config sentence the reviewer wrote on a real run: it quotes a documented Supabase setting
/// value, which is not a secret and must stay as written.
fn password_policy_sentence() -> String {
    format!(
        "Set `minimum_password_length = 8` and `password_requirements = \"{}\"` or stronger.",
        ["lower_upper_", "letters_digits"].concat()
    )
}

fn secret_hits(value: &serde_json::Value) -> usize {
    camerata_secrets::find_secret_hits_in_json(value).len()
}

#[tokio::test]
async fn a_secret_the_reviewer_echoes_ships_redacted_in_every_string_it_wrote() {
    let planted =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::SupabaseSecretKey);
    let leak = planted.value.clone();
    let sentence = password_policy_sentence();
    let mut report = report_of(&[(SQL, 4)]);
    let llm = Reviewer::new({
        let leak = leak.clone();
        let sentence = sentence.clone();
        move |call| {
            Ok(serde_json::json!({
                "summary": format!("{sentence} The key {leak} is committed."),
                "subgroups": [{
                    "label": format!("Key {leak}"),
                    "cause": format!("Cause {leak}"),
                    "summary": format!("Sub-group {leak}. {sentence}"),
                    "members": call.indices,
                }],
                "outliers": [{"index": 1, "reason": "different_object", "why": format!("Why {leak}")}],
                "escalations": [{"index": 2, "reason": "rule_misfit", "rank": 1, "why": format!("Esc {leak}")}],
                "disagreements": [{
                    "index": 3, "field": "real", "stage1": "yes", "mine": "no",
                    "why": format!("Dis {leak}")
                }],
            })
            .to_string())
        }
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(outcome.rules_reviewed, 1);

    let tree_json = serde_json::to_value(tree(&report)).unwrap();
    let rows_json = serde_json::to_value(
        report
            .findings
            .iter()
            .map(|f| &f.review)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for (what, json) in [("tree", &tree_json), ("rows", &rows_json)] {
        let text = json.to_string();
        assert!(!text.contains(&leak), "the {what} carries the planted key");
        assert_eq!(secret_hits(json), 0, "the gate would refuse the {what}");
    }
    // The redaction is the same one the other stages use, and the documented setting value is
    // left exactly as the reviewer wrote it.
    let rule = node(&report, &rule_node(&report, SQL));
    let summary = rule.summary.as_deref().unwrap();
    assert!(summary.contains(&sentence), "{summary}");
    assert!(summary.contains("sb_secret_\u{2026}"), "{summary}");
    let sub = &subgroups_of(&report, &rule.id)[0];
    assert!(sub.label.contains("sb_secret_\u{2026}"));
    assert!(sub.cause.as_deref().unwrap().contains("sb_secret_\u{2026}"));
    assert!(sub.summary.as_deref().unwrap().contains(&sentence));
    assert!(rule.escalations[0].why.contains("sb_secret_\u{2026}"));
}

#[tokio::test]
async fn the_rule_level_summary_call_is_redacted_too() {
    let planted =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::SupabaseSecretKey);
    let leak = planted.value.clone();
    let mut report = report_of(&[(SQL, 151)]);
    let llm = Reviewer::new({
        let leak = leak.clone();
        move |call| {
            if call.summary_call {
                Ok(summary_reply(&format!("Rule level {leak}.")))
            } else {
                Ok(one_group(call, "Part."))
            }
        }
    });
    run(&mut report, &llm).await;
    let json = serde_json::to_value(tree(&report)).unwrap();
    assert!(!json.to_string().contains(&leak));
    assert_eq!(secret_hits(&json), 0);
    let rule = node(&report, &rule_node(&report, SQL));
    assert!(rule
        .summary
        .as_deref()
        .unwrap()
        .contains("sb_secret_\u{2026}"));
}

// ── A reply that leaves `rank` out ────────────────────────────────────────────────────────

/// The reply shape a real run produced for ARCH-STRICT-LAYERING-1: every escalation complete
/// except for the `rank`.
fn reply_without_rank(call: &Call, escalated: &[usize]) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&one_group(call, "Layering.")).unwrap();
    v["escalations"] = serde_json::Value::Array(
        escalated
            .iter()
            .map(|i| serde_json::json!({"index": i, "reason": "rule_misfit", "why": "Odd one."}))
            .collect(),
    );
    v.to_string()
}

#[tokio::test]
async fn an_escalation_without_a_rank_is_kept_unranked_and_recorded_not_refused() {
    let mut report = report_of(&[(LAYERING, 4)]);
    let llm = Reviewer::new(|call| {
        let mut v: serde_json::Value =
            serde_json::from_str(&reply_without_rank(call, &[1, 2])).unwrap();
        // Site 3 comes back fully ranked, so the order between the two kinds is observable.
        v["escalations"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
                {"index": 3, "reason": "rule_misfit", "rank": 1, "why": "Worst."}
            ));
        Ok(v.to_string())
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(llm.calls().len(), 1, "no second call for a missing rank");
    assert_eq!(outcome.rules_reviewed, 1);
    assert_eq!(outcome.rules_failed, 0);
    assert_eq!(outcome.ranks_defaulted.len(), 2);

    // The ranked escalation comes first; the two with no rank follow in the engine's order.
    let rule = node(&report, &rule_node(&report, LAYERING));
    let by_key: BTreeMap<&str, u32> = rule
        .escalations
        .iter()
        .map(|e| (e.key.as_str(), e.rank))
        .collect();
    let rows = sites_of(&report, LAYERING);
    assert_eq!(by_key[rows[3].site_key.as_str()], 1);
    assert_eq!(by_key[rows[1].site_key.as_str()], 0);
    assert_eq!(by_key[rows[2].site_key.as_str()], 0);
    let order: Vec<&str> = tree(&report)
        .candidates
        .iter()
        .map(|c| c.key.as_str())
        .collect();
    assert_eq!(order[0], rows[3].site_key);

    // Never silent: the ledger names the sites.
    let notes = report.ledger.runner_notes().join("\n");
    assert!(notes.contains("without a rank"), "{notes}");
    assert!(notes.contains(&rows[1].site_key), "{notes}");
    assert!(notes.contains(&rows[2].site_key), "{notes}");
}

#[test]
fn a_rank_of_the_wrong_kind_is_still_refused_and_a_given_rank_is_not_recorded_as_defaulted() {
    let bad = serde_json::json!({
        "summary": "s", "subgroups": [], "outliers": [], "disagreements": [],
        "escalations": [{"index": 0, "reason": "rule_misfit", "rank": "first", "why": "w"}],
    })
    .to_string();
    let why = Stage2Reply::parse(&bad).unwrap_err();
    assert!(why.contains("rank is not a whole number"), "{why}");
    let ok = serde_json::json!({
        "summary": "s", "subgroups": [], "outliers": [], "disagreements": [],
        "escalations": [{"index": 0, "reason": "rule_misfit", "rank": 2, "why": "w"}],
    })
    .to_string();
    let parsed = Stage2Reply::parse(&ok).unwrap();
    assert_eq!(
        (
            parsed.escalations[0].rank,
            parsed.escalations[0].rank_defaulted
        ),
        (2, false)
    );
}

// ── The CLI backend: no tools ─────────────────────────────────────────────────────────────

/// The shape of the one reply a real run got from Fable on the CLI backend: a transcript of tool
/// calls (invented paths), not the JSON object. Nothing ran.
fn tool_call_transcript() -> String {
    [
        "I'll look at the repository layout first.\n\n",
        "<function_calls>\n<invoke name=\"Bash\">\n",
        "<parameter name=\"command\">cd /Users/auditor/Repos && ls -d app*</parameter>\n",
        "</invoke>\n</function_calls>",
    ]
    .concat()
}

#[tokio::test]
async fn the_cli_prompts_say_no_tools_exist_and_the_api_prompts_do_not() {
    for cli in [true, false] {
        let mut report = report_of(&[(LAYERING, 151)]);
        let llm = Reviewer::new(|call| {
            if call.summary_call {
                Ok(summary_reply("Whole rule."))
            } else {
                Ok(one_group(call, "Part."))
            }
        });
        let rules: Vec<(String, String)> = Vec::new();
        let config = GroupReviewConfig {
            cli_backend: cli,
            ..cfg(&rules)
        };
        review_groups(&mut report, &llm, &config, None).await;
        let requests = llm.requests();
        assert!(requests.len() >= 3, "chunk calls and the rule summary call");
        for req in requests {
            let system = req.system.clone().unwrap_or_default();
            assert_eq!(system.contains("No tools exist"), cli, "cli={cli}");
            // The line is added to the end of the prompt, which the stand-in and the journal
            // still recognise by its start.
            assert!(
                system.starts_with(&group_review_system_prompt())
                    || system.starts_with(&rule_summary_system_prompt())
            );
        }
    }
}

#[tokio::test]
async fn a_tool_call_transcript_reply_is_refused_and_never_stored_as_good() {
    assert!(Stage2Reply::parse(&tool_call_transcript()).is_err());
    assert!(parse_rule_summary(&tool_call_transcript()).is_err());

    let mut report = report_of(&[(LAYERING, 3)]);
    let llm = Reviewer::new(|call| {
        if call.attempt == 1 {
            Ok(tool_call_transcript())
        } else {
            Ok(one_group(call, "Layering."))
        }
    });
    let outcome = run(&mut report, &llm).await;
    assert_eq!(llm.calls().len(), 2, "refused once, asked again");
    assert_eq!(outcome.rules_failed, 0);
    // The second ask carries the refusal, and only the good reply reached the tree.
    assert!(llm.requests()[1].prompt.contains("not a JSON object"));
    let rule = node(&report, &rule_node(&report, LAYERING));
    assert_eq!(rule.summary.as_deref(), Some("Layering."));

    // A transcript on both tries is a failed part, with no reply kept for the rule.
    let mut report = report_of(&[(LAYERING, 3)]);
    let llm = Reviewer::new(|_| Ok(tool_call_transcript()));
    let outcome = run(&mut report, &llm).await;
    assert_eq!(outcome.rules_failed, 1);
    assert_eq!(
        node(&report, &rule_node(&report, LAYERING)).summary,
        None,
        "nothing from the transcript is stored as a summary"
    );
}
