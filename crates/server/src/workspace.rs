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

use serde::Serialize;
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
    let _ = git(Some(&dir), &["remote", "set-url", "origin", &clean_url(repo)]).await;
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
        assert_eq!(parse_owner_repo("https://github.com/acme/api.git"), Some("acme/api".into()));
        assert_eq!(parse_owner_repo("https://github.com/acme/api"), Some("acme/api".into()));
        assert_eq!(parse_owner_repo("git@github.com:acme/api.git"), Some("acme/api".into()));
        assert_eq!(parse_owner_repo("git@github.com:acme/api.git\n"), Some("acme/api".into()));
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
        let out = git(None, &["clone", &origin.to_string_lossy(), &dir.to_string_lossy()])
            .await
            .unwrap();
        assert!(out.status.success(), "clone: {}", stderr_of(&out));

        // Status: cloned, on a branch, clean.
        let st = checkout_status(&workspace, "local/demo").await;
        assert!(st.cloned);
        assert!(st.branch.is_some());
        assert!(!st.dirty);

        // Create a working branch and prove status reflects it.
        create_branch(&workspace, "local/demo", "camerata/work").await.unwrap();
        let st2 = checkout_status(&workspace, "local/demo").await;
        assert_eq!(st2.branch.as_deref(), Some("camerata/work"));

        // A local edit makes it dirty.
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let st3 = checkout_status(&workspace, "local/demo").await;
        assert!(st3.dirty);

        let _ = std::fs::remove_dir_all(&base);
    }
}
