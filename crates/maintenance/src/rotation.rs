//! Key and secret rotation schedule.
//!
//! [`KeyRotation`] tracks one credential's rotation schedule: when it was last
//! rotated and how often it should be rotated. [`KeyRotation::is_due`] checks
//! whether it is time to rotate against a caller-supplied timestamp (the pure
//! logic never calls the wall clock). [`due_rotations`] is the convenience
//! function that filters a slice to the ones that need attention.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The rotation schedule for a single key, secret, or API credential.
///
/// `interval_days` is the intended rotation cadence. Once `last_rotated +
/// interval_days` has passed (relative to `now`), the key is considered due.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRotation {
    /// A stable identifier for this credential (e.g. "stripe-api-key",
    /// "db-password"). Never a value, always a label.
    pub key_id: String,
    /// When this credential was last rotated. Caller-supplied (never computed
    /// inside this crate so tests can use fixed timestamps).
    pub last_rotated: DateTime<Utc>,
    /// How many days between scheduled rotations.
    pub interval_days: i64,
}

impl KeyRotation {
    /// Construct a rotation schedule entry.
    pub fn new(key_id: impl Into<String>, last_rotated: DateTime<Utc>, interval_days: i64) -> Self {
        Self {
            key_id: key_id.into(),
            last_rotated,
            interval_days,
        }
    }

    /// Returns `true` if this key is due for rotation as of `now`.
    ///
    /// A key is due when `last_rotated + interval_days <= now`. A zero or
    /// negative interval is treated as always due.
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        if self.interval_days <= 0 {
            return true;
        }
        let elapsed = now.signed_duration_since(self.last_rotated);
        elapsed.num_days() >= self.interval_days
    }
}

/// Filter `keys` to those that are due for rotation as of `now`.
///
/// Returns references into the input slice; does not allocate new `KeyRotation`
/// values.
pub fn due_rotations(keys: &[KeyRotation], now: DateTime<Utc>) -> Vec<&KeyRotation> {
    keys.iter().filter(|k| k.is_due(now)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn jan_1() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn mar_1() -> DateTime<Utc> {
        // 59 days after Jan 1 (January has 31 days, February has 28 in 2026).
        Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn is_due_true_when_interval_elapsed() {
        // Last rotated Jan 1, interval 30 days, check at Mar 1 (59 days later).
        let k = KeyRotation::new("api-key", jan_1(), 30);
        assert!(k.is_due(mar_1()));
    }

    #[test]
    fn is_due_false_when_interval_not_elapsed() {
        // Last rotated Jan 1, interval 90 days, check at Mar 1 (59 days later).
        let k = KeyRotation::new("api-key", jan_1(), 90);
        assert!(!k.is_due(mar_1()));
    }

    #[test]
    fn is_due_true_on_exact_boundary() {
        // Last rotated Jan 1, interval 59 days, check at Mar 1 (exactly 59 days).
        let k = KeyRotation::new("api-key", jan_1(), 59);
        assert!(k.is_due(mar_1()));
    }

    #[test]
    fn is_due_true_for_zero_interval() {
        let k = KeyRotation::new("api-key", jan_1(), 0);
        assert!(k.is_due(jan_1()));
    }

    #[test]
    fn is_due_true_for_negative_interval() {
        let k = KeyRotation::new("api-key", jan_1(), -1);
        assert!(k.is_due(jan_1()));
    }

    #[test]
    fn due_rotations_returns_only_due_keys() {
        let keys = vec![
            KeyRotation::new("stripe-key", jan_1(), 30), // due (59 >= 30)
            KeyRotation::new("db-password", jan_1(), 90), // not due (59 < 90)
            KeyRotation::new("jwt-secret", jan_1(), 59), // due (exactly 59)
        ];
        let due = due_rotations(&keys, mar_1());
        assert_eq!(due.len(), 2);
        assert!(due.iter().any(|k| k.key_id == "stripe-key"));
        assert!(due.iter().any(|k| k.key_id == "jwt-secret"));
        assert!(!due.iter().any(|k| k.key_id == "db-password"));
    }

    #[test]
    fn due_rotations_empty_when_none_due() {
        let keys = vec![
            KeyRotation::new("k1", jan_1(), 90),
            KeyRotation::new("k2", jan_1(), 120),
        ];
        let due = due_rotations(&keys, mar_1());
        assert!(due.is_empty());
    }

    #[test]
    fn due_rotations_empty_slice_returns_empty() {
        let due = due_rotations(&[], mar_1());
        assert!(due.is_empty());
    }

    #[test]
    fn key_rotation_round_trip_json() {
        let k = KeyRotation::new("my-api-key", jan_1(), 90);
        let json = serde_json::to_string(&k).unwrap();
        let back: KeyRotation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, k);
    }
}
