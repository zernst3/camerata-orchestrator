//! Construction-time secret redaction: the ONE choke point where a secret value stops being
//! client-visible text (docs/design/2026-10-23_secret-redaction-and-release-gate.md section 4).
//!
//! The governing law: a value that matches a secret shape never appears in full in anything the
//! client receives, and never reaches a model when we can avoid it. Redaction happens ONCE, where
//! a finding is built (every producer calls [`redact_finding`] the moment the row is complete),
//! and again, as belt and braces, in [`ScanReport::seal`](super::ScanReport::seal) and
//! `report_export::sanitize_finding_text`. The release gate (`crate::release_gate`) then proves it
//! over the exact bytes shipped. Every function here is idempotent: a redacted form matches no
//! shape.
//!
//! [`files_for_model`] is the model-side half: the code a model reads is redacted too, with every
//! line number preserved, so a `path:line` the model cites still resolves.

use std::borrow::Cow;

use super::{Finding, ScanReport};
use crate::report_export::strip_zero_width;
use camerata_api_types::audit::FindingKind;

/// `text` with zero-width characters stripped, then with every secret value replaced by its
/// redacted form. Stripping FIRST means a zero-width character inserted inside a key cannot hide
/// it from the detector while a reader still sees the key. Borrows when nothing changed.
pub(crate) fn redact_visible(text: &str) -> Cow<'_, str> {
    let clean = strip_zero_width(text);
    let redacted = match camerata_secrets::redact_str(&clean) {
        Cow::Owned(redacted) => Some(redacted),
        Cow::Borrowed(_) => None,
    };
    redacted.map_or(clean, Cow::Owned)
}

/// Redact `text` in place. Returns the number of values replaced. Zero-width characters are
/// stripped first (see [`redact_visible`]).
pub(crate) fn redact_string(text: &mut String) -> usize {
    if let Cow::Owned(clean) = strip_zero_width(text) {
        *text = clean;
    }
    let redacted = camerata_secrets::redact(text);
    let count = redacted.redactions.len();
    if count > 0 {
        *text = redacted.text;
    }
    count
}

fn redact_option(text: &mut Option<String>) -> usize {
    text.as_mut().map_or(0, redact_string)
}

/// Redact every client-visible free-text field of a finding IN PLACE: `snippet`, `detail`,
/// `fix_specific`, `calibration_rationale`, every `captures` value, every `also_locations`
/// snippet, and the string payload of the finding's kind. Returns the number of values replaced.
/// Idempotent. Called by every producer the moment a row is complete.
pub(crate) fn redact_finding(f: &mut Finding) -> usize {
    let mut count = redact_string(&mut f.snippet)
        + redact_string(&mut f.detail)
        + redact_option(&mut f.fix_specific)
        + redact_option(&mut f.calibration_rationale);
    for value in f.captures.values_mut() {
        count += redact_string(value);
    }
    for site in &mut f.also_locations {
        count += redact_string(&mut site.snippet);
    }
    count += match &mut f.kind {
        Some(FindingKind::Deviation(d)) => redact_string(&mut d.convention_label),
        Some(FindingKind::ConventionObserved(c)) => redact_string(&mut c.convention_label),
        Some(FindingKind::NoConvention(n)) => n
            .observed_options
            .iter_mut()
            .map(|option| redact_string(&mut option.label))
            .sum(),
        Some(FindingKind::NotAssessed(n)) => redact_string(&mut n.reason_detail),
        Some(FindingKind::Defect) | None => 0,
    };
    count
}

/// [`redact_finding`] over a slice. Returns the total number of values replaced.
pub(crate) fn redact_findings(findings: &mut [Finding]) -> usize {
    findings.iter_mut().map(redact_finding).sum()
}

/// The ONE way a code line becomes a snippet: trim, redact, THEN cap at `max` chars. Redacting
/// before the cap means a cap can never cut a secret in half and leave a prefix plus a partial
/// body that neither the redactor nor the release gate could recognise.
pub(crate) fn snippet_from_line(line: &str, max: usize) -> String {
    redact_visible(line.trim()).chars().take(max).collect()
}

/// Redact every model-written string of a deep-tier report: the lens prose and error, the SOC-2
/// gap rows, the threat rows and the deep-security findings. Used on the way into a stored
/// [`ScanReport`] and again by the deep-report export, which reads the report back from the job
/// store. Returns the number of values replaced. Idempotent.
pub(crate) fn redact_deep_report(deep: &mut crate::ai_audit::DeepReport) -> usize {
    let mut count = redact_string(&mut deep.disclaimer);
    for lens in &mut deep.lenses {
        count += redact_string(&mut lens.summary);
        count += redact_option(&mut lens.error);
        count += redact_findings(&mut lens.security_findings);
        for gap in &mut lens.soc2_gaps {
            count += redact_string(&mut gap.control);
            count += redact_string(&mut gap.title);
            count += redact_string(&mut gap.status);
            count += redact_string(&mut gap.observed);
            count += redact_string(&mut gap.gap);
        }
        for threat in &mut lens.threats {
            count += redact_string(&mut threat.component);
            count += redact_string(&mut threat.kind);
            count += redact_string(&mut threat.threat);
            count += redact_string(&mut threat.category);
            count += redact_string(&mut threat.mitigation);
        }
    }
    count
}

/// Redact ScanReport-level text that bypasses `Finding`: the per-rule recommendations
/// (`evidence`, `recommendation_reasoning`, observed option labels), coverage notes, failed
/// passes, the AI error and block reason, the report message, the proposed rules' text, and the
/// deep tier's prose. Returns the number of values replaced.
pub(crate) fn redact_report_text(report: &mut ScanReport) -> usize {
    let mut count = 0;
    for rec in report.recommendations.values_mut() {
        count += redact_option(&mut rec.evidence);
        count += redact_string(&mut rec.recommendation_reasoning);
        for option in &mut rec.observed_options {
            count += redact_string(&mut option.label);
        }
    }
    for note in &mut report.coverage_notes {
        count += redact_string(&mut note.message);
    }
    for pass in &mut report.failed_passes {
        count += redact_string(&mut pass.reason);
    }
    count += redact_option(&mut report.ai_error);
    count += redact_option(&mut report.ai_blocked_reason);
    count += redact_option(&mut report.message);
    for rule in &mut report.proposed_rules {
        count += redact_string(&mut rule.title);
        count += redact_option(&mut rule.decision_question);
        count += redact_option(&mut rule.decision_why);
        for option in &mut rule.options {
            count += redact_string(&mut option.label);
            count += redact_string(&mut option.directive);
            count += redact_string(&mut option.why);
        }
    }
    if let Some(deep) = &mut report.deep {
        count += redact_deep_report(deep);
    }
    count += report.ledger.redact_text(&mut redact_string);
    count
}

/// One sentence appended to every system prompt that carries client code, so the model reads a
/// redacted value as deliberate, not as a bug in the code, and never reproduces one.
pub(crate) const MODEL_REDACTION_NOTICE: &str = "\n\nCredential values in the code appear as \
`prefix\u{2026}tail (N chars)`; this is deliberate redaction, not a bug in the code. Never reproduce \
a credential value; refer to it by its variable name and line.";

/// The file set the MODEL tiers read. Same paths, same line count, every secret value replaced by
/// its redacted form (`sb_secret_…WvXy (51 chars)`). The deterministic floor and the
/// architectural checkers keep reading the RAW set (they must still FIND the secret); only what
/// leaves the machine is redacted. Honors the `model_prompt_redaction` flag (default ON), which is
/// read from the environment only, never from a config file in a client repo.
pub(crate) fn files_for_model(files: &[(String, String)]) -> Vec<(String, String)> {
    files_for_model_with(
        files,
        crate::feature_flags::model_prompt_redaction_enabled(),
    )
}

/// [`files_for_model`] with the flag decided by the caller (the pure, testable core). When
/// `enabled` is false the raw set is returned and a warning is logged: raw client code is
/// leaving the machine.
pub(crate) fn files_for_model_with(
    files: &[(String, String)],
    enabled: bool,
) -> Vec<(String, String)> {
    if !enabled {
        tracing::warn!(
            files = files.len(),
            "model_prompt_redaction is OFF: raw code, secret values included, is being sent to \
             the model (CAMERATA_FEATURE_MODEL_PROMPT_REDACTION=false)"
        );
        return files.to_vec();
    }
    files
        .iter()
        .map(|(path, content)| (path.clone(), redact_visible(content).into_owned()))
        .collect()
}

#[cfg(test)]
mod producers;
#[cfg(test)]
mod tests;
