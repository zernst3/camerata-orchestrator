//! The seam a client export calls later. Nothing in this crate is wired into any export today;
//! this is the function the export builder will call per rule, plus the line it prints.
//!
//! Policy (from the work order): a dead or changed source lowers that rule's confidence in client
//! reports and adds a "source re-checked <date>" line. It never disables the rule.
//!
//! It reads the sidecar, not the report, so it works from whatever the last run left behind and
//! needs no network.

use camerata_rules::{days_from_civil, parse_ymd, Rule};

use crate::sidecar::{LinkState, SourceHashes};

/// How long a section change keeps lowering confidence after the run that saw it. The sidecar
/// records when a section last changed, not when someone reviewed it, so a change ages out.
pub const DEFAULT_CHANGED_WINDOW_DAYS: i64 = 100;

/// What the sidecar says about one rule's sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConfidence {
    /// The oldest `last_checked` among the rule's web sources (the date every one was last
    /// re-checked by). `None` when the rule has no checked web source.
    pub rechecked_on: Option<String>,
    /// True when confidence should be lowered: a source is dead, or a cited section changed
    /// within the window.
    pub lowered: bool,
    /// Why, one short phrase per cause, for the export's detail line.
    pub reasons: Vec<String>,
}

impl SourceConfidence {
    /// The line the export prints, or `None` when no web source of this rule has been checked.
    pub fn line(&self) -> Option<String> {
        let date = self.rechecked_on.as_ref()?;
        Some(format!("Source re-checked {date}"))
    }
}

/// The source-freshness standing of `rule` as of `today` (`YYYY-MM-DD`), judged from `hashes`.
/// `changed_window_days` is normally [`DEFAULT_CHANGED_WINDOW_DAYS`].
pub fn source_confidence(
    rule: &Rule,
    hashes: &SourceHashes,
    today: &str,
    changed_window_days: i64,
) -> SourceConfidence {
    let today_days = parse_ymd(today).map(|(y, m, d)| days_from_civil(y, m, d));
    let mut rechecked_on: Option<&str> = None;
    let mut reasons = Vec::new();
    for source in &rule.sources {
        let url = source.url.trim();
        let Some(record) = hashes.entries.get(url) else {
            continue;
        };
        if rechecked_on.is_none_or(|oldest| record.last_checked.as_str() < oldest) {
            rechecked_on = Some(&record.last_checked);
        }
        if record.link == LinkState::Dead {
            reasons.push(format!("source link is dead: {url}"));
        }
        let changed_recently = match (
            record.last_changed.as_deref().and_then(parse_ymd),
            today_days,
        ) {
            (Some((y, m, d)), Some(now)) => now - days_from_civil(y, m, d) <= changed_window_days,
            _ => false,
        };
        if changed_recently {
            reasons.push(format!("cited section changed: {url}"));
        }
    }
    SourceConfidence {
        rechecked_on: rechecked_on.map(str::to_string),
        lowered: !reasons.is_empty(),
        reasons,
    }
}
