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
) -> Result<String, SessionError> {
    let mut env = std::collections::BTreeMap::new();
    env.insert(
        RULES_FILE_ENV.to_string(),
        rules_file.display().to_string(),
    );

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
}

/// Prepare one governed agent session on disk under `session_dir`.
///
/// Writes `<session_dir>/rules.json` (the role's rule-subset) and
/// `<session_dir>/gateway.json` (an mcp-config launching `gateway_bin` with
/// `CAMERATA_RULES_FILE` pointed at `rules.json`), and returns a
/// [`ClaudeCliDriver`] bound to that config. Does NOT spawn `claude` — the
/// caller does that via [`ClaudeCliDriver::run`], so latency/output capture
/// stay in the caller's hands.
pub fn prepare_session(
    session_dir: &Path,
    gateway_bin: &Path,
    role: &Role,
) -> Result<SessionSpawn, SessionError> {
    std::fs::create_dir_all(session_dir).map_err(|source| SessionError::Write {
        what: "session dir",
        path: session_dir.to_path_buf(),
        source,
    })?;

    let rules_file = session_dir.join("rules.json");
    let rules_json = render_rules_file(role)?;
    std::fs::write(&rules_file, rules_json).map_err(|source| SessionError::Write {
        what: "rules file",
        path: rules_file.clone(),
        source,
    })?;

    let mcp_config = session_dir.join("gateway.json");
    let config_json = render_mcp_config(gateway_bin, &rules_file)?;
    std::fs::write(&mcp_config, config_json).map_err(|source| SessionError::Write {
        what: "mcp-config",
        path: mcp_config.clone(),
        source,
    })?;

    let driver = ClaudeCliDriver::new(mcp_config.display().to_string());

    // A deterministic session id derived from the role + dir; the live session
    // id reported by `claude` is captured separately in the AgentOutcome.
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
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let server = &v["mcpServers"][MCP_SERVER_KEY];
        assert_eq!(server["command"], "/bin/camerata-gateway");
        assert_eq!(server["env"][RULES_FILE_ENV], "/tmp/s/rules.json");
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
        let dir = std::env::temp_dir().join(format!(
            "camerata-session-test-{}",
            std::process::id()
        ));
        let spawn =
            prepare_session(&dir, Path::new("/bin/camerata-gateway"), &role()).unwrap();
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

        let _ = std::fs::remove_dir_all(&dir);
    }
}
