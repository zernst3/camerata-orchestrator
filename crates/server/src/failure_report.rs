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
    /// When a later attempt (a resume, or a new run) completed this part, so it no longer fails.
    /// The entry and its attempts stay in the file as history. `None` while it still fails.
    #[serde(default)]
    pub resolved_at: Option<String>,
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
                resolved_at: None,
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

    /// The entries that still fail.
    pub fn unresolved(&self) -> impl Iterator<Item = &FailureEntry> {
        self.entries.iter().filter(|e| e.resolved_at.is_none())
    }

    /// One structured line per failure that still fails, for stderr: `key=value` pairs, stable
    /// order. A resolved entry is history and is not printed.
    pub fn stderr_lines(&self) -> Vec<String> {
        self.unresolved()
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
        let open = self.unresolved().count();
        let mut out = format!(
            "# Failure report\n\nGenerated {}. {} part(s) failed or did not run{}. Operator \
             only: this file is never part of an export.\n",
            self.generated_at,
            open,
            if open == self.entries.len() {
                String::new()
            } else {
                format!(
                    "; {} more failed in an earlier attempt and were completed since",
                    self.entries.len() - open
                )
            }
        );
        for (i, e) in self.entries.iter().enumerate() {
            out.push_str(&format!(
                "\n## {}. {} ({}){}\n\nClass: {}. Files: {}. Attempts: {}.",
                i + 1,
                e.part,
                e.repo,
                e.resolved_at
                    .as_ref()
                    .map(|t| format!(" - resolved {t}"))
                    .unwrap_or_default(),
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

/// How many resolved entries the file keeps as history.
const KEEP_RESOLVED: usize = 200;

/// The report as it should be on disk after an attempt: `current` (what still fails, now), with
/// the earlier attempts of each part put in front of its own, and every part that failed before
/// and does not fail now kept as history with the time it was completed.
pub fn merge_history(previous: &FailureReport, current: &FailureReport) -> FailureReport {
    let now = crate::client_source::records::now_rfc3339();
    let same = |a: &FailureEntry, b: &FailureEntry| a.repo == b.repo && a.part == b.part;
    let mut entries: Vec<FailureEntry> = current.entries.clone();
    for entry in &mut entries {
        if let Some(old) = previous.unresolved().find(|old| same(old, entry)) {
            let mut attempts = old.attempts.clone();
            attempts.append(&mut entry.attempts);
            entry.attempts = attempts;
            if !old.first_at.is_empty() {
                entry.first_at = old.first_at.clone();
            }
        }
    }
    let mut history: Vec<FailureEntry> = Vec::new();
    for old in &previous.entries {
        if old.resolved_at.is_some() {
            history.push(old.clone());
        } else if !current.entries.iter().any(|now| same(old, now)) {
            let mut done = old.clone();
            done.resolved_at = Some(now.clone());
            history.push(done);
        }
    }
    if history.len() > KEEP_RESOLVED {
        history.drain(..history.len() - KEEP_RESOLVED);
    }
    entries.extend(history);
    FailureReport {
        generated_at: current.generated_at.clone(),
        entries,
    }
}

/// Write `failures.json` and `failures.md` next to `ledger_file`. Returns the two paths, or
/// `None` (and removes any older pair) when nothing failed now and nothing failed before. A part
/// that failed in an earlier attempt and is complete now stays in the file as history, with the
/// time it was resolved and every attempt it took.
pub fn write(
    ledger_file: &Path,
    report: &FailureReport,
) -> std::io::Result<Option<(PathBuf, PathBuf)>> {
    let (json_path, md_path) = paths_next_to(ledger_file);
    let previous: FailureReport = std::fs::read(&json_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let merged = merge_history(&previous, report);
    if merged.is_empty() {
        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_file(&md_path);
        return Ok(None);
    }
    let json = serde_json::to_string_pretty(&merged).map_err(std::io::Error::other)?;
    std::fs::write(&json_path, json)?;
    std::fs::write(&md_path, merged.render_md())?;
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
        // Nothing failed now and nothing failed before: no files at all.
        let other = dir.path().join("y-5678.ledger.json");
        assert!(write(&other, &FailureReport::default()).unwrap().is_none());
        assert!(!paths_next_to(&other).0.exists());
    }

    #[test]
    fn a_part_completed_by_a_later_attempt_stays_as_history_with_a_resolved_time() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("x-1234.ledger.json");
        // Attempt 1: a part fails. Attempt 2 (a resume): it fails again once more, then a third
        // attempt completes it.
        write(&ledger, &build(&report_with(vec![failed()]))).unwrap();
        let (json, md) = write(&ledger, &build(&report_with(vec![failed()])))
            .unwrap()
            .expect("still failing");
        let twice: FailureReport =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(twice.entries.len(), 1);
        assert_eq!(twice.entries[0].attempts.len(), 4, "both attempts are kept");
        assert!(twice.entries[0].resolved_at.is_none());

        let done = write(&ledger, &build(&report_with(Vec::new())))
            .unwrap()
            .expect("the history is kept");
        let back: FailureReport =
            serde_json::from_str(&std::fs::read_to_string(&done.0).unwrap()).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert!(back.entries[0].resolved_at.is_some(), "{back:?}");
        assert_eq!(back.entries[0].attempts.len(), 4);
        assert_eq!(back.unresolved().count(), 0);
        assert!(
            back.stderr_lines().is_empty(),
            "history is not printed as a failure"
        );
        let text = std::fs::read_to_string(md).unwrap();
        assert!(text.contains("resolved"), "{text}");
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
