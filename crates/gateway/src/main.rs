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
use camerata_gateway::{enforced_gate_rules, evaluate_call, gov1_rule};
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

// The spawn modules now live in the gateway LIBRARY (so the in-process server path can
// reuse the SAME gated `run_delegated`/`run_fan_out` primitives). The binary references
// them through the crate's own lib rather than re-declaring the module files (which would
// compile them twice under two distinct paths).
use camerata_gateway::{delegate, fan_out};
use delegate::OrchestratorConfig;
use fan_out::FanOutEntry;

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

/// Optional env override for the escalation-request JSONL sink path (the agent-driven escalation
/// channel). When set, every `raise_escalation` call is appended (one JSON object per line) to this
/// file. When unset, the path is an `escalation-requests.jsonl` sibling of the per-session rules
/// file (same derivation as the clarify sink), so the run that wrote the rules file knows where to
/// read escalations from. This is the agent→run channel for rule-driven escalation; it never writes
/// to the repo.
pub const ESCALATION_REQUESTS_FILE_ENV: &str = "CAMERATA_ESCALATION_REQUESTS_FILE";

/// Optional env override for the memory-proposal JSONL sink (#112, Layer 3). When set, every
/// `propose_memory` call is appended to this file; else it derives a `memory-proposals.jsonl`
/// sibling of the rules file. The agent→run channel for proposed project-memory learnings; never
/// writes to the repo.
pub const MEMORY_PROPOSALS_FILE_ENV: &str = "CAMERATA_MEMORY_PROPOSALS_FILE";

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
    /// SHA-256 hex hash of the denied content (NEVER the raw content — public repo).
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

/// One escalation the GATED agent raises mid-run when its work meets the escalation CONDITION of a
/// rule the project selected (the rule-agnostic, agent-driven half of the escalation gate). Wire
/// shape between the gateway subprocess and the server: the server reads these back, resolves the
/// rule's AUTHORITATIVE severity from the corpus (the agent cannot downgrade a hard-pause), and
/// either pauses the run for human review or logs + continues. Recording-only — raising an
/// escalation is not a repo write and cannot change a gate verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRequestRecord {
    /// The rule id whose escalation condition was met (e.g. "ORCH-ONE-WAY-DOOR-1").
    pub rule_id: String,
    /// The agent's account of WHAT met the condition (the specific action or decision).
    pub condition_met: String,
    /// The agent's justification / the recommendation it would make, for the human reviewing.
    #[serde(default)]
    pub justification: String,
    /// Unix-epoch milliseconds when the escalation was recorded.
    pub ts_ms: u128,
}

/// Append an escalation-request record to the escalation JSONL sink (best-effort, recording-only).
/// Mirrors [`append_clarify_request`]: appends to the per-session sink (a sibling of the rules file,
/// OUTSIDE the worktree jail); a write failure is ignored; never touches the repo.
fn append_escalation_request(record: &EscalationRequestRecord) {
    let Some(path) = escalation_requests_sink_path() else {
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

/// One PROJECT-MEMORY learning the agent PROPOSES at run end (#112, Layer 3). Wire shape between the
/// gateway subprocess and the server: the server reads these back and appends them as `Proposed`
/// memory entries for the human to curate. Recording-only — proposing a learning is not a repo write
/// and cannot change a gate verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryProposalRecord {
    /// The kind of learning: `"decision"` | `"pattern"` | `"gotcha"` | `"constraint"`.
    #[serde(default)]
    pub kind: String,
    /// The learning itself, one fact.
    pub text: String,
    /// Unix-epoch milliseconds when the proposal was recorded.
    pub ts_ms: u128,
}

/// Append a memory-proposal record to the memory JSONL sink (best-effort, recording-only).
fn append_memory_proposal(record: &MemoryProposalRecord) {
    let Some(path) = memory_proposals_sink_path() else {
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

/// Resolve the escalation-request sink path: [`ESCALATION_REQUESTS_FILE_ENV`] if set, else an
/// `escalation-requests.jsonl` sibling of the per-session rules file. A RECORDING channel only,
/// like the clarify sink. Returns `None` when nothing is configured (standalone/test runs).
pub fn escalation_requests_sink_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os(ESCALATION_REQUESTS_FILE_ENV) {
        return Some(std::path::PathBuf::from(explicit));
    }
    let rules = std::env::var_os(RULES_FILE_ENV)?;
    let rules_path = std::path::PathBuf::from(rules);
    let dir = rules_path.parent()?;
    Some(dir.join("escalation-requests.jsonl"))
}

/// Resolve the memory-proposal sink path: [`MEMORY_PROPOSALS_FILE_ENV`] if set, else a
/// `memory-proposals.jsonl` sibling of the per-session rules file. A RECORDING channel only.
pub fn memory_proposals_sink_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os(MEMORY_PROPOSALS_FILE_ENV) {
        return Some(std::path::PathBuf::from(explicit));
    }
    let rules = std::env::var_os(RULES_FILE_ENV)?;
    let rules_path = std::path::PathBuf::from(rules);
    let dir = rules_path.parent()?;
    Some(dir.join("memory-proposals.jsonl"))
}

/// SHA-256 hash of `s`, returned as a 64-char lowercase hex string. Matches
/// `camerata_persistence::content_hash` so a given offending slice hashes identically
/// whether the gate (deny) or the floor scan records it.
///
/// Used ONLY to hash the content of denied writes before recording them. The raw content
/// is never stored in the observability record. SHA-256 (preimage-resistant, not a fast
/// fingerprint) is deliberate: the enforcement ledger is portable proof, and a denied
/// slice MIGHT contain a secret — a recoverable hash would defeat the point.
fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    format!("{digest:x}")
}

/// Build a [`GateDecisionRecord`] from a gate outcome. PURE: the verdict/rule/reason are
/// derived from the same `decision` string [`Gateway::gated_write`] returns, so this is
/// a faithful recording with zero decision logic of its own. Separated out so it is
/// unit-testable without a filesystem or clock (`ts_ms` is injected).
///
/// `content` is the write content: it is hashed (SHA-256 hex) and stored as
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
            content_hash: Some(sha256_hex(content)),
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
/// (so it works for not-yet-created files).
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

/// Resolve symlinks in the EXISTING ancestor prefix of `p`, returning the canonical
/// existing prefix rejoined with the remaining not-yet-created tail. Walk up from `p`
/// to the deepest ancestor that exists on disk, `canonicalize` it (which resolves every
/// symlink in that prefix), then re-append the stripped tail verbatim. The tail names a
/// path that does not exist yet, so none of its components can themselves be symlinks.
///
/// This is what stops a repo-committed symlink directory (e.g. `worktree/link -> /etc`)
/// from smuggling a gated write outside the jail: a purely lexical `starts_with` check
/// would see `worktree/link/passwd` as in-jail, but canonicalizing the existing
/// `worktree/link` prefix exposes the real `/private/etc/passwd` destination.
fn canonicalize_existing_prefix(p: &std::path::Path) -> std::path::PathBuf {
    let mut ancestor = p.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&ancestor) {
            let mut resolved = canon;
            for seg in tail.iter().rev() {
                resolved.push(seg);
            }
            return resolved;
        }
        match ancestor.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                if !ancestor.pop() {
                    return normalize_lexical(p);
                }
            }
            // No file_name (root `/` or a prefix) that still fails to canonicalize:
            // fall back to the lexical form rather than looping forever.
            None => return normalize_lexical(p),
        }
    }
}

/// Resolve `target` to the absolute path it must be WRITTEN to inside `root`, or `None`
/// when it escapes the jail. Relative targets resolve against `root` (the jail root, NOT
/// the process cwd — so the returned path is what the caller must actually write to,
/// keeping check and effect the same path: GATE-F4). Absolute targets are taken as-is.
/// Both the target and the root have symlinks in their existing prefix resolved
/// ([`canonicalize_existing_prefix`]) BEFORE the component-wise prefix check, so a
/// symlinked directory component cannot smuggle a write outside the jail (GATE-F1), and a
/// legitimate absolute write under a symlinked root prefix (e.g. macOS `/tmp` ->
/// `/private/tmp`) is not falsely denied (GATE-F5). An absolute path outside the
/// worktree, or a `..` climb above it, returns `None`.
fn resolve_in_jail(root: &std::path::Path, target: &str) -> Option<std::path::PathBuf> {
    let t = std::path::Path::new(target);
    let abs = if t.is_absolute() {
        t.to_path_buf()
    } else {
        root.join(t)
    };
    let target_resolved = canonicalize_existing_prefix(&normalize_lexical(&abs));
    let root_resolved = canonicalize_existing_prefix(&normalize_lexical(root));
    target_resolved
        .starts_with(&root_resolved)
        .then_some(target_resolved)
}

/// Whether `target` resolves to a path inside `root`. Thin boolean wrapper over
/// [`resolve_in_jail`]; use `resolve_in_jail` directly when you also need the resolved
/// write path (so the CHECK and the WRITE hit the same location). Retained for the jail
/// unit tests and as a boolean-only convenience.
#[allow(dead_code)]
fn within_jail(root: &std::path::Path, target: &str) -> bool {
    resolve_in_jail(root, target).is_some()
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
pub struct RaiseEscalationArgs {
    /// The id of the rule whose escalation condition your work meets (from the ESCALATION
    /// CONDITIONS list in your task), e.g. "ORCH-ONE-WAY-DOOR-1".
    pub rule_id: String,
    /// What SPECIFICALLY met the condition — the action or decision you are about to make.
    pub condition_met: String,
    /// Your justification / the recommendation you would make, for the human reviewing.
    #[serde(default)]
    pub justification: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ProposeMemoryArgs {
    /// The kind of learning: "decision" | "pattern" | "gotcha" | "constraint".
    #[serde(default)]
    pub kind: String,
    /// The learning itself, ONE durable fact worth carrying to future work on this project.
    pub text: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DelegateArgs {
    /// A clear, well-scoped instruction for the delegate child to carry out.
    pub subtask: String,
    /// The tier to delegate to: `"fast"` or `"balanced"` (you are `strongest`).
    pub tier: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct FanOutArgs {
    /// The list of per-repo/per-partition work items to run concurrently. Each
    /// entry targets one repo partition; no two entries may share the same repo
    /// (write-isolation invariant).
    pub entries: Vec<FanOutEntry>,
}

/// Load the session's rule-subset from `CAMERATA_RULES_FILE` if set, else fall
/// back to the verified default subset `["GOV-1"]`.
///
/// The file is a JSON array of rule-id strings, e.g. `["GOV-1"]`. This is the
/// data-driven delivery channel: the orchestrator's live rule selection arrives
/// as data, not code.
///
/// A file that is SET but unreadable/unparseable/empty is a delivery glitch on a
/// governed run, so it fails CLOSED onto the FULL enforced floor
/// (`enforced_gate_rules()`), not onto `[GOV-1]`. `[GOV-1]` alone is only a
/// synthetic verification fixture, so falling back to it would silently shed the
/// entire SEC-* floor for the session (fail-OPEN). Only the truly unconfigured
/// case (env var unset, i.e. the gateway run standalone with no orchestrator
/// delivery) uses the minimal `[GOV-1]` default.
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
                    "[gateway] {} parsed to an EMPTY subset; failing closed onto the full floor",
                    path.display()
                );
                enforced_gate_rules()
            }
            Err(e) => {
                eprintln!(
                    "[gateway] could not parse {} ({e}); failing closed onto the full floor",
                    path.display()
                );
                enforced_gate_rules()
            }
        },
        Err(e) => {
            eprintln!(
                "[gateway] could not read {} ({e}); failing closed onto the full floor",
                path.display()
            );
            enforced_gate_rules()
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
        //
        // When jailed, WRITE to the jail-resolved path, not the raw `path`: a relative
        // target resolves against the jail root here, but `std::fs::write` on the raw
        // relative path would resolve against the process cwd, so a cwd != jail-root
        // would check one file and write another (GATE-F4). `write_target` is the exact
        // path the jail check validated.
        let jail_check = match &self.jail_root {
            Some(root) => match resolve_in_jail(root, &path) {
                Some(resolved) => Ok(resolved),
                None => Err(()),
            },
            None => Ok(std::path::PathBuf::from(&path)),
        };
        let decision = match jail_check {
            Err(()) => format!("DENIED [JAIL: outside the worktree] path={path}"),
            Ok(write_target) => match self.evaluate(&path, &content) {
                Err(rule) => format!("DENIED [{rule}] path={path}"),
                Ok(()) => match std::fs::write(&write_target, content.as_bytes()) {
                    Ok(()) => format!("ALLOWED: wrote {} bytes to {path}", content.len()),
                    Err(e) => format!("ALLOWED but IO error on {path}: {e}"),
                },
            },
        };

        let micros = t0.elapsed().as_micros();
        let line = format!("gated_write gate_decision={micros}us -> {decision}\n");
        eprint!("[gateway] {line}");

        // Structured gate-decision sink (observability only): append a faithful record
        // of THIS decision so the server can fold real gate decisions out of the
        // subprocess into the run's event stream. Records what was decided above; it
        // does not — and cannot — change the decision.
        // content_hash is the SHA-256 hex of the denied write's content on DENY records
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

    /// Raise an escalation when the agent's work meets the escalation CONDITION of a selected rule
    /// (the rule-agnostic, agent-driven half of the escalation gate). Records the escalation to the
    /// per-session sink (outside the worktree jail) so the server can resolve the rule's severity
    /// from the corpus and either PAUSE the run for human review (hard-pause) or log it and let the
    /// run CONTINUE (soft-flag). Raising an escalation is not a write; the gate is unaffected.
    #[tool(
        name = "raise_escalation",
        description = "Raise an escalation when your work meets the escalation CONDITION of one of the rules listed under 'ESCALATION CONDITIONS' in your task. Give the rule_id, what specifically met the condition, and your justification. This does NOT write any files. For a HARD-PAUSE rule, STOP and end your turn after calling it — the run pauses for human review and you will be resumed with the decision. For a SOFT-FLAG rule you may continue. When unsure whether a rule's condition is met, raise it rather than proceed past it."
    )]
    pub async fn raise_escalation(&self, args: Parameters<RaiseEscalationArgs>) -> String {
        let RaiseEscalationArgs {
            rule_id,
            condition_met,
            justification,
        } = args.0;

        let record = EscalationRequestRecord {
            rule_id: rule_id.clone(),
            condition_met,
            justification,
            ts_ms: now_ms(),
        };

        eprintln!("[gateway] raise_escalation recorded for rule {rule_id}");

        // Record to the agent→run channel. RECORDING-ONLY: writes to the per-session sink outside
        // the worktree jail; touches no repo file; cannot change a gate verdict.
        append_escalation_request(&record);

        format!(
            "ESCALATION RECORDED for rule `{rule_id}`. The run's reviewer will resolve it. If this \
             rule is a HARD-PAUSE, STOP now and end your turn — the run will pause for human review \
             and you will be resumed with the decision. If it is a SOFT-FLAG, you may continue."
        )
    }

    /// Propose a PROJECT-MEMORY learning (#112, Layer 3): a durable fact (a decision that should
    /// hold, a pattern established, a gotcha learned, a constraint to respect) worth carrying to
    /// future work on this project. Records it to the per-session sink (outside the worktree jail);
    /// the server appends it as a PROPOSED entry the human curates. Not a write; the gate is intact.
    #[tool(
        name = "propose_memory",
        description = "Propose a durable PROJECT-MEMORY learning worth carrying to future work here: a decision that should hold, a pattern you established, a gotcha you hit, or a constraint to respect. Give a `kind` (decision|pattern|gotcha|constraint) and one-fact `text`. This does NOT write any files and does NOT pause the run — it records a suggestion the human curates. Only propose genuinely durable, non-obvious learnings; do not propose routine restatements of the task."
    )]
    pub async fn propose_memory(&self, args: Parameters<ProposeMemoryArgs>) -> String {
        let ProposeMemoryArgs { kind, text } = args.0;
        let record = MemoryProposalRecord {
            kind,
            text: text.clone(),
            ts_ms: now_ms(),
        };
        eprintln!("[gateway] propose_memory recorded");
        append_memory_proposal(&record);
        format!("MEMORY PROPOSED: \"{text}\". Recorded for the human to curate; continue your work.")
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

    /// Fan-out: concurrent multi-repo / multi-partition dispatch. ENABLED only in
    /// orchestrator mode (the lead agent's gateway). Spawns ALL workers concurrently
    /// (one per entry), each jailed to its own repo partition, each with `gated_write`
    /// ONLY — `fan_out` and `delegate` are DISABLED for every worker. Returns a
    /// formatted summary of all worker results. Use when a unit-of-work spans
    /// MULTIPLE repos and the partitions are independent (no cross-partition write
    /// ordering required). After all workers return, Camerata (not the agents) is the
    /// sole committer: read the results and drive commits yourself.
    #[tool(
        name = "fan_out",
        description = "Concurrently dispatch independent subtasks to per-repo worker agents. \
                       Each entry targets ONE repo partition; workers run in parallel with \
                       write-isolated jails (no worker can touch another's repo). \
                       NO two entries may share the same repo (partition collision). \
                       Workers have gated_write ONLY — they cannot fan_out or delegate further. \
                       Returns a summary of all worker outputs. You (the orchestrator) drive \
                       commits after all workers complete."
    )]
    pub async fn fan_out(&self, args: Parameters<FanOutArgs>) -> String {
        let FanOutArgs { entries } = args.0;

        // Per-process gate: if this gateway was not launched in orchestrator mode, refuse.
        let Some(config) = self.orchestrator.as_ref() else {
            eprintln!("[gateway] fan_out REFUSED: gateway is not in orchestrator mode");
            return "FAN_OUT REFUSED: this agent is not the orchestrator; \
                    fan_out is not available. Do the work yourself or use delegate."
                .to_string();
        };

        let entry_count = entries.len();
        eprintln!(
            "[gateway] fan_out dispatching {} entries depth={} max_depth={}",
            entry_count, config.depth, config.max_depth,
        );

        // Observability: record the fan-out dispatch in the structured sink.
        append_gate_record(&GateDecisionRecord {
            kind: "fan-out-dispatch".to_string(),
            verdict: "dispatch".to_string(),
            target: format!("{} repos", entry_count),
            rule: None,
            reason: format!("Fan-out dispatching {entry_count} concurrent workers."),
            ts_ms: now_ms(),
            content_hash: None,
        });

        // Dispatch all workers concurrently.
        let result = fan_out::run_fan_out(
            config,
            (*self.rule_subset).clone(),
            entries,
        )
        .await;

        // Observability: record the fan-out return.
        let (summary, incomplete_count) = match &result {
            Ok(results) => {
                let incomplete = results.iter().filter(|r| r.incomplete).count();
                let summary = format!(
                    "Fan-out complete: {total} workers returned, {incomplete} INCOMPLETE.",
                    total = results.len(),
                    incomplete = incomplete,
                );
                (summary, incomplete)
            }
            Err(e) => (e.to_string(), 0),
        };

        append_gate_record(&GateDecisionRecord {
            kind: "fan-out-return".to_string(),
            verdict: if incomplete_count > 0 {
                "incomplete".to_string()
            } else {
                "returned".to_string()
            },
            target: format!("{} repos", entry_count),
            rule: None,
            reason: summary.clone(),
            ts_ms: now_ms(),
            content_hash: None,
        });

        // Build the formatted output: one section per worker result.
        match result {
            Err(e) => format!("FAN_OUT REFUSED: {e}"),
            Ok(results) => {
                let mut out = format!(
                    "[fan-out complete: {} worker(s), {} INCOMPLETE]\n\n",
                    results.len(),
                    incomplete_count,
                );
                for r in &results {
                    out.push_str(&format!(
                        "--- repo={} domain={} incomplete={} ---\n{}\n\n",
                        r.repo, r.domain, r.incomplete, r.output
                    ));
                }
                // Assembly is available via assemble_by_repo for the commit phase.
                // The orchestrator reads this output and drives commits.
                out
            }
        }
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
    use super::{
        build_gate_record, enforced_gate_rules, gate_events_sink_path, load_rule_subset, now_ms,
        GateDecisionRecord,
    };

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
        // Deny records carry a content_hash (SHA-256 hex, NOT the raw content).
        assert!(r.content_hash.is_some());
        let hash = r.content_hash.as_deref().unwrap();
        assert_eq!(hash.len(), 64, "SHA-256 hex is 64 chars");
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

    #[test]
    fn rules_file_load_failure_fails_closed_onto_the_full_floor() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let floor = enforced_gate_rules();
        // Sanity: the full floor is more than the synthetic GOV-1 fixture, and
        // includes the SEC-* content rules that a fail-OPEN [GOV-1] would shed.
        assert!(floor.len() > 1);
        assert!(floor.iter().any(|r| r.0.starts_with("SEC-")));

        let dir = std::env::temp_dir().join(format!(
            "cam-rules-load-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // 1) File set but missing/unreadable -> full floor, NOT [GOV-1].
        std::env::set_var(super::RULES_FILE_ENV, dir.join("does-not-exist.json"));
        assert_eq!(load_rule_subset(), floor, "read failure must fail closed");

        // 2) File set but unparseable -> full floor.
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        std::env::set_var(super::RULES_FILE_ENV, &bad);
        assert_eq!(load_rule_subset(), floor, "parse failure must fail closed");

        // 3) File parses to an EMPTY subset -> full floor (never allow-everything).
        let empty = dir.join("empty.json");
        std::fs::write(&empty, "[]").unwrap();
        std::env::set_var(super::RULES_FILE_ENV, &empty);
        assert_eq!(load_rule_subset(), floor, "empty subset must fail closed");

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
    use super::{resolve_in_jail, within_jail};
    use std::path::Path;

    // GATE-F4: a relative target must resolve against the JAIL ROOT (what the write then
    // uses), never the process cwd, so check and effect hit the same file.
    #[test]
    fn resolve_in_jail_resolves_relative_targets_against_the_root() {
        let root = std::env::temp_dir().join(format!(
            "cam-jail-f4-{}-{}",
            std::process::id(),
            super::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let canon = std::fs::canonicalize(&root).unwrap();

        let resolved = resolve_in_jail(&root, "sub/file.rs").expect("relative in-jail target");
        assert_eq!(resolved, canon.join("sub").join("file.rs"));

        // Out-of-jail absolute target -> None.
        assert!(resolve_in_jail(&root, "/etc/passwd").is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

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

    // GATE-F1: a repo-committed symlink directory must NOT let a gated write escape the
    // jail. A purely lexical check would pass `worktree/escape/loot`; resolving the
    // existing symlinked prefix exposes the real out-of-jail destination.
    #[test]
    fn symlinked_directory_component_cannot_escape_the_jail() {
        let base = std::env::temp_dir().join(format!(
            "cam-jail-f1-{}-{}",
            std::process::id(),
            super::now_ms()
        ));
        let worktree = base.join("worktree");
        let outside = base.join("outside");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // A symlink INSIDE the worktree pointing at a sibling outside it.
        let link = worktree.join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(not(unix))]
        {
            let _ = &link;
            return; // symlink semantics differ on non-unix; skip.
        }

        // Writing THROUGH the symlink lands outside the worktree -> must be denied,
        // whether the final file exists yet or not.
        let via_link_existing = link.join("loot");
        std::fs::write(&via_link_existing, b"x").unwrap();
        assert!(
            !within_jail(&worktree, via_link_existing.to_str().unwrap()),
            "write through an in-jail symlink to an outside dir must be denied (existing target)"
        );
        let via_link_new = link.join("new-file");
        assert!(
            !within_jail(&worktree, via_link_new.to_str().unwrap()),
            "write through an in-jail symlink to an outside dir must be denied (new target)"
        );

        // A real in-jail write is still allowed.
        assert!(within_jail(&worktree, worktree.join("src.rs").to_str().unwrap()));

        let _ = std::fs::remove_dir_all(&base);
    }

    // GATE-F5: a legitimate absolute write under a SYMLINKED root prefix (the real macOS
    // case: /tmp -> /private/tmp) must NOT be falsely denied, now that the target's
    // existing prefix is canonicalized on the same footing as the root.
    #[test]
    fn absolute_write_under_a_symlinked_root_prefix_is_allowed() {
        // temp_dir() on macOS is under /var -> /private/var (a symlinked prefix).
        let root = std::env::temp_dir().join(format!(
            "cam-jail-f5-{}-{}",
            std::process::id(),
            super::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("sub").join("file.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();

        // The caller passes the UN-canonicalized root (as the env var might hold it),
        // and an absolute in-jail target: it must resolve as in-jail.
        assert!(
            within_jail(&root, target.to_str().unwrap()),
            "legit absolute write under a symlinked root prefix must be allowed"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod fan_out_gate_invariant_tests {
    use camerata_agent::{
        allowed_tools_for_role, allowed_tools_for_role_with_mode, FAN_OUT_TOOL, DELEGATE_TOOL,
        GATED_WRITE_TOOL,
    };
    use camerata_core::{Role, RuleId};

    fn role() -> Role {
        Role {
            name: "orchestrator-test".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["/work/project".to_string()],
        }
    }

    #[test]
    fn fan_out_tool_is_orchestrator_only_not_in_non_orchestrator_tools() {
        // The non-orchestrator allowed list must NOT include fan_out.
        // This is the depth-1 / no-recursive-fan-out gate-invariant test.
        let tools = allowed_tools_for_role(&role());
        assert!(
            !tools.iter().any(|t| t == FAN_OUT_TOOL),
            "fan_out must be absent from non-orchestrator tool list"
        );
        // Explicit mode=false agrees.
        let tools_false = allowed_tools_for_role_with_mode(&role(), false);
        assert!(!tools_false.iter().any(|t| t == FAN_OUT_TOOL));
    }

    #[test]
    fn fan_out_tool_present_in_orchestrator_mode() {
        // The orchestrator tool list MUST include fan_out alongside delegate.
        let tools = allowed_tools_for_role_with_mode(&role(), true);
        assert!(
            tools.iter().any(|t| t == FAN_OUT_TOOL),
            "orchestrator must have fan_out in its tool list"
        );
        assert!(
            tools.iter().any(|t| t == DELEGATE_TOOL),
            "orchestrator must still have delegate"
        );
        assert!(
            tools.iter().any(|t| t == GATED_WRITE_TOOL),
            "orchestrator must still have gated_write"
        );
    }

    #[test]
    fn fan_out_tool_constant_has_correct_mcp_prefix() {
        // Sanity-check the constant matches the server key / tool name pattern.
        assert_eq!(FAN_OUT_TOOL, "mcp__camerata__fan_out");
    }
}

#[cfg(test)]
mod integration_gate_module_tests {
    use camerata_gateway::integration_gate::{
        check_integration_gate, IntegrationGateInput, IntegrationGateResult,
    };
    use super::fan_out::WorkerResult;

    fn no_workers() -> Vec<WorkerResult> {
        vec![]
    }

    #[test]
    fn integration_gate_no_contract_is_no_contract_required() {
        let input = IntegrationGateInput {
            contract: None,
            assembled: &no_workers(),
        };
        assert_eq!(
            check_integration_gate(&input),
            IntegrationGateResult::NoContractRequired
        );
    }

    #[test]
    fn integration_gate_empty_contract_bounces() {
        let input = IntegrationGateInput {
            contract: Some(""),
            assembled: &no_workers(),
        };
        assert!(matches!(
            check_integration_gate(&input),
            IntegrationGateResult::BounceToOrchestrator { .. }
        ));
    }

    #[test]
    fn integration_gate_nonempty_contract_is_pending() {
        let input = IntegrationGateInput {
            contract: Some("GET /api/orgs → [{id, name}]"),
            assembled: &no_workers(),
        };
        assert!(matches!(
            check_integration_gate(&input),
            IntegrationGateResult::Pending { .. }
        ));
    }
}
