//! What the project chat assistant sees of an inspection: the review screen's own report.
//!
//! The chat used to read the draft the cockpit had saved (or the last scan held in memory): the
//! UNSEALED rows, with no hold and no junk mark, no group, no stage 2 verdict, no code and no
//! ledger. An operator who asked "why is this row held?" or "is this a duplicate of that one?"
//! got a model that could not see either. This module renders the SEALED report with the
//! reviewer's overlay applied ([`ReviewedReport`], the same value the review screen and the
//! exports read), so the assistant answers from what the operator is looking at:
//!
//! 1. the totals, and how many rows are held for a ruling;
//! 2. the held rows first, each with why it is held (the release gate's own holds, the junk mark's
//!    reason, a possible duplicate, a merge), its evidence, the group reviewer's columns and the
//!    code at the cited line;
//! 3. the top findings by severity, the same way;
//! 4. the groups (rule groups and their sub-groups: the group reviewer's summary, the cause and
//!    the counts the tree states);
//! 5. the stage 2 verdicts: the escalation queue, the outliers and the disagreements with stage 1;
//! 6. the ledger: what ran, what the engine did to the rows, and what the run could not do.
//!
//! It does NOT carry: any row beyond the caps below (the totals say how many there are), the
//! whole file around a row (only [`CODE_RADIUS`] lines either side of the cited line, for the
//! first [`MAX_CODE_ROWS`] rows in the order above), the reviewer's timing log, or a raw secret
//! (the rows are sealed, which redacts them, and the model-input chokepoint redacts what is sent).
//!
//! The text is bounded by [`MAX_CONTEXT_CHARS`]; every section has its own cap, so the cut only
//! falls on the least useful tail.

use std::collections::{BTreeMap, HashMap};

use camerata_api_types::review::EscalationState;
use camerata_api_types::review_view::HoldReason;

use crate::onboard::Finding;
use crate::report_export::overlay::merged_target;
use crate::report_export::ReviewedReport;
use crate::review_tree::{NodeLevel, TreeNode};
use crate::review_view::severity_rank;

/// The most characters the review context may hold.
pub(crate) const MAX_CONTEXT_CHARS: usize = 28_000;
/// The most held rows listed.
const MAX_HELD_ROWS: usize = 30;
/// The most top findings listed.
const MAX_TOP_ROWS: usize = 30;
/// The most groups listed.
const MAX_GROUPS: usize = 24;
/// The most escalation candidates listed.
const MAX_QUEUE: usize = 15;
/// The most rows that carry the code around their cited line.
const MAX_CODE_ROWS: usize = 10;
/// Lines of code either side of the cited line.
const CODE_RADIUS: usize = 3;
/// The longest line of code shown.
const MAX_CODE_LINE_CHARS: usize = 160;
/// The longest explanation, evidence or summary shown.
const MAX_TEXT_CHARS: usize = 280;
/// The most stages, notes and rules the ledger section lists.
const MAX_LEDGER_LINES: usize = 12;

/// File text by `(repo, path)`, as the model saw it (redacted, the way a scan reads a file).
pub(crate) type CodeFiles = HashMap<(String, String), String>;

/// `text` cut to `max` characters, on one line, with an ellipsis when it was cut.
fn short(text: &str, max: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max).collect();
    format!("{cut}\u{2026}")
}

/// A site key as the context shows it. The engine joins the parts of a key (repo, path,
/// fingerprint, ordinal) with a NUL byte, which a model prompt cannot carry (a command argument
/// refuses one), so the parts are joined with `|` here, everywhere a key is named, the pointers
/// between rows included.
fn shown_key(key: &str) -> String {
    key.replace('\0', "|")
}

/// The holds of every row by site key, from the one hold function.
fn holds_by_key(reviewed: &ReviewedReport) -> BTreeMap<String, Vec<(HoldReason, String)>> {
    crate::release_gate::holds::hold_set(reviewed).reasons
}

/// The rows in the order the context lists them: the held rows (in report order), then the rest
/// by severity, then status, then place.
fn rows_in_order<'a>(
    reviewed: &'a ReviewedReport,
    holds: &BTreeMap<String, Vec<(HoldReason, String)>>,
) -> (Vec<&'a Finding>, Vec<&'a Finding>) {
    let findings = &reviewed.report().findings;
    let held: Vec<&Finding> = findings
        .iter()
        .filter(|f| holds.contains_key(&f.site_key))
        .collect();
    let mut rest: Vec<&Finding> = findings
        .iter()
        .filter(|f| !holds.contains_key(&f.site_key))
        .collect();
    rest.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| (b.status == "active").cmp(&(a.status == "active")))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    (held, rest)
}

/// The files whose code the context shows, in priority order: the files of the held rows first,
/// then of the top findings, at most [`MAX_CODE_ROWS`] rows' worth. The caller reads these and
/// hands the text back to [`render_review_for_chat`], so the two agree on which rows carry code.
pub(crate) fn rows_wanting_code(reviewed: &ReviewedReport) -> Vec<(String, String)> {
    let holds = holds_by_key(reviewed);
    let (held, rest) = rows_in_order(reviewed, &holds);
    let mut files: Vec<(String, String)> = Vec::new();
    for f in held.iter().chain(rest.iter()).take(MAX_CODE_ROWS) {
        let file = (f.repo.clone(), f.path.clone());
        if !files.contains(&file) {
            files.push(file);
        }
    }
    files
}

/// The code around `line` (1-based) of `text`: [`CODE_RADIUS`] lines either side, numbered, the
/// cited line marked with `>`.
fn code_window(text: &str, line: usize) -> Option<String> {
    if line == 0 {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    if line > lines.len() {
        return None;
    }
    let from = line.saturating_sub(CODE_RADIUS).max(1);
    let to = (line + CODE_RADIUS).min(lines.len());
    let mut out = String::new();
    for n in from..=to {
        let marker = if n == line { ">" } else { " " };
        let text: String = lines[n - 1].chars().take(MAX_CODE_LINE_CHARS).collect();
        out.push_str(&format!("      {marker} {n}: {text}\n"));
    }
    Some(out)
}

/// One row: a line that says what and where it is and what marks it, then the lines under it.
fn row_block(
    f: &Finding,
    holds: &BTreeMap<String, Vec<(HoldReason, String)>>,
    code: Option<&str>,
) -> String {
    let mut out = format!(
        "  [{}] {} \u{b7} {}/{}:{} \u{b7} key {}",
        f.severity,
        f.rule_id,
        f.repo,
        f.path,
        f.line,
        shown_key(&f.site_key)
    );
    if f.status != "active" {
        out.push_str(&format!(" \u{b7} {}", f.status));
    }
    if let Some(reasons) = holds.get(&f.site_key) {
        let words: Vec<String> = reasons
            .iter()
            .map(|(reason, detail)| {
                if detail.is_empty() {
                    reason.phrase().to_string()
                } else {
                    format!("{} ({})", reason.phrase(), short(detail, 80))
                }
            })
            .collect();
        out.push_str(&format!(" \u{b7} HELD: {}", words.join("; ")));
    }
    if let Some(junk) = f.captures.get(crate::junk_finding::JUNK_CONTENT_CAPTURE) {
        out.push_str(&format!(" \u{b7} JUNK: {}", short(junk, 120)));
    }
    if let Some(dup) = &f.possible_duplicate_of {
        out.push_str(&format!(
            " \u{b7} possible duplicate of {} ({})",
            shown_key(&dup.key),
            dup.hint
        ));
    }
    if let Some(target) = merged_target(f) {
        out.push_str(&format!(" \u{b7} merged into {}", shown_key(target)));
    }
    match f.review.escalation_state {
        EscalationState::Escalated => out.push_str(&format!(
            " \u{b7} ESCALATED #{}",
            f.review.global_rank.unwrap_or(0)
        )),
        EscalationState::BelowTheLine => out.push_str(&format!(
            " \u{b7} escalation candidate below the line #{}",
            f.review.global_rank.unwrap_or(0)
        )),
        EscalationState::NotACandidate => {}
    }
    if let Some(reason) = f.review.outlier_reason {
        out.push_str(&format!(" \u{b7} outlier ({})", reason.as_str()));
    }
    out.push('\n');
    if !f.detail.trim().is_empty() {
        out.push_str(&format!(
            "    explanation: {}\n",
            short(&f.detail, MAX_TEXT_CHARS)
        ));
    }
    if !f.snippet.trim().is_empty() {
        out.push_str(&format!(
            "    evidence: {}\n",
            short(&f.snippet, MAX_TEXT_CHARS)
        ));
    }
    if let Some(stage1) = f
        .captures
        .get(crate::ai_audit::STAGE1_CAPTURE)
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
    {
        let real = stage1["real"].as_str().unwrap_or("?");
        let reason = stage1["reason"].as_str().unwrap_or("");
        out.push_str(&format!(
            "    stage 1: real {real} \u{2014} {}\n",
            short(reason, 160)
        ));
    }
    if let Some(why) = &f.calibration_rationale {
        out.push_str(&format!(
            "    calibration: {}\n",
            short(why, MAX_TEXT_CHARS)
        ));
    }
    let ratings: Vec<String> = [
        ("confidence", &f.confidence),
        ("effort", &f.effort),
        ("category", &f.category),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.as_ref().map(|v| format!("{name} {v}")))
    .collect();
    if !ratings.is_empty() {
        out.push_str(&format!("    {}\n", ratings.join(", ")));
    }
    for place in f.also_locations.iter().take(4) {
        out.push_str(&format!(
            "    also at {}/{}:{} ({})\n",
            place.repo, place.path, place.line, place.rule_id
        ));
    }
    let review = &f.review;
    if !review.subgroup_label.is_empty() {
        out.push_str(&format!(
            "    stage 2 placed it in \"{}\": {}\n",
            review.subgroup_label,
            short(&review.subgroup_cause, 160)
        ));
    }
    if let Some(reason) = review.escalation_reason {
        out.push_str(&format!(
            "    stage 2 escalation: {} \u{2014} {}\n",
            reason.phrase(),
            short(&review.escalation_why, 160)
        ));
    }
    if review.outlier_reason.is_some() && !review.outlier_why.is_empty() {
        out.push_str(&format!(
            "    stage 2 outlier: {}\n",
            short(&review.outlier_why, 160)
        ));
    }
    for d in &review.stage2_disagreement {
        out.push_str(&format!(
            "    stage 2 disagrees on {}: stage 1 said {}, stage 2 says {} \u{2014} {}\n",
            d.field.as_str(),
            d.stage1,
            d.mine,
            short(&d.why, 160)
        ));
    }
    if let Some(code) = code {
        out.push_str("    code:\n");
        out.push_str(code);
    }
    out
}

/// The groups of the review tree: rule groups and their sub-groups, biggest first.
fn groups_section(reviewed: &ReviewedReport) -> String {
    let Some(tree) = reviewed.report().tree.as_ref() else {
        return String::new();
    };
    let mut nodes: Vec<&TreeNode> = tree
        .nodes
        .iter()
        .filter(|n| matches!(n.level, NodeLevel::Rule | NodeLevel::Subgroup))
        .filter(|n| !n.id.contains("thrown"))
        .collect();
    nodes.sort_by_key(|n| (std::cmp::Reverse(tree.counts(&n.id).sites), n.id.clone()));
    if nodes.is_empty() {
        return String::new();
    }
    let total = nodes.len();
    let mut out = format!(
        "Groups ({}reviewed by the group reviewer; {} shown of {total}):\n",
        if tree.group_reviewed { "" } else { "NOT yet " },
        total.min(MAX_GROUPS)
    );
    for node in nodes.into_iter().take(MAX_GROUPS) {
        let counts = tree.counts(&node.id);
        out.push_str(&format!(
            "  {} \"{}\" \u{b7} {} sites, {} escalated, {} below the line, {} outliers",
            node.id,
            node.label,
            counts.sites,
            counts.escalated,
            counts.below_the_line,
            counts.outliers
        ));
        if let Some(cause) = &node.cause {
            out.push_str(&format!(" \u{b7} cause: {}", short(cause, 160)));
        }
        out.push('\n');
        if let Some(summary) = &node.summary {
            out.push_str(&format!("    {}\n", short(summary, MAX_TEXT_CHARS)));
        }
    }
    out
}

/// The stage 2 queue: the candidates the group reviewer wanted escalated, in rank order.
fn queue_section(reviewed: &ReviewedReport) -> String {
    let Some(tree) = reviewed.report().tree.as_ref() else {
        return String::new();
    };
    if tree.candidates.is_empty() {
        return String::new();
    }
    let rows: HashMap<&str, &Finding> = reviewed
        .report()
        .findings
        .iter()
        .map(|f| (f.site_key.as_str(), f))
        .collect();
    let mut out = format!(
        "Escalation queue (stage 2): {} escalated of a budget of {}, {} candidates in all:\n",
        tree.candidates.iter().filter(|c| c.escalated).count(),
        tree.budget.budget,
        tree.candidates.len()
    );
    for c in tree.candidates.iter().take(MAX_QUEUE) {
        let place = rows.get(c.key.as_str()).map_or_else(
            || c.key.clone(),
            |f| format!("{}/{}:{}", f.repo, f.path, f.line),
        );
        out.push_str(&format!(
            "  #{} {} \u{b7} {} \u{b7} {} \u{b7} key {} \u{2014} {}\n",
            c.global_rank,
            if c.escalated {
                "escalated"
            } else {
                "below the line"
            },
            c.reason.phrase(),
            place,
            shown_key(&c.key),
            short(&c.why, 160)
        ));
    }
    out
}

/// The ledger: what ran, what the engine did to the rows, what the run could not do.
fn ledger_section(reviewed: &ReviewedReport) -> String {
    let report = reviewed.report();
    let ledger = &report.ledger;
    let mut out = String::from("Ledger:\n");
    for stage in ledger.stages().iter().take(MAX_LEDGER_LINES) {
        let repo = if stage.repo.is_empty() {
            String::new()
        } else {
            format!(" ({})", stage.repo)
        };
        out.push_str(&format!(
            "  stage {}{repo}: {} in, {} out, {} unaccounted\n",
            stage.stage, stage.rows_in, stage.rows_out, stage.unaccounted
        ));
    }
    if let Some(block) = ledger.escalation() {
        out.push_str(&format!("  {}\n", block.sentence()));
    }
    let evidence = ledger.evidence_check();
    out.push_str(&format!(
        "  evidence check: {} checked, {} verified, {} unverified (held), {} exempt\n",
        evidence.checked, evidence.verified, evidence.unverified, evidence.exempt
    ));
    let content = ledger.content_check();
    out.push_str(&format!(
        "  junk check: {} rows read, {} held as junk, {} wording flags\n",
        content.rows_seen, content.held, content.wording_flags
    ));
    let discarded = ledger.discarded();
    if discarded.total() > 0 {
        let by_reason: Vec<String> = discarded
            .by_reason
            .iter()
            .map(|(reason, n)| format!("{n} {}", short(reason, 60)))
            .collect();
        out.push_str(&format!(
            "  thrown out by the engine and kept: {} ({})\n",
            discarded.total(),
            by_reason.join("; ")
        ));
    }
    for rule in ledger.excluded_rules().into_iter().take(MAX_LEDGER_LINES) {
        out.push_str(&format!(
            "  rule not run or partial: {} \u{2014} {}\n",
            rule.0,
            short(rule.1, 160)
        ));
    }
    for note in ledger.runner_notes().iter().take(MAX_LEDGER_LINES) {
        out.push_str(&format!("  run note: {}\n", short(note, MAX_TEXT_CHARS)));
    }
    for note in report.coverage_notes.iter().take(MAX_LEDGER_LINES) {
        out.push_str(&format!(
            "  coverage note [{}]: {}\n",
            note.tool,
            short(&note.message, MAX_TEXT_CHARS)
        ));
    }
    out
}

/// A one-line summary per row, for the context's `findings_summary` field: severity, rule, place
/// and the start of the explanation, held rows first, at most [`MAX_TOP_ROWS`] + [`MAX_HELD_ROWS`]
/// rows. Returns how many rows the report holds and the lines.
pub(crate) fn findings_summary_for_chat(reviewed: &ReviewedReport) -> (usize, String) {
    let holds = holds_by_key(reviewed);
    let (held, rest) = rows_in_order(reviewed, &holds);
    let total = held.len() + rest.len();
    let lines: Vec<String> = held
        .iter()
        .take(MAX_HELD_ROWS)
        .map(|f| (f, true))
        .chain(rest.iter().take(MAX_TOP_ROWS).map(|f| (f, false)))
        .map(|(f, is_held)| {
            format!(
                "[{}] {} in {}/{}:{}{} \u{2014} {}",
                f.severity,
                f.rule_id,
                f.repo,
                f.path,
                f.line,
                if is_held { " (HELD)" } else { "" },
                short(&f.detail, 120)
            )
        })
        .collect();
    (total, lines.join("\n"))
}

/// The review context, as the chat system prompt carries it. `code` holds the files
/// [`rows_wanting_code`] named, as the model saw them; a file missing from it leaves its rows
/// without code and changes nothing else.
pub(crate) fn render_review_for_chat(reviewed: &ReviewedReport, code: &CodeFiles) -> String {
    let report = reviewed.report();
    let holds = holds_by_key(reviewed);
    let (held, rest) = rows_in_order(reviewed, &holds);
    let total = held.len() + rest.len();

    let mut out = String::new();
    out.push_str(&format!(
        "Inspection {}: {total} rows kept, {} thrown out by the engine and kept apart, {} awaiting a ruling (held).\n",
        report.run_id.as_deref().unwrap_or("(unnamed)"),
        report.discarded.len(),
        held.len()
    ));
    let mut by_severity: BTreeMap<u8, (String, usize)> = BTreeMap::new();
    for f in held.iter().chain(rest.iter()) {
        by_severity
            .entry(severity_rank(&f.severity))
            .or_insert_with(|| (f.severity.clone(), 0))
            .1 += 1;
    }
    let severities: Vec<String> = by_severity
        .values()
        .map(|(name, n)| format!("{name} {n}"))
        .collect();
    out.push_str(&format!("Severity: {}\n", severities.join(", ")));
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for f in held.iter().chain(rest.iter()) {
        *by_rule.entry(f.rule_id.as_str()).or_insert(0) += 1;
    }
    let mut rules: Vec<(&str, usize)> = by_rule.into_iter().collect();
    rules.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let top_rules: Vec<String> = rules
        .iter()
        .take(15)
        .map(|(r, n)| format!("{r} {n}"))
        .collect();
    out.push_str(&format!("By rule (top): {}\n", top_rules.join(", ")));

    // The first `MAX_CODE_ROWS` rows in listing order carry the code around their cited line, the
    // same rows `rows_wanting_code` named the files of.
    let code_for = |ordinal: usize, f: &Finding| -> Option<String> {
        if ordinal >= MAX_CODE_ROWS {
            return None;
        }
        code_window(code.get(&(f.repo.clone(), f.path.clone()))?, f.line)
    };

    if !held.is_empty() {
        out.push_str(&format!(
            "\nHeld rows (awaiting a ruling; {} shown of {}):\n",
            held.len().min(MAX_HELD_ROWS),
            held.len()
        ));
        for (i, f) in held.iter().take(MAX_HELD_ROWS).enumerate() {
            out.push_str(&row_block(f, &holds, code_for(i, f).as_deref()));
        }
    }
    out.push_str(&format!(
        "\nTop findings ({} shown of {}, severity-ordered):\n",
        rest.len().min(MAX_TOP_ROWS),
        rest.len()
    ));
    for (i, f) in rest.iter().take(MAX_TOP_ROWS).enumerate() {
        out.push_str(&row_block(
            f,
            &holds,
            code_for(held.len() + i, f).as_deref(),
        ));
    }

    for section in [
        groups_section(reviewed),
        queue_section(reviewed),
        ledger_section(reviewed),
    ] {
        if !section.is_empty() {
            out.push('\n');
            out.push_str(&section);
        }
    }

    if out.chars().count() > MAX_CONTEXT_CHARS {
        let cut: String = out.chars().take(MAX_CONTEXT_CHARS).collect();
        out = format!("{cut}\n[context cut at {MAX_CONTEXT_CHARS} characters]\n");
    }
    out
}

/// What the chat context endpoint takes from the latest inspection.
pub(crate) struct ReviewChatContext {
    /// How many rows the report holds (the sealed rows, thrown-out rows not counted).
    pub total: usize,
    /// One line per row for the first rows ([`findings_summary_for_chat`]).
    pub findings_summary: String,
    /// The review context ([`render_review_for_chat`]).
    pub section: String,
}

/// The review context of `project_id`'s latest inspection, from the SAME sealed and overlaid
/// report the review screen and the exports read ([`crate::edits::routes::review_of`]), with the
/// code at the cited lines read from the project's checkout the way a scan read it (redacted).
/// `None` when the project has no inspection yet (the endpoint then falls back to the draft and
/// the last scan, sealed).
pub(crate) async fn review_context_of(
    state: &crate::AppState,
    project_id: &str,
) -> Option<ReviewChatContext> {
    let latest = state.inspections.latest(project_id)?;
    let (_, reviewed) = crate::edits::routes::review_of(state, project_id, &latest);
    let mut code = CodeFiles::new();
    for (repo, path) in rows_wanting_code(&reviewed) {
        let files = crate::edits::routes::source_of(state, project_id, &repo, &path).await;
        if let Some((_, text)) = files.into_iter().next() {
            code.insert((repo, path), text);
        }
    }
    let (total, findings_summary) = findings_summary_for_chat(&reviewed);
    Some(ReviewChatContext {
        total,
        findings_summary,
        section: render_review_for_chat(&reviewed, &code),
    })
}

#[cfg(test)]
mod tests;
