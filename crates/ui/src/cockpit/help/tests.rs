//! Tests of the shared help dialog: the real button and the real dialog, driven the way an
//! operator drives them (a click on the "?", Esc, the Close button, a click outside).
//!
//! A virtual DOM has no focus, so these tests prove the parts that decide focus: the dialog is
//! focusable, labelled and modal, the button's id is the one the return-focus script names, and
//! the scripts name the right elements. That focus really lands is checked in Chrome by the
//! coordinator's click-through, not here.

use std::collections::HashMap;
use std::rc::Rc;

use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
use dioxus::html::{Code, Key, Location, Modifiers, SerializedKeyboardData};

use super::*;

fn topic() -> HelpTopic {
    camerata_ui_core::help::inspection::checks_topic()
}

fn long_topic() -> HelpTopic {
    HelpTopic {
        id: "long-one",
        title: "A long topic",
        body: (0..40)
            .map(|i| format!("Paragraph {i} of a body long enough to need a scrollbar."))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

/// What a mounted screen has told us about its elements.
struct Screen {
    vdom: VirtualDom,
    by_html_id: HashMap<String, ElementId>,
    listeners: Vec<(String, ElementId)>,
}

impl Screen {
    fn mount(app: fn() -> Element) -> Self {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(app);
        let mut screen = Self {
            vdom: VirtualDom::new(|| rsx! {}),
            by_html_id: HashMap::new(),
            listeners: Vec::new(),
        };
        screen.take(&vdom.rebuild_to_vec());
        screen.vdom = vdom;
        screen
    }

    fn take(&mut self, m: &Mutations) {
        for edit in &m.edits {
            match edit {
                Mutation::SetAttribute {
                    name: "id",
                    value: AttributeValue::Text(text),
                    id,
                    ..
                } => {
                    self.by_html_id.insert(text.clone(), *id);
                }
                Mutation::NewEventListener { name, id } => {
                    self.listeners.push((name.clone(), *id));
                }
                _ => {}
            }
        }
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            let edits = self.vdom.render_immediate_to_vec();
            self.take(&edits);
        }
    }

    fn html(&self) -> String {
        dioxus_ssr::render(&self.vdom)
    }

    fn click(&mut self, id: ElementId) {
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        self.vdom
            .runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(data, true), id);
        self.settle();
    }

    fn click_html_id(&mut self, html_id: &str) {
        let id = *self
            .by_html_id
            .get(html_id)
            .unwrap_or_else(|| panic!("no element {html_id}: {:?}", self.by_html_id.keys()));
        self.click(id);
    }

    fn key(&mut self, html_id: &str, key: Key, code: Code) {
        let id = *self.by_html_id.get(html_id).expect("the dialog is drawn");
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            SerializedKeyboardData::new(
                key,
                code,
                Location::Standard,
                false,
                Modifiers::empty(),
                false,
            ),
        )));
        self.vdom.runtime().handle_event(
            "keydown",
            dioxus::dioxus_core::Event::new(data, true),
            id,
        );
        self.settle();
    }
}

fn one_button() -> Element {
    rsx! { HelpButton { topic: topic() } }
}

fn long_button() -> Element {
    rsx! { HelpButton { topic: long_topic() } }
}

#[test]
fn the_button_is_a_named_question_mark_and_the_dialog_is_not_drawn_until_it_is_pressed() {
    let screen = Screen::mount(one_button);
    let html = screen.html();
    assert!(
        html.contains("aria-label=\"About Checks to run\""),
        "{html}"
    );
    assert!(html.contains("aria-haspopup=\"dialog\""), "{html}");
    assert!(html.contains(">?</button>"), "{html}");
    assert!(!html.contains("role=\"dialog\""), "{html}");
    assert!(
        !html.contains("Deterministic checks only runs"),
        "the long text is not on the page: {html}"
    );
}

#[test]
fn pressing_the_button_opens_a_modal_dialog_with_the_topics_text_and_a_close_button() {
    let mut screen = Screen::mount(one_button);
    screen.click_html_id(&button_id("checks-to-run"));
    let html = screen.html();
    assert!(html.contains("role=\"dialog\""), "{html}");
    assert!(html.contains("aria-modal=\"true\""), "{html}");
    assert!(
        html.contains(&format!(
            "aria-labelledby=\"{}\"",
            title_id("checks-to-run")
        )),
        "{html}"
    );
    assert!(
        html.contains(&format!("<h3 id=\"{}\"", title_id("checks-to-run"))),
        "{html}"
    );
    // A dialog that can take focus, which the open script then gives it.
    assert!(html.contains("tabindex=\"-1\""), "{html}");
    assert!(html.contains("Deterministic checks only runs"), "{html}");
    assert!(html.contains(">Close</button>"), "{html}");
    assert!(
        focus_dialog_js("checks-to-run").contains(&dialog_id("checks-to-run")),
        "the open script names this dialog"
    );
    assert!(html.contains(&format!("id=\"{}\"", dialog_id("checks-to-run"))));
}

#[test]
fn escape_closes_the_dialog_and_the_return_script_names_the_button_that_opened_it() {
    let mut screen = Screen::mount(one_button);
    screen.click_html_id(&button_id("checks-to-run"));
    assert!(screen.html().contains("role=\"dialog\""));
    screen.key(&dialog_id("checks-to-run"), Key::Escape, Code::Escape);
    let html = screen.html();
    assert!(!html.contains("role=\"dialog\""), "{html}");
    // The button is still there, and it is the element the return script focuses.
    assert!(
        html.contains(&format!("id=\"{}\"", button_id("checks-to-run"))),
        "{html}"
    );
    let back = focus_button_js("checks-to-run");
    assert!(
        back.contains(&format!("getElementById('{}')", button_id("checks-to-run"))),
        "{back}"
    );
}

#[test]
fn another_key_does_not_close_it() {
    let mut screen = Screen::mount(one_button);
    screen.click_html_id(&button_id("checks-to-run"));
    screen.key(
        &dialog_id("checks-to-run"),
        Key::Character("a".into()),
        Code::KeyA,
    );
    assert!(screen.html().contains("role=\"dialog\""));
}

#[test]
fn the_close_button_closes_it_and_can_be_reopened() {
    let mut screen = Screen::mount(one_button);
    screen.click_html_id(&button_id("checks-to-run"));
    screen.click_html_id(&close_id("checks-to-run"));
    assert!(!screen.html().contains("role=\"dialog\""));
    screen.click_html_id(&button_id("checks-to-run"));
    assert!(screen.html().contains("role=\"dialog\""), "it opens again");
}

#[test]
fn a_click_outside_the_panel_closes_it_and_a_click_inside_does_not() {
    let mut screen = Screen::mount(one_button);
    screen.click_html_id(&button_id("checks-to-run"));
    screen.click_html_id(&dialog_id("checks-to-run"));
    assert!(
        screen.html().contains("role=\"dialog\""),
        "a click inside the panel leaves it open"
    );
    screen.click_html_id(&backdrop_id("checks-to-run"));
    assert!(!screen.html().contains("role=\"dialog\""));
}

#[test]
fn a_long_topic_scrolls_inside_the_dialog_and_the_text_is_selectable() {
    let mut screen = Screen::mount(long_button);
    screen.click_html_id(&button_id("long-one"));
    let html = screen.html();
    assert!(
        html.contains("Paragraph 39 of a body"),
        "the whole text is in the dialog"
    );
    assert!(html.contains("class=\"help-body\""), "{html}");
    let css = crate::style::GLOBAL_CSS;
    let body = css.split(".help-body {").nth(1).expect("a help-body rule");
    let body = &body[..body.find('}').unwrap()];
    assert!(body.contains("overflow-y: auto"), "{body}");
    let dialog = css
        .split(".help-dialog {")
        .nth(1)
        .expect("a help-dialog rule");
    let dialog = &dialog[..dialog.find('}').unwrap()];
    assert!(dialog.contains("max-height"), "{dialog}");
    assert!(dialog.contains("user-select: text"), "{dialog}");
}

#[test]
fn the_scripts_only_name_ids_made_of_letters_digits_and_dashes() {
    assert_eq!(button_id("a b'c"), "help-btn-a-b-c");
    for js in [
        focus_dialog_js("x"),
        focus_button_js("x"),
        tab_inside_js("x", true),
        tab_inside_js("x", false),
    ] {
        assert!(!js.contains("undefined"), "{js}");
    }
    assert!(tab_inside_js("x", true).contains("true ?"));
    assert!(tab_inside_js("x", false).contains("false ?"));
}
