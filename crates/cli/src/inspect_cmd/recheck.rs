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
    run_recheck_saved_with_store(
        &camerata_server::credentials::KeyringCredentialStore,
        run,
        repos,
        export,
        max_cost_usd,
        reask,
    )
    .await
}

/// [`run_recheck_saved`] with the credential store handed in.
pub async fn run_recheck_saved_with_store(
    store: &dyn camerata_server::credentials::CredentialStore,
    run: &str,
    repos: &[PathBuf],
    export: Option<PathBuf>,
    max_cost_usd: Option<f64>,
    reask: Option<usize>,
) -> Result<InspectOutcome, InspectError> {
    recheck_saved_inspect(
        run,
        repos,
        export,
        max_cost_usd,
        reask,
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

/// The model of a recheck run with `--reask-failed-judge`, built on its first call. Until then the
/// credential store is never asked: a recheck that settles everything from the run's stored
/// replies makes no call and needs no key (#604). The first call prints one line saying a key is
/// needed and why, reads the key, applies the client-engagement key rule and builds the real model.
struct LazyLlm<'a> {
    creds: &'a dyn camerata_server::credentials::CredentialStore,
    backend: camerata_server::llm::ProjectBackend,
    client_engagement: bool,
    usage: Arc<camerata_server::usage_ledger::UsageLedger>,
    notice: &'a (dyn Fn(&str) + Sync),
    built: std::sync::OnceLock<Result<camerata_server::llm::Llm, String>>,
}

impl LazyLlm<'_> {
    fn model(&self) -> anyhow::Result<&camerata_server::llm::Llm> {
        let built = self.built.get_or_init(|| {
            (self.notice)(
                "A clone check part needs a new model call (its stored reply cannot be read), so \
                 the API key is read from the credential store now.\n",
            );
            let has_api_key = api_key_present(self.creds);
            if self.client_engagement {
                super::require_api_key_for_client_inspection(self.backend, has_api_key)
                    .map_err(|e| e.to_string())?;
            }
            let backend = match camerata_server::llm::resolve_backend(self.backend, has_api_key) {
                camerata_server::llm::BackendResolution::Api => camerata_server::llm::Backend::Api,
                _ => camerata_server::llm::Backend::Cli,
            };
            Ok(camerata_server::onboard::build_llm(
                self.creds,
                backend,
                Some(self.usage.clone()),
            ))
        });
        built.as_ref().map_err(|e| anyhow::anyhow!("{e}"))
    }
}

#[async_trait::async_trait]
impl camerata_server::llm::LlmPort for LazyLlm<'_> {
    async fn complete(
        &self,
        req: camerata_server::llm::LlmRequest,
    ) -> anyhow::Result<camerata_server::llm::LlmResponse> {
        camerata_server::llm::LlmPort::complete(self.model()?, req).await
    }
    async fn complete_streaming(
        &self,
        req: camerata_server::llm::LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<camerata_server::llm::LlmResponse> {
        camerata_server::llm::LlmPort::complete_streaming(self.model()?, req, on_delta).await
    }
    /// The recheck never batches, so nothing downcasts this to the concrete model.
    fn as_any(&self) -> &dyn std::any::Any {
        &()
    }
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

    // A model is handed to the recheck only when the flag could use one, and even then it is built
    // (and the credential read) by its first call, which the recheck makes only once a failed
    // clone check part is confirmed to need one (#604).
    let usage = Arc::new(camerata_server::usage_ledger::UsageLedger::new());
    let lazy = match (reask, seams.llm) {
        (Some(_), None) => Some(LazyLlm {
            creds,
            backend: args.backend,
            client_engagement: args.client_engagement,
            usage,
            notice: seams.preflight_sink,
            built: std::sync::OnceLock::new(),
        }),
        _ => None,
    };
    let llm: Option<&dyn camerata_server::llm::LlmPort> = match (reask, seams.llm, lazy.as_ref()) {
        (None, _, _) => None,
        (Some(_), Some(scripted), _) => Some(scripted),
        (Some(_), None, Some(lazy)) => Some(lazy),
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

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_server::credentials::{CredentialError, CredentialStore};
    use camerata_server::llm::{LlmPort, LlmRequest, ProjectBackend};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct Counting(AtomicUsize);

    impl CredentialStore for Counting {
        fn set(&self, _: &str, _: &str) -> Result<(), CredentialError> {
            Ok(())
        }
        fn get(&self, _: &str) -> Result<Option<String>, CredentialError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
        fn delete(&self, _: &str) -> Result<(), CredentialError> {
            Ok(())
        }
    }

    /// The key is read by the first call and not before: building the model asks nothing, the
    /// first call prints one line saying why a key is needed and then reads it, and a later call
    /// neither prints again nor fails differently.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_key_is_read_only_by_the_first_call_after_one_printed_line() {
        let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
        let _no_key = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        let store = Counting(AtomicUsize::new(0));
        let said: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let sink = |text: &str| said.lock().unwrap().push(text.to_string());
        let lazy = LazyLlm {
            creds: &store,
            backend: ProjectBackend::Api,
            client_engagement: true,
            usage: Arc::new(camerata_server::usage_ledger::UsageLedger::new()),
            notice: &sink,
            built: std::sync::OnceLock::new(),
        };
        assert_eq!(store.0.load(Ordering::SeqCst), 0, "built, nothing asked");
        assert!(said.lock().unwrap().is_empty());
        let first = lazy.complete(LlmRequest::new("x".to_string())).await;
        assert!(first.is_err(), "a client run with no key refuses the call");
        assert!(store.0.load(Ordering::SeqCst) >= 1, "the key was read");
        assert_eq!(said.lock().unwrap().len(), 1, "one line, once");
        assert!(said.lock().unwrap()[0].contains("API key"));
        let asked = store.0.load(Ordering::SeqCst);
        let _ = lazy.complete(LlmRequest::new("y".to_string())).await;
        assert_eq!(store.0.load(Ordering::SeqCst), asked, "not read again");
        assert_eq!(said.lock().unwrap().len(), 1, "not said again");
    }
}
