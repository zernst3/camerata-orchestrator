//! The plain-text preview written next to a bundle and printed by the CLI, rendered from the
//! bundle JSON alone (design section 5), so `camerata diagnose preview` renders the same text
//! from a written bundle:
//!
//! 1. a header: engine, run status, attempt, and the part table;
//! 2. the failures: class, part skeleton, attempts and the shapes table;
//! 3. every distinct text value in the bundle, one per line, sorted;
//! 4. the `undecided` list and the leak check result.
//!
//! Every fixed word here is listed in [`super::BUNDLE_WORDS`], so the leak check knows the
//! preview's own words from a client's.

use std::collections::BTreeSet;
use std::fmt::Write;

use serde_json::Value;

fn s<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("-")
}

fn n(v: &Value, pointer: &str) -> String {
    match v.pointer(pointer) {
        Some(Value::Number(x)) => x.to_string(),
        Some(Value::Bool(b)) => if *b { "yes" } else { "no" }.to_string(),
        Some(Value::String(x)) if !x.is_empty() => x.clone(),
        _ => "-".to_string(),
    }
}

fn row(out: &mut String, cells: &[String], widths: &[usize]) {
    let mut line = String::from("  ");
    for (cell, w) in cells.iter().zip(widths) {
        let _ = write!(line, "{cell:<w$}  ");
    }
    out.push_str(line.trim_end());
    out.push('\n');
}

fn table(out: &mut String, header: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(c.chars().count());
            }
        }
    }
    row(
        out,
        &header.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
        &widths,
    );
    for r in rows {
        row(out, r, &widths);
    }
}

fn strings(v: &Value, skip: &[&str], path: &str, out: &mut BTreeSet<String>) {
    match v {
        Value::String(x) if !x.is_empty() => {
            out.insert(x.clone());
        }
        Value::Array(a) => a.iter().for_each(|v| strings(v, skip, path, out)),
        Value::Object(m) => {
            for (k, v) in m {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if !skip.contains(&child.as_str()) {
                    strings(v, skip, &child, out);
                }
            }
        }
        _ => {}
    }
}

/// The preview of a bundle.
pub fn render(bundle: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Camerata diagnostic bundle {}",
        s(bundle, "/bundle/id")
    );
    let _ = writeln!(
        out,
        "Engine {} on {} {}, made {}, named paths: {}",
        s(bundle, "/bundle/engine_version"),
        s(bundle, "/bundle/os"),
        s(bundle, "/bundle/arch"),
        s(bundle, "/bundle/made_at"),
        n(bundle, "/bundle/named_paths"),
    );
    let _ = writeln!(
        out,
        "Run status {}, attempt {}, origin {}, engine of the run {}",
        s(bundle, "/run/status"),
        n(bundle, "/run/attempt"),
        s(bundle, "/run/origin"),
        s(bundle, "/run/engine_version"),
    );

    out.push_str("\nParts\n");
    let parts: Vec<Vec<String>> = bundle
        .pointer("/run/plan")
        .and_then(Value::as_array)
        .map(|plan| {
            plan.iter()
                .map(|p| {
                    vec![
                        s(p, "/id").to_string(),
                        s(p, "/kind").to_string(),
                        s(p, "/status").to_string(),
                        n(p, "/calls_done"),
                        n(p, "/calls_not_done"),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    table(
        &mut out,
        &["part", "kind", "status", "calls done", "calls not done"],
        &parts,
    );

    out.push_str("\nFailures\n");
    match bundle
        .pointer("/failures/entries")
        .and_then(Value::as_array)
    {
        None => out.push_str("  none (no failure report)\n"),
        Some(entries) if entries.is_empty() => out.push_str("  none\n"),
        Some(entries) => {
            for (i, e) in entries.iter().enumerate() {
                let resolved = s(e, "/resolved_at");
                let _ = writeln!(
                    out,
                    "  {}. {} in {}: {}{}",
                    i + 1,
                    s(e, "/class"),
                    s(e, "/repo"),
                    s(e, "/part"),
                    if resolved == "-" || resolved.is_empty() {
                        String::new()
                    } else {
                        format!(" (resolved {resolved})")
                    }
                );
                let _ = writeln!(out, "     message: {}", s(e, "/message"));
                let attempts: Vec<Vec<String>> = e
                    .get("attempts")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|a| {
                                vec![
                                    s(a, "/at").to_string(),
                                    s(a, "/backend").to_string(),
                                    s(a, "/model").to_string(),
                                    n(a, "/http_status"),
                                    s(a, "/error_type").to_string(),
                                    n(a, "/exit_code"),
                                    n(a, "/wait_secs"),
                                    s(a, "/message").to_string(),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !attempts.is_empty() {
                    out.push_str("     attempts\n");
                    table(
                        &mut out,
                        &[
                            "at",
                            "backend",
                            "model",
                            "http",
                            "error type",
                            "exit",
                            "wait",
                            "message",
                        ],
                        &attempts,
                    );
                }
                if let Some(shapes) = e.get("shapes") {
                    let _ = writeln!(out, "     shapes: {}", s(shapes, "/status"));
                    let rows: Vec<Vec<String>> = shapes
                        .get("files")
                        .and_then(Value::as_array)
                        .map(|f| {
                            f.iter()
                                .map(|f| {
                                    vec![
                                        s(f, "/path").to_string(),
                                        n(f, "/present"),
                                        n(f, "/bytes"),
                                        n(f, "/lines"),
                                        n(f, "/longest_line"),
                                        n(f, "/line_ending"),
                                        n(f, "/utf8"),
                                        n(f, "/bom"),
                                        n(f, "/ends_with_newline"),
                                    ]
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if !rows.is_empty() {
                        table(
                            &mut out,
                            &[
                                "path",
                                "present",
                                "bytes",
                                "lines",
                                "longest",
                                "ending",
                                "utf8",
                                "bom",
                                "final newline",
                            ],
                            &rows,
                        );
                    }
                }
            }
        }
    }

    let mut texts = BTreeSet::new();
    strings(bundle, &["undecided", "rejected_values"], "", &mut texts);
    let _ = writeln!(out, "\nText values ({})", texts.len());
    for t in &texts {
        let _ = writeln!(out, "  {t}");
    }

    let list = |key: &str| -> Vec<String> {
        bundle
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let undecided = list("undecided");
    let _ = writeln!(
        out,
        "\nUndecided ({}): dropped, no decision",
        undecided.len()
    );
    for u in &undecided {
        let _ = writeln!(out, "  {u}");
    }
    let rejected = list("rejected_values");
    let _ = writeln!(
        out,
        "\nRejected values ({}): decided, but the value did not fit",
        rejected.len()
    );
    for r in &rejected {
        let _ = writeln!(out, "  {r}");
    }
    let _ = writeln!(out, "\nLeak check: {}", s(bundle, "/leak_check"));
    out
}
