//! Pull the text of one cited section out of an HTML page, normalise it, and hash it.
//!
//! This is a deliberately small tolerant tokenizer, not a browser: it only has to find the
//! element a `#anchor` names and gather the visible text that belongs to it. Pages that build
//! their DOM with JavaScript will not contain the anchor in the fetched HTML; that is reported as
//! "anchor not found", which is its own signal and never a "changed" claim.

use sha2::{Digest, Sha256};

/// Prefix on every stored hash, so a future algorithm change is distinguishable.
pub const HASH_PREFIX: &str = "sha256:";

/// Elements that never have a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
/// Elements whose text is not visible prose.
const RAW_TEXT: &[&str] = &["script", "style", "noscript", "template", "svg"];
/// Elements that do not break the text flow (no separator inserted around them).
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "em", "i", "kbd", "mark", "q", "s", "samp",
    "small", "span", "strong", "sub", "sup", "time", "u", "var",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Start {
        name: String,
        id: Option<String>,
        name_attr: Option<String>,
        self_closing: bool,
    },
    End(String),
    Text(String),
}

impl Token {
    fn heading_level(&self) -> Option<u8> {
        match self {
            Token::Start { name, .. } => heading_level(name),
            _ => None,
        }
    }
}

fn heading_level(name: &str) -> Option<u8> {
    match name.as_bytes() {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

/// Parse `html` into start tags (with only the attributes this module reads), end tags and text.
fn tokenize(html: &str) -> Vec<Token> {
    let bytes = html.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    let flush = |tokens: &mut Vec<Token>, from: usize, to: usize| {
        if to > from {
            tokens.push(Token::Text(html[from..to].to_string()));
        }
    };
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &html[i..];
        if rest.starts_with("<!--") {
            flush(&mut tokens, text_start, i);
            i = rest.find("-->").map_or(bytes.len(), |p| i + p + 3);
            text_start = i;
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            flush(&mut tokens, text_start, i);
            i = rest.find('>').map_or(bytes.len(), |p| i + p + 1);
            text_start = i;
        } else if rest.starts_with("</")
            && rest.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic)
        {
            flush(&mut tokens, text_start, i);
            let (name_end, end) = rest
                .find('>')
                .map_or((bytes.len(), bytes.len()), |p| (i + p, i + p + 1));
            let name = html[i + 2..name_end].trim().to_ascii_lowercase();
            tokens.push(Token::End(name));
            i = end;
            text_start = i;
        } else if rest.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic) {
            flush(&mut tokens, text_start, i);
            let (token, end) = parse_start_tag(html, i);
            let raw = match &token {
                Token::Start {
                    name,
                    self_closing: false,
                    ..
                } if RAW_TEXT.contains(&name.as_str()) => Some(name.clone()),
                _ => None,
            };
            tokens.push(token);
            i = end;
            if let Some(name) = raw {
                let close = format!("</{name}");
                let lower_rest = html[i..].to_ascii_lowercase();
                i = lower_rest.find(&close).map_or(bytes.len(), |p| i + p);
            }
            text_start = i;
        } else {
            i += 1;
        }
    }
    flush(&mut tokens, text_start, bytes.len());
    tokens
}

/// Parse the start tag beginning at `html[start] == '<'`. Returns the token and the index just
/// past the closing `>`.
fn parse_start_tag(html: &str, start: usize) -> (Token, usize) {
    let bytes = html.as_bytes();
    let mut i = start + 1;
    let name_start = i;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/'
    {
        i += 1;
    }
    let name = html[name_start..i].to_ascii_lowercase();
    let (mut id, mut name_attr, mut self_closing) = (None, None, false);
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            if bytes[i] == b'/' {
                self_closing = true;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'>' {
            i += 1;
            break;
        }
        self_closing = false;
        let attr_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let attr = html[attr_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = html[value_start..i].to_string();
                i = (i + 1).min(bytes.len());
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = html[value_start..i].to_string();
            }
        }
        match attr.as_str() {
            "id" => id = Some(value),
            "name" => name_attr = Some(value),
            _ => {}
        }
    }
    (
        Token::Start {
            name,
            id,
            name_attr,
            self_closing,
        },
        i,
    )
}

/// Decode the handful of HTML entities that appear in prose, plus numeric references.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let decoded = tail.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let entity = &tail[1..semi];
            let ch = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix('#')
                    .and_then(|num| {
                        num.strip_prefix(['x', 'X']).map_or_else(
                            || num.parse::<u32>().ok(),
                            |h| u32::from_str_radix(h, 16).ok(),
                        )
                    })
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, semi + 1))
        });
        match decoded {
            Some((c, consumed)) => {
                out.push(c);
                rest = &tail[consumed..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapse every run of whitespace to a single space and trim the ends. The one definition of
/// "the same text" used for hashing and diffing.
pub fn normalise_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The stored form of a hash: `sha256:<hex>` over the normalised text.
pub fn hash_text(normalised: &str) -> String {
    let digest = Sha256::digest(normalised.as_bytes());
    let mut hex = String::with_capacity(HASH_PREFIX.len() + 64);
    hex.push_str(HASH_PREFIX);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The visible text of `tokens`, with a space around every block-level boundary.
fn visible_text(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Text(t) => out.push_str(&decode_entities(t)),
            Token::Start { name, .. } | Token::End(name) => {
                if !INLINE.contains(&name.as_str()) {
                    out.push(' ');
                }
            }
        }
    }
    normalise_whitespace(&out)
}

/// Index of the token matching the element opened at `open` (same-name depth counting), or the
/// last token when the markup never closes it.
fn matching_end(tokens: &[Token], open: usize) -> usize {
    let Token::Start { name, .. } = &tokens[open] else {
        return open;
    };
    if VOID.contains(&name.as_str()) {
        return open;
    }
    let mut depth = 1usize;
    for (j, token) in tokens.iter().enumerate().skip(open + 1) {
        match token {
            Token::Start {
                name: n,
                self_closing: false,
                ..
            } if n == name => depth += 1,
            Token::End(n) if n == name => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    tokens.len() - 1
}

/// The heading section starting at the heading token `open`: everything up to the next heading of
/// the same or a higher level.
fn heading_section(tokens: &[Token], open: usize, level: u8) -> String {
    let end = tokens
        .iter()
        .enumerate()
        .skip(open + 1)
        .find(|(_, t)| t.heading_level().is_some_and(|l| l <= level))
        .map_or(tokens.len(), |(j, _)| j);
    visible_text(&tokens[open..end])
}

/// Index of the start tag of the heading that closes immediately before token `at` (ignoring
/// whitespace), if any. GitHub renders `<h2>Title</h2><a id="user-content-title"></a>`.
fn heading_just_before(tokens: &[Token], at: usize) -> Option<usize> {
    let prev = (0..at)
        .rev()
        .find(|&j| !matches!(&tokens[j], Token::Text(t) if t.trim().is_empty()))?;
    let Token::End(name) = &tokens[prev] else {
        return None;
    };
    heading_level(name)?;
    (0..prev)
        .rev()
        .find(|&j| matches!(&tokens[j], Token::Start { name: n, .. } if n == name))
}

/// What looking up an anchor found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionLookup {
    /// The section's normalised text.
    Found(String),
    /// No element carries the anchor, or the section has no text.
    NotFound,
}

/// Find the element `anchor` names (`id`, `name`, or GitHub's `user-content-` form) in `html` and
/// return its section's normalised text:
///
/// - a heading owns everything up to the next heading of the same or higher level;
/// - an empty marker (`<a id=x></a>`) borrows the heading it sits right after, else the next
///   heading;
/// - any other element owns its own contents.
pub fn extract_section(html: &str, anchor: &str) -> SectionLookup {
    let tokens = tokenize(html);
    let prefixed = format!("user-content-{anchor}");
    let hit = tokens.iter().position(|t| match t {
        Token::Start { id, name_attr, .. } => [id, name_attr]
            .into_iter()
            .flatten()
            .any(|v| v == anchor || *v == prefixed),
        _ => false,
    });
    let Some(open) = hit else {
        return SectionLookup::NotFound;
    };
    let text = if let Some(level) = tokens[open].heading_level() {
        heading_section(&tokens, open, level)
    } else {
        let close = matching_end(&tokens, open);
        let own = visible_text(&tokens[open..=close]);
        if own.is_empty() {
            let heading = heading_just_before(&tokens, open).or_else(|| {
                tokens
                    .iter()
                    .enumerate()
                    .skip(open + 1)
                    .find(|(_, t)| t.heading_level().is_some())
                    .map(|(j, _)| j)
            });
            heading
                .and_then(|h| {
                    tokens[h]
                        .heading_level()
                        .map(|l| heading_section(&tokens, h, l))
                })
                .unwrap_or_default()
        } else {
            own
        }
    };
    if text.is_empty() {
        SectionLookup::NotFound
    } else {
        SectionLookup::Found(text)
    }
}

/// The fragment of `url` (text after the first `#`), if non-empty, and the URL without it.
pub fn split_fragment(url: &str) -> (&str, Option<&str>) {
    match url.split_once('#') {
        Some((page, frag)) if !frag.is_empty() => (page, Some(frag)),
        Some((page, _)) => (page, None),
        None => (url, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_section_stops_at_the_next_heading_of_equal_or_higher_level() {
        let html = "<h1>Guide</h1><h2 id=a>Alpha</h2><p>one  two</p><h3>Sub</h3><p>three</p>\
                    <h2>Beta</h2><p>other</p>";
        assert_eq!(
            extract_section(html, "a"),
            SectionLookup::Found("Alpha one two Sub three".to_string())
        );
    }

    #[test]
    fn github_style_trailing_empty_anchor_borrows_the_heading_before_it() {
        let html = "<div><h2>Real Title</h2><a id=\"user-content-real-title\" href=\"#real-title\"></a></div>\
                    <p>body</p><h2>Next</h2>";
        assert_eq!(
            extract_section(html, "real-title"),
            SectionLookup::Found("Real Title body".to_string())
        );
    }

    #[test]
    fn leading_empty_anchor_borrows_the_next_heading() {
        let html = "<p>intro</p><a name=\"s4\"></a><h3>Four</h3><p>text</p><h3>Five</h3>";
        assert_eq!(
            extract_section(html, "s4"),
            SectionLookup::Found("Four text".to_string())
        );
    }

    #[test]
    fn container_anchor_owns_its_own_contents_and_scripts_are_ignored() {
        let html =
            "<div id=x><p>a &amp; b</p><script>var t='<h1>';</script><div>c</div></div><p>out</p>";
        assert_eq!(
            extract_section(html, "x"),
            SectionLookup::Found("a & b c".to_string())
        );
    }

    #[test]
    fn missing_anchor_is_not_found() {
        assert_eq!(
            extract_section("<h1 id=a>A</h1>text", "zzz"),
            SectionLookup::NotFound
        );
    }

    #[test]
    fn inline_markup_does_not_split_words_but_blocks_do() {
        let html = "<h2 id=a>Use <code>foo</code>bar</h2><p>x</p><p>y</p>";
        assert_eq!(
            extract_section(html, "a"),
            SectionLookup::Found("Use foobar x y".to_string())
        );
    }

    #[test]
    fn whitespace_normalisation_and_hash_are_stable() {
        assert_eq!(normalise_whitespace("  a \n\t b  "), "a b");
        assert_eq!(hash_text("a b"), hash_text(&normalise_whitespace("a\n  b")));
        assert_ne!(hash_text("a b"), hash_text("a c"));
        assert!(hash_text("x").starts_with(HASH_PREFIX));
    }

    #[test]
    fn fragment_splitting() {
        assert_eq!(split_fragment("https://a/b#c"), ("https://a/b", Some("c")));
        assert_eq!(split_fragment("https://a/b#"), ("https://a/b", None));
        assert_eq!(split_fragment("https://a/b"), ("https://a/b", None));
    }

    #[test]
    fn entities_numeric_and_unknown() {
        assert_eq!(
            decode_entities("a&#39;b&#x41;&bogus; &amp;"),
            "a'bA&bogus; &"
        );
    }
}
