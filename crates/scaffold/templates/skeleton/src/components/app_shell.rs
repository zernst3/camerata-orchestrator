use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct AppShellProps {
    pub title: String,
    #[props(default)]
    pub subtitle: String,
    children: Element,
}

/// The app's one top-level responsive layout shell (RUST-DIOXUS-14): a header +
/// a constrained, centered main column. Every route renders through this rather
/// than hand-rolling its own page chrome.
#[component]
pub fn AppShell(props: AppShellProps) -> Element {
    rsx! {
        div { class: "app-shell",
            header { class: "app-shell__header",
                h1 { class: "app-shell__title", "{props.title}" }
                if !props.subtitle.is_empty() {
                    p { class: "app-shell__subtitle", "{props.subtitle}" }
                }
            }
            main { class: "app-shell__main", {props.children} }
        }
    }
}
