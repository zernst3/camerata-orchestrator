//! The store-only model provider of a rerun (#588): a saved run's deterministic stages are run
//! again on a newer build with ZERO model calls.
//!
//! In a rerun no real model client exists. The seam every model stage calls through
//! ([`StoreOnlyLlm`], handed to [`super::journal::wrap`] where a real client would go) can only
//! be answered by the run's store: a reply the journal holds is returned, and anything else is a
//! miss. A miss is recorded in the run's [`StoreOnlyMisses`] and fails the call; the rerun reads
//! the list when the stages are done and refuses as a whole, naming every missing reply
//! ([`StoreOnlyMisses::refusal`]). Nothing is ever sent.
//!
//! The agent finder builds its own agent and never goes through that seam, so it has a twin of
//! its own ([`StoreOnlyAgent`]) that `find_repo_for_run_with` puts in place of any agent, a
//! scripted one included, whenever the run is a rerun
//! ([`crate::ai_audit::agent_finder::run::with_optional_store_only`]).

use std::sync::{Arc, Mutex};

use crate::ai_audit::agent_finder::{
    AgentRun, ConversationHandle, FollowUp, PieceAgent, PieceLimits, Room,
};
use crate::ai_audit::{BatchApi, BatchSeam};
use crate::llm::{
    BatchItem, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest, LlmResponse,
};

/// The model replies a rerun needed and the run's store did not hold, in the order they were
/// asked, each once. Shared by every seam of the run.
#[derive(Clone, Default)]
pub struct StoreOnlyMisses(Arc<Mutex<Vec<String>>>);

impl std::fmt::Debug for StoreOnlyMisses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StoreOnlyMisses")
            .field(&self.list())
            .finish()
    }
}

/// Two lists are the same list when they are the same run's list.
impl PartialEq for StoreOnlyMisses {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl StoreOnlyMisses {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note a reply the run needed and did not hold.
    pub fn record(&self, what: impl Into<String>) {
        let what = what.into();
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !list.contains(&what) {
            list.push(what);
        }
    }

    /// Every missing reply so far.
    pub fn list(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    /// The refusal of the whole rerun, naming every missing reply; `None` when none is missing.
    pub fn refusal(&self) -> Option<String> {
        let list = self.list();
        if list.is_empty() {
            return None;
        }
        let mut out = format!(
            "This rerun was refused: {} model reply(ies) it needs are not in the saved run, and \
             a rerun never asks a model. Nothing was sent and no export was written. Missing:\n",
            list.len()
        );
        for item in &list {
            out.push_str(&format!("  - {item}\n"));
        }
        out.push_str(
            "Resume the run on the build that started it to fetch what is missing, then rerun \
             the deterministic checks.",
        );
        Some(out)
    }
}

/// The model seam of a rerun: no client behind it. See the module doc.
pub struct StoreOnlyLlm {
    misses: StoreOnlyMisses,
    /// Whether the run started on the API backend (so the batch path stays the one it took).
    batchable: bool,
}

impl StoreOnlyLlm {
    pub fn new(misses: StoreOnlyMisses, batchable: bool) -> Self {
        Self { misses, batchable }
    }

    fn miss(&self, what: String) -> anyhow::Error {
        self.misses.record(what);
        super::journal::missing_reply_error()
    }
}

/// The seam a rerun hands to [`super::journal::wrap`] in place of a real client.
pub fn seam(misses: StoreOnlyMisses, batchable: bool) -> BatchSeam {
    BatchSeam::new(Arc::new(StoreOnlyLlm::new(misses, batchable)))
}

#[async_trait::async_trait]
impl LlmPort for StoreOnlyLlm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let (kind, repo, files) = super::journal::classify(&req);
        Err(self.miss(super::journal::describe_missing(kind, &repo, &files)))
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl BatchApi for StoreOnlyLlm {
    fn has_api_key(&self) -> bool {
        self.batchable
    }

    fn realtime(&self) -> &dyn LlmPort {
        self
    }

    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }

    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        Err(self.miss(format!(
            "a batch of {} model request(s) that the run never stored",
            items.len()
        )))
    }

    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        Err(self.miss(format!(
            "the status of batch {batch_id}, whose results are not stored"
        )))
    }

    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        Err(self.miss(format!(
            "the results of batch {batch_id}, which are not stored"
        )))
    }

    /// Cancelling asks for no model work and costs nothing; there is nothing to cancel.
    async fn cancel_batch(&self, _batch_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The agent finder's twin of [`StoreOnlyLlm`]: every turn is a miss.
pub struct StoreOnlyAgent {
    misses: StoreOnlyMisses,
}

impl StoreOnlyAgent {
    pub fn new(misses: StoreOnlyMisses) -> Self {
        Self { misses }
    }

    fn miss(&self, room: &Room) -> anyhow::Error {
        let files = room.piece_line_counts();
        let first = files.first().map_or("", |(p, _)| p.as_str());
        self.misses.record(format!(
            "agent finder review of {} file(s) starting at {first}",
            files.len()
        ));
        super::journal::missing_reply_error()
    }
}

#[async_trait::async_trait]
impl PieceAgent for StoreOnlyAgent {
    async fn run(
        &self,
        room: &Room,
        _brief: &str,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Err(self.miss(room))
    }

    async fn follow_up(
        &self,
        room: &Room,
        _prior: &AgentRun,
        _turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Err(self.miss(room))
    }

    fn release(&self, _conversation: &ConversationHandle) {}
}
