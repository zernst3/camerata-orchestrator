//! The product-export assembly, and the release gate that stands between it and the client
//! (docs/design/2026-10-23_secret-redaction-and-release-gate.md section 7).
//!
//! One path to zip bytes: [`build_product_package`] assembles everything one product export
//! consists of (the PDF's data and the compiled PDF, the workbook, `findings.json`, the CSV and
//! JSON datasets, the README and the provenance), and [`release`] is the ONLY function the three
//! real exporters (the HTTP `export_product`, `camerata inspect --export`, the sample generator)
//! call to obtain zip bytes. A source-grep test pins that.

use std::collections::HashMap;

use crate::onboard::SealedReport;
use crate::report_export::{AuditReportJson, DispositionWire, ReportOptions};
use crate::xlsx_export::FindingsExport;
use crate::ManifestProvenance;

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
    let stem = crate::report_filename_stem(report);
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
    let report_json =
        crate::report_export::build_report_json(report, dispositions, corpus, options);
    let data_json = crate::report_export::render_data_json(&report_json)?;
    let pdf = crate::report_export::compile_pdf_from_data(&data_json).await?;

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
    let provenance =
        crate::client_provenance::export_provenance(report, &report_json.cover, corpus);
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
    })
}

/// A package that cleared release: the zip a client may receive.
#[derive(Debug)]
pub struct ReleasedZip {
    pub stem: String,
    pub bytes: Vec<u8>,
}

/// Why [`release`] produced no zip.
#[derive(Debug)]
pub enum ReleaseError {
    /// The archive could not be assembled (an I/O error writing the in-memory zip).
    Assembly(String),
}

impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseError::Assembly(e) => {
                write!(f, "could not assemble the product export zip: {e}")
            }
        }
    }
}

impl std::error::Error for ReleaseError {}

/// Zip the package. This is the ONLY function the real exporters call to obtain zip bytes.
pub fn release(package: ProductPackage) -> Result<ReleasedZip, ReleaseError> {
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
    Ok(ReleasedZip {
        stem: package.stem,
        bytes,
    })
}
