//! Enforcement-catch ledger capture logic: extract catches from already-recorded
//! in-memory state at TERMINAL POINTS, then write them to the append-only SQLite
//! enforcement ledger (best-effort, fail-soft).
//!
//! Design:
//! - WRITE-ONLY: no read / query path here. The ledger is for external SQL analytics only.
//! - FAIL-SOFT: every async capture fn catches errors and logs; never propagates.
//! - PURE extraction fns (unit-tested): `extract_run_catches`, `revised_after`.
//! - CONTENT-HASH-NOT-RAW: only SHA-256 hex digests of offending content are stored.
//!   Raw content never enters the ledger.
//!
//! Three capture points:
//! 1. `capture_run_finalization` — called when a run reaches a terminal state
//!    (AwaitingQa, Failed, Cancelled). Iterates the run's GateEvents, emitting one
//!    catch per `verdict == "deny"` layer-1 event with `revised_after` computed.
//! 2. `capture_scan_findings` — called when a brownfield scan/audit completes.
//!    For each ACTIVE floor finding, emits a `floor`/`catch` record with the snippet
//!    hashed.
//! 3. `EnforcementLedger` wrapper: the optional ledger handle stored in AppState; if
//!    None (tests / ledger open failure), all capture methods are no-ops.

use std::sync::Arc;

use camerata_persistence::{content_hash, EnforcementCatch, EnforcementCatchLedger, SqliteStore};

use crate::onboard::Finding;
use crate::run::GateEvent;

// ---------------------------------------------------------------------------
// Ledger handle
// ---------------------------------------------------------------------------

/// The optional enforcement-catch ledger stored in AppState.
///
/// `None` means the ledger failed to open (or tests opted out). All `capture_*`
/// methods on `None` are no-ops — fail-soft by construction.
#[derive(Clone)]
pub struct EnforcementLedger(pub Option<Arc<SqliteStore>>);

impl EnforcementLedger {
    /// A no-op ledger (tests / fail-soft fallback).
    pub fn none() -> Self {
        Self(None)
    }

    /// Open the ledger at `path`. On any error: log and return a no-op `None` ledger
    /// so callers are always fail-soft. This is called from `AppState::from_env`.
    pub async fn open(path: &std::path::Path) -> Self {
        match SqliteStore::open_path(path).await {
            Ok(store) => {
                eprintln!("enforcement ledger opened at {}", path.display());
                Self(Some(Arc::new(store)))
            }
            Err(e) => {
                eprintln!(
                    "enforcement ledger could not open at {} ({e}); \
                     catch recording is disabled for this session",
                    path.display()
                );
                Self(None)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure extraction helpers (unit-tested, no I/O)
// ---------------------------------------------------------------------------

/// Determine whether a deny on `target` is followed by a later `allow` on the SAME
/// target within `events`. This is the `revised_after` signal: the agent received the
/// denial, revised its write, and later succeeded.
///
/// Pure: operates only on the event slice. Unit-testable without I/O.
pub fn revised_after(deny_seq: usize, target: &str, events: &[GateEvent]) -> bool {
    // A later allow on the same target = the agent revised after the deny.
    events.iter().any(|e| {
        e.seq > deny_seq && e.verdict == "allow" && e.detail.contains(target)
    })
}

/// Extract one `EnforcementCatch` per deny event from a run's gate-event slice.
///
/// For each event where `verdict == "deny"` and `layer == "layer-1"` (or `layer == "layer-2"`
/// for bounce events), produce a catch with:
/// - `layer`: `"gate"` for layer-1, `"layer2"` for layer-2
/// - `verdict`: `"deny"` (gate) or `"bounce"` (layer-2)
/// - `rule_id`: from `event.rule`
/// - `path`: extracted from `event.detail` (best-effort, `None` if not parseable)
/// - `content_hash`: from `event.content_hash` (already a SHA-256 hex from the gateway)
/// - `run_id`, `story_id`: from the supplied run metadata
/// - `revised_after`: computed via `revised_after()`
/// - `ts_ms`: current epoch-ms (best-effort; pure function receives it as a parameter
///   for testability)
///
/// Pure: no I/O. Unit-testable.
pub fn extract_run_catches(
    events: &[GateEvent],
    run_id: &str,
    story_id: &str,
    ts_ms: i64,
) -> Vec<EnforcementCatch> {
    let mut catches = Vec::new();
    for event in events {
        let (layer, verdict) = match (event.layer.as_str(), event.verdict.as_str()) {
            ("layer-1", "deny") => ("gate", "deny"),
            ("layer-2", "fail") | ("layer-2", "bounce") => ("layer2", "bounce"),
            _ => continue,
        };

        // Best-effort: extract path from detail. For layer-1 deny events the detail
        // is "Write to <path> denied. <reason>" (from gate_record_to_event) or the
        // narrative from the scripted run. We try to parse it; None is fine.
        let path = extract_path_from_detail(&event.detail);

        let revised = revised_after(event.seq, path.as_deref().unwrap_or(""), events);

        catches.push(EnforcementCatch {
            ts_ms,
            layer: layer.to_string(),
            verdict: verdict.to_string(),
            rule_id: event.rule.clone(),
            repo: None,
            path,
            line: None,
            content_hash: event.content_hash.clone(),
            run_id: Some(run_id.to_string()),
            story_id: Some(story_id.to_string()),
            revised_after: Some(revised),
        });
    }
    catches
}

/// Best-effort path extraction from a gate event detail string.
/// The live-fleet format is "Write to <path> denied. <reason>".
/// The scripted-run format varies; if we can't parse it cleanly we return None.
fn extract_path_from_detail(detail: &str) -> Option<String> {
    // Live format: "Write to <path> denied." or "Write to <path> allowed."
    let rest = detail.strip_prefix("Write to ")?;
    let end = rest.find(" denied.")
        .or_else(|| rest.find(" allowed."))
        .or_else(|| rest.find(' '))?;
    let path = &rest[..end];
    if path.is_empty() { None } else { Some(path.to_string()) }
}

// ---------------------------------------------------------------------------
// Capture point 1: run finalization
// ---------------------------------------------------------------------------

/// Capture enforcement catches at run finalization (terminal state reached).
///
/// Iterates the run's GateEvents, writes one `EnforcementCatch` per deny/bounce
/// to the ledger. Best-effort / fail-soft: errors are logged and swallowed.
/// Must be called AFTER the terminal status is set, off the hot path.
pub async fn capture_run_finalization(
    ledger: &EnforcementLedger,
    events: &[GateEvent],
    run_id: &str,
    story_id: &str,
) {
    let Some(store) = ledger.0.as_ref() else {
        return; // no ledger; no-op (fail-soft)
    };

    let ts_ms = now_ms();
    let catches = extract_run_catches(events, run_id, story_id, ts_ms);

    for catch in catches {
        if let Err(e) = store.record_catch(catch).await {
            eprintln!(
                "enforcement ledger: failed to record run catch for run={run_id}: {e}"
            );
            // fail-soft: swallow, do not propagate
        }
    }
}

// ---------------------------------------------------------------------------
// Capture point 2: scan completion (floor findings)
// ---------------------------------------------------------------------------

/// Capture enforcement catches for ACTIVE floor findings after a scan completes.
///
/// Each active (non-suppressed) finding from the deterministic floor audit becomes
/// one `floor`/`catch` record. The finding snippet is hashed (SHA-256 hex) — the
/// raw snippet is never stored.
///
/// Best-effort / fail-soft: errors are logged and swallowed.
pub async fn capture_scan_findings(
    ledger: &EnforcementLedger,
    findings: &[Finding],
) {
    let Some(store) = ledger.0.as_ref() else {
        return; // no ledger; no-op (fail-soft)
    };

    let ts_ms = now_ms();

    for finding in findings {
        // Only active (non-suppressed) floor findings.
        if finding.status != "active" {
            continue;
        }
        // Only floor (AUDIT_RULES) rule ids.
        if !crate::onboard::AUDIT_RULES.contains(&finding.rule_id.as_str()) {
            continue;
        }

        // Hash the snippet; NEVER store the raw snippet in the ledger.
        let snippet_hash = if finding.snippet.is_empty() {
            None
        } else {
            Some(content_hash(&finding.snippet))
        };

        let catch = EnforcementCatch::floor(
            ts_ms,
            finding.rule_id.clone(),
            finding.repo.clone(),
            finding.path.clone(),
            finding.line as i64,
            snippet_hash,
        );

        if let Err(e) = store.record_catch(catch).await {
            eprintln!(
                "enforcement ledger: failed to record floor catch for {}:{}: {e}",
                finding.path, finding.line
            );
            // fail-soft: swallow
        }
    }
}

// ---------------------------------------------------------------------------
// Timestamp helper
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_persistence::SqliteStore;

    // ── revised_after ────────────────────────────────────────────────────────

    #[test]
    fn revised_after_true_when_later_allow_on_same_target() {
        let events = vec![
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("GOV-1".to_string()),
                detail: "Write to src/foo.rs denied. rule fired.".to_string(),
                content_hash: None,
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to src/foo.rs allowed.".to_string(),
                content_hash: None,
            },
        ];
        assert!(revised_after(1, "src/foo.rs", &events));
    }

    #[test]
    fn revised_after_false_when_no_later_allow() {
        let events = vec![GateEvent {
            seq: 1,
            layer: "layer-1".to_string(),
            verdict: "deny".to_string(),
            rule: Some("GOV-1".to_string()),
            detail: "Write to src/foo.rs denied.".to_string(),
            content_hash: None,
        }];
        assert!(!revised_after(1, "src/foo.rs", &events));
    }

    #[test]
    fn revised_after_false_when_allow_is_for_different_target() {
        let events = vec![
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("GOV-1".to_string()),
                detail: "Write to src/foo.rs denied.".to_string(),
                content_hash: None,
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to src/bar.rs allowed.".to_string(),
                content_hash: None,
            },
        ];
        assert!(!revised_after(1, "src/foo.rs", &events));
    }

    #[test]
    fn revised_after_false_when_allow_is_before_deny() {
        // allow BEFORE the deny does not count
        let events = vec![
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to src/foo.rs allowed.".to_string(),
                content_hash: None,
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("GOV-1".to_string()),
                detail: "Write to src/foo.rs denied.".to_string(),
                content_hash: None,
            },
        ];
        // seq of deny = 2; allow has seq 1 < 2 so revised_after returns false
        assert!(!revised_after(2, "src/foo.rs", &events));
    }

    // ── extract_run_catches ───────────────────────────────────────────────────

    #[test]
    fn extract_run_catches_emits_one_catch_per_deny() {
        let events = vec![
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("SEC-NO-PATH-ESCAPE-1".to_string()),
                detail: "Write to /etc/cron.d/payload denied. path escape.".to_string(),
                content_hash: Some("abcd1234abcd1234".to_string()),
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("SEC-NO-HARDCODED-SECRETS-1".to_string()),
                detail: "Write to src/config.rs denied. secret detected.".to_string(),
                content_hash: Some("deadbeefdeadbeef".to_string()),
            },
            GateEvent {
                seq: 3,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to src/repo.rs allowed.".to_string(),
                content_hash: None,
            },
        ];

        let catches = extract_run_catches(&events, "run-1", "CAM-7", 1_700_000_000_000);
        assert_eq!(catches.len(), 2, "one catch per deny, not for allow");

        let c0 = &catches[0];
        assert_eq!(c0.layer, "gate");
        assert_eq!(c0.verdict, "deny");
        assert_eq!(c0.rule_id.as_deref(), Some("SEC-NO-PATH-ESCAPE-1"));
        assert_eq!(c0.content_hash.as_deref(), Some("abcd1234abcd1234"));
        assert_eq!(c0.run_id.as_deref(), Some("run-1"));
        assert_eq!(c0.story_id.as_deref(), Some("CAM-7"));
        assert_eq!(c0.revised_after, Some(false)); // no later allow on same target

        let c1 = &catches[1];
        assert_eq!(c1.rule_id.as_deref(), Some("SEC-NO-HARDCODED-SECRETS-1"));
        assert_eq!(c1.content_hash.as_deref(), Some("deadbeefdeadbeef"));
    }

    #[test]
    fn extract_run_catches_skips_allow_and_non_gate_events() {
        let events = vec![
            GateEvent {
                seq: 1,
                layer: "fleet".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: "Fleet started".to_string(),
                content_hash: None,
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to foo.rs allowed.".to_string(),
                content_hash: None,
            },
            GateEvent {
                seq: 3,
                layer: "delegate".to_string(),
                verdict: "dispatch".to_string(),
                rule: None,
                detail: "Delegated a subtask.".to_string(),
                content_hash: None,
            },
        ];
        let catches = extract_run_catches(&events, "run-2", "CAM-8", 0);
        assert!(catches.is_empty(), "no denies = no catches");
    }

    #[test]
    fn extract_run_catches_revised_after_true_when_later_allow() {
        let events = vec![
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("GOV-1".to_string()),
                detail: "Write to src/foo.rs denied. rule fired.".to_string(),
                content_hash: None,
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to src/foo.rs allowed.".to_string(),
                content_hash: None,
            },
        ];
        let catches = extract_run_catches(&events, "run-3", "CAM-9", 0);
        assert_eq!(catches.len(), 1);
        assert_eq!(catches[0].revised_after, Some(true));
    }

    #[test]
    fn extract_run_catches_no_content_hash_when_scripted() {
        // Scripted runs don't carry content_hash in GateEvents; it must stay None.
        let events = vec![GateEvent {
            seq: 1,
            layer: "layer-1".to_string(),
            verdict: "deny".to_string(),
            rule: Some("SEC-NO-PATH-ESCAPE-1".to_string()),
            detail: "Frontend attempted a write outside workspace. SEC-NO-PATH-ESCAPE-1".to_string(),
            content_hash: None, // scripted run has no hash
        }];
        let catches = extract_run_catches(&events, "run-scripted", "CAM-10", 0);
        assert_eq!(catches.len(), 1);
        assert!(catches[0].content_hash.is_none(), "scripted runs have no content_hash");
    }

    // ── fail-soft: failing/None ledger does not break callers ────────────────

    #[tokio::test]
    async fn capture_run_finalization_is_noop_with_none_ledger() {
        let ledger = EnforcementLedger::none();
        let events = vec![GateEvent {
            seq: 1,
            layer: "layer-1".to_string(),
            verdict: "deny".to_string(),
            rule: Some("GOV-1".to_string()),
            detail: "Write to src/foo.rs denied.".to_string(),
            content_hash: None,
        }];
        // Must not panic or error even with deny events.
        capture_run_finalization(&ledger, &events, "run-x", "CAM-x").await;
    }

    #[tokio::test]
    async fn capture_scan_findings_is_noop_with_none_ledger() {
        let ledger = EnforcementLedger::none();
        let findings = vec![crate::onboard::Finding {
            repo: "owner/repo".to_string(),
            path: "src/db.py".to_string(),
            line: 42,
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            severity: "critical".to_string(),
            snippet: "password = \"hunter2\"".to_string(),
            detail: "Hardcoded secret".to_string(),
            status: "active".to_string(),
            also_matches: vec![],
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        }];
        // Must not panic or error.
        capture_scan_findings(&ledger, &findings).await;
    }

    // ── fail-soft: actual ledger insert does not error ───────────────────────
    // (Raw DB read-back for content verification lives in camerata-persistence tests,
    //  which have direct sqlx access. Here we just verify: no panic, no propagated error.)

    #[tokio::test]
    async fn capture_run_finalization_does_not_panic_with_real_ledger() {
        let store = SqliteStore::open("sqlite::memory:").await.unwrap();
        let ledger = EnforcementLedger(Some(Arc::new(store)));

        let events = vec![
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some("SEC-NO-PATH-ESCAPE-1".to_string()),
                detail: "Write to /etc/cron.d/payload denied.".to_string(),
                content_hash: Some("abcdef0123456789".to_string()),
            },
            GateEvent {
                seq: 2,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "Write to src/repo.rs allowed.".to_string(),
                content_hash: None,
            },
        ];

        // Must complete without panic or propagated error (fail-soft contract).
        capture_run_finalization(&ledger, &events, "run-real", "CAM-real").await;
        // If we get here the insert ran without propagating an error.
    }

    #[tokio::test]
    async fn capture_scan_findings_does_not_panic_with_real_ledger() {
        let store = SqliteStore::open("sqlite::memory:").await.unwrap();
        let ledger = EnforcementLedger(Some(Arc::new(store)));

        let findings = vec![
            // Active floor finding → should be recorded (no panic).
            crate::onboard::Finding {
                repo: "owner/repo".to_string(),
                path: "src/db.py".to_string(),
                line: 42,
                rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
                severity: "critical".to_string(),
                snippet: "password = \"hunter2\"".to_string(),
                detail: "Hardcoded secret".to_string(),
                status: "active".to_string(),
                also_matches: vec![],
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
            },
            // Suppressed finding → skipped silently (no panic).
            crate::onboard::Finding {
                repo: "owner/repo".to_string(),
                path: "src/legacy.py".to_string(),
                line: 7,
                rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
                severity: "critical".to_string(),
                snippet: "api_key = \"old\"".to_string(),
                detail: "Hardcoded secret".to_string(),
                status: "suppressed-baseline".to_string(),
                also_matches: vec![],
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
            },
            // Non-floor rule → skipped silently (no panic).
            crate::onboard::Finding {
                repo: "owner/repo".to_string(),
                path: "src/some.rs".to_string(),
                line: 1,
                rule_id: "ARCH-NO-GOD-STRUCT-1".to_string(),
                severity: "medium".to_string(),
                snippet: "struct God { everything: () }".to_string(),
                detail: "God struct".to_string(),
                status: "active".to_string(),
                also_matches: vec![],
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
            },
        ];

        // Must complete without panic or propagated error (fail-soft contract).
        capture_scan_findings(&ledger, &findings).await;
    }

    // ── extract_path_from_detail ──────────────────────────────────────────────

    #[test]
    fn path_extracted_from_live_deny_detail() {
        let detail = "Write to src/config.rs denied. secret detected.";
        assert_eq!(
            super::extract_path_from_detail(detail),
            Some("src/config.rs".to_string())
        );
    }

    #[test]
    fn path_extracted_from_live_allow_detail() {
        let detail = "Write to src/repo.rs allowed.";
        assert_eq!(
            super::extract_path_from_detail(detail),
            Some("src/repo.rs".to_string())
        );
    }

    #[test]
    fn path_returns_none_for_non_write_detail() {
        let detail = "Frontend attempted a write that climbs out of the workspace. SEC-NO-PATH-ESCAPE-1";
        // Does not start with "Write to ", so path is None.
        assert_eq!(super::extract_path_from_detail(detail), None);
    }
}
