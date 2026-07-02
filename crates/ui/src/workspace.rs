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
    reqwest::get(format!("{}/api/settings", crate::bff_base()))
        .await
        .ok()?
        .json::<SettingsView>()
        .await
        .ok()
}

async fn set_workspace(path: &str) -> Option<SettingsView> {
    reqwest::Client::new()
        .post(format!("{}/api/settings/workspace", crate::bff_base()))
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
    reqwest::get(format!("{}/api/projects/active", crate::bff_base()))
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
        crate::bff_base(),
        project_id
    ))
    .await
    .ok()?
    .json::<Vec<RepoCheckout>>()
    .await
    .ok()
}

/// Public wrapper over the internal `clone_project` for the readiness gate's Clone path, which
/// reuses this exact checkout flow (do NOT reimplement cloning). Returns `true` when the checkout
/// call succeeded (a repo list came back).
pub async fn clone_project_public(project_id: &str) -> bool {
    clone_project(project_id).await.is_some()
}

async fn clone_project(project_id: &str) -> Option<Vec<RepoCheckout>> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/checkout",
            crate::bff_base(),
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
            crate::bff_base(),
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
            crate::bff_base(),
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
        crate::bff_base(),
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
        crate::bff_base(),
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
        crate::bff_base(),
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
            crate::bff_base(),
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
            crate::bff_base(),
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
            crate::bff_base(),
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
            crate::bff_base(),
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
            crate::bff_base(),
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

/// Export the active project as a JSON file via a native save dialog.
async fn export_project_json(id: &str, name: &str) -> bool {
    let Ok(resp) = reqwest::get(format!("{}/api/projects/{}/export", crate::bff_base(), id)).await
    else {
        return false;
    };
    let Ok(text) = resp.text().await else {
        return false;
    };
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let filename = format!("camerata-project-{slug}.json");
    match rfd::AsyncFileDialog::new()
        .set_file_name(&filename)
        .save_file()
        .await
    {
        Some(file) => file.write(text.as_bytes()).await.is_ok(),
        None => false,
    }
}

// ── RepoHealthPanel ───────────────────────────────────────────────────────────

/// Compact health summary bar: cloned count, dirty count, shown as color-coded pills.
/// Renders nothing when `checkouts` is empty (no project or no repos).
#[component]
fn RepoHealthPanel(checkouts: Vec<RepoCheckout>) -> Element {
    if checkouts.is_empty() {
        return rsx! {};
    }
    let total = checkouts.len();
    let cloned = checkouts.iter().filter(|c| c.cloned).count();
    let dirty = checkouts.iter().filter(|c| c.dirty).count();
    let clone_cls = if cloned == total {
        "ws-health-stat ok"
    } else {
        "ws-health-stat warn"
    };
    let dirty_cls = if dirty == 0 {
        "ws-health-stat ok"
    } else {
        "ws-health-stat bad"
    };
    rsx! {
        div { class: "ws-health",
            div { class: clone_cls,
                span { class: "ws-health-dot" }
                span { "{cloned}/{total} cloned" }
            }
            div { class: dirty_cls,
                span { class: "ws-health-dot" }
                span { if dirty == 0 { "all clean" } else { "{dirty} dirty" } }
            }
        }
    }
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

            // ── Repo health summary ──────────────────────────────────────────
            if !checkouts.is_empty() {
                RepoHealthPanel { checkouts: checkouts.clone() }
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
                                div { class: "ws-project-actions",
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
                                    {
                                        let export_id = proj.id.clone();
                                        let export_name = proj.name.clone();
                                        rsx! {
                                            button {
                                                class: "btn-edit-sm",
                                                title: "Export project config as JSON",
                                                onclick: move |_| {
                                                    let id = export_id.clone();
                                                    let name = export_name.clone();
                                                    spawn(async move {
                                                        export_project_json(&id, &name).await;
                                                    });
                                                },
                                                "Export JSON"
                                            }
                                        }
                                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure-logic: the query-string encoder used for every git/* request ─────────
    #[test]
    fn urlencoding_simple_encodes_slash_and_space() {
        assert_eq!(urlencoding_simple("zernst3/agora"), "zernst3%2Fagora");
        assert_eq!(urlencoding_simple("a b/c"), "a%20b%2Fc");
        // Nothing else is touched — a plain repo slug round-trips unchanged.
        assert_eq!(urlencoding_simple("plainrepo"), "plainrepo");
    }

    // ── Tier 2: network-helper tests (wiremock) ──────────────────────────────────
    // Each points the helper at a fake BFF via the CAMERATA_BFF_URL seam and asserts the
    // request CONTRACT (method + path + exact body for mutations). The env override is
    // process-global, so these must not run concurrently with each other; cargo runs the
    // tests in this module in-process and they each set+remove the var around one await.

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_settings_parses_workspace_root() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "workspace_root": "/tmp/ws" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_settings().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let out = out.expect("settings parse");
        assert_eq!(out.workspace_root.as_deref(), Some("/tmp/ws"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_workspace_posts_the_path() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/workspace"))
            .and(body_json(serde_json::json!({ "path": "/tmp/ws" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "workspace_root": "/tmp/ws" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::set_workspace("/tmp/ws").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(
            out.expect("settings echo").workspace_root.as_deref(),
            Some("/tmp/ws")
        );
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_active_project_parses_null_as_none() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(null)))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_active_project().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(out.is_none(), "a null body means no active project");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_active_project_parses_a_project() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "proj-7",
                "name": "Acme",
                "repos": ["zernst3/agora"],
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_active_project().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let proj = out.expect("project parsed");
        assert_eq!(proj.id, "proj-7");
        assert_eq!(proj.name, "Acme");
        assert_eq!(proj.repos, vec!["zernst3/agora".to_string()]);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_checkout_hits_project_scoped_path() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/checkout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "repo": "zernst3/agora",
                "cloned": true,
                "path": "/tmp/ws/zernst3/agora",
                "branch": "main",
                "dirty": false,
                "detail": "on main",
            }])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_checkout("proj-7").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let rows = out.expect("checkout list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "zernst3/agora");
        assert!(rows[0].cloned);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn clone_project_posts_to_checkout() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/checkout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "repo": "zernst3/agora",
                "cloned": true,
                "path": "/tmp/ws/zernst3/agora",
                "branch": "main",
                "dirty": false,
                "detail": "cloned",
            }])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::clone_project("proj-7").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let rows = out.expect("clone result");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].detail, "cloned");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn start_branch_posts_repo_and_branch() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/branch"))
            .and(body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "branch": "camerata/work",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "cloned": true,
                "path": "/tmp/ws/zernst3/agora",
                "branch": "camerata/work",
                "dirty": false,
                "detail": "on branch camerata/work",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::start_branch("proj-7", "zernst3/agora", "camerata/work").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(out.expect("checkout").branch.as_deref(), Some("camerata/work"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn ship_posts_full_body_and_returns_pr_url() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/ship"))
            .and(body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "branch": "camerata/work",
                "title": "Camerata: changes",
                "body": "",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "pr_url": "https://github.com/zernst3/agora/pull/42",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::ship("proj-7", "zernst3/agora", "camerata/work", "Camerata: changes").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(
            out.as_deref(),
            Some("https://github.com/zernst3/agora/pull/42")
        );
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_status_requires_ok_true_and_encodes_repo() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // wiremock decodes the query param, so the matcher sees the raw "owner/repo".
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/git/status"))
            .and(query_param("repo", "zernst3/agora"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "branch": "main",
                "dirty": true,
                "ahead": 2,
                "behind": 0,
                "detail": "ahead 2",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::api_git_status("proj-7", "zernst3/agora").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let st = out.expect("status parsed when ok=true");
        assert_eq!(st.branch, "main");
        assert!(st.dirty);
        assert_eq!(st.ahead, Some(2));
        assert_eq!(st.behind, Some(0));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_status_returns_none_when_not_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/git/status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": false })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::api_git_status("proj-7", "zernst3/agora").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(out.is_none(), "ok=false collapses to None");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_branches_parses_current_and_list() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "current": "main",
                "branches": ["main", "camerata/work"],
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::api_git_branches("proj-7", "zernst3/agora").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let bl = out.expect("branches parsed");
        assert_eq!(bl.current, "main");
        assert_eq!(bl.branches, vec!["main".to_string(), "camerata/work".to_string()]);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_log_extracts_commits_array() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/git/log"))
            .and(query_param("limit", "30"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "commits": [{
                    "sha": "abcdef0123",
                    "short": "abcdef0",
                    "subject": "Initial commit",
                    "author": "Zach",
                    "date": "2026-06-30",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::api_git_log("proj-7", "zernst3/agora", 30).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sha, "abcdef0123");
        assert_eq!(out[0].subject, "Initial commit");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_checkout_posts_create_flag_and_returns_ok_message() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/git/checkout"))
            .and(body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "branch": "feature/x",
                "create": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "message": "created feature/x",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, msg) = super::api_git_checkout("proj-7", "zernst3/agora", "feature/x", true).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
        assert_eq!(msg, "created feature/x");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_commit_posts_message_and_reads_output() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/git/commit"))
            .and(body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "message": "wip",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "output": "1 file changed",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, out) = super::api_git_commit("proj-7", "zernst3/agora", "wip").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
        assert_eq!(out, "1 file changed");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_push_posts_branch() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/git/push"))
            .and(body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "branch": "camerata/work",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, _msg) = super::api_git_push("proj-7", "zernst3/agora", "camerata/work").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_pull_reports_failure_message() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/git/pull"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "merge conflict",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, out) = super::api_git_pull("proj-7", "zernst3/agora", "main").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!ok);
        assert_eq!(out, "merge conflict");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_git_cherry_pick_posts_sha() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/git/cherry-pick"))
            .and(body_json(serde_json::json!({
                "repo": "zernst3/agora",
                "sha": "abcdef0123",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "output": "applied",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, out) = super::api_git_cherry_pick("proj-7", "zernst3/agora", "abcdef0123").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
        assert_eq!(out, "applied");
    }

    // ── Tier 1: render tests (dioxus-ssr) ────────────────────────────────────────

    #[test]
    fn repo_health_panel_all_cloned_and_clean() {
        fn harness() -> Element {
            rsx! {
                RepoHealthPanel {
                    checkouts: vec![
                        super::RepoCheckout {
                            repo: "acme/alpha".to_string(),
                            cloned: true,
                            path: "/ws/acme/alpha".to_string(),
                            branch: Some("main".to_string()),
                            dirty: false,
                            detail: String::new(),
                        },
                        super::RepoCheckout {
                            repo: "acme/beta".to_string(),
                            cloned: true,
                            path: "/ws/acme/beta".to_string(),
                            branch: Some("main".to_string()),
                            dirty: false,
                            detail: String::new(),
                        },
                    ],
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("2/2 cloned"), "cloned pill shows 2/2");
        assert!(html.contains("all clean"), "dirty pill shows all clean");
        assert!(html.contains("ws-health-stat ok"), "ok class present");
    }

    #[test]
    fn repo_health_panel_partially_cloned_and_dirty() {
        fn harness() -> Element {
            rsx! {
                RepoHealthPanel {
                    checkouts: vec![
                        super::RepoCheckout {
                            repo: "acme/alpha".to_string(),
                            cloned: true,
                            path: "/ws/acme/alpha".to_string(),
                            branch: Some("main".to_string()),
                            dirty: true,
                            detail: String::new(),
                        },
                        super::RepoCheckout {
                            repo: "acme/beta".to_string(),
                            cloned: false,
                            path: String::new(),
                            branch: None,
                            dirty: false,
                            detail: "not cloned".to_string(),
                        },
                    ],
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("1/2 cloned"), "cloned pill shows 1/2");
        assert!(html.contains("1 dirty"), "dirty pill shows 1");
        assert!(html.contains("ws-health-stat warn"), "warn class for partial clone");
        assert!(html.contains("ws-health-stat bad"), "bad class for dirty");
    }

    #[test]
    fn repo_health_panel_empty_renders_nothing() {
        fn harness() -> Element {
            rsx! { RepoHealthPanel { checkouts: vec![] } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("ws-health"),
            "empty checkouts renders nothing"
        );
    }

    // RepoCard with status=None renders the "not cloned" branch, which does NOT mount
    // GitPanel (that only renders when cloned) and issues no fetches — so it's cleanly
    // renderable in isolation with only props.
    #[test]
    fn repo_card_uncloned_renders_repo_name_and_hint() {
        fn harness() -> Element {
            rsx! {
                RepoCard {
                    repo: "zernst3/agora".to_string(),
                    project_id: "proj-7".to_string(),
                    status: None,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("zernst3/agora"), "repo name renders");
        assert!(html.contains("not cloned yet"), "uncloned detail renders");
        assert!(html.contains("Not cloned"), "the not-cloned hint renders");
    }

    // GitPanel uses use_context::<Signal<Vec<Toast>>> and three use_resource fetches.
    // The fetches are pending on first SSR render (so the loading/empty branches show),
    // but the static section scaffolding (labels, buttons, placeholders) renders. The
    // harness MUST provide the toast context or the component panics.
    #[test]
    fn git_panel_renders_static_section_scaffold() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            rsx! {
                GitPanel {
                    repo: "zernst3/agora".to_string(),
                    project_id: "proj-7".to_string(),
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("git-panel"), "panel root renders");
        assert!(html.contains("Branches"), "branches section label renders");
        assert!(html.contains("Commit"), "commit section label renders");
        assert!(html.contains("Recent commits"), "log section label renders");
        // Resources are pending on first render → the empty-state branches show.
        assert!(
            html.contains("No local branches (clone / update first)."),
            "empty branch list hint renders while branches fetch is pending"
        );
        assert!(
            html.contains("No commits yet."),
            "empty log hint renders while log fetch is pending"
        );
    }
}
