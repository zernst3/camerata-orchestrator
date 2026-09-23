//! Backend view logic: pure view logic for the CLI ⟷ API selector, shared by the per-project
//! **Backend** control and the global **Chat backend** toggle
//! (`docs/design/2026-09-22_per-project-backend.md`). No rendering-framework dependency
//! (RUST-HEADLESS-CORE-1); unit-tested here. The Dioxus adapter (`camerata-ui`) renders this
//! state and owns the HTTP.
//!
//! There are exactly two backend SETTINGS in the app, both using the same wire type
//! (`camerata_api_types::project::ProjectBackend`):
//! - **Per-project** (`Project.backend`) — the source of truth for every project-scoped model
//!   call: the scan, the alternative-recommendation pass, the disagreement rescan, and the
//!   governed dev loop. Set via `POST /api/projects/:id/backend`.
//! - **Chat** (`settings.chat_backend`) — a single global setting used ONLY by the
//!   project-less chat assistant. Set via `POST /api/settings/chat-backend`.
//!
//! Both resolve the same way: `Cli` spawns the logged-in Claude Code CLI (no API key needed);
//! `Api` calls the Anthropic Messages API and requires `ANTHROPIC_API_KEY`. A project (or the
//! chat) set to `Api` with no key configured is BLOCKED, not silently downgraded to `Cli` — so
//! the UI must warn (not block interaction) when that combination is selected.

use camerata_api_types::project::ProjectBackend;

/// View-logic helpers for [`ProjectBackend`] the Dioxus components need: lenient parsing of a
/// wire string, the wire value to POST, and the human label for a segmented control. An
/// extension trait rather than inherent methods because `ProjectBackend` is defined in the
/// pure-serde `camerata-api-types` leaf, which this crate doesn't own.
pub trait ProjectBackendExt: Sized + Copy {
    /// Parse a backend string (case-insensitive, trimmed). `"api"` maps to
    /// [`ProjectBackend::Api`]; everything else — including `"cli"`, blanks, and unknown
    /// values — collapses to [`ProjectBackend::Cli`], the safe default (matching the server's
    /// own `Default` impl and its `resolve_backend` fallback).
    fn parse_lenient(s: &str) -> Self;

    /// The wire value (`"cli"` | `"api"`) to POST.
    fn as_wire(self) -> &'static str;

    /// The human label for a segmented control.
    fn label(self) -> &'static str;
}

impl ProjectBackendExt for ProjectBackend {
    fn parse_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "api" => ProjectBackend::Api,
            _ => ProjectBackend::Cli,
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            ProjectBackend::Cli => "cli",
            ProjectBackend::Api => "api",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ProjectBackend::Cli => "CLI",
            ProjectBackend::Api => "API",
        }
    }
}

/// Whether the no-key warning should show: only when the API backend is selected AND no
/// Anthropic API key is present. In that state a project (or the chat) set to `Api` is BLOCKED
/// outright (no silent CLI fallback), so the UI must surface it plainly. `Cli` never needs a
/// key, so it never warns.
pub fn show_api_key_warning(selected: ProjectBackend, api_key_present: bool) -> bool {
    selected == ProjectBackend::Api && !api_key_present
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_maps_api_case_insensitively_and_defaults_to_cli() {
        assert_eq!(ProjectBackend::parse_lenient("api"), ProjectBackend::Api);
        assert_eq!(ProjectBackend::parse_lenient("  API "), ProjectBackend::Api);
        assert_eq!(ProjectBackend::parse_lenient("cli"), ProjectBackend::Cli);
        // Unknown / blank collapse to the safe default.
        assert_eq!(ProjectBackend::parse_lenient(""), ProjectBackend::Cli);
        assert_eq!(ProjectBackend::parse_lenient("gemini"), ProjectBackend::Cli);
    }

    #[test]
    fn wire_and_label_round_trip() {
        assert_eq!(ProjectBackend::Cli.as_wire(), "cli");
        assert_eq!(ProjectBackend::Api.as_wire(), "api");
        assert_eq!(ProjectBackend::Cli.label(), "CLI");
        assert_eq!(ProjectBackend::Api.label(), "API");
        // Wire value re-parses to the same variant.
        assert_eq!(ProjectBackend::parse_lenient(ProjectBackend::Api.as_wire()), ProjectBackend::Api);
        assert_eq!(ProjectBackend::parse_lenient(ProjectBackend::Cli.as_wire()), ProjectBackend::Cli);
    }

    #[test]
    fn warning_only_when_api_selected_without_key() {
        // API + no key -> warn (the server blocks the call rather than falling back to CLI).
        assert!(show_api_key_warning(ProjectBackend::Api, false));
        // API + key -> no warning.
        assert!(!show_api_key_warning(ProjectBackend::Api, true));
        // CLI never warns, key present or not.
        assert!(!show_api_key_warning(ProjectBackend::Cli, false));
        assert!(!show_api_key_warning(ProjectBackend::Cli, true));
    }

    #[test]
    fn project_backend_default_and_serde_roundtrip_is_cli() {
        // The shared type's Default AND serde default both land on Cli — the zero-setup
        // subscription path a fresh project (or absent legacy field) should always resolve to.
        assert_eq!(ProjectBackend::default(), ProjectBackend::Cli);
        let v: ProjectBackend = serde_json::from_value(serde_json::json!("cli")).unwrap();
        assert_eq!(v, ProjectBackend::Cli);
        let v: ProjectBackend = serde_json::from_value(serde_json::json!("api")).unwrap();
        assert_eq!(v, ProjectBackend::Api);
    }
}
