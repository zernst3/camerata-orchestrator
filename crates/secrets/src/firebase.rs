//! Recognises the `apiKey` of a Firebase app configuration (the `FirebaseWebApiKey` twin).
//!
//! Firebase's own guidance ("Learn about using and managing API keys for Firebase") says keys
//! restricted to Firebase services "do not need to be treated as secrets, and it's safe to include
//! them in your code or configuration files". A Google API key with NO Firebase context is a
//! Google Cloud key (a Maps browser key is public by design, an unrestricted server key with
//! billing attached is a real exposure) and the repository cannot show the restriction, so it
//! keeps its finding. Recognition is therefore content-only and conservative, three exact shapes:
//!
//! - (a) a Firebase-config key name (`apiKey`, `api_key`, `current_key`, `API_KEY`) beside at
//!   least two Firebase sibling keys; the snake_case `api_key` and `current_key` also need a
//!   name only a `google-services.json` carries (`mobilesdk_app_id`, `gcm_defaultSenderId`,
//!   `firebase_url`), because `project_id` and `storage_bucket` sit in any Google Cloud config;
//! - (b) an identifier that names Firebase (and no other Google product: not Gemini, Maps,
//!   Places, Translate, Vision or YouTube), beside a Firebase sibling key or an initialisation
//!   call;
//! - (c) a key-value position INSIDE the `{}` or `()` span of a Firebase initialisation call or
//!   config object, beside a sibling in the same span. A value that no span encloses never fires.
//!
//! Anything that does not match exactly stays a secret (a false negative on the twin is a
//! finding, never a miss).

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

/// Sibling names only a `google-services.json` carries. `project_id` and `storage_bucket` are in
/// every Google Cloud config, so they cannot tell a Firebase file from a generic one.
const GOOGLE_SERVICES_ONLY: &[&str] = &["mobilesdk_app_id", "gcm_defaultSenderId", "firebase_url"];

/// Google products whose keys are billed server credentials, never the Firebase web key. An
/// identifier naming one of them is not a Firebase config key whatever else it names.
const OTHER_GOOGLE_PRODUCTS: &[&str] = &[
    "GENAI",
    "GEMINI",
    "MAPS",
    "PLACES",
    "TRANSLATE",
    "VISION",
    "YOUTUBE",
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
            r#"(?:^|[^A-Za-z0-9_])(apiKey|api_key|current_key|API_KEY)\s*["']?\s*[:=]\s*["']?$"#,
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
        Regex::new(r#"(?i)([A-Za-z0-9_]*FIREBASE[A-Za-z0-9_]*)\s*["']?\s*[:=]\s*["']?$"#)
            .expect("re_firebase_named_prefix must compile")
    })
}

fn re_key_value_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\s*["']?\s*[:=]\s*["']?$"#)
            .expect("re_key_value_prefix must compile")
    })
}

fn re_firebase_named_sibling() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `VITE_FIREBASE_PROJECT_ID`, `firebaseProjectId`: the sibling of a Firebase-named key
        // carries the same prefix, so the plain sibling list (whole words) cannot see it.
        Regex::new(
            r"(?i)FIREBASE[A-Za-z0-9_]*(?:project_?id|auth_?domain|storage_?bucket|app_?id|messaging_?sender_?id|measurement_?id|database_?url)",
        )
        .expect("re_firebase_named_sibling must compile")
    })
}

/// True when `ctx` shows the value is the `apiKey` of a Firebase app configuration.
pub(crate) fn is_firebase_web_api_key(ctx: &TwinContext<'_>) -> bool {
    let before_value = ctx.window.get(..ctx.at).unwrap_or("");
    // (b) The identifier names Firebase (and no other Google product), beside a Firebase sibling
    // key or an initialisation call.
    if let Some(name) = captured(re_firebase_named_prefix(), ctx.line_prefix) {
        if !names_another_google_product(name)
            && (firebase_named_sibling_count(span_or_window(ctx)) + sibling_count(ctx, false) >= 1
                || has_init_marker(ctx.window))
        {
            return true;
        }
    }
    // (a) A Firebase-config key name beside Firebase sibling keys.
    let config_name = captured(re_config_key_prefix(), ctx.line_prefix).or_else(|| {
        re_plist_key_prefix()
            .is_match(before_value)
            .then_some("API_KEY")
    });
    if let Some(name) = config_name {
        // `current_key` is nested in an object of its own in `google-services.json`; every other
        // name reads its own enclosing span.
        let whole_window = name == "current_key";
        let span = if whole_window {
            ctx.window
        } else {
            span_or_window(ctx)
        };
        let google_services_name = matches!(name, "api_key" | "current_key");
        if sibling_count_in(span) >= 2
            && (!google_services_name
                || GOOGLE_SERVICES_ONLY
                    .iter()
                    .any(|key| contains_word(span, key)))
        {
            return true;
        }
    }
    // (c) A key-value position inside the span of a Firebase initialisation call or config
    // object, beside at least one sibling in that span.
    captured(re_key_value_prefix(), ctx.line_prefix).is_some_and(|name| {
        !names_another_google_product(name)
            && inside_init_span(ctx.window, ctx.at)
            && sibling_count(ctx, false) >= 1
    })
}

/// Group 1 of the first match of `re` in `text`.
fn captured<'t>(re: &Regex, text: &'t str) -> Option<&'t str> {
    re.captures(text)?.get(1).map(|m| m.as_str())
}

fn names_another_google_product(identifier: &str) -> bool {
    let upper = identifier.to_ascii_uppercase();
    OTHER_GOOGLE_PRODUCTS
        .iter()
        .any(|word| upper.contains(word))
}

fn has_init_marker(text: &str) -> bool {
    INIT_MARKERS.iter().any(|marker| text.contains(marker))
}

/// The smallest span around the value, or the whole window when no span encloses it.
fn span_or_window<'a>(ctx: &TwinContext<'a>) -> &'a str {
    enclosing_span(ctx.window, ctx.at).unwrap_or(ctx.window)
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
        span_or_window(ctx)
    };
    sibling_count_in(span)
}

fn sibling_count_in(span: &str) -> usize {
    SIBLING_KEYS
        .iter()
        .filter(|key| contains_word(span, key))
        .count()
}

/// Firebase-prefixed sibling variables in `span` (`VITE_FIREBASE_PROJECT_ID`).
fn firebase_named_sibling_count(span: &str) -> usize {
    re_firebase_named_sibling().find_iter(span).count()
}

/// Whether byte `at` of `window` sits inside the `()` of a Firebase initialisation call
/// (`initializeApp(`, `FirebaseOptions(`) or inside the `{}` assigned to a `firebaseConfig`
/// binding. A value that no span encloses is not inside anything.
fn inside_init_span(window: &str, at: usize) -> bool {
    open_brackets(window, at).iter().any(|&open| {
        let before = &window[..open];
        match window.as_bytes()[open] {
            b'(' => INIT_MARKERS
                .iter()
                .any(|marker| marker.ends_with('(') && window[..=open].ends_with(marker)),
            _ => {
                let line = before.rsplit('\n').next().unwrap_or(before);
                line.contains("firebaseConfig") && line.contains('=')
            }
        }
    })
}

/// Byte offsets of the brackets (`{` or `(`) still open at byte `at`, outermost first. Strings
/// (`'`, `"`, backtick) and comments are skipped.
fn open_brackets(window: &str, at: usize) -> Vec<usize> {
    scan_to(window, at).0
}

/// [`open_brackets`], plus where a forward scan from `at` must resume: the first byte at or after
/// `at` outside the string or comment the scan was in when it reached `at`. The value usually sits
/// inside a quoted string, and starting a forward scan inside it would read its CLOSING quote as
/// an opening one, flipping every string after it (a `}` outside a string skipped, one inside a
/// string counted).
fn scan_to(window: &str, at: usize) -> (Vec<usize>, usize) {
    let bytes = window.as_bytes();
    let mut open: Vec<usize> = Vec::new();
    let mut i = 0;
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
    (open, i.max(at))
}

/// The smallest balanced `{...}` or `(...)` span of `window` that contains byte `at`, or `None`
/// when none does. Strings (`'`, `"`, backtick) and comments are skipped; a missing closer runs
/// to the window's edge.
fn enclosing_span(window: &str, at: usize) -> Option<&str> {
    let bytes = window.as_bytes();
    let (open, resume) = scan_to(window, at);
    let start = *open.last()?;
    // Forward from the value (past the string it sits in) to the closer that matches the opener.
    let mut depth = 0usize;
    let mut j = resume.min(bytes.len());
    while j < bytes.len() {
        match bytes[j] {
            b'{' | b'(' => depth += 1,
            b'}' | b')' => {
                if depth == 0 {
                    return window.get(start..=j);
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
    window.get(start..)
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
