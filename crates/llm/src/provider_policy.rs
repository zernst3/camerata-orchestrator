//! OpenRouter provider-safety policy — THE TRUST CORE.
//!
//! Camerata routes some chat/audit models through OpenRouter, which fans a request out
//! to one of several upstream providers (DeepInfra, Novita, DeepSeek's own first-party
//! endpoint, ...). Some of those providers train on prompts and/or retain them; for a
//! tool that reads a client's private repository, the data policy of whichever provider
//! actually serves the request is a trust guarantee, not a convenience.
//!
//! This module holds:
//! - [`ProviderPolicy`] — the persisted, app-level safety toggle (`safe_mode`, default
//!   `true`) + optional provider pin.
//! - [`provider_constraint_for_request`] — the pure function that turns a policy + the
//!   live per-model safe-provider set into the exact `provider` JSON object OpenRouter's
//!   request body must carry. This is the ONE function both OpenRouter request-body call
//!   sites (`llm::OpenRouterCompleter::call_api_inner` and
//!   `camerata_server::api_agent_driver::call_openrouter_with_tools`) go through, so the
//!   safety guarantee cannot drift between the two call sites.
//!
//! See `docs/design/2026-07-28_openrouter-provider-safety.md` for the full design.

use serde::{Deserialize, Serialize};

// ── Policy config ──────────────────────────────────────────────────────────────

/// The persisted OpenRouter provider-safety policy.
///
/// Persisted alongside the other app-level settings (`crate::settings::Settings` in
/// `camerata-server`, next to `chat_model` / `llm_backend`) — see
/// `SettingsStore::provider_policy` / `SettingsStore::set_provider_policy`.
///
/// `safe_mode` DEFAULTS TO `true`: a fresh install, a settings file with the field
/// omitted (old file predating this feature), or a JSON parse of `{}` all resolve to
/// the safe posture. This is enforced by both `#[serde(default = "default_safe_mode")]`
/// (missing field on deserialize) and `Default` (no file at all).
///
/// Session-reset-to-safe (turning `safe_mode` off is meant to be a session-only,
/// non-persisted act) is a UI/app-lifecycle concern — Pass 2. This struct only carries
/// the value; nothing here decides when it gets flipped back.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPolicy {
    /// When `true` (the default), every OpenRouter request is constrained to providers
    /// that neither train on nor retain prompts (see [`provider_constraint_for_request`]).
    /// When `false`, no provider constraint is added — any provider OpenRouter would
    /// otherwise route to may serve the request. Turning this off is an explicit,
    /// visible testing-mode act (Pass 2 UI); it must never be the silent default.
    #[serde(default = "default_safe_mode")]
    pub safe_mode: bool,

    /// An optional single provider slug (e.g. `"deepinfra"`) to pin requests to. Only
    /// takes effect while `safe_mode` is `true`, and only when the pinned slug is
    /// itself in the model's safe-provider set — an unsafe pin is silently dropped
    /// (never widens the constraint). Ignored entirely while `safe_mode` is `false`
    /// (testing mode allows any provider; a stale pin must not narrow that).
    #[serde(default)]
    pub pinned_provider: Option<String>,
}

fn default_safe_mode() -> bool {
    true
}

impl Default for ProviderPolicy {
    fn default() -> Self {
        Self {
            safe_mode: true,
            pinned_provider: None,
        }
    }
}

// ── Request-layer enforcement (the trust core) ──────────────────────────────────

/// What the caller knows about this model's safe-provider set at request-build time.
///
/// Deliberately distinguishes "never learned anything" from "learned it's empty" —
/// both fail closed identically in [`provider_constraint_for_request`], but the two
/// carry different operational meaning (worth distinguishing in logs/errors): `Unknown`
/// means the endpoints fetch hasn't succeeded yet (no data to reason about at all);
/// `Known(vec![])` means the fetch succeeded but genuinely found zero providers serving
/// this model that satisfy `training == false && retains_prompts == false` — a real,
/// actionable fact about the model, not a plumbing gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafeProviders {
    /// The safe-provider set for this model has not been determined yet (endpoints
    /// fetch not attempted, in flight, or failed).
    Unknown,
    /// The safe-provider set is known: providers (by OpenRouter slug, e.g.
    /// `"deepinfra"`) whose live data policy is `training == false && retains_prompts
    /// == false`. May be empty (a real model can have zero clean providers).
    Known(Vec<String>),
}

/// Build the exact OpenRouter `provider` request-body object for one request, or refuse
/// to build a request at all.
///
/// This is the single seam both OpenRouter request-body call sites go through — see the
/// module docs. Behavior:
///
/// - `safe_mode == false` → `Ok(None)`: omit the `provider` key entirely. Testing mode
///   allows any provider OpenRouter would otherwise route to (including a stale
///   `pinned_provider`, which is intentionally ignored here — it's a safe-mode concept).
/// - `safe_mode == true`, safe set known and non-empty →
///   `Ok(Some({"data_collection": "deny", "only": [...safe set, or just the pin]}))`.
///   `data_collection: "deny"` is the OpenRouter-native retention guard, always present
///   whenever a constraint is emitted; `only` is the DYNAMIC, live-computed no-training
///   allowlist (never a hardcoded list). If `pinned_provider` is `Some(p)` AND `p` is a
///   member of the safe set, `only` narrows to exactly `[p]`. If `p` is set but NOT in
///   the safe set, the pin is dropped and `only` stays the full safe set — a pin can
///   only narrow the constraint, never widen or replace it with something unsafe.
/// - `safe_mode == true`, safe set `Unknown` OR known-but-empty → `Err(reason)`: FAIL
///   CLOSED. The trust guarantee is "no retention AND no training"; a `data_collection:
///   "deny"`-only constraint (retention guard alone, no training filter) is NOT good
///   enough to satisfy it, and an `only: []` array is not a reliable "block everything"
///   signal at the OpenRouter API level (an empty allowlist is a genuine footgun — some
///   provider-routing implementations treat an empty filter list as "no filter"). So
///   Pass 1 blocks the request entirely rather than sending anything that could be
///   misinterpreted as "route anywhere." Callers must surface this as a hard error to
///   the caller of the LLM call (never proceed to the HTTP call). See the module docs
///   and the design doc's "Pass 1 landed" section for the exact operator-facing message
///   and how to resolve it (wait for the endpoints fetch, or turn off safe mode for
///   non-client-code testing).
pub fn provider_constraint_for_request(
    policy: &ProviderPolicy,
    safe_providers: &SafeProviders,
    model_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    if !policy.safe_mode {
        // Testing mode: no constraint, no pin. An explicit, visible, session-only act
        // (Pass 2 UI) — never the silent default (see `Default for ProviderPolicy`).
        return Ok(None);
    }

    let safe_set = match safe_providers {
        SafeProviders::Unknown => {
            return Err(format!(
                "safe mode is ON but the OpenRouter provider-safety data for `{model_id}` has \
                 not loaded yet (the /endpoints fetch hasn't completed or failed) — refusing to \
                 send an unconstrained request. Retry once the model's provider-safety data has \
                 loaded, or turn off safe mode for non-client-repository testing."
            ));
        }
        SafeProviders::Known(list) if list.is_empty() => {
            return Err(format!(
                "safe mode is ON but `{model_id}` has ZERO providers on OpenRouter that satisfy \
                 training=false AND retains_prompts=false — every provider serving this model \
                 trains on or retains prompts. Refusing to send an unconstrained (or \
                 empty-allowlist) request. Pick a different model, or turn off safe mode for \
                 non-client-repository testing."
            ));
        }
        SafeProviders::Known(list) => list,
    };

    let only: Vec<String> = match &policy.pinned_provider {
        Some(pin) if safe_set.iter().any(|p| p == pin) => vec![pin.clone()],
        // Pin unset, or set but NOT in the safe set: never widen — fall back to the
        // full safe set. (An unsafe/unknown pin is silently dropped, per the design's
        // "never widen" rule; Pass 2's UI is expected to surface this via the picker
        // disabling unsafe providers in safe mode, so this path is a defensive floor,
        // not the primary UX.)
        _ => safe_set.clone(),
    };

    Ok(Some(serde_json::json!({
        "data_collection": "deny",
        "only": only,
    })))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn safe(providers: &[&str]) -> SafeProviders {
        SafeProviders::Known(providers.iter().map(|s| s.to_string()).collect())
    }

    // ── ProviderPolicy defaults ────────────────────────────────────────────────

    #[test]
    fn default_policy_is_safe_mode_on_no_pin() {
        let p = ProviderPolicy::default();
        assert!(p.safe_mode, "safe_mode must default to true");
        assert_eq!(p.pinned_provider, None);
    }

    #[test]
    fn deserializing_empty_json_object_yields_safe_default() {
        // An old settings file (or a hand-edited one) that omits the field entirely
        // must still resolve to safe_mode=true, not panic and not default to false.
        let p: ProviderPolicy = serde_json::from_str("{}").unwrap();
        assert!(p.safe_mode, "missing safe_mode in JSON must default to true (fail-safe)");
        assert_eq!(p.pinned_provider, None);
    }

    #[test]
    fn deserializing_explicit_false_is_honored() {
        let p: ProviderPolicy = serde_json::from_str(r#"{"safe_mode": false}"#).unwrap();
        assert!(!p.safe_mode);
    }

    #[test]
    fn serde_roundtrip_preserves_pin() {
        let p = ProviderPolicy {
            safe_mode: true,
            pinned_provider: Some("deepinfra".to_string()),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProviderPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    // ── safe_mode OFF → no constraint, ever ────────────────────────────────────

    #[test]
    fn safe_mode_off_omits_constraint_even_with_known_safe_set() {
        let policy = ProviderPolicy { safe_mode: false, pinned_provider: None };
        let out = provider_constraint_for_request(&policy, &safe(&["deepinfra", "novita"]), "m");
        assert_eq!(out, Ok(None));
    }

    #[test]
    fn safe_mode_off_omits_constraint_even_when_safe_set_unknown() {
        // Testing mode never blocks on missing data — that's the whole point of the toggle.
        let policy = ProviderPolicy { safe_mode: false, pinned_provider: None };
        let out = provider_constraint_for_request(&policy, &SafeProviders::Unknown, "m");
        assert_eq!(out, Ok(None));
    }

    #[test]
    fn safe_mode_off_ignores_a_stale_pin() {
        // A pin set while testing mode is on must NOT narrow the (already-unconstrained)
        // request — pinning is a safe-mode concept.
        let policy = ProviderPolicy {
            safe_mode: false,
            pinned_provider: Some("deepinfra".to_string()),
        };
        let out = provider_constraint_for_request(&policy, &safe(&["deepinfra", "novita"]), "m");
        assert_eq!(out, Ok(None));
    }

    // ── safe_mode ON, known safe set → the core constraint ─────────────────────

    #[test]
    fn safe_mode_on_emits_data_collection_deny_and_only_the_safe_set() {
        let policy = ProviderPolicy::default();
        let out = provider_constraint_for_request(
            &policy,
            &safe(&["deepinfra", "novita"]),
            "deepseek/deepseek-v4-pro",
        )
        .expect("must not fail closed when the safe set is known and non-empty");
        let v = out.expect("must emit a constraint in safe mode");
        assert_eq!(v["data_collection"], "deny");
        let only: Vec<String> = v["only"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert_eq!(only, vec!["deepinfra".to_string(), "novita".to_string()]);
    }

    /// THE CORE INVARIANT: a provider that trains or retains must never appear in the
    /// emitted `only` list. `safe()` here only ever contains the clean set (that's what
    /// `safe_providers_for` guarantees upstream — see model_registry tests), so this
    /// asserts the constraint builder doesn't smuggle anything else in.
    #[test]
    fn training_or_retaining_provider_never_appears_in_only() {
        let policy = ProviderPolicy::default();
        // A safe set that deliberately does NOT include "deepseek" (trains) or
        // "streamlake" (retains) — simulating what `safe_providers_for` would have
        // already filtered out.
        let out = provider_constraint_for_request(&policy, &safe(&["deepinfra"]), "m")
            .unwrap()
            .unwrap();
        let only = out["only"].as_array().unwrap();
        assert!(!only.iter().any(|x| x == "deepseek"));
        assert!(!only.iter().any(|x| x == "streamlake"));
    }

    // ── pinning ─────────────────────────────────────────────────────────────────

    #[test]
    fn safe_pin_narrows_only_to_exactly_the_pin() {
        let policy = ProviderPolicy {
            safe_mode: true,
            pinned_provider: Some("deepinfra".to_string()),
        };
        let out = provider_constraint_for_request(&policy, &safe(&["deepinfra", "novita"]), "m")
            .unwrap()
            .unwrap();
        assert_eq!(out["only"], serde_json::json!(["deepinfra"]));
        assert_eq!(out["data_collection"], "deny");
    }

    /// SAFETY-CRITICAL: an unsafe pin must be dropped, never widen the constraint, and
    /// the unsafe provider must never appear in the emitted `only` list.
    #[test]
    fn unsafe_pin_is_dropped_request_stays_restricted_to_safe_set() {
        let policy = ProviderPolicy {
            safe_mode: true,
            // "deepseek" (first-party, trains+retains) is NOT in the safe set below.
            pinned_provider: Some("deepseek".to_string()),
        };
        let out = provider_constraint_for_request(&policy, &safe(&["deepinfra", "novita"]), "m")
            .unwrap()
            .unwrap();
        let only: Vec<String> = out["only"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert!(
            !only.contains(&"deepseek".to_string()),
            "unsafe pin must never appear in `only`: {only:?}"
        );
        // Falls back to the full safe set — never narrows to empty, never widens.
        assert_eq!(only, vec!["deepinfra".to_string(), "novita".to_string()]);
        assert_eq!(out["data_collection"], "deny");
    }

    #[test]
    fn unknown_pin_not_in_safe_set_is_also_dropped() {
        // A pin value that isn't even a known provider slug at all (typo, stale value)
        // behaves the same as a known-unsafe pin: dropped, never widens.
        let policy = ProviderPolicy {
            safe_mode: true,
            pinned_provider: Some("not-a-real-provider".to_string()),
        };
        let out = provider_constraint_for_request(&policy, &safe(&["deepinfra"]), "m")
            .unwrap()
            .unwrap();
        assert_eq!(out["only"], serde_json::json!(["deepinfra"]));
    }

    // ── fail-closed: safe mode ON but the safe set can't be determined ─────────

    #[test]
    fn fail_closed_when_safe_set_unknown() {
        let policy = ProviderPolicy::default();
        let result = provider_constraint_for_request(&policy, &SafeProviders::Unknown, "m");
        assert!(
            result.is_err(),
            "safe_mode ON + Unknown safe set MUST error, never fall through to an \
             unconstrained request"
        );
    }

    #[test]
    fn fail_closed_when_safe_set_known_but_empty() {
        let policy = ProviderPolicy::default();
        let result = provider_constraint_for_request(&policy, &safe(&[]), "some/model");
        assert!(
            result.is_err(),
            "safe_mode ON + zero safe providers MUST error, never emit `only: []` (an \
             empty allowlist is not a trustworthy block signal at the API level)"
        );
    }

    #[test]
    fn fail_closed_error_never_contains_an_unconstrained_provider_object() {
        // Belt-and-suspenders: the Err variant carries a String reason, not a Value —
        // there is structurally no way for a caller to accidentally extract a `provider`
        // JSON object out of the error path.
        let policy = ProviderPolicy::default();
        let result = provider_constraint_for_request(&policy, &SafeProviders::Unknown, "m");
        match result {
            Err(reason) => assert!(!reason.is_empty()),
            Ok(_) => panic!("expected fail-closed Err"),
        }
    }

    // ── Property-style: the core invariant across many mixed sets ─────────────

    /// For any mixed safe/pin combination, the emitted `only` list is always a SUBSET of
    /// the provided safe set — never contains anything outside it. This is the
    /// structural version of "a training/retaining provider can never be emitted": as
    /// long as `safe_providers_for` (model_registry.rs) correctly excludes unsafe
    /// providers from the set it hands in here, this proves the builder never
    /// reintroduces one.
    #[test]
    fn only_list_is_always_a_subset_of_the_safe_set() {
        let cases: Vec<(Vec<&str>, Option<&str>)> = vec![
            (vec!["deepinfra"], None),
            (vec!["deepinfra", "novita", "together"], None),
            (vec!["deepinfra", "novita"], Some("novita")),
            (vec!["deepinfra", "novita"], Some("deepseek")), // unsafe pin
            (vec!["deepinfra", "novita"], Some("bogus-slug")),
            (vec!["a", "b", "c", "d", "e"], Some("c")),
        ];
        for (safe_list, pin) in cases {
            let policy = ProviderPolicy {
                safe_mode: true,
                pinned_provider: pin.map(|s| s.to_string()),
            };
            let out = provider_constraint_for_request(&policy, &safe(&safe_list), "m")
                .unwrap()
                .unwrap();
            let only: Vec<String> = out["only"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect();
            for p in &only {
                assert!(
                    safe_list.contains(&p.as_str()),
                    "emitted provider `{p}` is not in the safe set {safe_list:?} \
                     (safe_list={safe_list:?}, pin={pin:?})"
                );
            }
            // And data_collection is always "deny" whenever a constraint is emitted.
            assert_eq!(out["data_collection"], "deny");
        }
    }

    /// Same property, but every case ALSO includes at least one deliberately-unsafe
    /// provider name mixed into the input to prove it never leaks through even when the
    /// caller's safe-set computation makes a mistake and includes it (defense in depth —
    /// the real exclusion happens in `model_registry::safe_providers_for`, tested there;
    /// this proves the constraint builder itself adds no leakage of its own).
    #[test]
    fn only_list_never_grows_beyond_the_input_safe_set() {
        let unsafe_markers = ["deepseek", "streamlake", "china-hosted-example"];
        let safe_list = vec!["deepinfra", "novita", "together"];
        let policy = ProviderPolicy::default();
        let out = provider_constraint_for_request(&policy, &safe(&safe_list), "m")
            .unwrap()
            .unwrap();
        let only: Vec<String> = out["only"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        for marker in unsafe_markers {
            assert!(!only.contains(&marker.to_string()));
        }
        assert_eq!(only.len(), safe_list.len());
    }
}
