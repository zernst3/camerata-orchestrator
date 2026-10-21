//! Local repo file reading: walks a working tree, prunes noise, and returns auditable files —
//! both code (full treatment: stack detection, architectural checkers, AI review) and
//! non-code human-authored text/config where a committed secret is just as real a finding
//! (`.env*`, `.yaml`/`.json`/`.toml`, `.conf`/`.properties`, key material, dotfiles). See
//! `is_admissible_text` for the admission policy and why broadening it doesn't create noise
//! elsewhere.

/// Safety net for pathological monorepos so one scan can't exhaust memory. This
/// is NOT a per-scan window that rotates: a single tarball download covers the
/// WHOLE repo, and only a repo with more than this many auditable files is
/// truncated (and the report says so). Normal repos are fully scanned.
pub(crate) const HARD_CAP_FILES: usize = 20_000;
/// Skip files larger than this (likely generated/vendored/binary).
pub(crate) const MAX_FILE_BYTES: usize = 400_000;

/// Extensions worth auditing (source + config text). Keeps the scan off images,
/// lockfiles, and binaries.
const CODE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb", "php", "cs", "sql", "toml", "yaml",
    "yml", "json", "sh", "env", "cfg", "ini", "tf", "kt", "swift", "c", "cpp", "h",
    // IaC: Terragrunt/Packer HCL, Azure Bicep (Terraform `.tf` is already above).
    "hcl", "bicep",
];

/// Extensionless basenames that still carry stack/CI signal and must be extracted so
/// detection can see them (e.g. a Jenkins pipeline). Without this they'd be dropped as
/// "no code extension" and the CI/CD domain would never be detected from them.
const CODE_BASENAMES: &[&str] = &["Jenkinsfile", "Dockerfile"];

/// Extensions that are not "code" in the AST/style sense, but are human-authored config/text
/// files where a committed secret is exactly as real a finding as it is in a `.env`:
/// connection strings in `.properties`/`.conf` files, PEM/PKCS12/JKS key material, and plain
/// `.txt` notes (a `credentials.txt` committed by accident is precisely the mistake this list
/// exists to catch). Deliberately does NOT include anything already covered by `CODE_EXTS`.
///
/// This list feeds `is_admissible_text` — see that function's doc for why admitting these
/// extensions into the file list doesn't create noise on the architectural/style rules or the
/// scan-tool previews: those consumers already self-scope by their own file-type interest.
const CONFIG_TEXT_EXTS: &[&str] = &[
    "conf",
    "properties",
    "txt",
    "pem",
    "crt",
    "cert",
    "p12",
    "pfx",
    "jks",
    "keystore",
    "pgpass",
];

/// Directory names that are build output, dependency trees, caches, or tool state — pure
/// noise for an architecture audit, and the bulk of a repo's bytes/tokens. A real consumer
/// found 14 of 25 MB of one monorepo was `.turbo/cache` manifests + lockfiles; scanning
/// that is paying to audit generated artifacts. Matched on ANY path segment, so
/// `apps/web/node_modules/...` and `node_modules/...` both prune. Extend per-project via
/// the `CAMERATA_SCAN_EXCLUDE_DIRS` env (comma-separated extra dir names).
///
/// `bin` is deliberately NOT here. It is pruned only as .NET build output (`bin/Debug`,
/// `bin/Release`, see [`DOTNET_BUILD_CONFIGS`]), because a bare `bin` is also where Rust keeps
/// its binary targets (`src/bin/*.rs`) and where Node and shell projects keep their executable
/// scripts, which are hand-written source and, in a shell script, a very common place for a
/// committed credential.
const NOISE_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "jspm_packages",
    ".yarn",
    ".pnpm-store",
    ".git",
    ".svn",
    ".hg",
    "target",
    "dist",
    "build",
    "out",
    "obj",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".angular",
    ".expo",
    ".docusaurus",
    "storybook-static",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".serverless",
    "coverage",
    ".nyc_output",
    "vendor",
    "Pods",
    "DerivedData",
    ".dart_tool",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".gradle",
    ".terraform",
    ".terragrunt-cache",
    ".idea",
    ".vscode",
    // Generated-code + test-artifact dirs (codegen output, snapshot fixtures).
    "generated",
    "__generated__",
    "__snapshots__",
    "node_modules.bin",
];

/// The build-configuration folders .NET writes under `bin/`. A `bin` segment directly followed
/// by one of these is compiler output and is pruned like `obj/`; any other `bin/` is source.
const DOTNET_BUILD_CONFIGS: &[&str] = &["Debug", "Release"];

/// Generated / lock / vendored FILE basenames that carry no architectural signal but are
/// large (lockfiles are often the single biggest text files in a repo).
const NOISE_FILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "packages.lock.json",
    "Cargo.lock",
    "composer.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "go.sum",
    "bun.lock",
    "deno.lock",
    "flake.lock",
];

/// Generated-file suffixes: minified bundles, source maps, and codegen output. The codegen
/// patterns (`.gen.ts`, `.pb.go`, protobuf/relay/graphql/openapi output, etc.) are machine-
/// written from a schema — auditing them is paying to review code no human owns.
const NOISE_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    ".bundle.js",
    ".map",
    ".gen.ts",
    ".gen.tsx",
    ".gen.js",
    ".gen.go",
    ".gen.dart",
    ".generated.ts",
    ".generated.tsx",
    ".generated.js",
    ".generated.go",
    ".generated.cs",
    ".pb.go",
    ".pb.ts",
    ".pb.cc",
    ".pb.h",
    "_pb2.py",
    "_pb2.pyi",
    ".g.dart",
    ".freezed.dart",
];

/// True if a path has a code extension (from `CODE_EXTS`) or is a `CODE_BASENAMES` match.
pub(crate) fn has_code_ext(path: &str) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    if CODE_BASENAMES
        .iter()
        .any(|b| basename == *b || basename.starts_with(b))
    {
        return true;
    }
    match path.rsplit_once('.') {
        Some((_, ext)) => CODE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// True when `path` should be admitted into the scanned file list AT ALL: it's `has_code_ext`
/// (full treatment), a `CONFIG_TEXT_EXTS` match, or a dotfile (basename starts with `.`).
///
/// This is the general fix for a structural gap: `has_code_ext` extracts a path's extension
/// by splitting on the LAST `.`, which mishandles the entire class of Unix dotfiles that
/// encode a variant as a second dot-segment — `.env.production`'s "extension" by that scheme
/// is `production`, not `env`, so it (and `.env.local`, `.env.staging`, `.npmrc`, `.netrc`,
/// `.pgpass`, ...) was silently dropped before a single byte of its content was ever read.
/// Real-world committed-secret incidents concentrate in exactly this file class (dotenv
/// variants, npm/netrc/pgpass credential files), which is why ANY dotfile is admitted here —
/// gated only by the existing noise/size/UTF-8 checks the caller already applies, so a stray
/// `.DS_Store` or oversized dotfile is still dropped exactly like any other file.
///
/// A `CONFIG_TEXT_EXTS`/dotfile admission does NOT widen anything else's scope: the
/// architectural/AST checkers (`camerata_checks::arch_checker`) only look at files matching
/// their own `interest_globs`, and the scan-tool previews (`scan_tools::run_scan_tools`) only
/// run a tool against extensions that tool itself recognizes — so a `.conf` or `.env.staging`
/// file admitted here is simply invisible to those, while remaining fully visible to the
/// deterministic secrets floor (`onboard::audit::AUDIT_RULES`, run over every admitted file
/// with no extension filter of its own — see `onboard::audit::audit_files`).
pub(crate) fn is_admissible_text(path: &str) -> bool {
    if has_code_ext(path) {
        return true;
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    if basename.starts_with('.') && basename.len() > 1 {
        return true;
    }
    match path.rsplit_once('.') {
        Some((_, ext)) => CONFIG_TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        // Extensionless, non-dotfile basename (e.g. a bare `credentials`). Deliberately NOT
        // broadened here — `CODE_BASENAMES` (checked above via `has_code_ext`) remains the
        // explicit opt-in list for that narrower case; a blanket "any extensionless file"
        // admission would sweep up compiled binaries and other noise with no extension at all.
        None => false,
    }
}

/// The reserved filename prefix for every artifact Camerata itself writes into, or beside, a
/// scanned repository: the incremental scan cache, the scan ledger, the shared cargo target,
/// the provisioned virtualenv and the agent worktrees.
///
/// Declared as a PREFIX rather than an enumerated basename list on purpose. See
/// [`is_engine_artifact`] for why the enumerated form is the shape of the bug it fixes.
pub(crate) const ENGINE_ARTIFACT_PREFIX: &str = ".camerata-";

/// True when `path` names something CAMERATA ITSELF wrote into the scanned tree, at any depth.
///
/// # The defect this exists to prevent
///
/// On 2026-10-16 a hearth scan's audit digest went from 103,307 to 277,543 characters between
/// two runs of the SAME engine over the SAME commit, at 88 files both times and with
/// `files_excluded = 0`. The entire +174,236 was `.camerata-scan-cache.json`, which
/// `camerata_cli::inspect_cmd::manifest_cache_path` writes to the scanned repo's own root. So
/// roughly 63% of what the model was asked to audit was the engine's own prior findings, and
/// the run emitted a CRITICAL located at `.camerata-scan-cache.json:1778` — the engine
/// re-detecting a snippet quoted inside its own cache and ranking it at queue position #1.
///
/// # Why this predicate, and not two more entries in [`NOISE_FILES`]
///
/// Nothing was wrong with either half of the collision that produced it:
///
/// - [`is_admissible_text`] admits ANY dotfile, deliberately, because `.env.production`,
///   `.npmrc`, `.netrc` and `.pgpass` are exactly where real committed-credential incidents
///   concentrate, and the extension-splitting scheme used to drop that whole class.
/// - The cache is reasonably a dotfile at the repo root.
///
/// The gap was that neither clause of [`is_noise_path`] could see it: not a lockfile basename,
/// not a generated suffix, no directory segment to match. Adding `.camerata-scan-cache.json`
/// and `.camerata-scan-ledger.json` to `NOISE_FILES` would close THIS instance and leave the
/// mechanism intact — the next engine-written artifact would be admitted exactly the same way,
/// and would again be found only by an external grader reading a shipped client report. A
/// prefix rule makes the namespace closed by construction, so a new artifact is excluded on
/// the day it is introduced rather than on the day someone remembers this function exists.
///
/// # Why segment-wise rather than basename-only
///
/// Three of the five are DIRECTORIES (`.camerata-shared-target`, `.camerata-venv`,
/// `.camerata-worktrees`). Matching only the basename would exclude the directory entry and
/// admit every file inside it, which for a shared cargo target or a provisioned virtualenv is
/// the larger problem by orders of magnitude.
pub(crate) fn is_engine_artifact(path: &str) -> bool {
    path.split('/')
        .any(|seg| seg.starts_with(ENGINE_ARTIFACT_PREFIX))
}

/// True when a path should be pruned BEFORE scanning: it is an artifact Camerata itself wrote
/// ([`is_engine_artifact`]), it lives under a build/dep/cache directory, or it is a lockfile /
/// minified bundle / source map. `extra_dirs` holds any project-specific dir names from
/// `CAMERATA_SCAN_EXCLUDE_DIRS`.
pub(crate) fn is_noise_path(path: &str, extra_dirs: &[String]) -> bool {
    // Checked FIRST: the engine's own output is never audit input, regardless of what it is
    // named or where it sits. Cheapest check, and the one whose absence shipped a CRITICAL
    // finding about Camerata's own cache file to a client report.
    if is_engine_artifact(path) {
        return true;
    }
    let segments = path.split('/');
    let basename = path.rsplit('/').next().unwrap_or(path);
    if NOISE_FILES.contains(&basename) {
        return true;
    }
    if NOISE_SUFFIXES.iter().any(|s| basename.ends_with(s)) {
        return true;
    }
    let segs: Vec<&str> = segments.collect();
    segs.iter().enumerate().any(|(i, seg)| {
        extra_dirs.iter().any(|d| d == seg)
            || NOISE_DIRS.contains(seg)
            || (*seg == "bin"
                && segs
                    .get(i + 1)
                    .is_some_and(|next| DOTNET_BUILD_CONFIGS.contains(next)))
    })
}

/// Parse the `CAMERATA_SCAN_EXCLUDE_DIRS` env (comma-separated) into extra dir names.
pub(crate) fn extra_exclude_dirs() -> Vec<String> {
    std::env::var("CAMERATA_SCAN_EXCLUDE_DIRS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// What `read_local_repo_files` pulled from a working tree: the auditable files, whether the file
/// cap was hit, and how many would-be-scannable files were pruned as noise (so the scan can
/// SHOW the filter doing its job — "1,583 scanned, 2,800 excluded as build/generated noise").
pub struct ExtractedRepo {
    pub files: Vec<(String, String)>,
    pub truncated: bool,
    pub excluded_noise: usize,
}

impl ExtractedRepo {
    /// How many ADMITTED files are Camerata's own artifacts ([`is_engine_artifact`]).
    ///
    /// Must be zero. Non-zero means the engine is about to audit its own output, which is the
    /// 2026-10-16 defect (`.camerata-scan-cache.json` collected as source, 63% of the audit
    /// prompt). `is_noise_path` now excludes them, so this is the regression guard on that
    /// fix rather than a live counter — it is fed to the integrity gate's
    /// `engine_self_ingestion` family.
    ///
    /// Computed over the FINAL admitted list rather than counted at the collection sites, on
    /// purpose: there are two of those today and a third would silently miss a counter, while
    /// "what actually got through" is both the invariant we care about and impossible to
    /// bypass by adding a new collection path.
    pub fn engine_artifacts_admitted(&self) -> usize {
        self.files
            .iter()
            .filter(|(path, _)| is_engine_artifact(path))
            .count()
    }
}

/// Read a repo's auditable files from its LOCAL working tree — the local-first scan source.
///
/// When the directory is a git repo (`.git` exists) the walk is
/// [`ignore`](https://docs.rs/ignore)-powered: `.gitignore`, `.git/info/exclude`, and the
/// user's global gitignore are all honoured. A file that is gitignored is skipped; a file
/// that is committed but unignored (e.g. a tracked `.env`) is still scanned — which is
/// exactly correct. The noise denylist (`is_noise_path`) is applied on top as belt-and-
/// suspenders for any project that has not gitignored its build artefacts yet. The walk is
/// then UNIONED with `git ls-files` (see `git_tracked_files`) so a file that is committed but
/// LATER added to `.gitignore` — a common real-world mistake, and the highest-value case of
/// all, since it's an already-leaked secret whose owner is actively (if unknowingly) hiding
/// the fix — is still scanned; the `ignore` crate has no concept of the git index, so on its
/// own it would treat "matches .gitignore" and "not tracked" as the same thing.
///
/// When the directory is NOT a git repo, the function falls back to the original iterative
/// DFS noise-denylist walk (same behaviour as before `ignore` was added) so that the
/// non-repo case is never broken.
///
/// Paths are relative to the repo root, forward-slashed. Synchronous (blocking IO) —
/// call via `tokio::task::spawn_blocking`.
pub fn read_local_repo_files(root: &std::path::Path) -> anyhow::Result<ExtractedRepo> {
    let extra_dirs = extra_exclude_dirs();
    if root.join(".git").exists() {
        read_local_repo_files_gitignore(root, &extra_dirs)
    } else {
        read_local_repo_files_noise_denylist(root, &extra_dirs)
    }
}

/// Gitignore-aware walk (used when `.git` exists). Delegates to the `ignore` crate so
/// `.gitignore`, `.git/info/exclude`, and the user's global gitignore are all respected.
/// The noise denylist is applied afterward as a belt-and-suspenders guard.
fn read_local_repo_files_gitignore(
    root: &std::path::Path,
    extra_dirs: &[String],
) -> anyhow::Result<ExtractedRepo> {
    let mut files = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut excluded_noise = 0usize;
    let mut truncated = false;

    // hidden(false): we want to scan dotfiles like .env and .camerata/ — the `ignore`
    // crate would otherwise skip them by default.
    let walker = ignore::WalkBuilder::new(root)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(false)
        .build();

    'walk: for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let p = entry.path();
        // Skip the root dir itself and any non-file entries (dirs, symlinks, etc.).
        let ft = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if !ft.is_file() {
            continue;
        }
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        // Belt-and-suspenders: noise denylist on top of gitignore (catches projects whose
        // build artefacts are not yet gitignored, or repos where target/ was committed).
        let noise = is_noise_path(&rel, extra_dirs);
        let admissible = is_admissible_text(&rel);
        if noise && admissible {
            excluded_noise += 1;
        }
        if noise || !admissible {
            continue;
        }
        if entry
            .metadata()
            .map(|m| m.len() as usize)
            .unwrap_or(usize::MAX)
            > MAX_FILE_BYTES
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(p) else {
            continue; // skip non-UTF-8 / unreadable
        };
        seen.insert(rel.clone());
        files.push((rel, content));
        if files.len() >= HARD_CAP_FILES {
            truncated = true;
            break 'walk;
        }
    }

    // Union in git-TRACKED files the ignore-aware walk above skipped because they match a
    // `.gitignore` pattern. A committed secret doesn't stop being committed (or stop being
    // reachable via `git show HEAD:<path>` / the full clone history) just because a later
    // `.gitignore` edit tries to hide it going forward — that's the single highest-value case
    // for this scan to catch, not a gap to leave open. `git_tracked_files` returns `None` on
    // any failure (no `git` on PATH, etc.); the primary walk above already succeeded
    // regardless, so this is purely additive and never turns a working scan into an error.
    if !truncated {
        if let Some(tracked) = git_tracked_files(root) {
            for rel in tracked {
                if seen.contains(&rel) {
                    continue;
                }
                let noise = is_noise_path(&rel, extra_dirs);
                let admissible = is_admissible_text(&rel);
                if noise && admissible {
                    excluded_noise += 1;
                }
                if noise || !admissible {
                    continue;
                }
                let full = root.join(&rel);
                let Ok(meta) = std::fs::metadata(&full) else {
                    continue; // tracked in the index but not present on disk (rare edge case)
                };
                if !meta.is_file() || meta.len() as usize > MAX_FILE_BYTES {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&full) else {
                    continue; // skip non-UTF-8 / unreadable
                };
                seen.insert(rel.clone());
                files.push((rel, content));
                if files.len() >= HARD_CAP_FILES {
                    truncated = true;
                    break;
                }
            }
        }
    }

    Ok(ExtractedRepo {
        files,
        truncated,
        excluded_noise,
    })
}

/// The full set of git-TRACKED file paths under `root` (relative, forward-slashed), via
/// `git ls-files -z` (NUL-separated so paths with spaces/unicode parse unambiguously, and so
/// a path itself can never be mistaken for the separator). Returns `None` on any failure (no
/// `git` binary on PATH, non-zero exit, non-UTF-8 output, ...) — the caller treats that as "no
/// union available," never a hard error: the primary gitignore-aware walk already ran and
/// still stands on its own.
fn git_tracked_files(root: &std::path::Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(
        text.split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.replace('\\', "/"))
            .collect(),
    )
}

/// Noise-denylist walk (fallback for non-git directories). Iterative DFS so a deep tree
/// can't blow the stack. Mirrors the original `read_local_repo_files` body exactly.
fn read_local_repo_files_noise_denylist(
    root: &std::path::Path,
    extra_dirs: &[String],
) -> anyhow::Result<ExtractedRepo> {
    let mut files = Vec::new();
    let mut excluded_noise = 0usize;
    let mut truncated = false;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            // Path relative to the repo root, forward-slashed (matches tarball paths).
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            if ft.is_dir() {
                // Prune noise dirs (don't descend) — is_noise_path matches any segment, so a
                // noise dir name prunes the whole subtree before we read a single file in it.
                if !is_noise_path(&rel, extra_dirs) {
                    stack.push(p);
                }
                continue;
            }
            if !ft.is_file() {
                continue; // skip symlinks / fifos / etc.
            }
            let noise = is_noise_path(&rel, extra_dirs);
            let admissible = is_admissible_text(&rel);
            if noise && admissible {
                excluded_noise += 1;
            }
            if noise || !admissible {
                continue;
            }
            if entry
                .metadata()
                .map(|m| m.len() as usize)
                .unwrap_or(usize::MAX)
                > MAX_FILE_BYTES
            {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else {
                continue; // skip non-UTF-8 / unreadable
            };
            files.push((rel, content));
            if files.len() >= HARD_CAP_FILES {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }
    Ok(ExtractedRepo {
        files,
        truncated,
        excluded_noise,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &std::path::Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn scan(root: &std::path::Path) -> ExtractedRepo {
        read_local_repo_files(root).unwrap()
    }

    fn paths(extracted: &ExtractedRepo) -> Vec<String> {
        let mut v: Vec<String> = extracted.files.iter().map(|(p, _)| p.clone()).collect();
        v.sort();
        v
    }

    // ── bin/ pruning: only .NET build output is noise ──────────────────────────

    #[test]
    fn only_dotnet_build_output_under_bin_is_noise() {
        let none: &[String] = &[];
        for pruned in [
            "bin/Debug/net8.0/app.json",
            "bin/Release/app.deps.json",
            "src/Api/bin/Debug/net8.0/appsettings.json",
            "bin/Debug",
            "bin/Release",
        ] {
            assert!(is_noise_path(pruned, none), "{pruned} is build output");
        }
        for kept in [
            "bin/cli.js",
            "bin",
            "bin/deploy.sh",
            "src/bin/tool.rs",
            "crates/x/src/bin/multi/main.rs",
            "bin/debug/lowercase.rs",
            "bin/Debugger/x.js",
            "bin/ReleaseNotes.md",
            "src/binary/x.rs",
        ] {
            assert!(!is_noise_path(kept, none), "{kept} is source");
        }
    }

    #[test]
    fn an_explicit_exclude_dir_named_bin_still_prunes_everything_under_it() {
        let extra = vec!["bin".to_string()];
        assert!(is_noise_path("bin/cli.js", &extra));
        assert!(is_noise_path("src/bin/tool.rs", &extra));
    }

    #[test]
    fn other_noise_dirs_are_unaffected_by_the_bin_change() {
        let none: &[String] = &[];
        for pruned in [
            "obj/x.json",
            "node_modules/x/y.js",
            "target/debug/x.rs",
            "dist/a.js",
        ] {
            assert!(is_noise_path(pruned, none), "{pruned}");
        }
    }

    fn bin_fixture(root: &std::path::Path) {
        write(root, "src/bin/tool.rs", "fn main() {}\n");
        write(root, "bin/cli.js", "#!/usr/bin/env node\nconsole.log(1);\n");
        write(root, "bin/Debug/net8.0/app.json", "{\"built\": true}\n");
        write(root, "bin/Release/app.json", "{\"built\": true}\n");
        write(root, "src/main.rs", "fn main() {}\n");
    }

    fn expect_bin_scan(extracted: &ExtractedRepo) {
        assert_eq!(
            paths(extracted),
            ["bin/cli.js", "src/bin/tool.rs", "src/main.rs"],
            "src/bin and a Node bin/ script are admitted; bin/Debug and bin/Release stay pruned"
        );
    }

    #[test]
    fn bin_scripts_are_admitted_and_dotnet_output_stays_pruned_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        bin_fixture(dir.path());
        let extracted = scan(dir.path());
        expect_bin_scan(&extracted);
        // The directory walk prunes the .NET folders without reading them.
        assert_eq!(extracted.excluded_noise, 0);
    }

    #[test]
    fn bin_scripts_are_admitted_and_dotnet_output_stays_pruned_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        bin_fixture(dir.path());
        let extracted = scan(dir.path());
        expect_bin_scan(&extracted);
        // The gitignore-aware walk sees each file and counts the .NET outputs as noise.
        assert_eq!(extracted.excluded_noise, 2);
    }

    /// Security-coverage fix: `bin/` used to be pruned wholesale, so a credential committed in
    /// a `bin/` deploy script was never read by the secrets floor.
    #[test]
    fn a_secret_planted_in_a_bin_script_is_now_caught_by_the_secrets_floor() {
        let dir = tempfile::tempdir().unwrap();
        // A made-up 30-character literal assigned to a secret-named variable.
        let script = concat!(
            "#!/bin/sh\nAPI_KEY=\"Zq9Xk3Pm7Rt2",
            "Vw5Yb8Nc4Jd6Hf1Ga0\"\ncurl -H \"x: $API_KEY\" https://example.test\n"
        );
        write(dir.path(), "bin/deploy.sh", script);
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        let extracted = scan(dir.path());
        let findings = crate::onboard::audit::audit_files("o/r", &extracted.files);
        assert!(
            findings.iter().any(|f| f.path == "bin/deploy.sh"),
            "the floor must read bin/deploy.sh: {findings:#?}"
        );
    }
}
