//! Story decomposition: split a parent story into component child stories per the
//! org's practice (ADR `story_decomposition_by_practice`).
//!
//! Flow: PROPOSE children from the parent + a practice (deterministic here; a real
//! agent + repo context later), the architect edits them, then COMMIT creates them as
//! real stories on the spine, linked to the parent. The write-back to a tracker AS the
//! right work-item type with parent/child relationship metadata is the provider phase.
//!
//! The parent/child linkage lives in this BFF-level `DecompositionStore` for now; a
//! `parent_id` field on the canonical Story spine is the eventual clean home (deferred
//! to avoid churning the 16 CanonicalStory construction sites in this pass).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use camerata_worktracker::{CanonicalStory, FeatureStatus};

/// One child type a practice produces (e.g. a "UI" story, an "API" story).
#[derive(Clone)]
pub struct ChildType {
    pub kind: String,
    pub title_suffix: String,
}

/// A decomposition practice: what a parent of a given level splits into. Configurable
/// per org; this is the default (Feature -> UI story + API story). A team that runs
/// Feature -> Story -> Task would configure more levels.
#[derive(Clone)]
pub struct Practice {
    pub parent_label: String,
    pub children: Vec<ChildType>,
}

impl Practice {
    /// The default practice: a feature splits into a UI story and an API story.
    pub fn default_feature() -> Self {
        Self {
            parent_label: "Feature".to_string(),
            children: vec![
                ChildType {
                    kind: "UI".to_string(),
                    title_suffix: "UI".to_string(),
                },
                ChildType {
                    kind: "API".to_string(),
                    title_suffix: "API".to_string(),
                },
            ],
        }
    }
}

/// A proposed child story: not yet created. The architect reviews/edits these before
/// committing. Round-trips to/from the cockpit (Deserialize for the edited commit).
#[derive(Clone, Serialize, Deserialize)]
pub struct ProposedChild {
    pub kind: String,
    pub title: String,
    pub description: String,
}

/// Propose the component children for a parent under a practice. Deterministic: one
/// proposed child per child-type, titled/described from the parent. A real engine
/// would read the affected repos to ground these; the shape is identical.
pub fn propose(parent: &CanonicalStory, practice: &Practice) -> Vec<ProposedChild> {
    practice
        .children
        .iter()
        .map(|ct| ProposedChild {
            kind: ct.kind.clone(),
            title: format!("{} — {}", parent.title, ct.title_suffix),
            description: format!(
                "The {} slice of the parent feature \"{}\". {}",
                ct.kind, parent.title, parent.description
            ),
        })
        .collect()
}

/// AI decomposition: the model reads the parent story + the practice's child-types and
/// proposes a grounded, specific child per type (titles/descriptions that reflect the
/// actual feature, not a template). Falls back to the deterministic [`propose`] when the
/// model is unreachable or returns nothing parseable, so the flow never dead-ends.
pub async fn propose_ai(
    parent: &CanonicalStory,
    practice: &Practice,
    llm: &crate::llm::Llm,
) -> Vec<ProposedChild> {
    let kinds: Vec<String> = practice
        .children
        .iter()
        .map(|ct| format!("{} ({})", ct.kind, ct.title_suffix))
        .collect();
    let system = "You are Camerata's lead engineer decomposing a parent work item into its \
        component child stories. For EACH requested child-type, write a specific, grounded \
        title and a 1-3 sentence description that reflects the actual feature (not a \
        template). Return ONLY a JSON array, no prose: \
        [{\"kind\":\"...\",\"title\":\"...\",\"description\":\"...\"}]. Use exactly the \
        requested kinds, one object each.";
    let user = format!(
        "Parent story: {}\n\nDescription: {}\n\nChild-types to produce (kind = the type): {}",
        parent.title,
        parent.description,
        kinds.join(", ")
    );
    let req = crate::llm::LlmRequest::new(user).with_system(system);
    let Ok(resp) = llm.complete(req).await else {
        return propose(parent, practice);
    };
    match parse_children(&resp.text, practice) {
        Some(children) if !children.is_empty() => children,
        _ => propose(parent, practice),
    }
}

/// Parse a model JSON array of children, keeping only the practice's known kinds and
/// ensuring one entry per requested kind (filling any the model omitted from the
/// deterministic template). Returns None if the response has no JSON array.
fn parse_children(raw: &str, practice: &Practice) -> Option<Vec<ProposedChild>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    let arr: Vec<ProposedChild> = serde_json::from_str(&raw[start..=end]).ok()?;
    // Re-key to the practice's child-types so the result is always well-formed.
    let children = practice
        .children
        .iter()
        .map(|ct| {
            arr.iter()
                .find(|c| c.kind.eq_ignore_ascii_case(&ct.kind))
                .cloned()
                .unwrap_or(ProposedChild {
                    kind: ct.kind.clone(),
                    title: format!("{} — {}", ct.kind, ct.title_suffix),
                    description: String::new(),
                })
        })
        .collect();
    Some(children)
}

/// Turn a proposed (possibly edited) child into a real story under `parent_id`.
pub fn to_story(parent_id: &str, child: &ProposedChild) -> CanonicalStory {
    CanonicalStory {
        id: format!("{parent_id}-{}", child.kind.to_lowercase()),
        external_ref: None,
        title: child.title.clone(),
        description: child.description.clone(),
        status: FeatureStatus::Intake,
        created_by: "architect".to_string(),
        targets: vec![],
    }
}

/// In-memory parent -> child-ids linkage.
#[derive(Clone, Default)]
pub struct DecompositionStore {
    links: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl DecompositionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `parent_id` decomposed into `child_ids` (replaces any prior set).
    pub fn record(&self, parent_id: &str, child_ids: Vec<String>) {
        if let Ok(mut guard) = self.links.lock() {
            guard.insert(parent_id.to_string(), child_ids);
        }
    }

    /// The child ids of a parent, in order.
    pub fn children_of(&self, parent_id: &str) -> Vec<String> {
        self.links
            .lock()
            .map(|g| g.get(parent_id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> CanonicalStory {
        CanonicalStory {
            id: "CAM-1".to_string(),
            external_ref: None,
            title: "Add CSV export to org members".to_string(),
            description: "Export the member directory.".to_string(),
            status: FeatureStatus::Intake,
            created_by: "architect".to_string(),
            targets: vec![],
        }
    }

    #[test]
    fn default_practice_proposes_ui_and_api() {
        let children = propose(&parent(), &Practice::default_feature());
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].kind, "UI");
        assert_eq!(children[1].kind, "API");
        assert!(children[0].title.contains("Add CSV export"));
    }

    #[test]
    fn to_story_ids_are_parent_scoped_and_linkage_records() {
        let children = propose(&parent(), &Practice::default_feature());
        let ui = to_story("CAM-1", &children[0]);
        assert_eq!(ui.id, "CAM-1-ui");
        assert_eq!(ui.status, FeatureStatus::Intake);

        let store = DecompositionStore::new();
        store.record(
            "CAM-1",
            vec!["CAM-1-ui".to_string(), "CAM-1-api".to_string()],
        );
        assert_eq!(store.children_of("CAM-1").len(), 2);
        assert!(store.children_of("CAM-9").is_empty());
    }
}
