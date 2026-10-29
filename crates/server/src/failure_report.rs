//! The operator's failure report: for every part of a run that failed or did not run, what it was,
//! why, and what every attempt got back, so a failure can be diagnosed without the logs.
//!
//! Zach, 2026-10-28: "spell out what the logs were, what the returned API calls were saying."
//!
//! OPERATOR ONLY. [`write`] puts `failures.json` and a short readable `failures.md` next to the
//! integrity ledger in the state directory. Nothing here is ever added to an export: a test pins
//! that the product zip holds neither file. Every message is scrubbed of secrets and capped, and
//! no prompt text or client code is kept (attempts hold the provider's error message and the
//! CLI's stderr tail only).

use std::path::{Path, PathBuf};

use camerata_llm::failure::{scrub_message, AttemptRecord, FailureClass, MAX_RECORD_MESSAGE_CHARS};
use serde::{Deserialize, Serialize};

/// One failed or not-run part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureEntry {
    pub repo: String,
    /// The part's name (`audit pass[all] 1/3 · rules 2/4`, `fix-specific generation`, ...).
    pub part: String,
    /// Repo-relative files the part was shown and so did not review.
    pub files: Vec<String>,
    pub class: FailureClass,
    /// Every attempt, oldest first, the end-of-run second try included.
    pub attempts: Vec<AttemptRecord>,
    /// Time of the first attempt (empty when the part was never sent).
    pub first_at: String,
    /// Time of the last attempt (empty when the part was never sent).
    pub last_at: String,
    /// A short message, secrets redacted and capped; never client code.
    pub message: String,
}

/// The whole report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureReport {
    pub generated_at: String,
    pub entries: Vec<FailureEntry>,
}

/// The entries for every failed part of `report`. A disclosed scope limit (a tool not run by
/// policy) is not a failure and is not listed.
pub fn build(report: &crate::onboard::ScanReport) -> FailureReport {
    let entries = report
        .failed_passes
        .iter()
        .filter(|f| !f.scope_note)
        .map(|f| {
            let first_at = f.attempts.first().map(|a| a.at.clone()).unwrap_or_default();
            let last_at = f.attempts.last().map(|a| a.at.clone()).unwrap_or_default();
            let redacted = camerata_secrets::redact_str(&f.reason).into_owned();
            FailureEntry {
                repo: f.repo.clone(),
                part: f.pass.clone(),
                files: f.files.clone(),
                class: f.class,
                attempts: f.attempts.clone(),
                first_at,
                last_at,
                message: scrub_message(&redacted, MAX_RECORD_MESSAGE_CHARS),
            }
        })
        .collect();
    FailureReport {
        generated_at: crate::client_source::records::now_rfc3339(),
        entries,
    }
}

impl FailureReport {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One structured line per failure, for stderr: `key=value` pairs, stable order.
    pub fn stderr_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| {
                format!(
                    "camerata failure: class={} repo={} part=\"{}\" files={} attempts={} first={} last={} message=\"{}\"",
                    e.class.as_str(),
                    e.repo,
                    e.part,
                    e.files.len(),
                    e.attempts.len(),
                    if e.first_at.is_empty() { "-" } else { &e.first_at },
                    if e.last_at.is_empty() { "-" } else { &e.last_at },
                    e.message,
                )
            })
            .collect()
    }

    /// The short readable form.
    pub fn render_md(&self) -> String {
        let mut out = format!(
            "# Failure report\n\nGenerated {}. {} part(s) failed or did not run. Operator only: \
             this file is never part of an export.\n",
            self.generated_at,
            self.entries.len()
        );
        for (i, e) in self.entries.iter().enumerate() {
            out.push_str(&format!(
                "\n## {}. {} ({})\n\nClass: {}. Files: {}. Attempts: {}.",
                i + 1,
                e.part,
                e.repo,
                e.class.as_str(),
                e.files.len(),
                e.attempts.len()
            ));
            if !e.first_at.is_empty() {
                out.push_str(&format!(" First {}, last {}.", e.first_at, e.last_at));
            }
            out.push_str(&format!("\n\n{}\n", e.message));
            for (n, a) in e.attempts.iter().enumerate() {
                let mut line = format!(
                    "\n- attempt {}{}: {} {} {}",
                    n + 1,
                    if a.second_try { " (second try)" } else { "" },
                    a.at,
                    a.backend,
                    a.model
                );
                if let Some(s) = a.http_status {
                    line.push_str(&format!(", HTTP {s}"));
                }
                if let Some(t) = &a.error_type {
                    line.push_str(&format!(", {t}"));
                }
                if let Some(r) = &a.request_id {
                    line.push_str(&format!(", request {r}"));
                }
                if let Some(r) = a.retry_after_secs {
                    line.push_str(&format!(", retry-after {r}s"));
                }
                if let Some(w) = a.wait_secs {
                    line.push_str(&format!(", waited {w}s"));
                }
                if let Some(c) = a.exit_code {
                    line.push_str(&format!(", exit {c}"));
                }
                if !a.message.is_empty() {
                    line.push_str(&format!(": {}", a.message));
                }
                if let Some(t) = &a.stderr_tail {
                    line.push_str(&format!(" [stderr: {t}]"));
                }
                out.push_str(&line);
            }
            out.push('\n');
        }
        out
    }
}

/// Where the report goes for a given ledger file: next to it, same stem.
pub fn paths_next_to(ledger_file: &Path) -> (PathBuf, PathBuf) {
    let name = ledger_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".ledger.json").unwrap_or(&name);
    let dir = ledger_file.parent().unwrap_or_else(|| Path::new("."));
    (
        dir.join(format!("{stem}.failures.json")),
        dir.join(format!("{stem}.failures.md")),
    )
}

/// Write `failures.json` and `failures.md` next to `ledger_file`. Returns the two paths, or
/// `None` (and removes any older pair) when nothing failed.
pub fn write(
    ledger_file: &Path,
    report: &FailureReport,
) -> std::io::Result<Option<(PathBuf, PathBuf)>> {
    let (json_path, md_path) = paths_next_to(ledger_file);
    if report.is_empty() {
        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_file(&md_path);
        return Ok(None);
    }
    let json = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(&json_path, json)?;
    std::fs::write(&md_path, report.render_md())?;
    Ok(Some((json_path, md_path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_audit::FailedPass;

    fn failed() -> FailedPass {
        FailedPass {
            repo: "acme/api".to_string(),
            pass: "audit pass[all] 1/2 \u{b7} rules 1/1".to_string(),
            reason: "Anthropic API HTTP 529: overloaded key sk-ant-api03-AAAAAAAAAAAAAAAA1234"
                .to_string(),
            files_not_reviewed: 2,
            class: FailureClass::Transport,
            files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            attempts: vec![
                AttemptRecord {
                    at: "2026-10-28T10:00:00Z".to_string(),
                    backend: "api".to_string(),
                    model: "m".to_string(),
                    http_status: Some(529),
                    error_type: Some("overloaded_error".to_string()),
                    message: "Overloaded".to_string(),
                    request_id: Some("req_1".to_string()),
                    retry_after_secs: Some(30),
                    wait_secs: Some(5),
                    ..AttemptRecord::default()
                },
                AttemptRecord {
                    at: "2026-10-28T10:30:00Z".to_string(),
                    backend: "api".to_string(),
                    model: "m".to_string(),
                    http_status: Some(529),
                    second_try: true,
                    ..AttemptRecord::default()
                },
            ],
            scope_note: false,
        }
    }

    fn report_with(passes: Vec<FailedPass>) -> crate::onboard::ScanReport {
        let mut r = crate::release_gate::test_scan_report();
        r.failed_passes = passes;
        r
    }

    #[test]
    fn an_entry_has_repo_part_files_class_attempts_times_and_a_redacted_message() {
        let fr = build(&report_with(vec![failed()]));
        assert_eq!(fr.entries.len(), 1);
        let e = &fr.entries[0];
        assert_eq!(e.repo, "acme/api");
        assert_eq!(e.files.len(), 2);
        assert_eq!(e.class, FailureClass::Transport);
        assert_eq!(e.attempts.len(), 2);
        assert_eq!(e.first_at, "2026-10-28T10:00:00Z");
        assert_eq!(e.last_at, "2026-10-28T10:30:00Z");
        assert!(!e.message.contains("AAAAAAAA"), "{}", e.message);
        assert!(e.message.chars().count() <= MAX_RECORD_MESSAGE_CHARS + 3);
    }

    #[test]
    fn a_scope_note_is_not_a_failure() {
        let mut note = failed();
        note.scope_note = true;
        assert!(build(&report_with(vec![note])).is_empty());
    }

    #[test]
    fn the_readable_form_spells_out_every_attempt() {
        let md = build(&report_with(vec![failed()])).render_md();
        for needle in [
            "HTTP 529",
            "overloaded_error",
            "request req_1",
            "retry-after 30s",
            "waited 5s",
            "(second try)",
            "acme/api",
        ] {
            assert!(md.contains(needle), "{needle} missing from {md}");
        }
    }

    #[test]
    fn one_structured_stderr_line_per_failure() {
        let fr = build(&report_with(vec![failed(), failed()]));
        let lines = fr.stderr_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("camerata failure: class=transport repo=acme/api"));
    }

    #[test]
    fn the_files_land_next_to_the_ledger_and_an_empty_report_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("x-1234.ledger.json");
        let fr = build(&report_with(vec![failed()]));
        let (json, md) = write(&ledger, &fr).unwrap().expect("written");
        assert_eq!(json.parent(), ledger.parent());
        assert!(json.ends_with("x-1234.failures.json") && md.ends_with("x-1234.failures.md"));
        let back: FailureReport =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert!(write(&ledger, &FailureReport::default()).unwrap().is_none());
        assert!(!json.exists() && !md.exists());
    }

    #[test]
    fn the_failure_report_never_lands_in_the_zip() {
        let package = crate::release_gate::test_internal_incomplete_package();
        let released = crate::release_gate::release_internal(package).unwrap();
        let entries = crate::release_gate::read_zip_entries(&released.bytes).unwrap();
        assert!(
            entries.keys().all(|k| !k.contains("failures")),
            "{:?}",
            entries.keys().collect::<Vec<_>>()
        );
    }
}
