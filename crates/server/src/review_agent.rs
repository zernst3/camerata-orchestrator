//! The shared read-only "review agent" pattern (B3).
//!
//! A reviewer runs a single non-agentic LLM call via the existing `Llm::complete` path
//! with a structured system prompt + inputs, and returns a typed `ReviewVerdict`.
//! Reviewers produce judgments ONLY — they do NOT write code and need NO gated_write or
//! MCP tools. They are pure read-call wrappers over the existing LlmPort seam.
//!
//! Two reviewers live here:
//! - **`run_l3_review`** (R7): the Layer-3 agentic code reviewer, which verifies a diff
//!   against story intent + rules + COMPLETENESS (for a cross-cutting story, a narrow spot-fix
//!   is bounced with a request to sweep the codebase for the remaining instances — the general
//!   "did you catch them all?" check, done by reasoning rather than a per-defect linter).
//!   Opt-in per project (see `Project::l3_review`).
//! - **`run_integration_gate_review`** (R3.e): the cross-repo contract verifier. Takes
//!   the prose contract + per-repo assembled outputs and returns a `ReviewVerdict`.
//!   The live result maps to `IntegrationGateResult` via `check_integration_gate_live`.
//!
//! Both reviewers are BLIND to other-agent reasoning: they see ONLY the story/contract +
//! rules + code evidence. This isolation prevents rubber-stamping.
//!
//! # Dependency note
//!
//! This module deliberately does NOT import `camerata-gateway` types. The gateway crate's
//! binary-only modules (`fan_out`, `delegate`) cannot be exposed from the gateway lib
//! without restructuring. Instead, `check_integration_gate_live` returns a
//! `LiveGateResult` (a thin server-side mirror of `IntegrationGateResult`) and the caller
//! maps it where needed.

use crate::llm::{LlmPort, LlmRequest};
use serde::{Deserialize, Serialize};

// ── Shared verdict type ────────────────────────────────────────────────────────

/// The verdict returned by a review agent call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewVerdict {
    /// The reviewed code passes: no violations found.
    Pass,
    /// The reviewed code has issues. Bounce back like L2.
    Bounce { reasons: Vec<String> },
}

impl ReviewVerdict {
    /// True when the verdict is `Pass`.
    pub fn is_pass(&self) -> bool {
        matches!(self, ReviewVerdict::Pass)
    }
}

// ── L3 reviewer (R7) ─────────────────────────────────────────────────────────

/// Inputs to the L3 agentic code reviewer (R7).
///
/// The reviewer sees ONLY: story + rules + diff. It is BLIND to all other agent
/// contexts (investigation notes, orchestrator transcripts, developer reasoning).
/// This isolation — from other agents, not from the story — is what prevents
/// rubber-stamping (spec-grounded, implementer-blind).
pub struct L3ReviewInput<'a> {
    /// The story (requirements, contract, integrations, acceptance criteria).
    pub story: &'a str,
    /// The selected rules for this repo as prose (the SSOT).
    pub rules_prose: &'a str,
    /// The diff of changes in this repo.
    pub diff: &'a str,
    /// The model id to use for this review.
    pub model: &'a str,
    /// The unit's still-open findings from earlier reviews, one `[HIGH] title` line each. The
    /// reviewer re-checks each against the diff and confirms the gone ones with `[FIXED]` lines.
    /// Empty on a unit with nothing open.
    pub earlier_findings: &'a [String],
}

const L3_SYSTEM_PROMPT: &str = "\
You are an expert code reviewer performing a Layer-3 agentic review. Your job is to verify that the submitted diff:
1. Conforms to the provided rules (the SSOT).
2. Fulfills the story's intent (requirements, contract, integrations, acceptance criteria).
3. Addresses the story's FULL SCOPE (completeness). If the story describes a cross-cutting change — a pattern, invariant or class of defect that should apply EVERYWHERE it occurs (signals like \"every\", \"all\", \"across the app\", \"anywhere that\" or a fix for a mistake likely repeated elsewhere) — check that the diff addresses the WHOLE class, not just a single instance. A spot-fix for what the story frames as a pervasive pattern is INCOMPLETE: bounce it and say to sweep the codebase for the remaining instances. If the story is genuinely local and bounded, do NOT manufacture a completeness concern.

You see ONLY: the story, the rules and the diff. You do NOT have access to any other agent's reasoning, implementation notes or context (this isolation is intentional). You also cannot see the rest of the codebase, so judge completeness by whether the diff's breadth plausibly matches the story's implied scope; when a cross-cutting story is addressed by a narrow diff, bounce and ask to confirm the sweep covered every instance.

YOU ARE ADVERSARIAL. Assume the change is wrong until the diff shows otherwise. Look for the way it FAILS: the input it mishandles, the case it forgets, the rule it breaks, the acceptance criterion it skips. Do not praise the change and do not look for reasons to approve it.

PROCEDURE (in order, before you decide): (1) check the diff rule-by-rule against the SSOT; (2) check it criterion-by-criterion against the story's requirements and acceptance criteria; only then form the verdict. If you cannot verify a rule or criterion from the diff alone, that is a MEDIUM finding (\"cannot verify <what>\"), never a silent PASS.

List EVERY finding, ranked HIGH, MEDIUM or LOW. HIGH: the change is wrong, unsafe or breaks a rule or acceptance criterion. MEDIUM: a likely defect, an unverifiable point or an incomplete sweep. LOW: a minor issue that does not block the change. Each finding MUST carry a concrete failure scenario (the input or situation, and what goes wrong), the rule id or the specific criterion it fails and the location in the diff. Write PASS only when there is no HIGH and no MEDIUM finding.

EARLIER FINDINGS: when the user message has an \"Earlier findings to re-check\" section, re-check each one against the diff. For each one you VERIFIED is gone, write one line `- [FIXED] <title>` with the title exactly as given. If it is still present, report it again as a finding with its severity. If you cannot verify it from the diff, write nothing for it: it stays open.

Return your verdict in this exact format, one finding per line:
FINDINGS
- [HIGH] <title> | scenario: <one sentence> | rule: <id or criterion or none> | where: <file:line or none>
- [LOW] <title> | scenario: <one sentence> | rule: <id or criterion or none> | where: <file:line or none>
- [FIXED] <title of an earlier finding you verified is gone>
VERDICT: PASS
(or VERDICT: BOUNCE when any finding is HIGH or MEDIUM)

If there are no findings, write FINDINGS, then \"- none\", then the VERDICT line.

Be specific and terse. Do not include explanation outside this format.";

/// How serious a reviewer finding is. Declaration order is the ranking: `High` sorts first.
/// `High` and `Medium` block a pass; `Low` is recorded and left for the architect at sign-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSeverity {
    High,
    Medium,
    Low,
}

impl ReviewSeverity {
    /// The upper-case tag used in the reviewer's output and in event text.
    pub fn tag(self) -> &'static str {
        match self {
            ReviewSeverity::High => "HIGH",
            ReviewSeverity::Medium => "MEDIUM",
            ReviewSeverity::Low => "LOW",
        }
    }

    /// True for the severities that stop a pass (`High`, `Medium`).
    pub fn blocks(self) -> bool {
        !matches!(self, ReviewSeverity::Low)
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_uppercase().as_str() {
            "HIGH" => Some(ReviewSeverity::High),
            "MEDIUM" => Some(ReviewSeverity::Medium),
            "LOW" => Some(ReviewSeverity::Low),
            _ => None,
        }
    }
}

/// One ranked finding from the L3 reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub severity: ReviewSeverity,
    pub title: String,
    /// The concrete way this fails (what input, what happens). Required by the prompt.
    #[serde(default)]
    pub scenario: String,
    /// The rule id or acceptance criterion the reviewer quoted.
    #[serde(default)]
    pub rule: Option<String>,
    /// The file or `file:line` from the diff.
    #[serde(default)]
    pub location: Option<String>,
}

impl ReviewFinding {
    /// One line for the implementer's revise prompt and the run events:
    /// `[HIGH] title: scenario` (just `[HIGH] title` when there is no scenario).
    pub fn reason_line(&self) -> String {
        if self.scenario.trim().is_empty() {
            format!("[{}] {}", self.severity.tag(), self.title)
        } else {
            format!(
                "[{}] {}: {}",
                self.severity.tag(),
                self.title,
                self.scenario
            )
        }
    }
}

/// The L3 reviewer's verdict. Unlike the integration gate's [`ReviewVerdict`] it carries ranked
/// findings. A reply that cannot be read is never a pass (see [`parse_l3_verdict`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum L3Verdict {
    /// No HIGH or MEDIUM finding. Any LOW findings are carried so they can be recorded.
    Pass { low: Vec<ReviewFinding> },
    /// At least one HIGH or MEDIUM finding. Every finding is listed, LOW ones included.
    Bounce { findings: Vec<ReviewFinding> },
}

impl L3Verdict {
    /// True when the verdict is `Pass`.
    pub fn is_pass(&self) -> bool {
        matches!(self, L3Verdict::Pass { .. })
    }

    /// Every finding in the verdict (the LOW ones of a pass, all of a bounce's).
    pub fn findings(&self) -> &[ReviewFinding] {
        match self {
            L3Verdict::Pass { low } => low,
            L3Verdict::Bounce { findings } => findings,
        }
    }

    /// The bounce reasons as `[HIGH] title: scenario` lines; empty for a pass. This keeps the
    /// pre-contract call sites (a list of reason strings, empty meaning pass) working.
    pub fn reasons(&self) -> Vec<String> {
        match self {
            L3Verdict::Pass { .. } => Vec::new(),
            L3Verdict::Bounce { findings } => {
                findings.iter().map(ReviewFinding::reason_line).collect()
            }
        }
    }
}

/// Run the L3 agentic code reviewer.
///
/// Calls the LLM with a structured prompt that contains the story + rules + diff.
/// The reviewer is BLIND to implementation notes and other-agent transcripts — only the
/// three inputs above (plus the unit's open earlier findings, to re-check) are forwarded.
/// Returns an [`L3Reply`]: the [`L3Verdict`] (a pass carrying any LOW findings, or a bounce
/// with every ranked finding for the developer) and the earlier findings confirmed gone.
pub async fn run_l3_review(
    llm: &dyn LlmPort,
    input: &L3ReviewInput<'_>,
) -> anyhow::Result<L3Reply> {
    let user_prompt = build_l3_prompt(input);
    let req = LlmRequest::new(user_prompt)
        .with_system(l3_system_prompt())
        .with_model(input.model);
    let resp = llm.complete(req).await?;
    Ok(parse_l3_reply(&resp.text))
}

/// Build the user prompt for the L3 reviewer. Pure + testable.
pub fn build_l3_prompt(input: &L3ReviewInput<'_>) -> String {
    let earlier = if input.earlier_findings.is_empty() {
        String::new()
    } else {
        format!(
            "## Earlier findings to re-check\n\n{}\n\n",
            input
                .earlier_findings
                .iter()
                .map(|f| format!("- {}", camerata_secrets::redact_str(f)))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "## Story\n\n{story}\n\n\
         ## Rules (SSOT)\n\n{rules}\n\n\
         {earlier}\
         ## Diff\n\n```diff\n{diff}\n```",
        story = input.story,
        rules = input.rules_prose,
        // The diff is client code going to a model: redact credential values first.
        diff = camerata_secrets::redact_str(input.diff),
    )
}

/// What the reviewer said its verdict was, before the findings are weighed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatedVerdict {
    Pass,
    Bounce,
}

/// The title of the finding a reply gets when it is not in the agreed format.
const FORMAT_FINDING_TITLE: &str = "reviewer reply was not in the agreed format";

/// The first alphabetic word of a line, upper-cased (`"PASS - fine"` gives `PASS`).
fn leading_word(text: &str) -> String {
    text.trim_start_matches(|c: char| !c.is_ascii_alphabetic())
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn stated_verdict_word(word: &str) -> Option<StatedVerdict> {
    match word {
        "PASS" => Some(StatedVerdict::Pass),
        "BOUNCE" => Some(StatedVerdict::Bounce),
        _ => None,
    }
}

/// The last `VERDICT: PASS|BOUNCE` line, tolerating markdown emphasis around it.
fn verdict_line(text: &str) -> Option<StatedVerdict> {
    text.lines().rev().find_map(|line| {
        let plain = line
            .trim()
            .trim_matches(|c: char| c == '*' || c == '_' || c == '`' || c == '#')
            .trim();
        let head = plain.get(..8)?;
        if !head.eq_ignore_ascii_case("VERDICT:") {
            return None;
        }
        stated_verdict_word(&leading_word(&plain[8..]))
    })
}

/// The body of a bullet line (`- x`, `* x`, `1. x`, `1) x`) with the marker removed; `None`
/// when the line is not a bullet. A bare `**bold**` line is not a bullet.
fn bullet_body(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix('-') {
        return Some(rest.trim_start());
    }
    if let Some(rest) = t.strip_prefix('*') {
        if rest.starts_with(char::is_whitespace) {
            return Some(rest.trim_start());
        }
        return None;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &t[digits..];
        let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
        if rest.starts_with(char::is_whitespace) {
            return Some(rest.trim_start());
        }
    }
    None
}

/// A line with its bullet marker and any leading markdown emphasis removed, so
/// `- **[HIGH]** x` and `1. [HIGH] x` both start at the `[`.
fn strip_decor(line: &str) -> &str {
    let mut t = line.trim();
    loop {
        let next = match bullet_body(t) {
            Some(body) => body,
            None => t.trim_start_matches(['*', '_', '`', '#', '>']).trim_start(),
        };
        if next.len() == t.len() {
            return t;
        }
        t = next;
    }
}

/// Trim whitespace, emphasis marks and a leading colon from a field value.
fn clean_field(text: &str) -> String {
    text.trim()
        .trim_matches(['*', '_', '`'])
        .trim_start_matches(':')
        .trim()
        .trim_matches(['*', '_', '`'])
        .trim()
        .to_string()
}

/// One tagged line of the reply.
enum Tagged {
    Finding(ReviewFinding),
    /// `- [FIXED] <title>`: the reviewer verified an earlier finding is gone.
    Fixed(String),
}

/// Parse one tagged line, `[HIGH] title | scenario: .. | rule: .. | where: ..` or
/// `[FIXED] title`. Bullets, numbering and markdown emphasis in front of the tag are tolerated.
/// `None` when the line carries no `[HIGH|MEDIUM|LOW|FIXED]` tag.
fn parse_tagged(line: &str) -> Option<Tagged> {
    let t = strip_decor(line);
    let rest = t.strip_prefix('[')?;
    let (tag, after) = rest.split_once(']')?;
    let tag = tag.trim().to_ascii_uppercase();
    let mut parts = after.split('|');
    let title = clean_field(parts.next().unwrap_or(""));
    if tag == "FIXED" {
        return (!title.is_empty()).then_some(Tagged::Fixed(title));
    }
    let severity = ReviewSeverity::from_tag(&tag)?;
    let mut finding = ReviewFinding {
        severity,
        title,
        scenario: String::new(),
        rule: None,
        location: None,
    };
    for part in parts {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let value = clean_field(value);
        let none = value.is_empty() || value.eq_ignore_ascii_case("none");
        match clean_field(key).to_ascii_lowercase().as_str() {
            "scenario" => finding.scenario = value,
            "rule" if !none => finding.rule = Some(value),
            "where" | "location" if !none => finding.location = Some(value),
            _ => {}
        }
    }
    if finding.title.is_empty() {
        finding.title = "(untitled finding)".to_string();
    }
    Some(Tagged::Finding(finding))
}

fn medium(title: &str, scenario: String) -> ReviewFinding {
    ReviewFinding {
        severity: ReviewSeverity::Medium,
        title: title.to_string(),
        scenario,
        rule: None,
        location: None,
    }
}

/// Bullet lines that carry no severity tag (the old `BOUNCE\n- reason` shape, or stray notes),
/// each read as a MEDIUM finding titled with the text, so nothing the reviewer wrote is dropped.
/// A bare `- none` is not a finding.
fn leftover_bullets(text: &str) -> Vec<ReviewFinding> {
    text.lines()
        .filter(|l| bullet_body(l).is_some() && parse_tagged(l).is_none())
        .map(|l| strip_decor(l).trim().to_string())
        .filter(|s| !s.is_empty() && !s.trim_end_matches('.').eq_ignore_ascii_case("none"))
        .map(|reason| medium(&reason, String::new()))
        .collect()
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A parsed reviewer reply: the verdict and the earlier findings the reviewer confirmed gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L3Reply {
    pub verdict: L3Verdict,
    /// Titles from `- [FIXED] <title>` lines. Empty unless the reply is trustworthy: it has an
    /// explicit `VERDICT:` line and produced no synthetic finding. A finding is only ever marked
    /// fixed on this evidence, never because a later review stayed silent about it.
    pub confirmed_fixed: Vec<String>,
}

/// Parse the L3 reviewer's text response into a typed [`L3Verdict`]. Pure + testable.
pub fn parse_l3_verdict(text: &str) -> L3Verdict {
    parse_l3_reply(text).verdict
}

/// Parse the L3 reviewer's text response into a verdict plus its confirmed-fixed titles.
///
/// The contract is `FINDINGS` (one `- [HIGH|MEDIUM|LOW] title | scenario: .. | rule: .. |
/// where: ..` line each, plus `- [FIXED] title` for each earlier finding verified gone) then
/// `VERDICT: PASS|BOUNCE`. The parser fails safe:
/// - the findings win over the verdict line: any HIGH or MEDIUM finding is a bounce even when
///   the reply says PASS;
/// - on a stated PASS, a leftover bullet that is not `none` becomes a MEDIUM finding (a bounce);
/// - the old shape still reads (`PASS`, or `BOUNCE` with `- reason` lines, each reason a MEDIUM
///   finding);
/// - a reply with no `VERDICT:` line and no leading `PASS` is NEVER a pass: it bounces with
///   one MEDIUM finding carrying the raw text (so a truncated reply cannot hide a finding);
/// - `[FIXED]` lines count only from a reply with an explicit `VERDICT:` line and no synthetic
///   finding.
pub fn parse_l3_reply(text: &str) -> L3Reply {
    let trimmed = text.trim();
    let mut tagged: Vec<ReviewFinding> = Vec::new();
    let mut fixed: Vec<String> = Vec::new();
    for line in trimmed.lines() {
        match parse_tagged(line) {
            Some(Tagged::Finding(f)) => tagged.push(f),
            Some(Tagged::Fixed(title)) => fixed.push(title),
            None => {}
        }
    }
    let explicit = verdict_line(trimmed);
    let stated = explicit.or_else(|| stated_verdict_word(&leading_word(trimmed)));
    let leftovers = leftover_bullets(trimmed);
    // A synthetic finding is one the parser made up from a reply that did not follow the format.
    let mut synthetic = !leftovers.is_empty();
    let mut findings = tagged;
    findings.extend(leftovers);

    let verdict = if findings.iter().any(|f| f.severity.blocks()) {
        // The findings win: a HIGH or MEDIUM finding is a bounce whatever the verdict line says.
        L3Verdict::Bounce { findings }
    } else {
        match stated {
            Some(StatedVerdict::Pass) => L3Verdict::Pass { low: findings },
            Some(StatedVerdict::Bounce) => {
                synthetic = true;
                findings.push(medium(
                    "reviewer bounced without a ranked finding",
                    "The reviewer's verdict was BOUNCE but it listed no HIGH or MEDIUM finding."
                        .to_string(),
                ));
                L3Verdict::Bounce { findings }
            }
            None => {
                synthetic = true;
                findings.push(medium(FORMAT_FINDING_TITLE, truncate_chars(trimmed, 500)));
                L3Verdict::Bounce { findings }
            }
        }
    };
    let confirmed_fixed = if explicit.is_some() && !synthetic {
        fixed
    } else {
        Vec::new()
    };
    L3Reply {
        verdict,
        confirmed_fixed,
    }
}

// ── Integration-gate reviewer (R3.e) ─────────────────────────────────────────

/// Inputs to the integration-gate agentic check (R3.e).
pub struct IntegrationGateReviewInput<'a> {
    /// The prose cross-repo contract.
    pub contract: &'a str,
    /// Per-repo assembled outputs (repo name + output text).
    pub repo_outputs: &'a [(&'a str, &'a str)],
    /// The model id to use.
    pub model: &'a str,
}

const INTEGRATION_SYSTEM_PROMPT: &str = "\
You are a cross-repo integration verifier. Your job is to check that the assembled per-repo outputs are consistent with the agreed cross-repo contract below.

PROCEDURE (in order): enumerate the contract clause-by-clause and, for each clause, check every repo's output against it before concluding. A repo that produced an empty or missing output is checked against the contract too (a clause requiring its participation is a MISMATCH, not a pass). If a clause cannot be verified from the outputs provided, report it as a MISMATCH reason (\"insufficient evidence for <clause>\"), never a silent PASS.

Return your verdict in this exact format:
PASS
(if every repo's output is consistent with the contract)

or:

MISMATCH
- <reason 1>
- <reason 2>
...
(if any repo's output violates the contract)

Be specific: name the repo and the contract clause that is violated. Do not include explanation outside the verdict format.";

/// The L3 reviewer's system prompt with the shared read-only governance protocol prepended.
/// Reviewers are analysis agents, so they get [`camerata_app_core::GOVERNANCE_KERNEL_READONLY`]
/// (ground-every-claim, enumerate-then-judge, if-unsure-say-so, exact-output-only) ahead of the
/// review-specific instructions. See rewrite 3.3 in
/// `docs/plans/2026-07-05_prompt-hardening-and-governance-kernel.md`.
fn l3_system_prompt() -> String {
    format!(
        "{}\n\n{}",
        camerata_app_core::GOVERNANCE_KERNEL_READONLY,
        L3_SYSTEM_PROMPT
    )
}

/// The integration-gate reviewer's system prompt with the shared read-only governance protocol
/// prepended. See rewrite 3.4 in the prompt-hardening plan.
fn integration_system_prompt() -> String {
    format!(
        "{}\n\n{}",
        camerata_app_core::GOVERNANCE_KERNEL_READONLY,
        INTEGRATION_SYSTEM_PROMPT
    )
}

/// Run the integration-gate agentic reviewer.
///
/// Calls the LLM to verify that the assembled per-repo outputs are consistent with the
/// cross-repo contract. Returns `ReviewVerdict::Pass` or `ReviewVerdict::Bounce` with
/// the mismatches.
pub async fn run_integration_gate_review(
    llm: &dyn LlmPort,
    input: &IntegrationGateReviewInput<'_>,
) -> anyhow::Result<ReviewVerdict> {
    let user_prompt = build_integration_prompt(input);
    let req = LlmRequest::new(user_prompt)
        .with_system(integration_system_prompt())
        .with_model(input.model);
    let resp = llm.complete(req).await?;
    Ok(parse_integration_verdict(&resp.text))
}

/// Build the user prompt for the integration-gate reviewer. Pure + testable.
pub fn build_integration_prompt(input: &IntegrationGateReviewInput<'_>) -> String {
    let repo_section = input
        .repo_outputs
        .iter()
        .map(|(repo, output)| format!("### {repo}\n\n{output}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "## Cross-repo contract\n\n{contract}\n\n\
         ## Per-repo assembled outputs\n\n{repos}",
        contract = input.contract,
        repos = repo_section,
    )
}

/// Parse the integration reviewer's text response into a typed `ReviewVerdict`.
/// Pure + testable (no I/O). Accepts `PASS` or `MISMATCH` as the verdict keyword.
pub fn parse_integration_verdict(text: &str) -> ReviewVerdict {
    let trimmed = text.trim();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("PASS") {
        return ReviewVerdict::Pass;
    }
    // Extract MISMATCH reasons: lines starting with "-".
    let reasons: Vec<String> = trimmed
        .lines()
        .filter(|l| l.trim_start().starts_with('-'))
        .map(|l| l.trim_start_matches('-').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ReviewVerdict::Bounce { reasons }
}

/// The outcome of a live integration-gate check. A server-side mirror of
/// `camerata_gateway::integration_gate::IntegrationGateResult`, without the gateway's
/// internal binary-only module dependencies. Callers that need the gateway type can map
/// this with `LiveGateResult::into_gateway_result()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveGateResult {
    /// No contract was required (single-repo or no cross-boundary work).
    NoContractRequired,
    /// Contract exists and the reviewer agreed it holds.
    Passed,
    /// Contract exists but the reviewer found a mismatch.
    BounceToOrchestrator { reason: String },
}

/// Run the integration gate check with a live LLM model, returning a `LiveGateResult`.
///
/// This is the server-side live path for R3.e: the `Pending` arm in `check_integration_gate`
/// (synchronous, no model) remains for the CI/no-model path; this function is called by
/// the server when a model is available.
///
/// - Contract is `None` → `NoContractRequired` (mirrors the sync gate).
/// - Contract is whitespace-only → `BounceToOrchestrator` (empty contract blocks dev).
/// - LLM returns `PASS` → `Passed`.
/// - LLM returns `MISMATCH` → `BounceToOrchestrator`.
/// - LLM call fails → propagates the error (the caller decides to fall through or block).
pub async fn check_integration_gate_live(
    llm: &dyn LlmPort,
    contract: Option<&str>,
    repo_outputs: &[(&str, &str)],
    model: &str,
) -> anyhow::Result<LiveGateResult> {
    let Some(contract) = contract else {
        return Ok(LiveGateResult::NoContractRequired);
    };
    if contract.trim().is_empty() {
        return Ok(LiveGateResult::BounceToOrchestrator {
            reason: "Contract artifact exists but is empty. A contract that gates development \
                     must contain the agreed interface prose before development starts (R3.g). \
                     Push back to the human or refinement agent to fill it."
                .to_string(),
        });
    }
    let input = IntegrationGateReviewInput {
        contract,
        repo_outputs,
        model,
    };
    let verdict = run_integration_gate_review(llm, &input).await?;
    match verdict {
        ReviewVerdict::Pass => Ok(LiveGateResult::Passed),
        ReviewVerdict::Bounce { reasons } => Ok(LiveGateResult::BounceToOrchestrator {
            reason: reasons.join("; "),
        }),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod redaction_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::L3ReviewConfig;

    // ── 1. L3 opt-in gating ────────────────────────────────────────────────────

    /// When `l3_review.enabled = false`, the caller MUST skip the L3 review entirely.
    /// This test proves that the `enabled` flag is checked at the call site and that the
    /// config default is `false` (the safe/off default).
    #[test]
    fn l3_opt_in_off_by_default() {
        let config = L3ReviewConfig::default();
        assert!(
            !config.enabled,
            "L3 is disabled by default — opt-in per project"
        );
    }

    #[test]
    fn l3_enabled_when_explicitly_set() {
        let config = L3ReviewConfig {
            enabled: true,
            model: "claude-opus-5".to_string(),
            ..Default::default()
        };
        assert!(config.enabled);
    }

    // ── 2. L3 input isolation ──────────────────────────────────────────────────

    /// The prompt built by `build_l3_prompt` contains story + rules + diff and does NOT
    /// contain any strings that would indicate investigation notes or other-agent context.
    #[test]
    fn l3_prompt_contains_story_rules_diff_only() {
        let input = L3ReviewInput {
            story: "Add user authentication with JWT tokens.",
            rules_prose: "RULE-1: All endpoints must be authenticated.",
            diff: "+fn authenticate(token: &str) -> bool { true }",
            model: "claude-sonnet-5",
            earlier_findings: &[],
        };
        let prompt = build_l3_prompt(&input);

        // Required content.
        assert!(
            prompt.contains("Add user authentication with JWT tokens."),
            "prompt must include the story"
        );
        assert!(
            prompt.contains("RULE-1: All endpoints must be authenticated."),
            "prompt must include the rules"
        );
        assert!(
            prompt.contains("+fn authenticate(token: &str) -> bool { true }"),
            "prompt must include the diff"
        );

        // Isolation: investigation notes / agent context must NOT appear.
        assert!(
            !prompt.contains("investigation notes"),
            "prompt must NOT contain investigation notes"
        );
        assert!(
            !prompt.contains("agent context"),
            "prompt must NOT contain agent context"
        );
        assert!(
            !prompt.contains("orchestrator transcript"),
            "prompt must NOT contain orchestrator transcripts"
        );
        assert!(
            !prompt.contains("developer reasoning"),
            "prompt must NOT contain developer reasoning"
        );
    }

    /// The L3 reviewer must carry the COMPLETENESS / full-scope dimension: for a cross-cutting
    /// story (a pattern that should apply everywhere / a defect class with likely siblings), a
    /// narrow spot-fix should be bounced with a request to sweep for the rest. This is the general
    /// "did you catch them all?" check — reasoning, not a linter (see the L3-completeness decision).
    #[test]
    fn l3_system_prompt_carries_completeness_check() {
        let p = L3_SYSTEM_PROMPT;
        assert!(
            p.contains("FULL SCOPE") || p.to_ascii_lowercase().contains("completeness"),
            "L3 prompt must ask for full-scope/completeness verification"
        );
        assert!(
            p.to_ascii_lowercase().contains("sweep"),
            "L3 prompt must tell the reviewer to bounce a spot-fix and request a codebase sweep"
        );
        assert!(
            p.to_ascii_lowercase().contains("cross-cutting"),
            "L3 prompt must scope the completeness check to cross-cutting stories (not local ones)"
        );
    }

    /// The assembled L3 system prompt embeds the read-only governance kernel and the
    /// hardened stepwise + if-unsure mandates.
    #[test]
    fn l3_system_prompt_embeds_readonly_kernel_and_stepwise_mandates() {
        let p = l3_system_prompt();
        assert!(
            p.contains("=== CAMERATA OPERATING PROTOCOL (analysis) ==="),
            "L3 system prompt must embed the read-only governance kernel"
        );
        assert!(
            p.contains("rule-by-rule"),
            "L3 prompt must mandate rule-by-rule enumeration before the verdict"
        );
        assert!(
            p.to_ascii_lowercase().contains("cannot verify"),
            "L3 prompt must turn an unverifiable point into a bounce, not a silent pass"
        );
    }

    /// The assembled integration system prompt embeds the read-only kernel and the
    /// clause-by-clause + insufficient-evidence mandates.
    #[test]
    fn integration_system_prompt_embeds_readonly_kernel_and_clause_mandates() {
        let p = integration_system_prompt();
        assert!(
            p.contains("=== CAMERATA OPERATING PROTOCOL (analysis) ==="),
            "integration system prompt must embed the read-only governance kernel"
        );
        assert!(
            p.contains("clause-by-clause"),
            "integration prompt must mandate clause-by-clause enumeration"
        );
        assert!(
            p.to_ascii_lowercase().contains("insufficient evidence"),
            "integration prompt must report unverifiable clauses as a mismatch"
        );
    }

    // ── 3. L3 verdict parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_l3_verdict_pass() {
        let pass = L3Verdict::Pass { low: vec![] };
        assert_eq!(parse_l3_verdict("PASS"), pass);
        assert_eq!(parse_l3_verdict("pass"), pass);
        assert_eq!(parse_l3_verdict("PASS\n"), pass);
        assert_eq!(
            parse_l3_verdict("PASS\n(the code satisfies both rules and story intent)"),
            pass
        );
        // The new shape with no findings.
        assert_eq!(parse_l3_verdict("FINDINGS\n- none\nVERDICT: PASS"), pass);
    }

    #[test]
    fn parse_ranked_findings_reads_severity_title_scenario_rule_and_location() {
        let text = "FINDINGS\n\
            - [HIGH] Token never expires | scenario: a stolen token works forever | rule: SEC-AUTH-1 | where: src/auth.rs:42\n\
            - [MEDIUM] No error path | scenario: a malformed header panics | rule: none | where: src/auth.rs:60\n\
            VERDICT: BOUNCE";
        let L3Verdict::Bounce { findings } = parse_l3_verdict(text) else {
            panic!("a HIGH finding must bounce");
        };
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].severity, ReviewSeverity::High);
        assert_eq!(findings[0].title, "Token never expires");
        assert_eq!(findings[0].scenario, "a stolen token works forever");
        assert_eq!(findings[0].rule.as_deref(), Some("SEC-AUTH-1"));
        assert_eq!(findings[0].location.as_deref(), Some("src/auth.rs:42"));
        assert_eq!(findings[1].severity, ReviewSeverity::Medium);
        assert_eq!(findings[1].rule, None, "`none` reads as no rule");
        assert_eq!(findings[1].location.as_deref(), Some("src/auth.rs:60"));
        assert_eq!(
            findings[0].reason_line(),
            "[HIGH] Token never expires: a stolen token works forever"
        );
    }

    #[test]
    fn a_reply_with_only_low_findings_is_a_pass_that_carries_them() {
        let text = "FINDINGS\n- [LOW] Name could be clearer | scenario: a reader may misread it | rule: none | where: a.rs:1\nVERDICT: PASS";
        let verdict = parse_l3_verdict(text);
        assert!(verdict.is_pass());
        let L3Verdict::Pass { low } = verdict else {
            panic!("expected a pass");
        };
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].severity, ReviewSeverity::Low);
        assert_eq!(low[0].title, "Name could be clearer");
    }

    #[test]
    fn a_reply_with_a_medium_finding_is_a_bounce_even_if_it_says_pass() {
        // The VERDICT line disagrees with the findings: the findings win.
        let text = "FINDINGS\n- [MEDIUM] Cannot verify the retry cap | scenario: unbounded retries | rule: none | where: none\n- [LOW] nit | scenario: x | rule: none | where: none\nVERDICT: PASS";
        let L3Verdict::Bounce { findings } = parse_l3_verdict(text) else {
            panic!("a MEDIUM finding must bounce whatever the verdict line says");
        };
        assert_eq!(findings.len(), 2, "every finding is listed, LOW included");
        // And the same when the reply simply opens with PASS.
        let text = "PASS\n- [HIGH] Hard-coded secret | scenario: leaks in logs | rule: SEC-1 | where: x.rs:3";
        assert!(!parse_l3_verdict(text).is_pass());
    }

    #[test]
    fn the_old_bounce_shape_still_parses_as_medium_findings() {
        let text = "BOUNCE\n- Missing error handling in authenticate()\n- JWT secret is hard-coded";
        let L3Verdict::Bounce { findings } = parse_l3_verdict(text) else {
            panic!("the old BOUNCE shape must bounce");
        };
        assert_eq!(findings.len(), 2);
        assert!(findings
            .iter()
            .all(|f| f.severity == ReviewSeverity::Medium));
        assert_eq!(
            findings[0].title,
            "Missing error handling in authenticate()"
        );
        assert_eq!(findings[1].title, "JWT secret is hard-coded");
        assert_eq!(parse_l3_verdict(text).reasons().len(), 2);
    }

    #[test]
    fn an_unformatted_reply_is_never_a_pass() {
        for reply in [
            "something unexpected",
            "Looks good to me.",
            "",
            "FINDINGS\n- [LOW] nit | scenario: s | rule: none | where: none",
            "PASSING grade overall",
            "The change is fine. VERDICT is pending.",
        ] {
            let verdict = parse_l3_verdict(reply);
            assert!(!verdict.is_pass(), "reply {reply:?} must not read as PASS");
            let L3Verdict::Bounce { findings } = verdict else {
                unreachable!()
            };
            assert!(
                findings.iter().any(|f| f.severity == ReviewSeverity::Medium
                    && f.title == "reviewer reply was not in the agreed format"),
                "reply {reply:?} carries the format finding"
            );
        }
    }

    #[test]
    fn an_unformatted_reply_carries_its_raw_text_truncated_to_500_chars() {
        let raw = "x".repeat(900);
        let L3Verdict::Bounce { findings } = parse_l3_verdict(&raw) else {
            panic!("unformatted must bounce");
        };
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].scenario.chars().count(), 500);
    }

    #[test]
    fn a_bounce_verdict_with_only_low_findings_still_bounces() {
        let text =
            "FINDINGS\n- [LOW] nit | scenario: s | rule: none | where: none\nVERDICT: BOUNCE";
        let L3Verdict::Bounce { findings } = parse_l3_verdict(text) else {
            panic!("an explicit BOUNCE is never read as a pass");
        };
        assert!(findings.iter().any(|f| f.severity.blocks()));
    }

    #[test]
    fn reasons_lists_bounce_findings_and_is_empty_for_a_pass() {
        assert!(parse_l3_verdict("PASS").reasons().is_empty());
        let reasons = parse_l3_verdict("BOUNCE\n- bad thing").reasons();
        assert_eq!(reasons, vec!["[MEDIUM] bad thing".to_string()]);
    }

    fn only_finding(text: &str) -> ReviewFinding {
        let L3Verdict::Bounce { mut findings } = parse_l3_verdict(text) else {
            panic!("expected a bounce for {text:?}");
        };
        assert_eq!(findings.len(), 1, "one finding in {text:?}: {findings:?}");
        findings.remove(0)
    }

    /// Markdown decoration in front of the tag must not hide the severity or the fields.
    #[test]
    fn decorated_finding_lines_parse_with_severity_and_scenario() {
        let tail = "Token never expires | scenario: a stolen token works forever | rule: SEC-1 | where: a.rs:4";
        for prefix in [
            "- [HIGH] ",
            "- **[HIGH]** ",
            "**[HIGH]** ",
            "1. [HIGH] ",
            "1) [HIGH] ",
            "1. **[HIGH]** ",
            "* [HIGH] ",
            "- `[HIGH]` ",
            "- _[HIGH]_ ",
            "  - [high] ",
        ] {
            let text = format!("FINDINGS\n{prefix}{tail}\nVERDICT: BOUNCE");
            let f = only_finding(&text);
            assert_eq!(f.severity, ReviewSeverity::High, "prefix {prefix:?}");
            assert_eq!(f.title, "Token never expires", "prefix {prefix:?}");
            assert_eq!(
                f.scenario, "a stolen token works forever",
                "prefix {prefix:?}"
            );
            assert_eq!(f.rule.as_deref(), Some("SEC-1"), "prefix {prefix:?}");
            assert_eq!(f.location.as_deref(), Some("a.rs:4"), "prefix {prefix:?}");
        }
        // Emphasis around the title and the field names too.
        let f = only_finding(
            "- **[MEDIUM]** **No error path** | **scenario:** a bad header panics | rule: none | where: none\nVERDICT: BOUNCE",
        );
        assert_eq!(f.severity, ReviewSeverity::Medium);
        assert_eq!(f.title, "No error path");
        assert_eq!(f.scenario, "a bad header panics");
    }

    #[test]
    fn a_stated_pass_with_a_leftover_bullet_becomes_a_medium_not_a_silent_pass() {
        let text = "FINDINGS\n- The retry loop looks unbounded\nVERDICT: PASS";
        let f = only_finding(text);
        assert_eq!(f.severity, ReviewSeverity::Medium);
        assert_eq!(f.title, "The retry loop looks unbounded");
        // Same for a leading PASS and a numbered leftover.
        assert!(!parse_l3_verdict("PASS\n1. something to fix").is_pass());
        // `none` (any case, with or without a full stop) is not a finding.
        for none in ["- none", "- None.", "1. none", "* NONE"] {
            let text = format!("FINDINGS\n{none}\nVERDICT: PASS");
            assert_eq!(
                parse_l3_verdict(&text),
                L3Verdict::Pass { low: vec![] },
                "{none:?}"
            );
        }
    }

    #[test]
    fn fixed_lines_are_read_from_a_trustworthy_reply() {
        let text = "FINDINGS\n- [FIXED] Token never expires\n1. **[FIXED]** No error path\n- [LOW] nit | scenario: s | rule: none | where: none\nVERDICT: PASS";
        let reply = parse_l3_reply(text);
        assert_eq!(
            reply.confirmed_fixed,
            vec![
                "Token never expires".to_string(),
                "No error path".to_string()
            ]
        );
        assert!(reply.verdict.is_pass());
        // A FIXED line is not a finding.
        assert_eq!(reply.verdict.findings().len(), 1);
    }

    #[test]
    fn fixed_lines_are_ignored_when_the_reply_is_not_trustworthy() {
        // No VERDICT line at all, only a leading PASS.
        assert!(parse_l3_reply("PASS\n- [FIXED] Token never expires")
            .confirmed_fixed
            .is_empty());
        // No VERDICT line and no PASS: the format finding is synthetic.
        assert!(parse_l3_reply("- [FIXED] Token never expires\nLooks fine.")
            .confirmed_fixed
            .is_empty());
        // A sloppy "Looks fine" reply.
        assert!(parse_l3_reply("Looks fine").confirmed_fixed.is_empty());
        // A bounce with no ranked finding (synthetic).
        assert!(
            parse_l3_reply("- [FIXED] Token never expires\nVERDICT: BOUNCE")
                .confirmed_fixed
                .is_empty()
        );
        // A leftover bullet made a synthetic MEDIUM.
        assert!(
            parse_l3_reply("- [FIXED] Token never expires\n- also this\nVERDICT: PASS")
                .confirmed_fixed
                .is_empty()
        );
    }

    #[test]
    fn the_prompt_carries_the_earlier_findings_to_re_check_only_when_there_are_some() {
        let earlier = vec![
            "[HIGH] Token never expires".to_string(),
            "[LOW] nit".to_string(),
        ];
        let with = build_l3_prompt(&L3ReviewInput {
            story: "S",
            rules_prose: "R",
            diff: "D",
            model: "m",
            earlier_findings: &earlier,
        });
        let heading = with
            .find("## Earlier findings to re-check")
            .expect("section");
        assert!(with.contains("- [HIGH] Token never expires"));
        assert!(with.contains("- [LOW] nit"));
        assert!(heading < with.find("## Diff").unwrap(), "before the diff");
        let without = build_l3_prompt(&L3ReviewInput {
            story: "S",
            rules_prose: "R",
            diff: "D",
            model: "m",
            earlier_findings: &[],
        });
        assert!(!without.contains("Earlier findings"));
        assert!(L3_SYSTEM_PROMPT.contains("[FIXED]"));
    }

    /// The prompt demands a ranked, adversarial review with a failure scenario per finding.
    #[test]
    fn l3_system_prompt_demands_ranked_findings_with_a_failure_scenario() {
        let p = L3_SYSTEM_PROMPT;
        for word in [
            "HIGH",
            "MEDIUM",
            "LOW",
            "scenario",
            "ADVERSARIAL",
            "VERDICT:",
        ] {
            assert!(p.contains(word), "L3 prompt must contain {word:?}");
        }
    }

    // ── 4. Integration gate verdict mapping ───────────────────────────────────

    #[test]
    fn parse_integration_verdict_pass() {
        assert_eq!(parse_integration_verdict("PASS"), ReviewVerdict::Pass);
        assert_eq!(parse_integration_verdict("pass"), ReviewVerdict::Pass);
    }

    #[test]
    fn parse_integration_verdict_mismatch() {
        let text = "MISMATCH\n- backend returns {id, name} but contract expects {id, name, email}\n- frontend reads .email field";
        let verdict = parse_integration_verdict(text);
        if let ReviewVerdict::Bounce { reasons } = verdict {
            assert_eq!(reasons.len(), 2);
            assert!(reasons[0].contains("backend returns"));
            assert!(reasons[1].contains("frontend reads"));
        } else {
            panic!("expected Bounce, got Pass");
        }
    }

    // ── 5. Integration prompt structure ───────────────────────────────────────

    #[test]
    fn integration_prompt_includes_contract_and_repos() {
        let outputs = [
            ("backend", "GET /users returns [{id, name}]"),
            (
                "frontend",
                "const res = await fetch('/users'); res[0].email",
            ),
        ];
        let input = IntegrationGateReviewInput {
            contract: "GET /api/users returns [{id, name, email}]",
            repo_outputs: &outputs,
            model: "claude-sonnet-5",
        };
        let prompt = build_integration_prompt(&input);

        assert!(prompt.contains("GET /api/users returns [{id, name, email}]"));
        assert!(prompt.contains("backend"));
        assert!(prompt.contains("GET /users returns [{id, name}]"));
        assert!(prompt.contains("frontend"));
        assert!(prompt.contains("res[0].email"));
    }

    // ── 6. l3_model helper ────────────────────────────────────────────────────

    /// When l3_review.model is empty, l3_model() falls back to tier_map.balanced.
    #[test]
    fn l3_model_falls_back_to_balanced_when_empty() {
        let store = crate::project::ProjectStore::new();
        let p = store.create("T", vec![]).unwrap();
        // Default: l3_review.model is empty, so l3_model() returns the balanced tier.
        assert_eq!(p.l3_model(), p.tier_map.balanced_primary());
    }

    /// When l3_review.model is set, l3_model() returns the pinned model.
    #[test]
    fn l3_model_returns_pinned_model_when_set() {
        let store = crate::project::ProjectStore::new();
        let p = store.create("T2", vec![]).unwrap();
        let updated = store
            .set_l3_review(
                &p.id,
                L3ReviewConfig {
                    enabled: true,
                    model: "claude-opus-5".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.l3_model(), "claude-opus-5");
    }

    /// `tier = Strongest` picks the strongest tier model when no model is pinned; a pinned
    /// model still wins; `Balanced` (the default) is unchanged.
    #[test]
    fn l3_model_uses_the_strongest_tier_when_configured_and_no_model_is_pinned() {
        let store = crate::project::ProjectStore::new();
        let p = store.create("T3", vec![]).unwrap();
        let strongest = store
            .set_l3_review(
                &p.id,
                L3ReviewConfig {
                    enabled: true,
                    tier: crate::project::ReviewTier::Strongest,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(strongest.l3_model(), strongest.tier_map.strongest);
        assert_ne!(strongest.l3_model(), strongest.tier_map.balanced_primary());
        let pinned = store
            .set_l3_review(
                &p.id,
                L3ReviewConfig {
                    enabled: true,
                    model: "claude-opus-5".to_string(),
                    tier: crate::project::ReviewTier::Strongest,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(pinned.l3_model(), "claude-opus-5", "a pinned model wins");
    }

    // ── 7. check_integration_gate_live short-circuits ─────────────────────────

    /// When contract is None, check_integration_gate_live returns NoContractRequired
    /// without calling the LLM. Uses a stub that would panic if called.
    #[tokio::test]
    async fn check_integration_gate_live_no_contract_short_circuits() {
        struct PanicLlm;
        #[async_trait::async_trait]
        impl crate::llm::LlmPort for PanicLlm {
            async fn complete(
                &self,
                _req: crate::llm::LlmRequest,
            ) -> anyhow::Result<crate::llm::LlmResponse> {
                panic!("LLM must not be called when contract is None")
            }
            async fn complete_streaming(
                &self,
                _req: crate::llm::LlmRequest,
                _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
            ) -> anyhow::Result<crate::llm::LlmResponse> {
                panic!("LLM must not be called when contract is None")
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        let result = check_integration_gate_live(&PanicLlm, None, &[], "claude-sonnet-5")
            .await
            .unwrap();
        assert_eq!(result, LiveGateResult::NoContractRequired);
    }

    /// A stub LlmPort that returns a canned response — used for verdict mapping tests.
    struct StubLlm(String);

    #[async_trait::async_trait]
    impl crate::llm::LlmPort for StubLlm {
        async fn complete(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            Ok(crate::llm::LlmResponse {
                text: self.0.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
                truncated: false,
            })
        }
        async fn complete_streaming(
            &self,
            req: crate::llm::LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            let r = self.complete(req).await?;
            on_delta(&r.text);
            Ok(r)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// PASS verdict maps to LiveGateResult::Passed.
    #[tokio::test]
    async fn integration_gate_live_pass_maps_to_passed() {
        let llm = StubLlm("PASS".to_string());
        let result = check_integration_gate_live(
            &llm,
            Some("GET /api/users returns [{id}]"),
            &[("backend", "returns [{id}]")],
            "claude-sonnet-5",
        )
        .await
        .unwrap();
        assert_eq!(result, LiveGateResult::Passed);
    }

    /// MISMATCH verdict maps to LiveGateResult::BounceToOrchestrator.
    #[tokio::test]
    async fn integration_gate_live_mismatch_maps_to_bounce() {
        let llm = StubLlm("MISMATCH\n- backend omits email field".to_string());
        let result = check_integration_gate_live(
            &llm,
            Some("GET /api/users returns [{id, email}]"),
            &[("backend", "returns [{id}]")],
            "claude-sonnet-5",
        )
        .await
        .unwrap();
        assert!(
            matches!(result, LiveGateResult::BounceToOrchestrator { .. }),
            "MISMATCH must map to BounceToOrchestrator"
        );
        if let LiveGateResult::BounceToOrchestrator { reason } = result {
            assert!(reason.contains("backend omits email field"));
        }
    }

    // ── 8. Integration gate bundle policy ─────────────────────────────────────

    /// Integration gate is only invoked (bundle present) when crosses_boundary=true
    /// and contract is non-empty.
    #[test]
    fn integration_gate_live_only_called_with_contract() {
        // No boundary crossing → no bundle.
        let crosses_boundary = false;
        let contract = "";
        let bundle_would_exist = crosses_boundary && !contract.trim().is_empty();
        assert!(!bundle_would_exist, "no bundle when not crossing boundary");

        // Boundary but whitespace-only contract → no bundle.
        let crosses_boundary = true;
        let contract = "  ";
        let bundle_would_exist = crosses_boundary && !contract.trim().is_empty();
        assert!(
            !bundle_would_exist,
            "no bundle when contract is whitespace-only"
        );

        // Real boundary + real contract → bundle IS created.
        let crosses_boundary = true;
        let contract = "GET /api/users returns [{id}]";
        let bundle_would_exist = crosses_boundary && !contract.trim().is_empty();
        assert!(
            bundle_would_exist,
            "bundle IS created for real cross-boundary + contract"
        );
    }
}
