//! Shared markdown-to-HTML renderer. Used by the chat bubble and the docs view.
//!
//! GFM tables + strikethrough enabled so rule tables and doc tables render as
//! actual HTML tables rather than pipe-delimited text.

use pulldown_cmark::{html, Options, Parser};

/// Render `src` (Markdown) to an HTML string. Safe for use with `dangerous_inner_html`.
pub fn md_to_html(src: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(src, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}
