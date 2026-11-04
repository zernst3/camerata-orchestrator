//! Fixtures for the composer's tests: reports with a tree, composed without the stores.

use std::collections::HashMap;

use camerata_api_types::edits::Edit;
use camerata_api_types::project::ReviewSettings;
use camerata_api_types::review_view::{EngineCheckVerdict, MovedFromView};

use super::{ReviewComposer, ReviewInputs};
use crate::onboard::{Finding, ScanReport, SealedReport};
use crate::report_export::ReviewedReport;
use crate::review_tree::KeyLink;

pub(crate) const RUN: &str = "insp-test-1";

/// A row of `rule` at `path:line`.
pub(crate) fn row(rule: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("run(\"{rule}\", {line})"),
        detail: format!("Title of {rule} at {line} \u{2014} The call trusts the caller here."),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

/// A report of `rows`, keyed and with its tree built, then changed by `mutate`, then sealed.
pub(crate) fn sealed(rows: Vec<Finding>, mutate: impl FnOnce(&mut ScanReport)) -> SealedReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.code_lines = 10_000;
    report.findings = rows;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:05:00Z".to_string();
    report.normalize_loaded();
    mutate(&mut report);
    report.seal()
}

/// Everything a composer borrows, owned.
#[derive(Default)]
pub(crate) struct Stores {
    pub settings: ReviewSettings,
    pub edits: Vec<Edit>,
    pub verdicts: Vec<EngineCheckVerdict>,
    pub moved: HashMap<String, MovedFromView>,
    pub links: Vec<KeyLink>,
}

/// Compose over `reviewed` with `stores` and hand the composer to `f`.
pub(crate) fn compose<R>(
    reviewed: &ReviewedReport,
    stores: &Stores,
    f: impl FnOnce(&ReviewComposer<'_>) -> R,
) -> R {
    // The Engine Check's holds are read from the reviewed report, as the routes build it.
    let reviewed =
        &reviewed
            .clone()
            .with_engine_check(crate::engine_check::EngineCheckEvidence::Recorded {
                run_id: RUN.to_string(),
                verdicts: stores.verdicts.clone(),
                edits: stores.edits.clone(),
            });
    let inputs = ReviewInputs {
        run_id: RUN,
        engine_version: "0.0.0-test",
        reviewed,
        edits: &stores.edits,
        settings: &stores.settings,
        scan_model: "claude-opus-5-5",
        calibration_model: "claude-opus-5-5",
        fix_model: "",
        verdicts: &stores.verdicts,
        moved: &stores.moved,
        links: &stores.links,
    };
    f(&ReviewComposer::new(&inputs))
}
