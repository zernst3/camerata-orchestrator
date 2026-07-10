/// The kind of application shell the requirements ask for. `WebPwa` is the only
/// target the vetted skeleton fits — the other three are structural mismatches with
/// "a responsive, installable web PWA" and always route to
/// [`crate::ScaffoldStrategy::FromScratch`] (see [`crate::choose_strategy`]).
///
/// This is the STRUCTURED signal a caller sets deliberately (e.g. the orchestrator's
/// intake step classified the ask as desktop-only); the free-text
/// `DISQUALIFYING_SIGNALS` keyword scan in `crate::strategy` remains a SECONDARY
/// signal that still applies even to a `WebPwa` target (belt-and-suspenders: a
/// `summary` that slips in "native desktop app" despite `target: WebPwa` still gets
/// caught).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppTarget {
    /// A responsive, installable web PWA — the shape the vetted skeleton is built
    /// for. The default: most bespoke-app asks are this shape.
    #[default]
    WebPwa,
    /// A native desktop application shell (not a web PWA).
    Desktop,
    /// A native mobile application (iOS/Android).
    Mobile,
    /// A command-line interface / terminal application.
    Cli,
}

/// Whether a scaffolded app is reachable by anyone who has the URL, or hidden
/// behind a minimal shared-passcode lock by default (FOLD D — default-private
/// skeleton lock, `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`'s
/// usability backlog: "default-private deploy (single-user lock unless opted
/// public)").
///
/// The thesis: deploying your budget app should not put your finances on a public
/// URL. `Private` is the default for that reason — `Public` is always an explicit
/// opt-in the caller has to set, never the fallback of an unset/malformed field
/// (`#[serde(default)]` below resolves a missing `visibility` key to `Private`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// The default: the scaffolded skeleton ships with a minimal, dependency-free
    /// single-shared-passcode gate (`src/components/access_gate.rs` +
    /// `src/server_fns.rs`'s `verify_access_code`) wrapping the app's content, so
    /// nothing renders until the passcode configured in `APP_ACCESS_CODE` is
    /// entered. See [`crate::scaffold_skeleton`].
    #[default]
    Private,
    /// Explicit opt-in: the access-lock files are omitted entirely and the app
    /// renders directly. Never the default — a caller must set this deliberately.
    Public,
}

/// What the (future) orchestrator fills in to describe the app it wants built. This
/// is intentionally small: a display name, a description, a target platform, a
/// couple of coarse capability flags, and a free-text summary the human actually
/// typed (or said).
///
/// `needs_persistence` and `needs_auth` do NOT by themselves rule out the vetted
/// skeleton (see [`crate::choose_strategy`]'s doc comment for why) — they're read by
/// the *later* phases that layer a database or an auth module on top of this
/// skeleton, not by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct AppRequirements {
    /// Display name, e.g. "Trip Planner". Used verbatim in the PWA manifest, the
    /// HTML title, and the in-app header; also the seed for the derived snake_case
    /// package name (see [`AppRequirements::package_name`]).
    pub name: String,
    /// A one-line description. Used as the manifest/HTML `<meta description>` and
    /// the `Cargo.toml` package description.
    pub description: String,
    /// The kind of application shell requested. Defaults to [`AppTarget::WebPwa`]
    /// (the shape the vetted skeleton fits) — [`crate::choose_strategy`] routes
    /// anything else straight to [`crate::ScaffoldStrategy::FromScratch`].
    #[serde(default)]
    pub target: AppTarget,
    /// Whether the app's requirements imply a database. The base skeleton this
    /// crate emits never has one (DB-on-demand); a later phase adds persistence
    /// when this is `true`.
    #[serde(default)]
    pub needs_persistence: bool,
    /// Whether the app's requirements imply end-user login. The base skeleton ships
    /// with no auth by default; a later phase adds an auth module when this is
    /// `true`.
    #[serde(default)]
    pub needs_auth: bool,
    /// The free-text ask, roughly as the user phrased it (e.g. "an app that tracks
    /// my flights and lets me see them on a timeline"). [`crate::choose_strategy`]
    /// scans this (and `description`) for the small set of documented signals that
    /// genuinely don't fit a web PWA.
    #[serde(default)]
    pub summary: String,
    /// Override for the auto-capture reporter's POST target. `None` uses the
    /// default relative path `/api/feedback` (the app's own, not-yet-implemented —
    /// that's Part 2 — capture endpoint). A relative path is deliberate: it resolves
    /// against whatever origin actually serves the app (localhost in preview, the
    /// real domain once deployed) without the scaffolder needing to know that origin
    /// up front.
    #[serde(default)]
    pub capture_url: Option<String>,
    /// Whether the scaffolded app ships behind the default-private access lock, or
    /// is opted into being publicly reachable. Defaults to [`Visibility::Private`].
    #[serde(default)]
    pub visibility: Visibility,
}

impl AppRequirements {
    /// Derive the snake_case package/crate name substituted into the skeleton
    /// (`Cargo.toml`, `Dioxus.toml`, `src/main.rs`'s crate references).
    ///
    /// Lowercases, maps every run of non-alphanumeric characters to a single `_`,
    /// trims leading/trailing `_`, and prefixes `app_` if the result would
    /// otherwise start with a digit (an invalid Rust crate name). Falls back to
    /// `camerata_app` if `name` has no alphanumeric characters at all.
    pub fn package_name(&self) -> String {
        let mut out = String::new();
        let mut pending_sep = false;
        for ch in self.name.chars() {
            if ch.is_ascii_alphanumeric() {
                if pending_sep && !out.is_empty() {
                    out.push('_');
                }
                out.push(ch.to_ascii_lowercase());
                pending_sep = false;
            } else {
                pending_sep = true;
            }
        }
        if out.is_empty() {
            return "camerata_app".to_string();
        }
        if out.starts_with(|c: char| c.is_ascii_digit()) {
            out = format!("app_{out}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reqs(name: &str) -> AppRequirements {
        AppRequirements {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn simple_name_becomes_snake_case() {
        assert_eq!(reqs("Trip Planner").package_name(), "trip_planner");
    }

    #[test]
    fn punctuation_and_repeated_separators_collapse() {
        assert_eq!(
            reqs("Zach's  To-Do!! List").package_name(),
            "zach_s_to_do_list"
        );
    }

    #[test]
    fn leading_and_trailing_punctuation_is_trimmed() {
        assert_eq!(reqs("  Budget Mini  ").package_name(), "budget_mini");
        assert_eq!(reqs("--front--").package_name(), "front");
    }

    #[test]
    fn digit_leading_name_gets_prefixed() {
        assert_eq!(reqs("3D Printer Queue").package_name(), "app_3d_printer_queue");
    }

    #[test]
    fn empty_or_non_alphanumeric_name_falls_back() {
        assert_eq!(reqs("").package_name(), "camerata_app");
        assert_eq!(reqs("!!!").package_name(), "camerata_app");
    }

    #[test]
    fn already_snake_case_is_unchanged() {
        assert_eq!(reqs("already_snake_case").package_name(), "already_snake_case");
    }

    // ── Visibility (FOLD D: default-private skeleton lock) ──────────────────────

    #[test]
    fn default_visibility_is_private() {
        assert_eq!(Visibility::default(), Visibility::Private);
        assert_eq!(AppRequirements::default().visibility, Visibility::Private);
    }

    #[test]
    fn visibility_serde_round_trips_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Visibility::Private).unwrap(),
            "\"private\""
        );
        assert_eq!(
            serde_json::to_string(&Visibility::Public).unwrap(),
            "\"public\""
        );
        let parsed: Visibility = serde_json::from_str("\"public\"").unwrap();
        assert_eq!(parsed, Visibility::Public);
    }

    #[test]
    fn missing_visibility_field_deserializes_to_private() {
        // A minimal/older JSON payload that omits `visibility` entirely must default
        // to Private (never silently Public) — the whole point of FOLD D.
        let reqs: AppRequirements =
            serde_json::from_str(r#"{"name":"Budget","description":"Track spending."}"#).unwrap();
        assert_eq!(reqs.visibility, Visibility::Private);
    }
}
