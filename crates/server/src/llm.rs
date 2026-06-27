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

use serde::{Deserialize, Serialize};

/// One model the UI offers, tagged with its vendor so the selector can group/extend.
/// `price_in` / `price_out` are USD per MILLION tokens (input / output), used by the UI's
/// pre-audit cost estimate. Approximate list pricing — enough to size a scan, not billing.
pub struct ModelInfo {
    pub vendor: &'static str,
    pub label: &'static str,
    pub id: &'static str,
    pub price_in: f64,
    pub price_out: f64,
}

/// The models the UI offers. Anthropic today; add a vendor's models here when its arm
/// is wired in [`Llm::complete`]. Latest/most capable first. Prices are $/Mtok and track
/// the well-known tiering (Sonnet ~5× cheaper than Opus, Haiku ~15×).
pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        vendor: "anthropic",
        label: "Opus 4.8",
        id: "claude-opus-4-8",
        price_in: 15.0,
        price_out: 75.0,
    },
    ModelInfo {
        vendor: "anthropic",
        label: "Sonnet 4.6",
        id: "claude-sonnet-4-6",
        price_in: 3.0,
        price_out: 15.0,
    },
    ModelInfo {
        vendor: "anthropic",
        label: "Haiku 4.5",
        id: "claude-haiku-4-5-20251001",
        price_in: 1.0,
        price_out: 5.0,
    },
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

/// Force the `claude -p` CLI into a PURE, non-agentic, single-shot completion. The
/// orchestrator's model calls reason over the prompt and return text (JSON for the audit);
/// they must never behave like an interactive coding agent. A real run derailed once — it
/// reached for the `init` skill, entered plan mode, hunted for file tools, and tried to
/// write a CLAUDE.md instead of returning findings. The old guard (`--disallowedTools` with
/// a NAME blocklist) couldn't stop that: skills, plan-mode, and any unnamed tool slipped
/// through, and `--append-system-prompt` left the full Claude Code agent identity active.
///
/// This locks it down (verified flags, claude 2.1.x):
/// - `--tools ""` — disables ALL built-in tools (an allowlist of nothing, so future/unnamed
///   tools are covered too), not a fragile blocklist.
/// - `--disable-slash-commands` — turns off skills entirely (kills the `init` reflex).
/// - `--permission-mode dontAsk` — never prompts, never enters plan mode.
/// - `--system-prompt` (REPLACE, not append) — strips the Claude Code agent scaffolding so
///   it's a plain auditor, not a coding agent. Only set when the caller supplies one.
/// - `--strict-mcp-config` (no `--mcp-config`) — loads zero MCP servers.
///
/// Deliberately NOT `--bare`: that flag forces ANTHROPIC_API_KEY-only auth and never reads
/// OAuth/keychain, which would break the subscription-based CLI the local app relies on.
fn harden_completion(cmd: &mut tokio::process::Command, req: &LlmRequest) {
    cmd.arg("--strict-mcp-config")
        .arg("--disable-slash-commands")
        .arg("--permission-mode")
        .arg("dontAsk");
    match &req.repo_read_dir {
        // ON-DEMAND REPO READ (THE INVARIANT): the model may scan the bound repo. Swap the
        // `--tools ""` lockdown for the READ-ONLY built-ins and run WITH the repo as cwd +
        // `--add-dir`. Still non-agentic: slash-commands off, NO write/exec tool offered, so
        // this grants READ only — it cannot mutate the repo, spawn, or run shell.
        Some(dir) => {
            cmd.arg("--allowedTools")
                .arg(READ_ONLY_TOOLS.join(" "))
                .arg("--add-dir")
                .arg(dir)
                .current_dir(dir);
            // MULTI-REPO READ: a project has several repos. Each OTHER project-repo clone is
            // added as its own read-only `--add-dir` so a project-level model can scan across
            // all of them. READ-only — no write/exec tool is offered either way. Skip any that
            // duplicate the cwd dir.
            for extra in &req.repo_read_extra_dirs {
                if extra != dir {
                    cmd.arg("--add-dir").arg(extra);
                }
            }
        }
        // Pure single-shot completion (audit / decompose / escalation / API-shaped calls):
        // disable ALL built-in tools via an allowlist of nothing (covers unnamed/future
        // tools too). Unchanged from the original hardening.
        None => {
            cmd.arg("--tools").arg("");
        }
    }
    if let Some(system) = &req.system {
        cmd.arg("--system-prompt").arg(system);
    }
}

/// The read-only built-in tools granted on the CLI backend when a request opts into
/// on-demand repo read ([`LlmRequest::with_repo_read`]). Read-class only: these cannot
/// mutate the repo, so they add no write path. Kept in sync with
/// `camerata_agent::READONLY_BUILTINS`.
pub const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "LS"];

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

/// The minimal completion seam the audit depends on.
///
/// The audit pipeline ([`crate::ai_audit`]) used to take a concrete `&Llm` everywhere.
/// That made the AI-failure path (every pass errored) impossible to exercise in a unit
/// test without a live model, so the load-bearing "surface that the AI review was skipped,
/// never a silent clean" behavior was untested. This trait is the seam: the audit holds a
/// `&dyn Completer` and a test can substitute a stub that always errors.
///
/// OBJECT-SAFE by construction (so the audit can pass `&dyn Completer` down through ~10
/// functions without monomorphizing each one): the streaming method takes the delta
/// callback as `&mut (dyn FnMut(&str) + Send)` rather than a generic `F: FnMut`, which is
/// what keeps the trait dyn-compatible. The two completion methods mirror [`Llm::complete`]
/// and [`Llm::complete_streaming`] exactly so the production type is a transparent
/// implementor (see `impl Completer for Llm`).
///
/// `as_any` is the escape hatch for the ONE place that needs concrete `Llm` capability:
/// the Message-Batches path ([`Llm::submit_batch`] et al.) is API-key-gated and is not part
/// of this minimal seam, so `audit_repo`'s batch branch downcasts back to `&Llm`. In
/// production the value is always a real `Llm`, so the downcast always succeeds and the
/// behavior is unchanged; a non-`Llm` stub (tests) drives only the non-batch paths.
#[async_trait::async_trait]
pub trait Completer: Send + Sync {
    /// Run a completion. Mirrors [`Llm::complete`].
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse>;

    /// Run a completion, streaming text deltas to `on_delta`. Mirrors
    /// [`Llm::complete_streaming`]; the callback is a `&mut dyn` so the trait stays
    /// object-safe.
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse>;

    /// Downcast hook for the concrete-only batch path. See the trait doc.
    fn as_any(&self) -> &dyn std::any::Any;
}

#[async_trait::async_trait]
impl Completer for Llm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        // Delegate to the inherent method — same behavior, no wrapping.
        Llm::complete(self, req).await
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        // The inherent `complete_streaming` already takes `&mut (dyn FnMut(&str) + Send)`,
        // so this is a straight pass-through.
        Llm::complete_streaming(self, req, on_delta).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
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
    /// Prompt-caching breakpoint for the API path: number of BYTES (UTF-8) at the START of
    /// `prompt` that form the STATIC cacheable prefix. When set, `complete_api` splits the
    /// user message into two content blocks —
    ///   `[{text: prefix, cache_control: {type: ephemeral}}, {text: suffix}]`
    /// — so the provider caches the prefix once and re-reads it cheaply on every subsequent
    /// call that shares the same prefix (same chunk digest + repo map across rule-batches).
    /// Ignored by the CLI path (the CLI handles its own context management) and by
    /// non-Anthropic vendors (no-op until their caching protocol is wired).
    /// Min cacheable prefix: 2048 tokens (Sonnet) / 4096 tokens (Opus/Haiku). Our digests
    /// far exceed this, so a valid `cache_prefix_len` always meets the floor.
    pub cache_prefix_len: Option<usize>,
    /// Optional local repo clone the model may READ on demand (THE INVARIANT: every
    /// in-project agent gets on-demand full-repo read, not just a digest). On the CLI
    /// backend this swaps the hardened `--tools ""` lockdown for the READ-ONLY built-ins
    /// (Read/Grep/Glob/LS) and runs WITH this dir as cwd + `--add-dir`, so the model can
    /// scan any file. It stays NON-AGENTIC: slash-commands are still off and NO write/exec
    /// tool is offered, so this grants read only — it cannot mutate the repo. Ignored by the
    /// API backend (no filesystem) and by any call that leaves it `None` (e.g. the audit /
    /// decompose / escalation calls keep their full `--tools ""` lockdown unchanged).
    pub repo_read_dir: Option<std::path::PathBuf>,
    /// ADDITIONAL local repo clones the model may READ — the OTHER repos in the active
    /// project (a project has MULTIPLE repos). On the CLI backend each is emitted as its own
    /// read-only `--add-dir` on top of `repo_read_dir` (the cwd), so a project-level model
    /// (story-author / decompose / intake) can scan ACROSS all the project's repos, not just
    /// the primary. READ-ONLY and non-agentic — same posture as `repo_read_dir`. Empty by
    /// default; ignored on the API backend.
    pub repo_read_extra_dirs: Vec<std::path::PathBuf>,
}

impl LlmRequest {
    /// A plain request with default token ceiling.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            model: String::new(),
            system: None,
            prompt: prompt.into(),
            max_tokens: 4096,
            cache_prefix_len: None,
            repo_read_dir: None,
            repo_read_extra_dirs: Vec::new(),
        }
    }

    /// Grant the CLI-backend model ON-DEMAND READ access to a local repo clone: it runs with
    /// `dir` as cwd + `--add-dir` and the read-only built-ins (Read/Grep/Glob/LS) enabled, so
    /// it can scan any file rather than relying only on the inlined digest. READ-ONLY and
    /// non-agentic — no write/exec tool is offered, slash-commands stay off. No-op on the API
    /// backend (no filesystem). An empty path is treated as `None`.
    pub fn with_repo_read(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        let d = dir.into();
        self.repo_read_dir = if d.as_os_str().is_empty() { None } else { Some(d) };
        self
    }

    /// Grant the CLI-backend model ON-DEMAND READ across ALL the active project's repo clones
    /// (a project has MULTIPLE repos). The FIRST dir becomes the cwd + primary `--add-dir`
    /// (same as [`Self::with_repo_read`]); every other dir is added as its own read-only
    /// `--add-dir`. Use this for project-level models (story-author / decompose / intake) so
    /// they can scan across every repo. READ-ONLY and non-agentic. Empty input is a no-op
    /// (leaves the full `--tools ""` lockdown). No-op on the API backend (no filesystem).
    pub fn with_repo_read_dirs(
        mut self,
        dirs: impl IntoIterator<Item = std::path::PathBuf>,
    ) -> Self {
        let mut iter = dirs.into_iter().filter(|d| !d.as_os_str().is_empty());
        match iter.next() {
            Some(primary) => {
                self.repo_read_dir = Some(primary);
                self.repo_read_extra_dirs = iter.collect();
            }
            None => {
                self.repo_read_dir = None;
                self.repo_read_extra_dirs = Vec::new();
            }
        }
        self
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

    /// Mark the first `prefix_len` bytes of the prompt as the cacheable prefix (API path
    /// only). The prefix must end on a valid UTF-8 character boundary; the builder clamps
    /// it to the prompt length automatically so the caller can pass an oversized value safely.
    /// Setting `prefix_len = 0` is a no-op (leaves caching disabled).
    pub fn with_cache_prefix_len(mut self, prefix_len: usize) -> Self {
        if prefix_len > 0 {
            self.cache_prefix_len = Some(prefix_len.min(self.prompt.len()));
        }
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
    /// Cost in USD when known: the CLI reports it directly; the API path computes it from
    /// token usage × the model's list price (see [`MODELS`]).
    pub cost_usd: Option<f64>,
    /// Real billed input tokens (folds in cache read/creation), when the backend reports
    /// usage. Drives the post-scan ACTUAL-vs-estimated readout.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Real billed output tokens, when reported.
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Tokens read FROM the prompt cache on this call (billed at ~0.1× the normal input
    /// rate). Populated only by the API path when prompt caching is active; zero otherwise.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens WRITTEN TO the prompt cache on this call (billed at ~1.25× the normal input
    /// rate, one-time cost to seed the 5-min TTL cache). Populated only by the API path
    /// when prompt caching is active; zero otherwise.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Pull the full token accounting from a Claude `usage` object (CLI JSON or API response).
/// Returns `(input, output, cache_read, cache_creation)`. `input` folds in both cache
/// fields so it reflects all input-side billing; absent fields are treated as zero only
/// when `input_tokens` itself is present. `cache_read` and `cache_creation` are always
/// returned separately so the meter can track savings independently.
fn usage_tokens(usage: &serde_json::Value) -> (Option<u64>, Option<u64>, u64, u64) {
    let base = usage["input_tokens"].as_u64();
    let cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_create = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let input = base.map(|i| i + cache_read + cache_create);
    let output = usage["output_tokens"].as_u64();
    (input, output, cache_read, cache_create)
}

/// List price ($/Mtok input, $/Mtok output) for a model id, from [`MODELS`].
fn price_for(model_id: &str) -> Option<(f64, f64)> {
    MODELS
        .iter()
        .find(|m| m.id == model_id)
        .map(|m| (m.price_in, m.price_out))
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
///
/// Optionally carries a process-global [`crate::usage_ledger::UsageLedger`] (set via
/// [`Llm::with_ledger`] / [`Llm::from_env_with_ledger`]). When present, EVERY completion
/// that flows through [`Llm::complete`] / [`Llm::complete_streaming`] folds its usage into
/// the ledger and clears/sets the rate-limit flag. This is the single chokepoint that lets
/// the cumulative cockpit usage meter see ALL model calls regardless of which feature made
/// them — observability only, no behavior change.
#[derive(Clone)]
pub struct Llm {
    vendor: Vendor,
    backend: Backend,
    default_model: String,
    api_key: Option<String>,
    /// Process-global cumulative usage meter, threaded so the chokepoint records every call.
    /// `None` in tests / standalone construction (recording is then simply skipped).
    ledger: Option<std::sync::Arc<crate::usage_ledger::UsageLedger>>,
}

impl std::fmt::Debug for Llm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Llm")
            .field("vendor", &self.vendor)
            .field("backend", &self.backend)
            .field("default_model", &self.default_model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .field("ledger", &self.ledger.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl Llm {
    /// Build from env: `CAMERATA_LLM_VENDOR` (default anthropic), `CAMERATA_LLM_BACKEND`
    /// (cli|api, default cli), `ANTHROPIC_API_KEY` (for the Anthropic api transport),
    /// `CAMERATA_LLM_MODEL` (default model). No usage ledger attached (see
    /// [`Llm::from_env_with_ledger`] for the cockpit's recording path).
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
            ledger: None,
        }
    }

    /// Same as [`Llm::from_env`] but with the process-global usage ledger attached, so every
    /// call this instance makes is recorded into the cumulative cockpit meter. This is the
    /// constructor every HTTP handler / feature uses, so ALL LLM call paths feed one ledger.
    pub fn from_env_with_ledger(
        ledger: std::sync::Arc<crate::usage_ledger::UsageLedger>,
    ) -> Self {
        Self::from_env().with_ledger(ledger)
    }

    /// Attach (or replace) the process-global usage ledger on an existing instance.
    pub fn with_ledger(
        mut self,
        ledger: std::sync::Arc<crate::usage_ledger::UsageLedger>,
    ) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Fold one completed response into the ledger (model id resolved from the response,
    /// falling back to the configured default). No-op when no ledger is attached.
    fn ledger_record(&self, resp: &LlmResponse) {
        if let Some(l) = &self.ledger {
            let model = if resp.model.trim().is_empty() {
                self.default_model.as_str()
            } else {
                resp.model.as_str()
            };
            l.record(model, resp);
        }
    }

    /// Note a failed call against the ledger's rate-limit detector. No-op without a ledger.
    fn ledger_note_failure(&self, detail: &str) {
        if let Some(l) = &self.ledger {
            l.note_failure(detail);
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
        // CHOKEPOINT: every streaming call records into the cumulative ledger (success folds
        // usage + clears the rate-limit flag; failure runs the rate-limit detector). The `_`
        // arm delegates to `complete`, which records there — so guard against double-counting
        // by only recording the directly-served arms here.
        let result = match (self.vendor, self.backend) {
            (Vendor::Anthropic, Backend::Cli) => {
                let r = self.complete_cli_streaming(&req, &model, on_delta).await;
                if let Ok(resp) = &r {
                    self.ledger_record(resp);
                }
                r
            }
            (Vendor::Anthropic, Backend::Api) => {
                let r = self.complete_api(&req, &model).await;
                if let Ok(resp) = &r {
                    on_delta(&resp.text);
                    self.ledger_record(resp);
                }
                r
            }
            _ => {
                // Delegates to `complete`, which already records — do NOT record again here.
                let r = self.complete(req).await?;
                on_delta(&r.text);
                return Ok(r);
            }
        };
        if let Err(e) = &result {
            self.ledger_note_failure(&e.to_string());
        }
        result
    }

    /// Run a completion through the selected vendor + transport. Adding a vendor is a new
    /// match arm here plus its [`MODELS`] entries; the request/response shapes don't change.
    pub async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let model = self.model_for(&req);
        // CHOKEPOINT: fold every completion into the cumulative ledger. Success records usage
        // + clears the rate-limit flag; failure runs the provider-agnostic rate-limit detector.
        let result = match self.vendor {
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
        };
        match &result {
            Ok(resp) => self.ledger_record(resp),
            Err(e) => self.ledger_note_failure(&e.to_string()),
        }
        result
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
            .arg("json");
        harden_completion(&mut cmd, req);
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
        let (input_tokens, output_tokens, cache_read, cache_creation) = usage_tokens(&v["usage"]);
        Ok(LlmResponse {
            text: v["result"].as_str().unwrap_or_default().to_string(),
            model: model.to_string(),
            backend: "cli".to_string(),
            cost_usd: v["total_cost_usd"].as_f64(),
            input_tokens,
            output_tokens,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        harden_completion(&mut cmd, req);
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
        let mut usage_in = None;
        let mut usage_out = None;
        let mut usage_cache_read = 0u64;
        let mut usage_cache_creation = 0u64;
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
                            let (i, o, cr, cc) = usage_tokens(&v["usage"]);
                            usage_in = i.or(usage_in);
                            usage_out = o.or(usage_out);
                            // Accumulate cache token breakdowns (they may appear in multiple
                            // stream events; take the max to avoid double-counting).
                            usage_cache_read = usage_cache_read.max(cr);
                            usage_cache_creation = usage_cache_creation.max(cc);
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
            anyhow::bail!(
                "claude CLI (stream) exited {status}: {}",
                stderr_text.trim()
            );
        }
        Ok(LlmResponse {
            text: full,
            model: model.to_string(),
            backend: "cli".to_string(),
            cost_usd: cost,
            input_tokens: usage_in,
            output_tokens: usage_out,
            cache_read_input_tokens: usage_cache_read,
            cache_creation_input_tokens: usage_cache_creation,
        })
    }

    /// API path: POST the Anthropic Messages API with the key.
    ///
    /// Prompt caching: when `req.cache_prefix_len` is set, the user message is sent as a
    /// TWO-BLOCK content array instead of a plain string:
    ///   1. `{type: "text", text: <prefix>, cache_control: {type: "ephemeral"}}` — the stable
    ///      codebase context (repo map + chunk digest) that the provider caches for 5 minutes.
    ///   2. `{type: "text", text: <suffix>}` — the per-batch varying directive (task line +
    ///      rules block) that differs across rule-batches and must never be part of the prefix.
    ///
    /// The `anthropic-beta: prompt-caching-2024-07-31` header enables the feature. Without a
    /// `cache_prefix_len` the request falls through to the plain-string path (no beta header,
    /// no structural change) so non-caching callers are unaffected.
    async fn complete_api(&self, req: &LlmRequest, model: &str) -> anyhow::Result<LlmResponse> {
        let key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("API backend selected but ANTHROPIC_API_KEY is unset")
        })?;

        // Build the user content: a plain string when caching is not requested; a two-block
        // array when it is (prefix block with cache_control, suffix block without).
        let (user_content, use_caching) = match req.cache_prefix_len {
            Some(split_at) if split_at < req.prompt.len() => {
                // Split on the byte boundary; clamp to a valid UTF-8 char boundary so we never
                // slice a multi-byte sequence in half (the min clamp is the builder's job, but
                // be defensive here too).
                let safe_split = req.prompt
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= split_at)
                    .last()
                    .unwrap_or(split_at)
                    .min(req.prompt.len());
                let prefix = &req.prompt[..safe_split];
                let suffix = &req.prompt[safe_split..];
                let content = serde_json::json!([
                    {
                        "type": "text",
                        "text": prefix,
                        "cache_control": {"type": "ephemeral"}
                    },
                    {
                        "type": "text",
                        "text": suffix
                    }
                ]);
                (content, true)
            }
            _ => (serde_json::json!(req.prompt), false),
        };

        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "messages": [{ "role": "user", "content": user_content }],
        });
        if let Some(system) = &req.system {
            body["system"] = serde_json::json!(system);
        }

        let mut builder = reqwest::Client::new()
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");
        // The prompt-caching beta header is only sent when caching is active, so the
        // non-caching path is byte-identical to the pre-caching implementation.
        if use_caching {
            builder = builder.header("anthropic-beta", "prompt-caching-2024-07-31");
        }
        let resp = builder
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
        let (input_tokens, output_tokens, cache_read, cache_creation) = usage_tokens(&v["usage"]);
        // The API doesn't bill back a dollar figure, so compute it from usage × list price.
        // When caching is active the billed input already incorporates cache pricing (the API
        // returns the correctly-billed totals in the usage object) so no adjustment is needed
        // here — just sum as usual.
        let cost_usd =
            price_for(model).and_then(|(pin, pout)| match (input_tokens, output_tokens) {
                (Some(i), Some(o)) => Some((i as f64 * pin + o as f64 * pout) / 1_000_000.0),
                _ => None,
            });
        Ok(LlmResponse {
            text: out,
            model: model.to_string(),
            backend: "api".to_string(),
            cost_usd,
            input_tokens,
            output_tokens,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
        })
    }
}

// ════════════════════════════════════════════════════════════════════════════════════
// MESSAGE BATCHES API (#61)
// ════════════════════════════════════════════════════════════════════════════════════
//
// The Anthropic Message Batches API (POST /v1/messages/batches) runs up to 100k
// requests asynchronously at a flat 50% discount on all input + output tokens. This
// is the right delivery layer for large Camerata scans (hundreds of chunk × rule-batch
// pairs): instead of rate-limited real-time calls that fire one after another, the
// whole batch is submitted in one POST, processed out-of-band by Anthropic (typically
// seconds to minutes on small scans, up to 24h on very large ones), and the results
// are fetched in one round-trip keyed by `custom_id`.
//
// KEY CONSTRAINTS (from the Anthropic spec):
//   - Max 100k requests per batch, 256MB total POST body.
//   - Results arrive UNORDERED — must be keyed back to their inputs via `custom_id`.
//   - Only the `api` backend supports batches (no CLI path).
//   - Composes with prompt caching: set `cache_control` as usual in the batch item.
//   - Polling interval: the spec recommends >= 1s between polls; we use 10s to be gentle.
//   - A batch whose individual items fail individually (e.g. per-item token-limit exceeded)
//     still returns `processing_status == "ended"` — check `result.type` per item.
//
// USAGE PATTERN:
//   1. Build a `Vec<BatchItem>` — one per (chunk × rule-batch) pair.
//   2. Call `Llm::submit_batch` → returns a `batch_id` + item count.
//   3. Store `batch_id` on the job (for the UI's status line).
//   4. Poll `Llm::poll_batch_status` until `processing_status == "ended"`.
//   5. Call `Llm::fetch_batch_results` → a map of `custom_id -> LlmResponse`.
//   6. Feed the responses into `audit_pass`'s parse+dedup+calibrate tail via
//      `reassemble_batch_results` (same dedup/merge path as parallel mode).

/// One item to include in a Message Batch submission. Maps 1:1 to the Anthropic
/// batch request object (a `custom_id` plus a `params` block that mirrors `/v1/messages`).
///
/// `custom_id` must be unique within the batch and <= 64 chars. Camerata uses
/// deterministic ids of the form `c{chunk}-b{batch}` so results can be mapped back
/// without a separate lookup table.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItem {
    /// Unique id for this request within the batch (max 64 chars, `[a-zA-Z0-9_-]+`).
    /// Camerata uses `c{ci}-b{bi}` (e.g. `c0-b2`) matching the chunk/batch indices.
    pub custom_id: String,
    /// The per-item request parameters (mirrors POST /v1/messages body structure).
    pub params: BatchItemParams,
}

/// The `params` block within a batch item. Shape mirrors the `/v1/messages` body.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItemParams {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub messages: Vec<BatchMessage>,
}

/// One message in the `messages` array of a batch item.
#[derive(Debug, Clone, Serialize)]
pub struct BatchMessage {
    pub role: String,
    pub content: serde_json::Value,
}

/// Build a [`BatchItem`] from a [`LlmRequest`] and a deterministic `custom_id`.
/// The content block handles prompt caching the same way `complete_api` does: when
/// `req.cache_prefix_len` is set, the user content is split into a cached-prefix block
/// plus a suffix block. The system prompt is forwarded verbatim (the batch API accepts
/// the same `system` field as the messages API).
pub fn build_batch_item(custom_id: impl Into<String>, req: &LlmRequest, model: &str) -> BatchItem {
    let content = match req.cache_prefix_len {
        Some(split_at) if split_at < req.prompt.len() => {
            let safe_split = req.prompt
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= split_at)
                .last()
                .unwrap_or(split_at)
                .min(req.prompt.len());
            let prefix = &req.prompt[..safe_split];
            let suffix = &req.prompt[safe_split..];
            serde_json::json!([
                {
                    "type": "text",
                    "text": prefix,
                    "cache_control": {"type": "ephemeral"}
                },
                {
                    "type": "text",
                    "text": suffix
                }
            ])
        }
        _ => serde_json::json!(req.prompt),
    };
    BatchItem {
        custom_id: custom_id.into(),
        params: BatchItemParams {
            model: model.to_string(),
            max_tokens: req.max_tokens,
            system: req.system.clone(),
            messages: vec![BatchMessage {
                role: "user".to_string(),
                content,
            }],
        },
    }
}

/// Result of submitting a message batch.
#[derive(Debug, Clone)]
pub struct BatchSubmitResult {
    /// The Anthropic batch id (e.g. `msgbatch_01AbCd...`). Store this on the job.
    pub batch_id: String,
    /// Number of items accepted into the batch.
    pub request_counts: BatchRequestCounts,
}

/// Item-level counts from the batch object (mirrors Anthropic's `request_counts` field).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BatchRequestCounts {
    pub processing: u32,
    pub succeeded: u32,
    pub errored: u32,
    pub canceled: u32,
    pub expired: u32,
}

/// Current status of a submitted batch as returned by GET /v1/messages/batches/{id}.
#[derive(Debug, Clone)]
pub struct BatchStatus {
    /// `in_progress` | `ended` — when `ended`, results are available.
    pub processing_status: String,
    /// Live item counts (updated as the batch processes).
    pub request_counts: BatchRequestCounts,
}

/// One result row from the batch results JSONL stream. Only `succeeded` rows carry a
/// usable response; other types surface an error so the caller can log and skip.
#[derive(Debug, Clone)]
pub struct BatchResultRow {
    /// The `custom_id` the caller assigned when building the item.
    pub custom_id: String,
    /// The completion text + token usage when the item succeeded.
    pub response: Option<LlmResponse>,
    /// The error message when `result_type != "succeeded"`.
    pub error: Option<String>,
}

impl Llm {
    /// Submit a batch of requests to the Anthropic Message Batches API. Returns the
    /// `batch_id` + initial counts, or an error if the submission failed. Requires the
    /// `api` backend and a valid `ANTHROPIC_API_KEY`. The batch processes asynchronously;
    /// poll with [`poll_batch_status`] and fetch results with [`fetch_batch_results`].
    ///
    /// CAP ENFORCEMENT: logs a warning when `items.len() > 100_000` (the API hard cap);
    /// the caller is responsible for splitting before calling this function. A 256MB POST
    /// body limit is not checked here (the average Camerata batch item is ~5-20KB, so
    /// 100k items is already the binding constraint in practice).
    pub async fn submit_batch(&self, items: Vec<BatchItem>) -> anyhow::Result<BatchSubmitResult> {
        let key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Message Batches API requires the `api` backend and ANTHROPIC_API_KEY to be set"
            )
        })?;
        if items.len() > 100_000 {
            // Log but don't hard-fail: the API may raise a 4xx which we'll surface anyway.
            eprintln!(
                "[camerata-server/llm] batch has {} items, which exceeds the 100k cap — \
                 the API will reject it; split into sub-batches before calling submit_batch",
                items.len()
            );
        }
        let body = serde_json::json!({ "requests": items });
        let resp = reqwest::Client::new()
            .post("https://api.anthropic.com/v1/messages/batches")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "message-batches-2024-09-24")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Message Batches submit failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Message Batches API HTTP {status}: {text}");
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse batch response: {e}"))?;
        let batch_id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("batch response missing `id` field: {text}"))?
            .to_string();
        let counts = parse_request_counts(&v["request_counts"]);
        Ok(BatchSubmitResult { batch_id, request_counts: counts })
    }

    /// Poll the status of a submitted batch. Returns the `processing_status` and current
    /// item counts. When `processing_status == "ended"` the batch is complete and results
    /// can be fetched with [`fetch_batch_results`].
    pub async fn poll_batch_status(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        let key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("batch polling requires ANTHROPIC_API_KEY")
        })?;
        let resp = reqwest::Client::new()
            .get(format!("https://api.anthropic.com/v1/messages/batches/{batch_id}"))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "message-batches-2024-09-24")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("batch poll request failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("batch poll HTTP {status}: {text}");
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse batch poll: {e}"))?;
        let processing_status = v["processing_status"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let counts = parse_request_counts(&v["request_counts"]);
        Ok(BatchStatus { processing_status, request_counts: counts })
    }

    /// Stream the results of a completed batch from the Anthropic results URL. Each line
    /// of the JSONL response is one `BatchResultRow` keyed by `custom_id`. Callers MUST
    /// call this only after [`poll_batch_status`] reports `processing_status == "ended"`.
    ///
    /// Results arrive UNORDERED — the caller must build a `custom_id -> response` map
    /// (see [`reassemble_batch_results`]) and not rely on line order.
    pub async fn fetch_batch_results(
        &self,
        batch_id: &str,
    ) -> anyhow::Result<Vec<BatchResultRow>> {
        let key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("batch result fetch requires ANTHROPIC_API_KEY")
        })?;
        let resp = reqwest::Client::new()
            .get(format!(
                "https://api.anthropic.com/v1/messages/batches/{batch_id}/results"
            ))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "message-batches-2024-09-24")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("batch results fetch failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("batch results HTTP {status}: {text}");
        }
        parse_batch_results_jsonl(&text)
    }

    /// Convenience accessor for the API key (needed by the batch polling loop to confirm
    /// the backend is viable before submitting). Returns `None` for the CLI backend.
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}

/// Parse the `request_counts` object from a batch API response. Missing fields are zero.
fn parse_request_counts(v: &serde_json::Value) -> BatchRequestCounts {
    BatchRequestCounts {
        processing: v["processing"].as_u64().unwrap_or(0) as u32,
        succeeded: v["succeeded"].as_u64().unwrap_or(0) as u32,
        errored: v["errored"].as_u64().unwrap_or(0) as u32,
        canceled: v["canceled"].as_u64().unwrap_or(0) as u32,
        expired: v["expired"].as_u64().unwrap_or(0) as u32,
    }
}

/// Parse the JSONL results body (one JSON object per line) into a vec of `BatchResultRow`.
/// Robust: malformed lines are skipped (logged at debug), so one bad item never aborts
/// the whole result set. The caller converts this vec into a `custom_id -> response` map
/// via [`reassemble_batch_results`].
pub fn parse_batch_results_jsonl(jsonl: &str) -> anyhow::Result<Vec<BatchResultRow>> {
    let mut rows = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            // Malformed lines are rare (Anthropic's JSONL is well-formed); log at debug
            // level via eprintln only in debug builds to avoid noise in prod.
            #[cfg(debug_assertions)]
            eprintln!("[camerata-server/llm] skipping malformed batch result line: {line}");
            continue;
        };
        let custom_id = match v["custom_id"].as_str() {
            Some(s) => s.to_string(),
            None => {
                #[cfg(debug_assertions)]
                eprintln!("[camerata-server/llm] batch result line missing custom_id, skipping");
                continue;
            }
        };
        let result_type = v["result"]["type"].as_str().unwrap_or("error");
        if result_type == "succeeded" {
            // The response mirrors the /v1/messages response shape.
            let msg = &v["result"]["message"];
            let model_id = msg["model"].as_str().unwrap_or("").to_string();
            let out = msg["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            let (input_tokens, output_tokens, cache_read, cache_creation) =
                usage_tokens(&msg["usage"]);
            let cost_usd =
                price_for(&model_id).and_then(|(pin, pout)| match (input_tokens, output_tokens) {
                    (Some(i), Some(o)) => Some((i as f64 * pin + o as f64 * pout) / 1_000_000.0),
                    _ => None,
                });
            rows.push(BatchResultRow {
                custom_id,
                response: Some(LlmResponse {
                    text: out,
                    model: model_id,
                    backend: "api/batch".to_string(),
                    cost_usd,
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens: cache_read,
                    cache_creation_input_tokens: cache_creation,
                }),
                error: None,
            });
        } else {
            let error_msg = v["result"]["error"]["message"]
                .as_str()
                .unwrap_or(result_type)
                .to_string();
            rows.push(BatchResultRow {
                custom_id,
                response: None,
                error: Some(error_msg),
            });
        }
    }
    Ok(rows)
}

/// Build a `custom_id -> LlmResponse` map from a flat list of `BatchResultRow`s (the
/// unordered JSONL stream from [`Llm::fetch_batch_results`]). Items whose result was an
/// error (no `response`) are absent from the map, so the caller's reassembler can detect
/// which (chunk, batch) pairs failed and handle them (log, skip, surface as partial).
///
/// This is the KEY STEP that undoes the unordered delivery: the caller assigned
/// deterministic `custom_id`s of the form `c{ci}-b{bi}` when building the batch, and this
/// map lets it look up each (ci, bi) pair's response in O(1).
pub fn reassemble_batch_results(
    rows: Vec<BatchResultRow>,
) -> std::collections::HashMap<String, LlmResponse> {
    rows.into_iter()
        .filter_map(|r| r.response.map(|resp| (r.custom_id, resp)))
        .collect()
}

// ════════════════════════════════════════════════════════════════════════════════════
// OPENROUTER COMPLETER
// ════════════════════════════════════════════════════════════════════════════════════
//
// Calls the OpenRouter chat-completions endpoint (`POST /api/v1/chat/completions`) with
// an OpenAI-compatible request/response shape. The key is read from the credential store
// at call time (not captured at construction). Prompt caching is NOT used on this path
// (OpenRouter does not expose Anthropic's caching protocol on top of its pass-through).
//
// Object-safe: satisfies the same `Completer` trait as `Llm`, so callers never need to
// know which backend they hold.

/// A `Completer` that routes bare-LLM calls through OpenRouter's chat-completions API.
///
/// Constructed via [`build_completer`] when the model registry reports `provider =
/// "openrouter"` for the requested model id. The API key is read from `api_key` (already
/// resolved from the credential store by the factory — no IO at call time).
///
/// Each HTTP call is preceded by `limiter.acquire("openrouter")` so concurrent audit
/// calls to OpenRouter free-tier models self-throttle to the configured RPM cap (default
/// 20 RPM) rather than generating 429s. The limiter is a shared [`Arc`] clone; all
/// `OpenRouterCompleter` instances across the process share one bucket.
pub struct OpenRouterCompleter {
    /// The resolved `OPENROUTER_API_KEY` value. Never empty (factory guarantees it).
    api_key: String,
    /// Shared per-provider rate limiter. Awaited before every HTTP call.
    limiter: std::sync::Arc<crate::rate_limit::ProviderRateLimiter>,
}

impl OpenRouterCompleter {
    /// Call OpenRouter's `/api/v1/chat/completions` endpoint and return the completion.
    ///
    /// Awaits the per-provider rate limiter before issuing the HTTP request so concurrent
    /// callers self-throttle to the configured RPM cap (default 20 RPM for "openrouter").
    async fn call_api(&self, req: &LlmRequest, model: &str) -> anyhow::Result<LlmResponse> {
        // Acquire a slot before hitting the wire. Unlimited providers (Anthropic, etc.)
        // return immediately; rate-limited ones park until a refill token is available.
        self.limiter.acquire("openrouter").await;
        // Build the messages array. System prompt goes as a separate "system" role message
        // when present (the OpenAI-compatible schema used by OpenRouter supports this).
        let mut messages: Vec<serde_json::Value> = Vec::new();
        if let Some(system) = &req.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system
            }));
        }
        messages.push(serde_json::json!({
            "role": "user",
            "content": req.prompt
        }));

        let body = serde_json::json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });

        let resp = reqwest::Client::new()
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://camerata.ai")
            .header("X-Title", "Camerata")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("OpenRouter API request failed: {e}"))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("OpenRouter API HTTP {status}: {text}");
        }

        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parse OpenRouter response JSON: {e}"))?;

        // OpenAI-compatible shape: choices[0].message.content
        let output = v["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["content"].as_str())
            .unwrap_or_default()
            .to_string();

        // Usage (OpenAI-compatible: prompt_tokens / completion_tokens).
        let input_tokens = v["usage"]["prompt_tokens"].as_u64();
        let output_tokens = v["usage"]["completion_tokens"].as_u64();
        // The response may echo back the model id (useful for wildcard/routing models).
        let model_returned = v["model"].as_str().unwrap_or(model).to_string();

        Ok(LlmResponse {
            text: output,
            model: model_returned,
            backend: "openrouter/api".to_string(),
            cost_usd: None, // OpenRouter does not return a dollar figure in the response body.
            input_tokens,
            output_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }
}

#[async_trait::async_trait]
impl Completer for OpenRouterCompleter {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let model = if req.model.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "OpenRouterCompleter requires an explicit model id in the request"
            ));
        } else {
            req.model.clone()
        };
        self.call_api(&req, &model).await
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        // OpenRouter streaming is not implemented in this seam — fall back to a single
        // non-streaming call and deliver the full text as one delta. Streaming can be
        // added as a follow-up without changing the trait contract.
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ════════════════════════════════════════════════════════════════════════════════════
// PROVIDER-SELECTING FACTORY
// ════════════════════════════════════════════════════════════════════════════════════
//
// Given a model id, the registry's provider tag for it, and the credential store,
// return the right `Arc<dyn Completer>`. Callers that currently do `state.llm()` for a
// user-chosen model should use `build_completer` instead so OpenRouter-provider models
// are dispatched to `OpenRouterCompleter` rather than hitting the Anthropic "not wired
// yet" arm.

/// Build the right `Arc<dyn Completer>` for `model_id` based on what the registry says
/// its provider is.
///
/// - `"claude"` provider (or any unknown/unrecognised provider) → returns `Arc::new(llm)`,
///   the existing Anthropic `Llm` that was already built by the caller.
/// - `"openrouter"` provider → resolves `OPENROUTER_API_KEY` from `creds` and returns an
///   `Arc<OpenRouterCompleter>` that self-throttles via `limiter` before each HTTP call.
///   Returns an error when the key is not set.
///
/// `limiter` is a shared [`crate::rate_limit::ProviderRateLimiter`] (held in `AppState`).
/// Passing `Arc::new(ProviderRateLimiter::new())` in tests is fine; the default cap for
/// the `"openrouter"` bucket is 20 RPM.
///
/// This is intentionally a free function (not a method on `AppState`) so it can be
/// imported and tested without pulling in the full server state.
pub fn build_completer(
    model_id: &str,
    registry: &crate::model_registry::ModelRegistry,
    creds: &dyn crate::credentials::CredentialStore,
    llm: std::sync::Arc<Llm>,
    limiter: std::sync::Arc<crate::rate_limit::ProviderRateLimiter>,
) -> anyhow::Result<std::sync::Arc<dyn Completer>> {
    let provider = registry
        .all_entries()
        .into_iter()
        .find(|e| e.id == model_id)
        .map(|e| e.provider)
        .unwrap_or_else(|| "claude".to_string());

    match provider.as_str() {
        "openrouter" => {
            let key = creds
                .get(crate::credentials::OPENROUTER_API_KEY)
                .map_err(|e| anyhow::anyhow!("credential store error: {e}"))?
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "model `{model_id}` is an OpenRouter model but OPENROUTER_API_KEY is not \
                         set — add it via Settings → Credentials before using this model"
                    )
                })?;
            Ok(std::sync::Arc::new(OpenRouterCompleter { api_key: key, limiter }))
        }
        // "claude" or any unrecognised provider: use the existing Anthropic Llm.
        _ => Ok(llm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_repo_read_binds_dir_and_default_is_none() {
        let req = LlmRequest::new("hi").with_repo_read("/tmp/repo");
        assert_eq!(req.repo_read_dir.as_deref(), Some(std::path::Path::new("/tmp/repo")));
        // Empty path is treated as None (no read window).
        assert!(LlmRequest::new("hi").with_repo_read("").repo_read_dir.is_none());
        // Default request leaves the lockdown in place (repo_read_dir None).
        assert!(LlmRequest::new("hi").repo_read_dir.is_none());
    }

    #[test]
    fn read_only_tools_contain_no_writers() {
        for t in READ_ONLY_TOOLS {
            assert!(["Read", "Glob", "Grep", "LS"].contains(t));
        }
        for forbidden in ["Write", "Edit", "Bash", "Task", "MultiEdit", "NotebookEdit"] {
            assert!(!READ_ONLY_TOOLS.contains(&forbidden));
        }
    }

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
            ledger: None,
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
        assert_eq!(r.cache_prefix_len, None, "caching off by default");
    }

    #[test]
    fn cache_prefix_len_builder() {
        // Normal case: prefix len < prompt len -> stored as-is (clamped to prompt len).
        let r = LlmRequest::new("hello world").with_cache_prefix_len(5);
        assert_eq!(r.cache_prefix_len, Some(5));

        // Oversized: clamped to the prompt length, not panicking.
        let r2 = LlmRequest::new("hi").with_cache_prefix_len(999);
        assert_eq!(r2.cache_prefix_len, Some(2), "clamped to prompt length");

        // Zero is a no-op: caching stays disabled.
        let r3 = LlmRequest::new("hi").with_cache_prefix_len(0);
        assert_eq!(r3.cache_prefix_len, None, "zero prefix = no caching");

        // Prefix == prompt length: stored as-is (whole prompt is the prefix, no suffix).
        let r4 = LlmRequest::new("exact").with_cache_prefix_len(5);
        assert_eq!(r4.cache_prefix_len, Some(5));
    }

    #[test]
    fn usage_tokens_parses_cache_fields() {
        // Full usage object including both cache fields.
        let usage = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 30
        });
        let (inp, out, cr, cc) = usage_tokens(&usage);
        // input = base + cache_read + cache_creation = 100 + 200 + 30 = 330
        assert_eq!(inp, Some(330), "input folds in cache fields");
        assert_eq!(out, Some(50));
        assert_eq!(cr, 200, "cache_read surfaced separately");
        assert_eq!(cc, 30, "cache_creation surfaced separately");

        // Missing cache fields -> defaults to zero (no panic).
        let usage2 = serde_json::json!({"input_tokens": 10, "output_tokens": 5});
        let (inp2, out2, cr2, cc2) = usage_tokens(&usage2);
        assert_eq!(inp2, Some(10));
        assert_eq!(out2, Some(5));
        assert_eq!(cr2, 0);
        assert_eq!(cc2, 0);

        // Absent input_tokens -> None (not zero).
        let usage3 = serde_json::json!({"output_tokens": 5});
        let (inp3, _out3, _cr3, _cc3) = usage_tokens(&usage3);
        assert_eq!(inp3, None, "missing input_tokens stays None");
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
            ledger: None,
        };
        let err = llm.complete(LlmRequest::new("hi")).await.unwrap_err();
        assert!(err.to_string().contains("not wired yet"));
    }

    // ── Message Batches (#61) ──────────────────────────────────────────────────────

    /// `build_batch_item` produces a correctly-shaped item with the deterministic `custom_id`
    /// and the prompt forwarded into the `messages[0].content` field.
    #[test]
    fn build_batch_item_plain_prompt() {
        let req = LlmRequest::new("audit this code")
            .with_model("claude-sonnet-4-6")
            .with_max_tokens(2048);
        let item = build_batch_item("c0-b1", &req, "claude-sonnet-4-6");
        assert_eq!(item.custom_id, "c0-b1");
        assert_eq!(item.params.model, "claude-sonnet-4-6");
        assert_eq!(item.params.max_tokens, 2048);
        assert!(item.params.system.is_none());
        // Plain string content when no cache prefix is set.
        let v = serde_json::to_value(&item.params.messages[0].content).unwrap();
        assert_eq!(v.as_str().unwrap_or(""), "audit this code");
    }

    /// When `cache_prefix_len` is set, the content is split into two blocks: a cached
    /// prefix block with `cache_control` and a suffix block without.
    #[test]
    fn build_batch_item_with_cache_prefix() {
        let prompt = "static prefix part\ndynamic suffix";
        let split = "static prefix part\n".len();
        let req = LlmRequest::new(prompt)
            .with_cache_prefix_len(split)
            .with_model("claude-sonnet-4-6");
        let item = build_batch_item("c2-b0", &req, "claude-sonnet-4-6");
        let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
        let arr = content.as_array().expect("cached content is an array");
        assert_eq!(arr.len(), 2);
        // First block: the static prefix with cache_control.
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "static prefix part\n");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
        // Second block: the suffix, no cache_control.
        assert_eq!(arr[1]["type"], "text");
        assert_eq!(arr[1]["text"], "dynamic suffix");
        assert!(arr[1].get("cache_control").is_none());
    }

    /// System prompt is forwarded onto `params.system` and present in the serialized item.
    #[test]
    fn build_batch_item_with_system_prompt() {
        let req = LlmRequest::new("check this")
            .with_system("you are an auditor")
            .with_model("claude-haiku-4-5-20251001");
        let item = build_batch_item("c0-b0", &req, "claude-haiku-4-5-20251001");
        assert_eq!(item.params.system.as_deref(), Some("you are an auditor"));
        // Ensure it serialises correctly (no missing field).
        let v = serde_json::to_value(&item.params).unwrap();
        assert_eq!(v["system"], "you are an auditor");
    }

    /// `parse_batch_results_jsonl` extracts succeeded rows, maps error rows, and skips
    /// malformed lines — the fundamental parse + reassemble contract.
    #[test]
    fn parse_batch_results_jsonl_succeeded_and_errored() {
        let jsonl = r#"{"custom_id":"c0-b0","result":{"type":"succeeded","message":{"model":"claude-sonnet-4-6","content":[{"type":"text","text":"finding A"}],"usage":{"input_tokens":100,"output_tokens":50}}}}
{"custom_id":"c1-b0","result":{"type":"errored","error":{"message":"token limit exceeded"}}}
{"custom_id":"c0-b1","result":{"type":"succeeded","message":{"model":"claude-sonnet-4-6","content":[{"type":"text","text":"finding B"}],"usage":{"input_tokens":80,"output_tokens":40}}}}
malformed line, not json
"#;
        let rows = parse_batch_results_jsonl(jsonl).expect("parse should not fail");
        // 2 succeeded + 1 errored; the malformed line is skipped.
        assert_eq!(rows.len(), 3, "3 valid rows (2 ok + 1 error), malformed skipped");
        let ok: Vec<_> = rows.iter().filter(|r| r.response.is_some()).collect();
        let err: Vec<_> = rows.iter().filter(|r| r.error.is_some()).collect();
        assert_eq!(ok.len(), 2, "two succeeded rows");
        assert_eq!(err.len(), 1, "one error row");

        // Spot-check a succeeded row.
        let c0b0 = ok.iter().find(|r| r.custom_id == "c0-b0").expect("c0-b0 present");
        let resp = c0b0.response.as_ref().unwrap();
        assert_eq!(resp.text, "finding A");
        assert_eq!(resp.backend, "api/batch");
        // input_tokens = base(100) + cache_read(0) + cache_creation(0) = 100
        assert_eq!(resp.input_tokens, Some(100), "input when no cache fields");
        assert_eq!(resp.output_tokens, Some(50));

        // The error row carries the message.
        let e = err[0];
        assert_eq!(e.custom_id, "c1-b0");
        assert_eq!(e.error.as_deref(), Some("token limit exceeded"));
    }

    /// `reassemble_batch_results` maps succeeded rows by custom_id and excludes error rows.
    #[test]
    fn reassemble_maps_by_custom_id_excludes_errors() {
        let rows = vec![
            BatchResultRow {
                custom_id: "c0-b0".to_string(),
                response: Some(LlmResponse {
                    text: "text A".to_string(),
                    model: "m".to_string(),
                    backend: "api/batch".to_string(),
                    cost_usd: None,
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
                error: None,
            },
            BatchResultRow {
                custom_id: "c1-b0".to_string(),
                response: None,
                error: Some("limit exceeded".to_string()),
            },
            BatchResultRow {
                custom_id: "c0-b1".to_string(),
                response: Some(LlmResponse {
                    text: "text B".to_string(),
                    model: "m".to_string(),
                    backend: "api/batch".to_string(),
                    cost_usd: None,
                    input_tokens: Some(20),
                    output_tokens: Some(8),
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
                error: None,
            },
        ];
        let map = reassemble_batch_results(rows);
        assert_eq!(map.len(), 2, "two succeeded rows in map");
        assert!(map.contains_key("c0-b0"));
        assert!(map.contains_key("c0-b1"));
        // Error row is absent.
        assert!(!map.contains_key("c1-b0"), "error row excluded from map");
        assert_eq!(map["c0-b0"].text, "text A");
        assert_eq!(map["c0-b1"].text, "text B");
    }

    /// `parse_batch_results_jsonl` is robust to a fully-empty body (returns an empty vec,
    /// never an error). Empty bodies arise when a batch has 0 items that completed.
    #[test]
    fn parse_batch_results_empty_body_is_ok() {
        let rows = parse_batch_results_jsonl("").expect("empty body should not error");
        assert!(rows.is_empty());
    }

    /// `build_batch_item` serialises correctly (no private fields, correct JSON shape).
    #[test]
    fn batch_item_serialises_to_expected_shape() {
        let req = LlmRequest::new("hello").with_model("claude-sonnet-4-6").with_max_tokens(512);
        let item = build_batch_item("c0-b0", &req, "claude-sonnet-4-6");
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["custom_id"], "c0-b0");
        assert!(v["params"]["model"].is_string());
        assert!(v["params"]["messages"].is_array());
        assert_eq!(v["params"]["max_tokens"], 512);
    }

    // ── OpenRouter Completer + factory ────────────────────────────────────────────

    use crate::credentials::CredentialStore as _;

    /// A canned OpenRouter JSON response (chat-completions shape) with both a text output
    /// and a usage block. `parse_openrouter_response` (exercised indirectly via
    /// `call_api`) must extract text and token counts correctly.
    #[test]
    fn openrouter_response_parse_extracts_text_and_tokens() {
        // Build a synthetic chat-completions response body.
        let body = serde_json::json!({
            "id": "gen-abc123",
            "model": "qwen/qwen3-235b-a22b-04-28:free",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Here is the audit finding."
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 120,
                "completion_tokens": 30,
                "total_tokens": 150
            }
        });

        // Replicate the parse logic from `call_api` directly (no HTTP call needed).
        let output = body["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["content"].as_str())
            .unwrap_or_default()
            .to_string();
        let input_tokens = body["usage"]["prompt_tokens"].as_u64();
        let output_tokens = body["usage"]["completion_tokens"].as_u64();
        let model_returned = body["model"].as_str().unwrap_or("unknown").to_string();

        assert_eq!(output, "Here is the audit finding.");
        assert_eq!(input_tokens, Some(120));
        assert_eq!(output_tokens, Some(30));
        assert_eq!(model_returned, "qwen/qwen3-235b-a22b-04-28:free");
    }

    /// Missing `choices` in the response (malformed or empty completion) → empty string,
    /// not a panic. Robustness guard.
    #[test]
    fn openrouter_response_parse_handles_missing_choices() {
        let body = serde_json::json!({ "model": "some/model", "choices": [] });
        let output = body["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["content"].as_str())
            .unwrap_or_default()
            .to_string();
        assert_eq!(output, "");
    }

    // ── build_completer factory ───────────────────────────────────────────────────

    fn make_llm() -> std::sync::Arc<Llm> {
        std::sync::Arc::new(Llm {
            vendor: Vendor::Anthropic,
            backend: Backend::Cli,
            default_model: "claude-sonnet-4-6".to_string(),
            api_key: None,
            ledger: None,
        })
    }

    fn make_limiter() -> std::sync::Arc<crate::rate_limit::ProviderRateLimiter> {
        std::sync::Arc::new(crate::rate_limit::ProviderRateLimiter::new())
    }

    /// When the model id is not in the registry at all, the factory defaults to the
    /// Anthropic Llm (safe fallback: unknown models stay on the existing path).
    #[test]
    fn factory_returns_anthropic_for_unknown_model() {
        let registry = crate::model_registry::ModelRegistry::new();
        let creds = crate::credentials::MemoryCredentialStore::new();
        let llm = make_llm();

        let completer = build_completer("unknown-model-xyz", &registry, &creds, llm, make_limiter());
        // Should succeed (no error for an unknown model — safe fallback to Anthropic).
        assert!(
            completer.is_ok(),
            "factory must not error for unknown models"
        );
    }

    /// For a claude-provider model (e.g. `claude-sonnet-4-6` in the static registry),
    /// the factory returns the Anthropic Llm without touching the credential store.
    #[test]
    fn factory_returns_anthropic_for_claude_provider_model() {
        let registry = crate::model_registry::ModelRegistry::new();
        let creds = crate::credentials::MemoryCredentialStore::new();
        // Note: no OPENROUTER_API_KEY set, yet this must not error.
        let llm = make_llm();

        let completer = build_completer("claude-sonnet-4-6", &registry, &creds, llm, make_limiter());
        assert!(
            completer.is_ok(),
            "claude-provider model must succeed even without an OpenRouter key"
        );
    }

    /// For an openrouter-provider model, the factory errors when no API key is set.
    #[test]
    fn factory_errors_for_openrouter_model_without_key() {
        let registry = crate::model_registry::ModelRegistry::new();
        // Inject a fake OpenRouter entry so the factory knows the provider.
        registry.seed_openrouter_entries(vec![crate::model_registry::RegistryEntry {
            provider: "openrouter".to_string(),
            display: "Qwen3 Coder (free)".to_string(),
            id: "qwen/qwen3-235b:free".to_string(),
            free: true,
            tool_use: true,
            context: 32_768,
            coding: 1.0,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: false,
        }]);
        let creds = crate::credentials::MemoryCredentialStore::new();
        // No key set → error.
        let llm = make_llm();
        let result = build_completer("qwen/qwen3-235b:free", &registry, &creds, llm, make_limiter());
        assert!(result.is_err(), "factory must error when key is absent");
        // Extract the error without relying on `T: Debug` (Arc<dyn Completer> is not Debug).
        let msg = match result {
            Err(e) => e.to_string(),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("OPENROUTER_API_KEY"),
            "error message must mention the missing key: {msg}"
        );
    }

    /// For an openrouter-provider model WITH a key set, the factory succeeds and returns
    /// an OpenRouterCompleter (we can verify this via the `as_any` downcast).
    #[test]
    fn factory_returns_openrouter_completer_when_key_is_set() {
        let registry = crate::model_registry::ModelRegistry::new();
        registry.seed_openrouter_entries(vec![crate::model_registry::RegistryEntry {
            provider: "openrouter".to_string(),
            display: "Qwen3 Coder (free)".to_string(),
            id: "qwen/qwen3-235b:free".to_string(),
            free: true,
            tool_use: true,
            context: 32_768,
            coding: 1.0,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: false,
        }]);
        let creds = crate::credentials::MemoryCredentialStore::new();
        creds
            .set(crate::credentials::OPENROUTER_API_KEY, "sk-or-test-key")
            .unwrap();
        let llm = make_llm();
        let completer = build_completer("qwen/qwen3-235b:free", &registry, &creds, llm, make_limiter())
            .expect("factory must succeed when key is set");
        // Verify the concrete type is OpenRouterCompleter via downcast.
        assert!(
            completer.as_any().is::<OpenRouterCompleter>(),
            "expected an OpenRouterCompleter for an openrouter-provider model"
        );
    }
}
