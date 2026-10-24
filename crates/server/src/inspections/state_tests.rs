//! `AppState` behaviour around the inspection store: what `set_last_scan` persists, what
//! `get_last_scan` falls back to after a restart, and what an in-place amend does.

use tempfile::TempDir;

use crate::inspections::InspectionKind;
use crate::onboard::{AuditedRef, ScanReport};
use crate::AppState;

fn real_report(repo: &str, sha: &str, started: &str) -> ScanReport {
    let mut r = ScanReport::gated(&[repo.to_string()]);
    r.gated = false;
    r.message = None;
    r.provenance.audited_refs = vec![AuditedRef {
        repo: repo.to_string(),
        sha: Some(sha.to_string()),
        branch: Some("main".to_string()),
        ..AuditedRef::default()
    }];
    r.provenance.started_at = started.to_string();
    r.provenance.finished_at = started.to_string();
    r
}

#[test]
fn a_completed_scan_is_persisted_as_an_inspection_entry() {
    let state = AppState::from_data_dir(TempDir::new().unwrap().path());
    state.set_last_scan(
        "proj-1".to_string(),
        real_report("acme/app", "abc", "2026-10-24T10:00:00Z"),
    );

    let entry = state.inspections().latest("proj-1").expect("entry written");
    assert_eq!(entry.sha_for("acme/app"), Some("abc"));
    assert_eq!(entry.kind, InspectionKind::Full);
}

#[test]
fn gated_blocked_and_unaudited_reports_are_not_inspections() {
    let state = AppState::from_data_dir(TempDir::new().unwrap().path());

    // The connect-GitHub gate: nothing was scanned.
    state.set_last_scan("p".to_string(), ScanReport::gated(&["a/b".to_string()]));
    // A compliance block.
    let mut blocked = real_report("a/b", "s", "t");
    blocked.blocked = true;
    state.set_last_scan("p".to_string(), blocked);
    // A report that audited no repo at all (an empty provenance).
    let mut empty = real_report("a/b", "s", "t");
    empty.provenance.audited_refs.clear();
    state.set_last_scan("p".to_string(), empty);

    assert!(state.inspections().latest("p").is_none());
    // The in-memory last scan still serves the session, exactly as before.
    assert!(state.get_last_scan("p").is_some());
}

#[test]
fn get_last_scan_falls_back_to_persisted_entry_after_restart() {
    let tmp = TempDir::new().unwrap();
    let first = AppState::from_data_dir(tmp.path());
    let mut report = real_report("acme/app", "abc", "2026-10-24T10:00:00Z");
    report.message = Some("the delivered inspection".to_string());
    first.set_last_scan("proj-1".to_string(), report);
    drop(first);

    // A restart: a brand-new state over the same directory has an EMPTY in-memory map.
    let second = AppState::from_data_dir(tmp.path());
    let got = second
        .get_last_scan("proj-1")
        .expect("the persisted inspection answers after a restart");
    assert_eq!(got.message.as_deref(), Some("the delivered inspection"));
    assert!(second.get_last_scan("someone-else").is_none());
}

#[test]
fn the_in_memory_scan_wins_over_the_persisted_one_within_a_session() {
    let tmp = TempDir::new().unwrap();
    let state = AppState::from_data_dir(tmp.path());
    state.set_last_scan(
        "p".to_string(),
        real_report("a/b", "old", "2026-10-24T10:00:00Z"),
    );
    state.set_last_scan_in_memory(
        "p".to_string(),
        real_report("a/b", "new", "2026-10-24T11:00:00Z"),
    );
    assert_eq!(
        state.get_last_scan("p").unwrap().provenance.audited_refs[0]
            .sha
            .as_deref(),
        Some("new")
    );
}

#[test]
fn alternatives_rescan_updates_latest_entry_in_place() {
    let tmp = TempDir::new().unwrap();
    let state = AppState::from_data_dir(tmp.path());
    state.set_last_scan(
        "p".to_string(),
        real_report("a/b", "abc", "2026-10-24T10:00:00Z"),
    );
    let before = state.inspections().latest("p").unwrap();

    let mut merged = real_report("a/b", "abc", "2026-10-24T10:00:00Z");
    merged.message = Some("after the alternatives rescan".to_string());
    state.amend_last_scan("p".to_string(), merged);

    let all = state.inspections().list("p");
    assert_eq!(all.len(), 1, "an amend never appends a second entry");
    assert_eq!(all[0].id, before.id);
    assert_eq!(
        all[0].report.message.as_deref(),
        Some("after the alternatives rescan")
    );
    // And it is on disk: a restart sees the amended report.
    drop(state);
    let restarted = AppState::from_data_dir(tmp.path());
    assert_eq!(
        restarted.get_last_scan("p").unwrap().message.as_deref(),
        Some("after the alternatives rescan")
    );
}

#[test]
fn a_delta_retest_entry_carries_its_base_and_shas() {
    let state = AppState::from_data_dir(TempDir::new().unwrap().path());
    state.set_last_scan(
        "p".to_string(),
        real_report("a/b", "aaa", "2026-10-24T10:00:00Z"),
    );
    let base = state.inspections().latest("p").unwrap();

    let report = real_report("a/b", "bbb", "2026-10-25T10:00:00Z");
    state.record_inspection(
        "p",
        &report,
        InspectionKind::DeltaRetest {
            base_inspection_id: base.id.clone(),
            from_sha: "aaa".to_string(),
            to_sha: "bbb".to_string(),
        },
    );

    let latest = state.inspections().latest("p").unwrap();
    assert_eq!(
        latest.kind,
        InspectionKind::DeltaRetest {
            base_inspection_id: base.id,
            from_sha: "aaa".to_string(),
            to_sha: "bbb".to_string(),
        }
    );
    assert_eq!(state.inspections().list("p").len(), 2);
}

/// M2: the persisted inspection is a sealed artifact. A producer that missed redaction (the
/// sweep at seal time exists for exactly that) must not get its secret onto disk through the
/// store, which used to write the report as handed to it.
#[test]
fn a_secret_a_producer_failed_to_redact_never_reaches_the_inspection_file() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;

    let tmp = TempDir::new().unwrap();
    let state = AppState::from_data_dir(tmp.path());
    let secret = planted(SecretShape::StripeLiveSecretKey);
    let mut report = real_report("acme/app", "abc", "2026-10-24T10:00:00Z");
    report.findings.push(crate::onboard::Finding {
        repo: "acme/app".to_string(),
        path: ".env".to_string(),
        line: 3,
        rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
        severity: "critical".to_string(),
        snippet: secret.text.clone(),
        detail: format!("The line `{}` holds a credential.", secret.text),
        ..crate::onboard::Finding::default()
    });
    report.message = Some(format!("scan note carrying {}", secret.text));

    state.set_last_scan("proj-1".to_string(), report);

    let file = state.inspections().file_for("proj-1").expect("file-backed");
    let on_disk = std::fs::read_to_string(&file).expect("the inspection was persisted");
    assert!(
        secret.value.lines().all(|line| !on_disk.contains(line)),
        "the planted secret reached {}",
        file.display()
    );
    assert!(on_disk.contains("acme/app"), "the entry itself is there");
    // And the store's copy that the exporters read is the redacted one too.
    let entry = state.inspections().latest("proj-1").unwrap();
    assert!(!serde_json::to_string(&entry.report)
        .unwrap()
        .contains(&secret.text));
}

/// Forgetting a removed session marks the entry as cleared (so it is not mistaken for one that
/// never recorded any), and the mark survives a restart; an old file without the field reads as
/// not cleared.
#[test]
fn forgetting_sessions_marks_the_entry_cleared_and_it_persists() {
    use crate::llm::ModelSession;

    let tmp = TempDir::new().unwrap();
    let state = AppState::from_data_dir(tmp.path());
    let mut report = real_report("acme/app", "abc", "2026-10-24T10:00:00Z");
    report.provenance.model_sessions = vec![ModelSession {
        session_id: "6f1d2c3a-1111-4222-8333-444455556666".to_string(),
        cwd: "/work".to_string(),
        created_at: String::new(),
    }];
    state.set_last_scan("p".to_string(), report);
    let before = state.inspections().latest("p").unwrap();
    assert!(!before.sessions_unrecorded());

    state
        .inspections()
        .forget_model_sessions("p", &["6f1d2c3a-1111-4222-8333-444455556666".to_string()])
        .unwrap();

    let after = state.inspections().latest("p").unwrap();
    assert!(after.model_sessions.is_empty() && after.model_sessions_cleared);
    assert!(!after.sessions_unrecorded());
    drop(state);
    let restarted = AppState::from_data_dir(tmp.path());
    assert!(!restarted
        .inspections()
        .latest("p")
        .unwrap()
        .sessions_unrecorded());

    // An entry written before the field existed deserializes as "not cleared".
    let mut value = serde_json::to_value(&after).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("model_sessions_cleared");
    let old: crate::inspections::InspectionEntry = serde_json::from_value(value).unwrap();
    assert!(old.sessions_unrecorded());
}

/// The same sweep guards the in-place amend (the alternatives rescan rewrites the stored report).
#[test]
fn a_secret_in_an_amended_report_never_reaches_the_inspection_file_either() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;

    let tmp = TempDir::new().unwrap();
    let state = AppState::from_data_dir(tmp.path());
    state.set_last_scan(
        "p".to_string(),
        real_report("acme/app", "abc", "2026-10-24T10:00:00Z"),
    );
    let secret = planted(SecretShape::StripeLiveSecretKey);
    let mut merged = real_report("acme/app", "abc", "2026-10-24T10:00:00Z");
    merged.message = Some(format!("after the rescan: {}", secret.text));

    state.amend_last_scan("p".to_string(), merged);

    let on_disk = std::fs::read_to_string(state.inspections().file_for("p").unwrap()).unwrap();
    assert!(secret.value.lines().all(|line| !on_disk.contains(line)));
}
