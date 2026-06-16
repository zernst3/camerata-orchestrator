//! The LLM provider seam: every AI step in Camerata (the brownfield audit, story
//! investigation, clarification authoring, routine-prompt authoring, the research
//! chat) calls a model through ONE type with two interchangeable backends:
//!
//! - **ClaudeCli** — shells out to the `claude` CLI. This is the LOCAL path: a human
//!   driving the app uses their own CLI login, no API key, no separate billing.
//! - **AnthropicApi** — calls the Anthropic API with a key. This is the PRODUCTION
//!   path: a multi-user product can't ride one person's CLI session.
//!
//! Both ship out of the box; the backend is selected by env
//! (`CAMERATA_LLM_BACKEND=cli|api`, default `cli`) and the model by
//! `CAMERATA_LLM_MODEL`. Per-call overrides let the research chat pick a model.

use serde::Serialize;

/// The models the UI offers in a selector (label, model id). Latest/most capable first.
pub const MODELS: &[(&str, &str)] = &[
    ("Opus 4.8", "claude-opus-4-8"),
    ("Sonnet 4.6", "claude-sonnet-4-6"),
    ("Haiku 4.5", "claude-haiku-4-5-20251001"),
    ("Fable 5", "claude-fable-5"),
];

/// The default model when none is configured / requested. Capable by default; override
/// per call or via `CAMERATA_LLM_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// One completion request.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Model id (e.g. `claude-opus-4-8`). Empty -> the provider's default.
    pub model: String,
    /// Optional system prompt.
    pub system: Option<String>,
    /// The user prompt.
    pub prompt: String,
    /// Token ceiling for the response (API path; the CLI manages its own).
    pub max_tokens: u32,
}

impl LlmRequest {
    /// A plain request with default token ceiling.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            model: String::new(),
            system: None,
            prompt: prompt.into(),
            max_tokens: 4096,
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

/// A completion result.
#[derive(Debug, Clone, Serialize)]
pub struct LlmResponse {
    pub text: String,
    pub model: String,
    /// `cli` | `api` — which backend served it (surfaced honestly in the UI).
    pub backend: String,
    /// Cost in USD when the backend reports it (CLI does; API would need accounting).
    pub cost_usd: Option<f64>,
}

/// Which backend, resolved from env. Pure so it's unit-testable without real calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Cli,
    Api,
}

/// Decide the backend from the (optional) explicit preference and whether an API key is
/// present. Explicit `api` wins when a key exists; explicit `cli` always wins; with no
/// preference we default to the CLI (the local-human path) and only auto-pick the API
/// when a key is set AND the CLI isn't the stated choice.
pub fn select_backend(pref: Option<&str>, has_api_key: bool) -> Backend {
    match pref.map(|p| p.trim().to_ascii_lowercase()).as_deref() {
        Some("api") if has_api_key => Backend::Api,
        Some("api") => Backend::Cli, // asked for API but no key -> fall back, never silently fail hard
        Some("cli") => Backend::Cli,
        _ => Backend::Cli,
    }
}

/// The configured provider.
#[derive(Debug, Clone)]
pub struct Llm {
    backend: Backend,
    default_model: String,
    api_key: Option<String>,
}

impl Llm {
    /// Build from env: `CAMERATA_LLM_BACKEND` (cli|api, default cli),
    /// `ANTHROPIC_API_KEY` (required for api), `CAMERATA_LLM_MODEL` (default model).
    pub fn from_env() -> Self {
        let pref = std::env::var("CAMERATA_LLM_BACKEND").ok();
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());
        let backend = select_backend(pref.as_deref(), api_key.is_some());
        let default_model = std::env::var("CAMERATA_LLM_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self {
            backend,
            default_model,
            api_key,
        }
    }

    pub fn backend_label(&self) -> &'static str {
        match self.backend {
            Backend::Cli => "cli",
            Backend::Api => "api",
        }
    }

    fn model_for(&self, req: &LlmRequest) -> String {
        if req.model.trim().is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        }
    }

    /// Run a completion through the selected backend.
    pub async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let model = self.model_for(&req);
        match self.backend {
            Backend::Cli => self.complete_cli(&req, &model).await,
            Backend::Api => self.complete_api(&req, &model).await,
        }
    }

    /// CLI path: `claude -p <prompt> --model <model> --output-format json`, with the
    /// system prompt appended when present. No MCP / tools — a plain completion.
    async fn complete_cli(&self, req: &LlmRequest, model: &str) -> anyhow::Result<LlmResponse> {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--model")
            .arg(model)
            .arg("--output-format")
            .arg("json");
        if let Some(system) = &req.system {
            cmd.arg("--append-system-prompt").arg(system);
        }
        let out = cmd.output().await.map_err(|e| {
            anyhow::anyhow!("failed to spawn `claude` CLI (is it installed/on PATH?): {e}")
        })?;
        if !out.status.success() {
            anyhow::bail!(
                "claude CLI exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| anyhow::anyhow!("parse claude CLI JSON: {e}"))?;
        Ok(LlmResponse {
            text: v["result"].as_str().unwrap_or_default().to_string(),
            model: model.to_string(),
            backend: "cli".to_string(),
            cost_usd: v["total_cost_usd"].as_f64(),
        })
    }

    /// API path: POST the Anthropic Messages API with the key.
    async fn complete_api(&self, req: &LlmRequest, model: &str) -> anyhow::Result<LlmResponse> {
        let key = self
            .api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("API backend selected but ANTHROPIC_API_KEY is unset"))?;
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "messages": [{ "role": "user", "content": req.prompt }],
        });
        if let Some(system) = &req.system {
            body["system"] = serde_json::json!(system);
        }
        let resp = reqwest::Client::new()
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Anthropic API request failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Anthropic API HTTP {status}: {text}");
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse API JSON: {e}"))?;
        // content is an array of blocks; concatenate the text blocks.
        let out = v["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        Ok(LlmResponse {
            text: out,
            model: model.to_string(),
            backend: "api".to_string(),
            cost_usd: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_selection_rules() {
        // Explicit api with a key -> api.
        assert_eq!(select_backend(Some("api"), true), Backend::Api);
        // Explicit api WITHOUT a key -> falls back to cli (never hard-fail silently).
        assert_eq!(select_backend(Some("api"), false), Backend::Cli);
        // Explicit cli -> cli regardless of key.
        assert_eq!(select_backend(Some("cli"), true), Backend::Cli);
        // No preference -> cli (the local-human default).
        assert_eq!(select_backend(None, true), Backend::Cli);
        assert_eq!(select_backend(None, false), Backend::Cli);
        // Case / whitespace tolerant.
        assert_eq!(select_backend(Some(" API "), true), Backend::Api);
    }

    #[test]
    fn model_defaulting() {
        let llm = Llm {
            backend: Backend::Cli,
            default_model: "claude-sonnet-4-6".to_string(),
            api_key: None,
        };
        // Empty request model -> default.
        assert_eq!(llm.model_for(&LlmRequest::new("hi")), "claude-sonnet-4-6");
        // Explicit model wins.
        assert_eq!(
            llm.model_for(&LlmRequest::new("hi").with_model("claude-opus-4-8")),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn request_builder() {
        let r = LlmRequest::new("do it")
            .with_system("be terse")
            .with_model("claude-opus-4-8")
            .with_max_tokens(100);
        assert_eq!(r.prompt, "do it");
        assert_eq!(r.system.as_deref(), Some("be terse"));
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.max_tokens, 100);
    }

    #[test]
    fn models_list_has_known_ids() {
        assert!(MODELS.iter().any(|(_, id)| *id == "claude-opus-4-8"));
        assert!(MODELS.iter().any(|(_, id)| *id == DEFAULT_MODEL));
    }
}
