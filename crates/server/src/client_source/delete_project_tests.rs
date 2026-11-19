//! "Delete this project and all its data": every location Camerata can find for project A is
//! seeded with a sentinel string, so are project B's; after the action no A sentinel is left
//! anywhere under Camerata's folders, every B sentinel still is, and the record sits outside the
//! deleted tree with counts and no sentinel.

use std::path::{Path, PathBuf};

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

fn world() -> World {
    let fx = fixture();
    let (b_id, b_checkout) = add_project_b(&fx);
    let a_checkout = fx.checkout.clone();
    let pid = fx.project_id.clone();
    seed(&fx, &pid, REPO, &a_checkout, SENT_A, "aaaaaaaaaaaa");
    let b_files = seed(&fx, &b_id, REPO_B, &b_checkout, SENT_B, "bbbbbbbbbbbb");
    World { fx, b_id, b_files }
}

fn req(name: &str) -> DeleteAllRequest {
    DeleteAllRequest {
        confirm: name.to_string(),
    }
}

#[tokio::test]
async fn deleting_a_project_removes_every_copy_of_its_data_and_nothing_of_the_other_projects() {
    let w = world();
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

    // A restart finds nothing of A either (the files, not the memory, were removed).
    let again = fx.restarted();
    assert!(again.inspections.list(&fx.project_id).is_empty());
    assert!(again.projects.get(&fx.project_id).is_none());
}

#[tokio::test]
async fn a_second_deletion_finds_the_records_folder_untouched() {
    let w = world();
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
    let w = world();
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
    let w = world();
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
    let w = world();
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
    let w = world();
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
    let w = world();
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
    let w = world();
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
    let w = world();
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
