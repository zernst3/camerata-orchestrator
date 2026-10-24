//! In-flight bookkeeping for the operations that must not overlap on one repository: source
//! deletion, re-fetch, the delta rescan, and any scan that is reading the checkout.
//!
//! Two kinds of claim, both released by dropping a guard (RAII, so an early return or a panic
//! cannot leave a repo marked busy forever):
//!
//! - an EXCLUSIVE claim on one `(project, repo)` ([`SourceOps::try_begin`]), taken by delete,
//!   re-fetch and rescan-changes; a second claim on the same repo fails;
//! - a SHARED scan claim on a project ([`SourceOps::begin_scan`]), taken by a scan handler that
//!   runs in the request (the asynchronous job path is tracked by `JobStore` instead); any number
//!   may be held at once, and while one is, [`SourceOps::scan_running`] is true.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    /// `"<project>\u{1f}<repo lowercase>"` of every repo with an exclusive operation running.
    exclusive: HashSet<String>,
    /// project -> number of scans running in a request.
    scans: HashMap<String, usize>,
}

/// Shared, cloneable registry of in-flight source operations.
#[derive(Clone, Default)]
pub struct SourceOps {
    inner: Arc<Mutex<Inner>>,
}

fn key(project_id: &str, repo: &str) -> String {
    format!("{project_id}\u{1f}{}", repo.trim().to_ascii_lowercase())
}

impl SourceOps {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Claim `(project, repo)` exclusively; `None` when an operation on it is already running.
    pub fn try_begin(&self, project_id: &str, repo: &str) -> Option<OpGuard> {
        let k = key(project_id, repo);
        if !self.lock().exclusive.insert(k.clone()) {
            return None;
        }
        Some(OpGuard {
            ops: self.clone(),
            key: k,
        })
    }

    /// Whether an exclusive operation is running on `(project, repo)`.
    pub fn is_busy(&self, project_id: &str, repo: &str) -> bool {
        self.lock().exclusive.contains(&key(project_id, repo))
    }

    /// Register a scan running inside a request for `project_id`.
    pub fn begin_scan(&self, project_id: &str) -> ScanGuard {
        *self.lock().scans.entry(project_id.to_string()).or_insert(0) += 1;
        ScanGuard {
            ops: self.clone(),
            project_id: project_id.to_string(),
        }
    }

    /// Whether a request-scoped scan of `project_id` is running.
    pub fn scan_running(&self, project_id: &str) -> bool {
        self.lock().scans.get(project_id).is_some_and(|n| *n > 0)
    }
}

/// Releases an exclusive claim on drop.
pub struct OpGuard {
    ops: SourceOps,
    key: String,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        self.ops.lock().exclusive.remove(&self.key);
    }
}

/// Releases a scan claim on drop.
pub struct ScanGuard {
    ops: SourceOps,
    project_id: String,
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        let mut inner = self.ops.lock();
        if let Some(n) = inner.scans.get_mut(&self.project_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                inner.scans.remove(&self.project_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_exclusive_claim_on_the_same_repo_fails_until_the_first_drops() {
        let ops = SourceOps::new();
        let first = ops.try_begin("p", "Acme/App").expect("free");
        assert!(ops.is_busy("p", "acme/app"), "case-insensitive");
        assert!(ops.try_begin("p", "acme/app").is_none());
        // Another repo, and another project, are independent.
        assert!(ops.try_begin("p", "acme/lib").is_some());
        assert!(ops.try_begin("q", "acme/app").is_some());
        drop(first);
        assert!(!ops.is_busy("p", "acme/app"));
        assert!(ops.try_begin("p", "acme/app").is_some());
    }

    #[test]
    fn scan_claims_are_shared_and_counted() {
        let ops = SourceOps::new();
        assert!(!ops.scan_running("p"));
        let a = ops.begin_scan("p");
        let b = ops.begin_scan("p");
        assert!(ops.scan_running("p"));
        assert!(!ops.scan_running("q"));
        drop(a);
        assert!(ops.scan_running("p"), "one scan is still running");
        drop(b);
        assert!(!ops.scan_running("p"));
    }

    #[test]
    fn a_panic_while_holding_a_claim_still_releases_it() {
        let ops = SourceOps::new();
        let cloned = ops.clone();
        let result = std::panic::catch_unwind(move || {
            let _guard = cloned.try_begin("p", "a/b").expect("free");
            panic!("boom");
        });
        assert!(result.is_err());
        assert!(!ops.is_busy("p", "a/b"));
    }
}
