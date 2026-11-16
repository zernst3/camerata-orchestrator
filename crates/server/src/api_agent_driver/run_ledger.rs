//! The run ledger of the in-process agent tool loop: what every agent of one run READ, and
//! what it SPENT.
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
//! # The spend meter
//!
//! Every provider turn of every agent is priced from the model registry (input, output, cache
//! reads and writes) and recorded as a [`TurnCost`]. With a [`SpendLimit`] set, a turn is
//! admitted only while the run can still afford it ([`AgentRunLedger::admit_turn`]); a refused
//! turn is never sent and the agent stops with [`SpendLimitReached`]. A turn that cannot be
//! priced ends a limited run too: an unknown cost cannot be held to a limit.
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

/// A positive, finite spend limit in USD for one run's agents. Built only through
/// [`SpendLimit::usd`] or [`SpendLimit::from_cents`], so a zero, negative or NaN limit (which
/// would either stop everything or nothing) cannot exist.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpendLimit(f64);

impl SpendLimit {
    /// `None` unless `usd` is finite and above zero.
    pub fn usd(usd: f64) -> Option<Self> {
        (usd.is_finite() && usd > 0.0).then_some(Self(usd))
    }

    /// `None` for zero cents.
    pub fn from_cents(cents: u64) -> Option<Self> {
        Self::usd(cents as f64 / 100.0)
    }

    pub fn as_usd(self) -> f64 {
        self.0
    }
}

/// One provider turn of one agent, as the run ledger records it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnCost {
    /// The role name of the agent whose turn it was.
    pub agent: String,
    /// The turn's number within that agent's run (1-based).
    pub turn: usize,
    /// The model the turn requested.
    pub model: String,
    /// Every input token billed (fresh input plus cache reads and writes), when reported.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// The turn's dollar cost from the model registry; `None` when it could not be priced.
    pub cost_usd: Option<f64>,
}

/// Why the run's spend limit stopped an agent before its next provider turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendLimitReached {
    pub limit_usd: f64,
    pub spent_usd: f64,
    /// Provider turns already recorded for the run (every agent).
    pub turns: usize,
    pub cause: SpendStopCause,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpendStopCause {
    /// The spend has reached the limit.
    Reached,
    /// One more turn, priced at the costliest turn so far, would pass the limit.
    NextTurnWouldExceed { reserve_usd: f64 },
    /// A turn of `model` could not be priced, so the limit can no longer be enforced.
    Unpriced { model: String },
}

impl std::fmt::Display for SpendLimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let head = format!(
            "spend limit reached: this run's agents have spent ${:.4} of the ${:.2} spend \
             limit over {} model turn(s)",
            self.spent_usd, self.limit_usd, self.turns
        );
        match &self.cause {
            SpendStopCause::Reached => write!(f, "{head}; no further model call was sent."),
            SpendStopCause::NextTurnWouldExceed { reserve_usd } => write!(
                f,
                "{head}; the next turn could cost up to ${reserve_usd:.4} (the costliest turn \
                 so far), which would pass the limit, so it was not sent."
            ),
            SpendStopCause::Unpriced { model } => write!(
                f,
                "{head}; a turn on model `{model}` could not be priced from the model \
                 registry, so the limit cannot be enforced and no further model call was sent."
            ),
        }
    }
}

impl std::error::Error for SpendLimitReached {}

#[derive(Default)]
struct LedgerState {
    reads: Vec<ReadLogEntry>,
    turns: Vec<TurnCost>,
    /// Turns admitted and not yet recorded (concurrent fan-out children share one ledger).
    in_flight: usize,
    /// The model of the first turn that could not be priced, if any.
    unpriced_model: Option<String>,
}

/// What one run's agents read and spent. See the module docs.
#[derive(Default)]
pub struct AgentRunLedger {
    state: Mutex<LedgerState>,
    governance: Option<(Arc<camerata_persistence::GovernanceLog>, String)>,
    spend_limit: Option<SpendLimit>,
}

/// Admission for one provider turn. Recording it ([`TurnPermit::record`]) adds the turn's cost
/// to the run; dropping it unrecorded (the call failed before anything was billed) releases
/// the reservation.
pub(crate) struct TurnPermit<'a> {
    ledger: &'a AgentRunLedger,
}

impl TurnPermit<'_> {
    /// Record the admitted turn: its cost joins the run's spend (an unpriced turn marks the run
    /// as no longer enforceable), and, with a governance log wired, an `agent_turn` event.
    pub(crate) async fn record(self, turn: TurnCost) {
        let ledger = self.ledger;
        // Drop releases the in-flight reservation; the turn is pushed in the same critical
        // section so no admission sees the reservation gone and the cost not yet counted.
        {
            let mut st = ledger.lock();
            st.in_flight = st.in_flight.saturating_sub(1);
            if turn.cost_usd.is_none() && st.unpriced_model.is_none() {
                st.unpriced_model = Some(turn.model.clone());
            }
            st.turns.push(turn.clone());
        }
        std::mem::forget(self);
        ledger.record_turn_event(&turn).await;
    }
}

impl Drop for TurnPermit<'_> {
    /// Released without a record: the call failed before anything was billed.
    fn drop(&mut self) {
        let mut st = self.ledger.lock();
        st.in_flight = st.in_flight.saturating_sub(1);
    }
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

    /// Enforce `limit` on every provider turn of the run's agents. Builder form.
    pub fn with_spend_limit(mut self, limit: SpendLimit) -> Self {
        self.spend_limit = Some(limit);
        self
    }

    pub fn spend_limit(&self) -> Option<SpendLimit> {
        self.spend_limit
    }

    /// Dollars recorded so far (unpriced turns count as zero).
    pub fn spent_usd(&self) -> f64 {
        self.lock().turns.iter().filter_map(|t| t.cost_usd).sum()
    }

    /// Every provider turn so far, in the order they were recorded.
    pub fn turns(&self) -> Vec<TurnCost> {
        self.lock().turns.clone()
    }

    /// Admit one more provider turn, or say why the spend limit refuses it.
    ///
    /// With no limit every turn is admitted. With one, a turn is refused when the run has
    /// already spent the limit, when a turn could not be priced (the limit can no longer be
    /// enforced), or when one more turn priced at the costliest turn so far, on top of every
    /// turn already admitted and not yet recorded, would pass it.
    ///
    /// The overshoot bound: a turn is priced only after it returns, so a run can pass its limit
    /// by at most the amount one turn costs MORE than the costliest turn before it, once per
    /// agent running at the same time (fan-out children). The single-agent dev run stops at or
    /// below its limit whenever its turns cost about the same.
    pub(crate) fn admit_turn(&self) -> Result<TurnPermit<'_>, SpendLimitReached> {
        let mut st = self.lock();
        if let Some(limit) = self.spend_limit {
            let limit_usd = limit.as_usd();
            let spent_usd: f64 = st.turns.iter().filter_map(|t| t.cost_usd).sum();
            let reserve_usd = st
                .turns
                .iter()
                .filter_map(|t| t.cost_usd)
                .fold(0.0_f64, f64::max);
            let refuse = |cause| SpendLimitReached {
                limit_usd,
                spent_usd,
                turns: st.turns.len(),
                cause,
            };
            if let Some(model) = st.unpriced_model.clone() {
                return Err(refuse(SpendStopCause::Unpriced { model }));
            }
            if spent_usd >= limit_usd {
                return Err(refuse(SpendStopCause::Reached));
            }
            if spent_usd + (st.in_flight as f64 + 1.0) * reserve_usd > limit_usd {
                return Err(refuse(SpendStopCause::NextTurnWouldExceed { reserve_usd }));
            }
        }
        st.in_flight += 1;
        Ok(TurnPermit { ledger: self })
    }

    async fn record_turn_event(&self, turn: &TurnCost) {
        let Some((log, run_id)) = self.governance.as_ref() else {
            return;
        };
        let event =
            camerata_persistence::GovernanceEvent::info(run_id.clone(), "agent_turn", "agent")
                .with_reason(match turn.cost_usd {
                    Some(c) => format!(
                        "{} turn {} on `{}` cost ${c:.6}",
                        turn.agent, turn.turn, turn.model
                    ),
                    None => format!(
                        "{} turn {} on `{}` could not be priced",
                        turn.agent, turn.turn, turn.model
                    ),
                })
                .with_detail(serde_json::to_string(turn).unwrap_or_default());
        if let Err(e) = log.record(event).await {
            tracing::warn!(error = %e, run_id = %run_id, "failed to record agent_turn governance event");
        }
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
