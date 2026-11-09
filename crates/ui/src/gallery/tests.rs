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
    "tree/place",
    "queue/empty",
    "queue/loading",
    "queue/error",
    "queue/at-budget",
    "queue/over-budget",
    "queue/line-lowered",
    "queue/rate-changed-toast",
    "queue/30k",
    "queue/detail-form-open",
    "queue/merge-picker-empty-pool",
    "queue/place",
    "engine/lane-off",
    "engine/loading",
    "engine/card-pending",
    "engine/no-list-open",
    "engine/no-pending-correction",
    "engine/no-corrected",
    "engine/all-verdicts",
    "engine/small-category",
    "engine/thrown-out-held",
    "engine/thrown-out-confirmed",
    "engine/floor-binds",
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
    "detail/possible-duplicate",
    "detail/merge-picker-duplicate-first",
    "detail/over-budget",
    "detail/viewer-commit",
    "detail/thrown-out",
    "detail/carrier",
    "detail/carried",
    "settings/defaults",
    "settings/rate-changed",
    "settings/invalid",
    "settings/saving",
    "settings/error",
    "settings/run-in-flight",
    "export/clear",
    "export/blocked",
    "export/one-line-two-reasons",
];

/// The states still placeholders, with the work order that owns each. A work order that builds a
/// screen deletes its lines here in the same commit that registers the real renderer.
const EXPECTED_PLACEHOLDERS: &[(&str, &str)] = &[];

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

/// No screen is a placeholder any more, so the page that names what is not built is pinned on a
/// made-up state: the page stays right for the next screen that is added before it is built.
#[test]
fn a_placeholder_page_says_what_is_not_built_and_who_owns_it() {
    let state = GalleryState {
        screen: "x",
        state: "y",
        kind: StateKind::Placeholder { owner: "WOn" },
    };
    assert!(state.is_placeholder());
    let html = state.render();
    assert!(html.contains("not built: x/y, owned by WOn"), "{html}");
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

/// A site key joins its parts with NUL. A screen that puts one in an element id, an `href` or a
/// `data-` attribute writes a NUL byte into the page; this guards every owner's screens at once
/// (the tree, the queue, the Engine Check and the settings lane share it).
#[test]
fn no_gallery_page_contains_a_raw_nul_byte() {
    let out = scratch("no-nul");
    write_gallery(&out).unwrap();
    let mut checked = 0;
    for s in registry() {
        let path = out.join(s.screen).join(format!("{}.html", s.state));
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{} was not written: {e}", path.display()));
        assert!(
            !bytes.contains(&0u8),
            "{} contains a raw NUL byte: a key was written into the page without going through the one escape",
            s.id()
        );
        checked += 1;
    }
    let index = std::fs::read(out.join("index.html")).unwrap();
    assert!(!index.contains(&0u8), "index.html contains a raw NUL byte");
    assert_eq!(checked, SPEC_STATES.len(), "a vacuous pass proves nothing");
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

/// The one rule that themes every single-choice `<select>`: the text of its block in the
/// stylesheet, or a panic when it is missing.
fn select_rule_block() -> &'static str {
    let selector = "select:not([multiple]):not([size]) {";
    let start = GLOBAL_CSS
        .find(selector)
        .unwrap_or_else(|| panic!("the stylesheet has no `{selector}` rule"));
    let body = &GLOBAL_CSS[start..];
    &body[..body.find('}').expect("the select rule is closed")]
}

/// A native `<select>` draws its caret with the platform theme, and that drawing moved by about
/// two pixels between two runs on one machine and one Chrome (the visual check failed on nine
/// states with nothing else changed). Every single-choice select therefore drops the native
/// appearance and draws its caret from the stylesheet's own tokens, so the gallery, the app and
/// the pixel baselines cannot drift with the platform again.
#[test]
fn single_choice_selects_use_a_stylesheet_caret_not_the_native_one() {
    let rule = select_rule_block();
    assert!(
        rule.contains("appearance: none"),
        "selects must drop the native appearance: {rule}"
    );
    assert!(
        rule.contains("-webkit-appearance: none"),
        "the prefixed property too: {rule}"
    );
    assert!(
        rule.contains("background-image") && rule.contains("var(--ink)"),
        "the caret is drawn from the ink token: {rule}"
    );
    assert!(
        rule.contains("padding-right"),
        "room is kept for the caret: {rule}"
    );
}

/// Without the native appearance a disabled select lost its last visible cue (the greyed native
/// arrow), and several selects have no `:disabled` rule of their own, so one rule dims them all.
#[test]
fn a_disabled_single_choice_select_still_looks_disabled() {
    let selector = "select:not([multiple]):not([size]):disabled {";
    let start = GLOBAL_CSS
        .find(selector)
        .unwrap_or_else(|| panic!("the stylesheet has no `{selector}` rule"));
    let body = &GLOBAL_CSS[start..];
    let rule = &body[..body.find('}').expect("the disabled select rule is closed")];
    assert!(
        rule.contains("opacity"),
        "a disabled select is dimmed: {rule}"
    );
}

/// The rule above skips `multiple` and `size` selects (a list box has no caret). A gallery page
/// that rendered one, or a native file input, would draw platform pixels the rule does not cover.
#[test]
fn no_gallery_page_renders_a_native_control_the_stylesheet_does_not_theme() {
    let mut selects = 0;
    for s in registry() {
        let page = s.render().replace(GLOBAL_CSS, "");
        let mut rest = page.as_str();
        while let Some(at) = rest.find("<select") {
            let tail = &rest[at..];
            let end = tail.find('>').expect("a select tag is closed");
            let tag = &tail[..end];
            if tag.starts_with("<select ") || tag.starts_with("<select>") || tag == "<select" {
                selects += 1;
                assert!(
                    !tag.contains("multiple") && !tag.contains("size="),
                    "{} renders a list-box select the caret rule does not cover: {tag}",
                    s.id()
                );
            }
            rest = &tail[end..];
        }
        assert!(
            !page.contains("type=\"file\""),
            "{} renders a native file input, whose button is drawn by the platform",
            s.id()
        );
    }
    assert!(selects > 0, "a vacuous pass proves nothing");
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
