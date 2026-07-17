use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct CardProps {
    children: Element,
}

/// A single canonical content-container primitive (RUST-DIOXUS-14): elevated
/// surface, consistent padding/radius/border from `assets/design/components.css`.
#[component]
pub fn Card(props: CardProps) -> Element {
    rsx! {
        div { class: "card", {props.children} }
    }
}
