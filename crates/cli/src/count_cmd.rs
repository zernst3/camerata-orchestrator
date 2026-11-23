//! `camerata count <repo-path> [--json]`: how big a repo is as the inspection sees it.
//!
//! No model, no network, seconds to run. It is a thin shell over
//! [`camerata_server::onboard::count::count_repo`], which reads the repo through the
//! inspection's own file filter, so the number printed here is the number `camerata inspect`
//! is priced on. Output is a plain-text table, or the same figures as JSON with `--json`.

use std::path::PathBuf;

use camerata_server::onboard::count::count_repo;
use camerata_server::onboard::scope::ScanScope;

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

/// The report to print for `args.repo`. Errors when the path is not a directory.
pub fn run_count(args: &CountArgs) -> anyhow::Result<String> {
    if !args.repo.is_dir() {
        anyhow::bail!("{} is not a directory", args.repo.display());
    }
    let scope =
        ScanScope::from_patterns(&args.exclude).map_err(|e| anyhow::anyhow!("--exclude: {e}"))?;
    let name = crate::inspect_cmd::source_name(&args.repo, args.repo_name.as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;
    let count = count_repo(&args.repo, &scope.for_repo(&name))?;
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
    if args.json {
        Ok(serde_json::to_string_pretty(&count)?)
    } else {
        Ok(count.to_text())
    }
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
