//! Screen 1 — INTAKE. The strict-but-open user-story form.
//!
//! Per CONSUMER_UX.md it is NOT a single prompt box. It is paged into calm
//! sections (one decision at a time): what it is, who uses it, what it tracks,
//! what you can do with each thing, and anything unusual. Pre-filled with the
//! Riverside Pottery seed so it reads as a finished product; a recorder can edit
//! or just press the single primary action, "Talk to the engineer".

use dioxus::prelude::*;

use crate::app_state::{AppState, EntityInput, FieldInput, IntakeInputs, RoleInput};
use crate::data;
use crate::Screen;

/// The form is sectioned, and a person moves through it one section at a time —
/// the "one decision per screen" principle applied within intake itself.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    About,
    People,
    Things,
    Constraints,
}

#[component]
pub fn IntakeScreen(screen: Signal<Screen>) -> Element {
    let mut section = use_signal(|| Section::About);

    // The shared live project. On submit we build the real typed IntakeForm +
    // Project from the inputs below and hand it to the rest of the flow.
    let mut app = use_context::<Signal<Option<AppState>>>();

    // Seeded, editable form state. Only the free-text fields are live-editable
    // in this pass; the structured entities/roles render from the seed (the
    // visual richness is the point — wiring edits is a later pass).
    let mut app_name = use_signal(|| data::SEED_APP_NAME.to_string());
    let mut description = use_signal(|| data::SEED_DESCRIPTION.to_string());
    let mut constraints = use_signal(|| data::SEED_CONSTRAINTS.to_string());

    let roles = use_signal(data::seed_roles);
    let entities = use_signal(data::seed_entities);

    let advance = move |_| {
        let next = match section() {
            Section::About => Some(Section::People),
            Section::People => Some(Section::Things),
            Section::Things => Some(Section::Constraints),
            Section::Constraints => None,
        };
        match next {
            Some(s) => section.set(s),
            None => {
                // Submit: turn the collected inputs into a real typed IntakeForm,
                // run the deterministic investigation, and open the project. This
                // also queues the onboarding document + seeded stories for
                // persistence (the App effect flushes them).
                let inputs = IntakeInputs {
                    app_name: app_name(),
                    description: description(),
                    constraints: constraints(),
                    roles: roles()
                        .into_iter()
                        .map(|r| RoleInput { name: r.name, actions: r.actions })
                        .collect(),
                    entities: entities()
                        .into_iter()
                        .map(|e| EntityInput {
                            name: e.name,
                            fields: e
                                .fields
                                .into_iter()
                                .map(|f| FieldInput { name: f.name, type_label: f.type_label })
                                .collect(),
                            features: e.features,
                        })
                        .collect(),
                    // The style picker UI section is a later pass; default for now.
                    style: Default::default(),
                };
                app.set(Some(AppState::from_intake("project_1", &inputs)));
                screen.set(Screen::Clarify);
            }
        }
    };

    let is_last = section() == Section::Constraints;
    let primary_label = if is_last { "Talk to the engineer" } else { "Next" };

    rsx! {
        div { class: "page",
            p { class: "eyebrow", "New app" }

            // Each section is its own calm view. The header reframes per section
            // so only one decision is ever on screen.
            match section() {
                Section::About => rsx! {
                    AboutSection {
                        app_name,
                        description,
                        on_name: move |v| app_name.set(v),
                        on_desc: move |v| description.set(v),
                    }
                },
                Section::People => rsx! { PeopleSection { roles } },
                Section::Things => rsx! { ThingsSection { entities } },
                Section::Constraints => rsx! {
                    ConstraintsSection {
                        constraints,
                        on_change: move |v| constraints.set(v),
                    }
                },
            }

            div { class: "actions",
                button { class: "btn-primary", onclick: advance, "{primary_label}" }
                if section() != Section::About {
                    button {
                        class: "btn-quiet",
                        onclick: move |_| {
                            let prev = match section() {
                                Section::About => Section::About,
                                Section::People => Section::About,
                                Section::Things => Section::People,
                                Section::Constraints => Section::Things,
                            };
                            section.set(prev);
                        },
                        "Back"
                    }
                }
            }
        }
    }
}

#[component]
fn AboutSection(
    app_name: Signal<String>,
    description: Signal<String>,
    on_name: EventHandler<String>,
    on_desc: EventHandler<String>,
) -> Element {
    rsx! {
        h1 { class: "h1", "What are we making?" }
        p { class: "lede", "Give it a name and tell me, in your own words, what it's for. Plain language is perfect — I'll ask about the details next." }

        div { class: "field",
            p { class: "section-label", "Name" }
            input {
                class: "input",
                value: "{app_name}",
                placeholder: "e.g. Riverside Pottery Studio",
                oninput: move |e| on_name.call(e.value()),
            }
        }
        div { class: "field",
            p { class: "section-label", "In a sentence or two" }
            textarea {
                class: "textarea",
                value: "{description}",
                rows: "4",
                oninput: move |e| on_desc.call(e.value()),
            }
        }
    }
}

#[component]
fn PeopleSection(roles: Signal<Vec<data::Role>>) -> Element {
    rsx! {
        h1 { class: "h1", "Who uses it?" }
        p { class: "lede", "List the kinds of people, and the main things each one needs to do. This is what keeps the whole app pointed at real use." }

        for role in roles() {
            div { class: "card",
                div { class: "entity-head",
                    span { class: "entity-name", "{role.name}" }
                    span { class: "entity-kicker", "wants to…" }
                }
                div { class: "chips",
                    for action in role.actions {
                        span { class: "chip tag", "{action}" }
                    }
                }
            }
        }
    }
}

#[component]
fn ThingsSection(entities: Signal<Vec<data::Entity>>) -> Element {
    rsx! {
        h1 { class: "h1", "What does it keep track of?" }
        p { class: "lede", "These are the things your app remembers, each with a few details. Pick the kind of detail in plain words — no database-speak." }

        for entity in entities() {
            div { class: "card",
                div { class: "entity-head",
                    span { class: "entity-name", "{entity.name}" }
                    span { class: "entity-kicker", "you can " {entity.features.join(" · ")} }
                }
                for field in entity.fields {
                    div { class: "field-row",
                        {
                            let glyph = data::FIELD_TYPES
                                .iter()
                                .find(|t| t.label == field.type_label)
                                .map(|t| t.glyph)
                                .unwrap_or("·");
                            rsx! { span { class: "type-glyph", "{glyph}" } }
                        }
                        span { class: "field-name", "{field.name}" }
                        span { class: "type-label", "{field.type_label}" }
                    }
                }
            }
        }
    }
}

#[component]
fn ConstraintsSection(
    constraints: Signal<String>,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        h1 { class: "h1", "Anything important or unusual?" }
        p { class: "lede", "Rules, must-haves, a look you're after. Anything you'd tell a designer over coffee. The engineer reads this carefully before we start." }

        div { class: "field",
            textarea {
                class: "textarea",
                value: "{constraints}",
                rows: "6",
                placeholder: "e.g. it should feel warm, not corporate…",
                oninput: move |e| on_change.call(e.value()),
            }
        }
    }
}
