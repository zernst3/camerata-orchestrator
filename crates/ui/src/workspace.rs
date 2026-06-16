//! The Workspace surface: the local-checkout control panel.
//!
//! This is where the "run it locally before you push" loop is operated. The architect
//! picks a visible workspace folder once; the active project's repos clone under it at
//! `<workspace>/<owner>/<repo>`. From here you clone/update the repos, see each working
//! copy's branch + dirty state, start a working branch, and ship (push + open a PR).
//! Repo CONTENTS live on disk; only the project pointers persist server-side.

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
    reqwest::get(format!("{}/api/projects/{}/checkout", crate::BFF_URL, project_id))
        .await
        .ok()?
        .json::<Vec<RepoCheckout>>()
        .await
        .ok()
}

async fn clone_project(project_id: &str) -> Option<Vec<RepoCheckout>> {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/checkout", crate::BFF_URL, project_id))
        .send()
        .await
        .ok()?
        .json::<Vec<RepoCheckout>>()
        .await
        .ok()
}

async fn start_branch(project_id: &str, repo: &str, branch: &str) -> Option<RepoCheckout> {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/branch", crate::BFF_URL, project_id))
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
        .post(format!("{}/api/projects/{}/ship", crate::BFF_URL, project_id))
        .json(&serde_json::json!({ "repo": repo, "branch": branch, "title": title, "body": "" }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("pr_url").and_then(|u| u.as_str()).map(String::from)
}

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
            h1 { class: "h1", "Workspace" }
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
            } else {
                p { class: "ws-hint", "Not cloned. Use “Clone / update all repos” above to create the local working copy." }
            }
        }
    }
}
