//! `WeakTokenRandomnessChecker`: LEXICAL (comment-aware, not a bare regex) detection of
//! `SEC-NO-WEAK-TOKEN-RANDOMNESS-1` — a general-purpose PRNG (`Math.random`, Python's
//! `random.*`, C-family `rand()`, `new Random()`, Go's `math/rand`) OR another non-crypto,
//! low-entropy/guessable source (a wall-clock timestamp — `Date.now`, `performance.now`,
//! `new Date().getTime()`, Python's `time.time()` — or a process id — `os.getpid()`) feeding a
//! value whose surrounding NAME (the variable/field/function it flows into) marks it
//! security-relevant: a token, secret, password, nonce, session id, CSRF value, OTP, reset
//! code, API key, or a share-link / access identifier. See
//! `docs/plans/2026-09-29_codebase-inspection-hardening.md` D2 and
//! `crates/rules/principles/universal/sec-no-weak-token-randomness-1.toml` (P3 already
//! grounded this rule id with CWE-330/338 + the OWASP Cryptographic Storage cheat sheet — this
//! checker emits under that SAME id rather than inventing a new one).
//!
//! # Why "look at the surrounding identifier," not just "PRNG call present"
//!
//! `Math.random()` itself is completely benign — the defect is specifically a non-crypto PRNG
//! feeding a value that FUNCTIONS AS A CREDENTIAL. A bare "PRNG call found" checker would flag
//! every animation-jitter, sampling, and shuffle call in the repo, drowning the real defect in
//! noise. Instead this checker recovers the identifier the PRNG's result is assigned to (an
//! `=`/`:` target on the same line), the call it's passed into (`generateShareToken(Math.random())`),
//! or the nearest enclosing function/method declaration (`function createShareToken() { return
//! Math.random()... }`) and classifies THAT name. Non-security uses (UI jitter, animation
//! delay, array shuffling, sampling, backoff/retry, test fixtures, color generation) are never
//! flagged because their names carry none of the security vocabulary — see
//! [`DISCRIMINATOR_KEYWORDS`] for the belt-and-suspenders suppression on the few WEAK keywords
//! ambiguous enough to collide with a non-security name.
//!
//! # Severity
//!
//! Medium by default (SEC-NO-WEAK-TOKEN-RANDOMNESS-1's own baseline). Escalates to High when
//! the identifier reads as an UNAUTHENTICATED access credential on its own — a share-link or
//! access token/key/id (see [`is_high_severity_identifier`]) — because in that shape the weak
//! PRNG output alone grants access, with no other check in the path.
//!
//! # What this deliberately does NOT do
//!
//! No string-literal awareness (mirrors [`crate::ui_dates::UtcDatesChecker`]'s documented
//! discipline) and no cross-file identifier tracking — the context window is the current line,
//! a bounded lookback within the SAME file for the enclosing function name, and ONE forward hop
//! through a same-function intermediate local (see [`forward_taint_identifiers`]): `const raw =
//! Math.random(); ...; const shareToken = 'x_' + raw;` still classifies even though neither the
//! PRNG line's own target nor the enclosing function name names a credential. A PRNG value
//! whose security-relevant name only surfaces two hops away, several functions away, or in a
//! different file, is a false negative by design (explicitly preferred over a false positive
//! per the plan) — completeness-over-precision widens the taint window, not removes its bound.

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH, SEVERITY_MEDIUM};

pub const RULE_WEAK_TOKEN_RANDOMNESS: &str = "SEC-NO-WEAK-TOKEN-RANDOMNESS-1";

const RULE_IDS: &[&str] = &[RULE_WEAK_TOKEN_RANDOMNESS];

const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.py",
    "**/*.go",
    "**/*.rb",
    "**/*.java",
    "**/*.cs",
];

const SKIP_PATH_FRAGMENTS: &[&str] = &[
    "node_modules/",
    "/dist/",
    "/build/",
    "/.next/",
    ".min.js",
    "vendor/",
];

/// Non-crypto PRNG call shapes AND other low-entropy/guessable value sources this checker
/// recognizes. Every match is boundary-checked — the character immediately BEFORE the match
/// must not be an identifier character (letter/digit/`_`/`$`) — which is load-bearing for
/// short, generic needles like `rand(` that would otherwise match inside an unrelated longer
/// identifier (`errand(`, `grandTotal(`), and for `time.time(` which would otherwise match
/// inside Python's unrelated `datetime.time(` constructor; the longer, already-distinctive
/// needles (`Math.random(`, `random.random(`) don't strictly need it, but it costs nothing to
/// apply uniformly (see [`find_prng_call`]).
///
/// The timestamp/pid entries (`Date.now(`, `performance.now(`, `new Date().getTime(`,
/// `time.time(`, `os.getpid(`) are not PRNGs at all — they're even MORE predictable, since
/// they're not random-shaped in the first place, just a coarse clock reading or a small
/// sequential id an attacker can often observe or narrow to a small window directly. They
/// belong in this same needle list because the vulnerable CLASS is "any easily-guessable, non-
/// cryptographic value used to build a token/secret/id," not "specifically calls a PRNG
/// function" — see [`entropy_source_kind`] for the wording split used in the finding message.
const PRNG_NEEDLES: &[&str] = &[
    "Math.random(",
    "random.random(",
    "random.randint(",
    "random.randrange(",
    "random.choice(",
    "random.getrandbits(",
    "random.sample(",
    "new Random(",
    "rand.Intn(",
    "rand.Int63(",
    "rand.Int31(",
    "rand.Float64(",
    "rand(",
    "Date.now(",
    "performance.now(",
    "new Date().getTime(",
    "time.time(",
    "os.getpid(",
];

/// Substrings that, once found inside a NORMALIZED (lowercased, non-alphanumeric stripped)
/// candidate identifier, mark it as unambiguously security-relevant REGARDLESS of any
/// discriminator also present — a variable can't simultaneously be "the CSRF token" and "just
/// UI jitter," so these never get suppressed.
const STRONG_SECURITY_KEYWORDS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "apikey",
    "privatekey",
    "resetcode",
];

/// Substrings that mark POSSIBLE security relevance, but are common enough in non-security
/// contexts (a `sessionId` for an analytics ping, a `nonce` in a CSS animation-name generator)
/// that a [`DISCRIMINATOR_KEYWORDS`] hit on the SAME identifier suppresses the match — the
/// "prefer false negative on an ambiguous non-security name" instruction.
const WEAK_SECURITY_KEYWORDS: &[&str] = &[
    "session",
    "nonce",
    "csrf",
    "otp",
    "authcode",
    "accesscode",
    "verificationcode",
    "shareid",
    "accessid",
    "linkid",
];

/// Substrings marking a plainly non-security use of randomness. Checked against the SAME
/// normalized identifier a [`WEAK_SECURITY_KEYWORDS`] hit came from — never against
/// [`STRONG_SECURITY_KEYWORDS`], which are unambiguous.
const DISCRIMINATOR_KEYWORDS: &[&str] = &[
    "jitter",
    "delay",
    "backoff",
    "retry",
    "throttle",
    "debounce",
    "animation",
    "anim",
    "shuffle",
    "sample",
    "sampling",
    "color",
    "colour",
    "confetti",
    "particle",
    "mock",
    "fixture",
    "testdata",
    "placeholder",
    "avatar",
    "position",
    "offset",
    "interval",
    "timeout",
    "noise",
];

pub struct WeakTokenRandomnessChecker;

impl ArchChecker for WeakTokenRandomnessChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        repo.files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .filter(|(path, _)| !SKIP_PATH_FRAGMENTS.iter().any(|frag| path.contains(frag)))
            .flat_map(|(path, content)| violations_in_file(path, content))
            .collect()
    }
}

/// Whether `path`'s language uses `#` line comments with no block-comment syntax (Python,
/// Ruby) rather than the `//` + `/* */` family (JS/TS, Go, Java, C#).
fn is_hash_comment_language(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".rb")
}

fn violations_in_file(path: &str, content: &str) -> Vec<ArchViolation> {
    let hash_comments = is_hash_comment_language(path);
    let lines: Vec<&str> = content.lines().collect();

    // Comment-strip the whole file up front (not lazily inside the scan loop below) because the
    // forward-taint step needs RANDOM ACCESS to later lines' comment-stripped code, not just a
    // running cursor over the current line.
    let mut stripped: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_block_comment = false;
    for raw_line in &lines {
        let (code, next_in_block) = strip_comments(raw_line, in_block_comment, hash_comments);
        in_block_comment = next_in_block;
        stripped.push(code);
    }

    let mut violations = Vec::new();

    for (idx, code) in stripped.iter().enumerate() {
        if let Some((needle, match_start)) = find_prng_call(code) {
            let mut identifiers = candidate_identifiers(&lines, idx, code, match_start);
            if classify_named(&identifiers).is_none() {
                // Same-line target, enclosing call, and enclosing function name all came up
                // empty. One more shot: if the PRNG value was assigned to an INTERMEDIATE local
                // (e.g. `const raw = Math.random()`), follow that local forward through the rest
                // of the function for a line that consumes it while building a security-named
                // value (`const shareToken = 'x_' + raw`).
                if let Some(intermediate) = assignment_target(&code[..match_start]) {
                    identifiers.extend(forward_taint_identifiers(&stripped, idx, &intermediate));
                }
            }
            if let Some((severity, named)) = classify_named(&identifiers) {
                violations.push(ArchViolation {
                    rule_id: RULE_WEAK_TOKEN_RANDOMNESS.to_string(),
                    file: path.to_string(),
                    line: idx + 1,
                    object: Some(named.clone()),
                    severity,
                    message: message_for(needle, &named, severity),
                });
            }
        }
    }

    violations
}

/// One-hop forward taint: given `intermediate` (a local variable the PRNG/weak-entropy value on
/// line `from_line_idx` was assigned to), scan forward through up to 40 subsequent lines of the
/// SAME file for a line that both mentions `intermediate` as a whole word and either assigns it
/// into another identifier (`const shareToken = 'x_' + raw`) or passes it into a call
/// (`buildAccessToken(raw)`) — recovering that OTHER identifier as an additional classification
/// candidate. Lexical, not AST: `declaration_name` on a scanned line is treated as "a new
/// function/method starts here," which stops the scan — the same bounded, no-brace-counting
/// discipline [`enclosing_function_name`] already uses for its own backward lookback, so this
/// stays cheap and doesn't chase `intermediate` across an unrelated sibling function that
/// happens to reuse the same short local name.
fn forward_taint_identifiers(
    stripped_lines: &[String],
    from_line_idx: usize,
    intermediate: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    if intermediate.is_empty() {
        return out;
    }
    let bound = (from_line_idx + 1 + 40).min(stripped_lines.len());
    for line in &stripped_lines[from_line_idx + 1..bound] {
        if declaration_name(line).is_some() {
            break;
        }
        let Some(pos) = find_word(line, intermediate) else {
            continue;
        };
        let before = &line[..pos];
        if let Some(id) = assignment_target(before) {
            out.push(id);
        }
        if let Some(id) = enclosing_call_name(before) {
            out.push(id);
        }
    }
    out
}

/// Whether `text` contains `word` as a standalone identifier occurrence — the character
/// immediately before and after the match must not be an identifier character (letter/digit/
/// `_`/`$`) — mirroring [`find_prng_call`]'s own boundary discipline so `raw` doesn't match
/// inside `rawToken` or `drawnValue`. Returns the BYTE offset of the first such occurrence.
fn find_word(text: &str, word: &str) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut start = 0;
    while let Some(rel) = text[start..].find(word) {
        let pos = start + rel;
        let before_ok = text[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let after_ok = text[pos + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        if before_ok && after_ok {
            return Some(pos);
        }
        // Advance past this occurrence's start by at least one byte so we make forward
        // progress even when `word` is a single character (never zero-length here, guarded
        // above, but this keeps the step correct regardless of `word`'s length).
        start = pos + 1;
    }
    None
}

/// The clause describing WHY `needle`'s output is guessable, tailored to whether it's a PRNG
/// call (small/predictable internal state) or a bare clock/pid read (not random-shaped at all —
/// directly observable or narrow-window-guessable). Keeps [`message_for`] honest: calling
/// `Date.now()` "a pseudo-random number generator" would be inaccurate — it's not random in any
/// sense, which is arguably worse for a value used as a credential.
fn entropy_source_kind(needle: &str) -> &'static str {
    match needle {
        "Date.now(" | "performance.now(" | "new Date().getTime(" | "time.time(" => {
            "a wall-clock timestamp — not random at all, and often guessable to within a narrow \
             window from other observable signals (request timing, log timestamps, HTTP `Date` \
             headers)"
        }
        "os.getpid(" => {
            "the operating system's process id — a small, often sequential, externally \
             observable integer, not a source of unpredictability"
        }
        _ => {
            "a general-purpose pseudo-random number generator, designed for statistical \
             distribution rather than unpredictability"
        }
    }
}

fn message_for(needle: &str, identifier: &str, severity: &'static str) -> String {
    let call = needle.trim_end_matches('(');
    let source_desc = entropy_source_kind(needle);
    let escalation = if severity == SEVERITY_HIGH {
        " This value alone grants access (an unauthenticated share link or access token) — \
         anyone who can predict or brute-force the generator's output space forges a valid \
         credential without ever touching the underlying account, so this is rated High rather \
         than Medium."
    } else {
        ""
    };
    format!(
        "`{call}` — {source_desc} — is used to build `{identifier}`, whose name marks it as a \
         security-relevant credential (a token, secret, password, session id, or similar access \
         value). Its output is either drawn from a small, often time-seeded internal state or is \
         itself directly observable, so an attacker who observes or brute-forces a handful of \
         outputs can forge a valid value without ever compromising the account it protects. \
         Replace this with a cryptographically secure random source (crypto.randomBytes/\
         crypto.randomUUID in Node, Python's `secrets` module, SecureRandom on the JVM, or the \
         platform's equivalent) with at least 128 bits of entropy.{escalation}"
    )
}

/// Find the first PRNG call-shape in `code`, returning the matched needle and its BYTE offset.
/// `rand(` is boundary-checked (the char immediately before the match must not be an identifier
/// character) so it never fires inside a longer identifier like `errand(` or `grandTotal(`; the
/// other needles are distinctive enough not to need it, but the check is harmless for them too.
fn find_prng_call(code: &str) -> Option<(&'static str, usize)> {
    let mut best: Option<(&'static str, usize)> = None;
    for needle in PRNG_NEEDLES {
        if let Some(pos) = code.find(needle) {
            let prev = code[..pos].chars().next_back();
            let boundary_ok = match prev {
                None => true,
                Some(c) => !(c.is_alphanumeric() || c == '_' || c == '$'),
            };
            if !boundary_ok {
                continue;
            }
            if best.map(|(_, best_pos)| pos < best_pos).unwrap_or(true) {
                best = Some((needle, pos));
            }
        }
    }
    best
}

/// Every identifier this checker considers relevant context for the PRNG call at
/// `code[..match_start]` on line `line_idx` (0-based) of `lines`: the same-line assignment
/// target, the same-line enclosing call's callee name, and the nearest enclosing
/// function/method declaration found scanning backward through the file. Order matters only
/// for the `object` field's display name (first non-empty wins); classification considers ALL
/// of them.
fn candidate_identifiers(
    lines: &[&str],
    line_idx: usize,
    code: &str,
    match_start: usize,
) -> Vec<String> {
    let before = &code[..match_start];
    let mut out = Vec::new();
    if let Some(id) = assignment_target(before) {
        out.push(id);
    }
    if let Some(id) = enclosing_call_name(before) {
        out.push(id);
    }
    if let Some(id) = enclosing_function_name(lines, line_idx) {
        out.push(id);
    }
    out
}

/// The identifier immediately before the nearest `=` (not `==`/`!=`/`<=`/`>=`/`=>`) or `:`
/// scanning backward through `before` — covers `const shareToken = `, `token = `, and object-
/// literal `resetCode: `. Returns `None` when no such operator appears (a bare expression
/// statement, a function argument, a `return` with no local assignment).
fn assignment_target(before: &str) -> Option<String> {
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if c == '=' {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            if next == Some('=') || next == Some('>') {
                continue; // == or =>
            }
            if matches!(prev, Some('=') | Some('!') | Some('<') | Some('>')) {
                continue; // ==, !=, <=, >=
            }
            let head: String = chars[..i].iter().collect();
            return last_identifier(&head);
        }
        if c == ':' {
            // Skip `::` (Rust path separator / Ruby symbol-ish) — neither side is a plain
            // assignment target.
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            if prev == Some(':') || next == Some(':') {
                continue;
            }
            let head: String = chars[..i].iter().collect();
            return last_identifier(&head);
        }
    }
    None
}

/// The identifier immediately before the nearest UNMATCHED `(` scanning backward through
/// `before` — the innermost function call the PRNG expression is nested inside, e.g.
/// `generateShareToken(` in `generateShareToken(Math.random())`.
fn enclosing_call_name(before: &str) -> Option<String> {
    let chars: Vec<char> = before.chars().collect();
    let mut depth: i32 = 0;
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        match chars[i] {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    let head: String = chars[..i].iter().collect();
                    return last_identifier(&head);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The nearest enclosing function/method declaration's name, searching backward from
/// `from_line_idx` (exclusive) through up to 40 preceding lines of the SAME file. Recognizes
/// `function NAME(`, `async function NAME(`, `def NAME(`, and `const/let/var NAME = (` /
/// `= function` / `= async` (the arrow/anonymous-function assignment idiom). A bounded lookback
/// (not "scan to file start") keeps this cheap and keeps an unrelated same-named declaration
/// far above from ever being picked up.
fn enclosing_function_name(lines: &[&str], from_line_idx: usize) -> Option<String> {
    let bound = from_line_idx.saturating_sub(40);
    for idx in (bound..from_line_idx).rev() {
        if let Some(name) = declaration_name(lines[idx]) {
            return Some(name);
        }
    }
    None
}

fn declaration_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for kw in ["async function ", "function ", "def "] {
        if let Some(rest) = trimmed.strip_prefix(kw) {
            return leading_identifier(rest);
        }
    }
    for kw in ["export const ", "export let ", "const ", "let ", "var "] {
        if let Some(rest) = trimmed.strip_prefix(kw) {
            let eq_pos = rest.find('=')?;
            let name_part = rest[..eq_pos].trim();
            let after_eq = rest[eq_pos + 1..].trim_start();
            if after_eq.starts_with('(')
                || after_eq.starts_with("function")
                || after_eq.starts_with("async")
            {
                return leading_identifier(name_part);
            }
        }
    }
    None
}

fn leading_identifier(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(s[..end].to_string())
    }
}

/// The trailing run of identifier characters (letter/digit/`_`/`$`) at the END of `text` —
/// recovers `shareToken` from `"  const shareToken"`, `token` from `"this.token"` / `"self.
/// password"`, and `resetCode` from `"{ resetCode"`. Returns `None` when `text` ends with no
/// identifier characters at all (whitespace-only, or a bare operator).
fn last_identifier(text: &str) -> Option<String> {
    let trimmed = text.trim_end();
    let ident_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let end = trimmed.len();
    let mut start = end;
    for (i, c) in trimmed.char_indices().rev() {
        if ident_char(c) {
            start = i;
        } else {
            break;
        }
    }
    if start == end {
        None
    } else {
        Some(trimmed[start..end].to_string())
    }
}

/// Lowercase, non-alphanumeric-stripped form of an identifier, so `share_token`, `shareToken`,
/// and `SHARE_TOKEN` all normalize to `sharetoken` for substring matching.
fn normalize(ident: &str) -> String {
    ident
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// High-severity shape: the identifier reads as an UNAUTHENTICATED access credential on its
/// own — a share-link or access value that itself grants entry, with nothing else standing in
/// the way once it's known.
fn is_high_severity_identifier(norm: &str) -> bool {
    let has_share_or_access = norm.contains("share") || norm.contains("access");
    let names_credential = norm.contains("token")
        || norm.contains("link")
        || norm.contains("key")
        || norm.contains("id");
    has_share_or_access && names_credential
}

/// Classify a list of candidate identifiers (already produced by [`candidate_identifiers`],
/// possibly extended by [`forward_taint_identifiers`]): `None` when no candidate is
/// security-relevant (the common case — most PRNG calls in a repo are legitimately
/// non-security), `Some((severity, identifier))` otherwise, naming the SPECIFIC identifier
/// responsible for the match rather than always the first-computed candidate — once the
/// forward-taint hop is in play, `identifiers[0]` may be a boring intermediate (`raw`) while a
/// LATER entry (`shareToken`) is the one that actually earned the finding, and the emitted
/// message should name that one. Precedence across identifiers, in order: any
/// [`is_high_severity_identifier`] hit wins outright; otherwise the first
/// [`STRONG_SECURITY_KEYWORDS`] hit; otherwise the first [`WEAK_SECURITY_KEYWORDS`] hit not
/// suppressed by a co-occurring [`DISCRIMINATOR_KEYWORDS`] hit on that SAME identifier.
fn classify_named(identifiers: &[String]) -> Option<(&'static str, String)> {
    for ident in identifiers {
        let norm = normalize(ident);
        if !norm.is_empty() && is_high_severity_identifier(&norm) {
            return Some((SEVERITY_HIGH, ident.clone()));
        }
    }
    for ident in identifiers {
        let norm = normalize(ident);
        if !norm.is_empty() && STRONG_SECURITY_KEYWORDS.iter().any(|k| norm.contains(k)) {
            return Some((SEVERITY_MEDIUM, ident.clone()));
        }
    }
    for ident in identifiers {
        let norm = normalize(ident);
        if norm.is_empty() {
            continue;
        }
        let is_weak = WEAK_SECURITY_KEYWORDS.iter().any(|k| norm.contains(k));
        let has_discriminator = DISCRIMINATOR_KEYWORDS.iter().any(|k| norm.contains(k));
        if is_weak && !has_discriminator {
            return Some((SEVERITY_MEDIUM, ident.clone()));
        }
    }
    None
}

/// Strip a trailing line comment and any block-comment span from `line`. For `//`/`/* */`
/// languages (JS/TS, Go, Java, C#) this is byte-for-byte the same discipline as
/// [`crate::ui_dates::strip_comments`] (duplicated rather than shared — that helper is private
/// to its own module and this checker additionally needs the `#`-comment branch for Python/
/// Ruby, which have no block comments at all). Returns the stripped code plus whether the line
/// ENDS still inside an unterminated block comment, for the caller to carry into the next line.
fn strip_comments(line: &str, mut in_block_comment: bool, hash_comments: bool) -> (String, bool) {
    if hash_comments {
        // No block comments in this family; strip from the first unescaped `#` onward. Not
        // string-literal-aware (a `#` inside a string literal is a known, accepted limitation —
        // see the module doc), mirroring `ui_dates`'s own stance.
        let code = match line.find('#') {
            Some(pos) => &line[..pos],
            None => line,
        };
        return (code.to_string(), false);
    }

    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if in_block_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            break;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, in_block_comment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView {
            spec: "test/repo",
            files,
        }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }

    fn rule_hits(vs: &[ArchViolation]) -> Vec<&ArchViolation> {
        vs.iter()
            .filter(|v| v.rule_id == RULE_WEAK_TOKEN_RANDOMNESS)
            .collect()
    }

    // ── positives ───────────────────────────────────────────────────────────

    #[test]
    fn flags_math_random_share_token_as_high() {
        let f = files(vec![(
            "src/links/shareLink.ts",
            "export function createShareToken(): string {\n  const shareToken = Math.random().toString(36).slice(2);\n  return shareToken;\n}\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert_eq!(vs[0].line, 2);
        assert_eq!(vs[0].object.as_deref(), Some("shareToken"));
    }

    #[test]
    fn flags_math_random_access_token_as_high() {
        let f = files(vec![(
            "src/auth/access.ts",
            "const accessToken = Math.random().toString(36);\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn flags_math_random_reset_code_as_medium() {
        let f = files(vec![(
            "src/auth/reset.ts",
            "const resetCode = Math.random().toString().slice(2, 8);\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].severity, SEVERITY_MEDIUM,
            "a reset code alone isn't an unauth access grant: {vs:#?}"
        );
    }

    #[test]
    fn flags_python_random_random_password_as_medium() {
        let f = files(vec![(
            "app/accounts.py",
            "temp_password = random.random()\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
    }

    #[test]
    fn flags_python_secret_via_random_choice() {
        let f = files(vec![(
            "app/tokens.py",
            "api_secret = random.choice(string.ascii_letters)\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn flags_via_enclosing_function_name_when_no_local_assignment() {
        // No same-line assignment target and no wrapping call — the ONLY context is the
        // enclosing function's name declared several lines above.
        let f = files(vec![(
            "app/tokens.py",
            "def generate_share_token():\n    value = 0\n    # build it up\n    return random.random()\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].severity, SEVERITY_HIGH,
            "share + token in the function name: {vs:#?}"
        );
    }

    #[test]
    fn flags_via_enclosing_call_name_when_passed_as_an_argument() {
        let f = files(vec![(
            "src/lib.ts",
            "storeApiKey(Math.random().toString(36));\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn rand_call_is_boundary_checked_not_a_substring_match() {
        // `errand(` and `grandTotal(` both contain the literal bytes "rand(" — must NOT fire.
        let f = files(vec![
            ("a.go", "errand(shareToken)\n"),
            ("b.go", "grandTotal(shareToken)\n"),
        ]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn rand_call_alone_fires_when_word_bounded() {
        let f = files(vec![("a.rb", "share_token = rand(36**16).to_s(36)\n")]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    // ── safe twins: correct CSPRNG usage ───────────────────────────────────────

    #[test]
    fn crypto_random_uuid_for_a_token_is_never_flagged() {
        let f = files(vec![(
            "src/links/shareLink.ts",
            "const shareToken = crypto.randomUUID();\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn crypto_get_random_values_for_a_token_is_never_flagged() {
        let f = files(vec![(
            "src/links/shareLink.ts",
            "crypto.getRandomValues(shareTokenBuffer);\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn python_secrets_token_urlsafe_is_never_flagged() {
        let f = files(vec![(
            "app/tokens.py",
            "reset_code = secrets.token_urlsafe(16)\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    // ── safe twins: non-security use of a fast PRNG ────────────────────────────

    #[test]
    fn math_random_for_ui_jitter_is_never_flagged() {
        let f = files(vec![(
            "src/ui/toast.ts",
            "const jitterMs = Math.random() * 150;\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn math_random_for_animation_delay_is_never_flagged() {
        let f = files(vec![(
            "src/ui/confetti.tsx",
            "function nextAnimationDelayMs() {\n  return Math.random() * 400;\n}\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn math_random_for_array_shuffle_is_never_flagged() {
        let f = files(vec![(
            "src/lib/shuffle.ts",
            "function shuffleDisplayOrder(items) {\n  const j = Math.floor(Math.random() * items.length);\n  return j;\n}\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn math_random_for_sampling_is_never_flagged() {
        let f = files(vec![(
            "src/analytics/sample.ts",
            "const sampleRate = Math.random();\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn python_random_for_test_fixture_data_is_never_flagged() {
        let f = files(vec![(
            "tests/factories.py",
            "mock_color = random.choice(['red', 'green', 'blue'])\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn discriminator_suppresses_a_weak_keyword_collision() {
        // "session" is a WEAK keyword, but "jitter" in the same identifier marks this as UI
        // timing, not an actual session identifier — must be suppressed (prefer the false
        // negative over flagging every session-scoped animation helper).
        let f = files(vec![(
            "src/ui/presence.ts",
            "const sessionJitterMs = Math.random() * 50;\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn strong_keyword_is_never_suppressed_by_a_discriminator() {
        // Contrast the case above: "secret" is STRONG — even an odd co-occurring word must not
        // suppress it, because there's no safe reading of "the secret is just decorative."
        let f = files(vec![(
            "src/ui/theme.ts",
            "const secretColorSeed = Math.random();\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    // ── scoping / hygiene ───────────────────────────────────────────────────

    #[test]
    fn non_matching_extension_is_not_scoped_in() {
        let f = files(vec![("README.md", "shareToken = Math.random()\n")]);
        assert!(!crate::arch_checker::checker_applies(
            &WeakTokenRandomnessChecker,
            &f
        ));
    }

    #[test]
    fn vendor_and_build_output_paths_are_skipped() {
        let f = files(vec![
            (
                "node_modules/pkg/index.js",
                "const shareToken = Math.random();\n",
            ),
            (
                "apps/ui/dist/bundle.js",
                "const shareToken = Math.random();\n",
            ),
        ]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ignores_a_line_comment_occurrence() {
        let f = files(vec![(
            "a.ts",
            "// const shareToken = Math.random(); is banned\nconst x = 1;\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ignores_a_block_comment_occurrence() {
        let f = files(vec![(
            "a.ts",
            "/* const shareToken = Math.random(); */\nconst x = 1;\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn ignores_a_python_hash_comment_occurrence() {
        let f = files(vec![("app.py", "# share_token = random.random()\nx = 1\n")]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn empty_file_does_not_panic() {
        let f = files(vec![("a.ts", "")]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn unterminated_block_comment_does_not_panic_and_suppresses_rest_of_file() {
        let f = files(vec![(
            "a.ts",
            "/* unterminated\nconst shareToken = Math.random();\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn non_ascii_content_does_not_panic() {
        let f = files(vec![(
            "a.ts",
            "// 日本語のコメント\nconst shareToken = crypto.randomUUID();\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    // ── widened entropy sources: non-PRNG, low-entropy value builders ─────────

    #[test]
    fn flags_date_now_based_access_id_as_high() {
        let f = files(vec![(
            "src/links/access.ts",
            "const accessId = Date.now().toString(36);\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn flags_new_date_get_time_based_reset_code() {
        let f = files(vec![(
            "src/auth/reset.ts",
            "const resetCode = new Date().getTime().toString().slice(-6);\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
    }

    #[test]
    fn flags_performance_now_based_session_token() {
        let f = files(vec![(
            "src/ui/session.ts",
            "const sessionToken = performance.now().toString(36);\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn flags_python_time_time_based_api_secret() {
        let f = files(vec![("app/tokens.py", "api_secret = str(time.time())\n")]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn python_datetime_time_constructor_is_not_confused_with_time_time() {
        // `datetime.time(10, 30)` builds a time-of-day object — it contains the literal bytes
        // "time.time(" but must NOT be treated as the entropy-source needle: the identifier
        // boundary check (char before the match can't be an identifier char) rejects it because
        // the preceding char is the "e" of "datetime".
        let f = files(vec![(
            "app/scheduling.py",
            "share_token = datetime.time(10, 30)\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    #[test]
    fn flags_os_getpid_based_share_link() {
        let f = files(vec![(
            "src/links/pid_link.py",
            "share_link_id = str(os.getpid())\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].severity, SEVERITY_HIGH,
            "share + link/id credential shape: {vs:#?}"
        );
    }

    #[test]
    fn flags_math_random_to_string_36_share_link_idiom() {
        // The exact idiom named in the design spec: `Math.random().toString(36)` feeding a
        // share-link value (as opposed to the existing coverage of the same idiom feeding a
        // "...Token"-named identifier) — exercises the "link" branch of the credential-name
        // check, not just "token".
        let f = files(vec![(
            "src/links/shareLink.ts",
            "const shareLink = Math.random().toString(36).slice(2);\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert_eq!(vs[0].object.as_deref(), Some("shareLink"));
    }

    // ── widened taint window: intermediate variable feeding a token a few lines later ──

    #[test]
    fn flags_weak_prng_via_intermediate_variable_assigned_to_a_token_later_same_function() {
        // Neither the PRNG line's own assignment target ("raw") nor the enclosing function name
        // ("nextId") names a credential — the ONLY way to catch this is following `raw` forward
        // to the line that folds it into `shareToken` a couple of lines down, still inside the
        // same function. A sibling function in the SAME file reuses "raw" for a benign
        // jitter/backoff value and must NOT fire — proving the widened taint window still
        // discriminates.
        let f = files(vec![(
            "src/links/id.ts",
            "function nextId(): string {\n  const raw = Math.random();\n  // a couple of unrelated lines in between\n  const shareToken = 'tok_' + raw.toString(36);\n  return shareToken;\n}\n\nfunction scheduleBackoff(): number {\n  const raw = Math.random();\n  const sessionJitterMs = raw * 100 + 50;\n  return sessionJitterMs;\n}\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(
            vs[0].line, 2,
            "must attribute to the Math.random() call site"
        );
        assert_eq!(
            vs[0].severity, SEVERITY_HIGH,
            "share+token credential shape reached via the intermediate: {vs:#?}"
        );
        assert_eq!(
            vs[0].object.as_deref(),
            Some("shareToken"),
            "must name the credential the taint actually reached, not the boring intermediate: {vs:#?}"
        );
    }

    #[test]
    fn flags_weak_prng_via_intermediate_variable_passed_as_a_call_argument_later() {
        // Same one-hop taint, but the intermediate is consumed as a CALL ARGUMENT rather than an
        // assignment RHS (`buildAccessToken(raw)` instead of `const x = ...raw...`).
        let f = files(vec![(
            "src/links/id.ts",
            "function nextValue(): string {\n  const raw = Math.random();\n  return buildAccessToken(raw);\n}\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].line, 2);
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert_eq!(vs[0].object.as_deref(), Some("buildAccessToken"));
    }

    #[test]
    fn intermediate_variable_taint_does_not_cross_into_a_later_sibling_function() {
        // `raw` in `otherHelper` is unrelated to the `raw` declared (and never consumed) inside
        // `unrelatedNoise` — the scan must stop at the new `function` declaration rather than
        // reading forward across the function boundary.
        let f = files(vec![(
            "src/links/id.ts",
            "function unrelatedNoise(): number {\n  const raw = Math.random();\n  return raw;\n}\n\nfunction otherHelper(): string {\n  const shareToken = 'tok_' + raw;\n  return shareToken;\n}\n",
        )]);
        assert!(rule_hits(&WeakTokenRandomnessChecker.check(&view(&f))).is_empty());
    }

    // ── discrimination preserved: CSPRNG safe twin alongside a positive, one module ────

    #[test]
    fn crypto_random_bytes_and_secrets_token_hex_twins_do_not_suppress_the_real_positive() {
        let f = files(vec![(
            "src/links/mixed.ts",
            "export function createShareToken(): string {\n  const shareToken = Math.random().toString(36).slice(2);\n  return shareToken;\n}\n\nexport function createShareTokenSecure(): string {\n  return crypto.randomBytes(32).toString('hex');\n}\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].line, 2);
    }

    #[test]
    fn python_secrets_token_hex_twin_alongside_a_weak_prng_password() {
        let f = files(vec![(
            "app/accounts.py",
            "def make_temp_password():\n    return random.random()\n\n\ndef make_api_token():\n    return secrets.token_hex(32)\n",
        )]);
        let hits = WeakTokenRandomnessChecker.check(&view(&f));
        let vs = rule_hits(&hits);
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }
}
