//! The async clarify-bridge: the Tier-1 flow that lets a real Product Owner
//! participate from their own board (Jira / Azure DevOps / GitHub / native).
//!
//! This is the piece that turns the `WorkItemProvider` adapters into a usable
//! flow. When the lead engineer needs clarification, the bridge posts the PRODUCT
//! clarifying questions as a comment onto the PO's work item (outbound), then polls
//! for the PO's answer comment (inbound). It is provider-agnostic: it drives
//! whichever one `WorkItemProvider` it is handed, exactly the same way.
//!
//! Privilege boundary (from `docs/WORKTRACKER_INTEGRATION.md` section 0.5): the PO
//! can ANSWER and sign off through the tracker; they can NEVER trigger execution.
//! The bridge only carries questions out and answers back. The architect reviews
//! the ingested answer locally and runs the governed agents. That is why Tier 1
//! needs no central OAuth, no multi-tenant database, and no hosted compute.

use crate::{ExternalRef, InboundKind, WorkItemProvider};

/// A clarifying question posted to the PO's board and awaiting an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingClarification {
    /// The provider comment id the questions were posted under.
    pub comment_id: String,
    /// The questions that were posted, in order.
    pub questions: Vec<String>,
    /// The cursor to begin polling for the answer from (so the bridge does not
    /// re-ingest items older than the question).
    pub since_cursor: Option<String>,
}

/// A Product Owner's answer ingested from their board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoAnswer {
    /// The plain-text body of the PO's answer comment.
    pub body: String,
    /// Which work item it came from.
    pub reference: ExternalRef,
    /// When the provider recorded it.
    pub occurred_at: String,
}

/// Drives the async clarify-bridge over any [`WorkItemProvider`].
pub struct ClarifyBridge<'a> {
    provider: &'a dyn WorkItemProvider,
    reference: ExternalRef,
}

impl<'a> ClarifyBridge<'a> {
    /// Construct a bridge bound to one provider + work item.
    pub fn new(provider: &'a dyn WorkItemProvider, reference: ExternalRef) -> Self {
        Self {
            provider,
            reference,
        }
    }

    /// Post the lead engineer's clarifying questions onto the PO's work item, and
    /// capture a cursor baseline so the subsequent poll only sees NEW activity.
    ///
    /// `baseline_cursor` is the cursor the caller already holds for this item (the
    /// high-water mark of what it has already ingested); pass `None` on the first
    /// use. It is stored on the returned [`PendingClarification`] so [`poll_answer`]
    /// starts from the right place.
    pub async fn ask(
        &self,
        questions: &[String],
        baseline_cursor: Option<String>,
    ) -> anyhow::Result<PendingClarification> {
        let comment_id = self
            .provider
            .post_clarifying_questions(&self.reference, questions)
            .await?;
        Ok(PendingClarification {
            comment_id,
            questions: questions.to_vec(),
            since_cursor: baseline_cursor,
        })
    }

    /// Poll the provider ONCE for new PO answer comments on this item since
    /// `cursor`. Returns the answers found (the inbound `Commented` events for THIS
    /// work item) plus the advanced cursor. Echo events (the bridge's own writes
    /// bouncing back) are filtered out.
    pub async fn poll_answers(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<PoAnswer>, String)> {
        let (events, next) = self.provider.poll(cursor).await?;
        let answers = events
            .into_iter()
            .filter(|e| !e.is_echo)
            .filter(|e| e.kind == InboundKind::Commented)
            .filter(|e| e.reference.external_id == self.reference.external_id)
            .filter_map(|e| {
                e.body.map(|body| PoAnswer {
                    body,
                    reference: e.reference,
                    occurred_at: e.occurred_at,
                })
            })
            .collect();
        Ok((answers, next))
    }

    /// Post the questions, then poll up to `max_attempts` times for the first PO
    /// answer. Returns the answer once it arrives, or `None` if none arrived within
    /// the attempt cap (the caller then keeps the [`PendingClarification`] open and
    /// polls again later; a real deployment spaces the polls by minutes).
    ///
    /// This is a convenience for tests and simple flows; production drives `ask`
    /// once and `poll_answers` on its own reconciliation cadence.
    pub async fn ask_and_await(
        &self,
        questions: &[String],
        baseline_cursor: Option<String>,
        max_attempts: usize,
    ) -> anyhow::Result<Option<PoAnswer>> {
        let pending = self.ask(questions, baseline_cursor).await?;
        let mut cursor = pending.since_cursor;
        for _ in 0..max_attempts.max(1) {
            let (answers, next) = self.poll_answers(cursor.as_deref()).await?;
            if let Some(first) = answers.into_iter().next() {
                return Ok(Some(first));
            }
            cursor = Some(next);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExternalRef, NativeProvider, Provider};

    fn reference() -> ExternalRef {
        ExternalRef {
            provider: Provider::Native,
            external_id: "STORY-1".to_string(),
            container: None,
            url: "native://STORY-1".to_string(),
            revision: None,
        }
    }

    #[tokio::test]
    async fn ask_posts_questions_and_records_the_comment() {
        let provider = NativeProvider::new();
        let bridge = ClarifyBridge::new(&provider, reference());
        let pending = bridge
            .ask(&["Which currency?".to_string()], None)
            .await
            .unwrap();
        assert_eq!(pending.questions, vec!["Which currency?".to_string()]);
        assert!(!pending.comment_id.is_empty());
        // The provider recorded the posted questions.
        assert!(provider
            .posted_questions()
            .iter()
            .any(|(_, qs)| qs.iter().any(|q| q.contains("Which currency?"))));
    }

    #[tokio::test]
    async fn poll_returns_the_po_answer_as_a_commented_event() {
        let provider = NativeProvider::new();
        let bridge = ClarifyBridge::new(&provider, reference());
        bridge
            .ask(&["Which currency?".to_string()], None)
            .await
            .unwrap();
        // The PO replies on their board.
        provider.inject_answer(reference(), "USD please");
        let (answers, _next) = bridge.poll_answers(None).await.unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].body, "USD please");
        assert_eq!(answers[0].reference.external_id, "STORY-1");
    }

    #[tokio::test]
    async fn ask_and_await_round_trips_post_then_answer() {
        let provider = NativeProvider::new();
        let bridge = ClarifyBridge::new(&provider, reference());
        // Pre-stage the PO's answer so the poll finds it.
        provider.inject_answer(reference(), "monthly budget");
        let answer = bridge
            .ask_and_await(&["Monthly or weekly?".to_string()], None, 3)
            .await
            .unwrap();
        assert!(answer.is_some());
        assert_eq!(answer.unwrap().body, "monthly budget");
    }

    #[tokio::test]
    async fn ask_and_await_returns_none_when_po_has_not_answered() {
        let provider = NativeProvider::new();
        let bridge = ClarifyBridge::new(&provider, reference());
        // No inject_answer: the PO has not replied yet.
        let answer = bridge
            .ask_and_await(&["Anything?".to_string()], None, 2)
            .await
            .unwrap();
        assert!(answer.is_none());
    }

    #[tokio::test]
    async fn poll_ignores_answers_for_a_different_work_item() {
        let provider = NativeProvider::new();
        let bridge = ClarifyBridge::new(&provider, reference());
        // An answer on a DIFFERENT story must not be picked up for this one.
        let other = ExternalRef {
            external_id: "STORY-99".to_string(),
            ..reference()
        };
        provider.inject_answer(other.clone(), "not for this story");
        let (answers, _next) = bridge.poll_answers(None).await.unwrap();
        assert!(answers.is_empty());
    }
}
