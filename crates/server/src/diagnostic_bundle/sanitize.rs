//! The pure re-typing functions every bundle leaf goes through: the per-bundle salt, path
//! tokens, repo ordinals, the word skeleton of a text and the engine-location check.
//!
//! Nothing here reads a file or the environment. Every function is deterministic given its
//! salt, so the tests pin each one with a fixed salt.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// How many characters a text skeleton keeps.
pub const MAX_SKELETON_CHARS: usize = 200;

/// How much of a text the skeleton reads.
pub const MAX_SKELETON_INPUT_CHARS: usize = 8192;

/// The longest number a skeleton keeps, in digits. A longer run of digits can be an account,
/// order or phone number.
pub const MAX_KEPT_DIGITS: usize = 6;

/// The most digits one whitespace word may hold, in all its pieces, for its numbers to be kept.
pub const MAX_WORD_DIGITS: usize = 8;

/// The file extensions a path token names. Any other extension shows as `other`: an unusual
/// extension can name a client's own tool.
pub const ADMITTED_EXTENSIONS: &[&str] = &[
    "rs",
    "toml",
    "lock",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "json",
    "jsonc",
    "yaml",
    "yml",
    "md",
    "mdx",
    "txt",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    "py",
    "pyi",
    "rb",
    "go",
    "mod",
    "sum",
    "java",
    "kt",
    "kts",
    "scala",
    "swift",
    "m",
    "mm",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "cs",
    "fs",
    "php",
    "pl",
    "pm",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "sql",
    "graphql",
    "gql",
    "proto",
    "xml",
    "svg",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "ico",
    "pdf",
    "csv",
    "tsv",
    "env",
    "ini",
    "cfg",
    "conf",
    "gradle",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "elm",
    "clj",
    "lua",
    "r",
    "jl",
    "tf",
    "tfvars",
    "hcl",
    "dockerfile",
    "gitignore",
    "zip",
    "gz",
    "tar",
    "wasm",
    "map",
    "snap",
    "lockb",
];

/// The folders under `crates/` that hold Camerata's own code: the only places an
/// [`engine_location`] may point into. `known_crates_match_the_workspace` pins it to the
/// workspace.
pub const CAMERATA_CRATES: &[&str] = &[
    "agent",
    "api-types",
    "app-core",
    "camerata-check",
    "checks",
    "cli",
    "client",
    "core",
    "deploy",
    "fleet",
    "gateway",
    "intake",
    "linter-registry",
    "liveness",
    "llm",
    "maintenance",
    "mcp",
    "orchestrator",
    "persistence",
    "preview",
    "rules",
    "scaffold",
    "secrets",
    "server",
    "source-check",
    "test-env",
    "ui",
    "ui-core",
    "worktracker",
];

/// The private-use character a secret becomes while the later steps run, so step 2 cannot
/// read the `[secret]` marker as a bracketed span. Stripped from the input first.
const SECRET_MARK: char = '\u{E000}';
/// The same for a `[quoted N]` span: its length follows the mark, closed by [`END_MARK`].
const QUOTED_MARK: char = '\u{E001}';
const END_MARK: char = '\u{E002}';

// ── The salt ─────────────────────────────────────────────────────────────────────────────────

/// 32 random bytes made per bundle. It keys every path and custom rule hash, so a token can be
/// looked up only through the operator's lookup file, never by hashing a guessed path.
///
/// Deliberately neither `Serialize` nor `Clone`, and its `Debug` prints no byte: it lives for
/// one build and is never written anywhere.
pub struct BundleSalt([u8; 32]);

impl std::fmt::Debug for BundleSalt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BundleSalt(..)")
    }
}

impl BundleSalt {
    /// A fresh salt from the operating system's random source.
    ///
    /// # Panics
    ///
    /// Only when the operating system has no random source at all, as
    /// [`crate::prompt_frame`] does: a predictable salt would make every token reversible by
    /// hashing guessed paths, so this fails loudly instead.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).expect(
            "the operating system random source is unavailable; refusing to hash client paths \
             with a predictable salt",
        );
        Self(bytes)
    }

    /// A fixed salt, for tests that pin a token.
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// `sha256(salt || 0 || repo ordinal || 0 || path)`, the first 12 hex characters.
pub fn hash_path(salt: &BundleSalt, ordinal: u32, path: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.0);
    h.update([0u8]);
    h.update(ordinal.to_string().as_bytes());
    h.update([0u8]);
    h.update(path.as_bytes());
    h.finalize()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ── Repos ────────────────────────────────────────────────────────────────────────────────────

/// A repo of the run, as `run.json` names it. Never written into a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub name: String,
    pub dir: String,
}

impl RepoInfo {
    /// The last component of the repo's folder.
    pub fn folder_name(&self) -> &str {
        self.dir
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
    }
}

/// The repo's place in `RunRecord.repos`, from 1. `None` for a name the run does not list.
pub fn repo_ordinal(repos: &[RepoInfo], name: &str) -> Option<u32> {
    repos
        .iter()
        .position(|r| r.name == name)
        .and_then(|i| u32::try_from(i + 1).ok())
}

/// The label a repo ordinal shows as.
pub fn repo_label(ordinal: u32) -> String {
    format!("repo-{ordinal}")
}

// ── Paths ────────────────────────────────────────────────────────────────────────────────────

/// Turns paths into tokens for one bundle and remembers each token's real path for the
/// operator's lookup file.
pub struct PathTokens<'a> {
    salt: &'a BundleSalt,
    repos: &'a [RepoInfo],
    named_paths: bool,
    /// Token (without depth) to the real path, as the input held it.
    lookup: BTreeMap<String, String>,
    /// Every path this bundle met, as the input held it, for the leak check.
    seen: BTreeSet<String>,
    /// Under `--named-paths`, every repo-relative path shown as it is, for the leak check.
    shown: BTreeSet<String>,
    /// Every repo-relative path met in a typed `Path` leaf. Under `--named-paths` a word of a
    /// text is shown as it is only when it is one of these.
    typed: BTreeSet<String>,
}

/// A path made repo-relative where it can be.
struct NormalPath {
    ordinal: u32,
    path: String,
    /// True when the path is relative to a repo (it was relative, or under a repo's folder).
    relative: bool,
}

impl<'a> PathTokens<'a> {
    pub fn new(salt: &'a BundleSalt, repos: &'a [RepoInfo], named_paths: bool) -> Self {
        Self {
            salt,
            repos,
            named_paths,
            lookup: BTreeMap::new(),
            seen: BTreeSet::new(),
            shown: BTreeSet::new(),
            typed: BTreeSet::new(),
        }
    }

    /// The typed paths a first walk met, so a second walk can tell them from a text's words.
    pub fn typed(&self) -> &BTreeSet<String> {
        &self.typed
    }

    /// This tokenizer knowing `typed` paths from the start.
    pub fn with_typed(mut self, typed: BTreeSet<String>) -> Self {
        self.typed = typed;
        self
    }

    /// The salt, for the other hashes of the same bundle (custom rule ids, unknown repos).
    pub fn salt(&self) -> &BundleSalt {
        self.salt
    }

    /// The repos of the run.
    pub fn repos(&self) -> &[RepoInfo] {
        self.repos
    }

    /// Whether this bundle shows real repo-relative paths.
    pub fn named_paths(&self) -> bool {
        self.named_paths
    }

    /// Token to real path, for the lookup file.
    pub fn lookup(&self) -> &BTreeMap<String, String> {
        &self.lookup
    }

    /// Every path met, for the leak check.
    pub fn seen(&self) -> &BTreeSet<String> {
        &self.seen
    }

    /// The repo-relative paths shown as they are under `--named-paths`.
    pub fn shown(&self) -> &BTreeSet<String> {
        &self.shown
    }

    fn normalize(&self, raw: &str, ordinal: u32) -> NormalPath {
        let mut p = raw.replace('\\', "/");
        while let Some(rest) = p.strip_prefix("./") {
            p = rest.to_string();
        }
        let absolute = p.starts_with('/')
            || p.starts_with('~')
            || (p.len() > 2 && p.as_bytes()[1] == b':' && p.as_bytes()[2] == b'/');
        if absolute {
            for (i, repo) in self.repos.iter().enumerate() {
                let dir = repo.dir.replace('\\', "/");
                let dir = dir.trim_end_matches('/');
                if dir.is_empty() {
                    continue;
                }
                if let Some(rest) = p.strip_prefix(dir).and_then(|r| r.strip_prefix('/')) {
                    return NormalPath {
                        ordinal: u32::try_from(i + 1).unwrap_or(0),
                        path: rest.to_string(),
                        relative: true,
                    };
                }
            }
            return NormalPath {
                ordinal,
                path: p,
                relative: false,
            };
        }
        // A relative path that starts with a repo's folder or name: relative to that repo.
        for (i, repo) in self.repos.iter().enumerate() {
            for prefix in [repo.folder_name(), repo.name.as_str()] {
                if prefix.is_empty() {
                    continue;
                }
                if let Some(rest) = p.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
                    if !rest.is_empty() {
                        return NormalPath {
                            ordinal: u32::try_from(i + 1).unwrap_or(0),
                            path: rest.to_string(),
                            relative: true,
                        };
                    }
                }
            }
        }
        NormalPath {
            ordinal,
            path: p,
            relative: true,
        }
    }

    /// The token for a path met inside a text: `p:<12 hex>` plus its extension, or under
    /// `--named-paths` the repo-relative path (an absolute path outside every repo is still
    /// hashed: it names the operator's machine, not the client's code).
    pub fn token(&mut self, raw: &str, ordinal: u32) -> String {
        self.token_inner(raw, ordinal, false)
    }

    /// The token for a word met inside a free text. Under `--named-paths` it is shown as it is
    /// only when it equals a path met in a typed `Path` leaf of the same bundle; any other
    /// dotted or slashed word (`customer.taxId`, a host name, an email) is hashed.
    pub fn text_token(&mut self, raw: &str, ordinal: u32) -> String {
        self.token_inner(raw, ordinal, true)
    }

    fn token_inner(&mut self, raw: &str, ordinal: u32, from_text: bool) -> String {
        self.seen.insert(raw.to_string());
        let n = self.normalize(raw, ordinal);
        let allowed = !from_text || self.typed.contains(&n.path);
        // A path that climbs (`..`) or starts at a home (`~`) can name the operator's machine:
        // it is hashed even under `--named-paths`.
        let climbs = n.path.split('/').any(|seg| seg == "..") || n.path.starts_with('~');
        if self.named_paths && allowed && n.relative && !climbs {
            self.shown.insert(n.path.clone());
            return n.path;
        }
        let mut token = format!("p:{}", hash_path(self.salt, n.ordinal, &n.path));
        if let Some(ext) = extension_label(&n.path) {
            token.push('.');
            token.push_str(ext);
        }
        self.lookup.insert(token.clone(), raw.to_string());
        token
    }

    /// The value of a `Path` leaf: the token plus `@<depth>` (how many folders deep the file
    /// sits, counting the file), or under `--named-paths` the repo-relative path.
    pub fn leaf(&mut self, raw: &str, ordinal: u32) -> String {
        let normal = self.normalize(raw, ordinal).path;
        self.typed.insert(normal);
        let token = self.token(raw, ordinal);
        if self.named_paths && !token.starts_with("p:") {
            return token;
        }
        let depth = self
            .normalize(raw, ordinal)
            .path
            .split('/')
            .filter(|s| !s.is_empty())
            .count();
        format!("{token}@{depth}")
    }
}

/// The extension label of `path`'s last segment: the extension when admitted, `other` when not,
/// `None` when it has none.
fn extension_label(path: &str) -> Option<&'static str> {
    let last = path.rsplit('/').next().unwrap_or("");
    let dot = last.rfind('.').filter(|&i| i > 0)?;
    let ext = last[dot + 1..].to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    Some(
        ADMITTED_EXTENSIONS
            .iter()
            .find(|e| **e == ext)
            .copied()
            .unwrap_or("other"),
    )
}

/// Whether a whitespace-separated token reads as a path: it holds `/` or `\`, or a dot followed
/// by 1 to 6 letters. A plain fraction (`2/3`, a part's place in its stage) is not a path.
pub fn looks_like_path(token: &str) -> bool {
    if is_fraction(token) {
        return false;
    }
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    let chars: Vec<char> = token.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        *c == '.' && {
            let letters = chars[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_alphabetic())
                .count();
            (1..=6).contains(&letters)
        }
    })
}

fn is_fraction(token: &str) -> bool {
    let short_number = |s: &str| {
        !s.is_empty() && s.len() <= MAX_KEPT_DIGITS && s.chars().all(|c| c.is_ascii_digit())
    };
    token
        .split_once('/')
        .is_some_and(|(a, b)| short_number(a) && short_number(b))
}

// ── The vocabulary ───────────────────────────────────────────────────────────────────────────

/// The words a skeleton keeps, from the checked-in `diagnostic_vocab.txt`.
pub fn vocab() -> &'static HashSet<String> {
    static VOCAB: OnceLock<HashSet<String>> = OnceLock::new();
    VOCAB.get_or_init(|| {
        include_str!("diagnostic_vocab.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_lowercase)
            .collect()
    })
}

// ── The text skeleton ────────────────────────────────────────────────────────────────────────

/// The word skeleton of a free text (design section 1), in order:
///
/// 1. every `camerata_secrets::detect_secrets` span becomes `[secret]`, then
///    `camerata_llm::failure::scrub_message` runs;
/// 2. every span inside backticks, double or single quotes, `{}`, `()`, `[]` or `<>` becomes
///    `[quoted N]`, N its length in characters;
/// 3. every token holding `/`, `\` or a dot followed by 1 to 6 letters becomes a path token;
/// 4. the rest splits on anything not a letter or digit, and a piece stays only as a number of
///    at most six digits or a lower-cased word of the vocabulary; any other piece becomes `_`,
///    and a run of them `_xN`;
/// 5. the result is capped at 200 characters.
///
/// `ordinal` is the repo the text belongs to (0 when none), which keys its path tokens.
pub fn text_skeleton(text: &str, paths: &mut PathTokens<'_>, ordinal: u32) -> String {
    // Only the head of a long text is read: the skeleton keeps 200 characters, and the quote
    // search is quadratic in the worst case. A span cut open by this is no risk: its words
    // still pass the vocabulary in step 4.
    let clean: String = text
        .chars()
        .take(MAX_SKELETON_INPUT_CHARS)
        .filter(|c| !matches!(*c, SECRET_MARK | QUOTED_MARK | END_MARK))
        .collect();
    let marked = mark_secrets(&clean);
    let scrubbed = camerata_llm::failure::scrub_message(&marked, usize::MAX / 2)
        .replace("[REDACTED]", &SECRET_MARK.to_string());
    let quoted = mark_quoted(&scrubbed);
    let mut out: Vec<String> = Vec::new();
    for word in quoted.split_whitespace() {
        skeleton_word(word, paths, ordinal, &mut out);
    }
    let joined = collapse_unknown(&out).join(" ");
    cap(&joined, MAX_SKELETON_CHARS)
}

/// Step 1: every secret span becomes [`SECRET_MARK`].
fn mark_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for m in camerata_secrets::detect_secrets(text) {
        if m.start < at || m.end > text.len() {
            continue;
        }
        out.push_str(&text[at..m.start]);
        out.push(SECRET_MARK);
        at = m.end;
    }
    out.push_str(&text[at..]);
    out
}

/// The closer of an opening delimiter.
fn closer(open: char) -> Option<char> {
    Some(match open {
        '`' => '`',
        '"' => '"',
        '\'' => '\'',
        '{' => '}',
        '(' => ')',
        '[' => ']',
        '<' => '>',
        _ => return None,
    })
}

/// Step 2: every quoted or bracketed span becomes [`QUOTED_MARK`] + its length + [`END_MARK`].
/// The outermost span wins. A single quote opens only at the start of a word and closes only at
/// its end, so an apostrophe (`don't`) is not a quote. A delimiter with no closer stays as it is.
fn mark_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let opens = closer(c).filter(|_| c != '\'' || i == 0 || !chars[i - 1].is_alphanumeric());
        if let Some(close) = opens {
            if let Some(end) = find_close(&chars, i, c, close) {
                let len = end - i - 1;
                out.push(QUOTED_MARK);
                out.push_str(&len.to_string());
                out.push(END_MARK);
                i = end + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The index of the delimiter closing the span opened at `start`, if any.
fn find_close(chars: &[char], start: usize, open: char, close: char) -> Option<usize> {
    let nests = open != close;
    let mut depth = 0usize;
    for (j, &c) in chars.iter().enumerate().skip(start + 1) {
        if nests && c == open {
            depth += 1;
        } else if c == close {
            if open == '\'' && chars.get(j + 1).is_some_and(|n| n.is_alphanumeric()) {
                continue;
            }
            if depth == 0 {
                return Some(j);
            }
            depth -= 1;
        }
    }
    None
}

/// Steps 3 and 4 for one whitespace-separated word.
fn skeleton_word(word: &str, paths: &mut PathTokens<'_>, ordinal: u32, out: &mut Vec<String>) {
    // Split the marks off first: they are atomic.
    let mut rest = word;
    while !rest.is_empty() {
        let next_mark = rest.find([SECRET_MARK, QUOTED_MARK]);
        let (before, after) = match next_mark {
            Some(at) => rest.split_at(at),
            None => (rest, ""),
        };
        if !before.is_empty() {
            plain_word(before, paths, ordinal, out);
        }
        if after.is_empty() {
            break;
        }
        if let Some(tail) = after.strip_prefix(SECRET_MARK) {
            out.push("[secret]".to_string());
            rest = tail;
        } else if let Some(tail) = after.strip_prefix(QUOTED_MARK) {
            let (len, tail) = tail.split_once(END_MARK).unwrap_or((tail, ""));
            out.push(format!("[quoted {len}]"));
            rest = tail;
        } else {
            break;
        }
    }
}

/// A word with no mark in it: a path token, or its kept pieces.
fn plain_word(word: &str, paths: &mut PathTokens<'_>, ordinal: u32, out: &mut Vec<String>) {
    let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && !"/\\~._-".contains(c));
    let trimmed = trimmed.trim_end_matches(['.', ',', ';', ':']);
    if !trimmed.is_empty() && looks_like_path(trimmed) {
        let (path, suffix) = split_line_suffix(trimmed);
        out.push(format!("{}{suffix}", paths.text_token(path, ordinal)));
        return;
    }
    if is_fraction(trimmed) {
        out.push(trimmed.to_string());
        return;
    }
    // A word holding many digits in all (a phone or account number split by dashes or dots)
    // keeps none of its numbers.
    let digits = word.chars().filter(char::is_ascii_digit).count();
    for piece in word.split(|c: char| !c.is_alphanumeric()) {
        if piece.is_empty() {
            continue;
        }
        let lower = piece.to_lowercase();
        let number = digits <= MAX_WORD_DIGITS
            && piece.len() <= MAX_KEPT_DIGITS
            && piece.chars().all(|c| c.is_ascii_digit());
        if number || vocab().contains(&lower) {
            out.push(lower);
        } else {
            out.push("_".to_string());
        }
    }
}

/// A trailing `:line` or `:line:column` of a path, kept after its token.
fn split_line_suffix(token: &str) -> (&str, &str) {
    let mut cut = token.len();
    for _ in 0..2 {
        let head = &token[..cut];
        match head.rfind(':') {
            Some(at)
                if at + 1 < head.len()
                    && head[at + 1..].len() <= MAX_KEPT_DIGITS
                    && head[at + 1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                cut = at;
            }
            _ => break,
        }
    }
    (&token[..cut], &token[cut..])
}

/// A run of two or more `_` becomes `_xN`.
fn collapse_unknown(words: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut run = 0usize;
    let flush = |run: &mut usize, out: &mut Vec<String>| {
        match *run {
            0 => {}
            1 => out.push("_".to_string()),
            n => out.push(format!("_x{n}")),
        }
        *run = 0;
    };
    for w in words {
        if w == "_" {
            run += 1;
        } else {
            flush(&mut run, &mut out);
            out.push(w.clone());
        }
    }
    flush(&mut run, &mut out);
    out
}

/// At most `max` characters, cut at a space where one is near.
fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(at) if at > max / 2 => cut[..at].to_string(),
        _ => cut,
    }
}

// ── Engine locations ─────────────────────────────────────────────────────────────────────────

/// `file:line` in Camerata's own code, or `None`. Kept only when the path is relative, sits
/// under a known Camerata crate folder (`crates/<crate>/...`), names a `.rs` file, and every
/// segment is a plain lower-case Rust module name. Anything else (an absolute path, a client
/// crate, a `..`) is dropped: a client path in an origin field must never reach a bundle.
pub fn engine_location(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (file, line) = match raw.rsplit_once(':') {
        Some((f, l)) if !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()) => (f, Some(l)),
        _ => (raw, None),
    };
    let mut segments = file.split('/');
    if segments.next() != Some("crates") {
        return None;
    }
    let krate = segments.next()?;
    if !CAMERATA_CRATES.contains(&krate) {
        return None;
    }
    let rest: Vec<&str> = segments.collect();
    let (last, folders) = rest.split_last()?;
    let module_name = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    if !folders.iter().all(|s| module_name(s)) {
        return None;
    }
    let stem = last.strip_suffix(".rs")?;
    if !module_name(stem) {
        return None;
    }
    Some(match line {
        Some(l) if l.len() <= 6 => format!("{file}:{l}"),
        Some(_) => return None,
        None => file.to_string(),
    })
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn known_crates_match_the_workspace() {
        let crates_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut on_disk: Vec<String> = std::fs::read_dir(crates_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().join("Cargo.toml").exists())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        let mut known: Vec<String> = CAMERATA_CRATES.iter().map(|s| s.to_string()).collect();
        known.sort();
        assert_eq!(known, on_disk);
    }

    #[test]
    fn the_vocabulary_holds_no_fixture_word() {
        for word in ["acme", "billing", "charge", "auditor", "const", "root"] {
            assert!(!vocab().contains(word), "`{word}` is in the vocabulary");
        }
        assert!(vocab().len() > 300);
    }

    #[test]
    fn quoting_and_paths() {
        let salt = BundleSalt::from_bytes([9; 32]);
        let repos = vec![];
        let mut p = PathTokens::new(&salt, &repos, false);
        assert_eq!(
            text_skeleton("can't read (x y) [ab] {c} <d> 'e' \"f\"", &mut p, 0),
            "can t read [quoted 3] [quoted 2] [quoted 1] [quoted 1] [quoted 1] [quoted 1]"
        );
        assert_eq!(text_skeleton("audit pass 2/3", &mut p, 0), "audit pass 2/3");
        let t = text_skeleton("error at src/a.rs:12:4", &mut p, 0);
        assert!(
            t.starts_with("error at p:") && t.ends_with(".rs:12:4"),
            "{t}"
        );
        assert_eq!(
            text_skeleton("x.weird", &mut p, 0).rsplit('.').next(),
            Some("other")
        );
    }
}
