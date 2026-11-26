//! Every difference between two sealed results, named.

use serde_json::Value;

use crate::package::Sealed;

// ── The comparison ───────────────────────────────────────────────────────────────────────

/// Every difference between the two sealed results, named.
pub(crate) fn differences(app: &Sealed, cli: &Sealed) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |what: &str, a: String, c: String| {
        if a != c {
            out.push(format!("{what}:\n  app: {a}\n  cli: {c}"));
        }
    };
    check(
        "package file list",
        format!("{:?}", app.files),
        format!("{:?}", cli.files),
    );
    check(
        "findings",
        format!("{:?}", app.findings),
        format!("{:?}", cli.findings),
    );
    check(
        "coverage counts",
        app.coverage_counts.to_string(),
        cli.coverage_counts.to_string(),
    );
    check(
        "not-read list",
        format!("{:?}", app.not_read),
        format!("{:?}", cli.not_read),
    );
    check(
        "excluded-by-agreement list",
        format!("{:?}", app.excluded),
        format!("{:?}", cli.excluded),
    );
    for (what, a, c) in [
        ("text file", &app.text, &cli.text),
        ("workbook entry", &app.workbook, &cli.workbook),
    ] {
        let names: std::collections::BTreeSet<&String> = a.keys().chain(c.keys()).collect();
        for name in names {
            if a.get(name) != c.get(name) {
                out.push(format!("{what} {name} differs"));
            }
        }
    }
    if app.generated_at == cli.generated_at && app.pdf != cli.pdf {
        out.push(format!(
            "the PDF differs although both were built in the same minute ({})",
            app.generated_at
        ));
    }
    let names: std::collections::BTreeSet<&String> =
        app.json.keys().chain(cli.json.keys()).collect();
    for name in names {
        let (a, c) = (app.json.get(name), cli.json.get(name));
        match (a, c) {
            (Some(a), Some(c)) => {
                let mut paths = Vec::new();
                json_diff(name, a, c, &mut paths);
                for p in paths {
                    out.push(p);
                }
            }
            _ => out.push(format!(
                "{name}: only in {}",
                if a.is_some() { "app" } else { "cli" }
            )),
        }
    }
    out
}

/// The JSON paths at which `a` (app) and `c` (cli) differ, with both values.
pub(crate) fn json_diff(path: &str, a: &Value, c: &Value, out: &mut Vec<String>) {
    match (a, c) {
        (Value::Object(ma), Value::Object(mc)) => {
            let keys: std::collections::BTreeSet<&String> = ma.keys().chain(mc.keys()).collect();
            for k in keys {
                let null = Value::Null;
                json_diff(
                    &format!("{path}.{k}"),
                    ma.get(k).unwrap_or(&null),
                    mc.get(k).unwrap_or(&null),
                    out,
                );
            }
        }
        (Value::Array(va), Value::Array(vc)) if va.len() == vc.len() => {
            for (i, (x, y)) in va.iter().zip(vc).enumerate() {
                json_diff(&format!("{path}[{i}]"), x, y, out);
            }
        }
        _ if a != c => out.push(format!("{path}:\n  app: {a}\n  cli: {c}")),
        _ => {}
    }
}
