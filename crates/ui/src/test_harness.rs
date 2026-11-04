//! The one server-side-rendering helper for the cockpit's views (review spec section 1, rule 3).
//!
//! `render_props` renders any props-only component to a complete HTML document carrying the app's
//! real stylesheet (`style::GLOBAL_CSS`) and the web-fonts link, so the same function serves a unit
//! test (assert on the markup) and the gallery subcommand (write a page an agent can screenshot).
//! Older per-file `VirtualDom` helpers keep working; new screens use this one.

use dioxus::prelude::*;

use crate::style::GLOBAL_CSS;
use camerata_ui::desktop_chrome::GOOGLE_FONTS_LINK;

/// Render `component` with `props` to the markup it draws, with no document around it.
pub fn render_fragment<P: Clone + 'static>(component: fn(P) -> Element, props: P) -> String {
    let mut vdom = VirtualDom::new_with_props(component, props);
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

/// Render `component` with `props` to a full HTML document: doctype, the fonts link, the global
/// stylesheet inlined, and the component's markup inside the app root.
pub fn render_props<P: Clone + 'static>(component: fn(P) -> Element, props: P) -> String {
    document("Camerata gallery", &render_fragment(component, props))
}

/// Wrap already-rendered `body` markup in the same document shell `render_props` uses.
pub fn document(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n{GOOGLE_FONTS_LINK}\n<style>{GLOBAL_CSS}</style>\n</head>\n\
         <body>\n<div class=\"app-root\">{body}</div>\n</body>\n</html>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, Props)]
    struct Hello {
        who: String,
    }

    #[component]
    fn Greeting(props: Hello) -> Element {
        rsx! { p { class: "greeting", "hello {props.who}" } }
    }

    #[test]
    fn render_props_draws_the_component_with_its_props() {
        let html = render_props(
            Greeting,
            Hello {
                who: "gallery".into(),
            },
        );
        assert!(html.contains("hello gallery"), "{html}");
    }

    #[test]
    fn render_props_returns_a_full_document_with_the_real_css_and_fonts() {
        let html = render_props(Greeting, Hello { who: "x".into() });
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(
            html.contains(GLOBAL_CSS),
            "the global stylesheet is inlined whole"
        );
        assert!(
            html.contains("fonts.googleapis.com"),
            "the fonts link is present"
        );
        assert!(html.contains("<meta name=\"viewport\""));
    }

    #[test]
    fn render_fragment_has_no_document_around_it() {
        let html = render_fragment(Greeting, Hello { who: "x".into() });
        assert!(!html.contains("<html"));
        assert!(html.contains("greeting"));
    }
}
