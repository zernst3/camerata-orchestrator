//! Pure fold of a [`PreviewEvent`] stream into a [`PreviewStatus`].
//!
//! State machine: `Starting -> Serving{url} -> Building{url} -> Serving{url}/BuildFailed{url,summary}`.
//! `url` rides along through `Building`/`BuildFailed` as the LAST KNOWN good URL (once one
//! has been seen) so a caller can always show "here's where the preview last worked" even
//! mid-rebuild or after a failure, rather than losing it the moment a `Building` event fires.

use serde::{Deserialize, Serialize};

use crate::parser::PreviewEvent;

/// The folded state of a preview session. See the module docs for the transition diagram.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PreviewStatus {
    /// No build has completed and no URL is known yet (dx just spawned, or a config error
    /// — e.g. the spike's Dioxus.toml schema mismatch, Q1 Blocker 1 — hit before any build).
    Starting,
    /// Serving `url`; the last (or only) build succeeded.
    Serving { url: String },
    /// A full rebuild is in flight. `url` is the last known good one, if any is known yet.
    Building { url: Option<String> },
    /// The most recent build failed. `url` (if any) is still the last known GOOD preview —
    /// dx keeps serving the last-good build on a compile error (Q3 #3: "dx SURVIVES"), so the
    /// caller can legitimately keep pointing a live-open tab at it while showing `summary`.
    BuildFailed { url: Option<String>, summary: String },
}

impl PreviewStatus {
    /// The last known-good URL, if any has been observed yet (`None` only in `Starting`, or
    /// in `Building`/`BuildFailed` before the first-ever successful build).
    pub fn known_url(&self) -> Option<&str> {
        match self {
            PreviewStatus::Starting => None,
            PreviewStatus::Serving { url } => Some(url),
            PreviewStatus::Building { url } | PreviewStatus::BuildFailed { url, .. } => url.as_deref(),
        }
    }
}

/// Fold one [`PreviewEvent`] onto the current [`PreviewStatus`]. Pure: no I/O, no clock.
pub fn fold(status: &PreviewStatus, event: &PreviewEvent) -> PreviewStatus {
    match event {
        PreviewEvent::Serving { url } => PreviewStatus::Serving { url: url.clone() },

        // The ~1s hot-patch fast path never leaves "serving" conceptually (Q2: no rebuild,
        // no cargo/rustc child spawned) -- it just confirms the current URL is still live.
        // From Building/BuildFailed a Hotreload event (e.g. the recovery sequence in Q3 #4)
        // means dx accepted a new edit without a full rebuild; treat that as back-to-serving.
        PreviewEvent::Hotreload { .. } => match status.known_url() {
            Some(url) => PreviewStatus::Serving { url: url.to_string() },
            None => status.clone(),
        },

        PreviewEvent::RebuildStarted => PreviewStatus::Building { url: status.known_url().map(str::to_string) },

        PreviewEvent::BuildOk { .. } => match status.known_url() {
            Some(url) => PreviewStatus::Serving { url: url.to_string() },
            // A first-ever successful build with no URL seen yet: still "serving" in the
            // sense that matters (the build succeeded, the last-known-good exists), just
            // without a confirmed URL. Model as Building{url:None}->caller can't render a
            // link yet; this only happens if a real Serving/URL event never arrives, which
            // PreviewServer covers by constructing its own URL from the configured port
            // rather than depending on this fold path (see process.rs).
            None => PreviewStatus::Building { url: None },
        },

        PreviewEvent::BuildFailed { summary } => {
            PreviewStatus::BuildFailed { url: status.known_url().map(str::to_string), summary: summary.clone() }
        }

        // Unknown/noise doesn't change the folded status.
        PreviewEvent::Unknown => status.clone(),
    }
}

/// Fold a whole event sequence starting from [`PreviewStatus::Starting`]. Test/inspection
/// convenience; `PreviewServer` folds incrementally as lines arrive instead.
pub fn fold_all(events: &[PreviewEvent]) -> PreviewStatus {
    events.iter().fold(PreviewStatus::Starting, |acc, ev| fold(&acc, ev))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn starting_to_serving_on_serving_event() {
        let next = fold(&PreviewStatus::Starting, &PreviewEvent::Serving { url: url("http://127.0.0.1:8080/") });
        assert_eq!(next, PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") });
    }

    #[test]
    fn serving_stays_serving_on_hotreload() {
        let serving = PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") };
        let next = fold(&serving, &PreviewEvent::Hotreload { path: Some("/src/frontend/timeline.rs".into()) });
        assert_eq!(next, serving);
    }

    #[test]
    fn serving_to_building_on_rebuild_started() {
        let serving = PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") };
        let next = fold(&serving, &PreviewEvent::RebuildStarted);
        assert_eq!(next, PreviewStatus::Building { url: Some(url("http://127.0.0.1:8080/")) });
    }

    #[test]
    fn building_to_serving_on_build_ok() {
        let building = PreviewStatus::Building { url: Some(url("http://127.0.0.1:8080/")) };
        let next = fold(&building, &PreviewEvent::BuildOk { secs: Some(4.85) });
        assert_eq!(next, PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") });
    }

    #[test]
    fn building_to_build_failed_preserves_last_known_url() {
        let building = PreviewStatus::Building { url: Some(url("http://127.0.0.1:8080/")) };
        let next = fold(&building, &PreviewEvent::BuildFailed { summary: "cannot find crate `reqwest`".into() });
        assert_eq!(
            next,
            PreviewStatus::BuildFailed { url: Some(url("http://127.0.0.1:8080/")), summary: "cannot find crate `reqwest`".into() }
        );
    }

    #[test]
    fn build_failed_before_any_known_url_still_surfaces_with_no_url() {
        // Q1 Blocker 1: the Dioxus.toml schema mismatch fails before any build ever
        // completes. The failure must not be dropped just because there's no URL yet.
        let next = fold(&PreviewStatus::Starting, &PreviewEvent::BuildFailed { summary: "missing field `dev`".into() });
        assert_eq!(next, PreviewStatus::BuildFailed { url: None, summary: "missing field `dev`".into() });
    }

    #[test]
    fn build_failed_then_recovery_returns_to_serving() {
        // Q3 #4 "Recovery is real": after a silently-ignored/failed edit, a subsequent valid
        // edit triggers a normal rebuild that succeeds.
        let failed = PreviewStatus::BuildFailed { url: Some(url("http://127.0.0.1:8080/")), summary: "boom".into() };
        let rebuilding = fold(&failed, &PreviewEvent::RebuildStarted);
        assert_eq!(rebuilding, PreviewStatus::Building { url: Some(url("http://127.0.0.1:8080/")) });
        let recovered = fold(&rebuilding, &PreviewEvent::BuildOk { secs: Some(3.65) });
        assert_eq!(recovered, PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") });
    }

    #[test]
    fn unknown_event_does_not_change_status() {
        let serving = PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") };
        assert_eq!(fold(&serving, &PreviewEvent::Unknown), serving);
    }

    #[test]
    fn fold_all_replays_a_full_session_sequence() {
        // First launch (Q1/Q3 #2): Building -> BuildOk with no URL known yet (fold-only path;
        // in practice PreviewServer seeds the URL from its own --port, see process.rs), then a
        // real edit triggers RebuildStarted -> BuildFailed, then a fixed edit recovers.
        let events = vec![
            PreviewEvent::RebuildStarted,
            PreviewEvent::BuildOk { secs: Some(19.26) },
            PreviewEvent::Serving { url: url("http://127.0.0.1:8080/") },
            PreviewEvent::Hotreload { path: Some("timeline.rs".into()) },
            PreviewEvent::RebuildStarted,
            PreviewEvent::BuildFailed { summary: "cannot find crate `reqwest`".into() },
            PreviewEvent::RebuildStarted,
            PreviewEvent::BuildOk { secs: Some(3.65) },
        ];
        let status = fold_all(&events);
        assert_eq!(status, PreviewStatus::Serving { url: url("http://127.0.0.1:8080/") });
    }
}
