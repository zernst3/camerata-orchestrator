//! Camerata governance gateway — Rust MCP server (rmcp 1.7), stdio transport.
//!
//! Proves the load-bearing claim the Haiku NO-GO doc said was impossible:
//! a Rust-owned governance gate that dynamically allows/denies an agent's
//! tool calls, in-process, per a data-driven rule-subset. The agent
//! (`claude -p`) is locked to ONLY this server's `gated_write` tool; every
//! write the agent attempts routes through Rust code that applies the active
//! rule-subset BEFORE touching the filesystem.
//!
//! # Per-session rule delivery (the live slice)
//!
//! The rule-subset is NOT hard-coded. At agent spawn the orchestrator:
//!   1. computes the session's rule-subset,
//!   2. writes it to a per-session rules JSON file,
//!   3. generates an mcp-config that launches THIS binary with env
//!      `CAMERATA_RULES_FILE` pointing at that file.
//!
//! On startup this server reads `CAMERATA_RULES_FILE` (a JSON array of rule-id
//! strings, e.g. `["GOV-1"]`) and evaluates every tool call against it via the
//! SHARED [`camerata_gateway::evaluate_call`]. If the env var is unset it falls
//! back to the verified default subset `["GOV-1"]`. This is the stdio binding
//! of the same `GovernanceGateway` logic the in-process `GovernedGateway` uses
//! — byte-for-byte identical evaluation, two transports.
//!
//! # MCP tool namespacing
//!
//! Claude Code namespaces an MCP tool as `mcp__<server-key>__<tool>`, where
//! `<server-key>` is the key under `mcpServers` in the mcp-config. The config
//! generator (`camerata_agent::mcp_config`) uses the key `camerata`, and this
//! server registers the tool `gated_write`, so the agent sees exactly
//! `mcp__camerata__gated_write` — the constant `camerata_agent::GATED_WRITE_TOOL`.

use camerata_core::{Decision, RuleId, ToolCall};
use camerata_gateway::{evaluate_call, gov1_rule};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

mod delegate;
use delegate::OrchestratorConfig;

/// Env var the orchestrator points at the per-session rules JSON file.
pub const RULES_FILE_ENV: &str = "CAMERATA_RULES_FILE";

/// Optional env override for the structured gate-decision JSONL sink path. When set,
/// every gate decision is appended (one JSON object per line) to this file. When unset,
/// the sink path is derived from [`RULES_FILE_ENV`] (a `gate-events.jsonl` sibling of
/// the per-session rules file), so the orchestrator that wrote the rules file always
/// knows where to tail decisions from. This is the OBSERVABILITY channel — it records
/// what the gate decided; it never changes the decision.
pub const GATE_EVENTS_FILE_ENV: &str = "CAMERATA_GATE_EVENTS_FILE";

/// Optional env override for the clarification-request JSONL sink path (Phase 3b). When
/// set, every `ask_clarification` call is appended (one JSON object per line) to this
/// file. When unset, the path is derived from [`RULES_FILE_ENV`] (a
/// `clarify-requests.jsonl` sibling of the per-session rules file), so the orchestrator
/// that wrote the rules file always knows where to read questions from. This is the
/// agent→run channel; it carries STRUCTURED questions, never writes to the repo.
pub const CLARIFY_REQUESTS_FILE_ENV: &str = "CAMERATA_CLARIFY_REQUESTS_FILE";

/// One structured gate-decision record, appended as a single JSONL line to the sink.
///
/// Recording-only: this mirrors what [`Gateway::gated_write`] already decided, so the
/// server can fold REAL gate decisions out of the subprocess into the run's event
/// stream. It carries no logic; serializing it cannot change a verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateDecisionRecord {
    /// The kind of record: "gate" (a gated_write decision), "delegate-dispatch"
    /// (the orchestrator delegated a subtask), or "delegate-return" (the delegate
    /// child returned). Defaults to "gate" for back-compat when absent.
    #[serde(default = "default_gate_kind")]
    pub kind: String,
    /// "allow" or "deny" — straight from the gate's decision.
    pub verdict: String,
    /// The target path the agent attempted to write (or the delegate tier, for
    /// delegate records).
    pub target: String,
    /// The rule id (or structural guard tag, e.g. "JAIL") that denied, when denied.
    pub rule: Option<String>,
    /// Human-readable reason (the gate's own message), concise.
    pub reason: String,
    /// Unix-epoch milliseconds when the decision was recorded.
    pub ts_ms: u128,
    /// FNV-1a hex hash of the denied content (NEVER the raw content — public repo).
    /// Set only for DENY records where content is available; `None` for allow records
    /// and delegation records. Observability-only; cannot affect any gate verdict.
    #[serde(default)]
    pub content_hash: Option<String>,
}

fn default_gate_kind() -> String {
    "gate".to_string()
}

/// One selectable option on a structured clarification the agent raises. Mirrors the 3a
/// `ClarifyOption` shape (label + benefit/drawback description) so the server can post it
/// into the existing clarify store without translation. Serde-shaped, no logic.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClarifyRequestOption {
    /// The short, selectable label (what the answer records).
    pub label: String,
    /// A one-line benefit/drawback so the human can weigh the choice.
    #[serde(default)]
    pub description: String,
}

/// A structured clarification the GATED agent raises mid-run, recorded to the
/// clarify-request sink. This is the wire shape between the gateway subprocess and the
/// server: the server reads these back, posts them into the 3a clarify store keyed to the
/// run's story, and pauses the run. Recording-only — building or serializing one cannot
/// write to the repo or change any gate verdict (asking a question is not a write).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationRequestRecord {
    /// The question text.
    pub question: String,
    /// The structured options (empty = a pure free-text question).
    #[serde(default)]
    pub options: Vec<ClarifyRequestOption>,
    /// Whether more than one option may be selected.
    #[serde(default)]
    pub multi_select: bool,
    /// Whether the "Other" free-text escape is offered. Defaults true (the 3a default).
    #[serde(default = "default_true")]
    pub allow_free_text: bool,
    /// Unix-epoch milliseconds when the question was recorded.
    pub ts_ms: u128,
}

fn default_true() -> bool {
    true
}

/// Append a clarification-request record to the clarify-request JSONL sink (best-effort).
///
/// The agent→run channel for Phase 3b. RECORDING-ONLY: a write failure is ignored. This
/// never touches the repo (it appends to the per-session sink, a sibling of the rules
/// file, which lives OUTSIDE the worktree jail) and cannot change any gate verdict.
fn append_clarify_request(record: &ClarificationRequestRecord) {
    let Some(path) = clarify_requests_sink_path() else {
        return;
    };
    let Ok(mut line) = serde_json::to_string(record) else {
        return;
    };
    line.push('\n');
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = f.write_all(line.as_bytes());
    }
}

/// Resolve the structured gate-decision sink path: [`GATE_EVENTS_FILE_ENV`] if set,
/// else a `gate-events.jsonl` sibling of the [`RULES_FILE_ENV`] file. Returns `None`
/// when neither is configured (e.g. standalone/test runs) — then no sink is written and
/// only the stderr line carries the trace.
pub fn gate_events_sink_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os(GATE_EVENTS_FILE_ENV) {
        return Some(std::path::PathBuf::from(explicit));
    }
    let rules = std::env::var_os(RULES_FILE_ENV)?;
    let rules_path = std::path::PathBuf::from(rules);
    let dir = rules_path.parent()?;
    Some(dir.join("gate-events.jsonl"))
}

/// Resolve the clarification-request sink path: [`CLARIFY_REQUESTS_FILE_ENV`] if set,
/// else a `clarify-requests.jsonl` sibling of the [`RULES_FILE_ENV`] file. This is the
/// agent→run channel for Phase 3b: when the agent calls `ask_clarification`, the gateway
/// appends the STRUCTURED question here (one JSON object per line). The server reads this
/// file back after the agent returns, posts the question into the 3a clarify store, and
/// pauses the run at `AwaitingClarification`. Like [`gate_events_sink_path`], this is a
/// RECORDING channel only: writing a question never touches the repo and cannot change a
/// gate verdict. Returns `None` when nothing is configured (standalone/test runs).
pub fn clarify_requests_sink_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os(CLARIFY_REQUESTS_FILE_ENV) {
        return Some(std::path::PathBuf::from(explicit));
    }
    let rules = std::env::var_os(RULES_FILE_ENV)?;
    let rules_path = std::path::PathBuf::from(rules);
    let dir = rules_path.parent()?;
    Some(dir.join("clarify-requests.jsonl"))
}

/// FNV-1a 64-bit hash of `s`, returned as a 16-char hex string. Stable across
/// machines and Rust versions (unlike `DefaultHasher`). Matches the algorithm in
/// `camerata_persistence::content_hash` and `server::suppression::fnv1a`.
///
/// Used ONLY to hash the content of denied writes before recording them. The raw
/// content is never stored in the observability record (public repo safety).
fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}", h)
}

/// Build a [`GateDecisionRecord`] from a gate outcome. PURE: the verdict/rule/reason are
/// derived from the same `decision` string [`Gateway::gated_write`] returns, so this is
/// a faithful recording with zero decision logic of its own. Separated out so it is
/// unit-testable without a filesystem or clock (`ts_ms` is injected).
///
/// `content` is the write content: it is hashed (FNV-1a hex) and stored as
/// `content_hash` on DENY records. The raw content is NEVER stored.
pub fn build_gate_record(
    target: &str,
    decision: &str,
    ts_ms: u128,
    content: &str,
) -> GateDecisionRecord {
    // The decision string shape is one of:
    //   "ALLOWED: wrote N bytes to <path>"
    //   "ALLOWED but IO error on <path>: <e>"
    //   "DENIED [<rule>] path=<path>"
    //   "DENIED [JAIL: ...] path=<path>"
    if let Some(rest) = decision.strip_prefix("DENIED [") {
        // rest = "<rule>] path=<path>"  (rule may itself contain ']' for JAIL? no —
        // JAIL tag is "JAIL: outside the worktree"; the first ']' closes the bracket)
        let (rule, _after) = rest.split_once(']').unwrap_or((rest, ""));
        GateDecisionRecord {
            kind: default_gate_kind(),
            verdict: "deny".to_string(),
            target: target.to_string(),
            rule: Some(rule.to_string()),
            reason: decision.to_string(),
            ts_ms,
            content_hash: Some(fnv1a_hex(content)),
        }
    } else {
        GateDecisionRecord {
            kind: default_gate_kind(),
            verdict: "allow".to_string(),
            target: target.to_string(),
            rule: None,
            reason: decision.to_string(),
            ts_ms,
            content_hash: None,
        }
    }
}

/// Current Unix-epoch milliseconds (best-effort; `0` if the clock is before the epoch).
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Append a gate-decision record to the structured JSONL sink (best-effort).
///
/// Observability only: a write failure is ignored (the authoritative trace is the
/// stderr line). Never affects the gate's return value or any decision.
fn append_gate_record(record: &GateDecisionRecord) {
    let Some(path) = gate_events_sink_path() else {
        return;
    };
    let Ok(mut line) = serde_json::to_string(record) else {
        return;
    };
    line.push('\n');
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = f.write_all(line.as_bytes());
    }
}

/// Env var the orchestrator points at the worktree the agent is jailed to. When set,
/// `gated_write` refuses any write whose resolved target is outside this root, in CODE,
/// independent of any rule. This is the structural guard for "guard the guard":
/// `--add-dir` only scopes the agent's (disallowed) built-ins, not the gateway process
/// that performs the actual write, so the jail has to live here, not in a rule.
pub const WORKTREE_ROOT_ENV: &str = "CAMERATA_WORKTREE_ROOT";

/// Lexically normalize a path: resolve `.` and `..` WITHOUT touching the filesystem
/// (so it works for not-yet-created files). Symlink resolution is intentionally not
/// done; the agent cannot create symlinks because `Bash` is denied at the cage.
fn normalize_lexical(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `target` resolves to a path inside `root`. Relative targets resolve against
/// `root` (the agent's cwd is the worktree); absolute targets are taken as-is. Both are
/// lexically normalized, then a component-wise prefix check jails the result. An
/// absolute path outside the worktree, or a `..` climb above it, returns false.
fn within_jail(root: &std::path::Path, target: &str) -> bool {
    let t = std::path::Path::new(target);
    let abs = if t.is_absolute() {
        t.to_path_buf()
    } else {
        root.join(t)
    };
    normalize_lexical(&abs).starts_with(normalize_lexical(root))
}

/// Load the worktree jail root from the environment, canonicalized. `None` means no
/// jail is configured (standalone / test runs keep the rule-only behavior).
fn load_jail_root() -> Option<std::path::PathBuf> {
    let raw = std::env::var_os(WORKTREE_ROOT_ENV)?;
    let path = std::path::PathBuf::from(raw);
    match std::fs::canonicalize(&path) {
        Ok(canon) => Some(canon),
        Err(_) => Some(path),
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Absolute path to write.
    pub path: String,
    /// File content.
    pub content: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct AskClarificationArgs {
    /// The question to put to the human (a product/design decision you cannot make
    /// yourself).
    pub question: String,
    /// Structured options to choose from, each with a one-line benefit/drawback. Leave
    /// empty for a pure free-text question.
    #[serde(default)]
    pub options: Vec<ClarifyRequestOption>,
    /// Whether more than one option may be selected (checkboxes vs. radio).
    #[serde(default)]
    pub multi_select: bool,
    /// Whether the "Other" free-text escape is offered. Default true.
    #[serde(default = "default_true")]
    pub allow_free_text: bool,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DelegateArgs {
    /// A clear, well-scoped instruction for the delegate child to carry out.
    pub subtask: String,
    /// The tier to delegate to: `"fast"` or `"balanced"` (you are `strongest`).
    pub tier: String,
}

/// Load the session's rule-subset from `CAMERATA_RULES_FILE` if set, else fall
/// back to the verified default subset `["GOV-1"]`.
///
/// The file is a JSON array of rule-id strings, e.g. `["GOV-1"]`. This is the
/// data-driven delivery channel: the orchestrator's live rule selection arrives
/// as data, not code. A missing/unreadable/unparseable file fails CLOSED onto
/// the GOV-1 default rather than an empty (allow-everything) subset, so a
/// delivery glitch can never silently disable governance.
fn load_rule_subset() -> Vec<RuleId> {
    let Some(path) = std::env::var_os(RULES_FILE_ENV) else {
        eprintln!("[gateway] {RULES_FILE_ENV} unset; using default subset [GOV-1]");
        return vec![gov1_rule()];
    };
    let path = std::path::PathBuf::from(path);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Vec<RuleId>>(&text) {
            Ok(ids) if !ids.is_empty() => {
                eprintln!(
                    "[gateway] loaded {} rule(s) from {}: {}",
                    ids.len(),
                    path.display(),
                    ids.iter()
                        .map(|r| r.0.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                );
                ids
            }
            Ok(_) => {
                eprintln!(
                    "[gateway] {} parsed to an EMPTY subset; failing closed onto [GOV-1]",
                    path.display()
                );
                vec![gov1_rule()]
            }
            Err(e) => {
                eprintln!(
                    "[gateway] could not parse {} ({e}); failing closed onto [GOV-1]",
                    path.display()
                );
                vec![gov1_rule()]
            }
        },
        Err(e) => {
            eprintln!(
                "[gateway] could not read {} ({e}); failing closed onto [GOV-1]",
                path.display()
            );
            vec![gov1_rule()]
        }
    }
}

#[derive(Clone)]
pub struct Gateway {
    tool_router: ToolRouter<Self>,
    /// The session's rule-subset, delivered via `CAMERATA_RULES_FILE` at spawn.
    /// Shared (`Arc`) because rmcp clones the handler per connection.
    rule_subset: Arc<Vec<RuleId>>,
    /// The worktree the agent is jailed to (via `CAMERATA_WORKTREE_ROOT`). When set,
    /// `gated_write` refuses any target outside it, in code, before any rule runs.
    jail_root: Option<Arc<std::path::PathBuf>>,
    /// Orchestrator-mode config (`None` = the `delegate` tool is disabled). Set only
    /// when the gateway is launched for the LEAD agent with the delegate env vars.
    /// When `None`, `delegate` refuses; this is the per-process half of the gate.
    orchestrator: Option<Arc<OrchestratorConfig>>,
}

impl Gateway {
    /// Construct the gateway with the rule-subset + jail root read from the environment.
    pub fn new() -> Self {
        Self::with_rules(load_rule_subset())
    }

    /// Construct the gateway with an explicit rule-subset (used by `new` and by
    /// tests). Keeps the evaluation seam injectable. The jail root is read from the
    /// environment (unset = no jail).
    pub fn with_rules(rule_subset: Vec<RuleId>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            rule_subset: Arc::new(rule_subset),
            jail_root: load_jail_root().map(Arc::new),
            // Orchestrator mode is opt-in via env and OFF by default, so every
            // non-lead agent's gateway refuses `delegate`.
            orchestrator: OrchestratorConfig::from_env().map(Arc::new),
        }
    }

    /// Evaluate a write against the active rule-subset through the SHARED
    /// [`evaluate_call`], so this transport and the in-process `GovernedGateway`
    /// enforce byte-for-byte identical logic.
    ///
    /// BOTH `path` and `content` are forwarded into the `ToolCall.input`: path
    /// rules (GOV-1) key off `path`, and content rules
    /// (SEC-NO-HARDCODED-SECRETS-1, SEC-NO-RAW-SQL-CONCAT-1,
    /// ARCH-NO-SECRETS-IN-URL-1) key off `content`. Omitting `content` here
    /// would silently disable every content rule over the live transport — the
    /// gate would load them, report them, and never enforce them.
    fn evaluate(&self, path: &str, content: &str) -> Result<(), String> {
        let call = ToolCall {
            tool: "gated_write".to_string(),
            input: serde_json::json!({ "path": path, "content": content }),
        };
        match evaluate_call(&self.rule_subset, &call) {
            Decision::Allow => Ok(()),
            Decision::Deny { reason, .. } => Err(reason),
        }
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl Gateway {
    /// Write a file. Governed: the gate runs in Rust before any write happens.
    #[tool(
        name = "gated_write",
        description = "Write a file to disk. Governed by Camerata: the write is evaluated against the active rule-subset BEFORE execution; a denied write never touches the filesystem."
    )]
    pub async fn gated_write(&self, args: Parameters<WriteArgs>) -> String {
        let t0 = Instant::now();
        let WriteArgs { path, content } = args.0;

        // Structural worktree jail FIRST, independent of any rule: the gateway process
        // can write anywhere on the filesystem, so refuse any target outside the
        // worktree before evaluating content rules. This is what keeps the agent from
        // writing its own rules.json / system files via an absolute path.
        let decision = if self
            .jail_root
            .as_ref()
            .is_some_and(|root| !within_jail(root, &path))
        {
            format!("DENIED [JAIL: outside the worktree] path={path}")
        } else {
            match self.evaluate(&path, &content) {
                Err(rule) => format!("DENIED [{rule}] path={path}"),
                Ok(()) => match std::fs::write(&path, content.as_bytes()) {
                    Ok(()) => format!("ALLOWED: wrote {} bytes to {path}", content.len()),
                    Err(e) => format!("ALLOWED but IO error on {path}: {e}"),
                },
            }
        };

        let micros = t0.elapsed().as_micros();
        let line = format!("gated_write gate_decision={micros}us -> {decision}\n");
        eprint!("[gateway] {line}");

        // Structured gate-decision sink (observability only): append a faithful record
        // of THIS decision so the server can fold real gate decisions out of the
        // subprocess into the run's event stream. Records what was decided above; it
        // does not — and cannot — change the decision.
        // content_hash is the FNV-1a hex of the denied write's content on DENY records
        // (raw content is NEVER stored — public repo).
        append_gate_record(&build_gate_record(&path, &decision, now_ms(), &content));

        // Best-effort decision log next to the per-session rules file. The dir
        // may not exist in every environment; ignore failures (the stderr line
        // is the authoritative trace).
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/camerata-verify/gateway.log")
        {
            use std::io::Write as _;
            let _ = f.write_all(line.as_bytes());
        }
        decision
    }

    /// Raise a STRUCTURED clarifying question to the human (Phase 3b). READ-CLASS: this
    /// tool does NOT write to the repo, does NOT spawn anything, and does NOT escalate.
    /// It records the question to the per-session clarify-request sink (a sibling of the
    /// rules file, OUTSIDE the worktree jail) so the server can post it into the clarify
    /// store and PAUSE the run. The agent should call this ONCE for the single most
    /// blocking decision, then END its turn — the question is the pause point; a human
    /// answers and the run is RE-SPAWNED with the answer in context. Asking a question is
    /// not a write, so the gate is unaffected: there is no new write path here.
    #[tool(
        name = "ask_clarification",
        description = "Raise ONE structured clarifying question to the human when a product/design decision blocks you and you cannot make it yourself. Provide the question plus optional structured options (each with a short benefit/drawback). This does NOT write any files; it pauses the run for a human answer. After calling it, STOP and end your turn — you will be resumed with the answer. Do not guess past a real blocking decision."
    )]
    pub async fn ask_clarification(&self, args: Parameters<AskClarificationArgs>) -> String {
        let AskClarificationArgs {
            question,
            options,
            multi_select,
            allow_free_text,
        } = args.0;

        let record = ClarificationRequestRecord {
            question: question.clone(),
            options,
            multi_select,
            allow_free_text,
            ts_ms: now_ms(),
        };

        eprintln!(
            "[gateway] ask_clarification recorded ({} option(s)): {}",
            record.options.len(),
            question
        );

        // Record to the agent→run channel. RECORDING-ONLY: writes to the per-session
        // sink outside the worktree jail; touches no repo file; cannot change a gate
        // verdict. There is deliberately no filesystem write of repo content here.
        append_clarify_request(&record);

        format!(
            "CLARIFICATION RECORDED: \"{question}\". This question has been posted to the \
             human for an answer. STOP now and end your turn — the run will pause and you \
             will be resumed with the answer appended to your context. Do not attempt to \
             proceed past this decision."
        )
    }

    /// Governed delegation. ENABLED only in orchestrator mode (the lead agent's
    /// gateway). Spawns a SINGLE gated `claude -p` child on the requested tier's
    /// model — gated_write ONLY, `delegate` DISABLED, depth+1, same worktree —
    /// runs the subtask synchronously, and returns the child's full output. A
    /// child never calls "up": escalation is parent-driven (the orchestrator reads
    /// the result and decides). This is the ONLY governed spawn path; the raw CLI
    /// `Task` tool stays disallowed for every agent.
    #[tool(
        name = "delegate",
        description = "Delegate a well-scoped subtask to a lower tier. Spawns ONE gated child agent on the chosen tier ('fast' or 'balanced'), runs it in the same worktree, and returns its full output. If the child returns text starting with 'INCOMPLETE:' the work was above its tier — do it yourself or re-delegate higher. You cannot be delegated to; you are the strongest tier."
    )]
    pub async fn delegate(&self, args: Parameters<DelegateArgs>) -> String {
        let DelegateArgs { subtask, tier } = args.0;

        // Per-process gate: if this gateway was not launched in orchestrator mode,
        // refuse. Non-lead agents never set the orchestrator env, so their gateway
        // lands here. (Their --allowedTools also omits the tool, so this is the
        // belt to that suspenders.)
        let Some(config) = self.orchestrator.as_ref() else {
            eprintln!("[gateway] delegate REFUSED: gateway is not in orchestrator mode");
            return "DELEGATE REFUSED: this agent is not the orchestrator; \
                    delegation is not available. Do the work yourself."
                .to_string();
        };

        eprintln!(
            "[gateway] delegate tier={tier} depth={} max_depth={} subtask_len={}",
            config.depth,
            config.max_depth,
            subtask.len()
        );

        // Observability: record the dispatch (subtask → tier) into the structured sink
        // so the run's activity log shows the delegation. Recording only; the spawn
        // gate (orchestrator-mode + depth) is decided above, unchanged.
        append_gate_record(&GateDecisionRecord {
            kind: "delegate-dispatch".to_string(),
            verdict: "dispatch".to_string(),
            target: tier.clone(),
            rule: None,
            reason: format!("Delegated a subtask to the {tier} tier."),
            ts_ms: now_ms(),
            content_hash: None, // delegation records carry no content
        });

        let result = delegate::run_delegated(config, (*self.rule_subset).clone(), &subtask, &tier)
            .await
            .map(|output| output)
            .unwrap_or_else(|e| e.to_string());

        // Observability: record the return. A child that could not finish above its
        // tier signals with a leading `INCOMPLETE:` (per the delegate framing); surface
        // that as the verdict so the log shows the escalation honestly.
        let incomplete = result.contains("INCOMPLETE:");
        append_gate_record(&GateDecisionRecord {
            kind: "delegate-return".to_string(),
            verdict: if incomplete { "incomplete" } else { "returned" }.to_string(),
            target: tier.clone(),
            rule: None,
            reason: if incomplete {
                format!("Delegate ({tier}) returned INCOMPLETE — escalating.")
            } else {
                format!("Delegate ({tier}) returned its result.")
            },
            ts_ms: now_ms(),
            content_hash: None, // delegation records carry no content
        });

        result
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        // Register the server identity as `camerata`. (Claude Code derives the
        // tool's `mcp__<key>__` prefix from the mcp-config KEY, not this field;
        // we still set it so the server self-identifies consistently.)
        info.server_info = Implementation::new("camerata", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Camerata governance gateway. The ONLY way to write files is gated_write; \
             it is subject to governance rules enforced in-process."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subset = load_rule_subset();
    eprintln!(
        "[gateway] Camerata Rust MCP governance gateway up (rmcp 1.7, stdio); active subset: {}",
        subset
            .iter()
            .map(|r| r.0.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let service = Gateway::with_rules(subset).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod gate_sink_tests {
    use super::{build_gate_record, gate_events_sink_path, GateDecisionRecord};

    #[test]
    fn build_record_classifies_allow() {
        let r = build_gate_record(
            "crates/api/src/members_repo.rs",
            "ALLOWED: wrote 42 bytes to crates/api/src/members_repo.rs",
            123,
            "pub fn export_members() -> Vec<Member> { repo.all() }",
        );
        assert_eq!(r.verdict, "allow");
        assert!(r.rule.is_none());
        assert_eq!(r.target, "crates/api/src/members_repo.rs");
        assert_eq!(r.ts_ms, 123);
        assert!(r.reason.contains("ALLOWED"));
        // Allow records carry no content_hash (raw content must never be stored).
        assert!(r.content_hash.is_none());
    }

    #[test]
    fn build_record_classifies_deny_with_rule() {
        let r = build_gate_record(
            "crates/api/src/export_config.rs",
            "DENIED [SEC-NO-HARDCODED-SECRETS-1] path=crates/api/src/export_config.rs",
            7,
            "let token = \"secret\";",
        );
        assert_eq!(r.verdict, "deny");
        assert_eq!(r.rule.as_deref(), Some("SEC-NO-HARDCODED-SECRETS-1"));
        assert_eq!(r.target, "crates/api/src/export_config.rs");
        // Deny records carry a content_hash (FNV-1a hex, NOT the raw content).
        assert!(r.content_hash.is_some());
        let hash = r.content_hash.as_deref().unwrap();
        assert_eq!(hash.len(), 16, "FNV-1a hex is 16 chars");
        assert!(!hash.contains("secret"), "hash must not contain raw content");
    }

    #[test]
    fn build_record_classifies_jail_deny() {
        let r = build_gate_record(
            "/etc/cron.d/payload",
            "DENIED [JAIL: outside the worktree] path=/etc/cron.d/payload",
            0,
            "*/1 * * * * root sh -c id",
        );
        assert_eq!(r.verdict, "deny");
        assert_eq!(r.rule.as_deref(), Some("JAIL: outside the worktree"));
        assert!(r.content_hash.is_some());
    }

    #[test]
    fn record_round_trips_through_jsonl() {
        let r = build_gate_record("a/b.rs", "DENIED [GOV-1] path=a/b.rs", 9, "content");
        let line = serde_json::to_string(&r).unwrap();
        let back: GateDecisionRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back.verdict, "deny");
        assert_eq!(back.rule.as_deref(), Some("GOV-1"));
        assert_eq!(back.target, "a/b.rs");
        assert_eq!(back.ts_ms, 9);
        // content_hash round-trips; raw content is absent.
        assert!(back.content_hash.is_some());
    }

    #[test]
    fn allow_record_has_no_content_hash_after_jsonl_roundtrip() {
        // #[serde(default)] means a missing field deserializes to None.
        let r = build_gate_record(
            "a/b.rs",
            "ALLOWED: wrote 5 bytes to a/b.rs",
            1,
            "hello",
        );
        let line = serde_json::to_string(&r).unwrap();
        let back: GateDecisionRecord = serde_json::from_str(&line).unwrap();
        assert!(back.content_hash.is_none());
    }

    // Serialize the env-mutating test so it is order-independent within the binary.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn sink_path_prefers_explicit_env_then_derives_from_rules_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Explicit override wins.
        std::env::set_var(super::GATE_EVENTS_FILE_ENV, "/tmp/explicit-sink.jsonl");
        assert_eq!(
            gate_events_sink_path(),
            Some(std::path::PathBuf::from("/tmp/explicit-sink.jsonl"))
        );
        std::env::remove_var(super::GATE_EVENTS_FILE_ENV);

        // Otherwise it derives a gate-events.jsonl sibling of the rules file.
        std::env::set_var(super::RULES_FILE_ENV, "/tmp/session-1/rules.json");
        assert_eq!(
            gate_events_sink_path(),
            Some(std::path::PathBuf::from("/tmp/session-1/gate-events.jsonl"))
        );
        std::env::remove_var(super::RULES_FILE_ENV);
    }
}

#[cfg(test)]
mod clarify_tool_tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;

    // Serialize env-mutating clarify tests so they're order-independent in the binary.
    static CLARIFY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn clarify_sink_path_prefers_explicit_then_derives_from_rules_dir() {
        let _guard = CLARIFY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(CLARIFY_REQUESTS_FILE_ENV, "/tmp/explicit-clarify.jsonl");
        assert_eq!(
            clarify_requests_sink_path(),
            Some(std::path::PathBuf::from("/tmp/explicit-clarify.jsonl"))
        );
        std::env::remove_var(CLARIFY_REQUESTS_FILE_ENV);

        std::env::set_var(RULES_FILE_ENV, "/tmp/session-9/rules.json");
        assert_eq!(
            clarify_requests_sink_path(),
            Some(std::path::PathBuf::from("/tmp/session-9/clarify-requests.jsonl"))
        );
        std::env::remove_var(RULES_FILE_ENV);
    }

    #[tokio::test]
    async fn ask_clarification_records_a_structured_question_to_the_sink() {
        let _guard = CLARIFY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "cam-clarify-tool-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = dir.join("clarify-requests.jsonl");
        std::env::set_var(CLARIFY_REQUESTS_FILE_ENV, &sink);

        // A gateway with the default subset; ask_clarification is independent of rules.
        let gw = Gateway::with_rules(vec![gov1_rule()]);
        let out = gw
            .ask_clarification(Parameters(AskClarificationArgs {
                question: "Which timezone for reminders?".to_string(),
                options: vec![
                    ClarifyRequestOption {
                        label: "Org timezone".to_string(),
                        description: "one consistent send time".to_string(),
                    },
                    ClarifyRequestOption {
                        label: "Member timezone".to_string(),
                        description: "local hour per member".to_string(),
                    },
                ],
                multi_select: false,
                allow_free_text: true,
            }))
            .await;

        std::env::remove_var(CLARIFY_REQUESTS_FILE_ENV);

        // The tool's return tells the agent to STOP — the pause point.
        assert!(out.contains("CLARIFICATION RECORDED"));
        assert!(out.to_lowercase().contains("stop"));

        // The sink holds exactly one structured record, faithfully.
        let text = std::fs::read_to_string(&sink).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1);
        let rec: ClarificationRequestRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(rec.question, "Which timezone for reminders?");
        assert_eq!(rec.options.len(), 2);
        assert_eq!(rec.options[0].label, "Org timezone");
        assert!(!rec.multi_select);
        assert!(rec.allow_free_text);

        // CRITICAL: ask_clarification writes ONLY to the sink (outside any worktree), and
        // does NOT write any repo file — it created nothing else in the dir.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries.len(), 1, "only the sink file should exist: {entries:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod jail_tests {
    use super::within_jail;
    use std::path::Path;

    #[test]
    fn relative_paths_resolve_under_the_worktree() {
        let root = Path::new("/work/crate");
        assert!(within_jail(root, "src/lib.rs"));
        assert!(within_jail(root, "./src/api/members.rs"));
        assert!(within_jail(root, "Cargo.toml"));
    }

    #[test]
    fn absolute_paths_outside_the_worktree_are_jailed() {
        let root = Path::new("/work/crate");
        // The exact attack: the agent's own session config, a sibling of the worktree.
        assert!(!within_jail(root, "/work/session-1/rules.json"));
        assert!(!within_jail(root, "/work/session-1/gateway.json"));
        // System files.
        assert!(!within_jail(root, "/etc/passwd"));
        assert!(!within_jail(root, "/root/.ssh/authorized_keys"));
        // A sibling whose name merely starts with the root name (component-wise check).
        assert!(!within_jail(root, "/work/crate-evil/x.rs"));
    }

    #[test]
    fn dotdot_climbs_above_the_worktree_are_jailed() {
        let root = Path::new("/work/crate");
        assert!(!within_jail(root, "../session-1/rules.json"));
        assert!(!within_jail(root, "src/../../session-1/rules.json"));
        // A `..` that stays inside is fine.
        assert!(within_jail(root, "src/api/../lib.rs"));
    }

    #[test]
    fn absolute_paths_inside_the_worktree_are_allowed() {
        let root = Path::new("/work/crate");
        assert!(within_jail(root, "/work/crate/src/lib.rs"));
    }
}
