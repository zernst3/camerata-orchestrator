//! The cockpit's shape of the operator's rewrite advisory (`GET /api/projects/:id/rewrite-advisory`)
//! and the words of its internal panel.
//!
//! INTERNAL ONLY. Owner ruling: fix or rewrite is the architect's decision alone, and no client
//! file states or hints at it. The panel shows the server's fixed sentences and the numbers
//! behind them; this module adds no judgement of its own. Nothing here is part of an export
//! request ([`crate::product_export::request_body`] carries the dispositions and the report
//! options and nothing else, which a test pins), and every key the shape serializes is either on
//! the deny list in `camerata_api_types::rewrite_advisory` or one of its ordinary words, so the
//! release gate's tripwire knows every distinctive name.
//!
//! The shape is lenient (every field defaults), so a server that adds a number does not break an
//! older cockpit; the key test below then fails until the new key is listed.

use serde::{Deserialize, Serialize};

/// The heading of the panel.
pub const PANEL_TITLE: &str = "Rewrite advisory (internal)";
/// Its standing caveat.
pub const PANEL_NOTE: &str = "Internal note for the architect. It is not in any export or \
                              report a client receives. The decision is yours.";
/// What the panel says while the advisory is on its way, or when the server has none.
pub const PANEL_LOADING: &str = "Reading the advisory for the latest inspection.";

/// The document the route answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvisoryDocument {
    pub rewrite_advisory: RewriteAdvisoryView,
}

/// One guard that held.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardView {
    pub reason: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FixCountView {
    pub counted: usize,
    pub without_estimate: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FixHoursView {
    pub low_hours: u64,
    pub high_hours: u64,
    pub low: f64,
    pub high: f64,
    pub threshold: u64,
    pub over: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StructuralView {
    pub low_hours: u64,
    pub high_hours: u64,
    pub structural_low_hours: u64,
    pub structural_high_hours: u64,
    pub percent: f64,
    pub threshold: u64,
    pub over: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AreaView {
    pub category: String,
    pub assessed_rules: usize,
    pub unsettled_rules: usize,
    pub unsettled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnsettledView {
    pub assessed: usize,
    pub unsettled: usize,
    pub areas: Vec<AreaView>,
    pub threshold: u64,
    pub threshold_rules: u64,
    pub over: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CauseShareView {
    pub fix: String,
    pub places: usize,
    pub percent: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TopCausesView {
    pub fixes: Vec<String>,
    pub places: usize,
    pub percent: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepeatCauseView {
    pub total_places: usize,
    pub largest_high: Option<CauseShareView>,
    pub top_three: TopCausesView,
    pub threshold_one: u64,
    pub threshold_three: u64,
    pub over: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SignalsView {
    pub fix_hours_per_kloc: FixHoursView,
    pub structural_share: StructuralView,
    pub unsettled_areas: UnsettledView,
    pub repeat_cause_share: RepeatCauseView,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextView {
    pub critical_high_fixes: usize,
    pub per_kloc: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsView {
    pub min_code_lines: usize,
    pub max_unestimated_percent: u64,
    pub signals_to_fire: usize,
}

/// The advisory of the latest inspection.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RewriteAdvisoryView {
    pub run_id: String,
    pub finished_at: String,
    /// `fired`, `not_fired` or `not_enough_to_say`.
    pub outcome: String,
    pub signals_over: usize,
    pub code_lines: usize,
    pub files_not_reviewed: usize,
    pub guards: Vec<GuardView>,
    pub fixes: FixCountView,
    pub rewrite_signals: SignalsView,
    pub context: ContextView,
    pub limits: LimitsView,
    /// The server's fixed sentences, in order. The panel shows these and nothing it made up.
    pub lines: Vec<String>,
}

impl RewriteAdvisoryView {
    /// The panel's opening line (the server's first sentence), bold in the panel.
    pub fn opening(&self) -> &str {
        self.lines.first().map_or("", String::as_str)
    }

    /// The rest of the server's sentences, one paragraph each.
    pub fn rest(&self) -> &[String] {
        self.lines.get(1..).unwrap_or(&[])
    }

    /// Which inspection the advisory is for, in words.
    pub fn run_text(&self) -> String {
        match (self.run_id.is_empty(), self.finished_at.is_empty()) {
            (false, false) => format!("Inspection {}, finished {}.", self.run_id, self.finished_at),
            (false, true) => format!("Inspection {}.", self.run_id),
            (true, false) => format!("The latest inspection, finished {}.", self.finished_at),
            (true, true) => "The latest inspection.".to_string(),
        }
    }
}

/// What the panel shows: the advisory on its way, the advisory, or why there is none.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelState {
    Loading,
    Ready(RewriteAdvisoryView),
    Unavailable(String),
}

impl PanelState {
    /// The state a fetch's result puts the panel in.
    pub fn of(result: Result<RewriteAdvisoryView, String>) -> Self {
        match result {
            Ok(view) => PanelState::Ready(view),
            Err(message) => PanelState::Unavailable(message),
        }
    }
}

/// The advisory in a route's answer, or why it could not be read: the server's own message when
/// it gave one.
pub fn parse_response(status_ok: bool, body: &str) -> Result<RewriteAdvisoryView, String> {
    if !status_ok {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "The advisory could not be read.".to_string());
        return Err(message);
    }
    serde_json::from_str::<AdvisoryDocument>(body)
        .map(|d| d.rewrite_advisory)
        .map_err(|e| format!("The advisory could not be read: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::rewrite_advisory::{terms_in, GENERIC_KEYS, INTERNAL_ONLY_KEYS};
    use std::collections::BTreeSet;

    /// A document as the server writes it, every field present.
    const SAMPLE: &str = r#"{
      "rewrite_advisory": {
        "run_id": "run-1",
        "finished_at": "2026-11-15T10:00:00Z",
        "outcome": "fired",
        "signals_over": 2,
        "code_lines": 2000,
        "files_not_reviewed": 2,
        "guards": [{"reason": "few_lines", "detail": "d"}],
        "fixes": {"counted": 17, "without_estimate": 1},
        "rewrite_signals": {
          "fix_hours_per_kloc": {"low_hours": 32, "high_hours": 64, "low": 16.0, "high": 32.0, "threshold": 15, "over": true},
          "structural_share": {"low_hours": 32, "high_hours": 64, "structural_low_hours": 32, "structural_high_hours": 64, "percent": 100.0, "threshold": 40, "over": true},
          "unsettled_areas": {"assessed": 1, "unsettled": 0, "areas": [{"category": "Testing", "assessed_rules": 10, "unsettled_rules": 3, "unsettled": false}], "threshold": 50, "threshold_rules": 30, "over": false},
          "repeat_cause_share": {"total_places": 16, "largest_high": {"fix": "f1", "places": 3, "percent": 18.8}, "top_three": {"fixes": ["f1"], "places": 3, "percent": 18.8}, "threshold_one": 25, "threshold_three": 50, "over": false}
        },
        "context": {"critical_high_fixes": 4, "per_kloc": 2.0},
        "limits": {"min_code_lines": 2000, "max_unestimated_percent": 20, "signals_to_fire": 2},
        "lines": ["Worth a conversation: fix or rewrite. Two of four signals are over their starting thresholds.", "Fix hours against code size: 16.0 to 32.0 hours per 1,000 counted lines."]
      }
    }"#;

    fn keys(v: &serde_json::Value, out: &mut BTreeSet<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, item) in map {
                    out.insert(k.clone());
                    keys(item, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
            _ => {}
        }
    }

    #[test]
    fn the_servers_document_reads_whole_and_writes_back_the_same() {
        let doc: AdvisoryDocument = serde_json::from_str(SAMPLE).expect("parses");
        let a = &doc.rewrite_advisory;
        assert_eq!(a.outcome, "fired");
        assert_eq!(
            a.rewrite_signals.unsettled_areas.areas[0].category,
            "Testing"
        );
        assert_eq!(
            a.rewrite_signals
                .repeat_cause_share
                .largest_high
                .as_ref()
                .map(|c| c.places),
            Some(3)
        );
        let sent: serde_json::Value = serde_json::from_str(SAMPLE).expect("json");
        let back: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&doc).expect("serializes")).expect("json");
        assert_eq!(sent, back, "no field of the server's document is dropped");
    }

    /// Every key the shape serializes is deny-listed or an ordinary word, so a field added here
    /// without being listed fails, and the gate's tripwire knows every distinctive name.
    #[test]
    fn every_serialized_key_is_on_the_deny_list_or_an_ordinary_word() {
        let mut doc = AdvisoryDocument::default();
        doc.rewrite_advisory.guards.push(GuardView::default());
        doc.rewrite_advisory
            .rewrite_signals
            .unsettled_areas
            .areas
            .push(AreaView::default());
        doc.rewrite_advisory
            .rewrite_signals
            .repeat_cause_share
            .largest_high = Some(CauseShareView::default());
        let value = serde_json::to_value(&doc).expect("serializes");
        let mut seen = BTreeSet::new();
        keys(&value, &mut seen);
        for key in &seen {
            assert!(
                INTERNAL_ONLY_KEYS.contains(&key.as_str()) || GENERIC_KEYS.contains(&key.as_str()),
                "{key} is serialized but neither deny-listed nor an ordinary word"
            );
        }
        for key in INTERNAL_ONLY_KEYS {
            assert!(
                seen.contains(*key),
                "{key} is listed but the shape never serializes it"
            );
        }
    }

    /// The panel's own words say it is internal, and they are themselves deny-listed, so a panel
    /// title that reached a client file would be refused.
    #[test]
    fn the_panel_says_internal_and_its_title_is_deny_listed() {
        assert!(PANEL_TITLE.contains("(internal)"));
        assert!(PANEL_NOTE.contains("not in any export"));
        assert!(!terms_in(PANEL_TITLE).is_empty());
        for text in [PANEL_TITLE, PANEL_NOTE, PANEL_LOADING] {
            assert!(
                !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                "{text}"
            );
            let lower = text.to_lowercase();
            for word in ["should", "recommend", "must"] {
                assert!(!lower.contains(word), "{word} in {text}");
            }
        }
    }

    #[test]
    fn the_panel_splits_the_opening_from_the_rest() {
        let doc: AdvisoryDocument = serde_json::from_str(SAMPLE).expect("parses");
        let a = doc.rewrite_advisory;
        assert!(a.opening().starts_with("Worth a conversation"));
        assert_eq!(a.rest().len(), 1);
        assert_eq!(
            a.run_text(),
            "Inspection run-1, finished 2026-11-15T10:00:00Z."
        );
        let empty = RewriteAdvisoryView::default();
        assert_eq!(empty.opening(), "");
        assert!(empty.rest().is_empty());
        assert_eq!(empty.run_text(), "The latest inspection.");
    }

    #[test]
    fn a_refusal_carries_the_servers_message() {
        assert_eq!(
            parse_response(
                false,
                r#"{"ok":false,"code":"no_inspection","message":"Run an audit first."}"#
            ),
            Err("Run an audit first.".to_string())
        );
        assert_eq!(
            parse_response(false, "not json"),
            Err("The advisory could not be read.".to_string())
        );
        assert_eq!(
            parse_response(true, SAMPLE).map(|a| a.outcome),
            Ok("fired".to_string())
        );
    }
}
