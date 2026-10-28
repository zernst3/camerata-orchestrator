//! Client-facing fix text preparation: the ONE place a rule's authored `remediation` (and a
//! model-written `fix_specific`) is made presentable before any artifact renders it.
//!
//! Authored remediation lives in TOML multi-line strings, so it carries the source file's hard
//! line wraps (a newline every ~90 columns, mid-sentence) and inline-code spans in backticks.
//! Rendered as-is, the PDF card broke lines mid-sentence and printed the backticks literally.
//!
//! The contract:
//! - [`prepare`] joins every single newline into one space and keeps a blank line as a
//!   paragraph break (normalised to exactly `"\n\n"`). Backtick spans survive, so the PDF
//!   template (`fix_body` in `audit_report.typ`) can set them as inline mono.
//! - [`plain`] is [`prepare`] plus the backticks of well-formed single-backtick spans removed,
//!   for the plain-text artifacts (xlsx cells, CSV, findings.json) where backticks would read
//!   as stray punctuation.

/// Join hard line wraps into spaces, keep blank-line paragraph breaks, trim the ends.
pub(crate) fn prepare(text: &str) -> String {
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in normalised.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.join("\n\n")
}

/// [`prepare`], then drop the backticks around each single-backtick code span. A backtick that
/// is not part of a well-formed span (an unclosed one, or a run of two or more) is left alone,
/// mirroring exactly which spans the PDF template sets as mono.
pub(crate) fn plain(text: &str) -> String {
    let prepared = prepare(text);
    let mut out = String::with_capacity(prepared.len());
    let mut rest = prepared.as_str();
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        // A span is a non-empty run of non-backticks closed by the very next backtick.
        match after.find('`') {
            Some(n) if n > 0 => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..n]);
                rest = &after[n + 1..];
            }
            _ => {
                // Empty span or no closer: keep this backtick run verbatim.
                let run = rest[open..].chars().take_while(|c| *c == '`').count();
                out.push_str(&rest[..open + run]);
                rest = &rest[open + run..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hard_wrapped_remediation_becomes_one_paragraph() {
        let src = "Rotate the key immediately: treat it as\ncompromised, since it has shipped.\nRemove it from `.env`\nand `VITE_`,\nREACT_APP_.";
        assert_eq!(
            prepare(src),
            "Rotate the key immediately: treat it as compromised, since it has shipped. Remove it from `.env` and `VITE_`, REACT_APP_."
        );
    }

    #[test]
    fn a_blank_line_still_splits_paragraphs() {
        assert_eq!(
            prepare("one\nwrapped\n\ntwo\n\n\n\nthree"),
            "one wrapped\n\ntwo\n\nthree"
        );
        // Blank lines made of spaces and CRLF endings behave the same.
        assert_eq!(prepare("a\r\nb\r\n  \r\nc"), "a b\n\nc");
    }

    #[test]
    fn leading_and_trailing_whitespace_is_trimmed() {
        assert_eq!(prepare("\n\n  hello\nworld  \n\n"), "hello world");
        assert_eq!(prepare(""), "");
        assert_eq!(prepare("  \n \n"), "");
    }

    #[test]
    fn plain_text_strips_the_backticks_of_each_span_and_nothing_else() {
        assert_eq!(
            plain("Edit `apps/web/.env.production`\nand `VITE_`."),
            "Edit apps/web/.env.production and VITE_."
        );
    }

    #[test]
    fn plain_text_keeps_backticks_that_are_not_a_span() {
        // Unclosed, empty and triple runs are not spans, and the template leaves them literal.
        assert_eq!(plain("a ` b"), "a ` b");
        assert_eq!(plain("a `` b"), "a `` b");
        assert_eq!(plain("add ```rust fence"), "add ```rust fence");
    }

    #[test]
    fn plain_text_has_no_backticks_for_the_service_role_remediation_shape() {
        let src = "Remove it from\n`apps/web/.env.production` and any client-bundle-prefixed environment variable (`NEXT_PUBLIC_`, `VITE_`,\n`REACT_APP_`, `EXPO_PUBLIC_`), and move it.";
        let out = plain(src);
        assert!(!out.contains('`'), "{out}");
        assert!(!out.contains('\n'), "{out}");
    }
}
