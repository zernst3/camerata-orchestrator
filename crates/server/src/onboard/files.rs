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
    "conf", "properties", "txt", "pem", "crt", "cert", "p12", "pfx", "jks", "keystore", "pgpass",
];

/// Directory names that are build output, dependency trees, caches, or tool state — pure
/// noise for an architecture audit, and the bulk of a repo's bytes/tokens. A real consumer
/// found 14 of 25 MB of one monorepo was `.turbo/cache` manifests + lockfiles; scanning
/// that is paying to audit generated artifacts. Matched on ANY path segment, so
/// `apps/web/node_modules/...` and `node_modules/...` both prune. Extend per-project via
/// the `CAMERATA_SCAN_EXCLUDE_DIRS` env (comma-separated extra dir names).
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
    "bin",
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

/// True when a path should be pruned BEFORE scanning: it lives under a build/dep/cache
/// directory, or is a lockfile / minified bundle / source map. `extra_dirs` holds any
/// project-specific dir names from `CAMERATA_SCAN_EXCLUDE_DIRS`.
pub(crate) fn is_noise_path(path: &str, extra_dirs: &[String]) -> bool {
    let mut segments = path.split('/');
    let basename = path.rsplit('/').next().unwrap_or(path);
    if NOISE_FILES.contains(&basename) {
        return true;
    }
    if NOISE_SUFFIXES.iter().any(|s| basename.ends_with(s)) {
        return true;
    }
    segments.any(|seg| NOISE_DIRS.contains(&seg) || extra_dirs.iter().any(|d| d == seg))
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
