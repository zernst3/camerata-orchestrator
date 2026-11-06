//! M4's prose checks, applied to text the group reviewer wrote before it reaches client output
//! (design 3B item 1, amendment A8).
//!
//! A node's summary, label and cause are model prose written for the architect, not for the
//! client. Before one is printed in a client file (the PDF's patterns, the Tree and Remediation
//! plan sheets, `findings.json`'s tree, a ticket) it is held to the checks a finding's own text is:
//!
//! - it must not describe how Camerata found the thing ([`crate::ai_audit::fix_leaks_methodology`]
//!   is the one list of those phrases);
//! - it must not be a model's reply to the user, an apology or an untranslated JSON fragment
//!   ([`crate::junk_finding::classify_row`]).
//!
//! A hit is a review flag shown to the architect, never a hold (A8): the export goes ahead, and the
//! client file carries no text from the flagged field. The architect sees the flag beside the node
//! on the internal workbook and edits the text.

/// A node text as client output may carry it, and the flags it earned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedText {
    /// The text for client output: the original when it passed, empty when it did not.
    pub text: String,
    /// One flag per hit, `<field>:<check>:<detail>`. Empty when the text passed.
    pub flags: Vec<String>,
}

/// The flags `text` earns, each named `check:detail`.
pub fn prose_flags(text: &str) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(phrase) = crate::ai_audit::fix_leaks_methodology(text) {
        flags.push(format!("methodology_leak:{phrase}"));
    }
    let content = crate::junk_finding::classify_row("", "", "", text);
    for reason in &content.junk {
        flags.push(format!("junk_text:{}", reason.describe()));
    }
    flags
}

/// `text` for client output: unchanged when it passes the checks, withheld (empty) when it does
/// not, with the flags that withheld it named for `field`.
pub fn client_text(field: &str, text: &str) -> CheckedText {
    let flags: Vec<String> = prose_flags(text)
        .into_iter()
        .map(|flag| format!("{field}:{flag}"))
        .collect();
    CheckedText {
        text: if flags.is_empty() {
            text.to_string()
        } else {
            String::new()
        },
        flags,
    }
}
