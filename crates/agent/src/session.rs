//! Per-session spawn plumbing for the LIVE governed run.
//!
//! This module is the orchestrator side of the per-session rule-delivery
//! contract that the gateway's `main.rs` reads. For one agent session it:
//!
//!   1. computes/accepts the session's rule-subset (a `Vec<RuleId>` from the
//!      role — the live selection, NOT hard-coded in the gateway),
//!   2. writes that subset to a per-session rules JSON file,
//!   3. generates an mcp-config that launches the built `camerata-gateway`
//!      binary with env `CAMERATA_RULES_FILE` pointing at the rules file,
//!   4. hands back a [`ClaudeCliDriver`] wired to that config.
//!
//! The gateway reads `CAMERATA_RULES_FILE` on startup and evaluates every tool
//! call against it. This is the stdio binding of the same `GovernanceGateway`
//! logic; the embedded streamable-http transport (sharing the orchestrator's
//! live session map with no file) is the clean refinement for a later slice.

use std::path::{Path, PathBuf};

use camerata_core::{Role, SessionId};
use serde::Serialize;
use thiserror::Error;

use crate::{ClaudeCliDriver, GATED_WRITE_TOOL};

/// The env var name the gateway reads its per-session rule-subset from. Kept in
/// sync with `camerata-gateway`'s `RULES_FILE_ENV`.
pub const RULES_FILE_ENV: &str = "CAMERATA_RULES_FILE";

/// The env var name the gateway reads its worktree jail root from. Kept in sync with
/// the gateway binary's `WORKTREE_ROOT_ENV`. When set, the gateway refuses any
/// `gated_write` whose target resolves outside this worktree (a code-level jail,
/// independent of any rule).
pub const WORKTREE_ROOT_ENV: &str = "CAMERATA_WORKTREE_ROOT";

/// The env var name the gateway writes its structured gate-decision JSONL sink to.
/// Kept in sync with the gateway binary's `GATE_EVENTS_FILE_ENV`.
///
/// LIFECYCLE-10: this is threaded PER-SPAWN through the MCP config's `env` block
/// (not the parent process env). Each governed run points its own gateway
/// subprocesses at its own sink, so two concurrent runs never read each other's
/// gate provenance. Observability only — it routes WHERE decisions are recorded,
/// never what is decided.
pub const GATE_EVENTS_FILE_ENV: &str = "CAMERATA_GATE_EVENTS_FILE";

/// The mcp-config server KEY. Claude Code namespaces the tool as
/// `mcp__<key>__<tool>`; this key plus the gateway's `gated_write` tool yield
/// exactly [`GATED_WRITE_TOOL`] (`mcp__camerata__gated_write`).
pub const MCP_SERVER_KEY: &str = "camerata";

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("failed to write {what} at {path}: {source}")]
    Write {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to serialize {what}: {source}")]
    Serialize {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

// ─── mcp-config shape (serialized to the per-session gateway.json) ────────────

/// One server entry under `mcpServers`. Matches Claude Code's mcp-config schema.
#[derive(Debug, Serialize)]
struct McpServerEntry {
    command: String,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
}

/// The top-level mcp-config document.
#[derive(Debug, Serialize)]
struct McpConfig {
    #[serde(rename = "mcpServers")]
    mcp_servers: std::collections::BTreeMap<String, McpServerEntry>,
}

/// Build the mcp-config JSON string that launches `gateway_bin` as the
/// `camerata` MCP server with `CAMERATA_RULES_FILE` = `rules_file`.
///
/// Pure (no I/O) so it is unit-testable. The server key is [`MCP_SERVER_KEY`],
/// which is what makes the agent-visible tool name [`GATED_WRITE_TOOL`].
pub fn render_mcp_config(
    gateway_bin: &Path,
    rules_file: &Path,
    worktree: Option<&Path>,
    gate_events_file: Option<&Path>,
) -> Result<String, SessionError> {
    let mut env = std::collections::BTreeMap::new();
    env.insert(RULES_FILE_ENV.to_string(), rules_file.display().to_string());
    if let Some(wt) = worktree {
        env.insert(WORKTREE_ROOT_ENV.to_string(), wt.display().to_string());
    }
    // LIFECYCLE-10: point this gateway subprocess at the run's OWN gate-events sink,
    // per-spawn, via its explicit MCP-config env — never the shared parent process env.
    if let Some(sink) = gate_events_file {
        env.insert(GATE_EVENTS_FILE_ENV.to_string(), sink.display().to_string());
    }

    let mut servers = std::collections::BTreeMap::new();
    servers.insert(
        MCP_SERVER_KEY.to_string(),
        McpServerEntry {
            command: gateway_bin.display().to_string(),
            args: vec![],
            env,
        },
    );

    let config = McpConfig {
        mcp_servers: servers,
    };
    serde_json::to_string_pretty(&config).map_err(|source| SessionError::Serialize {
        what: "mcp-config",
        source,
    })
}

/// Serialize a role's rule-subset to the JSON array the gateway expects, e.g.
/// `["GOV-1"]`. The gateway deserializes this straight into `Vec<RuleId>`.
pub fn render_rules_file(role: &Role) -> Result<String, SessionError> {
    serde_json::to_string_pretty(&role.rule_subset).map_err(|source| SessionError::Serialize {
        what: "rules-file",
        source,
    })
}

/// Everything written to disk for one session, plus the driver wired to it.
pub struct SessionSpawn {
    /// The session id this spawn is bound to.
    pub session_id: SessionId,
    /// Path to the per-session rules JSON file (`CAMERATA_RULES_FILE` target).
    pub rules_file: PathBuf,
    /// Path to the generated mcp-config (`--mcp-config` target).
    pub mcp_config: PathBuf,
    /// Driver wired to the generated mcp-config, ready to `.run(role, task)`.
    pub driver: ClaudeCliDriver,
    /// RAII handle: the temp dir is deleted when this field is dropped (i.e. when
    /// the run function returns — normal, error, or panic path alike).
    /// ARCH-RESOURCE-LIFECYCLE-1: every temp artifact must be RAII-cleaned.
    pub _dir: tempfile::TempDir,
}

/// Prepare one governed agent session on disk.
///
/// Creates a fresh `TempDir` (removed automatically when [`SessionSpawn`] is
/// dropped), writes `rules.json` (the role's rule-subset) and `gateway.json`
/// (an mcp-config launching `gateway_bin` with `CAMERATA_RULES_FILE` pointed
/// at `rules.json`) into it, and returns a [`ClaudeCliDriver`] bound to that
/// config.  Does NOT spawn `claude` — the caller does that via
/// [`ClaudeCliDriver::run`], so latency/output capture stay in the caller's
/// hands.
///
/// The caller-supplied `session_dir` parameter is **no longer accepted**; the
/// directory is created internally so its lifetime is tracked by
/// `SessionSpawn::_dir`.  Callers that previously constructed and passed a
/// manual temp path should simply remove that construction — `prepare_session`
/// handles it.
///
/// `worktree`, when `Some`, does TWO independent things:
///  1. it binds the returned driver's cwd + `--add-dir` to that directory, giving
///     the agent ON-DEMAND READ ACCESS to the entire repo (Read/Grep/Glob/LS plus
///     `--add-dir` directory scope), and
///  2. it sets the gateway's `CAMERATA_WORKTREE_ROOT` write-jail so `gated_write`
///     (the ONLY write path) refuses targets outside it.
///
/// `read_dirs` is the MULTI-REPO read scope: a project has MULTIPLE repos, so this is
/// the OTHER project-repo clones the agent should also be able to READ (each emitted as
/// its own `--add-dir`). For a write-class UoW agent this lets it read sibling repos
/// (e.g. a frontend UoW reading the backend's API) while still WRITING only to its
/// single `worktree`. CRITICAL: `read_dirs` widens READS ONLY — it does NOT touch the
/// `CAMERATA_WORKTREE_ROOT` write jail, so `gated_write` still refuses any target outside
/// the single `worktree`. Pass an empty slice for the single-repo / no-extra-scope case.
///
/// Reads are ungated; writes stay gated + jailed. Pass `Some(repo_dir)` + the project's
/// repo clones for any in-project agent so it can consult the real code across all repos;
/// the write gate is unaffected.
///
/// `gate_events_file`, when `Some`, points THIS session's gateway subprocess at the run's
/// own structured gate-decision JSONL sink, threaded per-spawn via the mcp-config `env`
/// (LIFECYCLE-10). It is observability only (WHERE decisions are recorded), and being
/// per-spawn it never lets concurrent runs read each other's provenance. Pass `None` when
/// no live gate-events capture is wanted (the gateway then falls back to its own default).
pub fn prepare_session(
    gateway_bin: &Path,
    role: &Role,
    worktree: Option<&Path>,
    read_dirs: &[PathBuf],
    gate_events_file: Option<&Path>,
) -> Result<SessionSpawn, SessionError> {
    // Create a RAII temp dir.  Dropped (and thus removed from disk) when the
    // returned SessionSpawn goes out of scope.
    let dir = tempfile::TempDir::new().map_err(|source| SessionError::Write {
        what: "session temp dir",
        path: std::env::temp_dir(),
        source,
    })?;
    let session_dir = dir.path();

    let rules_file = session_dir.join("rules.json");
    let rules_json = render_rules_file(role)?;
    std::fs::write(&rules_file, rules_json).map_err(|source| SessionError::Write {
        what: "rules file",
        path: rules_file.clone(),
        source,
    })?;

    let mcp_config = session_dir.join("gateway.json");
    let config_json = render_mcp_config(gateway_bin, &rules_file, worktree, gate_events_file)?;
    std::fs::write(&mcp_config, config_json).map_err(|source| SessionError::Write {
        what: "mcp-config",
        path: mcp_config.clone(),
        source,
    })?;

    // Bind the driver to the worktree when one is given: this sets the child process
    // cwd AND passes `--add-dir <worktree>`, which is what gives the agent ON-DEMAND
    // READ ACCESS to the entire repo it is working in (Read/Grep/Glob/LS resolve against
    // it, and `--add-dir` lifts the directory-scope restriction). The gateway's
    // `CAMERATA_WORKTREE_ROOT` write-jail (set in the mcp-config above) is INDEPENDENT and
    // unchanged: `gated_write` remains the only write path and is still confined to this
    // worktree. Reads are ungated; writes stay gated + jailed. Previously the driver's
    // worktree was left unbound here, so worktree runs inherited the orchestrator cwd and
    // had no `--add-dir` — the agent could not reliably read the repo it was editing.
    let mut driver = ClaudeCliDriver::new(mcp_config.display().to_string());
    if let Some(wt) = worktree {
        driver = driver.with_worktree(wt);
    }
    // MULTI-REPO READ scope: add each OTHER project-repo clone as a read-only `--add-dir`.
    // This widens READS only (a UoW agent reading sibling repos); the write jail above
    // (CAMERATA_WORKTREE_ROOT) is independent and unchanged, so these dirs are NOT writable.
    if !read_dirs.is_empty() {
        driver = driver.with_read_dirs(read_dirs.iter().cloned());
    }

    // A deterministic session id derived from the role + tempdir name; the live
    // session id reported by `claude` is captured separately in the AgentOutcome.
    let session_id = SessionId(format!(
        "{}-{}",
        role.name.to_lowercase(),
        session_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("session")
    ));

    Ok(SessionSpawn {
        session_id,
        rules_file,
        mcp_config,
        driver,
        _dir: dir,
    })
}

/// Convenience: the fully-qualified governed write tool name the prepared
/// session exposes. Re-exported so callers don't reach into the crate root.
pub const fn gated_write_tool() -> &'static str {
    GATED_WRITE_TOOL
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::RuleId;

    fn role() -> Role {
        Role {
            name: "Backend".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/".to_string()],
        }
    }

    #[test]
    fn rules_file_is_a_json_array_of_rule_ids() {
        let json = render_rules_file(&role()).unwrap();
        // Must round-trip back to Vec<RuleId> exactly (the gateway's contract).
        let back: Vec<RuleId> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, vec![RuleId("GOV-1".to_string())]);
    }

    #[test]
    fn mcp_config_uses_camerata_key_and_sets_rules_env() {
        let cfg = render_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            None,
            None,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let server = &v["mcpServers"][MCP_SERVER_KEY];
        assert_eq!(server["command"], "/bin/camerata-gateway");
        assert_eq!(server["env"][RULES_FILE_ENV], "/tmp/s/rules.json");
        // No worktree passed -> the jail env is absent.
        assert!(server["env"].get(WORKTREE_ROOT_ENV).is_none());
        // No sink passed -> the gate-events env is absent.
        assert!(server["env"].get(GATE_EVENTS_FILE_ENV).is_none());
    }

    #[test]
    fn mcp_config_sets_the_gate_events_sink_env_when_given() {
        // LIFECYCLE-10: the per-run sink path is threaded into the gateway subprocess'
        // OWN mcp-config env, per-spawn — not the parent process env.
        let cfg = render_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            Some(Path::new("/work/crate")),
            Some(Path::new("/runs/run-abc/gate-events.jsonl")),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        assert_eq!(
            v["mcpServers"][MCP_SERVER_KEY]["env"][GATE_EVENTS_FILE_ENV],
            "/runs/run-abc/gate-events.jsonl"
        );
    }

    #[test]
    fn mcp_config_sets_the_worktree_jail_env_when_given() {
        let cfg = render_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            Some(Path::new("/work/crate")),
            None,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        assert_eq!(
            v["mcpServers"][MCP_SERVER_KEY]["env"][WORKTREE_ROOT_ENV],
            "/work/crate"
        );
    }

    #[test]
    fn mcp_server_key_yields_the_governed_tool_name() {
        // The agent-visible tool is mcp__<key>__gated_write; the key MUST be
        // the prefix of GATED_WRITE_TOOL for the lock to bind.
        assert_eq!(
            GATED_WRITE_TOOL,
            format!("mcp__{MCP_SERVER_KEY}__gated_write")
        );
    }

    #[test]
    fn prepare_session_writes_both_files_and_wires_driver() {
        // prepare_session now creates its own TempDir; no caller-supplied path needed.
        let spawn =
            prepare_session(Path::new("/bin/camerata-gateway"), &role(), None, &[], None).unwrap();
        assert!(spawn.rules_file.exists());
        assert!(spawn.mcp_config.exists());
        assert_eq!(
            spawn.driver.mcp_config_path,
            spawn.mcp_config.display().to_string()
        );
        // The driver's argv must lock the agent to the governed write tool.
        let args = spawn.driver.build_args(&role(), "task");
        let allowed_idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert!(args[allowed_idx + 1].contains(GATED_WRITE_TOOL));
        // No worktree passed -> the driver has no --add-dir (orchestrator cwd; read scope
        // is whatever the caller's cwd is). The worktree-bound case is asserted below.
        assert!(!args.iter().any(|a| a == "--add-dir"));
        // spawn._dir (TempDir) cleans up on drop — no manual remove_dir_all needed.
    }

    #[test]
    fn prepare_session_binds_driver_worktree_for_read_scope() {
        // THE INVARIANT: when a worktree dir is passed, prepare_session binds the driver's
        // cwd + `--add-dir` to it so the agent has on-demand READ access to that repo. The
        // gateway write-jail env is set too, but it is independent — the driver-side
        // `--add-dir` is what grants the read window.
        let wt = std::env::temp_dir().join("cam-prepare-wt-readscope");
        let spawn = prepare_session(Path::new("/bin/camerata-gateway"), &role(), Some(&wt), &[], None)
            .unwrap();
        let args = spawn.driver.build_args(&role(), "task");
        let idx = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("--add-dir present when a worktree is bound");
        assert_eq!(args[idx + 1], wt.display().to_string());
        // The read-only built-ins ride alongside (Read/Grep/Glob/LS) — the agent can open
        // any file under the repo. The write gate is untouched.
        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        assert!(allowed.split(' ').any(|t| t == "Read"));
        assert!(allowed.split(' ').any(|t| t == "Grep"));
        assert!(allowed.split(' ').any(|t| t == GATED_WRITE_TOOL));
    }

    #[test]
    fn prepare_session_adds_sibling_repos_to_read_scope_but_not_write_jail() {
        // MULTI-REPO INVARIANT: a project has several repos. A write-class UoW agent is
        // bound to ONE worktree (cwd + write jail) but must be able to READ the OTHER
        // project repos. prepare_session emits each sibling repo as its own `--add-dir`
        // (read scope) while the gateway write jail (CAMERATA_WORKTREE_ROOT) stays the
        // single worktree — so the siblings are readable but NOT writable.
        let wt = std::env::temp_dir().join("cam-uow-frontend-worktree");
        let backend = std::env::temp_dir().join("cam-sibling-backend-repo");
        let shared = std::env::temp_dir().join("cam-sibling-shared-repo");
        let spawn = prepare_session(
            Path::new("/bin/camerata-gateway"),
            &role(),
            Some(&wt),
            &[backend.clone(), shared.clone()],
            None,
        )
        .unwrap();

        // READ scope: every dir (worktree + both siblings) appears as an `--add-dir`.
        let args = spawn.driver.build_args(&role(), "task");
        let add_dirs: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, a)| a.as_str() == "--add-dir" && *i + 1 < args.len())
            .map(|(i, _)| &args[i + 1])
            .collect();
        assert!(add_dirs.iter().any(|d| **d == wt.display().to_string()));
        assert!(add_dirs.iter().any(|d| **d == backend.display().to_string()));
        assert!(add_dirs.iter().any(|d| **d == shared.display().to_string()));

        // WRITE jail: the gateway's CAMERATA_WORKTREE_ROOT is ONLY the worktree — never a
        // sibling repo. This is the gate guarantee: extra read dirs do not widen writes.
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&spawn.mcp_config).unwrap()).unwrap();
        let jail = cfg["mcpServers"][MCP_SERVER_KEY]["env"][WORKTREE_ROOT_ENV]
            .as_str()
            .expect("write jail set");
        assert_eq!(jail, wt.display().to_string());
        assert_ne!(jail, backend.display().to_string());
        assert_ne!(jail, shared.display().to_string());

        // And the only write tool exposed is the gated one — no built-in Write/Edit/Bash.
        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        assert!(allowed.split(' ').any(|t| t == GATED_WRITE_TOOL));
        assert!(!allowed.split(' ').any(|t| t == "Write"));
        assert!(!allowed.split(' ').any(|t| t == "Edit"));
        assert!(!allowed.split(' ').any(|t| t == "Bash"));
    }

    #[test]
    fn prepare_session_writes_per_run_gate_events_sink_and_two_runs_get_distinct_sinks() {
        // LIFECYCLE-10: each run passes its OWN sink path into prepare_session, which
        // lands it in that gateway subprocess' mcp-config env — no process-global state.
        // Two concurrent runs therefore write to DISTINCT sinks, so their gate provenance
        // cannot cross-contaminate.
        let sink_a = std::env::temp_dir().join("cam-run-a").join("gate-events.jsonl");
        let sink_b = std::env::temp_dir().join("cam-run-b").join("gate-events.jsonl");

        let spawn_a =
            prepare_session(Path::new("/bin/camerata-gateway"), &role(), None, &[], Some(&sink_a))
                .unwrap();
        let spawn_b =
            prepare_session(Path::new("/bin/camerata-gateway"), &role(), None, &[], Some(&sink_b))
                .unwrap();

        let read_sink = |spawn: &SessionSpawn| -> String {
            let cfg: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&spawn.mcp_config).unwrap()).unwrap();
            cfg["mcpServers"][MCP_SERVER_KEY]["env"][GATE_EVENTS_FILE_ENV]
                .as_str()
                .expect("gate-events sink env set")
                .to_string()
        };

        let a = read_sink(&spawn_a);
        let b = read_sink(&spawn_b);
        assert_eq!(a, sink_a.display().to_string());
        assert_eq!(b, sink_b.display().to_string());
        assert_ne!(a, b, "two runs must get DISTINCT sinks (no cross-contamination)");
    }
}
