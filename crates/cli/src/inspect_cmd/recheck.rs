//! `camerata inspect --recheck-saved <RUN>`: finish a saved, incomplete inspection on this build by
//! re-checking and re-exporting from its saved report, using every saved model result as it was
//! saved. The work is `camerata_server::run_store::recheck`; this file derives its inputs the way
//! the inspection did, calls it, and exports through the same release gate a normal run does. If
//! the gate refuses the command stops with its refusal and writes no export.

use std::path::PathBuf;
use std::sync::Arc;

use camerata_server::run_store as rs;
use camerata_server::run_store::recheck::{
    recheck_saved_run, ReaskFlag, RecheckError, RecheckInputs, Rechecked,
};

use super::{
    api_key_present, finish_run, release_product_export, render_ledger_summary, save_ledger,
    InspectArgs, InspectError, InspectOutcome, Seams,
};

/// Production entry point: the key and the credential store are the real ones. `reask` is the
/// count given to `--reask-failed-judge` (`None` when the flag is not given).
pub async fn run_recheck_saved(
    run: &str,
    repos: &[PathBuf],
    export: Option<PathBuf>,
    max_cost_usd: Option<f64>,
    reask: Option<usize>,
) -> Result<InspectOutcome, InspectError> {
    let store = &camerata_server::credentials::KeyringCredentialStore;
    let has_api_key = api_key_present(store);
    recheck_saved_inspect(
        run,
        repos,
        export,
        max_cost_usd,
        reask,
        has_api_key,
        store,
        Seams {
            resume: None,
            llm: None,
            preflight_sink: &|text| print!("{text}"),
            tools: None,
        },
    )
    .await
}

fn failed(e: RecheckError) -> InspectError {
    match e {
        RecheckError::AlreadyComplete(m) => InspectError::AlreadyComplete(m),
        RecheckError::Refused(m) => InspectError::Failed(m),
    }
}

/// [`run_recheck_saved`] with its credential store and seams explicit. A model handed in through
/// `seams.llm` is the only model the recheck may ask, and only for a clone check part the flag
/// allows.
#[allow(clippy::too_many_arguments)]
pub async fn recheck_saved_inspect(
    run: &str,
    repos: &[PathBuf],
    export: Option<PathBuf>,
    max_cost_usd: Option<f64>,
    reask: Option<usize>,
    has_api_key: bool,
    creds: &dyn camerata_server::credentials::CredentialStore,
    seams: Seams<'_>,
) -> Result<InspectOutcome, InspectError> {
    let project_key = match repos {
        [] => None,
        [one] => Some(camerata_server::scan_cache::state_dir::repo_state_key(one)),
        many => Some(camerata_server::scan_cache::state_dir::project_state_key(
            many,
        )),
    };
    let run_id = rs::resolve_run(run, project_key.as_deref(), rs::RunOrigin::Cli)
        .map_err(|e| InspectError::Failed(e.to_string()))?;
    let record = rs::read_record(&run_id).map_err(|e| InspectError::Failed(e.to_string()))?;
    if record.origin == rs::RunOrigin::Cockpit {
        return Err(InspectError::Failed(format!(
            "run {run_id} was started from the cockpit; `camerata inspect --recheck-saved` \
             rechecks runs started from the command line"
        )));
    }
    let mut args: InspectArgs = serde_json::from_value(record.args.clone()).map_err(|e| {
        InspectError::Failed(format!(
            "run {run_id} did not save the arguments it was started with ({e}); it cannot be \
             rechecked"
        ))
    })?;
    if let Some(export) = export {
        args.export = export;
    }
    if max_cost_usd.is_some() {
        args.max_cost_usd = max_cost_usd;
    }
    // The export is written last, to a folder that must be there: refused before anything is read
    // or changed.
    let export_dir = args
        .export
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if !export_dir.is_dir() {
        return Err(InspectError::Failed(format!(
            "the folder for the export ({}) does not exist; make it first. Nothing was read or \
             changed.",
            export_dir.display()
        )));
    }
    let mut controls = args.run_controls()?;
    let sources = args.resolve_sources()?;
    let names: Vec<String> = sources.iter().map(|(n, _)| n.clone()).collect();
    controls.scope = controls
        .scope
        .clone()
        .check_names(&names)
        .map_err(|why| InspectError::Failed(format!("--exclude: {why}")))?;

    // The rules the run selected and the library they came from, derived the way the inspection
    // derived them (the tree is unchanged, which the recheck verifies before it relies on this).
    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new(), &controls.scope).await;
    let selected = super::curated_rule_selection(&scan.proposed_rules);
    let (_ai_only, _excluded, _preview_rules, corpus, _load_failures) =
        camerata_server::split_scannable_rules(selected.clone()).await;
    let pin = rs::engine_pin::EnginePin::for_run(corpus.as_ref(), &controls);
    let (_manifest, ledger_file) = args.state_paths();

    // A model is built only when the flag could use one; a client run needs a key for it.
    let usage = Arc::new(camerata_server::usage_ledger::UsageLedger::new());
    let real = match (reask, seams.llm) {
        (Some(_), None) => {
            super::require_api_key_for_client_inspection(args.backend, has_api_key).or_else(
                |e| {
                    if args.client_engagement {
                        Err(e)
                    } else {
                        Ok(())
                    }
                },
            )?;
            let backend = match camerata_server::llm::resolve_backend(args.backend, has_api_key) {
                camerata_server::llm::BackendResolution::Api => camerata_server::llm::Backend::Api,
                _ => camerata_server::llm::Backend::Cli,
            };
            Some(camerata_server::onboard::build_llm(
                creds,
                backend,
                Some(usage),
            ))
        }
        _ => None,
    };
    let llm: Option<&dyn camerata_server::llm::LlmPort> = match (reask, seams.llm, real.as_ref()) {
        (None, _, _) => None,
        (Some(_), Some(scripted), _) => Some(scripted),
        (Some(_), None, Some(real)) => Some(real),
        (Some(_), None, None) => None,
    };
    let inputs = RecheckInputs {
        run_id: run_id.clone(),
        sources: &sources,
        selected: &selected,
        corpus: corpus.as_ref(),
        controls: &controls,
        pin,
        client_engagement: args.client_engagement,
        ledger_file: &ledger_file,
        reask: match reask {
            Some(max_parts) => ReaskFlag::Allow { max_parts },
            None => ReaskFlag::Off,
        },
    };
    let mut preflight = String::new();
    let rechecked = recheck_saved_run(&inputs, llm, &mut |text| {
        (seams.preflight_sink)(text);
        preflight.push_str(text);
    })
    .await
    .map_err(failed)?;
    let Rechecked {
        report,
        store,
        record: rerun,
        lines,
    } = rechecked;
    let failure_report = camerata_server::failure_report::build(&report);
    let failure_lines = failure_report.stderr_lines();
    let ledger_summary = render_ledger_summary(&report.ledger, &report.failed_passes, args.verbose);
    let model_record = camerata_server::ModelRecord::anthropic(args.backend)
        .with_client_engagement(args.client_engagement)
        .with_models_from(&report.provenance);

    // The release gate: bytes come back only when every check passed. A refusal ends the command
    // here: nothing is exported and the saved report is left as the first build wrote it.
    let released = match release_product_export(
        &report,
        corpus.as_ref(),
        Some(model_record),
        &args.client,
        !args.client_engagement,
        &args.repo_dirs(),
    )
    .await
    {
        Ok(released) => released,
        Err(e) => {
            // Nothing is changed: the store puts the run's record back when it is dropped.
            let run_notes = store.record().notes;
            return Err(match e {
                InspectError::Incomplete(refusal) => {
                    InspectError::IncompleteRun(Box::new(super::IncompleteRun {
                        refusal,
                        ledger_summary,
                        ledger_path: ledger_file,
                        failure_lines,
                        run_id: Some(run_id),
                        run_notes,
                    }))
                }
                other => other,
            });
        }
    };

    // The gate passed. The export is written first, to a temporary file beside it and then
    // renamed, and checked; only then do the rechecked report and ledger replace the saved ones.
    // Any failure before that leaves the saved run as it was found.
    let tmp = args.export.with_file_name(format!(
        ".{}.recheck-tmp",
        args.export
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let write_export = || -> std::io::Result<()> {
        std::fs::write(&tmp, &released.bytes)?;
        if std::fs::metadata(&tmp)?.len() != released.bytes.len() as u64 {
            return Err(std::io::Error::other("the export was not written in full"));
        }
        std::fs::rename(&tmp, &args.export)
    };
    if let Err(e) = write_export() {
        let _ = std::fs::remove_file(&tmp);
        return Err(InspectError::Failed(format!(
            "could not write {}: {e}; the saved run was not changed",
            args.export.display()
        )));
    }
    let report_path = store.dir().join(rs::REPORT_FILE);
    if let Err(e) = rs::write_report(&report_path, &report) {
        return Err(InspectError::Failed(format!(
            "the export was written but the rechecked report could not be saved ({}: {e}); the \
             run is left as it was found",
            report_path.display()
        )));
    }
    save_ledger(&ledger_file, &report.ledger);
    if let Err(e) = camerata_server::failure_report::write(&ledger_file, &failure_report) {
        eprintln!("camerata inspect: could not write the failure report: {e}");
    }
    store.keep_changes();
    store.commit_recheck(rerun);
    finish_run(Some(&store), &report, true);
    let run_notes = store.record().notes;
    let release_gate = format!(
        "release gate: passed ({} checks)",
        released.report.checks.len()
    );
    let mut summary = ledger_summary;
    for line in &lines {
        summary.push_str(&format!("\nrecheck: {line}"));
    }
    Ok(InspectOutcome {
        export_path: args.export,
        export_zip_bytes: released.bytes.len(),
        backend: args.backend,
        batch: args.batch,
        actual_usage: report.actual_usage.clone(),
        findings_count: report.findings.len(),
        ledger_summary: summary,
        ledger_path: ledger_file,
        release_gate,
        stage_telemetry: report.ledger.stage_telemetry().cloned(),
        preflight: (!preflight.is_empty()).then_some(preflight),
        efficiency: controls.efficiency,
        cost_cap: report.ledger.cost_cap().cloned(),
        outage: report.ledger.outage().cloned(),
        incomplete_reasons: report.incomplete_reasons(),
        failure_lines,
        run_id: Some(run_id),
        run_notes,
    })
}
