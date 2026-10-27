//! The two report renderings: plain text for a person (or a scheduled routine's post) and JSON for
//! a machine. Both come from the same [`SourceCheckReport`].

use std::fmt::Write as _;

use crate::check::{LinkFinding, RuleRef, SourceCheckReport};
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
        "  Cited web URLs: {} (ok {}, redirected {}, dead {}, unreachable {})",
        c.web_urls, c.ok, c.redirected, c.dead, c.unreachable
    );
    let _ = writeln!(
        out,
        "  Internal sources not fetched: {}",
        c.internal_sources_skipped
    );
    let _ = writeln!(
        out,
        "  Cited sections: {} (unchanged {}, changed and not yet reviewed {}, new baseline {}, \
         anchor not found {})",
        c.sections_checked,
        c.sections_unchanged,
        c.sections_changed,
        c.sections_new_baseline,
        c.sections_anchor_missing
    );

    write_links(&mut out, "DEAD LINKS", &report.dead);

    if !report.changed.is_empty() {
        let _ = writeln!(
            out,
            "\nCHANGED SECTIONS, NOT YET REVIEWED ({}; listed every run until accepted with \
             `camerata corpus check-sources --accept <url>` or `--accept-all`)",
            report.changed.len()
        );
        for ch in &report.changed {
            let _ = writeln!(out, "  {}", ch.url);
            let _ = writeln!(out, "    cited by: {}", ch.rules.join(", "));
            let _ = writeln!(out, "    first seen changed: {}", ch.first_seen);
            let _ = writeln!(out, "    {} -> {}", ch.old_hash, ch.new_hash);
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
        "REDIRECTED (update the cited URL)",
        &report.redirected,
    );

    if !report.anchor_missing.is_empty() {
        let _ = writeln!(
            out,
            "\nANCHOR NOT FOUND ({}; the page may build its content with JavaScript)",
            report.anchor_missing.len()
        );
        for m in &report.anchor_missing {
            let _ = writeln!(out, "  {}", m.url);
            let _ = writeln!(out, "    cited by: {}", m.rules.join(", "));
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
