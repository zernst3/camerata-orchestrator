//! The decision store, its application on a rerun, the routes and the draft migration (design
//! 3.5 to 3.7).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use crate::inspections::{InspectionEntry, InspectionKind};
use crate::onboard::{Finding, ScanReport};
use crate::release_gate::evidence::{unverified_without_decision, ReviewerDecisions};
use crate::review_tree::{match_runs, NodeLevel, ReviewTree};
use crate::{router, AppState};

// ── fixtures ──────────────────────────────────────────────────────────────────────────────

fn row(rule_id: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        detail: format!("detail for {rule_id}"),
        ..Finding::default()
    }
}

/// A report holding `rows`, with every row keyed and the tree built, the way a stored
/// inspection is.
fn entry_of(project: &str, rows: Vec<Finding>, started: &str) -> InspectionEntry {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.findings = rows;
    report.provenance.started_at = started.to_string();
    report.provenance.finished_at = started.to_string();
    InspectionEntry::from_report(project, report, InspectionKind::Full)
}

fn tree_of(rows: Vec<Finding>) -> (Vec<Finding>, ReviewTree) {
    let e = entry_of("p", rows, "2026-11-02T10:00:00Z");
    let tree = e.report.tree.clone().expect("a loaded report has a tree");
    (e.report.findings, tree)
}

/// The rule node that holds `rule_id`.
fn rule_node(tree: &ReviewTree, rule_id: &str) -> String {
    tree.nodes
        .iter()
        .find(|n| n.level == NodeLevel::Rule && n.rule_ids.iter().any(|r| r == rule_id))
        .unwrap_or_else(|| panic!("no rule node for {rule_id}"))
        .id
        .clone()
}

fn key_at(rows: &[Finding], path: &str) -> String {
    rows.iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .site_key
        .clone()
}

const NOW: &str = "2026-11-04T09:00:00Z";

fn ctx(run_id: &str) -> DecisionCtx<'_> {
    DecisionCtx {
        run_id,
        engine_version: "0.0.0-test",
        now: NOW,
    }
}

fn node_req(node_id: &str, state: DecisionState, reason: &str) -> NewDecisionReq {
    NewDecisionReq {
        scope: Scope::Node(node_id.to_string()),
        key: String::new(),
        state,
        reason: reason.to_string(),
        decided_by: "zach".to_string(),
        needed_me: None,
        sample_verdict: None,
    }
}

fn site_req(key: &str, state: DecisionState, reason: &str) -> NewDecisionReq {
    NewDecisionReq {
        scope: Scope::Site,
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        decided_by: "zach".to_string(),
        needed_me: None,
        sample_verdict: None,
    }
}

/// Three SEC-1 rows in three files, so the rule node has three members.
fn three_rows() -> Vec<Finding> {
    vec![
        row("SEC-1", "src/a.rs", 10, "let a = secret_a();"),
        row("SEC-1", "src/b.rs", 20, "let b = secret_b();"),
        row("SEC-1", "src/c.rs", 30, "let c = secret_c();"),
    ]
}

// ── node decisions ────────────────────────────────────────────────────────────────────────

#[test]
fn node_decision_writes_one_record_per_member_with_the_reason() {
    let (rows, tree) = tree_of(three_rows());
    let node = rule_node(&tree, "SEC-1");
    let members = tree.batch_members(&node);
    assert_eq!(members.len(), 3);

    let store = DecisionStore::new();
    let (record, written) = store
        .record_node(
            "p",
            &node,
            &members,
            &node_req(&node, DecisionState::Ignored, "all three are test fixtures"),
            ctx("insp-1"),
        )
        .unwrap();

    assert_eq!(written, 3, "one per-site record per member");
    assert_eq!(record.scope, Scope::Node(node.clone()));
    assert_eq!(record.applies_to, members);

    let all = store.list("p");
    assert_eq!(all.len(), 4, "the node record and one record per member");
    let inherited: Vec<&Decision> = all.iter().filter(|d| d.inherited_from.is_some()).collect();
    assert_eq!(inherited.len(), 3);
    let keys: Vec<&str> = inherited.iter().map(|d| d.key.as_str()).collect();
    for m in &members {
        assert!(keys.contains(&m.as_str()), "{m:?} has its own record");
    }
    for d in &inherited {
        assert_eq!(d.inherited_from.as_deref(), Some(record.id.as_str()));
        assert_eq!(d.reason, "all three are test fixtures");
        assert_eq!(d.state, DecisionState::Ignored);
        assert_eq!(d.scope, Scope::Site);
        assert_eq!(d.run_id, "insp-1");
        assert_eq!(d.decided_by, "zach");
    }
    // Every site carries the decision and its reason once applied.
    let applied = apply_to(&tree, &store.active("p"), &[]);
    for f in &rows {
        let d = applied
            .get(&f.site_key)
            .and_then(|s| s.decision.as_ref())
            .unwrap_or_else(|| panic!("{} is decided", f.path));
        assert_eq!(d.reason, "all three are test fixtures");
        assert_eq!(d.inherited_from.as_deref(), Some(record.id.as_str()));
    }
}

#[test]
fn a_node_decision_without_a_reason_is_refused() {
    let (_rows, tree) = tree_of(three_rows());
    let node = rule_node(&tree, "SEC-1");
    let members = tree.batch_members(&node);
    let store = DecisionStore::new();

    // Confirm included (D9): a group decision always carries a reason.
    for state in [
        DecisionState::Confirmed,
        DecisionState::Ignored,
        DecisionState::FalsePositive,
        DecisionState::TechDebtLater,
    ] {
        for reason in ["", "   \n"] {
            let err = store
                .record_node(
                    "p",
                    &node,
                    &members,
                    &node_req(&node, state.clone(), reason),
                    ctx("insp-1"),
                )
                .unwrap_err();
            assert!(
                matches!(err, DecisionStoreError::ReasonRequired(_)),
                "{state:?} with {reason:?}: {err}"
            );
        }
    }
    assert!(
        store.list("p").is_empty(),
        "a refused decision writes nothing"
    );

    // A site-scope Confirm needs none; any other site-scope decision does.
    store
        .record_site(
            "p",
            &site_req("k1", DecisionState::Confirmed, ""),
            ctx("insp-1"),
        )
        .unwrap();
    let err = store
        .record_site(
            "p",
            &site_req("k2", DecisionState::Ignored, ""),
            ctx("insp-1"),
        )
        .unwrap_err();
    assert!(matches!(err, DecisionStoreError::ReasonRequired(_)));
    assert_eq!(store.list("p").len(), 1);
}

#[test]
fn a_merge_needs_a_target_and_a_reason_and_is_never_a_group_decision() {
    let (_rows, tree) = tree_of(three_rows());
    let node = rule_node(&tree, "SEC-1");
    let store = DecisionStore::new();
    let merge = |key: &str, target: &str, reason: &str| {
        store.record_site(
            "p",
            &site_req(key, DecisionState::MergedInto(target.to_string()), reason),
            ctx("insp-1"),
        )
    };
    assert!(matches!(
        merge("k1", "", "same cause").unwrap_err(),
        DecisionStoreError::MergeTargetInvalid
    ));
    assert!(matches!(
        merge("k1", "k1", "same cause").unwrap_err(),
        DecisionStoreError::MergeTargetInvalid
    ));
    assert!(matches!(
        merge("k1", "k2", " ").unwrap_err(),
        DecisionStoreError::ReasonRequired(_)
    ));
    assert!(matches!(
        store
            .record_node(
                "p",
                &node,
                &tree.batch_members(&node),
                &node_req(&node, DecisionState::MergedInto("k9".into()), "why"),
                ctx("insp-1"),
            )
            .unwrap_err(),
        DecisionStoreError::NodeMerge
    ));
    assert!(matches!(
        store
            .record_site(
                "p",
                &site_req("  ", DecisionState::Confirmed, ""),
                ctx("insp-1")
            )
            .unwrap_err(),
        DecisionStoreError::KeyRequired
    ));
    assert!(store.list("p").is_empty());
    merge("k1", "k2", "same cause").unwrap();
    assert_eq!(store.list("p").len(), 1);
}

// ── reruns ────────────────────────────────────────────────────────────────────────────────

#[test]
fn rerun_member_not_in_applies_to_is_unresolved_and_flagged_new_since_decision() {
    let (rows1, tree1) = tree_of(three_rows());
    let node = rule_node(&tree1, "SEC-1");
    let store = DecisionStore::new();
    let (record, _) = store
        .record_node(
            "p",
            &node,
            &tree1.batch_members(&node),
            &node_req(&node, DecisionState::Confirmed, "reviewed the group"),
            ctx("insp-1"),
        )
        .unwrap();

    // The rerun finds the same three sites and one more.
    let mut again = three_rows();
    again.push(row("SEC-1", "src/d.rs", 40, "let d = secret_d();"));
    let (rows2, tree2) = tree_of(again);
    let applied = apply_to(&tree2, &store.active("p"), &[]);

    for f in &rows1 {
        let s = applied.get(&f.site_key).expect("an old member is decided");
        assert!(s.decision.is_some());
        assert!(s.new_since_decision.is_none());
    }
    let new_key = key_at(&rows2, "src/d.rs");
    let s = applied
        .get(&new_key)
        .expect("the new member has a standing");
    assert!(s.is_unresolved(), "the new member inherits nothing");
    let marker = s
        .new_since_decision
        .as_ref()
        .expect("and is flagged new since the decision");
    assert_eq!(marker.decision_id, record.id);
    assert_eq!(marker.node_id, node);
    assert_eq!(marker.decided_at, NOW);
}

#[test]
fn decision_applies_only_to_an_exact_key_match() {
    let (rows, tree) = tree_of(three_rows());
    let a = key_at(&rows, "src/a.rs");
    let b = key_at(&rows, "src/b.rs");
    let store = DecisionStore::new();
    store
        .record_site(
            "p",
            &site_req(&a, DecisionState::Ignored, "accepted"),
            ctx("insp-1"),
        )
        .unwrap();

    // A sibling in the same node and the same rule is not touched.
    let applied = apply_to(&tree, &store.active("p"), &[]);
    assert!(applied.get(&a).and_then(|s| s.decision.as_ref()).is_some());
    assert!(
        applied.get(&b).is_none(),
        "a decision on one site does not reach its neighbours"
    );

    // A rerun where the site's snippet changed has a different key: no exact match, so nothing
    // is applied to it.
    let (_rows2, tree2) = tree_of(vec![
        row("SEC-1", "src/a.rs", 10, "let a = secret_a_changed();"),
        row("SEC-1", "src/b.rs", 20, "let b = secret_b();"),
    ]);
    let applied2 = apply_to(&tree2, &store.active("p"), &[]);
    assert!(
        applied2.sites.iter().all(|s| s.decision.is_none()),
        "no key matches exactly, so no decision applies"
    );

    // An inherited record whose key is not in its node decision's applies_to is not applied.
    let node = rule_node(&tree, "SEC-1");
    let blank = |id: &str, key: &str| Decision {
        id: id.into(),
        key: key.into(),
        state: DecisionState::Confirmed,
        reason: "r".into(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        inherited_from: None,
        decided_by: "z".into(),
        decided_at: NOW.into(),
        run_id: "insp-1".into(),
        engine_version: "v".into(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    };
    let forged = vec![
        Decision {
            scope: Scope::Node(node.clone()),
            applies_to: vec![a.clone()],
            ..blank("dec-000001", &node)
        },
        Decision {
            inherited_from: Some("dec-000001".into()),
            ..blank("dec-000001.00000", &b)
        },
    ];
    let applied3 = apply_to(&tree, &forged, &[]);
    assert!(
        applied3.get(&b).and_then(|s| s.decision.as_ref()).is_none(),
        "{b:?} is not in applies_to"
    );
}

#[test]
fn moved_site_shows_the_prior_decision_and_stays_unresolved() {
    let prev = entry_of(
        "p",
        vec![row("SEC-2", "src/b.rs", 40, "run(user_input)")],
        "2026-11-02T10:00:00Z",
    );
    let next = entry_of(
        "p",
        vec![row(
            "SEC-2",
            "src/b.rs",
            41,
            "run(sanitize_badly(user_input))",
        )],
        "2026-11-03T10:00:00Z",
    );
    let old_key = prev.report.findings[0].site_key.clone();
    let new_key = next.report.findings[0].site_key.clone();
    assert_ne!(old_key, new_key);

    let store = DecisionStore::new();
    let ruled = store
        .record_site(
            "p",
            &site_req(&old_key, DecisionState::Ignored, "behind the gateway"),
            ctx("insp-1"),
        )
        .unwrap();

    // The links come the way a run writes them: match_runs, key_links.json, read back.
    let dir = tempfile::tempdir().unwrap();
    let path = match_runs(&prev.report.findings, &next.report.findings)
        .write_key_links(dir.path())
        .unwrap();
    let links = read_key_links(&path).unwrap();
    assert_eq!(links.len(), 1);

    let tree = next.report.tree.as_ref().unwrap();
    let applied = apply_to(tree, &store.active("p"), &links);
    let s = applied
        .get(&new_key)
        .expect("the moved site has a standing");
    assert!(s.decision.is_none(), "the prior decision is not applied");
    assert!(s.is_unresolved());
    let stale = s.stale.as_ref().expect("but it is shown, stale");
    assert_eq!(stale.decision_id, ruled.id);
    assert_eq!(stale.from_key, old_key);
    assert_eq!(stale.state, DecisionState::Ignored);
    assert_eq!(stale.reason, "behind the gateway");

    // Without the link the old decision is simply kept for history: nothing is shown.
    let unlinked = apply_to(tree, &store.active("p"), &[]);
    assert!(unlinked.get(&new_key).is_none());
    assert_eq!(store.list("p").len(), 1, "the old record is kept");
}

// ── revoke ────────────────────────────────────────────────────────────────────────────────

#[test]
fn revoked_node_decision_reverts_every_inherited_record() {
    let (rows, tree) = tree_of(three_rows());
    let node = rule_node(&tree, "SEC-1");
    let store = DecisionStore::new();
    let (record, _) = store
        .record_node(
            "p",
            &node,
            &tree.batch_members(&node),
            &node_req(&node, DecisionState::FalsePositive, "generated code"),
            ctx("insp-1"),
        )
        .unwrap();
    assert!(store.has_active_records("p"));

    let revoked = store
        .revoke("p", &record.id, "2026-11-05T08:00:00Z")
        .unwrap();
    assert_eq!(revoked.len(), 4, "the node record and its three records");

    let all = store.list("p");
    assert_eq!(all.len(), 4, "nothing is deleted");
    assert!(
        all.iter()
            .all(|d| d.revoked_at.as_deref() == Some("2026-11-05T08:00:00Z")),
        "every record carries revoked_at"
    );
    assert!(!store.has_active_records("p"));
    let applied = apply_to(&tree, &store.active("p"), &[]);
    for f in &rows {
        assert!(
            applied.get(&f.site_key).is_none(),
            "{} reverted to Unresolved",
            f.path
        );
    }
    // A second revoke is refused and changes nothing.
    assert!(matches!(
        store.revoke("p", &record.id, "later").unwrap_err(),
        DecisionStoreError::AlreadyRevoked(_)
    ));
}

#[test]
fn a_record_a_group_decision_wrote_cannot_be_revoked_alone_and_a_site_ruling_overrides_it() {
    let (rows, tree) = tree_of(three_rows());
    let node = rule_node(&tree, "SEC-1");
    let a = key_at(&rows, "src/a.rs");
    let store = DecisionStore::new();
    store
        .record_node(
            "p",
            &node,
            &tree.batch_members(&node),
            &node_req(&node, DecisionState::Confirmed, "pattern reviewed"),
            ctx("insp-1"),
        )
        .unwrap();
    let inherited_id = store
        .list("p")
        .into_iter()
        .find(|d| d.key == a && d.inherited_from.is_some())
        .unwrap()
        .id;
    assert!(matches!(
        store.revoke("p", &inherited_id, NOW).unwrap_err(),
        DecisionStoreError::InheritedRecord(_)
    ));

    // The reviewer's own ruling on one site wins over the batch; revoking it brings the batch back.
    let own = store
        .record_site(
            "p",
            &site_req(&a, DecisionState::Ignored, "this one is dead code"),
            ctx("insp-1"),
        )
        .unwrap();
    let applied = apply_to(&tree, &store.active("p"), &[]);
    let d = applied.get(&a).unwrap().decision.as_ref().unwrap();
    assert_eq!(d.state, DecisionState::Ignored);
    assert_eq!(d.inherited_from, None);
    store.revoke("p", &own.id, NOW).unwrap();
    let applied = apply_to(&tree, &store.active("p"), &[]);
    let d = applied.get(&a).unwrap().decision.as_ref().unwrap();
    assert_eq!(d.state, DecisionState::Confirmed);
    assert!(d.inherited_from.is_some());
}

// ── persistence ───────────────────────────────────────────────────────────────────────────

#[test]
fn decisions_survive_a_restart_and_a_failed_write_leaves_the_mirror_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = DecisionStore::at(dir.path().join("decisions"));
    store
        .record_site(
            "proj/1",
            &site_req("k1", DecisionState::Ignored, "accepted"),
            ctx("insp-1"),
        )
        .unwrap();
    let file = store.file_for("proj/1").unwrap();
    assert!(file.exists());
    assert!(!file.with_extension("json.tmp").exists());

    let reopened = DecisionStore::at(dir.path().join("decisions"));
    assert_eq!(reopened.list("proj/1").len(), 1);
    assert_eq!(reopened.list("proj/1")[0].key, "k1");

    // A store whose directory cannot be created refuses the write and keeps its old state.
    let blocked = dir.path().join("a_file");
    std::fs::write(&blocked, "x").unwrap();
    let broken = DecisionStore::at(blocked.join("decisions"));
    let err = broken
        .record_site(
            "p",
            &site_req("k", DecisionState::Ignored, "r"),
            ctx("insp-1"),
        )
        .unwrap_err();
    assert!(matches!(err, DecisionStoreError::Persist(_)));
    assert!(broken.list("p").is_empty());

    // A file that does not parse is moved aside, not overwritten.
    std::fs::write(&file, "not json").unwrap();
    let after = DecisionStore::at(dir.path().join("decisions"));
    assert!(after.list("proj/1").is_empty());
    assert!(file.with_extension("json.corrupt").exists());
}

// ── the release gate ──────────────────────────────────────────────────────────────────────

#[test]
fn the_store_answers_the_release_gates_question() {
    let (rows, tree) = tree_of(three_rows());
    let a = &rows[0];
    let b = rows.iter().find(|f| f.path == "src/b.rs").unwrap();
    let c = rows.iter().find(|f| f.path == "src/c.rs").unwrap();
    let store = DecisionStore::new();
    store
        .record_site(
            "p",
            &site_req(&a.site_key, DecisionState::Confirmed, ""),
            ctx("insp-1"),
        )
        .unwrap();
    let node = rule_node(&tree, "SEC-1");
    let (record, _) = store
        .record_node(
            "p",
            &node,
            std::slice::from_ref(&b.site_key),
            &node_req(&node, DecisionState::Ignored, "r"),
            ctx("insp-1"),
        )
        .unwrap();

    let decisions = StoreDecisions::for_project(&store, "p");
    assert!(decisions.has_decision(a), "a site decision");
    assert!(decisions.has_decision(b), "an inherited decision");
    assert!(!decisions.has_decision(c), "an undecided row");

    // The gate's own check, fed by the store: only the undecided unverified row is refused.
    let mark = |f: &Finding| {
        let mut f = f.clone();
        f.captures.insert(
            crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
            "not found near the cited line".to_string(),
        );
        f
    };
    let problems = unverified_without_decision(&[mark(a), mark(b), mark(c)], &decisions);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].location.contains("src/c.rs"));

    // A revoked decision no longer counts.
    store.revoke("p", &record.id, NOW).unwrap();
    let after = StoreDecisions::for_project(&store, "p");
    assert!(!after.has_decision(b));
    assert!(after.has_decision(a));
}

// ── the export request's dispositions ─────────────────────────────────────────────────────

fn wire(state: &str, reason: &str, bucket: &str) -> crate::report_export::DispositionWire {
    crate::report_export::DispositionWire {
        state: state.to_string(),
        reason: reason.to_string(),
        bucket: bucket.to_string(),
        confirmed_by_client: false,
    }
}

#[test]
fn the_export_ignores_request_dispositions_once_the_store_has_records_and_reports_disagreement() {
    let store = DecisionStore::new();
    let mut from_request = HashMap::new();
    from_request.insert("k1".to_string(), wire("Ignored", "from the draft", "Later"));

    // An empty store: the request's dispositions are used as they are.
    let e = effective_dispositions(&store, "p", &from_request);
    assert!(!e.from_store);
    assert_eq!(e.map, from_request);
    assert!(e.disagreements.is_empty());

    store
        .record_site(
            "p",
            &site_req("k1", DecisionState::TechDebtNow, "pull it in"),
            ctx("insp-1"),
        )
        .unwrap();
    store
        .record_site(
            "p",
            &site_req("k2", DecisionState::FalsePositive, "generated"),
            ctx("insp-1"),
        )
        .unwrap();

    let e = effective_dispositions(&store, "p", &from_request);
    assert!(
        e.from_store,
        "the store has records, so the request is ignored"
    );
    assert_eq!(e.map.len(), 2);
    assert_eq!(e.map["k1"], wire("TechDebt", "pull it in", "Now"));
    assert_eq!(e.map["k2"].state, "FalsePositive");
    // k1 differs (Ignored in the request, tech debt in the store); k2 is only in the store.
    let keys: Vec<&str> = e.disagreements.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(keys, vec!["k1", "k2"]);
    assert_eq!(e.disagreements[0].request.as_deref(), Some("Ignored"));
    assert_eq!(e.disagreements[0].store.as_deref(), Some("TechDebt"));
    assert_eq!(e.disagreements[1].request, None);

    // Agreement is not logged.
    let mut agree = HashMap::new();
    agree.insert("k1".to_string(), wire("TechDebt", "pull it in", "Now"));
    agree.insert(
        "k2".to_string(),
        wire("FalsePositive", "generated", "Later"),
    );
    assert!(effective_dispositions(&store, "p", &agree)
        .disagreements
        .is_empty());
}

// ── the routes ────────────────────────────────────────────────────────────────────────────

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
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn to_json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap()
}

#[tokio::test]
async fn the_routes_real_json_deserialises_into_the_api_types_the_ui_uses() {
    let (state, app, pid) = app_with_project();
    let entry = state
        .inspections
        .append(entry_of(&pid, three_rows(), "2026-11-02T10:00:00Z"))
        .unwrap();
    let tree = entry.report.tree.clone().unwrap();
    let node = rule_node(&tree, "SEC-1");
    let a = key_at(&entry.report.findings, "src/a.rs");
    let uri = format!("/api/projects/{pid}/decisions");

    // A site decision, the request built from the shared type.
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(site_req(
            &a,
            DecisionState::Ignored,
            "dead code",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let recorded: DecisionRecorded = serde_json::from_value(body).expect("DecisionRecorded");
    assert_eq!(recorded.inherited_count, 0);
    assert_eq!(recorded.decision.key, a);
    assert_eq!(recorded.decision.run_id, entry.id);

    // A node decision.
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(node_req(
            &node,
            DecisionState::Confirmed,
            "the pattern is real",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let node_done: DecisionRecorded = serde_json::from_value(body).expect("DecisionRecorded");
    assert_eq!(node_done.inherited_count, 3);

    // GET: records and standings.
    let (status, body) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let got: DecisionsResp = serde_json::from_value(body).expect("DecisionsResp");
    assert_eq!(got.decisions.len(), 1 + 1 + 3);
    assert!(!got.migrated_from_draft);
    assert_eq!(got.standings.len(), 3);
    // The node decision is the newer record for `a`, so it is the one that applies.
    let on_a = got.standings.iter().find(|s| s.key == a).unwrap();
    assert_eq!(
        on_a.decision.as_ref().unwrap().inherited_from.as_deref(),
        Some(node_done.decision.id.as_str())
    );

    // Revoke the node decision.
    let (status, body) = call(
        &app,
        "POST",
        &format!("{uri}/{}/revoke", node_done.decision.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let revoked: RevokeResp = serde_json::from_value(body).expect("RevokeResp");
    assert_eq!(revoked.revoked.len(), 4);
    assert!(!revoked.revoked_at.is_empty());

    // Refusals share one body shape.
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(node_req(
            &node,
            DecisionState::Confirmed,
            "",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let err: DecisionError = serde_json::from_value(body).expect("DecisionError");
    assert!(!err.ok);
    assert_eq!(err.code, "reason_required");
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(node_req(
            "rule:no-such-node",
            DecisionState::Confirmed,
            "r",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        serde_json::from_value::<DecisionError>(body).unwrap().code,
        "unknown_node"
    );
    let (status, _) = call(&app, "POST", &format!("{uri}/dec-999999/revoke"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "GET", "/api/projects/nope/decisions", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_decision_needs_an_inspection_to_be_made_against() {
    let (_state, app, pid) = app_with_project();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/projects/{pid}/decisions"),
        Some(to_json(&PostDecisionsReq::Decide(site_req(
            "k",
            DecisionState::Confirmed,
            "",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_value::<DecisionError>(body).unwrap().code,
        "no_inspection"
    );
    let (status, body) = call(&app, "GET", &format!("/api/projects/{pid}/decisions"), None).await;
    assert_eq!(status, StatusCode::OK);
    let empty: DecisionsResp = serde_json::from_value(body).unwrap();
    assert!(empty.decisions.is_empty() && empty.standings.is_empty());
}

#[tokio::test]
async fn the_standings_route_shows_a_moved_site_stale_against_the_run_it_was_decided_on() {
    let (state, app, pid) = app_with_project();
    let first = state
        .inspections
        .append(entry_of(
            &pid,
            vec![row("SEC-2", "src/b.rs", 40, "run(user_input)")],
            "2026-11-02T10:00:00Z",
        ))
        .unwrap();
    let old_key = first.report.findings[0].site_key.clone();
    let uri = format!("/api/projects/{pid}/decisions");
    let (status, _) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Decide(site_req(
            &old_key,
            DecisionState::Ignored,
            "behind the gateway",
        )))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The rerun: the same site, edited a line away, and a second inspection after it.
    let second = state
        .inspections
        .append(entry_of(
            &pid,
            vec![row(
                "SEC-2",
                "src/b.rs",
                41,
                "run(sanitize_badly(user_input))",
            )],
            "2026-11-03T10:00:00Z",
        ))
        .unwrap();
    let new_key = second.report.findings[0].site_key.clone();
    let third = state
        .inspections
        .append(entry_of(
            &pid,
            vec![row(
                "SEC-2",
                "src/b.rs",
                44,
                "run(sanitize_badly(user_input))",
            )],
            "2026-11-04T10:00:00Z",
        ))
        .unwrap();
    // The third run's site has the second run's key; the decision was made on the first run.
    assert_eq!(third.report.findings[0].site_key, new_key);

    let (_, body) = call(&app, "GET", &uri, None).await;
    let got: DecisionsResp = serde_json::from_value(body).unwrap();
    let s = got.standings.iter().find(|s| s.key == new_key).unwrap();
    assert!(s.decision.is_none());
    assert_eq!(s.stale.as_ref().unwrap().from_key, old_key);
    assert_eq!(s.stale.as_ref().unwrap().reason, "behind the gateway");
}

#[tokio::test]
async fn draft_dispositions_migrate_once_and_the_draft_is_blanked() {
    let (state, app, pid) = app_with_project();
    let rows = vec![
        row("SEC-1", "src/a.rs", 10, "let a = secret_a();"),
        row("SEC-1", "src/b.rs", 20, "let b = secret_b();"),
        row("SEC-3", "src/c.rs", 30, "eval(x)"),
    ];
    let entry = state
        .inspections
        .append(entry_of(&pid, rows.clone(), "2026-11-02T10:00:00Z"))
        .unwrap();
    let key_of = |path: &str| key_at(&entry.report.findings, path);

    // The draft as the UI saved it before the site key: dispositions under the line-based key.
    let old = |f: &Finding| legacy_finding_key(f);
    let dispositions = serde_json::json!({
        old(&rows[0]): { "state": "Ignored", "reason": "test fixture", "bucket": "Later" },
        old(&rows[1]): { "state": "TechDebt", "reason": "", "bucket": "Now" },
        "acme/app\u{0}GONE-1\u{0}src/gone.rs\u{0}9\u{0}old()": {
            "state": "FalsePositive", "reason": "was generated", "bucket": "Later"
        },
        old(&rows[2]): { "state": "Unresolved", "reason": "", "bucket": "Later" },
    });
    state.draft.save(
        &pid,
        serde_json::json!({
            "scan": { "repos": ["acme/app"] },
            "viewed_repo": "acme/app",
            "dispositions": dispositions,
        }),
    );

    let uri = format!("/api/projects/{pid}/decisions");
    let request: MigrateDraftReq = serde_json::from_value(
        serde_json::json!({"migrated_from_draft": true, "dispositions": dispositions}),
    )
    .unwrap();
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Migrate(request.clone()))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome: MigrationOutcome = serde_json::from_value(body).unwrap();
    assert!(!outcome.already_migrated);
    assert_eq!(outcome.migrated, 3, "two matched rows and one gone row");
    assert_eq!(outcome.unmatched.len(), 1);
    assert_eq!(outcome.skipped, 1, "the Unresolved entry is not a decision");
    assert_eq!(outcome.dispositions.len(), 2, "by current site key");
    assert_eq!(outcome.dispositions[&key_of("src/a.rs")].state, "Ignored");

    // The records, keyed by the current site keys.
    let stored = state.decisions.list(&pid);
    assert_eq!(stored.len(), 3);
    let a = stored.iter().find(|d| d.key == key_of("src/a.rs")).unwrap();
    assert_eq!(a.state, DecisionState::Ignored);
    assert_eq!(a.reason, "test fixture");
    assert_eq!(a.scope, Scope::Site);
    assert_eq!(a.inherited_from, None);
    assert_eq!(a.decided_by, MIGRATED_BY);
    assert_eq!(a.run_id, entry.id);
    let b = stored.iter().find(|d| d.key == key_of("src/b.rs")).unwrap();
    assert_eq!(b.state, DecisionState::TechDebtNow);
    let gone = stored
        .iter()
        .find(|d| d.key.contains("GONE-1"))
        .expect("a gone row's disposition is kept under its old key");
    assert_eq!(gone.state, DecisionState::FalsePositive);
    assert!(state.decisions.migrated_from_draft_at(&pid).is_some());

    // The draft is blanked: the dispositions are gone, the rest of it is untouched.
    let draft = state.draft.load(&pid).unwrap();
    assert_eq!(draft["dispositions"], serde_json::json!({}));
    assert_eq!(draft["viewed_repo"], "acme/app");
    assert_eq!(draft["scan"]["repos"][0], "acme/app");

    // Once: a second migration writes nothing, even with the draft re-filled.
    state.draft.save(
        &pid,
        serde_json::json!({ "dispositions": dispositions, "viewed_repo": "acme/app" }),
    );
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(to_json(&PostDecisionsReq::Migrate(request))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let again: MigrationOutcome = serde_json::from_value(body).unwrap();
    assert!(again.already_migrated);
    assert_eq!(again.migrated, 0);
    assert_eq!(state.decisions.list(&pid).len(), 3, "nothing was written");
    assert_eq!(
        state.draft.load(&pid).unwrap()["dispositions"],
        serde_json::json!({}),
        "and the draft is blank again"
    );

    // A draft key that is already a site key (saved after the site key existed) migrates too.
    let (state2, app2, pid2) = app_with_project();
    let entry2 = state2
        .inspections
        .append(entry_of(&pid2, rows.clone(), "2026-11-02T10:00:00Z"))
        .unwrap();
    let site_keyed = serde_json::json!({
        key_at(&entry2.report.findings, "src/a.rs"): {
            "state": "Confirmed", "reason": "", "bucket": "Later"
        }
    });
    let (_, body) = call(
        &app2,
        "POST",
        &format!("/api/projects/{pid2}/decisions"),
        Some(serde_json::json!({"migrated_from_draft": true, "dispositions": site_keyed})),
    )
    .await;
    let outcome: MigrationOutcome = serde_json::from_value(body).unwrap();
    assert_eq!(outcome.migrated, 1);
    assert!(outcome.unmatched.is_empty());
    assert_eq!(
        state2.decisions.list(&pid2)[0].key,
        key_at(&entry2.report.findings, "src/a.rs")
    );
}
