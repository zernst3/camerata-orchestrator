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

/// Clone the repo into the workspace, or fast-forward an existing clone. Returns the
/// resulting status. The token is used only for this network step and is scrubbed
/// from the persisted remote.
pub async fn clone_or_pull(root: &Path, repo: &str, token: &str) -> RepoCheckout {
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
    match detect_remote_repo(&dir).await {
        Ok(found) if found == repo => RepoResolution {
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
    create_branch_at(dir, branch).await?;
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
    // Push the branch to origin so it exists remotely too (no PR). FORCE the push: this is a
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
    Ok(dir.to_string_lossy().into_owned())
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
    use camerata_worktracker::{HttpTransport, ReqwestTransport};

    let (owner, _name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repo must be owner/repo, got {repo}"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let api = "https://api.github.com";

    let meta = transport.get(&format!("{api}/repos/{repo}")).await?;
    if !(200..300).contains(&meta.status) {
        anyhow::bail!("GET repo {repo}: HTTP {} {}", meta.status, meta.body);
    }
    let base = serde_json::from_str::<serde_json::Value>(&meta.body)?["default_branch"]
        .as_str()
        .unwrap_or("main")
        .to_string();

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
        return Ok(v["html_url"].as_str().unwrap_or_default().to_string());
    }
    // A PR for this head may already exist — find and return it.
    if pr.status == 422 {
        let list = transport
            .get(&format!(
                "{api}/repos/{repo}/pulls?head={owner}:{head}&state=open"
            ))
            .await?;
        if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(&list.body) {
            if let Some(first) = arr.first() {
                return Ok(first["html_url"].as_str().unwrap_or_default().to_string());
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
}
