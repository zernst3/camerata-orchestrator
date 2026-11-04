#![allow(clippy::unwrap_used)]
//! WO9c: the review read model's contracts. Each route's real output deserializes into the UI's
//! type (api-types, every struct denying unknown fields); a checked-in snapshot pins the wire
//! shape; the tree never carries a site; a node's sites page 500 at a time under a cursor that
//! survives writes; previewing the escalation line writes nothing and matches the real re-mark.

#[path = "common/review.rs"]
mod review;

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{EditField, NewEditReq};
use camerata_api_types::review::{re_mark, EscalationBudget, EscalationReason, OutlierReason};
use camerata_api_types::review_view::{
    EngineCheckError, EngineCheckLane, EngineCheckVerdict, EscalationBudgetOutcome,
    EscalationBudgetReq, ExportGateView, HoldReason, Marker, QueueView, ReviewSettingsView,
    ReviewShellView, ReviewViewError, SampleSizeReq, SampleSizeResp, SitesPage, TreeView,
};
use review::{call, path_key, review_fixture, ReviewFixture};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// A time after any verdict a test gives.
const LATER: &str = "2999-01-01T00:00:00Z";

fn base(f: &ReviewFixture) -> String {
    format!("/api/projects/{}", f.project_id)
}

/// Parse `body` as a `T` and check it is the same JSON back: nothing was dropped on the way in
/// (the types deny unknown fields) and nothing is lost on the way out.
fn round_trip<T: DeserializeOwned + serde::Serialize>(what: &str, body: &str) -> T {
    let typed: T =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{what} does not parse: {e}\n{body}"));
    let sent: Value = serde_json::from_str(body).unwrap();
    // Compare as text-parsed values both ways, so a float prints the same on both sides.
    let back: Value = serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
    assert_eq!(sent, back, "{what} is not the same JSON after a round trip");
    typed
}

async fn get<T: DeserializeOwned + serde::Serialize>(
    f: &ReviewFixture,
    what: &str,
    uri: &str,
) -> T {
    let (status, body) = call(&f.app, "GET", &format!("{}{uri}", base(f)), None).await;
    assert_eq!(status, 200, "{what}: {body}");
    round_trip(what, &body)
}

async fn post_ok<T: DeserializeOwned + serde::Serialize>(
    f: &ReviewFixture,
    what: &str,
    uri: &str,
    body: Value,
) -> T {
    let (status, text) = call(&f.app, "POST", &format!("{}{uri}", base(f)), Some(body)).await;
    assert_eq!(status, 200, "{what}: {text}");
    round_trip(what, &text)
}

async fn refused(
    f: &ReviewFixture,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, ReviewViewError) {
    let (status, text) = call(&f.app, method, &format!("{}{uri}", base(f)), body).await;
    assert!(!status.is_success(), "{uri} should be refused: {text}");
    (status.as_u16(), round_trip("a refusal", &text))
}

fn decision(scope: Scope, key: &str, state: DecisionState, reason: &str) -> Value {
    decision_at(scope, key, state, reason, "")
}

/// A decision the reviewer made at `at` (RFC3339; blank means now).
fn decision_at(scope: Scope, key: &str, state: DecisionState, reason: &str, at: &str) -> Value {
    serde_json::to_value(NewDecisionReq {
        scope,
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        decided_by: "zach".to_string(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: String::new(),
        decided_at: at.to_string(),
    })
    .unwrap()
}

/// Give the fixture a story for every view: a decision on a site and on a node, a restored row,
/// an edit, the line moved so a candidate sits below it, and two Engine Check verdicts.
async fn decorate(f: &ReviewFixture) {
    let b = base(f);
    let ok = |what: &'static str, status: axum::http::StatusCode, text: String| {
        assert_eq!(status, 200, "{what}: {text}");
    };
    let (s, t) = call(
        &f.app,
        "POST",
        &format!("{b}/decisions"),
        Some(decision(
            Scope::Site,
            &f.keys.plain,
            DecisionState::Confirmed,
            "",
        )),
    )
    .await;
    ok("confirm the plain row", s, t);
    let (s, t) = call(
        &f.app,
        "POST",
        &format!("{b}/decisions"),
        Some(decision(
            Scope::Site,
            &f.keys.restored,
            DecisionState::EngineRestored,
            "the engine was wrong to throw it out",
        )),
    )
    .await;
    ok("restore a row", s, t);
    let (s, t) = call(
        &f.app,
        "POST",
        &format!("{b}/decisions"),
        Some(decision(
            Scope::Node("sub:SEC-AUTHZ-4/0".to_string()),
            "",
            DecisionState::Confirmed,
            "the pattern is real",
        )),
    )
    .await;
    ok("decide the auth sub-group", s, t);
    let edit = NewEditReq {
        key: f.keys.below_line.clone(),
        field: EditField::Headline,
        new: "Raw SQL built from a request field".to_string(),
        reason: "the headline names the field".to_string(),
        by: "zach".to_string(),
        run_id: String::new(),
    };
    let (s, t) = call(
        &f.app,
        "POST",
        &format!("{b}/edits"),
        Some(serde_json::to_value(edit).unwrap()),
    )
    .await;
    ok("edit a headline", s, t);
    // The line: rate 0 and a floor of 1 puts the second candidate below it.
    let _: EscalationBudgetOutcome = post_ok(
        f,
        "move the line",
        "/escalation-budget",
        json!({ "rate_per_kloc": 0.0, "floor": 1, "over_budget_reasons": [] }),
    )
    .await;
    // Two Engine Check verdicts: a Yes and an uncorrected No.
    let lane: EngineCheckLane = get(f, "the lane", "/review/engine-check").await;
    assert!(lane.cards.len() >= 2, "{} cards", lane.cards.len());
    let _: EngineCheckVerdict = post_ok(
        f,
        "a yes",
        &format!(
            "/review/engine-check/{}",
            path_key(&lane.cards[0].site.site_key)
        ),
        json!({ "right": true }),
    )
    .await;
    let _: EngineCheckVerdict = post_ok(
        f,
        "a no",
        &format!(
            "/review/engine-check/{}",
            path_key(&lane.cards[1].site.site_key)
        ),
        json!({ "right": false, "wrong": "wrong_severity" }),
    )
    .await;
}

// ── every route round-trips into the UI's type ────────────────────────────────────────────

#[tokio::test]
async fn every_review_route_round_trips_into_the_ui_type() {
    let f = review_fixture(12);
    decorate(&f).await;

    let shell: ReviewShellView = get(&f, "the shell", "/review").await;
    assert_eq!(shell.run_id, f.run_id);
    let tree: TreeView = get(&f, "the tree", "/review/tree").await;
    assert!(!tree.nodes.is_empty());
    let sites: SitesPage = get(
        &f,
        "a node's sites",
        &format!("/review/sites?node={}", f.sql_sub),
    )
    .await;
    assert_eq!(sites.total, 13, "twelve SQL rows and the restored one");
    let queue: QueueView = get(&f, "the queue", "/review/queue?show_below=true").await;
    assert_eq!(queue.escalated.len(), 1);
    assert_eq!(queue.below.len(), 1);
    let lane: EngineCheckLane = get(&f, "the Engine Check lane", "/review/engine-check").await;
    assert!(lane.cards.iter().any(|c| c.verdict.is_some()));
    let gate: ExportGateView = get(&f, "the export gate", "/review/export-gate").await;
    assert!(gate.is_blocked());
    let reasons: std::collections::BTreeSet<String> = gate
        .held
        .iter()
        .chain(&gate.merged_without_decision)
        .map(|h| format!("{:?}", h.reason))
        .collect();
    let want: std::collections::BTreeSet<String> = [
        HoldReason::EvidenceUnverified,
        HoldReason::RestoredWithoutDecision,
        HoldReason::MergedWithoutDecision,
        HoldReason::EngineCheckNoWithoutCorrection,
    ]
    .iter()
    .map(|r| format!("{r:?}"))
    .collect();
    assert_eq!(reasons, want, "a row of each kind the fixture holds");
    let settings: ReviewSettingsView = get(&f, "the settings", "/review/settings").await;
    assert_eq!(settings.escalation.floor, 1);

    let req = EscalationBudgetReq {
        rate_per_kloc: 2.0,
        floor: 5,
        over_budget_reasons: EscalationReason::default_over_budget_set(),
    };
    let _: EscalationBudgetOutcome = post_ok(
        &f,
        "the budget preview",
        "/escalation-budget/preview",
        serde_json::to_value(&req).unwrap(),
    )
    .await;
    let size: SampleSizeResp = post_ok(
        &f,
        "the sample size",
        "/sample-size",
        serde_json::to_value(SampleSizeReq {
            sample_size_per_run: 7,
        })
        .unwrap(),
    )
    .await;
    assert_eq!(size.sample_size_per_run, 7);

    // A refusal is a typed body too.
    let (status, err) = refused(&f, "GET", "/review/sites?node=cat:nothing", None).await;
    assert_eq!((status, err.code.as_str()), (404, "unknown_node"));
}

#[tokio::test]
async fn a_route_refuses_a_run_that_is_not_the_latest() {
    let f = review_fixture(6);
    let (status, err) = refused(&f, "GET", "/review?run_id=insp-some-older-run", None).await;
    assert_eq!((status, err.code.as_str()), (409, "run_changed"));
    // The latest run's own id is accepted.
    let ok: ReviewShellView = get(&f, "the shell", &format!("/review?run_id={}", f.run_id)).await;
    assert_eq!(ok.run_id, f.run_id);
}

#[tokio::test]
async fn a_project_with_no_inspection_answers_no_inspection() {
    let f = review_fixture(6);
    let empty = f.state.projects().create("Empty", vec![]).unwrap();
    let (status, text) = call(
        &f.app,
        "GET",
        &format!("/api/projects/{}/review", empty.id),
        None,
    )
    .await;
    assert_eq!(status, 409, "{text}");
    let err: ReviewViewError = round_trip("a refusal", &text);
    assert_eq!(err.code, "no_inspection");
}

// ── the wire shape ────────────────────────────────────────────────────────────────────────

/// The shape of a JSON value: object keys with their value shapes, arrays as their elements'
/// merged shape, scalars as their type. Two objects merge key by key, so a field one row has and
/// another lacks is in the shape; `null` yields to anything else.
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(items) => match items.iter().map(shape).reduce(merge_shapes) {
            Some(s) => json!([s]),
            None => json!(["empty"]),
        },
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), shape(v))).collect())
        }
    }
}

/// The same value with every object's keys in sorted order, so the rendered snapshot does not
/// depend on the order fields were merged in.
fn sorted(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> =
                map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

fn is_one_of(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|m| m.len() == 1 && m.contains_key("$one_of"))
}

fn merge_shapes(a: Value, b: Value) -> Value {
    match (a, b) {
        (a, b) if a == b => a,
        (a, b) if is_one_of(&a) || is_one_of(&b) => one_of(a, b),
        (Value::String(s), other) | (other, Value::String(s)) if s == "null" => other,
        (Value::Object(mut a), Value::Object(b)) => {
            for (k, v) in b {
                let merged = match a.remove(&k) {
                    Some(existing) => merge_shapes(existing, v),
                    None => v,
                };
                a.insert(k, merged);
            }
            Value::Object(a)
        }
        (Value::Array(a), Value::Array(b)) => match (a.into_iter().next(), b.into_iter().next()) {
            (Some(x), Some(y)) => json!([merge_shapes(x, y)]),
            (Some(x), None) | (None, Some(x)) => json!([x]),
            (None, None) => json!(["empty"]),
        },
        (a, b) => one_of(a, b),
    }
}

/// Two shapes that cannot merge (a unit variant is a string and a struct variant an object): the
/// alternatives, each kind once. Objects merge with objects and arrays with arrays.
fn one_of(a: Value, b: Value) -> Value {
    let alternatives = |v: Value| match v {
        Value::Object(mut map) if map.len() == 1 && map.contains_key("$one_of") => {
            match map.remove("$one_of") {
                Some(Value::Array(items)) => items,
                other => other.into_iter().collect(),
            }
        }
        other => vec![other],
    };
    let mut merged: Vec<Value> = Vec::new();
    for item in alternatives(a).into_iter().chain(alternatives(b)) {
        let same_kind = |x: &Value| {
            matches!(
                (x, &item),
                (Value::Object(_), Value::Object(_)) | (Value::Array(_), Value::Array(_))
            )
        };
        match merged.iter().position(same_kind) {
            Some(i) => {
                let existing = merged.remove(i);
                merged.insert(i, merge_shapes(existing, item));
            }
            None if merged.contains(&item) => {}
            None => merged.push(item),
        }
    }
    merged.sort_by_key(Value::to_string);
    json!({ "$one_of": merged })
}

/// Every view's shape on a project with a story for each, plus the closed lists the screens
/// switch on.
async fn wire_shapes() -> Value {
    let f = review_fixture(12);
    decorate(&f).await;
    let fetch = |uri: String| {
        let f = &f;
        async move {
            let (status, body) = call(&f.app, "GET", &format!("{}{uri}", base(f)), None).await;
            assert_eq!(status, 200, "{uri}: {body}");
            serde_json::from_str::<Value>(&body).unwrap()
        }
    };
    let outcome = {
        let (status, body) = call(
            &f.app,
            "POST",
            &format!("{}/escalation-budget/preview", base(&f)),
            Some(json!({ "rate_per_kloc": 2.0, "floor": 5, "over_budget_reasons": ["secret_exposure"] })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<Value>(&body).unwrap()
    };
    let (_, error_body) = call(
        &f.app,
        "GET",
        &format!("{}/review/sites?node=cat:nothing", base(&f)),
        None,
    )
    .await;
    json!({
        "shell": shape(&fetch("/review".into()).await),
        "tree": shape(&fetch("/review/tree".into()).await),
        "sites": shape(&fetch(format!("/review/sites?node={}", f.sql_sub)).await),
        "queue": shape(&fetch("/review/queue?show_below=true".into()).await),
        "engine_check": shape(&fetch("/review/engine-check".into()).await),
        "export_gate": shape(&fetch("/review/export-gate".into()).await),
        "settings": shape(&fetch("/review/settings".into()).await),
        "budget_outcome": shape(&outcome),
        "error": shape(&serde_json::from_str::<Value>(&error_body).unwrap()),
        "closed_lists": {
            "markers": Marker::PRECEDENCE,
            "hold_reasons": HoldReason::ALL,
            "engine_check_errors": EngineCheckError::ALL,
            "escalation_reasons": EscalationReason::ALL.map(EscalationReason::as_str),
            "outlier_reasons": OutlierReason::ALL.map(OutlierReason::as_str),
        },
    })
}

const SNAPSHOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/review_wire_shape.json"
);

#[tokio::test]
async fn wire_shape_snapshot_fails_on_unreviewed_change() {
    let now = sorted(wire_shapes().await);
    let rendered = format!("{}\n", serde_json::to_string_pretty(&now).unwrap());
    if std::env::var("UPDATE_REVIEW_WIRE_SHAPE").is_ok_and(|v| v == "1") {
        std::fs::write(SNAPSHOT, &rendered).unwrap();
        return;
    }
    let want = std::fs::read_to_string(SNAPSHOT).unwrap_or_else(|_| {
        panic!("no snapshot at {SNAPSHOT}; run once with UPDATE_REVIEW_WIRE_SHAPE=1 and review it")
    });
    assert_eq!(
        rendered, want,
        "the review wire shape changed. If the change is on purpose, review it with the UI \
         builders, then run this test once with UPDATE_REVIEW_WIRE_SHAPE=1 to update {SNAPSHOT}"
    );
}

#[test]
fn the_shape_of_a_json_value_sees_a_field_added_renamed_or_retyped() {
    let base = json!({ "a": 1, "b": [{ "c": "x" }], "d": null });
    let added = json!({ "a": 1, "b": [{ "c": "x" }], "d": null, "e": true });
    let renamed = json!({ "a": 1, "b": [{ "cc": "x" }], "d": null });
    let retyped = json!({ "a": "1", "b": [{ "c": "x" }], "d": null });
    for (what, changed) in [("added", added), ("renamed", renamed), ("retyped", retyped)] {
        assert_ne!(
            shape(&base),
            shape(&changed),
            "a {what} field must change the shape"
        );
    }
    // A unit variant (a string) and a struct variant (an object) are two alternatives, and merging
    // more rows of either does not grow the shape.
    let rows = json!([
        "not_candidate",
        { "escalated": { "rank": 1 } },
        "not_candidate",
        { "escalated": { "rank": 2, "reason": "x" } },
        { "below_line": { "rank": 3 } },
    ]);
    let merged = shape(&rows);
    assert!(merged.to_string().len() < 200, "{merged}");
    assert_eq!(
        shape(&rows),
        shape(&json!([rows[1], rows[4], rows[0], rows[3]]))
    );
    // A value change does not.
    assert_eq!(
        shape(&base),
        shape(&json!({ "a": 2, "b": [{ "c": "y" }, { "c": "z" }], "d": null }))
    );
}

// ── U1: the tree is nodes only, sites come 500 at a time ─────────────────────────────────

#[tokio::test]
async fn tree_view_never_carries_sites() {
    let small = review_fixture(12);
    let big = review_fixture(1_200);
    let (_, small_body) = call(
        &small.app,
        "GET",
        &format!("{}/review/tree", base(&small)),
        None,
    )
    .await;
    let (_, big_body) = call(
        &big.app,
        "GET",
        &format!("{}/review/tree", base(&big)),
        None,
    )
    .await;

    // No site is in it: no path of any row, no site key, no member list.
    for needle in [
        "src/q0",
        "src/auth",
        "\\u0000",
        "\"members\"",
        "\"rows\"",
        "\"site_key\"",
    ] {
        assert!(!big_body.contains(needle), "the tree carries {needle}");
    }
    // The same nodes with a hundred times the sites: the response grows by the digits of the
    // counts and nothing else.
    let small_tree: TreeView = round_trip("tree", &small_body);
    let big_tree: TreeView = round_trip("tree", &big_body);
    assert_eq!(small_tree.nodes.len(), big_tree.nodes.len());
    assert!(
        big_body.len() < small_body.len() + 20 * big_tree.nodes.len(),
        "{} bytes for {} sites against {} bytes for 12",
        big_body.len(),
        1_203,
        small_body.len()
    );
    let sql = big_tree.nodes.iter().find(|n| n.id == big.sql_sub).unwrap();
    assert_eq!(sql.counts.sites, 1_200);
}

#[tokio::test]
async fn sites_page_pages_500_with_a_stable_cursor() {
    let f = review_fixture(1_200);
    let uri = |cursor: Option<&str>| match cursor {
        None => format!("/review/sites?node={}", f.sql_sub),
        Some(c) => format!("/review/sites?node={}&cursor={c}", f.sql_sub),
    };
    let one: SitesPage = get(&f, "page 1", &uri(None)).await;
    assert_eq!((one.total, one.offset, one.rows.len()), (1_200, 0, 500));
    let c1 = one.next_cursor.clone().expect("a second page");
    let two: SitesPage = get(&f, "page 2", &uri(Some(&c1))).await;
    assert_eq!((two.offset, two.rows.len()), (500, 500));
    let c2 = two.next_cursor.clone().expect("a third page");
    let three: SitesPage = get(&f, "page 3", &uri(Some(&c2))).await;
    assert_eq!((three.offset, three.rows.len()), (1_000, 200));
    assert_eq!(three.next_cursor, None);

    // Every site once, in one fixed order.
    let all: Vec<&str> = [&one, &two, &three]
        .iter()
        .flat_map(|p| p.rows.iter().map(|r| r.site_key.as_str()))
        .collect();
    let distinct: std::collections::BTreeSet<&str> = all.iter().copied().collect();
    assert_eq!(all.len(), 1_200);
    assert_eq!(distinct.len(), 1_200);

    // Writes between the pages move nothing: decide a row on page 1 and one on page 2, then read
    // page 2 again with the same cursor.
    for key in [&one.rows[3].site_key, &two.rows[3].site_key] {
        let (status, text) = call(
            &f.app,
            "POST",
            &format!("{}/decisions", base(&f)),
            Some(decision(Scope::Site, key, DecisionState::Confirmed, "")),
        )
        .await;
        assert_eq!(status, 200, "{text}");
    }
    let again: SitesPage = get(&f, "page 2 again", &uri(Some(&c1))).await;
    let keys = |p: &SitesPage| {
        p.rows
            .iter()
            .map(|r| r.site_key.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&again),
        keys(&two),
        "a write moved a row between pages"
    );
    assert!(again.rows[3].decision.is_some(), "the new decision shows");

    // A cursor that is not one of ours is refused, not read as the first page.
    let (status, err) = refused(&f, "GET", &uri(Some("not-a-cursor")), None).await;
    assert_eq!((status, err.code.as_str()), (422, "invalid_cursor"));
}

// ── U5: looking below the line writes nothing; the preview writes nothing ─────────────────

#[tokio::test]
async fn escalation_budget_preview_writes_nothing() {
    let f = review_fixture(12);
    let project_before = f.state.projects().get(&f.project_id).unwrap().review;
    let entry_before = f.state.inspections().latest(&f.project_id).unwrap();

    let req = json!({ "rate_per_kloc": 9.0, "floor": 0, "over_budget_reasons": [] });
    let outcome: EscalationBudgetOutcome =
        post_ok(&f, "the preview", "/escalation-budget/preview", req).await;
    assert!(!outcome.applied);
    // Looking below the line is a view flag and writes nothing either.
    let _: QueueView = get(&f, "the queue", "/review/queue?show_below=true").await;

    let project_after = f.state.projects().get(&f.project_id).unwrap().review;
    assert_eq!(project_after, project_before, "the preview wrote a setting");
    let entry_after = f.state.inspections().latest(&f.project_id).unwrap();
    let tree = |e: &camerata_server::inspections::InspectionEntry| e.report.tree.clone().unwrap();
    assert_eq!(tree(&entry_after).budget, tree(&entry_before).budget);
    assert_eq!(
        tree(&entry_after).candidates,
        tree(&entry_before).candidates
    );
    assert!(f.state.decisions().list(&f.project_id).is_empty());
    assert!(f.state.edits().list(&f.project_id).is_empty());
    assert!(f.state.engine_check().list(&f.project_id).is_empty());
    // And the shell still shows the line the settings give.
    let shell: ReviewShellView = get(&f, "the shell", "/review").await;
    assert_eq!(shell.budget.floor, project_before.escalation_floor);
}

#[tokio::test]
async fn escalation_budget_preview_matches_re_mark() {
    let f = review_fixture(12);
    // The stored candidates, marked again by the same function the scan used, under the
    // proposed settings.
    let entry = f.state.inspections().latest(&f.project_id).unwrap();
    let stored = entry.report.tree.clone().unwrap();
    let propose = |rate: f32, floor: usize, reasons: Vec<EscalationReason>| {
        let mut candidates = stored.candidates.clone();
        let mut budget = EscalationBudget::compute(rate, floor, stored.budget.counted_lines);
        re_mark(&mut candidates, &mut budget, &reasons);
        (budget, candidates)
    };

    for (rate, floor, reasons) in [
        (0.0, 1, Vec::new()),
        (0.0, 0, EscalationReason::default_over_budget_set()),
        (3.0, 40, vec![EscalationReason::SecretExposure]),
    ] {
        let (want_budget, want_candidates) = propose(rate, floor, reasons.clone());
        let outcome: EscalationBudgetOutcome = post_ok(
            &f,
            "the preview",
            "/escalation-budget/preview",
            json!({ "rate_per_kloc": rate, "floor": floor, "over_budget_reasons": reasons }),
        )
        .await;
        assert_eq!(outcome.after, want_budget, "rate {rate}, floor {floor}");
        let was: Vec<bool> = stored.candidates.iter().map(|c| c.escalated).collect();
        let now: Vec<bool> = want_candidates.iter().map(|c| c.escalated).collect();
        let above = was.iter().zip(&now).filter(|(w, n)| !**w && **n).count();
        let below = was.iter().zip(&now).filter(|(w, n)| **w && !**n).count();
        assert_eq!((outcome.moved_above, outcome.moved_below), (above, below));
    }

    // Applying it is the same answer, and the next read shows that line.
    let (want_budget, _) = propose(0.0, 1, Vec::new());
    let applied: EscalationBudgetOutcome = post_ok(
        &f,
        "the write",
        "/escalation-budget",
        json!({ "rate_per_kloc": 0.0, "floor": 1, "over_budget_reasons": [] }),
    )
    .await;
    assert!(applied.applied);
    assert_eq!(applied.after, want_budget);
    let shell: ReviewShellView = get(&f, "the shell", "/review").await;
    assert_eq!(shell.budget, want_budget);
    let queue: QueueView = get(&f, "the queue", "/review/queue").await;
    assert_eq!(queue.escalated.len(), want_budget.escalated);
    assert_eq!(
        queue.below_total,
        want_budget.candidates - want_budget.escalated
    );
    // The stored report keeps the line the scan marked: only the setting changed.
    let entry = f.state.inspections().latest(&f.project_id).unwrap();
    assert_eq!(entry.report.tree.unwrap().budget, stored.budget);
}

#[tokio::test]
async fn the_budget_and_sample_routes_refuse_values_outside_the_bounds() {
    let f = review_fixture(6);
    for body in [
        json!({ "rate_per_kloc": 10.5, "floor": 1, "over_budget_reasons": [] }),
        json!({ "rate_per_kloc": -1.0, "floor": 1, "over_budget_reasons": [] }),
        json!({ "rate_per_kloc": 1.0, "floor": 1001, "over_budget_reasons": [] }),
    ] {
        for uri in ["/escalation-budget", "/escalation-budget/preview"] {
            let (status, err) = refused(&f, "POST", uri, Some(body.clone())).await;
            assert_eq!(
                (status, err.code.as_str()),
                (422, "invalid_setting"),
                "{uri} {body}"
            );
        }
    }
    let (status, err) = refused(
        &f,
        "POST",
        "/sample-size",
        Some(json!({ "sample_size_per_run": 1001 })),
    )
    .await;
    assert_eq!((status, err.code.as_str()), (422, "invalid_setting"));
    // Nothing was written.
    let review = f.state.projects().get(&f.project_id).unwrap().review;
    assert_eq!(
        review,
        camerata_api_types::project::ReviewSettings {
            group_review_model: review.group_review_model.clone(),
            ..Default::default()
        }
    );
}

// ── the Engine Check ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_engine_check_no_holds_the_row_until_it_is_corrected() {
    let f = review_fixture(12);
    let lane: EngineCheckLane = get(&f, "the lane", "/review/engine-check").await;
    let key = lane.cards[0].site.site_key.clone();
    let held_before = get::<ReviewShellView>(&f, "the shell", "/review")
        .await
        .counts
        .held;

    // A No must say what was wrong.
    let (status, err) = refused(
        &f,
        "POST",
        &format!("/review/engine-check/{}", path_key(&key)),
        Some(json!({ "right": false })),
    )
    .await;
    assert_eq!((status, err.code.as_str()), (422, "wrong_required"));

    let _: EngineCheckVerdict = post_ok(
        &f,
        "a no",
        &format!("/review/engine-check/{}", path_key(&key)),
        json!({ "right": false, "wrong": "not_a_real_issue" }),
    )
    .await;
    let shell: ReviewShellView = get(&f, "the shell", "/review").await;
    assert_eq!(shell.counts.held, held_before + 1);
    assert!(shell
        .export_gate
        .held
        .iter()
        .any(|h| h.key == key && h.reason == HoldReason::EngineCheckNoWithoutCorrection));

    // A decision on the row is the correction.
    let (status, text) = call(
        &f.app,
        "POST",
        &format!("{}/decisions", base(&f)),
        Some(decision_at(
            Scope::Site,
            &key,
            DecisionState::FalsePositive,
            "not a real issue",
            LATER,
        )),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let shell: ReviewShellView = get(&f, "the shell", "/review").await;
    assert_eq!(shell.counts.held, held_before);
    assert!(shell.export_gate.held.iter().all(|h| h.key != key));
    // The lane shows the verdict on the card.
    let lane: EngineCheckLane = get(&f, "the lane", "/review/engine-check").await;
    let card = lane.cards.iter().find(|c| c.site.site_key == key).unwrap();
    assert_eq!(
        card.verdict.as_ref().unwrap().wrong,
        Some(EngineCheckError::NotARealIssue)
    );

    // A row outside the sample cannot be judged.
    let (status, err) = refused(
        &f,
        "POST",
        &format!("/review/engine-check/{}", path_key("not-a-sampled-row")),
        Some(json!({ "right": true })),
    )
    .await;
    assert_eq!((status, err.code.as_str()), (404, "not_in_sample"));
}

#[tokio::test]
async fn a_sample_size_of_zero_turns_the_lane_off() {
    let f = review_fixture(12);
    let size: SampleSizeResp = post_ok(
        &f,
        "the size",
        "/sample-size",
        json!({ "sample_size_per_run": 0 }),
    )
    .await;
    assert!(size.lane_off);
    let lane: EngineCheckLane = get(&f, "the lane", "/review/engine-check").await;
    assert!(lane.off && lane.cards.is_empty() && lane.drawn == 0);
    let shell: ReviewShellView = get(&f, "the shell", "/review").await;
    assert!(shell.lanes.engine_check_off);
    assert_eq!(shell.lanes.engine_check, 0);
}

#[tokio::test]
async fn a_node_decision_leaves_out_an_engine_check_no_that_nobody_has_corrected() {
    let f = review_fixture(12);
    let lane: EngineCheckLane = get(&f, "the lane", "/review/engine-check").await;
    let card = lane
        .cards
        .iter()
        .find(|c| {
            c.site.group_path.get(2) == Some(&f.sql_sub)
                && c.site.held.is_none()
                && c.site.decision.is_none()
                && !c.site.markers.contains(&Marker::Outlier)
        })
        .expect("a plain sampled row in the SQL sub-group");
    let key = card.site.site_key.clone();
    let _: EngineCheckVerdict = post_ok(
        &f,
        "a no",
        &format!("/review/engine-check/{}", path_key(&key)),
        json!({ "right": false, "wrong": "wrong_grouping" }),
    )
    .await;

    let recorded: camerata_api_types::decisions::DecisionRecorded = post_ok(
        &f,
        "a group decision",
        "/decisions",
        decision(
            Scope::Node(f.sql_sub.clone()),
            "",
            DecisionState::Confirmed,
            "the pattern is real",
        ),
    )
    .await;
    assert!(
        recorded.decision.except.contains(&key),
        "the group decision covered a row the Engine Check said the engine got wrong: {:?}",
        recorded.decision.except
    );
    assert!(!recorded.decision.applies_to.contains(&key));
}

/// The same 30,000 sites through the route: the whole request (the stored inspection read and
/// sealed again, the overlay, the composer, the JSON) for the tree and for one node's first page.
/// The size is held; the time is recorded.
#[tokio::test]
async fn a_thirty_thousand_site_project_serves_its_tree_and_a_page_through_the_route() {
    use camerata_server::inspections::{InspectionEntry, InspectionKind};
    use camerata_server::onboard::{Finding, ScanReport};
    use std::sync::Arc;

    let state =
        camerata_server::AppState::new(Arc::new(camerata_worktracker::InMemoryStoryStore::new()));
    let project = state
        .projects()
        .create("Big", vec![review::REPO.to_string()])
        .unwrap();
    let mut report = ScanReport::gated(&[review::REPO.to_string()]);
    report.gated = false;
    report.code_lines = 300_000;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:05:00Z".to_string();
    report.findings = (0..30_000)
        .map(|i| {
            let s = i / 13;
            let (c, r, k) = (s / 40, (s / 5) % 8, s % 5);
            let rule = format!("RULE-{c:02}-{r}");
            Finding {
                repo: review::REPO.to_string(),
                path: format!("src/mod{}/file{}.rs", i % 97, i / 7),
                line: 1 + i % 500,
                rule_id: rule.clone(),
                severity: "medium".to_string(),
                snippet: format!("run({i})"),
                detail: format!("Title {i} \u{2014} The call trusts the caller here."),
                group_path: vec![
                    format!("cat:cat{c:02}"),
                    format!("rule:{rule}"),
                    format!("sub:{rule}/{k}"),
                ],
                ..Finding::default()
            }
        })
        .collect();
    let entry = InspectionEntry::from_report(&project.id, report, InspectionKind::Full);
    let entry = state.inspections().append(entry).unwrap();
    let app = camerata_server::router(state.clone());
    let b = format!("/api/projects/{}", project.id);

    let t = std::time::Instant::now();
    let (status, body) = call(&app, "GET", &format!("{b}/review/tree"), None).await;
    let tree_time = t.elapsed();
    assert_eq!(status, 200);
    let tree: TreeView = round_trip("the tree", &body);
    eprintln!(
        "30,000 sites over the route: {} nodes, tree {} bytes in {tree_time:?}",
        tree.nodes.len(),
        body.len()
    );
    assert!(body.len() < 2 * 1024 * 1024, "{} bytes", body.len());

    let sub = tree
        .nodes
        .iter()
        .find(|n| n.counts.sites > 0 && n.id.starts_with("sub:"))
        .unwrap();
    let t = std::time::Instant::now();
    let (status, body) = call(
        &app,
        "GET",
        &format!("{b}/review/sites?node={}", sub.id),
        None,
    )
    .await;
    eprintln!(
        "first page of {}: {} bytes in {:?}",
        sub.id,
        body.len(),
        t.elapsed()
    );
    assert_eq!(status, 200);
    let page: SitesPage = round_trip("a page", &body);
    assert_eq!(page.total, sub.counts.sites);
    assert_eq!(entry.report.findings.len(), 30_000);
}
