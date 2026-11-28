//! TEST SUPPORT: a bookkeeping defect a test can switch on to stand in for "an older build that
//! lost a row", so a rerun on a "fixed build" (#588) can be shown to heal a saved run. Production
//! never sets it; the whole module is compiled only for tests and the `test-support` feature.

use std::sync::atomic::{AtomicBool, Ordering};

static LOSE_A_FINDER_ROW: AtomicBool = AtomicBool::new(false);

/// While alive, the convention accounting reports one finder row as lost, which the pipeline
/// integrity check turns into a failed part. A test that holds one must run serially (it is
/// process-wide).
#[must_use = "the defect ends when the guard is dropped"]
pub struct LostRowGuard(());

impl LostRowGuard {
    pub fn set() -> Self {
        LOSE_A_FINDER_ROW.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for LostRowGuard {
    fn drop(&mut self) {
        LOSE_A_FINDER_ROW.store(false, Ordering::SeqCst);
    }
}

static REJECT_FIX_REPLIES: AtomicBool = AtomicBool::new(false);

/// While alive, the fix stage's own checks turn down every fix text, as a build whose checks are
/// stricter than the one that made the run would. Process-wide, like [`LostRowGuard`].
#[must_use = "the defect ends when the guard is dropped"]
pub struct RejectFixGuard(());

impl RejectFixGuard {
    pub fn set() -> Self {
        REJECT_FIX_REPLIES.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for RejectFixGuard {
    fn drop(&mut self) {
        REJECT_FIX_REPLIES.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn rejecting_fix_replies() -> bool {
    REJECT_FIX_REPLIES.load(Ordering::SeqCst)
}

/// The rows the convention accounting found lost, plus the injected one when the defect is on.
pub(crate) fn lost_finder_rows(
    mut lost: Vec<(String, String, usize)>,
) -> Vec<(String, String, usize)> {
    if LOSE_A_FINDER_ROW.load(Ordering::SeqCst) {
        lost.push((
            "ACME-OPTION-RULE-1".to_string(),
            "src/acme.rs".to_string(),
            1,
        ));
    }
    lost
}
