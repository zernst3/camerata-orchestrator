//! `candidates`: where in a file a rule's subject can sit (token-efficiency rank 8).
//!
//! # Why a rule declares this
//!
//! Even after `applies_to` routing (rank 7), a rule is sent with whole files: a search-path
//! rule about `CREATE FUNCTION` statements reads every table definition, seed row and comment of
//! every migration. A rule that names a deterministic LOCATOR lets the scan send it only the
//! sites that locator finds (each with a few lines of context) plus the repo map, instead of the
//! whole files (`camerata-server`'s `candidates` module builds that prompt; the locators live in
//! `camerata-checks`' `candidates` module, on the same SQL statement splitter the Supabase
//! checkers use).
//!
//! # The shape
//!
//! ```toml
//! candidates = { locator = "sql_routines" }
//! ```
//!
//! `locator` is one of [`CandidateLocator`]'s names ([`CANDIDATE_LOCATORS`]). An absent
//! `candidates` means whole files, exactly as before this field existed.
//!
//! # Validation is the loader's job, and it is loud
//!
//! A locator that silently found nothing would hide every finding of its rule. So a value this
//! module cannot honour is a LOAD ERROR (it reaches the caller as a TOML parse error naming the
//! file): an unknown locator, an unknown key, a missing `locator`, a `candidates` that is not a
//! table. The scan adds the runtime half of the same guarantee: a locator that errors, times out
//! or cannot parse a file sends the rule the whole files of that chunk instead.

use serde::de::{self, Deserializer};
use serde::Deserialize;

/// A deterministic candidate-site locator. Each one is a statement filter over the SQL
/// statement splitter (`camerata_checks::supabase::splitter`); the filter is deliberately WIDE
/// (any statement that mentions the subject, not only the ones that look wrong), because a site
/// the locator misses is a finding the model never sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateLocator {
    /// SQL statements that create, alter, grant or revoke a routine (`FUNCTION`, `PROCEDURE`,
    /// `ROUTINE`, plural forms included), plus anonymous `DO` blocks.
    SqlRoutines,
    /// SQL statements that create, alter or drop a Row Level Security policy (`POLICY`,
    /// `POLICIES`).
    SqlPolicies,
    /// The union of [`Self::SqlPolicies`], [`Self::SqlRoutines`] and every view statement
    /// (`VIEW`, `VIEWS`, materialized views included): for a rule about policy predicates that a
    /// policy can also reach through a helper function it calls or a view it selects from.
    SqlPoliciesRoutinesAndViews,
    /// SQL statements that define or alter anything that gives a value a column type: a table,
    /// a type, a domain, a view or materialized view (`TABLE`, `TYPE`, `DOMAIN`, `VIEW`, plural
    /// forms included), and any non-INSERT statement with an `INTO` (`SELECT ... INTO new_table`,
    /// `CREATE TABLE ... AS`-style shapes): where column types, constraints and foreign keys are
    /// declared.
    SqlTables,
}

/// Every locator name, in declaration order: the closed vocabulary for `candidates.locator`.
pub const CANDIDATE_LOCATORS: &[&str] = &[
    "sql_routines",
    "sql_policies",
    "sql_policies_routines_and_views",
    "sql_tables",
];

impl CandidateLocator {
    /// Every locator, in the order of [`CANDIDATE_LOCATORS`].
    pub const ALL: &'static [CandidateLocator] = &[
        Self::SqlRoutines,
        Self::SqlPolicies,
        Self::SqlPoliciesRoutinesAndViews,
        Self::SqlTables,
    ];

    /// The name a rule TOML uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::SqlRoutines => "sql_routines",
            Self::SqlPolicies => "sql_policies",
            Self::SqlPoliciesRoutinesAndViews => "sql_policies_routines_and_views",
            Self::SqlTables => "sql_tables",
        }
    }

    /// The locator a rule TOML names. `Err` names the valid choices.
    pub fn from_name(name: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|l| l.name() == name)
            .ok_or_else(|| {
                format!(
                    "candidates locator `{name}` is not recognised; the locators are: {}",
                    CANDIDATE_LOCATORS.join(", ")
                )
            })
    }
}

/// A rule's declared candidate locator. Constructed only through [`Candidates::new`] or
/// deserialization, both of which validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidates {
    /// The locator that finds this rule's candidate sites.
    pub locator: CandidateLocator,
}

impl Candidates {
    /// Validate and build from a locator name. `Err` names the valid choices.
    pub fn new(locator: &str) -> Result<Self, String> {
        CandidateLocator::from_name(locator).map(|locator| Self { locator })
    }
}

/// The TOML shape of `candidates`. `deny_unknown_fields` so an unrecognised key (a future
/// `query`, or a typo of `locator`) is a load error, never an ignored locator.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidatesToml {
    locator: String,
}

impl<'de> Deserialize<'de> for Candidates {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = CandidatesToml::deserialize(deserializer)?;
        Candidates::new(&raw.locator).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<Candidates, String> {
        #[derive(Deserialize)]
        struct W {
            candidates: Candidates,
        }
        toml::from_str::<W>(src)
            .map(|w| w.candidates)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_known_locator_parses() {
        let c = parse(r#"candidates = { locator = "sql_routines" }"#).unwrap();
        assert_eq!(c.locator, CandidateLocator::SqlRoutines);
    }

    #[test]
    fn an_unknown_locator_is_a_load_error_naming_the_choices() {
        let err = parse(r#"candidates = { locator = "sql_routine" }"#).unwrap_err();
        assert!(
            err.contains("sql_routine") && err.contains("sql_policies, "),
            "{err}"
        );
    }

    #[test]
    fn a_malformed_block_is_a_load_error_never_matches_nothing() {
        for bad in [
            "candidates = {}",
            r#"candidates = { locator = "sql_routines", query = "x" }"#,
            r#"candidates = { locatr = "sql_routines" }"#,
            r#"candidates = "sql_routines""#,
            r#"candidates = { locator = "" }"#,
            r#"candidates = { locator = "SQL_ROUTINES" }"#,
        ] {
            assert!(parse(bad).is_err(), "`{bad}` must be rejected");
        }
    }

    #[test]
    fn the_name_table_and_the_enum_agree() {
        assert_eq!(CandidateLocator::ALL.len(), CANDIDATE_LOCATORS.len());
        for (locator, name) in CandidateLocator::ALL.iter().zip(CANDIDATE_LOCATORS) {
            assert_eq!(locator.name(), *name);
            assert_eq!(CandidateLocator::from_name(name), Ok(*locator));
        }
    }
}
