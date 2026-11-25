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
    /// Whether this screen can resume it: unfinished, started here and on this build.
    #[serde(default)]
    pub resumable_here: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
    /// The first 12 characters of the commit of the build it was started on; `None` when the
    /// run recorded none.
    #[serde(default)]
    pub started_on_build: Option<String>,
    /// The folder the build it was started on was made in, when recorded.
    #[serde(default)]
    pub built_from: Option<String>,
    /// Started on another build than this one: "Run what remains" is refused here (the build
    /// that starts an inspection must finish it).
    #[serde(default)]
    pub other_build: bool,
    /// The server's refusal for [`Self::other_build`], in plain words, naming the folder when
    /// it is known.
    #[serde(default)]
    pub build_refusal: Option<String>,
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
        if self.other_build {
            // Nothing of it carries over to this build: no cost of finishing it, no stored work.
            return format!(
                "Started {} on {repos}. {parts} {noun} remain.",
                self.started
            );
        }
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

    /// "Started on build 1a2b3c4d5e6f", for every unfinished inspection whose build is recorded.
    pub fn started_on_line(&self) -> Option<String> {
        if !self.is_unfinished() {
            return None;
        }
        self.started_on_build
            .as_deref()
            .map(|build| format!("Started on build {build}"))
    }

    /// The refusal shown in place of "Run what remains" for an inspection another build started.
    pub fn other_build_refusal(&self) -> Option<String> {
        (self.is_unfinished() && self.other_build).then(|| {
            self.build_refusal.clone().unwrap_or_else(|| {
                "This inspection was started on another build and cannot be continued on this \
                 build. The build that starts an inspection must finish it."
                    .to_string()
            })
        })
    }

    /// Whether this screen offers "Start again on this build": an unfinished cockpit inspection
    /// another build started (the app holds the request it was started with).
    pub fn can_start_again_here(&self) -> bool {
        self.is_unfinished() && self.other_build && self.origin == "cockpit"
    }

    /// For an unfinished inspection another place must resume, where that is.
    pub fn resume_elsewhere(&self) -> Option<String> {
        (self.is_unfinished() && !self.resumable_here && !self.other_build).then(|| {
            format!(
                "Started from the command line: resume it with camerata inspect --resume {}",
                self.id
            )
        })
    }
}

/// Decide what to show for `all` (any order). The newest unfinished inspection this screen can
/// resume, or start again on this build when another build started it, is offered; every other
/// unfinished or running one is listed, newest first. A live inspection is never offered.
pub fn offer(mut all: Vec<SavedInspection>) -> Offer {
    all.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    let offered_at = all
        .iter()
        .position(|r| r.is_unfinished() && (r.resumable_here || r.can_start_again_here()));
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
            started_on_build: Some("aaaaaaaaaaaa".to_string()),
            built_from: None,
            other_build: false,
            build_refusal: None,
        }
    }

    /// A cockpit row started on another build, as the server lists it.
    fn other_build_row(id: &str, started: &str) -> SavedInspection {
        SavedInspection {
            resumable_here: false,
            started_on_build: Some("1a2b3c4d5e6f".to_string()),
            built_from: Some("/Users/auditor/camerata-scan-acme".to_string()),
            other_build: true,
            build_refusal: Some(
                "This inspection was started on build 1a2b3c4d5e6f and cannot be continued on \
                 this build (aaaaaaaaaaaa). The build that starts an inspection must finish it."
                    .to_string(),
            ),
            ..row(id, started, SavedStatus::Interrupted)
        }
    }

    #[test]
    fn every_unfinished_row_says_which_build_started_it() {
        let interrupted = row("a", "t", SavedStatus::Interrupted);
        assert_eq!(
            interrupted.started_on_line().as_deref(),
            Some("Started on build aaaaaaaaaaaa")
        );
        assert_eq!(row("b", "t", SavedStatus::Complete).started_on_line(), None);
        let mut unknown = row("c", "t", SavedStatus::Incomplete);
        unknown.started_on_build = None;
        assert_eq!(unknown.started_on_line(), None);
    }

    /// An inspection another build started is still the one offered (it is the newest unfinished
    /// one), but to start again on this build, never to run what remains; it does not claim the
    /// work already paid for carries over, and it is not sent to the command line.
    #[test]
    fn an_inspection_another_build_started_is_offered_to_start_again_only() {
        let offer = offer(vec![
            row("old", "2026-10-27T10:00:00Z", SavedStatus::Incomplete),
            other_build_row("new", "2026-10-29T10:00:00Z"),
        ]);
        let offered = offer.offered.expect("the newest unfinished one is offered");
        assert_eq!(offered.id, "new");
        assert!(offered.can_start_again_here());
        assert!(!offered.resumable_here);
        assert!(
            !offered.summary().contains("not sent again"),
            "{}",
            offered.summary()
        );
        assert_eq!(offered.resume_elsewhere(), None);
        assert_eq!(offer.others.len(), 1);
        assert!(!offer.others[0].can_start_again_here());

        // One the command line started on another build: listed, and neither resumed nor
        // started again here.
        let mut cli = other_build_row("cli-1", "2026-10-29T10:00:00Z");
        cli.origin = "cli".to_string();
        assert!(!cli.can_start_again_here());
        let offer = super::offer(vec![cli]);
        assert!(offer.offered.is_none());
    }

    #[test]
    fn the_build_fields_parse_and_default_when_absent() {
        let parsed: SavedInspection = serde_json::from_value(serde_json::json!({
            "id": "r1", "status": "interrupted", "origin": "cockpit",
            "started_on_build": "1a2b3c4d5e6f", "built_from": "/Users/auditor/camerata-scan-acme",
            "other_build": true, "resumable_here": false,
            "build_refusal": "This inspection was started on build 1a2b3c4d5e6f"
        }))
        .unwrap();
        assert!(parsed.other_build);
        assert_eq!(parsed.started_on_build.as_deref(), Some("1a2b3c4d5e6f"));
        assert_eq!(
            parsed.built_from.as_deref(),
            Some("/Users/auditor/camerata-scan-acme")
        );
        let old: SavedInspection = serde_json::from_value(serde_json::json!({
            "id": "r1", "status": "interrupted"
        }))
        .unwrap();
        assert!(!old.other_build);
        assert_eq!(old.started_on_build, None);
        assert_eq!(old.build_refusal, None);
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
