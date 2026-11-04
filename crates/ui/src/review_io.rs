//! Where the review screens read from and write to (review UI spec section 1, rule 1).
//!
//! [`crate::review_source::ReviewSource`] is the read side. This file adds:
//!
//! - [`HttpReviewSource`]: the real source, reqwest against the BFF base URL, serving every read
//!   and every write the tree lane makes;
//! - [`ReviewWrites`]: the write side (a decision, a revoke, an edit), a trait so a host can be
//!   driven by a fake in a test and the gallery;
//! - [`RecordingWrites`]: that fake. It records what a host posted and answers with a made-up
//!   record, so a test can assert on the request;
//! - [`ReviewApi`]: the pair a host holds;
//! - the two reads a host makes of a whole node: [`read_all_sites`] (for a batch plan) and
//!   [`fetch_page`] (one page, for opening a node).
//!
//! A view never calls any of this; hosts do (the Rust port of "a view never fetches").

use std::cell::RefCell;
use std::rc::Rc;

use camerata_api_types::decisions::{
    Decision, DecisionError, DecisionRecorded, DecisionState, NewDecisionReq, RevokeResp,
};
use camerata_api_types::edits::{
    AddFindingReq, ClientAckRecorded, ClientAckReq, Edit, EditDiscarded, EditError, EditRecorded,
    FindingAdded, NewEditReq,
};
use camerata_api_types::review_view::{
    EngineCheckLane, EscalationBudgetOutcome, EscalationBudgetReq, ExportGateView, QueueView,
    ReviewSettingsView, ReviewShellView, ReviewViewError, SampleSizeReq, SampleSizeResp, SiteRow,
    SitesPage, TreeView, SITES_PAGE_SIZE,
};
use camerata_ui_core::tree::state::SitesRequest;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::review_source::{ReviewSource, ReviewSourceError, SourceFuture, SourceWindow};

// ── The write side ────────────────────────────────────────────────────────────────────────

/// Everything the review lanes write: the tree lane's decisions and edits, and the settings
/// lane's four writes (none of which calls a model).
pub trait ReviewWrites {
    /// `POST /api/projects/:id/decisions`: a site decision or a node batch.
    fn post_decision(&self, req: NewDecisionReq) -> SourceFuture<'_, DecisionRecorded>;
    /// `POST /api/projects/:id/decisions/:id/revoke`: the undo bar's button.
    fn revoke_decision<'a>(&'a self, decision_id: &'a str) -> SourceFuture<'a, RevokeResp>;
    /// `POST /api/projects/:id/edits`: a field of a site, or a node's summary.
    fn post_edit(&self, req: NewEditReq) -> SourceFuture<'_, EditRecorded>;

    // ── The site detail's writes (spec section 8). A merge is `post_decision` with a
    // `MergedInto` state and a split is `post_edit` on the sub-group field, so neither needs a
    // method of its own. The defaults refuse, so a write side that predates them still compiles.

    /// `POST /api/projects/:id/edits/:id/discard`: the undo bar's "Undo edit", and a stale edit's
    /// Discard.
    fn discard_edit<'a>(&'a self, _edit_id: &'a str) -> SourceFuture<'a, EditDiscarded> {
        unsupported("discarding an edit")
    }
    /// `POST /api/projects/:id/edits/:id/reapply`: a stale edit's Reapply, as a new edit with the
    /// site's current fingerprint.
    fn reapply_edit<'a>(&'a self, _edit_id: &'a str) -> SourceFuture<'a, EditRecorded> {
        unsupported("reapplying an edit")
    }
    /// `POST /api/projects/:id/added-findings`: a finding the engine missed. The server runs the
    /// evidence check and answers with its verdict.
    fn add_finding(&self, _req: AddFindingReq) -> SourceFuture<'_, FindingAdded> {
        unsupported("adding a finding")
    }
    /// `POST /api/projects/:id/decisions/:id/client-ack`: the client accepted an Ignored row.
    fn client_ack<'a>(
        &'a self,
        _decision_id: &'a str,
        _req: ClientAckReq,
    ) -> SourceFuture<'a, ClientAckRecorded> {
        unsupported("recording the client's acknowledgement")
    }

    // ── The settings lane's writes (spec section 9). ──

    /// `POST escalation-budget/preview`: the line a proposed rate and floor would give. Writes
    /// nothing.
    fn preview_budget(&self, req: EscalationBudgetReq)
        -> SourceFuture<'_, EscalationBudgetOutcome>;
    /// `POST escalation-budget`: save the rate, floor and over-budget reasons. The server marks
    /// the line again from the stored candidates (design A4).
    fn apply_budget(&self, req: EscalationBudgetReq) -> SourceFuture<'_, EscalationBudgetOutcome>;
    /// `POST sample-size`.
    fn set_sample_size(&self, sample_size_per_run: usize) -> SourceFuture<'_, SampleSizeResp>;
    /// `POST step-models { step, model }` for a stage (`audit`, `calibration`, `group_review`,
    /// `fix`).
    fn set_stage_model<'a>(&'a self, step: &'a str, model: &'a str) -> SourceFuture<'a, ()>;
}

/// A write this side does not make.
fn unsupported<'a, T: 'a>(what: &str) -> SourceFuture<'a, T> {
    let message = format!("This version of the app cannot do {what} here.");
    Box::pin(std::future::ready(Err(ReviewSourceError { message })))
}

/// The project the review screens are about, provided beside [`ReviewApi`] by the shell host. The
/// code viewer reads files through the project's own checkout, so the tree's `O` names it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewProject(pub String);

/// The reads and writes a host holds. Cloning shares the same source.
#[derive(Clone)]
pub struct ReviewApi {
    pub reads: Rc<dyn ReviewSource>,
    pub writes: Rc<dyn ReviewWrites>,
}

impl ReviewApi {
    /// The real thing: one [`HttpReviewSource`] for both sides.
    pub fn http(base: String, project_id: String) -> Self {
        let http = Rc::new(HttpReviewSource::new(base, project_id));
        Self {
            reads: http.clone(),
            writes: http,
        }
    }
}

impl PartialEq for ReviewApi {
    /// Two handles are equal when they share the same source, so a context value does not
    /// re-render its readers on every parent render.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(Rc::as_ptr(&self.reads), Rc::as_ptr(&other.reads))
    }
}

// ── The HTTP source ───────────────────────────────────────────────────────────────────────

/// The review routes of one project, over HTTP.
pub struct HttpReviewSource {
    base: String,
    project_id: String,
    /// The inspection the screens are about; blank means the latest.
    run_id: String,
    client: reqwest::Client,
}

impl HttpReviewSource {
    /// `base` is the BFF base URL (`crate::bff_base()`); the host passes it in, so this file has
    /// no hidden global.
    pub fn new(base: String, project_id: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            project_id,
            run_id: String::new(),
            client: reqwest::Client::new(),
        }
    }

    /// The same source pinned to one inspection.
    #[must_use]
    pub fn for_run(mut self, run_id: &str) -> Self {
        self.run_id = run_id.to_string();
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/projects/{}/{path}", self.base, self.project_id)
    }

    fn run_query(&self) -> Vec<(&'static str, String)> {
        if self.run_id.is_empty() {
            Vec::new()
        } else {
            vec![("run_id", self.run_id.clone())]
        }
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Vec<(&'static str, String)>,
    ) -> Result<T, ReviewSourceError> {
        let resp = self
            .client
            .get(self.url(path))
            .query(&query)
            .send()
            .await
            .map_err(|e| unreachable_error(&self.base, &e))?;
        read_json(resp).await
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ReviewSourceError> {
        let resp = self
            .client
            .post(self.url(path))
            .json(body)
            .send()
            .await
            .map_err(|e| unreachable_error(&self.base, &e))?;
        read_json(resp).await
    }
}

fn unreachable_error(base: &str, e: &reqwest::Error) -> ReviewSourceError {
    ReviewSourceError {
        message: format!(
            "The review service did not answer at {base}. Check that it is running. ({e})"
        ),
    }
}

/// Read a reply: the body as `T` on success; on a refusal the server's own reason (the review,
/// decision and edit routes all answer `{ ok: false, code, message }`).
async fn read_json<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, ReviewSourceError> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| ReviewSourceError {
        message: format!("The reply could not be read: {e}"),
    })?;
    if status.is_success() {
        return serde_json::from_slice(&bytes).map_err(|e| ReviewSourceError {
            message: format!("The reply was not what this version of the app expects: {e}"),
        });
    }
    Err(ReviewSourceError {
        message: refusal_message(status.as_u16(), &bytes),
    })
}

/// The words for a refused request.
pub fn refusal_message(status: u16, body: &[u8]) -> String {
    if let Ok(e) = serde_json::from_slice::<ReviewViewError>(body) {
        return e.message;
    }
    if let Ok(e) = serde_json::from_slice::<DecisionError>(body) {
        return e.message;
    }
    if let Ok(e) = serde_json::from_slice::<EditError>(body) {
        return e.message;
    }
    format!("The request failed with status {status}.")
}

impl HttpReviewSource {
    /// `GET /api/onboard/finding-context`: the source around `line`, as inspected. Not a
    /// project-scoped route, so the project rides along as a query parameter.
    async fn finding_context(
        &self,
        repo: &str,
        path: &str,
        line: usize,
    ) -> Result<SourceWindow, ReviewSourceError> {
        let resp = self
            .client
            .get(format!("{}/api/onboard/finding-context", self.base))
            .query(&[
                ("repo", repo.to_string()),
                ("path", path.to_string()),
                ("line", line.to_string()),
                ("project", self.project_id.clone()),
            ])
            .send()
            .await
            .map_err(|e| unreachable_error(&self.base, &e))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| ReviewSourceError {
            message: format!("The reply could not be read: {e}"),
        })?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ReviewSourceError {
                message: refusal_message(status.as_u16(), &bytes),
            })?;
        window_of(&value)
    }
}

/// The window a `finding-context` reply holds, or its own one-line reason.
fn window_of(reply: &serde_json::Value) -> Result<SourceWindow, ReviewSourceError> {
    if reply.get("status").and_then(serde_json::Value::as_str) == Some("ok") {
        let start_line = reply
            .get("start_line")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok());
        let lines: Option<Vec<String>> = reply
            .get("lines")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|l| l.as_str().unwrap_or_default().to_string())
                    .collect()
            });
        if let (Some(start_line), Some(lines)) = (start_line, lines) {
            return Ok(SourceWindow { start_line, lines });
        }
    }
    let message = reply
        .get("reason")
        .or_else(|| reply.get("message"))
        .and_then(serde_json::Value::as_str)
        .filter(|m| !m.is_empty())
        .unwrap_or("The code at that line could not be read.");
    Err(ReviewSourceError {
        message: message.to_string(),
    })
}

impl ReviewSource for HttpReviewSource {
    fn source_window<'a>(
        &'a self,
        repo: &'a str,
        path: &'a str,
        line: usize,
    ) -> SourceFuture<'a, SourceWindow> {
        Box::pin(self.finding_context(repo, path, line))
    }

    fn shell(&self) -> SourceFuture<'_, ReviewShellView> {
        Box::pin(self.get_json("review", self.run_query()))
    }

    fn tree(&self) -> SourceFuture<'_, TreeView> {
        Box::pin(self.get_json("review/tree", self.run_query()))
    }

    fn sites_page<'a>(
        &'a self,
        node: &'a str,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> SourceFuture<'a, SitesPage> {
        let mut query = self.run_query();
        query.push(("node", node.to_string()));
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        query.push(("limit", limit.to_string()));
        Box::pin(self.get_json("review/sites", query))
    }

    fn queue(
        &self,
        show_below: bool,
        below_offset: usize,
        below_limit: usize,
    ) -> SourceFuture<'_, QueueView> {
        let mut query = self.run_query();
        query.push(("show_below", show_below.to_string()));
        query.push(("below_offset", below_offset.to_string()));
        query.push(("below_limit", below_limit.to_string()));
        Box::pin(self.get_json("review/queue", query))
    }

    fn engine_check(&self) -> SourceFuture<'_, EngineCheckLane> {
        Box::pin(self.get_json("review/engine-check", self.run_query()))
    }

    fn export_gate(&self) -> SourceFuture<'_, ExportGateView> {
        Box::pin(self.get_json("review/export-gate", self.run_query()))
    }

    fn settings(&self) -> SourceFuture<'_, ReviewSettingsView> {
        Box::pin(self.get_json("review/settings", Vec::new()))
    }
}

impl ReviewWrites for HttpReviewSource {
    fn post_decision(&self, req: NewDecisionReq) -> SourceFuture<'_, DecisionRecorded> {
        Box::pin(async move { self.post_json("decisions", &req).await })
    }

    fn revoke_decision<'a>(&'a self, decision_id: &'a str) -> SourceFuture<'a, RevokeResp> {
        Box::pin(async move {
            let body = camerata_api_types::decisions::RevokeReq::default();
            self.post_json(&format!("decisions/{decision_id}/revoke"), &body)
                .await
        })
    }

    fn post_edit(&self, req: NewEditReq) -> SourceFuture<'_, EditRecorded> {
        Box::pin(async move { self.post_json("edits", &req).await })
    }

    fn discard_edit<'a>(&'a self, edit_id: &'a str) -> SourceFuture<'a, EditDiscarded> {
        Box::pin(async move {
            let body = camerata_api_types::edits::DiscardReq::default();
            self.post_json(&format!("edits/{edit_id}/discard"), &body)
                .await
        })
    }

    fn reapply_edit<'a>(&'a self, edit_id: &'a str) -> SourceFuture<'a, EditRecorded> {
        Box::pin(async move {
            let body = camerata_api_types::edits::ReapplyReq::default();
            self.post_json(&format!("edits/{edit_id}/reapply"), &body)
                .await
        })
    }

    fn add_finding(&self, req: AddFindingReq) -> SourceFuture<'_, FindingAdded> {
        Box::pin(async move { self.post_json("added-findings", &req).await })
    }

    fn client_ack<'a>(
        &'a self,
        decision_id: &'a str,
        req: ClientAckReq,
    ) -> SourceFuture<'a, ClientAckRecorded> {
        Box::pin(async move {
            self.post_json(&format!("decisions/{decision_id}/client-ack"), &req)
                .await
        })
    }

    fn preview_budget(
        &self,
        req: EscalationBudgetReq,
    ) -> SourceFuture<'_, EscalationBudgetOutcome> {
        Box::pin(async move { self.post_json("escalation-budget/preview", &req).await })
    }

    fn apply_budget(&self, req: EscalationBudgetReq) -> SourceFuture<'_, EscalationBudgetOutcome> {
        Box::pin(async move { self.post_json("escalation-budget", &req).await })
    }

    fn set_sample_size(&self, sample_size_per_run: usize) -> SourceFuture<'_, SampleSizeResp> {
        Box::pin(async move {
            self.post_json(
                "sample-size",
                &SampleSizeReq {
                    sample_size_per_run,
                },
            )
            .await
        })
    }

    fn set_stage_model<'a>(&'a self, step: &'a str, model: &'a str) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            // The step-models route answers 200 with `{ ok: false, message }` for a refusal
            // (an unknown step, a blank model), so the body decides, not the status.
            let reply: serde_json::Value = self
                .post_json(
                    "step-models",
                    &serde_json::json!({ "step": step, "model": model }),
                )
                .await?;
            step_models_outcome(&reply)
        })
    }
}

/// The step-models route's reply as a result: `Ok` when `ok` is true, else its own message.
pub(crate) fn step_models_outcome(reply: &serde_json::Value) -> Result<(), ReviewSourceError> {
    if reply.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(ReviewSourceError {
        message: reply
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or("The model was not saved.")
            .to_string(),
    })
}

// ── The fake write side ───────────────────────────────────────────────────────────────────

/// What a [`RecordingWrites`] was asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Written {
    Decision(NewDecisionReq),
    Revoke(String),
    Edit(NewEditReq),
    /// A budget preview (writes nothing on the server).
    PreviewBudget(EscalationBudgetReq),
    ApplyBudget(EscalationBudgetReq),
    SampleSize(usize),
    /// A stage's model, as (step key, model id).
    StageModel(String, String),
    /// The edit with this id was discarded.
    DiscardEdit(String),
    /// The stale edit with this id was reapplied.
    ReapplyEdit(String),
    /// A finding was added.
    AddFinding(AddFindingReq),
    /// The decision with this id was acknowledged by the client.
    ClientAck(String, ClientAckReq),
}

/// A write side that records what it is given and answers with a made-up record. For tests and
/// the gallery; it never reaches a server.
#[derive(Default)]
pub struct RecordingWrites {
    pub log: RefCell<Vec<Written>>,
    /// When set, every write is refused with this message.
    pub refuse: RefCell<Option<String>>,
}

impl RecordingWrites {
    fn refused(&self) -> Option<ReviewSourceError> {
        self.refuse
            .borrow()
            .as_ref()
            .map(|m| ReviewSourceError { message: m.clone() })
    }

    fn next_id(&self) -> String {
        format!("dec-{:06}", self.log.borrow().len() + 1)
    }
}

impl ReviewWrites for RecordingWrites {
    fn post_decision(&self, req: NewDecisionReq) -> SourceFuture<'_, DecisionRecorded> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                let id = self.next_id();
                let decision = Decision {
                    id,
                    key: req.key.clone(),
                    state: req.state.clone(),
                    reason: req.reason.clone(),
                    scope: req.scope.clone(),
                    applies_to: Vec::new(),
                    except: req.except.clone(),
                    inherited_from: None,
                    decided_by: "reviewer".to_string(),
                    decided_at: "2026-11-04T09:00:00Z".to_string(),
                    run_id: req.run_id.clone(),
                    engine_version: "0.0.0-test".to_string(),
                    needed_me: req.needed_me,
                    sample_verdict: None,
                    confirmed_by_client: None,
                    revoked_at: None,
                };
                self.log.borrow_mut().push(Written::Decision(req));
                Ok(DecisionRecorded {
                    decision,
                    inherited_count: 0,
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn revoke_decision<'a>(&'a self, decision_id: &'a str) -> SourceFuture<'a, RevokeResp> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                self.log
                    .borrow_mut()
                    .push(Written::Revoke(decision_id.to_string()));
                Ok(RevokeResp {
                    revoked: vec![decision_id.to_string()],
                    revoked_at: "2026-11-04T09:00:05Z".to_string(),
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn post_edit(&self, req: NewEditReq) -> SourceFuture<'_, EditRecorded> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                let edit = Edit {
                    id: format!("edit-{:06}", self.log.borrow().len() + 1),
                    key: req.key.clone(),
                    field: req.field,
                    old: String::new(),
                    new: req.new.clone(),
                    reason: req.reason.clone(),
                    by: "reviewer".to_string(),
                    at: "2026-11-04T09:00:00Z".to_string(),
                    run_id: req.run_id.clone(),
                    fingerprint_at_edit: String::new(),
                    stale: false,
                    discarded_at: None,
                    reapplied_from: None,
                };
                self.log.borrow_mut().push(Written::Edit(req));
                Ok(EditRecorded { edit })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn discard_edit<'a>(&'a self, edit_id: &'a str) -> SourceFuture<'a, EditDiscarded> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                self.log
                    .borrow_mut()
                    .push(Written::DiscardEdit(edit_id.to_string()));
                Ok(EditDiscarded {
                    edit: Edit {
                        discarded_at: Some("2026-11-04T09:00:05Z".to_string()),
                        ..a_made_up_edit(edit_id)
                    },
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn reapply_edit<'a>(&'a self, edit_id: &'a str) -> SourceFuture<'a, EditRecorded> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                let new_id = format!("edit-{:06}", self.log.borrow().len() + 1);
                self.log
                    .borrow_mut()
                    .push(Written::ReapplyEdit(edit_id.to_string()));
                Ok(EditRecorded {
                    edit: Edit {
                        reapplied_from: Some(edit_id.to_string()),
                        ..a_made_up_edit(&new_id)
                    },
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn add_finding(&self, req: AddFindingReq) -> SourceFuture<'_, FindingAdded> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                let key = format!("added-{:06}", self.log.borrow().len() + 1);
                self.log.borrow_mut().push(Written::AddFinding(req));
                Ok(FindingAdded {
                    key,
                    evidence_verified: true,
                    evidence_note: String::new(),
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn client_ack<'a>(
        &'a self,
        decision_id: &'a str,
        req: ClientAckReq,
    ) -> SourceFuture<'a, ClientAckRecorded> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                let decision = Decision {
                    id: decision_id.to_string(),
                    key: "k".to_string(),
                    state: DecisionState::Ignored,
                    reason: String::new(),
                    scope: camerata_api_types::decisions::Scope::Site,
                    applies_to: Vec::new(),
                    except: Vec::new(),
                    inherited_from: None,
                    decided_by: "reviewer".to_string(),
                    decided_at: "2026-11-04T09:00:00Z".to_string(),
                    run_id: String::new(),
                    engine_version: "0.0.0-test".to_string(),
                    needed_me: None,
                    sample_verdict: None,
                    confirmed_by_client: Some(camerata_api_types::decisions::ClientAck {
                        by: req.by.clone(),
                        at: "2026-11-05T10:00:00Z".to_string(),
                        note: req.note.clone(),
                    }),
                    revoked_at: None,
                };
                self.log
                    .borrow_mut()
                    .push(Written::ClientAck(decision_id.to_string(), req));
                Ok(ClientAckRecorded {
                    decision,
                    also_acknowledged: 0,
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn preview_budget(
        &self,
        req: EscalationBudgetReq,
    ) -> SourceFuture<'_, EscalationBudgetOutcome> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                self.log.borrow_mut().push(Written::PreviewBudget(req));
                Ok(unchanged_outcome(false))
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn apply_budget(&self, req: EscalationBudgetReq) -> SourceFuture<'_, EscalationBudgetOutcome> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                self.log.borrow_mut().push(Written::ApplyBudget(req));
                Ok(unchanged_outcome(true))
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn set_sample_size(&self, sample_size_per_run: usize) -> SourceFuture<'_, SampleSizeResp> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                self.log
                    .borrow_mut()
                    .push(Written::SampleSize(sample_size_per_run));
                Ok(SampleSizeResp {
                    sample_size_per_run,
                    lane_off: sample_size_per_run == 0,
                })
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn set_stage_model<'a>(&'a self, step: &'a str, model: &'a str) -> SourceFuture<'a, ()> {
        let result = match self.refused() {
            Some(e) => Err(e),
            None => {
                self.log
                    .borrow_mut()
                    .push(Written::StageModel(step.to_string(), model.to_string()));
                Ok(())
            }
        };
        Box::pin(std::future::ready(result))
    }
}

/// An edit the recording side answers with: nothing about it is real but its id.
fn a_made_up_edit(id: &str) -> Edit {
    Edit {
        id: id.to_string(),
        key: "k".to_string(),
        field: camerata_api_types::edits::EditField::Headline,
        old: String::new(),
        new: String::new(),
        reason: String::new(),
        by: "reviewer".to_string(),
        at: "2026-11-04T09:00:00Z".to_string(),
        run_id: String::new(),
        fingerprint_at_edit: String::new(),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

/// A recorded budget write's answer: the line did not move.
fn unchanged_outcome(applied: bool) -> EscalationBudgetOutcome {
    let line = camerata_api_types::review::EscalationBudget::default();
    EscalationBudgetOutcome {
        applied,
        before: line,
        after: line,
        moved_above: 0,
        moved_below: 0,
    }
}

// ── Reads of a node ───────────────────────────────────────────────────────────────────────

/// One page of one node, as a [`SitesRequest`] asks for it.
pub async fn fetch_page(
    source: &dyn ReviewSource,
    req: &SitesRequest,
) -> Result<SitesPage, ReviewSourceError> {
    source
        .sites_page(&req.node, req.cursor.as_deref(), req.limit)
        .await
}

/// Every site of `node`, read a page at a time ([`SITES_PAGE_SIZE`] each), for a batch plan. The
/// rows are used to work out the plan and dropped; the tree does not keep them. `on_page` is told
/// how many sites have been read, so the prompt can say so.
pub async fn read_all_sites(
    source: &dyn ReviewSource,
    node: &str,
    mut on_page: impl FnMut(usize, usize),
) -> Result<Vec<SiteRow>, ReviewSourceError> {
    let mut rows: Vec<SiteRow> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = source
            .sites_page(node, cursor.as_deref(), SITES_PAGE_SIZE)
            .await?;
        rows.extend(page.rows);
        on_page(rows.len(), page.total);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(rows),
        }
    }
}

/// The decision state a record carries, as a label for a toast.
pub fn recorded_label(recorded: &DecisionRecorded) -> &'static str {
    match recorded.decision.state {
        DecisionState::Confirmed => "confirmed",
        DecisionState::Ignored => "ignored",
        DecisionState::FalsePositive => "marked false positive",
        DecisionState::TechDebtNow | DecisionState::TechDebtLater => "saved as tech debt",
        DecisionState::MergedInto(_) => "merged",
        DecisionState::EngineRestored => "put back",
    }
}

#[cfg(test)]
mod tests;
