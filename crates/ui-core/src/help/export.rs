//! The help text of the export panel (work order E): what the client package holds and how the
//! export behaves. Built from [`crate::export_panel`]'s constants, which a server test checks
//! against the package itself, so the text cannot name a file or section the package lacks.

use super::HelpTopic;
use crate::export_panel;

/// The id of the export panel's topic.
pub const EXPORT_TOPIC_ID: &str = "export";

/// The export panel's help topic.
pub fn export_topic() -> HelpTopic {
    HelpTopic {
        id: EXPORT_TOPIC_ID,
        title: export_panel::HELP_TITLE,
        body: export_panel::help_body(),
    }
}

/// Every topic this file owns.
pub fn topics() -> Vec<HelpTopic> {
    vec![export_topic()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_export_topic_is_named_and_has_a_body() {
        let t = export_topic();
        assert_eq!(t.id, "export");
        assert_eq!(t.button_label(), "About Export product");
        assert!(t.body.contains("Area scorecard"), "{}", t.body);
    }

    #[test]
    fn no_topic_text_has_a_dash_or_a_serial_comma() {
        for t in topics() {
            let text = format!("{} {}", t.title, t.body);
            assert!(!text.contains(['\u{2013}', '\u{2014}']), "{text}");
            assert!(!text.contains(", and "), "{text}");
        }
    }
}
