use crate::AppRequirements;

/// Which path the scaffolder takes for a given app's requirements.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ScaffoldStrategy {
    /// The requirements fit the vetted Dioxus-fullstack PWA skeleton
    /// (`crate::scaffold_skeleton`). This is the common case.
    Skeleton,
    /// The requirements have a genuine incompatibility with the skeleton's
    /// assumptions (see the fit criteria on [`choose_strategy`]). `reason` names it
    /// in one sentence.
    ///
    /// This crate does not implement a from-scratch generator — per the
    /// skeleton-first decision (design doc, "Decisions already made"), that is a
    /// defined entry point the future orchestrator fulfills. Returning this variant
    /// is the honest signal that the skeleton path does not apply here, not a stub
    /// that silently falls back to Skeleton.
    FromScratch { reason: String },
}

/// A phrase that, if it appears in the requirements' free text, means the vetted web
/// PWA skeleton genuinely cannot express what's being asked — paired with the
/// human-readable capability the requirements are actually asking for.
///
/// This list is deliberately small and conservative (recall over precision is the
/// wrong call here, unlike detection routines: a false "incompatible" just costs an
/// unnecessary escalation to a from-scratch build, but a false "fits" would silently
/// hand the orchestrator a skeleton that structurally cannot do what was asked). Each
/// entry is a genuine structural mismatch with "a responsive, installable web PWA",
/// not a feature the skeleton merely doesn't have YET (persistence and auth are
/// deliberately NOT here — see `choose_strategy`'s doc comment).
const DISQUALIFYING_SIGNALS: &[(&str, &str)] = &[
    ("desktop app", "a native desktop application shell, not a web PWA"),
    ("desktop-only", "a native desktop application shell, not a web PWA"),
    ("native desktop", "a native desktop application shell, not a web PWA"),
    ("ios app", "a native iOS mobile application"),
    ("android app", "a native Android mobile application"),
    ("native mobile", "a native mobile application"),
    ("cli tool", "a command-line interface, not a web UI"),
    ("command-line tool", "a command-line interface, not a web UI"),
    ("command line interface", "a command-line interface, not a web UI"),
    ("terminal application", "a terminal UI, not a web UI"),
    (
        "browser extension",
        "a browser-extension manifest/runtime, not a standalone web app",
    ),
    (
        "chrome extension",
        "a browser-extension manifest/runtime, not a standalone web app",
    ),
    (
        "embedded system",
        "an embedded/firmware target, not a web runtime",
    ),
    ("microcontroller", "an embedded/firmware target, not a web runtime"),
    ("iot device", "an embedded/firmware target, not a web runtime"),
    (
        "system tray",
        "an OS-level background/tray process, not a web page",
    ),
    (
        "background daemon",
        "an OS-level background process with no UI, not a web page",
    ),
];

/// The fit-check: does `reqs` fit the vetted Dioxus-fullstack PWA skeleton, or does
/// it need a from-scratch build?
///
/// **Fit criteria.** The skeleton fits a "standard web app" — anything deliverable
/// as a responsive, installable web PWA reachable through server functions, which
/// covers the large majority of bespoke apps (the budget/itinerary references this
/// crate's skeleton is grounded in are both this shape). Two things do NOT
/// disqualify Skeleton, by design:
/// - `needs_persistence: true` — the base skeleton has no database on purpose
///   (DB-on-demand); a database is layered on top by a later phase, not by this
///   crate. The skeleton is still the right foundation.
/// - `needs_auth: true` — likewise layered on top later; the base skeleton ships
///   with no end-user auth by default.
///
/// What DOES disqualify Skeleton: the requirements' free text (`summary` +
/// `description`) mentioning a genuine structural incompatibility with "a web PWA" —
/// a native desktop/mobile shell, a CLI/terminal app, a browser extension, an
/// embedded/firmware target, or a background-only process with no page to render.
/// See [`DISQUALIFYING_SIGNALS`] for the exact list.
///
/// Everything else returns [`ScaffoldStrategy::Skeleton`].
pub fn choose_strategy(reqs: &AppRequirements) -> ScaffoldStrategy {
    let haystack = format!("{} {}", reqs.summary, reqs.description).to_lowercase();

    for (signal, capability) in DISQUALIFYING_SIGNALS {
        if haystack.contains(signal) {
            return ScaffoldStrategy::FromScratch {
                reason: format!(
                    "Requirements mention \"{signal}\", which needs {capability} — the vetted web PWA skeleton can't express that."
                ),
            };
        }
    }

    ScaffoldStrategy::Skeleton
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reqs(summary: &str) -> AppRequirements {
        AppRequirements {
            name: "Test App".to_string(),
            summary: summary.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn ordinary_web_app_requirements_fit_the_skeleton() {
        let r = reqs("an app that tracks my flights and shows them on a timeline");
        assert_eq!(choose_strategy(&r), ScaffoldStrategy::Skeleton);
    }

    #[test]
    fn persistence_and_auth_flags_do_not_disqualify_the_skeleton() {
        let mut r = reqs("a budget tracker with categories and monthly totals");
        r.needs_persistence = true;
        r.needs_auth = true;
        assert_eq!(choose_strategy(&r), ScaffoldStrategy::Skeleton);
    }

    #[test]
    fn native_desktop_ask_is_from_scratch() {
        let r = reqs("I want a native desktop app for managing my music library");
        match choose_strategy(&r) {
            ScaffoldStrategy::FromScratch { reason } => {
                assert!(reason.contains("desktop app"), "reason was: {reason}");
            }
            other => panic!("expected FromScratch, got {other:?}"),
        }
    }

    #[test]
    fn cli_tool_ask_is_from_scratch() {
        let r = reqs("build me a CLI tool for renaming files in bulk");
        match choose_strategy(&r) {
            ScaffoldStrategy::FromScratch { reason } => {
                assert!(reason.contains("cli tool"), "reason was: {reason}");
            }
            other => panic!("expected FromScratch, got {other:?}"),
        }
    }

    #[test]
    fn disqualifying_signal_match_is_case_insensitive() {
        let r = reqs("Ship this as a Native Desktop app please");
        assert!(matches!(
            choose_strategy(&r),
            ScaffoldStrategy::FromScratch { .. }
        ));
    }

    #[test]
    fn disqualifying_signal_in_description_is_also_caught() {
        let mut r = reqs("");
        r.description = "an Android app for tracking workouts".to_string();
        assert!(matches!(
            choose_strategy(&r),
            ScaffoldStrategy::FromScratch { .. }
        ));
    }
}
