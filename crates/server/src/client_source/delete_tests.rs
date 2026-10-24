//! Deletion end to end at the function level: what is removed, what is kept, what the record
//! says. Every path is under the fixture's temp root; nothing real is reachable.

use std::path::{Path, PathBuf};

use super::delete::{delete_preview, delete_source, DeleteOutcome, DeleteRequest};
use super::records::{DeletionRecord, PurgeOutcome, PurgedKind};
use super::status::{source_status, RepoSourceStatus};
use super::test_support::{fixture, report_at, Fx, REPO};
use crate::llm::ModelSession;
use crate::scan_cache::state_dir::repo_state_files_in;
use crate::scan_cache::{ScanManifest, MANIFEST_VERSION};
use crate::test_git::{git, write_file};

const SESSION_A: &str = "6f1d2c3a-1111-4222-8333-444455556666";
const SESSION_B: &str = "9d1c0000-0000-4000-8000-000000000000";
const SESSION_UNRECORDED: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn req(include_uncommitted: bool) -> DeleteRequest {
    DeleteRequest {
        confirm: REPO.to_string(),
        include_uncommitted,
    }
}

async fn delete_ok(fx: &Fx) -> DeletionRecord {
    match delete_source(&fx.state, &fx.project_id, REPO, &req(false)).await {
        Ok(DeleteOutcome::Done { record, .. }) => *record,
        Ok(DeleteOutcome::AlreadyDeleted(_)) => panic!("already deleted"),
        Err(refusal) => panic!("refused: {refusal}"),
    }
}

fn manifest_for(repos: &[&str]) -> ScanManifest {
    let mut manifest = ScanManifest {
        version: MANIFEST_VERSION,
        ..ScanManifest::default()
    };
    for repo in repos {
        manifest.files.insert(
            (*repo).to_string(),
            std::iter::once(("a.rs".to_string(), "ff".to_string())).collect(),
        );
        manifest.findings.push(crate::onboard::Finding {
            repo: (*repo).to_string(),
            path: "a.rs".to_string(),
            snippet: format!("cached excerpt of {repo}"),
            ..crate::onboard::Finding::default()
        });
    }
    manifest
}

fn item(record: &DeletionRecord, kind: PurgedKind) -> Vec<&super::records::PurgedItem> {
    record.items.iter().filter(|i| i.kind == kind).collect()
}

fn write_claude_session(roots_claude: &Path, dir: &str, id: &str, body: &str) -> PathBuf {
    let folder = roots_claude.join(dir);
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join(format!("{id}.jsonl"));
    std::fs::write(&file, body).unwrap();
    file
}

#[tokio::test]
async fn delete_removes_checkout_and_writes_record_with_counts() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let before_files = super::test_support::count_files(&fx.checkout);
    assert!(before_files > 5, "the clone has .git objects too");

    let record = delete_ok(&fx).await;

    assert!(!fx.checkout.exists(), "the checkout is gone");
    let checkout = item(&record, PurgedKind::Checkout);
    assert_eq!(checkout.len(), 1);
    assert_eq!(checkout[0].outcome, PurgeOutcome::Removed);
    assert_eq!(checkout[0].files, before_files, ".git is counted too");
    assert!(checkout[0].bytes > 0);
    assert_eq!(record.repo, REPO);
    assert_eq!(record.path, fx.checkout.to_string_lossy());
    assert_eq!(record.confirmed_with, REPO);
    assert_eq!(record.inspected_sha.as_deref(), Some(fx.head.as_str()));
    assert_eq!(
        record.head_sha_at_deletion.as_deref(),
        Some(fx.head.as_str())
    );
    assert_eq!(
        record.origin_url.as_deref(),
        Some("https://github.com/acme/app.git")
    );
    assert!(record
        .inspection_id
        .as_deref()
        .unwrap()
        .starts_with("insp-"));
    assert!(record.total_files >= before_files);
    assert_eq!(record.method, super::records::METHOD_SENTENCE);
    assert!(record.residuals.iter().any(|r| r.contains("Time Machine")));
    assert!(!record.machine.is_empty());
    // The record is stored and the repo now reads as deleted.
    assert_eq!(fx.state.source_records.deletions(&fx.project_id).len(), 1);
    let status = source_status(&fx.state, &fx.project_id, REPO).await;
    assert!(
        matches!(status, RepoSourceStatus::Deleted { .. }),
        "{status:?}"
    );
}

#[tokio::test]
async fn delete_record_items_match_preview_plan() {
    let fx = fixture();
    let preview = axum::extract::State(fx.state.clone());
    let response = delete_preview(
        preview,
        axum::extract::Path((fx.project_id.clone(), REPO.to_string())),
    )
    .await;
    let bytes = axum::body::to_bytes(
        axum::response::IntoResponse::into_response(response).into_body(),
        usize::MAX,
    )
    .await
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["ok"], true);
    assert!(fx.checkout.exists(), "a preview removes nothing");
    let planned: Vec<super::records::PurgedItem> =
        serde_json::from_value(json["plan"]["items"].clone()).unwrap();
    assert!(planned.iter().all(|i| i.outcome == PurgeOutcome::Planned));

    let record = delete_ok(&fx).await;

    assert_eq!(record.items.len(), planned.len());
    for (done, plan) in record.items.iter().zip(&planned) {
        assert_eq!(done.kind, plan.kind);
        assert_eq!(done.path, plan.path);
        assert_eq!(done.files, plan.files, "{}", plan.path);
        assert_eq!(done.bytes, plan.bytes, "{}", plan.path);
        assert_ne!(done.outcome, PurgeOutcome::Planned);
    }
    let sum: u64 = planned.iter().map(|i| i.files).sum();
    assert_eq!(json["plan"]["total_files"], sum);
}

#[tokio::test]
async fn delete_removes_registered_worktree_inside_checkout() {
    let fx = fixture();
    let wt = fx
        .checkout
        .join(".camerata-worktrees")
        .join("camerata-work");
    git(
        &fx.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "camerata-work",
            &wt.to_string_lossy(),
        ],
    );
    write_file(&wt, "scratch/built.o", "object file\n");

    let record = delete_ok(&fx).await;

    assert!(!fx.checkout.exists() && !wt.exists());
    let worktrees = item(&record, PurgedKind::Worktree);
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].outcome, PurgeOutcome::Removed);
    assert!(worktrees[0].files >= 2);
    // No file is counted twice: the checkout item excludes the worktree's files.
    let checkout = item(&record, PurgedKind::Checkout)[0];
    let all = checkout.files + worktrees[0].files;
    assert_eq!(record.total_files, all);
}

#[tokio::test]
async fn delete_removes_registered_worktree_outside_checkout_and_prunes() {
    let fx = fixture();
    let wt = fx.root.join("elsewhere").join("side-wt");
    std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
    git(
        &fx.checkout,
        &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
    );
    write_file(&wt, "notes.txt", "client code lives here too\n");
    assert!(wt.exists());

    let record = delete_ok(&fx).await;

    assert!(!wt.exists(), "a worktree outside the checkout is removed");
    assert!(!fx.checkout.exists());
    let worktrees = item(&record, PurgedKind::Worktree);
    assert_eq!(worktrees.len(), 1);
    assert_eq!(
        worktrees[0].path,
        wt.canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| wt.to_string_lossy().into_owned())
    );
    // The worktree's parent folder, which Camerata did not create, is left alone.
    assert!(fx.root.join("elsewhere").exists());
}

/// H3, end to end: a worktree folder was removed by hand and its path reused for the operator's
/// own data. Git keeps listing the stale registration; the deletion must neither remove that
/// folder nor count it in the record.
#[tokio::test]
async fn a_reused_folder_of_a_stale_worktree_registration_is_not_deleted_or_counted() {
    let fx = fixture();
    let wt = fx.root.join("scratch").join("wt");
    std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
    git(
        &fx.checkout,
        &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
    );
    std::fs::remove_dir_all(&wt).unwrap();
    write_file(
        &wt,
        "precious/thesis.txt",
        "the operator's unrelated data\n",
    );

    let record = delete_ok(&fx).await;

    assert!(!fx.checkout.exists());
    assert!(
        wt.join("precious/thesis.txt").exists(),
        "the reused folder is not ours to remove"
    );
    assert!(
        item(&record, PurgedKind::Worktree).is_empty(),
        "and it is not in the record: {:?}",
        record.items
    );
    assert!(
        record.items.iter().all(|i| !i.path.contains("scratch")),
        "{:?}",
        record.items
    );
}

/// H2, end to end: the override points at a linked worktree of the client's clone. Nothing is
/// deleted, no record is written, and the repo does not read as deleted.
#[tokio::test]
async fn deleting_a_linked_worktree_as_the_target_is_refused_and_nothing_is_recorded() {
    let fx = fixture();
    let main = fx.root.join("elsewhere").join("main");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::rename(&fx.checkout, &main).unwrap();
    let linked = fx.root.join("elsewhere").join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &linked.to_string_lossy(),
        ],
    );
    fx.state
        .settings
        .set_repo_path(REPO, Some(linked.to_string_lossy().into_owned()));

    let refusal = delete_source(&fx.state, &fx.project_id, REPO, &req(true))
        .await
        .err()
        .expect("refused");

    assert_eq!(refusal.code, "linked_worktree");
    assert!(main.join("src/main.rs").exists(), "the main clone survives");
    assert!(linked.exists());
    assert!(fx.state.source_records.deletions(&fx.project_id).is_empty());
    let status = source_status(&fx.state, &fx.project_id, REPO).await;
    assert!(
        matches!(status, RepoSourceStatus::Present { .. }),
        "{status:?}"
    );
}

#[tokio::test]
async fn delete_purges_server_scan_cache_for_that_repo_only() {
    let fx = fixture();
    fx.add_repo("acme/lib");
    fx.state
        .scan_cache
        .put(&fx.project_id, manifest_for(&[REPO, "acme/lib"]));

    let record = delete_ok(&fx).await;

    let left = fx
        .state
        .scan_cache
        .get(&fx.project_id)
        .expect("lib keeps its entry");
    assert_eq!(left.files.keys().collect::<Vec<_>>(), ["acme/lib"]);
    assert!(left.findings.iter().all(|f| f.repo == "acme/lib"));
    let cache = item(&record, PurgedKind::ServerScanCache);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0].outcome, PurgeOutcome::Removed);
    assert_eq!(
        cache[0].path,
        format!("scan-cache.json#{}/{REPO}", fx.project_id)
    );
    assert!(
        fx.root.join("work/acme/lib").exists(),
        "the other repo's checkout is untouched"
    );
}

#[tokio::test]
async fn delete_purges_cli_cache_and_ledger_under_state_dir_by_path_key() {
    let fx = fixture();
    let cli = fx.roots().cli_state_dir.unwrap();
    std::fs::create_dir_all(&cli).unwrap();
    let [cache, ledger] = repo_state_files_in(&cli, &fx.checkout);
    std::fs::write(&cache, "{\"findings\":[]}").unwrap();
    std::fs::write(&ledger, "{}").unwrap();
    // Another repo's files in the same directory must survive.
    let other_repo = fx.root.join("work/acme/other-repo");
    std::fs::create_dir_all(&other_repo).unwrap();
    let [other_cache, other_ledger] = repo_state_files_in(&cli, &other_repo);
    std::fs::write(&other_cache, "{}").unwrap();
    std::fs::write(&other_ledger, "{}").unwrap();

    let record = delete_ok(&fx).await;

    assert!(!cache.exists() && !ledger.exists());
    assert!(other_cache.exists() && other_ledger.exists());
    assert_eq!(
        item(&record, PurgedKind::CliScanCache)[0].outcome,
        PurgeOutcome::Removed
    );
    assert_eq!(
        item(&record, PurgedKind::CliLedger)[0].outcome,
        PurgeOutcome::Removed
    );
}

#[tokio::test]
async fn delete_purges_recorded_model_session_files_and_leaves_unrecorded_ones() {
    let fx = fixture();
    let claude = fx.roots().claude_projects_dir.unwrap();
    let a = write_claude_session(
        &claude,
        "-work-acme-app",
        SESSION_A,
        "prompt with client source",
    );
    let b = write_claude_session(
        &claude,
        "-private-tmp",
        SESSION_B,
        "another recorded session",
    );
    let sidecar = claude.join("-work-acme-app").join(SESSION_A);
    std::fs::create_dir_all(&sidecar).unwrap();
    std::fs::write(sidecar.join("notes"), "x").unwrap();
    let unrecorded = write_claude_session(
        &claude,
        "-work-acme-app",
        SESSION_UNRECORDED,
        "the operator's own chat",
    );

    let mut report = report_at(REPO, &fx.head);
    report.provenance.model_sessions = [SESSION_A, SESSION_B]
        .iter()
        .map(|id| ModelSession {
            session_id: (*id).to_string(),
            cwd: "/work".to_string(),
            created_at: "2026-10-24T10:01:00Z".to_string(),
        })
        .collect();
    fx.state.set_last_scan(fx.project_id.clone(), report);

    let record = delete_ok(&fx).await;

    assert!(!a.exists() && !b.exists() && !sidecar.exists());
    assert!(
        unrecorded.exists(),
        "a session Camerata did not record is never touched"
    );
    let sessions = item(&record, PurgedKind::ModelSessionFile);
    assert_eq!(
        sessions.len(),
        3,
        "two files and one sidecar directory: {sessions:?}"
    );
    assert!(sessions.iter().all(|s| s.outcome == PurgeOutcome::Removed));
    assert!(record.residuals.iter().any(|r| r.contains("Cli backend")));
    // The ids are forgotten once their files are gone.
    assert!(fx
        .state
        .inspections
        .latest(&fx.project_id)
        .unwrap()
        .model_sessions
        .is_empty());
}

#[tokio::test]
async fn a_recorded_session_whose_file_is_gone_is_listed_as_already_absent() {
    let fx = fixture();
    let mut report = report_at(REPO, &fx.head);
    report.provenance.model_sessions = vec![ModelSession {
        session_id: SESSION_A.to_string(),
        cwd: "/work".to_string(),
        created_at: String::new(),
    }];
    fx.state.set_last_scan(fx.project_id.clone(), report);
    let record = delete_ok(&fx).await;
    let sessions = item(&record, PurgedKind::ModelSessionFile);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, PurgeOutcome::AlreadyAbsent);
    assert!(sessions[0].path.contains(SESSION_A));
}

#[tokio::test]
async fn a_corrupted_session_id_in_a_record_never_becomes_a_path() {
    let fx = fixture();
    let claude = fx.roots().claude_projects_dir.unwrap();
    std::fs::create_dir_all(&claude).unwrap();
    let precious = fx.root.join("precious.jsonl");
    std::fs::write(&precious, "keep me").unwrap();
    let mut report = report_at(REPO, &fx.head);
    report.provenance.model_sessions = vec![ModelSession {
        session_id: "../../precious".to_string(),
        cwd: String::new(),
        created_at: String::new(),
    }];
    fx.state.set_last_scan(fx.project_id.clone(), report);

    let record = delete_ok(&fx).await;

    assert!(precious.exists());
    assert!(item(&record, PurgedKind::ModelSessionFile).is_empty());
}

#[tokio::test]
async fn delete_clears_transcripts_and_finished_jobs_for_the_project() {
    let fx = fixture();
    fx.state.transcripts.register(
        crate::SCAN_AUDIT_KEY,
        crate::transcript::AgentTranscript {
            session_id: "audit-1".to_string(),
            role: "auditor".to_string(),
            prompt: "prompt carrying client source".to_string(),
            output: "model output quoting client source".to_string(),
            status: "done".to_string(),
        },
    );
    let finished = fx.state.jobs.create("audit", Some(fx.project_id.clone()));
    fx.state.jobs.finish(
        &finished,
        crate::onboard::ScanReport::gated(&[REPO.to_string()]),
    );
    let other = fx
        .state
        .jobs
        .create("audit", Some("another-project".to_string()));
    fx.state.jobs.finish(
        &other,
        crate::onboard::ScanReport::gated(&["x/y".to_string()]),
    );

    let record = delete_ok(&fx).await;

    assert!(fx.state.transcripts.get(crate::SCAN_AUDIT_KEY).is_empty());
    assert!(fx.state.jobs.get(&finished).is_none());
    assert!(
        fx.state.jobs.get(&other).is_some(),
        "another project's job is untouched"
    );
    let memory = item(&record, PurgedKind::InMemory);
    assert_eq!(memory.len(), 2);
    assert!(memory.iter().all(|m| m.outcome == PurgeOutcome::Removed));
}

#[tokio::test]
async fn delete_keeps_onboarding_draft_and_inspection_entry_snippets() {
    let fx = fixture();
    let mut report = report_at(REPO, &fx.head);
    report.findings.push(crate::onboard::Finding {
        repo: REPO.to_string(),
        path: "src/main.rs".to_string(),
        snippet: "let kept_excerpt = \"KEPT-IN-THE-DELIVERED-REPORT\";".to_string(),
        ..crate::onboard::Finding::default()
    });
    fx.state.set_last_scan(fx.project_id.clone(), report);
    fx.state.draft.save(
        &fx.project_id,
        serde_json::json!({ "scan": { "findings": [{ "snippet": "KEPT-IN-THE-DELIVERED-REPORT" }] } }),
    );

    delete_ok(&fx).await;

    let entry = fx.state.inspections.latest(&fx.project_id).unwrap();
    assert!(entry.report.findings[0]
        .snippet
        .contains("KEPT-IN-THE-DELIVERED-REPORT"));
    let draft = fx.state.draft.load(&fx.project_id).unwrap();
    assert!(draft.to_string().contains("KEPT-IN-THE-DELIVERED-REPORT"));
    // And the in-memory last scan still serves the review screen.
    assert_eq!(
        fx.state
            .get_last_scan(&fx.project_id)
            .unwrap()
            .findings
            .len(),
        1
    );
}

#[tokio::test]
async fn delete_keeps_repo_path_override_so_refetch_returns_to_the_same_folder() {
    let fx = fixture();
    let handmade = fx.root.join("hand-made").join("app");
    std::fs::create_dir_all(handmade.parent().unwrap()).unwrap();
    crate::test_git::clone_as(&fx.remote_url, &handmade, "https://github.com/acme/app.git");
    fx.state
        .settings
        .set_repo_path(REPO, Some(handmade.to_string_lossy().into_owned()));

    delete_ok(&fx).await;

    assert!(!handmade.exists());
    assert_eq!(
        fx.state.settings.repo_path(REPO).as_deref(),
        Some(handmade.to_string_lossy().as_ref()),
        "the override survives so a re-fetch comes back to this folder"
    );
    assert!(
        fx.checkout.exists(),
        "the workspace-layout clone was not the deleted one"
    );
}

#[tokio::test]
async fn delete_removes_empty_owner_parent_only_under_workspace_root() {
    let fx = fixture();
    // With a sibling repo under the same owner, the owner folder stays.
    fx.add_repo("acme/lib");
    delete_ok(&fx).await;
    assert!(!fx.checkout.exists());
    assert!(
        fx.root.join("work/acme").exists(),
        "acme/lib still lives there"
    );
    assert!(
        fx.root.join("work").exists(),
        "the workspace root is never removed"
    );

    // Alone, the emptied owner folder goes with it, but never the workspace root.
    let solo = fixture();
    delete_ok(&solo).await;
    assert!(!solo.root.join("work/acme").exists());
    assert!(solo.root.join("work").exists());

    // A repo linked OUTSIDE the workspace root: its parent is not Camerata's to tidy.
    let linked = fixture();
    let outside = linked.root.join("somewhere").join("clients").join("app");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    crate::test_git::clone_as(
        &linked.remote_url,
        &outside,
        "https://github.com/acme/app.git",
    );
    linked
        .state
        .settings
        .set_repo_path(REPO, Some(outside.to_string_lossy().into_owned()));
    delete_ok(&linked).await;
    assert!(!outside.exists());
    assert!(
        linked.root.join("somewhere/clients").exists(),
        "an owner folder outside the workspace root is left alone"
    );
}

#[tokio::test]
async fn delete_second_call_reports_already_deleted_without_new_record() {
    let fx = fixture();
    let first = delete_ok(&fx).await;

    let second = delete_source(&fx.state, &fx.project_id, REPO, &req(false))
        .await
        .expect("not a refusal");

    let DeleteOutcome::AlreadyDeleted(existing) = second else {
        panic!("expected AlreadyDeleted");
    };
    assert_eq!(existing.id, first.id);
    assert_eq!(fx.state.source_records.deletions(&fx.project_id).len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn delete_records_partial_when_one_item_fails() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let wt = fx.root.join("elsewhere").join("locked-wt");
    std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
    git(
        &fx.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "locked",
            &wt.to_string_lossy(),
        ],
    );
    // A directory inside the worktree that cannot be emptied.
    let locked = wt.join("locked-dir");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("file"), "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    let outcome = delete_source(&fx.state, &fx.project_id, REPO, &req(false)).await;

    // Restore permissions BEFORE asserting so the temp dir can always be cleaned up.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Ok(DeleteOutcome::Done {
        record, partial, ..
    }) = outcome
    else {
        panic!("expected a completed (partial) deletion");
    };
    assert!(partial);
    let worktrees = item(&record, PurgedKind::Worktree);
    assert!(
        matches!(worktrees[0].outcome, PurgeOutcome::Failed { .. }),
        "{:?}",
        worktrees[0].outcome
    );
    // The rest still ran, and the record tells the truth.
    assert!(!fx.checkout.exists());
    assert_eq!(
        item(&record, PurgedKind::Checkout)[0].outcome,
        PurgeOutcome::Removed
    );
    assert!(record.render_text().contains("NOT REMOVED"));
    assert_eq!(fx.state.source_records.deletions(&fx.project_id).len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn delete_never_follows_a_symlink_inside_the_checkout() {
    let fx = fixture();
    let precious = fx.root.join("precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(
        precious.join("do-not-delete.txt"),
        "the operator's other work",
    )
    .unwrap();
    // A link inside the checkout (git sees it as an untracked file: tick the box).
    std::os::unix::fs::symlink(&precious, fx.checkout.join("link-out")).unwrap();

    match delete_source(&fx.state, &fx.project_id, REPO, &req(true)).await {
        Ok(DeleteOutcome::Done { .. }) => {}
        other => panic!("expected a deletion, got {}", other.is_ok()),
    }

    assert!(!fx.checkout.exists());
    assert!(
        precious.join("do-not-delete.txt").exists(),
        "removing the checkout removed only the link, never what it pointed at"
    );
}

#[tokio::test]
async fn delete_record_never_contains_a_token() {
    let fx = fixture();
    git(
        &fx.checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "https://x-access-token:ghp_SECRETSECRET123@github.com/acme/app.git",
        ],
    );

    let record = delete_ok(&fx).await;

    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains("ghp_SECRETSECRET123"), "{json}");
    assert!(!record.render_text().contains("ghp_SECRETSECRET123"));
    assert_eq!(
        record.origin_url.as_deref(),
        Some("https://github.com/acme/app.git")
    );
    let stored = serde_json::to_string(&fx.state.source_records.deletions(&fx.project_id)).unwrap();
    assert!(!stored.contains("ghp_SECRETSECRET123"));
}

#[tokio::test]
async fn delete_with_the_tick_records_the_uncommitted_residual_and_removes_the_work() {
    let fx = fixture();
    write_file(
        &fx.checkout,
        "src/main.rs",
        "fn main() { /* unfinished remediation */ }\n",
    );
    let refused = delete_source(&fx.state, &fx.project_id, REPO, &req(false)).await;
    assert_eq!(refused.err().map(|r| r.code), Some("dirty_tree"));
    assert!(fx.checkout.exists());

    let outcome = delete_source(&fx.state, &fx.project_id, REPO, &req(true)).await;
    let Ok(DeleteOutcome::Done { record, .. }) = outcome else {
        panic!("expected a deletion");
    };
    assert!(record.include_uncommitted);
    assert!(record
        .residuals
        .iter()
        .any(|r| r.contains("Uncommitted changes")));
    assert!(!fx.checkout.exists());
}

#[tokio::test]
async fn a_refused_delete_touches_nothing_and_records_nothing() {
    let fx = fixture();
    fx.state
        .scan_cache
        .put(&fx.project_id, manifest_for(&[REPO]));
    let wrong = DeleteRequest {
        confirm: "acme/App".to_string(),
        include_uncommitted: true,
    };

    let refused = delete_source(&fx.state, &fx.project_id, REPO, &wrong).await;

    assert_eq!(refused.err().map(|r| r.code), Some("confirmation_mismatch"));
    assert!(fx.checkout.join(".git").exists());
    assert!(fx.state.scan_cache.get(&fx.project_id).is_some());
    assert!(fx.state.source_records.deletions(&fx.project_id).is_empty());
}

#[tokio::test]
async fn a_deleted_repo_can_be_deleted_again_after_it_is_cloned_back_by_hand() {
    let fx = fixture();
    delete_ok(&fx).await;
    crate::test_git::clone_as(
        &fx.remote_url,
        &fx.checkout,
        "https://github.com/acme/app.git",
    );
    // On disk again: status is Present, and a new deletion is a new record.
    assert!(matches!(
        source_status(&fx.state, &fx.project_id, REPO).await,
        RepoSourceStatus::Present {
            has_deletion_history: true,
            ..
        }
    ));
    let second = delete_ok(&fx).await;
    assert_eq!(fx.state.source_records.deletions(&fx.project_id).len(), 2);
    assert_ne!(second.seq, 0);
}

// ── The HTTP surface (design 3.2 and 3.3) ──────────────────────────────────────────────────

mod http {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::super::test_support::{fixture, Fx, REPO};
    use crate::test_git::write_file;

    async fn post(
        fx: &Fx,
        suffix: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let resp = crate::router(fx.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{}/repos/{}/source/{suffix}",
                        fx.project_id,
                        REPO.replace('/', "%2F")
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn preview_lists_the_plan_and_warns_about_unpushed_work_without_refusing() {
        let fx = fixture();
        write_file(&fx.checkout, "src/main.rs", "fn main() { /* wip */ }\n");

        let (status, json) = post(&fx, "delete-preview", serde_json::json!({})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["ok"], true);
        let plan = &json["plan"];
        assert_eq!(plan["dirty"], true);
        assert_eq!(plan["origin_url"], "https://github.com/acme/app.git");
        assert!(plan["warnings"][0]
            .as_str()
            .unwrap()
            .contains("uncommitted changes"));
        assert_eq!(plan["items"][0]["kind"], "checkout");
        assert_eq!(plan["items"][0]["outcome"], "planned");
        // The dialog's worded list: same labels and counts the record will carry.
        assert_eq!(plan["item_view"][0]["label"], "checkout");
        assert_eq!(plan["item_view"][0]["path"], plan["items"][0]["path"]);
        assert!(plan["item_view"][0]["counts"]
            .as_str()
            .unwrap()
            .contains("files"));
        assert!(plan["total_summary"].as_str().unwrap().contains(" files, "));
        assert!(fx.checkout.exists(), "a preview removes nothing");
    }

    #[tokio::test]
    async fn delete_over_http_removes_the_source_and_returns_record_and_status() {
        let fx = fixture();
        let (status, json) = post(
            &fx,
            "delete",
            serde_json::json!({ "confirm": REPO, "include_uncommitted": false }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["ok"], true);
        assert_eq!(json["status"]["state"], "deleted");
        assert_eq!(json["record"]["repo"], REPO);
        assert!(!fx.checkout.exists());

        // Idempotent.
        let (status, again) = post(&fx, "delete", serde_json::json!({ "confirm": REPO })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["code"], "already_deleted");
        assert_eq!(again["record"]["id"], json["record"]["id"]);
        // The preview of a deleted repo is a refusal, not a plan.
        let (status, preview) = post(&fx, "delete-preview", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(preview["code"], "already_deleted");
    }

    #[tokio::test]
    async fn a_refusal_over_http_is_a_409_with_a_code_and_a_sentence() {
        let fx = fixture();
        let (status, json) = post(&fx, "delete", serde_json::json!({ "confirm": "acme/ap" })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["ok"], false);
        assert_eq!(json["code"], "confirmation_mismatch");
        assert!(json["message"].as_str().unwrap().contains("acme/app"));
        assert!(fx.checkout.exists());
    }

    #[tokio::test]
    async fn an_unknown_project_is_a_404() {
        let fx = fixture();
        let resp = crate::router(fx.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/nope/repos/acme%2Fapp/source/delete-preview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

// ── Honest residual sentences (what a deletion cannot cover) ───────────────────────────────

const UNRECORDED: &str = "before Camerata recorded session ids";
const NOT_TRACKED: &str = "chat assistant";
const MOVED: &str = "moved or renamed";
const OTHER_PROJECTS: &str = "Other projects";

fn has(record: &DeletionRecord, needle: &str) -> bool {
    record.residuals.iter().any(|r| r.contains(needle))
}

/// An inspection entry for `REPO` carrying `sessions` (empty = what every inspection made
/// before Camerata recorded session ids looks like).
fn append_inspection(fx: &Fx, started: &str, sessions: &[&str]) {
    let mut report = report_at(REPO, &fx.head);
    report.provenance.started_at = started.to_string();
    report.provenance.model_sessions = sessions
        .iter()
        .map(|id| ModelSession {
            session_id: (*id).to_string(),
            cwd: "/work".to_string(),
            created_at: String::new(),
        })
        .collect();
    fx.state
        .inspections
        .append(crate::inspections::InspectionEntry::from_report(
            &fx.project_id,
            report,
            crate::inspections::InspectionKind::Full,
        ))
        .unwrap();
}

async fn delete_repo(fx: &Fx, repo: &str) -> DeletionRecord {
    let request = DeleteRequest {
        confirm: repo.to_string(),
        include_uncommitted: false,
    };
    match delete_source(&fx.state, &fx.project_id, repo, &request).await {
        Ok(DeleteOutcome::Done { record, .. }) => *record,
        other => panic!("expected a deletion of {repo}: {:?}", other.err()),
    }
}

/// M1: an inspection from before session ids were recorded left Claude Code session files that
/// cannot be found. The record must say so instead of silently implying completeness.
#[tokio::test]
async fn a_cli_project_with_an_unrecorded_inspection_says_its_sessions_were_not_removed() {
    let fx = fixture();
    append_inspection(&fx, "2026-10-24T10:00:00Z", &[]);

    let record = delete_ok(&fx).await;

    assert!(has(&record, UNRECORDED), "{:?}", record.residuals);
    assert!(has(&record, "~/.claude/projects"), "{:?}", record.residuals);
}

#[tokio::test]
async fn a_cli_project_with_no_inspection_at_all_carries_the_same_sentence() {
    let fx = fixture();
    let record = delete_ok(&fx).await;
    assert!(has(&record, UNRECORDED), "{:?}", record.residuals);
}

#[tokio::test]
async fn an_api_project_has_no_claude_code_sessions_to_disclose() {
    let fx = fixture();
    fx.state
        .projects
        .set_backend(&fx.project_id, crate::project::ProjectBackend::Api);
    append_inspection(&fx, "2026-10-24T10:00:00Z", &[]);

    let record = delete_ok(&fx).await;

    assert!(!has(&record, UNRECORDED), "{:?}", record.residuals);
    assert!(!has(&record, NOT_TRACKED), "{:?}", record.residuals);
}

#[tokio::test]
async fn fully_recorded_inspections_do_not_carry_the_unrecorded_sentence() {
    let fx = fixture();
    append_inspection(&fx, "2026-10-24T10:00:00Z", &[SESSION_A]);

    let record = delete_ok(&fx).await;

    assert!(!has(&record, UNRECORDED), "{:?}", record.residuals);
}

/// The ids are forgotten after the first repo's deletion. That must not make a fully recorded
/// project look unrecorded to the second repo's record, nor hide a genuinely unrecorded entry.
#[tokio::test]
async fn forgetting_sessions_after_the_first_repo_keeps_the_second_records_sentence_correct() {
    let fx = fixture();
    fx.add_repo("acme/lib");
    append_inspection(&fx, "2026-10-24T10:00:00Z", &[SESSION_A]);

    let first = delete_repo(&fx, REPO).await;
    let second = delete_repo(&fx, "acme/lib").await;

    assert!(!has(&first, UNRECORDED), "{:?}", first.residuals);
    assert!(
        !has(&second, UNRECORDED),
        "the sessions were recorded, then removed with the first repo: {:?}",
        second.residuals
    );
}

#[tokio::test]
async fn a_genuinely_unrecorded_entry_is_still_disclosed_on_the_second_repos_record() {
    let fx = fixture();
    fx.add_repo("acme/lib");
    append_inspection(&fx, "2026-10-24T09:00:00Z", &[]);
    append_inspection(&fx, "2026-10-24T10:00:00Z", &[SESSION_A]);

    let first = delete_repo(&fx, REPO).await;
    let second = delete_repo(&fx, "acme/lib").await;

    assert!(has(&first, UNRECORDED), "{:?}", first.residuals);
    assert!(has(&second, UNRECORDED), "{:?}", second.residuals);
}

/// Owner decision (b): sessions of the chat assistant and of governed-dev agents are not
/// tracked, so a Cli project's record says what it cannot cover.
#[tokio::test]
async fn a_cli_record_discloses_untracked_assistant_and_governed_dev_sessions() {
    let fx = fixture();
    let record = delete_ok(&fx).await;
    assert!(has(&record, NOT_TRACKED), "{:?}", record.residuals);
    assert!(has(&record, "--add-dir"), "{:?}", record.residuals);
}

#[tokio::test]
async fn every_record_discloses_caches_left_under_an_old_path() {
    let fx = fixture();
    let record = delete_ok(&fx).await;
    assert!(has(&record, MOVED), "{:?}", record.residuals);
}

#[tokio::test]
async fn other_projects_scan_cache_slices_for_the_same_repo_are_disclosed_only_when_they_exist() {
    let fx = fixture();
    let other = fx
        .state
        .projects
        .create("Other", vec![REPO.to_string()])
        .unwrap();
    fx.state.scan_cache.put(&other.id, manifest_for(&[REPO]));
    // A third project lists the repo but holds no cache for it: not mentioned.
    let quiet = fx
        .state
        .projects
        .create("Quiet", vec![REPO.to_string()])
        .unwrap();

    let record = delete_ok(&fx).await;

    assert!(has(&record, OTHER_PROJECTS), "{:?}", record.residuals);
    let sentence = record
        .residuals
        .iter()
        .find(|r| r.contains(OTHER_PROJECTS))
        .unwrap();
    assert!(sentence.contains("Other"), "{sentence}");
    assert!(!sentence.contains(&quiet.name), "{sentence}");
    // The other project's cache is indeed still there.
    assert!(fx.state.scan_cache.get(&other.id).is_some());
}

#[tokio::test]
async fn no_other_projects_sentence_when_nobody_else_caches_the_repo() {
    let fx = fixture();
    let record = delete_ok(&fx).await;
    assert!(!has(&record, OTHER_PROJECTS), "{:?}", record.residuals);
}
