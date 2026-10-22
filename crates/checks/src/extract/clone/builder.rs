//! The mode-aware token sink every dialect tokenizer writes into.
//!
//! A dialect walker decides, per source token, its class and the text it has in each mode, and
//! hands both to [`ItemBuilder::push`]. The builder keeps only the active mode's text, so the
//! Structural stream (the one indexed over a whole repo) never allocates a string per token.
//! Both modes see the same tokens in the same order: only the text differs, which is what lets
//! a [`super::CodeSpan`] found in Structural mode address the same tokens in Exact mode.

use std::ops::Range;

use super::{token_id, ItemTokens, Token, TokenClass, TokenMode};

/// Accumulates the items of one file.
pub(super) struct ItemBuilder {
    mode: TokenMode,
    items: Vec<ItemTokens>,
    current: Option<ItemTokens>,
}

impl ItemBuilder {
    pub(super) fn new(mode: TokenMode) -> Self {
        Self {
            mode,
            items: Vec::new(),
            current: None,
        }
    }

    /// Start a new item. A previous item still open is closed first.
    pub(super) fn begin_item(&mut self, kind: &'static str, name: Option<String>) {
        self.end_item();
        self.current = Some(ItemTokens {
            item: 0,
            kind,
            name,
            start_line: 0,
            end_line: 0,
            tokens: Vec::new(),
            texts: Vec::new(),
        });
    }

    /// Close the open item. An item that ended with no tokens (an import block, a comment) is
    /// dropped, so it also takes no item number.
    pub(super) fn end_item(&mut self) {
        let Some(mut item) = self.current.take() else {
            return;
        };
        let (Some(first), Some(last)) = (item.tokens.first(), item.tokens.last()) else {
            return;
        };
        item.start_line = first.line;
        item.end_line = last.line;
        item.item = u32::try_from(self.items.len()).unwrap_or(u32::MAX);
        self.items.push(item);
    }

    /// Emit one token whose text is the same in both modes.
    pub(super) fn push(&mut self, class: TokenClass, text: &str, line: u32, raw: Range<u32>) {
        self.push_split(class, text, || text.to_string(), line, raw);
    }

    /// Emit one token with a Structural text and an Exact text. `exact` runs only in Exact
    /// mode, so computing it (unescaping a string, canonicalizing a number) costs nothing on the
    /// repo-wide Structural pass.
    pub(super) fn push_split(
        &mut self,
        class: TokenClass,
        structural: &str,
        exact: impl FnOnce() -> String,
        line: u32,
        raw: Range<u32>,
    ) {
        let Some(item) = self.current.as_mut() else {
            return;
        };
        match self.mode {
            TokenMode::Structural => {
                item.tokens.push(Token {
                    id: token_id(class, structural),
                    class,
                    line,
                    raw,
                });
            }
            TokenMode::Exact => {
                let text = exact();
                item.tokens.push(Token {
                    id: token_id(class, &text),
                    class,
                    line,
                    raw,
                });
                item.texts.push(text);
            }
        }
    }

    pub(super) fn finish(mut self) -> Vec<ItemTokens> {
        self.end_item();
        self.items
    }
}
