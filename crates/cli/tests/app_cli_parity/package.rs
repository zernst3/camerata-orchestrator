//! Reading an export package into what the comparison looks at, and the normalization of the
//! values two runs of the same inputs legitimately differ in.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

// ── Reading a package ────────────────────────────────────────────────────────────────────

/// Every entry of a zip, by name.
pub(crate) fn unzip(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if entry.is_dir() {
            continue;
        }
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        out.insert(entry.name().unwrap().to_string(), body);
    }
    out
}

/// What a run sealed, as the comparison reads it from its package.
#[derive(Debug, PartialEq)]
pub(crate) struct Sealed {
    pub(crate) files: Vec<String>,
    /// `(rule id, repo, file, line, severity, kind)` for every finding, sorted.
    pub(crate) findings: Vec<(String, String, String, u64, String, String)>,
    /// The Coverage section with its lists taken out (they are compared on their own).
    pub(crate) coverage_counts: Value,
    /// `(repo, file, reason)` for every file not read, sorted.
    pub(crate) not_read: Vec<(String, String, String)>,
    /// `(repo, file)` for every file excluded by agreement, sorted.
    pub(crate) excluded: Vec<(String, String)>,
    /// Every JSON entry of the package, normalized.
    pub(crate) json: BTreeMap<String, Value>,
    /// Every other text entry (the README and the CSV files), normalized.
    pub(crate) text: BTreeMap<String, String>,
    /// Every entry inside the workbook, normalized.
    pub(crate) workbook: BTreeMap<String, String>,
    /// The PDF's bytes, and the minute the package says it was built in.
    pub(crate) pdf: Vec<u8>,
    pub(crate) generated_at: String,
}

pub(crate) fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn sealed(zip: &[u8], root: &Path) -> Sealed {
    let entries = unzip(zip);
    let files: Vec<String> = entries.keys().cloned().collect();
    let mut json = BTreeMap::new();
    let mut text_files = BTreeMap::new();
    let mut workbook = BTreeMap::new();
    let mut pdf = Vec::new();
    for (name, body) in &entries {
        if name.ends_with(".json") {
            let v: Value =
                serde_json::from_slice(body).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
            json.insert(name.clone(), normalize_json(v, root));
        } else if name.ends_with(".xlsx") {
            for (inner, body) in unzip(body) {
                let body = String::from_utf8_lossy(&body).into_owned();
                workbook.insert(inner.clone(), normalize_workbook_entry(&inner, &body, root));
            }
        } else if name.ends_with(".pdf") {
            pdf.clone_from(body);
        } else {
            let body = String::from_utf8_lossy(body).into_owned();
            text_files.insert(name.clone(), normalize_text(&body, root));
        }
    }
    // Read before normalizing: the minute the package was built in.
    let generated_at = serde_json::from_slice::<Value>(&entries["manifest.json"]).unwrap()
        ["generated_at"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let inspection = json
        .get("json/inspection.json")
        .expect("the package holds json/inspection.json")
        .clone();
    let mut findings: Vec<_> = inspection["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            (
                text(&f["rule_id"]),
                text(&f["repo"]),
                text(&f["path"]),
                f["line"].as_u64().unwrap_or(0),
                text(&f["severity"]),
                text(&f["kind"]),
            )
        })
        .collect();
    findings.sort();
    let coverage = inspection["coverage"].clone();
    let mut not_read: Vec<_> = coverage["skipped_files"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| (text(&s["repo"]), text(&s["path"]), text(&s["reason"])))
        .collect();
    not_read.sort();
    let mut excluded: Vec<_> = not_read
        .iter()
        .filter(|(_, _, reason)| reason == "excluded by agreement")
        .map(|(repo, path, _)| (repo.clone(), path.clone()))
        .collect();
    excluded.sort();
    let mut coverage_counts = coverage;
    if let Some(map) = coverage_counts.as_object_mut() {
        map.remove("skipped_files");
    }
    Sealed {
        files,
        findings,
        coverage_counts,
        not_read,
        excluded,
        json,
        text: text_files,
        workbook,
        pdf,
        generated_at,
    }
}

// ── Normalization: only what two runs of the same inputs legitimately differ in ─────────────
//
// Each package is built by its own run, seconds apart, and stamps the wall clock in several
// places; nothing else in it may differ. Run ids, operator names and absolute paths are not in the
// package today (the operator name is the same built-in or environment value on both paths, so it
// is compared as it is). The test's temp folder is replaced in every string all the same, so an
// absolute path that only one of the two writes still shows as a difference.

pub(crate) const STAMP: &str = "<wall clock>";
/// The temp folder both runs live in, wherever it is on this machine.
pub(crate) const ROOT: &str = "<root>";

/// The files whose bytes embed the build time, so whose manifest hash differs between two builds.
/// Their content is compared on its own, normalized (the PDF only when both were built in the same
/// minute, since its compressed pages print the minute).
pub(crate) fn embeds_build_time(path: &str) -> bool {
    path == "README.txt"
        || path == "json/inspection.json"
        || path.ends_with(".pdf")
        || path.ends_with(".xlsx")
}

pub(crate) fn without_root(s: &str, root: &Path) -> String {
    s.replace(&*root.to_string_lossy(), ROOT)
}

/// JSON: `generated_at` (the minute the package was built, in `manifest.json` and
/// `json/inspection.json`), the manifest's hashes of the files that print that minute, and the
/// test's temp folder in any string.
pub(crate) fn normalize_json(v: Value, root: &Path) -> Value {
    fn walk(v: Value, root: &Path) -> Value {
        match v {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                let embeds = map
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(embeds_build_time);
                for (k, v) in map {
                    let v = if k == "generated_at" || (k == "sha256" && embeds) {
                        Value::String(STAMP.to_string())
                    } else {
                        walk(v, root)
                    };
                    out.insert(k, v);
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.into_iter().map(|v| walk(v, root)).collect()),
            Value::String(s) => Value::String(without_root(&s, root)),
            other => other,
        }
    }
    walk(v, root)
}

/// The README's `Generated:` line (the build minute) and the test's temp folder.
pub(crate) fn normalize_text(body: &str, root: &Path) -> String {
    let lines: Vec<String> = body
        .lines()
        .map(|l| {
            if l.starts_with("Generated: ") {
                format!("Generated: {STAMP}")
            } else {
                without_root(l, root)
            }
        })
        .collect();
    lines.join("\n")
}

/// The workbook's creation and modification times (`docProps/core.xml`, to the second) and its
/// "generated at" cell (the build minute, a shared string), and the test's temp folder.
pub(crate) fn normalize_workbook_entry(name: &str, body: &str, root: &Path) -> String {
    let body = without_root(body, root);
    match name {
        "docProps/core.xml" => regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
            .unwrap()
            .replace_all(&body, STAMP)
            .into_owned(),
        "xl/sharedStrings.xml" => regex::Regex::new(r"<t>\d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC</t>")
            .unwrap()
            .replace_all(&body, format!("<t>{STAMP}</t>").as_str())
            .into_owned(),
        _ => body,
    }
}
