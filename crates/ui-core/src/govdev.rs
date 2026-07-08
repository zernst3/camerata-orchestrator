//! The framework-agnostic Governed-Development state machine (Phase C of the UI-core
//! extraction, see `docs/plans/2026-07-01_ui-core-extraction.md`).
//!
//! [`GovDevState`] owns the poll / change-detection state that previously lived as three
//! process-global Dioxus signals in `camerata_ui::cockpit::uow` (`UOW_LAST_SEEN`,
//! `UOW_CHANGED`, `PULLED_WORK_ITEMS`), consolidated behind ONE TEA-style reducer
//! ([`GovDevState::apply`] over [`GovDevMsg`]) plus read-only selectors
//! (RUST-PURE-STATE-TRANSITIONS-1). The Dioxus adapter holds a single
//! `GlobalSignal<GovDevState>`, translates events/poll rows into messages, and renders
//! from the selectors; the reqwest calls stay at the adapter edge.

use std::collections::{HashMap, HashSet};

use camerata_api_types::workitems::WorkItem;

/// One input to the Governed-Development reducer. Every mutation of the poll /
/// change-detection state flows through exactly one of these via [`GovDevState::apply`].
#[derive(Clone, PartialEq, Debug)]
pub enum GovDevMsg {
    /// The background poll observed a work item's fresh `updated_at`. Folds the
    /// change-flag logic: first sight establishes the baseline WITHOUT flagging; a value
    /// strictly newer than the baseline flags CHANGED (the baseline is deliberately NOT
    /// advanced — only a pull/open advances it, so the flag persists until the user
    /// syncs); equal / older / empty values are no-ops.
    PollObserved { id: String, updated_at: String },
    /// The user pulled the latest for one work item (Pull latest / UoW open / a
    /// successful assign): clear its CHANGED flag and re-baseline `last_seen` to the
    /// freshly pulled `updated_at`, so the next poll measures against what was just seen
    /// (and e.g. a self-assign never self-flags). An empty `updated_at` still clears the
    /// flag but never clobbers a good baseline.
    PulledLatest { id: String, updated_at: String },
    /// A full work-item pull completed for a project: cache the items keyed by project
    /// id (so switching projects never shows stale items), replacing any previous pull.
    WorkItemsPulled { project_id: String, items: Vec<WorkItem> },
}

/// The Governed-Development poll / change-detection state. Pure (no I/O, no framework):
/// the Dioxus adapter owns one app-lifetime instance and drives it exclusively through
/// [`GovDevState::apply`]; renders read the selectors.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GovDevState {
    /// Per-work-item LAST-SEEN `updated_at`, keyed by the work item's stable id
    /// (`github:OWNER/REPO#N`). Captured when a UoW is opened / its work item is pulled;
    /// the baseline the background poll compares fresh timestamps against.
    last_seen: HashMap<String, String>,
    /// Work item ids currently flagged CHANGED (the board moved since the last-seen
    /// baseline). Shown as a change icon on the UoW detail header and the left-nav card;
    /// cleared by "Pull latest".
    changed: HashSet<String>,
    /// The last work-item pull, keyed by project id (so a project switch never shows
    /// stale items). `None` until the first pull of the session.
    pulled: Option<(String, Vec<WorkItem>)>,
}

impl GovDevState {
    /// An empty state: no baselines, nothing flagged, nothing pulled.
    pub fn new() -> Self {
        Self::default()
    }

    /// The single pure reducer: fold one [`GovDevMsg`] into the state.
    pub fn apply(&mut self, msg: GovDevMsg) {
        match msg {
            // Fold ONE poll result into the last-seen + changed state:
            // - No last-seen baseline yet for this id → establish it (this poll is the
            //   baseline; NOT flagged, so a first poll never marks everything changed).
            // - Polled `updated_at` strictly NEWER than last-seen → flag CHANGED.
            //   (ISO-8601 UTC timestamps compare correctly as strings.)
            // - Equal / older / empty polled value → no change.
            GovDevMsg::PollObserved { id, updated_at } => {
                if updated_at.is_empty() {
                    return;
                }
                match self.last_seen.get(&id) {
                    Some(seen) => {
                        if updated_at.as_str() > seen.as_str() {
                            self.changed.insert(id);
                        }
                    }
                    None => {
                        self.last_seen.insert(id, updated_at);
                    }
                }
            }
            // Clear the CHANGED flag and bump the last-seen baseline to the freshly
            // pulled `updated_at`, so the notification is dismissed and the next change
            // is measured against the value just seen. An empty `updated_at` still
            // clears the flag but must not overwrite a good baseline with a blank.
            GovDevMsg::PulledLatest { id, updated_at } => {
                self.changed.remove(&id);
                if !updated_at.is_empty() {
                    self.last_seen.insert(id, updated_at);
                }
            }
            // Cache the full pull keyed by project id, replacing any previous pull.
            GovDevMsg::WorkItemsPulled { project_id, items } => {
                self.pulled = Some((project_id, items));
            }
        }
    }

    /// Whether this work item is currently flagged CHANGED (the board moved since its
    /// last-seen baseline).
    pub fn is_changed(&self, id: &str) -> bool {
        self.changed.contains(id)
    }

    /// How many work items are currently flagged CHANGED.
    pub fn changed_count(&self) -> usize {
        self.changed.len()
    }

    /// The cached pull for `project_id`, or `None` when nothing was pulled for it (the
    /// cache belongs to a different project, or no pull happened yet). An empty
    /// `project_id` always returns `None` — an "unknown project" never matches a cache
    /// entry, even one that was (pathologically) stored under an empty id.
    pub fn pulled_for<'a>(&'a self, project_id: &str) -> Option<&'a [WorkItem]> {
        if project_id.is_empty() {
            return None;
        }
        match &self.pulled {
            Some((pid, items)) if pid == project_id => Some(items.as_slice()),
            _ => None,
        }
    }

    /// The last pull's items regardless of project (the chat system-prompt spine reads
    /// whatever was pulled this session). `None` until the first pull.
    pub fn pulled_items(&self) -> Option<&[WorkItem]> {
        self.pulled.as_ref().map(|(_, items)| items.as_slice())
    }
}

/// The label the UoW detail / nav card shows for the item's assignee(s): the joined
/// logins, or "Unassigned" when empty. Pure so it is unit-testable.
pub fn assignee_label(assignees: &[String]) -> String {
    if assignees.is_empty() {
        "Unassigned".to_string()
    } else {
        assignees.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::{assignee_label, GovDevMsg, GovDevState, WorkItem};

    /// Shorthand: apply a PollObserved message.
    fn poll(state: &mut GovDevState, id: &str, updated_at: &str) {
        state.apply(GovDevMsg::PollObserved {
            id: id.to_string(),
            updated_at: updated_at.to_string(),
        });
    }

    /// Shorthand: apply a PulledLatest message.
    fn pulled_latest(state: &mut GovDevState, id: &str, updated_at: &str) {
        state.apply(GovDevMsg::PulledLatest {
            id: id.to_string(),
            updated_at: updated_at.to_string(),
        });
    }

    /// A minimal WorkItem with just an id (the fields the cache tests care about).
    fn wi(id: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            ..WorkItem::default()
        }
    }

    #[test]
    fn assignee_label_joins_or_unassigned() {
        assert_eq!(assignee_label(&[]), "Unassigned");
        assert_eq!(assignee_label(&["octocat".to_string()]), "octocat");
        assert_eq!(
            assignee_label(&["octocat".to_string(), "hubot".to_string()]),
            "octocat, hubot"
        );
    }

    #[test]
    fn poll_first_sight_sets_baseline_without_flagging() {
        let mut state = GovDevState::new();
        // No baseline yet: this poll ESTABLISHES it and does not flag changed.
        poll(&mut state, "github:o/r#1", "2026-07-05T12:00:00Z");
        assert_eq!(
            state.last_seen.get("github:o/r#1").map(String::as_str),
            Some("2026-07-05T12:00:00Z")
        );
        assert!(!state.is_changed("github:o/r#1"), "first poll must not flag");
        assert_eq!(state.changed_count(), 0);
    }

    #[test]
    fn poll_newer_flags_changed_equal_or_older_does_not() {
        let mut state = GovDevState::new();
        state
            .last_seen
            .insert("wi".to_string(), "2026-07-05T12:00:00Z".to_string());

        // Equal → not changed.
        poll(&mut state, "wi", "2026-07-05T12:00:00Z");
        assert!(!state.is_changed("wi"), "equal timestamp is not a change");

        // Older → not changed.
        poll(&mut state, "wi", "2026-07-04T09:00:00Z");
        assert!(!state.is_changed("wi"), "older timestamp is not a change");

        // Empty polled → not changed (resilient: a blank value never flags).
        poll(&mut state, "wi", "");
        assert!(!state.is_changed("wi"), "empty polled value is not a change");

        // Strictly newer → changed. The last-seen baseline is deliberately NOT advanced
        // here (only a pull/open advances it), so the flag persists until the user syncs.
        poll(&mut state, "wi", "2026-07-06T08:00:00Z");
        assert!(state.is_changed("wi"), "newer timestamp flags changed");
        assert_eq!(state.changed_count(), 1);
        assert_eq!(
            state.last_seen.get("wi").map(String::as_str),
            Some("2026-07-05T12:00:00Z"),
            "a poll does not advance the baseline"
        );
    }

    #[test]
    fn pulled_latest_clears_flag_and_advances_baseline() {
        let mut state = GovDevState::new();
        state
            .last_seen
            .insert("wi".to_string(), "2026-07-05T12:00:00Z".to_string());
        state.changed.insert("wi".to_string());

        // A pull-latest with the fresh timestamp: flag clears, baseline advances.
        pulled_latest(&mut state, "wi", "2026-07-06T08:00:00Z");
        assert!(!state.is_changed("wi"), "pull-latest clears the flag");
        assert_eq!(
            state.last_seen.get("wi").map(String::as_str),
            Some("2026-07-06T08:00:00Z"),
            "pull-latest advances the last-seen baseline"
        );

        // After the bump, a poll at the same (now-baseline) timestamp does NOT re-flag.
        poll(&mut state, "wi", "2026-07-06T08:00:00Z");
        assert!(!state.is_changed("wi"), "no re-flag once the baseline caught up");
    }

    #[test]
    fn pulled_latest_with_empty_timestamp_still_clears_flag() {
        // A refresh that returned no updated_at must still dismiss the notification, and
        // must not overwrite a good baseline with an empty string.
        let mut state = GovDevState::new();
        state
            .last_seen
            .insert("wi".to_string(), "2026-07-05T12:00:00Z".to_string());
        state.changed.insert("wi".to_string());
        pulled_latest(&mut state, "wi", "");
        assert!(!state.is_changed("wi"), "flag clears even with an empty timestamp");
        assert_eq!(
            state.last_seen.get("wi").map(String::as_str),
            Some("2026-07-05T12:00:00Z"),
            "an empty timestamp must not clobber the baseline"
        );
    }

    /// Assigning a work item (including "Assign to me") must NOT itself cause the next
    /// background poll to flag the item CHANGED, because the assign response's
    /// `updated_at` re-baselines last-seen (same mechanism as a manual "Pull latest").
    /// A LATER real update (a strictly newer `updated_at`) must still flag. This is the
    /// full sequence: baseline established -> self-assign re-baselines -> same-timestamp
    /// poll does not flag -> newer-timestamp poll does flag.
    #[test]
    fn self_assign_rebaselines_so_the_next_poll_does_not_self_flag() {
        let mut state = GovDevState::new();
        let wid = "github:o/r#20";

        // 1. Baseline established (e.g. on UoW open / initial pull).
        poll(&mut state, wid, "2026-07-05T12:00:00Z");
        assert_eq!(
            state.last_seen.get(wid).map(String::as_str),
            Some("2026-07-05T12:00:00Z")
        );
        assert!(!state.is_changed(wid));

        // 2. Self-assign ("Assign to me") succeeds; GitHub's assign response carries a
        // fresh `updated_at` (the assignment itself bumped the issue). The adapter
        // applies PulledLatest with that timestamp to re-baseline.
        let assign_updated_at = "2026-07-05T12:05:00Z";
        pulled_latest(&mut state, wid, assign_updated_at);
        assert_eq!(state.last_seen.get(wid).map(String::as_str), Some(assign_updated_at));
        assert!(!state.is_changed(wid), "a successful assign must not leave a stale flag");

        // 3. The next background poll observes the SAME `updated_at` the assign already
        // re-baselined to (this is the case the assign itself produced) -> must NOT flag.
        poll(&mut state, wid, assign_updated_at);
        assert!(!state.is_changed(wid), "the poll must not self-flag the assign's own update");

        // 4. The story is updated again afterward by something else (a strictly newer
        // `updated_at`) -> the poll must still flag it CHANGED.
        poll(&mut state, wid, "2026-07-05T13:00:00Z");
        assert!(state.is_changed(wid), "a later real update must still flag changed");
    }

    #[test]
    fn work_items_pulled_stores_keyed_by_project() {
        let mut state = GovDevState::new();
        assert!(state.pulled_for("proj-1").is_none(), "nothing pulled yet");
        assert!(state.pulled_items().is_none(), "nothing pulled yet");

        state.apply(GovDevMsg::WorkItemsPulled {
            project_id: "proj-1".to_string(),
            items: vec![wi("github:o/r#1"), wi("github:o/r#2")],
        });

        // The pull is visible ONLY under its own project id.
        let items = state.pulled_for("proj-1").expect("pull cached for proj-1");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "github:o/r#1");
        assert!(
            state.pulled_for("proj-2").is_none(),
            "a different project never sees another project's pull"
        );
        // The project-agnostic read (chat spine) sees the same items.
        assert_eq!(state.pulled_items().map(<[WorkItem]>::len), Some(2));

        // A re-pull for another project REPLACES the cache (single-slot, keyed).
        state.apply(GovDevMsg::WorkItemsPulled {
            project_id: "proj-2".to_string(),
            items: vec![wi("github:o/r#9")],
        });
        assert!(state.pulled_for("proj-1").is_none(), "old project's pull is gone");
        assert_eq!(state.pulled_for("proj-2").map(<[WorkItem]>::len), Some(1));
    }

    #[test]
    fn pulled_for_empty_project_id_is_always_none() {
        let mut state = GovDevState::new();
        // Even a (pathological) pull stored under an empty project id must not match an
        // empty query — "no active project" never resolves to a cache hit.
        state.apply(GovDevMsg::WorkItemsPulled {
            project_id: String::new(),
            items: vec![wi("github:o/r#1")],
        });
        assert!(state.pulled_for("").is_none());
    }

    #[test]
    fn changed_count_tallies_flagged_items() {
        let mut state = GovDevState::new();
        state.last_seen.insert("a".to_string(), "2026-07-05T12:00:00Z".to_string());
        state.last_seen.insert("b".to_string(), "2026-07-05T12:00:00Z".to_string());
        poll(&mut state, "a", "2026-07-06T08:00:00Z");
        poll(&mut state, "b", "2026-07-06T08:00:00Z");
        assert_eq!(state.changed_count(), 2);
        assert!(state.is_changed("a") && state.is_changed("b"));

        pulled_latest(&mut state, "a", "2026-07-06T08:00:00Z");
        assert_eq!(state.changed_count(), 1);
        assert!(!state.is_changed("a"));
        assert!(state.is_changed("b"));
    }
}
