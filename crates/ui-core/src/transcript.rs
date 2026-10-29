//! The live transcript of a model's output, split into what the model said and the notes the
//! engine put between its attempts.
//!
//! When a model call is run again after an attempt that had already streamed text, the server
//! streams [`CLI_ATTEMPT_SUPERSEDED`] into the transcript so the earlier text is not mistaken for
//! part of the answer. The cockpit used to print that marker as a raw line; it shows a short muted
//! note instead ([`SUPERSEDED_NOTE`]).

use camerata_api_types::llm::CLI_ATTEMPT_SUPERSEDED;

/// What the cockpit shows where an attempt was superseded.
pub const SUPERSEDED_NOTE: &str = "retrying; the text above was superseded";

/// One stretch of a transcript's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPart {
    /// Text the model streamed.
    Text(String),
    /// The earlier text came from an attempt that failed; the model was asked again.
    Superseded,
}

/// `output` split at every superseded-attempt marker. Empty stretches are dropped, so a transcript
/// with no marker is one `Text` part (or none when it is empty) and the same text as before.
pub fn split_output(output: &str) -> Vec<OutputPart> {
    let mut parts = Vec::new();
    let mut rest = output;
    loop {
        match rest.find(CLI_ATTEMPT_SUPERSEDED) {
            Some(at) => {
                if at > 0 {
                    parts.push(OutputPart::Text(rest[..at].to_string()));
                }
                parts.push(OutputPart::Superseded);
                rest = &rest[at + CLI_ATTEMPT_SUPERSEDED.len()..];
            }
            None => {
                if !rest.is_empty() {
                    parts.push(OutputPart::Text(rest.to_string()));
                }
                return parts;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_without_a_marker_is_one_text_part_unchanged() {
        assert_eq!(
            split_output("PARTIAL answer"),
            vec![OutputPart::Text("PARTIAL answer".to_string())]
        );
        assert!(split_output("").is_empty());
    }

    #[test]
    fn the_marker_becomes_a_note_between_the_two_attempts_and_is_never_shown_raw() {
        let output = format!("PARTIAL{CLI_ATTEMPT_SUPERSEDED}RECOVERED");
        let parts = split_output(&output);
        assert_eq!(
            parts,
            vec![
                OutputPart::Text("PARTIAL".to_string()),
                OutputPart::Superseded,
                OutputPart::Text("RECOVERED".to_string()),
            ]
        );
        for part in &parts {
            if let OutputPart::Text(text) = part {
                assert!(
                    !text.contains("came from an attempt that failed"),
                    "{text:?}"
                );
            }
        }
    }

    #[test]
    fn two_retries_give_two_notes_and_a_marker_at_the_end_is_kept() {
        let output = format!("a{CLI_ATTEMPT_SUPERSEDED}b{CLI_ATTEMPT_SUPERSEDED}");
        assert_eq!(
            split_output(&output),
            vec![
                OutputPart::Text("a".to_string()),
                OutputPart::Superseded,
                OutputPart::Text("b".to_string()),
                OutputPart::Superseded,
            ]
        );
    }

    #[test]
    fn the_note_is_short_and_plain() {
        assert!(SUPERSEDED_NOTE.len() < 60);
        assert!(!SUPERSEDED_NOTE.contains('\u{2014}'));
    }
}
