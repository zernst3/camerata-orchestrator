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
//!    whose own text says "discard this finding" is marked thrown out and kept apart;
//! 3. the seal moves any such row that got that far aside the same way ([`take_thrown_out`]),
//!    and holds a row with broken text ([`mark_rows`], the [`JUNK_CONTENT_CAPTURE`] mark, read
//!    by `report_export::is_held_for_review`) until a reviewer has decided it
//!    (`release_gate::junk`).
//!
//! Only rows the model INVENTED (rule id prefixed `AI-`, so no adopted rule backs them) are held
//! to the discard-verdict test, so no prose can throw out a row an adopted or deterministic rule
//! produced.
//!
//! # Three outcomes for a row's text (M4, A10)
//!
//! [`classify_row`] sorts what it finds into three lists, and each has its own consequence:
//!
//! * **Thrown out** ([`DiscardReason`]) is a row that is not a finding at all: a placeholder rule
//!   id, a filler word where the title should be (`x`, `n/a`, `tbd`, ...), no code and no
//!   explanation, or the model's own verdict "discard this finding". The engine keeps the row,
//!   marks it `engine_discarded` and leaves it out of the client export (`crate::engine_discard`).
//!   It is not held, so it never blocks an export.
//! * **Held** ([`JunkReason`]) is broken text on a finding that may be real: an empty
//!   explanation, a refusal or chat lead-in where the explanation should be ("I cannot ...",
//!   "Here is the ..."), or an untranslated JSON fragment. A reviewer might never open the row,
//!   and a client would receive it, so the row is HELD: the release gate refuses the package
//!   until a reviewer decision exists for that row.
//! * **Wording judgments** ([`WordingFlag`]) are a reviewer's call on prose that is still a
//!   finding: a title or explanation that begins "Placeholder ..." as a sentence ("Placeholder
//!   credentials are committed"), and the phrase "placeholder finding" in an explanation that
//!   does not say to discard it. They become review flags on the row
//!   (`crate::review_flags`) and never hold or refuse anything.
//!
//! A row that is thrown out is not also held.

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
/// "discard this finding" or "placeholder finding" (the phrases the detail and calibration steps
/// wrote on the incident row). The two words apart are not a verdict: a real finding speaks of a
/// placeholder in the code or a value the code discards, and such a row must stay.
pub fn carries_discard_verdict(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("discard this finding") || t.contains("placeholder finding")
}

/// Whether a model-invented row is junk after the detail and calibration steps: its own text
/// carries a discard verdict, its rule id names a placeholder, or its headline is a placeholder.
/// Evidence length is deliberately not tested here (the parse test already did that on the
/// model's own words; the steps after it write prose, not code).
pub fn is_junk_invented_row(rule_id: &str, headline: &str, detail: &str) -> bool {
    says_discard(detail)
        || says_discard(headline)
        || is_placeholder_rule_id(rule_id)
        || (!headline.trim().is_empty() && is_filler(headline))
}

/// Whether `text` says, in so many words, to discard the row. The phrase "placeholder finding"
/// alone is a wording judgment ([`WordingFlag::PlaceholderFinding`]), not a verdict.
fn says_discard(text: &str) -> bool {
    text.to_lowercase().contains("discard this finding")
}

/// Whether `text` is empty or exactly a filler word (`x`, `n/a`, `tbd`, ...): no sentence, no
/// "placeholder ..." lead. The strict half of [`is_blank_or_placeholder`], the half that is a
/// fact about the text and not a judgment of its wording.
pub fn is_filler(text: &str) -> bool {
    let t = normalized(text);
    t.is_empty() || PLACEHOLDER_WORDS.contains(&t.as_str())
}

/// Why a row's text is broken: a row the release gate holds until a reviewer decides it. The row
/// may still be a real finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JunkReason {
    /// The explanation is empty or a filler word, on a row that does have code.
    EmptyExplanation,
    /// The text opens with a model refusal or chat lead-in.
    ModelRefusal,
    /// The text is an untranslated JSON fragment.
    JsonFragment,
}

impl JunkReason {
    /// The reason in a sentence, for the gate's problem line and the row's mark.
    pub fn describe(self) -> &'static str {
        match self {
            Self::EmptyExplanation => "its explanation is empty",
            Self::ModelRefusal => "its text is a model refusal or chat reply, not a finding",
            Self::JsonFragment => "its text is an untranslated JSON fragment",
        }
    }
}

/// Why a row is not a finding at all: the engine throws it out (keeps it, marks it, leaves it out
/// of the client export). It is never held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    /// The rule id names a placeholder (`JAVASCRIPT-TYPESRIPT-PLACEHOLDER`).
    PlaceholderRuleId,
    /// The title is exactly a filler word.
    PlaceholderTitle,
    /// No code and no explanation.
    NoEvidence,
    /// The text says "discard this finding".
    DiscardVerdict,
}

impl DiscardReason {
    /// The reason in words: the text written on the row's `engine_discarded` mark.
    pub fn describe(self) -> &'static str {
        match self {
            Self::PlaceholderRuleId => crate::engine_discard::PLACEHOLDER_RULE_ID,
            Self::PlaceholderTitle => crate::engine_discard::PLACEHOLDER_HEADLINE,
            Self::NoEvidence => "it has no code and no explanation",
            Self::DiscardVerdict => crate::engine_discard::DISCARD_VERDICT,
        }
    }
}

/// A wording judgment about a row's text: a review flag, never a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordingFlag {
    /// A title or explanation that begins "Placeholder ..." or "Lorem ipsum ..." as a sentence.
    PlaceholderLead,
    /// "placeholder finding" in an explanation that does not say to discard the row.
    PlaceholderFinding,
}

impl WordingFlag {
    /// The name the row's `review_flags` list carries.
    pub fn flag(self) -> &'static str {
        match self {
            Self::PlaceholderLead => "placeholder_wording",
            Self::PlaceholderFinding => "placeholder_finding_phrase",
        }
    }
}

/// What [`classify_row`] found in one row's text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RowContent {
    /// Broken text: the row is held.
    pub junk: Vec<JunkReason>,
    /// Not a finding: the row is thrown out and kept apart. A row with any of these is not held.
    pub discard: Vec<DiscardReason>,
    /// Wording judgments: the row is flagged.
    pub wording: Vec<WordingFlag>,
}

/// Lower-case openings that make a text a model's reply to the user and not a finding. Matched at
/// the start of the title, of the explanation, and of the part after the headline separator.
const REFUSAL_LEADS: &[&str] = &[
    "i cannot",
    "i can't",
    "i can\u{2019}t",
    "i'm sorry",
    "i am sorry",
    "i apologize",
    "i\u{2019}m sorry",
    "sorry, ",
    "i'm unable",
    "i am unable",
    "i\u{2019}m unable",
    "as an ai",
    "here is the ",
    "here's the ",
    "here are the ",
    "certainly!",
    "certainly,",
    "of course,",
    "sure,",
    "sure!",
];

fn starts_like_a_reply(text: &str) -> bool {
    let t = text.trim_start().to_lowercase();
    REFUSAL_LEADS.iter().any(|lead| t.starts_with(lead))
}

/// Whether `text` is a JSON object or array standing where prose should be: it opens with `{` or
/// `[` and holds a quoted key, or it carries the engine's own reply keys (`"why":`,
/// `"fix_direction":`), which no prose does.
fn is_json_fragment(text: &str) -> bool {
    let t = text.trim();
    let opens = t.starts_with('{') || t.starts_with('[');
    (opens && t.contains("\":")) || t.contains("\"why\":") || t.contains("\"fix_direction\":")
}

/// The parts of a row's text a model wrote: the whole, and each side of the headline separator.
fn text_parts(text: &str) -> Vec<&str> {
    let mut parts = vec![text];
    parts.extend(text.split(crate::ai_audit::HEADLINE_SEP));
    parts
}

/// Sort what a row's text holds into real junk and wording judgments. `headline`, `snippet` and
/// `detail` are the row's own fields; the junk tests about evidence apply only to a row the model
/// INVENTED (rule id prefixed `AI-`), the rest to every row.
pub fn classify_row(rule_id: &str, headline: &str, snippet: &str, detail: &str) -> RowContent {
    let invented = rule_id.starts_with("AI-");
    let mut out = RowContent::default();
    if invented {
        if is_placeholder_rule_id(rule_id) {
            out.discard.push(DiscardReason::PlaceholderRuleId);
        }
        if !headline.trim().is_empty() && is_filler(headline) {
            out.discard.push(DiscardReason::PlaceholderTitle);
        }
        // The exported snippet falls back to the title when the model gave no code, so a snippet
        // equal to the headline is "no code", not a second piece of evidence.
        let code = if snippet.trim() == headline.trim() {
            ""
        } else {
            snippet
        };
        // The explanation is what follows the headline separator; with no separator the whole
        // text is all the row says.
        let explanation = detail
            .split_once(crate::ai_audit::HEADLINE_SEP)
            .map_or(detail, |(_, rest)| rest);
        match (is_filler(code), is_filler(explanation)) {
            (true, true) => out.discard.push(DiscardReason::NoEvidence),
            (false, true) => out.junk.push(JunkReason::EmptyExplanation),
            _ => {}
        }
    }
    let lower_detail = detail.to_lowercase();
    let lower_headline = headline.to_lowercase();
    // Only a row the model invented is held to the discard verdict: no prose can hold a row an
    // adopted or deterministic rule produced for saying "discard".
    let discard = invented
        && (lower_detail.contains("discard this finding")
            || lower_headline.contains("discard this finding"));
    if discard {
        out.discard.push(DiscardReason::DiscardVerdict);
    }
    if text_parts(detail)
        .into_iter()
        .chain(text_parts(headline))
        .any(starts_like_a_reply)
    {
        out.junk.push(JunkReason::ModelRefusal);
    }
    if is_json_fragment(detail) || is_json_fragment(headline) {
        out.junk.push(JunkReason::JsonFragment);
    }
    if invented {
        let sentence_lead = |t: &str| {
            let t = normalized(t);
            !is_filler(&t) && (t.starts_with("placeholder") || t.starts_with("lorem ipsum"))
        };
        if text_parts(detail)
            .into_iter()
            .chain(text_parts(headline))
            .any(sentence_lead)
        {
            out.wording.push(WordingFlag::PlaceholderLead);
        }
        if !discard
            && (lower_detail.contains("placeholder finding")
                || lower_headline.contains("placeholder finding"))
        {
            out.wording.push(WordingFlag::PlaceholderFinding);
        }
    }
    // A row that is not a finding is thrown out, not held.
    if !out.discard.is_empty() {
        out.junk.clear();
    }
    out
}

/// What a standalone audit of a zip says about one exported row, or `None` when it is fine: the
/// first reason it is thrown out or held. Wording judgments are never a problem. A zip that holds
/// such a row is unfit to ship whichever list the row belongs to (the engine leaves a thrown-out
/// row out of the export, so one in a package is a defect); used where no reviewer decisions are
/// available.
pub fn row_problem(rule_id: &str, headline: &str, snippet: &str, detail: &str) -> Option<String> {
    let content = classify_row(rule_id, headline, snippet, detail);
    content
        .discard
        .first()
        .map(|reason| reason.describe())
        .or_else(|| content.junk.first().map(|reason| reason.describe()))
        .map(str::to_string)
}

/// The `captures` key that marks a row that is real junk, holding it. The value lists the reasons
/// in words. Absent on any other row. A bookkeeping key (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`).
pub const JUNK_CONTENT_CAPTURE: &str = "junk_content";

/// Move every row that is not a finding out of `findings` (except the rows the reviewer
/// `restored`, which stay, marked no longer thrown out): the rows already marked
/// `engine_discarded` and the rows [`classify_row`] throws out (marked here with the reason).
/// Nothing is deleted: the rows are returned, in their original order, for the caller to keep
/// apart. Run at seal, the safety net behind the engine's own throw-out steps.
pub fn take_thrown_out(
    findings: &mut Vec<crate::onboard::Finding>,
    restored: &crate::engine_discard::RestoredKeys,
) -> Vec<crate::onboard::Finding> {
    let rows = std::mem::take(findings);
    let mut thrown = Vec::new();
    for mut f in rows {
        // A row the reviewer put back is never thrown out again, whatever its text reads.
        if restored.covers(&f) {
            if let Some(reason) = crate::engine_discard::restore(&mut f) {
                f.captures.insert(
                    crate::engine_discard::ENGINE_RESTORED_CAPTURE.to_string(),
                    reason,
                );
            }
            findings.push(f);
            continue;
        }
        if !crate::engine_discard::is_discarded(&f) {
            if let Some(reason) = classify_finding(&f).discard.first() {
                crate::engine_discard::mark(&mut f, reason.describe());
            }
        }
        if crate::engine_discard::is_discarded(&f) {
            thrown.push(f);
        } else {
            findings.push(f);
        }
    }
    thrown
}

/// The tally of [`mark_rows`], for the scan ledger.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentCheck {
    /// Rows the check read.
    #[serde(default)]
    pub rows_seen: usize,
    /// Rows marked real junk (held until a reviewer decides them).
    #[serde(default)]
    pub held: usize,
    /// Wording flags written (one per flag, so a row may count twice).
    #[serde(default)]
    pub wording_flags: usize,
}

/// The title, code and explanation of a row, as [`classify_row`] reads them: the title is the part
/// of the detail before the headline separator.
fn row_text(f: &crate::onboard::Finding) -> (String, &str, &str) {
    let headline = f
        .detail
        .split_once(crate::ai_audit::HEADLINE_SEP)
        .map_or(f.detail.as_str(), |(head, _)| head)
        .to_string();
    (headline, f.snippet.as_str(), f.detail.as_str())
}

/// [`classify_row`] over a finding.
pub fn classify_finding(f: &crate::onboard::Finding) -> RowContent {
    let (headline, snippet, detail) = row_text(f);
    classify_row(&f.rule_id, &headline, snippet, detail)
}

/// Mark every row of `findings` that is real junk with [`JUNK_CONTENT_CAPTURE`] (and clear the
/// mark on a row that is not), and give every row its content flags. Run at seal, the point the
/// row set is final, so the mark and the flags describe the rows that ship.
pub fn mark_rows(findings: &mut [crate::onboard::Finding]) -> ContentCheck {
    let mut tally = ContentCheck::default();
    for f in findings.iter_mut() {
        tally.rows_seen += 1;
        let content = classify_finding(f);
        if content.junk.is_empty() {
            f.captures.remove(JUNK_CONTENT_CAPTURE);
        } else {
            tally.held += 1;
            let reasons = content
                .junk
                .iter()
                .map(|r| r.describe())
                .collect::<Vec<_>>()
                .join("; ");
            f.captures.insert(JUNK_CONTENT_CAPTURE.to_string(), reasons);
        }
        let flags: Vec<String> = content
            .wording
            .iter()
            .map(|w| w.flag().to_string())
            .collect();
        tally.wording_flags += flags.len();
        if !flags.is_empty() || !crate::review_flags::flags_of(f).is_empty() {
            crate::review_flags::set_stage_flags(f, crate::review_flags::FlagStage::Content, flags);
        }
    }
    tally
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

    /// A real finding may use both words in its own sense: a placeholder in the code, a value
    /// the code discards. Neither is a verdict on the row, so the row stays.
    #[test]
    fn a_real_finding_that_mentions_a_placeholder_and_discarding_is_not_a_verdict() {
        for text in [
            "The placeholder image URL is fetched without validation, and the handler discards requests whose origin is unknown.",
            "A placeholder session token `changeme` is accepted; expired sessions are never discarded.",
            "The upload result is discarded without inspection, so the placeholder row stays in the table.",
        ] {
            assert!(!carries_discard_verdict(text), "{text}");
            assert!(!is_junk_invented_row("AI-UPLOAD-RESULT-IGNORED", "Upload result ignored", text));
            assert!(row_problem("AI-UPLOAD-RESULT-IGNORED", "Upload result ignored", "await upload(file);", text).is_none());
        }
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

    // ── M4: real junk holds the row, wording only flags it ──────────────────────────────

    fn junk_of(rule_id: &str, headline: &str, snippet: &str, detail: &str) -> Vec<JunkReason> {
        classify_row(rule_id, headline, snippet, detail).junk
    }

    fn wording_of(rule_id: &str, headline: &str, snippet: &str, detail: &str) -> Vec<WordingFlag> {
        classify_row(rule_id, headline, snippet, detail).wording
    }

    fn discard_of(
        rule_id: &str,
        headline: &str,
        snippet: &str,
        detail: &str,
    ) -> Vec<DiscardReason> {
        classify_row(rule_id, headline, snippet, detail).discard
    }

    /// Every pattern that HOLDS a row (broken text on a finding that may be real), one by one.
    #[test]
    fn each_broken_text_pattern_holds_the_row() {
        // Code, but an empty explanation.
        assert_eq!(
            junk_of("AI-X-Y", "A title", "db.query(q)", "  "),
            vec![JunkReason::EmptyExplanation]
        );
        // A refusal or chat lead-in, whole or after the headline separator.
        for text in [
            "I cannot determine whether this is exploitable.",
            "I'm sorry, but I can't help with that.",
            "As an AI model I have no access to the repository.",
            "Here is the explanation you asked for.",
            "Certainly! The handler is wrong.",
            "A title \u{2014} I cannot assist with this request.",
        ] {
            assert!(
                junk_of("SEC-A-1", "A title", "db.query(q)", text)
                    .contains(&JunkReason::ModelRefusal),
                "{text}"
            );
        }
        // An untranslated JSON fragment.
        for text in [
            r#"{"why":"The handler is wrong.","fix_direction":"Verify."}"#,
            r#"[{"id":"a1b2c3","why":"x"}]"#,
            r#"The handler is wrong. "fix_direction": "Verify it.""#,
        ] {
            assert!(
                junk_of("SEC-A-1", "A title", "db.query(q)", text)
                    .contains(&JunkReason::JsonFragment),
                "{text}"
            );
        }
    }

    /// A10: these are not findings at all. The engine throws the row out (keeps and marks it); it
    /// is never also HELD.
    #[test]
    fn each_not_a_finding_pattern_is_thrown_out_and_never_held() {
        let row = classify_row("AI-X-PLACEHOLDER", "A title", "db.query(q)", "Real text.");
        assert_eq!(row.discard, vec![DiscardReason::PlaceholderRuleId]);
        assert!(row.junk.is_empty());
        assert!(discard_of("AI-X-Y", "tbd", "db.query(q)", "Real text.")
            .contains(&DiscardReason::PlaceholderTitle));
        assert!(discard_of("AI-X-Y", "A title", "x", "n/a").contains(&DiscardReason::NoEvidence));
        assert!(
            discard_of("AI-X-Y", "A title", "db.query(q)", "Discard this finding.")
                .contains(&DiscardReason::DiscardVerdict)
        );
        // A discard verdict wrapped in a refusal is still thrown out, not held.
        let both = classify_row(
            "AI-X-Y",
            "A title",
            "db.query(q)",
            "I cannot say. Discard this finding.",
        );
        assert!(!both.discard.is_empty() && both.junk.is_empty(), "{both:?}");
    }

    /// Prose that merely MENTIONS the junk words, or quotes JSON, is not junk.
    #[test]
    fn real_prose_is_not_junk() {
        for text in [
            "The handler cannot be reached without a token, so the check is bypassed.",
            "The config `{\"debug\": true}` enables verbose errors in production.",
            "Sorry-style error pages leak the stack: the handler echoes the exception.",
            "Unable to verify the signature is treated as success.",
            "Here, the redirect target is taken from the query string.",
        ] {
            assert!(
                junk_of("AI-X-Y", "A title", "db.query(q)", text).is_empty(),
                "{text}"
            );
        }
    }

    /// The wording judgments never hold: they are flags, in the other list.
    #[test]
    fn wording_judgments_are_flags_not_junk() {
        let row = classify_row(
            "AI-HARDCODED-CREDENTIAL",
            "Placeholder credentials are committed",
            "API_KEY = \"changeme\"",
            "Placeholder credentials are committed in the example file and loaded in production.",
        );
        assert!(row.junk.is_empty(), "{row:?}");
        assert_eq!(row.wording, vec![WordingFlag::PlaceholderLead]);
        let row = classify_row(
            "AI-X-Y",
            "A title",
            "db.query(q)",
            "Treats every row as a placeholder finding when the table is empty.",
        );
        assert!(row.junk.is_empty(), "{row:?}");
        assert_eq!(row.wording, vec![WordingFlag::PlaceholderFinding]);
        // The bare filler word is the other list.
        assert!(
            !wording_of("AI-X-Y", "placeholder", "db.query(q)", "Real text.")
                .contains(&WordingFlag::PlaceholderLead)
        );
        assert!(
            discard_of("AI-X-Y", "placeholder", "db.query(q)", "Real text.")
                .contains(&DiscardReason::PlaceholderTitle)
        );
    }

    /// An adopted or deterministic rule's row is never held for evidence, never thrown out for the
    /// discard verdict, and never flagged for wording; only a refusal or JSON text holds it.
    #[test]
    fn an_adopted_rule_row_is_held_only_for_a_reply_or_a_fragment() {
        assert!(junk_of("SEC-A-1", "placeholder", "x", "x").is_empty());
        assert!(discard_of("SEC-A-1", "placeholder", "x", "x").is_empty());
        assert!(discard_of("SEC-A-1", "A title", "code", "Discard this finding.").is_empty());
        assert!(wording_of("SEC-A-1", "Placeholder text", "code", "Placeholder text.").is_empty());
        assert!(!junk_of("SEC-A-1", "A title", "code", "I cannot say.").is_empty());
    }

    #[test]
    fn mark_rows_holds_junk_flags_wording_and_clears_a_stale_mark() {
        use crate::onboard::Finding;
        let row = |detail: &str| Finding {
            rule_id: "AI-X-Y".to_string(),
            snippet: "db.query(q)".to_string(),
            detail: detail.to_string(),
            ..Finding::default()
        };
        let mut rows = vec![
            row("A title \u{2014} Real text."),
            row("A title \u{2014} I cannot assist."),
            row("Placeholder secret \u{2014} Placeholder credentials are committed."),
        ];
        rows[0]
            .captures
            .insert(JUNK_CONTENT_CAPTURE.to_string(), "stale".to_string());
        let tally = mark_rows(&mut rows);
        assert_eq!(
            tally,
            ContentCheck {
                rows_seen: 3,
                held: 1,
                wording_flags: 1
            }
        );
        assert!(!rows[0].captures.contains_key(JUNK_CONTENT_CAPTURE));
        assert!(rows[1].captures.contains_key(JUNK_CONTENT_CAPTURE));
        assert!(!rows[2].captures.contains_key(JUNK_CONTENT_CAPTURE));
        assert_eq!(
            crate::review_flags::flags_of(&rows[2]),
            vec!["placeholder_wording".to_string()]
        );
        // Marking again changes nothing.
        let again = mark_rows(&mut rows);
        assert_eq!(again, tally);
    }

    /// A10: the seal's safety net moves a row that is not a finding aside, marked with its reason,
    /// and loses none; a row held for broken text stays where it is.
    #[test]
    fn take_thrown_out_moves_rows_aside_and_loses_none() {
        use crate::onboard::Finding;
        let row = |rule: &str, detail: &str| Finding {
            rule_id: rule.to_string(),
            snippet: "db.query(q)".to_string(),
            detail: detail.to_string(),
            ..Finding::default()
        };
        let mut already = row("AI-OTHER", "A title \u{2014} Real text.");
        crate::engine_discard::mark(&mut already, crate::engine_discard::STACK_EXCEPTION);
        let mut rows = vec![
            row("AI-KEEP", "A title \u{2014} Real text."),
            row("AI-X-PLACEHOLDER", "A title \u{2014} Real text."),
            row("AI-NOTE", "A title \u{2014} Discard this finding."),
            row("AI-REFUSAL", "A title \u{2014} I cannot assist."),
            already,
        ];
        let before = rows.len();
        let thrown = take_thrown_out(&mut rows, &crate::engine_discard::RestoredKeys::default());
        assert_eq!(rows.len() + thrown.len(), before, "no row is lost");
        assert_eq!(
            rows.iter().map(|f| f.rule_id.as_str()).collect::<Vec<_>>(),
            ["AI-KEEP", "AI-REFUSAL"]
        );
        assert_eq!(
            thrown
                .iter()
                .map(|f| f.rule_id.as_str())
                .collect::<Vec<_>>(),
            ["AI-X-PLACEHOLDER", "AI-NOTE", "AI-OTHER"]
        );
        assert_eq!(
            crate::engine_discard::reason_of(&thrown[0]),
            Some(crate::engine_discard::PLACEHOLDER_RULE_ID)
        );
        assert_eq!(
            crate::engine_discard::reason_of(&thrown[1]),
            Some(crate::engine_discard::DISCARD_VERDICT)
        );
        assert_eq!(
            crate::engine_discard::reason_of(&thrown[2]),
            Some(crate::engine_discard::STACK_EXCEPTION)
        );
    }
}
