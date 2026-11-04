use super::*;
use crate::style::GLOBAL_CSS;
use std::path::PathBuf;

/// Spec section 12, copied here on purpose: the registry must equal this list exactly.
const SPEC_STATES: &[&str] = &[
    "shell/empty",
    "shell/loading",
    "shell/error",
    "shell/held-count",
    "tree/empty",
    "tree/loading",
    "tree/error",
    "tree/30k",
    "tree/markers",
    "tree/node-prompt",
    "tree/node-decided",
    "tree/not-group-reviewed",
    "tree/narrow",
    "queue/empty",
    "queue/loading",
    "queue/error",
    "queue/at-budget",
    "queue/over-budget",
    "queue/line-lowered",
    "queue/rate-changed-toast",
    "queue/30k",
    "engine/lane-off",
    "engine/loading",
    "engine/card-pending",
    "engine/no-list-open",
    "engine/no-pending-correction",
    "engine/no-corrected",
    "engine/all-verdicts",
    "engine/small-category",
    "detail/empty",
    "detail/plain",
    "detail/escalated",
    "detail/outlier",
    "detail/edited-field",
    "detail/stale-edit",
    "detail/stale-decision",
    "detail/moved",
    "detail/new-since-decision",
    "detail/held",
    "detail/merged",
    "detail/added",
    "detail/ignored-needs-ack",
    "detail/ignored-acked",
    "detail/edit-open",
    "detail/merge-picker",
    "detail/split-picker",
    "detail/add-form",
    "detail/add-refused",
    "settings/defaults",
    "settings/rate-changed",
    "settings/invalid",
    "settings/saving",
    "settings/error",
    "settings/run-in-flight",
    "export/clear",
    "export/blocked",
];

/// The states still placeholders, with the work order that owns each. A work order that builds a
/// screen deletes its lines here in the same commit that registers the real renderer.
const EXPECTED_PLACEHOLDERS: &[(&str, &str)] = &[
    ("shell/empty", "WO10"),
    ("shell/loading", "WO10"),
    ("shell/error", "WO10"),
    ("shell/held-count", "WO10"),
    ("tree/empty", "WO10"),
    ("tree/loading", "WO10"),
    ("tree/error", "WO10"),
    ("tree/30k", "WO10"),
    ("tree/markers", "WO10"),
    ("tree/node-prompt", "WO10"),
    ("tree/node-decided", "WO10"),
    ("tree/not-group-reviewed", "WO10"),
    ("tree/narrow", "WO10"),
    ("queue/empty", "WO11"),
    ("queue/loading", "WO11"),
    ("queue/error", "WO11"),
    ("queue/at-budget", "WO11"),
    ("queue/over-budget", "WO11"),
    ("queue/line-lowered", "WO11"),
    ("queue/rate-changed-toast", "WO11"),
    ("queue/30k", "WO11"),
    ("engine/lane-off", "WO11"),
    ("engine/loading", "WO11"),
    ("engine/card-pending", "WO11"),
    ("engine/no-list-open", "WO11"),
    ("engine/no-pending-correction", "WO11"),
    ("engine/no-corrected", "WO11"),
    ("engine/all-verdicts", "WO11"),
    ("engine/small-category", "WO11"),
    ("detail/empty", "WO11"),
    ("detail/plain", "WO11"),
    ("detail/escalated", "WO11"),
    ("detail/outlier", "WO11"),
    ("detail/edited-field", "WO11"),
    ("detail/stale-edit", "WO11"),
    ("detail/stale-decision", "WO11"),
    ("detail/moved", "WO11"),
    ("detail/new-since-decision", "WO11"),
    ("detail/held", "WO11"),
    ("detail/merged", "WO11"),
    ("detail/added", "WO11"),
    ("detail/ignored-needs-ack", "WO11"),
    ("detail/ignored-acked", "WO11"),
    ("detail/edit-open", "WO11"),
    ("detail/merge-picker", "WO11"),
    ("detail/split-picker", "WO11"),
    ("detail/add-form", "WO11"),
    ("detail/add-refused", "WO11"),
    ("export/clear", "WO10"),
    ("export/blocked", "WO10"),
];

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("camerata-gallery-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn every_spec_state_is_registered() {
    let registered: Vec<String> = registry().iter().map(GalleryState::id).collect();
    let spec: Vec<String> = SPEC_STATES.iter().map(|s| (*s).to_string()).collect();
    let missing: Vec<&String> = spec.iter().filter(|s| !registered.contains(s)).collect();
    let extra: Vec<&String> = registered.iter().filter(|s| !spec.contains(s)).collect();
    assert!(
        missing.is_empty(),
        "spec states not registered: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "registered states not in the spec: {extra:?}"
    );
    assert_eq!(
        registered, spec,
        "same states in the same order, no duplicates"
    );
}

#[test]
fn placeholders_are_listed_with_their_owner() {
    let listed: Vec<(String, &str)> = registry()
        .iter()
        .filter_map(|s| match s.kind {
            StateKind::Placeholder { owner } => Some((s.id(), owner)),
            StateKind::Built(_) => None,
        })
        .collect();
    let expected: Vec<(String, &str)> = EXPECTED_PLACEHOLDERS
        .iter()
        .map(|(id, owner)| ((*id).to_string(), *owner))
        .collect();
    assert_eq!(listed, expected);
}

#[test]
fn a_placeholder_page_says_what_is_not_built_and_who_owns_it() {
    let state = registry()
        .into_iter()
        .find(|s| s.id() == "tree/30k")
        .expect("tree/30k is registered");
    let html = state.render();
    assert!(
        html.contains("not built: tree/30k, owned by WO10"),
        "{html}"
    );
}

#[test]
fn gallery_renders_every_registered_state_to_a_file() {
    let out = scratch("every-state");
    let report = write_gallery(&out).unwrap();
    let states = registry();
    assert_eq!(report.pages, SPEC_STATES.len());
    assert_eq!(report.pages, states.len());
    for s in &states {
        let path = out.join(s.screen).join(format!("{}.html", s.state));
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} was not written: {e}", path.display()));
        assert!(!html.is_empty(), "{} is empty", path.display());
        assert!(
            html.contains(GLOBAL_CSS),
            "{} lacks the global css",
            path.display()
        );
    }
}

#[test]
fn the_index_links_every_page_grouped_by_screen_with_the_placeholder_count_on_top() {
    let out = scratch("index");
    let report = write_gallery(&out).unwrap();
    let index = std::fs::read_to_string(out.join("index.html")).unwrap();
    for s in registry() {
        assert!(
            index.contains(&format!("href=\"{}/{}.html\"", s.screen, s.state)),
            "{}",
            s.id()
        );
    }
    let head = index.find("placeholders").expect("count is shown");
    let first_link = index.find("href=\"shell/").expect("first link");
    assert!(
        head < first_link,
        "the placeholder count is above the links"
    );
    assert!(index.contains(&format!("{} placeholders", report.placeholders)));
    for screen in [
        "shell", "tree", "queue", "engine", "detail", "settings", "export",
    ] {
        assert!(
            index.contains(&format!("<h2>{screen}</h2>")),
            "group for {screen}"
        );
    }
}

#[test]
fn no_gallery_page_contains_an_em_dash() {
    assert_eq!(
        registry().len(),
        SPEC_STATES.len(),
        "a vacuous pass proves nothing"
    );
    for s in registry() {
        // The stylesheet's comments carry dashes; the rule is about rendered text.
        let page = s.render().replace(GLOBAL_CSS, "");
        assert!(!page.contains('\u{2014}'), "{}", s.id());
    }
}

#[test]
fn fixtures_cover_every_state() {
    use camerata_ui_core::test_support::review_fixtures::{fixture, FIXTURE_IDS};
    let registered: Vec<String> = registry().iter().map(GalleryState::id).collect();
    for id in &registered {
        assert!(
            fixture(id).is_some(),
            "{id} has neither a fixture nor is it excluded: every named state has a fixture"
        );
    }
    for id in FIXTURE_IDS {
        assert!(
            registered.iter().any(|r| r == id),
            "fixture {id} belongs to no registered state"
        );
    }
    assert_eq!(registered.len(), FIXTURE_IDS.len());
}

#[test]
fn route_sends_the_gallery_subcommand_to_the_gallery_and_nothing_to_the_app() {
    let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        route(&args(&["gallery", "/tmp/out"])),
        Launch::Gallery(args(&["/tmp/out"]))
    );
    assert_eq!(route(&args(&["gallery"])), Launch::Gallery(Vec::new()));
    assert_eq!(route(&[]), Launch::App);
    assert_eq!(route(&args(&["--something"])), Launch::App);
}
