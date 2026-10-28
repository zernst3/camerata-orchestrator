//! House style: client-facing text carries no serial (Oxford) comma.
//!
//! Every client deliverable writes "A, B and C", never "A, B, and C". The fields are the same
//! client-facing set `no_em_dash_client_facing.rs` gates, walked by the shared helper in
//! `tests/common/mod.rs` so the two gates cannot drift apart.
//!
//! The gate flags the CLEAR list pattern only: inside one clause, a list of short comma-separated
//! items whose last item is introduced by `, and` or `, or` ("Log the request, the response, and
//! the error"). It deliberately misses what it cannot be sure about, because a false positive on a
//! legitimate comma is worse than a missed ambiguous case:
//! - a comma joining two independent clauses ("The cache is cold, and the queue is empty") is not
//!   preceded by a list in its own clause, or has a long side, and is left alone;
//! - a long item on either side (more than [`MAX_ITEM_WORDS`] words) is treated as a clause, not
//!   a list item;
//! - a final item that opens with a verb ("..., and call jest.useRealTimers()") is a verb phrase
//!   joined to the sentence, not a list item, so its comma is a clause join (see
//!   [`VERB_OPENERS`]).
//!
//! A clause starts after the previous `. `, `; `, `: ` or ` - `, or after an unclosed `(`.

mod common;

use camerata_rules::{corpus_path, load_corpus_lenient, RuleSet};
use common::client_facing_fields;

/// A list item longer than this many words is read as a clause, not a short list item.
const MAX_ITEM_WORDS: usize = 5;

/// Words that open a verb phrase rather than a noun-phrase list item. A comma before `and`/`or`
/// followed by one of these joins two predicates ("with per-session caps, and self-check
/// periodically"), where the comma is a deliberate clause join. Deliberately a short list: a verb
/// missing here only means the gate misses an ambiguous case, never that it flags a clause join.
const VERB_OPENERS: &[&str] = &[
    "self-check",
    "call",
    "remove",
    "check",
    "delete",
    "move",
    "commit",
    "bind",
    "keep",
    "route",
    "await",
    "derive",
    "pause",
    "tag",
    "let",
    "have",
    "export",
    "use",
    "pass",
    "reject",
    "rejects",
    "selects",
    "completes",
    "is",
    "are",
];

fn opens_with_verb(item: &str) -> bool {
    item.split_whitespace()
        .next()
        .is_some_and(|w| VERB_OPENERS.contains(&w.to_lowercase().as_str()))
}

/// Byte offset where the clause containing byte `pos` of `text` begins.
fn clause_start(text: &str, pos: usize) -> usize {
    let head = &text[..pos];
    let mut start = 0;
    for sep in [". ", "; ", ": ", " - "] {
        if let Some(i) = head.rfind(sep) {
            start = start.max(i + sep.len());
        }
    }
    // An unclosed "(" opens a parenthetical: the list inside it starts right after the paren.
    let mut depth = 0usize;
    for (i, ch) in head.char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' if depth == 0 => {
                start = start.max(i + 1);
                break;
            }
            '(' => depth -= 1,
            _ => {}
        }
    }
    start
}

/// Byte offset of the last comma in `prefix` that is not inside a parenthetical opened within
/// `prefix` (a comma inside `it.skip('x', () => {})` separates arguments, not list items).
fn last_top_level_comma(prefix: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut last = None;
    for (i, ch) in prefix.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => last = Some(i),
            _ => {}
        }
    }
    last
}

/// The item that follows the conjunction, and whether a comma closes it. An item closed by a
/// comma ("..., or any account holder, can upload") is a parenthetical aside, not the end of a list.
fn item_after(after: &str) -> (&str, bool) {
    let mut depth = 0usize;
    for (i, ch) in after.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return (&after[..i], false),
            ')' => depth -= 1,
            ',' if depth == 0 => return (&after[..i], true),
            '.' | ';' | ':' if depth == 0 => return (&after[..i], false),
            _ => {}
        }
    }
    (after, false)
}

/// The snippet around each serial comma ("A, B, and C") found in `text`.
fn serial_commas(text: &str) -> Vec<String> {
    // Hard-wrapped TOML strings carry newlines; read the text as one line.
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short = |s: &str| (1..=MAX_ITEM_WORDS).contains(&s.split_whitespace().count());
    let mut found = Vec::new();
    for conj in [", and ", ", or "] {
        for (pos, _) in flat.match_indices(conj) {
            let start = clause_start(&flat, pos);
            let prefix = &flat[start..pos];
            // A list needs an earlier comma in the same clause: the one after its first item.
            let Some(comma) = last_top_level_comma(prefix) else {
                continue;
            };
            let last_item = &prefix[comma + 1..];
            let (after_item, closed_by_comma) = item_after(&flat[pos + conj.len()..]);
            if !closed_by_comma
                && !opens_with_verb(after_item)
                && short(last_item)
                && short(after_item)
            {
                let from = flat[..pos]
                    .char_indices()
                    .rev()
                    .nth(40)
                    .map_or(0, |(i, _)| i);
                let to = (pos + conj.len() + after_item.len()).min(flat.len());
                found.push(flat[from..to].to_string());
            }
        }
    }
    found
}

/// Every `<rule id>: <field>: <snippet>` carrying a serial comma, over the client-facing fields.
fn serial_comma_offenders(corpus: &RuleSet) -> Vec<String> {
    client_facing_fields(corpus)
        .into_iter()
        .flat_map(|f| {
            serial_commas(f.text)
                .into_iter()
                .map(move |snip| format!("{}: {}: ...{snip}", f.rule_id, f.loc))
        })
        .collect()
}

#[tokio::test]
async fn no_client_facing_rule_field_carries_a_serial_comma() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    assert!(
        corpus.iter().count() > 100,
        "vacuity guard: the corpus loaded too few rules for this gate to mean anything"
    );

    let offenders = serial_comma_offenders(&corpus);
    assert!(
        offenders.is_empty(),
        "client-facing rule text must read \"A, B and C\", never \"A, B, and C\"; drop the comma \
         before the final and/or of a list ({} offenders):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// The detector must be able to fail, and must leave a comma that joins two clauses alone.
#[test]
fn the_serial_comma_detector_flags_a_list_and_spares_clause_joins() {
    for planted in [
        "Log the request, the response, and the error.",
        "Accept a token, a cookie, or a header.",
        "Verify (the key, the nonce, and the tag) before use.",
        "Step one: read, parse, and validate the input.",
        "Hard\nwrapped lists a,\nb, and c.",
    ] {
        assert!(
            !serial_commas(planted).is_empty(),
            "the detector missed a planted serial comma: {planted:?}"
        );
    }

    for clean in [
        // The house style itself.
        "Log the request, the response and the error.",
        "Accept a token, a cookie or a header.",
        // A comma joining two independent clauses, no list in the clause.
        "Rotate the key, and redeploy the application.",
        "The cache is cold, and the queue is empty.",
        // A clause join whose clause has a list but whose far side is a long clause.
        "Logs, metrics and traces are shipped, and the on-call engineer is paged when the error rate stays high.",
        // Separate sentences: the comma in the first must not make the second look like a list.
        "Use a token, not a password. Rotate it, and log the rotation.",
        // Two items only.
        "Choose a vendor, or build it yourself.",
        // A verb phrase joined to the sentence is a clause join, not a list item.
        "Use a cache, a queue, and self-check periodically during a run.",
        // A comma inside a call is an argument separator, not a list.
        "Skip exists (e.g. it.skip('x', () => {})), and using it still trips the lint.",
        // An aside closed by its own comma is not the tail of a list.
        "Anyone can read it, and anyone, or any account holder, can write it.",
    ] {
        assert!(
            serial_commas(clean).is_empty(),
            "the detector flagged text it should leave alone: {clean:?} -> {:?}",
            serial_commas(clean)
        );
    }
}
