//! House style: client-reaching rule text is American English.
//!
//! Runs over the same wide field set as the dash gate (`common::client_reaching_fields`). A
//! British spelling in a title, directive, option label or any other field can land on the Rules
//! checked sheet, coverage.json, coverage.csv or a report.
//!
//! The matcher is built from spelling PATTERNS, not a word list of the words seen so far, so a
//! new rule that writes "utilise" or "neighbour" is caught on first sight:
//! - `-ise`, `-ised`, `-ises`, `-ising`, `-iser`, `-isation`, except words American English also
//!   spells with `-ise` ([`ISE_OK`]);
//! - `-yse`, `-ysed`, `-ysing` and the like (`analyse`, `paralyse`), but not `analyses`, which is
//!   the American plural of `analysis`;
//! - `-our` words (`behaviour`, `colour`, `honour`, `favour`, `labour`, `neighbour` and their
//!   `-al`, `-ful`, `-ite`, `-ing`, `-ed` forms);
//! - `defence`, `offence`, `licence`, `pretence`, `centre`, `metre`, `catalogue`, `analogue`,
//!   `judgement`, `acknowledgement`, `programme` and a few like them;
//! - doubled-l inflections (`modelled`, `labelled`, `signalling`, `cancelled`, `cancelling`,
//!   `travelled`, `levelled`, `fuelled`, `totalled`, `counselling`). Ruling: American English
//!   applies, so `cancelled` is `canceled` and `cancelling` is `canceling`. `cancellation` is
//!   correct in both and is not flagged.
//!
//! `dialogue` is deliberately NOT flagged: American usage keeps it for conversation and uses
//! `dialog` for a UI window, and a rule may mean either.
//!
//! Skipped on purpose:
//! - a whitespace-separated token containing `_`, `://` or a backtick (code, an identifier or a
//!   URL, not prose), and a word with an internal capital (`somePromise`, `isCancelled`);
//! - a token that is a rule OPTION ID. Option ids are identifiers: adopted selections are stored
//!   by id, so an id is never renamed to fix a spelling. The option id of a field is not in the
//!   field list at all; this also covers an id quoted inside prose;
//! - the title of an external source (`[[sources]].title` and `.linter`), because a quoted
//!   standard keeps its own spelling.

mod common;

use camerata_rules::{corpus_path, load_corpus_lenient, RuleSet};
use common::client_reaching_fields;
use std::collections::HashSet;

/// Words that end in `-ise` (or inflect from one) in American English too.
const ISE_OK: &[&str] = &[
    "promise",
    "exercise",
    "otherwise",
    "precise",
    "advise",
    "revise",
    "compromise",
    "enterprise",
    "premise",
    "expertise",
    "franchise",
    "merchandise",
    "surprise",
    "wise",
    "noise",
    "raise",
    "rise",
    "arise",
    "praise",
    "concise",
    "devise",
    "supervise",
    "televise",
    "improvise",
    "disguise",
    "chastise",
    "likewise",
    "clockwise",
    "anticlockwise",
    "treatise",
    "demise",
    "paradise",
    "comprise",
    "despise",
    "apprise",
    "excise",
    "incise",
    "circumcise",
    "poise",
    "tortoise",
    "cruise",
    "bruise",
    "valise",
    "anise",
    "mortise",
    "advertise",
    "appraise",
    "reprise",
    "sunrise",
    "uprise",
    "guise",
    "chemise",
    "turquoise",
    "porpoise",
    "vise",
    "exorcise",
];
const ISE_SUFFIXES: &[&str] = &["ise", "ised", "ises", "ising", "iser", "isers"];
const YSE_SUFFIXES: &[&str] = &["yse", "ysed", "ysing", "yser", "ysers"];

const OUR_STEMS: &[&str] = &[
    "behaviour",
    "colour",
    "honour",
    "favour",
    "labour",
    "neighbour",
    "rumour",
    "flavour",
    "harbour",
    "humour",
    "vapour",
    "odour",
    "savour",
    "armour",
    "clamour",
    "endeavour",
    "rigour",
    "vigour",
    "valour",
    "tumour",
    "splendour",
    "candour",
    "ardour",
    "fervour",
    "demeanour",
    "saviour",
    "succour",
    "parlour",
];
const OUR_ENDINGS: &[&str] = &[
    "", "s", "ed", "ing", "ings", "able", "ably", "ite", "ites", "al", "ally", "ful", "fully",
    "less", "ist", "ists", "er", "ers", "y", "ised", "ising", "ize", "ized", "izing", "hood",
    "hoods",
];
/// Nouns ending `-ce` in British spelling and `-se` in American.
const CE_STEMS: &[&str] = &["defence", "offence", "licence", "pretence"];
/// Nouns ending `-re` in British spelling and `-er` in American.
const RE_STEMS: &[&str] = &["centre", "metre", "litre", "fibre", "theatre", "calibre"];
const RE_ENDINGS: &[&str] = &["", "s", "d", "piece", "pieces"];
const UE_STEMS: &[&str] = &["catalogue", "analogue"];
const UE_ENDINGS: &[&str] = &["", "s", "d"];
const MENT_STEMS: &[&str] = &["judgement", "acknowledgement", "abridgement", "lodgement"];
const MISC: &[&str] = &[
    "programme",
    "programmes",
    "cheque",
    "cheques",
    "artefact",
    "artefacts",
    "fulfil",
    "fulfils",
    "fulfilment",
    "enrolment",
    "instalment",
    "skilful",
    "ageing",
    "jewellery",
    "focussed",
    "focussing",
    "cataloguing",
    "analoguing",
];
/// Verb and noun stems that British spelling doubles an `l` after.
const L_STEMS: &[&str] = &[
    "model", "label", "signal", "cancel", "travel", "level", "fuel", "total", "counsel", "channel",
    "tunnel", "quarrel", "marvel", "shovel", "jewel", "panel", "dial", "rival", "equal", "pedal",
    "unravel", "grovel", "swivel", "chisel", "funnel", "libel", "bevel", "revel", "initial",
];
const L_ENDINGS: &[&str] = &[
    "led", "ling", "ler", "lers", "lings", "list", "lists", "lor", "lors", "lous",
];

/// Whether the lowercase alphabetic `w` is a British spelling.
fn is_british(w: &str) -> bool {
    if MISC.contains(&w) || w.ends_with("isation") || w.ends_with("isations") {
        return true;
    }
    for suf in ISE_SUFFIXES {
        if let Some(stem) = w.strip_suffix(suf) {
            if !stem.is_empty()
                && !ISE_OK.contains(&w)
                && !ISE_OK.contains(&format!("{stem}ise").as_str())
            {
                return true;
            }
            break;
        }
    }
    for suf in YSE_SUFFIXES {
        if w.len() > suf.len() && w.ends_with(suf) {
            return true;
        }
    }
    for stem in OUR_STEMS {
        if let Some(i) = w.find(stem) {
            if OUR_ENDINGS.contains(&&w[i + stem.len()..]) {
                return true;
            }
        }
    }
    for stem in CE_STEMS {
        if let Some(rest) = w.strip_prefix(stem) {
            if rest.is_empty() || rest == "s" {
                return true;
            }
        }
    }
    for stem in RE_STEMS {
        if let Some(rest) = w.strip_prefix(stem) {
            if RE_ENDINGS.contains(&rest) {
                return true;
            }
        }
    }
    for stem in UE_STEMS {
        if let Some(rest) = w.strip_prefix(stem) {
            if UE_ENDINGS.contains(&rest) {
                return true;
            }
        }
    }
    for stem in MENT_STEMS {
        if let Some(rest) = w.strip_prefix(stem) {
            if rest.is_empty() || rest == "s" {
                return true;
            }
        }
    }
    L_STEMS.iter().any(|stem| {
        L_ENDINGS
            .iter()
            .any(|end| w.strip_prefix(stem) == Some(end))
    })
}

/// A word with a capital after its first letter (`somePromise`) is an identifier, not prose.
fn is_identifier_case(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next();
    let rest: String = chars.collect();
    rest != rest.to_lowercase() && word != word.to_uppercase()
}

/// The British words found in `text`. `option_ids` are skipped (see the module doc).
fn british_words(text: &str, option_ids: &HashSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    for token in text.split_whitespace() {
        if token.contains('_') || token.contains("://") || token.contains('`') {
            continue;
        }
        let bare = token.trim_matches(|c: char| ".,;:()[]{}\"'!?*".contains(c));
        if option_ids.contains(bare) {
            continue;
        }
        for word in token.split(|c: char| !c.is_ascii_alphabetic()) {
            if word.is_empty() || is_identifier_case(word) {
                continue;
            }
            if is_british(&word.to_lowercase()) {
                found.push(word.to_lowercase());
            }
        }
    }
    found
}

fn option_ids(corpus: &RuleSet) -> HashSet<String> {
    corpus
        .iter()
        .flat_map(|r| r.options.iter().map(|o| o.id.clone()))
        .collect()
}

fn spelling_offenders(corpus: &RuleSet) -> Vec<String> {
    let ids = option_ids(corpus);
    client_reaching_fields(corpus)
        .into_iter()
        .filter(|f| !f.external_title)
        .flat_map(|f| {
            british_words(f.text, &ids)
                .into_iter()
                .map(move |w| format!("{}: {} ({w})", f.rule_id, f.loc))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn no_client_reaching_rule_field_uses_a_british_spelling() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    assert!(corpus.iter().count() > 100, "vacuity guard");
    let offenders = spelling_offenders(&corpus);
    assert!(
        offenders.is_empty(),
        "client-reaching rule text must use American spelling ({} offenders):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

#[test]
fn the_matcher_catches_each_pattern_and_spares_american_words() {
    let none = HashSet::new();
    for british in [
        "behaviour",
        "colours",
        "honourable",
        "favourite",
        "labour",
        "neighbouring",
        "rumours",
        "flavoured",
        "organise",
        "organised",
        "organisation",
        "recognising",
        "normalise",
        "utilise",
        "serialisation",
        "analyse",
        "analysed",
        "analysing",
        "paralyse",
        "defence",
        "offence",
        "licence",
        "centre",
        "centred",
        "catalogue",
        "analogue",
        "judgement",
        "acknowledgement",
        "modelled",
        "labelled",
        "signalling",
        "cancelled",
        "cancelling",
        "travelled",
        "levelled",
        "fuelled",
        "totalled",
        "counselling",
        "programme",
    ] {
        assert_eq!(
            british_words(british, &none),
            [british],
            "the matcher missed `{british}`"
        );
    }
    for american in [
        "behavior",
        "color",
        "organize",
        "analyze",
        "analyses",
        "defense",
        "license",
        "center",
        "catalog",
        "judgment",
        "modeled",
        "canceled",
        "canceling",
        "cancellation",
        "traveling",
        "promise",
        "exercise",
        "otherwise",
        "precise",
        "advise",
        "revise",
        "compromise",
        "enterprise",
        "premise",
        "premises",
        "expertise",
        "franchise",
        "surprise",
        "surprising",
        "wise",
        "noise",
        "raise",
        "raised",
        "rise",
        "rising",
        "arise",
        "praise",
        "concise",
        "devise",
        "supervise",
        "improvise",
        "disguise",
        "dialogue",
        "labour_cost",
        "somePromise",
        "isCancelled",
    ] {
        assert!(
            british_words(american, &none).is_empty(),
            "the matcher flagged `{american}`: {:?}",
            british_words(american, &none)
        );
    }
    // URLs and backticked code are not prose.
    assert!(british_words("See https://example.com/colour/behaviour", &none).is_empty());
    assert!(british_words("call `colour()` here", &none).is_empty());
}

#[test]
fn an_option_id_quoted_in_prose_is_not_flagged() {
    let ids: HashSet<String> = ["test-names-describe-behaviour".to_string()].into();
    assert!(british_words("Pick test-names-describe-behaviour.", &ids).is_empty());
    assert_eq!(
        british_words("Pick test-names-describe-behaviour here.", &HashSet::new()),
        ["behaviour"]
    );
}

/// The wide gate must be able to fail: a British word planted in every field of the wide list is
/// reported for each one, and the external source title and author-facing `relevance` are not.
#[tokio::test]
async fn the_wide_spelling_gate_reports_a_british_word_planted_in_every_client_reaching_field() {
    let (_dir, corpus) = common::load_planted("the behaviour was organised").await;
    let offenders = spelling_offenders(&corpus);
    for expected in common::PLANTED_LOCATIONS {
        assert!(
            offenders.iter().any(|o| o.contains(expected)),
            "the gate failed to flag `{expected}`: {offenders:#?}"
        );
    }
    for exempt in common::PLANTED_SOURCE_LOCATIONS
        .iter()
        .chain(&["relevance"])
    {
        assert!(
            !offenders.iter().any(|o| o.contains(exempt)),
            "`{exempt}` must be exempt: {offenders:#?}"
        );
    }
}
