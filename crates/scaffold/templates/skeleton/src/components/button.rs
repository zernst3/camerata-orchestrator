use dioxus::prelude::*;

/// The button's visual weight. Maps 1:1 to a `.btn--*` class in
/// `assets/design/components.css` — that file is the one place these decisions
/// (color, hover/focus states) live.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
    Ghost,
}

impl ButtonVariant {
    fn class(self) -> &'static str {
        match self {
            ButtonVariant::Primary => "btn btn--primary",
            ButtonVariant::Secondary => "btn btn--secondary",
            ButtonVariant::Ghost => "btn btn--ghost",
        }
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct ButtonProps {
    #[props(default)]
    pub variant: ButtonVariant,
    #[props(default)]
    pub disabled: bool,
    pub onclick: EventHandler<MouseEvent>,
    children: Element,
}

/// The one canonical clickable-action element in the app (RUST-DIOXUS-14). Views
/// never render a bare `button { .. }` — they compose this.
#[component]
pub fn Button(props: ButtonProps) -> Element {
    rsx! {
        button {
            class: "{props.variant.class()}",
            disabled: props.disabled,
            r#type: "button",
            onclick: move |evt| props.onclick.call(evt),
            {props.children}
        }
    }
}
