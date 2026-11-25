//! TEST SUPPORT: a scripted model for the app's scan job.
//!
//! The command line's pipeline takes a scripted model through its `Seams`; the app's scan job
//! ([`crate::start_audit_job`]) builds its model client from the credential store deep inside a
//! spawned task, out of a test's reach. A test that must run the app path with the SAME
//! scripted replies as the command line installs one here, and the job uses it in place of the
//! real client. Everything else about the job is unchanged: the run store still journals every
//! reply, and every stage still asks the model through the one seam.
//!
//! Compiled only for tests and the `test-support` feature (enabled from dev-dependencies), so a
//! production build does not contain it and the job always builds the real client there.

use std::sync::{Arc, Mutex, PoisonError};

use crate::llm::LlmPort;

/// The installed scripted model, when a test set one. Process-wide: a test that installs one
/// must run serially.
static SCAN_JOB_LLM: Mutex<Option<Arc<dyn LlmPort>>> = Mutex::new(None);

/// While alive, every scan job started in this process asks `llm` instead of the real client.
#[must_use = "the scripted model is removed when the guard is dropped"]
pub struct ScanJobLlmGuard(());

impl ScanJobLlmGuard {
    /// Install `llm` for the scan jobs started while the guard lives.
    pub fn install(llm: Arc<dyn LlmPort>) -> Self {
        *SCAN_JOB_LLM.lock().unwrap_or_else(PoisonError::into_inner) = Some(llm);
        Self(())
    }
}

impl Drop for ScanJobLlmGuard {
    fn drop(&mut self) {
        *SCAN_JOB_LLM.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The scripted model a test installed, if any.
pub(crate) fn installed() -> Option<Arc<dyn LlmPort>> {
    SCAN_JOB_LLM
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}
