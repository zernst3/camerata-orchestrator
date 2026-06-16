//! The LLM provider seam: every AI step in Camerata (the brownfield audit, story
//! investigation, clarification authoring, routine-prompt authoring, the research
//! chat) calls a model through ONE vendor-agnostic type.
//!
//! **Agent-agnostic by design.** Camerata HAPPENS to ship with Anthropic wired, but the
//! end state is vendor-neutral: a user picks whatever model they want — Anthropic,
//! OpenAI, Google, others. The request/response shapes here ([`LlmRequest`] /
//! [`LlmResponse`]) are deliberately vendor-neutral; a new vendor is a new match arm in
//! [`Llm::complete`] plus its entries in [`MODELS`], NOT a rewrite. Today only Anthropic
//! is implemented; the other vendors are reserved knobs that return a clear
//! "not wired yet" message pointing at the seam.
//!
//! Two axes:
//! - **Vendor** (`CAMERATA_LLM_VENDOR`, default `anthropic`) — which provider.
//! - **Transport** (`CAMERATA_LLM_BACKEND`, default `cli`) — for a vendor that offers
//!   both: `cli` shells the vendor's CLI (the LOCAL path: a human's own login, no key);
//!   `api` calls the vendor's HTTP API with a key (the PRODUCTION / multi-user path).
//!   Anthropic offers both; other vendors are API-only.
//!
//! Model selected by `CAMERATA_LLM_MODEL`, overridable per call (the research chat).

use serde::Serialize;

/// One model the UI offers, tagged with its vendor so the selector can group/extend.
pub struct ModelInfo {
    pub vendor: &'static str,
    pub label: &'static str,
    pub id: &'static str,
}

/// The models the UI offers. Anthropic today; add a vendor's models here when its arm
/// is wired in [`Llm::complete`]. Latest/most capable first.
pub const MODELS: &[ModelInfo] = &[
    ModelInfo { vendor: "anthropic", label: "Opus 4.8", id: "claude-opus-4-8" },
    ModelInfo { vendor: "anthropic", label: "Sonnet 4.6", id: "claude-sonnet-4-6" },
    ModelInfo { vendor: "anthropic", label: "Haiku 4.5", id: "claude-haiku-4-5-20251001" },
    ModelInfo { vendor: "anthropic", label: "Fable 5", id: "claude-fable-5" },
];

/// The model vendors Camerata knows about. Only `Anthropic` is wired today; the rest are
/// reserved so the env knob + extension point are explicit (selecting them returns a
/// clear "not wired yet" message rather than silently falling back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Anthropic,
    OpenAi,
    Google,
}

impl Vendor {
    /// Parse the `CAMERATA_LLM_VENDOR` value; unknown / empty -> Anthropic (the default).
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("openai") => Vendor::OpenAi,
            Some("google" | "gemini") => Vendor::Google,
            _ => Vendor::Anthropic,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Vendor::Anthropic => "anthropic",
            Vendor::OpenAi => "openai",
            Vendor::Google => "google",
        }
    }
}

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

/// The configured provider: a vendor + transport + model.
#[derive(Debug, Clone)]
pub struct Llm {
    vendor: Vendor,
    backend: Backend,
    default_model: String,
    api_key: Option<String>,
}

impl Llm {
    /// Build from env: `CAMERATA_LLM_VENDOR` (default anthropic), `CAMERATA_LLM_BACKEND`
    /// (cli|api, default cli), `ANTHROPIC_API_KEY` (for the Anthropic api transport),
    /// `CAMERATA_LLM_MODEL` (default model).
    pub fn from_env() -> Self {
        let vendor = Vendor::parse(std::env::var("CAMERATA_LLM_VENDOR").ok().as_deref());
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
            vendor,
            backend,
            default_model,
            api_key,
        }
    }

    /// A short label for the active provider, e.g. `anthropic/cli` — shown honestly in
    /// the UI so the user knows which vendor + transport is serving.
    pub fn backend_label(&self) -> String {
        let t = match self.backend {
            Backend::Cli => "cli",
            Backend::Api => "api",
        };
        format!("{}/{t}", self.vendor.label())
    }

    fn model_for(&self, req: &LlmRequest) -> String {
        if req.model.trim().is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        }
    }

    /// Run a completion through the selected vendor + transport. Adding a vendor is a new
    /// match arm here plus its [`MODELS`] entries; the request/response shapes don't change.
    pub async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let model = self.model_for(&req);
        match self.vendor {
            Vendor::Anthropic => match self.backend {
                Backend::Cli => self.complete_cli(&req, &model).await,
                Backend::Api => self.complete_api(&req, &model).await,
            },
            Vendor::OpenAi | Vendor::Google => anyhow::bail!(
                "model vendor `{}` is not wired yet — the provider seam is ready (add an \
                 arm in llm.rs::complete + its MODELS entries). Set CAMERATA_LLM_VENDOR=anthropic \
                 to use the wired vendor.",
                self.vendor.label()
            ),
        }
    }

    /// CLI path: a PURE text completion, not an agentic loop. The audit hands the model
    /// the whole code digest in the prompt, so it must NOT load MCP servers (the GitHub
    /// MCP), spawn sub-agents (Task/Explore), or touch the filesystem — that was wasteful,
    /// slow, and muddied the output. `--strict-mcp-config` with no `--mcp-config` loads no
    /// MCP servers; `--disallowedTools` forbids every built-in so the model can only
    /// reason over the prompt and answer.
    async fn complete_cli(&self, req: &LlmRequest, model: &str) -> anyhow::Result<LlmResponse> {
        const NO_TOOLS: &str =
            "Task Bash Read Edit Write MultiEdit Glob Grep WebFetch WebSearch NotebookEdit \
             TodoWrite BashOutput KillShell";
        let mut cmd = tokio::process::Command::new("claude");
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--model")
            .arg(model)
            .arg("--output-format")
            .arg("json")
            .arg("--strict-mcp-config")
            .arg("--disallowedTools")
            .arg(NO_TOOLS);
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
            vendor: Vendor::Anthropic,
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
        assert!(MODELS.iter().any(|m| m.id == "claude-opus-4-8"));
        assert!(MODELS.iter().any(|m| m.id == DEFAULT_MODEL));
        // Every model is tagged with a vendor (the agent-agnostic axis).
        assert!(MODELS.iter().all(|m| !m.vendor.is_empty()));
    }

    #[test]
    fn vendor_parsing_defaults_to_anthropic() {
        assert_eq!(Vendor::parse(Some("openai")), Vendor::OpenAi);
        assert_eq!(Vendor::parse(Some("Google")), Vendor::Google);
        assert_eq!(Vendor::parse(Some("gemini")), Vendor::Google);
        assert_eq!(Vendor::parse(Some("anthropic")), Vendor::Anthropic);
        assert_eq!(Vendor::parse(None), Vendor::Anthropic);
        assert_eq!(Vendor::parse(Some("nonsense")), Vendor::Anthropic);
    }

    #[tokio::test]
    async fn unwired_vendor_fails_clearly_without_calling_out() {
        let llm = Llm {
            vendor: Vendor::OpenAi,
            backend: Backend::Api,
            default_model: "gpt".to_string(),
            api_key: None,
        };
        let err = llm.complete(LlmRequest::new("hi")).await.unwrap_err();
        assert!(err.to_string().contains("not wired yet"));
    }
}
