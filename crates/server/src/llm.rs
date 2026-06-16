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

/// Tools forbidden on the CLI path so it stays a PURE text completion (no MCP servers via
/// `--strict-mcp-config`, no sub-agents, no filesystem). The audit reasons over the digest
/// in the prompt; it must not wander.
const NO_TOOLS: &str = "Task Bash Read Edit Write MultiEdit Glob Grep WebFetch WebSearch \
                        NotebookEdit TodoWrite BashOutput KillShell";

// ── In-flight subprocess registry (shutdown hook) ──────────────────────────────
// `kill_on_drop(true)` reaps subprocesses when their future is dropped (graceful runtime
// shutdown, e.g. closing the window). But a SIGNAL-driven quit (Ctrl+C from `cargo run`,
// SIGTERM) terminates the process WITHOUT running drops, which would orphan any audit
// `claude` mid-flight. This registry lets the app's signal handler reap them explicitly.

/// PIDs of currently-running `claude` subprocesses.
fn inflight_claude() -> &'static std::sync::Mutex<std::collections::HashSet<u32>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u32>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// RAII guard: registers a child PID on construction, removes it on drop (every exit path
/// of the spawning function — normal completion, early bail, timeout).
struct ClaudePidGuard(u32);
impl ClaudePidGuard {
    fn new(pid: u32) -> Self {
        if let Ok(mut g) = inflight_claude().lock() {
            g.insert(pid);
        }
        Self(pid)
    }
}
impl Drop for ClaudePidGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = inflight_claude().lock() {
            g.remove(&self.0);
        }
    }
}

/// Kill every in-flight `claude` subprocess. Called from the app's shutdown signal handler
/// (see `serve`) so quitting never leaves audit subprocesses running. Killing an
/// already-exited PID is a harmless no-op.
pub fn kill_inflight_claude() {
    if let Ok(g) = inflight_claude().lock() {
        for &pid in g.iter() {
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
        }
    }
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// Run a completion, STREAMING text deltas to `on_delta` as they arrive (so the UI can
    /// show the model producing output instead of a blank panel). Falls back to a single
    /// `on_delta(full_text)` for the API path / non-Anthropic vendors.
    pub async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let model = self.model_for(&req);
        match (self.vendor, self.backend) {
            (Vendor::Anthropic, Backend::Cli) => {
                self.complete_cli_streaming(&req, &model, on_delta).await
            }
            (Vendor::Anthropic, Backend::Api) => {
                let r = self.complete_api(&req, &model).await?;
                on_delta(&r.text);
                Ok(r)
            }
            _ => {
                let r = self.complete(req).await?;
                on_delta(&r.text);
                Ok(r)
            }
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
        let mut cmd = tokio::process::Command::new("claude");
        // kill_on_drop: if the caller's future is dropped (e.g. a timeout fires), the
        // spawned `claude` subprocess is killed rather than orphaned and left running.
        // stdin(null): `claude -p` blocks on inherited piped stdin until EOF (see the
        // streaming path for the full explanation). `.output()` already nulls stdin, but
        // set it explicitly so the behavior doesn't depend on that detail.
        cmd.kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .arg("-p")
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

    /// CLI streaming path: `--output-format stream-json --include-partial-messages`. Reads
    /// stdout line-by-line, calls `on_delta` with each `content_block_delta` text chunk
    /// (so the UI shows the model writing), and captures the final `result` event.
    async fn complete_cli_streaming(
        &self,
        req: &LlmRequest,
        model: &str,
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        use tokio::io::AsyncBufReadExt;
        let mut cmd = tokio::process::Command::new("claude");
        // kill_on_drop: a dropped future (timeout) kills the subprocess instead of
        // leaving a stalled `claude` running in the background.
        // stdin(null): CRITICAL. `claude -p` reads piped stdin and blocks until EOF before
        // responding. spawn() INHERITS the parent's stdin, and the desktop app's inherited
        // stdin is an open pipe that never EOFs — so the audit hung for minutes producing
        // nothing (shell tests were fast only because their stdin was a TTY/closed).
        // Redirecting stdin to /dev/null gives an immediate EOF so claude uses the -p arg
        // and responds at once.
        cmd.kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .arg("-p")
            .arg(&req.prompt)
            .arg("--model")
            .arg(model)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--include-partial-messages")
            .arg("--strict-mcp-config")
            .arg("--disallowedTools")
            .arg(NO_TOOLS)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(system) = &req.system {
            cmd.arg("--append-system-prompt").arg(system);
        }
        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn `claude` CLI (is it installed/on PATH?): {e}")
        })?;
        // Track the PID for the shutdown hook; the guard untracks on every exit path.
        let _pid_guard = child.id().map(ClaudePidGuard::new);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("claude CLI produced no stdout pipe"))?;
        // CRITICAL: drain stderr CONCURRENTLY. `--verbose` makes `claude` write a lot to
        // stderr; if we only read stdout, stderr's OS pipe buffer (~64KB) fills, the
        // subprocess BLOCKS on its next stderr write, stops producing stdout, and this
        // read loop hangs forever (observed: an 8-minute hang after a few tokens). A
        // spawned reader keeps stderr flowing so stdout never stalls.
        let stderr_task = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut s = String::new();
                let _ = tokio::io::BufReader::new(stderr)
                    .read_to_string(&mut s)
                    .await;
                s
            })
        });
        let mut lines = tokio::io::BufReader::new(stdout).lines();

        // PROGRESS/STALL timeout, not a total-time cap: the wall-clock allowed since the
        // last sign of the model actually RESPONDING. It resets only on real progress
        // (message/content events), NOT on `system`/`status`/`rate_limit` keepalive lines —
        // those keep arriving while a call is queued/throttled BEFORE the first token, and
        // resetting on them was why a stalled call ran for minutes without tripping. A
        // legitimate large scan keeps emitting content and never trips; a call stuck before
        // (or mid) generation does. `CAMERATA_LLM_IDLE_SECS` overrides the 120s default.
        let idle = std::time::Duration::from_secs(
            std::env::var("CAMERATA_LLM_IDLE_SECS")
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(120),
        );

        let mut full = String::new();
        let mut cost = None;
        let mut deadline = tokio::time::Instant::now() + idle;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                // No model progress for the whole window -> a true stall (often a queued /
                // rate-limited call sitting before its first token). kill_on_drop reaps the
                // subprocess when this future is dropped on the error path.
                anyhow::bail!(
                    "claude produced no model output for {}s — treating as a hang (likely rate-limited/queued; set CAMERATA_LLM_IDLE_SECS to tune)",
                    idle.as_secs()
                );
            }
            let remaining = deadline - now;
            match tokio::time::timeout(remaining, lines.next_line()).await {
                Err(_) => anyhow::bail!(
                    "claude produced no model output for {}s — treating as a hang (likely rate-limited/queued; set CAMERATA_LLM_IDLE_SECS to tune)",
                    idle.as_secs()
                ),
                Ok(Ok(None)) => break, // EOF — the process finished
                Ok(Err(e)) => anyhow::bail!("reading claude stdout: {e}"),
                Ok(Ok(Some(line))) => {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    // Real model progress extends the deadline; status/keepalive lines do not.
                    let mut progressed = false;
                    match v["type"].as_str() {
                        Some("stream_event") => {
                            let ev = &v["event"];
                            // Any of these mean the model is actively responding.
                            if matches!(
                                ev["type"].as_str(),
                                Some(
                                    "message_start"
                                        | "content_block_start"
                                        | "content_block_delta"
                                        | "content_block_stop"
                                        | "message_delta"
                                )
                            ) {
                                progressed = true;
                            }
                            if ev["type"] == "content_block_delta"
                                && ev["delta"]["type"] == "text_delta"
                            {
                                if let Some(t) = ev["delta"]["text"].as_str() {
                                    full.push_str(t);
                                    on_delta(t);
                                }
                            }
                        }
                        Some("assistant") => progressed = true,
                        Some("result") => {
                            progressed = true;
                            if let Some(r) = v["result"].as_str() {
                                if !r.is_empty() {
                                    full = r.to_string();
                                }
                            }
                            cost = v["total_cost_usd"].as_f64();
                        }
                        _ => {}
                    }
                    if progressed {
                        deadline = tokio::time::Instant::now() + idle;
                    }
                }
            }
        }
        let status = child.wait().await?;
        let stderr_text = match stderr_task {
            Some(t) => t.await.unwrap_or_default(),
            None => String::new(),
        };
        // SALVAGE: if the process exited non-zero (e.g. SIGKILL from a timeout, or an
        // external kill) but it already streamed a usable response, RETURN that text rather
        // than discarding it. The model's work is in `full`; throwing it away on a late
        // interruption is what made a fully-streamed audit collapse to "0/3 findings".
        // Only fail when there's genuinely nothing to salvage.
        if !status.success() && full.trim().is_empty() {
            anyhow::bail!("claude CLI (stream) exited {status}: {}", stderr_text.trim());
        }
        Ok(LlmResponse {
            text: full,
            model: model.to_string(),
            backend: "cli".to_string(),
            cost_usd: cost,
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
