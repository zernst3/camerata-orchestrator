//! Safe twins (spec section 2.3): values that match a secret SHAPE but are not secrets, and so
//! stay readable and never count as a gate failure.

use sha2::{Digest, Sha256};

use crate::firebase::{is_firebase_web_api_key, TwinContext};
use crate::shapes::{prefix_of, SecretShape};

/// Why a match is NOT a secret (left readable, never a gate failure).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Twin {
    /// T1: the opaque body contains a placeholder word (which one is carried).
    PlaceholderWord(&'static str),
    /// T2: a quoted identifier such as `SUPABASE_SERVICE_ROLE_KEY` (a NAME, not a value).
    EnvNameLiteral,
    /// T3: a key a vendor publishes in its documentation (matched by SHA-256 digest).
    PublishedExampleKey,
    /// T4: a Supabase JWT whose role is `anon` or `authenticated` (public by design).
    PublicKeyRole(String),
    /// T5: a template token (`${...}`, `{{...}}`, `<...>`, `[YOUR-PASSWORD]`, `%s`).
    TemplateToken,
    /// Extension: a path-like quoted literal (`app/api/auth/token/refresh`, `src/some/path.rs`) on
    /// a credential-named identifier. Real base64 secrets are not all-lowercase with several
    /// slashes.
    PathLike,
    /// T6: the value already carries the redaction ellipsis, so it is a redacted form.
    RedactionMarker,
    /// Extension: a PEM header with no key material after it (a one-line snippet).
    NoKeyMaterial,
    /// Extension: a value made only of words joined by `_`, `-` or `.`
    /// (`app_preferences_sidebar_collapsed_v2`, `process.env.SERVICE_ROLE_KEY`): an identifier
    /// or a key name, not a credential.
    ReadableName,
    /// Extension: a key that is public by design (`sb_publishable_`, `pk_live_`, `pk_test_`).
    PublishableKey,
    /// Extension (item 24): a Google API key that is the `apiKey` of a Firebase app configuration.
    /// Firebase documents it as an identifier, not a credential ("do not need to be treated as
    /// secrets, and it's safe to include them in your code or configuration files"). A Google key
    /// anywhere else is not covered.
    FirebaseWebApiKey,
    /// A qualified label name (`nodes.cloudhost.internal/worker-pool`: a lowercase dotted domain,
    /// one slash, a name) on an identifier that is not password-like. A label key, not a value.
    QualifiedLabelName,
    /// A quoted label on a Terraform block header (`resource "type" "name" {`): a name by grammar.
    BlockHeaderLabel,
    /// A module path in a `require(...)`, `import(...)`, `import '...'` or `from '...'`: the
    /// language reads the literal as a module name.
    ModulePath,
    /// The ciphertext of a sealed secret: a long base64 value under `encryptedData` in a
    /// `kind: SealedSecret` document. Encrypted for committing, so it is not a credential.
    SealedCiphertext,
}

impl Twin {
    /// Whether the scan floor still lists a twin match as a finding row when asked to count
    /// twins (a committed placeholder `sb_secret_` is still worth a row; a public anon key, an
    /// already-redacted form, or a file path on a credential-named variable never is).
    pub fn counts_as_line(&self) -> bool {
        !matches!(
            self,
            Twin::PublicKeyRole(_)
                | Twin::RedactionMarker
                | Twin::PathLike
                | Twin::ReadableName
                | Twin::PublishableKey
                | Twin::FirebaseWebApiKey
                | Twin::QualifiedLabelName
                | Twin::BlockHeaderLabel
                | Twin::ModulePath
                | Twin::SealedCiphertext
        )
    }
}

impl Twin {
    /// Whether a match with this twin may reach a client file UNMASKED (the release gate's
    /// decision, one arm per twin so a twin added tomorrow forces a decision here).
    ///
    /// Allowed because the value is public by design or is not a value at all: a Supabase `anon`
    /// or `authenticated` JWT, a publishable key, a Firebase web API key, an environment variable
    /// name, a path, a readable name, a label, a module path, a template token, an already
    /// redacted form, a PEM header with no key material, and a sealed secret's ciphertext
    /// (encrypted for committing). A placeholder word (`sk_live_your_key_here`) is allowed too
    /// when it is delimited or the body is single-case (the client path checks that, see
    /// [`crate::SecretMatch::is_client_visible_secret`]): it is a fake by its own spelling, and
    /// the placeholder is the documented way to show a key slot without a key.
    ///
    /// Refused: a key a vendor publishes in its documentation (a value documented only as one
    /// setting's value, a config enum, is not a key: [`crate::SecretMatch::is_client_visible_secret`]
    /// reads the digest for that). It is real-looking, it is not
    /// public by design (the vendor says to replace it), and a reader who finds it unmasked in a
    /// report cannot tell it from a leak. The export masks it, and the gate proves it did.
    pub fn ships_unmasked(&self) -> bool {
        match self {
            Twin::PublishedExampleKey => false,
            Twin::PlaceholderWord(_)
            | Twin::EnvNameLiteral
            | Twin::PublicKeyRole(_)
            | Twin::TemplateToken
            | Twin::PathLike
            | Twin::RedactionMarker
            | Twin::NoKeyMaterial
            | Twin::ReadableName
            | Twin::PublishableKey
            | Twin::FirebaseWebApiKey
            | Twin::QualifiedLabelName
            | Twin::BlockHeaderLabel
            | Twin::ModulePath
            | Twin::SealedCiphertext => true,
        }
    }
}

/// T1 placeholder words, matched case-insensitively against the opaque body. Adding one is a
/// one-line change with a twin test.
pub const PLACEHOLDER_WORDS: &[&str] = &[
    "your",
    "example",
    "placeholder",
    "replace",
    "changeme",
    "change_me",
    "change-me",
    "dummy",
    "fake",
    "sample",
    "redacted",
    "xxxx",
    "todo",
    "insert",
    "_here",
    "-here",
    "do_not",
    "donot",
    "mock",
];

/// The redaction ellipsis (U+2026). Outside every body class, so a redacted form never re-matches.
pub(crate) const ELLIPSIS: char = '\u{2026}';

pub(crate) fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The case class of a body byte: a boundary lies between two bytes of different classes
/// (`_`, `-`, a case change, a digit next to a letter).
fn byte_class(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => 0,
        b'A'..=b'Z' => 1,
        b'0'..=b'9' => 2,
        _ => 3,
    }
}

/// Whether `body[start..end]` (a placeholder word) stands apart from its neighbours: at the
/// start or end of the body, next to `_` or `-` (or carrying one itself), or at a case change.
fn is_delimited(body: &str, start: usize, end: usize, word: &str) -> bool {
    let bytes = body.as_bytes();
    let left = start == 0
        || word.starts_with(['_', '-'])
        || byte_class(bytes[start - 1]) != byte_class(bytes[start]);
    let right = end == bytes.len()
        || word.ends_with(['_', '-'])
        || byte_class(bytes[end - 1]) != byte_class(bytes[end]);
    left && right
}

/// T1. A VENDOR body is a random run, so a short word inside it is coincidence unless it is
/// delimited or the whole body is single-case (`sb_secret_yourkeyhere`, `FAKE_DO_NOT_USE`); a
/// context-shaped value is human-written, so any substring counts.
fn placeholder_word(body: &str, delimited_only: bool) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    let single_case = !body.bytes().any(|b| b.is_ascii_lowercase())
        || !body.bytes().any(|b| b.is_ascii_uppercase());
    PLACEHOLDER_WORDS.iter().copied().find(|word| {
        lower.match_indices(word).any(|(start, _)| {
            !delimited_only || single_case || is_delimited(body, start, start + word.len(), word)
        })
    })
}

/// `body` with every placeholder word replaced by `_` (a separator), case kept elsewhere.
fn without_placeholder_words(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let mut covered = vec![false; body.len()];
    for word in PLACEHOLDER_WORDS {
        for (start, _) in lower.match_indices(word) {
            covered[start..start + word.len()].fill(true);
        }
    }
    body.bytes()
        .zip(covered)
        .map(|(b, hit)| if hit { '_' } else { char::from(b) })
        .collect()
}

/// A segment of a human-written value (split at every non-alphanumeric byte) that reads as random
/// text, not as a word: six or more characters mixing letters and digits (`8842xQ`) or mixing
/// case inside one word (`MyCorp`, `Kx9mQ2vLp3Zr`). `Dummy`, `PASSWORD`, `v2` and `1234` are not.
fn is_random_segment(segment: &str) -> bool {
    segment.len() >= 6
        && ((segment.bytes().any(|b| b.is_ascii_alphabetic())
            && segment.bytes().any(|b| b.is_ascii_digit()))
            || !is_name_word(segment))
}

/// Whether a single-case body still reads as a placeholder: a placeholder word opens or closes
/// it (`EXAMPLEONLY00000`, `IOSFODNN7EXAMPLE`: the documented AWS style), or once every
/// placeholder word is taken out each run left is letters only or digits only (`yourkeyhere`,
/// `CHANGE_ME_TO_A_REAL_KEY_1234`). A word buried between random runs (`kx9fake2mq8vlp3`) is
/// neither.
fn single_case_is_plain_placeholder(body: &str) -> bool {
    if placeholder_word(body, false).is_none() {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    if PLACEHOLDER_WORDS
        .iter()
        .any(|word| lower.starts_with(word) || lower.ends_with(word))
    {
        return true;
    }
    without_placeholder_words(body)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .all(|run| {
            run.bytes().all(|b| b.is_ascii_alphabetic()) || run.bytes().all(|b| b.is_ascii_digit())
        })
}

/// Whether `value` carries a placeholder word AND that word holds under the client rule (see
/// [`placeholder_holds_for_client`]).
pub(crate) fn has_placeholder_that_holds(shape: SecretShape, value: &str) -> bool {
    let body = &value[prefix_of(shape, value).len()..];
    placeholder_word(body, false).is_some() && placeholder_holds_for_client(shape, value)
}

/// Whether the placeholder twin on `value` still holds under the CLIENT rule: the body is
/// single-case and plain placeholder text, or some placeholder word is set apart by a SEPARATOR
/// (a non-alphanumeric byte) or by the start or end of the body. A case change or a digit next to
/// the word is not enough, as it is for a vendor body: a word buried in mixed-case random text
/// (`Kx9fakeT2mQ8vLp3Zr7wYn5Bd1Hs`) proves nothing, so a client package masks it.
///
/// A human-written value (a context shape: `password = "..."`, a bearer token) is also masked
/// when any of its segments reads as random text: `MyCorp-Dummy-8842xQ-Prod-Main` has a delimited
/// `Dummy` and a real-looking password around it. A vendor body is random by nature, so only the
/// placeholder word is judged there.
pub(crate) fn placeholder_holds_for_client(shape: SecretShape, value: &str) -> bool {
    let body = &value[prefix_of(shape, value).len()..];
    if is_context_shaped(shape)
        && without_placeholder_words(body)
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(is_random_segment)
    {
        return false;
    }
    let single_case = !body.bytes().any(|b| b.is_ascii_lowercase())
        || !body.bytes().any(|b| b.is_ascii_uppercase());
    if single_case {
        return single_case_is_plain_placeholder(body);
    }
    let bytes = body.as_bytes();
    let lower = body.to_ascii_lowercase();
    PLACEHOLDER_WORDS.iter().any(|word| {
        lower.match_indices(word).any(|(start, _)| {
            let end = start + word.len();
            let left = start == 0
                || word.starts_with(['_', '-'])
                || !bytes[start - 1].is_ascii_alphanumeric();
            let right = end == bytes.len()
                || word.ends_with(['_', '-'])
                || !bytes[end].is_ascii_alphanumeric();
            left && right
        })
    })
}

/// T2: `^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$`.
fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    value.contains('_') && !value.ends_with('_') && !value.contains("__")
}

/// `value` opens with `open` and closes with `close` (and is longer than the two together).
fn is_closed(value: &str, open: &str, close: char) -> bool {
    value.len() > open.len() && value.starts_with(open) && value.ends_with(close)
}

fn is_identifier(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// A format-string placeholder: `%s`, `%(name)s`, or a `{name}` field (a Python or Rust format
/// string; `postgres://user:{password}@host` is code, not a password).
fn is_format_placeholder(value: &str) -> bool {
    value == "%s"
        || value
            .strip_prefix("%(")
            .and_then(|rest| rest.strip_suffix(")s"))
            .is_some_and(is_identifier)
        || value
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .is_some_and(is_identifier)
}

/// T5. Only a CLOSED template is one: a real password may start with `$`, `%`, `[`, `<` or `{`.
fn is_template(value: &str) -> bool {
    is_closed(value, "${", '}')
        || is_closed(value, "{{", '}')
        || is_closed(value, "<", '>')
        || is_closed(value, "[", ']')
        || is_format_placeholder(value)
        || ["password", "secret", "token", "changeme"]
            .iter()
            .any(|word| value.eq_ignore_ascii_case(word))
}

/// A key that is public by design.
fn is_publishable_key(value: &str) -> bool {
    ["sb_publishable_", "pk_live_", "pk_test_"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn is_name_separator(c: char) -> bool {
    matches!(c, '_' | '-' | '.')
}

/// One word of a readable name: all lowercase, all uppercase or Capitalized (digits tolerated).
fn is_name_word(segment: &str) -> bool {
    let mut letters = segment.chars().filter(char::is_ascii_alphabetic);
    let Some(first) = letters.next() else {
        return !segment.is_empty();
    };
    let rest: Vec<char> = letters.collect();
    rest.iter().all(char::is_ascii_lowercase)
        || (first.is_ascii_uppercase() && rest.iter().all(char::is_ascii_uppercase))
}

/// A readable name (owner decision 2026-10-23): at least three words joined by `_`, `-` or `.`,
/// at most two digits in all (`app_preferences_sidebar_collapsed_v2`,
/// `process.env.SERVICE_ROLE_KEY`). A random token has digits and mixed case.
fn is_readable_name(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || is_name_separator(c))
        && value.chars().filter(char::is_ascii_digit).count() <= 2
        && value.split(is_name_separator).count() >= 3
        && value.split(is_name_separator).all(is_name_word)
}

fn is_path_like(value: &str) -> bool {
    value.matches('/').count() >= 2
        && value.split('/').all(|segment| {
            segment.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.')
            })
        })
}

/// A qualified label name: a lowercase dotted domain, one slash, then a name
/// (`nodes.cloudhost.internal/worker-pool`). A credential is not shaped like this.
fn is_qualified_label_name(value: &str) -> bool {
    let Some((domain, name)) = value.split_once('/') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// Whether the line holding the value is a Terraform block header: `resource` or `data`, quoted
/// labels, then an opening brace. The value is one of the labels.
fn is_block_header_label(value: &str, context: &TwinContext<'_>) -> bool {
    let tail_end = context.window[context.at..]
        .find('\n')
        .map_or(context.window.len(), |i| context.at + i);
    let tail = context
        .window
        .get(context.at + value.len()..tail_end)
        .unwrap_or("");
    // The value sits inside quotes: the line must open with the keyword and end in a brace.
    let line = format!("{}{value}{tail}", context.line_prefix);
    let line = line.trim();
    let Some(rest) = line
        .strip_prefix("resource")
        .or_else(|| line.strip_prefix("data"))
    else {
        return false;
    };
    let Some(rest) = rest.strip_suffix('{') else {
        return false;
    };
    let mut rest = rest.trim();
    if !rest.starts_with('"') {
        return false;
    }
    // Every remaining token is a quoted label.
    let mut labels = 0;
    while let Some(after_open) = rest.strip_prefix('"') {
        let Some(close) = after_open.find('"') else {
            return false;
        };
        labels += 1;
        rest = after_open[close + 1..].trim_start();
    }
    labels >= 1 && rest.is_empty()
}

/// A module specifier: starts with `./` or `../`, or has a slash and no character a base64 run
/// would add (`+`, `=`).
fn is_module_path(value: &str) -> bool {
    value.starts_with("./")
        || value.starts_with("../")
        || (value.contains('/') && !value.contains(['+', '=']))
}

/// Whether the text just before the opening quote makes the literal a module name.
fn opens_a_module_specifier(before_value: &str) -> bool {
    let Some(head) = before_value.strip_suffix(['"', '\'']) else {
        return false;
    };
    let head = head.trim_end();
    head.ends_with("require(")
        || head.ends_with("import(")
        || ends_with_word(head, "import")
        || ends_with_word(head, "from")
}

/// `text` ends with `word` and the character before it is not part of an identifier
/// (`fromage` and `tokenfrom` do not end with the word `from`).
fn ends_with_word(text: &str, word: &str) -> bool {
    text.strip_suffix(word).is_some_and(|rest| {
        !rest
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn is_context_shaped(shape: SecretShape) -> bool {
    !shape.is_vendor_shaped()
}

/// Identifier words whose value is a secret even when it reads like a name: a diceware-style
/// passphrase (`correct-horse-battery-staple`) is words joined by `-` or `.`.
const SECRET_IDENTIFIER_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "secret",
    "credential",
];

/// Whether `before_value` (the identifier and what sits between it and the value) names a
/// password-like thing.
pub(crate) fn names_a_secret(before_value: &str) -> bool {
    let lower = before_value.to_ascii_lowercase();
    SECRET_IDENTIFIER_WORDS
        .iter()
        .any(|word| lower.contains(word))
}

/// Words that make a bare `key` a credential (`api_key`, `accessKey`, `AUTH_KEY`). A bare `key` or
/// a `STORAGE_KEY`, `queryKey` is a name in code, not a credential.
const KEY_QUALIFIERS: &[&str] = &[
    "api",
    "access",
    "auth",
    "private",
    "signing",
    "encryption",
    "license",
    "master",
    "session",
    "client",
    "consumer",
    "app",
    "service",
    "role",
    "jwt",
    "ssh",
    "deploy",
    "hmac",
    "webhook",
    "crypto",
    "stripe",
    "bot",
];

/// Trailing words that say which copy of a credential an identifier names, not what it is
/// (`API_KEY_PROD`, `API_TOKEN_STAGING`, `access_token_value`). Dropped before the last word is
/// read, with any all-digit word and any `v<digits>` word.
const NAME_SUFFIX_WORDS: &[&str] = &[
    "prod",
    "production",
    "staging",
    "stage",
    "dev",
    "test",
    "live",
    "value",
    "default",
    "primary",
    "secondary",
    "old",
    "new",
];

fn is_suffix_word(word: &str) -> bool {
    NAME_SUFFIX_WORDS.contains(&word)
        || (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
        || word
            .strip_prefix('v')
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Single words that already hold the qualifier (`apikey`, `accesskey`).
const KEY_COMPOUNDS: &[&str] = &[
    "apikey",
    "apikeys",
    "accesskey",
    "accesskeys",
    "authkey",
    "privatekey",
    "signingkey",
    "encryptionkey",
    "licensekey",
    "masterkey",
    "sessionkey",
    "authtoken",
    "accesstoken",
    "apitoken",
];

/// The words of `text`, lowercased, split on anything that is not a letter or digit and at a
/// camel-case boundary (`apiKey` is `api`, `key`; `APIKey` is `api`, `key`).
fn identifier_words(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let boundary = c.is_ascii_uppercase()
            && i > 0
            && (chars[i - 1].is_ascii_lowercase()
                || (chars[i - 1].is_ascii_uppercase()
                    && chars.get(i + 1).is_some_and(char::is_ascii_lowercase)));
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Whether `after_name` (the text after an identifier) assigns it: closing quotes (escaped ones
/// too), a bracket and spaces, then `:`, `=` or `=>`.
fn is_assigned(after_name: &str) -> bool {
    after_name
        .trim_start_matches(['"', '\'', '\\', ']', ' ', '\t'])
        .starts_with([':', '='])
}

/// `before_value` without what the unquoted pattern puts in front of the name: indentation,
/// `export ` and a list dash (`  - export API_KEY=` is `API_KEY=`).
fn strip_line_prefix(before_value: &str) -> &str {
    let mut rest = before_value.trim_start();
    loop {
        let next = rest
            .strip_prefix("export ")
            .or_else(|| rest.strip_prefix("- "))
            .map(str::trim_start);
        match next {
            Some(next) => rest = next,
            None => return rest,
        }
    }
}

/// The length of the identifier `text` opens with: letters, digits, `_`, `-` and `.`.
fn identifier_len(text: &str) -> usize {
    text.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .unwrap_or(text.len())
}

/// `before_value` with the identifier it starts in widened to the left: the quoted pattern's
/// identifier class stops at `-` and `.`, so `"x-api-key": "..."` matches from `key`. The
/// widening reads the line in front of the match over `[A-Za-z0-9_.-]`, and only when the name is
/// being assigned (`"x-api-key": "..."`, `headers["X-Api-Key"] =`, `'api-key' => '...'`).
fn identifier_head(before_value: &str, context: &TwinContext<'_>) -> String {
    let head = context.line_prefix.strip_suffix(before_value).unwrap_or("");
    let kept = head
        .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .len();
    let lead = &head[kept..];
    let stripped = strip_line_prefix(before_value);
    if is_assigned(&stripped[identifier_len(stripped)..]) {
        format!("{lead}{before_value}")
    } else {
        before_value.to_string()
    }
}

/// Whether `before_value` names something whose value may be a passphrase that READS like a name
/// (a diceware phrase on an `api_token` or `apiKey` identifier): everything [`names_a_secret`]
/// names, plus an identifier that ENDS in the word `token` (or `tokens`), in a compound such as
/// `apikey`, or in a `key` qualified as a credential (`api_key`, `accessKey`, `AUTH_KEY`). Whole
/// words only, so `monkey`, `keyboard` and `author` do not count; the identifier's last word
/// only, so `tokenName`, `api_key_header` and `STORAGE_KEY` (names in code) stay readable.
fn names_a_passphrase_holder(before_value: &str) -> bool {
    if names_a_secret(before_value) {
        return true;
    }
    let stripped = strip_line_prefix(before_value);
    let end = identifier_len(stripped);
    let assigned = is_assigned(&stripped[end..]);
    let mut words = if assigned {
        // A name being assigned: the whole identifier (kebab and dotted included), read without
        // its environment or version suffix (`API_KEY_PROD`, `apiKeyV2`).
        let mut words = identifier_words(&stripped[..end]);
        while words.len() > 1 && words.last().is_some_and(|w| is_suffix_word(w)) {
            words.pop();
        }
        words
    } else {
        // Not assigned (a fallback `|| "..."`, a call argument, an accessor): the words in front
        // of the first `:` or `=`, as the last word. A string in a list (`"SEC-NO-TOKEN-1"`)
        // ends in `1`, so it stays a value.
        let head = stripped.split([':', '=', '{']).next().unwrap_or(stripped);
        let mut words = identifier_words(head);
        while words.len() > 1
            && words
                .last()
                .is_some_and(|w| matches!(w.as_str(), "or" | "and"))
        {
            words.pop();
        }
        words
    };
    let Some(last) = words.pop() else {
        return false;
    };
    matches!(
        last.as_str(),
        "token" | "tokens" | "pass" | "auth" | "bearer"
    ) || KEY_COMPOUNDS.contains(&last.as_str())
        || (matches!(last.as_str(), "key" | "keys")
            && words
                .last()
                .is_some_and(|word| KEY_QUALIFIERS.contains(&word.as_str())))
}

/// The fewest characters a sealed-secret ciphertext value has (a real one is hundreds).
const SEALED_CIPHERTEXT_MIN_LEN: usize = 200;

/// Whether the value at `value` in `text` is the ciphertext of a sealed secret: the document has
/// `kind: SealedSecret` at its top level, the value is base64 of 200 or more characters and the
/// nearest less-indented key above its line is `encryptedData`. Only ciphertext under that key is
/// cleared: a short value, a value under `template` and the same key in a plain Secret all stay.
pub(crate) fn is_sealed_ciphertext(text: &str, value: &std::ops::Range<usize>) -> bool {
    let body = &text[value.clone()];
    if body.len() < SEALED_CIPHERTEXT_MIN_LEN
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return false;
    }
    // The document holding the value: between the `---` separator lines around it.
    let separator = |line: &str| line.starts_with("---");
    let line_start = text[..value.start].rfind('\n').map_or(0, |i| i + 1);
    let mut doc_start = 0;
    let mut offset = 0;
    for line in text[..line_start].split_inclusive('\n') {
        if separator(line) {
            doc_start = offset + line.len();
        }
        offset += line.len();
    }
    let mut doc_end = text.len();
    let mut offset = line_start;
    for line in text[line_start..].split_inclusive('\n') {
        if offset > line_start && separator(line) {
            doc_end = offset;
            break;
        }
        offset += line.len();
    }
    let document = &text[doc_start..doc_end];
    let is_sealed = document.lines().any(|line| {
        line.strip_prefix("kind:")
            .is_some_and(|rest| rest.trim().trim_matches(['"', '\'']) == "SealedSecret")
    });
    if !is_sealed {
        return false;
    }
    let indent_of = |line: &str| line.len() - line.trim_start().len();
    let own_line = text[line_start..].lines().next().unwrap_or("");
    let own_indent = indent_of(own_line);
    let above = &text[doc_start..line_start];
    for line in above.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if indent_of(line) < own_indent {
            return trimmed.trim_end_matches(':') == "encryptedData" && trimmed.ends_with(':');
        }
    }
    false
}

/// Classify a candidate. `before_value` is the matched text before the value (the identifier, for
/// a context shape). `role` is the decoded JWT role for the JWT shapes. `context` is the text
/// around the match (the Firebase `apiKey` twin reads it). Order (spec 3.2): redaction marker, T3
/// digest, T4 role, T2, T1, T5, then the path-like and Firebase extensions.
pub(crate) fn classify(
    shape: SecretShape,
    value: &str,
    before_value: &str,
    role: Option<&str>,
    context: &TwinContext<'_>,
) -> Option<Twin> {
    if value.contains(ELLIPSIS) {
        return Some(Twin::RedactionMarker);
    }
    if crate::published::is_published_example(&sha256_hex(value), before_value) {
        return Some(Twin::PublishedExampleKey);
    }
    if let Some(role) = role {
        if role == "anon" || role == "authenticated" {
            return Some(Twin::PublicKeyRole(role.to_string()));
        }
    }
    if is_context_shaped(shape) && is_publishable_key(value) {
        return Some(Twin::PublishableKey);
    }
    if shape == SecretShape::NamedSecretLiteral && is_env_name(value) {
        return Some(Twin::EnvNameLiteral);
    }
    // A JWT is base64 of structured data: a placeholder word inside it is coincidence, so the
    // body-word rule does not apply to the JWT shapes or the PEM body.
    let word_rule_applies = !matches!(
        shape,
        SecretShape::SupabaseServiceRoleJwt | SecretShape::OpaqueJwt | SecretShape::PemPrivateKey
    );
    if word_rule_applies {
        let body = &value[prefix_of(shape, value).len()..];
        if let Some(word) = placeholder_word(body, shape.is_vendor_shaped()) {
            return Some(Twin::PlaceholderWord(word));
        }
    }
    if is_context_shaped(shape) && is_template(value) {
        return Some(Twin::TemplateToken);
    }
    if shape == SecretShape::NamedSecretLiteral && is_path_like(value) {
        return Some(Twin::PathLike);
    }
    // A readable name is code (`STORAGE_KEY`, `queryKey`); on a password-like identifier the same
    // shape is a passphrase.
    if shape == SecretShape::NamedSecretLiteral
        && !names_a_passphrase_holder(&identifier_head(before_value, context))
        && is_readable_name(value)
    {
        return Some(Twin::ReadableName);
    }
    if shape == SecretShape::NamedSecretLiteral
        && !names_a_secret(before_value)
        && is_qualified_label_name(value)
    {
        return Some(Twin::QualifiedLabelName);
    }
    if shape == SecretShape::NamedSecretLiteral
        && opens_a_module_specifier(before_value)
        && is_module_path(value)
    {
        return Some(Twin::ModulePath);
    }
    if shape == SecretShape::NamedSecretLiteral && is_block_header_label(value, context) {
        return Some(Twin::BlockHeaderLabel);
    }
    if shape == SecretShape::BasicAuthCredential && value.bytes().all(|b| b.is_ascii_lowercase()) {
        // `Authorization: Basic authentication ...` is prose; a real credential is not one word.
        return Some(Twin::ReadableName);
    }
    if shape == SecretShape::GoogleApiKey && is_firebase_web_api_key(context) {
        return Some(Twin::FirebaseWebApiKey);
    }
    None
}
