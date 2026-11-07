//! The house punctuation rule for text a client reads: no em dash (U+2014), no en dash (U+2013)
//! and no multiplication sign (U+00D7).
//!
//! Two jobs, one list of characters:
//!
//! * [`plain`] rewrites the text the models write (a headline, a detail, a fix, a summary) as it
//!   is parsed, so a reply with an em dash ships without one. The rewrite reads the sentence: a
//!   dash that joins a title to its explanation becomes a colon, a pair of dashes around an aside
//!   become commas, a dash inside a number range becomes a hyphen.
//! * [`banned_in`] finds the characters again in any finished client file. The release gate
//!   refuses an export that still carries one (`release_gate::text_style`), whatever put it
//!   there.

use std::borrow::Cow;

/// The characters a client file must not carry, each with the name the gate prints.
pub const BANNED: [(char, &str); 3] = [
    ('\u{2014}', "em dash"),
    ('\u{2013}', "en dash"),
    ('\u{00D7}', "multiplication sign"),
];

/// Whether `c` is one of the [`BANNED`] characters.
pub fn is_banned(c: char) -> bool {
    BANNED.iter().any(|(banned, _)| *banned == c)
}

/// The name of the banned character `c` (`em dash`), or `None` for any other character.
pub fn banned_name(c: char) -> Option<&'static str> {
    BANNED
        .iter()
        .find(|(banned, _)| *banned == c)
        .map(|(_, name)| *name)
}

/// Every banned character in `text`, in order, with its byte offset.
pub fn banned_in(text: &str) -> Vec<(usize, char)> {
    text.char_indices().filter(|(_, c)| is_banned(*c)).collect()
}

/// `text` with every banned character rewritten. Borrows unchanged when there is none (the
/// common case), so it is free to call on every string. Idempotent.
///
/// The rewrite reads the sentence, so it is a judgment about punctuation and never a blind
/// substitution:
///
/// * two spaced dashes in one sentence enclose an aside, and both become commas;
/// * one spaced dash joins two parts: a colon when what follows starts a new statement (a capital
///   letter or a digit), a comma when it continues the sentence;
/// * a dash with no spaces becomes a comma and a space, except inside a range or a compound
///   (`10-20`, `client-server`), where it is a hyphen;
/// * a dash at either end of the text is dropped;
/// * a multiplication sign between spaces is "by" (`Severity by effort`), anywhere else `x`.
pub fn plain(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_banned) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(rewrite(text))
}

fn is_dash(c: char) -> bool {
    c == '\u{2014}' || c == '\u{2013}'
}

/// Whether the sentence that holds `chars[from..]` ends before `at`: a full stop, question mark
/// or exclamation mark followed by white space, or a line break.
fn sentence_ends(chars: &[char], from: usize, at: usize) -> bool {
    (from..at).any(|k| {
        chars[k] == '\n'
            || (matches!(chars[k], '.' | '!' | '?')
                && chars.get(k + 1).is_some_and(|next| next.is_whitespace()))
    })
}

/// Whether another dash with white space on both sides follows `chars[i]` in the same sentence.
fn closes_an_aside(chars: &[char], i: usize) -> bool {
    let mut k = i + 1;
    while k < chars.len() {
        if is_dash(chars[k])
            && k > 0
            && chars[k - 1].is_whitespace()
            && chars.get(k + 1).is_some_and(|c| c.is_whitespace())
        {
            return !sentence_ends(chars, i + 1, k);
        }
        k += 1;
    }
    false
}

fn rewrite(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut inside_aside = false;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '\u{00D7}' {
            let spaced = i > 0
                && chars[i - 1].is_whitespace()
                && chars.get(i + 1).is_some_and(|next| next.is_whitespace());
            out.push_str(if spaced { "by" } else { "x" });
            i += 1;
            continue;
        }
        if !is_dash(c) {
            out.push(c);
            i += 1;
            continue;
        }
        let before = i.checked_sub(1).map(|k| chars[k]);
        let after = chars.get(i + 1).copied();
        let touches = |side: Option<char>| side.is_some_and(|ch| !ch.is_whitespace());
        // A range or a compound: a hyphen, nothing else.
        if touches(before)
            && touches(after)
            && before.is_some_and(char::is_alphanumeric)
            && after.is_some_and(char::is_alphanumeric)
            && (c == '\u{2013}' || before.is_some_and(|b| b.is_ascii_digit()))
            && (c == '\u{2013}' || after.is_some_and(|a| a.is_ascii_digit()))
        {
            out.push('-');
            i += 1;
            continue;
        }
        // From here the dash is a break in the sentence. Drop the white space either side of it.
        let spaced_both = !touches(before) && !touches(after);
        while out.ends_with(char::is_whitespace) {
            out.pop();
        }
        let mut next = i + 1;
        while next < n && chars[next].is_whitespace() {
            next += 1;
        }
        let at_start = out.is_empty();
        let at_end = next >= n;
        if at_start || at_end {
            i = next;
            continue;
        }
        let opens_aside = spaced_both && !inside_aside && closes_an_aside(&chars, i);
        let closes = spaced_both && inside_aside;
        if opens_aside {
            inside_aside = true;
            out.push_str(", ");
        } else if closes {
            inside_aside = false;
            out.push_str(", ");
        } else {
            let starts_statement =
                spaced_both && chars[next].is_uppercase() || chars[next].is_ascii_digit();
            out.push_str(if starts_statement { ": " } else { ", " });
        }
        i = next;
    }
    out
}

/// `path:0` read as a place is a problem in a whole file, which a client reads as "whole file":
/// `.env.production:0` becomes `.env.production (whole file)`. Only a colon and a lone zero after
/// a letter are rewritten (`main.rs:0`), so a time (`10:05`), a ratio (`1:0`) and a line number
/// (`main.rs:10`) are left alone. Borrows unchanged when there is none.
pub fn whole_file_places(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let hit = |i: usize| {
        bytes[i] == b':'
            && i > 0
            && bytes[i - 1].is_ascii_alphabetic()
            && bytes.get(i + 1) == Some(&b'0')
            && !bytes
                .get(i + 2)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b':')
    };
    if !(0..bytes.len()).any(hit) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut last = 0;
    for i in 0..bytes.len() {
        if hit(i) {
            out.push_str(&text[last..i]);
            out.push_str(" (whole file)");
            last = i + 2;
        }
    }
    out.push_str(&text[last..]);
    Cow::Owned(out)
}

/// Whether `text` opens with a one-word label and a colon (`Borderline: ...`).
pub fn opens_with_label(text: &str) -> bool {
    let word: String = text
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
    !word.is_empty()
        && word.chars().count() <= 12
        && text.trim_start()[word.len()..].starts_with(':')
}

/// Openings that are the engine talking to itself, never a headline a client should read: a
/// diagnostic note about the rule's own evidence, a hedge label, a held-for-review marker. The
/// release gate refuses a client headline that starts with one.
pub const BANNED_HEADLINE_PREFIXES: [&str; 3] = [
    "This rule's own evaluation evidence",
    "Borderline:",
    "Needs review",
];

/// The banned opening of `headline`, when it has one (case does not matter).
pub fn banned_headline_prefix(headline: &str) -> Option<&'static str> {
    let lower = headline.trim_start().to_lowercase();
    BANNED_HEADLINE_PREFIXES
        .into_iter()
        .find(|prefix| lower.starts_with(&prefix.to_lowercase()))
}

/// `headline` without a hedge label in front of it: `Borderline: the policy is open` reads
/// `The policy is open`. A headline that starts with no label is returned as it is.
pub fn without_hedge_label(headline: &str) -> Cow<'_, str> {
    let trimmed = headline.trim_start();
    let Some(rest) = trimmed
        .get(..BORDERLINE.len())
        .filter(|head| head.eq_ignore_ascii_case(BORDERLINE))
        .map(|_| trimmed[BORDERLINE.len()..].trim_start())
        .filter(|rest| !rest.is_empty())
    else {
        return Cow::Borrowed(headline);
    };
    let mut chars = rest.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
    Cow::Owned(format!("{}{}", first.unwrap_or_default(), chars.as_str()))
}

const BORDERLINE: &str = "Borderline:";

/// A rule id written as a plain name for the places a rule has no title of its own:
/// `AI-PUBLIC-STORAGE-BUCKET` reads `Public storage bucket`, `SEC-NO-RAW-SQL-1` reads
/// `Sec no raw sql`. The `AI-` mark and a trailing version number are dropped, the words are
/// lower case and the first is capitalised. An id with nothing left comes back as it was.
pub fn plain_rule_name(rule_id: &str) -> String {
    let mut words: Vec<&str> = rule_id.split('-').filter(|w| !w.is_empty()).collect();
    if words.first().is_some_and(|w| w.eq_ignore_ascii_case("ai")) && words.len() > 1 {
        words.remove(0);
    }
    if words.len() > 1
        && words
            .last()
            .is_some_and(|w| w.chars().all(|c| c.is_ascii_digit()))
    {
        words.pop();
    }
    if words.is_empty() {
        return rule_id.to_string();
    }
    let lower = words.join(" ").to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => rule_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EM: char = '\u{2014}';
    const EN: char = '\u{2013}';
    const TIMES: char = '\u{00D7}';

    fn p(text: &str) -> String {
        plain(text).into_owned()
    }

    #[test]
    fn a_text_with_none_of_them_is_borrowed_unchanged() {
        assert!(matches!(
            plain("Plain text, with a: colon."),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_dash_between_a_title_and_its_explanation_becomes_a_colon() {
        assert_eq!(
            p(&format!(
                "Service role key under NEXT_PUBLIC_ prefix {EM} Line 6 defines it"
            )),
            "Service role key under NEXT_PUBLIC_ prefix: Line 6 defines it"
        );
    }

    #[test]
    fn a_dash_before_a_lower_case_clause_becomes_a_comma() {
        assert_eq!(
            p(&format!(
                "app/messages/page.tsx:15 {EM} an unbounded list query"
            )),
            "app/messages/page.tsx:15, an unbounded list query"
        );
    }

    #[test]
    fn a_pair_of_dashes_around_an_aside_becomes_a_pair_of_commas() {
        assert_eq!(
            p(&format!(
                "The policy {EM} which allows anyone {EM} exposes the rows"
            )),
            "The policy, which allows anyone, exposes the rows"
        );
    }

    #[test]
    fn a_dash_with_no_spaces_becomes_a_comma_and_a_space() {
        assert_eq!(p(&format!("fast{EM}but fragile")), "fast, but fragile");
    }

    #[test]
    fn an_en_dash_in_a_number_range_becomes_a_hyphen() {
        assert_eq!(p(&format!("lines 10{EN}20")), "lines 10-20");
        assert_eq!(p(&format!("client{EN}server")), "client-server");
    }

    #[test]
    fn a_spaced_en_dash_reads_as_an_em_dash() {
        assert_eq!(p(&format!("Title {EN} Detail")), "Title: Detail");
    }

    #[test]
    fn a_multiplication_sign_becomes_by_or_x() {
        assert_eq!(p(&format!("Severity {TIMES} effort")), "Severity by effort");
        assert_eq!(p(&format!("a 3{TIMES}3 grid")), "a 3x3 grid");
    }

    #[test]
    fn a_dash_at_either_end_of_the_text_is_dropped() {
        assert_eq!(p(&format!("{EM} leading")), "leading");
        assert_eq!(p(&format!("trailing {EM}")), "trailing");
    }

    #[test]
    fn the_rewrite_leaves_none_of_them_and_is_idempotent() {
        let messy =
            format!("A {EM} b{EM}c {EN} d 1{EN}2 e {TIMES} f {EM} g {EM} h. New {EM} sentence");
        let once = p(&messy);
        assert!(banned_in(&once).is_empty(), "{once}");
        assert_eq!(p(&once), once);
    }

    #[test]
    fn a_problem_in_a_whole_file_is_read_as_the_whole_file() {
        assert_eq!(
            whole_file_places("`.env.production:0` is marked as an outlier").as_ref(),
            "`.env.production (whole file)` is marked as an outlier"
        );
        assert_eq!(
            whole_file_places("see main.rs:0, then lib.rs:10 at 10:05 and a 1:0 ratio").as_ref(),
            "see main.rs (whole file), then lib.rs:10 at 10:05 and a 1:0 ratio"
        );
        assert!(matches!(whole_file_places("main.rs:10"), Cow::Borrowed(_)));
        assert!(matches!(whole_file_places("main.rs:07"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_rule_id_written_as_a_name_has_no_id_marks() {
        assert_eq!(
            plain_rule_name("AI-PUBLIC-STORAGE-BUCKET"),
            "Public storage bucket"
        );
        assert_eq!(plain_rule_name("SEC-NO-RAW-SQL-1"), "Sec no raw sql");
        assert_eq!(plain_rule_name("---"), "---");
    }

    #[test]
    fn a_banned_headline_opening_is_found_in_any_case() {
        assert!(banned_headline_prefix("Borderline: the policy is open").is_some());
        assert!(banned_headline_prefix("  needs review (uncited): x").is_some());
        assert!(
            banned_headline_prefix("This rule's own evaluation evidence reads \"x\".").is_some()
        );
        assert!(banned_headline_prefix("The policy is open to anyone").is_none());
    }

    #[test]
    fn a_hedge_label_in_front_of_a_headline_is_dropped() {
        assert_eq!(
            without_hedge_label("Borderline: the policy is open").as_ref(),
            "The policy is open"
        );
        assert!(matches!(
            without_hedge_label("The policy is open"),
            Cow::Borrowed(_)
        ));
        assert_eq!(without_hedge_label("Borderline:").as_ref(), "Borderline:");
    }

    #[test]
    fn a_one_word_label_with_a_colon_is_a_label() {
        assert!(opens_with_label("Borderline: the policy"));
        assert!(!opens_with_label("The policy: open"));
        assert!(!opens_with_label("no label here"));
    }

    #[test]
    fn banned_in_finds_each_character_with_its_offset() {
        let text = format!("a{EM}b{EN}c{TIMES}");
        let found = banned_in(&text);
        assert_eq!(
            found.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
            [EM, EN, TIMES]
        );
        assert_eq!(banned_name(EM), Some("em dash"));
        assert_eq!(banned_name('-'), None);
    }
}
