//! The one shared help dialog (settings overhaul, work orders B and C).
//!
//! A screen keeps at most one short line where the operator must act; everything longer is a
//! help topic (text as data in `camerata_ui_core::help`) behind a small "?" button. The button's
//! accessible name is "About <title>". Pressing it opens [`HelpDialog`]: focus moves into the
//! dialog, Tab stays inside it, Esc and the Close button close it, a click outside closes it, and
//! focus returns to the "?" button that opened it. The text is selectable and the body scrolls
//! when it is long.
//!
//! [`HelpDialog`] is props-only so a gallery state can draw it open. [`HelpButton`] owns the open
//! state and mounts the dialog beside the button (the dialog is a fixed overlay, so where it sits
//! in the page does not matter).

use camerata_ui_core::help::HelpTopic;
use dioxus::prelude::*;

/// The id of a topic's "?" button, for returning focus to it.
pub(crate) fn button_id(topic_id: &str) -> String {
    format!("help-btn-{}", safe(topic_id))
}

/// The id of a topic's dialog.
pub(crate) fn dialog_id(topic_id: &str) -> String {
    format!("help-dialog-{}", safe(topic_id))
}

/// The id of the dialog's backdrop, the click-outside area.
pub(crate) fn backdrop_id(topic_id: &str) -> String {
    format!("help-backdrop-{}", safe(topic_id))
}

/// The id of the dialog's Close button.
pub(crate) fn close_id(topic_id: &str) -> String {
    format!("help-close-{}", safe(topic_id))
}

/// The id of the dialog's title, which labels it.
pub(crate) fn title_id(topic_id: &str) -> String {
    format!("help-title-{}", safe(topic_id))
}

/// A topic id as an html id: letters, digits and dashes only.
fn safe(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The script that moves focus into the dialog once it is on screen.
pub(crate) fn focus_dialog_js(topic_id: &str) -> String {
    format!(
        "requestAnimationFrame(function() {{ var d = document.getElementById('{}'); \
         if (d) {{ d.focus(); }} }}); return true;",
        dialog_id(topic_id)
    )
}

/// The script that returns focus to the "?" button after the dialog closes.
pub(crate) fn focus_button_js(topic_id: &str) -> String {
    format!(
        "requestAnimationFrame(function() {{ var b = document.getElementById('{}'); \
         if (b) {{ b.focus(); }} }}); return true;",
        button_id(topic_id)
    )
}

/// The script that moves focus to the next (or previous) control inside the dialog, wrapping at
/// the ends, so Tab never leaves it.
pub(crate) fn tab_inside_js(topic_id: &str, backwards: bool) -> String {
    format!(
        "var d = document.getElementById('{id}'); if (d) {{ \
         var f = Array.prototype.slice.call(d.querySelectorAll(\
         'button, [href], input, select, textarea, [tabindex]:not([tabindex=\"-1\"])'))\
         .filter(function(e) {{ return !e.disabled; }}); \
         if (f.length === 0) {{ d.focus(); }} else {{ \
         var i = f.indexOf(document.activeElement); \
         var n = {back} ? (i <= 0 ? f.length - 1 : i - 1) : (i === f.length - 1 ? 0 : i + 1); \
         f[n].focus(); }} }} return true;",
        id = dialog_id(topic_id),
        back = backwards,
    )
}

/// The dialog, drawn from props. `on_close` is called for Esc, the Close button and a click
/// outside the panel.
#[component]
pub(crate) fn HelpDialog(topic: HelpTopic, on_close: EventHandler<()>) -> Element {
    let id = dialog_id(topic.id);
    let title = title_id(topic.id);
    let backdrop = backdrop_id(topic.id);
    let close = close_id(topic.id);
    let topic_id = topic.id;
    rsx! {
        div {
            id: "{backdrop}",
            class: "help-backdrop",
            onclick: move |_| on_close.call(()),
            div {
                id: "{id}",
                class: "help-dialog",
                role: "dialog",
                "aria-modal": "true",
                "aria-labelledby": "{title}",
                tabindex: "-1",
                onclick: move |e| e.stop_propagation(),
                onkeydown: move |e: KeyboardEvent| {
                    if e.key() == Key::Escape {
                        e.prevent_default();
                        on_close.call(());
                    } else if e.key() == Key::Tab {
                        e.prevent_default();
                        let _ = document::eval(&tab_inside_js(topic_id, e.modifiers().shift()));
                    }
                },
                h3 { id: "{title}", class: "help-title", "{topic.title}" }
                div { class: "help-body", tabindex: "0",
                    for (i, para) in topic.body.split("\n\n").enumerate() {
                        p { key: "{i}", "{para}" }
                    }
                }
                div { class: "help-actions",
                    button {
                        id: "{close}",
                        class: "btn-edit-sm help-close",
                        r#type: "button",
                        onclick: move |_| on_close.call(()),
                        "Close"
                    }
                }
            }
        }
    }
}

/// The "?" button beside a heading or control, and the dialog it opens.
#[component]
pub(crate) fn HelpButton(topic: HelpTopic) -> Element {
    let mut open = use_signal(|| false);
    let id = button_id(topic.id);
    let label = topic.button_label();
    let topic_id = topic.id;
    // Focus moves into the dialog once it is on screen.
    use_effect(move || {
        if open() {
            let _ = document::eval(&focus_dialog_js(topic_id));
        }
    });
    rsx! {
        button {
            id: "{id}",
            class: "help-button",
            r#type: "button",
            "aria-label": "{label}",
            "aria-haspopup": "dialog",
            title: "{label}",
            onclick: move |_| open.set(true),
            "?"
        }
        if open() {
            HelpDialog {
                topic: topic.clone(),
                on_close: move |()| {
                    open.set(false);
                    let _ = document::eval(&focus_button_js(topic_id));
                },
            }
        }
    }
}

#[cfg(test)]
mod tests;
