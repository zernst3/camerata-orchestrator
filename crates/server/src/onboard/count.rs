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
use super::scope::RepoScope;

/// Counted files and lines for one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
    pub lines: usize,
}

/// Counted files and lines of one kind of file that has no recognized language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OtherKindCount {
    /// "config", "docs", "templates", "styles" or "build files".
    pub kind: &'static str,
    pub files: usize,
    pub lines: usize,
}

/// The kind a counted file with no recognized language is shown under, in the order a quote lists
/// them. Taken from the same [`super::files::text_kind`] admission and routing use.
const OTHER_KINDS: [&str; 5] = ["config", "docs", "templates", "styles", "build files"];

fn other_kind_of(path: &str) -> &'static str {
    use super::files::TextKind;
    match super::files::text_kind(path) {
        None => "config",
        Some(TextKind::Docs) => "docs",
        Some(TextKind::Markup) => "templates",
        Some(TextKind::Styles) => "styles",
        Some(TextKind::Build) => "build files",
    }
}

/// The inspection's file filter applied to one repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoCount {
    /// Counted files and non-blank lines per recognized language, sorted by language.
    pub languages: Vec<LanguageCount>,
    /// Counted files with no recognized language (config, docs, templates, styles, build files,
    /// key material): the total, and the same files split by kind below.
    pub other_files: usize,
    pub other_lines: usize,
    /// The other files by kind, in the order a quote lists them; a kind with no file is absent.
    pub other_by_kind: Vec<OtherKindCount>,
    /// Everything the inspection reads: the languages plus the other bucket.
    pub total_files: usize,
    pub total_lines: usize,
    /// Files the noise denylist dropped (build output, lockfiles, vendored and generated code),
    /// and the non-blank lines in them.
    pub excluded_noise_files: usize,
    pub excluded_noise_lines: usize,
    /// Files that declare themselves generated (a tool's marker in a comment near the top): not
    /// read for the inspection, so not in the total above and not billed; the secret checks still
    /// search them. Counted here, with their non-blank lines, so a quote shows what was left out.
    pub generated_files: usize,
    pub generated_lines: usize,
    /// The file cap was hit: the counts cover only the first files read.
    pub truncated: bool,
    /// Every file the inspection does NOT read, grouped by why, so a quote is set from the
    /// billable total above and the reader sees what was left out. Reasons, in this order:
    /// excluded by agreement (the project's exclusion list), over the read limit, beyond the
    /// file limit, not UTF-8 text, a long line, generated, a file type not inspected yet. Files
    /// of a type not inspected yet are never in the billed total. A file over the read limit
    /// appears here only when the count runs before the file is excluded; the inspection itself starts with a warning. Empty when every admitted file is read.
    pub not_read: Vec<NotReadGroup>,
    /// Patterns of Excluded by agreement that matched no file in this repo. `camerata count`
    /// refuses to print figures while any is left, naming it.
    pub unmatched_patterns: Vec<String>,
    /// Patterns written for another repo of the project: not judged here (so a project-wide list
    /// works repo by repo for a quote) and listed as "not for this repo".
    pub not_for_this_repo: Vec<String>,
    /// Patterns for this repo that match only files never read anyway: a warning, never a
    /// refusal. Listed as "excluded by agreement; nothing here is read by default".
    pub never_read_patterns: Vec<String>,
    /// Operator only: the files that are neither read, listed as not read, noise nor excluded,
    /// counted by type (".png", "Gemfile"), most files first. A type nothing claims shows here on
    /// the first count of a repo instead of staying silent. Never part of client text.
    pub unclassified_types: Vec<UnclassifiedType>,
}

/// One type of file nothing claims, with how many files of it the walk met.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnclassifiedType {
    /// The extension with a dot (".xyz"), or the whole name of a file with none.
    pub extension: String,
    pub files: usize,
}

/// The files not read for one reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotReadGroup {
    /// The reason, as Coverage words it ([`crate::scan_ledger::SkipReason::label`]).
    pub reason: String,
    /// The files, in path order, with their size when it could be measured.
    pub files: Vec<NotReadFile>,
}

/// One file not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotReadFile {
    pub path: String,
    pub bytes: Option<u64>,
}

/// Count `root` through the inspection's own file filter. Blocking IO.
pub fn count_repo(root: &Path, scope: &RepoScope) -> anyhow::Result<RepoCount> {
    let extracted = read_local_repo_files(root, scope)?;
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

    let mut kinds: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    for (path, content) in &extracted.files {
        if lang_for_ext(path).is_none() {
            let entry = kinds.entry(other_kind_of(path)).or_default();
            entry.0 += 1;
            entry.1 += non_blank_lines(content);
        }
    }
    let other_by_kind: Vec<OtherKindCount> = OTHER_KINDS
        .into_iter()
        .filter_map(|kind| {
            kinds
                .get(kind)
                .map(|&(files, lines)| OtherKindCount { kind, files, lines })
        })
        .collect();

    let excluded_noise_lines = excluded_noise_lines(root, &extracted.excluded_noise_paths);
    let not_read = not_read_groups(&extracted);

    Ok(RepoCount {
        languages,
        other_files: extracted.files.len() - lang_files,
        other_lines: total_lines - lang_lines,
        other_by_kind,
        total_files: extracted.files.len(),
        total_lines,
        excluded_noise_files: extracted.excluded_noise,
        excluded_noise_lines,
        generated_files: extracted.generated.len(),
        generated_lines: non_blank_lines_in_files(root, &extracted.generated),
        truncated: extracted.truncated,
        not_read,
        unclassified_types: extracted
            .unclassified_types
            .iter()
            .map(|(extension, files)| UnclassifiedType {
                extension: extension.clone(),
                files: *files,
            })
            .collect(),
        unmatched_patterns: scope
            .patterns()
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                !extracted.matched_patterns.contains(i)
                    && scope.is_for_this_repo(p)
                    && super::scope::never_read_folder(
                        p,
                        std::slice::from_ref(&scope.spec().to_string()),
                    )
                    .is_none()
            })
            .map(|(_, p)| p.clone())
            .collect(),
        never_read_patterns: scope
            .patterns()
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                !extracted.matched_patterns.contains(i)
                    && scope.is_for_this_repo(p)
                    && super::scope::never_read_folder(
                        p,
                        std::slice::from_ref(&scope.spec().to_string()),
                    )
                    .is_some()
            })
            .map(|(_, p)| p.clone())
            .collect(),
        not_for_this_repo: scope
            .patterns()
            .iter()
            .filter(|p| !scope.is_for_this_repo(p))
            .cloned()
            .collect(),
    })
}

/// The files the walk named but did not read, grouped by reason in the order Coverage uses.
fn not_read_groups(extracted: &super::files::ExtractedRepo) -> Vec<NotReadGroup> {
    use crate::scan_ledger::SkipReason;
    let (plain, versions) = super::files::split_not_inspected(&extracted.not_inspected);
    [
        (
            SkipReason::ExcludedByAgreement,
            &extracted.excluded_by_agreement,
        ),
        (SkipReason::Oversize, &extracted.skipped_oversize),
        (SkipReason::BeyondFileCap, &extracted.skipped_beyond_cap),
        (SkipReason::NotText, &extracted.skipped_unreadable),
        (SkipReason::LongLine, &extracted.skipped_long_line),
        (SkipReason::Generated, &extracted.generated),
        (SkipReason::NotInspectedType, &plain),
        (SkipReason::NotInspectedVersionsOnly, &versions),
    ]
    .into_iter()
    .filter(|(_, paths)| !paths.is_empty())
    .map(|(reason, paths)| {
        let mut files: Vec<NotReadFile> = paths
            .iter()
            .map(|path| NotReadFile {
                path: path.clone(),
                bytes: extracted.skipped_sizes.get(path).copied(),
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        NotReadGroup {
            reason: reason.label(),
            files,
        }
    })
    .collect()
}

/// The non-blank lines in the files the noise filter dropped (`ExtractedRepo::
/// excluded_noise_paths`, relative to `root`). Shared by `camerata count` and the inspection
/// (`audit_repos` records it on the report for the export's manifest), so both state the same
/// figure. A file that cannot be read counts zero. Blocking IO.
pub(crate) fn excluded_noise_lines(root: &Path, excluded_noise_paths: &[String]) -> usize {
    non_blank_lines_in_files(root, excluded_noise_paths)
}

/// The non-blank lines in the files at `paths` (relative to `root`), counted as
/// [`excluded_noise_lines`] counts: a file that cannot be read counts zero. Also used for the
/// generated files, which are not read for the inspection but are sized for the quote.
pub fn non_blank_lines_in_files(root: &Path, paths: &[String]) -> usize {
    paths
        .iter()
        .filter_map(|rel| std::fs::read(root.join(rel)).ok())
        .map(|bytes| non_blank_lines(&String::from_utf8_lossy(&bytes)))
        .sum()
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
        for k in &self.other_by_kind {
            out.push_str(&format!(
                "{:<18} {:>6} {:>11}\n",
                format!("other: {}", k.kind),
                k.files,
                k.lines
            ));
        }
        out.push_str(&format!(
            "{:<18} {:>6} {:>11}\n",
            "TOTAL", self.total_files, self.total_lines
        ));
        out.push_str(&format!(
            "excluded as noise: {} files, {} lines\n",
            self.excluded_noise_files, self.excluded_noise_lines
        ));
        out.push_str(&format!(
            "generated, not billed: {} files, {} lines\n",
            self.generated_files, self.generated_lines
        ));
        if self.truncated {
            out.push_str("note: file cap reached, counts are partial\n");
        }
        for p in &self.never_read_patterns {
            out.push_str(&format!(
                "warning: {p} matches only files never read; excluded by agreement; nothing here is read by default\n"
            ));
        }
        for p in &self.not_for_this_repo {
            out.push_str(&format!("not for this repo: {p}\n"));
        }
        let not_inspected = [
            crate::onboard::files::not_inspected_label(),
            crate::onboard::files::not_inspected_versions_label(),
        ];
        for group in &self.not_read {
            let noun = if group.files.len() == 1 {
                "file"
            } else {
                "files"
            };
            // A file type not inspected yet is one line: how many of each type, then its paths
            // capped as the pre-start check caps them. Never each file's size.
            if not_inspected.contains(&group.reason) {
                let paths: Vec<String> = group.files.iter().map(|f| f.path.clone()).collect();
                let by_type = super::files::type_tally(&paths)
                    .iter()
                    .map(|(ty, n)| format!("{n} {ty}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "not read, {} ({} {noun}, by type: {by_type}): {}\n",
                    group.reason,
                    group.files.len(),
                    camerata_ui_core::readiness::capped_paths(&paths)
                ));
                continue;
            }
            out.push_str(&format!(
                "not read, {} ({} {noun}):\n",
                group.reason,
                group.files.len(),
            ));
            for f in &group.files {
                match f.bytes {
                    Some(bytes) => out.push_str(&format!(
                        "  {} ({})\n",
                        f.path,
                        crate::report_export::coverage_gaps::format_size(bytes)
                    )),
                    None => out.push_str(&format!("  {}\n", f.path)),
                }
            }
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

    /// Fix D: an over-limit file is searched for secrets but is not read for the inspection, so
    /// the quote count is the same with it as without it. The numbers are pinned.
    #[test]
    fn an_over_limit_file_does_not_change_the_quote_count() {
        let dir = git_fixture();
        write(dir.path(), "data/dump.js", &"var a = 1;\n".repeat(150_000));
        git(dir.path(), &["add", "."]);
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
        assert_eq!((c.total_files, c.total_lines), (5, 10));
        assert_eq!((c.other_files, c.other_lines), (2, 5));
        assert_eq!((c.excluded_noise_files, c.excluded_noise_lines), (3, 6));
        assert_eq!(c.not_read.len(), 1);
        assert_eq!(c.not_read[0].files[0].path, "data/dump.js");
    }

    #[test]
    fn counts_the_filtered_files_exactly_in_a_git_repo() {
        let dir = git_fixture();
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
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
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
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
        let extracted =
            read_local_repo_files(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
        assert_eq!(c.total_files, extracted.files.len());
        assert_eq!(c.excluded_noise_files, extracted.excluded_noise);
        assert_eq!(c.total_lines, count_source_lines(&extracted.files).0);
    }

    #[test]
    fn count_reports_dart_as_a_language_and_rules_files_as_other() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        write(dir.path(), "lib/app.dart", "void main() {\n  run();\n}\n"); // 3
        write(
            dir.path(),
            "firestore.rules",
            "rules_version = '2';\nservice cloud.firestore {\n}\n\n// c\n",
        ); // other, 4
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
        let dart = c.languages.iter().find(|l| l.language == "Dart").unwrap();
        assert_eq!((dart.files, dart.lines), (1, 3));
        // fixture(): config.yaml 3 + .gitignore 2 + secret.env 1 (not a git tree, so counted),
        // plus the 4 rules lines.
        assert_eq!(c.other_lines, 3 + 2 + 1 + 4);
        assert!(c.to_text().contains("Dart"));
    }

    #[test]
    fn json_has_the_documented_shape() {
        let dir = git_fixture();
        let v = serde_json::to_value(
            count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap(),
        )
        .unwrap();
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
        let text = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none())
            .unwrap()
            .to_text();
        assert!(text.contains("Rust"));
        assert!(text.contains("TOTAL"));
        assert!(text.contains("excluded as noise: 3 files, 6 lines"));
    }

    // ── Item 31 step 7: what is not read, with the reason, from the run's own file list ──────

    fn scope_of(patterns: &[&str]) -> RepoScope {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        crate::onboard::scope::ScanScope::from_patterns(&patterns)
            .unwrap()
            .for_repo("acme/demo")
    }

    #[test]
    fn the_count_lists_every_file_not_read_grouped_by_reason() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}\n\nfn a() {}\n");
        write(dir.path(), "seeds/rows.js", "var a;\nvar b;\n");
        write(dir.path(), "seeds/more.js", "var c;\n");
        write(
            dir.path(),
            "src/huge.rs",
            &"x".repeat(super::super::files::MAX_FILE_BYTES + 1),
        );
        let c = count_repo(dir.path(), &scope_of(&["demo/seeds/**"])).unwrap();
        assert_eq!(
            (c.total_files, c.total_lines),
            (1, 2),
            "only what the run reads is billed"
        );
        let reasons: Vec<(&str, Vec<&str>)> = c
            .not_read
            .iter()
            .map(|g| {
                (
                    g.reason.as_str(),
                    g.files.iter().map(|f| f.path.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            reasons,
            [
                (
                    "excluded by agreement",
                    vec!["seeds/more.js", "seeds/rows.js"]
                ),
                ("over the 1 MB file size limit", vec!["src/huge.rs"]),
            ]
        );
        let text = c.to_text();
        assert!(
            text.contains("not read, excluded by agreement (2 files):"),
            "{text}"
        );
        assert!(text.contains("  seeds/rows.js (14 B)"), "{text}");
        assert!(
            text.contains("not read, over the 1 MB file size limit (1 file):"),
            "{text}"
        );
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["not_read"][0]["reason"], "excluded by agreement");
        assert_eq!(v["not_read"][0]["files"][0]["path"], "seeds/more.js");
    }

    /// V1 item 32: generated files are listed (count, lines and each path) and are not in the
    /// billed total.
    #[test]
    fn the_count_lists_generated_files_with_lines_and_does_not_bill_them() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        write(dir.path(), "README.md", "# Title\n\ntext\n");
        write(
            dir.path(),
            "Migrations/Init.Designer.cs",
            "// <auto-generated />\nclass A {}\n\nclass B {}\n",
        );
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
        assert_eq!(
            (c.total_files, c.total_lines),
            (2, 3),
            "billed: main.rs and README.md"
        );
        assert_eq!((c.generated_files, c.generated_lines), (1, 3));
        assert_eq!(c.not_read.len(), 1);
        assert_eq!(
            c.not_read[0].reason,
            "generated: searched for secrets only, not billed"
        );
        assert_eq!(c.not_read[0].files[0].path, "Migrations/Init.Designer.cs");
        assert!(c
            .to_text()
            .contains("generated, not billed: 1 files, 3 lines"));
    }

    /// Round 1: the files with no recognized language are shown by kind, not all as "config".
    #[test]
    fn the_other_files_are_split_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        write(dir.path(), "config.yaml", "a: 1\nb: 2\n"); // config, 2
        write(dir.path(), "README.md", "# T\n\ntext\n"); // docs, 2
        write(dir.path(), "views/p.ejs", "<p></p>\n"); // templates, 1
        write(dir.path(), "public/i.html", "<p></p>\n<p></p>\n"); // templates, 2
        write(dir.path(), "site.css", "a { b: c }\n"); // styles, 1
        write(dir.path(), "Makefile", "all:\n\ttrue\n"); // build files, 2
        let c = count_repo(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
        let kinds: Vec<(&str, usize, usize)> = c
            .other_by_kind
            .iter()
            .map(|k| (k.kind, k.files, k.lines))
            .collect();
        assert_eq!(
            kinds,
            [
                ("config", 1, 2),
                ("docs", 1, 2),
                ("templates", 2, 3),
                ("styles", 1, 1),
                ("build files", 1, 2),
            ]
        );
        // The kinds add up to the one other bucket, so the total is unchanged.
        assert_eq!(c.other_files, 6);
        assert_eq!(c.other_lines, 10);
        assert_eq!(c.total_files, 7);
        let text = c.to_text();
        assert!(text.contains("other: templates"), "{text}");
        assert!(!text.contains("other (config)"), "{text}");
    }

    #[test]
    fn a_count_with_nothing_left_out_has_no_not_read_section() {
        let dir = git_fixture();
        let c = count_repo(dir.path(), &RepoScope::none()).unwrap();
        assert!(c.not_read.is_empty());
        assert!(!c.to_text().contains("not read"));
    }

    #[test]
    fn the_count_total_is_the_inspections_total_with_an_exclusion() {
        let dir = git_fixture();
        write(
            dir.path(),
            "seeds/big.ts",
            "export const rows = 1;\n\nexport const more = 2;\n",
        );
        let scope = scope_of(&["demo/seeds/**"]);
        let extracted = read_local_repo_files(dir.path(), &scope).unwrap();
        let c = count_repo(dir.path(), &scope).unwrap();
        assert_eq!(c.total_files, extracted.files.len());
        assert_eq!(c.total_lines, count_source_lines(&extracted.files).0);
        assert!(extracted
            .files
            .iter()
            .all(|(p, _)| !p.starts_with("seeds/")));
        let without = count_repo(dir.path(), &RepoScope::none()).unwrap();
        assert_eq!(
            without.total_lines,
            c.total_lines + 2,
            "the excluded lines are not billed"
        );
    }
}
