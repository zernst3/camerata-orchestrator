//! The runner, against the scripted stand-in agent (no model is called). Synthetic data only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camerata_api_types::audit::FindingKind;
use serde_json::{json, Value};

use super::super::coverage::FileState;
use super::super::{
    AgentRun, AgentStop, FollowUp, PieceAgent, PieceLimits, Room, ScriptStep, ScriptedAgent,
    TurnUsage,
};
use super::{find_repo, FinderOptions, FinderResult};
use crate::cost_cap::RunControls;
use crate::scan_routing::AuditRule;

pub(super) const REPO: &str = "acme/app";

pub(super) fn rust_file(n: usize) -> String {
    (1..=n)
        .map(|i| format!("fn item_{i}() {{ run({i}); }}\n"))
        .collect()
}

pub(super) fn repo_files() -> Vec<(String, String)> {
    vec![
        ("src/a.rs".to_string(), rust_file(6)),
        ("src/b.rs".to_string(), rust_file(4)),
    ]
}

pub(super) fn corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    set.push(camerata_rules::bare_rule("ARCH-ONE-1", "universal"));
    set.push(camerata_rules::bare_rule("SEC-TWO-1", "universal"));
    set
}

pub(super) fn rules() -> Vec<AuditRule> {
    vec![
        AuditRule::new("ARCH-ONE-1", "Keep one thing in one place."),
        AuditRule::new("SEC-TWO-1", "Check the caller."),
    ]
}

pub(super) fn options() -> FinderOptions {
    FinderOptions::new("claude-fable-5-2")
}

pub(super) async fn run_with(
    agent: &dyn PieceAgent,
    files: &[(String, String)],
    rules: &[AuditRule],
    corpus: &camerata_rules::RuleSet,
    options: &FinderOptions,
) -> FinderResult {
    find_repo(
        agent,
        REPO,
        files,
        rules,
        corpus,
        &RunControls::default(),
        options,
        None,
    )
    .await
    .expect("the finder returns a result")
}

pub(super) async fn run(agent: &dyn PieceAgent) -> FinderResult {
    run_with(agent, &repo_files(), &rules(), &corpus(), &options()).await
}

pub(super) fn problem(n: u32, path: &str, line: usize, severity: &str, code: &str) -> Value {
    json!({
        "n": n, "path": path, "line": line, "end_line": line, "severity": severity,
        "title": format!("Problem {n} needs a fix"), "code": code,
        "what": "The caller is not checked.", "why": "Anyone can reach it.",
        "fix": "Check the caller first.", "sure": "yes", "effort": "low",
        "label": "missing check"
    })
}

pub(super) fn out(file: &str, value: &Value) -> ScriptStep {
    ScriptStep::Write {
        path: format!("_camerata/out/{file}"),
        text: value.to_string(),
    }
}

pub(super) fn read_all(path: &str, lines: usize) -> ScriptStep {
    ScriptStep::Read {
        path: path.to_string(),
        first_line: 1,
        count: lines,
    }
}

pub(super) fn answer_clean(rule: &str, path: &str, line: usize) -> Value {
    json!({"rule": rule, "answer": "clean", "places": [{"path": path, "line": line}]})
}

/// A whole review of `paths` (each with its line count): the reads, the problems, the checklist,
/// the card and an honest empty list of unread files.
pub(super) fn full_script(
    paths: &[(&str, usize)],
    problems: Vec<Value>,
    citations: Vec<Value>,
    answers: Vec<Value>,
) -> Vec<ScriptStep> {
    let mut steps: Vec<ScriptStep> = paths.iter().map(|(p, n)| read_all(p, *n)).collect();
    steps.push(out("problems-1.json", &Value::Array(problems)));
    steps.push(out(
        "checklist.json",
        &json!({"citations": citations, "answers": answers}),
    ));
    steps.push(ScriptStep::Write {
        path: "_camerata/out/card.md".to_string(),
        text: "Entry points: none. It trusts its callers to authenticate.".to_string(),
    });
    steps.push(out("unread.json", &json!([])));
    steps
}

/// The standard good review of both files with the given problems and every rule answered clean.
pub(super) fn good_script(problems: Vec<Value>, citations: Vec<Value>) -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        problems,
        citations,
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/b.rs", 1),
        ],
    )
}

pub(super) fn usage(cost: f64) -> TurnUsage {
    TurnUsage {
        input_tokens: 1_000,
        output_tokens: 100,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        cost_usd: Some(cost),
    }
}

/// A stand-in that plays a different script for each piece (keyed by the piece's first file) and
/// for the joints call, and counts the calls it gets.
#[derive(Clone)]
pub(super) struct ByPiece {
    agents: HashMap<String, ScriptedAgent>,
    pub runs: Arc<AtomicUsize>,
    pub follow_ups: Arc<AtomicUsize>,
    pub seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ByPiece {
    /// Conversations still kept over every script (zero at the end of a run).
    pub fn kept(&self) -> usize {
        self.agents.values().map(ScriptedAgent::kept).sum()
    }

    pub fn new(agents: Vec<(&str, ScriptedAgent)>) -> Self {
        Self {
            agents: agents
                .into_iter()
                .map(|(k, a)| (k.to_string(), a))
                .collect(),
            runs: Arc::default(),
            follow_ups: Arc::default(),
            seen: Arc::default(),
        }
    }

    fn key(room: &Room) -> String {
        if room.extra_files().is_empty() {
            room.piece_files().first().cloned().unwrap_or_default()
        } else {
            "joints".to_string()
        }
    }

    fn pick(&self, room: &Room) -> &ScriptedAgent {
        self.agents
            .get(&Self::key(room))
            .unwrap_or_else(|| panic!("no script for {}", Self::key(room)))
    }
}

#[async_trait::async_trait]
impl PieceAgent for ByPiece {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(Self::key(room));
        self.pick(room).run(room, brief, limits).await
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.follow_ups.fetch_add(1, Ordering::SeqCst);
        self.pick(room).follow_up(room, prior, turn, limits).await
    }

    fn release(&self, handle: &super::super::ConversationHandle) {
        // Handles are unique over the scripts, so only the one that owns it forgets it.
        for agent in self.agents.values() {
            agent.release(handle);
        }
    }
}

mod checklist_outcomes;
mod checkpoint;
mod contract;
mod conventions;
mod coverage;
mod downstream;
mod join_follow_ups;
mod joints;
mod ledger;
mod metering;
mod no_convention;
mod one_piece;
mod resume;
mod setup;
mod severity;
mod switch;
mod vocabulary;

pub(super) fn kinds_of(rows: &[crate::onboard::Finding], rule: &str) -> Vec<&'static str> {
    rows.iter()
        .filter(|f| f.rule_id.eq_ignore_ascii_case(rule))
        .map(|f| match &f.kind {
            Some(FindingKind::Defect) => "defect",
            Some(FindingKind::Deviation(_)) => "deviation",
            Some(FindingKind::ConventionObserved(_)) => "convention_observed",
            Some(FindingKind::NoConvention(_)) => "no_convention",
            Some(FindingKind::NotAssessed(_)) => "not_assessed",
            _ => "other",
        })
        .collect()
}

pub(super) type Files = Vec<(String, String)>;
