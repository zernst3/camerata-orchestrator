//! `POST /api/apps` — Part 2 of the Rust-fullstack scaffolder
//! (`docs/plans/2026-07-09_product-owner-head-vibe-mode.md`): make `camerata-scaffold`
//! (Part 1) invokable, and auto-create the GitHub repo for the scaffolded app.
//!
//! Flow ([`run_create_app`]):
//! 1. `camerata_scaffold::choose_strategy` decides `Skeleton` vs `FromScratch`.
//!    `FromScratch` returns the reason immediately — this module never fakes a
//!    from-scratch generator (that's the future orchestrator's job).
//! 2. `Skeleton`: resolve the authenticated GitHub login (via the SAME injected HTTP
//!    seam used for repo creation — see below — so this stays testable), scaffold
//!    the skeleton into `<workspace_root>/<login>/<package_name>` (the same
//!    `<owner>/<repo>` nesting `crate::workspace::repo_dir` uses for every other
//!    project repo, so this app's checkout is discoverable by the existing
//!    clone/update tooling), create a private GitHub repo, push the initial commit,
//!    then register a Camerata project for it — already marked onboarded, with the
//!    two invented rules (FIX B) seeded as real `CustomRule`s — and record a
//!    `governance_events` row.
//!
//! Two injectable seams keep this testable without touching real GitHub or a real
//! git process:
//! - the GitHub HTTP calls go through `camerata_worktracker::HttpTransport` (the same
//!   seam `github_issues.rs` uses): production supplies `ReqwestTransport`, tests
//!   supply `FakeTransport`.
//! - the git side effects (init/commit/push) go through [`RepoPusher`]: production
//!   supplies [`LiveRepoPusher`] (which shells out via `crate::workspace`'s helpers),
//!   tests supply a no-op double so no git process or network push ever runs in a
//!   test.

use std::path::Path;

use async_trait::async_trait;
use camerata_worktracker::HttpTransport;

// ── GitHub repo create + push ────────────────────────────────────────────────────

/// A newly created GitHub repo, as returned by [`create_and_push_repo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedRepo {
    /// `owner/repo`, as GitHub reports it back (the authoritative name).
    pub full_name: String,
    /// The human-navigable `https://github.com/owner/repo` URL.
    pub html_url: String,
}

/// The git side effects [`create_and_push_repo`] performs against an ALREADY-CREATED
/// remote: turn the scaffolded directory into a git repo, commit everything, and push
/// it. A seam separate from the GitHub HTTP call because these are real
/// subprocess/filesystem effects an HTTP fake can't intercept.
#[async_trait]
pub trait RepoPusher: Send + Sync {
    /// Initialize `local_dir` as a git repo (if it isn't already one), commit
    /// everything, point `origin` at `repo_full_name`'s tokenless URL, and push
    /// `main` using `token` for the transient authenticated push only.
    async fn init_commit_and_push(
        &self,
        local_dir: &Path,
        repo_full_name: &str,
        token: &str,
    ) -> anyhow::Result<()>;
}

/// The live git pusher: shells out to the system `git` via `crate::workspace`'s
/// helpers, mirroring that module's authed-URL-never-persisted-to-disk pattern (the
/// token is used only for the transient `git push` argument; `origin` is set to the
/// tokenless URL).
pub struct LiveRepoPusher;

#[async_trait]
impl RepoPusher for LiveRepoPusher {
    async fn init_commit_and_push(
        &self,
        local_dir: &Path,
        repo_full_name: &str,
        token: &str,
    ) -> anyhow::Result<()> {
        crate::workspace::init_repo_with_initial_commit(
            local_dir,
            "Initial scaffold (Camerata)",
        )
        .await?;
        crate::workspace::set_origin(local_dir, repo_full_name).await?;
        crate::workspace::push_branch(local_dir, repo_full_name, "main", token).await?;
        Ok(())
    }
}

/// Resolve the authenticated login for the token backing `transport` via
/// `GET https://api.github.com/user` — the same call `crate::connections` /
/// `crate::github_issues::get_authenticated_login` make, but routed through the
/// CALLER'S injected transport (rather than building its own `ReqwestTransport`) so
/// `run_create_app`'s single `HttpTransport` seam covers every GitHub call this flow
/// makes, and a test can script both with one `FakeTransport`.
async fn resolve_login(transport: &dyn HttpTransport) -> anyhow::Result<String> {
    let resp = transport.get("https://api.github.com/user").await?;
    if resp.status != 200 {
        anyhow::bail!(
            "GitHub /user lookup failed (status {}): {}",
            resp.status,
            resp.body
        );
    }
    let json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| anyhow::anyhow!("parse GitHub /user response: {e}"))?;
    json.get("login")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("GitHub /user response missing login"))
}

/// Create a new PRIVATE GitHub repo named `repo_name` under the authenticated user's
/// account, then push `local_dir`'s contents into it as the initial commit.
///
/// `transport` is the injectable GitHub HTTP seam: production supplies
/// `camerata_worktracker::ReqwestTransport`, tests supply `FakeTransport` scripted
/// with a 201 response for `POST https://api.github.com/user/repos`. `pusher` is the
/// injectable git seam (see [`RepoPusher`]).
pub async fn create_and_push_repo(
    transport: &dyn HttpTransport,
    pusher: &dyn RepoPusher,
    repo_name: &str,
    description: &str,
    local_dir: &Path,
    token: &str,
) -> anyhow::Result<CreatedRepo> {
    let body = serde_json::json!({
        "name": repo_name,
        "description": description,
        "private": true,
    })
    .to_string();

    let resp = transport
        .post("https://api.github.com/user/repos", &body)
        .await?;
    if resp.status != 201 {
        anyhow::bail!(
            "GitHub repo create failed (status {}): {}",
            resp.status,
            resp.body
        );
    }
    let json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| anyhow::anyhow!("parse GitHub repo-create response: {e}"))?;
    let full_name = json
        .get("full_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("GitHub repo-create response missing full_name"))?
        .to_string();
    let html_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    pusher
        .init_commit_and_push(local_dir, &full_name, token)
        .await?;

    Ok(CreatedRepo { full_name, html_url })
}

// ── Orchestration: scaffold + create repo + register the project ───────────────────

/// The display name to use everywhere a human-readable app name is needed (project
/// name, repo description fallback, governance-event reason) — mirrors
/// `scaffold_skeleton`'s own blank-name fallback so the two never disagree.
fn display_name(reqs: &camerata_scaffold::AppRequirements) -> String {
    if reqs.name.trim().is_empty() {
        "Camerata App".to_string()
    } else {
        reqs.name.clone()
    }
}

/// Run the full `POST /api/apps` flow for `reqs` against `state`, using `transport`
/// for every GitHub HTTP call and `pusher` for the git side effects. Returns the
/// exact JSON body the endpoint sends back on success; a plain `String` message on
/// any failure (fail-soft — the endpoint reports `{ ok: false, message }`, never a
/// 500, mirroring this crate's other best-effort endpoints).
pub async fn run_create_app(
    state: &crate::AppState,
    reqs: camerata_scaffold::AppRequirements,
    transport: &dyn HttpTransport,
    pusher: &dyn RepoPusher,
) -> Result<serde_json::Value, String> {
    match camerata_scaffold::choose_strategy(&reqs) {
        camerata_scaffold::ScaffoldStrategy::FromScratch { reason } => Ok(serde_json::json!({
            "ok": true,
            "strategy": "from_scratch",
            "reason": reason,
        })),
        camerata_scaffold::ScaffoldStrategy::Skeleton => {
            scaffold_and_register(state, reqs, transport, pusher)
                .await
                .map_err(|e| e.to_string())
        }
    }
}

async fn scaffold_and_register(
    state: &crate::AppState,
    reqs: camerata_scaffold::AppRequirements,
    transport: &dyn HttpTransport,
    pusher: &dyn RepoPusher,
) -> anyhow::Result<serde_json::Value> {
    let root = state.settings().workspace_root().ok_or_else(|| {
        anyhow::anyhow!(
            "no workspace folder is set. Pick a workspace folder first (Settings → Workspace), then try again."
        )
    })?;
    let token = state
        .github_token()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no GitHub token set. Add one in Settings → Credentials (or set CAMERATA_GITHUB_TOKEN), then try again."
            )
        })?;

    // Resolve the owner BEFORE scaffolding (not after repo creation) so the local
    // checkout lands at the standard `<root>/<owner>/<repo>` path
    // (`crate::workspace::repo_dir`) that every other project-repo tool
    // (checkout/clone/resolve) expects — a flat `<root>/<repo>` would leave this new
    // app's clone undiscoverable by the rest of the workspace machinery.
    let login = resolve_login(transport).await?;
    let package_name = reqs.package_name();
    let repo_slug = format!("{login}/{package_name}");
    let target_dir = crate::workspace::repo_dir(std::path::Path::new(&root), &repo_slug);

    camerata_scaffold::scaffold_skeleton(&reqs, &target_dir)
        .map_err(|e| anyhow::anyhow!("scaffold failed: {e}"))?;

    let name = display_name(&reqs);
    let description = if reqs.description.trim().is_empty() {
        format!("{name} — scaffolded by Camerata.")
    } else {
        reqs.description.clone()
    };

    let created = create_and_push_repo(
        transport,
        pusher,
        &package_name,
        &description,
        &target_dir,
        &token,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GitHub repo create/push failed: {e}"))?;

    // FIX B: the two invented rules are seeded as real CUSTOM rules (not corpus-style
    // rule IDs) — see `camerata_scaffold::default_custom_rules`'s doc comment.
    let custom_rules: Vec<crate::project::CustomRule> = camerata_scaffold::default_custom_rules()
        .into_iter()
        .map(|(rule_name, body)| crate::project::CustomRule {
            name: rule_name.to_string(),
            body: body.to_string(),
            domain: "*".to_string(),
            repos: Vec::new(),
        })
        .collect();

    let project = state
        .projects()
        .create(&name, vec![created.full_name.clone()])
        .ok_or_else(|| anyhow::anyhow!("could not create a Camerata project for the scaffolded app"))?;
    // A scaffolded app is already governed (it shipped with the vetted skeleton's own
    // AGENTS.md/CONVENTIONS.md/.camerata rules baked in) — mark it onboarded rather
    // than leaving it in the "not yet onboarded" state a normal brownfield repo starts
    // in.
    let project = state
        .projects()
        .update(&project.id, |p| {
            p.merge_custom(&custom_rules);
            p.mark_onboarded(std::slice::from_ref(&created.full_name));
        })
        .unwrap_or(project);

    state
        .record_governance(
            camerata_persistence::GovernanceEvent::info(
                project.id.clone(),
                "app_scaffolded",
                "human",
            )
            .with_reason(format!("scaffolded \"{name}\" into {}", created.full_name))
            .with_detail(created.html_url.clone()),
        )
        .await;

    Ok(serde_json::json!({
        "ok": true,
        "strategy": "skeleton",
        "path": target_dir.to_string_lossy(),
        "repo_url": created.html_url,
        "project_id": project.id,
    }))
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub struct NoopRepoPusher {
    pub calls: std::sync::Mutex<Vec<(std::path::PathBuf, String, String)>>,
}

#[cfg(test)]
impl NoopRepoPusher {
    pub fn new() -> Self {
        Self { calls: std::sync::Mutex::new(Vec::new()) }
    }
}

#[cfg(test)]
#[async_trait]
impl RepoPusher for NoopRepoPusher {
    async fn init_commit_and_push(
        &self,
        local_dir: &Path,
        repo_full_name: &str,
        token: &str,
    ) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push((
            local_dir.to_path_buf(),
            repo_full_name.to_string(),
            token.to_string(),
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::FakeTransport;

    fn github_transport() -> FakeTransport {
        FakeTransport::new()
            .on("GET", "api.github.com/user", 200, r#"{"login":"zernst3"}"#)
            .on(
                "POST",
                "api.github.com/user/repos",
                201,
                r#"{"full_name":"zernst3/trip-planner","html_url":"https://github.com/zernst3/trip-planner"}"#,
            )
    }

    #[tokio::test]
    async fn resolve_login_reads_the_login_field() {
        let transport = github_transport();
        let login = resolve_login(&transport).await.expect("resolve_login");
        assert_eq!(login, "zernst3");
    }

    #[tokio::test]
    async fn resolve_login_fails_on_non_200() {
        let transport = FakeTransport::new().on("GET", "api.github.com/user", 401, "{}");
        let err = resolve_login(&transport).await.unwrap_err();
        assert!(err.to_string().contains("401"), "error was: {err}");
    }

    #[tokio::test]
    async fn create_and_push_repo_returns_full_name_and_html_url() {
        let transport = github_transport();
        let pusher = NoopRepoPusher::new();
        let tmp = tempfile::tempdir().expect("tempdir");

        let created = create_and_push_repo(
            &transport,
            &pusher,
            "trip-planner",
            "A trip planner.",
            tmp.path(),
            "ghp_secret",
        )
        .await
        .expect("create_and_push_repo");

        assert_eq!(created.full_name, "zernst3/trip-planner");
        assert_eq!(created.html_url, "https://github.com/zernst3/trip-planner");

        // The pusher was invoked with the right args, and the HTTP call carried the
        // right repo name/description/visibility.
        let calls = pusher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, tmp.path());
        assert_eq!(calls[0].1, "zernst3/trip-planner");
        assert_eq!(calls[0].2, "ghp_secret");

        let recorded = transport.recorded_calls();
        let create_call = recorded
            .iter()
            .find(|(method, url, _)| method == "POST" && url.contains("/user/repos"))
            .expect("repo-create call recorded");
        let sent: serde_json::Value = serde_json::from_str(&create_call.2).unwrap();
        assert_eq!(sent["name"], "trip-planner");
        assert_eq!(sent["description"], "A trip planner.");
        assert_eq!(sent["private"], true);
    }

    #[tokio::test]
    async fn create_and_push_repo_fails_on_non_201() {
        let transport = FakeTransport::new().on(
            "POST",
            "api.github.com/user/repos",
            422,
            r#"{"message":"name already exists on this account"}"#,
        );
        let pusher = NoopRepoPusher::new();
        let tmp = tempfile::tempdir().expect("tempdir");

        let err = create_and_push_repo(
            &transport,
            &pusher,
            "trip-planner",
            "A trip planner.",
            tmp.path(),
            "ghp_secret",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("422"), "error was: {err}");
        assert!(
            pusher.calls.lock().unwrap().is_empty(),
            "pusher must not run when repo creation failed"
        );
    }

    // ── run_create_app: FromScratch short-circuits before any GitHub call ──────────

    #[tokio::test]
    async fn from_scratch_target_never_touches_github_or_disk() {
        let state = crate::AppState::seeded();
        let transport = FakeTransport::new(); // no scripts — any call 404s
        let pusher = NoopRepoPusher::new();
        let reqs = camerata_scaffold::AppRequirements {
            name: "Music Library".to_string(),
            target: camerata_scaffold::AppTarget::Desktop,
            ..Default::default()
        };

        let result = run_create_app(&state, reqs, &transport, &pusher).await.unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["strategy"], "from_scratch");
        assert!(result["reason"].as_str().unwrap().contains("Desktop"));
        assert!(transport.recorded_calls().is_empty());
        assert!(pusher.calls.lock().unwrap().is_empty());
    }

    // ── run_create_app: Skeleton error paths ────────────────────────────────────────

    #[tokio::test]
    async fn skeleton_without_workspace_root_fails_clearly() {
        // A fresh `seeded()` state has no workspace root and no GitHub token; the
        // workspace-root check must fire FIRST (before any GitHub call), so this
        // proves the error names the workspace, not the token.
        let state = crate::AppState::seeded();
        let transport = github_transport();
        let pusher = NoopRepoPusher::new();
        let reqs = camerata_scaffold::AppRequirements {
            name: "Trip Planner".to_string(),
            ..Default::default()
        };

        let err = run_create_app(&state, reqs, &transport, &pusher).await.unwrap_err();
        assert!(err.contains("workspace"), "error was: {err}");
        assert!(transport.recorded_calls().is_empty());
    }

    #[tokio::test]
    async fn skeleton_without_github_token_fails_clearly() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let state = crate::AppState::seeded();
        state.settings().set_workspace_root(Some(
            std::env::temp_dir().to_string_lossy().into_owned(),
        ));
        let transport = github_transport();
        let pusher = NoopRepoPusher::new();
        let reqs = camerata_scaffold::AppRequirements {
            name: "Trip Planner".to_string(),
            ..Default::default()
        };

        let err = run_create_app(&state, reqs, &transport, &pusher).await.unwrap_err();
        assert!(err.contains("GitHub token"), "error was: {err}");
    }

    // ── run_create_app: Skeleton happy path (GitHub mocked, no real push) ──────────

    #[tokio::test]
    async fn skeleton_scaffolds_creates_repo_and_registers_onboarded_project() {
        let mut state = crate::AppState::seeded();
        state
            .credential_store
            .set(crate::credentials::GITHUB_TOKEN, "ghp_test_token")
            .expect("set token");
        state.governance_log = Some(std::sync::Arc::new(
            camerata_persistence::GovernanceLog::open_in_memory()
                .await
                .expect("in-memory governance log"),
        ));
        let tmp_workspace = tempfile::tempdir().expect("tempdir");
        state
            .settings()
            .set_workspace_root(Some(tmp_workspace.path().to_string_lossy().into_owned()));

        let transport = github_transport();
        let pusher = NoopRepoPusher::new();
        let reqs = camerata_scaffold::AppRequirements {
            name: "Trip Planner".to_string(),
            description: "Track flights and stays.".to_string(),
            summary: "an app that tracks my flights".to_string(),
            ..Default::default()
        };

        let result = run_create_app(&state, reqs, &transport, &pusher)
            .await
            .expect("skeleton flow must succeed");

        assert_eq!(result["ok"], true);
        assert_eq!(result["strategy"], "skeleton");
        assert_eq!(result["repo_url"], "https://github.com/zernst3/trip-planner");
        let project_id = result["project_id"].as_str().unwrap().to_string();

        // The scaffolded files actually landed on disk, nested under owner/repo.
        let expected_dir = tmp_workspace.path().join("zernst3").join("trip_planner");
        assert!(expected_dir.join("Cargo.toml").is_file());
        assert!(expected_dir.join("CONVENTIONS.md").is_file());

        // No real git process ran (the NoopRepoPusher recorded the call instead).
        let pusher_calls = pusher.calls.lock().unwrap();
        assert_eq!(pusher_calls.len(), 1);
        assert_eq!(pusher_calls[0].1, "zernst3/trip-planner");
        assert_eq!(pusher_calls[0].2, "ghp_test_token");

        // The project is registered, onboarded, and carries the two seeded custom rules.
        let project = state.projects().get(&project_id).expect("project registered");
        assert_eq!(project.repos, vec!["zernst3/trip-planner".to_string()]);
        assert_eq!(project.onboarded, vec!["zernst3/trip-planner".to_string()]);
        assert_eq!(project.ruleset.custom.len(), 2);
        let names: Vec<&str> = project.ruleset.custom.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"db-on-demand"));
        assert!(names.contains(&"pwa-auto-capture"));

        // A governance event was recorded for the scaffold.
        let events = state
            .governance_log
            .as_ref()
            .unwrap()
            .recent(10)
            .await
            .expect("recent governance events");
        assert!(events.iter().any(|e| e.kind == "app_scaffolded" && e.run_id == project_id));
    }
}
