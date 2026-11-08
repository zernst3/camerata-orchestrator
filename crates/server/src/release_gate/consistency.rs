//! The gate's consistency checks (docs/design/2026-10-20_machine-readable-exports.md gates 1, 2,
//! 4, 6 and 7, and the PDF/`findings.json` row identity): the comparison logic lives HERE, in
//! production, and the export tests call these same functions and assert on what they return.
//! A test that re-implemented a comparison would be testing its own copy.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::report::Problem;
use crate::report_export::AuditReportJson;
use crate::xlsx_export::{FindingsExport, DATASETS};

const DATASET_SET: &str = "dataset set";
const DATASET_PARITY: &str = "dataset parity";

type Entries = BTreeMap<String, Vec<u8>>;

fn problem(file: &str, location: &str, message: impl AsRef<str>) -> Problem {
    Problem::new(file, location, message, None)
}

/// The filename stem of a package: the one top-level `*.pdf` member's name.
pub fn package_stem(entries: &Entries) -> Option<String> {
    let mut pdfs = entries
        .keys()
        .filter(|n| !n.contains('/') && n.ends_with(".pdf"));
    let first = pdfs.next()?;
    if pdfs.next().is_some() {
        return None;
    }
    first.strip_suffix(".pdf").map(str::to_string)
}

// ---- dataset set (gate 4) -----------------------------------------------------------------

/// Exactly the promised files and datasets: the PDF (starts `%PDF-`), the workbook (a zip with a
/// shared-strings table), `findings.json`, `README.txt`, `manifest.json`, and exactly the eight
/// `csv/*.csv` and `json/*.json` datasets. Nothing missing, nothing extra.
pub fn dataset_set_problems(entries: &Entries) -> Vec<Problem> {
    let mut problems = Vec::new();
    let Some(stem) = package_stem(entries) else {
        problems.push(problem(
            "(package)",
            DATASET_SET,
            "the package must contain exactly one top-level PDF",
        ));
        return problems;
    };
    let mut expected: BTreeSet<String> = BTreeSet::new();
    let pdf = format!("{stem}.pdf");
    let xlsx = format!("{stem}-findings.xlsx");
    expected.insert(pdf.clone());
    expected.insert(xlsx.clone());
    for fixed in [
        "findings.json",
        "decisions.json",
        "README.txt",
        "manifest.json",
    ] {
        expected.insert(fixed.to_string());
    }
    for dataset in DATASETS {
        expected.insert(format!("csv/{dataset}.csv"));
        expected.insert(format!("json/{dataset}.json"));
    }
    // The ticket file is a CSV with no JSON sibling (one ticket per sub-group, for an importer).
    expected.insert(crate::xlsx_export::tickets::TICKETS_CSV.to_string());
    // H4: the optional triage log is admitted when present (it is off by default, so a package
    // without it is complete). Its content is still scanned by the secrets check like every
    // other member.
    let mut allowed = expected.clone();
    allowed.insert(crate::xlsx_export::triage_log::TRIAGE_LOG_CSV.to_string());
    for name in &expected {
        if !entries.contains_key(name) {
            problems.push(problem(name, DATASET_SET, "a promised file is missing"));
        }
    }
    for name in entries.keys() {
        if !allowed.contains(name) {
            problems.push(problem(
                name,
                DATASET_SET,
                "an unexpected file is in the package",
            ));
        }
    }
    if let Some(bytes) = entries.get(&pdf) {
        if !bytes.starts_with(b"%PDF-") {
            problems.push(problem(
                &pdf,
                DATASET_SET,
                "the PDF does not start with %PDF-",
            ));
        }
    }
    if let Some(bytes) = entries.get(&xlsx) {
        if super::archive::xlsx_strings(bytes).is_err() {
            problems.push(problem(
                &xlsx,
                DATASET_SET,
                "the workbook is not a zip with a shared-strings table",
            ));
        }
    }
    for fixed in [
        "findings.json",
        "decisions.json",
        "README.txt",
        "manifest.json",
    ] {
        if entries.get(fixed).is_some_and(Vec::is_empty) {
            problems.push(problem(fixed, DATASET_SET, "the file is empty"));
        }
    }
    for dataset in DATASETS {
        let json = format!("json/{dataset}.json");
        if entries.get(&json).is_some_and(Vec::is_empty) {
            problems.push(problem(&json, DATASET_SET, "the file is empty"));
        }
    }
    problems
}

// ---- dataset parity (gates 1 and 2) ---------------------------------------------------------

fn rows_of(bytes: &[u8]) -> Result<Vec<Value>, String> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Array(rows)) => Ok(rows),
        Ok(_) => Err("the dataset JSON is not an array of rows".to_string()),
        Err(e) => Err(format!("the dataset JSON does not parse: {e}")),
    }
}

/// A JSON value as one CSV cell: string as is, number/bool as text, `null` or absent as empty, a
/// nested array/object as compact JSON.
fn cell_text(row: &Value, key: &str) -> String {
    match row.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn union_of_keys(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(Value::as_object)
        .flat_map(|object| object.keys().cloned())
        .collect()
}

/// One dataset's CSV against its JSON rows: the same row count, a header that is exactly the
/// union of the JSON rows' keys, and every cell equal to its JSON value under the documented
/// flattening. Parsed with the `csv` crate, so a quoting bug shows up as a different row or
/// column count.
pub fn compare_csv_to_json(dataset: &str, csv_bytes: &[u8], json_bytes: &[u8]) -> Vec<Problem> {
    let file = format!("csv/{dataset}.csv");
    let mut problems = Vec::new();
    let rows = match rows_of(json_bytes) {
        Ok(rows) => rows,
        Err(e) => {
            problems.push(problem(&format!("json/{dataset}.json"), DATASET_PARITY, e));
            return problems;
        }
    };
    let mut reader = csv::ReaderBuilder::new()
        .flexible(false)
        .from_reader(csv_bytes);
    let headers: Vec<String> = match reader.headers() {
        Ok(h) => h.iter().map(String::from).collect(),
        Err(e) => {
            problems.push(problem(
                &file,
                DATASET_PARITY,
                format!("the CSV header is unreadable: {e}"),
            ));
            return problems;
        }
    };
    let mut records: Vec<csv::StringRecord> = Vec::new();
    for (i, record) in reader.records().enumerate() {
        match record {
            Ok(r) => records.push(r),
            Err(e) => {
                problems.push(problem(
                    &file,
                    &format!("row {}", i + 1),
                    format!("the CSV row is unreadable: {e}"),
                ));
                return problems;
            }
        }
    }
    if records.len() != rows.len() {
        problems.push(problem(
            &file,
            DATASET_PARITY,
            format!(
                "the CSV has {} rows and the JSON has {}",
                records.len(),
                rows.len()
            ),
        ));
        return problems;
    }
    if rows.is_empty() {
        return problems;
    }
    let json_keys = union_of_keys(&rows);
    let csv_keys: BTreeSet<String> = headers.iter().cloned().collect();
    if csv_keys != json_keys || headers.len() != csv_keys.len() {
        problems.push(problem(
            &file,
            "header",
            "the CSV columns are not exactly the union of the JSON row keys",
        ));
        return problems;
    }
    for (i, (record, row)) in records.iter().zip(&rows).enumerate() {
        for (c, header) in headers.iter().enumerate() {
            if record.get(c).unwrap_or("") != cell_text(row, header) {
                problems.push(problem(
                    &file,
                    &format!("row {}, column {header:?}", i + 1),
                    "the cell differs between the CSV and the JSON",
                ));
            }
        }
    }
    problems
}

/// Every dataset's CSV against its JSON, and `findings.json`'s `findings` array against
/// `json/all-findings.json` (gate 1). Files missing from `entries` are the dataset-set check's
/// to report, not this one's.
pub fn dataset_parity_problems(entries: &Entries) -> Vec<Problem> {
    let mut problems = Vec::new();
    for dataset in DATASETS {
        let (Some(csv), Some(json)) = (
            entries.get(&format!("csv/{dataset}.csv")),
            entries.get(&format!("json/{dataset}.json")),
        ) else {
            continue;
        };
        problems.extend(compare_csv_to_json(dataset, csv, json));
    }
    if let (Some(findings), Some(all)) = (
        entries.get("findings.json"),
        entries.get("json/all-findings.json"),
    ) {
        match (serde_json::from_slice::<Value>(findings), rows_of(all)) {
            (Ok(doc), Ok(all_rows)) => {
                let findings_rows = doc
                    .get("findings")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if findings_rows != all_rows {
                    let at = findings_rows
                        .iter()
                        .zip(&all_rows)
                        .position(|(a, b)| a != b)
                        .unwrap_or_else(|| findings_rows.len().min(all_rows.len()));
                    problems.push(problem(
                        "findings.json",
                        &format!("findings[{at}]"),
                        "findings.json disagrees with json/all-findings.json",
                    ));
                }
            }
            _ => problems.push(problem(
                "findings.json",
                DATASET_PARITY,
                "findings.json or json/all-findings.json does not parse",
            )),
        }
    }
    problems.extend(fix_label_problems(entries));
    problems
}

/// The only labels a fix carries in a client file (D8): empty (the row has no fix text), or one of
/// the two the exporters write. Anything else, or a row with fix text and no label, is a defect
/// of the exporter, so the gate refuses the package.
fn fix_label_is_valid(row: &Value) -> bool {
    let label = cell_text(row, "fix_type");
    let has_fix = !cell_text(row, "fix").is_empty() || !cell_text(row, "fix_specific").is_empty();
    match label.as_str() {
        crate::report_export::TAILORED_FIX_LABEL => !cell_text(row, "fix_specific").is_empty(),
        crate::report_export::STANDARD_FIX_LABEL => has_fix,
        "" => !has_fix,
        _ => false,
    }
}

/// Every fix in `findings.json`, `json/all-findings.json` and `json/deviations.json` is labelled,
/// and labelled with one of the two client labels. The CSVs are the same rows by the parity check
/// above, so a label missing from a CSV already shows there as a column or cell difference.
fn fix_label_problems(entries: &Entries) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut check = |file: &str, rows: Vec<Value>, location: &dyn Fn(usize) -> String| {
        for (i, row) in rows.iter().enumerate() {
            if row.get("fix_type").is_none() {
                problems.push(problem(
                    file,
                    &location(i),
                    "a row has no fix_type field, so its fix is not labelled",
                ));
            } else if !fix_label_is_valid(row) {
                problems.push(problem(
                    file,
                    &location(i),
                    "the fix_type of a row is not Tailored fix or Standard fix, or does not match the fix the row carries",
                ));
            }
        }
    };
    if let Some(Ok(doc)) = entries
        .get("findings.json")
        .map(|b| serde_json::from_slice::<Value>(b))
    {
        let rows = doc
            .get("findings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        check("findings.json", rows, &|i| format!("findings[{i}]"));
    }
    for file in ["json/all-findings.json", "json/deviations.json"] {
        if let Some(Ok(rows)) = entries.get(file).map(|b| rows_of(b)) {
            // A deviations row carries its fix as `recommended_fix`; read it under the same name.
            // A deviations row without that key is not a group row, so it has no fix to label.
            let rows: Vec<Value> = rows
                .into_iter()
                .filter_map(|mut row| {
                    if file == "json/deviations.json" {
                        let fix = row.get("recommended_fix").cloned()?;
                        row["fix"] = fix;
                    }
                    Some(row)
                })
                .collect();
            check(file, rows, &|i| format!("row {}", i + 1));
        }
    }
    problems
}

// ---- row parity (req_d1) ----------------------------------------------------------------------

type RowId = (String, String, String, usize);

/// The identity set `(rule_id, repo, path, line)` of every row the PDF renders (all six matrix
/// buckets) must equal `findings.json`'s. A row in one and not the other is named by its rule
/// and location (never its text).
pub fn row_parity(report: &AuditReportJson, findings: &FindingsExport) -> Vec<Problem> {
    if report.reduced {
        return reduced_row_parity(report, findings);
    }
    let mut pdf: BTreeSet<RowId> = BTreeSet::new();
    for bucket in [
        &report.matrix.do_now,
        &report.matrix.do_next,
        &report.matrix.plan,
        &report.matrix.accepted,
        &report.matrix.informational,
        &report.matrix.held,
    ] {
        for f in bucket {
            pdf.insert((f.rule_id.clone(), f.repo.clone(), f.path.clone(), f.line));
        }
    }
    let value = serde_json::to_value(findings).unwrap_or(Value::Null);
    let mut json: BTreeSet<RowId> = BTreeSet::new();
    for row in value
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        json.insert((
            row["rule_id"].as_str().unwrap_or_default().to_string(),
            row["repo"].as_str().unwrap_or_default().to_string(),
            row["path"].as_str().unwrap_or_default().to_string(),
            usize::try_from(row["line"].as_u64().unwrap_or_default()).unwrap_or_default(),
        ));
    }
    let describe = |(rule, _repo, path, line): &RowId| format!("{rule} at {path}:{line}");
    let mut problems = Vec::new();
    for row in pdf.difference(&json) {
        problems.push(problem(
            "findings.json",
            &describe(row),
            "a row the PDF renders is missing from findings.json",
        ));
    }
    for row in json.difference(&pdf) {
        problems.push(problem(
            "PDF data",
            &describe(row),
            "a row in findings.json is missing from the PDF",
        ));
    }
    problems
}

/// Row parity for the reshaped PDF (above 30,000 sites), which lists no row: the count of rows in
/// each bucket, severity and kind that the PDF states must equal `findings.json`'s.
fn reduced_row_parity(report: &AuditReportJson, findings: &FindingsExport) -> Vec<Problem> {
    let mut pdf: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for r in &report.reduced_matrix {
        *pdf.entry((r.bucket.clone(), r.severity.clone(), r.kind.clone()))
            .or_default() += r.count;
    }
    let value = serde_json::to_value(findings).unwrap_or(Value::Null);
    let mut json: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for row in value
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        *json
            .entry((
                row["bucket"].as_str().unwrap_or_default().to_string(),
                row["severity"].as_str().unwrap_or_default().to_string(),
                row["kind"].as_str().unwrap_or_default().to_string(),
            ))
            .or_default() += 1;
    }
    let mut problems = Vec::new();
    let keys: BTreeSet<&(String, String, String)> = pdf.keys().chain(json.keys()).collect();
    for key in keys {
        let (p, j) = (
            pdf.get(key).copied().unwrap_or(0),
            json.get(key).copied().unwrap_or(0),
        );
        if p != j {
            problems.push(problem(
                "PDF data",
                &format!("bucket {}, severity {}, kind {}", key.0, key.1, key.2),
                format!("the reshaped PDF counts {p} rows here and findings.json has {j}"),
            ));
        }
    }
    problems
}

// ---- severity and kind counts (22d) ------------------------------------------------------------

const COUNTS: &str = "severity and kind counts";

/// Rows counted by `(severity, kind tag)`.
type Tally = BTreeMap<(String, String), usize>;

/// The four severity columns the summaries print (PDF cover, workbook Index): anything that is
/// not critical, high or medium is counted as low, exactly as both summaries count it.
fn severity_bucket(severity: &str) -> &'static str {
    match severity {
        "critical" => "critical",
        "high" => "high",
        "medium" => "medium",
        _ => "low",
    }
}

fn by_severity_bucket(tally: &Tally) -> BTreeMap<&'static str, usize> {
    let mut out: BTreeMap<&'static str, usize> = ["critical", "high", "medium", "low"]
        .iter()
        .map(|s| (*s, 0))
        .collect();
    for ((severity, _), n) in tally {
        *out.entry(severity_bucket(severity)).or_insert(0) += n;
    }
    out
}

fn by_kind(tally: &Tally) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for ((_, kind), n) in tally {
        *out.entry(kind.clone()).or_insert(0) += n;
    }
    out
}

/// Every `(severity, kind)` cell where `got` differs from `want`, one problem each, named by the
/// cell and both counts (never row text).
fn tally_differences(file: &str, what: &str, want: &Tally, got: &Tally) -> Vec<Problem> {
    tally_differences_against(file, what, "json/all-findings.json", want, got)
}

/// [`tally_differences`] against a reference other than `json/all-findings.json`, named in the
/// message.
fn tally_differences_against(
    file: &str,
    what: &str,
    reference: &str,
    want: &Tally,
    got: &Tally,
) -> Vec<Problem> {
    let keys: BTreeSet<&(String, String)> = want.keys().chain(got.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let (w, g) = (
                want.get(key).copied().unwrap_or(0),
                got.get(key).copied().unwrap_or(0),
            );
            (w != g).then(|| {
                problem(
                    file,
                    &format!("severity {}, kind {}", key.0, key.1),
                    format!("{what} has {g} rows here and {reference} has {w}"),
                )
            })
        })
        .collect()
}

/// `json/all-findings.json` by `(severity, kind)`: the reference every other artifact is held to.
fn json_tally(entries: &Entries) -> Option<Result<Tally, Problem>> {
    let bytes = entries.get("json/all-findings.json")?;
    Some(
        rows_of(bytes)
            .map(|rows| {
                let mut tally = Tally::new();
                for row in rows {
                    *tally
                        .entry((cell_text(&row, "severity"), cell_text(&row, "kind")))
                        .or_insert(0) += 1;
                }
                tally
            })
            .map_err(|e| problem("json/all-findings.json", COUNTS, e)),
    )
}

/// `json/all-findings.json` by `(severity, kind)` for the rows of `kinds`, one row for each place:
/// the first row at a (repo, file, line) heads it, as it does on the All problems sheet, so the
/// sheet is held to the places and not to the rows behind them.
fn places_tally(entries: &Entries, kinds: &[&str]) -> Tally {
    let mut tally = Tally::new();
    let Some(rows) = entries
        .get("json/all-findings.json")
        .and_then(|bytes| rows_of(bytes).ok())
    else {
        return tally;
    };
    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    for row in &rows {
        let kind = cell_text(row, "kind");
        if !kinds.contains(&kind.as_str()) {
            continue;
        }
        let place = (
            cell_text(row, "repo"),
            cell_text(row, "path"),
            cell_text(row, "line"),
        );
        if seen.insert(place) {
            *tally.entry((cell_text(row, "severity"), kind)).or_insert(0) += 1;
        }
    }
    tally
}

/// `csv/all-findings.csv` by `(severity, kind)`, located by header name.
fn csv_tally(bytes: &[u8]) -> Result<Tally, String> {
    let mut reader = csv::ReaderBuilder::new().from_reader(bytes);
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    // A table with no rows is written with no columns either (the header is the union of the
    // rows' keys), so there is nothing to locate: it tallies as empty, and the JSON's rows are
    // then held against that.
    if headers.iter().all(str::is_empty) && reader.records().next().is_none() {
        return Ok(Tally::new());
    }
    let col = |name: &str| {
        headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("the CSV has no {name:?} column"))
    };
    let (sev, kind) = (col("severity")?, col("kind")?);
    let mut tally = Tally::new();
    for record in reader.records() {
        let record = record.map_err(|e| e.to_string())?;
        let key = (
            record.get(sev).unwrap_or("").to_string(),
            record.get(kind).unwrap_or("").to_string(),
        );
        *tally.entry(key).or_insert(0) += 1;
    }
    Ok(tally)
}

/// The workbook's All problems sheet by `(severity, kind)` (its Severity cells are title-cased
/// for the reader; compared lower-cased).
fn workbook_rows_tally(xlsx: &[u8]) -> Result<Tally, String> {
    use crate::xlsx_export::vocabulary as v;
    let grid = super::archive::xlsx_sheet_grid(xlsx, v::ALL_PROBLEMS)?;
    let header = grid
        .first()
        .ok_or_else(|| format!("the {} sheet is empty", v::ALL_PROBLEMS))?;
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("the {} sheet has no {name:?} column", v::ALL_PROBLEMS))
    };
    let (sev, kind) = (col(v::H_SEVERITY)?, col(v::H_KIND)?);
    let mut tally = Tally::new();
    for row in grid.iter().skip(1) {
        let cell = |c: usize| row.get(c).map(String::as_str).unwrap_or("");
        // The note on an empty sheet sits in the first column with no Severity beside it.
        if cell(sev).is_empty() && cell(kind).is_empty() {
            continue;
        }
        *tally
            .entry((cell(sev).to_lowercase(), cell(kind).to_string()))
            .or_insert(0) += 1;
    }
    Ok(tally)
}

/// The Your conventions sheet's rows by kind, read from the How consistently cell (the sheet
/// prints no kind tag in its plain columns). A cell with neither known wording counts under
/// `"unrecognised status"`, so an edited cell shows up as a disagreement instead of vanishing.
fn conventions_sheet_tally(xlsx: &[u8]) -> Result<BTreeMap<String, usize>, String> {
    use crate::xlsx_export::vocabulary as v;
    let grid = super::archive::xlsx_sheet_grid(xlsx, v::YOUR_CONVENTIONS)?;
    let header = grid
        .first()
        .ok_or_else(|| format!("the {} sheet is empty", v::YOUR_CONVENTIONS))?;
    let status = header
        .iter()
        .position(|h| h == v::H_HOW_CONSISTENTLY)
        .ok_or_else(|| {
            format!(
                "the {} sheet has no {:?} column",
                v::YOUR_CONVENTIONS,
                v::H_HOW_CONSISTENTLY
            )
        })?;
    let mut tally = BTreeMap::new();
    for row in grid.iter().skip(1) {
        // The note on an empty sheet sits in the first column with no status beside it.
        let cell = row.get(status).map(String::as_str).unwrap_or("");
        if cell.is_empty() {
            continue;
        }
        let kind = if cell == crate::xlsx_export::CONVENTION_STATUS_FOLLOWED {
            "convention_observed"
        } else if cell == crate::xlsx_export::CONVENTION_STATUS_NONE_ADOPTED {
            "no_convention"
        } else {
            "unrecognised status"
        };
        *tally.entry(kind.to_string()).or_insert(0) += 1;
    }
    Ok(tally)
}

/// How many rows the Not checked sheet lists as "Rule could not be completed": its
/// `not_assessed` rows.
fn not_checked_sheet_not_assessed(xlsx: &[u8]) -> Result<usize, String> {
    use crate::xlsx_export::vocabulary as v;
    let grid = super::archive::xlsx_sheet_grid(xlsx, v::NOT_CHECKED)?;
    let Some(gap) = grid
        .first()
        .and_then(|header| header.iter().position(|h| h == v::H_GAP))
    else {
        return Ok(0);
    };
    Ok(grid
        .iter()
        .skip(1)
        .filter(|row| {
            row.get(gap)
                .is_some_and(|c| c == v::GAP_RULE_COULD_NOT_COMPLETE)
        })
        .count())
}

/// Start here's summaries: the row count it states for each sheet, and the scorecard's Total row
/// per severity column.
type StartSummary = (BTreeMap<String, usize>, BTreeMap<&'static str, usize>);

fn workbook_start_summary(xlsx: &[u8]) -> Result<StartSummary, String> {
    use crate::xlsx_export::vocabulary as v;
    let grid = super::archive::xlsx_sheet_grid(xlsx, v::START_HERE)?;
    let number = |row: &Vec<String>, c: usize| -> usize {
        row.get(c)
            .and_then(|t| t.parse::<f64>().ok())
            .map_or(0, |n| n as usize)
    };
    let mut sheet_counts = BTreeMap::new();
    for sheet in [v::ALL_PROBLEMS, v::YOUR_CONVENTIONS] {
        let count = grid
            .iter()
            .find(|row| row.first().is_some_and(|c| c == sheet))
            .map_or(0, |row| number(row, 2));
        sheet_counts.insert(sheet.to_string(), count);
    }
    let header_at = grid
        .iter()
        .position(|row| {
            row.first().is_some_and(|c| c == v::H_AREA) && row.get(5).is_some_and(|c| c == "Total")
        })
        .ok_or("Start here has no scorecard")?;
    let total_row = grid
        .iter()
        .skip(header_at + 1)
        .find(|row| row.first().is_some_and(|c| c == "Total"))
        .ok_or("the Start here scorecard has no Total row")?;
    let severities = ["critical", "high", "medium", "low"]
        .into_iter()
        .enumerate()
        .map(|(i, s)| (s, number(total_row, i + 1)))
        .collect();
    Ok((sheet_counts, severities))
}

/// 22d, over the archive: the CSV, the workbook's All problems rows, the Your conventions and
/// Not checked sheets' rows (by kind) and Start here's summaries agree with
/// `json/all-findings.json` by severity AND kind, not only in total.
/// Missing files are the dataset-set check's to report.
pub fn count_reconciliation_problems(entries: &Entries) -> Vec<Problem> {
    use crate::xlsx_export::vocabulary as v;
    let json = match json_tally(entries) {
        None => return Vec::new(),
        Some(Err(p)) => return vec![p],
        Some(Ok(tally)) => tally,
    };
    let mut problems = Vec::new();
    if let Some(csv) = entries.get("csv/all-findings.csv") {
        match csv_tally(csv) {
            Ok(tally) => problems.extend(tally_differences(
                "csv/all-findings.csv",
                "the CSV",
                &json,
                &tally,
            )),
            Err(e) => problems.push(problem("csv/all-findings.csv", COUNTS, e)),
        }
    }
    let Some(stem) = package_stem(entries) else {
        return problems;
    };
    let xlsx_name = format!("{stem}-findings.xlsx");
    let Some(xlsx) = entries.get(&xlsx_name) else {
        return problems;
    };
    let sheet_kinds = crate::xlsx_export::ALL_PROBLEMS_SHEET_KINDS;
    // All problems lists one row for each place (the row that heads the rows at that place), so the
    // reference is the JSON rows of those kinds collapsed to their places the same way.
    let json_for_sheet: Tally = places_tally(entries, &sheet_kinds);
    match workbook_rows_tally(xlsx) {
        Ok(tally) => problems.extend(tally_differences(
            &xlsx_name,
            "the All problems sheet",
            &json_for_sheet,
            &tally,
        )),
        Err(e) => problems.push(problem(&xlsx_name, COUNTS, e)),
    }
    // Your conventions and Not checked hold the other three kinds, grouped by sheet rather than
    // by severity, so their rows are tallied by kind against the same JSON rows. (Start here's
    // summaries below count from the report, so they cannot see a row dropped from a sheet.)
    let json_kinds = by_kind(&json);
    let mut listed_rows: Vec<(&str, String, usize)> = Vec::new();
    match conventions_sheet_tally(xlsx) {
        Ok(listed) => {
            for kind in ["convention_observed", "no_convention"] {
                let n = listed.get(kind).copied().unwrap_or(0);
                listed_rows.push((v::YOUR_CONVENTIONS, kind.to_string(), n));
            }
            if let Some(n) = listed.get("unrecognised status") {
                listed_rows.push((v::YOUR_CONVENTIONS, "unrecognised status".to_string(), *n));
            }
        }
        Err(e) => problems.push(problem(&xlsx_name, COUNTS, e)),
    }
    match not_checked_sheet_not_assessed(xlsx) {
        Ok(n) => listed_rows.push((v::NOT_CHECKED, "not_assessed".to_string(), n)),
        Err(e) => problems.push(problem(&xlsx_name, COUNTS, e)),
    }
    for (sheet, kind, listed) in listed_rows {
        let want = json_kinds.get(&kind).copied().unwrap_or(0);
        if listed != want {
            problems.push(problem(
                &xlsx_name,
                &format!("sheet \"{sheet}\" rows, kind {kind}"),
                format!("the sheet lists {listed} rows and json/all-findings.json has {want}"),
            ));
        }
    }
    match workbook_start_summary(xlsx) {
        Ok((sheet_counts, severities)) => {
            let want_problems: usize = json_for_sheet.values().sum();
            let want_conventions = ["convention_observed", "no_convention"]
                .iter()
                .map(|k| json_kinds.get(*k).copied().unwrap_or(0))
                .sum::<usize>();
            for (sheet, w) in [
                (v::ALL_PROBLEMS, want_problems),
                (v::YOUR_CONVENTIONS, want_conventions),
            ] {
                let g = sheet_counts.get(sheet).copied().unwrap_or(0);
                if w != g {
                    problems.push(problem(
                        &xlsx_name,
                        &format!("sheet \"Start here\" row count, sheet {sheet}"),
                        format!("Start here counts {g} and json/all-findings.json has {w}"),
                    ));
                }
            }
            for (severity, w) in by_severity_bucket(&json) {
                let g = severities.get(severity).copied().unwrap_or(0);
                if w != g {
                    problems.push(problem(
                        &xlsx_name,
                        &format!("sheet \"Start here\" scorecard totals, severity {severity}"),
                        format!("Start here counts {g} and json/all-findings.json has {w}"),
                    ));
                }
            }
        }
        Err(e) => problems.push(problem(&xlsx_name, COUNTS, e)),
    }
    problems
}

const PATTERNS: &str = "patterns across services";

/// The "Patterns across services" section is an observation, never a finding: it must change no
/// severity total and no finding count. Held here as structure, because a total that moved would
/// have to come from the section being counted somewhere:
///
/// * `findings.json` carries the section under its own key when the report has one, and no key
///   when it does not, and the workbook has the matching sheet or none;
/// * the section carries no severity anywhere;
/// * no pattern row sits inside the `findings` array, so every count read off that array (and
///   off `json/all-findings.json`, which the rest of this check holds it to) is findings only.
pub fn patterns_section_problems(entries: &Entries, report: &AuditReportJson) -> Vec<Problem> {
    let mut problems = Vec::new();
    let findings_json: Option<Value> = entries
        .get("findings.json")
        .and_then(|bytes| serde_json::from_slice(bytes).ok());
    if let Some(value) = &findings_json {
        let section = value.get("patterns_across_services");
        match (&report.patterns_across_services, section) {
            (Some(_), None) => problems.push(problem(
                "findings.json",
                PATTERNS,
                "the report has a patterns section and findings.json has no patterns_across_services key",
            )),
            (None, Some(_)) => problems.push(problem(
                "findings.json",
                PATTERNS,
                "findings.json has a patterns_across_services key and the report has no such section",
            )),
            (Some(want), Some(got)) => {
                let rows = got.get("rows").and_then(Value::as_array).map_or(0, Vec::len);
                if rows != want.rows.len() {
                    problems.push(problem(
                        "findings.json",
                        PATTERNS,
                        format!(
                            "findings.json lists {rows} pattern rows and the PDF's data has {}",
                            want.rows.len()
                        ),
                    ));
                }
                if got.to_string().contains("\"severity\"") {
                    problems.push(problem(
                        "findings.json",
                        PATTERNS,
                        "the patterns section carries a severity; it is an observation and has none",
                    ));
                }
            }
            (None, None) => {}
        }
        for (i, row) in value
            .get("findings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if row.get("cells").is_some() || row.get("statement").is_some() {
                problems.push(problem(
                    "findings.json",
                    &format!("findings[{i}]"),
                    "a pattern row is inside the findings array; it would be counted as a finding",
                ));
            }
        }
    }
    if let Some(stem) = package_stem(entries) {
        if let Some(xlsx) = entries.get(&format!("{stem}-findings.xlsx")) {
            if let Ok(names) = super::archive::xlsx_sheet_names(xlsx) {
                let has_sheet = names
                    .iter()
                    .any(|n| n == crate::xlsx_export::vocabulary::PATTERNS_ACROSS_SERVICES);
                if has_sheet != report.patterns_across_services.is_some() {
                    problems.push(problem(
                        &format!("{stem}-findings.xlsx"),
                        PATTERNS,
                        if has_sheet {
                            "the workbook has a Patterns across services sheet and the report has no such section"
                        } else {
                            "the report has a patterns section and the workbook has no Patterns across services sheet"
                        },
                    ));
                }
            }
        }
    }
    problems
}

/// 22d, the PDF side (the in-memory release, which holds the PDF's data): the rows the PDF
/// counts (all six matrix buckets) agree with `json/all-findings.json` by severity and kind, the
/// finding cards it renders (curated plus held for review) agree with those matrix rows by
/// severity and kind, and the cover's severity summary agrees by severity column.
pub fn pdf_count_reconciliation_problems(
    entries: &Entries,
    report: &AuditReportJson,
) -> Vec<Problem> {
    let mut pdf = Tally::new();
    for bucket in [
        &report.matrix.do_now,
        &report.matrix.do_next,
        &report.matrix.plan,
        &report.matrix.accepted,
        &report.matrix.informational,
        &report.matrix.held,
    ] {
        for f in bucket {
            *pdf.entry((f.severity.clone(), f.kind.tag().to_string()))
                .or_insert(0) += 1;
        }
    }
    // The reshaped PDF (above 30,000 sites) lists no row; it states the counts instead.
    for r in &report.reduced_matrix {
        *pdf.entry((r.severity.clone(), r.kind.clone())).or_insert(0) += r.count;
    }
    // What the PDF actually draws is its finding cards, "Curated findings" plus "Held for
    // review", not the matrix the totals above are taken from. Every matrix row must be drawn
    // exactly once, so a finding that falls out of the cards (or is drawn twice) fails here
    // even though the matrix, the cover and every total still agree.
    let mut rendered = Tally::new();
    if report.reduced {
        // No card is drawn by design, so the cards are the counts.
        rendered = pdf.clone();
    }
    for group in report
        .curated_findings
        .iter()
        .chain(report.held_for_review_findings.iter())
    {
        for site in &group.sites {
            *rendered
                .entry((site.severity.clone(), site.kind.tag().to_string()))
                .or_insert(0) += 1;
        }
    }
    let mut problems = tally_differences_against(
        "PDF data",
        "the PDF's rendered finding cards",
        "its own matrix",
        &pdf,
        &rendered,
    );
    problems.extend(patterns_section_problems(entries, report));
    let json = match json_tally(entries) {
        None => return problems,
        Some(Err(p)) => {
            problems.push(p);
            return problems;
        }
        Some(Ok(tally)) => tally,
    };
    problems.extend(tally_differences("PDF data", "the PDF", &json, &pdf));
    let stats = &report.cover.stats;
    for (severity, printed) in [
        ("critical", stats.critical),
        ("high", stats.high),
        ("medium", stats.medium),
        ("low", stats.low),
    ] {
        let w = by_severity_bucket(&json)
            .get(severity)
            .copied()
            .unwrap_or(0);
        if w != printed {
            problems.push(problem(
                "PDF data",
                &format!("cover.stats.{severity}"),
                format!("the PDF summary counts {printed} and json/all-findings.json has {w}"),
            ));
        }
    }
    problems
}

// ---- confidentiality marking (2026-10-24 decision) -------------------------------------------

const MARKING: &str = "confidentiality marking";

/// The prefix every confidentiality line starts with when it names a client (see
/// `report_export::confidentiality_marking`).
pub const MARKING_PREFIX: &str = "Confidential, prepared for ";

/// The workbook's member name, and each sheet's name with its footer's left section (`None`
/// when the sheet has no footer).
type WorkbookFooters = (String, Vec<(String, Option<String>)>);

/// The workbook's sheets and their decoded footers, or the problem that stops the check. `None`
/// when there is no workbook to check (the dataset-set check reports that).
fn workbook_footers(entries: &Entries) -> Option<Result<WorkbookFooters, Problem>> {
    let stem = package_stem(entries)?;
    let xlsx_name = format!("{stem}-findings.xlsx");
    let xlsx = entries.get(&xlsx_name)?;
    let (Ok(sheets), Ok(parts)) = (
        super::archive::xlsx_sheet_names(xlsx),
        super::archive::xlsx_header_footers(xlsx),
    ) else {
        return Some(Err(problem(
            &xlsx_name,
            MARKING,
            "the workbook's sheet footers could not be read",
        )));
    };
    let footers = sheets
        .into_iter()
        .map(|sheet| {
            let left = parts
                .iter()
                .find(|p| p.part == "footer" && p.sheet == sheet)
                .map(|p| p.left.clone());
            (sheet, left)
        })
        .collect();
    Some(Ok((xlsx_name, footers)))
}

/// Every worksheet of the package's workbook carries a footer whose left section is a
/// confidentiality line naming the client, and every sheet prints the same line.
pub fn marking_problems(entries: &Entries) -> Vec<Problem> {
    let (xlsx_name, footers) = match workbook_footers(entries) {
        None => return Vec::new(),
        Some(Err(p)) => return vec![p],
        Some(Ok(found)) => found,
    };
    let mut problems = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (sheet, left) in &footers {
        let location = format!("sheet {sheet:?} footer");
        match left {
            None => problems.push(problem(
                &xlsx_name,
                &location,
                "the sheet has no confidentiality footer",
            )),
            Some(line) if !line.starts_with(MARKING_PREFIX) => problems.push(problem(
                &xlsx_name,
                &location,
                "the sheet footer is not a confidentiality line naming the client",
            )),
            Some(line) => {
                seen.insert(line);
            }
        }
    }
    if seen.len() > 1 {
        problems.push(problem(
            &xlsx_name,
            MARKING,
            "the sheets print different confidentiality lines",
        ));
    }
    problems
}

/// The PDF's confidentiality line (from its data) names the client, and is the line the
/// workbook's sheets print. Run by the in-memory release, which holds the PDF's data; the
/// sheets' own problems are [`marking_problems`]'s to report.
pub fn pdf_marking_problems(entries: &Entries, pdf_line: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    if !pdf_line.starts_with(MARKING_PREFIX) {
        problems.push(problem(
            "PDF data",
            "confidentiality.line",
            "the PDF's confidentiality line does not name a client",
        ));
    }
    if let Some(Ok((xlsx_name, footers))) = workbook_footers(entries) {
        for (sheet, left) in &footers {
            if left.as_deref().is_some_and(|line| line != pdf_line) {
                problems.push(problem(
                    &xlsx_name,
                    &format!("sheet {sheet:?} footer"),
                    "the sheet footer's confidentiality line differs from the PDF's",
                ));
            }
        }
    }
    problems
}

// ---- manifest (gate 6) --------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every `files[]` entry's claimed sha256 against the archive member's actual hash; a listed
/// file that is absent; a member the manifest does not list. The package verifier a recipient
/// would run.
pub fn verify_manifest(entries: &Entries) -> Vec<Problem> {
    let Some(manifest_bytes) = entries.get("manifest.json") else {
        return vec![problem(
            "manifest.json",
            "manifest",
            "the package has no manifest.json",
        )];
    };
    let Ok(manifest) = serde_json::from_slice::<Value>(manifest_bytes) else {
        return vec![problem(
            "manifest.json",
            "manifest",
            "manifest.json does not parse",
        )];
    };
    let mut problems = Vec::new();
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for file in manifest["files"].as_array().into_iter().flatten() {
        let path = file["path"].as_str().unwrap_or_default().to_string();
        let claimed = file["sha256"].as_str().unwrap_or_default();
        match entries.get(&path) {
            Some(bytes) if sha256_hex(bytes) == claimed => {}
            Some(_) => problems.push(problem(
                &path,
                "sha256",
                "the file's hash does not match the manifest",
            )),
            None => problems.push(problem(
                &path,
                "manifest",
                "the manifest lists a file the package does not contain",
            )),
        }
        listed.insert(path);
    }
    for name in entries.keys() {
        if name != "manifest.json" && !listed.contains(name) {
            problems.push(problem(
                name,
                "manifest",
                "the manifest does not list this file",
            ));
        }
    }
    problems
}

// ---- provenance (gate 7) --------------------------------------------------------------------------

/// What the README prints for each provenance line, parsed back out of the text by label, so a
/// README that omits, renames or mangles a line fails the comparison.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReadmeProvenance {
    pub repos: Option<String>,
    pub generated: Option<String>,
    /// repo name to (commit, "uncommitted changes" note present), from `Commit (repo): sha`.
    pub commits: BTreeMap<String, (String, bool)>,
    pub engine_version: Option<String>,
    pub corpus_version: Option<String>,
    pub rules_last_reviewed: Option<String>,
    pub model_backend: Option<String>,
    pub model_provider: Option<String>,
    /// `(role, model)` pairs, in README order, from `Model (role): model`.
    pub models: Vec<(String, String)>,
    /// `Client engagement: yes|no`; `None` when the line is absent.
    pub client_engagement: Option<bool>,
    /// The checks named by the `Release checks:` line, when it says they all passed.
    pub release_checks: Option<Vec<String>>,
    /// The spend-limit line is present (the inspection was stopped by the operator's limit).
    pub cost_cap_reached: bool,
    /// The spend-limit line is the "could not be enforced" variant, not the "reached" one.
    pub cost_cap_unenforced: bool,
    /// Every `Vulnerability data (<feed>) ...` stamp line, verbatim and in README order.
    pub vulnerability_data: Vec<String>,
    /// 22c: `Prepared for: <client>`.
    pub client_name: Option<String>,
    /// 22c: `Corpus rules applied: N`.
    pub corpus_rules_applied: Option<u64>,
    /// 22c: `Counted lines: N`.
    pub counted_lines: Option<u64>,
    /// 22c: `Excluded as noise: F files[, L lines]`, as (files, lines).
    pub excluded_noise: Option<(u64, Option<u64>)>,
}

/// `"F files, L lines"` or `"F files"` to (F, L).
fn parse_excluded_noise(v: &str) -> Option<(u64, Option<u64>)> {
    let (files, rest) = v.split_once(" files")?;
    let files = files.parse().ok()?;
    match rest.strip_prefix(", ") {
        None if rest.is_empty() => Some((files, None)),
        Some(lines) => Some((files, Some(lines.strip_suffix(" lines")?.parse().ok()?))),
        None => None,
    }
}

pub fn parse_readme_provenance(readme: &str) -> ReadmeProvenance {
    let mut p = ReadmeProvenance::default();
    for line in readme.lines() {
        if let Some(v) = line.strip_prefix("Prepared for: ") {
            p.client_name = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Corpus rules applied: ") {
            p.corpus_rules_applied = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("Counted lines: ") {
            p.counted_lines = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("Excluded as noise: ") {
            p.excluded_noise = parse_excluded_noise(v);
        } else if let Some(v) = line.strip_prefix("Repos inspected: ") {
            p.repos = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Generated: ") {
            p.generated = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Commit (") {
            if let Some((repo, rest)) = v.split_once("): ") {
                let note = " (the working tree had uncommitted changes)";
                let (sha, dirty) = match rest.strip_suffix(note) {
                    Some(sha) => (sha, true),
                    None => (rest, false),
                };
                p.commits.insert(repo.to_string(), (sha.to_string(), dirty));
            }
        } else if let Some(v) = line.strip_prefix("Engine version: ") {
            p.engine_version = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Corpus version: ") {
            p.corpus_version = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Rules last reviewed on ") {
            p.rules_last_reviewed = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Model backend: ") {
            p.model_backend = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Model provider: ") {
            p.model_provider = Some(v.to_string());
        } else if line.starts_with(crate::vuln_intel::README_PREFIX) {
            p.vulnerability_data.push(line.to_string());
        } else if let Some(v) = line.strip_prefix("Model (") {
            if let Some((role, model)) = v.split_once("): ") {
                p.models.push((role.to_string(), model.to_string()));
            }
        } else if let Some(v) = line.strip_prefix("Client engagement: ") {
            p.client_engagement = match v {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            };
        } else if line == crate::COST_CAP_README_LINE {
            p.cost_cap_reached = true;
        } else if line == crate::COST_CAP_UNENFORCED_README_LINE {
            p.cost_cap_reached = true;
            p.cost_cap_unenforced = true;
        } else if let Some(v) = line.strip_prefix("Release checks: ") {
            p.release_checks = v
                .strip_suffix(" (all passed)")
                .map(|names| names.split(", ").map(str::to_string).collect());
        }
    }
    p
}

/// The README lines of a provenance block, by their opening words. A client README names none of
/// them: it says what each file is and how to import the tickets, and nothing about how the
/// inspection was run.
const PROVENANCE_LINE_OPENINGS: [&str; 16] = [
    "Prepared for: ",
    "Repos inspected: ",
    "Generated: ",
    "Commit (",
    "Engine version: ",
    "Corpus version: ",
    "Corpus rules applied: ",
    "Rules last reviewed on ",
    "Counted lines: ",
    "Excluded as noise: ",
    "Model backend: ",
    "Model provider: ",
    "Model (",
    "Client engagement: ",
    "Release checks: ",
    "Spend limit: ",
];

/// A CLIENT package's README carries no provenance line, no vulnerability-data stamp and no
/// engine or corpus identity, whatever it says elsewhere.
pub fn client_readme_problems(readme: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (i, line) in readme.lines().enumerate() {
        let opening = PROVENANCE_LINE_OPENINGS
            .iter()
            .find(|opening| line.starts_with(**opening))
            .copied()
            .or_else(|| {
                line.starts_with(crate::vuln_intel::README_PREFIX)
                    .then_some("Vulnerability data")
            });
        if let Some(opening) = opening {
            problems.push(problem(
                "README.txt",
                &format!("line {}", i + 1),
                format!(
                    "a client README carries no provenance line, and this one opens with \
                     \"{}\": the models, the corpus, the engine commit and the release checks \
                     are in manifest.json",
                    opening.trim_end()
                ),
            ));
        }
    }
    problems
}

fn manifest_str(manifest: &Value, key: &str) -> Option<String> {
    manifest["inspection"][key].as_str().map(str::to_string)
}

/// Each README provenance line equals the manifest's `inspection` block field by field, an
/// absent value is `null` in the manifest and omitted from the README, and every non-null value
/// the manifest carries appears in the README (found generically, so a field added to one and
/// not the other fails without this function being edited).
pub fn verify_readme_provenance(readme: &str, manifest: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut mismatch = |what: &str| {
        problems.push(problem(
            "README.txt",
            what,
            "the README's provenance line disagrees with manifest.json",
        ));
    };
    let parsed = parse_readme_provenance(readme);

    let repos = manifest["inspection"]["repos"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let names: Vec<String> = repos
        .iter()
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    if parsed.repos != Some(names.join(", ")) {
        mismatch("Repos inspected");
    }
    if parsed.generated != manifest["generated_at"].as_str().map(str::to_string) {
        mismatch("Generated");
    }
    let manifest_commits: BTreeMap<String, (String, bool)> = repos
        .iter()
        .filter_map(|r| {
            Some((
                r["name"].as_str()?.to_string(),
                (r["commit"].as_str()?.to_string(), r["dirty"].as_bool()?),
            ))
        })
        .collect();
    if parsed.commits != manifest_commits {
        mismatch("Commit");
    }
    if parsed.engine_version != manifest_str(manifest, "engine_version") {
        mismatch("Engine version");
    }
    if parsed.corpus_version != manifest_str(manifest, "corpus_version") {
        mismatch("Corpus version");
    }
    if parsed.rules_last_reviewed != manifest_str(manifest, "rules_last_reviewed") {
        mismatch("Rules last reviewed");
    }
    if parsed.model_backend != manifest_str(manifest, "model_backend") {
        mismatch("Model backend");
    }
    if parsed.model_provider != manifest_str(manifest, "model_provider") {
        mismatch("Model provider");
    }
    if parsed.client_name != manifest_str(manifest, "client_name") {
        mismatch("Prepared for");
    }
    let manifest_u64 = |key: &str| manifest["inspection"][key].as_u64();
    if parsed.corpus_rules_applied != manifest_u64("corpus_rules_applied") {
        mismatch("Corpus rules applied");
    }
    if parsed.counted_lines != manifest_u64("counted_lines") {
        mismatch("Counted lines");
    }
    let manifest_noise = manifest_u64("excluded_noise_files")
        .map(|files| (files, manifest_u64("excluded_noise_lines")));
    if parsed.excluded_noise != manifest_noise {
        mismatch("Excluded as noise");
    }
    let manifest_models: Vec<(String, String)> = manifest["inspection"]["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some((
                        m["role"].as_str()?.to_string(),
                        m["model"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if parsed.models != manifest_models {
        mismatch("Model");
    }
    if parsed.client_engagement != manifest["inspection"]["client_engagement"].as_bool() {
        mismatch("Client engagement");
    }
    let manifest_checks: Option<Vec<String>> = manifest["inspection"]["release_checks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        });
    if parsed.release_checks != manifest_checks {
        mismatch("Release checks");
    }
    // The flag is always in the manifest (a missing key reads as not capped, never as an error:
    // a package built before the flag existed is complete by definition).
    if parsed.cost_cap_reached
        != manifest["inspection"]["cost_cap_reached"]
            .as_bool()
            .unwrap_or(false)
    {
        mismatch("Spend limit");
    }
    // Which line it is: "reached" or "could not be enforced". A package built before the
    // variant existed has no key, which reads as "reached" (the only line it could print).
    if parsed.cost_cap_unenforced
        != manifest["inspection"]["cost_cap_unenforced"]
            .as_bool()
            .unwrap_or(false)
    {
        mismatch("Spend limit (cause)");
    }

    // The vulnerability-data stamp: the README's stamp lines are exactly the manifest's
    // `inspection.vulnerability_data.<feed>.stamp` values, in order. A package built before the
    // stamp existed has neither, which agrees.
    let manifest_stamps: Vec<String> = manifest["inspection"]["vulnerability_data"]
        .as_object()
        .map(|feeds| {
            feeds
                .values()
                .filter_map(|feed| feed["stamp"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if parsed.vulnerability_data != manifest_stamps {
        mismatch("Vulnerability data");
    }

    // Generic: every non-null value in the manifest's inspection block is in the README.
    if let Some(inspection) = manifest["inspection"].as_object() {
        for (key, value) in inspection {
            let rendered: Vec<String> = match value {
                Value::String(s) => vec![s.clone()],
                Value::Array(items) => items
                    .iter()
                    .flat_map(|item| match item {
                        Value::String(s) => vec![s.clone()],
                        Value::Object(repo) => ["name", "commit"]
                            .into_iter()
                            .filter_map(|k| repo.get(k).and_then(Value::as_str).map(str::to_string))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for text in rendered {
                if !readme.contains(&text) {
                    problems.push(problem(
                        "README.txt",
                        &format!("inspection.{key}"),
                        "a value in manifest.json is not in the README",
                    ));
                }
            }
        }
    }
    problems
}

// ---- one fix, one row (item #1) -------------------------------------------------------------

const FIX_PARITY: &str = "one fix per row";

/// A Fix is one change, so the package must hold it once everywhere. Over the archive:
///
/// * every row of `findings.json` that says something is wrong has a Fix id, and every Fix id
///   names a Fix of its `fixes` list;
/// * every member of every Fix is a rule group of the package's own tree;
/// * the tickets and the Fix plan sheet list the same Fixes, each once, and each is a Fix of the
///   `fixes` list: one ticket for each plan row, one plan row for each Fix with a place to fix.
pub fn fix_archive_problems(entries: &Entries) -> Vec<Problem> {
    use crate::xlsx_export::vocabulary as v;
    let mut problems = Vec::new();
    let Some(findings) = entries
        .get("findings.json")
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
    else {
        return problems;
    };
    let fixes = findings.get("fixes").and_then(Value::as_array);
    // A package written before Fixes existed has none to check.
    let Some(fixes) = fixes else {
        return problems;
    };
    let fix_ids: BTreeSet<String> = fixes
        .iter()
        .filter_map(|f| f.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let groups: BTreeSet<String> = findings
        .get("tree")
        .and_then(|t| t.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for fix in fixes {
        let id = fix.get("id").and_then(Value::as_str).unwrap_or("");
        let members = fix
            .get("members")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if members.is_empty() {
            problems.push(problem(
                "findings.json",
                &format!("fixes, Fix {id}"),
                "a Fix has no rule group",
            ));
        }
        for member in members.iter().filter_map(Value::as_str) {
            if !groups.is_empty() && !groups.contains(member) {
                problems.push(problem(
                    "findings.json",
                    &format!("fixes, Fix {id}"),
                    format!("the Fix names the rule group {member}, which is not in the tree"),
                ));
            }
        }
    }
    let rows = findings
        .get("findings")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // A report with no review tree has no Fixes and says so elsewhere; its rows are not held to one.
    for (i, row) in rows.iter().enumerate().filter(|_| !fixes.is_empty()) {
        let kind = cell_text(row, "kind");
        if !crate::xlsx_export::ALL_PROBLEMS_SHEET_KINDS.contains(&kind.as_str()) {
            continue;
        }
        let fix_id = cell_text(row, "fix_id");
        if fix_id.is_empty() && !cell_text(row, "group").is_empty() {
            problems.push(problem(
                "findings.json",
                &format!("findings[{i}]"),
                "a row in a rule group has no Fix id",
            ));
        } else if !fix_id.is_empty() && !fix_ids.contains(&fix_id) {
            problems.push(problem(
                "findings.json",
                &format!("findings[{i}]"),
                format!("the row names the Fix {fix_id}, which is not in the fixes list"),
            ));
        }
    }

    // Tickets and plan rows.
    let tickets = entries
        .get(crate::xlsx_export::tickets::TICKETS_CSV)
        .map(|b| ticket_fix_ids(b));
    let plan = package_stem(entries).and_then(|stem| {
        let xlsx = entries.get(&format!("{stem}-findings.xlsx"))?;
        Some(plan_fix_ids(xlsx))
    });
    let (tickets, plan) = match (tickets, plan) {
        (Some(t), Some(p)) => (t, p),
        _ => return problems,
    };
    let (tickets, plan) = match (tickets, plan) {
        (Ok(t), Ok(p)) => (t, p),
        (Err(e), _) => {
            problems.push(problem(
                crate::xlsx_export::tickets::TICKETS_CSV,
                FIX_PARITY,
                e,
            ));
            return problems;
        }
        (_, Err(e)) => {
            problems.push(problem(
                "(workbook)",
                &format!("sheet \"{}\"", v::FIX_PLAN),
                e,
            ));
            return problems;
        }
    };
    for (what, ids) in [("a ticket", &tickets), ("a Fix plan row", &plan)] {
        let unique: BTreeSet<&String> = ids.iter().collect();
        if unique.len() != ids.len() {
            problems.push(problem(
                "(package)",
                FIX_PARITY,
                format!("{what} names the same Fix twice"),
            ));
        }
        for id in ids {
            if !id.is_empty() && !fix_ids.contains(id) {
                problems.push(problem(
                    "(package)",
                    FIX_PARITY,
                    format!("{what} names the Fix {id}, which is not in findings.json"),
                ));
            }
        }
    }
    let t: BTreeSet<&String> = tickets.iter().collect();
    let p: BTreeSet<&String> = plan.iter().collect();
    if t != p {
        problems.push(problem(
            "(package)",
            FIX_PARITY,
            format!(
                "the tickets list {} Fixes and the {} sheet lists {}; they must be the same Fixes",
                tickets.len(),
                v::FIX_PLAN,
                plan.len()
            ),
        ));
    }
    problems
}

/// The Fix id of every ticket of `tickets.csv`.
fn ticket_fix_ids(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut reader = csv::ReaderBuilder::new().from_reader(bytes);
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    let col = headers
        .iter()
        .position(|h| h == crate::xlsx_export::vocabulary::H_FIX_ID)
        .ok_or_else(|| "the tickets file has no Fix id column".to_string())?;
    let mut out = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| e.to_string())?;
        out.push(record.get(col).unwrap_or("").to_string());
    }
    Ok(out)
}

/// The Fix id of every row of the workbook's Fix plan sheet.
fn plan_fix_ids(xlsx: &[u8]) -> Result<Vec<String>, String> {
    use crate::xlsx_export::vocabulary as v;
    let grid = super::archive::xlsx_sheet_grid(xlsx, v::FIX_PLAN)?;
    let header = grid
        .first()
        .ok_or_else(|| format!("the {} sheet is empty", v::FIX_PLAN))?;
    let col = header
        .iter()
        .position(|h| h == v::H_FIX_ID)
        .ok_or_else(|| format!("the {} sheet has no {:?} column", v::FIX_PLAN, v::H_FIX_ID))?;
    Ok(grid
        .iter()
        .skip(1)
        .filter_map(|row| row.get(col))
        .filter(|id| !id.is_empty())
        .cloned()
        .collect())
}

/// Over the rows in memory: the PDF's escalation list has one entry for each place that holds an
/// escalated row (an escalation slot) and none twice, and the top three are three distinct Fixes
/// that are Fixes of `findings.json`.
pub fn fix_report_problems(report: &AuditReportJson, findings: &FindingsExport) -> Vec<Problem> {
    let mut problems = Vec::new();
    let slots: BTreeSet<(String, String, String)> = findings
        .findings
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .filter(|r| cell_text(r, "escalated") == "true")
        .map(|r| {
            (
                cell_text(&r, "repo"),
                cell_text(&r, "path"),
                cell_text(&r, "line"),
            )
        })
        .collect();
    let entries: Vec<(String, String, String)> = report
        .escalations
        .items
        .iter()
        .map(|i| (i.repo.clone(), i.path.clone(), i.line.to_string()))
        .collect();
    let listed: BTreeSet<&(String, String, String)> = entries.iter().collect();
    if listed.len() != entries.len() {
        problems.push(problem(
            "PDF data",
            "escalations",
            "the escalation list names one place more than once",
        ));
    }
    if entries.len() != slots.len() {
        problems.push(problem(
            "PDF data",
            "escalations",
            format!(
                "the escalation list has {} entries and findings.json has {} escalated places",
                entries.len(),
                slots.len()
            ),
        ));
    }
    let fix_ids: BTreeSet<&str> = findings.fixes.iter().map(|f| f.id.as_str()).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for item in &report.three_things.items {
        if item.fix_id.is_empty() {
            continue;
        }
        if !seen.insert(item.fix_id.as_str()) {
            problems.push(problem(
                "PDF data",
                "three_things",
                format!("the top three names the Fix {} twice", item.fix_id),
            ));
        }
        if !fix_ids.contains(item.fix_id.as_str()) {
            problems.push(problem(
                "PDF data",
                "three_things",
                format!(
                    "the top three names the Fix {}, which is not a Fix",
                    item.fix_id
                ),
            ));
        }
    }
    problems
}
