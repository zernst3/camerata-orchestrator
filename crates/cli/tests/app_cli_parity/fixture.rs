//! The neutral two-repo project, the settings both paths are given and the scripted model both
//! paths ask.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};

/// The project's two repos, in the order both paths list them.
pub(crate) const REPOS: [&str; 2] = ["acme/api", "acme/web"];
/// Excluded by agreement on both paths (matched against `<repo name>/<path>`).
pub(crate) const EXCLUSION: &str = "web/generated/**";
/// Who the inspection is for (synthetic), the same on both paths.
pub(crate) const CLIENT: &str = "Acme Test Client";
/// The scan and stage 1 models, pinned to the same ids on both paths. Each path has its own
/// fallback when none is set (the command line's library default, the app's project step model),
/// so "the same settings" names them.
pub(crate) const AUDIT_MODEL: &str = "claude-sonnet-5-5";
pub(crate) const CALIBRATION_MODEL: &str = "claude-opus-5-5";

/// The scripted model both paths ask: the group reviewer's stand-in reply where the request is
/// one of its own, else one AI finding in `acme/web` `src/app.ts` (a rule that is not adopted, so
/// it is filed under an `AI-` id, as a real model's would be) wherever the request shows that file,
/// and nothing anywhere else. The reply depends on the request only, so the order the calls arrive
/// in cannot change the result.
pub(crate) const AI_REPLY: &str = r#"{"findings":[{"rule":"ACME-GREETING-ESCAPE-1","severity":"medium","path":"src/app.ts","line":3,"code":"  return `Hello ${name}`;","title":"unescaped greeting","detail":"the name is placed in the greeting without escaping"}],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#;

pub(crate) const NO_FINDINGS: &str = r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#;

pub(crate) struct Scripted {
    pub(crate) calls: AtomicUsize,
}

impl Scripted {
    pub(crate) fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = camerata_server::ai_audit::group_review::stand_in_reply(
            req.system.as_deref(),
            &req.prompt,
        )
        .unwrap_or_else(|| {
            if req.prompt.contains("src/app.ts") {
                AI_REPLY.to_string()
            } else {
                NO_FINDINGS.to_string()
            }
        });
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── The fixture ──────────────────────────────────────────────────────────────────────────

pub(crate) fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        // A fixed author and date, so the commit is the same whoever runs the test and when.
        .env("GIT_AUTHOR_NAME", "Parity Fixture")
        .env("GIT_AUTHOR_EMAIL", "parity@example.test")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "Parity Fixture")
        .env("GIT_COMMITTER_EMAIL", "parity@example.test")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub(crate) fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

pub(crate) fn commit(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
}

/// A vendor token shape the deterministic floor flags, built at run time (never a real token).
pub(crate) fn planted_token() -> String {
    format!("ghp_{}", &"0123456789".repeat(4)[..36])
}

/// `<root>/work/acme/api` and `<root>/work/acme/web`: the app finds them under its workspace
/// folder, the command line is given the same two folders.
pub(crate) fn stage_project(root: &Path) -> [PathBuf; 2] {
    let api = root.join("work/acme/api");
    write(
        &api,
        "package.json",
        r#"{"name":"acme-api","version":"1.0.0"}"#,
    );
    write(
        &api,
        "src/index.ts",
        "export function total(a: number, b: number): number {\n  return a + b;\n}\n",
    );
    write(
        &api,
        "src/config.ts",
        &format!("export const deployToken = \"{}\";\n", planted_token()),
    );
    // Over the 1 MiB read limit and not excluded: left out of the inspection.
    write(&api, "assets/bundle.js", &"var row = 1;\n".repeat(90_000));
    commit(&api);

    let web = root.join("work/acme/web");
    write(
        &web,
        "package.json",
        r#"{"name":"acme-web","version":"1.0.0"}"#,
    );
    write(
        &web,
        "src/app.ts",
        "export const title = 'Acme';\nexport function greet(name: string): string {\n  return `Hello ${name}`;\n}\n",
    );
    // Excluded by agreement: never opened.
    write(
        &web,
        "generated/client.ts",
        "export const generatedClient = 'do not inspect';\n",
    );
    commit(&web);
    [api, web]
}

/// The engine's environment for both runs: the dependency audit and semgrep are off (they reach
/// the network), and each run has a state folder of its own inside the test's temp folder.
pub(crate) fn engine_env(state_dir: &Path) -> Vec<camerata_test_env::EnvGuard> {
    vec![
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state_dir),
        camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
        camerata_test_env::EnvGuard::set(camerata_server::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1"),
        // No real key reaches either path: the placeholder below is the only one.
        camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY"),
    ]
}
