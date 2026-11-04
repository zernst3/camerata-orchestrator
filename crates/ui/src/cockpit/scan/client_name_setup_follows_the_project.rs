//! Tests: client name setup follows the project.

use super::ClientNameSetup;
use dioxus::prelude::*;
use std::cell::RefCell;

thread_local! {
    static SHOWN: RefCell<Option<Signal<(String, String)>>> = const { RefCell::new(None) };
}

fn app() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    let shown = use_signal(|| ("p1".to_string(), "Acme Ltd".to_string()));
    SHOWN.with(|s| *s.borrow_mut() = Some(shown));
    let (project_id, initial) = shown();
    rsx! {
        span { class: "probe", "{project_id}" }
        ClientNameSetup { project_id, initial }
    }
}

fn show(vdom: &mut VirtualDom, project_id: &str, initial: &str) -> String {
    let shown = SHOWN.with(|s| s.borrow().expect("the harness ran"));
    let next = (project_id.to_string(), initial.to_string());
    vdom.in_runtime(|| {
        let mut shown = shown;
        shown.set(next);
    });
    let _ = vdom.render_immediate_to_vec();
    dioxus_ssr::render(vdom)
}

fn start() -> (VirtualDom, String) {
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    (vdom, html)
}

#[test]
fn it_shows_the_saved_name_of_the_project_it_starts_on() {
    let (_vdom, html) = start();
    assert!(html.contains("Acme Ltd"), "{html}");
}

#[test]
fn switching_projects_shows_the_new_projects_name_never_the_old_ones() {
    let (mut vdom, _) = start();
    let html = show(&mut vdom, "p2", "Beta Corp");
    assert!(html.contains("Beta Corp"), "{html}");
    assert!(
        !html.contains("Acme Ltd"),
        "the first project's name must not be carried to the second: {html}"
    );
}

#[test]
fn a_project_with_no_name_yet_shows_an_empty_field_and_the_requirement() {
    let (mut vdom, _) = start();
    let html = show(&mut vdom, "p3", "");
    assert!(!html.contains("Acme Ltd"), "{html}");
    assert!(html.contains("Required before export."), "{html}");
}

#[test]
fn a_changed_saved_name_for_the_same_project_resets_the_field() {
    let (mut vdom, _) = start();
    let html = show(&mut vdom, "p1", "Acme Holdings");
    assert!(html.contains("Acme Holdings"), "{html}");
    assert!(!html.contains("Acme Ltd"), "{html}");
}
