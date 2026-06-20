//! The Workspace surface: the local-checkout control panel.
//!
//! This is where the "run it locally before you push" loop is operated. The architect
//! picks a visible workspace folder once; the active project's repos clone under it at
//! `<workspace>/<owner>/<repo>`. From here you clone/update the repos, see each working
//! copy's branch + dirty state, start a working branch, and ship (push + open a PR).
//!
//! Issue #37 adds a full git panel per repo: branch list (switch/create), commit log,
//! commit-all, push, pull, and cherry-pick (both via drag-and-drop and per-commit button).

use dioxus::prelude::*;

/// App settings as the BFF reports them (`/api/settings`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct SettingsView {
    #[serde(default)]
    workspace_root: Option<String>,
}

/// The active project (minimal shape: id / name / repos).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct ProjectLite {
    id: String,
    name: String,
    #[serde(default)]
    repos: Vec<String>,
}

/// One repo's local checkout state (`/api/projects/:id/checkout`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RepoCheckout {
    repo: String,
    cloned: bool,
    path: String,
    branch: Option<String>,
    dirty: bool,
    detail: String,
}

/// One commit from the git log panel.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct CommitRow {
    sha: String,
    short: String,
    subject: String,
    author: String,
    date: String,
}

/// Branch list response from `/api/projects/:id/git/branches`.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct BranchListView {
    #[serde(default)]
    current: String,
    #[serde(default)]
    branches: Vec<String>,
}

/// Full git status from `/api/projects/:id/git/status`: branch, dirty flag,
/// ahead/behind counts, and a human-readable detail string.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct GitStatusView {
    #[serde(default)]
    branch: String,
    #[serde(default)]
    dirty: bool,
    #[serde(default)]
    ahead: Option<u32>,
    #[serde(default)]
    behind: Option<u32>,
    #[serde(default)]
    detail: String,
}

// ── BFF fetch helpers ─────────────────────────────────────────────────────────

async fn fetch_settings() -> Option<SettingsView> {
    reqwest::get(format!("{}/api/settings", crate::BFF_URL))
        .await
        .ok()?
        .json::<SettingsView>()
        .await
        .ok()
}

async fn set_workspace(path: &str) -> Option<SettingsView> {
    reqwest::Client::new()
        .post(format!("{}/api/settings/workspace", crate::BFF_URL))
        .json(&serde_json::json!({ "path": path }))
        .send()
        .await
        .ok()?
        .json::<SettingsView>()
        .await
        .ok()
}

async fn fetch_active_project() -> Option<ProjectLite> {
    // The endpoint returns `null` when no project exists yet; Option parses that.
    reqwest::get(format!("{}/api/projects/active", crate::BFF_URL))
        .await
        .ok()?
        .json::<Option<ProjectLite>>()
        .await
        .ok()
        .flatten()
}

async fn fetch_checkout(project_id: &str) -> Option<Vec<RepoCheckout>> {
    reqwest::get(format!(
        "{}/api/projects/{}/checkout",
        crate::BFF_URL,
        project_id
    ))
    .await
    .ok()?
    .json::<Vec<RepoCheckout>>()
    .await
    .ok()
}

async fn clone_project(project_id: &str) -> Option<Vec<RepoCheckout>> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/checkout",
            crate::BFF_URL,
            project_id
        ))
        .send()
        .await
        .ok()?
        .json::<Vec<RepoCheckout>>()
        .await
        .ok()
}

async fn start_branch(project_id: &str, repo: &str, branch: &str) -> Option<RepoCheckout> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/branch",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "branch": branch }))
        .send()
        .await
        .ok()?
        .json::<RepoCheckout>()
        .await
        .ok()
}

/// Returns the PR URL on success.
async fn ship(project_id: &str, repo: &str, branch: &str, title: &str) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/ship",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "branch": branch, "title": title, "body": "" }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("pr_url").and_then(|u| u.as_str()).map(String::from)
}

// ── Git panel API calls (issue #37) ──────────────────────────────────────────

/// Minimal percent-encode for `owner/repo` paths in query strings (encodes `/` as `%2F`).
fn urlencoding_simple(s: &str) -> String {
    s.replace('/', "%2F").replace(' ', "%20")
}

/// Fetch the full git status (branch + dirty + ahead/behind) for `repo`.
/// Returns `None` when the repo is not locally resolved or git fails.
async fn api_git_status(project_id: &str, repo: &str) -> Option<GitStatusView> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/git/status?repo={}",
        crate::BFF_URL,
        project_id,
        urlencoding_simple(repo),
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    if v.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        serde_json::from_value(v).ok()
    } else {
        None
    }
}

async fn api_git_branches(project_id: &str, repo: &str) -> Option<BranchListView> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/git/branches?repo={}",
        crate::BFF_URL,
        project_id,
        urlencoding_simple(repo),
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    if v.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        serde_json::from_value(v).ok()
    } else {
        None
    }
}

async fn api_git_log(project_id: &str, repo: &str, limit: usize) -> Vec<CommitRow> {
    let v: serde_json::Value = match reqwest::get(format!(
        "{}/api/projects/{}/git/log?repo={}&limit={}",
        crate::BFF_URL,
        project_id,
        urlencoding_simple(repo),
        limit,
    ))
    .await
    .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return Vec::new(),
    };
    v.get("commits")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default()
}

async fn api_git_checkout(
    project_id: &str,
    repo: &str,
    branch: &str,
    create: bool,
) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/git/checkout",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "branch": branch, "create": create }))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return (false, "network error".to_string()),
    };
    let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let msg = v
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    (ok, msg)
}

async fn api_git_commit(project_id: &str, repo: &str, message: &str) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/git/commit",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "message": message }))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return (false, "network error".to_string()),
    };
    let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let out = v
        .get("output")
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    (ok, out)
}

async fn api_git_push(project_id: &str, repo: &str, branch: &str) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/git/push",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "branch": branch }))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return (false, "network error".to_string()),
    };
    let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let msg = v
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    (ok, msg)
}

async fn api_git_pull(project_id: &str, repo: &str, branch: &str) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/git/pull",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "branch": branch }))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return (false, "network error".to_string()),
    };
    let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let out = v
        .get("output")
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    (ok, out)
}

async fn api_git_cherry_pick(project_id: &str, repo: &str, sha: &str) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/git/cherry-pick",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "repo": repo, "sha": sha }))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return (false, "network error".to_string()),
    };
    let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let out = v
        .get("output")
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    (ok, out)
}

// ── Top-level view ────────────────────────────────────────────────────────────

#[component]
pub fn WorkspaceView() -> Element {
    let mut refresh = use_signal(|| 0u32);
    let settings_res = use_resource(move || {
        let _dep = refresh();
        async move { fetch_settings().await }
    });
    let project_res = use_resource(move || {
        let _dep = refresh();
        async move { fetch_active_project().await }
    });

    let settings = settings_res.read().clone().flatten();
    let workspace_root = settings.and_then(|s| s.workspace_root);
    let project = project_res.read().clone().flatten();

    // Checkout status depends on both a workspace + a project; refetched on `refresh`.
    let checkout_res = use_resource(move || {
        let _dep = refresh();
        let pid = project_res.read().clone().flatten().map(|p| p.id);
        async move {
            match pid {
                Some(id) => fetch_checkout(&id).await,
                None => None,
            }
        }
    });
    let checkouts = checkout_res.read().clone().flatten().unwrap_or_default();

    let busy = use_signal(|| false);

    rsx! {
        div { class: "page page-wide",
            p { class: "eyebrow", "Local" }
            h1 { class: "h1", "Repository Workspace" }
            p { class: "lede",
                "Your repos are cloned into a folder you can open and run. The governed fleet edits these local working copies on a branch; you run and test them here; then you ship a branch (push + open a PR). Nothing pushes on its own."
            }

            // ── Workspace folder picker ──────────────────────────────────────
            div { class: "ws-folder",
                p { class: "section-label", "Workspace folder" }
                match &workspace_root {
                    Some(path) => rsx! {
                        div { class: "ws-folder-row",
                            span { class: "ws-path", "{path}" }
                            button {
                                class: "btn-edit-sm",
                                onclick: move |_| {
                                    spawn(async move {
                                        if let Some(folder) = rfd::AsyncFileDialog::new()
                                            .set_title("Choose workspace folder")
                                            .pick_folder()
                                            .await
                                        {
                                            let p = folder.path().to_string_lossy().to_string();
                                            if set_workspace(&p).await.is_some() {
                                                refresh += 1;
                                            }
                                        }
                                    });
                                },
                                "Change…"
                            }
                        }
                    },
                    None => rsx! {
                        div { class: "ws-folder-row",
                            span { class: "ws-path none", "No workspace folder chosen yet." }
                            button {
                                class: "btn-run",
                                onclick: move |_| {
                                    spawn(async move {
                                        if let Some(folder) = rfd::AsyncFileDialog::new()
                                            .set_title("Choose workspace folder")
                                            .pick_folder()
                                            .await
                                        {
                                            let p = folder.path().to_string_lossy().to_string();
                                            if set_workspace(&p).await.is_some() {
                                                refresh += 1;
                                            }
                                        }
                                    });
                                },
                                "Choose folder…"
                            }
                        }
                    },
                }
            }

            // ── Project + repo checkouts ─────────────────────────────────────
            match (&workspace_root, &project) {
                (None, _) => rsx! {
                    p { class: "ws-hint", "Pick a workspace folder above to start cloning repos locally." }
                },
                (Some(_), None) => rsx! {
                    p { class: "ws-hint", "No active project. Create a project (and add its repos) first, then come back to clone them here." }
                },
                (Some(_), Some(proj)) => {
                    let pid = proj.id.clone();
                    rsx! {
                        div { class: "ws-project",
                            div { class: "ws-project-head",
                                div {
                                    p { class: "section-label", "Project — {proj.name}" }
                                    p { class: "ws-hint", "{proj.repos.len()} repo(s) in scope." }
                                }
                                button {
                                    class: "btn-run",
                                    disabled: busy(),
                                    onclick: move |_| {
                                        let id = pid.clone();
                                        let mut busy = busy;
                                        busy.set(true);
                                        spawn(async move {
                                            let _ = clone_project(&id).await;
                                            busy.set(false);
                                            refresh += 1;
                                        });
                                    },
                                    if busy() { "Cloning…" } else { "Clone / update all repos" }
                                }
                            }

                            if proj.repos.is_empty() {
                                p { class: "ws-hint", "This project has no repos yet. Add repos to it (via onboarding) to clone them." }
                            }
                            for repo in proj.repos.iter() {
                                {
                                    let status = checkouts.iter().find(|c| &c.repo == repo).cloned();
                                    let project_id = proj.id.clone();
                                    rsx! {
                                        RepoCard { key: "{repo}", repo: repo.clone(), project_id, status }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── RepoCard ──────────────────────────────────────────────────────────────────

#[component]
fn RepoCard(repo: String, project_id: String, status: Option<RepoCheckout>) -> Element {
    let mut branch = use_signal(|| "camerata/work".to_string());
    let mut title = use_signal(|| "Camerata: changes".to_string());
    let mut pr_url = use_signal(String::new);
    let mut msg = use_signal(String::new);
    let mut working = use_signal(|| false);

    let cloned = status.as_ref().map(|s| s.cloned).unwrap_or(false);
    let detail = status
        .as_ref()
        .map(|s| s.detail.clone())
        .unwrap_or_else(|| "not cloned yet".to_string());
    let dirty = status.as_ref().map(|s| s.dirty).unwrap_or(false);
    let path = status.as_ref().map(|s| s.path.clone()).unwrap_or_default();

    rsx! {
        div { class: "ws-repo",
            div { class: "ws-repo-head",
                span { class: "ws-repo-name", "{repo}" }
                span {
                    class: if cloned { "ws-repo-state cloned" } else { "ws-repo-state" },
                    "{detail}"
                }
            }
            if !path.is_empty() {
                p { class: "ws-repo-path", "{path}" }
            }
            if cloned {
                div { class: "ws-repo-actions",
                    label { class: "sched-field",
                        span { "Branch" }
                        input {
                            class: "addressee-input ws-branch",
                            value: "{branch}",
                            oninput: move |e| branch.set(e.value()),
                        }
                    }
                    button {
                        class: "btn-edit-sm",
                        disabled: working(),
                        onclick: {
                            let (pid, rp) = (project_id.clone(), repo.clone());
                            move |_| {
                                let (pid, rp, br) = (pid.clone(), rp.clone(), branch());
                                working.set(true);
                                spawn(async move {
                                    match start_branch(&pid, &rp, &br).await {
                                        Some(_) => msg.set(format!("on branch {br}")),
                                        None => msg.set("could not switch branch".to_string()),
                                    }
                                    working.set(false);
                                });
                            }
                        },
                        "Start branch"
                    }
                    label { class: "sched-field ws-title-field",
                        span { "PR title" }
                        input {
                            class: "addressee-input",
                            value: "{title}",
                            oninput: move |e| title.set(e.value()),
                        }
                    }
                    button {
                        class: "btn-run",
                        disabled: working(),
                        onclick: {
                            let (pid, rp) = (project_id.clone(), repo.clone());
                            move |_| {
                                let (pid, rp, br, ti) = (pid.clone(), rp.clone(), branch(), title());
                                working.set(true);
                                pr_url.set(String::new());
                                msg.set(String::new());
                                spawn(async move {
                                    match ship(&pid, &rp, &br, &ti).await {
                                        Some(url) if !url.is_empty() => pr_url.set(url),
                                        _ => msg.set("ship failed — check the branch has commits and the token can push".to_string()),
                                    }
                                    working.set(false);
                                });
                            }
                        },
                        if working() { "Working…" } else { "Ship (push + PR)" }
                    }
                }
                if dirty {
                    p { class: "ws-repo-dirty", "Uncommitted changes in the working copy — commit them before shipping." }
                }
                if !pr_url().is_empty() {
                    p { class: "ws-repo-pr",
                        "Opened PR: "
                        a { href: "{pr_url}", "{pr_url}" }
                    }
                }
                if !msg().is_empty() {
                    p { class: "ws-repo-msg", "{msg}" }
                }

                // ── Git panel (issue #37) ─────────────────────────────────
                GitPanel {
                    repo: repo.clone(),
                    project_id: project_id.clone(),
                }
            } else {
                p { class: "ws-hint", "Not cloned. Use \"Clone / update all repos\" above to create the local working copy." }
            }
        }
    }
}

// ── GitPanel ──────────────────────────────────────────────────────────────────

/// The full local git control panel for one repo. Embedded inside RepoCard when cloned.
/// Provides branch list, commit log, commit-all, push, pull, and cherry-pick.
#[component]
fn GitPanel(repo: String, project_id: String) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // ── refresh counter (bumped after any mutating git op) ────────────────
    let mut git_refresh = use_signal(|| 0u32);

    // Branch panel state
    let mut new_branch_input = use_signal(String::new);
    let mut branch_working = use_signal(|| false);

    // Commit panel state
    let mut commit_msg = use_signal(String::new);
    let mut commit_working = use_signal(|| false);

    // Push / pull state
    let mut net_working = use_signal(|| false);

    // Drag-and-drop: stash the SHA of the row being dragged
    let mut dragged_sha = use_signal(String::new);

    // ── data fetches ─────────────────────────────────────────────────────────
    let pid_s = project_id.clone();
    let rp_s = repo.clone();
    let status_res = use_resource(move || {
        let _dep = git_refresh();
        let pid = pid_s.clone();
        let rp = rp_s.clone();
        async move { api_git_status(&pid, &rp).await }
    });

    let pid_b = project_id.clone();
    let rp_b = repo.clone();
    let branches_res = use_resource(move || {
        let _dep = git_refresh();
        let pid = pid_b.clone();
        let rp = rp_b.clone();
        async move { api_git_branches(&pid, &rp).await }
    });

    let pid_l = project_id.clone();
    let rp_l = repo.clone();
    let log_res = use_resource(move || {
        let _dep = git_refresh();
        let pid = pid_l.clone();
        let rp = rp_l.clone();
        async move { api_git_log(&pid, &rp, 30).await }
    });

    let git_status = status_res.read().clone().flatten();
    let branch_list = branches_res.read().clone().flatten().unwrap_or_default();
    let commits: Vec<CommitRow> = log_res
        .read()
        .as_ref()
        .map(|v| v.clone())
        .unwrap_or_default();
    let current_branch = branch_list.current.clone();

    rsx! {
        div { class: "git-panel",
            // ── Status bar (branch · ahead/behind · dirty) ────────────────
            {
                let status_detail = git_status.as_ref().map(|s| s.detail.clone()).unwrap_or_default();
                let is_dirty = git_status.as_ref().map(|s| s.dirty).unwrap_or(false);
                let ahead = git_status.as_ref().and_then(|s| s.ahead);
                let behind = git_status.as_ref().and_then(|s| s.behind);
                let has_status = !status_detail.is_empty();
                rsx! {
                    if has_status {
                        div { class: "git-status-bar",
                            span { class: "git-status-detail", "{status_detail}" }
                            if is_dirty {
                                span { class: "git-status-badge git-status-dirty", "dirty" }
                            }
                            if ahead == Some(0) && behind == Some(0) {
                                span { class: "git-status-badge git-status-sync", "in sync" }
                            } else {
                                if let Some(a) = ahead {
                                    if a > 0 {
                                        span { class: "git-status-badge git-status-ahead", "{a} ahead" }
                                    }
                                }
                                if let Some(b) = behind {
                                    if b > 0 {
                                        span { class: "git-status-badge git-status-behind", "{b} behind" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Branch list ───────────────────────────────────────────────
            div { class: "git-section",
                p { class: "git-section-label", "Branches" }

                // Existing branches: click to switch; each is also a drop target for cherry-pick
                div { class: "git-branch-list",
                    for br in branch_list.branches.iter() {
                        {
                            let br_name = br.clone();
                            let is_current = *br == current_branch;
                            let pid_sw = project_id.clone();
                            let rp_sw = repo.clone();
                            rsx! {
                                div {
                                    key: "{br_name}",
                                    class: if is_current { "git-branch current" } else { "git-branch" },
                                    // Drop target: cherry-pick the dragged commit onto the current branch
                                    ondragover: move |evt| { evt.prevent_default(); },
                                    ondrop: {
                                        let pid = pid_sw.clone();
                                        let rp = rp_sw.clone();
                                        move |evt| {
                                            evt.prevent_default();
                                            let sha = dragged_sha();
                                            if sha.is_empty() { return; }
                                            let pid = pid.clone();
                                            let rp = rp.clone();
                                            spawn(async move {
                                                let (ok, out) = api_git_cherry_pick(&pid, &rp, &sha).await;
                                                if ok {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Cherry-picked {sha} onto current branch."));
                                                    git_refresh += 1;
                                                } else {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Cherry-pick failed: {out}"));
                                                }
                                            });
                                        }
                                    },
                                    // Click to switch branches
                                    onclick: {
                                        let pid = pid_sw.clone();
                                        let rp = rp_sw.clone();
                                        let br2 = br_name.clone();
                                        move |_| {
                                            if is_current { return; }
                                            let pid = pid.clone();
                                            let rp = rp.clone();
                                            let br2 = br2.clone();
                                            branch_working.set(true);
                                            spawn(async move {
                                                let (ok, err_msg) = api_git_checkout(&pid, &rp, &br2, false).await;
                                                branch_working.set(false);
                                                if ok {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Switched to {br2}"));
                                                    git_refresh += 1;
                                                } else {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Switch failed: {err_msg}"));
                                                }
                                            });
                                        }
                                    },
                                    span { class: "git-branch-name", "{br_name}" }
                                    if is_current {
                                        span { class: "git-branch-current-mark", "HEAD" }
                                    }
                                }
                            }
                        }
                    }
                    if branch_list.branches.is_empty() {
                        p { class: "ws-hint", "No local branches (clone / update first)." }
                    }
                }

                // New branch input + create button
                div { class: "git-new-branch-row",
                    input {
                        class: "addressee-input git-new-branch-input",
                        placeholder: "new-branch-name",
                        value: "{new_branch_input}",
                        oninput: move |e| new_branch_input.set(e.value()),
                    }
                    button {
                        class: "btn-edit-sm",
                        disabled: branch_working() || new_branch_input().trim().is_empty(),
                        onclick: {
                            let pid = project_id.clone();
                            let rp = repo.clone();
                            move |_| {
                                let br = new_branch_input().trim().to_string();
                                if br.is_empty() { return; }
                                let pid = pid.clone();
                                let rp = rp.clone();
                                branch_working.set(true);
                                spawn(async move {
                                    let (ok, err_msg) = api_git_checkout(&pid, &rp, &br, true).await;
                                    branch_working.set(false);
                                    if ok {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Created and switched to {br}"));
                                        new_branch_input.set(String::new());
                                        git_refresh += 1;
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Create branch failed: {err_msg}"));
                                    }
                                });
                            }
                        },
                        if branch_working() { "Working…" } else { "New branch" }
                    }
                }
            }

            // ── Commit-all ────────────────────────────────────────────────
            div { class: "git-section",
                p { class: "git-section-label", "Commit" }
                div { class: "git-commit-row",
                    input {
                        class: "addressee-input git-commit-input",
                        placeholder: "Commit message",
                        value: "{commit_msg}",
                        oninput: move |e| commit_msg.set(e.value()),
                    }
                    button {
                        class: "btn-edit-sm",
                        disabled: commit_working() || commit_msg().trim().is_empty(),
                        onclick: {
                            let pid = project_id.clone();
                            let rp = repo.clone();
                            move |_| {
                                let msg_txt = commit_msg().trim().to_string();
                                if msg_txt.is_empty() { return; }
                                let pid = pid.clone();
                                let rp = rp.clone();
                                commit_working.set(true);
                                spawn(async move {
                                    let (ok, out) = api_git_commit(&pid, &rp, &msg_txt).await;
                                    commit_working.set(false);
                                    if ok {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Committed: {out}"));
                                        commit_msg.set(String::new());
                                        git_refresh += 1;
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Commit failed: {out}"));
                                    }
                                });
                            }
                        },
                        if commit_working() { "Committing…" } else { "Commit all" }
                    }
                }
            }

            // ── Push / Pull ───────────────────────────────────────────────
            div { class: "git-section git-net-row",
                p { class: "git-section-label", "Sync — {current_branch}" }
                div { class: "git-net-btns",
                    button {
                        class: "btn-edit-sm",
                        disabled: net_working() || current_branch.is_empty(),
                        onclick: {
                            let pid = project_id.clone();
                            let rp = repo.clone();
                            let br = current_branch.clone();
                            move |_| {
                                let pid = pid.clone();
                                let rp = rp.clone();
                                let br = br.clone();
                                net_working.set(true);
                                spawn(async move {
                                    let (ok, out) = api_git_pull(&pid, &rp, &br).await;
                                    net_working.set(false);
                                    if ok {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Pulled {br}: {out}"));
                                        git_refresh += 1;
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Pull failed: {out}"));
                                    }
                                });
                            }
                        },
                        if net_working() { "Working…" } else { "Pull" }
                    }
                    button {
                        class: "btn-run btn-run-sm",
                        disabled: net_working() || current_branch.is_empty(),
                        onclick: {
                            let pid = project_id.clone();
                            let rp = repo.clone();
                            let br = current_branch.clone();
                            move |_| {
                                let pid = pid.clone();
                                let rp = rp.clone();
                                let br = br.clone();
                                net_working.set(true);
                                spawn(async move {
                                    let (ok, out) = api_git_push(&pid, &rp, &br).await;
                                    net_working.set(false);
                                    if ok {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Pushed {br} to origin."));
                                        git_refresh += 1;
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Push failed: {out}"));
                                    }
                                });
                            }
                        },
                        if net_working() { "Working…" } else { "Push" }
                    }
                }
            }

            // ── Commit log ────────────────────────────────────────────────
            div { class: "git-section",
                p { class: "git-section-label",
                    "Recent commits"
                    span { class: "git-log-hint", " — drag a row onto a branch to cherry-pick it, or use the button" }
                }
                div { class: "git-log",
                    for commit in commits.iter() {
                        {
                            let sha = commit.sha.clone();
                            let short = commit.short.clone();
                            let subject = commit.subject.clone();
                            let author = commit.author.clone();
                            let date = commit.date.clone();
                            let pid_cp = project_id.clone();
                            let rp_cp = repo.clone();
                            let sha_drag = sha.clone();
                            let sha_btn = sha.clone();
                            rsx! {
                                div {
                                    key: "{sha}",
                                    class: "git-commit-row-log",
                                    // Draggable: stash SHA on drag start so drop targets can read it
                                    draggable: "true",
                                    ondragstart: {
                                        let sha_d = sha_drag.clone();
                                        move |_| { dragged_sha.set(sha_d.clone()); }
                                    },
                                    div { class: "git-commit-meta",
                                        span { class: "git-commit-short", "{short}" }
                                        span { class: "git-commit-date", "{date}" }
                                        span { class: "git-commit-author", "{author}" }
                                    }
                                    div { class: "git-commit-subject", "{subject}" }
                                    // Per-commit cherry-pick button: fallback when drag is
                                    // unavailable, and a convenience shortcut regardless
                                    button {
                                        class: "btn-edit-sm git-cherry-btn",
                                        title: "Cherry-pick {short} onto current branch",
                                        onclick: {
                                            let pid = pid_cp.clone();
                                            let rp = rp_cp.clone();
                                            let sha = sha_btn.clone();
                                            move |_| {
                                                let pid = pid.clone();
                                                let rp = rp.clone();
                                                let sha = sha.clone();
                                                spawn(async move {
                                                    let (ok, out) = api_git_cherry_pick(&pid, &rp, &sha).await;
                                                    if ok {
                                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Cherry-picked {sha}."));
                                                        git_refresh += 1;
                                                    } else {
                                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Cherry-pick conflict: {out}"));
                                                    }
                                                });
                                            }
                                        },
                                        "Cherry-pick"
                                    }
                                }
                            }
                        }
                    }
                    if commits.is_empty() {
                        p { class: "ws-hint", "No commits yet." }
                    }
                }
            }
        }
    }
}
