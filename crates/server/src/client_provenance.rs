//! The client export's provenance, assembled in one place (design:
//! `docs/design/2026-10-20_machine-readable-exports.md`, "manifest.json").
//!
//! Both export entry points, `export_product` (HTTP) and `camerata inspect --export` (CLI), call
//! [`export_provenance`] and pass the result to `product_export_readme` and `build_product_zip`,
//! so the manifest and the README are filled the same way everywhere:
//!
//! - **commit, per repo**: the git identity the scan recorded (see
//!   [`ManifestProvenance::from_cover`]).
//! - **corpus version**: [`camerata_rules::corpus_version`], a content digest of the rule corpus.
//! - **rules last reviewed**: [`camerata_rules::client_rules_last_reviewed`], the oldest review
//!   date across the rules the scan used, `None` while any of them has never been reviewed.
//! - **vulnerability data as of**: the scan's own `ScanProvenance::vulnerability_data` stamp
//!   (CISA KEV, FIRST EPSS, OSV), see [`crate::vuln_intel`].
//! - **corpus rules applied, counted lines, excluded noise** (22c): from the scan's own record
//!   (`audited_rule_ids` against the corpus; `code_lines`, `files_excluded` and
//!   `excluded_noise_lines`, read at scan time through `camerata count`'s filter).
//!
//! # The one sanctioned freshness path
//!
//! Rule-maintenance status is an operator concern, and none of its text may reach a client
//! artifact: the client-artifact gate in the `lib.rs` tests scans
//! the artifact builders' source for the operator-side identifiers and fails on any of them.
//! The one date a client IS meant to see is the one above. It reaches the export through this
//! module and nowhere else, because this is the only file the gate lets name the watermark type
//! and the sanctioned function (and nothing else from the maintenance code). The builders
//! themselves (`report_export`, `xlsx_export`, `build_product_zip`) stay free of it entirely:
//! they receive a finished [`ManifestProvenance`] and print whatever date it carries.

use camerata_rules::{client_rules_last_reviewed, corpus_version, CorpusFreshness, RuleSet};

use crate::onboard::ScanReport;
use crate::report_export::CoverJson;
use crate::ManifestProvenance;

/// The provenance for a product export, read against the bundled maintenance watermark.
///
/// A watermark that is absent or does not parse yields no review date (never an error): a
/// missing file cannot be allowed to fail an export, and a date we cannot read is a date we do
/// not claim. See [`export_provenance_with`] for what each field means.
pub fn export_provenance(
    report: &ScanReport,
    cover: &CoverJson,
    corpus: Option<&RuleSet>,
) -> ManifestProvenance {
    let path = camerata_rules::corpus_freshness_path();
    let watermark = match CorpusFreshness::load(&path) {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "rule review watermark does not parse; the export will carry no review date"
            );
            None
        }
    };
    export_provenance_with(report, cover, corpus, watermark.as_ref())
}

/// [`export_provenance`] with the watermark supplied, so the result is a pure function of its
/// inputs (and testable without a file).
///
/// - `repos`/`generated_at`/commits come from the cover ([`ManifestProvenance::from_cover`]).
/// - `corpus_version` is the digest of the WHOLE loaded corpus, or `None` when no corpus is
///   loaded. It identifies the rules the exporting process had, which is the scan's corpus when
///   the scan and the export run against the same build (the CLI always, the server unless it
///   was rebuilt in between).
/// - `rules_last_reviewed` is the oldest effective review date across the rules the scan
///   audited (`provenance.audited_rule_ids`). It is `None` when there is no corpus or no
///   watermark, when the scan audited nothing, when any audited rule id is not in the loaded
///   corpus (a rule we hold no review record for, so no date can vouch for it), or when any
///   audited rule has never been reviewed.
pub fn export_provenance_with(
    report: &ScanReport,
    cover: &CoverJson,
    corpus: Option<&RuleSet>,
    watermark: Option<&CorpusFreshness>,
) -> ManifestProvenance {
    let mut provenance = ManifestProvenance::from_cover(cover);
    provenance.cost_cap_reached = report.ledger.is_cost_capped();
    provenance.cost_cap_unenforced =
        provenance.cost_cap_reached && report.ledger.cost_cap().is_some_and(|cap| cap.cost_unknown);
    provenance.incomplete = report.incomplete_reasons();
    provenance.vulnerability_data = report.provenance.vulnerability_data.clone();
    provenance.corpus_version = corpus.map(corpus_version);
    provenance.rules_last_reviewed = match (watermark, rules_used_by_scan(report, corpus)) {
        (Some(watermark), Some(used)) => client_rules_last_reviewed(watermark, &used),
        _ => None,
    };
    provenance.corpus_rules_applied = corpus.map(|corpus| corpus_rules_applied(report, corpus));
    // The quote basis, as the scan recorded it through `camerata count`'s own filter and line
    // definition (the export never reads the repository).
    let read_files = report.code_lines > 0;
    provenance.counted_lines = read_files.then_some(report.code_lines);
    provenance.excluded_noise_files = read_files.then_some(report.files_excluded);
    provenance.excluded_noise_lines = report.excluded_noise_lines.filter(|_| read_files);
    provenance
}

/// How many distinct rules the scan audited that the loaded corpus holds.
fn corpus_rules_applied(report: &ScanReport, corpus: &RuleSet) -> usize {
    report
        .provenance
        .audited_rule_ids
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .filter(|id| corpus.get_by_id(id).is_some())
        .count()
}

/// The rules the scan audited, resolved against `corpus`. `None` when there is no corpus or any
/// audited id is unknown to it, so a caller can never date a set it cannot fully account for.
/// Duplicate ids count once.
fn rules_used_by_scan(report: &ScanReport, corpus: Option<&RuleSet>) -> Option<RuleSet> {
    let corpus = corpus?;
    let mut used = RuleSet::default();
    let mut seen = std::collections::HashSet::new();
    for id in &report.provenance.audited_rule_ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        used.push(corpus.get_by_id(id)?.clone());
    }
    Some(used)
}
