//! M12: dependency manifests osv-scanner cannot audit, and files it could not read.
//!
//! osv-scanner reads LOCKFILES (and a few manifests such as `requirements.txt`, `pom.xml` and
//! `go.mod`). A `package.json` or `Cargo.toml` with no lockfile beside or above it declares
//! packages that are never checked, and a successful run said nothing about them. On the success
//! path the run also threw away osv-scanner's stderr, where a lockfile it failed to parse is
//! reported. This module finds both so the coverage note can name them.
//!
//! Deliberately NOT built on `results[].source.path`: osv-scanner leaves a lockfile out of
//! `results` when it holds no vulnerable package, so comparing manifests against `results` would
//! call every clean project unaudited. The check here is on the files themselves.

use std::path::{Path, PathBuf};

/// Directories never walked: dependencies, build output and VCS metadata.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    "Pods",
    "bower_components",
];

/// The most entries the walk visits, so a pathological tree cannot stall the scan.
const WALK_ENTRY_LIMIT: usize = 50_000;

/// The most manifests named in the note; the rest are counted.
const NAMED_LIMIT: usize = 15;

/// The most unreadable-file lines quoted from osv-scanner's stderr.
const STDERR_LINES: usize = 5;

/// How a manifest is covered.
struct Rule {
    /// The manifest's file name (or, with a leading `*`, its extension).
    manifest: &'static str,
    /// Lockfile names that cover it.
    locks: &'static [&'static str],
    /// True when a lockfile in an ancestor directory also covers it (a workspace root).
    ancestors: bool,
}

const RULES: &[Rule] = &[
    Rule {
        manifest: "package.json",
        locks: &[
            "package-lock.json",
            "npm-shrinkwrap.json",
            "yarn.lock",
            "pnpm-lock.yaml",
            "bun.lock",
            "bun.lockb",
        ],
        ancestors: true,
    },
    Rule {
        manifest: "Cargo.toml",
        locks: &["Cargo.lock"],
        ancestors: true,
    },
    Rule {
        manifest: "pyproject.toml",
        locks: &[
            "poetry.lock",
            "uv.lock",
            "pdm.lock",
            "Pipfile.lock",
            "requirements.txt",
        ],
        ancestors: false,
    },
    Rule {
        manifest: "Pipfile",
        locks: &["Pipfile.lock"],
        ancestors: false,
    },
    Rule {
        manifest: "Gemfile",
        locks: &["Gemfile.lock"],
        ancestors: true,
    },
    Rule {
        manifest: "composer.json",
        locks: &["composer.lock"],
        ancestors: true,
    },
    Rule {
        manifest: "build.gradle",
        locks: &["gradle.lockfile"],
        ancestors: true,
    },
    Rule {
        manifest: "build.gradle.kts",
        locks: &["gradle.lockfile"],
        ancestors: true,
    },
    Rule {
        manifest: "mix.exs",
        locks: &["mix.lock"],
        ancestors: true,
    },
    Rule {
        manifest: "pubspec.yaml",
        locks: &["pubspec.lock"],
        ancestors: true,
    },
    Rule {
        manifest: "*.csproj",
        locks: &["packages.lock.json"],
        ancestors: false,
    },
    Rule {
        manifest: "*.fsproj",
        locks: &["packages.lock.json"],
        ancestors: false,
    },
    Rule {
        manifest: "*.vbproj",
        locks: &["packages.lock.json"],
        ancestors: false,
    },
];

fn rule_for(file_name: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| match r.manifest.strip_prefix('*') {
        Some(ext) => file_name.ends_with(ext),
        None => file_name == r.manifest,
    })
}

/// True when the manifest declares at least one dependency, so a manifest with nothing in it
/// (a `package.json` holding only `"type": "module"`) is not reported.
fn declares_dependencies(path: &Path, file_name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        // Unreadable: say so rather than assume it is empty.
        return true;
    };
    match file_name {
        "package.json" => serde_json::from_str::<serde_json::Value>(&text)
            .map(|v| {
                [
                    "dependencies",
                    "devDependencies",
                    "optionalDependencies",
                    "peerDependencies",
                ]
                .iter()
                .any(|k| {
                    v.get(k)
                        .and_then(|d| d.as_object())
                        .is_some_and(|d| !d.is_empty())
                })
            })
            .unwrap_or(true),
        "Cargo.toml" => ["[dependencies", "[dev-dependencies", "[build-dependencies"]
            .iter()
            .any(|h| text.contains(h)),
        // A .NET project with no PackageReference has no NuGet packages to check.
        n if n.ends_with(".csproj") || n.ends_with(".fsproj") || n.ends_with(".vbproj") => {
            text.to_ascii_lowercase().contains("<packagereference")
        }
        "Gemfile" => text
            .lines()
            .any(|l| l.trim_start().starts_with("gem ") || l.trim_start().starts_with("gem(")),
        "pubspec.yaml" => {
            let lines: Vec<&str> = text.lines().collect();
            lines.iter().enumerate().any(|(i, l)| {
                matches!(l.trim_end(), "dependencies:" | "dev_dependencies:")
                    && lines[i + 1..]
                        .iter()
                        .find(|n| !n.trim().is_empty() && !n.trim_start().starts_with('#'))
                        .is_some_and(|n| n.starts_with(' ') || n.starts_with('\t'))
            })
        }
        _ => true,
    }
}

fn covered(dir: &Path, root: &Path, rule: &Rule) -> bool {
    let mut current = Some(dir);
    while let Some(d) = current {
        if rule.locks.iter().any(|l| d.join(l).is_file()) {
            return true;
        }
        if !rule.ancestors || d == root {
            return false;
        }
        current = d.parent();
    }
    false
}

/// The manifests under `repo_dir` that no lockfile covers, as repo-relative paths in sorted
/// order. Only files the walk can name are reported; a tree walk that hits the entry limit stops
/// there, so the list can undercount but never invents a path.
pub fn unaudited_manifests(repo_dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![repo_dir.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > WALK_ENTRY_LIMIT {
                return finish(found);
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            let Some(rule) = rule_for(&name) else {
                continue;
            };
            if covered(&dir, repo_dir, rule) || !declares_dependencies(&path, &name) {
                continue;
            }
            let rel = path
                .strip_prefix(repo_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            found.push(rel);
        }
    }
    finish(found)
}

fn finish(mut found: Vec<String>) -> Vec<String> {
    found.sort();
    found.dedup();
    found
}

/// The sentence for [`crate::dep_audit::MANIFESTS_UNAUDITED`], or `None` when nothing is left
/// out. Names the first [`NAMED_LIMIT`] manifests and counts the rest.
pub fn manifests_sentence(manifests: &[String]) -> Option<String> {
    if manifests.is_empty() {
        return None;
    }
    let named: Vec<&str> = manifests
        .iter()
        .take(NAMED_LIMIT)
        .map(String::as_str)
        .collect();
    let more = manifests.len().saturating_sub(NAMED_LIMIT);
    let mut list = named.join(", ");
    if more > 0 {
        list.push_str(&format!(" and {more} more"));
    }
    Some(format!(
        "{} (no lockfile): {list}. osv-scanner reads lockfiles, so the packages these declare \
         were not checked for known vulnerabilities.",
        crate::dep_audit::MANIFESTS_UNAUDITED
    ))
}

/// The prefixes osv-scanner puts on a stderr line that reports a problem reading a file. A line
/// is quoted only when it STARTS with one of these, so a progress line that merely contains the
/// word "error" (a path such as `error-handling/package-lock.json`) is not mistaken for one.
const ERROR_PREFIXES: &[&str] = &[
    "Error during extraction",
    "Error:",
    "Error ",
    "error:",
    "ERROR",
    "Failed to ",
    "failed to ",
];

/// Remove every local filesystem path from `text`. The clone directory (as given and as
/// canonicalised, since `/tmp` may resolve to `/private/tmp`) becomes `.`, so a path inside it
/// reads as a repository-relative one; any other absolute path (the tooling directory, a home
/// directory, a Windows drive path) is cut down to its file name. URLs and relative paths are
/// left alone.
pub fn scrub_local_paths(text: &str, repo_dir: &Path) -> String {
    let mut roots: Vec<String> = vec![repo_dir.to_string_lossy().into_owned()];
    if let Ok(canonical) = std::fs::canonicalize(repo_dir) {
        roots.push(canonical.to_string_lossy().into_owned());
    }
    roots.retain(|r| r.len() > 1);
    roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
    roots.dedup();
    let mut out = text.to_string();
    for root in &roots {
        out = out
            .replace(&format!("{root}/"), "./")
            .replace(root.as_str(), ".");
    }
    out.split_inclusive(char::is_whitespace)
        .map(scrub_token)
        .collect()
}

/// Cut the absolute path inside one whitespace-delimited token down to its file name.
fn scrub_token(token: &str) -> String {
    const OPENERS: &str = "(\"'=[<";
    const CLOSERS: &str = ")]\"',;:.>";
    let trailing_ws = token.len() - token.trim_end().len();
    let (body, ws) = token.split_at(token.len() - trailing_ws);
    let bytes: Vec<char> = body.chars().collect();
    let mut start = None;
    for (i, c) in bytes.iter().enumerate() {
        let after_opener = i == 0 || OPENERS.contains(bytes[i - 1]);
        if !after_opener {
            continue;
        }
        let unix = *c == '/' && bytes.get(i + 1) != Some(&'/');
        let windows = c.is_ascii_alphabetic()
            && bytes.get(i + 1) == Some(&':')
            && matches!(bytes.get(i + 2), Some('\\') | Some('/'));
        if unix || windows {
            start = Some(i);
            break;
        }
    }
    let Some(start) = start else {
        return token.to_string();
    };
    let rest: String = bytes[start..].iter().collect();
    let trimmed = rest.trim_end_matches(|c| CLOSERS.contains(c));
    let tail = &rest[trimmed.len()..];
    let name = trimmed
        .rsplit(['/', '\\'])
        .find(|p| !p.is_empty())
        .unwrap_or("path");
    let head: String = bytes[..start].iter().collect();
    format!("{head}{name}{tail}{ws}")
}

/// The lines of osv-scanner's stderr that report a problem reading a file, with every local
/// path removed, as the sentence for [`crate::dep_audit::UNREAD_FILES`]; `None` when stderr
/// holds no such line. A lockfile osv-scanner failed to parse was not audited, and a clean exit
/// code does not say so.
pub fn unread_files_sentence(stderr: &str, repo_dir: &Path) -> Option<String> {
    let lines: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| ERROR_PREFIXES.iter().any(|p| l.starts_with(p)))
        .take(STDERR_LINES)
        .map(|l| {
            let scrubbed = scrub_local_paths(l, repo_dir);
            scrubbed.chars().take(200).collect::<String>()
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "{}: osv-scanner reported problems reading some files, so they may not have been \
         checked ({}).",
        crate::dep_audit::UNREAD_FILES,
        lines.join(" | ")
    ))
}

/// The coverage message for a SUCCESSFUL run: the unaudited-manifests sentence and the
/// unread-files sentence joined, or `None` when the run left nothing unchecked.
pub fn success_gap_message(
    manifests_sentence: Option<String>,
    stderr: &str,
    repo_dir: &Path,
) -> Option<String> {
    let parts: Vec<String> = manifests_sentence
        .into_iter()
        .chain(unread_files_sentence(stderr, repo_dir))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const DEPS: &str = r#"{"dependencies":{"left-pad":"1.0.0"}}"#;

    #[test]
    fn a_package_json_with_no_lockfile_is_reported_and_one_with_a_lockfile_is_not() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", DEPS);
        assert_eq!(unaudited_manifests(dir.path()), ["package.json"]);
        write(dir.path(), "package-lock.json", "{}");
        assert!(unaudited_manifests(dir.path()).is_empty());
    }

    #[test]
    fn a_workspace_member_is_covered_by_the_root_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Cargo.lock", "");
        write(
            dir.path(),
            "crates/a/Cargo.toml",
            "[package]\nname=\"a\"\n[dependencies]\nserde=\"1\"\n",
        );
        assert!(unaudited_manifests(dir.path()).is_empty());
        let lone = tempfile::tempdir().unwrap();
        write(
            lone.path(),
            "crates/a/Cargo.toml",
            "[package]\nname=\"a\"\n[dependencies]\nserde=\"1\"\n",
        );
        assert_eq!(unaudited_manifests(lone.path()), ["crates/a/Cargo.toml"]);
    }

    #[test]
    fn dependency_folders_and_empty_manifests_are_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "node_modules/x/package.json", DEPS);
        write(dir.path(), "target/y/Cargo.toml", "[dependencies]\n");
        write(dir.path(), "package.json", r#"{"type":"module"}"#);
        write(dir.path(), "tools/Cargo.toml", "[package]\nname=\"t\"\n");
        assert!(unaudited_manifests(dir.path()).is_empty());
    }

    #[test]
    fn a_dotnet_project_needs_its_own_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/App/App.csproj",
            "<Project><ItemGroup><PackageReference Include=\"X\"/></ItemGroup></Project>",
        );
        write(dir.path(), "packages.lock.json", "{}");
        assert_eq!(unaudited_manifests(dir.path()), ["src/App/App.csproj"]);
        write(dir.path(), "src/App/packages.lock.json", "{}");
        assert!(unaudited_manifests(dir.path()).is_empty());
    }

    #[test]
    fn the_sentence_names_the_first_fifteen_and_counts_the_rest() {
        let many: Vec<String> = (0..20).map(|i| format!("p{i:02}/package.json")).collect();
        let s = manifests_sentence(&many).unwrap();
        assert!(s.starts_with("Dependency manifests not audited (no lockfile): p00/package.json"));
        assert!(s.contains("p14/package.json and 5 more."), "{s}");
        assert!(!s.contains("p15/package.json"));
        assert!(manifests_sentence(&[]).is_none());
    }

    #[test]
    fn stderr_problems_are_quoted_without_the_local_path() {
        let stderr = "Scanned /tmp/clone/package-lock.json file and found 3 packages\n\
                      Error during extraction: could not parse /tmp/clone/yarn.lock\n";
        let s = unread_files_sentence(stderr, Path::new("/tmp/clone")).unwrap();
        assert!(s.starts_with("Dependency files not read:"), "{s}");
        assert!(s.contains("could not parse ./yarn.lock"), "{s}");
        assert!(!s.contains("/tmp/clone"), "{s}");
        assert!(!s.contains("found 3 packages"), "{s}");
        assert!(unread_files_sentence("Scanned a file\n", Path::new("/x")).is_none());
    }

    #[test]
    fn a_project_that_declares_nothing_is_not_called_unchecked() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a/A.csproj",
            "<Project Sdk=\"x\"><PropertyGroup/></Project>",
        );
        write(dir.path(), "b/Gemfile", "source 'https://rubygems.org'\n");
        write(dir.path(), "c/pubspec.yaml", "name: c\nversion: 1.0.0\n");
        assert!(unaudited_manifests(dir.path()).is_empty());

        write(
            dir.path(),
            "a/A.csproj",
            "<Project><ItemGroup><PackageReference Include=\"X\" Version=\"1\"/></ItemGroup></Project>",
        );
        write(dir.path(), "b/Gemfile", "source 'x'\ngem 'rails'\n");
        write(
            dir.path(),
            "c/pubspec.yaml",
            "name: c\ndependencies:\n  http: ^1.0.0\n",
        );
        assert_eq!(
            unaudited_manifests(dir.path()),
            ["a/A.csproj", "b/Gemfile", "c/pubspec.yaml"]
        );
    }

    #[test]
    fn a_clean_successful_run_has_no_gap_message_and_a_gappy_one_is_not_clean() {
        assert_eq!(
            success_gap_message(None, "Scanned a file\n", Path::new("/x")),
            None
        );
        let message = success_gap_message(
            manifests_sentence(&["package.json".to_string()]),
            "error: bad lock\n",
            Path::new("/x"),
        )
        .unwrap();
        let note = crate::onboard::CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "osv-scanner".to_string(),
            message,
        };
        // osv-scanner ran (so KEV and EPSS enrichment still runs) but the check is not complete.
        assert!(crate::dep_audit::note_means_scanned(&note));
        assert!(!crate::dep_audit::note_proves_clean(&note));
        assert!(note.message.contains("package.json"));
        assert!(note.message.contains("error: bad lock"));
    }
}
