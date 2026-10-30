//! The model seam of a saved run: every call is stored when it returns, and on a resume a call
//! whose reply is stored and good is answered from the store.
//!
//! A reply is "good" when the model finished (it was not cut off at the output limit) and the
//! text holds a JSON object that parses. That is what every stage asks for, so a malformed or cut
//! off reply is never stored as good and is asked again on a resume. A reply that parses but that
//! a later step then rejects (no usable fix in it, no estimate in it) is set aside at the end of
//! the attempt ([`quarantine_rejected`]) for the same reason.
//!
//! A request is named by a digest of everything the model sees (model, system prompt, prompt,
//! output limit, cache breakpoints, effort, temperature). The same request asked twice in one
//! attempt (a retry with an unchanged prompt) is stored as the first and the second occurrence.
//! On a resume the n-th ask of a request is answered by its n-th GOOD stored reply, in the order
//! the replies were stored: a first occurrence that failed and a second that succeeded (the
//! end-of-run second try) leave one good reply, and it answers the resume's one ask, so nothing
//! that was paid for is paid for again.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    hex, now, read_json, write_atomic, FinishFailure, PartKind, PartStatus, ResumeEstimate,
    RunRecord, RunStore,
};
use crate::ai_audit::BatchApi;
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};

// ── The prompt seed ──────────────────────────────────────────────────────────────────────────

/// What the journal does when it is asked to journal a call and no prompt seed is in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedPolicy {
    /// The call fails with the reason (tests: a journaled call without a seed is a bug).
    Refuse,
    /// The call goes ahead, with a loud warning on stderr and a note on the run.
    WarnAndRecord,
}

/// The policy this build uses: tests refuse, production warns and records.
const SEED_POLICY: SeedPolicy = if cfg!(test) {
    SeedPolicy::Refuse
} else {
    SeedPolicy::WarnAndRecord
};

/// The note a run carries when a model call was journaled with no prompt seed in scope.
pub(crate) const UNSEEDED_CALL_NOTE: &str = "a model call was made with no prompt seed in \
     scope, so its prompt carries a random delimiter token; its stored reply cannot match the \
     same call on a resume and that call is paid for again";

/// Check that the journal may journal a call: its prompt seed must be in scope (`in_scope`).
pub(crate) fn check_seed(
    store: &RunStore,
    in_scope: bool,
    policy: SeedPolicy,
) -> anyhow::Result<()> {
    if in_scope {
        return Ok(());
    }
    match policy {
        SeedPolicy::Refuse => anyhow::bail!(
            "a model call was journaled with no prompt seed in scope (wrap the run in \
             prompt_frame::with_seed with the run's seed): {UNSEEDED_CALL_NOTE}"
        ),
        SeedPolicy::WarnAndRecord => {
            if store.add_note(UNSEEDED_CALL_NOTE) {
                eprintln!("[camerata-server] WARNING: {UNSEEDED_CALL_NOTE}");
            }
            Ok(())
        }
    }
}

/// [`check_seed`] for a call made now.
fn require_seed(store: &RunStore) -> anyhow::Result<()> {
    check_seed(store, crate::prompt_frame::seed_in_scope(), SEED_POLICY)
}

// ── What is stored ───────────────────────────────────────────────────────────────────────────

/// A model reply as stored. [`LlmResponse`] is serialize-only, so this mirrors it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    pub text: String,
    pub model: String,
    pub backend: String,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub or_cache_discount: Option<f64>,
    #[serde(default)]
    pub truncated: bool,
}

impl StoredResponse {
    fn of(r: &LlmResponse) -> Self {
        Self {
            text: r.text.clone(),
            model: r.model.clone(),
            backend: r.backend.clone(),
            cost_usd: r.cost_usd,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cache_read_input_tokens: r.cache_read_input_tokens,
            cache_creation_input_tokens: r.cache_creation_input_tokens,
            or_cache_discount: r.or_cache_discount,
            truncated: r.truncated,
        }
    }

    /// The reply as a replay returns it: the same text, and no cost, because this attempt did not
    /// pay for it. The usage meter and the spend limit therefore count only what this attempt
    /// sends.
    fn replayed(&self) -> LlmResponse {
        LlmResponse {
            text: self.text.clone(),
            model: self.model.clone(),
            backend: self.backend.clone(),
            cost_usd: Some(0.0),
            input_tokens: Some(0),
            output_tokens: Some(0),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: self.or_cache_discount,
            truncated: self.truncated,
        }
    }
}

/// What became of one occurrence of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// A good reply: replayed on a resume.
    Done,
    /// The call failed (transport, auth, spend limit ...): asked again on a resume.
    Failed,
    /// A reply came back but was cut off or held no JSON: asked again on a resume.
    Unusable,
    /// A good reply every entry of which a later step rejected: asked again on a resume. The
    /// reason is on the entry ([`CallEntry::reason`]). A reply only PART of which was rejected
    /// stays [`EntryStatus::Done`] with the reason: what it got right is replayed.
    Rejected,
}

/// One occurrence of a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEntry {
    pub attempt: u32,
    pub status: EntryStatus,
    pub at: String,
    #[serde(default)]
    pub response: Option<StoredResponse>,
    /// The scrubbed error text of a failed call.
    #[serde(default)]
    pub error: Option<String>,
    /// The failure class of a failed call.
    #[serde(default)]
    pub class: Option<String>,
    /// What the call cost when it was made.
    #[serde(default)]
    pub cost_usd: f64,
    /// Why this reply, or part of it, was turned down or unusable, set at the point of
    /// rejection: a reply cut off at the output limit, one holding no JSON, a fix that named
    /// something not in the evidence, a detail that dropped a name. Only the reply that was
    /// turned down carries one; a reply that stays `Done` with a reason had some entries
    /// accepted and some turned down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Which path produced this reply: `"schema"` when the request's JSON Schema went to the
    /// API as `output_config.format` and the reply was produced under it, `"tolerant"` when the
    /// request carried one but the backend or model could not take it (the CLI backend, a model
    /// without structured output) so the reply is free text read by the tolerant parser. Absent
    /// for a request with no schema, and for a call that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
}

/// Everything stored for one request digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallFile {
    pub key: String,
    pub kind: PartKind,
    /// The repo the request was about, when its prompt names one.
    #[serde(default)]
    pub repo: String,
    /// The file paths framed in the prompt, for matching a failed pass to the calls over its files.
    #[serde(default)]
    pub files: Vec<String>,
    /// By occurrence (0 is the first time the request was asked).
    pub entries: BTreeMap<usize, CallEntry>,
}

fn call_path(dir: &Path, key: &str) -> std::path::PathBuf {
    dir.join("calls").join(format!("{key}.json"))
}

// ── Naming a request ─────────────────────────────────────────────────────────────────────────

/// The digest that names `req`.
pub fn request_key(req: &LlmRequest) -> String {
    let mut h = Sha256::new();
    let mut part = |label: &str, bytes: &[u8]| {
        h.update(label.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part("model", req.model.as_bytes());
    part("system", req.system.as_deref().unwrap_or("").as_bytes());
    part("prompt", req.prompt.as_bytes());
    part("max_tokens", &req.max_tokens.to_le_bytes());
    for b in &req.cache_breakpoints {
        part("breakpoint", &(*b as u64).to_le_bytes());
    }
    part("effort", req.effort.as_deref().unwrap_or("").as_bytes());
    part(
        "temperature",
        &req.temperature.map_or(u32::MAX, f32::to_bits).to_le_bytes(),
    );
    if let Some(dir) = &req.repo_read_dir {
        part("read", dir.to_string_lossy().as_bytes());
    }
    // The schema a reply must satisfy is part of the question (a different schema is a
    // different request, and a stored reply to the other would not be this one's). A request
    // with none hashes exactly as before.
    if let Some(schema) = &req.response_schema {
        part("schema", schema.to_string().as_bytes());
    }
    hex(&h.finalize())
}

/// The prefix of each stage's system prompt that tells the stages apart.
fn stage_prefixes() -> &'static Vec<(PartKind, String)> {
    static PREFIXES: std::sync::OnceLock<Vec<(PartKind, String)>> = std::sync::OnceLock::new();
    PREFIXES.get_or_init(|| {
        let head = |s: String| s.chars().take(160).collect::<String>();
        vec![
            (
                PartKind::Scan,
                head(crate::ai_audit::audit_system_prompt_with(false)),
            ),
            (
                PartKind::Scan,
                head(crate::ai_audit::audit_system_prompt_with(true)),
            ),
            (
                PartKind::Calibration,
                head(crate::ai_audit::verify_system_prompt()),
            ),
            (
                PartKind::Alternatives,
                head(crate::ai_audit::alternatives_system_prompt()),
            ),
            (
                PartKind::Fix,
                head(crate::ai_audit::fix_specific_system_prompt()),
            ),
            (
                PartKind::Detail,
                head(crate::ai_audit::detail_regen::detail_regen_system_prompt()),
            ),
        ]
    })
}

/// What stage a request belongs to, the repo its prompt names (if it does), and the file paths it
/// frames.
pub fn classify(req: &LlmRequest) -> (PartKind, String, Vec<String>) {
    let system = req.system.as_deref().unwrap_or("");
    let kind = stage_prefixes()
        .iter()
        .find(|(_, prefix)| system.starts_with(prefix.as_str()))
        .map_or(PartKind::Other, |(kind, _)| *kind);
    let repo = req
        .prompt
        .lines()
        .find_map(|l| l.strip_prefix("Repository: "))
        .map(|r| r.trim().to_string())
        .unwrap_or_default();
    let mut files: Vec<String> = Vec::new();
    for line in req.prompt.lines() {
        // `<<<UNTRUSTED <token> "<label>">>>`
        if let Some(rest) = line.strip_prefix("<<<UNTRUSTED ") {
            if let Some((_, label)) = rest.split_once(' ') {
                let label = label.trim_end_matches(">>>");
                if let Ok(serde_json::Value::String(path)) = serde_json::from_str(label) {
                    if path != "repo map" && !path.starts_with("finding ") {
                        files.push(path);
                    }
                }
            }
        }
    }
    files.sort();
    files.dedup();
    (kind, repo, files)
}

/// Whether a stored entry of a call over a `kind` stage is answered from the store on a resume.
///
/// A good reply always is. The fix and detail stages are the two whose own rounds change the
/// request after a reply they turned down (the next round carries the feedback line, or a smaller
/// batch), so for them a reply every entry of which was turned down ([`EntryStatus::Rejected`]),
/// and one that was cut off or held no JSON ([`EntryStatus::Unusable`]), is replayed too: the stage
/// judges it again, turns it down for the same reason and sends the next round's changed request,
/// which the live run stored under its own key. Asking the rejected request again, identically,
/// would pay a second time for an answer already judged wrong. Every other stage (scan,
/// calibration, alternatives) keeps its policy: a rejected or unusable reply is asked again, for
/// those stages set a whole stage aside ([`quarantine_rejected`]) and have no rounds of their own
/// that change the request after a reply they turned down. A failed call has no reply to replay.
pub(crate) fn replays_on_resume(status: EntryStatus, kind: PartKind) -> bool {
    match status {
        EntryStatus::Done => true,
        EntryStatus::Rejected | EntryStatus::Unusable => {
            matches!(kind, PartKind::Fix | PartKind::Detail)
        }
        EntryStatus::Failed => false,
    }
}

/// True when a reply is one a resume may return again: finished, with a JSON object that parses.
fn reply_is_good(resp: &LlmResponse) -> bool {
    !resp.truncated
        && crate::ai_audit::extract_json_object(&resp.text)
            .is_some_and(|json| serde_json::from_str::<serde_json::Value>(json).is_ok())
}

// ── The call seam ────────────────────────────────────────────────────────────────────────────

/// What `plan_call` decided for one request.
enum Planned {
    Replay(Box<LlmResponse>),
    Fresh(FreshCall),
}

struct FreshCall {
    /// Whether the request carried a response schema (whether it was sent is decided when the
    /// reply names its backend and model).
    asked_schema: bool,
    key: String,
    idx: usize,
    kind: PartKind,
    repo: String,
    files: Vec<String>,
}

fn load_call_file(dir: &Path, key: &str) -> Option<CallFile> {
    read_json(&call_path(dir, key))
}

fn plan_call(store: &RunStore, req: &LlmRequest) -> Planned {
    let key = request_key(req);
    let (kind, repo, files) = classify(req);
    let planned = {
        let mut st = store.lock_state();
        let idx = {
            let n = st.asked.entry(key.clone()).or_insert(0);
            let idx = *n;
            *n += 1;
            idx
        };
        let dir = store.dir().to_path_buf();
        let st = &mut *st;
        let file = st.calls.entry(key.clone()).or_insert_with(|| {
            load_call_file(&dir, &key).unwrap_or_else(|| CallFile {
                key: key.clone(),
                kind,
                repo: repo.clone(),
                files: files.clone(),
                entries: BTreeMap::new(),
            })
        });
        // The idx-th ask is answered by the idx-th good reply, in the order they were stored.
        let file_kind = file.kind;
        let good = file
            .entries
            .values()
            .filter(|e| replays_on_resume(e.status, file_kind))
            .filter_map(|e| e.response.as_ref())
            .nth(idx);
        let good_slot = file
            .entries
            .iter()
            .filter(|(_, e)| replays_on_resume(e.status, file_kind) && e.response.is_some())
            .map(|(slot, _)| *slot)
            .nth(idx);
        match good {
            Some(stored) => {
                let replay = stored.replayed();
                if let Some(slot) = good_slot {
                    st.last_served.insert(key.clone(), slot);
                }
                Planned::Replay(Box::new(replay))
            }
            None => {
                // A fresh call is stored in the next free slot of the file, never over an
                // existing entry (a good one at a later slot must stay).
                let next = st
                    .next_slot
                    .entry(key.clone())
                    .or_insert_with(|| file.entries.keys().next_back().map_or(0, |k| k + 1));
                let slot = *next;
                *next += 1;
                st.last_served.insert(key.clone(), slot);
                Planned::Fresh(FreshCall {
                    asked_schema: req.response_schema.is_some(),
                    key,
                    idx: slot,
                    kind,
                    repo,
                    files,
                })
            }
        }
    };
    if let Planned::Fresh(f) = &planned {
        store.part_started(&f.repo_for_plan(store), f.kind);
    }
    planned
}

impl FreshCall {
    /// The repo whose part this call belongs to: the one its prompt names, else the only repo.
    fn repo_for_plan(&self, store: &RunStore) -> String {
        if !self.repo.is_empty() {
            return self.repo.clone();
        }
        let record = store.lock_state();
        match record.record.repos.as_slice() {
            [only] => only.name.clone(),
            _ => String::new(),
        }
    }
}

fn record_entry(store: &RunStore, call: &FreshCall, entry: CallEntry) {
    let mut st = store.lock_state();
    let attempt = st.record.attempt;
    let dir = store.dir().to_path_buf();
    let file = st
        .calls
        .entry(call.key.clone())
        .or_insert_with(|| CallFile {
            key: call.key.clone(),
            kind: call.kind,
            repo: call.repo.clone(),
            files: call.files.clone(),
            entries: BTreeMap::new(),
        });
    file.entries
        .insert(call.idx, CallEntry { attempt, ..entry });
    if let Ok(bytes) = serde_json::to_vec(file) {
        let _ = write_atomic(&call_path(&dir, &call.key), &bytes);
    }
}

fn after_call(store: &RunStore, call: FreshCall, result: &anyhow::Result<LlmResponse>) {
    let entry = match result {
        Ok(resp) => CallEntry {
            attempt: 0,
            status: if reply_is_good(resp) {
                EntryStatus::Done
            } else {
                EntryStatus::Unusable
            },
            at: now(),
            response: Some(StoredResponse::of(resp)),
            error: None,
            class: None,
            cost_usd: resp.cost_usd.unwrap_or(0.0),
            reason: unusable_reason(resp),
            output_path: call.asked_schema.then(|| {
                if crate::llm::structured_output_used(true, &resp.backend, &resp.model) {
                    "schema"
                } else {
                    "tolerant"
                }
                .to_string()
            }),
        },
        Err(e) => {
            let class = crate::failure::classify_error(e);
            CallEntry {
                attempt: 0,
                status: EntryStatus::Failed,
                at: now(),
                response: None,
                error: Some(crate::failure::scrub_message(
                    &e.to_string(),
                    crate::failure::MAX_RECORD_MESSAGE_CHARS,
                )),
                class: Some(class.as_str().to_string()),
                cost_usd: 0.0,
                reason: None,
                output_path: None,
            }
        }
    };
    record_entry(store, &call, entry);
}

/// Why a reply that came back is not a good one, or `None` for a good one: cut off at the output
/// limit (with the tokens it was billed for), or holding no JSON object that parses.
fn unusable_reason(resp: &LlmResponse) -> Option<String> {
    if reply_is_good(resp) {
        return None;
    }
    Some(if resp.truncated {
        format!(
            "the reply was cut off at the output limit after {} output token(s) and {} character(s) of text",
            resp.output_tokens.unwrap_or(0),
            resp.text.chars().count()
        )
    } else {
        "the reply held no JSON object that parses".to_string()
    })
}

/// The longest rejection reason a stored entry keeps.
const MAX_REASON_CHARS: usize = 1_500;

/// Record on the stored reply to `request_key` that the caller turned it down for `reason`: the
/// reply handed back last for that key (a replay's stored reply or the fresh call just made).
/// When `whole` (every entry of the reply was turned down) a good reply becomes
/// [`EntryStatus::Rejected`] so a resume asks again; a reply only part of which was turned down
/// keeps its status and only gains the reason, so a resume replays the entries that were
/// accepted (the ones turned down were re-asked with a changed request, stored under its own
/// key) instead of paying for the whole reply again. A reply that was already unusable keeps
/// its status and gains the reason after the one it has. No reply is touched except that one.
pub(crate) fn reject_reply(store: &RunStore, request_key: &str, reason: &str, whole: bool) {
    let reason = crate::failure::scrub_message(reason, MAX_REASON_CHARS);
    let dir = store.dir().to_path_buf();
    let mut st = store.lock_state();
    let Some(&slot) = st.last_served.get(request_key) else {
        return;
    };
    let Some(file) = st.calls.get_mut(request_key) else {
        return;
    };
    let Some(entry) = file.entries.get_mut(&slot) else {
        return;
    };
    if whole && entry.status == EntryStatus::Done {
        entry.status = EntryStatus::Rejected;
    }
    entry.reason = Some(match entry.reason.take() {
        Some(earlier) if !earlier.contains(&reason) => format!("{earlier}; {reason}"),
        Some(earlier) => earlier,
        None => reason,
    });
    if let Ok(bytes) = serde_json::to_vec(file) {
        let _ = write_atomic(&call_path(&dir, request_key), &bytes);
    }
}

/// The model seam over a plain [`LlmPort`] (a scripted model in tests, or the real client for a
/// mode that cannot batch).
pub struct JournalLlm<'a> {
    inner: &'a dyn LlmPort,
    store: Arc<RunStore>,
}

impl<'a> JournalLlm<'a> {
    pub fn new(inner: &'a dyn LlmPort, store: Arc<RunStore>) -> Self {
        Self { inner, store }
    }
}

#[async_trait::async_trait]
impl LlmPort for JournalLlm<'_> {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        require_seed(&self.store)?;
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => Ok(*resp),
            Planned::Fresh(call) => {
                let result = self.inner.complete(req).await;
                after_call(&self.store, call, &result);
                result
            }
        }
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        require_seed(&self.store)?;
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => {
                on_delta(&resp.text);
                Ok(*resp)
            }
            Planned::Fresh(call) => {
                let result = self.inner.complete_streaming(req, on_delta).await;
                after_call(&self.store, call, &result);
                result
            }
        }
    }

    fn note_rejected_reply(&self, request_key: &str, reason: &str, whole: bool) {
        reject_reply(&self.store, request_key, reason, whole);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        // A borrowed completer cannot be downcast (it is not 'static), and a plain completer has
        // no batch path anyway: the batch dispatch asks for a concrete client and says so.
        &()
    }
}

/// The real-time half of a journaled [`BatchApi`]: calls go to the batch API's own completer.
struct JournalRealtime {
    inner: Arc<dyn BatchApi>,
    store: Arc<RunStore>,
}

#[async_trait::async_trait]
impl LlmPort for JournalRealtime {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        require_seed(&self.store)?;
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => Ok(*resp),
            Planned::Fresh(call) => {
                let result = self.inner.realtime().complete(req).await;
                after_call(&self.store, call, &result);
                result
            }
        }
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        require_seed(&self.store)?;
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => {
                on_delta(&resp.text);
                Ok(*resp)
            }
            Planned::Fresh(call) => {
                let result = self
                    .inner
                    .realtime()
                    .complete_streaming(req, on_delta)
                    .await;
                after_call(&self.store, call, &result);
                result
            }
        }
    }

    fn note_rejected_reply(&self, request_key: &str, reason: &str, whole: bool) {
        reject_reply(&self.store, request_key, reason, whole);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Batches ──────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRow {
    custom_id: String,
    response: Option<StoredResponse>,
    error: Option<String>,
    billed: Option<StoredResponse>,
}

impl StoredRow {
    fn of(row: &BatchResultRow) -> Self {
        Self {
            custom_id: row.custom_id.clone(),
            response: row.response.as_ref().map(StoredResponse::of),
            error: row.error.clone(),
            billed: row.billed.as_ref().map(StoredResponse::of),
        }
    }

    fn replayed(&self) -> BatchResultRow {
        BatchResultRow {
            custom_id: self.custom_id.clone(),
            response: self.response.as_ref().map(StoredResponse::replayed),
            error: self.error.clone(),
            billed: self.billed.as_ref().map(StoredResponse::replayed),
        }
    }
}

/// A submitted batch, stored the moment the submit returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchFile {
    key: String,
    n: usize,
    batch_id: String,
    submitted_at: String,
    attempt: u32,
    custom_ids: Vec<String>,
    /// The fetched results, once there are some.
    #[serde(default)]
    rows: Option<Vec<StoredRow>>,
}

fn batch_path(dir: &Path, key: &str, n: usize) -> std::path::PathBuf {
    dir.join("batches").join(format!("{key}-{n}.json"))
}

/// The batch calls of a run, stored. A batch already submitted is never submitted again: its id is
/// returned, and the caller polls and fetches it. Fetched results are kept, so a batch fetched
/// once is not fetched again (batch results expire).
pub struct JournalBatchApi {
    inner: Arc<dyn BatchApi>,
    store: Arc<RunStore>,
    realtime: JournalRealtime,
    /// Batch id -> (key, n), for the batches this attempt submitted or found.
    by_id: std::sync::Mutex<HashMap<String, (String, usize)>>,
}

impl JournalBatchApi {
    pub fn new(inner: Arc<dyn BatchApi>, store: Arc<RunStore>) -> Self {
        Self {
            realtime: JournalRealtime {
                inner: inner.clone(),
                store: store.clone(),
            },
            inner,
            store,
            by_id: Default::default(),
        }
    }

    fn remember(&self, batch_id: &str, key: &str, n: usize) {
        if let Ok(mut map) = self.by_id.lock() {
            map.insert(batch_id.to_string(), (key.to_string(), n));
        }
    }
}

#[async_trait::async_trait]
impl BatchApi for JournalBatchApi {
    fn has_api_key(&self) -> bool {
        self.inner.has_api_key()
    }

    fn realtime(&self) -> &dyn LlmPort {
        &self.realtime
    }

    async fn count_input_tokens(&self, req: &LlmRequest, model: &str) -> Option<u64> {
        self.inner.count_input_tokens(req, model).await
    }

    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        require_seed(&self.store)?;
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(&items).unwrap_or_default());
        let key = hex(&h.finalize());
        let n = {
            let mut st = self.store.lock_state();
            let n = st.submitted.entry(key.clone()).or_insert(0);
            let this = *n;
            *n += 1;
            this
        };
        let dir = self.store.dir().to_path_buf();
        if let Some(stored) = read_json::<BatchFile>(&batch_path(&dir, &key, n)) {
            // Submitted before, perhaps finished and already billed: never submit it again.
            self.remember(&stored.batch_id, &key, n);
            return Ok(stored.batch_id);
        }
        let custom_ids = items.iter().map(|i| i.custom_id.clone()).collect();
        let batch_id = self.inner.submit(items).await?;
        let record = BatchFile {
            key: key.clone(),
            n,
            batch_id: batch_id.clone(),
            submitted_at: now(),
            attempt: self.store.record().attempt,
            custom_ids,
            rows: None,
        };
        if let Ok(bytes) = serde_json::to_vec(&record) {
            let _ = write_atomic(&batch_path(&dir, &key, n), &bytes);
        }
        self.remember(&batch_id, &key, n);
        Ok(batch_id)
    }

    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        let stored_rows = self.stored_rows(batch_id).is_some();
        if stored_rows {
            // Fetched in an earlier attempt: it ended, and there is nothing to wait for.
            return Ok(BatchStatus {
                processing_status: "ended".to_string(),
                request_counts: BatchRequestCounts::default(),
            });
        }
        self.inner.poll(batch_id).await
    }

    async fn fetch(&self, batch_id: &str, ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        if let Some(rows) = self.stored_rows(batch_id) {
            return Ok(rows.iter().map(StoredRow::replayed).collect());
        }
        let rows = self.inner.fetch(batch_id, ttl).await?;
        let found = self
            .by_id
            .lock()
            .ok()
            .and_then(|m| m.get(batch_id).cloned());
        if let Some((key, n)) = found {
            let dir = self.store.dir().to_path_buf();
            if let Some(mut stored) = read_json::<BatchFile>(&batch_path(&dir, &key, n)) {
                stored.rows = Some(rows.iter().map(StoredRow::of).collect());
                if let Ok(bytes) = serde_json::to_vec(&stored) {
                    let _ = write_atomic(&batch_path(&dir, &key, n), &bytes);
                }
            }
        }
        Ok(rows)
    }
}

impl JournalBatchApi {
    fn stored_rows(&self, batch_id: &str) -> Option<Vec<StoredRow>> {
        let (key, n) = self.by_id.lock().ok()?.get(batch_id).cloned()?;
        read_json::<BatchFile>(&batch_path(self.store.dir(), &key, n))?.rows
    }
}

/// Journal the model seam `llm` for a run. A completer that carries a [`BatchApi`] (the real
/// client, or a test double) becomes a batch seam whose submits, polls and fetches are stored too;
/// any other completer becomes a [`JournalLlm`].
pub fn wrap<'a>(llm: &'a dyn LlmPort, store: Arc<RunStore>) -> Box<dyn LlmPort + 'a> {
    if let Some(seam) = llm.as_any().downcast_ref::<crate::ai_audit::BatchSeam>() {
        return Box::new(crate::ai_audit::BatchSeam::new(Arc::new(
            JournalBatchApi::new(seam.api_arc(), store),
        )));
    }
    if let Some(real) = llm.as_any().downcast_ref::<crate::llm::Llm>() {
        let api: Arc<dyn BatchApi> = Arc::new(real.clone());
        return Box::new(crate::ai_audit::BatchSeam::new(Arc::new(
            JournalBatchApi::new(api, store),
        )));
    }
    Box::new(JournalLlm::new(llm, store))
}

// ── The end of an attempt ────────────────────────────────────────────────────────────────────

fn all_call_files(store: &RunStore) -> Vec<CallFile> {
    let dir = store.dir().join("calls");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut from_disk: HashMap<String, CallFile> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_json::<CallFile>(&e.path()))
        .map(|f| (f.key.clone(), f))
        .collect();
    // This attempt's in-memory files are the newest.
    for (key, file) in &store.lock_state().calls {
        from_disk.insert(key.clone(), file.clone());
    }
    from_disk.into_values().collect()
}

/// Set aside the good replies of a stage that a later step rejected, so a resume asks again. A
/// pass that failed for a CONTENT reason (the reply parsed but held nothing usable) is the case:
/// its replies are marked rejected. A scan pass names the files it did not review, so only the
/// calls over those files are set aside; every other stage is set aside as a whole for the repo,
/// with the failed part named as the reason. The fix and detail stages are NOT set aside here:
/// each of their replies was marked at the point of rejection ([`reject_reply`]), with its
/// reason, and a reply with an accepted entry in it stays good.
pub(crate) fn quarantine_rejected(store: &RunStore, failures: &[FinishFailure]) {
    let rejected: Vec<&FinishFailure> = failures
        .iter()
        .filter(|f| !f.scope_note && f.class == "content")
        .collect();
    if rejected.is_empty() {
        return;
    }
    let dir = store.dir().to_path_buf();
    for mut file in all_call_files(store) {
        if matches!(file.kind, PartKind::Fix | PartKind::Detail) {
            continue;
        }
        let hit = rejected.iter().find(|f| {
            PartKind::of_pass_name(&f.pass) == file.kind
                && (f.repo.is_empty() || file.repo.is_empty() || f.repo == file.repo)
                && (file.kind != PartKind::Scan
                    || f.files.is_empty()
                    || file.files.iter().any(|p| f.files.contains(p)))
        });
        let Some(hit) = hit else {
            continue;
        };
        let mut changed = false;
        for entry in file.entries.values_mut() {
            if entry.status == EntryStatus::Done {
                entry.status = EntryStatus::Rejected;
                entry.reason.get_or_insert_with(|| {
                    format!(
                        "set aside because the part \"{}\" failed on content: no usable result \
                         came out of this stage's replies",
                        hit.pass
                    )
                });
                changed = true;
            }
        }
        if changed {
            if let Ok(bytes) = serde_json::to_vec(&file) {
                let _ = write_atomic(&call_path(&dir, &file.key), &bytes);
            }
            store.lock_state().calls.insert(file.key.clone(), file);
        }
    }
}

/// Fill each model part's call counts from the stored files.
pub(crate) fn count_calls(store: &RunStore) {
    let files = all_call_files(store);
    {
        let mut st = store.lock_state();
        let single_repo = st.record.repos.len() == 1;
        for part in &mut st.record.plan {
            part.calls_done = 0;
            part.calls_not_done = 0;
        }
        for file in &files {
            for entry in file.entries.values() {
                for part in st.record.plan.iter_mut().filter(|p| {
                    p.kind == file.kind
                        && (p.repo == file.repo || (file.repo.is_empty() && single_repo))
                }) {
                    if replays_on_resume(entry.status, file.kind) {
                        part.calls_done += 1;
                    } else {
                        part.calls_not_done += 1;
                    }
                }
            }
        }
    }
    let _ = store.persist();
}

/// What a resume of `record` would cost, from the stored files of `dir`.
pub(crate) fn estimate_remaining(dir: &Path, record: &RunRecord) -> ResumeEstimate {
    let mut stored = 0usize;
    let mut again = 0usize;
    let mut spent = 0.0f64;
    if let Ok(entries) = std::fs::read_dir(dir.join("calls")) {
        for file in entries
            .filter_map(Result::ok)
            .filter_map(|e| read_json::<CallFile>(&e.path()))
        {
            for entry in file.entries.values() {
                if replays_on_resume(entry.status, file.kind) {
                    stored += 1;
                    spent += entry.cost_usd;
                } else {
                    again += 1;
                }
            }
        }
    }
    let average = if stored > 0 {
        spent / stored as f64
    } else {
        0.0
    };
    let unreached = record
        .plan
        .iter()
        .filter(|p| {
            p.kind.is_model_work() && matches!(p.status, PartStatus::Pending | PartStatus::Running)
        })
        .count();
    let again_usd = again as f64 * average;
    let (usd, basis) = match record.estimate_usd {
        Some(full) if unreached > 0 => (
            (full - spent).max(again_usd),
            format!(
                "the run's own pre-flight estimate of ${full:.2}, less the ${spent:.2} already \
                 spent; {unreached} stage(s) had not finished"
            ),
        ),
        _ if unreached > 0 => (
            again_usd,
            format!(
                "{again} call(s) at the ${average:.4} average of the stored ones; {unreached} \
                 stage(s) had not finished and this run recorded no estimate for them"
            ),
        ),
        _ => (
            again_usd,
            format!("{again} call(s) at the ${average:.4} average of the stored ones"),
        ),
    };
    ResumeEstimate {
        remaining: record
            .remaining_parts()
            .iter()
            .map(|p| {
                let status = match p.status {
                    PartStatus::Failed => "failed",
                    PartStatus::Running => "was running",
                    _ => "not started",
                };
                if p.repo.is_empty() {
                    format!("{} ({status})", p.kind.label())
                } else {
                    format!("{}: {} ({status})", p.repo, p.kind.label())
                }
            })
            .collect(),
        calls_stored: stored,
        calls_to_send_again: again,
        usd,
        basis,
    }
}
