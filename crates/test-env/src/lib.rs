//! `camerata-test-env` — the one sanctioned way for a test to mutate process-global
//! environment variables.
//!
//! # The problem this closes
//!
//! `cargo test` runs the tests of one test binary on parallel threads of ONE process, and
//! the process environment is shared, mutable, global state. A test that calls
//! `std::env::set_var` / `remove_var` therefore races with:
//!
//! 1. any other test that mutates the same variable, and
//! 2. any other test whose code under test READS that variable (it can observe the first
//!    test's mutation mid-flight).
//!
//! Before this crate the workspace guarded that with a patchwork: several different
//! `serial_test` keys (`bff_env`, `corpus_env`, `camerata_live_build`, ...) that each
//! protected one variable but not each other's readers, bespoke `Mutex`es, and many tests
//! with no guard at all. A panicking test also skipped its trailing `remove_var`, leaking
//! state into whichever test ran next.
//!
//! # The mechanism (both halves are required)
//!
//! 1. [`EnvGuard`] sets (or removes) a variable and RESTORES the previous value on `Drop`,
//!    so an unwinding assertion failure cannot leak state.
//! 2. One shared `serial_test` key, `process_env`, serializes every test that mutates the
//!    environment AND every test whose code under test reads a mutated variable:
//!
//! ```ignore
//! #[test]
//! #[serial_test::serial(process_env)]
//! fn reads_the_api_key() {
//!     let _key = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
//!     // ... exercise code that reads ANTHROPIC_API_KEY ...
//! }   // prior value restored here, even on panic
//! ```
//!
//! The key is global to a test binary (a process), which is exactly the scope of the race.
//! Never invent a second key for a variable: two keys do not exclude each other, so two
//! tests under different keys can still race on the same variable.
//!
//! # Enforcement
//!
//! * `clippy.toml` disallows `std::env::set_var` / `remove_var` workspace-wide, so a raw
//!   call fails `cargo clippy -D warnings`. This crate is the one place that allows them.
//! * `tests/env_discipline.rs` fails if a test uses [`EnvGuard`] without the
//!   `process_env` key, or if any `serial_test` key other than `process_env` exists.
//!
//! What no tool can check is the READER half: whether a test that merely exercises code
//! reading a mutated variable carries the key. That remains a review obligation; grep the
//! production code for `env::var("<NAME>")` and map callers to tests.

use std::ffi::{OsStr, OsString};

/// The shared `serial_test` key every env-mutating or env-reading test must carry.
///
/// `#[serial_test::serial(...)]` needs a literal, so tests spell `process_env` out; this
/// constant exists so the discipline test and docs name the key in one place.
pub const SERIAL_KEY: &str = "process_env";

/// Sets or removes one environment variable for the lifetime of the guard, then restores
/// whatever was there before (including "unset").
///
/// Hold it in a binding that lives for as long as the override must (`let _env = ...`, never
/// `let _ = ...`, which would drop it immediately). Multiple guards on the same key nest
/// correctly: they restore in reverse order of creation.
///
/// Constructing a guard does NOT serialize anything by itself; the test must also carry
/// `#[serial_test::serial(process_env)]`.
#[must_use = "the variable is restored as soon as the guard is dropped; bind it with `let _env = ...`"]
pub struct EnvGuard {
    key: OsString,
    previous: Option<OsString>,
}

// The ONLY place in the workspace allowed to touch the raw process-env mutators; see
// `clippy.toml`. `unsafe_code` is forbidden workspace-wide and the crates are edition 2021,
// where these are safe fns.
#[allow(clippy::disallowed_methods)]
impl EnvGuard {
    /// Set `key` to `value`, remembering the prior value.
    pub fn set(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref().to_os_string();
        let previous = std::env::var_os(&key);
        std::env::set_var(&key, value);
        Self { key, previous }
    }

    /// Remove `key`, remembering the prior value.
    pub fn remove(key: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref().to_os_string();
        let previous = std::env::var_os(&key);
        std::env::remove_var(&key);
        Self { key, previous }
    }
}

#[allow(clippy::disallowed_methods)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(&self.key, value),
            None => std::env::remove_var(&self.key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test uses a variable name private to it, so these tests cannot race with each
    // other or with anything else in this binary.

    #[test]
    fn set_overrides_then_restores_unset() {
        const K: &str = "CAMERATA_TEST_ENV_SELF_SET_UNSET";
        assert!(std::env::var_os(K).is_none());
        {
            let _env = EnvGuard::set(K, "v");
            assert_eq!(std::env::var(K).as_deref(), Ok("v"));
        }
        assert!(std::env::var_os(K).is_none(), "unset before => unset after");
    }

    #[test]
    fn set_restores_the_prior_value() {
        const K: &str = "CAMERATA_TEST_ENV_SELF_SET_PRIOR";
        let _outer = EnvGuard::set(K, "outer");
        {
            let _inner = EnvGuard::set(K, "inner");
            assert_eq!(std::env::var(K).as_deref(), Ok("inner"));
        }
        assert_eq!(std::env::var(K).as_deref(), Ok("outer"));
    }

    #[test]
    fn remove_unsets_then_restores_the_prior_value() {
        const K: &str = "CAMERATA_TEST_ENV_SELF_REMOVE";
        let _outer = EnvGuard::set(K, "kept");
        {
            let _gone = EnvGuard::remove(K);
            assert!(std::env::var_os(K).is_none());
        }
        assert_eq!(std::env::var(K).as_deref(), Ok("kept"));
    }

    #[test]
    fn remove_of_an_unset_var_leaves_it_unset() {
        const K: &str = "CAMERATA_TEST_ENV_SELF_REMOVE_UNSET";
        drop(EnvGuard::remove(K));
        assert!(std::env::var_os(K).is_none());
    }

    #[test]
    fn guards_nest_and_restore_in_reverse_order() {
        const K: &str = "CAMERATA_TEST_ENV_SELF_NEST";
        {
            let _a = EnvGuard::set(K, "a");
            let _b = EnvGuard::set(K, "b");
            let _c = EnvGuard::remove(K);
            assert!(std::env::var_os(K).is_none());
        }
        assert!(std::env::var_os(K).is_none());
    }

    #[test]
    fn a_panicking_test_still_restores_the_variable() {
        const K: &str = "CAMERATA_TEST_ENV_SELF_PANIC";
        let result = std::panic::catch_unwind(|| {
            let _env = EnvGuard::set(K, "leaked?");
            panic!("simulated assertion failure");
        });
        assert!(result.is_err());
        assert!(
            std::env::var_os(K).is_none(),
            "the unwinding guard must restore the variable"
        );
    }
}
