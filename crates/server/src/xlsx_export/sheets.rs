//! The sheets of the client workbook.
//!
//! # Fold map (which old sheet became which)
//!
//! | Old sheet | New sheet |
//! |---|---|
//! | Remediation plan | Fix plan |
//! | All Findings, the Primary rows of Sites, the per-area sheets (one for each area) | All problems (the Area column filters what a per-area sheet showed) |
//! | Every row of Sites | All source rows |
//! | Held rows (the held-for-review part of All Findings) | Needs a decision |
//! | Conventions | Your conventions |
//! | Deviations | Departures |
//! | Dependencies | Dependencies (its coverage notes moved to Not checked) |
//! | Coverage: the per-rule table and the Rules applied block | Rules checked |
//! | Coverage: the could-not-complete, not-run and not-reviewed blocks | Not checked |
//! | Patterns across services | Differences across services |
//! | False Positives, Thrown out by the engine | Ruled out |
//! | Index | Start here (the counts and the cover facts) and Glossary (the legend) |
//! | Tree | How problems were grouped |
//! | Decisions | Reviewer decisions |
//!
//! Every name comes from [`super::vocabulary`]; nothing here spells a sheet or a shared column
//! for itself.

use std::collections::{BTreeMap, HashMap};

use rust_xlsxwriter::{Color, Workbook, XlsxError};

use super::subgroups::{Sequence, SubgroupFacts};
use super::tables::{Cell, Col, Table};
use super::vocabulary as v;
use super::{
    flagged_here_notes, kind_sheet, rule_title, severity_rank, site_list_cell, site_locations,
    title_case, CappedSheet, ConventionRow, CoverageRow, DepRow, DeviationGroupRow, FindingRow,
    Formats, KindSheet, NotAssessedRow, RulesAppliedRow, SiteParent, SiteRow,
};
use crate::report_export::Disposition;

// ── Which rows are problems, and in what order ──────────────────────────────────────────

/// THE seam for the representative rows: the one place that decides which engine rows stand for a
/// problem on every sheet except All source rows.
///
/// Today the engine collapses rows of one rule at one place and keeps the others under the same
/// Place id, so each row it hands over is already a Primary row. The cross-rule merge (one row per
/// place listing every rule) is a separate later branch: it replaces this function, and every
/// sheet follows because none of them filters rows for itself.
pub(super) fn representative_rows(rows: &[FindingRow]) -> Vec<&FindingRow> {
    rows.iter()
        .filter(|r| !r.is_fp && kind_sheet(r) == KindSheet::Findings)
        .collect()
}

/// Everything the sheets need to know about each other, worked out once: where a problem sits on
/// each sheet, so a link on one sheet can name a row of another.
pub(super) struct Layout<'a> {
    pub internal: bool,
    pub multi_repo: bool,
    pub plan: &'a [SubgroupFacts],
    /// The problems in the order All problems prints them.
    pub problems: Vec<&'a FindingRow>,
    /// Place id to the sheet and zero-based row that stands for it.
    pub placement: HashMap<String, (String, u32)>,
    /// Fix id to its zero-based row on Fix plan, and its position in plan order.
    pub fix_at: HashMap<String, (u32, usize)>,
    /// Fix id to the first row of its places on All problems.
    pub first_problem_of_fix: HashMap<String, u32>,
    /// Rule id to the first row of its problems on All problems.
    pub first_problem_of_rule: HashMap<String, u32>,
}

impl<'a> Layout<'a> {
    pub(super) fn new(
        rows: &'a [FindingRow],
        plan: &'a [SubgroupFacts],
        internal: bool,
        multi_repo: bool,
    ) -> Self {
        let fix_at: HashMap<String, (u32, usize)> = plan
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id.clone(), (i as u32 + 1, i)))
            .collect();
        let mut problems = representative_rows(rows);
        problems.sort_by_key(|r| fix_order_key(r, &fix_at, plan));
        let mut placement = HashMap::new();
        let mut first_problem_of_fix = HashMap::new();
        let mut first_problem_of_rule = HashMap::new();
        for (i, row) in problems.iter().enumerate() {
            let at = i as u32 + 1;
            placement
                .entry(row.facts.site_id.clone())
                .or_insert((v::ALL_PROBLEMS.to_string(), at));
            if !row.facts.group.is_empty() {
                first_problem_of_fix
                    .entry(row.facts.group.clone())
                    .or_insert(at);
            }
            first_problem_of_rule
                .entry(row.rule_id.clone())
                .or_insert(at);
        }
        Self {
            internal,
            multi_repo,
            plan,
            problems,
            placement,
            fix_at,
            first_problem_of_fix,
            first_problem_of_rule,
        }
    }
}

/// The sort key of a problem: its fix order, then the position of its fix in the plan, then the
/// worst severity first.
fn fix_order_key(
    row: &FindingRow,
    fix_at: &HashMap<String, (u32, usize)>,
    plan: &[SubgroupFacts],
) -> (Sequence, usize, u8, u8, String, String, usize) {
    let (sequence, position) = match fix_at.get(&row.facts.group) {
        Some((_, i)) => (plan[*i].sequence, *i),
        None => (Sequence::of(&row.severity, &row.effort), usize::MAX),
    };
    (
        sequence,
        position,
        severity_rank(&row.severity),
        crate::report_export::provenance_tier_rank(&row.citation_kind),
        row.repo.clone(),
        row.path.clone(),
        row.line,
    )
}

/// The fix order word of a row.
fn fix_order_word(row: &FindingRow, lay: &Layout) -> &'static str {
    match lay.fix_at.get(&row.facts.group) {
        Some((_, i)) => lay.plan[*i].sequence.label(),
        None => Sequence::of(&row.severity, &row.effort).label(),
    }
}

// ── Row cells, assembled by header ──────────────────────────────────────────────────────

/// The cells of one row keyed by their header, so a column list can be changed without a cell
/// landing in the wrong column.
#[derive(Default)]
pub(super) struct RowCells(Vec<(String, Cell)>);

impl RowCells {
    pub(super) fn put(&mut self, header: &str, cell: Cell) -> &mut Self {
        self.0.retain(|(h, _)| h != header);
        self.0.push((header.to_string(), cell));
        self
    }

    fn get(&self, header: &str) -> Option<&Cell> {
        self.0.iter().find(|(h, _)| h == header).map(|(_, c)| c)
    }

    /// The cells in `cols` order; a column with no cell is empty.
    pub(super) fn assemble(&self, cols: &[Col]) -> Vec<Cell> {
        cols.iter()
            .map(|c| self.get(&c.header).cloned().unwrap_or(Cell::Empty))
            .collect()
    }
}

fn yes(b: bool) -> Cell {
    if b {
        Cell::text(v::YES)
    } else {
        Cell::Empty
    }
}

fn line_cell(line: usize) -> Cell {
    if line == 0 {
        Cell::text(v::WHOLE_FILE)
    } else {
        Cell::Num(line as f64)
    }
}

/// What a reader sees for the engine's provenance label: Camerata's own check, a model's reading
/// or an outside tool's pass.
pub(super) fn source_label(provenance: &str) -> String {
    if let Some(tool) = provenance.strip_prefix("Preview: ") {
        format!("{}{tool}", v::SOURCE_TOOL_PREFIX)
    } else if provenance == "Deterministic" {
        v::SOURCE_DETERMINISTIC.to_string()
    } else if provenance.is_empty() {
        String::new()
    } else {
        v::SOURCE_MODEL_REVIEWED.to_string()
    }
}

/// How a rule was checked, from its citation kind.
pub(super) fn how_checked_label(citation_kind: &str) -> &'static str {
    match citation_kind {
        "grounded" => v::SOURCE_DETERMINISTIC,
        "preview" => v::SOURCE_TOOL,
        _ => v::SOURCE_MODEL_REVIEWED,
    }
}

/// The confidence as a reader sees it: High, or Low when the review was unsure.
fn confidence_label(raw: &str) -> String {
    match raw {
        "" => String::new(),
        "needs-review" => "Low".to_string(),
        other => title_case(other),
    }
}

fn review_level_label(wire: &str) -> &'static str {
    if wire == "personally_reviewed" {
        "reviewed in person"
    } else if wire.starts_with("reviewed_as_pattern") {
        "reviewed with others like it"
    } else if wire == "engine_verified" {
        "engine verified"
    } else {
        ""
    }
}

/// What a reviewer has already decided about a problem, in plain words. An open problem has no
/// decision yet.
fn decision_so_far(row: &FindingRow) -> String {
    if !row.facts.merged_into.is_empty() {
        return row.disposition_label.clone();
    }
    match row.disposition_kind {
        None | Some(Disposition::Unresolved) => String::new(),
        Some(Disposition::Confirmed) => "Confirmed by the reviewer".to_string(),
        Some(_) => row.disposition_label.clone(),
    }
}

/// Why a problem waits for a reviewer's ruling. Empty for a problem that does not.
fn why_waiting(row: &FindingRow, internal: bool) -> String {
    if !row.needs_review {
        return String::new();
    }
    let mut why = match row.bucket {
        "held" => {
            "The review was unsure, or no published standard backs it, so a reviewer must rule \
             before you act on it."
        }
        "informational" => {
            "It reads as a convention to consider, so a reviewer must decide whether it applies \
             to your code."
        }
        _ => "The review was unsure it is a real problem, so a reviewer must rule on it.",
    }
    .to_string();
    if internal && !row.held.is_empty() {
        why.push_str(&format!(" Held because: {}.", row.held));
    }
    why
}

/// The rules a problem breaks: its own rule first, then each other rule that flagged the same
/// place, as titles.
fn rules_cell(row: &FindingRow) -> Cell {
    let mut lines = vec![row.rule_title.clone()];
    if !row.also_matches.is_empty() {
        lines.push(row.also_matches.clone());
    }
    Cell::wrap(lines.join("\n"))
}

/// Every rule id behind a row: its own, the ones collapsed into it and the ones absorbed.
fn rule_ids_cell(row: &FindingRow) -> Cell {
    let mut ids = vec![row.rule_id.clone()];
    for b in &row.flagged_by {
        if !ids.contains(&b.rule_id) {
            ids.push(b.rule_id.clone());
        }
    }
    for l in &row.also_locations {
        if !l.rule_id.is_empty() && !ids.contains(&l.rule_id) {
            ids.push(l.rule_id.clone());
        }
    }
    Cell::wrap(ids.join(", "))
}

fn also_found_cell(row: &FindingRow) -> String {
    let listed = site_locations(row)
        .into_iter()
        .skip(usize::from(!row.path.is_empty()))
        .chain(flagged_here_notes(row))
        .collect::<Vec<_>>();
    let mut cell = site_list_cell(&listed);
    if !row.same_credential_note.is_empty() {
        if !cell.is_empty() {
            cell.push('\n');
        }
        cell.push_str(&row.same_credential_note);
    }
    cell
}

// ── Problems ────────────────────────────────────────────────────────────────────────────

fn problem_cols(lay: &Layout, waiting: bool) -> Vec<Col> {
    let mut cols = vec![
        Col::plain(v::H_FIX_ORDER, 10.0),
        Col::plain(v::H_SEVERITY, 10.0),
        Col::plain(v::H_PROBLEM, 60.0),
        Col::plain(v::H_FILE, 40.0),
        Col::plain(v::H_LINE, 10.0),
        Col::plain(v::H_REPO, 18.0),
        Col::plain(v::H_RULES, 40.0),
        Col::plain(v::H_AREA, 18.0),
        Col::plain(v::H_WHAT_TO_DO_HERE, 70.0),
        Col::plain(v::H_FIX_TYPE, 14.0),
        Col::plain(v::H_EFFORT, 10.0),
        Col::plain(v::H_ESTIMATED_HOURS, 16.0),
        Col::plain(v::H_CONFIDENCE, 12.0),
        Col::plain(v::H_REVIEWED_IN_PERSON, 12.0),
        Col::plain(v::H_NEEDS_A_DECISION, 12.0),
        Col::plain(v::H_DECISION_SO_FAR, 30.0),
    ];
    if waiting {
        cols.push(Col::plain(v::H_WHY_IT_IS_WAITING, 60.0));
    }
    cols.extend([
        Col::plain(v::H_SOURCE, 16.0),
        Col::plain(v::H_STANDARD_CITED, 34.0),
        Col::plain(v::H_CODE_SEEN, 60.0),
        Col::plain(v::H_MORE_DETAIL, 60.0),
        Col::plain(v::H_FIX, 36.0),
        Col::plain(v::H_PLACES, 8.0),
        Col::plain(v::H_ALSO_FOUND_AT, 44.0),
        Col::plain(v::H_SEVERITY_NOTE, 44.0),
    ]);
    cols.extend([
        Col::technical(v::H_PLACE_ID, 18.0),
        Col::technical(v::H_FIX_ID, 30.0),
        Col::technical(v::H_RULE_IDS, 30.0),
        Col::technical(v::H_KIND, 18.0),
        Col::technical(v::H_STATUS, 18.0),
        Col::technical(v::H_CARRIED_FROM, 14.0),
        Col::technical(v::H_REVIEW_LEVEL, 30.0),
        Col::technical(v::H_MERGED_INTO, 18.0),
        Col::technical(v::H_REVIEWER_EDITED, 10.0),
        Col::technical(v::H_EDIT_IDS, 22.0),
        Col::technical(v::H_CITATION_LINKS, 34.0),
    ]);
    if lay.internal {
        cols.extend([
            Col::technical(v::H_REVIEW_FLAGS, 36.0),
            Col::technical(v::H_HELD, 40.0),
            Col::technical(v::H_ESCALATED, 10.0),
        ]);
        for h in crate::report_export::row_facts::INTERNAL_TRIAGE_HEADERS {
            cols.push(Col::technical(h, 28.0));
        }
    }
    cols
}

fn problem_cells(row: &FindingRow, lay: &Layout) -> RowCells {
    let mut c = RowCells::default();
    c.put(v::H_FIX_ORDER, Cell::text(fix_order_word(row, lay)))
        .put(v::H_SEVERITY, Cell::Severity(row.severity.clone()))
        .put(v::H_PROBLEM, Cell::wrap(&row.headline))
        .put(v::H_FILE, Cell::text(&row.path))
        .put(v::H_LINE, line_cell(row.line))
        .put(v::H_REPO, Cell::text(&row.repo))
        .put(v::H_RULES, rules_cell(row))
        .put(v::H_AREA, Cell::text(&row.category))
        .put(
            v::H_WHAT_TO_DO_HERE,
            Cell::wrap(
                row.fix_specific
                    .as_deref()
                    .or(row.fix.as_deref())
                    .unwrap_or(""),
            ),
        )
        .put(v::H_FIX_TYPE, Cell::text(&row.fix_type))
        .put(v::H_EFFORT, Cell::text(&row.effort))
        .put(v::H_ESTIMATED_HOURS, Cell::text(&row.est_hours))
        .put(
            v::H_CONFIDENCE,
            Cell::text(confidence_label(&row.confidence)),
        )
        .put(
            v::H_REVIEWED_IN_PERSON,
            yes(row.facts.review_level == "personally_reviewed"),
        )
        .put(v::H_NEEDS_A_DECISION, yes(row.needs_review))
        .put(v::H_DECISION_SO_FAR, Cell::wrap(decision_so_far(row)))
        .put(
            v::H_WHY_IT_IS_WAITING,
            Cell::wrap(why_waiting(row, lay.internal)),
        )
        .put(v::H_SOURCE, Cell::text(source_label(&row.provenance)))
        .put(v::H_STANDARD_CITED, Cell::wrap(&row.citation_label))
        .put(v::H_CODE_SEEN, Cell::Mono(row.snippet.clone()))
        .put(v::H_MORE_DETAIL, Cell::wrap(&row.detail))
        .put(v::H_PLACES, Cell::Num(row.site_count as f64))
        .put(v::H_ALSO_FOUND_AT, Cell::wrap(also_found_cell(row)))
        .put(v::H_SEVERITY_NOTE, Cell::wrap(&row.severity_adjustments))
        .put(v::H_PLACE_ID, Cell::text(&row.facts.site_id))
        .put(v::H_FIX_ID, Cell::text(&row.facts.group))
        .put(v::H_RULE_IDS, rule_ids_cell(row))
        .put(v::H_KIND, Cell::text(row.kind.tag()))
        .put(v::H_STATUS, Cell::text(&row.status))
        .put(v::H_CARRIED_FROM, Cell::text(&row.carried_from))
        .put(v::H_REVIEW_LEVEL, Cell::text(&row.facts.review_level))
        .put(v::H_MERGED_INTO, Cell::text(&row.facts.merged_into))
        .put(v::H_REVIEWER_EDITED, yes(row.reviewer_edited))
        .put(v::H_EDIT_IDS, Cell::text(row.edit_ids.join(", ")))
        .put(v::H_CITATION_LINKS, Cell::wrap(&row.citation_urls));
    if let Some((fix_row, at)) = lay.fix_at.get(&row.facts.group) {
        c.put(
            v::H_FIX,
            Cell::link(v::FIX_PLAN, *fix_row, &lay.plan[*at].label),
        );
    }
    if lay.internal {
        c.put(v::H_REVIEW_FLAGS, Cell::wrap(&row.review_flags))
            .put(v::H_HELD, Cell::wrap(&row.held))
            .put(v::H_ESCALATED, yes(row.facts.escalated));
        if let Some(triage) = &row.internal_triage {
            for (h, cell) in crate::report_export::row_facts::INTERNAL_TRIAGE_HEADERS
                .iter()
                .zip(triage.cells())
            {
                c.put(h, Cell::wrap(cell));
            }
        }
    }
    c
}

fn problems_table(name: &str, rows: &[&FindingRow], lay: &Layout, waiting: bool) -> Table {
    let cols = problem_cols(lay, waiting);
    let mut table = Table::new(name, cols);
    table.frozen_cols = 3;
    for row in rows {
        table
            .rows
            .push(problem_cells(row, lay).assemble(&table.cols));
    }
    table.empty_note = Some(
        if waiting {
            "Nothing waits for a decision: every problem can be acted on as it stands."
        } else {
            "No problems were found in this inspection."
        }
        .to_string(),
    );
    table
}

/// The All problems sheet.
pub(super) fn all_problems_table(lay: &Layout) -> Table {
    problems_table(v::ALL_PROBLEMS, &lay.problems, lay, false)
}

/// The Needs a decision sheet: the problems a reviewer must rule on, in the order All problems
/// prints them.
pub(super) fn needs_a_decision_table(lay: &Layout) -> Table {
    let waiting: Vec<&FindingRow> = lay
        .problems
        .iter()
        .copied()
        .filter(|r| r.needs_review)
        .collect();
    problems_table(v::NEEDS_A_DECISION, &waiting, lay, true)
}

// ── The fix plan ────────────────────────────────────────────────────────────────────────

pub(super) fn fix_plan_table(lay: &Layout, no_tree_note: &str) -> Table {
    let mut table = Table::new(
        v::FIX_PLAN,
        vec![
            Col::plain(v::H_ORDER, 8.0),
            Col::plain(v::H_FIX, 40.0),
            Col::plain(v::H_FIX_ORDER, 10.0),
            Col::plain(v::H_WHAT_IS_WRONG, 60.0),
            Col::plain(v::H_WHY_IT_MATTERS, 50.0),
            Col::plain(v::H_WHAT_TO_DO, 60.0),
            Col::plain(v::H_EXAMPLE, 50.0),
            Col::plain(v::H_SEVERITY, 10.0),
            Col::plain(v::H_EFFORT, 9.0),
            Col::plain(v::H_PLACES, 8.0),
            Col::plain(v::H_AREAS, 18.0),
            Col::plain(v::H_RULES, 34.0),
            Col::plain(v::H_FOLDERS, 44.0),
            Col::plain(v::H_REVIEWED_BY, 36.0),
            Col::plain(v::H_GO_TO_PLACES, 16.0),
            Col::technical(v::H_FIX_ID, 34.0),
        ],
    );
    table.frozen_cols = 2;
    for (i, g) in lay.plan.iter().enumerate() {
        let reviewed: Vec<String> = g
            .review_levels
            .iter()
            .filter_map(|(level, n)| {
                let label = review_level_label(level);
                (!label.is_empty()).then(|| format!("{n} {label}"))
            })
            .collect();
        let go = match lay.first_problem_of_fix.get(&g.id) {
            Some(row) => Cell::link(
                v::ALL_PROBLEMS,
                *row,
                format!(
                    "{} {}",
                    g.open_site_total,
                    if g.open_site_total == 1 {
                        "place"
                    } else {
                        "places"
                    }
                ),
            ),
            None => Cell::Empty,
        };
        let what_is_wrong = if g.cause.is_empty() {
            g.summary.clone()
        } else {
            g.cause.clone()
        };
        let why = if g.cause.is_empty() {
            String::new()
        } else {
            g.summary.clone()
        };
        let mut cells = RowCells::default();
        cells
            .put(v::H_ORDER, Cell::Num((i + 1) as f64))
            .put(v::H_FIX, Cell::wrap(&g.label))
            .put(v::H_FIX_ORDER, Cell::text(g.sequence.label()))
            .put(v::H_WHAT_IS_WRONG, Cell::wrap(what_is_wrong))
            .put(v::H_WHY_IT_MATTERS, Cell::wrap(why))
            .put(v::H_WHAT_TO_DO, Cell::wrap(&g.fix))
            .put(v::H_EXAMPLE, Cell::wrap(&g.worked_example))
            .put(
                v::H_SEVERITY,
                Cell::Severity(g.severity.to_ascii_lowercase()),
            )
            .put(v::H_EFFORT, Cell::text(&g.effort))
            .put(v::H_PLACES, Cell::Num(g.open_site_total as f64))
            .put(v::H_AREAS, Cell::text(&g.category))
            .put(v::H_RULES, Cell::wrap(&g.rule_title))
            .put(v::H_FOLDERS, Cell::wrap(g.folders.join("\n")))
            .put(v::H_REVIEWED_BY, Cell::wrap(reviewed.join(", ")))
            .put(v::H_GO_TO_PLACES, go)
            .put(v::H_FIX_ID, Cell::text(&g.id));
        table.rows.push(cells.assemble(&table.cols));
    }
    if lay.plan.is_empty() {
        table.empty_note = Some(if no_tree_note.is_empty() {
            "Nothing needs fixing in this inspection.".to_string()
        } else {
            no_tree_note.to_string()
        });
    }
    table
}

// ── Conventions and departures ──────────────────────────────────────────────────────────

pub(super) fn conventions_table(rows: &[ConventionRow], lay: &Layout) -> Table {
    let mut cols = vec![
        Col::plain(v::H_AREA, 18.0),
        Col::plain(v::H_WHAT_WE_CHECKED, 50.0),
        Col::plain(v::H_WHAT_YOUR_CODE_DOES, 50.0),
        Col::plain(v::H_HOW_CONSISTENTLY, 36.0),
        Col::plain(v::H_PLACES_LOOKED_AT, 10.0),
        Col::plain(v::H_RULE, 40.0),
        Col::plain(v::H_MORE_DETAIL, 60.0),
        Col::plain(v::H_ALSO_FOUND_AT, 36.0),
    ];
    if lay.multi_repo {
        cols.push(Col::plain(v::H_REPO, 30.0));
    }
    cols.extend([
        Col::technical(v::H_PLACE_ID, 18.0),
        Col::technical(v::H_RULE_ID, 28.0),
    ]);
    let mut table = Table::new(v::YOUR_CONVENTIONS, cols);
    table.frozen_cols = 2;
    for row in rows {
        let what_it_does = if row.convention_label.is_empty() {
            if row.patterns_in_use.is_empty() {
                row.headline.clone()
            } else {
                row.patterns_in_use.clone()
            }
        } else {
            row.convention_label.clone()
        };
        let mut c = RowCells::default();
        c.put(v::H_AREA, Cell::text(&row.category))
            .put(v::H_WHAT_WE_CHECKED, Cell::wrap(&row.headline))
            .put(v::H_WHAT_YOUR_CODE_DOES, Cell::wrap(what_it_does))
            .put(v::H_HOW_CONSISTENTLY, Cell::wrap(&row.status))
            .put(v::H_PLACES_LOOKED_AT, Cell::Num(row.sites as f64))
            .put(v::H_RULE, Cell::wrap(&row.rule_title))
            .put(v::H_MORE_DETAIL, Cell::wrap(&row.detail))
            .put(
                v::H_ALSO_FOUND_AT,
                Cell::wrap(site_list_cell(&row.also_found_items)),
            )
            .put(v::H_REPO, Cell::text(&row.repo))
            .put(v::H_PLACE_ID, Cell::text(&row.place_id))
            .put(v::H_RULE_ID, Cell::text(&row.rule_id));
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note = Some(
        "This inspection derived no conventions. A convention is listed here only when your code \
         follows one consistently, or when it demonstrably has none."
            .to_string(),
    );
    table
}

pub(super) fn departures_table(groups: &[DeviationGroupRow], lay: &Layout) -> Table {
    let mut cols = vec![
        Col::plain(v::H_AREA, 18.0),
        Col::plain(v::H_CONVENTION, 44.0),
        Col::plain(v::H_PLACES_DEPARTING, 14.0),
        Col::plain(v::H_PLACES_CHECKED, 14.0),
        Col::plain(v::H_WORST_SEVERITY, 16.0),
        Col::plain(v::H_WHERE, 44.0),
        Col::plain(v::H_WHAT_TO_DO, 50.0),
        Col::plain(v::H_FIX_TYPE, 14.0),
        Col::plain(v::H_RULE, 40.0),
    ];
    if lay.multi_repo {
        cols.push(Col::plain(v::H_REPO, 30.0));
    }
    cols.push(Col::technical(v::H_RULE_ID, 28.0));
    let mut table = Table::new(v::DEPARTURES, cols);
    table.frozen_cols = 2;
    for g in groups {
        let mut c = RowCells::default();
        c.put(v::H_AREA, Cell::text(&g.category))
            .put(v::H_CONVENTION, Cell::wrap(&g.convention_label))
            .put(v::H_PLACES_DEPARTING, Cell::Num(g.deviating_sites as f64))
            .put(
                v::H_PLACES_CHECKED,
                match g.assessed_sites {
                    Some(n) => Cell::Num(n as f64),
                    None => Cell::text("not counted"),
                },
            )
            .put(
                v::H_WORST_SEVERITY,
                Cell::Severity(g.worst_severity.to_ascii_lowercase()),
            )
            .put(v::H_WHERE, Cell::wrap(site_list_cell(&g.locations)))
            .put(v::H_WHAT_TO_DO, Cell::wrap(&g.recommended_fix))
            .put(v::H_FIX_TYPE, Cell::text(&g.fix_type))
            .put(v::H_RULE, Cell::wrap(&g.rule_title))
            .put(v::H_REPO, Cell::text(&g.repo))
            .put(v::H_RULE_ID, Cell::text(&g.rule_id));
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note =
        Some("No departures from your own conventions were identified in this inspection.".into());
    table
}

// ── Dependencies ────────────────────────────────────────────────────────────────────────

/// The dependency rows in the order the sheet prints them.
pub(super) fn sorted_deps(deps: &[DepRow]) -> Vec<DepRow> {
    let mut sorted = deps.to_vec();
    sorted.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| a.package.cmp(&b.package))
    });
    sorted
}

fn dep_action(row: &DepRow) -> String {
    if row.action.is_empty() {
        row.fixed_version
            .as_deref()
            .map(|v| format!("Upgrade to >= {v}."))
            .unwrap_or_default()
    } else {
        row.action.clone()
    }
}

pub(super) fn dependencies_table(sorted: &[DepRow]) -> Table {
    let mut table = Table::new(
        v::DEPENDENCIES,
        vec![
            Col::plain(v::H_PACKAGE, 30.0),
            Col::plain(v::H_ADVISORY, 60.0),
            Col::plain(v::H_SEVERITY, 12.0),
            Col::plain(v::H_REPO, 18.0),
            Col::plain(v::H_FIXED_IN, 14.0),
            Col::plain(v::H_WHAT_TO_DO, 48.0),
            Col::plain(v::H_FILE, 30.0),
            Col::plain(v::H_REVIEWED_IN_PERSON, 12.0),
            Col::technical(v::H_PLACE_ID, 18.0),
            Col::technical(v::H_FIX_ID, 30.0),
            Col::technical(v::H_REVIEW_LEVEL, 30.0),
            Col::technical(v::H_REVIEWER_EDITED, 10.0),
            Col::technical(v::H_EDIT_IDS, 22.0),
        ],
    );
    for row in sorted {
        let mut c = RowCells::default();
        c.put(v::H_PACKAGE, Cell::text(&row.package))
            .put(v::H_ADVISORY, Cell::wrap(&row.advisory))
            .put(v::H_SEVERITY, Cell::Severity(row.severity.clone()))
            .put(v::H_REPO, Cell::text(&row.repo))
            .put(
                v::H_FIXED_IN,
                Cell::text(row.fixed_version.as_deref().unwrap_or("(no patch yet)")),
            )
            .put(v::H_WHAT_TO_DO, Cell::wrap(dep_action(row)))
            .put(v::H_FILE, Cell::text(&row.path))
            .put(
                v::H_REVIEWED_IN_PERSON,
                yes(row.facts.review_level == "personally_reviewed"),
            )
            .put(v::H_PLACE_ID, Cell::text(&row.facts.site_id))
            .put(v::H_FIX_ID, Cell::text(&row.facts.group))
            .put(v::H_REVIEW_LEVEL, Cell::text(&row.facts.review_level))
            .put(v::H_REVIEWER_EDITED, yes(row.reviewer_edited))
            .put(v::H_EDIT_IDS, Cell::text(row.edit_ids.join(", ")));
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note = Some("No known vulnerable dependencies were found.".into());
    table
}

// ── What we checked ─────────────────────────────────────────────────────────────────────

pub(super) fn rules_checked_table(
    coverage: &[CoverageRow],
    applied: &[RulesAppliedRow],
    lay: &Layout,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Table {
    let mut table = Table::new(
        v::RULES_CHECKED,
        vec![
            Col::plain(v::H_AREA, 18.0),
            Col::plain(v::H_RULE, 60.0),
            Col::plain(v::H_RESULT, 24.0),
            Col::plain(v::H_HOW_WE_CHECKED, 18.0),
            Col::plain(v::H_STANDARD_CITED, 40.0),
            Col::plain(v::H_ALTERNATIVE_APPLIED, 40.0),
            Col::plain(v::H_EVIDENCE, 44.0),
            Col::plain(v::H_GO_TO_FIX, 30.0),
            Col::technical(v::H_RULE_ID, 28.0),
        ],
    );
    table.frozen_cols = 2;
    let mut picks: BTreeMap<&str, Vec<&RulesAppliedRow>> = BTreeMap::new();
    for a in applied {
        picks.entry(a.rule_id.as_str()).or_default().push(a);
    }
    for row in coverage {
        // A result in words, not a bare integer: "0" does not say whether the rule was clean or
        // never ran, and the never-ran rules are named on Not checked.
        let mut result = if row.findings_count == 0 {
            "Clean".to_string()
        } else {
            format!(
                "Found {} {}",
                row.findings_count,
                if row.findings_count == 1 {
                    "place"
                } else {
                    "places"
                }
            )
        };
        if !row.by_repo.is_empty() {
            let everywhere = row.by_repo.iter().all(|c| c.result == "Clean");
            if row.findings_count == 0 && !everywhere {
                let unreviewed = row
                    .by_repo
                    .iter()
                    .any(|c| c.result.starts_with("Not reviewed"));
                result = if unreviewed {
                    "Not reviewed in every service".to_string()
                } else {
                    "Clean where it applies".to_string()
                };
            }
            for cell in &row.by_repo {
                result.push_str(&format!("\n{}: {}", cell.repo, cell.result));
            }
        }
        let applied_here = picks.get(row.rule_id.as_str());
        let multi_pick = applied_here.is_some_and(|p| p.iter().any(|a| !a.repo.is_empty()));
        let chosen = applied_here
            .map(|p| {
                p.iter()
                    .map(|a| {
                        if multi_pick {
                            format!("{}: {}", a.repo, a.chosen)
                        } else {
                            a.chosen.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let evidence = applied_here
            .map(|p| {
                p.iter()
                    .filter(|a| !a.evidence.is_empty())
                    .map(|a| {
                        if multi_pick {
                            format!("{}: {}", a.repo, a.evidence)
                        } else {
                            a.evidence.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let go_to_fix = if let Some(g) = lay.plan.iter().find(|g| g.rule_id == row.rule_id) {
            let at = lay.fix_at.get(&g.id).map_or(0, |(r, _)| *r);
            Cell::link(v::FIX_PLAN, at, &g.label)
        } else if let Some(at) = lay.first_problem_of_rule.get(&row.rule_id) {
            Cell::link(v::ALL_PROBLEMS, *at, "See the problems")
        } else {
            Cell::Empty
        };
        let mut c = RowCells::default();
        c.put(v::H_AREA, Cell::text(&row.category))
            .put(v::H_RULE, Cell::wrap(&row.title))
            .put(v::H_RESULT, Cell::wrap(result))
            .put(
                v::H_HOW_WE_CHECKED,
                Cell::text(how_checked_label(&row.citation_kind)),
            )
            .put(v::H_STANDARD_CITED, Cell::wrap(&row.citation_label))
            .put(v::H_ALTERNATIVE_APPLIED, Cell::wrap(chosen))
            .put(v::H_EVIDENCE, Cell::wrap(evidence))
            .put(v::H_GO_TO_FIX, go_to_fix)
            .put(v::H_RULE_ID, Cell::text(&row.rule_id));
        table.rows.push(c.assemble(&table.cols));
    }
    // A rule an alternative was chosen for but that is not in the checked list still gets its
    // row, so the choice and its evidence are never lost.
    let listed: std::collections::HashSet<&str> =
        coverage.iter().map(|r| r.rule_id.as_str()).collect();
    for (rule_id, picks) in &picks {
        if listed.contains(rule_id) {
            continue;
        }
        let multi_pick = picks.iter().any(|a| !a.repo.is_empty());
        let join = |f: &dyn Fn(&RulesAppliedRow) -> &str| {
            picks
                .iter()
                .filter(|a| !f(a).is_empty())
                .map(|a| {
                    if multi_pick {
                        format!("{}: {}", a.repo, f(a))
                    } else {
                        f(a).to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut c = RowCells::default();
        c.put(
            v::H_AREA,
            Cell::text(crate::report_export::category_for(rule_id, corpus)),
        )
        .put(v::H_RULE, Cell::wrap(rule_title(rule_id, corpus)))
        .put(v::H_RESULT, Cell::text("Alternative chosen"))
        .put(v::H_ALTERNATIVE_APPLIED, Cell::wrap(join(&|a| &a.chosen)))
        .put(v::H_EVIDENCE, Cell::wrap(join(&|a| &a.evidence)))
        .put(v::H_RULE_ID, Cell::text(*rule_id));
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note = Some("No rules were recorded as checked for this inspection.".into());
    table
}

/// The Differences across services sheet: one row for each rule the services answer differently,
/// one column for each service.
pub(super) fn differences_table(
    patterns: &crate::report_export::PatternsAcrossServicesJson,
) -> Table {
    let mut cols = vec![Col::plain(v::H_WHAT_WE_CHECKED, 56.0)];
    for repo in &patterns.repos {
        cols.push(Col::plain(repo, 34.0));
    }
    cols.push(Col::technical(v::H_RULE_ID, 28.0));
    let mut table = Table::new(v::DIFFERENCES_ACROSS_SERVICES, cols);
    table.header_note = Some(patterns.note.clone());
    for row in &patterns.rows {
        let mut cells = vec![Cell::wrap(&row.statement)];
        for repo in &patterns.repos {
            let label = row
                .cells
                .iter()
                .find(|cell| &cell.repo == repo)
                .map(|cell| super::sheet_cell_label(&cell.state))
                .unwrap_or_default();
            cells.push(Cell::wrap(label));
        }
        cells.push(Cell::text(&row.rule_id));
        table.rows.push(cells);
    }
    table.empty_note =
        Some("No rule was found violated in one service and clean in another.".into());
    table
}

/// The rows the engine threw out and kept, for an internal export.
pub(super) fn engine_discards(
    discarded: &[crate::onboard::Finding],
) -> Vec<(String, String, usize, String, String, String)> {
    discarded
        .iter()
        .map(|f| {
            (
                crate::report_export::defect_headline(&f.detail, &f.rule_id),
                f.path.clone(),
                f.line,
                f.rule_id.clone(),
                crate::engine_discard::reason_of(f)
                    .unwrap_or("")
                    .to_string(),
                f.repo.clone(),
            )
        })
        .collect()
}

pub(super) fn ruled_out_table(
    ruled_out: &[&FindingRow],
    engine: &[(String, String, usize, String, String, String)],
) -> Table {
    let mut table = Table::new(
        v::RULED_OUT,
        vec![
            Col::plain(v::H_PROBLEM, 60.0),
            Col::plain(v::H_FILE, 40.0),
            Col::plain(v::H_LINE, 10.0),
            Col::plain(v::H_RULE, 40.0),
            Col::plain(v::H_RULED_OUT_BY, 16.0),
            Col::plain(v::H_REASON, 60.0),
            Col::technical(v::H_PLACE_ID, 18.0),
            Col::technical(v::H_RULE_ID, 28.0),
        ],
    );
    table.frozen_cols = 1;
    for row in ruled_out {
        let mut c = RowCells::default();
        c.put(v::H_PROBLEM, Cell::wrap(&row.headline))
            .put(v::H_FILE, Cell::text(&row.path))
            .put(v::H_LINE, line_cell(row.line))
            .put(v::H_RULE, Cell::wrap(&row.rule_title))
            .put(v::H_RULED_OUT_BY, Cell::text(v::REVIEWER))
            .put(v::H_REASON, Cell::wrap(&row.fp_reason))
            .put(v::H_PLACE_ID, Cell::text(&row.facts.site_id))
            .put(v::H_RULE_ID, Cell::text(&row.rule_id));
        table.rows.push(c.assemble(&table.cols));
    }
    for (headline, path, line, rule_id, reason, _repo) in engine {
        let mut c = RowCells::default();
        c.put(v::H_PROBLEM, Cell::wrap(headline))
            .put(v::H_FILE, Cell::text(path))
            .put(v::H_LINE, line_cell(*line))
            .put(v::H_RULE, Cell::wrap(rule_id))
            .put(v::H_RULED_OUT_BY, Cell::text(v::ENGINE))
            .put(v::H_REASON, Cell::wrap(reason))
            .put(v::H_RULE_ID, Cell::text(rule_id));
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note = Some("Nothing was ruled out in this inspection.".into());
    table
}

/// What the Not checked sheet lists, collected from every place the inspection records a gap.
pub(super) struct NotChecked<'a> {
    pub not_assessed: &'a [NotAssessedRow],
    pub rules_not_run: &'a [(String, String)],
    pub tool_notes: &'a [String],
    pub dependency_notes: &'a [String],
    pub excluded_mechanical: &'a [String],
    pub gaps: &'a crate::report_export::CoverageGapsJson,
}

pub(super) fn not_checked_table(n: &NotChecked, corpus: Option<&camerata_rules::RuleSet>) -> Table {
    let mut table = Table::new(
        v::NOT_CHECKED,
        vec![
            Col::plain(v::H_GAP, 30.0),
            Col::plain(v::H_WHAT, 60.0),
            Col::plain(v::H_WHY, 60.0),
            Col::plain(v::H_REPO, 24.0),
            Col::plain(v::H_RULE, 40.0),
            Col::plain(v::H_MORE_DETAIL, 60.0),
            Col::technical(v::H_RULE_ID, 28.0),
        ],
    );
    let mut push =
        |gap: &str, what: &str, why: &str, repo: &str, rule: &str, detail: &str, rule_id: &str| {
            let mut c = RowCells::default();
            c.put(v::H_GAP, Cell::text(gap))
                .put(v::H_WHAT, Cell::wrap(what))
                .put(v::H_WHY, Cell::wrap(why))
                .put(v::H_REPO, Cell::text(repo))
                .put(v::H_RULE, Cell::wrap(rule))
                .put(v::H_MORE_DETAIL, Cell::wrap(detail))
                .put(v::H_RULE_ID, Cell::text(rule_id));
            table.rows.push(c.assemble(&table.cols));
        };
    for r in n.not_assessed {
        // The row's own statement is the "what"; the rule it belongs to has its own column.
        push(
            v::GAP_RULE_COULD_NOT_COMPLETE,
            &r.headline,
            &r.why,
            &r.repo,
            &r.rule_title,
            &r.detail,
            &r.rule_id,
        );
    }
    for (rid, reason) in n.rules_not_run {
        push(
            v::GAP_RULE_NOT_RUN,
            &rule_title(rid, corpus),
            reason,
            "",
            "",
            "",
            rid,
        );
    }
    for rid in n.excluded_mechanical {
        push(
            v::GAP_LEFT_TO_CI,
            &rule_title(rid, corpus),
            "Mechanical, enforced by your CI instead of this code-only inspection.",
            "",
            "",
            "",
            rid,
        );
    }
    for note in n.tool_notes {
        push(v::GAP_TOOL_NOTE, note, "", "", "", "", "");
    }
    for note in n.dependency_notes {
        push(v::GAP_DEPENDENCY, note, "", "", "", "", "");
    }
    let g = n.gaps;
    for line in g
        .failed_ai_passes
        .iter()
        .chain(&g.dependency_gaps)
        .chain(&g.file_limit_notes)
        .chain(g.skipped_summary.iter())
        .chain(g.evidence_note.iter())
    {
        push(v::GAP_REVIEW_PASS, line, "", "", "", "", "");
    }
    if let Some(note) = &g.scope_limited_note {
        push(v::GAP_RULE_PART_OF_FILES, note, "", "", "", "", "");
        for rule in &g.scope_limited_rules {
            push(
                v::GAP_RULE_PART_OF_FILES,
                &rule_title(&rule.rule_id, corpus),
                &format!("Scope: {}", rule.scope),
                "",
                "",
                &format!(
                    "Checked {} of {} files ({}).",
                    rule.files_checked,
                    rule.files_total,
                    if rule.changed_files_only {
                        "files changed since the earlier inspection"
                    } else {
                        "all files"
                    }
                ),
                &rule.rule_id,
            );
        }
    }
    for skipped in &g.skipped_files {
        push(
            v::GAP_FILE_NOT_READ,
            &skipped.path,
            &skipped.reason,
            &skipped.repo,
            "",
            &skipped.size,
            "",
        );
    }
    if let Some(note) = &g.skipped_beyond_cap_note {
        push(v::GAP_FILE_NOT_READ, note, "", "", "", "", "");
    }
    table.empty_note = Some("Everything in scope was checked.".into());
    table
}

// ── All source rows ─────────────────────────────────────────────────────────────────────

/// The banner of the All source rows sheet: its title and the first line.
pub(super) fn source_banner() -> (String, String) {
    (
        format!("{} (complete record)", v::ALL_SOURCE_ROWS),
        "Raw record. Every row the engine produced, including the collapsed source rows behind a \
         merged problem. A problem can be listed several times here. Every other sheet shows it \
         once."
            .to_string(),
    )
}

pub(super) fn source_rows_tables(
    sources: &[SiteRow],
    lay: &Layout,
    corpus: Option<&camerata_rules::RuleSet>,
    chunk: usize,
    names: &[String],
) -> Vec<Table> {
    let mut cols = vec![
        Col::plain(v::H_ROW_ROLE, 18.0),
        Col::plain(v::H_FOUND_HOW_MANY_WAYS, 12.0),
        Col::plain(v::H_REPRESENTED_BY, 24.0),
        Col::plain(v::H_SHOWN_ON, 22.0),
    ];
    cols.extend(problem_cols(lay, false));
    let mut ways: HashMap<&str, usize> = HashMap::new();
    for s in sources {
        *ways.entry(s.facts.site_id.as_str()).or_default() += 1;
    }
    let pages: Vec<&[SiteRow]> = if sources.is_empty() {
        vec![&[]]
    } else {
        sources.chunks(chunk).collect()
    };
    let mut out = Vec::new();
    for (page, name) in pages.iter().zip(names) {
        let mut table = Table::new(name, cols.clone());
        table.banner = Some(source_banner());
        table.frozen_cols = 1;
        for s in *page {
            let site = s;
            // A row behind a problem carries only what is its own: its place, rule and code.
            // The problem's fix, cited standard and long text are on its Primary row, and
            // repeating them for each of thousands of absorbed places would bury the sheet.
            let mut c = match (s.parent, site.site_type == v::ROLE_PRIMARY) {
                (SiteParent::Problem(row), true) => problem_cells(row, lay),
                (SiteParent::Dependency(dep), _) => dep_source_cells(dep),
                (SiteParent::Problem(_), false) => {
                    let mut lean = RowCells::default();
                    lean.put(v::H_PLACE_ID, Cell::text(&site.facts.site_id))
                        .put(v::H_FIX_ID, Cell::text(&site.facts.group))
                        .put(v::H_REVIEW_LEVEL, Cell::text(&site.facts.review_level));
                    lean
                }
            };
            let represented = match lay.placement.get(&site.facts.site_id) {
                Some((sheet, row)) => Cell::link(sheet, *row, sheet),
                None => Cell::Empty,
            };
            c.put(v::H_ROW_ROLE, Cell::text(site.site_type))
                .put(
                    v::H_FOUND_HOW_MANY_WAYS,
                    Cell::Num(ways.get(site.facts.site_id.as_str()).copied().unwrap_or(1) as f64),
                )
                .put(v::H_REPRESENTED_BY, represented)
                .put(v::H_SHOWN_ON, Cell::text(site.listed_on))
                .put(
                    v::H_SEVERITY,
                    Cell::Severity(site.severity.to_ascii_lowercase()),
                )
                .put(v::H_PROBLEM, Cell::wrap(&site.headline))
                .put(v::H_FILE, Cell::text(&site.path))
                .put(v::H_LINE, line_cell(site.line))
                .put(v::H_REPO, Cell::text(&site.repo))
                .put(v::H_CODE_SEEN, Cell::Mono(site.snippet.clone()))
                .put(v::H_RULES, Cell::wrap(rule_title(&site.rule_id, corpus)))
                .put(v::H_RULE_IDS, Cell::text(&site.rule_id));
            table.rows.push(c.assemble(&table.cols));
        }
        table.empty_note = Some("No source rows were produced.".into());
        out.push(table);
    }
    out
}

fn dep_source_cells(dep: &DepRow) -> RowCells {
    let mut c = RowCells::default();
    c.put(v::H_AREA, Cell::text("Dependencies"))
        .put(v::H_SOURCE, Cell::text(v::SOURCE_DETERMINISTIC))
        .put(v::H_WHAT_TO_DO_HERE, Cell::wrap(dep_action(dep)))
        .put(
            v::H_REVIEWED_IN_PERSON,
            yes(dep.facts.review_level == "personally_reviewed"),
        )
        .put(v::H_PLACE_ID, Cell::text(&dep.facts.site_id))
        .put(v::H_FIX_ID, Cell::text(&dep.facts.group))
        .put(v::H_REVIEW_LEVEL, Cell::text(&dep.facts.review_level))
        .put(v::H_REVIEWER_EDITED, yes(dep.reviewer_edited))
        .put(v::H_EDIT_IDS, Cell::text(dep.edit_ids.join(", ")))
        .put(v::H_KIND, Cell::text("dependency_advisory"));
    c
}

// ── Glossary ────────────────────────────────────────────────────────────────────────────

pub(super) fn glossary_table(internal: bool) -> Table {
    let mut table = Table::new(
        v::GLOSSARY,
        vec![
            Col::plain(v::H_TERM, 30.0),
            Col::plain(v::H_PLAIN_MEANING, 90.0),
            Col::plain(v::H_WHERE_YOU_MEET_IT, 50.0),
        ],
    );
    let internal_entries: &[v::GlossaryEntry] = if internal {
        v::INTERNAL_GLOSSARY_ENTRIES
    } else {
        &[]
    };
    for e in v::GLOSSARY_ENTRIES.iter().chain(internal_entries) {
        table.rows.push(vec![
            Cell::wrap(e.term),
            Cell::wrap(e.meaning),
            Cell::wrap(e.where_you_meet_it),
        ]);
    }
    table
}

// ── Start here ──────────────────────────────────────────────────────────────────────────

/// The facts the Start here sheet states about the inspection, in the words of the PDF cover.
pub(super) struct StartFacts<'a> {
    pub report: &'a crate::onboard::ScanReport,
    pub opts: &'a crate::report_export::ReportOptions,
    /// Every non-ruled-out row, for the scorecard.
    pub live_rows: &'a [&'a FindingRow],
    pub corpus: Option<&'a camerata_rules::RuleSet>,
    /// How many rows each sheet holds, in reading order.
    pub counts: Vec<(String, usize)>,
    /// The one line that says what the rules checked came to.
    pub coverage_summary: String,
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The cover's code volume line: lines of code, by language, and by repo for several repos.
pub(super) fn code_volume_label(report: &crate::onboard::ScanReport) -> Option<String> {
    if report.code_lines == 0 {
        return None;
    }
    let mut base = format!(
        "{} {} of code",
        thousands(report.code_lines),
        if report.code_lines == 1 {
            "line"
        } else {
            "lines"
        }
    );
    if !report.code_lines_by_language.is_empty() {
        base.push_str(&format!(
            " ({})",
            report
                .code_lines_by_language
                .iter()
                .map(|l| format!("{}: {}", l.language, thousands(l.lines)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if report.repos.len() > 1 && !report.volume_by_repo.is_empty() {
        base.push_str(&format!(
            ". By repo: {}",
            report
                .volume_by_repo
                .iter()
                .map(|r| format!("{}: {}", r.repo, thousands(r.lines)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(base)
}

/// The scorecard: one row for each area, its problems by severity. The PDF scorecard counts the
/// same rows the same way.
pub(super) fn scorecard(
    live_rows: &[&FindingRow],
    report: &crate::onboard::ScanReport,
    corpus: Option<&camerata_rules::RuleSet>,
) -> BTreeMap<String, [usize; 4]> {
    let mut by_area: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    for row in live_rows {
        let counts = by_area.entry(row.category.clone()).or_insert([0; 4]);
        match row.severity.as_str() {
            "critical" => counts[0] += 1,
            "high" => counts[1] += 1,
            "medium" => counts[2] += 1,
            _ => counts[3] += 1,
        }
    }
    // An area with rules checked and nothing found still gets its row of zeros, as in the PDF.
    for rid in &report.provenance.audited_rule_ids {
        by_area
            .entry(crate::report_export::category_for(rid, corpus))
            .or_insert([0; 4]);
    }
    by_area
}

pub(super) fn write_start_here(
    wb: &mut Workbook,
    fmts: &Formats,
    facts: &StartFacts,
) -> Result<(), XlsxError> {
    let mut ws = CappedSheet::new(wb.add_worksheet());
    ws.set_name(v::START_HERE)?;
    let _ = ws.set_tab_color(Color::from(v::tab_color_of(v::START_HERE)));
    ws.set_column_width(0, 34.0)?;
    ws.set_column_width(1, 90.0)?;
    for c in 2..6u16 {
        ws.set_column_width(c, 12.0)?;
    }
    let link_fmt = rust_xlsxwriter::Format::new()
        .set_font_color("#0563C1")
        .set_underline(rust_xlsxwriter::FormatUnderline::Single);
    let wrap = fmts.cell(None, true, false);

    let mut r = 0u32;
    ws.write_text_with_format(r, 0, v::WORKBOOK_TITLE, &fmts.title)?;
    r += 2;
    let opts = facts.opts;
    if !opts.project_title.is_empty() {
        ws.write_text(r, 0, &opts.project_title)?;
        r += 1;
    }
    let client = crate::report_export::clean_client_name(&opts.client_name);
    if !client.is_empty() {
        ws.write_text(r, 0, format!("{} {client}", v::P_PREPARED_FOR))?;
        r += 1;
    }
    if !opts.prepared_by.is_empty() {
        ws.write_text(r, 0, format!("{} {}", v::P_PREPARED_BY, opts.prepared_by))?;
        r += 1;
    }
    r += 1;
    ws.write_text_with_format(
        r,
        0,
        "This file holds the detail behind your report. Begin with the Fix plan: one row for \
         each fix, in the order to do them. Click a sheet name below to open it; every sheet has \
         a link back to this page.",
        &wrap,
    )?;
    ws.set_row_height(r, 48)?;
    r += 2;

    // ── The sheets, each linked, with what it is and how many rows it holds ──
    ws.write_text_with_format(r, 0, "Where everything is", &fmts.section_title)?;
    r += 1;
    for (c, h) in [v::H_SHEET, v::H_WHAT_IT_IS, v::H_ROWS].iter().enumerate() {
        ws.write_text_with_format(r, c as u16, *h, &fmts.header)?;
    }
    r += 1;
    let mut last_group: Option<v::SheetGroup> = None;
    for info in &v::SHEETS {
        let Some((name, count)) = facts.counts.iter().find(|(n, _)| n == info.name) else {
            continue; // a sheet this workbook does not hold
        };
        if last_group != Some(info.group) {
            ws.write_text_with_format(r, 0, info.group.heading(), &fmts.bold)?;
            r += 1;
            last_group = Some(info.group);
        }
        if name == v::START_HERE {
            ws.write_text(r, 0, name)?;
        } else {
            ws.write_link_with_format(r, 0, &super::tables::link_target(name, 0), name, &link_fmt)?;
        }
        ws.write_text_with_format(r, 1, info.purpose, &wrap)?;
        if name != v::START_HERE {
            ws.write_number(r, 2, *count as f64)?;
        }
        r += 1;
    }
    r += 1;

    // ── The scorecard ──
    ws.write_text_with_format(r, 0, "Scorecard", &fmts.section_title)?;
    r += 1;
    for (c, h) in [v::H_AREA, "Critical", "High", "Medium", "Low", "Total"]
        .iter()
        .enumerate()
    {
        ws.write_text_with_format(r, c as u16, *h, &fmts.header)?;
    }
    r += 1;
    let by_area = scorecard(facts.live_rows, facts.report, facts.corpus);
    let mut totals = [0usize; 4];
    for (area, counts) in &by_area {
        ws.write_text(r, 0, area)?;
        for (i, count) in counts.iter().enumerate() {
            ws.write_number(r, (i + 1) as u16, *count as f64)?;
            totals[i] += *count;
        }
        ws.write_number(r, 5, counts.iter().sum::<usize>() as f64)?;
        r += 1;
    }
    ws.write_text_with_format(r, 0, "Total", &fmts.bold)?;
    for (i, total) in totals.iter().enumerate() {
        ws.write_number_with_format(r, (i + 1) as u16, *total as f64, &fmts.bold)?;
    }
    ws.write_number_with_format(r, 5, totals.iter().sum::<usize>() as f64, &fmts.bold)?;
    r += 2;

    // ── About the inspection, in the words of the PDF cover ──
    let report = facts.report;
    ws.write_text_with_format(r, 0, "About this inspection", &fmts.section_title)?;
    r += 1;
    ws.write_text(r, 0, v::P_REPOS_INSPECTED)?;
    ws.write_text(r, 1, report.repos.join(", "))?;
    r += 1;
    for aref in &report.provenance.audited_refs {
        let sha = aref
            .sha
            .as_deref()
            .map(|s| s.chars().take(7).collect::<String>())
            .unwrap_or_else(|| "no commit recorded".to_string());
        let branch = aref.branch.as_deref().unwrap_or("unknown branch");
        let dirty = if aref.dirty { " (dirty)" } else { "" };
        ws.write_text(r, 0, &aref.repo)?;
        ws.write_text(r, 1, format!("{sha} on {branch}{dirty}"))?;
        r += 1;
    }
    ws.write_text(r, 0, v::P_FILES_SCANNED)?;
    ws.write_text(
        r,
        1,
        format!(
            "{} ({} excluded as noise)",
            thousands(report.files_scanned),
            thousands(report.files_excluded)
        ),
    )?;
    r += 1;
    if let Some(volume) = code_volume_label(report) {
        ws.write_text(r, 0, v::P_CODE_VOLUME)?;
        ws.write_text(r, 1, volume)?;
        r += 1;
    }
    ws.write_text(r, 0, v::P_GENERATED)?;
    // The scan time, the same string the PDF cover prints.
    ws.write_text(
        r,
        1,
        crate::report_export::generated_at_label(&report.provenance),
    )?;
    r += 1;
    ws.write_text(r, 0, v::P_WHAT_WE_CHECKED)?;
    ws.write_text(r, 1, &facts.coverage_summary)?;
    Ok(())
}
