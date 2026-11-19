//! The advisory's words: fixed sentences, numbers filled in.
//!
//! The text never advises. It states which numbers are over their starting thresholds and leaves
//! the decision to the architect, so no sentence here says "should", "recommend" or "must". No em
//! dash, no en dash, no serial comma. Tests pin all of it.

use super::{
    FixCount, Outcome, RewriteAdvisory, FIX_HOURS_PER_KLOC_OVER, MAX_UNESTIMATED_PERCENT,
    MIN_CODE_LINES, ONE_CAUSE_OVER_PERCENT, STRUCTURAL_SHARE_OVER_PERCENT, TOP_CAUSES_OVER_PERCENT,
    UNSETTLED_AREAS_OVER_PERCENT, UNSETTLED_RULES_OVER_PERCENT,
};

/// The heading of the Markdown file and of the cockpit panel.
pub const TITLE: &str = "Rewrite advisory (internal)";

/// The opening line when two or more signals are over.
const FIRED_OPENING: &str = "Worth a conversation: fix or rewrite.";
/// The opening line when fewer than two are over.
pub(super) const NOT_FIRED: &str = "No rewrite signal this run.";
/// The opening line when a guard holds.
pub(super) const NOT_ENOUGH: &str = "Not enough to say whether there is a rewrite signal.";

pub(super) const NO_REVIEW_TREE: &str = "The run has no review tree, so it has no Fixes to count.";
pub(super) const NO_SCAN: &str = "The run did not complete: no scan ran.";
pub(super) const AI_NOT_RUN: &str = "The run did not complete: the AI review did not run.";
pub(super) const AI_FAILED: &str = "The run did not complete: the AI review failed.";
pub(super) const REPO_NOT_INSPECTED: &str =
    "The run did not complete: a repository was not inspected.";
pub(super) const RUN_STOPPED: &str =
    "The run did not complete: the spend limit or an outage stopped it.";

/// `12,345`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `18.0`, always one decimal.
fn dec(x: f64) -> String {
    format!("{x:.1}")
}

fn word(n: usize) -> String {
    match n {
        0 => "None".to_string(),
        1 => "One".to_string(),
        2 => "Two".to_string(),
        3 => "Three".to_string(),
        4 => "Four".to_string(),
        n => n.to_string(),
    }
}

fn verdict(over: bool) -> &'static str {
    if over {
        "Over its threshold."
    } else {
        "Not over its threshold."
    }
}

pub(super) fn few_lines(code_lines: usize) -> String {
    format!(
        "Fewer than {} counted lines: this run counted {}.",
        thousands(MIN_CODE_LINES),
        thousands(code_lines)
    )
}

pub(super) fn many_unestimated(fixes: FixCount) -> String {
    format!(
        "{} of {} Fixes ({} percent) have no effort estimate, over the {} percent limit.",
        fixes.without_estimate,
        fixes.counted,
        dec(super::percent(
            fixes.without_estimate as u64,
            fixes.counted as u64
        )),
        MAX_UNESTIMATED_PERCENT
    )
}

/// The four signal lines, A to D.
fn signal_lines(a: &RewriteAdvisory) -> Vec<String> {
    let s = &a.rewrite_signals;
    let hours = &s.fix_hours_per_kloc;
    let structural = &s.structural_share;
    let areas = &s.unsettled_areas;
    let repeat = &s.repeat_cause_share;
    let largest = match &repeat.largest_high {
        Some(c) => format!(
            "the largest high-effort Fix covers {} percent of places",
            dec(c.percent)
        ),
        None => "no Fix has a high effort estimate".to_string(),
    };
    let top = match repeat.top_three.fixes.len() {
        0 => "no Fix has a medium or high effort estimate".to_string(),
        1 => format!(
            "the one medium or high Fix covers {} percent",
            dec(repeat.top_three.percent)
        ),
        n => format!(
            "the top {n} medium or high Fixes cover {} percent",
            dec(repeat.top_three.percent)
        ),
    };
    vec![
        format!(
            "Fix hours against code size: {} to {} hours per 1,000 counted lines. Starting \
             threshold: over {FIX_HOURS_PER_KLOC_OVER} at the low end. {}",
            dec(hours.low),
            dec(hours.high),
            verdict(hours.over)
        ),
        format!(
            "Structural share: {} percent of the Fix hours are on architectural rules or in the \
             Architecture area. Starting threshold: over {STRUCTURAL_SHARE_OVER_PERCENT} percent. \
             {}",
            dec(structural.percent),
            verdict(structural.over)
        ),
        format!(
            "Areas without a settled habit: {} of {} assessed areas have a departure or no \
             convention in over {UNSETTLED_RULES_OVER_PERCENT} percent of their assessed rules. \
             Starting threshold: over {UNSETTLED_AREAS_OVER_PERCENT} percent of the areas. {}",
            areas.unsettled,
            areas.assessed,
            verdict(areas.over)
        ),
        format!(
            "One cause repeated widely: {largest} and {top}. Starting thresholds: over \
             {ONE_CAUSE_OVER_PERCENT} percent for one high-effort Fix, over \
             {TOP_CAUSES_OVER_PERCENT} percent for the top three. {}",
            verdict(repeat.over)
        ),
    ]
}

fn context_line(a: &RewriteAdvisory) -> String {
    let n = a.context.critical_high_fixes;
    format!(
        "Context, not a signal: {} critical or high {} per 1,000 counted lines ({n} in all).",
        dec(a.context.per_kloc),
        if n == 1 { "Fix" } else { "Fixes" }
    )
}

fn understated_line(files: usize) -> String {
    format!(
        "Understated: {files} {} not reviewed.",
        if files == 1 { "file" } else { "files" }
    )
}

fn caveat_line(without_estimate: usize) -> String {
    let left_out = match without_estimate {
        0 => "Every Fix has an estimate.".to_string(),
        1 => "1 Fix has no estimate and is left out.".to_string(),
        n => format!("{n} Fixes have no estimate and are left out."),
    };
    format!(
        "These thresholds are guesses, not measured. Effort figures are rough bands. {left_out} \
         The decision is yours."
    )
}

/// The text of `a`, one line per sentence group.
pub(super) fn lines(a: &RewriteAdvisory) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match a.outcome {
        Outcome::NotEnoughToSay => {
            out.push(NOT_ENOUGH.to_string());
            out.extend(a.guards.iter().map(|g| g.detail.clone()));
            return out;
        }
        Outcome::Fired => out.push(format!(
            "{FIRED_OPENING} {} of four signals are over their starting thresholds.",
            word(a.signals_over)
        )),
        Outcome::NotFired => out.push(NOT_FIRED.to_string()),
    }
    out.extend(signal_lines(a));
    out.push(context_line(a));
    if a.files_not_reviewed > 0 {
        out.push(understated_line(a.files_not_reviewed));
    }
    out.push(caveat_line(a.fixes.without_estimate));
    out
}

/// The readable form saved beside the JSON.
pub fn render_md(a: &RewriteAdvisory) -> String {
    let run = if a.run_id.is_empty() {
        "an unsaved run".to_string()
    } else {
        format!("run {}", a.run_id)
    };
    let mut out = format!(
        "# {TITLE}\n\nFor {run}, finished {}. {} counted lines. Operator only: this file is \
         never part of an export.\n",
        if a.finished_at.is_empty() {
            "at an unrecorded time"
        } else {
            &a.finished_at
        },
        thousands(a.code_lines)
    );
    for line in &a.lines {
        out.push('\n');
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(2_000), "2,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
