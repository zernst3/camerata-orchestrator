//! The home page: demonstrates the design system (AppShell, Card, Button, Field)
//! and a round-trip through a server function, so `dx serve` shows something real
//! on first run rather than a blank page.

use dioxus::prelude::*;

use crate::components::{AppShell, Button, ButtonVariant, Card, Field};
use crate::server_fns::greet;

#[component]
pub fn Home() -> Element {
    let mut name = use_signal(String::new);
    let mut greeting = use_signal(|| Option::<String>::None);
    let mut pending = use_signal(|| false);

    rsx! {
        AppShell {
            title: "{{APP_NAME}}",
            subtitle: "{{APP_DESCRIPTION}}",
            Card {
                h2 { "Say hello to the server" }
                p {
                    class: "text-muted",
                    "This calls a Dioxus server function (src/server_fns.rs) — the only \
                     way this app's frontend reaches the server."
                }
                Field {
                    label: "Your name",
                    value: name(),
                    placeholder: "World",
                    oninput: move |v| name.set(v),
                }
                Button {
                    variant: ButtonVariant::Primary,
                    disabled: pending(),
                    onclick: move |_| {
                        let current = name();
                        pending.set(true);
                        spawn(async move {
                            let result = greet(current).await;
                            pending.set(false);
                            greeting.set(result.ok());
                        });
                    },
                    if pending() { "Saying hello..." } else { "Say hello" }
                }
                if let Some(msg) = greeting() {
                    p { class: "greeting-result", "{msg}" }
                }
            }
        }
    }
}
