//! A benchmark-style harness on a synthetic 20,000-file repository (the scan's hard cap).
//!
//! It is `#[ignore]`d: it takes tens of seconds and is for measuring, not for gating. Run it in
//! release mode and read the peak memory from the OS:
//!
//! ```text
//! CARGO_INCREMENTAL=0 cargo test -p camerata-checks --release --lib \
//!     extract::clone::tests::perf -- --ignored --nocapture
//! /usr/bin/time -l <the test binary> perf --ignored --nocapture    # peak RSS
//! ```
//!
//! The corpus is built from random expression trees (high structural entropy, like real code
//! that is NOT copied), with a library of shared functions copied into about 3% of the files
//! under renamed locals (the clones to find), and a framework skeleton in about 2% (the house
//! pattern the ubiquity guard must drop).

use std::time::Instant;

use super::*;

const FILES: usize = 20_000;

/// The dialects the generator writes.
#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Ts,
    Py,
    Rs,
}

/// `a.b.c`: a name followed by one to four property reads.
fn chain(rng: &mut Rng) -> String {
    let mut out = rng.ident();
    for _ in 0..(1 + rng.below(4)) {
        out.push('.');
        out.push_str(&rng.ident());
    }
    out
}

/// A comma-separated list of one to four expressions.
fn args(rng: &mut Rng, d: Dialect, depth: usize) -> String {
    let count = 1 + rng.below(4);
    (0..count)
        .map(|_| expr(rng, d, depth))
        .collect::<Vec<_>>()
        .join(", ")
}

fn expr(rng: &mut Rng, d: Dialect, depth: usize) -> String {
    if depth == 0 || rng.below(4) == 0 {
        return match rng.below(7) {
            0 => rng.ident(),
            1 => chain(rng),
            2 => format!("{}", rng.below(1000)),
            3 => match d {
                Dialect::Rs => format!("\"{}\"", rng.ident()),
                _ => format!("'{}'", rng.ident()),
            },
            4 => format!("{}[{}]", rng.ident(), rng.below(9)),
            5 => format!("{}.{}()", chain(rng), rng.ident()),
            _ => match d {
                Dialect::Ts => format!("`{}${{{}}}`", rng.ident(), rng.ident()),
                Dialect::Py => format!("f'{}{{{}}}'", rng.ident(), rng.ident()),
                Dialect::Rs => format!("{}::{}", rng.ident(), rng.ident()),
            },
        };
    }
    let arith = ["+", "-", "*", "/", "%"];
    let compare_ts = ["===", "!==", "<", ">", "<=", ">=", "&&", "||", "??"];
    let compare_py = ["==", "!=", "<", ">", "<=", ">=", "and", "or", "in"];
    let compare_rs = ["==", "!=", "<", ">", "<=", ">=", "&&", "||"];
    match rng.below(8) {
        0 => format!(
            "{} {} {}",
            expr(rng, d, depth - 1),
            rng.pick(&arith),
            expr(rng, d, depth - 1)
        ),
        1 => {
            let op = match d {
                Dialect::Ts => *rng.pick(&compare_ts),
                Dialect::Py => *rng.pick(&compare_py),
                Dialect::Rs => *rng.pick(&compare_rs),
            };
            let text = format!(
                "{} {} {}",
                expr(rng, d, depth - 1),
                op,
                expr(rng, d, depth - 1)
            );
            // Rust has no chained comparison: `a < b < c` does not parse.
            if d == Dialect::Rs {
                format!("({text})")
            } else {
                text
            }
        }
        2 => format!("{}({})", rng.ident(), args(rng, d, depth - 1)),
        3 => {
            let (c, a, b) = (
                expr(rng, d, depth - 1),
                expr(rng, d, depth - 1),
                expr(rng, d, depth - 1),
            );
            match d {
                Dialect::Ts => format!("{c} ? {a} : {b}"),
                Dialect::Py => format!("{a} if {c} else {b}"),
                Dialect::Rs => format!("(if {c} {{ {a} }} else {{ {b} }})"),
            }
        }
        4 => match d {
            Dialect::Rs => format!("vec![{}]", args(rng, d, depth - 1)),
            _ => format!("[{}]", args(rng, d, depth - 1)),
        },
        5 => format!(
            "{}.{}({})",
            chain(rng),
            rng.ident(),
            args(rng, d, depth - 1)
        ),
        6 => match d {
            Dialect::Py => format!("not {}", expr(rng, d, depth - 1)),
            _ => format!("!{}", expr(rng, d, depth - 1)),
        },
        _ => match d {
            Dialect::Ts => format!("({}) => {}", rng.ident(), expr(rng, d, depth - 1)),
            Dialect::Py => format!("(lambda {}: {})", rng.ident(), expr(rng, d, depth - 1)),
            Dialect::Rs => format!("(|{}| {})", rng.ident(), expr(rng, d, depth - 1)),
        },
    }
}

fn params(rng: &mut Rng, typed: Option<&str>) -> String {
    let count = rng.below(5);
    (0..count)
        .map(|_| match typed {
            Some(ty) => format!("{}: {ty}", rng.ident()),
            None => rng.ident(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn ts_statement(rng: &mut Rng, indent: &str) -> String {
    let d = Dialect::Ts;
    match rng.below(9) {
        0 => format!("{indent}const {} = {};\n", rng.ident(), expr(rng, d, 3)),
        1 => format!(
            "{indent}if ({}) {{\n{indent}  return {};\n{indent}}}\n",
            expr(rng, d, 2),
            expr(rng, d, 2)
        ),
        2 => format!("{indent}{}.{} = {};\n", rng.ident(), rng.ident(), expr(rng, d, 3)),
        3 => format!(
            "{indent}for (const {} of {}.{}) {{\n{indent}  {}({});\n{indent}}}\n",
            rng.ident(),
            rng.ident(),
            rng.ident(),
            rng.ident(),
            expr(rng, d, 2)
        ),
        4 => format!("{indent}{} += {};\n", rng.ident(), expr(rng, d, 3)),
        5 => format!("{indent}{}({});\n", rng.ident(), args(rng, d, 2)),
        6 => format!(
            "{indent}try {{\n{indent}  {};\n{indent}}} catch ({}) {{\n{indent}  {}({});\n{indent}}}\n",
            expr(rng, d, 2),
            rng.ident(),
            rng.ident(),
            rng.ident()
        ),
        7 => format!(
            "{indent}while ({}) {{\n{indent}  {} = {};\n{indent}}}\n",
            expr(rng, d, 2),
            rng.ident(),
            expr(rng, d, 2)
        ),
        _ => format!("{indent}let {} = {};\n", rng.ident(), expr(rng, d, 3)),
    }
}

fn ts_file(rng: &mut Rng) -> String {
    let mut out = String::from(
        "import { helper } from './helper';\nimport type { Shape } from './types';\n\n",
    );
    for _ in 0..(3 + rng.below(4)) {
        let (name, p) = (rng.ident(), params(rng, None));
        out.push_str(&match rng.below(3) {
            0 => format!("export function {name}({p}) {{\n"),
            1 => format!("export async function {name}({p}) {{\n"),
            _ => format!("function {name}({p}) {{\n"),
        });
        for _ in 0..(5 + rng.below(6)) {
            out.push_str(&ts_statement(rng, "  "));
        }
        if rng.below(3) != 0 {
            out.push_str(&format!("  return {};\n", expr(rng, Dialect::Ts, 2)));
        }
        out.push_str("}\n\n");
    }
    out
}

fn py_statement(rng: &mut Rng, indent: &str) -> String {
    let d = Dialect::Py;
    match rng.below(7) {
        0 => format!("{indent}{} = {}\n", rng.ident(), expr(rng, d, 3)),
        1 => format!(
            "{indent}if {}:\n{indent}    return {}\n",
            expr(rng, d, 2),
            expr(rng, d, 2)
        ),
        2 => format!(
            "{indent}for {} in {}.{}:\n{indent}    {} += {}\n",
            rng.ident(),
            rng.ident(),
            rng.ident(),
            rng.ident(),
            expr(rng, d, 2)
        ),
        3 => format!("{indent}{}({})\n", rng.ident(), args(rng, d, 2)),
        4 => format!(
            "{indent}{}.{} = {}\n",
            rng.ident(),
            rng.ident(),
            expr(rng, d, 3)
        ),
        5 => format!(
            "{indent}try:\n{indent}    {}\n{indent}except {}:\n{indent}    {}({})\n",
            expr(rng, d, 2),
            rng.ident(),
            rng.ident(),
            rng.ident()
        ),
        _ => format!(
            "{indent}while {}:\n{indent}    {} = {}\n",
            expr(rng, d, 2),
            rng.ident(),
            expr(rng, d, 2)
        ),
    }
}

fn py_file(rng: &mut Rng) -> String {
    let mut out = String::from("import os\nfrom typing import Any\n\n");
    for _ in 0..(3 + rng.below(4)) {
        let (name, p) = (rng.ident(), params(rng, None));
        out.push_str(&format!("def {name}({p}):\n"));
        for _ in 0..(5 + rng.below(6)) {
            out.push_str(&py_statement(rng, "    "));
        }
        if rng.below(3) != 0 {
            out.push_str(&format!("    return {}\n", expr(rng, Dialect::Py, 2)));
        } else {
            out.push_str(&format!(
                "    {}({})\n",
                rng.ident(),
                args(rng, Dialect::Py, 1)
            ));
        }
        out.push('\n');
    }
    out
}

fn rs_statement(rng: &mut Rng) -> String {
    let d = Dialect::Rs;
    match rng.below(6) {
        0 => format!("    let {} = {};\n", rng.ident(), expr(rng, d, 3)),
        1 => format!(
            "    if {} {{\n        {} += {};\n    }}\n",
            expr(rng, d, 2),
            rng.ident(),
            expr(rng, d, 2)
        ),
        2 => format!("    {}({});\n", rng.ident(), args(rng, d, 2)),
        3 => format!(
            "    for {} in {}.{}() {{\n        {};\n    }}\n",
            rng.ident(),
            rng.ident(),
            rng.ident(),
            expr(rng, d, 2)
        ),
        4 => format!(
            "    {}.{} = {};\n",
            rng.ident(),
            rng.ident(),
            expr(rng, d, 3)
        ),
        _ => format!(
            "    while {} {{\n        {} = {};\n    }}\n",
            expr(rng, d, 2),
            rng.ident(),
            expr(rng, d, 2)
        ),
    }
}

fn rs_file(rng: &mut Rng) -> String {
    let mut out = String::from("use std::fmt;\n\n");
    for _ in 0..(3 + rng.below(4)) {
        let (name, p) = (rng.ident(), params(rng, Some("u64")));
        out.push_str(&format!("pub fn {name}({p}) -> u64 {{\n"));
        for _ in 0..(5 + rng.below(6)) {
            out.push_str(&rs_statement(rng));
        }
        out.push_str(&format!("    {}\n}}\n\n", expr(rng, Dialect::Rs, 2)));
    }
    out
}

/// The synthetic repository: 70% TypeScript, 20% Python, 10% Rust, plus the shared library.
fn synthetic_repo(files: usize) -> Vec<(String, String)> {
    let mut rng = Rng(0x5eed);
    let library: Vec<String> = (0..50)
        .map(|n| {
            // 50 distinct shared functions: the discount shape with `n` extra statements.
            let extra = "  log(base, order.id);\n".repeat(1 + n % 7);
            let tail = format!("  return base + order.shipping + order.f{n};\n");
            ts_discount(
                "shared",
                "order",
                "user",
                "base",
                &format!("0.{}", 1 + n % 90),
            )
            .replace(
                "  return base + order.shipping;\n",
                &format!("{extra}{tail}"),
            )
        })
        .collect();
    let skeleton = |n: usize| {
        format!(
            "export async function handler{n}(req, res) {{
  const session = await req.getSession();
  if (session.status === 401 || session.expired > 30) {{
    return res.status(401).json({{ error: 'unauthorized' }});
  }}
  return res.status(200).json(await load(req.query.id));
}}
"
        )
    };
    let mut out = Vec::with_capacity(files);
    for n in 0..files {
        let dir = format!("packages/p{:03}/src/m{:02}", n % 300, (n / 300) % 40);
        let (name, mut text) = match rng.below(10) {
            0..=6 => (format!("{dir}/f{n}.ts"), ts_file(&mut rng)),
            7 | 8 => (format!("{dir}/f{n}.py"), py_file(&mut rng)),
            _ => (format!("{dir}/f{n}.rs"), rs_file(&mut rng)),
        };
        if name.ends_with(".ts") {
            match rng.below(100) {
                0..=2 => {
                    let pick = rng.below(library.len());
                    let body = library[pick].replacen(
                        "function shared(",
                        &format!("function {}(", rng.ident()),
                        1,
                    );
                    text.push_str(&body.replacen("import { money } from './money';\n", "", 1));
                }
                3 | 4 => text.push_str(&skeleton(n)),
                _ => {}
            }
        }
        out.push((name, text));
    }
    out
}

#[test]
#[ignore = "perf harness: run with --ignored --nocapture, in release mode"]
fn perf_twenty_thousand_file_repo() {
    let generate = Instant::now();
    let repo = synthetic_repo(FILES);
    let bytes: usize = repo.iter().map(|(_, c)| c.len()).sum();
    let lines: usize = repo.iter().map(|(_, c)| c.lines().count()).sum();
    eprintln!(
        "generated {} files, {} lines, {:.1} MB in {:.1}s",
        repo.len(),
        lines,
        bytes as f64 / 1e6,
        generate.elapsed().as_secs_f64()
    );

    // `CLONE_PERF_GENERATE_ONLY` stops here, so the process's peak memory can be read for the
    // corpus alone and subtracted from a full run's.
    if std::env::var("CLONE_PERF_GENERATE_ONLY").is_ok() {
        return;
    }
    // `CLONE_PERF_MAX_PAIRS` overrides the pair cap, to measure what a run costs when the
    // default cap does not cut it short.
    let mut options = CloneOptions::default();
    if let Some(pairs) = std::env::var("CLONE_PERF_MAX_PAIRS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        options.max_pairs = pairs;
    }
    let started = Instant::now();
    let index = detect(&repo, &options);
    let elapsed = started.elapsed();
    eprintln!(
        "detect: {:.2}s, files_read {}, skipped {}, families {}, boilerplate buckets {}, truncated {}",
        elapsed.as_secs_f64(),
        index.files_read,
        index.skipped.len(),
        index.families.len(),
        index.boilerplate_buckets_skipped,
        index.truncated
    );
    let mut reasons = std::collections::BTreeMap::new();
    for (_, reason) in &index.skipped {
        *reasons.entry(format!("{reason:?}")).or_insert(0usize) += 1;
    }
    eprintln!("skip reasons: {reasons:?}");
    let house = index.families.iter().filter(|f| f.house_pattern).count();
    eprintln!("house patterns: {house}");

    assert!(!index.families.is_empty(), "the shared library is found");
    if !index.truncated {
        assert_eq!(index.files_read as usize + index.skipped.len(), FILES);
    }
    if std::env::var("CLONE_PERF_REPEAT").is_ok() {
        let again = Instant::now();
        let second = detect(&repo, &options);
        eprintln!("second run: {:.2}s", again.elapsed().as_secs_f64());
        assert_eq!(second, index, "deterministic at scale");
    }
}

#[test]
fn synthetic_repo_generator_is_deterministic_and_small_runs_find_the_library() {
    let a = synthetic_repo(400);
    let b = synthetic_repo(400);
    assert_eq!(a, b);
    let index = index_of(&a);
    assert!(!index.truncated);
    assert_eq!(index.files_read as usize + index.skipped.len(), 400);
}

/// Every source file under `root`, as the scan would hand them over: dot-directories,
/// `node_modules` and `target` are pruned, and files over the scan's size limit are left out.
fn read_tree(root: &std::path::Path, prefix: &str, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if path.is_dir() {
            read_tree(&path, &format!("{prefix}{name}/"), out);
        } else if crate::extract::lang_for_path(&name).is_some() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text.len() <= 400_000 {
                    out.push((format!("{prefix}{name}"), text));
                }
            }
        }
    }
}

#[test]
#[ignore = "perf harness: set CLONE_PERF_ROOTS to colon-separated directories"]
fn perf_real_corpus() {
    let Ok(roots) = std::env::var("CLONE_PERF_ROOTS") else {
        eprintln!("CLONE_PERF_ROOTS not set; nothing to measure");
        return;
    };
    let mut repo = Vec::new();
    for root in roots.split(':').filter(|r| !r.is_empty()) {
        let path = std::path::Path::new(root);
        let label = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().to_string());
        read_tree(path, &format!("{label}/"), &mut repo);
    }
    let bytes: usize = repo.iter().map(|(_, c)| c.len()).sum();
    eprintln!(
        "real corpus: {} files, {:.1} MB",
        repo.len(),
        bytes as f64 / 1e6
    );
    let started = Instant::now();
    let index = detect(&repo, &CloneOptions::default());
    eprintln!(
        "detect: {:.2}s, files_read {}, skipped {}, families {}, boilerplate buckets {}, truncated {}",
        started.elapsed().as_secs_f64(),
        index.files_read,
        index.skipped.len(),
        index.families.len(),
        index.boilerplate_buckets_skipped,
        index.truncated
    );
    let mut by_len: Vec<&CloneFamily> = index.families.iter().collect();
    by_len.sort_by_key(|f| std::cmp::Reverse(f.token_len));
    for family in by_len.iter().take(8) {
        eprintln!(
            "  family len {} files {} members {}: {}",
            family.token_len,
            family.distinct_files,
            family.members.len(),
            family.members.first().map_or(String::new(), |m| format!(
                "{}:{}-{}",
                m.file, m.start_line, m.end_line
            ))
        );
    }
}

#[test]
#[ignore = "perf harness"]
fn perf_generated_rust_parses() {
    let mut rng = Rng(5);
    let mut failures = 0;
    for _ in 0..200 {
        let source = rs_file(&mut rng);
        if let Err(error) = syn::parse_file(&source) {
            failures += 1;
            if failures == 1 {
                let at = error.span().start();
                let line = source.lines().nth(at.line - 1).unwrap_or("");
                eprintln!("first error: {error} at {}:{}: {line}", at.line, at.column);
            }
        }
    }
    eprintln!("rust parse failures: {failures}/200");
}

#[test]
#[ignore = "perf harness"]
fn perf_tokenizer_throughput() {
    let mut rng = Rng(77);
    let ts: Vec<String> = (0..300).map(|_| ts_file(&mut rng)).collect();
    let py: Vec<String> = (0..300).map(|_| py_file(&mut rng)).collect();
    let rs: Vec<String> = (0..300).map(|_| rs_file(&mut rng)).collect();
    for (name, lang, set) in [
        ("ts", SourceLang::TypeScript, &ts),
        ("py", SourceLang::Python, &py),
        ("rs", SourceLang::Rust, &rs),
    ] {
        let bytes: usize = set.iter().map(String::len).sum();
        let started = Instant::now();
        let mut total_tokens = 0usize;
        for source in set {
            total_tokens += tokens(lang, source, TokenMode::Structural)
                .iter()
                .map(|i| i.tokens.len())
                .sum::<usize>();
        }
        let secs = started.elapsed().as_secs_f64();
        eprintln!(
            "{name}: {} files, {:.1} MB, {} tokens in {:.3}s = {:.1} MB/s, {:.0} ns/token",
            set.len(),
            bytes as f64 / 1e6,
            total_tokens,
            secs,
            bytes as f64 / 1e6 / secs,
            secs * 1e9 / total_tokens as f64
        );
    }
    // The grammar alone, for comparison.
    let started = Instant::now();
    let bytes: usize = ts.iter().map(String::len).sum();
    for source in &ts {
        let _ = crate::extract::ecma::parse(crate::extract::ecma::EcmaDialect::TypeScript, source);
    }
    let secs = started.elapsed().as_secs_f64();
    eprintln!("ts parse only: {:.1} MB/s", bytes as f64 / 1e6 / secs);
}
