//! The name-word rule of ARCH-NAMING-ONE-CONCEPT-1: when do two function names say different
//! things? A verb choice (`getUser` / `fetchUser`) and a spelling convention (`formatPrice` /
//! `format_price`) are not a concept split; a different noun (`formatPrice` / `formatMoney`) is.

/// Leading verbs that are dropped before comparing: choosing among them is a verb choice, not a
/// different concept.
const LEADING_VERBS: &[&str] = &[
    "get",
    "set",
    "fetch",
    "load",
    "compute",
    "calc",
    "calculate",
    "format",
    "render",
    "build",
    "make",
    "create",
    "to",
    "from",
    "as",
    "is",
    "has",
    "should",
];

/// Trailing words that carry no concept.
const TRAILING_NOISE: &[&str] = &["impl", "helper", "util", "utils", "fn", "func"];

/// The words of an identifier: split on `_`, `-`, camel and Pascal boundaries (an acronym run
/// stays together: `HTTPResponse` is `http`, `response`), lower-cased.
pub(super) fn split_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' || c == '$' {
            flush(&mut words, &mut current);
            continue;
        }
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        let next = chars.get(i + 1).copied();
        let boundary = match prev {
            Some(p) if c.is_uppercase() => {
                // `fooBar`: lower to upper. `HTTPResponse`: the last capital of a run that is
                // followed by a lower-case letter starts a new word.
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            }
            _ => false,
        };
        if boundary {
            flush(&mut words, &mut current);
        }
        current.extend(c.to_lowercase());
    }
    flush(&mut words, &mut current);
    words
}

fn flush(words: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        words.push(std::mem::take(current));
    }
}

/// The concept words of a function name: [`split_words`], minus one leading verb and any trailing
/// noise words, sorted (so the comparison is of word multisets). A name that would be emptied by
/// the removals keeps its words, so `get` and `set` stay different.
pub(super) fn concept_words(name: &str) -> Vec<String> {
    let all = split_words(name);
    let mut words = all.clone();
    if words.len() > 1 && LEADING_VERBS.contains(&words[0].as_str()) {
        words.remove(0);
    }
    while words.len() > 1
        && words
            .last()
            .is_some_and(|w| TRAILING_NOISE.contains(&w.as_str()))
    {
        words.pop();
    }
    if words.is_empty() {
        words = all;
    }
    words.sort();
    words
}

/// Names that never count as a declared name: a default export and an anonymous function are
/// required by the framework, not chosen.
pub(super) fn is_countable_name(name: &str) -> bool {
    !name.is_empty() && name != "default" && name != "<anonymous>" && name != "anonymous"
}

/// Whether, among `names`, at least two have different concept words.
pub(super) fn has_two_different_concepts<'a>(names: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen: Vec<Vec<String>> = Vec::new();
    for name in names {
        if !is_countable_name(name) {
            continue;
        }
        let words = concept_words(name);
        if !seen.contains(&words) {
            seen.push(words);
        }
    }
    seen.len() >= 2
}
