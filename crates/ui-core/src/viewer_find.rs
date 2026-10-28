//! Pure logic for the code viewer's reading aids (`WORK_ORDER_REVIEW_SPEED.md` A.5 and A.6):
//! the finding popover, in-file search, symbol references and the permalink.
//!
//! The key map and the navigation live in [`crate::viewer`]; this module holds what the aids
//! decide. Everything here is a plain function with a unit test and no rendering framework:
//!
//! * [`popover_view`]: which eight lines the popover shows for a finding;
//! * [`search_lines`], [`cycle_match`], [`mark_matches`]: finding, stepping through and
//!   highlighting matches;
//! * [`symbol_prefill`], [`SymbolRefsView`]: what `S` asks for and what the answer means;
//! * [`permalink`]: the text `Y` copies.

use std::collections::BTreeMap;

use camerata_api_types::symbol::is_valid_symbol;
use serde::Deserialize;

use crate::rules::split_needs_review;
use crate::source::short_sha;
use crate::triage::{
    confidence_tag, deviation_headline, finding_key, kind_tag, FindingView, FocusMove, TriageState,
};
use crate::viewer::{FileFinding, Segment, SourceFileView};

// ── Search (A.6) ─────────────────────────────────────────────────────────────────────────

/// Matches kept for one search. A needle that matches more than this is still stepped through;
/// the status says the count stopped here.
pub const MAX_MATCHES: usize = 10_000;

/// One char, folded for a case-insensitive compare. One char in, one char out, so a column in
/// the folded line is the same column in the original (`str::to_lowercase` can change lengths).
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Every occurrence of `needle` in `lines`: `(line, col)`, 1-based line within `lines` and
/// 0-based column in CHARS, so a multi-byte character never shifts a match. Plain substring,
/// case-insensitive, non-overlapping, line by line (a match never spans a line break). An empty
/// needle matches nothing. At most [`MAX_MATCHES`].
pub fn search_lines(lines: &[String], needle: &str) -> Vec<(usize, usize)> {
    let wanted: Vec<char> = needle.chars().map(fold).collect();
    let mut found = Vec::new();
    if wanted.is_empty() {
        return found;
    }
    for (i, line) in lines.iter().enumerate() {
        let hay: Vec<char> = line.chars().map(fold).collect();
        let mut col = 0;
        while col + wanted.len() <= hay.len() {
            if hay[col..col + wanted.len()] == wanted[..] {
                found.push((i + 1, col));
                if found.len() >= MAX_MATCHES {
                    return found;
                }
                col += wanted.len();
            } else {
                col += 1;
            }
        }
    }
    found
}

/// The answer to a search over what the viewer has loaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResult {
    /// `(file line, char column)`, in file order.
    pub matches: Vec<(usize, usize)>,
    /// Chars in the needle: the length of every highlight.
    pub needle_chars: usize,
    /// Set when only part of the file is loaded (a file over 400 KB is shown in windows):
    /// the search covers those lines and the status says so.
    pub scope_note: Option<String>,
    /// The match list stopped at [`MAX_MATCHES`].
    pub capped: bool,
}

/// The text of one loaded line, joined from its highlighted segments.
pub fn segments_text(segments: &[Segment]) -> String {
    segments.iter().map(|(_, t)| t.as_str()).collect()
}

/// Search the lines `view` carries, reporting file line numbers.
pub fn search_view(view: &SourceFileView, needle: &str) -> SearchResult {
    let first = view.window_or_default().from.max(1);
    let lines: Vec<String> = view.lines.iter().map(|s| segments_text(s)).collect();
    let matches: Vec<(usize, usize)> = search_lines(&lines, needle)
        .into_iter()
        .map(|(line, col)| (line + first - 1, col))
        .collect();
    let w = view.window_or_default();
    let scope_note = (!view.is_whole() && !view.lines.is_empty()).then(|| {
        format!(
            "searching lines {} to {} of {} only",
            w.from, w.to, w.total_lines
        )
    });
    SearchResult {
        capped: matches.len() >= MAX_MATCHES,
        matches,
        needle_chars: needle.chars().count(),
        scope_note,
    }
}

/// The match to start on when the needle changes: the first at or after `from_line`, else the
/// first of all.
pub fn first_match_from(matches: &[(usize, usize)], from_line: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(
        matches
            .iter()
            .position(|(line, _)| *line >= from_line)
            .unwrap_or(0),
    )
}

/// Step to the next (`forward`) or previous match: `(new index, wrapped)`. Past either end it
/// wraps, and says so, so the end of the list is never crossed silently. `None` with no matches.
pub fn cycle_match(current: Option<usize>, total: usize, forward: bool) -> Option<(usize, bool)> {
    if total == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => (0, false),
        (None, false) => (total - 1, false),
        (Some(i), true) if i + 1 < total => (i + 1, false),
        (Some(_), true) => (0, true),
        (Some(i), false) if i > 0 => (i - 1, false),
        (Some(_), false) => (total - 1, true),
    })
}

/// The status line of the search field: `3 of 12`, `3 of 12 (wrapped)`, `No matches`.
pub fn search_status(
    needle: &str,
    result: &SearchResult,
    current: Option<usize>,
    wrapped: bool,
) -> String {
    if needle.is_empty() {
        return "Type to search this file".to_string();
    }
    let total = result.matches.len();
    if total == 0 {
        return match &result.scope_note {
            Some(note) => format!("No matches ({note})"),
            None => "No matches".to_string(),
        };
    }
    let plus = if result.capped { "+" } else { "" };
    let n = current.map_or(0, |i| i + 1);
    let mut text = format!("{n} of {total}{plus}");
    if wrapped {
        text.push_str(" (wrapped)");
    }
    if let Some(note) = &result.scope_note {
        text.push_str(&format!(" ({note})"));
    }
    text
}

/// What a press in the search field asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKey {
    Next,
    Previous,
    Close,
}

/// The search field's own keys: Enter steps forward, Shift+Enter back, Esc closes the field.
/// Anything else is text.
pub fn search_key(key: &str, shift: bool) -> Option<SearchKey> {
    match key {
        "Enter" if shift => Some(SearchKey::Previous),
        "Enter" => Some(SearchKey::Next),
        "Escape" => Some(SearchKey::Close),
        _ => None,
    }
}

/// How a run of text is marked by the search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    /// A match.
    Match,
    /// The match Enter will step from.
    Current,
}

/// A run of one line: its syntax class, its text and whether it is a search match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkedSegment {
    pub class: Option<usize>,
    pub text: String,
    pub mark: Mark,
}

/// Cut `segments` at the edges of `ranges` (`(char column, chars)`, ascending and not
/// overlapping, as [`search_lines`] gives them) so each piece is either all match or none.
/// `current` is the column of the match to draw as the current one. The pieces re-join to the
/// line exactly.
pub fn mark_matches(
    segments: &[Segment],
    ranges: &[(usize, usize)],
    current: Option<usize>,
) -> Vec<MarkedSegment> {
    let mut out = Vec::new();
    let mut offset = 0;
    for (class, text) in segments {
        let chars: Vec<char> = text.chars().collect();
        let end = offset + chars.len();
        let mut pos = offset;
        while pos < end {
            let inside = ranges.iter().find(|(c, l)| pos >= *c && pos < c + l);
            let (stop, mark) = match inside {
                Some((c, l)) => (
                    end.min(c + l),
                    if current == Some(*c) {
                        Mark::Current
                    } else {
                        Mark::Match
                    },
                ),
                None => {
                    let next = ranges
                        .iter()
                        .map(|(c, _)| *c)
                        .filter(|c| *c > pos)
                        .min()
                        .unwrap_or(end);
                    (end.min(next), Mark::None)
                }
            };
            out.push(MarkedSegment {
                class: *class,
                text: chars[pos - offset..stop - offset].iter().collect(),
                mark,
            });
            pos = stop;
        }
        offset = end;
    }
    out
}

// ── Symbol references (A.6) ──────────────────────────────────────────────────────────────

/// Capture names that hold a symbol worth searching for, best first.
const SYMBOL_CAPTURES: [&str; 8] = [
    "symbol",
    "function-name",
    "function",
    "method",
    "identifier",
    "name",
    "class",
    "table",
];

/// The longest identifier (`[A-Za-z_][A-Za-z0-9_]*`) on `line`; the first of equals.
pub fn longest_identifier(line: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    let mut start: Option<usize> = None;
    let bytes = line.as_bytes();
    let mut consider = |from: usize, to: usize| {
        let word = &line[from..to];
        if best.is_none_or(|b| word.len() > b.len()) {
            best = Some(word);
        }
    };
    for (i, b) in bytes.iter().enumerate() {
        let word_byte = b.is_ascii_alphanumeric() || *b == b'_';
        match (start, word_byte) {
            (None, true) if !b.is_ascii_digit() => start = Some(i),
            (Some(s), false) => {
                consider(s, i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        consider(s, bytes.len());
    }
    best.map(str::to_string)
}

/// What `S` pre-fills: the symbol the detector captured when it recorded one (a known capture
/// name first, then any capture that is a valid symbol), else the longest identifier on the
/// violation line. `None` when neither gives a searchable symbol.
pub fn symbol_prefill(captures: &BTreeMap<String, String>, line: &str) -> Option<String> {
    let usable = |v: &str| {
        let v = v.trim();
        is_valid_symbol(v).then(|| v.to_string())
    };
    for name in SYMBOL_CAPTURES {
        if let Some(found) = captures.get(name).and_then(|v| usable(v)) {
            return Some(found);
        }
    }
    captures
        .values()
        .find_map(|v| usable(v))
        .or_else(|| longest_identifier(line).filter(|w| is_valid_symbol(w)))
}

/// One reference: a code location, not a finding.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct RefHit {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub text: String,
}

/// One `GET /api/onboard/symbol-refs` response, deserialized as it came. Every field defaults,
/// so an answer this build does not recognise degrades to "nothing to show".
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct SymbolRefsView {
    #[serde(default)]
    pub status: String,
    /// `git` (the inspected commit) or `working_tree`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub hits: Vec<RefHit>,
    /// There were more than the list holds.
    #[serde(default)]
    pub more: bool,
}

/// What a references answer means for the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefsOutcome {
    /// A list to show (possibly empty: "no references").
    List,
    /// Nothing to list; this is the sentence to show.
    Note(String),
}

impl SymbolRefsView {
    /// Whether the list can be shown.
    pub fn outcome(&self) -> RefsOutcome {
        match self.status.as_str() {
            "ok" | "commit_unavailable" => RefsOutcome::List,
            "source_deleted" => {
                RefsOutcome::Note(self.reason_or("Source deleted, re-fetch to view"))
            }
            "file_missing" => RefsOutcome::Note(self.reason_or("The repo was not found")),
            "too_large" => RefsOutcome::Note(self.reason_or("The search took too long")),
            "unavailable" => RefsOutcome::Note(self.reason_or("The search could not run")),
            _ => RefsOutcome::Note(self.reason_or("The references could not be loaded")),
        }
    }

    fn reason_or(&self, fallback: &str) -> String {
        if self.reason.trim().is_empty() {
            fallback.to_string()
        } else {
            self.reason.clone()
        }
    }

    /// The amber line when the hits are not from the inspected commit.
    pub fn banner(&self) -> Option<String> {
        (self.outcome() == RefsOutcome::List && self.source == "working_tree")
            .then(|| self.reason_or("No inspected commit was recorded; searched the working tree"))
    }

    /// The list's heading: `14 references to foo`, `200 references to foo (more not shown)`.
    /// A search that did not run says so; it is never "No references". Zero hits with `more` set
    /// means the output was cut before a hit could be listed, which is not "none".
    pub fn summary(&self) -> String {
        if self.outcome() != RefsOutcome::List {
            return format!("Could not search for {}", self.symbol);
        }
        let n = self.hits.len();
        let plural = if n == 1 { "reference" } else { "references" };
        match (n, self.more) {
            (0, true) => format!(
                "No references to {} could be listed (the search output was cut short)",
                self.symbol
            ),
            (0, false) => format!("No references to {}", self.symbol),
            (_, true) => format!("{n} {plural} to {} (more not shown)", self.symbol),
            _ => format!("{n} {plural} to {}", self.symbol),
        }
    }
}

/// Move the selection in the references list by one. No wrap: the ends are the ends.
pub fn move_selection(selected: usize, len: usize, dir: FocusMove) -> usize {
    if len == 0 {
        return 0;
    }
    match dir {
        FocusMove::Down => (selected + 1).min(len - 1),
        FocusMove::Up => selected.saturating_sub(1),
    }
}

/// What opening a place in the code does to the viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocationPlan {
    /// The place is in the file on screen: scroll to the line.
    Scroll,
    /// The place is in the finding's own file, and a reference was on screen: come back to it.
    ShowOwnFile,
    /// The place is in another file: show that file (a reference).
    ShowReference(String),
}

/// How to show `target` when `current` is on screen and `own` is the finding's file. A place is
/// never a finding: whichever way it goes, the queue's cursor does not move.
pub fn location_plan(own: &str, current: &str, target: &str) -> LocationPlan {
    if target == current {
        LocationPlan::Scroll
    } else if target == own {
        LocationPlan::ShowOwnFile
    } else {
        LocationPlan::ShowReference(target.to_string())
    }
}

// ── Permalink (A.6) ──────────────────────────────────────────────────────────────────────

/// What `Y` copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permalink {
    /// `owner/repo@<7-char sha>:<path>:<line>`.
    pub short: String,
    /// The GitHub blob URL, when the repo's origin is GitHub.
    pub url: Option<String>,
}

impl Permalink {
    /// The clipboard text: the short form, then the URL on its own line when there is one.
    pub fn clipboard_text(&self) -> String {
        match &self.url {
            Some(url) => format!("{}\n{url}", self.short),
            None => self.short.clone(),
        }
    }
}

/// Whether `origin_url` (the tokenless origin remote the scan recorded) is on github.com, in any
/// of the forms git prints it: `https://github.com/o/r(.git)`, `git@github.com:o/r.git`,
/// `ssh://git@github.com/o/r.git`.
pub fn is_github_origin(origin_url: &str) -> bool {
    let o = origin_url.trim();
    let host_ok = |rest: &str| {
        let host = rest
            .split(['/', ':'])
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("");
        host.eq_ignore_ascii_case("github.com")
    };
    if let Some(rest) = o
        .strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"))
        .or_else(|| o.strip_prefix("ssh://"))
        .or_else(|| o.strip_prefix("git://"))
    {
        return host_ok(rest);
    }
    // scp-like: git@github.com:owner/repo.git
    o.contains(':') && !o.contains("://") && host_ok(o)
}

/// Percent-encode a repo path for a URL: unreserved characters and `/` stay, every other byte
/// becomes `%XX`.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The permalink for `repo@sha:path:line`. `None` when no commit is known: a permalink to the
/// working tree would point at nothing anyone else could open. It carries no file content, so
/// it cannot carry a secret.
pub fn permalink(
    repo: &str,
    sha: &str,
    path: &str,
    line: usize,
    origin_url: Option<&str>,
) -> Option<Permalink> {
    if sha.is_empty() {
        return None;
    }
    let short = format!("{repo}@{}:{path}:{line}", short_sha(sha));
    let url = origin_url.filter(|o| is_github_origin(o)).map(|_| {
        format!(
            "https://github.com/{repo}/blob/{sha}/{}#L{line}",
            encode_path(path)
        )
    });
    Some(Permalink { short, url })
}

// ── The popover (A.5) ────────────────────────────────────────────────────────────────────

/// How much to trust a rule: the Authority column's three tiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    Enforced,
    Preview,
    Advisory,
}

impl Authority {
    /// The tier of `f`: preview first, then the deterministic floor, else advisory. The floor
    /// set lives with the rules table, so the caller says whether the rule is on it.
    pub fn of(f: &FindingView, is_floor_rule: bool) -> Self {
        if f.preview {
            Authority::Preview
        } else if is_floor_rule {
            Authority::Enforced
        } else {
            Authority::Advisory
        }
    }

    /// The same words the Authority column's badge uses.
    pub fn label(self) -> &'static str {
        match self {
            Authority::Enforced => "Rule \u{b7} enforced",
            Authority::Preview => "Preview \u{b7} not enforced until wired",
            Authority::Advisory => "AI \u{b7} advisory",
        }
    }
}

/// The directive line is cut at this many characters.
pub const DIRECTIVE_CHARS: usize = 160;

/// The headline is cut at this many characters.
pub const HEADLINE_CHARS: usize = 200;

/// Related findings listed before "and N more".
pub const RELATED_SHOWN: usize = 4;

/// `text` cut to at most `max` chars (never inside a character), ending in `...` when cut.
pub fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut cut: String = text.chars().take(keep).collect();
    cut.push_str("...");
    cut
}

/// The first sentence of `text`: up to the first full stop followed by a space or the end,
/// else the whole text; trimmed, and cut at [`HEADLINE_CHARS`].
pub fn first_sentence(text: &str) -> String {
    let t = text.trim();
    let end = t
        .char_indices()
        .find(|(i, c)| {
            matches!(c, '.' | '!' | '?')
                && t[i + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map_or(t.len(), |(i, c)| i + c.len_utf8());
    cut_chars(t[..end].trim(), HEADLINE_CHARS)
}

/// Another finding in the same file, as a popover link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelatedFinding {
    pub key: String,
    pub line: usize,
    pub rule_id: String,
    /// The cursor can land on it (it is still a row of the queue).
    pub navigable: bool,
}

/// Everything the popover says about one finding. Eight lines at most: see [`line_count`].
///
/// [`line_count`]: PopoverView::line_count
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopoverView {
    pub key: String,
    /// Line 1: the rule id (a link to the Rule Hub), the kind badge and the authority badge.
    pub rule_id: String,
    pub kind_tag: &'static str,
    pub authority: Authority,
    /// Line 2: the first sentence of the detail for a defect, the deviation claim for a deviation.
    pub headline: Option<String>,
    /// Line 3: the rule's directive, cut at [`DIRECTIVE_CHARS`].
    pub directive: Option<String>,
    /// Line 4.
    pub severity: String,
    pub confidence: &'static str,
    pub effort: Option<String>,
    /// Line 5: calibration's reason for hedging, shown in bold.
    pub needs_review: Option<String>,
    /// Line 6: the disposition, and the "previously" line when decision memory has one.
    pub disposition: String,
    /// Decision memory (B.5): `PriorDecision::chip_text` for this finding's shape, set by the
    /// caller. `None` is no chip, and the line is then simply absent. It shares line 6 with the
    /// disposition, so it never adds a line.
    pub previously: Option<String>,
    /// Line 7: other findings in this file.
    pub related: Vec<RelatedFinding>,
    /// How many of those are not listed.
    pub related_more: usize,
    /// Line 8: other rules violated at this same place.
    pub also_matches: Vec<String>,
}

impl PopoverView {
    /// The lines that have something to say. Never more than eight.
    pub fn line_count(&self) -> usize {
        1 + usize::from(self.headline.is_some())
            + usize::from(self.directive.is_some())
            + 1
            + usize::from(self.needs_review.is_some())
            + 1
            + usize::from(!self.related.is_empty())
            + usize::from(!self.also_matches.is_empty())
    }
}

/// The popover for `f`. `items` is [`crate::viewer::file_findings`] for the finding's file;
/// `directive` is the rule's text from the descriptions already loaded; `previously` is the
/// decision-memory line (B.5), `None` when the draft has nothing on this shape.
pub fn popover_view(
    f: &FindingView,
    state: TriageState,
    directive: Option<&str>,
    authority: Authority,
    items: &[FileFinding],
    previously: Option<String>,
) -> PopoverView {
    let key = finding_key(f);
    let (body, needs_review) = split_needs_review(&f.detail);
    let headline = deviation_headline(f)
        .map(|h| h.sentence())
        .or_else(|| Some(first_sentence(&body)).filter(|s| !s.is_empty()));
    let others: Vec<RelatedFinding> = items
        .iter()
        .filter(|i| i.key != key)
        .map(|i| RelatedFinding {
            key: i.key.clone(),
            line: i.line,
            rule_id: i.rule_id.clone(),
            navigable: i.navigable,
        })
        .collect();
    let related_more = others.len().saturating_sub(RELATED_SHOWN);
    let related = others.into_iter().take(RELATED_SHOWN).collect();
    let mut disposition = state.label().to_string();
    if f.status != "active" && !f.status.is_empty() {
        disposition.push_str(&format!(" ({})", f.status));
    }
    PopoverView {
        key,
        rule_id: f.rule_id.clone(),
        kind_tag: kind_tag(f),
        authority,
        headline,
        directive: directive
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| cut_chars(d, DIRECTIVE_CHARS)),
        severity: f.severity.clone(),
        confidence: confidence_tag(f),
        effort: f.effort.clone().filter(|e| !e.is_empty()),
        needs_review: needs_review.filter(|r| !r.is_empty()).or_else(|| {
            (f.needs_review || f.confidence.as_deref() == Some("needs-review"))
                .then(|| "needs review".to_string())
        }),
        disposition,
        previously,
        related,
        related_more,
        also_matches: f.also_matches.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::TriageState;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| (*s).to_string()).collect()
    }

    // ── search ──

    #[test]
    fn search_is_a_case_insensitive_substring_with_char_columns() {
        let l = lines(&["Foo foo FOO", "no", "xfoo"]);
        assert_eq!(
            search_lines(&l, "foo"),
            vec![(1, 0), (1, 4), (1, 8), (3, 1)]
        );
        assert!(search_lines(&l, "").is_empty());
        assert!(search_lines(&l, "absent").is_empty());
        // Matches do not overlap and do not span lines.
        assert_eq!(search_lines(&lines(&["aaaa"]), "aa"), vec![(1, 0), (1, 2)]);
        assert!(search_lines(&lines(&["ab", "cd"]), "bc").is_empty());
    }

    #[test]
    fn search_columns_are_chars_not_bytes() {
        // The match is after two multi-byte chars: byte column 5, char column 2.
        let l = lines(&["éüNeedle", "日本語 needle 日本"]);
        assert_eq!(search_lines(&l, "needle"), vec![(1, 2), (2, 4)]);
        // Case folding keeps the column: `İ` lowercases to two chars in `str::to_lowercase`.
        assert_eq!(search_lines(&lines(&["İx x"]), "x"), vec![(1, 1), (1, 3)]);
        assert_eq!(
            search_lines(&lines(&["ÉCOLE école"]), "école"),
            vec![(1, 0), (1, 6)]
        );
    }

    #[test]
    fn the_match_list_is_capped() {
        let l = vec!["a".repeat(MAX_MATCHES + 50)];
        assert_eq!(search_lines(&l, "a").len(), MAX_MATCHES);
    }

    fn view(from: usize, lines: &[&str], total: usize) -> SourceFileView {
        serde_json::from_value(serde_json::json!({
            "status": "ok", "source": "git", "total_lines": total,
            "window": {"from": from, "to": from + lines.len() - 1, "total_lines": total},
            "lines": lines.iter().map(|l| vec![serde_json::json!([null, l])]).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn search_view_reports_file_lines_and_the_scope_of_a_window() {
        let whole = view(1, &["let a = 1;", "let B = a;"], 2);
        let r = search_view(&whole, "A");
        assert_eq!(r.matches, vec![(1, 4), (2, 8)]);
        assert_eq!(r.needle_chars, 1);
        assert_eq!(r.scope_note, None);

        let windowed = view(101, &["x needle", "needle"], 5000);
        let r = search_view(&windowed, "needle");
        assert_eq!(r.matches, vec![(101, 2), (102, 0)]);
        assert_eq!(
            r.scope_note.as_deref(),
            Some("searching lines 101 to 102 of 5000 only")
        );
        assert!(search_status("needle", &r, Some(0), false).contains("1 of 2"));
        assert!(search_status("needle", &r, Some(0), false).contains("lines 101 to 102"));
    }

    #[test]
    fn cycling_wraps_and_says_so() {
        assert_eq!(cycle_match(None, 0, true), None);
        assert_eq!(cycle_match(None, 3, true), Some((0, false)));
        assert_eq!(cycle_match(None, 3, false), Some((2, false)));
        assert_eq!(cycle_match(Some(0), 3, true), Some((1, false)));
        assert_eq!(cycle_match(Some(2), 3, true), Some((0, true)));
        assert_eq!(cycle_match(Some(1), 3, false), Some((0, false)));
        assert_eq!(cycle_match(Some(0), 3, false), Some((2, true)));
        assert_eq!(cycle_match(Some(0), 1, true), Some((0, true)));
    }

    #[test]
    fn the_status_counts_and_names_a_wrap() {
        let r = SearchResult {
            matches: vec![(1, 0), (2, 0), (3, 0)],
            needle_chars: 1,
            ..SearchResult::default()
        };
        assert_eq!(search_status("a", &r, Some(1), false), "2 of 3");
        assert_eq!(search_status("a", &r, Some(0), true), "1 of 3 (wrapped)");
        assert_eq!(
            search_status("", &r, None, false),
            "Type to search this file"
        );
        let none = SearchResult::default();
        assert_eq!(search_status("zz", &none, None, false), "No matches");
        let capped = SearchResult { capped: true, ..r };
        assert_eq!(search_status("a", &capped, Some(0), false), "1 of 3+");
    }

    #[test]
    fn typing_starts_on_the_first_match_at_or_after_the_line() {
        let m = [(2, 0), (10, 4), (30, 1)];
        assert_eq!(first_match_from(&m, 1), Some(0));
        assert_eq!(first_match_from(&m, 10), Some(1));
        assert_eq!(first_match_from(&m, 11), Some(2));
        assert_eq!(
            first_match_from(&m, 99),
            Some(0),
            "past the last: the first"
        );
        assert_eq!(first_match_from(&[], 1), None);
    }

    #[test]
    fn the_search_field_keys_are_enter_shift_enter_and_escape() {
        assert_eq!(search_key("Enter", false), Some(SearchKey::Next));
        assert_eq!(search_key("Enter", true), Some(SearchKey::Previous));
        assert_eq!(search_key("Escape", false), Some(SearchKey::Close));
        for k in ["a", "/", " ", "ArrowDown", "Tab", "Backspace"] {
            assert_eq!(search_key(k, false), None, "{k} is text");
        }
    }

    #[test]
    fn marks_cut_segments_at_match_edges_and_rejoin_exactly() {
        let segs: Vec<Segment> = vec![
            (Some(0), "let ".to_string()),
            (None, "éfoo = foo;".to_string()),
        ];
        // "let éfoo = foo;": matches at char 5 (len 3) and char 11 (len 3).
        let marked = mark_matches(&segs, &[(5, 3), (11, 3)], Some(11));
        let rejoined: String = marked.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(rejoined, "let éfoo = foo;");
        let shown: Vec<(&str, Mark)> = marked.iter().map(|m| (m.text.as_str(), m.mark)).collect();
        assert_eq!(
            shown,
            vec![
                ("let ", Mark::None),
                ("é", Mark::None),
                ("foo", Mark::Match),
                (" = ", Mark::None),
                ("foo", Mark::Current),
                (";", Mark::None),
            ]
        );
        // The class of the segment a piece came from is kept.
        assert_eq!(marked[0].class, Some(0));
        assert_eq!(marked[2].class, None);
    }

    #[test]
    fn a_match_spanning_two_segments_is_marked_in_both() {
        let segs: Vec<Segment> = vec![(Some(1), "ab".to_string()), (Some(2), "cd".to_string())];
        let marked = mark_matches(&segs, &[(1, 2)], None);
        let shown: Vec<(&str, Option<usize>, Mark)> = marked
            .iter()
            .map(|m| (m.text.as_str(), m.class, m.mark))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("a", Some(1), Mark::None),
                ("b", Some(1), Mark::Match),
                ("c", Some(2), Mark::Match),
                ("d", Some(2), Mark::None),
            ]
        );
        assert!(mark_matches(&[], &[(0, 1)], None).is_empty());
    }

    // ── symbols ──

    #[test]
    fn the_longest_identifier_is_found_and_ties_go_to_the_first() {
        assert_eq!(
            longest_identifier("let query = build_sql(x);").as_deref(),
            Some("build_sql")
        );
        assert_eq!(longest_identifier("ab cd").as_deref(), Some("ab"));
        assert_eq!(longest_identifier("a1b2 _x").as_deref(), Some("a1b2"));
        assert_eq!(longest_identifier("9lives ok").as_deref(), Some("lives"));
        assert_eq!(longest_identifier("== -- ()"), None);
        assert_eq!(
            longest_identifier("tail_ident").as_deref(),
            Some("tail_ident")
        );
        assert_eq!(longest_identifier(""), None);
    }

    fn caps(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn the_prefill_is_the_capture_when_there_is_one() {
        let line = "let query = build_sql(x);";
        assert_eq!(
            symbol_prefill(&caps(&[]), line).as_deref(),
            Some("build_sql")
        );
        assert_eq!(
            symbol_prefill(&caps(&[("table", "profiles")]), line).as_deref(),
            Some("profiles")
        );
        // A known name beats an unknown one, whatever the map order.
        assert_eq!(
            symbol_prefill(
                &caps(&[("a-thing", "zzz"), ("function-name", "send-invite")]),
                line
            )
            .as_deref(),
            Some("send-invite")
        );
        // An unusable capture is skipped; an unknown name still counts.
        assert_eq!(
            symbol_prefill(&caps(&[("table", "a b")]), line).as_deref(),
            Some("build_sql")
        );
        assert_eq!(
            symbol_prefill(&caps(&[("bucket", "avatars")]), line).as_deref(),
            Some("avatars")
        );
        assert_eq!(symbol_prefill(&caps(&[]), "== --"), None);
    }

    #[test]
    fn a_references_answer_says_what_it_means() {
        let ok: SymbolRefsView = serde_json::from_value(serde_json::json!({
            "status": "ok", "source": "git", "symbol": "foo",
            "hits": [{"path": "a.rs", "line": 3, "text": "foo()"}], "more": false
        }))
        .unwrap();
        assert_eq!(ok.outcome(), RefsOutcome::List);
        assert_eq!(ok.banner(), None);
        assert_eq!(ok.summary(), "1 reference to foo");

        let many = SymbolRefsView {
            hits: vec![RefHit::default(), RefHit::default()],
            more: true,
            ..ok.clone()
        };
        assert_eq!(many.summary(), "2 references to foo (more not shown)");
        let none = SymbolRefsView {
            hits: vec![],
            ..ok.clone()
        };
        assert_eq!(none.summary(), "No references to foo");
        // Zero listed with the output cut is not "none".
        let cut = SymbolRefsView {
            hits: vec![],
            more: true,
            ..ok.clone()
        };
        assert!(cut.summary().contains("cut short"), "{}", cut.summary());

        let wt = SymbolRefsView {
            status: "commit_unavailable".into(),
            source: "working_tree".into(),
            reason: "Inspected commit not available in this clone; showing the working tree".into(),
            ..ok.clone()
        };
        assert_eq!(wt.outcome(), RefsOutcome::List);
        assert!(wt
            .banner()
            .unwrap()
            .starts_with("Inspected commit not available"));

        for (status, want) in [
            ("source_deleted", "Source deleted"),
            ("file_missing", "The repo was not found"),
            ("too_large", "The search took too long"),
            ("unavailable", "The search could not run"),
            ("anything else", "could not be loaded"),
        ] {
            let v = SymbolRefsView {
                status: status.into(),
                reason: String::new(),
                ..SymbolRefsView::default()
            };
            match v.outcome() {
                RefsOutcome::Note(t) => assert!(t.contains(want), "{status}: {t}"),
                other => panic!("{status}: {other:?}"),
            }
            assert_eq!(v.banner(), None);
        }
        // A search that did not run is a failure heading, never "No references".
        for status in ["unavailable", "too_large", "source_deleted", "file_missing"] {
            let v = SymbolRefsView {
                status: status.into(),
                symbol: "foo".into(),
                ..SymbolRefsView::default()
            };
            assert_eq!(v.summary(), "Could not search for foo", "{status}");
            assert!(v.hits.is_empty());
        }
        // A server reason wins over the fallback.
        let deleted = SymbolRefsView {
            status: "source_deleted".into(),
            reason: "Source deleted on 3 Oct 2026, re-fetch to view".into(),
            ..SymbolRefsView::default()
        };
        assert_eq!(
            deleted.outcome(),
            RefsOutcome::Note("Source deleted on 3 Oct 2026, re-fetch to view".into())
        );
        // An empty body parses to nothing to show.
        let empty: SymbolRefsView = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(matches!(empty.outcome(), RefsOutcome::Note(_)));
    }

    #[test]
    fn the_selection_stops_at_the_ends() {
        assert_eq!(move_selection(0, 3, FocusMove::Up), 0);
        assert_eq!(move_selection(0, 3, FocusMove::Down), 1);
        assert_eq!(move_selection(2, 3, FocusMove::Down), 2);
        assert_eq!(move_selection(5, 0, FocusMove::Down), 0);
    }

    // ── locations ──

    #[test]
    fn a_place_is_a_scroll_a_return_or_a_reference() {
        let own = "src/db.rs";
        // On the finding's file: a hit in the same file is a scroll.
        assert_eq!(location_plan(own, own, own), LocationPlan::Scroll);
        // On the finding's file: a hit elsewhere is a reference.
        assert_eq!(
            location_plan(own, own, "src/x.rs"),
            LocationPlan::ShowReference("src/x.rs".into())
        );
        // On a reference: a hit in the same reference is a scroll...
        assert_eq!(
            location_plan(own, "src/x.rs", "src/x.rs"),
            LocationPlan::Scroll
        );
        // ...in the finding's file is the way back...
        assert_eq!(
            location_plan(own, "src/x.rs", own),
            LocationPlan::ShowOwnFile
        );
        // ...and in a third file is another reference.
        assert_eq!(
            location_plan(own, "src/x.rs", "src/y.rs"),
            LocationPlan::ShowReference("src/y.rs".into())
        );
    }

    // ── permalink ──

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn the_permalink_is_repo_sha_path_line_and_carries_no_content() {
        let p = permalink("acme/app", SHA, "src/main.rs", 42, None).unwrap();
        assert_eq!(p.short, "acme/app@0123456:src/main.rs:42");
        assert_eq!(p.url, None);
        assert_eq!(p.clipboard_text(), "acme/app@0123456:src/main.rs:42");

        let g = permalink(
            "acme/app",
            SHA,
            "src/a b#c.rs",
            7,
            Some("git@github.com:acme/app.git"),
        )
        .unwrap();
        assert_eq!(
            g.url.as_deref(),
            Some(format!("https://github.com/acme/app/blob/{SHA}/src/a%20b%23c.rs#L7").as_str())
        );
        assert_eq!(
            g.clipboard_text(),
            format!(
                "acme/app@0123456:src/a b#c.rs:7\nhttps://github.com/acme/app/blob/{SHA}/src/a%20b%23c.rs#L7"
            )
        );
        // No commit, no permalink.
        assert_eq!(permalink("acme/app", "", "a.rs", 1, None), None);
    }

    #[test]
    fn only_a_github_origin_gets_a_url() {
        for yes in [
            "https://github.com/acme/app.git",
            "https://github.com/acme/app",
            "http://github.com/acme/app",
            "git@github.com:acme/app.git",
            "ssh://git@github.com/acme/app.git",
            "git://github.com/acme/app.git",
            "https://GitHub.com/acme/app",
        ] {
            assert!(is_github_origin(yes), "{yes}");
        }
        for no in [
            "",
            "https://gitlab.com/acme/app.git",
            "git@gitlab.com:acme/app.git",
            "https://github.com.evil.example/acme/app",
            "https://example.com/github.com/acme/app",
            "https://evilgithub.com/acme/app",
            "/srv/git/app.git",
            "file:///srv/github.com/app.git",
        ] {
            assert!(!is_github_origin(no), "{no}");
        }
        assert_eq!(
            permalink(
                "acme/app",
                SHA,
                "a.rs",
                1,
                Some("https://gitlab.com/acme/app")
            )
            .unwrap()
            .url,
            None
        );
    }

    // ── popover ──

    fn finding(json: serde_json::Value) -> FindingView {
        let mut base = serde_json::json!({
            "repo": "o/r", "path": "src/a.rs", "line": 10, "rule_id": "SQL-INJECTION-1",
            "severity": "high", "snippet": "db.query(x)", "detail": "", "kind": "defect"
        });
        for (k, v) in json.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn item(rule: &str, line: usize, navigable: bool) -> FileFinding {
        FileFinding {
            key: format!("k-{rule}-{line}"),
            line,
            rule_id: rule.to_string(),
            severity: "low".to_string(),
            state: TriageState::Unresolved,
            navigable,
        }
    }

    #[test]
    fn the_headline_is_the_first_sentence_of_a_defects_detail() {
        let f = finding(serde_json::json!({
            "detail": "User input reaches the query string. It is built by concatenation. [needs review: dynamic table name]",
            "confidence": "needs-review"
        }));
        let p = popover_view(
            &f,
            TriageState::Unresolved,
            None,
            Authority::Advisory,
            &[],
            None,
        );
        assert_eq!(
            p.headline.as_deref(),
            Some("User input reaches the query string.")
        );
        assert_eq!(p.needs_review.as_deref(), Some("dynamic table name"));
        assert_eq!(p.kind_tag, "defect");
        assert_eq!(p.confidence, "needs-review");
    }

    #[test]
    fn a_deviation_reads_its_structured_claim_not_prose() {
        let f = finding(serde_json::json!({
            "rule_id": "API-VERSIONING-1", "kind": "deviation",
            "deviating_sites": 2, "assessed_sites": 47,
            "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints",
            "detail": "Some unrelated prose. More prose."
        }));
        let p = popover_view(
            &f,
            TriageState::Unresolved,
            None,
            Authority::Advisory,
            &[],
            None,
        );
        let claim = deviation_headline(&f).unwrap().sentence();
        assert_eq!(p.headline, Some(claim));
        assert_eq!(p.kind_tag, "deviation");
    }

    #[test]
    fn the_first_sentence_stops_at_a_full_stop_and_is_char_safe() {
        assert_eq!(first_sentence("One. Two."), "One.");
        assert_eq!(first_sentence("No stop here"), "No stop here");
        assert_eq!(first_sentence("v1.2 is old. Next"), "v1.2 is old.");
        assert_eq!(first_sentence("Why? Because."), "Why?");
        assert_eq!(first_sentence("  "), "");
        let long = format!("{}.", "é".repeat(HEADLINE_CHARS + 40));
        let cut = first_sentence(&long);
        assert_eq!(cut.chars().count(), HEADLINE_CHARS);
        assert!(cut.ends_with("..."));
    }

    #[test]
    fn the_directive_is_cut_at_160_chars_without_splitting_a_character() {
        let long = "日".repeat(300);
        let cut = cut_chars(&long, DIRECTIVE_CHARS);
        assert_eq!(cut.chars().count(), DIRECTIVE_CHARS);
        assert!(cut.ends_with("..."));
        let exact = "x".repeat(DIRECTIVE_CHARS);
        assert_eq!(cut_chars(&exact, DIRECTIVE_CHARS), exact);
        let f = finding(serde_json::json!({}));
        let p = popover_view(
            &f,
            TriageState::Unresolved,
            Some(&format!("  {long}  ")),
            Authority::Enforced,
            &[],
            None,
        );
        assert_eq!(p.directive.as_deref().map(|d| d.chars().count()), Some(160));
        let none = popover_view(
            &f,
            TriageState::Unresolved,
            Some("   "),
            Authority::Enforced,
            &[],
            None,
        );
        assert_eq!(none.directive, None);
    }

    #[test]
    fn related_findings_exclude_this_one_and_are_capped() {
        let f = finding(serde_json::json!({}));
        let me = FileFinding {
            key: finding_key(&f),
            line: 10,
            rule_id: "SQL-INJECTION-1".into(),
            severity: "high".into(),
            state: TriageState::Unresolved,
            navigable: true,
        };
        let mut items = vec![me.clone()];
        for n in 0..6 {
            items.push(item("R", 20 + n, n % 2 == 0));
        }
        let p = popover_view(
            &f,
            TriageState::Unresolved,
            None,
            Authority::Advisory,
            &items,
            None,
        );
        assert_eq!(p.related.len(), RELATED_SHOWN);
        assert_eq!(p.related_more, 2);
        assert!(p.related.iter().all(|r| r.key != me.key));
        assert!(p.related[0].navigable && !p.related[1].navigable);
        let alone = popover_view(
            &f,
            TriageState::Unresolved,
            None,
            Authority::Advisory,
            &[me],
            None,
        );
        assert!(alone.related.is_empty() && alone.related_more == 0);
    }

    #[test]
    fn the_authority_tiers_use_the_columns_words() {
        let normal = finding(serde_json::json!({}));
        let preview = finding(serde_json::json!({"preview": true}));
        assert_eq!(Authority::of(&normal, true), Authority::Enforced);
        assert_eq!(Authority::of(&normal, false), Authority::Advisory);
        assert_eq!(Authority::of(&preview, true), Authority::Preview);
        assert_eq!(Authority::Enforced.label(), "Rule \u{b7} enforced");
        assert_eq!(Authority::Advisory.label(), "AI \u{b7} advisory");
        assert!(Authority::Preview.label().starts_with("Preview"));
    }

    #[test]
    fn the_popover_is_never_more_than_eight_lines_and_previously_adds_no_line() {
        let full = finding(serde_json::json!({
            "detail": "It is bad. [needs review: maybe fine]",
            "effort": "low", "also_matches": ["A-1", "B-2"],
            "status": "suppressed-baseline"
        }));
        let items = vec![item("R", 20, true)];
        let p = popover_view(
            &full,
            TriageState::Ignored,
            Some("Use parameters."),
            Authority::Enforced,
            &items,
            None,
        );
        assert_eq!(p.line_count(), 8);
        assert_eq!(p.disposition, "Ignored (suppressed-baseline)");
        assert_eq!(p.effort.as_deref(), Some("low"));
        assert_eq!(p.also_matches, vec!["A-1", "B-2"]);
        assert_eq!(p.previously, None, "no chip when the caller has none");
        // A caller that has a memory line sets it, and no line is added.
        let with = popover_view(
            &full,
            TriageState::Ignored,
            Some("Use parameters."),
            Authority::Enforced,
            &items,
            Some("Previously on this shape: Ignored".into()),
        );
        assert_eq!(
            with.previously.as_deref(),
            Some("Previously on this shape: Ignored")
        );
        assert_eq!(with.line_count(), 8);
        // The sparsest popover still has its three fixed lines.
        let bare = finding(serde_json::json!({"status": "active"}));
        let b = popover_view(
            &bare,
            TriageState::Unresolved,
            None,
            Authority::Advisory,
            &[],
            None,
        );
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.disposition, "Unresolved");
    }
}
