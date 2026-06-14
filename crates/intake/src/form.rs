//! The Product-Owner intake form schema.
//!
//! This is the structured-requirements artifact a Product Owner fills out
//! instead of writing a single prompt. It is deliberately story-level and
//! CRUD-shaped (entities, their fields, and the views over them), matching the
//! V1 PO-mode scope: bespoke CRUD apps (frontend + backend + database). It is
//! the INPUT to a [`crate::engine::LeadEngineer`]; it carries no plan and makes
//! no build decisions.

use serde::{Deserialize, Serialize};

/// The scalar kind of an entity field. Kept small and CRUD-shaped on purpose:
/// these are the field types a self-serve budgeting-class app needs. Richer
/// codegen (relations, enums, money types) is documented remaining work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Free text (a name, a note).
    Text,
    /// A whole number.
    Integer,
    /// A decimal / monetary amount (rendered as `f64` in V1 codegen).
    Decimal,
    /// A calendar date.
    Date,
    /// A true/false flag.
    Boolean,
}

impl FieldKind {
    /// A human label for the field kind (used in the lead-engineer prompt and
    /// the demo summary).
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

/// One field on an [`Entity`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// The field's name (snake_case, e.g. `amount`).
    pub name: String,
    /// The field's scalar kind.
    pub kind: FieldKind,
    /// Whether the field is required (non-null).
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

/// A domain entity the app stores (a row type / table). The Product Owner
/// describes WHAT the app tracks; the lead engineer decides HOW.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    /// The entity name (PascalCase singular, e.g. `Expense`).
    pub name: String,
    /// The entity's fields (the `id` primary key is implicit, not listed here).
    pub fields: Vec<Field>,
}

/// The kind of a UI view the Product Owner wants over an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    /// A list/table view of all rows of an entity.
    List,
    /// A single-record detail view.
    Detail,
    /// A create/edit form for an entity.
    Form,
}

impl ViewKind {
    /// A human label for the view kind.
    pub fn label(&self) -> &'static str {
        match self {
            ViewKind::List => "list",
            ViewKind::Detail => "detail",
            ViewKind::Form => "form",
        }
    }
}

/// A view the app should present over one of its entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSpec {
    /// The entity this view is over (must match an [`Entity::name`]).
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

/// One round of clarification from the multi-turn clarify loop: the lead
/// engineer's questions and the Product Owner's answers for that round.
///
/// Stored in [`IntakeForm::clarifications`] so subsequent `evaluate` calls
/// see the full Q&A history via [`IntakeForm::brief`]. Defined here (in
/// `form`) rather than `clarify` because it is part of the form's persistent
/// state, not the driver's logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationRound {
    /// The questions the lead engineer asked in this round.
    pub questions: Vec<String>,
    /// The PO's answers, indexed parallel to `questions`.
    pub answers: Vec<String>,
}

impl ClarificationRound {
    /// Render this round as a human/LLM-readable Q&A block for inclusion in
    /// a form brief.
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

/// The complete Product-Owner intake form for one bespoke app.
///
/// This is the whole structured-requirements payload a PO submits. It is
/// story-level (what the app is, what it tracks, what screens it has) and
/// carries no engineering decisions — those are the lead engineer's job.
///
/// The `clarifications` field accumulates Q&A rounds produced by the
/// multi-turn clarify loop ([`crate::clarify::ClarifyDriver`]). It starts
/// empty; each clarification turn appends one [`ClarificationRound`]. The
/// lead engineer sees the full history on every subsequent `evaluate` call
/// via [`IntakeForm::brief`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeForm {
    /// The app's name (kebab/snake friendly, e.g. `budget-tracker`).
    pub app_name: String,
    /// One or two sentences: what the app is for, in the PO's own words.
    pub description: String,
    /// The entities the app tracks.
    pub entities: Vec<Entity>,
    /// The views (screens) the app should present.
    pub views: Vec<ViewSpec>,
    /// Accumulated Q&A rounds from the multi-turn clarify loop. Empty on
    /// initial submission; each clarify turn appends one round. The clarify
    /// driver manages this field; callers constructing a fresh form should
    /// leave it as `vec![]`.
    #[serde(default)]
    pub clarifications: Vec<ClarificationRound>,
}

impl IntakeForm {
    /// The canonical SAMPLE form `po-demo` uses: a tiny budgeting app with a
    /// single `Expense` entity (an amount, a category, a date) and a list view.
    ///
    /// This is the PO-mode equivalent of a hand-filled form; it is deliberately
    /// small so the end-to-end pipeline (evaluate → plan → governed build →
    /// cargo) is exercisable in one demo run.
    pub fn sample_budgeting_app() -> Self {
        Self {
            app_name: "budget-tracker".to_string(),
            description: "A tiny personal budgeting app to record expenses and \
                          see them in a list."
                .to_string(),
            entities: vec![Entity {
                name: "Expense".to_string(),
                fields: vec![
                    Field::required("amount", FieldKind::Decimal),
                    Field::required("category", FieldKind::Text),
                    Field::required("spent_on", FieldKind::Date),
                    Field::optional("note", FieldKind::Text),
                ],
            }],
            views: vec![ViewSpec::new("Expense", ViewKind::List)],
            clarifications: vec![],
        }
    }

    /// A minimal, deliberately underspecified form for demonstrating the
    /// clarify loop: one entity with only an `amount` field and no description
    /// of business rules, so a discerning lead engineer would ask questions.
    pub fn sample_underspecified_app() -> Self {
        Self {
            app_name: "budget-tracker".to_string(),
            description: "track my money".to_string(),
            entities: vec![Entity {
                name: "Expense".to_string(),
                fields: vec![Field::required("amount", FieldKind::Decimal)],
            }],
            views: vec![ViewSpec::new("Expense", ViewKind::List)],
            clarifications: vec![],
        }
    }

    /// Render the form as a compact, deterministic human/LLM-readable brief. This
    /// is what the lead engineer is handed; keeping it as one method makes the
    /// real-call and the stub see EXACTLY the same input.
    ///
    /// If the form has accumulated clarification rounds (from the multi-turn
    /// clarify loop), they are appended as a "Clarifications" section so the
    /// lead engineer sees the full Q&A history on every subsequent evaluation.
    pub fn brief(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("App: {}\n", self.app_name));
        out.push_str(&format!("Description: {}\n", self.description));
        out.push_str("Entities:\n");
        for entity in &self.entities {
            out.push_str(&format!("  - {}\n", entity.name));
            for field in &entity.fields {
                out.push_str(&format!(
                    "      {} : {}{}\n",
                    field.name,
                    field.kind.label(),
                    if field.required { " (required)" } else { "" },
                ));
            }
        }
        out.push_str("Views:\n");
        for view in &self.views {
            out.push_str(&format!("  - {} {}\n", view.entity, view.kind.label()));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_form_is_the_budgeting_app() {
        let form = IntakeForm::sample_budgeting_app();
        assert_eq!(form.app_name, "budget-tracker");
        assert_eq!(form.entities.len(), 1);
        let expense = &form.entities[0];
        assert_eq!(expense.name, "Expense");
        // amount + category + spent_on + note.
        assert_eq!(expense.fields.len(), 4);
        assert!(expense.fields.iter().any(|f| f.name == "amount"
            && f.kind == FieldKind::Decimal
            && f.required));
        assert!(expense.fields.iter().any(|f| f.name == "note" && !f.required));
        assert_eq!(form.views.len(), 1);
        assert_eq!(form.views[0].kind, ViewKind::List);
    }

    #[test]
    fn brief_mentions_app_entity_fields_and_view() {
        let brief = IntakeForm::sample_budgeting_app().brief();
        assert!(brief.contains("budget-tracker"));
        assert!(brief.contains("Expense"));
        assert!(brief.contains("amount"));
        assert!(brief.contains("decimal"));
        assert!(brief.contains("list"));
    }

    #[test]
    fn form_roundtrips_through_json() {
        let form = IntakeForm::sample_budgeting_app();
        let json = serde_json::to_string(&form).unwrap();
        let back: IntakeForm = serde_json::from_str(&json).unwrap();
        assert_eq!(form, back);
    }
}
