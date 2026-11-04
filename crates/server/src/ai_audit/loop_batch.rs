//! A Message Batches test double that answers each submitted item with a real-time stub.
//!
//! The post-scan stages (detail rewrites, fix generation, alternatives) each have a real-time
//! stub of their own that answers by what the prompt asks. This double puts such a stub behind
//! the batch API: `submit` keeps the items, `fetch` asks the stub about each one as a real-time
//! request rebuilt from the batch item, and hands back its reply as the item's result. So a
//! stage run in batch mode and the same stage run in real time see the same replies, and a test
//! can say what was absorbed, and how.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::batch_mode::{BatchApi, BatchSeam};
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
};

/// See the module comment.
pub(crate) struct LoopBatch {
    stub: Arc<dyn LlmPort>,
    /// `(custom id, request)` per accepted batch; the batch id is `msgbatch_<index>`.
    batches: Mutex<Vec<Vec<(String, LlmRequest)>>>,
    submits: AtomicUsize,
    /// When set, the batch fetched next never gives its results back (see `doomed`): it was
    /// accepted, so it may still be running and billed, but every try to fetch it fails.
    fail_next_fetch: AtomicBool,
    /// The batch whose every fetch attempt fails as a dropped connection.
    doomed: Mutex<Option<String>>,
}

impl LoopBatch {
    pub(crate) fn new(stub: Arc<dyn LlmPort>) -> Arc<Self> {
        Arc::new(Self {
            stub,
            batches: Mutex::new(Vec::new()),
            submits: AtomicUsize::new(0),
            fail_next_fetch: AtomicBool::new(false),
            doomed: Mutex::new(None),
        })
    }

    /// The model seam a stage is handed: real-time calls go to the stub, batches to this double.
    pub(crate) fn seam(self: &Arc<Self>) -> BatchSeam {
        BatchSeam::new(self.clone() as Arc<dyn BatchApi>)
    }

    /// Make every fetch attempt of the next batch fetched fail in transport (the runner tries a
    /// fetch more than once, so one failed attempt would only be retried).
    pub(crate) fn fail_next_fetch(&self) {
        self.fail_next_fetch.store(true, Ordering::SeqCst);
    }

    /// How many batches were submitted.
    pub(crate) fn submit_calls(&self) -> usize {
        self.submits.load(Ordering::SeqCst)
    }

    /// Every submitted item's prompt, over every batch, in submission order.
    pub(crate) fn submitted_prompts(&self) -> Vec<String> {
        self.batches
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .map(|(_, req)| req.prompt.clone())
            .collect()
    }
}

/// The text of a batch item's user content, however it was split into cached blocks.
fn item_prompt(item: &BatchItem) -> String {
    let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
    match content.as_str() {
        Some(t) => t.to_string(),
        None => content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
    }
}

#[async_trait::async_trait]
impl BatchApi for LoopBatch {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        self.stub.as_ref()
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let mut batches = self.batches.lock().unwrap();
        let id = format!("msgbatch_{}", batches.len());
        batches.push(
            items
                .iter()
                .map(|i| {
                    let mut req = LlmRequest::new(item_prompt(i));
                    if let Some(system) = &i.params.system {
                        req = req.with_system(system.clone());
                    }
                    (i.custom_id.clone(), req)
                })
                .collect(),
        );
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn cancel_batch(&self, _batch_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        if self.fail_next_fetch.swap(false, Ordering::SeqCst) {
            *self.doomed.lock().unwrap() = Some(batch_id.to_string());
        }
        if self.doomed.lock().unwrap().as_deref() == Some(batch_id) {
            anyhow::bail!("error sending request: connection reset by peer");
        }
        let n: usize = batch_id.trim_start_matches("msgbatch_").parse().unwrap();
        let members = self.batches.lock().unwrap()[n].clone();
        let mut rows = Vec::new();
        for (custom_id, req) in members {
            let row = match self.stub.complete(req).await {
                Ok(response) => BatchResultRow {
                    custom_id,
                    response: Some(response),
                    error: None,
                    billed: None,
                },
                Err(e) => BatchResultRow {
                    custom_id,
                    response: None,
                    error: Some(format!("{e:#}")),
                    billed: None,
                },
            };
            rows.push(row);
        }
        Ok(rows)
    }
}
