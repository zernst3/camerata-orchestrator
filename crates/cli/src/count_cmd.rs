//! `camerata count <repo-path> [--json]`: how big a repo is as the inspection sees it.
//!
//! No model, no network, seconds to run. It is a thin shell over
//! [`camerata_server::onboard::count::count_repo`], which reads the repo through the
//! inspection's own file filter, so the number printed here is the number `camerata inspect`
//! is priced on. Output is a plain-text table, or the same figures as JSON with `--json`.
//!
//! # The full-scan estimate (`--estimate`, `--cost-per-1k-usd`)
//!
//! `--estimate` adds the cost of a full scan before it starts, so a spend limit is set from a
//! number: the point figure, a range, and a suggested spend limit (1.5 x the point figure). Several
//! repo paths in one call give a line per repo and the total. The formula is exactly `counted lines / 1,000 * dollars per 1,000 counted
//! lines`, the same unit `camerata inspect` prints in its stage table
//! (`StageTelemetry::cost_per_1k_lines`). `--cost-per-1k-usd` takes that figure from the
//! operator's own earlier run of the same kind of code; without it the documented constant
//! [`MEASURED_USD_PER_1K_LINES`] is used and the line says so. Nothing here reads another
//! project's data. The range ratios (0.69 and 2.16 of the point figure) come from the same cost
//! model: its low and high cases on a large real project, against the measured rate.

use std::path::PathBuf;

use camerata_server::onboard::count::count_repo;
use camerata_server::onboard::scope::ScanScope;

/// The only measurement at scale (one run of the earlier engine on a large real project), per
/// 1,000 counted lines. A stand-in for operators with no run of their own; the agent finder has
/// no measurement at scale yet.
pub const MEASURED_USD_PER_1K_LINES: f64 = 1.71;

/// Low end of the range as a share of the point figure (the cost model's low case).
pub const LOW_FACTOR: f64 = 0.69;
/// High end of the range as a share of the point figure (the cost model's bad case).
pub const HIGH_FACTOR: f64 = 2.16;
/// The suggested spend limit as a share of the point figure.
pub const LIMIT_FACTOR: f64 = 1.5;

/// The full-scan estimate: `lines / 1,000 * usd_per_1k`.
pub fn full_scan_estimate_usd(counted_lines: usize, usd_per_1k: f64) -> f64 {
    counted_lines as f64 / 1_000.0 * usd_per_1k
}

/// One estimate: the point figure, the range and the suggested spend limit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Estimate {
    lines: usize,
    rate: f64,
    from_flag: bool,
    point: f64,
    low: f64,
    high: f64,
    limit: f64,
}

impl Estimate {
    fn of(lines: usize, rate_flag: Option<f64>) -> Self {
        let rate = rate_flag.unwrap_or(MEASURED_USD_PER_1K_LINES);
        let point = full_scan_estimate_usd(lines, rate);
        Self {
            lines,
            rate,
            from_flag: rate_flag.is_some(),
            point,
            low: point * LOW_FACTOR,
            high: point * HIGH_FACTOR,
            limit: point * LIMIT_FACTOR,
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "usd": self.point,
            "low_usd": self.low,
            "high_usd": self.high,
            "suggested_spend_limit_usd": self.limit,
            "counted_lines": self.lines,
            "usd_per_1k_counted_lines": self.rate,
            "rate_from": if self.from_flag { "flag" } else { "documented constant" },
            "note": NOTE,
        })
    }

    fn text(&self, label: &str) -> String {
        let basis = if self.from_flag {
            "from --cost-per-1k-usd"
        } else {
            "the documented constant measured at scale on an earlier engine; pass \
             --cost-per-1k-usd from your own run to replace it"
        };
        format!(
            "{label}: about ${:.2} ({} counted lines / 1,000 x ${:.2} per 1,000 counted lines; \
             {basis}).\n  Range: ${:.2} to ${:.2} (low = {LOW_FACTOR} x and high = {HIGH_FACTOR} x the \
             point figure, from the documented best guess and bad case).\n  Suggested spend limit: \
             ${:.2} ({LIMIT_FACTOR} x the point figure).\n  {NOTE}",
            self.point, self.lines, self.rate, self.low, self.high, self.limit
        )
    }
}

/// Said on every estimate.
const NOTE: &str =
    "Measured on the earlier engine, not the agent finder. An estimate, not a quote.";

/// Arguments of `camerata count`.
#[derive(Debug, Clone)]
pub struct CountArgs {
    pub repo: PathBuf,
    pub json: bool,
    /// Files agreed to be outside the inspection (`--exclude`): globs matched against
    /// `<repo name>/<path within the repo>`, the repo name being the folder's name.
    pub exclude: Vec<String>,
    /// `--repo-name OWNER/REPO`: the name the repo goes by, as `camerata inspect --repo-name`
    /// takes it. Without it the folder's own name, as inspect names a lone unnamed repo.
    pub repo_name: Option<String>,
}

/// The estimate options of `camerata count`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EstimateArgs {
    /// `--estimate`: add the full-scan cost estimate.
    pub estimate: bool,
    /// `--cost-per-1k-usd`: dollars per 1,000 counted lines from the operator's own earlier run
    /// (implies `--estimate`).
    pub cost_per_1k_usd: Option<f64>,
}

/// The report to print for `args.repo`. Errors when the path is not a directory.
pub fn run_count(args: &CountArgs) -> anyhow::Result<String> {
    run_count_with_estimate(args, EstimateArgs::default())
}

/// [`run_count`] with the optional full-scan estimate.
pub fn run_count_with_estimate(args: &CountArgs, est: EstimateArgs) -> anyhow::Result<String> {
    run_count_many(std::slice::from_ref(&args.repo), args, est)
}

/// Count one repo as the inspection reads it. Errors when the path is not a directory or an
/// exclusion pattern matches no file.
fn count_one(
    repo: &std::path::Path,
    args: &CountArgs,
) -> anyhow::Result<camerata_server::onboard::count::RepoCount> {
    if !repo.is_dir() {
        anyhow::bail!("{} is not a directory", repo.display());
    }
    let scope =
        ScanScope::from_patterns(&args.exclude).map_err(|e| anyhow::anyhow!("--exclude: {e}"))?;
    let name = crate::inspect_cmd::source_name(repo, args.repo_name.as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;
    let count = count_repo(repo, &scope.for_repo(&name))?;
    // A pattern that matches no file is an exclusion that never happened: refuse, naming it.
    if !count.unmatched_patterns.is_empty() {
        anyhow::bail!(
            "Excluded by agreement holds a pattern that matches no file: {}. Fix or remove it.",
            count
                .unmatched_patterns
                .iter()
                .zip(camerata_server::onboard::scope::unmatched_notes(
                    &count.unmatched_patterns,
                    std::slice::from_ref(&name)
                ))
                .map(|(p, note)| if note.is_empty() {
                    format!("\"{p}\"")
                } else {
                    format!("\"{p}\" (matches no file: {note})")
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(count)
}

/// The report for one or several repos of a project. One repo prints exactly what it always did
/// (plus the estimate when asked); several print each repo's table, then a line per repo and the
/// total of the estimate.
pub fn run_count_many(
    repos: &[PathBuf],
    args: &CountArgs,
    est: EstimateArgs,
) -> anyhow::Result<String> {
    let rate = est.cost_per_1k_usd;
    if let Some(r) = rate {
        if !(r.is_finite() && r >= 0.0) {
            anyhow::bail!("--cost-per-1k-usd must be a number of dollars, 0 or more");
        }
    }
    if repos.is_empty() {
        anyhow::bail!("give at least one repo path");
    }
    if repos.len() > 1 && args.repo_name.is_some() {
        anyhow::bail!("--repo-name names one repo; give it with a single path");
    }
    let estimate = est.estimate || rate.is_some();
    let counts: Vec<_> = repos
        .iter()
        .map(|r| count_one(r, args))
        .collect::<anyhow::Result<_>>()?;
    let total_lines: usize = counts.iter().map(|c| c.total_lines).sum();
    if repos.len() == 1 {
        let count = &counts[0];
        return if args.json {
            let mut value = serde_json::to_value(count)?;
            if estimate {
                value["full_scan_estimate"] = Estimate::of(count.total_lines, rate).json();
            }
            Ok(serde_json::to_string_pretty(&value)?)
        } else if estimate {
            Ok(format!(
                "{}\n{}",
                count.to_text(),
                Estimate::of(count.total_lines, rate).text("Full-scan estimate")
            ))
        } else {
            Ok(count.to_text())
        };
    }
    if args.json {
        let rows: Vec<serde_json::Value> = repos
            .iter()
            .zip(&counts)
            .map(|(path, c)| {
                let mut v = serde_json::to_value(c)?;
                v["path"] = serde_json::json!(path.display().to_string());
                if estimate {
                    v["full_scan_estimate"] = Estimate::of(c.total_lines, rate).json();
                }
                Ok(v)
            })
            .collect::<anyhow::Result<_>>()?;
        let mut value = serde_json::json!({ "repos": rows, "total_lines": total_lines });
        if estimate {
            value["full_scan_estimate"] = Estimate::of(total_lines, rate).json();
        }
        return Ok(serde_json::to_string_pretty(&value)?);
    }
    let mut out = String::new();
    for (path, c) in repos.iter().zip(&counts) {
        out.push_str(&format!("== {} ==\n{}\n", path.display(), c.to_text()));
    }
    if estimate {
        for (path, c) in repos.iter().zip(&counts) {
            let e = Estimate::of(c.total_lines, rate);
            out.push_str(&format!(
                "{}: {} counted lines, about ${:.2} (range ${:.2} to ${:.2})\n",
                path.display(),
                c.total_lines,
                e.point,
                e.low,
                e.high
            ));
        }
        out.push_str(&Estimate::of(total_lines, rate).text("Full-scan estimate, all repos"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n\nfn b() {}\n").unwrap();
        std::fs::create_dir(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join("vendor/x.js"), "var x;\n").unwrap();
        std::fs::write(dir.path().join("package-lock.json"), "{\n}\n").unwrap();
        dir
    }

    #[test]
    fn the_estimate_is_counted_lines_over_1000_times_the_rate() {
        let dir = repo();
        let args = CountArgs {
            repo: dir.path().to_path_buf(),
            json: true,
            exclude: Vec::new(),
            repo_name: None,
        };
        // 2 counted lines at $1,500 per 1,000 lines is exactly $3.00.
        let out = run_count_with_estimate(
            &args,
            EstimateArgs {
                estimate: false,
                cost_per_1k_usd: Some(1500.0),
            },
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["full_scan_estimate"]["usd"], 3.0);
        assert_eq!(v["full_scan_estimate"]["counted_lines"], 2);
        assert_eq!(v["full_scan_estimate"]["rate_from"], "flag");
        assert!((full_scan_estimate_usd(100_000, 1.71) - 171.0).abs() < 1e-9);
        let text = run_count_with_estimate(
            &CountArgs {
                json: false,
                ..args.clone()
            },
            EstimateArgs {
                estimate: true,
                cost_per_1k_usd: None,
            },
        )
        .unwrap();
        assert!(text.contains("Full-scan estimate: about $0.00"), "{text}");
        assert!(text.contains("Suggested spend limit"), "{text}");
        assert!(
            text.contains("Measured on the earlier engine, not the agent finder"),
            "{text}"
        );
        assert!(text.contains("documented constant"), "{text}");
        // Without the flag nothing is added.
        assert!(!run_count(&args).unwrap().contains("full_scan_estimate"));
        let bad = run_count_with_estimate(
            &args,
            EstimateArgs {
                estimate: true,
                cost_per_1k_usd: Some(-1.0),
            },
        );
        assert!(bad.is_err());
    }

    #[test]
    fn several_repos_give_a_line_each_and_a_total_with_range_and_limit() {
        let (a, b) = (repo(), repo());
        let args = CountArgs {
            repo: a.path().to_path_buf(),
            json: false,
            exclude: Vec::new(),
            repo_name: None,
        };
        let paths = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let est = EstimateArgs {
            estimate: false,
            cost_per_1k_usd: Some(1000.0),
        };
        // 2 counted lines each, $1,000 per 1,000 lines: $2.00 a repo, $4.00 in all.
        let text = run_count_many(&paths, &args, est).unwrap();
        assert!(
            text.contains("2 counted lines, about $2.00 (range $1.38 to $4.32)"),
            "{text}"
        );
        assert!(
            text.contains("Full-scan estimate, all repos: about $4.00"),
            "{text}"
        );
        assert!(text.contains("Range: $2.76 to $8.64"), "{text}");
        assert!(
            text.contains("Suggested spend limit: $6.00 (1.5 x the point figure)"),
            "{text}"
        );
        assert!(
            text.contains("Measured on the earlier engine, not the agent finder"),
            "{text}"
        );
        let json = run_count_many(
            &paths,
            &CountArgs {
                json: true,
                ..args.clone()
            },
            est,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["repos"].as_array().unwrap().len(), 2);
        assert_eq!(v["total_lines"], 4);
        assert_eq!(v["full_scan_estimate"]["usd"], 4.0);
        assert_eq!(v["full_scan_estimate"]["suggested_spend_limit_usd"], 6.0);
        assert_eq!(v["repos"][1]["full_scan_estimate"]["usd"], 2.0);
        assert!(v["full_scan_estimate"]["low_usd"].as_f64().unwrap() < 4.0);
        assert!(v["full_scan_estimate"]["high_usd"].as_f64().unwrap() > 4.0);
        let named = CountArgs {
            repo_name: Some("a/b".into()),
            ..args
        };
        assert!(run_count_many(&paths, &named, est).is_err());
    }

    #[test]
    fn text_output_has_the_per_language_row_total_and_noise_line() {
        let dir = repo();
        let out = run_count(&CountArgs {
            repo: dir.path().to_path_buf(),
            json: false,
            exclude: Vec::new(),
            repo_name: None,
        })
        .unwrap();
        assert!(out.contains("Rust"), "{out}");
        assert!(out.contains("TOTAL"), "{out}");
        assert!(out.contains("excluded as noise: 1 files, 2 lines"), "{out}");
    }

    #[test]
    fn json_output_carries_the_counted_and_excluded_figures() {
        let dir = repo();
        let out = run_count(&CountArgs {
            repo: dir.path().to_path_buf(),
            json: true,
            exclude: Vec::new(),
            repo_name: None,
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["total_files"], 1);
        assert_eq!(v["total_lines"], 2);
        assert_eq!(v["languages"][0]["language"], "Rust");
        assert_eq!(v["excluded_noise_files"], 1);
        assert_eq!(v["excluded_noise_lines"], 2);
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_an_error() {
        let dir = repo();
        let err = run_count(&CountArgs {
            repo: dir.path().join("a.rs"),
            json: false,
            exclude: Vec::new(),
            repo_name: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }
}
