//! Local repo checkouts: the foundation for "run it locally before you push".
//!
//! Repo CONTENTS live on disk here (cloned), not in the project store — the project
//! store keeps only the configs + pointers. Each project's repos are cloned at
//! `<workspace_root>/<owner>/<repo>`. The governed fleet edits files in these working
//! copies on a branch; the developer runs/tests them locally; then an explicit
//! `ship` step pushes the branch and opens a PR. Nothing auto-merges.
//!
//! Git is driven by shelling out to the system `git` (the desktop dev tool assumes
//! git is installed — it gets the user's credentials/SSH config for free). The token
//! is injected ONLY into the transient network commands (clone / fetch / push) via an
//! `x-access-token` URL, and the persisted `origin` is rewritten to the tokenless URL
//! so the secret never lands in `.git/config` on disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// The state of one repo's local working copy, as the cockpit renders it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoCheckout {
    /// `owner/repo`.
    pub repo: String,
    /// Whether a local clone exists.
    pub cloned: bool,
    /// Absolute path to the working copy.
    pub path: String,
    /// Current checked-out branch (when cloned).
    pub branch: Option<String>,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
    /// Human-readable status / last error.
    pub detail: String,
}

impl RepoCheckout {
    fn not_cloned(root: &Path, repo: &str) -> Self {
        Self {
            repo: repo.to_string(),
            cloned: false,
            path: repo_dir(root, repo).to_string_lossy().into_owned(),
            branch: None,
            dirty: false,
            detail: "not cloned yet".to_string(),
        }
    }

    fn error(root: &Path, repo: &str, detail: String) -> Self {
        Self {
            repo: repo.to_string(),
            cloned: is_git_repo(&repo_dir(root, repo)),
            path: repo_dir(root, repo).to_string_lossy().into_owned(),
            branch: None,
            dirty: false,
            detail,
        }
    }
}

/// Local path for a repo: `<root>/<owner>/<repo>`.
pub fn repo_dir(root: &Path, repo: &str) -> PathBuf {
    root.join(repo)
}

/// True if `dir` holds a git working copy.
fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// The tokenless HTTPS remote (what we persist on disk).
fn clean_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

/// The authenticated HTTPS remote — used ONLY for transient network commands, never
/// written into `.git/config`.
fn authed_url(repo: &str, token: &str) -> String {
    format!("https://x-access-token:{token}@github.com/{repo}.git")
}

/// Run a git command (optionally in `cwd`) and capture its output.
async fn git(cwd: Option<&Path>, args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new("git");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.args(args);
    // ARCH-RESOURCE-LIFECYCLE-1: reap git if our future is dropped.
    cmd.kill_on_drop(true);
    cmd.output().await
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

/// Read the live status (branch + dirty) of a working copy — no network.
pub async fn checkout_status(root: &Path, repo: &str) -> RepoCheckout {
    let dir = repo_dir(root, repo);
    if !is_git_repo(&dir) {
        return RepoCheckout::not_cloned(root, repo);
    }
    let branch = git(Some(&dir), &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let dirty = git(Some(&dir), &["status", "--porcelain"])
        .await
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let detail = match (&branch, dirty) {
        (Some(b), true) => format!("on {b} · uncommitted changes"),
        (Some(b), false) => format!("on {b} · clean"),
        (None, _) => "cloned".to_string(),
    };
    RepoCheckout {
        repo: repo.to_string(),
        cloned: true,
        path: dir.to_string_lossy().into_owned(),
        branch,
        dirty,
        detail,
    }
}

/// Override-aware checkout status (issue #33 / #38): resolve the repo's local folder via the
/// per-repo override first, falling back to `<workspace_root>/<owner>/<repo>`. A folder only counts
/// as `cloned` when it's a git checkout whose `origin` matches `owner/repo` — a git repo with the
/// WRONG origin is reported not-cloned with a clear reason. When it IS a matching checkout, the
/// live branch/dirty enrichment runs on the RESOLVED dir. When neither an override nor a workspace
/// root is available, the repo reports not-cloned with a helpful reason (no hard error).
///
/// This unifies the Workspace status path with the readiness-gate resolution primitives so a clone
/// living at a non-standard path (a flat `.../repo`, or the workspace folder itself) is recognized.
pub async fn checkout_status_resolved(
    override_path: Option<&str>,
    workspace_root: Option<&str>,
    repo: &str,
) -> RepoCheckout {
    let Some(dir) = resolve_repo_dir(override_path, workspace_root, repo) else {
        return RepoCheckout {
            repo: repo.to_string(),
            cloned: false,
            path: String::new(),
            branch: None,
            dirty: false,
            detail: "no local path set — link the repo's folder (or set a workspace root)"
                .to_string(),
        };
    };
    let path_str = dir.to_string_lossy().into_owned();

    if !is_git_repo(&dir) {
        return RepoCheckout {
            repo: repo.to_string(),
            cloned: false,
            path: path_str,
            branch: None,
            dirty: false,
            detail: "not cloned yet".to_string(),
        };
    }

    // A git folder with the WRONG origin must NOT count as cloned.
    match detect_remote_repo(&dir).await {
        Ok(found) if found.eq_ignore_ascii_case(repo.trim()) => {}
        Ok(found) => {
            return RepoCheckout {
                repo: repo.to_string(),
                cloned: false,
                path: path_str,
                branch: None,
                dirty: false,
                detail: format!("that folder is a different repo (origin: {found})"),
            };
        }
        Err(e) => {
            return RepoCheckout {
                repo: repo.to_string(),
                cloned: false,
                path: path_str,
                branch: None,
                dirty: false,
                detail: e,
            };
        }
    }

    // Matching git checkout — enrich with the live branch + dirty state on the RESOLVED dir.
    let branch = git(Some(&dir), &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let dirty = git(Some(&dir), &["status", "--porcelain"])
        .await
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let detail = match (&branch, dirty) {
        (Some(b), true) => format!("on {b} · uncommitted changes"),
        (Some(b), false) => format!("on {b} · clean"),
        (None, _) => "cloned".to_string(),
    };
    RepoCheckout {
        repo: repo.to_string(),
        cloned: true,
        path: path_str,
        branch,
        dirty,
        detail,
    }
}

/// Clone the repo into the workspace, or fast-forward an existing clone. Returns the
/// resulting status. The token is used only for this network step and is scrubbed
/// from the persisted remote.
pub async fn clone_or_pull(root: &Path, repo: &str, token: &str) -> RepoCheckout {
    // Belt-and-suspenders: a `repo` containing a NUL byte is never a real `owner/repo` — it
    // is the UI-internal single-repo sentinel (`"\u{0}__single_repo__"`) that leaked into an
    // emit path. Fail loud + safe here rather than shelling out to `git clone` and getting the
    // opaque "nul byte found in provided data" panic-message. The server-side `normalize_repos`
    // should have stripped it upstream; this is the last line of defence.
    if repo.contains('\0') {
        return RepoCheckout::error(
            root,
            repo,
            "invalid repo identifier (internal sentinel leaked) — reselect the rule".to_string(),
        );
    }
    let dir = repo_dir(root, repo);
    let authed = authed_url(repo, token);

    if is_git_repo(&dir) {
        // Fetch from the authenticated URL and fast-forward only (never clobber local
        // work; a dirty/diverged tree just stays as-is and is reported).
        let _ = git(Some(&dir), &["fetch", "--prune", &authed]).await;
        let _ = git(Some(&dir), &["merge", "--ff-only", "FETCH_HEAD"]).await;
        return checkout_status(root, repo).await;
    }

    if let Some(parent) = dir.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return RepoCheckout::error(root, repo, format!("create workspace dir: {e}"));
        }
    }
    let out = git(None, &["clone", &authed, &dir.to_string_lossy()]).await;
    // Scrub the token from origin so it never persists on disk.
    let _ = git(
        Some(&dir),
        &["remote", "set-url", "origin", &clean_url(repo)],
    )
    .await;
    match out {
        Ok(o) if o.status.success() => checkout_status(root, repo).await,
        Ok(o) => RepoCheckout::error(root, repo, format!("git clone failed: {}", stderr_of(&o))),
        Err(e) => RepoCheckout::error(root, repo, format!("git clone failed: {e}")),
    }
}

/// Refresh an ALREADY-RESOLVED repo checkout (an override path or the standard workspace layout
/// that already passed origin validation) by fast-forward pulling its current branch from an
/// authenticated origin URL. Used by "Clone / update all repos" so a repo that's already
/// linked/cloned actually gets updated instead of just being re-reported as-is. Best-effort: a
/// diverged/detached/offline branch just falls through to reporting the live status below —
/// `pull_branch` is `--ff-only`, so this never clobbers local work.
pub async fn refresh_resolved(dir: &Path, repo: &str, token: &str) -> RepoCheckout {
    let path_str = dir.to_string_lossy().into_owned();
    let branch = git(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(b) = &branch {
        let _ = pull_branch(dir, repo, b, token).await;
    }
    let dirty = git(Some(dir), &["status", "--porcelain"])
        .await
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let detail = match (&branch, dirty) {
        (Some(b), true) => format!("on {b} · uncommitted changes"),
        (Some(b), false) => format!("on {b} · clean"),
        (None, _) => "cloned".to_string(),
    };
    RepoCheckout {
        repo: repo.to_string(),
        cloned: true,
        path: path_str,
        branch,
        dirty,
        detail,
    }
}

/// Read the GitHub `owner/repo` from a local git checkout's `origin` remote, so the UI
/// can let a developer NAVIGATE to a repo folder instead of typing `owner/repo`. Returns
/// a specific human error so the UI can tell the user exactly what went wrong.
pub async fn detect_remote_repo(path: &Path) -> Result<String, String> {
    let out = git(Some(path), &["config", "--get", "remote.origin.url"])
        .await
        .map_err(|e| format!("couldn't run `git` ({e}) — is git installed and on PATH?"))?;
    if !out.status.success() {
        let stderr = stderr_of(&out);
        return Err(if stderr.is_empty() {
            "that folder has no `origin` remote (is it cloned from GitHub?)".to_string()
        } else {
            stderr
        });
    }
    let url = String::from_utf8_lossy(&out.stdout);
    parse_owner_repo(&url)
        .ok_or_else(|| format!("the origin remote isn't a GitHub URL: {}", url.trim()))
}

/// Whether a git remote URL's `origin` refers to the same GitHub repo as `owner/repo`.
///
/// Normalizes BOTH forms before comparing: `https://github.com/owner/repo(.git)(/)` and
/// `git@github.com:owner/repo(.git)`. The compare is case-insensitive on the whole
/// `owner/repo` (GitHub owners/repos are case-insensitive) and ignores a trailing `.git` /
/// trailing slash (both handled by [`parse_owner_repo`]). Returns `false` when `remote_url`
/// isn't a parseable GitHub URL. Pure — the "link existing clone" endpoint uses this to VALIDATE
/// a chosen folder's `origin` matches the project's stored identity before recording the override.
pub fn origin_matches_repo(remote_url: &str, repo: &str) -> bool {
    match parse_owner_repo(remote_url) {
        Some(found) => found.eq_ignore_ascii_case(repo.trim()),
        None => false,
    }
}

/// Validate that `path` is a local git clone whose `origin` remote matches the project's identity
/// for `repo` (`owner/repo`). Used by the "Select existing local clone" resolve path (ADR
/// `2026-07-01_project-readiness-gate`): the caller records the per-repo path override ONLY on
/// `Ok`. Returns a specific human error on a bad path / non-git folder / missing-or-mismatched
/// origin so the endpoint can surface exactly why the link was refused (nothing is recorded on
/// `Err`).
pub async fn validate_link_target(path: &Path, repo: &str) -> Result<(), String> {
    if !is_git_repo(path) {
        return Err(format!(
            "{} is not a git clone (no .git) — choose the folder that was cloned from GitHub",
            path.display()
        ));
    }
    // `detect_remote_repo` reads `origin` and returns the parsed `owner/repo`, or a human error.
    let found = detect_remote_repo(path).await?;
    if found.eq_ignore_ascii_case(repo.trim()) {
        Ok(())
    } else {
        Err(format!(
            "that folder's origin is {found}, not {repo} — pick the clone of {repo}"
        ))
    }
}

/// Parse `owner/repo` from a GitHub remote URL (https or ssh form), tolerant of a
/// trailing `.git` and extra path segments.
fn parse_owner_repo(url: &str) -> Option<String> {
    let s = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let i = s.find("github.com")?;
    let after = s[i + "github.com".len()..].trim_start_matches([':', '/']);
    let parts: Vec<&str> = after.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        Some(format!("{}/{}", parts[0], parts[1]))
    } else {
        None
    }
}

/// Resolve a repo's local folder (local-first, issue #33): a machine-local per-repo override
/// path wins; otherwise fall back to the `<workspace_root>/<owner>/<repo>` convention. None
/// when neither is available (no override and no workspace root) — that repo is unresolved.
pub fn resolve_repo_dir(
    override_path: Option<&str>,
    workspace_root: Option<&str>,
    repo: &str,
) -> Option<PathBuf> {
    if let Some(p) = override_path.filter(|p| !p.trim().is_empty()) {
        return Some(PathBuf::from(p));
    }
    workspace_root
        .filter(|r| !r.trim().is_empty())
        .map(|root| repo_dir(Path::new(root), repo))
}

/// One repo's local-path resolution status for the health check (issue #33).
#[derive(serde::Serialize)]
pub struct RepoResolution {
    pub repo: String,
    /// The resolved local folder, if any was determined.
    pub path: Option<String>,
    /// True only when `path` is a git checkout whose `origin` matches `repo`.
    pub resolved: bool,
    /// Human reason when not resolved (no path / not a checkout / wrong origin).
    pub reason: String,
}

/// Resolve + validate one repo against the local filesystem: is there a folder, is it a git
/// checkout, and does its `origin` remote match `owner/repo`? Pure of side effects.
pub async fn repo_resolution(
    override_path: Option<&str>,
    workspace_root: Option<&str>,
    repo: &str,
) -> RepoResolution {
    let Some(dir) = resolve_repo_dir(override_path, workspace_root, repo) else {
        return RepoResolution {
            repo: repo.to_string(),
            path: None,
            resolved: false,
            reason: "no local path set — choose the repo's folder (or set a workspace root)"
                .to_string(),
        };
    };
    let path_str = dir.to_string_lossy().into_owned();
    // `detect_remote_repo` returns the already-parsed `owner/repo` string (from the origin
    // URL). Compare case-insensitively so a clone whose origin casing differs from the stored
    // project identity (e.g. stored `Owner/Repo`, origin `owner/repo`) still resolves as
    // expected — matching the same invariant enforced by `validate_link_target`.
    match detect_remote_repo(&dir).await {
        Ok(found) if found.eq_ignore_ascii_case(repo.trim()) => RepoResolution {
            repo: repo.to_string(),
            path: Some(path_str),
            resolved: true,
            reason: String::new(),
        },
        Ok(found) => RepoResolution {
            repo: repo.to_string(),
            path: Some(path_str),
            resolved: false,
            reason: format!("that folder is a different repo (origin: {found})"),
        },
        Err(e) => RepoResolution {
            repo: repo.to_string(),
            path: Some(path_str),
            resolved: false,
            reason: e,
        },
    }
}

/// Create (or switch to) a working branch in the local clone. Used when the fleet
/// starts code work so edits land on a branch, not the default.
pub async fn create_branch(root: &Path, repo: &str, branch: &str) -> anyhow::Result<()> {
    let dir = repo_dir(root, repo);
    if !is_git_repo(&dir) {
        anyhow::bail!("{repo} is not cloned yet");
    }
    // Create the branch; if it already exists, just switch to it.
    let created = git(Some(&dir), &["checkout", "-b", branch]).await?;
    if created.status.success() {
        return Ok(());
    }
    let switched = git(Some(&dir), &["checkout", branch]).await?;
    if switched.status.success() {
        return Ok(());
    }
    anyhow::bail!("git checkout {branch}: {}", stderr_of(&switched))
}

/// Apply governance files onto a branch in the repo's LOCAL clone AND push that branch to
/// origin — WITHOUT opening a PR. The architect can then edit the working copy freely before
/// opening the PR (a separate step). The branch therefore exists in BOTH places.
///
/// Steps: ensure the clone exists (clone if missing) → create/switch to `branch` →
/// write each `(path, content)` file → `git add -A` → commit (tolerating "nothing to
/// commit" on a re-apply) → push the branch with the authenticated URL (token stays out of
/// config). Returns the local working-copy path.
pub async fn apply_local_and_push(
    dir: &Path,
    repo: &str,
    clone_root: Option<&Path>,
    branch: &str,
    files: &[(String, String)],
    commit_msg: &str,
    token: &str,
) -> anyhow::Result<String> {
    // Back-compat wrapper for callers (onboarding) that always want branch + commit + push.
    apply_local(
        dir, repo, clone_root, branch, files, commit_msg, token, true, true,
    )
    .await
}

/// Apply governance files into the repo's LOCAL clone, with optional escalation. This is the
/// staged primitive behind the Rules-page "Emit rules locally" cascade (each level requires
/// the previous):
/// - `do_branch == false`: just WRITE the files into the working copy on the current branch
///   (no branch, no commit). The lightest "emit locally" — drops the files in for the architect
///   to review and commit themselves.
/// - `do_branch == true`: create/switch to `branch`, write the files, then stage + commit ONLY
///   those files onto that branch.
/// - `do_push == true` (requires `do_branch`): force-push that branch to origin.
///
/// Opening a PR is a further step the caller performs via [`open_branch_pr`]. Returns the local
/// working-copy path.
#[allow(clippy::too_many_arguments)]
pub async fn apply_local(
    dir: &Path,
    repo: &str,
    clone_root: Option<&Path>,
    branch: &str,
    files: &[(String, String)],
    commit_msg: &str,
    token: &str,
    do_branch: bool,
    do_push: bool,
) -> anyhow::Result<String> {
    // Belt-and-suspenders: a `repo` containing a NUL byte is never a real `owner/repo` — it is
    // the UI-internal single-repo sentinel (`"\u{0}__single_repo__"`) that leaked into the emit
    // path. Fail loud + safe here (a clear error) instead of shelling out to git and getting the
    // opaque "nul byte found in provided data" panic-message. `normalize_repos` strips it upstream.
    if repo.contains('\0') {
        anyhow::bail!("invalid repo identifier (internal sentinel leaked) — reselect the rule");
    }
    // The repo must be local. With a workspace root (`clone_root`), clone it into that root if
    // it isn't on disk yet. With a per-repo path override (`clone_root = None`), the folder must
    // already be a local clone — we never clone over an explicitly-chosen path.
    if !is_git_repo(dir) {
        match clone_root {
            Some(root) => {
                let res = clone_or_pull(root, repo, token).await;
                if !res.cloned {
                    anyhow::bail!("{repo}: {} (couldn't get a local clone to apply into)", res.detail);
                }
            }
            None => anyhow::bail!(
                "{repo}: {} isn't a local git clone — choose the repo's folder (repo health) or set a workspace folder.",
                dir.display()
            ),
        }
    }
    // Only switch to the managed branch when the caller wants the emit committed there. Without
    // it, the files land on whatever branch is currently checked out, uncommitted.
    if do_branch {
        create_branch_at(dir, branch).await?;
    }
    // Write the governance files into the working copy, creating parent dirs as needed.
    for (rel, content) in files {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
        }
        tokio::fs::write(&full, content)
            .await
            .map_err(|e| anyhow::anyhow!("write {}: {e}", full.display()))?;
    }
    if do_branch {
        // Stage + commit ONLY the governance files we just wrote. NEVER `git add -A` here: that
        // would sweep the architect's unrelated in-flight work (untracked or modified files already
        // in the clone) onto this Camerata-MANAGED branch and force-push it. We stage the exact
        // files by pathspec AND restrict the commit to the same pathspecs, so even pre-staged
        // unrelated changes are excluded. A no-op re-apply (identical files) leaves nothing to commit.
        let rels: Vec<&str> = files.iter().map(|(rel, _)| rel.as_str()).collect();
        let mut add_args: Vec<&str> = vec!["add", "--"];
        add_args.extend_from_slice(&rels);
        let add = git(Some(dir), &add_args).await?;
        if !add.status.success() {
            anyhow::bail!("git add: {}", stderr_of(&add));
        }
        let mut commit_args: Vec<&str> = vec!["commit", "-m", commit_msg, "--"];
        commit_args.extend_from_slice(&rels);
        let commit = git(Some(dir), &commit_args).await?;
        if !commit.status.success() {
            let err = stderr_of(&commit);
            let out = String::from_utf8_lossy(&commit.stdout);
            let nothing = err.contains("nothing to commit")
                || out.contains("nothing to commit")
                || err.contains("no changes added")
                || out.contains("no changes added");
            if !nothing {
                anyhow::bail!("git commit: {err}");
            }
        }
    }
    if do_push {
        // Push the branch to origin so it exists remotely too. FORCE the push: this is a
        // Camerata-MANAGED branch that Apply fully REGENERATES each run (the governance files are
        // rewritten from the current ruleset). Re-applying — especially after re-cloning/re-
        // onboarding a repo — creates a fresh local branch whose history doesn't descend from the
        // stale remote one left by a prior Apply, so an ordinary push is rejected non-fast-forward.
        // Force-pushing makes origin mirror the freshly regenerated branch, which is exactly the
        // intent. It only ever touches `camerata/onboard-governance`, never the repo's own branches.
        let push = git(
            Some(dir),
            &[
                "push",
                "--force",
                "--set-upstream",
                &authed_url(repo, token),
                branch,
            ],
        )
        .await?;
        if !push.status.success() {
            anyhow::bail!("git push {branch}: {}", stderr_of(&push));
        }
    }
    Ok(dir.to_string_lossy().into_owned())
}

// ── Per-UoW git worktrees (issue: per-UoW worktrees + PR lifecycle, Decision 1) ──
//
// Two Units of Work on the SAME repo can't both check out their branch in the one
// shared clone — git refuses ("branch is already checked out"). The fix is git
// worktrees: a repo's shared clone keeps the `.git` object store, and each UoW gets
// its OWN working tree off it, checked out on the UoW's branch. N branches checked
// out at once, one object store.
//
// Invariant: a branch can be checked out in only ONE worktree, so each UoW MUST have
// a distinct branch (already true — UoW branches are keyed by story). If two UoWs ever
// shared a branch they would, by definition, share a worktree — `ensure_uow_worktree`
// returns the existing worktree for a branch rather than erroring, so that degenerate
// case degrades to "they collaborate in one tree" instead of failing.

/// The directory that holds a repo's per-UoW worktrees, nested under the shared clone:
/// `<clone>/.camerata-worktrees`. One subdir per UoW branch lives here.
fn worktrees_root(clone: &Path) -> PathBuf {
    clone.join(".camerata-worktrees")
}

/// Canonicalize `p` for a STABLE worktree identity, falling back to `p` unchanged if the
/// path can't be canonicalized (e.g. it doesn't exist yet). `git worktree list` reports
/// fully-resolved paths (e.g. macOS `/var` → `/private/var`); a freshly-constructed
/// `<clone>/.camerata-worktrees/<branch>` is NOT resolved. Canonicalizing both sides means
/// the path returned for a brand-new worktree string-matches the one `git worktree list`
/// reports on the next resolve — so reuse is idempotent and callers get one stable identity.
fn canonical_or_self(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// Sanitize a git branch name into a single safe directory segment. Branch names may
/// contain `/` (e.g. `camerata/story-7`) and other path-hostile characters; encode them
/// so the result is one flat, collision-resistant segment. `/` → `__`, and any char that
/// isn't alphanumeric / `.` / `-` / `_` → `-`. Distinct branches map to distinct dirs
/// for the inputs Camerata produces (branch names are slug-like).
pub fn sanitize_branch_segment(branch: &str) -> String {
    let mut out = String::with_capacity(branch.len());
    let mut chars = branch.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' => out.push_str("__"),
            c if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' => out.push(c),
            _ => out.push('-'),
        }
    }
    if out.is_empty() {
        out.push_str("uow");
    }
    out
}

/// Find the worktree path that currently has `branch` checked out, if any, by scanning
/// `git worktree list --porcelain`. Returns the absolute worktree path. This is how we
/// honor "a branch is already checked out elsewhere" gracefully: rather than letting
/// `git worktree add` error, we locate and reuse the existing worktree.
async fn worktree_for_branch(clone: &Path, branch: &str) -> Option<PathBuf> {
    let out = git(Some(clone), &["worktree", "list", "--porcelain"])
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Porcelain output is paragraphs separated by blank lines; each has a `worktree <path>`
    // line and (when on a branch) a `branch refs/heads/<name>` line.
    let want = format!("refs/heads/{branch}");
    let mut current_path: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(p.trim()));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b.trim() == want {
                return current_path.clone();
            }
        } else if line.trim().is_empty() {
            current_path = None;
        }
    }
    None
}

/// Ensure a per-UoW worktree for `branch` exists off the shared `clone`, checked out on
/// `branch`, and return its path. Idempotent + collision-safe:
///
/// - If `branch` is ALREADY checked out in some worktree (this one or any other), return
///   that existing path — never error-clobber. This covers both re-resolving the same UoW
///   (idempotent reuse) and the degenerate "two UoWs share a branch" case.
/// - Otherwise `git worktree add` a fresh worktree at `<clone>/.camerata-worktrees/<sani>`.
///   If the target dir already exists on disk but isn't a registered worktree (e.g. a stale
///   leftover after a prune), it is reused via `git worktree add` with the dir already there
///   only when it's a valid worktree; a stale non-worktree dir is removed first.
/// - The branch is created if it doesn't exist yet (`-b`), else checked out (`add <dir> <branch>`).
///
/// The `clone` must be an existing git checkout (the caller ensures the clone exists via the
/// normal clone path). Returns the worktree path on success.
pub async fn ensure_uow_worktree(clone: &Path, branch: &str) -> anyhow::Result<PathBuf> {
    if !is_git_repo(clone) {
        anyhow::bail!(
            "{}: not a git checkout — clone the repo before creating a UoW worktree",
            clone.display()
        );
    }

    // 0. Disk-headroom preflight guard (dogfood disk-safety, 2026-06-22): refuse to create
    //    another worktree when the disk is running low. Each worktree + shared target can consume
    //    significant space; this guard is the backstop that prevents filling the disk.
    ensure_disk_headroom(clone, disk_headroom_threshold_bytes())?;

    // 1. If the branch is already checked out anywhere, reuse that worktree (collision-safe,
    //    idempotent). This is the "already checked out elsewhere" graceful path.
    if let Some(existing) = worktree_for_branch(clone, branch).await {
        return Ok(canonical_or_self(existing));
    }

    let dir = worktrees_root(clone).join(sanitize_branch_segment(branch));

    // 2. If the target dir is itself an already-registered worktree (its branch may differ),
    //    just return it. (Branch match was handled above; this guards the path being live.)
    if is_git_repo(&dir) {
        return Ok(canonical_or_self(dir));
    }
    // A stale, non-worktree dir at the target path would make `git worktree add` fail; clear it.
    if dir.exists() {
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
    if let Some(parent) = dir.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| anyhow::anyhow!("create worktrees dir {}: {e}", parent.display()))?;
    }

    let dir_str = dir.to_string_lossy().into_owned();

    // 3. Does the branch exist already? If so, check it out into the new worktree; otherwise
    //    create it (`-b`). A worktree can't check out a branch that's live elsewhere, but we
    //    already returned that case above.
    let branch_exists = git(
        Some(clone),
        &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
    )
    .await
    .map(|o| o.status.success())
    .unwrap_or(false);

    let add = if branch_exists {
        git(Some(clone), &["worktree", "add", &dir_str, branch]).await?
    } else {
        git(Some(clone), &["worktree", "add", "-b", branch, &dir_str]).await?
    };

    if add.status.success() {
        return Ok(canonical_or_self(dir));
    }

    // Last-resort: a race may have created the worktree for this branch between our check and
    // the add (or the branch became live elsewhere). Re-scan and reuse if so, rather than error.
    if let Some(existing) = worktree_for_branch(clone, branch).await {
        return Ok(canonical_or_self(existing));
    }
    anyhow::bail!("git worktree add ({branch}): {}", stderr_of(&add))
}

/// Resolve the per-UoW working directory for `repo` on `branch`: the canonical place this
/// UoW's code lives in the repo. Resolves the shared clone via [`resolve_repo_dir`]
/// (override path wins, else `<workspace_root>/<owner>/<repo>`), then ensures + returns the
/// UoW's own worktree off it. `None` when the repo isn't resolved to a local clone (no
/// override and no workspace root) or the clone doesn't exist on disk yet — the caller
/// surfaces that exactly as it does for the shared clone.
///
/// This is the seam the dev run / update-branch / (Phase 2) ship+push run through, so two
/// same-repo UoWs operate in separate worktrees and never collide on a checkout.
pub async fn resolve_uow_worktree(
    override_path: Option<&str>,
    workspace_root: Option<&str>,
    repo: &str,
    branch: &str,
) -> Option<PathBuf> {
    let clone = resolve_repo_dir(override_path, workspace_root, repo)?;
    if !is_git_repo(&clone) {
        return None;
    }
    ensure_uow_worktree(&clone, branch).await.ok()
}

/// Remove a UoW's worktree (best-effort) — used when the UoW is signed off / torn down.
/// `git worktree remove --force` drops the working tree AND deregisters it (also handling a
/// dirty tree). Never fatal: a missing/already-removed worktree is fine. Also prunes stale
/// administrative entries afterward.
pub async fn remove_uow_worktree(clone: &Path, branch: &str) {
    if !is_git_repo(clone) {
        return;
    }
    // Prefer removing by the registered path (handles a branch checked out under a path that
    // doesn't match the sanitized name, e.g. a worktree created out-of-band).
    let path = worktree_for_branch(clone, branch)
        .await
        .unwrap_or_else(|| worktrees_root(clone).join(sanitize_branch_segment(branch)));
    let path_str = path.to_string_lossy().into_owned();
    let _ = git(Some(clone), &["worktree", "remove", "--force", &path_str]).await;
    // Belt-and-suspenders: if the dir lingers (e.g. it was never a registered worktree),
    // drop it from disk, then prune the admin records.
    if path.exists() {
        let _ = tokio::fs::remove_dir_all(&path).await;
    }
    prune_worktrees(clone).await;
}

/// `git worktree prune` on the shared clone: drop administrative records for worktrees whose
/// directories no longer exist (e.g. removed out-of-band, or a crashed run). Best-effort,
/// called on startup and after a remove. No-op when `clone` isn't a checkout.
pub async fn prune_worktrees(clone: &Path) {
    if !is_git_repo(clone) {
        return;
    }
    let _ = git(Some(clone), &["worktree", "prune"]).await;
}

// ── Shared Cargo target dir (dogfood disk-safety, 2026-06-22) ────────────────
//
// PROBLEM: Without a shared target, each UoW worktree's `cargo build / test / clippy`
// builds its OWN `target/` directory (~5 GB for this workspace). With several concurrent
// UoWs that multiplied to 115 GB across worktrees on the incident machine, filling the
// disk to 131 MB free and corrupting builds.
//
// SOLUTION: All of a repo's UoW worktrees share ONE `target/` directory: the
// `.camerata-shared-target/` sibling to `.camerata-worktrees/`, nested inside the shared
// clone. Per-repo (not global) because different repos cannot share a cargo target.
//
// CONCURRENCY TRADEOFF: Cargo file-locks `target/` during a build, so concurrent builds
// on the same repo SERIALIZE at the lock rather than running in parallel. This is the
// accepted tradeoff: correctness (no interleaved artifacts) over parallelism. Camerata's
// serial-by-default UoW execution means this rarely matters in practice; even when it
// does, waiting is far better than filling the disk.
//
// LOCATION: `<clone>/.camerata-shared-target` is outside every worktree root, so it
// never appears in any worktree's `git status`. It lives inside the clone directory
// (which Camerata manages), so it is cleaned up with the clone and never in the user's
// own repo. Do NOT add it to the user's `.gitignore` — it lives outside all worktree
// roots and is invisible to git.

/// Path of the shared Cargo target directory for a repo's clone.
///
/// All of a clone's UoW worktrees set `CARGO_TARGET_DIR` to this path so every
/// `cargo fmt / clippy / test` invocation writes into one shared artifact store
/// rather than a separate `target/` per worktree.
///
/// The directory is `<clone>/.camerata-shared-target` — a SIBLING of `.camerata-worktrees/`
/// inside the shared clone but OUTSIDE every individual worktree, so it is invisible to
/// `git status` in any worktree and never pollutes the user's repo.
pub fn shared_target_dir(clone: &Path) -> PathBuf {
    clone.join(".camerata-shared-target")
}

/// Ensure the shared Cargo target directory exists for `clone`, creating it if needed.
/// Best-effort: if creation fails the caller continues without the shared target (cargo
/// falls back to the default `<worktree>/target/` in that case).
pub async fn ensure_shared_target_dir(clone: &Path) -> PathBuf {
    let dir = shared_target_dir(clone);
    let _ = tokio::fs::create_dir_all(&dir).await;
    dir
}

// ── Disk-headroom preflight guard (dogfood disk-safety, 2026-06-22) ──────────
//
// A hard disk preflight guard that fires BEFORE creating a new worktree AND before
// starting a cargo build. This is the absolute backstop: even if the shared-target
// optimization fails (e.g. CARGO_TARGET_DIR not threaded correctly), the guard catches
// the disk running low before we make it worse.
//
// Threshold: 10 GB by default. Override with `CAMERATA_MIN_DISK_HEADROOM_GB` (integer).
// On failure the error message names the free / required amounts and suggests remediation
// (remove stale worktrees / shared target) so it surfaces actionably in the UI.

/// Minimum free disk bytes required before a worktree or build is allowed to proceed.
/// Default: 10 GiB. Override at runtime via `CAMERATA_MIN_DISK_HEADROOM_GB`.
pub const MIN_DISK_HEADROOM_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Parse a GB value string into bytes, falling back to `default_bytes` on invalid input.
/// Exported for unit-tests — tests drive this pure function directly rather than
/// manipulating `CAMERATA_MIN_DISK_HEADROOM_GB` in a shared test process environment
/// (where parallel tests would race on the same env var).
pub fn parse_disk_headroom_gb(raw: Option<&str>, default_bytes: u64) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(|gb| gb * 1024 * 1024 * 1024)
        .unwrap_or(default_bytes)
}

/// Read the effective disk-headroom threshold in bytes: `CAMERATA_MIN_DISK_HEADROOM_GB`
/// env var (integer GiB) if set and valid, otherwise [`MIN_DISK_HEADROOM_BYTES`] (10 GiB).
pub fn disk_headroom_threshold_bytes() -> u64 {
    let raw = std::env::var("CAMERATA_MIN_DISK_HEADROOM_GB").ok();
    parse_disk_headroom_gb(raw.as_deref(), MIN_DISK_HEADROOM_BYTES)
}

/// Pure headroom test — separated from the real `fs2` call so the decision logic
/// can be unit-tested without disk access.
///
/// Returns `true` when `available >= min` (headroom is sufficient).
pub fn has_headroom(available: u64, min: u64) -> bool {
    available >= min
}

/// Query the available disk space at `path` using a single `statvfs` syscall.
///
/// Returns `None` when the path does not exist or the OS call fails (e.g. on
/// an unsupported platform). The caller should fail-open on `None` (let the
/// operation proceed) to avoid spurious blocks on platforms where the query
/// is unavailable.
pub fn available_disk_bytes(path: &Path) -> Option<u64> {
    // fs2::available_space is a single statvfs call — negligible overhead.
    fs2::available_space(path).ok()
}

/// Assert there is at least `min_bytes` of free disk space at `path`.
///
/// Returns `Ok(())` when headroom is sufficient (or the query cannot be made —
/// fail-open for cross-platform safety). Returns a descriptive [`anyhow::Error`]
/// when free space is confirmed below the threshold so the error surfaces in the
/// run status / UI rather than silently filling the disk.
///
/// Call this at the start of [`ensure_uow_worktree`] and before cargo build steps
/// in the check runner. The check is cheap (one `statvfs` syscall).
pub fn ensure_disk_headroom(path: &Path, min_bytes: u64) -> anyhow::Result<()> {
    let Some(available) = available_disk_bytes(path) else {
        // Cannot query — fail-open: better to attempt the operation than to
        // block it spuriously on a platform where statvfs is unavailable.
        return Ok(());
    };
    if has_headroom(available, min_bytes) {
        return Ok(());
    }
    let available_gb = available as f64 / (1024.0 * 1024.0 * 1024.0);
    let required_gb = min_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    anyhow::bail!(
        "insufficient disk headroom: {available_gb:.1} GB free, need >= {required_gb:.0} GB; \
         reclaim space (remove stale worktrees under .camerata-worktrees/ or \
         .camerata-shared-target/) before starting more work"
    )
}

// ── Local git controls (issue #37) ───────────────────────────────────────────

/// How far HEAD is ahead of and behind its upstream tracking branch.
///
/// Both counts are `None` when the branch has no upstream tracking ref (e.g. a
/// freshly-created local branch that has never been pushed). Both are `Some(0)`
/// when the branch is exactly in sync with its upstream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AheadBehind {
    /// Commits on HEAD not yet on the upstream.
    pub ahead: Option<u32>,
    /// Commits on the upstream not yet on HEAD.
    pub behind: Option<u32>,
}

/// Full inspection status for a repo's current HEAD — branch, dirty flag,
/// ahead/behind counts, and a human-readable one-liner. Returned by
/// [`git_status`] and surfaced in the cockpit's status bar for the repo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoGitStatus {
    /// Current HEAD branch name (or `"HEAD"` when detached).
    pub branch: String,
    /// True when the working tree or index has uncommitted changes.
    pub dirty: bool,
    /// Counts relative to the upstream tracking branch.
    pub sync: AheadBehind,
    /// A concise human-readable summary of the above fields, suitable for a
    /// single status-bar line.
    pub detail: String,
}

/// Parse the raw output of `git rev-list --left-right --count HEAD...@{u}` into
/// an `AheadBehind`. The format is two tab-separated integers: `<ahead>\t<behind>`.
///
/// Returns `AheadBehind { ahead: None, behind: None }` for any malformed or
/// empty input (no upstream set, detached HEAD, new repo, etc.).
/// Exported for unit tests.
pub fn parse_ahead_behind(raw: &str) -> AheadBehind {
    let raw = raw.trim();
    if raw.is_empty() {
        return AheadBehind::default();
    }
    let parts: Vec<&str> = raw.split('\t').collect();
    if parts.len() != 2 {
        return AheadBehind::default();
    }
    match (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
        (Ok(a), Ok(b)) => AheadBehind {
            ahead: Some(a),
            behind: Some(b),
        },
        _ => AheadBehind::default(),
    }
}

/// Inspect the current HEAD of the working copy at `dir`: branch, dirty flag,
/// and how many commits ahead/behind the tracking branch. Pure-ish — no network.
///
/// The ahead/behind query (`git rev-list --left-right --count HEAD...@{u}`)
/// only reads what was fetched last time; call `pull_branch` / `clone_or_pull`
/// to refresh the remote view first.
pub async fn git_status(dir: &Path) -> anyhow::Result<RepoGitStatus> {
    // 1. Current branch name.
    let branch_out = git(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let branch = if branch_out.status.success() {
        String::from_utf8_lossy(&branch_out.stdout)
            .trim()
            .to_string()
    } else {
        anyhow::bail!("git rev-parse: {}", stderr_of(&branch_out));
    };

    // 2. Dirty flag: any output from `git status --porcelain` means uncommitted changes.
    let dirty = git(Some(dir), &["status", "--porcelain"])
        .await
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    // 3. Ahead / behind the tracking branch (best-effort: no upstream → None counts).
    let sync = {
        let ab = git(
            Some(dir),
            &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
        )
        .await;
        match ab {
            Ok(o) if o.status.success() => {
                parse_ahead_behind(&String::from_utf8_lossy(&o.stdout))
            }
            // No upstream set, detached HEAD, or empty repo — not an error.
            _ => AheadBehind::default(),
        }
    };

    // 4. Build the human-readable one-liner.
    let detail = build_status_detail(&branch, dirty, &sync);

    Ok(RepoGitStatus {
        branch,
        dirty,
        sync,
        detail,
    })
}

/// Build a concise status-bar string from the inspection result components.
/// Kept as a pure function so the UI can format the same data without calling git.
pub fn build_status_detail(branch: &str, dirty: bool, sync: &AheadBehind) -> String {
    let mut parts: Vec<String> = vec![format!("on {branch}")];
    match (sync.ahead, sync.behind) {
        (Some(a), Some(b)) => {
            if a > 0 && b > 0 {
                parts.push(format!("{a} ahead, {b} behind"));
            } else if a > 0 {
                parts.push(format!("{a} ahead"));
            } else if b > 0 {
                parts.push(format!("{b} behind"));
            } else {
                parts.push("in sync".to_string());
            }
        }
        _ => {} // no upstream — say nothing about sync
    }
    if dirty {
        parts.push("uncommitted changes".to_string());
    }
    parts.join(" · ")
}

/// A single commit in the log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub date: String,
}

/// Branch list for a local checkout: the current HEAD branch plus all local branches.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BranchList {
    pub current: String,
    pub branches: Vec<String>,
}

/// Return the current branch and all local branch names in the working copy at `dir`.
pub async fn list_branches(dir: &Path) -> anyhow::Result<BranchList> {
    let current_out = git(Some(&dir), &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let current = if current_out.status.success() {
        String::from_utf8_lossy(&current_out.stdout)
            .trim()
            .to_string()
    } else {
        anyhow::bail!("git rev-parse: {}", stderr_of(&current_out));
    };

    let branch_out = git(Some(dir), &["branch", "--format=%(refname:short)"]).await?;
    if !branch_out.status.success() {
        anyhow::bail!("git branch: {}", stderr_of(&branch_out));
    }
    let branches = String::from_utf8_lossy(&branch_out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(BranchList { current, branches })
}

/// Return the `limit` most-recent commits in the working copy at `dir`.
pub async fn git_log(dir: &Path, limit: usize) -> anyhow::Result<Vec<Commit>> {
    let limit_str = limit.to_string();
    let format_arg = "--pretty=format:%H\x1f%h\x1f%s\x1f%an\x1f%ad";
    let out = git(
        Some(dir),
        &["log", "-n", &limit_str, format_arg, "--date=short"],
    )
    .await?;

    // git won't error on an empty repo history — just yields no output.
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(5, '\x1f').collect();
        if parts.len() == 5 {
            commits.push(Commit {
                sha: parts[0].to_string(),
                short: parts[1].to_string(),
                subject: parts[2].to_string(),
                author: parts[3].to_string(),
                date: parts[4].to_string(),
            });
        }
    }
    Ok(commits)
}

/// Parse `git log` `--pretty=format:%H\x1f%h\x1f%s\x1f%an\x1f%ad` raw output into commits.
/// Exported so unit tests can drive the parser without a real git process.
pub fn parse_git_log(raw: &str) -> Vec<Commit> {
    raw.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(5, '\x1f').collect();
            if parts.len() == 5 {
                Some(Commit {
                    sha: parts[0].to_string(),
                    short: parts[1].to_string(),
                    subject: parts[2].to_string(),
                    author: parts[3].to_string(),
                    date: parts[4].to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Parse `git branch --format=%(refname:short)` raw output into branch names.
/// Exported so unit tests can drive the parser without a real git process.
pub fn parse_branch_list(current: &str, raw: &str) -> BranchList {
    let branches = raw
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    BranchList {
        current: current.trim().to_string(),
        branches,
    }
}

// ── Update-branch support (AI-assisted merge of a source branch into the UoW branch) ──

/// The set of branches a UoW can merge FROM, split by where they live. The
/// "Update branch" picker is populated from this: `local` are branches already in
/// the working copy, `origin` are remote-tracking branches (the `origin/` prefix
/// stripped). Both are empty for a repo that isn't cloned — a graceful, token-less
/// fallback so the UI can render an empty picker instead of erroring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MergeSourceBranches {
    /// Local branch names (`git branch --format=%(refname:short)`).
    pub local: Vec<String>,
    /// Remote-tracking branch names under `origin/`, with the `origin/` prefix
    /// stripped (so a UI picker shows `main`, not `origin/main`). `origin/HEAD`
    /// is filtered out — it is a symbolic ref, not a mergeable branch.
    pub origin: Vec<String>,
}

/// Parse `git branch -r` raw output into the origin-tracking branch names, stripping
/// the `origin/` prefix and dropping the `origin/HEAD -> …` symbolic ref. Only
/// `origin/*` refs are kept (other remotes are ignored — Camerata's clones use a
/// single `origin`). Exported so tests can drive the parser without a real git process.
pub fn parse_origin_branches(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        // Drop the symbolic `origin/HEAD -> origin/main` line.
        .filter(|l| !l.contains("->"))
        .filter_map(|l| l.strip_prefix("origin/"))
        .map(|l| l.to_string())
        .collect()
}

/// List the branches a UoW can merge from in the working copy at `dir`: local
/// branches and `origin/*` remote-tracking branches (prefix stripped). Reads only
/// what is already in the clone (no network) — `clone_or_pull` refreshes the remote
/// view. Returns empty lists when `dir` is not a git checkout (graceful, token-less).
pub async fn list_merge_sources(dir: &Path) -> MergeSourceBranches {
    if !is_git_repo(dir) {
        return MergeSourceBranches::default();
    }
    let local = git(Some(dir), &["branch", "--format=%(refname:short)"])
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let origin = git(Some(dir), &["branch", "-r"])
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_origin_branches(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    MergeSourceBranches { local, origin }
}

/// The outcome of attempting a `git merge <source>` into the current branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The merge completed cleanly (fast-forward or an auto-created merge commit). No
    /// agent is needed. Carries git's stdout summary.
    Clean(String),
    /// The merge left conflicts in the working tree. The repo is mid-merge; a gated
    /// agent must resolve the markers and the server then completes the commit. Carries
    /// the list of conflicted paths (from `git diff --name-only --diff-filter=U`).
    Conflicts(Vec<String>),
}

/// Whether the working tree at `dir` is mid-merge (a `MERGE_HEAD` exists). Used to
/// decide between completing the merge commit and reporting a clean fast-forward.
pub async fn is_merge_in_progress(dir: &Path) -> bool {
    git(Some(dir), &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The paths with unresolved merge conflicts in the working tree at `dir`.
pub async fn conflicted_paths(dir: &Path) -> Vec<String> {
    git(Some(dir), &["diff", "--name-only", "--diff-filter=U"])
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Run `git merge <source>` in the working copy at `dir` (already on the target branch).
///
/// `--no-edit` is passed so a clean merge auto-commits with the default message instead
/// of opening an editor (which would hang headless). A clean result returns
/// [`MergeOutcome::Clean`]; a conflict returns [`MergeOutcome::Conflicts`] with the
/// conflicted paths (the repo is left mid-merge for the agent to resolve). A merge that
/// fails for a NON-conflict reason (e.g. unknown ref) is an `Err`, NOT a false conflict.
pub async fn merge_source(dir: &Path, source: &str) -> anyhow::Result<MergeOutcome> {
    let out = git(Some(dir), &["merge", "--no-edit", source]).await?;
    if out.status.success() {
        return Ok(MergeOutcome::Clean(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ));
    }
    // A merge can fail for two distinct reasons: real conflicts (recoverable — the agent
    // resolves them) or a hard error (unknown ref, not a repo, dirty tree). Distinguish
    // them by whether the tree is now mid-merge with conflicted paths.
    let conflicts = conflicted_paths(dir).await;
    if !conflicts.is_empty() {
        return Ok(MergeOutcome::Conflicts(conflicts));
    }
    // No conflicted paths but the merge failed → a hard error. Surface git's stderr.
    anyhow::bail!("git merge {source}: {}", stderr_of(&out));
}

/// Abort an in-progress merge, restoring the working tree to the pre-merge state.
/// Best-effort: used on the fail-closed path so a merge that can't be completed never
/// leaves a half-merged tree behind.
pub async fn merge_abort(dir: &Path) -> anyhow::Result<()> {
    let out = git(Some(dir), &["merge", "--abort"]).await?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("git merge --abort: {}", stderr_of(&out));
}

/// Complete an in-progress merge by committing the (now resolved + staged) tree.
/// `--no-edit` keeps git's default merge message. Returns the commit stdout. Errors if
/// there are still unresolved conflicts (git refuses to commit), which the caller treats
/// as a failed resolution.
pub async fn commit_merge(dir: &Path) -> anyhow::Result<String> {
    let out = git(Some(dir), &["commit", "--no-edit"]).await?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    anyhow::bail!("git commit (merge): {}", stderr_of(&out));
}

/// Complete an in-progress merge with an explicit `message` (instead of git's auto-generated
/// `Merge branch ...` subject). `git commit -m <message>` in the merging state records the
/// merge commit with the given message, letting Camerata author a process-rule-compliant merge
/// message rather than bypassing the gate.
pub async fn commit_merge_with_message(dir: &Path, message: &str) -> anyhow::Result<String> {
    let out = git(Some(dir), &["commit", "-m", message]).await?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    anyhow::bail!("git commit (merge): {}", stderr_of(&out));
}

/// Fetch a single `branch` from origin into the local `origin/<branch>` tracking ref,
/// using an authenticated transient URL (token never lands in `.git/config`). Used by
/// the update-branch flow when the merge source is an origin branch, so the local
/// `origin/<branch>` ref is current before the merge. No fast-forward of any local branch.
pub async fn fetch_branch(dir: &Path, repo: &str, branch: &str, token: &str) -> anyhow::Result<()> {
    let out = git(Some(dir), &["fetch", &authed_url(repo, token), branch]).await?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("git fetch {branch}: {}", stderr_of(&out));
}

/// Fetch EVERY branch from origin into its local `refs/remotes/origin/*` tracking ref, using an
/// authenticated transient URL (the token never lands in `.git/config`). Unlike [`fetch_branch`]
/// (a single named branch), this brings the full set of origin branches up to date locally in one
/// call, e.g. so the branch list / ahead-behind counts reflect branches other people pushed. Never
/// touches the working tree or any local branch — a pure network refresh.
pub async fn fetch_all(dir: &Path, repo: &str, token: &str) -> anyhow::Result<String> {
    let out = git(
        Some(dir),
        &[
            "fetch",
            &authed_url(repo, token),
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )
    .await?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !stdout.is_empty() {
            return Ok(stdout);
        }
        return Ok(stderr_of(&out));
    }
    anyhow::bail!("git fetch --all: {}", stderr_of(&out));
}

/// Stage all changes with `git add -A` then commit with `message`. Returns the
/// commit output (or a "nothing to commit" notice if the tree was already clean).
pub async fn commit_all(dir: &Path, message: &str) -> anyhow::Result<String> {
    let add = git(Some(&dir), &["add", "-A"]).await?;
    if !add.status.success() {
        anyhow::bail!("git add: {}", stderr_of(&add));
    }
    let commit = git(Some(&dir), &["commit", "-m", message]).await?;
    if commit.status.success() {
        let out = String::from_utf8_lossy(&commit.stdout).trim().to_string();
        return Ok(out);
    }
    let err = stderr_of(&commit);
    let stdout = String::from_utf8_lossy(&commit.stdout);
    if err.contains("nothing to commit") || stdout.contains("nothing to commit") {
        return Ok("nothing to commit — working tree clean".to_string());
    }
    anyhow::bail!("git commit: {err}");
}

/// Turn a bare directory (freshly scaffolded, no git history at all) into a git repo
/// with one initial commit on `main`. Used only by the create-app flow (Part 2 of the
/// scaffolder, `crate::create_app`) — every other function in this module operates on
/// an already-cloned repo.
///
/// Sets a LOCAL (repo-scoped, not global) commit identity before committing: a
/// freshly scaffolded app has no prior git identity to inherit, and the environment
/// running Camerata may have no global `user.name`/`user.email` configured at all (a
/// bare CI container, a fresh machine) — without this, the initial commit would fail
/// with git's "Please tell me who you are" error.
pub async fn init_repo_with_initial_commit(dir: &Path, message: &str) -> anyhow::Result<()> {
    let init = git(Some(dir), &["init", "-b", "main"]).await?;
    if !init.status.success() {
        anyhow::bail!("git init: {}", stderr_of(&init));
    }
    let email = git(
        Some(dir),
        &["config", "user.email", "camerata-scaffold@camerata.local"],
    )
    .await?;
    if !email.status.success() {
        anyhow::bail!("git config user.email: {}", stderr_of(&email));
    }
    let name = git(Some(dir), &["config", "user.name", "Camerata Scaffolder"]).await?;
    if !name.status.success() {
        anyhow::bail!("git config user.name: {}", stderr_of(&name));
    }
    commit_all(dir, message).await?;
    Ok(())
}

/// Point `dir`'s `origin` remote at `repo`'s TOKENLESS URL. Used right after
/// [`init_repo_with_initial_commit`], before the actual (authenticated, transient)
/// push — mirrors every other path in this module: the token only ever appears in a
/// throwaway push argument (see [`push_branch`]), never in a persisted remote.
pub async fn set_origin(dir: &Path, repo: &str) -> anyhow::Result<()> {
    let out = git(Some(dir), &["remote", "add", "origin", &clean_url(repo)]).await?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("git remote add origin: {}", stderr_of(&out));
}

/// Snapshot any uncommitted changes in `dir` with a `camerata: snapshot <task>` commit.
///
/// Idempotent: when the working tree is already clean (nothing to commit), this is a no-op
/// and returns `Ok(None)`. When there are staged or unstaged changes, they are committed and
/// the new commit SHA is returned as `Ok(Some(sha))`.
///
/// This is called at each natural per-task boundary (dev-run iteration completing, delegate
/// child completing, fan-out worker completing) so that uncommitted work is never left
/// unprotected between tasks — a later `git worktree remove` or branch clobber cannot lose it.
pub async fn snapshot_worktree(dir: &Path, task: &str) -> anyhow::Result<Option<String>> {
    let msg = format!("camerata: snapshot {task}");
    match commit_all(dir, &msg).await {
        Ok(out) if out.contains("nothing to commit") => Ok(None),
        Ok(out) => {
            // Extract the short SHA from the commit output (git prints e.g. "[branch abc1234] ...")
            let sha = out
                .split_whitespace()
                .find(|t| t.len() >= 6 && t.chars().all(|c| c.is_ascii_hexdigit()))
                .map(|t| t.to_string());
            Ok(sha)
        }
        Err(e) => Err(e),
    }
}

/// Push `branch` to origin using an authenticated transient URL (token never lands
/// in `.git/config`). This is the user-triggered push from the UI; the server never
/// calls this on its own.
pub async fn push_branch(dir: &Path, repo: &str, branch: &str, token: &str) -> anyhow::Result<()> {
    let out = git(Some(&dir), &["push", &authed_url(repo, token), branch]).await?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("git push {branch}: {}", stderr_of(&out));
}

/// Best-effort fast-forward pull of the repo's CURRENT branch from its configured origin. Used by
/// read-only refreshes (e.g. the suppressions registry) so the data reflects the latest remote
/// state, without touching the working copy beyond a clean fast-forward. Returns true on success;
/// false on any failure (diverged history, no upstream, offline, not a clone) — callers treat it
/// as best-effort and read whatever is on disk regardless.
pub async fn pull_local_ff(dir: &Path) -> bool {
    if !is_git_repo(dir) {
        return false;
    }
    match git(Some(dir), &["pull", "--ff-only"]).await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Fast-forward `branch` from origin using an authenticated transient URL.
pub async fn pull_branch(
    dir: &Path,
    repo: &str,
    branch: &str,
    token: &str,
) -> anyhow::Result<String> {
    let out = git(
        Some(&dir),
        &["pull", "--ff-only", &authed_url(repo, token), branch],
    )
    .await?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    anyhow::bail!("git pull {branch}: {}", stderr_of(&out));
}

/// Switch to an existing local branch (no creation). Use `create_branch_at` to create + switch.
pub async fn switch_branch(dir: &Path, branch: &str) -> anyhow::Result<()> {
    let out = git(Some(&dir), &["checkout", branch]).await?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("git checkout {branch}: {}", stderr_of(&out));
}

/// Create a new branch at `dir` and switch to it. If the branch already exists, just
/// switch to it. This variant takes the resolved `dir` directly (unlike `create_branch`
/// which accepts root + repo identifiers).
pub async fn create_branch_at(dir: &Path, branch: &str) -> anyhow::Result<()> {
    let created = git(Some(&dir), &["checkout", "-b", branch]).await?;
    if created.status.success() {
        return Ok(());
    }
    // Branch already exists — switch to it.
    let switched = git(Some(&dir), &["checkout", branch]).await?;
    if switched.status.success() {
        return Ok(());
    }
    anyhow::bail!("git checkout {branch}: {}", stderr_of(&switched))
}

/// Cherry-pick `sha` onto the current HEAD branch. On conflict, returns the stderr so
/// the UI can show it; the repo is left in conflict state (the user resolves or aborts).
pub async fn cherry_pick(dir: &Path, sha: &str) -> anyhow::Result<String> {
    let out = git(Some(&dir), &["cherry-pick", sha]).await?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    // Don't silently swallow the conflict — return it so the UI shows it.
    let detail = stderr_of(&out);
    anyhow::bail!("cherry-pick {sha}: {detail}");
}

/// Open a governance PR from an already-pushed branch (the explicit, separate step after
/// `apply_local_and_push`). Returns the PR URL; tolerant of a pre-existing open PR.
pub async fn open_branch_pr(
    repo: &str,
    branch: &str,
    title: &str,
    body: &str,
    token: &str,
) -> anyhow::Result<String> {
    open_pr(repo, branch, title, body, token).await
}

/// Push the local branch to GitHub (authenticated transient command), then open a PR
/// into the default branch. Returns the PR URL. This is the explicit ship step.
pub async fn ship(
    repo: &str,
    branch: &str,
    title: &str,
    body: &str,
    root: &Path,
    token: &str,
) -> anyhow::Result<String> {
    let dir = repo_dir(root, repo);
    if !is_git_repo(&dir) {
        anyhow::bail!("{repo} is not cloned yet");
    }
    // 1. Push the branch using the authenticated URL (token stays out of config).
    let push = git(Some(&dir), &["push", &authed_url(repo, token), branch]).await?;
    if !push.status.success() {
        anyhow::bail!("git push {branch}: {}", stderr_of(&push));
    }
    // 2. Open the PR via the GitHub API.
    open_pr(repo, branch, title, body, token).await
}

/// Open a PR for `head` into the repo's default branch (tolerant of a pre-existing
/// open PR, which it returns). Mirrors the transport pattern used by `arm`.
async fn open_pr(
    repo: &str,
    head: &str,
    title: &str,
    body: &str,
    token: &str,
) -> anyhow::Result<String> {
    open_pr_with_base(repo, head, None, title, body, token)
        .await
        .map(|p| p.url)
}

/// The result of opening (or discovering) a PR: its number AND its html_url. The PR
/// lifecycle stores the number on the UoW so it can be re-resolved later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedPr {
    /// The PR number (`#N`).
    pub number: u64,
    /// The PR's html_url.
    pub url: String,
}

/// Open a PR for `head` into a CHOSEN `base` branch (the console's target/base picker),
/// returning the PR number + url. `None` for `base` falls back to the repo's default
/// branch. Tolerant of a pre-existing PR for the head (state=all so a merged/closed one
/// is also discovered + returned). This is the number-carrying variant used by the PR
/// lifecycle; the plain [`open_pr`] keeps returning just the url for `ship`/governance.
pub async fn open_pr_with_base(
    repo: &str,
    head: &str,
    base: Option<&str>,
    title: &str,
    body: &str,
    token: &str,
) -> anyhow::Result<OpenedPr> {
    use camerata_worktracker::{HttpTransport, ReqwestTransport};

    let (owner, _name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repo must be owner/repo, got {repo}"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let api = "https://api.github.com";

    let base = match base.filter(|b| !b.trim().is_empty()) {
        Some(b) => b.to_string(),
        None => {
            let meta = transport.get(&format!("{api}/repos/{repo}")).await?;
            if !(200..300).contains(&meta.status) {
                anyhow::bail!("GET repo {repo}: HTTP {} {}", meta.status, meta.body);
            }
            serde_json::from_str::<serde_json::Value>(&meta.body)?["default_branch"]
                .as_str()
                .unwrap_or("main")
                .to_string()
        }
    };

    let pr_body = serde_json::json!({
        "title": title,
        "head": head,
        "base": base,
        "body": body,
    });
    let pr = transport
        .post(&format!("{api}/repos/{repo}/pulls"), &pr_body.to_string())
        .await?;
    if (200..300).contains(&pr.status) {
        let v: serde_json::Value = serde_json::from_str(&pr.body)?;
        return Ok(OpenedPr {
            number: v["number"].as_u64().unwrap_or_default(),
            url: v["html_url"].as_str().unwrap_or_default().to_string(),
        });
    }
    // A PR for this head may already exist — find and return it (state=all so a
    // merged/closed PR is also surfaced, not just an open one).
    if pr.status == 422 {
        let list = transport
            .get(&format!(
                "{api}/repos/{repo}/pulls?head={owner}:{head}&state=all"
            ))
            .await?;
        if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(&list.body) {
            if let Some(first) = arr.first() {
                return Ok(OpenedPr {
                    number: first["number"].as_u64().unwrap_or_default(),
                    url: first["html_url"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
    }
    anyhow::bail!("open PR: HTTP {} {}", pr.status, pr.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_dir_nests_owner_and_repo_under_root() {
        let root = Path::new("/Users/me/Camerata");
        let dir = repo_dir(root, "acme/api");
        assert!(dir.ends_with("acme/api"));
        assert!(dir.starts_with("/Users/me/Camerata"));
    }

    #[test]
    fn parses_owner_repo_from_remote_urls() {
        assert_eq!(
            parse_owner_repo("https://github.com/acme/api.git"),
            Some("acme/api".into())
        );
        assert_eq!(
            parse_owner_repo("https://github.com/acme/api"),
            Some("acme/api".into())
        );
        assert_eq!(
            parse_owner_repo("git@github.com:acme/api.git"),
            Some("acme/api".into())
        );
        assert_eq!(
            parse_owner_repo("git@github.com:acme/api.git\n"),
            Some("acme/api".into())
        );
        // Non-GitHub or malformed -> None.
        assert_eq!(parse_owner_repo("https://gitlab.com/acme/api.git"), None);
        assert_eq!(parse_owner_repo("not a url"), None);
    }

    #[test]
    fn url_helpers_inject_and_scrub_token() {
        assert_eq!(clean_url("acme/api"), "https://github.com/acme/api.git");
        let a = authed_url("acme/api", "ghp_secret");
        assert!(a.contains("x-access-token:ghp_secret@github.com/acme/api.git"));
        // The clean URL never carries the token.
        assert!(!clean_url("acme/api").contains("ghp_secret"));
    }

    #[tokio::test]
    async fn status_reports_not_cloned_for_empty_root() {
        let root = std::env::temp_dir().join(format!("camerata-ws-empty-{}", std::process::id()));
        let st = checkout_status(&root, "acme/api").await;
        assert!(!st.cloned);
        assert_eq!(st.detail, "not cloned yet");
    }

    // Full git round-trip against a local "remote" — exercises clone, branch, status,
    // and the token-scrubbed origin, with no network.
    // ── Tests for the new git-controls parsers (issue #37) ──────────────────

    #[test]
    fn parse_git_log_parses_valid_lines() {
        let raw = "abc123\x1fabc\x1ffix: the bug\x1fAlice\x1f2024-01-15\n\
                   def456\x1fdef\x1ffeat: new thing\x1fBob\x1f2024-01-14";
        let commits = parse_git_log(raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].short, "abc");
        assert_eq!(commits[0].subject, "fix: the bug");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].date, "2024-01-15");
        assert_eq!(commits[1].sha, "def456");
        assert_eq!(commits[1].subject, "feat: new thing");
    }

    #[test]
    fn parse_git_log_tolerates_empty_input() {
        let commits = parse_git_log("");
        assert!(commits.is_empty());
    }

    #[test]
    fn parse_git_log_skips_malformed_lines() {
        // A line with only 3 fields should be skipped; the valid one after it parses.
        let raw = "bad\x1fline\x1fonly-three\n\
                   abc\x1fABC\x1fgood: commit\x1fAuthor\x1f2024-02-01";
        let commits = parse_git_log(raw);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].short, "ABC");
    }

    #[test]
    fn parse_git_log_subject_may_contain_record_separator_via_splitn5() {
        // splitn(5, ..) ensures only the first 4 fields split; the subject can contain spaces.
        let raw = "sha1\x1fsh1\x1fsubject with spaces here\x1fAuthor Name\x1f2024-03-10";
        let commits = parse_git_log(raw);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "subject with spaces here");
        assert_eq!(commits[0].author, "Author Name");
    }

    #[test]
    fn parse_branch_list_builds_branch_list() {
        let raw = "main\nfeature/foo\ncamerata/work\n";
        let bl = parse_branch_list("feature/foo", raw);
        assert_eq!(bl.current, "feature/foo");
        assert_eq!(bl.branches, vec!["main", "feature/foo", "camerata/work"]);
    }

    #[test]
    fn parse_branch_list_trims_whitespace() {
        let raw = "  main  \n  other  ";
        let bl = parse_branch_list("  main  ", raw);
        assert_eq!(bl.current, "main");
        assert!(bl.branches.iter().all(|b| b == b.trim()));
    }

    #[tokio::test]
    async fn clone_branch_and_status_round_trip() {
        let base = std::env::temp_dir().join(format!("camerata-ws-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let origin = base.join("origin");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&origin).unwrap();

        // Build a tiny upstream repo with one commit on `main`.
        let g = |dir: &Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        g(&origin, &["init", "-q", "-b", "main"]);
        g(&origin, &["config", "user.email", "t@example.com"]);
        g(&origin, &["config", "user.name", "Test"]);
        std::fs::write(origin.join("README.md"), "hi").unwrap();
        g(&origin, &["add", "."]);
        g(&origin, &["commit", "-q", "-m", "init"]);

        // Clone it into the workspace via a file:// "authed" URL stand-in.
        let dir = repo_dir(&workspace, "local/demo");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        let out = git(
            None,
            &["clone", &origin.to_string_lossy(), &dir.to_string_lossy()],
        )
        .await
        .unwrap();
        assert!(out.status.success(), "clone: {}", stderr_of(&out));

        // Status: cloned, on a branch, clean.
        let st = checkout_status(&workspace, "local/demo").await;
        assert!(st.cloned);
        assert!(st.branch.is_some());
        assert!(!st.dirty);

        // Create a working branch and prove status reflects it.
        create_branch(&workspace, "local/demo", "camerata/work")
            .await
            .unwrap();
        let st2 = checkout_status(&workspace, "local/demo").await;
        assert_eq!(st2.branch.as_deref(), Some("camerata/work"));

        // A local edit makes it dirty.
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let st3 = checkout_status(&workspace, "local/demo").await;
        assert!(st3.dirty);

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── Tests for parse_ahead_behind + build_status_detail (issue #37 addendum) ─

    #[test]
    fn parse_ahead_behind_clean_sync() {
        // Both zero means in sync.
        let ab = parse_ahead_behind("0\t0");
        assert_eq!(ab.ahead, Some(0));
        assert_eq!(ab.behind, Some(0));
    }

    #[test]
    fn parse_ahead_behind_ahead_only() {
        let ab = parse_ahead_behind("3\t0");
        assert_eq!(ab.ahead, Some(3));
        assert_eq!(ab.behind, Some(0));
    }

    #[test]
    fn parse_ahead_behind_behind_only() {
        let ab = parse_ahead_behind("0\t5");
        assert_eq!(ab.ahead, Some(0));
        assert_eq!(ab.behind, Some(5));
    }

    #[test]
    fn parse_ahead_behind_both_diverged() {
        let ab = parse_ahead_behind("2\t4");
        assert_eq!(ab.ahead, Some(2));
        assert_eq!(ab.behind, Some(4));
    }

    #[test]
    fn parse_ahead_behind_empty_input_gives_none() {
        // No upstream tracking branch: git exits non-zero, we get empty string.
        let ab = parse_ahead_behind("");
        assert_eq!(ab.ahead, None);
        assert_eq!(ab.behind, None);
    }

    #[test]
    fn parse_ahead_behind_malformed_gives_none() {
        // Only one field (git ate the tab somehow) — no panic, just None.
        let ab = parse_ahead_behind("3");
        assert_eq!(ab.ahead, None);
        assert_eq!(ab.behind, None);
    }

    #[test]
    fn parse_ahead_behind_non_numeric_gives_none() {
        let ab = parse_ahead_behind("a\tb");
        assert_eq!(ab.ahead, None);
        assert_eq!(ab.behind, None);
    }

    #[test]
    fn parse_ahead_behind_strips_trailing_newline() {
        // git appends a newline; we should still parse correctly.
        let ab = parse_ahead_behind("1\t2\n");
        assert_eq!(ab.ahead, Some(1));
        assert_eq!(ab.behind, Some(2));
    }

    #[test]
    fn build_status_detail_in_sync_clean() {
        let sync = AheadBehind { ahead: Some(0), behind: Some(0) };
        let s = build_status_detail("main", false, &sync);
        assert_eq!(s, "on main · in sync");
    }

    #[test]
    fn build_status_detail_dirty_ahead() {
        let sync = AheadBehind { ahead: Some(2), behind: Some(0) };
        let s = build_status_detail("feature/x", true, &sync);
        assert_eq!(s, "on feature/x · 2 ahead · uncommitted changes");
    }

    #[test]
    fn build_status_detail_behind_only() {
        let sync = AheadBehind { ahead: Some(0), behind: Some(3) };
        let s = build_status_detail("main", false, &sync);
        assert_eq!(s, "on main · 3 behind");
    }

    #[test]
    fn build_status_detail_diverged() {
        let sync = AheadBehind { ahead: Some(1), behind: Some(2) };
        let s = build_status_detail("fix/bug", false, &sync);
        assert_eq!(s, "on fix/bug · 1 ahead, 2 behind");
    }

    #[test]
    fn build_status_detail_no_upstream() {
        // No upstream: ahead/behind are None — only branch + dirty mentioned.
        let sync = AheadBehind::default();
        let s = build_status_detail("local-branch", true, &sync);
        assert_eq!(s, "on local-branch · uncommitted changes");
    }

    #[test]
    fn build_status_detail_no_upstream_clean() {
        let sync = AheadBehind::default();
        let s = build_status_detail("local-branch", false, &sync);
        assert_eq!(s, "on local-branch");
    }

    // ── Update-branch: origin-branch parsing (no real git) ──────────────────

    #[test]
    fn parse_origin_branches_strips_prefix_and_drops_head() {
        let raw = "  origin/HEAD -> origin/main\n  origin/main\n  origin/feature/foo\n";
        let got = parse_origin_branches(raw);
        assert_eq!(got, vec!["main", "feature/foo"]);
    }

    #[test]
    fn parse_origin_branches_ignores_other_remotes_and_blanks() {
        // Only origin/* refs are kept; an upstream/* ref (other remote) is dropped.
        let raw = "origin/main\n\nupstream/main\norigin/dev\n";
        let got = parse_origin_branches(raw);
        assert_eq!(got, vec!["main", "dev"]);
    }

    #[test]
    fn parse_origin_branches_empty_input_is_empty() {
        assert!(parse_origin_branches("").is_empty());
    }

    #[tokio::test]
    async fn list_merge_sources_empty_for_non_repo() {
        // Token-less / no-clone → empty lists (graceful), never an error.
        let dir = std::env::temp_dir().join(format!("cam-upd-nonrepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let got = list_merge_sources(&dir).await;
        assert!(got.local.is_empty());
        assert!(got.origin.is_empty());
    }

    // ── Update-branch: real git round-trip for clean + conflict paths ───────

    #[tokio::test]
    async fn merge_source_clean_then_conflict_path_selection() {
        let base = std::env::temp_dir().join(format!("cam-upd-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let g = |dir: &Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        g(&base, &["init", "-q", "-b", "main"]);
        g(&base, &["config", "user.email", "t@example.com"]);
        g(&base, &["config", "user.name", "Test"]);
        std::fs::write(base.join("f.txt"), "base\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "init"]);

        // ── Clean merge: a source branch that touches a DIFFERENT file. ──
        g(&base, &["checkout", "-q", "-b", "clean-src"]);
        std::fs::write(base.join("other.txt"), "new\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "add other"]);
        g(&base, &["checkout", "-q", "main"]);
        let outcome = merge_source(&base, "clean-src").await.unwrap();
        assert!(matches!(outcome, MergeOutcome::Clean(_)), "clean merge");
        assert!(!is_merge_in_progress(&base).await);

        // ── Conflicting merge: both branches change the SAME line of f.txt. ──
        std::fs::write(base.join("f.txt"), "main-change\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "main edits f"]);
        g(&base, &["checkout", "-q", "-b", "conflict-src", "HEAD~2"]);
        std::fs::write(base.join("f.txt"), "branch-change\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "branch edits f"]);
        g(&base, &["checkout", "-q", "main"]);
        let outcome = merge_source(&base, "conflict-src").await.unwrap();
        match outcome {
            MergeOutcome::Conflicts(paths) => {
                assert!(paths.contains(&"f.txt".to_string()), "f.txt conflicted: {paths:?}");
            }
            MergeOutcome::Clean(_) => panic!("expected a conflict"),
        }
        assert!(is_merge_in_progress(&base).await, "mid-merge after conflict");

        // ── Fail-closed: abort restores a clean (non-merging) tree. ──
        merge_abort(&base).await.unwrap();
        assert!(!is_merge_in_progress(&base).await, "abort cleared the merge");

        // ── Hard error (unknown ref) is an Err, not a false conflict. ──
        let err = merge_source(&base, "no-such-branch").await;
        assert!(err.is_err(), "unknown ref errors");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── Per-UoW worktrees (Decision 1) ──────────────────────────────────────

    #[test]
    fn sanitize_branch_segment_encodes_slashes_and_unsafe_chars() {
        assert_eq!(sanitize_branch_segment("camerata/story-7"), "camerata__story-7");
        assert_eq!(sanitize_branch_segment("feat/a/b"), "feat__a__b");
        // Alnum, dot, dash, underscore are preserved.
        assert_eq!(sanitize_branch_segment("v1.2_x-y"), "v1.2_x-y");
        // Other chars collapse to '-'.
        assert_eq!(sanitize_branch_segment("a b@c"), "a-b-c");
        // Distinct branch names stay distinct.
        assert_ne!(
            sanitize_branch_segment("feat/x"),
            sanitize_branch_segment("feat/y")
        );
        // Empty → a stable fallback segment (never an empty path).
        assert_eq!(sanitize_branch_segment(""), "uow");
    }

    /// Build a throwaway git repo with one commit on `main` and return its dir.
    #[cfg(test)]
    fn init_repo_with_commit(dir: &Path) {
        let g = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        std::fs::create_dir_all(dir).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "init"]);
    }

    /// Which branch a worktree dir currently has checked out (for assertions).
    #[cfg(test)]
    async fn branch_of(dir: &Path) -> String {
        let out = git(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// THE CORE GUARANTEE (what's broken today): two UoWs on the SAME repo with DISTINCT
    /// branches each get their OWN worktree, BOTH branches checked out at once, no error.
    #[tokio::test]
    async fn two_uows_same_repo_distinct_branches_get_separate_worktrees() {
        let base = std::env::temp_dir().join(format!("cam-wt-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let clone = base.join("clone");
        init_repo_with_commit(&clone);

        let wt_a = ensure_uow_worktree(&clone, "camerata/story-a")
            .await
            .expect("worktree A created");
        let wt_b = ensure_uow_worktree(&clone, "camerata/story-b")
            .await
            .expect("worktree B created");

        // Distinct directories.
        assert_ne!(wt_a, wt_b, "each UoW gets its own worktree dir");
        assert!(is_git_repo(&wt_a) && is_git_repo(&wt_b));

        // BOTH branches are checked out simultaneously — the thing the shared clone can't do.
        assert_eq!(branch_of(&wt_a).await, "camerata/story-a");
        assert_eq!(branch_of(&wt_b).await, "camerata/story-b");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Re-resolving the SAME UoW returns the SAME worktree (idempotent — no duplicate add).
    #[tokio::test]
    async fn ensure_uow_worktree_is_idempotent() {
        let base = std::env::temp_dir().join(format!("cam-wt-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let clone = base.join("clone");
        init_repo_with_commit(&clone);

        let first = ensure_uow_worktree(&clone, "camerata/dup")
            .await
            .expect("first");
        let second = ensure_uow_worktree(&clone, "camerata/dup")
            .await
            .expect("second");
        assert_eq!(first, second, "same UoW resolves to the same worktree");

        // Exactly one worktree for this branch is registered (no duplicate `worktree add`).
        let listed = worktree_for_branch(&clone, "camerata/dup").await;
        assert_eq!(listed.as_deref(), Some(first.as_path()));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// "Branch already checked out elsewhere" is handled gracefully: when a branch is live in
    /// an out-of-band worktree, `ensure_uow_worktree` returns THAT path instead of erroring.
    #[tokio::test]
    async fn branch_checked_out_elsewhere_is_reused_not_errored() {
        let base = std::env::temp_dir().join(format!("cam-wt-elsewhere-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let clone = base.join("clone");
        init_repo_with_commit(&clone);

        // Create the branch in a worktree OUTSIDE the `.camerata-worktrees` dir (out-of-band).
        let external = base.join("external-wt");
        let add = git(
            Some(&clone),
            &[
                "worktree",
                "add",
                "-b",
                "camerata/live",
                &external.to_string_lossy(),
            ],
        )
        .await
        .unwrap();
        assert!(add.status.success(), "external worktree: {}", stderr_of(&add));

        // Resolving the UoW for the same branch must NOT error — it returns the existing path.
        let resolved = ensure_uow_worktree(&clone, "camerata/live")
            .await
            .expect("reuses existing worktree, no error");
        // Compare canonical forms: `git worktree list` reports resolved paths (macOS
        // `/var` → `/private/var`), and `ensure_uow_worktree` returns a stable canonical id.
        assert_eq!(
            resolved,
            canonical_or_self(external),
            "returns the already-checked-out worktree"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// remove + prune behave: removing a UoW worktree deregisters it and drops its dir;
    /// prune afterward leaves the clone consistent (no error, branch survives for the PR).
    #[tokio::test]
    async fn remove_and_prune_behave() {
        let base = std::env::temp_dir().join(format!("cam-wt-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let clone = base.join("clone");
        init_repo_with_commit(&clone);

        let wt = ensure_uow_worktree(&clone, "camerata/teardown")
            .await
            .expect("created");
        assert!(wt.exists());

        remove_uow_worktree(&clone, "camerata/teardown").await;
        assert!(!wt.exists(), "worktree dir removed");
        // Deregistered: no worktree holds the branch anymore.
        assert!(worktree_for_branch(&clone, "camerata/teardown").await.is_none());
        // The branch itself survives (it may still back a PR).
        let branch_exists = git(
            Some(&clone),
            &["rev-parse", "--verify", "--quiet", "refs/heads/camerata/teardown"],
        )
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
        assert!(branch_exists, "the branch is left intact after worktree removal");

        // Re-creating after removal works again (and prune of an empty clone is a no-op).
        prune_worktrees(&clone).await;
        let again = ensure_uow_worktree(&clone, "camerata/teardown")
            .await
            .expect("re-created after removal");
        assert!(again.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `resolve_uow_worktree` honors the override path and threads through to a worktree.
    #[tokio::test]
    async fn resolve_uow_worktree_uses_override_clone() {
        let base = std::env::temp_dir().join(format!("cam-wt-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let clone = base.join("my-checkout");
        init_repo_with_commit(&clone);

        let resolved = resolve_uow_worktree(
            Some(&clone.to_string_lossy()),
            None,
            "acme/api",
            "camerata/story-1",
        )
        .await
        .expect("resolves a worktree from the override clone");
        assert!(
            resolved.starts_with(canonical_or_self(clone.clone())),
            "worktree nested under the clone"
        );
        assert_eq!(branch_of(&resolved).await, "camerata/story-1");

        // An unresolved repo (no override, no workspace root) yields None.
        let none = resolve_uow_worktree(None, None, "acme/api", "b").await;
        assert!(none.is_none());

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── Disk-safety unit tests (2026-06-22) ─────────────────────────────────

    /// `shared_target_dir` is a sibling of `.camerata-worktrees/` under the clone.
    #[test]
    fn shared_target_dir_is_sibling_of_worktrees_root() {
        let clone = Path::new("/Users/me/ws/acme/api");
        let target = shared_target_dir(clone);
        // Must live INSIDE the clone directory.
        assert!(target.starts_with(clone), "shared target under clone");
        // Must be `.camerata-shared-target`, NOT inside `.camerata-worktrees/`.
        assert_eq!(
            target.file_name().and_then(|n| n.to_str()),
            Some(".camerata-shared-target"),
            "correct dir name"
        );
        // Confirm it is a DIRECT child (one level), not nested inside worktrees.
        assert_eq!(
            target,
            clone.join(".camerata-shared-target"),
            "exact expected path"
        );
    }

    /// The derivation `worktree.parent().parent()` recovers the clone root for the
    /// canonical layout `<clone>/.camerata-worktrees/<branch>`.
    #[test]
    fn clone_derivation_from_worktree_path() {
        let clone = Path::new("/Users/me/ws/acme/api");
        let branch_dir = worktrees_root(clone).join("camerata__story-7");
        // Derive clone root: parent() → .camerata-worktrees, parent() → clone.
        let derived_clone = branch_dir.parent().and_then(|p| p.parent());
        assert_eq!(derived_clone, Some(clone), "clone root correctly recovered");

        // shared_target_dir derived from the worktree path must equal the one from clone.
        let derived_target = derived_clone.map(shared_target_dir);
        assert_eq!(derived_target, Some(shared_target_dir(clone)));
    }

    /// `has_headroom` is a pure decision function: available >= min → true.
    #[test]
    fn has_headroom_is_true_when_available_meets_threshold() {
        assert!(has_headroom(10 * 1024 * 1024 * 1024, 10 * 1024 * 1024 * 1024));
        assert!(has_headroom(20 * 1024 * 1024 * 1024, 10 * 1024 * 1024 * 1024));
        assert!(has_headroom(u64::MAX, 0));
    }

    /// `has_headroom` returns false when available < min (the guard must fire).
    #[test]
    fn has_headroom_is_false_when_below_threshold() {
        assert!(!has_headroom(0, 1));
        assert!(!has_headroom(
            5 * 1024 * 1024 * 1024,
            10 * 1024 * 1024 * 1024
        ));
        assert!(!has_headroom(131 * 1024 * 1024, 10 * 1024 * 1024 * 1024)); // incident: 131 MB free
    }

    /// `ensure_disk_headroom` returns `Ok(())` when space is sufficient (simulated via
    /// a path with real free space and a min of 0).
    #[test]
    fn ensure_disk_headroom_passes_at_zero_minimum() {
        // Any real path with 0 minimum always has headroom.
        let tmp = std::env::temp_dir();
        assert!(ensure_disk_headroom(&tmp, 0).is_ok());
    }

    /// `ensure_disk_headroom` returns `Err` when min exceeds any conceivable free space.
    #[test]
    fn ensure_disk_headroom_fails_when_min_exceeds_free_space() {
        let tmp = std::env::temp_dir();
        // u64::MAX bytes required — no disk can satisfy this.
        assert!(ensure_disk_headroom(&tmp, u64::MAX).is_err());
    }

    /// The error message from `ensure_disk_headroom` names free / required amounts
    /// and includes actionable remediation text.
    #[test]
    fn ensure_disk_headroom_error_is_actionable() {
        let tmp = std::env::temp_dir();
        let err = ensure_disk_headroom(&tmp, u64::MAX).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("insufficient disk headroom"), "names problem: {msg}");
        assert!(msg.contains("GB free"), "states free space: {msg}");
        assert!(msg.contains("reclaim space"), "gives remediation: {msg}");
    }

    /// `parse_disk_headroom_gb` returns the default bytes when env var is absent (None).
    #[test]
    fn disk_headroom_threshold_falls_back_to_default() {
        // Tests drive the pure parse function, not the env-reading wrapper, to avoid
        // races on the shared process environment when tests run in parallel.
        assert_eq!(
            parse_disk_headroom_gb(None, MIN_DISK_HEADROOM_BYTES),
            MIN_DISK_HEADROOM_BYTES
        );
    }

    /// `parse_disk_headroom_gb` converts a "5" override to 5 GiB in bytes.
    #[test]
    fn disk_headroom_threshold_respects_env_override() {
        let threshold = parse_disk_headroom_gb(Some("5"), MIN_DISK_HEADROOM_BYTES);
        assert_eq!(threshold, 5 * 1024 * 1024 * 1024, "5 GB from env var override");
    }

    /// `parse_disk_headroom_gb` falls back to default on non-numeric input.
    #[test]
    fn disk_headroom_threshold_falls_back_on_invalid_input() {
        assert_eq!(
            parse_disk_headroom_gb(Some("not-a-number"), MIN_DISK_HEADROOM_BYTES),
            MIN_DISK_HEADROOM_BYTES
        );
    }

    // Tests for snapshot_worktree
    #[tokio::test]
    async fn snapshot_worktree_noop_on_clean_tree() {
        let dir = std::env::temp_dir().join(format!("cam-snap-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Init git repo with an initial commit.
        let _ = tokio::process::Command::new("git").args(["init"]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["config", "user.name", "T"]).current_dir(&dir).output().await;
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        let _ = tokio::process::Command::new("git").args(["add", "."]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["commit", "-m", "init"]).current_dir(&dir).output().await;
        // Tree is clean — snapshot should be a no-op.
        let result = snapshot_worktree(&dir, "test-task").await.unwrap();
        assert!(result.is_none(), "clean tree must return None (no-op)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_worktree_commits_dirty_tree() {
        let dir = std::env::temp_dir().join(format!("cam-snap-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = tokio::process::Command::new("git").args(["init"]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["config", "user.name", "T"]).current_dir(&dir).output().await;
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        let _ = tokio::process::Command::new("git").args(["add", "."]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["commit", "-m", "init"]).current_dir(&dir).output().await;
        // Dirty: write a new file without committing.
        std::fs::write(dir.join("new.rs"), "fn main() {}").unwrap();
        let result = snapshot_worktree(&dir, "my-task").await.unwrap();
        // Should have made a commit (result is not None).
        // (SHA may or may not be parseable from git output depending on format, but commit happened.)
        let _ = result; // we just check no error
        // Verify the new file is now committed (git status clean).
        let status = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();
        assert!(status.stdout.is_empty(), "tree must be clean after snapshot");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_worktree_commit_message_contains_task() {
        let dir = std::env::temp_dir().join(format!("cam-snap-msg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = tokio::process::Command::new("git").args(["init"]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["config", "user.name", "T"]).current_dir(&dir).output().await;
        std::fs::write(dir.join("f.txt"), "a").unwrap();
        let _ = tokio::process::Command::new("git").args(["add", "."]).current_dir(&dir).output().await;
        let _ = tokio::process::Command::new("git").args(["commit", "-m", "init"]).current_dir(&dir).output().await;
        // Make dirty.
        std::fs::write(dir.join("f.txt"), "b").unwrap();
        let _ = snapshot_worktree(&dir, "dev-implement iteration 2").await.unwrap();
        // Verify commit message.
        let log = tokio::process::Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();
        let msg = String::from_utf8_lossy(&log.stdout);
        assert!(msg.contains("camerata: snapshot dev-implement iteration 2"), "commit msg must contain task label; got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── init_repo_with_initial_commit / set_origin (create-app flow, Part 2) ───────

    /// `init_repo_with_initial_commit` turns a bare directory (no prior git history)
    /// into a clean repo with one commit, WITHOUT relying on any global
    /// `user.name`/`user.email` config — it sets a local bot identity itself, so this
    /// must succeed even in an environment with no global git config at all (which is
    /// exactly why the function sets it explicitly rather than assuming it exists).
    #[tokio::test]
    async fn init_repo_with_initial_commit_succeeds_with_no_global_git_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();

        init_repo_with_initial_commit(dir.path(), "Initial scaffold (Camerata)")
            .await
            .expect("init + commit must succeed even with no global git identity");

        let status = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        assert!(status.stdout.is_empty(), "tree must be clean after the initial commit");

        let log = tokio::process::Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("Initial scaffold (Camerata)"));

        let branch = tokio::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "main");
    }

    /// `set_origin` points `origin` at the TOKENLESS URL — the token must never land
    /// in `.git/config`, even though the create-app flow that calls this has a real
    /// token in hand for the (separate, transient) push step.
    #[tokio::test]
    async fn set_origin_persists_the_tokenless_url() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        init_repo_with_initial_commit(dir.path(), "init")
            .await
            .expect("init + commit");

        set_origin(dir.path(), "acme/my-app").await.expect("set_origin");

        let remote = tokio::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        let url = String::from_utf8_lossy(&remote.stdout).trim().to_string();
        assert_eq!(url, "https://github.com/acme/my-app.git");
        assert!(!url.contains('@'), "persisted origin must carry no credentials");
    }

    // ── Link-existing-clone origin validation (readiness-gate ADR) ──────────────────

    #[test]
    fn origin_matches_repo_normalizes_https_and_ssh_and_git_suffix() {
        // https, with and without a trailing `.git` / slash.
        assert!(origin_matches_repo("https://github.com/me/api.git", "me/api"));
        assert!(origin_matches_repo("https://github.com/me/api", "me/api"));
        assert!(origin_matches_repo("https://github.com/me/api/", "me/api"));
        // ssh form, with and without `.git`.
        assert!(origin_matches_repo("git@github.com:me/api.git", "me/api"));
        assert!(origin_matches_repo("git@github.com:me/api", "me/api"));
        // Case-insensitive on the whole owner/repo.
        assert!(origin_matches_repo("https://github.com/Me/API.git", "me/api"));
        assert!(origin_matches_repo("git@github.com:ME/Api", "me/API"));
    }

    #[test]
    fn origin_matches_repo_rejects_different_repo_and_non_github() {
        // A DIFFERENT repo must not match — this is the guard against linking the wrong folder.
        assert!(!origin_matches_repo("https://github.com/me/other.git", "me/api"));
        assert!(!origin_matches_repo("git@github.com:someone/api.git", "me/api"));
        // Non-GitHub / unparseable remotes never match.
        assert!(!origin_matches_repo("https://gitlab.com/me/api.git", "me/api"));
        assert!(!origin_matches_repo("not-a-url", "me/api"));
        assert!(!origin_matches_repo("", "me/api"));
    }

    /// Build a throwaway git repo with a specific `origin` remote URL (no commit needed —
    /// validation only reads `remote.origin.url`).
    #[cfg(test)]
    fn init_repo_with_origin(dir: &Path, origin: &str) {
        let g = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        std::fs::create_dir_all(dir).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["remote", "add", "origin", origin]);
    }

    #[tokio::test]
    async fn validate_link_target_accepts_matching_origin_https_and_ssh() {
        let base = std::env::temp_dir().join(format!("cam-link-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // https origin with `.git` suffix → matches the bare `owner/repo`.
        let https = base.join("https");
        init_repo_with_origin(&https, "https://github.com/me/api.git");
        assert!(
            validate_link_target(&https, "me/api").await.is_ok(),
            "an https clone of me/api must validate"
        );

        // ssh origin (no `.git`) → also matches after normalization.
        let ssh = base.join("ssh");
        init_repo_with_origin(&ssh, "git@github.com:me/api");
        assert!(
            validate_link_target(&ssh, "me/api").await.is_ok(),
            "an ssh clone of me/api must validate (ssh vs https normalization)"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn validate_link_target_rejects_mismatched_origin() {
        let base = std::env::temp_dir().join(format!("cam-link-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clone");
        // A clone of a DIFFERENT repo.
        init_repo_with_origin(&dir, "https://github.com/me/other.git");
        let err = validate_link_target(&dir, "me/api")
            .await
            .expect_err("a mismatched origin must be rejected");
        assert!(
            err.contains("me/other"),
            "the error names the actual origin so the user sees why; got: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn validate_link_target_rejects_non_git_folder() {
        let base = std::env::temp_dir().join(format!("cam-link-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A plain folder, not a git clone.
        let err = validate_link_target(&base, "me/api")
            .await
            .expect_err("a non-git folder must be rejected");
        assert!(
            err.contains("not a git clone"),
            "the error explains it is not a git clone; got: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── Bug regression: repo_resolution must be case-insensitive on origin (Bug 1) ─────────────
    //
    // `validate_link_target` accepts a link when the folder's origin casing DIFFERS from the stored
    // project identity (e.g. stored `Owner/Repo`, origin `owner/repo`), because it uses
    // `eq_ignore_ascii_case`. Before the fix, `repo_resolution` used `==` (case-sensitive), so a
    // successfully-linked repo would still fail the resolution check and leave the project stuck
    // Unlinked/Partial (paused) forever. This test is the regression guard.
    #[tokio::test]
    async fn repo_resolution_resolved_when_origin_casing_differs_from_stored_repo() {
        let base =
            std::env::temp_dir().join(format!("cam-res-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("clone");

        // Clone whose origin uses LOWER-CASE `owner/repo` (what GitHub actually stores).
        init_repo_with_origin(&dir, "https://github.com/owner/repo.git");

        // But the project stores the identity with UPPER-CASE `Owner/Repo` (e.g. typed by the
        // user or imported from a GitHub API response that preserves display casing).
        let stored_repo = "Owner/Repo";

        let resolution = repo_resolution(
            Some(&dir.to_string_lossy()),
            None,  // no workspace root — use the explicit override path
            stored_repo,
        )
        .await;

        assert!(
            resolution.resolved,
            "repo_resolution must resolve when origin casing differs from stored identity; reason: {}",
            resolution.reason
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── Override-aware checkout_status_resolved (issue #38) ─────────────────────────────────────
    //
    // The Workspace status handler used to check only the DERIVED path
    // `<workspace_root>/<owner>/<repo>`, so a clone at a non-standard path was reported "not
    // cloned" forever. `checkout_status_resolved` resolves via the per-repo override first.

    #[tokio::test]
    async fn resolved_status_reports_cloned_via_override_matching_origin() {
        let base = std::env::temp_dir().join(format!("cam-rstat-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // A clone of me/api at a FLAT, non-`<owner>/<repo>` path.
        let dir = base.join("flat-checkout");
        init_repo_with_origin(&dir, "https://github.com/me/api.git");

        let st = checkout_status_resolved(
            Some(&dir.to_string_lossy()),
            None, // no workspace root — resolve purely via the override
            "me/api",
        )
        .await;

        assert!(
            st.cloned,
            "an override pointing at a real matching checkout must report cloned; detail: {}",
            st.detail
        );
        assert_eq!(
            st.path,
            dir.to_string_lossy(),
            "the reported path must be the resolved override path, not the derived one"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn resolved_status_not_cloned_when_override_points_at_wrong_origin() {
        let base = std::env::temp_dir().join(format!("cam-rstat-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("other-checkout");
        // A git checkout, but of a DIFFERENT repo.
        init_repo_with_origin(&dir, "https://github.com/me/other.git");

        let st =
            checkout_status_resolved(Some(&dir.to_string_lossy()), None, "me/api").await;

        assert!(
            !st.cloned,
            "a git folder with the wrong origin must NOT count as cloned"
        );
        assert!(
            st.detail.contains("me/other") || st.detail.contains("different repo"),
            "the detail must explain it's a different repo; got: {}",
            st.detail
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn resolved_status_does_not_error_when_no_workspace_root_but_override_resolves() {
        // Regression for the relaxed error: with NO workspace root, a per-repo override still
        // resolves and reports cloned (the old handler hard-errored the whole endpoint here).
        let base = std::env::temp_dir().join(format!("cam-rstat-noroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("checkout");
        init_repo_with_origin(&dir, "https://github.com/me/api.git");

        let st =
            checkout_status_resolved(Some(&dir.to_string_lossy()), None, "me/api").await;
        assert!(st.cloned, "override must resolve with no workspace root; detail: {}", st.detail);

        // And with NEITHER an override NOR a workspace root: not-cloned with a helpful reason,
        // never a panic/error.
        let none = checkout_status_resolved(None, None, "me/api").await;
        assert!(!none.cloned);
        assert!(
            none.detail.contains("no local path"),
            "detail should guide the user to link a folder; got: {}",
            none.detail
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
