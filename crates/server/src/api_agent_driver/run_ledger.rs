//! The run ledger of the in-process agent tool loop: what every agent of one run READ.
//!
//! One [`AgentRunLedger`] is made per run and shared (an `Arc`) by every [`super::ApiAgentDriver`]
//! the run builds: the top-level agent, an orchestrator lead, and every child the lead delegates
//! to or fans out to (the server's driver factories carry it into each child they build). So the
//! run's ledger holds the reads of the whole agent tree, not just the first agent's.
//!
//! # The read log
//!
//! Every read-tool call (`Read`, `Glob`, `Grep`, `LS`) appends one [`ReadLogEntry`]: which agent
//! (its role name), which tool, the path relative to the read jail, the bytes of output handed to
//! the model, and whether the jail allowed or refused it. It answers "what of this repository did
//! the model see?" for a client engagement, after the fact, from the run alone.
//!
//! # Where it is kept
//!
//! In memory for the life of the run (callers read it back with [`AgentRunLedger::read_log`]),
//! and, when the run wired a governance log ([`AgentRunLedger::with_governance`]), as one durable
//! `agent_read` governance event per entry under the run's id. Recording is fail-soft like every
//! governance write: a failed write is logged and never stops the agent.

use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::ReadVerdict;

/// One read-tool call, as the run ledger records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadLogEntry {
    /// The role name of the agent that made the call (`delegate-child` for a delegated child).
    pub agent: String,
    /// `Read`, `Glob`, `Grep` or `LS`.
    pub tool: String,
    /// The path asked for: relative to the worktree root (or `read-folder:<name>/...`) when it
    /// resolved inside the jail, else as the model wrote it.
    pub path: String,
    /// Bytes of tool output returned to the model (zero unless allowed).
    pub bytes: usize,
    pub verdict: ReadVerdict,
}

#[derive(Default)]
struct LedgerState {
    reads: Vec<ReadLogEntry>,
}

/// What one run's agents read. See the module docs.
#[derive(Default)]
pub struct AgentRunLedger {
    state: Mutex<LedgerState>,
    governance: Option<(Arc<camerata_persistence::GovernanceLog>, String)>,
}

impl AgentRunLedger {
    /// An empty ledger with no durable sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also record every entry as a governance event under `run_id`. Builder form.
    pub fn with_governance(
        mut self,
        log: Arc<camerata_persistence::GovernanceLog>,
        run_id: impl Into<String>,
    ) -> Self {
        self.governance = Some((log, run_id.into()));
        self
    }

    /// Every read so far, in the order the calls were made.
    pub fn read_log(&self) -> Vec<ReadLogEntry> {
        self.lock().reads.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        // A panic while the lock was held cannot leave the log half-written (each mutation is
        // one push), so a poisoned lock is still safe to read and extend.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append one read and, when a governance log is wired, record it durably.
    pub(crate) async fn record_read(&self, entry: ReadLogEntry) {
        self.lock().reads.push(entry.clone());
        let Some((log, run_id)) = self.governance.as_ref() else {
            return;
        };
        let event = if entry.verdict == ReadVerdict::Refused {
            camerata_persistence::GovernanceEvent::warn(run_id.clone(), "agent_read", "agent")
        } else {
            camerata_persistence::GovernanceEvent::info(run_id.clone(), "agent_read", "agent")
        }
        .with_reason(format!(
            "{} {} `{}` ({} bytes) by {}",
            entry.tool,
            match entry.verdict {
                ReadVerdict::Allowed => "allowed",
                ReadVerdict::Refused => "REFUSED",
                ReadVerdict::Error => "failed",
            },
            entry.path,
            entry.bytes,
            entry.agent
        ))
        .with_detail(serde_json::to_string(&entry).unwrap_or_default());
        if let Err(e) = log.record(event).await {
            tracing::warn!(error = %e, run_id = %run_id, "failed to record agent_read governance event");
        }
    }
}
