//! Local repo file reading: walks a working tree, prunes noise, and returns auditable files —
//! both code (full treatment: stack detection, architectural checkers, AI review) and
//! non-code human-authored text/config where a committed secret is just as real a finding
//! (`.env*`, `.yaml`/`.json`/`.toml`, `.conf`/`.properties`, key material, dotfiles). See
//! `is_admissible_text` for the admission policy and why broadening it doesn't create noise
//! elsewhere.

use super::scope::RepoScope;

/// Safety net for pathological monorepos so one scan can't exhaust memory. This
/// is NOT a per-scan window that rotates: a single tarball download covers the
/// WHOLE repo, and only a repo with more than this many auditable files is
/// truncated (and the report says so). Normal repos are fully scanned.
pub(crate) const HARD_CAP_FILES: usize = 20_000;
/// The reason a file with a line longer than the room's read page is not read ("has a line longer
/// than 100 KB"), from the room's own page limit so the two never differ.
pub fn long_line_label() -> String {
    // In the base sizes are printed in (1 KB = 1,024 bytes), rounded down, so a file shown with
    // this reason really has a line longer than the number printed.
    format!(
        "has a line longer than {} KB",
        crate::ai_audit::agent_finder::room::MAX_READ_LINE_BYTES / 1024
    )
}

/// Files larger than this are not read for the inspection. An admitted text file over it (and not
/// excluded by agreement) is a pre-start warning, never a refusal, and is listed under Coverage;
/// the secret checks still search it up to [`MAX_SECRET_SCAN_BYTES`].
pub(crate) const MAX_FILE_BYTES: usize = 1024 * 1024;

/// The largest file whose contents the secret checks search when it is over [`MAX_FILE_BYTES`]
/// (20 MB). Such a file is still not read for the inspection, billed or counted; only the
/// secret-shaped floor rules look inside it. Over this, only its file name is checked.
pub(crate) const MAX_SECRET_SCAN_BYTES: usize = 20 * 1024 * 1024;

/// The secret search ceiling as a client reads it ("20 MB"), taken from [`MAX_SECRET_SCAN_BYTES`].
pub fn secret_scan_label() -> String {
    format!("{} MB", MAX_SECRET_SCAN_BYTES / (1024 * 1024))
}

/// What reading one file for the secret checks gave.
#[derive(Debug, PartialEq, Eq)]
pub enum SecretRead {
    /// The file's text.
    Text(String),
    /// More than [`MAX_SECRET_SCAN_BYTES`] bytes: the file grew after the walk measured it. Only
    /// its name is checked, like any file over the ceiling.
    OverCeiling,
    /// Unreadable, or not text this scan can decode (UTF-8, or UTF-16 with a byte order mark).
    NotText,
}

/// Read `rel` under `root` for the secret checks only. At most [`MAX_SECRET_SCAN_BYTES`] + 1 bytes
/// are read, so a file that grew after the walk cannot be pulled whole into memory. The caller
/// holds one file at a time.
pub fn read_for_secret_search(root: &std::path::Path, rel: &str) -> SecretRead {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(root.join(rel)) else {
        return SecretRead::NotText;
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_SECRET_SCAN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return SecretRead::NotText;
    }
    if bytes.len() > MAX_SECRET_SCAN_BYTES {
        return SecretRead::OverCeiling;
    }
    match decode_text(&bytes) {
        Some(text) => SecretRead::Text(text),
        None => SecretRead::NotText,
    }
}

/// The read limit as a client reads it ("1 MB"), taken from [`MAX_FILE_BYTES`] so a message,
/// a label and a test never state a number the engine does not apply.
pub fn max_file_label() -> String {
    format!("{} MB", MAX_FILE_BYTES / (1024 * 1024))
}

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
    // H8: build and script source the walk used to drop unseen. Gradle build scripts (Groovy and
    // Kotlin) hold signing passwords and repository tokens; Scala, C++ headers and PowerShell and
    // batch scripts are hand-written source and a common home of inline credentials.
    "gradle", "kts", "scala", "cc", "hpp", "ps1", "bat",
    // H8: single-file components. Their `<script>` is source code the model never saw while they
    // were only reference text for the dead-code rule (which still reads them: the architectural
    // tier is handed the audited files and the reference list together).
    "vue", "svelte", "astro",
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
    // H8: secret-bearing files the walk used to drop unseen. A `.key` is a private key by
    // convention (the secret-file rule names the extension); `.tfvars` and `.tfstate` hold
    // Terraform variables and state, which carry credentials in clear; `.p8` is an Apple signing
    // key; `.xml` and `.config` hold connection strings and API keys (`web.config`, Maven
    // `settings.xml`, Android resources); a notebook's outputs keep whatever was printed.
    "key",
    "tfvars",
    "tfstate",
    "p8",
    "xml",
    "config",
    "ipynb",
];

/// Extensionless file names that hold credentials, admitted by name because they have no
/// extension to match (H8): the AWS shared credentials file, a kubeconfig, and the default SSH
/// private key names with any suffix (`id_rsa_deploy`). A public key (`id_rsa.pub`) has an
/// extension and is not one of these.
fn is_credential_basename(basename: &str) -> bool {
    const PRIVATE_KEY_PREFIXES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
    basename == "credentials"
        || basename == "kubeconfig"
        || PRIVATE_KEY_PREFIXES.iter().any(|p| basename.starts_with(p))
}

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
    // The extension of the file NAME, not of the whole path: `home/.aws/credentials` has a dot in
    // a directory, and splitting the path on its last dot read `aws/credentials` as an extension.
    match basename.rsplit_once('.') {
        Some((_, ext)) => CONFIG_TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        // Extensionless, non-dotfile basename. Deliberately NOT broadened to "any extensionless
        // file" (that would sweep up compiled binaries and other noise): `CODE_BASENAMES`
        // (checked above via `has_code_ext`) and `is_credential_basename` are the explicit
        // opt-in lists for this narrower case.
        None => is_credential_basename(basename),
    }
}

/// Extensions of REFERENCE TEXT: files that are not audited as code and carry no committed-secret
/// signal of their own, but whose text can REFER to a source file by name (a `<script src>` in
/// HTML, a run instruction in a README, a stylesheet `@import`; Vue, Svelte and Astro components
/// are audited source now (H8) and reach the dead-code rule with the audited files). The dead-code rule needs them to see that a file is still used; nothing else
/// reads them. See [`ExtractedRepo::reference_files`].
const REFERENCE_TEXT_EXTS: &[&str] = &[
    "html", "htm", "md", "mdx", "css", "scss", "less", "ejs", "erb", "hbs", "pug", "njk", "j2",
    "jinja", "jinja2",
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

/// Whether a folder name is one the walk never reads (build output, dependencies, caches, or a
/// folder named by `CAMERATA_SCAN_EXCLUDE_DIRS`).
pub(crate) fn is_noise_dir_name(name: &str) -> bool {
    NOISE_DIRS.contains(&name) || extra_exclude_dirs().iter().any(|d| d == name)
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
    /// Admissible files NOT read because they exceed [`MAX_FILE_BYTES`], by path. Listed under
    /// Coverage in the report and `json/inspection.json`, so a skipped file is named, never silent.
    pub skipped_oversize: Vec<String>,
    /// The files in `skipped_oversize` that are not over [`MAX_SECRET_SCAN_BYTES`]: the floor
    /// opens each one, one at a time, for the secret checks only. Paths only, never contents.
    pub secret_searchable: Vec<String>,
    /// Admissible files NOT read because the walk had already admitted [`HARD_CAP_FILES`], by
    /// path. Non-empty exactly when `truncated` is true. Listed under Coverage like
    /// `skipped_oversize`.
    pub skipped_beyond_cap: Vec<String>,
    /// H9: admissible files NOT read because their bytes are not text this scan can decode
    /// (binary key stores, files in a legacy code page), by path. Named under Coverage, and the
    /// path-only floor rules still run over each, so a `.p12` is flagged by its name though it
    /// could not be read.
    pub skipped_unreadable: Vec<String>,
    /// Admissible files NOT read because a line is longer than the room's read page
    /// ([`long_line_label`]): the room could not show such a file in full. Treated like
    /// `skipped_oversize`: a pre-start warning, never a refusal.
    pub skipped_long_line: Vec<String>,
    /// The decoded text of each file in `skipped_long_line`, by path. Held for the deterministic
    /// floor's secret-shaped rules only (`audit::audit_long_line_secrets`): the file is not read,
    /// billed or counted, but a token or key inside it must still be found. Never persisted.
    pub long_line_contents: Vec<(String, String)>,
    /// The size in bytes of each file named in the three `skipped_*` lists, by path, so Coverage
    /// states how big the file was. Every skipped file the walk could measure is here.
    pub skipped_sizes: std::collections::HashMap<String, u64>,
    /// The file limit this walk applied: [`HARD_CAP_FILES`] in production.
    pub file_cap: usize,
    pub excluded_noise: usize,
    /// The paths behind `excluded_noise` (always the same length), so `camerata count` can size
    /// what the filter dropped without a second walk.
    pub excluded_noise_paths: Vec<String>,
    /// Files the project's exclusion list names ([`super::scope`]) that the run would otherwise
    /// have read: not read, not counted, listed by path under Coverage as "excluded by
    /// agreement". Their sizes are in `skipped_sizes`.
    pub excluded_by_agreement: Vec<String>,
    /// The indexes (into the project's list) of every pattern that matched at least one walked
    /// file, of any kind. A pattern in no repo's list of these matches nothing.
    pub matched_patterns: Vec<usize>,
}

/// A file the run cannot read because it is over [`MAX_FILE_BYTES`] and the project's exclusion
/// list does not name it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OverLimitFile {
    pub path: String,
    pub bytes: u64,
    /// Why it cannot be read: empty for the 1 MB file size limit, else [`long_line_label`].
    #[serde(default)]
    pub reason: String,
    /// The path as an Excluded by agreement pattern writes it (`repo/path`, or `owner/repo/path`
    /// when another repo in the project has the same name). Filled by the preflight route.
    #[serde(default)]
    pub display: String,
}

impl ExtractedRepo {
    /// The admitted files over the read limit, with their sizes, in path order. A run with any
    /// still starts, with a warning (the operator may exclude each by agreement); `camerata count`
    /// and the desktop's pre-start check list them.
    pub fn over_limit_files(&self) -> Vec<OverLimitFile> {
        let mut out: Vec<OverLimitFile> = self
            .skipped_oversize
            .iter()
            .map(|path| OverLimitFile {
                path: path.clone(),
                bytes: self.skipped_sizes.get(path).copied().unwrap_or(0),
                display: String::new(),
                reason: String::new(),
            })
            .collect();
        out.extend(self.skipped_long_line.iter().map(|path| OverLimitFile {
            path: path.clone(),
            bytes: self.skipped_sizes.get(path).copied().unwrap_or(0),
            display: String::new(),
            reason: long_line_label(),
        }));
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Put every list in path order. The directory walks list entries in whatever order the
    /// filesystem returns them (sorted on APFS, hash order on ext4), and everything downstream
    /// of this struct (the audit digest the model sees, finding order, which files survive a
    /// cap) would otherwise depend on that. Sorting here makes the same tree produce the same
    /// extraction on every filesystem.
    fn sorted(mut self) -> Self {
        self.files.sort_by(|a, b| a.0.cmp(&b.0));
        self.reference_files.sort_by(|a, b| a.0.cmp(&b.0));
        self.excluded_noise_paths.sort();
        self.excluded_by_agreement.sort();
        self
    }

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
pub fn read_local_repo_files(
    root: &std::path::Path,
    scope: &RepoScope,
) -> anyhow::Result<ExtractedRepo> {
    read_local_repo_files_capped(root, HARD_CAP_FILES, scope)
}

/// [`read_local_repo_files`] with the file limit as a parameter: [`HARD_CAP_FILES`] in
/// production, a small number in tests of what a repo past the limit reports.
pub(crate) fn read_local_repo_files_capped(
    root: &std::path::Path,
    cap: usize,
    scope: &RepoScope,
) -> anyhow::Result<ExtractedRepo> {
    // A folder that is gone, or a path that is not a folder, is a repo that could not be read.
    // Walking it would return no files and no error, which reads as an empty repo with nothing
    // wrong in it.
    if !root.is_dir() {
        anyhow::bail!(
            "the repository folder could not be read: it is not a folder on this machine"
        );
    }
    let extra_dirs = extra_exclude_dirs();
    if root.join(".git").exists() {
        read_local_repo_files_gitignore(root, &extra_dirs, cap, scope)
    } else {
        read_local_repo_files_noise_denylist(root, &extra_dirs, cap, scope)
    }
}

/// Where the walk puts each admissible (non-noise, auditable) file: read into `files`, or
/// named in `oversize` / `beyond_cap` when it is not read. Every admissible file lands in
/// exactly one of the three, so nothing the scan would have audited is dropped silently (H4).
struct AdmittedSink {
    cap: usize,
    files: Vec<(String, String)>,
    oversize: Vec<String>,
    secret_searchable: Vec<String>,
    beyond_cap: Vec<String>,
    unreadable: Vec<String>,
    long_line: Vec<String>,
    /// The decoded text of each `long_line` file, kept for the secrets floor only.
    long_line_contents: Vec<(String, String)>,
    /// Sizes of the files in `oversize`, `beyond_cap` and `unreadable`, by path.
    sizes: std::collections::HashMap<String, u64>,
}

impl AdmittedSink {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            files: Vec::new(),
            oversize: Vec::new(),
            secret_searchable: Vec::new(),
            beyond_cap: Vec::new(),
            unreadable: Vec::new(),
            long_line: Vec::new(),
            long_line_contents: Vec::new(),
            sizes: std::collections::HashMap::new(),
        }
    }

    /// Admit `rel` (at `full`, `len` bytes on disk; `None` when the size could not be read,
    /// which skips it as before: an unreadable entry is not a file the walk can name honestly).
    /// Returns whether it was read into `files`.
    fn admit(&mut self, rel: String, full: &std::path::Path, len: Option<u64>) -> bool {
        let Some(len) = len else {
            return false;
        };
        if len as usize > MAX_FILE_BYTES {
            self.sizes.insert(rel.clone(), len);
            if len as usize <= MAX_SECRET_SCAN_BYTES {
                self.secret_searchable.push(rel.clone());
            }
            self.oversize.push(rel);
            return false;
        }
        if self.files.len() >= self.cap {
            self.sizes.insert(rel.clone(), len);
            self.beyond_cap.push(rel);
            return false;
        }
        // H9: a file that is not valid UTF-8 is decoded when it is UTF-16 with a byte order mark
        // (what Windows tools and SQL Server write), and otherwise named as unreadable. It is
        // never dropped without a trace.
        let Some(content) = std::fs::read(full).ok().and_then(|b| decode_text(&b)) else {
            self.sizes.insert(rel.clone(), len);
            self.unreadable.push(rel);
            return false;
        };
        // A line the room's Read page cannot carry means the file cannot be read in full: it is
        // named, not read for the inspection, and warned about like a file over the size limit.
        let longest = content.lines().map(str::len).max().unwrap_or(0);
        if longest > crate::ai_audit::agent_finder::room::MAX_READ_LINE_BYTES {
            self.sizes.insert(rel.clone(), len);
            self.long_line.push(rel.clone());
            self.long_line_contents.push((rel, content));
            return false;
        }
        self.files.push((rel, content));
        true
    }

    fn truncated(&self) -> bool {
        !self.beyond_cap.is_empty()
    }
}

/// Everything a walk collects, and the ONE place a walked file is classified. The three walk
/// bodies (gitignore walk, `git ls-files` union, non-git DFS) each call [`WalkSink::classify`]
/// for every file they reach, so a rule about which files are read lives here and nowhere else.
struct WalkSink {
    extra_dirs: Vec<String>,
    scope: RepoScope,
    excluded_by_agreement: Vec<String>,
    matched_patterns: std::collections::BTreeSet<usize>,
    admitted: AdmittedSink,
    excluded_noise_paths: Vec<String>,
    reference: ReferenceSink,
}

impl WalkSink {
    fn new(extra_dirs: &[String], cap: usize, scope: &RepoScope) -> Self {
        Self {
            extra_dirs: extra_dirs.to_vec(),
            scope: scope.clone(),
            excluded_by_agreement: Vec::new(),
            matched_patterns: std::collections::BTreeSet::new(),
            admitted: AdmittedSink::new(cap),
            excluded_noise_paths: Vec::new(),
            reference: ReferenceSink::new(HARD_CAP_REFERENCE_FILES),
        }
    }

    /// Classify `rel` (at `full`): counted as excluded noise, offered as reference text, or
    /// admitted for reading. `len` is asked for only when the file is admissible; `None` (a
    /// size that cannot be read, or an entry that is gone) skips it.
    fn classify(&mut self, rel: String, full: &std::path::Path, len: impl FnOnce() -> Option<u64>) {
        // Belt-and-suspenders: noise denylist on top of gitignore (catches projects whose
        // build artefacts are not yet gitignored, or repos where target/ was committed).
        let noise = is_noise_path(&rel, &self.extra_dirs);
        let admissible = is_admissible_text(&rel);
        // The project's exclusion list decides first and for every reader: a file named there is
        // neither read nor offered as reference text, and one the run would otherwise have read
        // is named, with its size, so the report lists it as excluded by agreement.
        let hits = self.scope.matching(&rel);
        self.matched_patterns.extend(hits.iter().copied());
        if !hits.is_empty() {
            // An agreed exclusion wins over every other class: never read, never counted in any
            // figure (noise included), named only as excluded by agreement.
            if admissible {
                if let Some(len) = len() {
                    self.admitted.sizes.insert(rel.clone(), len);
                }
                self.excluded_by_agreement.push(rel);
            }
            return;
        }
        if noise && admissible {
            self.excluded_noise_paths.push(rel.clone());
        }
        if !noise && !admissible {
            self.reference.offer(&rel, full);
        } else if noise && admissible && is_generated_only_noise(&rel, &self.extra_dirs) {
            self.reference.accept(&rel, full);
        }
        if noise || !admissible {
            return;
        }
        // Past the cap the walk continues, naming each further admissible file instead of
        // reading it, so the report can list what the cap left out.
        self.admitted.admit(rel, full, len());
    }

    fn finish(self) -> ExtractedRepo {
        let truncated = self.admitted.truncated();
        ExtractedRepo {
            files: self.admitted.files,
            reference_files: self.reference.files,
            truncated,
            reference_truncated: self.reference.truncated,
            skipped_oversize: self.admitted.oversize,
            secret_searchable: self.admitted.secret_searchable,
            skipped_beyond_cap: self.admitted.beyond_cap,
            skipped_unreadable: self.admitted.unreadable,
            skipped_long_line: self.admitted.long_line,
            long_line_contents: self.admitted.long_line_contents,
            skipped_sizes: self.admitted.sizes,
            file_cap: self.admitted.cap,
            excluded_noise: self.excluded_noise_paths.len(),
            excluded_noise_paths: self.excluded_noise_paths,
            excluded_by_agreement: self.excluded_by_agreement,
            matched_patterns: self.matched_patterns.into_iter().collect(),
        }
        .sorted()
    }
}

/// The text in `bytes`: UTF-8, or UTF-16 announced by a byte order mark. `None` for anything else
/// (binary, or a legacy code page), which the caller names as unreadable rather than guessing.
fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Some(text.to_string());
    }
    let (body, little_endian) = match bytes {
        [0xFF, 0xFE, rest @ ..] => (rest, true),
        [0xFE, 0xFF, rest @ ..] => (rest, false),
        _ => return None,
    };
    if body.len() % 2 != 0 {
        return None;
    }
    let units = body.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

/// Gitignore-aware walk (used when `.git` exists). Delegates to the `ignore` crate so
/// `.gitignore`, `.git/info/exclude`, and the user's global gitignore are all respected.
/// The noise denylist is applied afterward as a belt-and-suspenders guard. `cap` is
/// [`HARD_CAP_FILES`] in production; tests pass a small one.
fn read_local_repo_files_gitignore(
    root: &std::path::Path,
    extra_dirs: &[String],
    cap: usize,
    scope: &RepoScope,
) -> anyhow::Result<ExtractedRepo> {
    let mut sink = WalkSink::new(extra_dirs, cap, scope);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // hidden(false): we want to scan dotfiles like .env and .camerata/ — the `ignore`
    // crate would otherwise skip them by default.
    let walker = ignore::WalkBuilder::new(root)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(false)
        // Walk in path order, so which files survive a cap is the same on every filesystem.
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();

    for result in walker {
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
        sink.classify(rel, p, || entry.metadata().ok().map(|m| m.len()));
    }

    // Union in git-TRACKED files the ignore-aware walk above skipped because they match a
    // `.gitignore` pattern. A committed secret doesn't stop being committed (or stop being
    // reachable via `git show HEAD:<path>` / the full clone history) just because a later
    // `.gitignore` edit tries to hide it going forward — that's the single highest-value case
    // for this scan to catch, not a gap to leave open. `git_tracked_files` returns `None` on
    // any failure (no `git` on PATH, etc.); the primary walk above already succeeded
    // regardless, so this is purely additive and never turns a working scan into an error.
    // It runs past the cap too, so a tracked file beyond the cap is named like any other.
    {
        if let Some(tracked) = git_tracked_files(root) {
            for rel in tracked {
                if seen.contains(&rel) {
                    continue;
                }
                let full = root.join(&rel);
                // Tracked in the index but not present on disk (rare edge case) has no size.
                let len = || {
                    std::fs::metadata(&full)
                        .ok()
                        .filter(std::fs::Metadata::is_file)
                        .map(|m| m.len())
                };
                seen.insert(rel.clone());
                sink.classify(rel, &full, len);
            }
        }
    }

    Ok(sink.finish())
}

/// The full set of git-TRACKED file paths under `root` (relative, forward-slashed), via
/// `git ls-files -z` (NUL-separated so paths with spaces/unicode parse unambiguously, and so
/// a path itself can never be mistaken for the separator). Returns `None` on any failure (no
/// `git` binary on PATH, non-zero exit, non-UTF-8 output, ...) — the caller treats that as "no
/// union available," never a hard error: the primary gitignore-aware walk already ran and
/// still stands on its own.
fn git_tracked_files(root: &std::path::Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .args(crate::workspace::GIT_NO_REPO_COMMANDS)
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
    cap: usize,
    scope: &RepoScope,
) -> anyhow::Result<ExtractedRepo> {
    let mut sink = WalkSink::new(extra_dirs, cap, scope);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sorted by name, so which files survive a cap is the same on every filesystem.
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
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
            sink.classify(rel, &p, || entry.metadata().ok().map(|m| m.len()));
        }
    }
    Ok(sink.finish())
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
        read_local_repo_files(root, &crate::onboard::scope::RepoScope::none()).unwrap()
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
            // Single-file components are audited source now (H8), not reference text.
            "src/App.vue",
            "src/Page.svelte",
            "src/pages/index.astro",
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
        for path in ["index.html", "README.md", "a.css", "Makefile"] {
            assert!(
                !is_admissible_text(path),
                "{path} must stay out of the audited file list"
            );
        }
    }

    fn reference_fixture(root: &std::path::Path) {
        write(root, "src/a.ts", "export const a = 1;\n");
        write(root, "index.html", "<script src=\"/src/a.ts\"></script>\n");
        write(
            root,
            "views/page.ejs",
            "<script src=\"/src/a.ts\"></script>\n",
        );
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
                "styles/a.css",
                "views/page.ejs"
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

    /// A tree with enough files and directories, created in reverse order, that a scanner
    /// returning filesystem order would differ from path order on at least one filesystem.
    fn many_files_fixture(root: &std::path::Path) {
        for dir in ["zeta", "alpha", "mid", "beta", "src/z", "src/a"] {
            for i in (0..12).rev() {
                write(root, &format!("{dir}/f{i:02}.ts"), "export {};\n");
            }
        }
        write(root, "z-last.ts", "export {};\n");
        write(root, "a-first.ts", "export {};\n");
    }

    fn expect_path_order(extracted: &ExtractedRepo) {
        let order: Vec<&str> = extracted.files.iter().map(|(p, _)| p.as_str()).collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order.len(), 6 * 12 + 2, "every fixture file is admitted");
        assert_eq!(
            order, sorted,
            "the scanner must return files in path order, not filesystem order"
        );
    }

    #[test]
    fn scan_returns_files_in_stable_sorted_order_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        many_files_fixture(dir.path());
        expect_path_order(&scan(dir.path()));
    }

    #[test]
    fn scan_returns_files_in_stable_sorted_order_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        many_files_fixture(dir.path());
        expect_path_order(&scan(dir.path()));
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

    // ── H4: no silent loss — skipped files are named, by path ──────────────────

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    fn oversize_fixture(root: &std::path::Path) {
        write(root, "src/a.rs", "fn a() {}\n");
        write(root, "src/huge.rs", &"x".repeat(MAX_FILE_BYTES + 1));
        // Not admissible at all (no code extension), so not "skipped for size" either.
        write(root, "assets/huge.bin", &"x".repeat(MAX_FILE_BYTES + 1));
    }

    #[test]
    fn a_file_over_the_size_cap_is_listed_by_path_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        oversize_fixture(dir.path());
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["src/a.rs"]);
        assert_eq!(extracted.skipped_oversize, ["src/huge.rs"]);
        assert!(extracted.skipped_beyond_cap.is_empty());
    }

    #[test]
    fn a_file_over_the_size_cap_is_listed_by_path_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        oversize_fixture(dir.path());
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["src/a.rs"]);
        assert_eq!(extracted.skipped_oversize, ["src/huge.rs"]);
    }

    #[test]
    fn a_tracked_but_gitignored_oversize_file_is_listed_by_path() {
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
        write(root, "src/a.rs", "fn a() {}\n");
        write(root, "gen/big.rs", &"x".repeat(MAX_FILE_BYTES + 1));
        assert!(git(&["add", "-f", "src/a.rs", "gen/big.rs"]));
        write(root, ".gitignore", "gen/\n");
        let extracted = scan(root);
        assert_eq!(extracted.skipped_oversize, ["gen/big.rs"]);
    }

    fn cap_fixture(root: &std::path::Path) {
        for i in 0..5 {
            write(root, &format!("src/f{i}.rs"), "fn f() {}\n");
        }
        write(
            root,
            "README.md",
            "not admissible, never counted against the cap\n",
        );
    }

    #[test]
    fn files_beyond_the_file_cap_are_listed_by_path_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        cap_fixture(dir.path());
        let extracted = read_local_repo_files_noise_denylist(
            dir.path(),
            &[],
            3,
            &crate::onboard::scope::RepoScope::none(),
        )
        .expect("walk succeeds");
        assert!(extracted.truncated);
        assert_eq!(extracted.files.len(), 3);
        assert_eq!(extracted.skipped_beyond_cap.len(), 2);
        let mut all: Vec<String> = extracted.files.iter().map(|(p, _)| p.clone()).collect();
        all.extend(extracted.skipped_beyond_cap.clone());
        assert_eq!(
            sorted(all),
            [
                "src/f0.rs",
                "src/f1.rs",
                "src/f2.rs",
                "src/f3.rs",
                "src/f4.rs"
            ],
            "every admissible file is either read or named as beyond the cap"
        );
    }

    #[test]
    fn files_beyond_the_file_cap_are_listed_by_path_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        cap_fixture(dir.path());
        let extracted = read_local_repo_files_gitignore(
            dir.path(),
            &[],
            3,
            &crate::onboard::scope::RepoScope::none(),
        )
        .expect("walk succeeds");
        assert!(extracted.truncated);
        assert_eq!(extracted.files.len(), 3);
        let mut all: Vec<String> = extracted.files.iter().map(|(p, _)| p.clone()).collect();
        all.extend(extracted.skipped_beyond_cap.clone());
        assert_eq!(
            sorted(all),
            [
                "src/f0.rs",
                "src/f1.rs",
                "src/f2.rs",
                "src/f3.rs",
                "src/f4.rs"
            ]
        );
    }

    #[test]
    fn a_tree_under_the_cap_lists_nothing_as_skipped() {
        let dir = tempfile::tempdir().unwrap();
        cap_fixture(dir.path());
        let extracted = scan(dir.path());
        assert!(!extracted.truncated);
        assert!(extracted.skipped_beyond_cap.is_empty());
        assert!(extracted.skipped_oversize.is_empty());
    }

    // ── H8 / H9: secret-bearing and source files the walk used to drop unseen ───────────────

    #[test]
    fn h8_files_that_carry_secrets_or_source_are_admitted() {
        for yes in [
            "deploy/server.key",
            "infra/prod.tfvars",
            "infra/terraform.tfstate",
            "keys/AuthKey_ABC123.p8",
            "android/app/build.gradle",
            "android/app/build.gradle.kts",
            "settings.kts",
            "config/web.config",
            "pom.xml",
            "src/Main.scala",
            "src/util.cc",
            "include/util.hpp",
            "scripts/deploy.ps1",
            "scripts/deploy.bat",
            "notebooks/analysis.ipynb",
            "src/App.vue",
            "src/Page.svelte",
            "src/pages/index.astro",
            "home/.aws/credentials",
            "credentials",
            "kubeconfig",
            "ops/kubeconfig",
            "keys/id_rsa",
            "keys/id_ed25519",
            "keys/id_rsa_deploy",
        ] {
            assert!(is_admissible_text(yes), "{yes} must be audited");
        }
        for no in ["README", "notes", "keys/id_rsa.pub", "logo.png", "Makefile"] {
            assert!(!is_admissible_text(no), "{no} must stay out");
        }
    }

    #[test]
    fn h9_a_pem_private_key_file_is_read_and_reaches_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "deploy/server.key",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeKLs1Pt8Qu\n-----END RSA PRIVATE KEY-----\n",
        );
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["deploy/server.key"]);
        let findings = crate::onboard::audit::audit_files("o/r", &extracted.files);
        assert!(
            findings
                .iter()
                .any(|f| f.path == "deploy/server.key" && f.rule_id == "SEC-NO-SECRET-FILE-1"),
            "{findings:#?}"
        );
    }

    #[test]
    fn h9_a_utf16_file_with_a_byte_order_mark_is_decoded_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let text = "CREATE TABLE users (id int);\r\n";
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let full = dir.path().join("db/schema.sql");
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, &bytes).unwrap();
        let big_endian: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        fs::write(dir.path().join("db/other.sql"), &big_endian).unwrap();
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["db/other.sql", "db/schema.sql"]);
        assert!(extracted
            .files
            .iter()
            .all(|(_, content)| content.contains("CREATE TABLE users")));
        assert!(extracted.skipped_unreadable.is_empty());
    }

    #[test]
    fn h9_a_file_that_is_not_text_is_named_as_skipped_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("deploy")).unwrap();
        fs::write(
            dir.path().join("deploy/store.p12"),
            [0x30, 0x82, 0x00, 0xFF, 0xFE],
        )
        .unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["src/main.rs"]);
        assert_eq!(extracted.skipped_unreadable, ["deploy/store.p12"]);
    }

    #[test]
    fn h9_the_path_rule_runs_over_a_file_the_walk_could_not_read() {
        let findings = crate::onboard::audit::audit_unread_paths(
            "o/r",
            &[
                "deploy/store.p12".to_string(),
                "src/huge.rs".to_string(),
                "keys/big.pem".to_string(),
            ],
            &[],
        );
        let hits: Vec<&str> = findings.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(hits, ["deploy/store.p12", "keys/big.pem"], "{findings:#?}");
        assert!(findings
            .iter()
            .all(|f| f.rule_id == "SEC-NO-SECRET-FILE-1" && f.line == 0));
    }

    #[test]
    fn h8_a_tfstate_with_a_single_line_json_escaped_pem_is_flagged_and_never_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let body = "MIIBOgIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeKLs1Pt8QuKUpRKfFLfRYC9AIKjbJTWit";
        // Terraform state writes a PEM on one line, newlines as JSON escapes.
        let state = format!(
            "{{\"resources\":[{{\"instances\":[{{\"attributes\":{{\"private_key_pem\":\
             \"-----BEGIN RSA PRIVATE KEY-----\\n{body}\\n-----END RSA PRIVATE KEY-----\\n\"}}}}]}}]}}\n"
        );
        write(dir.path(), "infra/terraform.tfstate", &state);
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["infra/terraform.tfstate"]);
        let findings = crate::onboard::audit::audit_files("o/r", &extracted.files);
        assert!(
            findings.iter().any(|f| f.path == "infra/terraform.tfstate"),
            "the state file is flagged: {findings:#?}"
        );
        for f in &findings {
            let shown = format!("{} {} {:?}", f.snippet, f.detail, f.captures);
            assert!(
                !shown.contains(&body[..24]),
                "the key material must not be quoted: {shown}"
            );
        }
    }

    /// The secret search reads at most the ceiling plus one byte: a file that grew after the walk
    /// is over the ceiling, never read whole.
    #[test]
    fn the_secret_search_read_is_capped_at_the_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "small.js", "var a = 1;\n");
        assert_eq!(
            read_for_secret_search(dir.path(), "small.js"),
            SecretRead::Text("var a = 1;\n".to_string())
        );
        let at = dir.path().join("at.js");
        std::fs::write(&at, vec![b'x'; MAX_SECRET_SCAN_BYTES]).unwrap();
        assert!(matches!(
            read_for_secret_search(dir.path(), "at.js"),
            SecretRead::Text(_)
        ));
        let grown = dir.path().join("grown.js");
        let f = std::fs::File::create(&grown).unwrap();
        f.set_len(MAX_SECRET_SCAN_BYTES as u64 + 1).unwrap();
        assert_eq!(
            read_for_secret_search(dir.path(), "grown.js"),
            SecretRead::OverCeiling
        );
        std::fs::write(dir.path().join("bin.p12"), [0xFFu8; 64]).unwrap();
        assert_eq!(
            read_for_secret_search(dir.path(), "bin.p12"),
            SecretRead::NotText
        );
        assert_eq!(
            read_for_secret_search(dir.path(), "gone.js"),
            SecretRead::NotText
        );
    }

    // ── Item 31: the exclusion list (excluded by agreement) and the 1 MiB read limit ────────

    fn scan_excluding(root: &std::path::Path, patterns: &[&str]) -> ExtractedRepo {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        let scope = crate::onboard::scope::ScanScope::from_patterns(&patterns)
            .unwrap()
            .for_repo("acme/app");
        read_local_repo_files(root, &scope).unwrap()
    }

    fn exclusion_fixture(root: &std::path::Path) {
        write(root, "src/a.rs", "fn a() {}\n");
        write(
            root,
            "seeds/data.js",
            "const rows = [1, 2, 3];\n\nexport default rows;\n",
        );
        write(root, "seeds/notes.md", "# seed notes, reference text\n");
        write(root, "seeds/package-lock.json", "{}\n");
    }

    fn excluded_expectations(extracted: &ExtractedRepo) {
        let read: Vec<String> = paths(extracted)
            .into_iter()
            .filter(|p| p != ".gitignore")
            .collect();
        assert_eq!(read, ["src/a.rs"]);
        // The noise file in the folder is named too: an agreed exclusion wins over noise.
        assert_eq!(
            extracted.excluded_by_agreement,
            ["seeds/data.js", "seeds/package-lock.json"]
        );
        assert_eq!(
            extracted.skipped_sizes.get("seeds/data.js").copied(),
            Some(46),
            "the size of an excluded file is recorded for Coverage"
        );
        assert!(
            extracted
                .reference_files
                .iter()
                .all(|(p, _)| !p.starts_with("seeds/")),
            "an excluded folder gives no reference text: {:?}",
            extracted.reference_files
        );
        assert!(
            extracted
                .excluded_noise_paths
                .iter()
                .all(|p| p != "seeds/data.js"),
            "an excluded file is not also counted as noise"
        );
    }

    #[test]
    fn an_excluded_file_is_not_read_and_is_named_in_a_non_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        exclusion_fixture(dir.path());
        excluded_expectations(&scan_excluding(dir.path(), &["app/seeds/**"]));
    }

    #[test]
    fn an_excluded_file_is_not_read_and_is_named_in_a_git_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        exclusion_fixture(dir.path());
        excluded_expectations(&scan_excluding(dir.path(), &["app/seeds/**"]));
    }

    #[test]
    fn an_excluded_file_is_not_read_when_it_is_tracked_but_gitignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            matches!(
                std::process::Command::new("git").args(args).current_dir(root).status(),
                Ok(s) if s.success()
            )
        };
        if !git(&["init", "-q"]) {
            return; // no git on PATH: the union path is not exercisable here
        }
        exclusion_fixture(root);
        assert!(git(&[
            "add",
            "-f",
            "src/a.rs",
            "seeds/data.js",
            "seeds/notes.md",
            "seeds/package-lock.json"
        ]));
        write(root, ".gitignore", "seeds/\n");
        excluded_expectations(&scan_excluding(root, &["app/seeds/**"]));
    }

    #[test]
    fn without_an_exclusion_the_same_files_are_read() {
        let dir = tempfile::tempdir().unwrap();
        exclusion_fixture(dir.path());
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["seeds/data.js", "src/a.rs"]);
        assert!(extracted.excluded_by_agreement.is_empty());
        assert_eq!(extracted.excluded_noise_paths, ["seeds/package-lock.json"]);
        assert!(extracted
            .reference_files
            .iter()
            .any(|(p, _)| p == "seeds/notes.md"));
    }

    /// RULING (coordinator, round 1 item 5): an agreed exclusion wins over every other class.
    /// A noise file the list names is never read, never counted in any figure (not as noise
    /// either), and named only as excluded by agreement.
    #[test]
    fn an_exclusion_by_agreement_wins_over_noise() {
        let dir = tempfile::tempdir().unwrap();
        exclusion_fixture(dir.path());
        let extracted = scan_excluding(dir.path(), &["app/seeds/package-lock.json"]);
        assert_eq!(extracted.excluded_by_agreement, ["seeds/package-lock.json"]);
        assert!(
            extracted.excluded_noise_paths.is_empty(),
            "{:?}",
            extracted.excluded_noise_paths
        );
        assert_eq!(extracted.excluded_noise, 0);
    }

    #[test]
    fn an_over_limit_file_that_is_excluded_by_agreement_blocks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn a() {}\n");
        write(dir.path(), "seeds/big.js", &"x".repeat(MAX_FILE_BYTES + 1));
        let plain = scan(dir.path());
        assert_eq!(plain.skipped_oversize, ["seeds/big.js"]);
        assert_eq!(
            plain.over_limit_files(),
            [OverLimitFile {
                path: "seeds/big.js".to_string(),
                bytes: MAX_FILE_BYTES as u64 + 1,
                display: String::new(),
                reason: String::new(),
            }]
        );
        let agreed = scan_excluding(dir.path(), &["app/seeds/big.js"]);
        assert!(agreed.skipped_oversize.is_empty());
        assert!(agreed.over_limit_files().is_empty());
        assert_eq!(agreed.excluded_by_agreement, ["seeds/big.js"]);
    }

    /// Item 31 step 4: the read limit is 1 MiB, the number every label takes from the constant.
    #[test]
    fn the_read_limit_is_one_mebibyte_and_a_file_at_the_limit_is_read() {
        assert_eq!(MAX_FILE_BYTES, 1024 * 1024);
        assert_eq!(max_file_label(), "1 MB");
        let dir = tempfile::tempdir().unwrap();
        // Short lines: a single 1 MiB line would also have a line longer than the read page.
        write(
            dir.path(),
            "src/at_limit.rs",
            &"x\n".repeat(MAX_FILE_BYTES / 2),
        );
        write(dir.path(), "src/over.rs", &"x".repeat(MAX_FILE_BYTES + 1));
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["src/at_limit.rs"]);
        assert_eq!(extracted.skipped_oversize, ["src/over.rs"]);
    }

    /// Item 31 step 1: the walks classify through one function, so a rule added there reaches all
    /// three. A second `is_admissible_text(&rel)` call in a walk body would be a copy of it.
    #[test]
    fn the_three_walks_classify_through_one_function() {
        let source = include_str!("files.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert_eq!(
            production.matches("is_admissible_text(&rel)").count(),
            1,
            "only WalkSink::classify decides whether a walked file is admissible"
        );
        assert_eq!(
            production.matches("sink.classify(").count(),
            3,
            "the gitignore walk, the git ls-files union and the plain walk each classify through it"
        );
    }

    /// Gap 5 (coordinator ruling): an admitted file with a line longer than the room's read page
    /// cannot be read in full, so it is treated like an over-limit file: not read for the
    /// inspection, listed with the reason, and warned about at the start (never a refusal).
    #[test]
    fn a_file_with_a_line_longer_than_the_read_page_is_not_read_and_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn a() {}\n");
        write(
            dir.path(),
            "data/ok.js",
            &format!("{}\n", "x".repeat(90_000)),
        );
        write(
            dir.path(),
            "data/wide.js",
            &format!("var a;\n{}\n", "y".repeat(100_001)),
        );
        let extracted = scan(dir.path());
        assert_eq!(paths(&extracted), ["data/ok.js", "src/a.rs"]);
        assert_eq!(extracted.skipped_long_line, ["data/wide.js"]);
        let listed = extracted.over_limit_files();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "data/wide.js");
        assert_eq!(listed[0].reason, long_line_label());
        assert_eq!(long_line_label(), "has a line longer than 97 KB");
        // Excluded by agreement, it no longer blocks.
        let agreed = scan_excluding(dir.path(), &["app/data/wide.js"]);
        assert!(agreed.over_limit_files().is_empty());
    }
}
