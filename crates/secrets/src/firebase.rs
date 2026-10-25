//! Recognises the `apiKey` of a Firebase app configuration (the `FirebaseWebApiKey` twin).
//!
//! Firebase's own guidance ("Learn about using and managing API keys for Firebase") says keys
//! restricted to Firebase services "do not need to be treated as secrets, and it's safe to include
//! them in your code or configuration files". A Google API key with NO Firebase context is a
//! Google Cloud key (a Maps browser key is public by design, an unrestricted server key with
//! billing attached is a real exposure) and the repository cannot show the restriction, so it
//! keeps its finding. Recognition is therefore content-only and conservative: the key must sit
//! on a Firebase-config key name beside Firebase sibling keys, or on an identifier that names
//! Firebase, or in a key-value position beside a Firebase initialisation call. Anything that does
//! not match exactly stays a secret (a false negative on the twin is a finding, never a miss).

use std::sync::OnceLock;

use regex::Regex;

/// What a classifier sees around a match, beyond the matched value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TwinContext<'a> {
    /// The text from the previous newline to the value, at most [`LINE_PREFIX_CAP`] bytes.
    pub line_prefix: &'a str,
    /// Up to [`WINDOW`] bytes either side of the value.
    pub window: &'a str,
    /// Byte offset of the value's start within `window`.
    pub at: usize,
}

/// Bytes of context either side of a match (a Firebase config object is 6 to 8 short lines).
pub(crate) const WINDOW: usize = 800;
/// The most of the match's own line read as its prefix.
pub(crate) const LINE_PREFIX_CAP: usize = 200;

/// Keys that appear beside the web `apiKey` in every Firebase config shape (JS object, Dart
/// `FirebaseOptions`, `google-services.json`, `GoogleService-Info.plist`, `.env`).
const SIBLING_KEYS: &[&str] = &[
    "authDomain",
    "projectId",
    "storageBucket",
    "messagingSenderId",
    "appId",
    "measurementId",
    "databaseURL",
    "project_id",
    "storage_bucket",
    "mobilesdk_app_id",
    "gcm_defaultSenderId",
    "firebase_url",
    "GOOGLE_APP_ID",
    "GCM_SENDER_ID",
    "PROJECT_ID",
    "STORAGE_BUCKET",
    "BUNDLE_ID",
];

/// Calls and names that mark a Firebase initialisation.
const INIT_MARKERS: &[&str] = &[
    "initializeApp(",
    "Firebase.initializeApp(",
    "FirebaseOptions(",
    "firebaseConfig",
];

fn re_config_key_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // The name is not the tail of a longer identifier (`GOOGLE_MAPS_API_KEY`, `mapsApiKey`).
        Regex::new(
            r#"(?:^|[^A-Za-z0-9_])(?:apiKey|api_key|current_key|API_KEY)\s*["']?\s*[:=]\s*["']?$"#,
        )
        .expect("re_config_key_prefix must compile")
    })
}

fn re_plist_key_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<key>API_KEY</key>\s*<string>$").expect("re_plist_key_prefix must compile")
    })
}

fn re_firebase_named_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)[A-Za-z0-9_]*FIREBASE[A-Za-z0-9_]*\s*["']?\s*[:=]\s*["']?$"#)
            .expect("re_firebase_named_prefix must compile")
    })
}

fn re_key_value_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"[A-Za-z_][A-Za-z0-9_]*\s*["']?\s*[:=]\s*["']?$"#)
            .expect("re_key_value_prefix must compile")
    })
}

/// True when `ctx` shows the value is the `apiKey` of a Firebase app configuration.
pub(crate) fn is_firebase_web_api_key(ctx: &TwinContext<'_>) -> bool {
    let before_value = ctx.window.get(..ctx.at).unwrap_or("");
    // (b) The identifier names Firebase: the name is the context, no siblings needed.
    if re_firebase_named_prefix().is_match(ctx.line_prefix) {
        return true;
    }
    // (a) A Firebase-config key name beside Firebase sibling keys.
    let named_key = re_config_key_prefix().is_match(ctx.line_prefix)
        || re_plist_key_prefix().is_match(before_value);
    if named_key && sibling_count(ctx, ctx.line_prefix.contains("current_key")) >= 2 {
        return true;
    }
    // (c) A key-value position beside a Firebase initialisation call and at least one sibling.
    re_key_value_prefix().is_match(ctx.line_prefix)
        && INIT_MARKERS
            .iter()
            .any(|marker| ctx.window.contains(marker))
        && sibling_count(ctx, false) >= 1
}

/// Distinct Firebase sibling keys that count for the value at `ctx.at`. They are read from the
/// smallest `{...}` or `(...)` span around the value (a Maps key in its own object does not
/// inherit a neighbouring Firebase object's siblings); with no enclosing span, the whole window.
/// `whole_window` widens that to the window for the one nested shape: `google-services.json`
/// keeps the key in `"api_key": [{ "current_key": ... }]`, an object of its own, with the project
/// keys in enclosing objects.
fn sibling_count(ctx: &TwinContext<'_>, whole_window: bool) -> usize {
    let span = if whole_window {
        ctx.window
    } else {
        enclosing_span(ctx.window, ctx.at)
    };
    SIBLING_KEYS
        .iter()
        .filter(|key| contains_word(span, key))
        .count()
}

/// The smallest balanced `{...}` or `(...)` span of `window` that contains byte `at`, or the whole
/// window when none does. Strings (`'`, `"`, backtick) and comments are skipped; an unclosed
/// opener or a missing closer runs to the window's edge.
fn enclosing_span(window: &str, at: usize) -> &str {
    let bytes = window.as_bytes();
    let mut open: Vec<usize> = Vec::new();
    let mut i = 0;
    let mut start = 0;
    while i < at.min(bytes.len()) {
        match bytes[i] {
            b'{' | b'(' => open.push(i),
            b'}' | b')' => {
                open.pop();
            }
            b'\'' | b'"' | b'`' => i = skip_string(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => i = skip_line(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            _ => {}
        }
        i += 1;
    }
    if let Some(&last) = open.last() {
        start = last;
    }
    if open.is_empty() {
        return window;
    }
    // Forward from the value to the closer that matches the opener.
    let mut depth = 0usize;
    let mut j = at.min(bytes.len());
    while j < bytes.len() {
        match bytes[j] {
            b'{' | b'(' => depth += 1,
            b'}' | b')' => {
                if depth == 0 {
                    return window.get(start..=j).unwrap_or(window);
                }
                depth -= 1;
            }
            b'\'' | b'"' | b'`' => j = skip_string(bytes, j),
            b'/' if bytes.get(j + 1) == Some(&b'/') => j = skip_line(bytes, j),
            b'/' if bytes.get(j + 1) == Some(&b'*') => j = skip_block_comment(bytes, j),
            _ => {}
        }
        j += 1;
    }
    window.get(start..).unwrap_or(window)
}

/// Index of the closing quote of the string opened at `from` (or the last byte when unclosed).
fn skip_string(bytes: &[u8], from: usize) -> usize {
    let quote = bytes[from];
    let mut i = from + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b if b == quote => return i,
            _ => {}
        }
        i += 1;
    }
    bytes.len().saturating_sub(1)
}

/// Index of the newline ending the line comment that starts at `from`.
fn skip_line(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len().saturating_sub(1), |p| from + p)
}

/// Index of the `/` closing the block comment that starts at `from`.
fn skip_block_comment(bytes: &[u8], from: usize) -> usize {
    let mut i = from + 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len().saturating_sub(1)
}

/// Whether `word` appears in `text` bounded by non-identifier characters.
fn contains_word(text: &str, word: &str) -> bool {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = text.as_bytes();
    text.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        (start == 0 || !is_ident(bytes[start - 1])) && (end >= bytes.len() || !is_ident(bytes[end]))
    })
}
