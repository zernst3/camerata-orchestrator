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
//! - `model_prompt_redaction` (`CAMERATA_FEATURE_MODEL_PROMPT_REDACTION`) — Redact secret
//!   values from code before any model sees it (`onboard::redact::files_for_model`). ON by
//!   default; operator-only, no UI. **Environment variable ONLY**: no config file can turn it
//!   off, because the file would sit in (or be resolved against) a client repository, and a
//!   client repo must never decide that its own code reaches a model unredacted. Turning it OFF
//!   prints a warning on the CLI's stderr and logs one on every call. See
//!   docs/design/2026-10-23_secret-redaction-and-release-gate.md.
//!
//! # Adding a new flag
//!
//! 1. Add a `pub <name>: bool` field to [`FeatureFlags`] with
//!    `#[serde(default = "default_true")]`.
//! 2. Add the env-override arm in [`FeatureFlags::apply_env_overrides`].
//! 3. Document it in this module comment.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// serde default helper — returns `true`, which is the absence-default for
/// every flag in this struct.
fn default_true() -> bool {
    true
}

/// The one switch for `model_prompt_redaction`: set to `false` to send raw code to the model.
pub const MODEL_PROMPT_REDACTION_ENV: &str = "CAMERATA_FEATURE_MODEL_PROMPT_REDACTION";

/// The operator's value of [`MODEL_PROMPT_REDACTION_ENV`] as it was before any `.env` loader ran.
static OPERATOR_VALUE: OnceLock<Option<String>> = OnceLock::new();

/// Record the operator's real environment value for the redaction switch. A binary calls this
/// FIRST in `main`, before it loads a `.env` file: a `.env` is resolved against the working
/// directory, which may be a client repository, and must not be able to switch redaction off.
/// Idempotent; without a call the live environment is read.
pub fn pin_operator_redaction_setting() {
    OPERATOR_VALUE.get_or_init(|| std::env::var(MODEL_PROMPT_REDACTION_ENV).ok());
}

/// Whether code is redacted before any model sees it. Reads the environment ONLY (never a file in
/// the working directory or a client repository); ON unless the variable is explicitly `false`.
pub fn model_prompt_redaction_enabled() -> bool {
    let value = match OPERATOR_VALUE.get() {
        Some(pinned) => pinned.clone(),
        None => std::env::var(MODEL_PROMPT_REDACTION_ENV).ok(),
    };
    !value.is_some_and(|v| is_false(&v))
}

/// The operator-facing warning when redaction is switched OFF, or `None` when it is on. The CLI
/// prints it on stderr at startup.
pub fn redaction_disabled_warning() -> Option<String> {
    (!model_prompt_redaction_enabled()).then(|| {
        format!(
            "WARNING: model-prompt redaction is OFF ({MODEL_PROMPT_REDACTION_ENV}=false). \
             Raw client code, secret values included, will be sent to the model."
        )
    })
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
    /// Redact secret values from the code before it is sent to ANY model (the audit passes, the
    /// fix-specific pass, the deep tier, the clone judge): `sb_secret_…WvXy (51 chars)` instead
    /// of the key, line numbers unchanged. ON by default (owner decision, 2026-10-22: "redact
    /// BEFORE any code is sent to a model"). Operator-only: no UI affordance. When `false`,
    /// `onboard::redact::files_for_model` sends raw code and logs a warning once per call.
    /// Set from the ENVIRONMENT only (`skip_deserializing`: a config file cannot touch it).
    #[serde(default = "default_true", skip_deserializing)]
    pub model_prompt_redaction: bool,
}

impl Default for FeatureFlags {
    /// All flags on by default.
    fn default() -> Self {
        Self {
            soc2: true,
            model_prompt_redaction: true,
        }
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
        let mut flags = Self::load_from(std::path::Path::new(".camerata/features.toml"));
        flags.apply_env_overrides();
        flags
    }

    /// Load from the config file at `path`. Falls back to [`Default`] on any error (file not
    /// found, parse error, etc.).
    fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::from_config_text(&text))
            .unwrap_or_default()
    }

    /// Parse config-file text. `model_prompt_redaction` is never read from it (environment only).
    fn from_config_text(text: &str) -> Self {
        toml::from_str::<Self>(text).unwrap_or_default()
    }

    /// Apply `CAMERATA_FEATURE_<UPPER_NAME>=false` env overrides on top of the
    /// current values. Only an explicit `"false"` (case-insensitive) disables a
    /// flag; absent or any other value leaves it as-is.
    fn apply_env_overrides(&mut self) {
        if env_is_false("CAMERATA_FEATURE_SOC2") {
            self.soc2 = false;
        }
        self.model_prompt_redaction = model_prompt_redaction_enabled();
    }
}

/// Returns `true` iff the named env var exists AND its value is `"false"` (case-
/// insensitive). An absent, empty, or non-`"false"` value returns `false` (the
/// feature stays at its configured value).
fn env_is_false(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| is_false(&v))
}

fn is_false(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("false")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_flags_are_all_true() {
        let flags = FeatureFlags::default();
        assert!(flags.soc2, "soc2 defaults to true");
        assert!(
            flags.model_prompt_redaction,
            "model_prompt_redaction defaults to ON: secrets are redacted before any model sees code"
        );
    }

    #[test]
    fn model_prompt_redaction_cannot_be_switched_off_by_a_config_file() {
        // A client repo can carry `.camerata/features.toml`; it must never be able to send that
        // client's own code to a model unredacted.
        let flags = FeatureFlags::from_config_text("model_prompt_redaction = false\nsoc2 = false");
        assert!(flags.model_prompt_redaction, "the file has no say");
        assert!(!flags.soc2, "other flags still come from the file");
    }

    #[test]
    fn a_features_toml_in_a_client_repo_has_no_effect_on_model_prompt_redaction() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join(".camerata")).expect("mkdir");
        let file = repo.path().join(".camerata/features.toml");
        std::fs::write(&file, "model_prompt_redaction = false\n").expect("write");
        assert!(FeatureFlags::load_from(&file).model_prompt_redaction);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn model_prompt_redaction_is_on_unless_the_environment_says_false() {
        let _unset = camerata_test_env::EnvGuard::remove(MODEL_PROMPT_REDACTION_ENV);
        assert!(model_prompt_redaction_enabled());
        assert_eq!(redaction_disabled_warning(), None);
        drop(_unset);
        let _off = camerata_test_env::EnvGuard::set(MODEL_PROMPT_REDACTION_ENV, "false");
        assert!(!model_prompt_redaction_enabled());
        let warning = redaction_disabled_warning().expect("a warning when it is off");
        assert!(warning.contains("redaction is OFF"), "{warning}");
        assert!(warning.contains(MODEL_PROMPT_REDACTION_ENV), "{warning}");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn model_prompt_redaction_env_override_switches_it_off() {
        let _env =
            camerata_test_env::EnvGuard::set("CAMERATA_FEATURE_MODEL_PROMPT_REDACTION", "false");
        let mut flags = FeatureFlags::default();
        flags.apply_env_overrides();
        assert!(!flags.model_prompt_redaction);
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

    /// `env_is_false` without the env-var lookup, so tests don't need to touch the real
    /// environment.
    fn env_is_false_str(val: &str) -> bool {
        is_false(val)
    }
}
