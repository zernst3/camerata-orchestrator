//! End-of-life data from endoflife.date, one product at a time.
//!
//! `GET https://endoflife.date/api/<product>.json` returns the product's release lines, newest
//! first, each with an `eol` that is a date (`"2023-09-11"`), `true` (already ended, date not
//! recorded) or `false` (no end date set yet). The parser keeps what the audit reads and refuses a
//! body it cannot believe, so a changed schema is a failed fetch (cache, or not checked) and never
//! an empty feed that was "checked".

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// The endoflife.date API root.
pub const EOL_API: &str = "https://endoflife.date/api";

/// The URL for one product.
pub fn product_url(slug: &str) -> String {
    format!("{EOL_API}/{slug}.json")
}

/// When a release line stops receiving security fixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "date", rename_all = "snake_case")]
pub enum EolDate {
    /// Ends on this date (`YYYY-MM-DD`).
    On(String),
    /// Already ended; the feed records no date.
    Ended,
    /// No end date is set: the line is supported.
    Supported,
    /// The feed gives a value for the end date that is not a date or a boolean (the text as
    /// given). The release line is known but its end date is not, so it is not guessed.
    Unreadable(String),
}

/// One release line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EolCycle {
    pub cycle: String,
    pub eol: EolDate,
}

/// One product's release lines, newest first as the feed lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EolFeed {
    pub cycles: Vec<EolCycle>,
}

/// Where a release line stands on a given day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Security fixes have ended. `since` is the end date when the feed records one.
    Ended { since: Option<String> },
    /// Security fixes end on this date, within the warning window.
    EndsSoon { on: String, days: i64 },
    /// Supported beyond the warning window.
    Supported,
}

impl EolFeed {
    /// Parse the body of `<product>.json`.
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        let rows = v
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("endoflife.date response is not a list of releases"))?;
        let cycles: Vec<EolCycle> = rows
            .iter()
            .filter_map(|row| {
                let cycle = match row.get("cycle")? {
                    serde_json::Value::String(s) => s.trim().to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };
                if cycle.is_empty() {
                    return None;
                }
                let eol = match row.get("eol") {
                    Some(serde_json::Value::Bool(true)) => EolDate::Ended,
                    Some(serde_json::Value::Bool(false)) => EolDate::Supported,
                    Some(serde_json::Value::String(text)) => {
                        let date = text
                            .trim()
                            .get(..10)
                            .filter(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok());
                        match date {
                            Some(date) => EolDate::On(date.to_string()),
                            None => EolDate::Unreadable(text.trim().to_string()),
                        }
                    }
                    Some(other) => EolDate::Unreadable(other.to_string()),
                    None => EolDate::Unreadable("no end date given".to_string()),
                };
                Some(EolCycle { cycle, eol })
            })
            .collect();
        // A body whose every end date is unreadable is a changed schema, not a feed to trust.
        if !cycles
            .iter()
            .any(|c| !matches!(c.eol, EolDate::Unreadable(_)))
        {
            anyhow::bail!(
                "endoflife.date response had {} rows but none with a release line and an end date",
                rows.len()
            );
        }
        Ok(Self { cycles })
    }

    /// The feed's release line that a declared version belongs to: the longest line that is the
    /// version itself or a dotted prefix of it (`3.11` for `3.11.4`, `8` for `8.0`).
    pub fn cycle_for(&self, version: &str) -> CycleMatch<'_> {
        let best = self
            .cycles
            .iter()
            .filter(|c| version == c.cycle || version.starts_with(&format!("{}.", c.cycle)))
            .max_by_key(|c| c.cycle.len());
        if let Some(cycle) = best {
            return CycleMatch::Found(cycle);
        }
        // `3` names many lines (`3.9`, `3.10`, ...): not one release line, so it is not guessed.
        let prefix = format!("{version}.");
        if self.cycles.iter().any(|c| c.cycle.starts_with(&prefix)) {
            CycleMatch::TooBroad
        } else {
            CycleMatch::Unlisted
        }
    }

    /// The nearest release line above `cycle` that is supported on `today` and does not end
    /// within `warn_days`: moving to a line about to end would only repeat the problem. A line
    /// whose end date cannot be read is not suggested.
    pub fn nearest_supported_above(
        &self,
        cycle: &str,
        today: NaiveDate,
        warn_days: i64,
    ) -> Option<&str> {
        let floor = sort_key(cycle);
        self.cycles
            .iter()
            .filter(|c| sort_key(&c.cycle) > floor)
            .filter(|c| !matches!(c.eol, EolDate::Unreadable(_)))
            .filter(|c| matches!(standing(&c.eol, today, warn_days), Standing::Supported))
            .min_by_key(|c| sort_key(&c.cycle))
            .map(|c| c.cycle.as_str())
    }
}

/// The result of placing a declared version on the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleMatch<'a> {
    Found(&'a EolCycle),
    /// The declared version names several release lines.
    TooBroad,
    /// The feed lists no such release line.
    Unlisted,
}

/// A release line name as comparable numbers (`3.10` after `3.9`).
fn sort_key(cycle: &str) -> Vec<u64> {
    cycle
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

/// Where a release line stands on `today`, with `warn_days` of notice before it ends.
pub fn standing(eol: &EolDate, today: NaiveDate, warn_days: i64) -> Standing {
    match eol {
        EolDate::Supported | EolDate::Unreadable(_) => Standing::Supported,
        EolDate::Ended => Standing::Ended { since: None },
        EolDate::On(date) => {
            let Ok(on) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
                return Standing::Supported;
            };
            let days = (on - today).num_days();
            if days < 0 {
                Standing::Ended {
                    since: Some(date.clone()),
                }
            } else if days <= warn_days {
                Standing::EndsSoon {
                    on: date.clone(),
                    days,
                }
            } else {
                Standing::Supported
            }
        }
    }
}
