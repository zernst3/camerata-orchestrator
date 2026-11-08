//! `csv/tickets.csv`: one ticket per Fix (design 3B item 4, D16, item #1), a file a client can
//! import into Jira, Linear or GitHub Issues. A Fix is one change, so one committed secret flagged
//! by six rules is one ticket that lists every rule and its citation.
//!
//! Generic columns, one row per Fix with something left to fix. A false positive or an
//! ignored place is on no ticket. The places a ticket names are listed inline up to
//! [`INLINE_SITES`], then the cell points at the All problems sheet filter that lists the rest.

use super::subgroups::{plan_facts, FixFacts};
use super::vocabulary as v;
use crate::report_export::ReviewedReport;

/// The archive path of the ticket file.
pub const TICKETS_CSV: &str = "csv/tickets.csv";

/// The most places a ticket's Places cell lists before it points at the All problems sheet.
pub const INLINE_SITES: usize = 50;

/// The ticket file's columns, in order. Every column that names a concept takes its word from
/// the vocabulary, so a ticket says "Places" for the count, as the workbook does, "Place list"
/// for the places it names and "Fix id" for the Fix a ticket is.
pub const TICKET_HEADERS: [&str; 8] = [
    "Title",
    "Description",
    "Priority",
    "Labels",
    "Estimate",
    v::H_FIX_ID,
    v::H_PLACES,
    v::H_PLACE_LIST,
];

fn priority_of(severity: &str) -> &'static str {
    match severity.to_ascii_lowercase().as_str() {
        "critical" => "Highest",
        "high" => "High",
        "medium" => "Medium",
        _ => "Low",
    }
}

/// `low` effort as the band the report states for a finding of that effort.
fn estimate_of(effort: &str, places: usize) -> String {
    if effort.is_empty() {
        return "not estimated this run".to_string();
    }
    let (_, band) = crate::report_export::effort_hours_bounds(Some(effort));
    if places > 1 {
        format!("{band} {}", v::PER_PLACE)
    } else {
        band
    }
}

fn label_part(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    out
}

fn ticket_row(g: &FixFacts) -> [String; 8] {
    let title_tail = if g.cause.is_empty() {
        g.label.clone()
    } else {
        g.cause.clone()
    };
    let title = format!("{}: {title_tail}", g.rule_title);
    let mut description = Vec::new();
    if !g.summary.is_empty() {
        description.push(g.summary.clone());
    }
    if !g.cause.is_empty() {
        description.push(format!("Why these places belong together: {}", g.cause));
    }
    // Every rule the ticket closes, with its citation: nothing a Fix satisfies is left out.
    let closes: Vec<String> = g
        .rules
        .iter()
        .map(|r| {
            let mut line = format!("- {} ({})", r.line(g.open_site_total), r.rule_id);
            if !r.citation.is_empty() {
                line.push_str(&format!(", {}", r.citation));
            }
            line
        })
        .collect();
    description.push(format!("{}\n{}", v::TICKET_CLOSES, closes.join("\n")));
    if !g.fix.is_empty() {
        description.push(format!("Fix: {}", g.fix));
    }
    if !g.worked_example.is_empty() {
        description.push(format!("Example from your code: {}", g.worked_example));
    }
    description.push(format!(
        "{} {}, listed as rows below and on the {} sheet (filter {} = {}).",
        g.open_site_total,
        v::places_word(g.open_site_total),
        v::ALL_PROBLEMS,
        v::H_FIX_ID,
        g.id
    ));
    let mut labels = vec!["camerata".to_string()];
    for rule in &g.rules {
        let part = label_part(&rule.rule_id);
        if !part.is_empty() && !labels.contains(&part) {
            labels.push(part);
        }
    }
    for area in g.category.split(", ") {
        let category = label_part(area);
        if !category.is_empty() && !labels.contains(&category) {
            labels.push(category);
        }
    }
    let shown: Vec<String> = g
        .open
        .iter()
        .take(INLINE_SITES)
        .map(|s| {
            if s.line > 0 {
                format!("{}:{}", s.path, s.line)
            } else {
                s.path.clone()
            }
        })
        .collect();
    let mut sites = shown.join("\n");
    if g.open.len() > INLINE_SITES {
        sites.push_str(&format!(
            "\nand {} more rows, see the {} sheet, filter {} = {}",
            g.open.len() - INLINE_SITES,
            v::ALL_PROBLEMS,
            v::H_FIX_ID,
            g.id
        ));
    }
    [
        title,
        description.join("\n\n"),
        priority_of(&g.severity).to_string(),
        labels.join(","),
        estimate_of(&g.effort, g.open_site_total),
        g.id.clone(),
        g.open_site_total.to_string(),
        sites,
    ]
}

/// The bytes of `csv/tickets.csv` for `reviewed`.
pub fn tickets_csv(
    reviewed: &ReviewedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_options: &std::collections::HashMap<String, String>,
) -> anyhow::Result<Vec<u8>> {
    let (rows, deps) = super::partition_rows_for(reviewed, corpus, chosen_options, false);
    // The tickets are the plan: the same Fixes, in the same order.
    let groups = plan_facts(reviewed, &rows, &deps, corpus);
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer.write_record(TICKET_HEADERS)?;
    for g in &groups {
        writer.write_record(ticket_row(g))?;
    }
    Ok(writer.into_inner()?)
}
