//! The saved inspections of the open project, as the scan screen offers them.
//!
//! Every cockpit scan is saved as a run (see the server's run store). When the app is closed or
//! crashes in the middle of one, or a part failed, the run stays on disk. The scan screen lists
//! them (`GET /api/inspections`) and offers the newest unfinished one with "Run what remains":
//! "This inspection was interrupted" when its process is gone, "This inspection did not finish"
//! when it ended with a part left. Zach's rule: any interruption should cost minutes, never the
//! run.
//!
//! Pure: the wire shape, which saved inspection to offer, and the words. No clock, no network.

use serde::Deserialize;

/// Where a saved inspection stands (the server's `InspectionStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedStatus {
    Complete,
    /// Ended with a failed part, a part that did not run, or a refused export.
    Incomplete,
    /// Its process is gone before it finished.
    Interrupted,
    /// A process works on it right now.
    Live,
}

/// One saved inspection (one row of `GET /api/inspections`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SavedInspection {
    pub id: String,
    /// When it started (RFC 3339).
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub repos: Vec<String>,
    pub status: SavedStatus,
    /// `cli` or `cockpit`: where it was started.
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub remaining_parts: usize,
    #[serde(default)]
    pub estimate_usd: f64,
    /// Whether this screen can resume it: unfinished and started here.
    #[serde(default)]
    pub resumable_here: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// The body of `GET /api/inspections`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct SavedInspections {
    #[serde(default)]
    pub inspections: Vec<SavedInspection>,
}

/// What the scan screen shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Offer {
    /// The newest unfinished inspection this screen can resume.
    pub offered: Option<SavedInspection>,
    /// The others worth listing: older unfinished ones, ones another place must resume, and ones
    /// running now. Newest first. A complete inspection is never listed.
    pub others: Vec<SavedInspection>,
}

impl SavedInspection {
    /// True for an inspection that is unfinished (interrupted or incomplete).
    pub fn is_unfinished(&self) -> bool {
        matches!(
            self.status,
            SavedStatus::Interrupted | SavedStatus::Incomplete
        )
    }

    /// The headline over the offer.
    pub fn headline(&self) -> &'static str {
        match self.status {
            SavedStatus::Interrupted => "This inspection was interrupted",
            SavedStatus::Incomplete => "This inspection did not finish",
            SavedStatus::Live => "This inspection is running",
            SavedStatus::Complete => "This inspection is complete",
        }
    }

    /// One plain sentence: when it started, on what, and what finishing it costs.
    pub fn summary(&self) -> String {
        let repos = if self.repos.is_empty() {
            "no repositories".to_string()
        } else {
            self.repos.join(", ")
        };
        let parts = self.remaining_parts;
        let noun = if parts == 1 { "part" } else { "parts" };
        format!(
            "Started {} on {repos}. {parts} {noun} remain; finishing it should cost about ${:.2} \
             more. Work already paid for is not sent again.",
            self.started, self.estimate_usd
        )
    }

    /// The line a listed inspection gets.
    pub fn list_line(&self) -> String {
        let state = match self.status {
            SavedStatus::Interrupted => "interrupted",
            SavedStatus::Incomplete => "did not finish",
            SavedStatus::Live => "running now",
            SavedStatus::Complete => "complete",
        };
        let repos = self.repos.join(", ");
        if self.is_unfinished() {
            format!(
                "{} on {repos}: {state}, {} remaining",
                self.started, self.remaining_parts
            )
        } else {
            format!("{} on {repos}: {state}", self.started)
        }
    }

    /// For an unfinished inspection another place must resume, where that is.
    pub fn resume_elsewhere(&self) -> Option<String> {
        (self.is_unfinished() && !self.resumable_here).then(|| {
            format!(
                "Started from the command line: resume it with camerata inspect --resume {}",
                self.id
            )
        })
    }
}

/// Decide what to show for `all` (any order). The newest unfinished inspection this screen can
/// resume is offered; every other unfinished or running one is listed, newest first. A live
/// inspection is never offered.
pub fn offer(mut all: Vec<SavedInspection>) -> Offer {
    all.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    let offered_at = all
        .iter()
        .position(|r| r.is_unfinished() && r.resumable_here);
    let offered = offered_at.map(|i| all.remove(i));
    let others = all
        .into_iter()
        .filter(|r| r.is_unfinished() || r.status == SavedStatus::Live)
        .collect();
    Offer { offered, others }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, started: &str, status: SavedStatus) -> SavedInspection {
        SavedInspection {
            id: id.to_string(),
            started: started.to_string(),
            repos: vec!["acme/api".to_string()],
            status,
            origin: "cockpit".to_string(),
            remaining_parts: 3,
            estimate_usd: 0.5,
            resumable_here: matches!(status, SavedStatus::Interrupted | SavedStatus::Incomplete),
            reasons: Vec::new(),
        }
    }

    #[test]
    fn the_newest_unfinished_inspection_is_offered_and_the_others_are_listed() {
        let offer = offer(vec![
            row("a", "2026-10-27T10:00:00Z", SavedStatus::Incomplete),
            row("c", "2026-10-29T10:00:00Z", SavedStatus::Interrupted),
            row("b", "2026-10-28T10:00:00Z", SavedStatus::Complete),
            row("d", "2026-10-28T12:00:00Z", SavedStatus::Interrupted),
        ]);
        assert_eq!(offer.offered.as_ref().map(|r| r.id.as_str()), Some("c"));
        let listed: Vec<&str> = offer.others.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(listed, vec!["d", "a"], "newest first, no complete one");
    }

    #[test]
    fn a_running_inspection_is_listed_and_never_offered() {
        let offer = offer(vec![
            row("live", "2026-10-29T10:00:00Z", SavedStatus::Live),
            row("old", "2026-10-28T10:00:00Z", SavedStatus::Interrupted),
        ]);
        assert_eq!(offer.offered.as_ref().map(|r| r.id.as_str()), Some("old"));
        assert_eq!(offer.others.len(), 1);
        assert_eq!(offer.others[0].id, "live");
        assert!(offer.others[0].list_line().contains("running now"));

        let only_live = super::offer(vec![row("live", "2026-10-29T10:00:00Z", SavedStatus::Live)]);
        assert!(only_live.offered.is_none());
    }

    #[test]
    fn an_inspection_another_place_must_resume_is_listed_with_where() {
        let mut cli = row("cli-1", "2026-10-29T10:00:00Z", SavedStatus::Interrupted);
        cli.origin = "cli".to_string();
        cli.resumable_here = false;
        let offer = offer(vec![cli]);
        assert!(offer.offered.is_none(), "this screen cannot resume it");
        let line = offer.others[0].resume_elsewhere().expect("says where");
        assert!(line.contains("camerata inspect --resume cli-1"), "{line}");
    }

    #[test]
    fn nothing_saved_or_everything_complete_offers_nothing() {
        assert_eq!(offer(Vec::new()), Offer::default());
        let done = offer(vec![row(
            "x",
            "2026-10-29T10:00:00Z",
            SavedStatus::Complete,
        )]);
        assert_eq!(done, Offer::default());
    }

    #[test]
    fn the_words_say_interrupted_or_did_not_finish() {
        let interrupted = row("a", "t", SavedStatus::Interrupted);
        let incomplete = row("b", "t", SavedStatus::Incomplete);
        assert_eq!(interrupted.headline(), "This inspection was interrupted");
        assert_eq!(incomplete.headline(), "This inspection did not finish");
        let summary = interrupted.summary();
        assert!(summary.contains("3 parts remain"), "{summary}");
        assert!(summary.contains("$0.50"), "{summary}");
        assert!(!summary.contains('\u{2014}'), "{summary}");
        let mut one = interrupted.clone();
        one.remaining_parts = 1;
        assert!(one.summary().contains("1 part remain"), "{}", one.summary());
    }

    #[test]
    fn the_wire_body_parses_as_the_server_sends_it() {
        let body = serde_json::json!({
            "project_id": "p1",
            "inspections": [{
                "id": "20261029T161803Z-9c402a",
                "started": "2026-10-29T16:18:03Z",
                "repos": ["acme/one"],
                "status": "interrupted",
                "origin": "cockpit",
                "remaining_parts": 7,
                "estimate_usd": 0.12,
                "resumable_here": true,
                "reasons": []
            }]
        });
        let parsed: SavedInspections = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.inspections.len(), 1);
        assert_eq!(parsed.inspections[0].status, SavedStatus::Interrupted);
        assert!(parsed.inspections[0].resumable_here);
    }
}
