//! "Delete this project and all its data": every location Camerata can find for project A is
//! seeded with a sentinel string, so are project B's; after the action no A sentinel is left
//! anywhere under Camerata's folders, every B sentinel still is, and the record sits outside the
//! deleted tree with counts and no sentinel.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::os::unix::fs::PermissionsExt;
use tower::ServiceExt;

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::EngineCheckVerdictReq;
use camerata_llm::session_sink::ModelSession;

use super::delete_project::{delete_all, DeleteAllRequest, RECORDS_FOLDER};
use super::test_support::{fixture, report_at, Fx, REPO};
use crate::decisions::DecisionCtx;
use crate::edits::{EditCtx, EditDraft};
use crate::onboard::Finding;
use crate::run_store::{ModelsRecord, RunOrigin, RunRecord, RunStatus, STORE_SCHEMA_VERSION};
use crate::scan_cache::state_dir::repo_state_key;
use crate::scan_cache::{ScanManifest, MANIFEST_VERSION};

const SENT_A: &str = "SENTINEL-ALPHA-9F31";
const SENT_B: &str = "SENTINEL-BETA-77C2";
const REPO_B: &str = "other/lib";
const NOW: &str = "2026-11-19T09:00:00Z";

/// A second project over its own repo, cloned under the workspace.
fn add_project_b(fx: &Fx) -> (String, PathBuf) {
    let b = fx
        .state
        .projects
        .create("Beta", vec![REPO_B.to_string()])
        .unwrap();
    let base = fx.root.join("remote-other-lib");
    std::fs::create_dir_all(&base).unwrap();
    let (_bare, url, _sha) = crate::test_git::bare_remote(&base, &[("lib.rs", "pub fn f() {}\n")]);
    let checkout = fx.root.join("work").join(REPO_B);
    crate::test_git::clone_as(&url, &checkout, &format!("https://github.com/{REPO_B}.git"));
    (b.id, checkout)
}

fn finding(repo: &str, sentinel: &str) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: "src/main.rs".to_string(),
        line: 1,
        rule_id: "R-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("let k = \"{sentinel}\";"),
        detail: "detail".to_string(),
        ..Finding::default()
    }
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Seed every location for one project and return the paths (and what each holds) that a
/// sentinel was written into as plain files.
fn seed(
    fx: &Fx,
    pid: &str,
    repo: &str,
    checkout: &Path,
    sentinel: &str,
    tag: &str,
) -> Vec<PathBuf> {
    let state = &fx.state;
    let roots = fx.roots();
    let cli = roots.cli_state_dir.clone().unwrap();
    let claude = roots.claude_projects_dir.clone().unwrap();
    let mut files = Vec::new();
    let mut put = |p: PathBuf, body: String| {
        write(&p, &body);
        files.push(p);
    };

    put(checkout.join("secret.txt"), sentinel.to_string());

    // Server scan cache slice.
    let mut manifest = ScanManifest {
        version: MANIFEST_VERSION,
        findings: vec![finding(repo, sentinel)],
        ..ScanManifest::default()
    };
    manifest.files.insert(
        repo.to_string(),
        [("src/main.rs".to_string(), "ab".to_string())].into(),
    );
    state.scan_cache.put(pid, manifest);

    // Inspection (persisted), with a recorded model session and its file.
    let mut report = report_at(repo, "0".repeat(40).as_str());
    report.findings = vec![finding(repo, sentinel)];
    state.set_last_scan(pid.to_string(), report);
    let session = format!("6f1d2c3a-1111-4222-8333-{tag}");
    state
        .inspections
        .record_model_session(
            pid,
            ModelSession {
                session_id: session.clone(),
                cwd: checkout.to_string_lossy().into_owned(),
                created_at: NOW.to_string(),
            },
        )
        .unwrap();
    put(
        claude
            .join(format!("-work-{tag}"))
            .join(format!("{session}.jsonl")),
        sentinel.to_string(),
    );

    // Onboarding draft entry.
    state
        .draft
        .save(pid, serde_json::json!({ "note": sentinel }));

    // Decisions, edits, engine check.
    state
        .decisions
        .record_site(
            pid,
            &NewDecisionReq {
                scope: Scope::Site,
                key: format!("key-{tag}"),
                state: DecisionState::Ignored,
                reason: format!("accepted: {sentinel}"),
                decided_by: "zach".to_string(),
                needed_me: None,
                sample_verdict: None,
                except: Vec::new(),
                run_id: String::new(),
                decided_at: String::new(),
            },
            DecisionCtx {
                run_id: "insp-1",
                engine_version: "9.9.9",
                now: NOW,
            },
        )
        .unwrap();
    state
        .edits
        .record_edit(
            pid,
            EditDraft {
                key: format!("key-{tag}"),
                field: EditField::Headline,
                old: "old".to_string(),
                new: format!("new {sentinel}"),
                reason: "wording".to_string(),
                by: "zach".to_string(),
                run_id: String::new(),
                fingerprint: "fp".to_string(),
            },
            EditCtx {
                run_id: "insp-1",
                now: NOW,
            },
        )
        .unwrap();
    state
        .engine_check
        .record(
            pid,
            &format!("{sentinel}-site"),
            &EngineCheckVerdictReq {
                right: true,
                wrong: None,
                correction: None,
                by: "zach".to_string(),
            },
            "insp-1",
            NOW,
        )
        .unwrap();

    // Stray leftovers beside the inspection file.
    let insp = state.inspections.file_for(pid).unwrap();
    put(insp.with_extension("json.tmp"), sentinel.to_string());
    put(insp.with_extension("json.corrupt"), sentinel.to_string());

    // CLI bookkeeping keyed by the checkout, finder files, a multi-repo set.
    let key = repo_state_key(checkout);
    for suffix in [
        ".cache.json",
        ".ledger.json",
        ".finder-run.json",
        ".run-difference.md",
    ] {
        put(cli.join(format!("{key}{suffix}")), sentinel.to_string());
    }
    let canon = checkout
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let set = format!("project-1-{tag}00000");
    put(
        cli.join(format!("{set}.repos.json")),
        serde_json::to_string(&vec![canon]).unwrap(),
    );
    put(cli.join(format!("{set}.cache.json")), sentinel.to_string());
    put(cli.join(format!("{set}.ledger.json")), sentinel.to_string());

    // A saved run of the project, with a finder room and a trace inside it.
    let run_dir = cli.join("runs").join(format!("run-{tag}"));
    let record = RunRecord {
        schema: STORE_SCHEMA_VERSION,
        run_id: format!("run-{tag}"),
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        status: RunStatus::Complete,
        origin: RunOrigin::Cockpit,
        project_id: Some(pid.to_string()),
        attempt: 1,
        engine_version: "9.9.9".to_string(),
        project_key: "k".to_string(),
        repos: vec![crate::run_store::RepoRecord {
            name: repo.to_string(),
            dir: checkout.to_string_lossy().into_owned(),
            commit: None,
            clean: true,
            tree_at_start: String::new(),
            tree_at_end: None,
        }],
        models: ModelsRecord {
            audit: "m".into(),
            calibration: "m".into(),
            fix: "m".into(),
            backend: "cli".into(),
            mode: "auto".into(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
        },
        rules_fingerprint: String::new(),
        ai_scan_key: Default::default(),
        nonce_seed: String::new(),
        args: serde_json::Value::Null,
        estimate_usd: None,
        plan: Vec::new(),
        incomplete_reasons: Vec::new(),
        notes: Vec::new(),
    };
    put(
        run_dir.join("run.json"),
        serde_json::to_string(&record).unwrap(),
    );
    put(run_dir.join("calls").join("c.json"), sentinel.to_string());
    put(
        run_dir.join("finder-rooms/room-1/code/a.rs"),
        sentinel.to_string(),
    );
    put(run_dir.join("finder-traces/t.jsonl"), sentinel.to_string());
    files
}

/// Every file under `root` whose bytes contain `needle`.
fn hits(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(root) else {
        return out;
    };
    for e in read.filter_map(Result::ok) {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            out.extend(hits(&p, needle));
        } else if std::fs::read(&p)
            .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
        {
            out.push(p);
        }
    }
    out
}

struct World {
    fx: Fx,
    b_id: String,
    b_files: Vec<PathBuf>,
}

/// Seed the development console rows of one project (a unit of work, a story, a question, a
/// resume point, a routine, a checkpoint and an escalation), each carrying `sentinel`.
async fn seed_dev(fx: &Fx, pid: &str, repo: &str, sentinel: &str) {
    use crate::checkpoint::NewCheckpoint;
    use crate::clarify_resume::{ClarifyResumeContext, PausedPhase};
    use camerata_app_core::escalation::{RaiseEscalationReq, SubjectKind};
    use camerata_app_core::routine::CreateRoutineReq;
    use camerata_worktracker::{CanonicalStory, FeatureStatus, RepoTarget};
    let state = &fx.state;
    let unit = state
        .uow
        .create_native(Some(repo.to_string()), Some(pid.to_string()))
        .unwrap();
    let sid = unit.story_id.clone();
    state.uow.append_investigation_chat(&sid, "user", sentinel);
    state
        .stories
        .upsert(CanonicalStory {
            id: format!("story-{pid}"),
            external_ref: None,
            title: sentinel.to_string(),
            description: sentinel.to_string(),
            status: FeatureStatus::Intake,
            created_by: "zach".to_string(),
            targets: vec![RepoTarget {
                repo: repo.to_string(),
                role: None,
            }],
        })
        .await
        .unwrap();
    let clar = state
        .clarifications
        .post(&sid, &format!("question {sentinel}"), "zach");
    state.clarify_resume.put(
        &clar.id,
        ClarifyResumeContext {
            run_id: "run-1".to_string(),
            story_id: sid.clone(),
            story_title: sentinel.to_string(),
            story_desc: String::new(),
            model: "m".to_string(),
            phase: PausedPhase::Investigation,
            original_task: sentinel.to_string(),
            asked_question: String::new(),
        },
    );
    state.routines.create(&CreateRoutineReq {
        name: "nightly".to_string(),
        schedule: "daily".to_string(),
        intent: sentinel.to_string(),
        prompt: String::new(),
        scope: String::new(),
        model: None,
        project_id: Some(pid.to_string()),
    });
    state.checkpoints.create(NewCheckpoint {
        story_id: sid.clone(),
        run_id: "run-1".to_string(),
        escalation_id: String::new(),
        pause_reason: sentinel.to_string(),
        repo: repo.to_string(),
        branch: String::new(),
        worktree_dir: String::new(),
        base_commit: String::new(),
        iteration: 0,
        max_iterations: 1,
        model: String::new(),
        project_id: Some(pid.to_string()),
    });
    state.escalations.raise(
        RaiseEscalationReq {
            subject_kind: SubjectKind::Uow,
            routine_id: sid,
            reason: sentinel.to_string(),
            stopped_for: String::new(),
            suggestions: Vec::new(),
            raw_context: String::new(),
            checkpoint_id: None,
        },
        "run",
    );
}

const DEV_FILES: [&str; 7] = [
    "uow.json",
    "stories.json",
    "clarifications.json",
    "clarify-resume.json",
    "routines.json",
    "checkpoints.json",
    "escalations.json",
];

async fn world() -> World {
    let fx = fixture();
    let (b_id, b_checkout) = add_project_b(&fx);
    let a_checkout = fx.checkout.clone();
    let pid = fx.project_id.clone();
    seed(&fx, &pid, REPO, &a_checkout, SENT_A, "aaaaaaaaaaaa");
    let b_files = seed(&fx, &b_id, REPO_B, &b_checkout, SENT_B, "bbbbbbbbbbbb");
    seed_dev(&fx, &pid, REPO, SENT_A).await;
    seed_dev(&fx, &b_id, REPO_B, SENT_B).await;
    World { fx, b_id, b_files }
}

fn req(name: &str) -> DeleteAllRequest {
    DeleteAllRequest {
        confirm: name.to_string(),
    }
}

#[tokio::test]
async fn deleting_a_project_removes_every_copy_of_its_data_and_nothing_of_the_other_projects() {
    let w = world().await;
    let fx = &w.fx;
    assert!(!hits(&fx.root, SENT_A).is_empty(), "the seed reached disk");

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .expect("deleted");

    // No A sentinel anywhere under Camerata's folders or the workspace (record included).
    let left = hits(&fx.root, SENT_A);
    assert!(left.is_empty(), "A data left behind: {left:?}");
    assert!(!fx.checkout.exists());
    assert!(
        fx.state.projects.get(&fx.project_id).is_none(),
        "project record gone"
    );
    assert!(fx.state.inspections.list(&fx.project_id).is_empty());
    assert!(fx.state.decisions.list(&fx.project_id).is_empty());
    assert!(fx.state.edits.list(&fx.project_id).is_empty());
    assert!(fx.state.engine_check.list(&fx.project_id).is_empty());
    assert!(fx.state.draft.load(&fx.project_id).is_none());
    assert!(fx.state.get_last_scan(&fx.project_id).is_none());
    assert_eq!(fx.state.scan_cache.project_bytes(&fx.project_id), 0);
    assert!(!out.partial);

    // Every B sentinel is still there, and B's stores still answer.
    for f in &w.b_files {
        assert!(f.exists(), "B file removed: {}", f.display());
        if f.to_string_lossy().ends_with(".repos.json") || f.ends_with("run.json") {
            continue; // holds folder paths, not the sentinel
        }
        assert!(
            std::fs::read(f)
                .unwrap()
                .windows(SENT_B.len())
                .any(|x| x == SENT_B.as_bytes()),
            "{}",
            f.display()
        );
    }
    assert!(fx.state.projects.get(&w.b_id).is_some());
    assert_eq!(fx.state.inspections.list(&w.b_id).len(), 1);
    assert_eq!(fx.state.decisions.list(&w.b_id).len(), 1);
    assert_eq!(fx.state.edits.list(&w.b_id).len(), 1);
    assert_eq!(fx.state.engine_check.list(&w.b_id).len(), 1);
    assert!(fx.state.draft.load(&w.b_id).is_some());
    assert!(fx.state.scan_cache.project_bytes(&w.b_id) > 0);

    // The record sits outside the deleted tree, with counts and no sentinel.
    let record_path = out.record_path.expect("record written");
    assert!(record_path.starts_with(fx.root.join("data").join(RECORDS_FOLDER)));
    let text = std::fs::read_to_string(&record_path).unwrap();
    assert!(!text.contains(SENT_A));
    assert!(out.record.total_files > 0 && out.record.project_removed);
    assert!(out
        .record
        .items
        .iter()
        .any(|i| i.category == "checkout" && i.files > 0));
    assert!(out.record.items.iter().any(|i| i.category == "saved run"));
    assert!(out
        .record
        .items
        .iter()
        .any(|i| i.category == "model session file"));
    assert!(out
        .record
        .items
        .iter()
        .any(|i| i.category == "finder run file"));
    assert!(out
        .record
        .not_reached
        .iter()
        .any(|n| n.contains("Time Machine")));
    assert!(record_path.with_extension("txt").is_file());

    // The development console rows: A's are gone from every store file, B's are still there.
    for f in DEV_FILES {
        let text = std::fs::read_to_string(fx.root.join("data").join(f)).unwrap();
        assert!(!text.contains(SENT_A), "{f} still holds A");
        assert!(text.contains(SENT_B), "{f} lost B's row");
    }
    assert!(out
        .record
        .items
        .iter()
        .any(|i| i.category == "development console record" && i.path.starts_with("uow.json#")));
    // The deleting mark is released when the deletion ends.
    assert!(fx
        .state
        .source_ops
        .deleting_refusal(&fx.project_id)
        .is_none());

    // A restart finds nothing of A either (the files, not the memory, were removed).
    let again = fx.restarted();
    assert!(again.inspections.list(&fx.project_id).is_empty());
    assert!(again.projects.get(&fx.project_id).is_none());
}

#[tokio::test]
async fn a_second_deletion_finds_the_records_folder_untouched() {
    let w = world().await;
    let fx = &w.fx;
    let first = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();
    let second = delete_all(&fx.state, &w.b_id, &req("Beta")).await.unwrap();
    assert!(
        first.record_path.unwrap().is_file(),
        "the first record survives the second delete"
    );
    assert!(second.record_path.unwrap().is_file());
}

#[tokio::test]
async fn it_is_refused_while_a_scan_of_the_project_is_running_and_removes_nothing() {
    let w = world().await;
    let fx = &w.fx;
    let _scan = fx.state.source_ops.begin_scan(&fx.project_id);

    let refused = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .err()
        .expect("refused");

    assert_eq!(refused.code, "scan_in_progress");
    assert!(fx.checkout.exists());
    assert!(!hits(&fx.root, SENT_A).is_empty());
    assert!(fx.state.projects.get(&fx.project_id).is_some());
}

#[tokio::test]
async fn it_is_refused_while_a_saved_run_of_the_project_is_live() {
    let w = world().await;
    let fx = &w.fx;
    // A live lock on the run: hold the OS lock the store probes.
    let run_dir = fx
        .roots()
        .cli_state_dir
        .unwrap()
        .join("runs")
        .join("run-aaaaaaaaaaaa");
    let lock = crate::run_store::RunLock::acquire(&run_dir).expect("lock");

    let refused = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .err()
        .expect("refused");

    drop(lock);
    assert_eq!(refused.code, "scan_in_progress");
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn a_wrong_typed_name_deletes_nothing() {
    let w = world().await;
    let fx = &w.fx;
    for typed in ["", "acme", "Acme ", "Beta"] {
        let refused = delete_all(&fx.state, &fx.project_id, &req(typed))
            .await
            .err()
            .expect("refused");
        assert_eq!(refused.code, "confirmation_mismatch", "{typed:?}");
    }
    assert!(fx.checkout.exists());
    assert!(!hits(&fx.root, SENT_A).is_empty());
    assert!(fx.state.projects.get(&fx.project_id).is_some());
}

#[tokio::test]
async fn a_checkout_another_project_also_uses_is_kept_and_named_in_the_record() {
    let w = world().await;
    let fx = &w.fx;
    // Project C lists A's repo too.
    let c = fx
        .state
        .projects
        .create("Gamma", vec![REPO.to_string()])
        .unwrap();

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(fx.checkout.exists(), "the shared checkout stays");
    assert!(out
        .record
        .kept
        .iter()
        .any(|k| k.what.contains(REPO) && k.why.contains("Gamma")));
    assert!(fx.state.projects.get(&c.id).is_some());
    // The project's own stores are still gone.
    assert!(fx.state.inspections.list(&fx.project_id).is_empty());
    assert!(fx.state.projects.get(&fx.project_id).is_none());
}

#[tokio::test]
async fn a_cockpit_run_of_another_project_over_a_shared_repo_is_never_removed() {
    let w = world().await;
    let fx = &w.fx;
    // B's run records B as its project but covers A's checkout too (a shared folder).
    let cli = fx.roots().cli_state_dir.unwrap();
    let run_json = cli.join("runs/run-bbbbbbbbbbbb/run.json");
    let mut rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&run_json).unwrap()).unwrap();
    rec["repos"][0]["dir"] = serde_json::json!(fx.checkout.to_string_lossy());
    std::fs::write(&run_json, rec.to_string()).unwrap();

    delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(run_json.is_file(), "B's run stays");
}

#[tokio::test]
async fn a_failed_location_keeps_the_project_so_the_deletion_can_be_retried() {
    let w = world().await;
    let fx = &w.fx;
    // Make the inspection file undeletable by turning it into a non-empty directory? A plain
    // directory is removable, so block instead through a repo the guard refuses: point the repo
    // at a folder that is not a clone of it.
    let wrong = fx.root.join("work").join("acme").join("app");
    std::fs::remove_dir_all(wrong.join(".git")).unwrap();

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(out.partial);
    assert!(!out.record.project_removed);
    assert!(
        fx.state.projects.get(&fx.project_id).is_some(),
        "the project stays for a retry"
    );
    assert!(out
        .record
        .items
        .iter()
        .any(|i| !i.outcome.is_clear() && i.category == "checkout"));
}

#[tokio::test]
async fn the_preview_lists_the_plan_and_removes_nothing() {
    use axum::extract::{Path as AxPath, State};
    let w = world().await;
    let fx = &w.fx;
    let resp = super::delete_project::delete_all_preview(
        State(fx.state.clone()),
        AxPath(fx.project_id.clone()),
    )
    .await;
    let body = axum::body::to_bytes(
        axum::response::IntoResponse::into_response(resp).into_body(),
        usize::MAX,
    )
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], true);
    let labels: Vec<String> = v["plan"]["item_view"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect();
    for want in [
        "checkout",
        "inspection store",
        "saved run",
        "project record",
    ] {
        assert!(
            labels.iter().any(|l| l == want),
            "{want} missing from {labels:?}"
        );
    }
    assert!(fx.checkout.exists());
    assert!(fx.state.projects.get(&fx.project_id).is_some());
}

// ── round 2 ──────────────────────────────────────────────────────────────────────────────

fn b_checkout(fx: &Fx) -> PathBuf {
    fx.root.join("work").join(REPO_B)
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A test that makes a file unwritable needs a user the permission bits bind.
fn assert_permissions_bind(dir: &Path) {
    let probe = dir.join("probe-root-check");
    assert!(
        std::fs::write(&probe, "x").is_err(),
        "run these tests as an ordinary user: the permission bits do not bind this one"
    );
}

#[tokio::test]
async fn a_folder_linked_to_the_wrong_repo_loses_nothing_of_another_projects() {
    let w = world().await;
    let fx = &w.fx;
    // Project A's repo is linked (by the machine-local path setting) to project B's checkout.
    fx.state
        .settings
        .set_repo_path(REPO, Some(b_checkout(fx).to_string_lossy().into_owned()));

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    for f in &w.b_files {
        assert!(f.exists(), "B's file was removed: {}", f.display());
    }
    assert!(
        out.partial,
        "the refused folder is reported, the project kept"
    );
    assert!(fx.state.projects.get(&fx.project_id).is_some());
    assert!(out
        .record
        .items
        .iter()
        .any(|i| i.category == "checkout" && !i.outcome.is_clear()));
}

#[tokio::test]
async fn a_failed_session_removal_is_found_again_by_the_retry() {
    let w = world().await;
    let fx = &w.fx;
    let folder = fx
        .roots()
        .claude_projects_dir
        .unwrap()
        .join("-work-aaaaaaaaaaaa");
    let session = folder.join("6f1d2c3a-1111-4222-8333-aaaaaaaaaaaa.jsonl");
    chmod(&folder, 0o555);
    assert_permissions_bind(&folder);

    let first = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(first.partial);
    assert!(session.exists(), "the session file could not be removed");
    assert!(
        !fx.state.inspections.list(&fx.project_id).is_empty(),
        "the inspections, which record the session ids, are kept until everything else is clear"
    );
    assert!(fx.state.projects.get(&fx.project_id).is_some());

    chmod(&folder, 0o755);
    let second = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(!second.partial, "{:?}", second.record.items);
    assert!(
        !session.exists(),
        "the retry found and removed the session file"
    );
    assert!(hits(&fx.root, SENT_A).is_empty());
    assert!(fx.state.projects.get(&fx.project_id).is_none());
}

#[tokio::test]
async fn a_write_that_fails_is_reported_failed_not_removed_and_a_retry_succeeds() {
    for (file, category) in [
        ("scan-cache.json", "scan cache entry"),
        ("onboarding-draft.json", "onboarding draft"),
        ("uow.json", "development console record"),
        ("projects.json", "project record"),
    ] {
        let w = world().await;
        let fx = &w.fx;
        let path = fx.root.join("data").join(file);
        chmod(&path, 0o444);
        assert!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .is_err(),
            "run these tests as an ordinary user"
        );

        let first = delete_all(&fx.state, &fx.project_id, &req("Acme"))
            .await
            .unwrap();

        assert!(first.partial, "{file}: {:?}", first.record.items);
        assert!(
            first.record.items.iter().any(|i| i.category == category
                && matches!(i.outcome, super::PurgeOutcome::Failed { .. })),
            "{file}: no failed {category} item in {:?}",
            first.record.items
        );
        assert!(
            !first
                .record
                .items
                .iter()
                .any(|i| i.path.contains(file.trim_end_matches(".json"))
                    && i.category == category
                    && i.outcome == super::PurgeOutcome::Removed),
            "{file}: reported removed although the write failed"
        );
        assert!(fx.state.projects.get(&fx.project_id).is_some(), "{file}");

        chmod(&path, 0o644);
        let second = delete_all(&fx.state, &fx.project_id, &req("Acme"))
            .await
            .unwrap();
        assert!(!second.partial, "{file}: {:?}", second.record.items);
        assert!(
            hits(&fx.root, SENT_A).is_empty(),
            "{file}: A data left after the retry"
        );
        assert!(fx.state.projects.get(&fx.project_id).is_none(), "{file}");
    }
}

#[tokio::test]
async fn a_scan_or_job_start_while_the_project_is_being_deleted_is_refused() {
    let fx = fixture();
    let _deleting = fx
        .state
        .source_ops
        .begin_deleting(&fx.project_id)
        .expect("not yet marked");
    let project = fx.state.projects.get(&fx.project_id).unwrap();
    let request = super::rescan::project_scan_request(&project, &[REPO.to_string()])
        .await
        .unwrap();

    let job = crate::start_audit_job(
        &fx.state,
        request,
        crate::AuditJobOptions {
            project_id: Some(fx.project_id.clone()),
            ..crate::AuditJobOptions::default()
        },
    )
    .await;

    let state = fx
        .state
        .jobs
        .get(&job)
        .expect("the refused job is recorded");
    assert_eq!(state.status, "failed");
    assert!(
        state.message.unwrap_or_default().contains("being deleted"),
        "the refusal says why"
    );
    // A second deletion of the same project is refused too.
    let again = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .err()
        .expect("refused");
    assert!(again.message.contains("already being deleted"));
}

#[tokio::test]
async fn the_preview_and_the_record_state_the_work_that_will_be_lost() {
    use axum::extract::{Path as AxPath, State};
    let w = world().await;
    let fx = &w.fx;
    let resp = super::delete_project::delete_all_preview(
        State(fx.state.clone()),
        AxPath(fx.project_id.clone()),
    )
    .await;
    let body = axum::body::to_bytes(
        axum::response::IntoResponse::into_response(resp).into_body(),
        usize::MAX,
    )
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let sentences: Vec<String> = v["plan"]["loss_sentences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(sentences.len(), 1, "{sentences:?}");
    assert!(
        sentences[0].starts_with("acme/app: 1 uncommitted change and "),
        "{sentences:?}"
    );
    assert!(sentences[0].ends_with("will be lost."), "{sentences:?}");
    assert_eq!(v["plan"]["losses"][0]["repo"], "acme/app");

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();
    assert_eq!(out.record.lost_work, sentences);
    assert!(out.record.render_text().contains("will be lost."));
}

#[tokio::test]
async fn console_rows_tied_to_a_shared_repository_are_kept_and_listed() {
    let w = world().await;
    let fx = &w.fx;
    fx.state
        .projects
        .create("Gamma", vec![REPO.to_string()])
        .unwrap();
    // A unit with no project of its own on the shared repo: not attributable to Acme alone.
    let shared = fx
        .state
        .uow
        .create_native(Some(REPO.to_string()), None)
        .unwrap();

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(
        fx.state.uow.get(&shared.story_id).is_some(),
        "the shared unit stays"
    );
    assert!(out.record.kept.iter().any(
        |k| k.what.contains("development console record") && k.why.contains("another project")
    ));
    // Acme's own unit (it records its project) went.
    assert!(fx
        .state
        .uow
        .list()
        .iter()
        .all(|u| u.project_id.as_deref() != Some(fx.project_id.as_str())));
}

async fn http(state: &crate::AppState, method: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = crate::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn the_plain_project_delete_is_refused_while_the_project_holds_data() {
    let w = world().await;
    let fx = &w.fx;
    let (status, body) = http(
        &fx.state,
        "DELETE",
        &format!("/api/projects/{}", fx.project_id),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "project_holds_data");
    assert_eq!(
        body["message"],
        "This project still has data on this machine. Use Delete source code from this machine \
         or Delete project and all its data first."
    );
    assert!(fx.state.projects.get(&fx.project_id).is_some());

    // The findings the source-only delete keeps are data too.
    let findings = fx.state.projects.create("Findings", Vec::new()).unwrap();
    fx.state
        .set_last_scan(findings.id.clone(), report_at(REPO, &"1".repeat(40)));
    let (status, body) = http(
        &fx.state,
        "DELETE",
        &format!("/api/projects/{}", findings.id),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(fx.state.projects.get(&findings.id).is_some());
}

#[tokio::test]
async fn the_plain_project_delete_still_removes_a_project_with_no_data() {
    let fx = fixture();
    let empty = fx.state.projects.create("Empty", Vec::new()).unwrap();
    let (status, body) = http(&fx.state, "DELETE", &format!("/api/projects/{}", empty.id)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert!(fx.state.projects.get(&empty.id).is_none());
}

// ── round 3 ──────────────────────────────────────────────────────────────────────────────

async fn post(state: &crate::AppState, uri: &str) -> (StatusCode, serde_json::Value) {
    http(state, "POST", uri).await
}

#[tokio::test]
async fn a_failure_after_the_stories_leaves_the_unit_the_retry_needs_to_find_the_rest() {
    let w = world().await;
    let fx = &w.fx;
    let stories = fx.root.join("data").join("stories.json");
    chmod(&stories, 0o444);
    assert!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&stories)
            .is_err(),
        "run these tests as an ordinary user"
    );

    let first = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    assert!(first.partial);
    // The dependents went first; the units of work (through which the retry finds them) stayed.
    assert!(
        fx.state
            .uow
            .list()
            .iter()
            .any(|u| u.project_id.as_deref() == Some(fx.project_id.as_str())),
        "the unit of work must stay until its dependents and stories are gone"
    );
    let failed: Vec<&str> = first
        .record
        .items
        .iter()
        .filter(|i| i.category == "development console record" && !i.outcome.is_clear())
        .map(|i| i.path.as_str())
        .collect();
    assert!(
        failed.iter().any(|p| p.starts_with("stories.json#")),
        "{failed:?}"
    );
    assert!(
        failed.iter().any(|p| p.starts_with("uow.json#")),
        "the unit set is reported as not attempted: {failed:?}"
    );

    chmod(&stories, 0o644);
    let second = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();
    assert!(!second.partial, "{:?}", second.record.items);
    for f in DEV_FILES {
        let text = std::fs::read_to_string(fx.root.join("data").join(f)).unwrap();
        assert!(!text.contains(SENT_A), "{f} still holds A after the retry");
    }
}

#[tokio::test]
async fn unattributed_rows_and_the_logs_are_listed_as_kept_with_their_reasons() {
    let w = world().await;
    let fx = &w.fx;
    // A unit with no project and no repository: it may be this project's.
    fx.state.uow.create_native(None, None).unwrap();

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    let unassigned = out
        .record
        .kept
        .iter()
        .find(|k| k.what.starts_with("1 development console record"))
        .expect("the unattributed unit is counted");
    assert!(
        unassigned.why.contains("may include this project's"),
        "{}",
        unassigned.why
    );
    assert!(
        unassigned.why.contains("check them by hand"),
        "{}",
        unassigned.why
    );
    let logs = out
        .record
        .kept
        .iter()
        .find(|k| k.what.contains("enforcement_catches.db"))
        .expect("the SQLite logs are listed");
    assert!(logs.why.contains("not client code"), "{}", logs.why);
    assert!(out
        .record
        .kept
        .iter()
        .any(|k| k.what == "source-records.json"));
    assert!(out.record.render_text().contains("check them by hand"));
}

#[tokio::test]
async fn a_refused_checkout_names_the_way_out_in_the_record_and_in_the_remove_refusal() {
    let w = world().await;
    let fx = &w.fx;
    fx.state
        .settings
        .set_repo_path(REPO, Some(b_checkout(fx).to_string_lossy().into_owned()));

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    let blocked = out
        .record
        .items
        .iter()
        .find(|i| i.category == "checkout" && !i.outcome.is_clear())
        .expect("the refused folder is a failed item");
    let super::PurgeOutcome::Failed { error } = &blocked.outcome else {
        panic!("not failed")
    };
    assert!(
        error.contains("is a clone of"),
        "the guard's own reason is kept: {error}"
    );
    assert!(
        error.contains("The way out: unlink acme/app in the project's repositories"),
        "{error}"
    );
    assert!(
        error.contains("remove the folder by hand, then delete again."),
        "{error}"
    );

    // Remove from list is blocked by the same dead end, and says the same way out.
    let (status, body) = http(
        &fx.state,
        "DELETE",
        &format!("/api/projects/{}", fx.project_id),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.starts_with("This project still has data on this machine."),
        "{message}"
    );
    assert!(
        message.contains("The way out: unlink acme/app"),
        "{message}"
    );
}

#[tokio::test]
async fn the_last_check_keeps_the_project_when_a_scan_started_or_another_item_failed() {
    let w = world().await;
    let fx = &w.fx;
    let project = fx.state.projects.get(&fx.project_id).unwrap();
    let clear: Vec<super::delete_project::ProjectDeletionItem> = Vec::new();
    assert_eq!(
        super::delete_project::final_block(&fx.state, &project, &clear),
        None
    );

    // A scan job that started after the checks at the top of the deletion.
    let job = fx.state.jobs.create("audit", Some(fx.project_id.clone()));
    let why = super::delete_project::final_block(&fx.state, &project, &clear)
        .expect("a running scan blocks the last phase");
    assert!(why.contains("a scan started during the deletion"), "{why}");
    fx.state.jobs.fail(&job, "stopped");

    let failed = vec![super::delete_project::ProjectDeletionItem {
        category: "saved run".to_string(),
        path: "x".to_string(),
        files: 0,
        bytes: 0,
        outcome: super::PurgeOutcome::Failed {
            error: "denied".to_string(),
        },
    }];
    let why = super::delete_project::final_block(&fx.state, &project, &failed)
        .expect("a failed item blocks the last phase");
    assert!(
        why.contains("another location could not be removed"),
        "{why}"
    );
}

#[tokio::test]
async fn a_governed_run_or_a_routine_start_is_refused_while_the_project_is_being_deleted() {
    let w = world().await;
    let fx = &w.fx;
    let unit = fx
        .state
        .uow
        .list()
        .into_iter()
        .find(|u| u.project_id.as_deref() == Some(fx.project_id.as_str()))
        .expect("A's unit");
    let routine = fx
        .state
        .routines
        .list_for_project(&fx.project_id)
        .pop()
        .expect("A's routine");
    let _deleting = fx.state.source_ops.begin_deleting(&fx.project_id).unwrap();
    let sentence =
        "This project is being deleted with all its data, so nothing can be started on it.";

    let (status, body) = post(&fx.state, &format!("/api/stories/{}/run", unit.story_id)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], sentence);

    let (status, body) = post(&fx.state, &format!("/api/routines/{}/run", routine.id)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], sentence);

    // Another project's routine is not affected.
    let other = fx.state.routines.list_for_project(&w.b_id).pop().unwrap();
    let (status, _) = post(&fx.state, &format!("/api/routines/{}/run", other.id)).await;
    assert_ne!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_set_aside_unreadable_draft_is_listed_as_a_leftover_and_never_removed() {
    let w = world().await;
    let fx = &w.fx;
    let aside = fx
        .root
        .join("data")
        .join("onboarding-draft.json.unreadable-1700000000");
    std::fs::write(&aside, "{ torn, may name proj-1").unwrap();

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();

    let item = out
        .record
        .kept
        .iter()
        .find(|k| {
            k.what
                .ends_with("onboarding-draft.json.unreadable-1700000000")
        })
        .expect("the set-aside draft is listed");
    assert!(
        item.why.contains("may include this project's"),
        "{}",
        item.why
    );
    assert!(item.why.contains("check it by hand"), "{}", item.why);
    assert!(aside.exists(), "it is not removed: it cannot be attributed");
    assert!(
        out.record
            .render_text()
            .contains("onboarding-draft.json.unreadable-1700000000"),
        "the deletion record names it"
    );
}

#[tokio::test]
async fn a_cleared_projects_deletion_leaves_its_id_nowhere_in_the_draft_file_or_its_bak() {
    let w = world().await;
    let fx = &w.fx;
    // The architect started over: no draft, but a discard stamp for the project.
    fx.state
        .draft
        .save(&fx.project_id, serde_json::json!({"scan": 1}));
    fx.state.draft.clear(&fx.project_id);
    fx.state
        .draft
        .save("other-project", serde_json::json!({"scan": 2}));
    let file = fx.root.join("data").join("onboarding-draft.json");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains(&fx.project_id),
        "the stamp names the project"
    );

    let out = delete_all(&fx.state, &fx.project_id, &req("Acme"))
        .await
        .unwrap();
    assert!(!out.partial, "{:?}", out.record.items);

    for f in ["onboarding-draft.json", "onboarding-draft.json.bak"] {
        let text = std::fs::read_to_string(fx.root.join("data").join(f)).unwrap_or_default();
        assert!(
            !text.contains(&fx.project_id),
            "{f} still names the project: {text}"
        );
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("other-project"),
        "another project's draft is kept"
    );
}
