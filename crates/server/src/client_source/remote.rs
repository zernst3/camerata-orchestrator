//! Talking to the repo's remote for a re-fetch: the full-history clone, the remote probe, and the
//! plain-language classification of what git says when it fails.
//!
//! The token only ever appears in the command line of the network step (`git clone`,
//! `git ls-remote`, `git fetch`), exactly as everywhere else in Camerata (`workspace` module
//! docs). The persisted `origin` is rewritten to the tokenless URL straight after, and every
//! piece of git's output is run through [`redact_token`] before it is classified or shown.

use std::path::Path;
use std::sync::Arc;

use tokio::process::Command;

use crate::workspace::{authed_url, clean_url};

/// Where a repo's remote lives, with a seam for tests. Production resolves every repo to its
/// GitHub HTTPS URL with the token embedded; a test maps a repo to a local `file://` bare
/// repository instead (and may declare a GitHub rename), so nothing touches the network.
#[derive(Debug, Clone, Default)]
pub struct GitRemotes {
    urls: Arc<std::collections::HashMap<String, String>>,
    full_names: Arc<std::collections::HashMap<String, String>>,
}

impl GitRemotes {
    /// Map `repo` to `url` for every network git step.
    #[must_use]
    pub fn with_url(mut self, repo: &str, url: &str) -> Self {
        Arc::make_mut(&mut self.urls).insert(repo.to_ascii_lowercase(), url.to_string());
        self
    }

    /// Declare that GitHub now calls `repo` `full_name` (a rename), for tests.
    #[must_use]
    pub fn with_full_name(mut self, repo: &str, full_name: &str) -> Self {
        Arc::make_mut(&mut self.full_names)
            .insert(repo.to_ascii_lowercase(), full_name.to_string());
        self
    }

    /// The URL to run network git commands against: the override, else GitHub with the token.
    pub fn fetch_url(&self, repo: &str, token: &str) -> String {
        self.urls
            .get(&repo.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| authed_url(repo, token))
    }

    /// What GitHub currently calls `repo` (`owner/name`), or `None` when it cannot be learned.
    /// A repo that was renamed answers its NEW name: git clones through GitHub's redirect without
    /// saying so, the API does not.
    pub async fn current_full_name(&self, repo: &str, token: &str) -> Option<String> {
        if !self.urls.is_empty() || !self.full_names.is_empty() {
            // Sandboxed (tests): never the network.
            return self.full_names.get(&repo.to_ascii_lowercase()).cloned();
        }
        let client = reqwest::Client::builder()
            .user_agent("camerata")
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .ok()?;
        let resp = client
            .get(format!("https://api.github.com/repos/{repo}"))
            .bearer_auth(token)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().await.ok()?;
        json.get("full_name")
            .and_then(|n| n.as_str())
            .map(str::to_string)
    }
}

/// `Some(new)` when GitHub's current name differs from the name the project records
/// (case-insensitively: GitHub names are case-insensitive).
pub fn renamed_to(project_repo: &str, current: Option<&str>) -> Option<String> {
    let current = current?.trim();
    (!current.is_empty() && !current.eq_ignore_ascii_case(project_repo.trim()))
        .then(|| current.to_string())
}

/// Why a network git step failed, in the operator's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefetchError {
    /// GitHub rejected the credentials.
    AuthFailed,
    /// The repository is not visible to this token (deleted, renamed away, or private).
    RepoNotFound,
    /// Anything else: DNS, TLS, timeouts, or git's own message (trimmed, token removed).
    Network(String),
}

/// Remove the token from git's output: the whole authed URL becomes the clean one, and any
/// remaining occurrence of the token itself is masked.
pub fn redact_token(text: &str, repo: &str, token: &str) -> String {
    let mut out = text.to_string();
    if !token.is_empty() {
        out = out.replace(&authed_url(repo, token), &clean_url(repo));
        out = out.replace(token, "[token]");
    }
    out
}

/// Classify git's stderr (ALREADY redacted by [`redact_token`]) from a clone, fetch or ls-remote.
pub fn classify_git_failure(stderr: &str) -> RefetchError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("invalid username or token")
        || lower.contains("bad credentials")
        || lower.contains("terminal prompts disabled")
    {
        return RefetchError::AuthFailed;
    }
    if lower.contains("repository not found")
        || (lower.contains("repository '") && lower.contains("not found"))
    {
        return RefetchError::RepoNotFound;
    }
    RefetchError::Network(stderr.trim().to_string())
}

/// The arguments of the full-history clone. Spelled out (and asserted by a test) so the intent
/// is in the command line: NO `--depth`, NO `--filter`, every branch fetched.
pub fn clone_args(source_url: &str, dir: &Path) -> Vec<String> {
    vec![
        "clone".to_string(),
        "--no-single-branch".to_string(),
        source_url.to_string(),
        dir.to_string_lossy().into_owned(),
    ]
}

/// A git command that can never stop to ask for a password.
fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0").kill_on_drop(true);
    cmd
}

/// Clone `repo` into `dir` with its FULL history (`secret` is the token whose traces must not
/// survive, and `source_url` is where to clone from). The origin is rewritten to the tokenless
/// GitHub URL straight away and `.git/config` is checked for the secret afterwards.
pub async fn clone_full_from(
    source_url: &str,
    repo: &str,
    dir: &Path,
    secret: &str,
) -> Result<(), RefetchError> {
    let out = git_command()
        .args(clone_args(source_url, dir))
        .output()
        .await
        .map_err(|e| RefetchError::Network(format!("couldn't run git: {e}")))?;
    if !out.status.success() {
        let stderr = redact_token(&String::from_utf8_lossy(&out.stderr), repo, secret);
        return Err(classify_git_failure(&stderr));
    }
    // Scrub the token from origin so it never persists on disk.
    let scrub = git_command()
        .current_dir(dir)
        .args(["remote", "set-url", "origin", &clean_url(repo)])
        .output()
        .await
        .map_err(|e| RefetchError::Network(format!("couldn't run git: {e}")))?;
    if !scrub.status.success() {
        return Err(RefetchError::Network(redact_token(
            &String::from_utf8_lossy(&scrub.stderr),
            repo,
            secret,
        )));
    }
    if !secret.is_empty() {
        let config = tokio::fs::read_to_string(dir.join(".git").join("config"))
            .await
            .unwrap_or_default();
        if config.contains(secret) {
            return Err(RefetchError::Network(
                "the clone's .git/config still contains the access token; the clone was kept \
                 but should be removed"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// The remote's default branch and head, from `git ls-remote --symref <url> HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHead {
    pub default_branch: Option<String>,
    pub head_sha: String,
}

/// Parse `ls-remote --symref` output:
///
/// ```text
/// ref: refs/heads/main<TAB>HEAD
/// 3f2a9c14...<TAB>HEAD
/// ```
pub fn parse_symref_head(stdout: &str) -> Option<RemoteHead> {
    let mut branch = None;
    let mut sha = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
            branch = rest.split('\t').next().map(str::to_string);
        } else if let Some((id, name)) = line.split_once('\t') {
            if name.trim() == "HEAD" && crate::client_source::git::is_full_sha(id) {
                sha = Some(id.to_string());
            }
        }
    }
    sha.map(|head_sha| RemoteHead {
        default_branch: branch,
        head_sha,
    })
}

/// Probe the remote for its default branch and head.
pub async fn probe_remote_from(
    source_url: &str,
    repo: &str,
    secret: &str,
) -> Result<RemoteHead, RefetchError> {
    let out = git_command()
        .args(["ls-remote", "--symref", source_url, "HEAD"])
        .output()
        .await
        .map_err(|e| RefetchError::Network(format!("couldn't run git: {e}")))?;
    if !out.status.success() {
        let stderr = redact_token(&String::from_utf8_lossy(&out.stderr), repo, secret);
        return Err(classify_git_failure(&stderr));
    }
    parse_symref_head(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| {
        RefetchError::Network("the remote did not report a default branch head".to_string())
    })
}

/// Fetch one commit by id from the remote (for the "inspected commit" target when the clone does
/// not already have it). Returns `false` when the remote does not serve it.
pub async fn fetch_commit_from(
    dir: &Path,
    source_url: &str,
    sha: &str,
    repo: &str,
    secret: &str,
) -> bool {
    if !crate::client_source::git::is_full_sha(sha) {
        return false;
    }
    let out = git_command()
        .current_dir(dir)
        .args(["fetch", "--no-tags", source_url, sha])
        .output()
        .await;
    match out {
        Ok(o) => o.status.success(),
        Err(e) => {
            tracing::warn!(error = %redact_token(&e.to_string(), repo, secret), "git fetch failed to start");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_git::{bare_remote, git, push_commit};
    use tempfile::TempDir;

    const TOKEN: &str = "ghp_SECRETSECRETSECRETSECRETSECRET123456";

    #[test]
    fn redact_token_removes_authed_url_from_stderr() {
        let authed = authed_url("acme/app", TOKEN);
        let stderr =
            format!("fatal: unable to access '{authed}': Could not resolve host\nheader {TOKEN}");
        let clean = redact_token(&stderr, "acme/app", TOKEN);
        assert!(!clean.contains(TOKEN), "{clean}");
        assert!(clean.contains("https://github.com/acme/app.git"));
        assert_eq!(redact_token("nothing here", "acme/app", ""), "nothing here");
    }

    #[test]
    fn classify_git_failure_maps_auth_not_found_and_network() {
        let auth = [
            "remote: Invalid username or token. Password authentication is not supported for Git operations.\nfatal: Authentication failed for 'https://github.com/acme/app.git/'",
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            "remote: Bad credentials",
        ];
        for s in auth {
            assert_eq!(classify_git_failure(s), RefetchError::AuthFailed, "{s}");
        }
        assert_eq!(
            classify_git_failure(
                "remote: Repository not found.\nfatal: repository 'https://github.com/acme/app.git/' not found"
            ),
            RefetchError::RepoNotFound
        );
        let net = classify_git_failure(
            "fatal: unable to access 'https://github.com/acme/app.git/': Could not resolve host: github.com\n",
        );
        assert!(
            matches!(&net, RefetchError::Network(m) if m.contains("Could not resolve host")),
            "{net:?}"
        );
        assert!(matches!(
            classify_git_failure("  some other failure \n"),
            RefetchError::Network(m) if m == "some other failure"
        ));
    }

    #[test]
    fn remote_probe_parses_symref_default_branch_and_head() {
        let out = format!("ref: refs/heads/main\tHEAD\n{}\tHEAD\n", "a".repeat(40));
        assert_eq!(
            parse_symref_head(&out),
            Some(RemoteHead {
                default_branch: Some("main".to_string()),
                head_sha: "a".repeat(40)
            })
        );
        // A remote whose HEAD is not a branch still has a head commit.
        let detached = format!("{}\tHEAD\n", "b".repeat(40));
        assert_eq!(
            parse_symref_head(&detached),
            Some(RemoteHead {
                default_branch: None,
                head_sha: "b".repeat(40)
            })
        );
        assert_eq!(parse_symref_head(""), None);
        assert_eq!(parse_symref_head("ref: refs/heads/main\tHEAD\n"), None);
    }

    #[test]
    fn refetch_passes_no_depth_and_no_single_branch() {
        let args = clone_args(
            "https://x@github.com/acme/app.git",
            Path::new("/work/acme/app"),
        );
        assert_eq!(args[0], "clone");
        assert!(args.contains(&"--no-single-branch".to_string()));
        assert!(
            !args.iter().any(|a| a.starts_with("--depth")
                || a.starts_with("--filter")
                || a == "--shallow-since"
                || a == "--single-branch"),
            "{args:?}"
        );
        assert_eq!(args[2], "https://x@github.com/acme/app.git");
        assert_eq!(args[3], "/work/acme/app");
    }

    #[test]
    fn renamed_to_reports_only_a_real_difference() {
        assert_eq!(
            renamed_to("acme/app", Some("acme/new-name")).as_deref(),
            Some("acme/new-name")
        );
        assert_eq!(renamed_to("acme/app", Some("ACME/App")), None);
        assert_eq!(renamed_to("acme/app", None), None);
        assert_eq!(renamed_to("acme/app", Some("  ")), None);
    }

    #[test]
    fn fetch_url_is_the_override_or_github_with_the_token() {
        let remotes = GitRemotes::default().with_url("Acme/App", "file:///tmp/remote.git");
        assert_eq!(
            remotes.fetch_url("acme/app", TOKEN),
            "file:///tmp/remote.git"
        );
        assert_eq!(
            GitRemotes::default().fetch_url("acme/app", "t0k"),
            "https://x-access-token:t0k@github.com/acme/app.git"
        );
    }

    #[tokio::test]
    async fn clone_full_keeps_every_commit_and_every_branch() {
        let tmp = TempDir::new().unwrap();
        let (bare, url, _first) = bare_remote(tmp.path(), &[("a.txt", "1\n")]);
        for i in 2..=4 {
            push_commit(
                tmp.path(),
                "a.txt",
                &format!("{i}\n"),
                &format!("commit {i}"),
            );
        }
        git(
            &tmp.path().join("seed"),
            &["push", "-q", "origin", "main:side-branch"],
        );
        let dest = tmp.path().join("clone");

        clone_full_from(&url, "acme/app", &dest, "").await.unwrap();

        assert_eq!(
            git(&dest, &["rev-list", "--count", "--all"]),
            git(&bare, &["rev-list", "--count", "--all"])
        );
        assert_eq!(
            git(&dest, &["rev-parse", "--is-shallow-repository"]),
            "false"
        );
        assert!(git(&dest, &["branch", "-r"]).contains("origin/side-branch"));
        assert_eq!(
            git(&dest, &["config", "--get", "remote.origin.url"]),
            "https://github.com/acme/app.git"
        );
    }

    #[tokio::test]
    async fn refetch_scrubs_token_from_git_config() {
        let tmp = TempDir::new().unwrap();
        // A source URL that CONTAINS the secret, like an authed GitHub URL does: git records it
        // as origin on clone, and the scrub must replace it.
        let secret = "SECRETSECRET";
        let base = tmp.path().join(secret);
        std::fs::create_dir_all(&base).unwrap();
        let (_bare, url, _) = bare_remote(&base, &[("a.txt", "1\n")]);
        assert!(url.contains(secret));
        let dest = tmp.path().join("clone");

        clone_full_from(&url, "acme/app", &dest, secret)
            .await
            .unwrap();

        let config = std::fs::read_to_string(dest.join(".git/config")).unwrap();
        assert!(!config.contains(secret), "{config}");
        assert!(config.contains("https://github.com/acme/app.git"));
    }

    #[tokio::test]
    async fn clone_failures_are_classified_and_never_echo_the_secret() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("no-such-remote.git");
        let url = format!("file://{}", missing.display());
        let err = clone_full_from(&url, "acme/app", &tmp.path().join("clone"), "sekret")
            .await
            .unwrap_err();
        assert!(matches!(err, RefetchError::Network(_)), "{err:?}");
        assert!(!format!("{err:?}").contains("sekret"));
    }

    #[tokio::test]
    async fn the_probe_reads_a_real_remote_head() {
        let tmp = TempDir::new().unwrap();
        let (_bare, url, sha) = bare_remote(tmp.path(), &[("a.txt", "1\n")]);
        let head = probe_remote_from(&url, "acme/app", "").await.unwrap();
        assert_eq!(head.head_sha, sha);
        assert_eq!(head.default_branch.as_deref(), Some("main"));
        let broken = probe_remote_from("file:///definitely/not/here.git", "acme/app", "").await;
        assert!(
            matches!(broken, Err(RefetchError::Network(_))),
            "{broken:?}"
        );
    }
}
