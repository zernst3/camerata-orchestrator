//! The shared design corpus: opt-in learning across bespoke apps.
//!
//! Per Zach (2026-06-14): in the refinement flow a user can opt IN or OUT of
//! sharing their design documents and stories. Shared (and abstracted) designs go
//! into a corpus that Camerata can draw on when the NEXT user builds a similar app
//! (a second person building a rental-payment app can start from the shape of one
//! that already exists). The second user separately chooses whether to "use
//! historical data to influence the design." The payoff is consistency and
//! robustness across all the bespoke apps Camerata builds, which makes them easier
//! to maintain, plus a faster intake for the consuming user.
//!
//! Two independent consents, both OPT-IN (default off):
//! - [`SharingPreferences::contribute_design`] — share my design to help future apps.
//! - [`SharingPreferences::use_historical`] — let prior designs influence/speed mine.
//!
//! Privacy is load-bearing: only the SHAPE of a design is ever shared, never the
//! business's data or its sensitive free text. [`abstract_design`] strips the
//! description, constraints, style, and field option-values, keeping the structure
//! (entities, capabilities, story patterns). Fuller anonymization (e.g. generalizing
//! identifying entity names) is a follow-up captured in the decision record.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::form::IntakeForm;
use crate::story::{StoryOrigin, UserStory};

// ─── consent ─────────────────────────────────────────────────────────────────

/// A project's opt-in consents for the shared design corpus. Both default to
/// `false`: nothing is shared and no history is used unless the user chooses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SharingPreferences {
    /// Opt-in: contribute this project's ABSTRACTED design + stories to the corpus
    /// so future users building similar apps can learn from them.
    #[serde(default)]
    pub contribute_design: bool,
    /// Opt-in: use historical designs from prior consenting users to influence and
    /// speed up THIS project's intake.
    #[serde(default)]
    pub use_historical: bool,
}

impl SharingPreferences {
    /// Whether this project contributes its design to the corpus.
    pub fn is_contributing(&self) -> bool {
        self.contribute_design
    }

    /// Whether this project draws on historical designs.
    pub fn is_consuming(&self) -> bool {
        self.use_historical
    }
}

/// Plain-language copy shown next to the "share my design" opt-in, so the user
/// understands the benefit and the privacy guarantee.
pub const CONTRIBUTE_BENEFIT: &str =
    "Sharing your design helps Camerata build better, more consistent apps for \
     everyone. Only the SHAPE of your app is shared (the kinds of things it tracks \
     and what people can do); your business data and your private notes are never \
     shared.";

/// Plain-language copy shown next to the "use historical data" opt-in.
pub const USE_HISTORICAL_BENEFIT: &str =
    "Starting from what others built for similar apps can speed up your setup and \
     begin you from a proven, consistent design you can still change freely.";

// ─── an abstracted, shareable design ─────────────────────────────────────────

/// A bug that was reported and fixed, recorded so future similar bugs benefit.
/// The `symptom` is what went wrong, in plain language; the `fix` is what changed
/// to resolve it. These travel WITH a shared design (when the user opts in), so the
/// corpus carries fix knowledge, not just app shapes: the next person hitting a
/// similar bug starts from a known remedy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedBug {
    /// What went wrong, in plain language (abstracted from the bug report).
    pub symptom: String,
    /// What changed to fix it, in plain language.
    pub fix: String,
}

impl ResolvedBug {
    /// Construct a resolved-bug record.
    pub fn new(symptom: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            symptom: symptom.into(),
            fix: fix.into(),
        }
    }
}

/// A consented, abstracted prior design the corpus can offer the next user as a
/// reference. It carries the SHAPE of an app, not its data, plus the fix history so
/// future similar apps inherit hard-won bug knowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignReference {
    /// The contribution id: the owning project's id, stamped on this design AND
    /// on every derived row (each design / story / bug-fix vector) in the search
    /// index. It is the handle that makes opt-out deletable: withdrawing this id
    /// removes every trace of the contribution from the corpus and its vector DB.
    /// Empty for an un-contributed, in-flight abstraction.
    #[serde(default)]
    pub id: String,
    /// A short, generalized label for the kind of app (e.g. "rental payment app").
    pub app_kind: String,
    /// A one-line, non-identifying summary of the shape.
    pub summary: String,
    /// The abstracted user stories (structure + plain wants; specifics removed).
    pub stories: Vec<UserStory>,
    /// Bugs that were reported and fixed in this design, abstracted so the next
    /// builder of a similar app inherits the fix. Empty when there is no fix history.
    #[serde(default)]
    pub resolved_bugs: Vec<ResolvedBug>,
}

impl DesignReference {
    /// Attach a fix history (the resolved bugs from the project). Builder form.
    pub fn with_resolved_bugs(mut self, bugs: Vec<ResolvedBug>) -> Self {
        self.resolved_bugs = bugs;
        self
    }

    /// Stamp the contribution id (the owning project's id). Builder form. Required
    /// before contributing if the design is ever to be withdrawable.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
}

/// Abstract a form + its stories into a shareable [`DesignReference`]. Strips the
/// description, constraints, look-and-feel, and field option-values (anything that
/// could carry the business's specifics), keeping the structural shape. The
/// `app_kind` is derived from the entity names, which describe the domain pattern.
pub fn abstract_design(form: &IntakeForm, stories: &[UserStory]) -> DesignReference {
    let entity_names: Vec<&str> = form.entities.iter().map(|e| e.name.as_str()).collect();
    let app_kind = if entity_names.is_empty() {
        "app".to_string()
    } else {
        format!("{} app", entity_names.join(" / ").to_lowercase())
    };
    let summary = format!(
        "Tracks {} thing(s) ({}); {} role(s).",
        form.entities.len(),
        entity_names.join(", "),
        form.roles.len(),
    );

    // Re-stamp stories as corpus references: keep title/for_whom/wants (the shape),
    // drop any motivation note that could be specific, and mark provenance.
    let stories = stories
        .iter()
        .map(|s| {
            UserStory {
                id: s.id.clone(),
                title: s.title.clone(),
                for_whom: s.for_whom.clone(),
                wants: s.wants.clone(),
                so_that: None,
                origin: StoryOrigin::Investigation,
            }
        })
        .collect();

    DesignReference {
        // Id + fix history are attached by the caller (they live on the Project,
        // not the form), via `with_id` / `with_resolved_bugs`.
        id: String::new(),
        app_kind,
        summary,
        stories,
        resolved_bugs: Vec::new(),
    }
}

// ─── the corpus seam ─────────────────────────────────────────────────────────

/// DESIGN-CORPUS SEAM — the store of consented, abstracted designs.
///
/// `similar` powers the consuming side (use_historical): given a form, return prior
/// designs whose shape resembles it. `contribute` powers the contributing side: add
/// an already-abstracted design. Implementations decide similarity + storage; the
/// caller is responsible for only contributing designs the user consented to share.
#[async_trait]
pub trait DesignCorpus: Send + Sync {
    /// Prior consented designs similar to `form`, best first.
    async fn similar(&self, form: &IntakeForm) -> Vec<DesignReference>;
    /// Contribute one abstracted design. Upserts by [`DesignReference::id`]: a
    /// re-contribution from the same project replaces its prior entry rather than
    /// duplicating it (so the corpus never accumulates stale copies of one app).
    async fn contribute(&self, design: DesignReference);
    /// Withdraw a contribution by its id (the owning project's id). This is the
    /// opt-out / right-to-be-forgotten path: it removes the design AND, in a real
    /// implementation, every derived vector row stamped with that id. Removing a
    /// missing id is a no-op.
    async fn withdraw(&self, id: &str);
}

/// An in-memory [`DesignCorpus`] for tests and the prototype. Similarity is a naive
/// overlap of entity-name tokens, which is enough to prove the seam; a real corpus
/// would use embeddings / structural matching.
#[derive(Debug, Default)]
pub struct InMemoryDesignCorpus {
    // `std` mutex is correct here: the lock is never held across an `.await`
    // (each method locks, does synchronous work, and drops the guard), so this
    // needs no async-mutex and no extra tokio feature.
    designs: std::sync::Mutex<Vec<DesignReference>>,
}

impl InMemoryDesignCorpus {
    /// Construct an empty corpus.
    pub fn new() -> Self {
        Self {
            designs: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl DesignCorpus for InMemoryDesignCorpus {
    async fn similar(&self, form: &IntakeForm) -> Vec<DesignReference> {
        let want: Vec<String> = form
            .entities
            .iter()
            .map(|e| e.name.to_lowercase())
            .collect();
        let designs = self.designs.lock().expect("corpus mutex poisoned");
        let mut scored: Vec<(usize, DesignReference)> = designs
            .iter()
            .map(|d| {
                let score = want
                    .iter()
                    .filter(|w| d.app_kind.to_lowercase().contains(w.as_str()))
                    .count();
                (score, d.clone())
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, d)| d).collect()
    }

    async fn contribute(&self, design: DesignReference) {
        let mut designs = self.designs.lock().expect("corpus mutex poisoned");
        // Upsert by id: a re-contribution replaces the project's prior entry. An
        // empty id (un-stamped) always appends (it is not withdrawable).
        if !design.id.is_empty() {
            designs.retain(|d| d.id != design.id);
        }
        designs.push(design);
    }

    async fn withdraw(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.designs
            .lock()
            .expect("corpus mutex poisoned")
            .retain(|d| d.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rental_form() -> IntakeForm {
        let mut form = IntakeForm::sample_app();
        form.app_name = "rent-tracker".into();
        form.description = "PRIVATE: my tenants and what they owe me at 42 Main St".into();
        form.constraints = "PRIVATE: do not share my landlord LLC details".into();
        form.entities[0].name = "Tenant".into();
        form
    }

    #[test]
    fn consents_default_to_opt_out() {
        let prefs = SharingPreferences::default();
        assert!(!prefs.is_contributing());
        assert!(!prefs.is_consuming());
    }

    #[test]
    fn abstraction_strips_private_text_and_keeps_shape() {
        let form = rental_form();
        let stories = vec![UserStory::from_investigation(
            "s1",
            "Track rent",
            "Landlord",
            vec!["I can see who owes me".into()],
        )
        .so_that("PRIVATE specific motivation")];
        let design = abstract_design(&form, &stories);

        // The shape is kept...
        assert!(design.app_kind.contains("tenant"));
        assert!(design.stories[0].wants.iter().any(|w| w.contains("owes")));
        // ...the private specifics are gone.
        let blob = serde_json::to_string(&design).unwrap();
        assert!(!blob.contains("PRIVATE"));
        assert!(!blob.contains("42 Main St"));
        assert!(design.stories[0].so_that.is_none());
    }

    #[tokio::test]
    async fn corpus_contributes_and_finds_similar_designs() {
        let corpus = InMemoryDesignCorpus::new();
        // First landlord contributes (after consenting).
        let first = abstract_design(&rental_form(), &[]);
        corpus.contribute(first).await;

        // A second user building a similar app finds it.
        let mut second = IntakeForm::sample_app();
        second.entities[0].name = "Tenant".into();
        let hits = corpus.similar(&second).await;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].app_kind.contains("tenant"));
    }

    #[tokio::test]
    async fn contribute_upserts_by_id_no_duplicates() {
        let corpus = InMemoryDesignCorpus::new();
        let base = abstract_design(&rental_form(), &[]).with_id("proj_42");
        corpus.contribute(base.clone()).await;
        // Re-contribute the same project (e.g. after an edit): replaces, not dupes.
        corpus.contribute(base.with_resolved_bugs(vec![ResolvedBug::new("x", "y")])).await;

        let mut second = IntakeForm::sample_app();
        second.entities[0].name = "Tenant".into();
        let hits = corpus.similar(&second).await;
        assert_eq!(hits.len(), 1, "re-contribution must replace, not duplicate");
        assert_eq!(hits[0].resolved_bugs.len(), 1);
    }

    #[tokio::test]
    async fn withdraw_deletes_the_contribution_by_id() {
        let corpus = InMemoryDesignCorpus::new();
        corpus
            .contribute(abstract_design(&rental_form(), &[]).with_id("proj_42"))
            .await;
        let mut second = IntakeForm::sample_app();
        second.entities[0].name = "Tenant".into();
        assert_eq!(corpus.similar(&second).await.len(), 1);

        // Opt out: withdraw by id removes it entirely.
        corpus.withdraw("proj_42").await;
        assert!(corpus.similar(&second).await.is_empty());
        // Withdrawing an unknown id is a harmless no-op.
        corpus.withdraw("never_existed").await;
    }

    #[tokio::test]
    async fn corpus_returns_nothing_for_unrelated_app() {
        let corpus = InMemoryDesignCorpus::new();
        corpus.contribute(abstract_design(&rental_form(), &[])).await;
        // An unrelated app (expense tracker, entity "Expense") matches nothing.
        let unrelated = IntakeForm::sample_app(); // entity "Expense"
        let hits = corpus.similar(&unrelated).await;
        assert!(hits.is_empty());
    }

    #[test]
    fn preferences_round_trip_json() {
        let prefs = SharingPreferences {
            contribute_design: true,
            use_historical: false,
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let back: SharingPreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(back, prefs);
    }

    #[test]
    fn resolved_bugs_travel_with_a_shared_design() {
        let design = abstract_design(&rental_form(), &[]).with_resolved_bugs(vec![
            ResolvedBug::new(
                "A booking could be made past the seat limit",
                "Added a check that rejects a booking when the class is full",
            ),
        ]);
        assert_eq!(design.resolved_bugs.len(), 1);
        // Fix knowledge round-trips and carries no private specifics.
        let json = serde_json::to_string(&design).unwrap();
        let back: DesignReference = serde_json::from_str(&json).unwrap();
        assert_eq!(back.resolved_bugs[0].fix, design.resolved_bugs[0].fix);
        assert!(!json.contains("PRIVATE"));
    }

    #[tokio::test]
    async fn corpus_carries_fix_knowledge_to_the_next_user() {
        let corpus = InMemoryDesignCorpus::new();
        let shared = abstract_design(&rental_form(), &[]).with_resolved_bugs(vec![
            ResolvedBug::new("Late fee applied twice", "Made the late-fee job idempotent"),
        ]);
        corpus.contribute(shared).await;

        let mut second = IntakeForm::sample_app();
        second.entities[0].name = "Tenant".into();
        let hits = corpus.similar(&second).await;
        assert_eq!(hits.len(), 1);
        // The next builder inherits the prior fix.
        assert_eq!(hits[0].resolved_bugs.len(), 1);
        assert!(hits[0].resolved_bugs[0].fix.contains("idempotent"));
    }
}
