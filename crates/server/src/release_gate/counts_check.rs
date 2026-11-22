//! The gate's check of every count in the package (single counts, work order B; design section
//! 5.7). [`counts_problems`](super::consistency::counts_problems) calls it.
//!
//! The recount works from the archive's JSON files with its own small tally. It does NOT call
//! `report_counts` or `places_of`: a check that calls the code it checks proves nothing. The only
//! thing it shares with the object is the data type [`ReportCounts`], used as a plain container.
//!
//! Checks (numbers are the design's):
//!
//! 1. The recount equals the object in `json/inspection.json`, field by field.
//! 2. The identities hold, on the recount and on the object (restated here, not called).
//! 3. Every surface equals the object: the PDF data's `counts`, the cover, the grid, the
//!    methodology fields the template prints as "problems" and "need a decision", the Start here
//!    row counts, the rows of each sheet and `tickets.csv`.
//! 4. Every count sentence in the PDF data equals the sentence rebuilt from the recount.
//! 5. No stray number: every integer in an executive summary override is a value of the object.
//! 6. (work order D, flag off, not built: it needs the status words D introduces.)
//! 7. (work order D, flag off) Every folded row names its head in `carried_by`.

use camerata_api_types::package_paths::{
    ALL_FINDINGS_JSON, DEPENDENCIES_JSON, FALSE_POSITIVES_JSON, INSPECTION_JSON, TICKETS_CSV,
};
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::report::Problem;
use crate::report_export::counts_prose;
use crate::report_export::{AuditReportJson, ReportCounts};
use crate::xlsx_export::vocabulary as v;

type Entries = BTreeMap<String, Vec<u8>>;

const COUNTS: &str = "counts";

/// Which of the design's checks run. Checks 1 to 5 run today; 7 belongs to work order D and is
/// off until D switches it on.
#[derive(Debug, Clone, Copy, Default)]
pub struct CountsChecks {
    /// Check 7 (work order D): every folded row names its head in `carried_by`.
    pub folded_rows_name_their_head: bool,
}

fn problem(file: &str, location: &str, message: impl AsRef<str>) -> Problem {
    Problem::new(file, location, message, None)
}

fn text(row: &Value, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// The gate's own place key for a JSON row: (repo, path, line), and for a whole-file problem
/// (line 0) the rule id as well, so whole-file problems of different rules are separate places.
pub(super) fn place_key_of(row: &Value) -> (String, String, String, String) {
    let line = text(row, "line");
    let rule = if line == "0" {
        text(row, "rule_id")
    } else {
        String::new()
    };
    (text(row, "repo"), text(row, "path"), line, rule)
}

fn cannot_recount(file: &str, why: impl AsRef<str>) -> Problem {
    problem(
        file,
        COUNTS,
        format!(
            "the counts cannot be recounted from the package: {}",
            why.as_ref()
        ),
    )
}

/// A dataset of the archive that must be a JSON array of rows. Missing or any other shape is a
/// problem (fail closed): the recount works from these files, so a package that cannot be
/// recounted is not a package that passes.
fn array_of(entries: &Entries, name: &str) -> Result<Vec<Value>, Problem> {
    let Some(bytes) = entries.get(name) else {
        return Err(cannot_recount(name, "the file is missing"));
    };
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Array(rows)) => Ok(rows),
        Ok(_) => Err(cannot_recount(name, "the file is not a JSON array of rows")),
        Err(e) => Err(cannot_recount(
            name,
            format!("the file does not parse ({e})"),
        )),
    }
}

fn severity_slot(s: &str) -> usize {
    match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// The recount, from the archive's own files, with a tally written for this check alone. A file
/// it needs that is missing or of the wrong shape is a problem, never a pass (fail closed).
pub(super) fn recount(
    entries: &Entries,
    report: &AuditReportJson,
) -> Result<ReportCounts, Vec<Problem>> {
    let mut failures = Vec::new();
    let mut take = |name: &str| match array_of(entries, name) {
        Ok(rows) => rows,
        Err(p) => {
            failures.push(p);
            Vec::new()
        }
    };
    let live = take(ALL_FINDINGS_JSON);
    let ruled_out = take(FALSE_POSITIVES_JSON);
    let deps = take(DEPENDENCIES_JSON);
    let findings_json: Value = match entries
        .get(INSPECTION_JSON)
        .map(|b| serde_json::from_slice(b))
    {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            failures.push(cannot_recount(
                INSPECTION_JSON,
                format!("the file does not parse ({e})"),
            ));
            Value::Null
        }
        None => {
            failures.push(cannot_recount(INSPECTION_JSON, "the file is missing"));
            Value::Null
        }
    };
    let fixes = findings_json.get("fixes").and_then(Value::as_array);
    if fixes.is_none() && !findings_json.is_null() {
        failures.push(cannot_recount(
            INSPECTION_JSON,
            "it has no fixes array to count the fixes from",
        ));
    }
    if !failures.is_empty() {
        return Err(failures);
    }
    let fixes = fixes.expect("checked above");

    let is_problem = |r: &&Value| {
        crate::xlsx_export::ALL_PROBLEMS_SHEET_KINDS.contains(&text(r, "kind").as_str())
    };
    let problem_rows: Vec<&Value> = live.iter().filter(is_problem).collect();

    let mut c = ReportCounts::default();
    c.rows.dependency = deps.len();
    c.rows.ruled_out = ruled_out.len();
    c.rows.problem = problem_rows.len();
    c.rows.convention = live
        .iter()
        .filter(|r| {
            matches!(
                text(r, "kind").as_str(),
                "convention_observed" | "no_convention"
            )
        })
        .count();
    c.rows.not_checked = live
        .iter()
        .filter(|r| text(r, "kind") == "not_assessed")
        .count();
    c.rows.tool_preview = live
        .iter()
        .filter(|r| text(r, "provenance").starts_with("Preview:"))
        .count();
    c.rows.total = live.len() + ruled_out.len() + deps.len();

    let mut at: BTreeMap<(String, String, String, String), Vec<&Value>> = BTreeMap::new();
    for r in &problem_rows {
        at.entry(place_key_of(r)).or_default().push(r);
    }
    c.problems.total = at.len();
    let mut waiting_places: BTreeSet<(String, String, String, String)> = BTreeSet::new();
    for (place, members) in &at {
        c.rows.folded += members.len() - 1;
        let worst = members
            .iter()
            .map(|m| severity_slot(&text(m, "severity")))
            .min()
            .unwrap_or(3);
        let sev = ["critical", "high", "medium", "low"][worst.min(3)];
        c.problems.by_severity.add(sev);
        let buckets: Vec<String> = members.iter().map(|m| text(m, "bucket")).collect();
        let known = ["do_now", "do_next", "plan", "informational", "accepted"];
        // Fail closed: a held row, or a bucket nobody knows, needs a decision.
        if buckets.iter().any(|b| !known.contains(&b.as_str())) {
            waiting_places.insert(place.clone());
            c.problems.needs_decision += 1;
            c.problems.needs_decision_by_severity.add(sev);
        } else if buckets.iter().any(|b| b == "do_now") {
            c.problems.for_action += 1;
            c.problems.first += 1;
        } else if buckets.iter().any(|b| b == "do_next") {
            c.problems.for_action += 1;
            c.problems.next += 1;
        } else if buckets.iter().any(|b| b == "plan") {
            c.problems.for_action += 1;
            c.problems.later += 1;
        } else if buckets.iter().any(|b| b == "informational") {
            c.problems.information_only += 1;
            c.problems.information_only_by_severity.add(sev);
        } else {
            c.problems.accepted += 1;
        }
        if members
            .iter()
            .any(|m| m.get("escalated").and_then(Value::as_bool) == Some(true))
        {
            c.problems.escalated += 1;
        }
        // A place a person read: any row there carries the review level `personally_reviewed`,
        // which is what the workbook's "Reviewed in person" cell says.
        if members
            .iter()
            .any(|m| text(m, "review_level") == camerata_api_types::review::PERSONALLY_REVIEWED)
        {
            c.review.reviewed_in_person += 1;
        }
    }

    // Fail closed: each open place of another rule that a ruled-out row carries, and that no
    // problem row already holds, is a problem that needs a decision, at the severity of
    // the ruled-out row that carries it.
    let mut beside: BTreeSet<(String, String, String, String)> = BTreeSet::new();
    for site in findings_json
        .get("open_beside_ruled_out")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = place_key_of(site);
        if !at.contains_key(&key) && beside.insert(key) {
            c.problems.total += 1;
            c.problems.needs_decision += 1;
            c.problems.by_severity.add(&text(site, "severity"));
            c.problems
                .needs_decision_by_severity
                .add(&text(site, "severity"));
            c.problems.beside_ruled_out += 1;
        }
    }

    c.fixes.total = fixes.len();
    // A Fix is to do while a place of it is still to change: a problem row a reviewer, a baseline
    // record or a waiver has not accepted, at a place that does not wait for a decision, or any
    // dependency advisory. A Fix whose only places wait for a decision is not yet a change to make.
    let to_do: BTreeSet<String> = problem_rows
        .iter()
        .copied()
        .filter(|r| {
            !matches!(
                text(r, "disposition_kind").as_str(),
                "Ignored" | "BaselineAccepted" | "WaivedInline"
            ) && !waiting_places.contains(&place_key_of(r))
        })
        .chain(deps.iter())
        .map(|r| text(r, "fix_id"))
        .filter(|id| !id.is_empty())
        .collect();
    c.fixes.to_do = fixes
        .iter()
        .filter(|f| to_do.contains(&text(f, "id")))
        .count();

    c.other.files_scanned = report.cover.files_scanned;
    c.other.repositories = report.cover.repos.len();
    // Rules checked is a fact of the scan, not of any row: it cannot be recounted from the
    // archive, so the recount takes the object's own figure and the check compares the rest.
    c.other.rules_checked = report.counts.other.rules_checked;
    // Found in the client's repository at scan time; no row in the package can recount it.
    c.other.client_suppressions_found = report.counts.other.client_suppressions_found;
    // The review budget is a setting of the run and whether every critical and unsure row was
    // decided is the gate's own check ([`super::needs_decision`]): neither can be recounted from
    // the archive, so the recount takes the object's figures and the check compares the rest.
    c.review.review_budget = report.counts.review.review_budget;
    c.review.every_critical_and_unsure_decided =
        report.counts.review.every_critical_and_unsure_decided;
    c.other.rules_with_problems = problem_rows
        .iter()
        .map(|r| text(r, "rule_id"))
        .collect::<BTreeSet<_>>()
        .len();
    Ok(c)
}

/// The identities of the design, restated here with the gate's own arithmetic (it does not call
/// `ReportCounts::identity_failures`: a check that calls the code it checks proves nothing).
/// The list of those that do not hold.
fn identity_failures(c: &ReportCounts) -> Vec<&'static str> {
    let (r, p) = (&c.rows, &c.problems);
    let sev = &p.by_severity;
    let mut out = Vec::new();
    if r.total != r.dependency + r.ruled_out + r.problem + r.convention + r.not_checked {
        out.push(
            "source rows are not dependency + ruled out + problem + convention + not checked rows",
        );
    }
    if p.total != p.needs_decision + p.for_action + p.accepted + p.information_only {
        out.push("problems are not needs a decision + for action + accepted + information only");
    }
    if r.problem + p.beside_ruled_out != p.total + r.folded {
        out.push("problem rows and places beside a ruled out row are not problems + rows at a shared place");
    }
    if p.for_action != p.first + p.next + p.later {
        out.push("for action is not first + next + later");
    }
    if p.total != sev.critical + sev.high + sev.medium + sev.low {
        out.push("problems are not critical + high + medium + low");
    }
    out
}

/// The object as `(path, value)` leaves, for comparing two objects field by field.
fn leaves(c: &ReportCounts) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    fn walk(prefix: &str, v: &Value, out: &mut Vec<(String, u64)>) {
        match v {
            Value::Object(map) => {
                for (k, x) in map {
                    let p = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&p, x, out);
                }
            }
            Value::Number(n) => out.push((prefix.to_string(), n.as_u64().unwrap_or(u64::MAX))),
            _ => {}
        }
    }
    walk(
        "",
        &serde_json::to_value(c).unwrap_or(Value::Null),
        &mut out,
    );
    out
}

/// Fields where two counts objects differ, as `(path, left, right)`.
fn differences(left: &ReportCounts, right: &ReportCounts) -> Vec<(String, u64, u64)> {
    let r: BTreeMap<String, u64> = leaves(right).into_iter().collect();
    leaves(left)
        .into_iter()
        .filter_map(|(k, a)| {
            let b = r.get(&k).copied().unwrap_or(u64::MAX);
            (a != b).then_some((k, a, b))
        })
        .collect()
}

/// Every integer written in `sentence`, with thousands commas removed.
fn integers_in(sentence: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let chars: Vec<char> = sentence.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            let mut digits = String::new();
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || (chars[i] == ',' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()))
            {
                if chars[i] != ',' {
                    digits.push(chars[i]);
                }
                i += 1;
            }
            let glued = (start > 0 && chars[start - 1].is_alphanumeric())
                || (i < chars.len() && chars[i].is_alphanumeric());
            if !glued {
                if let Ok(v) = digits.parse() {
                    out.push(v);
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The column that is filled in every data row of a sheet (and never holds the note an empty
/// sheet carries in its first column), found by its header name.
fn key_header(sheet: &str) -> Option<&'static str> {
    match sheet {
        v::ALL_PROBLEMS | v::NEEDS_A_DECISION => Some(v::H_SEVERITY),
        v::FIX_PLAN => Some(v::H_FIX_ID),
        v::DEPENDENCIES => Some(v::H_ADVISORY),
        v::YOUR_CONVENTIONS => Some(v::H_HOW_CONSISTENTLY),
        _ => None,
    }
}

/// The data rows of a workbook sheet: the rows after the header with the sheet's key column
/// filled. The header is found by name, as the older checks find theirs. `Err` when the sheet or
/// its key column is not there.
pub(super) fn sheet_rows(xlsx: &[u8], sheet: &str) -> Result<usize, String> {
    let key = key_header(sheet).ok_or_else(|| format!("no key column is known for {sheet:?}"))?;
    let grid = super::archive::xlsx_sheet_grid(xlsx, sheet)?;
    let header = grid
        .first()
        .ok_or_else(|| format!("the {sheet} sheet is empty"))?;
    let col = header
        .iter()
        .position(|h| h == key)
        .ok_or_else(|| format!("the {sheet} sheet has no {key:?} column"))?;
    Ok(grid
        .iter()
        .skip(1)
        .filter(|r| r.get(col).is_some_and(|c| !c.is_empty()))
        .count())
}

/// The row count Start here states for `sheet` (its third column).
fn start_here_count(grid: &[Vec<String>], sheet: &str) -> Option<usize> {
    grid.iter()
        .find(|row| row.first().is_some_and(|c| c == sheet))
        .and_then(|row| row.get(2))
        .and_then(|t| t.parse::<f64>().ok())
        .map(|x| x as usize)
}

pub(super) fn counts_problems_with(
    entries: &Entries,
    report: &AuditReportJson,
    checks: CountsChecks,
) -> Vec<Problem> {
    let recounted = match recount(entries, report) {
        Ok(c) => c,
        Err(refusals) => return refusals,
    };
    let mut problems = Vec::new();

    // 1. The recount equals the object in json/inspection.json, and the PDF data's object equals both.
    let findings_json: Option<Value> = entries
        .get(INSPECTION_JSON)
        .and_then(|b| serde_json::from_slice(b).ok());
    match findings_json
        .as_ref()
        .and_then(|v| v.get("counts"))
        .and_then(|c| serde_json::from_value::<ReportCounts>(c.clone()).ok())
    {
        None => problems.push(problem(
            INSPECTION_JSON,
            COUNTS,
            concat!(
                camerata_api_types::pkg_inspection_json!(),
                " has no counts object"
            ),
        )),
        Some(object) => {
            for (field, printed, want) in differences(&object, &recounted) {
                problems.push(problem(
                    INSPECTION_JSON,
                    &format!("counts.{field}"),
                    format!(
                        "{INSPECTION_JSON} states {printed} and the files in the package give {want}"
                    ),
                ));
            }
            for (field, pdf, json) in differences(&report.counts, &object) {
                problems.push(problem(
                    "PDF data",
                    &format!("counts.{field}"),
                    format!("the PDF data states {pdf} and {INSPECTION_JSON} states {json}"),
                ));
            }
        }
    }

    // 1b. Every field of json/inspection.json `summary` equals the recount.
    problems.extend(summary_problems(findings_json.as_ref(), &recounted));
    problems.extend(more_line_problems(report, &recounted));

    // 2. The identities, on the recount and on the object (the gate's own arithmetic).
    for failure in identity_failures(&recounted) {
        problems.push(problem(ALL_FINDINGS_JSON, "identities", failure));
    }
    if let Some(object) = findings_json
        .as_ref()
        .and_then(|v| v.get("counts"))
        .and_then(|c| serde_json::from_value::<ReportCounts>(c.clone()).ok())
    {
        for failure in identity_failures(&object) {
            problems.push(problem(INSPECTION_JSON, "identities", failure));
        }
    }

    // 3. Every surface equals the object (the recount is the reference).
    let want = &recounted;
    let stats = &report.cover.stats;
    for (name, printed, w) in [
        (
            "critical",
            stats.critical,
            want.problems.by_severity.critical,
        ),
        ("high", stats.high, want.problems.by_severity.high),
        ("medium", stats.medium, want.problems.by_severity.medium),
        ("low", stats.low, want.problems.by_severity.low),
        ("accepted", stats.accepted, want.problems.accepted),
        (
            "dependency_advisories",
            stats.dependency_advisories,
            want.rows.dependency,
        ),
    ] {
        if printed != w {
            problems.push(problem(
                "PDF data",
                &format!("cover.stats.{name}"),
                format!("the cover states {printed} and the counts give {w}"),
            ));
        }
    }
    let grid_cells: usize = report
        .priority_grid
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .map(|c| c.findings.len())
        .sum();
    if grid_cells != want.problems.for_action {
        problems.push(problem(
            "PDF data",
            "priority_grid",
            format!(
                "the grid holds {grid_cells} problems and the counts give {} for action",
                want.problems.for_action
            ),
        ));
    }
    // The cover strip, the grid's notes and the grid's counts are the sentences rebuilt from the
    // recount, string for string.
    for (location, printed, expected) in [
        (
            "cover.strip",
            report.cover.strip.clone(),
            counts_prose::cover_strip(want),
        ),
        (
            "priority_grid.accepted_line",
            report.priority_grid.accepted_line.clone(),
            counts_prose::grid_accepted_line(want),
        ),
        (
            "priority_grid.left_out",
            report.priority_grid.left_out.clone(),
            counts_prose::grid_left_out(want),
        ),
        (
            "methodology.reconciliation",
            report.methodology.reconciliation.clone(),
            counts_prose::methodology_line(want, report.review_state),
        ),
    ] {
        if printed != expected {
            problems.push(problem(
                "PDF data",
                location,
                format!("the PDF data reads {printed:?} and the counts give {expected:?}"),
            ));
        }
    }
    for (name, printed, w) in [
        (
            "accepted_count",
            report.priority_grid.accepted_count,
            want.problems.accepted,
        ),
        (
            "informational_count",
            report.priority_grid.informational_count,
            want.problems.information_only,
        ),
        (
            "held_count",
            report.priority_grid.held_count,
            want.problems.needs_decision,
        ),
    ] {
        if printed != w {
            problems.push(problem(
                "PDF data",
                &format!("priority_grid.{name}"),
                format!("the grid states {printed} and the counts give {w}"),
            ));
        }
    }
    let grid_total: usize = report
        .priority_grid
        .rows
        .iter()
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.count)
        .sum();
    if grid_total != want.problems.for_action {
        problems.push(problem(
            "PDF data",
            "priority_grid.cells",
            format!(
                "the grid's cells add up to {grid_total} and the counts give {} problems for action",
                want.problems.for_action
            ),
        ));
    }
    problems.extend(workbook_problems(entries, want));
    problems.extend(order_problems(entries, report, want));
    if let Some(csv) = entries.get(TICKETS_CSV) {
        let tickets = csv::Reader::from_reader(csv.as_slice()).records().count();
        if tickets != want.fixes.to_do {
            problems.push(problem(
                TICKETS_CSV,
                "rows",
                format!(
                    "the file lists {tickets} tickets and the counts give {} fixes to do",
                    want.fixes.to_do
                ),
            ));
        }
    } else {
        problems.push(problem(
            TICKETS_CSV,
            "rows",
            "the tickets file is missing, so its rows cannot be counted",
        ));
    }

    // 4. Count sentences equal the ones rebuilt from the recount.
    let summary = &report.executive_summary;
    if !summary.is_override {
        let expected = counts_prose::narrative(want, report.review_state);
        if summary.narrative != expected {
            problems.push(problem(
                "PDF data",
                "executive_summary.narrative",
                format!(
                    "the narrative reads {:?} and the counts give {expected:?}",
                    summary.narrative
                ),
            ));
        }
    }
    let next_steps = &report.methodology.next_steps;
    let expected = counts_prose::decision_sentence(want);
    if counts_prose::states_decision_count(report.review_state, report.internal) {
        if !next_steps.contains(&expected) {
            problems.push(problem(
                "PDF data",
                "methodology.next_steps",
                format!("the next steps do not state {expected:?}; they read {next_steps:?}"),
            ));
        }
    } else {
        // A released client package: the next steps equal the rebuilt sentence, which states no
        // decision count and no "so far".
        let rebuilt = counts_prose::next_steps(
            want,
            report.cover.repos.len() > 1,
            report.review_state,
            report.internal,
        );
        if *next_steps != rebuilt {
            problems.push(problem(
                "PDF data",
                "methodology.next_steps",
                format!("a released package's next steps read {next_steps:?} and the counts give {rebuilt:?}"),
            ));
        }
    }

    // 5. No stray number in an override.
    if summary.is_override {
        let values: BTreeSet<u64> = leaves(want).into_iter().map(|(_, x)| x).collect();
        for number in integers_in(&summary.narrative) {
            if !values.contains(&number) {
                problems.push(problem(
                    "PDF data",
                    "executive_summary.narrative",
                    format!("the summary text states {number}, which is no count of this package; the operator fixes the text"),
                ));
            }
        }
    }

    // 6. (work order D) A row a baseline record or a waiver closed needs a reviewer's decision.
    problems.extend(accepted_rows_without_a_decision_problems(entries));

    // 7. (work order D) Every folded row names its head.
    if checks.folded_rows_name_their_head {
        problems.extend(folded_rows_problems(entries));
    }
    problems
}

/// The workbook part of check 3: the rows of each sheet and the row count Start here states for
/// it equal the counts. Fail closed: a missing stem, workbook, sheet, key column or Start here row
/// is a problem, never a skip.
fn workbook_problems(entries: &Entries, want: &ReportCounts) -> Vec<Problem> {
    let Some(stem) = super::consistency::package_stem(entries) else {
        return vec![problem(
            "package",
            "workbook",
            "the package does not hold exactly one PDF, so its workbook cannot be found to count its rows",
        )];
    };
    let xlsx_name = camerata_api_types::package_paths::workbook_name(&stem);
    let Some(xlsx) = entries.get(&xlsx_name) else {
        return vec![problem(
            &xlsx_name,
            "workbook",
            "the workbook is missing, so its rows cannot be counted",
        )];
    };
    let start = match super::archive::xlsx_sheet_grid(xlsx, v::START_HERE) {
        Ok(grid) => grid,
        Err(e) => {
            return vec![problem(
                &xlsx_name,
                "sheet \"Start here\"",
                format!("the Start here sheet cannot be read: {e}"),
            )]
        }
    };
    let mut problems = Vec::new();
    for (sheet, w) in [
        (v::ALL_PROBLEMS, want.problems.total),
        (v::NEEDS_A_DECISION, want.problems.needs_decision),
        (v::FIX_PLAN, want.fixes.to_do),
        (v::DEPENDENCIES, want.rows.dependency),
        (v::YOUR_CONVENTIONS, want.rows.convention),
    ] {
        match sheet_rows(xlsx, sheet) {
            Ok(rows) if rows != w => problems.push(problem(
                &xlsx_name,
                &format!("sheet \"{sheet}\" rows"),
                format!("the sheet lists {rows} rows and the counts give {w}"),
            )),
            Ok(_) => {}
            Err(e) => problems.push(problem(
                &xlsx_name,
                &format!("sheet \"{sheet}\" rows"),
                format!("the rows of the sheet cannot be counted: {e}"),
            )),
        }
        match start_here_count(&start, sheet) {
            Some(said) if said != w => problems.push(problem(
                &xlsx_name,
                &format!("sheet \"Start here\" row count, sheet {sheet}"),
                format!("Start here states {said} and the counts give {w}"),
            )),
            Some(_) => {}
            None => problems.push(problem(
                &xlsx_name,
                &format!("sheet \"Start here\" row count, sheet {sheet}"),
                format!("Start here has no row count for the {sheet} sheet"),
            )),
        }
    }
    // The scorecard's Total row on Start here: the problems by severity and in all.
    let sev = &want.problems.by_severity;
    let total_row: Option<Vec<usize>> = start
        .iter()
        .find(|row| row.first().is_some_and(|c| c == "Total"))
        .map(|row| {
            (1..=5)
                .map(|i| {
                    row.get(i)
                        .and_then(|t| t.parse::<f64>().ok())
                        .map_or(usize::MAX, |x| x as usize)
                })
                .collect()
        });
    let wanted = vec![
        sev.critical,
        sev.high,
        sev.medium,
        sev.low,
        want.problems.total,
    ];
    match total_row {
        Some(printed) if printed != wanted => problems.push(problem(
            &xlsx_name,
            "sheet \"Start here\" scorecard Total",
            format!("the scorecard Total states {printed:?} and the counts give {wanted:?}"),
        )),
        Some(_) => {}
        None => problems.push(problem(
            &xlsx_name,
            "sheet \"Start here\" scorecard Total",
            "Start here has no scorecard Total row",
        )),
    }
    // The Not checked sheet lists a row for each rule that could not be completed.
    match not_checked_rule_rows(xlsx) {
        Ok(rows) if rows != want.rows.not_checked => problems.push(problem(
            &xlsx_name,
            &format!("sheet \"{}\" rows", v::NOT_CHECKED),
            format!(
                "the sheet lists {rows} rules that could not be completed and the counts give {}",
                want.rows.not_checked
            ),
        )),
        Ok(_) => {}
        Err(e) => problems.push(problem(
            &xlsx_name,
            &format!("sheet \"{}\" rows", v::NOT_CHECKED),
            format!("the rows of the sheet cannot be counted: {e}"),
        )),
    }
    problems
}

/// The rows of the Not checked sheet that stand for a rule that could not be completed.
fn not_checked_rule_rows(xlsx: &[u8]) -> Result<usize, String> {
    let grid = super::archive::xlsx_sheet_grid(xlsx, v::NOT_CHECKED)?;
    let header = grid
        .first()
        .ok_or_else(|| format!("the {} sheet is empty", v::NOT_CHECKED))?;
    let col = header
        .iter()
        .position(|h| h == v::H_GAP)
        .ok_or_else(|| format!("the {} sheet has no {:?} column", v::NOT_CHECKED, v::H_GAP))?;
    Ok(grid
        .iter()
        .skip(1)
        .filter(|r| {
            r.get(col)
                .is_some_and(|c| c == v::GAP_RULE_COULD_NOT_COMPLETE)
        })
        .count())
}

/// Every "and N more" line of the PDF data is the line the counts and the shown list give: the
/// dependency, escalation and patterns lines recomputed exactly; the Fix plan and Departures lines
/// (whose totals the object does not carry) may only be there when their list is full.
fn more_line_problems(report: &AuditReportJson, want: &ReportCounts) -> Vec<Problem> {
    use crate::report_export::{PDF_MAX_DEPARTURES, PDF_MAX_OTHER_FIXES, PDF_MAX_SERIOUS_PROBLEMS};
    let mut out = Vec::new();
    let mut expect = |location: &str, printed: &str, expected: String| {
        if printed != expected {
            out.push(problem(
                "PDF data",
                location,
                format!("the PDF data states {printed:?} and the counts give {expected:?}"),
            ));
        }
    };
    let line = |n: usize, sheet: &str| {
        if n == 0 {
            String::new()
        } else {
            v::more_in_sheet(n, sheet)
        }
    };
    let snapshot = &report.dependency_snapshot;
    expect(
        "dependency_snapshot.more_line",
        &snapshot.more_line,
        line(
            want.rows.dependency.saturating_sub(snapshot.worst.len()),
            v::DEPENDENCIES,
        ),
    );
    let esc = &report.escalations;
    expect(
        "escalations.more_line",
        &esc.more_line,
        line(
            esc.items.len().saturating_sub(PDF_MAX_SERIOUS_PROBLEMS),
            v::FIX_PLAN,
        ),
    );
    if let Some(p) = &report.patterns_across_services {
        expect(
            "patterns_across_services.more_line",
            &p.more_line,
            line(
                p.rows.len().saturating_sub(p.summary_lines.len()),
                v::PATTERNS_ACROSS_SERVICES,
            ),
        );
    }
    // The totals of these two are not in the object: a line is only possible when the list is
    // full, and it must be a well formed "and N more" line for its sheet.
    let conv = &report.conventions;
    let other = &report.other_fixes;
    for (location, printed, full, sheet) in [
        (
            "conventions.more_line",
            conv.more_line.as_str(),
            conv.departures.len() >= PDF_MAX_DEPARTURES,
            v::DEPARTURES,
        ),
        (
            "other_fixes.more_line",
            other.more_line.as_str(),
            other.shown >= PDF_MAX_OTHER_FIXES,
            v::FIX_PLAN,
        ),
    ] {
        if printed.is_empty() {
            continue;
        }
        let well_formed = (1..100_000usize).any(|n| printed == v::more_in_sheet(n, sheet));
        if !full || !well_formed {
            out.push(problem(
                "PDF data",
                location,
                format!("the PDF data states {printed:?} for a list that is not full or the line is malformed"),
            ));
        }
    }
    out
}

/// Every tally of json/inspection.json `summary` equals the recount, and the retired row tallies
/// (`curated_total`, `open`) are not in the file. Fail closed: a missing summary or tally is a
/// problem.
fn summary_problems(findings_json: Option<&Value>, want: &ReportCounts) -> Vec<Problem> {
    let Some(summary) = findings_json.and_then(|v| v.get("summary")) else {
        return vec![problem(
            INSPECTION_JSON,
            "summary",
            concat!(
                camerata_api_types::pkg_inspection_json!(),
                " has no summary object"
            ),
        )];
    };
    let mut out = Vec::new();
    let expected: [(&str, usize); 8] = [
        ("candidates_reviewed", want.rows.total),
        ("excluded_false_positive", want.rows.ruled_out),
        ("held_for_review", want.problems.needs_decision),
        ("dependency_advisories", want.rows.dependency),
        ("do_now", want.problems.first),
        ("do_next", want.problems.next),
        ("plan", want.problems.later),
        ("accepted", want.problems.accepted),
    ];
    for (field, n) in expected {
        match summary.get(field).and_then(Value::as_u64) {
            Some(printed) if printed as usize == n => {}
            printed => out.push(problem(
                INSPECTION_JSON,
                &format!("summary.{field}"),
                format!(
                    "{INSPECTION_JSON} summary states {} and the counts give {n}",
                    printed.map_or("nothing".to_string(), |p| p.to_string())
                ),
            )),
        }
    }
    for retired in ["curated_total", "open"] {
        if summary.get(retired).is_some() {
            out.push(problem(
                INSPECTION_JSON,
                &format!("summary.{retired}"),
                concat!(
                    camerata_api_types::pkg_inspection_json!(),
                    " summary holds a retired row tally"
                ),
            ));
        }
    }
    out
}

/// Check 6: every row with status `carried-baseline` or `waived-inline` has a reviewer's decision
/// this session. A row nobody decided is still read as accepted by its baseline record or waiver
/// alone, which closes a problem without anyone ruling on it. The row's disposition says which:
/// `BaselineAccepted` and `WaivedInline` are the dispositions with no decision behind them.
fn accepted_rows_without_a_decision_problems(entries: &Entries) -> Vec<Problem> {
    let Ok(live) = array_of(entries, ALL_FINDINGS_JSON) else {
        // The recount has already refused a package it cannot read.
        return Vec::new();
    };
    live.iter()
        .filter(|r| {
            let status = text(r, "status");
            crate::finding_status::is_carried_baseline(&status)
                || crate::finding_status::is_waived_inline(&status)
        })
        .filter(|r| {
            matches!(
                text(r, "disposition_kind").as_str(),
                "BaselineAccepted" | "WaivedInline"
            )
        })
        .map(|r| {
            problem(
                ALL_FINDINGS_JSON,
                &format!("place {}:{}", text(r, "path"), text(r, "line")),
                format!(
                    "{} is closed by {} and no reviewer has decided it this session",
                    text(r, "rule_id"),
                    if crate::finding_status::is_waived_inline(&text(r, "status")) {
                        "a waiver comment"
                    } else {
                        "a baseline record"
                    },
                ),
            )
        })
        .collect()
}

/// Check 7: in every place with more than one problem row, all rows but the head carry
/// `carried_by` naming the head's `site_id`.
fn folded_rows_problems(entries: &Entries) -> Vec<Problem> {
    let Ok(live) = array_of(entries, ALL_FINDINGS_JSON) else {
        // The recount has already refused a package it cannot read.
        return Vec::new();
    };
    let mut at: BTreeMap<(String, String, String, String), Vec<&Value>> = BTreeMap::new();
    for r in live.iter().filter(|r| {
        crate::xlsx_export::ALL_PROBLEMS_SHEET_KINDS.contains(&text(r, "kind").as_str())
    }) {
        at.entry(place_key_of(r)).or_default().push(r);
    }
    let mut out = Vec::new();
    for (place, members) in at.iter().filter(|(_, m)| m.len() > 1) {
        let heads: BTreeSet<String> = members
            .iter()
            .map(|m| text(m, "carried_by"))
            .filter(|h| !h.is_empty())
            .collect();
        let uncarried = members
            .iter()
            .filter(|m| text(m, "carried_by").is_empty())
            .count();
        // One head, named by every other row.
        if heads.len() != 1 || uncarried != 1 {
            out.push(problem(
                ALL_FINDINGS_JSON,
                &format!("place {}:{}", place.1, place.2),
                format!(
                    "{} rows share this place and {} of them do not name the row that heads it",
                    members.len(),
                    uncarried.saturating_sub(1)
                ),
            ));
        }
    }
    out
}

// ── Fix order: the gate's own tally of the order words ─────────────────────────────────────

const ORDER_WORDS: [&str; 6] = [
    v::FIX_ORDER_FIRST,
    v::FIX_ORDER_NEXT,
    v::FIX_ORDER_LATER,
    v::FIX_CLASS_NEEDS_DECISION,
    v::FIX_CLASS_INFORMATION_ONLY,
    v::FIX_CLASS_ACCEPTED,
];

/// The rank of an order word in print order (First is 0). `None` for a word nobody prints.
fn word_rank(word: &str) -> Option<usize> {
    ORDER_WORDS.iter().position(|w| *w == word)
}

/// The rank of a place from the buckets of its rows: a held row or an unknown bucket needs a
/// decision whatever else is there, then the earliest row for action, then information only, then
/// accepted risk. Written here for the gate alone.
fn place_rank(buckets: &[String]) -> usize {
    let known = ["do_now", "do_next", "plan", "informational", "accepted"];
    if buckets.iter().any(|b| !known.contains(&b.as_str())) {
        return 3;
    }
    for (bucket, rank) in [("do_now", 0), ("do_next", 1), ("plan", 2)] {
        if buckets.iter().any(|b| b == bucket) {
            return rank;
        }
    }
    if buckets.iter().any(|b| b == "informational") {
        4
    } else {
        5
    }
}

/// The order rank each Fix should print, from the rows of the package alone: the earliest place
/// for action among the places of its problem rows, else the earliest of the other classes.
/// An advisory is a place for action, First when critical or high, otherwise Later (the order of a row
/// with no effort estimate and no decision).
fn expected_fix_ranks(live: &[Value], deps: &[Value]) -> BTreeMap<String, usize> {
    let mut at: BTreeMap<(String, String, String, String), Vec<&Value>> = BTreeMap::new();
    for r in live.iter().filter(|r| {
        crate::xlsx_export::ALL_PROBLEMS_SHEET_KINDS.contains(&text(r, "kind").as_str())
    }) {
        at.entry(place_key_of(r)).or_default().push(r);
    }
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    let mut note = |fix: String, rank: usize| {
        if !fix.is_empty() {
            let slot = out.entry(fix).or_insert(rank);
            *slot = (*slot).min(rank);
        }
    };
    for members in at.values() {
        let buckets: Vec<String> = members.iter().map(|m| text(m, "bucket")).collect();
        let rank = place_rank(&buckets);
        // A row a reviewer, a baseline record or a waiver accepted is not a change of its Fix.
        for m in members.iter().filter(|m| {
            !matches!(
                text(m, "disposition_kind").as_str(),
                "Ignored" | "BaselineAccepted" | "WaivedInline"
            )
        }) {
            note(text(m, "fix_id"), rank);
        }
    }
    for d in deps {
        let rank = match text(d, "severity").to_ascii_lowercase().as_str() {
            "critical" | "high" => 0,
            _ => 2,
        };
        note(text(d, "fix_id"), rank);
    }
    out
}

/// The column of `header` in a sheet grid.
fn column_of(grid: &[Vec<String>], header: &str) -> Option<usize> {
    grid.first()?.iter().position(|h| h == header)
}

/// Fix order, recounted. All surfaces take their order words from the rows: the All problems
/// column tallies to the problems counts, each Fix plan row and each "other fixes" line carries
/// the order its places give, and the three-things box is empty exactly when nothing is First.
fn order_problems(
    entries: &Entries,
    report: &AuditReportJson,
    want: &ReportCounts,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let (Ok(live), Ok(deps)) = (
        array_of(entries, ALL_FINDINGS_JSON),
        array_of(entries, DEPENDENCIES_JSON),
    ) else {
        // The recount has already refused a package it cannot read.
        return out;
    };
    let ranks = expected_fix_ranks(&live, &deps);

    // The three-things box is empty exactly when no problem is First.
    if report.three_things.items.is_empty() != (want.problems.first == 0) {
        out.push(problem(
            "PDF data",
            "three_things",
            format!(
                "the box lists {} things and the counts give {} problems First",
                report.three_things.items.len(),
                want.problems.first
            ),
        ));
    }

    // Each "other fixes" line.
    for line in report.other_fixes.groups.iter().flat_map(|g| &g.lines) {
        match ranks.get(&line.fix_id) {
            Some(rank) if word_rank(&line.fix_order) == Some(*rank) => {}
            Some(rank) => out.push(problem(
                "PDF data",
                &format!("other_fixes, fix {}", line.fix_id),
                format!(
                    "the line says {:?} and the places of the fix give {:?}",
                    line.fix_order, ORDER_WORDS[*rank]
                ),
            )),
            None => out.push(problem(
                "PDF data",
                &format!("other_fixes, fix {:?}", line.fix_id),
                "the line names a fix that has no place in the package",
            )),
        }
    }

    // The workbook: the All problems column and each Fix plan row. Fail closed: a workbook,
    // sheet or column that cannot be read is a problem, never a skip.
    let Some(stem) = super::consistency::package_stem(entries) else {
        out.push(problem(
            "package",
            "fix order",
            "the package does not hold exactly one PDF, so its workbook cannot be found to check the fix order",
        ));
        return out;
    };
    let xlsx_name = camerata_api_types::package_paths::workbook_name(&stem);
    let Some(xlsx) = entries.get(&xlsx_name) else {
        out.push(problem(
            &xlsx_name,
            "fix order",
            "the workbook is missing, so its fix order cannot be checked",
        ));
        return out;
    };
    // A sheet's grid and the columns of it that the check reads.
    let mut sheet = |name: &str, headers: [&str; 2]| -> Option<(Vec<Vec<String>>, [usize; 2])> {
        let grid = match super::archive::xlsx_sheet_grid(xlsx, name) {
            Ok(g) => g,
            Err(e) => {
                out.push(problem(
                    &xlsx_name,
                    &format!("sheet \"{name}\" fix order"),
                    format!("the sheet cannot be read to check the fix order: {e}"),
                ));
                return None;
            }
        };
        let mut at = [0usize; 2];
        for (slot, header) in at.iter_mut().zip(headers) {
            match column_of(&grid, header) {
                Some(c) => *slot = c,
                None => {
                    out.push(problem(
                        &xlsx_name,
                        &format!("sheet \"{name}\" fix order"),
                        format!("the sheet has no {header:?} column, so its fix order cannot be checked"),
                    ));
                    return None;
                }
            }
        }
        Some((grid, at))
    };
    let all = sheet(v::ALL_PROBLEMS, [v::H_FIX_ORDER, v::H_SEVERITY]);
    let plan = sheet(v::FIX_PLAN, [v::H_FIX_ORDER, v::H_FIX_ID]);
    if let Some((grid, [order, sev])) = all {
        let mut tally = [0usize; 6];
        for row in grid
            .iter()
            .skip(1)
            .filter(|r| r.get(sev).is_some_and(|c| !c.is_empty()))
        {
            match row.get(order).and_then(|w| word_rank(w)) {
                Some(rank) => tally[rank] += 1,
                None => out.push(problem(
                    &xlsx_name,
                    "sheet \"All problems\" Fix order",
                    format!(
                        "a row says {:?}, which is no order or class",
                        row.get(order)
                    ),
                )),
            }
        }
        let p = &want.problems;
        for (word, got, w) in [
            (v::FIX_ORDER_FIRST, tally[0], p.first),
            (v::FIX_ORDER_NEXT, tally[1], p.next),
            (v::FIX_ORDER_LATER, tally[2], p.later),
            (v::FIX_CLASS_NEEDS_DECISION, tally[3], p.needs_decision),
            (v::FIX_CLASS_INFORMATION_ONLY, tally[4], p.information_only),
            (v::FIX_CLASS_ACCEPTED, tally[5], p.accepted),
        ] {
            if got != w {
                out.push(problem(
                    &xlsx_name,
                    &format!("sheet \"All problems\" Fix order {word:?}"),
                    format!("the column says {word:?} {got} times and the counts give {w}"),
                ));
            }
        }
    }
    if let Some((grid, [order, id])) = plan {
        let dependency_fixes: BTreeSet<String> = deps
            .iter()
            .map(|d| text(d, "fix_id"))
            .filter(|f| !f.is_empty())
            .collect();
        let mut dependency_fixes_first = 0usize;
        for row in grid
            .iter()
            .skip(1)
            .filter(|r| r.get(id).is_some_and(|c| !c.is_empty()))
        {
            let fix = &row[id];
            let said = row.get(order).map_or("", String::as_str);
            if said == v::FIX_ORDER_FIRST && dependency_fixes.contains(fix) {
                dependency_fixes_first += 1;
            }
            match ranks.get(fix) {
                Some(rank) if word_rank(said) == Some(*rank) => {}
                Some(rank) => out.push(problem(
                    &xlsx_name,
                    &format!("sheet \"Fix plan\", fix {fix}"),
                    format!(
                        "the row says {said:?} and the places of the fix give {:?}",
                        ORDER_WORDS[*rank]
                    ),
                )),
                None => out.push(problem(
                    &xlsx_name,
                    &format!("sheet \"Fix plan\", fix {fix}"),
                    "the row is a fix that has no place in the package",
                )),
            }
        }
        // An empty box says which dependency fixes are due first: the number is the First
        // dependency fixes on the Fix plan.
        if report.three_things.items.is_empty() {
            let expected = counts_prose::nothing_first_sentence(dependency_fixes_first);
            if report.three_things.total_hours_label != expected {
                out.push(problem(
                    "PDF data",
                    "three_things.total_hours_label",
                    format!(
                        "the empty box reads {:?} and the Fix plan gives {expected:?}",
                        report.three_things.total_hours_label
                    ),
                ));
            }
        }
    }
    out
}
