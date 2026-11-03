#![allow(clippy::unwrap_used)]
//! Design amendment A11: every producer emits one row per site in a fresh scan.
//!
//! Before grouping stopped absorbing, a producer that found one thing in several places wrote one
//! row and put the other places in `also_locations`. Those places were exported but were not rows,
//! so a reviewer could not decide them and the Engine Check could not sample them. `also_locations`
//! now exists only so a report stored by an older engine can be loaded (the loader unfolds it into
//! rows). No producer may write it.
//!
//! Two checks hold that line. The first runs the two producers that used to write it (the clone
//! advisory and the runtime audit) and asserts that every site is a row of its own. The second
//! reads the producers' source, so a producer added later that writes `also_locations` fails here
//! whether or not this file knows about it.
//!
//! No network and no model: the runtime feeds are recorded fixtures and the clone judge is a
//! scripted stub.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use camerata_checks::clone_rules::{candidates, CloneRule};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::clone_advisory::prompt::BATCH_MARKER;
use camerata_server::onboard::clone_advisory::{
    judge_clone_candidates, JudgeRequest, VerdictCache,
};
use camerata_server::onboard::Finding;
use camerata_server::runtime_audit::{audit_runtimes, NvdPolicy, EOL_RULE_ID, VULN_RULE_ID};
use camerata_server::vuln_intel::FeedFetcher;
use chrono::{TimeZone, Utc};

const EOL_NODE: &str = include_str!("fixtures/runtime_audit/endoflife_nodejs.json");
const NVD_NODE: &str = include_str!("fixtures/runtime_audit/nvd_nodejs_18.17.0.json");

// ---- the clone advisory ---------------------------------------------------------------------

/// A judge that confirms every candidate it is asked about.
struct ConfirmsEverything {
    asked: Mutex<usize>,
}

#[async_trait]
impl LlmPort for ConfirmsEverything {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        *self.asked.lock().unwrap() += 1;
        let ids: Vec<String> = req
            .prompt
            .split_once(BATCH_MARKER)
            .map(|(_, rest)| rest)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with('{'))
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|i| i["id"].as_str().map(str::to_string))
            .collect();
        let items: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "verdict": "same_decision",
                    "concept": "the premium discount calculation",
                })
            })
            .collect();
        Ok(LlmResponse {
            text: serde_json::json!({ "verdicts": items }).to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let response = self.complete(req).await?;
        on_delta(&response.text);
        Ok(response)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn discount(func: &str, rate: &str) -> String {
    format!(
        "import {{ money }} from './money';
export function {func}(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = base * {rate};
    return base - discount;
  }}
  if (base > 100 && order.coupon !== null) {{
    return base - 10;
  }}
  return base + order.shipping;
}}
"
    )
}

/// The clone advisory's rows for one decision written in four files.
async fn clone_rows() -> Vec<Finding> {
    let files: Vec<(String, String)> = [
        ("src/cart/total.ts", "applyDiscount", "0.2"),
        ("src/checkout/summary.ts", "quoteTotal", "0.15"),
        ("src/orders/price.ts", "priceOrder", "0.2"),
        ("src/invoices/amount.ts", "invoiceAmount", "0.1"),
    ]
    .iter()
    .map(|(p, f, r)| ((*p).to_string(), discount(f, r)))
    .collect();
    let armed = std::collections::HashSet::from([CloneRule::SingleSourceLogic.id()]);
    let set = candidates(&files, &armed);
    let judge = ConfirmsEverything {
        asked: Mutex::new(0),
    };
    let outcome = judge_clone_candidates(
        Some(&judge),
        JudgeRequest {
            repo: "acme/shop",
            files: &files,
            set,
            judge_model: "stub-judge-model",
            corpus: None,
            unavailable_reason: "deterministic-only scan",
        },
        &mut VerdictCache::default(),
        None,
        None,
    )
    .await;
    assert!(*judge.asked.lock().unwrap() > 0, "the judge was asked");
    outcome.findings
}

// ---- the runtime audit ----------------------------------------------------------------------

struct Feeds;

#[async_trait]
impl FeedFetcher for Feeds {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        if url.starts_with("https://endoflife.date/api/nodejs") {
            Ok(EOL_NODE.to_string())
        } else if url.contains("services.nvd.nist.gov") && url.contains("node.js:18.17.0") {
            Ok(NVD_NODE.to_string())
        } else {
            anyhow::bail!("no recorded answer for {url}")
        }
    }
}

/// A repository that declares Node 18.17.0 (past end of life, with known vulnerabilities) in
/// three files.
fn three_file_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("Dockerfile", "FROM node:18.17.0-alpine\n"),
        (".nvmrc", "18.17.0\n"),
        (
            "package.json",
            "{\n  \"engines\": {\n    \"node\": \"18.17.0\"\n  }\n}\n",
        ),
    ];
    for (path, body) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    dir
}

async fn runtime_rows() -> Vec<Finding> {
    let dir = three_file_repo();
    audit_runtimes(
        &[("acme/app".to_string(), dir.path().to_path_buf())],
        &Feeds,
        None,
        Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap(),
        &NvdPolicy::instant(),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    )
    .await
    .findings
}

// ---- the checks -----------------------------------------------------------------------------

#[tokio::test]
async fn every_producer_emits_one_row_per_site_in_a_fresh_scan() {
    let clone = clone_rows().await;
    let runtime = runtime_rows().await;

    // The clone advisory: four copies, four rows, each at its own file.
    assert_eq!(clone.len(), 4, "{clone:#?}");
    let places: BTreeSet<&str> = clone.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(places.len(), 4, "one row per copy: {places:?}");

    // The runtime audit: the end-of-life finding and the vulnerability finding each cover three
    // files, so each is three rows.
    let eol = runtime.iter().filter(|f| f.rule_id == EOL_RULE_ID).count();
    let vuln = runtime.iter().filter(|f| f.rule_id == VULN_RULE_ID).count();
    assert_eq!((eol, vuln), (3, 3), "{runtime:#?}");

    let fresh: Vec<&Finding> = clone.iter().chain(runtime.iter()).collect();
    let carrying: Vec<String> = fresh
        .iter()
        .filter(|f| !f.also_locations.is_empty())
        .map(|f| {
            format!(
                "{} {}:{} carries {} also_locations",
                f.rule_id,
                f.path,
                f.line,
                f.also_locations.len()
            )
        })
        .collect();
    assert!(
        carrying.is_empty(),
        "a fresh scan must not put a site in also_locations: {carrying:#?}"
    );

    // Every row has a place of its own: no two rows of one rule share a file and line.
    let mut seen = BTreeSet::new();
    for f in &fresh {
        assert!(
            seen.insert((f.rule_id.as_str(), f.path.as_str(), f.line)),
            "two rows at {} {}:{}",
            f.rule_id,
            f.path,
            f.line
        );
    }
}

/// The lines of `source` outside any `#[cfg(test)]` item that write `also_locations` or build a
/// `MergedLocation`, as `(1-based line, text)`. The loader may empty or take the field and a
/// struct may start it empty; nothing else may fill it.
fn producer_writes(source: &str, allowed: &[&str]) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            // Skip the item the attribute belongs to.
            let attr_indent = indent(line);
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_start().starts_with("#[") {
                j += 1;
            }
            if j < lines.len() && lines[j].trim_end().ends_with(';') {
                i = j + 1;
                continue;
            }
            while j < lines.len()
                && !(indent(lines[j]) == attr_indent && lines[j].trim_start().starts_with('}'))
            {
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if !trimmed.starts_with("//")
            && writes_also_locations(trimmed)
            && !allowed.iter().any(|a| trimmed.contains(a))
        {
            found.push((i + 1, trimmed.to_string()));
        }
        i += 1;
    }
    found
}

fn writes_also_locations(line: &str) -> bool {
    if line.contains("MergedLocation::site(") {
        return true;
    }
    if line.contains("MergedLocation {") && !line.contains("struct ") && !line.contains("impl ") {
        return true;
    }
    for call in [".push(", ".extend(", ".insert(", ".append("] {
        if line.contains(&format!("also_locations{call}")) {
            return true;
        }
    }
    if line.contains("mem::replace(&mut") && line.contains("also_locations") {
        return true;
    }
    let mut rest = line;
    while let Some(at) = rest.find("also_locations") {
        let after = rest[at + "also_locations".len()..].trim_start();
        if let Some(value) = after.strip_prefix(':').map(str::trim_start) {
            // `also_locations: Vec::new()` starts a row empty; `pub also_locations: Vec<..>` is a
            // field declaration.
            let empty = ["Vec::new()", "Vec::default()", "Vec<", "vec![]"]
                .iter()
                .any(|e| value.starts_with(e));
            if !empty {
                return true;
            }
        } else if after.starts_with('=') && !after.starts_with("==") {
            let value = after[1..].trim_start();
            if !(value.starts_with("Vec::new()") || value.starts_with("Vec::default()")) {
                return true;
            }
        }
        rest = &rest[at + "also_locations".len()..];
    }
    false
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The out-of-line modules `source` (at `path`) declares, as `(gated, files)`: `gated` is true when
/// the declaration is under `#[cfg(test)]`, and `files` are the paths the module may live at.
fn declared_modules(path: &Path, source: &str) -> Vec<(bool, Vec<PathBuf>)> {
    let lines: Vec<&str> = source.lines().collect();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    // A module file's children sit beside it; any other file's sit in a folder named for it.
    let home = if matches!(stem.as_str(), "mod" | "lib" | "main") {
        dir.to_path_buf()
    } else {
        dir.join(&stem)
    };
    let mut out = Vec::new();
    // The inline modules the line sits in: (indent, name, whether it is `#[cfg(test)]`).
    let mut open: Vec<(usize, String, bool)> = Vec::new();
    let attrs_above = |i: usize| {
        let mut j = i;
        let mut attrs = Vec::new();
        while j > 0 && lines[j - 1].trim_start().starts_with("#[") {
            j -= 1;
            attrs.push(lines[j].trim());
        }
        attrs
    };
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if trimmed.starts_with('}') {
            while open.last().is_some_and(|(at, _, _)| *at >= indent) {
                open.pop();
            }
        }
        let bare = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub "))
            .unwrap_or(trimmed);
        let Some(rest) = bare.strip_prefix("mod ") else {
            continue;
        };
        let attrs = attrs_above(i);
        let own_gate = attrs.iter().any(|a| a.starts_with("#[cfg(test)]"));
        if let Some(name) = rest.strip_suffix('{') {
            open.push((indent, name.trim().to_string(), own_gate));
            continue;
        }
        let Some(name) = rest.strip_suffix(';') else {
            continue;
        };
        let name = name.trim().to_string();
        let gated = own_gate || open.iter().any(|(_, _, g)| *g);
        let mut folder = home.clone();
        for (_, inline, _) in &open {
            folder = folder.join(inline);
        }
        let explicit = attrs.iter().find_map(|a| {
            a.strip_prefix("#[path = \"")
                .and_then(|r| r.strip_suffix("\"]"))
                .map(|f| dir.join(f))
        });
        let files = match explicit {
            Some(file) => vec![file],
            None => vec![
                folder.join(format!("{name}.rs")),
                folder.join(&name).join("mod.rs"),
            ],
        };
        out.push((gated, files));
    }
    out
}

/// The files that exist only under `#[cfg(test)]`: a module declared `#[cfg(test)]` where it is
/// declared, and every module those files declare in turn. A file is never test-only because of
/// what it is called or where it sits.
fn test_only_files(sources: &[(PathBuf, String)]) -> BTreeSet<PathBuf> {
    let text_of: std::collections::BTreeMap<&Path, &str> = sources
        .iter()
        .map(|(p, t)| (p.as_path(), t.as_str()))
        .collect();
    let mut test_only: BTreeSet<PathBuf> = BTreeSet::new();
    let mut work: Vec<PathBuf> = Vec::new();
    for (path, text) in sources {
        for (gated, files) in declared_modules(path, text) {
            if gated {
                work.extend(files);
            }
        }
    }
    while let Some(file) = work.pop() {
        if !test_only.insert(file.clone()) {
            continue;
        }
        if let Some(text) = text_of.get(file.as_path()) {
            for (_, children) in declared_modules(&file, text) {
                work.extend(children);
            }
        }
    }
    test_only
}

/// Lines that copy a row's `also_locations` into a view of it for an exporter. They read the
/// field; they do not produce a site.
const ALLOWED: &[&str] = &["also_locations: f.also_locations.clone()"];

#[test]
fn no_producer_source_writes_also_locations() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap().filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        // The review screen's crates mirror the field to read it; they never build a row.
        if name == "ui" || name == "ui-core" {
            continue;
        }
        rust_sources(&entry.path().join("src"), &mut files);
    }
    assert!(
        files.len() > 100,
        "the scan found the sources: {}",
        files.len()
    );
    files.sort();
    let sources: Vec<(PathBuf, String)> = files
        .iter()
        .map(|p| (p.clone(), std::fs::read_to_string(p).unwrap()))
        .collect();
    let test_only = test_only_files(&sources);
    assert!(
        test_only
            .iter()
            .any(|p| p.ends_with("clone_advisory/tests/mod.rs")),
        "the scan found the gated test modules"
    );
    let mut offences = Vec::new();
    for (path, source) in &sources {
        if test_only.contains(path) {
            continue;
        }
        for (line, text) in producer_writes(source, ALLOWED) {
            let rel = path.strip_prefix(&crates).unwrap_or(path);
            offences.push(format!("{}:{line}: {text}", rel.display()));
        }
    }
    assert!(
        offences.is_empty(),
        "a producer writes also_locations; emit one row per site instead:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_source_scan_catches_a_new_producer_and_ignores_test_code() {
    let producer = "\
fn finding() -> Finding {
    let mut f = Finding::default();
    f.also_locations.push(MergedLocation::site(r, p, 1, id, s, false));
    f
}
";
    assert_eq!(producer_writes(producer, &[]).len(), 1);
    let literal = "    also_locations: sites.iter().map(to_site).collect(),\n";
    assert_eq!(producer_writes(literal, &[]).len(), 1);
    let assigned = "    row.also_locations = sites;\n";
    assert_eq!(producer_writes(assigned, &[]).len(), 1);
    // Starting empty, emptying and reading are fine.
    let quiet = "\
    also_locations: Vec::new(),
    row.also_locations = Vec::new();
    let n = f.also_locations.len();
    let taken = std::mem::take(&mut f.also_locations);
";
    assert!(producer_writes(quiet, &[]).is_empty());
    // Test code inside the file is not a producer.
    let tested = "\
fn real() {}

#[cfg(test)]
mod tests {
    fn fixture() {
        f.also_locations = vec![MergedLocation::site(r, p, 1, id, s, false)];
    }
}

fn after() {}
";
    assert!(producer_writes(tested, &[]).is_empty());
    let after_tests = "\
#[cfg(test)]
mod tests {
    fn fixture() {}
}

fn late() {
    f.also_locations.push(x);
}
";
    assert_eq!(producer_writes(after_tests, &[]).len(), 1);
}

#[test]
fn a_file_is_test_only_only_when_it_is_gated_where_it_is_declared() {
    let root = PathBuf::from("/src");
    let lib = (
        root.join("lib.rs"),
        "mod export_tests;\n#[cfg(test)]\nmod gated;\n#[cfg(test)]\nmod outer {\n    mod inner;\n}\n"
            .to_string(),
    );
    let gated = (root.join("gated.rs"), "mod child;\n".to_string());
    let only = test_only_files(&[lib, gated]);
    // A name that says "tests" proves nothing when the declaration is not gated.
    assert!(!only.contains(&root.join("export_tests.rs")));
    assert!(only.contains(&root.join("gated.rs")));
    assert!(only.contains(&root.join("gated/child.rs")));
    assert!(only.contains(&root.join("outer/inner.rs")));
}
