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

/// A finding that ships the rule's own authored fix (a "Standard fix" in the export) because no
/// model-written fix passed the checks in any round (D8). Not a failure: the run is complete.
/// Listed here as a quality signal, since a checker bug can hide behind one (D7 did).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandardFixEntry {
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    /// Why the checks turned the model's fix down, secrets scrubbed and capped.
    pub reasons: String,
}

/// A finding that ships the scan's own statement plus the accepted fix direction as its detail
/// because the model's regenerated explanation was turned down in every round for inventing an
/// exploit precondition (R1-1). Not a failure: the run is complete. Listed here as a quality
/// signal, since a checker bug can hide behind one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailFallbackEntry {
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    /// Why the checks turned the model's explanation down, secrets scrubbed and capped.
    pub reasons: String,
}

/// The whole report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureReport {
    pub generated_at: String,
    pub entries: Vec<FailureEntry>,
    /// Every finding of this run that ships a Standard fix after the checks turned the model's fix
    /// down in every round. Operator only, like the rest of the file. `serde(default)` so a report
    /// written before D8 still loads.
    #[serde(default)]
    pub standard_fixes: Vec<StandardFixEntry>,
    /// Every finding of this run that ships the scan's own statement as its detail after the
    /// regenerated explanation was turned down in every round for inventing a precondition.
    /// Operator only. `serde(default)` so a report written before R1-1 still loads.
    #[serde(default)]
    pub detail_fallbacks: Vec<DetailFallbackEntry>,
    /// What the batch runner told the operator about spend the ledger cannot count (a batch the
    /// provider no longer has, a submission that may have been accepted and orphaned). Not a
    /// failure and never the reason a report exists: it rides along in a report that does.
    /// Operator only. `serde(default)` so a report written before it existed still loads.
    #[serde(default)]
    pub runner_notes: Vec<String>,
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
    let standard_fixes = report
        .findings
        .iter()
        .filter(|f| {
            f.captures
                .get(crate::report_export::FIX_SOURCE_CAPTURE)
                .is_some_and(|v| v == crate::report_export::STANDARD_FIX)
                && f.captures
                    .contains_key(crate::report_export::FIX_REJECTION_CAPTURE)
        })
        .map(|f| StandardFixEntry {
            repo: f.repo.clone(),
            path: f.path.clone(),
            line: f.line,
            rule_id: f.rule_id.clone(),
            reasons: f
                .captures
                .get(crate::report_export::FIX_REJECTION_CAPTURE)
                .cloned()
                .unwrap_or_default(),
        })
        .collect();
    let detail_fallbacks = report
        .findings
        .iter()
        .filter(|f| {
            f.captures
                .get(crate::report_export::DETAIL_SOURCE_CAPTURE)
                .is_some_and(|v| v == crate::report_export::SCAN_DETAIL)
                && f.captures
                    .contains_key(crate::report_export::DETAIL_REJECTION_CAPTURE)
        })
        .map(|f| DetailFallbackEntry {
            repo: f.repo.clone(),
            path: f.path.clone(),
            line: f.line,
            rule_id: f.rule_id.clone(),
            reasons: f
                .captures
                .get(crate::report_export::DETAIL_REJECTION_CAPTURE)
                .cloned()
                .unwrap_or_default(),
        })
        .collect();
    FailureReport {
        generated_at: crate::client_source::records::now_rfc3339(),
        entries,
        standard_fixes,
        detail_fallbacks,
        runner_notes: report
            .ledger
            .runner_notes()
            .iter()
            .map(|n| camerata_secrets::redact_str(n).into_owned())
            .collect(),
    }
}

impl FailureReport {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
            && self.standard_fixes.is_empty()
            && self.detail_fallbacks.is_empty()
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
        if !self.standard_fixes.is_empty() {
            out.push_str(&format!(
                "\n## Standard fixes ({})\n\nThese findings ship the rule's own authored fix, \
                 labelled \"Standard fix\" in the export, because no model-written fix passed \
                 the checks in any round. The run is complete. A reason that names a real, \
                 ordinary name is a signal the checker is too strict (defect D7 was one).\n",
                self.standard_fixes.len()
            ));
            for s in &self.standard_fixes {
                out.push_str(&format!(
                    "\n- {} {}:{} {}: {}",
                    s.repo, s.path, s.line, s.rule_id, s.reasons
                ));
            }
            out.push('\n');
        }
        if !self.detail_fallbacks.is_empty() {
            out.push_str(&format!(
                "\n## Scan statements shipped as the detail ({})\n\nThese findings ship the \
                 scan's own statement plus the model's accepted fix direction as their detail, \
                 because the model's full explanation was turned down in every round for adding \
                 an exploit precondition that neither the statement nor the code shown makes. \
                 The rejected text is not shipped. The run is complete. A reason that restates \
                 what the code shown says is a signal the checker is too strict (defect R1-1 \
                 was one).\n",
                self.detail_fallbacks.len()
            ));
            for s in &self.detail_fallbacks {
                out.push_str(&format!(
                    "\n- {} {}:{} {}: {}",
                    s.repo, s.path, s.line, s.rule_id, s.reasons
                ));
            }
            out.push('\n');
        }
        if !self.runner_notes.is_empty() {
            out.push_str(&format!(
                "\n## Batch runner notes ({})\n\nSpend this run's ledger cannot count, and what \
                 was done about it. These are not failures.\n",
                self.runner_notes.len()
            ));
            for note in &self.runner_notes {
                out.push_str(&format!("\n- {note}"));
            }
            out.push('\n');
        }
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
                for reason in &a.rejections {
                    line.push_str(&format!("\n  - turned down: {reason}"));
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
        standard_fixes: current.standard_fixes.clone(),
        detail_fallbacks: current.detail_fallbacks.clone(),
        runner_notes: current.runner_notes.clone(),
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
    fn the_batch_runners_notes_ride_along_in_a_report_that_exists() {
        let mut scan = report_with(vec![failed()]);
        scan.ledger.record_runner_notes(vec![
            "Batch msgbatch_x no longer exists at the provider.".to_string()
        ]);
        let fr = build(&scan);
        assert_eq!(
            fr.runner_notes,
            ["Batch msgbatch_x no longer exists at the provider."]
        );
        let md = fr.render_md();
        assert!(md.contains("msgbatch_x"), "{md}");
        // They survive the merge with an earlier report and the file round trip.
        let merged = merge_history(&FailureReport::default(), &fr);
        assert_eq!(merged.runner_notes, fr.runner_notes);
        let json = serde_json::to_string(&merged).unwrap();
        let back: FailureReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.runner_notes, fr.runner_notes);
        // A report written before the field existed still loads.
        let old: FailureReport =
            serde_json::from_str(r#"{"generated_at":"t","entries":[]}"#).unwrap();
        assert!(old.runner_notes.is_empty());
    }

    #[test]
    fn a_batch_note_alone_is_not_a_failure_and_writes_no_report() {
        let mut scan = report_with(Vec::new());
        scan.ledger
            .record_runner_notes(vec!["Batch msgbatch_x is gone.".to_string()]);
        assert!(build(&scan).is_empty());
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

    fn report_with_a_standard_fix() -> crate::onboard::ScanReport {
        let mut r = report_with(Vec::new());
        let mut f = r.findings.remove(0);
        f.repo = "acme/api".to_string();
        f.path = "tests/format.test.ts".to_string();
        f.line = 10;
        f.fix_specific = Some("The rule's own fix.".to_string());
        crate::report_export::mark_standard_fix(
            &mut f,
            "tests/format.test.ts:10 R-1: you named `zzRejectedName`, which appears neither in \
             this finding's evidence nor anywhere in the repository",
        );
        r.findings = vec![f];
        r
    }

    /// D8: a finding that ships a Standard fix is listed for the operator with the reason the
    /// checks turned the model's fix down, is not a failed part, and keeps the files written
    /// even though nothing failed.
    #[test]
    fn every_standard_fix_is_listed_with_its_rejection_reason_and_is_not_a_failure() {
        let fr = build(&report_with_a_standard_fix());
        assert!(fr.entries.is_empty(), "not a failed part");
        assert!(fr.stderr_lines().is_empty(), "not printed as a failure");
        assert_eq!(fr.standard_fixes.len(), 1);
        let s = &fr.standard_fixes[0];
        assert_eq!((s.path.as_str(), s.line), ("tests/format.test.ts", 10));
        assert!(s.reasons.contains("zzRejectedName"), "{}", s.reasons);
        let md = fr.render_md();
        assert!(md.contains("Standard fixes (1)"), "{md}");
        assert!(md.contains("zzRejectedName"), "{md}");
        assert!(md.contains("tests/format.test.ts:10"), "{md}");

        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("x-1234.ledger.json");
        let (json, _md) = write(&ledger, &fr).unwrap().expect("written");
        let back: FailureReport =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(back.standard_fixes, fr.standard_fixes);
        // A report written before this field existed still loads.
        let old: FailureReport =
            serde_json::from_str(r#"{"generated_at":"t","entries":[]}"#).unwrap();
        assert!(old.standard_fixes.is_empty());
    }

    /// A run with no Standard fix and no failure lists none.
    #[test]
    fn a_clean_run_lists_no_standard_fixes() {
        assert!(build(&report_with(Vec::new())).standard_fixes.is_empty());
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
