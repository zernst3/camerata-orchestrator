//! Per-provider RPM rate limiter.
//!
//! Bursty model work routed through a rate-limited provider (OpenRouter free tier = ~20
//! req/min) would otherwise generate 429s. This module provides a shared, cloneable
//! [`ProviderRateLimiter`] that self-throttles: callers `await`
//! [`ProviderRateLimiter::acquire`] before sending an HTTP request, and the future
//! resolves only when a slot is free.
//!
//! # Design
//!
//! One token-bucket per provider string. Each bucket is a [`tokio::sync::Semaphore`]
//! pre-loaded with `capacity` tokens (= the per-minute cap). Every `acquire` consumes
//! one token via `sem.acquire().forget()`. A background refill task — spawned on the
//! first `acquire` call (so construction is safe outside a tokio runtime) — adds one
//! token back every `60s / cap` seconds, keeping throughput at or below the cap.
//!
//! Unlimited providers (not listed in the RPM map) skip the semaphore entirely and
//! return immediately from `acquire`.
//!
//! # Default RPM config
//!
//!   - `"openrouter"` → 20 RPM
//!   - all other providers → unlimited (immediate return)
//!
//! Extend by passing a custom `[(provider, rpm)]` slice to
//! [`ProviderRateLimiter::with_limits`].

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;

/// Default per-provider request-per-minute caps. Providers absent from this list are
/// unlimited.
pub const DEFAULT_RPM: &[(&str, u32)] = &[("openrouter", 20)];

/// Internal state for one provider's token bucket.
struct Bucket {
    sem: Arc<Semaphore>,
    capacity: u32,
    /// Interval between refill ticks (`60s / capacity`).
    interval: Duration,
    /// Ensures the background refill task is only spawned once, on the first `acquire`.
    spawned: OnceLock<()>,
}

impl Bucket {
    fn new(capacity: u32) -> Self {
        let interval = Duration::from_secs(60) / capacity;
        Self {
            sem: Arc::new(Semaphore::new(capacity as usize)),
            capacity,
            interval,
            spawned: OnceLock::new(),
        }
    }

    /// Ensure the background refill task is running. Safe to call repeatedly; spawns
    /// exactly once (guarded by `OnceLock`). Must be called from within a tokio runtime
    /// (i.e. from an async context), which is always the case since `acquire` is async.
    fn ensure_refill_spawned(&self) {
        self.spawned.get_or_init(|| {
            let sem = self.sem.clone();
            let interval = self.interval;
            let capacity = self.capacity;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    // Only add if below the cap so the bucket never overflows.
                    if sem.available_permits() < capacity as usize {
                        sem.add_permits(1);
                    }
                }
            });
        });
    }

    async fn acquire(&self) {
        self.ensure_refill_spawned();
        // Consume one token. `forget()` means the permit is NOT returned on drop —
        // the refill task, not `Drop`, controls when the token comes back. This
        // implements the token-bucket semantics (fixed-rate refill) rather than the
        // leaky-bucket semantics (token returns immediately when the call finishes).
        let permit = self
            .sem
            .acquire()
            .await
            .expect("rate-limit semaphore closed unexpectedly");
        permit.forget();
    }
}

/// A shared, cheaply-cloneable per-provider rate limiter.
///
/// Clone it freely — all clones share the same underlying buckets.
///
/// Construction is safe outside a tokio runtime (no tasks are spawned until the first
/// `acquire` call).
#[derive(Clone, Debug)]
pub struct ProviderRateLimiter {
    /// Keyed by lowercase provider string. Providers absent from this map are unlimited.
    buckets: Arc<HashMap<String, Arc<Bucket>>>,
}

impl std::fmt::Debug for Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bucket")
            .field("capacity", &self.capacity)
            .field("interval", &self.interval)
            .field("available", &self.sem.available_permits())
            .finish()
    }
}

impl ProviderRateLimiter {
    /// Build a limiter from the [`DEFAULT_RPM`] table (20 RPM for "openrouter",
    /// unlimited for all other providers).
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_RPM)
    }

    /// Build a limiter from a custom `[(provider, rpm)]` slice. `rpm = 0` is treated
    /// as unlimited (no entry in the bucket map). Provider names are normalised to
    /// lowercase.
    pub fn with_limits(limits: &[(&str, u32)]) -> Self {
        let mut buckets: HashMap<String, Arc<Bucket>> = HashMap::new();
        for &(provider, rpm) in limits {
            if rpm == 0 {
                continue; // 0 = unlimited — omit from map
            }
            buckets.insert(provider.to_ascii_lowercase(), Arc::new(Bucket::new(rpm)));
        }
        Self {
            buckets: Arc::new(buckets),
        }
    }

    /// Acquire one slot for `provider`. Returns immediately when the provider is
    /// unlimited (no entry in the RPM map). When the provider is rate-limited, suspends
    /// the calling task until a refill token is available.
    ///
    /// The background refill task for the provider's bucket is lazily spawned on the
    /// first call, so this method must be called from a tokio async context (it always
    /// is in practice — callers are HTTP handlers or completer impls).
    pub async fn acquire(&self, provider: &str) {
        let key = provider.to_ascii_lowercase();
        if let Some(bucket) = self.buckets.get(&key) {
            bucket.acquire().await;
        }
        // No entry → unlimited → return immediately.
    }
}

impl Default for ProviderRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // ── unlimited provider ────────────────────────────────────────────────────────

    /// An unlisted provider must return immediately (no blocking, no panic).
    #[tokio::test]
    async fn unlimited_provider_returns_immediately() {
        let limiter = ProviderRateLimiter::new();
        let t = Instant::now();
        limiter.acquire("anthropic").await;
        limiter.acquire("claude").await;
        limiter.acquire("unknown-provider").await;
        // All three should complete in under 50 ms (well below any timeout).
        assert!(
            t.elapsed() < Duration::from_millis(50),
            "unlimited providers must not block"
        );
    }

    // ── keying / case-insensitivity ───────────────────────────────────────────────

    /// Provider keys are normalised to lowercase, so "OpenRouter" and "openrouter" hit
    /// the same bucket.
    #[tokio::test]
    async fn provider_keying_is_case_insensitive() {
        // 2-RPM bucket so 2 calls are instant and the 3rd would block (we never issue it).
        let limiter = ProviderRateLimiter::with_limits(&[("openrouter", 2)]);
        let t = Instant::now();
        limiter.acquire("OpenRouter").await;
        limiter.acquire("OPENROUTER").await;
        // Both drained the same 2-token bucket; both must complete without waiting.
        assert!(
            t.elapsed() < Duration::from_millis(50),
            "both case variants must hit the same 2-token bucket and both return instantly"
        );
    }

    // ── cap enforcement + wait behaviour ─────────────────────────────────────────

    /// A rate-limited provider with cap=2 admits the first 2 requests immediately.
    /// The 3rd must park (bucket exhausted). We verify this by racing the 3rd acquire
    /// against a 30 ms timeout: if the acquire completes before the timeout the bucket
    /// wasn't exhausted, which is a failure.
    #[tokio::test]
    async fn rate_limited_provider_parks_after_cap_exhausted() {
        // 2-token bucket (interval = 30 s — way longer than the test timeout).
        let limiter = ProviderRateLimiter::with_limits(&[("testprovider", 2)]);

        // First two must complete immediately.
        let t = Instant::now();
        limiter.acquire("testprovider").await;
        limiter.acquire("testprovider").await;
        assert!(
            t.elapsed() < Duration::from_millis(50),
            "first two (within cap) must return immediately"
        );

        // Third acquire should block — race it against a 30 ms deadline.
        let limiter2 = limiter.clone();
        let handle = tokio::spawn(async move {
            limiter2.acquire("testprovider").await;
        });
        let result = tokio::time::timeout(Duration::from_millis(30), handle).await;
        // Expect timeout (Err), meaning the acquire correctly parked.
        assert!(
            result.is_err(),
            "3rd acquire must park (bucket exhausted) — it must NOT return within 30 ms"
        );
    }

    // ── with_limits: rpm=0 treated as unlimited ───────────────────────────────────

    /// `rpm = 0` in a custom limit map is treated as unlimited (no bucket entry).
    #[tokio::test]
    async fn zero_rpm_is_unlimited() {
        let limiter = ProviderRateLimiter::with_limits(&[("myprovider", 0)]);
        let t = Instant::now();
        for _ in 0..5 {
            limiter.acquire("myprovider").await;
        }
        assert!(
            t.elapsed() < Duration::from_millis(50),
            "rpm=0 must be treated as unlimited"
        );
    }

    // ── construction outside a tokio runtime ─────────────────────────────────────

    /// `ProviderRateLimiter::new()` must not panic when called from a sync context (no
    /// tokio runtime active). Background tasks are only spawned on the first `acquire`.
    #[test]
    fn construction_is_safe_outside_tokio_runtime() {
        // This test runs in a plain sync thread (no #[tokio::test]), so there is no
        // runtime. Construction must succeed without panicking.
        let _limiter = ProviderRateLimiter::new();
        let _limiter2 = ProviderRateLimiter::with_limits(&[("openrouter", 5)]);
    }
}
