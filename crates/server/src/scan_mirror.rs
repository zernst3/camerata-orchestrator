//! The scan's private copy of a repository: a "mirror".
//!
//! Every external tool the scan starts on a repository (semgrep, eslint, ruff, `cargo clippy`,
//! osv-scanner) runs on the mirror, never on the client's checkout. A code tool's mirror holds
//! exactly the ADMITTED set ([`admitted_paths`]): the files `camerata count` counts and Coverage
//! lists. osv-scanner's mirror adds the dependency files (manifests and lockfiles) its audit
//! reads, vendored ones included ([`osv_paths`]). Same relative paths; never a symbolic link or a
//! `.git` folder, so no tool can follow a link out of the repository and quote a file that is not
//! the client's.
//!
//! # Where it lives
//!
//! `<state dir>/scan-mirrors/<run-id>/mirror/<repo-label>/` (private, mode 0700), where the state
//! dir is the one `CAMERATA_STATE_DIR` controls (the run store's), with a separate
//! `mirror-target/<repo-label>/` next to it for `cargo`'s build output (`CARGO_TARGET_DIR`).
//! Never inside the client clone and never in `/tmp`. A test process never writes under the real
//! state dir: see [`mirrors_root_for`].
//!
//! # Lifecycle
//!
//! Built once per repository before its tools run. Removed by [`Mirror::finish`] when the
//! repository's tools are done; [`Drop`] removes it on any other exit (a failure, a panic, or a
//! stopped run, whose future is dropped). A process killed outright leaves its folder behind, so
//! the next build sweeps stale run folders. Removal only ever touches a folder under the run's
//! own folder ([`remove_mirror_dir`] refuses anything else).
//!
//! # Paths
//!
//! The layout is identical to the repository's, so a tool run with the mirror as its working
//! directory reports repo-relative paths, or absolute paths under the mirror that
//! [`crate::scan_tools::repo_relative_path`] makes relative with the mirror as the root. Either
//! way the engine sees the client's real relative paths.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::onboard::files::ExtractedRepo;

/// Free space kept beyond the staged files' size. Cloning needs almost none; a plain-copy
/// fallback needs all of it, so the check is made for the worse case.
pub const DISK_MARGIN_BYTES: u64 = 1 << 30;

/// A run folder this old, left by a process that was killed, is swept at the next build.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Name prefix of every run folder this module makes (the sweep touches only these).
const RUN_PREFIX: &str = "scan-";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Why a mirror could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorError {
    /// Not enough free disk space for the staged files plus [`DISK_MARGIN_BYTES`].
    LowDisk { needed: u64, available: u64 },
    /// Any other failure, in words.
    Io(String),
}

impl std::fmt::Display for MirrorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LowDisk { needed, available } => write!(
                f,
                "there is not enough free disk space for the private copy of the repository the \
                 tools run on ({} MB needed, {} MB free)",
                needed / (1024 * 1024),
                available / (1024 * 1024)
            ),
            Self::Io(e) => write!(
                f,
                "the private copy of the repository the tools run on could not be made: {e}"
            ),
        }
    }
}

impl std::error::Error for MirrorError {}

impl From<io::Error> for MirrorError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// One coverage note per tool that did not run on `repo` because its mirror could not be made
/// (`why` says so in words). A failed part, never a silent skip: it blocks a client export until
/// the run is resumed.
pub fn not_run_notes(
    repo: &str,
    tools: Vec<String>,
    why: &str,
) -> Vec<crate::onboard::CoverageNote> {
    tools
        .into_iter()
        .map(|tool| crate::onboard::CoverageNote {
            tool,
            message: format!("The tool did not run on this repository: {why}."),
            repo: Some(repo.to_string()),
            tool_failed: true,
        })
        .collect()
}

/// The [`CoverageNote::tool`] of a note about files that could not be copied into a repository's
/// private copy. Deliberately not a tool's own name: a note keyed by a tool's name reads, when the
/// ledger is reconciled, as that tool having failed or not run, and every rule of that tool with
/// no finding would then read "did not run" (see `reconcile_external_tool_ledger`).
pub const MIRROR_NOTE_TOOL: &str = "private-copy";

/// The phrase every not-copied note carries. The dependency audit's own readers
/// ([`crate::dep_audit::note_proves_clean`], [`crate::dep_audit::note_means_scanned`]) key off it:
/// a lockfile that did not reach osv-scanner never reads as "no known vulnerable dependencies",
/// and a run that did happen never reads as not run.
pub const NOT_COPIED_PHRASE: &str = "could not be copied into the private copy of the repository";

/// One coverage note, keyed `tool_id`, naming the files that could not be copied into the mirror
/// and so were not checked by the tools that run on it. A disclosed limit, not a failed part (the
/// tools ran on the rest). `None` when nothing failed.
pub fn not_copied_note(
    repo: &str,
    tool_id: &str,
    files: &[String],
) -> Option<crate::onboard::CoverageNote> {
    if files.is_empty() {
        return None;
    }
    const NAMED: usize = 10;
    let mut list = files
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() > NAMED {
        list.push_str(&format!(" and {} more", files.len() - NAMED));
    }
    Some(crate::onboard::CoverageNote {
        tool: tool_id.to_string(),
        message: format!(
            "{} file(s) {NOT_COPIED_PHRASE} the tools run on, so those tools did not check them: \
             {list}.",
            files.len()
        ),
        repo: Some(repo.to_string()),
        tool_failed: false,
    })
}

/// The repo-relative paths a code tool (semgrep, eslint, ruff, clippy) is given: the ADMITTED set,
/// the files `camerata count` counts and Coverage lists. That is the files the reader read plus
/// the admitted files it listed as not read (too big, not decodable, a very long line, generated,
/// a type it does not inspect yet, past the file cap). Reference text, build or dependency noise
/// and files excluded by agreement are not in it. Sorted, no duplicates.
pub fn admitted_paths(ex: &ExtractedRepo) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    all.extend(ex.files.iter().map(|(p, _)| p.clone()));
    all.extend(ex.skipped_oversize.iter().cloned());
    all.extend(ex.skipped_beyond_cap.iter().cloned());
    all.extend(ex.skipped_unreadable.iter().cloned());
    all.extend(ex.skipped_long_line.iter().cloned());
    all.extend(ex.generated.iter().cloned());
    all.extend(ex.not_inspected.iter().cloned());
    all.sort();
    all.dedup();
    all
}

/// The Cargo files a `cargo clippy` build needs beyond the admitted source: the manifest, the
/// lockfile and the toolchain pin.
const CARGO_BUILD_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// What the code tools (semgrep, eslint, ruff, clippy) are given: the admitted set plus the Cargo
/// files the build needs, and nothing else. A finding in any other file cannot exist, so it
/// cannot reach the report.
pub fn code_tool_paths(
    ex: &ExtractedRepo,
    root: &Path,
    scope: &crate::onboard::scope::RepoScope,
) -> Vec<String> {
    let mut all = admitted_paths(ex);
    all.extend(
        dependency_files(root, scope)
            .into_iter()
            .filter(|p| CARGO_BUILD_FILES.contains(&p.rsplit('/').next().unwrap_or(p.as_str()))),
    );
    all.sort();
    all.dedup();
    all
}

/// What osv-scanner is given: the admitted set plus every dependency file (manifest or lockfile),
/// INCLUDING those inside noise folders such as `vendor/`, because a vulnerable package in a
/// vendored lockfile must still be found. See [`dependency_files`].
pub fn osv_paths(
    ex: &ExtractedRepo,
    root: &Path,
    scope: &crate::onboard::scope::RepoScope,
) -> Vec<String> {
    let mut all = admitted_paths(ex);
    all.extend(dependency_files(root, scope));
    all.sort();
    all.dedup();
    all
}

/// Every dependency file under `root`: in a git checkout the TRACKED ones (`git ls-files`, so a
/// gitignored or untracked lockfile is out), anywhere else the regular ones the reader's own walk
/// reaches (it prunes noise folders and `.git`). In a git checkout noise folders such as `vendor/`
/// are included. Names come from
/// [`crate::dep_audit::is_dependency_file`]. Files the project's exclusion list names are left
/// out: they are never read, so their package names never leave the machine. Sorted. Links are refused again when the mirror is
/// built.
pub fn dependency_files(root: &Path, scope: &crate::onboard::scope::RepoScope) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if root.join(".git").exists() {
        if let Some(tracked) = git_tracked(root) {
            found.extend(tracked.into_iter().filter(|p| {
                !p.split('/').any(|c| c == ".git")
                    && crate::dep_audit::is_dependency_file(p.rsplit('/').next().unwrap_or(p))
            }));
            found.retain(|p| !scope.excludes(p));
            found.sort();
            return found;
        }
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_dir() {
                if name != ".git" && !crate::onboard::files::is_noise_dir_name(&name) {
                    stack.push(entry.path());
                }
            } else if kind.is_file() && crate::dep_audit::is_dependency_file(&name) {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    found.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    found.retain(|p| !scope.excludes(p));
    found.sort();
    found
}

/// `git ls-files -z` (tracked files) under `root`, or `None` when git cannot answer.
fn git_tracked(root: &Path) -> Option<Vec<String>> {
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
            .filter(|p| !p.is_empty())
            .map(|p| p.replace('\\', "/"))
            .collect(),
    )
}

/// True when `rel` names a regular file under `root` with no symbolic link in ANY component, the
/// last included. Local twin of `onboard::files::is_regular_file_inside` (same rule; this one
/// exists so the mirror does not wait on that module).
pub fn is_regular_file_inside(root: &Path, rel: &str) -> bool {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel_path.is_absolute() {
        return false;
    }
    let mut cur = root.to_path_buf();
    let mut components = rel_path.components().peekable();
    while let Some(c) = components.next() {
        let Component::Normal(part) = c else {
            return false;
        };
        cur.push(part);
        let Ok(md) = std::fs::symlink_metadata(&cur) else {
            return false;
        };
        let ft = md.file_type();
        if ft.is_symlink() {
            return false;
        }
        if components.peek().is_some() {
            if !ft.is_dir() {
                return false;
            }
        } else {
            return ft.is_file();
        }
    }
    false
}

fn sanitize_label(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('.').to_string();
    if s.is_empty() {
        "repo".to_string()
    } else {
        s
    }
}

fn fresh_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{RUN_PREFIX}{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Create `path` (and parents) readable by the owner only.
fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Remove `path` only when it is a real folder strictly inside `run_root`. Refuses (with
/// `PermissionDenied`) a path with `..`, the run folder itself, anything outside it, a symbolic
/// link, and anything whose real location is outside the run folder's real location. A path that
/// does not exist is already removed.
pub fn remove_mirror_dir(run_root: &Path, path: &Path) -> io::Result<()> {
    let refuse = |why: &str| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing to remove a folder outside the run's own folder ({why})"),
        ))
    };
    let has_parent_ref = |p: &Path| p.components().any(|c| matches!(c, Component::ParentDir));
    if has_parent_ref(path) || has_parent_ref(run_root) {
        return refuse("parent reference");
    }
    if path == run_root || !path.starts_with(run_root) {
        return refuse("not under the run folder");
    }
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(md) if md.file_type().is_symlink() => return refuse("symbolic link"),
        Ok(_) => {}
    }
    let (Ok(real_path), Ok(real_root)) = (path.canonicalize(), run_root.canonicalize()) else {
        return refuse("unresolvable");
    };
    if real_path == real_root || !real_path.starts_with(&real_root) {
        return refuse("resolves outside the run folder");
    }
    std::fs::remove_dir_all(path)
}

/// Folder name, under the state dir, that holds every run's mirrors.
const MIRRORS_DIR: &str = "scan-mirrors";

/// Where mirrors are made: `<state dir>/scan-mirrors`, the state dir being the one
/// `CAMERATA_STATE_DIR` controls. A test process that resolved to the REAL per-user state dir is
/// sent to a temp folder instead, so no test ever writes under it.
pub fn mirrors_root() -> PathBuf {
    let state = crate::scan_cache::state_dir::dir();
    let real = crate::scan_cache::state_dir::dir_from(
        None,
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
    );
    let test_process = std::env::current_exe()
        .ok()
        .is_some_and(|exe| crate::tool_provisioning::is_cargo_test_binary(&exe));
    mirrors_root_for(&state, &real, test_process)
}

/// The pure decision behind [`mirrors_root`].
pub fn mirrors_root_for(state: &Path, real_state: &Path, test_process: bool) -> PathBuf {
    let same = state == real_state
        || matches!(
            (state.canonicalize(), real_state.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        );
    if test_process && same {
        return std::env::temp_dir().join("camerata-test").join(MIRRORS_DIR);
    }
    state.join(MIRRORS_DIR)
}

/// Remove stale mirror folders: run folders under `runs_root` named `scan-*` not modified for
/// `max_age`. Best effort; a failure leaves the folder for the next sweep.
pub fn sweep_stale(runs_root: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(runs_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(RUN_PREFIX) {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        if !md.is_dir() {
            continue;
        }
        let old = md
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if !old || owner_is_alive(&name) {
            continue;
        }
        let run_root = entry.path();
        for sub in ["mirror", "mirror-target"] {
            let _ = remove_mirror_dir(&run_root, &run_root.join(sub));
        }
        let _ = std::fs::remove_dir(&run_root);
    }
}

/// True when the process that made run folder `name` (`scan-<pid>-...`) is still running, so its
/// mirror is in use and must not be swept.
fn owner_is_alive(name: &str) -> bool {
    let Some(pid) = name
        .strip_prefix(RUN_PREFIX)
        .and_then(|rest| rest.split('-').next())
        .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    else {
        return false;
    };
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The `CARGO_TARGET_DIR` for a tool run whose working directory is a mirror made by this module
/// (`<runs>/scan-*/mirror/<label>`): the sibling `mirror-target/<label>`. `None` for any other
/// directory, so a tool run on an ordinary folder builds where it always did.
pub fn cargo_target_dir_for(dir: &Path) -> Option<PathBuf> {
    let parent = dir.parent()?;
    if parent.file_name()? != "mirror" {
        return None;
    }
    let run_root = parent.parent()?;
    let run_name = run_root.file_name()?.to_string_lossy();
    if !run_name.starts_with(RUN_PREFIX) || run_root.parent()?.file_name()? != MIRRORS_DIR {
        return None;
    }
    Some(run_root.join("mirror-target").join(dir.file_name()?))
}

/// The `GIT_CEILING_DIRECTORIES` for a tool run on a mirror folder (`<run>/mirror/<label>`): the
/// `mirror` folder, so git never looks above the mirror for a parent repository's settings. `None`
/// for any other directory.
pub fn git_ceiling_for(dir: &Path) -> Option<PathBuf> {
    cargo_target_dir_for(dir)?;
    dir.parent().map(Path::to_path_buf)
}

/// `text` with the mirror's folder replaced by `.`, so a tool's message names the repository's
/// relative paths and never the private copy's location.
pub fn scrub_mirror_path(text: &str, mirror: &Path) -> String {
    let mut roots: Vec<String> = vec![mirror.to_string_lossy().into_owned()];
    if let Ok(canonical) = std::fs::canonicalize(mirror) {
        roots.push(canonical.to_string_lossy().into_owned());
    }
    if let Some(target) = cargo_target_dir_for(mirror) {
        roots.push(target.to_string_lossy().into_owned());
    }
    roots.retain(|r| r.len() > 1);
    roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
    roots.dedup();
    let mut out = text.to_string();
    for root in &roots {
        out = out
            .replace(&format!("{root}/"), "")
            .replace(root.as_str(), ".");
    }
    out
}

/// The regular files of `rels` with their sizes, and how many were left out (a link anywhere on
/// the path, or a file that is gone).
fn plan_files(real: &Path, rels: &[String]) -> (Vec<(String, u64)>, usize) {
    let mut plan: Vec<(String, u64)> = Vec::new();
    let mut skipped = 0usize;
    let mut seen = std::collections::HashSet::new();
    for rel in rels {
        if !seen.insert(rel.as_str()) {
            continue;
        }
        if !is_regular_file_inside(real, rel) {
            skipped += 1;
            continue;
        }
        let size = std::fs::symlink_metadata(real.join(rel))
            .map(|m| m.len())
            .unwrap_or(0);
        plan.push((rel.clone(), size));
    }
    (plan, skipped)
}

/// Most files that may fail to copy before the mirror is refused, as a share of the plan, and the
/// number below which a failed share never refuses it.
const COPY_FAIL_MIN: usize = 10;
const COPY_FAIL_PERCENT: usize = 5;

/// True for a full disk: the one copy error that says every later copy will fail too.
fn is_disk_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull || e.raw_os_error() == Some(28)
}

/// Whether `failed` files out of `total` is too many to run the tools on what is left.
fn too_many_failed(failed: usize, total: usize) -> bool {
    failed > COPY_FAIL_MIN && failed * 100 > total * COPY_FAIL_PERCENT
}

/// Create `root` (private) and copy the planned files into it. A file that cannot be copied
/// (it vanished, is unreadable) is skipped and returned, never fatal on its own, and any partial
/// copy of it is removed so no tool scans half a file. A full disk, or a failed share above a
/// small threshold, is an error: the tools would run on a near empty copy.
/// The function that copies one file (`std::fs::copy` in production).
pub type CopyFn = dyn Fn(&Path, &Path) -> io::Result<u64>;

fn stage(
    copier: &CopyFn,
    real: &Path,
    plan: &[(String, u64)],
    root: &Path,
) -> io::Result<Vec<String>> {
    private_dir(root)?;
    let mut failed: Vec<String> = Vec::new();
    for (rel, _) in plan {
        let src = real.join(rel);
        let dest = root.join(rel);
        let made = dest
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| copy_one(copier, &src, &dest));
        if let Err(e) = made {
            if is_disk_full(&e) {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "the disk filled up while the private copy was being made",
                ));
            }
            failed.push(rel.clone());
            if too_many_failed(failed.len(), plan.len()) {
                return Err(io::Error::other(format!(
                    "{} of {} files could not be copied into the private copy",
                    failed.len(),
                    plan.len()
                )));
            }
        }
    }
    Ok(failed)
}

/// A staged copy of one repository's admitted files. See the module documentation.
#[derive(Debug)]
pub struct Mirror {
    run_root: PathBuf,
    root: PathBuf,
    target: PathBuf,
    removed: bool,
    /// Paths named for staging that were not regular files inside the repository (a link, a
    /// vanished file) and so were left out.
    pub skipped: usize,
    /// Files that were staged but could not be copied (see [`stage`]), repo-relative.
    pub copy_failed: Vec<String>,
}

impl Mirror {
    /// The mirror's folder: the working directory and target of every tool.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Where `cargo` writes build output for this mirror (removed with it).
    pub fn target_dir(&self) -> &Path {
        &self.target
    }

    /// The run folder this mirror lives in.
    pub fn run_root(&self) -> &Path {
        &self.run_root
    }

    /// Build a mirror of `real` holding `rels` under a fresh run folder in the state dir.
    /// Blocking work runs off the async runtime.
    pub async fn build(real: &Path, rels: Vec<String>, label: &str) -> Result<Self, MirrorError> {
        let runs = mirrors_root();
        let run_root = runs.join(fresh_run_id());
        let real = real.to_path_buf();
        let label = label.to_string();
        tokio::task::spawn_blocking(move || {
            sweep_stale(&runs, STALE_AFTER);
            Self::build_blocking(&real, &rels, &run_root, &label, &|p| {
                fs2::available_space(p).ok()
            })
        })
        .await
        .map_err(|e| MirrorError::Io(e.to_string()))?
    }

    /// [`Mirror::build`] with the run folder and the free-space query given, so both are
    /// testable. `available` returns the free bytes at a path, or `None` when it cannot be asked
    /// (then the build goes ahead).
    pub fn build_blocking(
        real: &Path,
        rels: &[String],
        run_root: &Path,
        label: &str,
        available: &dyn Fn(&Path) -> Option<u64>,
    ) -> Result<Self, MirrorError> {
        Self::build_with_copier(
            &|from: &Path, to: &Path| std::fs::copy(from, to),
            real,
            rels,
            run_root,
            label,
            available,
        )
    }

    /// [`Mirror::build_blocking`] with the function that copies one file given, so a copy that
    /// fails midway can be exercised.
    pub fn build_with_copier(
        copier: &CopyFn,
        real: &Path,
        rels: &[String],
        run_root: &Path,
        label: &str,
        available: &dyn Fn(&Path) -> Option<u64>,
    ) -> Result<Self, MirrorError> {
        let label = sanitize_label(label);

        // Only regular files with no link anywhere on the path are staged; this is the guard, not
        // the list alone.
        let (plan, skipped) = plan_files(real, rels);
        let needed = plan
            .iter()
            .map(|(_, s)| *s)
            .sum::<u64>()
            .saturating_add(DISK_MARGIN_BYTES);

        private_dir(run_root)?;
        // The real path, so a tool that reports canonical paths (macOS `/private/...`) reports
        // paths under the same root the engine strips.
        let run_root = &run_root
            .canonicalize()
            .unwrap_or_else(|_| run_root.to_path_buf());
        let root = run_root.join("mirror").join(&label);
        let target = run_root.join("mirror-target").join(&label);
        // From here on the guard removes whatever was made, on every exit.
        let mirror = Self {
            run_root: run_root.to_path_buf(),
            root: root.clone(),
            target,
            removed: false,
            skipped,
            copy_failed: Vec::new(),
        };
        if let Some(free) = available(run_root) {
            if free < needed {
                return Err(MirrorError::LowDisk {
                    needed,
                    available: free,
                });
            }
        }
        let mut mirror = mirror;
        mirror.copy_failed = stage(copier, real, &plan, &root)?;
        Ok(mirror)
    }

    /// Remove the mirror (and its cargo target folder) now, off the async runtime.
    pub async fn finish(mut self) {
        match tokio::task::spawn_blocking(move || self.remove_now()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("the scan's private repository copy was not removed: {e}"),
            Err(e) => tracing::warn!("removing the scan's private repository copy failed: {e}"),
        }
    }

    /// Remove the mirror now. Safe to call twice.
    pub fn remove_now(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        remove_mirror_dir(&self.run_root, &self.root)?;
        remove_mirror_dir(&self.run_root, &self.target)?;
        for dir in [
            self.run_root.join("mirror"),
            self.run_root.join("mirror-target"),
            self.run_root.clone(),
        ] {
            // Only empty folders go; a folder with anything else in it stays.
            let _ = std::fs::remove_dir(dir);
        }
        self.removed = true;
        Ok(())
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        if let Err(e) = self.remove_now() {
            tracing::warn!("the scan's private repository copy was not removed: {e}");
        }
    }
}

/// Copy one file with [`std::fs::copy`]. On macOS (Rust 1.95.0,
/// `library/std/src/sys/fs/unix.rs`, `copy` for Apple targets) that first tries `fclonefileat`
/// (an APFS copy-on-write clone) and falls back to `fcopyfile` where cloning is refused (another
/// volume, not APFS); elsewhere it is a plain copy.
fn copy_one(copier: &CopyFn, src: &Path, dest: &Path) -> io::Result<()> {
    if let Err(e) = copier(src, dest) {
        // A copy that failed midway leaves a partial file; it must not be scanned or listed.
        let _ = std::fs::remove_file(dest);
        return Err(e);
    }
    if let Ok(modified) = std::fs::metadata(src).and_then(|m| m.modified()) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(dest) {
            let _ = f.set_modified(modified);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::scope::RepoScope;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    const SECRET: &str = "acme-outside-secret-content";

    /// Neutral fixture: regular files, a lockfile, build noise, a `.git` file, and links that
    /// point OUTSIDE the repo (one to a file, one to a folder). Returns `(outside, repo)`.
    #[cfg(unix)]
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let outside = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write(outside.path(), "acme-outside.js", SECRET);
        write(outside.path(), "acme-outdir/acme-inner.js", SECRET);
        write(repo.path(), "src/acme-ok.js", "export const a = 1;\n");
        write(
            repo.path(),
            "package.json",
            "{\"dependencies\":{\"acme\":\"1\"}}",
        );
        write(repo.path(), "package-lock.json", "{}");
        write(repo.path(), "node_modules/acme-dep/index.js", "x");
        write(repo.path(), ".git/HEAD", "ref: refs/heads/main\n");
        std::os::unix::fs::symlink(
            outside.path().join("acme-outside.js"),
            repo.path().join("src/acme-link.js"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("acme-outdir"),
            repo.path().join("acme-linkdir"),
        )
        .unwrap();
        (outside, repo)
    }

    fn extracted(repo: &Path) -> ExtractedRepo {
        crate::onboard::files::read_local_repo_files(repo, &RepoScope::none()).unwrap()
    }

    /// What a code tool's mirror holds: the admitted set.
    fn staged_for(repo: &Path) -> Vec<String> {
        admitted_paths(&extracted(repo))
    }

    fn plenty(_: &Path) -> Option<u64> {
        Some(u64::MAX)
    }

    fn walk(root: &Path) -> Vec<(PathBuf, std::fs::FileType)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let ft = e.file_type().unwrap();
                if ft.is_dir() {
                    stack.push(e.path());
                }
                out.push((e.path(), ft));
            }
        }
        out
    }

    /// A repository with noise folders, a vendored lockfile, generated files, an ignored folder
    /// and links, optionally a git checkout.
    #[cfg(unix)]
    fn parity_fixture(git: bool) -> (tempfile::TempDir, tempfile::TempDir) {
        let (outside, repo) = fixture();
        let r = repo.path();
        write(r, "vendor/acme-lib/package-lock.json", "{}");
        write(r, "vendor/acme-lib/index.js", "var v = 1;");
        write(r, "dist/bundle.min.js", "var a=1;");
        write(r, "generated/acme.gen.ts", "export {};");
        write(r, "ignored/acme-build.js", "var i = 1;");
        write(r, "vendor/acme-ignored/package-lock.json", "{}");
        write(
            r,
            ".gitignore",
            "ignored/\nvendor/acme-ignored/\nnode_modules/\n",
        );
        // `fixture()` plants a `.git/HEAD` file; start from a clean slate either way.
        std::fs::remove_dir_all(r.join(".git")).unwrap();
        if git {
            let ok = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(r)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git init");
            let ok = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(r)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git add");
        }
        (outside, repo)
    }

    /// The set of regular files in a folder, relative, sorted.
    #[cfg(unix)]
    fn files_in(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = walk(root)
            .into_iter()
            .filter(|(_, ft)| ft.is_file())
            .map(|(p, _)| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// The mirror a code tool (semgrep, eslint, ruff, clippy) runs on is exactly the admitted set
    /// `camerata count` counts and Coverage lists: no lockfile, no noise folder, no link.
    #[cfg(unix)]
    #[test]
    fn a_code_tools_mirror_is_exactly_the_admitted_set() {
        for git in [false, true] {
            let (_outside, repo) = parity_fixture(git);
            let ex = extracted(repo.path());
            let admitted = admitted_paths(&ex);
            let run = tempfile::tempdir().unwrap();
            let m = Mirror::build_blocking(repo.path(), &admitted, run.path(), "acme/web", &plenty)
                .unwrap();
            // The reader's tracked-file union can name a tracked link; the mirror refuses it.
            let regular: Vec<String> = admitted
                .iter()
                .filter(|p| is_regular_file_inside(repo.path(), p))
                .cloned()
                .collect();
            assert_eq!(files_in(m.path()), regular, "git={git}");
            assert_eq!(m.skipped, admitted.len() - regular.len());
            assert!(admitted.iter().any(|p| p == "src/acme-ok.js"));
            if git {
                // The staged set: a gitignored hand-written file is not in it, so no tool is
                // ever given it (eslint used to lint it; that findings path is closed).
                assert!(
                    !admitted.iter().any(|p| p == "src/acme-x.js"),
                    "{admitted:?}"
                );
            }
            for out in [
                "package-lock.json",
                "vendor/acme-lib/package-lock.json",
                "dist/bundle.min.js",
            ] {
                assert!(
                    !admitted.iter().any(|p| p == out),
                    "{out} admitted: {admitted:?}"
                );
            }
        }
    }

    /// osv-scanner's mirror: the admitted set plus the regular dependency files, vendored ones
    /// included, a gitignored one and every link left out.
    #[cfg(unix)]
    #[test]
    fn osv_mirror_is_the_admitted_set_plus_regular_dependency_files() {
        let (outside, repo) = parity_fixture(true);
        write(outside.path(), "acme-outside-package-lock.json", "{}");
        std::os::unix::fs::symlink(
            outside.path().join("acme-outside-package-lock.json"),
            repo.path().join("acme-linked-package-lock.json"),
        )
        .unwrap();
        let ex = extracted(repo.path());
        let paths = osv_paths(&ex, repo.path(), &RepoScope::none());
        let run = tempfile::tempdir().unwrap();
        let m =
            Mirror::build_blocking(repo.path(), &paths, run.path(), "acme/web", &plenty).unwrap();
        let held = files_in(m.path());
        for want in [
            "package-lock.json",
            "package.json",
            "vendor/acme-lib/package-lock.json",
            "src/acme-ok.js",
        ] {
            assert!(held.iter().any(|p| p == want), "{want} missing: {held:?}");
        }
        for unwanted in [
            "vendor/acme-ignored/package-lock.json",
            "acme-linked-package-lock.json",
            "acme-new/package-lock.json",
            "dist/bundle.min.js",
        ] {
            assert!(
                !held.iter().any(|p| p == unwanted),
                "{unwanted} held: {held:?}"
            );
        }
        assert!(walk(m.path()).iter().all(|(_, ft)| !ft.is_symlink()));
    }

    /// An outside-pointing link is in no mirror, code tool or osv.
    #[cfg(unix)]
    #[test]
    fn an_outside_pointing_link_is_in_no_mirror() {
        let (_outside, repo) = parity_fixture(true);
        let ex = extracted(repo.path());
        for paths in [
            admitted_paths(&ex),
            osv_paths(&ex, repo.path(), &RepoScope::none()),
        ] {
            let run = tempfile::tempdir().unwrap();
            let m = Mirror::build_blocking(repo.path(), &paths, run.path(), "r", &plenty).unwrap();
            assert!(walk(m.path()).iter().all(|(_, ft)| !ft.is_symlink()));
            let out = std::process::Command::new("grep")
                .args(["-r", SECRET, "."])
                .current_dir(m.path())
                .output()
                .unwrap();
            assert!(out.stdout.is_empty(), "outside content reachable: {out:?}");
        }
    }

    /// A dependency file the project's agreement excludes is never given to osv-scanner (its
    /// package names would leave the machine).
    #[cfg(unix)]
    #[test]
    fn an_excluded_dependency_file_is_not_in_osv_s_mirror() {
        let (_outside, repo) = parity_fixture(true);
        let patterns = vec!["web/vendor/acme-lib/package-lock.json".to_string()];
        let scope = crate::onboard::scope::ScanScope::from_patterns(&patterns)
            .unwrap()
            .for_repo("acme/web");
        let ex = crate::onboard::files::read_local_repo_files(repo.path(), &scope).unwrap();
        let paths = osv_paths(&ex, repo.path(), &scope);
        assert!(
            !paths.iter().any(|p| p.starts_with("vendor/acme-lib/")),
            "{paths:?}"
        );
        assert!(paths.iter().any(|p| p == "package-lock.json"), "{paths:?}");
        assert!(!dependency_files(repo.path(), &scope)
            .iter()
            .any(|p| p.starts_with("vendor/acme-lib/")));
    }

    /// One file that cannot be copied is skipped and counted; the rest of the mirror is built.
    #[cfg(unix)]
    #[test]
    fn one_copy_error_does_not_fail_the_mirror() {
        use std::os::unix::fs::PermissionsExt;
        let (_outside, repo) = fixture();
        let bad = repo.path().join("src/acme-unreadable.js");
        write(repo.path(), "src/acme-unreadable.js", "var u = 1;");
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&bad).is_ok() {
            // Running as a user that can read anything: the error cannot be provoked here.
            return;
        }
        let run = tempfile::tempdir().unwrap();
        let rels = vec![
            "src/acme-ok.js".to_string(),
            "src/acme-unreadable.js".to_string(),
        ];
        let m = Mirror::build_blocking(repo.path(), &rels, run.path(), "r", &plenty).unwrap();
        assert!(m.path().join("src/acme-ok.js").is_file());
        assert_eq!(m.copy_failed, ["src/acme-unreadable.js"]);
        let note = not_copied_note("acme/web", MIRROR_NOTE_TOOL, &m.copy_failed).expect("note");
        assert!(
            note.message.contains("src/acme-unreadable.js"),
            "{}",
            note.message
        );
        assert!(note.message.contains(NOT_COPIED_PHRASE));
        // Keyed by its own id, never by a tool's name.
        assert_eq!(note.tool, "private-copy");
        assert!(!note.tool_failed);
        // No half-copied file is left for a tool to scan.
        assert!(!m.path().join("src/acme-unreadable.js").exists());
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// A full disk, or too many failed copies, refuses the mirror (a failed part that can be
    /// resumed) instead of running the tools on a near empty copy.
    #[cfg(unix)]
    #[test]
    fn a_full_disk_or_too_many_failed_copies_refuses_the_mirror() {
        use std::os::unix::fs::PermissionsExt;
        assert!(is_disk_full(&io::Error::from_raw_os_error(28)));
        assert!(!is_disk_full(&io::Error::from_raw_os_error(13)));
        assert!(!too_many_failed(10, 100));
        assert!(!too_many_failed(11, 1000));
        assert!(too_many_failed(11, 100));
        let repo = tempfile::tempdir().unwrap();
        let mut rels = Vec::new();
        for i in 0..30 {
            let rel = format!("src/acme-{i}.js");
            write(repo.path(), &rel, "var a = 1;");
            rels.push(rel);
        }
        for rel in &rels {
            std::fs::set_permissions(
                repo.path().join(rel),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
        }
        let readable = std::fs::read(repo.path().join(&rels[0])).is_ok();
        let run = tempfile::tempdir().unwrap();
        let built = Mirror::build_blocking(repo.path(), &rels, run.path(), "r", &plenty);
        for rel in &rels {
            std::fs::set_permissions(
                repo.path().join(rel),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        if readable {
            return;
        }
        let err = built.expect_err("a mirror of nothing must be refused");
        assert!(err.to_string().contains("could not be copied"), "{err}");
    }

    /// The code tools are given the admitted set plus the Cargo files a build needs, nothing else:
    /// no other lockfile, no vendored dependency file.
    #[test]
    fn code_tools_get_the_admitted_set_plus_cargo_files_only() {
        let repo = tempfile::tempdir().unwrap();
        for rel in [
            "src/acme-ok.rs",
            "Cargo.toml",
            "Cargo.lock",
            "package-lock.json",
            "Gemfile.lock",
            "vendor/acme-lib/package-lock.json",
        ] {
            write(repo.path(), rel, "x");
        }
        let ex = extracted(repo.path());
        let paths = code_tool_paths(&ex, repo.path(), &RepoScope::none());
        for want in ["src/acme-ok.rs", "Cargo.toml", "Cargo.lock"] {
            assert!(paths.iter().any(|p| p == want), "{want}: {paths:?}");
        }
        for not in [
            "package-lock.json",
            "Gemfile.lock",
            "vendor/acme-lib/package-lock.json",
        ] {
            assert!(!paths.iter().any(|p| p == not), "{not}: {paths:?}");
        }
    }

    /// A copy that fails AFTER writing some bytes (a full disk mid-file, a read error) leaves
    /// no file behind, and the file is neither scanned nor listed as checked. A mode 000 source
    /// fails before the destination exists and proves nothing, so the copier here writes first.
    #[cfg(unix)]
    #[test]
    fn a_copy_that_fails_midway_leaves_no_partial_file_and_is_not_listed_as_checked() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "src/acme-ok.js", "var ok = 1;");
        write(
            repo.path(),
            "package-lock.json",
            "{\"name\":\"a\",\"lockfileVersion\":3}",
        );
        let failing = |from: &Path, to: &Path| -> io::Result<u64> {
            if from.ends_with("package-lock.json") {
                std::fs::write(to, b"{\"name\":")?; // some bytes land first
                return Err(io::Error::other("read error midway"));
            }
            std::fs::copy(from, to)
        };
        let run = tempfile::tempdir().unwrap();
        let rels = vec![
            "src/acme-ok.js".to_string(),
            "package-lock.json".to_string(),
        ];
        let m = Mirror::build_with_copier(&failing, repo.path(), &rels, run.path(), "r", &plenty)
            .unwrap();
        assert_eq!(m.copy_failed, ["package-lock.json"]);
        assert!(m.path().join("src/acme-ok.js").is_file());
        assert!(
            !m.path().join("package-lock.json").exists(),
            "a half-copied file is left for a tool to scan"
        );
        // Not listed as checked either.
        let file = |p: &str| crate::scan_ledger::AuditedDependencyFile {
            repo: "acme/web".to_string(),
            path: p.to_string(),
            vendored: false,
        };
        let listed = crate::dep_audit::given_to_osv_for_test(
            vec![file("package-lock.json")],
            m.path(),
            &m.copy_failed,
        );
        assert!(listed.is_empty(), "{listed:?}");
    }

    #[test]
    fn nothing_copied_wrong_says_nothing() {
        assert!(not_copied_note("acme/web", MIRROR_NOTE_TOOL, &[]).is_none());
    }

    /// A run folder whose owning process is alive is not swept, whatever its age.
    #[cfg(unix)]
    #[test]
    fn the_sweep_keeps_a_folder_whose_process_is_alive() {
        let base = tempfile::tempdir().unwrap();
        let runs = base.path().join("scan-mirrors");
        let mine = format!("scan-{}-1-1", std::process::id());
        let dead = "scan-999999999-1-1";
        for name in [mine.as_str(), dead] {
            std::fs::create_dir_all(runs.join(name).join("mirror/r")).unwrap();
        }
        sweep_stale(&runs, Duration::ZERO);
        assert!(
            runs.join(&mine).join("mirror/r").exists(),
            "a live run was swept"
        );
        assert!(!runs.join(dead).exists(), "a dead run was kept");
    }

    #[test]
    fn a_tool_message_loses_the_mirror_and_build_folders_and_git_stops_at_the_mirror() {
        let m = Path::new("/d/camerata/scan-mirrors/scan-1-2-3/mirror/acme_web");
        let said =
            "error in /d/camerata/scan-mirrors/scan-1-2-3/mirror/acme_web/src/a.rs; built into \
                    /d/camerata/scan-mirrors/scan-1-2-3/mirror-target/acme_web/debug";
        let clean = scrub_mirror_path(said, m);
        assert!(!clean.contains("scan-mirrors"), "{clean}");
        assert_eq!(
            git_ceiling_for(m),
            Some(PathBuf::from("/d/camerata/scan-mirrors/scan-1-2-3/mirror"))
        );
        assert_eq!(git_ceiling_for(Path::new("/work/app")), None);
    }

    /// No test process builds a mirror under the real per-user state dir.
    #[test]
    fn a_test_process_never_gets_a_mirror_root_under_the_real_state_dir() {
        let real = crate::scan_cache::state_dir::dir_from(
            None,
            std::env::var_os("HOME").as_deref(),
            std::env::var_os("XDG_CACHE_HOME").as_deref(),
        );
        // The live resolution, in this very test process.
        assert!(
            !mirrors_root().starts_with(&real),
            "{:?} is under the real state dir {real:?}",
            mirrors_root()
        );
        // The pure rule: a test process on the real dir is redirected, an explicit seam is honoured,
        // and a non-test process uses the state dir.
        let redirected = mirrors_root_for(&real, &real, true);
        assert!(!redirected.starts_with(&real), "{redirected:?}");
        let seam = tempfile::tempdir().unwrap();
        assert_eq!(
            mirrors_root_for(seam.path(), &real, true),
            seam.path().join("scan-mirrors")
        );
        assert_eq!(
            mirrors_root_for(&real, &real, false),
            real.join("scan-mirrors")
        );
    }

    /// (b) A mirror of a repo with outside-pointing links holds no link, none of the outside
    /// content, no `.git`, and a process run in it sees none of that either.
    #[cfg(unix)]
    #[test]
    fn a_mirror_has_no_link_no_git_and_nothing_from_outside() {
        let (_outside, repo) = fixture();
        let mut rels = staged_for(repo.path());
        // Name the links explicitly too: the guard, not the list, must keep them out.
        rels.push("src/acme-link.js".into());
        rels.push("acme-linkdir/acme-inner.js".into());
        let run = tempfile::tempdir().unwrap();
        let m = Mirror::build_blocking(repo.path(), &rels, run.path(), "acme/web", &plenty)
            .expect("build");
        assert!(m.skipped >= 2, "both link paths refused: {}", m.skipped);
        let entries = walk(m.path());
        assert!(
            entries.iter().all(|(_, ft)| !ft.is_symlink()),
            "the mirror holds a symlink: {entries:?}"
        );
        assert!(m.path().join("src/acme-ok.js").is_file());
        assert!(!m.path().join("src/acme-link.js").exists());
        assert!(!m.path().join("acme-linkdir").exists());
        assert!(!m.path().join(".git").exists());
        // A tool process run in the mirror finds nothing that came from outside.
        let out = std::process::Command::new("grep")
            .args(["-r", SECRET, "."])
            .current_dir(m.path())
            .output()
            .unwrap();
        assert!(out.stdout.is_empty(), "outside content reachable: {out:?}");
        let found = std::process::Command::new("find")
            .args([".", "-type", "l"])
            .current_dir(m.path())
            .output()
            .unwrap();
        assert!(found.stdout.is_empty(), "{found:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_mirror_folder_is_private_and_file_content_and_times_are_kept() {
        use std::os::unix::fs::PermissionsExt;
        let (_outside, repo) = fixture();
        let src = repo.path().join("src/acme-ok.js");
        let when = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(when)
            .unwrap();
        let run = tempfile::tempdir().unwrap();
        let m = Mirror::build_blocking(
            repo.path(),
            &staged_for(repo.path()),
            run.path(),
            "r",
            &plenty,
        )
        .unwrap();
        let mode = std::fs::metadata(m.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let copy = m.path().join("src/acme-ok.js");
        assert_eq!(
            std::fs::read_to_string(&copy).unwrap(),
            "export const a = 1;\n"
        );
        assert_eq!(std::fs::metadata(&copy).unwrap().modified().unwrap(), when);
    }

    /// (c) A path a tool reports inside the mirror maps back to the repo-relative path.
    #[cfg(unix)]
    #[test]
    fn a_tool_path_inside_the_mirror_maps_to_the_real_relative_path() {
        let (_outside, repo) = fixture();
        let run = tempfile::tempdir().unwrap();
        let m = Mirror::build_blocking(
            repo.path(),
            &staged_for(repo.path()),
            run.path(),
            "r",
            &plenty,
        )
        .unwrap();
        let abs = std::fs::canonicalize(m.path().join("src/acme-ok.js")).unwrap();
        let abs = abs.to_string_lossy();
        assert!(crate::scan_tools::is_in_repo_scope(m.path(), &abs));
        assert_eq!(
            crate::scan_tools::repo_relative_path(m.path(), &abs),
            "src/acme-ok.js",
            "a finding must name the client's relative path"
        );
        assert_eq!(
            crate::scan_tools::repo_relative_path(m.path(), "src/acme-ok.js"),
            "src/acme-ok.js"
        );
        let said = format!("could not read {}/src/acme-ok.js", m.path().display());
        assert_eq!(
            scrub_mirror_path(&said, m.path()),
            "could not read src/acme-ok.js"
        );
    }

    /// (d) The mirror is removed after success, after a failure and after a stop.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_mirror_is_removed_after_success_failure_and_stop() {
        let (_outside, repo) = fixture();
        let rels = staged_for(repo.path());

        // Success: finish() removes it, and the empty run folder with it.
        let run = tempfile::tempdir().unwrap();
        let run_root = run.path().join("scan-a");
        let m = Mirror::build_blocking(repo.path(), &rels, &run_root, "r", &plenty).unwrap();
        let (mirror_dir, target_dir) = (m.path().to_path_buf(), m.target_dir().to_path_buf());
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("build.o"), "x").unwrap();
        m.finish().await;
        assert!(!mirror_dir.exists(), "mirror left after success");
        assert!(!target_dir.exists(), "cargo target left after success");
        assert!(!run_root.exists(), "empty run folder left after success");

        // Failure: a tool step that returns an error still drops the mirror.
        let run_root = run.path().join("scan-b");
        let failed: Result<(), String> = {
            let m = Mirror::build_blocking(repo.path(), &rels, &run_root, "r", &plenty).unwrap();
            assert!(m.path().exists());
            Err("tool failed".to_string())
        };
        assert!(failed.is_err());
        assert!(
            !run_root.join("mirror").exists(),
            "mirror left after failure"
        );

        // Stop: the run's task is cancelled while a tool is running.
        let run_root = run.path().join("scan-c");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let repo_path = repo.path().to_path_buf();
        let rr = run_root.clone();
        let task = tokio::spawn(async move {
            let m = Mirror::build_blocking(&repo_path, &rels, &rr, "r", &plenty).unwrap();
            started_tx.send(m.path().to_path_buf()).unwrap();
            tokio::time::sleep(Duration::from_secs(3600)).await;
            drop(m);
        });
        let dir = started_rx.await.unwrap();
        assert!(dir.exists());
        task.abort();
        let _ = task.await;
        assert!(!dir.exists(), "mirror left after the run was stopped");
    }

    /// (e) Removal outside the run's own folder is refused, and nothing is deleted.
    #[cfg(unix)]
    #[test]
    fn removal_outside_the_run_folder_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let run_root = base.path().join("runs/scan-x");
        std::fs::create_dir_all(run_root.join("mirror/r")).unwrap();
        let other = base.path().join("acme-client-clone");
        write(&other, "keep.txt", "keep");

        let refused = |p: &Path| {
            let e = remove_mirror_dir(&run_root, p).expect_err("must refuse");
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{p:?}");
        };
        refused(&other);
        refused(&run_root);
        refused(&run_root.join("mirror/../../acme-client-clone"));
        // A link inside the run folder that points at the client's folder.
        std::os::unix::fs::symlink(&other, run_root.join("mirror/evil")).unwrap();
        refused(&run_root.join("mirror/evil"));
        assert!(
            other.join("keep.txt").exists(),
            "the client's folder was touched"
        );

        // The run's own folder is removable.
        remove_mirror_dir(&run_root, &run_root.join("mirror/r")).expect("own folder is removed");
        assert!(!run_root.join("mirror/r").exists());
    }

    /// (f) Low disk is an error naming the shortfall, and nothing is left behind.
    #[cfg(unix)]
    #[test]
    fn low_disk_is_reported_and_leaves_nothing() {
        let (_outside, repo) = fixture();
        let run = tempfile::tempdir().unwrap();
        let run_root = run.path().join("scan-low");
        let err = Mirror::build_blocking(
            repo.path(),
            &staged_for(repo.path()),
            &run_root,
            "r",
            &|_| Some(1024),
        )
        .expect_err("must refuse");
        let MirrorError::LowDisk { needed, available } = err.clone() else {
            panic!("{err:?}");
        };
        assert_eq!(available, 1024);
        assert!(needed > DISK_MARGIN_BYTES);
        assert!(err.to_string().contains("not enough free disk space"));
        assert!(!run_root.exists(), "a refused build left its folder");
    }

    /// (f, disclosure) A repo whose mirror could not be made has every one of its tools named as
    /// not run, as a failed part.
    #[test]
    fn a_failed_mirror_is_disclosed_for_every_tool() {
        let why = MirrorError::LowDisk {
            needed: 3 << 30,
            available: 1 << 20,
        }
        .to_string();
        let notes = not_run_notes("acme/web", vec!["eslint".into(), "semgrep".into()], &why);
        assert_eq!(notes.len(), 2);
        for n in &notes {
            assert!(n.tool_failed);
            assert_eq!(n.repo.as_deref(), Some("acme/web"));
            assert!(
                n.message.contains("not enough free disk space"),
                "{}",
                n.message
            );
        }
        assert_eq!(notes[0].tool, "eslint");
    }

    #[test]
    fn cargo_target_dir_is_a_sibling_only_for_a_mirror_folder() {
        let m = Path::new("/d/camerata/scan-mirrors/scan-1-2-3/mirror/acme_web");
        assert_eq!(
            cargo_target_dir_for(m),
            Some(PathBuf::from(
                "/d/camerata/scan-mirrors/scan-1-2-3/mirror-target/acme_web"
            ))
        );
        // A client folder that merely has a parent named `mirror` is not redirected.
        assert_eq!(cargo_target_dir_for(Path::new("/work/mirror/app")), None);
        assert_eq!(cargo_target_dir_for(Path::new("/work/app")), None);
    }

    #[cfg(unix)]
    #[test]
    fn stale_run_folders_are_swept_and_fresh_ones_kept() {
        let base = tempfile::tempdir().unwrap();
        let runs = base.path().join("runs");
        for name in ["scan-old", "other-folder"] {
            std::fs::create_dir_all(runs.join(name).join("mirror/r")).unwrap();
        }
        sweep_stale(&runs, Duration::from_secs(3600));
        assert!(runs.join("scan-old/mirror/r").exists(), "fresh run swept");
        sweep_stale(&runs, Duration::ZERO);
        assert!(!runs.join("scan-old").exists(), "stale run kept");
        assert!(
            runs.join("other-folder/mirror/r").exists(),
            "a foreign folder was touched"
        );
    }

    #[test]
    fn labels_cannot_escape_the_run_folder() {
        assert_eq!(sanitize_label("acme/web"), "acme_web");
        assert_eq!(sanitize_label(""), "repo");
        let odd = sanitize_label("../../x");
        assert!(!odd.contains('/') && !odd.contains(".."), "{odd}");
    }
}
