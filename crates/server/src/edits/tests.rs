//! The edit store: append-only, two edits on one field both kept, a damaged file never replaced.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use crate::inspections::{InspectionEntry, InspectionKind};
use crate::onboard::{Finding, ScanReport};
use crate::{router, AppState};

const NOW: &str = "2026-11-04T09:00:00Z";

fn ctx() -> EditCtx<'static> {
    EditCtx {
        run_id: "insp-1",
        now: NOW,
    }
}

fn draft(key: &str, field: EditField, old: &str, new: &str) -> EditDraft {
    EditDraft {
        key: key.to_string(),
        field,
        old: old.to_string(),
        new: new.to_string(),
        reason: "the engine's wording was wrong".to_string(),
        by: "zach".to_string(),
        run_id: String::new(),
        fingerprint: "fp-1".to_string(),
    }
}

fn row(rule_id: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        detail: format!("Title for {rule_id} \u{2014} The handler trusts the caller here."),
        ..Finding::default()
    }
}

#[test]
fn two_edits_on_one_field_both_kept_and_the_later_applies() {
    let store = EditStore::new();
    let first = store
        .record_edit(
            "p",
            draft("k1", EditField::Headline, "Engine title", "First rewrite"),
            ctx(),
        )
        .unwrap();
    let second = store
        .record_edit(
            "p",
            draft("k1", EditField::Headline, "First rewrite", "Second rewrite"),
            ctx(),
        )
        .unwrap();
    assert_ne!(first.id, second.id);
    let kept = store.list("p");
    assert_eq!(kept.len(), 2, "both are kept");
    assert_eq!(kept[0].new, "First rewrite");
    assert_eq!(kept[1].new, "Second rewrite");

    // Applied in the order written: the later one is what the row says.
    let report = {
        let mut r = ScanReport::gated(&["acme/app".to_string()]);
        r.gated = false;
        r.findings = vec![row("SEC-1", "src/a.rs", 10, "let a = secret_a();")];
        r.seal()
    };
    let key = report.findings[0].site_key.clone();
    let fp = camerata_api_types::fingerprint::fingerprint("SEC-1", "let a = secret_a();");
    let mut edits = Vec::new();
    for (old, new) in [
        ("Engine title", "First rewrite"),
        ("First rewrite", "Second rewrite"),
    ] {
        let mut d = draft(&key, EditField::Headline, old, new);
        d.fingerprint = fp.clone();
        edits.push(store.record_edit("q", d, ctx()).unwrap());
    }
    let reviewed = crate::report_export::apply_overlay(&report, &[], &edits, &[]);
    let edited = &reviewed.report().findings[0];
    assert_eq!(crate::ai_audit::finding_headline(edited), "Second rewrite");
    let ids = crate::report_export::edit_ids_of(edited);
    assert_eq!(ids, vec![edits[0].id.clone(), edits[1].id.clone()]);
    assert_eq!(reviewed.applied_edits().len(), 2);
    // The sealed report is untouched.
    assert_eq!(
        crate::ai_audit::finding_headline(&report.findings[0]),
        "Title for SEC-1"
    );
}

#[test]
fn an_edit_needs_a_key_a_valid_value_and_a_reason_where_it_makes_a_claim() {
    let store = EditStore::new();
    let mut no_key = draft("  ", EditField::Headline, "a", "b");
    no_key.key = "  ".to_string();
    assert!(matches!(
        store.record_edit("p", no_key, ctx()),
        Err(EditStoreError::KeyRequired)
    ));
    let mut no_reason = draft("k", EditField::Severity, "high", "low");
    no_reason.reason = " ".to_string();
    assert!(matches!(
        store.record_edit("p", no_reason, ctx()),
        Err(EditStoreError::ReasonRequired(_))
    ));
    // Rewording owes no reason.
    let mut reword = draft("k", EditField::Headline, "a", "b");
    reword.reason = String::new();
    assert!(store.record_edit("p", reword, ctx()).is_ok());
    assert!(matches!(
        store.record_edit(
            "p",
            draft("k", EditField::Severity, "high", "urgent"),
            ctx()
        ),
        Err(EditStoreError::InvalidValue(_))
    ));
    assert!(matches!(
        store.record_edit("p", draft("k", EditField::FixType, "", "Best fix"), ctx()),
        Err(EditStoreError::InvalidValue(_))
    ));
    assert!(matches!(
        store.record_edit("p", draft("k", EditField::Kind, "", "deviation"), ctx()),
        Err(EditStoreError::InvalidValue(_))
    ));
    assert!(store
        .record_edit("p", draft("k", EditField::Kind, "", "defect"), ctx())
        .is_ok());
    assert!(matches!(
        store.record_edit("p", draft("k", EditField::Headline, "same", "same"), ctx()),
        Err(EditStoreError::NoChange)
    ));
}

#[test]
fn a_discarded_edit_is_kept_and_a_reapplied_one_points_back() {
    let store = EditStore::new();
    let old = store
        .record_edit("p", draft("k1", EditField::Headline, "a", "b"), ctx())
        .unwrap();
    let again = store
        .reapply(
            "p",
            &old.id,
            draft("k2", EditField::Headline, "a", "b"),
            ctx(),
        )
        .unwrap();
    assert_eq!(again.reapplied_from.as_deref(), Some(old.id.as_str()));
    let all = store.list("p");
    assert_eq!(all.len(), 2, "nothing is deleted");
    assert!(all[0].discarded_at.is_some(), "the stale edit is replaced");
    assert_eq!(store.active("p").len(), 1);
    assert!(matches!(
        store.discard("p", &old.id, NOW),
        Err(EditStoreError::AlreadyDiscarded(_))
    ));
    let gone = store.discard("p", &again.id, NOW).unwrap();
    assert!(gone.discarded_at.is_some());
    assert!(store.active("p").is_empty());
    assert!(matches!(
        store.discard("p", "edit-999999", NOW),
        Err(EditStoreError::NotFound(_))
    ));
}

#[test]
fn edits_and_added_findings_survive_a_restart_and_a_failed_write_leaves_the_mirror_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edits");
    let store = EditStore::at(path.clone());
    store
        .record_edit("p", draft("k1", EditField::Headline, "a", "b"), ctx())
        .unwrap();
    let mut added = row("SEC-9", "src/new.rs", 3, "danger();");
    added.site_key = "k-added".to_string();
    added.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    store
        .add_finding(
            "p",
            AddedFinding {
                finding: added,
                reason: "the scan missed it".to_string(),
                by: "zach".to_string(),
                at: NOW.to_string(),
                run_id: "insp-1".to_string(),
            },
        )
        .unwrap();
    let reopened = EditStore::at(path.clone());
    assert_eq!(reopened.list("p").len(), 1);
    assert_eq!(reopened.added("p").len(), 1);

    // A directory in the way of the write: the mirror keeps what it had.
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::write(&path, "a file where the directory should be").unwrap();
    let failed = store.record_edit("p", draft("k2", EditField::Headline, "a", "c"), ctx());
    assert!(
        matches!(failed, Err(EditStoreError::Persist(_))),
        "{failed:?}"
    );
    assert_eq!(store.list("p").len(), 1);
}

#[test]
fn an_added_finding_needs_its_evidence_a_kind_and_a_reason() {
    let store = EditStore::new();
    let complete = || {
        let mut f = row("SEC-9", "src/new.rs", 3, "danger();");
        f.site_key = "k-added".to_string();
        f.kind = Some(camerata_api_types::audit::FindingKind::Defect);
        AddedFinding {
            finding: f,
            reason: "the scan missed it".to_string(),
            by: "zach".to_string(),
            at: NOW.to_string(),
            run_id: String::new(),
        }
    };
    let mut no_kind = complete();
    no_kind.finding.kind = None;
    assert!(matches!(
        store.add_finding("p", no_kind),
        Err(EditStoreError::AddedFindingIncomplete(_))
    ));
    let mut no_snippet = complete();
    no_snippet.finding.snippet = " ".to_string();
    assert!(matches!(
        store.add_finding("p", no_snippet),
        Err(EditStoreError::AddedFindingIncomplete(_))
    ));
    let mut no_reason = complete();
    no_reason.reason = String::new();
    assert!(matches!(
        store.add_finding("p", no_reason),
        Err(EditStoreError::ReasonRequired(_))
    ));
    assert!(store.add_finding("p", complete()).is_ok());
    assert!(store.add_finding("p", complete()).is_err(), "no duplicate");
}

// ── a damaged file ────────────────────────────────────────────────────────────────────────

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_damaged_edit_store_answers_503_and_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let edits_dir = dir.path().join("edits");
    let mut state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state.edits = EditStore::at(edits_dir.clone());
    let project = state
        .projects
        .create("Acme", vec!["acme/app".to_string()])
        .unwrap();
    let pid = project.id;
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.findings = vec![row("SEC-1", "src/a.rs", 10, "let a = secret_a();")];
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:00:00Z".to_string();
    let entry = state
        .inspections
        .append(InspectionEntry::from_report(
            &pid,
            report,
            InspectionKind::Full,
        ))
        .unwrap();
    let key = entry.report.findings[0].site_key.clone();
    state
        .edits
        .record_edit(&pid, draft(&key, EditField::Headline, "a", "b"), ctx())
        .unwrap();
    let file = state.edits.file_for(&pid).unwrap();
    std::fs::write(&file, "{ this is not json").unwrap();

    // A restart finds it damaged.
    state.edits = EditStore::at(edits_dir);
    let app = router(state.clone());
    let base = format!("/api/projects/{pid}");
    let new_edit = serde_json::json!({
        "key": key, "field": "headline", "new": "A better title", "reason": "", "by": "zach"
    });
    for (method, uri, body) in [
        ("GET", format!("{base}/edits"), None),
        ("POST", format!("{base}/edits"), Some(new_edit.clone())),
        (
            "POST",
            format!("{base}/edits/split"),
            Some(serde_json::json!({
                "keys": [key], "subgroup": "x", "reason": "r", "by": "zach"
            })),
        ),
        ("POST", format!("{base}/edits/edit-000001/discard"), None),
        ("POST", format!("{base}/edits/edit-000001/reapply"), None),
        (
            "POST",
            format!("{base}/added-findings"),
            Some(serde_json::json!({
                "repo": "acme/app", "path": "src/n.rs", "line": 1, "rule_id": "SEC-9",
                "severity": "high", "snippet": "x", "headline": "h", "detail": "d",
                "kind": { "kind": "defect" }, "reason": "r"
            })),
        ),
        ("GET", format!("{base}/reviewed-report"), None),
    ] {
        let (status, body) = call(&app, method, &uri, body).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri} must answer 503 for a damaged store: {body}"
        );
        assert_eq!(body["code"], "store_corrupt", "{method} {uri}: {body}");
    }

    // The export is refused too: nothing is exported from guesses.
    let (status, body) = call(
        &app,
        "POST",
        &format!("{base}/product-export"),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    // The damaged file is kept aside byte for byte, nothing was written over it, and a direct
    // write is refused as well.
    assert_eq!(
        std::fs::read_to_string(file.with_extension("json.corrupt")).unwrap(),
        "{ this is not json"
    );
    assert!(!file.exists(), "no empty store was created over it");
    assert!(matches!(
        state
            .edits
            .record_edit(&pid, draft(&key, EditField::Headline, "a", "c"), ctx())
            .unwrap_err(),
        EditStoreError::Corrupt
    ));
    assert!(!state.edits.is_corrupt("some-other-project"));
}
