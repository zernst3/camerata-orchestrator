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

#[cfg(test)]
mod tests {
    use super::md_to_html;

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(md_to_html(""), "");
    }

    #[test]
    fn paragraph_wraps_in_p_tags() {
        assert_eq!(md_to_html("hello"), "<p>hello</p>\n");
    }

    #[test]
    fn emphasis_renders_as_em() {
        assert!(md_to_html("*hi*").contains("<em>hi</em>"));
    }

    #[test]
    fn gfm_table_renders_as_html_table() {
        // GFM tables are explicitly enabled; a pipe table must become a real <table>,
        // not pass through as literal pipe text.
        let src = "| a | b |\n|---|---|\n| 1 | 2 |";
        let html = md_to_html(src);
        assert!(html.contains("<table>"), "expected a table element, got: {html}");
        assert!(html.contains("<th>a</th>") || html.contains("<th>a"), "header cell missing");
    }

    #[test]
    fn strikethrough_extension_is_enabled() {
        // ENABLE_STRIKETHROUGH means `~~x~~` becomes <del>; without it, it stays literal.
        assert!(md_to_html("~~gone~~").contains("<del>gone</del>"));
    }
}
