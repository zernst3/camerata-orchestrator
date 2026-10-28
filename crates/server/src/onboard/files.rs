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
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py", "go", "java", "rb", "php",
    "cs", "sql", "toml", "yaml", "yml", "json", "sh", "env", "cfg", "ini", "tf", "kt", "swift",
    "c", "cpp", "h",
    // IaC: Terragrunt/Packer HCL, Azure Bicep (Terraform `.tf` is already above).
    "hcl", "bicep",
    // Dart (Flutter): hand-written source, and `lib/firebase_options.dart` is where a Flutter
    // app's Firebase `apiKey` lives. Generated Dart is pruned by `NOISE_SUFFIXES`.
    "dart",
    // Firebase Security Rules (`firestore.rules`, `storage.rules`, and any `.rules` file
    // `firebase.json` names). Other tools also use `.rules` (udev, Prometheus alerting, Snort);
    // those are admitted as config text and ignored by the Firebase checker, which recognises a
    // Firebase rules file by its `service` header, never by its name. Admitted by extension, not
    // by the two default basenames, because `firebase.json` may point at any file.
    "rules",
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
    // Apple property lists: `GoogleService-Info.plist` carries the iOS Firebase `API_KEY`, and
    // real committed credentials land in plists.
    "plist",
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
    // `firebase deploy` writes `hosting.<hash>.cache` here: a generated manifest, never source.
    ".firebase",
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

/// The build-configuration folders .NET writes under `bin/`, directly (`bin/Debug`) or under a
/// platform folder (`bin/x64/Debug`, `bin/Any CPU/Release`). A path with one of these anywhere
/// below a `bin` segment is compiler output and is pruned like `obj/`; any other `bin/` is
/// source.
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
    // build_runner outputs with a fixed suffix and no hand-written content: mockito, auto_route,
    // protobuf. `.config.dart` is deliberately absent (injectable generates it, but a hand-written
    // `env.config.dart` is common; excluding it would blind the secrets floor).
    ".mocks.dart",
    ".gr.dart",
    ".pb.dart",
    ".pbenum.dart",
    ".pbjson.dart",
    ".pbserver.dart",
];

/// The subset of [`NOISE_SUFFIXES`] and [`NOISE_DIRS`] that marks machine-written code which is
/// pruned from the audited list but still handed to the architectural tier as reference text:
/// codegen is often the only importer of hand-written files (TanStack Router's
/// `routeTree.gen.ts` imports every route). Minified bundles, source maps and protobuf output
/// stay dropped.
const GENERATED_NOISE_SUFFIXES: &[&str] = &[
    ".gen.ts",
    ".gen.tsx",
    ".gen.js",
    ".generated.ts",
    ".generated.tsx",
    ".generated.js",
];
const GENERATED_NOISE_DIRS: &[&str] = &["generated", "__generated__"];

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

/// Extensions of REFERENCE TEXT: files that are not audited as code and carry no committed-secret
/// signal of their own, but whose text can REFER to a source file by name (a `<script src>` in
/// HTML, an import inside a Vue or Svelte component, a run instruction in a README, a stylesheet
/// `@import`). The dead-code rule needs them to see that a file is still used; nothing else
/// reads them. See [`ExtractedRepo::reference_files`].
const REFERENCE_TEXT_EXTS: &[&str] = &[
    "html", "htm", "vue", "svelte", "astro", "md", "mdx", "css", "scss", "less", "ejs", "erb",
    "hbs", "pug", "njk", "j2", "jinja", "jinja2",
];

/// Extensionless build-runner files that name the scripts and sources they run.
const REFERENCE_TEXT_BASENAMES: &[&str] = &[
    "Makefile",
    "GNUmakefile",
    "makefile",
    "Procfile",
    "Justfile",
    "justfile",
];

/// Safety net for the reference list, separate from [`HARD_CAP_FILES`] so documentation-heavy
/// repos do not crowd out code. Hitting it marks the list incomplete
/// ([`ExtractedRepo::reference_truncated`]), which disables every rule that reasons from
/// absence, exactly like [`ExtractedRepo::truncated`].
pub(crate) const HARD_CAP_REFERENCE_FILES: usize = 20_000;

/// True when `path` is reference text (see [`REFERENCE_TEXT_EXTS`]).
pub(crate) fn is_reference_text(path: &str) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    if REFERENCE_TEXT_BASENAMES.contains(&basename) {
        return true;
    }
    match basename.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty() && REFERENCE_TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_str())
        }
        None => false,
    }
}

/// Collects reference text under the same noise, size and UTF-8 rules as the audited files.
struct ReferenceSink {
    files: Vec<(String, String)>,
    truncated: bool,
    cap: usize,
}

impl ReferenceSink {
    fn new(cap: usize) -> Self {
        Self {
            files: Vec::new(),
            truncated: false,
            cap,
        }
    }

    /// Offer `rel` (read from `full`) when it is reference text that audit admission skipped.
    /// A file over the size limit or not valid UTF-8 is dropped, like any other file; a list
    /// that is already full is marked truncated instead of growing.
    fn offer(&mut self, rel: &str, full: &std::path::Path) {
        if is_reference_text(rel) {
            self.accept(rel, full);
        }
    }

    /// Take `rel` as reference text whatever its name: generated code that audit admission
    /// pruned ([`is_generated_only_noise`]).
    fn accept(&mut self, rel: &str, full: &std::path::Path) {
        if self.files.len() >= self.cap {
            self.truncated = true;
            return;
        }
        let too_big = std::fs::metadata(full)
            .map(|m| m.len() as usize > MAX_FILE_BYTES)
            .unwrap_or(true);
        if too_big {
            return;
        }
        if let Ok(content) = std::fs::read_to_string(full) {
            self.files.push((rel.to_string(), content));
        }
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
    noise_path(path, extra_dirs, false)
}

/// True when `path` is noise ONLY because it is generated code ([`GENERATED_NOISE_SUFFIXES`],
/// [`GENERATED_NOISE_DIRS`]): the same path under `node_modules/`, `dist/` or an excluded dir is
/// not, and neither is a minified bundle or a source map.
pub(crate) fn is_generated_only_noise(path: &str, extra_dirs: &[String]) -> bool {
    noise_path(path, extra_dirs, false) && !noise_path(path, extra_dirs, true)
}

fn noise_path(path: &str, extra_dirs: &[String], allow_generated: bool) -> bool {
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
    let generated_suffix = |s: &&str| GENERATED_NOISE_SUFFIXES.contains(s);
    if NOISE_SUFFIXES
        .iter()
        .filter(|s| !(allow_generated && generated_suffix(s)))
        .any(|s| basename.ends_with(s))
    {
        return true;
    }
    let segs: Vec<&str> = segments.collect();
    segs.iter().enumerate().any(|(i, seg)| {
        extra_dirs.iter().any(|d| d == seg)
            || (NOISE_DIRS.contains(seg)
                && !(allow_generated && GENERATED_NOISE_DIRS.contains(seg)))
            || (*seg == "bin"
                && segs[i + 1..]
                    .iter()
                    .any(|next| DOTNET_BUILD_CONFIGS.contains(next)))
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
    /// The audited files: code and committed-secret-bearing config. This is what the AI digest,
    /// the secrets floor, stack detection and every other consumer read.
    pub files: Vec<(String, String)>,
    /// Reference text ([`is_reference_text`]): HTML, single-file components, Markdown,
    /// stylesheets, templates and build-runner files. Kept apart from `files` on purpose, so
    /// the AI digest (which takes whatever slice it is given, capped by characters) and the
    /// secrets floor see exactly the input they always have, and no model cost or prompt
    /// space changes. Only the architectural tier is handed `files` plus this list, and only
    /// when a rule that reasons from absence is armed.
    pub reference_files: Vec<(String, String)>,
    pub truncated: bool,
    /// The reference list hit [`HARD_CAP_REFERENCE_FILES`]: some reference text is missing.
    pub reference_truncated: bool,
    pub excluded_noise: usize,
    /// The paths behind `excluded_noise` (always the same length), so `camerata count` can size
    /// what the filter dropped without a second walk.
    pub excluded_noise_paths: Vec<String>,
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
    let mut excluded_noise_paths: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut reference = ReferenceSink::new(HARD_CAP_REFERENCE_FILES);

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
        // Every walked file is "seen", admitted or not, so the ls-files union below only
        // revisits files the walk skipped (gitignored); it must not classify a walked noise or
        // reference file a second time.
        seen.insert(rel.clone());
        // Belt-and-suspenders: noise denylist on top of gitignore (catches projects whose
        // build artefacts are not yet gitignored, or repos where target/ was committed).
        let noise = is_noise_path(&rel, extra_dirs);
        let admissible = is_admissible_text(&rel);
        if noise && admissible {
            excluded_noise_paths.push(rel.clone());
        }
        if !noise && !admissible {
            reference.offer(&rel, p);
        } else if noise && admissible && is_generated_only_noise(&rel, extra_dirs) {
            reference.accept(&rel, p);
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
                    excluded_noise_paths.push(rel.clone());
                }
                let full = root.join(&rel);
                if !noise && !admissible {
                    reference.offer(&rel, &full);
                } else if noise && admissible && is_generated_only_noise(&rel, extra_dirs) {
                    reference.accept(&rel, &full);
                }
                if noise || !admissible {
                    continue;
                }
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
        reference_files: reference.files,
        truncated,
        reference_truncated: reference.truncated,
        excluded_noise: excluded_noise_paths.len(),
        excluded_noise_paths,
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
    let mut excluded_noise_paths: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut reference = ReferenceSink::new(HARD_CAP_REFERENCE_FILES);
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
                // Generated folders are descended: their files are reference text.
                if !is_noise_path(&rel, extra_dirs) || is_generated_only_noise(&rel, extra_dirs) {
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
                excluded_noise_paths.push(rel.clone());
            }
            if !noise && !admissible {
                reference.offer(&rel, &p);
            } else if noise && admissible && is_generated_only_noise(&rel, extra_dirs) {
                reference.accept(&rel, &p);
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
        reference_files: reference.files,
        truncated,
        reference_truncated: reference.truncated,
        excluded_noise: excluded_noise_paths.len(),
        excluded_noise_paths,
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
            "bin/x64/Debug/net8.0/app.json",
            "src/Api/bin/Any CPU/Release/app.deps.json",
            "bin/x86/Release",
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
            "bin/x64/tool.js",
            "Debug/notes/a.js",
            "src/Debug/a.ts",
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

    /// A tracked noise file is seen by the walk AND listed by `git ls-files`; it must still be
    /// excluded (and counted as excluded) exactly once, so the noise figure is not inflated.
    #[test]
    fn a_tracked_noise_file_is_counted_as_excluded_once() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        write(dir.path(), "vendor/dep/lib.js", "var a;\n");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        let extracted = scan(dir.path());
        assert_eq!(extracted.excluded_noise, 1);
        assert_eq!(extracted.excluded_noise_paths, vec!["vendor/dep/lib.js"]);
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

    // ── reference text tier ────────────────────────────────────────────────────

    #[test]
    fn reference_text_covers_markup_components_docs_styles_templates_and_runners() {
        for yes in [
            "index.html",
            "pages/x.htm",
            "src/App.vue",
            "src/Page.svelte",
            "src/pages/index.astro",
            "README.md",
            "docs/guide.mdx",
            "styles/a.css",
            "styles/a.scss",
            "styles/a.less",
            "views/layout.ejs",
            "app/views/home/index.html.erb",
            "views/p.hbs",
            "views/p.pug",
            "views/p.njk",
            "templates/p.j2",
            "templates/p.jinja",
            "templates/p.jinja2",
            "Makefile",
            "sub/Procfile",
            "Justfile",
            "justfile",
            "INDEX.HTML",
        ] {
            assert!(is_reference_text(yes), "{yes}");
        }
        for no in [
            "src/a.ts",
            "src/a.rs",
            "Makefile.bak",
            "notes",
            "a.md.bak",
            ".html",
            "src/htmlparser.ts",
            "Cargo.toml",
        ] {
            assert!(!is_reference_text(no), "{no}");
        }
    }

    #[test]
    fn reference_text_is_not_audit_input() {
        for path in [
            "index.html",
            "src/App.vue",
            "README.md",
            "a.css",
            "Makefile",
        ] {
            assert!(
                !is_admissible_text(path),
                "{path} must stay out of the audited file list"
            );
        }
    }

    fn reference_fixture(root: &std::path::Path) {
        write(root, "src/a.ts", "export const a = 1;\n");
        write(root, "index.html", "<script src=\"/src/a.ts\"></script>\n");
        write(root, "src/App.vue", "<script setup>import './a'</script>\n");
        // A secret-shaped literal in prose: reference text is not read by the secrets floor.
        let readme = concat!("Run it.\nAPI_KEY=\"Zq9Xk3Pm7Rt2", "Vw5Yb8Nc4Jd6Hf1Ga0\"\n");
        write(root, "README.md", readme);
        write(root, "Makefile", "run:\n\tnode src/a.ts\n");
        write(root, "styles/a.css", "body { color: red; }\n");
        write(root, "node_modules/x/README.md", "vendored\n");
        write(root, ".camerata-cache/notes.md", "engine output\n");
        write(root, "docs/huge.md", &"x".repeat(MAX_FILE_BYTES + 1));
    }

    fn reference_paths(extracted: &ExtractedRepo) -> Vec<String> {
        let mut v: Vec<String> = extracted
            .reference_files
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        v.sort();
        v
    }

    fn expect_reference_tier(extracted: &ExtractedRepo) {
        // The audited list, which feeds the AI digest and the secrets floor, is exactly what it
        // was before the tier existed.
        assert_eq!(paths(extracted), ["src/a.ts"]);
        assert_eq!(extracted.excluded_noise, 0);
        assert_eq!(
            reference_paths(extracted),
            [
                "Makefile",
                "README.md",
                "index.html",
                "src/App.vue",
                "styles/a.css"
            ],
            "noise dirs, engine artifacts and oversized files are excluded from the tier too"
        );
        assert!(!extracted.reference_truncated);
        assert!(!extracted.truncated);

        // The digest input is byte-for-byte the digest of the audited files alone.
        let nonce = crate::prompt_frame::FrameNonce::generate();
        let expected = crate::ai_audit::build_digest(
            &[("src/a.ts".to_string(), "export const a = 1;\n".to_string())],
            &nonce,
        );
        assert_eq!(
            crate::ai_audit::build_digest(&extracted.files, &nonce),
            expected
        );

        // The secrets floor reads only the audited files, so the literal in the README is not a
        // finding, and nothing about the floor's input changed.
        let findings = crate::onboard::audit::audit_files("o/r", &extracted.files);
        assert!(
            findings.iter().all(|f| f.path != "README.md"),
            "{findings:#?}"
        );
    }

    #[test]
    fn reference_text_is_collected_apart_from_audited_files_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        reference_fixture(dir.path());
        expect_reference_tier(&scan(dir.path()));
    }

    #[test]
    fn reference_text_is_collected_apart_from_audited_files_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        reference_fixture(dir.path());
        expect_reference_tier(&scan(dir.path()));
    }

    #[test]
    fn a_git_tracked_but_gitignored_reference_file_is_still_collected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status();
            matches!(status, Ok(s) if s.success())
        };
        if !git(&["init", "-q"]) {
            return; // no git on PATH: the union path is not exercisable here
        }
        write(root, "src/a.ts", "export const a = 1;\n");
        write(root, "docs/guide.md", "see a.ts\n");
        assert!(git(&["add", "-f", "src/a.ts", "docs/guide.md"]));
        write(root, ".gitignore", "docs/\n");
        let extracted = scan(root);
        assert_eq!(reference_paths(&extracted), ["docs/guide.md"]);
    }

    #[test]
    fn the_reference_list_marks_itself_truncated_instead_of_growing_past_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.md", "b.md", "c.md"] {
            write(dir.path(), name, "text\n");
        }
        let mut sink = ReferenceSink::new(2);
        for name in ["a.md", "b.md", "c.md", "src/skip.ts"] {
            sink.offer(name, &dir.path().join(name));
        }
        assert_eq!(sink.files.len(), 2);
        assert!(sink.truncated);

        let mut roomy = ReferenceSink::new(10);
        roomy.offer("a.md", &dir.path().join("a.md"));
        roomy.offer("missing.md", &dir.path().join("missing.md"));
        assert_eq!(roomy.files.len(), 1);
        assert!(!roomy.truncated);
    }

    // ── module-variant extensions ───────────────────────────────────────────────

    #[test]
    fn module_variant_extensions_are_audited_code() {
        for path in [
            "src/a.mjs",
            "src/a.cjs",
            "src/a.mts",
            "src/a.cts",
            "src/A.MJS",
        ] {
            assert!(has_code_ext(path), "{path}");
            assert!(is_admissible_text(path), "{path}");
        }
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/db/index.mjs",
            "import { rows } from './seedRows.ts';\n",
        );
        write(
            dir.path(),
            "src/db/seedRows.ts",
            "export const rows = [];\n",
        );
        write(dir.path(), "src/legacy.cjs", "module.exports = {};\n");
        let extracted = scan(dir.path());
        assert_eq!(
            paths(&extracted),
            ["src/db/index.mjs", "src/db/seedRows.ts", "src/legacy.cjs"]
        );
    }

    // ── generated code: out of the audited list, kept as reference ──────────────

    #[test]
    fn generated_code_is_still_noise_but_is_recognised_as_generated_only_noise() {
        let none: &[String] = &[];
        for path in [
            "src/routeTree.gen.ts",
            "src/schema.generated.tsx",
            "src/__generated__/types.ts",
            "src/generated/client.ts",
            "api/ops.gen.js",
        ] {
            assert!(
                is_noise_path(path, none),
                "{path} stays out of the audited list"
            );
            assert!(is_generated_only_noise(path, none), "{path}");
        }
        for path in [
            "node_modules/x/a.gen.ts",
            "dist/a.gen.ts",
            "src/app.min.js",
            "src/a.js.map",
            "src/__snapshots__/a.test.tsx",
            "src/a.ts",
        ] {
            assert!(!is_generated_only_noise(path, none), "{path}");
        }
    }

    fn generated_fixture(root: &std::path::Path) {
        write(
            root,
            "src/main.tsx",
            "import { routeTree } from './routeTree.gen';\n",
        );
        write(
            root,
            "src/routeTree.gen.ts",
            "import { Route as About } from './routes/about'\n",
        );
        write(root, "src/routes/about.tsx", "export const Route = 1;\n");
        write(
            root,
            "src/__generated__/ops.ts",
            "import '../routes/about'\n",
        );
        write(root, "node_modules/x/vendored.gen.ts", "export {};\n");
        write(root, "dist/bundle.gen.js", "export {};\n");
        write(root, "src/vendor.min.js", "var a=1;\n");
    }

    fn expect_generated_tier(extracted: &ExtractedRepo) {
        // The audited list (digest, secrets floor) is unchanged: no generated file in it.
        assert_eq!(paths(extracted), ["src/main.tsx", "src/routes/about.tsx"]);
        assert_eq!(
            reference_paths(extracted),
            ["src/__generated__/ops.ts", "src/routeTree.gen.ts"],
            "generated code is kept as reference; vendored, built and minified code is not"
        );
        let nonce = crate::prompt_frame::FrameNonce::generate();
        let expected = crate::ai_audit::build_digest(
            &[
                (
                    "src/main.tsx".to_string(),
                    "import { routeTree } from './routeTree.gen';\n".to_string(),
                ),
                (
                    "src/routes/about.tsx".to_string(),
                    "export const Route = 1;\n".to_string(),
                ),
            ],
            &nonce,
        );
        assert_eq!(
            crate::ai_audit::build_digest(&extracted.files, &nonce),
            expected
        );
    }

    #[test]
    fn generated_code_goes_to_the_reference_tier_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        generated_fixture(dir.path());
        expect_generated_tier(&scan(dir.path()));
    }

    #[test]
    fn generated_code_goes_to_the_reference_tier_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        generated_fixture(dir.path());
        expect_generated_tier(&scan(dir.path()));
    }

    #[test]
    fn a_secret_in_generated_code_is_still_not_secrets_floor_input() {
        let dir = tempfile::tempdir().unwrap();
        let body = concat!(
            "export const API_KEY = \"Zq9Xk3Pm7Rt2",
            "Vw5Yb8Nc4Jd6Hf1Ga0\";\n"
        );
        write(dir.path(), "src/client.gen.ts", body);
        write(dir.path(), "src/a.ts", "export {};\n");
        let extracted = scan(dir.path());
        let findings = crate::onboard::audit::audit_files("o/r", &extracted.files);
        assert!(
            findings.iter().all(|f| f.path != "src/client.gen.ts"),
            "{findings:#?}"
        );
    }

    // ── Firebase / Flutter admission (MASTER-SEQUENCE item 24a) ────────────────

    fn firebase_rules_fixture(root: &std::path::Path) {
        write(root, "firestore.rules", "rules_version = '2';\n");
        write(root, "storage.rules", "rules_version = '2';\n");
        write(root, "rules/firestore.prod.rules", "rules_version = '2';\n");
        write(root, "database.rules.json", "{\"rules\":{}}\n");
        write(root, "firebase.json", "{}\n");
        write(root, ".firebaserc", "{}\n");
        write(root, "src/main.ts", "export {};\n");
    }

    fn expect_firebase_rules_scan(extracted: &ExtractedRepo) {
        assert_eq!(
            paths(extracted),
            [
                ".firebaserc",
                "database.rules.json",
                "firebase.json",
                "firestore.rules",
                "rules/firestore.prod.rules",
                "src/main.ts",
                "storage.rules"
            ]
        );
        assert!(
            reference_paths(extracted).is_empty(),
            "rules files are audited code, not reference text"
        );
    }

    #[test]
    fn firebase_rules_files_are_admitted_as_audited_code_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        firebase_rules_fixture(dir.path());
        expect_firebase_rules_scan(&scan(dir.path()));
    }

    #[test]
    fn firebase_rules_files_are_admitted_as_audited_code_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        firebase_rules_fixture(dir.path());
        expect_firebase_rules_scan(&scan(dir.path()));
    }

    fn dart_fixture(root: &std::path::Path) {
        write(root, "lib/main.dart", "void main() {}\n");
        write(
            root,
            "lib/firebase_options.dart",
            "// File generated by FlutterFire CLI.\n",
        );
        write(root, "lib/models/user.g.dart", "part of 'user.dart';\n");
        write(
            root,
            "lib/models/user.freezed.dart",
            "part of 'user.dart';\n",
        );
        write(root, "lib/gen/assets.gen.dart", "class Assets {}\n");
        write(root, "test/mocks.mocks.dart", "class MockA {}\n");
        write(root, "lib/router.gr.dart", "class R {}\n");
        write(root, "lib/proto/a.pb.dart", "class A {}\n");
        write(root, "lib/proto/a.pbenum.dart", "class A {}\n");
        write(root, "lib/proto/a.pbjson.dart", "class A {}\n");
        write(root, "lib/proto/a.pbserver.dart", "class A {}\n");
        write(root, "lib/env.config.dart", "const k = 1;\n");
    }

    const DART_GENERATED: [&str; 9] = [
        "lib/gen/assets.gen.dart",
        "lib/models/user.freezed.dart",
        "lib/models/user.g.dart",
        "lib/proto/a.pb.dart",
        "lib/proto/a.pbenum.dart",
        "lib/proto/a.pbjson.dart",
        "lib/proto/a.pbserver.dart",
        "lib/router.gr.dart",
        "test/mocks.mocks.dart",
    ];

    #[test]
    fn dart_sources_are_admitted_and_generated_dart_stays_noise() {
        let none: &[String] = &[];
        for path in DART_GENERATED {
            assert!(is_noise_path(path, none), "{path}");
            assert!(
                !is_generated_only_noise(path, none),
                "{path} is dropped, not handed to the architectural tier"
            );
        }
        for path in [
            "lib/main.dart",
            "lib/firebase_options.dart",
            "lib/env.config.dart",
        ] {
            assert!(!is_noise_path(path, none), "{path}");
            assert!(has_code_ext(path), "{path}");
        }
    }

    fn expect_dart_scan(extracted: &ExtractedRepo) {
        assert_eq!(
            paths(extracted),
            [
                "lib/env.config.dart",
                "lib/firebase_options.dart",
                "lib/main.dart"
            ]
        );
        assert!(reference_paths(extracted).is_empty());
    }

    #[test]
    fn dart_sources_are_admitted_and_generated_dart_is_dropped_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        dart_fixture(dir.path());
        expect_dart_scan(&scan(dir.path()));
    }

    #[test]
    fn dart_sources_are_admitted_and_generated_dart_is_counted_as_noise_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        dart_fixture(dir.path());
        let extracted = scan(dir.path());
        let mut excluded = extracted.excluded_noise_paths.clone();
        excluded.sort();
        assert_eq!(excluded, DART_GENERATED);
        expect_dart_scan(&extracted);
    }

    #[test]
    fn the_firebase_hosting_cache_folder_is_noise() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".firebase/hosting.abc.cache", "a,b\n");
        write(dir.path(), ".firebase/hosting.abc.json", "{}\n");
        write(dir.path(), ".firebaserc", "{}\n");
        let none: &[String] = &[];
        assert!(is_noise_path(".firebase/hosting.abc.cache", none));
        assert!(!is_noise_path(".firebaserc", none));
        assert_eq!(paths(&scan(dir.path())), [".firebaserc"]);
    }

    #[test]
    fn plist_config_is_admitted_as_config_text() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "ios/Runner/GoogleService-Info.plist",
            "<plist></plist>\n",
        );
        assert!(is_admissible_text("ios/Runner/GoogleService-Info.plist"));
        assert_eq!(
            paths(&scan(dir.path())),
            ["ios/Runner/GoogleService-Info.plist"]
        );
    }

    #[test]
    fn a_rules_file_from_another_tool_is_admitted_as_config_text() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "etc/udev/70-usb.rules", "ACTION==\"add\"\n");
        write(dir.path(), "monitoring/alerts.rules", "groups: []\n");
        assert_eq!(
            paths(&scan(dir.path())),
            ["etc/udev/70-usb.rules", "monitoring/alerts.rules"]
        );
    }

    /// 24a changed something for Flutter: a credential in a Dart source file is now read by the
    /// secrets floor. The key is assembled at runtime so no literal sits in this source.
    #[test]
    fn a_secret_planted_in_a_dart_file_is_now_caught_by_the_secrets_floor() {
        let dir = tempfile::tempdir().unwrap();
        let key = format!("sk_live_{}", "Zq9Xk3Pm7Rt2Vw5Yb8Nc4Jd6");
        write(
            dir.path(),
            "lib/config.dart",
            &format!("const stripeKey = '{key}';\n"),
        );
        write(dir.path(), "lib/main.dart", "void main() {}\n");
        let extracted = scan(dir.path());
        let findings = crate::onboard::audit::audit_files("o/r", &extracted.files);
        assert!(
            findings
                .iter()
                .any(|f| f.path == "lib/config.dart" && f.rule_id == "SEC-NO-VENDOR-TOKEN-1"),
            "the floor must read lib/config.dart: {findings:#?}"
        );
    }
}
