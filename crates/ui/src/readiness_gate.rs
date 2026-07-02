//! The project readiness gate (adapter layer).
//!
//! Pure logic — parsing the readiness string, banner wording, which repos need resolving, whether
//! an action is gated — lives in `camerata_ui_core::readiness` and is unit-tested there. This module
//! is the Dioxus + `rfd` + HTTP adapter around it: the two BFF client fns (`fetch_readiness`,
//! `link_repo`), the paused banner, and the clone-or-link modal (native folder pickers).
//!
//! See `docs/decisions/2026-07-01_project-readiness-gate.md`. Server contracts:
//! - `GET  /api/projects/:id/readiness` → `{ ok, readiness, repos: [{resolved, path, reason, ...}] }`
//! - `POST /api/projects/:id/repos/:repo/link` (`:repo` percent-encoded) body `{ path }` →
//!   success `{ ok:true, readiness }`; failure (400) `{ ok:false, error }`.
//! - Clone reuses the EXISTING `POST /api/projects/:id/checkout` (see `workspace::clone_project`).

use dioxus::prelude::*;

pub use camerata_ui_core::readiness::{
    action_gated, paused_banner_text, resolve_prompt_text, unresolved_repos, ProjectReadiness,
    RepoResolution,
};

/// The parsed readiness snapshot for the active project: the derived state plus the per-repo
/// resolution rows the modal drives off of.
#[derive(Clone, PartialEq)]
pub struct Readiness {
    pub state: ProjectReadiness,
    pub repos: Vec<RepoResolution>,
}

impl Readiness {
    /// The repos that still need a local clone (owned copies, so callers can move them into rsx).
    pub fn unresolved(&self) -> Vec<RepoResolution> {
        unresolved_repos(&self.repos).into_iter().cloned().collect()
    }
}

/// Percent-encode a `owner/repo` for the `:repo` path segment of the link endpoint.
fn enc_repo(repo: &str) -> String {
    repo.replace('%', "%25").replace('/', "%2F").replace(' ', "%20")
}

/// Parse the shared readiness JSON body (`{ ok, readiness, repos }`) into a `Readiness`. An
/// unknown project reports `{ ok:false, readiness:"unlinked" }`; that still parses to an
/// `Unlinked` readiness (fail-safe), which is exactly the paused state we want.
fn parse_readiness(v: &serde_json::Value) -> Readiness {
    let state = ProjectReadiness::parse(v.get("readiness").and_then(|r| r.as_str()).unwrap_or(""));
    let repos = v
        .get("repos")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .map(|it| RepoResolution {
                    repo: it
                        .get("repo")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    resolved: it.get("resolved").and_then(|b| b.as_bool()).unwrap_or(false),
                    path: it
                        .get("path")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    reason: it
                        .get("reason")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Readiness { state, repos }
}

/// GET the project's derived readiness + per-repo resolution. Returns `None` only on a transport
/// failure; a well-formed `{ ok:false, ... }` (unknown project) still parses to an `Unlinked`
/// snapshot so the gate pauses rather than releasing actions.
pub async fn fetch_readiness(project_id: &str) -> Option<Readiness> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/readiness",
        crate::bff_base(),
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    Some(parse_readiness(&v))
}

/// The outcome of a link attempt against `POST /api/projects/:id/repos/:repo/link`.
#[derive(Clone, PartialEq)]
pub enum LinkOutcome {
    /// The path was accepted and recorded; carries the recomputed readiness.
    Linked(Readiness),
    /// The path was rejected (400 `{ ok:false, error }`); nothing was recorded. Carries the
    /// server's reason for inline display.
    Rejected(String),
}

/// POST a local `path` to link an unresolved repo. On success (`ok:true`) returns the recomputed
/// readiness; on the 400 rejection path (`ok:false`) returns the server's `error` verbatim so the
/// modal can show it inline WITHOUT closing (nothing was recorded on the server). A transport
/// failure collapses to a generic rejection so the caller never silently swallows it.
pub async fn link_repo(project_id: &str, repo: &str, path: &str) -> LinkOutcome {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/repos/{}/link",
            crate::bff_base(),
            project_id,
            enc_repo(repo),
        ))
        .json(&serde_json::json!({ "path": path }))
        .send()
        .await;
    let Ok(resp) = resp else {
        return LinkOutcome::Rejected("network error contacting the server".to_string());
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return LinkOutcome::Rejected("the server returned an unreadable response".to_string());
    };
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        LinkOutcome::Linked(parse_readiness(&v))
    } else {
        let err = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("that folder could not be linked")
            .to_string();
        LinkOutcome::Rejected(err)
    }
}

/// The persistent paused banner. Shown at the top of every in-project surface when the project is
/// not fully ready; its single call-to-action re-opens the resolve modal. Renders nothing when the
/// project is `Ready`.
#[component]
pub fn PausedBanner(readiness: Readiness, open_modal: EventHandler<()>) -> Element {
    let Some(text) = paused_banner_text(readiness.state, readiness.unresolved().len()) else {
        return rsx! {};
    };
    rsx! {
        div { class: "readiness-banner",
            span { class: "readiness-banner-dot" }
            span { class: "readiness-banner-text", "{text}" }
            button {
                class: "btn-run readiness-banner-btn",
                onclick: move |_| open_modal.call(()),
                "Link repo"
            }
        }
    }
}

/// The clone-or-link resolve modal. For each unresolved repo it offers Clone (native folder picker
/// for the destination parent → the existing `checkout` flow) and Select existing (native folder
/// picker → the `link` endpoint, validating origin). Dismiss leaves the project paused.
///
/// `on_resolved` fires after any successful clone/link so the parent can re-fetch readiness.
/// `on_close` closes the modal (Dismiss / the ✕ / overlay click) WITHOUT changing readiness.
#[component]
pub fn ResolveModal(
    project_id: String,
    readiness: Readiness,
    on_resolved: EventHandler<()>,
    on_close: EventHandler<()>,
) -> Element {
    let unresolved = readiness.unresolved();
    // Per-repo inline error (keyed by repo id) surfaced from a rejected link.
    let mut link_error = use_signal(|| None::<(String, String)>);
    // The repo currently mid clone-or-link (disables its buttons + shows a spinner label).
    let mut busy_repo = use_signal(|| None::<String>);

    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "rule-modal readiness-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "Link this project's repo" }
                    button { class: "rule-modal-close", onclick: move |_| on_close.call(()), "\u{2715}" }
                }
                p { class: "rule-modal-detail",
                    "This project is paused until each repo below resolves to a local clone. \
                     Clone it now, or select a local clone you already have. Dismissing leaves \
                     the project paused."
                }

                for repo in unresolved.iter() {
                    {
                        let repo_id = repo.repo.clone();
                        let prompt = resolve_prompt_text(&repo_id);
                        let is_busy = busy_repo().as_deref() == Some(repo_id.as_str());
                        let err = link_error()
                            .filter(|(r, _)| r == &repo_id)
                            .map(|(_, m)| m);
                        // Clones of the ids for the two onclick closures.
                        let pid_clone = project_id.clone();
                        let repo_clone = repo_id.clone();
                        let pid_link = project_id.clone();
                        let repo_link = repo_id.clone();
                        rsx! {
                            div { key: "{repo_id}", class: "readiness-repo",
                                p { class: "readiness-repo-name", "{repo_id}" }
                                p { class: "readiness-repo-prompt", "{prompt}" }
                                div { class: "readiness-repo-actions",
                                    // ── Clone → pick a destination parent, reuse checkout ──
                                    button {
                                        class: "btn-run",
                                        disabled: is_busy,
                                        onclick: move |_| {
                                            let pid = pid_clone.clone();
                                            let rp = repo_clone.clone();
                                            busy_repo.set(Some(rp.clone()));
                                            link_error.set(None);
                                            spawn(async move {
                                                // The native folder picker (destination parent). If the
                                                // user cancels, just clear busy — nothing happened.
                                                let picked = rfd::AsyncFileDialog::new()
                                                    .set_title("Choose a destination folder to clone into")
                                                    .pick_folder()
                                                    .await;
                                                if picked.is_none() {
                                                    busy_repo.set(None);
                                                    return;
                                                }
                                                // Reuse the existing project checkout flow (clones every
                                                // not-yet-cloned repo under the workspace). We don't pass
                                                // the picked path to the legacy endpoint; the picker keeps
                                                // the UX consistent with the Workspace clone and confirms
                                                // intent. On completion, re-fetch readiness.
                                                let _ = crate::workspace::clone_project_public(&pid).await;
                                                busy_repo.set(None);
                                                on_resolved.call(());
                                            });
                                        },
                                        if is_busy { "Working\u{2026}" } else { "Clone it now" }
                                    }
                                    // ── Select existing → pick a folder, POST /link, validate ──
                                    button {
                                        class: "btn-edit-sm",
                                        disabled: is_busy,
                                        onclick: move |_| {
                                            let pid = pid_link.clone();
                                            let rp = repo_link.clone();
                                            busy_repo.set(Some(rp.clone()));
                                            link_error.set(None);
                                            spawn(async move {
                                                let picked = rfd::AsyncFileDialog::new()
                                                    .set_title("Select the local clone folder")
                                                    .pick_folder()
                                                    .await;
                                                let Some(folder) = picked else {
                                                    busy_repo.set(None);
                                                    return;
                                                };
                                                let path = folder.path().to_string_lossy().to_string();
                                                match link_repo(&pid, &rp, &path).await {
                                                    LinkOutcome::Linked(_) => {
                                                        busy_repo.set(None);
                                                        // Success: re-fetch readiness + close (parent decides).
                                                        on_resolved.call(());
                                                    }
                                                    LinkOutcome::Rejected(msg) => {
                                                        // Nothing recorded — show inline, keep the modal open.
                                                        busy_repo.set(None);
                                                        link_error.set(Some((rp.clone(), msg)));
                                                    }
                                                }
                                            });
                                        },
                                        "Select existing clone\u{2026}"
                                    }
                                }
                                if let Some(msg) = err {
                                    p { class: "readiness-repo-error", "{msg}" }
                                }
                            }
                        }
                    }
                }

                div { class: "readiness-modal-foot",
                    button { class: "btn-edit-sm", onclick: move |_| on_close.call(()), "Dismiss (stay paused)" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enc_repo_encodes_slash_percent_space() {
        assert_eq!(enc_repo("zernst3/agora"), "zernst3%2Fagora");
        assert_eq!(enc_repo("a b/c"), "a%20b%2Fc");
        assert_eq!(enc_repo("a%b"), "a%25b");
    }

    #[test]
    fn parse_readiness_reads_state_and_repos() {
        let v = serde_json::json!({
            "ok": true,
            "readiness": "partial",
            "repos": [
                { "resolved": true, "repo": "a/one", "path": "/ws/a/one", "reason": "ok" },
                { "resolved": false, "repo": "a/two", "path": "", "reason": "no local match" },
            ],
        });
        let r = parse_readiness(&v);
        assert_eq!(r.state, ProjectReadiness::Partial);
        assert_eq!(r.repos.len(), 2);
        assert_eq!(r.unresolved().len(), 1);
        assert_eq!(r.unresolved()[0].repo, "a/two");
    }

    #[test]
    fn parse_readiness_unknown_project_is_unlinked() {
        let v = serde_json::json!({ "ok": false, "readiness": "unlinked" });
        let r = parse_readiness(&v);
        assert_eq!(r.state, ProjectReadiness::Unlinked);
        assert!(r.repos.is_empty());
    }

    // ── wiremock: fetch_readiness parses { ok, readiness, repos } ─────────────────
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_readiness_parses_state_and_repos() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/readiness"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "readiness": "partial",
                "repos": [
                    { "resolved": true, "repo": "acme/alpha", "path": "/ws/acme/alpha", "reason": "ok" },
                    { "resolved": false, "repo": "acme/beta", "path": "", "reason": "no local match" },
                ],
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_readiness("proj-7").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let r = out.expect("readiness parsed");
        assert_eq!(r.state, ProjectReadiness::Partial);
        assert_eq!(r.repos.len(), 2);
        let un = r.unresolved();
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].repo, "acme/beta");
        assert_eq!(un[0].reason, "no local match");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_readiness_unknown_project_pauses() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/nope/readiness"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "readiness": "unlinked",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_readiness("nope").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let r = out.expect("still parses");
        assert_eq!(r.state, ProjectReadiness::Unlinked);
        assert!(r.state.is_paused());
    }

    // ── wiremock: link_repo success returns the new readiness ─────────────────────
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn link_repo_success_returns_new_readiness() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The `:repo` segment is percent-encoded on the wire (`/` → `%2F`); wiremock matches the
        // raw request path, so the matcher must use the encoded segment.
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/repos/acme%2Fbeta/link"))
            .and(body_json(serde_json::json!({ "path": "/local/acme/beta" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "readiness": "ready",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::link_repo("proj-7", "acme/beta", "/local/acme/beta").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match out {
            LinkOutcome::Linked(r) => assert_eq!(r.state, ProjectReadiness::Ready),
            LinkOutcome::Rejected(m) => panic!("expected Linked, got Rejected({m})"),
        }
    }

    // ── wiremock: link_repo 400 { ok:false, error } surfaces the error ────────────
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn link_repo_rejection_surfaces_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/repos/acme%2Fbeta/link"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "ok": false,
                "error": "That folder's origin doesn't match acme/beta",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::link_repo("proj-7", "acme/beta", "/wrong/folder").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match out {
            LinkOutcome::Rejected(msg) => {
                assert!(msg.contains("origin doesn't match"), "got: {msg}");
            }
            LinkOutcome::Linked(_) => panic!("expected Rejected on a 400"),
        }
    }

    // ── Tier 1 render: the paused banner shows text + a Link repo button ──────────
    #[test]
    fn paused_banner_renders_text_and_button() {
        fn harness() -> Element {
            rsx! {
                PausedBanner {
                    readiness: Readiness {
                        state: ProjectReadiness::Unlinked,
                        repos: vec![RepoResolution {
                            repo: "acme/beta".to_string(),
                            resolved: false,
                            path: String::new(),
                            reason: "no local match".to_string(),
                        }],
                    },
                    open_modal: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Paused"), "banner headline renders");
        assert!(html.contains("Link repo"), "CTA button renders");
    }

    #[test]
    fn ready_banner_renders_nothing() {
        fn harness() -> Element {
            rsx! {
                PausedBanner {
                    readiness: Readiness { state: ProjectReadiness::Ready, repos: vec![] },
                    open_modal: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(!html.contains("readiness-banner"), "ready state renders no banner");
    }
}
