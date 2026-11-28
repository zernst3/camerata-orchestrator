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
        let claude: Vec<&ModelOption> = self
            .models
            .iter()
            .filter(|m| m.provider == "claude")
            .collect();
        let openrouter: Vec<&ModelOption> = self
            .models
            .iter()
            .filter(|m| m.provider == "openrouter")
            .collect();
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

/// True when `id` is one of the registry's models. The only ids the chat selector may persist:
/// a placeholder ("default", blank) or a model of an unloaded registry is never saved.
pub fn is_registry_model(models: &Option<ModelsResp>, id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && models
            .as_ref()
            .is_some_and(|m| m.models.iter().any(|o| o.id == id))
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
    // The fallback is never the word "default": with no real current model it is an id-less
    // "Loading models" option that the change handler must not save.
    let cur = current.trim();
    let option = if cur.is_empty() || cur.eq_ignore_ascii_case("default") {
        ModelOption {
            label: "Loading models\u{2026}".to_string(),
            id: String::new(),
            provider: String::new(),
        }
    } else {
        ModelOption {
            label: cur.to_string(),
            id: cur.to_string(),
            provider: String::new(),
        }
    };
    vec![("Current".to_string(), vec![option])]
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
        let groups = chat_model_groups(&None, "claude-opus-5");
        assert!(
            total_options(&groups) >= 1,
            "selector must always have an option"
        );
        assert!(groups
            .iter()
            .any(|(_, o)| o.iter().any(|m| m.id == "claude-opus-5")));
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
        assert!(groups
            .iter()
            .any(|(_, o)| o.iter().any(|m| m.id == "sonnet-x")));
    }

    #[test]
    fn chat_model_groups_falls_back_to_default_when_current_is_blank() {
        // Even with no current model and no registry, offer a placeholder so the <select> renders.
        let groups = chat_model_groups(&None, "");
        assert!(total_options(&groups) >= 1);
    }

    #[test]
    fn chat_model_groups_fallback_is_labelled_loading_not_default() {
        // The registry has not loaded and no real model is chosen: the one fallback option must
        // say it is loading, carry NO id (so it can never be mistaken for a model), and never be
        // the self-named word "default".
        for current in ["", "default", " Default "] {
            let groups = chat_model_groups(&None, current);
            let all: Vec<&ModelOption> = groups.iter().flat_map(|(_, o)| o.iter()).collect();
            assert_eq!(all.len(), 1, "current {current:?}");
            assert_eq!(all[0].id, "", "current {current:?}");
            assert!(all[0].label.to_lowercase().contains("loading"));
            assert!(!all[0].label.eq_ignore_ascii_case("default"));
        }
    }

    #[test]
    fn is_registry_model_only_accepts_ids_in_the_registry() {
        let resp = ModelsResp {
            models: vec![opt("claude-opus-5", "claude")],
            default: String::new(),
            backend: String::new(),
        };
        let some = Some(resp);
        assert!(is_registry_model(&some, "claude-opus-5"));
        assert!(!is_registry_model(&some, "default"));
        assert!(!is_registry_model(&some, ""));
        assert!(!is_registry_model(&None, "claude-opus-5"));
    }

    #[test]
    fn chat_model_groups_uses_registry_when_present() {
        let resp = ModelsResp {
            models: vec![opt("claude-opus-5", "claude"), opt("gpt-x", "openrouter")],
            default: "claude-opus-5".into(),
            backend: String::new(),
        };
        let groups = chat_model_groups(&Some(resp), "claude-opus-5");
        let ids: Vec<String> = groups
            .iter()
            .flat_map(|(_, o)| o.iter().map(|m| m.id.clone()))
            .collect();
        assert!(ids.contains(&"claude-opus-5".to_string()));
        assert!(ids.contains(&"gpt-x".to_string()));
        // Two providers -> two groups (not the single "Current" fallback).
        assert!(
            groups.len() >= 2,
            "registry models should be grouped by provider"
        );
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
        assert_eq!(
            groups[0]
                .1
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["opus", "sonnet"]
        );
        // OpenRouter group second.
        assert_eq!(groups[1].0, "OpenRouter");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn grouped_omits_an_empty_provider_group() {
        let resp =
            models_resp_from(r#"{"models":[{"label":"Opus","id":"opus","provider":"claude"}]}"#);
        let groups = resp.grouped();
        // Only the Claude group is present; no empty OpenRouter optgroup.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Claude (subscription)");
    }

    #[test]
    fn grouped_falls_back_to_generic_header_when_provider_unset() {
        // Entries with no provider must still render under a generic "Models" header
        // rather than vanishing.
        let resp = models_resp_from(r#"{"models":[{"label":"Mystery","id":"m"}]}"#);
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
