//! User stories: the consumer-abstracted unit of the living spec.
//!
//! After the onboarding document ([`crate::form::IntakeForm`]) seeds the first
//! investigation, the lead engineer turns it into a list of [`UserStory`]s. From
//! that point on, the user stories (and later, bug stories) are the SOURCE OF
//! TRUTH for the app, exactly the way stories and bug tickets are the source of
//! truth in real software development. The onboarding document is frozen as
//! read-only origin; the stories are what the user and the AI keep editing.
//!
//! A [`UserStory`] is deliberately NOT a Product-Owner / engineering user story.
//! There are no API contracts, no Gherkin acceptance criteria, no technical
//! vocabulary. It is what a non-technical person would understand: who it is for,
//! and a plain bulleted list of what they want to be able to SEE and DO. The user
//! can add, edit, and delete these freely during a refinement session
//! (`crate::refinement`).

use serde::{Deserialize, Serialize};

/// Stable identifier for a [`UserStory`], unique within one project. Snake_case
/// or a short slug (e.g. `"see_monthly_expenses"`); stable across refinement
/// turns so edits, suggestion references, and bug stories can point at it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoryId(pub String);

impl StoryId {
    /// Construct a story id from anything string-like.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a [`UserStory`] came from. Provenance matters because the stories are
/// the source of truth: the UI distinguishes what the AI proposed from what the
/// user wrote from what a bug surfaced, and the audit trail wants the origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryOrigin {
    /// Pre-filled by the lead engineer from the onboarding document during the
    /// first investigation.
    Investigation,
    /// Added by the user during a refinement session.
    UserAdded,
    /// Born from a post-build structured bug report (a "bug story"). These feed
    /// a post-build refinement session and then re-execution.
    BugReport,
}

/// A consumer-abstracted user story: the editable unit of the living spec.
///
/// Read it aloud and a non-technical person understands it: "For a `for_whom`, I
/// want to ..." followed by the plain bullets in `wants`. No API contracts, no
/// technical terms. The lead engineer pre-fills these from the onboarding
/// document; the user adds, edits, and deletes them; product suggestions
/// (`crate::engine::ProductSuggestion`) reference them by [`StoryId`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserStory {
    /// Stable id, unique within the project.
    pub id: StoryId,
    /// One-line plain-language title, e.g. "See my monthly expenses".
    pub title: String,
    /// Which kind of person this is for, in plain language (maps loosely to a
    /// role name from the onboarding document, but stated as a person, not a
    /// permission set). Example: "Anyone tracking their spending".
    pub for_whom: String,
    /// The plain bulleted list of what the user wants to be able to SEE and DO.
    /// NOT API contracts: "I can see a list of my expenses", "I can add a new
    /// one", "I can search by month". Each entry is one capability in the user's
    /// own words.
    pub wants: Vec<String>,
    /// Optional plain-language "so that ..." motivation. `None` when the user did
    /// not give one; the AI may fill it in as a suggestion.
    #[serde(default)]
    pub so_that: Option<String>,
    /// Where this story came from.
    pub origin: StoryOrigin,
}

impl UserStory {
    /// Construct a story pre-filled by the lead engineer's investigation.
    pub fn from_investigation(
        id: impl Into<String>,
        title: impl Into<String>,
        for_whom: impl Into<String>,
        wants: Vec<String>,
    ) -> Self {
        Self {
            id: StoryId::new(id),
            title: title.into(),
            for_whom: for_whom.into(),
            wants,
            so_that: None,
            origin: StoryOrigin::Investigation,
        }
    }

    /// Construct a story the user added by hand during a refinement session.
    pub fn user_added(
        id: impl Into<String>,
        title: impl Into<String>,
        for_whom: impl Into<String>,
        wants: Vec<String>,
    ) -> Self {
        Self {
            id: StoryId::new(id),
            title: title.into(),
            for_whom: for_whom.into(),
            wants,
            so_that: None,
            origin: StoryOrigin::UserAdded,
        }
    }

    /// Construct a "bug story" born from a post-build structured bug report.
    pub fn from_bug(
        id: impl Into<String>,
        title: impl Into<String>,
        for_whom: impl Into<String>,
        wants: Vec<String>,
    ) -> Self {
        Self {
            id: StoryId::new(id),
            title: title.into(),
            for_whom: for_whom.into(),
            wants,
            so_that: None,
            origin: StoryOrigin::BugReport,
        }
    }

    /// Attach a plain-language motivation. Builder form.
    pub fn so_that(mut self, motivation: impl Into<String>) -> Self {
        self.so_that = Some(motivation.into());
        self
    }

    /// Render the story as a plain-language block a non-technical reader (and the
    /// lead engineer) can read verbatim. Used in the brief the AI reviews and in
    /// the UI transcript.
    pub fn render(&self) -> String {
        let mut out = format!(
            "Story «{}» — {}\n  For: {}\n",
            self.id.as_str(),
            self.title,
            self.for_whom
        );
        for want in &self.wants {
            out.push_str(&format!("  • {want}\n"));
        }
        if let Some(so_that) = &self.so_that {
            out.push_str(&format!("  so that {so_that}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigation_story_has_investigation_origin() {
        let s = UserStory::from_investigation(
            "see_expenses",
            "See my expenses",
            "Anyone tracking spending",
            vec!["I can see a list of my expenses".to_string()],
        );
        assert_eq!(s.origin, StoryOrigin::Investigation);
        assert_eq!(s.id.as_str(), "see_expenses");
        assert!(s.so_that.is_none());
    }

    #[test]
    fn user_added_and_bug_origins_are_distinct() {
        let user = UserStory::user_added("a", "t", "w", vec![]);
        let bug = UserStory::from_bug("b", "t", "w", vec![]);
        assert_eq!(user.origin, StoryOrigin::UserAdded);
        assert_eq!(bug.origin, StoryOrigin::BugReport);
    }

    #[test]
    fn so_that_builder_attaches_motivation() {
        let s = UserStory::user_added("a", "t", "w", vec![]).so_that("I stay on budget");
        assert_eq!(s.so_that.as_deref(), Some("I stay on budget"));
    }

    #[test]
    fn render_is_plain_language_and_lists_wants() {
        let s = UserStory::from_investigation(
            "see_expenses",
            "See my expenses",
            "Anyone tracking spending",
            vec![
                "I can see a list of my expenses".to_string(),
                "I can search by month".to_string(),
            ],
        )
        .so_that("I know where my money goes");
        let r = s.render();
        assert!(r.contains("See my expenses"));
        assert!(r.contains("Anyone tracking spending"));
        assert!(r.contains("• I can see a list of my expenses"));
        assert!(r.contains("• I can search by month"));
        assert!(r.contains("so that I know where my money goes"));
        // No technical vocabulary leaked into the rendering helper itself.
        assert!(!r.to_lowercase().contains("api"));
    }

    #[test]
    fn story_round_trips_json() {
        let s = UserStory::from_investigation("x", "T", "W", vec!["a".to_string()]);
        let json = serde_json::to_string(&s).unwrap();
        let back: UserStory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn story_origin_serializes_snake_case() {
        let json = serde_json::to_string(&StoryOrigin::BugReport).unwrap();
        assert_eq!(json, "\"bug_report\"");
    }
}
