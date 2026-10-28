//! Claude Code session bookkeeping for the CLI backend.
//!
//! Every `claude -p` call persists its whole conversation, prompt included, under
//! `~/.claude/projects/<cwd with "/" and "." replaced by "-">/<session id>.jsonl`. On a client
//! engagement that prompt contains client source, so "delete the client's source from this
//! machine" has to reach those files too (Zach, 2026-10-24: Camerata tags each call it makes
//! with a session id it chose, remembers the ids, and later deletes exactly those files).
//!
//! This module is the recording half: [`new_session_id`] mints the id handed to
//! `claude --session-id`, and a [`SessionSink`] attached to an [`crate::llm::Llm`] collects one
//! [`ModelSession`] per spawn. The sink records at SPAWN time, not on success, because a call
//! that times out or is killed has still written (part of) a session file.
//!
//! The deletion half lives in the server (`client_source`), which reads the recorded ids back
//! from the inspection entry.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One Claude Code session Camerata created through a CLI-backend model call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelSession {
    /// The UUID passed to `claude --session-id`; also the stem of the persisted `.jsonl` file.
    pub session_id: String,
    /// The working directory the call ran in (decides which `~/.claude/projects/<dir>` folder
    /// holds the file). The process cwd for single-shot calls, the repo for repo-read calls.
    pub cwd: String,
    /// RFC3339 UTC time the call was spawned.
    #[serde(default)]
    pub created_at: String,
}

/// A shared, cloneable collector of [`ModelSession`]s. Attach one to an `Llm` with
/// `Llm::with_session_sink`; every clone of that `Llm` records into the same list.
#[derive(Debug, Clone, Default)]
pub struct SessionSink {
    inner: Arc<Mutex<Vec<ModelSession>>>,
}

impl SessionSink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one spawned session. A poisoned lock is recovered, never dropped: losing an id
    /// would leave a client's prompt on disk after "delete".
    pub fn record(&self, session: ModelSession) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(session);
    }

    /// Every session recorded so far, oldest first. The sink keeps its contents.
    pub fn snapshot(&self) -> Vec<ModelSession> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A fresh random (version 4) UUID in the canonical 8-4-4-4-12 lowercase form, which is what
/// `claude --session-id` requires.
///
/// Built from `std`'s per-instance OS-seeded SipHash keys plus a process-wide counter and the
/// clock, so no dependency is added for one call site. Uniqueness is what matters here (a
/// collision would make two calls share one session file); the ids are not secrets.
pub fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let half = |salt: u64| -> u64 {
        // A fresh `RandomState` draws new random keys for every hasher family, so the two
        // halves are independent even for identical inputs.
        let mut h = RandomState::new().build_hasher();
        h.write_u64(salt);
        h.write_u64(n);
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        h.finish()
    };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&half(0x9e37_79b9_7f4a_7c15).to_be_bytes());
    bytes[8..].copy_from_slice(&half(0xc2b2_ae3d_27d4_eb4f).to_be_bytes());
    // RFC 4122: version 4 in the high nibble of byte 6, variant 10xx in byte 8.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The current time as RFC3339 UTC (`2026-10-24T17:40:12Z`). Hand-rolled (civil-from-days) so
/// this crate needs no date dependency.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// RFC3339 UTC for a Unix timestamp in seconds. Split out so the conversion is testable.
pub fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_a_canonical_v4_uuid() {
        let id = new_session_id();
        assert_eq!(id.len(), 36, "{id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(id
            .chars()
            .all(|c| c == '-' || c.is_ascii_digit() || ('a'..='f').contains(&c)));
        assert!(parts[2].starts_with('4'), "version nibble: {id}");
        assert!(
            matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b')),
            "variant nibble: {id}"
        );
    }

    #[test]
    fn session_ids_do_not_repeat() {
        let ids: std::collections::HashSet<String> = (0..2_000).map(|_| new_session_id()).collect();
        assert_eq!(ids.len(), 2_000);
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        // 2026-10-24T17:40:12Z
        assert_eq!(rfc3339_from_unix(1_792_863_612), "2026-10-24T17:40:12Z");
        // A leap day.
        assert_eq!(rfc3339_from_unix(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn sink_records_in_order_and_clones_share_the_list() {
        let sink = SessionSink::new();
        let other = sink.clone();
        sink.record(ModelSession {
            session_id: "a".into(),
            cwd: "/x".into(),
            created_at: String::new(),
        });
        other.record(ModelSession {
            session_id: "b".into(),
            cwd: "/y".into(),
            created_at: String::new(),
        });
        let ids: Vec<String> = sink.snapshot().into_iter().map(|s| s.session_id).collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
