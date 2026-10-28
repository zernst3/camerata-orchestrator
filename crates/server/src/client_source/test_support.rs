//! Shared fixture for the `client_source` unit tests: a project `acme/app` whose checkout is a
//! real git clone (of a local bare remote, with `origin` rewritten to the GitHub URL) inside a
//! temp directory, an `AppState` whose stores live in that directory, and nothing outside it.
//!
//! Every path a test can delete is under `root`, a canonical temp directory the test owns.

// The fixture is shared by several test modules added across the build; not every module uses
// every field.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::remote::GitRemotes;
use super::storage::StorageRoots;
use crate::onboard::{AuditedRef, ScanReport};
use crate::test_git::{bare_remote, clone_as};
use crate::AppState;

pub(crate) const REPO: &str = "acme/app";

pub(crate) struct Fx {
    _tmp: TempDir,
    /// Canonical temp root; the workspace root is `root/work`, the data dir `root/data`.
    pub root: PathBuf,
    pub state: AppState,
    pub project_id: String,
    /// `root/work/acme/app`, a clone whose `origin` is `https://github.com/acme/app.git`.
    pub checkout: PathBuf,
    /// `file://` URL of the bare remote the checkout was cloned from.
    pub remote_url: String,
    /// The bare remote's directory base (for `push_commit`).
    pub remote_base: PathBuf,
    /// The commit the checkout was cloned at.
    pub head: String,
}

/// A fresh fixture with one project and one cloned repo.
pub(crate) fn fixture() -> Fx {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical temp root");
    let remote_base = root.join("remote-base");
    std::fs::create_dir_all(&remote_base).expect("remote base");
    let (_bare, remote_url, head) = bare_remote(
        &remote_base,
        &[
            ("src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n"),
            ("README.md", "# app\n"),
        ],
    );
    let work = root.join("work");
    let checkout = work.join("acme").join("app");
    clone_as(&remote_url, &checkout, "https://github.com/acme/app.git");

    // Every location deletion may touch is under `root`: nothing real can be reached.
    // Network git steps (re-fetch, probe) go to the local bare remote, never to GitHub.
    let state = AppState::from_data_dir(&root.join("data"))
        .with_storage_roots(StorageRoots::sandboxed(&root))
        .with_git_remotes(GitRemotes::default().with_url(REPO, &remote_url));
    state
        .credential_store
        .set(
            crate::credentials::GITHUB_TOKEN,
            "test-token-never-sent-anywhere",
        )
        .expect("memory credential store");
    state
        .settings
        .set_workspace_root(Some(work.to_string_lossy().into_owned()));
    let project = state
        .projects
        .create("Acme", vec![REPO.to_string()])
        .expect("project");
    // These tests exercise the Claude Code subscription path (session files, a CLI scan); a new
    // project defaults to the Api backend, so the fixture opts in explicitly. The subscription is
    // refused for a client engagement (`crate::client_lock`), so the fixture is also marked as
    // the maintainer's own project.
    state
        .projects
        .set_backend(&project.id, crate::project::ProjectBackend::Cli)
        .expect("project exists");
    let project = state
        .projects
        .set_client_engagement(&project.id, false, false)
        .expect("project exists");
    Fx {
        _tmp: tmp,
        root,
        state,
        project_id: project.id,
        checkout,
        remote_url,
        remote_base,
        head,
    }
}

impl Fx {
    /// Record a completed inspection of `REPO` at `sha` (what a scan's `set_last_scan` writes).
    pub(crate) fn inspect_at(&self, sha: &str) {
        self.state
            .set_last_scan(self.project_id.clone(), report_at(REPO, sha));
    }

    /// Add a second repo `owner/name` to the project, cloned under the workspace like the
    /// first. Returns its checkout.
    pub(crate) fn add_repo(&self, repo: &str) -> PathBuf {
        let base = self.root.join(format!("remote-{}", repo.replace('/', "-")));
        std::fs::create_dir_all(&base).expect("remote base");
        let (_bare, url, _sha) = bare_remote(&base, &[("lib.rs", "pub fn f() {}\n")]);
        let checkout = self.root.join("work").join(repo);
        clone_as(&url, &checkout, &format!("https://github.com/{repo}.git"));
        self.state
            .projects
            .update(&self.project_id, |p| p.repos.push(repo.to_string()))
            .expect("project exists");
        checkout
    }

    /// Make GitHub "have" everything committed so far, as a push would: point the remote-tracking
    /// ref of `main` at HEAD. (The checkout's `origin` URL is a GitHub one nothing can push to.)
    pub(crate) fn mark_head_pushed(&self) {
        crate::test_git::git(
            &self.checkout,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
    }

    /// The sandboxed storage roots this fixture's state was built with.
    pub(crate) fn roots(&self) -> StorageRoots {
        StorageRoots::sandboxed(&self.root)
    }

    /// A second, independent state over the same data directory: a restart.
    pub(crate) fn restarted(&self) -> AppState {
        let restarted = AppState::from_data_dir(&self.root.join("data"))
            .with_storage_roots(StorageRoots::sandboxed(&self.root))
            .with_git_remotes(GitRemotes::default().with_url(REPO, &self.remote_url));
        restarted
            .settings
            .set_workspace_root(Some(self.root.join("work").to_string_lossy().into_owned()));
        restarted
    }
}

/// A real (non-gated) report that audited `repo` at `sha`.
pub(crate) fn report_at(repo: &str, sha: &str) -> ScanReport {
    let mut r = ScanReport::gated(&[repo.to_string()]);
    r.gated = false;
    r.message = None;
    r.provenance.audited_refs = vec![AuditedRef {
        repo: repo.to_string(),
        sha: Some(sha.to_string()),
        branch: Some("main".to_string()),
        origin_url: Some(format!("https://github.com/{repo}.git")),
        default_branch: Some("main".to_string()),
        dirty: false,
    }];
    r.provenance.started_at = "2026-10-24T10:00:00Z".to_string();
    r.provenance.finished_at = "2026-10-24T10:05:00Z".to_string();
    r.provenance.camerata_version = "9.9.9".to_string();
    r
}

/// Total number of files under `dir` (recursive); 0 when it does not exist.
pub(crate) fn count_files(dir: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    read.filter_map(Result::ok)
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                count_files(&p)
            } else {
                1
            }
        })
        .sum()
}

/// The canned `claude -p` stream-json result line for an answer object.
pub(crate) fn result_line(answer: &serde_json::Value) -> String {
    serde_json::json!({
        "type": "result",
        "is_error": false,
        "result": answer.to_string(),
        "total_cost_usd": 0.001,
        "usage": { "input_tokens": 10, "output_tokens": 5 },
    })
    .to_string()
}

/// Install a fake `claude` in `dir` (put `dir` first on PATH). It appends every argument of
/// every call to `prompts.log` in `dir` and answers the fix-specific pass with `fix`, any call
/// whose arguments contain `trigger` with `finding`, and everything else with an empty (but
/// well-formed) result.
pub(crate) fn install_fake_claude(
    dir: &Path,
    trigger: &str,
    finding: &serde_json::Value,
    fix: &serde_json::Value,
) {
    use std::os::unix::fs::PermissionsExt;
    let empty = serde_json::json!({
        "findings": [], "proposed_rules": [], "needs_files": [], "verdicts": [],
        "fixes": [], "recommendations": []
    });
    std::fs::write(dir.join("empty.json"), result_line(&empty)).unwrap();
    std::fs::write(dir.join("with-finding.json"), result_line(finding)).unwrap();
    std::fs::write(dir.join("fix.json"), result_line(fix)).unwrap();
    let script = format!(
        "#!/bin/sh\nD=\"$(dirname \"$0\")\"\nprintf '%s\\n' \"$@\" >> \"$D/prompts.log\"\ncase \"$*\" in\n  *'codebase-specific fix per finding'*) cat \"$D/fix.json\" ;;\n  *{trigger}*) cat \"$D/with-finding.json\" ;;\n  *) cat \"$D/empty.json\" ;;\nesac\n"
    );
    let bin = dir.join("claude");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}
