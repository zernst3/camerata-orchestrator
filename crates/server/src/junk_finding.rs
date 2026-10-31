//! The content test for a junk model finding (defect D1): a row with no evidence and no
//! explanation, a placeholder where a title should be, or a detail that is itself the model's
//! verdict that the row should be thrown away.
//!
//! A scan reply once carried a row with rule `JAVASCRIPT-TYPESRIPT-PLACEHOLDER`, title
//! `placeholder`, code `x` and detail `x`. Calibration and the detail step both said to discard
//! it; nothing could, and it shipped in the PDF, the workbook and `findings.json`. Three layers
//! now hold it back, all reading THIS one definition so they cannot drift apart:
//!
//! 1. scan parsing ([`parse_gate_reason`]) never lets such a row exist;
//! 2. after the detail and calibration steps ([`carries_discard_verdict`]) a model-invented row
//!    whose own text says "discard this finding" is removed and counted;
//! 3. the release gate ([`row_problem`]) refuses a package that still holds one.
//!
//! Only rows the model INVENTED (rule id prefixed `AI-`, so no adopted rule backs them) are held
//! to the discard-verdict test, so no prose can remove a row an adopted or deterministic rule
//! produced.

/// Words (lower case, after trimming) that stand where real text should be.
const PLACEHOLDER_WORDS: &[&str] = &[
    "x",
    "xx",
    "xxx",
    "n/a",
    "na",
    "none",
    "null",
    "nil",
    "tbd",
    "todo",
    "fixme",
    "placeholder",
    "lorem ipsum",
    "foo",
    "bar",
    "baz",
    "unknown",
    "...",
    "\u{2026}",
    "-",
    "--",
    "?",
    "??",
];

/// The text without the quotes, backticks, brackets and end punctuation a model wraps a filler
/// word in, lower-cased.
fn normalized(text: &str) -> String {
    text.trim()
        .trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '[' | ']' | '(' | ')' | '<' | '>')
        })
        .trim_end_matches(['.', '!', ';', ','])
        .trim()
        .to_lowercase()
}

/// Whether `text` is empty or stands in for text: a filler word (`x`, `n/a`, `tbd`, ...) or a
/// sentence that begins with "placeholder" or "lorem ipsum".
pub fn is_blank_or_placeholder(text: &str) -> bool {
    let t = normalized(text);
    if t.is_empty() || PLACEHOLDER_WORDS.contains(&t.as_str()) {
        return true;
    }
    t.starts_with("placeholder") || t.starts_with("lorem ipsum")
}

/// Whether a NON-EMPTY title is a placeholder word. An empty title is not (a row may have none).
pub fn is_placeholder_title(title: &str) -> bool {
    !title.trim().is_empty() && is_blank_or_placeholder(title)
}

/// Whether a rule id names a placeholder (`JAVASCRIPT-TYPESRIPT-PLACEHOLDER`): any hyphen
/// separated word of it is `PLACEHOLDER`.
pub fn is_placeholder_rule_id(rule_id: &str) -> bool {
    rule_id
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("placeholder"))
}

/// Why a freshly parsed scan row must not exist, or `None` when it may.
///
/// `invented` is whether the rule id is one the model made up (no adopted rule backs it). An
/// invented row is refused when its code and detail are both blank or placeholders, when its
/// title is a placeholder, or when its rule id names a placeholder. A row under an adopted rule
/// is refused only when ALL of its text is blank or placeholder, since that rule, not the text,
/// is what backs it.
pub fn parse_gate_reason(
    invented: bool,
    rule_id: &str,
    title: &str,
    code: &str,
    detail: &str,
) -> Option<&'static str> {
    let code_junk = is_blank_or_placeholder(code);
    let detail_junk = is_blank_or_placeholder(detail);
    let title_junk = is_blank_or_placeholder(title);
    if invented {
        if is_placeholder_rule_id(rule_id) {
            return Some("its rule id names a placeholder");
        }
        if is_placeholder_title(title) {
            return Some("its title is a placeholder");
        }
        if code_junk && detail_junk {
            return Some("it has no code and no explanation");
        }
        None
    } else if code_junk && detail_junk && title_junk {
        Some("it has no title, no code and no explanation")
    } else {
        None
    }
}

/// Whether model-written text is the model's own verdict that its row is to be thrown away:
/// "discard this finding", "placeholder finding", or both words "placeholder" and "discard".
pub fn carries_discard_verdict(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("discard this finding")
        || t.contains("placeholder finding")
        || (t.contains("placeholder") && t.contains("discard"))
}

/// Whether a model-invented row is junk after the detail and calibration steps: its own text
/// carries a discard verdict, its rule id names a placeholder, or its headline is a placeholder.
/// Evidence length is deliberately not tested here (the parse test already did that on the
/// model's own words; the steps after it write prose, not code).
pub fn is_junk_invented_row(rule_id: &str, headline: &str, detail: &str) -> bool {
    carries_discard_verdict(detail)
        || carries_discard_verdict(headline)
        || is_placeholder_rule_id(rule_id)
        || is_placeholder_title(headline)
}

/// What the release gate says about one exported row, or `None` when it is fine. Only rows whose
/// rule id starts with `AI-` (invented by the model) are tested.
pub fn row_problem(rule_id: &str, headline: &str, snippet: &str, detail: &str) -> Option<String> {
    if !rule_id.starts_with("AI-") {
        return None;
    }
    if carries_discard_verdict(detail) || carries_discard_verdict(headline) {
        return Some("its own text says to discard it".to_string());
    }
    // The exported snippet falls back to the title when the model gave no code, so a snippet
    // equal to the headline is "no code", not a second piece of evidence.
    let code = if snippet.trim() == headline.trim() {
        ""
    } else {
        snippet
    };
    parse_gate_reason(true, rule_id, headline, code, detail).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_words_and_single_characters_are_placeholders() {
        for t in [
            "",
            "  ",
            "x",
            "X",
            "\"x\"",
            "`x`",
            "N/A",
            "placeholder",
            "TBD.",
            "...",
            "-",
            "Placeholder text",
        ] {
            assert!(is_blank_or_placeholder(t), "{t:?}");
        }
    }

    #[test]
    fn real_text_is_not_a_placeholder() {
        for t in [
            "let x = 1;",
            "db.query(`select * from users where id = ${id}`)",
            "The handler trusts the client",
            "id",
            "fn",
        ] {
            assert!(!is_blank_or_placeholder(t), "{t:?}");
        }
    }

    #[test]
    fn the_incident_row_is_refused_at_parse() {
        assert!(parse_gate_reason(
            true,
            "AI-JAVASCRIPT-TYPESRIPT-PLACEHOLDER",
            "placeholder",
            "x",
            "x"
        )
        .is_some());
    }

    #[test]
    fn an_invented_row_with_real_code_and_a_placeholder_title_is_refused() {
        assert!(parse_gate_reason(
            true,
            "AI-NO-RAW-SQL",
            "todo",
            "db.query(sql)",
            "Raw SQL is built from user input."
        )
        .is_some());
    }

    #[test]
    fn an_invented_row_with_real_evidence_is_kept() {
        assert!(parse_gate_reason(
            true,
            "AI-NO-RAW-SQL",
            "Raw SQL",
            "db.query(sql)",
            "Raw SQL is built from user input."
        )
        .is_none());
        // Code alone, or an explanation alone, is enough evidence.
        assert!(parse_gate_reason(true, "AI-X-Y", "A title", "db.query(sql)", "").is_none());
        assert!(parse_gate_reason(true, "AI-X-Y", "A title", "", "A real explanation.").is_none());
    }

    #[test]
    fn an_adopted_rule_row_needs_all_three_texts_blank_to_be_refused() {
        assert!(parse_gate_reason(false, "SEC-A-1", "A real title", "", "").is_none());
        assert!(parse_gate_reason(false, "SEC-A-1", "", "x", "x").is_some());
    }

    #[test]
    fn the_discard_verdict_is_read_from_the_row_text() {
        assert!(carries_discard_verdict(
            "This finding contains only a placeholder statement. Discard this finding."
        ));
        assert!(carries_discard_verdict(
            "Placeholder finding with no concrete defect"
        ));
        assert!(!carries_discard_verdict(
            "The key is a placeholder in an example file."
        ));
        assert!(!carries_discard_verdict(
            "Raw SQL is built from user input."
        ));
    }

    #[test]
    fn only_invented_rows_are_tested_by_the_release_gate() {
        assert!(row_problem("SEC-A-1", "placeholder", "x", "x").is_none());
        assert!(row_problem("AI-A-B", "placeholder", "x", "x").is_some());
        assert!(row_problem("AI-A-B", "Raw SQL", "db.query(sql)", "Built from input.").is_none());
        assert!(row_problem("AI-A-B", "Raw SQL", "Raw SQL", "Built from input.").is_none());
        assert!(row_problem("AI-A-B", "Raw SQL", "Raw SQL", "").is_some());
        assert!(row_problem(
            "AI-A-B",
            "Raw SQL",
            "db.query(sql)",
            "Discard this finding."
        )
        .is_some());
    }
}
