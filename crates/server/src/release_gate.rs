//! The product-export assembly, and the release gate that stands between it and the client
//! (docs/design/2026-10-23_secret-redaction-and-release-gate.md section 7).
//!
//! One path to zip bytes: [`build_product_package`] assembles everything one product export
//! consists of (the PDF's data and the compiled PDF, the workbook, `findings.json`, the CSV and
//! JSON datasets, the README and the provenance), and [`release`] is the ONLY function the three
//! real exporters (the HTTP `export_product`, `camerata inspect --export`, the sample generator)
//! call to obtain zip bytes. [`release`] zips the package, then runs every check over the
//! archive it just built, so what is checked is the exact bytes that would ship, and refuses it
//! if any check finds a problem. A source-grep test pins that no other production code calls
//! `build_product_zip(` or `compile_pdf_from_data(`.
//!
//! The checks, in order: secrets, dataset set, dataset parity, row parity, severity and kind
//! counts, confidentiality marking, manifest, provenance
//! (see [`CheckId`]). The comparison logic lives in [`consistency`]; the export tests call those
//! same functions, so the test and the runtime check cannot drift.

mod archive;
mod consistency;
mod report;
mod secrets;

use std::collections::HashMap;

use crate::onboard::SealedReport;
use crate::report_export::{AuditReportJson, DispositionWire, ReportOptions};
use crate::xlsx_export::FindingsExport;
use crate::ManifestProvenance;

pub use archive::{
    read_zip_entries, xlsx_cells, xlsx_header_footers, xlsx_sheet_grid, xlsx_sheet_names,
    xlsx_strings, XlsxCell, XlsxHeaderFooter,
};
pub use consistency::{
    compare_csv_to_json, count_reconciliation_problems, dataset_parity_problems,
    dataset_set_problems, marking_problems, package_stem, parse_readme_provenance,
    patterns_section_problems, pdf_count_reconciliation_problems, pdf_marking_problems, row_parity,
    verify_manifest, verify_readme_provenance, ReadmeProvenance,
};
pub use report::{CheckId, CheckOutcome, GateFailure, GateReport, Problem};

/// Everything one product export consists of, built by [`build_product_package`] and nothing else.
pub struct ProductPackage {
    /// `camerata-inspection-{repo}-{sha}`: the shared filename stem.
    pub stem: String,
    /// The PDF's input data, structured.
    pub report_json: AuditReportJson,
    /// The EXACT bytes `compile_pdf_from_data` handed to Typst. The template's only data source
    /// is this string, so every client-visible dynamic string in the PDF is in it.
    pub data_json: String,
    /// The compiled PDF.
    pub pdf: Vec<u8>,
    /// The workbook.
    pub xlsx: Vec<u8>,
    /// The rows behind `findings.json` (and the workbook's "All Findings" sheet).
    pub findings_export: FindingsExport,
    /// `findings.json`, serialized.
    pub findings_json: Vec<u8>,
    /// `csv/*.csv` then `json/*.json`.
    pub datasets: Vec<(String, Vec<u8>)>,
    /// `README.txt`.
    pub readme: String,
    /// The provenance the README and `manifest.json` both render.
    pub provenance: ManifestProvenance,
    /// The package is for the operator's own review. Only an internal package may record an
    /// incomplete run, and only [`release_internal`] releases it.
    pub internal: bool,
}

/// THE assembly: `build_report_json`, `render_data_json`, `compile_pdf_from_data`,
/// `build_workbook`, `build_findings_export`, the CSV and JSON datasets, `export_provenance`
/// and `product_export_readme`, in that order, over one sealed report. Replaces the three
/// hand-rolled copies the exporters used to carry.
///
/// Errors carry the same plain-language messages the HTTP route has always returned (a missing
/// Typst names the fix), so callers surface `e.to_string()` unchanged.
pub async fn build_product_package(
    report: &SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    options: &ReportOptions,
) -> anyhow::Result<ProductPackage> {
    let stem = crate::report_filename_stem_for(report, &options.project_title);
    build_product_package_with_stem(report, dispositions, corpus, options, &stem).await
}

/// [`build_product_package`] under an explicit filename stem (the sample generator ships
/// `camerata-sample-audit`). The stem is the only difference; the assembly is the same code.
pub async fn build_product_package_with_stem(
    report: &SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    options: &ReportOptions,
    stem: &str,
) -> anyhow::Result<ProductPackage> {
    // Confidentiality (2026-10-24 decision): no package without a client name to mark it
    // with. Checked first, before anything is built or compiled. Callers surface the error's
    // message unchanged (it is a `report_export::ClientNameRequired`).
    crate::report_export::require_client_name(options)?;
    // And no package for a report that covers fewer repos than the project holds.
    crate::report_export::require_all_repos(report, options)?;
    // And no client package for a run that did not run to completion.
    crate::report_export::require_complete_run(report, options)?;
    let report_json =
        crate::report_export::build_report_json(report, dispositions, corpus, options);
    let data_json = crate::report_export::render_data_json(&report_json)?;
    // The secrets check over the PDF's data runs BEFORE the compile: Typst gets a temp dir, and
    // a value that would be refused must never be written to disk, let alone compiled.
    check_pdf_data(stem, &data_json).map_err(ReleaseError::Blocked)?;
    let pdf = crate::report_export::compile_pdf_from_data(&data_json).await?;
    assemble_package(
        report,
        dispositions,
        corpus,
        options,
        stem,
        (report_json, data_json, pdf),
    )
}

/// The rest of the assembly, around an already-compiled PDF. Split out so the gate's own tests
/// can build a real package (real workbook, real datasets, real provenance) with a stand-in
/// PDF when Typst is not installed; production reaches it only through
/// [`build_product_package_with_stem`].
pub(crate) fn assemble_package(
    report: &SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    options: &ReportOptions,
    stem: &str,
    (report_json, data_json, pdf): (AuditReportJson, String, Vec<u8>),
) -> anyhow::Result<ProductPackage> {
    let xlsx = crate::xlsx_export::build_workbook(report, dispositions, corpus, options)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // `findings.json`: the machine-readable sibling of the workbook's "All Findings" sheet,
    // built from the SAME report/dispositions/corpus pass (and `report_json`'s cover and
    // executive summary), so it can never disagree with the PDF or the workbook.
    let findings_export = crate::xlsx_export::build_findings_export(
        report,
        dispositions,
        corpus,
        &report_json,
        &options.chosen_options,
    );
    let findings_json = serde_json::to_vec_pretty(&findings_export)
        .map_err(|e| anyhow::anyhow!("could not serialize findings.json: {e}"))?;

    // The per-dataset machine-readable tables: the SAME `partition_rows` pass as the workbook
    // and findings.json, so every artifact agrees. CSV first, then JSON.
    let csv = crate::xlsx_export::build_csv_exports(report, dispositions, corpus, options)
        .map_err(|e| anyhow::anyhow!("could not build the machine-readable exports: {e}"))?;
    let json = crate::xlsx_export::build_json_exports(report, dispositions, corpus, options)
        .map_err(|e| anyhow::anyhow!("could not build the machine-readable exports: {e}"))?;
    let mut datasets = csv;
    datasets.extend(json);

    // Commit per repo, the corpus digest and the rules-last-reviewed date all come from the one
    // sanctioned provenance bridge (`client_provenance`).
    let mut provenance =
        crate::client_provenance::export_provenance(report, &report_json.cover, corpus);
    // The models are what the scan recorded as having run, never what was configured.
    provenance.model = options
        .model
        .clone()
        .map(|m| m.with_models_from(&report.provenance));
    let client = crate::report_export::clean_client_name(&options.client_name);
    provenance.client_name = (!client.is_empty()).then_some(client);
    let readme = crate::product_export_readme(stem, &provenance);

    Ok(ProductPackage {
        stem: stem.to_string(),
        report_json,
        data_json,
        pdf,
        xlsx,
        findings_export,
        findings_json,
        datasets,
        readme,
        provenance,
        internal: options.internal,
    })
}

/// A package that cleared release: the zip a client may receive, and the gate's record that it
/// ran (six checks, no problems).
#[derive(Debug)]
pub struct ReleasedZip {
    pub stem: String,
    pub bytes: Vec<u8>,
    pub report: GateReport,
}

impl ReleasedZip {
    /// What to remember about this package on the inspection it was made from: when, the
    /// SHA-256 of the exact bytes delivered, and the manifest inside them (file names and
    /// hashes), so the package can be proven later without the zip. A manifest that cannot be
    /// read back is recorded as `null` rather than failing the export: the hash still proves
    /// the bytes.
    pub fn package_record(&self) -> crate::inspections::PackageRecord {
        use sha2::{Digest, Sha256};
        let zip_sha256: String = Sha256::digest(&self.bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let manifest_json = read_zip_entries(&self.bytes)
            .ok()
            .and_then(|entries| entries.get("manifest.json").cloned())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(serde_json::Value::Null);
        crate::inspections::PackageRecord {
            generated_at: crate::client_source::records::now_rfc3339(),
            zip_sha256,
            manifest_json,
        }
    }
}

/// Why [`release`] produced no zip.
#[derive(Debug)]
pub enum ReleaseError {
    /// The archive could not be assembled (an I/O error writing the in-memory zip).
    Assembly(String),
    /// The gate refused the package. Nothing was returned.
    Blocked(GateFailure),
}

impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseError::Assembly(e) => {
                write!(f, "could not assemble the product export zip: {e}")
            }
            ReleaseError::Blocked(failure) => write!(f, "{failure}"),
        }
    }
}

impl std::error::Error for ReleaseError {}

/// The gate's refusal, when `error` (from [`build_product_package`]) is one. The assembly runs the
/// secrets check over the PDF's data before the PDF is compiled, so a refusal can surface there
/// as well as from [`release`]; callers map it to the same blocked response.
pub fn gate_failure_of(error: &anyhow::Error) -> Option<&GateFailure> {
    match error.downcast_ref::<ReleaseError>() {
        Some(ReleaseError::Blocked(failure)) => Some(failure),
        _ => None,
    }
}

/// Run every gate check over an EXISTING zip (the acceptance run and `camerata release-gate`):
/// secrets over every text member, every workbook cell and string, and `pdf_text` when the
/// caller extracted it; the dataset set; CSV against JSON; the manifest; the provenance. It
/// cannot run row parity (that needs the in-memory rows), so that check is absent from the
/// report.
pub fn audit_zip(zip_bytes: &[u8], pdf_text: Option<&str>) -> GateReport {
    audit_zip_with(zip_bytes, pdf_text, false)
}

/// [`audit_zip`], optionally letting a package that records an incomplete run through (the
/// internal export of such a run; nothing else passes it).
fn audit_zip_with(zip_bytes: &[u8], pdf_text: Option<&str>, allow_incomplete: bool) -> GateReport {
    let mut report = GateReport::default();
    let entries = match read_zip_entries(zip_bytes) {
        Ok(entries) => entries,
        Err(e) => {
            report.record(
                CheckId::DatasetSet,
                vec![Problem::new("(package)", "archive", e, None)],
            );
            return report;
        }
    };
    report.record(
        CheckId::Secrets,
        archive_secret_problems(&entries, pdf_text),
    );
    report.record(CheckId::DatasetSet, dataset_set_problems(&entries));
    report.record(CheckId::DatasetParity, dataset_parity_problems(&entries));
    report.record(CheckId::Counts, count_reconciliation_problems(&entries));
    report.record(CheckId::Marking, marking_problems(&entries));
    let mut manifest_problems = verify_manifest(&entries);
    if !allow_incomplete {
        manifest_problems.extend(incomplete_run_problems(&entries));
    }
    report.record(CheckId::Manifest, manifest_problems);
    report.record(CheckId::Provenance, provenance_problems(&entries));
    report.sorted()
}

/// The check that a package does not record an inspection that failed to run to completion: such
/// a package is for the operator's own review, never for a client. Reads the manifest's
/// `inspection.run_incomplete` flag, the README's INTERNAL INCOMPLETE line and the marking in
/// `findings.json`, none of which a complete run carries, so a package edited to drop one of them
/// is still refused.
fn incomplete_run_problems(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let refusal = |file: &str, reasons: &str| {
        Problem::new(
            file,
            "run complete",
            format!(
                "this package records an inspection that did not run to completion, so it is not \
                 for a client ({reasons})"
            ),
            None,
        )
    };
    let json_of = |name: &str| {
        entries
            .get(name)
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
    };
    if let Some(manifest) = json_of("manifest.json") {
        if manifest["inspection"]["run_incomplete"].as_bool() == Some(true) {
            let reasons = manifest["inspection"]["incomplete_reasons"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            problems.push(refusal("manifest.json", &reasons));
        }
    }
    if let Some(readme) = entries
        .get("README.txt")
        .and_then(|b| std::str::from_utf8(b).ok())
    {
        if readme.contains(crate::INTERNAL_INCOMPLETE_README_LINE) {
            problems.push(refusal("README.txt", "the README says INTERNAL INCOMPLETE"));
        }
    }
    if let Some(findings) = json_of("findings.json") {
        if let Some(marking) = findings["provenance"]["internal_incomplete"].as_str() {
            problems.push(refusal("findings.json", marking));
        }
    }
    problems
}

/// The secrets check over every archive member, by kind, and over the member NAMES.
fn archive_secret_problems(
    entries: &std::collections::BTreeMap<String, Vec<u8>>,
    pdf_text: Option<&str>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (name, bytes) in entries {
        problems.extend(secrets::text_problems("(archive member name)", name));
        if name.ends_with(".xlsx") {
            problems.extend(secrets::xlsx_problems(name, bytes));
        } else if name.ends_with(".json") {
            problems.extend(secrets::json_bytes_problems(name, bytes));
        } else if name.ends_with(".csv") {
            problems.extend(secrets::csv_problems(name, bytes));
        } else if name.ends_with(".pdf") {
            if let Some(text) = pdf_text {
                problems.extend(secrets::pdf_text_problems(name, text));
            }
        } else {
            problems.extend(secrets::text_problems(
                name,
                &String::from_utf8_lossy(bytes),
            ));
        }
    }
    problems
}

fn provenance_problems(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<Problem> {
    let (Some(readme), Some(manifest)) = (entries.get("README.txt"), entries.get("manifest.json"))
    else {
        // The dataset-set check reports the missing file.
        return Vec::new();
    };
    match serde_json::from_slice::<serde_json::Value>(manifest) {
        Ok(manifest) => verify_readme_provenance(&String::from_utf8_lossy(readme), &manifest),
        Err(_) => vec![Problem::new(
            "manifest.json",
            "manifest",
            "manifest.json does not parse",
            None,
        )],
    }
}

/// The secrets check over the PDF's data: `data.json` is the only data the Typst template reads,
/// so every client-visible dynamic string in the PDF is in it.
fn pdf_data_problems(data_json: &str) -> Vec<Problem> {
    secrets::json_bytes_problems("PDF data (data.json)", data_json.as_bytes())
}

/// Zip the package and run every check over the archive it produced. This is the ONLY function
/// the real exporters call to obtain zip bytes: `Ok` only when every check passed, and on
/// failure no bytes are returned.
pub fn release(package: ProductPackage) -> Result<ReleasedZip, ReleaseError> {
    release_checked(package, false)
}

/// [`release`] for an INTERNAL package: one marked for the operator's own review, which may
/// record a run that did not complete. Everything else is checked exactly as for a client; a
/// package that is not internal is refused if it records an incomplete run, so this cannot be
/// used to put one in front of a client.
pub fn release_internal(package: ProductPackage) -> Result<ReleasedZip, ReleaseError> {
    release_checked(package, true)
}

fn release_checked(
    package: ProductPackage,
    internal_ok: bool,
) -> Result<ReleasedZip, ReleaseError> {
    let allow_incomplete = internal_ok && package.internal;
    let bytes = crate::build_product_zip(
        &package.stem,
        &package.pdf,
        &package.xlsx,
        &package.findings_json,
        &package.readme,
        &package.datasets,
        &package.provenance,
    )
    .map_err(|e| ReleaseError::Assembly(e.to_string()))?;

    let mut report = audit_zip_with(&bytes, None, allow_incomplete);
    report.record(CheckId::Secrets, pdf_data_problems(&package.data_json));
    // The archive pass above held the workbook and the CSV to the JSON rows, and the sheet
    // footers to each other; this pass also holds the PDF's rows, cover summary and
    // confidentiality line to them.
    if let Ok(entries) = read_zip_entries(&bytes) {
        report.record(
            CheckId::Counts,
            pdf_count_reconciliation_problems(&entries, &package.report_json),
        );
        report.record(
            CheckId::Marking,
            pdf_marking_problems(&entries, &package.report_json.confidentiality.line),
        );
    }
    report.record(
        CheckId::RowParity,
        row_parity(&package.report_json, &package.findings_export),
    );
    let report = report.sorted();

    if report.is_clean() {
        Ok(ReleasedZip {
            stem: package.stem,
            bytes,
            report,
        })
    } else {
        Err(ReleaseError::Blocked(GateFailure {
            stem: package.stem,
            report,
        }))
    }
}

/// The PDF-only route's gate: the secrets check over `data.json` alone (that route ships no zip,
/// so there is no archive to check).
pub fn check_pdf_data(stem: &str, data_json: &str) -> Result<(), GateFailure> {
    let mut report = GateReport::default();
    report.record(CheckId::Secrets, pdf_data_problems(data_json));
    if report.is_clean() {
        Ok(())
    } else {
        Err(GateFailure {
            stem: stem.to_string(),
            report,
        })
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod triage_log_tests;

#[cfg(test)]
mod run_complete_tests;

/// A clean report, for other modules' tests.
#[cfg(test)]
pub(crate) fn test_scan_report() -> crate::onboard::ScanReport {
    tests::scan_report()
}

/// An internal package made from an incomplete run, for other modules' tests.
#[cfg(test)]
pub(crate) fn test_internal_incomplete_package() -> ProductPackage {
    run_complete_tests::internal_incomplete_package()
}

#[cfg(test)]
mod patterns_tests;

// The enforcement of "the review timer and the per-row reviewed record are INTERNAL ONLY".
#[cfg(test)]
mod internal_only_tests;
