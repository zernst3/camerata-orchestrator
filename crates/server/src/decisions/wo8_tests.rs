//! WO8: the decision follow-ups. A restored thrown-out finding is a decision, not a text change;
//! one hold list; the store alone once it has records; the client's acknowledgement.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use crate::engine_discard::{self, RestoredKeys};
use crate::inspections::{InspectionEntry, InspectionKind, PackageRecord};
use crate::onboard::{Finding, ScanReport};
use crate::release_gate::evidence::ReviewerDecisions;
use crate::{router, AppState};

// ── fixtures ──────────────────────────────────────────────────────────────────────────────

const NOW: &str = "2026-11-04T09:00:00Z";

fn ctx(run_id: &str) -> DecisionCtx<'_> {
    DecisionCtx {
        run_id,
        engine_version: "0.0.0-test",
        now: NOW,
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

/// A model-invented row whose own text says to discard it: the seal's safety net throws it out.
fn discard_shaped_row() -> Finding {
    Finding {
        rule_id: "AI-NOTE".to_string(),
        detail: "A title \u{2014} Discard this finding.".to_string(),
        ..row("AI-NOTE", "src/note.rs", 4, "db.query(q)")
    }
}

fn report_of(rows: Vec<Finding>) -> ScanReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.findings = rows;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:00:00Z".to_string();
    report
}

fn req(key: &str, state: DecisionState, reason: &str) -> NewDecisionReq {
    NewDecisionReq {
        scope: Scope::Site,
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        decided_by: "zach".to_string(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: String::new(),
        decided_at: String::new(),
    }
}

fn node_req(node_id: &str, state: DecisionState, reason: &str) -> NewDecisionReq {
    NewDecisionReq {
        scope: Scope::Node(node_id.to_string()),
        ..req("", state, reason)
    }
}

fn app_with_project() -> (AppState, axum::Router, String) {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["acme/app".to_string()])
        .expect("project");
    let app = router(state.clone());
    (state, app, project.id)
}

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

fn to_json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap()
}

// ── A12 item 2: a restore is a decision ───────────────────────────────────────────────────

#[test]
fn a_restored_row_is_never_thrown_out_again_on_seal_or_rerun() {
    let kept = row("SEC-1", "src/a.rs", 10, "let a = secret_a();");
    let report = report_of(vec![kept, discard_shaped_row()]);

    // Without a restore the seal's safety net throws the row out, and keeps it apart.
    let sealed = report.clone().seal();
    assert_eq!(sealed.discarded.len(), 1, "the baseline throws it out");
    let key = sealed.discarded[0].site_key.clone();
    assert!(!key.is_empty());

    // The reviewer puts it back: one decision in the store.
    let store = DecisionStore::new();
    store
        .record_site(
            "p",
            &req(
                &key,
                DecisionState::EngineRestored,
                "a real problem: it names the query",
            ),
            ctx("insp-1"),
        )
        .unwrap();
    let restored = restored_keys_of(&store.active("p"));
    assert!(!restored.is_empty());

    // The stored report is sealed again (an export, a reload): the row stays among the kept
    // rows. And again: the seal does not undo it a second time.
    let once = sealed.into_inner().seal_with_restored(&restored);
    assert!(once.discarded.is_empty(), "{:?}", once.discarded);
    assert!(once.findings.iter().any(|f| f.site_key == key));
    let twice = once.into_inner().seal_with_restored(&restored);
    assert!(twice.discarded.is_empty(), "{:?}", twice.discarded);
    assert!(twice.findings.iter().any(|f| f.site_key == key));
    assert!(twice.discarded_reconciles(), "the ledger still reconciles");
    let back = twice.findings.iter().find(|f| f.site_key == key).unwrap();
    assert!(!engine_discard::is_discarded(back));
    assert!(back
        .captures
        .contains_key(engine_discard::ENGINE_RESTORED_CAPTURE));

    // A rerun: a fresh report whose row carries no mark yet. The safety net skips it.
    let rerun = report.seal_with_restored(&restored);
    assert!(rerun.discarded.is_empty(), "{:?}", rerun.discarded);
    assert!(rerun.findings.iter().any(|f| f.site_key == key));
}

#[test]
fn every_throw_out_step_skips_a_restored_row() {
    let mut rows = vec![discard_shaped_row()];
    let key = crate::report_export::finding_key(&rows[0]);
    let restored = RestoredKeys::from_keys([key.as_str()]);
    let thrown = crate::junk_finding::take_thrown_out(&mut rows, &restored);
    assert!(thrown.is_empty(), "the seal's safety net skips it");
    assert_eq!(rows.len(), 1);

    // The ordinal is not part of the match: a throw-out step can run before the final keys.
    let ordinal_other = format!("{}9", key.trim_end_matches('0'));
    assert!(RestoredKeys::from_keys([ordinal_other.as_str()]).covers(&rows[0]));
}

#[test]
fn a_restored_row_is_held_until_decided() {
    let kept = row("SEC-1", "src/a.rs", 10, "let a = secret_a();");
    let key = crate::report_export::finding_key(&kept);
    let store = DecisionStore::new();
    store
        .record_site(
            "p",
            &req(&key, DecisionState::EngineRestored, "the engine was wrong"),
            ctx("insp-1"),
        )
        .unwrap();

    // Held from every batch decision, and decided by nobody yet.
    let held = held_keys(std::slice::from_ref(&kept), &store.active("p"), &none());
    assert!(held.contains(&key), "{held:?}");
    assert!(
        !StoreDecisions::for_project(&store, "p").has_decision(&kept),
        "a restore decides nothing"
    );

    // The reviewer's ruling releases the hold and counts as the decision.
    store
        .record_site("p", &req(&key, DecisionState::Confirmed, ""), ctx("insp-1"))
        .unwrap();
    let held = held_keys(std::slice::from_ref(&kept), &store.active("p"), &none());
    assert!(!held.contains(&key), "{held:?}");
    assert!(StoreDecisions::for_project(&store, "p").has_decision(&kept));
}

#[test]
fn a_restore_needs_a_reason() {
    let store = DecisionStore::new();
    let refused = store.record_site(
        "p",
        &req("k1", DecisionState::EngineRestored, "  "),
        ctx("insp-1"),
    );
    assert!(
        matches!(refused, Err(DecisionStoreError::ReasonRequired(_))),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_no_on_a_thrown_out_finding_restores_and_holds_it() {
    let (state, app, pid) = app_with_project();
    let mut report = report_of(vec![row("SEC-1", "src/a.rs", 10, "let a = secret_a();")]);
    report.discarded = vec![discard_shaped_row()];
    let entry = state
        .inspections
        .append(InspectionEntry::from_report(
            &pid,
            report,
            InspectionKind::Full,
        ))
        .unwrap();
    let thrown_key = entry.report.discarded[0].site_key.clone();
    let uri = format!("/api/projects/{pid}/decisions");

    // A restore needs a reason (who and why), and a row the engine actually threw out.
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(req(
            &thrown_key,
            DecisionState::EngineRestored,
            "",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "reason_required");
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(req(
            "not\u{0}a\u{0}thrown\u{0}row",
            DecisionState::EngineRestored,
            "no such row",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "not_thrown_out");

    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(req(
            &thrown_key,
            DecisionState::EngineRestored,
            "a real problem: the query is built from input",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The row is back among the reviewed rows, no longer marked thrown out, and held.
    let records = state.decisions.list(&pid);
    let sealed = entry
        .report
        .clone()
        .seal_with_restored(&restored_keys_of(&state.decisions.active(&pid)));
    let reviewed = crate::report_export::apply_overlay(&sealed, &records, &[], &[]);
    let row = reviewed
        .report()
        .findings
        .iter()
        .find(|f| f.site_key == thrown_key)
        .expect("the restored row is among the reviewed rows");
    assert!(!engine_discard::is_discarded(row));
    assert!(reviewed.report().discarded.is_empty());
    let held = held_keys(
        &reviewed.report().findings,
        &state.decisions.active(&pid),
        &none(),
    );
    assert!(held.contains(&thrown_key));

    // The release gate refuses it until it is decided.
    let problems = crate::release_gate::held::restored_without_decision(
        &reviewed.report().findings,
        &reviewed,
    );
    assert_eq!(problems.len(), 1, "{problems:?}");

    // Zach's ruling releases it.
    state
        .decisions
        .record_site(
            &pid,
            &req(&thrown_key, DecisionState::Confirmed, ""),
            ctx(&entry.id),
        )
        .unwrap();
    let reviewed =
        crate::report_export::apply_overlay(&sealed, &state.decisions.list(&pid), &[], &[]);
    assert!(crate::release_gate::held::restored_without_decision(
        &reviewed.report().findings,
        &reviewed
    )
    .is_empty());
}

// ── one hold list ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn held_keys_is_the_only_hold_list() {
    // One rule node with a plain row and a row for each reason a row is held.
    let plain = row("SEC-1", "src/plain.rs", 1, "let p = plain();");
    let mut unverified = row("SEC-1", "src/unverified.rs", 2, "let u = unverified();");
    unverified.captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found near the cited line".to_string(),
    );
    let mut junk = row("SEC-1", "src/junk.rs", 3, "let j = junk();");
    junk.captures.insert(
        crate::junk_finding::JUNK_CONTENT_CAPTURE.to_string(),
        "placeholder headline".to_string(),
    );
    let merged = row("SEC-1", "src/merged.rs", 4, "let m = merged();");
    let restored = row("SEC-1", "src/restored.rs", 5, "let r = restored();");

    let (state, app, pid) = app_with_project();
    let entry = state
        .inspections
        .append(InspectionEntry::from_report(
            &pid,
            report_of(vec![
                plain.clone(),
                unverified.clone(),
                junk.clone(),
                merged.clone(),
                restored.clone(),
            ]),
            InspectionKind::Full,
        ))
        .unwrap();
    let key_of = |path: &str| {
        entry
            .report
            .findings
            .iter()
            .find(|f| f.path == path)
            .unwrap()
            .site_key
            .clone()
    };
    let (plain_k, unverified_k, junk_k, merged_k, restored_k) = (
        key_of("src/plain.rs"),
        key_of("src/unverified.rs"),
        key_of("src/junk.rs"),
        key_of("src/merged.rs"),
        key_of("src/restored.rs"),
    );
    state
        .decisions
        .record_site(
            &pid,
            &req(
                &merged_k,
                DecisionState::MergedInto(plain_k.clone()),
                "same cause",
            ),
            ctx(&entry.id),
        )
        .unwrap();
    state
        .decisions
        .record_site(
            &pid,
            &req(
                &restored_k,
                DecisionState::EngineRestored,
                "the engine was wrong",
            ),
            ctx(&entry.id),
        )
        .unwrap();

    // Each reason is in the hold list, from one function.
    let held = held_keys(
        &entry.report.findings,
        &state.decisions.active(&pid),
        &none(),
    );
    for (reason, key) in [
        ("evidence unverified", &unverified_k),
        ("junk", &junk_k),
        ("merged", &merged_k),
        ("restored and undecided", &restored_k),
    ] {
        assert!(held.contains(key), "{reason} is not held: {held:?}");
    }
    assert!(!held.contains(&plain_k));

    // A node decision leaves every held row out, by the route.
    let node = entry
        .report
        .tree
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.level == crate::review_tree::NodeLevel::Rule)
        .unwrap()
        .id
        .clone();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/projects/{pid}/decisions"),
        Some(to_json(&PostDecisionsReq::Decide(node_req(
            &node,
            DecisionState::Confirmed,
            "the pattern is real",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let done: DecisionRecorded = serde_json::from_value(body).unwrap();
    assert_eq!(done.inherited_count, 1, "only the plain row is covered");
    let mut left_out = done.decision.except.clone();
    left_out.sort();
    let mut want = vec![unverified_k, junk_k, merged_k, restored_k];
    want.sort();
    assert_eq!(left_out, want);

    // And by the store's own `except` path.
    let store = DecisionStore::new();
    let members: Vec<String> = entry
        .report
        .findings
        .iter()
        .map(|f| f.site_key.clone())
        .collect();
    let mut with_held = node_req(&node, DecisionState::Confirmed, "r");
    with_held.except = held.iter().cloned().collect();
    let (record, written) = store
        .record_node("q", &node, &members, &with_held, ctx("insp-1"))
        .unwrap();
    assert_eq!(written, 1);
    assert_eq!(record.applies_to, vec![plain_k]);

    // The route and the apply step have no hold list of their own: neither names a hold capture.
    for (file, text) in [
        ("routes.rs", include_str!("routes.rs")),
        ("apply.rs", include_str!("apply.rs")),
    ] {
        for capture in [
            "EVIDENCE_UNVERIFIED_CAPTURE",
            "JUNK_CONTENT_CAPTURE",
            "ENGINE_RESTORED_CAPTURE",
        ] {
            assert!(
                !text.contains(capture),
                "{file} reads {capture}; every hold reason lives in held_keys"
            );
        }
    }
}

// ── the store alone ───────────────────────────────────────────────────────────────────────

fn wire(state: &str, reason: &str, bucket: &str) -> crate::report_export::DispositionWire {
    crate::report_export::DispositionWire {
        state: state.to_string(),
        reason: reason.to_string(),
        bucket: bucket.to_string(),
        confirmed_by_client: false,
    }
}

#[test]
fn decisions_come_from_the_store_only_once_it_has_records() {
    let store = DecisionStore::new();
    let mut from_request = HashMap::new();
    from_request.insert("k1".to_string(), wire("Ignored", "from the draft", "Later"));
    from_request.insert("k9".to_string(), wire("Confirmed", "", "Later"));

    // An empty store: the request's dispositions are used as they are (one more release).
    let e = effective_dispositions(&store, "p", &from_request);
    assert!(!e.from_store);
    assert_eq!(e.map, from_request);

    store
        .record_site(
            "p",
            &req("k1", DecisionState::TechDebtNow, "pull it in"),
            ctx("insp-1"),
        )
        .unwrap();

    // Once the store holds a record, the request contributes nothing: not even for a key the
    // store has no record for.
    let e = effective_dispositions(&store, "p", &from_request);
    assert!(e.from_store);
    assert_eq!(e.map.len(), 1, "{:?}", e.map);
    assert_eq!(e.map["k1"], wire("TechDebt", "pull it in", "Now"));
    assert!(!e.map.contains_key("k9"), "the request's k9 is not used");

    // A key the store revoked is undecided: the request's entry for it is NOT used.
    let k1 = store.list("p").into_iter().find(|d| d.key == "k1").unwrap();
    store.revoke("p", &k1.id, NOW).unwrap();
    store
        .record_site(
            "p",
            &req("k2", DecisionState::FalsePositive, "generated"),
            ctx("i"),
        )
        .unwrap();
    let e = effective_dispositions(&store, "p", &from_request);
    assert!(!e.map.contains_key("k1"), "{:?}", e.map);
    assert!(!e.map.contains_key("k9"));
    assert_eq!(e.map["k2"].state, "FalsePositive");
}

// ── the client's acknowledgement ──────────────────────────────────────────────────────────

fn ack(by: &str, note: &str) -> ClientAck {
    ClientAck {
        by: by.to_string(),
        at: NOW.to_string(),
        note: note.to_string(),
    }
}

#[test]
fn the_store_acknowledges_only_an_ignored_decision_and_a_group_with_its_records() {
    let store = DecisionStore::new();
    let ignored = store
        .record_site(
            "p",
            &req("k1", DecisionState::Ignored, "defense in depth"),
            ctx("i"),
        )
        .unwrap();
    let confirmed = store
        .record_site("p", &req("k2", DecisionState::Confirmed, ""), ctx("i"))
        .unwrap();

    assert!(matches!(
        store.ack_client("p", &confirmed.id, ack("cto@acme.test", "ok")),
        Err(DecisionStoreError::AckNeedsIgnored(_))
    ));
    assert!(matches!(
        store.ack_client("p", &ignored.id, ack("  ", "ok")),
        Err(DecisionStoreError::AckNeedsWho)
    ));
    let (acked, also) = store
        .ack_client(
            "p",
            &ignored.id,
            ack("cto@acme.test", "accepted on the call"),
        )
        .expect("an Ignored decision can be acknowledged");
    assert_eq!(also, 0);
    let ack_record = acked.confirmed_by_client.expect("stamped");
    assert_eq!(ack_record.by, "cto@acme.test");
    assert!(matches!(
        store.ack_client("p", &ignored.id, ack("cto@acme.test", "again")),
        Err(DecisionStoreError::AlreadyAcknowledged(_))
    ));

    // A group decision is acknowledged with every per-site record it wrote.
    let (node, written) = store
        .record_node(
            "p",
            "rule:SEC-1",
            &["a".to_string(), "b".to_string()],
            &node_req("rule:SEC-1", DecisionState::Ignored, "test fixtures"),
            ctx("i"),
        )
        .unwrap();
    let (_, also) = store
        .ack_client("p", &node.id, ack("cto@acme.test", ""))
        .unwrap();
    assert_eq!(also, written);
    assert!(store
        .active("p")
        .iter()
        .filter(|d| d.inherited_from.as_deref() == Some(node.id.as_str()))
        .all(|d| d.confirmed_by_client.is_some()));
}

#[tokio::test]
async fn confirmed_by_client_is_false_until_the_client_ack_route_is_called() {
    let (state, app, pid) = app_with_project();
    let entry = state
        .inspections
        .append(InspectionEntry::from_report(
            &pid,
            report_of(vec![row("SEC-1", "src/a.rs", 10, "let a = secret_a();")]),
            InspectionKind::Full,
        ))
        .unwrap();
    let key = entry.report.findings[0].site_key.clone();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/projects/{pid}/decisions"),
        Some(to_json(&PostDecisionsReq::Decide(req(
            &key,
            DecisionState::Ignored,
            "defense in depth only",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let decided: DecisionRecorded = serde_json::from_value(body).unwrap();
    assert!(decided.decision.confirmed_by_client.is_none());

    let label_of = |records: &[Decision]| -> String {
        let sealed = entry.report.clone().seal();
        let reviewed = crate::report_export::apply_overlay(&sealed, records, &[], &[]);
        let json = crate::report_export::build_report_json(
            &reviewed,
            None,
            &crate::report_export::ReportOptions::default(),
        );
        serde_json::to_string(&json).unwrap()
    };
    let before = label_of(&state.decisions.list(&pid));
    assert!(before.contains("Needs client confirmation"), "{before}");
    assert!(!before.contains("Accepted risk"), "{before}");

    let uri = format!(
        "/api/projects/{pid}/decisions/{}/client-ack",
        decided.decision.id
    );
    let body = to_json(&camerata_api_types::edits::ClientAckReq {
        by: "cto@acme.test".to_string(),
        note: "accepted on the call".to_string(),
    });

    // Not before delivery: nothing has been handed to the client yet.
    let (status, refused) = call(&app, "POST", &uri, Some(body.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "not_delivered");
    assert!(state.decisions.list(&pid)[0].confirmed_by_client.is_none());

    state
        .inspections
        .set_package(
            &pid,
            PackageRecord {
                generated_at: NOW.to_string(),
                zip_sha256: "00".to_string(),
                manifest_json: serde_json::Value::Null,
            },
        )
        .unwrap();
    let (status, done) = call(&app, "POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let recorded: camerata_api_types::edits::ClientAckRecorded =
        serde_json::from_value(done).unwrap();
    assert_eq!(
        recorded
            .decision
            .confirmed_by_client
            .as_ref()
            .map(|a| a.by.as_str()),
        Some("cto@acme.test")
    );

    let after = label_of(&state.decisions.list(&pid));
    assert!(
        after.contains("Accepted risk: defense in depth only"),
        "{after}"
    );
    assert!(!after.contains("Needs client confirmation"), "{after}");
}

/// No Engine Check evidence: the hold list of these tests is the decisions' and the rows' alone.
fn none() -> crate::engine_check::EngineCheckEvidence {
    crate::engine_check::EngineCheckEvidence::NotRecorded
}
