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
