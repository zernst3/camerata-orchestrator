//! The in-chatbox model selector's data shapes + option-group building, extracted from the chat UI.
//!
//! These are BFF view types (deserialized from `GET /api/models/registry`) plus the pure function that
//! turns them into the selector's option groups. Fields are `pub` because the Dioxus adapter renders
//! them across the crate boundary. No rendering-framework dependency; unit-tested here.

/// One model the selector offers (sourced from `GET /api/models/registry`).
#[derive(Clone, PartialEq, serde::Deserialize)]
pub struct ModelOption {
    pub label: String,
    pub id: String,
    /// Provider key: "claude" | "openrouter". Used for `<optgroup>` grouping.
    #[serde(default)]
    pub provider: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
pub struct ModelsResp {
    pub models: Vec<ModelOption>,
    #[serde(default)]
    pub default: String,
    /// Not returned by the registry endpoint; kept for graceful zero-value.
    #[serde(default)]
    pub backend: String,
}

impl ModelsResp {
    /// Return models grouped by provider for `<optgroup>` rendering.
    pub fn grouped(&self) -> Vec<(&'static str, Vec<&ModelOption>)> {
        let claude: Vec<&ModelOption> =
            self.models.iter().filter(|m| m.provider == "claude").collect();
        let openrouter: Vec<&ModelOption> =
            self.models.iter().filter(|m| m.provider == "openrouter").collect();
        let mut groups = Vec::new();
        if !claude.is_empty() {
            groups.push(("Claude (subscription)", claude));
        }
        if !openrouter.is_empty() {
            groups.push(("OpenRouter", openrouter));
        }
        // If provider isn't set on any entry (shouldn't happen but safe fallback),
        // render them all without grouping under a generic header.
        if groups.is_empty() && !self.models.is_empty() {
            groups.push(("Models", self.models.iter().collect()));
        }
        groups
    }
}

/// Build the option groups for the in-chatbox model selector. This ALWAYS returns at least one
/// option, so the `<select>` can never render empty/invisible — the recurring "the chat model
/// selector disappeared" regression. When the model registry has not loaded yet (resource `None`)
/// or comes back empty, it falls back to the currently-selected model id as a single self-named
/// option, so the selector stays visible and usable. Pure + owned strings so it is unit-tested
/// without a Dioxus runtime (see `chat_model_groups_*` tests). Whenever the header rsx changes,
/// those tests keep the selector from silently vanishing again.
pub fn chat_model_groups(
    models: &Option<ModelsResp>,
    current: &str,
) -> Vec<(String, Vec<ModelOption>)> {
    if let Some(m) = models {
        let grouped = m.grouped();
        if !grouped.is_empty() {
            return grouped
                .into_iter()
                .map(|(label, opts)| (label.to_string(), opts.into_iter().cloned().collect()))
                .collect();
        }
    }
    // Registry absent or empty: never leave the selector without an option.
    let id = if current.trim().is_empty() {
        "default".to_string()
    } else {
        current.to_string()
    };
    vec![(
        "Current".to_string(),
        vec![ModelOption {
            label: id.clone(),
            id,
            provider: String::new(),
        }],
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── in-chatbox model selector: it must NEVER render empty/invisible ────────
    // (this selector has regressed away repeatedly; these guard the option-building logic. Moved
    // verbatim from camerata-ui; now unit-tested with no VirtualDom.)

    fn opt(id: &str, provider: &str) -> ModelOption {
        ModelOption {
            label: id.to_string(),
            id: id.to_string(),
            provider: provider.to_string(),
        }
    }

    fn total_options(groups: &[(String, Vec<ModelOption>)]) -> usize {
        groups.iter().map(|(_, o)| o.len()).sum()
    }

    #[test]
    fn chat_model_groups_never_empty_when_registry_not_loaded() {
        // Resource None (still loading or fetch failed): the selector must still show the current
        // model, never render with zero options (which reads as "the selector disappeared").
        let groups = chat_model_groups(&None, "claude-opus-4-8");
        assert!(total_options(&groups) >= 1, "selector must always have an option");
        assert!(groups
            .iter()
            .any(|(_, o)| o.iter().any(|m| m.id == "claude-opus-4-8")));
    }

    #[test]
    fn chat_model_groups_never_empty_when_registry_is_empty() {
        let empty = ModelsResp {
            models: vec![],
            default: String::new(),
            backend: String::new(),
        };
        let groups = chat_model_groups(&Some(empty), "sonnet-x");
        assert!(total_options(&groups) >= 1);
        assert!(groups.iter().any(|(_, o)| o.iter().any(|m| m.id == "sonnet-x")));
    }

    #[test]
    fn chat_model_groups_falls_back_to_default_when_current_is_blank() {
        // Even with no current model and no registry, offer a placeholder so the <select> renders.
        let groups = chat_model_groups(&None, "");
        assert!(total_options(&groups) >= 1);
    }

    #[test]
    fn chat_model_groups_uses_registry_when_present() {
        let resp = ModelsResp {
            models: vec![opt("claude-opus-4-8", "claude"), opt("gpt-x", "openrouter")],
            default: "claude-opus-4-8".into(),
            backend: String::new(),
        };
        let groups = chat_model_groups(&Some(resp), "claude-opus-4-8");
        let ids: Vec<String> = groups
            .iter()
            .flat_map(|(_, o)| o.iter().map(|m| m.id.clone()))
            .collect();
        assert!(ids.contains(&"claude-opus-4-8".to_string()));
        assert!(ids.contains(&"gpt-x".to_string()));
        // Two providers -> two groups (not the single "Current" fallback).
        assert!(groups.len() >= 2, "registry models should be grouped by provider");
    }

    // ── ModelsResp::grouped: provider partitioning for <optgroup> ─────────────

    fn models_resp_from(json: &str) -> ModelsResp {
        serde_json::from_str(json).expect("valid ModelsResp json")
    }

    #[test]
    fn grouped_partitions_claude_and_openrouter_in_order() {
        let resp = models_resp_from(
            r#"{"models":[
                {"label":"Opus","id":"opus","provider":"claude"},
                {"label":"DeepSeek","id":"ds","provider":"openrouter"},
                {"label":"Sonnet","id":"sonnet","provider":"claude"}
            ]}"#,
        );
        let groups = resp.grouped();
        assert_eq!(groups.len(), 2);
        // Claude group comes first and holds both claude entries.
        assert_eq!(groups[0].0, "Claude (subscription)");
        assert_eq!(groups[0].1.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["opus", "sonnet"]);
        // OpenRouter group second.
        assert_eq!(groups[1].0, "OpenRouter");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn grouped_omits_an_empty_provider_group() {
        let resp = models_resp_from(
            r#"{"models":[{"label":"Opus","id":"opus","provider":"claude"}]}"#,
        );
        let groups = resp.grouped();
        // Only the Claude group is present; no empty OpenRouter optgroup.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Claude (subscription)");
    }

    #[test]
    fn grouped_falls_back_to_generic_header_when_provider_unset() {
        // Entries with no provider must still render under a generic "Models" header
        // rather than vanishing.
        let resp = models_resp_from(
            r#"{"models":[{"label":"Mystery","id":"m"}]}"#,
        );
        let groups = resp.grouped();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Models");
        assert_eq!(groups[0].1.len(), 1);
    }

    #[test]
    fn grouped_empty_models_yields_no_groups() {
        let resp = models_resp_from(r#"{"models":[]}"#);
        assert!(resp.grouped().is_empty());
    }
}
