//! The outage stop: when the model provider cannot be reached, the run stops cleanly instead of
//! failing every remaining part one by one.
//!
//! Zach, 2026-10-28: "We can't ship a product to anyone that didn't fully run end-to-end
//! properly." Retries catch nearly everything; a true outage must stop the run, record every
//! part it did not send, and leave the run re-triggerable later.
//!
//! [`RunHealth`] is the run-wide counter. Every model call reports to it as it completes:
//!
//! * a success resets the streak;
//! * a transport failure (after the transport's own retries) adds to the streak, and the run
//!   stops at [`OUTAGE_STREAK`] in a row;
//! * an auth or billing failure stops the run at once (waiting does not fix a key or a balance);
//! * a content failure (a cut-off or malformed reply) neither counts nor resets: the provider
//!   answered, so it is not an outage, and it is not proof the provider is healthy either.
//!
//! "In a row" is completion order, and the counter is safe under concurrent passes. Once stopped,
//! admission refuses every new part ([`crate::ai_audit::UsageMeter::admit`]); calls already in
//! flight finish. Each refused part is recorded as not run with class `OutageStop`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use camerata_llm::failure::FailureClass;
use serde::{Deserialize, Serialize};

use crate::cost_cap::{NotRunPass, NotRunStage};

/// How many transport failures in a row stop the run.
pub const OUTAGE_STREAK: u32 = 3;

/// What stopped the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutageTrigger {
    /// `Transport` (a streak) or `AuthOrBilling` (one failure).
    pub class: FailureClass,
    /// A plain sentence naming what happened. Operator only.
    pub detail: String,
    /// RFC3339 UTC time the run stopped.
    pub at: String,
}

/// The run-wide health counter. See the module docs for the rules.
#[derive(Debug, Default)]
pub struct RunHealth {
    streak: AtomicU32,
    stopped: AtomicBool,
    trigger: Mutex<Option<OutageTrigger>>,
}

impl RunHealth {
    /// A model call succeeded: the provider is reachable, so the streak starts over.
    pub fn note_success(&self) {
        self.streak.store(0, Ordering::SeqCst);
    }

    /// A model call failed after its retries, with `class`. `detail` is a short note for the
    /// operator (the error text).
    pub fn note_failure(&self, class: FailureClass, detail: &str) {
        match class {
            FailureClass::Transport => {
                let streak = self.streak.fetch_add(1, Ordering::SeqCst) + 1;
                if streak >= OUTAGE_STREAK {
                    self.trip(
                        FailureClass::Transport,
                        format!(
                            "{streak} model calls in a row could not reach the provider \
                             (last: {detail})"
                        ),
                    );
                }
            }
            FailureClass::AuthOrBilling => self.trip(
                FailureClass::AuthOrBilling,
                format!("the provider refused the credentials or the account (last: {detail})"),
            ),
            // The provider answered: not an outage, and not proof of health either.
            FailureClass::Content
            | FailureClass::SpendLimit
            | FailureClass::OutageStop
            | FailureClass::Io => {}
        }
    }

    fn trip(&self, class: FailureClass, detail: String) {
        let mut trigger = self
            .trigger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if trigger.is_none() {
            *trigger = Some(OutageTrigger {
                class,
                detail: camerata_llm::failure::scrub_message(&detail, 400),
                at: crate::client_source::records::now_rfc3339(),
            });
        }
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// True once the run has stopped. New work is refused from here on.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// What stopped the run, once it has stopped.
    pub fn trigger(&self) -> Option<OutageTrigger> {
        self.trigger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The current streak, for tests and the operator log.
    pub fn streak(&self) -> u32 {
        self.streak.load(Ordering::SeqCst)
    }
}

/// What the outage stop left undone, as the operator ledger records it. Like the spend-limit
/// record it names every part that was never sent, so a later run can pick them up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutageRecord {
    pub trigger: OutageTrigger,
    pub not_run_passes: Vec<NotRunPass>,
    pub not_run_stages: Vec<NotRunStage>,
    /// Repos the run reached after it had stopped; their model work was never started.
    #[serde(default)]
    pub repos_not_finished: Vec<String>,
}

impl OutageRecord {
    /// The failed-pass entries for everything the stop left undone. Operator only: a client
    /// export is refused while any of these exist.
    pub fn failed_passes(&self) -> Vec<crate::ai_audit::FailedPass> {
        crate::cost_cap::not_run_failed_passes(
            &self.not_run_passes,
            &self.not_run_stages,
            "the run stopped because the model provider could not be reached",
        )
    }

    /// The repos the stop left unfinished: those named plus any with a part that did not run.
    pub fn unfinished_repos(&self) -> Vec<String> {
        let mut repos: std::collections::BTreeSet<String> =
            self.repos_not_finished.iter().cloned().collect();
        repos.extend(self.not_run_passes.iter().map(|p| p.repo.clone()));
        repos.extend(self.not_run_stages.iter().map(|s| s.repo.clone()));
        repos.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_transport_failures_in_a_row_stop_the_run() {
        let h = RunHealth::default();
        h.note_failure(FailureClass::Transport, "a");
        h.note_failure(FailureClass::Transport, "b");
        assert!(!h.is_stopped(), "two is not an outage");
        h.note_failure(FailureClass::Transport, "c");
        assert!(h.is_stopped());
        assert_eq!(h.trigger().unwrap().class, FailureClass::Transport);
    }

    #[test]
    fn any_success_resets_the_streak() {
        let h = RunHealth::default();
        h.note_failure(FailureClass::Transport, "a");
        h.note_failure(FailureClass::Transport, "b");
        h.note_success();
        h.note_failure(FailureClass::Transport, "c");
        h.note_failure(FailureClass::Transport, "d");
        assert!(!h.is_stopped());
        assert_eq!(h.streak(), 2);
    }

    #[test]
    fn content_failures_never_count_and_never_reset() {
        let h = RunHealth::default();
        for _ in 0..10 {
            h.note_failure(FailureClass::Content, "malformed");
        }
        assert!(!h.is_stopped(), "content failures are not an outage");
        h.note_failure(FailureClass::Transport, "a");
        h.note_failure(FailureClass::Content, "malformed");
        h.note_failure(FailureClass::Transport, "b");
        h.note_failure(FailureClass::Transport, "c");
        assert!(
            h.is_stopped(),
            "a content failure between them did not reset"
        );
    }

    #[test]
    fn an_auth_or_billing_failure_stops_the_run_at_once() {
        let h = RunHealth::default();
        h.note_failure(FailureClass::AuthOrBilling, "HTTP 401");
        assert!(h.is_stopped());
        assert_eq!(h.trigger().unwrap().class, FailureClass::AuthOrBilling);
    }

    #[test]
    fn the_first_trigger_is_kept() {
        let h = RunHealth::default();
        h.note_failure(FailureClass::AuthOrBilling, "first");
        h.note_failure(FailureClass::Transport, "x");
        h.note_failure(FailureClass::Transport, "y");
        h.note_failure(FailureClass::Transport, "z");
        assert_eq!(h.trigger().unwrap().class, FailureClass::AuthOrBilling);
    }

    #[test]
    fn the_counter_is_safe_under_concurrent_failures() {
        let h = std::sync::Arc::new(RunHealth::default());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let h = h.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        h.note_failure(FailureClass::Transport, "x");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(h.is_stopped());
        assert_eq!(h.streak(), 400, "no increment was lost");
    }
}
