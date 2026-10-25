//! `camerata count`: the size of a repo as the inspection sees it, with no model and no network.
//!
//! This is the number a quote is priced on, so it must be the inspection's own number. It calls
//! [`read_local_repo_files`] (gitignore + noise denylist + extension allowlist + the
//! `git ls-files` union), the single function `audit_repos` reads a repo through, and counts
//! lines with [`non_blank_lines`], the definition the inspection's volume figures use. Nothing
//! here re-implements either; a client-side `cloc` differs because it counts generated and
//! vendored files this filter drops.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::files::read_local_repo_files;
use super::propose::{count_source_lines, lang_for_ext, non_blank_lines};

/// Counted files and lines for one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
    pub lines: usize,
}

/// The inspection's file filter applied to one repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoCount {
    /// Counted files and non-blank lines per recognized language, sorted by language.
    pub languages: Vec<LanguageCount>,
    /// Counted files with no recognized language (config, text, key material).
    pub other_files: usize,
    pub other_lines: usize,
    /// Everything the inspection reads: the languages plus the other bucket.
    pub total_files: usize,
    pub total_lines: usize,
    /// Files the noise denylist dropped (build output, lockfiles, vendored and generated code),
    /// and the non-blank lines in them.
    pub excluded_noise_files: usize,
    pub excluded_noise_lines: usize,
    /// The file cap was hit: the counts cover only the first files read.
    pub truncated: bool,
}

/// Count `root` through the inspection's own file filter. Blocking IO.
pub fn count_repo(root: &Path) -> anyhow::Result<RepoCount> {
    let extracted = read_local_repo_files(root)?;
    let (total_lines, lines_by_lang) = count_source_lines(&extracted.files);

    let mut files_by_lang: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (path, _) in &extracted.files {
        if let Some(lang) = lang_for_ext(path) {
            *files_by_lang.entry(lang).or_insert(0) += 1;
        }
    }
    let languages: Vec<LanguageCount> = lines_by_lang
        .iter()
        .map(|(language, lines)| LanguageCount {
            language: language.clone(),
            files: files_by_lang.get(language.as_str()).copied().unwrap_or(0),
            lines: *lines,
        })
        .collect();
    let lang_files: usize = languages.iter().map(|l| l.files).sum();
    let lang_lines: usize = languages.iter().map(|l| l.lines).sum();

    let excluded_noise_lines = extracted
        .excluded_noise_paths
        .iter()
        .filter_map(|rel| std::fs::read(root.join(rel)).ok())
        .map(|bytes| non_blank_lines(&String::from_utf8_lossy(&bytes)))
        .sum();

    Ok(RepoCount {
        languages,
        other_files: extracted.files.len() - lang_files,
        other_lines: total_lines - lang_lines,
        total_files: extracted.files.len(),
        total_lines,
        excluded_noise_files: extracted.excluded_noise,
        excluded_noise_lines,
        truncated: extracted.truncated,
    })
}

impl RepoCount {
    /// The plain-text report.
    pub fn to_text(&self) -> String {
        let mut out = String::from("language            files       lines\n");
        for l in &self.languages {
            out.push_str(&format!(
                "{:<18} {:>6} {:>11}\n",
                l.language, l.files, l.lines
            ));
        }
        out.push_str(&format!(
            "{:<18} {:>6} {:>11}\n",
            "other (config)", self.other_files, self.other_lines
        ));
        out.push_str(&format!(
            "{:<18} {:>6} {:>11}\n",
            "TOTAL", self.total_files, self.total_lines
        ));
        out.push_str(&format!(
            "excluded as noise: {} files, {} lines\n",
            self.excluded_noise_files, self.excluded_noise_lines
        ));
        if self.truncated {
            out.push_str("note: file cap reached, counts are partial\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Code, a config file, a lockfile, a generated file, a vendored dir and a gitignored file.
    fn fixture(root: &Path) {
        write(root, "src/main.rs", "fn main() {}\n\nfn a() {}\n"); // 2 non-blank
        write(root, "src/lib.rs", "pub fn b() {}\n"); // 1
        write(root, "web/app.ts", "const x = 1;\nconst y = 2;\n"); // 2
        write(root, "config.yaml", "a: 1\nb: 2\nc: 3\n"); // other, 3
        write(root, "package-lock.json", "{\n}\n"); // noise, 2
        write(root, "web/api.gen.ts", "export {};\n"); // noise, 1
        write(root, "vendor/dep/lib.js", "var a;\nvar b;\nvar c;\n"); // noise, 3
        write(root, ".gitignore", "secret.env\nignored/\n"); // other, 2
        write(root, "secret.env", "TOKEN=1\n"); // gitignored
        write(root, "ignored/skip.rs", "fn skip() {}\n"); // gitignored
    }

    fn git(root: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn git_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "."]);
        dir
    }

    #[test]
    fn counts_the_filtered_files_exactly_in_a_git_repo() {
        let dir = git_fixture();
        let c = count_repo(dir.path()).unwrap();
        assert_eq!(
            c.languages,
            vec![
                LanguageCount {
                    language: "Rust".into(),
                    files: 2,
                    lines: 3
                },
                LanguageCount {
                    language: "TypeScript".into(),
                    files: 1,
                    lines: 2
                },
            ]
        );
        assert_eq!((c.other_files, c.other_lines), (2, 5)); // config.yaml + .gitignore
        assert_eq!((c.total_files, c.total_lines), (5, 10));
        assert_eq!((c.excluded_noise_files, c.excluded_noise_lines), (3, 6));
        assert!(!c.truncated);
    }

    #[test]
    fn counts_a_folder_that_is_not_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let c = count_repo(dir.path()).unwrap();
        // Without git the .gitignore is not applied: the ignored files count like any other.
        let langs: Vec<_> = c
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l.files, l.lines))
            .collect();
        assert_eq!(langs, vec![("Rust", 3, 4), ("TypeScript", 1, 2)]);
        // The vendored directory is pruned without being read, so only the two noise files outside it count.
        assert_eq!((c.excluded_noise_files, c.excluded_noise_lines), (2, 3));
    }

    #[test]
    fn counts_the_same_file_set_the_inspection_reads() {
        let dir = git_fixture();
        let extracted = read_local_repo_files(dir.path()).unwrap();
        let c = count_repo(dir.path()).unwrap();
        assert_eq!(c.total_files, extracted.files.len());
        assert_eq!(c.excluded_noise_files, extracted.excluded_noise);
        assert_eq!(c.total_lines, count_source_lines(&extracted.files).0);
    }

    #[test]
    fn json_has_the_documented_shape() {
        let dir = git_fixture();
        let v = serde_json::to_value(count_repo(dir.path()).unwrap()).unwrap();
        assert_eq!(v["total_files"], 5);
        assert_eq!(v["total_lines"], 10);
        assert_eq!(v["excluded_noise_files"], 3);
        assert_eq!(v["excluded_noise_lines"], 6);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["languages"][0]["language"], "Rust");
        assert_eq!(v["languages"][0]["files"], 2);
        assert_eq!(v["languages"][0]["lines"], 3);
    }

    #[test]
    fn text_report_names_languages_totals_and_noise() {
        let dir = git_fixture();
        let text = count_repo(dir.path()).unwrap().to_text();
        assert!(text.contains("Rust"));
        assert!(text.contains("TOTAL"));
        assert!(text.contains("excluded as noise: 3 files, 6 lines"));
    }
}
