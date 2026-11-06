//! The cockpit's read-only view of "Patterns across services": where the services of one
//! project differ on a rule. Mirrors `camerata_server::onboard::patterns_matrix`, which builds
//! it from the scan ledger and the findings; the cockpit only reads it.
//!
//! It is an observation, not a finding: no severity, no triage state, in no count. A service
//! where a rule does not apply, or was not reviewed, is shown as such and is never read as
//! clean. The tab exists only for a project of more than one repo.

use serde::{Deserialize, Serialize};

/// One service's cell in a pattern row: the ledger's cell state as the server serialized it
/// (`violated` with `sites`, `clean`, `not_applicable` or `not_reviewed` with a `reason`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PatternCellView {
    pub repo: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub sites: Option<usize>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl PatternCellView {
    /// The cell as the cockpit prints it: "Violated, 3 sites", "Clean", "Not applicable: reason"
    /// or "Not reviewed: reason". An unknown state prints as not reviewed, never as clean.
    pub fn label(&self) -> String {
        let reason = self.reason.as_deref().unwrap_or("no reason recorded");
        match self.state.as_str() {
            "violated" => {
                let n = self.sites.unwrap_or(0);
                format!("Violated, {n} {}", if n == 1 { "site" } else { "sites" })
            }
            "clean" => "Clean".to_string(),
            "not_applicable" => format!("Not applicable: {reason}"),
            _ => format!("Not reviewed: {reason}"),
        }
    }
}

/// One rule the services differ on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PatternRowView {
    pub rule_id: String,
    #[serde(default)]
    pub cells: Vec<PatternCellView>,
}

/// A group of rows a model pass found a common subject in.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PatternThemeView {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rule_ids: Vec<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub note: String,
}

/// The whole view. Empty (not present) for a project of one repo.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PatternsView {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub rows: Vec<PatternRowView>,
    #[serde(default)]
    pub themes: Vec<PatternThemeView>,
}

impl PatternsView {
    /// Whether the cockpit shows the tab at all: only a project of more than one repo has one.
    pub fn is_present(&self) -> bool {
        self.repos.len() > 1
    }

    /// The number beside the tab: the rows where the services differ.
    pub fn count(&self) -> usize {
        self.rows.len()
    }

    /// The rows the tab shows, each with the title it prints. With the rule library loaded
    /// (`titles`: rule id to title) a row is shown under its rule's title and a row whose rule
    /// is not in the library is dropped, exactly as the report's section does, so the two
    /// agree. With no library loaded yet a row is shown under its rule id and none is dropped.
    pub fn rows_with_titles<'a>(
        &'a self,
        titles: Option<&std::collections::HashMap<String, String>>,
    ) -> Vec<(&'a PatternRowView, String)> {
        self.rows
            .iter()
            .filter_map(|row| match titles {
                Some(map) => map
                    .iter()
                    .find(|(id, _)| id.eq_ignore_ascii_case(&row.rule_id))
                    .map(|(_, title)| (row, title.clone())),
                None => Some((row, row.rule_id.clone())),
            })
            .collect()
    }

    /// The number beside the tab for the rows [`Self::rows_with_titles`] shows.
    pub fn count_with_titles(
        &self,
        titles: Option<&std::collections::HashMap<String, String>>,
    ) -> usize {
        self.rows_with_titles(titles).len()
    }
}

/// The fixed sentence that introduces a row. The same wording the report prints.
pub fn pattern_sentence(title: &str) -> String {
    format!(
        "These services handle {title} differently. If that is intended, record it as a \
         convention; if not, a shared library or a written standard would close the gap."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_of(ids: &[&str]) -> PatternsView {
        PatternsView {
            repos: vec!["a".to_string(), "b".to_string()],
            rows: ids
                .iter()
                .map(|id| PatternRowView {
                    rule_id: (*id).to_string(),
                    cells: Vec::new(),
                })
                .collect(),
            themes: Vec::new(),
        }
    }

    #[test]
    fn with_the_library_loaded_a_row_prints_its_title_and_an_unknown_rule_is_dropped() {
        let view = view_of(&["ARCH-PAGINATION-1", "GONE-1"]);
        let titles: std::collections::HashMap<String, String> = [(
            "ARCH-PAGINATION-1".to_string(),
            "Cursor pagination".to_string(),
        )]
        .into();
        let rows = view.rows_with_titles(Some(&titles));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "Cursor pagination");
        // The count beside the tab is the count of the rows shown, as in the report.
        assert_eq!(view.count_with_titles(Some(&titles)), 1);
        assert!(pattern_sentence(&rows[0].1).contains("Cursor pagination differently"));
    }

    #[test]
    fn with_no_library_loaded_every_row_shows_under_its_rule_id() {
        let view = view_of(&["ARCH-PAGINATION-1", "GONE-1"]);
        let rows = view.rows_with_titles(None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "ARCH-PAGINATION-1");
        assert_eq!(view.count_with_titles(None), 2);
    }

    #[test]
    fn the_server_json_round_trips_into_the_view() {
        let json = serde_json::json!({
            "repos": ["acme/web", "acme/api"],
            "rows": [{
                "rule_id": "ARCH-PAGINATION-1",
                "cells": [
                    {"repo": "acme/web", "state": "violated", "sites": 2},
                    {"repo": "acme/api", "state": "clean"},
                    {"repo": "acme/worker", "state": "not_applicable", "reason": "no files"},
                    {"repo": "acme/jobs", "state": "not_reviewed", "reason": "spend limit"}
                ]
            }],
            "themes": []
        });
        let view: PatternsView = serde_json::from_value(json).unwrap();
        assert!(view.is_present());
        assert_eq!(view.count(), 1);
        let labels: Vec<String> = view.rows[0]
            .cells
            .iter()
            .map(PatternCellView::label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "Violated, 2 sites",
                "Clean",
                "Not applicable: no files",
                "Not reviewed: spend limit"
            ]
        );
    }

    #[test]
    fn an_absent_section_is_not_present() {
        let view: PatternsView = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!view.is_present());
        assert_eq!(view.count(), 0);
    }

    #[test]
    fn an_unknown_state_is_never_read_as_clean() {
        let cell = PatternCellView {
            repo: "r".into(),
            state: "something_new".into(),
            ..PatternCellView::default()
        };
        assert!(cell.label().starts_with("Not reviewed"));
    }

    #[test]
    fn one_site_reads_as_singular() {
        let cell = PatternCellView {
            repo: "r".into(),
            state: "violated".into(),
            sites: Some(1),
            reason: None,
        };
        assert_eq!(cell.label(), "Violated, 1 site");
    }

    #[test]
    fn the_sentence_matches_the_reports_wording() {
        assert_eq!(
            pattern_sentence("pagination"),
            "These services handle pagination differently. If that is intended, record it as a \
             convention; if not, a shared library or a written standard would close the gap."
        );
    }
}
