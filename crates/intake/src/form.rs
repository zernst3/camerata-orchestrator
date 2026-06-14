//! The Product-Owner intake form schema — open-ended but strict.
//!
//! This module defines the structured-requirements artifact a Product Owner fills
//! out instead of writing a single prompt. The form is STRICT IN STRUCTURE (every
//! app must declare at least one role and at least one entity) and OPEN IN CONTENT
//! (any domain: budgeting, event RSVPs, reading lists, CRMs, whatever).
//!
//! The five-part spine (from [`IntakeForm`]) matches the CONSUMER_UX.md spec:
//!
//! 1. `app_name` + `description` — what is it?
//! 2. `roles` — who uses it, and what can each kind of person do?
//! 3. `entities` — what does it keep track of?
//! 4. Per-entity `capabilities` — what can a person do with each thing?
//! 5. `constraints` — anything important or unusual (free text)?
//!
//! The `clarifications` field accumulates Q&A rounds from the multi-turn clarify
//! loop ([`crate::clarify::ClarifyDriver`]). It is not part of the PO's initial
//! submission; the driver appends to it on each turn.
//!
//! ## Backward compatibility
//!
//! The `views` field is retained as a `#[serde(default)]` legacy field so that
//! any CLI or serialized payload that references it still compiles and deserializes
//! cleanly. New code should prefer the per-entity `capabilities` on each
//! [`EntityDefinition`].

use serde::{Deserialize, Serialize};

// ─── FieldType (consumer-friendly, not SQL) ──────────────────────────────────

/// The consumer-visible type of an entity field.
///
/// These are the types a non-technical Product Owner sees in the form: plain
/// English descriptions, not SQL or Rust types. The lead engineer and code
/// generator translate them to concrete language types.
///
/// `LinkTo` and `Choice` carry content; all others are unit variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// A short piece of free text (a name, a label, a short note).
    Text,
    /// A longer block of free text (a bio, a description, a comment).
    LongText,
    /// A monetary amount or decimal number (e.g. a price, a score).
    Money,
    /// A plain whole number (e.g. a count, a rank).
    Number,
    /// A yes/no flag (true or false).
    YesNo,
    /// A calendar date (no time component).
    Date,
    /// A date and time.
    DateTime,
    /// An email address.
    Email,
    /// A URL / web link.
    Url,
    /// A link to another entity (a foreign-key relation). The string is the
    /// target entity name (must match an [`EntityDefinition::name`] in the form).
    LinkTo(String),
    /// A choice from a fixed list of options (an enum / select). The vec is
    /// the list of allowed option strings.
    Choice(Vec<String>),

    // ── aliases kept for back-compat / test code that uses the old names ──────
    //
    // These map to the canonical variants above in `label()` and in tests.
    // `serde(alias)` is not available on variants; callers that previously used
    // `Integer` or `Decimal` should migrate to `Number` / `Money`.
    /// Alias for [`FieldType::Number`]. Prefer `Number` in new code.
    Integer,
    /// Alias for [`FieldType::Money`]. Prefer `Money` in new code.
    Decimal,
    /// Alias for [`FieldType::YesNo`]. Prefer `YesNo` in new code.
    Boolean,
}

impl FieldType {
    /// A human-readable label for the field type, shown in the lead-engineer
    /// brief and in demo summaries.
    pub fn label(&self) -> &str {
        match self {
            FieldType::Text => "text",
            FieldType::LongText => "long text",
            FieldType::Money => "money/decimal",
            FieldType::Number => "number",
            FieldType::YesNo => "yes/no",
            FieldType::Date => "date",
            FieldType::DateTime => "date+time",
            FieldType::Email => "email",
            FieldType::Url => "url",
            FieldType::LinkTo(entity) => entity.as_str(),
            FieldType::Choice(_) => "choice",
            FieldType::Integer => "number",
            FieldType::Decimal => "money/decimal",
            FieldType::Boolean => "yes/no",
        }
    }
}

// ─── Legacy FieldKind (serde alias for backward-compat) ──────────────────────

/// Legacy field-kind enum kept for backward-compatible deserialization of
/// serialized `IntakeForm` payloads that used the old `FieldKind` name. New code
/// should use [`FieldType`].
///
/// `FieldKind` and `FieldType` serialize to the same JSON snake_case strings for
/// the overlapping variants (`text`, `integer`, `decimal`, `date`, `boolean`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Free text.
    Text,
    /// A whole number.
    Integer,
    /// A decimal / monetary amount.
    Decimal,
    /// A calendar date.
    Date,
    /// A true/false flag.
    Boolean,
}

impl FieldKind {
    /// Human label (used in the lead-engineer prompt).
    pub fn label(&self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Integer => "integer",
            FieldKind::Decimal => "decimal",
            FieldKind::Date => "date",
            FieldKind::Boolean => "boolean",
        }
    }
}

// ─── EntityField ─────────────────────────────────────────────────────────────

/// One field on an [`EntityDefinition`].
///
/// Uses the consumer-friendly [`FieldType`] rather than SQL / Rust types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityField {
    /// The field's name (snake_case, e.g. `amount`, `event_date`).
    pub name: String,
    /// The consumer-visible type of this field.
    pub field_type: FieldType,
    /// Whether the field is required (non-null / must be filled in).
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

impl EntityField {
    /// Construct a required field.
    pub fn required(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            required: true,
        }
    }

    /// Construct an optional field.
    pub fn optional(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            required: false,
        }
    }
}

// ─── Legacy Field (backward compat for clarify.rs tests) ─────────────────────

/// Legacy field type kept so existing code that constructs `Field` directly
/// still compiles. New code should use [`EntityField`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// The field's name.
    pub name: String,
    /// The field's scalar kind (legacy).
    pub kind: FieldKind,
    /// Whether the field is required.
    pub required: bool,
}

impl Field {
    /// Construct a required field.
    pub fn required(name: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            name: name.into(),
            kind,
            required: true,
        }
    }

    /// Construct an optional field.
    pub fn optional(name: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            name: name.into(),
            kind,
            required: false,
        }
    }
}

// ─── EntityCapabilities (per-entity CRUD features) ───────────────────────────

/// The CRUD-ish features the Product Owner wants for one entity, expressed in
/// consumer words. Every field defaults to `false`; the PO checks the ones they
/// want.
///
/// Consumer word → engineering meaning:
/// - `can_add`    → create form
/// - `can_list`   → list / table view
/// - `can_view`   → single-record detail view
/// - `can_edit`   → edit form
/// - `can_remove` → delete / archive
/// - `can_search` → search / filter the list
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EntityCapabilities {
    /// "Add a new one" — create form.
    #[serde(default)]
    pub can_add: bool,
    /// "See a list of them" — list / table view.
    #[serde(default)]
    pub can_list: bool,
    /// "See the details of one" — detail view.
    #[serde(default)]
    pub can_view: bool,
    /// "Change an existing one" — edit form.
    #[serde(default)]
    pub can_edit: bool,
    /// "Remove one" — delete or archive.
    #[serde(default)]
    pub can_remove: bool,
    /// "Find / filter them" — search bar or filter controls.
    #[serde(default)]
    pub can_search: bool,
}

impl EntityCapabilities {
    /// Full CRUD: add + list + view + edit + remove + search.
    pub fn full() -> Self {
        Self {
            can_add: true,
            can_list: true,
            can_view: true,
            can_edit: true,
            can_remove: true,
            can_search: true,
        }
    }

    /// A read-only browsing set: list + view + search (no mutations).
    pub fn read_only() -> Self {
        Self {
            can_add: false,
            can_list: true,
            can_view: true,
            can_edit: false,
            can_remove: false,
            can_search: true,
        }
    }

    /// Render as a compact comma-separated capability list for the brief.
    pub fn label(&self) -> String {
        let mut caps = Vec::new();
        if self.can_add {
            caps.push("add");
        }
        if self.can_list {
            caps.push("list");
        }
        if self.can_view {
            caps.push("view");
        }
        if self.can_edit {
            caps.push("edit");
        }
        if self.can_remove {
            caps.push("remove");
        }
        if self.can_search {
            caps.push("search");
        }
        if caps.is_empty() {
            "(none)".to_string()
        } else {
            caps.join(", ")
        }
    }
}

// ─── EntityDefinition ────────────────────────────────────────────────────────

/// A domain entity the app stores (a "thing it keeps track of").
///
/// The Product Owner names it, describes it in plain language, lists its fields,
/// and says what operations are available on it. The lead engineer decides HOW to
/// store and render it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityDefinition {
    /// The entity name (PascalCase singular, e.g. `Expense`, `Event`, `Book`).
    pub name: String,
    /// A plain-language description of what this entity represents.
    #[serde(default)]
    pub description: String,
    /// The entity's fields. The `id` primary key is implicit; do not list it.
    pub fields: Vec<EntityField>,
    /// The CRUD-ish capabilities the PO wants for this entity.
    #[serde(default)]
    pub capabilities: EntityCapabilities,
}

// ─── Legacy Entity (backward compat for clarify.rs tests) ────────────────────

/// Legacy entity type kept so existing test code that constructs `Entity`
/// directly still compiles. New code should use [`EntityDefinition`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    /// The entity name.
    pub name: String,
    /// The entity's fields (legacy [`Field`] type).
    pub fields: Vec<Field>,
}

// ─── UserRole ─────────────────────────────────────────────────────────────────

/// A kind of person who uses the app, with the top-level actions they take.
///
/// This is the user-story forcing function: "As a [role], I want to [action]."
/// Every app must declare at least one role.
///
/// `actions` should be verb phrases: "add an expense", "view my reading list",
/// "invite a guest", "approve a request".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRole {
    /// The role's name (e.g. `Owner`, `Member`, `Guest`, `Admin`).
    pub name: String,
    /// The top-level actions this role can take (verb phrases).
    pub actions: Vec<String>,
}

// ─── ViewKind / ViewSpec (backward-compat stubs) ─────────────────────────────

/// Legacy view-kind enum, retained for backward-compatible deserialization and
/// for CLI code that references `form.views`. New capability modeling uses
/// [`EntityCapabilities`] on each [`EntityDefinition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    /// A list/table view.
    List,
    /// A single-record detail view.
    Detail,
    /// A create/edit form.
    Form,
}

impl ViewKind {
    /// Human label.
    pub fn label(&self) -> &'static str {
        match self {
            ViewKind::List => "list",
            ViewKind::Detail => "detail",
            ViewKind::Form => "form",
        }
    }
}

/// Legacy view spec, retained so `form.views` compiles in CLI code that has not
/// yet migrated to [`EntityCapabilities`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSpec {
    /// The entity this view is over.
    pub entity: String,
    /// The kind of view.
    pub kind: ViewKind,
}

impl ViewSpec {
    /// Construct a view spec.
    pub fn new(entity: impl Into<String>, kind: ViewKind) -> Self {
        Self {
            entity: entity.into(),
            kind,
        }
    }
}

// ─── ClarificationRound ──────────────────────────────────────────────────────

/// One round of clarification from the multi-turn clarify loop.
///
/// Stored in [`IntakeForm::clarifications`] so subsequent `evaluate` calls see
/// the full Q&A history via [`IntakeForm::brief`]. Defined here (in `form`)
/// rather than `clarify` because it is part of the form's persistent state, not
/// the driver's logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationRound {
    /// The questions the lead engineer asked in this round.
    pub questions: Vec<String>,
    /// The PO's answers, indexed parallel to `questions`.
    pub answers: Vec<String>,
}

impl ClarificationRound {
    /// Render this round as a human/LLM-readable Q&A block for inclusion in a
    /// form brief.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, q) in self.questions.iter().enumerate() {
            let answer = self
                .answers
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or("(no answer)");
            out.push_str(&format!("  Q: {q}\n  A: {answer}\n"));
        }
        out
    }
}

// ─── IntakeForm ───────────────────────────────────────────────────────────────

/// The complete Product-Owner intake form for one bespoke app.
///
/// The spine is strict (every app must have at least one role and at least one
/// entity) and open (any domain). The five sections match CONSUMER_UX.md §1:
///
/// 1. `app_name` + `description` — what is it?
/// 2. `roles` — who uses it, and what can each kind of person do?
/// 3. `entities` — what things does it keep track of, with their fields and
///    per-entity CRUD capabilities?
/// 4. `constraints` — anything important or unusual (free text)?
///
/// The `clarifications` field is managed by [`crate::clarify::ClarifyDriver`];
/// callers constructing a fresh form should leave it as `vec![]`.
///
/// The `views` field is a **legacy backward-compat field** for code that
/// accesses `form.views` (e.g. the `po-demo` CLI summary line). It defaults to
/// an empty `Vec` and is not rendered in `brief()`. Prefer
/// [`EntityDefinition::capabilities`] for new feature modeling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeForm {
    /// The app's name (kebab/snake friendly, e.g. `budget-tracker`).
    pub app_name: String,
    /// One plain-language paragraph: what the app is for, in the PO's words.
    pub description: String,
    /// The kinds of users and their top actions (the user-story forcing function).
    /// Every app must declare at least one role.
    #[serde(default)]
    pub roles: Vec<UserRole>,
    /// The domain entities the app tracks. Every app must declare at least one.
    pub entities: Vec<EntityDefinition>,
    /// Anything important, unusual, or constraining: must-haves, rules,
    /// look-and-feel wishes (free text; optional).
    #[serde(default)]
    pub constraints: String,
    /// Accumulated Q&A rounds from the multi-turn clarify loop. Empty on initial
    /// submission; each clarify turn appends one round.
    #[serde(default)]
    pub clarifications: Vec<ClarificationRound>,
    /// Legacy backward-compat field. New code should use per-entity
    /// `capabilities` instead. Retained so CLI code that calls `form.views.len()`
    /// still compiles.
    #[serde(default)]
    pub views: Vec<ViewSpec>,
}

impl IntakeForm {
    /// The canonical GENERIC sample form used for demos and integration tests:
    /// a simple personal expense tracker that demonstrates open-ended content
    /// within the strict form structure.
    ///
    /// This replaces the old `sample_budgeting_app()` with a more complete
    /// open-ended example.
    pub fn sample_app() -> Self {
        Self {
            app_name: "expense-tracker".to_string(),
            description: "A personal app to record everyday expenses, see them \
                          in a list grouped by category, and understand where \
                          money goes each month."
                .to_string(),
            roles: vec![UserRole {
                name: "Owner".to_string(),
                actions: vec![
                    "add an expense with an amount, category, and date".to_string(),
                    "view a list of all expenses sorted by date".to_string(),
                    "edit or remove an expense I entered by mistake".to_string(),
                    "filter expenses by category or date range".to_string(),
                ],
            }],
            entities: vec![EntityDefinition {
                name: "Expense".to_string(),
                description: "A single spending record: how much, what for, and when.".to_string(),
                fields: vec![
                    EntityField::required("amount", FieldType::Money),
                    EntityField::required("category", FieldType::Choice(vec![
                        "Food".to_string(),
                        "Transport".to_string(),
                        "Housing".to_string(),
                        "Entertainment".to_string(),
                        "Other".to_string(),
                    ])),
                    EntityField::required("spent_on", FieldType::Date),
                    EntityField::optional("note", FieldType::Text),
                ],
                capabilities: EntityCapabilities {
                    can_add: true,
                    can_list: true,
                    can_view: false,
                    can_edit: true,
                    can_remove: true,
                    can_search: true,
                },
            }],
            constraints: "Single-user only for v1. No budget limits or alerts needed. \
                         Keep it simple; no import/export."
                .to_string(),
            clarifications: vec![],
            views: vec![ViewSpec::new("Expense", ViewKind::List)],
        }
    }

    /// A deliberately underspecified form for demonstrating the clarify loop:
    /// minimal description, single entity with one field, no roles declared,
    /// no constraints — all the gaps a discerning lead engineer would probe.
    ///
    /// Used by `po-demo` to exercise the multi-turn clarify pipeline.
    pub fn sample_underspecified_app() -> Self {
        Self {
            app_name: "expense-tracker".to_string(),
            description: "track my money".to_string(),
            roles: vec![],
            entities: vec![EntityDefinition {
                name: "Expense".to_string(),
                description: String::new(),
                fields: vec![EntityField::required("amount", FieldType::Money)],
                capabilities: EntityCapabilities {
                    can_add: true,
                    can_list: true,
                    ..Default::default()
                },
            }],
            constraints: String::new(),
            clarifications: vec![],
            views: vec![ViewSpec::new("Expense", ViewKind::List)],
        }
    }

    /// An event RSVP app — a second open-ended example showing the form works
    /// for non-budgeting domains.
    pub fn sample_event_rsvp_app() -> Self {
        Self {
            app_name: "event-rsvp".to_string(),
            description: "A simple app for organising events and tracking who is \
                          coming. An organiser creates events; guests submit an \
                          RSVP; the organiser sees the headcount."
                .to_string(),
            roles: vec![
                UserRole {
                    name: "Organiser".to_string(),
                    actions: vec![
                        "create a new event with a name, date, and location".to_string(),
                        "view the list of RSVPs for an event".to_string(),
                        "remove an event that was cancelled".to_string(),
                    ],
                },
                UserRole {
                    name: "Guest".to_string(),
                    actions: vec![
                        "submit an RSVP with my name and whether I am attending".to_string(),
                        "update my RSVP if my plans change".to_string(),
                    ],
                },
            ],
            entities: vec![
                EntityDefinition {
                    name: "Event".to_string(),
                    description: "A gathering with a name, a date, and a location.".to_string(),
                    fields: vec![
                        EntityField::required("title", FieldType::Text),
                        EntityField::required("event_date", FieldType::DateTime),
                        EntityField::required("location", FieldType::Text),
                        EntityField::optional("description", FieldType::LongText),
                    ],
                    capabilities: EntityCapabilities {
                        can_add: true,
                        can_list: true,
                        can_view: true,
                        can_edit: true,
                        can_remove: true,
                        can_search: false,
                    },
                },
                EntityDefinition {
                    name: "Rsvp".to_string(),
                    description: "A guest's response to an event invitation.".to_string(),
                    fields: vec![
                        EntityField::required("guest_name", FieldType::Text),
                        EntityField::required("email", FieldType::Email),
                        EntityField::required("attending", FieldType::YesNo),
                        EntityField::required("event", FieldType::LinkTo("Event".to_string())),
                        EntityField::optional("note", FieldType::LongText),
                    ],
                    capabilities: EntityCapabilities {
                        can_add: true,
                        can_list: true,
                        can_view: false,
                        can_edit: true,
                        can_remove: false,
                        can_search: false,
                    },
                },
            ],
            constraints: "No login system needed for v1; anyone with the link can \
                         RSVP. The organiser sees a simple headcount (attending / \
                         not attending)."
                .to_string(),
            clarifications: vec![],
            views: vec![
                ViewSpec::new("Event", ViewKind::List),
                ViewSpec::new("Rsvp", ViewKind::List),
            ],
        }
    }

    /// A reading-list app — a third open-ended example (books, articles, or
    /// anything you want to track reading progress on).
    pub fn sample_reading_list_app() -> Self {
        Self {
            app_name: "reading-list".to_string(),
            description: "A personal tracker for books and articles I want to \
                          read or have already read, with notes and a read/unread \
                          status."
                .to_string(),
            roles: vec![UserRole {
                name: "Reader".to_string(),
                actions: vec![
                    "add a book or article to my list".to_string(),
                    "mark an item as read".to_string(),
                    "add a personal note or rating to something I have read".to_string(),
                    "see a list of everything I want to read".to_string(),
                    "filter by status (to read / finished)".to_string(),
                ],
            }],
            entities: vec![EntityDefinition {
                name: "ReadingItem".to_string(),
                description: "A book, article, or other piece of content to track.".to_string(),
                fields: vec![
                    EntityField::required("title", FieldType::Text),
                    EntityField::optional("author", FieldType::Text),
                    EntityField::required("status", FieldType::Choice(vec![
                        "To Read".to_string(),
                        "Reading".to_string(),
                        "Finished".to_string(),
                        "Abandoned".to_string(),
                    ])),
                    EntityField::optional("url", FieldType::Url),
                    EntityField::optional("finished_on", FieldType::Date),
                    EntityField::optional("rating", FieldType::Number),
                    EntityField::optional("notes", FieldType::LongText),
                ],
                capabilities: EntityCapabilities::full(),
            }],
            constraints: "Single user only. No social / sharing features.".to_string(),
            clarifications: vec![],
            views: vec![ViewSpec::new("ReadingItem", ViewKind::List)],
        }
    }

    /// Render the form as a compact, deterministic human/LLM-readable brief.
    ///
    /// This is what the lead engineer is handed; keeping it as one method makes
    /// the real call and the stub see EXACTLY the same input.
    ///
    /// Sections rendered:
    /// 1. App name + description
    /// 2. Roles (with actions) — the user-story forcing function
    /// 3. Entities (with fields and capabilities)
    /// 4. Constraints (if non-empty)
    /// 5. Clarifications (prior Q&A rounds, if any)
    pub fn brief(&self) -> String {
        let mut out = String::new();

        // ── 1. What is it? ──────────────────────────────────────────────────
        out.push_str(&format!("App: {}\n", self.app_name));
        out.push_str(&format!("Description: {}\n", self.description));

        // ── 2. Roles (user-story forcing function) ──────────────────────────
        if !self.roles.is_empty() {
            out.push_str("Roles:\n");
            for role in &self.roles {
                out.push_str(&format!("  - {} can:\n", role.name));
                for action in &role.actions {
                    out.push_str(&format!("      * {action}\n"));
                }
            }
        }

        // ── 3. Entities (with fields + capabilities) ────────────────────────
        out.push_str("Entities:\n");
        for entity in &self.entities {
            out.push_str(&format!("  - {}", entity.name));
            if !entity.description.is_empty() {
                out.push_str(&format!(" — {}", entity.description));
            }
            out.push('\n');
            out.push_str(&format!(
                "      capabilities: {}\n",
                entity.capabilities.label()
            ));
            for field in &entity.fields {
                out.push_str(&format!(
                    "      {} : {}{}\n",
                    field.name,
                    field.field_type.label(),
                    if field.required { " (required)" } else { "" },
                ));
            }
        }

        // ── 4. Constraints ───────────────────────────────────────────────────
        if !self.constraints.is_empty() {
            out.push_str(&format!("Constraints: {}\n", self.constraints));
        }

        // ── 5. Clarifications ────────────────────────────────────────────────
        if !self.clarifications.is_empty() {
            out.push_str("Clarifications (prior Q&A rounds):\n");
            for (i, round) in self.clarifications.iter().enumerate() {
                out.push_str(&format!("  Round {}:\n", i + 1));
                out.push_str(&round.render());
            }
        }

        out
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── sample_app ────────────────────────────────────────────────────────────

    #[test]
    fn sample_app_has_required_spine() {
        let form = IntakeForm::sample_app();
        assert_eq!(form.app_name, "expense-tracker");
        assert!(!form.description.is_empty(), "description must be non-empty");
        assert!(!form.roles.is_empty(), "at least one role is required");
        assert!(!form.entities.is_empty(), "at least one entity is required");
    }

    #[test]
    fn sample_app_expense_entity_has_money_field() {
        let form = IntakeForm::sample_app();
        let expense = form
            .entities
            .iter()
            .find(|e| e.name == "Expense")
            .expect("Expense entity missing");
        assert!(
            expense.fields.iter().any(|f| f.name == "amount" && f.field_type == FieldType::Money && f.required),
            "Expense must have a required `amount` Money field"
        );
    }

    #[test]
    fn sample_app_entity_has_capabilities() {
        let form = IntakeForm::sample_app();
        let expense = &form.entities[0];
        assert!(expense.capabilities.can_add);
        assert!(expense.capabilities.can_list);
    }

    #[test]
    fn sample_app_owner_role_has_actions() {
        let form = IntakeForm::sample_app();
        let owner = form.roles.iter().find(|r| r.name == "Owner").expect("Owner role missing");
        assert!(!owner.actions.is_empty(), "Owner must have at least one action");
    }

    // ── sample_underspecified_app ─────────────────────────────────────────────

    #[test]
    fn sample_underspecified_app_is_minimal() {
        let form = IntakeForm::sample_underspecified_app();
        assert_eq!(form.app_name, "expense-tracker");
        assert_eq!(form.entities.len(), 1);
        let entity = &form.entities[0];
        assert_eq!(entity.fields.len(), 1);
        assert_eq!(entity.fields[0].name, "amount");
        // views kept for CLI backward compat
        assert_eq!(form.views.len(), 1);
    }

    // ── event RSVP + reading list samples ───────────────────────────────────

    #[test]
    fn sample_event_rsvp_has_two_entities_and_two_roles() {
        let form = IntakeForm::sample_event_rsvp_app();
        assert_eq!(form.entities.len(), 2);
        assert_eq!(form.roles.len(), 2);
        assert!(form.entities.iter().any(|e| e.name == "Event"));
        assert!(form.entities.iter().any(|e| e.name == "Rsvp"));
    }

    #[test]
    fn sample_reading_list_has_choice_and_url_fields() {
        let form = IntakeForm::sample_reading_list_app();
        let item = &form.entities[0];
        assert!(item.fields.iter().any(|f| matches!(f.field_type, FieldType::Choice(_))));
        assert!(item.fields.iter().any(|f| f.field_type == FieldType::Url));
    }

    #[test]
    fn reading_list_entity_has_full_capabilities() {
        let form = IntakeForm::sample_reading_list_app();
        let caps = &form.entities[0].capabilities;
        assert!(caps.can_add && caps.can_list && caps.can_view && caps.can_edit && caps.can_remove && caps.can_search);
    }

    // ── brief() ──────────────────────────────────────────────────────────────

    #[test]
    fn brief_contains_app_name_description_roles_and_entities() {
        let brief = IntakeForm::sample_app().brief();
        assert!(brief.contains("expense-tracker"), "brief must contain app name");
        assert!(brief.contains("Expense"), "brief must mention the entity");
        assert!(brief.contains("Owner"), "brief must mention the role");
        assert!(brief.contains("amount"), "brief must mention the field");
        assert!(brief.contains("money/decimal"), "brief must show field type label");
    }

    #[test]
    fn brief_shows_capabilities() {
        let brief = IntakeForm::sample_app().brief();
        assert!(
            brief.contains("capabilities:"),
            "brief must render per-entity capabilities"
        );
        assert!(brief.contains("add"), "capabilities must include 'add'");
        assert!(brief.contains("list"), "capabilities must include 'list'");
    }

    #[test]
    fn brief_shows_constraints_when_present() {
        let brief = IntakeForm::sample_app().brief();
        assert!(brief.contains("Constraints:"), "brief must render the constraints section");
        assert!(brief.contains("Single-user"), "brief must include the constraints text");
    }

    #[test]
    fn brief_omits_constraints_section_when_empty() {
        let form = IntakeForm::sample_underspecified_app();
        let brief = form.brief();
        assert!(
            !brief.contains("Constraints:"),
            "empty constraints should not produce a Constraints: section"
        );
    }

    #[test]
    fn brief_includes_clarification_rounds() {
        let mut form = IntakeForm::sample_underspecified_app();
        form.clarifications.push(ClarificationRound {
            questions: vec!["Which currency?".to_string()],
            answers: vec!["USD".to_string()],
        });
        let brief = form.brief();
        assert!(brief.contains("Clarifications"), "brief must include Clarifications section");
        assert!(brief.contains("Which currency?"));
        assert!(brief.contains("USD"));
    }

    // ── JSON round-trip ───────────────────────────────────────────────────────

    #[test]
    fn sample_app_roundtrips_through_json() {
        let form = IntakeForm::sample_app();
        let json = serde_json::to_string(&form).unwrap();
        let back: IntakeForm = serde_json::from_str(&json).unwrap();
        assert_eq!(form, back);
    }

    #[test]
    fn event_rsvp_roundtrips_through_json() {
        let form = IntakeForm::sample_event_rsvp_app();
        let json = serde_json::to_string(&form).unwrap();
        let back: IntakeForm = serde_json::from_str(&json).unwrap();
        assert_eq!(form, back);
    }

    #[test]
    fn link_to_and_choice_field_types_roundtrip() {
        let field_link = EntityField::required("event", FieldType::LinkTo("Event".to_string()));
        let field_choice = EntityField::required(
            "status",
            FieldType::Choice(vec!["A".to_string(), "B".to_string()]),
        );
        let json_link = serde_json::to_string(&field_link).unwrap();
        let json_choice = serde_json::to_string(&field_choice).unwrap();
        let back_link: EntityField = serde_json::from_str(&json_link).unwrap();
        let back_choice: EntityField = serde_json::from_str(&json_choice).unwrap();
        assert_eq!(field_link, back_link);
        assert_eq!(field_choice, back_choice);
    }

    // ── EntityCapabilities ────────────────────────────────────────────────────

    #[test]
    fn full_capabilities_label_contains_all_six_words() {
        let label = EntityCapabilities::full().label();
        for word in &["add", "list", "view", "edit", "remove", "search"] {
            assert!(label.contains(word), "full() label must contain '{word}'");
        }
    }

    #[test]
    fn empty_capabilities_label_is_none() {
        let caps = EntityCapabilities::default();
        assert_eq!(caps.label(), "(none)");
    }

    // ── FieldType labels ──────────────────────────────────────────────────────

    #[test]
    fn field_type_labels_are_human_readable() {
        assert_eq!(FieldType::Text.label(), "text");
        assert_eq!(FieldType::LongText.label(), "long text");
        assert_eq!(FieldType::Money.label(), "money/decimal");
        assert_eq!(FieldType::Number.label(), "number");
        assert_eq!(FieldType::YesNo.label(), "yes/no");
        assert_eq!(FieldType::Date.label(), "date");
        assert_eq!(FieldType::DateTime.label(), "date+time");
        assert_eq!(FieldType::Email.label(), "email");
        assert_eq!(FieldType::Url.label(), "url");
        assert_eq!(FieldType::Choice(vec![]).label(), "choice");
        assert_eq!(FieldType::LinkTo("Book".to_string()).label(), "Book");
    }

    // ── backward-compat: Field + FieldKind still usable ──────────────────────

    #[test]
    fn legacy_field_kind_labels_unchanged() {
        assert_eq!(FieldKind::Text.label(), "text");
        assert_eq!(FieldKind::Integer.label(), "integer");
        assert_eq!(FieldKind::Decimal.label(), "decimal");
        assert_eq!(FieldKind::Date.label(), "date");
        assert_eq!(FieldKind::Boolean.label(), "boolean");
    }

    #[test]
    fn legacy_field_constructors_still_work() {
        let f = Field::required("amount", FieldKind::Decimal);
        assert_eq!(f.name, "amount");
        assert!(f.required);
        let g = Field::optional("note", FieldKind::Text);
        assert!(!g.required);
    }
}
