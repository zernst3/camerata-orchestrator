//! Stage 2, the group reviewer (review tree design 4.2, 4.3, 4.4 and A5, A8, A9, A13).
//!
//! Stage 1 rates each site. Stage 2 is the architect's first reader of each rule group: one
//! structured call per rule node writes the summary the architect reads at that node, sorts the
//! members into sub-groups by shared cause, marks the sites that do not belong with the rest and
//! picks the sites he must look at himself, each with one reason from a closed list. The
//! escalation formula ([`crate::review_tree::rank_candidates`]) then decides how many of those
//! reach his queue.
//!
//! # What the model may and may not do
//!
//! * It never removes a site. Every member index is in exactly one sub-group; an index the reply
//!   leaves out lands in the rule's `unplaced` sub-group and is named in the ledger, and an index
//!   the reply invents is counted and ignored. A chunk the model could not be read for keeps the
//!   placement the grouping pass gave it.
//! * It never overwrites stage 1. A disagreement is stored beside stage 1's answer, whose value
//!   is read from the row, not from the reply.
//! * It never writes a count. The numbers shown beside a summary come from the tree; a number the
//!   prose states is checked against it, and a site that differs from the stated cause but is not
//!   named in the summary is flagged (A9).
//! * Every field of its reply has a home on a row or in a node's summary
//!   ([`STAGE2_FIELD_HOMES`]); the reply schema has no field without one, and is closed.
//!
//! # How it runs
//!
//! Through the shared batch runner ([`super::stage_batch::run_stage`]) in `Parallel` mode,
//! because the architect is waiting on it: AIMD concurrency, a pause on a 429 or an overloaded
//! answer, one re-ask of a reply that was turned down. A node over [`MAX_CHUNK_MEMBERS`] members
//! is split along its sub-group hints and gets one more short call for the rule-level summary.
//! Every request passes `ensure_request_fits` (A7): a chunk that is too big is split further, and
//! one that cannot be made to fit is a failed part, never sent.
//!
//! The model comes only from the `--group-review-model` flag or the project setting
//! ([`resolve_group_review_model`]); this stage reads no environment variable.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use camerata_api_types::review::{DisagreementField, EscalationReason, OutlierReason};

use super::stage_batch::{run_stage, ItemReject, StageCtx, StageItems, StageMode, StageSettings};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::onboard::{Finding, ScanReport};
use crate::review_tree::{
    is_thrown_out_id, rank_candidates, NodeEscalation, NodeLevel, Outlier, OutlierBy, RankSettings,
    ReviewTree, TextSource, TreeNode,
};

/// The most members one stage 2 call is asked about. A bigger rule group is read in chunks cut
/// along its sub-group hints, plus one short rule-level summary call.
pub const MAX_CHUNK_MEMBERS: usize = 150;

/// The ledger stage name of the group review.
pub const STAGE_SAMPLE_NAME: &str = "ai-group-review";

/// Output tokens budgeted per member of a chunk: the member's index in a sub-group, its share of
/// the sub-group summaries and of the outlier, escalation and disagreement entries.
pub(super) const OUTPUT_TOKENS_PER_MEMBER: u32 = 40;

/// The longest stage 1 reason a member line carries, in characters.
const REASON_CHARS: usize = 200;

/// Where every field of the stage 2 reply lands. The reply schema
/// ([`reply_schemas::stage2_reply_schema`]) has exactly these fields and no others; a test pins
/// that, so a field cannot be added to the schema without a home.
pub const STAGE2_FIELD_HOMES: &[(&str, &str)] = &[
    ("summary", "TreeNode.summary of the rule node"),
    (
        "subgroups[].label",
        "TreeNode.label of the sub-group, and Finding.review.subgroup_label of its members",
    ),
    (
        "subgroups[].cause",
        "TreeNode.cause of the sub-group, and Finding.review.subgroup_cause of its members",
    ),
    (
        "subgroups[].summary",
        "TreeNode.summary of the sub-group, the text the architect reads at it (A15)",
    ),
    (
        "subgroups[].members[]",
        "TreeNode.members of the sub-group, and Finding.group_path of each member",
    ),
    (
        "outliers[].index",
        "TreeNode.outliers[].key on the sub-group the site is in",
    ),
    (
        "outliers[].reason",
        "TreeNode.outliers[].reason, and Finding.review.outlier_reason",
    ),
    (
        "outliers[].why",
        "TreeNode.outliers[].why, and Finding.review.outlier_why",
    ),
    (
        "escalations[].index",
        "ReviewTree.candidates[].key, and the row it names",
    ),
    (
        "escalations[].reason",
        "ReviewTree.candidates[].reason, and Finding.review.escalation_reason",
    ),
    (
        "escalations[].rank",
        "ReviewTree.candidates[].stage2_rank, and Finding.review.stage2_rank",
    ),
    (
        "escalations[].why",
        "ReviewTree.candidates[].why, and Finding.review.escalation_why",
    ),
    (
        "disagreements[].index",
        "Finding.review.stage2_disagreement on the row it names",
    ),
    (
        "disagreements[].field",
        "Finding.review.stage2_disagreement[].field",
    ),
    (
        "disagreements[].stage1",
        "Finding.review.stage2_disagreement[].stage1, read from the row",
    ),
    (
        "disagreements[].mine",
        "Finding.review.stage2_disagreement[].mine",
    ),
    (
        "disagreements[].why",
        "Finding.review.stage2_disagreement[].why",
    ),
];

// ── The model ──────────────────────────────────────────────────────────────────────────────

/// The group reviewer's model: the pick (the `--group-review-model` flag or the project
/// setting), else the built-in default ([`camerata_api_types::project::STAGE2_DEFAULT_MODEL`]).
/// Never the environment.
pub fn resolve_group_review_model(pick: Option<&str>) -> String {
    pick.map(str::trim).filter(|m| !m.is_empty()).map_or_else(
        || camerata_api_types::project::STAGE2_DEFAULT_MODEL.to_string(),
        str::to_string,
    )
}

// ── The prompts ────────────────────────────────────────────────────────────────────────────

/// The system prompt of a rule-group call. It frames the model as the architect's first reader of
/// the group, says in plain words how the architect works, names the closed lists and what each
/// field means. It names no model and asks for no confidence, so it works for any capable one.
pub fn group_review_system_prompt() -> String {
    let mut p = String::from(
        "You are the group reviewer, the architect's first reader of this rule group. He will see \
         every site.\n\n\
         How the architect works. He reviews patterns, not every site: he reads a summary at each \
         group, and goes down to single sites only where it matters. He sees everything, so \
         nothing you say hides a site from him. Your job is to decide what needs his attention \
         most, and why. You never remove a site: every site you are shown stays in the report \
         whatever you say about it.\n\n\
         A per-site reviewer has already read each site on its own and rated it: its severity, \
         whether it is real (`yes`, `no`, or `unclear` when the code it was shown did not settle \
         it), the effort to fix it, and a one-line reason. You are reading the whole group at \
         once, so you can see what it could not: which sites share a cause, which do not belong \
         with the rest, and which differ.\n\n\
         Decide what in this group needs his attention most and why, in his words: a summary he \
         can read at the rule, the sub-groups by shared cause, each with a summary of its own \
         (the sub-group is the unit he reviews), the sites that do not belong with \
         the rest and the sites he must look at himself, each with one reason from the closed \
         list. You may disagree with the per-site reviewer and must say why. You cannot remove a \
         site.\n\n\
         Answer with one JSON object and nothing else:\n\
         {\"summary\": \"...\",\n \
         \"subgroups\": [{\"label\": \"...\", \"cause\": \"...\", \"summary\": \"...\", \"members\": [index, ...]}],\n \
         \"outliers\": [{\"index\": i, \"reason\": \"<outlier reason>\", \"why\": \"...\"}],\n \
         \"escalations\": [{\"index\": i, \"reason\": \"<escalation reason>\", \"rank\": n, \"why\": \"...\"}],\n \
         \"disagreements\": [{\"index\": i, \"field\": \"severity\" or \"real\", \"stage1\": \"...\", \"mine\": \"...\", \"why\": \"...\"}]}\n\n\
         Rules for each part.\n\
         - summary: what the architect reads at the rule, across its sub-groups. Say what the \
         sites have in common, what is wrong, and what he should do about it. In the summary, \
         name every site that does not share the cause you state, by its `path:line` exactly as \
         listed, and mark each one as an outlier. Do not state how many sites there are, or how \
         many share a cause: the report counts them itself, and a count in your prose is \
         checked against it.\n\
         - subgroups: sort the sites by shared cause. Every index you were given goes in exactly \
         one sub-group. The hint sub-groups below are a starting point from simple shared-cause \
         tests; keep them, merge them or split them as the code warrants. `label` is a few words, \
         `cause` is one sentence that says why these sites belong together. `summary` is what he \
         reads at this sub-group: what its sites share, the fix once, and the sites that differ \
         from the rest of it, each named by its `path:line` exactly as listed and marked as an \
         outlier. The same rule holds: name every site that does not share the stated cause, and \
         do not state how many sites there are.\n\
         - outliers: the sites that do not belong with the rest of their sub-group, each with a \
         reason from the outlier list and a one-line why. An outlier mark alone never puts a \
         site in the architect's queue.\n\
         - escalations: the sites he must look at himself, each with one reason from the \
         escalation list, a rank (1 is the one to see first in this group) and a one-line why. \
         Escalate sparingly: the architect has a limited number of minutes, and a list of \
         everything is a list of nothing; but never hold back a real flaw that fits one of the \
         first six reasons below. One reason per site.\n\
         - disagreements: where you disagree with the per-site reviewer. `field` is `severity` \
         (critical, high, medium or low) or `real` (yes, no or unclear); `stage1` is the \
         per-site reviewer's value and `mine` is yours. Say why.\n\n\
         If the group is shown in parts, place only the sites listed in this part.\n\n\
         The escalation reasons, most urgent first:\n",
    );
    for r in EscalationReason::ALL {
        p.push_str(&format!("- {}: {}\n", r.as_str(), escalation_meaning(r)));
    }
    p.push_str("\nThe outlier reasons:\n");
    for r in OutlierReason::ALL {
        p.push_str(&format!("- {}: {}\n", r.as_str(), outlier_meaning(r)));
    }
    p
}

fn escalation_meaning(r: EscalationReason) -> &'static str {
    match r {
        EscalationReason::UnauthenticatedReach => "someone with no login can reach the flaw",
        EscalationReason::SecretExposure => "a secret or a credential is exposed",
        EscalationReason::DataLoss => "the flaw can destroy or corrupt data",
        EscalationReason::CrossTenant => "one customer's data can be reached by another customer",
        EscalationReason::AuthorizationBypass => {
            "a caller can act without the right ownership or role check"
        }
        EscalationReason::Injection => {
            "input reaches an interpreter (SQL, shell, template, stored XSS)"
        }
        EscalationReason::ConflictingVerdicts => {
            "you and the per-site reviewer disagree on whether it is real, or by two or more \
             severity levels (a disagreement you record escalates the site by this reason \
             automatically)"
        }
        EscalationReason::LikelyFalseAlarmAtHighSeverity => {
            "it is rated high or critical but looks like a false alarm"
        }
        EscalationReason::OutlierInGroup => {
            "it differs from the rest of its group in a way the architect should judge"
        }
        EscalationReason::RuleMisfit => "the rule is the wrong lens for this site",
        EscalationReason::EvidenceUnverified => {
            "the quoted code was not found in the file (the report adds this itself)"
        }
        EscalationReason::NeedsMoreCode => "the answer depends on code neither reviewer was shown",
    }
}

fn outlier_meaning(r: OutlierReason) -> &'static str {
    match r {
        OutlierReason::SeverityRaiseHeld => {
            "the per-site reviewer wanted a higher severity and was held at the original"
        }
        OutlierReason::DifferentObject => "it concerns a different object than the rest",
        OutlierReason::DifferentConstruct => "it uses a different code construct than the rest",
        OutlierReason::OnlySiteInFile => "it is the only site in its file",
        OutlierReason::Stage2Outlier => "it does not belong with the rest, for another reason",
        OutlierReason::PreconditionAdded => {
            "its explanation adds a precondition the code does not show"
        }
        OutlierReason::UnverifiedName => {
            "its explanation names something not found in the evidence"
        }
        OutlierReason::FixUngrounded => "its fix names something not found in the repository",
        OutlierReason::Contradiction => "its fix contradicts its own explanation",
        OutlierReason::EvidenceUnverified => "its quoted code was not found in the file",
        OutlierReason::InTestCode => "it is in test code",
        OutlierReason::CouldBeIntentional => "it could be deliberate",
        OutlierReason::MethodologyLeak => "its text names the audit tool or process",
    }
}

/// The line added to both stage 2 prompts on the CLI backend. There the model is Claude Code with
/// every tool switched off, and one reply on a real run was a transcript of tool calls instead of
/// the JSON object (nothing ran). It is a suffix, so the base prompt stays the prefix of the
/// full one.
pub(crate) const NO_TOOLS_LINE: &str = "\nNo tools exist in this conversation. You cannot read \
    files or run commands, and a tool call you write does nothing. Your whole reply is the one \
    JSON object described above, with no tool call, no transcript and no text before or after \
    it.\n";

/// The group reviewer's system prompt for the backend in use.
pub(crate) fn group_review_system_prompt_for(cli_backend: bool) -> String {
    let mut p = group_review_system_prompt();
    if cli_backend {
        p.push_str(NO_TOOLS_LINE);
    }
    p
}

/// The rule-level summary prompt for the backend in use.
pub(crate) fn rule_summary_system_prompt_for(cli_backend: bool) -> String {
    let mut p = rule_summary_system_prompt();
    if cli_backend {
        p.push_str(NO_TOOLS_LINE);
    }
    p
}

/// The system prompt of the rule-level summary call.
pub(crate) fn rule_summary_system_prompt() -> String {
    String::from(
        "You are the group reviewer, the architect's first reader of this rule group. The group \
         was too large to read at once, so it was read in parts. Each part has a summary and the \
         sub-groups it was sorted into. Write the one summary the architect reads at the rule: \
         what the sites have in common, what is wrong, and what he should do about it. He reviews \
         patterns, not every site, and he sees everything; you decide what needs his attention \
         most. In the summary, name every site that does not share the cause you state, by its \
         `path:line` exactly as listed. Do not state how many sites there are or how many share a \
         cause: the report counts them itself, and a count in your prose is checked against it.\n\n\
         Answer with one JSON object and nothing else: {\"summary\": \"...\"}\n",
    )
}

// ── Reading a reply ────────────────────────────────────────────────────────────────────────

/// A reply from the group reviewer, read and checked against the closed lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage2Reply {
    pub summary: String,
    pub subgroups: Vec<ReplySubgroup>,
    pub outliers: Vec<ReplyOutlier>,
    pub escalations: Vec<ReplyEscalation>,
    pub disagreements: Vec<ReplyDisagreement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplySubgroup {
    pub label: String,
    pub cause: String,
    pub summary: String,
    pub members: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyOutlier {
    pub index: usize,
    pub reason: OutlierReason,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyEscalation {
    pub index: usize,
    pub reason: EscalationReason,
    /// The order within the group, 1 first. 0 when the reply left it out (see `rank_defaulted`):
    /// the tree sorts rank 0 after every ranked escalation and keeps the engine's order among
    /// them.
    pub rank: u32,
    /// The reply gave no `rank`; the engine's order stands and the ledger says so.
    pub rank_defaulted: bool,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyDisagreement {
    pub index: usize,
    pub field: DisagreementField,
    pub stage1: String,
    pub mine: String,
    pub why: String,
}

type Obj = serde_json::Map<String, serde_json::Value>;

/// `value` as an object with exactly the fields `allowed` (all required).
fn exact_object<'v>(
    value: &'v serde_json::Value,
    allowed: &[&str],
    at: &str,
) -> Result<&'v Obj, String> {
    object_with(value, allowed, &[], at)
}

/// `value` as an object whose fields are `required` plus any of `optional`.
fn object_with<'v>(
    value: &'v serde_json::Value,
    required: &[&str],
    optional: &[&str],
    at: &str,
) -> Result<&'v Obj, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("{at} is not an object"))?;
    for key in obj.keys() {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(format!(
                "{at} has a field the schema does not have: `{key}`"
            ));
        }
    }
    for key in required {
        if !obj.contains_key(*key) {
            return Err(format!("{at} is missing the field `{key}`"));
        }
    }
    Ok(obj)
}

fn text_of(obj: &Obj, key: &str, at: &str) -> Result<String, String> {
    obj[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{at}.{key} is not a string"))
}

/// A text field the reviewer wrote for the client to read, redacted on the way in. Every parse
/// of a reply (a live one, a stored one a resume replays) goes through here, so no summary,
/// label, cause or reason reaches the tree or a row with a secret in it. The same detector the
/// release gate runs does the redacting, so what ships passes the gate.
fn prose_of(obj: &Obj, key: &str, at: &str) -> Result<String, String> {
    text_of(obj, key, at).map(|text| {
        // Redacted first, then rewritten for the client: no em dash, no en dash, no
        // multiplication sign, and a problem in a whole file is "whole file", not `path:0`.
        crate::report_export::client_prose(&camerata_secrets::redact_str(&text))
    })
}

fn index_of(obj: &Obj, key: &str, at: &str) -> Result<usize, String> {
    obj[key]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("{at}.{key} is not a whole number from zero up"))
}

fn array_of<'v>(obj: &'v Obj, key: &str) -> Result<&'v Vec<serde_json::Value>, String> {
    obj[key]
        .as_array()
        .ok_or_else(|| format!("`{key}` is not an array"))
}

const SEVERITY_LABELS: [&str; 4] = ["critical", "high", "medium", "low"];
const REAL_LABELS: [&str; 3] = ["yes", "no", "unclear"];

impl Stage2Reply {
    /// Read a reply. `Err` carries the reason it is turned down: not JSON, a field the schema
    /// does not have, a reason outside the closed lists, a value of the wrong kind. Whether an
    /// index names a member of the group is not judged here (see [`apply_rule`]).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let json = extract_json_object(raw)
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .ok_or_else(|| "the reply is not a JSON object".to_string())?;
        let top = exact_object(
            &json,
            &[
                "summary",
                "subgroups",
                "outliers",
                "escalations",
                "disagreements",
            ],
            "the reply",
        )?;
        let summary = prose_of(top, "summary", "the reply")?;

        let mut subgroups = Vec::new();
        for (i, v) in array_of(top, "subgroups")?.iter().enumerate() {
            let at = format!("subgroups[{i}]");
            let o = exact_object(v, &["label", "cause", "summary", "members"], &at)?;
            let members = array_of(o, "members")
                .map_err(|e| format!("{at}: {e}"))?
                .iter()
                .map(|m| {
                    m.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| format!("{at}.members holds a value that is not an index"))
                })
                .collect::<Result<Vec<usize>, String>>()?;
            subgroups.push(ReplySubgroup {
                label: prose_of(o, "label", &at)?,
                cause: prose_of(o, "cause", &at)?,
                summary: prose_of(o, "summary", &at)?,
                members,
            });
        }

        let mut outliers = Vec::new();
        for (i, v) in array_of(top, "outliers")?.iter().enumerate() {
            let at = format!("outliers[{i}]");
            let o = exact_object(v, &["index", "reason", "why"], &at)?;
            let reason_name = text_of(o, "reason", &at)?;
            let reason = OutlierReason::parse(&reason_name)
                .ok_or_else(|| format!("{at}.reason `{reason_name}` is not on the outlier list"))?;
            outliers.push(ReplyOutlier {
                index: index_of(o, "index", &at)?,
                reason,
                why: prose_of(o, "why", &at)?,
            });
        }

        let mut escalations = Vec::new();
        for (i, v) in array_of(top, "escalations")?.iter().enumerate() {
            let at = format!("escalations[{i}]");
            // A reply that leaves `rank` out is read, not refused: the rank only orders the
            // escalations of one group, and a refusal costs a whole call (4 of 90 on a real run).
            let o = object_with(v, &["index", "reason", "why"], &["rank"], &at)?;
            let reason_name = text_of(o, "reason", &at)?;
            let reason = EscalationReason::parse(&reason_name).ok_or_else(|| {
                format!("{at}.reason `{reason_name}` is not on the escalation list")
            })?;
            let (rank, rank_defaulted) = match o.get("rank") {
                None | Some(serde_json::Value::Null) => (0, true),
                Some(v) => (
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| format!("{at}.rank is not a whole number"))?,
                    false,
                ),
            };
            escalations.push(ReplyEscalation {
                index: index_of(o, "index", &at)?,
                reason,
                rank,
                rank_defaulted,
                why: prose_of(o, "why", &at)?,
            });
        }

        let mut disagreements = Vec::new();
        for (i, v) in array_of(top, "disagreements")?.iter().enumerate() {
            let at = format!("disagreements[{i}]");
            let o = exact_object(v, &["index", "field", "stage1", "mine", "why"], &at)?;
            let field_name = text_of(o, "field", &at)?;
            let field = DisagreementField::parse(&field_name).ok_or_else(|| {
                format!("{at}.field `{field_name}` is neither `severity` nor `real`")
            })?;
            let mine = text_of(o, "mine", &at)?.trim().to_ascii_lowercase();
            let allowed: &[&str] = match field {
                DisagreementField::Severity => &SEVERITY_LABELS,
                DisagreementField::Real => &REAL_LABELS,
            };
            if !allowed.contains(&mine.as_str()) {
                return Err(format!(
                    "{at}.mine `{mine}` is not a {} value",
                    field.as_str()
                ));
            }
            disagreements.push(ReplyDisagreement {
                index: index_of(o, "index", &at)?,
                field,
                stage1: prose_of(o, "stage1", &at)?,
                mine,
                why: prose_of(o, "why", &at)?,
            });
        }

        Ok(Self {
            summary,
            subgroups,
            outliers,
            escalations,
            disagreements,
        })
    }
}

/// The rule-level summary reply: exactly `{"summary": "..."}`.
pub(crate) fn parse_rule_summary(raw: &str) -> Result<String, String> {
    let json = extract_json_object(raw)
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .ok_or_else(|| "the reply is not a JSON object".to_string())?;
    let o = exact_object(&json, &["summary"], "the reply")?;
    prose_of(o, "summary", "the reply")
}

// ── Chunks ─────────────────────────────────────────────────────────────────────────────────

/// Cut a rule node's members into the chunks one call each is asked about, along the node's
/// sub-group hints: `hints` are the members (by index) of each hint sub-group, in order. A hint
/// sub-group stays whole when it fits; several small ones are packed into a chunk together; one
/// bigger than `limit` is cut in order. Every member is in exactly one chunk and no chunk is
/// bigger than `limit`.
pub fn chunk_plan(hints: &[Vec<usize>], limit: usize) -> Vec<Vec<usize>> {
    let limit = limit.max(1);
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    for hint in hints {
        if hint.is_empty() {
            continue;
        }
        if hint.len() > limit {
            // Too big to keep whole: close the chunk being filled and cut this one in order.
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            for piece in hint.chunks(limit) {
                chunks.push(piece.to_vec());
            }
            continue;
        }
        if current.len() + hint.len() > limit {
            chunks.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(hint);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

// ── The summary check (A9) ─────────────────────────────────────────────────────────────────

/// The facts a summary's prose is checked against (A9). All come from the tree and the rows.
#[derive(Debug, Clone, Default)]
pub struct SummaryFacts {
    /// Sites in the rule group.
    pub sites: usize,
    /// The size of each sub-group.
    pub subgroup_sizes: Vec<usize>,
    /// Sites with an outlier mark.
    pub outliers: usize,
    /// Sites the group reviewer escalated.
    pub escalations: usize,
    /// Distinct files the sites are in.
    pub files: usize,
    /// Sites no sub-group named.
    pub unplaced: usize,
    /// Members the summary has to name: `(site key, path, line, index)`.
    pub must_name: Vec<(String, String, usize, usize)>,
}

impl SummaryFacts {
    /// Every count the tree can back, so a stated count that matches none of them is a mismatch.
    fn known_counts(&self) -> BTreeSet<usize> {
        let mut known: BTreeSet<usize> = BTreeSet::new();
        known.insert(self.sites);
        known.insert(self.outliers);
        known.insert(self.escalations);
        known.insert(self.files);
        known.insert(self.unplaced);
        known.insert(self.sites.saturating_sub(self.outliers));
        for &size in &self.subgroup_sizes {
            known.insert(size);
            known.insert(self.sites.saturating_sub(size));
        }
        known
    }
}

const NUMBER_WORDS: [(&str, usize); 20] = [
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("thirteen", 13),
    ("fourteen", 14),
    ("fifteen", 15),
    ("sixteen", 16),
    ("seventeen", 17),
    ("eighteen", 18),
    ("nineteen", 19),
    ("twenty", 20),
];

/// The review flags a summary earns against the data (A9): `summary_count_mismatch:<claim>` for a
/// count it states that no count of the tree backs, and `summary_omits_member:<site key>` for a
/// site that differs from the stated cause (an outlier by cause, or one no sub-group named) that
/// the summary does not name by `path:line`, by path or by `#index`.
pub fn check_summary(summary: &str, facts: &SummaryFacts) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    let known = facts.known_counts();
    let re = regex::Regex::new(
        r"(?i)\b(\d[\d,]*|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty)\s+(?:[a-z-]+\s+){0,2}?(sites?|findings?|members?|occurrences?|instances?|locations?|places?|files?|violations?)\b",
    )
    .expect("a static pattern");
    for caps in re.captures_iter(summary) {
        let word = caps[1].to_ascii_lowercase();
        let n = word.replace(',', "").parse::<usize>().ok().or_else(|| {
            NUMBER_WORDS
                .iter()
                .find(|(w, _)| *w == word)
                .map(|(_, n)| *n)
        });
        let Some(n) = n else { continue };
        if !known.contains(&n) {
            let claim = format!("{n} {}", caps[2].to_ascii_lowercase());
            let flag = format!(
                "summary_count_mismatch:{claim} (the report has {} sites)",
                facts.sites
            );
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        }
    }
    for (key, path, line, index) in &facts.must_name {
        let named = summary.contains(&format!("{path}:{line}"))
            || summary.contains(path.as_str())
            || summary.contains(&format!("#{index}"))
            || summary.contains(&format!("[{index}]"));
        if !named {
            flags.push(format!("summary_omits_member:{key}"));
        }
    }
    flags
}

// ── The work: rule groups, members, hints ──────────────────────────────────────────────────

/// One member of a rule group, as the model is told about it.
#[derive(Debug, Clone)]
struct Member {
    /// Its position in the group (the index the model uses).
    idx: usize,
    /// Its row in the report's findings.
    row: usize,
    key: String,
    path: String,
    line: usize,
}

/// A sub-group the grouping pass formed, offered to the model as a hint.
#[derive(Debug, Clone)]
struct Hint {
    label: String,
    cause: Option<String>,
    idxs: Vec<usize>,
}

/// One rule group to read.
#[derive(Debug, Clone)]
struct RuleWork {
    node_id: String,
    label: String,
    rule_ids: Vec<String>,
    members: Vec<Member>,
    hints: Vec<Hint>,
    /// The member indexes each call is asked about; set when the requests are planned.
    chunks: Vec<Vec<usize>>,
}

fn build_works(report: &ScanReport) -> Vec<RuleWork> {
    let Some(tree) = report.tree.as_ref() else {
        return Vec::new();
    };
    let mut works = Vec::new();
    for node in tree
        .nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Rule && !is_thrown_out_id(&n.id))
    {
        let mut rows: Vec<usize> = report
            .findings
            .iter()
            .enumerate()
            .filter(|(_, f)| f.group_path.get(1).map(String::as_str) == Some(node.id.as_str()))
            .map(|(i, _)| i)
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by(|&a, &b| {
            let (fa, fb) = (&report.findings[a], &report.findings[b]);
            (&fa.repo, &fa.path, fa.line, &fa.site_key).cmp(&(
                &fb.repo,
                &fb.path,
                fb.line,
                &fb.site_key,
            ))
        });
        let members: Vec<Member> = rows
            .iter()
            .enumerate()
            .map(|(idx, &row)| {
                let f = &report.findings[row];
                Member {
                    idx,
                    row,
                    key: f.site_key.clone(),
                    path: f.path.clone(),
                    line: f.line,
                }
            })
            .collect();
        let idx_of: HashMap<&str, usize> =
            members.iter().map(|m| (m.key.as_str(), m.idx)).collect();
        let mut hints: Vec<Hint> = tree
            .nodes
            .iter()
            .filter(|n| n.level == NodeLevel::Subgroup && n.parent.as_deref() == Some(&node.id))
            .map(|n| Hint {
                label: n.label.clone(),
                cause: n.cause.clone(),
                idxs: n
                    .members
                    .iter()
                    .filter_map(|k| idx_of.get(k.as_str()).copied())
                    .collect(),
            })
            .filter(|h| !h.idxs.is_empty())
            .collect();
        // A member the tree's sub-groups do not list still belongs to a chunk.
        let listed: BTreeSet<usize> = hints.iter().flat_map(|h| h.idxs.iter().copied()).collect();
        let rest: Vec<usize> = (0..members.len()).filter(|i| !listed.contains(i)).collect();
        if !rest.is_empty() {
            hints.push(Hint {
                label: "Not in a sub-group".to_string(),
                cause: None,
                idxs: rest,
            });
        }
        works.push(RuleWork {
            node_id: node.id.clone(),
            label: node.label.clone(),
            rule_ids: node.rule_ids.clone(),
            members,
            hints,
            chunks: Vec::new(),
        });
    }
    works
}

// ── The requests ───────────────────────────────────────────────────────────────────────────

/// What the prompts need to know about the run.
struct PromptContext<'a> {
    repo_shape: &'a str,
    repo: String,
    model: &'a str,
    nonce: crate::prompt_frame::FrameNonce,
    system: String,
    rule_system: String,
    rules: HashMap<String, &'a str>,
}

/// The stage 1 and review-flag facts about one member, as one line of the member list.
pub(super) fn member_line(f: &Finding, idx: usize, with_repo: bool) -> String {
    let stage1 = stage1_of(f);
    let real = stage1.as_ref().map_or("not rated", |s| s.real.as_str());
    let reason: String = stage1
        .as_ref()
        .map(|s| s.reason.chars().take(REASON_CHARS).collect())
        .unwrap_or_default();
    let mut flags: Vec<String> = crate::review_flags::flags_of(f);
    if f.captures
        .contains_key(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE)
    {
        flags.push("evidence_unverified".to_string());
    }
    for r in outlier_reasons_of(f) {
        flags.push(r.as_str().to_string());
    }
    let place = if with_repo {
        format!("{}:{}:{}", f.repo, f.path, f.line)
    } else {
        format!("{}:{}", f.path, f.line)
    };
    // A rule's declared minimum can raise the shown severity above what the per-site reviewer
    // rated; say so, so the group reviewer never reads the minimum as that reviewer's call.
    let severity = match stage1.as_ref().and_then(|s| s.severity.as_deref()) {
        Some(rated) if !rated.eq_ignore_ascii_case(&f.severity) => {
            format!(
                "{} (floor after review; per-site reviewer: {rated})",
                f.severity
            )
        }
        _ => f.severity.clone(),
    };
    let mut line = format!(
        "{idx}: {place} | {severity} | real: {real} | effort: {} | stage 1: {} | {}",
        f.effort.as_deref().unwrap_or("-"),
        reason.split_whitespace().collect::<Vec<_>>().join(" "),
        finding_headline(f),
    );
    if !flags.is_empty() {
        line.push_str(&format!(" | flags: {}", flags.join(", ")));
    }
    camerata_secrets::redact_str(&line).into_owned()
}

/// The user prompt for one chunk of a rule group.
fn chunk_prompt(
    ctx: &PromptContext<'_>,
    work: &RuleWork,
    findings: &[Finding],
    chunk: &[usize],
    part: Option<(usize, usize)>,
) -> String {
    let with_repo = findings
        .iter()
        .map(|f| f.repo.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    let mut p = format!("Repository: {}\n{}\n", ctx.repo, ctx.repo_shape);
    p.push_str(&format!("Rule group: {}\n", work.label));
    p.push_str(&format!("Rule group node: {}\n", work.node_id));
    if let Some((i, n)) = part {
        p.push_str(&format!(
            "Part {i} of {n} of this rule group. Place only the sites listed below; the other \
             parts are read in their own calls.\n"
        ));
    }
    p.push_str(&format!(
        "Rules in this group: {}\n",
        work.rule_ids.join(", ")
    ));
    for id in &work.rule_ids {
        if let Some(text) = ctx.rules.get(&id.to_ascii_uppercase()) {
            p.push_str(&format!("\nRule {id}: {text}\n"));
        }
    }
    p.push_str(
        "\nThe sites. Each line: index, place, severity, whether the per-site reviewer found it \
         real, effort, its one-line reason, the finding's headline, and any flags the report \
         raised on it.\n",
    );
    let lines: String = chunk
        .iter()
        .map(|&idx| {
            let m = &work.members[idx];
            member_line(&findings[m.row], idx, with_repo)
        })
        .collect::<Vec<_>>()
        .join("\n");
    p.push_str(&crate::prompt_frame::frame_untrusted(
        &ctx.nonce,
        &format!("sites of {}", work.node_id),
        &lines,
    ));
    p.push_str(
        "\nHint sub-groups (sites that simple shared-cause tests grouped; you may regroup):\n",
    );
    let in_chunk: BTreeSet<usize> = chunk.iter().copied().collect();
    let mut any = false;
    for h in &work.hints {
        let idxs: Vec<String> = h
            .idxs
            .iter()
            .filter(|i| in_chunk.contains(i))
            .map(ToString::to_string)
            .collect();
        if idxs.is_empty() {
            continue;
        }
        any = true;
        match &h.cause {
            Some(cause) => p.push_str(&format!("- {} ({cause}): {}\n", h.label, idxs.join(", "))),
            None => p.push_str(&format!("- {}: {}\n", h.label, idxs.join(", "))),
        }
    }
    if !any {
        p.push_str("- none\n");
    }
    p
}

/// The request for one chunk.
fn chunk_request(
    ctx: &PromptContext<'_>,
    work: &RuleWork,
    findings: &[Finding],
    chunk: &[usize],
    part: Option<(usize, usize)>,
) -> LlmRequest {
    with_stage_effort(
        LlmRequest::new(chunk_prompt(ctx, work, findings, chunk, part))
            .with_system(ctx.system.clone())
            .with_max_tokens(staged_max_tokens(
                chunk.len(),
                OUTPUT_TOKENS_PER_MEMBER,
                camerata_ui_core::scan::GROUP_REVIEW_EFFORT,
            ))
            .with_temperature(AUDIT_TEMPERATURE)
            .with_response_schema(reply_schemas::stage2_reply_schema())
            .with_model(stage_model(Some(ctx.model))),
        Stage::GroupReview,
    )
}

/// Plan the chunks of every rule group and build their requests. A chunk whose request does not
/// fit the model's window is split in halves until it does (A7); one that does not fit even with
/// a single member is left whole, so the runner records it as a failed part instead of sending it.
fn plan_chunks(
    ctx: &PromptContext<'_>,
    works: &mut [RuleWork],
    findings: &[Finding],
) -> Vec<(String, usize, Vec<usize>, LlmRequest)> {
    let mut out = Vec::new();
    for (wi, work) in works.iter_mut().enumerate() {
        let hints: Vec<Vec<usize>> = work.hints.iter().map(|h| h.idxs.clone()).collect();
        let mut pending: Vec<Vec<usize>> = chunk_plan(&hints, MAX_CHUNK_MEMBERS);
        pending.reverse();
        let mut final_chunks: Vec<Vec<usize>> = Vec::new();
        while let Some(chunk) = pending.pop() {
            let fits = |c: &[usize]| {
                request_budget::ensure_request_fits(&chunk_request(ctx, work, findings, c, None))
                    .is_ok()
            };
            if chunk.len() > 1 && !fits(&chunk) && fits(&chunk[..1]) {
                let mid = chunk.len() / 2;
                pending.push(chunk[mid..].to_vec());
                pending.push(chunk[..mid].to_vec());
            } else {
                final_chunks.push(chunk);
            }
        }
        let n = final_chunks.len();
        for (ci, chunk) in final_chunks.iter().enumerate() {
            let part = (n > 1).then_some((ci + 1, n));
            let req = chunk_request(ctx, work, findings, chunk, part);
            out.push((format!("{}#{ci}", work.node_id), wi, chunk.clone(), req));
        }
        work.chunks = final_chunks;
    }
    out
}

// ── The stage's items ──────────────────────────────────────────────────────────────────────

/// A note appended to a request that is asked a second time, saying why the first reply was
/// turned down.
fn retry_note(reject: &ItemReject) -> String {
    format!(
        "\n\nYour previous reply was refused: {}. Answer again with one JSON object that has \
         exactly the fields described, every reason taken from the closed lists.\n",
        reject.reason
    )
}

/// The retry of a request: the same request with the refusal appended, and twice the output room
/// when the reply was cut off.
fn retried(req: &LlmRequest, reject: &ItemReject, truncated: bool) -> Option<LlmRequest> {
    let mut again = req.clone();
    again.prompt.push_str(&retry_note(reject));
    if truncated {
        let raised = again
            .max_tokens
            .saturating_mul(2)
            .min(camerata_ui_core::scan::AGGREGATE_MAX_TOKENS_CEILING);
        if raised <= again.max_tokens {
            return None;
        }
        again.max_tokens = raised;
    }
    request_budget::ensure_request_fits(&again).ok()?;
    Some(again)
}

/// The chunk calls of a group review, in the shape the shared runner takes.
struct ChunkItems {
    requests: BTreeMap<String, LlmRequest>,
    order: Vec<String>,
    replies: BTreeMap<String, Stage2Reply>,
    truncated: BTreeSet<String>,
}

impl StageItems for ChunkItems {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.order
            .iter()
            .map(|id| (id.clone(), self.requests[id].clone()))
            .collect()
    }

    fn absorb(&mut self, id: &String, resp: LlmResponse) -> Result<(), ItemReject> {
        match Stage2Reply::parse(&resp.text) {
            Ok(reply) => {
                self.replies.insert(id.clone(), reply);
                Ok(())
            }
            Err(why) => {
                if resp.truncated {
                    self.truncated.insert(id.clone());
                    Err(ItemReject::new(
                        "the reply was cut off at the output limit before it was complete",
                    ))
                } else {
                    Err(ItemReject::new(why))
                }
            }
        }
    }

    fn retry(&self, id: &String, reject: &ItemReject) -> Option<LlmRequest> {
        let truncated = self.truncated.contains(id);
        retried(self.requests.get(id)?, reject, truncated)
    }
}

/// The rule-level summary calls: one per rule group that was read in more than one chunk.
struct SummaryItems {
    requests: BTreeMap<String, LlmRequest>,
    summaries: BTreeMap<String, String>,
}

impl StageItems for SummaryItems {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.requests
            .iter()
            .map(|(id, req)| (id.clone(), req.clone()))
            .collect()
    }

    fn absorb(&mut self, id: &String, resp: LlmResponse) -> Result<(), ItemReject> {
        match parse_rule_summary(&resp.text) {
            Ok(summary) => {
                self.summaries.insert(id.clone(), summary);
                Ok(())
            }
            Err(why) => Err(ItemReject::new(why)),
        }
    }

    fn retry(&self, id: &String, reject: &ItemReject) -> Option<LlmRequest> {
        retried(self.requests.get(id)?, reject, false)
    }
}

// ── Applying a reply ───────────────────────────────────────────────────────────────────────

/// The severity labels in order, for the distance between two of them.
fn severity_notches(a: &str, b: &str) -> usize {
    let (a, b) = (severity_rank(a), severity_rank(b));
    usize::from(a.abs_diff(b))
}

/// The highest sub-group ordinal under `rule` in `tree`, so new sub-groups never reuse an id a
/// kept placement still names.
fn max_sub_ordinal(tree: &ReviewTree, rule: &str) -> u32 {
    tree.nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Subgroup && n.parent.as_deref() == Some(rule))
        .filter_map(|n| n.id.rsplit('/').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

/// The id of a rule's `unplaced` sub-group.
pub fn unplaced_sub_id(rule_node_id: &str) -> String {
    format!(
        "sub:{}/unplaced",
        rule_node_id.strip_prefix("rule:").unwrap_or(rule_node_id)
    )
}

/// The label of every rule's `unplaced` sub-group.
const UNPLACED_LABEL: &str = "Not placed by the group reviewer";
/// Its cause.
const UNPLACED_CAUSE: &str =
    "the group reviewer's sub-groups did not name these sites, so they are kept together here";

/// What applying one rule group's replies changed.
#[derive(Debug, Default)]
struct RuleApplied {
    unplaced: Vec<String>,
    stray_indexes: usize,
    /// Site keys whose escalation came back without a `rank`.
    ranks_defaulted: Vec<String>,
}

/// One chunk's reply, with the indexes the chunk was asked about.
struct ChunkReply<'a> {
    idxs: &'a [usize],
    reply: Option<&'a Stage2Reply>,
}

/// Write the replies of one rule group onto the rows and into `tree` (the tree the final
/// `build_tree` starts from). Members of a chunk with no reply keep the place they had.
fn apply_rule(
    work: &RuleWork,
    chunks: &[ChunkReply<'_>],
    rule_summary: Option<String>,
    findings: &mut [Finding],
    tree: &mut ReviewTree,
) -> RuleApplied {
    let mut applied = RuleApplied::default();
    let rule_id = work.node_id.as_str();

    // Sub-groups: first placement of an index wins, an index the chunk was not asked about is
    // counted and ignored, and an index the reply leaves out is unplaced.
    struct NewSub {
        label: String,
        cause: String,
        summary: String,
        idxs: Vec<usize>,
    }
    let mut new_subs: Vec<NewSub> = Vec::new();
    let mut placed: BTreeSet<usize> = BTreeSet::new();
    let mut unplaced: Vec<usize> = Vec::new();
    let mut reviewed: BTreeSet<usize> = BTreeSet::new();
    for chunk in chunks {
        let Some(reply) = chunk.reply else { continue };
        let asked: BTreeSet<usize> = chunk.idxs.iter().copied().collect();
        reviewed.extend(asked.iter().copied());
        for sg in &reply.subgroups {
            let mut idxs = Vec::new();
            for &i in &sg.members {
                if !asked.contains(&i) {
                    applied.stray_indexes += 1;
                } else if placed.insert(i) {
                    idxs.push(i);
                }
            }
            if !idxs.is_empty() {
                new_subs.push(NewSub {
                    label: sg.label.clone(),
                    cause: sg.cause.clone(),
                    summary: sg.summary.clone(),
                    idxs,
                });
            }
        }
        for &i in chunk.idxs {
            if !placed.contains(&i) {
                unplaced.push(i);
            }
        }
    }
    if reviewed.is_empty() {
        return applied;
    }

    // The place of every reviewed member.
    let base = max_sub_ordinal(tree, rule_id);
    let mut sub_of: HashMap<usize, String> = HashMap::new();
    let mut new_nodes: Vec<TreeNode> = Vec::new();
    let mut label_cause_of: HashMap<usize, (String, String)> = HashMap::new();
    // A sub-group whose summary the architect edited keeps its node (and so the edit) when the
    // new sub-group has exactly its members; an edit never vanishes silently (A15).
    let edited: Vec<(BTreeSet<&str>, &TreeNode)> = tree
        .nodes
        .iter()
        .filter(|n| {
            n.level == NodeLevel::Subgroup
                && n.parent.as_deref() == Some(rule_id)
                && n.summary_source == TextSource::Reviewer
                && !n.members.is_empty()
        })
        .map(|n| (n.members.iter().map(String::as_str).collect(), n))
        .collect();
    let mut sub_members: HashMap<String, Vec<usize>> = HashMap::new();
    let mut kept_edits: HashMap<String, (String, TextSource)> = HashMap::new();
    for (n, sg) in new_subs.iter().enumerate() {
        let keys: BTreeSet<&str> = sg
            .idxs
            .iter()
            .map(|&i| work.members[i].key.as_str())
            .collect();
        let reused = edited.iter().find(|(members, _)| *members == keys);
        let id = match reused {
            Some((_, old)) => {
                kept_edits.insert(
                    old.id.clone(),
                    (old.summary.clone().unwrap_or_default(), old.summary_source),
                );
                old.id.clone()
            }
            None => crate::review_tree::sub_id(rule_id, base + 1 + n as u32),
        };
        for &i in &sg.idxs {
            sub_of.insert(i, id.clone());
            label_cause_of.insert(i, (sg.label.clone(), sg.cause.clone()));
        }
        sub_members.insert(id.clone(), sg.idxs.clone());
        let mut node = sub_node(&id, rule_id, &sg.label, &sg.cause);
        node.summary = Some(sg.summary.clone());
        if let Some((text, source)) = kept_edits.get(&id) {
            node.summary = Some(text.clone());
            node.summary_source = *source;
        }
        new_nodes.push(node);
    }
    if !unplaced.is_empty() {
        let id = unplaced_sub_id(rule_id);
        for &i in &unplaced {
            sub_of.insert(i, id.clone());
            label_cause_of.insert(i, (UNPLACED_LABEL.to_string(), UNPLACED_CAUSE.to_string()));
            applied.unplaced.push(work.members[i].key.clone());
        }
        if !new_nodes.iter().any(|n| n.id == id) {
            new_nodes.push(sub_node(&id, rule_id, UNPLACED_LABEL, UNPLACED_CAUSE));
        }
    }
    let reviewed_keys: BTreeSet<&str> = reviewed
        .iter()
        .map(|&i| work.members[i].key.as_str())
        .collect();
    for &i in &reviewed {
        let row = work.members[i].row;
        let f = &mut findings[row];
        if let Some(id) = sub_of.get(&i) {
            if f.group_path.len() == 3 {
                f.group_path[2].clone_from(id);
            }
        }
        // The columns this stage owns are rewritten from this reply alone.
        let (label, cause) = label_cause_of.get(&i).cloned().unwrap_or_default();
        f.review.subgroup_label = label;
        f.review.subgroup_cause = cause;
        f.review.outlier_reason = None;
        f.review.outlier_why.clear();
        f.review.stage2_disagreement.clear();
    }

    // What the model said about single sites.
    let mut outliers_by_sub: BTreeMap<String, Vec<Outlier>> = BTreeMap::new();
    let mut escalations: Vec<NodeEscalation> = Vec::new();
    let mut seen_outlier: BTreeSet<usize> = BTreeSet::new();
    for chunk in chunks {
        let Some(reply) = chunk.reply else { continue };
        let asked: BTreeSet<usize> = chunk.idxs.iter().copied().collect();
        for o in &reply.outliers {
            if !asked.contains(&o.index) {
                applied.stray_indexes += 1;
                continue;
            }
            if !seen_outlier.insert(o.index) {
                continue;
            }
            let m = &work.members[o.index];
            let f = &mut findings[m.row];
            f.review.outlier_reason = Some(o.reason);
            f.review.outlier_why.clone_from(&o.why);
            if let Some(sub) = sub_of.get(&o.index) {
                outliers_by_sub
                    .entry(sub.clone())
                    .or_default()
                    .push(Outlier {
                        key: m.key.clone(),
                        reason: o.reason,
                        by: OutlierBy::Model,
                        why: o.why.clone(),
                    });
            }
        }
        let mut seen_escalation: BTreeSet<usize> = BTreeSet::new();
        for e in &reply.escalations {
            if !asked.contains(&e.index) {
                applied.stray_indexes += 1;
                continue;
            }
            if e.rank_defaulted {
                let key = work.members[e.index].key.clone();
                if !applied.ranks_defaulted.contains(&key) {
                    applied.ranks_defaulted.push(key);
                }
            }
            // One reason per site: the highest-ranked one the reply gave. Rank 0 (none given)
            // sorts after every ranked one, as the tree does.
            let order = |reason: EscalationReason, rank: u32| {
                (reason.rank(), if rank == 0 { u32::MAX } else { rank })
            };
            if !seen_escalation.insert(e.index) {
                if let Some(have) = escalations
                    .iter_mut()
                    .find(|x| x.key == work.members[e.index].key)
                {
                    if order(e.reason, e.rank) < order(have.reason, have.rank) {
                        have.reason = e.reason;
                        have.rank = e.rank;
                        have.why.clone_from(&e.why);
                    }
                }
                continue;
            }
            escalations.push(NodeEscalation {
                key: work.members[e.index].key.clone(),
                reason: e.reason,
                rank: e.rank,
                why: e.why.clone(),
            });
        }
        let mut seen_field: BTreeSet<(usize, &'static str)> = BTreeSet::new();
        for d in &reply.disagreements {
            if !asked.contains(&d.index) {
                applied.stray_indexes += 1;
                continue;
            }
            if !seen_field.insert((d.index, d.field.as_str())) {
                continue;
            }
            let m = &work.members[d.index];
            let f = &mut findings[m.row];
            // Stage 1's own value, read from the row, whatever the reply quotes.
            let stage1_value = match d.field {
                // Stage 1's own rating, not the severity a severity floor raised it to; a run
                // saved before that was kept falls back to the row's severity.
                DisagreementField::Severity => stage1_of(f)
                    .and_then(|s| s.severity)
                    .unwrap_or_else(|| f.severity.clone()),
                DisagreementField::Real => stage1_of(f)
                    .map(|s| s.real.as_str().to_string())
                    .unwrap_or_default(),
            };
            let conflicts = match d.field {
                DisagreementField::Severity => severity_notches(&stage1_value, &d.mine) >= 2,
                DisagreementField::Real => !stage1_value.is_empty() && stage1_value != d.mine,
            };
            let differs = stage1_value != d.mine;
            if !differs {
                continue;
            }
            f.review
                .stage2_disagreement
                .push(camerata_api_types::review::Stage2Disagreement {
                    field: d.field,
                    stage1: stage1_value,
                    mine: d.mine.clone(),
                    why: d.why.clone(),
                });
            if conflicts {
                let key = m.key.clone();
                match escalations.iter_mut().find(|x| x.key == key) {
                    Some(have)
                        if have.reason.rank() <= EscalationReason::ConflictingVerdicts.rank() => {}
                    Some(have) => {
                        have.reason = EscalationReason::ConflictingVerdicts;
                        have.why.clone_from(&d.why);
                    }
                    None => escalations.push(NodeEscalation {
                        key,
                        reason: EscalationReason::ConflictingVerdicts,
                        rank: 0,
                        why: d.why.clone(),
                    }),
                }
            }
        }
    }

    // The tree the final build starts from: new sub-groups with their model outliers, the rule's
    // escalations and summary.
    for node in tree
        .nodes
        .iter_mut()
        .filter(|n| n.level == NodeLevel::Subgroup && n.parent.as_deref() == Some(rule_id))
    {
        node.outliers
            .retain(|o| !(o.by == OutlierBy::Model && reviewed_keys.contains(o.key.as_str())));
    }
    for mut node in new_nodes {
        if let Some(outliers) = outliers_by_sub.remove(&node.id) {
            node.outliers = outliers;
        }
        // A9, per sub-group: its summary against the sub-group's own data. An edited summary is
        // the architect's and is not checked.
        if node.summary_source != TextSource::Reviewer {
            if let (Some(text), Some(idxs)) = (&node.summary, sub_members.get(&node.id)) {
                node.flags =
                    sub_summary_flags(text, work, idxs, &node.outliers, &escalations, findings);
            }
        }
        match tree.nodes.iter_mut().find(|n| n.id == node.id) {
            Some(existing) => *existing = node,
            None => tree.nodes.push(node),
        }
    }
    let summary = match chunks.iter().filter_map(|c| c.reply).count() {
        0 => None,
        _ => Some(rule_summary.unwrap_or_else(|| {
            chunks
                .iter()
                .filter_map(|c| c.reply)
                .map(|r| r.summary.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })),
    };
    let escalation_count = escalations.len();
    if let Some(rule) = tree.nodes.iter_mut().find(|n| n.id == rule_id) {
        rule.escalations
            .retain(|e| !reviewed_keys.contains(e.key.as_str()));
        rule.escalations.extend(escalations);
        if let Some(summary) = &summary {
            // A summary the architect edited is his: a rerun does not overwrite it.
            if rule.summary_source != TextSource::Reviewer {
                rule.summary = Some(summary.clone());
                rule.summary_source = TextSource::Model;
            }
        }
    }

    // A9: the summary against the data.
    let flags = match tree.nodes.iter().find(|n| n.id == rule_id) {
        Some(rule) if rule.summary_source == TextSource::Reviewer => Vec::new(),
        Some(rule) => {
            let text = rule.summary.clone().unwrap_or_default();
            summary_flags(&text, work, findings, tree, &sub_of, escalation_count)
        }
        None => Vec::new(),
    };
    if let Some(rule) = tree.nodes.iter_mut().find(|n| n.id == rule_id) {
        if summary.is_some() {
            rule.flags = flags;
        }
    }
    applied
}

fn sub_node(id: &str, rule_id: &str, label: &str, cause: &str) -> TreeNode {
    TreeNode {
        id: id.to_string(),
        level: NodeLevel::Subgroup,
        parent: Some(rule_id.to_string()),
        label: label.to_string(),
        rule_ids: Vec::new(),
        kind: crate::review_tree::NodeKind::Bulk,
        cause: Some(cause.to_string()),
        members: Vec::new(),
        outliers: Vec::new(),
        summary: None,
        summary_source: TextSource::Model,
        escalations: Vec::new(),
        flags: Vec::new(),
    }
}

/// The flags a sub-group's summary earns against its own members: a count it states that the
/// sub-group's data contradicts, and a member that differs in cause (a model outlier) that it does
/// not name.
fn sub_summary_flags(
    summary: &str,
    work: &RuleWork,
    idxs: &[usize],
    outliers: &[Outlier],
    escalations: &[NodeEscalation],
    findings: &[Finding],
) -> Vec<String> {
    let keys: BTreeSet<&str> = idxs.iter().map(|&i| work.members[i].key.as_str()).collect();
    let mut outlier_keys: BTreeSet<&str> = outliers.iter().map(|o| o.key.as_str()).collect();
    for &i in idxs {
        if !crate::review_tree::engine_outlier_reasons(&findings[work.members[i].row]).is_empty() {
            outlier_keys.insert(work.members[i].key.as_str());
        }
    }
    let by_cause: BTreeSet<&str> = outliers
        .iter()
        .filter(|o| o.reason.differs_in_cause())
        .map(|o| o.key.as_str())
        .collect();
    let facts = SummaryFacts {
        sites: idxs.len(),
        subgroup_sizes: vec![idxs.len()],
        outliers: outlier_keys.len(),
        escalations: escalations
            .iter()
            .filter(|e| keys.contains(e.key.as_str()))
            .count(),
        files: idxs
            .iter()
            .map(|&i| work.members[i].path.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        unplaced: 0,
        must_name: idxs
            .iter()
            .map(|&i| &work.members[i])
            .filter(|m| by_cause.contains(m.key.as_str()))
            .map(|m| (m.key.clone(), m.path.clone(), m.line, m.idx))
            .collect(),
    };
    check_summary(summary, &facts)
}

/// The flags a rule group's summary earns against the final placement.
fn summary_flags(
    summary: &str,
    work: &RuleWork,
    findings: &[Finding],
    tree: &ReviewTree,
    sub_of: &HashMap<usize, String>,
    escalations: usize,
) -> Vec<String> {
    // Sub-group sizes: the new ones by index, and the kept ones by their rows' place.
    let mut size_of: BTreeMap<String, usize> = BTreeMap::new();
    for m in &work.members {
        let id = sub_of
            .get(&m.idx)
            .cloned()
            .or_else(|| findings[m.row].group_path.get(2).cloned())
            .unwrap_or_default();
        *size_of.entry(id).or_default() += 1;
    }
    let sub_ids: BTreeSet<&str> = size_of.keys().map(String::as_str).collect();
    let mut outlier_keys: BTreeSet<&str> = BTreeSet::new();
    let mut by_cause: BTreeSet<&str> = BTreeSet::new();
    for n in tree
        .nodes
        .iter()
        .filter(|n| sub_ids.contains(n.id.as_str()))
    {
        for o in &n.outliers {
            outlier_keys.insert(o.key.as_str());
            if o.reason.differs_in_cause() {
                by_cause.insert(o.key.as_str());
            }
        }
    }
    let unplaced_id = unplaced_sub_id(&work.node_id);
    let files: BTreeSet<&str> = work.members.iter().map(|m| m.path.as_str()).collect();
    let must_name: Vec<(String, String, usize, usize)> = work
        .members
        .iter()
        .filter(|m| {
            by_cause.contains(m.key.as_str())
                || sub_of.get(&m.idx).is_some_and(|s| *s == unplaced_id)
        })
        .map(|m| (m.key.clone(), m.path.clone(), m.line, m.idx))
        .collect();
    let facts = SummaryFacts {
        sites: work.members.len(),
        subgroup_sizes: size_of.values().copied().collect(),
        outliers: outlier_keys.len(),
        escalations,
        files: files.len(),
        unplaced: size_of.get(&unplaced_id).copied().unwrap_or(0),
        must_name,
    };
    check_summary(summary, &facts)
}

// ── The run ────────────────────────────────────────────────────────────────────────────────

/// What the group review needs besides the report.
#[derive(Clone)]
pub struct GroupReviewConfig<'a> {
    /// The resolved model ([`resolve_group_review_model`]).
    pub model: &'a str,
    /// One repo-shape sentence, e.g. "This is a Next.js repo with 42 code files."
    pub repo_shape: &'a str,
    /// `(rule id, directive)` for the rule text of each group.
    pub rules: &'a [(String, String)],
    /// The escalation settings. The counted lines are the report's own and replace the value
    /// given here.
    pub rank: RankSettings,
    /// The most calls in flight at once.
    pub max_in_flight: usize,
    /// The model runs through the Claude Code CLI (no tools, but it can drift into agent mode):
    /// the prompts say outright that no tools exist.
    pub cli_backend: bool,
    /// The job the stage reports to: its activity is touched and its progress set as calls end.
    pub job: Option<(&'a crate::jobs::JobStore, &'a str)>,
}

/// What a group review run came to.
#[derive(Debug, Clone, Default)]
pub struct GroupReviewOutcome {
    /// Rule groups every call of which was read.
    pub rules_reviewed: usize,
    /// Rule groups with a call that could not be read (each is a failed part of the report).
    pub rules_failed: usize,
    /// The site keys no sub-group named; they sit in the rule's `unplaced` sub-group.
    pub unplaced: Vec<String>,
    /// Indexes a reply named that are not members of what it was asked about.
    pub stray_indexes: usize,
    /// The site keys whose escalation came back with no `rank`: they keep the engine's order and
    /// sort after every ranked escalation. Also recorded as an operator note in the ledger.
    pub ranks_defaulted: Vec<String>,
    /// `(node id, bytes of the user prompt)` of every call planned, rule-level summary calls
    /// included.
    pub prompt_bytes: Vec<(String, usize)>,
}

/// The user prompt of a rule's summary call: the parts' summaries, the sub-groups they chose
/// (with their sizes) and the sites that do not share their sub-group's cause.
fn summary_prompt(
    ctx: &PromptContext<'_>,
    work: &RuleWork,
    chunk_summaries: &[String],
    chunks: &[ChunkReply<'_>],
) -> String {
    let mut p = format!("Repository: {}\n{}\n", ctx.repo, ctx.repo_shape);
    p.push_str(&format!("Rule group: {}\n", work.label));
    p.push_str(&format!("Rule group node: {}\n", work.node_id));
    p.push_str(&format!(
        "Rules in this group: {}\n",
        work.rule_ids.join(", ")
    ));
    p.push_str("\nThe summaries of the parts, in order:\n");
    let mut text = String::new();
    for (i, s) in chunk_summaries.iter().enumerate() {
        text.push_str(&format!("Part {}: {s}\n", i + 1));
    }
    p.push_str(&crate::prompt_frame::frame_untrusted(
        &ctx.nonce,
        &format!("part summaries of {}", work.node_id),
        &text,
    ));
    p.push_str("\nThe sub-groups the parts were sorted into:\n");
    let mut sizes = String::new();
    let mut outliers = String::new();
    for chunk in chunks {
        let Some(reply) = chunk.reply else { continue };
        let asked: BTreeSet<usize> = chunk.idxs.iter().copied().collect();
        for sg in &reply.subgroups {
            let n = sg.members.iter().filter(|i| asked.contains(i)).count();
            sizes.push_str(&format!("- {} ({n} sites): {}\n", sg.label, sg.cause));
        }
        for o in reply
            .outliers
            .iter()
            .filter(|o| o.reason.differs_in_cause() && asked.contains(&o.index))
        {
            let m = &work.members[o.index];
            outliers.push_str(&format!(
                "- {}:{} ({}): {}\n",
                m.path,
                m.line,
                o.reason.as_str(),
                o.why
            ));
        }
    }
    p.push_str(&crate::prompt_frame::frame_untrusted(
        &ctx.nonce,
        &format!("sub-groups of {}", work.node_id),
        &sizes,
    ));
    if !outliers.is_empty() {
        p.push_str(
            "\nSites that do not share their sub-group's cause. Name each in the summary:\n",
        );
        p.push_str(&crate::prompt_frame::frame_untrusted(
            &ctx.nonce,
            &format!("outliers of {}", work.node_id),
            &outliers,
        ));
    }
    p
}

/// Run the group reviewer over `report`'s tree: one call per rule group (more for a big one),
/// then the escalation formula over what they asked for. Writes the summaries, sub-groups,
/// outliers, escalation candidates and the rows' columns, records the stage and the escalation
/// block in the report's ledger, and lists every call that could not be read as a failed part.
/// Nothing is removed from the report.
pub async fn review_groups(
    report: &mut ScanReport,
    llm: &dyn LlmPort,
    cfg: &GroupReviewConfig<'_>,
    meter: Option<&UsageMeter>,
) -> GroupReviewOutcome {
    let mut outcome = GroupReviewOutcome::default();
    // The tree is made from the rows (this is idempotent) so the rule groups are the report's own.
    report.assign_site_keys();
    let mut works = build_works(report);
    let sites = report.findings.len();
    if works.is_empty() {
        record_ledger(report, cfg, &outcome, sites);
        return outcome;
    }

    // One delimiter token for every call of the run, drawn so it occurs in no text it frames.
    let texts: Vec<String> = report
        .findings
        .iter()
        .flat_map(|f| [f.path.clone(), f.detail.clone(), f.snippet.clone()])
        .collect();
    let avoid: Vec<&str> = texts.iter().map(String::as_str).collect();
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding(&avoid);
    let repo_label = report.repos.join(", ");
    let ctx = PromptContext {
        repo_shape: cfg.repo_shape,
        repo: repo_label.clone(),
        model: cfg.model,
        system: crate::prompt_frame::with_untrusted_clause(
            group_review_system_prompt_for(cfg.cli_backend),
            &nonce,
        ),
        rule_system: crate::prompt_frame::with_untrusted_clause(
            rule_summary_system_prompt_for(cfg.cli_backend),
            &nonce,
        ),
        nonce,
        rules: cfg
            .rules
            .iter()
            .map(|(id, text)| (id.to_ascii_uppercase(), text.as_str()))
            .collect(),
    };

    // Pass A: one call per chunk of every rule group.
    let planned = plan_chunks(&ctx, &mut works, &report.findings);
    let mut chunk_of: BTreeMap<String, (usize, Vec<usize>)> = BTreeMap::new();
    let mut items = ChunkItems {
        requests: BTreeMap::new(),
        order: Vec::new(),
        replies: BTreeMap::new(),
        truncated: BTreeSet::new(),
    };
    for (id, wi, idxs, req) in planned {
        outcome
            .prompt_bytes
            .push((works[wi].node_id.clone(), req.prompt.len()));
        chunk_of.insert(id.clone(), (wi, idxs));
        items.order.push(id.clone());
        items.requests.insert(id, req);
    }
    // With the run's checkpoint folder in scope, every call that is read is written down as it
    // ends, so a restarted run re-asks only the chunks with no result and reports the usage of
    // the ones it skips (design 6.2). The rule-level summary calls have a checkpoint of their own.
    let chunk_checkpoint = crate::checkpoint::stage_checkpoint("group-review");
    let summary_checkpoint = crate::checkpoint::stage_checkpoint("group-review-summary");
    let stage_ctx = StageCtx {
        repo: &repo_label,
        model: cfg.model,
        realtime: llm,
        batch: None,
        meter,
        job: cfg.job,
        checkpoint: chunk_checkpoint.as_ref(),
        settings: StageSettings::default(),
    };
    let mode = StageMode::Parallel {
        max_in_flight: cfg.max_in_flight.max(1),
    };
    let ran = run_stage(Stage::GroupReview, &mut items, mode, &stage_ctx).await;

    // What came back, per rule group.
    let mut failed_reasons: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut failed_items: BTreeMap<usize, Vec<&super::stage_batch::FailedItem<String>>> =
        BTreeMap::new();
    for f in &ran.failed {
        if let Some((wi, _)) = chunk_of.get(&f.id) {
            failed_reasons
                .entry(*wi)
                .or_default()
                .push(f.reason.clone());
            failed_items.entry(*wi).or_default().push(f);
        }
    }
    if let Some(cancelled) = &ran.cancelled {
        for id in &cancelled.not_sent {
            if let Some((wi, _)) = chunk_of.get(id) {
                failed_reasons
                    .entry(*wi)
                    .or_default()
                    .push("the job was cancelled before this part was read".to_string());
            }
        }
    }

    // Pass B: the rule-level summary of a rule group read in more than one chunk, once every one
    // of its chunks has been read.
    let mut summaries = SummaryItems {
        requests: BTreeMap::new(),
        summaries: BTreeMap::new(),
    };
    for work in &works {
        let wi = works
            .iter()
            .position(|w| w.node_id == work.node_id)
            .unwrap_or(0);
        if work.chunks.len() < 2 || failed_reasons.contains_key(&wi) {
            continue;
        }
        let chunk_replies: Vec<ChunkReply<'_>> = work
            .chunks
            .iter()
            .enumerate()
            .map(|(ci, idxs)| ChunkReply {
                idxs,
                reply: items.replies.get(&format!("{}#{ci}", work.node_id)),
            })
            .collect();
        let chunk_summaries: Vec<String> = chunk_replies
            .iter()
            .filter_map(|c| c.reply.map(|r| r.summary.clone()))
            .collect();
        if chunk_summaries.len() != work.chunks.len() {
            continue;
        }
        let req = with_stage_effort(
            LlmRequest::new(summary_prompt(&ctx, work, &chunk_summaries, &chunk_replies))
                .with_system(ctx.rule_system.clone())
                .with_max_tokens(staged_max_tokens(
                    1,
                    2_000,
                    camerata_ui_core::scan::GROUP_REVIEW_EFFORT,
                ))
                .with_temperature(AUDIT_TEMPERATURE)
                .with_response_schema(reply_schemas::rule_summary_reply_schema())
                .with_model(stage_model(Some(cfg.model))),
            Stage::GroupReview,
        );
        outcome
            .prompt_bytes
            .push((work.node_id.clone(), req.prompt.len()));
        summaries.requests.insert(work.node_id.clone(), req);
    }
    let summary_ran = if summaries.requests.is_empty() {
        None
    } else {
        let summary_ctx = StageCtx {
            checkpoint: summary_checkpoint.as_ref(),
            settings: stage_ctx.settings.clone(),
            ..stage_ctx
        };
        Some(run_stage(Stage::GroupReview, &mut summaries, mode, &summary_ctx).await)
    };

    // Apply every rule group's replies to the rows and to the tree the final build starts from.
    let mut tree = report.tree.clone().unwrap_or_default();
    let mut prev_failed: Vec<(usize, String)> = Vec::new();
    for (wi, work) in works.iter().enumerate() {
        let chunk_replies: Vec<ChunkReply<'_>> = work
            .chunks
            .iter()
            .enumerate()
            .map(|(ci, idxs)| ChunkReply {
                idxs,
                reply: items.replies.get(&format!("{}#{ci}", work.node_id)),
            })
            .collect();
        let rule_summary = summaries.summaries.get(&work.node_id).cloned();
        let applied = apply_rule(
            work,
            &chunk_replies,
            rule_summary,
            &mut report.findings,
            &mut tree,
        );
        outcome.unplaced.extend(applied.unplaced);
        outcome.stray_indexes += applied.stray_indexes;
        outcome.ranks_defaulted.extend(applied.ranks_defaulted);
        if failed_reasons.contains_key(&wi) {
            outcome.rules_failed += 1;
        } else {
            outcome.rules_reviewed += 1;
        }
    }
    if let Some(ran) = &summary_ran {
        for f in &ran.failed {
            prev_failed.push((
                works.iter().position(|w| w.node_id == f.id).unwrap_or(0),
                f.reason.clone(),
            ));
        }
    }

    // The failed parts, by name.
    for (wi, reasons) in &failed_reasons {
        let work = &works[*wi];
        let files: Vec<String> = {
            let mut v: Vec<String> = work.members.iter().map(|m| m.path.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        let attempts = failed_items
            .get(wi)
            .map(|items| items.iter().flat_map(|f| f.attempts.clone()).collect())
            .unwrap_or_default();
        let class = failed_items
            .get(wi)
            .and_then(|items| items.first())
            .map_or(FailureClass::Content, |f| f.class);
        report.failed_passes.push(FailedPass {
            repo: repo_label.clone(),
            pass: "group review".to_string(),
            reason: format!(
                "the group reviewer could not read rule group {} ({}), so its {} site{} keep the \
                 placement the grouping gave them and carry no summary or escalation from it: {}",
                work.label,
                work.node_id,
                work.members.len(),
                if work.members.len() == 1 { "" } else { "s" },
                reasons.join("; ")
            ),
            files_not_reviewed: 0,
            class,
            files,
            attempts,
            scope_note: false,
        });
    }
    for (wi, reason) in prev_failed {
        let work = &works[wi];
        report.failed_passes.push(FailedPass {
            repo: repo_label.clone(),
            pass: "group review".to_string(),
            reason: format!(
                "the rule-level summary of {} ({}) could not be written, so its summary is the \
                 parts' summaries as they were: {reason}",
                work.label, work.node_id
            ),
            files_not_reviewed: 0,
            class: FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
            scope_note: false,
        });
    }

    // The final tree: every row in exactly one sub-group, nodes above them, then the line.
    let prev = Some(tree);
    let run_id = report.run_id.clone().unwrap_or_default();
    let mut rebuilt = crate::review_tree::build_tree(
        &mut report.findings,
        &mut report.discarded,
        prev,
        &crate::review_tree::ToolMap::builtin(),
        &run_id,
    );
    let mut settings = cfg.rank.clone();
    settings.counted_lines = report.code_lines;
    let ranked = rank_candidates(&rebuilt.nodes, &report.findings, &settings);
    rebuilt.budget = ranked.budget;
    rebuilt.candidates = ranked.candidates;
    rebuilt.sync_rows(&mut report.findings);
    rebuilt.group_reviewed = outcome.rules_reviewed + outcome.rules_failed > 0;
    report.tree = Some(rebuilt);

    record_ledger(report, cfg, &outcome, sites);
    if !outcome.ranks_defaulted.is_empty() {
        // Never silent: the operator is told which escalations had no rank from the reviewer.
        report.ledger.record_runner_notes(vec![format!(
            "group review: {} escalation(s) came back without a rank and were kept, not refused. \
             They sort after every ranked escalation in the engine's order: {}",
            outcome.ranks_defaulted.len(),
            outcome.ranks_defaulted.join(", ")
        )]);
    }
    outcome
}

/// Record the stage (its model, and that it removed no site) and the escalation block.
fn record_ledger(
    report: &mut ScanReport,
    cfg: &GroupReviewConfig<'_>,
    outcome: &GroupReviewOutcome,
    sites: usize,
) {
    report
        .ledger
        .record_stage_sample(crate::scan_ledger::StageSample {
            stage: STAGE_SAMPLE_NAME.to_string(),
            rows_in: sites,
            rows_out: report.findings.len(),
            accounting: crate::scan_ledger::StageAccounting::default(),
            model: Some(cfg.model.to_string()),
        });
    let tree = report.tree.clone().unwrap_or_default();
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut over_by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for c in tree.candidates.iter().filter(|c| c.escalated) {
        *by_reason.entry(c.reason.as_str().to_string()).or_default() += 1;
        if let Some(why) = c.over_budget_reason {
            *over_by_reason.entry(why.as_str().to_string()).or_default() += 1;
        }
    }
    let budget = tree.budget;
    let total = report.findings.len();
    report
        .ledger
        .record_escalation(crate::scan_ledger::EscalationBlock {
            model: Some(cfg.model.to_string()),
            rate_per_kloc: cfg.rank.rate_per_kloc,
            floor: cfg.rank.floor,
            counted_lines: report.code_lines,
            budget: budget.budget,
            // Both counts are in sites (slots), the unit the budget is in; the rows behind them
            // are stated beside, so the two units never mix in one subtraction.
            candidates: camerata_api_types::review::count_slots(tree.candidates.iter()).0,
            escalated: budget.escalated,
            candidate_rows: tree.candidates.len(),
            escalated_rows: tree.candidates.iter().filter(|c| c.escalated).count(),
            over_budget: budget.over_budget,
            total_sites: total,
            share_escalated: if total == 0 {
                0.0
            } else {
                budget.escalated as f64 / total as f64
            },
            by_reason,
            over_budget_by_reason: over_by_reason,
            over_budget_reasons: cfg
                .rank
                .over_budget_reasons
                .iter()
                .map(|r| r.as_str().to_string())
                .collect(),
            unplaced: outcome.unplaced.clone(),
            rules_reviewed: outcome.rules_reviewed,
            rules_failed: outcome.rules_failed,
        });
}

/// What the stage's meter learned about spend, folded into the scan's report: the spend limit's
/// record, an outage stop and the batch runner's notes.
pub(super) fn fold_spend_into_report(report: &mut ScanReport, meter: &UsageMeter) {
    // The batch runner's notes are kept whether or not a limit was set, and join the scan's.
    report.ledger.record_runner_notes(meter.notes());
    // The spend limit: what it stopped here is a failed part and joins the scan's record.
    if let Some(cap) = meter.cost_cap_record() {
        let has_new = !cap.not_run_passes.is_empty() || !cap.not_run_stages.is_empty();
        if has_new {
            report.failed_passes.extend(cap.failed_passes());
        }
        // Always folded: this meter counts the whole run (it continues the scan's, and a resumed
        // stage puts its stored usage on it), so the record's spend must be its total even when
        // the stage refused nothing, or the final report reads the scan's smaller figure beside a
        // run that is past its limit.
        match report.ledger.cost_cap().cloned() {
            Some(mut earlier) => {
                earlier.absorb(cap);
                report.ledger.record_cost_cap(earlier);
            }
            None => report.ledger.record_cost_cap(cap),
        }
    }
    // An outage that stopped this stage.
    if let Some(outage) = meter.outage_record() {
        report.failed_passes.extend(outage.failed_passes());
        if report.ledger.outage().is_none() {
            report.ledger.record_outage(outage);
        }
    }
}

/// [`review_groups`] on a meter that continues the scan's own: the whole run's spend counts toward
/// a spend limit, and what the stage spent, was refused or lost to an outage is folded back into
/// the report's usage, ledger and failed parts. This is what the command line and the app call.
pub async fn review_groups_metered(
    report: &mut ScanReport,
    llm: &dyn LlmPort,
    cfg: &GroupReviewConfig<'_>,
    controls: &crate::cost_cap::RunControls,
) -> GroupReviewOutcome {
    let prior = report.actual_usage.clone().unwrap_or_default();
    let meter = UsageMeter::continuing(controls.clone(), &prior);
    let outcome = review_groups(report, llm, cfg, Some(&meter)).await;

    // Usage: the meter started from the scan's, so its snapshot is the whole run's.
    if meter.snapshot().calls > 0 {
        report.actual_usage = Some(meter.snapshot());
    }
    fold_spend_into_report(report, &meter);
    // Per-stage telemetry: this stage's row joins the scan's.
    let fresh = meter.stage_telemetry(report.code_lines);
    if !fresh.stages.is_empty() {
        let mut telemetry = report
            .ledger
            .stage_telemetry()
            .cloned()
            .unwrap_or_else(|| fresh.clone());
        for record in &fresh.stages {
            match telemetry
                .stages
                .iter_mut()
                .find(|s| s.stage == record.stage)
            {
                Some(have) => *have = record.clone(),
                None => telemetry.stages.push(record.clone()),
            }
        }
        telemetry
            .stages
            .sort_by_key(|s| Stage::ALL.iter().position(|a| *a == s.stage));
        report.ledger.record_stage_telemetry(telemetry);
    }
    outcome
}

/// Run the group reviewer over a finished scan's report, the way the command line and the app do:
/// the model and the escalation settings come from the run's controls (the project's `review`
/// settings or the flags), the rule text from the rules the run checked, the spend limit from the
/// run's meter. Does nothing for a report with no findings. Records the model on the report's
/// provenance. Returns what the run came to, or `None` when it did not run.
pub async fn review_scan_report(
    report: &mut ScanReport,
    llm: &dyn LlmPort,
    controls: &crate::cost_cap::RunControls,
    rules: &[(String, String)],
    cli_backend: bool,
    job: Option<(&crate::jobs::JobStore, &str)>,
) -> Option<GroupReviewOutcome> {
    if report.findings.is_empty() {
        return None;
    }
    let model = controls.group_review_model_for();
    let repo_shape = format!(
        "This scan covers {} repositor{} with {} code files and about {} lines of code.",
        report.repos.len().max(1),
        if report.repos.len() > 1 { "ies" } else { "y" },
        report.files_scanned,
        report.code_lines
    );
    let cfg = GroupReviewConfig {
        model: &model,
        repo_shape: &repo_shape,
        rules,
        rank: RankSettings {
            rate_per_kloc: controls.review.escalation_rate_per_kloc,
            floor: controls.review.escalation_floor,
            counted_lines: report.code_lines,
            over_budget_reasons: controls.review.over_budget_reasons.clone(),
        },
        max_in_flight: if cli_backend {
            stage_batch::CLI_MAX_IN_FLIGHT
        } else {
            stage_batch::DEFAULT_MAX_IN_FLIGHT
        },
        cli_backend,
        job,
    };
    let outcome = review_groups_metered(report, llm, &cfg, controls).await;
    report.provenance.group_review_model = Some(model);
    Some(outcome)
}

/// A stand-in for the group reviewer, for scripted models in tests (no network, no spend): when
/// the request is a group review call (`system` is the group reviewer's prompt), a well-formed
/// reply that puts every site of the call in one sub-group and escalates none; when it is the
/// rule-level summary call, a summary. `None` for any other request. Deterministic, so a resumed
/// run's stored replies match.
pub fn stand_in_reply(system: Option<&str>, prompt: &str) -> Option<String> {
    let system = system?;
    if system.starts_with(&rule_summary_system_prompt()) {
        return Some(
            serde_json::json!({ "summary": "The sites of this rule share one cause." }).to_string(),
        );
    }
    if !system.starts_with(&group_review_system_prompt()) {
        return None;
    }
    let re = regex::Regex::new(r"(?m)^(\d+): ").expect("a static pattern");
    let members: Vec<usize> = re
        .captures_iter(prompt)
        .filter_map(|c| c[1].parse().ok())
        .collect();
    Some(
        serde_json::json!({
            "summary": "The sites of this rule share one cause.",
            "subgroups": [{
                "label": "All",
                "cause": "one shared cause",
                "summary": "These sites share one cause.",
                "members": members,
            }],
            "outliers": [],
            "escalations": [],
            "disagreements": [],
        })
        .to_string(),
    )
}
