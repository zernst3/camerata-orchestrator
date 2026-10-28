//! The two report renderings: plain text for a person (or a scheduled routine's post) and JSON for
//! a machine. Both come from the same [`SourceCheckReport`].

use std::fmt::Write as _;

use crate::check::{AnchorProblem, LinkFinding, RuleRef, SourceCheckReport};
use crate::diff::DiffOp;

/// Longest diff segment shown in the text report; the JSON keeps it whole.
const SEGMENT_PREVIEW_CHARS: usize = 400;

fn clip(text: &str) -> String {
    if text.chars().count() <= SEGMENT_PREVIEW_CHARS {
        text.to_string()
    } else {
        let head: String = text.chars().take(SEGMENT_PREVIEW_CHARS).collect();
        format!("{head}...")
    }
}

fn write_links(out: &mut String, title: &str, findings: &[LinkFinding]) {
    if findings.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title} ({})", findings.len());
    for f in findings {
        let _ = writeln!(out, "  {}", f.url);
        if !f.detail.is_empty() {
            let _ = writeln!(out, "    {}", f.detail);
        }
        if let Some(to) = &f.final_url {
            let _ = writeln!(out, "    now at {to}");
        }
        let _ = writeln!(out, "    cited by: {}", f.rules.join(", "));
    }
}

fn write_rules(out: &mut String, title: &str, rules: &[RuleRef]) {
    if rules.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title} ({})", rules.len());
    for r in rules {
        let _ = writeln!(out, "  {} [{}] {}", r.id, r.domain, r.title);
    }
}

/// The plain-text report: date-stamped, counts first, then dead links, changed sections with
/// their diffs, rules with no source, and the lower-urgency lists.
pub fn render_text(report: &SourceCheckReport) -> String {
    let c = &report.counts;
    let mut out = String::new();
    let _ = writeln!(out, "CORPUS SOURCE CHECK, {}", report.checked_on);
    let _ = writeln!(out, "\nCounts");
    let _ = writeln!(
        out,
        "  Rules: {} ({} with no source, {} citing internal documents only)",
        c.rules, c.rules_without_source, c.rules_internal_sources_only
    );
    let _ = writeln!(
        out,
        "  Cited web URLs: {} (ok {}, redirected {}, dead {}, unreachable {}, refused {})",
        c.web_urls, c.ok, c.redirected, c.dead, c.unreachable, c.refused
    );
    let _ = writeln!(
        out,
        "  Internal sources not fetched: {}",
        c.internal_sources_skipped
    );
    let _ = writeln!(
        out,
        "  Cited sections: {} (unchanged {}, changed and not yet reviewed {}, unreviewed from \
         earlier runs and not re-read {}, new baseline {}, anchor not found {}, not verified {})",
        c.sections_checked,
        c.sections_unchanged,
        c.sections_changed,
        c.sections_pending_carried,
        c.sections_new_baseline,
        c.sections_anchor_missing,
        c.sections_unverified
    );

    write_links(&mut out, "DEAD LINKS", &report.dead);

    if !report.changed.is_empty() {
        let _ = writeln!(
            out,
            "\nCHANGED SECTIONS, NOT YET REVIEWED ({}; listed every run until accepted with \
             `camerata corpus check-sources --accept <url>=<hash>` or `--accept-all`)",
            report.changed.len()
        );
        for ch in &report.changed {
            let _ = writeln!(out, "  {}", ch.url);
            let _ = writeln!(out, "    cited by: {}", ch.rules.join(", "));
            let _ = writeln!(out, "    first seen changed: {}", ch.first_seen);
            if ch.carried_over {
                let _ = writeln!(
                    out,
                    "    (not re-read this run: carried over from an earlier run)"
                );
            }
            let _ = writeln!(
                out,
                "    accepted text {} -> pending text {}",
                ch.old_hash, ch.new_hash
            );
            let _ = writeln!(
                out,
                "    accept exactly this version: --accept \"{}={}\"",
                ch.url, ch.new_hash
            );
            if ch.diff.is_empty() {
                let _ = writeln!(
                    out,
                    "    (the text differs beyond what the stored excerpt shows; read the page)"
                );
            }
            for line in &ch.diff {
                let mark = match line.op {
                    DiffOp::Removed => '-',
                    DiffOp::Added => '+',
                };
                let _ = writeln!(out, "    {mark} {}", clip(&line.text));
            }
            if ch.beyond_excerpt && !ch.diff.is_empty() {
                let _ = writeln!(
                    out,
                    "    (long section: changes past the excerpt are not shown)"
                );
            }
        }
    }

    write_rules(
        &mut out,
        "RULES WITH NO SOURCE",
        &report.rules_without_source,
    );
    write_links(
        &mut out,
        "UNREACHABLE (not a verdict on the page)",
        &report.unreachable,
    );
    write_links(
        &mut out,
        "REFUSED BY THE NETWORK POLICY (nothing was requested; not a verdict on the page)",
        &report.refused,
    );
    write_links(
        &mut out,
        "REDIRECTED (update the cited URL)",
        &report.redirected,
    );

    if !report.anchor_missing.is_empty() {
        let _ = writeln!(
            out,
            "\nSECTION NOT CHECKED ({}; the anchor is missing, which may mean the page builds its \
             content with JavaScript, or the page was too large to read in full)",
            report.anchor_missing.len()
        );
        for m in &report.anchor_missing {
            let _ = writeln!(out, "  {}", m.url);
            let _ = writeln!(out, "    cited by: {}", m.rules.join(", "));
            let why = match m.reason {
                AnchorProblem::NotFound => "anchor not found in the page",
                AnchorProblem::PageTruncated => {
                    "page larger than the download limit; nothing was hashed"
                }
            };
            let _ = writeln!(out, "    {why}");
        }
    }
    write_rules(
        &mut out,
        "RULES CITING INTERNAL DOCUMENTS ONLY",
        &report.rules_internal_sources_only,
    );
    out
}

/// The JSON report, pretty-printed.
pub fn render_json(report: &SourceCheckReport) -> serde_json::Result<String> {
    serde_json::to_string_pretty(report)
}
