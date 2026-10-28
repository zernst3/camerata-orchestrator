//! The per-measure switches for the token-efficiency work (`SPIKE_TOKEN_EFFICIENCY_RESULTS.md`).
//!
//! # Why one struct
//!
//! Every efficiency measure changes how a scan is run, not what it finds, and each is meant to
//! be a pure saving. When a run costs more than it should, or answers differently than it did,
//! the first question is "which measure did it?", and the fastest honest answer is to switch one
//! off and run again WITHOUT a rebuild. This is the one place those switches live. A later
//! measure adds one line to the [`efficiency_measures!`] list below and reads
//! `meter.efficiency().<name>` at its code path; nothing else changes.
//!
//! # Settings
//!
//! * Default: every built measure is ON.
//! * Environment: `CAMERATA_EFFICIENCY=name=off,other=off` (entries are `name=on|off`,
//!   comma separated).
//! * CLI: `camerata inspect --efficiency name=off`, repeatable. The CLI applies after the
//!   environment, so a flag wins.
//!
//! An unknown name is an ERROR, never ignored: a typo that leaves a measure on while the
//! operator believes it is off would defeat the point of the switch.
//!
//! # Recorded, never exported
//!
//! The resolved set is written to the operator ledger and printed in the per-stage telemetry
//! table, so every run states which measures were on. It is not part of the client export (the
//! export format is a protected area; `tests/stage_telemetry_not_exported.rs` pins that).

use serde::{Deserialize, Serialize};

/// The environment variable that carries `name=on|off` entries.
pub const EFFICIENCY_ENV: &str = "CAMERATA_EFFICIENCY";

/// Defines [`EfficiencyConfig`] and its name table from ONE list, so a measure's field, its
/// public name and its one-line description cannot drift apart.
macro_rules! efficiency_measures {
    ($( $(#[$meta:meta])* $field:ident => $desc:literal ),+ $(,)?) => {
        /// One boolean per efficiency measure. `true` = the measure is applied (the default).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct EfficiencyConfig {
            $( $(#[$meta])* pub $field: bool, )+
        }

        impl Default for EfficiencyConfig {
            /// Every built measure ON.
            fn default() -> Self {
                Self { $( $field: true, )+ }
            }
        }

        impl EfficiencyConfig {
            /// Every measure name and its one-line description, in declaration order.
            pub const MEASURES: &'static [(&'static str, &'static str)] = &[
                $( (stringify!($field), $desc), )+
            ];

            /// Set one measure by its public name. `Err` names the valid choices.
            pub fn set(&mut self, name: &str, on: bool) -> Result<(), String> {
                match name {
                    $( stringify!($field) => self.$field = on, )+
                    other => return Err(format!(
                        "unknown efficiency measure `{other}`; the measures are: {}",
                        Self::names().join(", ")
                    )),
                }
                Ok(())
            }

            /// Every measure with its current state, in declaration order.
            pub fn entries(&self) -> Vec<(&'static str, bool)> {
                vec![ $( (stringify!($field), self.$field), )+ ]
            }
        }
    };
}

efficiency_measures! {
    /// Rank 3: a chunk's first rule-batch runs and returns before its siblings start, so the
    /// siblings read the prompt cache entry it wrote. Off: every pass is dispatched as soon as
    /// a concurrency slot is free (the pre-rank-3 behavior).
    warm_schedule => "a chunk's first pass warms the prompt cache before its sibling passes start",
    /// Rank 3: the per-group label line sits in the uncached suffix, so routing groups over the
    /// same files read one cached digest. Off: the label leads the digest segment again.
    label_after_prefix => "the group label stays out of the cached prompt prefix",
    /// Rank 3: Message Batch items ask for the 1-hour prompt cache. Off: they ask for the
    /// 5-minute cache, which expires before most batch items run.
    batch_long_cache => "batch items request the 1-hour prompt cache",
    /// Rank 5: an incremental scan reuses the previous scan's AI findings for unchanged files
    /// (keyed by model, prompt version and rule wording). Off: always a full AI re-scan, like
    /// `camerata inspect --full`.
    result_cache => "unchanged files reuse the previous scan's AI findings",
    /// Rank 6: a scan reply cut off at the output limit is repeated once with a higher limit.
    /// Off: the cut-off reply is used as it came.
    truncation_retry => "a scan reply cut off at the output limit is retried once",
    /// Rank 6: a finding with no surrounding code goes straight to the rule's authored
    /// remediation with no model call. Off: it is sent to the fix model like any other.
    fix_prefilter => "findings with no code context skip the fix model when the rule has an authored fix",
    /// Rank 10: with no fix model set, the fix stage runs on
    /// [`crate::ai_audit::FIX_STAGE_HAIKU_MODEL`] at effort [`crate::ai_audit::FIX_STAGE_EFFORT`].
    /// Off: it runs on the calibration model, else the scan model (the pre-rank-10 choice).
    fix_on_haiku => "the fix stage runs on Haiku unless a fix model is set",
    /// Rank 4: scan passes ask for a one-sentence `detail` and no `proposed_rules` (the advisory
    /// pass keeps them), and the full `detail` is regenerated once, after the first merge and
    /// before calibration, for the findings that survived. Off: the scan prompts and the
    /// pipeline are exactly what they were before rank 4.
    terse_output => "scan passes write a short detail; the full detail is regenerated for the findings that survive the merge",
    /// Rank 2: on the API backend a scan runs as a Message Batch unless real time is forced,
    /// errored or expired batch items are resubmitted once in a follow-up batch, a batch item
    /// cut off at the output limit gets the real-time truncation retry, batch rule-batches hold
    /// 15 rules, and the batch cache TTL is the cheaper one by the price arithmetic. Off: batch
    /// is opt-in again and runs exactly as before rank 2 (one submission, all rules per item,
    /// the `batch_long_cache` TTL).
    batch_default => "on the API backend scans run as a Message Batch, with failed items resubmitted once",
}

impl EfficiencyConfig {
    /// Just the names, in declaration order.
    pub fn names() -> Vec<&'static str> {
        Self::MEASURES.iter().map(|(n, _)| *n).collect()
    }

    /// Apply one `name=on|off` entry (`name=off`, `name=on`; also `0/1`, `false/true`).
    pub fn apply_entry(&mut self, entry: &str) -> Result<(), String> {
        let (name, value) = entry.split_once('=').ok_or_else(|| {
            format!("`{entry}` is not `name=on` or `name=off` (e.g. `warm_schedule=off`)")
        })?;
        let on = match value.trim().to_ascii_lowercase().as_str() {
            "on" | "1" | "true" | "yes" => true,
            "off" | "0" | "false" | "no" => false,
            other => {
                return Err(format!(
                    "`{entry}`: `{other}` is not on or off (e.g. `{}=off`)",
                    name.trim()
                ))
            }
        };
        self.set(name.trim(), on)
    }

    /// Apply a comma-separated list of entries (the [`EFFICIENCY_ENV`] value). Blank is a
    /// no-op.
    pub fn apply_list(&mut self, list: &str) -> Result<(), String> {
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            self.apply_entry(entry)?;
        }
        Ok(())
    }

    /// The defaults with [`EFFICIENCY_ENV`] applied. `Err` on a malformed or unknown entry.
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_value(std::env::var(EFFICIENCY_ENV).ok().as_deref())
    }

    /// [`Self::from_env`] over an already-read value (`None` = unset), so the parse is testable
    /// without mutating the process environment.
    pub fn from_env_value(value: Option<&str>) -> Result<Self, String> {
        let mut config = Self::default();
        if let Some(list) = value {
            config
                .apply_list(list)
                .map_err(|e| format!("{EFFICIENCY_ENV}: {e}"))?;
        }
        Ok(config)
    }

    /// [`Self::from_env`] for a caller with nowhere to report an error (the server's own scan
    /// handlers): a bad value is logged loudly and the defaults are used, so a typo can never
    /// stop a scan, and the log says why a measure the operator tried to disable is still on.
    pub fn from_env_or_default() -> Self {
        Self::from_env().unwrap_or_else(|e| {
            eprintln!("[camerata-server] {e}; running with every efficiency measure ON");
            Self::default()
        })
    }

    /// The measures that are OFF, by name.
    pub fn disabled(&self) -> Vec<&'static str> {
        self.entries()
            .into_iter()
            .filter(|(_, on)| !on)
            .map(|(n, _)| n)
            .collect()
    }

    /// One line stating the whole set, e.g. `warm_schedule=on label_after_prefix=on ...`.
    /// Printed in the telemetry table and the CLI summary.
    pub fn render(&self) -> String {
        self.entries()
            .iter()
            .map(|(n, on)| format!("{n}={}", if *on { "on" } else { "off" }))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_measure_defaults_on() {
        let config = EfficiencyConfig::default();
        assert!(config.entries().iter().all(|(_, on)| *on));
        assert!(config.disabled().is_empty());
    }

    #[test]
    fn a_name_switches_exactly_that_measure() {
        for name in EfficiencyConfig::names() {
            let mut config = EfficiencyConfig::default();
            config.apply_entry(&format!("{name}=off")).unwrap();
            assert_eq!(config.disabled(), vec![name], "only {name} should be off");
            config.apply_entry(&format!("{name}=on")).unwrap();
            assert_eq!(config, EfficiencyConfig::default());
        }
    }

    #[test]
    fn a_list_applies_in_order_and_later_entries_win() {
        let mut config = EfficiencyConfig::default();
        config
            .apply_list(" warm_schedule=off , truncation_retry=off,warm_schedule=on ")
            .unwrap();
        assert_eq!(config.disabled(), vec!["truncation_retry"]);
    }

    #[test]
    fn an_unknown_or_malformed_entry_is_an_error_naming_the_choices() {
        let mut config = EfficiencyConfig::default();
        let err = config.apply_entry("warm_shedule=off").unwrap_err();
        assert!(
            err.contains("warm_shedule") && err.contains("warm_schedule"),
            "{err}"
        );
        assert!(config.apply_entry("warm_schedule").is_err());
        assert!(config.apply_entry("warm_schedule=maybe").is_err());
        assert_eq!(
            config,
            EfficiencyConfig::default(),
            "an error changes nothing"
        );
    }

    #[test]
    fn render_states_every_measure() {
        let mut config = EfficiencyConfig::default();
        config.set("result_cache", false).unwrap();
        let line = config.render();
        for name in EfficiencyConfig::names() {
            assert!(line.contains(&format!("{name}=")), "{line}");
        }
        assert!(line.contains("result_cache=off") && line.contains("warm_schedule=on"));
    }

    #[test]
    fn a_stored_set_missing_a_newer_measure_loads_with_it_on() {
        let back: EfficiencyConfig = serde_json::from_str(r#"{"warm_schedule":false}"#).unwrap();
        assert!(!back.warm_schedule);
        assert!(back.truncation_retry, "an absent measure defaults ON");
    }

    #[test]
    fn the_environment_value_is_parsed_and_a_bad_value_is_an_error() {
        assert_eq!(
            EfficiencyConfig::from_env_value(None).unwrap(),
            EfficiencyConfig::default()
        );
        assert_eq!(
            EfficiencyConfig::from_env_value(Some("fix_prefilter=off"))
                .unwrap()
                .disabled(),
            vec!["fix_prefilter"]
        );
        let err = EfficiencyConfig::from_env_value(Some("nonsense=off")).unwrap_err();
        assert!(err.starts_with(EFFICIENCY_ENV), "{err}");
    }
}
