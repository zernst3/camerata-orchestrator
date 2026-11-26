//! #560: every export and review screen reads the rules the inspection ran with (the run's rule
//! snapshot), never the live library, and refuses when that snapshot is missing or damaged.
//!
//! The live library here is the bundled corpus. The run's snapshot is the same corpus with one
//! rule changed (its fix text, its title and a severity bound), so a surface that read the live
//! library would show the live text and fail.

use super::*;
use crate::run_store::rule_snapshot;

const RULE: &str = "SEC-NO-RAW-SQL-CONCAT-1";
/// One word no real rule uses, in the snapshot's fix text and title.
const MARK: &str = "Zanzibarquill";

fn snapshot_fix() -> String {
    format!("Bind every value as a parameter and drop the {MARK} string building.")
}

/// The PDF names a deterministic finding by its option's authored headline.
fn snapshot_headline() -> String {
    format!("Your `<path>` file builds a {MARK} query from pasted text.")
}

fn snapshot_title() -> String {
    format!("{MARK} SQL is never built from strings")
}

async fn live_rules() -> camerata_rules::RuleSet {
    let (set, errors) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "the bundled corpus loads: {errors:?}");
    set
}

/// The live library with [`RULE`] changed: what an inspection that ran on an older library kept.
fn snapshot_rules(live: &camerata_rules::RuleSet) -> camerata_rules::RuleSet {
    let mut out = camerata_rules::RuleSet::default();
    for rule in live.iter() {
        let mut rule = rule.clone();
        if rule.id.0 == RULE {
            rule.title = snapshot_title();
            rule.deviation_ceiling = Some("low".to_string());
            let default = rule.default_option.clone().expect("the rule has a default");
            let option = rule
                .options
                .iter_mut()
                .find(|o| o.id == default)
                .expect("the default option");
            assert!(
                !option.remediation.as_deref().unwrap_or("").contains(MARK),
                "the live text must differ"
            );
            option.remediation = Some(snapshot_fix());
            option.finding_headline = Some(snapshot_headline());
            option.effort = Some("high".to_string());
        }
        out.push(rule);
    }
    assert_ne!(
        camerata_rules::corpus_version(&out),
        camerata_rules::corpus_version(live)
    );
    out
}

struct Fx {
    _state_dir: tempfile::TempDir,
    _guard: camerata_test_env::EnvGuard,
    _repo: tempfile::TempDir,
    state: AppState,
    project_id: String,
    run_id: String,
    run_dir: std::path::PathBuf,
}

/// A saved run that loaded `kept`, an app inspection made from it, and a project ready to export.
/// `before_record` runs on the run's folder before the inspection is recorded (to make a run from
/// before snapshots were kept, or a damaged one).
async fn fixture(
    kept: &camerata_rules::RuleSet,
    before_record: impl FnOnce(&std::path::Path),
) -> Fx {
    use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};
    let state_dir = tempfile::tempdir().expect("state dir");
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state_dir.path());
    let repo = tempfile::tempdir().expect("repo");
    let (run_id, run_dir) = {
        let store = RunStore::create(NewRun {
            pin: crate::run_store::engine_pin::EnginePin::for_run(Some(kept), &Default::default()),
            rules: Some(kept),
            project_key: "acme-snapshot".to_string(),
            repos: vec![("acme/portal".to_string(), repo.path().to_path_buf())],
            models: ModelsRecord {
                audit: "claude-a".to_string(),
                calibration: "claude-a".to_string(),
                fix: "claude-a".to_string(),
                backend: "api".to_string(),
                mode: "parallel".to_string(),
                resolved: true,
                group_review: String::new(),
                finder: String::new(),
                finder_model: String::new(),
                scan_exclusions: Vec::new(),
            },
            ai_scan_key: crate::scan_cache::AiScanKey {
                audit_model: "claude-a".to_string(),
                calibration_model: String::new(),
                prompt_schema_version: 2,
                directives_fingerprint: "fp".to_string(),
                terse_output: true,
                applies_to_routing: true,
            },
            args: serde_json::json!({}),
            estimate_usd: None,
            include_alternatives: false,
            origin: RunOrigin::Cockpit,
            project_id: None,
        })
        .expect("the run is created");
        (store.run_id(), store.dir().to_path_buf())
    };
    before_record(&run_dir);
    let mut state = AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
    // A file-backed inspection store, as the app has: the kept copy is a file.
    state.inspections =
        crate::inspections::InspectionStore::at(state_dir.path().join("inspections"));
    let project = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project");
    state
        .projects
        .update(&project.id, |p| {
            p.client_name = "Acme Client Ltd".to_string()
        })
        .expect("project");
    let mut report = crate::release_gate::tests::scan_report();
    report.run_id = Some(run_id.clone());
    state.set_last_scan(project.id.clone(), report);
    Fx {
        _state_dir: state_dir,
        _guard: guard,
        _repo: repo,
        state,
        project_id: project.id,
        run_id,
        run_dir,
    }
}

fn request() -> Json<AuditReportReq> {
    Json(AuditReportReq {
        dispositions: std::collections::HashMap::new(),
        options: crate::report_export::ReportOptions::default(),
    })
}

async fn product(fx: &Fx) -> Response {
    crate::export_product(
        State(fx.state.clone()),
        Path(fx.project_id.clone()),
        request(),
    )
    .await
}

async fn pdf_only(fx: &Fx) -> Response {
    crate::export_audit_report(
        State(fx.state.clone()),
        Path(fx.project_id.clone()),
        request(),
    )
    .await
}

async fn triage_log(fx: &Fx) -> Response {
    crate::export_triage_log(State(fx.state.clone()), Path(fx.project_id.clone())).await
}

async fn bytes_of(resp: Response) -> (axum::http::StatusCode, Vec<u8>) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, bytes.to_vec())
}

fn typst_missing(test: &str) -> bool {
    !camerata_test_env::typst_on_path_for(test)
}

/// The refusal every export answers with when the snapshot cannot be used.
async fn assert_refused(resp: Response, code: &str, says: &str) {
    let (status, bytes) = bytes_of(resp).await;
    assert_eq!(
        status,
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "refused: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a JSON refusal");
    assert_eq!(body["ok"], false, "{body}");
    assert_eq!(body["gate"], "rule_snapshot", "{body}");
    assert_eq!(body["reason"], code, "{body}");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(says),
        "the message says {says:?}: {message}"
    );
}

fn text_of_entries(entries: &std::collections::BTreeMap<String, Vec<u8>>, suffix: &str) -> String {
    let (_, bytes) = entries
        .iter()
        .find(|(name, _)| name.ends_with(suffix))
        .unwrap_or_else(|| panic!("the package carries {suffix}: {:?}", entries.keys()));
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn every_surface_shows_the_snapshot_rules_when_the_live_library_differs() {
    if typst_missing("every_surface_shows_the_snapshot_rules_when_the_live_library_differs") {
        return;
    }
    let live = live_rules().await;
    let fx = fixture(&snapshot_rules(&live), |_| {}).await;

    // The package: PDF, workbook, CSV and JSON.
    let (status, zip) = bytes_of(product(&fx).await).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&zip));
    let entries = crate::release_gate::read_zip_entries(&zip).expect("a zip");
    let (_, pdf) = entries
        .iter()
        .find(|(name, _)| name.ends_with(".pdf"))
        .expect("a PDF");
    let pdf_text = pdf_extract::extract_text_from_mem(pdf).unwrap_or_default();
    assert!(
        pdf_text.contains(MARK),
        "the package PDF shows the snapshot's headline"
    );
    let (_, xlsx) = entries
        .iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .expect("a workbook");
    let cells = crate::release_gate::xlsx_cells(xlsx).expect("cells");
    assert!(
        cells.iter().any(|c| c.text.contains(&snapshot_fix())),
        "the workbook shows the snapshot's fix text"
    );
    let csv = text_of_entries(
        &entries,
        camerata_api_types::package_paths::ALL_FINDINGS_CSV,
    );
    assert!(
        csv.contains(MARK),
        "the CSV shows the snapshot's rules: {csv}"
    );
    let json = text_of_entries(&entries, camerata_api_types::package_paths::INSPECTION_JSON);
    assert!(json.contains(MARK), "the JSON shows the snapshot's rules");

    // The PDF on its own.
    let (status, pdf) = bytes_of(pdf_only(&fx).await).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&pdf));
    let pdf_text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(pdf_text.contains(MARK), "the PDF route shows the snapshot");

    // The review rows carry the snapshot's severity bounds.
    let bounds = review_bounds(&fx).await;
    assert_eq!(
        bounds.get("deviation_ceiling").and_then(|v| v.as_str()),
        Some("low"),
        "the review row shows the snapshot's bound: {bounds}"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_triage_log_names_the_rule_by_the_snapshot_title() {
    let live = live_rules().await;
    let fx = fixture(&snapshot_rules(&live), |_| {}).await;
    // The triage log lists the rows the AI pass proposed: the same inspection, its row AI tier.
    let mut report = crate::release_gate::tests::scan_report();
    report.run_id = Some(fx.run_id.clone());
    report.findings[0].confidence = Some("high".to_string());
    fx.state.set_last_scan(fx.project_id.clone(), report);
    let (status, csv) = bytes_of(triage_log(&fx).await).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&csv));
    let csv = String::from_utf8_lossy(&csv);
    assert!(
        csv.contains(&snapshot_title()),
        "the snapshot's title: {csv}"
    );
}

/// The severity bounds of the review row of [`RULE`].
async fn review_bounds(fx: &Fx) -> serde_json::Value {
    let latest = fx
        .state
        .inspections
        .latest(&fx.project_id)
        .expect("the inspection");
    let tree = latest.report.tree.as_ref().expect("a review tree");
    let node = tree
        .nodes
        .iter()
        .find(|n| n.level == crate::review_tree::NodeLevel::Subgroup && n.id.contains(RULE))
        .expect("the rule's sub-group")
        .id
        .clone();
    let app = router(fx.state.clone());
    let uri = format!(
        "/api/projects/{}/review/sites?node={}",
        fx.project_id,
        node.bytes()
            .map(|b| format!("%{b:02X}"))
            .collect::<String>()
    );
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, 200, "{body}");
    body["rows"][0]["severity_bounds"].clone()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_inspection_whose_run_kept_no_snapshot_refuses_and_names_the_backfill_command() {
    let live = live_rules().await;
    let fx = fixture(&live, |run_dir| {
        std::fs::remove_dir_all(run_dir.join(rule_snapshot::SNAPSHOT_DIR)).expect("remove");
    })
    .await;
    let command = format!("camerata run snapshot-rules --run {}", fx.run_id);
    assert_refused(product(&fx).await, "missing", &command).await;
    assert_refused(pdf_only(&fx).await, "missing", &command).await;
    assert_refused(triage_log(&fx).await, "missing", &command).await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_snapshot_with_one_byte_changed_refuses_the_export() {
    let live = live_rules().await;
    // Damaged in the run's folder before the inspection is recorded.
    let damage = |path: &std::path::Path| {
        let mut bytes = std::fs::read(path).expect("read");
        let at = String::from_utf8_lossy(&bytes)
            .find("parameterized")
            .expect("rule text");
        bytes[at] = b'P';
        std::fs::write(path, bytes).expect("write");
    };
    let fx = fixture(&live, |run_dir| {
        let pinned = camerata_rules::corpus_version(&live);
        damage(&rule_snapshot::path_in(run_dir, &pinned).expect("path"));
    })
    .await;
    assert_refused(
        product(&fx).await,
        "digest_mismatch",
        "not the rules it ran with",
    )
    .await;
    assert_refused(
        pdf_only(&fx).await,
        "digest_mismatch",
        "not the rules it ran with",
    )
    .await;
    assert_refused(triage_log(&fx).await, "digest_mismatch", "not the rules").await;

    // Damaged in the copy kept beside the inspection after it was recorded.
    let fx = fixture(&live, |_| {}).await;
    let pinned = camerata_rules::corpus_version(&live);
    let kept = fx
        .state
        .inspections
        .rule_snapshot_bytes(&pinned)
        .expect("readable")
        .expect("the inspection keeps a copy");
    let mut bytes = kept.to_vec();
    let at = String::from_utf8_lossy(&bytes)
        .find("parameterized")
        .expect("rule text");
    bytes[at] = b'P';
    fx.state
        .inspections
        .keep_rule_snapshot(&pinned, &bytes)
        .expect("replace the copy");
    assert_refused(
        product(&fx).await,
        "digest_mismatch",
        "not the rules it ran with",
    )
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_inspection_with_no_saved_run_refuses() {
    let live = live_rules().await;
    let fx = fixture(&live, |_| {}).await;
    // A report with no run (one written before runs were saved).
    let mut report = crate::release_gate::tests::scan_report();
    report.run_id = None;
    fx.state.set_last_scan(fx.project_id.clone(), report);
    assert_refused(product(&fx).await, "no_run", "no saved run").await;
    assert_refused(pdf_only(&fx).await, "no_run", "no saved run").await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_inspection_keeps_its_rules_after_its_run_folder_is_gone() {
    if typst_missing("the_inspection_keeps_its_rules_after_its_run_folder_is_gone") {
        return;
    }
    let live = live_rules().await;
    let fx = fixture(&snapshot_rules(&live), |_| {}).await;
    // Source deletion removes the run's folder; the inspection is what exports are built from.
    std::fs::remove_dir_all(&fx.run_dir).expect("remove the run");
    let (status, pdf) = bytes_of(pdf_only(&fx).await).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&pdf));
    let pdf_text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(pdf_text.contains(MARK), "the PDF still shows the snapshot");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_snapshot_saved_into_the_run_later_is_adopted_by_the_inspection() {
    if typst_missing("a_snapshot_saved_into_the_run_later_is_adopted_by_the_inspection") {
        return;
    }
    let live = live_rules().await;
    let kept = snapshot_rules(&live);
    // The inspection is recorded while its run holds no snapshot (a run made before snapshots
    // were kept), so the inspection keeps no copy and the export refuses.
    let fx = fixture(&kept, |run_dir| {
        std::fs::remove_dir_all(run_dir.join(rule_snapshot::SNAPSHOT_DIR)).expect("remove");
    })
    .await;
    assert_refused(pdf_only(&fx).await, "missing", "snapshot-rules").await;
    // The snapshot is saved into the run's folder afterwards (what the backfill does).
    rule_snapshot::write_in(&fx.run_dir, &kept).expect("write");
    let (status, pdf) = bytes_of(pdf_only(&fx).await).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&pdf));
    // The export kept a copy beside the inspection: it still exports once the run is gone.
    std::fs::remove_dir_all(&fx.run_dir).expect("remove the run");
    let (status, pdf) = bytes_of(pdf_only(&fx).await).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&pdf));
    let pdf_text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(
        pdf_text.contains(MARK),
        "the adopted snapshot is the one shown"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn when_the_kept_copy_is_gone_the_run_folder_still_answers() {
    let live = live_rules().await;
    let fx = fixture(&snapshot_rules(&live), |_| {}).await;
    let digest = fx
        .state
        .inspections
        .rule_digest_for_run(&fx.project_id, Some(&fx.run_id))
        .expect("the inspection names its rules");
    let copy = fx
        .state
        .inspections
        .rule_snapshot_path(&digest)
        .expect("a file");
    std::fs::remove_file(&copy).expect("remove the copy");
    let report = fx.state.get_last_scan(&fx.project_id).expect("report");
    let rules = crate::export_rules::rules_for_export(&fx.state, &fx.project_id, &report)
        .expect("the run's folder answers");
    assert_eq!(camerata_rules::corpus_version(&rules), digest);
    assert!(copy.is_file(), "the copy is kept again");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_finding_modal_fix_comes_from_the_snapshot() {
    let live = live_rules().await;
    let fx = fixture(&snapshot_rules(&live), |_| {}).await;
    assert!(fx.state.projects.set_active(&fx.project_id));
    let app = router(fx.state.clone());
    for uri in [
        format!(
            "/api/onboard/finding-fix?rule_id={RULE}&project_id={}",
            fx.project_id
        ),
        // No project named: the active project's inspection.
        format!("/api/onboard/finding-fix?rule_id={RULE}"),
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = body_json(resp).await;
        assert!(
            body["fix"].as_str().is_some_and(|f| f.contains(MARK)),
            "{uri}: {body}"
        );
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_rewrite_advisory_reads_the_snapshot() {
    let live = live_rules().await;
    let kept = snapshot_rules(&live);
    let fx = fixture(&kept, |_| {}).await;
    let latest = fx
        .state
        .inspections
        .latest(&fx.project_id)
        .expect("the inspection");
    let from_snapshot = crate::rewrite_advisory::compute(&latest.report, Some(&kept));
    assert_ne!(
        from_snapshot,
        crate::rewrite_advisory::compute(&latest.report, Some(&live)),
        "the snapshot's authored effort changes the advisory"
    );
    let resp = router(fx.state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{}/rewrite-advisory", fx.project_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    let doc: crate::rewrite_advisory::AdvisoryDocument =
        serde_json::from_value(body).expect("the advisory document");
    assert_eq!(doc.rewrite_advisory, from_snapshot);
}
