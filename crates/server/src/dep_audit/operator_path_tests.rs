//! A dependency check that fails at scan time tells the client one fixed sentence, and tells the
//! operator log everything osv-scanner said (#557). osv-scanner's stderr holds paths of the
//! machine that ran it; none of it is stored in a note a client export reads.

use std::io::Write;
use std::sync::{Arc, Mutex};

use super::*;
use crate::onboard::scope::RepoScope;

/// A fake `osv-scanner`: `--version` works; a scan writes a path of the operator's machine to
/// stderr and exits 2, which osv-scanner uses for a tool error.
const FAILING_OSV: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "osv-scanner 0.0.0"; exit 0; fi
echo "failed to open /Users/auditor/Library/Application Support/osv-scanner/db.json: permission denied" >&2
exit 2
"#;

/// The sentence the client reads.
const FIXED_WORDING: &str = "dependency audit (osv-scanner) did not run on this repository";

/// A log writer the test can read back.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_osv_failure_stores_fixed_wording_and_logs_the_stderr_for_the_operator() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let scan_dir = root.path().join("scan");
    let tooling = root.path().join("tooling");
    for d in [&bin, &scan_dir, &tooling] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(scan_dir.join("package-lock.json"), "{}").unwrap();
    let osv = bin.join("osv-scanner");
    std::fs::write(&osv, FAILING_OSV).unwrap();
    std::fs::set_permissions(&osv, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The fake is handed over by its path, as the pinned osv-scanner is (#587).
    let _ = &tooling;

    let log = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);

    let (findings, note) = run_dep_audit_core(
        "acme/web",
        &scan_dir,
        &scan_dir,
        &osv,
        std::ffi::OsStr::new("/usr/bin:/bin"),
        &RepoScope::none(),
    )
    .await;

    assert!(findings.is_empty());
    let note = note.expect("a failed check leaves a note");
    assert!(
        note.message.contains(FIXED_WORDING),
        "the stored note is not the fixed wording: {}",
        note.message
    );
    for tool_word in [
        "permission denied",
        "db.json",
        "Application Support",
        "code 2",
    ] {
        assert!(
            !note.message.contains(tool_word),
            "the stored note holds the tool's own words ({tool_word:?}): {}",
            note.message
        );
    }
    let logged = String::from_utf8_lossy(&log.0.lock().unwrap()).into_owned();
    assert!(
        logged.contains("permission denied") && logged.contains("/Users/auditor"),
        "the operator log lacks osv-scanner's stderr: {logged}"
    );
}

// ---- the export-time rewrite of a stored note ------------------------------------------------

#[test]
fn a_stored_note_with_the_tools_words_becomes_fixed_wording() {
    for stored in [
        "dependency audit (osv-scanner) did not run: process exited with code 2: boom /Users/auditor/b",
        "dependency audit (osv-scanner) did not run: could not spawn process: /Users/auditor/osv",
        "dependency audit (osv-scanner) did not run: download failed (network unavailable?): \
         error sending request for url (https://example.test/x)",
        "dependency audit (osv-scanner) did not run: no network, tried /Users/auditor/cache",
        "dependency audit (osv-scanner) could not parse output: bad token /Users/auditor/o.json",
    ] {
        assert_eq!(client_note_message(stored), FIXED_WORDING, "{stored}");
    }
}

#[test]
fn a_note_the_engine_wrote_itself_is_kept_as_written() {
    for kept in [
        "dependency audit (osv-scanner) did not run: subprocess timed out after 120 s",
        "dependency audit (osv-scanner) did not run: it exited normally but produced no output",
        "dependency audit (osv-scanner) did not run: no network",
        "dependency audit (osv-scanner) found no lockfiles in acme/web; no dependency exposure detected",
        "Dependency manifests not audited (no lockfile): web/package.json.",
        "dependency audit did not run: network unavailable",
        FIXED_WORDING,
    ] {
        assert_eq!(client_note_message(kept), kept);
    }
}

#[test]
fn a_stored_unread_files_sentence_keeps_its_meaning_and_loses_the_quoted_lines() {
    let stored = "Dependency manifests not audited (no lockfile): web/package.json. Dependency \
                  files not read: osv-scanner reported problems reading some files, so they may \
                  not have been checked (failed to parse /Users/auditor/x.lock: bad (nested) json).";
    assert_eq!(
        client_note_message(stored),
        "Dependency manifests not audited (no lockfile): web/package.json. Dependency files not \
         read: osv-scanner reported problems reading some files, so they may not have been \
         checked."
    );
}

#[test]
fn an_older_unread_sentence_keeps_the_text_after_its_parenthesis() {
    // `with_copy_failures` appends the not-copied sentence after the unread sentence; only the
    // parenthetical (with its own nested parentheses) goes.
    let tail = "Dependency files not copied: 2 files, so they may not have been checked.";
    let stored = format!(
        "Dependency files not read: osv-scanner reported problems reading some files, so they may \
         not have been checked (failed to parse /Users/auditor/x.lock: bad (nested) json). {tail}"
    );
    let rendered = client_note_message(&stored);
    assert!(!rendered.contains("/Users/auditor"), "{rendered}");
    assert!(!rendered.contains("nested"), "{rendered}");
    assert!(rendered.ends_with(tail), "{rendered}");
    assert_eq!(
        rendered,
        format!(
            "Dependency files not read: osv-scanner reported problems reading some files, so \
             they may not have been checked. {tail}"
        )
    );
}

#[test]
fn an_engine_reason_is_matched_whole_so_a_longer_reason_is_still_rewritten() {
    // A trailing full stop or space does not change the reason.
    let prefix = "dependency audit (osv-scanner) did not run: ";
    for kept in ["no network.", "no network ", "no network. "] {
        let note = format!("{prefix}{kept}");
        assert_eq!(client_note_message(&note), note, "{note:?}");
    }
    // The same words with the tool's own after them are not the engine's reason.
    for rewritten in [
        "subprocess timed out after 120 s: killed /Users/auditor/osv",
        "subprocess timed out after 5 s",
        "it exited normally but produced no output /Users/auditor/o",
        "no network: dial tcp /Users/auditor/sock",
        "no networks",
    ] {
        assert_eq!(
            client_note_message(&format!("{prefix}{rewritten}")),
            FIXED_WORDING,
            "{rewritten}"
        );
    }
}
