//! A review project for the review read model's integration tests (WO9c): a project with one
//! inspection whose tree has stage 2 data (candidates, an outlier, summaries) and one row of each
//! kind the release gate holds. Built through the server's public API only.

#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_api_types::review::{
    re_mark, EscalationBudget, EscalationCandidate, EscalationReason, OutlierReason,
};
use camerata_server::inspections::{InspectionEntry, InspectionKind};
use camerata_server::onboard::{Finding, ScanReport};
use camerata_server::review_tree::{NodeLevel, Outlier, OutlierBy};
use camerata_server::{router, AppState};
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

pub const REPO: &str = "acme/app";
pub const SQL_RULE: &str = "SEC-NO-RAW-SQL-CONCAT-1";
pub const AUTH_RULE: &str = "SEC-AUTHZ-4";

/// The keys of the rows with a story.
pub struct Keys {
    /// Escalated, critical.
    pub escalated: String,
    /// A candidate below the line.
    pub below_line: String,
    /// Marked as an outlier by the group reviewer.
    pub outlier: String,
    /// Quoted evidence not verified: held.
    pub unverified: String,
    /// Exports as merged with no decision record: held.
    pub merged: String,
    /// A row the engine threw out; the reviewer can put it back, and then it is held.
    pub restored: String,
    /// A plain row.
    pub plain: String,
}

pub struct ReviewFixture {
    pub state: AppState,
    pub app: axum::Router,
    pub project_id: String,
    pub run_id: String,
    pub keys: Keys,
    /// The sub-group node holding the SQL rows.
    pub sql_sub: String,
    /// The rule node of the SQL rows.
    pub sql_rule_node: String,
}

fn row(rule: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("run(\"{rule}\", {line})"),
        detail: format!("Title of {rule} at {line} \u{2014} The call trusts the caller here."),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

/// The report all the fixtures share: `n_sql` SQL rows in one sub-group and a few others.
pub fn report_with(n_sql: usize) -> ScanReport {
    let mut report = ScanReport::gated(&[REPO.to_string()]);
    report.gated = false;
    report.code_lines = 10_000;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:05:00Z".to_string();
    report.provenance.audited_refs = vec![camerata_server::onboard::AuditedRef {
        repo: REPO.to_string(),
        sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        ..camerata_server::onboard::AuditedRef::default()
    }];
    for i in 0..n_sql {
        report.findings.push(row(
            SQL_RULE,
            &format!("src/q{:05}.rs", i),
            10 + i % 40,
            "high",
        ));
    }
    for i in 0..3 {
        report
            .findings
            .push(row(AUTH_RULE, &format!("src/auth{i}.rs"), 5, "critical"));
    }
    report
}

/// Run one request against the router.
pub async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The percent-encoding of a site key for a path segment (a key holds NULs).
pub fn path_key(key: &str) -> String {
    key.bytes().map(|b| format!("%{b:02X}")).collect()
}

/// A project with `n_sql` SQL rows and the story rows. The scan's escalation settings are the
/// design's, so the budget is the floor of 20 until a test changes the project's settings.
pub fn review_fixture(n_sql: usize) -> ReviewFixture {
    assert!(n_sql >= 6, "the fixture needs six SQL rows for its stories");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects()
        .create("Acme", vec![REPO.to_string()])
        .expect("project");
    let mut report = report_with(n_sql);
    // The story rows are the first rows of the SQL rule.
    report.findings[1].captures.insert(
        camerata_server::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found near the cited line".to_string(),
    );
    // A row the engine threw out; the reviewer can put it back.
    let mut thrown = row(SQL_RULE, "src/thrown.rs", 3, "high");
    camerata_server::engine_discard::mark(&mut thrown, "its headline is a placeholder");
    report.discarded.push(thrown);
    // A second one stays thrown out, so the Engine Check has a thrown-out stratum to draw from.
    let mut stays = row(SQL_RULE, "src/thrown2.rs", 4, "high");
    camerata_server::engine_discard::mark(&mut stays, "its rule id names a placeholder");
    report.discarded.push(stays);
    let mut entry = InspectionEntry::from_report(&project.id, report, InspectionKind::Full);

    // The story rows by position: the tree may have reordered nothing, but go by path.
    let key_of = |entry: &InspectionEntry, path: &str| {
        entry
            .report
            .findings
            .iter()
            .chain(&entry.report.discarded)
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no row in {path}"))
            .site_key
            .clone()
    };
    let keys = Keys {
        plain: key_of(&entry, "src/q00000.rs"),
        unverified: key_of(&entry, "src/q00001.rs"),
        merged: key_of(&entry, "src/q00002.rs"),
        restored: key_of(&entry, "src/thrown.rs"),
        outlier: key_of(&entry, "src/q00004.rs"),
        below_line: key_of(&entry, "src/q00005.rs"),
        escalated: key_of(&entry, "src/auth0.rs"),
    };

    // The plain row has everything a row can say about itself.
    let dup_target = keys.unverified.clone();
    for f in &mut entry.report.findings {
        if f.site_key == keys.plain {
            f.effort = Some("low".to_string());
            f.confidence = Some("high".to_string());
            f.category = Some("injection".to_string());
            f.captures.insert(
                "stage1_verdict".to_string(),
                r#"{"real":"yes","reason":"the query is built from a request field"}"#.to_string(),
            );
            f.possible_duplicate_of = Some(camerata_server::review_tree::DuplicateLink {
                key: dup_target.clone(),
                hint: "same_line".to_string(),
            });
            f.flagged_by.push(camerata_server::review_tree::FlaggedBy {
                rule_id: "eslint/no-sql-concat".to_string(),
                tool: Some("eslint".to_string()),
                site_key: "acme/app\u{0}src/q00000.rs\u{0}ffffffffffffffff\u{0}0".to_string(),
                path: "src/q00000.rs".to_string(),
                line: 10,
            });
        }
    }

    // The merged row exports as merged with no decision behind it.
    let merged_target = keys.plain.clone();
    for f in &mut entry.report.findings {
        if f.site_key == keys.merged {
            f.captures
                .insert("merged_into".to_string(), merged_target.clone());
            f.captures.insert(
                "merged_into_label".to_string(),
                "Merged into src/q00000.rs:10".to_string(),
            );
        }
    }

    // Stage 2: two candidates, one escalated now and one below the line once the floor is 1, and
    // an outlier on the SQL sub-group.
    let tree = entry.report.tree.as_mut().expect("the entry has a tree");
    let sql_sub = tree
        .nodes
        .iter()
        .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&keys.plain))
        .expect("the SQL rows are in a sub-group")
        .id
        .clone();
    let sql_rule_node = tree
        .nodes
        .iter()
        .find(|n| n.level == NodeLevel::Rule && n.id.contains(SQL_RULE))
        .map(|n| n.id.clone())
        .expect("the SQL rule has a node");
    let candidate = |key: &str, reason, severity: &str, rank| EscalationCandidate {
        key: key.to_string(),
        node_id: sql_rule_node.clone(),
        reason,
        severity: severity.to_string(),
        stage2_rank: rank,
        global_rank: rank,
        escalated: false,
        over_budget_reason: None,
        why: "reachable without signing in".to_string(),
    };
    let mut candidates = vec![
        candidate(
            &keys.escalated,
            EscalationReason::UnauthenticatedReach,
            "critical",
            1,
        ),
        candidate(&keys.below_line, EscalationReason::RuleMisfit, "low", 2),
    ];
    let mut budget = EscalationBudget::compute(1.0, 20, 10_000);
    re_mark(
        &mut candidates,
        &mut budget,
        &EscalationReason::default_over_budget_set(),
    );
    tree.candidates = candidates;
    tree.budget = budget;
    tree.group_reviewed = true;
    if let Some(node) = tree.nodes.iter_mut().find(|n| n.id == sql_sub) {
        node.summary = Some("Every query in this group builds SQL from request text.".to_string());
        node.outliers.push(Outlier {
            key: keys.outlier.clone(),
            reason: OutlierReason::DifferentObject,
            by: OutlierBy::Model,
            why: "this one builds a table name, not a value".to_string(),
        });
    }
    let tree = entry.report.tree.take().expect("tree");
    tree.sync_rows(&mut entry.report.findings);
    entry.report.tree = Some(tree);

    let entry = state.inspections().append(entry).expect("append");
    let app = router(state.clone());
    ReviewFixture {
        state,
        app,
        project_id: project.id,
        run_id: entry.id,
        keys,
        sql_sub,
        sql_rule_node,
    }
}
