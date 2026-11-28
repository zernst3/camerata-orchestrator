//! The command line saves its sealed report in the run's own folder (#561): after a fresh run and
//! after a resume, as JSON that reads back into the report type and renders the export the run
//! wrote. A report that cannot be saved fails the run and leaves it resumable. The export zip
//! gains nothing.

use super::*;

use camerata_server::onboard::ScanReport;

/// Where the run `run_id` keeps its sealed report.
fn saved_report_of(run_id: &str) -> PathBuf {
    camerata_server::run_store::runs_dir()
        .join(run_id)
        .join("report.json")
}

fn read_saved(run_id: &str) -> (Vec<u8>, ScanReport) {
    let path = saved_report_of(run_id);
    assert!(
        path.is_file(),
        "the sealed report is saved in the run's folder: {}",
        path.display()
    );
    let bytes = std::fs::read(&path).unwrap();
    let report: ScanReport =
        serde_json::from_slice(&bytes).expect("the saved report reads back as the report type");
    (bytes, report)
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_fresh_run_saves_its_sealed_report_in_the_run_folder_and_not_in_the_zip() {
    if !typst_available("a_fresh_run_saves_its_sealed_report") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("run.zip");
    let llm = Scripted::new(Outage::default());
    let outcome = run(&llm, &[repo.path()], &["acme/one"], &export)
        .await
        .expect("the run completes");
    let run_id = outcome.run_id.clone().expect("the run was saved");
    let (bytes, report) = read_saved(&run_id);
    eprintln!("saved report: {} bytes", bytes.len());
    assert_eq!(report.run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(report.findings.len(), outcome.findings_count);
    assert!(report.incomplete_reasons().is_empty());
    let path = saved_report_of(&run_id);
    assert!(
        outcome.ledger_summary.contains(&path.display().to_string()),
        "the summary says where the report is: {}",
        outcome.ledger_summary
    );
    // The client package carries none of it.
    for name in entries(&export).keys() {
        assert!(
            !name.ends_with("report.json"),
            "no saved report in the zip: {name}"
        );
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resumed_run_saves_the_report_of_the_completed_run() {
    if !typst_available("a_resumed_run_saves_the_report") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("resumed.zip");
    let (llm, run_id) = interrupted_run(
        repo.path(),
        &export,
        Outage {
            fix_stage: true,
            ..Outage::default()
        },
    )
    .await;
    // The refused attempt saved the report it sealed, with what did not complete.
    let (_, first) = read_saved(&run_id);
    assert!(!first.incomplete_reasons().is_empty());
    // Gone, so only the resume can put it back.
    std::fs::remove_file(saved_report_of(&run_id)).unwrap();
    let outcome = resume(&llm, &run_id, Some(export.clone()))
        .await
        .expect("the resume completes");
    let (_, resumed) = read_saved(&run_id);
    assert!(
        resumed.incomplete_reasons().is_empty(),
        "{:?}",
        resumed.incomplete_reasons()
    );
    assert_eq!(resumed.findings.len(), outcome.findings_count);
    assert_eq!(resumed.run_id.as_deref(), Some(run_id.as_str()));
}

/// Wall-clock stamps a compiled PDF carries in its own metadata (the XMP packet, `D:` dates and
/// the trailer `/ID`), blanked so two renders of the same data compare equal.
fn pdf_without_clock(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for p in [
        r"(?s)<\?xpacket begin.*?<\?xpacket end[^>]*\?>",
        r"(?s)<x:xmpmeta.*?</x:xmpmeta>",
        r"D:\d{14}(?:[Zz]|[+-]\d{2}'?\d{2}'?)?",
        r"/ID ?\[\s*(?:<[0-9A-Fa-f]*>|\([^)]*\))\s*(?:<[0-9A-Fa-f]*>|\([^)]*\))\s*\]",
    ] {
        out = regex::bytes::Regex::new(p)
            .unwrap()
            .replace_all(&out, &b"<clock>"[..])
            .into_owned();
    }
    out
}

/// The reloaded report, sealed again as the app seals a stored report, renders the export the run
/// wrote from its report in memory: every member, the PDF included.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_saved_report_reloads_and_renders_the_export_the_run_wrote() {
    if !typst_available("the_saved_report_reloads_and_renders") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("run.zip");
    let llm = Scripted::new(Outage::default());
    let outcome = run(&llm, &[repo.path()], &["acme/one"], &export)
        .await
        .expect("the run completes");
    let run_id = outcome.run_id.clone().expect("the run was saved");
    let (bytes, reloaded) = read_saved(&run_id);
    let on_disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        serde_json::to_value(&reloaded).unwrap(),
        on_disk,
        "nothing the file holds is lost on reload"
    );

    // The export assembly `camerata inspect` runs, over the reloaded report.
    let (corpus, _) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    let mut options = camerata_server::report_export::ReportOptions::default();
    options.apply_env_defaults();
    options.model = Some(
        camerata_server::ModelRecord::anthropic(ProjectBackend::Api)
            .with_client_engagement(true)
            .with_models_from(&reloaded.provenance),
    );
    options.client_name = "Example Client Ltd".to_string();
    let sealed = reloaded.seal();
    let dispositions = std::collections::HashMap::new();
    let package = camerata_server::release_gate::build_product_package(
        &camerata_server::report_export::ReviewedReport::unreviewed(&sealed, &dispositions),
        Some(&corpus),
        &options,
    )
    .await
    .expect("the reloaded report assembles");
    let released =
        camerata_server::release_gate::release(package).expect("the reloaded report releases");
    let again = out.path().join("reloaded.zip");
    std::fs::write(&again, &released.bytes).unwrap();

    assert_same_exports(&again, &export);
    let pdfs = |path: &Path| -> Vec<Vec<u8>> {
        entries(path)
            .into_iter()
            .filter(|(name, _)| name.ends_with(".pdf"))
            .map(|(_, b)| pdf_without_clock(&b))
            .collect()
    };
    let (written, rendered) = (pdfs(&export), pdfs(&again));
    assert_eq!(written.len(), 1, "the export has its PDF");
    assert!(
        written == rendered,
        "the PDF rendered from the reloaded report is the PDF the run wrote"
    );
}

/// A report that cannot be saved (here a folder squats on its name, so the rename fails) fails the
/// run with a plain message, writes no export and leaves the run resumable; once the name is free
/// a resume saves it and completes.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_report_that_cannot_be_saved_fails_the_run_and_leaves_it_resumable() {
    if !typst_available("a_report_that_cannot_be_saved") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("resumed.zip");
    let (llm, run_id) = interrupted_run(
        repo.path(),
        &export,
        Outage {
            fix_stage: true,
            ..Outage::default()
        },
    )
    .await;
    let path = saved_report_of(&run_id);
    if path.is_file() {
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("occupied"), b"x").unwrap();

    let err = resume(&llm, &run_id, Some(export.clone()))
        .await
        .expect_err("a run whose report was not saved does not succeed");
    assert_ne!(err.exit_code(), 0, "{err}");
    assert!(
        err.to_string().contains("the report was not saved"),
        "{err}"
    );
    assert!(!export.exists(), "no export without the saved report");
    let record = camerata_server::run_store::read_record(&run_id).unwrap();
    assert_eq!(
        record.status,
        camerata_server::run_store::RunStatus::Incomplete,
        "the run is not complete without its report"
    );

    std::fs::remove_dir_all(&path).unwrap();
    let calls_before = llm.calls();
    let outcome = resume(&llm, &run_id, Some(export.clone()))
        .await
        .expect("the resume saves the report and completes");
    assert_eq!(
        llm.calls(),
        calls_before,
        "the resume after a failed save sends nothing to the model: every reply is stored"
    );
    assert!(outcome.incomplete_reasons.is_empty());
    let (_, report) = read_saved(&run_id);
    assert!(report.incomplete_reasons().is_empty());
    assert!(export.exists());
    let record = camerata_server::run_store::read_record(&run_id).unwrap();
    assert_eq!(
        record.status,
        camerata_server::run_store::RunStatus::Complete
    );
}

/// A run that did not complete (the fix stage is still down) and whose report cannot be saved
/// ends as that run ends, with its own exit code and account, and says the report was not saved
/// too. It stays resumable.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_outage_run_whose_report_cannot_be_saved_keeps_its_own_exit_code_and_says_both() {
    if !typst_available("an_outage_run_whose_report_cannot_be_saved") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("resumed.zip");
    let down = Outage {
        fix_stage: true,
        ..Outage::default()
    };
    let (llm, run_id) = interrupted_run(repo.path(), &export, down).await;
    let path = saved_report_of(&run_id);
    if path.is_file() {
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("occupied"), b"x").unwrap();
    llm.set(down);

    let err = resume(&llm, &run_id, Some(export.clone()))
        .await
        .expect_err("the fix stage is still down");
    assert_eq!(err.exit_code(), EXIT_OUTAGE_STOPPED, "{err}");
    assert!(
        matches!(err, InspectError::IncompleteRun(_)),
        "the run's own account: {err}"
    );
    let text = err.to_string();
    assert!(text.contains("the report was not saved"), "{text}");
    assert!(text.contains("== PIPELINE STAGES =="), "{text}");
    assert!(!export.exists());
    let record = camerata_server::run_store::read_record(&run_id).unwrap();
    assert_eq!(
        record.status,
        camerata_server::run_store::RunStatus::Incomplete
    );
}

/// A client engagement whose run folder cannot be created does not start: nothing is sent to a
/// model, and the message says why.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_run_with_no_run_folder_is_refused_before_any_model_call() {
    if !typst_available("a_client_run_with_no_run_folder") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("run.zip");
    // A file where the runs folder goes: no run folder can be created.
    let runs = camerata_server::run_store::runs_dir();
    std::fs::create_dir_all(runs.parent().unwrap()).unwrap();
    std::fs::write(&runs, b"not a folder").unwrap();
    let llm = Scripted::new(Outage::default());
    let err = run(&llm, &[repo.path()], &["acme/one"], &export)
        .await
        .expect_err("a client engagement with no run folder does not start");
    assert_eq!(llm.calls(), 0, "nothing was sent to a model");
    assert_ne!(err.exit_code(), 0);
    assert!(
        err.to_string().contains("run folder could not be created"),
        "{err}"
    );
    assert!(!export.exists());
}

/// An internal run whose run folder cannot be created still runs, and saves its sealed report
/// beside its ledger.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_internal_run_with_no_run_folder_saves_its_report_beside_the_ledger() {
    if !typst_available("an_internal_run_with_no_run_folder") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("run.zip");
    let runs = camerata_server::run_store::runs_dir();
    std::fs::create_dir_all(runs.parent().unwrap()).unwrap();
    std::fs::write(&runs, b"not a folder").unwrap();
    let llm = Scripted::new(Outage::default());
    let sink = Mutex::new(Vec::new());
    let mut internal = args(&[repo.path()], &["acme/one"], &export);
    internal.client_engagement = false;
    let outcome = run_inspect_pipeline_with(internal, true, &store(), seams(&llm, &sink))
        .await
        .expect("an internal run goes on without a run folder");
    assert!(outcome.run_id.is_none(), "no run was saved");
    let ledger = camerata_server::scan_cache::state_dir::ledger_path(repo.path());
    let name = ledger.file_name().unwrap().to_string_lossy().into_owned();
    let saved = ledger.with_file_name(name.replace(".ledger.json", ".report.json"));
    assert!(
        saved.is_file(),
        "the report is beside the ledger: {}",
        saved.display()
    );
    let report: ScanReport = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
    assert_eq!(report.findings.len(), outcome.findings_count);
}
