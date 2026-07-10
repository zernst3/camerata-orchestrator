use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct FieldProps {
    /// Visible label, also wired to the input via a generated `id` for a11y.
    pub label: String,
    pub value: String,
    #[props(default)]
    pub placeholder: String,
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
                r#type: "text",
                value: "{props.value}",
                placeholder: "{props.placeholder}",
                oninput: move |evt| props.oninput.call(evt.value()),
            }
        }
    }
}
