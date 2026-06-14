//! The bridge between the consumer screens and the real engine.
//!
//! The screens used to render purely mocked data (`data.rs`). This module wires
//! them to the typed engine instead: it turns the intake screen's raw inputs into
//! a real [`IntakeForm`], runs a deterministic "investigation" that derives
//! consumer-abstracted [`UserStory`]s, opens a real [`Project`] with a pre-build
//! [`RefinementSession`], and records every user/AI edit as a versioned revision
//! in the [`ArtifactStore`].
//!
//! Everything here is plain data + pure transitions so it is UNIT-TESTABLE without
//! a running Dioxus renderer (the Dioxus views just call into it). The async
//! persistence helpers are exercised against an in-memory SQLite store in the
//! tests, proving edits are recorded with real version history.
//!
//! What is deliberately still a seam: the "investigation" here is deterministic
//! (`stories_from_form`), not a live `claude -p` call, and the build/QA steps are
//! not yet driven by the governed fleet. Those are the live-agent wirings; this
//! module is the data-model + persistence spine they will plug into.

use chrono::{DateTime, Utc};

use camerata_intake::{
    abstract_design, BugReport, DesignCorpus, DesignReference, EntityCapabilities,
    EntityDefinition, EntityField, FieldType, IntakeForm, Phase, Project, RefinementContext,
    RefinementReviewer, RefinementSession, ReviewError, StoryId, StylePreferences, UserRole,
    UserStory, ViewKind, ViewSpec,
};
use camerata_persistence::{
    encode, ArtifactKind, ArtifactStore, EditActor, NewRevision, PersistenceError, RevisionOp,
};

// ─── raw intake inputs (what the intake screen collects, in consumer words) ───

/// One field on an entity, as the user typed it (a name + a plain-language type).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInput {
    /// The field's name in the user's words ("Title", "Day & time", "Price").
    pub name: String,
    /// The plain-language type label the intake screen offered (e.g. "a price").
    pub type_label: String,
}

/// One entity, as the user described it: a name, its fields, and the consumer-word
/// features ("add", "see a list", "edit", "remove", "search").
#[derive(Debug, Clone, PartialEq)]
pub struct EntityInput {
    /// The entity name ("Class", "Student", "Booking").
    pub name: String,
    /// The fields the user listed.
    pub fields: Vec<FieldInput>,
    /// Consumer-word features.
    pub features: Vec<String>,
}

/// One role and its top actions, as the user described them.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleInput {
    /// The role name ("Studio owner", "Student").
    pub name: String,
    /// The verb-phrase actions ("browse classes", "book a seat").
    pub actions: Vec<String>,
}

/// Everything the intake screen collects, before it becomes a typed form.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IntakeInputs {
    /// The app name.
    pub app_name: String,
    /// The one-paragraph plain-language description.
    pub description: String,
    /// The free-text constraints / look-and-feel wishes.
    pub constraints: String,
    /// The roles.
    pub roles: Vec<RoleInput>,
    /// The entities.
    pub entities: Vec<EntityInput>,
    /// The look-and-feel selections from the intake style picker (shipped
    /// palette, button shape, font, inspiration images). Defaults to "no
    /// preference".
    pub style: StylePreferences,
}

// ─── plain-language -> typed mappings ────────────────────────────────────────

/// Map a plain-language type label (the words the intake screen shows) to a typed
/// [`FieldType`]. Unknown labels fall back to free text, which is always safe.
pub fn field_type_from_label(label: &str) -> FieldType {
    match label.trim().to_lowercase().as_str() {
        "text" => FieldType::Text,
        "a longer piece of text" | "long text" => FieldType::LongText,
        "a number" => FieldType::Number,
        "a price" => FieldType::Money,
        "a date" => FieldType::Date,
        "a date and time" => FieldType::DateTime,
        "yes / no" | "yes/no" => FieldType::YesNo,
        "an email" | "email" => FieldType::Email,
        "a link" | "a web link" | "url" => FieldType::Url,
        "a link to another thing" => FieldType::LinkTo(String::new()),
        _ => FieldType::Text,
    }
}

/// Map consumer feature words ("add", "see a list", "edit", "remove", "search")
/// to the typed [`EntityCapabilities`].
pub fn capabilities_from_features(features: &[String]) -> EntityCapabilities {
    let mut caps = EntityCapabilities::default();
    for feature in features {
        match feature.trim().to_lowercase().as_str() {
            "add" | "create" => caps.can_add = true,
            "see a list" | "list" | "search" => caps.can_list = true,
            "see one" | "see the details" | "view" | "details" => caps.can_view = true,
            "edit" | "change" => caps.can_edit = true,
            "remove" | "delete" | "cancel" => caps.can_remove = true,
            _ => {}
        }
    }
    caps
}

/// Build a typed [`IntakeForm`] (the onboarding document) from the raw intake
/// inputs. Pure and total: any inputs produce a well-formed form.
pub fn intake_form_from_inputs(inputs: &IntakeInputs) -> IntakeForm {
    let roles = inputs
        .roles
        .iter()
        .map(|r| UserRole {
            name: r.name.clone(),
            actions: r.actions.clone(),
        })
        .collect();

    let entities: Vec<EntityDefinition> = inputs
        .entities
        .iter()
        .map(|e| EntityDefinition {
            name: e.name.clone(),
            description: String::new(),
            fields: e
                .fields
                .iter()
                .map(|f| EntityField::required(f.name.clone(), field_type_from_label(&f.type_label)))
                .collect(),
            capabilities: capabilities_from_features(&e.features),
        })
        .collect();

    // A list view for every entity that can be listed (the common consumer case).
    let views: Vec<ViewSpec> = entities
        .iter()
        .filter(|e| e.capabilities.can_list)
        .map(|e| ViewSpec::new(e.name.clone(), ViewKind::List))
        .collect();

    IntakeForm {
        app_name: inputs.app_name.clone(),
        description: inputs.description.clone(),
        roles,
        entities,
        constraints: inputs.constraints.clone(),
        views,
        clarifications: vec![],
        // The intake style picker is wired in a later pass; default = "no
        // preference, the lead engineer chooses" until then.
        style: inputs.style.clone(),
    }
}

/// The deterministic "investigation": derive consumer-abstracted [`UserStory`]s
/// from a typed form. One story per role (what that person can do), plus one per
/// listable entity (working with that thing). Stable ids so edits and revisions
/// can reference them across turns.
///
/// This stands in for the live lead-engineer investigation; it is deterministic so
/// the prototype opens on believable, editable stories without a model call.
pub fn stories_from_form(form: &IntakeForm) -> Vec<UserStory> {
    let mut stories = Vec::new();

    for (i, role) in form.roles.iter().enumerate() {
        let wants: Vec<String> = role
            .actions
            .iter()
            .map(|a| format!("I can {a}"))
            .collect();
        stories.push(UserStory::from_investigation(
            format!("role_{i}"),
            format!("As {}, what I can do", role.name),
            role.name.clone(),
            wants,
        ));
    }

    for entity in &form.entities {
        let caps = &entity.capabilities;
        let mut wants = Vec::new();
        if caps.can_add {
            wants.push(format!("I can add a {}", entity.name));
        }
        if caps.can_list {
            wants.push(format!("I can see a list of {}", entity.name));
        }
        if caps.can_edit {
            wants.push(format!("I can change a {}", entity.name));
        }
        if caps.can_remove {
            wants.push(format!("I can remove a {}", entity.name));
        }
        if wants.is_empty() {
            wants.push(format!("I can work with {}", entity.name));
        }
        stories.push(UserStory::from_investigation(
            format!("entity_{}", slug(&entity.name)),
            format!("Working with {}", entity.name),
            "Anyone using the app",
            wants,
        ));
    }

    stories
}

/// A lowercase, underscore slug for stable ids.
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

// ─── the app state (the live Project the screens render + edit) ──────────────

/// The consumer app's live state: a [`Project`] plus a queue of unflushed
/// revisions. Screens read the active session's stories and confidence, and call
/// the edit methods; each edit BOTH mutates the session AND queues a versioned
/// [`NewRevision`] so the change can be persisted in real time. The UI drains the
/// queue asynchronously with [`flush_pending`].
#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    /// The project under construction.
    pub project: Project,
    /// Revisions recorded but not yet flushed to the store. Drained by
    /// [`flush_pending`]. Every user/AI edit appends here, which is how "saved in
    /// real time, with version history" is realized.
    pub pending: Vec<NewRevision>,
}

impl AppState {
    /// Start a project from intake inputs: build the onboarding document, create
    /// the project, run the deterministic investigation, freeze the document, and
    /// open the first pre-build refinement session over the seeded stories. Queues
    /// the onboarding document and each seeded story as initial `Create` revisions.
    pub fn from_intake(project_id: impl Into<String>, inputs: &IntakeInputs) -> Self {
        let form = intake_form_from_inputs(inputs);
        let stories = stories_from_form(&form);
        let project_id = project_id.into();
        let now = Utc::now();

        let mut pending = Vec::new();
        // The onboarding document is the first revision (the frozen seed).
        if let Ok(rev) = onboarding_revision(&project_id, &form, now) {
            pending.push(rev);
        }
        // Each seeded story is an AI-authored Create (the investigation wrote it).
        for story in &stories {
            if let Ok(rev) = story_revision(&project_id, story, EditActor::Ai, RevisionOp::Create, now)
            {
                pending.push(rev);
            }
        }

        let mut project = Project::new(project_id, form);
        // Seeding freezes the onboarding document and opens the pre-build session.
        project
            .seed_from_investigation("session_1", stories)
            .expect("fresh project is never pre-frozen");
        Self { project, pending }
    }

    /// Queue a story revision for the given op/actor at the current time.
    fn queue_story(&mut self, story: &UserStory, actor: EditActor, op: RevisionOp) {
        if let Ok(rev) = story_revision(&self.project.id, story, actor, op, Utc::now()) {
            self.pending.push(rev);
        }
    }

    /// Drain the queued revisions, leaving the queue empty.
    pub fn take_pending(&mut self) -> Vec<NewRevision> {
        std::mem::take(&mut self.pending)
    }

    /// How many revisions are waiting to be flushed.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// The stories currently under refinement (the live source of truth in flight).
    pub fn active_stories(&self) -> &[UserStory] {
        self.project
            .active_session()
            .map(|s| s.stories.as_slice())
            .unwrap_or(&[])
    }

    /// The current confidence percentage (0 when there is no active session).
    pub fn confidence(&self) -> u8 {
        self.project
            .active_session()
            .map(|s| s.confidence.value())
            .unwrap_or(0)
    }

    /// The active refinement session, if any.
    pub fn active_session(&self) -> Option<&RefinementSession> {
        self.project.active_session()
    }

    /// USER adds a story to the active session and queues a `Create` revision.
    /// No-op if there is no session.
    pub fn add_story(&mut self, story: UserStory) {
        if let Some(session) = self.project.active_session_mut() {
            session.add_story(story.clone());
            self.queue_story(&story, EditActor::User, RevisionOp::Create);
        }
    }

    /// USER removes a story by id from the active session and queues a `Delete`
    /// revision. Returns whether it removed anything.
    pub fn remove_story(&mut self, id: &StoryId) -> bool {
        // Capture the story (for the revision payload id) before removing it.
        let removed_story = self
            .project
            .active_session()
            .and_then(|s| s.stories.iter().find(|st| &st.id == id).cloned());
        let removed = match self.project.active_session_mut() {
            Some(session) => session.remove_story(id),
            None => false,
        };
        if removed {
            if let Some(story) = removed_story {
                self.queue_story(&story, EditActor::User, RevisionOp::Delete);
            }
        }
        removed
    }

    /// USER edits (upserts) a story in the active session and queues an `Update`
    /// revision.
    pub fn upsert_story(&mut self, story: UserStory) {
        if let Some(session) = self.project.active_session_mut() {
            session.upsert_story(story.clone());
            self.queue_story(&story, EditActor::User, RevisionOp::Update);
        }
    }

    /// The project's lifecycle phase.
    pub fn phase(&self) -> Phase {
        self.project.phase
    }

    /// Run ONE AI review turn on the active session: the reviewer reads the live
    /// session, the result is folded in (story edits, new questions, suggestions,
    /// updated confidence), and any AI-upserted story is queued as a versioned
    /// revision. Returns whether a turn ran (false if there is no active session).
    ///
    /// The session is snapshotted before the await so no borrow is held across it.
    pub async fn run_review_turn(
        &mut self,
        reviewer: &dyn RefinementReviewer,
    ) -> Result<bool, ReviewError> {
        let form = self.project.onboarding.clone();
        let snapshot = match self.project.active_session() {
            Some(s) => s.clone(),
            None => return Ok(false),
        };
        let review = reviewer.review(&snapshot, &form).await?;
        let ai_upserts = review.upserted_stories.clone();
        if let Some(session) = self.project.active_session_mut() {
            session.apply_review(review);
        }
        for story in ai_upserts {
            self.queue_story(&story, EditActor::Ai, RevisionOp::Update);
        }
        Ok(true)
    }

    /// Historical designs to seed/influence this intake, but ONLY if the user
    /// opted in (`use_historical`). Returns an empty list otherwise. This is how
    /// the consuming side of the shared corpus reaches the UI.
    pub async fn historical_references(
        &self,
        corpus: &dyn DesignCorpus,
    ) -> Vec<DesignReference> {
        if self.project.sharing.is_consuming() {
            corpus.similar(&self.project.onboarding).await
        } else {
            Vec::new()
        }
    }

    /// Contribute this project's abstracted design to the corpus, but ONLY if the
    /// user opted in (`contribute_design`). No-op otherwise. Privacy-safe: only the
    /// abstracted shape is contributed (see `abstract_design`). Returns whether a
    /// contribution was made.
    pub async fn contribute_if_consented(&self, corpus: &dyn DesignCorpus) -> bool {
        if self.project.sharing.is_contributing() {
            // Attach the project's id (so it is withdrawable) and its fix history
            // (so the corpus carries bug knowledge, not just app shapes).
            let design = abstract_design(&self.project.onboarding, self.active_stories())
                .with_id(self.project.id.clone())
                .with_resolved_bugs(self.project.resolved_bugs.clone());
            corpus.contribute(design).await;
            true
        } else {
            false
        }
    }

    /// Withdraw this project's contribution from the corpus (the opt-out /
    /// right-to-be-forgotten path). Removes the design and every derived vector row
    /// stamped with this project's id. Called when the user toggles sharing OFF, so
    /// opting out actually deletes the shared data rather than just stopping future
    /// shares.
    pub async fn withdraw_from_corpus(&self, corpus: &dyn DesignCorpus) {
        corpus.withdraw(&self.project.id).await;
    }

    /// File a structured bug after QA: opens a POST-BUILD refinement session seeded
    /// with the report (which becomes a first-class bug story), so the fix runs
    /// through the same governed loop as a feature. The session id is derived from
    /// how many sessions have run.
    pub fn file_bug(&mut self, report: BugReport) {
        let session_id = format!("session_{}", self.project.session_count() + 1);
        self.project
            .begin_session(session_id, RefinementContext::PostBuild { bugs: vec![report] });
    }

    /// Record that a reported bug was fixed (symptom + what changed) into the
    /// project's history. With the user's consent this later enriches the shared
    /// corpus so future similar apps inherit the fix.
    pub fn record_fix(&mut self, symptom: impl Into<String>, fix: impl Into<String>) {
        self.project.record_fix(symptom, fix);
    }

    /// The build plan for this project: derived deterministically from the frozen
    /// onboarding document (the same `Plan` shape the governed fleet builds, the
    /// same the CLI po-demo uses). The Build screen hands this to the governed fleet
    /// runner (`camerata_fleet::build_from_plan`).
    pub fn build_plan(&self) -> camerata_intake::Plan {
        camerata_intake::StubLeadEngineer::plan_for(&self.project.onboarding)
    }
}

// ─── persistence bridge (every edit becomes a versioned revision) ────────────

/// Build the [`NewRevision`] for one story edit. The UI calls this on each
/// add/edit/remove and hands the result to [`ArtifactStore::record_revision`], so
/// every change (by the user OR the AI) is stored with its own version, actor, and
/// timestamp. `created_at` is caller-supplied for deterministic tests.
pub fn story_revision(
    project_id: &str,
    story: &UserStory,
    actor: EditActor,
    op: RevisionOp,
    created_at: DateTime<Utc>,
) -> Result<NewRevision, PersistenceError> {
    let payload = if matches!(op, RevisionOp::Delete) {
        String::new()
    } else {
        encode(story)?
    };
    Ok(NewRevision {
        project_id: project_id.to_string(),
        kind: ArtifactKind::UserStory,
        artifact_id: story.id.as_str().to_string(),
        actor,
        op,
        payload,
        created_at,
    })
}

/// Build the [`NewRevision`] for the onboarding document (the frozen seed). Stored
/// once under a singleton artifact id so its original state is always recoverable.
pub fn onboarding_revision(
    project_id: &str,
    form: &IntakeForm,
    created_at: DateTime<Utc>,
) -> Result<NewRevision, PersistenceError> {
    Ok(NewRevision {
        project_id: project_id.to_string(),
        kind: ArtifactKind::OnboardingDocument,
        artifact_id: "onboarding".to_string(),
        actor: EditActor::User,
        op: RevisionOp::Create,
        payload: encode(form)?,
        created_at,
    })
}

/// Persist a drained batch of revisions to a store, in order. The caller drains
/// the queue first (`AppState::take_pending`) so no UI signal guard is held across
/// the awaits here. This is what the UI's persistence coroutine calls after
/// `from_intake` and after each edit, so every change lands with real version
/// history. Returns the number of revisions written.
pub async fn flush<S: ArtifactStore + ?Sized>(
    store: &S,
    pending: &[NewRevision],
) -> Result<usize, PersistenceError> {
    for rev in pending {
        store.record_revision(rev).await?;
    }
    Ok(pending.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_intake::{InMemoryDesignCorpus, SharingPreferences, StubRefinementReviewer};
    use camerata_persistence::SqliteStore;

    fn sample_inputs() -> IntakeInputs {
        IntakeInputs {
            app_name: "Riverside Pottery".into(),
            description: "Students book weekly classes.".into(),
            constraints: "Warm, phone-friendly.".into(),
            roles: vec![
                RoleInput {
                    name: "Studio owner".into(),
                    actions: vec!["set up classes".into(), "see who booked".into()],
                },
                RoleInput {
                    name: "Student".into(),
                    actions: vec!["browse classes".into(), "book a seat".into()],
                },
            ],
            entities: vec![EntityInput {
                name: "Class".into(),
                fields: vec![
                    FieldInput { name: "Title".into(), type_label: "text".into() },
                    FieldInput { name: "Price".into(), type_label: "a price".into() },
                    FieldInput { name: "Day".into(), type_label: "a date".into() },
                ],
                features: vec!["add".into(), "see a list".into(), "edit".into(), "remove".into()],
            }],
            style: StylePreferences::default(),
        }
    }

    #[test]
    fn field_type_label_mapping() {
        assert_eq!(field_type_from_label("a price"), FieldType::Money);
        assert_eq!(field_type_from_label("a number"), FieldType::Number);
        assert_eq!(field_type_from_label("yes / no"), FieldType::YesNo);
        assert_eq!(field_type_from_label("a date"), FieldType::Date);
        assert!(matches!(field_type_from_label("a link to another thing"), FieldType::LinkTo(_)));
        // Unknown falls back to text.
        assert_eq!(field_type_from_label("something weird"), FieldType::Text);
    }

    #[test]
    fn capabilities_from_consumer_words() {
        let caps = capabilities_from_features(&[
            "add".into(),
            "see a list".into(),
            "edit".into(),
            "remove".into(),
        ]);
        assert!(caps.can_add && caps.can_list && caps.can_edit && caps.can_remove);
        assert!(!caps.can_view);
    }

    #[test]
    fn intake_form_is_built_from_inputs() {
        let form = intake_form_from_inputs(&sample_inputs());
        assert_eq!(form.app_name, "Riverside Pottery");
        assert_eq!(form.roles.len(), 2);
        assert_eq!(form.entities.len(), 1);
        let class = &form.entities[0];
        assert_eq!(class.name, "Class");
        assert_eq!(class.fields.len(), 3);
        // Price mapped to Money.
        assert!(class.fields.iter().any(|f| f.field_type == FieldType::Money));
        assert!(class.capabilities.can_add && class.capabilities.can_remove);
        // A list view was generated for the listable entity.
        assert_eq!(form.views.len(), 1);
    }

    #[test]
    fn investigation_derives_consumer_stories() {
        let form = intake_form_from_inputs(&sample_inputs());
        let stories = stories_from_form(&form);
        // 2 role stories + 1 entity story.
        assert_eq!(stories.len(), 3);
        // Role stories carry the actions as plain "I can ..." wants.
        let owner = stories.iter().find(|s| s.for_whom == "Studio owner").unwrap();
        assert!(owner.wants.iter().any(|w| w.contains("set up classes")));
        // Entity story lists the capabilities in plain language.
        let class = stories.iter().find(|s| s.title.contains("Class")).unwrap();
        assert!(class.wants.iter().any(|w| w == "I can add a Class"));
        assert!(class.wants.iter().any(|w| w == "I can remove a Class"));
    }

    #[test]
    fn from_intake_opens_a_frozen_project_with_a_pre_build_session() {
        let state = AppState::from_intake("proj_1", &sample_inputs());
        assert!(state.project.onboarding_frozen);
        assert_eq!(state.phase(), Phase::Refining);
        assert_eq!(state.active_session().unwrap().context.label(), "pre_build");
        // 3 seeded stories under refinement.
        assert_eq!(state.active_stories().len(), 3);
    }

    #[test]
    fn user_edits_mutate_the_session_and_queue_revisions() {
        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        let before = state.active_stories().len();
        // from_intake queued the onboarding doc + 3 story creates.
        assert_eq!(state.pending_count(), 4);

        state.add_story(UserStory::user_added(
            "extra",
            "A thing I thought of",
            "Me",
            vec!["I can do the thing".into()],
        ));
        assert_eq!(state.active_stories().len(), before + 1);
        // The add queued one more revision.
        assert_eq!(state.pending_count(), 5);

        assert!(state.remove_story(&StoryId::new("extra")));
        assert_eq!(state.active_stories().len(), before);
        // The remove queued a delete revision.
        assert_eq!(state.pending_count(), 6);
    }

    #[tokio::test]
    async fn every_edit_is_persisted_with_version_history() {
        let store = SqliteStore::open("sqlite::memory:").await.unwrap();
        let mut state = AppState::from_intake("proj_1", &sample_inputs());

        // Flush the initial state: onboarding + 3 stories = 4 revisions.
        let initial = state.take_pending();
        let written = flush(&store, &initial).await.unwrap();
        assert_eq!(written, 4);
        assert_eq!(state.pending_count(), 0);

        // The user edits role_0 twice; each queues a revision; flush persists them.
        state.upsert_story(UserStory::user_added(
            "role_0",
            "Edited once",
            "Studio owner",
            vec!["new".into()],
        ));
        state.upsert_story(UserStory::user_added(
            "role_0",
            "Edited twice",
            "Studio owner",
            vec!["newer".into()],
        ));
        let edits = state.take_pending();
        let written2 = flush(&store, &edits).await.unwrap();
        assert_eq!(written2, 2);

        // role_0 now has 3 versions: the seeded Create + 2 user Updates.
        let history = store
            .history("proj_1", ArtifactKind::UserStory, "role_0")
            .await
            .unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].op, RevisionOp::Create);
        assert_eq!(history[0].actor, EditActor::Ai); // seeded by the investigation
        assert_eq!(history[2].op, RevisionOp::Update);
        assert_eq!(history[2].actor, EditActor::User);
        // The latest version is the second edit.
        let current: UserStory = history[2].decode().unwrap();
        assert_eq!(current.title, "Edited twice");

        // The onboarding document is recoverable at version 1.
        let onboarding = store
            .current_artifact("proj_1", ArtifactKind::OnboardingDocument, "onboarding")
            .await
            .unwrap()
            .unwrap();
        let form: IntakeForm = onboarding.decode().unwrap();
        assert_eq!(form.app_name, "Riverside Pottery");
    }

    #[test]
    fn onboarding_revision_round_trips_the_form() {
        let form = intake_form_from_inputs(&sample_inputs());
        let rev = onboarding_revision("p", &form, Utc::now()).unwrap();
        assert_eq!(rev.kind, ArtifactKind::OnboardingDocument);
        let back: IntakeForm = serde_json::from_str(&rev.payload).unwrap();
        assert_eq!(back.app_name, form.app_name);
    }

    #[test]
    fn delete_revision_has_empty_payload() {
        let story = UserStory::user_added("x", "t", "w", vec![]);
        let rev = story_revision("p", &story, EditActor::User, RevisionOp::Delete, Utc::now()).unwrap();
        assert!(rev.payload.is_empty());
    }

    #[tokio::test]
    async fn run_review_turn_drives_the_real_reviewer() {
        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        // Before a review the session confidence is 0.
        assert_eq!(state.confidence(), 0);

        let reviewer = StubRefinementReviewer::new();
        let ran = state.run_review_turn(&reviewer).await.unwrap();
        assert!(ran);
        // The stub's first turn sets confidence to its start (55) and raises an
        // admin suggestion (sample_inputs has a "Studio owner" role).
        assert_eq!(state.confidence(), 55);
        let session = state.active_session().unwrap();
        assert!(session.suggestions.iter().any(|s| s.id == "admin_users"));
        assert!(!session.open_questions().is_empty());
    }

    #[tokio::test]
    async fn historical_references_respects_the_opt_in() {
        let corpus = InMemoryDesignCorpus::new();
        // Seed the corpus with a prior design whose entity matches sample_inputs ("Class").
        let prior = abstract_design(&intake_form_from_inputs(&sample_inputs()), &[]);
        corpus.contribute(prior).await;

        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        // Opted OUT by default: no history, even though a match exists.
        assert!(state.historical_references(&corpus).await.is_empty());

        // Opt in: now the matching prior design is offered.
        state.project.sharing = SharingPreferences {
            contribute_design: false,
            use_historical: true,
        };
        let hits = state.historical_references(&corpus).await;
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn contribute_only_happens_with_consent() {
        let corpus = InMemoryDesignCorpus::new();
        let mut state = AppState::from_intake("proj_1", &sample_inputs());

        // Opted out: no contribution.
        assert!(!state.contribute_if_consented(&corpus).await);
        assert!(corpus.similar(&intake_form_from_inputs(&sample_inputs())).await.is_empty());

        // Opt in: the abstracted design is contributed and findable.
        state.project.sharing = SharingPreferences {
            contribute_design: true,
            use_historical: false,
        };
        assert!(state.contribute_if_consented(&corpus).await);
        assert!(!corpus.similar(&intake_form_from_inputs(&sample_inputs())).await.is_empty());
    }

    #[tokio::test]
    async fn contributed_design_carries_the_projects_fix_history() {
        let corpus = InMemoryDesignCorpus::new();
        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        state.project.record_fix("A class could be overbooked", "Reject bookings when full");
        state.project.sharing = SharingPreferences {
            contribute_design: true,
            use_historical: false,
        };
        assert!(state.contribute_if_consented(&corpus).await);

        let hits = corpus.similar(&intake_form_from_inputs(&sample_inputs())).await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].resolved_bugs.len(), 1);
        assert!(hits[0].resolved_bugs[0].fix.contains("Reject bookings"));
    }

    #[tokio::test]
    async fn opting_out_withdraws_the_contribution() {
        let corpus = InMemoryDesignCorpus::new();
        let mut state = AppState::from_intake("proj_1", &sample_inputs());

        // Opt in and contribute.
        state.project.sharing = SharingPreferences {
            contribute_design: true,
            use_historical: false,
        };
        assert!(state.contribute_if_consented(&corpus).await);
        assert!(!corpus.similar(&intake_form_from_inputs(&sample_inputs())).await.is_empty());

        // Opt out at any time: the shared data is deleted from the corpus.
        state.withdraw_from_corpus(&corpus).await;
        assert!(corpus.similar(&intake_form_from_inputs(&sample_inputs())).await.is_empty());
    }

    #[test]
    fn filing_a_bug_opens_a_post_build_session_with_a_bug_story() {
        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        let sessions_before = state.project.session_count();
        state.file_bug(BugReport::new(
            "Class list",
            "tapped Book on a full class",
            "to join a waitlist",
            "nothing happened",
        ));
        assert_eq!(state.project.session_count(), sessions_before + 1);
        let session = state.active_session().unwrap();
        assert_eq!(session.context.label(), "post_build");
        // The report became a first-class bug story.
        assert!(session
            .stories
            .iter()
            .any(|s| s.origin == camerata_intake::StoryOrigin::BugReport));
    }

    #[test]
    fn build_plan_is_derived_and_buildable() {
        let state = AppState::from_intake("proj_1", &sample_inputs());
        let plan = state.build_plan();
        assert!(plan.is_buildable());
        // The plan names the app and references its entity (Class).
        assert_eq!(plan.app_name, "Riverside Pottery");
        assert!(plan.tasks.iter().any(|t| t.description.contains("Class")));
    }

    #[test]
    fn recording_a_fix_feeds_the_project_history() {
        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        state.record_fix("Overbooking was possible", "Reject bookings when full");
        assert_eq!(state.project.resolved_bugs.len(), 1);
        assert_eq!(state.project.resolved_bugs[0].fix, "Reject bookings when full");
    }

    /// End-to-end: drive the whole composed consumer lifecycle through the bridge,
    /// proving the session's pieces (review, persistence, execution, post-build bug
    /// loop, corpus contribute/withdraw) compose. Doubles as executable documentation
    /// of the flow.
    #[tokio::test]
    async fn full_consumer_lifecycle_through_the_bridge() {
        let store = SqliteStore::open("sqlite::memory:").await.unwrap();
        let corpus = InMemoryDesignCorpus::new();
        let reviewer = StubRefinementReviewer::new();

        // 1. INTAKE: a typed onboarding document + seeded stories + a pre-build session.
        let mut state = AppState::from_intake("proj_1", &sample_inputs());
        assert_eq!(state.phase(), Phase::Refining);
        // The initial documents persist (onboarding + stories) with version history.
        let initial = state.take_pending();
        assert!(flush(&store, &initial).await.unwrap() >= 1);

        // 2. REFINE: a real AI review turn moves confidence off zero and suggests RBAC.
        assert!(state.run_review_turn(&reviewer).await.unwrap());
        assert!(state.confidence() > 0);
        assert!(state
            .active_session()
            .unwrap()
            .suggestions
            .iter()
            .any(|s| s.id == "admin_users"));

        // 3. CONVERGE + EXECUTE: the user is happy; the build runs (governed elsewhere).
        state.project.active_session_mut().unwrap().converge();
        state.project.enter_execution().unwrap();
        assert_eq!(state.phase(), Phase::Executing);
        state.project.finish_execution().unwrap();

        // 4. POST-BUILD: QA finds a bug; filing it opens a post-build session.
        state.file_bug(BugReport::new("Class list", "tapped Book on a full class", "a waitlist", "nothing"));
        assert_eq!(state.active_session().unwrap().context.label(), "post_build");
        // The fix is built and recorded.
        state.record_fix("Booking allowed past the seat limit", "Reject bookings when full");

        // 5. PUBLISH after the (already executed) build.
        state.project.publish().unwrap();
        assert!(state.project.is_published());

        // 6. SHARE (opt-in): the abstracted design + the fix reach the corpus.
        state.project.sharing.contribute_design = true;
        assert!(state.contribute_if_consented(&corpus).await);
        let hits = corpus.similar(&intake_form_from_inputs(&sample_inputs())).await;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].resolved_bugs.iter().any(|b| b.fix.contains("Reject bookings")));

        // 7. OPT-OUT (right to be forgotten): the contribution is deleted.
        state.withdraw_from_corpus(&corpus).await;
        assert!(corpus.similar(&intake_form_from_inputs(&sample_inputs())).await.is_empty());
    }
}
