//! The seed command: a fixture run and its overlays load into a data directory through the
//! server's own stores, and the result opens as the review project the smoke checklist expects.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_api_types::review_view::{
    EngineCheckLane, ExportGateView, HoldReason, Marker, QueueView, ReviewCounts,
    ReviewSettingsView, ReviewShellView, SitesPage, TreeLevel, TreeView,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tower::ServiceExt;

use super::{seed_review, store_dir, SeedError, SeedOptions, Seeded, Variant};
use crate::release_gate::{assemble_package, release, CheckId, ReleaseError};
use crate::report_export::{build_report_json, render_data_json, ReportOptions, ReviewedReport};
use crate::{router, AppState};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/review-demo")
}

fn fixture_json(file: &str) -> Value {
    let text = std::fs::read_to_string(fixture_dir().join(file)).expect("fixture file");
    serde_json::from_str(&text).expect("fixture json")
}

async fn seed(variant: Variant) -> (tempfile::TempDir, Seeded) {
    let dir = tempfile::tempdir().expect("temp dir");
    let seeded = seed_review(
        &fixture_dir(),
        dir.path(),
        &SeedOptions {
            variant,
            force: false,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("the seed must succeed: {e}"));
    (dir, seeded)
}

/// A server opened on the seeded directory from scratch, as the liveview build opens it.
fn reopen(seeded: &Seeded) -> (AppState, axum::Router) {
    let state = AppState::from_data_dir(&seeded.store_dir);
    let app = router(state.clone());
    (state, app)
}

async fn call(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get<T: DeserializeOwned>(
    app: &axum::Router,
    seeded: &Seeded,
    what: &str,
    tail: &str,
) -> T {
    let uri = format!("/api/projects/{}/{tail}", seeded.project_id);
    let (status, body) = call(app, "GET", &uri).await;
    assert_eq!(status, 200, "{what}: {body}");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{what} does not parse: {e}\n{body}"))
}

/// The (path, line) of the fixture site `id`, in the run the variant ends on.
fn place_of(id: &str, variant: Variant) -> (String, usize) {
    let run = fixture_json("run.json");
    let mut sites: Vec<Value> = run["sites"].as_array().unwrap().clone();
    sites.extend(run["thrown_out"].as_array().unwrap().iter().cloned());
    if variant == Variant::Moved {
        let moved = fixture_json("moved.json");
        sites.extend(moved["new_sites"].as_array().unwrap().iter().cloned());
    }
    let site = sites
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no fixture site {id}"));
    let mut line = site["line"].as_u64().unwrap() as usize;
    if variant == Variant::Moved {
        let moved = fixture_json("moved.json");
        if let Some(m) = moved["moves"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["site"] == id)
        {
            line = m["line"].as_u64().unwrap() as usize;
        }
    }
    (site["path"].as_str().unwrap().to_string(), line)
}

type Held = (HoldReason, String, usize);

fn sorted(mut rows: Vec<Held>) -> Vec<Held> {
    rows.sort_by_key(|(reason, path, line)| (reason.phrase(), path.clone(), *line));
    rows
}

/// The rows the fixture says the release gate must refuse for `variant`, as (reason, path, line).
fn expected_held(variant: Variant) -> Vec<Held> {
    let overlays = fixture_json("overlays.json");
    let key = match variant {
        Variant::Base => "base",
        Variant::Moved => "moved",
    };
    let rows: Vec<Held> = overlays["expected_held"][key]
        .as_array()
        .expect("expected_held lists the variant")
        .iter()
        .map(|h| {
            let reason: HoldReason =
                serde_json::from_value(h["reason"].clone()).expect("a hold reason");
            let (path, line) = place_of(h["site"].as_str().unwrap(), variant);
            (reason, path, line)
        })
        .collect();
    sorted(rows)
}

fn view_rows(view: &ExportGateView) -> Vec<Held> {
    sorted(
        view.held
            .iter()
            .chain(&view.merged_without_decision)
            .map(|h| (h.reason, h.path.clone(), h.line))
            .collect(),
    )
}

/// How many rows carry each marker, over every site of the run (read page by page from the
/// category nodes, as the tree screen reads them).
async fn marker_counts(
    app: &axum::Router,
    seeded: &Seeded,
) -> std::collections::HashMap<Marker, usize> {
    let tree: TreeView = get(app, seeded, "the tree", "review/tree").await;
    let mut counts = std::collections::HashMap::new();
    let mut rows = 0;
    for node in tree.nodes.iter().filter(|n| n.level == TreeLevel::Category) {
        let page: SitesPage = get(
            app,
            seeded,
            "a node's sites",
            &format!("review/sites?node={}", node.id),
        )
        .await;
        assert!(
            page.next_cursor.is_none(),
            "the fixture is small: one page a node"
        );
        rows += page.rows.len();
        for row in &page.rows {
            for marker in &row.markers {
                *counts.entry(*marker).or_insert(0) += 1;
            }
        }
    }
    counts.insert(
        Marker::Held,
        counts.get(&Marker::Held).copied().unwrap_or(0),
    );
    assert_eq!(rows, seeded.shell.counts.total, "every site is on a page");
    counts
}

// ── the seed loads the run and its overlays ──────────────────────────────────────────────

#[tokio::test]
async fn seed_review_loads_a_fixture_run_with_its_overlays() {
    let (_dir, seeded) = seed(Variant::Base).await;
    assert_eq!(seeded.run_ids.len(), 1);
    let (_state, app) = reopen(&seeded);

    // The shell: the counts the progress strip shows.
    let shell: ReviewShellView = get(&app, &seeded, "the shell", "review").await;
    assert_eq!(shell.run_id, seeded.run_ids[0]);
    assert_eq!(
        shell.counts,
        ReviewCounts {
            total: 31,
            unresolved: 15,
            confirmed: 11,
            ignored: 1,
            false_positive: 2,
            tech_debt: 1,
            merged: 1,
            held: 5,
        }
    );
    assert!(shell.group_reviewed);
    assert_eq!(
        (
            shell.stale.stale_decisions,
            shell.stale.stale_edits,
            shell.stale.moved
        ),
        (0, 0, 0),
        "nothing is stale in the base variant"
    );
    assert_eq!(shell.stale.new_since_decision, 0);

    // The line: two inside the budget of two, two over it (critical, severe reach), two below.
    assert_eq!(shell.budget.budget, 2);
    assert_eq!(
        (
            shell.budget.candidates,
            shell.budget.escalated,
            shell.budget.over_budget
        ),
        (6, 4, 2)
    );
    let queue: QueueView = get(&app, &seeded, "the queue", "review/queue").await;
    assert_eq!(queue.escalated.len(), 4);
    assert_eq!(queue.below_total, 2);

    // The tree: nodes and markers, never a site.
    let tree: TreeView = get(&app, &seeded, "the tree", "review/tree").await;
    assert!(!tree.nodes.is_empty());

    // The states the smoke checklist and the click-through tests exercise, one row each at least.
    let markers = marker_counts(&app, &seeded).await;
    let n = |m: Marker| markers.get(&m).copied().unwrap_or(0);
    assert_eq!(n(Marker::Held), 5, "one row of each held kind");
    assert_eq!(n(Marker::Escalated), 4, "the queue");
    assert_eq!(n(Marker::OverBudget), 2, "escalated past the budget");
    assert_eq!(n(Marker::Candidate), 2, "below the line");
    // The group reviewer's outlier, and the row the engine marks because its evidence is unverified.
    assert_eq!(n(Marker::Outlier), 2);
    assert_eq!(n(Marker::Edited), 3);
    assert_eq!(n(Marker::PossibleDuplicate), 1);
    assert_eq!(
        (
            n(Marker::Stale),
            n(Marker::Moved),
            n(Marker::NewSinceDecision)
        ),
        (0, 0, 0)
    );

    // The Engine Check lane: cards drawn, one Yes, one uncorrected No, one corrected No.
    let lane: EngineCheckLane = get(&app, &seeded, "the lane", "review/engine-check").await;
    assert!(lane.drawn >= 3, "{} cards", lane.drawn);
    let judged: Vec<_> = lane
        .cards
        .iter()
        .filter_map(|c| c.verdict.as_ref())
        .collect();
    assert_eq!(judged.len(), 3);
    assert_eq!(judged.iter().filter(|v| v.right).count(), 1);
    assert_eq!(
        judged
            .iter()
            .filter(|v| !v.right && v.correction.is_none())
            .count(),
        1
    );
    assert_eq!(
        judged
            .iter()
            .filter(|v| !v.right && v.correction.is_some())
            .count(),
        1
    );

    // The settings the fixture set.
    let settings: ReviewSettingsView = get(&app, &seeded, "the settings", "review/settings").await;
    assert_eq!(settings.escalation.floor, 2);
    assert_eq!(settings.engine_check.sample_size_per_run, 40);

    // The reviewed report: the overlay applied, one edit in each state the base variant has.
    let report: Value = get(&app, &seeded, "the reviewed report", "reviewed-report").await;
    assert_eq!(report["applied_edit_ids"].as_array().unwrap().len(), 3);
    assert_eq!(report["stale"].as_array().unwrap().len(), 0);
    assert_eq!(report["merges"].as_array().unwrap().len(), 1);
    assert_eq!(report["held"].as_array().unwrap().len(), 5);
    assert_eq!(report["findings"].as_array().unwrap().len(), 31);
}

#[tokio::test]
async fn seed_moved_variant_produces_stale_records() {
    let (_dir, seeded) = seed(Variant::Moved).await;
    assert_eq!(seeded.run_ids.len(), 2, "the first run and the rerun");
    let (_state, app) = reopen(&seeded);

    let shell: ReviewShellView = get(&app, &seeded, "the shell", "review").await;
    assert_eq!(shell.run_id, seeded.run_ids[1], "the rerun is the latest");
    assert_eq!(
        shell.stale.stale_decisions, 1,
        "the decision on the moved row"
    );
    assert_eq!(
        shell.stale.stale_edits, 1,
        "the edit on the other moved row"
    );
    assert_eq!(
        shell.stale.moved, 2,
        "two rows changed their snippet and key"
    );
    assert_eq!(
        shell.stale.new_since_decision, 1,
        "the new row in the decided group"
    );

    // Each state of the moved variant is on a row a reviewer can open.
    let markers = marker_counts(&app, &seeded).await;
    let n = |m: Marker| markers.get(&m).copied().unwrap_or(0);
    assert_eq!(n(Marker::Moved), 2);
    assert!(n(Marker::Stale) >= 1, "the row with the stale decision");
    assert_eq!(n(Marker::NewSinceDecision), 1);

    // The reviewed report names the stale edit, and where its site went.
    let report: Value = get(&app, &seeded, "the reviewed report", "reviewed-report").await;
    let stale = report["stale"].as_array().unwrap();
    assert_eq!(stale.len(), 1);
    assert!(
        !stale[0]["current_key"].as_str().unwrap().is_empty(),
        "the engine found where the edited site went"
    );
    // 31 rows of the first run and the new one.
    assert_eq!(report["findings"].as_array().unwrap().len(), 32);

    // A row whose line moved and whose key did not keeps its decision.
    let (path, line) = place_of("sql-01", Variant::Moved);
    let row = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path.as_str() && f["line"] == line)
        .expect("sql-01 at its new line");
    assert_eq!(row["line"], 47);
}

/// Every row of the run, read page by page from the category nodes.
async fn all_rows(
    app: &axum::Router,
    seeded: &Seeded,
) -> Vec<camerata_api_types::review_view::SiteRow> {
    let tree: TreeView = get(app, seeded, "the tree", "review/tree").await;
    let mut rows = Vec::new();
    for node in tree.nodes.iter().filter(|n| n.level == TreeLevel::Category) {
        let page: SitesPage = get(
            app,
            seeded,
            "a node's sites",
            &format!("review/sites?node={}", node.id),
        )
        .await;
        rows.extend(page.rows);
    }
    rows
}

#[tokio::test]
async fn a_stale_edit_on_a_moved_site_shows_on_the_moved_row() {
    let (_dir, seeded) = seed(Variant::Moved).await;
    let (_state, app) = reopen(&seeded);
    let edits: camerata_api_types::edits::EditsResp =
        get(&app, &seeded, "the edits", "edits").await;
    assert_eq!(edits.stale.len(), 1, "the seed leaves one stale edit");
    let moved_key = edits.stale[0].current_key.clone();
    assert!(!moved_key.is_empty(), "the engine found where it went");

    let rows = all_rows(&app, &seeded).await;
    let row = rows
        .iter()
        .find(|r| r.site_key == moved_key)
        .expect("the moved row is on a page");
    assert!(
        row.markers.contains(&Marker::Stale),
        "the moved row carries the Stale marker: {:?}",
        row.markers
    );
    assert!(
        row.edits
            .iter()
            .any(|e| e.id == edits.stale[0].edit.id && e.stale),
        "the moved row lists the stale edit with stale set: {:?}",
        row.edits
    );
    // The tree counts it on the row's node, not only in the shell.
    let tree: TreeView = get(&app, &seeded, "the tree", "review/tree").await;
    let stale_in_tree: usize = tree
        .nodes
        .iter()
        .filter(|n| n.level == TreeLevel::Category)
        .map(|n| n.counts.stale)
        .sum();
    assert!(
        stale_in_tree >= 2,
        "the stale decision and the stale edit: {stale_in_tree}"
    );
}

// ── refusing a data directory with something in it ───────────────────────────────────────

#[tokio::test]
async fn seed_refuses_a_non_empty_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("keep-me.txt");
    std::fs::write(&existing, "mine").unwrap();

    let refused = seed_review(&fixture_dir(), dir.path(), &SeedOptions::default()).await;
    assert!(
        matches!(refused, Err(SeedError::NotEmpty(_))),
        "a non-empty data dir is refused, got {refused:?}"
    );
    // Nothing was written and nothing was deleted.
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "mine");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert!(!store_dir(dir.path()).exists());

    // With force the seed goes ahead beside what is there, and still deletes nothing.
    let forced = seed_review(
        &fixture_dir(),
        dir.path(),
        &SeedOptions {
            variant: Variant::Base,
            force: true,
        },
    )
    .await
    .expect("force seeds a non-empty dir");
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "mine");
    assert!(store_dir(dir.path()).join("projects.json").exists());

    // A second forced seed adds a second project and leaves the first as it was.
    let again = seed_review(
        &fixture_dir(),
        dir.path(),
        &SeedOptions {
            variant: Variant::Base,
            force: true,
        },
    )
    .await
    .expect("a second forced seed");
    assert_ne!(again.project_id, forced.project_id);
    let state = AppState::from_data_dir(&store_dir(dir.path()));
    assert!(state.inspections().latest(&forced.project_id).is_some());
    assert!(state.inspections().latest(&again.project_id).is_some());
}

// ── the release gate refuses exactly the held rows the fixture declares ───────────────────

/// The package for `reviewed`, around a stand-in PDF, through the gate: the problems per check as
/// held row, or `None` when the gate let it through.
fn gate_refusals(reviewed: &ReviewedReport) -> Option<Vec<Held>> {
    let options = ReportOptions {
        client_name: "Acme Corporation".to_string(),
        ..ReportOptions::default()
    };
    let json = build_report_json(reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        reviewed,
        None,
        &options,
        "acme-portal-inspection",
        (json, data, b"%PDF-1.7 stand-in for the seed tests".to_vec()),
    )
    .expect("the package assembles");
    match release(package) {
        Ok(_) => None,
        Err(ReleaseError::Blocked(failure)) => {
            let mut rows = Vec::new();
            for outcome in &failure.report.checks {
                for p in outcome
                    .problems
                    .iter()
                    .filter(|p| p.file == "findings.json")
                {
                    let Some(reason) = reason_of(outcome.check) else {
                        continue;
                    };
                    if let Some((path, line)) = p.location.rsplit_once(" line ") {
                        rows.push((reason, path.to_string(), line.parse().unwrap()));
                    }
                }
            }
            Some(sorted(rows))
        }
        Err(other) => panic!("the package could not be assembled: {other}"),
    }
}

fn reason_of(check: CheckId) -> Option<HoldReason> {
    match check {
        CheckId::Content => Some(HoldReason::JunkContent),
        CheckId::EvidenceUnverified => Some(HoldReason::EvidenceUnverified),
        CheckId::RestoredWithoutDecision => Some(HoldReason::RestoredWithoutDecision),
        CheckId::MergedWithoutDecision => Some(HoldReason::MergedWithoutDecision),
        CheckId::EngineCheckNoWithoutCorrection => Some(HoldReason::EngineCheckNoWithoutCorrection),
        _ => None,
    }
}

#[tokio::test]
async fn seeded_project_export_is_blocked_by_exactly_the_seeded_held_rows() {
    for variant in [Variant::Base, Variant::Moved] {
        let (_dir, seeded) = seed(variant).await;
        let (state, app) = reopen(&seeded);
        let expected = expected_held(variant);
        assert!(!expected.is_empty(), "{variant:?}: the fixture holds rows");

        // What the export panel lists.
        let gate: ExportGateView =
            get(&app, &seeded, "the export gate", "review/export-gate").await;
        assert_eq!(view_rows(&gate), expected, "{variant:?}: the panel");

        // What the gate refuses, over the very bytes it would ship.
        let latest = state.inspections().latest(&seeded.project_id).unwrap();
        let (_, reviewed) = crate::edits::routes::review_of(&state, &seeded.project_id, &latest);
        let refused = gate_refusals(&reviewed).expect("the gate must refuse the seeded project");
        assert_eq!(refused, expected, "{variant:?}: the gate");
    }
}

#[tokio::test]
async fn an_uncorrected_engine_check_no_is_still_refused_after_a_rerun() {
    // The No was given on the first run; the rerun (the moved variant) has the same site under
    // the same key and no verdict of its own.
    let (_dir, seeded) = seed(Variant::Moved).await;
    let (state, app) = reopen(&seeded);
    let (path, line) = place_of("sql-11", Variant::Moved);
    let want: Held = (HoldReason::EngineCheckNoWithoutCorrection, path, line);

    let gate: ExportGateView = get(&app, &seeded, "the export gate", "review/export-gate").await;
    assert!(
        view_rows(&gate).contains(&want),
        "the panel still lists the row: {:?}",
        view_rows(&gate)
    );
    let latest = state.inspections().latest(&seeded.project_id).unwrap();
    let (_, reviewed) = crate::edits::routes::review_of(&state, &seeded.project_id, &latest);
    let refused = gate_refusals(&reviewed).expect("the gate must refuse the seeded project");
    assert!(
        refused.contains(&want),
        "the export is refused naming the site: {refused:?}"
    );
}

// ── a loader that forgets the Engine Check's evidence lets the gate pass silently ─────────

/// The non-test source files, relative to the crate's `src`.
fn source_files() -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let is_test = name == "tests.rs"
                    || name == "test_support.rs"
                    || name.ends_with("_tests.rs")
                    || rel.split('/').any(|part| part == "tests");
                if !is_test {
                    out.push((rel, std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

#[tokio::test]
async fn uncorrected_engine_check_no_is_refused_through_every_loader() {
    let (_dir, seeded) = seed(Variant::Base).await;
    let (state, _app) = reopen(&seeded);
    let id = seeded.project_id.as_str();
    let latest = state.inspections().latest(id).unwrap();
    let no_rows: Vec<Held> = expected_held(Variant::Base)
        .into_iter()
        .filter(|(r, _, _)| *r == HoldReason::EngineCheckNoWithoutCorrection)
        .collect();
    assert_eq!(no_rows.len(), 1, "the fixture has one uncorrected No");
    let (_, path, line) = no_rows[0].clone();

    // Every loader the server has: the review routes' and the export's.
    let sealed = latest
        .report
        .clone()
        .seal_with_restored(&crate::decisions::restored_keys_of(
            &state.decisions.active(id),
        ));
    let loaders: Vec<(&str, ReviewedReport)> = vec![
        (
            "edits::routes::review_of",
            crate::edits::routes::review_of(&state, id, &latest).1,
        ),
        (
            "reviewed_for_export",
            crate::reviewed_for_export(&state, id, &sealed, &Default::default()),
        ),
    ];
    for (name, reviewed) in &loaders {
        let panel = crate::release_gate::holds::gate_holds(reviewed);
        assert!(
            view_rows(&panel).contains(&(
                HoldReason::EngineCheckNoWithoutCorrection,
                path.clone(),
                line
            )),
            "{name}: the export panel lists the row the Engine Check said No to"
        );
        let refused = gate_refusals(reviewed).unwrap_or_else(|| {
            panic!("{name}: the gate let a package through with an uncorrected Engine Check No")
        });
        assert!(
            refused.contains(&(
                HoldReason::EngineCheckNoWithoutCorrection,
                path.clone(),
                line
            )),
            "{name}: the gate does not name the row: {refused:?}"
        );
    }

    // And no loader the list above does not know about: every place that applies the overlay in
    // production code is one of the two above or the definition, and attaches the evidence in
    // the same statement. A new loader that skips it fails here.
    let mut found = 0;
    for (file, text) in source_files() {
        for (at, _) in text.match_indices("apply_overlay(") {
            if text[..at].ends_with("fn ") || file == "report_export/overlay.rs" {
                continue;
            }
            found += 1;
            let statement = &text[at..];
            let statement = &statement[..statement.find(';').unwrap_or(statement.len())];
            assert!(
                statement.contains(".with_engine_check("),
                "{file} applies the overlay without attaching the Engine Check's evidence, so \
                 the release gate would pass an uncorrected No silently. Attach it, and add the \
                 loader to the list in this test."
            );
        }
    }
    assert_eq!(
        found,
        loaders.len(),
        "the loaders this test names are the ones in the source"
    );
}
