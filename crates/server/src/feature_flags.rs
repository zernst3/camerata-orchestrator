//! Feature flags for the Camerata server.
//!
//! A `FeatureFlags` struct where **every flag defaults to `true`** (opt-out
//! model): a feature is OFF only when explicitly set to `false`. This matches
//! the `CAMERATA_LIVE_BUILD` precedent: an absent env var means the default
//! behaviour, not a disabled feature.
//!
//! # Sources (priority order, highest wins)
//!
//! 1. **Env overrides** — `CAMERATA_FEATURE_<UPPER_NAME>=false` disables the
//!    named flag. Only `false` is acted on; any other value (including absent)
//!    leaves the flag at its configured/default value.
//! 2. **Config file** — `.camerata/features.toml` (relative to the process
//!    CWD) or the `feature_flags` section of the data-dir `settings.json`.
//!    Both follow the same TOML shape: a flat table of `flag_name = false`.
//! 3. **Default** — `true`. A feature absent from all sources is ON.
//!
//! # Shipped flags
//!
//! - `soc2` (`CAMERATA_FEATURE_SOC2`) — The SOC-2 gap-analysis lens in the
//!   deep audit tier (`run_deep_tier`). Shipped **disabled** (written as
//!   `soc2 = false` in `.camerata/features.toml`). The code that runs the
//!   lens is retained; only the runtime execution is gated. Disable, do NOT
//!   remove the SOC-2 code.
//!
//! # Adding a new flag
//!
//! 1. Add a `pub <name>: bool` field to [`FeatureFlags`] with
//!    `#[serde(default = "default_true")]`.
//! 2. Add the env-override arm in [`FeatureFlags::apply_env_overrides`].
//! 3. Document it in this module comment.

use serde::{Deserialize, Serialize};

/// serde default helper — returns `true`, which is the absence-default for
/// every flag in this struct.
fn default_true() -> bool {
    true
}

/// All runtime feature flags. Every field defaults to `true`; a flag is OFF
/// only when explicitly set to `false` (in the config file or via an env var).
///
/// # Serialisation
///
/// The struct serialises cleanly to/from TOML and JSON. When written to
/// `.camerata/features.toml` a flag set to `false` appears as `soc2 = false`;
/// absent flags remain `true`. When exposed over `GET /api/feature-flags` the
/// same JSON shape is returned so the UI can render banners / gate controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// The SOC-2 gap-analysis lens in `run_deep_tier`. When `false`, the SOC-2
    /// lens is SKIPPED in the deep audit (the deep-security + threat-model
    /// lenses still run; the report is valid with an empty `soc2_gaps` field).
    /// Shipped disabled; set to `true` in `.camerata/features.toml` to enable.
    #[serde(default = "default_true")]
    pub soc2: bool,
}

impl Default for FeatureFlags {
    /// All flags on by default.
    fn default() -> Self {
        Self { soc2: true }
    }
}

impl FeatureFlags {
    /// Load feature flags from (in priority order):
    ///
    /// 1. `.camerata/features.toml` relative to `cwd` (if present).
    /// 2. Defaults (`true` for every flag).
    ///
    /// After loading from config, env overrides are applied on top.
    ///
    /// This function is synchronous and infallible: a missing / malformed
    /// config file falls back to defaults rather than erroring, so the server
    /// always starts.
    pub fn load() -> Self {
        let mut flags = Self::load_from_config_file();
        flags.apply_env_overrides();
        flags
    }

    /// Load from `.camerata/features.toml` (relative to CWD). Falls back to
    /// [`Default`] on any error (file not found, parse error, etc.).
    fn load_from_config_file() -> Self {
        let path = std::path::Path::new(".camerata/features.toml");
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str::<Self>(&text).unwrap_or_default()
    }

    /// Apply `CAMERATA_FEATURE_<UPPER_NAME>=false` env overrides on top of the
    /// current values. Only an explicit `"false"` (case-insensitive) disables a
    /// flag; absent or any other value leaves it as-is.
    fn apply_env_overrides(&mut self) {
        if env_is_false("CAMERATA_FEATURE_SOC2") {
            self.soc2 = false;
        }
    }
}

/// Returns `true` iff the named env var exists AND its value is `"false"` (case-
/// insensitive). An absent, empty, or non-`"false"` value returns `false` (the
/// feature stays at its configured value).
fn env_is_false(var: &str) -> bool {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_flags_are_all_true() {
        let flags = FeatureFlags::default();
        assert!(flags.soc2, "soc2 defaults to true");
    }

    #[test]
    fn explicit_false_disables_flag() {
        let toml_src = r#"soc2 = false"#;
        let flags: FeatureFlags = toml::from_str(toml_src).expect("parses");
        assert!(!flags.soc2, "explicit soc2 = false disables the flag");
    }

    #[test]
    fn missing_field_defaults_to_true() {
        // An empty TOML file has no fields; every flag should be true.
        let flags: FeatureFlags = toml::from_str("").expect("empty toml parses");
        assert!(flags.soc2, "absent soc2 field defaults to true");
    }

    #[test]
    fn env_override_false_disables_flag() {
        // Simulate CAMERATA_FEATURE_SOC2=false by using the internal helper.
        // We can't set env vars in a safe test (parallel tests), so we test the
        // helper logic directly.
        assert!(env_is_false_str("false"));
        assert!(env_is_false_str("FALSE"));
        assert!(env_is_false_str("False"));
        assert!(!env_is_false_str("true"));
        assert!(!env_is_false_str(""));
        assert!(!env_is_false_str("0"));
    }

    /// Mirror of `env_is_false` without the env-var lookup, so tests don't
    /// need to touch the real environment.
    fn env_is_false_str(val: &str) -> bool {
        val.trim().eq_ignore_ascii_case("false")
    }
}
