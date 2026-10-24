//! End to end: deleting a client's source leaves NOTHING of it on this machine except what is
//! kept on purpose, and everything that is kept keeps working.
//!
//! Two tests, both over the real scan pipeline (a fake `claude` stands in for the model) and the
//! real HTTP router, with every path inside a temp directory the test owns:
//!
//! - [`purge_completeness_no_client_file_remains_in_any_known_location`] plants two sentinel
//!   strings in the client's code, one on a line a finding legitimately quotes and one in an
//!   otherwise clean file, spreads copies through every location the design lists (scan caches,
//!   the CLI cache and ledger, Claude Code's session files, a UoW worktree), deletes, and then
//!   walks the whole sandbox.
//! - [`project_fully_usable_after_source_deletion_e2e`] deletes, restarts, and proves findings,
//!   review and exports still work, rescans are refused with the reason, and the code view says
//!   the source is deleted.
//!
//! These live next to the unit tests rather than under `crates/server/tests/` because the
//! fixture (`test_support`), the sandboxed storage roots and the job runner are crate-internal
//! on purpose: nothing outside the crate can aim a deletion at anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_test_env::EnvGuard;
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::records::PurgedKind;
use super::storage::{known_storage_locations, Treatment};
use super::test_support::{fixture, install_fake_claude, Fx, REPO};
use crate::scan_cache::state_dir::{ledger_path, manifest_cache_path};
use crate::test_git::{commit_all, git, write_file};
use crate::{start_audit_job, AuditJobOptions};

/// On a line a finding quotes: it is part of the delivered product and is KEPT.
const SENTINEL_FINDING: &str = "SENTINEL_FINDING_7c1e";
/// In an otherwise clean file: no finding quotes it, so it must vanish from every location.
const SENTINEL_SILENT: &str = "SENTINEL_SILENT_52ab";
const RULE: &str = "ARCH-CURRENCY-CONSISTENCY-1";

fn leak_ts() -> String {
    format!(
        "export function leakyTotal(order: Order): number {{\n  const {SENTINEL_FINDING} = order.subtotal + order.shippingCents;\n  return {SENTINEL_FINDING};\n}}\n"
    )
}

fn quiet_ts() -> String {
    format!(
        "// A clean module: nothing in it is a finding.\nexport const quietNote = \"{SENTINEL_SILENT}\";\nexport function quietSum(a: number, b: number): number {{\n  return a + b;\n}}\n"
    )
}

struct Rig {
    fx: Fx,
    tools: TempDir,
    _guards: Vec<EnvGuard>,
}

/// A project of one repo with a leaky file and a clean file committed, the rule selected, a fake
/// `claude` on PATH that flags the leaky file, and the CLI's state directory inside the sandbox.
fn rig() -> Rig {
    let tools = TempDir::new().unwrap();
    let finding = serde_json::json!({
        "findings": [{
            "path": "src/leak.ts",
            "line": 2,
            "severity": "high",
            "rule": RULE,
            "title": "currency mixed with a shipping amount",
            "code": format!("const {SENTINEL_FINDING} = order.subtotal + order.shippingCents;"),
            "detail": "the shipping amount is in another currency",
        }],
        "proposed_rules": [], "needs_files": []
    });
    let fix = serde_json::json!({
        "fixes": [{ "index": 0, "fix": "Convert the shipping amount into the order's currency before adding it to the subtotal." }]
    });
    install_fake_claude(tools.path(), SENTINEL_FINDING, &finding, &fix);
    let path = format!(
        "{}:{}",
        tools.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fx = fixture();
    let guards = vec![
        EnvGuard::set("PATH", path),
        EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
        EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1"),
        // The CLI's cache directory resolves inside the sandbox (`camerata/` is appended), which
        // is where the sandboxed storage roots expect it.
        EnvGuard::set("CAMERATA_STATE_DIR", fx.root.join("cli-state")),
    ];
    write_file(&fx.checkout, "package.json", "{\"private\":true}\n");
    write_file(&fx.checkout, "src/leak.ts", &leak_ts());
    write_file(&fx.checkout, "src/quiet.ts", &quiet_ts());
    commit_all(&fx.checkout, "typescript sources");
    // Pushed, so the guard's "work GitHub does not have" refusal stands aside.
    fx.mark_head_pushed();
    fx.state
        .projects
        .update(&fx.project_id, |p| {
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: RULE.to_string(),
                chosen_option: None,
                repos: Vec::new(),
                ..Default::default()
            });
        })
        .unwrap();
    fx.state.projects.set_active(&fx.project_id);
    Rig {
        fx,
        tools,
        _guards: guards,
    }
}

impl Rig {
    fn prompts(&self) -> String {
        std::fs::read_to_string(self.tools.path().join("prompts.log")).unwrap_or_default()
    }
}

/// Run the first inspection through the job path the scan button uses; returns the job id.
async fn inspect(fx: &Fx) -> String {
    let project = fx.state.projects.get(&fx.project_id).unwrap();
    let request = super::rescan::project_scan_request(&project, &[REPO.to_string()])
        .await
        .unwrap();
    let job = start_audit_job(
        &fx.state,
        request,
        AuditJobOptions {
            project_id: Some(fx.project_id.clone()),
            ..AuditJobOptions::default()
        },
    )
    .await;
    for _ in 0..1_200 {
        if let Some(j) = fx.state.jobs.get(&job) {
            if j.status != "running" {
                assert_eq!(j.status, "done");
                return job;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the inspection did not finish");
}

/// One HTTP round trip against a fresh router over `state`.
async fn call(
    state: &crate::AppState,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, Vec<u8>) {
    let resp = crate::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(300)])
        )
    })
}

/// Every regular file under `dir` whose bytes contain `needle`, as paths relative to `base`.
fn files_containing(base: &Path, dir: &Path, needle: &str, skip: &[PathBuf]) -> Vec<String> {
    let mut hits = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return hits;
    };
    for entry in read.filter_map(Result::ok) {
        let path = entry.path();
        if skip.iter().any(|s| path.starts_with(s)) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            hits.extend(files_containing(base, &path, needle, skip));
        } else if meta.is_file() {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
            {
                hits.push(
                    path.strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    hits.sort();
    hits
}

fn delete_request() -> DeleteRequest {
    DeleteRequest {
        confirm: REPO.to_string(),
        include_uncommitted: false,
    }
}

/// Where Claude Code would have persisted a session of `cwd`: `<projects>/<cwd with every "/"
/// and "." replaced by "-">/<id>.jsonl`.
fn session_file(projects_dir: &Path, cwd: &str, id: &str) -> PathBuf {
    let dir_name: String = cwd
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    projects_dir.join(dir_name).join(format!("{id}.jsonl"))
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn purge_completeness_no_client_file_remains_in_any_known_location() {
    let rig = rig();
    let fx = &rig.fx;
    let roots = fx.roots();
    let job_id = inspect(fx).await;

    // ── What the scan itself left behind ────────────────────────────────────────────────
    let entry = fx.state.inspections.latest(&fx.project_id).unwrap();
    assert!(
        entry
            .report
            .findings
            .iter()
            .any(|f| f.path == "src/leak.ts" && f.snippet.contains(SENTINEL_FINDING)),
        "the finding quotes the sentinel line: {:?}",
        entry.report.findings
    );
    assert!(
        !entry.model_sessions.is_empty(),
        "the CLI backend recorded the sessions it created"
    );
    let prompts = rig.prompts();
    assert!(
        prompts.contains(SENTINEL_SILENT),
        "the model was shown the clean file too, so its prompt text is client code"
    );

    // ── Plant what the CLI and Claude Code would have written ───────────────────────────
    // The server's scan cache already exists (the job wrote it). The CLI keeps the same kind of
    // manifest in its own directory under the key the shared function computes.
    let cli_cache = manifest_cache_path(&fx.checkout);
    std::fs::create_dir_all(cli_cache.parent().unwrap()).unwrap();
    let manifest = fx
        .state
        .scan_cache
        .get(&fx.project_id)
        .expect("the scan populated the server's incremental cache");
    std::fs::write(&cli_cache, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let cli_ledger = ledger_path(&fx.checkout);
    std::fs::write(&cli_ledger, b"{\"stages\":[],\"note\":\"counts only\"}").unwrap();
    let projects_dir = roots.claude_projects_dir.clone().unwrap();
    let mut session_files = Vec::new();
    for session in &entry.model_sessions {
        let file = session_file(&projects_dir, &session.cwd, &session.session_id);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Claude Code stores the whole prompt of a print-mode session.
        std::fs::write(&file, &prompts).unwrap();
        session_files.push(file);
    }
    // A session Camerata did not create: not ours to remove.
    let stranger = session_file(
        &projects_dir,
        "/some/other/project",
        "11111111-2222-4333-8444-555555555555",
    );
    std::fs::create_dir_all(stranger.parent().unwrap()).unwrap();
    std::fs::write(&stranger, b"someone else's conversation").unwrap();
    // The operator's review draft: findings with their excerpts, kept.
    fx.state.draft.save(
        &fx.project_id,
        serde_json::json!({ "scan": serde_json::to_value(&entry.report).unwrap() }),
    );
    // A UoW worktree inside the checkout.
    let worktree = fx.checkout.join(".camerata-worktrees").join("camerata-wt");
    git(
        &fx.checkout,
        &[
            "worktree",
            "add",
            "-b",
            "camerata/wt",
            worktree.to_str().unwrap(),
        ],
    );
    assert!(worktree.join("src/quiet.ts").exists());

    // ── The test is not vacuous: before deletion both sentinels are everywhere ──────────
    let skip = vec![fx.root.join("remote-base"), fx.root.join("remote-acme-app")];
    let before_silent = files_containing(&fx.root, &fx.root, SENTINEL_SILENT, &skip);
    assert!(
        before_silent
            .iter()
            .any(|p| p.starts_with("work/acme/app/src/quiet.ts")),
        "{before_silent:?}"
    );
    assert!(
        before_silent
            .iter()
            .any(|p| p.contains(".camerata-worktrees")),
        "{before_silent:?}"
    );
    assert!(
        before_silent.iter().any(|p| p.contains(".claude/projects")),
        "{before_silent:?}"
    );
    let before_finding = files_containing(&fx.root, &fx.root, SENTINEL_FINDING, &skip);
    for expected in [
        "data/scan-cache.json",
        "data/onboarding-draft.json",
        "cli-state/camerata",
    ] {
        assert!(
            before_finding.iter().any(|p| p.contains(expected)),
            "{expected} should hold the finding before deletion: {before_finding:?}"
        );
    }

    // ── Delete ──────────────────────────────────────────────────────────────────────────
    let outcome = delete_source(&fx.state, &fx.project_id, REPO, &delete_request())
        .await
        .expect("the guard passes on a clean, pushed checkout");
    let DeleteOutcome::Done {
        record, partial, ..
    } = outcome
    else {
        panic!("expected a deletion, got an already-deleted answer");
    };
    assert!(!partial, "every location cleared: {:?}", record.items);

    // ── Every purged location is gone ───────────────────────────────────────────────────
    assert!(!fx.checkout.exists(), "the checkout");
    assert!(!worktree.exists(), "the UoW worktree");
    assert!(!cli_cache.exists(), "the CLI scan cache");
    assert!(!cli_ledger.exists(), "the CLI ledger");
    for file in &session_files {
        assert!(!file.exists(), "recorded session file {}", file.display());
    }
    assert!(
        stranger.exists(),
        "a session Camerata did not create is untouched"
    );
    assert!(
        fx.state.transcripts.get(&job_id).is_empty(),
        "the in-memory transcript of the scan job"
    );
    let kinds: Vec<PurgedKind> = record.items.iter().map(|i| i.kind).collect();
    for kind in [
        PurgedKind::Checkout,
        PurgedKind::Worktree,
        PurgedKind::ServerScanCache,
        PurgedKind::CliScanCache,
        PurgedKind::CliLedger,
        PurgedKind::ModelSessionFile,
        PurgedKind::InMemory,
    ] {
        assert!(
            kinds.contains(&kind),
            "the record lists {kind:?}: {kinds:?}"
        );
    }

    // ── The walk: nothing of the clean file is anywhere ─────────────────────────────────
    let after_silent = files_containing(&fx.root, &fx.root, SENTINEL_SILENT, &skip);
    assert_eq!(
        after_silent,
        Vec::<String>::new(),
        "SENTINEL_SILENT survived deletion in these files"
    );

    // ── The finding's excerpt is kept, and ONLY where it is part of the product ─────────
    let after_finding = files_containing(&fx.root, &fx.root, SENTINEL_FINDING, &skip);
    let kept: Vec<&String> = after_finding.iter().collect();
    assert!(
        kept.iter().all(|p| {
            p.as_str() == "data/onboarding-draft.json" || p.starts_with("data/inspections/")
        }),
        "SENTINEL_FINDING may remain only in the draft and the inspection, found in {after_finding:?}"
    );
    assert!(
        after_finding
            .iter()
            .any(|p| p.starts_with("data/inspections/")),
        "the inspection keeps the excerpt: {after_finding:?}"
    );
    assert!(
        after_finding
            .iter()
            .any(|p| p == "data/onboarding-draft.json"),
        "the draft keeps the excerpt: {after_finding:?}"
    );

    // ── Each known location agrees with its stated treatment ────────────────────────────
    for location in known_storage_locations(&roots) {
        match location.treatment {
            Treatment::Purged => {
                for needle in [SENTINEL_SILENT, SENTINEL_FINDING] {
                    let hits = files_containing(&fx.root, &location.path, needle, &[]);
                    assert!(
                        hits.is_empty(),
                        "{} is a purged location but still holds {needle}: {hits:?}",
                        location.path.display()
                    );
                }
            }
            Treatment::Kept | Treatment::NoClientCode => {
                let hits = files_containing(&fx.root, &location.path, SENTINEL_SILENT, &[]);
                assert!(
                    hits.is_empty(),
                    "{} must never hold client code: {hits:?}",
                    location.path.display()
                );
            }
        }
    }
}

/// A scan that holds its HTTP request open (the synchronous audit) is still a scan: while it
/// reads the checkout, deleting that checkout is refused, and the claim is released when the
/// request ends.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_synchronous_audit_blocks_deletion_while_it_runs() {
    let rig = rig();
    let fx = &rig.fx;
    let pid = fx.project_id.clone();
    // A model that takes its time, so the request is observably in flight.
    let slow = "#!/bin/sh\nsleep 2\ncat \"$(dirname \"$0\")/empty.json\"\n";
    let claude = rig.tools.path().join("claude");
    std::fs::write(&claude, slow).unwrap();

    let state = fx.state.clone();
    let scan = tokio::spawn(async move {
        call(
            &state,
            "POST",
            "/api/onboard/audit",
            serde_json::json!({
                "repos": [REPO],
                "rules": [{ "id": RULE, "directive": "one currency", "repos": [] }],
                "run_ai_review": true,
                "run_deterministic": false,
            }),
        )
        .await
    });

    let mut running = false;
    for _ in 0..100 {
        if fx.state.source_ops.scan_running(&pid) {
            running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        running,
        "the synchronous audit registered as a running scan"
    );
    let refusal = delete_source(&fx.state, &pid, REPO, &delete_request())
        .await
        .err()
        .expect("deletion is refused while the audit reads the checkout");
    assert_eq!(refusal.code, "scan_in_progress");
    assert!(fx.checkout.exists(), "nothing was touched");

    let (status, _) = scan.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(
        !fx.state.source_ops.scan_running(&pid),
        "the claim is released when the request ends"
    );
}

/// The body of the function that starts at `signature` in `source`: up to the first line that is
/// exactly `}` (rustfmt puts a top-level function's closing brace in column zero).
fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} not found"));
    let end = source[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .unwrap_or_else(|| panic!("{signature} has no closing brace"));
    &source[start..end]
}

/// Everything before a file's inline `#[cfg(test)]` module.
fn production_part(source: &str) -> &str {
    source
        .find("\n#[cfg(test)]\nmod ")
        .map_or(source, |at| &source[..at])
}

/// Exports are rendered from the stored report alone: they must not know where a repository
/// lives, and must not read a file from one. This is what lets a project be packaged, again and
/// again, after the source is deleted, so a new dependency on the checkout fails here.
#[test]
fn exports_never_touch_resolve_repo_dir_or_read_the_repo() {
    const FORBIDDEN: [&str; 5] = [
        "resolve_repo_dir",
        "repo_path(",
        "read_local_repo_files",
        "tokio::fs::read(",
        "std::fs::read_to_string(",
    ];
    let lib = production_part(include_str!("../lib.rs"));
    let handlers: Vec<(String, &str)> = [
        "async fn export_product(",
        "async fn export_audit_report(",
        "async fn export_deep_report(",
    ]
    .into_iter()
    .map(|signature| (signature.to_string(), function_body(lib, signature)))
    .collect();
    let files: Vec<(String, &str)> = [
        ("report_export.rs", include_str!("../report_export.rs")),
        ("xlsx_export.rs", include_str!("../xlsx_export.rs")),
        ("onboard/report.rs", include_str!("../onboard/report.rs")),
        (
            "client_provenance.rs",
            include_str!("../client_provenance.rs"),
        ),
        ("release_gate.rs", include_str!("../release_gate.rs")),
    ]
    .into_iter()
    .map(|(name, source)| (name.to_string(), production_part(source)))
    .collect();

    // The one read these files make: Typst's own output in its temp directory (the compiled PDF),
    // not a file of any repository.
    const ALLOWED: [(&str, &str); 1] = [("report_export.rs", "tokio::fs::read(&pdf_path)")];

    // Anti-vacuity: the scan sees real code.
    assert!(handlers.iter().all(|(_, body)| body.lines().count() > 20));
    assert!(files.iter().all(|(_, body)| body.lines().count() > 20));

    let mut hits = Vec::new();
    for (name, body) in handlers.iter().chain(files.iter()) {
        for (n, line) in body.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//")
                || ALLOWED
                    .iter()
                    .any(|(file, allowed)| name == file && code.starts_with(allowed))
            {
                continue;
            }
            for token in FORBIDDEN {
                if line.contains(token) {
                    hits.push(format!("{name}:{}: {token}", n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "an export now depends on the repository being on disk:\n  {}",
        hits.join("\n  ")
    );
}

/// Unzip an in-memory archive into name -> bytes.
fn unzip(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a zip");
    let mut out = std::collections::BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        if file.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();
        out.insert(file.name().to_string(), data);
    }
    out
}

/// Whether `typst` (which compiles every PDF) is missing. A test that needs a PDF skips itself
/// with a message naming it, instead of returning silently; the assertions that need no PDF live
/// in tests of their own that always run.
fn typst_missing(test: &str) -> bool {
    let missing = std::process::Command::new("typst")
        .arg("--version")
        .output()
        .is_err();
    if missing {
        eprintln!("SKIPPED {test}: typst is not on PATH, so no PDF can be compiled");
    }
    missing
}

/// Delete `REPO` over HTTP exactly as the modal does; returns the record's `completed_at`.
async fn delete_over_http(fx: &Fx) -> String {
    let (status, body) = call(
        &fx.state,
        "POST",
        &format!(
            "/api/projects/{}/repos/acme%2Fapp/source/delete",
            fx.project_id
        ),
        serde_json::json!({ "confirm": REPO, "include_uncommitted": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        json(&body)["ok"],
        true,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(!fx.checkout.exists());
    json(&body)["record"]["completed_at"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn export_product_works_after_restart_and_records_package() {
    use sha2::{Digest, Sha256};
    if typst_missing("export_product_works_after_restart_and_records_package") {
        return;
    }
    let rig = rig();
    let fx = &rig.fx;
    let pid = fx.project_id.clone();
    inspect(fx).await;
    assert!(
        fx.state.inspections.latest(&pid).unwrap().package.is_none(),
        "nothing has been packaged yet"
    );

    // A restart: the in-memory last scan is gone, the persisted inspection is all there is.
    let state = fx.restarted();
    state.projects.set_active(&pid);
    let (status, zip_bytes) = call(
        &state,
        "POST",
        &format!("/api/projects/{pid}/product-export"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&zip_bytes)
    );

    // The package is on the inspection: what was delivered, provable without the zip.
    let package = state
        .inspections
        .latest(&pid)
        .unwrap()
        .package
        .expect("export_product records the package it delivered");
    let digest = Sha256::digest(&zip_bytes);
    let expected: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(package.zip_sha256, expected);
    assert!(!package.generated_at.is_empty());
    let entries = unzip(&zip_bytes);
    let manifest: serde_json::Value = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert_eq!(package.manifest_json, manifest, "the manifest as delivered");

    // And it survives another restart.
    let again = fx.restarted();
    assert_eq!(
        again
            .inspections
            .latest(&pid)
            .unwrap()
            .package
            .unwrap()
            .zip_sha256,
        expected
    );
}

/// Everything about a project that needs NO PDF keeps working after its source is deleted and
/// the server restarts: readiness, status, the draft, the findings, the code view, the refusals
/// and the Workspace card. (The exports are proven by the test below, which needs `typst`.)
#[tokio::test]
#[serial_test::serial(process_env)]
async fn project_fully_usable_after_source_deletion_e2e() {
    let rig = rig();
    let fx = &rig.fx;
    let pid = fx.project_id.clone();
    inspect(fx).await;
    // The operator's review draft, as the UI saves it while triaging.
    let scanned = fx.state.get_last_scan(&pid).unwrap();
    fx.state.draft.save(
        &pid,
        serde_json::json!({ "scan": serde_json::to_value(&scanned).unwrap() }),
    );

    let deleted_at = delete_over_http(fx).await;

    // ── Restart: a new state over the same data directory, nothing in memory ────────────
    let state = fx.restarted();
    state.projects.set_active(&pid);

    let (status, body) = call(
        &state,
        "GET",
        &format!("/api/projects/{pid}/readiness"),
        serde_json::json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let readiness = json(&body);
    assert_eq!(
        readiness["readiness"], "ready",
        "a deleted source is not an unlinked repo: {readiness}"
    );
    assert_eq!(readiness["repos"][0]["source_deleted"], true);

    let (_, body) = call(
        &state,
        "GET",
        &format!("/api/projects/{pid}/source"),
        serde_json::json!(null),
    )
    .await;
    let source = json(&body);
    assert_eq!(source["repos"][0]["state"], "deleted", "{source}");
    assert_eq!(source["repos"][0]["deleted_at"], deleted_at.as_str());

    let (_, body) = call(&state, "GET", "/api/onboard/draft", serde_json::json!(null)).await;
    let draft = json(&body);
    assert!(
        draft["scan"]["findings"]
            .as_array()
            .is_some_and(|f| f.iter().any(|f| f["path"] == "src/leak.ts")),
        "the draft still carries the findings and their excerpts: {draft}"
    );

    // The review loads from the persisted inspection.
    assert!(
        state
            .get_last_scan(&pid)
            .expect("the last scan survives the restart")
            .findings
            .iter()
            .any(|f| f.path == "src/leak.ts"),
        "findings are still there"
    );

    // The code view says why there is no code, with the date.
    let (status, body) = call(
        &state,
        "GET",
        "/api/onboard/finding-context?repo=acme%2Fapp&path=src%2Fleak.ts&line=2",
        serde_json::json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let context = json(&body);
    assert_eq!(context["status"], "source_deleted", "{context}");
    assert_eq!(context["deleted_at"], deleted_at.as_str());

    // Everything that needs the source is refused up front, with the reason.
    for (uri, body) in [
        (
            "/api/onboard/audit/start".to_string(),
            serde_json::json!({ "repos": [REPO], "rules": [] }),
        ),
        (
            "/api/onboard/apply".to_string(),
            serde_json::json!({ "repos": [REPO] }),
        ),
        (
            format!("/api/projects/{pid}/rescan-alternatives"),
            serde_json::json!({ "overrides": {} }),
        ),
        (
            format!("/api/projects/{pid}/emit-local"),
            serde_json::json!({}),
        ),
        (
            format!("/api/projects/{pid}/repos/acme%2Fapp/rescan-changes"),
            serde_json::json!({}),
        ),
    ] {
        let (status, body) = call(&state, "POST", &uri, body).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{uri}: {}",
            String::from_utf8_lossy(&body)
        );
        let refusal = json(&body);
        assert_eq!(refusal["code"], "source_deleted", "{uri}: {refusal}");
        assert!(
            refusal["message"].as_str().unwrap().contains("deleted"),
            "{uri}: {refusal}"
        );
    }

    // Suppressions: empty, flagged as deleted, no tree read.
    let (status, body) = call(
        &state,
        "GET",
        &format!("/api/projects/{pid}/suppressions"),
        serde_json::json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let suppressions = json(&body);
    assert_eq!(suppressions["source_deleted"], true);
    assert_eq!(suppressions["entries"], serde_json::json!([]));

    // The Workspace card reads "source deleted on ...".
    let (_, body) = call(
        &state,
        "GET",
        &format!("/api/projects/{pid}/checkout"),
        serde_json::json!(null),
    )
    .await;
    let checkout = json(&body);
    assert!(
        checkout[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("source deleted on "),
        "{checkout}"
    );
}

/// The exports regenerate, through the release gate and from the stored report alone, after the
/// source is deleted and the server restarts: the same findings dataset, and a PDF. Needs `typst`
/// (it says so when skipped).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn exports_regenerate_from_the_stored_report_after_source_deletion_e2e() {
    if typst_missing("exports_regenerate_from_the_stored_report_after_source_deletion_e2e") {
        return;
    }
    let rig = rig();
    let fx = &rig.fx;
    let pid = fx.project_id.clone();
    inspect(fx).await;

    // The package the client would receive, before anything is deleted.
    let (status, before_zip) = call(
        &fx.state,
        "POST",
        &format!("/api/projects/{pid}/product-export"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&before_zip)
    );
    let before = unzip(&before_zip);
    let before_findings = before
        .iter()
        .find(|(name, _)| name.ends_with("findings.json"))
        .map(|(_, bytes)| bytes.clone())
        .expect("the package carries findings.json");
    assert!(String::from_utf8_lossy(&before_findings).contains("src/leak.ts"));

    delete_over_http(fx).await;
    let state = fx.restarted();
    state.projects.set_active(&pid);

    let (status, after_zip) = call(
        &state,
        "POST",
        &format!("/api/projects/{pid}/product-export"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&after_zip)
    );
    let after = unzip(&after_zip);
    let after_findings = after
        .iter()
        .find(|(name, _)| name.ends_with("findings.json"))
        .map(|(_, bytes)| bytes.clone())
        .expect("the regenerated package carries findings.json");
    let strip_clock = |bytes: &[u8]| -> serde_json::Value {
        let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        if let Some(map) = value.as_object_mut() {
            map.remove("generated_at");
        }
        value
    };
    assert_eq!(
        strip_clock(&before_findings),
        strip_clock(&after_findings),
        "the findings dataset is the same before and after deletion"
    );
    let (status, pdf) = call(
        &state,
        "POST",
        &format!("/api/projects/{pid}/audit-report"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    assert!(pdf.starts_with(b"%PDF"));
}
