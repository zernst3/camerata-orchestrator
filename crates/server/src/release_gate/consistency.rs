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
    for fixed in ["findings.json", "README.txt", "manifest.json"] {
        expected.insert(fixed.to_string());
    }
    for dataset in DATASETS {
        expected.insert(format!("csv/{dataset}.csv"));
        expected.insert(format!("json/{dataset}.json"));
    }
    for name in &expected {
        if !entries.contains_key(name) {
            problems.push(problem(name, DATASET_SET, "a promised file is missing"));
        }
    }
    for name in entries.keys() {
        if !expected.contains(name) {
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
    for fixed in ["findings.json", "README.txt", "manifest.json"] {
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
    problems
}

// ---- row parity (req_d1) ----------------------------------------------------------------------

type RowId = (String, String, String, usize);

/// The identity set `(rule_id, repo, path, line)` of every row the PDF renders (all six matrix
/// buckets) must equal `findings.json`'s. A row in one and not the other is named by its rule
/// and location (never its text).
pub fn row_parity(report: &AuditReportJson, findings: &FindingsExport) -> Vec<Problem> {
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
    /// The checks named by the `Release checks:` line, when it says they all passed.
    pub release_checks: Option<Vec<String>>,
    /// The spend-limit line is present (the inspection was stopped by the operator's limit).
    pub cost_cap_reached: bool,
}

pub fn parse_readme_provenance(readme: &str) -> ReadmeProvenance {
    let mut p = ReadmeProvenance::default();
    for line in readme.lines() {
        if let Some(v) = line.strip_prefix("Repos inspected: ") {
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
        } else if line == crate::COST_CAP_README_LINE {
            p.cost_cap_reached = true;
        } else if let Some(v) = line.strip_prefix("Release checks: ") {
            p.release_checks = v
                .strip_suffix(" (all passed)")
                .map(|names| names.split(", ").map(str::to_string).collect());
        }
    }
    p
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
