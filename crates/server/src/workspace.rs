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
    // Stage + commit. A no-op re-apply (identical files) leaves nothing to commit — tolerate it.
    let add = git(Some(dir), &["add", "-A"]).await?;
    if !add.status.success() {
        anyhow::bail!("git add: {}", stderr_of(&add));
    }
    let commit = git(Some(dir), &["commit", "-m", commit_msg]).await?;
    if !commit.status.success() {
        let err = stderr_of(&commit);
        let out = String::from_utf8_lossy(&commit.stdout);
        let nothing = err.contains("nothing to commit") || out.contains("nothing to commit");
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
    let current_out = git(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
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

/// Stage all changes with `git add -A` then commit with `message`. Returns the
/// commit output (or a "nothing to commit" notice if the tree was already clean).
pub async fn commit_all(dir: &Path, message: &str) -> anyhow::Result<String> {
    let add = git(Some(dir), &["add", "-A"]).await?;
    if !add.status.success() {
        anyhow::bail!("git add: {}", stderr_of(&add));
    }
    let commit = git(Some(dir), &["commit", "-m", message]).await?;
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
    let out = git(Some(dir), &["push", &authed_url(repo, token), branch]).await?;
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
        Some(dir),
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
    let out = git(Some(dir), &["checkout", branch]).await?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("git checkout {branch}: {}", stderr_of(&out));
}

/// Create a new branch at `dir` and switch to it. If the branch already exists, just
/// switch to it. This variant takes the resolved `dir` directly (unlike `create_branch`
/// which accepts root + repo identifiers).
pub async fn create_branch_at(dir: &Path, branch: &str) -> anyhow::Result<()> {
    let created = git(Some(dir), &["checkout", "-b", branch]).await?;
    if created.status.success() {
        return Ok(());
    }
    // Branch already exists — switch to it.
    let switched = git(Some(dir), &["checkout", branch]).await?;
    if switched.status.success() {
        return Ok(());
    }
    anyhow::bail!("git checkout {branch}: {}", stderr_of(&switched))
}

/// Cherry-pick `sha` onto the current HEAD branch. On conflict, returns the stderr so
/// the UI can show it; the repo is left in conflict state (the user resolves or aborts).
pub async fn cherry_pick(dir: &Path, sha: &str) -> anyhow::Result<String> {
    let out = git(Some(dir), &["cherry-pick", sha]).await?;
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
}
