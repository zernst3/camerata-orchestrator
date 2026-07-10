use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct FieldProps {
    /// Visible label, also wired to the input via a generated `id` for a11y.
    pub label: String,
    pub value: String,
    #[props(default)]
    pub placeholder: String,
    /// When `true`, renders as `type="password"` (masked input) instead of plain
    /// text. Defaults to `false` — existing callers are unaffected. Added for the
    /// default-private access-lock's passcode prompt (`access_gate.rs`), which is
    /// the one place in the skeleton a `Field` value is a secret being typed.
    #[props(default)]
    pub password: bool,
    pub oninput: EventHandler<String>,
}

/// The one canonical labeled-text-input element (RUST-DIOXUS-14). Wraps a native
/// `<label>` + `<input>` pair with the design system's focus-ring and spacing.
#[component]
pub fn Field(props: FieldProps) -> Element {
    // A stable-enough id derived from the label; fine for a single-instance-per-page
    // field. A form with many dynamically-generated fields would want a real
    // generated-id scheme instead.
    let input_id = format!(
        "field-{}",
        props.label.to_lowercase().replace(' ', "-")
    );

    rsx! {
        div { class: "field",
            label { r#for: "{input_id}", class: "field__label", "{props.label}" }
            input {
                id: "{input_id}",
                class: "field__input",
                r#type: if props.password { "password" } else { "text" },
                value: "{props.value}",
                placeholder: "{props.placeholder}",
                oninput: move |evt| props.oninput.call(evt.value()),
            }
        }
    }
}
