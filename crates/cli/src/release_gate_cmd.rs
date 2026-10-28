//! `camerata release-gate <zip>`: run the export release gate over an EXISTING product zip.
//!
//! The same checks `camerata inspect --export` and the cockpit run before they hand a package to
//! a client (secrets in every file and workbook cell, the dataset set, CSV against JSON, the
//! manifest hashes, README against manifest), but over a zip that is already on disk: the
//! acceptance tool for exports made before the gate existed, and a way for a person to audit any
//! package they were handed.
//!
//! With the `pdf-text` cargo feature the PDF's rendered text is extracted (`pdf-extract`) and
//! checked too; without it the PDF is checked only through the `data.json` check that ran at
//! export time, and the verdict says so. Exit codes: `0` clean, `3` blocked (the same code
//! `camerata inspect` uses when the gate refuses an export), `2` the gate could not run (an
//! unreadable file or archive; deliberately not a pass).
//!
//! The output names each problem by file, location and REDACTED form only; it never prints a
//! secret value.

use std::path::{Path, PathBuf};

use camerata_server::release_gate::{audit_zip, read_zip_entries, GateFailure, GateReport};

/// Exit code: the package passed every check that ran.
pub const EXIT_CLEAN: i32 = 0;
/// Exit code: the gate could not run (unreadable file or archive). NOT a pass.
pub const EXIT_GATE_UNAVAILABLE: i32 = 2;
/// Exit code: the gate refused the package (same as `camerata inspect`).
pub const EXIT_BLOCKED: i32 = 3;

/// Arguments of `camerata release-gate`.
#[derive(Debug, Clone)]
pub struct ReleaseGateArgs {
    pub zip: PathBuf,
}

/// What the gate concluded about one zip.
#[derive(Debug)]
pub struct ReleaseGateVerdict {
    pub zip: PathBuf,
    pub report: GateReport,
    /// Whether the PDF's rendered text was extracted and checked.
    pub pdf_text_checked: bool,
}

impl ReleaseGateVerdict {
    pub fn exit_code(&self) -> i32 {
        if self.report.is_clean() {
            EXIT_CLEAN
        } else {
            EXIT_BLOCKED
        }
    }
}

/// The PDF's rendered text, when the `pdf-text` feature is built in and the zip carries a PDF.
#[cfg(feature = "pdf-text")]
fn extract_pdf_text(zip_bytes: &[u8]) -> Option<String> {
    let entries = read_zip_entries(zip_bytes).ok()?;
    let pdf = entries
        .iter()
        .find(|(name, _)| !name.contains('/') && name.ends_with(".pdf"))
        .map(|(_, bytes)| bytes)?;
    pdf_extract::extract_text_from_mem(pdf).ok()
}

#[cfg(not(feature = "pdf-text"))]
fn extract_pdf_text(_zip_bytes: &[u8]) -> Option<String> {
    None
}

/// Read `args.zip`, extract the PDF text when built with `pdf-text`, and run every check.
pub fn run_release_gate(args: &ReleaseGateArgs) -> Result<ReleaseGateVerdict, String> {
    let bytes = std::fs::read(&args.zip)
        .map_err(|e| format!("could not read {}: {e}", args.zip.display()))?;
    read_zip_entries(&bytes).map_err(|e| format!("{}: {e}", args.zip.display()))?;
    let pdf_text = extract_pdf_text(&bytes);
    let report = audit_zip(&bytes, pdf_text.as_deref());
    Ok(ReleaseGateVerdict {
        zip: args.zip.clone(),
        pdf_text_checked: pdf_text.is_some(),
        report,
    })
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "package".to_string())
}

/// The text `camerata release-gate` prints. A blocked package prints the gate's own block (the
/// one `inspect` and the cockpit show); a clean one prints which checks ran.
pub fn format_verdict(verdict: &ReleaseGateVerdict) -> String {
    let pdf_note = if verdict.pdf_text_checked {
        "PDF text: extracted and checked"
    } else {
        "PDF text: not checked here (build with --features pdf-text); the PDF's data.json is \
         checked when the export is made"
    };
    if verdict.report.is_clean() {
        format!(
            "release gate: passed {} ({} checks: {})\n{pdf_note}",
            verdict.zip.display(),
            verdict.report.checks.len(),
            verdict.report.check_names().join(", ")
        )
    } else {
        let failure = GateFailure {
            stem: stem_of(&verdict.zip),
            report: verdict.report.clone(),
        };
        format!("{failure}\n{pdf_note}")
    }
}
