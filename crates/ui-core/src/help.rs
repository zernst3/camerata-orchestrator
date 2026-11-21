//! Help topics: the explanatory text the screens keep out of the page and show in the help dialog.
//!
//! The text lives here as data, not in the screens, so tests can check it. One file per area holds
//! that area's topics.
//!
//! NOTE: work order B owns this module and builds the dialog that shows these topics. This copy is
//! the minimum work order E needs (the topic type and E's own file); B's merge replaces it.

pub mod export;
pub mod inspection;

/// One help topic: what the "?" button beside a heading or control opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpTopic {
    /// The id the button names (`export`).
    pub id: &'static str,
    /// The dialog's title, and the words after "About" in the button's accessible name.
    pub title: &'static str,
    /// The text. Paragraphs are separated by a blank line.
    pub body: String,
}

impl HelpTopic {
    /// The accessible name of the topic's button: `About <title>`.
    pub fn button_label(&self) -> String {
        format!("About {}", self.title)
    }
}
