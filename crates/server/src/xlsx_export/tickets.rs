//! `csv/tickets.csv`: one ticket per sub-group (design 3B item 4, D16), a file a client can import
//! into Jira, Linear or GitHub Issues.
//!
//! Generic columns, one row per sub-group with something left to fix. A false positive or an
//! ignored site is on no ticket. The sites a ticket names are listed inline up to
//! [`INLINE_SITES`], then the cell points at the Sites sheet filter that lists the rest.

use super::subgroups::{in_plan_order, subgroup_facts, SubgroupFacts};
use crate::report_export::ReviewedReport;

/// The archive path of the ticket file.
pub const TICKETS_CSV: &str = "csv/tickets.csv";

/// The most sites a ticket's Sites cell lists before it points at the Sites sheet.
pub const INLINE_SITES: usize = 50;

/// The ticket file's columns, in order.
pub const TICKET_HEADERS: [&str; 8] = [
    "Title",
    "Description",
    "Priority",
    "Labels",
    "Estimate",
    "Group id",
    "Site count",
    "Sites",
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
fn estimate_of(effort: &str, sites: usize) -> String {
    if effort.is_empty() {
        return "not estimated this run".to_string();
    }
    let (_, band) = crate::report_export::effort_hours_bounds(Some(effort));
    if sites > 1 {
        format!("{band} per site")
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

fn ticket_row(g: &SubgroupFacts) -> [String; 8] {
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
        description.push(format!("Why these sites belong together: {}", g.cause));
    }
    if !g.fix.is_empty() {
        description.push(format!("Fix: {}", g.fix));
    }
    if !g.worked_example.is_empty() {
        description.push(format!("Worked example: {}", g.worked_example));
    }
    description.push(format!(
        "{} {}, listed as rows below and on the Sites sheet (filter Group = {}).",
        g.open_site_total,
        if g.open_site_total == 1 {
            "site"
        } else {
            "sites"
        },
        g.id
    ));
    let mut labels = vec!["camerata".to_string(), label_part(&g.rule_id)];
    let category = label_part(&g.category);
    if !category.is_empty() {
        labels.push(category);
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
            "\nand {} more rows, see Sites sheet, filter Group = {}",
            g.open.len() - INLINE_SITES,
            g.id
        ));
    }
    [
        title,
        description.join("\n\n"),
        priority_of(&g.severity).to_string(),
        labels.join(","),
        estimate_of(&g.effort, g.open.len()),
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
    let groups = in_plan_order(subgroup_facts(reviewed, &rows, &deps, corpus));
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer.write_record(TICKET_HEADERS)?;
    for g in &groups {
        writer.write_record(ticket_row(g))?;
    }
    Ok(writer.into_inner()?)
}
