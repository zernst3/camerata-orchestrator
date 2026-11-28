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
use camerata_llm::failure::FailureClass;

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
    /// The model the backend said answered, beside the requested `model` above (see
    /// `LlmResponse::reported_model`).
    #[serde(default)]
    pub reported_model: Option<String>,
    /// The dollar figure the CLI itself reported, beside the modeled `cost_usd` (see
    /// `LlmResponse::cli_cost_usd`).
    #[serde(default)]
    pub cli_cost_usd: Option<f64>,
    /// How long the call took in the session that made it (`LlmResponse::elapsed_ms`). Kept so a
    /// resume's saved journal still holds the earlier sessions' timings; a replay returns none of
    /// it, so live telemetry counts only what the session itself sends. Operator-only.
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
    /// Unix milliseconds at which the call ended, in the session that made it.
    #[serde(default)]
    pub ended_unix_ms: Option<u64>,
}

impl StoredResponse {
    pub(crate) fn of(r: &LlmResponse) -> Self {
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
            reported_model: r.reported_model.clone(),
            cli_cost_usd: r.cli_cost_usd,
            elapsed_ms: r.elapsed_ms,
            ended_unix_ms: Some(crate::stage_telemetry::now_unix_ms()),
        }
    }

    /// The reply as a replay returns it: the same text, and no cost, because this attempt did not
    /// pay for it. The usage meter therefore counts only what this attempt sends; a spend limit
    /// adds the stored cost separately (`RunControls::stored_spend_usd`).
    pub(crate) fn replayed(&self) -> LlmResponse {
        LlmResponse {
            reported_model: self.reported_model.clone(),
            // Replayed at no cost, so there is no CLI figure to compare either.
            cli_cost_usd: None,
            elapsed_ms: None,
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
    /// A reply came back but was cut off or held no JSON: asked again on a resume (the fix and
    /// detail stages replay it, see [`replays_on_resume`]).
    Unusable,
    /// A good reply every entry of which a later step rejected: asked again on a resume (never
    /// replayed, whatever the stage). The reason is on the entry ([`CallEntry::reason`]). A
    /// reply only PART of which was rejected stays [`EntryStatus::Done`] with the reason: what
    /// it got right is replayed.
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
    /// The stage that owns the call when it records its own rejected replies at the point of
    /// rejection ([`reject_reply`]). `None` for every other call and for a file written before
    /// this existed (those keep the blanket set-aside of [`quarantine_rejected`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<CallOwner>,
    /// By occurrence (0 is the first time the request was asked).
    pub entries: BTreeMap<usize, CallEntry>,
}

/// A stage that records its own rejected replies, so no other stage's failure may set its calls
/// aside as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOwner {
    /// The clone judge (`onboard::clone_advisory`).
    CloneJudge,
}

impl CallOwner {
    /// The owner of a request, from its system prompt.
    fn of(req: &LlmRequest) -> Option<Self> {
        req.system
            .as_deref()
            .is_some_and(|s| {
                s.starts_with(crate::onboard::clone_advisory::prompt::JUDGE_SYSTEM_PREFIX)
            })
            .then_some(Self::CloneJudge)
    }
}

fn call_path(dir: &Path, key: &str) -> std::path::PathBuf {
    dir.join("calls").join(format!("{key}.json"))
}

// ── Naming a request ─────────────────────────────────────────────────────────────────────────

/// The digest that names `req`, computed over the request AFTER the model-input chokepoint
/// (`camerata_llm::model_input::guarded`): the key names what the model is sent. A request that
/// is already redacted (every inspection prompt is) passes the chokepoint unchanged, so its key
/// is the one a saved run filed its reply under before the chokepoint existed; a request that
/// still carried a secret is named by its redacted form, the same as if it had been redacted
/// upstream.
pub fn request_key(req: &LlmRequest) -> String {
    let req = camerata_llm::model_input::guarded(req);
    let req = &*req;
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
            (
                PartKind::GroupReview,
                head(crate::ai_audit::group_review::group_review_system_prompt()),
            ),
            (
                PartKind::GroupReview,
                head(crate::ai_audit::group_review::rule_summary_system_prompt()),
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
/// A good reply ([`EntryStatus::Done`]) always is, including one only PART of which was turned
/// down: what it got right costs nothing again, and the findings it got wrong were re-asked by
/// the stage's next round, whose request names the reason and is stored under its own key.
///
/// A reply every entry of which was turned down ([`EntryStatus::Rejected`]) is NOT replayed, for
/// any stage: a resume of a part that failed on content exists to ask again, and replaying the
/// rejected reply makes the stage judge it again, turn it down for the same reason and have no
/// round left (the hearth run printed "0 to send again" and failed identically). It is asked
/// again, and it counts under "to send again" in the resume estimate.
///
/// The fix and detail stages keep one more case: a reply that was cut off or held no JSON
/// ([`EntryStatus::Unusable`]) is replayed, because the stage's next round splits the batch and
/// that smaller request was stored under its own key; asking the cut-off request again would pay
/// twice for a reply that will be cut off again. (A last round of one finding has no smaller
/// round to split to; that case still replays.) A failed call has no reply to replay.
///
/// Every other stage asks an unusable reply again ONLY IF the current parser still refuses it
/// ([`entry_replays`]): "unusable" was decided by the parser of the run that stored it, and a
/// paid reply is never paid for twice because a parser was wrong.
pub(crate) fn replays_on_resume(status: EntryStatus, kind: PartKind) -> bool {
    match status {
        EntryStatus::Done => true,
        EntryStatus::Unusable => matches!(kind, PartKind::Fix | PartKind::Detail),
        EntryStatus::Rejected | EntryStatus::Failed => false,
    }
}

/// Whether the stored `entry` of a call over a `kind` stage is answered from the store on a
/// resume: [`replays_on_resume`], and for a stored unusable reply of a stage that does not replay
/// those, whether the CURRENT parser accepts the stored text ([`reply_is_good`]: finished, a JSON
/// object that parses, and for the group reviewer its closed schema).
///
/// Why. A reply is stored as unusable when the parser of the run that made the call refused it,
/// and the call was paid for. When the parser is later fixed (a missing escalation rank is read
/// as unranked, ecc043b4) the stored text is a good reply, and asking again would pay twice for
/// it. Re-reading it is free. A reply the current parser still refuses (a tool-call transcript,
/// a reply cut off at the output limit, a field the schema does not have) is asked again.
///
/// Every kind that can be stored unusable and what this does for it:
/// - scan, calibration, alternatives, other (the clone judge, the deep tier): the reply is
///   usable when it is finished and holds a JSON object that parses; re-read with that rule.
/// - group review: also the closed schema ([`Stage2Reply::parse`] or [`parse_rule_summary`]).
/// - fix, detail: replayed whatever the parser says (see [`replays_on_resume`]); their own
///   stage reads a replayed reply entry by entry and re-asks only what it cannot read.
/// - tools, merge, export: no model call is stored under them.
///
/// [`Stage2Reply::parse`]: crate::ai_audit::group_review::Stage2Reply::parse
pub(crate) fn entry_replays(entry: &CallEntry, kind: PartKind) -> bool {
    if replays_on_resume(entry.status, kind) {
        return true;
    }
    // `Other` (the clone judge, the deep lenses) has no per-request parser a resume can run: the
    // request that would say which shape the reply should have is not stored. What can be said
    // is whether the reply holds a verdict object, the clone judge's shape. An unusable `Other`
    // reply the judge's parser reads is good (so it is not paid for twice when the parser was
    // fixed since); any other unusable `Other` reply is always asked again.
    if entry.status != EntryStatus::Unusable {
        return false;
    }
    let Some(stored) = entry.response.as_ref() else {
        return false;
    };
    let reply = stored.replayed();
    if kind == PartKind::Other {
        return !reply.truncated
            && crate::onboard::clone_advisory::prompt::find_verdict_object(&reply.text).is_some();
    }
    reply_is_good(kind, &reply)
}

/// True when a reply is one a resume may return again: finished, with a JSON object that parses.
/// A group reviewer reply must also satisfy its closed schema ([`Stage2Reply::parse`]): the stage
/// turns down a reply that does not, and replaying it on a resume would only be turned down again.
///
/// [`Stage2Reply::parse`]: crate::ai_audit::group_review::Stage2Reply::parse
fn reply_is_good(kind: PartKind, resp: &LlmResponse) -> bool {
    let parses = !resp.truncated
        && crate::ai_audit::extract_json_object(&resp.text)
            .is_some_and(|json| serde_json::from_str::<serde_json::Value>(json).is_ok());
    // A clone judge reply with braces in prose around its verdict object does not parse as one
    // span but is read by the judge's own parser; the other `Other` calls have no such shape.
    if kind == PartKind::Other {
        return !resp.truncated
            && (parses
                || crate::onboard::clone_advisory::prompt::find_verdict_object(&resp.text)
                    .is_some());
    }
    if !parses {
        return false;
    }
    // Each stage's OWN parser decides, never "some JSON object parses": a tool-call transcript or
    // a refusal that quotes an object is a JSON object that parses and a reply of no stage.
    match kind {
        PartKind::GroupReview => {
            crate::ai_audit::group_review::Stage2Reply::parse(&resp.text).is_ok()
                || crate::ai_audit::group_review::parse_rule_summary(&resp.text).is_ok()
        }
        PartKind::Scan => crate::ai_audit::is_scan_reply(&resp.text),
        PartKind::Calibration => crate::ai_audit::is_verdict_reply(&resp.text),
        PartKind::Alternatives => crate::ai_audit::is_recommendations_reply(&resp.text),
        // Fix and detail read their replies entry by entry; the other calls (the clone judge, the
        // deep lenses) each read their own shape. Finished and holding an object is all that can
        // be said of them here.
        _ => true,
    }
}

// ── The call seam ────────────────────────────────────────────────────────────────────────────

/// What `plan_call` decided for one request.
enum Planned {
    Replay(Box<LlmResponse>),
    Fresh(FreshCall),
    /// A rerun (#588) holds no stored reply for the request and may not ask a model: the miss is
    /// recorded in the run's list and the call fails. Nothing is stored and no part is started.
    Missing,
}

/// How a missing reply is named in a rerun's refusal: the stage, the repo and the files its
/// request framed.
pub(crate) fn describe_missing(kind: PartKind, repo: &str, files: &[String]) -> String {
    let mut out = format!("{} reply", kind.label());
    if !repo.is_empty() {
        out.push_str(&format!(" for {repo}"));
    }
    match files {
        [] => {}
        [one] => out.push_str(&format!(" (file {one})")),
        [first, rest @ ..] => {
            out.push_str(&format!(" (file {first} and {} more)", rest.len()));
        }
    }
    out
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
    owner: Option<CallOwner>,
}

/// The slots of `file` a request is answered from, in the order they answer. A resume answers
/// from the good replies only. A rerun answers the same way and falls back to a reply the run
/// stored but a resume would ask again (rejected, unusable) only when the request has no good one,
/// so a bad first attempt never comes before a good later one.
fn replayable_slots(file: &CallFile, rerun: bool) -> Vec<usize> {
    let kind = file.kind;
    let good: Vec<usize> = file
        .entries
        .iter()
        .filter(|(_, e)| entry_replays(e, kind) && e.response.is_some())
        .map(|(s, _)| *s)
        .collect();
    if rerun && good.is_empty() {
        return file
            .entries
            .iter()
            .filter(|(_, e)| e.response.is_some())
            .map(|(s, _)| *s)
            .collect();
    }
    good
}

/// What a stage sees when a rerun has no stored reply for its request.
pub(crate) fn missing_reply_error() -> anyhow::Error {
    anyhow::anyhow!(
        "this is a rerun of the deterministic stages: it answers only from the run's stored \
         replies and never asks a model, and this reply is not stored"
    )
}

/// Every stored reply under the request digest `key` in the run folder `dir`, oldest first,
/// whatever became of it (a reply a stage turned down is still the model's reply). A reply that
/// was cut off at the output limit is left out. Nothing is changed: this reads the folder and
/// takes no part in the run's own bookkeeping.
pub fn stored_reply_texts(dir: &Path, key: &str) -> Vec<String> {
    let Some(file) = load_call_file(dir, key) else {
        return Vec::new();
    };
    file.entries
        .values()
        .filter_map(|e| e.response.as_ref())
        .filter(|r| !r.truncated)
        .map(|r| r.text.clone())
        .collect()
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
                owner: CallOwner::of(req),
                entries: BTreeMap::new(),
            })
        });
        // The idx-th ask is answered by the idx-th good reply, in the order they were stored.
        let file_kind = file.kind;
        // A rerun keeps the model work as the saved run made it: every stored reply is the answer,
        // whatever the stage made of it (a reply the stage turned down is turned down again, the
        // way it was). Only a call that never got a reply is missing. A resume asks those again.
        let rerun = store.rerun_misses().is_some();
        let good_slot = replayable_slots(file, rerun).into_iter().nth(idx);
        let good = good_slot.and_then(|slot| {
            // A refused reply the current parser accepts is a good reply from here on: stored as
            // one, so the run's accounting (a later set-aside, the counts) agrees with the replay.
            // A rerun changes nothing it stored.
            let entry = file.entries.get_mut(&slot)?;
            if !rerun
                && entry.status == EntryStatus::Unusable
                && !replays_on_resume(entry.status, file_kind)
            {
                entry.status = EntryStatus::Done;
                entry.reason = None;
                if let Ok(bytes) = serde_json::to_vec(&*file) {
                    let _ = write_atomic(&call_path(&dir, &key), &bytes);
                }
            }
            file.entries
                .get(&slot)?
                .response
                .as_ref()
                .map(StoredResponse::replayed)
        });
        match good {
            Some(replay) => {
                if let Some(slot) = good_slot {
                    st.last_served.insert(key.clone(), slot);
                }
                Planned::Replay(Box::new(replay))
            }
            None => {
                // A rerun answers from the store alone: a missing reply is recorded by name
                // and the call fails before anything is stored or started.
                if let Some(misses) = store.rerun_misses() {
                    misses.record(describe_missing(kind, &repo, &files));
                    return Planned::Missing;
                }
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
                    owner: CallOwner::of(req),
                })
            }
        }
    };
    if let Planned::Fresh(f) = &planned {
        store.part_started(&f.repo_for_plan(store), f.kind);
    }
    planned
}

/// Whether `plan_call` would answer `req` from the store right now: the same key, the same
/// position among this attempt's asks, the same test of a stored reply, with nothing recorded.
/// A spend limit asks this before admitting a call, because a replay costs nothing and refusing
/// it would drop a reply the run already paid for.
fn would_replay(store: &RunStore, req: &LlmRequest) -> bool {
    let req = camerata_llm::model_input::guarded(req);
    let key = request_key(&req);
    let (kind, repo, files) = classify(&req);
    let mut st = store.lock_state();
    let idx = st.asked.get(&key).copied().unwrap_or(0);
    let dir = store.dir().to_path_buf();
    let st = &mut *st;
    let file = st.calls.entry(key.clone()).or_insert_with(|| {
        load_call_file(&dir, &key).unwrap_or_else(|| CallFile {
            key: key.clone(),
            kind,
            repo: repo.clone(),
            files: files.clone(),
            owner: CallOwner::of(&req),
            entries: BTreeMap::new(),
        })
    });
    let rerun = store.rerun_misses().is_some();
    replayable_slots(file, rerun).get(idx).is_some()
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
            owner: call.owner,
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
            status: if reply_is_good(call.kind, resp) {
                EntryStatus::Done
            } else {
                EntryStatus::Unusable
            },
            at: now(),
            response: Some(StoredResponse::of(resp)),
            error: None,
            class: None,
            cost_usd: resp.cost_usd.unwrap_or(0.0),
            reason: unusable_reason(call.kind, resp),
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
                // A refusal carries what the call was billed: the money was paid, so a resumed
                // run's starting spend must hold it.
                cost_usd: crate::llm::refusal_billed(e)
                    .and_then(|b| b.cost_usd)
                    .unwrap_or(0.0),
                reason: None,
                output_path: None,
            }
        }
    };
    record_entry(store, &call, entry);
}

/// Why a reply that came back is not a good one, or `None` for a good one: cut off at the output
/// limit (with the tokens it was billed for), or holding no JSON object that parses.
fn unusable_reason(kind: PartKind, resp: &LlmResponse) -> Option<String> {
    if reply_is_good(kind, resp) {
        return None;
    }
    Some(if resp.truncated {
        format!(
            "the reply was cut off at the output limit after {} output token(s) and {} character(s) of text",
            resp.output_tokens.unwrap_or(0),
            resp.text.chars().count()
        )
    } else if kind == PartKind::GroupReview {
        "the reply is not a group reviewer reply that fits the closed schema".to_string()
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
    // A rerun keeps the saved run's replies exactly as they are stored.
    if store.rerun_misses().is_some() {
        return;
    }
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
        // The seam names the request it hands on, so the model sees what the key names.
        let req = camerata_llm::model_input::guarded_owned(req);
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => Ok(*resp),
            Planned::Missing => Err(missing_reply_error()),
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
        let req = camerata_llm::model_input::guarded_owned(req);
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => {
                on_delta(&resp.text);
                Ok(*resp)
            }
            Planned::Missing => Err(missing_reply_error()),
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

    fn replays(&self, req: &LlmRequest) -> bool {
        would_replay(&self.store, req)
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
        let req = camerata_llm::model_input::guarded_owned(req);
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => Ok(*resp),
            Planned::Missing => Err(missing_reply_error()),
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
        let req = camerata_llm::model_input::guarded_owned(req);
        match plan_call(&self.store, &req) {
            Planned::Replay(resp) => {
                on_delta(&resp.text);
                Ok(*resp)
            }
            Planned::Missing => Err(missing_reply_error()),
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

    fn replays(&self, req: &LlmRequest) -> bool {
        would_replay(&self.store, req)
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
    /// The batch a later attempt sent for this one's rows that were refused for
    /// billing (no reply, nothing billed): its file's `(key, n)`. That batch is an ordinary batch file
    /// and may carry a follow-up of its own, so the files form a chain from the batch the stage
    /// first submitted. The chain is read as ONE set of rows ([`merged_rows`]).
    #[serde(default)]
    follow: Option<(String, usize)>,
}

impl StoredRow {
    /// Whether the row is sent again on a resume: only a row the account refused for billing
    /// (the class that stops a run, [`FailureClass::AuthOrBilling`]) with no reply and nothing
    /// billed. Such a row never ran, and the stage did not send it again in its attempt.
    ///
    /// Every other row stays as it was stored and is replayed as before: a reply is `Done`
    /// ([`replays_on_resume`]); a refusal or any other error was already dealt with by the
    /// stage in that attempt (it resent the item as a batch of its own, which is stored under
    /// its own hash and found by the stage on the resume), and sending it from here too would
    /// pay for it twice or duplicate a batch that may be running. What a row was billed stays in
    /// the stored spend ([`stored_spend`]).
    ///
    /// One more kind of row never ran: a row of a batch that was CANCELED (the operator's stop,
    /// #583, or a cancel at the provider) or that EXPIRED first, which the provider answers as
    /// `canceled` or `expired`, nothing billed ([`camerata_llm::failure::is_unrun_batch_row`]).
    /// The stage never sends such a row again while the run lives (that would be billed once then
    /// and once more here); without this a resume would answer the item from that batch for ever
    /// and the run could never complete.
    fn needs_resend(&self) -> bool {
        self.response.is_none()
            && self.billed.is_none()
            && self.error.as_deref().is_some_and(|e| {
                camerata_llm::failure::is_unrun_batch_row(e)
                    || camerata_llm::failure::classify_text(e).0 == FailureClass::AuthOrBilling
            })
    }
}

/// The files of a batch and of every batch sent after it for its failed rows, oldest first.
fn batch_chain(dir: &Path, key: &str, n: usize) -> Vec<(String, usize, BatchFile)> {
    let mut chain = Vec::new();
    let mut next = Some((key.to_string(), n));
    while let Some((k, i)) = next {
        let Some(file) = read_json::<BatchFile>(&batch_path(dir, &k, i)) else {
            break;
        };
        // A chain is as long as a run has resumes; the bound only stops a damaged loop.
        if chain.len() >= 1_000 {
            break;
        }
        next = file.follow.clone();
        chain.push((k, i, file));
    }
    chain
}

/// The rows of a chain as one set, in the order of the first batch: a row that is not to be sent
/// again ([`StoredRow::needs_resend`]) wins over the rows after it, and an item with none carries
/// its latest row. `None` when the first batch has no
/// fetched rows.
fn merged_rows(chain: &[(String, usize, BatchFile)]) -> Option<Vec<StoredRow>> {
    let mut merged = chain.first()?.2.rows.clone()?;
    for (_, _, file) in &chain[1..] {
        for row in file.rows.iter().flatten() {
            match merged.iter_mut().find(|m| m.custom_id == row.custom_id) {
                Some(m) if m.needs_resend() => *m = row.clone(),
                Some(_) => {}
                None => merged.push(row.clone()),
            }
        }
    }
    Some(merged)
}

fn hash_items(items: &[BatchItem]) -> String {
    let mut h = Sha256::new();
    h.update(serde_json::to_vec(items).unwrap_or_default());
    hex(&h.finalize())
}

fn batch_path(dir: &Path, key: &str, n: usize) -> std::path::PathBuf {
    dir.join("batches").join(format!("{key}-{n}.json"))
}

/// Where a batch id is stored: its own file `(key, n)`, and the first batch of its chain `view`,
/// whose rows the caller of `fetch` gets (the same file for a batch that was never resent).
#[derive(Debug, Clone)]
struct Slot {
    key: String,
    n: usize,
    view: (String, usize),
}

/// The batch calls of a run, stored. A batch already submitted is never submitted again: its id is
/// returned, and the caller polls and fetches it. Fetched results are kept, so a batch fetched
/// once is not fetched again (batch results expire).
pub struct JournalBatchApi {
    inner: Arc<dyn BatchApi>,
    store: Arc<RunStore>,
    realtime: JournalRealtime,
    /// Batch id -> where it is stored, for the batches this attempt submitted or found.
    by_id: std::sync::Mutex<HashMap<String, Slot>>,
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

    fn remember(&self, batch_id: &str, key: &str, n: usize, view: (&str, usize)) {
        if let Ok(mut map) = self.by_id.lock() {
            map.insert(
                batch_id.to_string(),
                Slot {
                    key: key.to_string(),
                    n,
                    view: (view.0.to_string(), view.1),
                },
            );
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
        let key = hash_items(&items);
        let n = {
            let mut st = self.store.lock_state();
            let n = st.submitted.entry(key.clone()).or_insert(0);
            let this = *n;
            *n += 1;
            this
        };
        let dir = self.store.dir().to_path_buf();
        let chain = batch_chain(&dir, &key, n);
        if let Some((tail_key, tail_n, tail)) = chain.last() {
            if tail.rows.is_none() {
                // Submitted before and not fetched (the batch itself, or the one sent for its
                // failed rows): it may be running or finished and billed. Never submit it again.
                self.remember(&tail.batch_id, tail_key, *tail_n, (key.as_str(), n));
                return Ok(tail.batch_id.clone());
            }
            let merged = merged_rows(&chain).unwrap_or_default();
            let again: Vec<BatchItem> = items
                .iter()
                .filter(|i| {
                    merged
                        .iter()
                        .find(|r| r.custom_id == i.custom_id)
                        .is_some_and(StoredRow::needs_resend)
                })
                .cloned()
                .collect();
            if again.is_empty() {
                // No row is to be sent again: the stored rows answer it, nothing is sent.
                self.remember(&chain[0].2.batch_id, &key, n, (key.as_str(), n));
                return Ok(chain[0].2.batch_id.clone());
            }
            // Some rows were refused for billing: only those go out again, as a new batch stored
            // like any other and chained to this one. A row the batch left out is not one of them:
            // the stage retries a missing item itself, as a batch of its own that is stored under
            // its own hash. The caller gets the new id, and its fetch returns the stored rows with
            // the new rows.
            let key2 = hash_items(&again);
            let mut link_to = None;
            let mut n2 = 0;
            while let Some(file) = read_json::<BatchFile>(&batch_path(&dir, &key2, n2)) {
                // A batch of these very items that was submitted and never linked or fetched (a
                // kill between its record and the link, or the stage's own batch of them) may be
                // running and billed: adopt it, never submit the items twice.
                if file.rows.is_none() && !chain.iter().any(|(k, i, _)| *k == key2 && *i == n2) {
                    link_to = Some(file.batch_id.clone());
                    break;
                }
                n2 += 1;
            }
            let batch_id = match link_to {
                Some(id) => id,
                None => {
                    let custom_ids = again.iter().map(|i| i.custom_id.clone()).collect();
                    let batch_id = self.inner.submit(again).await?;
                    let record = BatchFile {
                        key: key2.clone(),
                        n: n2,
                        batch_id: batch_id.clone(),
                        submitted_at: now(),
                        attempt: self.store.record().attempt,
                        custom_ids,
                        rows: None,
                        follow: None,
                    };
                    if let Ok(bytes) = serde_json::to_vec(&record) {
                        let _ = write_atomic(&batch_path(&dir, &key2, n2), &bytes);
                    }
                    batch_id
                }
            };
            let mut linked = tail.clone();
            linked.follow = Some((key2.clone(), n2));
            if let Ok(bytes) = serde_json::to_vec(&linked) {
                let _ = write_atomic(&batch_path(&dir, tail_key, *tail_n), &bytes);
            }
            self.remember(&batch_id, &key2, n2, (key.as_str(), n));
            return Ok(batch_id);
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
            follow: None,
        };
        if let Ok(bytes) = serde_json::to_vec(&record) {
            let _ = write_atomic(&batch_path(&dir, &key, n), &bytes);
        }
        self.remember(&batch_id, &key, n, (key.as_str(), n));
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

    async fn cancel_batch(&self, batch_id: &str) -> anyhow::Result<()> {
        self.inner.cancel_batch(batch_id).await
    }

    async fn fetch(&self, batch_id: &str, ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        if let Some(rows) = self.stored_rows(batch_id) {
            return Ok(rows.iter().map(StoredRow::replayed).collect());
        }
        let rows = self.inner.fetch(batch_id, ttl).await?;
        let found = self.slot_of(batch_id);
        let Some(slot) = found else {
            return Ok(rows);
        };
        let dir = self.store.dir().to_path_buf();
        if let Some(mut stored) = read_json::<BatchFile>(&batch_path(&dir, &slot.key, slot.n)) {
            stored.rows = Some(rows.iter().map(StoredRow::of).collect());
            if let Ok(bytes) = serde_json::to_vec(&stored) {
                let _ = write_atomic(&batch_path(&dir, &slot.key, slot.n), &bytes);
            }
        }
        if (slot.key.as_str(), slot.n) == (slot.view.0.as_str(), slot.view.1) {
            return Ok(rows);
        }
        // A batch sent for the failed rows of an earlier one: the caller submitted the whole set
        // and gets the whole set. The new rows are live (this attempt paid for them); the rows
        // answered by the store are replayed.
        let chain = batch_chain(&dir, &slot.view.0, slot.view.1);
        let Some(merged) = merged_rows(&chain) else {
            return Ok(rows);
        };
        Ok(merged
            .iter()
            .map(|m| {
                rows.iter()
                    .find(|r| r.custom_id == m.custom_id)
                    .cloned()
                    .unwrap_or_else(|| m.replayed())
            })
            .collect())
    }
}

impl JournalBatchApi {
    /// Where `batch_id` is stored. This attempt's submits are remembered; an id from an earlier
    /// process (a stage polls and fetches the batches it left running by id, without a submit) is
    /// found in the stored batch files, with the first batch of its chain as the view, so its
    /// fetch still returns the stored good rows with the new ones.
    fn slot_of(&self, batch_id: &str) -> Option<Slot> {
        if let Some(slot) = self.by_id.lock().ok()?.get(batch_id).cloned() {
            return Some(slot);
        }
        let dir = self.store.dir();
        let files: Vec<BatchFile> = std::fs::read_dir(dir.join("batches"))
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|e| read_json::<BatchFile>(&e.path()))
            .collect();
        let own = files.iter().find(|f| f.batch_id == batch_id)?;
        let mut view = (own.key.clone(), own.n);
        // Walk back to the batch the stage first submitted (a bounded walk).
        for _ in 0..1_000 {
            match files
                .iter()
                .find(|f| f.follow.as_ref() == Some(&(view.0.clone(), view.1)))
            {
                Some(before) => view = (before.key.clone(), before.n),
                None => break,
            }
        }
        let slot = Slot {
            key: own.key.clone(),
            n: own.n,
            view,
        };
        self.remember(
            batch_id,
            &slot.key,
            slot.n,
            (slot.view.0.as_str(), slot.view.1),
        );
        Some(slot)
    }

    /// The stored rows a fetch of `batch_id` answers: the batch is fetched and stored, and the
    /// rows are those of its whole chain ([`merged_rows`]).
    fn stored_rows(&self, batch_id: &str) -> Option<Vec<StoredRow>> {
        let slot = self.slot_of(batch_id)?;
        let dir = self.store.dir();
        read_json::<BatchFile>(&batch_path(dir, &slot.key, slot.n))?.rows?;
        merged_rows(&batch_chain(dir, &slot.view.0, slot.view.1))
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
/// with the failed part named as the reason. The fix and detail stages, and the clone judge, are
/// NOT set aside here: each of their replies was marked at the point of rejection
/// ([`reject_reply`]), with its reason, and a reply with an accepted entry in it stays good.
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
        // A group reviewer reply that broke its schema was stored unusable when it came back, so
        // it is asked again; the replies of the rule groups that were read are not set aside for
        // one that was not.
        if matches!(
            file.kind,
            PartKind::Fix | PartKind::Detail | PartKind::GroupReview
        ) {
            continue;
        }
        let hit = rejected.iter().find(|f| {
            // A stage that records each rejected reply itself keeps the replies that parsed, and
            // its calls are never set aside by another stage's failure. (A file written before
            // owners were recorded has none and keeps the blanket set-aside.)
            !(file.kind == PartKind::Other
                && (f.per_reply_rejections || file.owner == Some(CallOwner::CloneJudge)))
                && PartKind::of_pass_name(&f.pass) == file.kind
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
                    if entry_replays(entry, file.kind) {
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

/// What the stored calls and the stored batch rows of the run in `dir` cost in all. Every stored call counts, a reply later
/// set aside included: the money was paid either way. A spend limit on a resume counts this whole
/// figure up front ([`crate::cost_cap::RunControls::stored_spend_usd`]).
pub(crate) fn stored_spend(dir: &Path) -> f64 {
    let mut all = 0.0f64;
    if let Ok(entries) = std::fs::read_dir(dir.join("calls")) {
        for file in entries
            .filter_map(Result::ok)
            .filter_map(|e| read_json::<CallFile>(&e.path()))
        {
            for entry in file.entries.values() {
                all += entry.cost_usd;
            }
        }
    }
    // The fetched rows of the batches the run submitted live under `batches/`, not `calls/`. A
    // stage's checkpoint replays them onto the meter as resumed usage, so the stored figure has to
    // hold them too, or the limit would take off more than it ever added.
    if let Ok(entries) = std::fs::read_dir(dir.join("batches")) {
        for batch in entries
            .filter_map(Result::ok)
            .filter_map(|e| read_json::<BatchFile>(&e.path()))
        {
            for row in batch.rows.iter().flatten() {
                for paid in [row.response.as_ref(), row.billed.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    all += paid.cost_usd.unwrap_or(0.0);
                }
            }
        }
    }
    all
}

/// The model the group reviewer's stored replies in `dir` came from: the requested model of the
/// most recent good one. A stored reply is keyed by a digest that includes the model, so a resume
/// on any other model asks (and pays for) the whole stage again. `None` when no group review
/// reply is stored.
pub(crate) fn stored_group_review_model(dir: &Path) -> Option<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    for file in std::fs::read_dir(dir.join("calls"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| read_json::<CallFile>(&e.path()))
        .filter(|f| f.kind == PartKind::GroupReview)
    {
        for entry in file.entries.values() {
            if entry.status != EntryStatus::Done {
                continue;
            }
            if let Some(model) = entry
                .response
                .as_ref()
                .map(|r| r.model.trim())
                .filter(|m| !m.is_empty())
            {
                found.push((entry.at.clone(), model.to_string()));
            }
        }
    }
    found.into_iter().max().map(|(_, model)| model)
}

/// Whether every part of `kind` for `repo` in the plan is done. A call whose repo the prompt did
/// not name is matched against the parts of that kind in any repo.
fn part_finished(record: &RunRecord, kind: PartKind, repo: &str) -> bool {
    let mut parts = record
        .plan
        .iter()
        .filter(|p| p.kind == kind && (repo.is_empty() || p.repo.is_empty() || p.repo == repo))
        .peekable();
    parts.peek().is_some() && parts.all(|p| p.status == PartStatus::Done)
}

/// What a resume of `record` would cost, from the stored files of `dir`.
pub(crate) fn estimate_remaining(dir: &Path, record: &RunRecord) -> ResumeEstimate {
    let mut stored = 0usize;
    let mut again = 0usize;
    let mut spent = 0.0f64;
    let stored_usd = stored_spend(dir);
    if let Ok(entries) = std::fs::read_dir(dir.join("calls")) {
        for file in entries
            .filter_map(Result::ok)
            .filter_map(|e| read_json::<CallFile>(&e.path()))
        {
            for entry in file.entries.values() {
                if entry_replays(entry, file.kind) {
                    stored += 1;
                    spent += entry.cost_usd;
                } else if !part_finished(record, file.kind, &file.repo) {
                    // A call that came back unusable or failed is owed again only while its part
                    // is open. A finished part read its item on a later try (under another
                    // request): the entry is a superseded first try and owes nothing.
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
        stored_usd,
        basis,
    }
}

#[cfg(test)]
mod unrun_row_tests {
    use super::StoredRow;

    fn row(error: &str) -> StoredRow {
        StoredRow {
            custom_id: "acme-1".to_string(),
            response: None,
            error: Some(error.to_string()),
            billed: None,
        }
    }

    #[test]
    fn a_canceled_or_an_expired_row_is_sent_again_on_the_resume_and_nothing_else_is() {
        assert!(row("canceled").needs_resend());
        assert!(
            row("expired").needs_resend(),
            "an expired batch never ran the row"
        );
        assert!(row(" expired\n").needs_resend());
        // The stage already dealt with any other failure in its attempt.
        assert!(!row("overloaded_error: Overloaded").needs_resend());
        assert!(!row("the row expired somewhere in its text").needs_resend());
    }
}
