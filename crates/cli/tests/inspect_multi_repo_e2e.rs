#![allow(clippy::unwrap_used)]
//! `camerata inspect --repo A --repo B` (M11): several repos are ONE project. One scan, one
//! report, one export, one incremental cache and one ledger, with each repo kept under its own
//! `--repo-name` even though both have a `src/index.ts`. Run end to end over two small repos
//! with a SCRIPTED model (no network, no spend).

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

struct Scripted {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
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

/// A tiny TypeScript repo whose only source file is `src/index.ts`.
fn stage_repo(dest: &Path, body: &str) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(
        dest.join("package.json"),
        r#"{"name":"svc","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(dest.join("src/index.ts"), body).unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "multi-e2e@camerata.local"]);
    g(&["config", "user.name", "Multi E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(a: &Path, b: &Path, export: &Path) -> InspectArgs {
    InspectArgs {
        repo: a.to_path_buf(),
        also_repos: vec![b.to_path_buf()],
        repo_names: vec!["acme/api".to_string(), "acme/web".to_string()],
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: true,
        model: None,
        calibration_model: None,
        fix_model: None,
        full: false,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        client_engagement: true,
        run_client_tooling: false,
        consensus: false,
    }
}

fn zip_entry(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn two_repos_with_the_same_path_are_one_project_one_report_one_cache() {
    if !camerata_test_env::typst_on_path_for(
        "two_repos_with_the_same_path_are_one_project_one_report_one_cache",
    ) {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state =
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
    let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");

    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("api");
    let b = root.path().join("web");
    stage_repo(&a, "export const apiName = 'api';\n");
    stage_repo(&b, "export const webName = 'web';\n");
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    let llm = Scripted {
        calls: AtomicUsize::new(0),
    };
    let outcome = run_inspect_pipeline_with(
        args(&a, &b, &export),
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|_| {},
        },
    )
    .await
    .expect("the two-repo inspection completes");

    // One export, one report: both repos are in it, under the names they were given.
    assert!(export.is_file());
    let readme = zip_entry(&export, "README.txt");
    let findings = zip_entry(&export, "findings.json");
    for name in ["acme/api", "acme/web"] {
        assert!(
            readme.contains(name) || findings.contains(name),
            "{name} must appear in the one report"
        );
    }

    // One ledger, at the project key, not one per repo.
    let ledger = outcome.ledger_path.clone();
    let ledger_name = ledger.file_name().unwrap().to_string_lossy().to_string();
    assert!(ledger_name.starts_with("project-2-"), "{ledger_name}");
    assert!(ledger.is_file());

    // One cache at the project key, holding BOTH repos under their own names even though each
    // has a `src/index.ts`.
    let caches: Vec<_> = std::fs::read_dir(state.path().join("camerata"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".cache.json"))
        .collect();
    assert_eq!(
        caches.len(),
        1,
        "one cache for the whole project: {caches:?}"
    );
    assert!(caches[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("project-2-"));
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&caches[0]).unwrap()).unwrap();
    let files = manifest["files"].as_object().unwrap();
    for name in ["acme/api", "acme/web"] {
        let paths = files
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the cache: {files:?}"))
            .as_object()
            .unwrap();
        assert!(paths.contains_key("src/index.ts"), "{name}: {paths:?}");
    }
    assert_ne!(
        files["acme/api"]["src/index.ts"], files["acme/web"]["src/index.ts"],
        "each repo's copy is fingerprinted on its own"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn several_repos_without_names_are_refused_before_anything_runs() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("api");
    let b = root.path().join("web");
    stage_repo(&a, "export const a = 1;\n");
    stage_repo(&b, "export const b = 2;\n");
    let mut unnamed = args(&a, &b, &root.path().join("o.zip"));
    unnamed.repo_names.clear();
    let llm = Scripted {
        calls: AtomicUsize::new(0),
    };
    let err = run_inspect_pipeline_with(
        unnamed,
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|_| panic!("no estimate before the arguments are valid"),
        },
    )
    .await
    .expect_err("each repo needs a name");
    assert!(err.to_string().contains("--repo-name"), "{err}");
    assert_eq!(
        llm.calls.load(Ordering::SeqCst),
        0,
        "no model call was made"
    );
}
