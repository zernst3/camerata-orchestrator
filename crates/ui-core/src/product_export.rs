//! The product-export request: what the export dialog sends to `POST
//! /api/projects/:id/product-export`. Pure, so the options that reach the server are testable
//! without a renderer.
//!
//! The client name is deliberately not part of the request: the server reads it from the
//! project's inspection setup, so every export names the same client.

use serde::Serialize;

/// The report options the export dialog collects. Every field is optional framing except the
/// switches, which default to off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductExportOptions {
    /// Shown on the cover when set.
    pub project_title: String,
    /// The preparer's name on the sign-off block.
    pub prepared_by: String,
    /// Replaces the generated executive summary verbatim when `Some`.
    pub executive_summary_override: Option<String>,
    /// Also export the triage log: per rule, what the AI pass proposed, what review kept and
    /// threw out, and why. A workbook sheet plus `csv/triage-log.csv`. Off by default.
    pub triage_log: bool,
}

/// The JSON body of the export request: the dispositions map and the `options` object.
/// `ReportOptions` on the server deserializes `options` (every key has a server-side default).
pub fn request_body<D: Serialize>(
    dispositions: &D,
    options: &ProductExportOptions,
) -> serde_json::Value {
    serde_json::json!({
        "dispositions": dispositions,
        "options": {
            "project_title": options.project_title,
            "prepared_by": options.prepared_by,
            "executive_summary_override": options.executive_summary_override,
            "triage_log": options.triage_log,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn body(options: &ProductExportOptions) -> serde_json::Value {
        let dispositions: HashMap<String, String> = HashMap::new();
        request_body(&dispositions, options)
    }

    #[test]
    fn the_triage_log_is_off_unless_the_box_is_ticked() {
        let value = body(&ProductExportOptions::default());
        assert_eq!(value["options"]["triage_log"], serde_json::json!(false));
    }

    #[test]
    fn a_ticked_triage_log_box_reaches_the_request() {
        let value = body(&ProductExportOptions {
            triage_log: true,
            ..ProductExportOptions::default()
        });
        assert_eq!(value["options"]["triage_log"], serde_json::json!(true));
    }

    #[test]
    fn the_other_options_and_the_dispositions_ride_along_and_no_client_name_is_sent() {
        let mut dispositions = HashMap::new();
        dispositions.insert("k".to_string(), "v".to_string());
        let value = request_body(
            &dispositions,
            &ProductExportOptions {
                project_title: "T".to_string(),
                prepared_by: "P".to_string(),
                executive_summary_override: Some("S".to_string()),
                triage_log: false,
            },
        );
        assert_eq!(value["dispositions"]["k"], "v");
        assert_eq!(value["options"]["project_title"], "T");
        assert_eq!(value["options"]["prepared_by"], "P");
        assert_eq!(value["options"]["executive_summary_override"], "S");
        assert!(value["options"].get("client_name").is_none());
    }
}
