//! LLM backend toggle: pure view logic for the CLI ⟷ API selector. No rendering-framework
//! dependency (RUST-HEADLESS-CORE-1); unit-tested here. The Dioxus adapter (`camerata-ui`)
//! renders this state and owns the HTTP.
//!
//! The app runs the fleet through one of two backends:
//! - **CLI** — spawns the logged-in Claude Code CLI (`claude -p`), using the user's existing
//!   subscription. No API key required.
//! - **API** — the Anthropic Messages API, which requires `ANTHROPIC_API_KEY`.
//!
//! The server's selection logic falls back to CLI whenever `api` is chosen but no key is
//! present, so the UI must warn (not block) in that case. This module derives the two view
//! facts the control needs: which segment is active, and whether to show the no-key warning.

/// The selectable LLM backends. `parse` collapses anything unrecognised to `Cli` — the safe
/// default the server also uses (matching its `select_backend` fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmBackend {
    /// Spawn the logged-in Claude Code CLI. No API key needed.
    Cli,
    /// Anthropic Messages API. Requires `ANTHROPIC_API_KEY`.
    Api,
}

impl LlmBackend {
    /// Parse the server's `llm_backend` string (case-insensitive, trimmed). `"api"` maps to
    /// [`LlmBackend::Api`]; everything else — including `"cli"`, blanks, and unknown values —
    /// collapses to [`LlmBackend::Cli`], the safe default.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "api" => LlmBackend::Api,
            _ => LlmBackend::Cli,
        }
    }

    /// The wire value (`"cli"` | `"api"`) to POST to `/api/settings/llm-backend`.
    pub fn as_wire(self) -> &'static str {
        match self {
            LlmBackend::Cli => "cli",
            LlmBackend::Api => "api",
        }
    }

    /// The human label for the segmented control.
    pub fn label(self) -> &'static str {
        match self {
            LlmBackend::Cli => "CLI",
            LlmBackend::Api => "API",
        }
    }
}

/// Whether the no-key warning should show: only when the API backend is selected AND no
/// Anthropic API key is present. In that state the server silently falls back to CLI, so the
/// UI must surface it. CLI never needs a key, so it never warns.
pub fn show_api_key_warning(selected: LlmBackend, api_key_present: bool) -> bool {
    selected == LlmBackend::Api && !api_key_present
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_maps_api_case_insensitively_and_defaults_to_cli() {
        assert_eq!(LlmBackend::parse("api"), LlmBackend::Api);
        assert_eq!(LlmBackend::parse("  API "), LlmBackend::Api);
        assert_eq!(LlmBackend::parse("cli"), LlmBackend::Cli);
        // Unknown / blank collapse to the safe default.
        assert_eq!(LlmBackend::parse(""), LlmBackend::Cli);
        assert_eq!(LlmBackend::parse("gemini"), LlmBackend::Cli);
    }

    #[test]
    fn wire_and_label_round_trip() {
        assert_eq!(LlmBackend::Cli.as_wire(), "cli");
        assert_eq!(LlmBackend::Api.as_wire(), "api");
        assert_eq!(LlmBackend::Cli.label(), "CLI");
        assert_eq!(LlmBackend::Api.label(), "API");
        // Wire value re-parses to the same variant.
        assert_eq!(LlmBackend::parse(LlmBackend::Api.as_wire()), LlmBackend::Api);
        assert_eq!(LlmBackend::parse(LlmBackend::Cli.as_wire()), LlmBackend::Cli);
    }

    #[test]
    fn warning_only_when_api_selected_without_key() {
        // API + no key -> warn (server falls back to CLI).
        assert!(show_api_key_warning(LlmBackend::Api, false));
        // API + key -> no warning.
        assert!(!show_api_key_warning(LlmBackend::Api, true));
        // CLI never warns, key present or not.
        assert!(!show_api_key_warning(LlmBackend::Cli, false));
        assert!(!show_api_key_warning(LlmBackend::Cli, true));
    }
}
